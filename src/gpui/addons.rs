//! The Mods, Resource packs and Shaders tabs: a sortable, filterable table
//! of what an instance has installed, with updates, toggles and removal.

use crate::store::{Store, Updates};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::cf;
use hopper::install::Update;
use hopper::instances::{Addon, Instance};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Show {
    All,
    Updates,
    Disabled,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortCol {
    Name,
    Author,
    Env,
    Active,
    Added,
}

/// One line of the table.
#[derive(Clone)]
enum Row {
    /// Index into `Instance::addons`.
    Addon(usize),
    /// "N jars not from CurseForge".
    LooseHeader(usize),
    Loose(String),
}

gpui_kit::actions!(
    addons,
    [RowUp, RowDown, RowFirst, RowLast, RowPageUp, RowPageDown, RowToggle, RowOpen]
);

/// The table's keys, while it has focus.
pub fn bind_keys(cx: &mut App) {
    let ctx = Some("AddonsTable");
    cx.bind_keys([
        KeyBinding::new("up", RowUp, ctx),
        KeyBinding::new("down", RowDown, ctx),
        KeyBinding::new("home", RowFirst, ctx),
        KeyBinding::new("end", RowLast, ctx),
        KeyBinding::new("pageup", RowPageUp, ctx),
        KeyBinding::new("pagedown", RowPageDown, ctx),
        KeyBinding::new("space", RowToggle, ctx),
        KeyBinding::new("enter", RowOpen, ctx),
    ]);
}

pub fn in_class(a: &Addon, class: u32) -> bool {
    a.class_id == class || (class == cf::CLASS_MODS && a.class_id == 0)
}

// Column widths, shared by the header and every row so they line up.
fn col_check() -> Div {
    div().w_6().flex_none()
}
fn col_action() -> Div {
    div().w_32().flex_none()
}
fn col_author() -> Div {
    div().w_40().flex_none()
}
fn col_env() -> Div {
    div().w_12().flex_none()
}
fn col_active() -> Div {
    div().w_16().flex_none()
}
fn col_date() -> Div {
    div().w_24().flex_none()
}
fn col_trash() -> Div {
    div().w_8().flex_none()
}

/// Row height: a 40 px icon plus breathing room.
const ROW_H: f32 = 3.5;

/// What the table shows, built from the store and the disk when the
/// instance, its updates or the jobs change, and kept between frames: the
/// store redraws every 50 ms while jobs run, and scrolling redraws every
/// frame, so nothing here may read files or clone the instance per frame.
#[derive(Default)]
struct Table {
    /// Fingerprint of the store state it was built from.
    sig: Option<u64>,
    inst: Option<Rc<Instance>>,
    /// Pending updates of this tab's class, by project.
    updates: Rc<HashMap<u64, Update>>,
    /// Lower-case resource pack files switched on in game.
    active_rp: Rc<HashSet<String>>,
    /// The shader pack on in game, as options say it.
    shader: Option<String>,
    has_shader_loader: bool,
    /// The filtered, sorted rows, and the filters they are for.
    rows: Rc<Vec<Row>>,
    rows_for: Option<RowsKey>,
}

/// The view's filters and sort, as the rows were built for them.
#[derive(Clone, PartialEq)]
struct RowsKey {
    query: String,
    show: Show,
    added_only: bool,
    updates_first: bool,
    sort: (SortCol, bool),
}

/// Everything the table shows from the store, hashed: a different value
/// means the table must be rebuilt. Cheap (no I/O, no allocation).
fn fingerprint(store: &Store, root: &Path, class: u32) -> Option<u64> {
    let inst = store.instance(root)?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (root, class, &inst.name, &inst.loader, &inst.game_version).hash(&mut h);
    inst.pack.as_ref().map(|p| (p.project_id, p.file_id)).hash(&mut h);
    inst.mod_profiles.len().hash(&mut h);
    for a in &inst.addons {
        (a.project_id, a.file_id, &a.file_name, &a.name, &a.author, &a.icon).hash(&mut h);
        (a.enabled, a.from_pack, a.locked, a.class_id, &a.env, &a.date_added).hash(&mut h);
        (&a.mr_project, &a.page).hash(&mut h);
    }
    match store.updates(root) {
        None => 0u8.hash(&mut h),
        Some(Updates::Checking) => 1u8.hash(&mut h),
        Some(Updates::Failed(e)) => (2u8, e).hash(&mut h),
        Some(Updates::Ready(u)) => {
            3u8.hash(&mut h);
            for x in u {
                (x.project_id, x.file.id).hash(&mut h);
            }
        }
    }
    // The game writes options.txt (resource packs, shaders) as it runs.
    store.running(root).is_some().hash(&mut h);
    // Jobs that ended may have changed the folders (dropped jars, identify).
    for j in store.jobs().snapshot() {
        if j.done {
            j.id.hash(&mut h);
        }
    }
    (store.server_rev(), store.settings().mods_server_column).hash(&mut h);
    Some(h.finish())
}

/// What the Server column was compared for: the server's revision and each
/// mod's file, state and version.
fn marks_key(store: &Store, inst: &Instance) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    store.server_rev().hash(&mut h);
    for a in &inst.addons {
        (&a.file_name, a.enabled, a.file_id).hash(&mut h);
    }
    h.finish()
}

/// Where `remove_files` put `original`: the newest `.hopper-backups/<time>/`
/// entry for its name (repeats are numbered `2-name`), from `since` on.
fn find_backup(root: &Path, original: &Path, since: u64) -> Option<PathBuf> {
    let name = original.file_name()?.to_string_lossy().into_owned();
    let mut best: Option<((u64, u32), PathBuf)> = None;
    for d in std::fs::read_dir(root.join(hopper::sync::BACKUP_DIR)).ok()?.flatten() {
        let Some(stamp) = d.file_name().to_str().and_then(|s| s.parse::<u64>().ok()) else {
            continue;
        };
        if stamp < since {
            continue;
        }
        for e in std::fs::read_dir(d.path()).into_iter().flatten().flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            let rank = if n == name {
                Some(1)
            } else {
                n.strip_suffix(&name)
                    .and_then(|p| p.strip_suffix('-'))
                    .and_then(|p| p.parse::<u32>().ok())
            };
            if let Some(r) = rank
                && best.as_ref().is_none_or(|(k, _)| *k < (stamp, r))
            {
                best = Some(((stamp, r), e.path()));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// A removed file: where it was, where its backup is, and its entry.
#[derive(Clone)]
struct Removed {
    original: PathBuf,
    backup: PathBuf,
    addon: Option<Addon>,
}

/// A dialog's text: a line, then each warning with a triangle.
fn warning_list(intro: &'static str, warnings: &[String], cx: &App) -> Div {
    let color = cx.theme().warning;
    v_flex().gap_2().child(intro).children(warnings.iter().map(|w| {
        h_flex()
            .gap_2()
            .text_color(color)
            .child(Icon::new(IconName::TriangleAlert).small())
            .child(w.clone())
    }))
}

/// What removing some jars would leave other mods without.
struct Breaks {
    /// (the mod that needs it, its jar, the id it needs), new compared to now.
    lost: Vec<(String, String, String)>,
    /// Which candidate jar provides which ids.
    provides: std::collections::HashMap<String, Vec<String>>,
}

impl Breaks {
    fn check(dir: &std::path::Path, candidates: &[String]) -> Self {
        let jars: Vec<String> = candidates.iter().filter(|f| f.to_lowercase().ends_with(".jar")).cloned().collect();
        if jars.is_empty() {
            return Self { lost: Vec::new(), provides: Default::default() };
        }
        let key = |m: &hopper::deps::Missing| (m.file_name.clone(), m.needs.clone());
        let (now, after) = std::thread::scope(|s| {
            let a = s.spawn(|| hopper::deps::missing(dir));
            let b = s.spawn(|| hopper::deps::missing_without(dir, &jars));
            (a.join().unwrap_or_default(), b.join().unwrap_or_default())
        });
        let had: std::collections::HashSet<_> = now.iter().filter(|m| m.wrong_version.is_none()).map(key).collect();
        let lost = after
            .iter()
            .filter(|m| m.wrong_version.is_none() && !had.contains(&key(m)))
            .map(|m| (m.mod_name.clone(), m.file_name.clone(), m.needs.clone()))
            .collect();
        let provides = jars.iter().map(|f| (f.clone(), hopper::deps::provides(&dir.join(f)))).collect();
        Self { lost, provides }
    }

    /// "X needs y" for what removing `gone` breaks (mods being removed too
    /// don't count).
    fn after(&self, gone: &[String]) -> Vec<String> {
        let mut out: Vec<String> = self
            .lost
            .iter()
            .filter(|(_, jar, _)| !gone.contains(jar))
            .filter(|(_, _, id)| {
                gone.iter().any(|g| self.provides.get(g).is_some_and(|p| p.contains(id)))
            })
            .map(|(name, _, id)| format!("{name} needs {id}"))
            .collect();
        out.dedup();
        out
    }
}

/// Moves removed files back and puts their entries back in the instance.
fn undo_remove(store: &Entity<Store>, root: PathBuf, items: Vec<Removed>, window: &mut Window, cx: &mut App) {
    let s = store.read(cx);
    if let Some(why) = s.edit_hold_reason(&root) {
        window.push_notification(Notification::error(format!("Can't undo now: {why}.")), cx);
        return;
    }
    let Some(mut inst) = s.instance(&root).cloned() else {
        window.push_notification(Notification::error("That instance is gone."), cx);
        return;
    };
    s.spawn_instance_job(&root, "Undoing remove", move |job| {
        let mut back = 0;
        for it in &items {
            if it.original.exists() {
                job.note(format!("{} is back already; left the backup alone", it.original.display()));
                continue;
            }
            if std::fs::rename(&it.backup, &it.original).is_err() {
                std::fs::copy(&it.backup, &it.original)?;
                std::fs::remove_file(&it.backup)?;
            }
            back += 1;
            if let Some(a) = &it.addon
                && !inst.addons.iter().any(|x| x.file_name.eq_ignore_ascii_case(&a.file_name))
            {
                inst.addons.push(a.clone());
            }
        }
        inst.save()?;
        job.detail(format!("Put back {back} files"));
        Ok(crate::store::Msg::InstanceChanged(inst.root.clone()))
    });
}

/// What the table asks its owner to do.
pub enum AddonsEvent {
    /// Find more content of the table's class for its instance.
    AddContent { root: PathBuf, class: u32 },
    /// Find content that goes into a folder of the instance.
    AddTo {
        root: PathBuf,
        class: u32,
        dest: String,
    },
    /// Search Browse for a mod by name or id.
    Find { root: PathBuf, text: String },
    /// Show a project's page in Browse.
    OpenProject { root: PathBuf, project: u64 },
    /// Compare the instance with this server in Server sync.
    Compare(PathBuf),
}

impl EventEmitter<AddonsEvent> for AddonsView {}

pub struct AddonsView {
    /// The table, for its keys.
    focus: FocusHandle,
    /// The row the keys act on.
    cursor: Option<usize>,
    pub(crate) store: Entity<Store>,
    pub(crate) root: Option<PathBuf>,
    pub(crate) class: u32,
    pub(crate) globals: crate::addons_extra::GlobalPacks,
    /// Each mod's state on the paired server, by lower-case file name, for
    /// the server root and [`marks_key`] it was compared at.
    pub(crate) marks: Option<(PathBuf, u64, HashMap<String, hopper::model::DiffEntry>)>,
    pub(crate) missing: crate::addons_extra::MissingDeps,
    search: Entity<InputState>,
    query: String,
    show: Show,
    added_only: bool,
    updates_first: bool,
    sort: (SortCol, bool),
    selected: HashSet<String>,
    /// Jars in the mods folder Hopper doesn't track, by folder mtime.
    loose: (Option<(PathBuf, std::time::SystemTime)>, Vec<String>),
    /// What the table shows, built when it changes rather than per frame.
    table: Table,
    scroll: UniformListScrollHandle,
    /// A mod's project page, docked on the right.
    peek: Entity<crate::browse::BrowseView>,
    /// Whether the Author column fits; set each frame from the window.
    author_fits: bool,
    _subscriptions: Vec<Subscription>,
}

impl AddonsView {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let peek = cx.new(|cx| crate::browse::BrowseView::peek(store.clone(), window, cx));
        let subs = vec![
            cx.observe(&peek, |_, _, cx| cx.notify()),
            cx.observe(&store, |this, _, cx| {
                this.want_updates(cx);
                this.sync(false, cx);
                cx.notify();
            }),
            cx.subscribe_in(&search, window, |this, state, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.query = state.read(cx).value().trim().to_lowercase();
                    cx.notify();
                }
            }),
        ];
        Self {
            focus: cx.focus_handle(),
            cursor: None,
            store,
            root: None,
            class: cf::CLASS_MODS,
            globals: Default::default(),
            marks: None,
            missing: Default::default(),
            search,
            query: String::new(),
            show: Show::All,
            added_only: false,
            updates_first: true,
            sort: (SortCol::Added, false),
            selected: HashSet::new(),
            loose: (None, Vec::new()),
            table: Table::default(),
            scroll: UniformListScrollHandle::new(),
            peek,
            author_fits: true,
            _subscriptions: subs,
        }
    }

    /// The project panel, which the Library docks beside the page.
    pub fn peek(&self) -> &Entity<crate::browse::BrowseView> {
        &self.peek
    }

    pub(crate) fn open_peek(&mut self, project: u64, cx: &mut Context<Self>) {
        if let Some(root) = self.root.clone() {
            self.peek
                .update(cx, |p, cx| p.peek_project(root, Some(project), cx));
        }
    }

    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.focus(window, cx));
    }

    /// Puts `text` in the search box (the command palette's jump to a mod).
    pub fn set_search(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let t = text.to_string();
        self.search.update(cx, |s, cx| s.set_value(t, window, cx));
        self.query = text.trim().to_lowercase();
        cx.notify();
    }

    /// Points the table at an instance and class.
    pub fn show_for(&mut self, root: PathBuf, class: u32, cx: &mut Context<Self>) {
        if self.root.as_ref() != Some(&root) || self.class != class {
            self.selected.clear();
            self.show = Show::All;
            self.cursor = None;
            self.scroll.scroll_to_item(0, ScrollStrategy::Top);
            self.peek
                .update(cx, |p, cx| p.peek_project(root.clone(), None, cx));
        }
        self.root = Some(root);
        self.class = class;
        self.want_updates(cx);
        // Opening the tab reads the folders again (files may have been
        // added by hand since).
        self.sync(true, cx);
        cx.notify();
    }

    /// Rebuilds the table when what it shows changed in the store (or
    /// always, with `force`). All the disk reads the table needs happen
    /// here, never while rendering.
    fn sync(&mut self, force: bool, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            self.table = Table::default();
            return;
        };
        let sig = fingerprint(self.store.read(cx), &root, self.class);
        if !force && sig == self.table.sig {
            return;
        }
        let Some(inst) = self.store.read(cx).instance(&root).cloned() else {
            self.table = Table::default();
            return;
        };
        let started = std::time::Instant::now();
        let class = self.class;
        let updates = self.updates(&inst, cx);
        self.load_globals(&inst);
        let active_rp = if class == cf::CLASS_RESOURCE_PACKS {
            hopper::instance_ops::active_resourcepacks(&inst)
        } else {
            HashSet::new()
        };
        let (shader, has_shader_loader) = if class == cf::CLASS_SHADERS {
            (
                hopper::instance_ops::active_shader(&inst),
                hopper::instance_ops::has_shader_loader(&inst),
            )
        } else {
            (None, true)
        };
        self.refresh_loose(&inst);
        if class == cf::CLASS_MODS {
            self.want_missing(&inst, cx);
        }
        self.table = Table {
            sig,
            inst: Some(Rc::new(inst)),
            updates: Rc::new(updates),
            active_rp: Rc::new(active_rp),
            shader,
            has_shader_loader,
            rows: Rc::default(),
            rows_for: None,
        };
        self.ensure_rows();
        log::debug!(
            "addons: rebuilt the {} table ({} rows) in {:?}",
            root.display(),
            self.table.rows.len(),
            started.elapsed()
        );
        self.want_images(cx);
        self.want_marks(cx);
    }

    /// Reads the folders again and rebuilds the table.
    pub(crate) fn resync(&mut self, cx: &mut Context<Self>) {
        self.sync(true, cx);
        cx.notify();
    }

    /// The shader pack on in game and whether a shader loader is installed,
    /// as read when the table was built.
    pub(crate) fn shader_state(&self) -> (Option<String>, bool) {
        (self.table.shader.clone(), self.table.has_shader_loader)
    }

    /// The view's filters and sort now.
    fn rows_key(&self) -> RowsKey {
        RowsKey {
            query: self.query.clone(),
            show: self.show,
            added_only: self.added_only,
            updates_first: self.updates_first,
            sort: self.sort,
        }
    }

    /// Re-filters and re-sorts the rows when a filter changed (in memory).
    fn ensure_rows(&mut self) {
        let key = self.rows_key();
        if self.table.rows_for.as_ref() == Some(&key) {
            return;
        }
        let rows = match &self.table.inst {
            Some(inst) => self.rows(inst, &self.table.updates),
            None => Vec::new(),
        };
        self.table.rows = Rc::new(rows);
        self.table.rows_for = Some(key);
    }

    /// Compares the mods with the paired server, for the Server column.
    pub(crate) fn want_marks(&mut self, cx: &mut Context<Self>) {
        if self.class != cf::CLASS_MODS {
            return;
        }
        let store = self.store.read(cx);
        if !store.settings().mods_server_column {
            return;
        }
        let Some(root) = self.root.clone() else { return };
        let Some(server) = store.server_of(&root) else {
            return;
        };
        let Some(inst) = store.instance(&root) else { return };
        // Toggling or updating a mod changes the key, not just adding one.
        let key = marks_key(store, inst);
        if self
            .marks
            .as_ref()
            .is_some_and(|(s, r, _)| *s == server.root && *r == key)
        {
            return;
        }
        let (server, inst) = (server.clone(), inst.clone());
        self.marks = Some((server.root.clone(), key, HashMap::new()));
        let anyway = store.settings().sync_anyway.clone();
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move { hopper::rigsync::compare(&inst, &server, &anyway) })
                .await;
            this.update(cx, |this, cx| {
                if let (Ok((diff, _)), Some((_, k, m))) = (r, this.marks.as_mut())
                    && *k == key
                {
                    *m = diff
                        .into_iter()
                        .filter_map(|e| Some((e.client.as_ref()?.file_name.to_lowercase(), e)))
                        .collect();
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Like CurseForge: look for updates as soon as the instance opens,
    /// and again once the last look is half an hour old.
    fn want_updates(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            return;
        };
        if self.store.read(cx).updates_stale(&root) {
            self.store.update(cx, |s, _| s.check_updates(&root, false));
        }
    }

    fn want_images(&mut self, cx: &mut Context<Self>) {
        let (Some(inst), class) = (self.table.inst.clone(), self.class) else {
            return;
        };
        self.store.update(cx, |s, _| {
            for a in inst.addons.iter().filter(|a| in_class(a, class)) {
                s.want_image(&a.icon);
            }
            s.want_categories(inst.addons.iter().filter(|a| in_class(a, class)));
        });
    }

    /// Pending updates of this tab's class, by project.
    fn updates(&self, inst: &Instance, cx: &App) -> HashMap<u64, Update> {
        match self.store.read(cx).updates(&inst.root) {
            Some(Updates::Ready(u)) => u
                .iter()
                .filter(|u| {
                    inst.addons
                        .iter()
                        .any(|a| a.project_id == u.project_id && in_class(a, self.class))
                })
                .map(|u| (u.project_id, u.clone()))
                .collect(),
            _ => HashMap::new(),
        }
    }

    /// Re-lists untracked jars when the mods folder changed.
    fn refresh_loose(&mut self, inst: &Instance) {
        if self.class != cf::CLASS_MODS {
            self.loose = (None, Vec::new());
            return;
        }
        let mtime = std::fs::metadata(inst.mods_dir())
            .and_then(|m| m.modified())
            .ok();
        let key = mtime.map(|t| (inst.root.clone(), t));
        if key.is_none() || self.loose.0 != key {
            self.loose = (key, hopper::install::loose_jars(inst));
        }
    }

    fn rows(&self, inst: &Instance, updates: &HashMap<u64, Update>) -> Vec<Row> {
        let q = &self.query;
        let mut rows: Vec<(usize, String)> = inst
            .addons
            .iter()
            .enumerate()
            .filter(|(_, a)| in_class(a, self.class))
            .filter(|(_, a)| !self.added_only || inst.pack.is_none() || !a.from_pack)
            .filter(|(_, a)| match self.show {
                Show::Updates => updates.contains_key(&a.project_id),
                Show::Disabled => !a.enabled,
                Show::All => true,
            })
            .filter(|(_, a)| {
                q.is_empty()
                    || a.name.to_lowercase().contains(q)
                    || a.file_name.to_lowercase().contains(q)
                    || a.author.to_lowercase().contains(q)
            })
            .map(|(i, a)| (i, a.name.to_lowercase()))
            .collect();
        let ((col, asc), first) = (self.sort, self.updates_first);
        rows.sort_by(|(ia, na), (ib, nb)| {
            let (a, b) = (&inst.addons[*ia], &inst.addons[*ib]);
            let up = |x: &Addon| updates.contains_key(&x.project_id);
            let lead = if first {
                up(b).cmp(&up(a))
            } else {
                std::cmp::Ordering::Equal
            };
            let ord = match col {
                SortCol::Name => na.cmp(nb),
                SortCol::Author => a.author.to_lowercase().cmp(&b.author.to_lowercase()),
                SortCol::Env => a.env.cmp(&b.env),
                SortCol::Active => a.enabled.cmp(&b.enabled),
                SortCol::Added => a.date_added.cmp(&b.date_added),
            };
            lead.then(if asc { ord } else { ord.reverse() })
                .then_with(|| na.cmp(nb))
        });
        let mut out: Vec<Row> = rows.into_iter().map(|(i, _)| Row::Addon(i)).collect();
        let loose = &self.loose.1;
        if self.class == cf::CLASS_MODS
            && !loose.is_empty()
            && self.show == Show::All
            && !self.added_only
        {
            out.push(Row::LooseHeader(loose.len()));
            out.extend(
                loose
                    .iter()
                    .filter(|f| q.is_empty() || f.to_lowercase().contains(q))
                    .cloned()
                    .map(Row::Loose),
            );
        }
        out
    }

    // ---- actions -------------------------------------------------------

    fn report(&self, r: Result<(), String>, window: &mut Window, cx: &mut App) {
        if let Err(e) = r {
            window.push_notification(Notification::error(e), cx);
        }
    }

    fn set_locks(&mut self, files: Vec<String>, locked: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(root) = self.root.clone() {
            set_locks(&self.store, root, files, locked, window, cx);
        }
    }

    /// "X is required by A, B." for each of `files` that enabled mods
    /// outside `files` need.
    fn dependents(&self, files: &[String], cx: &App) -> Vec<String> {
        let Some(root) = &self.root else { return Vec::new() };
        let Some(inst) = self.store.read(cx).instance(root) else {
            return Vec::new();
        };
        files
            .iter()
            .filter_map(|f| inst.addon_by_file(f))
            .filter_map(|a| {
                let by = inst.required_by(a.project_id, files);
                (!by.is_empty()).then(|| format!("{} is required by {}.", a.name, by.join(", ")))
            })
            .collect()
    }

    /// The name of one file's mod, or "N files".
    fn describe(&self, files: &[String], cx: &App) -> String {
        match files {
            [one] => self
                .root
                .as_ref()
                .and_then(|r| self.store.read(cx).instance(r))
                .and_then(|i| i.addon_by_file(one))
                .map_or(one.clone(), |a| a.name.clone()),
            many => format!("{} files", many.len()),
        }
    }

    /// Switches files on or off. Mods that need them (when turning off), or
    /// that they need (when turning on), go along, stay, or are asked
    /// about, as Settings says.
    fn toggle(
        &mut self,
        files: Vec<String>,
        on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.root.clone() else { return };
        let (related, mode) = {
            let s = self.store.read(cx);
            let Some(inst) = s.instance(&root) else { return };
            let related: Vec<(String, String)> = if on {
                inst.missing_deps_of(&files)
            } else {
                inst.dependents_of(&files)
            }
            .into_iter()
            .map(|a| (a.file_name.clone(), a.name.clone()))
            .collect();
            (related, s.settings().dependents.clone())
        };
        if related.is_empty() {
            self.set_enabled(files, on, window, cx);
            return;
        }
        let names: Vec<String> = related.iter().map(|(_, n)| n.clone()).collect();
        let with: Vec<String> = files.iter().cloned().chain(related.iter().map(|(f, _)| f.clone())).collect();
        match mode.as_str() {
            "all" => {
                self.set_enabled(with, on, window, cx);
                window.push_notification(
                    Notification::info(format!(
                        "Also turned {} {}.",
                        if on { "on" } else { "off" },
                        names.join(", ")
                    )),
                    cx,
                );
            }
            "only" => self.set_enabled(files, on, window, cx),
            _ => self.ask_related(files, with, names, on, window, cx),
        }
    }

    /// "Y and Z need X. Turn them off too?" (or, turning on, "X needs Y").
    fn ask_related(
        &mut self,
        files: Vec<String>,
        with: Vec<String>,
        names: Vec<String>,
        on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let what = self.describe(&files, cx);
        let view = cx.entity();
        let remember = std::rc::Rc::new(std::cell::Cell::new(false));
        window.open_dialog(cx, move |dialog, window, cx| {
            let (view, files, with, remember) = (view.clone(), files.clone(), with.clone(), remember.clone());
            let n = names.len();
            let (title, intro, all_label, only_label) = if on {
                (
                    format!("Turn on {what}?"),
                    format!(
                        "{what} needs {} that {} off. The game stops at loading without {}.",
                        if n == 1 { "a mod" } else { "mods" },
                        if n == 1 { "is" } else { "are" },
                        if n == 1 { "it" } else { "them" },
                    ),
                    format!("Turn on all {}", n + files.len()),
                    format!("Only {what}"),
                )
            } else {
                (
                    format!("Turn off {what}?"),
                    format!(
                        "{} {what}. The game stops at loading while a mod it needs is off.",
                        if n == 1 { "This mod needs" } else { "These mods need" },
                    ),
                    format!("Turn off all {}", n + files.len()),
                    format!("Only {what}"),
                )
            };
            let color = cx.theme().warning;
            let (v2, f2, r2) = (view.clone(), files.clone(), remember.clone());
            let r3 = remember.clone();
            dialog
                .title(title)
                .w(rems(30.).to_pixels(window.rem_size()))
                .child(
                    v_flex()
                        .gap_2()
                        .child(div().text_sm().child(intro))
                        .children(names.iter().map(|w| {
                            h_flex()
                                .gap_2()
                                .text_sm()
                                .text_color(color)
                                .child(Icon::new(IconName::TriangleAlert).small())
                                .child(w.clone())
                        }))
                        .child(
                            gpui_kit::component::checkbox::Checkbox::new("deps-remember")
                                .mt_2()
                                .label("Do this every time (change it in Settings → General)")
                                .checked(remember.get())
                                .on_click(move |v: &bool, window, _| {
                                    r3.set(*v);
                                    window.refresh();
                                }),
                        ),
                )
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().child(Button::new("deps-cancel").ghost().label("Cancel")))
                        .child(Button::new("deps-only").outline().label(only_label).on_click(
                            move |_, window, cx| {
                                v2.update(cx, |v, cx| {
                                    if r2.get() {
                                        v.remember_dependents("only", cx);
                                    }
                                    v.set_enabled(f2.clone(), on, window, cx)
                                });
                                window.close_dialog(cx);
                            },
                        ))
                        .child(DialogAction::new().child(Button::new("deps-all").primary().label(all_label))),
                )
                .on_ok(move |_, window, cx| {
                    view.update(cx, |v, cx| {
                        if remember.get() {
                            v.remember_dependents("all", cx);
                        }
                        v.set_enabled(with.clone(), on, window, cx)
                    });
                    true
                })
        });
    }

    fn remember_dependents(&mut self, mode: &str, cx: &mut Context<Self>) {
        let mode = mode.to_string();
        self.store.update(cx, |s, cx| s.update_settings(|st| st.dependents = mode, cx));
    }

    fn set_enabled(
        &mut self,
        files: Vec<String>,
        on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.root.clone() else {
            return;
        };
        let r = self.store.update(cx, |s, cx| {
            let r = s.set_enabled(&root, &files, on);
            cx.notify();
            r
        });
        self.report(r, window, cx);
    }

    fn ask_remove(&mut self, files: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let what = self.describe(&files, cx);
        let warnings = self.dependents(&files, cx);
        // Libraries nothing else needs once these go: offered, ticked.
        let orphans: Vec<(String, String)> = self
            .root
            .as_ref()
            .and_then(|r| self.store.read(cx).instance(r))
            .map(|inst| {
                inst.orphans_of(&files)
                    .into_iter()
                    .map(|a| (a.file_name.clone(), a.name.clone()))
                    .collect()
            })
            .unwrap_or_default();
        // Ticked to go along only when mods are being removed; for resource
        // packs, shaders and the like they're listed but left unticked.
        let removing_mods = self
            .root
            .as_ref()
            .and_then(|r| self.store.read(cx).instance(r))
            .is_some_and(|inst| {
                inst.addons
                    .iter()
                    .filter(|a| files.contains(&a.file_name))
                    .any(|a| a.class_id == cf::CLASS_MODS || a.class_id == 0)
            });
        // Offered, never ticked: removing them is the user's call.
        let _ = removing_mods;
        let picked: Rc<std::cell::RefCell<std::collections::HashSet<String>>> = Default::default();
        // What the remaining jars would lose: checked once in the background
        // for everything that may go; shown for what actually goes.
        let breaks: Rc<std::cell::RefCell<Option<Breaks>>> = Default::default();
        if let Some(dir) = self
            .root
            .as_ref()
            .and_then(|r| self.store.read(cx).instance(r))
            .map(|i| i.mods_dir())
        {
            let candidates: Vec<String> = files.iter().cloned().chain(orphans.iter().map(|(f, _)| f.clone())).collect();
            let holder = breaks.clone();
            cx.spawn(async move |_, cx| {
                let b = cx
                    .background_spawn(async move { Breaks::check(&dir, &candidates) })
                    .await;
                *holder.borrow_mut() = Some(b);
                let _ = cx.update(|cx| cx.refresh_windows());
            })
            .detach();
        }
        let view = cx.entity();
        window.open_dialog(cx, move |dialog, window, cx| {
            let with = {
                let (files, picked) = (files.clone(), picked.clone());
                move || -> Vec<String> {
                    files.iter().cloned().chain(picked.borrow().iter().cloned()).collect()
                }
            };
            let (files, view, w2) = (with(), view.clone(), what.clone());
            let (f2, v2, w3) = (with(), view.clone(), what.clone());
            let libs = (!orphans.is_empty()).then(|| {
                v_flex()
                    .gap_1p5()
                    .pt_2()
                    .child(div().text_sm().font_semibold().child(if orphans.len() == 1 {
                        "Also remove this mod? Nothing else needs it:".to_string()
                    } else {
                        format!("Also remove these {} mods? Nothing else needs them:", orphans.len())
                    }))
                    .children(orphans.iter().enumerate().map(|(i, (file, name))| {
                        let (p, file) = (picked.clone(), file.clone());
                        let on = picked.borrow().contains(&file);
                        gpui_kit::component::checkbox::Checkbox::new(("orphan", i))
                            .label(name.clone())
                            .checked(on)
                            .on_click(move |v: &bool, window, _| {
                                if *v {
                                    p.borrow_mut().insert(file.clone());
                                } else {
                                    p.borrow_mut().remove(&file);
                                }
                                window.refresh();
                            })
                    }))
            });
            dialog
                .title(format!("Remove {what}?"))
                .w(rems(30.).to_pixels(window.rem_size()))
                .child(warning_list(
                    "Move to backups keeps a copy in .hopper-backups in the instance folder, with Undo. Delete sends the files to the Recycle Bin.",
                    &warnings,
                    cx,
                ))
                .children(libs)
                .child(match &*breaks.borrow() {
                    None => h_flex()
                        .gap_2()
                        .pt_2()
                        .child(gpui_kit::component::spinner::Spinner::new().small())
                        .child(widgets::caption("Checking what the other mods need…", cx))
                        .into_any_element(),
                    Some(b) => {
                        let gone: Vec<String> = with();
                        let hits = b.after(&gone);
                        if hits.is_empty() {
                            div().into_any_element()
                        } else {
                            let danger = cx.theme().danger;
                            v_flex()
                                .gap_1()
                                .mt_2()
                                .p_3()
                                .rounded(cx.theme().radius)
                                .border_1()
                                .border_color(danger.opacity(0.5))
                                .bg(danger.opacity(0.08))
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .text_sm()
                                        .font_semibold()
                                        .text_color(danger)
                                        .child(Icon::new(IconName::TriangleAlert).small())
                                        .child("The game won't start without these:"),
                                )
                                .children(hits.iter().take(12).map(|h| div().text_sm().child(h.clone())))
                                .when(hits.len() > 12, |c| {
                                    c.child(widgets::caption(format!("and {} more", hits.len() - 12), cx).text_xs())
                                })
                                .into_any_element()
                        }
                    }
                })
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().child(Button::new("remove-cancel").ghost().label("Cancel")))
                        .child(
                            Button::new("remove-delete")
                                .outline()
                                .danger()
                                .icon(IconName::Trash)
                                .label("Delete")
                                .on_click(move |_, window, cx| {
                                    v2.update(cx, |v, cx| v.delete(f2.clone(), w3.clone(), window, cx));
                                    window.close_dialog(cx);
                                }),
                        )
                        .child(DialogAction::new().child(
                            Button::new("remove-backup").primary().icon(IconName::Archive).label("Move to backups"),
                        )),
                )
                .on_ok(move |_, window, cx| {
                    view.update(cx, |v, cx| v.remove(files.clone(), w2.clone(), window, cx));
                    true
                })
        });
    }

    /// Deletes files (to the Recycle Bin), no backup kept.
    fn delete(&mut self, files: Vec<String>, what: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let r = self.store.update(cx, |s, cx| {
            let r = s.delete_files(&root, &files);
            cx.notify();
            r
        });
        let n = match r {
            Ok(()) if files.len() == 1 => Notification::success(format!("Deleted {what} (it's in the Recycle Bin)")),
            Ok(()) => Notification::success(format!("Deleted {} files (they're in the Recycle Bin)", files.len())),
            Err(e) => Notification::error(e),
        };
        window.push_notification(n, cx);
    }

    /// Moves files to `.hopper-backups`, then offers to put them back.
    pub(crate) fn remove(&mut self, files: Vec<String>, what: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            return;
        };
        let store = self.store.clone();
        // Where each file is now and its entry, to find and restore it.
        let before: Vec<(PathBuf, Option<Addon>)> = store
            .read(cx)
            .instance(&root)
            .map(|inst| {
                files
                    .iter()
                    .map(|f| match inst.addons.iter().find(|a| &a.file_name == f) {
                        Some(a) => (inst.addon_path(a), Some(a.clone())),
                        None => (inst.mods_dir().join(f), None),
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Backup folders are named by the second; allow for a tick.
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
            .saturating_sub(1);
        let r = store.update(cx, |s, cx| {
            let r = s.remove_files(&root, &files);
            cx.notify();
            r
        });
        let moved: Vec<Removed> = before
            .into_iter()
            .filter(|(original, _)| !original.exists())
            .filter_map(|(original, addon)| {
                let backup = find_backup(&root, &original, since)?;
                Some(Removed {
                    original,
                    backup,
                    addon,
                })
            })
            .collect();
        if let Err(e) = r {
            window.push_notification(Notification::error(e), cx);
        }
        if !moved.is_empty() {
            let n = moved.len();
            let undone = Rc::new(std::cell::Cell::new(false));
            let note = Notification::success(if n == 1 {
                format!("Removed {what}. It's in ⋯ › Backups.")
            } else {
                format!("Removed {n} files. They're in ⋯ › Backups.")
            })
            .action(move |_, _, cx| {
                let (store, root, moved, undone) =
                    (store.clone(), root.clone(), moved.clone(), undone.clone());
                Button::new("undo-remove")
                    .label("Undo")
                    .disabled(undone.get())
                    .on_click(cx.listener(move |note, _, window, cx| {
                        if !undone.replace(true) {
                            undo_remove(&store, root.clone(), moved.clone(), window, cx);
                        }
                        note.dismiss(window, cx);
                    }))
            });
            window.push_notification(note, cx);
        }
        self.selected.clear();
        // A loose jar leaves the instance as it was: read the folder again.
        self.sync(true, cx);
        cx.notify();
    }

    fn run_updates(&mut self, updates: Vec<Update>, cx: &mut Context<Self>) {
        let Some(root) = &self.root else { return };
        let store = self.store.read(cx);
        // A running update job queues these; any other job holds the
        // instance.
        if store.queues_updates(root) || store.edit_hold_reason(root).is_none() {
            store.run_updates(root, updates);
        }
        cx.notify();
    }

    fn set_sort(&mut self, col: SortCol, cx: &mut Context<Self>) {
        self.sort = if self.sort.0 == col {
            (col, !self.sort.1)
        } else {
            // Dates read newest first; text reads A to Z.
            (col, col != SortCol::Added)
        };
        cx.notify();
    }

    // ---- rendering -----------------------------------------------------

    fn toolbar(
        &self,
        inst: &Instance,
        updates: &HashMap<u64, Update>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let of_class = || inst.addons.iter().filter(|a| in_class(a, self.class));
        let n_up = of_class()
            .filter(|a| updates.contains_key(&a.project_id))
            .count();
        let n_off = of_class().filter(|a| !a.enabled).count();
        let n_added = of_class().filter(|a| !a.from_pack).count();
        let show_ix = match self.show {
            Show::All => 0,
            Show::Updates => 1,
            Show::Disabled => 2,
        };
        let status: AnyElement = match self.store.read(cx).updates(&inst.root) {
            Some(Updates::Checking) => h_flex()
                .gap_2()
                .child(Spinner::new().small())
                .child(widgets::caption("Checking for updates", cx))
                .into_any_element(),
            Some(Updates::Failed(e)) => div()
                .text_sm()
                .text_color(cx.theme().danger)
                .truncate()
                .child(e.clone())
                .into_any_element(),
            Some(Updates::Ready(_)) if updates.is_empty() => h_flex()
                .gap_1p5()
                .text_sm()
                .text_color(cx.theme().success)
                .child(Icon::new(IconName::CircleCheck).small())
                .child("Up to date")
                .into_any_element(),
            Some(Updates::Ready(_)) => {
                let store = self.store.read(cx);
                // What isn't already updating or queued.
                let all: Vec<Update> = updates
                    .values()
                    .filter(|u| store.update_state(&inst.root, u.project_id).is_none())
                    .cloned()
                    .collect();
                let queues = store.queues_updates(&inst.root);
                let hold = if queues { None } else { store.edit_hold_reason(&inst.root) };
                let busy = !queues && store.is_busy(&inst.root);
                Button::new("update-all")
                    .primary()
                    .small()
                    .icon(IconName::Download)
                    .label(format!("Update all ({})", all.len()))
                    .loading(all.is_empty() || busy)
                    .disabled(hold.is_some())
                    .when_some(hold, |b, why| b.tooltip(why))
                    .on_click(cx.listener(move |this, _, _, cx| this.run_updates(all.clone(), cx)))
                    .into_any_element()
            }
            None => div().into_any_element(),
        };
        let root = inst.root.clone();
        h_flex()
            .px_6()
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
                Button::new("add-content")
                    .small()
                    .primary()
                    .icon(IconName::Plus)
                    .label("Add content")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(root) = this.root.clone() {
                            cx.emit(AddonsEvent::AddContent {
                                root,
                                class: this.class,
                            });
                        }
                    })),
            )
            .when(self.class == cf::CLASS_MODS, |r| r.child(self.profiles_button(inst, cx)))
            .child(
                TabBar::new("show")
                    .segmented()
                    .small()
                    .selected_index(show_ix)
                    .child(Tab::new().label("All"))
                    .child(Tab::new().label(format!("Updates {n_up}")))
                    .child(Tab::new().label(format!("Disabled {n_off}")))
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.show = [Show::All, Show::Updates, Show::Disabled][*ix];
                        cx.notify();
                    })),
            )
            .when(inst.pack.is_some(), |r| {
                r.child(
                    Checkbox::new("added-only")
                        .label(format!("Added by me ({n_added})"))
                        .tooltip("Only what you added on top of the modpack")
                        .checked(self.added_only)
                        .on_change(cx.listener(|this, v: &bool, _, cx| {
                            this.added_only = *v;
                            cx.notify();
                        })),
                )
            })
            .when(
                self.class == cf::CLASS_MODS
                    && self.store.read(cx).server_of(&inst.root).is_some(),
                |r| {
                    r.child(
                        Checkbox::new("server-column")
                            .label("Server")
                            .tooltip("Show each mod's state on the paired rig server")
                            .checked(self.store.read(cx).settings().mods_server_column)
                            .on_change(cx.listener(|this, v: &bool, _, cx| {
                                let v = *v;
                                this.store.update(cx, |s, cx| {
                                    s.update_settings(|st| st.mods_server_column = v, cx)
                                });
                                this.marks = None;
                                this.want_marks(cx);
                            })),
                    )
                },
            )
            .child(
                Checkbox::new("updates-first")
                    .label("Updates first")
                    .checked(self.updates_first)
                    .on_change(cx.listener(|this, v: &bool, _, cx| {
                        this.updates_first = *v;
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
            .when(self.store.read(cx).running(&inst.root).is_some(), |r| {
                r.child(
                    widgets::caption("The game is running: changes apply on its next start", cx)
                        .text_xs(),
                )
            })
            // The update status and its refresh wrap as one.
            .child(
                h_flex().flex_none().gap_1().child(status).child(
                    Button::new("recheck")
                        .ghost()
                        .small()
                        .icon(IconName::RefreshCw)
                        .tooltip("Check for updates again")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.store.update(cx, |s, cx| {
                                s.check_updates(&root, true);
                                cx.notify();
                            })
                        })),
                ),
            )
    }

    fn bulk_bar(
        &self,
        inst: &Instance,
        updates: &HashMap<u64, Update>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.selected.is_empty() {
            return None;
        }
        let files: Vec<String> = self.selected.iter().cloned().collect();
        let ups: Vec<Update> = files
            .iter()
            .filter_map(|f| inst.addon_by_file(f))
            .filter_map(|a| updates.get(&a.project_id).cloned())
            .collect();
        // Lock unless every picked mod is locked already.
        let all_locked = files
            .iter()
            .filter_map(|f| inst.addon_by_file(f))
            .all(|a| a.locked);
        let (f1, f2, f3, f4) = (files.clone(), files.clone(), files.clone(), files);
        let theme = cx.theme();
        // Floats over the table, bottom centre, so the list doesn't jump.
        let bar = h_flex()
            .id("bulk-bar")
            .px_2()
            .py_1p5()
            .gap_1()
            .items_center()
            .rounded(theme.radius_lg)
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .border_1()
            .border_color(theme.border)
            .shadow_xl()
            .occlude()
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .px_2()
                    .child(format!("{} selected", self.selected.len())),
            )
            .child(div().w_px().h_5().bg(theme.border))
            .child(
                Button::new("bulk-enable")
                    .small()
                    .ghost()
                    .icon(IconName::CircleCheck)
                    .label("Enable")
                    .on_click(
                        cx.listener(move |this, _, w, cx| this.toggle(f1.clone(), true, w, cx)),
                    ),
            )
            .child(
                Button::new("bulk-disable")
                    .small()
                    .ghost()
                    .icon(IconName::CircleOff)
                    .label("Disable")
                    .on_click(
                        cx.listener(move |this, _, w, cx| this.toggle(f2.clone(), false, w, cx)),
                    ),
            )
            .child(
                Button::new("bulk-lock")
                    .small()
                    .ghost()
                    .icon(if all_locked { IconName::LockOpen } else { IconName::Lock })
                    .label(if all_locked { "Unlock" } else { "Lock" })
                    .tooltip(if all_locked {
                        "Allow updates again"
                    } else {
                        "Keep this version: updates are ignored"
                    })
                    .on_click(cx.listener(move |this, _, w, cx| {
                        this.set_locks(f4.clone(), !all_locked, w, cx)
                    })),
            )
            .when(!ups.is_empty(), |r| {
                let n = ups.len();
                r.child(
                    Button::new("bulk-update")
                        .small()
                        .primary()
                        .icon(IconName::Download)
                        .label(format!("Update {n}"))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.run_updates(ups.clone(), cx)),
                        ),
                )
            })
            .child(
                Button::new("bulk-remove")
                    .small()
                    .ghost()
                    .danger()
                    .icon(IconName::Trash)
                    .label("Remove")
                    .on_click(
                        cx.listener(move |this, _, w, cx| this.ask_remove(f3.clone(), w, cx)),
                    ),
            )
            .child(div().w_px().h_5().bg(theme.border))
            .child(
                Button::new("bulk-clear")
                    .small()
                    .ghost()
                    .icon(IconName::X)
                    .tooltip("Clear selection")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.selected.clear();
                        cx.notify();
                    })),
            );
        Some(
            h_flex()
                .absolute()
                .bottom_6()
                .left_0()
                .right_0()
                .justify_center()
                .child(widgets::enter("bulk-bar-in", div().child(bar)))
                .into_any_element(),
        )
    }

    fn header(
        &self,
        rows: &[Row],
        inst: &Instance,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let files: Vec<String> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Addon(i) => inst.addons.get(*i).map(|a| a.file_name.clone()),
                _ => None,
            })
            .collect();
        let all = !files.is_empty() && files.iter().all(|f| self.selected.contains(f));
        let theme = cx.theme();
        let (sort, asc) = self.sort;
        let head = |col: SortCol, label: &'static str, cell: Div| {
            let active = sort == col;
            cell.child(
                h_flex()
                    .id(label)
                    .gap_1()
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme.foreground))
                    .when(active, |r| r.text_color(theme.foreground))
                    .child(label)
                    .when(active, |r| {
                        r.child(
                            Icon::new(if asc {
                                IconName::ArrowUp
                            } else {
                                IconName::ArrowDown
                            })
                            .xsmall(),
                        )
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.set_sort(col, cx))),
            )
        };
        h_flex()
            .mx_6()
            .pl_3()
            .pr_2()
            .py_2()
            .gap_3()
            .border_b_1()
            .border_color(theme.border)
            .text_xs()
            .font_medium()
            .text_color(theme.muted_foreground)
            .child(
                col_check().child(
                    Checkbox::new("select-all")
                        .checked(all)
                        .on_change(cx.listener(move |this, v: &bool, _, cx| {
                            if *v {
                                this.selected.extend(files.iter().cloned());
                            } else {
                                this.selected.clear();
                            }
                            cx.notify();
                        })),
                ),
            )
            .child(div().w_10().flex_none())
            .child(head(SortCol::Name, "Name", div().flex_1().min_w_0()))
            .child(col_action())
            .when(self.author_fits, |r| {
                r.child(head(SortCol::Author, "Author", col_author()))
            })
            .child(head(SortCol::Env, "Side", col_env()))
            .child(head(SortCol::Active, "On", col_active()))
            .child(head(SortCol::Added, "Added", col_date()))
            .child(col_trash())
    }

    fn render_rows(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        // All from the table built in `sync`: scrolling reads no files and
        // clones no instance, only these reference counts.
        self.ensure_rows();
        let Some(inst) = self.table.inst.clone() else {
            return Vec::new();
        };
        let updates = self.table.updates.clone();
        let rows = self.table.rows.clone();
        let active_rp = self.table.active_rp.clone();
        let active_shader = self.table.shader.as_ref().map(|s| s.to_lowercase());
        let now = hopper::time::parse_iso(&hopper::time::now_iso()).unwrap_or(0);
        let cursor = self.cursor.filter(|_| self.focus.is_focused(window));
        let ring = cx.theme().ring;
        let start = range.start.min(rows.len());
        rows[start..range.end.min(rows.len())]
            .iter()
            .enumerate()
            .map(|(k, row)| (start + k, row))
            .map(|(ix, row)| {
                let el = self.render_row(row, &inst, &updates, &active_rp, &active_shader, now, cx);
                div()
                    .relative()
                    // A click puts the keys on this row.
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.cursor = Some(ix);
                            window.focus(&this.focus, cx);
                        }),
                    )
                    .child(el)
                    .when(cursor == Some(ix), |d| {
                        // A ring that doesn't move the row.
                        d.child(
                            div()
                                .absolute()
                                .inset_0()
                                .rounded(cx.theme().radius_lg)
                                .border_1()
                                .border_color(ring),
                        )
                    })
                    .into_any_element()
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_row(
        &mut self,
        row: &Row,
        inst: &Instance,
        updates: &HashMap<u64, Update>,
        active_rp: &HashSet<String>,
        active_shader: &Option<String>,
        now: i64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match row {
                Row::Addon(i) => {
                    let a = &inst.addons[*i];
                    let f = a.file_name.to_lowercase();
                    let in_use = active_rp.contains(&f)
                        || active_shader
                            .as_ref()
                            .is_some_and(|s| *s == f || *s == f.trim_end_matches(".zip"));
                    let new = hopper::time::parse_iso(&a.date_added)
                        .is_some_and(|t| now - t < 3 * 86_400);
                    let flags = Flags {
                        added: inst.pack.is_some() && !a.from_pack,
                        in_use,
                        new,
                    };
                    self.addon_row(a, updates.get(&a.project_id), flags, cx)
                }
                Row::LooseHeader(n) => h_flex()
                    .h(rems(ROW_H))
                    .w_full()
                    .px_3()
                    .gap_3()
                    .child(widgets::caption(
                        format!("{n} jars not from CurseForge"),
                        cx,
                    ))
                    .child(
                        Button::new("identify")
                            .small()
                            .outline()
                            .icon(IconName::ScanSearch)
                            .label("Identify on CurseForge")
                            .tooltip("Match them by fingerprint, like CurseForge's scan")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(r) = &this.root {
                                    this.store.read(cx).identify(r);
                                }
                            })),
                    )
                    .into_any_element(),
                Row::Loose(name) => {
                    let file = name.clone();
                    h_flex()
                        .id(ElementId::Name(format!("loose:{name}").into()))
                        .h(rems(ROW_H))
                        .w_full()
                        .pl_3()
                        .pr_2()
                        .gap_3()
                        .rounded(cx.theme().radius)
                        .hover(|s| s.bg(cx.theme().list_hover))
                        .child(col_check())
                        .child(widgets::thumb(None, rems(2.5), cx))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .italic()
                                .truncate()
                                .child(name.clone()),
                        )
                        .child(
                            col_trash().child(
                                Button::new(ElementId::Name(format!("rm-loose:{name}").into()))
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Trash)
                                    .tooltip("Remove (kept in .hopper-backups)")
                                    .on_click(cx.listener(move |this, _, w, cx| {
                                        this.ask_remove(vec![file.clone()], w, cx)
                                    })),
                            ),
                        )
                        .into_any_element()
                }
            }
    }

    /// Moves the key cursor to `ix` (clamped) and scrolls it into view.
    fn move_cursor(&mut self, ix: isize, cx: &mut Context<Self>) {
        let n = self.row_count(cx);
        if n == 0 {
            return;
        }
        let ix = ix.clamp(0, n as isize - 1) as usize;
        self.cursor = Some(ix);
        self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn row_count(&mut self, _: &mut Context<Self>) -> usize {
        self.ensure_rows();
        self.table.rows.len()
    }

    fn cursor_row(&mut self, _: &mut Context<Self>) -> Option<(Rc<Instance>, Row)> {
        let ix = self.cursor?;
        self.ensure_rows();
        let inst = self.table.inst.clone()?;
        let row = self.table.rows.get(ix)?.clone();
        Some((inst, row))
    }

    fn on_row_key(&mut self, step: isize, cx: &mut Context<Self>) {
        let at = self.cursor.map_or(-1, |c| c as isize);
        self.move_cursor(at + step, cx);
    }

    fn on_toggle(&mut self, _: &RowToggle, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((inst, Row::Addon(i))) = self.cursor_row(cx) {
            let a = &inst.addons[i];
            self.toggle(vec![a.file_name.clone()], !a.enabled, window, cx);
        }
    }

    fn on_open(&mut self, _: &RowOpen, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((inst, Row::Addon(i))) = self.cursor_row(cx) {
            let pid = inst.addons[i].project_id;
            if pid != 0 {
                let root = inst.root.clone();
                self.peek
                    .update(cx, |p, cx| p.peek_project(root, Some(pid), cx));
            }
        }
    }

    fn addon_row(
        &self,
        a: &Addon,
        update: Option<&Update>,
        flags: Flags,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let store = self.store.read(cx);
        let theme = cx.theme();
        let hold = self
            .root
            .as_ref()
            .filter(|r| !store.queues_updates(r))
            .and_then(|r| store.edit_hold_reason(r));
        // `Some(true)` downloading now, `Some(false)` queued.
        let updating = self.root.as_ref().and_then(|r| store.update_state(r, a.project_id));
        let icon = store.image(&a.icon).map(Path::to_path_buf);
        let file = a.file_name.clone();
        let picked = self.selected.contains(&file);
        let dim = !a.enabled;
        let id = |p: &str| ElementId::Name(format!("{p}:{file}").into());
        let (f_sel, f_on, f_rm, f_menu) = (file.clone(), file.clone(), file.clone(), file.clone());
        let on = a.enabled;

        // The Server column: this mod's state on the paired server.
        let mark = self
            .marks
            .as_ref()
            .and_then(|(_, _, m)| m.get(&a.file_name.to_lowercase()))
            .map(|e| {
                use hopper::model::DiffStatus::*;
                let (color, text) = match (e.status, &e.skip_reason) {
                    (Same, _) => (theme.success, "On the server".to_string()),
                    (ClientNewer, _) => (theme.info, "The server has an older version".to_string()),
                    (ServerNewer, _) => (theme.warning, "The server has a newer version".to_string()),
                    (OnlyClient, Some(r)) => (theme.muted_foreground, format!("Kept off the server: {r}")),
                    (OnlyClient, None) => (theme.primary, "Not on the server yet".to_string()),
                    _ => (theme.muted_foreground, String::new()),
                };
                (color, SharedString::from(format!("Server: {text}")))
            });
        let side = crate::side_card::side_of(&a.env);
        let card = self.side_card(a, cx);

        let tags = widgets::mod_tags(
            widgets::ModTags {
                in_use: flags.in_use,
                new: flags.new,
                added: flags.added,
                env: &a.env,
                categories: store.categories(a),
                modrinth: !a.mr_project.is_empty(),
            },
            cx,
        );
        let name_line = h_flex()
            .gap_1p5()
            .child({
                let pid = a.project_id;
                div()
                    .id(id("name"))
                    .min_w_0()
                    .text_sm()
                    .font_medium()
                    .truncate()
                    .when(pid != 0, |d| {
                        d.cursor_pointer()
                            .hover(|s| s.text_color(theme.primary))
                            .tooltip(|w, cx| {
                                gpui_kit::component::tooltip::Tooltip::new("Details").build(w, cx)
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.open_peek(pid, cx);
                            }))
                    })
                    .child(a.name.clone())
            })
            .when(a.locked, |r| {
                r.child(
                    div()
                        .id(id("locked"))
                        .text_color(theme.muted_foreground)
                        .tooltip(|w, cx| {
                            gpui_kit::component::tooltip::Tooltip::new(
                                "Updates ignored. Right-click to allow them.",
                            )
                            .build(w, cx)
                        })
                        .child(Icon::new(IconName::Lock).xsmall()),
                )
            });

        let action = col_action().when_some(update.cloned(), |c, u| {
            let tip = format!("{}  →  {}", u.from, u.file.file_name);
            let (pid, file) = (u.project_id, u.file.clone());
            c.child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new(id("update"))
                            .small()
                            .outline()
                            .icon(IconName::Download)
                            .label(match updating {
                                Some(true) => "Updating",
                                Some(false) => "Queued",
                                None => "Update",
                            })
                            .loading(updating == Some(true))
                            .tooltip(hold.map(String::from).unwrap_or(tip))
                            .disabled(dim || hold.is_some() || updating.is_some())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.run_updates(vec![u.clone()], cx)
                            })),
                    )
                    .child(
                        Button::new(id("changes"))
                            .ghost()
                            .xsmall()
                            .icon(IconName::ScrollText)
                            .tooltip("What changed")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.versions(pid, Some(file.clone()), window, cx)
                            })),
                    ),
            )
        });

        let view = cx.entity();
        let (pid_menu, update_file) = (a.project_id, update.map(|u| u.file.clone()));
        let (page, locked, mr, has_project) = (
            a.page.clone(),
            a.locked,
            !a.mr_project.is_empty(),
            a.project_id != 0,
        );
        h_flex()
            .id(id("row"))
            .h(rems(ROW_H))
            .w_full()
            .pl_3()
            .pr_2()
            .gap_3()
            .relative()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.transparent)
            .when(picked, |r| r.bg(theme.list_active).border_color(theme.primary.opacity(0.25)))
            .when(!picked, |r| {
                r.hover(|s| s.bg(theme.list_hover).border_color(theme.border))
            })
            // Additions on top of the pack get a stripe on the leading edge.
            .when(flags.added, |r| {
                r.child(
                    div()
                        .absolute()
                        .left_0()
                        .top_2()
                        .bottom_2()
                        .w_0p5()
                        .rounded_full()
                        .bg(theme.warning),
                )
            })
            .child(
                col_check().child(
                    Checkbox::new(id("sel"))
                        .checked(picked)
                        .on_change(cx.listener(move |this, v: &bool, _, cx| {
                            if *v {
                                this.selected.insert(f_sel.clone());
                            } else {
                                this.selected.remove(&f_sel);
                            }
                            cx.notify();
                        })),
                ),
            )
            .child({
                let pid = a.project_id;
                div()
                    .id(id("thumb"))
                    .when(dim, |d| d.opacity(0.45))
                    .when(pid != 0, |d| {
                        d.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.open_peek(pid, cx);
                        }))
                    })
                    .child(widgets::thumb(icon.as_deref(), rems(2.5), cx))
            })
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .when(dim, |d| d.opacity(0.55))
                    .child(name_line)
                    .child(h_flex().gap_2().min_w_0().child(tags).child({
                        // File name and version chip. Click either to pick
                        // another version.
                        let (pid, upd) = (a.project_id, update.map(|u| u.file.clone()));
                        let primary = theme.primary;
                        let chip = h_flex()
                            .flex_none()
                            .max_w(rems(16.))
                            .min_w_0()
                            .overflow_hidden()
                            .gap_1()
                            .px_1p5()
                            .rounded(theme.radius)
                            .text_xs();
                        let chip = match update.map(|u| u.versions()) {
                            // The update inline: installed → offered.
                            Some((old, new)) => Some(
                                chip.bg(primary.opacity(0.1))
                                    .group_hover("file", move |s| s.bg(primary.opacity(0.2)))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(theme.muted_foreground)
                                            .child(old),
                                    )
                                    .child(
                                        Icon::new(IconName::ArrowRight)
                                            .xsmall()
                                            .flex_none()
                                            .text_color(primary),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(primary)
                                            .child(new),
                                    ),
                            ),
                            None => {
                                let v = hopper::scan::short_version(&a.file_name);
                                (!v.is_empty()).then(|| {
                                    chip.bg(theme.muted)
                                        .text_color(theme.muted_foreground)
                                        .group_hover("file", move |s| {
                                            s.bg(primary.opacity(0.1)).text_color(primary)
                                        })
                                        .child(div().min_w_0().truncate().child(v))
                                })
                            }
                        };
                        h_flex()
                            .id(id("file"))
                            .min_w_0()
                            .gap_2()
                            .children(chip)
                            .child(
                                widgets::caption(a.file_name.clone(), cx)
                                    .text_xs()
                                    .min_w_0()
                                    .group_hover("file", move |s| s.text_color(primary)),
                            )
                            .when(pid != 0, |d| {
                                d.cursor_pointer()
                                    .group("file")
                                    .child(
                                        div()
                                            .flex_none()
                                            .ml(px(-4.))
                                            .invisible()
                                            .group_hover("file", |s| s.visible())
                                            .text_color(primary)
                                            .child(Icon::new(IconName::ChevronDown).xsmall()),
                                    )
                                    .tooltip(|w, cx| {
                                        gpui_kit::component::tooltip::Tooltip::new("Change version")
                                            .build(w, cx)
                                    })
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.versions(pid, upd.clone(), window, cx)
                                    }))
                            })
                    })),
            )
            .child(action)
            // Room for the project panel.
            .when(self.author_fits, |r| {
                r.child(
                    col_author()
                        .text_sm()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(a.author.clone()),
                )
            })
            .child(col_env().child(crate::side_card::wrap(
                id("side-card"),
                h_flex()
                    .id(id("side"))
                    .gap_1()
                    .px_1()
                    .py_0p5()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.list_hover))
                    .child(
                        // A fixed slot, so the server dot lines up on every row.
                        div()
                            .size_4()
                            .flex_none()
                            .text_color(theme.muted_foreground)
                            .when_some(side, |d, (icon, _)| d.child(Icon::new(icon).small())),
                    )
                    .when_some(mark, |c, (color, _)| {
                        c.child(div().size_2().rounded_full().bg(color))
                    }),
                card,
            )))
            .child(
                col_active().child(
                    Switch::new(id("on"))
                        .small()
                        .checked(on)
                        .tooltip(if on { "Disable" } else { "Enable" })
                        .on_change(cx.listener(move |this, v: &bool, w, cx| {
                            this.toggle(vec![f_on.clone()], *v, w, cx)
                        })),
                ),
            )
            .child(
                col_date()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(hopper::time::relative(&a.date_added)),
            )
            .child(
                col_trash().child(
                    Button::new(id("rm"))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Trash)
                        .tooltip("Remove (kept in .hopper-backups)")
                        .on_click(cx.listener(move |this, _, w, cx| {
                            this.ask_remove(vec![f_rm.clone()], w, cx)
                        })),
                ),
            )
            .context_menu(move |menu, _, _| {
                let f = f_menu.clone();
                let (v1, v2, v3, v4, v5) = (
                    view.clone(),
                    view.clone(),
                    view.clone(),
                    view.clone(),
                    view.clone(),
                );
                let (f1, f2, f3, f4, f5) = (f.clone(), f.clone(), f.clone(), f.clone(), f.clone());
                let page = page.clone();
                let (v6, v7) = (view.clone(), view.clone());
                let upd = update_file.clone();
                menu.when(has_project, |m| {
                    m.item(
                        PopupMenuItem::new("Open in Browse")
                            .icon(IconName::Search)
                            .on_click(move |_, _, cx| {
                                v7.update(cx, |this, cx| {
                                    if let Some(root) = this.root.clone() {
                                        cx.emit(AddonsEvent::OpenProject { root, project: pid_menu });
                                    }
                                })
                            }),
                    )
                    .item(
                        PopupMenuItem::new("Versions and changelogs")
                            .icon(IconName::Clock)
                            .on_click(move |_, window, cx| {
                                v6.update(cx, |this, cx| this.versions(pid_menu, upd.clone(), window, cx))
                            }),
                    )
                })
                .item(
                    PopupMenuItem::new("Show in folder")
                        .icon(IconName::FolderOpen)
                        .on_click(move |_, _, cx| {
                            v1.update(cx, |this, cx| this.show_in_folder(&f1, cx))
                        }),
                )
                .when(!page.is_empty(), |m| {
                    m.item(
                        PopupMenuItem::new("Open on CurseForge")
                            .icon(IconName::ExternalLink)
                            .on_click(move |_, _, cx| cx.open_url(&page)),
                    )
                })
                .when(has_project, |m| {
                    m.item(
                        PopupMenuItem::new(if mr {
                            "Switch to CurseForge"
                        } else {
                            "Switch to Modrinth"
                        })
                        .icon(IconName::Shuffle)
                        .on_click(move |_, _, cx| {
                            v2.update(cx, |this, cx| {
                                if let Some(r) = &this.root {
                                    this.store.read(cx).switch_source(r, f2.clone());
                                }
                            })
                        }),
                    )
                })
                .item(
                    PopupMenuItem::new(if locked {
                        "Allow updates"
                    } else {
                        "Ignore updates"
                    })
                    .icon(if locked {
                        IconName::LockOpen
                    } else {
                        IconName::Lock
                    })
                    .on_click(move |_, window, cx| {
                        v3.update(cx, |this, cx| {
                            this.set_locks(vec![f3.clone()], !locked, window, cx)
                        })
                    }),
                )
                .item(
                    PopupMenuItem::new(if on { "Disable" } else { "Enable" }).on_click(
                        move |_, window, cx| {
                            v4.update(cx, |this, cx| {
                                this.toggle(vec![f4.clone()], !on, window, cx)
                            })
                        },
                    ),
                )
                .separator()
                .item(PopupMenuItem::new("Remove").icon(IconName::Trash).on_click(
                    move |_, window, cx| {
                        v5.update(cx, |this, cx| this.ask_remove(vec![f5.clone()], window, cx))
                    },
                ))
            })
            .into_any_element()
    }

    fn versions(
        &mut self,
        pid: u64,
        update: Option<hopper::cf::File>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(root) = self.root.clone() {
            crate::versions::open(self.store.clone(), root, pid, false, update, window, cx);
        }
    }

    fn show_in_folder(&self, file: &str, cx: &App) {
        let Some(root) = &self.root else { return };
        if let Some(inst) = self.store.read(cx).instance(root)
            && let Some(a) = inst.addon_by_file(file)
        {
            let _ = std::process::Command::new("explorer")
                .arg(format!("/select,{}", inst.addon_path(a).display()))
                .spawn();
        }
    }
}

/// Keeps `files` out of (or back in) update offers and version changes,
/// and tells CurseForge's own file so the CurseForge app agrees.
pub fn set_locks(
    store: &Entity<Store>,
    root: PathBuf,
    files: Vec<String>,
    locked: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let res = store.update(cx, |s, cx| {
        let res = s.set_locked_many(&root, &files, locked);
        cx.notify();
        res
    });
    if let Err(e) = res {
        window.push_notification(Notification::error(e), cx);
        return;
    }
    let store = store.clone();
    window
        .spawn(cx, async move |cx| {
            let root2 = root.clone();
            let r = cx
                .background_spawn(async move {
                    let mut synced = hopper::cf_instance::Synced::Nothing;
                    hopper::instances::Instance::update_on_disk(&root, |i| {
                        synced = match hopper::cf_instance::write_locks(i) {
                            Ok(s) => s,
                            Err(e) => {
                                log::warn!("writing CurseForge locks for {}: {e:#}", i.name);
                                hopper::cf_instance::Synced::Nothing
                            }
                        };
                    })
                    .map(|_| synced)
                })
                .await;
            cx.update(|window, cx| match r {
                // Pick up the new file time and cf_locked flags.
                Ok(hopper::cf_instance::Synced::Written) => {
                    store.read(cx).spawn(move || crate::store::Msg::InstanceChanged(root2))
                }
                Ok(hopper::cf_instance::Synced::CurseForgeOpen) => window.push_notification(
                    Notification::warning(
                        "Saved in Hopper. CurseForge is open, so it can still update these mods. Close it, then use Update CurseForge in the instance's Settings.",
                    ),
                    cx,
                ),
                _ => {}
            })
            .ok();
        })
        .detach();
}

/// Badges for a row.
#[derive(Clone, Copy)]
struct Flags {
    /// Added by the user on top of the instance's modpack.
    added: bool,
    /// Switched on in game (resource pack or shader).
    in_use: bool,
    /// Added in the last three days.
    new: bool,
}

impl Render for AddonsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // With the project panel open, Author goes only on a narrow window.
        self.author_fits = !self.peek.read(cx).is_open()
            || window.viewport_size().width / window.rem_size() >= 100.0;
        // Built by `sync` when the store changes; here only the filters
        // may need applying again (in memory).
        self.ensure_rows();
        let Some(inst) = self.table.inst.clone() else {
            return div().into_any_element();
        };
        let tabs = self.pack_tabs(&inst, cx);
        if let Some(global) = self.global_packs(&inst, cx) {
            return v_flex()
                .size_full()
                .gap_3()
                .children(tabs)
                .child(global)
                .into_any_element();
        }
        let missing = self.missing_banner(&inst, cx);
        let dups = self.duplicate_banner(&inst, cx);
        let shaders = self.shader_notes(&inst, cx);
        let updates = self.table.updates.clone();
        let rows = self.table.rows.clone();
        let toolbar = self.toolbar(&inst, &updates, cx);
        let bulk = self.bulk_bar(&inst, &updates, cx);
        let header = self.header(&rows, &inst, cx);
        let count = rows.len();
        let body: AnyElement = if count == 0 {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .child(widgets::caption(
                    if self.query.is_empty() && self.show == Show::All && !self.added_only {
                        "Nothing installed here yet."
                    } else {
                        "Nothing matches."
                    },
                    cx,
                ))
                .into_any_element()
        } else {
            div()
                .id("addon-table")
                .track_focus(&self.focus)
                .key_context("AddonsTable")
                .on_action(cx.listener(|this, _: &RowUp, _, cx| this.on_row_key(-1, cx)))
                .on_action(cx.listener(|this, _: &RowDown, _, cx| this.on_row_key(1, cx)))
                .on_action(cx.listener(|this, _: &RowPageUp, _, cx| this.on_row_key(-10, cx)))
                .on_action(cx.listener(|this, _: &RowPageDown, _, cx| this.on_row_key(10, cx)))
                .on_action(cx.listener(|this, _: &RowFirst, _, cx| this.move_cursor(0, cx)))
                .on_action(cx.listener(|this, _: &RowLast, _, cx| this.move_cursor(isize::MAX, cx)))
                .on_action(cx.listener(Self::on_toggle))
                .on_action(cx.listener(Self::on_open))
                .flex_1()
                .min_h_0()
                .relative()
                .child(
                    uniform_list(
                        "addon-rows",
                        count,
                        cx.processor(|this, range: Range<usize>, window, cx| {
                            this.render_rows(range, window, cx)
                        }),
                    )
                    .track_scroll(&self.scroll)
                    .size_full()
                    .px_6()
                    // Room for the floating selection bar under the last row.
                    .map(|l| if self.selected.is_empty() { l.pb_4() } else { l.pb_20() }),
                )
                .vertical_scrollbar(&self.scroll)
                .into_any_element()
        };
        let list = v_flex()
            .gap_3()
            .children(tabs)
            .child(toolbar)
            .children(dups)
            .children(missing)
            .children(shaders)
            .child(v_flex().flex_1().min_h_0().child(header).child(body));
        list.size_full().relative().children(bulk).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::find_backup;

    #[test]
    fn finds_the_newest_backup_of_a_removed_file() {
        let root = std::env::temp_dir().join(format!("hopper-undo-test-{}", std::process::id()));
        let b = root.join(hopper::sync::BACKUP_DIR);
        for (dir, name) in [("100", "a.jar"), ("200", "a.jar"), ("200", "2-a.jar"), ("200", "b.jar")] {
            std::fs::create_dir_all(b.join(dir)).unwrap();
            std::fs::write(b.join(dir).join(name), "x").unwrap();
        }
        let original = root.join("mods").join("a.jar");
        assert_eq!(find_backup(&root, &original, 150), Some(b.join("200").join("2-a.jar")));
        assert_eq!(find_backup(&root, &original, 300), None);
        assert_eq!(find_backup(&root, &root.join("mods").join("c.jar"), 0), None);
        // Real `sync::backup_into` lands where it looks.
        std::fs::create_dir_all(root.join("mods")).unwrap();
        std::fs::write(root.join("mods").join("c.jar"), "y").unwrap();
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 1;
        let moved = hopper::sync::backup_into(&root, &root.join("mods").join("c.jar"))
            .unwrap()
            .unwrap();
        assert_eq!(find_backup(&root, &root.join("mods").join("c.jar"), since), Some(moved));
        let _ = std::fs::remove_dir_all(&root);
    }
}
