//! Hosted servers as a [`ServerPanel`], so the Servers page drives them
//! like Crafty's: start, stop, console, stats.
//!
//! A server's id and `path` are its folder's full path, so
//! [`crate::panel::match_server`] pairs it with the [`crate::server::ServerInfo`]
//! of the same folder by name.

use super::process::{LocalServer, STOP_TIMEOUT, ServerState};
use super::properties::Properties;
use crate::panel::{PanelAction, PanelServer, PanelStats, ServerPanel};
use anyhow::bail;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// The servers this PC hosts, keyed by folder.
#[derive(Default)]
pub struct LocalPanel {
    servers: Mutex<BTreeMap<String, Arc<LocalServer>>>,
}

fn key(root: &Path) -> String {
    root.to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_string()
}

impl LocalPanel {
    pub fn new() -> Self {
        Self::default()
    }

    /// A panel with every server folder (one with `server.properties`)
    /// under `servers_root`.
    pub fn discover(servers_root: &Path) -> Self {
        let p = Self::new();
        for s in crate::server::discover(servers_root) {
            p.add(&s.root);
        }
        p
    }

    /// Manages the server in `root` (no-op if it already does).
    pub fn add(&self, root: &Path) -> Arc<LocalServer> {
        self.servers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(key(root))
            .or_insert_with(|| Arc::new(LocalServer::new(root)))
            .clone()
    }

    /// Stops managing `root`; refuses while it runs.
    pub fn remove(&self, root: &Path) -> anyhow::Result<()> {
        let mut g = self.servers.lock().unwrap_or_else(|e| e.into_inner());
        if g.get(&key(root)).is_some_and(|s| s.state().is_alive()) {
            bail!("Stop the server first.");
        }
        g.remove(&key(root));
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<Arc<LocalServer>> {
        self.servers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id.trim_end_matches(['\\', '/']))
            .cloned()
    }

    fn server(&self, id: &str) -> anyhow::Result<Arc<LocalServer>> {
        self.get(id)
            .ok_or_else(|| anyhow::anyhow!("Hopper doesn't host a server at {id}"))
    }

    pub fn all(&self) -> Vec<Arc<LocalServer>> {
        self.servers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }

    /// Any server still running (to ask before quitting).
    pub fn any_running(&self) -> bool {
        self.all().iter().any(|s| s.state().is_alive())
    }

    /// Stops every running server and waits (for app shutdown).
    pub fn stop_all(&self) {
        let running: Vec<_> = self.all().into_iter().filter(|s| s.state().is_alive()).collect();
        std::thread::scope(|sc| {
            for s in &running {
                sc.spawn(move || s.stop_blocking(STOP_TIMEOUT));
            }
        });
    }
}

/// "3.2 GB" style.
pub fn format_bytes(b: u64) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    let b = b as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else {
        format!("{:.0} MB", b / MB)
    }
}

/// "2h 05m", "12m", "3d 4h".
pub fn uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60);
    match (d, h) {
        (0, 0) => format!("{m}m"),
        (0, _) => format!("{h}h {m:02}m"),
        _ => format!("{d}d {h}h"),
    }
}

fn total_memory() -> u64 {
    let mut s = sysinfo::System::new();
    s.refresh_memory();
    s.total_memory()
}

impl ServerPanel for LocalPanel {
    fn name(&self) -> &'static str {
        "Hopper"
    }

    fn servers(&self) -> anyhow::Result<Vec<PanelServer>> {
        Ok(self
            .all()
            .iter()
            .map(|s| {
                let root = s.root();
                let props = Properties::load(&root.join("server.properties")).unwrap_or_default();
                PanelServer {
                    id: key(root),
                    name: root
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    path: key(root),
                    port: props.port(),
                }
            })
            .collect())
    }

    fn stats(&self, id: &str) -> anyhow::Result<PanelStats> {
        let s = self.server(id)?;
        let state = s.state();
        let t = s.tracker();
        let props = Properties::load(&s.root().join("server.properties")).unwrap_or_default();
        let max = t
            .max_from_list
            .or_else(|| props.get("max-players").and_then(|m| m.trim().parse().ok()))
            .unwrap_or(20);
        let (cpu, mem) = s.usage().unwrap_or((0.0, 0));
        let total = total_memory();
        let started = s
            .started_at()
            .filter(|_| state.is_alive())
            .and_then(|t| t.elapsed().ok())
            .map(|d| format!("up {}", uptime(d.as_secs())))
            .unwrap_or_default();
        Ok(PanelStats {
            running: state.is_alive(),
            crashed: matches!(state, ServerState::Crashed(_)),
            starting: state == ServerState::Starting,
            updating: false,
            cpu,
            mem: if mem > 0 { format_bytes(mem) } else { String::new() },
            mem_percent: if total > 0 { mem as f32 * 100.0 / total as f32 } else { 0.0 },
            online: t.players.len() as u32,
            max,
            players: t.players.into_iter().collect(),
            version: t.version,
            world_size: String::new(),
            started,
            ..Default::default()
        })
    }

    fn act(&self, id: &str, action: PanelAction) -> anyhow::Result<()> {
        let s = self.server(id)?;
        match action {
            PanelAction::Start => s.start(),
            PanelAction::Stop => s.stop(STOP_TIMEOUT),
            PanelAction::Kill => s.kill(),
            PanelAction::Restart => {
                if !s.state().is_alive() {
                    return s.start();
                }
                s.stop(STOP_TIMEOUT)?;
                std::thread::Builder::new()
                    .name("server-restart".into())
                    .spawn(move || {
                        let end = std::time::Instant::now() + STOP_TIMEOUT + std::time::Duration::from_secs(5);
                        while s.state().is_alive() && std::time::Instant::now() < end {
                            std::thread::sleep(std::time::Duration::from_millis(250));
                        }
                        if let Err(e) = s.start() {
                            log::warn!("restarting {}: {e:#}", s.root().display());
                        }
                    })?;
                Ok(())
            }
            PanelAction::Backup => bail!("Hopper doesn't back up hosted servers yet."),
        }
    }

    fn console(&self, id: &str) -> anyhow::Result<Vec<String>> {
        Ok(self.server(id)?.console())
    }

    fn command(&self, id: &str, line: &str) -> anyhow::Result<()> {
        self.server(id)?.command(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_servers_matching_their_folders() {
        let base = std::env::temp_dir().join(format!("hopper-localpanel-{}", std::process::id()));
        let a = base.join("My Pack");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join("server.properties"), "server-port=25570\nmax-players=8\n").unwrap();
        let p = LocalPanel::discover(&base);
        let list = p.servers().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "My Pack");
        assert_eq!(list[0].port, 25570);
        let info = crate::server::load(&a);
        assert_eq!(crate::panel::match_server(&list, &info).unwrap().id, list[0].id);
        let st = p.stats(&list[0].id).unwrap();
        assert_eq!(st.state(), "Stopped");
        assert_eq!(st.max, 8);
        // No EULA: start refuses before running anything.
        let e = p.act(&list[0].id, PanelAction::Start).unwrap_err();
        assert!(e.to_string().contains("EULA"));
        assert!(p.command(&list[0].id, "say hi").is_err());
        assert_eq!(uptime(7500), "2h 05m");
        assert_eq!(uptime(90_000), "1d 1h");
        p.remove(&a).unwrap();
        assert!(p.servers().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }
}
