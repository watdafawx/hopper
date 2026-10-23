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
    pub port: u16,
    /// Address to listen on; 0.0.0.0 is every network.
    pub bind: String,
    /// Every folder in here with a server.properties is a server.
    pub servers_root: Option<PathBuf>,
    /// Server folders outside `servers_root`.
    pub servers: Vec<PathBuf>,
    /// Announce the panel on the LAN (mDNS).
    pub discovery: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            name: hostname(),
            port: DEFAULT_PORT,
            bind: "0.0.0.0".into(),
            servers_root: None,
            servers: Vec::new(),
            discovery: true,
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
