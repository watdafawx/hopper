//! A pack compared with its rig server: what differs, and pushing, pulling
//! or removing mods. Used by the Server sync page and an instance's Server tab.

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

const ROW_H: f32 = 3.5;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Mods,
    Configs,
    History,
}

pub struct SyncPanel {
    store: Entity<Store>,
    section: Section,
    configs: Entity<crate::configs::ConfigsPanel>,
    history: Entity<crate::configs::HistoryPanel>,
    /// Start / stop / restart through the server's panel, beside the header.
    control: Entity<crate::server_control::ServerControl>,
    /// Inside an instance, whose header already shows the pack.
    in_instance: bool,
    server: Option<PathBuf>,
    diff: Option<Result<Vec<DiffEntry>, String>>,
    note: Option<Annotation>,
    crash: Option<ServerCrash>,
    /// The NeoForge version the server's run.bat starts, read with each
    /// comparison (run.bat may sit on a network share: not per frame).
    server_nf: Option<String>,
    /// NeoForge builds for the pack's Minecraft version, newest first, for
    /// the loader picker: (Minecraft version, builds).
    nf_versions: Option<(String, Result<Vec<String>, String>)>,
    nf_loading: bool,
    /// The sidebar's pick: only this group's rows.
    only: Option<Group>,
    filters: Filters,
    search: Entity<InputState>,
    selected: HashSet<String>,
    /// Groups showing their rows.
    open: HashSet<Group>,
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
    /// The Library's mod panel, opened by a mod's name or icon.
    peek: Option<Entity<crate::browse::BrowseView>>,
    _task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl SyncPanel {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search mods"));
        let subs = vec![
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
        let configs = cx.new(|cx| crate::configs::ConfigsPanel::new(store.clone(), window, cx));
        let history = cx.new(|cx| crate::configs::HistoryPanel::new(window, cx));
        let control =
            cx.new(|cx| crate::server_control::ServerControl::new(store.clone(), window, cx));
        let mut subs = subs;
        subs.push(cx.observe(&control, |_, _, cx| cx.notify()));
        Self {
            store,
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
            server: None,
            diff: None,
            note: None,
            crash: None,
            server_nf: None,
            nf_versions: None,
            nf_loading: false,
            only: None,
            filters: Filters::default(),
            search,
            selected: HashSet::new(),
            open: [Group::Add, Group::Update, Group::ServerOnly, Group::KeptOff].into(),
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
            peek: None,
            _task: None,
            _subscriptions: subs,
        }
    }

    pub fn set_in_instance(&mut self, on: bool) {
        self.in_instance = on;
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

    /// Compares `root`'s server with its pack (again when it is the same, so
    /// coming back to the tab picks up mods changed meanwhile).
    pub fn show(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let r2 = root.clone();
        self.configs.update(cx, |c, cx| c.show(r2.clone(), cx));
        self.history.update(cx, |h, cx| h.show(r2.clone(), cx));
        self.control.update(cx, |c, cx| c.show(r2, cx));
        let moved = self.server.as_ref() != Some(&root);
        if moved {
            self.server = Some(root);
            self.filters = Filters {
                text: self.filters.text.clone(),
                ..Default::default()
            };
            self.result = None;
            self.diff = None;
            self.server_nf = None;
            self.only = None;
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

    /// Up, by ping or by its panel (which also knows a server that is still
    /// starting and doesn't answer pings yet).
    fn online(&self, cx: &App) -> bool {
        let Some(r) = self.server.as_ref() else { return false };
        // The panel's state this view polls is the freshest.
        if let Some(up) = self.control.read(cx).running() {
            return up;
        }
        let store = self.store.read(cx);
        store.status(r).is_some_and(|s| matches!(s, Status::Online { .. }))
            || store.panel_stats(r).is_some_and(|p| p.running || p.starting)
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
                .background_spawn(async move { rigsync::run(ops, &server.root, &inst.root) })
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
                    rigsync::run(ops, &server.root, &inst.root)
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
                .title(format!("Copy this instance's mods to {name}?"))
                .description(body.clone())
                .confirm()
                .ok_text("Update server")
                .on_ok(move |_, _, cx| {
                    let s = store.read(cx);
                    let host = s.settings().server_host.clone();
                    let anyway = s.settings().sync_anyway.clone();
                    let java =
                        hopper::launch::installer_java(Path::new(&s.settings().launcher_dir));
                    let panel = hopper::panel::from_settings(s.settings());
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
                    let panel = hopper::panel::from_settings(s.settings());
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

    /// The Server tab's left column: the server and its controls, whether
    /// the mods match, the loader, which group to show, and the big actions.
    fn sidebar(&self, server: &ServerInfo, inst: &Instance, cx: &mut Context<Self>) -> AnyElement {
        let controls = self
            .control
            .read(cx)
            .configured(cx)
            .then(|| self.control.update(cx, |c, cx| c.controls_compact(cx)));
        let online = self.online(cx);
        let counts: Vec<(Group, usize)> = {
            let groups: Vec<Group> = self.counted(cx).into_iter().map(Group::of).collect();
            Group::ALL.map(|g| (g, groups.iter().filter(|x| **x == g).count())).into()
        };
        let n = |g: Group| counts.iter().find(|(x, _)| *x == g).map_or(0, |(_, n)| *n);
        // Whether the mods match goes by the whole comparison, not the search.
        let all: Vec<(Group, usize)> = {
            let blacklist = &self.store.read(cx).settings().sync_blacklist;
            let unfiltered = Filters::default();
            let groups: Vec<Group> = match &self.diff {
                Some(Ok(d)) => d.iter().filter(|d| unfiltered.passes(d, blacklist)).map(Group::of).collect(),
                _ => Vec::new(),
            };
            Group::ALL.map(|g| (g, groups.iter().filter(|x| **x == g).count())).into()
        };
        let n_all = |g: Group| all.iter().find(|(x, _)| *x == g).map_or(0, |(_, n)| *n);
        let to_change = n_all(Group::Add) + n_all(Group::Update) + n_all(Group::ServerOnly);
        let scanned = matches!(self.diff, Some(Ok(_)));
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
        let heading = |t: &'static str| {
            div()
                .text_xs()
                .font_semibold()
                .text_color(theme.muted_foreground)
                .child(t.to_uppercase())
        };

        // The server, its state and controls.
        let pack_line = match self.note.as_ref().map(|a| (a.server_pack_jars, &a.note)) {
            Some((Some(n), _)) => Some(format!("The pack's server pack lists {n} mods")),
            Some((None, Some(note))) => Some(note.clone()),
            _ => None,
        };
        let server_card = card()
            .child(
                h_flex()
                    .gap_2()
                    .child(Icon::new(IconName::Server).small().text_color(theme.muted_foreground))
                    .child(div().flex_1().min_w_0().font_semibold().truncate().child(server.name.clone()))
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
            .children(controls)
            .when_some(pack_line, |c, t| c.child(widgets::caption(t, cx).text_xs()));

        // Whether the mods match, with the split as a bar.
        let total: usize = counts.iter().map(|(_, n)| n).sum();
        let total_all: usize = all.iter().map(|(_, n)| n).sum();
        let bar = h_flex()
            .h_1p5()
            .w_full()
            .rounded_full()
            .overflow_hidden()
            .bg(theme.muted)
            .children(all.iter().filter(|(_, n)| *n > 0).map(|(g, k)| {
                div()
                    .h_full()
                    .w(relative(*k as f32 / total_all.max(1) as f32))
                    .bg(Self::group_color(*g, cx))
            }));
        let status = if !scanned {
            card()
                .child(
                    h_flex()
                        .gap_2()
                        .child(Spinner::new().small())
                        .child(widgets::caption("Comparing mod folders", cx)),
                )
                .into_any_element()
        } else {
            let (color, icon, title, sub) = if to_change == 0 {
                (
                    theme.success,
                    IconName::Check,
                    "Mods match".to_string(),
                    "Nothing to add, update or remove".to_string(),
                )
            } else {
                let parts: Vec<String> = [Group::Add, Group::Update, Group::ServerOnly]
                    .into_iter()
                    .filter(|g| n_all(*g) > 0)
                    .map(|g| count_words(g, n_all(g)))
                    .collect();
                (
                    theme.primary,
                    IconName::ArrowLeftRight,
                    format!("{to_change} to sync"),
                    parts.join(" · "),
                )
            };
            card()
                .border_color(color.opacity(0.35))
                .bg(color.opacity(0.07))
                .child(
                    h_flex()
                        .gap_2p5()
                        .child(
                            div()
                                .flex_none()
                                .size_8()
                                .rounded_full()
                                .bg(color.opacity(0.16))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(Icon::new(icon).small().text_color(color)),
                        )
                        .child(
                            v_flex()
                                .min_w_0()
                                .child(div().font_semibold().child(title))
                                .child(widgets::caption(sub, cx).text_xs()),
                        ),
                )
                .child(bar)
                .child(
                    h_flex()
                        .gap_3()
                        .flex_wrap()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .children(all.iter().filter(|(_, n)| *n > 0).map(|(g, k)| {
                            h_flex()
                                .gap_1p5()
                                .child(div().size_2().rounded_full().bg(Self::group_color(*g, cx)))
                                .child(count_words(*g, *k))
                        })),
                )
                .when(online, |c| {
                    c.child(
                        h_flex()
                            .gap_1p5()
                            .text_xs()
                            .text_color(theme.warning)
                            .child(Icon::new(IconName::TriangleAlert).xsmall())
                            .child("Running: stop it before syncing"),
                    )
                })
                .into_any_element()
        };

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
                                            .primary()
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
                                        .primary()
                                        .small()
                                        .icon(IconName::ChevronDown)
                                        .rounded_tl(px(0.))
                                        .rounded_bl(px(0.))
                                        .border_l_1()
                                        .border_color(black().opacity(0.2)),
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

        // Which group the list shows.
        let row = |key: &'static str, icon: IconName, color: Hsla, label: &'static str, count: usize, on: bool, pick: Option<Group>| {
            let dim = count == 0 && pick.is_some();
            h_flex()
                .id(key)
                .gap_2()
                .px_2()
                .py_1p5()
                .rounded(theme.radius)
                .text_sm()
                .cursor_pointer()
                .when(on, |r| r.bg(theme.list_active))
                .when(!on, |r| r.hover(|s| s.bg(theme.list_hover)))
                .when(dim, |r| r.opacity(0.45))
                .child(Icon::new(icon).small().text_color(color))
                .child(div().flex_1().text_color(if on { theme.foreground } else { theme.muted_foreground }).child(label))
                .child(
                    div()
                        .font_semibold()
                        .text_color(if count == 0 { theme.muted_foreground } else { color })
                        .child(count.to_string()),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.only = if this.only == pick { None } else { pick };
                    if let Some(g) = pick {
                        this.open.insert(g);
                    }
                    this.scroll.scroll_to_item(0, ScrollStrategy::Top);
                    cx.notify();
                }))
        };
        let label = |g: Group| match g {
            Group::Add => "To add",
            Group::Update => "Updates",
            Group::ServerOnly => "Only on server",
            Group::KeptOff => "Kept off",
            Group::InSync => "In sync",
        };
        let ids = |g: Group| match g {
            Group::Add => "show-add",
            Group::Update => "show-update",
            Group::ServerOnly => "show-only-server",
            Group::KeptOff => "show-kept-off",
            Group::InSync => "show-in-sync",
        };
        let mut rows: Vec<AnyElement> = vec![
            row("show-all", IconName::List, theme.foreground, "Everything", total, self.only.is_none(), None)
                .into_any_element(),
        ];
        for g in Group::ALL {
            let k = n(g);
            let actionable = matches!(g, Group::Add | Group::Update | Group::ServerOnly);
            if k == 0 && actionable && to_change == 0 {
                continue;
            }
            rows.push(
                row(ids(g), Self::group_icon(g), Self::group_color(g, cx), label(g), k, self.only == Some(g), Some(g))
                    .into_any_element(),
            );
        }
        if scanned && to_change == 0 {
            rows.push(
                h_flex()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(IconName::CircleCheck).xsmall())
                    .child("Nothing to add, update or remove")
                    .into_any_element(),
            );
        }
        let show = card().gap_0p5().child(div().pb_1().child(heading("Show"))).children(rows);

        // The big actions, last.
        let cf_pack = inst
            .pack
            .as_ref()
            .is_some_and(|p| p.project_id != 0 && !hopper::modrinth::is_mr(p.project_id));
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
        let copy = Button::new("update-server")
            .small()
            .w_full()
            .icon(IconName::ServerCog)
            .label("Copy my mods to server")
            .map(|b| if to_change > 0 { b.primary() } else { b.ghost() })
            .tooltip("Makes the server's mods match this instance (leaving client-only ones out), and installs the pack's NeoForge on it if needed. The server must be stopped.")
            .disabled(online || self.running)
            .on_click(cx.listener(|this, _, w, cx| this.confirm_update(w, cx)));

        v_flex()
            .id("sync-side")
            .w(rems(18.))
            .flex_none()
            .h_full()
            .gap_3()
            .p_4()
            .border_r_1()
            .border_color(theme.border)
            .overflow_y_scroll()
            .child(server_card)
            .child(status)
            .children(loader)
            .child(show)
            .child(div().flex_1())
            .child(v_flex().gap_1p5().children(pack_btn).child(copy))
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
        let controls = self
            .control
            .read(cx)
            .configured(cx)
            .then(|| self.control.update(cx, |c, cx| c.controls(cx)));
        let stop_where = if controls.is_some() { "stop it here" } else { "stop it in Crafty/MCSS" };
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
            .when(!in_instance, |r| {
                r.child(div().truncate().child(inst.name.clone())).child(
                    Icon::new(IconName::ArrowRight)
                        .small()
                        .text_color(theme.muted_foreground),
                )
            })
            .when(in_instance, |r| {
                r.child(Icon::new(IconName::Server).text_color(theme.muted_foreground))
            })
            .child(div().truncate().child(server.name.clone()));
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_3()
                    .when(!in_instance, |r| r.child(widgets::thumb(icon.as_deref(), rems(2.5), cx)))
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
                    .child({
                        let lead = Button::new("update-server")
                            .primary()
                            .small()
                            .icon(IconName::ServerCog)
                            .label("Copy my mods to server")
                            .tooltip("Makes the server's mods match this instance (leaving client-only ones out), and installs the pack's NeoForge on it if needed. The server must be stopped.")
                            .disabled(online || self.running)
                            .on_click(cx.listener(|this, _, w, cx| this.confirm_update(w, cx)));
                        match inst.pack.as_ref().map(|p| p.project_id) {
                            Some(pid) => {
                                let (store, root) = (self.store.clone(), inst.root.clone());
                                widgets::split(
                                    widgets::split_lead(lead),
                                    "update-server-notes",
                                    widgets::Tint::Primary,
                                    gpui_kit::component::Size::Small,
                                    "Pack versions and changelogs",
                                    move |_, window, cx| {
                                        let upd = store
                                            .read(cx)
                                            .instance(&root)
                                            .and_then(|i| store.read(cx).pack_update(i).cloned());
                                        crate::versions::open(store.clone(), root.clone(), pid, true, upd, window, cx)
                                    },
                                    cx,
                                )
                                .into_any_element()
                            }
                            None => lead.into_any_element(),
                        }
                    })
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

    fn group_icon(g: Group) -> IconName {
        match g {
            Group::Add => IconName::Plus,
            Group::Update => IconName::ArrowUp,
            Group::ServerOnly => IconName::Server,
            Group::KeptOff => IconName::ShieldCheck,
            Group::InSync => IconName::Check,
        }
    }

    /// Search, which side a mod runs on, and pack or added.
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
        h_flex()
            .gap_3()
            .flex_wrap()
            .child(
                div().w_56().child(
                    Input::new(&self.search)
                        .small()
                        .prefix(Icon::new(IconName::Search).small())
                        .cleanable(true),
                ),
            )
            .child(
                TabBar::new("sync-env")
                    .segmented()
                    .small()
                    .selected_index(env_ix)
                    .children(ENVS.iter().map(|(_, l)| Tab::new().label(*l)))
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.filters.env = ENVS[*ix].0;
                        cx.notify();
                    })),
            )
            .child(
                TabBar::new("sync-source")
                    .segmented()
                    .small()
                    .selected_index(src_ix)
                    .children(SOURCES.iter().map(|(_, l)| Tab::new().label(*l)))
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.filters.source = SOURCES[*ix].0;
                        cx.notify();
                    })),
            )
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
    }

    /// A card per group with its count; a click opens the group and
    /// scrolls to it.
    fn summary(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        // The rows the filters show, so a card matches its group's count.
        let groups: Vec<Group> = self.visible(cx).into_iter().map(Group::of).collect();
        h_flex().gap_2().children(Group::ALL.map(|g| {
            let n = groups.iter().filter(|x| **x == g).count();
            let color = Self::group_color(g, cx);
            div()
                .id(ElementId::Name(format!("card-{g:?}").into()))
                .flex_1()
                .min_w_0()
                .px_3()
                .py_2()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .hover(|s| s.bg(theme.list_hover))
                .child(
                    div()
                        .text_xl()
                        .font_weight(FontWeight::BOLD)
                        .text_color(if n == 0 { theme.muted_foreground } else { color })
                        .child(n.to_string()),
                )
                .child(
                    h_flex()
                        .gap_1p5()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(Icon::new(Self::group_icon(g)).xsmall())
                        .child(g.short()),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open.insert(g);
                    this.build_items(cx);
                    if let Some(ix) = this
                        .items
                        .iter()
                        .position(|i| matches!(i, Item::Header(h, _) if *h == g))
                    {
                        this.scroll.scroll_to_item(ix, ScrollStrategy::Top);
                    }
                    cx.notify();
                }))
        }))
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
        let parts: Vec<String> = [
            ("add", count(Group::Add, None)),
            ("update", count(Group::Update, None)),
            ("remove", count(Group::ServerOnly, None)),
        ]
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(w, n)| format!("{w} {n}"))
        .collect();
        let online = self.online(cx);
        let blocked = self.running || (online && !self.allow_online);
        let n_sel = picked.len();
        let theme = cx.theme();
        let text = if parts.is_empty() {
            "Nothing selected. Tick mods, or use Select all on a group.".to_string()
        } else {
            format!("Apply will {} on {}", parts.join(", "), server.name)
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
                    .tooltip("Hide the selected mods from this comparison")
                    .disabled(n_sel == 0)
                    .on_click(cx.listener(|this, _, _, cx| {
                        let keys: Vec<String> = this
                            .selected_rows(cx)
                            .iter()
                            .filter_map(rigsync::blacklist_key)
                            .collect();
                        this.selected.clear();
                        this.store.update(cx, |s, cx| {
                            s.update_settings(|st| st.sync_blacklist.extend(keys), cx)
                        });
                    })),
            )
            .child(
                Button::new("apply")
                    .small()
                    .primary()
                    .icon(IconName::Upload)
                    .label("Apply to server")
                    .tooltip("Replaced and removed jars are kept in .hopper-backups")
                    .disabled(ops.is_empty() || blocked)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let ops = Ops {
                            up: ops.up.clone(),
                            down: ops.down.clone(),
                        };
                        this.selected.clear();
                        this.run(ops, cx)
                    })),
            )
            .into_any_element()
    }

    /// The rows the filters let through.
    fn visible(&self, cx: &App) -> Vec<&DiffEntry> {
        self.counted(cx)
            .into_iter()
            .filter(|d| self.only.is_none_or(|g| Group::of(d) == g))
            .collect()
    }

    /// The rows the search and side filters let through, whatever group the
    /// sidebar picked: what its counts count.
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
        let Some(Ok(diff)) = &self.diff else {
            self.items.clear();
            return;
        };
        let blacklist = &self.store.read(cx).settings().sync_blacklist;
        let mut by_group: Vec<(Group, Vec<usize>)> = Group::ALL.map(|g| (g, Vec::new())).into();
        for (i, d) in diff.iter().enumerate() {
            if self.filters.passes(d, blacklist) && self.only.is_none_or(|g| Group::of(d) == g) {
                let g = Group::of(d);
                if let Some((_, v)) = by_group.iter_mut().find(|(x, _)| *x == g) {
                    v.push(i);
                }
            }
        }
        let mut items = Vec::new();
        for (g, rows) in by_group {
            if rows.is_empty() {
                continue;
            }
            items.push(Item::Header(g, rows.len()));
            if self.open.contains(&g) {
                items.extend(rows.into_iter().map(Item::Row));
            }
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
                Item::Header(..) => (*i, None),
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
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.list_hover))
                    .child(
                        Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                            .small()
                            .text_color(theme.muted_foreground),
                    )
                    .child(div().size_2().rounded_full().bg(color))
                    .child(div().text_sm().font_semibold().child(g.title()))
                    .child(div().text_sm().text_color(theme.muted_foreground).child(n.to_string()))
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
        let version = match (g, &d.client, &d.server) {
            (Group::Update, Some(c), Some(s)) => format!("{} → {}", s.version, c.version),
            _ => m.version.clone(),
        };
        // One action per row, named for what it does.
        let act = |label: &'static str, icon: IconName, a: RowAct, danger: bool| {
            let (d2, pair2, key2) = (d.clone(), pair.clone(), key.clone());
            let words = match label {
                "Add" => ("Adding", "Added"),
                "Replace" => ("Replacing", "Replaced"),
                "Update" => ("Updating", "Updated"),
                "Remove" => ("Removing", "Removed"),
                _ => ("Sending", "Sent"),
            };
            Button::new(id(label))
                .small()
                .outline()
                .icon(icon)
                .label(label)
                .when(danger, |b| b.with_variant(ButtonVariant::Danger))
                .disabled(blocked)
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    if let Some((s, i)) = &pair2 {
                        this.act_row(key2.clone(), words, rigsync::chip_ops(a, &d2, i, s), cx);
                    }
                }))
        };
        let theme = cx.theme().clone();
        let action: AnyElement = match g {
            Group::Add => act("Add", IconName::Plus, RowAct::Push, false).into_any_element(),
            Group::Update => {
                let lead = act(
                    if d.status == DiffStatus::ServerNewer { "Replace" } else { "Update" },
                    IconName::ArrowUp,
                    RowAct::Push,
                    false,
                );
                match pair.as_ref().filter(|_| pid != 0 && d.client.is_some()) {
                    Some((_, inst)) => {
                        let (store, root) = (self.store.clone(), inst.root.clone());
                        widgets::split(
                            widgets::split_lead(lead),
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
                    None => lead.into_any_element(),
                }
            }
            Group::ServerOnly => h_flex()
                .gap_1()
                .when(d.status == DiffStatus::OnlyServer, |r| {
                    let (d2, pair2, key2) = (d.clone(), pair.clone(), key.clone());
                    r.child(
                        Button::new(id("pull"))
                            .small()
                            .ghost()
                            .icon(IconName::Download)
                            .label("Copy to pack")
                            .tooltip("Copy this jar from the server into this instance")
                            .disabled(self.running)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                if let Some((s, i)) = &pair2 {
                                    this.act_row(key2.clone(), ("Copying", "Copied"), rigsync::chip_ops(RowAct::Pull, &d2, i, s), cx);
                                }
                            })),
                    )
                })
                .child(act("Remove", IconName::Trash, RowAct::Remove, true))
                .into_any_element(),
            Group::KeptOff => h_flex()
                .gap_3()
                .child(act("Send once", IconName::Upload, RowAct::Push, false).tooltip(
                    "Copy it to the server this time; the next comparison still keeps it off",
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
                d.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
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
            .when(picked, |r| r.bg(theme.primary.opacity(0.06)))
            .hover(|s| s.bg(theme.list_hover))
            .when(pickable, |r| {
                r.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                    if !this.selected.remove(&k2) {
                        this.selected.insert(k2.clone());
                    }
                    cx.notify();
                }))
            })
            .child(div().w_6().flex_none().when(pickable, |c| {
                let pick_id = id("pick");
                let key = key.clone();
                c.child(
                    Checkbox::new(pick_id)
                        .checked(picked)
                        .on_change(cx.listener(move |this, v: &bool, _, cx| {
                            if *v {
                                this.selected.insert(key.clone());
                            } else {
                                this.selected.remove(&key);
                            }
                            cx.notify();
                        })),
                )
            }))
            .child(thumb)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(name)
                    .child(
                        widgets::caption(sub, cx)
                            .text_xs()
                            .truncate()
                            .when(why.is_none(), |c| c.opacity(0.75)),
                    ),
            )
            .child(
                div().flex_1().min_w_0().child(widgets::mod_tags(
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
            )
            .child(
                widgets::caption(version, cx)
                    .w(rems(10.))
                    .flex_none()
                    .text_xs()
                    .truncate(),
            )
            .child(h_flex().w(rems(14.)).flex_none().justify_end().child(action))
            .into_any_element()
    }
}

/// "1 update", "3 to add", "120 kept off".
fn count_words(g: Group, n: usize) -> String {
    match g {
        Group::Update if n == 1 => "1 update".into(),
        _ => format!("{n} {}", g.short()),
    }
}

/// One line of the changeset list.
#[derive(Clone, Copy)]
enum Item {
    /// A group's header and how many rows it has.
    Header(Group, usize),
    /// An index into the comparison.
    Row(usize),
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

impl Render for SyncPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(root) = self.server.clone() else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(widgets::caption(
                    "Pick a server to compare it with its pack.",
                    cx,
                ))
                .into_any_element();
        };
        let Some((server, inst)) = self.pair_header(cx) else {
            let name = self
                .store
                .read(cx)
                .server(&root)
                .map(|s| s.name.clone())
                .unwrap_or_default();
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(widgets::caption(
                    format!("{name} has no paired pack yet. Pick one in its list entry."),
                    cx,
                ))
                .into_any_element();
        };
        let side = self.in_instance;
        let header = (!side).then(|| self.header(&server, &inst, cx));
        let sidebar = side.then(|| self.sidebar(&server, &inst, cx));
        let crash = self.crash_banner(cx);
        let section_ix = match self.section {
            Section::Mods => 0,
            Section::Configs => 1,
            Section::History => 2,
        };
        let sections = TabBar::new("sync-section")
            .underline()
            .selected_index(section_ix)
            .child(Tab::new().prefix(Icon::new(IconName::Puzzle).small()).label("Mods"))
            .child(Tab::new().prefix(Icon::new(IconName::Settings).small()).label("Configs"))
            .child(Tab::new().prefix(Icon::new(IconName::Clock).small()).label("History"))
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                this.section = [Section::Mods, Section::Configs, Section::History][*ix];
                cx.notify();
            }));
        self.build_items(cx);
        let body: AnyElement = match (self.section, &self.diff) {
            (Section::Configs, _) => self.configs.clone().into_any_element(),
            (Section::History, _) => self.history.clone().into_any_element(),
            (Section::Mods, d) => match d {
            None => h_flex()
                .gap_2()
                .child(Spinner::new().small())
                .child(widgets::caption("Scanning mod folders", cx))
                .into_any_element(),
            Some(Err(e)) => div()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(e.clone())
                .into_any_element(),
            Some(Ok(_)) => {
                let count = self.items.len();
                let toolbar = self.toolbar(cx);
                let summary = (!side).then(|| self.summary(cx));
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
                    .children(summary)
                    .child(toolbar)
                    .child(
                        div()
                            .id("sync-rows-area")
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
                    )
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
                        .child(v_flex().flex_1().min_h_0().child(body)),
                )
                .into_any_element();
        }
        v_flex()
            .size_full()
            .px_6()
            .py_5()
            .gap_4()
            .children(header)
            .children(crash)
            .child(sections)
            .child(v_flex().flex_1().min_h_0().child(body))
            .into_any_element()
    }
}
