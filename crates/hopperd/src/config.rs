//! hopperd's settings, kept in `hopperd.toml` in its data folder.

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DEFAULT_PORT: u16 = 8790;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Shown in the UI and in LAN discovery ("rig").
    pub name: String,
    /// The panel folder: servers and the panel's own data live in it, and
    /// the panel never shows anything outside it. Picked at setup.
    pub home: Option<PathBuf>,
    /// Move the panel folder here at the next start (Settings › Panel
    /// folder › Move).
    pub move_to: Option<PathBuf>,
    pub port: u16,
    /// Address to listen on; 0.0.0.0 is every network.
    pub bind: String,
    /// Every folder in here with a server.properties is a server.
    pub servers_root: Option<PathBuf>,
    /// Server folders outside `servers_root`.
    pub servers: Vec<PathBuf>,
    /// Server folders in `servers_root` the panel leaves out (removed from
    /// the panel, the folder kept).
    pub hidden: Vec<PathBuf>,
    /// Announce the panel on the LAN (mDNS).
    pub discovery: bool,
    /// Plain HTTP from the LAN and Tailscale too is refused (it always is
    /// from the internet).
    pub require_tls: bool,
    /// Other names or addresses the panel is reached by (a public IP, a
    /// DDNS name), put in its certificate.
    pub public_names: Vec<String>,
    /// A domain to get a Let's Encrypt certificate for. Needs port 443 on
    /// that name to reach this panel.
    pub acme_domain: Option<String>,
    /// Contact for Let's Encrypt.
    pub acme_email: String,
    /// Use Let's Encrypt's staging server (for testing).
    pub acme_staging: bool,
    /// CurseForge API key for mod search, identify and updates (Modrinth
    /// needs none).
    pub curseforge_key: String,
    /// SFTP port; 0 turns SFTP off.
    pub sftp_port: u16,
    /// Server addresses: one Minecraft port for every server.
    pub router: crate::router::RouterConfig,
    /// Until there's an owner, a browser on this PC itself sets the owner
    /// password without the setup code. Off in Docker regardless.
    pub trust_local_setup: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            name: hostname(),
            home: None,
            move_to: None,
            port: DEFAULT_PORT,
            bind: "0.0.0.0".into(),
            servers_root: None,
            servers: Vec::new(),
            hidden: Vec::new(),
            discovery: true,
            require_tls: false,
            public_names: Vec::new(),
            acme_domain: None,
            acme_email: String::new(),
            acme_staging: false,
            curseforge_key: String::new(),
            sftp_port: 8022,
            router: Default::default(),
            trust_local_setup: true,
        }
    }
}

pub const FILE: &str = "hopperd.toml";

impl Config {
    /// The config in `dir`, made with defaults (and saved) if missing.
    pub fn load_or_create(dir: &Path) -> anyhow::Result<Config> {
        let path = dir.join(FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("reading {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let c = Config::default();
                c.save(dir)?;
                Ok(c)
            }
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self, dir: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(FILE);
        std::fs::write(&path, toml::to_string_pretty(self)?).with_context(|| format!("writing {}", path.display()))
    }
}

/// Where hopperd keeps its config and accounts: `HOPPERD_DATA`, else the
/// system's app data folder.
pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("HOPPERD_DATA") {
        return PathBuf::from(d);
    }
    directories::ProjectDirs::from("", "", "hopperd")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("hopperd-data"))
}

pub fn hostname() -> String {
    sysinfo::System::host_name()
        .filter(|h| !h.trim().is_empty())
        .unwrap_or_else(|| "hopperd".into())
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_then_reads_back() {
        let dir = std::env::temp_dir().join(format!("hopperd-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut c = Config::load_or_create(&dir).unwrap();
        assert_eq!(c.port, DEFAULT_PORT);
        c.servers_root = Some("G:/Minecraft/packs".into());
        c.save(&dir).unwrap();
        let back = Config::load_or_create(&dir).unwrap();
        assert_eq!(back.servers_root, c.servers_root);
        // Missing keys fall back to defaults.
        std::fs::write(dir.join(FILE), "port = 9000\n").unwrap();
        let partial = Config::load_or_create(&dir).unwrap();
        assert_eq!(partial.port, 9000);
        assert!(partial.discovery);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
