//! The ssh connection the relay setup runs its commands over (russh, as
//! the SFTP code uses it). Nothing is sent to a VPS before its host key is
//! known to be the one the owner confirmed: [`Ssh::host_key_of`] only reads
//! the key (it ends the connection at the key check, before any sign-in),
//! and [`Ssh::connect`] refuses to sign in without a key to match.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::ChannelMsg;
use russh::keys::{HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate};

struct KeyCheck {
    /// The host key the sign-in must see. `None`: only read it, then end the
    /// connection (nothing is sent to a host nobody has checked).
    want: Option<String>,
    seen: Arc<Mutex<String>>,
}

impl russh::client::Handler for KeyCheck {
    type Error = russh::Error;
    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let fp = match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.fingerprint(HashAlg::Sha256).to_string(),
            PublicKeyOrCertificate::Certificate(c) => c.public_key().fingerprint(HashAlg::Sha256).to_string(),
        };
        *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = fp.clone();
        Ok(self.want.as_deref() == Some(fp.as_str()))
    }
}

/// A host key as `ssh-keygen -l` prints it ("SHA256:" and 43 base64 digits),
/// or `None` if `s` isn't one. Trims spaces.
pub fn clean_host_key(s: &str) -> Option<String> {
    let s = s.trim();
    let rest = s.strip_prefix("SHA256:")?;
    (rest.len() == 43 && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')).then(|| s.to_string())
}

pub enum Auth {
    Key(PrivateKey),
    /// Used once, to put a key on the VPS; never kept.
    Password(String),
}

pub struct Ssh {
    h: russh::client::Handle<KeyCheck>,
    /// "SHA256:…" of the VPS's host key.
    pub host_key: String,
}

/// What a command printed and how it ended.
#[derive(Debug, Default)]
pub struct Out {
    pub code: u32,
    pub out: String,
    pub err: String,
}

impl Out {
    pub fn ok(&self) -> bool {
        self.code == 0
    }
    /// The first line of what it said went wrong, for a step's detail.
    pub fn why(&self) -> String {
        let e = self.err.lines().map(str::trim).find(|l| !l.is_empty()).or_else(|| self.out.lines().map(str::trim).find(|l| !l.is_empty())).unwrap_or("");
        let e: String = e.chars().take(200).collect();
        if e.is_empty() { format!("it ended with code {}", self.code) } else { e }
    }
}

impl Ssh {
    /// Reads the VPS's host key and ends the connection at once: no user
    /// name, key or password is offered. The owner checks what comes back
    /// before anything secret is sent anywhere.
    pub async fn host_key_of(host: &str, port: u16) -> Result<String, String> {
        match open(host, port, None).await {
            // The key check said no on purpose; what it saw is the answer.
            Err((key, _)) if !key.is_empty() => Ok(key),
            Err((_, e)) => Err(e),
            Ok(_) => Err("The VPS's key couldn't be read.".into()),
        }
    }

    /// Signs in. `pinned`: the host key it must have; it can't be empty, so
    /// the password or key is only ever offered to a host whose key was
    /// confirmed.
    pub async fn connect(host: &str, port: u16, user: &str, auth: Auth, pinned: &str) -> Result<Ssh, String> {
        let pinned = pinned.trim();
        if pinned.is_empty() {
            return Err("Check the VPS's host key first: nothing is sent to it until you've confirmed it.".into());
        }
        let (mut h, host_key) = match open(host, port, Some(pinned.to_string())).await {
            Ok(x) => x,
            Err((key, _)) if !key.is_empty() && key != pinned => {
                return Err(format!(
                    "The VPS's host key changed (now {key}, saved {pinned}). If you reinstalled it, confirm the new key; otherwise something else answers at that address."
                ));
            }
            Err((_, e)) => return Err(e),
        };
        let user = user.to_string();
        let auth = match auth {
            Auth::Key(k) => {
                let hash = h.best_supported_rsa_hash().await.ok().flatten().flatten();
                h.authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(k), hash)).await
            }
            Auth::Password(p) => h.authenticate_password(user, p).await,
        }
        .map_err(|e| format!("Signing in failed: {e}"))?;
        if !matches!(auth, russh::client::AuthResult::Success) {
            return Err("The VPS refused the user name, key or password.".into());
        }
        Ok(Ssh { h, host_key })
    }

    /// Runs one command, `stdin` sent to it (secrets go there, never on the
    /// command line). Gives up after `limit`.
    pub async fn run(&self, cmd: &str, stdin: Option<&[u8]>, limit: Duration) -> Result<Out, String> {
        let work = async {
            let mut ch = self.h.channel_open_session().await.map_err(|e| format!("ssh: {e}"))?;
            ch.exec(true, cmd).await.map_err(|e| format!("ssh: {e}"))?;
            if let Some(b) = stdin {
                ch.data(b).await.map_err(|e| format!("ssh: {e}"))?;
            }
            ch.eof().await.map_err(|e| format!("ssh: {e}"))?;
            let (mut out, mut err, mut code) = (Vec::new(), Vec::new(), None);
            while let Some(m) = ch.wait().await {
                match m {
                    ChannelMsg::Data { data } => out.extend_from_slice(&data),
                    ChannelMsg::ExtendedData { data, .. } => err.extend_from_slice(&data),
                    ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status),
                    ChannelMsg::Close => break,
                    _ => {}
                }
                // Bounded: nothing the setup runs prints much.
                if out.len() > 4 << 20 || err.len() > 4 << 20 {
                    return Err("The command printed too much.".to_string());
                }
            }
            Ok(Out { code: code.unwrap_or(255), out: String::from_utf8_lossy(&out).into_owned(), err: String::from_utf8_lossy(&err).into_owned() })
        };
        tokio::time::timeout(limit, work).await.map_err(|_| format!("The VPS didn't finish in {} s.", limit.as_secs()))?
    }

    pub async fn close(&self) {
        let _ = self.h.disconnect(russh::Disconnect::ByApplication, "", "en").await;
    }
}

/// Opens the connection and runs the key exchange. `want`: the host key to
/// accept (`None`: accept none, to read it). On failure: the host key seen
/// (empty if none was) and what to tell the player.
async fn open(host: &str, port: u16, want: Option<String>) -> Result<(russh::client::Handle<KeyCheck>, String), (String, String)> {
    let seen = Arc::new(Mutex::new(String::new()));
    let check = KeyCheck { want, seen: seen.clone() };
    let cfg = Arc::new(russh::client::Config { inactivity_timeout: Some(Duration::from_secs(300)), keepalive_interval: Some(Duration::from_secs(20)), ..Default::default() });
    let target = format!("{}:{port}", if host.contains(':') { format!("[{host}]") } else { host.to_string() });
    let key = || seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
    match tokio::time::timeout(Duration::from_secs(20), russh::client::connect(cfg, (host, port), check)).await {
        Err(_) => Err((key(), format!("{target} didn't answer. Check the address, and that the cloud's firewall lets ssh in."))),
        Ok(Err(e)) => Err((
            key(),
            match e {
                russh::Error::IO(io) if io.kind() == std::io::ErrorKind::ConnectionRefused => format!("{target} refused the connection: is ssh on port {port}?"),
                russh::Error::IO(io) => format!("Can't reach {target}: {io}"),
                e => format!("ssh to {target} failed: {e}"),
            },
        )),
        Ok(Ok(h)) => Ok((h, key())),
    }
}

/// A new ed25519 key for the setup's login: the private half in OpenSSH's
/// format and the public line to put in `authorized_keys`.
pub fn new_key() -> Result<(String, String), String> {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| e.to_string())?;
    let pair = russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&seed);
    let key = PrivateKey::new(russh::keys::ssh_key::private::KeypairData::Ed25519(pair), "hopper-relay-setup").map_err(|e| e.to_string())?;
    encode(&key)
}

/// A pasted or uploaded private key (OpenSSH or PuTTY), its passphrase
/// taken off so it can be used again for repair and removal.
pub fn read_key(text: &str, passphrase: &str) -> Result<(String, String), String> {
    let phrase = (!passphrase.is_empty()).then_some(passphrase);
    let key = russh::keys::decode_secret_key(text.trim(), phrase).map_err(|e| {
        let s = e.to_string();
        if s.to_lowercase().contains("encrypt") || s.to_lowercase().contains("passphrase") || s.to_lowercase().contains("password") {
            "This key has a passphrase. Type it under the key.".to_string()
        } else {
            "That isn't a private key Hopper can read. Use the file ssh-keygen made (id_ed25519, not the .pub one), or let Hopper make a new key.".to_string()
        }
    })?;
    encode(&key)
}

fn encode(key: &PrivateKey) -> Result<(String, String), String> {
    let private = key.to_openssh(russh::keys::ssh_key::LineEnding::LF).map_err(|e| e.to_string())?.to_string();
    let public = key.public_key().to_openssh().map_err(|e| e.to_string())?;
    Ok((private, public))
}

pub fn decode(private: &str) -> Result<PrivateKey, String> {
    russh::keys::decode_secret_key(private, None).map_err(|_| "The saved ssh key can't be read. Connect again.".to_string())
}
