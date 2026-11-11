//! FTP and FTPS (explicit, `AUTH TLS`) with suppaftp, over the same rustls
//! as the rest of Hopper.

use super::{Entry, Remote};
use anyhow::{Context as _, bail};
use std::net::ToSocketAddrs as _;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};
use suppaftp::list::File as Listed;
use suppaftp::types::FileType;

pub struct Ftp {
    s: suppaftp::RustlsFtpStream,
    /// MLSD works (exact times); else LIST, parsed.
    mlsd: bool,
}

impl Ftp {
    /// `tls`: None for plain FTP, Some(self_signed) for FTPS.
    pub fn connect(host: &str, port: u16, user: &str, pass: &str, tls: Option<bool>) -> anyhow::Result<Self> {
        let addr = (host, port).to_socket_addrs()?.next().with_context(|| format!("{host} not found"))?;
        let mut s = suppaftp::RustlsFtpStream::connect_timeout(addr, Duration::from_secs(15))
            .map_err(|e| anyhow::anyhow!("{host}:{port}: {e}"))?;
        if let Some(self_signed) = tls {
            let conn = suppaftp::RustlsConnector::from(crate::tls::client_config(self_signed));
            s = s.into_secure(conn, host).map_err(|e| {
                anyhow::anyhow!("{host} didn't start TLS ({e}). If it only speaks plain FTP, pick FTP (not secure).")
            })?;
        }
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

    #[test]
    fn ftps_round_trip() {
        let dir = temp("cert");
        let ck = rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
        let (c, k) = (dir.join("cert.pem"), dir.join("key.pem"));
        std::fs::write(&c, ck.cert.pem()).unwrap();
        std::fs::write(&k, ck.key_pair.serialize_pem()).unwrap();
        let port = free_port();
        serve(server_tree("tls"), port, free_range(21), Some((c, k)));
        // Its own certificate: refused unless the site says self-signed.
        assert!(Ftp::connect("127.0.0.1", port, "u", "p", Some(false)).is_err());
        round_trip(Ftp::connect("127.0.0.1", port, "u", "p", Some(true)).unwrap(), "tls");
    }
}

