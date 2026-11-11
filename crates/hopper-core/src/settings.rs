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

/// A peer's instance this PC keeps matching: the local root, the peer and
/// the last code applied (and seen, when it differs).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FollowedRecipe {
    pub peer_id: String,
    pub peer_name: String,
    /// The sender's instance: its [`crate::instances::Instance::id`], or
    /// its folder name from before ids (changed to the id when the next
    /// code arrives with both).
    pub inst: String,
    pub name: String,
    /// The local instance root.
    pub root: String,
    /// The local instance's id, so a renamed folder is found again.
    pub local: String,
    /// The code last applied here.
    pub code: String,
    /// The newest code fetched (equals `code` when up to date).
    pub seen: String,
    /// Files the recipe couldn't describe; the player has to get those.
    pub left_out: Vec<String>,
    /// When the sender last changed the pack (unix seconds, the relay's).
    pub at: u64,
    /// Why no more updates come, once they stopped: "unshared" (they turned
    /// sharing off), "deleted" (the pack is gone on their PC), "left_group"
    /// (it came through a group this PC is no longer in). The instance here
    /// stays as it is.
    pub ended: String,
    /// The friend group "Keep everyone in step" followed it through, if any.
    pub group: String,
}

impl FollowedRecipe {
    /// Whether an incoming code from `from` for their instance `inst` (old
    /// folder name `key`) is the one this follows.
    pub fn matches(&self, from: &str, inst: &str, key: &str) -> bool {
        self.peer_id == from
            && (self.inst == inst || (!key.is_empty() && self.inst.eq_ignore_ascii_case(key)))
    }
}

/// One pack shared with one friend or one friend group.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SharedPack {
    /// The instance's [`crate::instances::Instance::id`].
    pub inst: String,
    /// The mailbox id of the friend who can see it (empty for a group).
    pub friend: String,
    /// The friend group that can see it (empty for one friend).
    pub group: String,
    /// Older builds keyed shares by the instance folder's name. Kept until
    /// the folder is found and `inst` filled in, then cleared.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub key: String,
}

impl SharedPack {
    fn same_target(&self, o: &SharedPack) -> bool {
        self.friend == o.friend && self.group == o.group
    }
}

/// A named set of friends who share packs with each other. The relay holds
/// the group (members, who said yes); this is this PC's copy, to show while
/// the relay can't be reached, plus what only this PC decides.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FriendGroup {
    pub id: String,
    pub name: String,
    /// Mailbox ids of everyone in it who said yes, the owner too.
    pub members: Vec<String>,
    /// Who made it; only they change its name and members.
    pub owner: String,
    /// This PC said yes to "Keep everyone in step": a group pack another
    /// member follows is followed here too.
    pub in_step: bool,
    /// Group packs ("<from>/<inst>") this PC stopped following, so keeping
    /// in step doesn't follow them again.
    pub stopped: Vec<String>,
}

/// One pack to send to the relay: the friends and groups it goes to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PublishTo {
    pub inst: String,
    pub to: Vec<String>,
    pub groups: Vec<String>,
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
    /// How the game's window opens for instances without their own setting;
    /// None until the player sets one (then the stock 1280 × 720 window).
    pub default_window: Option<crate::instances::WindowSetting>,
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
    /// Switching a modpack to another version (roll back, update to a
    /// picked one) opens the comparison first. Off: it installs straight
    /// away, only stopping to ask when your own settings clash.
    pub compare_before_switch: bool,
    /// Pack updates and roll backs are clean by default: every pack mod and
    /// pack file becomes the pack's, like the CurseForge app (your own mods
    /// and settings stay; History undoes the rest). Off: "Keep my changes",
    /// which compares first.
    pub clean_pack_updates: bool,
    /// The choice per instance (or server) folder, once picked there:
    /// true = clean update, false = keep my changes.
    pub pack_update_clean: HashMap<String, bool>,
    /// How each non-mods folder syncs, per server root.
    pub folder_rules: crate::folderrules::Rules,
    /// Peer sync with other PCs running Hopper.
    pub peer_sync: bool,
    /// Mailbox for PCs that can't reach each other directly.
    pub mailbox_url: String,
    /// The name other PCs see. Empty uses this PC's name.
    pub mailbox_name: String,
    /// Recipes this PC follows from paired peers, one per local instance.
    pub followed_recipes: Vec<FollowedRecipe>,
    /// Packs the player shares, one entry per friend or friend group.
    /// Empty means nothing is shared; every pack starts off private.
    pub shared_with: Vec<SharedPack>,
    /// What was last sent to the relay, so a share turned off (or a pack
    /// deleted) can be taken back from the friends who had it.
    pub mailbox_sent: Vec<SharedPack>,
    /// Friend groups this PC is in (see [`FriendGroup`]).
    pub friend_groups: Vec<FriendGroup>,
    /// The newest mailbox event acted on once (a friend joining with a
    /// pack's invite, notices). Every start reads the whole feed again to
    /// list friends' packs; events up to here aren't acted on twice.
    pub mailbox_handled: u64,
    /// Older builds shared a pack with every friend at once. Read so those
    /// players keep sharing, then fanned out into `shared_with` and cleared.
    pub shared_instances: Vec<String>,
    /// The "Share your packs with your friends?" prompt was answered.
    pub share_prompt_done: bool,
    /// The newest friend's pack change (`Event::at`) seen in the Friends
    /// panel. A newer one puts a dot on the title bar's Friends button.
    pub friends_seen: u64,
    /// Per friend (their id): the newest of their packs' changes seen on
    /// their page in the Friends panel. Newer ones show as "2 new" on their
    /// row. A friend with no entry yet counts from `friends_seen`.
    pub friend_seen: HashMap<String, u64>,
    /// Friends' packs hidden from the Friends panel ("<from>/<inst>/<at>").
    /// A change to the pack shows it again.
    pub friends_hidden: Vec<String>,
    /// Where data packs go, per instance root: a folder relative to it.
    pub datapack_dest: HashMap<String, String>,
    /// Hopper opens curseforge.com's Install links; what handled them
    /// before (the CurseForge app), to give them back.
    pub links_prev_handler: Option<String>,
    pub links_enabled: bool,
    /// Offer to open CurseForge and Modrinth links copied in other apps.
    pub clip_links: bool,
    /// Hopper is offered to Windows as a browser, so clicked mod links open
    /// in it (browser.rs).
    pub open_links: bool,
    /// Where Hopper passes every other link: a browser's class name in
    /// Windows; empty for the default browser before Hopper.
    pub open_links_browser: String,
    /// Minimize Hopper while a game runs, and bring it back after.
    pub minimize_while_playing: bool,
    /// What Discord shows of what you're doing (Settings › Discord).
    pub discord: crate::discord::Prefs,
    /// Record FPS, frame times, CPU, RAM, GPU and GC of each game run, to
    /// compare Java argument presets.
    pub record_sessions: bool,
    /// A start with no log progress and an idle CPU for this many seconds
    /// counts as stuck (0: don't check). See [`crate::stuckstart`].
    pub stuck_start_secs: u64,
    /// Mods tab shows a dot per mod for its state on the paired server.
    pub mods_server_column: bool,
    /// Content tabs' table: columns the user hid ("author", "side", "added").
    pub content_hidden_cols: Vec<String>,
    /// Content tabs' table: the user's column order, by key; empty = default.
    pub content_col_order: Vec<String>,
    /// The GPUI Library shows its instance list as a narrow rail.
    pub sidebar_collapsed: bool,
    /// Width of the GPUI instance list, in rem.
    pub sidebar_width: f32,
    /// The Accounts dialog shows its account list as a rail of faces.
    pub accounts_sidebar_collapsed: bool,
    /// Width of the Accounts dialog's account list, in rem.
    pub accounts_sidebar_width: f32,
    /// Width of the project page panel of older builds, in rem. It moved to
    /// `panel_widths["dock"]` (see [`Settings::migrate_dock_width`]).
    pub browse_panel_width: f32,
    /// Widths of the GPUI side panels in rem, by panel key. Negative means
    /// collapsed to a rail, remembering the open width.  Missing = default.
    pub panel_widths: HashMap<String, f32>,
    /// Rows the Java arguments box shows, set by dragging its bottom edge.
    /// 0 fits the box to the flags.
    pub java_args_rows: f32,
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
    /// Server sync works while the server runs (the changes load on its
    /// next restart). On unless turned off: older settings files, from
    /// before it was saved, read as on.
    #[serde(default = "yes")]
    pub sync_while_online: bool,
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
    /// Crafty, MCSS and Pterodactyl panels, any number of each. The single
    /// panel of older builds (`panel_kind` / `panel_url`) moves here.
    pub panels: Vec<OtherPanel>,
    /// Servers reached over SFTP or FTP(S) instead of a shared folder.
    pub file_sites: Vec<FileSite>,
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
    /// Gigabytes the image cache (icons, gallery and description pictures)
    /// may hold before the pictures used longest ago go; 0 is no limit.
    /// Installed mods' icons always stay.
    pub image_cache_gb: u32,
    /// How many website-only CurseForge files Hopper's download window
    /// gets at once (1 to 8), each in its own page.
    pub web_downloads: u8,
    /// Deleted instances, worlds and backups are removed outright instead
    /// of filling the Recycle Bin (on C:) with gigabytes.
    pub delete_permanently: bool,
    /// Mods may be added from GitHub, Maven, Jenkins and download links
    /// (they skip CurseForge's and Modrinth's review). Off by default.
    pub allow_sources: bool,
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
    /// For a mod version whose author wrote no changelog, show what changed
    /// in the files (compared by [`crate::jar_changes`]). Off until the user
    /// turns it on, here or from the notice on the changelog; off, nothing
    /// is compared or downloaded.
    pub files_changes: bool,
    /// Keep the instance and server header still (the blurred art) instead
    /// of the slow color drift.
    pub still_header: bool,
    /// Mouse-wheel scrolling glides instead of jumping a few lines at a
    /// time (touchpads scroll smoothly either way).
    pub smooth_scroll: bool,
    /// Languages (`translate::Lang::code`) translated without asking.
    pub translate_always: Vec<String>,
    /// How long `.hopper-backups` keeps the files behind Undo.
    pub backup_retention: crate::retention::Retention,
    /// The in-game options Hopper writes into every instance it starts.
    pub mc_options: crate::mcoptions::McOptions,
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

/// A Crafty, MCSS or Pterodactyl panel Hopper drives with an API key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OtherPanel {
    /// Stable id; "" for the panel set up before several were possible
    /// (its API key keeps its old name in Credential Manager).
    pub id: String,
    /// "crafty", "mcss" or "pterodactyl" ([`crate::panel::KINDS`]).
    pub kind: String,
    /// What the list calls it ("rig").
    pub name: String,
    pub url: String,
    /// Accept the panel's self-signed certificate (Crafty makes its own).
    pub self_signed: bool,
}

impl Default for OtherPanel {
    fn default() -> Self {
        OtherPanel { id: String::new(), kind: "crafty".into(), name: String::new(), url: String::new(), self_signed: true }
    }
}

impl OtherPanel {
    /// Where its API key is kept in Credential Manager.
    pub fn key_name(&self) -> String {
        crate::secrets::panel_key(&self.id)
    }

    /// Its name, or the host it's on when it has none.
    pub fn label(&self) -> String {
        let n = self.name.trim();
        if !n.is_empty() {
            return n.to_string();
        }
        let host = crate::hopperd::host_name(&crate::panel::base_url(&self.url));
        if host.is_empty() { crate::panel::short_name(&self.kind).to_string() } else { host }
    }
}

/// Server files over SFTP or FTP(S): one server, or a folder of them. The
/// password (or key passphrase) is kept in Credential Manager.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileSite {
    pub id: String,
    /// What the list calls it; the host when empty.
    pub name: String,
    /// "sftp", "ftps" (FTP with TLS) or "ftp" (plain, not secure).
    pub proto: String,
    pub host: String,
    /// 0: the protocol's usual port.
    pub port: u16,
    pub user: String,
    /// The folder on the server: a server, or a folder of servers.
    pub folder: String,
    /// SFTP: a private key file to sign in with instead of a password.
    pub key_file: String,
    /// SFTP: the server's key ("SHA256:…"), pinned on the first connect.
    pub host_key: String,
    /// FTPS: accept a certificate the server makes for itself.
    pub self_signed: bool,
}

impl FileSite {
    pub fn label(&self) -> String {
        let n = self.name.trim();
        if n.is_empty() { self.host.trim().to_string() } else { n.to_string() }
    }
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
            default_window: None,
            java_presets: Vec::new(),
            library_by_recent: false,
            groups: HashMap::new(),
            theme: 0,
            auto_update_servers: false,
            compare_before_switch: false,
            clean_pack_updates: true,
            pack_update_clean: HashMap::new(),
            folder_rules: Default::default(),
            peer_sync: false,
            mailbox_url: crate::mailbox::DEFAULT_URL.to_string(),
            mailbox_name: String::new(),
            followed_recipes: Vec::new(),
            shared_with: Vec::new(),
            mailbox_sent: Vec::new(),
            friend_groups: Vec::new(),
            mailbox_handled: 0,
            shared_instances: Vec::new(),
            share_prompt_done: false,
            friends_seen: 0,
            friend_seen: HashMap::new(),
            friends_hidden: Vec::new(),
            datapack_dest: HashMap::new(),
            links_prev_handler: None,
            links_enabled: false,
            clip_links: false,
            open_links: false,
            open_links_browser: String::new(),
            minimize_while_playing: false,
            discord: Default::default(),
            record_sessions: true,
            stuck_start_secs: crate::stuckstart::DEFAULT_SECS,
            mods_server_column: true,
            content_hidden_cols: Vec::new(),
            content_col_order: Vec::new(),
            sidebar_collapsed: false,
            sidebar_width: 20.0,
            accounts_sidebar_collapsed: false,
            accounts_sidebar_width: 17.0,
            browse_panel_width: 32.0,
            panel_widths: HashMap::new(),
            java_args_rows: 0.,
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
            sync_while_online: true,
            panel_kind: "crafty".into(),
            panel_url: String::new(),
            panel_self_signed: true,
            panel_ca: String::new(),
            hopper_panels: Vec::new(),
            panels: Vec::new(),
            file_sites: Vec::new(),
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
            image_cache_gb: crate::imgcache::DEFAULT_GB,
            web_downloads: 4,
            delete_permanently: true,
            allow_sources: false,
            // A new user: Hopper shows its setup on the first start.
            setup_done: false,
            data_move_declined: false,
            server_tools: None,
            hide_servers_item: false,
            server_hint_dismissed: Vec::new(),
            dismissed_notices: Vec::new(),
            update_holds: Vec::new(),
            translate: false,
            files_changes: false,
            still_header: false,
            smooth_scroll: true,
            translate_always: Vec::new(),
            backup_retention: Default::default(),
            mc_options: Default::default(),
            unknown: serde_json::Map::new(),
        }
    }
}

impl Settings {
    /// Whether pack updates of the instance (or server) at `root` are clean
    /// updates: its own pick, else the default.
    pub fn clean_update(&self, root: &std::path::Path) -> bool {
        self.pack_update_clean
            .get(&*root.to_string_lossy())
            .copied()
            .unwrap_or(self.clean_pack_updates)
    }

    /// Whether the instance `inst` (its id) is shared with the friend
    /// `friend` (their mailbox id) directly, not through a group.
    pub fn shares_with(&self, inst: &str, friend: &str) -> bool {
        self.shared_with
            .iter()
            .any(|s| !s.inst.is_empty() && s.inst == inst && s.friend == friend && s.group.is_empty())
    }

    /// Whether the instance `inst` is shared with the friend group `group`.
    pub fn shares_with_group(&self, inst: &str, group: &str) -> bool {
        self.shared_with
            .iter()
            .any(|s| !s.inst.is_empty() && s.inst == inst && s.group == group && s.friend.is_empty())
    }

    /// Turns one pack's share on or off for one friend.
    pub fn set_shared(&mut self, inst: &str, friend: &str, on: bool) {
        self.shared_with
            .retain(|s| !(s.inst == inst && s.friend == friend && s.group.is_empty()));
        if on && !inst.is_empty() && !friend.is_empty() {
            self.shared_with.push(SharedPack {
                inst: inst.to_string(),
                friend: friend.to_string(),
                ..Default::default()
            });
        }
    }

    /// Turns one pack's share on or off for a friend group.
    pub fn set_shared_group(&mut self, inst: &str, group: &str, on: bool) {
        self.shared_with
            .retain(|s| !(s.inst == inst && s.group == group && s.friend.is_empty()));
        if on && !inst.is_empty() && !group.is_empty() {
            self.shared_with.push(SharedPack {
                inst: inst.to_string(),
                group: group.to_string(),
                ..Default::default()
            });
        }
    }

    /// Shares every instance in `insts` (ids) with every friend on
    /// `friends`. Shares already on stay as they are.
    pub fn share_with_all(&mut self, insts: &[String], friends: &[String]) {
        for inst in insts {
            for friend in friends {
                if !self.shares_with(inst, friend) {
                    self.set_shared(inst, friend, true);
                }
            }
        }
    }

    /// Folds the older all-friends share list (folder names) into
    /// `shared_with`, once the friends are known; the folder names become
    /// ids in [`Settings::reconcile_instances`]. Returns whether anything
    /// changed.
    pub fn migrate_shared(&mut self, friends: &[String]) -> bool {
        if self.shared_instances.is_empty() || friends.is_empty() {
            return false;
        }
        for key in std::mem::take(&mut self.shared_instances) {
            for friend in friends {
                let dup = self
                    .shared_with
                    .iter()
                    .any(|s| s.friend == *friend && s.key.eq_ignore_ascii_case(&key));
                if !dup {
                    self.shared_with.push(SharedPack {
                        key: key.clone(),
                        friend: friend.clone(),
                        ..Default::default()
                    });
                }
            }
        }
        true
    }

    /// Brings shares and follows in line with the instances on this PC,
    /// given as (id, root): shares from before ids (by folder name) get
    /// their instance's id, follows learn their instance's id, and a
    /// followed instance whose folder was renamed is found again by it.
    /// Returns whether anything changed.
    pub fn reconcile_instances(&mut self, live: &[(String, String)]) -> bool {
        let folder = |root: &str| {
            Path::new(root)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let mut changed = false;
        for s in &mut self.shared_with {
            if s.inst.is_empty()
                && !s.key.is_empty()
                && let Some((id, _)) = live.iter().find(|(_, r)| folder(r).eq_ignore_ascii_case(&s.key))
            {
                s.inst = id.clone();
                s.key.clear();
                changed = true;
            }
        }
        // Two old entries may have become the same share.
        let mut seen: Vec<SharedPack> = Vec::new();
        self.shared_with.retain(|s| {
            if s.inst.is_empty() {
                return true;
            }
            if seen.iter().any(|x| x.inst == s.inst && x.same_target(s)) {
                changed = true;
                return false;
            }
            seen.push(s.clone());
            true
        });
        for f in &mut self.followed_recipes {
            if f.local.is_empty() {
                if let Some((id, _)) = live.iter().find(|(_, r)| Path::new(r) == Path::new(&f.root)) {
                    f.local = id.clone();
                    changed = true;
                }
            } else if let Some((_, r)) = live.iter().find(|(id, _)| *id == f.local)
                && Path::new(r) != Path::new(&f.root)
            {
                f.root = r.clone();
                changed = true;
            }
        }
        changed
    }

    /// What to send to the relay: each shared instance on this PC (`live`
    /// ids) with the friends and groups it goes to. A friend appears once
    /// per pack however often it's shared with them; the relay then sends
    /// one copy to someone who gets it directly and through a group. Shares
    /// with someone who isn't a friend any more, or a group this PC isn't
    /// in, are left out.
    pub fn publish_plan(&self, live: &[String], friends: &[String], groups: &[String]) -> Vec<PublishTo> {
        let mut out: Vec<PublishTo> = Vec::new();
        for s in &self.shared_with {
            if s.inst.is_empty() || !live.contains(&s.inst) {
                continue;
            }
            let ix = match out.iter().position(|p| p.inst == s.inst) {
                Some(ix) => ix,
                None => {
                    out.push(PublishTo {
                        inst: s.inst.clone(),
                        ..Default::default()
                    });
                    out.len() - 1
                }
            };
            let p = &mut out[ix];
            if !s.friend.is_empty() && friends.contains(&s.friend) && !p.to.contains(&s.friend) {
                p.to.push(s.friend.clone());
            }
            if !s.group.is_empty() && groups.contains(&s.group) && !p.groups.contains(&s.group) {
                p.groups.push(s.group.clone());
            }
        }
        out.retain(|p| !p.to.is_empty() || !p.groups.is_empty());
        out
    }

    /// Shares sent before that aren't wanted any more, with why: the share
    /// was turned off ("unshared") or the instance is gone from this PC
    /// ("deleted"). The caller tells the relay, then forgets each with
    /// [`Settings::forget_sent`].
    pub fn stale_shares(&self, live: &[String]) -> Vec<(SharedPack, &'static str)> {
        self.mailbox_sent
            .iter()
            .filter_map(|s| {
                if !live.contains(&s.inst) {
                    Some((s.clone(), "deleted"))
                } else if !self
                    .shared_with
                    .iter()
                    .any(|w| w.inst == s.inst && w.same_target(s))
                {
                    Some((s.clone(), "unshared"))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Remembers that `plan` reached the relay.
    pub fn note_sent(&mut self, plan: &PublishTo) {
        let friends = plan.to.iter().map(|f| SharedPack {
            inst: plan.inst.clone(),
            friend: f.clone(),
            ..Default::default()
        });
        let groups = plan.groups.iter().map(|g| SharedPack {
            inst: plan.inst.clone(),
            group: g.clone(),
            ..Default::default()
        });
        for s in friends.chain(groups).collect::<Vec<_>>() {
            if !self.mailbox_sent.contains(&s) {
                self.mailbox_sent.push(s);
            }
        }
    }

    /// Forgets a sent share once the relay has taken it back. A deleted
    /// instance's share goes from `shared_with` too.
    pub fn forget_sent(&mut self, s: &SharedPack, deleted: bool) {
        self.mailbox_sent
            .retain(|x| !(x.inst == s.inst && x.same_target(s)));
        if deleted {
            self.shared_with
                .retain(|x| !(x.inst == s.inst && x.same_target(s)));
        }
    }

    /// The local follows of a friend's pack, as indexes into
    /// `followed_recipes`. Several instances may follow the same pack.
    pub fn followers(&self, from: &str, inst: &str, key: &str) -> Vec<usize> {
        self.followed_recipes
            .iter()
            .enumerate()
            .filter(|(_, f)| f.matches(from, inst, key))
            .map(|(i, _)| i)
            .collect()
    }

    /// Stops following at the instance `root`. The instance stays as it
    /// is. A pack that came through a group is remembered, so keeping the
    /// group in step doesn't follow it again. Returns whether it followed.
    pub fn stop_following(&mut self, root: &str) -> bool {
        let gone: Vec<FollowedRecipe> = self
            .followed_recipes
            .iter()
            .filter(|f| Path::new(&f.root) == Path::new(root))
            .cloned()
            .collect();
        for f in &gone {
            let key = format!("{}/{}", f.peer_id, f.inst);
            for g in &mut self.friend_groups {
                if (f.group.is_empty() || g.id == f.group) && g.members.contains(&f.peer_id) && !g.stopped.contains(&key) {
                    g.stopped.push(key.clone());
                }
            }
        }
        self.followed_recipes
            .retain(|f| Path::new(&f.root) != Path::new(root));
        !gone.is_empty()
    }

    /// Stores the relay's view of the groups this PC is in, keeping what
    /// only this PC decides (packs it stopped following). Shares with a
    /// group this PC is no longer in go too. Returns whether anything
    /// changed.
    pub fn set_friend_groups(&mut self, mut groups: Vec<FriendGroup>) -> bool {
        for g in &mut groups {
            if let Some(old) = self.friend_groups.iter().find(|o| o.id == g.id) {
                g.stopped = old.stopped.clone();
            }
        }
        let before = self.shared_with.len();
        self.shared_with
            .retain(|s| s.group.is_empty() || groups.iter().any(|g| g.id == s.group));
        let changed = groups != self.friend_groups || before != self.shared_with.len();
        self.friend_groups = groups;
        changed
    }

    /// [`Settings::web_downloads`], kept to 1..=8 (an old or hand-edited
    /// file may hold 0 or more).
    pub fn web_downloads(&self) -> usize {
        self.web_downloads.clamp(1, 8) as usize
    }

    fn path() -> Option<PathBuf> {
        crate::paths::dirs().map(|d| d.data_dir().join("settings.json"))
    }

    /// The saved settings, or `None` when there are none yet.
    ///
    /// A settings file that can't be parsed is renamed to
    /// `settings.json.bad-<unix time>` (so the next save can't overwrite it),
    /// `None` is returned, and [`Settings::load_error`] says what happened.
    pub fn load() -> Option<Settings> {
        // A save still queued would be read as the old settings.
        if let Err(e) = flush() {
            log::warn!("saving settings: {e:#}");
        }
        Self::load_from(&Self::path()?)
    }

    /// The saved settings, or the defaults for a new user (who then sees the
    /// first-run setup). Settings that exist but can't be read count as set
    /// up: their user isn't new, and is told about the reset instead.
    pub fn load_or_default() -> Settings {
        if let Err(e) = flush() {
            log::warn!("saving settings: {e:#}");
        }
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
                s.migrate_dock_width();
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

    /// The project panel's width of older builds becomes the right-hand
    /// dock's (the mod peek and the Friends panel share it). Once: the old
    /// value goes back to its default, as the dock drops its own entry
    /// whenever it's dragged back to the default.
    pub fn migrate_dock_width(&mut self) {
        let old = std::mem::replace(&mut self.browse_panel_width, 32.0);
        if old != 32.0 && !self.panel_widths.contains_key("dock") {
            self.panel_widths.insert("dock".into(), old);
        }
    }

    /// The single Hopper Server of older builds becomes the first entry of
    /// [`Settings::hopper_panels`]; the Panel setting goes back to Crafty.
    pub fn migrate_panels(&mut self) {
        if self.panel_kind != "hopperd" {
            // The one Crafty / MCSS / Pterodactyl panel of older builds
            // becomes the first of the list (its key keeps its name).
            if !self.panel_url.trim().is_empty() && !self.panels.iter().any(|p| p.id.is_empty()) {
                self.panels.insert(
                    0,
                    OtherPanel {
                        id: String::new(),
                        kind: self.panel_kind.clone(),
                        name: String::new(),
                        url: self.panel_url.trim().to_string(),
                        self_signed: self.panel_self_signed,
                    },
                );
                self.panel_url.clear();
            }
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

    /// Writes the settings now. A copy [`Settings::save_later`] still has
    /// queued for the file is dropped: this one is newer.
    pub fn save(&self) -> anyhow::Result<()> {
        let p = Self::path().ok_or_else(|| anyhow::anyhow!("no data folder"))?;
        writer::write_now(&p, &serde_json::to_vec_pretty(self)?)
    }

    /// Writes the settings on a background thread, so the UI doesn't wait
    /// for the disk (a few ms a save, on every tab switch). Saves that come
    /// faster than the disk keeps up are merged: only the newest copy is
    /// written. Call [`flush`] before the process ends or another Hopper
    /// reads the file.
    pub fn save_later(&self) -> anyhow::Result<()> {
        let p = Self::path().ok_or_else(|| anyhow::anyhow!("no data folder"))?;
        writer::queue(p, serde_json::to_vec_pretty(self)?);
        Ok(())
    }
}

/// Writes what [`Settings::save_later`] queued, now, and waits for it: on
/// quit, before a restart, before the data folder moves.
pub fn flush() -> anyhow::Result<()> {
    writer::flush()
}

/// [`flush`] for a panic: gives up after a moment instead of waiting
/// forever, in case the panic came from the writer itself.
pub fn flush_on_panic() {
    writer::flush_within(std::time::Duration::from_millis(500));
}

/// The background writer behind [`Settings::save_later`].
mod writer {
    use std::path::{Path, PathBuf};
    use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
    use std::time::{Duration, Instant};

    /// The newest copy waiting for each file.
    static QUEUED: Mutex<Vec<(PathBuf, Vec<u8>)>> = Mutex::new(Vec::new());
    static WAKE: Condvar = Condvar::new();
    /// Held while a file is written, so an older copy still being written
    /// can't land after a newer one.
    static WRITING: Mutex<()> = Mutex::new(());
    /// Whether the thread started.
    static THREAD: OnceLock<bool> = OnceLock::new();

    fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
        m.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A temporary file, then a rename: a crash mid-write leaves the old
    /// settings whole.
    fn write_file(p: &Path, bytes: &[u8]) -> anyhow::Result<()> {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(tmp, p)?;
        Ok(())
    }

    pub(super) fn queue(p: PathBuf, bytes: Vec<u8>) {
        let started = *THREAD.get_or_init(|| std::thread::Builder::new().name("settings-writer".into()).spawn(run).is_ok());
        {
            let mut q = lock(&QUEUED);
            q.retain(|(x, _)| *x != p);
            q.push((p, bytes));
        }
        if started {
            WAKE.notify_one();
        } else if let Err(e) = flush() {
            log::warn!("saving settings: {e:#}");
        }
    }

    fn run() {
        loop {
            {
                let mut q = lock(&QUEUED);
                while q.is_empty() {
                    q = WAKE.wait(q).unwrap_or_else(|e| e.into_inner());
                }
            }
            if let Err(e) = flush() {
                log::warn!("saving settings: {e:#}");
            }
        }
    }

    /// Writes everything queued. Takes `WRITING` before the queue, so
    /// whoever writes last holds the newest copy.
    pub(super) fn flush() -> anyhow::Result<()> {
        let held = lock(&WRITING);
        write_queued(held)
    }

    fn write_queued(_held: MutexGuard<'_, ()>) -> anyhow::Result<()> {
        let mut result = Ok(());
        loop {
            let next = {
                let mut q = lock(&QUEUED);
                if q.is_empty() { None } else { Some(q.remove(0)) }
            };
            let Some((p, bytes)) = next else { break };
            if let Err(e) = write_file(&p, &bytes) {
                result = Err(e);
            }
        }
        result
    }

    pub(super) fn flush_within(wait: Duration) {
        let end = Instant::now() + wait;
        loop {
            match WRITING.try_lock() {
                Ok(held) => {
                    let _ = write_queued(held);
                    return;
                }
                Err(std::sync::TryLockError::Poisoned(e)) => {
                    let _ = write_queued(e.into_inner());
                    return;
                }
                Err(std::sync::TryLockError::WouldBlock) if Instant::now() < end => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => return,
            }
        }
    }

    pub(super) fn write_now(p: &Path, bytes: &[u8]) -> anyhow::Result<()> {
        let _held = lock(&WRITING);
        lock(&QUEUED).retain(|(x, _)| x != p);
        write_file(p, bytes)
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

    /// What the appearance popover changes survives a restart: it goes
    /// through the settings file and comes back the same.
    #[test]
    fn appearance_choices_are_saved_and_loaded() {
        let d = temp("appearance");
        let file = d.join("settings.json");
        let s = Settings {
            theme_name: "Catppuccin Macchiato".into(),
            zoom: 125,
            still_header: true,
            smooth_scroll: false,
            ..Settings::default()
        };
        std::fs::write(&file, serde_json::to_vec_pretty(&s).unwrap()).unwrap();
        let back = Settings::load_from(&file).expect("loads");
        assert_eq!(back.theme_name, "Catppuccin Macchiato");
        assert_eq!(back.zoom, 125);
        assert!(back.still_header);
        assert!(!back.smooth_scroll);
        // Settings from before these existed keep the defaults.
        let old: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!((old.zoom, old.still_header, old.smooth_scroll), (100, false, true));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn dock_width_moves_once() {
        let mut s = Settings { browse_panel_width: 40.0, ..Settings::default() };
        s.migrate_dock_width();
        assert_eq!(s.panel_widths.get("dock"), Some(&40.0));
        assert_eq!(s.browse_panel_width, 32.0);
        // Dragged back to the default, the entry goes; it must stay gone.
        s.panel_widths.remove("dock");
        s.migrate_dock_width();
        assert!(s.panel_widths.get("dock").is_none());
        let mut fresh = Settings::default();
        fresh.migrate_dock_width();
        assert!(fresh.panel_widths.is_empty());
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
    fn the_single_panel_becomes_the_first_of_the_list() {
        let mut s: Settings = serde_json::from_str(
            r#"{"panel_kind":"mcss","panel_url":" https://nas:25560 ","panel_self_signed":false}"#,
        )
        .unwrap();
        s.migrate_panels();
        assert_eq!(s.panels.len(), 1);
        let p = &s.panels[0];
        assert_eq!((p.id.as_str(), p.kind.as_str(), p.url.as_str(), p.self_signed), ("", "mcss", "https://nas:25560", false));
        assert_eq!(p.key_name(), crate::secrets::PANEL_KEY);
        assert!(s.panel_url.is_empty());
        // Loading again doesn't add it twice.
        s.migrate_panels();
        assert_eq!(s.panels.len(), 1);
        // No old panel: nothing to move; an older Hopper Server still moves.
        let mut none = Settings::default();
        none.migrate_panels();
        assert!(none.panels.is_empty());
        let mut hd: Settings = serde_json::from_str(r#"{"panel_kind":"hopperd","panel_url":"rig"}"#).unwrap();
        hd.migrate_panels();
        assert!(hd.panels.is_empty());
        assert_eq!(hd.hopper_panels.len(), 1);
    }

    #[test]
    fn other_panels_name_themselves() {
        let p = OtherPanel { id: "p1".into(), kind: "pterodactyl".into(), url: "https://panel.example.com/".into(), ..Default::default() };
        assert_eq!(p.label(), "panel.example.com");
        assert_eq!(p.key_name(), "panel_api_key.p1");
        let named = OtherPanel { name: " rig ".into(), ..p };
        assert_eq!(named.label(), "rig");
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
    fn server_sync_while_online_is_on_unless_turned_off() {
        // Saved before the setting existed: on.
        let old: Settings = serde_json::from_str(r#"{"instances_root":"X:\\I"}"#).unwrap();
        assert!(old.sync_while_online);
        assert!(Settings::default().sync_while_online);
        let off = Settings { sync_while_online: false, ..Settings::default() };
        let back: Settings = serde_json::from_str(&serde_json::to_string(&off).unwrap()).unwrap();
        assert!(!back.sync_while_online);
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
    fn background_saves_keep_the_newest_copy() {
        let t = temp("writer");
        let p = t.join("settings.json");
        let at = |zoom: u16| serde_json::to_vec_pretty(&Settings { zoom, ..Settings::default() }).unwrap();
        // A burst of saves: whatever the thread wrote along the way, the
        // last one is what's on disk after a flush.
        for z in 100..140 {
            writer::queue(p.clone(), at(z));
        }
        flush().unwrap();
        assert_eq!(Settings::load_from(&p).unwrap().zoom, 139);
        // A direct save after a queued one wins, even if the thread is
        // still busy with the queued one.
        writer::queue(p.clone(), at(150));
        writer::write_now(&p, &at(160)).unwrap();
        flush().unwrap();
        assert_eq!(Settings::load_from(&p).unwrap().zoom, 160);
        // Without a flush the thread still writes it on its own.
        writer::queue(p.clone(), at(170));
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        // (Read directly: a read that meets the rename mustn't count as a
        // load error, which another test checks.)
        let zoom = || std::fs::read(&p).ok().and_then(|b| serde_json::from_slice::<Settings>(&b).ok()).map(|s| s.zoom);
        while zoom() != Some(170) {
            assert!(std::time::Instant::now() < end, "the writer thread never wrote");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // Written through a temporary file that's gone afterwards.
        flush().unwrap();
        assert!(!p.with_extension("json.tmp").exists());
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

    #[test]
    fn a_pack_is_shared_with_one_friend_at_a_time() {
        let mut s = Settings::default();
        s.set_shared("id-pizza", "kimbley", true);
        s.set_shared("id-pizza", "sam", true);
        assert!(s.shares_with("id-pizza", "kimbley"));
        assert!(s.shares_with("id-pizza", "sam"));
        assert!(!s.shares_with("id-pizza", "alex"));

        s.set_shared("id-pizza", "kimbley", false);
        assert!(!s.shares_with("id-pizza", "kimbley"));
        assert!(s.shares_with("id-pizza", "sam"));
        assert_eq!(s.shared_with.len(), 1);

        // A group share is its own entry; the direct one is untouched.
        s.set_shared_group("id-pizza", "crew", true);
        assert!(s.shares_with_group("id-pizza", "crew"));
        assert!(!s.shares_with("id-pizza", "crew"));
        s.set_shared_group("id-pizza", "crew", false);
        assert!(s.shares_with("id-pizza", "sam"));
        assert_eq!(s.shared_with.len(), 1);
    }

    #[test]
    fn share_all_adds_every_friend_once() {
        let mut s = Settings::default();
        let insts = vec!["id-pizza".to_string(), "id-sky".to_string()];
        let friends = vec!["kimbley".to_string(), "sam".to_string()];
        s.share_with_all(&insts, &friends);
        s.share_with_all(&insts, &friends);
        assert_eq!(s.shared_with.len(), 4);
    }

    #[test]
    fn an_old_share_list_is_fanned_out_to_every_friend_once() {
        let mut s = Settings::default();
        s.shared_instances = vec!["Pizza".to_string()];
        // No friends yet: the list waits, nothing is lost.
        assert!(!s.migrate_shared(&[]));
        assert_eq!(s.shared_instances, vec!["Pizza".to_string()]);

        assert!(s.migrate_shared(&["kimbley".to_string(), "sam".to_string()]));
        assert!(s.shared_instances.is_empty());
        // Running it again changes nothing.
        assert!(!s.migrate_shared(&["kimbley".to_string()]));
        assert_eq!(s.shared_with.len(), 2);
        // Once the folder is found, the shares are by its id.
        let live = vec![("id-pizza".to_string(), r"C:\inst\pizza".to_string())];
        assert!(s.reconcile_instances(&live));
        assert!(s.shares_with("id-pizza", "kimbley"));
        assert!(s.shares_with("id-pizza", "sam"));
        assert!(s.shared_with.iter().all(|p| p.key.is_empty()));
    }

    /// Settings saved by the branch before ids: `shared_with` by folder
    /// name, follows without their instance's id.
    #[test]
    fn shares_and_follows_from_before_ids_move_to_ids() {
        let old = r#"{
            "shared_with": [
                {"key": "Pizza", "friend": "sam"},
                {"key": "pizza", "friend": "sam"},
                {"key": "Gone", "friend": "sam"}
            ],
            "followed_recipes": [
                {"peer_id": "sam", "inst": "Their Pack", "root": "C:\\inst\\Mine", "code": "HOP-a", "seen": "HOP-a"}
            ]
        }"#;
        let mut s: Settings = serde_json::from_str(old).unwrap();
        let live = vec![
            ("id-pizza".to_string(), r"C:\inst\Pizza".to_string()),
            ("id-mine".to_string(), r"C:\inst\Mine".to_string()),
        ];
        assert!(s.reconcile_instances(&live));
        // The two spellings were one share; the missing folder waits.
        assert_eq!(s.shared_with.len(), 2);
        assert!(s.shares_with("id-pizza", "sam"));
        assert_eq!(s.shared_with[1].key, "Gone");
        assert_eq!(s.followed_recipes[0].local, "id-mine");
        assert!(!s.reconcile_instances(&live));

        // The followed folder is renamed: found again by its id.
        let renamed = vec![("id-mine".to_string(), r"C:\inst\Mine 2".to_string())];
        assert!(s.reconcile_instances(&renamed));
        assert_eq!(s.followed_recipes[0].root, r"C:\inst\Mine 2");

        // Their next code names the pack by id and by its old folder name;
        // the follow matches either.
        let f = &s.followed_recipes[0];
        assert!(f.matches("sam", "id-theirs", "their pack"));
        assert!(f.matches("sam", "Their Pack", ""));
        assert!(!f.matches("alex", "id-theirs", "Their Pack"));
        assert!(!f.matches("sam", "id-theirs", ""));
    }

    #[test]
    fn a_friend_in_a_group_gets_one_copy_per_pack() {
        let mut s = Settings::default();
        s.set_shared("id-pizza", "sam", true);
        s.set_shared_group("id-pizza", "crew", true);
        s.set_shared("id-sky", "kim", true);
        s.set_shared("id-old", "sam", true);
        // A stale duplicate (an older build) is still one recipient.
        s.shared_with.push(SharedPack {
            inst: "id-pizza".into(),
            friend: "sam".into(),
            ..Default::default()
        });
        s.set_shared_group("id-sky", "left", true);
        let live = vec!["id-pizza".to_string(), "id-sky".to_string()];
        let friends = vec!["sam".to_string()];
        let groups = vec!["crew".to_string()];
        let plan = s.publish_plan(&live, &friends, &groups);
        assert_eq!(
            plan,
            vec![PublishTo {
                inst: "id-pizza".into(),
                to: vec!["sam".into()],
                groups: vec!["crew".into()],
            }]
        );
        // Kim isn't a friend any more and "left" isn't joined: nothing
        // goes for id-sky; id-old isn't on this PC.
    }

    #[test]
    fn shares_turned_off_or_deleted_are_taken_back_once() {
        let mut s = Settings::default();
        s.set_shared("id-pizza", "sam", true);
        s.set_shared_group("id-pizza", "crew", true);
        s.set_shared("id-sky", "sam", true);
        let live = vec!["id-pizza".to_string(), "id-sky".to_string()];
        for p in s.publish_plan(&live, &["sam".to_string()], &["crew".to_string()]) {
            s.note_sent(&p);
        }
        assert_eq!(s.mailbox_sent.len(), 3);
        assert!(s.stale_shares(&live).is_empty());

        s.set_shared_group("id-pizza", "crew", false);
        let live = vec!["id-pizza".to_string()];
        let mut stale = s.stale_shares(&live);
        stale.sort_by(|a, b| a.0.inst.cmp(&b.0.inst));
        assert_eq!(stale.len(), 2);
        assert_eq!((stale[0].0.group.as_str(), stale[0].1), ("crew", "unshared"));
        assert_eq!((stale[1].0.inst.as_str(), stale[1].1), ("id-sky", "deleted"));
        for (p, why) in &stale {
            s.forget_sent(p, *why == "deleted");
        }
        assert!(s.stale_shares(&live).is_empty());
        assert_eq!(s.mailbox_sent.len(), 1);
        // The deleted pack's share went with it; the live one stays.
        assert!(!s.shares_with("id-sky", "sam"));
        assert!(s.shares_with("id-pizza", "sam"));
    }

    #[test]
    fn every_instance_following_a_pack_is_found() {
        let mut s = Settings::default();
        for root in [r"C:\a", r"C:\b"] {
            s.followed_recipes.push(FollowedRecipe {
                peer_id: "sam".into(),
                inst: "id-pizza".into(),
                root: root.into(),
                ..Default::default()
            });
        }
        s.followed_recipes.push(FollowedRecipe {
            peer_id: "sam".into(),
            inst: "id-other".into(),
            root: r"C:\c".into(),
            ..Default::default()
        });
        assert_eq!(s.followers("sam", "id-pizza", ""), vec![0, 1]);
        assert!(s.followers("kim", "id-pizza", "").is_empty());
    }

    #[test]
    fn group_changes_keep_local_choices_and_drop_left_shares() {
        let mut s = Settings::default();
        let crew = FriendGroup {
            id: "crew".into(),
            name: "The server crew".into(),
            members: vec!["me".into(), "sam".into()],
            owner: "sam".into(),
            ..Default::default()
        };
        assert!(s.set_friend_groups(vec![crew.clone()]));
        s.set_shared_group("id-pizza", "crew", true);
        s.followed_recipes.push(FollowedRecipe {
            peer_id: "sam".into(),
            inst: "id-sky".into(),
            root: r"C:\sky".into(),
            group: "crew".into(),
            ..Default::default()
        });
        assert!(s.stop_following(r"C:\sky"));
        assert_eq!(s.friend_groups[0].stopped, vec!["sam/id-sky".to_string()]);
        // A refresh from the relay keeps what this PC stopped following.
        let mut renamed = crew.clone();
        renamed.name = "Crew".into();
        assert!(s.set_friend_groups(vec![renamed]));
        assert_eq!(s.friend_groups[0].stopped, vec!["sam/id-sky".to_string()]);
        assert!(!s.set_friend_groups(s.friend_groups.clone()));
        // Out of the group: its shares go.
        assert!(s.set_friend_groups(Vec::new()));
        assert!(!s.shares_with_group("id-pizza", "crew"));
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
