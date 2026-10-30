//! The servers this panel runs: found in the servers folder (and any extra
//! folders in the config), driven through hopper-core's [`LocalPanel`] so a
//! server keeps running when hopperd restarts.

use hopper_core::hosting::{LocalPanel, LocalServer, ServerState};
use hopper_core::panel::{PanelAction, ServerPanel};
use hopper_core::server::{self, Manager};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

pub struct Entry {
    /// Short id for URLs, from the folder name ("tnp-limitless-8").
    pub id: String,
    /// LocalPanel's id (the folder's full path).
    panel_id: String,
    pub root: PathBuf,
    pub name: String,
    pub loader: Option<String>,
    /// Minecraft version, from the folder's files (before it has run).
    pub minecraft: Option<String>,
    pub max_memory: Option<String>,
    /// Crafty or MCSS also knows this folder (it runs here in place too).
    pub managed_by: Option<&'static str>,
    pub server: Arc<LocalServer>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Summary {
    pub id: String,
    pub name: String,
    pub path: String,
    pub port: u16,
    /// stopped, starting, running, stopping, crashed.
    pub state: &'static str,
    pub exit_code: Option<i32>,
    pub managed_by: Option<&'static str>,
    pub loader: Option<String>,
    pub version: String,
    pub max_memory: Option<String>,
    /// Percent of the whole machine.
    pub cpu: f32,
    pub mem_bytes: u64,
    pub players: Vec<String>,
    pub max_players: u32,
    pub uptime_secs: Option<u64>,
    /// Stopping and saving its worlds (shown as "Saving worlds…").
    pub saving: bool,
    /// While starting: what it is doing, how far along, time left.
    pub boot: Option<hopper_core::hosting::boot::BootStatus>,
    /// Addresses players join it by (server addresses / the router):
    /// saved ones, or the default `name.<domain>` (see
    /// [`crate::router::effective`], which the callers fill in).
    pub addresses: Vec<String>,
    /// `addresses` is the default, not saved.
    pub address_default: bool,
    /// Sleeps after this many minutes with nobody on; 0: never.
    pub sleep_after_minutes: u32,
    /// `server-icon.png`'s modified time (Unix seconds), when it has one;
    /// part of the icon's URL so a new icon shows at once.
    pub icon: Option<u64>,
}

pub struct Servers {
    panel: Arc<LocalPanel>,
    entries: RwLock<Vec<Arc<Entry>>>,
}

/// "TNP Limitless 8" → "tnp-limitless-8".
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "server".into() } else { out }
}

fn state_name(s: &ServerState) -> &'static str {
    match s {
        ServerState::Stopped => "stopped",
        ServerState::Starting => "starting",
        ServerState::Running => "running",
        ServerState::Stopping => "stopping",
        ServerState::Crashed(_) => "crashed",
    }
}

impl Servers {
    pub fn new() -> Self {
        Servers { panel: Arc::new(LocalPanel::new()), entries: RwLock::new(Vec::new()) }
    }

    /// Picks up every server folder under `root` and in `extra`. Servers
    /// already known keep their state; folders that are gone are dropped
    /// unless their server still runs.
    pub fn scan(&self, root: Option<&Path>, extra: &[PathBuf]) {
        self.scan_except(root, extra, &[]);
    }

    /// [`Self::scan`], leaving out the folders in `hidden` (removed from
    /// the panel).
    pub fn scan_except(&self, root: Option<&Path>, extra: &[PathBuf], hidden: &[PathBuf]) {
        let low = |p: &Path| p.to_string_lossy().trim_end_matches(['\\', '/']).replace('\\', "/").to_lowercase();
        let hidden: Vec<String> = hidden.iter().map(|h| low(h)).collect();
        let mut found: Vec<PathBuf> = root
            .map(server::discover)
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.root)
            .filter(|r| !hidden.contains(&low(r)))
            .collect();
        for p in extra {
            if p.join("server.properties").is_file() && !found.contains(p) {
                found.push(p.clone());
            }
        }
        let mut entries = self.entries.write().unwrap();
        entries.retain(|e| found.contains(&e.root) || e.server.state().is_alive());
        for root in found {
            if entries.iter().any(|e| e.root == root) {
                continue;
            }
            let server = self.panel.add(&root);
            let info = server::load(&root);
            let minecraft = hopper_core::hosting::script::start_info(&root).minecraft;
            let panel_id = self
                .panel
                .servers()
                .unwrap_or_default()
                .into_iter()
                .find(|s| Path::new(&s.path) == root.as_path() || s.path == root.to_string_lossy().trim_end_matches(['\\', '/']))
                .map(|s| s.id)
                .unwrap_or_else(|| root.to_string_lossy().into_owned());
            let base = slug(&info.name);
            let mut id = base.clone();
            let mut n = 2;
            while entries.iter().any(|e| e.id == id) {
                id = format!("{base}-{n}");
                n += 1;
            }
            entries.push(Arc::new(Entry {
                id,
                panel_id,
                name: info.name.clone(),
                loader: info.loader.clone(),
                minecraft,
                max_memory: info.max_memory.clone(),
                managed_by: match info.manager {
                    Manager::Crafty => Some("Crafty"),
                    Manager::Mcss => Some("MCSS"),
                    Manager::Unknown => None,
                },
                root,
                server,
            }));
        }
        entries.sort_by_key(|e| e.name.to_lowercase());
    }

    /// The panel the scheduler runs over.
    pub fn panel(&self) -> Arc<LocalPanel> {
        self.panel.clone()
    }

    pub fn list(&self) -> Vec<Arc<Entry>> {
        self.entries.read().unwrap().clone()
    }

    pub fn get(&self, id: &str) -> Option<Arc<Entry>> {
        self.entries.read().unwrap().iter().find(|e| e.id == id).cloned()
    }

    /// Reads a server's loader, version and memory from its files again
    /// (after its version changed), keeping its id and process.
    pub fn reload_info(&self, id: &str) {
        let mut entries = self.entries.write().unwrap();
        let Some(i) = entries.iter().position(|e| e.id == id) else { return };
        let e = &entries[i];
        let info = server::load(&e.root);
        let fresh = Entry {
            id: e.id.clone(),
            panel_id: e.panel_id.clone(),
            root: e.root.clone(),
            name: e.name.clone(),
            loader: info.loader.clone(),
            minecraft: hopper_core::hosting::script::start_info(&e.root).minecraft,
            max_memory: info.max_memory.clone(),
            managed_by: e.managed_by,
            server: e.server.clone(),
        };
        entries[i] = Arc::new(fresh);
    }

    pub fn summary(&self, e: &Entry) -> Summary {
        let state = e.server.state();
        let t = e.server.tracker();
        let props = hopper_core::hosting::Properties::load(&e.root.join("server.properties")).unwrap_or_default();
        let (cpu, mem) = if state.is_alive() { e.server.usage().unwrap_or((0.0, 0)) } else { (0.0, 0) };
        let route = hopper_core::hosting::router::RouteSettings::load(&e.root);
        // Servers Hopper made know their version before they've run.
        let meta = hopper_core::hosting::HostedMeta::load(&e.root);
        Summary {
            id: e.id.clone(),
            name: e.name.clone(),
            path: e.root.to_string_lossy().into_owned(),
            port: props.port(),
            state: state_name(&state),
            saving: state == ServerState::Stopping && t.saving_at.is_some() && t.saved_at.is_none(),
            boot: e.server.boot_status(),
            exit_code: match state {
                ServerState::Crashed(c) => c,
                _ => None,
            },
            managed_by: e.managed_by,
            loader: e.loader.clone().or_else(|| meta.as_ref().map(|m| m.loader.clone()).filter(|l| !l.is_empty() && l != "Vanilla")),
            version: if t.version.is_empty() {
                meta.map(|m| m.minecraft).filter(|v| !v.is_empty()).or_else(|| e.minecraft.clone()).unwrap_or_default()
            } else {
                t.version
            },
            max_memory: e.max_memory.clone(),
            cpu,
            mem_bytes: mem,
            players: t.players.into_iter().collect(),
            max_players: t
                .max_from_list
                .or_else(|| props.get("max-players").and_then(|m| m.trim().parse().ok()))
                .unwrap_or(20),
            uptime_secs: e
                .server
                .started_at()
                .filter(|_| state.is_alive())
                .and_then(|s| s.elapsed().ok())
                .map(|d| d.as_secs()),
            addresses: route.addresses,
            address_default: false,
            sleep_after_minutes: route.sleep_after_minutes,
            icon: std::fs::metadata(e.root.join("server-icon.png"))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs()),
        }
    }

    /// Start, stop, restart or kill. Servers other panels (Crafty, MCSS)
    /// also know run here in place too; starting one that's already running
    /// somewhere is refused by the start itself (its port or world is taken).
    pub fn act(&self, e: &Entry, action: PanelAction) -> anyhow::Result<()> {
        self.panel.act(&e.panel_id, action)
    }

    pub fn command(&self, e: &Entry, line: &str) -> anyhow::Result<()> {
        let line = line.trim().trim_start_matches('/');
        if line.is_empty() {
            anyhow::bail!("Type a command.");
        }
        self.panel.command(&e.panel_id, line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slug("TNP Limitless 8"), "tnp-limitless-8");
        assert_eq!(slug("  ATM 10 -- (test) "), "atm-10-test");
        assert_eq!(slug("§§"), "server");
    }

    #[test]
    fn scans_folders_and_refuses_managed_starts() {
        let base = std::env::temp_dir().join(format!("hopperd-servers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for (name, managed) in [("My Pack", false), ("my pack!", false), ("Crafty One", true)] {
            let d = base.join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("server.properties"), "server-port=25599\nmax-players=6\n").unwrap();
            if managed {
                std::fs::write(d.join("crafty_managed.txt"), "").unwrap();
            }
        }
        std::fs::create_dir_all(base.join("not a server")).unwrap();
        let s = Servers::new();
        s.scan(Some(&base), &[]);
        let ids: Vec<String> = s.list().iter().map(|e| e.id.clone()).collect();
        assert_eq!(ids.len(), 3);
        assert!(ids.contains(&"my-pack".to_string()) && ids.contains(&"my-pack-2".to_string()));
        let crafty = s.get("crafty-one").unwrap();
        assert_eq!(crafty.managed_by, Some("Crafty"));
        // Another panel's folder still starts here (this one fails on its EULA).
        let e = s.act(&crafty, PanelAction::Start).unwrap_err().to_string();
        assert!(!e.contains("manages") && e.contains("EULA"), "{e}");
        let sum = s.summary(&s.get("my-pack").unwrap());
        assert_eq!((sum.state, sum.port, sum.max_players), ("stopped", 25599, 6));
        assert_eq!((sum.loader, sum.version.as_str(), sum.max_memory), (None, "", None));
        // Rescanning keeps ids stable.
        s.scan(Some(&base), &[]);
        assert_eq!(s.list().len(), 3);
        assert!(s.get("my-pack-2").is_some());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A Fabric server started by start.bat (as Crafty left BCG): loader,
    /// version and memory come from its files before it has ever run here.
    #[test]
    fn summary_of_a_fabric_start_bat_server() {
        let base = std::env::temp_dir().join(format!("hopperd-servers-fabric-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let d = base.join("BCG");
        std::fs::create_dir_all(d.join(".fabric/server")).unwrap();
        std::fs::create_dir_all(d.join("libraries/net/fabricmc/fabric-loader/0.18.1")).unwrap();
        std::fs::write(d.join("server.properties"), "server-port=25598\n").unwrap();
        std::fs::write(d.join("start.bat"), "java -Dfabric.log.level=DEBUG -Xmx8G -jar fabric-server-launch.jar nogui\r\npause").unwrap();
        std::fs::write(d.join("fabric-server-launch.jar"), "").unwrap();
        std::fs::write(d.join(".fabric/server/1.20.1-server.jar"), "").unwrap();
        let s = Servers::new();
        s.scan(Some(&base), &[]);
        let sum = s.summary(&s.get("bcg").unwrap());
        assert_eq!(sum.loader.as_deref(), Some("fabric 0.18.1"));
        assert_eq!(sum.version, "1.20.1");
        assert_eq!(sum.max_memory.as_deref(), Some("8G"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
