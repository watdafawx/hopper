//! Hosting a Minecraft server on this PC: making the server folder for a
//! loader (or from a client instance), its settings (server.properties,
//! Java arguments, run.bat, EULA), and running it as a process of its own that
//! the Servers page drives through [`LocalPanel`], like a Crafty server,
//! and that keeps running when Hopper restarts.

pub mod backup;
pub mod create;
pub mod jvm;
pub mod local_panel;
pub mod logparse;
pub mod process;
pub mod properties;
pub mod router;
pub mod schedule;
pub mod script;
pub mod server_pack;
pub mod zip_import;

pub use create::{
    CreateOptions, CreateReport, ServerLoader, create_from_client_pack, create_from_instance,
    create_server, java_check,
};
pub use jvm::{JvmArgs, JvmPreset};
pub use local_panel::LocalPanel;
pub use process::{LocalServer, ServerState};
pub use properties::{PropKind, PropSpec, Properties};
pub use schedule::{Schedule, ScheduleEvent, Scheduler};
pub use script::{Launch, LaunchTarget};
pub use server_pack::{
    PackTarget, ServerPackReport, create_from_server_files, create_from_server_pack, has_server_pack,
    pack_target, resolve_server_pack,
};

use std::fs;
use std::net::TcpListener;
use std::path::Path;

pub const EULA_FILE: &str = "eula.txt";
pub const EULA_URL: &str = "https://aka.ms/MinecraftEULA";

/// Whether eula.txt says `eula=true`.
/// What Hopper made a hosted server as, kept in `.hopper-server.json`.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct HostedMeta {
    /// e.g. "1.21.1".
    pub minecraft: String,
    /// e.g. "NeoForge 21.1.256" or "Vanilla".
    pub loader: String,
}

const META_FILE: &str = ".hopper-server.json";

impl HostedMeta {
    pub fn load(root: &Path) -> Option<Self> {
        serde_json::from_str(&std::fs::read_to_string(root.join(META_FILE)).ok()?).ok()
    }

    pub fn save(&self, root: &Path) -> anyhow::Result<()> {
        std::fs::write(root.join(META_FILE), serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

pub fn eula_accepted(root: &Path) -> bool {
    properties::Properties::load(&root.join(EULA_FILE))
        .ok()
        .and_then(|p| p.get("eula").map(|v| v.trim().eq_ignore_ascii_case("true")))
        .unwrap_or(false)
}

/// Writes eula.txt the way the server does. Pass `true` only after the user
/// agreed to the EULA.
pub fn write_eula(root: &Path, accepted: bool) -> anyhow::Result<()> {
    fs::write(
        root.join(EULA_FILE),
        format!(
            "#By changing the setting below to TRUE you are indicating your agreement to our EULA ({EULA_URL}).\n#Written by Hopper {}\neula={accepted}\n",
            crate::time::now_iso()
        ),
    )?;
    Ok(())
}

/// Whether a server could listen on `port` (nothing else has it).
pub fn port_free(port: u16) -> bool {
    TcpListener::bind(("0.0.0.0", port)).is_ok()
}

/// A port for a new server under `servers_root`: 25565, or the next one
/// after those its other servers use, that is free now.
pub fn suggest_port(servers_root: &Path) -> u16 {
    let used: Vec<u16> = crate::server::discover(servers_root)
        .iter()
        .map(|s| s.port())
        .collect();
    (25565..=25665)
        .find(|p| !used.contains(p) && port_free(*p))
        .unwrap_or(25565)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eula_and_ports() {
        let dir = std::env::temp_dir().join(format!("hopper-eula-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        assert!(!eula_accepted(&dir));
        write_eula(&dir, false).unwrap();
        assert!(!eula_accepted(&dir));
        write_eula(&dir, true).unwrap();
        assert!(eula_accepted(&dir));

        let l = TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(!port_free(port));
        drop(l);
        // Servers under the root take their ports out of the running.
        fs::create_dir_all(dir.join("a")).unwrap();
        fs::write(dir.join("a/server.properties"), "server-port=25565\n").unwrap();
        assert_ne!(suggest_port(&dir), 25565);
        let _ = fs::remove_dir_all(&dir);
    }
}
