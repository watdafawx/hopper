//! A Hopper Server server's Mods tab, like the panel's (Mods.svelte,
//! ModPeek.svelte, ReviewUpdates.svelte, ModBackups.svelte, CfKey.svelte)
//! and laid out like the desktop instance Mods tab: sub-tabs Mods and
//! Backups, filters, rows with icon, chips and a switch, and a details
//! panel for the picked mod (versions, changelog, update, lock, config
//! reset, remove). Unknown jars are identified once per server per run and
//! updates are checked on their own.

use super::{Load, Target, call, get, job, load_state, q};
use crate::drag_pick::{self, DragPick, PickDrag, PickState, Picks, Sweep};
use crate::store::Store;
use crate::widgets::{self, Clickable as _};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::Path;
use std::rc::Rc;

/// The details panel: docked on the right, its width saved.
const PEEK_PANEL: widgets::PanelSpec = widgets::PanelSpec::new("panel.mods.peek", 24., 18., 36.).trailing();

/// A row's height in the list.
const ROW_H: f32 = 3.;

// ---- the panel's answers ----------------------------------------------------

/// A string field (numbers as text), empty when missing.
fn st(v: &Value, k: &str) -> String {
    match &v[k] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn bl(v: &Value, k: &str) -> bool {
    v[k].as_bool().unwrap_or(false)
}

fn arr<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v[k].as_array().map_or(&[], Vec::as_slice)
}

/// An update the panel found (`mods/updates`, and a row's `update`).
#[derive(Clone)]
struct Up {
    project_id: String,
    name: String,
    from: String,
    to: String,
    file_id: String,
}

impl Up {
    fn parse(v: &Value) -> Option<Self> {
        v.is_object().then(|| Up {
            project_id: st(v, "project_id"),
            name: st(v, "name"),
            from: st(v, "from"),
            to: st(v, "to"),
            file_id: st(v, "file_id"),
        })
    }
}

/// One mod of `mods/list`.
#[derive(Clone)]
struct ModRow {
    file: String,
    name: String,
    version: String,
    icon: String,
    enabled: bool,
    /// "client" | "server" | "both" | "".
    side: String,
    /// "modrinth" | "curseforge" | "" (unknown).
    source: String,
    from_pack: bool,
    page: String,
    author: String,
    locked: bool,
    update: Option<Up>,
}

impl ModRow {
    fn parse(v: &Value) -> Self {
        let file = st(v, "file");
        let name = st(v, "name");
        ModRow {
            name: if name.is_empty() { file.clone() } else { name },
            file,
            version: st(v, "version"),
            icon: st(v, "icon"),
            enabled: bl(v, "enabled"),
            side: st(v, "side"),
            source: st(v, "source"),
            from_pack: bl(v, "from_pack"),
            page: st(v, "page"),
            author: st(v, "author"),
            locked: bl(v, "locked"),
            update: Up::parse(&v["update"]),
        }
    }

    fn site(&self) -> &'static str {
        if self.source == "modrinth" { "Modrinth" } else { "CurseForge" }
    }
}

struct ModList {
    game_version: String,
    loader: String,
    pack: Option<String>,
    curseforge: bool,
    mods: Vec<ModRow>,
}

impl ModList {
    fn parse(v: &Value) -> Self {
        ModList {
            game_version: st(v, "game_version"),
            loader: st(v, "loader"),
            pack: v["pack"].as_str().filter(|p| !p.is_empty()).map(str::to_string),
            curseforge: bl(v, "curseforge"),
            mods: arr(v, "mods").iter().map(ModRow::parse).collect(),
        }
    }
}

/// A file of a mod for this server's Minecraft and loader (`mods/versions`).
struct Ver {
    id: String,
    name: String,
    date: String,
    version: String,
    kind: String,
    installed: bool,
}

struct Versions {
    project_id: String,
    list: Vec<Ver>,
}

/// A search hit (`mods/search`).
struct Hit {
    id: String,
    name: String,
    summary: String,
    icon: String,
    author: String,
    installed: bool,
}

/// One set aside in .hopper-backups (`mod-backups`).
struct BackupSet {
    when: i64,
    size: u64,
    files: Vec<(String, u64, String)>,
}

struct Backups {
    sets: Vec<BackupSet>,
    total: u64,
}

impl Backups {
    fn parse(v: &Value) -> Self {
        Backups {
            total: v["total_size"].as_u64().unwrap_or(0),
            sets: arr(v, "sets")
                .iter()
                .map(|s| BackupSet {
                    when: s["when"].as_i64().unwrap_or(0),
                    size: s["size"].as_u64().unwrap_or(0),
                    files: arr(s, "files")
                        .iter()
                        .map(|f| (st(f, "name"), f["size"].as_u64().unwrap_or(0), st(f, "restore_to")))
                        .collect(),
                })
                .collect(),
        }
    }
}

// ---- filters -------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    On,
    Off,
    Yours,
    Client,
    Updates,
    Unknown,
}

const FILTERS: [Filter; 7] =
    [Filter::All, Filter::On, Filter::Off, Filter::Yours, Filter::Client, Filter::Updates, Filter::Unknown];

impl Filter {
    fn label(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::On => "On",
            Filter::Off => "Off",
            Filter::Yours => "Only yours",
            Filter::Client => "Client-only",
            Filter::Updates => "Updates",
            Filter::Unknown => "Unknown",
        }
    }

    fn fits(self, m: &ModRow) -> bool {
        match self {
            Filter::All => true,
            Filter::On => m.enabled,
            Filter::Off => !m.enabled,
            Filter::Yours => !m.source.is_empty() && !m.from_pack,
            Filter::Client => m.side == "client",
            Filter::Updates => m.update.is_some(),
            Filter::Unknown => m.source.is_empty(),
        }
    }
}

/// The picked mod's details.
struct Peek {
    file: String,
    versions: Option<Load<Versions>>,
    /// The version whose changelog shows.
    shown: Option<String>,
    /// That changelog; None while it loads.
    notes: Option<Result<String, String>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sub {
    Mods,
    Backups,
}

pub struct ModsTab {
    store: Entity<Store>,
    target: Option<Target>,
    /// Bumped when the server changes: answers for the old one are dropped.
    epoch: u64,
    sub: Sub,
    data: Load<ModList>,
    backups: Option<Load<Backups>>,
    open_sets: HashSet<i64>,
    filter: Filter,
    query: String,
    search: Entity<InputState>,
    /// Rows shown, as indices into the list's mods.
    shown: Rc<Vec<usize>>,
    scroll: UniformListScrollHandle,
    /// A drag across the switches, while the button is down.
    pick: PickState,
    /// The files a switch drag will switch when let go.
    swept: HashSet<String>,
    updates: Option<Vec<Up>>,
    checking: bool,
    identifying: bool,
    /// A running job's title.
    busy: Option<String>,
    /// The last outcome: (is an error, text).
    note: Option<(bool, String)>,
    /// Servers identified this run (panel + id).
    identified: HashSet<String>,
    peek: Option<Peek>,
    peek_panel: widgets::SidePanel,
    // Add mods
    adding: bool,
    modrinth: bool,
    add_query: Entity<InputState>,
    results: Option<Load<Vec<Hit>>>,
    // Review updates
    reviewing: bool,
    review_picked: HashSet<String>,
    /// Ticks dragged down the review list.
    paint: drag_pick::Paint,
    review_open: Option<String>,
    review_notes: HashMap<String, Option<String>>,
    // CurseForge key
    cf_key: Entity<InputState>,
    cf_saving: bool,
    // Minecraft version and loader
    meta_edit: bool,
    meta: Entity<InputState>,
    _subs: Vec<Subscription>,
}

impl ModsTab {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Filter mods"));
        let add_query = cx.new(|cx| InputState::new(window, cx).placeholder("Search mods"));
        let cf_key = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Paste your CurseForge API key"));
        let meta = cx.new(|cx| InputState::new(window, cx).placeholder("1.21.1 neoforge-21.1.259"));
        let subs = vec![
            // Icons arrive through the store.
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.subscribe_in(&search, window, |this, state, e, _, cx| {
                if matches!(e, InputEvent::Change) {
                    this.query = state.read(cx).value().trim().to_lowercase();
                    this.refilter();
                    cx.notify();
                }
            }),
            cx.subscribe_in(&add_query, window, |this, _, e, _, cx| {
                if matches!(e, InputEvent::PressEnter { .. }) {
                    this.run_search(cx);
                }
            }),
            cx.subscribe_in(&cf_key, window, |this, _, e, window, cx| {
                if matches!(e, InputEvent::PressEnter { .. }) {
                    this.save_cf_key(window, cx);
                }
            }),
            cx.subscribe_in(&meta, window, |this, _, e, _, cx| {
                if matches!(e, InputEvent::PressEnter { .. }) {
                    this.save_meta(cx);
                }
            }),
        ];
        Self {
            store,
            target: None,
            epoch: 0,
            sub: Sub::Mods,
            data: Load::Waiting,
            backups: None,
            open_sets: HashSet::new(),
            filter: Filter::All,
            query: String::new(),
            search,
            shown: Rc::default(),
            scroll: UniformListScrollHandle::new(),
            pick: PickState::default(),
            swept: HashSet::new(),
            updates: None,
            checking: false,
            identifying: false,
            busy: None,
            note: None,
            identified: HashSet::new(),
            peek: None,
            peek_panel: widgets::SidePanel::new(PEEK_PANEL),
            adding: false,
            modrinth: true,
            add_query,
            results: None,
            reviewing: false,
            review_picked: HashSet::new(),
            paint: Default::default(),
            review_open: None,
            review_notes: HashMap::new(),
            cf_key,
            cf_saving: false,
            meta_edit: false,
            meta,
            _subs: subs,
        }
    }

    /// Shows `target`'s Mods (again when it's the same server: a refresh).
    pub fn show(&mut self, target: Target, _window: &mut Window, cx: &mut Context<Self>) {
        let same = self.target.as_ref() == Some(&target);
        self.target = Some(target);
        if !same {
            self.epoch += 1;
            self.data = Load::Waiting;
            self.backups = None;
            self.open_sets.clear();
            self.filter = Filter::All;
            self.updates = None;
            self.checking = false;
            self.identifying = false;
            self.busy = None;
            self.note = None;
            self.peek = None;
            self.adding = false;
            self.results = None;
            self.reviewing = false;
            self.meta_edit = false;
            self.shown = Rc::default();
        }
        self.load(true, cx);
        self.load_backups(cx);
        cx.notify();
    }

    fn say(&mut self, text: impl Into<String>) {
        self.note = Some((false, text.into()));
    }

    fn fail(&mut self, text: impl Into<String>) {
        self.note = Some((true, text.into()));
    }

    /// Runs `done` only while the same server is shown.
    fn fresh<T>(
        &self,
        done: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
    ) -> impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static {
        let epoch = self.epoch;
        move |this: &mut Self, r: T, cx: &mut Context<Self>| {
            if this.epoch == epoch {
                done(this, r, cx)
            }
        }
    }

    fn mods(&self) -> &[ModRow] {
        self.data.ready().map_or(&[], |d| d.mods.as_slice())
    }

    fn picked_mod(&self) -> Option<&ModRow> {
        let p = self.peek.as_ref()?;
        self.mods().iter().find(|m| m.file == p.file)
    }

    fn refilter(&mut self) {
        let (f, q) = (self.filter, self.query.clone());
        self.shown = Rc::new(
            self.mods()
                .iter()
                .enumerate()
                .filter(|(_, m)| f.fits(m))
                .filter(|(_, m)| {
                    hopper::fuzzy::words_match(&q, &[&m.name, &m.file, &m.author])
                })
                .map(|(i, _)| i)
                .collect(),
        );
    }

    // ---- loading --------------------------------------------------------------

    /// The mod list; `first`: the tab just opened, so identify unknown jars
    /// (once per server per run) or check for updates.
    fn load(&mut self, first: bool, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let key = format!("{}/{}", t.remote.panel, t.id());
        get(cx, &t, t.sp("/mods/list"), self.fresh(move |this: &mut Self, r: Result<Value, String>, cx| {
            match r {
                Ok(v) => this.data = Load::Ready(ModList::parse(&v)),
                // Keep what's shown when a refresh fails.
                Err(e) if this.data.ready().is_some() => this.fail(e),
                Err(e) => this.data = Load::Failed(e),
            }
            this.refilter();
            let icons: Vec<String> = this.mods().iter().map(|m| m.icon.clone()).filter(|i| !i.is_empty()).collect();
            this.store.update(cx, |s, _| icons.iter().for_each(|i| s.want_image(i)));
            if this.peek.is_some() && this.picked_mod().is_none() {
                this.peek = None;
            }
            if first {
                let unknown = this.mods().iter().any(|m| m.source.is_empty());
                if unknown && this.identified.insert(key) {
                    this.identify(true, cx);
                } else if this.data.ready().is_some() {
                    this.check_updates(false, cx);
                }
            }
        }));
    }

    fn load_backups(&mut self, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        get(cx, &t, t.sp("/mod-backups"), self.fresh(|this: &mut Self, r: Result<Value, String>, _| {
            this.backups = Some(Load::from(r.map(|v| Backups::parse(&v))));
        }));
    }

    /// Looks unknown jars up on CurseForge and Modrinth.
    fn identify(&mut self, auto: bool, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        if self.identifying {
            return;
        }
        self.identifying = true;
        job(cx, &self.store, &t, "Identifying the server's mods", t.sp("/mods/identify"), json!({}), self.fresh(
            move |this: &mut Self, r: Result<Value, String>, cx| {
                this.identifying = false;
                match r {
                    Ok(v) => {
                        let (n, left) = (v["identified"].as_u64().unwrap_or(0), v["unknown"].as_u64().unwrap_or(0));
                        if !auto || n > 0 {
                            this.say(if left > 0 { format!("Identified {n}; {left} still unknown") } else { format!("Identified {n}") });
                        }
                    }
                    Err(e) if !auto => this.fail(e),
                    Err(_) => {}
                }
                this.load(false, cx);
                this.check_updates(false, cx);
            },
        ));
        cx.notify();
    }

    fn check_updates(&mut self, force: bool, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        self.checking = true;
        let path = t.sp(if force { "/mods/updates?force=1" } else { "/mods/updates" });
        get(cx, &t, path, self.fresh(move |this: &mut Self, r: Result<Value, String>, cx| {
            this.checking = false;
            match r {
                Ok(v) => {
                    let ups: Vec<Up> = arr(&v, "updates").iter().filter_map(Up::parse).collect();
                    if force && ups.is_empty() {
                        this.say("Every mod added on top of the pack is up to date.");
                    }
                    this.updates = Some(ups);
                    this.load(false, cx);
                }
                Err(e) if force => this.fail(e),
                Err(_) => {}
            }
        }));
        cx.notify();
    }

    // ---- changes ----------------------------------------------------------------

    /// Switches several mods at once (a drag across the switches).
    fn toggle_many(&mut self, files: Vec<String>, on: bool, cx: &mut Context<Self>) {
        if files.len() == 1 {
            return self.toggle(files[0].clone(), on, cx);
        }
        let Some(t) = self.target.clone() else { return };
        if let Load::Ready(d) = &mut self.data {
            for m in d.mods.iter_mut().filter(|m| files.contains(&m.file)) {
                m.enabled = on;
            }
        }
        let path = t.sp("/mods/toggle-many");
        call(cx, &t, move |c| c.post(&path, json!({ "files": files, "enabled": on })), self.fresh(
            |this: &mut Self, r: Result<Value, String>, cx| {
                match r {
                    Ok(v) if bl(&v, "restart_needed") => this.say("Restart the server to apply."),
                    Ok(_) => {}
                    Err(e) => this.fail(e),
                }
                this.load(false, cx);
            },
        ));
        cx.notify();
    }

    /// Row `ix`'s file and whether it's on.
    fn row_switch(&self, ix: usize) -> Option<(String, bool)> {
        let m = self.mods().get(*self.shown.get(ix)?)?;
        Some((m.file.clone(), m.enabled))
    }

    /// The rows a switch drag crosses whose switch it changes.
    fn swept_files(&self, p: &DragPick) -> Vec<String> {
        p.rows()
            .filter_map(|i| self.row_switch(i))
            .filter(|(_, on)| *on != p.on)
            .map(|(f, _)| f)
            .collect()
    }

    fn toggle(&mut self, file: String, on: bool, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        // Shown at once; the reload says how it went.
        if let Load::Ready(d) = &mut self.data
            && let Some(m) = d.mods.iter_mut().find(|m| m.file == file)
        {
            m.enabled = on;
        }
        let path = t.sp("/mods/toggle");
        call(cx, &t, move |c| c.post(&path, json!({ "file": file, "enabled": on })), self.fresh(
            |this: &mut Self, r: Result<Value, String>, cx| {
                match r {
                    Ok(v) if bl(&v, "restart_needed") => this.say("Restart the server to apply."),
                    Ok(_) => {}
                    Err(e) => this.fail(e),
                }
                this.load(false, cx);
            },
        ));
        cx.notify();
    }

    fn run_search(&mut self, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let text = self.add_query.read(cx).value().trim().to_string();
        let source = if self.modrinth { "modrinth" } else { "curseforge" };
        self.results = Some(Load::Waiting);
        let path = t.sp(&format!("/mods/search?q={}&source={source}", q(&text)));
        get(cx, &t, path, self.fresh(|this: &mut Self, r: Result<Value, String>, cx| {
            let hits = r.map(|v| {
                arr(&v, "results")
                    .iter()
                    .map(|h| Hit {
                        id: st(h, "id"),
                        name: st(h, "name"),
                        summary: st(h, "summary"),
                        icon: st(h, "icon"),
                        author: st(h, "author"),
                        installed: bl(h, "installed"),
                    })
                    .collect::<Vec<_>>()
            });
            if let Ok(h) = &hits {
                let icons: Vec<String> = h.iter().map(|h| h.icon.clone()).filter(|i| !i.is_empty()).collect();
                this.store.update(cx, |s, _| icons.iter().for_each(|i| s.want_image(i)));
            }
            this.results = Some(Load::from(hits));
        }));
        cx.notify();
    }

    fn install(&mut self, id: String, name: String, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        self.busy = Some(format!("Adding {name}"));
        self.note = None;
        let body = json!({ "project_id": id });
        job(cx, &self.store, &t, format!("Adding {name} to the server"), t.sp("/mods/install"), body, self.fresh(
            move |this: &mut Self, r: Result<Value, String>, cx| {
                this.busy = None;
                match r {
                    Ok(v) => {
                        let added: Vec<String> =
                            arr(&v, "added").iter().filter_map(|a| a.as_str().map(str::to_string)).collect();
                        this.say(format!("Added {}. If the server is running, restart it to load it.", added.join(", ")));
                        if let Some(Load::Ready(hits)) = &mut this.results
                            && let Some(h) = hits.iter_mut().find(|h| h.id == id)
                        {
                            h.installed = true;
                        }
                    }
                    Err(e) => this.fail(e),
                }
                this.load(false, cx);
            },
        ));
        cx.notify();
    }

    fn apply_updates(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        if ids.is_empty() {
            return;
        }
        self.busy = Some("Updating".into());
        self.note = None;
        let title = if ids.len() == 1 { "Updating a server mod".to_string() } else { format!("Updating {} server mods", ids.len()) };
        job(cx, &self.store, &t, title, t.sp("/mods/update"), json!({ "project_ids": ids }), self.fresh(
            |this: &mut Self, r: Result<Value, String>, cx| {
                this.busy = None;
                match r {
                    Ok(v) => {
                        let n = arr(&v, "updated").len();
                        this.say(format!(
                            "Updated {n}. If the server is running, restart it to load {}. The old jars are in Backups.",
                            if n == 1 { "it" } else { "them" }
                        ));
                        this.reviewing = false;
                        this.updates = None;
                        this.check_updates(false, cx);
                        this.load_backups(cx);
                    }
                    Err(e) => this.fail(e),
                }
                this.load(false, cx);
            },
        ));
        cx.notify();
    }

    fn save_cf_key(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let key = self.cf_key.read(cx).value().trim().to_string();
        if key.is_empty() || self.cf_saving {
            return;
        }
        self.cf_saving = true;
        call(cx, &t, move |c| c.put("/settings/curseforge", json!({ "key": key })), self.fresh(
            |this: &mut Self, r: Result<Value, String>, cx| {
                this.cf_saving = false;
                match r {
                    Ok(_) => {
                        this.say("CurseForge key saved: CurseForge search and updates work now.");
                        this.load(false, cx);
                        this.check_updates(true, cx);
                    }
                    Err(e) => this.fail(e),
                }
            },
        ));
        self.cf_key.update(cx, |s, cx| s.set_value("", _window, cx));
        cx.notify();
    }

    fn edit_meta(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = self.data.ready().map(|d| format!("{} {}", d.game_version, d.loader).trim().to_string()).unwrap_or_default();
        self.meta.update(cx, |s, cx| s.set_value(now, window, cx));
        self.meta_edit = !self.meta_edit;
        cx.notify();
    }

    fn save_meta(&mut self, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let text = self.meta.read(cx).value().trim().to_string();
        let mut parts = text.split_whitespace();
        let Some(game_version) = parts.next().map(str::to_string) else { return };
        let loader = parts.next().unwrap_or("").to_string();
        let path = t.sp("/mods/meta");
        self.meta_edit = false;
        call(cx, &t, move |c| c.put(&path, json!({ "game_version": game_version, "loader": loader })), self.fresh(
            |this: &mut Self, r: Result<Value, String>, cx| {
                if let Err(e) = r {
                    this.fail(e);
                }
                this.load(false, cx);
            },
        ));
        cx.notify();
    }

    // ---- the details panel ----------------------------------------------------

    fn pick(&mut self, file: String, cx: &mut Context<Self>) {
        if self.peek.as_ref().is_some_and(|p| p.file == file) {
            self.peek = None;
            cx.notify();
            return;
        }
        let known = self.mods().iter().find(|m| m.file == file).is_some_and(|m| !m.source.is_empty());
        self.peek = Some(Peek { file, versions: None, shown: None, notes: None });
        if known {
            self.load_versions(cx);
        }
        cx.notify();
    }

    fn load_versions(&mut self, cx: &mut Context<Self>) {
        let (Some(t), Some(p)) = (self.target.clone(), self.peek.as_mut()) else { return };
        p.versions = Some(Load::Waiting);
        p.shown = None;
        p.notes = None;
        let file = p.file.clone();
        let path = t.sp(&format!("/mods/versions?file={}", q(&file)));
        get(cx, &t, path, self.fresh(move |this: &mut Self, r: Result<Value, String>, cx| {
            let Some(p) = this.peek.as_mut().filter(|p| p.file == file) else { return };
            let v = r.map(|v| Versions {
                project_id: st(&v, "project_id"),
                list: arr(&v, "versions")
                    .iter()
                    .map(|x| Ver {
                        id: st(x, "id"),
                        name: st(x, "name"),
                        date: st(x, "date"),
                        version: st(x, "version"),
                        kind: st(x, "type"),
                        installed: bl(x, "installed"),
                    })
                    .collect(),
            });
            // The first version not installed, else the installed one.
            let first = v.as_ref().ok().and_then(|v| v.list.iter().find(|x| !x.installed).or(v.list.first()).map(|x| x.id.clone()));
            p.versions = Some(Load::from(v));
            if let Some(id) = first {
                this.show_notes(id, cx);
            }
        }));
    }

    fn show_notes(&mut self, file_id: String, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let Some(p) = self.peek.as_mut() else { return };
        let Some(project) = p.versions.as_ref().and_then(|v| v.ready()).map(|v| v.project_id.clone()) else { return };
        p.shown = Some(file_id.clone());
        p.notes = None;
        let path = t.sp(&format!("/mods/changelog?project={}&file={}", q(&project), q(&file_id)));
        get(cx, &t, path, self.fresh(move |this: &mut Self, r: Result<Value, String>, _| {
            if let Some(p) = this.peek.as_mut().filter(|p| p.shown.as_deref() == Some(&file_id)) {
                p.notes = Some(r.map(|v| st(&v, "markdown")));
            }
        }));
        cx.notify();
    }

    /// Installs version `file_id` of the picked mod (update or roll back).
    fn set_version(&mut self, file_id: String, label: String, verb: &str, cx: &mut Context<Self>) {
        let (Some(t), Some(m)) = (self.target.clone(), self.picked_mod().cloned()) else { return };
        self.busy = Some(format!("{verb} {}", m.name));
        self.note = None;
        let body = json!({ "file": m.file, "file_id": file_id });
        job(cx, &self.store, &t, format!("{verb} {} on the server", m.name), t.sp("/mods/version"), body, self.fresh(
            move |this: &mut Self, r: Result<Value, String>, cx| {
                this.busy = None;
                match r {
                    Ok(v) => {
                        this.say(format!(
                            "{} is {label} now. If the server is running, restart it to load it. The old jar is in Backups.",
                            m.name
                        ));
                        // The jar has a new name: follow it.
                        let new = st(&v, "file");
                        if let Some(p) = this.peek.as_mut().filter(|_| !new.is_empty()) {
                            p.file = new;
                        }
                        this.updates = None;
                        this.check_updates(false, cx);
                        this.load_backups(cx);
                        if this.peek.is_some() {
                            this.load_versions(cx);
                        }
                    }
                    Err(e) => this.fail(e),
                }
                this.load(false, cx);
            },
        ));
        cx.notify();
    }

    fn lock(&mut self, cx: &mut Context<Self>) {
        let (Some(t), Some(m)) = (self.target.clone(), self.picked_mod().cloned()) else { return };
        let path = t.sp("/mods/lock");
        let body = json!({ "file": m.file, "locked": !m.locked });
        call(cx, &t, move |c| c.post(&path, body), self.fresh(move |this: &mut Self, r: Result<Value, String>, cx| {
            match r {
                Ok(_) if m.locked => this.say(format!("{} gets updates again.", m.name)),
                Ok(_) => this.say(format!("{} stays on this version: update checks skip it.", m.name)),
                Err(e) => this.fail(e),
            }
            this.load(false, cx);
        }));
    }

    /// Lists the picked mod's config files, then asks before moving them
    /// to .hopper-backups.
    fn reset_config(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(t), Some(m)) = (self.target.clone(), self.picked_mod().cloned()) else { return };
        let path = t.sp(&format!("/mods/config?file={}", q(&m.file)));
        let (client, epoch) = (t.client.clone(), self.epoch);
        cx.spawn_in(window, async move |this, cx| {
            let r = cx.background_spawn(async move { client.get(&path).map_err(|e| format!("{e:#}")) }).await;
            this.update_in(cx, |this, window, cx| {
                if this.epoch != epoch {
                    return;
                }
                let files: Vec<String> = match r {
                    Ok(v) => arr(&v, "files").iter().filter_map(|f| f.as_str().map(str::to_string)).collect(),
                    Err(e) => {
                        this.fail(e);
                        cx.notify();
                        return;
                    }
                };
                if files.is_empty() {
                    this.say(format!("No config files named after {} in config/.", m.name));
                    cx.notify();
                    return;
                }
                let body = format!(
                    "{} to .hopper-backups: {}. The mod writes fresh ones on the next start.",
                    if files.len() == 1 { "This file goes".to_string() } else { format!("These {} files go", files.len()) },
                    files.join(", ")
                );
                let file = m.file.clone();
                this.confirm(format!("Reset {}'s config?", m.name), body, "Reset config", window, cx, move |this, cx| {
                    let Some(t) = this.target.clone() else { return };
                    let path = t.sp("/mods/config-reset");
                    let body = json!({ "file": file });
                    call(cx, &t, move |c| c.post(&path, body), this.fresh(|this: &mut Self, r: Result<Value, String>, _| {
                        match r {
                            Ok(v) => {
                                let n = v["reset"].as_u64().unwrap_or(0);
                                this.say(format!(
                                    "Moved {n} file{} to .hopper-backups.{}",
                                    if n == 1 { "" } else { "s" },
                                    if bl(&v, "restart_needed") { " Restart the server to apply." } else { "" }
                                ));
                            }
                            Err(e) => this.fail(e),
                        }
                    }));
                });
            })
            .ok();
        })
        .detach();
    }

    fn remove(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(m) = self.picked_mod().cloned() else { return };
        let file = m.file.clone();
        self.confirm(
            format!("Remove {}?", m.name),
            "It's kept in .hopper-backups (Mods › Backups).".into(),
            "Remove",
            window,
            cx,
            move |this, cx| {
                let Some(t) = this.target.clone() else { return };
                let path = t.sp("/mods/remove");
                let body = json!({ "file": file });
                call(cx, &t, move |c| c.post(&path, body), this.fresh(|this: &mut Self, r: Result<Value, String>, cx| {
                    match r {
                        Ok(v) => {
                            this.peek = None;
                            if bl(&v, "restart_needed") {
                                this.say("Removed. Restart the server to apply.");
                            }
                            this.updates = None;
                            this.check_updates(false, cx);
                            this.load_backups(cx);
                        }
                        Err(e) => this.fail(e),
                    }
                    this.load(false, cx);
                }));
            },
        );
    }

    // ---- backups -----------------------------------------------------------------

    /// Puts back set `when` (only `name` when given); `what` names it in
    /// the question.
    fn restore(&mut self, when: i64, name: Option<String>, what: String, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm(
            format!("Put back {what}?"),
            "A mod's current jar moves to Backups first, so this can be undone the same way.".into(),
            "Restore",
            window,
            cx,
            move |this, cx| {
                let Some(t) = this.target.clone() else { return };
                this.busy = Some("Restoring".into());
                let path = t.sp("/mod-backups/restore");
                let body = json!({ "when": when, "name": name });
                call(cx, &t, move |c| c.post(&path, body), this.fresh(|this: &mut Self, r: Result<Value, String>, cx| {
                    this.busy = None;
                    match r {
                        Ok(v) => {
                            let back: Vec<String> =
                                arr(&v, "restored").iter().filter_map(|a| a.as_str().map(str::to_string)).collect();
                            this.say(format!(
                                "Put back {}.{}",
                                back.join(", "),
                                if bl(&v, "restart_needed") { " Restart the server to load it." } else { "" }
                            ));
                            this.updates = None;
                            this.check_updates(false, cx);
                        }
                        Err(e) => this.fail(e),
                    }
                    this.load_backups(cx);
                    this.load(false, cx);
                }));
            },
        );
    }

    fn delete_sets(&mut self, whens: Vec<i64>, title: String, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm(title, "They go to the Recycle Bin on the server PC.".into(), "Delete", window, cx, move |this, cx| {
            let Some(t) = this.target.clone() else { return };
            this.busy = Some("Deleting backups".into());
            let path = t.sp("/mod-backups/delete");
            let whens = whens.clone();
            call(
                cx,
                &t,
                move |c| {
                    for w in whens {
                        c.post(&path, json!({ "when": w }))?;
                    }
                    Ok(())
                },
                this.fresh(|this: &mut Self, r: Result<(), String>, cx| {
                    this.busy = None;
                    if let Err(e) = r {
                        this.fail(e);
                    }
                    this.load_backups(cx);
                }),
            );
        });
    }

    // ---- review updates ------------------------------------------------------

    fn open_review(&mut self, cx: &mut Context<Self>) {
        self.reviewing = true;
        self.review_open = None;
        self.review_picked = self.updates.iter().flatten().map(|u| u.project_id.clone()).collect();
        cx.notify();
    }

    fn review_notes(&mut self, u: &Up, cx: &mut Context<Self>) {
        let pid = u.project_id.clone();
        if self.review_open.as_deref() == Some(&pid) {
            self.review_open = None;
            cx.notify();
            return;
        }
        self.review_open = Some(pid.clone());
        if !self.review_notes.contains_key(&pid)
            && let Some(t) = self.target.clone()
        {
            self.review_notes.insert(pid.clone(), None);
            let path = t.sp(&format!("/mods/changelog?project={}&file={}", q(&pid), q(&u.file_id)));
            get(cx, &t, path, self.fresh(move |this: &mut Self, r: Result<Value, String>, _| {
                this.review_notes.insert(pid, Some(r.map(|v| st(&v, "markdown")).unwrap_or_default()));
            }));
        }
        cx.notify();
    }

    // ---- confirm ------------------------------------------------------------------

    /// A small "are you sure" with a red OK that runs `then` on this view.
    fn confirm(
        &self,
        title: String,
        body: String,
        ok: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) {
        let view = cx.entity();
        super::confirm(
            window,
            cx,
            title,
            body,
            ok,
            true,
            move |_, cx| {
                view.update(cx, |v, cx| {
                    then(v, cx);
                    cx.notify();
                })
            },
            None,
        );
    }

    // ---- rendering ------------------------------------------------------------

    fn render_rows(&mut self, range: Range<usize>, _window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let shown = self.shown.clone();
        let picked = self.peek.as_ref().map(|p| p.file.clone());
        let mut out = Vec::new();
        for ix in range {
            let Some(m) = shown.get(ix).and_then(|i| self.mods().get(*i)).cloned() else { continue };
            let icon = self.store.read(cx).image(&m.icon).map(Path::to_path_buf);
            out.push(self.mod_row(&m, icon.as_deref(), picked.as_deref() == Some(m.file.as_str()), cx));
        }
        out
    }

    fn mod_row(&self, m: &ModRow, icon: Option<&Path>, sel: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (primary, warn, muted) = (theme.primary, theme.warning, theme.muted_foreground);
        let file = m.file.clone();
        let id = |p: &str| ElementId::Name(format!("{p}:{file}").into());
        let sub = if m.source.is_empty() {
            m.file.clone()
        } else {
            let mut s = String::new();
            if !m.version.is_empty() {
                s.push_str(&m.version);
                s.push_str(" · ");
            }
            s.push_str(m.site());
            if !m.author.is_empty() {
                s.push_str(" · ");
                s.push_str(&m.author);
            }
            s
        };
        let (f_pick, f_on, on) = (file.clone(), file.clone(), m.enabled);
        let f_sweep = file.clone();
        // A drag across switches: the state letting go will set.
        let swept = self.pick.sweeping() == Some(Sweep::Switch) && self.swept.contains(&file);
        h_flex()
            .id(id("mod"))
            .w_full()
            .h(rems(ROW_H))
            .px_2()
            .gap_3()
            .rounded(theme.radius)
            .clickable(cx)
            .hover(|s| widgets::row_hover(s, cx))
            .when(sel, |d| widgets::row_selected(d, cx))
            .on_click(cx.listener(move |this, _, _, cx| {
                // A drag let go on this row isn't a click on it.
                if !drag_pick::spreading(this) {
                    this.pick(f_pick.clone(), cx)
                }
            }))
            .child(div().when(!on, |d| d.opacity(0.55)).child(widgets::thumb(icon, rems(1.75), cx)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .when(!on, |d| d.opacity(0.55))
                    .child(
                        h_flex()
                            .gap_1p5()
                            .min_w_0()
                            .child(div().min_w_0().truncate().text_sm().font_medium().child(m.name.clone()))
                            .when_some(m.update.as_ref(), |r, u| r.child(widgets::chip(primary).child(format!("↑ {}", u.to))))
                            .when(m.locked, |r| {
                                r.child(
                                    widgets::chip(muted)
                                        .child(h_flex().gap_1().child(Icon::new(IconName::Lock).xsmall()).child("Locked")),
                                )
                            })
                            .when(m.side == "client", |r| r.child(widgets::chip(warn).child("Client-only")))
                            .when(m.from_pack, |r| r.child(widgets::chip(muted).child("Pack")))
                            .when(m.source.is_empty(), |r| r.child(widgets::chip(muted).child("Unknown"))),
                    )
                    .child(widgets::caption(sub, cx).text_xs()),
            )
            .child(
                // The switch's click stays off the row.
                // Pressed and dragged across rows: their switches too.
                drag_pick::handle(div().id(id("sw")).flex_none(), cx, move |this, _, cx| {
                    let Some(ix) = this.shown.iter().position(|&i| this.mods().get(i).is_some_and(|m| m.file == f_sweep)) else {
                        return;
                    };
                    this.swept.clear();
                    drag_pick::start(this, DragPick::sweep(ix, Sweep::Switch, !on), cx);
                })
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(
                    Switch::new(id("on"))
                        .small()
                        .checked(on != swept)
                        .tooltip(if on { "Turn off" } else { "Turn on" })
                        .on_change(cx.listener(move |this, v: &bool, _, cx| {
                            if drag_pick::click_swallowed(this) {
                                return;
                            }
                            this.toggle(f_on.clone(), *v, cx)
                        })),
                ),
            )
            .into_any_element()
    }

    fn sub_tabs(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let n_mods = self.data.ready().map(|d| d.mods.len());
        let n_sets = self.backups.as_ref().and_then(|b| b.ready()).map(|b| b.sets.len()).filter(|n| *n > 0);
        let theme = cx.theme();
        let meta = self.data.ready().map(|d| {
            let text = format!(
                "{} · {}",
                if d.game_version.is_empty() { "version?" } else { &d.game_version },
                if d.loader.is_empty() { "loader?" } else { &d.loader }
            );
            div()
                .id("mods-meta")
                .clickable(cx)
                .rounded(px(crate::theme::RADIUS))
                .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("Minecraft version and loader: change").build(w, cx))
                .on_click(cx.listener(|this, _, window, cx| this.edit_meta(window, cx)))
                .child(
                    widgets::chip(theme.muted_foreground)
                        .child(h_flex().gap_1().child(Icon::new(IconName::Settings2).xsmall()).child(text)),
                )
        });
        let pack = self.data.ready().and_then(|d| d.pack.clone()).map(|p| {
            div()
                .id("mods-pack")
                .tooltip(|w, cx| {
                    gpui_kit::component::tooltip::Tooltip::new("Pack mods update through Update from server pack").build(w, cx)
                })
                .child(widgets::chip(theme.primary).child(p))
        });
        let _ = window;
        h_flex()
            .gap_2()
            .child(
                TabBar::new("mods-sub")
                    .underline()
                    .small()
                    .selected_index(if self.sub == Sub::Mods { 0 } else { 1 })
                    .child(Tab::new().label(n_mods.map_or("Mods".to_string(), |n| format!("Mods {n}"))))
                    .child(Tab::new().label(n_sets.map_or("Backups".to_string(), |n| format!("Backups {n}"))))
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.sub = if *ix == 0 { Sub::Mods } else { Sub::Backups };
                        if this.sub == Sub::Backups {
                            this.load_backups(cx);
                        }
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
            .children(meta)
            .children(pack)
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mods = self.mods();
        let count = |f: Filter| mods.iter().filter(|m| f.fits(m)).count();
        let filters: Vec<Filter> =
            FILTERS.into_iter().filter(|f| *f == Filter::All || *f == self.filter || count(*f) > 0).collect();
        let picked = filters.iter().position(|f| *f == self.filter).unwrap_or(0);
        let unknown = count(Filter::Unknown);
        let fl = filters.clone();
        h_flex()
            .gap_2()
            .flex_wrap()
            .child(
                TabBar::new("mods-filter")
                    .segmented()
                    .small()
                    .selected_index(picked)
                    .children(filters.iter().map(|f| Tab::new().label(format!("{} {}", f.label(), count(*f)))))
                    .on_click(cx.listener(move |this, ix: &usize, _, cx| {
                        if let Some(f) = fl.get(*ix) {
                            this.filter = *f;
                            this.refilter();
                            cx.notify();
                        }
                    })),
            )
            .child(div().flex_1())
            .child(
                div().w_56().child(
                    Input::new(&self.search).small().prefix(Icon::new(IconName::Search).small()).cleanable(true),
                ),
            )
            .child(
                Button::new("mods-identify")
                    .small()
                    .outline()
                    .icon(IconName::ScanSearch)
                    .label(if unknown > 0 { format!("Identify {unknown}") } else { "Identify".into() })
                    .tooltip("Look jars up on CurseForge and Modrinth")
                    .loading(self.identifying)
                    .disabled(self.identifying || unknown == 0)
                    .on_click(cx.listener(|this, _, _, cx| this.identify(false, cx))),
            )
            .child(
                Button::new("mods-check")
                    .small()
                    .outline()
                    .icon(IconName::RefreshCw)
                    .label(if self.checking { "Checking…" } else { "Check updates" })
                    .disabled(self.checking)
                    .on_click(cx.listener(|this, _, _, cx| this.check_updates(true, cx))),
            )
            .child(
                Button::new("mods-add")
                    .small()
                    .primary()
                    .icon(IconName::Plus)
                    .label("Add mods")
                    .disabled(self.busy.is_some())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.adding = !this.adding;
                        cx.notify();
                    })),
            )
    }

    /// Identify / busy / outcome / CurseForge key / client-only / updates.
    fn bars(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let (info, warn, danger, success, primary) = (theme.info, theme.warning, theme.danger, theme.success, theme.primary);
        let mut out = Vec::new();
        if self.identifying {
            out.push(
                widgets::notice_bar(info, IconName::ScanSearch, cx)
                    .child(Spinner::new().small())
                    .child(
                        div()
                            .text_sm()
                            .child("Identifying jars on CurseForge and Modrinth… names, icons and updates fill in when it's done."),
                    )
                    .into_any_element(),
            );
        }
        if let Some(b) = &self.busy {
            out.push(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .text_color(warn)
                    .child(Spinner::new().small())
                    .child(b.clone())
                    .into_any_element(),
            );
        }
        if let Some((err, text)) = &self.note {
            out.push(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .text_color(if *err { danger } else { success })
                    .child(Icon::new(if *err { IconName::TriangleAlert } else { IconName::CircleCheck }).small())
                    .child(div().flex_1().min_w_0().child(text.clone()))
                    .child(
                        Button::new("mods-note-x")
                            .ghost()
                            .xsmall()
                            .icon(IconName::X)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.note = None;
                                cx.notify();
                            })),
                    )
                    .into_any_element(),
            );
        }
        if let Some(d) = self.data.ready() {
            if !d.curseforge {
                out.push(
                    widgets::notice_bar(info, IconName::KeyRound, cx)
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_2()
                                .child(div().text_sm().child(
                                    "CurseForge needs an API key for search, identify and updates. Modrinth works without one.",
                                ))
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .child(div().flex_1().max_w(rems(28.)).child(Input::new(&self.cf_key).small()))
                                        .child(
                                            Button::new("mods-cf-save")
                                                .small()
                                                .outline()
                                                .icon(IconName::Save)
                                                .label(if self.cf_saving { "Checking…" } else { "Save" })
                                                .disabled(self.cf_saving)
                                                .on_click(cx.listener(|this, _, window, cx| this.save_cf_key(window, cx))),
                                        )
                                        .child(
                                            Button::new("mods-cf-get")
                                                .small()
                                                .ghost()
                                                .icon(IconName::ExternalLink)
                                                .label("Get a key")
                                                .on_click(|_, _, cx| cx.open_url("https://console.curseforge.com/")),
                                        ),
                                ),
                        )
                        .into_any_element(),
                );
            }
            let client_on = d.mods.iter().filter(|m| m.side == "client" && m.enabled).count();
            if client_on > 0 {
                let one = client_on == 1;
                out.push(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .text_color(warn)
                        .child(Icon::new(IconName::TriangleAlert).small())
                        .child(format!(
                            "{client_on} client-only mod{} on: servers usually don't need {}, and some won't start with {}.",
                            if one { "" } else { "s" },
                            if one { "it" } else { "them" },
                            if one { "it" } else { "them" },
                        ))
                        .child(
                            Button::new("mods-show-client")
                                .ghost()
                                .xsmall()
                                .label("Show")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.filter = Filter::Client;
                                    this.refilter();
                                    cx.notify();
                                })),
                        )
                        .into_any_element(),
                );
            }
        }
        if let Some(ups) = self.updates.as_ref().filter(|u| !u.is_empty() && !self.reviewing) {
            let n = ups.len();
            let names = ups.iter().take(3).map(|u| u.name.as_str()).collect::<Vec<_>>().join(", ");
            let more = if n > 3 { format!(" +{}", n - 3) } else { String::new() };
            let label = if n == 1 { "Review 1 update".to_string() } else { format!("Review {n} updates") };
            out.push(
                widgets::notice_bar(primary, IconName::CircleArrowUp, cx)
                    .child(div().flex_none().text_sm().font_semibold().child(if n == 1 {
                        "1 update".to_string()
                    } else {
                        format!("{n} updates")
                    }))
                    .child(widgets::caption(format!("· {names}{more}"), cx).flex_1().min_w_0())
                    .child(
                        Button::new("mods-review")
                            .small()
                            .primary()
                            .label(label)
                            .disabled(self.busy.is_some())
                            .on_click(cx.listener(|this, _, _, cx| this.open_review(cx))),
                    )
                    .into_any_element(),
            );
        }
        out
    }

    fn meta_editor(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_1()
            .child(widgets::caption(
                "Used to pick mod files. For example: 1.21.1 neoforge-21.1.259 (or fabric, quilt, forge-47.4.0).",
                cx,
            ))
            .child(
                h_flex()
                    .gap_2()
                    .child(div().w(rems(22.)).child(Input::new(&self.meta).small()))
                    .child(
                        Button::new("mods-meta-save")
                            .small()
                            .outline()
                            .label("Save")
                            .on_click(cx.listener(|this, _, _, cx| this.save_meta(cx))),
                    )
                    .child(Button::new("mods-meta-x").small().ghost().label("Cancel").on_click(cx.listener(|this, _, _, cx| {
                        this.meta_edit = false;
                        cx.notify();
                    }))),
            )
    }

    fn add_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let has_cf = self.data.ready().is_some_and(|d| d.curseforge);
        let gv = self.data.ready().map(|d| d.game_version.clone()).unwrap_or_default();
        let body: AnyElement = match &self.results {
            None => widgets::caption("Search to find mods. Dependencies are added with them.", cx).into_any_element(),
            Some(Load::Waiting) => widgets::skeleton_rows(4, rems(2.5), cx).into_any_element(),
            Some(Load::Failed(e)) => div().text_sm().text_color(theme.danger).child(e.clone()).into_any_element(),
            Some(Load::Ready(hits)) if hits.is_empty() => widgets::caption("Nothing found.", cx).into_any_element(),
            Some(Load::Ready(hits)) => {
                let store = self.store.read(cx);
                let n = hits.len();
                div()
                    .id("mods-hits")
                    .when(n > 5, |d| d.h(rems(18.)))
                    .overflow_y_scrollbar()
                    .child(v_flex().children(hits.iter().map(|h| {
                        let icon = store.image(&h.icon).map(Path::to_path_buf);
                        let (id, name) = (h.id.clone(), h.name.clone());
                        h_flex()
                            .py_1p5()
                            .gap_3()
                            .border_b_1()
                            .border_color(theme.border)
                            .child(widgets::thumb(icon.as_deref(), rems(2.), cx))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .min_w_0()
                                            .child(div().text_sm().font_medium().truncate().child(h.name.clone()))
                                            .child(widgets::caption(h.author.clone(), cx).text_xs().flex_none()),
                                    )
                                    .child(widgets::caption(h.summary.clone(), cx).text_xs()),
                            )
                            .child(if h.installed {
                                widgets::chip(theme.success).child("Added").into_any_element()
                            } else {
                                Button::new(ElementId::Name(format!("hit-add:{}", h.id).into()))
                                    .small()
                                    .outline()
                                    .icon(IconName::Plus)
                                    .label("Add")
                                    .disabled(self.busy.is_some())
                                    .on_click(cx.listener(move |this, _, _, cx| this.install(id.clone(), name.clone(), cx)))
                                    .into_any_element()
                            })
                    })))
                    .into_any_element()
            }
        };
        v_flex()
            .gap_2()
            .p_3()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        TabBar::new("mods-source")
                            .segmented()
                            .small()
                            .selected_index(if self.modrinth { 0 } else { 1 })
                            .child(Tab::new().label("Modrinth"))
                            .child(Tab::new().label("CurseForge").disabled(!has_cf))
                            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                this.modrinth = *ix == 0;
                                cx.notify();
                            })),
                    )
                    .child(
                        div().flex_1().child(
                            Input::new(&self.add_query)
                                .small()
                                .prefix(Icon::new(IconName::Search).small())
                                .cleanable(true),
                        ),
                    )
                    .when(!gv.is_empty(), |r| r.child(widgets::caption(format!("for {gv}"), cx).flex_none()))
                    .child(
                        Button::new("mods-search")
                            .small()
                            .outline()
                            .label("Search")
                            .loading(matches!(self.results, Some(Load::Waiting)))
                            .on_click(cx.listener(|this, _, _, cx| this.run_search(cx))),
                    )
                    .child(
                        Button::new("mods-add-x")
                            .small()
                            .ghost()
                            .icon(IconName::X)
                            .tooltip("Close")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.adding = false;
                                cx.notify();
                            })),
                    ),
            )
            .child(body)
    }

    fn markdown(id: impl Into<ElementId>, md: &str, cx: &App) -> AnyElement {
        if md.trim().is_empty() {
            return widgets::caption("No changelog for this version.", cx).italic().into_any_element();
        }
        TextView::markdown(id, SharedString::from(md.to_string()))
            .selectable(true)
            .scrollable(false)
            .on_link_click(|url, _, _, cx| cx.open_url(url))
            .into_any_element()
    }

    fn review(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let ups = self.updates.clone().unwrap_or_default();
        let n = ups.len();
        let k = ups.iter().filter(|u| self.review_picked.contains(&u.project_id)).count();
        let busy = self.busy.is_some();
        let rows = ups.iter().map(|u| {
            let pid = u.project_id.clone();
            let open = self.review_open.as_deref() == Some(pid.as_str());
            let (p1, u1) = (pid.clone(), u.clone());
            let notes: Option<AnyElement> = open.then(|| match self.review_notes.get(&pid) {
                Some(Some(md)) => Self::markdown(ElementId::Name(format!("rv-notes:{pid}").into()), md, cx),
                _ => widgets::caption("Loading…", cx).into_any_element(),
            });
            // Press a box and drag down the list: the rows crossed take its
            // new state.
            let on = self.review_picked.contains(&pid);
            let view = cx.entity();
            let set = {
                let pid = pid.clone();
                Rc::new(move |v: usize, _: &mut Window, cx: &mut App| {
                    let pid = pid.clone();
                    view.update(cx, |this, cx| {
                        let changed = if v == 1 { this.review_picked.insert(pid) } else { this.review_picked.remove(&pid) };
                        if changed {
                            cx.notify();
                        }
                    })
                })
            };
            let (s1, s2, paint) = (set.clone(), set.clone(), self.paint.clone());
            v_flex()
                .px_3()
                .py_1p5()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    drag_pick::paint_row(h_flex(), &self.paint, "review", move |v, w, cx| s1(v, w, cx))
                        .gap_2()
                        .child(drag_pick::paint_option(div(), &self.paint, "review", usize::from(!on), move |v, w, cx| s2(v, w, cx)).child(
                            Checkbox::new(ElementId::Name(format!("rv-pick:{pid}").into()))
                                .checked(on)
                                .on_click(cx.listener(move |this, v: &bool, _, cx| {
                                    // The press already set it.
                                    if paint.active(&"review".into()) {
                                        return;
                                    }
                                    if *v {
                                        this.review_picked.insert(p1.clone());
                                    } else {
                                        this.review_picked.remove(&p1);
                                    }
                                    cx.notify();
                                })),
                        ))
                        .child(
                            h_flex()
                                .id(ElementId::Name(format!("rv-open:{pid}").into()))
                                .flex_1()
                                .min_w_0()
                                .gap_1p5()
                                .clickable(cx)
                                .on_click(cx.listener(move |this, _, _, cx| this.review_notes(&u1, cx)))
                                .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall())
                                .child(div().text_sm().font_semibold().truncate().child(u.name.clone()))
                                .child(widgets::caption(format!("{} →", u.from), cx).flex_none())
                                .child(div().text_sm().text_color(theme.primary).flex_none().child(u.to.clone())),
                        ),
                )
                .when_some(notes, |c, n| c.child(div().pl_8().pt_1().child(n)))
        });
        v_flex()
            .flex_1()
            .min_h_0()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().text_base().font_semibold().child(if n == 1 {
                        "Review 1 update".to_string()
                    } else {
                        format!("Review {n} updates")
                    }))
                    .child(div().flex_1())
                    .child(
                        Button::new("rv-all")
                            .small()
                            .ghost()
                            .label("All")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.review_picked = this.updates.iter().flatten().map(|u| u.project_id.clone()).collect();
                                cx.notify();
                            })),
                    )
                    .child(Button::new("rv-none").small().ghost().label("None").on_click(cx.listener(|this, _, _, cx| {
                        this.review_picked.clear();
                        cx.notify();
                    }))),
            )
            .child(widgets::caption(
                "The jars they replace go to Mods › Backups, so each one can be put back. If the server is running, restart it afterwards to load them.",
                cx,
            ))
            .child(
                div()
                    .id("rv-list")
                    .flex_1()
                    .min_h_0()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .overflow_y_scrollbar()
                    .child(v_flex().children(rows)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("rv-cancel")
                            .small()
                            .outline()
                            .label("Cancel")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.reviewing = false;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("rv-go")
                            .small()
                            .primary()
                            .icon(IconName::CircleArrowUp)
                            .label(format!("Update {k}"))
                            .loading(busy)
                            .disabled(k == 0 || busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                let ids: Vec<String> = this
                                    .updates
                                    .iter()
                                    .flatten()
                                    .map(|u| u.project_id.clone())
                                    .filter(|p| this.review_picked.contains(p))
                                    .collect();
                                this.apply_updates(ids, cx);
                            })),
                    ),
            )
    }

    fn peek_view(&self, m: &ModRow, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(p) = &self.peek else { return div().into_any_element() };
        let icon = self.store.read(cx).image(&m.icon).map(Path::to_path_buf);
        let busy = self.busy.is_some();
        let known = !m.source.is_empty();
        let mut by = if !m.author.is_empty() {
            m.author.clone()
        } else if known {
            m.site().to_string()
        } else {
            "Unknown source".to_string()
        };
        if !m.side.is_empty() {
            by.push_str(&format!(" · {}", m.side));
        }
        by.push_str(if m.from_pack { " · from the pack" } else { " · added on top" });
        let gv = self.data.ready().map(|d| d.game_version.clone()).unwrap_or_default();

        let body: AnyElement = if !known {
            widgets::caption(
                "Hopper doesn't know where this jar is from yet, so there are no versions or changelogs. Identify looks it up on CurseForge and Modrinth.",
                cx,
            )
            .whitespace_normal()
            .into_any_element()
        } else {
            match &p.versions {
                None | Some(Load::Waiting) => widgets::skeleton_rows(4, rems(2.), cx).into_any_element(),
                Some(Load::Failed(e)) => div().text_sm().text_color(theme.danger).child(e.clone()).into_any_element(),
                Some(Load::Ready(v)) => {
                    let cur = v.list.iter().position(|x| x.installed);
                    let rows = v.list.iter().enumerate().map(|(i, x)| {
                        let on = p.shown.as_deref() == Some(x.id.as_str());
                        let (id1, id2) = (x.id.clone(), x.id.clone());
                        let label = if x.version.is_empty() { x.name.clone() } else { x.version.clone() };
                        let verb = if cur.is_some_and(|c| i > c) { "Roll back" } else { "Install" };
                        let mut when = hopper::time::date_of(&x.date);
                        if !x.kind.is_empty() && x.kind != "release" {
                            when.push_str(&format!(" · {}", x.kind));
                        }
                        h_flex()
                            .gap_2()
                            .px_2()
                            .py_1()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(if on {
                                theme.primary.opacity(widgets::alpha::LINE)
                            } else if x.installed {
                                theme.success.opacity(widgets::alpha::LINE)
                            } else {
                                theme.border
                            })
                            .child(
                                v_flex()
                                    .id(ElementId::Name(format!("ver:{}", x.id).into()))
                                    .flex_1()
                                    .min_w_0()
                                    .clickable(cx)
                                    .on_click(cx.listener(move |this, _, _, cx| this.show_notes(id1.clone(), cx)))
                                    .child(div().text_sm().font_medium().truncate().child(label.clone()))
                                    .child(widgets::caption(when, cx).text_xs()),
                            )
                            .child(if x.installed {
                                widgets::chip(theme.success).child("Installed").into_any_element()
                            } else {
                                Button::new(ElementId::Name(format!("ver-go:{}", x.id).into()))
                                    .xsmall()
                                    .outline()
                                    .label(verb)
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let verb = if verb == "Roll back" { "Rolling back" } else { "Installing" };
                                        this.set_version(id2.clone(), label.clone(), verb, cx)
                                    }))
                                    .into_any_element()
                            })
                    });
                    let notes: Option<AnyElement> = p.shown.as_ref().map(|id| match &p.notes {
                        None => widgets::caption("Loading…", cx).into_any_element(),
                        Some(Err(e)) => div().text_sm().text_color(theme.danger).child(e.clone()).into_any_element(),
                        Some(Ok(md)) => Self::markdown(ElementId::Name(format!("notes:{id}").into()), md, cx),
                    });
                    v_flex()
                        .gap_2()
                        .child(div().text_xs().font_semibold().text_color(theme.muted_foreground).child(format!(
                            "VERSIONS FOR MINECRAFT {}",
                            gv.to_uppercase()
                        )))
                        .child(if v.list.is_empty() {
                            widgets::caption("No files for this Minecraft version and loader.", cx).into_any_element()
                        } else {
                            v_flex().gap_1().children(rows).into_any_element()
                        })
                        .when_some(notes, |c, n| {
                            c.child(div().text_xs().font_semibold().text_color(theme.muted_foreground).child("CHANGELOG"))
                                .child(n)
                        })
                        .into_any_element()
                }
            }
        };

        let update = m.update.clone();
        let page = m.page.clone();
        v_flex()
            .id("mods-peek")
            .size_full()
            .gap_3()
            .p_4()
            .overflow_y_scrollbar()
            .child(
                h_flex()
                    .gap_3()
                    .child(widgets::thumb(icon.as_deref(), rems(2.5), cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().font_semibold().truncate().child(m.name.clone()))
                            .child(widgets::caption(by, cx).text_xs()),
                    )
                    .child(
                        Button::new("peek-x")
                            .ghost()
                            .small()
                            .icon(IconName::X)
                            .tooltip("Close")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.peek = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .when_some(update, |r, u| {
                        let (fid, to) = (u.file_id.clone(), u.to.clone());
                        r.child(
                            Button::new("peek-update")
                                .small()
                                .primary()
                                .icon(IconName::CircleArrowUp)
                                .label(format!("Update to {}", u.to))
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, _, cx| this.set_version(fid.clone(), to.clone(), "Updating", cx))),
                        )
                    })
                    .when(known, |r| {
                        r.child(
                            Button::new("peek-lock")
                                .small()
                                .outline()
                                .icon(if m.locked { IconName::LockOpen } else { IconName::Lock })
                                .label(if m.locked { "Unlock" } else { "Lock" })
                                .on_click(cx.listener(|this, _, _, cx| this.lock(cx))),
                        )
                        .child(
                            Button::new("peek-config")
                                .small()
                                .ghost()
                                .icon(IconName::RotateCcw)
                                .label("Reset config…")
                                .on_click(cx.listener(|this, _, window, cx| this.reset_config(window, cx))),
                        )
                    })
                    .child(div().flex_1())
                    .when(!page.is_empty(), |r| {
                        let site = m.site();
                        r.child(
                            Button::new("peek-site")
                                .small()
                                .ghost()
                                .icon(IconName::ExternalLink)
                                .label(site)
                                .on_click(move |_, _, cx| cx.open_url(&page)),
                        )
                    })
                    .child(
                        Button::new("peek-remove")
                            .small()
                            .ghost()
                            .icon(Icon::new(IconName::Trash).text_color(theme.danger))
                            .tooltip("Remove (kept in Backups)")
                            .on_click(cx.listener(|this, _, window, cx| this.remove(window, cx))),
                    ),
            )
            .when(m.locked, |c| c.child(widgets::caption("Locked: update checks skip it.", cx).text_xs()))
            .child(widgets::caption(m.file.clone(), cx).text_xs().font_family("monospace"))
            .child(body)
            .into_any_element()
    }

    fn backups_view(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let _ = window;
        let Some(l) = &self.backups else {
            return widgets::skeleton_rows(4, rems(2.25), cx).into_any_element();
        };
        if let Some(el) = load_state(l, cx) {
            return el;
        }
        let Some(b) = l.ready() else { return div().into_any_element() };
        let theme = cx.theme();
        let busy = self.busy.is_some();
        let now = hopper::time::now();
        let old: Vec<i64> = b.sets.iter().filter(|s| now - s.when > 30 * 86_400).map(|s| s.when).collect();
        let size = hopper::instance_ops::human_bytes;
        let head = h_flex()
            .gap_2()
            .child(widgets::caption(
                format!(
                    "{} backup{} · {} in .hopper-backups",
                    b.sets.len(),
                    if b.sets.len() == 1 { "" } else { "s" },
                    size(b.total)
                ),
                cx,
            ))
            .child(div().flex_1())
            .when(!old.is_empty(), |r| {
                let n = old.len();
                r.child(
                    Button::new("bk-old")
                        .small()
                        .outline()
                        .icon(IconName::Trash)
                        .label("Delete older than 30 days")
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.delete_sets(
                                old.clone(),
                                format!("Delete {n} backup{} older than 30 days?", if n == 1 { "" } else { "s" }),
                                window,
                                cx,
                            )
                        })),
                )
            });
        if b.sets.is_empty() {
            return v_flex()
                .gap_2()
                .child(head)
                .child(widgets::empty_state(
                    IconName::Archive,
                    "Nothing set aside yet",
                    Some("Jars that updates replace and mods you remove land here.".into()),
                    cx,
                ))
                .into_any_element();
        }
        let sets = b.sets.iter().map(|s| {
            let when = s.when;
            let open = self.open_sets.contains(&when);
            let summary = if s.files.len() == 1 {
                s.files[0].0.clone()
            } else {
                let names: Vec<String> = s.files.iter().take(3).map(|f| short_name(&f.0)).collect();
                format!("{} files: {}{}", s.files.len(), names.join(", "), if s.files.len() > 3 { "…" } else { "" })
            };
            let stamp = format!("{} · {}", hopper::time::date(when), hopper::time::relative_secs(when));
            let many = s.files.len() > 1;
            let files = open.then(|| {
                v_flex().pl_8().children(s.files.iter().map(|(name, sz, to)| {
                    let n1 = name.clone();
                    let dir = to.split('/').next().unwrap_or("").to_string();
                    h_flex()
                        .gap_2()
                        .py_0p5()
                        .child(div().flex_1().min_w_0().truncate().text_sm().font_family("monospace").child(name.clone()))
                        .child(widgets::caption(format!("→ {dir}/ · {}", size(*sz)), cx).text_xs().flex_none())
                        .when(many, |r| {
                            r.child(
                                Button::new(ElementId::Name(format!("bk-one:{when}:{name}").into()))
                                    .xsmall()
                                    .ghost()
                                    .label("Restore")
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.restore(when, Some(n1.clone()), n1.clone(), window, cx)
                                    })),
                            )
                        })
                }))
            });
            let what = if many { format!("these {} files", s.files.len()) } else { summary.clone() };
            v_flex()
                .px_3()
                .py_1p5()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            h_flex()
                                .id(ElementId::Name(format!("bk-open:{when}").into()))
                                .flex_1()
                                .min_w_0()
                                .gap_2()
                                .clickable(cx)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if !this.open_sets.remove(&when) {
                                        this.open_sets.insert(when);
                                    }
                                    cx.notify();
                                }))
                                .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall())
                                .child(widgets::caption(stamp, cx).flex_none().w(rems(13.)))
                                .child(div().flex_1().min_w_0().truncate().text_sm().child(summary.clone())),
                        )
                        .child(widgets::caption(size(s.size), cx).text_xs().flex_none())
                        .child(
                            Button::new(ElementId::Name(format!("bk-restore:{when}").into()))
                                .small()
                                .outline()
                                .icon(IconName::Undo2)
                                .label(if many { "Restore all" } else { "Restore" })
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.restore(when, None, what.clone(), window, cx)
                                })),
                        )
                        .child(
                            Button::new(ElementId::Name(format!("bk-del:{when}").into()))
                                .small()
                                .ghost()
                                .icon(IconName::Trash)
                                .tooltip("Delete this backup")
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.delete_sets(vec![when], "Delete this backup?".into(), window, cx)
                                })),
                        ),
                )
                .children(files)
        });
        v_flex()
            .size_full()
            .gap_2()
            .child(head)
            .child(
                div()
                    .id("bk-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(
                        v_flex()
                            .rounded(theme.radius_lg)
                            .border_1()
                            .border_color(theme.border)
                            .children(sets),
                    ),
            )
            .into_any_element()
    }
}

/// A jar's name without its version: "sodium-fabric-0.5.8.jar" → "sodium".
fn short_name(name: &str) -> String {
    let b = name.as_bytes();
    for i in 0..b.len() {
        if (b[i] == b'-' || b[i] == b'_') && b.get(i + 1).is_some_and(|c| c.is_ascii_digit()) {
            return name[..i].trim_end_matches(['-', '_']).trim_end_matches("-neoforge").trim_end_matches("-fabric").to_string();
        }
    }
    name.trim_end_matches(".disabled").trim_end_matches(".jar").to_string()
}

impl Picks for ModsTab {
    fn picks(&mut self) -> &mut PickState {
        &mut self.pick
    }

    fn pick_list(&self) -> (UniformListScrollHandle, usize) {
        (self.scroll.clone(), self.shown.len())
    }

    fn pick_changed(&mut self, p: &DragPick, _: &mut Context<Self>) -> SharedString {
        self.swept = self.swept_files(p).into_iter().collect();
        let (verb, none) = if p.on { ("Turning on", "All on") } else { ("Turning off", "All off") };
        DragPick::count_label(verb, none, self.swept.len())
    }

    fn pick_commit(&mut self, p: DragPick, _: &mut Window, cx: &mut Context<Self>) {
        self.swept.clear();
        let files = self.swept_files(&p);
        if !files.is_empty() {
            self.toggle_many(files, p.on, cx);
        }
    }
}

impl Render for ModsTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = self.sub_tabs(window, cx).into_any_element();
        if self.sub == Sub::Backups {
            let body = self.backups_view(window, cx);
            return v_flex().size_full().gap_3().child(tabs).child(div().flex_1().min_h_0().child(body)).into_any_element();
        }
        if let Some(el) = load_state(&self.data, cx) {
            return v_flex().size_full().gap_3().child(tabs).child(el).into_any_element();
        }
        let toolbar = self.toolbar(cx).into_any_element();
        let bars = self.bars(cx);
        let meta = self.meta_edit.then(|| self.meta_editor(cx).into_any_element());
        let add = self.adding.then(|| self.add_panel(cx).into_any_element());

        let main: AnyElement = if self.reviewing && self.updates.as_ref().is_some_and(|u| !u.is_empty()) {
            self.review(cx).into_any_element()
        } else {
            let count = self.shown.len();
            let list: AnyElement = if count == 0 {
                let empty = self.mods().is_empty();
                widgets::empty_state(
                    IconName::Package,
                    if empty { "No mods" } else { "No mod fits the filter" },
                    None,
                    cx,
                )
                .into_any_element()
            } else {
                div()
                    .id("panel-mod-area")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .relative()
                    .on_drag_move::<PickDrag>(cx.listener(|this, e: &DragMoveEvent<PickDrag>, window, cx| {
                        let row_h = rems(ROW_H).to_pixels(window.rem_size());
                        drag_pick::moved(this, e.event.position.y, row_h, window, cx)
                    }))
                    .on_mouse_up(MouseButton::Left, cx.listener(|this, _, w, cx| drag_pick::released(this, w, cx)))
                    .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _, w, cx| drag_pick::released(this, w, cx)))
                    .child(
                        uniform_list(
                            "panel-mod-rows",
                            count,
                            cx.processor(|this, range: Range<usize>, window, cx| this.render_rows(range, window, cx)),
                        )
                        .track_scroll(&self.scroll)
                        .size_full()
                        .pr_2(),
                    )
                    .vertical_scrollbar(&self.scroll)
                    .into_any_element()
            };
            let peek = self.picked_mod().cloned().map(|m| {
                let content = self.peek_view(&m, cx);
                let border = cx.theme().border;
                widgets::side_panel("mods-peek-resize", &self.peek_panel, |v: &mut Self| &mut v.peek_panel, &self.store, content, window, cx)
                    .border_l_1()
                    .border_color(border)
                    .into_any_element()
            });
            h_flex()
                .flex_1()
                .min_h_0()
                .items_start()
                .gap_3()
                .child(div().flex_1().min_w_0().h_full().flex().child(list))
                .children(peek)
                .into_any_element()
        };

        v_flex()
            .size_full()
            .gap_3()
            .child(tabs)
            .child(toolbar)
            .children(bars)
            .children(meta)
            .children(add)
            .child(main)
            .into_any_element()
    }
}
