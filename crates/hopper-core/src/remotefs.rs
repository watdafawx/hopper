//! Servers whose files Hopper reaches over SFTP or FTP(S) rather than a
//! shared folder (rented hosts, Pterodactyl's SFTP, a PC with an SSH server).
//!
//! Hopper keeps a small local copy ("mirror") of the parts of each such
//! server that sync, config compare and crash scans read: `mods`, `config`,
//! the start scripts and so on. Refreshing it fetches only files whose size
//! or time changed. Sync then runs on the mirror as on any folder, and every
//! change it made is played back on the server: replaced and removed files
//! are first moved into the server's own `.hopper-backups/<time>/`, uploads
//! go to `name.part` and are renamed into place, so an interrupted upload
//! never leaves half a jar.

mod ftp;
mod sftp;

use crate::model::{SyncOp, SyncResult};
use crate::settings::FileSite;
use anyhow::{Context as _, bail};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A file or folder on the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
    /// Seconds since 1970, as the server says.
    pub mtime: u64,
}

/// A connection to a server's files. Paths are the server's, `/`-separated.
pub trait Remote: Send {
    fn list(&mut self, dir: &str) -> anyhow::Result<Vec<Entry>>;
    fn get(&mut self, path: &str, local: &Path) -> anyhow::Result<()>;
    /// Writes `path` from `local`, through `path.part` and a rename.
    fn put(&mut self, local: &Path, path: &str) -> anyhow::Result<()>;
    fn rename(&mut self, from: &str, to: &str) -> anyhow::Result<()>;
    fn mkdirs(&mut self, dir: &str) -> anyhow::Result<()>;
    /// The server's SSH key ("SHA256:…"), for pinning.
    fn host_key(&self) -> Option<String> {
        None
    }
}

/// Fails for a server reached over SFTP / FTP: installers and loader
/// changes run on the server's own PC, which Hopper can't do from here.
pub fn local_only(root: &Path, what: &str) -> anyhow::Result<()> {
    if mirror_of(root).is_some() {
        bail!("{what} runs on the server's own PC, and Hopper reaches this server over SFTP or FTP. Do it in its panel; Sync still copies mods and configs.");
    }
    Ok(())
}

/// The port a protocol uses unless set.
pub fn default_port(proto: &str) -> u16 {
    if proto == "sftp" { 22 } else { 21 }
}

/// Where a site's password (or key passphrase) is kept in Credential Manager.
pub fn secret_name(id: &str) -> String {
    format!("files_secret.{id}")
}

pub fn secret(id: &str) -> Option<String> {
    crate::secrets::get(&secret_name(id))
}

pub fn set_secret(id: &str, value: &str) -> anyhow::Result<()> {
    crate::secrets::set(&secret_name(id), value)
}

/// Connects to `site` (as `user` when given, for Pterodactyl's per-server
/// names).
pub fn connect(site: &FileSite) -> anyhow::Result<Box<dyn Remote>> {
    if site.host.trim().is_empty() || site.user.trim().is_empty() {
        bail!("Add the server's address and user name first.");
    }
    let pass = secret(&site.id).unwrap_or_default();
    let port = if site.port == 0 { default_port(&site.proto) } else { site.port };
    Ok(match site.proto.as_str() {
        "sftp" => Box::new(sftp::Sftp::connect(site.host.trim(), port, site.user.trim(), &pass, site.key_file.trim(), &site.host_key)?),
        "ftp" => Box::new(ftp::Ftp::connect(site.host.trim(), port, site.user.trim(), &pass, None)?),
        _ => Box::new(ftp::Ftp::connect(site.host.trim(), port, site.user.trim(), &pass, Some(site.self_signed))?),
    })
}

/// `a/b`, without doubled or trailing slashes.
pub fn join(a: &str, b: &str) -> String {
    let a = a.trim_end_matches('/');
    let b = b.trim_start_matches('/');
    match (a.is_empty(), b.is_empty()) {
        (true, _) => format!("/{b}").replace("//", "/"),
        (_, true) => a.to_string(),
        _ => format!("{a}/{b}"),
    }
}

/// The server folders in a site's folder: the folder itself when it holds
/// `server.properties`, else each folder in it that does.
pub fn servers(r: &mut dyn Remote, folder: &str) -> anyhow::Result<Vec<String>> {
    // Empty: the folder the server signs you in to.
    let folder = if folder.trim().is_empty() { "." } else { folder.trim() };
    let top = r.list(folder).with_context(|| format!("listing {folder}"))?;
    if top.iter().any(|e| !e.dir && e.name == "server.properties") {
        return Ok(vec![folder.to_string()]);
    }
    let mut out = Vec::new();
    for d in top.iter().filter(|e| e.dir && !e.name.starts_with('.')) {
        let path = join(folder, &d.name);
        if r.list(&path).is_ok_and(|l| l.iter().any(|e| !e.dir && e.name == "server.properties")) {
            out.push(path);
        }
    }
    out.sort_by_key(|p| p.to_lowercase());
    Ok(out)
}

// ---- the mirror ----------------------------------------------------------------

const INDEX: &str = ".hopper-remote.json";

/// A local copy of a server's files, and where they live.
#[derive(Clone, Debug, PartialEq)]
pub struct Mirror {
    pub root: PathBuf,
    pub site: FileSite,
    /// The server's folder on the site.
    pub folder: String,
}

static MIRRORS: std::sync::LazyLock<Mutex<Vec<(Mirror, Option<Instant>)>>> = std::sync::LazyLock::new(Default::default);

/// Where the local copy of `folder` on site `site` lives.
pub fn mirror_root(site: &str, folder: &str) -> PathBuf {
    let name = folder.trim_end_matches('/').rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or("server");
    let safe: String = name.chars().map(|c| if c.is_alphanumeric() || "-_. ".contains(c) { c } else { '_' }).collect();
    crate::paths::dirs()
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(std::env::temp_dir)
        .join("remote-files")
        .join(site)
        .join(safe.trim())
}

pub fn register(m: &Mirror) {
    let mut all = MIRRORS.lock().unwrap_or_else(|e| e.into_inner());
    all.retain(|(x, _)| x.root != m.root);
    all.push((m.clone(), None));
}

/// The mirror `p` is in (or is).
pub fn mirror_of(p: &Path) -> Option<Mirror> {
    MIRRORS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(m, _)| p.starts_with(&m.root))
        .map(|(m, _)| m.clone())
}

/// Brings the mirror that holds `p` up to date, unless that happened in the
/// last few seconds. Does nothing for other paths. Returns how many files
/// came down.
pub fn refresh_path(p: &Path) -> anyhow::Result<usize> {
    let due = {
        let all = MIRRORS.lock().unwrap_or_else(|e| e.into_inner());
        all.iter()
            .find(|(m, _)| p.starts_with(&m.root))
            .filter(|(_, at)| at.is_none_or(|t| t.elapsed() > Duration::from_secs(10)))
            .map(|(m, _)| m.clone())
    };
    match due {
        Some(m) => refresh(&m),
        None => Ok(0),
    }
}

/// What the mirror keeps, by path inside the server folder.
pub fn wanted(rel: &str, dir: bool, size: u64) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    let first = parts[0];
    if first.starts_with('.') {
        return false;
    }
    if parts.len() == 1 {
        if dir {
            return matches!(first, "mods" | "config" | "defaultconfigs" | "kubejs" | "crash-reports" | "logs");
        }
        let ext = first.rsplit_once('.').map(|x| x.1.to_ascii_lowercase()).unwrap_or_default();
        return size <= 1 << 20 && matches!(ext.as_str(), "properties" | "json" | "txt" | "bat" | "sh" | "cmd" | "toml");
    }
    match first {
        // Jars only, not their own sub-folders (like mods/.connector).
        "mods" => parts.len() == 2 && !dir && !parts[1].starts_with('.') && (rel.ends_with(".jar") || rel.ends_with(".jar.disabled")),
        "logs" => parts.len() == 2 && parts[1] == "latest.log",
        "crash-reports" => parts.len() == 2 && size <= 4 << 20,
        _ => dir || size <= 16 << 20,
    }
}

fn load_index(root: &Path) -> BTreeMap<String, (u64, u64)> {
    std::fs::read(root.join(INDEX)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_index(root: &Path, idx: &BTreeMap<String, (u64, u64)>) {
    if let Ok(b) = serde_json::to_vec(idx) {
        let _ = std::fs::write(root.join(INDEX), b);
    }
}

/// Every wanted file under the server folder: path inside it → (size, time).
/// `light`: only the files at the top (properties, scripts), for listing.
fn walk(r: &mut dyn Remote, folder: &str, light: bool) -> anyhow::Result<BTreeMap<String, (u64, u64)>> {
    let mut out = BTreeMap::new();
    let mut todo = vec![String::new()];
    while let Some(rel) = todo.pop() {
        let entries = match r.list(&join(folder, &rel)) {
            Ok(e) => e,
            Err(e) if rel.is_empty() => return Err(e),
            Err(_) => continue,
        };
        for e in entries {
            if e.name == "." || e.name == ".." || e.name.ends_with(".part") {
                continue;
            }
            let p = if rel.is_empty() { e.name.clone() } else { format!("{rel}/{}", e.name) };
            if !wanted(&p, e.dir, e.size) {
                continue;
            }
            if e.dir {
                if !light {
                    todo.push(p);
                }
            } else {
                out.insert(p, (e.size, e.mtime));
            }
        }
    }
    Ok(out)
}

/// Fetches what changed on the server into the mirror (and removes what's
/// gone there). Returns how many files came down.
pub fn refresh(m: &Mirror) -> anyhow::Result<usize> {
    let mut r = connect(&m.site)?;
    refresh_with(&mut *r, m)
}

pub fn refresh_with(r: &mut dyn Remote, m: &Mirror) -> anyhow::Result<usize> {
    fetch(r, m, false)
}

fn fetch(r: &mut dyn Remote, m: &Mirror, light: bool) -> anyhow::Result<usize> {
    std::fs::create_dir_all(&m.root)?;
    let theirs = walk(r, &m.folder, light)?;
    let mut idx = load_index(&m.root);
    let mut got = 0;
    for (rel, stamp) in &theirs {
        let local = m.root.join(rel);
        if idx.get(rel) == Some(stamp) && local.is_file() {
            continue;
        }
        if let Some(dir) = local.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let part = part_of(&local);
        r.get(&join(&m.folder, rel), &part).with_context(|| format!("downloading {rel}"))?;
        std::fs::rename(&part, &local)?;
        idx.insert(rel.clone(), *stamp);
        got += 1;
        // Saved as it goes, so a dropped connection keeps what came down.
        if got % 25 == 0 {
            save_index(&m.root, &idx);
        }
    }
    // A light fetch only knows the top level.
    let gone: Vec<String> = idx.keys().filter(|k| !theirs.contains_key(*k) && (!light || !k.contains('/'))).cloned().collect();
    for rel in gone {
        let _ = std::fs::remove_file(m.root.join(&rel));
        idx.remove(&rel);
    }
    save_index(&m.root, &idx);
    if !light {
        let mut all = MIRRORS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(x) = all.iter_mut().find(|(x, _)| x.root == m.root) {
            x.1 = Some(Instant::now());
        }
    }
    Ok(got)
}

fn part_of(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// `p` inside the mirror, as a path inside the server folder.
fn rel_in(m: &Mirror, p: &Path) -> Option<String> {
    let rel = p.strip_prefix(&m.root).ok()?;
    let s = rel.to_string_lossy().replace('\\', "/");
    (!s.is_empty() && !s.starts_with(crate::sync::BACKUP_DIR)).then_some(s)
}

/// Plays the sync's changes to the mirror back on the server. A result whose
/// change didn't reach the server is marked failed and says so.
pub fn push(m: &Mirror, results: &mut [SyncResult]) {
    let touches = results.iter().any(|x| x.ok && server_rel(m, &x.op).is_some());
    if !touches {
        return;
    }
    let mut r = match connect(&m.site) {
        Ok(r) => r,
        Err(e) => {
            fail_all(m, results, &format!("{e:#}"));
            return;
        }
    };
    push_with(&mut *r, m, results);
}

/// The path on the server a sync result changed, when it changed the server.
fn server_rel(m: &Mirror, op: &SyncOp) -> Option<String> {
    match op {
        SyncOp::Copy { dst, .. } => rel_in(m, dst),
        SyncOp::Delete { path } => rel_in(m, path),
    }
}

fn fail_all(m: &Mirror, results: &mut [SyncResult], why: &str) {
    for x in results.iter_mut().filter(|x| x.ok && server_rel(m, &x.op).is_some()) {
        x.ok = false;
        x.error = Some(format!("Changed in Hopper's copy but not on the server: {why}"));
    }
}

pub fn push_with(r: &mut dyn Remote, m: &Mirror, results: &mut [SyncResult]) {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let backups = join(&m.folder, &format!("{}/{stamp}", crate::sync::BACKUP_DIR));
    let mut idx = load_index(&m.root);
    let mut made_backups = false;
    let mut touched_dirs: Vec<String> = Vec::new();
    for x in results.iter_mut().filter(|x| x.ok) {
        let Some(rel) = server_rel(m, &x.op) else { continue };
        let remote = join(&m.folder, &rel);
        let dir = rel.rsplit_once('/').map(|d| d.0.to_string()).unwrap_or_default();
        let step = (|| -> anyhow::Result<()> {
            // What's there now goes to the server's backups first.
            if idx.contains_key(&rel) {
                if !made_backups {
                    r.mkdirs(&backups)?;
                    made_backups = true;
                }
                let name = rel.replace('/', "_");
                r.rename(&remote, &join(&backups, &name)).with_context(|| format!("keeping a backup of {rel}"))?;
                idx.remove(&rel);
            }
            if let SyncOp::Copy { dst, .. } = &x.op {
                if !dir.is_empty() {
                    r.mkdirs(&join(&m.folder, &dir))?;
                }
                r.put(dst, &remote).with_context(|| format!("uploading {rel}"))?;
            }
            Ok(())
        })();
        match step {
            Ok(()) => {
                if !touched_dirs.contains(&dir) {
                    touched_dirs.push(dir);
                }
            }
            Err(e) => {
                x.ok = false;
                x.error = Some(format!("Changed in Hopper's copy but not on the server: {e:#}"));
            }
        }
    }
    // The new files' sizes and times as the server has them, so the next
    // refresh doesn't fetch them back.
    for dir in touched_dirs {
        let Ok(list) = r.list(&join(&m.folder, &dir)) else { continue };
        for e in list.into_iter().filter(|e| !e.dir) {
            let rel = if dir.is_empty() { e.name.clone() } else { format!("{dir}/{}", e.name) };
            if m.root.join(&rel).is_file() && wanted(&rel, false, e.size) {
                idx.insert(rel, (e.size, e.mtime));
            }
        }
    }
    save_index(&m.root, &idx);
    if made_backups {
        log::info!("{}: replaced and removed files kept in {backups} on the server", m.site.host);
    }
}

/// The servers on `site`, each with a local copy of its top-level files
/// (server.properties, start scripts), and the SSH key seen when the site
/// has none pinned yet. Mods and configs come down when they're compared.
pub fn discover(site: &FileSite) -> anyhow::Result<(Vec<crate::server::ServerInfo>, Option<String>)> {
    let mut r = connect(site)?;
    let mut out = Vec::new();
    for folder in servers(&mut *r, &site.folder)? {
        let m = Mirror { root: mirror_root(&site.id, &folder), site: site.clone(), folder };
        register(&m);
        fetch(&mut *r, &m, true)?;
        out.push(crate::server::load(&m.root));
    }
    let key = r.host_key().filter(|_| site.host_key.is_empty());
    Ok((out, key))
}

/// Tries a site: connects and lists its folder. Returns the servers found
/// and the SSH key seen (for pinning).
pub fn test(site: &FileSite) -> anyhow::Result<(Vec<String>, Option<String>)> {
    let mut r = connect(site)?;
    let found = servers(&mut *r, &site.folder)?;
    Ok((found, r.host_key()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A server held in memory.
    #[derive(Default)]
    struct Fake {
        files: HashMap<String, (Vec<u8>, u64)>,
        dirs: std::collections::HashSet<String>,
        clock: u64,
        log: Vec<String>,
    }

    impl Fake {
        fn add(&mut self, path: &str, body: &str) {
            self.clock += 1;
            self.files.insert(path.into(), (body.as_bytes().to_vec(), self.clock));
            let mut d = path.rsplit_once('/').map(|x| x.0).unwrap_or("");
            while !d.is_empty() {
                self.dirs.insert(d.to_string());
                d = d.rsplit_once('/').map(|x| x.0).unwrap_or("");
            }
        }
    }

    impl Remote for Fake {
        fn list(&mut self, dir: &str) -> anyhow::Result<Vec<Entry>> {
            let dir = dir.trim_end_matches('/');
            if !self.dirs.contains(dir) {
                bail!("no such folder {dir}");
            }
            let mut out = Vec::new();
            for (p, (b, t)) in &self.files {
                if let Some(name) = p.strip_prefix(&format!("{dir}/")).filter(|n| !n.contains('/')) {
                    out.push(Entry { name: name.into(), dir: false, size: b.len() as u64, mtime: *t });
                }
            }
            for d in &self.dirs {
                if let Some(name) = d.strip_prefix(&format!("{dir}/")).filter(|n| !n.contains('/')) {
                    out.push(Entry { name: name.into(), dir: true, size: 0, mtime: 0 });
                }
            }
            Ok(out)
        }
        fn get(&mut self, path: &str, local: &Path) -> anyhow::Result<()> {
            self.log.push(format!("get {path}"));
            std::fs::write(local, &self.files.get(path).context("missing")?.0)?;
            Ok(())
        }
        fn put(&mut self, local: &Path, path: &str) -> anyhow::Result<()> {
            self.log.push(format!("put {path}"));
            let body = std::fs::read_to_string(local)?;
            self.add(path, &body);
            Ok(())
        }
        fn rename(&mut self, from: &str, to: &str) -> anyhow::Result<()> {
            self.log.push(format!("rename {from} -> {to}"));
            let f = self.files.remove(from).context("missing")?;
            self.files.insert(to.into(), f);
            Ok(())
        }
        fn mkdirs(&mut self, dir: &str) -> anyhow::Result<()> {
            let mut d = dir.to_string();
            while !d.is_empty() {
                self.dirs.insert(d.clone());
                d = d.rsplit_once('/').map(|x| x.0.to_string()).unwrap_or_default();
            }
            Ok(())
        }
    }

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopper-remotefs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// Against a real server: `HOPPER_FILES_TEST=sftp|ftp|ftps,host,port,user,pass,folder`
    /// (a throwaway server with a server folder in `folder`). Lists it, copies
    /// it, uploads a jar, replaces it and removes it again, then checks the
    /// server's backups folder has the old ones.
    #[test]
    #[ignore]
    fn against_a_real_server() {
        let spec = std::env::var("HOPPER_FILES_TEST").expect("HOPPER_FILES_TEST");
        let p: Vec<&str> = spec.split(',').collect();
        let site = FileSite {
            id: format!("test{}", std::process::id()),
            proto: p[0].into(),
            host: p[1].into(),
            port: p[2].parse().unwrap(),
            user: p[3].into(),
            folder: p[5].into(),
            self_signed: true,
            ..Default::default()
        };
        set_secret(&site.id, p[4]).unwrap();
        let started = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let run = || -> anyhow::Result<()> {
            let (found, key) = test(&site)?;
            println!("servers: {found:?}, key: {key:?}");
            let folder = found.first().context("no server found")?.clone();
            let mut r = connect(&site)?;
            let m = Mirror { root: temp("real"), site: site.clone(), folder: folder.clone() };
            register(&m);
            let n = refresh_with(&mut *r, &m)?;
            println!("fetched {n}");
            assert_eq!(refresh_with(&mut *r, &m)?, 0, "a second refresh fetches nothing");
            let inst = temp("real-inst");
            std::fs::create_dir_all(&inst)?;
            std::fs::write(inst.join("hopper-test-1.jar"), "one")?;
            std::fs::write(inst.join("hopper-test-2.jar"), "two")?;
            let mods = m.root.join("mods");
            // Add, then replace, then remove: each a sync run.
            for ops in [
                vec![SyncOp::Copy { src: inst.join("hopper-test-1.jar"), dst: mods.join("hopper-test-1.jar") }],
                vec![
                    SyncOp::Delete { path: mods.join("hopper-test-1.jar") },
                    SyncOp::Copy { src: inst.join("hopper-test-2.jar"), dst: mods.join("hopper-test-2.jar") },
                ],
                vec![SyncOp::Delete { path: mods.join("hopper-test-2.jar") }],
            ] {
                let res = crate::sync::execute(ops, &m.root);
                assert!(res.iter().all(|x| x.ok), "{res:?}");
                assert_eq!(refresh_with(&mut *r, &m)?, 0, "uploads are known");
                std::thread::sleep(Duration::from_millis(1100));
            }
            let names: Vec<String> = r.list(&join(&folder, "mods"))?.into_iter().map(|e| e.name).collect();
            assert!(!names.iter().any(|n| n.starts_with("hopper-test")), "{names:?}");
            let backups = r.list(&join(&folder, crate::sync::BACKUP_DIR))?;
            println!("backups: {:?}", backups.iter().map(|e| &e.name).collect::<Vec<_>>());
            // Only this run's backups (a re-run on the same server has older ones).
            let kept: usize = backups
                .iter()
                .filter(|b| b.dir && b.name.parse::<u64>().is_ok_and(|t| t >= started))
                .map(|b| r.list(&join(&folder, &format!("{}/{}", crate::sync::BACKUP_DIR, b.name))).map_or(0, |l| l.iter().filter(|e| e.name.contains("hopper-test")).count()))
                .sum();
            assert_eq!(kept, 2, "both test jars kept in the server's backups");
            let _ = std::fs::remove_dir_all(&m.root);
            let _ = std::fs::remove_dir_all(&inst);
            Ok(())
        };
        let r = run();
        let _ = set_secret(&site.id, "");
        r.unwrap();
    }

    #[test]
    fn keeps_only_what_sync_reads() {
        assert!(wanted("mods", true, 0));
        assert!(wanted("mods/create-6.0.jar", false, 30 << 20));
        assert!(!wanted("mods/.connector/x.jar", false, 1));
        assert!(wanted("config/create-common.toml", false, 100));
        assert!(wanted("server.properties", false, 100));
        assert!(wanted("run.bat", false, 100));
        assert!(!wanted("world", true, 0));
        assert!(!wanted("libraries", true, 0));
        assert!(!wanted("server.jar", false, 100));
        assert!(wanted("logs/latest.log", false, 100));
        assert!(!wanted("logs/2026-10-01-1.log.gz", false, 100));
        assert!(!wanted(".hopper-backups", true, 0));
    }

    #[test]
    fn finds_servers_in_a_folder() {
        let mut f = Fake::default();
        f.add("/home/mc/servers/TNP/server.properties", "x");
        f.add("/home/mc/servers/ATM/server.properties", "x");
        f.add("/home/mc/servers/junk/readme.txt", "x");
        assert_eq!(servers(&mut f, "/home/mc/servers").unwrap(), vec!["/home/mc/servers/ATM", "/home/mc/servers/TNP"]);
        assert_eq!(servers(&mut f, "/home/mc/servers/TNP/").unwrap(), vec!["/home/mc/servers/TNP/"]);
        assert_eq!(join("/", "a"), "/a");
        assert_eq!(join("/srv/", "/mods/x.jar"), "/srv/mods/x.jar");
    }

    #[test]
    fn mirror_fetches_only_changes_and_pushes_back_with_backups() {
        let mut f = Fake::default();
        f.add("/srv/server.properties", "server-port=25565");
        f.add("/srv/mods/a-1.jar", "A1");
        f.add("/srv/mods/b-1.jar", "B1");
        f.add("/srv/world/level.dat", "big");
        let root = temp("mirror");
        let site = FileSite { id: "s1".into(), proto: "sftp".into(), ..Default::default() };
        let m = Mirror { root: root.clone(), site, folder: "/srv".into() };
        assert_eq!(refresh_with(&mut f, &m).unwrap(), 3);
        assert!(!root.join("world").exists());
        assert_eq!(std::fs::read_to_string(root.join("mods/b-1.jar")).unwrap(), "B1");
        // Nothing changed: nothing comes down.
        f.log.clear();
        assert_eq!(refresh_with(&mut f, &m).unwrap(), 0);
        assert!(f.log.is_empty());
        // One changed on the server, one went.
        f.add("/srv/mods/a-1.jar", "A1 again");
        f.files.remove("/srv/mods/b-1.jar");
        assert_eq!(refresh_with(&mut f, &m).unwrap(), 1);
        assert!(!root.join("mods/b-1.jar").exists());

        // A sync on the mirror: a-1 replaced by a-2, plus a new c-1.
        let inst = temp("inst");
        std::fs::create_dir_all(&inst).unwrap();
        std::fs::write(inst.join("a-2.jar"), "A2").unwrap();
        std::fs::write(inst.join("c-1.jar"), "C1").unwrap();
        let ops = vec![
            SyncOp::Delete { path: root.join("mods/a-1.jar") },
            SyncOp::Copy { src: inst.join("a-2.jar"), dst: root.join("mods/a-2.jar") },
            SyncOp::Copy { src: inst.join("c-1.jar"), dst: root.join("mods/c-1.jar") },
        ];
        let mut results = crate::sync::execute_local(ops, &root);
        assert!(results.iter().all(|r| r.ok));
        f.log.clear();
        push_with(&mut f, &m, &mut results);
        assert!(results.iter().all(|r| r.ok), "{results:?}");
        assert!(!f.files.contains_key("/srv/mods/a-1.jar"));
        assert!(f.files.keys().any(|k| k.starts_with("/srv/.hopper-backups/") && k.ends_with("mods_a-1.jar")));
        assert_eq!(f.files["/srv/mods/a-2.jar"].0, b"A2");
        assert_eq!(f.files["/srv/mods/c-1.jar"].0, b"C1");
        // The uploads are known: the next refresh fetches nothing.
        f.log.clear();
        assert_eq!(refresh_with(&mut f, &m).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&inst);
    }
}
