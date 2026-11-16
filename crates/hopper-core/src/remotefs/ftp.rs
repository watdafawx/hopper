//! FTP and FTPS (explicit, `AUTH TLS`) with suppaftp, over the same rustls
//! as the rest of Hopper.

use super::{Entry, Remote};
use crate::settings::FileSite;
use anyhow::{Context as _, bail};
use std::net::ToSocketAddrs as _;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};
use suppaftp::list::File as Listed;
use suppaftp::types::FileType;

/// How an FTPS connection checks the server's certificate.
#[derive(Clone)]
pub struct Tls {
    config: Arc<rustls::ClientConfig>,
    /// Why the check refused a certificate, in words for the player.
    why: Arc<Mutex<String>>,
}

impl Tls {
    /// The usual check against the web's authorities.
    pub fn web() -> Self {
        Tls { config: crate::tls::client_config(), why: Default::default() }
    }

    /// For `site`: the usual check, or (when the server makes its own
    /// certificate) only the one the user trusted. A site saved before
    /// pinning trusts the first one it's shown, like panels.
    pub fn of(site: &FileSite) -> Self {
        if !site.self_signed {
            return Self::web();
        }
        let (id, pin, ask) = (site.id.clone(), site.cert_pin.trim().to_string(), site.cert_ask);
        let why: Arc<Mutex<String>> = Default::default();
        let why2 = why.clone();
        let config = crate::tls::pinned_config(Arc::new(move |fp: &str, vouched| {
            let r = crate::certpin::check(crate::certpin::Subject::Files, &id, &pin, ask, fp, vouched);
            if let Err(e) = &r {
                *why2.lock().unwrap_or_else(|e| e.into_inner()) = e.clone();
            }
            r
        }));
        Tls { config, why }
    }

    fn refused(&self) -> Option<String> {
        Some(self.why.lock().unwrap_or_else(|e| e.into_inner()).clone()).filter(|w| !w.is_empty())
    }
}

pub struct Ftp {
    s: suppaftp::RustlsFtpStream,
    /// MLSD works (exact times); else LIST, parsed.
    mlsd: bool,
}

/// Opens the control connection and, for FTPS, starts TLS. suppaftp sends
/// PBSZ and PROT right after AUTH TLS, which makes the handshake happen
/// here: a certificate that's refused stops this call, before any user
/// name or password is sent.
fn open(host: &str, port: u16, tls: Option<&Tls>) -> anyhow::Result<suppaftp::RustlsFtpStream> {
    let addr = (host, port).to_socket_addrs()?.next().with_context(|| format!("{host} not found"))?;
    let mut s = suppaftp::RustlsFtpStream::connect_timeout(addr, Duration::from_secs(15))
        .map_err(|e| anyhow::anyhow!("{host}:{port}: {e}"))?;
    if let Some(tls) = tls {
        let conn = suppaftp::RustlsConnector::from(tls.config.clone());
        s = s.into_secure(conn, host).map_err(|e| match tls.refused() {
            // A certificate that wasn't trusted says why in plain words.
            Some(why) => anyhow::anyhow!(why),
            None => anyhow::anyhow!("{host} didn't start TLS ({e}). If it only speaks plain FTP, pick FTP (not secure)."),
        })?;
    }
    Ok(s)
}

/// The certificate the FTPS server at `host:port` answers with, found
/// before any user name or password is sent. None: an authority signs it for
/// that address, so there's nothing to trust.
pub fn probe_cert(host: &str, port: u16) -> anyhow::Result<Option<String>> {
    let seen: Arc<Mutex<String>> = Default::default();
    let seen2 = seen.clone();
    let why: Arc<Mutex<String>> = Default::default();
    let config = crate::tls::pinned_config(Arc::new(move |fp: &str, vouched| {
        if vouched {
            return Ok(());
        }
        *seen2.lock().unwrap_or_else(|e| e.into_inner()) = fp.to_string();
        Err("only looking at the certificate".to_string())
    }));
    let r = open(host, port, Some(&Tls { config, why }));
    let fp = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if !fp.is_empty() {
        return Ok(Some(fp));
    }
    r.map(|_| None)
}

impl Ftp {
    /// `tls`: None for plain FTP, Some for FTPS.
    pub fn connect(host: &str, port: u16, user: &str, pass: &str, tls: Option<&Tls>) -> anyhow::Result<Self> {
        let mut s = open(host, port, tls)?;
        s.login(user, pass).map_err(|e| {
            if e.to_string().to_lowercase().contains("encryption") {
                anyhow::anyhow!("This server only allows FTP with TLS: pick FTPS.")
            } else {
                anyhow::anyhow!("The FTP server refused the user name or password ({e}).")
            }
        })?;
        s.transfer_type(FileType::Binary)?;
        let mlsd = s.feat().map(|f| f.contains_key("MLST")).unwrap_or(false);
        Ok(Ftp { s, mlsd })
    }
}

fn entry(f: &Listed) -> Entry {
    Entry {
        name: f.name().to_string(),
        dir: f.is_directory(),
        size: f.size() as u64,
        mtime: f.modified().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()),
    }
}

impl Remote for Ftp {
    fn list(&mut self, dir: &str) -> anyhow::Result<Vec<Entry>> {
        let mut out = Vec::new();
        if self.mlsd {
            for line in self.s.mlsd(Some(dir)).with_context(|| format!("listing {dir}"))? {
                if let Ok(f) = suppaftp::list::ListParser::parse_mlsd(&line) {
                    out.push(entry(&f));
                }
            }
        } else {
            for line in self.s.list(Some(dir)).with_context(|| format!("listing {dir}"))? {
                let f = suppaftp::list::ListParser::parse_posix(&line).or_else(|_| suppaftp::list::ListParser::parse_dos(&line));
                if let Ok(f) = f {
                    out.push(entry(&f));
                }
            }
        }
        out.retain(|e| e.name != "." && e.name != "..");
        Ok(out)
    }

    fn get(&mut self, path: &str, local: &Path) -> anyhow::Result<()> {
        let mut out = std::fs::File::create(local)?;
        self.s.retr(path, |r| std::io::copy(r, &mut out).map_err(suppaftp::FtpError::ConnectionError))?;
        Ok(())
    }

    fn put(&mut self, local: &Path, path: &str) -> anyhow::Result<()> {
        let mut src = std::fs::File::open(local)?;
        let part = format!("{path}.part");
        self.s.put_file(&part, &mut src)?;
        // Some servers won't rename onto an existing file.
        let _ = self.s.rm(path);
        self.s.rename(part.as_str(), path)?;
        Ok(())
    }

    fn rename(&mut self, from: &str, to: &str) -> anyhow::Result<()> {
        self.s.rename(from, to)?;
        Ok(())
    }

    fn mkdirs(&mut self, dir: &str) -> anyhow::Result<()> {
        let mut cur = String::new();
        for part in dir.split('/').filter(|p| !p.is_empty()) {
            cur = if dir.starts_with('/') || !cur.is_empty() { format!("{cur}/{part}") } else { part.to_string() };
            // Already there is fine; anything else shows on the upload.
            let _ = self.s.mkdir(&cur);
        }
        if cur.is_empty() {
            bail!("No folder given.");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SyncOp;
    use crate::remotefs::{Mirror, refresh_with, servers};

    /// A free port for the test server. Fixed ports made two test runs at once
    /// (several sessions, or a run and CI-like reruns) talk to each other's
    /// server and hang or fail.
    /// A free control port from 20000-29999, apart from the passive ranges
    /// and below the ports the system hands out by itself (49152 up on
    /// Windows, 32768 up on Linux): one of those could become another test
    /// server's passive port, and a test would then talk to the wrong server.
    fn free_port() -> u16 {
        static TAKEN: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
        let mut p = 20000 + (std::process::id() % 9000) as u16 + TAKEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        while std::net::TcpListener::bind(("127.0.0.1", p)).is_err() {
            p = if p >= 29999 { 20000 } else { p + 1 };
        }
        p
    }

    /// `n` free ports in a row for passive data connections, starting at a
    /// spot that depends on this process so parallel runs rarely overlap.
    fn free_range(n: u16) -> std::ops::RangeInclusive<u16> {
        // The tests of this process run side by side: each takes its own slice.
        static TAKEN: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
        // From 30000 up, above `free_port`'s block, so a passive port never
        // takes another test server's control port.
        let mut start = 30000 + (std::process::id() % 15000) as u16 + TAKEN.fetch_add(n + 1, std::sync::atomic::Ordering::Relaxed);
        loop {
            if (start..start + n).all(|p| std::net::TcpListener::bind(("127.0.0.1", p)).is_ok()) {
                return start..=start + n - 1;
            }
            start += n + 1;
        }
    }

    /// An FTP server on localhost over `root` (any user and password), with
    /// TLS when `cert` is given.
    fn serve(root: std::path::PathBuf, port: u16, passive: std::ops::RangeInclusive<u16>, cert: Option<(std::path::PathBuf, std::path::PathBuf)>) {
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
            rt.block_on(async move {
                let mut b = libunftp::ServerBuilder::new(Box::new(move || unftp_sbe_fs::Filesystem::new(root.clone()).unwrap()))
                    .passive_ports(passive);
                if let Some((c, k)) = cert {
                    b = b.ftps(c, k);
                }
                b.build().unwrap().listen(format!("127.0.0.1:{port}")).await.unwrap();
            });
        });
        for _ in 0..50 {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("FTP server didn't start");
    }

    fn temp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("hopper-ftp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn round_trip(mut r: Ftp, tag: &str) {
        let mut found = servers(&mut r, "/").unwrap();
        found.sort();
        assert_eq!(found, vec!["/srv"]);
        let root = temp(&format!("{tag}-mirror"));
        let site = crate::settings::FileSite { id: format!("ftp-{tag}"), proto: "ftp".into(), ..Default::default() };
        let m = Mirror { root: root.clone(), site, folder: "/srv".into() };
        assert_eq!(refresh_with(&mut r, &m).unwrap(), 2);
        assert_eq!(std::fs::read_to_string(root.join("mods/a-1.jar")).unwrap(), "A1");
        assert_eq!(refresh_with(&mut r, &m).unwrap(), 0);
        let inst = temp(&format!("{tag}-inst"));
        std::fs::write(inst.join("a-2.jar"), "A2").unwrap();
        let ops = vec![
            SyncOp::Delete { path: root.join("mods/a-1.jar") },
            SyncOp::Copy { src: inst.join("a-2.jar"), dst: root.join("mods/a-2.jar") },
        ];
        let mut res = crate::sync::execute_local(ops, &root);
        crate::remotefs::push_with(&mut r, &m, &mut res);
        assert!(res.iter().all(|x| x.ok), "{res:?}");
        let names: Vec<String> = r.list("/srv/mods").unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, vec!["a-2.jar"]);
        let mut a2 = String::new();
        r.s.retr("/srv/mods/a-2.jar", |rd| rd.read_to_string(&mut a2).map_err(suppaftp::FtpError::ConnectionError)).unwrap();
        assert_eq!(a2, "A2");
        let kept = r.list("/srv/.hopper-backups").unwrap();
        assert_eq!(kept.len(), 1);
        let inside = r.list(&format!("/srv/.hopper-backups/{}", kept[0].name)).unwrap();
        assert_eq!(inside[0].name, "mods_a-1.jar");
        assert_eq!(refresh_with(&mut r, &m).unwrap(), 0, "the upload is known");
    }

    fn server_tree(tag: &str) -> std::path::PathBuf {
        let srv = temp(tag);
        std::fs::create_dir_all(srv.join("srv/mods")).unwrap();
        std::fs::write(srv.join("srv/server.properties"), "server-port=25565").unwrap();
        std::fs::write(srv.join("srv/mods/a-1.jar"), "A1").unwrap();
        std::fs::create_dir_all(srv.join("other")).unwrap();
        srv
    }

    #[test]
    fn plain_ftp_round_trip() {
        let port = free_port();
        serve(server_tree("plain"), port, free_range(21), None);
        round_trip(Ftp::connect("127.0.0.1", port, "u", "p", None).unwrap(), "plain");
    }

    /// A self-made certificate (as a home server's FTPS has) and the PEM files
    /// for the test server: (cert file, key file, its fingerprint).
    fn made_cert(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, String) {
        let dir = temp(tag);
        let ck = rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
        let (c, k) = (dir.join("cert.pem"), dir.join("key.pem"));
        std::fs::write(&c, ck.cert.pem()).unwrap();
        std::fs::write(&k, ck.key_pair.serialize_pem()).unwrap();
        (c, k, crate::tls::fingerprint(ck.cert.der()))
    }

    fn site(id: &str, port: u16, pin: &str) -> FileSite {
        FileSite { id: id.into(), proto: "ftps".into(), host: "127.0.0.1".into(), port, self_signed: true, cert_pin: pin.into(), ..Default::default() }
    }

    #[test]
    fn ftps_round_trip() {
        let (c, k, fp) = made_cert("cert");
        let port = free_port();
        serve(server_tree("tls"), port, free_range(21), Some((c, k)));
        // Its own certificate: refused by the usual check.
        assert!(Ftp::connect("127.0.0.1", port, "u", "p", Some(&Tls::web())).is_err());
        // Trusted by its fingerprint: connects, lists and copies (every data
        // connection is checked against the same certificate).
        let s = site("t-ftp-trip", port, &fp);
        round_trip(Ftp::connect("127.0.0.1", port, "u", "p", Some(&Tls::of(&s))).unwrap(), "tls");
        assert!(crate::certpin::cert_changed(crate::certpin::Subject::Files, &s.id).is_none());
    }

    /// An FTPS server on 127.0.0.1 behind a certificate it made itself. It
    /// answers just enough to sign in and records every command it's sent
    /// once TLS is up, so a test can tell what a refused certificate kept
    /// from it. Returns (port, the certificate's fingerprint, the commands).
    fn fake_ftps() -> (u16, String, Arc<Mutex<Vec<String>>>) {
        use std::io::{BufRead as _, BufReader, Write as _};
        let made = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()]).unwrap();
        let fp = crate::tls::fingerprint(made.cert.der());
        let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![made.cert.der().clone()], rustls::pki_types::PrivateKeyDer::Pkcs8(made.key_pair.serialize_der().into()))
            .unwrap();
        let cfg = Arc::new(cfg);
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let log = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        std::thread::spawn(move || {
            for c in l.incoming().flatten() {
                let (cfg, log) = (cfg.clone(), log2.clone());
                std::thread::spawn(move || {
                    let _ = c.set_read_timeout(Some(Duration::from_secs(5)));
                    let mut c = c;
                    let _ = c.write_all(b"220 ready\r\n");
                    let mut line = String::new();
                    // Nothing follows AUTH TLS until the server answers it.
                    let mut plain = BufReader::new(c.try_clone().unwrap());
                    if plain.read_line(&mut line).unwrap_or(0) == 0 || !line.to_uppercase().starts_with("AUTH TLS") {
                        return;
                    }
                    let _ = c.write_all(b"234 go\r\n");
                    let Ok(conn) = rustls::ServerConnection::new(cfg) else { return };
                    let mut r = BufReader::new(rustls::StreamOwned::new(conn, c));
                    loop {
                        line.clear();
                        if r.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        let cmd = line.trim().to_string();
                        log.lock().unwrap().push(cmd.clone());
                        let reply: &[u8] = match cmd.split(' ').next().unwrap_or("").to_uppercase().as_str() {
                            "USER" => b"331 password please\r\n",
                            "PASS" => b"230 in\r\n",
                            "NOOP" | "TYPE" | "PBSZ" | "PROT" => b"200 ok\r\n",
                            "QUIT" => b"221 bye\r\n",
                            _ => b"502 no\r\n",
                        };
                        let s = r.get_mut();
                        let _ = s.write_all(reply);
                        let _ = s.flush();
                    }
                });
            }
        });
        (port, fp, log)
    }

    fn sent(log: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        std::thread::sleep(Duration::from_millis(150));
        log.lock().unwrap().clone()
    }

    #[test]
    fn a_new_self_signed_site_sends_nothing_until_its_certificate_is_trusted() {
        let (port, fp, log) = fake_ftps();
        let mut s = FileSite { cert_ask: true, ..site("t-ftps-ask", port, "") };
        let e = Ftp::connect("127.0.0.1", port, "u", "secret", Some(&Tls::of(&s))).err().expect("refused");
        assert!(format!("{e:#}").contains("Trust the server's certificate first"), "{e:#}");
        // The form's look at the certificate sends nothing either.
        assert_eq!(probe_cert("127.0.0.1", port).unwrap().as_deref(), Some(fp.as_str()));
        assert!(sent(&log).is_empty(), "no user name or password reached the server: {:?}", sent(&log));
        // Trusted: it signs in.
        s.cert_pin = fp;
        s.cert_ask = false;
        Ftp::connect("127.0.0.1", port, "u", "secret", Some(&Tls::of(&s))).map(|_| ()).unwrap();
        let got = sent(&log);
        assert!(got.contains(&"USER u".to_string()) && got.contains(&"PASS secret".to_string()), "{got:?}");
        assert!(crate::certpin::cert_changed(crate::certpin::Subject::Files, &s.id).is_none());
        assert!(crate::certpin::take_learned_of(crate::certpin::Subject::Files, |i| i == s.id).is_empty(), "a trusted pin isn't learned");
    }

    #[test]
    fn a_pinned_site_with_a_new_certificate_is_refused_before_the_password() {
        use crate::certpin::{Subject, cert_changed, forget};
        let (port, fp, log) = fake_ftps();
        let was = crate::tls::fingerprint(b"the certificate it had");
        let s = site("t-ftps-changed", port, &was);
        let e = Ftp::connect("127.0.0.1", port, "u", "secret", Some(&Tls::of(&s))).err().expect("refused");
        let e = format!("{e:#}");
        assert!(e.contains("certificate changed") && e.contains(&fp) && e.contains(&was), "{e}");
        assert!(!e.contains("refused the user name"), "{e}");
        assert_eq!(cert_changed(Subject::Files, &s.id), Some((was, fp.clone())));
        assert!(sent(&log).is_empty(), "nothing was sent: {:?}", sent(&log));
        // Trusting the new one works and clears the warning.
        let s = FileSite { cert_pin: fp.to_lowercase(), ..s };
        Ftp::connect("127.0.0.1", port, "u", "secret", Some(&Tls::of(&s))).map(|_| ()).unwrap();
        assert!(cert_changed(Subject::Files, &s.id).is_none());
        forget(Subject::Files, &s.id);
    }

    #[test]
    fn a_site_saved_before_pinning_pins_on_its_next_connect() {
        use crate::certpin::{Subject, cert_changed, forget, take_learned_of};
        let (port, fp, log) = fake_ftps();
        // As read from settings.json of an older build: no pin, not asking.
        let s: FileSite = serde_json::from_value(
            serde_json::json!({"id": "t-ftps-old", "proto": "ftps", "host": "127.0.0.1", "port": port, "self_signed": true}),
        )
        .unwrap();
        assert!(s.cert_pin.is_empty() && !s.cert_ask);
        Ftp::connect("127.0.0.1", port, "u", "p", Some(&Tls::of(&s))).map(|_| ()).unwrap();
        assert!(sent(&log).contains(&"PASS p".to_string()), "a working setup keeps working");
        assert_eq!(take_learned_of(Subject::Files, |i| i == s.id), vec![(s.id.clone(), Some(fp.clone()))]);
        assert!(take_learned_of(Subject::Files, |i| i == s.id).is_empty(), "told once");
        // From then on, even before the settings are saved, only that one.
        let (port2, fp2, log2) = fake_ftps();
        let again = FileSite { port: port2, ..s.clone() };
        let e = Ftp::connect("127.0.0.1", port2, "u", "p", Some(&Tls::of(&again))).err().expect("refused");
        assert!(format!("{e:#}").contains("certificate changed"), "{e:#}");
        assert_eq!(cert_changed(Subject::Files, &s.id), Some((fp.clone(), fp2)));
        assert!(sent(&log2).is_empty());
        assert!(Ftp::connect("127.0.0.1", port, "u", "p", Some(&Tls::of(&s))).is_ok());
        forget(Subject::Files, &s.id);
    }

    #[test]
    fn a_site_not_marked_self_signed_keeps_the_usual_check() {
        let (port, _, log) = fake_ftps();
        let s = FileSite { self_signed: false, ..site("t-ftps-web", port, "") };
        assert!(Ftp::connect("127.0.0.1", port, "u", "p", Some(&Tls::of(&s))).is_err());
        assert!(sent(&log).is_empty());
        assert!(crate::certpin::take_learned_of(crate::certpin::Subject::Files, |i| i == s.id).is_empty());
    }

    #[test]
    fn pin_fields_are_saved_only_when_set() {
        let v = serde_json::to_value(FileSite::default()).unwrap();
        assert!(v.get("cert_pin").is_none() && v.get("cert_ask").is_none());
        let s = FileSite { cert_pin: "AB".into(), cert_ask: true, ..Default::default() };
        let back: FileSite = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert_eq!((back.cert_pin.as_str(), back.cert_ask), ("AB", true));
    }
}
