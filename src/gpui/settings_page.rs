//! The Settings page: a sidebar of categories (with a search over every
//! setting) beside one category at a time, as cards of rows: what it is on
//! the left, its control on the right.
//!
//! Switches, choices and the theme apply at once. Text and folder fields
//! save when they lose focus or on Enter, with a "Saved" tick, or say
//! inline why they didn't. Buttons are only for actions (test, install…).

use crate::widgets::Clickable as _;
use crate::java_form::JavaForm;
use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ 
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const SAVED_KEY: &str = "Saved. Paste a new key to replace it";
/// How wide the cards get.
const MAX_W: f32 = 48.;
/// How long a "Saved" tick and a search highlight stay.
const TICK: Duration = Duration::from_millis(2500);
const FLASH: Duration = Duration::from_millis(1800);

// ---- categories and the settings in them --------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Category {
    General,
    Accounts,
    Folders,
    Launchers,
    Java,
    Servers,
    Content,
    Appearance,
    Updates,
    Advanced,
}

impl Category {
    pub const ALL: [Category; 10] = [
        Category::General,
        Category::Accounts,
        Category::Folders,
        Category::Launchers,
        Category::Java,
        Category::Servers,
        Category::Content,
        Category::Appearance,
        Category::Updates,
        Category::Advanced,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::General => "General",
            Category::Accounts => "Accounts",
            Category::Folders => "Library & folders",
            Category::Launchers => "Other launchers",
            Category::Java => "Playing & Java",
            Category::Servers => "Servers",
            Category::Content => "CurseForge & Modrinth",
            Category::Appearance => "Appearance",
            Category::Updates => "Updates & install",
            Category::Advanced => "Advanced",
        }
    }

    pub fn icon(self) -> IconName {
        match self {
            Category::General => IconName::SlidersHorizontal,
            Category::Accounts => IconName::UserRound,
            Category::Folders => IconName::FolderOpen,
            Category::Launchers => IconName::Layers,
            Category::Java => IconName::Cpu,
            Category::Servers => IconName::Server,
            Category::Content => IconName::Compass,
            Category::Appearance => IconName::Palette,
            Category::Updates => IconName::Download,
            Category::Advanced => IconName::Wrench,
        }
    }

    /// The line under the category's title.
    fn blurb(self) -> &'static str {
        match self {
            Category::General => "How the library behaves day to day.",
            Category::Accounts => "Who plays: your Microsoft accounts.",
            Category::Folders => "Where Hopper finds your instances and Minecraft's launcher.",
            Category::Launchers => "Instances from other launchers, managed where they are, without copying.",
            Category::Java => "How games start, what happens while one runs, and memory and Java for instances that don't set their own.",
            Category::Servers => "Your server packs, servers hosted on this PC, and the panel that runs them.",
            Category::Content => "The CurseForge API key, and where Browse looks first.",
            Category::Appearance => "Theme and size.",
            Category::Updates => "Hopper's version, its updates, and installing it.",
            Category::Advanced => "AI assistants (MCP), the notification-area icon, website links and Hopper's own files.",
        }
    }

    /// A category from a word ("java", "servers", "curseforge"…), for the
    /// palette and the MCP `navigate` tool.
    pub fn find(word: &str) -> Option<Category> {
        let w = word.trim().to_lowercase();
        if w.is_empty() {
            return None;
        }
        Category::ALL
            .into_iter()
            .find(|c| c.label().to_lowercase().starts_with(&w))
            .or_else(|| {
                Category::ALL
                    .into_iter()
                    .find(|c| c.label().to_lowercase().contains(&w))
            })
    }

    /// Every setting's name in it, for the palette's search.
    pub fn keywords(self) -> &'static str {
        static WORDS: OnceLock<Vec<String>> = OnceLock::new();
        let all = WORDS.get_or_init(|| {
            Category::ALL
                .iter()
                .map(|c| {
                    ENTRIES
                        .iter()
                        .filter(|e| e.cat == *c)
                        .map(|e| format!("{} {}", e.label, e.words))
                        .collect::<Vec<_>>()
                        .join(" ")
                        .to_lowercase()
                })
                .collect()
        });
        let ix = Category::ALL.iter().position(|c| *c == self).unwrap_or(0);
        &all[ix]
    }
}

/// One setting, as search finds it.
struct Entry {
    cat: Category,
    id: &'static str,
    label: &'static str,
    desc: &'static str,
    /// Other words it answers to.
    words: &'static str,
}

const fn e(
    cat: Category,
    id: &'static str,
    label: &'static str,
    desc: &'static str,
    words: &'static str,
) -> Entry {
    Entry { cat, id, label, desc, words }
}

use Category as C;

/// Every setting, in page order. Rows take their label and description
/// from here (some descriptions change with the state).
const ENTRIES: &[Entry] = &[
    e(C::General, "start-page", "Hopper opens on", "The page Hopper shows when it starts. Last used picks up where you left off.", "start page library browse servers sync resume continue last"),
    e(C::General, "instance-tab", "Instances open on", "The tab an instance shows first when Hopper starts. Last used opens the tab you had open.", "overview mods tab start resume continue last"),
    e(C::General, "browse-sort", "Browse sorts by", "How Browse orders results when it opens. Last used keeps the sort you picked last.", "sort order popularity downloads newest updated name featured last"),
    e(C::General, "dependents", "Mods others need", "Turning off a mod that others need, or turning on one that needs mods that are off.", "dependencies deps required library"),
    e(C::General, "delete-permanently", "Skip the Recycle Bin", "Deleted instances, worlds and backups are removed for good, so they don't fill up your C: drive. Off sends them to the Recycle Bin.", "delete recycle bin trash permanent disk space"),
    e(C::General, "translate", "Translate mod text", "Offer to translate changelogs and descriptions that aren't in English. It runs on this PC: a language's model downloads only when you first translate from it (about 300 MB, 150 MB once stored), and no text is sent anywhere.", "translate translation language english chinese russian japanese korean changelog description offline"),
    e(C::General, "translate-langs", "Translation models", "Languages downloaded for translation. Remove one to free its space.", "translate translation language model download disk space remove"),
    e(C::Accounts, "account", "Microsoft accounts", "Switch which one starts your games, add another, or sign one out.", "sign in login microsoft profile skin"),
    e(C::Folders, "instances-root", "Instances folder", "Where your instances live. CurseForge's Instances folder works.", "path directory library"),
    e(C::Folders, "launcher-dir", "Launcher folder", "The folder with minecraft.exe. CurseForge keeps one in Curse\\Install.", "minecraft.exe mojang path directory"),
    e(C::Launchers, "launchers", "Other launchers", "Found on this PC, and folders you add. Each one's instances can be managed in Hopper.", "modrinth prism multimc atlauncher gdlauncher ftb import manage"),
    e(C::Java, "launch-with", "Launch with", "Hopper starts games itself with your Microsoft sign-in, like CurseForge. Mojang's launcher opens instead and you press Play there.", "launcher native mojang direct"),
    e(C::Java, "minimize", "Minimize while playing", "Hide Hopper while a game runs, and bring it back when it closes.", "minimize window hide"),
    e(C::Java, "record", "Record how games run", "FPS, frame times, CPU, RAM, GPU and GC pauses of each run, to compare Java arguments. See an instance's Performance tab.", "performance fps stats gc record benchmark"),
    e(C::Java, "memory", "Memory", "For instances that don't set their own.", "ram xmx gb heap"),
    e(C::Java, "java", "Java", "The Java instances use unless they pick one.", "jdk jre runtime path"),
    e(C::Java, "java-args", "Java arguments", "Extra arguments for instances that leave theirs blank.", "jvm flags aikar g1 zgc generator presets xms xmx graalvm"),
    e(C::Java, "get-java", "Get Java", "Downloads into Hopper's own folder; they show in every Java list. GraalVM comes under Oracle's free terms (GraalVM Free Terms and Conditions).", "download temurin graalvm jdk"),
    e(C::Servers, "server-tools", "Server tools", "The Servers page, server buttons on instances, and Peers. Off hides them; Servers stays as a quiet item in the top bar unless you also hide that.", "servers host hosting panel peers opt in turn on off hide"),
    e(C::Servers, "hide-servers", "Hide the Servers item completely", "With server tools off, the top bar shows only Library and Browse. Ctrl+K and the ⋯ menus still reach servers.", "servers top bar nav hide"),
    e(C::Servers, "servers-root", "Server packs", "The folder your server folders are in (a shared folder for a server on another PC).", "folder rig path directory"),
    e(C::Servers, "hosted-root", "Hosted servers", "Servers Hopper runs on this PC live here.", "folder host path directory"),
    e(C::Servers, "server-host", "Server host", "The server PC's name or address, for status pings.", "hostname ping address rig"),
    e(C::Servers, "auto-update", "Update paired servers", "After a pack update, update its paired server too (only while it is offline).", "rig auto update"),
    e(C::Servers, "hopper-panels", "Hopper Servers", "Panels this PC is paired with. Each one is a group in the Servers list, with everything its web page can do.", "hopper server hopperd panels paired remove"),
    e(C::Servers, "panel-add", "Add a panel", "Crafty Controller 4, MCSS or Pterodactyl. Hopper starts, stops and updates servers through it.", "crafty mcss pterodactyl server panel add"),
    e(C::Servers, "other-panel", "Your panel", "Hopper starts, stops and updates servers through it. One at a time: remove it to add a different one.", "crafty mcss pterodactyl server panel remove delete none"),
    e(C::Servers, "panel-kind", "Panel", "Crafty, MCSS or Pterodactyl, for the servers folder above: start, stop, console and backups.", "crafty mcss pterodactyl server panel"),
    e(C::Servers, "panel-url", "Address", "Where the panel answers, like https://myserver:8443.", "url https server panel"),
    e(C::Servers, "panel-find", "Find on the LAN", "Hopper Servers announce themselves on your network.", "hopper server hopperd discover lan"),
    e(C::Servers, "panel-pair", "Add a Hopper Server", "Its address, then Pair: the panel shows the same code, allow it there under Access & devices.", "hopper server hopperd pair code add address"),
    e(C::Servers, "panel-key", "API key", "Kept in Windows Credential Manager, not in settings.json.", "token server panel"),
    e(C::Servers, "panel-cert", "Self-signed certificate", "Accept a certificate the panel makes for itself.", "https ssl tls server panel"),
    e(C::Servers, "panel-test", "Connection", "Whether the panel answers with this address and key.", "test server panel"),
    e(C::Content, "cf-key", "CurseForge API key", "Needed for CurseForge: browsing, installs and updates. Get one at console.curseforge.com.", "api key token console"),
    e(C::Content, "browse-source", "Browse searches", "Which site Browse opens on. Switch any time in Browse too.", "modrinth curseforge source search"),
    e(C::Appearance, "theme", "Theme", "Hopper's colors. Your own theme files go in the folder next to it.", "dark light color colour glass mica"),
    e(C::Appearance, "zoom", "Zoom", "Ctrl + and Ctrl - change it anywhere; Ctrl 0 resets it.", "scale size font bigger smaller"),
    e(C::Updates, "self-update", "Hopper updates", "Hopper checks for new releases at start and every 30 minutes, and updates itself from them.", "version upgrade release"),
    e(C::Updates, "update-repo", "Update source", "The GitHub repository (owner/name) Hopper's releases come from.", "github repo repository"),
    e(C::Updates, "start-menu", "Start menu", "Adds this copy of Hopper to the Start menu without installing it, so you can pin it to Start.", "pin start menu shortcut taskbar portable"),
    e(C::Updates, "install", "Install", "Copies Hopper into your apps with Start menu and desktop shortcuts. No admin rights needed.", "shortcut start menu uninstall"),
    e(C::Updates, "setup", "Setup", "The steps Hopper shows on its first start: install, CurseForge key, instances, servers and theme.", "wizard first run onboarding welcome"),
    e(C::Advanced, "tray-icon", "Notification-area icon", "Hopper's icon by the clock, with notifications for what finishes while it's in the background. From the next start.", "tray notifications background"),
    e(C::Advanced, "links", "Website links", "Open curseforge.com's Install buttons in Hopper. Turning this off gives them back to the CurseForge app.", "curseforge protocol handler url install button"),
    e(C::Advanced, "mcp", "AI assistants (MCP)", "Lets AI assistants such as Claude read your instances, mods, logs and crash reports, and look things up on CurseForge and Modrinth. Only programs on this PC can connect.", "mcp ai claude assistant agent model context protocol chatgpt"),
    e(C::Advanced, "mcp-port", "Port", "Where assistants connect, on this PC only.", "mcp port"),
    e(C::Advanced, "mcp-changes", "Let assistants change instances", "Install mods, switch them on or off and update them. Each change runs as a task you can see.", "mcp install update enable disable"),
    e(C::Advanced, "mcp-window", "Let assistants see and use the window", "Screenshots, clicks and typing in Hopper, for debugging Hopper itself.", "mcp screenshot click debug"),
    e(C::Advanced, "mcp-connect", "Connect an assistant", "Copies what to paste into your assistant's settings.", "mcp claude desktop code config json"),
    e(C::Advanced, "pack-cache", "Downloaded packs", "Kept so comparing and installing an update doesn't download the pack again.", "cache disk space clear delete zip modpack server pack keep days"),
    e(C::Advanced, "data-folder", "Hopper's folder", "Settings, accounts, downloads and caches Hopper keeps. New copies of Hopper keep them in a Hopper Data folder next to hopper.exe.", "appdata settings.json files data folder portable move drive"),
];

fn entry(id: &str) -> &'static Entry {
    ENTRIES
        .iter()
        .find(|e| e.id == id)
        .unwrap_or_else(|| panic!("no settings entry {id}"))
}

/// A setting's id as the page knows it (`"zoom"`, `"cf-key"`…).
pub fn entry_id(id: &str) -> Option<&'static str> {
    ENTRIES.iter().find(|e| e.id == id).map(|e| e.id)
}

/// The settings matching every word of `query` (in page order).
fn matches(query: &str) -> Vec<&'static Entry> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Vec::new();
    }
    ENTRIES
        .iter()
        .filter(|e| {
            let hay = format!("{} {} {} {}", e.label, e.desc, e.words, e.cat.label()).to_lowercase();
            words.iter().all(|w| hay.contains(w.as_str()))
        })
        .collect()
}

// ---- text fields ----------------------------------------------------------------

/// The text fields that save on their own (when they lose focus, or on Enter).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Text {
    Instances,
    Launcher,
    ServerPacks,
    Hosted,
    Host,
    PanelUrl,
    UpdateRepo,
    McpPort,
}

impl Text {
    const ALL: [Text; 8] = [
        Text::Instances,
        Text::Launcher,
        Text::ServerPacks,
        Text::Hosted,
        Text::Host,
        Text::PanelUrl,
        Text::UpdateRepo,
        Text::McpPort,
    ];

    fn saved(self, s: &hopper::settings::Settings) -> &str {
        match self {
            Text::Instances => &s.instances_root,
            Text::Launcher => &s.launcher_dir,
            Text::ServerPacks => &s.servers_root,
            Text::Hosted => &s.hosted_root,
            Text::Host => &s.server_host,
            Text::PanelUrl => &s.panel_url,
            Text::UpdateRepo => &s.update_repo,
            Text::McpPort => &s.mcp_port,
        }
    }

    fn set(self, s: &mut hopper::settings::Settings, v: String) {
        match self {
            Text::Instances => s.instances_root = v,
            Text::Launcher => s.launcher_dir = v,
            Text::ServerPacks => s.servers_root = v,
            Text::Hosted => s.hosted_root = v,
            Text::Host => s.server_host = v,
            Text::PanelUrl => s.panel_url = v,
            Text::UpdateRepo => s.update_repo = v,
            Text::McpPort => s.mcp_port = v,
        }
    }

    /// The library is read again after it changes.
    fn refreshes(self) -> bool {
        matches!(
            self,
            Text::Instances | Text::Launcher | Text::ServerPacks | Text::Hosted | Text::Host
        )
    }

    /// Why `v` can't be saved, if it can't.
    fn problem(self, v: &str) -> Option<&'static str> {
        if v.is_empty() {
            return None;
        }
        match self {
            Text::Instances | Text::Launcher | Text::ServerPacks if !Path::new(v).is_dir() => {
                Some("Folder not found, so not saved.")
            }
            Text::PanelUrl if !(v.starts_with("https://") || v.starts_with("http://")) => {
                Some("Start it with https:// (or http://).")
            }
            Text::UpdateRepo if v.split('/').filter(|p| !p.trim().is_empty()).count() != 2 => {
                Some("Use owner/name, like watdafawx/hopper.")
            }
            Text::McpPort => match v.parse::<u16>() {
                Ok(47802) => Some("47802 is Hopper's link handler; pick another."),
                Ok(p) if p >= 1024 => None,
                _ => Some("Use a number from 1024 to 65535."),
            },
            _ => None,
        }
    }
}

/// What a text field says under itself after a save.
enum Note {
    Saved(Instant),
    /// Why the value wasn't saved, and that value.
    Problem(&'static str, String),
    Info(&'static str),
}

// ---- the page --------------------------------------------------------------------

pub struct SettingsPage {
    store: Entity<Store>,
    /// The category list down the side, resizable.
    nav_panel: widgets::SidePanel,
    category: Category,
    search: Entity<InputState>,
    query: String,
    scroll: ScrollHandle,
    /// The setting to bring into view at the next render.
    reveal: Option<&'static str>,
    /// The setting just jumped to, lit up for a moment, and its turn.
    flash: Option<(&'static str, usize)>,
    flashes: usize,
    texts: HashMap<Text, Entity<InputState>>,
    notes: HashMap<Text, Note>,
    /// A new panel API key; empty keeps the saved one.
    panel_token: Entity<InputState>,
    panel_key_saved: bool,
    /// The last connection test: Ok(summary) or Err(why).
    panel_test: Option<Result<String, String>>,
    panel_testing: bool,
    /// The add / edit form of the other panel is open.
    panel_form: bool,
    /// The form adds a panel (Cancel forgets it) rather than editing one.
    panel_form_new: bool,
    /// Remove on the other panel was clicked once (asks again).
    panel_removing: bool,
    /// Hopper Servers found on the LAN (None until looked for).
    found: Option<Vec<hopper::hopperd::Found>>,
    finding: bool,
    pairing: Pairing,
    /// The address of a Hopper Server to pair with.
    new_panel: Entity<InputState>,
    /// The address and name a panel announced on the LAN, when picked
    /// from there.
    new_panel_name: Option<(String, String)>,
    /// The paired panel being paired again (by id).
    repair: Option<String>,
    /// The panel whose Remove was clicked once (asks again).
    removing: Option<String>,
    /// A new CurseForge key; empty keeps the current one.
    cf_key: Entity<InputState>,
    cf_test: Option<Result<String, String>>,
    cf_testing: bool,
    installing: bool,
    java: Entity<JavaForm>,
    launchers: Entity<crate::launchers_list::LaunchersList>,
    _subscriptions: Vec<Subscription>,
}

/// Pairing with a Hopper Server.
#[derive(Clone, Debug, PartialEq)]
enum Pairing {
    Idle,
    Asking,
    /// Waiting for the owner to allow it; both sides show `code`. And the
    /// start of the certificate fingerprint.
    Waiting(String, String),
    Done(String),
    Failed(String),
}

/// What the Settings page asks of the app.
pub enum SettingsEvent {
    /// "Run setup again".
    RunSetup,
    /// Browse should search Modrinth (true) or CurseForge.
    BrowseSource(bool),
}

impl EventEmitter<SettingsEvent> for SettingsPage {}

impl SettingsPage {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let s = store.read(cx).settings().clone();
        let mut texts = HashMap::new();
        for t in Text::ALL {
            let value = t.saved(&s).to_string();
            let placeholder = match t {
                Text::PanelUrl => "https://myserver:8443",
                Text::UpdateRepo => hopper::settings::DEFAULT_UPDATE_REPO,
                Text::McpPort => "47801",
                Text::Host => "myserver",
                _ => "",
            };
            texts.insert(
                t,
                cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(placeholder)
                        .default_value(value)
                }),
            );
        }
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search settings")
                .clean_on_escape()
        });
        let cf_key = cx.new(|cx| {
            InputState::new(window, cx).masked(true).placeholder(
                if hopper::cf::api_key_source().is_some_and(|(_, s)| s == hopper::cf::KeySource::Saved) {
                    SAVED_KEY
                } else {
                    "Paste your CurseForge API key"
                },
            )
        });
        let panel_key_saved = hopper::panel::token().is_some();
        let panel_token = cx.new(|cx| {
            InputState::new(window, cx).masked(true).placeholder(if panel_key_saved {
                SAVED_KEY
            } else {
                "Paste the panel's API key"
            })
        });
        let new_panel = cx.new(|cx| InputState::new(window, cx).placeholder("rig, 192.168.50.3 or https://panel.example.com"));
        let java = cx.new(|cx| JavaForm::new(store.clone(), None, window, cx));
        java.update(cx, |j, cx| {
            j.set(
                s.default_memory_mb,
                &s.default_java_path,
                &s.default_java_args,
                None,
                window,
                cx,
            )
        });
        let mut subs = vec![
            // Java defaults apply as they change.
            cx.observe(&java, |this, java, cx| {
                let (mem, path, args) = java.read(cx).values();
                this.store.update(cx, |s, cx| {
                    s.update_settings(
                        |st| {
                            st.default_memory_mb = mem.max(1024);
                            st.default_java_path = path;
                            st.default_java_args = args;
                        },
                        cx,
                    )
                });
                cx.notify();
            }),
            cx.subscribe(&search, |this, state, e: &InputEvent, cx| match e {
                InputEvent::Change => {
                    this.query = state.read(cx).value().to_string();
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    if let Some(hit) = matches(&this.query).first() {
                        this.jump(hit.id, cx);
                    }
                }
                _ => {}
            }),
            cx.subscribe_in(&cf_key, window, |this, _, e: &InputEvent, window, cx| match e {
                InputEvent::PressEnter { .. } => this.save_cf_key(window, cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            }),
            cx.subscribe_in(&panel_token, window, |this, _, e: &InputEvent, window, cx| match e {
                InputEvent::PressEnter { .. } => this.save_panel(window, cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            }),
            cx.subscribe_in(&new_panel, window, |this, state, e: &InputEvent, _, cx| match e {
                InputEvent::PressEnter { .. } if !state.read(cx).value().trim().is_empty() => {
                    this.repair = None;
                    this.pair_panel(cx)
                }
                InputEvent::Change => cx.notify(),
                _ => {}
            }),
            // Settings changed elsewhere (the setup wizard): show them,
            // unless that field is being edited.
            cx.observe_in(&store, window, |this, store, window, cx| {
                let s = store.read(cx).settings().clone();
                for t in Text::ALL {
                    let input = this.texts[&t].clone();
                    let saved = t.saved(&s).to_string();
                    let shown = input.read(cx).value().trim().to_string();
                    let editing = input.read(cx).focus_handle(cx).is_focused(window);
                    if shown != saved && !editing && !matches!(this.notes.get(&t), Some(Note::Problem(..))) {
                        input.update(cx, |i, cx| i.set_value(saved, window, cx));
                    }
                }
            }),
        ];
        for t in Text::ALL {
            subs.push(cx.subscribe(&texts[&t], move |this, _, e: &InputEvent, cx| match e {
                InputEvent::Blur | InputEvent::PressEnter { .. } => this.commit(t, cx),
                // Editing what was turned down clears why. (Enter also
                // reports a change, of nothing.)
                InputEvent::Change => {
                    let now = this.texts[&t].read(cx).value().trim().to_string();
                    if matches!(this.notes.get(&t), Some(Note::Problem(_, was)) if *was != now) {
                        this.notes.remove(&t);
                    }
                    cx.notify();
                }
                _ => {}
            }));
        }
        let launchers = cx.new(|_| crate::launchers_list::LaunchersList::new(store.clone()));
        subs.push(cx.observe(&launchers, |_, _, cx| cx.notify()));
        Self {
            store,
            nav_panel: widgets::SidePanel::new(widgets::PanelSpec::new("settings.nav", 15., 12., 24.)),
            category: Category::General,
            search,
            query: String::new(),
            scroll: ScrollHandle::new(),
            reveal: None,
            flash: None,
            flashes: 0,
            texts,
            notes: HashMap::new(),
            panel_token,
            panel_key_saved,
            panel_test: None,
            panel_form: false,
            panel_form_new: false,
            panel_removing: false,
            panel_testing: false,
            found: None,
            finding: false,
            pairing: Pairing::Idle,
            new_panel,
            new_panel_name: None,
            repair: None,
            removing: None,
            cf_key,
            cf_test: None,
            cf_testing: false,
            installing: false,
            launchers,
            java,
            _subscriptions: subs,
        }
    }

    /// The category shown, for back and forward.
    pub(crate) fn category(&self) -> Category {
        self.category
    }

    /// Shows a category (from the palette, Ctrl+, or another page).
    pub fn show(&mut self, category: Category, cx: &mut Context<Self>) {
        if self.category != category {
            self.category = category;
            self.scroll.set_offset(point(px(0.), px(0.)));
        }
        cx.notify();
    }

    /// Shows a setting and lights it up for a moment.
    pub fn jump(&mut self, id: &'static str, cx: &mut Context<Self>) {
        self.show(entry(id).cat, cx);
        // The other panel's fields only show in its add / edit form.
        if matches!(id, "panel-kind" | "panel-url" | "panel-key" | "panel-cert" | "panel-test") && !self.panel_form {
            self.panel_form = true;
            self.panel_form_new = !self.other_panel_added(self.store.read(cx).settings());
        }
        self.reveal = Some(id);
        self.flashes += 1;
        let turn = self.flashes;
        self.flash = Some((id, turn));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FLASH).await;
            this.update(cx, |this, cx| {
                if this.flash.is_some_and(|(_, t)| t == turn) {
                    this.flash = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.focus(window, cx));
    }

    /// Saves a text field when it changed and makes sense.
    fn commit(&mut self, t: Text, cx: &mut Context<Self>) {
        let typed = self.texts[&t].read(cx).value().trim().to_string();
        let saved = t.saved(self.store.read(cx).settings()).to_string();
        if typed == saved {
            if matches!(self.notes.get(&t), Some(Note::Problem(..))) {
                self.notes.remove(&t);
                cx.notify();
            }
            return;
        }
        if let Some(why) = t.problem(&typed) {
            self.notes.insert(t, Note::Problem(why, typed));
            cx.notify();
            return;
        }
        let missing = t == Text::Hosted && !typed.is_empty() && !Path::new(&typed).is_dir();
        self.store.update(cx, |s, cx| {
            s.update_settings(|st| t.set(st, typed), cx);
            if t.refreshes() {
                s.refresh();
            }
            cx.notify();
        });
        self.notes.insert(
            t,
            if missing {
                Note::Info("Saved. The folder doesn't exist yet.")
            } else {
                Note::Saved(Instant::now())
            },
        );
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TICK + Duration::from_millis(100)).await;
            this.update(cx, |this, cx| {
                if matches!(this.notes.get(&t), Some(Note::Saved(at)) if at.elapsed() >= TICK) {
                    this.notes.remove(&t);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// The "Saved" tick, or why it wasn't saved, under a text field.
    fn note(&self, t: Text, cx: &App) -> Option<AnyElement> {
        let theme = cx.theme();
        let line = |icon: IconName, text: &'static str, color: Hsla| {
            h_flex()
                .gap_1()
                .text_xs()
                .text_color(color)
                .child(Icon::new(icon).xsmall())
                .child(text)
                .into_any_element()
        };
        match self.notes.get(&t)? {
            Note::Saved(at) if at.elapsed() < TICK => {
                Some(line(IconName::Check, "Saved", theme.success))
            }
            Note::Saved(_) => None,
            Note::Problem(why, _) => Some(line(IconName::TriangleAlert, why, theme.warning)),
            Note::Info(what) => Some(line(IconName::Info, what, theme.muted_foreground)),
        }
    }

    /// A folder field with Browse…; both save at once.
    fn folder(&self, t: Text, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let input = self.texts[&t].clone();
        h_flex()
            .gap_2()
            .child(div().w(rems(20.)).child(Input::new(&input).small()))
            .child(
                Button::new(SharedString::from(format!("browse-{t:?}")))
                    .outline()
                    .small()
                    .label("Browse…")
                    .on_click(cx.listener(move |_, _, window, cx| {
                        let rx = cx.prompt_for_paths(PathPromptOptions {
                            files: false,
                            directories: true,
                            multiple: false,
                            prompt: Some("Use this folder".into()),
                        });
                        let input = input.clone();
                        cx.spawn_in(window, async move |this, cx| {
                            if let Ok(Ok(Some(p))) = rx.await
                                && let Some(p) = p.into_iter().next()
                            {
                                let p = p.to_string_lossy().into_owned();
                                cx.update(|window, cx| {
                                    input.update(cx, |s, cx| s.set_value(p, window, cx));
                                    this.update(cx, |this, cx| this.commit(t, cx)).ok();
                                })
                                .ok();
                            }
                        })
                        .detach();
                    })),
            )
    }

    /// Saves the panel's address and (when typed) its API key, then tests it.
    fn save_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit(Text::PanelUrl, cx);
        // Saved now, so leaving the form keeps it (Done, not Cancel).
        self.panel_form_new = false;
        let key = self.panel_token.read(cx).value().trim().to_string();
        if !key.is_empty() {
            if let Err(e) = hopper::panel::set_token(&key) {
                window.push_notification(Notification::error(format!("{e:#}")), cx);
                return;
            }
            self.panel_key_saved = true;
            self.panel_token.update(cx, |s, cx| {
                s.set_value(String::new(), window, cx);
                s.set_placeholder(SAVED_KEY, window, cx);
            });
        }
        self.test_panel(cx);
    }

    fn test_panel(&mut self, cx: &mut Context<Self>) {
        let s = self.store.read(cx).settings().clone();
        let Some(panel) = hopper::panel::from_settings(&s) else {
            self.panel_test = Some(Err("Add the address and an API key first.".into()));
            cx.notify();
            return;
        };
        self.panel_testing = true;
        self.panel_test = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move {
                    panel
                        .servers()
                        .map(|list| {
                            let names: Vec<String> = list.iter().map(|s| s.name.clone()).collect();
                            format!("Connected: {} servers ({})", list.len(), names.join(", "))
                        })
                        .map_err(|e| format!("{e:#}"))
                })
                .await;
            this.update(cx, |this, cx| {
                this.panel_testing = false;
                if r.is_ok() {
                    this.panel_form = false;
                    this.panel_form_new = false;
                }
                this.panel_test = Some(r);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn save_cf_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.cf_key.read(cx).value().trim().to_string();
        if key.is_empty() {
            return;
        }
        match hopper::cf::set_api_key(&key) {
            Ok(()) => {
                self.cf_key.update(cx, |s, cx| {
                    s.set_value(String::new(), window, cx);
                    s.set_placeholder(SAVED_KEY, window, cx);
                });
                self.cf_test = None;
                // Browse and updates can run now.
                self.store.update(cx, |s, cx| {
                    s.refresh();
                    cx.notify();
                });
                window.push_notification(Notification::success("CurseForge key saved."), cx);
            }
            Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
        }
        cx.notify();
    }

    /// Asks CurseForge whether the typed key (or else the current one) works.
    fn test_cf_key(&mut self, cx: &mut Context<Self>) {
        let typed = self.cf_key.read(cx).value().trim().to_string();
        let pasted = !typed.is_empty();
        let key = Some(typed)
            .filter(|k| !k.is_empty())
            .or_else(|| hopper::cf::api_key().map(String::from));
        let Some(key) = key else {
            self.cf_test = Some(Err("Paste a key first.".into()));
            cx.notify();
            return;
        };
        self.cf_testing = true;
        self.cf_test = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move {
                    hopper::cf::test_key(&key)
                        .map(|()| {
                            if pasted { "CurseForge accepts this key." } else { "CurseForge accepts the key in use." }.to_string()
                        })
                        .map_err(|e| format!("{e:#}"))
                })
                .await;
            this.update(cx, |this, cx| {
                this.cf_testing = false;
                this.cf_test = Some(r);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.installing = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let r = cx
                .background_spawn(async move { hopper::setup::install(true) })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.installing = false;
                let n = match r {
                    Ok(_) => Notification::success(
                        "Hopper is installed. Find it in the Start menu or on the desktop.",
                    ),
                    Err(e) => Notification::error(format!("{e:#}")),
                };
                window.push_notification(n, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn switch(
        &self,
        id: &'static str,
        on: bool,
        set: fn(&mut hopper::settings::Settings, bool),
        cx: &mut Context<Self>,
    ) -> Switch {
        Switch::new(id)
            .checked(on)
            .on_click(cx.listener(move |this, v: &bool, _, cx| {
                let v = *v;
                this.store
                    .update(cx, |s, cx| s.update_settings(|st| set(st, v), cx));
            }))
    }

    /// A picker over `options` (settings key, label) with "Last used"
    /// (an empty key) first.
    fn key_picker(
        &self,
        id: &'static str,
        current: &str,
        options: Vec<(String, String)>,
        set: fn(&mut hopper::settings::Settings, String),
        rem: Pixels,
    ) -> impl IntoElement {
        let mut all = vec![(String::new(), "Last used".to_string())];
        all.extend(options);
        let ix = all.iter().position(|(k, _)| k == current).unwrap_or(0);
        let keys: Vec<String> = all.iter().map(|(k, _)| k.clone()).collect();
        let store = self.store.clone();
        div().w(rems(15.)).child(widgets::picker(
            id,
            all[ix].1.clone(),
            all.iter().enumerate().map(|(i, (_, l))| (l.clone(), i == ix)).collect(),
            rem,
            Rc::new(move |i, _, cx| {
                let v = keys.get(i).cloned().unwrap_or_default();
                store.update(cx, |s, cx| s.update_settings(|st| set(st, v), cx));
            }),
        ))
    }

    // ---- categories ----------------------------------------------------------------

    fn general(&self, s: &hopper::settings::Settings, rem: Pixels, cx: &mut Context<Self>) -> Vec<Block> {
        let mode = s.dependents.clone();
        let deps_ix = match mode.as_str() {
            "all" => 1,
            "only" => 2,
            _ => 0,
        };
        vec![Block::card(
            None,
            vec![
                Row::new("start-page").control(self.key_picker(
                    "start-page",
                    &s.start_page,
                    crate::app::Page::ALL.iter().map(|p| (p.key().to_string(), p.label().to_string())).collect(),
                    |st, v| st.start_page = v,
                    rem,
                )),
                Row::new("instance-tab").control(self.key_picker(
                    "instance-tab",
                    &s.instance_tab,
                    crate::library::InstTab::ALL.iter().map(|t| (t.key(), t.label().to_string())).collect(),
                    |st, v| st.instance_tab = v,
                    rem,
                )),
                Row::new("browse-sort").control(self.key_picker(
                    "browse-sort",
                    &s.browse_sort,
                    hopper::cf::SortField::ALL.iter().map(|f| (f.key().to_string(), f.name().to_string())).collect(),
                    |st, v| st.browse_sort = v,
                    rem,
                )),
                Row::new("dependents").control(div().w(rems(15.)).child(widgets::picker(
                    "dependents",
                    ["Ask each time", "Change them too", "Only change that mod"][deps_ix].to_string(),
                    ["Ask each time", "Change them too", "Only change that mod"]
                        .iter()
                        .enumerate()
                        .map(|(i, l)| (l.to_string(), i == deps_ix))
                        .collect(),
                    rem,
                    {
                        let store = self.store.clone();
                        Rc::new(move |ix, _, cx| {
                            let v = ["ask", "all", "only"][ix.min(2)].to_string();
                            store.update(cx, |s, cx| s.update_settings(|st| st.dependents = v, cx));
                        })
                    },
                ))),
                Row::new("delete-permanently").control(self.switch(
                    "delete-permanently",
                    s.delete_permanently,
                    |st, v| st.delete_permanently = v,
                    cx,
                )),
                Row::new("translate").control(self.switch("translate", s.translate, |st, v| st.translate = v, cx)),
                Row::new("translate-langs").below(self.translate_models(cx)),
            ],
        )]
    }

    /// The downloaded translation models, each with its size and Remove.
    fn translate_models(&self, cx: &mut Context<Self>) -> AnyElement {
        let list = hopper::translate::installed_list();
        if list.is_empty() {
            return widgets::caption("None yet. A language downloads the first time you translate from it.", cx)
                .into_any_element();
        }
        let always = self.store.read(cx).settings().translate_always.clone();
        v_flex()
            .gap_1()
            .children(list.into_iter().map(|(lang, bytes)| {
                let store = self.store.clone();
                h_flex()
                    .gap_3()
                    .child(div().text_sm().child(format!("{} to English", lang.name)))
                    .when(always.iter().any(|c| c == lang.code), |r| {
                        r.child(widgets::chip(cx.theme().info).child("Always"))
                    })
                    .child(div().flex_1())
                    .child(widgets::caption(format!("{} MB", bytes >> 20), cx))
                    .child(
                        Button::new(SharedString::from(format!("tr-remove-{}", lang.code)))
                            .label("Remove")
                            .ghost()
                            .small()
                            .on_click(cx.listener(move |_, _, _, cx| {
                                store.update(cx, |s, cx| s.tr_remove(lang, cx));
                                cx.notify();
                            })),
                    )
            }))
            .into_any_element()
    }

    fn accounts(&self, s: &hopper::settings::Settings, cx: &mut Context<Self>) -> Vec<Block> {
        let theme = cx.theme().clone();
        let store = self.store.read(cx);
        let accounts = store.accounts().clone();
        let legacy = store.legacy_login() && accounts.list.is_empty();
        let mut list = accounts.list.clone();
        // Active first, then by last use.
        let is_active = |a: &hopper::accounts::Saved| accounts.active.as_deref() == Some(a.id.as_str());
        list.sort_by(|a, b| is_active(b).cmp(&is_active(a)).then(b.last_used.cmp(&a.last_used)));
        let rows: Vec<AnyElement> = list
            .iter()
            .map(|a| {
                let active = is_active(a);
                let sub = if active {
                    "Starts your games".to_string()
                } else {
                    a.last_used
                        .as_deref()
                        .map(|t| format!("Used {}", hopper::time::relative(t).to_lowercase()))
                        .unwrap_or_else(|| "Not used yet".into())
                };
                let (id, saved) = (a.id.clone(), a.clone());
                h_flex()
                    .gap_3()
                    .py_1()
                    .child(crate::accounts::head(store, a, rems(2.25), cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(div().font_semibold().truncate().child(a.name.clone()))
                                    .when(active, |r| r.child(widgets::pill("Active", theme.success, true))),
                            )
                            .child(widgets::caption(sub, cx).text_xs()),
                    )
                    .when(!active, |r| {
                        r.child(
                            Button::new(SharedString::from(format!("switch:{}", a.id)))
                                .small()
                                .outline()
                                .label("Switch")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    crate::accounts::switch_account(&this.store, id.clone(), window, cx)
                                })),
                        )
                    })
                    .child(
                        Button::new(SharedString::from(format!("signout:{}", a.id)))
                            .small()
                            .ghost()
                            .icon(IconName::LogOut)
                            .tooltip("Sign out")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                crate::accounts::ask_sign_out(this.store.clone(), saved.clone(), window, cx)
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        let empty = rows.is_empty() && !legacy;
        let below = v_flex()
            .gap_1()
            .children(rows)
            .when(legacy, |r| {
                r.child(
                    h_flex()
                        .gap_3()
                        .child(div().font_semibold().child(s.signed_in_as.clone().unwrap_or_else(|| "Saved sign-in".into())))
                        .child(widgets::caption("Saved sign-in", cx).text_xs()),
                )
            })
            .when(empty, |r| r.child(widgets::caption("Not signed in. Add an account to start games directly.", cx)));
        let any = !list.is_empty() || legacy;
        let account = Row::new("account")
            .control(
                h_flex()
                    .gap_2()
                    .when(any, |r| {
                        r.child(
                            Button::new("manage-accounts")
                                .small()
                                .ghost()
                                .icon(IconName::Shirt)
                                .label("Skins & names…")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    crate::accounts::open(this.store.clone(), window, cx)
                                })),
                        )
                    })
                    .child(
                        Button::new("add-account")
                            .small()
                            .when(any, |b| b.outline())
                            .when(!any, |b| b.primary())
                            .icon(IconName::UserPlus)
                            .label(if any { "Add account" } else { "Sign in with Microsoft" })
                            .on_click(cx.listener(|this, _, window, cx| {
                                crate::accounts::add_account(&this.store, window, cx)
                            })),
                    ),
            )
            .below(below);
        vec![Block::card(None, vec![account])]
    }

    /// "Launch with": Mojang's launcher or Hopper's own start.
    fn launch_row(&self, s: &hopper::settings::Settings, cx: &mut Context<Self>) -> Row {
        Row::new("launch-with").below(
            v_flex()
                .gap_2()
                .child(
                    RadioGroup::new("launch-with")
                        .children([
                            "Mojang's launcher (opens it; press Play there)",
                            "Hopper directly, like CurseForge (Microsoft sign-in, no launcher)",
                        ])
                        .selected_index(Some(usize::from(s.launch_native)))
                        .on_change(cx.listener(|this, ix: &usize, _, cx| {
                            let native = *ix == 1;
                            this.store.update(cx, |s, cx| {
                                s.update_settings(
                                    |st| {
                                        st.launch_native = native;
                                        st.launch_picked = true;
                                    },
                                    cx,
                                )
                            });
                        })),
                )
                .when(s.launch_native && hopper::msauth::client_id().is_none(), |r| {
                    r.child(warning(
                        "Needs MS_CLIENT_ID in .env: an Azure app id approved for the Minecraft API.",
                        cx,
                    ))
                }),
        )
    }

    fn folders(&self, cx: &mut Context<Self>) -> Vec<Block> {
        vec![Block::card(
            None,
            vec![
                Row::new("instances-root")
                    .control(self.folder(Text::Instances, cx))
                    .note(self.note(Text::Instances, cx)),
                Row::new("launcher-dir")
                    .control(self.folder(Text::Launcher, cx))
                    .note(self.note(Text::Launcher, cx)),
            ],
        )]
    }

    fn launchers(&self, cx: &mut Context<Self>) -> Vec<Block> {
        let (managed, found) = self.launchers.read(cx).counts(cx);
        vec![Block::card(
            None,
            vec![Row::new("launchers")
                .control(widgets::pill(
                    if found == 0 {
                        "None found".to_string()
                    } else {
                        format!("{managed} of {found} managed")
                    },
                    if managed > 0 { cx.theme().success } else { cx.theme().muted_foreground },
                    managed > 0,
                ))
                .below(self.launchers.clone())],
        )]
    }

    fn java(&self, s: &hopper::settings::Settings, window: &mut Window, cx: &mut Context<Self>) -> Vec<Block> {
        let rem = window.rem_size();
        let [memory, java, args] = self.java.update(cx, |j, cx| j.parts(rem, cx));
        let get_java = h_flex()
            .gap_2()
            .flex_wrap()
            .children([8u32, 17, 21, 25].into_iter().filter_map(|m| crate::java_form::java_get(m, false, false, false, cx)))
            .children(hopper::javadl::GRAALVM_MAJORS.iter().filter_map(|&m| crate::java_form::java_get(m, true, false, false, cx)));
        vec![
            Block::card(
                None,
                vec![
                    self.launch_row(s, cx),
                    Row::new("minimize").control(self.switch(
                        "minimize",
                        s.minimize_while_playing,
                        |st, v| st.minimize_while_playing = v,
                        cx,
                    )),
                    Row::new("record").control(self.switch(
                        "record",
                        s.record_sessions,
                        |st, v| st.record_sessions = v,
                        cx,
                    )),
                ],
            ),
            Block::card(
                Some("Java defaults"),
                vec![
                    // Below, so the slider fills the row.
                    Row::new("memory").below(memory),
                    Row::new("java").below(java),
                    Row::new("java-args").below(args),
                    Row::new("get-java").below(get_java),
                ],
            ),
        ]
    }

    fn servers(&self, s: &hopper::settings::Settings, window: &mut Window, cx: &mut Context<Self>) -> Vec<Block> {
        let rem = window.rem_size();
        let theme = cx.theme().clone();
        let kind = s.panel_kind.clone();
        let kind_label = hopper::panel::KINDS
            .iter()
            .find(|(k, _)| *k == kind)
            .map_or("Crafty Controller 4", |(_, n)| n)
            .to_string();
        let kind_help = match kind.as_str() {
            "mcss" => "API key: MCSS Web Panel → API keys. Its web API must be on (port 25560 by default).",
            "pterodactyl" => "API key: a client key (ptlc_…) from Account → API Credentials. The console stays in the panel.",
            "hopperd" => "Hopper's own panel (hopperd) on the server PC. Find it on the LAN, then pair: no API key to copy. Sync goes over the network, no shared drive needed.",
            _ => "API key: in Crafty, your user → API Keys, with the Commands permission.",
        };
        let store = self.store.clone();
        // Hopper Servers have their own list above.
        let kinds: Vec<(&str, &str)> = hopper::panel::KINDS.iter().copied().filter(|(k, _)| *k != "hopperd").collect();
        let kinds2 = kinds.clone();
        let picker = widgets::picker(
            "panel-kind",
            kind_label.clone(),
            kinds.iter().map(|(k, n)| (n.to_string(), *k == kind)).collect(),
            rem,
            Rc::new(move |ix, _, cx| {
                if let Some((k, _)) = kinds2.get(ix) {
                    let k = k.to_string();
                    store.update(cx, |s, cx| {
                        s.update_settings(|st| st.panel_kind = k, cx);
                        // A Hopper Server lists the servers; others use the folder.
                        s.rediscover_servers();
                    });
                }
            }),
        );
        let key_typed = !self.panel_token.read(cx).value().trim().is_empty();
        let result: AnyElement = match &self.panel_test {
            _ if self.panel_testing => widgets::caption("Connecting…", cx).into_any_element(),
            Some(Ok(t)) => status_line(IconName::CircleCheck, t.clone(), theme.success),
            Some(Err(e)) => status_line(IconName::CircleX, e.clone(), theme.danger),
            None => widgets::caption("Not tested yet.", cx).text_xs().into_any_element(),
        };
        let url_typed = !self.texts[&Text::PanelUrl].read(cx).value().trim().is_empty();
        let can_save = url_typed && (key_typed || self.panel_key_saved);
        let tools = self.store.read(cx).server_tools();
        let mut tool_rows = vec![Row::new("server-tools").control(self.switch(
            "server-tools",
            tools,
            |st, v| st.server_tools = Some(v),
            cx,
        ))];
        if !tools {
            tool_rows.push(Row::new("hide-servers").control(self.switch(
                "hide-servers",
                s.hide_servers_item,
                |st, v| st.hide_servers_item = v,
                cx,
            )));
        }
        vec![
            Block::card(None, tool_rows),
            Block::card(
                Some("Server packs and hosting"),
                vec![
                    Row::new("servers-root")
                        .control(self.folder(Text::ServerPacks, cx))
                        .note(self.note(Text::ServerPacks, cx)),
                    Row::new("hosted-root")
                        .control(self.folder(Text::Hosted, cx))
                        .note(self.note(Text::Hosted, cx)),
                    Row::new("server-host")
                        .control(div().w(rems(12.)).child(Input::new(&self.texts[&Text::Host]).small()))
                        .note(self.note(Text::Host, cx)),
                    Row::new("auto-update").control(self.switch(
                        "auto-update",
                        s.auto_update_servers,
                        |st, v| st.auto_update_servers = v,
                        cx,
                    )),
                ],
            ),
            Block::card(Some("Hopper Servers"), self.hopperd_rows(s, cx)),
            Block::card(Some("Other server panel"), if self.panel_form {
                let mut rows = vec![
                    Row::new("panel-kind")
                        .desc(kind_help)
                        .control(div().w(rems(15.)).child(picker)),
                    Row::new("panel-url")
                        .control(div().w(rems(20.)).child(Input::new(&self.texts[&Text::PanelUrl]).small()))
                        .note(self.note(Text::PanelUrl, cx)),
                ];
                rows.extend(self.key_rows(s, cx));
                rows.push(
                    Row::new("panel-test").note(Some(result)).control(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("panel-cancel")
                                    .ghost()
                                    .small()
                                    .label(if self.panel_form_new { "Cancel" } else { "Done" })
                                    .on_click(cx.listener(|this, _, window, cx| this.close_panel_form(window, cx))),
                            )
                            .child(
                                Button::new("panel-save")
                                    .small()
                                    .primary()
                                    .label("Save and test")
                                    .loading(self.panel_testing)
                                    .disabled(self.panel_testing || !can_save)
                                    .when(!can_save, |b| b.tooltip("Add the address and an API key first"))
                                    .on_click(cx.listener(|this, _, window, cx| this.save_panel(window, cx))),
                            ),
                    ),
                );
                rows
            } else if self.other_panel_added(s) {
                vec![Row::new("other-panel").below(self.other_panel_row(s, kind_label, cx))]
            } else {
                vec![Row::new("panel-add").control(
                    Button::new("panel-add")
                        .small()
                        .outline()
                        .icon(IconName::Plus)
                        .label("Add a panel")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.panel_form = true;
                            this.panel_form_new = true;
                            this.panel_test = None;
                            cx.notify();
                        })),
                )]
            }),
        ]
    }

    /// The API key rows, for Crafty, MCSS and Pterodactyl.
    fn key_rows(&self, s: &hopper::settings::Settings, cx: &mut Context<Self>) -> Vec<Row> {
        let theme = cx.theme().clone();
        vec![
                    Row::new("panel-key")
                        .control(div().w(rems(17.)).child(Input::new(&self.panel_token).small().mask_toggle()))
                        .note(Some(if self.panel_key_saved {
                            widgets::pill("Saved", theme.success, false).into_any_element()
                        } else {
                            widgets::pill("Missing", theme.warning, false).into_any_element()
                        })),
                    Row::new("panel-cert").control(self.switch(
                        "panel-self-signed",
                        s.panel_self_signed,
                        |st, v| st.panel_self_signed = v,
                        cx,
                    )),
        ]
    }

    /// A panel counts as added once it has an address or a saved key, so
    /// a key left behind by an older build can still be removed.
    fn other_panel_added(&self, s: &hopper::settings::Settings) -> bool {
        !s.panel_url.trim().is_empty() || self.panel_key_saved
    }

    /// The added Crafty / MCSS / Pterodactyl panel: Test, Edit, Remove.
    fn other_panel_row(&self, s: &hopper::settings::Settings, kind_label: String, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let url = s.panel_url.trim().to_string();
        let configured = !url.is_empty() && self.panel_key_saved;
        let state = match &self.panel_test {
            _ if self.panel_testing => widgets::pill("Connecting", theme.muted_foreground, false),
            Some(Ok(_)) => widgets::pill("Connected", theme.success, false),
            Some(Err(_)) => widgets::pill("Can't connect", theme.danger, false),
            None if url.is_empty() => widgets::pill("No address", theme.warning, false),
            None if !self.panel_key_saved => widgets::pill("No API key", theme.warning, false),
            None => widgets::pill("Not tested", theme.muted_foreground, false),
        };
        let detail: Option<AnyElement> = match &self.panel_test {
            Some(Ok(t)) if !self.panel_testing => Some(status_line(IconName::CircleCheck, t.clone(), theme.success)),
            Some(Err(e)) if !self.panel_testing => Some(status_line(IconName::CircleX, e.clone(), theme.danger)),
            _ => None,
        };
        let asking = self.panel_removing;
        let open = url.clone();
        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .py_1()
                    .child(Icon::new(IconName::Server).small().text_color(theme.muted_foreground))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().font_medium().truncate().child(kind_label))
                            .child(
                                widgets::caption(if url.is_empty() { "Only an API key is saved".to_string() } else { url.clone() }, cx)
                                    .text_xs()
                                    .truncate(),
                            ),
                    )
                    .child(state)
                    .when(!url.is_empty(), |r| {
                        r.child(
                            Button::new("other-panel-open")
                                .ghost()
                                .small()
                                .icon(IconName::ExternalLink)
                                .tooltip("Open its web page")
                                .on_click(move |_, _, cx| cx.open_url(&open)),
                        )
                    })
                    .child(
                        Button::new("other-panel-test")
                            .outline()
                            .small()
                            .label("Test")
                            .loading(self.panel_testing)
                            .disabled(self.panel_testing || !configured)
                            .when(!configured, |b| b.tooltip("Add the address and an API key first (Edit)"))
                            .on_click(cx.listener(|this, _, _, cx| this.test_panel(cx))),
                    )
                    .child(
                        Button::new("other-panel-edit")
                            .outline()
                            .small()
                            .label("Edit")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.panel_form = true;
                                this.panel_form_new = false;
                                this.panel_removing = false;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("other-panel-remove")
                            .ghost()
                            .small()
                            .when(asking, |b| b.danger())
                            .label(if asking { "Remove?" } else { "Remove" })
                            .tooltip("Forget this panel and delete its API key here. Its servers keep running.")
                            .on_click(cx.listener(|this, _, window, cx| this.remove_other_panel(window, cx))),
                    ),
            )
            .children(detail)
            .into_any_element()
    }

    /// Closes the add / edit form. Cancelling an add forgets the address
    /// typed into it; a key is only saved by Save and test.
    fn close_panel_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.panel_form_new {
            self.store.update(cx, |s, cx| s.update_settings(|st| st.panel_url.clear(), cx));
            self.texts[&Text::PanelUrl].update(cx, |t, cx| t.set_value(String::new(), window, cx));
            self.panel_token.update(cx, |t, cx| t.set_value(String::new(), window, cx));
            self.notes.remove(&Text::PanelUrl);
            self.panel_test = None;
        }
        self.panel_form = false;
        self.panel_form_new = false;
        cx.notify();
    }

    /// Forgets the other panel (asks once more first): its address here and
    /// its API key in Credential Manager. The panel itself is untouched.
    fn remove_other_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.panel_removing {
            self.panel_removing = true;
            cx.notify();
            return;
        }
        self.panel_removing = false;
        if let Err(e) = hopper::panel::set_token("") {
            window.push_notification(Notification::error(format!("Couldn't delete the API key: {e:#}")), cx);
            return;
        }
        self.panel_key_saved = hopper::panel::token().is_some();
        self.panel_test = None;
        self.store.update(cx, |s, cx| {
            s.update_settings(|st| st.panel_url.clear(), cx);
            s.rediscover_servers();
        });
        self.texts[&Text::PanelUrl].update(cx, |t, cx| t.set_value(String::new(), window, cx));
        self.panel_token.update(cx, |t, cx| {
            t.set_value(String::new(), window, cx);
            t.set_placeholder("Paste the panel's API key", window, cx);
        });
        self.notes.remove(&Text::PanelUrl);
        window.push_notification(Notification::info("Removed the panel and its API key from this PC."), cx);
        cx.notify();
    }

    /// The paired Hopper Servers, and finding and pairing another.
    fn hopperd_rows(&self, s: &hopper::settings::Settings, cx: &mut Context<Self>) -> Vec<Row> {
        let theme = cx.theme().clone();
        let busy = matches!(self.pairing, Pairing::Asking | Pairing::Waiting(..));
        let paired: AnyElement = if s.hopper_panels.is_empty() {
            widgets::caption("None yet. Find one on the LAN or type its address below.", cx).text_xs().into_any_element()
        } else {
            v_flex()
                .gap_1()
                .children(s.hopper_panels.iter().enumerate().map(|(i, p)| {
                    let has_token = hopper::panel::hopperd_token_for(&p.id).is_some();
                    let store = self.store.read(cx);
                    let offline = store.panel_error(&p.id).is_some();
                    let state = match store.panel_error(&p.id) {
                        Some(e) => widgets::pill(e.to_string(), theme.danger, false),
                        None if !has_token => widgets::pill("Not paired", theme.warning, false),
                        None if store.panel_listed(&p.id) => widgets::pill(
                            format!(
                                "{} servers",
                                store.servers().iter().filter(|x| x.remote.as_ref().is_some_and(|r| r.panel == p.id)).count()
                            ),
                            theme.success,
                            false,
                        ),
                        None => widgets::pill("Connecting", theme.muted_foreground, false),
                    };
                    let (id, id2, url) = (p.id.clone(), p.id.clone(), p.url.clone());
                    let asking = self.removing.as_deref() == Some(p.id.as_str());
                    h_flex()
                        .gap_2()
                        .py_1()
                        .child(Icon::new(IconName::Server).small().text_color(theme.muted_foreground))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_sm().font_medium().truncate().child(p.name.clone()))
                                .child(widgets::caption(p.url.clone(), cx).text_xs().truncate()),
                        )
                        .child(state)
                        .child(
                            Button::new(("panel-open", i))
                                .ghost()
                                .small()
                                .icon(IconName::ExternalLink)
                                .tooltip("Open its web page")
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                        )
                        .child(
                            Button::new(("panel-repair", i))
                                .outline()
                                .small()
                                .label("Pair again")
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.repair = Some(id.clone());
                                    this.pair_panel(cx)
                                })),
                        )
                        .child(
                            Button::new(("panel-remove", i))
                                .ghost()
                                .small()
                                .when(asking, |b| b.danger())
                                .label(if asking { "Remove?" } else { "Remove" })
                                .tooltip(if offline {
                                    "It can't be reached, so it will still list this PC: remove it there under Access & devices. Its servers keep running."
                                } else {
                                    "Unpair this PC in the panel and forget it here. Its servers keep running."
                                })
                                .on_click(cx.listener(move |this, _, window, cx| this.remove_panel(id2.clone(), window, cx))),
                        )
                        .into_any_element()
                }))
                .into_any_element()
        };
        let found: AnyElement = match &self.found {
            _ if self.finding => widgets::caption("Looking on the LAN…", cx).text_xs().into_any_element(),
            None => widgets::caption("Not looked yet.", cx).text_xs().into_any_element(),
            Some(list) if list.is_empty() => widgets::caption(
                "None found. Is hopperd running on the server PC? Type its address below instead.",
                cx,
            )
            .text_xs()
            .into_any_element(),
            Some(list) => v_flex()
                .gap_1()
                .children(list.iter().enumerate().map(|(i, f)| {
                    let (url, name) = (f.url.clone(), f.name.clone());
                    let known = s.hopper_panels.iter().any(|p| hopper::hopperd::base_url(&p.url) == hopper::hopperd::base_url(&f.url));
                    Button::new(("found-panel", i))
                        .small()
                        .outline()
                        .icon(IconName::Server)
                        .label(format!("{} · {} · v{}{}", f.name, f.url, f.version, if known { " · paired" } else { "" }))
                        .disabled(known)
                        .on_click(cx.listener(move |this, _, window, cx| this.use_panel_url(url.clone(), name.clone(), window, cx)))
                }))
                .into_any_element(),
        };
        let has_url = !self.new_panel.read(cx).value().trim().is_empty();
        let state: AnyElement = match &self.pairing {
            Pairing::Idle => div().into_any_element(),
            Pairing::Asking => widgets::caption("Asking the panel…", cx).text_xs().into_any_element(),
            Pairing::Waiting(code, fp) => v_flex()
                .gap_1()
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().text_lg().font_weight(FontWeight::BOLD).font_family("JetBrains Mono").child(code.clone()))
                        .child(widgets::caption("Allow it in the panel: Access & devices. Check it shows this code.", cx).text_xs()),
                )
                .child(widgets::caption(format!("Certificate {fp}…"), cx).text_xs())
                .into_any_element(),
            Pairing::Done(t) => status_line(IconName::CircleCheck, t.clone(), theme.success),
            Pairing::Failed(e) => status_line(IconName::CircleX, e.clone(), theme.danger),
        };
        vec![
            Row::new("hopper-panels").below(paired),
            Row::new("panel-find")
                .control(
                    Button::new("panel-find")
                        .small()
                        .outline()
                        .icon(IconName::Search)
                        .label("Find")
                        .loading(self.finding)
                        .disabled(self.finding)
                        .on_click(cx.listener(|this, _, _, cx| this.find_panels(cx))),
                )
                .below(found),
            Row::new("panel-pair")
                .control(
                    h_flex()
                        .gap_2()
                        .child(div().w(rems(17.)).child(Input::new(&self.new_panel).small()))
                        .child(
                            Button::new("panel-pair")
                                .small()
                                .primary()
                                .label("Pair")
                                .disabled(!has_url || busy)
                                .when(!has_url, |b| b.tooltip("Find the panel or type its address first"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.repair = None;
                                    this.pair_panel(cx)
                                })),
                        ),
                )
                .below(state),
        ]
    }

    fn use_panel_url(&mut self, url: String, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.new_panel.update(cx, |t, cx| t.set_value(url.clone(), window, cx));
        self.new_panel_name = Some((url, name));
        cx.notify();
    }

    /// Unpairs from a panel (asks once more first): the panel forgets this
    /// PC when it can be reached, and this PC forgets the panel and its token.
    fn remove_panel(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.removing.as_deref() != Some(id.as_str()) {
            self.removing = Some(id);
            cx.notify();
            return;
        }
        self.removing = None;
        self.pairing = Pairing::Idle;
        let panel = self.store.read(cx).settings().hopper_panels.iter().find(|p| p.id == id).cloned();
        // Made before the token goes, so it can still sign in to unpair.
        let client = panel.as_ref().and_then(hopper::hopperd::Client::for_panel);
        if let Err(e) = hopper::panel::set_hopperd_token_for(&id, "") {
            log::warn!("forgetting the panel's token: {e:#}");
        }
        self.store.update(cx, |s, cx| {
            s.update_settings(|st| st.hopper_panels.retain(|p| p.id != id), cx);
            s.rediscover_servers();
        });
        cx.notify();
        let (Some(panel), Some(client)) = (panel, client) else { return };
        cx.spawn_in(window, async move |_, cx| {
            let r = cx.background_spawn(async move { client.unpair() }).await;
            cx.update(|window, cx| {
                let note = match r {
                    Ok(()) => Notification::success(format!("Unpaired from {}.", panel.name)),
                    Err(e) => {
                        log::warn!("unpairing from {}: {e:#}", panel.url);
                        Notification::warning(format!(
                            "Removed here, but {} couldn't be told, so it still lists this PC. Remove it there under Access & devices.",
                            panel.name
                        ))
                    }
                };
                window.push_notification(note, cx);
            })
            .ok();
        })
        .detach();
    }

    fn find_panels(&mut self, cx: &mut Context<Self>) {
        self.finding = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_spawn(async move { hopper::hopperd::discover(std::time::Duration::from_secs(3)) })
                .await;
            this.update(cx, |this, cx| {
                this.finding = false;
                this.found = Some(found);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Asks the panel to pair, then waits (up to five minutes) for the
    /// owner to allow it there.
    fn pair_panel(&mut self, cx: &mut Context<Self>) {
        let url = match &self.repair {
            Some(id) => self.store.read(cx).settings().hopper_panels.iter().find(|p| &p.id == id).map(|p| p.url.clone()),
            None => Some(self.new_panel.read(cx).value().trim().to_string()),
        }
        .unwrap_or_default();
        // An address already paired is paired again, not added twice.
        let repair = self.repair.clone().or_else(|| {
            let base = hopper::hopperd::base_url(&url);
            self.store.read(cx).settings().hopper_panels.iter().find(|p| hopper::hopperd::base_url(&p.url) == base).map(|p| p.id.clone())
        });
        let mut announced = self.new_panel_name.clone().filter(|(u, _)| *u == url).map(|(_, n)| n);
        // Each new panel gets its own token name (only one paired before
        // several panels were possible keeps the old one, id "").
        let id = repair.clone().unwrap_or_else(|| {
            let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
            format!("p{n:x}")
        });
        let id2 = id.clone();
        self.pairing = Pairing::Asking;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let req = cx
                .background_spawn(async move { hopper::hopperd::pair_request(&url, &hopper::hopperd::device_name()) })
                .await;
            let req = match req {
                Ok(r) => r,
                Err(e) => {
                    this.update(cx, |this, cx| {
                        this.pairing = Pairing::Failed(format!("{e:#}"));
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let code = req.code.clone();
            let fp: String = req.fingerprint.chars().take(23).collect();
            if this
                .update(cx, |this, cx| {
                    this.pairing = Pairing::Waiting(code, fp);
                    cx.notify();
                })
                .is_err()
            {
                return;
            }
            let end = std::time::Instant::now() + std::time::Duration::from_secs(300);
            let outcome = loop {
                cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                let r = req.clone();
                let st = cx.background_spawn(async move { hopper::hopperd::pair_status(&r) }).await;
                match st {
                    Ok(hopper::hopperd::PairState::Waiting) if std::time::Instant::now() < end => continue,
                    Ok(hopper::hopperd::PairState::Approved(token)) => {
                        // The name the panel gives itself ("rig").
                        if announced.is_none() {
                            let base = req.base.clone();
                            announced = cx
                                .background_spawn(async move { hopper::hopperd::hello(&base).ok().map(|h| h.name) })
                                .await
                                .filter(|n| !n.trim().is_empty());
                        }
                        break match hopper::panel::set_hopperd_token_for(&id2, &token) {
                            Ok(()) => Ok("Paired over an encrypted connection. Hopper now syncs with this panel over the network.".to_string()),
                            Err(e) => Err(format!("Paired, but the token couldn't be saved: {e:#}")),
                        };
                    }
                    Ok(hopper::hopperd::PairState::Denied) => break Err("The panel refused this PC.".into()),
                    Ok(_) => break Err("The request expired. Pair again and allow it in the panel within 5 minutes.".into()),
                    Err(e) => break Err(format!("{e:#}")),
                }
            };
            this.update(cx, |this, cx| {
                match outcome {
                    Ok(t) => {
                        this.pairing = Pairing::Done(t);
                        this.repair = None;
                        this.new_panel_name = None;
                        // Keep the pinned certificate and the HTTPS address.
                        let (ca, base) = (req.ca.clone(), req.base.clone());
                        let name = announced.clone().unwrap_or_else(|| hopper::hopperd::host_name(&base));
                        this.store.update(cx, |s, cx| {
                            s.update_settings(
                                |st| match st.hopper_panels.iter_mut().find(|p| p.id == id) {
                                    Some(p) => {
                                        p.ca = ca;
                                        p.url = base;
                                    }
                                    None => {
                                        // Two panels on one PC both call themselves by its name.
                                        let name = if st.hopper_panels.iter().any(|p| p.name == name) {
                                            let port = base.rsplit_once(':').map(|(_, p)| p.trim_end_matches('/')).unwrap_or("");
                                            format!("{name} ({port})")
                                        } else {
                                            name
                                        };
                                        st.hopper_panels.push(hopper::settings::HopperPanel { id: id.clone(), name, url: base, ca })
                                    }
                                },
                                cx,
                            );
                            s.rediscover_servers();
                        });
                    }
                    Err(e) => this.pairing = Pairing::Failed(e),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn content(&self, s: &hopper::settings::Settings, cx: &mut Context<Self>) -> Vec<Block> {
        use hopper::cf::KeySource;
        let theme = cx.theme().clone();
        let source = hopper::cf::api_key_source().map(|(_, s)| s);
        let (pill, desc) = match source {
            Some(KeySource::Saved) => (
                widgets::pill("Your key", theme.success, true),
                if hopper::cf::env_key_ignored() {
                    "Using your saved key. CF_API_KEY in .env holds a different key, which Hopper ignores while one is saved."
                } else {
                    "Using your saved key, kept in Windows Credential Manager. Paste another to replace it."
                },
            ),
            Some(KeySource::Env) => (
                widgets::pill("From .env", theme.info, true),
                "Using CF_API_KEY from .env. A key you save here takes its place.",
            ),
            Some(KeySource::Session) => (
                widgets::pill("This run", theme.info, true),
                "Using a key given for this run only.",
            ),
            Some(KeySource::BuiltIn) => (
                widgets::pill("Built in", theme.muted_foreground, true),
                "Using Hopper's built-in key. Paste your own to use it instead; it keeps working if Hopper's key is ever revoked.",
            ),
            None => (
                widgets::pill("Missing", theme.danger, true),
                "Needed for CurseForge: browsing, installs and updates. Get one at console.curseforge.com.",
            ),
        };
        let typed = !self.cf_key.read(cx).value().trim().is_empty();
        let result: Option<AnyElement> = match &self.cf_test {
            _ if self.cf_testing => Some(widgets::caption("Asking CurseForge…", cx).text_xs().into_any_element()),
            Some(Ok(t)) => Some(status_line(IconName::CircleCheck, t.clone(), theme.success)),
            Some(Err(e)) => Some(status_line(IconName::CircleX, e.clone(), theme.danger)),
            None => None,
        };
        let key = Row::new("cf-key")
            .desc(desc)
            .control(pill)
            .note(result)
            .below(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(div().w(rems(22.)).child(Input::new(&self.cf_key).small().mask_toggle()))
                    .child(
                        Button::new("cf-key-save")
                            .small()
                            .primary()
                            .label("Save")
                            .disabled(!typed)
                            .on_click(cx.listener(|this, _, window, cx| this.save_cf_key(window, cx))),
                    )
                    .child(
                        Button::new("cf-key-test")
                            .small()
                            .outline()
                            .icon(IconName::Activity)
                            .label("Test")
                            .tooltip(if typed { "Tests the key you pasted, without saving it" } else { "Tests the key in use" })
                            .loading(self.cf_testing)
                            .disabled(self.cf_testing || (!typed && source.is_none()))
                            .on_click(cx.listener(|this, _, _, cx| this.test_cf_key(cx))),
                    )
                    .when(source == Some(KeySource::Saved), |r| {
                        r.child(
                            Button::new("cf-key-remove")
                                .small()
                                .ghost()
                                .label("Remove")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    if let Err(e) = hopper::cf::set_api_key("") {
                                        window.push_notification(Notification::error(format!("{e:#}")), cx);
                                    }
                                    this.cf_key.update(cx, |s, cx| {
                                        s.set_placeholder("Paste your CurseForge API key", window, cx)
                                    });
                                    this.cf_test = None;
                                    cx.notify();
                                })),
                        )
                    }),
            );
        let browse = Row::new("browse-source").control(
            TabBar::new("browse-source")
                .segmented()
                .small()
                .selected_index(usize::from(s.browse_modrinth))
                .child(Tab::new().label("CurseForge"))
                .child(Tab::new().label("Modrinth"))
                .on_click(cx.listener(|_, ix: &usize, _, cx| {
                    cx.emit(SettingsEvent::BrowseSource(*ix == 1))
                })),
        );
        vec![Block::card(None, vec![key, browse])]
    }

    fn appearance(&self, s: &hopper::settings::Settings, window: &mut Window, cx: &mut Context<Self>) -> Vec<Block> {
        let rem = window.rem_size();
        let theme = h_flex()
            .gap_1()
            .child(div().w(rems(15.)).child(self.theme_picker(s, rem)))
            .child(
                Button::new("theme-folder")
                    .ghost()
                    .small()
                    .icon(IconName::FolderOpen)
                    .tooltip("Your own themes: gpui-component theme .json files go here")
                    .on_click(|_, _, _| {
                        if let Some(d) = crate::theme::custom_dir() {
                            let _ = std::fs::create_dir_all(&d);
                            crate::store::open_path(&d);
                        }
                    }),
            )
            .child(
                Button::new("theme-reload")
                    .ghost()
                    .small()
                    .icon(IconName::RefreshCw)
                    .tooltip("Read the theme folder again")
                    .on_click(cx.listener(|_, _, window, cx| {
                        let n = crate::theme::reload_custom();
                        window.push_notification(Notification::info(format!("{n} themes of your own.")), cx);
                        cx.notify();
                    })),
            );
        let zoom = s.zoom;
        let zooms = crate::app::AppView::ZOOMS;
        let store = self.store.clone();
        let zoom_picker = widgets::picker(
            "zoom",
            format!("{zoom}%"),
            zooms
                .iter()
                .map(|z| (if *z == 100 { "100% (default)".to_string() } else { format!("{z}%") }, *z == zoom))
                .collect(),
            rem,
            Rc::new(move |ix, _, cx| {
                if let Some(z) = zooms.get(ix).copied() {
                    crate::theme::set_zoom(z, cx);
                    store.update(cx, |s, cx| s.update_settings(|st| st.zoom = z, cx));
                }
            }),
        );
        vec![Block::card(
            None,
            vec![
                Row::new("theme").control(theme),
                Row::new("zoom").control(div().w(rems(9.)).child(zoom_picker)),
            ],
        )]
    }

    fn updates(&self, cx: &mut Context<Self>) -> Vec<Block> {
        use crate::store::SelfUpdate;
        let theme = cx.theme().clone();
        let state = self.store.read(cx).self_update().cloned();
        let check = Button::new("self-check")
            .small()
            .outline()
            .icon(IconName::RefreshCw)
            .label("Check now")
            .disabled(matches!(state, Some(SelfUpdate::Checking | SelfUpdate::Installing(_))))
            .on_click(cx.listener(|this, _, _, cx| {
                this.commit(Text::UpdateRepo, cx);
                this.store.update(cx, |s, cx| {
                    s.check_self_update();
                    cx.notify();
                });
            }));
        let (pill, line, action): (Div, Option<SharedString>, Option<AnyElement>) = match state {
            None => (widgets::pill("Not checked", theme.muted_foreground, false), None, Some(check.into_any_element())),
            Some(SelfUpdate::Checking) => (widgets::pill("Checking…", theme.info, true), None, None),
            Some(SelfUpdate::UpToDate) => (
                widgets::pill("Up to date", theme.success, true),
                Some("This is the latest release.".into()),
                Some(check.into_any_element()),
            ),
            Some(SelfUpdate::Available(r)) => (
                widgets::pill(format!("{} is out", r.version), theme.primary, true),
                None,
                Some({
                    let store = self.store.clone();
                    widgets::split(
                        widgets::split_lead(
                            Button::new("self-update")
                                .small()
                                .primary()
                                .icon(IconName::Download)
                                .label(format!("Update to {}", r.version))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.store.update(cx, |s, cx| {
                                        s.install_self_update();
                                        cx.notify();
                                    })
                                })),
                        ),
                        "self-update-notes",
                        widgets::Tint::Primary,
                        gpui_kit::component::Size::Small,
                        format!("What's new in {}", r.version),
                        move |_, window, cx| crate::hopper_versions::open(store.clone(), window, cx),
                        cx,
                    )
                    .into_any_element()
                }),
            ),
            Some(SelfUpdate::Installing(v)) => (widgets::pill(format!("Installing {v}…"), theme.info, true), None, None),
            Some(SelfUpdate::Ready(v)) => (
                widgets::pill(format!("{v} installed"), theme.success, true),
                Some("It runs from the next start.".into()),
                Some(
                    Button::new("self-restart")
                        .small()
                        .primary()
                        .label("Restart now")
                        .on_click(|_, _, cx| restart(cx))
                        .into_any_element(),
                ),
            ),
            Some(SelfUpdate::Failed(e)) => (
                widgets::pill("Check failed", theme.danger, true),
                Some(e.into()),
                Some(check.into_any_element()),
            ),
        };
        let whats_new = Button::new("whats-new")
            .small()
            .ghost()
            .icon(IconName::Info)
            .label("What's new")
            .on_click(cx.listener(|this, _, window, cx| {
                let repo = this.store.read(cx).settings().update_repo.clone();
                crate::whats_new::open_recent(repo, window, cx);
            }));
        let version = h_flex()
            .gap_3()
            .child(widgets::stat_tile(hopper::selfupdate::CURRENT, "Version", cx))
            .child(
                v_flex()
                    .gap_1()
                    .child(pill)
                    .when_some(line, |c, l| c.child(widgets::caption(l, cx).text_xs())),
            )
            .child(whats_new)
            .child(
                Button::new("hopper-versions")
                    .small()
                    .ghost()
                    .icon(IconName::RotateCcw)
                    .label("Other versions")
                    .tooltip("Pick any release, including older ones to roll back to")
                    .on_click(cx.listener(|this, _, window, cx| {
                        crate::hopper_versions::open(this.store.clone(), window, cx)
                    })),
            );

        // Install: here, or where it is.
        let install = if hopper::setup::running_installed() {
            let dir = hopper::setup::install_dir()
                .map(|d| d.display().to_string())
                .unwrap_or_default();
            Row::new("install")
                .desc(format!("Installed in {dir}. Remove it from Windows Settings → Apps."))
                .control(widgets::pill("Installed", theme.success, true))
        } else {
            let other = hopper::setup::installed_exe();
            let busy = self.installing;
            Row::new("install")
                .when(other.is_some(), |r| {
                    r.desc("A copy is already installed. Installing replaces it with this one.")
                })
                .control(
                    h_flex()
                        .gap_2()
                        .when_some(other, |r, exe| {
                            r.child(
                                Button::new("open-installed")
                                    .small()
                                    .ghost()
                                    .label("Open the installed Hopper")
                                    .on_click(move |_, _, cx| {
                                        if std::process::Command::new(&exe).spawn().is_ok() {
                                            cx.quit();
                                        }
                                    }),
                            )
                        })
                        .child(
                            Button::new("install-hopper")
                                .small()
                                .outline()
                                .icon(IconName::Download)
                                .label(if hopper::setup::installed_exe().is_some() {
                                    "Install this version"
                                } else {
                                    "Install Hopper"
                                })
                                .loading(busy)
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, window, cx| this.install(window, cx))),
                        ),
                )
        };
        // Start menu shortcut for a copy that isn't installed.
        let start_menu = (!hopper::setup::running_installed()).then(|| {
            let added = hopper::setup::in_start_menu();
            Row::new("start-menu").control(
                Button::new("start-menu")
                    .small()
                    .outline()
                    .icon(if added { IconName::Close } else { IconName::Plus })
                    .label(if added { "Remove from Start menu" } else { "Add to Start menu" })
                    .on_click(cx.listener(move |_, _, window, cx| {
                        let (r, ok) = if added {
                            (hopper::setup::remove_from_start_menu(), "Removed Hopper from the Start menu.")
                        } else {
                            (
                                hopper::setup::add_to_start_menu(),
                                "Hopper is in the Start menu. To pin it, right-click it there and pick Pin to Start.",
                            )
                        };
                        let n = match r {
                            Ok(()) => Notification::success(ok),
                            Err(e) => Notification::error(format!("{e:#}")),
                        };
                        window.push_notification(n, cx);
                        cx.notify();
                    })),
            )
        });
        vec![
            Block::card(
                None,
                vec![
                    Row::new("self-update").control_any(action).below(version),
                    Row::new("update-repo")
                        .control(div().w(rems(16.)).child(Input::new(&self.texts[&Text::UpdateRepo]).small()))
                        .note(self.note(Text::UpdateRepo, cx)),
                ],
            ),
            Block::card(
                None,
                std::iter::once(install)
                    .chain(start_menu)
                    .chain([Row::new("setup").control(
                        Button::new("run-setup")
                            .small()
                            .outline()
                            .icon(IconName::Sparkles)
                            .label("Run setup again")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::RunSetup))),
                    )])
                    .collect(),
            ),
        ]
    }

    fn advanced(&self, s: &hopper::settings::Settings, rem: Pixels, cx: &mut Context<Self>) -> Vec<Block> {
        let links = Switch::new("links")
            .checked(s.links_enabled)
            .on_click(cx.listener(|this, v: &bool, window, cx| {
                let on = *v;
                let prev = this.store.read(cx).settings().links_prev_handler.clone();
                let r = if on {
                    hopper::links::take_over().map(Some)
                } else {
                    hopper::links::give_back(prev.as_deref()).map(|()| None)
                };
                match r {
                    Ok(prev) => this.store.update(cx, |s, cx| {
                        s.update_settings(
                            |st| {
                                if let Some(p) = prev {
                                    st.links_prev_handler = p;
                                }
                                st.links_enabled = on;
                            },
                            cx,
                        )
                    }),
                    Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
                }
            }));
        let data = hopper::paths::dirs().map(|d| d.root().unwrap_or(d.data_dir()).to_path_buf());
        // "Next to hopper.exe" while the files are somewhere else.
        let beside = hopper::paths::beside_exe()
            .filter(|b| hopper::paths::dirs().is_some_and(|d| d.root() != Some(b.as_path())))
            .filter(|_| !hopper::paths::dev_build());
        let (store_mv, store_pick) = (self.store.clone(), self.store.clone());
        let port = crate::mcp::port_of(s);
        let url = format!("http://127.0.0.1:{port}/mcp");
        let status: Option<AnyElement> = s.mcp_enabled.then(|| {
            let theme = cx.theme();
            match crate::mcp::status() {
                Some(Ok(p)) => status_line(IconName::CircleCheck, format!("Running on http://127.0.0.1:{p}/mcp"), theme.success),
                Some(Err(e)) => status_line(IconName::CircleX, format!("Couldn't start: {e}"), theme.danger),
                None => status_line(IconName::Clock, "Starting…".to_string(), theme.muted_foreground),
            }
        });
        let json_cfg = format!(
            "{{\n  \"mcpServers\": {{\n    \"hopper\": {{ \"type\": \"http\", \"url\": \"{url}\" }}\n  }}\n}}"
        );
        let claude_code = format!("claude mcp add --transport http hopper {url}");
        let mut mcp_rows = vec![Row::new("mcp")
            .control(self.switch("mcp", s.mcp_enabled, |st, v| st.mcp_enabled = v, cx))
            .note(status)];
        if s.mcp_enabled {
            mcp_rows.push(
                Row::new("mcp-port")
                    .control(div().w(rems(8.)).child(Input::new(&self.texts[&Text::McpPort]).small()))
                    .note(self.note(Text::McpPort, cx)),
            );
            mcp_rows.push(Row::new("mcp-changes").control(self.switch(
                "mcp-changes",
                s.mcp_changes,
                |st, v| st.mcp_changes = v,
                cx,
            )));
            mcp_rows.push(Row::new("mcp-window").control(self.switch(
                "mcp-window",
                s.mcp_window,
                |st, v| st.mcp_window = v,
                cx,
            )));
            mcp_rows.push(
                Row::new("mcp-connect").control(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("mcp-copy-json")
                                .small()
                                .outline()
                                .icon(IconName::Copy)
                                .label("Copy JSON")
                                .tooltip("For Claude Desktop's claude_desktop_config.json, Cursor, and most MCP clients")
                                .on_click(move |_, window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(json_cfg.clone()));
                                    window.push_notification(Notification::success("Copied the MCP settings"), cx);
                                }),
                        )
                        .child(
                            Button::new("mcp-copy-claude")
                                .small()
                                .outline()
                                .icon(IconName::SquareTerminal)
                                .label("Copy Claude Code command")
                                .tooltip(claude_code.clone())
                                .on_click(move |_, window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(claude_code.clone()));
                                    window.push_notification(Notification::success("Copied: run it in a terminal"), cx);
                                }),
                        ),
                ),
            );
        }
        vec![Block::card(Some("AI assistants"), mcp_rows), Block::card(
            None,
            vec![
                Row::new("tray-icon").control(self.switch("tray-icon", s.tray_icon, |st, v| st.tray_icon = v, cx)),
                Row::new("links").control(links),
                Row::new("data-folder")
                    .when_some(data.clone(), |r, d| r.desc(d.display().to_string()))
                    .control(
                      h_flex().gap_2().when_some(beside, |r, b| r.child(
                        Button::new("data-move")
                            .small()
                            .outline()
                            .label("Move next to hopper.exe")
                            .tooltip(b.display().to_string())
                            .on_click(move |_, window, cx| crate::data_move::confirm(store_mv.clone(), b.clone(), window, cx)),
                      )).child(
                        Button::new("data-move-to")
                            .small()
                            .outline()
                            .icon(IconName::FolderOpen)
                            .label("Move to…")
                            .on_click(move |_, window, cx| crate::data_move::pick(store_pick.clone(), window, cx)),
                      ).child(
                        Button::new("data-folder")
                            .small()
                            .outline()
                            .icon(IconName::FolderOpen)
                            .label("Open")
                            .disabled(data.is_none())
                            .on_click(move |_, _, _| {
                                if let Some(d) = &data {
                                    crate::store::open_path(d);
                                }
                            }),
                      )),
                self.pack_cache_row(s, rem, cx),
            ],
        )]
    }

    /// Downloaded packs: what they take, how long they stay, and Clear.
    fn pack_cache_row(&self, s: &hopper::settings::Settings, rem: Pixels, cx: &mut Context<Self>) -> Row {
        const DAYS: [(u32, &str); 4] = [(7, "Keep 1 week"), (30, "Keep 30 days"), (90, "Keep 90 days"), (0, "Keep forever")];
        let (n, bytes) = hopper::modpack::pack_cache_stats();
        let used = match n {
            0 => "None kept now.".to_string(),
            1 => format!("1 pack, {}.", hopper::instance_ops::human_bytes(bytes)),
            n => format!("{n} packs, {}.", hopper::instance_ops::human_bytes(bytes)),
        };
        let label = DAYS
            .iter()
            .find(|(d, _)| *d == s.pack_keep_days)
            .map_or_else(|| format!("Keep {} days", s.pack_keep_days), |(_, l)| l.to_string());
        let store = self.store.clone();
        let picker = widgets::picker(
            "pack-cache",
            label,
            DAYS.iter().map(|(d, l)| (l.to_string(), *d == s.pack_keep_days)).collect(),
            rem,
            Rc::new(move |ix, _, cx| {
                let days = DAYS[ix.min(DAYS.len() - 1)].0;
                store.update(cx, |s, cx| s.update_settings(|st| st.pack_keep_days = days, cx));
                // A shorter time applies now, not at the next start.
                std::thread::spawn(move || hopper::modpack::prune_packs_older_than(days));
            }),
        );
        Row::new("pack-cache")
            .desc(format!("{} {used}", entry("pack-cache").desc))
            .control(
                h_flex()
                    .gap_2()
                    .child(div().w(rems(10.)).child(picker))
                    .child(
                        Button::new("pack-cache-clear")
                            .small()
                            .outline()
                            .label("Clear")
                            .disabled(n == 0)
                            .tooltip("Removes every downloaded pack now. The next update downloads its pack again.")
                            .on_click(cx.listener(|_, _, window, cx| {
                                let n = match hopper::modpack::prune_pack_cache(0, None) {
                                    Ok(n) => Notification::success(format!(
                                        "Freed {}",
                                        hopper::instance_ops::human_bytes(n)
                                    )),
                                    Err(e) => Notification::error(format!("{e:#}")),
                                };
                                window.push_notification(n, cx);
                                cx.notify();
                            })),
                    ),
            )
    }

    /// The theme menu: each with a strip of its colors.
    fn theme_picker(&self, s: &hopper::settings::Settings, rem: Pixels) -> impl IntoElement + use<> {
        use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
        let current = if s.theme_name.is_empty() && s.theme == 1 {
            crate::theme::HOPPER_DARK.to_string()
        } else if s.theme_name.is_empty() && s.theme == 2 {
            crate::theme::HOPPER_LIGHT.to_string()
        } else {
            s.theme_name.clone()
        };
        let label = if current.is_empty() { "Follow Windows".to_string() } else { current.clone() };
        let store = self.store.clone();
        Button::new("theme")
            .outline()
            .small()
            .w_full()
            .label(label)
            .dropdown_caret(true)
            .dropdown_menu(move |menu: PopupMenu, _, _| {
                let mut menu = menu
                    .min_w(rems(17.).to_pixels(rem))
                    .max_h(rems(28.).to_pixels(rem))
                    .scrollable(true);
                for name in std::iter::once(String::new()).chain(crate::theme::names()) {
                    let checked = name == current;
                    let (n2, store) = (name.clone(), store.clone());
                    menu = menu.item(
                        PopupMenuItem::element(move |_, cx| theme_row(&name, cx))
                            .checked(checked)
                            .on_click(move |_, _, cx| {
                                let name = n2.clone();
                                store.update(cx, |s, cx| {
                                    s.update_settings(
                                        |st| {
                                            st.theme_name = name.clone();
                                            st.theme = 0;
                                        },
                                        cx,
                                    )
                                });
                                let zoom = store.read(cx).settings().zoom;
                                crate::theme::apply(&name, 0, zoom, cx);
                            }),
                    );
                }
                menu
            })
    }

    /// The status pill beside a category's title.
    fn header_status(&self, cx: &App) -> Option<Div> {
        let theme = cx.theme();
        match self.category {
            Category::Servers => {
                let s = self.store.read(cx).settings();
                Some(if s.panel_url.trim().is_empty() && s.hopper_panels.is_empty() {
                    widgets::pill("No panel", theme.muted_foreground, false)
                } else if s.panel_url.trim().is_empty() {
                    let n = s.hopper_panels.len();
                    widgets::pill(if n == 1 { "Hopper Server".to_string() } else { format!("{n} Hopper Servers") }, theme.primary, true)
                } else {
                    let name = hopper::panel::KINDS
                        .iter()
                        .find(|(k, _)| *k == s.panel_kind)
                        .map_or("Panel", |(_, n)| n);
                    widgets::pill(name.split(" (").next().unwrap_or(name).to_string(), theme.primary, true)
                })
            }
            _ => None,
        }
    }

    fn sidebar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let hits = matches(&self.query);
        let searching = !self.query.trim().is_empty();
        let item = |id: ElementId, icon: IconName, label: SharedString, active: bool, cx: &App| {
            h_flex()
                .id(id)
                .h(rems(2.))
                .px_2p5()
                .gap_2p5()
                .rounded(theme.radius)
                .text_sm()
                .clickable(cx)
                .when(active, |r| widgets::row_selected(r, cx).font_medium())
                .when(!active, |r| r.hover(|s| widgets::row_hover(s, cx)))
                .child(
                    Icon::new(icon)
                        .small()
                        .text_color(if active { theme.primary } else { theme.muted_foreground }),
                )
                .child(div().flex_1().min_w_0().truncate().child(label))
        };
        let body: AnyElement = if searching {
            v_flex()
                .gap_0p5()
                .child(
                    div()
                        .px_2p5()
                        .pb_1()
                        .text_xs()
                        .font_semibold()
                        .text_color(theme.muted_foreground)
                        .child(match hits.len() {
                            0 => "NO MATCHES".to_string(),
                            1 => "1 MATCH".to_string(),
                            n => format!("{n} MATCHES"),
                        }),
                )
                .children(hits.into_iter().enumerate().map(|(i, e)| {
                    let id = e.id;
                    v_flex()
                        .id(("settings-hit", i))
                        .px_2p5()
                        .py_1p5()
                        .rounded(theme.radius)
                        .clickable(cx)
                        .hover(|s| widgets::row_hover(s, cx))
                        .when(i == 0, |r| widgets::row_hover(r, cx))
                        .child(div().text_sm().truncate().child(e.label))
                        .child(
                            h_flex()
                                .gap_1()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(Icon::new(e.cat.icon()).xsmall())
                                .child(e.cat.label()),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.jump(id, cx)))
                }))
                .into_any_element()
        } else {
            v_flex()
                .gap_0p5()
                .children(Category::ALL.into_iter().map(|c| {
                    item(
                        ElementId::Name(format!("settings-cat:{}", c.label()).into()),
                        c.icon(),
                        c.label().into(),
                        c == self.category,
                        cx,
                    )

                    .on_click(cx.listener(move |this, _, _, cx| this.show(c, cx)))
                }))
                .into_any_element()
        };
        let content = v_flex()
            .id("settings-sidebar")
            .size_full()
            .child(
                v_flex()
                    .px_3()
                    .pt_4()
                    .pb_3()
                    .gap_3()
                    .child(div().px_1().text_lg().font_semibold().child("Settings"))
                    .child(
                        Input::new(&self.search)
                            .small()
                            .prefix(Icon::new(IconName::Search).small())
                            .cleanable(true),
                    ),
            )
            .child(
                div()
                    .id("settings-nav")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_3()
                    .pb_3()
                    .child(body),
            )
            .child(
                h_flex()
                    .px_4()
                    .py_2p5()
                    .border_t_1()
                    .border_color(theme.sidebar_border)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(if searching { "Enter opens the first match · Esc clears" } else { "Ctrl+, opens Settings · Ctrl+F searches" }),
            );
        widgets::side_panel("settings-nav-resize", &self.nav_panel, |p| &mut p.nav_panel, &self.store, content, window, cx)
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
    }
}

impl Render for SettingsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = self.store.read(cx).settings().clone();
        let cat = self.category;
        let blocks = match cat {
            Category::General => self.general(&s, window.rem_size(), cx),
            Category::Accounts => self.accounts(&s, cx),
            Category::Folders => self.folders(cx),
            Category::Launchers => self.launchers(cx),
            Category::Java => self.java(&s, window, cx),
            Category::Servers => self.servers(&s, window, cx),
            Category::Content => self.content(&s, cx),
            Category::Appearance => self.appearance(&s, window, cx),
            Category::Updates => self.updates(cx),
            Category::Advanced => self.advanced(&s, window.rem_size(), cx),
        };
        let theme = cx.theme().clone();
        let header = h_flex()
            .w_full()
            .max_w(rems(MAX_W))
            .gap_4()
            .pb_2()
            .child(
                div()
                    .flex_none()
                    .size(rems(2.75))
                    .rounded(theme.radius_lg)
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme.primary.opacity(0.12))
                    .border_1()
                    .border_color(theme.primary.opacity(0.25))
                    .text_color(theme.primary)
                    .child(Icon::new(cat.icon())),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_2xl().font_semibold().child(cat.label()))
                    .child(div().text_sm().text_color(theme.muted_foreground).child(cat.blurb())),
            )
            .children(self.header_status(cx));

        // Every row is a child of the scrolled column, so search can scroll
        // to it.
        let mut children: Vec<AnyElement> = vec![header.into_any_element()];
        let mut index: HashMap<&'static str, usize> = HashMap::new();
        for block in blocks {
            let Block::Card(title, rows) = block;
            if let Some(t) = title {
                children.push(
                    div()
                        .w_full()
                        .max_w(rems(MAX_W))
                        .pt_4()
                        .pb_2()
                        .px_1()
                        .text_xs()
                        .font_semibold()
                        .text_color(theme.muted_foreground)
                        .child(t.to_uppercase())
                        .into_any_element(),
                );
            } else {
                children.push(div().h_4().into_any_element());
            }
            let n = rows.len();
            for (i, row) in rows.into_iter().enumerate() {
                index.insert(row.id, children.len());
                let flash = self.flash.filter(|(id, _)| *id == row.id).map(|(_, t)| t);
                children.push(row.render(i == 0, i + 1 == n, flash, cx));
            }
        }
        if let Some(id) = self.reveal.take()
            && let Some(ix) = index.get(id)
        {
            // The one before it shows at the top, for context.
            self.scroll.scroll_to_top_of_item(ix.saturating_sub(1));
        }

        h_flex()
            .id("settings-page")
            .size_full()
            .child(self.sidebar(window, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .relative()
                    .child(
                        div()
                            .id("settings-pane")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .px_8()
                            .pt_6()
                            .pb_12()
                            .flex()
                            .flex_col()
                            .items_center()
                            .children(children),
                    )
                    .vertical_scrollbar(&self.scroll),
            )
    }
}

// ---- rows and cards ----------------------------------------------------------------

/// A titled card of rows.
enum Block {
    Card(Option<&'static str>, Vec<Row>),
}

impl Block {
    fn card(title: Option<&'static str>, rows: Vec<Row>) -> Self {
        Block::Card(title, rows)
    }
}

/// One setting: its label and description on the left, its control on the
/// right, and maybe more under both.
struct Row {
    id: &'static str,
    desc: Option<SharedString>,
    control: Option<AnyElement>,
    note: Option<AnyElement>,
    below: Option<AnyElement>,
}

impl Row {
    fn new(id: &'static str) -> Self {
        Self {
            id,
            desc: None,
            control: None,
            note: None,
            below: None,
        }
    }

    /// A description other than the usual one.
    fn desc(mut self, d: impl Into<SharedString>) -> Self {
        self.desc = Some(d.into());
        self
    }

    fn control(mut self, c: impl IntoElement) -> Self {
        self.control = Some(c.into_any_element());
        self
    }

    fn control_any(mut self, c: Option<AnyElement>) -> Self {
        self.control = c;
        self
    }

    /// A status line under the description.
    fn note(mut self, n: Option<AnyElement>) -> Self {
        self.note = n;
        self
    }

    fn below(mut self, b: impl IntoElement) -> Self {
        self.below = Some(b.into_any_element());
        self
    }

    fn when(self, cond: bool, f: impl FnOnce(Self) -> Self) -> Self {
        if cond { f(self) } else { self }
    }

    fn when_some<T>(self, v: Option<T>, f: impl FnOnce(Self, T) -> Self) -> Self {
        match v {
            Some(v) => f(self, v),
            None => self,
        }
    }

    fn render(self, first: bool, last: bool, flash: Option<usize>, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let e = entry(self.id);
        let desc = self.desc.unwrap_or_else(|| e.desc.into());
        let radius = theme.radius_lg;
        let border = theme.foreground.opacity(0.09);
        let text = v_flex()
            .flex_1()
            .min_w(rems(14.))
            .gap_0p5()
            .child(div().text_sm().font_medium().child(e.label))
            .when(!desc.is_empty(), |c| {
                c.child(
                    div()
                        .text_xs()
                        .line_height(rems(1.1))
                        .text_color(theme.muted_foreground)
                        .child(desc),
                )
            })
            .children(self.note.map(|n| h_flex().pt_1().child(n)));
        let head = h_flex()
            .w_full()
            .flex_wrap()
            .gap_x_6()
            .gap_y_2()
            .child(text)
            .children(self.control.map(|c| h_flex().flex_none().child(c)));
        v_flex()
            .id(self.id)
            .relative()
            .w_full()
            .max_w(rems(MAX_W))
            .px_4()
            .py_3()
            .gap_3()
            .bg(theme.foreground.opacity(0.025))
            .border_color(border)
            .border_l_1()
            .border_r_1()
            .border_t_1()
            .when(first, |d| d.rounded_tl(radius).rounded_tr(radius))
            .when(last, |d| d.border_b_1().rounded_bl(radius).rounded_br(radius))
            .child(head)
            .children(self.below.map(|b| div().w_full().child(b)))
            .when_some(flash, |d, turn| {
                d.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .when(first, |d| d.rounded_tl(radius).rounded_tr(radius))
                        .when(last, |d| d.rounded_bl(radius).rounded_br(radius))
                        .bg(theme.primary.opacity(0.14))
                        .border_1()
                        .border_color(theme.primary.opacity(0.7))
                        .with_animation(
                            ("settings-flash", turn),
                            Animation::new(FLASH).with_easing(ease_in_out),
                            |el, t| el.opacity(if t < 0.4 { 1.0 } else { 1.0 - (t - 0.4) / 0.6 }),
                        ),
                )
            })
            .into_any_element()
    }
}

fn warning(text: &'static str, cx: &App) -> impl IntoElement {
    h_flex()
        .gap_1p5()
        .text_sm()
        .text_color(cx.theme().warning)
        .child(Icon::new(IconName::TriangleAlert).small())
        .child(text)
}

/// A colored line with an icon: a test result.
fn status_line(icon: IconName, text: String, color: Hsla) -> AnyElement {
    h_flex()
        .gap_1()
        .text_xs()
        .text_color(color)
        .child(Icon::new(icon).xsmall().flex_none())
        .child(div().min_w_0().child(text))
        .into_any_element()
}

/// Set by [`restart`]: quitting to come right back, so hosted servers keep
/// running for the new Hopper to pick up instead of being stopped.
pub static RESTARTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Starts Hopper again (the updated exe) and quits this one.
pub fn restart(cx: &mut App) {
    match std::env::current_exe() {
        Ok(exe) => {
            if let Err(e) = std::process::Command::new(exe).spawn() {
                log::error!("restarting: {e}");
                return;
            }
            RESTARTING.store(true, std::sync::atomic::Ordering::SeqCst);
            cx.quit();
        }
        Err(e) => log::error!("restarting: {e}"),
    }
}

/// One theme in the menu: its colors, its name, and whether it's glass.
fn theme_row(name: &str, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let swatch: AnyElement = if name.is_empty() {
        // Follow Windows: Hopper Dark beside Hopper Light.
        let [d, ..] = crate::theme::swatch(crate::theme::HOPPER_DARK);
        let [l, ..] = crate::theme::swatch(crate::theme::HOPPER_LIGHT);
        h_flex()
            .child(div().w_5().h_4().bg(d))
            .child(div().w_5().h_4().bg(l))
            .into_any_element()
    } else {
        let [bg, panel, accent, fg] = crate::theme::swatch(name);
        h_flex()
            .child(div().w_2p5().h_4().bg(panel))
            .child(
                h_flex()
                    .w_8()
                    .h_4()
                    .bg(bg)
                    .items_center()
                    .justify_center()
                    .gap_0p5()
                    .child(div().size_1p5().rounded_full().bg(accent))
                    .child(div().w_2().h_0p5().rounded_full().bg(fg)),
            )
            .into_any_element()
    };
    h_flex()
        .gap_2p5()
        .child(
            div()
                .flex_none()
                .rounded(px(3.))
                .overflow_hidden()
                .border_1()
                .border_color(theme.border)
                .child(swatch),
        )
        .child(div().flex_1().child(if name.is_empty() {
            "Follow Windows".to_string()
        } else {
            name.to_string()
        }))
        .when(crate::theme::is_glass(name), |r| {
            r.child(div().text_xs().text_color(theme.muted_foreground).child("glass"))
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{Category, ENTRIES, Text, matches};

    #[test]
    fn entries_are_unique_and_in_page_order() {
        let mut seen = std::collections::HashSet::new();
        let mut last = 0;
        for e in ENTRIES {
            assert!(seen.insert(e.id), "duplicate {}", e.id);
            let ix = Category::ALL.iter().position(|c| *c == e.cat).unwrap();
            assert!(ix >= last, "{} is out of order", e.id);
            last = ix;
        }
        // Every category has something in it.
        for c in Category::ALL {
            assert!(ENTRIES.iter().any(|e| e.cat == c), "{c:?} is empty");
        }
    }

    #[test]
    fn search_matches_every_word() {
        let ids = |q: &str| matches(q).iter().map(|e| e.id).collect::<Vec<_>>();
        assert!(ids("zoom").contains(&"zoom"));
        assert!(ids("panel key").contains(&"panel-key"));
        assert!(!ids("panel key").contains(&"cf-key"));
        assert!(ids("crafty").contains(&"panel-kind"));
        assert!(ids("").is_empty());
        assert!(ids("nothing-like-this").is_empty());
    }

    #[test]
    fn categories_by_word() {
        assert_eq!(Category::find("java"), Some(Category::Java));
        assert_eq!(Category::find("Servers"), Some(Category::Servers));
        assert_eq!(Category::find("curseforge"), Some(Category::Content));
        assert_eq!(Category::find("modrinth"), Some(Category::Content));
        assert_eq!(Category::find(""), None);
    }

    #[test]
    fn text_checks() {
        assert!(Text::PanelUrl.problem("rig:8443").is_some());
        assert!(Text::PanelUrl.problem("https://rig:8443").is_none());
        assert!(Text::UpdateRepo.problem("watdafawx/hopper").is_none());
        assert!(Text::UpdateRepo.problem("hopper").is_some());
        assert!(Text::Instances.problem("").is_none());
        assert!(Text::Instances.problem("Z:\\surely\\not\\here").is_some());
        assert!(Text::Host.problem("anything").is_none());
    }
}
