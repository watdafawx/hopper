//! Who may use the panel: the owner's password (browser sessions), other
//! people the owner adds (users with a role), paired devices such as Hopper
//! desktop (bearer tokens), and the pairing requests waiting for the owner
//! to allow them.
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

/// Whether a request comes from a browser on this PC itself, for setting
/// the owner password without the setup code. Only a direct connection
/// from 127.0.0.1/::1 counts: one a proxy or tunnel on this PC forwards
/// (it adds Forwarded/X-Forwarded-For or sends the public name as Host),
/// a page on another site (Origin), or anything in Docker (where other
/// containers can look local) needs the code.
pub fn trusted_local(enabled: bool, peer: IpAddr, headers: &axum::http::HeaderMap) -> bool {
    const PROXY: &[&str] = &["forwarded", "x-forwarded-for", "x-forwarded-host", "x-real-ip", "cf-connecting-ip", "true-client-ip", "via"];
    let loopback = match peer {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(v6.is_loopback(), |v4| v4.is_loopback()),
        IpAddr::V4(v4) => v4.is_loopback(),
    };
    let local_name = |h: &str| {
        let h = h.trim().to_ascii_lowercase();
        let host = if h.starts_with('[') { h.split(']').next().map(|x| format!("{x}]")).unwrap_or_default() } else { h.split(':').next().unwrap_or("").to_string() };
        matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]")
    };
    let header = |n: &str| headers.get(n).and_then(|v| v.to_str().ok());
    let origin_ok = header("origin").is_none_or(|o| local_name(o.split("://").nth(1).unwrap_or("")));
    enabled
        && loopback
        && !in_docker()
        && PROXY.iter().all(|h| !headers.contains_key(*h))
        && header("host").is_some_and(local_name)
        && origin_ok
}

/// Whether setting up from this PC without the code can happen at all.
pub fn local_setup_possible(enabled: bool) -> bool {
    enabled && !in_docker()
}

fn in_docker() -> bool {
    Path::new("/.dockerenv").exists() || std::env::var_os("HOPPERD_DOCKER").is_some()
}

/// A code people read and type: no 0/O, 1/I.
fn readable_code(len: usize) -> String {
    const ABC: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut b = vec![0u8; len];
    getrandom::fill(&mut b).expect("system random generator");
    b.iter().map(|x| ABC[*x as usize % ABC.len()] as char).collect()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    hash: String,
    /// The user it belongs to; the owner's when `None`.
    #[serde(default)]
    user: Option<String>,
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
    /// The owner's two-factor secret (base32), once turned on.
    #[serde(default)]
    totp: Option<String>,
    /// The last time step a code was used for, so a code works once.
    #[serde(default)]
    totp_step: u64,
    #[serde(default)]
    tokens: Vec<ApiToken>,
    #[serde(default)]
    users: Vec<User>,
}

/// What a user may do. The owner can do everything; only the owner manages
/// users, devices, tokens, webhooks, two-factor and panel settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Everything on their servers: files, mods, settings, backups.
    Admin,
    /// Start, stop, console, players and making backups.
    Operator,
    /// Looking only.
    Viewer,
}

impl Role {
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "admin" => Some(Role::Admin),
            "operator" => Some(Role::Operator),
            "viewer" => Some(Role::Viewer),
            _ => None,
        }
    }
}

/// Someone the owner added, who signs in with a name and password.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub name: String,
    hash: String,
    pub role: Role,
    /// Only these servers; all when empty.
    #[serde(default)]
    pub servers: Vec<String>,
    pub created: u64,
    #[serde(default)]
    pub last_seen: u64,
    #[serde(default)]
    totp: Option<String>,
    #[serde(default)]
    totp_step: u64,
}

impl User {
    pub fn two_factor(&self) -> bool {
        self.totp.is_some()
    }
}

/// The owner (`None`) or a user's id.
pub type Account<'a> = Option<&'a str>;

impl Stored {
    fn secrets(&self, acct: Account) -> Option<(String, Option<String>, u64)> {
        match acct {
            None => self.owner.clone().map(|h| (h, self.totp.clone(), self.totp_step)),
            Some(id) => self.users.iter().find(|u| u.id == id).map(|u| (u.hash.clone(), u.totp.clone(), u.totp_step)),
        }
    }

    fn set_totp(&mut self, acct: Account, secret: Option<String>) {
        match acct {
            None => self.totp = secret,
            Some(id) => {
                if let Some(u) = self.users.iter_mut().find(|u| u.id == id) {
                    u.totp = secret;
                }
            }
        }
    }

    fn set_step(&mut self, acct: Account, step: u64) {
        match acct {
            None => self.totp_step = step,
            Some(id) => {
                if let Some(u) = self.users.iter_mut().find(|u| u.id == id) {
                    u.totp_step = step;
                }
            }
        }
    }
}

fn hash_password(password: &str) -> anyhow::Result<String> {
    if password.chars().count() < MIN_PASSWORD {
        bail!("Use at least {MIN_PASSWORD} characters.");
    }
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &SaltString::encode_b64(&random_bytes16()).unwrap())
        .map_err(|e| anyhow::anyhow!("hashing the password: {e}"))?
        .to_string())
}

fn verify_password(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash).ok().is_some_and(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
}

/// An API token the owner made for a script or another app.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiToken {
    pub id: String,
    pub name: String,
    hash: String,
    pub created: u64,
    pub last_used: u64,
    /// Unix seconds; never when `None`.
    pub expires: Option<u64>,
    /// Only GET requests (looking, not changing).
    pub read_only: bool,
    /// Only these servers' routes; any when empty.
    pub servers: Vec<String>,
}

/// Why a sign-in didn't work.
#[derive(Debug, PartialEq)]
pub enum LoginError {
    /// Wrong password or code, a lockout, no owner yet.
    Refused(String),
    /// The password was right; a two-factor code is needed too.
    NeedCode,
    /// From the internet without two-factor sign-in set up.
    NeedTwoFactor,
}

impl std::fmt::Display for LoginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoginError::Refused(m) => f.write_str(m),
            LoginError::NeedCode => f.write_str("Enter the 6-digit code from your authenticator app."),
            LoginError::NeedTwoFactor => f.write_str(
                "Signing in from the internet needs two-factor sign-in. Turn it on from the LAN first (Access & devices).",
            ),
        }
    }
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
    Token { id: String, name: String, read_only: bool, servers: Vec<String> },
    User { id: String, name: String, role: Role, servers: Vec<String> },
}

impl Who {
    pub fn name(&self) -> String {
        match self {
            Who::Owner => "owner".into(),
            Who::Device { name, .. } => name.clone(),
            Who::Token { name, .. } => format!("token \"{name}\""),
            Who::User { name, .. } => format!("user \"{name}\""),
        }
    }

    /// Whether this one may see the server with `id` at all.
    pub fn sees(&self, id: &str) -> bool {
        match self {
            Who::Token { servers, .. } | Who::User { servers, .. } => servers.is_empty() || servers.iter().any(|s| s == id),
            _ => true,
        }
    }

    /// The account two-factor and passwords are about.
    pub fn account(&self) -> Option<Account<'_>> {
        match self {
            Who::Owner => Some(None),
            Who::User { id, .. } => Some(Some(id.as_str())),
            _ => None,
        }
    }
}

pub struct Auth {
    path: PathBuf,
    stored: Mutex<Stored>,
    setup_code: Mutex<Option<String>>,
    fails: Mutex<HashMap<IpAddr, (u32, Instant)>>,
    /// A two-factor secret being set up, until a code confirms it.
    pending_totp: Mutex<HashMap<String, String>>,
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
            pending_totp: Default::default(),
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
    /// `local` (see [`trusted_local`]) skips the code.
    pub fn setup(&self, code: &str, password: &str, ip: IpAddr) -> anyhow::Result<String> {
        self.setup_as(code, password, ip, false)
    }

    pub fn setup_as(&self, code: &str, password: &str, ip: IpAddr, local: bool) -> anyhow::Result<String> {
        self.check_lockout(ip)?;
        let mut sc = self.setup_code.lock().unwrap();
        let Some(expected) = sc.as_deref() else { bail!("This panel already has an owner.") };
        if !(local && code.trim().is_empty()) && !code.trim().eq_ignore_ascii_case(expected) {
            drop(sc);
            self.failed(ip);
            bail!("That setup code isn't right. It's in hopperd's log and in setup-code.txt.");
        }
        let hash = hash_password(password)?;
        *sc = None;
        drop(sc);
        let mut s = self.stored.lock().unwrap();
        s.owner = Some(hash);
        let token = new_session(&mut s, None, ip);
        self.save(&s);
        Ok(token)
    }

    /// Checks the owner's password; returns a session token.
    pub fn login(&self, password: &str, ip: IpAddr) -> anyhow::Result<String> {
        self.login_2fa(password, None, ip, true).map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Checks the password, and the two-factor code when it's on.
    /// `private`: the client is on this PC, the LAN or Tailscale; from the
    /// internet, two-factor sign-in must be on.
    pub fn login_2fa(&self, password: &str, code: Option<&str>, ip: IpAddr, private: bool) -> Result<String, LoginError> {
        self.login_as("", password, code, ip, private)
    }

    /// Signs in the owner (empty `name`, or "owner") or the user called `name`.
    pub fn login_as(&self, name: &str, password: &str, code: Option<&str>, ip: IpAddr, private: bool) -> Result<String, LoginError> {
        self.check_lockout(ip).map_err(|e| LoginError::Refused(e.to_string()))?;
        let name = name.trim();
        let (acct, secrets) = {
            let s = self.stored.lock().unwrap();
            if s.owner.is_none() {
                return Err(LoginError::Refused("Set the panel up first.".into()));
            }
            let acct = if name.is_empty() || name.eq_ignore_ascii_case("owner") {
                None
            } else {
                s.users.iter().find(|u| u.name.eq_ignore_ascii_case(name)).map(|u| u.id.clone())
            };
            let secrets = if name.is_empty() || name.eq_ignore_ascii_case("owner") || acct.is_some() { s.secrets(acct.as_deref()) } else { None };
            (acct, secrets)
        };
        let Some((hash, totp, _)) = secrets else {
            // Same work and answer as a wrong password: names aren't revealed.
            static DUMMY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
            let _ = verify_password(DUMMY.get_or_init(|| hash_password("not-a-real-password").unwrap_or_default()), password);
            self.failed(ip);
            return Err(LoginError::Refused("Wrong name or password.".into()));
        };
        if !verify_password(&hash, password) {
            self.failed(ip);
            return Err(LoginError::Refused(if name.is_empty() { "Wrong password.".into() } else { "Wrong name or password.".into() }));
        }
        let acct = acct.as_deref();
        match (&totp, code.map(str::trim).filter(|c| !c.is_empty())) {
            (None, _) if !private => return Err(LoginError::NeedTwoFactor),
            (None, _) => {}
            (Some(_), None) => return Err(LoginError::NeedCode),
            (Some(secret), Some(code)) => {
                if !self.use_code(acct, secret, code) {
                    self.failed(ip);
                    return Err(LoginError::Refused("That code isn't right (or was already used).".into()));
                }
            }
        }
        self.fails.lock().unwrap().remove(&ip);
        let mut s = self.stored.lock().unwrap();
        let token = new_session(&mut s, acct.map(str::to_string), ip);
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
            let user = sess.user.clone();
            let who = match user {
                None => Who::Owner,
                Some(uid) => {
                    // A removed user's sessions end with them.
                    let u = s.users.iter_mut().find(|u| u.id == uid)?;
                    u.last_seen = t;
                    Who::User { id: u.id.clone(), name: u.name.clone(), role: u.role, servers: u.servers.clone() }
                }
            };
            if stale {
                self.save(&s);
            }
            return Some(who);
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
        if let Some(tk) = s.tokens.iter_mut().find(|x| x.hash == h) {
            if tk.expires.is_some_and(|e| e <= t) {
                return None;
            }
            let stale = t.saturating_sub(tk.last_used) > 60;
            tk.last_used = t;
            let who = Who::Token { id: tk.id.clone(), name: tk.name.clone(), read_only: tk.read_only, servers: tk.servers.clone() };
            if stale {
                self.save(&s);
            }
            return Some(who);
        }
        None
    }

    // ---- two-factor sign-in --------------------------------------------------

    /// Whether a code matches `secret` now (one step either side), and it
    /// wasn't used before. Uses it up.
    fn use_code(&self, acct: Account, secret: &str, code: &str) -> bool {
        let Some(key) = base32_decode(secret) else { return false };
        let step = now() / 30;
        let mut s = self.stored.lock().unwrap();
        let last = s.secrets(acct).map_or(0, |x| x.2);
        for st in [step.saturating_sub(1), step, step + 1] {
            if st > last && format!("{:06}", totp(&key, st)) == code {
                s.set_step(acct, st);
                self.save(&s);
                return true;
            }
        }
        false
    }

    pub fn two_factor_on(&self, acct: Account) -> bool {
        self.stored.lock().unwrap().secrets(acct).is_some_and(|x| x.1.is_some())
    }

    /// A new secret to scan; it takes effect once a code from it is
    /// confirmed with [`Self::two_factor_enable`].
    pub fn two_factor_begin(&self, acct: Account, account: &str) -> (String, String) {
        let mut key = [0u8; 20];
        getrandom::fill(&mut key).expect("system random generator");
        let secret = base32_encode(&key);
        self.pending_totp.lock().unwrap().insert(acct.unwrap_or("").to_string(), secret.clone());
        let label: String = account.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
        let uri = format!("otpauth://totp/Hopper%20Server:{label}?secret={secret}&issuer=Hopper%20Server&digits=6&period=30");
        (secret, uri)
    }

    pub fn two_factor_enable(&self, acct: Account, code: &str) -> anyhow::Result<()> {
        let key = acct.unwrap_or("").to_string();
        let Some(secret) = self.pending_totp.lock().unwrap().get(&key).cloned() else {
            bail!("Start setting up two-factor sign-in again.");
        };
        if !self.use_code(acct, &secret, code.trim()) {
            bail!("That code isn't right. Check the time on your phone and try the next one.");
        }
        let mut s = self.stored.lock().unwrap();
        s.set_totp(acct, Some(secret));
        self.save(&s);
        self.pending_totp.lock().unwrap().remove(&key);
        Ok(())
    }

    /// Turns it off; needs the password and a current code.
    pub fn two_factor_disable(&self, acct: Account, password: &str, code: &str, ip: IpAddr) -> anyhow::Result<()> {
        let Some((hash, Some(secret), _)) = self.stored.lock().unwrap().secrets(acct) else { return Ok(()) };
        if !verify_password(&hash, password) || !self.use_code(acct, &secret, code.trim()) {
            self.failed(ip);
            bail!("The password or code isn't right.");
        }
        let mut s = self.stored.lock().unwrap();
        s.set_totp(acct, None);
        self.save(&s);
        Ok(())
    }

    /// SFTP sign-in: a person's name and password (not from the internet,
    /// and not when they use two-factor sign-in, which SFTP can't ask for),
    /// or an API or device token as the password, from anywhere.
    pub fn sftp_login(&self, name: &str, password: &str, ip: IpAddr, private: bool) -> anyhow::Result<Who> {
        self.check_lockout(ip)?;
        if let Some(who @ (Who::Token { .. } | Who::Device { .. })) = self.check(password.trim()) {
            return Ok(who);
        }
        if !private {
            self.failed(ip);
            bail!("from the internet, sign in with an API token as the password");
        }
        let name = name.trim();
        let (acct, secrets) = {
            let s = self.stored.lock().unwrap();
            let acct = if name.eq_ignore_ascii_case("owner") { None } else { s.users.iter().find(|u| u.name.eq_ignore_ascii_case(name)).map(|u| u.id.clone()) };
            let found = name.eq_ignore_ascii_case("owner") || acct.is_some();
            let secrets = if found { s.secrets(acct.as_deref()) } else { None };
            (acct, secrets)
        };
        let Some((hash, totp, _)) = secrets.filter(|(h, _, _)| verify_password(h, password)) else {
            self.failed(ip);
            bail!("wrong name or password");
        };
        let _ = hash;
        if totp.is_some() {
            bail!("this account uses two-factor sign-in; use an API token as the password");
        }
        self.fails.lock().unwrap().remove(&ip);
        let s = self.stored.lock().unwrap();
        Ok(match acct {
            None => Who::Owner,
            Some(id) => {
                let u = s.users.iter().find(|u| u.id == id).context("user gone")?;
                Who::User { id: u.id.clone(), name: u.name.clone(), role: u.role, servers: u.servers.clone() }
            }
        })
    }

    // ---- users ----------------------------------------------------------------------

    pub fn users(&self) -> Vec<User> {
        self.stored.lock().unwrap().users.clone()
    }

    fn check_user_name(s: &Stored, name: &str, except: Option<&str>) -> anyhow::Result<()> {
        if name.is_empty() || name.len() > 32 || !name.chars().all(|c| c.is_alphanumeric() || "-_.".contains(c)) {
            bail!("Names are up to 32 letters, digits, - _ or .");
        }
        if name.eq_ignore_ascii_case("owner") || s.users.iter().any(|u| u.name.eq_ignore_ascii_case(name) && Some(u.id.as_str()) != except) {
            bail!("That name is taken.");
        }
        Ok(())
    }

    pub fn user_create(&self, name: &str, password: &str, role: Role, servers: Vec<String>) -> anyhow::Result<User> {
        let name = name.trim();
        let hash = hash_password(password)?;
        let mut s = self.stored.lock().unwrap();
        Self::check_user_name(&s, name, None)?;
        let u = User {
            id: random_hex(6),
            name: name.to_string(),
            hash,
            role,
            servers,
            created: now(),
            last_seen: 0,
            totp: None,
            totp_step: 0,
        };
        s.users.push(u.clone());
        self.save(&s);
        Ok(u)
    }

    /// Changes a user's role, servers, and (when given) password. A new
    /// password signs them out everywhere.
    pub fn user_update(&self, id: &str, name: &str, role: Role, servers: Vec<String>, password: Option<&str>) -> anyhow::Result<()> {
        let hash = password.filter(|p| !p.is_empty()).map(hash_password).transpose()?;
        let mut s = self.stored.lock().unwrap();
        Self::check_user_name(&s, name.trim(), Some(id))?;
        let Some(u) = s.users.iter_mut().find(|u| u.id == id) else { bail!("No user with that id.") };
        u.name = name.trim().to_string();
        u.role = role;
        u.servers = servers;
        if let Some(h) = hash {
            u.hash = h;
            s.sessions.retain(|x| x.user.as_deref() != Some(id));
        }
        self.save(&s);
        Ok(())
    }

    /// Turns a user's two-factor sign-in off (they lost their phone).
    pub fn user_reset_2fa(&self, id: &str) -> bool {
        let mut s = self.stored.lock().unwrap();
        let Some(u) = s.users.iter_mut().find(|u| u.id == id) else { return false };
        u.totp = None;
        self.save(&s);
        true
    }

    pub fn user_delete(&self, id: &str) -> bool {
        let mut s = self.stored.lock().unwrap();
        let before = s.users.len();
        s.users.retain(|u| u.id != id);
        let gone = s.users.len() != before;
        if gone {
            s.sessions.retain(|x| x.user.as_deref() != Some(id));
            self.save(&s);
        }
        gone
    }

    /// Changes your own password; needs the current one.
    pub fn change_password(&self, acct: Account, current: &str, new: &str, ip: IpAddr) -> anyhow::Result<()> {
        let Some((hash, _, _)) = self.stored.lock().unwrap().secrets(acct) else { bail!("No such account.") };
        if !verify_password(&hash, current) {
            self.failed(ip);
            bail!("The current password isn't right.");
        }
        let h = hash_password(new)?;
        let mut s = self.stored.lock().unwrap();
        match acct {
            None => s.owner = Some(h),
            Some(id) => {
                if let Some(u) = s.users.iter_mut().find(|u| u.id == id) {
                    u.hash = h;
                }
            }
        }
        self.save(&s);
        Ok(())
    }

    // ---- API tokens ---------------------------------------------------------------

    /// Makes a token; returns it (shown once) and its record.
    pub fn token_create(&self, name: &str, read_only: bool, servers: Vec<String>, days: Option<u64>) -> anyhow::Result<(String, ApiToken)> {
        let name = name.trim();
        if name.is_empty() || name.len() > 64 {
            bail!("Give the token a name (up to 64 characters).");
        }
        let token = format!("hst_{}", random_hex(32));
        let t = ApiToken {
            id: random_hex(6),
            name: name.to_string(),
            hash: hash_token(&token),
            created: now(),
            last_used: 0,
            expires: days.filter(|d| *d > 0).map(|d| now() + d * 86_400),
            read_only,
            servers,
        };
        let mut s = self.stored.lock().unwrap();
        s.tokens.push(t.clone());
        self.save(&s);
        Ok((token, t))
    }

    pub fn tokens(&self) -> Vec<ApiToken> {
        self.stored.lock().unwrap().tokens.clone()
    }

    pub fn token_revoke(&self, id: &str) -> bool {
        let mut s = self.stored.lock().unwrap();
        let before = s.tokens.len();
        s.tokens.retain(|t| t.id != id);
        let gone = s.tokens.len() != before;
        if gone {
            self.save(&s);
        }
        gone
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
    pub fn pair_request(&self, device: &str, ip: IpAddr, fingerprint: &str) -> anyhow::Result<(String, String, String)> {
        let device = device.trim();
        if device.is_empty() || device.len() > 64 {
            bail!("Give the device a name (up to 64 characters).");
        }
        let mut p = self.pairs.lock().unwrap();
        p.retain(|r| r.at.elapsed() < PAIR_TTL);
        if p.iter().filter(|r| r.answer.is_none()).count() >= MAX_PENDING {
            bail!("Too many pairing requests waiting. Answer them in the panel first.");
        }
        let (id, secret) = (random_hex(8), random_hex(32));
        let req = PairRequest {
            code: hopper_core::hopperd::pair_code(fingerprint, &id, &secret),
            id,
            device: device.to_string(),
            ip: ip.to_string(),
            secret,
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

/// RFC 6238 TOTP (HMAC-SHA1, 6 digits) for time step `step`.
pub fn totp(key: &[u8], step: u64) -> u32 {
    let mac = hmac_sha1(key, &step.to_be_bytes());
    let off = (mac[19] & 0x0f) as usize;
    let bin = u32::from_be_bytes([mac[off] & 0x7f, mac[off + 1], mac[off + 2], mac[off + 3]]);
    bin % 1_000_000
}

fn hmac_sha1(key: &[u8], msg: &[u8]) -> [u8; 20] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..20].copy_from_slice(&sha1_smol::Sha1::from(key).digest().bytes());
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = sha1_smol::Sha1::new();
    inner.update(&k.map(|b| b ^ 0x36));
    inner.update(msg);
    let mut outer = sha1_smol::Sha1::new();
    outer.update(&k.map(|b| b ^ 0x5c));
    outer.update(&inner.digest().bytes());
    outer.digest().bytes()
}

const B32: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

fn base32_encode(data: &[u8]) -> String {
    let mut out = String::new();
    let (mut buf, mut bits) = (0u32, 0);
    for &b in data {
        buf = (buf << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            out.push(B32[((buf >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        out.push(B32[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut buf, mut bits) = (0u32, 0);
    for c in s.chars().filter(|c| *c != '=' && !c.is_whitespace()) {
        let v = B32.iter().position(|&x| x as char == c.to_ascii_uppercase())? as u32;
        buf = (buf << 5) | v;
        bits += 5;
        if bits >= 8 {
            out.push((buf >> (bits - 8)) as u8);
            bits -= 8;
        }
    }
    Some(out)
}

fn random_bytes16() -> [u8; 16] {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("system random generator");
    b
}

fn new_session(s: &mut Stored, user: Option<String>, ip: IpAddr) -> String {
    let token = random_hex(32);
    s.sessions.push(Session { hash: hash_token(&token), user, created: now(), last_seen: now(), ip: ip.to_string() });
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
    fn same_pc_setup() {
        use axum::http::{HeaderMap, HeaderValue};
        let lo: IpAddr = [127, 0, 0, 1].into();
        let h = |pairs: &[(&'static str, &str)]| {
            let mut m = HeaderMap::new();
            for (k, v) in pairs {
                m.insert(*k, HeaderValue::from_str(v).unwrap());
            }
            m
        };
        assert!(trusted_local(true, lo, &h(&[("host", "localhost:8790")])));
        assert!(trusted_local(true, "::1".parse().unwrap(), &h(&[("host", "[::1]:8790"), ("origin", "http://[::1]:8790")])));
        assert!(!trusted_local(false, lo, &h(&[("host", "localhost:8790")])), "turned off");
        assert!(!trusted_local(true, IP, &h(&[("host", "localhost:8790")])), "another device");
        assert!(!trusted_local(true, lo, &h(&[("host", "mc.example.com")])), "a tunnel sends the public name");
        assert!(!trusted_local(true, lo, &h(&[("host", "localhost"), ("x-forwarded-for", "8.8.8.8")])), "a proxy");
        assert!(!trusted_local(true, lo, &h(&[("host", "localhost"), ("origin", "https://evil.example")])), "another site's page");
        assert!(!trusted_local(true, lo, &h(&[])), "no Host");

        let dir = std::env::temp_dir().join(format!("hopperd-local-setup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = Auth::load(&dir).unwrap();
        assert!(a.setup_as("", "password1", lo, false).is_err(), "the code is needed unless local");
        assert!(a.setup_as("", "password1", lo, true).is_ok());
        assert!(a.setup_as("", "password2", lo, true).is_err(), "only once");
        let _ = std::fs::remove_dir_all(&dir);
    }

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
    fn totp_matches_rfc6238() {
        // RFC 6238 test vector (SHA-1): time 59 → 94287082 (8 digits).
        let key = b"12345678901234567890";
        assert_eq!(totp(key, 59 / 30), 94287082 % 1_000_000);
        assert_eq!(totp(key, 1111111109 / 30), 7081804 % 1_000_000);
        let enc = base32_encode(key);
        assert_eq!(enc, "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
        assert_eq!(base32_decode(&enc).unwrap(), key);
    }

    #[test]
    fn two_factor_login() {
        let dir = temp();
        let a = Auth::load(&dir).unwrap();
        a.setup(&a.setup_code().unwrap(), "password1", IP).unwrap();
        let net: IpAddr = "8.8.8.8".parse().unwrap();
        // From the internet, two-factor must be on.
        assert_eq!(a.login_2fa("password1", None, net, false).unwrap_err(), LoginError::NeedTwoFactor);
        let (secret, uri) = a.two_factor_begin(None, "rig");
        assert!(uri.starts_with("otpauth://totp/") && uri.contains(&secret));
        let key = base32_decode(&secret).unwrap();
        let code = || format!("{:06}", totp(&key, now() / 30));
        assert!(a.two_factor_enable(None, "000000").is_err() || code() == "000000");
        a.two_factor_enable(None, &code()).unwrap();
        assert!(a.two_factor_on(None));
        assert_eq!(a.login_2fa("password1", None, IP, true).unwrap_err(), LoginError::NeedCode);
        // The code used to turn it on can't be reused.
        assert!(matches!(a.login_2fa("password1", Some(&code()), net, false), Err(LoginError::Refused(_))));
        let next = format!("{:06}", totp(&key, now() / 30 + 1));
        assert!(a.login_2fa("password1", Some(&next), net, false).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn users_sign_in_by_name() {
        let dir = temp();
        let a = Auth::load(&dir).unwrap();
        a.setup(&a.setup_code().unwrap(), "password1", IP).unwrap();
        let u = a.user_create("Alex", "alexpass1", Role::Operator, vec!["tnp".into()]).unwrap();
        assert!(a.user_create("alex", "whatever1", Role::Viewer, vec![]).is_err(), "names are unique");
        assert!(a.user_create("owner", "whatever1", Role::Viewer, vec![]).is_err());
        assert!(a.user_create("bo", "short", Role::Viewer, vec![]).is_err());
        let s = a.login_as("alex", "alexpass1", None, IP, true).unwrap();
        let who = a.check(&s).unwrap();
        assert_eq!(who, Who::User { id: u.id.clone(), name: "Alex".into(), role: Role::Operator, servers: vec!["tnp".into()] });
        assert!(who.sees("tnp") && !who.sees("other"));
        assert!(a.login_as("alex", "password1", None, IP, true).is_err(), "not the owner's password");
        assert!(a.login_as("nobody", "alexpass1", None, IP, true).is_err());
        assert_eq!(a.check(&a.login_as("", "password1", None, IP, true).unwrap()), Some(Who::Owner));
        // Their own two-factor, apart from the owner's.
        let (secret, _) = a.two_factor_begin(Some(&u.id), "Alex");
        let key = base32_decode(&secret).unwrap();
        a.two_factor_enable(Some(&u.id), &format!("{:06}", totp(&key, now() / 30))).unwrap();
        assert!(a.two_factor_on(Some(&u.id)) && !a.two_factor_on(None));
        assert_eq!(a.login_as("Alex", "alexpass1", None, IP, true).unwrap_err(), LoginError::NeedCode);
        assert!(a.user_reset_2fa(&u.id));
        // A new password signs them out; removing them ends their sessions.
        a.user_update(&u.id, "Alex", Role::Viewer, vec![], Some("newpass12")).unwrap();
        assert_eq!(a.check(&s), None);
        let s2 = a.login_as("alex", "newpass12", None, IP, true).unwrap();
        assert!(matches!(a.check(&s2), Some(Who::User { role: Role::Viewer, .. })));
        assert!(a.user_delete(&u.id));
        assert_eq!(a.check(&s2), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn api_tokens_scoped_and_expiring() {
        let dir = temp();
        let a = Auth::load(&dir).unwrap();
        let (tok, rec) = a.token_create("grafana", true, vec!["tnp".into()], None).unwrap();
        assert!(tok.starts_with("hst_"));
        assert_eq!(a.check(&tok), Some(Who::Token { id: rec.id.clone(), name: "grafana".into(), read_only: true, servers: vec!["tnp".into()] }));
        assert!(!std::fs::read_to_string(dir.join(FILE)).unwrap().contains(&tok));
        assert!(a.token_revoke(&rec.id));
        assert_eq!(a.check(&tok), None);
        let (tok2, _) = a.token_create("short", false, vec![], Some(1)).unwrap();
        assert!(a.check(&tok2).is_some());
        assert!(a.token_create("", false, vec![], None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pairing_flow() {
        let dir = temp();
        let a = Auth::load(&dir).unwrap();
        let (id, secret, code) = a.pair_request("Hopper on FOX", IP, "AA:BB").unwrap();
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
        let (id2, s2, _) = a.pair_request("Phone", IP, "AA:BB").unwrap();
        a.answer(&id2, false).unwrap();
        assert_eq!(a.pair_status(&id2, &s2), PairStatus::Denied);
        assert!(a.pair_request("", IP, "AA:BB").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
