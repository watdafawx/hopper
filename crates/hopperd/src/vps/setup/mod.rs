//! Setting up the relay on a VPS over ssh (design/vps-relay.md, B3): the
//! owner gives an ssh login, the panel checks the VPS, installs Hopper
//! Server there in its relay role as a systemd service, opens the firewall
//! for the two ports, and pairs itself with it. Each install step checks
//! before it acts, so running the setup again is the repair; Remove takes
//! out exactly what the setup put there.
//!
//! - The login (host, user, the VPS's host key and the private key) is kept
//!   in `vps-ssh.json`, owner-only, for repair and removal. A typed password
//!   is used once to put a new key on the VPS and is never kept. Nothing of
//!   the private key, the password or the relay key goes into a log, an
//!   error, the audit log or an API answer.
//! - The VPS's host key is learnt on the first connection, shown in the
//!   dialog, and must match on every later one.
//! - Every command is fixed text from [`plan`], shown under "Show the
//!   commands". Secrets go on the command's stdin.
//!
//! Test-only settings (never set by a release): `HOPPERD_VPS_TEST_BINARY`,
//! a Linux hopperd to upload instead of the release download, and
//! `HOPPERD_VPS_TEST_BIND`, the address the relay listens on (a test box
//! that must not be reachable from the network uses 127.0.0.1), and
//! `HOPPERD_VPS_TEST_OFFLINE=1`, Velocity without account checks (a test
//! box's logins have no Mojang account).
//!
//! B4 adds Velocity (design/vps-relay.md, "B4 as built"; B5 moved it to 4.x): Java and the
//! Velocity jar, both pinned with their SHA-256 in [`plan`], its own user
//! and service, and the forwarding secret, sent on stdin.

pub mod plan;
pub mod ssh;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::{HeaderMap, Method};
use axum::routing::{get, post};
use axum::{Json, Router};
use hopper_core::hosting::relay_codec::CodecConfig;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use self::plan::{BACKEND_PORT, BIN, DATA, ETC, Installed, JAVA, JAVA_DIR, OPT, SECRET, Step, UNIT, USER, VELOCITY, VELOCITY_DATA, VELOCITY_DIR, VELOCITY_JAR, VELOCITY_PORT, VELOCITY_SERVICE, VELOCITY_TOML, VELOCITY_UNIT, VELOCITY_USER, VOICE_PORT, as_relay, as_root, sh_quote};
use self::ssh::{Auth, Out, Ssh};
use super::home::{HomeConfig, TUNNEL_PORT, write_private};
use super::link;
use crate::App;
use crate::api::{ApiResult, bad, not_found, require_owner};

const LOGIN_FILE: &str = "vps-ssh.json";
pub const GAME_PORT: u16 = 25565;
const SHORT: Duration = Duration::from_secs(60);
const LONG: Duration = Duration::from_secs(600);

/// `vps-ssh.json`: the login kept for repair and removal.
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
struct Login {
    host: String,
    port: u16,
    user: String,
    /// "SHA256:…", learnt on the first connection.
    host_key: String,
    /// OpenSSH format, without a passphrase. Never sent back.
    private_key: String,
    public_key: String,
    /// The setup put `public_key` in the user's `authorized_keys` (signed
    /// in with a password), so Remove takes it out again.
    key_added: bool,
    root: bool,
    /// Made by "New key", waiting for the player to put it on the VPS.
    new_private: String,
    new_public: String,
}

#[derive(Clone, Serialize, Default)]
struct Run {
    /// "install" or "remove".
    kind: &'static str,
    running: bool,
    steps: Vec<Step>,
    error: String,
    /// Every command it ran, for "Show the commands" (no secrets: those go
    /// on stdin).
    commands: Vec<String>,
    /// Steps that changed something (0: a repair that found it all right).
    changed: usize,
    server: String,
    finished: Option<u64>,
}

pub struct Setup {
    dir: PathBuf,
    login: Mutex<Login>,
    check: Mutex<Option<Value>>,
    run: Mutex<Option<Run>>,
}

fn unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Setup {
    pub fn new(dir: &Path) -> Setup {
        let login = std::fs::read(dir.join(LOGIN_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Setup { dir: dir.to_path_buf(), login: Mutex::new(login), check: Mutex::new(None), run: Mutex::new(None) }
    }

    fn login(&self) -> Login {
        self.login.lock().unwrap().clone()
    }

    fn save_login(&self, l: Login) -> Result<(), String> {
        let empty = l.host.is_empty() && l.new_private.is_empty();
        let path = self.dir.join(LOGIN_FILE);
        if empty {
            let _ = std::fs::remove_file(&path);
        } else {
            write_private(&path, &serde_json::to_vec_pretty(&l).map_err(|e| e.to_string())?).map_err(|e| format!("Saving the login: {e:#}"))?;
        }
        *self.login.lock().unwrap() = l;
        Ok(())
    }

    fn busy(&self) -> bool {
        self.run.lock().unwrap().as_ref().is_some_and(|r| r.running)
    }

    fn with_run(&self, f: impl FnOnce(&mut Run)) {
        if let Some(r) = self.run.lock().unwrap().as_mut() {
            f(r);
        }
    }

    fn set_step(&self, id: &str, state: &'static str, detail: impl Into<String>) {
        let detail = detail.into();
        self.with_run(|r| {
            if let Some(s) = r.steps.iter_mut().find(|s| s.id == id) {
                s.state = state;
                s.detail = detail;
            }
            // A warning alone (a firewall to open by hand) changed nothing.
            if state == "done" {
                r.changed += 1;
            }
        });
    }
}

// ---- running commands --------------------------------------------------------------------

/// One connection's worth of a check or a run.
struct Ctx<'a> {
    app: &'a Arc<App>,
    ssh: Ssh,
    root: bool,
    /// Commands run, when they go into a run's list.
    log: Mutex<Vec<String>>,
    /// The relay's program or key changed: restart the service.
    restart: bool,
    /// Java, Velocity or its settings changed: restart Velocity.
    restart_velocity: bool,
}

impl Ctx<'_> {
    async fn exec(&self, cmd: &str, stdin: Option<&[u8]>, limit: Duration) -> Result<Out, String> {
        self.log.lock().unwrap().push(cmd.to_string());
        self.app.vps_setup.with_run(|r| r.commands.push(cmd.to_string()));
        self.ssh.run(cmd, stdin, limit).await
    }

    /// `cmd` with root rights.
    async fn root(&self, cmd: &str) -> Result<Out, String> {
        self.exec(&as_root(self.root, cmd), None, SHORT).await
    }

    async fn root_in(&self, cmd: &str, stdin: &[u8], limit: Duration) -> Result<Out, String> {
        self.exec(&as_root(self.root, cmd), Some(stdin), limit).await
    }

    /// `cmd` with root rights that has to work.
    async fn must(&self, cmd: &str, what: &str) -> Result<Out, String> {
        let o = self.root(cmd).await?;
        if o.ok() { Ok(o) } else { Err(format!("{what}: {}", o.why())) }
    }
}

/// Signs in with the saved login.
async fn connect_saved(app: &Arc<App>) -> Result<Ctx<'_>, String> {
    let l = app.vps_setup.login();
    if l.host.is_empty() || l.private_key.is_empty() {
        return Err("Connect to the VPS first.".into());
    }
    let ssh = Ssh::connect(&l.host, l.port, &l.user, Auth::Key(ssh::decode(&l.private_key)?), &l.host_key).await?;
    let root = ssh.run("id -u", None, SHORT).await.map(|o| o.out.trim() == "0").unwrap_or(false);
    Ok(Ctx { app, ssh, root, log: Mutex::new(Vec::new()), restart: false, restart_velocity: false })
}

fn bind_addr() -> String {
    std::env::var("HOPPERD_VPS_TEST_BIND").ok().filter(|b| b.parse::<std::net::IpAddr>().is_ok()).unwrap_or_else(|| "0.0.0.0".into())
}

fn offline_test() -> bool {
    std::env::var("HOPPERD_VPS_TEST_OFFLINE").is_ok_and(|v| v == "1")
}

fn test_binary() -> Option<PathBuf> {
    std::env::var_os("HOPPERD_VPS_TEST_BINARY").map(PathBuf::from).filter(|p| p.is_file())
}

fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

// ---- the check -----------------------------------------------------------------------------

#[derive(Default)]
struct Tools {
    curl: bool,
    wget: bool,
    systemd: bool,
    ufw: bool,
    iptables: bool,
    nft: bool,
    persist: bool,
    runuser: bool,
}

async fn tools(c: &Ctx<'_>) -> Result<Tools, String> {
    // One `command -v` per name: dash's answers only the first of several.
    // Root's PATH has the sbin ones.
    let o = c.root("for c in curl wget systemctl ufw iptables nft netfilter-persistent runuser useradd; do command -v \"$c\"; done; true").await?;
    let has = |n: &str| o.out.lines().any(|l| l.trim().rsplit('/').next() == Some(n));
    Ok(Tools {
        curl: has("curl"),
        wget: has("wget"),
        systemd: has("systemctl"),
        ufw: has("ufw"),
        iptables: has("iptables"),
        nft: has("nft"),
        persist: has("netfilter-persistent"),
        runuser: has("runuser") && has("useradd"),
    })
}

fn row(id: &str, ok: bool, text: impl Into<String>) -> Value {
    json!({ "id": id, "state": if ok { "ok" } else { "fail" }, "text": text.into() })
}

async fn run_check(app: &Arc<App>, ports: (u16, u16)) -> Result<Value, String> {
    let c = connect_saved(app).await?;
    let l = app.vps_setup.login();
    let mut rows = Vec::new();
    let os = plan::parse_os_release(&c.exec("cat /etc/os-release", None, SHORT).await?.out);
    let arch_raw = c.exec("uname -m", None, SHORT).await?.out;
    let arch = plan::parse_arch(&arch_raw);
    match (plan::os_supported(&os), arch) {
        (Ok(name), Some(a)) => rows.push(row("system", true, format!("{name} · {a}"))),
        (Err(e), _) => rows.push(row("system", false, e)),
        (Ok(name), None) => rows.push(row("system", false, format!("{name} on {} isn't supported: the relay runs on x86_64 (x64) and aarch64 (arm64).", arch_raw.trim()))),
    }
    let mem = plan::parse_meminfo(&c.exec("cat /proc/meminfo", None, SHORT).await?.out).unwrap_or(0);
    let cache = CodecConfig::for_memory(mem).cache_bytes;
    rows.push(if mem >= plan::MIN_MEMORY {
        row("memory", true, format!("{} memory: the relay keeps a {} cache", plan::size(mem), plan::size(cache)))
    } else {
        row("memory", false, format!("The VPS has {} of memory. The relay needs 2 GB.", plan::size(mem)))
    });
    let disk = plan::parse_df_avail(&c.exec("df -Pk /opt /var/lib", None, SHORT).await?.out).unwrap_or(0);
    rows.push(if disk >= plan::MIN_DISK { row("disk", true, format!("{} free", plan::size(disk))) } else { row("disk", false, format!("Only {} free. The relay needs 2 GB.", plan::size(disk))) });
    let sudo = c.root || c.exec("sudo -n true", None, SHORT).await?.ok();
    rows.push(if c.root {
        row("sudo", true, "Signed in as root")
    } else if sudo {
        row("sudo", true, format!("Signed in as {}, sudo works", l.user))
    } else {
        row("sudo", false, format!("{} can't use sudo without a password. Sign in as a user that can (cloud images' default user can), or as root.", l.user))
    });
    if sudo {
        let t = tools(&c).await?;
        if !t.systemd {
            rows.push(row("tools", false, "This VPS doesn't run systemd, which the relay service needs."));
        } else if !t.runuser {
            rows.push(row("tools", false, "useradd or runuser is missing on the VPS."));
        } else if !t.curl && !t.wget {
            // Java and Velocity are downloaded on the VPS.
            rows.push(row("tools", false, "Neither curl nor wget is on the VPS. Install one (sudo apt install curl), then check again."));
        }
        // Ports: free, or already the relay's own.
        let listening = plan::parse_listening(&c.root("ss -Hltn").await?.out);
        let ours = c.root(&format!("systemctl is-active {}", plan::SERVICE)).await?.out.trim() == "active";
        let mut theirs = Vec::new();
        if ours && let Some(s) = plan::last_json::<plan::RelayStatus>(&c.root(&as_relay(&format!("{BIN} relay status --data {DATA}"))).await?.out) {
            theirs = vec![s.game_port, s.tunnel_port, s.backend_port];
        }
        if c.root(&format!("systemctl is-active {VELOCITY_SERVICE}")).await?.out.trim() == "active" {
            theirs.push(VELOCITY_PORT);
        }
        // Velocity's two ports on the VPS's own loopback (another Velocity
        // there would take them).
        let inner: Vec<String> = [VELOCITY_PORT, BACKEND_PORT].into_iter().filter(|p| listening.contains(p) && !theirs.contains(p)).map(|p| p.to_string()).collect();
        if !inner.is_empty() {
            rows.push(row("inner-ports", false, format!("Port {} is in use by another program on the VPS, maybe another Velocity. The relay runs its own Velocity there: stop that program, then check again.", inner.join(" and "))));
        }
        let busy: Vec<u16> = [ports.0, ports.1].into_iter().filter(|p| listening.contains(p) && !theirs.contains(p)).collect();
        rows.push(if ports.0 == ports.1 {
            row("ports", false, "The two ports must differ.")
        } else if busy.is_empty() {
            row("ports", true, if theirs.contains(&ports.0) { format!("Ports {} and {}: the relay's already", ports.0, ports.1) } else { format!("Ports {} and {} are free", ports.0, ports.1) })
        } else {
            let list: Vec<String> = busy.iter().map(u16::to_string).collect();
            json!({ "id": "ports", "state": "fail", "text": format!("Port {} is in use by another program on the VPS. Pick another below.", list.join(" and ")), "busy": busy })
        });
    }
    c.ssh.close().await;
    let ok = rows.iter().all(|r| r["state"] == "ok");
    let commands = c.log.lock().unwrap().clone();
    let res = json!({
        "ok": ok, "rows": rows, "os": os.pretty, "arch": arch, "memory": mem, "cache": cache,
        "ports": { "game": ports.0, "tunnel": ports.1 }, "commands": commands, "at": unix(),
    });
    *app.vps_setup.check.lock().unwrap() = Some(res.clone());
    Ok(res)
}

// ---- the program for the VPS --------------------------------------------------------------

enum Source {
    /// A file on this PC (tests): its bytes and SHA-256.
    File(Vec<u8>, String),
    /// The release download: its URL and the SHA-256 GitHub lists for it.
    Release { url: String, sha256: String },
}

fn sha256_hex(b: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

/// Where the VPS gets the same hopperd version as this panel.
async fn source(arch: &str) -> Result<Source, String> {
    if let Some(p) = test_binary() {
        let b = tokio::fs::read(&p).await.map_err(|e| format!("Reading {}: {e}", p.display()))?;
        if plan::elf_arch(&b) != Some(arch) {
            return Err(format!("The test binary isn't a Linux {arch} program."));
        }
        let sha = sha256_hex(&b);
        return Ok(Source::File(b, sha));
    }
    let (v, arch) = (version().to_string(), arch.to_string());
    tokio::task::spawn_blocking(move || release_asset(&v, &arch)).await.map_err(|e| e.to_string())?
}

#[derive(Deserialize)]
struct GhRelease {
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    digest: Option<String>,
}

fn release_asset(v: &str, arch: &str) -> Result<Source, String> {
    let repo = crate::autoupdate::REPO;
    let url = format!("https://api.github.com/repos/{repo}/releases/tags/hopperd-v{v}");
    let missing = || format!("Hopper Server {v} has no Linux {arch} download on GitHub, so the relay can't be installed from this version. Update Hopper Server and try again.");
    let r: GhRelease = hopper_core::cf::agent().get(&url).header("Accept", "application/vnd.github+json").call().map_err(|_| missing())?.body_mut().read_json().map_err(|e| format!("Reading GitHub's answer: {e}"))?;
    let name = format!("hopperd-{v}-linux-{arch}.tar.gz");
    let a = r.assets.into_iter().find(|a| a.name == name).ok_or_else(missing)?;
    // Only from this repository's releases, and only with a checksum to
    // check it against on the VPS.
    let prefix = format!("https://github.com/{repo}/releases/download/");
    if !a.browser_download_url.starts_with(&prefix) || a.browser_download_url.chars().any(|c| c.is_whitespace() || c == '\'') {
        return Err(missing());
    }
    let sha256 = crate::updswap::expected_sha256(a.digest.as_deref()).filter(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit())).ok_or_else(|| format!("GitHub lists no checksum for {name}, so it isn't installed."))?;
    Ok(Source::Release { url: a.browser_download_url, sha256 })
}

// ---- install -------------------------------------------------------------------------------

type StepResult = Result<(&'static str, String), String>;

async fn load_installed(c: &Ctx<'_>) -> Installed {
    let o = c.root(&format!("cat {} 2>/dev/null", plan::INSTALLED)).await;
    o.ok().and_then(|o| serde_json::from_str(&o.out).ok()).unwrap_or_default()
}

async fn save_installed(c: &Ctx<'_>, i: &Installed) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(i).map_err(|e| e.to_string())?;
    let p = plan::INSTALLED;
    // Root's, in root's folder: the relay's own user can't change what
    // Remove will take out.
    let o = c.root_in(&format!("if [ -d {OPT} ]; then umask 077 && cat > {p}.tmp && mv -f {p}.tmp {p}; else cat > /dev/null; fi"), &body, SHORT).await?;
    if o.ok() { Ok(()) } else { Err(format!("Saving what was installed: {}", o.why())) }
}

async fn step_user(c: &Ctx<'_>, inst: &mut Installed) -> StepResult {
    let now = c.root(&format!("id -u {USER} >/dev/null 2>&1 && stat -c '%n %U %G %a' {OPT} {DATA}")).await?;
    if plan::folders_right(now.ok(), &now.out) {
        return Ok(("ok", format!("{USER}, {OPT}, {DATA}")));
    }
    if !c.root(&format!("id -u {USER}")).await?.ok() {
        c.must(&format!("useradd --system --user-group --no-create-home --home-dir {DATA} --shell /usr/sbin/nologin {USER}"), "Making the relay user").await?;
        inst.made_user = true;
    }
    c.must(&format!("install -d -o root -g root -m 0755 {OPT} && install -d -o {USER} -g {USER} -m 0700 {DATA}"), "Making the folders").await?;
    Ok(("done", "made".into()))
}

async fn step_binary(c: &mut Ctx<'_>, inst: &mut Installed, arch: &str) -> StepResult {
    let src = source(arch).await?;
    let cur = c.root(&format!("sha256sum {BIN} 2>/dev/null")).await?.out.split_whitespace().next().unwrap_or("").to_string();
    let ran = c.root(&format!("{BIN} --version 2>&1")).await?;
    let says = ran.out.trim().to_string();
    let file_sha = match &src {
        Source::File(_, sha) => Some(sha.as_str()),
        Source::Release { .. } => None,
    };
    let right = plan::binary_right(&cur, &inst.binary_sha256, &says, version(), file_sha);
    if right && ran.ok() {
        return Ok(("ok", if matches!(src, Source::File(..)) { "the test file, already there".into() } else { "already there, checksum ok".into() }));
    }
    let tmp = format!("{OPT}/.download");
    match &src {
        Source::File(bytes, sha) => {
            // Sent on stdin, checked on the VPS before it replaces anything.
            let cmd = format!("rm -rf {tmp} && mkdir -m 0700 {tmp} && cat > {tmp}/hopperd && echo {} | sha256sum -c --quiet && install -o root -g root -m 0755 {tmp}/hopperd {BIN}.new && mv -f {BIN}.new {BIN} && rm -rf {tmp}", sh_quote(&format!("{sha}  {tmp}/hopperd")));
            let o = c.root_in(&cmd, bytes, LONG).await?;
            if !o.ok() {
                return Err(format!("Copying the relay program: {}", o.why()));
            }
        }
        Source::Release { url, sha256 } => {
            let t = tools(c).await?;
            let get = if t.curl { format!("curl -fsSL --proto =https -o {tmp}/h.tar.gz {}", sh_quote(url)) } else { format!("wget -q -O {tmp}/h.tar.gz {}", sh_quote(url)) };
            let cmd = format!(
                "rm -rf {tmp} && mkdir -m 0700 {tmp} && {get} && echo {} | sha256sum -c --quiet && tar -xzf {tmp}/h.tar.gz -C {tmp} hopperd && install -o root -g root -m 0755 {tmp}/hopperd {BIN}.new && mv -f {BIN}.new {BIN} && rm -rf {tmp}",
                sh_quote(&format!("{sha256}  {tmp}/h.tar.gz"))
            );
            let o = c.exec(&as_root(c.root, &cmd), None, LONG).await?;
            if !o.ok() {
                let _ = c.root(&format!("rm -rf {tmp}")).await;
                return Err(if o.err.contains("FAILED") || o.out.contains("FAILED") {
                    "The download doesn't match the checksum GitHub lists, so it wasn't installed.".into()
                } else {
                    format!("Downloading the relay program: {}", o.why())
                });
            }
        }
    }
    // It must run here (a program built for a newer system won't).
    let ran = c.root(&format!("{BIN} --version 2>&1")).await?;
    if !ran.ok() || !ran.out.starts_with("hopperd ") {
        return Err(format!("The relay program doesn't run on this VPS: {}", ran.why()));
    }
    inst.binary_sha256 = c.root(&format!("sha256sum {BIN}")).await?.out.split_whitespace().next().unwrap_or("").to_string();
    inst.binary_version = ran.out.trim().trim_start_matches("hopperd ").to_string();
    c.restart = true;
    Ok(("done", match src {
        Source::File(..) => "copied from this PC, checksum ok".into(),
        Source::Release { .. } => "downloaded, checksum ok".into(),
    }))
}

/// The address home dials: `host:port`, IPv6 in brackets.
fn tunnel_address(host: &str, port: u16) -> String {
    if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") }
}

async fn step_keys(c: &mut Ctx<'_>, host: &str, ports: (u16, u16)) -> StepResult {
    let bind = bind_addr();
    let st: Option<plan::RelayStatus> = plan::last_json(&c.root(&as_relay(&format!("{BIN} relay status --data {DATA}"))).await?.out);
    let home = c.app.vps.config();
    let addr = tunnel_address(host, ports.1);
    // Its folder only for its user, every file in it too.
    let private = plan::data_private(&c.root(&format!("stat -c '%U %a' {DATA} {DATA}/*")).await?.out);
    let (key_ok, all_ok) = plan::keys_right(st.as_ref(), &home.key, &home.fingerprint, &home.address, &addr, &bind, ports, private);
    if all_ok {
        return Ok(("ok", "key matches this panel's".into()));
    }
    let key = if key_ok { home.key.clone() } else { link::new_key() };
    let init = format!("{BIN} relay init --data {DATA} --key-stdin --bind {bind} --game-port {} --tunnel-port {} --velocity-port {VELOCITY_PORT} --backend-port {BACKEND_PORT} --voice-port {VOICE_PORT}", ports.0, ports.1);
    let o = c.root_in(&as_relay(&init), format!("{key}\n").as_bytes(), SHORT).await?;
    let got: Option<Value> = plan::last_json(&o.out);
    let fp = got.as_ref().and_then(|v| v["fingerprint"].as_str()).map(str::to_string);
    let (true, Some(fp)) = (o.ok(), fp) else { return Err(format!("Making the relay key: {}", o.why())) };
    c.must(&format!("chown {USER}:{USER} {DATA}/* && chmod 0600 {DATA}/* && chmod 0700 {DATA}"), "Locking the relay's files").await?;
    // Saved here at once, so the key is never only on the VPS's side.
    let mut cfg = c.app.vps.config();
    cfg.key = key;
    cfg.fingerprint = fp;
    cfg.address = addr;
    c.app.vps.set_config(cfg).map_err(|e| format!("Saving the relay key here: {e:#}"))?;
    c.restart = true;
    Ok(("done", if key_ok { "settings updated".into() } else { "new key, saved here".into() }))
}

/// Downloads `p` to `dest` on the VPS and checks it against its pinned
/// SHA-256 there; a file that doesn't match is deleted.
async fn fetch(c: &Ctx<'_>, p: &plan::Pinned, dest: &str) -> Result<(), String> {
    let t = tools(c).await?;
    let get = if t.curl {
        format!("curl -fsSL --proto =https -o {dest} {}", sh_quote(p.url))
    } else if t.wget {
        format!("wget -q -O {dest} {}", sh_quote(p.url))
    } else {
        return Err("Neither curl nor wget is on the VPS. Install one (sudo apt install curl), then run the setup again.".into());
    };
    let cmd = format!("{get} && echo {} | sha256sum -c --quiet", sh_quote(&format!("{}  {dest}", p.sha256)));
    let o = c.exec(&as_root(c.root, &cmd), None, LONG).await?;
    if o.ok() {
        return Ok(());
    }
    let _ = c.root(&format!("rm -f {dest}")).await;
    Err(if o.err.contains("FAILED") || o.out.contains("FAILED") { format!("The {} download doesn't match its checksum, so it wasn't installed.", p.name) } else { format!("Downloading {}: {}", p.name, o.why()) })
}

async fn step_java(c: &mut Ctx<'_>, inst: &mut Installed, arch: &str) -> StepResult {
    let p = plan::java(arch).ok_or("There's no Java for this VPS's CPU.")?;
    let ran = c.root(&format!("{JAVA} -version 2>&1")).await?;
    if inst.java_sha256 == p.sha256 && ran.ok() {
        return Ok(("ok", format!("{}, already there", p.name)));
    }
    let tmp = format!("{OPT}/.java");
    c.must(&format!("rm -rf {tmp} && mkdir -m 0700 {tmp} {tmp}/x"), "Making a folder for Java").await?;
    fetch(c, &p, &format!("{tmp}/java.tar.gz")).await?;
    c.must(
        &format!("tar -xzf {tmp}/java.tar.gz -C {tmp}/x --strip-components=1 && chown -R root:root {tmp}/x && chmod -R go-w,a+rX {tmp}/x && rm -rf {JAVA_DIR}.old && if [ -d {JAVA_DIR} ]; then mv {JAVA_DIR} {JAVA_DIR}.old; fi && mv {tmp}/x {JAVA_DIR} && rm -rf {JAVA_DIR}.old {tmp}"),
        "Unpacking Java",
    )
    .await?;
    let ran = c.root(&format!("{JAVA} -version 2>&1")).await?;
    if !ran.ok() {
        return Err(format!("Java doesn't run on this VPS: {}", ran.why()));
    }
    inst.java_sha256 = p.sha256.to_string();
    c.restart_velocity = true;
    Ok(("done", "downloaded, checksum ok".into()))
}

async fn step_velocity(c: &mut Ctx<'_>, inst: &mut Installed) -> StepResult {
    let mut did = Vec::new();
    if !c.root(&format!("id -u {VELOCITY_USER}")).await?.ok() {
        c.must(&format!("useradd --system --user-group --no-create-home --home-dir {VELOCITY_DATA} --shell /usr/sbin/nologin {VELOCITY_USER}"), "Making Velocity's user").await?;
        inst.made_velocity_user = true;
        did.push("its user");
    }
    c.must(&format!("install -d -o root -g root -m 0755 {VELOCITY_DIR} && install -d -o root -g {VELOCITY_USER} -m 0750 {ETC} && install -d -o {VELOCITY_USER} -g {VELOCITY_USER} -m 0700 {VELOCITY_DATA}"), "Making Velocity's folders").await?;
    let jar_sha = c.root(&format!("sha256sum {VELOCITY_JAR} 2>/dev/null")).await?.out.split_whitespace().next().unwrap_or("").to_string();
    if jar_sha != VELOCITY.sha256 {
        let tmp = format!("{VELOCITY_DIR}/.download.jar");
        fetch(c, &VELOCITY, &tmp).await?;
        c.must(&format!("chown root:root {tmp} && chmod 0644 {tmp} && mv -f {tmp} {VELOCITY_JAR}"), "Putting Velocity in place").await?;
        did.push("downloaded, checksum ok");
    }
    // The secret only ever goes on stdin; only its checksum is compared.
    let secret = c.app.vps_forward.ensure_secret().map_err(|e| format!("Making the forwarding secret: {e:#}"))?;
    let have = c.root(&format!("sha256sum {SECRET} 2>/dev/null")).await?.out.split_whitespace().next().unwrap_or("").to_string();
    if have != sha256_hex(secret.as_bytes()) {
        let o = c.root_in(&format!("umask 027 && cat > {SECRET}.tmp && chown root:{VELOCITY_USER} {SECRET}.tmp && chmod 0640 {SECRET}.tmp && mv -f {SECRET}.tmp {SECRET}"), secret.as_bytes(), SHORT).await?;
        if !o.ok() {
            return Err(format!("Writing the forwarding secret: {}", o.why()));
        }
        did.push("forwarding secret");
    }
    let toml = plan::velocity_toml(!offline_test());
    if c.root(&format!("cat {VELOCITY_TOML} 2>/dev/null")).await?.out != toml {
        let o = c.root_in(&format!("umask 027 && cat > {VELOCITY_TOML}.tmp && chown root:{VELOCITY_USER} {VELOCITY_TOML}.tmp && chmod 0640 {VELOCITY_TOML}.tmp && mv -f {VELOCITY_TOML}.tmp {VELOCITY_TOML}"), toml.as_bytes(), SHORT).await?;
        if !o.ok() {
            return Err(format!("Writing Velocity's settings: {}", o.why()));
        }
        did.push("settings");
    }
    // Velocity sends usage numbers to bStats unless its file says no; a
    // relay set up for a player doesn't. Written as Velocity's user, since
    // its own folder could hold a link a root write would follow.
    let bstats = format!("{VELOCITY_DATA}/plugins/bStats/config.txt");
    if !c.root(&format!("grep -qx enabled=false {bstats}")).await?.ok() {
        let o = c.root_in(&format!("runuser -u {VELOCITY_USER} -- mkdir -p {VELOCITY_DATA}/plugins/bStats && runuser -u {VELOCITY_USER} -- tee {bstats} >/dev/null"), b"enabled=false\n", SHORT).await?;
        if !o.ok() {
            return Err(format!("Turning off Velocity's usage reports: {}", o.why()));
        }
        did.push("usage reports off");
    }
    let stat = c.root(&format!("stat -c '%n %U %G %a' {ETC} {SECRET} {VELOCITY_TOML} {VELOCITY_DATA} {VELOCITY_JAR}")).await?.out;
    if !plan::velocity_files_right(&stat) {
        c.must(
            &format!("chown root:{VELOCITY_USER} {ETC} {SECRET} {VELOCITY_TOML} && chmod 0750 {ETC} && chmod 0640 {SECRET} {VELOCITY_TOML} && chown {VELOCITY_USER}:{VELOCITY_USER} {VELOCITY_DATA} && chmod 0700 {VELOCITY_DATA} && chown root:root {VELOCITY_JAR} && chmod 0644 {VELOCITY_JAR}"),
            "Locking Velocity's files",
        )
        .await?;
        did.push("file rights");
    }
    if did.is_empty() {
        return Ok(("ok", format!("{}, already there", VELOCITY.name)));
    }
    c.restart_velocity = true;
    Ok(("done", did.join(", ")))
}

async fn step_velocity_service(c: &Ctx<'_>) -> StepResult {
    let unit = plan::velocity_unit();
    let svc = VELOCITY_SERVICE;
    let now = c.root(&format!("cat {VELOCITY_UNIT} 2>/dev/null")).await?.out;
    let enabled = c.root(&format!("systemctl is-enabled {svc}")).await?.out;
    let active = c.root(&format!("systemctl is-active {svc}")).await?.out;
    let acts = plan::service_acts_for(&unit, &now, &enabled, &active, c.restart_velocity);
    let changed = !acts.is_empty();
    for a in acts {
        match a {
            plan::ServiceAct::WriteUnit => {
                let o = c.root_in(&format!("umask 022 && cat > {VELOCITY_UNIT}.tmp && mv -f {VELOCITY_UNIT}.tmp {VELOCITY_UNIT} && systemctl daemon-reload"), unit.as_bytes(), SHORT).await?;
                if !o.ok() {
                    return Err(format!("Writing Velocity's service: {}", o.why()));
                }
            }
            plan::ServiceAct::Enable => _ = c.must(&format!("systemctl enable {svc}"), "Turning Velocity's service on").await?,
            plan::ServiceAct::Restart => _ = c.must(&format!("systemctl restart {svc}"), "Starting Velocity").await?,
        }
    }
    // Java takes a few seconds; up once it listens.
    for _ in 0..120 {
        let l = plan::parse_listening(&c.root("ss -Hltn").await?.out);
        if l.contains(&VELOCITY_PORT) && c.root(&format!("systemctl is-active {svc}")).await?.out.trim() == "active" {
            return Ok((if changed { "done" } else { "ok" }, if changed { "started".into() } else { "running".into() }));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let j = c.root(&format!("journalctl -u {svc} -n 8 --no-pager -o cat")).await.map(|o| o.out).unwrap_or_default();
    let last = j.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no log").trim().to_string();
    Err(format!("Velocity didn't start: {}", hopper_core::applog::redact(&last)))
}

async fn step_service(c: &Ctx<'_>, ports: (u16, u16)) -> StepResult {
    let unit = plan::unit();
    let svc = plan::SERVICE;
    let now = c.root(&format!("cat {UNIT} 2>/dev/null")).await?.out;
    let enabled = c.root(&format!("systemctl is-enabled {svc}")).await?.out;
    let active = c.root(&format!("systemctl is-active {svc}")).await?.out;
    let acts = plan::service_acts(&now, &enabled, &active, c.restart);
    let changed = !acts.is_empty();
    for a in acts {
        match a {
            plan::ServiceAct::WriteUnit => {
                let o = c.root_in(&format!("umask 022 && cat > {UNIT}.tmp && mv -f {UNIT}.tmp {UNIT} && systemctl daemon-reload"), unit.as_bytes(), SHORT).await?;
                if !o.ok() {
                    return Err(format!("Writing the service: {}", o.why()));
                }
            }
            plan::ServiceAct::Enable => _ = c.must(&format!("systemctl enable {svc}"), "Turning the service on").await?,
            plan::ServiceAct::Restart => _ = c.must(&format!("systemctl restart {svc}"), "Starting the service").await?,
        }
    }
    // Up once it listens on both ports.
    for _ in 0..30 {
        let l = plan::parse_listening(&c.root("ss -Hltn").await?.out);
        if l.contains(&ports.0) && l.contains(&ports.1) && c.root(&format!("systemctl is-active {svc}")).await?.out.trim() == "active" {
            return Ok((if changed { "done" } else { "ok" }, if changed { "started".into() } else { "running".into() }));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let j = c.root(&format!("journalctl -u {svc} -n 8 --no-pager -o cat")).await.map(|o| o.out).unwrap_or_default();
    let last = j.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no log").trim().to_string();
    Err(format!("The relay service didn't start: {}", hopper_core::applog::redact(&last)))
}

async fn firewall_state(c: &Ctx<'_>) -> Result<(Tools, Option<String>, Option<String>, Option<String>), String> {
    let t = tools(c).await?;
    let ufw = if t.ufw { Some(c.root("ufw status").await?.out) } else { None };
    let ipt = if t.iptables { Some(c.root("iptables -S INPUT").await?).filter(Out::ok).map(|o| o.out) } else { None };
    let nft = if t.nft { Some(c.root("nft list ruleset").await?).filter(Out::ok).map(|o| o.out) } else { None };
    Ok((t, ufw, ipt, nft))
}

async fn step_firewall(c: &Ctx<'_>, inst: &mut Installed, ports: (u16, u16)) -> StepResult {
    let (t, ufw, ipt, nft) = firewall_state(c).await?;
    let p = plan::plan_firewall(ufw.as_deref(), ipt.as_deref(), nft.as_deref(), t.persist, &[ports.0, ports.1], &[VOICE_PORT]);
    for cmd in &p.add {
        c.must(cmd, "Opening the port").await?;
    }
    for r in &p.rules {
        if !inst.firewall.contains(r) {
            inst.firewall.push(r.clone());
        }
    }
    if let Some(m) = p.manual {
        return Ok(("warn", m));
    }
    if p.rules.is_empty() {
        // No firewall on, or one that lets everything in: nothing to open.
        let none = ufw.as_deref().is_none_or(|s| !plan::ufw_active(s)) && ipt.as_deref().is_none_or(|r| plan::iptables_block_at(r, &[ports.0, ports.1, VOICE_PORT]).is_none());
        return Ok(("ok", if none { "nothing on the VPS blocks them".into() } else { "already open".into() }));
    }
    let ipt_added = p.rules.iter().any(|r| matches!(r, plan::FwRule::Iptables { .. }));
    if ipt_added && !p.saved {
        c.app.vps_setup.with_run(|r| r.changed += 1);
        return Ok(("warn", "opened until the VPS restarts (netfilter-persistent isn't installed); run the setup again after a restart".into()));
    }
    Ok(("done", "opened".into()))
}

async fn step_reach(host: &str, ports: (u16, u16)) -> StepResult {
    let mut shut = Vec::new();
    for p in [ports.0, ports.1] {
        let ok = matches!(tokio::time::timeout(Duration::from_secs(6), tokio::net::TcpStream::connect((host, p))).await, Ok(Ok(_)));
        if !ok {
            shut.push(p);
        }
    }
    if shut.is_empty() {
        // UDP has no answer to wait for here: home's voice link checks it once
        // connected, and the relay card says when it doesn't get through.
        return Ok(("ok", format!("{} and {} answer from here", ports.0, ports.1)));
    }
    let list: Vec<String> = shut.iter().map(|p| format!("{p}/tcp")).collect();
    Err(format!("Can't reach {} from here. Open {} in your cloud provider's firewall page for this VPS, then press Run again.", list.join(" and "), list.join(" and ")))
}

async fn step_pair(app: &Arc<App>, server: &str, public_address: Option<&str>) -> StepResult {
    let before = app.vps.config();
    let mut cfg = before.clone();
    cfg.enabled = true;
    if !cfg.servers.iter().any(|s| s == server) {
        cfg.servers.push(server.to_string());
    }
    if let Some(p) = public_address {
        cfg.public_address = p.to_string();
    }
    let changed = cfg != before;
    if changed {
        app.vps.set_config(cfg).map_err(|e| format!("Saving the relay settings: {e:#}"))?;
    }
    for _ in 0..40 {
        let s = app.vps.status();
        if s.state == "up" {
            return Ok((if changed { "done" } else { "ok" }, "connected".into()));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let s = app.vps.status();
    Err(if s.error.is_empty() { "This panel couldn't connect to the relay.".into() } else { s.error })
}

struct InstallArgs {
    server: String,
    ports: (u16, u16),
    public_address: Option<String>,
}

async fn install(app: &Arc<App>, a: &InstallArgs) -> Result<(), String> {
    let mut c = connect_saved(app).await?;
    let host = app.vps_setup.login().host;
    let arch_raw = c.exec("uname -m", None, SHORT).await?.out;
    let arch = plan::parse_arch(&arch_raw).ok_or("The VPS's CPU isn't supported.")?;
    let mut inst = load_installed(&c).await;
    let s = &app.vps_setup;
    macro_rules! step {
        ($id:literal, $e:expr) => {{
            s.set_step($id, "run", "");
            match $e {
                Ok((state, detail)) => s.set_step($id, state, detail),
                Err(e) => {
                    s.set_step($id, "fail", e.clone());
                    let _ = save_installed(&c, &inst).await;
                    c.ssh.close().await;
                    return Err(e);
                }
            }
        }};
    }
    step!("user", step_user(&c, &mut inst).await);
    save_installed(&c, &inst).await?;
    step!("binary", step_binary(&mut c, &mut inst, arch).await);
    save_installed(&c, &inst).await?;
    step!("java", step_java(&mut c, &mut inst, arch).await);
    save_installed(&c, &inst).await?;
    step!("velocity", step_velocity(&mut c, &mut inst).await);
    save_installed(&c, &inst).await?;
    step!("keys", step_keys(&mut c, &host, a.ports).await);
    step!("service", step_service(&c, a.ports).await);
    step!("velocity-service", step_velocity_service(&c).await);
    step!("firewall", step_firewall(&c, &mut inst, a.ports).await);
    save_installed(&c, &inst).await?;
    c.ssh.close().await;
    step!("reach", step_reach(&host, a.ports).await);
    step!("pair", step_pair(app, &a.server, a.public_address.as_deref()).await);
    Ok(())
}

// ---- remove ----------------------------------------------------------------------------

async fn remove(app: &Arc<App>) -> Result<(), String> {
    let c = connect_saved(app).await?;
    let s = &app.vps_setup;
    let inst = load_installed(&c).await;
    let svc = plan::SERVICE;
    macro_rules! step {
        ($id:literal, $e:expr) => {{
            s.set_step($id, "run", "");
            let r: StepResult = $e;
            match r {
                Ok((state, detail)) => s.set_step($id, state, detail),
                Err(e) => {
                    s.set_step($id, "fail", e.clone());
                    c.ssh.close().await;
                    return Err(e);
                }
            }
        }};
    }
    step!("service", async {
        let mut did = 0;
        for (unit, svc) in [(VELOCITY_UNIT, VELOCITY_SERVICE), (UNIT, svc)] {
            if c.root(&format!("test -e {unit} || systemctl cat {svc} >/dev/null 2>&1")).await?.ok() {
                c.must(&format!("systemctl disable --now {svc} 2>/dev/null; rm -f {unit} && systemctl daemon-reload"), "Removing the service").await?;
                did += 1;
            }
        }
        Ok(if did == 0 { ("ok", "not there".into()) } else { ("done", "stopped and removed".into()) })
    }
    .await);
    step!("firewall", async {
        let (t, _, ipt, _) = firewall_state(&c).await?;
        let mut rules = inst.firewall.clone();
        // A rule marked as the relay's that the record lost (a run that
        // stopped half-way) goes too; nothing unmarked is touched.
        for r in ipt.as_deref().map(plan::iptables_marked).unwrap_or_default() {
            if !rules.contains(&r) {
                rules.push(r);
            }
        }
        if rules.is_empty() {
            return Ok(("ok", "it added none".into()));
        }
        for cmd in plan::remove_firewall(&rules, t.persist) {
            // ufw says so when a rule is already gone; that's fine.
            let _ = c.root(&cmd).await?;
        }
        Ok(("done", format!("{} taken out", rules.len())))
    }
    .await);
    step!("files", async {
        if !c.root(&format!("test -e {OPT} -o -e {DATA} -o -e {ETC} -o -e {VELOCITY_DATA}")).await?.ok() {
            return Ok(("ok", "not there".into()));
        }
        c.must(&format!("rm -rf {OPT} {DATA} {ETC} {VELOCITY_DATA}"), "Deleting the files").await?;
        Ok(("done", format!("{OPT}, {DATA}, {ETC}, {VELOCITY_DATA}")))
    }
    .await);
    step!("user", async {
        let mut did = Vec::new();
        for u in [USER, VELOCITY_USER] {
            let mut gone = false;
            if c.root(&format!("id -u {u}")).await?.ok() {
                c.must(&format!("userdel {u}"), "Deleting the user").await?;
                gone = true;
            }
            if c.root(&format!("getent group {u}")).await?.ok() {
                c.must(&format!("groupdel {u}"), "Deleting the group").await?;
                gone = true;
            }
            if gone {
                did.push(u);
            }
        }
        Ok(if did.is_empty() { ("ok", "not there".into()) } else { ("done", did.join(", ")) })
    }
    .await);
    step!("pair", async {
        let before = app.vps.config();
        if before == HomeConfig::default() {
            return Ok(("ok", "not paired".into()));
        }
        app.vps.set_config(HomeConfig::default()).map_err(|e| format!("{e:#}"))?;
        Ok(("done", "disconnected".into()))
    }
    .await);
    step!("login", async {
        let l = app.vps_setup.login();
        if l.key_added {
            // The key line the setup added, nothing else in the file.
            let cmd = format!(
                "f=\"$HOME/.ssh/authorized_keys\"; if [ -f \"$f\" ]; then grep -vxF {q} \"$f\" > \"$f.hopper\"; cat \"$f.hopper\" > \"$f\"; rm -f \"$f.hopper\"; fi",
                q = sh_quote(l.public_key.trim())
            );
            let _ = c.exec(&cmd, None, SHORT).await?;
        }
        c.ssh.close().await;
        app.vps_setup.save_login(Login::default())?;
        Ok(("done", if l.key_added { "key taken off the VPS, login forgotten".into() } else { "forgotten here".into() }))
    }
    .await);
    Ok(())
}

// ---- runs as jobs ------------------------------------------------------------------------

fn start_run(app: &Arc<App>, kind: &'static str, steps: Vec<Step>, server: String, args: Option<InstallArgs>) -> Result<(), String> {
    {
        let mut r = app.vps_setup.run.lock().unwrap();
        if r.as_ref().is_some_and(|r| r.running) {
            return Err("The setup is already running.".into());
        }
        *r = Some(Run { kind, running: true, steps, server, ..Default::default() });
    }
    // In the panel's task list too (only the step's name, no address).
    let job = app.jobs.start(if kind == "install" { "Setting up the relay" } else { "Removing the relay" });
    let a = app.clone();
    tokio::spawn(async move {
        let total = a.vps_setup.run.lock().unwrap().as_ref().map_or(1, |r| r.steps.len());
        let progress = {
            let (a, job) = (a.clone(), job.clone());
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    let Some(r) = a.vps_setup.run.lock().unwrap().clone() else { return };
                    let done = r.steps.iter().filter(|s| !matches!(s.state, "wait" | "run")).count();
                    if let Some(s) = r.steps.iter().find(|s| s.state == "run") {
                        job.detail(s.title.clone());
                    }
                    job.progress(done, total);
                    if !r.running {
                        return;
                    }
                }
            })
        };
        let r = match (kind, args) {
            ("install", Some(args)) => install(&a, &args).await,
            _ => remove(&a).await,
        };
        a.vps_setup.with_run(|run| {
            run.running = false;
            run.finished = Some(unix());
            if let Err(e) = &r {
                run.error = e.clone();
                for s in run.steps.iter_mut().filter(|s| s.state == "wait") {
                    s.state = "skip";
                }
            }
        });
        match &r {
            Ok(()) => log::info!("relay setup: {kind} finished"),
            Err(e) => log::warn!("relay setup: {kind} stopped: {}", hopper_core::applog::redact(e)),
        }
        let _ = progress.await;
        job.finish(&r.map_err(|e| anyhow::anyhow!(e)));
    });
    Ok(())
}

// ---- API -------------------------------------------------------------------------------

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/vps-relay/setup", get(get_setup))
        .route("/api/v1/vps-relay/setup/new-key", post(new_key))
        .route("/api/v1/vps-relay/setup/connect", post(connect))
        .route("/api/v1/vps-relay/setup/check", post(check))
        .route("/api/v1/vps-relay/setup/install", post(start_install))
        .route("/api/v1/vps-relay/setup/remove", post(start_remove))
        .route("/api/v1/vps-relay/setup/forget", post(forget))
}

fn login_json(l: &Login) -> Value {
    if l.host.is_empty() {
        return Value::Null;
    }
    // Never the private key.
    json!({ "host": l.host, "port": l.port, "user": l.user, "host_key": l.host_key, "public_key": l.public_key, "root": l.root, "key_added": l.key_added })
}

fn setup_json(app: &App) -> Value {
    let l = app.vps_setup.login();
    let cfg = app.vps.config();
    json!({
        "login": login_json(&l),
        "new_public": l.new_public,
        "check": app.vps_setup.check.lock().unwrap().clone(),
        "run": app.vps_setup.run.lock().unwrap().clone(),
        "version": version(),
        "source": if test_binary().is_some() { "file" } else { "release" },
        "ports": { "game": GAME_PORT, "tunnel": TUNNEL_PORT },
        "paired": { "address": cfg.address, "fingerprint": cfg.fingerprint, "servers": cfg.servers, "enabled": cfg.enabled, "public_address": cfg.public_address },
        "status": app.vps.status(),
    })
}

/// Owner (and Manager) only: it signs in to another machine with sudo.
async fn get_setup(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    Ok(Json(setup_json(&app)))
}

async fn new_key(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let mut l = app.vps_setup.login();
    if l.new_public.is_empty() {
        let (private, public) = ssh::new_key().map_err(bad)?;
        l.new_private = private;
        l.new_public = public;
        app.vps_setup.save_login(l.clone()).map_err(bad)?;
    }
    Ok(Json(json!({ "public_key": l.new_public })))
}

#[derive(Deserialize)]
struct ConnectBody {
    host: String,
    port: Option<u16>,
    user: String,
    /// "key" (pasted or uploaded), "new" (the one `new-key` made), "password".
    mode: String,
    #[serde(default)]
    key: String,
    #[serde(default)]
    passphrase: String,
    #[serde(default)]
    password: String,
    /// Forget the saved host key (the VPS was reinstalled).
    #[serde(default)]
    trust_new: bool,
}

async fn connect(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<ConnectBody>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    if app.vps_setup.busy() {
        return Err(bad("The setup is running. Wait for it to finish."));
    }
    let host = plan::valid_host(&b.host).map_err(bad)?;
    let user = plan::valid_user(&b.user).map_err(bad)?;
    let port = b.port.unwrap_or(22);
    if port == 0 {
        return Err(bad("The ssh port is a number from 1 to 65535."));
    }
    let saved = app.vps_setup.login();
    let pinned = if !b.trust_new && saved.host == host && saved.port == port { saved.host_key.clone() } else { String::new() };
    let (private, public, added) = match b.mode.as_str() {
        "key" => {
            let (p, q) = ssh::read_key(&b.key, &b.passphrase).map_err(bad)?;
            (p, q, false)
        }
        "new" => {
            if saved.new_private.is_empty() {
                return Err(bad("Make the new key first."));
            }
            (saved.new_private.clone(), saved.new_public.clone(), false)
        }
        "password" => {
            if b.password.is_empty() {
                return Err(bad("Type the password."));
            }
            // Signs in once with the password and puts a new key on the
            // VPS; the password goes no further than this.
            let s = Ssh::connect(&host, port, &user, Auth::Password(b.password), &pinned).await.map_err(bad)?;
            let (p, q) = ssh::new_key().map_err(bad)?;
            let qq = sh_quote(q.trim());
            let cmd = format!("umask 077 && mkdir -p \"$HOME/.ssh\" && touch \"$HOME/.ssh/authorized_keys\" && (grep -qxF {qq} \"$HOME/.ssh/authorized_keys\" || printf '%s\\n' {qq} >> \"$HOME/.ssh/authorized_keys\")");
            let o = s.run(&cmd, None, SHORT).await.map_err(bad)?;
            s.close().await;
            if !o.ok() {
                return Err(bad(format!("Putting a key on the VPS failed: {}", o.why())));
            }
            (p, q, true)
        }
        _ => return Err(bad("Pick how to sign in.")),
    };
    let pin = pinned.clone();
    let s = Ssh::connect(&host, port, &user, Auth::Key(ssh::decode(&private).map_err(bad)?), &pin).await.map_err(|e| {
        if b.mode == "new" && e.contains("refused") { bad("The VPS doesn't take the new key yet. Add the public key below to the VPS (your cloud's ssh keys page, or ~/.ssh/authorized_keys), then connect again.") } else { bad(e) }
    })?;
    let root = s.run("id -u", None, SHORT).await.map(|o| o.out.trim() == "0").unwrap_or(false);
    let host_key = s.host_key.clone();
    s.close().await;
    let key_added = added || (saved.key_added && saved.public_key == public && saved.host == host);
    let new_login = Login { host, port, user, host_key, private_key: private, public_key: public, key_added, root, ..Default::default() };
    app.vps_setup.save_login(new_login.clone()).map_err(bad)?;
    *app.vps_setup.check.lock().unwrap() = None;
    log::info!("relay setup: signed in to the VPS (host key {})", new_login.host_key);
    Ok(Json(json!({ "login": login_json(&new_login) })))
}

#[derive(Deserialize, Default)]
struct PortsBody {
    game_port: Option<u16>,
    tunnel_port: Option<u16>,
}

fn ports_of(game: Option<u16>, tunnel: Option<u16>) -> ApiResult<(u16, u16)> {
    let g = plan::valid_port(game.unwrap_or(GAME_PORT)).map_err(bad)?;
    let t = plan::valid_port(tunnel.unwrap_or(TUNNEL_PORT)).map_err(bad)?;
    if g == t {
        return Err(bad("The two ports must differ."));
    }
    Ok((g, t))
}

async fn check(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, body: Option<Json<PortsBody>>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    if app.vps_setup.busy() {
        return Err(bad("The setup is running. Wait for it to finish."));
    }
    let b = body.map(|b| b.0).unwrap_or_default();
    let ports = ports_of(b.game_port, b.tunnel_port)?;
    let r = tokio::time::timeout(Duration::from_secs(90), run_check(&app, ports)).await.map_err(|_| bad("The VPS took too long to answer."))?.map_err(bad)?;
    Ok(Json(r))
}

#[derive(Deserialize)]
struct InstallBody {
    server: String,
    game_port: Option<u16>,
    tunnel_port: Option<u16>,
    public_address: Option<String>,
}

async fn start_install(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<InstallBody>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    app.servers.get(&b.server).ok_or_else(|| not_found("server"))?;
    let ports = ports_of(b.game_port, b.tunnel_port)?;
    let public_address = match b.public_address.map(|p| p.trim().to_string()) {
        Some(p) if p.chars().count() > 255 || p.chars().any(|ch| ch.is_whitespace() || ch.is_control()) => return Err(bad("The players' address is one name or IP, with a port if it isn't 25565.")),
        p => p,
    };
    if app.vps_setup.login().host.is_empty() {
        return Err(bad("Connect to the VPS first."));
    }
    let steps = plan::install_steps(version(), ports);
    start_run(&app, "install", steps, b.server.clone(), Some(InstallArgs { server: b.server, ports, public_address })).map_err(bad)?;
    Ok(Json(setup_json(&app)))
}

async fn start_remove(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    if app.vps_setup.login().host.is_empty() {
        return Err(bad("There's no VPS login to remove the relay with. Use Forget instead."));
    }
    // Its account checks would go with it, and the server is set not to do its own.
    if let Some(id) = app.vps_forward.first_changed() {
        let name = app.servers.get(&id).map_or(id, |e| e.name.clone());
        return Err(bad(format!("{name} checks accounts through the relay. Stop using the relay on it first.")));
    }
    start_run(&app, "remove", plan::remove_steps(), String::new(), None).map_err(bad)?;
    Ok(Json(setup_json(&app)))
}

/// The VPS is gone: forget it here without connecting.
async fn forget(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    if app.vps_setup.busy() {
        return Err(bad("The setup is running. Wait for it to finish."));
    }
    app.vps.set_config(HomeConfig::default()).map_err(|e| bad(format!("{e:#}")))?;
    app.vps_setup.save_login(Login::default()).map_err(bad)?;
    *app.vps_setup.check.lock().unwrap() = None;
    *app.vps_setup.run.lock().unwrap() = None;
    log::info!("relay setup: forgot the VPS");
    Ok(Json(setup_json(&app)))
}
