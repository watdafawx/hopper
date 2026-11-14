//! The node's side of node mode (`design/multi-panel.md`, A1): joining a
//! main (`hopperd node join`), the connection the running panel keeps open
//! to it, and what `hopperd node status` and Settings › This node show.
//!
//! What joining leaves in the panel's start-up folder:
//! - `node.json`: the main's address, its CA (the pin), this node's id and
//!   its node token. Readable by the panel's user only.
//! - `node-status.json`: the connection's last state, written by the
//!   running panel, so `hopperd node status` in another terminal can say
//!   which step failed.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use axum::extract::State;
use axum::http::{HeaderMap, Method};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::dial::{self, ConnectError, Pin, Target};
use super::frame::code;
use super::mux::{self, Config, MuxError, Side};
use super::ws;
use crate::App;

pub const FILE: &str = "node.json";
pub const STATUS_FILE: &str = "node-status.json";

/// What joining a main leaves on the node.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Joined {
    pub main_url: String,
    #[serde(default)]
    pub main_name: String,
    /// The main's CA: the only certificate authority this node trusts for it.
    pub ca_pem: String,
    pub fingerprint: String,
    pub node_id: String,
    pub token: String,
    /// look, operate or manage: what the main may do here (A2 enforces it).
    pub ceiling: String,
    pub joined_at: u64,
    /// "key" (a join command) or "code" (compared 6-digit codes).
    pub via: String,
}

pub fn load(dir: &Path) -> Option<Joined> {
    serde_json::from_slice(&std::fs::read(dir.join(FILE)).ok()?).ok()
}

fn save(dir: &Path, j: &Joined) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = dir.join(format!("{FILE}.tmp"));
    let _ = std::fs::remove_file(&tmp);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    // The node token is the node's identity on the main: the panel's user only.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    use std::io::Write as _;
    let mut f = o.open(&tmp).with_context(|| format!("writing {}", tmp.display()))?;
    f.write_all(&serde_json::to_vec_pretty(j)?)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, dir.join(FILE))?;
    Ok(())
}

/// Forgets the main: the token and the pin. Works offline.
fn forget(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(FILE));
    let _ = std::fs::remove_file(dir.join(STATUS_FILE));
    *STATUS.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

// ---- connection state ----

/// The connection as the node sees it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Status {
    /// connecting, connected, reconnecting or stopped.
    pub state: String,
    /// The step the last attempt reached or failed at: dns, tcp,
    /// certificate, key, version, hello, connected.
    pub step: String,
    /// What to tell the person, in plain words.
    pub message: String,
    /// When `state` began (unix seconds).
    pub since: u64,
    /// When the node last had a working connection.
    pub last_ok: u64,
    pub rtt_ms: Option<u64>,
    /// When the next try is due, while reconnecting.
    pub next_try: u64,
    /// When this was written; old means the panel isn't running.
    pub updated: u64,
}

static STATUS: Mutex<Option<Status>> = Mutex::new(None);

fn now() -> u64 {
    crate::security::now()
}

/// Changes the status and writes it for `hopperd node status`.
fn set_status(dir: &Path, f: impl FnOnce(&mut Status)) {
    let mut g = STATUS.lock().unwrap_or_else(|e| e.into_inner());
    let s = g.get_or_insert_with(Status::default);
    let before = s.state.clone();
    f(s);
    if s.state != before {
        s.since = now();
    }
    s.updated = now();
    let body = serde_json::to_vec_pretty(&*s).unwrap_or_default();
    drop(g);
    let _ = std::fs::write(dir.join(STATUS_FILE), body);
}

pub fn status_now() -> Option<Status> {
    STATUS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

// ---- the running panel's connection ----

/// Keeps this panel connected to its main while `node.json` is there:
/// starts when a join writes it, stops when `leave` removes it.
pub fn start(app: Arc<App>) {
    adopt_left_behind(&app.boot_dir, &app.data_dir);
    let dir: PathBuf = app.data_dir.clone();
    tokio::spawn(async move {
        loop {
            match load(&dir) {
                Some(j) => {
                    run(&app, &dir, &j).await;
                    // Stopped (refused, removed): wait for a new join or a leave.
                    while load(&dir).is_some_and(|n| n.token == j.token) {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
                None => tokio::time::sleep(Duration::from_secs(2)).await,
            }
        }
    });
}

/// Whether `node.json` still holds this join.
fn still_joined(dir: &Path, j: &Joined) -> bool {
    load(dir).is_some_and(|n| n.token == j.token && n.main_url == j.main_url)
}

/// What the main may do here, as `node.json` says it now: read for every
/// stream, so lowering the ceiling on the node applies to the open
/// connection. Read strictly: unreadable, unknown, or another join refuses.
pub(crate) fn ceiling_now(dir: &Path, j: &Joined) -> crate::relay::CeilingNow {
    let (dir, token, url) = (dir.to_path_buf(), j.token.clone(), j.main_url.clone());
    std::sync::Arc::new(move || load(&dir).filter(|n| n.token == token && n.main_url == url).and_then(|n| crate::relay::ceiling_role(&n.ceiling)))
}

fn rand_ms(max: u16) -> Duration {
    let mut b = [0u8; 2];
    let _ = getrandom::fill(&mut b);
    Duration::from_millis(u64::from(u16::from_le_bytes(b) % max))
}

const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// What the node's tunnel config says about itself.
fn tunnel_config() -> Config {
    // The version it says it runs: HOPPERD_TEST_VERSION makes a scratch
    // node look older, to try "Update" from its main.
    Config { capabilities: crate::api::CAPABILITIES.iter().map(|c| c.to_string()).collect(), version: crate::autoupdate::current(), ..Config::default() }
}

/// Connects, and reconnects with backoff, until the main refuses this node
/// or `node.json` changes. Returns when it stops.
async fn run(app: &Arc<App>, dir: &Path, j: &Joined) {
    let Some(ca) = dial::from_pem(&j.ca_pem) else {
        set_status(dir, |s| {
            s.state = "stopped".into();
            s.step = "certificate".into();
            s.message = "The main's certificate in node.json can't be read. Join again (hopperd node join).".into();
        });
        return;
    };
    let target = match Target::parse(&j.main_url) {
        Ok(t) => t,
        Err(e) => {
            set_status(dir, |s| {
                s.state = "stopped".into();
                s.step = "address".into();
                s.message = e.to_string();
            });
            return;
        }
    };
    let pin = Pin::Ca(ca);
    log::info!("node: connecting to the main at {}", j.main_url);
    let mut backoff = Duration::from_secs(1);
    let mut ever = false;
    loop {
        if !still_joined(dir, j) {
            return;
        }
        set_status(dir, |s| {
            if s.state != "reconnecting" {
                s.state = if ever { "reconnecting" } else { "connecting" }.into();
            }
            s.next_try = 0;
        });
        let mut wait = backoff;
        match ws::connect(&target, &pin, &j.token).await {
            Ok((tx, rx)) => match mux::start(Side::Node, tunnel_config(), tx, rx).await {
                Ok(t) => {
                    ever = true;
                    log::info!("node: connected to the main (hopperd {})", t.peer.version);
                    set_status(dir, |s| {
                        s.state = "connected".into();
                        s.step = "connected".into();
                        s.message = format!("Connected to {}.", if j.main_name.is_empty() { &j.main_url } else { &j.main_name });
                        s.last_ok = now();
                        s.rtt_ms = None;
                    });
                    let at = Instant::now();
                    let why = session(app, dir, j, t).await;
                    // Start the backoff again only after a connection that
                    // lasted: a main that accepts and drops at once would
                    // otherwise get every node back each second (A0 review 8).
                    if at.elapsed() >= Duration::from_secs(60) {
                        backoff = Duration::from_secs(1);
                        wait = backoff;
                    }
                    match why {
                        Ended::Left => return,
                        Ended::Mux(MuxError::GoAway(code::BYE, reason)) if reason == "removed" => {
                            log::warn!("node: the main removed this node");
                            set_status(dir, |s| {
                                s.state = "stopped".into();
                                s.step = "key".into();
                                s.message = "The main removed this node. To connect again, join with a new command from the main.".into();
                            });
                            return;
                        }
                        Ended::Mux(e) => {
                            log::warn!("node: connection to the main ended: {e}");
                            set_status(dir, |s| {
                                s.state = "reconnecting".into();
                                s.step = "connected".into();
                                s.message = format!("Lost the connection ({}). Reconnecting.", plain_mux(&e));
                                s.last_ok = now();
                            });
                        }
                    }
                }
                Err(MuxError::Version { .. }) => {
                    set_status(dir, |s| {
                        s.state = "reconnecting".into();
                        s.step = "version".into();
                        s.message = "This node and the main are too far apart in version. Update the older one.".into();
                    });
                    wait = MAX_BACKOFF;
                }
                Err(e) => {
                    set_status(dir, |s| {
                        s.state = "reconnecting".into();
                        s.step = "hello".into();
                        s.message = format!("The main accepted the key but the connection failed: {}.", plain_mux(&e));
                    });
                }
            },
            Err(ConnectError::Refused(code)) => {
                // A revoked key stays revoked: hammering the main would only
                // trip the limit for every node behind this address.
                log::warn!("node: the main refused this node's key (HTTP {code}); stopped");
                set_status(dir, |s| {
                    s.state = "stopped".into();
                    s.step = "key".into();
                    s.message = "The main refused this node's key: it was removed there. To connect again, join with a new command from the main.".into();
                });
                return;
            }
            Err(e) => {
                let step = e.step();
                let mut message = format!("Can't reach the main: {e}. Trying again.");
                if let ConnectError::Certificate(why) = &e {
                    // Only a new pairing changes the pin; trying every second
                    // won't help, so try slowly in case it was a proxy blip.
                    message = format!(
                        "The main's certificate doesn't match the one this node trusts. If the main was reinstalled or its certificate was reset on purpose, run hopperd node leave, then join again. Otherwise something between here and the main is answering for it. ({why})"
                    );
                    wait = MAX_BACKOFF;
                }
                log::warn!("node: connecting to the main: {e}");
                set_status(dir, |s| {
                    s.state = if ever { "reconnecting" } else { "connecting" }.into();
                    s.step = step.into();
                    s.message = message;
                });
            }
        }
        let pause = wait + rand_ms(1000);
        set_status(dir, |s| s.next_try = now() + pause.as_secs());
        let until = Instant::now() + pause;
        while Instant::now() < until {
            if !still_joined(dir, j) {
                return;
            }
            tokio::time::sleep((until - Instant::now()).min(Duration::from_secs(2))).await;
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

fn plain_mux(e: &MuxError) -> String {
    match e {
        MuxError::Timeout => "the main stopped answering".into(),
        MuxError::Closed => "the main closed it".into(),
        MuxError::GoAway(_, r) if !r.is_empty() => format!("the main said: {r}"),
        MuxError::Io(_) => "the connection dropped".into(),
        other => other.to_string(),
    }
}

enum Ended {
    /// `node.json` went away or changed: this node left.
    Left,
    Mux(MuxError),
}

async fn session(app: &Arc<App>, dir: &Path, j: &Joined, mut t: mux::Tunnel) -> Ended {
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    let mut n = 0u32;
    // The main's requests (A2). The ceiling is read strictly from
    // node.json for every stream: anything but look/operate/manage refuses.
    let incoming = std::mem::replace(&mut t.incoming, tokio::sync::mpsc::channel(1).1);
    let _serving = crate::nodes::Follow(tokio::spawn(crate::relay::serve(app.clone(), ceiling_now(dir, j), j.main_name.clone(), incoming)));
    loop {
        tokio::select! {
            r = &mut t.done => return Ended::Mux(r.unwrap_or(MuxError::Closed)),
            _ = tick.tick() => {
                if !still_joined(dir, j) {
                    t.handle.close("left").await;
                    let _ = tokio::time::timeout(Duration::from_secs(3), &mut t.done).await;
                    return Ended::Left;
                }
                n += 1;
                if n % 8 == 0 {
                    let rtt = t.handle.rtt().map(|d| d.as_millis() as u64);
                    set_status(dir, |s| {
                        s.rtt_ms = rtt;
                        s.last_ok = now();
                    });
                }
            }
        }
    }
}

// ---- hopperd node join|status|leave ----

const USAGE: &str = "Usage:
  hopperd node join https://main:8790 [--key <join key> --ca <fingerprint>] [--name <name>] [--data <folder>] [--service]
  hopperd node status [--data <folder>]
  hopperd node leave [--data <folder>]
  hopperd node ceiling [look|operate] [--data <folder>]

ceiling shows what the main may do on this node's servers; with a level, it lowers it (raising it means joining again at the higher level).

Without --key, join shows a 6-digit code: check the main shows the same one in Settings › Nodes and allow it there.";

struct Opts {
    url: Option<String>,
    key: Option<String>,
    ca: Option<String>,
    name: Option<String>,
    data: Option<PathBuf>,
    service: bool,
}

fn opts(args: &[String]) -> anyhow::Result<Opts> {
    let mut o = Opts { url: None, key: None, ca: None, name: None, data: None, service: false };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut v = || it.next().cloned().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--key" => o.key = Some(v()?),
            "--ca" => o.ca = Some(v()?),
            "--name" => o.name = Some(v()?),
            "--data" => o.data = Some(v()?.into()),
            "--service" => o.service = true,
            "--help" | "-h" => bail!("{USAGE}"),
            s if !s.starts_with('-') && o.url.is_none() => o.url = Some(s.to_string()),
            other => bail!("Unknown option {other}.\n\n{USAGE}"),
        }
    }
    Ok(o)
}

pub fn command(args: &[String]) -> anyhow::Result<()> {
    let (what, rest) = args.split_first().map(|(w, r)| (w.as_str(), r)).unwrap_or(("", &[]));
    let o = opts(rest)?;
    let boot = crate::absolute(&o.data.clone().unwrap_or_else(crate::service::default_data));
    // node.json lives with the rest of the panel's data: in its panel
    // folder once it has one (`--home`), else in the data folder itself.
    let dir = crate::service::real_data(&boot).0;
    // As the running panel does, so `leave` and `ceiling` act on the file
    // the panel will read, not report "isn't a node" and change nothing.
    adopt_left_behind(&boot, &dir);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    match what {
        "join" => rt.block_on(join(&boot, &dir, o)),
        "status" => rt.block_on(status(&dir)),
        "leave" => rt.block_on(leave(&dir)).map(|m| println!("{m}")),
        "ceiling" => ceiling(&dir, o.url.as_deref()).map(|m| println!("{m}")),
        _ => bail!("{USAGE}"),
    }
}

fn say(step: &str, ok: bool, text: &str) {
    let text = if text.is_empty() { String::new() } else { format!(": {text}") };
    println!("  [{}] {step}{text}", if ok { "ok" } else { "x " });
}

/// A failed step, printed with the ones before it marked done.
fn print_failure(e: &ConnectError) {
    let order = ["dns", "tcp", "certificate", "key"];
    let names = ["Address found", "Port open", "Certificate matches", "Key accepted"];
    let at = order.iter().position(|s| *s == e.step()).unwrap_or(order.len());
    for n in &names[..at.min(names.len())] {
        say(n, true, "");
    }
    say(names.get(at).copied().unwrap_or("Connection"), false, &e.to_string());
}

/// What this node lets its main do, or (with a level) lowers it. Only
/// lowers: a higher level is the main's and the node's agreement at join
/// time, so it takes joining again. A running panel reads the new level
/// for the next request (`ceiling_now`).
pub fn ceiling(dir: &Path, to: Option<&str>) -> anyhow::Result<String> {
    let Some(mut j) = load(dir) else { return Ok("This server isn't a node of any main panel.".into()) };
    let main = if j.main_name.is_empty() { j.main_url.clone() } else { j.main_name.clone() };
    let Some(now_role) = crate::relay::ceiling_role(&j.ceiling) else {
        bail!("node.json has a level this version doesn't know ({}). Leave and join again.", plain(&j.ceiling));
    };
    let Some(to) = to else {
        return Ok(format!(
            "{main} may {} here ({}).\nLower it with: hopperd node ceiling look   (or operate). To allow more, leave and join again at the higher level.",
            crate::relay::ceiling_text(now_role),
            j.ceiling
        ));
    };
    let Some(to_role) = crate::relay::ceiling_role(to).filter(|_| matches!(to, "look" | "operate" | "manage")) else {
        bail!("The level is look, operate or manage.");
    };
    if crate::relay::cap(to_role, now_role) != to_role {
        bail!("{main} may {} here now. A higher level means joining again: hopperd node leave, then the main's join command at that level.", crate::relay::ceiling_text(now_role));
    }
    j.ceiling = to.to_string();
    save(dir, &j)?;
    Ok(format!("{main} may {} here now ({to}). The main still shows its own level until you lower it there too (Settings › Nodes).", crate::relay::ceiling_text(to_role)))
}

/// A `node.json` left in the start-up folder (`--data`) when the panel's
/// data lives in its panel folder (`--home`): joined with an older
/// version, or before the panel folder was set. It moves in with the rest.
pub fn adopt_left_behind(boot: &Path, data: &Path) {
    if crate::home::inside(boot, data) && crate::home::inside(data, boot) {
        return;
    }
    if boot.join(FILE).is_file() && !data.join(FILE).is_file() {
        // A rename fails across drives; then write it anew (keeping the
        // owner-only mode `save` gives it) and remove the old one.
        let moved = std::fs::rename(boot.join(FILE), data.join(FILE)).or_else(|e| match load(boot) {
            Some(j) => save(data, &j).and_then(|()| Ok(std::fs::remove_file(boot.join(FILE))?)),
            None => Err(e.into()),
        });
        match moved {
            Ok(()) => {
                let _ = std::fs::remove_file(boot.join(STATUS_FILE));
                log::info!("node: moved node.json into {}", data.display());
            }
            Err(e) => log::warn!("node: couldn't move node.json into {}: {e:#}", data.display()),
        }
    }
}

async fn join(boot: &Path, dir: &Path, o: Opts) -> anyhow::Result<()> {
    let url = o.url.clone().with_context(|| format!("Which main? {USAGE}"))?;
    let target = Target::parse(&url).map_err(|e| anyhow::anyhow!("{e}"))?;
    if o.key.is_some() && o.ca.is_none() {
        bail!("A join key needs the main's fingerprint too (--ca): copy the whole command from the main.");
    }
    if let Some(old) = load(dir) {
        bail!(
            "This server is already a node of {} ({}). To join another main, run hopperd node leave first.",
            if old.main_name.is_empty() { "a main" } else { &old.main_name },
            old.main_url
        );
    }
    let name = o.name.clone().unwrap_or_else(crate::config::hostname);
    println!("Joining {} as \"{name}\"", target.url());
    let pin = match &o.ca {
        Some(fp) => Pin::Fingerprint(fp.clone()),
        None => Pin::Any,
    };
    let version = env!("CARGO_PKG_VERSION");
    let joined = if let Some(key) = &o.key {
        let body = json!({ "key": key, "name": name, "version": version });
        let (status, v, ca) = match dial::request(&target, &pin, "POST", "/api/v1/node/join", None, Some(&body)).await {
            Ok(r) => r,
            Err(e) => {
                print_failure(&e);
                bail!("Couldn't join.");
            }
        };
        say("Certificate matches", true, &crate::tls::fingerprint(&ca));
        match status {
            200 => {}
            401 | 403 => {
                say("Join key accepted", false, "refused");
                bail!("The main refused the join key: it was used already, expired (keys last 30 minutes) or was removed. Make a new join command on the main: Settings › Nodes › Make a join command.");
            }
            429 => bail!("The main is refusing this address for a while (too many wrong keys). Try again in 10 minutes."),
            s => bail!("The main answered {s}: {}", plain(v["error"].as_str().unwrap_or(""))),
        }
        say("Join key accepted", true, "");
        joined_from(&url, &ca, &v, "key")?
    } else {
        // The nonce goes out only as its hash, and is revealed after the main
        // has picked the id and secret (see `nodes::node_code`).
        let nonce = crate::auth::random_hex(32);
        let body = json!({ "name": name, "version": version, "commit": crate::auth::hash_token(&nonce) });
        let (status, v, ca) = match dial::request(&target, &pin, "POST", "/api/v1/node/request", None, Some(&body)).await {
            Ok(r) => r,
            Err(e) => {
                print_failure(&e);
                bail!("Couldn't ask to join.");
            }
        };
        if status != 200 {
            bail!("The main answered {status}: {}", plain(v["error"].as_str().unwrap_or("it doesn't take nodes (update it first?)")));
        }
        let (id, secret) = (v["id"].as_str().unwrap_or_default().to_string(), v["secret"].as_str().unwrap_or_default().to_string());
        let fp = crate::tls::fingerprint(&ca);
        let pin = Pin::Ca(ca.clone());
        let ask = |id: &str, secret: &str| json!({ "id": id, "secret": secret, "nonce": nonce });
        // The reveal: only now does the main show the code.
        match dial::request(&target, &pin, "POST", "/api/v1/node/request/status", None, Some(&ask(&id, &secret))).await {
            Ok((200, v, _)) if v["state"] == "waiting" => {}
            Ok((s, v, _)) => bail!("The main answered {s}: {}", plain(v["error"].as_str().or(v["state"].as_str()).unwrap_or(""))),
            Err(e) => {
                print_failure(&e);
                bail!("Couldn't ask to join.");
            }
        }
        let code = crate::nodes::node_code(&fp, &id, &secret, &nonce);
        println!();
        println!("  On the main, open Settings › Nodes. It should show this code:");
        println!();
        println!("      {} {}", &code[..3], &code[3..]);
        println!();
        println!("  Allow it there only if the codes match. Main's certificate: {fp}");
        println!("  Waiting (up to 5 minutes)...");
        let started = Instant::now();
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if started.elapsed() > Duration::from_secs(5 * 60 + 10) {
                bail!("Nobody allowed it within 5 minutes. Run the command again for a new code.");
            }
            let v = match dial::request(&target, &pin, "POST", "/api/v1/node/request/status", None, Some(&ask(&id, &secret))).await {
                Ok((200, v, _)) => v,
                Ok((s, v, _)) => bail!("The main answered {s}: {}", plain(v["error"].as_str().unwrap_or(""))),
                // A blip while waiting: keep waiting.
                Err(e) => {
                    log::debug!("waiting: {e}");
                    continue;
                }
            };
            match v["state"].as_str() {
                Some("allowed") => break joined_from(&url, &ca, &v, "code")?,
                Some("denied") => bail!("The main refused this node."),
                Some("waiting") => {}
                _ => bail!("The request ran out. Run the command again for a new code."),
            }
        }
    };
    save(dir, &joined)?;
    println!();
    println!("Joined {} as node \"{name}\" (it may: {}).", if joined.main_name.is_empty() { &joined.main_url } else { &joined.main_name }, joined.ceiling);
    println!("Saved in {}", dir.join(FILE).display());
    if o.service {
        let mut args = vec!["install".to_string(), "--data".into(), boot.display().to_string()];
        if cfg!(windows) {
            args.push("--copy".into());
        }
        crate::service::command(&args)?;
        println!("The service connects to the main by itself. Check with: hopperd node status");
    } else {
        println!("Hopper Server connects when it runs with this data folder (a running one picks it up within seconds).");
        println!("Check with: hopperd node status");
    }
    Ok(())
}

/// Text from the main, made safe to print in a terminal (often a root one):
/// no control characters, so no escape sequences that could redraw the
/// screen, and short.
fn plain(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(300).collect()
}

fn joined_from(url: &str, ca: &rustls::pki_types::CertificateDer<'_>, v: &Value, via: &str) -> anyhow::Result<Joined> {
    let s = |k: &str| plain(v[k].as_str().unwrap_or_default());
    let (node_id, token) = (s("node_id"), s("token"));
    // Both end up in file names, headers and A2's checks: plain ASCII only.
    let tidy = |x: &str| !x.is_empty() && x.len() <= 128 && x.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !tidy(&node_id) || !tidy(&token) {
        bail!("The main's answer had no node key.");
    }
    let ceiling = s("ceiling");
    if !matches!(ceiling.as_str(), "look" | "operate" | "manage") {
        bail!("The main's answer didn't say what it may do here.");
    }
    let target = Target::parse(url).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(Joined {
        main_url: target.url(),
        main_name: s("main_name").chars().take(100).collect(),
        ca_pem: dial::to_pem(ca),
        fingerprint: crate::tls::fingerprint(ca),
        node_id,
        token,
        ceiling,
        joined_at: now(),
        via: via.into(),
    })
}

fn ago(t: u64) -> String {
    let d = now().saturating_sub(t);
    match d {
        0..60 => format!("{d} s ago"),
        60..3600 => format!("{} min ago", d / 60),
        3600..86400 => format!("{} h ago", d / 3600),
        _ => format!("{} days ago", d / 86400),
    }
}

async fn status(dir: &Path) -> anyhow::Result<()> {
    let Some(j) = load(dir) else {
        println!("This server isn't a node of any main panel.");
        println!("Join one with: hopperd node join https://main:8790   (or paste the join command from the main)");
        return Ok(());
    };
    println!("Main:        {}{}", if j.main_name.is_empty() { String::new() } else { format!("{} ", j.main_name) }, j.main_url);
    println!("Certificate: {}", j.fingerprint);
    println!("Node id:     {}   may: {}   joined {}", j.node_id, j.ceiling, ago(j.joined_at));
    let live: Option<Status> = std::fs::read(dir.join(STATUS_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok());
    // The running panel rewrites it at least every 16 s while connected,
    // and on every try while not.
    match live.filter(|s| now().saturating_sub(s.updated) <= 75) {
        Some(s) => {
            println!();
            let state = match s.state.as_str() {
                "connected" => "Connected",
                "connecting" => "Connecting",
                "reconnecting" => "Reconnecting",
                "stopped" => "Stopped",
                other => other,
            };
            println!("{state} (since {})", ago(s.since));
            println!("  {}", s.message);
            if let Some(r) = s.rtt_ms {
                println!("  Round trip: {r} ms");
            }
            if s.state != "connected" && s.last_ok > 0 {
                println!("  Last connected {}", ago(s.last_ok));
            }
            if s.next_try > now() {
                println!("  Next try in {} s", s.next_try - now());
            }
        }
        None => {
            println!();
            println!("Hopper Server isn't running with this data folder ({}). Checking the connection now:", dir.display());
            let target = Target::parse(&j.main_url).map_err(|e| anyhow::anyhow!("{e}"))?;
            let ca = dial::from_pem(&j.ca_pem).context("the main's certificate in node.json can't be read")?;
            match dial::request(&target, &Pin::Ca(ca), "GET", "/api/v1/node/check", Some(&j.token), None).await {
                Ok((200, _, _)) => {
                    for n in ["Address found", "Port open", "Certificate matches", "Key accepted"] {
                        say(n, true, "");
                    }
                    println!("Start Hopper Server here and it connects.");
                }
                Ok((401 | 403, _, _)) => print_failure(&ConnectError::Refused(401)),
                Ok((429, _, _)) => println!("The main is refusing this address for a while (too many wrong keys)."),
                Ok((s, v, _)) => println!("The main answered {s}: {}", plain(v["error"].as_str().unwrap_or(""))),
                Err(e) => print_failure(&e),
            }
        }
    }
    Ok(())
}

/// Tells the main (best effort) and forgets it here either way.
async fn leave(dir: &Path) -> anyhow::Result<String> {
    let Some(j) = load(dir) else { return Ok("This server isn't a node of any main panel.".into()) };
    let told = async {
        let target = Target::parse(&j.main_url).ok()?;
        let pin = Pin::Ca(dial::from_pem(&j.ca_pem)?);
        Some(tokio::time::timeout(Duration::from_secs(8), dial::request(&target, &pin, "POST", "/api/v1/node/leave", Some(&j.token), None)).await)
    }
    .await;
    forget(dir);
    let main = if j.main_name.is_empty() { j.main_url.clone() } else { j.main_name.clone() };
    Ok(match told {
        Some(Ok(Ok((200, _, _)))) => format!("Left {main}: it no longer lists this node."),
        Some(Ok(Ok((401 | 403, _, _)))) => format!("Left {main} (it had already removed this node)."),
        Some(Ok(Err(e))) => format!("Forgot {main} here, but couldn't tell it ({e}). Remove the node there too: Settings › Nodes."),
        _ => format!("Forgot {main} here, but couldn't tell it. Remove the node there too: Settings › Nodes."),
    })
}

// ---- Settings › This node (the node's own page) ----

pub fn routes() -> Router<Arc<App>> {
    Router::new().route("/api/v1/this-node", get(this_node).delete(this_node_leave))
}

fn only_owner(app: &App, headers: &HeaderMap, method: &Method) -> crate::api::ApiResult<()> {
    let who = crate::api::require(app, headers, method)?;
    if !who.is_owner() {
        return Err(crate::api::ApiError(axum::http::StatusCode::FORBIDDEN, "Only the owner can do that.".into()));
    }
    Ok(())
}

async fn this_node(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> crate::api::ApiResult<Json<Value>> {
    only_owner(&app, &headers, &method)?;
    let Some(j) = load(&app.data_dir) else { return Ok(Json(json!({ "joined": false }))) };
    Ok(Json(json!({
        "joined": true,
        "main_url": j.main_url,
        "main_name": j.main_name,
        "fingerprint": j.fingerprint,
        "ceiling": j.ceiling,
        "joined_at": j.joined_at,
        "status": status_now(),
    })))
}

async fn this_node_leave(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> crate::api::ApiResult<axum::response::Response> {
    only_owner(&app, &headers, &method)?;
    let msg = leave(&app.data_dir).await.map_err(crate::api::bad)?;
    use axum::response::IntoResponse as _;
    let mut res = Json(json!({ "ok": true, "message": msg })).into_response();
    res.extensions_mut().insert(crate::security::AuditNote("left the main panel".into()));
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Security review (low): what the main answers is printed in a root
    /// terminal, and during code pairing nothing about the main is proven
    /// yet; its words can't carry escape sequences, and an answer that
    /// doesn't say what it may do here isn't saved.
    #[test]
    fn answers_from_the_main_are_checked_before_printing_or_saving() {
        assert_eq!(plain("ok\u{1b}[2J\u{1b}[1;1HJoined\r\n"), "ok[2J[1;1HJoined");
        let ca = rustls::pki_types::CertificateDer::from(vec![1u8, 2, 3]);
        let good = json!({ "node_id": "ab12", "token": "hnt_00ff", "ceiling": "look", "main_name": "rig\u{7}" });
        let j = joined_from("https://rig:8790", &ca, &good, "code").unwrap();
        assert_eq!((j.ceiling.as_str(), j.main_name.as_str()), ("look", "rig"));
        for (k, v) in [("ceiling", "admin"), ("ceiling", ""), ("token", "hnt_x\r\nX: y"), ("node_id", "../x")] {
            let mut bad = good.clone();
            bad[k] = json!(v);
            assert!(joined_from("https://rig:8790", &ca, &bad, "code").is_err(), "{k}={v:?}");
        }
    }

    /// Security review (medium): the ceiling is node.json's as it is now,
    /// not as it was when the connection opened; another join, an unknown
    /// ceiling or no file refuses.
    #[test]
    fn the_ceiling_is_read_from_node_json_each_time() {
        let dir = std::env::temp_dir().join(format!("hopperd-ceiling-{}", crate::auth::random_hex(4)));
        let ca = rustls::pki_types::CertificateDer::from(vec![1u8, 2, 3]);
        let mut j = joined_from("https://rig:8790", &ca, &json!({ "node_id": "ab12", "token": "hnt_00ff", "ceiling": "manage" }), "key").unwrap();
        save(&dir, &j).unwrap();
        let now = ceiling_now(&dir, &j);
        assert_eq!(now(), Some(crate::auth::Role::Admin));
        let first = j.clone();
        j.ceiling = "look".into();
        save(&dir, &j).unwrap();
        assert_eq!(now(), Some(crate::auth::Role::Viewer), "lowered on the node: applies at once");
        j.ceiling = "everything".into();
        save(&dir, &j).unwrap();
        assert_eq!(now(), None);
        j.ceiling = "manage".into();
        j.token = "hnt_11ee".into();
        save(&dir, &j).unwrap();
        assert_eq!(ceiling_now(&dir, &first)(), None, "another join");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(now(), None, "left");
    }

    /// A3 leftover: with `--home`, the data moves into the panel folder but
    /// a node.json written before (in the `--data` folder) is moved along,
    /// so the node keeps its join.
    #[test]
    fn a_node_json_left_in_the_start_folder_moves_into_the_data() {
        let root = std::env::temp_dir().join(format!("hopperd-adopt-{}", crate::auth::random_hex(4)));
        let (boot, data) = (root.join("boot"), root.join("home").join(".hopper-server"));
        let ca = rustls::pki_types::CertificateDer::from(vec![1u8, 2, 3]);
        let j = joined_from("https://rig:8790", &ca, &json!({ "node_id": "ab12", "token": "hnt_00ff", "ceiling": "operate" }), "key").unwrap();
        save(&boot, &j).unwrap();
        std::fs::write(boot.join(STATUS_FILE), "{}").unwrap();
        std::fs::create_dir_all(&data).unwrap();
        adopt_left_behind(&boot, &data);
        assert!(!boot.join(FILE).exists() && !boot.join(STATUS_FILE).exists());
        assert_eq!(load(&data).unwrap().token, "hnt_00ff");
        // One already in the data folder wins; the same folder is left alone.
        save(&boot, &joined_from("https://other:8790", &ca, &json!({ "node_id": "cd34", "token": "hnt_11ee", "ceiling": "look" }), "key").unwrap()).unwrap();
        adopt_left_behind(&boot, &data);
        assert_eq!(load(&data).unwrap().token, "hnt_00ff");
        assert!(boot.join(FILE).exists());
        adopt_left_behind(&data, &data);
        assert!(data.join(FILE).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `hopperd node ceiling` shows the level and only ever lowers it.
    #[test]
    fn the_ceiling_command_only_lowers() {
        let dir = std::env::temp_dir().join(format!("hopperd-ceilcmd-{}", crate::auth::random_hex(4)));
        assert!(ceiling(&dir, None).unwrap().contains("isn't a node"));
        let ca = rustls::pki_types::CertificateDer::from(vec![1u8, 2, 3]);
        save(&dir, &joined_from("https://rig:8790", &ca, &json!({ "node_id": "ab12", "token": "hnt_00ff", "ceiling": "operate", "main_name": "rig" }), "key").unwrap()).unwrap();
        assert!(ceiling(&dir, None).unwrap().contains("(operate)"));
        assert!(ceiling(&dir, Some("manage")).is_err());
        assert!(ceiling(&dir, Some("admin")).is_err());
        assert_eq!(load(&dir).unwrap().ceiling, "operate");
        ceiling(&dir, Some("look")).unwrap();
        assert_eq!(load(&dir).unwrap().ceiling, "look");
        assert!(ceiling(&dir, Some("operate")).is_err(), "back up means joining again");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
