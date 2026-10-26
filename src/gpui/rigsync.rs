//! A pack compared with its rig server: what differs, and pushing, pulling
//! or removing mods. Used by the Server sync page and an instance's Server tab.

use crate::widgets::Clickable as _;
use crate::drag_pick::{self, DragPick, PickDrag, PickState, Picks, Sweep};
use crate::store::{Msg, Store};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::crashscan::ServerCrash;
use hopper::instances::Instance;
use hopper::model::{DiffEntry, DiffStatus, ModInfo};
use hopper::rigsync::{self, EnvFilter, Filters, Group, Ops, RowAct, SourceFilter, row_key};
use hopper::server::{ServerInfo, Status};
use hopper::serverpack::Annotation;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;

const ROW_H: f32 = 3.5;
/// The mod name's least width in rem: other cells give way first.
const NAME_MIN: f32 = 14.0;
/// The action cell: "Update server", its changelog half, the ⋯ slot and
/// the gaps, with a little to spare.
const ACTION_W: f32 = 15.5;
/// The same with icon-only buttons (their words in the tooltip).
const ACTION_W_COMPACT: f32 = 7.5;

/// How the row's cells fit the list's width: the tags column's width
/// (none: the side shows as an icon by the name), the version cell's, and
/// the action cell's (compact: icon-only buttons).
#[derive(Clone, Copy)]
struct Fit {
    tags: Option<f32>,
    version: f32,
    compact: bool,
    action: f32,
}

impl Fit {
    /// From the list's width in rem. Only from widths known before layout,
    /// so it never flips back and forth. In order, as room runs out: the
    /// version cell narrows, the tags go, then the buttons lose their words.
    fn of(list: f32) -> Fit {
        // Row padding, tick, icon, a scrollbar, a margin, and the gaps
        // between six cells.
        let fixed = 1.5 + 1.5 + 2.0 + 1.0 + 1.0 + 0.75 * 5.0;
        let free = list - fixed;
        let version = if free - ACTION_W - 10.0 - 6.0 >= NAME_MIN { 10.0 } else { 7.0 };
        let tags = free - ACTION_W - version - NAME_MIN;
        let compact = free - ACTION_W - version < NAME_MIN;
        let action = if compact { ACTION_W_COMPACT } else { ACTION_W };
        Fit {
            tags: (tags >= 6.0).then_some(tags.min(22.0)),
            version: if compact { 6.0 } else { version },
            compact,
            action,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Mods,
    Configs,
    History,
    /// The server panel's console (only when a panel is set up).
    Console,
}

/// Which rows the list shows: the filter above it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Show {
    /// To add, updates and only on the server.
    Needs,
    Kept,
    InSync,
    All,
}

impl Show {
    const ALL: [Show; 4] = [Show::Needs, Show::Kept, Show::InSync, Show::All];

    fn label(self) -> &'static str {
        match self {
            Show::Needs => "Needs action",
            Show::Kept => "Kept off",
            Show::InSync => "In sync",
            Show::All => "All",
        }
    }

    fn has(self, g: Group) -> bool {
        match self {
            Show::Needs => matches!(g, Group::Add | Group::Update | Group::ServerOnly),
            Show::Kept => g == Group::KeptOff,
            Show::InSync => g == Group::InSync,
            Show::All => true,
        }
    }
}

/// What the panel tells its page.
pub enum SyncEvent {
    /// The last start's crash was read again (the header's status dot).
    Crash,
}

impl EventEmitter<SyncEvent> for SyncPanel {}

pub struct SyncPanel {
    store: Entity<Store>,
    /// The side column's width (inside an instance), resizable.
    side_panel: widgets::SidePanel,
    section: Section,
    configs: Entity<crate::configs::ConfigsPanel>,
    history: Entity<crate::configs::HistoryPanel>,
    /// Start / stop / restart through the server's panel, beside the header.
    control: Entity<crate::server_control::ServerControl>,
    /// Inside an instance, whose header already shows the pack.
    in_instance: bool,
    /// The Sync tab of a server's page, whose header already shows the
    /// server and its start / stop.
    in_server_page: bool,
    server: Option<PathBuf>,
    diff: Option<Result<Vec<DiffEntry>, String>>,
    note: Option<Annotation>,
    crash: Option<ServerCrash>,
    /// Global datapacks and mod configs that differ from the pack, looked
    /// up after each comparison (they sit on the same slow share).
    drift: Option<hopper::configdiff::Drift>,
    drift_open: bool,
    /// The drift the user hid: shown again once it changes.
    drift_hidden: Option<u64>,
    _drift_task: Option<Task<()>>,
    /// The NeoForge version the server's run.bat starts, read with each
    /// comparison (run.bat may sit on a network share: not per frame).
    server_nf: Option<String>,
    /// NeoForge builds for the pack's Minecraft version, newest first, for
    /// the loader picker: (Minecraft version, builds).
    nf_versions: Option<(String, Result<Vec<String>, String>)>,
    nf_loading: bool,
    /// The filter's pick; None picks Needs action when something does,
    /// else All.
    show: Option<Show>,
    filters: Filters,
    search: Entity<InputState>,
    selected: HashSet<String>,
    /// A drag across the tick boxes or row buttons, while the button is down.
    pick: PickState,
    /// The row a tick box was last pressed on, for Shift.
    pick_anchor: Option<usize>,
    /// The rows a button drag will act on when let go.
    swept: HashSet<String>,
    /// Groups showing their rows.
    open: HashSet<Group>,
    /// The rows deliberately kept off the server show in the list.
    show_kept: bool,
    /// The list as drawn, rebuilt each frame from the comparison.
    items: Vec<Item>,
    /// A bulk apply runs: every action waits for it.
    running: bool,
    /// A comparison runs.
    comparing: bool,
    /// Rows whose own action runs now, by row key: (word while busy, word
    /// when done).
    acting: HashMap<String, (&'static str, &'static str)>,
    /// Rows whose action finished, with the word to show, until they go.
    acted: HashMap<String, &'static str>,
    /// Done rows fading out now: (since, keys).
    leaving: Option<(std::time::Instant, HashSet<String>)>,
    /// A comparison that came in while rows were acting or done: shown once
    /// they've gone, so rows don't jump under the pointer.
    diff_next: Option<(Result<Vec<DiffEntry>, String>, Option<Annotation>)>,
    settle: widgets::Settle,
    _ticker: Option<Task<()>>,
    result: Option<Result<String, Vec<String>>>,
    allow_online: bool,
    /// The store's server revision this comparison is for.
    seen_rev: u64,
    scroll: UniformListScrollHandle,
    /// The work area's width in rem (set by the Library), for fitting the
    /// row's cells; None: the window's.
    pub room: Option<f32>,
    fit: Fit,
    /// The list shows one group: its header skips the count the filter
    /// already shows.
    one_group: bool,
    /// The Library's mod panel, opened by a mod's name or icon.
    peek: Option<Entity<crate::browse::BrowseView>>,
    _task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl SyncPanel {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search mods"));
        let mut subs = vec![
            cx.observe(&store, |this, _, cx| {
                // The server or the pack's mods changed elsewhere: compare again.
                let rev = this.compare_key(cx);
                if rev != this.seen_rev {
                    this.seen_rev = rev;
                    this.rescan(cx);
                }
                cx.notify();
            }),
            cx.subscribe_in(&search, window, |this, state, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.filters.text = state.read(cx).value().trim().to_string();
                    cx.notify();
                }
            }),
        ];
        let seen_rev = 0;
        let history = cx.new(|cx| crate::configs::HistoryPanel::new(window, cx));
        let control =
            cx.new(|cx| crate::server_control::ServerControl::new(store.clone(), window, cx));
        let configs = cx.new(|cx| {
            let mut c = crate::configs::ConfigsPanel::new(store.clone(), window, cx);
            c.set_control(control.clone(), cx);
            c
        });
        // The Configs tab shows how many files need syncing.
        subs.push(cx.subscribe(&configs, |_, _, _: &crate::configs::NeedsChanged, cx| cx.notify()));
        subs.push(cx.observe(&control, |_, _, cx| cx.notify()));
        subs.push(cx.subscribe(&control, |this, _, e: &crate::server_control::ControlEvent, cx| {
            if let crate::server_control::ControlEvent::Console = e {
                this.open_console(cx);
            }
        }));
        Self {
            store,
            side_panel: widgets::SidePanel::new(widgets::PanelSpec::new("sync.side", 18., 15., 28.)),
            // `--section configs|history` for scripted runs.
            section: match std::env::args()
                .skip_while(|a| a != "--section")
                .nth(1)
                .as_deref()
            {
                Some("configs") => Section::Configs,
                Some("history") => Section::History,
                _ => Section::Mods,
            },
            configs,
            history,
            control,
            in_instance: false,
            in_server_page: false,
            server: None,
            diff: None,
            note: None,
            crash: None,
            drift: None,
            drift_open: false,
            drift_hidden: None,
            _drift_task: None,
            server_nf: None,
            nf_versions: None,
            nf_loading: false,
            show: None,
            filters: Filters::default(),
            search,
            selected: HashSet::new(),
            pick: PickState::default(),
            pick_anchor: None,
            swept: HashSet::new(),
            open: [Group::Add, Group::Update, Group::ServerOnly, Group::KeptOff].into(),
            show_kept: false,
            items: Vec::new(),
            running: false,
            comparing: false,
            acting: HashMap::new(),
            acted: HashMap::new(),
            leaving: None,
            diff_next: None,
            settle: Default::default(),
            _ticker: None,
            result: None,
            allow_online: false,
            seen_rev,
            scroll: UniformListScrollHandle::new(),
            room: None,
            fit: Fit::of(80.0),
            one_group: false,
            peek: None,
            _task: None,
            _subscriptions: subs,
        }
    }

    pub fn set_in_instance(&mut self, on: bool) {
        self.in_instance = on;
    }

    /// Shown as a server page's Sync tab.
    pub fn set_in_server_page(&mut self, on: bool) {
        self.in_server_page = on;
    }

    /// The instances to pair a server with: a picker that pairs on pick.
    fn pair_picker(&self, server: &Path, label: String, current: Option<&Path>, cx: &App) -> AnyElement {
        let store = self.store.read(cx);
        let roots: Vec<PathBuf> = store.instances().iter().map(|i| i.root.clone()).collect();
        let items = store
            .instances()
            .iter()
            .map(|i| (i.name.clone(), current == Some(i.root.as_path())))
            .collect();
        let (store, server) = (self.store.clone(), server.to_path_buf());
        widgets::picker(
            "sync-pair",
            label,
            items,
            cx.theme().font_size,
            Rc::new(move |ix, _, cx| {
                if let Some(r) = roots.get(ix) {
                    let r = r.clone();
                    store.update(cx, |s, cx| s.set_pairing(&server, &r, cx));
                }
            }),
        )
        .into_any_element()
    }

    pub fn set_peek(&mut self, peek: Entity<crate::browse::BrowseView>) {
        self.peek = Some(peek);
    }

    /// Opens (or closes, when it's the open one) a mod's page beside the list.
    fn open_peek(&mut self, project: u64, cx: &mut Context<Self>) {
        let Some(root) = self.pair_header(cx).map(|(_, i)| i.root) else {
            return;
        };
        if let Some(p) = &self.peek {
            p.update(cx, |p, cx| p.peek_project(root, Some(project), cx));
        }
    }

    pub fn server(&self) -> Option<&Path> {
        self.server.as_deref()
    }

    /// The start / stop control (the instance header's server menu uses it).
    pub fn control(&self) -> &Entity<crate::server_control::ServerControl> {
        &self.control
    }

    /// Points the start / stop control at `root` without comparing, so the
    /// header's menu can act before the tab is opened.
    pub fn watch(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.control.update(cx, |c, cx| c.show(root, cx));
    }

    /// One poll of the server's state now (instance picked, window focused).
    pub fn refresh_state(&mut self, cx: &mut Context<Self>) {
        self.control.update(cx, |c, cx| c.refresh(cx));
    }

    /// The last start's crash of `root`, once this panel has compared it
    /// (Some(None): no crash).
    pub fn crash_of(&self, root: &Path) -> Option<Option<&ServerCrash>> {
        (self.server.as_deref() == Some(root) && self.diff.is_some()).then_some(self.crash.as_ref())
    }

    /// The Mods list on what needs syncing.
    pub fn open_sync(&mut self, cx: &mut Context<Self>) {
        self.set_section(Section::Mods, cx);
        self.show = Some(Show::Needs);
        self.open.extend([Group::Add, Group::Update, Group::ServerOnly]);
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// The server's console, when a panel is set up.
    pub fn open_console(&mut self, cx: &mut Context<Self>) {
        let section = if self.control.read(cx).configured(cx) { Section::Console } else { Section::Mods };
        self.set_section(section, cx);
    }

    /// The tab went out of view: the console stops, and polls slow down to
    /// the header dot's rate.
    pub fn hidden(&mut self, cx: &mut Context<Self>) {
        self.control.update(cx, |c, cx| {
            c.set_console_open(false, cx);
            c.set_active(false, cx);
        });
    }

    /// Tells Configs which mods the server won't run (client-only, or kept
    /// off for another reason), so their config files count as not needed.
    fn share_client_mods(&self, cx: &mut Context<Self>) {
        let Some(Ok(diff)) = &self.diff else { return };
        let mods: HashSet<String> = diff
            .iter()
            .filter(|d| d.server.is_none())
            .filter_map(|d| d.client.as_ref().filter(|c| c.side == "client" || d.skip_reason.is_some()))
            .flat_map(|c| [hopper::configdiff::mod_key(&c.mod_id), hopper::configdiff::mod_key(&c.mod_name)])
            .filter(|k| k.len() >= 3)
            .collect();
        self.configs.update(cx, |c, cx| c.set_client_mods(mods, cx));
    }

    fn set_section(&mut self, section: Section, cx: &mut Context<Self>) {
        self.section = section;
        let open = section == Section::Console;
        self.control.update(cx, |c, cx| c.set_console_open(open, cx));
        cx.notify();
    }

    /// The filter as it applies now.
    fn shown(&self, cx: &App) -> Show {
        self.show.unwrap_or_else(|| {
            let needs = self.whole(cx).iter().any(|g| Show::Needs.has(*g));
            if needs { Show::Needs } else { Show::All }
        })
    }

    /// Every row's group, whatever the filters (blacklisted rows left out).
    fn whole(&self, cx: &App) -> Vec<Group> {
        let blacklist = &self.store.read(cx).settings().sync_blacklist;
        let unfiltered = Filters::default();
        match &self.diff {
            Some(Ok(d)) => d.iter().filter(|d| unfiltered.passes(d, blacklist)).map(Group::of).collect(),
            _ => Vec::new(),
        }
    }

    /// Compares `root`'s server with its pack (again when it is the same, so
    /// coming back to the tab picks up mods changed meanwhile).
    pub fn show(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let r2 = root.clone();
        self.configs.update(cx, |c, cx| c.show(r2.clone(), cx));
        self.history.update(cx, |h, cx| h.show(r2.clone(), cx));
        self.control.update(cx, |c, cx| {
            c.show(r2, cx);
            c.set_active(true, cx);
        });
        // Back on the tab with the console open: it polls again.
        if self.section == Section::Console {
            self.control.update(cx, |c, cx| c.set_console_open(true, cx));
        }
        let moved = self.server.as_ref() != Some(&root);
        if moved {
            self.server = Some(root);
            self.filters = Filters {
                text: self.filters.text.clone(),
                ..Default::default()
            };
            self.result = None;
            self.diff = None;
            self.drift = None;
            self.drift_hidden = None;
            self.server_nf = None;
            self.show = None;
        }
        // Same server mid-sync: the sync rescans when it ends.
        if moved || !self.running {
            self.seen_rev = self.compare_key(cx);
            self.rescan(cx);
        }
    }

    /// What the comparison depends on in the store: the server's revision and
    /// the paired pack's mod files.
    fn compare_key(&self, cx: &App) -> u64 {
        use std::hash::{Hash, Hasher};
        let store = self.store.read(cx);
        let mut h = std::collections::hash_map::DefaultHasher::new();
        store.server_rev().hash(&mut h);
        let inst = self
            .server
            .as_ref()
            .and_then(|r| store.server(r))
            .and_then(|s| store.paired(s));
        if let Some(inst) = inst {
            inst.root.hash(&mut h);
            for a in &inst.addons {
                (&a.file_name, a.enabled, a.file_id).hash(&mut h);
            }
        }
        h.finish()
    }

    fn pair(&self, cx: &App) -> Option<(ServerInfo, Instance)> {
        let store = self.store.read(cx);
        let s = store.server(self.server.as_ref()?)?.clone();
        let inst = store.paired(&s)?.clone();
        Some((s, inst))
    }

    /// The server and its pack's header (no mod list), for drawing.
    fn pair_header(&self, cx: &App) -> Option<(ServerInfo, Instance)> {
        let store = self.store.read(cx);
        let s = store.server(self.server.as_ref()?)?;
        let inst = store.paired(s)?.clone_header();
        Some((s.clone(), inst))
    }

    fn rescan(&mut self, cx: &mut Context<Self>) {
        let Some((server, inst)) = self.pair(cx) else {
            cx.notify();
            return;
        };
        self.comparing = true;
        let anyway = self.store.read(cx).settings().sync_anyway.clone();
        self._task = Some(cx.spawn(async move |this, cx| {
            let root = server.root.clone();
            let (diff, crash, server_nf) = cx
                .background_spawn(async move {
                    (
                        rigsync::compare(&inst, &server, &anyway),
                        hopper::crashscan::last_crash(&server.root),
                        hopper::server_update::server_neoforge(&server.root),
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                if this.server.as_ref() != Some(&root) {
                    return;
                }
                this.comparing = false;
                this.crash = crash;
                cx.emit(SyncEvent::Crash);
                if let Ok((d, _)) = &diff {
                    this.check_drift(d, cx);
                }
                this.server_nf = server_nf;
                this.want_nf_versions(cx);
                if this.holding() {
                    this.diff_next = Some(diff.map(|(d, n)| (d, Some(n))).map_or_else(|e| (Err(e), None), |(d, n)| (Ok(d), n)));
                    cx.notify();
                    return;
                }
                match diff {
                    Ok((d, note)) => {
                        let inst = this.pair(cx).map(|(_, i)| i);
                        this.store.update(cx, |s, _| {
                            for m in d.iter().filter_map(|e| e.client.as_ref()) {
                                s.want_image(&m.icon);
                            }
                            if let Some(i) = &inst {
                                s.want_categories(&i.addons);
                            }
                        });
                        // Nothing is picked for you; picks that still exist
                        // survive a rescan.
                        let keys: HashSet<String> = d.iter().map(row_key).collect();
                        this.selected.retain(|k| keys.contains(k));
                        this.note = Some(note);
                        this.diff = Some(Ok(d));
                    }
                    Err(e) => this.diff = Some(Err(e)),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Looks for global datapacks and configs of the server's mods that
    /// differ from the pack, apart from the mod comparison so its list
    /// isn't held up.
    fn check_drift(&mut self, diff: &[DiffEntry], cx: &mut Context<Self>) {
        let Some((server, inst)) = self.pair_header(cx) else { return };
        let mods: Vec<(String, String)> = diff
            .iter()
            .filter_map(|e| e.server.as_ref())
            .filter(|m| m.side != "client" && !m.mod_id.is_empty())
            .map(|m| (m.mod_id.clone(), m.mod_name.clone()))
            .collect();
        let root = server.root.clone();
        self._drift_task = Some(cx.spawn(async move |this, cx| {
            let d = cx
                .background_spawn(async move { hopper::configdiff::drift(&inst.root, &server.root, &mods) })
                .await;
            this.update(cx, |this, cx| {
                if this.server.as_ref() == Some(&root) {
                    this.drift = Some(d);
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// Up, by ping or by its panel (which also knows a server that is still
    /// starting and doesn't answer pings yet).
    fn online(&self, cx: &App) -> bool {
        let Some(r) = self.server.as_ref() else { return false };
        server_online(self.store.read(cx), Some(self.control.read(cx)), r)
    }

    fn run(&mut self, ops: Ops, cx: &mut Context<Self>) {
        let Some((server, inst)) = self.pair(cx) else {
            return;
        };
        if ops.is_empty() {
            return;
        }
        self.running = true;
        self.result = None;
        self._task = Some(cx.spawn(async move |this, cx| {
            let results = cx
                .background_spawn(async move { rigsync::run_on(ops, &server, &inst.root) })
                .await;
            this.update(cx, |this, cx| {
                this.running = false;
                this.result = Some(rigsync::summary(&results));
                this.rescan(cx);
            })
            .ok();
        }));
        cx.notify();
    }

    /// Rows are acting, done, or leaving: a new comparison waits.
    fn holding(&self) -> bool {
        !self.acting.is_empty() || !self.acted.is_empty() || self.leaving.is_some()
    }

    /// One row's action: runs beside other rows' (no waiting on them), then
    /// marks the row done and compares again. The new comparison shows once
    /// the user stops clicking.
    fn act_row(&mut self, key: String, words: (&'static str, &'static str), ops: Ops, cx: &mut Context<Self>) {
        let Some((server, inst)) = self.pair(cx) else { return };
        if ops.is_empty() || self.acting.contains_key(&key) {
            return;
        }
        self.settle.clicked();
        self.acting.insert(key.clone(), words);
        self.result = None;
        self.ensure_ticker(cx);
        cx.spawn(async move |this, cx| {
            // History is read, changed and written: one action at a time.
            static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
            let results = cx
                .background_spawn(async move {
                    let _g = ONE.lock().unwrap_or_else(|e| e.into_inner());
                    rigsync::run_on(ops, &server, &inst.root)
                })
                .await;
            this.update(cx, |this, cx| {
                this.acting.remove(&key);
                match rigsync::summary(&results) {
                    Ok(_) => {
                        this.acted.insert(key, words.1);
                    }
                    Err(e) => this.result = Some(Err(e)),
                }
                this.rescan(cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn ensure_ticker(&mut self, cx: &mut Context<Self>) {
        if self._ticker.is_some() {
            return;
        }
        self._ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(100)).await;
                if !this.update(cx, |this, cx| this.tick(cx)).unwrap_or(false) {
                    break;
                }
            }
        }));
    }

    /// Lets done rows go once the user is done clicking: they fade, then the
    /// comparison that waited is shown.
    fn tick(&mut self, cx: &mut Context<Self>) -> bool {
        if let Some((since, _)) = &self.leaving {
            if since.elapsed() >= widgets::FADE && !self.comparing {
                self.leaving = None;
                self.acted.clear();
                if let Some((diff, note)) = self.diff_next.take() {
                    if let Ok(d) = &diff {
                        let keys: HashSet<String> = d.iter().map(row_key).collect();
                        self.selected.retain(|k| keys.contains(k));
                    }
                    if note.is_some() {
                        self.note = note;
                    }
                    self.diff = Some(diff);
                }
                cx.notify();
            }
        } else if !self.acted.is_empty() && self.acting.is_empty() && self.settle.settled() {
            self.leaving = Some((std::time::Instant::now(), self.acted.keys().cloned().collect()));
            cx.notify();
        } else if self.acted.is_empty() && self.acting.is_empty() && !self.comparing {
            // Nothing to wait for: a comparison that came in shows now.
            if let Some((diff, note)) = self.diff_next.take() {
                if note.is_some() {
                    self.note = note;
                }
                self.diff = Some(diff);
                cx.notify();
            }
        }
        let more = self.holding() || self.diff_next.is_some() || self.comparing;
        if !more {
            self._ticker = None;
        }
        more
    }

    /// Picked rows the filters show: a pick the filters hide does nothing.
    fn selected_rows(&self, cx: &App) -> Vec<DiffEntry> {
        self.visible(cx)
            .into_iter()
            .filter(|e| self.selected.contains(&row_key(e)))
            .cloned()
            .collect()
    }

    fn confirm_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((server, inst)) = self.pair(cx) else {
            return;
        };
        let server_nf = hopper::server_update::server_neoforge(&server.root);
        let pack_nf = inst.loader.strip_prefix("neoforge-").map(str::to_string);
        let what = match (&pack_nf, &server_nf) {
            (Some(p), Some(sv)) if p != sv => {
                format!("install NeoForge {p}, point run.bat at it, and sync mods")
            }
            _ => "sync mods".to_string(),
        };
        let store = self.store.clone();
        let name = server.name.clone();
        let body = format!(
            "This will {what} in {}. Replaced files go to .hopper-backups.",
            server.root.display()
        );
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (store, server, inst) = (store.clone(), server.clone(), inst.clone());
            alert
                .title(format!("Sync {name} to this pack?"))
                .description(body.clone())
                .confirm()
                .ok_text("Sync to server")
                .on_ok(move |_, _, cx| {
                    let s = store.read(cx);
                    let host = s.settings().server_host.clone();
                    let anyway = s.settings().sync_anyway.clone();
                    let java =
                        hopper::launch::installer_java(Path::new(&s.settings().launcher_dir));
                    let panel = hopper::panel::for_server(s.settings(), &server);
                    let (server, inst) = (server.clone(), inst.clone());
                    s.spawn_job(format!("Updating {}", server.name), move |job| {
                        let r = hopper::server_update::update_server(
                            &server, &inst, &host, &anyway, &java, panel.as_deref(), job,
                        )?;
                        job.detail(r.summary());
                        for f in &r.failed {
                            job.note(f.clone());
                        }
                        Ok(Msg::ServersChanged)
                    });
                    true
                })
        });
    }

    /// Hide asks first: the picked mods leave this comparison until shown
    /// again from a mod's card.
    fn confirm_hide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let keys: Vec<String> = self.selected_rows(cx).iter().filter_map(rigsync::blacklist_key).collect();
        if keys.is_empty() {
            return;
        }
        let n = keys.len();
        let view = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (view, keys) = (view.clone(), keys.clone());
            alert
                .title(format!("Hide {n} mod{} from this comparison?", if n == 1 { "" } else { "s" }))
                .description("Nothing is deleted or changed on the server. They stop showing here, and counts leave them out.")
                .confirm()
                .ok_text("Hide")
                .on_ok(move |_, _, cx| {
                    view.update(cx, |this, cx| {
                        this.selected.clear();
                        this.store.update(cx, |s, cx| {
                            s.update_settings(|st| st.sync_blacklist.extend(keys.clone()), cx)
                        });
                    });
                    true
                })
        });
    }

    /// A sync that removes jars from the server says so first.
    fn confirm_sync(&mut self, ops: Ops, n_sel: usize, n_remove: usize, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.entity();
        let up = ops.up.clone();
        let down = ops.down.clone();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (view, up, down) = (view.clone(), up.clone(), down.clone());
            alert
                .title(format!("Sync {n_sel} mod{} to the server?", if n_sel == 1 { "" } else { "s" }))
                .description(format!(
                    "{n_remove} will be removed from the server. Removed and replaced jars go to .hopper-backups, and the change log in History can put them back."
                ))
                .confirm()
                .ok_text("Sync to server")
                .on_ok(move |_, _, cx| {
                    view.update(cx, |this, cx| {
                        this.selected.clear();
                        this.run(Ops { up: up.clone(), down: down.clone() }, cx)
                    });
                    true
                })
        });
    }

    /// The pick key of list line `ix`: a row whose group can be picked.
    fn line_key(&self, ix: usize) -> Option<String> {
        let Some(Ok(diff)) = &self.diff else { return None };
        match self.items.get(ix)? {
            Item::Row(i) => diff.get(*i).filter(|d| Group::of(d).pickable()).map(row_key),
            _ => None,
        }
    }

    /// List line `ix`'s row button, when it can run now: the row's key, the
    /// button's words, what it does, its group and the row.
    fn line_act(&self, ix: usize, cx: &App) -> Option<(String, (&'static str, &'static str), RowAct, Group, DiffEntry)> {
        let Some(Ok(diff)) = &self.diff else { return None };
        let Item::Row(i) = self.items.get(ix)? else { return None };
        let d = diff.get(*i)?;
        let g = Group::of(d);
        let (words, act) = match g {
            Group::Add => (("Adding", "Added"), RowAct::Push),
            Group::Update if d.status == DiffStatus::ServerNewer => (("Replacing", "Replaced"), RowAct::Push),
            Group::Update => (("Updating", "Updated"), RowAct::Push),
            Group::ServerOnly => (("Removing", "Removed"), RowAct::Remove),
            Group::KeptOff => (("Sending", "Sent"), RowAct::Push),
            _ => return None,
        };
        let key = row_key(d);
        let blocked = self.running || (self.online(cx) && !self.allow_online);
        if blocked || self.acted.contains_key(&key) || self.acting.contains_key(&key) {
            return None;
        }
        Some((key, words, act, g, d.clone()))
    }

    /// The rows a button drag crosses that have the first row's kind of
    /// button (Add, Update, Remove…).
    fn swept_acts(&self, p: &DragPick, cx: &App) -> Vec<(String, (&'static str, &'static str), RowAct, DiffEntry)> {
        let Some((.., g0, _)) = self.line_act(p.start, cx) else { return Vec::new() };
        p.rows()
            .filter_map(|ix| self.line_act(ix, cx))
            .filter(|(.., g, _)| *g == g0)
            .map(|(k, w, a, _, d)| (k, w, a, d))
            .collect()
    }

    /// A row button pressed: a drag across rows may follow.
    fn sweep_down(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(ix) = (0..self.items.len()).find(|&i| {
            matches!(self.items.get(i), Some(Item::Row(r)) if matches!(&self.diff, Some(Ok(d)) if d.get(*r).is_some_and(|d| row_key(d) == key)))
        }) else {
            return;
        };
        if self.line_act(ix, cx).is_none() {
            return;
        }
        self.swept.clear();
        drag_pick::start(self, DragPick::sweep(ix, Sweep::Action, true), cx);
    }

    /// A tick box pressed: Shift ticks the rows from the last pressed box,
    /// else a drag starts (ticking, or unticking from a ticked box).
    fn pick_down(&mut self, key: &str, shift: bool, cx: &mut Context<Self>) {
        let Some(ix) = (0..self.items.len()).find(|&i| self.line_key(i).as_deref() == Some(key)) else {
            return;
        };
        match self.pick_anchor.filter(|_| shift) {
            Some(a) => {
                for i in a.min(ix)..=a.max(ix) {
                    if let Some(k) = self.line_key(i) {
                        self.selected.insert(k);
                    }
                }
                self.pick.drag = None;
            }
            None => {
                let p = DragPick::new(ix, !self.selected.contains(key), self.selected.clone());
                self.selected = p.apply(|i| self.line_key(i));
                drag_pick::start(self, p, cx);
            }
        }
        self.pick_anchor = Some(ix);
        cx.notify();
    }

    /// Lists the NeoForge builds for the pack's Minecraft version, once,
    /// when the server runs NeoForge.
    fn want_nf_versions(&mut self, cx: &mut Context<Self>) {
        if self.nf_loading || self.server_nf.is_none() {
            return;
        }
        let Some((_, inst)) = self.pair_header(cx) else { return };
        if self.nf_versions.as_ref().is_some_and(|(mc, _)| *mc == inst.game_version) {
            return;
        }
        self.nf_loading = true;
        let mc = inst.game_version.clone();
        cx.spawn(async move |this, cx| {
            let mc2 = mc.clone();
            let list = cx
                .background_spawn(async move {
                    hopper::cf::loader_versions(&mc2)
                        .map(|v| {
                            v.into_iter()
                                .filter_map(|l| l.name.strip_prefix("neoforge-").map(str::to_string))
                                .collect::<Vec<_>>()
                        })
                        .map_err(|e| format!("{e:#}"))
                })
                .await;
            this.update(cx, |this, cx| {
                this.nf_loading = false;
                this.nf_versions = Some((mc, list));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Asks, then installs NeoForge `want` on the server (mods untouched).
    fn set_loader(&mut self, want: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some((server, _)) = self.pair_header(cx) else { return };
        let have = self.server_nf.clone().unwrap_or_default();
        if have == want {
            return;
        }
        let store = self.store.clone();
        let name = server.name.clone();
        let older = hopper::scan::compare_versions(&want, &have) == std::cmp::Ordering::Less;
        let body = format!(
            "Installs NeoForge {want} in {} and points run.bat at it{}. Mods aren't touched. The old run.bat is kept in .hopper-backups.",
            server.root.display(),
            if older { " (an older build than the server has now)" } else { "" },
        );
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (store, server, want) = (store.clone(), server.clone(), want.clone());
            alert
                .title(format!("Switch {name} to NeoForge {want}?"))
                .description(body.clone())
                .confirm()
                .ok_text("Switch loader")
                .on_ok(move |_, _, cx| {
                    let s = store.read(cx);
                    let host = s.settings().server_host.clone();
                    let java = hopper::launch::installer_java(Path::new(&s.settings().launcher_dir));
                    let panel = hopper::panel::for_server(s.settings(), &server);
                    let (server, want) = (server.clone(), want.clone());
                    s.spawn_job(format!("NeoForge {want} on {}", server.name), move |job| {
                        let r = hopper::server_update::set_server_neoforge(
                            &server, &want, &host, &java, panel.as_deref(), job,
                        )?;
                        job.detail(r);
                        Ok(Msg::ServersChanged)
                    });
                    true
                })
        });
    }

    /// The loader picker's menu: the pack's build, the newest, the one on the
    /// server now, then every build for the pack's Minecraft version.
    fn loader_menu(view: &Entity<Self>, menu: PopupMenu, cx: &App) -> PopupMenu {
        let this = view.read(cx);
        let Some((_, inst)) = this.pair_header(cx) else { return menu };
        let pack = inst.loader.strip_prefix("neoforge-").map(str::to_string);
        let have = this.server_nf.clone();
        let list = this.nf_versions.as_ref().map(|(_, l)| l.clone());
        let pick = |v: String, label: String| {
            let view = view.clone();
            let current = have.as_deref() == Some(v.as_str());
            PopupMenuItem::new(label)
                .checked(current)
                .disabled(current)
                .on_click(move |_, window, cx| {
                    let v = v.clone();
                    view.update(cx, |this, cx| this.set_loader(v, window, cx))
                })
        };
        let mut menu = menu.scrollable(true).max_h(px(440.)).label("Suggested");
        if let Some(p) = &pack {
            menu = menu.item(pick(p.clone(), format!("{p}  ·  the pack's")));
        }
        let latest = list.as_ref().and_then(|l| l.as_ref().ok()).and_then(|l| l.first().cloned());
        if let Some(l) = latest.clone().filter(|l| Some(l) != pack.as_ref()) {
            menu = menu.item(pick(l.clone(), format!("{l}  ·  newest")));
        }
        if let Some(h) = have.clone().filter(|h| Some(h) != pack.as_ref() && Some(h) != latest.as_ref()) {
            menu = menu.item(pick(h.clone(), format!("{h}  ·  on the server now")));
        }
        menu = menu.separator().label(format!("All for Minecraft {}", inst.game_version));
        match list {
            None => menu.item(PopupMenuItem::new("Loading versions…").disabled(true)),
            Some(Err(e)) => menu.item(PopupMenuItem::new(e).disabled(true)),
            Some(Ok(l)) => l.into_iter().take(80).fold(menu, |m, v| m.item(pick(v.clone(), v))),
        }
    }

    /// The server's state dot and word: the panel's state, else a ping,
    /// and red when its last start crashed.
    fn state(&self, cx: &App) -> (Hsla, &'static str) {
        let Some(root) = self.server.as_deref() else {
            return (cx.theme().muted_foreground, "");
        };
        let ctl = self.control.read(cx);
        let store = self.store.read(cx);
        let stats = ctl.stats_of(root).or_else(|| store.panel_stats(root));
        let pinged = store.status(root).map(|s| matches!(s, Status::Online { .. }));
        crate::server_control::state_of(stats, pinged, self.crash.is_some(), ctl.busy(), cx)
    }

    /// The Server tab's left column: the server with its state and
    /// controls, the loader, and the pack's server pack.
    fn sidebar(&self, server: &ServerInfo, inst: &Instance, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let controls = self
            .control
            .read(cx)
            .configured(cx)
            .then(|| self.control.update(cx, |c, cx| c.controls_compact(cx)));
        let online = self.online(cx);
        let (dot, word) = self.state(cx);
        let view = cx.entity();
        let theme = cx.theme().clone();
        let card = || {
            v_flex()
                .gap_2()
                .p_3()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .bg(theme.secondary.opacity(0.5))
        };

        // The server: its name and state, why its last start crashed, and
        // its controls.
        let reason = self.crash.as_ref().filter(|_| !online).map(crash_reason);
        let server_card = card()
            .child(
                h_flex()
                    .gap_2()
                    .child(widgets::status_dot(dot))
                    .child(div().min_w_0().font_semibold().truncate().child(server.name.clone()))
                    .when(!word.is_empty(), |r| {
                        r.child(widgets::caption(format!("· {word}"), cx).text_xs().flex_none().text_color(dot))
                    })
                    .child(div().flex_1())
                    .child(
                        Button::new("rescan")
                            .ghost()
                            .xsmall()
                            .icon(IconName::RefreshCw)
                            .tooltip("Compare again")
                            .loading(self.comparing)
                            .disabled(self.running || self.comparing)
                            .on_click(cx.listener(|this, _, _, cx| this.rescan(cx))),
                    ),
            )
            .when_some(reason, |c, r| c.child(div().text_xs().text_color(theme.danger).child(r)))
            .children(controls)
            .when(online, |c| {
                c.child(
                    h_flex()
                        .gap_1p5()
                        .text_xs()
                        .text_color(theme.warning)
                        .child(Icon::new(IconName::TriangleAlert).xsmall())
                        .child("Running: stop it before syncing"),
                )
            });

        // The loader: a fix when it differs from the pack, else a quiet line.
        let pack_nf = inst.loader.strip_prefix("neoforge-").map(str::to_string);
        let blocked = online || self.running;
        let why_blocked = if online { "Stop the server first" } else { "Wait for the sync to finish" };
        let picker = |b: Button| {
            let view = view.clone();
            b.disabled(blocked)
                .when(blocked, |b| b.tooltip(why_blocked))
                .dropdown_menu(move |menu, _, cx| Self::loader_menu(&view, menu, cx))
                .into_any_element()
        };
        let loader = match (&pack_nf, &self.server_nf) {
            (Some(p), Some(sv)) if p != sv => {
                let p2 = p.clone();
                let kv = |k: &'static str, v: String, c: Hsla| {
                    h_flex()
                        .text_xs()
                        .child(div().flex_1().text_color(theme.muted_foreground).child(k))
                        .child(div().text_color(c).child(v))
                };
                Some(
                    card()
                        .border_color(theme.warning.opacity(0.45))
                        .child(
                            h_flex()
                                .gap_1p5()
                                .text_xs()
                                .font_semibold()
                                .text_color(theme.warning)
                                .child(Icon::new(IconName::TriangleAlert).xsmall())
                                .child("LOADER DIFFERS"),
                        )
                        .child(kv("Server", format!("NeoForge {sv}"), theme.foreground))
                        .child(kv("Pack", format!("NeoForge {p}"), theme.primary))
                        .child(
                            h_flex()
                                .w_full()
                                .child(
                                    widgets::split_lead(
                                        Button::new("nf-match")
                                            .outline()
                                            .small()
                                            .flex_1()
                                            .icon(IconName::RefreshCw)
                                            .label(format!("Match pack · {p}"))
                                            .disabled(blocked)
                                            .tooltip(if blocked {
                                                why_blocked.to_string()
                                            } else {
                                                format!("Install NeoForge {p} on the server. Mods aren't touched.")
                                            })
                                            .on_click(cx.listener(move |this, _, w, cx| {
                                                this.set_loader(p2.clone(), w, cx)
                                            })),
                                    ),
                                )
                                .child(picker(
                                    Button::new("nf-pick")
                                        .outline()
                                        .small()
                                        .icon(IconName::ChevronDown)
                                        .rounded_tl(px(0.))
                                        .rounded_bl(px(0.))
                                        .border_l_0(),

                                )),
                        )
                        .into_any_element(),
                )
            }
            (_, Some(sv)) => Some(
                card()
                    .child(
                        h_flex()
                            .gap_2()
                            .text_sm()
                            .child(Icon::new(IconName::CircleCheck).small().text_color(theme.success))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().truncate().child(format!("NeoForge {sv}")))
                                    .child(
                                        widgets::caption(
                                            if pack_nf.is_some() { "Same as the pack" } else { "On the server" },
                                            cx,
                                        )
                                        .text_xs(),
                                    ),
                            )
                            .child(picker(
                                Button::new("nf-change").ghost().xsmall().label("Change").icon(IconName::ChevronDown),
                            )),
                    )
                    .into_any_element(),
            ),
            _ => None,
        };

        // The pack author's server pack: what it lists, and taking its mods
        // and configs.
        let cf_pack = inst
            .pack
            .as_ref()
            .is_some_and(|p| p.project_id != 0 && !hopper::modrinth::is_mr(p.project_id));
        let pack_line = match self.note.as_ref().map(|a| (a.server_pack_jars, &a.note)) {
            Some((Some(n), _)) => Some(format!("Lists {n} mods")),
            Some((None, Some(note))) => Some(note.clone()),
            _ => None,
        };
        let pack_btn = cf_pack.then(|| {
            let (server, inst) = (server.clone(), inst.clone());
            Button::new("server-pack-update")
                .outline()
                .small()
                .w_full()
                .icon(IconName::Package)
                .label("Update from server pack")
                .tooltip("Uses the pack author's official server pack: its mods and config files. Shows what changes first, like a pack update.")
                .disabled(online || self.running)
                .on_click(cx.listener(move |this, _, w, cx| {
                    crate::pack_update::open_server(this.store.clone(), server.clone(), inst.clone(), w, cx)
                }))
        });
        let pack_card = (pack_line.is_some() || pack_btn.is_some()).then(|| {
            card()
                .child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .font_semibold()
                        .child(Icon::new(IconName::Package).small().text_color(theme.muted_foreground))
                        .child("Server pack"),
                )
                .when_some(pack_line, |c, t| c.child(widgets::caption(t, cx).text_xs()))
                .children(pack_btn)
        });

        let border = theme.border;
        let content = v_flex()
            .id("sync-side")
            .size_full()
            .gap_3()
            .p_4()
            .overflow_y_scroll()
            .child(server_card)
            .children(loader)
            .children(pack_card);
        widgets::side_panel("sync-side-resize", &self.side_panel, |p| &mut p.side_panel, &self.store, content, window, cx)
            .border_r_1()
            .border_color(border)
            .into_any_element()
    }

    // ---- rendering -----------------------------------------------------

    fn header(
        &self,
        server: &ServerInfo,
        inst: &Instance,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        // Quick start / stop when a panel is set up; Servers sets one up.
        let in_server_page = self.in_server_page;
        let controls = (!in_server_page && self.control.read(cx).configured(cx))
            .then(|| self.control.update(cx, |c, cx| c.controls(cx)));
        // The server's page has start / stop above: the pack is what to pick.
        let repair = in_server_page.then(|| {
            div()
                .w(rems(14.))
                .child(self.pair_picker(&server.root, inst.name.clone(), Some(&inst.root), cx))
        });
        let stop_where = if self.control.read(cx).configured(cx) { "stop it here" } else { "stop it in Crafty/MCSS" };
        let in_instance = self.in_instance;
        let store = self.store.read(cx);
        let theme = cx.theme();
        let icon = inst
            .icon
            .as_deref()
            .and_then(|u| store.image(u))
            .map(Path::to_path_buf);
        let online = self.online(cx);
        let pack_nf = inst.loader.strip_prefix("neoforge-").map(str::to_string);
        let nf_mismatch = match (&pack_nf, &self.server_nf) {
            (Some(p), Some(sv)) if p != sv => Some(format!("Server NeoForge {sv}, pack {p}")),
            _ => None,
        };
        // One line of facts: loader, then what the server pack said.
        let mut facts = Vec::new();
        if nf_mismatch.is_none()
            && let (Some(p), Some(_)) = (&pack_nf, &self.server_nf)
        {
            facts.push(format!("NeoForge {p} on both"));
        }
        match self.note.as_ref().map(|a| (a.server_pack_jars, &a.note, a.flagged)) {
            Some((Some(n), _, _)) => facts.push(format!("the pack's server pack lists {n} mods")),
            Some((None, Some(note), f)) => facts.push(format!("{note} {f} mods flagged.")),
            _ => {}
        }
        let facts = (!facts.is_empty()).then(|| {
            let mut t = facts.join(" · ");
            if let Some(c) = t.get(0..1) {
                t.replace_range(0..1, &c.to_uppercase());
            }
            t
        });
        let title = h_flex()
            .flex_1()
            .min_w_0()
            .gap_2()
            .text_lg()
            .font_semibold()
            .when(in_server_page, |r| {
                r.child(div().text_sm().font_normal().text_color(theme.muted_foreground).child("Paired with"))
                    .children(repair)
            })
            .when(!in_instance && !in_server_page, |r| {
                r.child(div().truncate().child(inst.name.clone())).child(
                    Icon::new(IconName::ArrowRight)
                        .small()
                        .text_color(theme.muted_foreground),
                )
            })
            .when(in_instance, |r| {
                r.child(Icon::new(IconName::Server).text_color(theme.muted_foreground))
            })
            .when(!in_server_page, |r| r.child(div().truncate().child(server.name.clone())));
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_3()
                    .when(!in_instance, |r| r.child(widgets::thumb(icon.as_deref(), rems(if in_server_page { 2. } else { 2.5 }), cx)))
                    .child(title)
                    .children(controls)
                    .when(
                        inst.pack
                            .as_ref()
                            .is_some_and(|p| p.project_id != 0 && !hopper::modrinth::is_mr(p.project_id)),
                        |r| {
                            let (server, inst) = (server.clone(), inst.clone());
                            r.child(
                                Button::new("server-pack-update")
                                    .outline()
                                    .small()
                                    .icon(IconName::Package)
                                    .label("Update from server pack")
                                    .tooltip("Uses the pack author's official server pack: its mods and config files. Shows what changes first, like a pack update.")
                                    .disabled(online || self.running)
                                    .on_click(cx.listener(move |this, _, w, cx| {
                                        crate::pack_update::open_server(
                                            this.store.clone(),
                                            server.clone(),
                                            inst.clone(),
                                            w,
                                            cx,
                                        )
                                    })),
                            )
                        },
                    )
                    // Sync is the filter row's button, as on the instance's Server tab.
                    .child(
                        Button::new("rescan")
                            .ghost()
                            .small()
                            .icon(IconName::RefreshCw)
                            .tooltip("Compare again")
                            .disabled(self.running || self.comparing)
                            .on_click(cx.listener(|this, _, _, cx| this.rescan(cx))),
                    ),
            )
            .when_some(nf_mismatch, |r, t| {
                r.child(
                    h_flex()
                        .gap_1p5()
                        .text_sm()
                        .text_color(theme.warning)
                        .child(Icon::new(IconName::TriangleAlert).small())
                        .child(t),
                )
            })
            .when_some(facts, |r, t| r.child(widgets::caption(t, cx)))
            .when(online, |r| {
                r.child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .text_color(theme.warning)
                        .child(Icon::new(IconName::TriangleAlert).small())
                        .child(format!("Server is online. Mods load at startup, so {stop_where} before syncing.")),
                )
            })
    }

    fn crash_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let c = self.crash.clone()?;
        let server_mods: Vec<ModInfo> = match &self.diff {
            Some(Ok(d)) => d.iter().filter_map(|e| e.server.clone()).collect(),
            _ => Vec::new(),
        };
        let online = self.online(cx);
        let theme = cx.theme();
        let rows = c.hits.iter().map(|h| {
            let jar = server_mods
                .iter()
                .find(|m| {
                    (!h.file_name.is_empty() && m.file_name.eq_ignore_ascii_case(&h.file_name))
                        || (!h.mod_id.is_empty() && m.mod_id.eq_ignore_ascii_case(&h.mod_id))
                })
                .map(|m| m.file_name.clone());
            let name = if h.mod_id.is_empty() {
                h.file_name.clone()
            } else {
                h.mod_id.clone()
            };
            // The pack's copy of the mod: its name, icon and page.
            let addon = {
                let store = self.store.read(cx);
                let stem = hopper::serverpack::jar_stem(&h.file_name);
                self.server
                    .as_ref()
                    .and_then(|r| store.server(r))
                    .and_then(|sv| store.paired(sv))
                    .and_then(|inst| {
                        inst.addons
                            .iter()
                            .find(|a| {
                                (!h.file_name.is_empty() && a.file_name.eq_ignore_ascii_case(&h.file_name))
                                    || (!stem.is_empty() && hopper::serverpack::jar_stem(&a.file_name) == stem)
                            })
                            .cloned()
                    })
            };
            let name = addon.as_ref().map(|a| a.name.clone()).filter(|n| !n.is_empty()).unwrap_or(name);
            let pid = addon.as_ref().map_or(0, |a| a.project_id);
            let icon = addon
                .as_ref()
                .and_then(|a| self.store.read(cx).image(&a.icon))
                .map(Path::to_path_buf);
            let view = cx.entity();
            h_flex()
                .gap_3()
                .child(
                    widgets::mod_link(
                        ElementId::Name(format!("crash-mod:{name}").into()),
                        name.clone(),
                        icon.as_deref(),
                        pid,
                        move |p, _, cx| view.update(cx, |this, cx| this.open_peek(p, cx)),
                        cx,
                    )
                    .text_sm()
                    .font_semibold(),
                )
                .child(widgets::caption(h.reason.clone(), cx).flex_1().min_w_0())
                .when_some(jar, |r, j| {
                    let server = self.server.clone();
                    r.child(
                        Button::new(ElementId::Name(format!("crash-rm:{j}").into()))
                            .xsmall()
                            .outline()
                            .label("Remove from server")
                            .tooltip(format!("Moves {j} to .hopper-backups (undo from History)"))
                            .disabled(online || self.running)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(root) = &server {
                                    let path = root.join("mods").join(&j);
                                    this.run(
                                        Ops {
                                            up: vec![hopper::model::SyncOp::Delete { path }],
                                            down: Vec::new(),
                                        },
                                        cx,
                                    );
                                }
                            })),
                    )
                })
        });
        Some(
            v_flex()
                .gap_2()
                .p_3()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.danger)
                .child(
                    h_flex()
                        .gap_2()
                        .text_color(theme.danger)
                        .font_semibold()
                        .child(Icon::new(IconName::TriangleAlert).small())
                        .child(format!("The server's last start crashed: {}", c.headline)),
                )
                .child(widgets::caption(
                    if c.client_only {
                        format!(
                            "From {}. Client-only mods on the server are the likely cause.",
                            c.source
                        )
                    } else {
                        format!("From {}.", c.source)
                    },
                    cx,
                ))
                .children(rows)
                .into_any_element(),
        )
    }

    /// Opens the Configs section on the files a drift group lists.
    fn review_drift(&mut self, folder: &str, filter: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.section = Section::Configs;
        self.configs.update(cx, |c, cx| c.focus(folder, filter, window, cx));
        cx.notify();
    }

    /// Pack and server updates often leave global datapacks and the configs
    /// of new mods behind: which differ from the pack, each a click from
    /// its files in Configs.
    fn drift_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        use hopper::configdiff::{DriftGroup, FileStatus};
        let d = self.drift.as_ref().filter(|d| !d.is_empty())?;
        let sig = d.signature();
        if self.drift_hidden == Some(sig) || self.section != Section::Mods {
            return None;
        }
        let theme = cx.theme().clone();
        let counts = |g: &DriftGroup| {
            let mut v = Vec::new();
            for (s, w) in [
                (FileStatus::OnlyClient, "missing on the server"),
                (FileStatus::Modified, "changed"),
                (FileStatus::OnlyServer, "only on the server"),
            ] {
                let n = g.count(s);
                if n > 0 {
                    v.push(format!("{n} {w}"));
                }
            }
            v.join(" · ")
        };
        let row = |ix: usize, g: &DriftGroup, data: bool, cx: &mut Context<Self>| {
            let (folder, filter) = (g.folder.clone(), g.filter.clone());
            h_flex()
                .gap_3()
                .child(
                    Icon::new(if data { IconName::Package } else { IconName::Settings })
                        .xsmall()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(div().text_sm().font_semibold().truncate().max_w(rems(22.)).child(g.label.clone()))
                .child(widgets::caption(counts(g), cx).flex_1().min_w_0())
                .child(
                    Button::new(ElementId::Name(format!("drift-review:{ix}").into()))
                        .xsmall()
                        .outline()
                        .label("Review")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.review_drift(&folder, &filter, window, cx)
                        })),
                )
        };
        const SHOWN: usize = 8;
        let groups: Vec<(&DriftGroup, bool)> = d
            .data
            .iter()
            .map(|g| (g, true))
            .chain(d.mods.iter().map(|g| (g, false)))
            .collect();
        let more = groups.len().saturating_sub(SHOWN);
        let rows: Vec<AnyElement> = if self.drift_open {
            groups
                .iter()
                .take(SHOWN)
                .enumerate()
                .map(|(i, (g, data))| row(i, g, *data, cx).into_any_element())
                .collect()
        } else {
            Vec::new()
        };
        let open = self.drift_open;
        // The first group's folder, unfiltered: the rest are a tab away.
        let first = groups.first().map(|(g, _)| g.folder.clone());
        // One line (a tint like the mock's), its groups below when opened.
        Some(
            v_flex()
                .gap_2()
                .px_3()
                .py_2()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.warning.opacity(0.45))
                .bg(theme.warning.opacity(0.05))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            h_flex()
                                .id("drift-toggle")
                                .flex_1()
                                .min_w_0()
                                .gap_2()
                                .clickable(cx)
                                .text_color(theme.warning)
                                .font_semibold()
                                .child(
                                    Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                                        .small(),
                                )
                                .child(Icon::new(IconName::TriangleAlert).small())
                                .child(
                                    div()
                                        .truncate()
                                        .child(format!("Also differs: {}", d.summary())),
                                )
                                // The why, which used to be a paragraph.
                                .tooltip(|w, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new(
                                        "Pack and server updates don't always bring these along. Check global datapacks \
                                         and server scripts after every update, and the configs of mods new to the pack.",
                                    )
                                    .build(w, cx)
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.drift_open = !this.drift_open;
                                    cx.notify();
                                })),
                        )
                        .when_some(first, |r, folder| {
                            r.child(
                                Button::new("drift-review-all")
                                    .xsmall()
                                    .outline()
                                    .label("Review in Configs")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.review_drift(&folder, "", window, cx)
                                    })),
                            )
                        })
                        .child(
                            Button::new("drift-hide")
                                .xsmall()
                                .ghost()
                                .icon(IconName::X)
                                .tooltip("Hide until something else changes")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.drift_hidden = Some(sig);
                                    cx.notify();
                                })),
                        ),
                )
                .children(rows)
                .when(open && more > 0, |r| {
                    r.child(widgets::caption(format!("And {more} more in Configs."), cx))
                })
                .into_any_element(),
        )
    }

    fn group_color(g: Group, cx: &App) -> Hsla {
        let theme = cx.theme();
        match g {
            Group::Add => theme.primary,
            Group::Update => theme.info,
            Group::ServerOnly => theme.danger,
            Group::KeptOff => theme.warning,
            Group::InSync => theme.success,
        }
    }

    /// Search; the counts as one filter (Needs action · Kept off · In sync
    /// · All); side and source as two small menus; and, in an instance, the
    /// one filled button: Sync N to server.
    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        const ENVS: [(EnvFilter, &str); 4] = [
            (EnvFilter::Any, "Any side"),
            (EnvFilter::Client, "Client only"),
            (EnvFilter::Both, "Client + server"),
            (EnvFilter::Server, "Server only"),
        ];
        const SOURCES: [(SourceFilter, &str); 2] =
            [(SourceFilter::Any, "Pack + yours"), (SourceFilter::Added, "Only yours")];
        let env_ix = ENVS.iter().position(|(e, _)| *e == self.filters.env).unwrap_or(0);
        let src_ix = SOURCES
            .iter()
            .position(|(s, _)| *s == self.filters.source)
            .unwrap_or(0);
        let n_sel = self.selected_rows(cx).len();
        // Counts go by the search and the side and source menus.
        let groups: Vec<Group> = self.counted(cx).into_iter().map(Group::of).collect();
        let shown = self.shown(cx);
        // Empty choices hide, except the one picked.
        let choices: Vec<(Show, usize)> = Show::ALL
            .into_iter()
            .map(|s| (s, groups.iter().filter(|g| s.has(**g)).count()))
            .filter(|(s, n)| *n > 0 || *s == shown)
            .collect();
        let show_ix = choices.iter().position(|(s, _)| *s == shown).unwrap_or(0);
        let picks: Vec<Show> = choices.iter().map(|(s, _)| *s).collect();
        let view = cx.entity();
        let (v1, v2) = (view.clone(), view.clone());
        let side = widgets::filter_menu(
            "sync-env",
            "Side",
            ENVS.iter().map(|(_, l)| *l).collect(),
            env_ix,
            std::rc::Rc::new(move |ix, _, cx| {
                v1.update(cx, |this, cx| {
                    this.filters.env = ENVS[ix].0;
                    cx.notify();
                })
            }),
        );
        let source = widgets::filter_menu(
            "sync-source",
            "Source",
            SOURCES.iter().map(|(_, l)| *l).collect(),
            src_ix,
            std::rc::Rc::new(move |ix, _, cx| {
                v2.update(cx, |this, cx| {
                    this.filters.source = SOURCES[ix].0;
                    cx.notify();
                })
            }),
        );
        // What Sync changes: the whole comparison, whatever the filters.
        let to_change = self.whole(cx).iter().filter(|g| Show::Needs.has(**g)).count();
        let online = self.online(cx);
        let sync = Some(()).map(|_| {
            Button::new("update-server")
                .small()
                .icon(IconName::ServerCog)
                .label(if to_change > 0 { format!("Sync {to_change} to server") } else { "Sync to server".to_string() })
                // Picked rows have their own filled button below.
                .map(|b| if to_change > 0 && n_sel == 0 { b.primary() } else { b.outline() })
                .tooltip(if online {
                    "Stop the server first: mods load at startup."
                } else {
                    "Makes the server's mods match this instance (leaving client-only ones out), and installs the pack's NeoForge on it if needed. Asks first; replaced files go to .hopper-backups."
                })
                .disabled(online || self.running)
                .on_click(cx.listener(|this, _, w, cx| this.confirm_update(w, cx)))
        });
        h_flex()
            .gap_2()
            .flex_wrap()
            .child(
                div().w_48().child(
                    Input::new(&self.search)
                        .small()
                        .prefix(Icon::new(IconName::Search).small())
                        .cleanable(true),
                ),
            )
            .child(
                TabBar::new("sync-show")
                    .segmented()
                    .small()
                    .selected_index(show_ix)
                    .children(choices.iter().map(|(s, n)| Tab::new().label(format!("{} {n}", s.label()))))
                    .on_click(cx.listener(move |this, ix: &usize, _, cx| {
                        if let Some(s) = picks.get(*ix) {
                            this.show = Some(*s);
                            // A pick shows its rows, not folded headers.
                            if *s != Show::All {
                                this.open.extend(Group::ALL.into_iter().filter(|g| s.has(*g)));
                            }
                            this.scroll.scroll_to_item(0, ScrollStrategy::Top);
                        }
                        cx.notify();
                    })),
            )
            .child(side)
            .child(source)
            .child(div().flex_1())
            .when(n_sel > 0, |r| {
                r.child(
                    Button::new("sel-clear")
                        .xsmall()
                        .ghost()
                        .label("Clear selection")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.selected.clear();
                            cx.notify();
                        })),
                )
            })
            .children(sync)
    }

    /// "Server → Pack" over the version column, lined up with the rows.
    fn column_header(&self, cx: &App) -> impl IntoElement + use<> {
        let theme = cx.theme();
        h_flex()
            .w_full()
            .px_3()
            .gap_3()
            .pb_1()
            .text_xs()
            .font_semibold()
            .text_color(theme.muted_foreground)
            .border_b_1()
            .border_color(theme.border.opacity(0.4))
            .child(div().w_6().flex_none())
            .child(div().w(rems(2.)).flex_none())
            .child(div().flex_1().min_w(rems(NAME_MIN)).child("Mod"))
            .when_some(self.fit.tags, |r, w| r.child(div().w(rems(w)).flex_none()))
            .child(div().w(rems(self.fit.version)).flex_none().truncate().child("Server → Pack"))
            .child(div().w(rems(self.fit.action)).flex_none())
    }

    /// What Apply will do, and the buttons for the picked rows.
    fn footer(&self, server: &ServerInfo, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let picked = self.selected_rows(cx);
        let Some((srv, inst)) = self.pair_header(cx) else {
            return div().into_any_element();
        };
        let ops = rigsync::apply_ops(&picked, &inst, &srv);
        let count = |g: Group, s: Option<DiffStatus>| {
            picked
                .iter()
                .filter(|d| Group::of(d) == g && s.is_none_or(|s| d.status == s))
                .count()
        };
        let online = self.online(cx);
        let blocked = self.running || (online && !self.allow_online);
        let n_sel = picked.len();
        let n_remove = count(Group::ServerOnly, None);
        let parts = action_line(picked.iter());
        let todo = action_line(self.visible(cx).into_iter());
        let theme = cx.theme();
        let text = if !parts.is_empty() {
            format!("{parts} on {}", server.name)
        } else if !todo.is_empty() {
            format!("{todo} · tick mods to choose")
        } else {
            "Nothing to sync.".to_string()
        };
        h_flex()
            .gap_3()
            .px_4()
            .py_2p5()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(Icon::new(IconName::ListChecks).small().text_color(theme.muted_foreground))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .truncate()
                    .text_color(if parts.is_empty() {
                        theme.muted_foreground
                    } else {
                        theme.foreground
                    })
                    .child(text),
            )
            .when(self.running, |r| r.child(Spinner::new().small()))
            .when(online, |r| {
                r.child(
                    Checkbox::new("allow-online")
                        .label("Allow while online")
                        .checked(self.allow_online)
                        .on_change(cx.listener(|this, v: &bool, _, cx| {
                            this.allow_online = *v;
                            cx.notify();
                        })),
                )
            })
            .child(
                Button::new("blacklist")
                    .small()
                    .outline()
                    .label(format!("Hide ({n_sel})"))
                    .tooltip("Hide the selected mods from this comparison. Show them again from a mod's card.")
                    .disabled(n_sel == 0)
                    .on_click(cx.listener(|this, _, w, cx| this.confirm_hide(w, cx))),
            )
            .child(
                Button::new("apply")
                    .small()
                    // Filled only with rows ticked: else the filter row's Sync is the one.
                    .map(|b| if n_sel > 0 { b.primary() } else { b.outline() })
                    .icon(IconName::Upload)
                    .label(if n_sel > 0 { format!("Sync to server ({n_sel})") } else { "Sync to server".to_string() })
                    .tooltip("Pushes added and updated jars to the server and removes the ones it no longer needs. Replaced and removed jars are kept in .hopper-backups.")
                    .disabled(ops.is_empty() || blocked)
                    .on_click(cx.listener(move |this, _, w, cx| {
                        let ops = Ops {
                            up: ops.up.clone(),
                            down: ops.down.clone(),
                        };
                        // Removing asks first; adding and updating are backed up.
                        if n_remove > 0 {
                            this.confirm_sync(ops, n_sel, n_remove, w, cx);
                        } else {
                            this.selected.clear();
                            this.run(ops, cx)
                        }
                    })),
            )
            .into_any_element()
    }

    /// The rows the filters let through.
    fn visible(&self, cx: &App) -> Vec<&DiffEntry> {
        let show = self.shown(cx);
        self.counted(cx)
            .into_iter()
            .filter(|d| show.has(Group::of(d)))
            .collect()
    }

    /// The rows the search, side and source filters let through, whatever
    /// the Needs action / Kept off / … pick: what its counts count.
    fn counted(&self, cx: &App) -> Vec<&DiffEntry> {
        let Some(Ok(diff)) = &self.diff else {
            return Vec::new();
        };
        let blacklist = &self.store.read(cx).settings().sync_blacklist;
        diff.iter()
            .filter(|d| self.filters.passes(d, blacklist))
            .collect()
    }

    /// The list: each group's header, then its rows when it's open.
    fn build_items(&mut self, cx: &App) {
        if !matches!(self.diff, Some(Ok(_))) {
            self.items.clear();
            return;
        }
        let show = self.shown(cx);
        let Some(Ok(diff)) = &self.diff else { return };
        let blacklist = &self.store.read(cx).settings().sync_blacklist;
        let mut by_group: Vec<(Group, Vec<usize>)> = Group::ALL.map(|g| (g, Vec::new())).into();
        for (i, d) in diff.iter().enumerate() {
            if self.filters.passes(d, blacklist) && show.has(Group::of(d)) {
                let g = Group::of(d);
                if let Some((_, v)) = by_group.iter_mut().find(|(x, _)| *x == g) {
                    v.push(i);
                }
            }
        }
        let mut items = Vec::new();
        let mut kept = 0;
        self.one_group = by_group.iter().filter(|(_, r)| !r.is_empty()).count() == 1;
        for (g, rows) in by_group {
            if rows.is_empty() {
                continue;
            }
            // Kept-off rows sit behind one footer row until asked for.
            if g == Group::KeptOff && !self.show_kept && show != Show::Kept {
                kept = rows.len();
                continue;
            }
            if g == Group::KeptOff {
                kept = rows.len();
            }
            items.push(Item::Header(g, rows.len()));
            if self.open.contains(&g) {
                items.extend(rows.into_iter().map(Item::Row));
            }
        }
        if kept > 0 && show == Show::All {
            items.push(Item::Kept(kept));
        }
        self.items = items;
    }

    fn render_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(Ok(diff)) = &self.diff else {
            return Vec::new();
        };
        let items: Vec<Item> = self.items[range.start.min(self.items.len())..range.end.min(self.items.len())].to_vec();
        let rows: Vec<(Item, Option<DiffEntry>)> = items
            .iter()
            .map(|i| match i {
                Item::Row(ix) => (*i, diff.get(*ix).cloned()),
                _ => (*i, None),
            })
            .collect();
        // Badges come from the pack's addons: read them all before drawing.
        let tags: Vec<Option<RowTags>> = {
            let store = self.store.read(cx);
            let inst = self
                .server
                .as_ref()
                .and_then(|r| store.server(r))
                .and_then(|s| store.paired(s));
            let now = hopper::time::parse_iso(&hopper::time::now_iso()).unwrap_or(0);
            rows.iter()
                .map(|(_, d)| {
                    let d = d.as_ref()?;
                    let m = d.client.as_ref().or(d.server.as_ref())?;
                    let mut t = RowTags::of(m, inst, store, now);
                    if d.client.is_none() {
                        (t.new, t.added) = (false, false);
                    }
                    Some(t)
                })
                .collect()
        };
        rows.into_iter()
            .zip(tags)
            .map(|((item, d), tags)| match (item, d) {
                (Item::Header(g, n), _) => self.group_header(g, n, cx),
                (Item::Kept(n), _) => self.kept_row(n, cx),
                (Item::Row(_), Some(d)) => {
                    let key = row_key(&d);
                    let el = self.row(&d, tags.unwrap_or_default(), cx);
                    if self.leaving.as_ref().is_some_and(|(_, k)| k.contains(&key)) {
                        widgets::fade_out(ElementId::Name(format!("sync-leave:{key}").into()), el)
                    } else {
                        el
                    }
                }
                _ => div().into_any_element(),
            })
            .collect()
    }

    fn group_header(&self, g: Group, n: usize, cx: &mut Context<Self>) -> AnyElement {
        let open = self.open.contains(&g);
        let keys: Vec<String> = if g.pickable() {
            self.visible(cx)
                .into_iter()
                .filter(|d| Group::of(d) == g)
                .map(row_key)
                .collect()
        } else {
            Vec::new()
        };
        let all = !keys.is_empty() && keys.iter().all(|k| self.selected.contains(k));
        let color = Self::group_color(g, cx);
        let theme = cx.theme();
        h_flex()
            .id(ElementId::Name(format!("group-{g:?}").into()))
            .clickable(cx)
            .h(rems(ROW_H))
            .w_full()
            .items_end()
            .pb_1()
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded(theme.radius_lg)
                    .bg(theme.secondary)
                    .hover(|s| widgets::row_hover(s, cx))
                    .child(
                        Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                            .small()
                            .text_color(theme.muted_foreground),
                    )
                    .child(div().size_2().rounded_full().bg(color))
                    .child(div().text_sm().font_semibold().child(g.title()))
                    .when(!self.one_group, |r| {
                        r.child(div().text_sm().text_color(theme.muted_foreground).child(n.to_string()))
                    })
                    .child(
                        widgets::caption(format!("· {}", g.hint()), cx)
                            .text_xs()
                            .flex_1()
                            .min_w_0(),
                    )
                    .when(!keys.is_empty(), |r| {
                        r.child(
                            div()
                                .id(ElementId::Name(format!("all-{g:?}").into()))
                                .on_click(|_, _, cx| cx.stop_propagation())
                                .child(
                                    Checkbox::new(ElementId::Name(format!("pick-all-{g:?}").into()))
                                        .label("Select all")
                                        .checked(all)
                                        .on_change(cx.listener(move |this, v: &bool, _, cx| {
                                            for k in &keys {
                                                if *v {
                                                    this.selected.insert(k.clone());
                                                } else {
                                                    this.selected.remove(k);
                                                }
                                            }
                                            cx.notify();
                                        })),
                                ),
                        )
                    }),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.open.remove(&g) {
                    this.open.insert(g);
                }
                cx.notify();
            }))
            .into_any_element()
    }

    /// "N mods intentionally not synced": client-only or left out by the
    /// pack's author, folded away until asked for.
    fn kept_row(&self, n: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let shown = self.show_kept;
        h_flex()
            .id("kept-footer")
            .clickable(cx)
            .h(rems(ROW_H))
            .w_full()
            .items_end()
            .pb_1()
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded(theme.radius_lg)
                    .hover(|s| widgets::row_hover(s, cx))
                    .child(Icon::new(IconName::EyeOff).small().text_color(theme.muted_foreground))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "{n} mod{} intentionally not synced (client-only, or left out by the pack)",
                                if n == 1 { "" } else { "s" }
                            )),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .text_color(theme.primary)
                            .child(if shown { "Hide" } else { "Show" }),
                    ),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.show_kept = !this.show_kept;
                cx.notify();
            }))
            .into_any_element()
    }

    fn row(&self, d: &DiffEntry, tags: RowTags, cx: &mut Context<Self>) -> AnyElement {
        let g = Group::of(d);
        let key = row_key(d);
        let id = |p: &str| ElementId::Name(format!("{p}:{key}").into());
        let Some(m) = d.client.as_ref().or(d.server.as_ref()).cloned() else {
            return div().into_any_element();
        };
        let picked = g.pickable() && self.selected.contains(&key);
        let online = self.online(cx);
        let blocked = self.running || (online && !self.allow_online);
        let anyway = self.store.read(cx).settings().sync_anyway.clone();
        let pair = self.pair_header(cx);
        let icon = self.store.read(cx).image(&m.icon).map(Path::to_path_buf);
        let pid = d
            .client
            .as_ref()
            .map(|c| c.project_id)
            .filter(|p| *p != 0)
            .unwrap_or(m.project_id);
        let linked = pid != 0 && self.peek.is_some();
        // The line under the name: why it's kept off, else the file.
        let why = match g {
            Group::KeptOff => d
                .skip_reason
                .clone()
                .or_else(|| d.client.as_ref().and_then(|c| c.wont_sync.clone())),
            Group::ServerOnly if d.status == DiffStatus::ServerStale => {
                Some("An older copy left behind; the pack has this mod under a newer file".into())
            }
            Group::Update if d.status == DiffStatus::ServerNewer => {
                Some("The server has a newer version than the pack".into())
            }
            _ => None,
        };
        // Each side's version, under "Server → Pack" ("–": not there).
        let version = match (g, &d.client, &d.server) {
            (Group::Update, Some(c), Some(s)) => format!("{} → {}", s.version, c.version),
            (Group::Add, Some(c), _) => format!("– → {}", c.version),
            (Group::ServerOnly, _, Some(s)) => format!("{} → –", s.version),
            _ => m.version.clone(),
        };
        // One action per row, named for what it does to the server.
        let compact = self.fit.compact;
        // A drag across row buttons: the ones letting go will press.
        let lit = self.pick.sweeping() == Some(Sweep::Action) && self.swept.contains(&key);
        let act = |key_id: &'static str,
                   label: &'static str,
                   icon: IconName,
                   words: (&'static str, &'static str),
                   a: RowAct,
                   danger: bool| {
            let (d2, pair2, key2) = (d.clone(), pair.clone(), key.clone());
            Button::new(id(key_id))
                .small()
                .when(!lit, |b| b.outline())
                .when(lit && !danger, |b| b.primary())
                .icon(icon)
                // Tight: the icon alone, its words in the tooltip.
                .map(|b| if compact { b.tooltip(label) } else { b.label(label) })
                .when(danger, |b| b.with_variant(ButtonVariant::Danger))
                .disabled(blocked)
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    if drag_pick::click_swallowed(this) {
                        return;
                    }
                    if let Some((s, i)) = &pair2 {
                        this.act_row(key2.clone(), words, rigsync::chip_ops(a, &d2, i, s), cx);
                    }
                }))
        };
        // The rarer direction, server to pack, sits in the row's ⋯.
        let pull: Option<(String, (&'static str, &'static str))> = match g {
            Group::Update if d.client.is_some() && d.server.is_some() => Some((
                format!(
                    "Use server's version ({})",
                    d.server.as_ref().map(|m| m.version.clone()).unwrap_or_default()
                ),
                ("Pulling", "Pulled"),
            )),
            Group::ServerOnly if d.status == DiffStatus::OnlyServer => {
                Some(("Copy to pack".to_string(), ("Copying", "Copied")))
            }
            _ => None,
        };
        let more = pull.map(|(label, words)| {
            let (d2, pair2, key2, view) = (d.clone(), pair.clone(), key.clone(), cx.entity());
            let running = self.running;
            div()
                .id(id("row-more-wrap"))
                // The row's click picks it; this button doesn't.
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(
                    Button::new(id("row-more"))
                        .small()
                        .ghost()
                        .icon(IconName::Ellipsis)
                        .tooltip("More")
                        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                            let (d2, pair2, key2, view, label) =
                                (d2.clone(), pair2.clone(), key2.clone(), view.clone(), label.clone());
                            menu.item(
                                PopupMenuItem::new(label)
                                    .icon(IconName::Download)
                                    .disabled(running)
                                    .on_click(move |_, _, cx| {
                                        if let Some((s, i)) = &pair2 {
                                            let ops = rigsync::chip_ops(RowAct::Pull, &d2, i, s);
                                            view.update(cx, |this, cx| this.act_row(key2.clone(), words, ops, cx));
                                        }
                                    }),
                            )
                        }),
                )
        });
        // Pressed and dragged across rows: the same button on each.
        let sweep = |el: AnyElement, cx: &Context<Self>| -> AnyElement {
            let k = key.clone();
            drag_pick::handle(div().id(id("act-sweep")).flex_none(), cx, move |this, _, cx| this.sweep_down(&k, cx))
                .child(el)
                .into_any_element()
        };
        let theme = cx.theme().clone();
        let action: AnyElement = match g {
            Group::Add => sweep(
                act("Add", "Add to server", IconName::Plus, ("Adding", "Added"), RowAct::Push, false).into_any_element(),
                cx,
            ),
            Group::Update => {
                let lead = if d.status == DiffStatus::ServerNewer {
                    act("Replace", "Replace on server", IconName::ArrowUp, ("Replacing", "Replaced"), RowAct::Push, false)
                        .when(!compact, |b| b.tooltip("The server has a newer version: this puts the pack's on it"))
                } else {
                    act("Update", "Update server", IconName::ArrowUp, ("Updating", "Updated"), RowAct::Push, false)
                        .when(!compact, |b| {
                            b.tooltip("Puts the pack's version on the server (the old jar goes to .hopper-backups)")
                        })
                };
                match pair.as_ref().filter(|_| pid != 0 && d.client.is_some()) {
                    Some((_, inst)) => {
                        let (store, root) = (self.store.clone(), inst.root.clone());
                        widgets::split(
                            sweep(widgets::split_lead(lead).into_any_element(), cx),
                            id("changes"),
                            widgets::Tint::Outline,
                            gpui_kit::component::Size::Small,
                            "What changed",
                            move |_, window, cx| {
                                cx.stop_propagation();
                                crate::versions::open(store.clone(), root.clone(), pid, false, None, window, cx)
                            },
                            cx,
                        )
                        .into_any_element()
                    }
                    None => sweep(lead.into_any_element(), cx),
                }
            }
            Group::ServerOnly => sweep(
                act(
                    "Remove",
                    "Remove from server",
                    IconName::Trash,
                    ("Removing", "Removed"),
                    RowAct::Remove,
                    true,
                )
                .when(!compact, |b| b.tooltip("Moves the jar to the server's .hopper-backups (undo from History)"))
                .into_any_element(),
                cx,
            ),
            Group::KeptOff => h_flex()
                .gap_3()
                .child(sweep(
                    act("Send once", "Send once", IconName::Upload, ("Sending", "Sent"), RowAct::Push, false)
                        .when(!compact, |b| {
                            b.tooltip("Copy it to the server this time; the next comparison still keeps it off")
                        })
                        .into_any_element(),
                    cx,
                ))
                .when(pid != 0, |r| {
                    r.child(
                        Checkbox::new(id("always"))
                            .label("Always")
                            .checked(anyway.contains(&pid))
                            .tooltip("Always send this mod to the server")
                            .on_change(cx.listener(move |this, v: &bool, _, cx| {
                                let on = *v;
                                this.store.update(cx, |s, cx| {
                                    s.update_settings(
                                        |st| {
                                            if on {
                                                st.sync_anyway.insert(pid);
                                            } else {
                                                st.sync_anyway.remove(&pid);
                                            }
                                        },
                                        cx,
                                    )
                                });
                                this.rescan(cx);
                            })),
                    )
                })
                .into_any_element(),
            Group::InSync => widgets::caption(
                if d.client.as_ref().zip(d.server.as_ref()).is_some_and(|(c, s)| {
                    c.file_name.eq_ignore_ascii_case(&s.file_name)
                }) {
                    "Same file"
                } else {
                    "Same version"
                },
                cx,
            )
            .text_xs()
            .into_any_element(),
        };
        let action: AnyElement = if let Some(w) = self.acted.get(&key) {
            widgets::done_mark(id("done"), *w, false, cx).into_any_element()
        } else if let Some((busy, _)) = self.acting.get(&key) {
            Button::new(id("acting"))
                .small()
                .outline()
                .loading(true)
                .label(*busy)
                .disabled(true)
                .into_any_element()
        } else {
            action
        };
        let link = |el: Stateful<Div>, cx: &mut Context<Self>| {
            el.when(linked, |d| {
                d.clickable(cx).on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.open_peek(pid, cx);
                }))
            })
        };
        let primary = theme.primary;
        let thumb = link(
            div()
                .id(id("thumb"))
                .flex_none()
                .when(linked, |d| d.hover(|s| s.opacity(0.8)))
                .child(widgets::thumb(icon.as_deref(), rems(2.), cx)),
            cx,
        );
        let name = link(
            div()
                .id(id("name"))
                .min_w_0()
                .text_sm()
                .font_medium()
                .truncate()
                .when(linked, |d| {
                    d.hover(move |s| s.text_color(primary)).tooltip(|w, cx| {
                        gpui_kit::component::tooltip::Tooltip::new("Details").build(w, cx)
                    })
                })
                .child(m.mod_name.clone()),
            cx,
        );
        let sub = why.clone().unwrap_or_else(|| m.file_name.clone());
        let fit = self.fit;
        let side_icon = fit
            .tags
            .is_none()
            .then(|| crate::side_card::side_of(&tags.env))
            .flatten();
        let k2 = key.clone();
        let pickable = g.pickable();
        h_flex()
            .id(id("sync-row"))
            .w_full()
            .h(rems(ROW_H))
            .px_3()
            .gap_3()
            .border_b_1()
            .border_color(theme.border.opacity(0.4))
            .when(picked, |r| widgets::row_selected(r, cx))
            .when(!picked, |r| r.hover(|s| widgets::row_hover(s, cx)))
            .when(pickable, |r| {
                r.clickable(cx).on_click(
cx.listener(move |this, _, _, cx| {
                    // A drag let go on this row isn't a click on it.
                    if drag_pick::spreading(this) {
                        return;
                    }
                    if !this.selected.remove(&k2) {
                        this.selected.insert(k2.clone());
                    }
                    cx.notify();
                }))
            })
            .child(div().w_6().h_full().flex_none().when(pickable, |c| {
                let key = key.clone();
                c.child(
                    drag_pick::handle(
                        div().id(id("pick-cell")).size_full().flex().items_center().clickable(cx),
                        cx,
                        move |this, e, cx| this.pick_down(&key, e.modifiers.shift, cx),
                    )
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(Checkbox::new(id("pick")).checked(picked)),
                )
            }))
            .child(thumb)
            .child(
                v_flex()
                    .flex_1()
                    .min_w(rems(NAME_MIN))
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_1p5()
                            .child(name)
                            // No tags column: the side as an icon by the name.
                            .when_some(side_icon, |r, (icon, what)| {
                                r.child(
                                    div()
                                        .id(id("side-icon"))
                                        .flex_none()
                                        .text_color(theme.muted_foreground)
                                        .child(Icon::new(icon).xsmall())
                                        .tooltip(move |w, cx| {
                                            gpui_kit::component::tooltip::Tooltip::new(what).build(w, cx)
                                        }),
                                )
                            }),
                    )
                    .child(
                        widgets::caption(sub, cx)
                            .text_xs()
                            .truncate()
                            .when(why.is_none(), |c| c.opacity(0.75)),
                    ),
            )
            .when_some(fit.tags, |r, w| r.child(
                div().w(rems(w)).flex_none().min_w_0().child(widgets::mod_tags(
                    widgets::ModTags {
                        key: &m.file_name,
                        new: tags.new,
                        added: tags.added,
                        // The line under the name already says client-only.
                        env: if why.as_deref().is_some_and(|w| w.to_lowercase().contains("client-only")) {
                            ""
                        } else {
                            &tags.env
                        },
                        categories: &tags.categories,
                        modrinth: tags.modrinth,
                        ..Default::default()
                    },
                    cx,
                )),
            ))
            .child(
                widgets::caption(version, cx)
                    .w(rems(fit.version))
                    .flex_none()
                    .text_xs()
                    .truncate(),
            )
            .child(
                h_flex()
                    .w(rems(fit.action))
                    .flex_none()
                    .gap_1()
                    .justify_end()
                    .child(action)
                    .child(div().w_8().flex_none().children(more)),
            )
            .into_any_element()
    }
}

/// One line of the changeset list.
#[derive(Clone, Copy)]
enum Item {
    /// A group's header and how many rows it has.
    Header(Group, usize),
    /// An index into the comparison.
    Row(usize),
    /// The footer for rows kept off on purpose: how many, and Show / Hide.
    Kept(usize),
}

/// What one side of a row shows as badges, read from the pack's addon.
#[derive(Clone, Default)]
struct RowTags {
    env: String,
    categories: Vec<String>,
    new: bool,
    added: bool,
    modrinth: bool,
}

impl RowTags {
    fn of(m: &ModInfo, inst: Option<&Instance>, store: &Store, now: i64) -> Self {
        let a = inst.and_then(|i| i.addon_by_file(&m.file_name));
        RowTags {
            // The jar's own word first, then the site's tags.
            env: if m.side.is_empty() {
                a.map(|a| a.env.clone()).unwrap_or_default()
            } else {
                m.side.clone()
            },
            categories: a.map(|a| store.categories(a).to_vec()).unwrap_or_default(),
            new: a
                .and_then(|a| hopper::time::parse_iso(&a.date_added))
                .is_some_and(|t| now - t < 3 * 86_400),
            added: inst.is_some_and(|i| i.pack.is_some()) && a.is_some_and(|a| !a.from_pack),
            modrinth: a.is_some_and(|a| !a.mr_project.is_empty()),
        }
    }
}

impl Picks for SyncPanel {
    fn picks(&mut self) -> &mut PickState {
        &mut self.pick
    }

    fn pick_list(&self) -> (UniformListScrollHandle, usize) {
        (self.scroll.clone(), self.items.len())
    }

    fn pick_changed(&mut self, p: &DragPick, cx: &mut Context<Self>) -> SharedString {
        match p.kind {
            Sweep::Tick => {
                self.selected = p.apply(|i| self.line_key(i));
                p.tick_label()
            }
            Sweep::Switch | Sweep::Action => {
                let acts = self.swept_acts(p, cx);
                self.swept = acts.iter().map(|(k, ..)| k.clone()).collect();
                let verb = self.line_act(p.start, cx).map_or("Doing", |(_, w, ..)| w.0);
                DragPick::count_label(verb, "Nothing to do", self.swept.len())
            }
        }
    }

    fn pick_commit(&mut self, p: DragPick, _: &mut Window, cx: &mut Context<Self>) {
        self.swept.clear();
        if p.kind == Sweep::Tick {
            return;
        }
        let Some((s, i)) = self.pair_header(cx) else { return };
        for (key, words, act, d) in self.swept_acts(&p, cx) {
            self.act_row(key, words, rigsync::chip_ops(act, &d, &i, &s), cx);
        }
        cx.notify();
    }
}

impl Render for SyncPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(root) = self.server.clone() else {
            // No servers at all is a different page from "none picked".
            let none = self.store.read(cx).servers().is_empty();
            let store = self.store.clone();
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(if none {
                    widgets::empty_state(
                        IconName::Server,
                        "No servers yet",
                        Some("Host one on this PC, then compare it with its pack here.".into()),
                        cx,
                    )
                    .child(
                        Button::new("sync-host")
                            .small()
                            .primary()
                            .icon(IconName::Plus)
                            .label("Host a server")
                            .on_click(move |_, window, cx| {
                                crate::host_dialog::open(store.clone(), None, window, cx)
                            }),
                    )
                } else {
                    widgets::empty_state(
                        IconName::ArrowLeftRight,
                        "Pick a server",
                        Some("Choose one on the left to compare it with its pack.".into()),
                        cx,
                    )
                })
                .into_any_element();
        };
        let Some((server, inst)) = self.pair_header(cx) else {
            let name = self
                .store
                .read(cx)
                .server(&root)
                .map(|s| s.name.clone())
                .unwrap_or_default();
            let pick = self.pair_picker(&root, "Pair a pack…".into(), None, cx);
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(
                    widgets::empty_state(
                        IconName::Link,
                        format!("{name} has no paired pack"),
                        Some("Pick the instance this server runs, to compare and sync their mods.".into()),
                        cx,
                    )
                    .child(div().w_64().child(pick)),
                )
                .into_any_element();
        };
        self.share_client_mods(cx);
        let side = self.in_instance;
        let header = (!side).then(|| self.header(&server, &inst, cx));
        let sidebar = side.then(|| self.sidebar(&server, &inst, window, cx));
        let crash = self.crash_banner(cx);
        let drift = self.drift_banner(cx);
        // The console is a section where the panel is set up (in an
        // instance; the Servers page has its own).
        let console = side && self.control.read(cx).configured(cx);
        if self.section == Section::Console && !console {
            self.section = Section::Mods;
        }
        let section_ix = match self.section {
            Section::Mods => 0,
            Section::Configs => 1,
            Section::History => 2,
            Section::Console => 3,
        };
        // What each section has left to sync, like the instance tabs' counts.
        let count = |n: usize, cx: &App| {
            (n > 0).then(|| div().text_xs().text_color(cx.theme().muted_foreground).child(n.to_string()))
        };
        let mods_n = count(self.whole(cx).iter().filter(|g| Show::Needs.has(**g)).count(), cx);
        let cfg_n = count(self.configs.read(cx).needs_count().unwrap_or(0), cx);
        let sections = TabBar::new("sync-section")
            .underline()
            .selected_index(section_ix)
            .child(
                Tab::new()
                    .prefix(Icon::new(IconName::Puzzle).small())
                    .label("Mods")
                    .when_some(mods_n, |t, n| t.suffix(n)),
            )
            .child(
                Tab::new()
                    .prefix(Icon::new(IconName::Settings).small())
                    .label("Configs")
                    .when_some(cfg_n, |t, n| t.suffix(n)),
            )
            .child(Tab::new().prefix(Icon::new(IconName::Clock).small()).label("History"))
            .when(console, |t| {
                t.child(Tab::new().prefix(Icon::new(IconName::SquareTerminal).small()).label("Console"))
            })
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                let s = [Section::Mods, Section::Configs, Section::History, Section::Console][(*ix).min(3)];
                this.set_section(s, cx);
            }));
        // The list's width: the work area less the side column and padding.
        let room = self.room.unwrap_or_else(|| window.viewport_size().width / window.rem_size());
        let side_w = if side {
            self.side_panel.layout(self.store.read(cx).settings(), window).width
        } else {
            0.0
        };
        self.fit = Fit::of(room - side_w - if side { 2.5 } else { 3.0 });
        self.build_items(cx);
        let body: AnyElement = match (self.section, &self.diff) {
            (Section::Configs, _) => self.configs.clone().into_any_element(),
            (Section::History, _) => self.history.clone().into_any_element(),
            (Section::Console, _) => self.control.update(cx, |c, cx| c.console_view(window, cx)),
            (Section::Mods, d) => match d {
            None => h_flex()
                .gap_2()
                .child(Spinner::new().small())
                .child(widgets::caption("Scanning mod folders", cx))
                .into_any_element(),
            // The server's folder couldn't be read (often its share is offline).
            Some(Err(e)) => widgets::empty_state(
                IconName::TriangleAlert,
                format!("Can't reach {}", server.name),
                Some(e.clone().into()),
                cx,
            )
            .child(
                Button::new("sync-retry")
                    .small()
                    .outline()
                    .icon(IconName::RefreshCw)
                    .label("Retry")
                    .loading(self.comparing)
                    .on_click(cx.listener(|this, _, _, cx| this.rescan(cx))),
            )
            .into_any_element(),
            Some(Ok(_)) => {
                let count = self.items.len();
                let toolbar = self.toolbar(cx);
                let columns = self.column_header(cx);
                // Beside the sidebar, the apply bar shows once something is picked.
                let footer = (!side || !self.selected_rows(cx).is_empty()).then(|| self.footer(&server, cx));
                let theme = cx.theme();
                let result: Option<AnyElement> = self.result.as_ref().map(|r| match r {
                    Ok(line) => h_flex()
                        .gap_2()
                        .text_sm()
                        .text_color(theme.success)
                        .child(Icon::new(IconName::CircleCheck).small())
                        .child(line.clone())
                        .into_any_element(),
                    Err(failed) => v_flex()
                        .text_sm()
                        .text_color(theme.danger)
                        .children(failed.iter().cloned())
                        .into_any_element(),
                });
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .gap_3()
                    .child(toolbar)
                    .child(v_flex().flex_1().min_h_0().child(columns).child(
                        div()
                            .id("sync-rows-area")
                            .on_drag_move::<PickDrag>(cx.listener(|this, e: &DragMoveEvent<PickDrag>, window, cx| {
                                let row_h = rems(ROW_H).to_pixels(window.rem_size());
                                drag_pick::moved(this, e.event.position.y, row_h, window, cx)
                            }))
                            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, w, cx| drag_pick::released(this, w, cx)))
                            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _, w, cx| drag_pick::released(this, w, cx)))
                            .on_hover(cx.listener(|this, h: &bool, _, _| this.settle.pointer_in = *h))
                            .flex_1()
                            .min_h_0()
                            .relative()
                            .child(
                                uniform_list(
                                    "sync-rows",
                                    count,
                                    cx.processor(|this, range: Range<usize>, _, cx| {
                                        this.render_rows(range, cx)
                                    }),
                                )
                                .track_scroll(&self.scroll)
                                .size_full(),
                            )
                            .vertical_scrollbar(&self.scroll),
                    ))
                    .children(footer)
                    .children(result)
                    .into_any_element()
            }
            },
        };
        if let Some(sidebar) = sidebar {
            return h_flex()
                .size_full()
                .items_stretch()
                .child(sidebar)
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .px_5()
                        .pt_2()
                        .pb_4()
                        .gap_3()
                        .child(sections)
                        .children(crash)
                        .children(drift)
                        .child(v_flex().flex_1().min_h_0().child(body)),
                )
                .into_any_element();
        }
        let pad = !self.in_server_page;
        v_flex()
            .size_full()
            .when(pad, |c| c.px_6().py_5())
            .gap_4()
            .children(header)
            .children(crash)
            .children(drift)
            .child(sections)
            .child(v_flex().flex_1().min_h_0().child(body))
            .into_any_element()
    }
}

/// Whether the server at `root` is up: its panel's state when the panel
/// controls know it (the freshest, and it counts a server still starting),
/// else the ping or the panel's polled stats. The status card and the
/// Configs section both ask this, so they agree.
pub(crate) fn server_online(
    store: &Store,
    control: Option<&crate::server_control::ServerControl>,
    root: &Path,
) -> bool {
    if let Some(up) = control.and_then(|c| c.running()) {
        return up;
    }
    store.status(root).is_some_and(|s| matches!(s, Status::Online { .. }))
        || store.panel_stats(root).is_some_and(|p| p.running || p.starting)
}

/// One line for why the last start crashed ("smallships failed to load"),
/// else the crash's headline.
pub(crate) fn crash_reason(c: &ServerCrash) -> String {
    let Some(h) = c.hits.first() else {
        return c.headline.clone();
    };
    let name = if h.mod_id.is_empty() { &h.file_name } else { &h.mod_id };
    match c.hits.len() {
        1 => format!("{name} failed to load"),
        n => format!("{name} and {} more failed to load", n - 1),
    }
}

/// "1 to push · 1 to update · 1 to remove" for the rows given.
fn action_line<'a>(rows: impl Iterator<Item = &'a DiffEntry>) -> String {
    let (mut push, mut update, mut remove) = (0, 0, 0);
    for d in rows {
        match Group::of(d) {
            Group::Add => push += 1,
            Group::Update => update += 1,
            Group::ServerOnly => remove += 1,
            _ => {}
        }
    }
    [("to push", push), ("to update", update), ("to remove", remove)]
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(w, n)| format!("{n} {w}"))
        .collect::<Vec<_>>()
        .join(" · ")
}
