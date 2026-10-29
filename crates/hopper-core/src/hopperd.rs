//! Talking to Hopper Server (`hopperd`, crates/hopperd): finding it on the
//! LAN, pairing, and the calls the desktop app makes for servers it runs:
//! their details and mods, power and console, and file transfers for sync.
//!
//! A server on a Hopper Server shows up in Hopper as a [`ServerInfo`] whose
//! `remote` is set. Its `root` is a stand-in (`hopperd:\<folder>`) that
//! keeps the folder name for matching and pairing; no local file exists
//! there, so anything that reads it directly finds nothing, quickly.

use crate::model::{ModInfo, SyncOp, SyncResult};
use crate::server::{Manager, ServerInfo};
use anyhow::{Context as _, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The panel's default port.
pub const PORT: u16 = 8790;
/// mDNS service hopperd announces.
pub const SERVICE: &str = "_hopperd._tcp.local.";
/// Upload chunk size (hopperd takes up to 32 MB).
const CHUNK: usize = 8 * 1024 * 1024;

/// Which Hopper Server a server is on, and its id there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    /// "https://192.168.50.3:8790"
    pub base: String,
    pub id: String,
    /// The panel's certificate authority (PEM), pinned at pairing.
    pub ca: String,
    /// Which paired panel ([`crate::settings::HopperPanel::id`]).
    pub panel: String,
}

/// Stand-in roots and the panels they're on, so code given only a path
/// (config compare, file copies, crash scan) can reach the server.
static REMOTES: std::sync::LazyLock<std::sync::Mutex<Vec<(PathBuf, Remote)>>> = std::sync::LazyLock::new(Default::default);

pub fn register(root: &Path, remote: &Remote) {
    let mut r = REMOTES.lock().unwrap_or_else(|e| e.into_inner());
    r.retain(|(p, _)| p != root);
    r.push((root.to_path_buf(), remote.clone()));
}

/// The server root `p` is in (or is), and its panel, when it's remote.
pub fn remote_of(p: &Path) -> Option<(PathBuf, Remote)> {
    if !is_remote_root(p) {
        return None;
    }
    REMOTES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(root, _)| p.starts_with(root))
        .cloned()
}

/// `p` inside a remote server: the client, the server's id, and the path
/// relative to its folder.
pub fn locate(p: &Path) -> Option<anyhow::Result<(Client, String, String)>> {
    let (root, r) = remote_of(p)?;
    let rel = remote_rel(&root, p).unwrap_or_default();
    Some(Client::for_remote(&r).map(|c| (c, r.id.clone(), rel)))
}

/// Where the panel's web UI shows `p` (a file opens in its editor, a
/// folder in its file manager), when `p` is on a remote server.
pub fn web_url(p: &Path) -> Option<String> {
    let (root, r) = remote_of(p)?;
    let rel = remote_rel(&root, p).unwrap_or_default();
    let segs: Vec<String> = rel.split('/').filter(|s| !s.is_empty()).map(enc).collect();
    let file = segs.last().is_some_and(|l| l.contains('.'));
    let tab = if file { "edit" } else { "files" };
    let mut url = format!("{}/#/server/{}/{tab}", r.base, enc(&r.id));
    for s in segs {
        url.push('/');
        url.push_str(&s);
    }
    Some(url)
}

/// The stand-in root for a server on a Hopper Server. The first panel's
/// servers keep `hopperd:\<folder>` (pairings are saved by it); others get
/// their panel's id in between (see [`panel_root`]).
pub fn remote_root(folder: &str) -> PathBuf {
    PathBuf::from(format!("hopperd:\\{folder}"))
}

/// [`remote_root`] for a server on panel `panel`.
pub fn panel_root(panel: &str, folder: &str) -> PathBuf {
    if panel.is_empty() { remote_root(folder) } else { PathBuf::from(format!("hopperd:\\@{panel}\\{folder}")) }
}

/// "rig" from "https://rig:8790", for a panel's default name.
pub fn host_name(base: &str) -> String {
    let host = base.split("://").nth(1).unwrap_or(base).split('/').next().unwrap_or("");
    let host = host.rsplit_once(':').map_or(host, |(h, _)| h);
    host.trim_matches(['[', ']']).to_string()
}

pub fn is_remote_root(p: &Path) -> bool {
    p.to_string_lossy().starts_with("hopperd:")
}

/// `https://host:8790` from what was typed ("rig", "rig:8790", a URL).
/// An old `http://` address is moved to HTTPS.
pub fn base_url(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    let url = match url.split_once("://") {
        Some(("http", rest)) | Some(("https", rest)) => format!("https://{rest}"),
        Some(_) => url.to_string(),
        None => format!("https://{url}"),
    };
    let after = url.split("://").nth(1).unwrap_or("");
    let host = after.split('/').next().unwrap_or("");
    // No port given: hopperd's.
    let has_port = host.rsplit_once(':').is_some_and(|(_, p)| p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty());
    if has_port || host.is_empty() { url } else { format!("{url}:{PORT}") }
}

/// The addresses to try, in order, for what was typed. A web address with
/// no port (`https://hopper.example.com`) is most likely a panel behind a
/// reverse proxy on 443, so that comes first, then hopperd's own port.
/// Everything else has one answer: [`base_url`]. An `http://` address with
/// no port stays on hopperd's port, as before: the panel only speaks HTTPS,
/// and a proxy's port 80 just redirects. Each address names its port, so
/// what pairing saves never needs probing again.
pub fn candidates(url: &str) -> Vec<String> {
    let base = base_url(url);
    let t = url.trim();
    let Some(rest) = t.strip_prefix("https://") else { return vec![base] };
    let host = rest.split('/').next().unwrap_or("");
    let has_port = host.rsplit_once(':').is_some_and(|(_, p)| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    if host.is_empty() || has_port {
        return vec![base];
    }
    vec![format!("https://{host}:443"), base]
}

/// Whether a saved panel address and a typed one are the same panel.
pub fn same_panel(saved: &str, typed: &str) -> bool {
    let saved = base_url(saved);
    candidates(typed).iter().any(|c| *c == saved)
}

/// The first of [`candidates`] that `check` accepts, with what it gave.
/// `check` gets how long to wait for the connection to open: short on the
/// first of several tries, so a wrong guess doesn't hold pairing up. The
/// answer itself gets longer ([`ANSWER_WAIT`]): a panel behind a proxy, on
/// a slow link, can open fast and still take a few seconds to reply. When
/// all fail, the error names every address.
fn first_reachable<T>(url: &str, check: impl Fn(&str, Duration) -> anyhow::Result<T>) -> anyhow::Result<(String, T)> {
    let tries = candidates(url);
    if tries.len() == 1 {
        let base = tries.into_iter().next().unwrap_or_default();
        return check(&base, Duration::from_secs(5)).map(|v| (base, v));
    }
    let mut errors = Vec::new();
    for (i, base) in tries.iter().enumerate() {
        let wait = Duration::from_secs(if i + 1 < tries.len() { 3 } else { 5 });
        match check(base, wait) {
            Ok(v) => return Ok((base.clone(), v)),
            Err(e) => errors.push(format!("{base}: {e:#}")),
        }
    }
    bail!("No Hopper Server found at {}. Tried {}", tries.join(" or "), errors.join("; "))
}

/// Whether `base`'s host is a LAN, Tailscale or local name: the panel's
/// own certificate covers those. A public domain has a Let's Encrypt one.
fn private_host(base: &str) -> bool {
    let host = base.split("://").nth(1).unwrap_or("").split('/').next().unwrap_or("");
    let host = host.rsplit_once(':').map_or(host, |(h, p)| if p.chars().all(|c| c.is_ascii_digit()) { h } else { host });
    let host = host.trim_matches(['[', ']']);
    host.parse::<std::net::IpAddr>().is_ok() || !host.contains('.') || host.ends_with(".local") || host.ends_with(".ts.net")
}

/// How to trust the panel's certificate.
#[derive(Clone)]
enum Trust {
    /// Its own CA (PEM), pinned at pairing.
    Pinned(String),
    /// The normal web roots (Let's Encrypt on a public domain).
    Web,
    /// Anything: only to fetch the CA before pairing.
    Unverified,
}

fn trust_for(base: &str, ca: &str) -> Trust {
    if !ca.trim().is_empty() && private_host(base) { Trust::Pinned(ca.to_string()) } else { Trust::Web }
}

/// How long one pairing request may take, start to finish.
const ANSWER_WAIT: Duration = Duration::from_secs(15);

fn agent_with(timeout: Duration, trust: &Trust) -> ureq::Agent {
    agent_timed(None, timeout, trust)
}

fn agent_timed(connect: Option<Duration>, timeout: Duration, trust: &Trust) -> ureq::Agent {
    use ureq::tls::{Certificate, RootCerts, TlsConfig};
    let tls = match trust {
        Trust::Pinned(pem) => match Certificate::from_pem(pem.as_bytes()) {
            Ok(c) => TlsConfig::builder().root_certs(RootCerts::Specific(std::sync::Arc::new(vec![c]))).build(),
            // A broken pin trusts nothing rather than everything.
            Err(_) => TlsConfig::builder().root_certs(RootCerts::Specific(std::sync::Arc::new(Vec::new()))).build(),
        },
        Trust::Web => TlsConfig::builder().root_certs(RootCerts::WebPki).build(),
        Trust::Unverified => TlsConfig::builder().disable_verification(true).build(),
    };
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .timeout_connect(connect)
        .http_status_as_error(false)
        .tls_config(tls)
        .build()
        .into()
}

/// "AB:CD:…" of a PEM certificate, worked out here (never taken from what
/// the panel says about itself).
pub fn fingerprint_of(pem: &str) -> Option<String> {
    use base64::Engine as _;
    let b64: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    let der = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let hex = crate::javadl::sha256_hex(&der);
    Some(hex.as_bytes().chunks(2).map(|c| String::from_utf8_lossy(c).to_uppercase()).collect::<Vec<_>>().join(":"))
}

/// A paired Hopper Server.
pub struct Client {
    pub base: String,
    /// Which paired panel ([`crate::settings::HopperPanel::id`]).
    pub panel: String,
    token: String,
    ca: String,
    agent: ureq::Agent,
    /// For file transfers, which take as long as they take.
    slow: ureq::Agent,
}

/// The reply's JSON, or its `error` as the error.
fn reply(resp: Result<ureq::http::Response<ureq::Body>, ureq::Error>, base: &str) -> anyhow::Result<Value> {
    let mut resp = resp.with_context(|| format!("can't reach Hopper Server at {base}"))?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    match status {
        200..=299 => Ok(v),
        401 => bail!("Hopper Server doesn't know this PC any more. Pair again in Settings → Server panel."),
        _ => bail!("{}", v["error"].as_str().map(str::to_string).unwrap_or_else(|| format!("Hopper Server answered {status}"))),
    }
}

impl Client {
    /// `ca`: the panel's certificate authority, pinned at pairing.
    pub fn new(url: &str, token: &str, ca: &str) -> Self {
        let base = base_url(url);
        let trust = trust_for(&base, ca);
        Client {
            agent: agent_with(Duration::from_secs(15), &trust),
            slow: agent_with(Duration::from_secs(3600), &trust),
            base,
            panel: String::new(),
            token: token.to_string(),
            ca: ca.to_string(),
        }
    }

    /// A paired panel, when its token is saved.
    pub fn for_panel(p: &crate::settings::HopperPanel) -> Option<Client> {
        let url = if p.id.is_empty() {
            std::env::var("HOPPER_PANEL_URL").ok().filter(|u| !u.trim().is_empty()).unwrap_or_else(|| p.url.clone())
        } else {
            p.url.clone()
        };
        if url.trim().is_empty() {
            return None;
        }
        let mut c = Client::new(&url, &crate::panel::hopperd_token_for(&p.id)?, &p.ca);
        c.panel = p.id.clone();
        Some(c)
    }

    /// The token, to make a [`crate::panel::HopperServer`] from this client.
    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn ca(&self) -> &str {
        &self.ca
    }

    /// The first paired panel.
    pub fn from_settings(s: &crate::settings::Settings) -> Option<Client> {
        s.hopper_panels.first().and_then(Client::for_panel)
    }

    /// For a server on this panel.
    pub fn for_remote(r: &Remote) -> anyhow::Result<Client> {
        let token =
            crate::panel::hopperd_token_for(&r.panel).context("Pair with the Hopper Server first (Settings → Servers).")?;
        let mut c = Client::new(&r.base, &token, &r.ca);
        c.panel = r.panel.clone();
        Ok(c)
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1{path}", self.base)
    }

    fn auth(&self) -> String {
        format!("Bearer {}", self.token)
    }

    pub fn get(&self, path: &str) -> anyhow::Result<Value> {
        reply(self.agent.get(&self.url(path)).header("Authorization", &self.auth()).call(), &self.base)
    }

    pub fn post(&self, path: &str, body: Value) -> anyhow::Result<Value> {
        reply(self.agent.post(&self.url(path)).header("Authorization", &self.auth()).send_json(body), &self.base)
    }

    pub fn put(&self, path: &str, body: Value) -> anyhow::Result<Value> {
        reply(self.agent.put(&self.url(path)).header("Authorization", &self.auth()).send_json(body), &self.base)
    }

    pub fn delete(&self, path: &str) -> anyhow::Result<Value> {
        reply(self.agent.delete(&self.url(path)).header("Authorization", &self.auth()).call(), &self.base)
    }

    /// Unpairs this PC in the panel, so it leaves Access & devices and the
    /// token stops working there. Panels before this route answer 403.
    pub fn unpair(&self) -> anyhow::Result<()> {
        self.delete("/devices/me").map(|_| ())
    }

    /// Starts a job with a POST and follows it to the end (see [`Client::follow`]).
    pub fn run_job(&self, path: &str, body: Value, job: &crate::jobs::Job) -> anyhow::Result<Value> {
        let v = self.post(path, body)?;
        let id = v["job"].as_u64().context("the panel didn't start a job")?;
        self.follow(id, job)
    }

    /// `/servers/{id}` + `rest`, with the id encoded.
    pub fn server_path(id: &str, rest: &str) -> String {
        format!("/servers/{}{rest}", enc(id))
    }

    /// `/fs/{id}` + `rest`, with the id encoded.
    pub fn fs_path(id: &str, rest: &str) -> String {
        format!("/fs/{}{rest}", enc(id))
    }

    // ---- servers -------------------------------------------------------------

    /// Every server on the panel, as Hopper's [`ServerInfo`].
    pub fn server_infos(&self) -> anyhow::Result<Vec<ServerInfo>> {
        let list = self.get("/servers/infos")?;
        Ok(list
            .as_array()
            .into_iter()
            .flatten()
            .map(|v| info_from(v, &self.base, &self.ca, &self.panel))
            .collect())
    }

    /// A server's mods, read by the panel itself (no jar crosses the wire).
    pub fn mods(&self, id: &str) -> anyhow::Result<Vec<ModInfo>> {
        let v = self.get(&format!("/servers/{}/mods", enc(id)))?;
        Ok(serde_json::from_value(v["mods"].clone())?)
    }

    /// The server's last crash, read by the panel from its logs.
    pub fn crash(&self, id: &str) -> anyhow::Result<Option<crate::crashdetect::Crash>> {
        let v = self.get(&format!("/servers/{}/crash", enc(id)))?["crash"].clone();
        // Hopper Server before 0.14 sends its old scan: say it crashed, with
        // its headline, but no mods (that scan's guesses were often wrong).
        if v.get("hits").is_some() {
            return Ok(Some(crate::crashdetect::Crash {
                headline: v["headline"].as_str().unwrap_or_default().to_string(),
                server: true,
                ..Default::default()
            }));
        }
        Ok(serde_json::from_value(v)?)
    }

    /// Puts back the older copy of the mod the server's last crash names
    /// (`file`: the jar there now), as [`crate::crashdetect::put_back`].
    pub fn crash_put_back(&self, id: &str, file: &str) -> anyhow::Result<()> {
        self.post(&format!("/servers/{}/crash/put-back", enc(id)), serde_json::json!({ "file": file }))?;
        Ok(())
    }

    /// The NeoForge version the server's run.bat starts.
    pub fn neoforge(&self, id: &str) -> anyhow::Result<Option<String>> {
        Ok(self.get(&format!("/servers/{}/neoforge", enc(id)))?["version"].as_str().map(str::to_string))
    }

    /// Every file under `folder` (relative to the server folder) as (path
    /// inside the folder, size, modified time). Empty when it's missing.
    pub fn walk(&self, id: &str, folder: &str) -> anyhow::Result<Vec<(String, u64, u64)>> {
        let v = self.get(&format!("/fs/{}/walk?path={}", enc(id), enc(folder)))?;
        Ok(serde_json::from_value(v["files"].clone())?)
    }

    /// Content hashes of files (paths relative to the server folder).
    pub fn hashes(&self, id: &str, rels: &[String]) -> anyhow::Result<std::collections::HashMap<String, String>> {
        let v = self.post(&format!("/fs/{}/hashes", enc(id)), json!({ "paths": rels }))?;
        Ok(serde_json::from_value(v["hashes"].clone())?)
    }

    /// A text file's content.
    pub fn read_text(&self, id: &str, rel: &str) -> anyhow::Result<String> {
        let v = self.get(&format!("/fs/{}/text?path={}", enc(id), enc(rel)))?;
        Ok(v["content"].as_str().unwrap_or("").to_string())
    }

    /// Which of `names` are folders in the server folder.
    pub fn dirs(&self, id: &str, names: &[&str]) -> anyhow::Result<Vec<String>> {
        let v = self.get(&format!("/fs/{}/dirs?names={}", enc(id), enc(&names.join(","))))?;
        Ok(serde_json::from_value(v["dirs"].clone())?)
    }

    // ---- jobs on the panel ---------------------------------------------------------

    /// Follows a job the panel runs until it ends, showing its detail and
    /// progress in `job`; cancelling `job` cancels it there. Returns its result.
    pub fn follow(&self, remote_job: u64, job: &crate::jobs::Job) -> anyhow::Result<Value> {
        let mut cancelled = false;
        loop {
            std::thread::sleep(Duration::from_millis(400));
            if job.is_cancelled() && !cancelled {
                cancelled = true;
                let _ = self.post(&format!("/jobs/{remote_job}/cancel"), json!({}));
            }
            let v = self.get(&format!("/jobs/{remote_job}"))?;
            if let Some(d) = v["detail"].as_str().filter(|d| !d.is_empty()) {
                job.detail(d);
            }
            if let Some(p) = v["progress"].as_f64() {
                job.progress((p * 1000.0) as usize, 1000);
            }
            if v["done"].as_bool() == Some(true) {
                if let Some(e) = v["error"].as_str() {
                    bail!("{e}");
                }
                return Ok(v["result"].clone());
            }
        }
    }

    /// Plans a server pack update on the panel (it downloads the server
    /// pack and compares it with the server's files there).
    pub fn pack_plan(
        &self,
        id: &str,
        inst: &crate::instances::Instance,
        pack: &crate::cf::File,
        compare: bool,
        job: &crate::jobs::Job,
    ) -> anyhow::Result<crate::server_pack_update::ServerPlan> {
        let v = self.post(
            &format!("/servers/{}/pack/plan", enc(id)),
            json!({ "inst": inst, "pack": pack, "compare": compare, "cf_key": crate::cf::api_key().unwrap_or(""), "keep_days": keep_days() }),
        )?;
        let result = self.follow(v["job"].as_u64().context("no job id")?, job)?;
        Ok(serde_json::from_value(result)?)
    }

    /// Applies a server pack update on the panel.
    pub fn pack_apply(
        &self,
        id: &str,
        plan: &crate::server_pack_update::ServerPlan,
        take: &std::collections::HashSet<String>,
        job: &crate::jobs::Job,
    ) -> anyhow::Result<crate::server_pack_update::Report> {
        let v = self.post(
            &format!("/servers/{}/pack/apply", enc(id)),
            json!({ "plan": plan, "take": take, "cf_key": crate::cf::api_key().unwrap_or(""), "keep_days": keep_days() }),
        )?;
        let result = self.follow(v["job"].as_u64().context("no job id")?, job)?;
        Ok(serde_json::from_value(result)?)
    }

    // ---- files -----------------------------------------------------------------

    /// Uploads `local` to `rel` in the server folder, replacing what is
    /// there (the panel keeps the old file in its trash).
    pub fn upload(&self, id: &str, local: &Path, rel: &str) -> anyhow::Result<()> {
        let mut f = std::fs::File::open(local).with_context(|| format!("opening {}", local.display()))?;
        let size = f.metadata()?.len();
        let start = self.post(&format!("/fs/{}/uploads", enc(id)), json!({ "path": rel, "size": size, "overwrite": true }))?;
        let up = start["id"].as_str().context("no upload id")?.to_string();
        let mut offset = 0u64;
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = read_full(&mut f, &mut buf)?;
            let url = self.url(&format!("/fs/{}/uploads/{up}?offset={offset}", enc(id)));
            let v = reply(
                self.slow
                    .put(&url)
                    .header("Authorization", &self.auth())
                    .content_type("application/octet-stream")
                    .send(&buf[..n]),
                &self.base,
            )?;
            offset = v["offset"].as_u64().unwrap_or(offset + n as u64);
            if v["done"].as_bool() == Some(true) || n == 0 {
                return Ok(());
            }
        }
    }

    /// Downloads `rel` from the server folder to `local`.
    pub fn download(&self, id: &str, rel: &str, local: &Path) -> anyhow::Result<()> {
        let url = self.url(&format!("/fs/{}/file?path={}", enc(id), enc(rel)));
        let mut resp = self
            .slow
            .get(&url)
            .header("Authorization", &self.auth())
            .call()
            .with_context(|| format!("can't reach Hopper Server at {}", self.base))?;
        if !resp.status().is_success() {
            let text = resp.body_mut().read_to_string().unwrap_or_default();
            let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            bail!("{}", v["error"].as_str().unwrap_or("download failed"));
        }
        if let Some(p) = local.parent() {
            std::fs::create_dir_all(p)?;
        }
        let tmp = local.with_extension("hopper-download");
        let mut out = std::fs::File::create(&tmp)?;
        let mut body = resp.body_mut().with_config().limit(u64::MAX).reader();
        std::io::copy(&mut body, &mut out)?;
        out.flush()?;
        drop(out);
        std::fs::rename(&tmp, local)?;
        Ok(())
    }

    /// Moves files on the server to its trash.
    pub fn trash(&self, id: &str, rels: &[String]) -> anyhow::Result<()> {
        self.post(&format!("/fs/{}/ops", enc(id)), json!({ "ops": [{ "op": "trash", "paths": rels }] }))?;
        Ok(())
    }
}

fn read_full(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        let r = f.read(&mut buf[n..])?;
        if r == 0 {
            break;
        }
        n += r;
    }
    Ok(n)
}

/// `s` percent-encoded for a URL path segment or query value.
pub fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The desktop's "keep downloaded packs" days, sent with pack jobs so the
/// panel prunes its own cache the same way.
fn keep_days() -> u32 {
    crate::settings::Settings::load().map_or(30, |s| s.pack_keep_days)
}

/// Twins found so far: a local (share) server root and the same folder on a
/// Hopper Server.
static TWINS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<PathBuf, ServerInfo>>> =
    std::sync::LazyLock::new(Default::default);

/// `server` reached through a share (like `Q:\Minecraft\packs\atm10`) as
/// the same folder on one of `panels`, so work that downloads (server pack
/// updates) runs on the server's PC instead of over the share: same folder
/// name and the same `server.properties`. `None` when it isn't on one, or
/// no panel answers.
pub fn twin(server: &ServerInfo, panels: &[crate::settings::HopperPanel]) -> Option<ServerInfo> {
    if server.remote.is_some() || panels.is_empty() {
        return None;
    }
    if let Some(t) = TWINS.lock().unwrap_or_else(|e| e.into_inner()).get(&server.root) {
        return Some(t.clone());
    }
    let folder = server.root.file_name()?.to_string_lossy().to_lowercase();
    let props = "server.properties".to_string();
    let ours = crate::configdiff::file_hash(&server.root.join(&props))?;
    for p in panels {
        let Some(c) = Client::for_panel(p) else { continue };
        let Ok(infos) = c.server_infos() else { continue };
        for info in infos {
            let same_name = info.root.file_name().is_some_and(|f| f.to_string_lossy().to_lowercase() == folder);
            let Some(r) = info.remote.as_ref().filter(|_| same_name) else { continue };
            let theirs = c.hashes(&r.id, std::slice::from_ref(&props)).ok().and_then(|h| h.get(&props).cloned());
            if theirs.as_deref() == Some(ours.as_str()) {
                TWINS.lock().unwrap_or_else(|e| e.into_inner()).insert(server.root.clone(), info.clone());
                return Some(info);
            }
        }
    }
    None
}

fn info_from(v: &Value, base: &str, ca: &str, panel: &str) -> ServerInfo {
    let s = |k: &str| v[k].as_str().map(str::to_string);
    let name = s("name").unwrap_or_default();
    let folder = s("path")
        .and_then(|p| p.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().map(str::to_string))
        .filter(|f| !f.is_empty())
        .unwrap_or_else(|| name.clone());
    let properties: BTreeMap<String, String> = serde_json::from_value(v["properties"].clone()).unwrap_or_default();
    let root = panel_root(panel, &folder);
    let remote = Remote { base: base.to_string(), id: s("id").unwrap_or_default(), ca: ca.to_string(), panel: panel.to_string() };
    register(&root, &remote);
    ServerInfo {
        name,
        root,
        manager: match v["managed_by"].as_str() {
            Some("Crafty") => Manager::Crafty,
            Some("MCSS") => Manager::Mcss,
            _ => Manager::Unknown,
        },
        properties,
        loader: s("loader"),
        java: s("java"),
        max_memory: s("max_memory"),
        remote: Some(remote),
    }
}

// ---- running sync operations on a remote server --------------------------------

/// `p` relative to the stand-in root, with forward slashes.
fn remote_rel(root: &Path, p: &Path) -> Option<String> {
    let rel = p.strip_prefix(root).ok()?;
    Some(rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/"))
}

/// Runs sync operations where one side is a remote server: copies to it
/// upload, copies from it download (through the local side's backups),
/// deletes on it go to the panel's trash.
pub fn execute(ops: Vec<SyncOp>, remote: &Remote, root: &Path, local_root: &Path) -> Vec<SyncResult> {
    let client = match Client::for_remote(remote) {
        Ok(c) => c,
        Err(e) => {
            return ops
                .into_iter()
                .map(|op| SyncResult { op, ok: false, error: Some(format!("{e:#}")), backup: None })
                .collect();
        }
    };
    let mut out = Vec::new();
    for op in ops {
        let r: anyhow::Result<()> = match &op {
            SyncOp::Copy { src, dst } if is_remote_root(dst) => match remote_rel(root, dst) {
                Some(rel) => client.upload(&remote.id, src, &rel),
                None => Err(anyhow::anyhow!("{} isn't on this server", dst.display())),
            },
            SyncOp::Copy { src, dst } if is_remote_root(src) => match remote_rel(root, src) {
                // Through a temp file and the local backups, like a copy.
                Some(rel) => {
                    let tmp = std::env::temp_dir().join(format!("hopper-pull-{}", std::process::id()));
                    let staged = tmp.join(src.file_name().unwrap_or_default());
                    let r = client.download(&remote.id, &rel, &staged).and_then(|_| {
                        let res = crate::sync::execute_local(vec![SyncOp::Copy { src: staged.clone(), dst: dst.clone() }], local_root);
                        match res.into_iter().next() {
                            Some(r) if r.ok => Ok(()),
                            Some(r) => Err(anyhow::anyhow!(r.error.unwrap_or_default())),
                            None => Ok(()),
                        }
                    });
                    let _ = std::fs::remove_dir_all(&tmp);
                    r
                }
                None => Err(anyhow::anyhow!("{} isn't on this server", src.display())),
            },
            SyncOp::Delete { path } if is_remote_root(path) => match remote_rel(root, path) {
                Some(rel) => client.trash(&remote.id, &[rel]),
                None => Err(anyhow::anyhow!("{} isn't on this server", path.display())),
            },
            // Local-only operations (removing the client's old jar on a pull).
            _ => {
                let res = crate::sync::execute_local(vec![op.clone()], local_root);
                out.extend(res);
                continue;
            }
        };
        out.push(match r {
            Ok(()) => SyncResult { op, ok: true, error: None, backup: None },
            Err(e) => SyncResult { op, ok: false, error: Some(format!("{e:#}")), backup: None },
        });
    }
    out
}

// ---- finding and pairing ----------------------------------------------------------

/// A panel found on the LAN.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub name: String,
    pub url: String,
    pub version: String,
}

/// Looks for Hopper Servers on the LAN for up to `wait`.
pub fn discover(wait: Duration) -> Vec<Found> {
    let Ok(mdns) = mdns_sd::ServiceDaemon::new() else { return Vec::new() };
    let Ok(rx) = mdns.browse(SERVICE) else { return Vec::new() };
    let end = std::time::Instant::now() + wait;
    let mut out: Vec<Found> = Vec::new();
    while let Some(left) = end.checked_duration_since(std::time::Instant::now()) {
        if let Ok(mdns_sd::ServiceEvent::ServiceResolved(info)) = rx.recv_timeout(left) {
            // One entry per panel, on its likeliest LAN address.
            let mut addrs: Vec<std::net::IpAddr> = info.get_addresses().iter().copied().filter(|a| a.is_ipv4()).collect();
            addrs.sort_by_key(addr_rank);
            if let Some(ip) = addrs.first() {
                let name = info.get_fullname().trim_end_matches(SERVICE).trim_end_matches('.').to_string();
                let found = Found {
                    name: name.clone(),
                    url: format!("http://{ip}:{}", info.get_port()),
                    version: info.get_property_val_str("version").unwrap_or("").to_string(),
                };
                match out.iter_mut().find(|f| f.name == name) {
                    Some(f) if addr_rank(ip) < url_rank(&f.url) => *f = found,
                    Some(_) => {}
                    None => out.push(found),
                }
            }
        }
    }
    let _ = mdns.shutdown();
    out
}

/// Home LAN first, then other private ranges, Tailscale, the rest.
fn addr_rank(a: &std::net::IpAddr) -> u8 {
    match a {
        std::net::IpAddr::V4(v) => match v.octets() {
            [192, 168, ..] => 0,
            [10, ..] => 1,
            [172, 16..=31, ..] => 2,
            [100, 64..=127, ..] => 3,
            _ => 4,
        },
        std::net::IpAddr::V6(_) => 5,
    }
}

fn url_rank(url: &str) -> u8 {
    url.split("://")
        .nth(1)
        .and_then(|h| h.split(':').next())
        .and_then(|h| h.parse::<std::net::IpAddr>().ok())
        .map_or(9, |a| addr_rank(&a))
}

#[derive(Debug, Deserialize)]
pub struct Hello {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub tls: Option<HelloTls>,
}

#[derive(Debug, Deserialize)]
pub struct HelloTls {
    pub fingerprint: String,
    pub ca: String,
    pub domain: Option<String>,
}

/// The panel's name, version and certificate authority. Unverified: only
/// to learn the CA, which pairing then pins and the codes confirm.
pub fn hello(url: &str) -> anyhow::Result<Hello> {
    hello_at(&base_url(url), Duration::from_secs(5))
}

/// `connect`: how long to wait for the connection to open.
fn hello_at(base: &str, connect: Duration) -> anyhow::Result<Hello> {
    let agent = agent_timed(Some(connect), ANSWER_WAIT, &Trust::Unverified);
    let v = reply(agent.get(&format!("{base}/api/v1/hello")).call(), base)?;
    if v["panel"].as_str() != Some("hopperd") {
        bail!("{base} isn't a Hopper Server.");
    }
    Ok(serde_json::from_value(v)?)
}

/// [`hello`] for a typed address, trying each of [`candidates`]: the
/// address that answered (port included) and what it said.
pub fn find(url: &str) -> anyhow::Result<(String, Hello)> {
    first_reachable(url, hello_at)
}

/// A pairing request waiting for the owner.
#[derive(Clone, Debug)]
pub struct Pairing {
    pub base: String,
    pub id: String,
    pub secret: String,
    /// The 6-digit code, worked out here from the panel's certificate; the
    /// panel shows its own, and they only match for the real panel.
    pub code: String,
    /// The panel's CA (PEM), to keep once paired; empty when a LAN name's
    /// certificate turned out to be a proxy's public one, so the saved
    /// panel is checked with the web roots instead of a pin.
    pub ca: String,
    pub fingerprint: String,
    /// One agent for the whole wait, so the polls reuse one connection
    /// instead of opening a new one through the proxy every second.
    agent: ureq::Agent,
}

/// Whether an error is the certificate being refused (rather than the
/// panel being slow or away).
fn cert_rejected(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        let s = c.to_string().to_lowercase();
        s.contains("certificate") || s.contains("unknownissuer") || s.contains("notvalidforname")
    })
}

/// Asks the panel to pair this PC, named `device`. Everything after
/// learning the CA goes over a checked connection: pinned to that CA on a
/// LAN address, the web roots on a public name. A LAN or Tailscale name
/// whose certificate isn't the panel's own (a proxy in front of it, such as
/// `tailscale serve`) is checked with the web roots instead; if those don't
/// accept it either, pairing stops.
pub fn pair_request(url: &str, device: &str) -> anyhow::Result<Pairing> {
    let (base, h) = find(url)?;
    let tls = h.tls.context("This Hopper Server is too old for encrypted pairing; update it.")?;
    let fingerprint = fingerprint_of(&tls.ca).context("The panel sent a broken certificate.")?;
    let ask = |trust: &Trust| {
        let agent = agent_with(ANSWER_WAIT, trust);
        let v = reply(agent.post(&format!("{base}/api/v1/pair/request")).send_json(json!({ "device": device })), &base);
        v.map(|v| (v, agent))
    };
    let first = trust_for(&base, &tls.ca);
    let (v, agent, ca) = match ask(&first) {
        Ok((v, a)) => (v, a, tls.ca),
        Err(e) if matches!(first, Trust::Pinned(_)) && cert_rejected(&e) => match ask(&Trust::Web) {
            Ok((v, a)) => (v, a, String::new()),
            Err(_) => return Err(e.context(format!("{base} answered with a certificate that isn't the panel's"))),
        },
        Err(e) if cert_rejected(&e) => return Err(e.context(format!("{base}'s certificate didn't check out"))),
        Err(e) => return Err(e.context("Couldn't send the pairing request")),
    };
    let (id, secret) = (v["id"].as_str().unwrap_or("").to_string(), v["secret"].as_str().unwrap_or("").to_string());
    if id.is_empty() || secret.is_empty() {
        bail!("{base} didn't take the pairing request.");
    }
    Ok(Pairing { code: pair_code(&fingerprint, &id, &secret), base, id, secret, ca, fingerprint, agent })
}

#[derive(Debug, PartialEq)]
pub enum PairState {
    Waiting,
    /// The token to keep.
    Approved(String),
    Denied,
    Expired,
}

/// Where a pairing request stands.
pub fn pair_status(p: &Pairing) -> anyhow::Result<PairState> {
    let v = reply(
        p.agent.post(&format!("{}/api/v1/pair/status", p.base)).send_json(json!({ "id": p.id, "secret": p.secret })),
        &p.base,
    )?;
    Ok(match v["status"].as_str() {
        Some("waiting") => PairState::Waiting,
        Some("approved") => PairState::Approved(v["token"].as_str().unwrap_or("").into()),
        Some("denied") => PairState::Denied,
        _ => PairState::Expired,
    })
}

/// How long the owner has to allow a request (the panel forgets it then).
pub const PAIR_WAIT: Duration = Duration::from_secs(300);
/// How long the panel may stay out of reach while waiting before pairing
/// gives up. One lost poll on a slow link or through a proxy is normal.
const PAIR_OUT_OF_REACH: Duration = Duration::from_secs(60);

/// Waits for the owner to answer `p`, asking every second or so. Polls that
/// fail are tried again; pairing only stops when the panel has been out of
/// reach for a minute. Returns `Approved`, `Denied` or `Expired`.
pub fn wait_for_answer(p: &Pairing) -> anyhow::Result<PairState> {
    wait_with(|| pair_status(p), std::thread::sleep, Instant::now, PAIR_WAIT)
}

/// [`wait_for_answer`] with the poll, sleep and clock passed in.
fn wait_with(
    mut poll: impl FnMut() -> anyhow::Result<PairState>,
    mut sleep: impl FnMut(Duration),
    now: impl Fn() -> Instant,
    wait: Duration,
) -> anyhow::Result<PairState> {
    let start = now();
    let mut reached = start;
    let mut failed: Option<anyhow::Error> = None;
    loop {
        sleep(if failed.is_some() { Duration::from_secs(3) } else { Duration::from_secs(1) });
        match poll() {
            Ok(PairState::Waiting) if now().duration_since(start) < wait => {
                reached = now();
                failed = None;
            }
            Ok(PairState::Waiting) => return Ok(PairState::Expired),
            // The panel forgot the request right after a lost answer: it
            // may have been the approval (panels before 0.13.14 hand the
            // token out only once).
            Ok(PairState::Expired) if failed.is_some() => bail!(
                "Lost the panel's answer: it may have allowed this PC already. Remove this PC from Access & devices in the panel, then pair again."
            ),
            Ok(s) => return Ok(s),
            Err(e) => {
                if now().duration_since(reached) >= PAIR_OUT_OF_REACH || now().duration_since(start) >= wait {
                    return Err(e.context("Lost the connection to the panel while waiting for it to allow this PC"));
                }
                failed = Some(e);
            }
        }
    }
}

/// The 6-digit pairing code: from the panel's certificate fingerprint and
/// the request, so both sides only show the same code when the desktop
/// talks to the real panel (a machine in the middle has another
/// certificate, so the codes differ).
pub fn pair_code(fingerprint: &str, id: &str, secret: &str) -> String {
    let h = crate::javadl::sha256_hex(format!("{}|{id}|{secret}", fingerprint.to_uppercase()).as_bytes());
    let n = u32::from_str_radix(&h[..8], 16).unwrap_or(0);
    format!("{:06}", n % 1_000_000)
}

/// "Hopper on FOX".
pub fn device_name() -> String {
    let host = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "this PC".into());
    format!("Hopper on {host}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Against the real panel and share named in `HOPPER_TWIN_ROOT`: read
    /// only (lists servers, hashes server.properties).
    #[test]
    #[ignore]
    fn twin_live() {
        let Ok(root) = std::env::var("HOPPER_TWIN_ROOT") else { return };
        let s = crate::settings::Settings::load().expect("settings");
        let server = crate::server::load(Path::new(&root));
        let t = twin(&server, &s.hopper_panels);
        println!("twin of {root}: {:?}", t.as_ref().map(|t| (t.root.clone(), t.remote.as_ref().map(|r| r.id.clone()))));
        assert!(t.is_some());
    }

    #[test]
    fn urls() {
        assert_eq!(base_url("rig"), "https://rig:8790");
        assert_eq!(base_url("192.168.50.3:9000/"), "https://192.168.50.3:9000");
        assert_eq!(base_url("https://panel.example.com"), "https://panel.example.com:8790");
        assert_eq!(base_url("http://rig:8790"), "https://rig:8790");
        assert_eq!(base_url("https://panel.example.com:443"), "https://panel.example.com:443");
        for (b, private) in [
            ("https://rig:8790", true),
            ("https://192.168.50.3:8790", true),
            ("https://rig.tail3c9e.ts.net:8790", true),
            ("https://fox.local:8790", true),
            ("https://panel.example.com", false),
        ] {
            assert_eq!(private_host(b), private, "{b}");
        }
        assert_eq!(pair_code("aa:bb", "1", "2"), pair_code("AA:BB", "1", "2"));
        assert_ne!(pair_code("AA:BB", "1", "2"), pair_code("AA:BC", "1", "2"));
        assert_eq!(pair_code("AA", "1", "2").len(), 6);
        assert_eq!(enc("config/a b.toml"), "config%2Fa%20b.toml");
    }

    #[test]
    fn address_candidates() {
        // A web address with no port: the proxy's 443 first, then hopperd's.
        for typed in ["https://hopper.example.com", "https://hopper.example.com/", " https://hopper.example.com/ "] {
            assert_eq!(candidates(typed), ["https://hopper.example.com:443", "https://hopper.example.com:8790"], "{typed:?}");
        }
        // Everything else: one address, as before.
        assert_eq!(candidates("rig"), ["https://rig:8790"]);
        assert_eq!(candidates("rig:9000"), ["https://rig:9000"]);
        assert_eq!(candidates("192.168.50.3"), ["https://192.168.50.3:8790"]);
        assert_eq!(candidates("https://hopper.example.com:8443"), ["https://hopper.example.com:8443"]);
        assert_eq!(candidates("http://hopper.example.com"), ["https://hopper.example.com:8790"]);
        assert_eq!(candidates("http://rig:8791"), ["https://rig:8791"]);

        assert!(same_panel("https://hopper.example.com:443", "https://hopper.example.com"));
        assert!(same_panel("https://hopper.example.com:8790", "https://hopper.example.com/"));
        assert!(same_panel("https://rig:8790", "rig"));
        assert!(!same_panel("https://rig:8791", "rig"));
    }

    #[test]
    fn probe_falls_back_to_panel_port() {
        use std::cell::RefCell;
        let tried = RefCell::new(Vec::new());
        // 443 answers: no second try, and the saved address names the port.
        let r = first_reachable("https://hopper.example.com", |b, t| {
            tried.borrow_mut().push((b.to_string(), t));
            Ok(1)
        });
        assert_eq!(r.unwrap(), ("https://hopper.example.com:443".to_string(), 1));
        assert_eq!(tried.borrow().len(), 1);
        assert!(tried.borrow()[0].1 < Duration::from_secs(5));

        // 443 times out: 8790 is tried and kept.
        tried.borrow_mut().clear();
        let r = first_reachable("https://hopper.example.com", |b, t| {
            tried.borrow_mut().push((b.to_string(), t));
            if b.ends_with(":443") { bail!("timeout: global") } else { Ok(2) }
        });
        assert_eq!(r.unwrap(), ("https://hopper.example.com:8790".to_string(), 2));
        assert_eq!(tried.borrow().len(), 2);

        // Both fail: the error names both addresses.
        let e = first_reachable("https://hopper.example.com", |b, _| -> anyhow::Result<()> { bail!("{b} isn't a Hopper Server.") })
            .unwrap_err()
            .to_string();
        assert!(e.contains("https://hopper.example.com:443") && e.contains("https://hopper.example.com:8790"), "{e}");

        // One address: one try, its error unchanged.
        tried.borrow_mut().clear();
        let e = first_reachable("rig", |b, t| -> anyhow::Result<()> {
            tried.borrow_mut().push((b.to_string(), t));
            bail!("timeout: global")
        })
        .unwrap_err();
        assert_eq!(e.to_string(), "timeout: global");
        assert_eq!(*tried.borrow(), [("https://rig:8790".to_string(), Duration::from_secs(5))]);
    }

    /// Runs [`wait_with`] over scripted poll results on a fake clock; each
    /// poll takes `poll_takes`. Returns the outcome and how many polls ran.
    fn wait_script(script: Vec<anyhow::Result<PairState>>, poll_takes: Duration) -> (anyhow::Result<PairState>, usize) {
        use std::cell::{Cell, RefCell};
        let t0 = Instant::now();
        let clock = Cell::new(t0);
        let script = RefCell::new(script.into_iter());
        let polls = Cell::new(0);
        let out = wait_with(
            || {
                polls.set(polls.get() + 1);
                clock.set(clock.get() + poll_takes);
                // Past the script: the panel stays out of reach.
                script.borrow_mut().next().unwrap_or_else(|| Err(anyhow::anyhow!("timeout: global")))
            },
            |d| clock.set(clock.get() + d),
            || clock.get(),
            PAIR_WAIT,
        );
        (out, polls.get())
    }

    #[test]
    fn waiting_for_approval_survives_lost_polls() {
        use PairState::*;
        let lost = || Err(anyhow::anyhow!("can't reach Hopper Server at https://x:443: timeout: global"));
        let slow = Duration::from_secs(15);
        // A poll lost through the proxy, then the approval: paired.
        let (out, polls) = wait_script(vec![Ok(Waiting), lost(), lost(), Ok(Approved("tok".into()))], slow);
        assert_eq!(out.unwrap(), Approved("tok".into()));
        assert_eq!(polls, 4);
        // Denied and expired come straight back.
        assert_eq!(wait_script(vec![Ok(Waiting), Ok(Denied)], slow).0.unwrap(), Denied);
        assert_eq!(wait_script(vec![Ok(Expired)], slow).0.unwrap(), Expired);
        // Out of reach for a minute: gives up, and says at which step.
        let (out, polls) = wait_script(vec![Ok(Waiting)], slow);
        let e = format!("{:#}", out.unwrap_err());
        assert!(e.starts_with("Lost the connection to the panel while waiting") && e.contains("timeout: global"), "{e}");
        assert!((3..=6).contains(&polls), "{polls}");
        // An old panel that forgot the request after a lost reply: it may
        // have been the approval, so say what to do instead of "expired".
        let e = format!("{:#}", wait_script(vec![Ok(Waiting), lost(), Ok(Expired)], slow).0.unwrap_err());
        assert!(e.contains("Remove this PC"), "{e}");
        // Nobody answers for the whole wait: expired.
        let fast = Duration::from_millis(50);
        let (out, polls) = wait_script((0..1000).map(|_| Ok(Waiting)).collect(), fast);
        assert_eq!(out.unwrap(), Expired);
        assert!(polls > 200 && polls < 320, "{polls}");
    }

    #[test]
    fn certificate_errors_told_apart() {
        let cert = anyhow::anyhow!("invalid peer certificate: UnknownIssuer").context("can't reach Hopper Server at https://rig:8790");
        assert!(cert_rejected(&cert));
        assert!(cert_rejected(&anyhow::anyhow!("invalid peer certificate: NotValidForName")));
        let slow = anyhow::anyhow!("timeout: global").context("can't reach Hopper Server at https://rig:8790");
        assert!(!cert_rejected(&slow));
    }

    #[test]
    fn web_urls() {
        let root = remote_root("Web Test");
        register(&root, &Remote { base: "http://rig:8790".into(), id: "web-test".into(), ca: String::new(), panel: String::new() });
        assert_eq!(
            web_url(&root.join("config").join("a b.toml")).unwrap(),
            "http://rig:8790/#/server/web-test/edit/config/a%20b.toml"
        );
        assert_eq!(web_url(&root.join("mods")).unwrap(), "http://rig:8790/#/server/web-test/files/mods");
        assert_eq!(web_url(&root).unwrap(), "http://rig:8790/#/server/web-test/files");
        assert!(web_url(Path::new(r"C:\x")).is_none());
    }

    #[test]
    fn lan_addresses_rank_first() {
        assert!(url_rank("http://192.168.50.2:8790") < url_rank("http://10.66.0.2:8790"));
        assert!(url_rank("http://10.66.0.2:8790") < url_rank("http://100.101.2.3:8790"));
    }

    #[test]
    fn remote_roots() {
        let root = remote_root("TNP Limitless 8");
        assert!(is_remote_root(&root));
        assert_eq!(root.file_name().unwrap(), "TNP Limitless 8");
        assert_eq!(remote_rel(&root, &root.join("mods").join("a.jar")).as_deref(), Some("mods/a.jar"));
        assert!(!is_remote_root(Path::new(r"Q:\Minecraft\packs\TNP")));
        // Reading it fails at once instead of waiting on the network.
        let t = std::time::Instant::now();
        assert!(std::fs::read_dir(root.join("mods")).is_err());
        assert!(t.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn server_info_from_panel_json() {
        let v = json!({
            "id": "tnp-limitless-8", "name": "TNP Limitless 8", "path": "G:\\Minecraft\\packs\\TNP Limitless 8",
            "managed_by": null, "loader": "neoforge 21.1.172", "java": null, "max_memory": "12G",
            "properties": { "server-port": "25565", "motd": "hi" }
        });
        let i = info_from(&v, "https://rig:8790", "", "");
        assert_eq!(i.root, remote_root("TNP Limitless 8"));
        let j = info_from(&v, "https://vps:8790", "", "p2");
        assert_eq!(j.root, panel_root("p2", "TNP Limitless 8"));
        assert_eq!(j.root.file_name().unwrap(), "TNP Limitless 8");
        assert_eq!(host_name("https://rig:8790"), "rig");
        assert_eq!(i.port(), 25565);
        assert_eq!(i.remote.as_ref().unwrap().id, "tnp-limitless-8");
        assert_eq!(i.max_memory.as_deref(), Some("12G"));
        assert_eq!(i.manager, Manager::Unknown);
    }
}
