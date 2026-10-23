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
use std::time::Duration;

/// The panel's default port.
pub const PORT: u16 = 8790;
/// mDNS service hopperd announces.
pub const SERVICE: &str = "_hopperd._tcp.local.";
/// Upload chunk size (hopperd takes up to 32 MB).
const CHUNK: usize = 8 * 1024 * 1024;

/// Which Hopper Server a server is on, and its id there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    /// "http://192.168.50.3:8790"
    pub base: String,
    pub id: String,
}

/// The stand-in root for a server on a Hopper Server.
pub fn remote_root(folder: &str) -> PathBuf {
    PathBuf::from(format!("hopperd:\\{folder}"))
}

pub fn is_remote_root(p: &Path) -> bool {
    p.to_string_lossy().starts_with("hopperd:")
}

/// `http://host:8790` from what was typed ("rig", "rig:8790", a URL).
pub fn base_url(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    let url = if url.contains("://") { url.to_string() } else { format!("http://{url}") };
    let after = url.split("://").nth(1).unwrap_or("");
    let host = after.split('/').next().unwrap_or("");
    // No port given: hopperd's.
    let has_port = host.rsplit_once(':').is_some_and(|(_, p)| p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty());
    if has_port || host.is_empty() { url } else { format!("{url}:{PORT}") }
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build()
        .into()
}

/// A paired Hopper Server.
pub struct Client {
    pub base: String,
    token: String,
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
    pub fn new(url: &str, token: &str) -> Self {
        Client {
            base: base_url(url),
            token: token.to_string(),
            agent: agent(Duration::from_secs(15)),
            slow: agent(Duration::from_secs(3600)),
        }
    }

    /// The Hopper Server set up in Settings, when it is the panel kind.
    pub fn from_settings(s: &crate::settings::Settings) -> Option<Client> {
        if s.panel_kind != "hopperd" {
            return None;
        }
        let url = std::env::var("HOPPER_PANEL_URL").ok().filter(|u| !u.trim().is_empty()).unwrap_or_else(|| s.panel_url.clone());
        if url.trim().is_empty() {
            return None;
        }
        Some(Client::new(&url, &crate::panel::hopperd_token()?))
    }

    /// For a server on this panel.
    pub fn for_remote(r: &Remote) -> anyhow::Result<Client> {
        let token = crate::panel::hopperd_token().context("Pair with the Hopper Server first (Settings → Server panel).")?;
        Ok(Client::new(&r.base, &token))
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

    // ---- servers -------------------------------------------------------------

    /// Every server on the panel, as Hopper's [`ServerInfo`].
    pub fn server_infos(&self) -> anyhow::Result<Vec<ServerInfo>> {
        let list = self.get("/servers/infos")?;
        Ok(list
            .as_array()
            .into_iter()
            .flatten()
            .map(|v| info_from(v, &self.base))
            .collect())
    }

    /// A server's mods, read by the panel itself (no jar crosses the wire).
    pub fn mods(&self, id: &str) -> anyhow::Result<Vec<ModInfo>> {
        let v = self.get(&format!("/servers/{}/mods", enc(id)))?;
        Ok(serde_json::from_value(v["mods"].clone())?)
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

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn info_from(v: &Value, base: &str) -> ServerInfo {
    let s = |k: &str| v[k].as_str().map(str::to_string);
    let name = s("name").unwrap_or_default();
    let folder = s("path")
        .and_then(|p| p.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().map(str::to_string))
        .filter(|f| !f.is_empty())
        .unwrap_or_else(|| name.clone());
    let properties: BTreeMap<String, String> = serde_json::from_value(v["properties"].clone()).unwrap_or_default();
    ServerInfo {
        name,
        root: remote_root(&folder),
        manager: match v["managed_by"].as_str() {
            Some("Crafty") => Manager::Crafty,
            Some("MCSS") => Manager::Mcss,
            _ => Manager::Unknown,
        },
        properties,
        loader: s("loader"),
        java: s("java"),
        max_memory: s("max_memory"),
        remote: Some(Remote { base: base.to_string(), id: s("id").unwrap_or_default() }),
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
                        let res = crate::sync::execute(vec![SyncOp::Copy { src: staged.clone(), dst: dst.clone() }], local_root);
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
                let res = crate::sync::execute(vec![op.clone()], local_root);
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
}

/// The panel's name and version (no pairing needed).
pub fn hello(url: &str) -> anyhow::Result<Hello> {
    let base = base_url(url);
    let v = reply(agent(Duration::from_secs(5)).get(&format!("{base}/api/v1/hello")).call(), &base)?;
    if v["panel"].as_str() != Some("hopperd") {
        bail!("{base} isn't a Hopper Server.");
    }
    Ok(serde_json::from_value(v)?)
}

/// A pairing request waiting for the owner.
#[derive(Clone, Debug)]
pub struct Pairing {
    pub base: String,
    pub id: String,
    pub secret: String,
    /// The 6-digit code both sides show.
    pub code: String,
}

/// Asks the panel to pair this PC, named `device`.
pub fn pair_request(url: &str, device: &str) -> anyhow::Result<Pairing> {
    let base = base_url(url);
    hello(&base)?;
    let v = reply(
        agent(Duration::from_secs(10)).post(&format!("{base}/api/v1/pair/request")).send_json(json!({ "device": device })),
        &base,
    )?;
    Ok(Pairing {
        base,
        id: v["id"].as_str().unwrap_or("").into(),
        secret: v["secret"].as_str().unwrap_or("").into(),
        code: v["code"].as_str().unwrap_or("").into(),
    })
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
        agent(Duration::from_secs(10))
            .post(&format!("{}/api/v1/pair/status", p.base))
            .send_json(json!({ "id": p.id, "secret": p.secret })),
        &p.base,
    )?;
    Ok(match v["status"].as_str() {
        Some("waiting") => PairState::Waiting,
        Some("approved") => PairState::Approved(v["token"].as_str().unwrap_or("").into()),
        Some("denied") => PairState::Denied,
        _ => PairState::Expired,
    })
}

/// "Hopper on FOX".
pub fn device_name() -> String {
    let host = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "this PC".into());
    format!("Hopper on {host}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(base_url("rig"), "http://rig:8790");
        assert_eq!(base_url("192.168.50.3:9000/"), "http://192.168.50.3:9000");
        assert_eq!(base_url("https://panel.example.com"), "https://panel.example.com:8790");
        assert_eq!(base_url("http://rig:8790"), "http://rig:8790");
        assert_eq!(enc("config/a b.toml"), "config%2Fa%20b.toml");
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
        let i = info_from(&v, "http://rig:8790");
        assert_eq!(i.root, remote_root("TNP Limitless 8"));
        assert_eq!(i.port(), 25565);
        assert_eq!(i.remote.as_ref().unwrap().id, "tnp-limitless-8");
        assert_eq!(i.max_memory.as_deref(), Some("12G"));
        assert_eq!(i.manager, Manager::Unknown);
    }
}
