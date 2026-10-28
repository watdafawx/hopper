//! The Library page: instances on the leading side, the picked instance's
//! header and tabs in the work area.

use crate::widgets::Clickable as _;
use crate::addons::{AddonsEvent, AddonsView, in_class};
use crate::content::{ContentEvent, ScreenshotsView, WorldsView};
use crate::inst_settings::InstanceSettings;
use crate::logs::LogsView;
use crate::peers::PeersView;
use crate::rigsync::SyncPanel;
use crate::store::{self, Msg, Store};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::cf;
use hopper::instances::Instance;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// How long the tab underline and the hero's height take to move.
const MOTION: Duration = Duration::from_millis(300);

/// The hero's height in rem, the same on every tab.
const HERO: f32 = 9.5;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstTab {
    Overview,
    Mods,
    ResourcePacks,
    Shaders,
    Datapacks,
    Worlds,
    Screenshots,
    Server,
    Peers,
    Settings,
    Logs,
    Performance,
}

impl InstTab {
    pub(crate) const ALL: [InstTab; 12] = [
        InstTab::Overview,
        InstTab::Mods,
        InstTab::ResourcePacks,
        InstTab::Shaders,
        InstTab::Datapacks,
        InstTab::Worlds,
        InstTab::Screenshots,
        InstTab::Server,
        InstTab::Peers,
        InstTab::Settings,
        InstTab::Logs,
        InstTab::Performance,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            InstTab::Overview => "Overview",
            InstTab::Mods => "Mods",
            InstTab::ResourcePacks => "Resource packs",
            InstTab::Shaders => "Shaders",
            InstTab::Datapacks => "Data packs",
            InstTab::Worlds => "Worlds",
            InstTab::Screenshots => "Screenshots",
            InstTab::Server => "Server",
            InstTab::Peers => "Peers",
            InstTab::Settings => "Settings",
            InstTab::Logs => "Logs & crashes",
            InstTab::Performance => "Performance",
        }
    }

    /// Its key in settings.
    pub(crate) fn key(self) -> String {
        self.label().to_lowercase()
    }

    pub(crate) fn from_key(key: &str) -> Option<InstTab> {
        Self::ALL.into_iter().find(|t| t.key() == key)
    }

    fn icon(self) -> IconName {
        match self {
            InstTab::Overview => IconName::LayoutDashboard,
            InstTab::Mods => IconName::Puzzle,
            InstTab::ResourcePacks => IconName::Paintbrush,
            InstTab::Shaders => IconName::Sun,
            InstTab::Datapacks => IconName::Database,
            InstTab::Worlds => IconName::Globe,
            InstTab::Screenshots => IconName::Image,
            InstTab::Server => IconName::HardDrive,
            InstTab::Peers => IconName::Users,
            InstTab::Settings => IconName::SlidersHorizontal,
            InstTab::Logs => IconName::Stethoscope,
            InstTab::Performance => IconName::Activity,
        }
    }

    /// Resource packs, shaders and data packs: one "Packs" tab in the bar,
    /// with sub-tabs.
    pub(crate) fn is_pack(self) -> bool {
        matches!(self, InstTab::ResourcePacks | InstTab::Shaders | InstTab::Datapacks)
    }

    /// The addon class a tab lists, for the tabs that list addons.
    fn class(self) -> Option<u32> {
        match self {
            InstTab::Mods => Some(cf::CLASS_MODS),
            InstTab::ResourcePacks => Some(cf::CLASS_RESOURCE_PACKS),
            InstTab::Shaders => Some(cf::CLASS_SHADERS),
            InstTab::Datapacks => Some(cf::CLASS_DATAPACKS),
            _ => None,
        }
    }
}

/// The bar's tabs, left to right; `ResourcePacks` stands for "Packs".
const BAR: [InstTab; 7] = [
    InstTab::Overview,
    InstTab::Mods,
    InstTab::ResourcePacks,
    InstTab::Worlds,
    InstTab::Screenshots,
    InstTab::Logs,
    InstTab::Performance,
];
/// Tools around the pack, as icon buttons at the bar's right end. Server
/// isn't one: the header's server button opens it (also Ctrl+K and MCP).
const MANAGE: [InstTab; 2] = [InstTab::Peers, InstTab::Settings];

/// Where the header's server menu goes on the Server tab.
#[derive(Clone, Copy)]
enum ServerPlace {
    Page,
    Sync,
    Console,
}
/// The sub-tabs of "Packs".
const PACKS: [InstTab; 3] = [InstTab::ResourcePacks, InstTab::Shaders, InstTab::Datapacks];

/// A tab's view, cached: it re-renders when it changes, not on every frame
/// of the header's and underline's motion.
fn cached(view: impl Into<AnyView>) -> AnyElement {
    view.into()
        .cached(StyleRefinement::default().size_full())
        .into_any_element()
}

fn id(prefix: &str, path: &Path) -> ElementId {
    ElementId::Name(format!("{prefix}:{}", path.display()).into())
}

/// What the Library asks the rest of the app to do.
pub enum LibraryEvent {
    /// Find content of a class for an instance; data packs go to `dest`
    /// (relative to the instance).
    Browse {
        root: PathBuf,
        class: u32,
        dest: Option<String>,
    },
    /// Find a modpack to install.
    BrowsePacks,
    /// Search Browse for mods by name or id.
    Find { root: PathBuf, text: String },
    /// Show a project's page in Browse, installing into `root`.
    OpenProject { root: PathBuf, project: u64 },
    /// Compare a server with its pack (its Sync tab on the Servers page).
    Compare(PathBuf),
}

impl EventEmitter<LibraryEvent> for LibraryView {}

/// Dragging the instance list's edge.
struct SidebarResize;

/// What a drag shows under the pointer: nothing.
struct Blank;

impl Render for Blank {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// Instance list widths in rem: narrowest open width, widest, and the
/// width below which letting go collapses it to the rail.
const SIDEBAR_MIN: f32 = 14.0;
const SIDEBAR_MAX: f32 = 36.0;
const SIDEBAR_COLLAPSE: f32 = 10.0;
const RAIL: f32 = 4.5;

pub struct LibraryView {
    /// The list's width while its edge is being dragged.
    drag_width: Option<f32>,
    /// Instance groups folded shut.
    folded: std::collections::HashSet<String>,
    store: Entity<Store>,
    /// The Overview's modpack description, its GIFs playing.
    pack_desc: Entity<widgets::DescView>,
    selected: Option<PathBuf>,
    tab: InstTab,
    /// The tab before the last switch, and when it happened, for the
    /// sliding underline and the hero's height.
    tab_from: InstTab,
    tab_moved: Option<Instant>,
    tab_serial: usize,
    /// Where each tab was last laid out, for the underline.
    /// (left, width) of each tab from the first one's left edge.
    tab_bounds: Rc<RefCell<Vec<(Pixels, Pixels)>>>,
    /// The pack sub-tab "Packs" opens on: the last one shown.
    last_pack: InstTab,
    /// The window's width and rem size at the last render, for fitting the
    /// tab bar.
    view_w: Pixels,
    rem: Pixels,
    /// Blurred pack art per icon file (None while it's being made).
    blurs: HashMap<PathBuf, Option<PathBuf>>,
    /// Folder size per instance (None while it's being counted).
    sizes: HashMap<PathBuf, Option<u64>>,
    /// Each paired server's last start crash, read when its instance is
    /// picked (the header's status dot); the Server tab's own read wins.
    crashes: HashMap<PathBuf, Option<hopper::crashdetect::Crash>>,
    /// The work area's width in rem, as of the last render.
    detail_w: f32,
    addons: Entity<AddonsView>,
    server: Entity<SyncPanel>,
    worlds: Entity<WorldsView>,
    shots: Entity<ScreenshotsView>,
    settings: Entity<InstanceSettings>,
    logs: Entity<LogsView>,
    perf: Entity<crate::perf_view::PerfView>,
    peers: Entity<PeersView>,
    search: Entity<InputState>,
    query: String,
    /// The shown instance's snapshot count, read at most every few seconds.
    snap_count: std::cell::RefCell<Option<(PathBuf, std::time::Instant, usize)>>,
    _subscriptions: Vec<Subscription>,
}

impl LibraryView {
    /// Ctrl+F: the Mods search of the picked instance.
    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab.class().is_none() {
            self.set_tab(InstTab::Mods, cx);
        }
        self.addons.update(cx, |a, cx| a.focus_search(window, cx));
    }

    /// Where dropped files go: the picked instance, and the folder for the
    /// open tab (jars always go to mods).
    pub fn drop_target(&self, cx: &App) -> (Option<PathBuf>, String) {
        let folder = match self.tab {
            InstTab::ResourcePacks => "resourcepacks",
            InstTab::Shaders => "shaderpacks",
            InstTab::Datapacks => {
                // The data pack location shown.
                if let Some(f) = self.addons.read(cx).folder.clone() {
                    return (self.selected.clone(), f);
                }
                "mods"
            }
            InstTab::Worlds => "saves",
            InstTab::Screenshots => "screenshots",
            _ => "mods",
        };
        (self.selected.clone(), folder.to_string())
    }

    /// Selects an instance (from elsewhere in the app).
    pub fn open(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.select(root, cx);
    }

    /// Picks the instance `delta` places up or down the list.
    pub fn step_instance(&mut self, delta: isize, cx: &mut Context<Self>) {
        let list: Vec<PathBuf> = self.ordered(cx).into_iter().flat_map(|(_, l)| l).collect();
        if list.is_empty() {
            return;
        }
        let at = self
            .selected
            .as_ref()
            .and_then(|s| list.iter().position(|r| r == s))
            .map_or(0, |i| i as isize);
        let next = (at + delta).clamp(0, list.len() as isize - 1) as usize;
        self.select(list[next].clone(), cx);
    }

    pub fn selected(&self) -> Option<&PathBuf> {
        self.selected.as_ref()
    }

    pub fn addons(&self) -> &Entity<AddonsView> {
        &self.addons
    }

    /// What either table asks of the Library.
    fn addons_event(&mut self, event: &AddonsEvent, cx: &mut Context<Self>) {
        match event {
            AddonsEvent::AddContent { root, class } => cx.emit(LibraryEvent::Browse {
                root: root.clone(),
                class: *class,
                dest: None,
            }),
            AddonsEvent::AddTo { root, class, dest } => cx.emit(LibraryEvent::Browse {
                root: root.clone(),
                class: *class,
                dest: Some(dest.clone()),
            }),
            AddonsEvent::Find { root, text } => cx.emit(LibraryEvent::Find {
                root: root.clone(),
                text: text.clone(),
            }),
            AddonsEvent::OpenProject { root, project } => cx.emit(LibraryEvent::OpenProject {
                root: root.clone(),
                project: *project,
            }),
            AddonsEvent::Compare(server) => cx.emit(LibraryEvent::Compare(server.clone())),
            AddonsEvent::CollapseList => self.set_collapsed(true, cx),
        }
    }

    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search instances"));
        let addons = cx.new(|cx| AddonsView::new(store.clone(), window, cx));
        let server = cx.new(|cx| {
            let mut p = SyncPanel::new(store.clone(), window, cx);
            p.set_in_instance(true);
            p
        });
        let worlds = cx.new(|cx| WorldsView::new(store.clone(), window, cx));
        let peek = addons.read(cx).peek().clone();
        let shots = cx.new(|cx| ScreenshotsView::new(store.clone(), window, cx));
        let settings = cx.new(|cx| InstanceSettings::new(store.clone(), window, cx));
        let logs = cx.new(|cx| LogsView::new(store.clone(), window, cx));
        let perf = cx.new(|cx| crate::perf_view::PerfView::new(store.clone(), window, cx));
        let peers = cx.new(|cx| PeersView::new(store.clone(), window, cx));
        server.update(cx, |s, _| s.set_peek(peek.clone()));
        let control = server.read(cx).control().clone();
        let subs = vec![
            // The header's server dot follows the panel's polls and the
            // Server tab's crash read.
            cx.observe(&control, |_, _, cx| cx.notify()),
            cx.subscribe(&server, |_, _, _: &crate::rigsync::SyncEvent, cx| cx.notify()),
            cx.observe(&peek, |_, _, cx| cx.notify()),
            cx.subscribe(&addons, |this, _, event, cx| this.addons_event(event, cx)),
            cx.subscribe(&worlds, |this, _, event, cx| this.content_event(event, cx)),
            cx.observe(&store, |this, _, cx| {
                this.ensure_selection(cx);
                this.follow_paired_server(cx);
                this.watch_paired(false, cx);
                cx.notify();
            }),
            cx.subscribe_in(&search, window, |this, state, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.query = state.read(cx).value().trim().to_lowercase();
                    cx.notify();
                }
            }),
        ];
        let first_tab = {
            let st = store.read(cx).settings();
            let key = if st.instance_tab.is_empty() { &st.last_tab } else { &st.instance_tab };
            // Before "Last used", a switch picked Overview or Mods.
            let old_mods = st.unknown.get("instance_opens_mods").and_then(|v| v.as_bool()) == Some(true);
            InstTab::from_key(key).unwrap_or(if old_mods { InstTab::Mods } else { InstTab::Overview })
        };
        let pack_desc = cx.new(|cx| widgets::DescView::new(store.clone(), window, cx));
        // Back in the window: the header's server dot is read once.
        let mut subs = subs;
        subs.push(cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.server.update(cx, |p, cx| p.refresh_state(cx));
            }
        }));
        let mut view = Self {
            drag_width: None,
            folded: Default::default(),
            store,
            pack_desc,
            selected: None,
            tab: first_tab,
            tab_from: first_tab,
            tab_moved: None,
            tab_serial: 0,
            tab_bounds: Default::default(),
            last_pack: InstTab::ResourcePacks,
            view_w: px(1440.),
            rem: px(16.),
            blurs: HashMap::new(),
            sizes: HashMap::new(),
            crashes: HashMap::new(),
            detail_w: 80.0,
            addons,
            server,
            worlds,
            shots,
            settings,
            logs,
            perf,
            peers,
            search,
            query: String::new(),
            snap_count: Default::default(),
            _subscriptions: subs,
        };
        view.ensure_selection(cx);
        view.apply_args(cx);
        view
    }

    /// `--instance name` and `--tab mods|worlds|…`, for scripted runs.
    fn apply_args(&mut self, cx: &mut Context<Self>) {
        let args: Vec<String> = std::env::args().collect();
        let value = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .and_then(|i| args.get(i + 1).cloned())
        };
        self.navigate(value("--instance").as_deref(), value("--tab").as_deref(), cx);
    }

    /// Picks an instance (part of its name) and a tab (start of its label).
    pub fn navigate(&mut self, instance: Option<&str>, tab: Option<&str>, cx: &mut Context<Self>) {
        if let Some(name) = instance {
            let name = name.to_lowercase();
            let root = self
                .store
                .read(cx)
                .instances()
                .iter()
                .find(|i| i.name.to_lowercase().contains(&name))
                .map(|i| i.root.clone());
            if let Some(r) = root {
                self.select(r, cx);
            }
        }
        if let Some(t) = tab {
            let t = t.to_lowercase().replace(' ', "");
            if let Some(tab) = InstTab::ALL
                .into_iter()
                .find(|x| x.label().to_lowercase().replace(' ', "").starts_with(&t))
            {
                self.set_tab(tab, cx);
            }
        }
    }

    /// Keeps a valid instance picked.
    /// On the Server tab, points the compare panel at the paired server once
    /// it is known: at startup the tab opens before the servers have loaded.
    fn follow_paired_server(&mut self, cx: &mut Context<Self>) {
        if self.tab != InstTab::Server {
            return;
        }
        let Some(root) = self.selected.as_ref() else { return };
        let Some(server) = self.store.read(cx).server_of(root).map(|s| s.root.clone()) else {
            return;
        };
        if self.server.read(cx).server() != Some(server.as_path()) {
            self.server.update(cx, |p, cx| p.show(server, cx));
        }
    }

    /// Points the server control at the picked instance's server (for the
    /// header's Start / Stop) and reads its last start's crash, off the UI
    /// thread: once, or again when `again`.
    fn watch_paired(&mut self, again: bool, cx: &mut Context<Self>) {
        let Some(root) = self.selected.as_ref() else { return };
        let Some(server) = self.store.read(cx).server_of(root).map(|s| s.root.clone()) else {
            return;
        };
        self.server.update(cx, |p, cx| {
            p.watch(server.clone(), cx);
            if again {
                p.refresh_state(cx);
            }
        });
        if !again && self.crashes.contains_key(&server) {
            return;
        }
        self.crashes.entry(server.clone()).or_insert(None);
        cx.spawn(async move |this, cx| {
            let r = server.clone();
            let crash = cx
                .background_spawn(async move { hopper::crashscan::last_crash(&r) })
                .await;
            this.update(cx, |this, cx| {
                this.crashes.insert(server, crash);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A server's dot color, state word and, when its last start crashed,
    /// why: the Server tab's reading when it has one, else the header's.
    fn server_state(&self, root: &Path, cx: &App) -> (Hsla, &'static str, Option<String>) {
        let panel = self.server.read(cx);
        let crash = match panel.crash_of(root) {
            Some(c) => c.cloned(),
            None => self.crashes.get(root).cloned().flatten(),
        };
        let ctl = panel.control().read(cx);
        // The panel didn't answer: no state to show.
        if ctl.cant_reach(root) {
            return (cx.theme().muted_foreground, "unreachable", Some(format!("Can't reach {}", ctl.panel_name(cx))));
        }
        let store = self.store.read(cx);
        let stats = ctl.stats_of(root).or_else(|| store.panel_stats(root));
        let pinged = store.status(root).map(|s| matches!(s, hopper::server::Status::Online { .. }));
        let busy = ctl.stats_of(root).and(ctl.busy());
        let (color, word) = crate::server_control::state_of(stats, pinged, crash.is_some(), busy, cx);
        let reason = crash
            .filter(|_| word.starts_with("crashed"))
            .map(|c| crate::rigsync::crash_reason(&c));
        (color, word, reason)
    }

    /// The Server tab, on the part the header's server menu asked for.
    fn open_server_tab(&mut self, to: ServerPlace, cx: &mut Context<Self>) {
        self.set_tab(InstTab::Server, cx);
        match to {
            ServerPlace::Page => {}
            ServerPlace::Sync => self.server.update(cx, |p, cx| p.open_sync(cx)),
            ServerPlace::Console => self.server.update(cx, |p, cx| p.open_console(cx)),
        }
    }

    fn ensure_selection(&mut self, cx: &mut Context<Self>) {
        let store = self.store.read(cx);
        if self
            .selected
            .as_ref()
            .is_some_and(|s| store.instance(s).is_some())
        {
            return;
        }
        // The one picked last time, else the first in the list.
        let last = PathBuf::from(&store.settings().last_instance);
        let pick = if store.instance(&last).is_some() {
            Some(last)
        } else {
            self.ordered(cx).into_iter().flat_map(|(_, l)| l).next()
        };
        if let Some(root) = pick {
            self.select(root, cx);
        }
    }

    fn select(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.selected = Some(root.clone());
        self.watch_paired(true, cx);
        let key = root.to_string_lossy().to_string();
        self.store.update(cx, |s, cx| {
            if s.settings().last_instance != key {
                s.update_settings(|st| st.last_instance = key, cx);
            }
        });
        self.want_tab_images(cx);
        cx.notify();
    }

    /// Opens a project's page docked beside an instance, on the Mods tab
    /// unless the tab shown already has the panel.
    pub fn peek(&mut self, root: PathBuf, project: u64, cx: &mut Context<Self>) {
        if self.selected.as_ref() != Some(&root) {
            self.select(root.clone(), cx);
        }
        if self.tab.class().is_none() && !matches!(self.tab, InstTab::Server | InstTab::Datapacks) {
            self.set_tab(InstTab::Mods, cx);
        }
        let peek = self.addons.read(cx).peek().clone();
        peek.update(cx, |p, cx| {
            // Not a toggle: the same project stays open.
            if p.selected_id() != Some(project) {
                p.peek_project(root, Some(project), cx);
            }
        });
        cx.notify();
    }

    /// Where the Library is, for back and forward: the instance, its tab
    /// and the project open beside it.
    pub(crate) fn nav_loc(&self, cx: &App) -> (Option<PathBuf>, InstTab, Option<u64>) {
        let peek = self.addons.read(cx).peek().read(cx).selected_id();
        (self.selected.clone(), self.tab, peek)
    }

    /// The project panel beside the instance, watched for back and forward.
    pub(crate) fn peek_view(&self, cx: &App) -> Entity<crate::browse::BrowseView> {
        self.addons.read(cx).peek().clone()
    }

    /// Goes back (or forward) to a place `nav_loc` gave.
    pub(crate) fn nav_restore(
        &mut self,
        inst: Option<PathBuf>,
        tab: InstTab,
        peek: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        if let Some(r) = inst
            && self.selected.as_ref() != Some(&r)
        {
            self.select(r, cx);
        }
        self.set_tab(tab, cx);
        let Some(root) = self.selected.clone() else { return };
        let view = self.addons.read(cx).peek().clone();
        view.update(cx, |p, cx| {
            if p.selected_id() != peek {
                p.peek_project(root, peek, cx);
            }
        });
    }

    fn set_tab(&mut self, tab: InstTab, cx: &mut Context<Self>) {
        if tab.is_pack() {
            self.last_pack = tab;
        }
        if self.tab == InstTab::Server && tab != InstTab::Server {
            self.server.update(cx, |p, cx| p.hidden(cx));
        }
        if tab != self.tab {
            self.tab_from = self.tab;
            self.tab_moved = Some(Instant::now());
            self.tab_serial += 1;
        }
        self.tab = tab;
        let key = tab.key();
        self.store.update(cx, |s, cx| {
            if s.settings().last_tab != key {
                s.update_settings(|st| st.last_tab = key, cx);
            }
        });
        self.want_tab_images(cx);
        cx.notify();
    }

    fn content_event(&mut self, event: &ContentEvent, cx: &mut Context<Self>) {
        let Some(root) = self.selected.clone() else { return };
        match event {
            ContentEvent::Browse { class, dest } => cx.emit(LibraryEvent::Browse {
                root,
                class: *class,
                dest: dest.clone(),
            }),
        }
    }

    /// Points the tab's view at the picked instance.
    fn want_tab_images(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.selected.clone() else {
            return;
        };
        match self.tab {
            InstTab::Overview => {
                self.load_overview(root, cx);
                return;
            }
            InstTab::Worlds => {
                self.worlds.update(cx, |w, cx| w.show_for(root, cx));
                return;
            }
            InstTab::Screenshots => {
                self.shots.update(cx, |w, cx| w.show_for(root, cx));
                return;
            }
            InstTab::Settings => {
                self.settings.update(cx, |w, cx| w.show_for(root, cx));
                return;
            }
            InstTab::Logs => {
                self.logs.update(cx, |w, cx| w.show_for(root, cx));
                return;
            }
            InstTab::Performance => {
                self.perf.update(cx, |w, cx| w.show_for(root, cx));
                return;
            }
            InstTab::Peers => {
                self.peers.update(cx, |w, cx| w.show_for(root, cx));
                return;
            }
            _ => {}
        }
        if self.tab == InstTab::Server {
            // A mod page left open on another tab doesn't follow here.
            let peek = self.addons.read(cx).peek().clone();
            peek.update(cx, |p, cx| p.peek_project(root.clone(), None, cx));
            let server = self.store.read(cx).server_of(&root).map(|s| s.root.clone());
            if let Some(s) = server {
                self.server.update(cx, |p, cx| p.show(s, cx));
            }
            return;
        }
        let Some(class) = self.tab.class() else {
            return;
        };
        self.addons.update(cx, |a, cx| a.show_for(root, class, cx));
    }

    /// Asks for the page of the instance's modpack.
    fn load_overview(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let id = self
            .store
            .read(cx)
            .instance(&root)
            .and_then(|i| i.pack.as_ref())
            .map_or(0, |p| p.project_id);
        self.store.update(cx, |s, _| s.want_pack_page(id));
    }

    /// Starts blurring `icon` for the hero backdrop, once.
    fn want_blur(&mut self, icon: &Path, cx: &mut Context<Self>) {
        if self.blurs.contains_key(icon) {
            return;
        }
        let icon = icon.to_path_buf();
        self.blurs.insert(icon.clone(), None);
        cx.spawn(async move |this, cx| {
            let src = icon.clone();
            let r = cx
                .background_spawn(async move { hopper::imgcache::blurred(&src) })
                .await;
            match r {
                Ok(p) => {
                    this.update(cx, |this, cx| {
                        this.blurs.insert(icon, Some(p));
                        cx.notify();
                    })
                    .ok();
                }
                Err(e) => log::debug!("blur {}: {e:#}", icon.display()),
            }
        })
        .detach();
    }

    /// Starts counting the instance folder's size, once per run.
    fn want_size(&mut self, root: &Path, cx: &mut Context<Self>) {
        if self.sizes.contains_key(root) {
            return;
        }
        let root = root.to_path_buf();
        self.sizes.insert(root.clone(), None);
        cx.spawn(async move |this, cx| {
            let dir = root.clone();
            let n = cx
                .background_spawn(async move { hopper::instance_ops::folder_size(&dir) })
                .await;
            this.update(cx, |this, cx| {
                this.sizes.insert(root, Some(n));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Whether a tab switch is still moving (underline, hero height).
    fn moving(&self) -> bool {
        self.tab_moved.is_some_and(|t| t.elapsed() < MOTION)
    }

    /// Instance roots by group (ungrouped first), sorted and filtered.
    fn ordered(&self, cx: &App) -> Vec<(String, Vec<PathBuf>)> {
        let store = self.store.read(cx);
        let settings = store.settings();
        let mut groups: BTreeMap<String, Vec<&Instance>> = BTreeMap::new();
        for inst in store.instances() {
            if !hopper::fuzzy::words_match(&self.query, &[&inst.name]) {
                continue;
            }
            let g = settings
                .groups
                .get(inst.root.to_string_lossy().as_ref())
                .cloned()
                .unwrap_or_default();
            groups.entry(g).or_default().push(inst);
        }
        let recent = settings.library_by_recent;
        groups
            .into_iter()
            .map(|(g, mut list)| {
                list.sort_by(|a, b| {
                    let by_name = || a.name.to_lowercase().cmp(&b.name.to_lowercase());
                    if recent {
                        b.last_played.cmp(&a.last_played).then_with(by_name)
                    } else {
                        by_name()
                    }
                });
                (g, list.into_iter().map(|i| i.root.clone()).collect())
            })
            .collect()
    }

    fn play(&mut self, root: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let native = self.store.read(cx).settings().launch_native;
        if native && !hopper::msauth::has_saved_login() {
            crate::dialogs::open_sign_in(self.store.clone(), window, cx);
            return;
        }
        if let Err(e) = self.store.update(cx, |s, _| s.play(root)) {
            window.push_notification(
                gpui_kit::component::notification::Notification::warning(e),
                cx,
            );
        }
    }

    fn confirm_stop(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let name = store
            .read(cx)
            .instance(&root)
            .map(|i| i.name.clone())
            .unwrap_or_default();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (store, root) = (store.clone(), root.clone());
            alert
                .title(format!("Stop {name}?"))
                .description(
                    "Minecraft closes without saving. Use the game's menu to quit when you can.",
                )
                .confirm()
                .ok_text("Stop game")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    store.read(cx).stop(&root);
                    true
                })
        });
    }

    // ---- instance list -------------------------------------------------

    fn set_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        self.store.update(cx, |s, cx| {
            s.update_settings(|st| st.sidebar_collapsed = collapsed, cx)
        });
    }

    /// The list's edge was let go: collapse below the threshold, else keep
    /// the new width.
    fn end_resize(&mut self, cx: &mut Context<Self>) {
        let Some(w) = self.drag_width.take() else { return };
        self.store.update(cx, |s, cx| {
            s.update_settings(
                |st| {
                    if w < SIDEBAR_COLLAPSE {
                        st.sidebar_collapsed = true;
                    } else {
                        st.sidebar_collapsed = false;
                        st.sidebar_width = w.clamp(SIDEBAR_MIN, SIDEBAR_MAX);
                    }
                },
                cx,
            )
        });
        cx.notify();
    }

    /// The instance list, or a narrow rail of icons when collapsed. Drag
    /// its edge to resize; below the minimum it collapses. Toggling slides
    /// the width.
    fn sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let settings = self.store.read(cx).settings();
        let saved = settings.sidebar_width.clamp(SIDEBAR_MIN, SIDEBAR_MAX);
        let (collapsed, width) = match self.drag_width {
            Some(w) if w < SIDEBAR_COLLAPSE => (true, RAIL),
            Some(w) => (false, w.clamp(SIDEBAR_MIN, SIDEBAR_MAX)),
            None if settings.sidebar_collapsed => (true, RAIL),
            None => (false, saved),
        };
        let from = if collapsed { saved } else { RAIL };
        let inner = if collapsed {
            self.rail(cx).into_any_element()
        } else {
            self.full_sidebar(width, cx).into_any_element()
        };
        let theme = cx.theme();
        let (handle_bg, active) = (theme.primary, self.drag_width.is_some());
        div()
            .relative()
            .flex_none()
            .h_full()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(div().size_full().overflow_hidden().child(inner))
            .child(
                div()
                    .id("sidebar-resize")
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(rems(-0.25))
                    .w(rems(0.5))
                    .cursor_col_resize()
                    .when(active, |d| d.bg(handle_bg.opacity(0.5)))
                    .hover(move |s| s.bg(handle_bg.opacity(0.35)))
                    .on_drag(SidebarResize, |_, _, _, cx| cx.new(|_| Blank)),
            )
            .with_animation(
                ElementId::Name(format!("sidebar:{collapsed}").into()),
                Animation::new(std::time::Duration::from_millis(220))
                    .with_easing(ease_out_quint()),
                move |el, t| el.w(rems(from + (width - from) * t)),
            )
            .into_any_element()
    }

    fn rail(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let roots: Vec<PathBuf> = self.ordered(cx).into_iter().flat_map(|(_, l)| l).collect();
        let view = cx.entity();
        let store = self.store.read(cx);
        let theme = cx.theme();
        let items: Vec<AnyElement> = roots
            .into_iter()
            .filter_map(|root| {
                let inst = store.instance(&root)?;
                let icon = inst
                    .icon
                    .as_deref()
                    .and_then(|u| store.image(u))
                    .map(Path::to_path_buf);
                let picked = self.selected.as_deref() == Some(root.as_path());
                let playing = store.running(&root).is_some();
                let name = inst.name.clone();
                let r = root.clone();
                Some(
                    div()
                        .id(id("rail", &root))
                        .relative()
                        .p_0p5()
                        .rounded(theme.radius_lg)
                        .border_2()
                        .clickable(cx)
                        .border_color(if picked { theme.primary } else { theme.transparent })
                        .when(picked, |d| {
                            d.shadow(vec![
                                BoxShadow::new(px(0.), px(4.), theme.primary.opacity(0.35)).blur_radius(px(14.)),
                            ])
                        })
                        .hover(|s| s.bg(theme.list_hover))
                        .tooltip(move |w, cx| {
                            gpui_kit::component::tooltip::Tooltip::new(name.clone()).build(w, cx)
                        })
                        .on_click(cx.listener(move |this, _, _, cx| this.select(r.clone(), cx)))
                        .context_menu({
                            let (view, root) = (view.clone(), root.clone());
                            move |menu, _, cx| instance_menu(menu, &view, &root, false, cx)
                        })
                        .child(widgets::thumb(icon.as_deref(), rems(2.5), cx))
                        .when(playing, |d| {
                            d.child(
                                div()
                                    .absolute()
                                    .bottom_0()
                                    .right_0()
                                    .size_2p5()
                                    .rounded_full()
                                    .border_2()
                                    .border_color(theme.sidebar)
                                    .bg(theme.success),
                            )
                        })
                        .into_any_element(),
                )
            })
            .collect();
        v_flex()
            .w(rems(4.5))
            .h_full()
            .items_center()
            .pt_3()
            .gap_2()
            .child(
                Button::new("expand-sidebar")
                    .ghost()
                    .small()
                    .icon(IconName::PanelLeftOpen)
                    .tooltip("Show instance list")
                    .on_click(cx.listener(|this, _, _, cx| this.set_collapsed(false, cx))),
            )
            .child(
                Button::new("rail-new")
                    .ghost()
                    .small()
                    .icon(IconName::Plus)
                    .tooltip("New instance")
                    .on_click(cx.listener(|this, _, window, cx| {
                        crate::dialogs::open_create(this.store.clone(), window, cx)
                    })),
            )
            .child(div().w_8().h_px().my_1().bg(theme.sidebar_border))
            .child(
                v_flex()
                    .id("rail-list")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .items_center()
                    .gap_1p5()
                    .pb_3()
                    .children(items)
                    .overflow_y_scrollbar(),
            )
    }

    fn full_sidebar(&self, width: f32, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let recent = self.store.read(cx).settings().library_by_recent;
        v_flex()
            .w(rems(width))
            .h_full()
            .child(
                v_flex()
                    .gap_3()
                    .px_4()
                    .pt_4()
                    .pb_3()
                    .child(
                        h_flex()
                            .justify_between()
                            .child(div().text_lg().font_semibold().child("Instances"))
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        Button::new("new-instance")
                                            .icon(IconName::Plus)
                                            .small()
                                            .ghost()
                                            .tooltip("New instance")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                crate::dialogs::open_create(
                                                    this.store.clone(),
                                                    window,
                                                    cx,
                                                )
                                            })),
                                    )
                                    .child(
                                        Button::new("get-packs")
                                            .icon(IconName::Package)
                                            .small()
                                            .ghost()
                                            .tooltip("Find a modpack")
                                            .on_click(cx.listener(|_, _, _, cx| {
                                                cx.emit(LibraryEvent::BrowsePacks)
                                            })),
                                    )
                                    .child(
                                        Button::new("import-pack")
                                            .icon(IconName::Download)
                                            .small()
                                            .ghost()
                                            .tooltip("Import a modpack zip or .mrpack")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                crate::dialogs::import_pack(this.store.clone(), cx)
                                            })),
                                    )
                                    .child(
                                        Button::new("redeem-code")
                                            .icon(IconName::Link)
                                            .small()
                                            .ghost()
                                            .tooltip("Install from a share code")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                crate::dialogs::open_redeem(this.store.clone(), window, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new("collapse-sidebar")
                                            .icon(IconName::PanelLeftClose)
                                            .small()
                                            .ghost()
                                            .tooltip("Hide instance list")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.set_collapsed(true, cx)
                                            })),
                                    ),
                            ),
                    )
                    .child(
                        Input::new(&self.search)
                            .small()
                            .prefix(Icon::new(IconName::Search).small())
                            .cleanable(true),
                    )
                    .child(self.sort_bar(recent, cx)),
            )
            .child(self.instance_list(cx))
    }

    /// Name or Recent, styled like Browse's sort row: a SORT label and a
    /// track whose two halves share the width, the picked one tinted.
    fn sort_bar(&self, recent: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let (muted, fg, hover, track, radius, color) =
            (theme.muted_foreground, theme.foreground, theme.list_hover, theme.muted, theme.radius, theme.primary);
        let button = |key: &'static str, label: &'static str, icon: IconName, by_recent: bool, cx: &mut Context<Self>| {
            let on = by_recent == recent;
            h_flex()
                .id(ElementId::Name(format!("lib-sort-{key}").into()))
                .flex_1()
                .justify_center()
                .h_7()
                .px_2()
                .gap_1p5()
                .rounded(radius)
                .text_sm()
                .clickable(cx)
                .child(Icon::new(icon).small())
                .child(label)
                .map(|d| {
                    if on {
                        d.bg(color.opacity(0.15)).text_color(color).font_medium()
                    } else {
                        d.text_color(muted).hover(move |s| s.bg(hover).text_color(fg))
                    }
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if by_recent != recent {
                        this.store.update(cx, |s, cx| s.update_settings(|st| st.library_by_recent = by_recent, cx));
                    }
                }))
        };
        h_flex()
            .w_full()
            .gap_2()
            .child(div().flex_none().text_xs().font_semibold().text_color(muted).child("SORT"))
            .child(
                h_flex()
                    .flex_1()
                    .p_0p5()
                    .gap_0p5()
                    .rounded(radius * 1.5)
                    .bg(track)
                    .child(button("name", "Name", IconName::ArrowDownAZ, false, cx))
                    .child(button("recent", "Recent", IconName::Clock, true, cx)),
            )
    }

    fn instance_list(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let groups = self.ordered(cx);
        let empty = groups.is_empty();
        let mut list = v_flex()
            .id("instance-list")
            .flex_1()
            .min_h_0()
            .px_2()
            .pb_3()
            .gap_0p5();
        for (name, roots) in groups {
            let folded = !name.is_empty() && self.folded.contains(&name);
            if !name.is_empty() {
                let g = name.clone();
                let theme = cx.theme();
                list = list.child(
                    h_flex()
                        .id(SharedString::from(format!("group:{name}")))
                        .px_2()
                        .pt_3()
                        .pb_1()
                        .gap_2()
                        .text_xs()
                        .font_semibold()
                        .clickable(cx)
                        .text_color(theme.muted_foreground)
                        .hover(|s| s.text_color(theme.foreground))
                        .child(
                            Icon::new(if folded {
                                IconName::ChevronRight
                            } else {
                                IconName::ChevronDown
                            })
                            .xsmall(),
                        )
                        .child(name.to_uppercase())
                        .child(roots.len().to_string())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.folded.remove(&g) {
                                this.folded.insert(g.clone());
                            }
                            cx.notify();
                        })),
                );
            }
            if folded {
                continue;
            }
            for root in roots {
                list = list.child(self.instance_row(&root, cx));
            }
        }
        if empty {
            list = list.child(if self.query.is_empty() {
                widgets::empty_state(
                    IconName::Package,
                    "No instances yet",
                    Some("Install a modpack, or make an instance of your own.".into()),
                    cx,
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("empty-browse")
                                .small()
                                .primary()
                                .icon(IconName::Package)
                                .label("Find a modpack")
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(LibraryEvent::BrowsePacks))),
                        )
                        .child(
                            Button::new("empty-new")
                                .small()
                                .outline()
                                .icon(IconName::Plus)
                                .label("New instance")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    crate::dialogs::open_create(this.store.clone(), window, cx)
                                })),
                        ),
                )
            } else {
                widgets::empty_state(IconName::Search, "No instance matches", None, cx)
            });
        }
        list.overflow_y_scrollbar()
    }

    fn instance_row(&self, root: &Path, cx: &mut Context<Self>) -> AnyElement {
        let store = self.store.read(cx);
        let Some(inst) = store.instance(root) else {
            return div().into_any_element();
        };
        let picked = self.selected.as_deref() == Some(root);
        let playing = store.running(root).is_some();
        let pack_update = store.pack_update(inst).is_some();
        let mod_updates = match store.updates(root) {
            Some(crate::store::Updates::Ready(u)) => u.len(),
            _ => 0,
        };
        let icon = inst
            .icon
            .as_deref()
            .and_then(|u| store.image(u))
            .map(Path::to_path_buf);
        let mut sub = format!(
            "{} · {}",
            inst.game_version,
            store::loader_label(&inst.loader)
        );
        if let Some(p) = &inst.pack
            && !p.version.is_empty()
        {
            sub.push_str(&format!(" · v{}", p.version));
        }
        let name = inst.name.clone();
        let theme = cx.theme();
        let group: SharedString = format!("row:{}", root.display()).into();
        // One update signal: a pack update covers the mods too.
        let mods_word = if mod_updates == 1 { "mod update" } else { "mod updates" };
        let signal = if pack_update {
            Some((
                widgets::chip(cx.theme().primary).xsmall().child("Update"),
                if mod_updates > 0 {
                    format!("Pack update available, and {mod_updates} {mods_word}")
                } else {
                    "Pack update available".to_string()
                },
            ))
        } else if mod_updates > 0 {
            Some((widgets::chip(cx.theme().info).xsmall().child(format!("{mod_updates}")), format!("{mod_updates} {mods_word}")))
        } else {
            None
        };
        let sub_tip = SharedString::from(sub.clone());

        let thumb = div()
            .relative()
            .flex_none()
            .child(widgets::thumb(icon.as_deref(), rems(2.5), cx))
            .when(!playing, |d| {
                let root = root.to_path_buf();
                d.child(
                    div()
                        .id(id("row-play", &root))
                        .absolute()
                        .inset_0()
                        .rounded(theme.radius)
                        .bg(theme.overlay)
                        .flex()
                        .items_center()
                        .justify_center()
                        .opacity(0.)
                        .group_hover(group.clone(), |s| s.opacity(1.))
                        .clickable(cx)
                        // Shown while focused too, not only on hover.
                        .focus_visible({
                            let ring = widgets::focus_shadow(cx);
                            move |s| s.opacity(1.).shadow(ring)
                        })

                        .tooltip(|window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new("Play").build(window, cx)
                        })
                        .child(Icon::new(IconName::Play).text_color(theme.foreground))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.play(&root, window, cx);
                        })),
                )
            });

        let select_root = root.to_path_buf();
        let menu_root = root.to_path_buf();
        let menu_view = cx.entity();
        h_flex()
            .id(id("row", root))
            .group(group)
            .relative()
            .gap_3()
            .px_2()
            .py_1p5()
            .rounded(theme.radius_lg)
            .clickable(cx)
            .when(picked, |r| widgets::row_selected(r, cx))
            .when(!picked, |r| r.hover(|s| widgets::row_hover(s, cx)))
            .on_click(cx.listener(move |this, _, _, cx| this.select(select_root.clone(), cx)))
            .child(thumb)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .font_medium()
                                    .truncate()
                                    .child(name),
                            )
                            .when(playing, |r| {
                                r.child(widgets::chip(cx.theme().success).xsmall().child("Playing"))
                            })
                            .when_some(signal, |r, (tag, tip)| {
                                let tip = SharedString::from(tip);
                                r.child(
                                    div()
                                        .id(id("row-update", root))
                                        .flex_none()
                                        .child(tag)
                                        .tooltip(move |window, cx| {
                                            gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                                        }),
                                )
                            }),
                    )
                    .child(
                        // The whole width, so "NeoForge 21.1.x" fits; a
                        // tooltip has it all when it doesn't.
                        widgets::caption(sub, cx)
                            .id(id("row-sub", root))
                            .text_xs()
                            .min_w_0()
                            .truncate()
                            .tooltip(move |window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(sub_tip.clone()).build(window, cx)
                            }),
                    ),
            )
            .context_menu(move |menu, _, cx| instance_menu(menu, &menu_view, &menu_root, false, cx))
            .into_any_element()
    }

    fn duplicate(&mut self, root: &Path, cx: &mut Context<Self>) {
        let store = self.store.read(cx);
        if let Some(full) = store.instance(root).cloned() {
            // Another launcher's instance is copied into Hopper's folder.
            if !full.source.is_empty() {
                let dest = PathBuf::from(&store.settings().instances_root);
                store.spawn_job(format!("Copying {} into Hopper", full.name), move |job| {
                    Ok(Msg::InstanceChanged(hopper::instance_ops::copy_into(&full, &dest, job)?))
                });
                return;
            }
            store.spawn_job(format!("Duplicating {}", full.name), move |job| {
                Ok(Msg::InstanceChanged(hopper::instance_ops::duplicate(
                    &full, job,
                )?))
            });
        }
    }

    fn repair(&mut self, root: &Path, cx: &mut Context<Self>) {
        let store = self.store.read(cx);
        if let Some(full) = store.instance(root).cloned() {
            store.spawn_instance_job(&full.root.clone(), format!("Repairing {}", full.name), move |job| {
                let n = hopper::instance_ops::repair(&full, job)?;
                job.detail(if n == 0 {
                    "Nothing was missing".into()
                } else {
                    format!("Restored {n} files")
                });
                Ok(Msg::InstanceChanged(full.root.clone()))
            });
        }
    }

    // ---- detail --------------------------------------------------------

    /// "N files to download by hand", with the list and a retry.
    /// With server tools off, once per instance when there's a sign the
    /// user runs a server for it: an offer to set them up.
    fn server_hint(&self, inst: &Instance, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.store.read(cx).server_hint(inst) {
            return None;
        }
        let (r1, r2, r3) = (inst.root.clone(), inst.root.clone(), inst.root.clone());
        Some(
            widgets::notice_bar(cx.theme().info, IconName::Server, cx)
                .mx_6()
                .mt_3()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .child("Running a server for this pack? Hopper can host it, keep its mods in step and start it from here."),
                )
                .child(
                    Button::new("server-hint-go")
                        .small()
                        .outline()
                        .label("Set it up")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.store.update(cx, |s, cx| s.set_server_tools(true, cx));
                            this.select(r1.clone(), cx);
                            this.set_tab(InstTab::Server, cx);
                        })),
                )
                .child(
                    Button::new("server-hint-later")
                        .small()
                        .ghost()
                        .label("Not now")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.store.update(cx, |s, cx| s.dismiss_server_hint(&r2, cx))
                        })),
                )
                .child(
                    Button::new("server-hint-close")
                        .small()
                        .ghost()
                        .icon(IconName::X)
                        .tooltip("Don't show this again for this pack")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.store.update(cx, |s, cx| s.dismiss_server_hint(&r3, cx))
                        })),
                )
                .into_any_element(),
        )
    }

    fn pending_banner(&self, inst: &Instance, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pending = self.store.read(cx).pending(&inst.root).to_vec();
        if pending.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let (root, n) = (inst.root.clone(), pending.len());
        let list = pending.clone();
        Some(
            h_flex()
                .mx_6()
                .mt_3()
                .px_4()
                .py_2p5()
                .gap_3()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.warning.opacity(0.5))
                .bg(theme.warning.opacity(0.08))
                .child(Icon::new(IconName::TriangleAlert).small().text_color(theme.warning))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().font_weight(FontWeight::MEDIUM).truncate().child(if n == 1 {
                            format!("{} to download in your browser", pending[0].name)
                        } else {
                            format!("{n} files to download in your browser")
                        }))
                        .child(widgets::caption(
                            "Their authors only allow downloads from the CurseForge website. Download opens each page in a small window that saves the file and closes; files you download yourself are picked up from Downloads too.",
                            cx,
                        )),
                )
                .child({
                    let urls: Vec<String> = pending.iter().map(|p| p.url.clone()).filter(|u| !u.is_empty()).collect();
                    let store = self.store.clone();
                    Button::new("pending-open-all")
                        .small()
                        .primary()
                        .icon(IconName::Download)
                        .label(if urls.len() == 1 { "Download".to_string() } else { format!("Download {}", urls.len()) })
                        .tooltip("Opens each page in a small Hopper window that saves the file and closes")
                        .disabled(urls.is_empty())
                        .on_click(move |_, _, cx| store.read(cx).fetch_pages(urls.clone()))
                })
                .child(
                    Button::new("pending-show")
                        .small()
                        .outline()
                        .label("Show files")
                        .on_click(move |_, window, cx| {
                            let list = list.clone();
                            window.open_dialog(cx, move |d, _, cx| {
                                d.title("Download these by hand").child(
                                    v_flex().gap_2().children(list.iter().enumerate().map(|(i, p)| {
                                        let url = p.url.clone();
                                        h_flex()
                                            .gap_3()
                                            .child(
                                                v_flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .child(div().text_sm().truncate().child(p.name.clone()))
                                                    .child(widgets::caption(p.file_name.clone(), cx).text_xs()),
                                            )
                                            .child(
                                                Button::new(("pending-open", i))
                                                    .xsmall()
                                                    .outline()
                                                    .icon(IconName::ExternalLink)
                                                    .label("Open page")
                                                    .on_click(move |_, _, cx| cx.open_url(&url)),
                                            )
                                    })),
                                )
                            });
                        }),
                )
                .child({
                    let r = root.clone();
                    Button::new("pending-dismiss")
                        .small()
                        .ghost()
                        .icon(IconName::Close)
                        .tooltip("Forget these; don't wait for them")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let r = r.clone();
                            this.store.update(cx, |s, cx| {
                                s.forget_pending(&r);
                                cx.notify();
                            });
                        }))
                })
                .child(
                    Button::new("pending-check")
                        .small()
                        .ghost()
                        .label("Check now")
                        .tooltip("Hopper checks Downloads every few seconds; this re-runs the pack's install too")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.store.read(cx).finish_pack(&root)
                        })),
                )
                .into_any_element(),
        )
    }

    fn header(&self, inst: &Instance, cx: &mut Context<Self>) -> AnyElement {
        let store = self.store.read(cx);
        let playing = store.running(&inst.root).is_some();
        let icon = inst
            .icon
            .as_deref()
            .and_then(|u| store.image(u))
            .map(Path::to_path_buf);
        let root = inst.root.clone();
        let (r_play, r_folder, r_more) = (root.clone(), root.clone(), root.clone());
        // Running or being changed by a job: hold off other changes.
        let hold = store.hold_reason(&root);
        let busy = store.is_busy(&root);
        let starting = store.is_launching(&root);
        let primary = cx.theme().primary;
        let pack_update = store.pack_update(inst).cloned().map(|f| {
            let (r1, r2, f1, f2) = (root.clone(), root.clone(), f.clone(), f.clone());
            h_flex()
                .gap_1()
                .child(widgets::split(
                    // Outline: Play is the one filled button. The icon keeps
                    // the update color.
                    widgets::split_lead(Button::new("update-pack")
                        .outline()
                        .icon(Icon::new(IconName::Download).text_color(primary))
                        .label(format!("Update pack to {}", f.display_name))
                        .disabled(hold.is_some())
                        .when_some(hold, |b, why| b.tooltip(why))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            crate::pack_update::open(
                                this.store.clone(),
                                r1.clone(),
                                f1.clone(),
                                window,
                                cx,
                            )
                        }))),
                    "pack-changes",
                    widgets::Tint::Outline,
                    gpui_kit::component::Size::Medium,
                    "What changed in the pack",
                    {
                        let store = self.store.clone();
                        move |_, window, cx| {
                            let pid = store
                                .read(cx)
                                .instance(&r2)
                                .and_then(|i| i.pack.as_ref().map(|p| p.project_id))
                                .unwrap_or(0);
                            crate::versions::open(store.clone(), r2.clone(), pid, true, Some(f2.clone()), window, cx)
                        }
                    },
                    cx,
                ))
        });
        // The paired server: its name and a state dot (green running, grey
        // stopped, red when its last start crashed). The name opens the
        // Server tab, ▾ the server's actions. Unpaired: "Pair a server…".
        let paired = store.server_of(&inst.root).map(|s| {
            let players = store
                .panel_stats(&s.root)
                .filter(|p| p.running && !p.starting && p.max > 0)
                .map(|p| format!(", {}/{} players", p.online, p.max));
            (s.root.clone(), s.name.clone(), players.unwrap_or_default())
        });
        let can_pair = paired.is_none() && !store.servers().is_empty();
        let on_server_tab = self.tab == InstTab::Server;
        // Server tools off: no server button (the ⋯ menu still has them).
        let paired = paired.filter(|_| store.server_tools());
        let can_pair = can_pair && store.server_tools();
        let sync_to: Option<AnyElement> = match paired {
            Some((sroot, name, players)) => {
                let (dot, word, reason) = self.server_state(&sroot, cx);
                let tip = match (&reason, word) {
                    (Some(r), "unreachable") => r.clone(),
                    (Some(r), _) => format!("{name}: {word}, {r}. Opens the Server tab."),
                    (None, "") => format!("{name}: its mods compared with this instance's (the Server tab)"),
                    (None, w) => format!("{name} is {w}{players}. Opens the Server tab."),
                };
                let lead = Button::new("sync-to")
                    .outline()
                    .selected(on_server_tab)
                    .child(h_flex().gap_2().items_center().child(widgets::status_dot(dot)).child(name))
                    .tooltip(tip)
                    .on_click(cx.listener(|this, _, _, cx| this.set_tab(InstTab::Server, cx)));
                let view = cx.entity();
                let menu = Button::new("server-menu")
                    .outline()
                    .icon(IconName::ChevronDown)
                    .tooltip("Server actions")
                    .rounded_tl(px(0.))
                    .rounded_bl(px(0.))
                    .border_l_0()
                    // Right edges aligned, under the button.
                    .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, cx| {
                        server_menu(menu, &view, &sroot, cx)
                    });
                Some(
                    h_flex()
                        .flex_none()
                        .child(widgets::split_lead(lead))
                        .child(menu)
                        .into_any_element(),
                )
            }
            None if can_pair => Some(
                Button::new("pair-server")
                    .outline()
                    .selected(on_server_tab)
                    .icon(IconName::Server)
                    .label("Pair a server…")
                    .tooltip("Pick the server that runs this pack, to compare and sync their mods")
                    .on_click(cx.listener(|this, _, _, cx| this.set_tab(InstTab::Server, cx)))
                    .into_any_element(),
            ),
            None => None,
        };
        let view = cx.entity();

        let play = if playing {
            Button::new("stop")
                .danger()
                .icon(IconName::CircleStop)
                .label("Stop")
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.confirm_stop(r_play.clone(), window, cx)
                }))
        } else {
            Button::new("play")
                .success()
                .icon(IconName::Play)
                .label(if starting { "Starting…" } else if busy { "Working…" } else { "Play" })
                .loading(busy || starting)
                .disabled(busy || starting)
                .when(busy, |b| b.tooltip("A task is changing this instance"))
                .on_click(cx.listener(move |this, _, window, cx| this.play(&r_play, window, cx)))
        };

        let theme = cx.theme();
        let play_color = if playing { theme.danger } else { theme.success };
        // First in the row on every tab, so it never moves.
        let play_small = Some(widgets::glow(play.min_w_24(), play_color, cx));
        let play_big: Option<Div> = None;
        let site = self.pack_facts(inst, cx).map(|(_, _, _, url, mr)| (url, mr));
        let actions = h_flex()
                            .flex_none()
                            .flex_nowrap()
                            .gap_2()
                            .children(play_small)
                            .children(pack_update)
                            .children(sync_to)
                            .child(
                                Button::new("folder")
                                    .outline()
                                    .icon(IconName::FolderOpen)
                                    .label("Folder")
                                    .on_click(move |_, _, _| store::open_path(&r_folder)),
                            )
                            .when_some(site, |r, (url, mr)| {
                                r.child(
                                    Button::new("pack-site")
                                        .outline()
                                        .icon(IconName::ExternalLink)
                                        .text_color(widgets::site_color(mr))
                                        .tooltip(if mr { "Open on Modrinth" } else { "Open on CurseForge" })
                                        .on_click(move |_, _, cx| cx.open_url(&url)),
                                )
                            })
                            .child(
                                Button::new("more")
                                    .outline()
                                    .icon(IconName::Ellipsis)
                                    .tooltip("More")
                                    // Right-aligned to the button, off the window edge.
                                    .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, cx| {
                                        instance_menu(menu, &view, &r_more, true, cx)
                                    }),
                            );
        self.hero(inst, icon, playing, actions, play_big, cx)
    }

    /// On Overview, the pack's page facts the banner shows in place of the
    /// page's own header: (author, downloads, summary, page url, Modrinth).
    fn pack_facts(&self, inst: &Instance, cx: &App) -> Option<(String, String, String, String, bool)> {
        if self.tab != InstTab::Overview {
            return None;
        }
        let p = inst.pack.as_ref().filter(|p| p.project_id != 0)?;
        match self.store.read(cx).pack_page(p.project_id)? {
            store::PackPage::Ready(m, ..) => Some((
                m.author().to_string(),
                widgets::downloads_label(m.download_count),
                m.summary.clone(),
                m.page(),
                m.modrinth.is_some(),
            )),
            _ => None,
        }
    }

    /// The instance's header: its art large over a blurred copy of itself
    /// (made once, off the UI thread), state pills, name, actions and a
    /// few figures. The same on every tab.
    fn hero(
        &self,
        inst: &Instance,
        icon: Option<PathBuf>,
        playing: bool,
        actions: Div,
        play_big: Option<Div>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let page = self.pack_facts(inst, cx);
        let store = self.store.read(cx);
        let theme = cx.theme();
        let glass = widgets::glass(cx);
        let dark = theme.mode.is_dark();

        // State pills.
        let mut pills: Vec<AnyElement> = Vec::new();
        if let Some(r) = store.running(&inst.root) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let mins = now.saturating_sub(r.started) / 60;
            let up = if mins >= 60 {
                format!("{}h {}m", mins / 60, mins % 60)
            } else {
                format!("{mins}m")
            };
            pills.push(widgets::pill(format!("Running · {up}"), theme.success, true).into_any_element());
        } else if playing {
            pills.push(widgets::pill("Running", theme.success, true).into_any_element());
        }
        if store.is_busy(&inst.root) {
            pills.push(widgets::pill("Working", theme.warning, true).into_any_element());
        }
        // The update pills open what they're about.
        if let Some(f) = store.pack_update(inst).cloned() {
            let root = inst.root.clone();
            let hover = theme.primary.opacity(0.2);
            pills.push(
                widgets::pill("Pack update", theme.primary, false)
                    .id("pill-pack-update")
                    .clickable(cx)
                    .hover(move |s| s.bg(hover))
                    .tooltip(|window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new("Review the pack update").build(window, cx)
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        crate::pack_update::open(this.store.clone(), root.clone(), f.clone(), window, cx)
                    }))
                    .into_any_element(),
            );
        }
        if let Some(store::Updates::Ready(u)) = store.updates(&inst.root)
            && !u.is_empty()
        {
            let n = u.len();
            let hover = theme.info.opacity(0.2);
            pills.push(
                widgets::pill(format!("{n} mod update{}", if n == 1 { "" } else { "s" }), theme.info, false)
                    .id("pill-mod-updates")
                    .clickable(cx)
                    .hover(move |s| s.bg(hover))
                    .tooltip(|window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new("Show them in Mods").build(window, cx)
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_tab(InstTab::Mods, cx);
                        this.addons.update(cx, |a, cx| a.show_updates(cx));
                    }))
                    .into_any_element(),
            );
        }
        // Another launcher's instance says whose it is.
        if let Some(k) = hopper::launchers::Kind::from_key(&inst.source) {
            pills.push(widgets::pill(k.name(), theme.accent_foreground, false).into_any_element());
        }
        pills.push(
            match &inst.pack {
                Some(p) => {
                    let mr = hopper::modrinth::is_mr(p.project_id);
                    widgets::pill(if mr { "Modrinth" } else { "CurseForge" }, widgets::site_color(mr), false)
                }
                None => widgets::pill("Custom", theme.muted_foreground, false),
            }
            .into_any_element(),
        );

        // Minecraft, loader, pack version.
        // Each in its own color (Minecraft green, loader yellow, version
        // blue), as Browse colors a project's facts.
        let fact = |icon: IconName, text: String| {
            let color = match icon {
                IconName::Box => theme.success,
                IconName::Blocks => theme.warning,
                _ => theme.info,
            };
            h_flex()
                .flex_none()
                .gap_1p5()
                .child(Icon::new(icon).xsmall().text_color(color))
                .child(text)
        };
        let facts = h_flex()
            .gap_4()
            .min_w_0()
            .overflow_hidden()
            .text_sm()
            .text_color(theme.foreground.opacity(0.8))
            .child(fact(IconName::Box, format!("Minecraft {}", inst.game_version)))
            .child({
                let f = fact(IconName::Blocks, store::loader_label(&inst.loader));
                if inst.loader.is_empty() {
                    f.into_any_element()
                } else {
                    // Opens the loader's builds and changelog over this tab.
                    let root = inst.root.clone();
                    let fg = theme.foreground;
                    f.id("hero-loader")
                        .clickable(cx)
                        .hover(move |s| s.text_color(fg))
                        .tooltip(|window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new("Loader versions and changelog")
                                .build(window, cx)
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            crate::inst_settings::open_loader_versions(this.store.clone(), root.clone(), None, window, cx);
                        }))
                        .into_any_element()
                }
            })
            .when_some(inst.pack.as_ref().filter(|p| !p.version.is_empty()), |r, p| {
                let v = if p.version.starts_with(|c: char| c.is_ascii_digit()) {
                    format!("v{}", p.version)
                } else {
                    p.version.clone()
                };
                // Opens the pack's versions and changelogs (Pack versions).
                let (store, root, pid) = (self.store.clone(), inst.root.clone(), p.project_id);
                let upd = self.store.read(cx).pack_update(inst).cloned();
                let fg = theme.foreground;
                r.child(
                    fact(IconName::Tag, v)
                        .id("hero-pack-version")
                        .when(pid != 0, |d| {
                            d.clickable(cx)
                                .hover(move |s| s.text_color(fg))
                                .tooltip(|window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new("Pack versions and changelogs").build(window, cx)
                                })
                                .on_click(move |_, window, cx| {
                                    crate::versions::open(store.clone(), root.clone(), pid, true, upd.clone(), window, cx)
                                })
                        }),
                )
            })
            .when_some(page.clone(), |r, (author, downloads, ..)| {
                let soft = |icon: IconName, text: String| {
                    h_flex()
                        .flex_none()
                        .gap_1p5()
                        .text_color(theme.muted_foreground)
                        .child(Icon::new(icon).xsmall())
                        .child(text)
                };
                r.child(soft(IconName::User, format!("by {author}")))
                    .child(soft(IconName::Download, downloads))
            });
        let summary = page
            .map(|(_, _, s, ..)| s)
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .truncate()
                    .child(s)
            });
        let taller = summary.is_some();

        // The art: large, rounded, lifted off the backdrop.
        let art = div()
            .flex_none()
            .rounded(rems(1.))
            .overflow_hidden()
            .bg(theme.muted)
            .border_1()
            .border_color(white().opacity(0.12))
            .shadow(vec![
                BoxShadow::new(px(0.), px(14.), black().opacity(if dark { 0.5 } else { 0.25 }))
                    .blur_radius(px(32.)),
            ])
            .map(|d| match icon.clone() {
                Some(p) => d.child(img(p).size_full().object_fit(ObjectFit::Cover)),
                None => d
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(IconName::Package).large()),
            });
        let art = art.size(rems(6.5));

        // The banner: pills over the title, the facts under it; on the right
        // the figures in one box over the actions.
        let content = {
            let mods = inst.addons.iter().filter(|a| in_class(a, cf::CLASS_MODS)).count();
            let (time, time_label) = match store::played(inst.play_seconds) {
                Some(t) => (t, "Played"),
                None => (
                    inst.last_played
                        .as_deref()
                        .map(hopper::time::relative)
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "Never".into()),
                    "Last played",
                ),
            };
            let size = match self.sizes.get(&inst.root) {
                Some(Some(n)) => Some(hopper::instance_ops::human_bytes(*n)),
                _ => None,
            };
            let room = match self.detail_w {
                w if w >= 80.0 => 3,
                w if w >= 64.0 => 2,
                _ => 0,
            };
            let cells: Vec<(String, &str)> = [Some((time, time_label)), Some((mods.to_string(), "Mods")), size.map(|s| (s, "Size"))]
                .into_iter()
                .flatten()
                .take(room)
                .collect();
            let line = white().opacity(0.08);
            let stats = (!cells.is_empty()).then(|| {
                h_flex()
                    .flex_none()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(line)
                    .bg(black().opacity(if dark { 0.3 } else { 0.08 }))
                    .children(cells.into_iter().enumerate().map(|(i, (v, l))| {
                        v_flex()
                            .min_w(rems(5.75))
                            .px_4()
                            .py_1p5()
                            .when(i > 0, |c| c.border_l_1().border_color(line))
                            .child(div().text_base().font_weight(FontWeight::BOLD).child(v))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.muted_foreground)
                                    .child(l.to_uppercase()),
                            )
                    }))
            });
            h_flex()
                .absolute()
                .inset_0()
                .items_center()
                .gap_5()
                .px_6()
                .child(art)
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1p5()
                        .child(h_flex().gap_1p5().flex_wrap().children(pills))
                        .child(
                            div()
                                .text_size(rems(1.65))
                                .font_weight(FontWeight::EXTRA_BOLD)
                                .line_height(relative(1.15))
                                .truncate()
                                .child(inst.name.clone()),
                        )
                        .child(facts)
                        .children(summary),
                )
                .child(
                    v_flex()
                        .flex_none()
                        .items_end()
                        .gap_3()
                        .children(stats)
                        .child(actions),
                )
                .children(play_big)
        };

        // The backdrop: the art blurred and stretched, faded into the page
        // (or, on glass, into a tint that keeps text readable).
        let blur = icon.as_ref().and_then(|p| self.blurs.get(p).cloned().flatten());
        let base = if glass {
            if dark { black() } else { white() }
        } else {
            theme.background
        };
        let (top_a, bottom_a, side_a) = if glass { (0.05, 0.6, 0.5) } else { (0.15, 1.0, 0.85) };
        let backdrop = div()
            .absolute()
            .inset_0()
            .map(|d| match blur {
                Some(p) => d.child(
                    img(p)
                        .size_full()
                        .object_fit(ObjectFit::Cover)
                        .opacity(if dark { 0.75 } else { 0.55 }),
                ),
                None => d.bg(linear_gradient(
                    135.,
                    linear_color_stop(theme.primary.opacity(0.22), 0.),
                    linear_color_stop(theme.primary.opacity(0.), 1.),
                )),
            })
            .child(div().absolute().inset_0().bg(linear_gradient(
                180.,
                linear_color_stop(base.opacity(top_a), 0.),
                linear_color_stop(base.opacity(bottom_a), 1.),
            )))
            .child(div().absolute().inset_0().bg(linear_gradient(
                90.,
                linear_color_stop(base.opacity(side_a), 0.),
                linear_color_stop(base.opacity(0.), 0.7),
            )));

        let hero = div()
            .relative()
            .flex_none()
            .overflow_hidden()
            .map(|d| {
                if glass {
                    // A floating translucent panel, lit along its top edge.
                    d.mx_3()
                        .mt_3()
                        .rounded(rems(1.))
                        .border_1()
                        .border_color(white().opacity(if dark { 0.1 } else { 0.5 }))
                        .bg(theme.foreground.opacity(0.03))
                } else {
                    d.border_b_1().border_color(theme.border)
                }
            })
            .child(backdrop)
            .child(content);
        hero.h(rems(if taller { HERO + 1.5 } else { HERO })).into_any_element()
    }

    /// The tab bar: content tabs with an underline that slides from the
    /// last tab to the picked one, then Peers and Settings as icon
    /// buttons. Tabs that don't fit the window go in a "More" menu. Tabs
    /// report where they were laid out; the underline is placed from that
    /// (one frame late the very first time).
    fn tabs(&self, inst: &Instance, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let slot = |tab: InstTab| if tab.is_pack() { InstTab::ResourcePacks } else { tab };
        // Which bar tabs fit: a rough width per tab from its label, against
        // the room the window leaves beside the instance list.
        let settings = self.store.read(cx).settings();
        let side = if settings.sidebar_collapsed { RAIL } else { settings.sidebar_width.clamp(SIDEBAR_MIN, SIDEBAR_MAX) };
        let room = self.view_w - self.rem * (side + 3.5) - px(MANAGE.len() as f32 * 34. + 24.);
        let want = |t: InstTab| px(46. + Self::bar_label(t).len() as f32 * 7.6 + if t == InstTab::Mods { 30. } else { 0. });
        let mut used = px(0.);
        let mut shown: Vec<InstTab> = Vec::new();
        let mut folded: Vec<InstTab> = Vec::new();
        for (i, t) in BAR.iter().enumerate() {
            let more = if i + 1 < BAR.len() { px(84.) } else { px(0.) };
            if folded.is_empty() && used + want(*t) + more <= room {
                used += want(*t);
                shown.push(*t);
            } else {
                folded.push(*t);
            }
        }
        // The open tab always shows.
        if let Some(i) = folded.iter().position(|t| *t == slot(self.tab)) {
            let t = folded.remove(i);
            if let Some(last) = shown.pop() {
                folded.insert(0, last);
            }
            shown.push(t);
        }
        let pos = |tab: InstTab| shown.iter().position(|t| *t == slot(tab));
        let (to, from) = (pos(self.tab), pos(self.tab_from));
        let items = shown.iter().enumerate().map(|(i, t)| {
            let active = *t == slot(self.tab);
            let count = t
                .class()
                .filter(|_| !t.is_pack())
                .map(|c| inst.addons.iter().filter(|a| in_class(a, c)).count());
            let tab = *t;
            h_flex()
                .id(("inst-tab", i))
                .flex_none()
                .gap_2()
                .px_2p5()
                .py_3()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .clickable(cx)
                .text_color(if active { theme.foreground } else { theme.muted_foreground })
                .hover(|s| s.text_color(theme.foreground))
                .child(
                    Icon::new(Self::bar_icon(tab))
                        .small()
                        .when(active, |i| i.text_color(theme.primary)),
                )
                .child(Self::bar_label(tab))
                .when_some(count, |d, n| d.child(div().text_xs().text_color(theme.muted_foreground).child(n.to_string())))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let to = if tab.is_pack() { this.last_pack } else { tab };
                    this.set_tab(to, cx)
                }))
        });
        let (bounds, entity) = (self.tab_bounds.clone(), cx.entity_id());
        let strip = h_flex()
            .gap_1()
            .children(items)
            .on_children_prepainted(move |b, _, cx| {
                // Only the tabs' places relative to each other matter, so
                // the strip moving (scrolling, the header animating) doesn't
                // ask for another render.
                let first = b.first().map_or(px(0.), |f| f.left());
                let rel: Vec<(Pixels, Pixels)> =
                    b.iter().map(|b| (b.left() - first, b.size.width)).collect();
                if *bounds.borrow() != rel {
                    *bounds.borrow_mut() = rel;
                    cx.notify(entity);
                }
            });
        let laid = self.tab_bounds.borrow();
        let place = |i: Option<usize>| i.and_then(|i| laid.get(i).copied());
        let underline = place(to).map(|(left, width)| {
            let bar = div()
                .absolute()
                .bottom_0()
                .h(px(2.))
                .rounded_full()
                .bg(theme.primary)
                .shadow(vec![
                    BoxShadow::new(px(0.), px(0.), theme.primary.opacity(0.6)).blur_radius(px(8.)),
                ]);
            match place(from).filter(|_| self.moving() && from != to) {
                Some((l0, w0)) => bar
                    .with_animation(
                        ElementId::Name(format!("tab-ul:{}", self.tab_serial).into()),
                        Animation::new(MOTION).with_easing(ease_out_quint()),
                        move |el, t| el.left(l0 + (left - l0) * t).w(w0 + (width - w0) * t),
                    )
                    .into_any_element(),
                None => bar.left(left).w(width).into_any_element(),
            }
        });
        let more = (!folded.is_empty()).then(|| {
            let view = cx.entity();
            let list = folded.clone();
            Button::new("inst-tabs-more")
                .ghost()
                .small()
                .label(format!("More ({})", folded.len()))
                .dropdown_caret(true)
                // Right edges aligned, so it opens inside the window.
                .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                    list.iter().fold(menu, |m, t| {
                        let (view, t) = (view.clone(), *t);
                        m.item(PopupMenuItem::new(Self::bar_label(t)).icon(Self::bar_icon(t)).on_click(move |_, _, cx| {
                            view.update(cx, |this, cx| {
                                let to = if t.is_pack() { this.last_pack } else { t };
                                this.set_tab(to, cx)
                            });
                        }))
                    })
                })
        });
        // Peers is a server tool: hidden with them off (unless it's open).
        let tools = self.store.read(cx).server_tools();
        let manage = MANAGE.iter().filter(|t| tools || **t != InstTab::Peers || self.tab == InstTab::Peers).map(|t| {
            let (tab, active) = (*t, *t == self.tab);
            div()
                .id(SharedString::from(format!("inst-manage:{}", t.key())))
                .size(rems(2.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(theme.radius)
                .clickable(cx)
                .when(active, |d| d.bg(theme.primary.opacity(widgets::alpha::SELECTED)))
                .when(!active, |d| d.hover(|s| widgets::row_hover(s, cx)))
                .child(Icon::new(t.icon()).small().text_color(if active { theme.primary } else { theme.muted_foreground }))
                .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tab.label()).build(w, cx))
                .on_click(cx.listener(move |this, _, _, cx| this.set_tab(tab, cx)))
        });
        // Backups: snapshots to undo an update, and what was removed.
        let backups = {
            let n = self.snapshot_count(&inst.root);
            let (store, root) = (self.store.clone(), inst.root.clone());
            let muted = theme.muted_foreground;
            div()
                .id("inst-backups")
                .relative()
                .size(rems(2.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(theme.radius)
                .clickable(cx)
                .hover(|s| widgets::row_hover(s, cx))
                .child(Icon::new(IconName::Archive).small().text_color(muted))
                .when(n > 0, |d| {
                    d.child(
                        div()
                            .absolute()
                            .top(px(-2.))
                            .right(px(-4.))
                            .px_1()
                            .rounded_full()
                            .bg(theme.secondary)
                            .border_1()
                            .border_color(theme.border)
                            .text_size(rems(0.625))
                            .font_weight(FontWeight::BOLD)
                            .text_color(muted)
                            .child(n.to_string()),
                    )
                })
                .tooltip(|w, cx| {
                    gpui_kit::component::tooltip::Tooltip::new("Snapshots and backups: undo an update or a removal").build(w, cx)
                })
                .on_click(move |_, window, cx| crate::backups_view::open(store.clone(), root.clone(), window, cx))
        };
        div()
            .flex_none()
            .px_6()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .id("inst-tabs")
                            .flex_1()
                            .min_w_0()
                            .overflow_x_scroll()
                            .child(div().relative().child(strip).children(underline)),
                    )
                    .children(more)
                    .child(div().w(px(1.)).h(rems(1.25)).bg(theme.border))
                    .child(h_flex().gap_0p5().child(backups).children(manage)),
            )
            .into_any_element()
    }

    /// `root`'s snapshot count, cached for a few seconds (the tab row draws
    /// often).
    fn snapshot_count(&self, root: &Path) -> usize {
        let mut c = self.snap_count.borrow_mut();
        if let Some((r, t, n)) = c.as_ref()
            && r == root
            && t.elapsed() < std::time::Duration::from_secs(5)
        {
            return *n;
        }
        let n = hopper::snapshots::count(root);
        *c = Some((root.to_path_buf(), std::time::Instant::now(), n));
        n
    }

    /// A bar tab's name: "Packs" for the pack tabs.
    fn bar_label(t: InstTab) -> &'static str {
        if t.is_pack() { "Packs" } else { t.label() }
    }

    fn bar_icon(t: InstTab) -> IconName {
        if t.is_pack() { IconName::Layers } else { t.icon() }
    }

    /// Resource packs · Shaders · Data packs, above a pack tab's list.
    fn pack_tabs(&self, inst: &Instance, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.tab.is_pack() {
            return None;
        }
        // What's in the folder (hand-added packs too). Data packs live in
        // several places: no one count.
        let count = |t: InstTab| match t.class().filter(|c| *c != cf::CLASS_DATAPACKS) {
            Some(c) => {
                let n: usize = hopper::packs::locations(inst, c).iter().filter(|l| l.key == "instance").map(|l| l.count).sum();
                format!(" ({n})")
            }
            None => String::new(),
        };
        let ix = PACKS.iter().position(|t| *t == self.tab).unwrap_or(0);
        Some(
            div()
                .flex_none()
                .px_6()
                .pt_2()
                .child(
                    gpui_kit::component::tab::TabBar::new("pack-tabs")
                        .underline()
                        .small()
                        .selected_index(ix)
                        .children(PACKS.iter().map(|t| gpui_kit::component::tab::Tab::new().label(format!("{}{}", t.label(), count(*t)))))
                        .on_click(cx.listener(|this, ix: &usize, _, cx| this.set_tab(PACKS[*ix], cx))),
                )
                .into_any_element(),
        )
    }

    /// Overview: the page of the modpack the instance came from, from
    /// CurseForge or Modrinth.
    fn overview_body(&self, inst: &Instance, cx: &mut Context<Self>) -> AnyElement {
        let pack_id = inst.pack.as_ref().map_or(0, |p| p.project_id);
        if pack_id != 0 {
            self.pack_desc.update(cx, |d, _| d.show_pack(pack_id));
        }
        let store = self.store.read(cx);
        let body = match inst.pack.as_ref().filter(|p| p.project_id != 0) {
            Some(p) => {
                let page = store.pack_page(p.project_id);
                let icon = match page {
                    Some(store::PackPage::Ready(m, ..)) => m.icon().and_then(|u| store.image(u)),
                    _ => None,
                };
                widgets::pack_page(page, icon, &self.pack_desc, false, cx)
            }
            None => widgets::caption("This instance wasn't installed from a modpack.", cx).into_any_element(),
        };
        div()
            .id("overview")
            .size_full()
            .px_6()
            .pt_2()
            .pb_6()
            .child(body)
            .overflow_y_scrollbar()
            .into_any_element()
    }

    fn tab_body(&self, inst: &Instance, cx: &mut Context<Self>) -> AnyElement {
        match self.tab.class() {
            Some(_) => cached(self.addons.clone()),
            None if self.tab == InstTab::Overview => self.overview_body(inst, cx),
            None if self.tab == InstTab::Server => self.server_tab(cx),
            None if self.tab == InstTab::Worlds => cached(self.worlds.clone()),
            None if self.tab == InstTab::Screenshots => cached(self.shots.clone()),
            None if self.tab == InstTab::Settings => cached(self.settings.clone()),
            None if self.tab == InstTab::Logs => cached(self.logs.clone()),
            None if self.tab == InstTab::Performance => cached(self.perf.clone()),
            None if self.tab == InstTab::Peers => cached(self.peers.clone()),
            None => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .child(
                    Icon::new(self.tab.icon())
                        .large()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(widgets::caption(
                    format!("{} is moving to the new UI next.", self.tab.label()),
                    cx,
                ))
                .into_any_element(),
        }
    }

    /// The paired rig server compared in place, or a picker to pair one.
    fn server_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(root) = self.selected.clone() else {
            return div().into_any_element();
        };
        let store = self.store.read(cx);
        if store.server_of(&root).is_some() {
            return cached(self.server.clone());
        }
        let servers: Vec<(PathBuf, String)> = store
            .servers()
            .iter()
            .map(|s| (s.root.clone(), s.name.clone()))
            .collect();
        let items = servers.iter().map(|(_, n)| (n.clone(), false)).collect();
        let no_servers = servers.is_empty();
        let (store, view) = (self.store.clone(), cx.entity());
        let pick = std::rc::Rc::new(move |ix: usize, _: &mut Window, cx: &mut App| {
            if let Some((s, _)) = servers.get(ix) {
                let (s, r) = (s.clone(), root.clone());
                store.update(cx, |st, cx| st.set_pairing(&s, &r, cx));
                view.update(cx, |v, cx| v.want_tab_images(cx));
            }
        });
        // No servers yet: host one from this pack instead.
        if no_servers {
            let (store, root) = (self.store.clone(), self.selected.clone());
            return v_flex()
                .size_full()
                .justify_center()
                .child(
                    widgets::empty_state(
                        IconName::HardDrive,
                        "No servers yet",
                        Some("Host one on this PC from this pack, or add your servers in Settings › Servers.".into()),
                        cx,
                    )
                    .child(
                        Button::new("server-tab-host")
                            .small()
                            .primary()
                            .icon(IconName::Plus)
                            .label("Host a server from this pack…")
                            .on_click(move |_, window, cx| {
                                store.update(cx, |s, cx| s.set_server_tools(true, cx));
                                crate::host_dialog::open(store.clone(), root.clone(), window, cx)
                            }),
                    ),
                )
                .into_any_element();
        }
        v_flex()
            .size_full()
            .justify_center()
            .child(
                widgets::empty_state(
                    IconName::HardDrive,
                    "No server paired",
                    Some("This pack isn't paired with a server yet.".into()),
                    cx,
                )
                .child(div().w_64().child(widgets::picker(
                "pair-server",
                "Pick its server".into(),
                items,
                cx.theme().font_size,
                pick,
            ))),
            )
            .into_any_element()
    }

    fn detail(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(inst) = self
            .selected
            .as_ref()
            .and_then(|r| self.store.read(cx).instance(r))
            .cloned()
        else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(widgets::caption("Pick an instance.", cx))
                .into_any_element();
        };
        // A hopper.json Hopper can't read: never written over, so ask.
        if let Some(why) = inst.unreadable.clone() {
            let root = inst.root.clone();
            let theme = cx.theme();
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .px_8()
                .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(inst.name.clone()))
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.danger)
                        .child(format!("Hopper can't read this instance's hopper.json: {why}")),
                )
                .child(widgets::caption(
                    "Its files are untouched. Start over keeps the damaged file aside (hopper.json.bad-…) and reads the instance again from CurseForge's records and its folders.",
                    cx,
                ))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("unreadable-folder")
                                .outline()
                                .icon(IconName::FolderOpen)
                                .label("Folder")
                                .on_click({
                                    let r = root.clone();
                                    move |_, _, _| crate::store::open_path(&r)
                                }),
                        )
                        .child(
                            Button::new("unreadable-reset")
                                .primary()
                                .label("Start over")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    match hopper::instances::set_aside_unreadable(&root) {
                                        Ok(_) => this.store.update(cx, |s, cx| {
                                            s.refresh();
                                            cx.notify();
                                        }),
                                        Err(e) => window.push_notification(
                                            gpui_kit::component::notification::Notification::error(format!("{e:#}")),
                                            cx,
                                        ),
                                    }
                                })),
                        ),
                )
                .into_any_element();
        }
        // The hero's blurred backdrop and the size figure, made off-thread.
        let icon = inst
            .icon
            .as_deref()
            .and_then(|u| self.store.read(cx).image(u))
            .map(Path::to_path_buf);
        if let Some(icon) = icon {
            self.want_blur(&icon, cx);
        }
        self.want_size(&inst.root, cx);
        let header = self.header(&inst, cx);
        let hint = self.server_hint(&inst, cx);
        let pending = self.pending_banner(&inst, cx);
        let tabs = self.tabs(&inst, cx);
        let pack_tabs = self.pack_tabs(&inst, cx);
        let body = self.tab_body(&inst, cx);
        let anim = format!("{}:{}", inst.root.display(), self.tab.label());
        v_flex()
            .size_full()
            .child(widgets::enter(
                id("detail", &inst.root),
                div().flex_none().child(header),
            ))
            .children(hint)
            .children(pending)
            .child(tabs)
            .children(pack_tabs)
            .child(div().flex_1().min_h_0().pt_3().child(widgets::enter(
                ElementId::Name(anim.into()),
                div().size_full().child(body),
            )))
            .into_any_element()
    }
}

/// Asks for an image and makes it the instance's icon.
fn change_icon(store: Entity<Store>, root: PathBuf, cx: &mut App) {
    let rx = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some("Use as icon".into()),
    });
    let Some(window) = cx.active_window() else { return };
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(p))) = rx.await else { return };
        let Some(p) = p.into_iter().next() else { return };
        let r = store.update(cx, |s, cx| {
            let r = s.set_icon(&root, &p);
            cx.notify();
            r
        });
        if let Err(e) = r {
            let _ = window.update(cx, |_, window, cx| {
                window.push_notification(
                    gpui_kit::component::notification::Notification::error(format!("Couldn't set the icon: {e:#}")),
                    cx,
                )
            });
        }
    })
    .detach();
}

/// Names a new group and moves the instance into it.
fn new_group_dialog(store: Entity<Store>, root: PathBuf, window: &mut Window, cx: &mut App) {
    use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
    let input = cx.new(|cx| InputState::new(window, cx).placeholder("Group name"));
    window.open_dialog(cx, move |dialog, window, _| {
        let (store, root, i2) = (store.clone(), root.clone(), input.clone());
        dialog
            .title("New group")
            .w(rems(24.).to_pixels(window.rem_size()))
            .child(Input::new(&input))
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("g-cancel").outline().label("Cancel")))
                    .child(DialogAction::new().child(Button::new("g-ok").primary().label("Create"))),
            )
            .on_ok(move |_, _, cx| {
                let name = i2.read(cx).value().trim().to_string();
                if name.is_empty() {
                    return false;
                }
                store.update(cx, |s, cx| s.set_group(&root, Some(name), cx));
                true
            })
    });
}

impl Render for LibraryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.view_w = window.viewport_size().width;
        self.rem = window.rem_size();
        // The work area's width in rem, for what the header has room for.
        let settings = self.store.read(cx).settings();
        let side = match self.drag_width {
            Some(w) if w < SIDEBAR_COLLAPSE => RAIL,
            Some(w) => w.clamp(SIDEBAR_MIN, SIDEBAR_MAX),
            None if settings.sidebar_collapsed => RAIL,
            None => settings.sidebar_width.clamp(SIDEBAR_MIN, SIDEBAR_MAX),
        };
        // A mod's project page, docked full height beside the instance.
        let peek = Some(self.addons.read(cx).peek().clone())
            .filter(|p| {
                (self.tab.class().is_some() || matches!(self.tab, InstTab::Server | InstTab::Datapacks))
                    && self.selected.is_some()
                    && p.read(cx).is_open()
            });
        let peek_w = peek.as_ref().map_or(0.0, |p| p.read(cx).panel_width(window, cx));
        self.detail_w = window.viewport_size().width / window.rem_size() - side - peek_w;
        // The table fits its columns to this; it is re-laid out whenever
        // the room changes (its bounds change), so no notify is needed.
        let (room, list_open) = (self.detail_w, side > RAIL);
        self.addons.update(cx, |a, _| {
            a.room = Some(room);
            a.list_open = list_open;
        });
        self.server.update(cx, |p, _| p.room = Some(room));
        h_flex()
            .id("library")
            .size_full()
            .on_drag_move(cx.listener(|this, e: &DragMoveEvent<SidebarResize>, window, cx| {
                let x = e.event.position.x - e.bounds.left();
                this.drag_width = Some(x / window.rem_size());
                cx.notify();
            }))
            .on_drop(cx.listener(|this, _: &SidebarResize, _, cx| this.end_resize(cx)))
            // The docked project page's edge: it tracks the drag itself;
            // re-lay out here and save the width wherever it's let go.
            .on_drag_move(cx.listener(|_, _: &DragMoveEvent<crate::browse::PanelResize>, _, cx| {
                cx.notify()
            }))
            .on_drop(cx.listener(|this, _: &crate::browse::PanelResize, _, cx| {
                let peek = this.addons.read(cx).peek().clone();
                peek.update(cx, |p, cx| p.end_resize(cx));
            }))
            .items_stretch()
            .child(self.sidebar(cx))
            .child(div().flex_1().min_w_0().h_full().child(self.detail(cx)))
            .children(peek)
    }
}

/// The header's server ▾, read when it opens: Start, or Stop and Restart,
/// where a panel is set up (they ask first as on the Server tab), then the
/// Server tab's places.
fn server_menu(menu: PopupMenu, view: &Entity<LibraryView>, root: &Path, cx: &App) -> PopupMenu {
    use hopper::panel::PanelAction;
    let control = view.read(cx).server.read(cx).control().clone();
    let ctl = control.read(cx);
    let configured = ctl.configured(cx);
    let stats = ctl.stats_of(root);
    // The panel lists this server and answered: actions can be sent.
    let ready = ctl.ready() && stats.is_some();
    let running = stats.is_some_and(|s| s.running || s.starting);
    let busy = ctl.busy().is_some();
    let pname = ctl.panel_name(cx);
    let mut menu = menu;
    if configured {
        let act = |label: &'static str, icon: IconName, a: PanelAction| {
            let c = control.clone();
            PopupMenuItem::new(label)
                .icon(icon)
                .disabled(!ready || busy)
                .on_click(move |_, window, cx| c.update(cx, |c, cx| c.request(a, window, cx)))
        };
        menu = if running {
            menu.item(act("Stop", IconName::Square, PanelAction::Stop))
                .item(act("Restart", IconName::RotateCcw, PanelAction::Restart))
        } else {
            menu.item(act("Start", IconName::Play, PanelAction::Start))
        };
        if !ready {
            menu = menu.item(PopupMenuItem::new(format!("Waiting for {pname}…")).disabled(true));
        }
        menu = menu.separator();
    }
    let go = |label: &'static str, icon: IconName, to: ServerPlace| {
        let v = view.clone();
        PopupMenuItem::new(label)
            .icon(icon)
            .on_click(move |_, _, cx| v.update(cx, |this, cx| this.open_server_tab(to, cx)))
    };
    menu = menu
        .item(go("Open server page", IconName::Server, ServerPlace::Page))
        .item(go("Sync to server…", IconName::ServerCog, ServerPlace::Sync));
    if configured {
        menu = menu.item(go("Console", IconName::SquareTerminal, ServerPlace::Console));
    }
    menu
}

/// An instance's menu, the same wherever it opens: right-click in the list
/// or on its icon in the rail, and the header's ⋯ (`header`: Play and the
/// folder have their own buttons there). Read when it opens, so it is
/// never stale.
fn instance_menu(
    menu: PopupMenu,
    view: &Entity<LibraryView>,
    root: &Path,
    header: bool,
    cx: &App,
) -> PopupMenu {
    let store = view.read(cx).store.clone();
    let s = store.read(cx);
    let Some(inst) = s.instance(root) else { return menu };
    let playing = s.running(root).is_some();
    // Another launcher's instance: deleting it here would break it there.
    let launcher = hopper::launchers::Kind::from_key(&inst.source).map(|k| k.name());
    let pack = inst.pack.as_ref().map(|p| (p.project_id, s.pack_update(inst).cloned()));
    let server_pack = inst
        .pack
        .as_ref()
        .filter(|p| p.server_pack_file_id != 0)
        .map(|p| (p.project_id, p.server_pack_file_id, p.name.clone()));
    let groups = s.groups();
    let group_now = s.settings().groups.get(root.to_string_lossy().as_ref()).cloned();
    let name = inst.name.clone();
    let (v, r) = (view.clone(), root.to_path_buf());
    // Each item takes its own copies.
    let vr = || (v.clone(), r.clone());
    let st = || store.clone();

    menu.when(!header, |m| {
        let (v1, r1) = vr();
        let r2 = r.clone();
        m.item(
            PopupMenuItem::new("Play")
                .icon(IconName::Play)
                .disabled(playing)
                .on_click(move |_, window, cx| v1.update(cx, |this, cx| this.play(&r1, window, cx))),
        )
        .item(
            PopupMenuItem::new("Open folder")
                .icon(IconName::FolderOpen)
                .on_click(move |_, _, _| store::open_path(&r2)),
        )
    })
    // The header has the Settings tab right below it.
    .when(!header, |m| {
        let (v, r) = vr();
        m.item(PopupMenuItem::new("Settings").icon(IconName::SlidersHorizontal).on_click(move |_, _, cx| {
            v.update(cx, |this, cx| {
                this.select(r.clone(), cx);
                this.set_tab(InstTab::Settings, cx);
            })
        }))
        .separator()
    })
    .when_some(pack, |m, (pid, upd)| {
        let (s, r) = (st(), r.clone());
        m.item(PopupMenuItem::new("Pack versions").icon(IconName::Clock).on_click(move |_, window, cx| {
            crate::versions::open(s.clone(), r.clone(), pid, true, upd.clone(), window, cx)
        }))
    })
    .item({
        let (s, r) = (st(), r.clone());
        PopupMenuItem::new("Export…")
            .icon(IconName::Upload)
            .on_click(move |_, window, cx| crate::dialogs::open_export(s.clone(), r.clone(), window, cx))
    })
    .item({
        let (s, r) = (st(), r.clone());
        PopupMenuItem::new("Share code…")
            .icon(IconName::Share2)
            .on_click(move |_, window, cx| crate::dialogs::open_share(s.clone(), r.clone(), window, cx))
    })
    // Always here, server tools on or off: using one turns them on.
    .separator()
    .label("Server")
    .item({
        let (s, r) = (st(), r.clone());
        // A pack with a server pack builds from it (the dialog says so).
        PopupMenuItem::new("Host a server from this pack…")
            .icon(IconName::Server)
            .on_click(move |_, window, cx| {
                s.update(cx, |s, cx| s.set_server_tools(true, cx));
                crate::host_dialog::open(s.clone(), Some(r.clone()), window, cx)
            })
    })
    .item({
        let (v, s) = vr();
        let st2 = st();
        PopupMenuItem::new("Pair with a server…")
            .icon(IconName::ArrowLeftRight)
            .on_click(move |_, _, cx| {
                st2.update(cx, |st, cx| st.set_server_tools(true, cx));
                v.update(cx, |this, cx| {
                    this.select(s.clone(), cx);
                    this.set_tab(InstTab::Server, cx);
                })
            })
    })
    .when_some(server_pack.clone(), |m, (pid, fid, name)| {
        let s = st();
        m.item(PopupMenuItem::new("Download server pack…").icon(IconName::Download).on_click(
            move |_, _, cx| {
                s.update(cx, |s, _| s.note_server_files(pid));
                crate::dialogs::save_server_pack(s.clone(), pid, fid, name.clone(), cx)
            },
        ))
    })
    .separator()
    .item({
        let (s, r) = (st(), r.clone());
        PopupMenuItem::new("Change icon…")
            .icon(IconName::Image)
            .on_click(move |_, _, cx| change_icon(s.clone(), r.clone(), cx))
    })
    .item(
        PopupMenuItem::new("Create desktop shortcut")
            .icon(IconName::ExternalLink)
            .on_click(move |_, window, cx| {
                let n = match hopper::shortcut::desktop_shortcut(&name) {
                    Ok(_) => gpui_kit::component::notification::Notification::success(
                        "Shortcut created on the desktop.",
                    ),
                    Err(e) => gpui_kit::component::notification::Notification::error(format!("{e:#}")),
                };
                window.push_notification(n, cx);
            }),
    )
    .separator()
    .label("Group")
    .map(|m| {
        groups.iter().cloned().fold(m, |m, g| {
            let (s, r, g2) = (st(), r.clone(), g.clone());
            m.item(
                PopupMenuItem::new(g.clone())
                    .checked(group_now.as_deref() == Some(g.as_str()))
                    .on_click(move |_, _, cx| s.update(cx, |s, cx| s.set_group(&r, Some(g2.clone()), cx))),
            )
        })
    })
    .item({
        let (s, r) = (st(), r.clone());
        PopupMenuItem::new("New group…")
            .icon(IconName::Plus)
            .on_click(move |_, window, cx| new_group_dialog(s.clone(), r.clone(), window, cx))
    })
    .when(group_now.is_some(), |m| {
        let (s, r) = (st(), r.clone());
        m.item(
            PopupMenuItem::new("Remove from group")
                .on_click(move |_, _, cx| s.update(cx, |s, cx| s.set_group(&r, None, cx))),
        )
    })
    .separator()
    .item({
        let (v, r) = vr();
        PopupMenuItem::new(if launcher.is_some() { "Copy into Hopper" } else { "Duplicate" })
            .icon(IconName::Copy)
            .on_click(move |_, _, cx| v.update(cx, |this, cx| this.duplicate(&r, cx)))
    })
    .item({
        let (v, r) = vr();
        PopupMenuItem::new("Repair")
            .icon(IconName::Wrench)
            .on_click(move |_, _, cx| v.update(cx, |this, cx| this.repair(&r, cx)))
    })
    .item({
        let (s, r) = (st(), r.clone());
        PopupMenuItem::new("Backups…")
            .icon(IconName::Archive)
            .on_click(move |_, window, cx| crate::backups_view::open(s.clone(), r.clone(), window, cx))
    })
    .separator()
    .item({
        let (s, r) = (st(), r.clone());
        PopupMenuItem::new(match launcher {
            Some(l) => format!("Delete it in {l}"),
            None => "Delete…".to_string(),
        })
        .icon(IconName::Trash)
        .disabled(playing || launcher.is_some())
        .on_click(move |_, window, cx| crate::dialogs::confirm_delete(s.clone(), r.clone(), window, cx))
    })
}
