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
    /// The delete dialog starts on "Delete permanently" instead of moving
    /// the folder to .hopper-deleted for 30 days.
    pub delete_permanently: bool,
    /// Announce the panel on the LAN (mDNS).
    pub discovery: bool,
    /// Plain HTTP from the LAN and Tailscale too is refused (it always is
    /// from the internet).
    pub require_tls: bool,
    /// Other names or addresses the panel is reached by (a public IP, a
    /// DDNS name), put in its certificate.
    pub public_names: Vec<String>,
    /// A domain to get a Let's Encrypt certificate for, when it points at
    /// this server. Needs port 443 on that name to reach this panel.
    pub acme_domain: Option<String>,
    /// Get a trusted Let's Encrypt certificate by itself: for the domain,
    /// else the public IP, else its sslip.io name. Unset: on when a domain
    /// is set (as before this setting).
    pub auto_cert: Option<bool>,
    /// Contact for Let's Encrypt.
    pub acme_email: String,
    /// Use Let's Encrypt's staging server (for testing).
    pub acme_staging: bool,
    /// CurseForge API key for mod search, identify and updates (Modrinth
    /// needs none).
    pub curseforge_key: String,
    /// Mods may be added from GitHub, Maven, Jenkins and download links
    /// (they skip CurseForge's and Modrinth's review). Off by default.
    pub allow_sources: bool,
    /// Optional GitHub token for those checks (raises GitHub's limit).
    pub github_token: String,
    /// SFTP port; 0 turns SFTP off.
    pub sftp_port: u16,
    /// Server addresses: one Minecraft port for every server.
    pub router: crate::router::RouterConfig,
    /// Until there's an owner, a browser on this PC itself sets the owner
    /// password without the setup code. Off in Docker regardless.
    pub trust_local_setup: bool,
    /// Update Hopper Server by itself when a new version is out (Windows;
    /// not in Docker, where the image is updated instead).
    pub auto_update: bool,
    /// Set by rolling back: auto-update doesn't install this version or
    /// older ones again (a newer release still installs).
    pub skip_auto_up_to: Option<String>,
    /// Switching a server to another pack version opens the comparison
    /// first. Off: it updates straight away, stopping only when something
    /// needs the owner's say (as in Hopper desktop's setting).
    pub compare_before_switch: bool,
    /// Pack updates are clean by default: every pack mod and file becomes
    /// the server pack's, like the CurseForge app (as in Hopper desktop).
    /// Off: "Keep the server's changes", which previews first.
    pub clean_pack_updates: bool,
    /// The choice per server id, once picked there (true = clean).
    pub pack_update_clean: std::collections::HashMap<String, bool>,
    /// How long each server's `.hopper-backups` keeps the files behind Undo.
    pub backup_retention: hopper_core::retention::Retention,
    /// For a mod version with no changelog from its author, show what changed
    /// in the files (as in Hopper desktop's setting). Off until the owner
    /// turns it on; the panel then compares the old and new jar for its own mods.
    pub files_changes: bool,
    /// Other web pages that may use this panel: exact origins
    /// (`http://fox:8790`) whose pages get CORS answers. Empty: none.
    pub web_origins: Vec<String>,
    /// Let those pages also manage who can get in (users, devices, tokens,
    /// two-factor, the certificate). Off: only this panel's own page can.
    pub web_manage_access: bool,
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
            delete_permanently: false,
            discovery: true,
            require_tls: false,
            public_names: Vec::new(),
            acme_domain: None,
            auto_cert: None,
            acme_email: String::new(),
            acme_staging: false,
            curseforge_key: String::new(),
            allow_sources: false,
            github_token: String::new(),
            sftp_port: 8022,
            router: Default::default(),
            trust_local_setup: true,
            auto_update: true,
            skip_auto_up_to: None,
            compare_before_switch: false,
            clean_pack_updates: true,
            pack_update_clean: Default::default(),
            backup_retention: Default::default(),
            files_changes: false,
            web_origins: Vec::new(),
            web_manage_access: false,
        }
    }
}

pub const FILE: &str = "hopperd.toml";

impl Config {
    /// Whether to get a Let's Encrypt certificate (see [`Config::auto_cert`]).
    pub fn auto_cert_on(&self) -> bool {
        self.auto_cert.unwrap_or_else(|| self.acme_domain.as_deref().is_some_and(|d| !d.trim().is_empty()))
    }

    /// The config in `dir`, made with defaults (and saved) if missing.
    pub fn load_or_create(dir: &Path) -> anyhow::Result<Config> {
        let path = dir.join(FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let mut c: Config = toml::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
                // A hand-edited list: written the way browsers send `Origin`,
                // else it would never match. Bad entries are left out.
                c.web_origins = c
                    .web_origins
                    .iter()
                    .filter_map(|o| crate::web_access::normalize(o).inspect_err(|e| log::warn!("web_origins in {FILE}: {e} Left out.")).ok())
                    .collect();
                Ok(c)
            }
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
