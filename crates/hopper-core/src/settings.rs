//! App settings, shared by every Hopper UI: a JSON file in Hopper's data
//! folder.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

static LOAD_ERROR: Mutex<Option<String>> = Mutex::new(None);

fn set_load_error(msg: String) {
    *LOAD_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = Some(msg);
}

/// A named set of Java arguments the user saved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JavaPreset {
    pub name: String,
    pub args: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Where instances live (CurseForge's Instances folder works).
    pub instances_root: String,
    /// Mojang launcher folder with `minecraft.exe` (CurseForge's Install folder).
    pub launcher_dir: String,
    /// Server packs on rig, through the share.
    pub servers_root: String,
    /// Host used for the status ping.
    pub server_host: String,
    pub default_memory_mb: u32,
    /// Browse searches Modrinth instead of CurseForge.
    pub browse_modrinth: bool,
    /// Java arguments and executable for instances that leave theirs empty.
    pub default_java_args: String,
    pub default_java_path: String,
    /// Java argument presets the user saved, in the order they were made.
    pub java_presets: Vec<JavaPreset>,
    /// Library order: false = name, true = last played.
    pub library_by_recent: bool,
    /// Instance folder -> group name.
    pub groups: HashMap<String, String>,
    /// 0 follows Windows, 1 dark, 2 light.
    pub theme: u8,
    /// After a pack update, bring its paired rig server along (when offline).
    pub auto_update_servers: bool,
    /// How each non-mods folder syncs, per server root.
    pub folder_rules: crate::folderrules::Rules,
    /// Peer sync with other PCs running Hopper.
    pub peer_sync: bool,
    /// Where data packs go, per instance root: a folder relative to it.
    pub datapack_dest: HashMap<String, String>,
    /// Hopper opens curseforge.com's Install links; what handled them
    /// before (the CurseForge app), to give them back.
    pub links_prev_handler: Option<String>,
    pub links_enabled: bool,
    /// Minimize Hopper while a game runs, and bring it back after.
    pub minimize_while_playing: bool,
    /// Record FPS, frame times, CPU, RAM, GPU and GC of each game run, to
    /// compare Java argument presets.
    pub record_sessions: bool,
    /// Mods tab shows a dot per mod for its state on the paired server.
    pub mods_server_column: bool,
    /// The GPUI Library shows its instance list as a narrow rail.
    pub sidebar_collapsed: bool,
    /// Width of the GPUI instance list, in rem.
    pub sidebar_width: f32,
    /// The Accounts dialog shows its account list as a rail of faces.
    pub accounts_sidebar_collapsed: bool,
    /// Width of the Accounts dialog's account list, in rem.
    pub accounts_sidebar_width: f32,
    /// Width of the GPUI project page panel (Browse and the Library peek), in rem.
    pub browse_panel_width: f32,
    /// Widths of the GPUI side panels in rem, by panel key. Negative means
    /// collapsed to a rail, remembering the open width.  Missing = default.
    pub panel_widths: HashMap<String, f32>,
    /// The instance picked last, to open on it again.
    pub last_instance: String,
    /// GPUI zoom in percent (Ctrl + / Ctrl - / Ctrl 0).
    pub zoom: u16,
    /// Start games through Hopper (Microsoft sign-in) instead of Mojang's launcher.
    pub launch_native: bool,
    /// The user chose "Launch with" themselves. Until then, a Microsoft
    /// sign-in switches it to Hopper, as CurseForge starts games directly.
    pub launch_picked: bool,
    /// Name of the Microsoft-signed-in profile, for display.
    pub signed_in_as: Option<String>,
    /// Server folder -> client instance folder.
    pub pairings: HashMap<String, String>,
    /// Pack projects the user syncs even though the pack author excludes them.
    pub sync_anyway: HashSet<u64>,
    /// Mod ids or file names hidden from the server comparison.
    pub sync_blacklist: HashSet<String>,
    /// Which server panel: "crafty", "mcss" or "pterodactyl".
    pub panel_kind: String,
    /// The server panel's address (Crafty: https://rig:8443); empty is none.
    /// Its API key is kept apart, in `panel-token`.
    pub panel_url: String,
    /// Accept the panel's self-signed certificate (Crafty makes its own).
    pub panel_self_signed: bool,
    /// Hopper Server's certificate authority (PEM), pinned when pairing.
    pub panel_ca: String,
    /// Paired Hopper Server panels, each a group in the Servers list.
    pub hopper_panels: Vec<HopperPanel>,
    /// Server list groups folded shut (panel ids, "local", "folder").
    pub collapsed_groups: HashSet<String>,
    /// Where the GPUI window was last, to open there again.
    pub window: Option<SavedWindow>,
    /// The GPUI theme by name; empty follows Windows (Hopper Dark or Light).
    pub theme_name: String,
    /// The GitHub repository (owner/name) Hopper updates itself from.
    pub update_repo: String,
    /// The Hopper version What's new was last shown for; a newer build
    /// shows what changed since, once.
    pub seen_version: String,
    /// Hopper's icon in the notification area, with its notifications.
    pub tray_icon: bool,
    /// The MCP server AI assistants connect to (Settings > Advanced).
    pub mcp_enabled: bool,
    /// Its port on 127.0.0.1; empty for 47801.
    pub mcp_port: String,
    /// Its tools that change instances: install, switch on or off, update.
    pub mcp_changes: bool,
    /// Its tools that see and drive the window: screenshot, click, type.
    pub mcp_window: bool,
    /// Where servers Hopper hosts on this PC live.
    pub hosted_root: String,
    /// The page Hopper starts on: "library", "browse", "servers" or
    /// "sync"; empty for the one left last.
    pub start_page: String,
    /// The page shown last (same keys), for an empty `start_page`.
    pub last_page: String,
    /// The tab an instance opens on (an instance tab's label, lowercase);
    /// empty for the one shown last.
    pub instance_tab: String,
    /// The instance tab shown last, for an empty `instance_tab`.
    pub last_tab: String,
    /// Browse's sort ([`crate::cf::SortField::key`]); empty for the one
    /// picked last.
    pub browse_sort: String,
    /// The Browse sort picked last, for an empty `browse_sort`.
    pub last_sort: String,
    /// Turning a mod off or on that others depend on (or that needs mods
    /// that are off): "ask" (or empty), "all" to change them too, "only"
    /// to change just that mod.
    pub dependents: String,
    /// Days a downloaded pack (kept to compare and install updates without
    /// downloading it again) stays after it was last used; 0 keeps them.
    pub pack_keep_days: u32,
    /// Deleted instances, worlds and backups are removed outright instead
    /// of filling the Recycle Bin (on C:) with gigabytes.
    pub delete_permanently: bool,
    /// Other launchers' folders whose instances Hopper manages in place.
    pub launcher_dirs: Vec<LauncherDir>,
    /// The first-run setup was finished or skipped. A settings file from
    /// before the setup existed has no such field: those users are set up
    /// already, so a missing field reads as true. Only a brand-new
    /// [`Settings::default`] (no settings file yet) starts at false.
    #[serde(default = "yes")]
    pub setup_done: bool,
    /// "Don't ask again" on moving Hopper's files next to its exe.
    pub data_move_declined: bool,
    /// Server tools (the Servers page, server buttons on instances, Peers):
    /// `None` until chosen, which reads as on when servers, a panel or
    /// peers are set up and off for a fresh install.
    pub server_tools: Option<bool>,
    /// With server tools off, the quiet Servers item in the top bar goes too.
    pub hide_servers_item: bool,
    /// Instances (their folders) whose "Running a server for this pack?"
    /// hint was dismissed.
    pub server_hint_dismissed: Vec<String>,
    /// Notices the player closed, as `place|kind|hash of what it said`
    /// (place: an instance's or server's folder). A notice that says
    /// something new has a new hash, so it shows again.
    pub dismissed_notices: Vec<String>,
    /// Mods Hopper reverted because their newer versions need a newer mod
    /// or loader: their updates are marked until that changes.
    pub update_holds: Vec<UpdateHold>,
    /// Offer to translate changelogs and descriptions into English, on this
    /// PC. Off until the user turns it on: nothing is detected, shown or
    /// downloaded before.
    pub translate: bool,
    /// Languages (`translate::Lang::code`) translated without asking.
    pub translate_always: Vec<String>,
    /// How long `.hopper-backups` keeps the files behind Undo.
    pub backup_retention: crate::retention::Retention,
    /// Settings this build doesn't know (saved by a newer Hopper), kept as
    /// they are so going back a version and forward again loses none.
    #[serde(flatten)]
    pub unknown: serde_json::Map<String, serde_json::Value>,
}

fn yes() -> bool {
    true
}

/// A paired Hopper Server.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HopperPanel {
    /// Stable id; "" for the one paired before several panels were
    /// possible (its token and server roots keep their old names).
    pub id: String,
    /// What the list calls it ("rig").
    pub name: String,
    pub url: String,
    /// Its certificate authority (PEM), pinned when pairing.
    pub ca: String,
}

impl HopperPanel {
    /// Where its token is kept in Credential Manager.
    pub fn token_key(&self) -> String {
        crate::secrets::hopperd_key(&self.id)
    }
}

/// Another launcher's folder Hopper reads instances from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LauncherDir {
    /// [`crate::launchers::Kind::key`].
    pub kind: String,
    pub path: String,
}

/// A window's place on screen, in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedWindow {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}

/// Folders found on this PC, for the defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetectedFolders {
    pub instances_root: String,
    pub launcher_dir: String,
    pub servers_root: String,
    pub server_host: String,
}

/// Finds CurseForge's Minecraft folders: the custom folder recorded in the
/// CurseForge app's `storage.json` (under `appdata`, i.e. %APPDATA%), then
/// `<home>\curseforge\minecraft`. With nothing found, instances point at CurseForge's standard
/// folder under `home`, there are no servers, and the host is localhost.
pub fn detect_folders(home: &Path, appdata: Option<&Path>) -> DetectedFolders {
    detect_folders_with(home, appdata, Path::new(""), Path::new(""))
}

fn detect_folders_with(
    home: &Path,
    appdata: Option<&Path>,
    legacy_curse: &Path,
    legacy_servers: &Path,
) -> DetectedFolders {
    let standard = home.join("curseforge").join("minecraft");
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(r) = appdata.and_then(curseforge_app_root) {
        roots.push(r);
    }
    roots.push(standard.clone());
    if !legacy_curse.as_os_str().is_empty() {
        roots.push(legacy_curse.to_path_buf());
    }
    let curse = roots
        .into_iter()
        .find(|r| r.join("Instances").is_dir())
        .unwrap_or(standard);
    let (servers_root, server_host) = if !legacy_servers.as_os_str().is_empty() && legacy_servers.is_dir() {
        (
            legacy_servers.to_string_lossy().into_owned(),
            "localhost".to_string(),
        )
    } else {
        (String::new(), "localhost".to_string())
    };
    DetectedFolders {
        instances_root: curse.join("Instances").to_string_lossy().into_owned(),
        launcher_dir: curse.join("Install").to_string_lossy().into_owned(),
        servers_root,
        server_host,
    }
}

/// `minecraftRoot` from the CurseForge app's settings: `storage.json` holds
/// `"minecraft-settings"` as a JSON string with that field.
fn curseforge_app_root(appdata: &Path) -> Option<PathBuf> {
    let b = std::fs::read(appdata.join("CurseForge").join("storage.json")).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&b).ok()?;
    let inner = &v["minecraft-settings"];
    let inner: serde_json::Value = match inner {
        serde_json::Value::String(s) => serde_json::from_str(s).ok()?,
        other => other.clone(),
    };
    let root = inner["minecraftRoot"].as_str()?.trim();
    (!root.is_empty()).then(|| PathBuf::from(root))
}

/// With `HOPPER_DATA_DIR` set (a test or portable copy), nothing on the PC
/// is looked for: instances and game files start empty inside that folder,
/// so a copy never sees (or changes) the user's real instances, even when
/// its settings are missing.
fn forced_folders(root: &Path) -> DetectedFolders {
    DetectedFolders {
        instances_root: root.join("instances").to_string_lossy().into_owned(),
        launcher_dir: root.join("minecraft").to_string_lossy().into_owned(),
        servers_root: String::new(),
        server_host: String::new(),
    }
}

/// [`detect_folders`] for this user, looked up once per run.
fn detected() -> &'static DetectedFolders {
    static D: OnceLock<DetectedFolders> = OnceLock::new();
    D.get_or_init(|| {
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .unwrap_or_default();
        let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
        detected_for(crate::paths::forced_root().as_deref(), &home, appdata.as_deref())
    })
}

/// [`detected`] for a given `HOPPER_DATA_DIR` (`forced`), home and %APPDATA%.
fn detected_for(forced: Option<&Path>, home: &Path, appdata: Option<&Path>) -> DetectedFolders {
    if let Some(root) = forced {
        return forced_folders(root);
    }
    let mut d = detect_folders(home, appdata);
    // No CurseForge folders to share: a new user's instances and game
    // files go in Hopper's own folder, next to its exe.
    if !Path::new(&d.instances_root).is_dir()
        && let Some(root) = crate::paths::dirs().and_then(|d| d.root())
    {
        d.instances_root = root.join("instances").to_string_lossy().into_owned();
        d.launcher_dir = root.join("minecraft").to_string_lossy().into_owned();
    }
    d
}

impl Default for Settings {
    fn default() -> Self {
        let d = detected();
        Self {
            instances_root: d.instances_root.clone(),
            launcher_dir: d.launcher_dir.clone(),
            servers_root: d.servers_root.clone(),
            server_host: d.server_host.clone(),
            default_memory_mb: 8192,
            browse_modrinth: false,
            default_java_args: String::new(),
            default_java_path: String::new(),
            java_presets: Vec::new(),
            library_by_recent: false,
            groups: HashMap::new(),
            theme: 0,
            auto_update_servers: false,
            folder_rules: Default::default(),
            peer_sync: false,
            datapack_dest: HashMap::new(),
            links_prev_handler: None,
            links_enabled: false,
            minimize_while_playing: false,
            record_sessions: true,
            mods_server_column: true,
            sidebar_collapsed: false,
            sidebar_width: 20.0,
            accounts_sidebar_collapsed: false,
            accounts_sidebar_width: 17.0,
            browse_panel_width: 32.0,
            panel_widths: HashMap::new(),
            last_instance: String::new(),
            zoom: 100,
            // Mojang's launcher is only found as CurseForge's minecraft.exe,
            // so elsewhere games start through Hopper.
            launch_native: !cfg!(windows),
            launch_picked: false,
            signed_in_as: None,
            pairings: HashMap::new(),
            sync_anyway: HashSet::new(),
            sync_blacklist: HashSet::new(),
            panel_kind: "crafty".into(),
            panel_url: String::new(),
            panel_self_signed: true,
            panel_ca: String::new(),
            hopper_panels: Vec::new(),
            collapsed_groups: HashSet::new(),
            window: None,
            theme_name: String::new(),
            update_repo: DEFAULT_UPDATE_REPO.into(),
            seen_version: String::new(),
            tray_icon: true,
            mcp_enabled: false,
            mcp_port: String::new(),
            mcp_changes: false,
            mcp_window: false,
            hosted_root: default_hosted_root(),
            launcher_dirs: Vec::new(),
            start_page: String::new(),
            last_page: String::new(),
            instance_tab: String::new(),
            last_tab: String::new(),
            browse_sort: String::new(),
            last_sort: String::new(),
            dependents: String::new(),
            pack_keep_days: 30,
            delete_permanently: true,
            // A new user: Hopper shows its setup on the first start.
            setup_done: false,
            data_move_declined: false,
            server_tools: None,
            hide_servers_item: false,
            server_hint_dismissed: Vec::new(),
            dismissed_notices: Vec::new(),
            update_holds: Vec::new(),
            translate: false,
            translate_always: Vec::new(),
            backup_retention: Default::default(),
            unknown: serde_json::Map::new(),
        }
    }
}

impl Settings {
    fn path() -> Option<PathBuf> {
        crate::paths::dirs().map(|d| d.data_dir().join("settings.json"))
    }

    /// The saved settings, or `None` when there are none yet.
    ///
    /// A settings file that can't be parsed is renamed to
    /// `settings.json.bad-<unix time>` (so the next save can't overwrite it),
    /// `None` is returned, and [`Settings::load_error`] says what happened.
    pub fn load() -> Option<Settings> {
        Self::load_from(&Self::path()?)
    }

    /// The saved settings, or the defaults for a new user (who then sees the
    /// first-run setup). Settings that exist but can't be read count as set
    /// up: their user isn't new, and is told about the reset instead.
    pub fn load_or_default() -> Settings {
        match Self::path() {
            Some(p) => Self::load_or_default_from(&p),
            None => Settings::default(),
        }
    }

    fn load_or_default_from(p: &Path) -> Settings {
        let existed = p.exists();
        Self::load_from(p).unwrap_or_else(|| Settings {
            setup_done: existed,
            ..Settings::default()
        })
    }

    /// Whether Hopper should show its first-run setup.
    pub fn needs_setup(&self) -> bool {
        !self.setup_done
    }

    fn load_from(p: &Path) -> Option<Settings> {
        let b = match std::fs::read(p) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                let msg = format!("Could not read {}: {e}. Using defaults.", p.display());
                log::warn!("{msg}");
                set_load_error(msg);
                return None;
            }
        };
        match serde_json::from_slice::<Settings>(&b) {
            Ok(mut s) => {
                s.migrate_panels();
                Some(s)
            }
            Err(e) => {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                let mut bad = p.as_os_str().to_owned();
                bad.push(format!(".bad-{ts}"));
                let bad = PathBuf::from(bad);
                let msg = match std::fs::rename(p, &bad) {
                    Ok(()) => format!(
                        "Settings could not be read ({e}); they were reset. The old file is kept as {}.",
                        bad.display()
                    ),
                    Err(re) => format!(
                        "Settings could not be read ({e}), and keeping a copy failed ({re}); \
                         saving settings will replace {}.",
                        p.display()
                    ),
                };
                log::warn!("{msg}");
                set_load_error(msg);
                None
            }
        }
    }

    /// The single Hopper Server of older builds becomes the first entry of
    /// [`Settings::hopper_panels`]; the Panel setting goes back to Crafty.
    pub fn migrate_panels(&mut self) {
        if self.panel_kind != "hopperd" {
            return;
        }
        if !self.panel_url.trim().is_empty() && !self.hopper_panels.iter().any(|p| p.id.is_empty()) {
            let base = crate::hopperd::base_url(&self.panel_url);
            self.hopper_panels.insert(
                0,
                HopperPanel { id: String::new(), name: crate::hopperd::host_name(&base), url: base, ca: self.panel_ca.clone() },
            );
        }
        self.panel_kind = "crafty".into();
        self.panel_url.clear();
        self.panel_ca.clear();
    }

    /// Why the settings file couldn't be used on this run, if it couldn't:
    /// for a notification. Stays set for the whole run.
    pub fn load_error() -> Option<String> {
        LOAD_ERROR.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let p = Self::path().ok_or_else(|| anyhow::anyhow!("no data folder"))?;
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, p)?;
        Ok(())
    }
}

/// A mod put back to an older version because its newer ones need a newer
/// mod (or loader) than the instance has: its update offers are marked
/// with what they need until the needed mod's jar (or the loader) changes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateHold {
    /// The instance's folder.
    pub instance: String,
    pub project_id: u64,
    /// What the newer versions need, for the mark: "TMRV Updated 19.70.0.460+".
    pub needs: String,
    /// The needed mod's jar when it was reverted; for the loader, the
    /// instance's loader ("neoforge-21.1.259").
    pub have: String,
}

/// The key a closed notice is remembered by: where it shows (an
/// instance's or server's folder), what kind it is, and a hash of what it
/// says. FNV-1a, so the hash stays the same across Hopper versions.
pub fn notice_key(place: &str, kind: &str, text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{place}|{kind}|{h:016x}")
}

/// How many closed notices are remembered; the oldest go first.
const NOTICES_KEPT: usize = 300;

impl Settings {
    /// Whether the player closed this notice (see [`notice_key`]).
    pub fn notice_hidden(&self, key: &str) -> bool {
        self.dismissed_notices.iter().any(|k| k == key)
    }

    /// Remembers a closed notice. An older version of the same notice in
    /// the same place said something else and can't come back the same,
    /// so it is forgotten.
    pub fn hide_notice(&mut self, key: &str) {
        let prefix = key.rsplit_once('|').map_or(key, |(p, _)| p);
        self.dismissed_notices
            .retain(|k| k.rsplit_once('|').map_or(k.as_str(), |(p, _)| p) != prefix);
        self.dismissed_notices.push(key.to_string());
        let extra = self.dismissed_notices.len().saturating_sub(NOTICES_KEPT);
        self.dismissed_notices.drain(..extra);
    }

    /// Shows a closed notice again.
    pub fn show_notice(&mut self, key: &str) {
        self.dismissed_notices.retain(|k| k != key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopper-settings-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn panel_widths_load_and_round_trip() {
        // Settings from before panel widths existed still load.
        let old: Settings = serde_json::from_str(r#"{"sidebar_width":22.0}"#).unwrap();
        assert!(old.panel_widths.is_empty());
        let mut s = old;
        s.panel_widths.insert("browse.filters".into(), 18.5);
        s.panel_widths.insert("servers.list".into(), -20.0);
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.panel_widths, s.panel_widths);
    }

    #[test]
    fn server_tools_settings_load_and_round_trip() {
        // Settings from before the switch existed: not chosen yet.
        let old: Settings = serde_json::from_str(r#"{"servers_root":"S:\\servers"}"#).unwrap();
        assert_eq!(old.server_tools, None);
        assert!(!old.hide_servers_item);
        assert!(old.server_hint_dismissed.is_empty());
        let mut s = old;
        s.server_tools = Some(false);
        s.hide_servers_item = true;
        s.server_hint_dismissed.push("C:\\inst\\ATM".into());
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.server_tools, Some(false));
        assert!(back.hide_servers_item);
        assert_eq!(back.server_hint_dismissed, s.server_hint_dismissed);
    }

    #[test]
    fn dismissed_notices_load_and_round_trip() {
        let old: Settings = serde_json::from_str(r#"{"servers_root":"S:\\servers"}"#).unwrap();
        assert!(old.dismissed_notices.is_empty());
        let mut s = old;
        s.dismissed_notices.push("C:\\inst\\ATM|blockers|00ff".into());
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.dismissed_notices, s.dismissed_notices);
    }

    #[test]
    fn notice_keys_hide_show_and_replace() {
        let a = notice_key("C:/inst", "blockers", "ExtendedAE-Plus needs TMRV 19.70");
        let b = notice_key("C:/inst", "blockers", "ExtendedAE-Plus needs TMRV 19.80");
        assert_ne!(a, b);
        assert_eq!(a, notice_key("C:/inst", "blockers", "ExtendedAE-Plus needs TMRV 19.70"));
        let other = notice_key("C:/inst", "dups", "x");
        let mut s = Settings::default();
        s.hide_notice(&a);
        s.hide_notice(&other);
        assert!(s.notice_hidden(&a) && !s.notice_hidden(&b));
        // The changed notice replaces the old one; the other kind stays.
        s.hide_notice(&b);
        assert!(!s.notice_hidden(&a) && s.notice_hidden(&b) && s.notice_hidden(&other));
        s.show_notice(&b);
        assert!(!s.notice_hidden(&b));
        for i in 0..400 {
            s.hide_notice(&notice_key(&format!("p{i}"), "k", "t"));
        }
        assert_eq!(s.dismissed_notices.len(), NOTICES_KEPT);
        assert!(s.notice_hidden(&notice_key("p399", "k", "t")));
    }

    #[test]
    fn java_presets_load_and_default() {
        // Settings from before presets existed still load.
        let old: Settings = serde_json::from_str(r#"{"default_java_args":"-XX:+UseZGC"}"#).unwrap();
        assert!(old.java_presets.is_empty());
        assert_eq!(old.default_java_args, "-XX:+UseZGC");
        let mut s = old;
        s.java_presets.push(JavaPreset { name: "LL8".into(), args: "-XX:+UseZGC".into() });
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.java_presets, s.java_presets);
    }

    #[test]
    fn a_forced_data_dir_never_finds_the_real_folders() {
        let t = temp("forced");
        let home = t.join("home");
        // A real-looking CurseForge folder in home.
        std::fs::create_dir_all(home.join("curseforge/minecraft/Instances/Real Pack")).unwrap();
        let normal = detected_for(None, &home, None);
        assert!(normal.instances_root.starts_with(&*home.to_string_lossy()));
        let forced = t.join("scratch");
        let d = detected_for(Some(&forced), &home, None);
        assert_eq!(d.instances_root, forced.join("instances").to_string_lossy());
        assert_eq!(d.launcher_dir, forced.join("minecraft").to_string_lossy());
        assert_eq!((d.servers_root.as_str(), d.server_host.as_str()), ("", ""));
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn detects_curseforge_folders() {
        let t = temp("detect");
        let (home, appdata) = (t.join("home"), t.join("appdata"));
        let (legacy, legacy_srv) = (t.join("legacy"), t.join("legacy-servers"));
        let detect = || detect_folders_with(&home, Some(&appdata), &legacy, &legacy_srv);

        // Nothing here: CurseForge's standard place, no servers, localhost.
        let d = detect();
        let std_root = home.join("curseforge").join("minecraft");
        assert_eq!(
            d.instances_root,
            std_root.join("Instances").to_string_lossy()
        );
        assert_eq!(d.launcher_dir, std_root.join("Install").to_string_lossy());
        assert_eq!(d.servers_root, "");
        assert_eq!(d.server_host, "localhost");

        // A fallback layout counts only when it exists.
        std::fs::create_dir_all(legacy.join("Instances")).unwrap();
        std::fs::create_dir_all(&legacy_srv).unwrap();
        let d = detect();
        assert_eq!(d.instances_root, legacy.join("Instances").to_string_lossy());
        assert_eq!(d.servers_root, legacy_srv.to_string_lossy());
        assert_eq!(d.server_host, "localhost");

        // The standard folder beats the legacy one.
        std::fs::create_dir_all(std_root.join("Instances")).unwrap();
        assert_eq!(
            detect().instances_root,
            std_root.join("Instances").to_string_lossy()
        );

        // The CurseForge app's custom folder beats both.
        let custom = t.join("Games").join("Curse");
        std::fs::create_dir_all(custom.join("Instances")).unwrap();
        std::fs::create_dir_all(appdata.join("CurseForge")).unwrap();
        let inner = serde_json::json!({ "minecraftRoot": custom.to_string_lossy() }).to_string();
        std::fs::write(
            appdata.join("CurseForge").join("storage.json"),
            serde_json::json!({ "session-counter": "3", "minecraft-settings": inner }).to_string(),
        )
        .unwrap();
        let d = detect();
        assert_eq!(d.instances_root, custom.join("Instances").to_string_lossy());
        assert_eq!(d.launcher_dir, custom.join("Install").to_string_lossy());

        // A recorded folder that's gone is skipped.
        std::fs::remove_dir_all(&custom).unwrap();
        assert_eq!(
            detect().instances_root,
            std_root.join("Instances").to_string_lossy()
        );
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn keeps_settings_from_a_newer_hopper() {
        let s: Settings = serde_json::from_str(r#"{"theme_name":"X","from_the_future":{"a":1}}"#).unwrap();
        assert_eq!(s.theme_name, "X");
        let back = serde_json::to_value(&s).unwrap();
        assert_eq!(back["from_the_future"]["a"], 1);
        assert!(back.get("unknown").is_none());
    }

    #[test]
    fn saved_values_win_over_detection() {
        let s: Settings = serde_json::from_str(
            r#"{"instances_root":"X:\\I","launcher_dir":"X:\\L","servers_root":"","server_host":"rig"}"#,
        )
        .unwrap();
        assert_eq!(s.instances_root, r"X:\I");
        assert_eq!(s.launcher_dir, r"X:\L");
        assert_eq!(s.servers_root, "");
        assert_eq!(s.server_host, "rig");
    }

    #[test]
    fn first_run_setup_only_for_new_users() {
        // A settings file from before the setup existed: already set up.
        let old: Settings = serde_json::from_str(r#"{"instances_root":"X:\\I"}"#).unwrap();
        assert!(old.setup_done);
        assert!(!old.needs_setup());
        let empty: Settings = serde_json::from_str("{}").unwrap();
        assert!(empty.setup_done);
        // Deletes skip the Recycle Bin unless turned off.
        assert!(empty.delete_permanently);
        // No settings file: a new user, who sees the setup.
        assert!(Settings::default().needs_setup());
        // Saved either way, it reads back the same.
        for done in [false, true] {
            let s = Settings { setup_done: done, ..Settings::default() };
            let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
            assert_eq!(back.setup_done, done);
        }

        let t = temp("first-run");
        let p = t.join("settings.json");
        // Nothing saved yet.
        assert!(Settings::load_or_default_from(&p).needs_setup());
        // An existing user's file, without the field.
        std::fs::write(&p, br#"{"theme_name":"Nord","zoom":110}"#).unwrap();
        let s = Settings::load_or_default_from(&p);
        assert!(!s.needs_setup());
        assert_eq!(s.theme_name, "Nord");
        // A new user who saved something before finishing the setup.
        std::fs::write(&p, br#"{"zoom":110,"setup_done":false}"#).unwrap();
        assert!(Settings::load_or_default_from(&p).needs_setup());
        // A file that can't be read: not a new user.
        std::fs::write(&p, b"{ broken").unwrap();
        assert!(!Settings::load_or_default_from(&p).needs_setup());
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn corrupt_settings_are_kept_aside() {
        let t = temp("corrupt");
        let p = t.join("settings.json");
        std::fs::write(&p, b"{ \"instances_root\": ").unwrap();
        assert!(Settings::load_from(&p).is_none());
        assert!(!p.exists());
        let bad: Vec<_> = std::fs::read_dir(&t)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("settings.json.bad-")
            })
            .collect();
        assert_eq!(bad.len(), 1);
        assert_eq!(
            std::fs::read(bad[0].path()).unwrap(),
            b"{ \"instances_root\": "
        );
        assert!(
            Settings::load_error()
                .unwrap()
                .contains("settings.json.bad-")
        );
        // A missing file is just "no settings yet" and keeps the error.
        assert!(Settings::load_from(&p).is_none());
        assert!(Settings::load_error().is_some());
        let _ = std::fs::remove_dir_all(&t);
    }
}

/// `%USERPROFILE%\Hopper Servers`.
pub fn default_hosted_root() -> String {
    if let Some(root) = crate::paths::dirs().and_then(|d| d.root()) {
        return root.join("servers").to_string_lossy().into_owned();
    }
    directories::UserDirs::new()
        .map(|u| u.home_dir().join("Hopper Servers").to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Where Hopper's releases are published; copies update from it.
pub const DEFAULT_UPDATE_REPO: &str = "watdafawx/hopper";
