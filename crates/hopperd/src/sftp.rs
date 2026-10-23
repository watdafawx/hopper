//! SFTP on its own port (8022 unless set), for FileZilla, WinSCP and
//! scripts. `/` lists the servers you can see; `/<server id>/…` is inside
//! that server's folder and can't leave it.
//!
//! Signing in: a person's name and password from this PC, the LAN or
//! Tailscale (when they haven't turned on two-factor sign-in); or, from
//! anywhere, any name with an API token or device token as the password.
//! What you may change follows your role or token, like the file manager.
//! Files saved over keep their old version in the trash, and deleted ones
//! go to the trash, like in the web UI.

use crate::App;
use crate::auth::Who;
use crate::files;
use crate::servers::Entry;
use axum::http::Method;
use russh::keys::PrivateKey;
use russh::server::{Auth, ChannelOpenHandle, Msg, Server as _, Session};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode};
use std::collections::HashMap;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

const KEY_FILE: &str = "ssh_host_ed25519_key";
/// Saved-over files up to this size keep a copy as their old version;
/// bigger ones are moved to the trash whole.
const KEEP_COPY_MAX: u64 = 16 * 1024 * 1024;

/// The host key, made on first start.
fn host_key(dir: &Path) -> anyhow::Result<PrivateKey> {
    let path = dir.join(KEY_FILE);
    if let Ok(text) = std::fs::read_to_string(&path) {
        return Ok(PrivateKey::from_openssh(text)?);
    }
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| anyhow::anyhow!("{e}"))?;
    let key = PrivateKey::from(russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&seed));
    std::fs::write(&path, key.to_openssh(russh::keys::ssh_key::LineEnding::LF)?.as_bytes())?;
    Ok(key)
}

/// "SHA256:…" of the host key, as ssh clients show it.
pub fn fingerprint(dir: &Path) -> Option<String> {
    let key = host_key(dir).ok()?;
    Some(key.public_key().fingerprint(russh::keys::HashAlg::Sha256).to_string())
}

/// Listens for SFTP for as long as the panel runs.
pub async fn serve(app: Arc<App>, bind: String, port: u16) {
    let key = match host_key(&app.data_dir) {
        Ok(k) => k,
        Err(e) => {
            log::warn!("SFTP off: host key: {e:#}");
            return;
        }
    };
    let config = Arc::new(russh::server::Config {
        auth_rejection_time: Duration::from_secs(2),
        auth_rejection_time_initial: Some(Duration::from_millis(50)),
        inactivity_timeout: Some(Duration::from_secs(30 * 60)),
        keys: vec![key],
        methods: russh::MethodSet::from(&[russh::MethodKind::Password][..]),
        ..Default::default()
    });
    let listener = match tokio::net::TcpListener::bind((bind.as_str(), port)).await {
        Ok(l) => l,
        Err(e) => {
            log::warn!("SFTP off: can't listen on port {port}: {e}. Pick another with sftp_port in hopperd.toml (0 turns it off).");
            return;
        }
    };
    log::info!("SFTP on port {port} (host key {})", fingerprint(&app.data_dir).unwrap_or_default());
    let mut server = SshServer { app };
    if let Err(e) = server.run_on_socket(config, &listener).await {
        log::warn!("SFTP stopped: {e}");
    }
}

struct SshServer {
    app: Arc<App>,
}

impl russh::server::Server for SshServer {
    type Handler = SshSession;

    fn new_client(&mut self, peer: Option<SocketAddr>) -> SshSession {
        SshSession { app: self.app.clone(), ip: peer.map_or(IpAddr::from([0, 0, 0, 0]), |p| p.ip()), who: None, channels: HashMap::new() }
    }
}

struct SshSession {
    app: Arc<App>,
    ip: IpAddr,
    who: Option<Who>,
    channels: HashMap<ChannelId, Channel<Msg>>,
}

impl russh::server::Handler for SshSession {
    type Error = anyhow::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        if !self.app.limiter.allow(self.ip, true) {
            log::info!("SFTP sign-in from {}: too many tries, wait a minute", self.ip);
            return Ok(retry());
        }
        let private = crate::net::Net::of(self.ip).private();
        let res = self.app.auth.sftp_login(user, password, self.ip, private);
        let ok = res.is_ok();
        self.app.audit.record(&crate::security::Event {
            at: crate::security::now(),
            who: res.as_ref().map_or_else(|_| "-".into(), |w| w.name()),
            ip: self.ip.to_string(),
            net: crate::net::Net::of(self.ip),
            method: "SFTP".into(),
            path: "sign-in".into(),
            status: if ok { 200 } else { 401 },
        });
        match res {
            Ok(w) => {
                log::info!("SFTP sign-in: {} from {}", w.name(), self.ip);
                self.who = Some(w);
                Ok(Auth::Accept)
            }
            Err(e) => {
                log::info!("SFTP sign-in refused for {user} from {}: {e}", self.ip);
                Ok(retry())
            }
        }
    }

    async fn channel_open_session(&mut self, channel: Channel<Msg>, reply: ChannelOpenHandle, _session: &mut Session) -> Result<(), Self::Error> {
        if self.who.is_none() {
            return Ok(());
        }
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn channel_eof(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        session.close(channel)?;
        Ok(())
    }

    async fn subsystem_request(&mut self, id: ChannelId, name: &str, session: &mut Session) -> Result<(), Self::Error> {
        match (name, self.channels.remove(&id), self.who.clone()) {
            ("sftp", Some(channel), Some(who)) => {
                session.channel_success(id)?;
                let sftp = Sftp { app: self.app.clone(), who, handles: HashMap::new(), next: 0, ip: self.ip };
                tokio::spawn(russh_sftp::server::run(channel.into_stream(), sftp));
            }
            _ => session.channel_failure(id)?,
        }
        Ok(())
    }
}

enum Open {
    File { file: std::fs::File, server: String, rel: String, wrote: bool },
    Dir { files: Vec<File>, sent: bool },
}

struct Sftp {
    app: Arc<App>,
    who: Who,
    handles: HashMap<String, Open>,
    next: u64,
    ip: IpAddr,
}

/// A normalised absolute path: "/", "/tnp", "/tnp/config/a.toml".
fn normalise(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for p in path.split(['/', '\\']) {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    format!("/{}", parts.join("/"))
}

/// "/tnp/config/a.toml" → ("tnp", "config/a.toml"); the root → None.
fn split(path: &str) -> Option<(String, String)> {
    let n = normalise(path);
    let rest = n.trim_start_matches('/');
    if rest.is_empty() {
        return None;
    }
    let (id, rel) = rest.split_once('/').unwrap_or((rest, ""));
    Some((id.to_string(), rel.to_string()))
}

/// No, but the client may try again (with a password: it's all there is).
fn retry() -> Auth {
    Auth::Reject { proceed_with_methods: Some(russh::MethodSet::from(&[russh::MethodKind::Password][..])), partial_success: false }
}

fn dir_attrs() -> FileAttributes {
    FileAttributes { permissions: Some(0o40755), size: Some(0), ..FileAttributes::empty() }
}

fn attrs_of(meta: &std::fs::Metadata) -> FileAttributes {
    let mtime = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs() as u32);
    FileAttributes {
        size: Some(if meta.is_dir() { 0 } else { meta.len() }),
        permissions: Some(if meta.is_dir() { 0o40755 } else { 0o100644 }),
        mtime,
        atime: mtime,
        ..FileAttributes::empty()
    }
}

fn ok(id: u32) -> Status {
    Status { id, status_code: StatusCode::Ok, error_message: "Ok".into(), language_tag: "en-US".into() }
}

fn io_code(e: &std::io::Error) -> StatusCode {
    match e.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
        std::io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    }
}

impl Sftp {
    /// The server and real path for `path`, if this person may `method` it.
    fn locate(&self, path: &str, method: Method) -> Result<(Arc<Entry>, String, PathBuf), StatusCode> {
        let (id, rel) = split(path).ok_or(StatusCode::PermissionDenied)?;
        let e = self.app.servers.get(&id).filter(|_| self.who.sees(&id)).ok_or(StatusCode::NoSuchFile)?;
        if rel.split('/').any(files::is_internal) {
            return Err(StatusCode::NoSuchFile);
        }
        let route = if method == Method::GET { format!("/api/v1/fs/{id}/list") } else { format!("/api/v1/fs/{id}/text") };
        if crate::security::refusal(&self.who, &method, &route).is_some() {
            return Err(StatusCode::PermissionDenied);
        }
        let p = files::resolve(&e.root, &rel).map_err(|_| StatusCode::PermissionDenied)?;
        Ok((e, rel, p))
    }

    fn handle(&mut self, open: Open) -> String {
        self.next += 1;
        let h = self.next.to_string();
        self.handles.insert(h.clone(), open);
        h
    }

    fn servers(&self) -> Vec<Arc<Entry>> {
        self.app.servers.list().into_iter().filter(|e| self.who.sees(&e.id)).collect()
    }

    fn log_change(&self, what: &str, server: &str, rel: &str) {
        self.app.audit.record(&crate::security::Event {
            at: crate::security::now(),
            who: self.who.name(),
            ip: self.ip.to_string(),
            net: crate::net::Net::of(self.ip),
            method: "SFTP".into(),
            path: format!("{what} {server}/{rel}"),
            status: 200,
        });
    }
}

impl russh_sftp::server::Handler for Sftp {
    type Error = StatusCode;

    fn unimplemented(&self) -> StatusCode {
        StatusCode::OpUnsupported
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, StatusCode> {
        Ok(Name { id, files: vec![File::dummy(normalise(&path))] })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        match split(&path) {
            None => Ok(Attrs { id, attrs: dir_attrs() }),
            Some((_, rel)) if rel.is_empty() => {
                self.locate(&path, Method::GET)?;
                Ok(Attrs { id, attrs: dir_attrs() })
            }
            Some(_) => {
                let (_, _, p) = self.locate(&path, Method::GET)?;
                let meta = std::fs::metadata(&p).map_err(|e| io_code(&e))?;
                Ok(Attrs { id, attrs: attrs_of(&meta) })
            }
        }
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        self.stat(id, path).await
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, StatusCode> {
        match self.handles.get(&handle) {
            Some(Open::File { file, .. }) => Ok(Attrs { id, attrs: attrs_of(&file.metadata().map_err(|e| io_code(&e))?) }),
            Some(Open::Dir { .. }) => Ok(Attrs { id, attrs: dir_attrs() }),
            None => Err(StatusCode::Failure),
        }
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, StatusCode> {
        let files = match split(&path) {
            None => self.servers().iter().map(|e| File::new(e.id.clone(), dir_attrs())).collect(),
            Some(_) => {
                let (_, _, p) = self.locate(&path, Method::GET)?;
                let rd = std::fs::read_dir(&p).map_err(|e| io_code(&e))?;
                rd.flatten()
                    .filter_map(|d| {
                        let name = d.file_name().to_string_lossy().into_owned();
                        if files::is_internal(&name) {
                            return None;
                        }
                        let meta = d.metadata().ok()?;
                        Some(File::new(name, attrs_of(&meta)))
                    })
                    .collect()
            }
        };
        let handle = self.handle(Open::Dir { files, sent: false });
        Ok(Handle { id, handle })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, StatusCode> {
        match self.handles.get_mut(&handle) {
            Some(Open::Dir { files, sent }) if !*sent => {
                *sent = true;
                Ok(Name { id, files: std::mem::take(files) })
            }
            Some(Open::Dir { .. }) => Err(StatusCode::Eof),
            _ => Err(StatusCode::Failure),
        }
    }

    async fn open(&mut self, id: u32, filename: String, pflags: OpenFlags, _attrs: FileAttributes) -> Result<Handle, StatusCode> {
        let writing = pflags.intersects(OpenFlags::WRITE | OpenFlags::APPEND | OpenFlags::CREATE | OpenFlags::TRUNCATE);
        let (e, rel, p) = self.locate(&filename, if writing { Method::PUT } else { Method::GET })?;
        if rel.is_empty() {
            return Err(StatusCode::PermissionDenied);
        }
        let exists = p.is_file();
        if writing && exists && pflags.contains(OpenFlags::EXCLUDE) {
            return Err(StatusCode::Failure);
        }
        // Saving over a file: keep the old one, like the editor does.
        if writing && exists && pflags.contains(OpenFlags::TRUNCATE) {
            let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            let kept = if size <= KEEP_COPY_MAX { files::keep_version(&e.root, &rel, &p).is_ok() } else { files::trash(&e.root, &rel).is_ok() };
            if !kept {
                log::warn!("SFTP: couldn't keep the old {rel} before saving over it");
            }
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(writing)
            .append(pflags.contains(OpenFlags::APPEND))
            .create(pflags.contains(OpenFlags::CREATE))
            .truncate(pflags.contains(OpenFlags::TRUNCATE))
            .open(&p)
            .map_err(|e| io_code(&e))?;
        let handle = self.handle(Open::File { file, server: e.id.clone(), rel, wrote: false });
        Ok(Handle { id, handle })
    }

    async fn read(&mut self, id: u32, handle: String, offset: u64, len: u32) -> Result<Data, StatusCode> {
        let Some(Open::File { file, .. }) = self.handles.get_mut(&handle) else { return Err(StatusCode::Failure) };
        file.seek(SeekFrom::Start(offset)).map_err(|e| io_code(&e))?;
        let mut buf = vec![0u8; len.min(256 * 1024) as usize];
        let n = file.read(&mut buf).map_err(|e| io_code(&e))?;
        if n == 0 {
            return Err(StatusCode::Eof);
        }
        buf.truncate(n);
        Ok(Data { id, data: buf })
    }

    async fn write(&mut self, id: u32, handle: String, offset: u64, data: Vec<u8>) -> Result<Status, StatusCode> {
        let Some(Open::File { file, wrote, .. }) = self.handles.get_mut(&handle) else { return Err(StatusCode::Failure) };
        file.seek(SeekFrom::Start(offset)).map_err(|e| io_code(&e))?;
        file.write_all(&data).map_err(|e| io_code(&e))?;
        *wrote = true;
        Ok(ok(id))
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, StatusCode> {
        if let Some(Open::File { server, rel, wrote: true, .. }) = self.handles.remove(&handle) {
            self.log_change("wrote", &server, &rel);
        }
        Ok(ok(id))
    }

    async fn setstat(&mut self, id: u32, path: String, _attrs: FileAttributes) -> Result<Status, StatusCode> {
        // Times and modes aren't kept; say yes so uploads don't fail.
        self.locate(&path, Method::PUT)?;
        Ok(ok(id))
    }

    async fn fsetstat(&mut self, id: u32, _handle: String, _attrs: FileAttributes) -> Result<Status, StatusCode> {
        Ok(ok(id))
    }

    async fn mkdir(&mut self, id: u32, path: String, _attrs: FileAttributes) -> Result<Status, StatusCode> {
        let (e, rel, p) = self.locate(&path, Method::PUT)?;
        std::fs::create_dir(&p).map_err(|e| io_code(&e))?;
        self.log_change("made folder", &e.id, &rel);
        Ok(ok(id))
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, StatusCode> {
        let (e, rel, p) = self.locate(&filename, Method::PUT)?;
        if rel.is_empty() || !p.exists() {
            return Err(StatusCode::NoSuchFile);
        }
        files::trash(&e.root, &rel).map_err(|_| StatusCode::Failure)?;
        self.log_change("trashed", &e.id, &rel);
        Ok(ok(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, StatusCode> {
        self.remove(id, path).await
    }

    async fn rename(&mut self, id: u32, oldpath: String, newpath: String) -> Result<Status, StatusCode> {
        let (e, rel, from) = self.locate(&oldpath, Method::PUT)?;
        let (e2, rel2, to) = self.locate(&newpath, Method::PUT)?;
        if e.id != e2.id || rel.is_empty() || rel2.is_empty() {
            return Err(StatusCode::OpUnsupported);
        }
        if to.exists() {
            return Err(StatusCode::Failure);
        }
        std::fs::rename(&from, &to).map_err(|e| io_code(&e))?;
        self.log_change(&format!("renamed {rel} to"), &e.id, &rel2);
        Ok(ok(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(normalise("/a/./b/../c"), "/a/c");
        assert_eq!(normalise("../../.."), "/");
        assert_eq!(normalise("."), "/");
        assert_eq!(split("/"), None);
        assert_eq!(split("/tnp"), Some(("tnp".into(), "".into())));
        assert_eq!(split("/tnp/config/../mods/a.jar"), Some(("tnp".into(), "mods/a.jar".into())));
        assert_eq!(split("/tnp/../../etc"), Some(("etc".into(), "".into())), "can't climb out of the root");
    }
}
