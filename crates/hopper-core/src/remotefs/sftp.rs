//! SFTP with russh (the same crates Hopper Server's SFTP uses), behind the
//! blocking [`Remote`] calls: one small tokio runtime per connection.

use super::{Entry, Remote};
use anyhow::{Context as _, bail};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct KeyCheck {
    want: String,
    seen: Arc<Mutex<String>>,
}

impl russh::client::Handler for KeyCheck {
    type Error = russh::Error;
    async fn check_server_key(&mut self, key: &russh::keys::PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let fp = match key {
            russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } => key.fingerprint(russh::keys::HashAlg::Sha256).to_string(),
            russh::keys::PublicKeyOrCertificate::Certificate(c) => c.public_key().fingerprint(russh::keys::HashAlg::Sha256).to_string(),
        };
        *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = fp.clone();
        // An unpinned site learns the key; a pinned one must match it.
        Ok(self.want.is_empty() || self.want == fp)
    }
}

pub struct Sftp {
    rt: tokio::runtime::Runtime,
    sftp: russh_sftp::client::SftpSession,
    _ssh: russh::client::Handle<KeyCheck>,
    key: String,
}

impl Sftp {
    /// Signs in with `key_file` (its passphrase in `pass`) when given, else
    /// with the password. `pinned`: the host key it must have, if known.
    pub fn connect(host: &str, port: u16, user: &str, pass: &str, key_file: &str, pinned: &str) -> anyhow::Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let seen = Arc::new(Mutex::new(String::new()));
        let check = KeyCheck { want: pinned.trim().to_string(), seen: seen.clone() };
        let key = if key_file.is_empty() {
            None
        } else {
            let phrase = (!pass.is_empty()).then_some(pass);
            Some(russh::keys::load_secret_key(key_file, phrase).with_context(|| format!("reading the key {key_file}"))?)
        };
        let (host, user, pass) = (host.to_string(), user.to_string(), pass.to_string());
        let r = rt.block_on(async move {
            let cfg = Arc::new(russh::client::Config { inactivity_timeout: Some(Duration::from_secs(120)), ..Default::default() });
            let mut h = tokio::time::timeout(Duration::from_secs(20), russh::client::connect(cfg, (host.as_str(), port), check))
                .await
                .map_err(|_| anyhow::anyhow!("{host}:{port} didn't answer"))?
                .map_err(|e| anyhow::anyhow!("{host}:{port}: {e}"))?;
            let auth = match key {
                Some(k) => {
                    let hash = h.best_supported_rsa_hash().await.ok().flatten().flatten();
                    h.authenticate_publickey(user, russh::keys::PrivateKeyWithHashAlg::new(Arc::new(k), hash)).await?
                }
                None => h.authenticate_password(user, pass).await?,
            };
            if !matches!(auth, russh::client::AuthResult::Success) {
                bail!("The SFTP server refused the user name, password or key.");
            }
            let ch = h.channel_open_session().await?;
            ch.request_subsystem(true, "sftp").await?;
            let s = russh_sftp::client::SftpSession::new(ch.into_stream()).await?;
            anyhow::Ok((s, h))
        });
        let key = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let (sftp, ssh) = r.map_err(|e| {
            if !pinned.is_empty() && !key.is_empty() && key != pinned.trim() {
                anyhow::anyhow!(
                    "The server's key changed (now {key}, was {pinned}). If you expect that (a reinstall), clear the key under Edit and connect again."
                )
            } else {
                e
            }
        })?;
        Ok(Sftp { rt, sftp, _ssh: ssh, key })
    }
}

impl Remote for Sftp {
    fn list(&mut self, dir: &str) -> anyhow::Result<Vec<Entry>> {
        let rd = self.rt.block_on(self.sftp.read_dir(dir.to_string())).with_context(|| format!("listing {dir}"))?;
        Ok(rd
            .filter(|e| e.file_name() != "." && e.file_name() != "..")
            .map(|e| {
                let m = e.metadata();
                Entry { name: e.file_name(), dir: e.file_type().is_dir(), size: m.size.unwrap_or(0), mtime: m.mtime.unwrap_or(0) as u64 }
            })
            .collect())
    }

    fn get(&mut self, path: &str, local: &Path) -> anyhow::Result<()> {
        use tokio::io::AsyncReadExt as _;
        let mut out = std::fs::File::create(local)?;
        self.rt.block_on(async {
            let mut f = self.sftp.open(path.to_string()).await?;
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                let n = f.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                std::io::Write::write_all(&mut out, &buf[..n])?;
            }
            anyhow::Ok(())
        })
    }

    fn put(&mut self, local: &Path, path: &str) -> anyhow::Result<()> {
        use tokio::io::AsyncWriteExt as _;
        let mut src = std::fs::File::open(local)?;
        let part = format!("{path}.part");
        self.rt.block_on(async {
            let mut f = self.sftp.create(part.clone()).await?;
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                let n = std::io::Read::read(&mut src, &mut buf)?;
                if n == 0 {
                    break;
                }
                f.write_all(&buf[..n]).await?;
            }
            f.shutdown().await?;
            // SFTP v3 rename won't replace a file.
            let _ = self.sftp.remove_file(path.to_string()).await;
            self.sftp.rename(part.clone(), path.to_string()).await?;
            anyhow::Ok(())
        })
    }

    fn rename(&mut self, from: &str, to: &str) -> anyhow::Result<()> {
        self.rt.block_on(self.sftp.rename(from.to_string(), to.to_string()))?;
        Ok(())
    }

    fn mkdirs(&mut self, dir: &str) -> anyhow::Result<()> {
        let mut cur = String::new();
        for part in dir.split('/') {
            if part.is_empty() {
                if cur.is_empty() {
                    cur.push('/');
                }
                continue;
            }
            if !cur.is_empty() && !cur.ends_with('/') {
                cur.push('/');
            }
            cur.push_str(part);
            if !self.rt.block_on(self.sftp.try_exists(cur.clone())).unwrap_or(false) {
                self.rt.block_on(self.sftp.create_dir(cur.clone())).with_context(|| format!("making {cur}"))?;
            }
        }
        Ok(())
    }

    fn host_key(&self) -> Option<String> {
        (!self.key.is_empty()).then(|| self.key.clone())
    }
}
