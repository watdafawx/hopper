//! Server panels: the software that runs the rig's servers. Hopper lists a
//! panel's servers, shows their live stats and console, and starts, stops,
//! restarts or kills them. Crafty Controller 4 is the first; others plug in
//! through [`ServerPanel`].

mod hopperd;
mod mcss;
mod pterodactyl;
mod pterodactyl_ws;

pub use hopperd::HopperServer;
pub use mcss::Mcss;
pub use pterodactyl::Pterodactyl;

use anyhow::{Context as _, bail};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

/// What a panel can be asked to do to a server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelAction {
    Start,
    Stop,
    Restart,
    /// End the process at once, without saving the world.
    Kill,
    Backup,
}

impl PanelAction {
    pub fn label(self) -> &'static str {
        match self {
            PanelAction::Start => "Start",
            PanelAction::Stop => "Stop",
            PanelAction::Restart => "Restart",
            PanelAction::Kill => "Kill",
            PanelAction::Backup => "Back up",
        }
    }

    /// Whether a poll's `s` shows this action finished, so the app stops
    /// showing it as under way. `seen_up`: a poll since the action was
    /// sent showed the process running (starting counts); `waited`: time
    /// since it was sent.
    ///
    /// A start is over once the server is up, or once its process ended:
    /// a server that dies while loading mods goes starting → stopped (or
    /// crashed) and never reaches running, and waiting for running would
    /// keep it "starting" with Start greyed out for good. A panel can
    /// still say stopped for a moment right after taking the request, so
    /// a stopped server it never saw up only counts after a short grace.
    pub fn settled(self, s: &PanelStats, seen_up: bool, waited: std::time::Duration) -> bool {
        match self {
            PanelAction::Start => {
                s.crashed || (s.running && !s.starting) || (!s.running && (seen_up || waited >= START_GRACE))
            }
            PanelAction::Stop | PanelAction::Kill => !s.running,
            PanelAction::Restart | PanelAction::Backup => true,
        }
    }

    /// What to ask before doing this to the server called `name`, or None
    /// to go ahead at once. A kill always asks. A stop or restart asks when
    /// players are on (naming them) or when Hopper can't tell who is
    /// (`online` is None); with nobody on there's nothing to lose, so it
    /// just goes. Hopper Server's panel words it the same way
    /// (`ui/src/lib/consoleKit.js`).
    pub fn confirm_text(self, name: &str, online: Option<u32>, players: &[String]) -> Option<String> {
        match self {
            PanelAction::Kill => Some(format!(
                "Kill {name}? The process ends at once: the world isn't saved, so anything since the last autosave is lost."
            )),
            PanelAction::Stop | PanelAction::Restart => match online {
                None => Some(format!(
                    "{} {name}? Hopper can't read its state right now, so it can't tell who is online.",
                    self.label()
                )),
                Some(0) => None,
                Some(n) => Some(format!("{} {name}? {}", self.label(), who_online(n, players))),
            },
            PanelAction::Start | PanelAction::Backup => None,
        }
    }
}

/// "Steve is online.", "Steve and Alex are online.", "Steve, Alex and 3
/// others are online.", or just the count when the names aren't known.
pub fn who_online(n: u32, players: &[String]) -> String {
    let names: Vec<&str> = players.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    if names.is_empty() || names.len() as u32 != n {
        return format!("{n} player{} online.", if n == 1 { " is" } else { "s are" });
    }
    match names.as_slice() {
        [one] => format!("{one} is online."),
        [a, b] => format!("{a} and {b} are online."),
        [a, b, c] => format!("{a}, {b} and {c} are online."),
        [a, b, rest @ ..] => format!("{a}, {b} and {} others are online.", rest.len()),
        [] => unreachable!(),
    }
}

/// How long a panel may keep saying "stopped" after a start request
/// before Hopper believes it (see [`PanelAction::settled`]).
pub const START_GRACE: std::time::Duration = std::time::Duration::from_secs(20);

/// A server as the panel knows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PanelServer {
    pub id: String,
    pub name: String,
    /// The server's folder on the machine the panel runs on.
    pub path: String,
    pub port: u16,
}

/// A server's latest stats.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PanelStats {
    pub running: bool,
    pub crashed: bool,
    /// Starting up (waiting for the server to come up).
    pub starting: bool,
    pub updating: bool,
    pub cpu: f32,
    pub mem: String,
    pub mem_percent: f32,
    pub online: u32,
    pub max: u32,
    pub players: Vec<String>,
    pub version: String,
    pub world_size: String,
    /// When it started, as the panel writes it.
    pub started: String,
    /// Addresses players join it by (Hopper Server's router).
    pub addresses: Vec<String>,
    /// Sleeps after this many minutes with nobody on (Hopper Server); 0: never.
    pub sleep_minutes: u32,
    /// Ticks per second, from panels that read it (Hopper Server).
    pub tps: Option<Tps>,
    /// Why such a panel has no TPS for it: "rcon_off" or "unsupported".
    pub no_tps: Option<String>,
    /// How this PC reaches the panel, as Hopper Server sees it: "LAN",
    /// "Tailscale", "Internet" or "Local". The panel decides from the real
    /// client address, so a public name that points home still says LAN.
    pub via: Option<&'static str>,
}

/// Hopper Server's `via` (from /hello) in words, as the web panel shows it.
pub fn route_label(via: &str) -> Option<&'static str> {
    match via {
        "local" => Some("Local"),
        "lan" => Some("LAN"),
        "tailscale" => Some("Tailscale"),
        "internet" => Some("Internet"),
        _ => None,
    }
}

/// A TPS reading, with the time a tick takes when the loader says.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tps {
    pub tps: f32,
    pub mspt: Option<f32>,
}

/// How healthy a TPS reading is: 20 is full speed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TpsLevel {
    Good,
    Slow,
    Bad,
}

impl Tps {
    /// Yellow below 18, red below 15 (Hopper Server's default alert).
    pub fn level(&self) -> TpsLevel {
        if self.tps >= 18.0 {
            TpsLevel::Good
        } else if self.tps >= 15.0 {
            TpsLevel::Slow
        } else {
            TpsLevel::Bad
        }
    }

    /// "19.9", one decimal like the panel.
    pub fn text(&self) -> String {
        format!("{:.1}", self.tps)
    }

    /// "12.4 ms a tick", when known.
    pub fn mspt_text(&self) -> Option<String> {
        self.mspt.map(|m| format!("{m:.1} ms a tick"))
    }
}

impl PanelStats {
    /// "Up 3h 30m" from a panel that gives the uptime, "Up since …" from one
    /// that gives the start time; empty when it gives neither.
    pub fn up_text(&self) -> String {
        let s = self.started.trim();
        if s.is_empty() {
            String::new()
        } else if let Some(rest) = s.strip_prefix("up ") {
            format!("Up {rest}")
        } else {
            format!("Up since {s}")
        }
    }

    pub fn state(&self) -> &'static str {
        if self.crashed {
            "Crashed"
        } else if self.updating {
            "Updating"
        } else if self.starting {
            "Starting"
        } else if self.running {
            "Running"
        } else {
            "Stopped"
        }
    }
}

/// How the panel runs a server, where the panel has such settings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PanelOptions {
    /// Start the server again when it crashes.
    pub crash_detection: bool,
    /// Start the server when the panel starts.
    pub auto_start: bool,
    /// Seconds to wait before that start.
    pub auto_start_delay: u32,
    /// The console command that stops the server.
    pub stop_command: String,
    /// Days to keep old logs; 0 keeps them all.
    pub logs_delete_after: u32,
    /// The command the panel starts the server with (shown, not edited).
    pub execution_command: String,
}

/// One of a server's backup setups in the panel, with its backup files.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BackupSet {
    pub id: String,
    pub name: String,
    /// Backup files, newest first (as the panel names them).
    pub files: Vec<String>,
}

/// A server panel Hopper can drive.
pub trait ServerPanel: Send + Sync {
    /// "Crafty", for messages.
    fn name(&self) -> &'static str;
    fn servers(&self) -> anyhow::Result<Vec<PanelServer>>;
    fn stats(&self, id: &str) -> anyhow::Result<PanelStats>;
    fn act(&self, id: &str, action: PanelAction) -> anyhow::Result<()>;
    /// The console's latest lines, oldest first.
    fn console(&self, id: &str) -> anyhow::Result<Vec<String>>;
    /// Sends a console command (without the leading slash).
    fn command(&self, id: &str, line: &str) -> anyhow::Result<()>;
    /// The whole log file of the current run, where the panel reads it;
    /// else the console's latest lines.
    fn log_file(&self, id: &str) -> anyhow::Result<Vec<String>> {
        self.console(id)
    }
    /// How the panel runs the server, where it has such settings.
    fn options(&self, _id: &str) -> anyhow::Result<Option<PanelOptions>> {
        Ok(None)
    }
    /// Changes how the panel runs the server (only the fields that differ
    /// from `old`).
    fn set_options(&self, _id: &str, _old: &PanelOptions, _new: &PanelOptions) -> anyhow::Result<()> {
        bail!("Change this in the panel itself.")
    }
    /// The server's backup setups and files, where the panel lists them.
    fn backups(&self, _id: &str) -> anyhow::Result<Vec<BackupSet>> {
        bail!("Backups for this panel are in the panel itself.")
    }
    /// Runs one backup setup now.
    fn backup_now(&self, id: &str, _set: &str) -> anyhow::Result<()> {
        self.act(id, PanelAction::Backup)
    }
    /// Points the panel's own launch command at NeoForge `version`, for a
    /// panel that keeps one apart from run.bat. Says what changed, if
    /// anything.
    fn use_neoforge(&self, _id: &str, _version: &str) -> anyhow::Result<Option<String>> {
        Ok(None)
    }
    /// Restores a backup file over the server (it must be stopped).
    fn restore(&self, _id: &str, _set: &str, _file: &str) -> anyhow::Result<()> {
        bail!("Restore from the panel itself.")
    }
}

/// The panels Hopper knows: (settings value, name shown).
pub const KINDS: [(&str, &str); 4] = [
    ("hopperd", "Hopper Server"),
    ("crafty", "Crafty Controller 4"),
    ("mcss", "MCSS (MC Server Soft)"),
    ("pterodactyl", "Pterodactyl"),
];

/// The short name of the panel kind in settings ("Crafty").
pub fn short_name(kind: &str) -> &'static str {
    match kind {
        "mcss" => "MCSS",
        "pterodactyl" => "Pterodactyl",
        "hopperd" => "Hopper Server",
        _ => "Crafty",
    }
}

/// Where each panel answers by default, as a hint.
pub fn address_hint(kind: &str) -> &'static str {
    match kind {
        "mcss" => "https://rig:25560",
        "hopperd" => "rig (or http://192.168.50.3:8790)",
        "pterodactyl" => "https://panel.example.com",
        _ => "https://rig:8443",
    }
}

/// The Crafty / MCSS / Pterodactyl panels set up in Settings, in order,
/// with an address. `HOPPER_PANEL_URL` (and `HOPPER_PANEL_TOKEN`) in .env
/// replace the first one's, or make one, for tests.
pub fn panels(s: &crate::settings::Settings) -> Vec<crate::settings::OtherPanel> {
    let mut out = s.panels.clone();
    if let Some(url) = env("HOPPER_PANEL_URL") {
        match out.iter_mut().find(|p| p.id.is_empty()) {
            Some(p) => p.url = url,
            None => out.insert(
                0,
                crate::settings::OtherPanel {
                    kind: env("HOPPER_PANEL_KIND").unwrap_or_else(|| s.panel_kind.clone()),
                    url,
                    self_signed: s.panel_self_signed,
                    ..Default::default()
                },
            ),
        }
    }
    out.retain(|p| !p.url.trim().is_empty());
    out
}

/// A client for panel `p`, when its API key is saved.
pub fn open(p: &crate::settings::OtherPanel) -> Option<Box<dyn ServerPanel>> {
    let token = token_for(&p.id)?;
    let (url, ss) = (p.url.trim(), p.self_signed);
    if url.is_empty() {
        return None;
    }
    Some(match p.kind.as_str() {
        "mcss" => Box::new(Mcss::new(url, &token, ss)),
        "pterodactyl" => Box::new(Pterodactyl::new(url, &token, ss)),
        _ => Box::new(Crafty::new(url, &token, ss)),
    })
}

/// What a panel's client is made from, to tell when it must be made again
/// (the key's length stands in for the key).
pub fn setup_key(p: &crate::settings::OtherPanel) -> String {
    format!("{}|{}|{}|{}|{}", p.id, p.kind, p.url.trim(), p.self_signed, token_for(&p.id).map_or(0, |t| t.len()))
}

/// The first panel set up in Settings, if any (for code with no server
/// in mind).
pub fn from_settings(s: &crate::settings::Settings) -> Option<Box<dyn ServerPanel>> {
    panels(s).iter().find_map(open)
}

/// Which panel each server folder was found on (folder → panel id), as
/// the app's poll last saw it.
static PANEL_OF: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<PathBuf, String>>> =
    std::sync::LazyLock::new(Default::default);

/// Notes that the server in `root` is on panel `id`.
pub fn remember(root: &std::path::Path, id: &str) {
    PANEL_OF.lock().unwrap_or_else(|e| e.into_inner()).insert(root.to_path_buf(), id.to_string());
}

/// The panel the server in `root` was last found on, or the only panel;
/// None while that's unknown. No network: fine on the UI thread.
pub fn known_panel(s: &crate::settings::Settings, root: &std::path::Path) -> Option<crate::settings::OtherPanel> {
    let all = panels(s);
    let known = PANEL_OF.lock().unwrap_or_else(|e| e.into_inner()).get(root).cloned();
    match known.and_then(|id| all.iter().find(|p| p.id == id).cloned()) {
        Some(p) => Some(p),
        None if all.len() == 1 => all.into_iter().next(),
        None => None,
    }
}

/// The panel that runs the server in `server.root`: the one it was last
/// found on; else the only panel; else the first whose list has it.
pub fn panel_of(
    s: &crate::settings::Settings,
    server: &crate::server::ServerInfo,
) -> Option<(crate::settings::OtherPanel, Box<dyn ServerPanel>)> {
    let all = panels(s);
    let known = PANEL_OF.lock().unwrap_or_else(|e| e.into_inner()).get(&server.root).cloned();
    if let Some(p) = known.and_then(|id| all.iter().find(|p| p.id == id)) {
        return Some((p.clone(), open(p)?));
    }
    if let [p] = &all[..] {
        return Some((p.clone(), open(p)?));
    }
    for p in &all {
        let Some(panel) = open(p) else { continue };
        let Ok(list) = panel.servers() else { continue };
        if match_server(&list, server).is_some() {
            remember(&server.root, &p.id);
            return Some((p.clone(), panel));
        }
    }
    None
}

/// `url` with a scheme and no trailing slash.
pub fn base_url(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    if url.contains("://") {
        url.to_string()
    } else {
        format!("https://{url}")
    }
}

/// An HTTP client for a panel: short timeouts, HTTP errors as replies (the
/// panels explain them in JSON), and optionally a self-signed certificate.
pub(crate) fn agent(self_signed: bool) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .disable_verification(self_signed)
                .build(),
        )
        .build()
        .into()
}

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The panel's server for one of Hopper's servers: the same folder name
/// (the panel sees the rig's own drive, Hopper a mapped one); else the one
/// panel server on its game port (Crafty keeps servers in id-named folders,
/// often junctions to the real ones); else the same name.
pub fn match_server<'a>(
    servers: &'a [PanelServer],
    server: &crate::server::ServerInfo,
) -> Option<&'a PanelServer> {
    let folder = server.root.file_name()?.to_string_lossy().to_lowercase();
    let port = server.port();
    let last = |p: &str| {
        p.trim_end_matches(['\\', '/'])
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or("")
            .to_lowercase()
    };
    let on_port: Vec<&PanelServer> = servers.iter().filter(|s| s.port == port).collect();
    servers
        .iter()
        .find(|s| last(&s.path) == folder)
        .or(match on_port[..] {
            [one] if port != 0 => Some(one),
            _ => None,
        })
        .or_else(|| {
            servers
                .iter()
                .find(|s| s.name.trim().to_lowercase() == folder)
        })
}

// ---- the API key, kept in Credential Manager --------------------------------

/// Where earlier builds kept the key, moved into Credential Manager once.
fn old_token_path() -> Option<PathBuf> {
    crate::paths::dirs().map(|d| d.data_dir().join("panel-token"))
}

/// The panel's API key, when one is saved (HOPPER_PANEL_TOKEN wins).
pub fn token() -> Option<String> {
    if let Some(t) = env("HOPPER_PANEL_TOKEN") {
        return Some(t);
    }
    if let Some(t) = crate::secrets::get(crate::secrets::PANEL_KEY) {
        return Some(t);
    }
    let path = old_token_path()?;
    let t = std::fs::read_to_string(&path).ok()?.trim().to_string();
    if t.is_empty() {
        return None;
    }
    if crate::secrets::set(crate::secrets::PANEL_KEY, &t).is_ok() {
        let _ = std::fs::remove_file(path);
    }
    Some(t)
}

/// Saves the API key (empty removes it).
pub fn set_token(token: &str) -> anyhow::Result<()> {
    crate::secrets::set(crate::secrets::PANEL_KEY, token)
}

/// The API key of panel `id` ("" is the first; see [`token`]).
pub fn token_for(id: &str) -> Option<String> {
    if id.is_empty() {
        return token();
    }
    crate::secrets::get(&crate::secrets::panel_key(id))
}

/// Saves panel `id`'s API key (empty removes it).
pub fn set_token_for(id: &str, token: &str) -> anyhow::Result<()> {
    crate::secrets::set(&crate::secrets::panel_key(id), token)
}

/// The token from pairing with a Hopper Server, kept apart from other
/// panels' API keys (HOPPER_PANEL_TOKEN wins, for tests).
pub fn hopperd_token() -> Option<String> {
    env("HOPPER_PANEL_TOKEN").or_else(|| crate::secrets::get(crate::secrets::HOPPERD_KEY))
}

pub fn set_hopperd_token(token: &str) -> anyhow::Result<()> {
    crate::secrets::set(crate::secrets::HOPPERD_KEY, token)
}

/// The token of paired panel `id` ("" is the first; HOPPER_PANEL_TOKEN wins
/// for it, for tests).
pub fn hopperd_token_for(id: &str) -> Option<String> {
    if id.is_empty() {
        return hopperd_token();
    }
    crate::secrets::get(&crate::secrets::hopperd_key(id))
}

pub fn set_hopperd_token_for(id: &str, token: &str) -> anyhow::Result<()> {
    crate::secrets::set(&crate::secrets::hopperd_key(id), token)
}

/// The panel that runs `server`: its Hopper Server when it's on one, else
/// the panel it was found on ([`panel_of`]).
pub fn for_server(s: &crate::settings::Settings, server: &crate::server::ServerInfo) -> Option<Box<dyn ServerPanel>> {
    match &server.remote {
        Some(r) => Some(Box::new(HopperServer::from_client(crate::hopperd::Client::for_remote(r).ok()?))),
        None => panel_of(s, server).map(|(_, p)| p),
    }
}

// ---- Crafty Controller 4 ---------------------------------------------------

/// Crafty Controller 4's API (v2), with an API key from Crafty's user
/// settings (API Keys).
pub struct Crafty {
    base: String,
    token: String,
    agent: ureq::Agent,
}

impl Crafty {
    /// `self_signed` accepts the certificate Crafty makes for itself, which
    /// no authority signs.
    pub fn new(url: &str, token: &str, self_signed: bool) -> Self {
        Crafty {
            base: base_url(url),
            token: token.to_string(),
            agent: agent(self_signed),
        }
    }

    fn call(&self, method: &str, path: &str, body: Option<&str>) -> anyhow::Result<Value> {
        let url = format!("{}/api/v2{path}", self.base);
        let auth = format!("Bearer {}", self.token);
        let resp = match (method, body) {
            ("GET", _) => self.agent.get(&url).header("Authorization", &auth).call(),
            (_, Some(b)) => self
                .agent
                .post(&url)
                .header("Authorization", &auth)
                .content_type("text/plain")
                .send(b),
            (_, None) => self
                .agent
                .post(&url)
                .header("Authorization", &auth)
                .send_empty(),
        };
        let mut resp = resp.with_context(|| format!("Crafty at {}", self.base))?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        unwrap(status, &text)
    }

    fn call_json(&self, path: &str, body: &str) -> anyhow::Result<Value> {
        self.send_json("POST", path, body)
    }

    fn send_json(&self, method: &str, path: &str, body: &str) -> anyhow::Result<Value> {
        let url = format!("{}/api/v2{path}", self.base);
        let req = match method {
            "PATCH" => self.agent.patch(&url),
            _ => self.agent.post(&url),
        };
        let mut resp = req
            .header("Authorization", &format!("Bearer {}", self.token))
            .content_type("application/json")
            .send(body)
            .with_context(|| format!("Crafty at {}", self.base))?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        unwrap(status, &text)
    }
}

/// Crafty's `{status, data, error, error_data}` reply, as its data or an
/// error that says what went wrong.
fn unwrap(status: u16, text: &str) -> anyhow::Result<Value> {
    let v: Value = serde_json::from_str(text)
        .with_context(|| format!("Crafty answered {status} with something that isn't JSON"))?;
    if v["status"] == "ok" && status < 400 {
        return Ok(v["data"].clone());
    }
    let error = v["error"].as_str().unwrap_or("error");
    let detail = v["error_data"].as_str().unwrap_or("");
    match (status, error) {
        (401 | 403, _) | (_, "ACCESS_DENIED") => {
            bail!("Crafty refused the API key. Make a new one in Crafty (your user → API Keys).")
        }
        (_, "NOT_AUTHORIZED") => {
            bail!("The API key can't do that on this server. Give it the Commands permission in Crafty.")
        }
        (_, "SERVER_NOT_RUNNING") => bail!("The server isn't running."),
        _ if detail.is_empty() => bail!("Crafty: {error} ({status})"),
        _ => bail!("Crafty: {error}: {detail}"),
    }
}

fn parse_servers(data: &Value) -> Vec<PanelServer> {
    data.as_array()
        .into_iter()
        .flatten()
        .map(|s| PanelServer {
            id: match &s["server_id"] {
                Value::String(x) => x.clone(),
                other => other.to_string(),
            },
            name: s["server_name"].as_str().unwrap_or("").to_string(),
            path: s["path"].as_str().unwrap_or("").to_string(),
            port: s["server_port"].as_u64().unwrap_or(0) as u16,
        })
        .collect()
}

/// Crafty sends memory as "4.1GB" or, on newer versions, as plain bytes
/// ("17668972544.0"); bytes read as "16.5 GB".
fn memory(raw: &str) -> String {
    match raw.trim().parse::<f64>() {
        Ok(b) if b >= 1024.0 => crate::instance_ops::human_bytes(b as u64),
        _ => raw.to_string(),
    }
}

fn parse_stats(d: &Value) -> PanelStats {
    let num = |k: &str| match &d[k] {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => s.trim_end_matches('%').parse().unwrap_or(0.0),
        _ => 0.0,
    };
    let text = |k: &str| match &d[k] {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    // Players come as a JSON list, or as that list in a string.
    let players = match &d["players"] {
        Value::Array(a) => a.iter().filter_map(|p| p.as_str().map(String::from)).collect(),
        Value::String(s) => serde_json::from_str::<Vec<String>>(&s.replace('\'', "\""))
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    PanelStats {
        running: d["running"].as_bool().unwrap_or(false),
        crashed: d["crashed"].as_bool().unwrap_or(false),
        starting: d["waiting_start"].as_bool().unwrap_or(false),
        updating: d["updating"].as_bool().unwrap_or(false),
        cpu: num("cpu") as f32,
        mem: memory(&text("mem")),
        mem_percent: num("mem_percent") as f32,
        online: num("online") as u32,
        max: num("max") as u32,
        players,
        version: text("version"),
        world_size: text("world_size"),
        started: text("started"),
        ..Default::default()
    }
}

fn parse_backup_sets(data: &Value) -> Vec<BackupSet> {
    let text = |v: &Value| match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    data.as_array()
        .into_iter()
        .flatten()
        .map(|b| BackupSet {
            id: text(&b["backup_id"]),
            name: Some(text(&b["backup_name"]))
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "Backup".into()),
            files: Vec::new(),
        })
        .filter(|b| !b.id.is_empty())
        .collect()
}

/// Backup file paths as just their names, newest (by name, which Crafty
/// stamps with the date) first.
fn backup_files(data: &Value) -> Vec<String> {
    let mut files: Vec<String> = data["backups"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p.as_str())
        .map(|p| p.rsplit(['/', '\\']).next().unwrap_or(p).to_string())
        .collect();
    files.sort();
    files.reverse();
    files
}

/// The fields of a Crafty server record to change so it starts NeoForge
/// `version`: its execution command and executable, where they name
/// another NeoForge.
fn crafty_neoforge_patch(d: &Value, version: &str) -> serde_json::Map<String, Value> {
    let mut patch = serde_json::Map::new();
    for key in ["execution_command", "executable"] {
        let Some(text) = d[key].as_str() else { continue };
        if let Some(old) = crate::server_update::version_in(text)
            && old != version
        {
            let new = crate::server_update::swap_neoforge(text, &old, version);
            patch.insert(key.into(), Value::String(new));
        }
    }
    patch
}

/// Crafty's log lines, which it escapes for its web page (&quot; and so on).
fn log_lines(data: &Value) -> Vec<String> {
    data.as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| l.as_str().map(unescape_html))
        .collect()
}

/// A Crafty server record's run settings.
fn parse_options(d: &Value) -> PanelOptions {
    let num = |k: &str| match &d[k] {
        Value::Number(n) => n.as_u64().unwrap_or(0) as u32,
        Value::String(s) => s.trim().parse().unwrap_or(0),
        _ => 0,
    };
    PanelOptions {
        crash_detection: d["crash_detection"].as_bool().unwrap_or(false),
        auto_start: d["auto_start"].as_bool().unwrap_or(false),
        auto_start_delay: num("auto_start_delay"),
        stop_command: d["stop_command"].as_str().unwrap_or("stop").to_string(),
        logs_delete_after: num("logs_delete_after"),
        execution_command: d["execution_command"].as_str().unwrap_or_default().to_string(),
    }
}

/// The fields of a Crafty server record that changed from `old` to `new`.
fn options_patch(old: &PanelOptions, new: &PanelOptions) -> serde_json::Map<String, Value> {
    let mut p = serde_json::Map::new();
    if old.crash_detection != new.crash_detection {
        p.insert("crash_detection".into(), new.crash_detection.into());
    }
    if old.auto_start != new.auto_start {
        p.insert("auto_start".into(), new.auto_start.into());
    }
    if old.auto_start_delay != new.auto_start_delay {
        p.insert("auto_start_delay".into(), new.auto_start_delay.into());
    }
    if old.stop_command != new.stop_command {
        p.insert("stop_command".into(), new.stop_command.trim().into());
    }
    if old.logs_delete_after != new.logs_delete_after {
        p.insert("logs_delete_after".into(), new.logs_delete_after.into());
    }
    p
}

/// Undoes HTML escaping: `&quot;`, `&amp;`, `&lt;`, `&gt;`, `&#39;`, `&#x27;`
/// and other numeric references. Anything else stays as it is.
fn unescape_html(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest[1..].find(';').map(|e| e + 1).filter(|&e| e <= 10);
        let decoded = end.and_then(|e| {
            let name = &rest[1..e];
            let c = match name {
                "quot" => Some('"'),
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => {
                    let n = name.strip_prefix('#')?;
                    let code = match n.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => n.parse().ok()?,
                    };
                    char::from_u32(code)
                }
            }?;
            Some((c, e))
        });
        match decoded {
            Some((c, e)) => {
                out.push(c);
                rest = &rest[e + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn crafty_action(a: PanelAction) -> &'static str {
    match a {
        PanelAction::Start => "start_server",
        PanelAction::Stop => "stop_server",
        PanelAction::Restart => "restart_server",
        PanelAction::Kill => "kill_server",
        PanelAction::Backup => "backup_server",
    }
}

impl ServerPanel for Crafty {
    fn name(&self) -> &'static str {
        "Crafty"
    }

    fn servers(&self) -> anyhow::Result<Vec<PanelServer>> {
        Ok(parse_servers(&self.call("GET", "/servers", None)?))
    }

    fn stats(&self, id: &str) -> anyhow::Result<PanelStats> {
        Ok(parse_stats(&self.call("GET", &format!("/servers/{id}/stats"), None)?))
    }

    fn act(&self, id: &str, action: PanelAction) -> anyhow::Result<()> {
        let path = format!("/servers/{id}/action/{}", crafty_action(action));
        self.call("POST", &path, None).map(|_| ())
    }

    fn console(&self, id: &str) -> anyhow::Result<Vec<String>> {
        Ok(log_lines(&self.call("GET", &format!("/servers/{id}/logs"), None)?))
    }

    fn log_file(&self, id: &str) -> anyhow::Result<Vec<String>> {
        Ok(log_lines(&self.call("GET", &format!("/servers/{id}/logs?file=true"), None)?))
    }

    fn options(&self, id: &str) -> anyhow::Result<Option<PanelOptions>> {
        Ok(Some(parse_options(&self.call("GET", &format!("/servers/{id}"), None)?)))
    }

    fn set_options(&self, id: &str, old: &PanelOptions, new: &PanelOptions) -> anyhow::Result<()> {
        let patch = options_patch(old, new);
        if patch.is_empty() {
            return Ok(());
        }
        let body = Value::Object(patch).to_string();
        self.send_json("PATCH", &format!("/servers/{id}"), &body)
            .map(|_| ())
            .map_err(|e| {
                if e.to_string().contains("Commands permission") {
                    anyhow::anyhow!("the API key needs the Config permission on this server")
                } else {
                    e
                }
            })
    }

    fn command(&self, id: &str, line: &str) -> anyhow::Result<()> {
        let line = line.trim().trim_start_matches('/');
        if line.is_empty() {
            return Ok(());
        }
        self.call("POST", &format!("/servers/{id}/stdin"), Some(line))
            .map(|_| ())
    }

    fn backups(&self, id: &str) -> anyhow::Result<Vec<BackupSet>> {
        let mut sets = parse_backup_sets(&self.call("GET", &format!("/servers/{id}/backups"), None)?);
        for set in &mut sets {
            let files = self.call("GET", &format!("/servers/{id}/backups/backup/{}/files", set.id), None);
            set.files = files.map(|f| backup_files(&f)).unwrap_or_default();
        }
        Ok(sets)
    }

    fn backup_now(&self, id: &str, set: &str) -> anyhow::Result<()> {
        self.call("POST", &format!("/servers/{id}/action/backup_server/{set}"), None)
            .map(|_| ())
    }

    /// Crafty starts a server with its own Server Execution Command (and
    /// keeps the loader jar as its executable), not run.bat.
    fn use_neoforge(&self, id: &str, version: &str) -> anyhow::Result<Option<String>> {
        let d = self.call("GET", &format!("/servers/{id}"), None)?;
        let patch = crafty_neoforge_patch(&d, version);
        if patch.is_empty() {
            return Ok(None);
        }
        let body = Value::Object(patch).to_string();
        self.send_json("PATCH", &format!("/servers/{id}"), &body).map_err(|e| {
            if e.to_string().contains("Commands permission") {
                anyhow::anyhow!("the API key needs the Config permission on this server")
            } else {
                e
            }
        })?;
        Ok(Some(format!("Crafty's launch command now uses NeoForge {version}")))
    }

    fn restore(&self, id: &str, set: &str, file: &str) -> anyhow::Result<()> {
        let body = serde_json::json!({ "filename": file, "inPlace": true }).to_string();
        self.call_json(&format!("/servers/{id}/backups/backup/{set}"), &body)
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn confirm_skips_an_empty_server() {
        let p = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let r = PanelAction::Restart;
        assert_eq!(r.confirm_text("TNP", Some(0), &[]), None);
        assert_eq!(PanelAction::Stop.confirm_text("TNP", Some(0), &[]), None);
        assert!(r.confirm_text("TNP", None, &[]).unwrap().contains("can't tell who is online"));
        assert_eq!(r.confirm_text("TNP", Some(1), &p(&["Steve"])).unwrap(), "Restart TNP? Steve is online.");
        assert_eq!(
            PanelAction::Stop.confirm_text("TNP", Some(2), &p(&["Steve", "Alex"])).unwrap(),
            "Stop TNP? Steve and Alex are online."
        );
        assert_eq!(
            r.confirm_text("TNP", Some(5), &p(&["a", "b", "c", "d", "e"])).unwrap(),
            "Restart TNP? a, b and 3 others are online."
        );
        // Names that don't match the count: fall back to the number.
        assert_eq!(r.confirm_text("TNP", Some(3), &p(&["a"])).unwrap(), "Restart TNP? 3 players are online.");
        assert!(PanelAction::Kill.confirm_text("TNP", Some(0), &[]).is_some());
        assert_eq!(PanelAction::Start.confirm_text("TNP", None, &[]), None);
    }

    fn st(running: bool, starting: bool, crashed: bool) -> PanelStats {
        PanelStats { running, starting, crashed, ..Default::default() }
    }

    #[test]
    fn up_text_reads_both_kinds() {
        let up = |s: &str| PanelStats { started: s.into(), ..Default::default() }.up_text();
        assert_eq!(up("up 3h 30m"), "Up 3h 30m");
        assert_eq!(up("2026-10-12 10:00:00"), "Up since 2026-10-12 10:00:00");
        assert_eq!(up(""), "");
    }

    #[test]
    fn tps_levels_and_text() {
        let t = |tps: f32| Tps { tps, mspt: None };
        assert_eq!(t(20.0).level(), TpsLevel::Good);
        assert_eq!(t(18.0).level(), TpsLevel::Good);
        assert_eq!(t(17.9).level(), TpsLevel::Slow);
        assert_eq!(t(15.0).level(), TpsLevel::Slow);
        assert_eq!(t(14.9).level(), TpsLevel::Bad);
        assert_eq!(t(19.94).text(), "19.9");
        assert_eq!(t(20.0).mspt_text(), None);
        assert_eq!(Tps { tps: 20.0, mspt: Some(12.44) }.mspt_text().as_deref(), Some("12.4 ms a tick"));
    }

    #[test]
    fn start_settles_when_the_process_ends() {
        use std::time::Duration;
        let now = Duration::ZERO;
        let start = PanelAction::Start;
        // Still loading: under way.
        assert!(!start.settled(&st(true, true, false), true, now));
        // Up: done.
        assert!(start.settled(&st(true, false, false), true, now));
        // Died while loading mods, exit code 0 (hopperd says stopped): done.
        assert!(start.settled(&st(false, false, false), true, now));
        // The same with a non-zero exit (crashed): done, even unseen.
        assert!(start.settled(&st(false, false, true), false, now));
        // Stopped before the panel ever showed it starting: a panel that
        // hasn't caught up yet, until the grace runs out.
        assert!(!start.settled(&st(false, false, false), false, Duration::from_secs(3)));
        assert!(start.settled(&st(false, false, false), false, START_GRACE));
    }

    #[test]
    fn other_actions_settle() {
        use std::time::Duration;
        let z = Duration::ZERO;
        assert!(!PanelAction::Stop.settled(&st(true, false, false), true, z));
        assert!(PanelAction::Stop.settled(&st(false, false, false), true, z));
        assert!(PanelAction::Kill.settled(&st(false, false, true), true, z));
        assert!(PanelAction::Restart.settled(&st(true, true, false), true, z));
    }

    fn server(root: &str, port: u16) -> crate::server::ServerInfo {
        crate::server::ServerInfo {
            name: String::new(),
            root: root.into(),
            manager: crate::server::Manager::Crafty,
            properties: [("server-port".to_string(), port.to_string())].into(),
            loader: None,
            java: None,
            max_memory: None,
            remote: None,
        }
    }

    #[test]
    #[cfg_attr(not(windows), ignore = "uses Windows paths and tools")]
    fn reads_servers_and_matches_folders() {
        let list = parse_servers(&json!([
            { "server_id": "a1-b2", "server_name": "TNP", "path": "G:\\Minecraft\\packs\\TNP-LL8\\", "server_port": 25565 },
            { "server_id": "c3", "server_name": "val ", "path": "/srv/other", "server_port": 25570 },
            { "server_id": "d4", "server_name": "TNP-LL ", "path": "G:/crafty/servers/051e6fc7", "server_port": 25567 }
        ]));
        assert_eq!(list[0].id, "a1-b2");
        assert_eq!(list[0].port, 25565);
        let hit = match_server(&list, &server(r"Q:\Minecraft\packs\TNP-LL8", 1)).unwrap();
        assert_eq!(hit.id, "a1-b2");
        // Crafty's id-named folder: the game port finds it.
        assert_eq!(match_server(&list, &server(r"Q:\packs\Mine", 25567)).unwrap().id, "d4");
        // Falls back to the panel's name for the server (trimmed).
        assert_eq!(match_server(&list, &server(r"Q:\packs\Val", 1)).unwrap().id, "c3");
        assert!(match_server(&list, &server(r"Q:\packs\none", 1)).is_none());
    }

    #[test]
    fn options_read_and_patch_only_what_changed() {
        let o = parse_options(&json!({
            "crash_detection": true, "auto_start": false, "auto_start_delay": "10",
            "stop_command": "stop", "logs_delete_after": 7, "execution_command": "java -jar x.jar"
        }));
        assert!(o.crash_detection && !o.auto_start);
        assert_eq!((o.auto_start_delay, o.logs_delete_after), (10, 7));
        let mut n = o.clone();
        n.auto_start = true;
        n.stop_command = " end ".into();
        let p = options_patch(&o, &n);
        assert_eq!(p.len(), 2);
        assert_eq!(p["auto_start"], json!(true));
        assert_eq!(p["stop_command"], json!("end"));
        assert!(options_patch(&o, &o).is_empty());
    }

    #[test]
    fn console_lines_are_unescaped() {
        assert_eq!(
            unescape_html("Stuck Thread: &quot;DH-IO Thread[4]&quot; Id=1457"),
            "Stuck Thread: \"DH-IO Thread[4]\" Id=1457"
        );
        assert_eq!(unescape_html("a &lt;b&gt; &amp; c&#39;s &#x27;d&#x27;"), "a <b> & c's 'd'");
        assert_eq!(unescape_html("R&D & co &unknown; &"), "R&D & co &unknown; &");
    }

    #[test]
    fn reads_stats() {
        let s = parse_stats(&json!({
            "running": true, "crashed": false, "waiting_start": false, "updating": false,
            "cpu": 12.5, "mem": "4.1GB", "mem_percent": 25.0, "online": 2, "max": 20,
            "players": "['Alex', 'Steve']", "version": "1.21.1", "world_size": "1.2GB",
            "started": "2026-10-12 10:00:00"
        }));
        assert!(s.running);
        assert_eq!(s.state(), "Running");
        assert_eq!(s.players, vec!["Alex", "Steve"]);
        assert_eq!((s.online, s.max), (2, 20));
        assert_eq!(parse_stats(&json!({ "crashed": true })).state(), "Crashed");
        assert_eq!(s.mem, "4.1GB");
        assert_eq!(parse_stats(&json!({ "mem": "17668972544.0" })).mem, "16.5 GB");
        assert_eq!(parse_stats(&json!({ "mem": 17668972544.0 })).mem, "16.5 GB");
    }

    #[test]
    fn errors_say_what_to_do() {
        assert!(unwrap(200, r#"{"status":"ok","data":[1]}"#).unwrap().is_array());
        let e = unwrap(400, r#"{"status":"error","error":"NOT_AUTHORIZED"}"#).unwrap_err();
        assert!(e.to_string().contains("Commands permission"));
        let e = unwrap(200, r#"{"status":"error","error":"SERVER_NOT_RUNNING"}"#).unwrap_err();
        assert!(e.to_string().contains("isn't running"));
        let e = unwrap(403, r#"{"status":"error","error":"ACCESS_DENIED"}"#).unwrap_err();
        assert!(e.to_string().contains("API key"));
    }

    #[test]
    fn reads_backups() {
        let sets = parse_backup_sets(&json!([
            { "backup_id": "b-1", "backup_name": "Nightly" },
            { "backup_id": 7, "backup_name": "" }
        ]));
        assert_eq!(sets.len(), 2);
        assert_eq!(sets[1].id, "7");
        assert_eq!(sets[1].name, "Backup");
        let files = backup_files(&json!({ "total": 2, "backups": [
            "G:/crafty/backups/x/2026-10-01_03-00-00.zip",
            "G:\\crafty\\backups\\x\\2026-10-12_03-00-00.zip" ] }));
        assert_eq!(files, vec!["2026-10-12_03-00-00.zip", "2026-10-01_03-00-00.zip"]);
    }

    #[test]
    fn crafty_launch_follows_neoforge() {
        let d = json!({
            "executable": "libraries/net/neoforged/neoforge/21.1.230/neoforge-21.1.230-server.jar",
            "execution_command": r#""G:\jdk\bin\java.exe" @user_jvm_args.txt @libraries/net/neoforged/neoforge/21.1.256/win_args.txt nogui  %*"#,
        });
        let p = crafty_neoforge_patch(&d, "21.1.260");
        assert_eq!(p["executable"], "libraries/net/neoforged/neoforge/21.1.260/neoforge-21.1.260-server.jar");
        assert_eq!(
            p["execution_command"],
            r#""G:\jdk\bin\java.exe" @user_jvm_args.txt @libraries/net/neoforged/neoforge/21.1.260/win_args.txt nogui  %*"#
        );
        // Already right, or not NeoForge: nothing to change.
        assert!(crafty_neoforge_patch(&d, "21.1.256").get("execution_command").is_none());
        assert!(crafty_neoforge_patch(&json!({"execution_command": "java -jar fabric.jar"}), "21.1.260").is_empty());
    }

    #[test]
    fn actions_map_to_crafty() {
        assert_eq!(crafty_action(PanelAction::Kill), "kill_server");
        assert_eq!(crafty_action(PanelAction::Start), "start_server");
    }
}

