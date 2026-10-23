//! Who may use the panel: the owner's password (browser sessions), paired
//! devices such as Hopper desktop (bearer tokens), and the pairing requests
//! waiting for the owner to allow them.
//!
//! Secrets are only stored hashed: the password with Argon2, tokens with
//! SHA-256 (they are long random values, so a fast hash is enough).

use anyhow::{Context as _, bail};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const FILE: &str = "auth.json";
/// Browser sessions end after this long unused.
const SESSION_IDLE: u64 = 30 * 24 * 3600;
/// Failed logins from one address before it has to wait.
const MAX_FAILS: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(10 * 60);
/// A pairing request the owner hasn't answered expires.
const PAIR_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_PENDING: usize = 8;
pub const MIN_PASSWORD: usize = 8;

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// `n` random bytes as lowercase hex.
pub fn random_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b).expect("system random generator");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn hash_token(token: &str) -> String {
    Sha256::digest(token.as_bytes()).iter().map(|x| format!("{x:02x}")).collect()
}

/// A code people read and type: no 0/O, 1/I.
fn readable_code(len: usize) -> String {
    const ABC: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut b = vec![0u8; len];
    getrandom::fill(&mut b).expect("system random generator");
    b.iter().map(|x| ABC[*x as usize % ABC.len()] as char).collect()
}

fn six_digits() -> String {
    let mut b = [0u8; 4];
    getrandom::fill(&mut b).expect("system random generator");
    format!("{:06}", u32::from_le_bytes(b) % 1_000_000)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    hash: String,
    pub created: u64,
    pub last_seen: u64,
    pub ip: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    hash: String,
    pub created: u64,
    pub last_seen: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct Stored {
    owner: Option<String>,
    #[serde(default)]
    sessions: Vec<Session>,
    #[serde(default)]
    devices: Vec<Device>,
}

/// A device asking to pair, shown to the owner with its code.
#[derive(Clone, Debug, Serialize)]
pub struct PairRequest {
    pub id: String,
    pub device: String,
    pub code: String,
    pub ip: String,
    #[serde(skip)]
    secret: String,
    #[serde(skip)]
    at: Instant,
    #[serde(skip)]
    answer: Option<Result<String, ()>>,
}

/// What a pairing device learns when it asks again.
#[derive(Debug, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PairStatus {
    Waiting,
    Approved { token: String, device_id: String },
    Denied,
    Unknown,
}

/// Who made a request.
#[derive(Clone, Debug, PartialEq)]
pub enum Who {
    Owner,
    Device { id: String, name: String },
}

pub struct Auth {
    path: PathBuf,
    stored: Mutex<Stored>,
    setup_code: Mutex<Option<String>>,
    fails: Mutex<HashMap<IpAddr, (u32, Instant)>>,
    pairs: Mutex<Vec<PairRequest>>,
}

impl Auth {
    pub fn load(dir: &Path) -> anyhow::Result<Auth> {
        let path = dir.join(FILE);
        let stored: Stored = match std::fs::read_to_string(&path) {
            Ok(t) => serde_json::from_str(&t).with_context(|| format!("reading {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Stored::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let setup_code = stored.owner.is_none().then(|| readable_code(8));
        Ok(Auth {
            path,
            stored: Mutex::new(stored),
            setup_code: Mutex::new(setup_code),
            fails: Default::default(),
            pairs: Default::default(),
        })
    }

    fn save(&self, s: &Stored) {
        let tmp = self.path.with_extension("json.tmp");
        let res = std::fs::write(&tmp, serde_json::to_string_pretty(s).unwrap_or_default())
            .and_then(|_| std::fs::rename(&tmp, &self.path));
        if let Err(e) = res {
            log::error!("saving {}: {e}", self.path.display());
        }
    }

    /// The one-time code that claims the panel, until an owner is set.
    pub fn setup_code(&self) -> Option<String> {
        self.setup_code.lock().unwrap().clone()
    }

    pub fn needs_setup(&self) -> bool {
        self.stored.lock().unwrap().owner.is_none()
    }

    /// Sets the owner's password with the setup code; returns a session.
    pub fn setup(&self, code: &str, password: &str, ip: IpAddr) -> anyhow::Result<String> {
        self.check_lockout(ip)?;
        let mut sc = self.setup_code.lock().unwrap();
        let Some(expected) = sc.as_deref() else { bail!("This panel already has an owner.") };
        if !code.trim().eq_ignore_ascii_case(expected) {
            drop(sc);
            self.failed(ip);
            bail!("That setup code isn't right. It's in hopperd's log and in setup-code.txt.");
        }
        if password.chars().count() < MIN_PASSWORD {
            bail!("Use at least {MIN_PASSWORD} characters.");
        }
        let hash = Argon2::default()
            .hash_password(password.as_bytes(), &SaltString::encode_b64(&random_bytes16()).unwrap())
            .map_err(|e| anyhow::anyhow!("hashing the password: {e}"))?
            .to_string();
        *sc = None;
        drop(sc);
        let mut s = self.stored.lock().unwrap();
        s.owner = Some(hash);
        let token = new_session(&mut s, ip);
        self.save(&s);
        Ok(token)
    }

    /// Checks the owner's password; returns a session token.
    pub fn login(&self, password: &str, ip: IpAddr) -> anyhow::Result<String> {
        self.check_lockout(ip)?;
        let owner = self.stored.lock().unwrap().owner.clone();
        let Some(owner) = owner else { bail!("Set the panel up first.") };
        let ok = PasswordHash::new(&owner)
            .ok()
            .is_some_and(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok());
        if !ok {
            self.failed(ip);
            bail!("Wrong password.");
        }
        self.fails.lock().unwrap().remove(&ip);
        let mut s = self.stored.lock().unwrap();
        let token = new_session(&mut s, ip);
        self.save(&s);
        Ok(token)
    }

    pub fn logout(&self, token: &str) {
        let h = hash_token(token);
        let mut s = self.stored.lock().unwrap();
        s.sessions.retain(|x| x.hash != h);
        self.save(&s);
    }

    fn check_lockout(&self, ip: IpAddr) -> anyhow::Result<()> {
        let mut f = self.fails.lock().unwrap();
        if let Some((n, since)) = f.get(&ip).copied() {
            if since.elapsed() > LOCKOUT {
                f.remove(&ip);
            } else if n >= MAX_FAILS {
                let wait = LOCKOUT.saturating_sub(since.elapsed()).as_secs() / 60 + 1;
                bail!("Too many wrong tries. Wait {wait} min.");
            }
        }
        Ok(())
    }

    fn failed(&self, ip: IpAddr) {
        let mut f = self.fails.lock().unwrap();
        let e = f.entry(ip).or_insert((0, Instant::now()));
        e.0 += 1;
        e.1 = Instant::now();
    }

    /// Who a session cookie or bearer token belongs to.
    pub fn check(&self, token: &str) -> Option<Who> {
        if token.is_empty() {
            return None;
        }
        let h = hash_token(token);
        let t = now();
        let mut s = self.stored.lock().unwrap();
        s.sessions.retain(|x| t.saturating_sub(x.last_seen) < SESSION_IDLE);
        if let Some(sess) = s.sessions.iter_mut().find(|x| x.hash == h) {
            // Saved lazily: a minute's precision is plenty.
            let stale = t.saturating_sub(sess.last_seen) > 60;
            sess.last_seen = t;
            if stale {
                self.save(&s);
            }
            return Some(Who::Owner);
        }
        if let Some(d) = s.devices.iter_mut().find(|x| x.hash == h) {
            let stale = t.saturating_sub(d.last_seen) > 60;
            d.last_seen = t;
            let who = Who::Device { id: d.id.clone(), name: d.name.clone() };
            if stale {
                self.save(&s);
            }
            return Some(who);
        }
        None
    }

    pub fn devices(&self) -> Vec<Device> {
        self.stored.lock().unwrap().devices.clone()
    }

    pub fn forget_device(&self, id: &str) -> bool {
        let mut s = self.stored.lock().unwrap();
        let before = s.devices.len();
        s.devices.retain(|d| d.id != id);
        let gone = s.devices.len() != before;
        if gone {
            self.save(&s);
        }
        gone
    }

    // ---- pairing ---------------------------------------------------------

    /// A device asks to pair. Returns (request id, secret to poll with,
    /// code both sides show).
    pub fn pair_request(&self, device: &str, ip: IpAddr) -> anyhow::Result<(String, String, String)> {
        let device = device.trim();
        if device.is_empty() || device.len() > 64 {
            bail!("Give the device a name (up to 64 characters).");
        }
        let mut p = self.pairs.lock().unwrap();
        p.retain(|r| r.at.elapsed() < PAIR_TTL);
        if p.iter().filter(|r| r.answer.is_none()).count() >= MAX_PENDING {
            bail!("Too many pairing requests waiting. Answer them in the panel first.");
        }
        let req = PairRequest {
            id: random_hex(8),
            device: device.to_string(),
            code: six_digits(),
            ip: ip.to_string(),
            secret: random_hex(32),
            at: Instant::now(),
            answer: None,
        };
        let out = (req.id.clone(), req.secret.clone(), req.code.clone());
        p.push(req);
        Ok(out)
    }

    /// Requests the owner hasn't answered yet.
    pub fn pending(&self) -> Vec<PairRequest> {
        let mut p = self.pairs.lock().unwrap();
        p.retain(|r| r.at.elapsed() < PAIR_TTL);
        p.iter().filter(|r| r.answer.is_none()).cloned().collect()
    }

    /// The owner allows or refuses a request.
    pub fn answer(&self, id: &str, allow: bool) -> anyhow::Result<()> {
        let mut p = self.pairs.lock().unwrap();
        let Some(r) = p.iter_mut().find(|r| r.id == id && r.answer.is_none() && r.at.elapsed() < PAIR_TTL) else {
            bail!("That pairing request has expired.");
        };
        if !allow {
            r.answer = Some(Err(()));
            return Ok(());
        }
        let token = random_hex(32);
        let device = Device {
            id: random_hex(6),
            name: r.device.clone(),
            hash: hash_token(&token),
            created: now(),
            last_seen: now(),
        };
        r.answer = Some(Ok(format!("{}:{token}", device.id)));
        drop(p);
        let mut s = self.stored.lock().unwrap();
        s.devices.push(device);
        self.save(&s);
        Ok(())
    }

    /// What the device that made request `id` gets when it asks again. The
    /// token is handed out once.
    pub fn pair_status(&self, id: &str, secret: &str) -> PairStatus {
        let mut p = self.pairs.lock().unwrap();
        let Some(i) = p.iter().position(|r| r.id == id && r.secret == secret) else {
            return PairStatus::Unknown;
        };
        match p[i].answer.clone() {
            None if p[i].at.elapsed() >= PAIR_TTL => {
                p.remove(i);
                PairStatus::Unknown
            }
            None => PairStatus::Waiting,
            Some(Err(())) => {
                p.remove(i);
                PairStatus::Denied
            }
            Some(Ok(v)) => {
                p.remove(i);
                let (device_id, token) = v.split_once(':').unwrap_or_default();
                PairStatus::Approved { token: token.into(), device_id: device_id.into() }
            }
        }
    }
}

fn random_bytes16() -> [u8; 16] {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("system random generator");
    b
}

fn new_session(s: &mut Stored, ip: IpAddr) -> String {
    let token = random_hex(32);
    s.sessions.push(Session { hash: hash_token(&token), created: now(), last_seen: now(), ip: ip.to_string() });
    token
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopperd-auth-{}-{}", std::process::id(), random_hex(4)));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const IP: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 50, 21));

    #[test]
    fn setup_login_and_sessions() {
        let dir = temp();
        let a = Auth::load(&dir).unwrap();
        assert!(a.needs_setup());
        let code = a.setup_code().unwrap();
        assert!(a.setup("WRONG", "password1", IP).is_err());
        assert!(a.setup(&code, "short", IP).is_err());
        let s1 = a.setup(&code.to_lowercase(), "password1", IP).unwrap();
        assert_eq!(a.check(&s1), Some(Who::Owner));
        assert!(a.setup(&code, "password1", IP).is_err(), "only once");
        // Survives a restart; the password is stored hashed.
        let raw = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(!raw.contains("password1") && !raw.contains(&s1));
        let a = Auth::load(&dir).unwrap();
        assert!(!a.needs_setup() && a.setup_code().is_none());
        assert_eq!(a.check(&s1), Some(Who::Owner));
        assert!(a.login("nope", IP).is_err());
        let s2 = a.login("password1", IP).unwrap();
        a.logout(&s2);
        assert_eq!(a.check(&s2), None);
        assert_eq!(a.check(""), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lockout_after_wrong_passwords() {
        let dir = temp();
        let a = Auth::load(&dir).unwrap();
        a.setup(&a.setup_code().unwrap(), "password1", IP).unwrap();
        for _ in 0..MAX_FAILS {
            assert!(a.login("bad", IP).is_err());
        }
        let e = a.login("password1", IP).unwrap_err().to_string();
        assert!(e.contains("Too many"), "{e}");
        // Another address isn't affected.
        assert!(a.login("password1", "10.0.0.2".parse().unwrap()).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pairing_flow() {
        let dir = temp();
        let a = Auth::load(&dir).unwrap();
        let (id, secret, code) = a.pair_request("Hopper on FOX", IP).unwrap();
        assert_eq!(code.len(), 6);
        assert_eq!(a.pair_status(&id, &secret), PairStatus::Waiting);
        assert_eq!(a.pair_status(&id, "wrong"), PairStatus::Unknown);
        assert_eq!(a.pending().len(), 1);
        a.answer(&id, true).unwrap();
        let PairStatus::Approved { token, device_id } = a.pair_status(&id, &secret) else { panic!() };
        assert_eq!(a.check(&token), Some(Who::Device { id: device_id.clone(), name: "Hopper on FOX".into() }));
        // Handed out once.
        assert_eq!(a.pair_status(&id, &secret), PairStatus::Unknown);
        assert!(a.pending().is_empty());
        assert!(a.forget_device(&device_id));
        assert_eq!(a.check(&token), None);
        let (id2, s2, _) = a.pair_request("Phone", IP).unwrap();
        a.answer(&id2, false).unwrap();
        assert_eq!(a.pair_status(&id2, &s2), PairStatus::Denied);
        assert!(a.pair_request("", IP).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
