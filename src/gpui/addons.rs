//! The Mods, Resource packs, Shaders and Data packs tabs: a sortable, filterable table
//! of what an instance has installed, with updates, toggles and removal.

use crate::drag_pick::{DragPick, PickDrag, Picks};
use crate::store::{Store, Updates};
use crate::widgets::{self, Clickable as _};
use gpui_kit::assets::IconName;
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
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
pub(crate) enum Row {
    /// Index into `Instance::addons`.
    Addon(usize),
    /// "N jars not from CurseForge".
    LooseHeader(usize),
    Loose(String),
    /// A group's heading: resource packs "In game · top wins", "Not in use".
    Section(&'static str, &'static str),
    /// Shaders' "Off" choice, above the packs.
    ShaderOff,
    /// Resource packs a global folder (Paxi…) also has and loads itself,
    /// folded away: how many, whether shown, and where.
    Copies(usize, bool, String),
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
pub(crate) fn col_check() -> Div {
    div().w_6().flex_none()
}
/// The Update / Changelog column: wide enough for "Updating 100%" and the
/// changelog half, or with `compact` the icon-only pill.
pub(crate) fn col_action(compact: bool) -> Div {
    div()
        .w(rems(if compact { widgets::SPLIT_W_COMPACT + 0.25 } else { 9.5 }))
        .flex_none()
}
pub(crate) fn col_author() -> Div {
    div().w_40().flex_none()
}
pub(crate) fn col_env() -> Div {
    div().w_12().flex_none()
}
pub(crate) fn col_active() -> Div {
    div().w_16().flex_none()
}
pub(crate) fn col_date() -> Div {
    div().w_24().flex_none()
}
/// The row's ⋯ menu and Remove.
pub(crate) fn col_trash() -> Div {
    h_flex().w_16().flex_none().justify_end()
}

/// The name column's narrowest (columns step aside below it) and widest
/// (on wide windows the actions follow the name instead of the far edge).
const NAME_MIN: f32 = 16.0;
const NAME_MAX: f32 = 40.0;

/// Which optional columns the table shows, fitted to its room each frame.
/// Header and rows read the same value so they line up.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Cols {
    pub(crate) date: bool,
    /// Off: the author goes into the name's tooltip.
    pub(crate) author: bool,
    pub(crate) side: bool,
    /// The Side icon in the name line, once its column has gone.
    pub(crate) side_inline: bool,
    /// The name stops at NAME_MAX and a spacer takes the rest.
    pub(crate) cap: bool,
}

impl Default for Cols {
    fn default() -> Self {
        Self { date: true, author: true, side: true, side_inline: false, cap: false }
    }
}

/// Row height: a 40 px icon plus breathing room.
pub(crate) const ROW_H: f32 = 3.5;

/// A row's hover group, for controls shown only on the hovered row.
pub(crate) const ROW_GROUP: &str = "addon-row";

/// What the table shows, built from the store and the disk when the
/// instance, its updates or the jobs change, and kept between frames: the
/// store redraws every 50 ms while jobs run, and scrolling redraws every
/// frame, so nothing here may read files or clone the instance per frame.
#[derive(Default)]
pub(crate) struct Table {
    /// Fingerprint of the store state it was built from.
    pub(crate) sig: Option<u64>,
    pub(crate) inst: Option<Rc<Instance>>,
    /// Pending updates of this tab's class, by project.
    pub(crate) updates: Rc<HashMap<u64, Update>>,
    /// Lower-case resource pack files switched on in game.
    pub(crate) active_rp: Rc<HashSet<String>>,
    /// The shader pack on in game, as options say it.
    pub(crate) shader: Option<String>,
    pub(crate) has_shader_loader: bool,
    /// Other locations holding the same file, by lower-case file name
    /// (resource and data packs), read when the table is built.
    pub(crate) also: Rc<HashMap<String, String>>,
    /// Resource packs in use: each addon's place in game order (1 = top).
    pub(crate) rank: Rc<HashMap<usize, usize>>,
    /// The filtered, sorted rows, and the filters they are for.
    pub(crate) rows: Rc<Vec<Row>>,
    pub(crate) rows_for: Option<RowsKey>,
    /// Some row has a side (client / server / both): else no Side column.
    pub(crate) has_side: bool,
}

/// The view's filters and sort, as the rows were built for them.
#[derive(Clone, PartialEq)]
pub(crate) struct RowsKey {
    query: String,
    show: Show,
    added_only: bool,
    show_copies: bool,
    /// The "also in" map changed (copies to fold).
    also: usize,
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
pub(crate) fn find_backup(root: &Path, original: &Path, since: u64) -> Option<PathBuf> {
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
pub(crate) struct Removed {
    pub(crate) original: PathBuf,
    pub(crate) backup: PathBuf,
    pub(crate) addon: Option<Addon>,
}

/// A dialog's text: a line, then each warning with a triangle.
/// Opens a project in the Library's panel, from inside a dialog.
/// Opens a mod in the panel beside the page; knows each project's icon.
#[derive(Clone)]
struct Peek {
    open: Rc<dyn Fn(u64, &mut Window, &mut App)>,
    icons: Rc<HashMap<u64, PathBuf>>,
}

/// A mod's name that opens it in the panel beside the page (when it's a
/// project), leaving the dialog open.
fn peek_link(id: impl Into<ElementId>, name: String, pid: u64, peek: &Peek, cx: &App) -> AnyElement {
    let open = peek.open.clone();
    widgets::mod_link(id, name, peek.icons.get(&pid).map(PathBuf::as_path), pid, move |p, w, cx| open(p, w, cx), cx)
        .flex_none()
        .into_any_element()
}

/// Mods' names in a wrapping row, each opening its project: the first
/// `cap`, then "and n more" with the rest in a tooltip.
fn peek_links(key: &str, lead: String, mods: &[(String, u64)], cap: usize, peek: &Peek, cx: &App) -> Div {
    let n = mods.len();
    let rest: SharedString = mods.iter().skip(cap).map(|(m, _)| m.as_str()).collect::<Vec<_>>().join(", ").into();
    h_flex()
        .flex_wrap()
        .gap_x_1()
        .min_w_0()
        .children(lead.split(' ').map(|w| div().flex_none().child(w.to_string())))
        .children(mods.iter().take(cap).enumerate().map(|(i, (name, pid))| {
            let comma = i + 1 < n.min(cap) || n > cap;
            h_flex()
                .flex_none()
                .child(peek_link(ElementId::Name(format!("{key}:{i}").into()), name.clone(), *pid, peek, cx))
                .when(comma, |r| r.child(","))
        }))
        .when(n > cap, |r| {
            r.child(
                div()
                    .id(ElementId::Name(format!("{key}:more").into()))
                    .flex_none()
                    .child(format!("and {} more", n - cap))
                    .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(rest.clone()).build(w, cx)),
            )
        })
}

/// The intro, then "X is required by A, B" for each mod being removed that
/// others need, wrapping so a long list stays inside the dialog.
fn warning_list(intro: &'static str, warnings: &[Dependent], peek: &Peek, cx: &App) -> Div {
    let color = cx.theme().warning;
    v_flex().gap_2().child(intro).children(warnings.iter().enumerate().map(|(i, d)| {
        h_flex()
            .items_start()
            .gap_2()
            .text_color(color)
            .child(div().flex_none().pt_0p5().child(Icon::new(IconName::TriangleAlert).small()))
            .child(
                peek_links(&format!("req{i}"), format!("{} is required by", d.name), &d.by, 12, peek, cx)
                    .flex_1(),
            )
    }))
}

/// A mod being removed and the enabled mods that need it.
struct Dependent {
    name: String,
    by: Vec<(String, u64)>,
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

    /// (the mod, its jar, the id it needs) for what removing `gone` breaks
    /// (mods being removed too don't count).
    fn after(&self, gone: &[String]) -> Vec<(String, String, String)> {
        let mut out: Vec<(String, String, String)> = self
            .lost
            .iter()
            .filter(|(_, jar, _)| !gone.contains(jar))
            .filter(|(_, _, id)| {
                gone.iter().any(|g| self.provides.get(g).is_some_and(|p| p.contains(id)))
            })
            .cloned()
            .collect();
        out.dedup();
        out
    }
}

/// Moves removed files back and puts their entries back in the instance.
pub(crate) fn undo_remove(store: &Entity<Store>, root: PathBuf, items: Vec<Removed>, window: &mut Window, cx: &mut App) {
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
    /// Collapse the Library's instance list to its rail, for room.
    CollapseList,
}

impl EventEmitter<AddonsEvent> for AddonsView {}

pub struct AddonsView {
    /// The table, for its keys.
    focus: FocusHandle,
    /// The row the keys act on.
    cursor: Option<usize>,
    /// Where a Shift+click range starts: the row last clicked without Shift.
    anchor: Option<usize>,
    pub(crate) store: Entity<Store>,
    pub(crate) root: Option<PathBuf>,
    pub(crate) class: u32,
    pub(crate) globals: crate::addons_extra::GlobalPacks,
    /// Each mod's state on the paired server, by lower-case file name, for
    /// the server root and [`marks_key`] it was compared at.
    pub(crate) marks: Option<(PathBuf, u64, HashMap<String, hopper::model::DiffEntry>)>,
    pub(crate) missing: crate::addons_extra::MissingDeps,
    /// The find-the-broken-mod hunt, when one runs.
    pub(crate) hunt: crate::addons_extra::HuntUi,
    search: Entity<InputState>,
    query: String,
    show: Show,
    added_only: bool,
    updates_first: bool,
    sort: (SortCol, bool),
    pub(crate) selected: HashSet<String>,
    /// Resource packs Paxi (or another global folder) also has: shown,
    /// rather than folded into one row.
    pub(crate) show_copies: bool,
    /// A drag across the tick boxes, while the button is down.
    pick: Option<DragPick>,
    /// The row a tick box was last pressed on, for Shift.
    pick_anchor: Option<usize>,
    pick_row_h: Pixels,
    _pick_tick: Option<Task<()>>,
    /// Jars in the mods folder Hopper doesn't track, by folder mtime.
    /// Keyed by folder, its mtime and the tracked file names (identifying
    /// jars tracks them without touching the folder).
    loose: (Option<(PathBuf, std::time::SystemTime, u64)>, Vec<String>),
    /// What the table shows, built when it changes rather than per frame.
    pub(crate) table: Table,
    pub(crate) scroll: UniformListScrollHandle,
    /// A mod's project page, docked on the right.
    peek: Entity<crate::browse::BrowseView>,
    /// A data pack folder shown instead of the instance's content
    /// (instance-relative), and its packs once read.
    pub(crate) folder: Option<String>,
    pub(crate) folder_packs: Option<Vec<hopper::instance_ops::PackFile>>,
    /// Where this class's packs can be (resource and data packs).
    pub(crate) locs: Vec<hopper::packs::Location>,
    /// The shown data pack folder's and saves/' times as last read, to
    /// notice files and worlds coming and going.
    pub(crate) folder_stamp: (Option<std::time::SystemTime>, Option<std::time::SystemTime>),
    /// Resource packs as the game orders them, top (wins) first.
    pub(crate) rp_order: Rc<Vec<String>>,
    /// Packs in the instance's own folder it doesn't track (added by hand,
    /// copied in): the folder, its time when read, and the packs.
    pub(crate) loose_packs: Option<(PathBuf, Option<std::time::SystemTime>, Vec<hopper::instance_ops::PackFile>)>,
    /// Lower-case names of those listed now.
    pub(crate) loose_set: HashSet<String>,
    /// The optional columns that fit; set each frame from `room`.
    pub(crate) cols: Cols,
    /// The work area's width in rem, from the Library (None: the window's).
    pub(crate) room: Option<f32>,
    /// The Library's instance list is open (not the rail).
    pub(crate) list_open: bool,
    /// "Collapse the list for more room" was dismissed this session.
    hint_gone: bool,
    /// The list is narrow (the project panel is open, or it's a global or
    /// Paxi folder beside its folder list): the action column shrinks to
    /// icon buttons and leaves the name its room.
    pub(crate) compact: bool,
    /// Narrower still (a global or Paxi folder with the project panel
    /// open): the Added column goes, and the changelog column unless a row
    /// has an update (the open panel lists the files and their changelogs).
    pub(crate) tight: bool,
    /// Mods just updated, still shown (as Updated) until the user stops
    /// clicking Update, so rows don't move under the pointer.
    recent: HashMap<u64, Update>,
    /// Mods the instance's last game crash names: lower-case file name → why.
    crashed: HashMap<String, String>,
    /// Clicks and pointer, for when Updated rows may go.
    pub(crate) settle: widgets::Settle,
    /// Missing mods installing from the banner: mod id → its job.
    pub(crate) dep_jobs: HashMap<String, u64>,
    /// Installed from the banner, shown as Installed until they go.
    pub(crate) dep_done: HashSet<String>,
    /// Installed chips fading out now.
    pub(crate) dep_leaving: Option<(std::time::Instant, HashSet<String>)>,
    /// A missing list read while chips were installing or done: shown once
    /// they've gone, so the banner doesn't reflow under the pointer.
    pub(crate) missing_next: Option<Vec<hopper::deps::Missing>>,
    /// Rows fading out now: (since, projects).
    leaving: Option<(std::time::Instant, HashSet<u64>)>,
    /// Rows sliding up into the room the leaving ones left: (generation,
    /// since, rows of room by project).
    slide: Option<(u64, std::time::Instant, HashMap<u64, usize>)>,
    slide_gen: u64,
    /// Redraws while updates run (button progress) and lets rows go.
    _ticker: Option<Task<()>>,
    /// The row whose ⋯ menu is open (its button stays shown).
    menu_for: Option<String>,
    /// Waits for a reviewed Update all to end, then sums it up.
    _bulk_watch: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// Updated rows fade out over this long, then the rows below slide up.
const LEAVE: std::time::Duration = widgets::FADE;
const SLIDE: std::time::Duration = std::time::Duration::from_millis(220);

impl AddonsView {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let peek = cx.new(|cx| crate::browse::BrowseView::peek(store.clone(), window, cx));
        Self::with_peek(store, peek, window, cx)
    }

    /// A table that opens projects in `peek` (another table's panel).
    pub fn with_peek(
        store: Entity<Store>,
        peek: Entity<crate::browse::BrowseView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let subs = vec![
            cx.observe(&peek, |_, _, cx| cx.notify()),
            cx.observe(&store, |this, _, cx| {
                this.want_updates(cx);
                this.pull_recent(cx);
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
            anchor: None,
            store,
            root: None,
            class: cf::CLASS_MODS,
            globals: Default::default(),
            folder: None,
            folder_packs: None,
            locs: Vec::new(),
            folder_stamp: (None, None),
            rp_order: Rc::default(),
            loose_packs: None,
            loose_set: HashSet::new(),
            marks: None,
            missing: Default::default(),
            hunt: Default::default(),
            search,
            query: String::new(),
            show: Show::All,
            added_only: false,
            show_copies: false,
            updates_first: true,
            sort: (SortCol::Added, false),
            selected: HashSet::new(),
            pick: None,
            pick_anchor: None,
            pick_row_h: px(0.),
            _pick_tick: None,
            loose: (None, Vec::new()),
            table: Table::default(),
            scroll: UniformListScrollHandle::new(),
            peek,
            cols: Cols::default(),
            room: None,
            list_open: false,
            hint_gone: false,
            compact: false,
            tight: false,
            recent: HashMap::new(),
            settle: Default::default(),
            crashed: HashMap::new(),
            dep_jobs: HashMap::new(),
            dep_done: HashSet::new(),
            dep_leaving: None,
            missing_next: None,
            leaving: None,
            slide: None,
            slide_gen: 0,
            _ticker: None,
            menu_for: None,
            _bulk_watch: None,
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

    /// Shows only the mods with an update (the header pill links here).
    pub fn show_updates(&mut self, cx: &mut Context<Self>) {
        self.show = Show::Updates;
        cx.notify();
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
        self.read_hunt(cx);
        // The last crash's mods, for their tag (read once, then cached).
        if let Some(root) = self.root.clone() {
            let c = self.store.update(cx, |s, _| s.crash(&root).cloned());
            // A mod turned off since the crash isn't marked any more.
            let off: HashSet<String> = self
                .store
                .read(cx)
                .instance(&root)
                .map(|i| i.addons.iter().filter(|a| !a.enabled).map(|a| a.file_name.trim_end_matches(".disabled").to_lowercase()).collect())
                .unwrap_or_default();
            let now: HashMap<String, String> = c
                .map(|c| {
                    c.suspects
                        .iter()
                        .filter(|s| !off.contains(&s.file_name.trim_end_matches(".disabled").to_lowercase()))
                        .map(|s| {
                            let file = s.file_name.trim_end_matches(".disabled").to_lowercase();
                            (file, format!("{} in the last crash. {} (Logs & crashes has more.)", s.certainty.label(), s.why))
                        })
                        .collect()
                })
                .unwrap_or_default();
            if now != self.crashed {
                self.crashed = now;
                cx.notify();
            }
        }
        let Some(root) = self.root.clone() else {
            self.table = Table::default();
            return;
        };
        let sig = fingerprint(self.store.read(cx), &root, self.class);
        // A data pack folder or a world came or went.
        let force = self.folder_moved(&root) || force;
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
        self.load_globals(&inst, cx);
        self.load_locations(&inst, cx);
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
            also: Rc::default(),
            rank: Rc::default(),
            rows: Rc::default(),
            rows_for: None,
            has_side: false,
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
        self.want_loose(cx);
        self.apply_global(cx);
        self.fill_also();
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
            show_copies: self.show_copies,
            also: self.table.also.len(),
            updates_first: self.updates_first,
            sort: self.sort,
        }
    }

    /// Re-filters and re-sorts the rows when a filter changed (in memory).
    pub(crate) fn ensure_rows(&mut self) {
        let key = self.rows_key();
        if self.table.rows_for.as_ref() == Some(&key) {
            return;
        }
        let rows = match &self.table.inst {
            Some(inst) => self.rows(inst, &self.table.updates),
            None => Vec::new(),
        };
        self.table.rank = Rc::new(self.order_ranks());
        self.table.has_side = self.class == cf::CLASS_MODS
            || self.table.inst.as_ref().is_some_and(|inst| {
                rows.iter().any(|r| match r {
                    Row::Addon(i) => inst.addons.get(*i).is_some_and(|a| crate::side_card::side_of(&a.env).is_some()),
                    _ => false,
                })
            });
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

    pub(crate) fn want_images(&mut self, cx: &mut Context<Self>) {
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

    /// Pending updates of this tab's class, by project, with the ones just
    /// done (shown as Updated for a moment).
    pub(crate) fn updates(&self, inst: &Instance, cx: &App) -> HashMap<u64, Update> {
        let mut out = self.pending_updates(inst, cx);
        for (pid, u) in &self.recent {
            if inst.addons.iter().any(|a| a.project_id == *pid && in_class(a, self.class)) {
                out.insert(*pid, u.clone());
            }
        }
        out
    }

    fn pending_updates(&self, inst: &Instance, cx: &App) -> HashMap<u64, Update> {
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

    // ---- updated rows ------------------------------------------------------

    /// Takes in updates a job just finished, to show them as Updated.
    fn pull_recent(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let store = self.store.read(cx);
        let mut added = false;
        for u in store.recent_updates(&root) {
            if !self.recent.contains_key(&u.project_id) {
                self.recent.insert(u.project_id, u);
                added = true;
            }
        }
        let running = store.queues_updates(&root);
        if added {
            self.sync(true, cx);
        }
        if running || !self.recent.is_empty() {
            self.start_ticker(cx);
        }
    }

    /// Ticks while updates run or Updated rows wait: repaints the buttons'
    /// progress and lets rows go once the user is done clicking.
    pub(crate) fn start_ticker(&mut self, cx: &mut Context<Self>) {
        if self._ticker.is_some() {
            return;
        }
        self._ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(100))
                    .await;
                let go_on = this.update(cx, |this, cx| this.tick(cx)).unwrap_or(false);
                if !go_on {
                    break;
                }
            }
        }));
    }

    fn tick(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(root) = self.root.clone() else {
            self._ticker = None;
            return false;
        };
        let now = std::time::Instant::now();
        // Faded out: drop them, and slide the rows below up.
        if let Some((since, ids)) = self.leaving.clone()
            && now.duration_since(since) >= LEAVE
        {
            let order: Vec<u64> = self.row_projects();
            let mut gone = 0;
            let mut room = HashMap::new();
            for pid in order {
                if ids.contains(&pid) {
                    gone += 1;
                } else if gone > 0 {
                    room.insert(pid, gone);
                }
            }
            self.leaving = None;
            self.drop_recent(&root, &ids, cx);
            self.slide_gen += 1;
            self.slide = (!room.is_empty()).then(|| (self.slide_gen, now, room));
        }
        if self.slide.as_ref().is_some_and(|(_, t, _)| now.duration_since(*t) >= SLIDE) {
            self.slide = None;
        }
        // Done clicking: the pointer left the rows, or rested long enough.
        if self.leaving.is_none() && !self.recent.is_empty() && self.settle.settled() {
            let ids: HashSet<u64> = self.recent.keys().copied().collect();
            if self.show == Show::Updates {
                self.leaving = Some((now, ids));
            } else {
                // Elsewhere the row stays; only its Updated mark goes.
                self.drop_recent(&root, &ids, cx);
            }
        }
        let store = self.store.read(cx);
        let ended: Vec<(String, bool)> = self
            .dep_jobs
            .iter()
            .filter_map(|(id, job)| match store.job_state(*job) {
                Some(j) if !j.done => None,
                Some(j) => Some((id.clone(), j.error.is_none())),
                None => Some((id.clone(), false)),
            })
            .collect();
        for (id, ok) in ended {
            self.dep_jobs.remove(&id);
            if ok {
                self.dep_done.insert(id);
            }
        }
        self.tick_deps(now);
        cx.notify();
        let running = self.store.read(cx).queues_updates(&root);
        let more = running
            || !self.recent.is_empty()
            || self.leaving.is_some()
            || self.slide.is_some()
            || self.deps_holding()
            || self.missing_next.is_some();
        if !more {
            self._ticker = None;
        }
        more
    }

    /// Banner chips: installs that ended become Installed (or go back to
    /// Install when they failed); Installed chips fade once the user is
    /// done clicking; then the missing list read meanwhile shows.
    fn tick_deps(&mut self, now: std::time::Instant) {
        if let Some((since, ids)) = self.dep_leaving.clone()
            && now.duration_since(since) >= widgets::FADE
        {
            self.dep_leaving = None;
            self.dep_done.clear();
            match self.missing_next.take() {
                Some(next) => self.missing.list = next,
                None => self.missing.list.retain(|m| !ids.contains(&m.needs)),
            }
        }
        if self.dep_leaving.is_none() && !self.dep_done.is_empty() && self.dep_jobs.is_empty() && self.settle.settled() {
            self.dep_leaving = Some((now, self.dep_done.clone()));
        }
        if !self.deps_holding()
            && let Some(next) = self.missing_next.take()
        {
            self.missing.list = next;
        }
    }

    pub(crate) fn deps_holding(&self) -> bool {
        !self.dep_jobs.is_empty() || !self.dep_done.is_empty() || self.dep_leaving.is_some()
    }

    fn drop_recent(&mut self, root: &Path, ids: &HashSet<u64>, cx: &mut Context<Self>) {
        self.recent.retain(|k, _| !ids.contains(k));
        self.store.read(cx).forget_updates(root, ids);
        self.sync(true, cx);
    }

    /// The table's projects, top to bottom.
    fn row_projects(&self) -> Vec<u64> {
        let Some(inst) = &self.table.inst else { return Vec::new() };
        self.table
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Addon(i) => inst.addons.get(*i).map(|a| a.project_id),
                _ => None,
            })
            .collect()
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
        let tracked = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            for a in &inst.addons {
                a.file_name.hash(&mut h);
            }
            h.finish()
        };
        let key = mtime.map(|t| (inst.root.clone(), t, tracked));
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
                Show::Disabled => !self.row_on(a),
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
        // The instance's resource packs that Paxi (or another global
        // folder) also has: packs ship them so CurseForge updates them,
        // and Paxi loads its own copy. Folded away unless asked for or in
        // use here.
        let mut copies = 0usize;
        let mut where_: Vec<String> = Vec::new();
        if self.order_mode() && q.is_empty() && self.show == Show::All {
            rows.retain(|(i, _)| {
                let a = &inst.addons[*i];
                let key = a.file_name.trim_end_matches(".disabled").to_lowercase();
                match self.table.also.get(&key) {
                    Some(at) if !self.row_on(a) => {
                        copies += 1;
                        for w in at.split(", ") {
                            if !where_.iter().any(|x| x == w) {
                                where_.push(w.to_string());
                            }
                        }
                        self.show_copies
                    }
                    _ => true,
                }
            });
        }
        let mut out: Vec<Row> = if self.order_mode() {
            self.order_rows(inst, rows.into_iter().map(|(i, _)| i).collect())
        } else {
            rows.into_iter().map(|(i, _)| Row::Addon(i)).collect()
        };
        if copies > 0 {
            out.push(Row::Copies(copies, self.show_copies, where_.join(", ")));
        }
        // Shaders: one choice, "Off" first.
        if self.class == cf::CLASS_SHADERS && self.show == Show::All && q.is_empty() && !out.is_empty() {
            out.insert(0, Row::ShaderOff);
        }
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

    /// Each of `files` that enabled mods outside `files` need, with them.
    fn dependents(&self, files: &[String], cx: &App) -> Vec<Dependent> {
        let Some(root) = &self.root else { return Vec::new() };
        let Some(inst) = self.store.read(cx).instance(root) else {
            return Vec::new();
        };
        files
            .iter()
            .filter_map(|f| inst.addon_by_file(f))
            .filter_map(|a| {
                let by: Vec<(String, u64)> = inst
                    .addons
                    .iter()
                    .filter(|b| b.enabled && b.requires.contains(&a.project_id) && !files.contains(&b.file_name))
                    .map(|b| (b.name.clone(), b.project_id))
                    .collect();
                (!by.is_empty()).then(|| Dependent { name: a.name.clone(), by })
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
    pub(crate) fn toggle(
        &mut self,
        files: Vec<String>,
        on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Resource packs of the instance: on means used in game.
        if self.order_mode() {
            return self.set_in_use(files, on, window, cx);
        }
        if let Some(dir) = self.global_dir() {
            return self.toggle_global(dir, files, on, window, cx);
        }
        // A hunt owns the on/off state until it ends.
        if self.hunt_running() {
            window.push_notification(
                Notification::info("Finding the broken mod: mark mods known good on their chip, or stop the hunt first."),
                cx,
            );
            cx.notify();
            return;
        }
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
                .margin_top(rems(crate::widgets::DIALOG_TOP).to_pixels(window.rem_size()))
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

    pub(crate) fn set_enabled(
        &mut self,
        files: Vec<String>,
        on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Packs the instance doesn't track are renamed where they are.
        let (loose, files): (Vec<String>, Vec<String>) = files.into_iter().partition(|f| self.is_loose(f));
        if !loose.is_empty() {
            self.set_loose_enabled(loose, on, window, cx);
        }
        if files.is_empty() {
            return;
        }
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
        if let Some(dir) = self.global_dir() {
            return self.remove_global(dir, files, window, cx);
        }
        // Packs the instance doesn't track go to backups as they are.
        let (loose, files): (Vec<String>, Vec<String>) = files.into_iter().partition(|f| self.is_loose(f));
        if !loose.is_empty() {
            self.remove_loose(loose, window, cx);
        }
        if files.is_empty() {
            return;
        }
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
        // Each jar's project, so a mod named in the dialog opens in the panel.
        let pids: Rc<HashMap<String, u64>> = Rc::new(
            self.root
                .as_ref()
                .and_then(|r| self.store.read(cx).instance(r))
                .map(|i| i.addons.iter().map(|a| (a.file_name.clone(), a.project_id)).collect())
                .unwrap_or_default(),
        );
        // Focus inside the dialog's body, so Shift going down or up reaches
        // its listener (they're called along the focused element's path).
        let body_focus = cx.focus_handle();
        let peek: Peek = {
            let view = view.clone();
            let store = self.store.read(cx);
            let icons: HashMap<u64, PathBuf> = self
                .root
                .as_ref()
                .and_then(|r| store.instance(r))
                .map(|i| {
                    i.addons
                        .iter()
                        .filter(|a| a.project_id != 0)
                        .filter_map(|a| Some((a.project_id, store.image(&a.icon)?.to_path_buf())))
                        .collect()
                })
                .unwrap_or_default();
            Peek {
                open: Rc::new(move |pid, window, cx| {
                    view.update(cx, |v, cx| v.open_peek(pid, cx));
                    window.refresh();
                }),
                icons: Rc::new(icons),
            }
        };
        let focus_later = body_focus.clone();
        window.open_dialog(cx, move |dialog, window, cx| {
            let with = {
                let (files, picked) = (files.clone(), picked.clone());
                move || -> Vec<String> {
                    files.iter().cloned().chain(picked.borrow().iter().cloned()).collect()
                }
            };
            let (files, view, w2) = (with(), view.clone(), what.clone());
            let (f2, v2, w3) = (with(), view.clone(), what.clone());
            // Shift turns Delete into deleting for good.
            let shift = window.modifiers().shift;
            // With a mod open in the panel, the page isn't dimmed so it reads.
            let peeking = view.read(cx).peek().read(cx).is_open();
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
                        let pid = pids.get(&file).copied().unwrap_or(0);
                        h_flex()
                            .gap_1()
                            .child(widgets::thumb(peek.icons.get(&pid).map(PathBuf::as_path), rems(1.25), cx))
                            .child(
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
                                    }),
                            )
                            .when(pid != 0, |r| {
                                let peek = peek.clone();
                                r.child(
                                    Button::new(("orphan-peek", i))
                                        .icon(IconName::PanelRight)
                                        .xsmall()
                                        .ghost()
                                        .tooltip("Show in the panel")
                                        .on_click(move |_, window, cx| (peek.open)(pid, window, cx)),
                                )
                            })
                    }))
            });
            dialog
                .title(format!("Remove {what}?"))
                .w(rems(30.).to_pixels(window.rem_size()))
                .margin_top(rems(crate::widgets::DIALOG_TOP).to_pixels(window.rem_size()))
                .overlay(!peeking)
                .child(
                    // Redraws as Shift goes down or up, for the Delete button.
                    div().track_focus(&body_focus).on_modifiers_changed(|_, window, _| window.refresh()).child(warning_list(
                        "Move to backups keeps a copy in .hopper-backups in the instance folder, with Undo. Delete sends the files to the Recycle Bin; hold Shift to delete them for good.",
                        &warnings,
                        &peek,
                        cx,
                    )),
                )
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
                                .children(hits.iter().take(12).enumerate().map(|(i, (name, jar, id))| {
                                    let pid = pids.get(jar).copied().unwrap_or(0);
                                    h_flex()
                                        .flex_wrap()
                                        .gap_1()
                                        .text_sm()
                                        .child(peek_link(("hit", i), name.clone(), pid, &peek, cx))
                                        .child(format!("needs {id}"))
                                }))
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
                                .when(shift, |b| b.danger())
                                .when(!shift, |b| b.outline().danger())
                                .icon(IconName::Trash)
                                .label(if shift { "Delete permanently" } else { "Delete" })
                                .on_click(move |_, window, cx| {
                                    let permanent = window.modifiers().shift;
                                    v2.update(cx, |v, cx| v.delete(f2.clone(), w3.clone(), permanent, window, cx));
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
        window.defer(cx, move |window, cx| window.focus(&focus_later, cx));
    }

    /// Deletes files (to the Recycle Bin, or for good with `permanent`),
    /// no backup kept.
    fn delete(&mut self, files: Vec<String>, what: String, permanent: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let r = self.store.update(cx, |s, cx| {
            let r = s.delete_files(&root, &files, permanent);
            cx.notify();
            r
        });
        let n = match r {
            Ok(()) if permanent && files.len() == 1 => Notification::success(format!("Deleted {what} for good")),
            Ok(()) if permanent => Notification::success(format!("Deleted {} files for good", files.len())),
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
            })
            // The undo stays in Backups; the toast shouldn't cover the header.
            .autohide(true);
            window.push_notification(note, cx);
        }
        self.selected.clear();
        // A loose jar leaves the instance as it was: read the folder again.
        self.sync(true, cx);
        cx.notify();
    }

    fn run_updates(&mut self, updates: Vec<Update>, cx: &mut Context<Self>) {
        if let Some(dir) = self.global_dir() {
            return self.update_global(dir, updates, cx);
        }
        // Untracked packs update in place, like a global folder's.
        let (loose, updates): (Vec<Update>, Vec<Update>) =
            updates.into_iter().partition(|u| self.loose_pid(u.project_id));
        if let (false, Some(dir)) = (loose.is_empty(), self.own_dir()) {
            self.update_global(dir, loose, cx);
        }
        if updates.is_empty() {
            return;
        }
        let Some(root) = &self.root else { return };
        let store = self.store.read(cx);
        // A running update job queues these; any other job holds the
        // instance.
        if store.queues_updates(root) || store.edit_hold_reason(root).is_none() {
            store.run_updates(root, updates);
        }
        cx.notify();
    }

    /// Update all, reviewed first: the updates grouped by how big a jump
    /// they are, each one skippable.
    fn review_updates(&mut self, all: Vec<Update>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(inst) = self.table.inst.clone() else { return };
        let store = self.store.read(cx);
        let mut skip = HashSet::new();
        let mut held = HashSet::new();
        let mut rows: Vec<ReviewRow> = all
            .into_iter()
            .map(|u| {
                let a = inst.addons.iter().find(|a| a.project_id == u.project_id);
                let risk = Risk::of(&u);
                // A test version in place of a release is left out at first.
                if risk == Risk::Pre && a.is_none_or(|a| a.release_type <= 1) {
                    skip.insert(u.project_id);
                }
                let (issues, missing) = load_problems(&inst, &u.file);
                // So is one whose new file would stop the game loading.
                if (!issues.is_empty() || !missing.is_empty()) && skip.insert(u.project_id) {
                    held.insert(u.project_id);
                }
                let (from, to) = u.versions();
                ReviewRow {
                    icon: a.and_then(|a| store.image(&a.icon)).map(Path::to_path_buf),
                    name: a.map_or(u.name.clone(), |a| a.name.clone()),
                    from,
                    to,
                    risk,
                    issues,
                    missing: missing.into_iter().map(|id| (id, None)).collect(),
                    update: u,
                }
            })
            .collect();
        rows.sort_by_key(|r| r.name.to_lowercase());
        let total = rows.len();
        // Required mods the instance lacks are known only by id: their
        // names come from the sites, and one installed from the other site
        // (another id) turns out fine.
        let lookup: Vec<u64> = {
            let mut ids: Vec<u64> = rows.iter().flat_map(|r| r.missing.iter().map(|(id, _)| *id)).collect();
            ids.sort_unstable();
            ids.dedup();
            ids
        };
        let review = cx.new(|_| UpdateReview { rows, skip, held });
        if !lookup.is_empty() {
            let (inst, review) = ((*inst).clone(), review.clone());
            window
                .spawn(cx, async move |cx| {
                    let found = cx
                        .background_spawn(async move {
                            lookup
                                .into_iter()
                                .map(|id| match hopper::source::get_mod(id) {
                                    Ok(m) if hopper::browse::installed_addon(&inst, &m).is_some_and(|a| a.enabled) => {
                                        (id, None)
                                    }
                                    Ok(m) => (id, Some(m.name)),
                                    // Unknown: the warning stays, unnamed.
                                    Err(_) => (id, Some(String::new())),
                                })
                                .collect::<Vec<_>>()
                        })
                        .await;
                    cx.update(|window, cx| {
                        review.update(cx, |r, cx| {
                            r.resolve(&found);
                            cx.notify();
                        });
                        // The footer's count is the dialog's.
                        window.refresh();
                    })
                    .ok();
                })
                .detach();
        }
        let view = cx.entity();
        window.open_dialog(cx, move |dialog, window, cx| {
            let n = review.read(cx).chosen().len();
            let (view, review) = (view.clone(), review.clone());
            let rem = window.rem_size();
            dialog
                .title(if total == 1 { "Review 1 update".to_string() } else { format!("Review {total} updates") })
                .w(rems(40.).to_pixels(rem))
                // Under the title bar like every dialog (see versions.rs).
                .margin_top(rems(crate::widgets::DIALOG_TOP).to_pixels(rem))
                .child(review.clone())
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().child(Button::new("review-cancel").ghost().label("Cancel")))
                        .child(DialogAction::new().child(
                            Button::new("review-go")
                                .primary()
                                .icon(IconName::Download)
                                .label(format!("Update {n}"))
                                .disabled(n == 0),
                        )),
                )
                .on_ok(move |_, window, cx| {
                    let chosen = review.read(cx).chosen();
                    if chosen.is_empty() {
                        return false;
                    }
                    view.update(cx, |v, cx| v.start_bulk(chosen, window, cx));
                    true
                })
        });
    }

    /// Runs reviewed updates; once they end, says how it went and offers
    /// Undo (the old files are in `.hopper-backups`).
    fn start_bulk(&mut self, updates: Vec<Update>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let global = self.global_dir().is_some();
        // Each mod as it is now, and where its file is.
        let olds: Vec<(Addon, PathBuf)> = if global {
            Vec::new()
        } else {
            self.store
                .read(cx)
                .instance(&root)
                .map(|inst| {
                    updates
                        .iter()
                        .filter_map(|u| inst.addons.iter().find(|a| a.project_id == u.project_id && !a.locked))
                        .map(|a| (a.clone(), inst.addon_path(a)))
                        .collect()
                })
                .unwrap_or_default()
        };
        // Backup folders are named by the second; allow for a tick.
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
            .saturating_sub(1);
        self.run_updates(updates, cx);
        self.pull_recent(cx);
        if olds.is_empty() || !self.store.read(cx).queues_updates(&root) {
            return;
        }
        let store = self.store.clone();
        self._bulk_watch = Some(cx.spawn_in(window, async move |this, cx| {
            // Until the update job (and anything else on the instance) ends.
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(300)).await;
                let Ok(done) = this.update(cx, |this, cx| {
                    let s = this.store.read(cx);
                    !s.queues_updates(&root) && !s.is_busy(&root)
                }) else {
                    return;
                };
                if done {
                    break;
                }
            }
            // The saved instance says which files changed.
            let r2 = root.clone();
            let sum = cx
                .background_spawn(async move { BulkSum::read(&r2, olds, since) })
                .await;
            let Some(sum) = sum else { return };
            cx.update(|window, cx| sum.notify(store, root, window, cx)).ok();
        }));
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
        hint: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let pending: HashMap<u64, Update> = updates
            .iter()
            .filter(|(k, _)| !self.recent.contains_key(k))
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        let updates = &pending;
        let of_class = || inst.addons.iter().filter(|a| in_class(a, self.class));
        let n_up = of_class()
            .filter(|a| updates.contains_key(&a.project_id))
            .count();
        let n_off = of_class().filter(|a| !self.row_on(a)).count();
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
                // The toolbar's one filled button: opens the review first.
                Button::new("update-all")
                    .primary()
                    .small()
                    .icon(IconName::Download)
                    .label(if all.len() == 1 {
                        "Review 1 update".to_string()
                    } else {
                        format!("Review {} updates", all.len())
                    })
                    .loading(all.is_empty() || busy)
                    .disabled(hold.is_some())
                    .when_some(hold, |b, why| b.tooltip(why))
                    .on_click(cx.listener(move |this, _, window, cx| this.review_updates(all.clone(), window, cx)))
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
                    .outline()
                    .icon(IconName::Plus)
                    .label("Add content")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let Some(root) = this.root.clone() else { return };
                        // With Paxi's folder shown, what's added goes there.
                        let rel = this
                            .folder
                            .clone()
                            .filter(|_| this.class == cf::CLASS_DATAPACKS)
                            .or_else(|| this.globals.picked.filter(|_| this.class == cf::CLASS_RESOURCE_PACKS).map(str::to_string));
                        match rel {
                            Some(rel) => cx.emit(AddonsEvent::AddTo {
                                root,
                                class: this.class,
                                dest: rel,
                            }),
                            None => cx.emit(AddonsEvent::AddContent {
                                root,
                                class: this.class,
                            }),
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
                    .child(Tab::new().label(format!("Off {n_off}")))
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.show = [Show::All, Show::Updates, Show::Disabled][*ix];
                        cx.notify();
                    })),
            )
            .when(inst.pack.is_some() && self.global_dir().is_none(), |r| {
                r.child(
                    Checkbox::new("added-only")
                        .label(format!("Only yours ({n_added})"))
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
            .when(hint, |r| r.child(self.collapse_hint(cx)))
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

    /// "Collapse the list for more room": collapses the instance list, or
    /// goes for the session with its ×.
    fn collapse_hint(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        h_flex()
            .flex_none()
            .gap_0p5()
            .pl_2()
            .rounded(theme.radius)
            .bg(theme.primary.opacity(widgets::alpha::TINT))
            .child(
                Button::new("collapse-list")
                    .ghost()
                    .xsmall()
                    .icon(IconName::PanelLeftClose)
                    .label("Collapse the list for more room")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.hint_gone = true;
                        cx.emit(AddonsEvent::CollapseList);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("collapse-hint-x")
                    .ghost()
                    .xsmall()
                    .icon(IconName::X)
                    .tooltip("Don't show this again")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.hint_gone = true;
                        cx.notify();
                    })),
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
                            cx.listener(move |this, _, window, cx| this.review_updates(ups.clone(), window, cx)),
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
                    .clickable(cx)
                    .gap_1()
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
            .when(self.order_mode(), |r| r.child(crate::datapacks::col_order()))
            .child(div().w_10().flex_none())
            .child(head(SortCol::Name, "Name", self.name_cell()))
            .when(self.action_fits(), |r| r.child(col_action(self.compact)))
            .children(self.spacer())
            .when(self.cols.author, |r| {
                r.child(head(SortCol::Author, "Author", col_author()))
            })
            // Data packs run on the server by nature, and packs with no
            // side say nothing: no Side column.
            .when(self.cols.side, |r| r.child(head(SortCol::Env, "Side", col_env()).id("head-side").tooltip(|w, cx| {
                gpui_kit::component::tooltip::Tooltip::element(|_, cx| {
                    let muted = cx.theme().muted_foreground;
                    let line = |icon: IconName, text: &'static str| {
                        h_flex().gap_2().child(Icon::new(icon).small().text_color(muted)).child(text)
                    };
                    v_flex()
                        .gap_1()
                        .text_sm()
                        .child("Where the mod runs:")
                        .child(line(IconName::Monitor, "Client only: your game"))
                        .child(line(IconName::Server, "Server only"))
                        .child(line(IconName::ArrowLeftRight, "Client and server: both need it"))
                        .child(div().text_xs().text_color(muted).child("Hover a row's icon for more. Click to sort."))
                })
                .build(w, cx)
            })))
            .child(head(SortCol::Active, if self.class == cf::CLASS_SHADERS { "Use" } else { "On" }, col_active()).id("head-on").tooltip(|w, cx| {
                gpui_kit::component::tooltip::Tooltip::new(
                    "Off keeps the file but the game doesn't load it (renamed to .disabled). Click to sort.",
                )
                .build(w, cx)
            }))
            .when(self.cols.date, |r| r.child(head(SortCol::Added, "Added", col_date())))
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
        let start = range.start.min(rows.len());
        rows[start..range.end.min(rows.len())]
            .iter()
            .enumerate()
            .map(|(k, row)| (start + k, row))
            .map(|(ix, row)| {
                let el = self.render_row(row, &inst, &updates, &active_rp, &active_shader, now, cx);
                let pid = match row {
                    Row::Addon(i) => inst.addons.get(*i).map(|a| a.project_id),
                    _ => None,
                };
                let leaving = pid.is_some_and(|p| self.leaving.as_ref().is_some_and(|(_, ids)| ids.contains(&p)));
                let slide = pid.and_then(|p| {
                    let (g, _, room) = self.slide.as_ref()?;
                    Some((*g, p, *room.get(&p)?))
                });
                let el: AnyElement = if leaving {
                    widgets::fade_out(ElementId::Name(format!("leave-{}", pid.unwrap_or(0)).into()), el)
                } else if let Some((g, p, k)) = slide {
                    div()
                        .relative()
                        .child(el)
                        .with_animation(
                            ElementId::Name(format!("slide-{g}-{p}").into()),
                            Animation::new(SLIDE).with_easing(ease_out_quint()),
                            move |d, t| d.top(rems(ROW_H * k as f32 * (1.0 - t))),
                        )
                        .into_any_element()
                } else {
                    el
                };
                let row = div()
                    .relative()
                    .rounded(cx.theme().radius_lg)
                    // A click puts the keys on this row; with Ctrl it also
                    // picks or unpicks it, with Shift it picks every row
                    // from the last one clicked, like a file explorer.
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                            this.cursor = Some(ix);
                            window.focus(&this.focus, cx);
                            if e.modifiers.shift {
                                this.select_range(this.anchor.unwrap_or(ix), ix, e.modifiers.control);
                                cx.notify();
                            } else {
                                this.anchor = Some(ix);
                                if e.modifiers.control {
                                    this.toggle_row(ix);
                                    cx.notify();
                                }
                            }
                        }),
                    )
                    .child(el);
                // The keys' row: the theme's focus ring, which doesn't move it.
                widgets::focus_ring(row, cursor == Some(ix), cx).into_any_element()
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
                    // Order groups and the shader choice say it already.
                    let plain = !self.order_mode() && self.class != cf::CLASS_SHADERS;
                    let in_use = plain && active_rp.contains(&f)
                        || active_shader
                            .as_ref()
                            .is_some_and(|s| *s == f || *s == f.trim_end_matches(".zip"));
                    let new = hopper::time::parse_iso(&a.date_added)
                        .is_some_and(|t| now - t < 3 * 86_400);
                    let flags = Flags {
                        added: inst.pack.is_some() && !a.from_pack,
                        in_use,
                        new,
                        rank: self.table.rank.get(i).copied(),
                    };
                    self.addon_row(a, updates.get(&a.project_id), flags, cx)
                }
                Row::Section(title, hint) => self.section_row(title, hint, cx),
                Row::ShaderOff => self.shader_off_row(cx),
                Row::Copies(n, shown, at) => h_flex()
                    .h(rems(ROW_H))
                    .w_full()
                    .px_3()
                    .gap_3()
                    .child(widgets::caption(
                        format!(
                            "{n} pack{} also in {at}, which loads {} into every world",
                            if *n == 1 { "" } else { "s" },
                            if *n == 1 { "it" } else { "them" }
                        ),
                        cx,
                    ))
                    .child(
                        Button::new("show-copies")
                            .small()
                            .ghost()
                            .icon(if *shown { IconName::ChevronUp } else { IconName::ChevronDown })
                            .label(if *shown { "Hide" } else { "Show" })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_copies = !this.show_copies;
                                this.ensure_rows();
                                cx.notify();
                            })),
                    )
                    .into_any_element(),
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
                        .hover(|s| widgets::row_hover(s, cx))
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

    /// The file of row `ix`, when it's an addon or loose jar.
    fn row_file(&self, ix: usize) -> Option<String> {
        let inst = self.table.inst.as_ref()?;
        match self.table.rows.get(ix)? {
            Row::Addon(i) => Some(inst.addons.get(*i)?.file_name.clone()),
            Row::Loose(name) => Some(name.clone()),
            Row::LooseHeader(_) | Row::Section(..) | Row::ShaderOff | Row::Copies(..) => None,
        }
    }

    /// The row showing `file`.
    fn row_ix_of(&self, file: &str) -> Option<usize> {
        (0..self.table.rows.len()).find(|&ix| self.row_file(ix).as_deref() == Some(file))
    }

    /// A tick box pressed: Shift ticks the rows from the last pressed box,
    /// else a drag starts (ticking, or unticking from a ticked box).
    fn pick_down(&mut self, file: &str, shift: bool, cx: &mut Context<Self>) {
        let Some(ix) = self.row_ix_of(file) else { return };
        match self.pick_anchor.or(self.cursor).filter(|_| shift) {
            Some(a) => {
                self.select_range(a, ix, true);
                self.pick = None;
            }
            None => {
                let p = DragPick::new(ix, !self.selected.contains(file), self.selected.clone());
                self.selected = p.apply(|i| self.row_file(i));
                self.pick = Some(p);
            }
        }
        self.pick_anchor = Some(ix);
        cx.notify();
    }

    fn pick_move(&mut self, y: Pixels, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut p) = self.pick.take() else { return };
        self.pick_row_h = rems(ROW_H).to_pixels(window.rem_size());
        if p.moved(y, &self.scroll, self.pick_row_h, self.table.rows.len()) {
            self.selected = p.apply(|i| self.row_file(i));
            cx.notify();
        }
        if let Some(c) = &p.chip {
            let t = p.label();
            c.update(cx, |c, cx| {
                c.text = t;
                cx.notify();
            });
        }
        self.pick = Some(p);
        if self._pick_tick.is_none() {
            self._pick_tick = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(std::time::Duration::from_millis(16)).await;
                    let go = this.update(cx, |this, cx| this.pick_frame(cx)).unwrap_or(false);
                    if !go {
                        break;
                    }
                }
            }));
        }
    }

    /// One frame while a drag runs: scrolls at the edges. False once it ended.
    fn pick_frame(&mut self, cx: &mut Context<Self>) -> bool {
        if !cx.has_active_drag() {
            self.pick_end(cx);
            return false;
        }
        let Some(mut p) = self.pick.take() else {
            self._pick_tick = None;
            return false;
        };
        if p.edge_scroll(&self.scroll, self.pick_row_h, self.table.rows.len()) {
            self.selected = p.apply(|i| self.row_file(i));
            if let Some(c) = &p.chip {
                let t = p.label();
                c.update(cx, |c, cx| {
                    c.text = t;
                    cx.notify();
                });
            }
            cx.notify();
        }
        self.pick = Some(p);
        true
    }

    fn pick_end(&mut self, cx: &mut Context<Self>) {
        if self.pick.take().is_some() {
            cx.notify();
        }
        self._pick_tick = None;
    }

    /// Picks row `ix`, or unpicks it if picked (Ctrl+click).
    fn toggle_row(&mut self, ix: usize) {
        if let Some(f) = self.row_file(ix)
            && !self.selected.remove(&f)
        {
            self.selected.insert(f);
        }
    }

    /// Picks rows `a` to `b` (Shift+click); without `add` (Ctrl) the rows
    /// picked before are let go first.
    fn select_range(&mut self, a: usize, b: usize, add: bool) {
        if !add {
            self.selected.clear();
        }
        for ix in a.min(b)..=a.max(b) {
            if let Some(f) = self.row_file(ix) {
                self.selected.insert(f);
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
            self.toggle(vec![a.file_name.clone()], !self.row_on(a), window, cx);
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
        let hunt_chip = self.hunt_chip(&a.file_name, cx);
        // Resource packs in game order: grip, place and ↑↓ (or Use).
        let order_cell = self.order_mode().then(|| self.order_cell(&a.file_name, &a.name, flags.rank, cx));
        let shader_on = self.class == cf::CLASS_SHADERS && self.is_active_shader(&a.file_name);
        // Iris and Oculus run one pack: a choice, not switches.
        let shader_radio = (self.class == cf::CLASS_SHADERS).then(|| {
            let id = ElementId::Name(format!("use:{}", a.file_name).into());
            self.shader_radio(id, Some(a.file_name.clone()), shader_on, cx)
        });
        let store = self.store.read(cx);
        let theme = cx.theme();
        let hold = self
            .root
            .as_ref()
            .filter(|r| !store.queues_updates(r))
            .and_then(|r| store.edit_hold_reason(r));
        // `Some(true)` downloading now, `Some(false)` queued.
        let updating = self.root.as_ref().and_then(|r| store.update_state(r, a.project_id));
        // Its new file is with the download window (the author allows
        // downloads from CurseForge's website only).
        let from_web = a.project_id != 0
            && self.root.as_ref().is_some_and(|r| store.pending(r).iter().any(|p| p.project_id == a.project_id));
        // A Paxi pack no site knows shows its own pack.png.
        // A global folder's pack, or one the instance doesn't track.
        let global = self.global_dir().is_some() || self.is_loose(&a.file_name);
        let icon = store.image(&a.icon).map(Path::to_path_buf).or_else(|| {
            (global && !a.icon.starts_with("http")).then(|| PathBuf::from(&a.icon)).filter(|p| p.is_file())
        });
        let file = a.file_name.clone();
        let picked = self.selected.contains(&file);
        // Resource packs of the instance: on is "used in game".
        let on = self.row_on(a);
        let dim = !on;
        let id = |p: &str| ElementId::Name(format!("{p}:{file}").into());
        let (f_sel, f_on, f_rm, f_menu) = (file.clone(), file.clone(), file.clone(), file.clone());
        let also = self.table.also.get(&a.file_name.trim_end_matches(".disabled").to_lowercase()).cloned();

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
        // The Side cell, in its column or (narrow) by the name.
        let side_el = (self.cols.side || self.cols.side_inline).then(|| {
            let card = self.side_card(a, cx);
            let theme = cx.theme();
            crate::side_card::wrap(
                id("side-card"),
                h_flex()
                    .id(id("side"))
                    .flex_none()
                    .gap_1()
                    .px_1()
                    .py_0p5()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .hover(|s| widgets::row_hover(s, cx))
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
            )
        });
        let (side_inline, side_col) = if self.cols.side_inline { (side_el, None) } else { (None, side_el) };
        // The Author column gone: the name's tooltip says it.
        let by = (!self.cols.author && !a.author.is_empty()).then(|| format!("by {}", a.author));

        let tags = widgets::mod_tags(
            widgets::ModTags {
                key: &file,
                // Shaders say "Active" instead.
                in_use: flags.in_use && self.class != cf::CLASS_SHADERS,
                // One marker: "Yours" says it's new to the pack already.
                new: flags.new && !flags.added,
                added: flags.added,
                env: &a.env,
                // Narrow, the categories give the version chip room.
                categories: if self.compact { &[] } else { store.categories(a) },
                modrinth: !a.mr_project.is_empty(),
                crashed: self.crashed.get(&a.file_name.trim_end_matches(".disabled").to_lowercase()).map(String::as_str),
            },
            cx,
        );
        let name_line = h_flex()
            .gap_1p5()
            .child({
                let pid = a.project_id;
                let tip: Option<SharedString> = match (by, pid != 0) {
                    (Some(by), true) => Some(format!("{by}
Details").into()),
                    (Some(by), false) => Some(by.into()),
                    (None, true) => Some("Details".into()),
                    (None, false) => None,
                };
                div()
                    .id(id("name"))
                    .min_w_0()
                    .text_sm()
                    .font_medium()
                    .truncate()
                    .when_some(tip, |d, tip| {
                        d.tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
                    })
                    .when(pid != 0, |d| {
                        d.clickable(cx)
                            .hover(|s| s.text_color(theme.primary))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.open_peek(pid, cx);
                            }))
                    })
                    .child(a.name.clone())
            })
            .children(side_inline)
            .children(hunt_chip)
            .when(shader_on, |r| r.child(widgets::chip(theme.success).child("Active")))
            .when_some(also, |r, also| {
                r.child(
                    div()
                        .id(id("also"))
                        .flex_none()
                        .child(widgets::chip(theme.primary).child(format!("Also in: {also}")))
                        .tooltip(|w, cx| {
                            gpui_kit::component::tooltip::Tooltip::new("The same file is in these places too").build(w, cx)
                        }),
                )
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

        let done = self.recent.contains_key(&a.project_id);
        // Hover-only controls stay shown on a picked row, the key cursor's
        // row and the row whose menu is open.
        let shown = picked
            || self.cursor.and_then(|c| self.row_file(c)).as_deref() == Some(file.as_str())
            || self.menu_for.as_deref() == Some(file.as_str());
        let progress = self
            .root
            .as_ref()
            .filter(|_| updating == Some(true))
            .and_then(|r| store.update_progress(r, a.project_id));
        let compact = self.compact;
        let pill_w = rems(if compact { widgets::SPLIT_W_COMPACT } else { widgets::SPLIT_W });
        let action = col_action(compact).when(done, |c| {
            // Updated: a quiet green mark where the button was, until the
            // row goes.
            c.child(widgets::done_mark(id("updated"), "Updated", false, cx))
        }).when_some(update.cloned().filter(|_| !done), |c, u| {
            let tip = format!("{}  →  {}", u.from, u.file.file_name);
            let (pid, file) = (u.project_id, u.file.clone());
            let view = cx.entity();
            let split = widgets::split(
                widgets::split_lead(
                        Button::new(id("update"))
                            .flex_1()
                            .small()
                            .outline()
                            .icon(IconName::Download)
                            // Narrow, the icon alone (the tooltip says the rest).
                            .when(!compact, |b| {
                                b.label(match (updating, progress) {
                                    (Some(true), Some(p)) => format!("Updating {}%", (p * 100.0).floor() as u32),
                                    (Some(true), None) => "Updating".to_string(),
                                    (Some(false), _) => "Queued".to_string(),
                                    (None, _) if from_web => "From website".to_string(),
                                    (None, _) => "Update".to_string(),
                                })
                            })
                            .loading(updating == Some(true))
                            .tooltip(if from_web && updating.is_none() {
                                "The author only allows downloads from CurseForge's website: Hopper's download window is getting it, and adds it when it arrives".to_string()
                            } else if let Some(h) = hold {
                                h.to_string()
                            } else if dim {
                                // Updating keeps it off (see install::apply_updates).
                                format!("{tip}
It stays disabled after updating: turn it on in the On column.")
                            } else {
                                tip
                            })
                            .disabled(hold.is_some() || updating.is_some() || from_web)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.settle.clicked();
                                this.run_updates(vec![u.clone()], cx);
                                this.pull_recent(cx);
                            })),
                ),
                id("changes"),
                widgets::Tint::Outline,
                gpui_kit::component::Size::Small,
                "What changed",
                move |_, window, cx| view.update(cx, |this, cx| this.versions(pid, Some(file.clone()), window, cx)),
                cx,
            );
            // The download fills the button from the left.
            c.child(widgets::progress_fill(split.min_w(pill_w), progress, cx))
        })
        // Nothing to update: the changelog alone, in the pill's place, only
        // on the hovered or selected row so the column stays quiet.
        .when(update.is_none() && !done && a.project_id != 0, |c| {
            let (pid, view) = (a.project_id, cx.entity());
            c.child(
                div()
                    .when(!shown, |d| d.invisible().group_hover(ROW_GROUP, |s| s.visible()))
                    .child(widgets::split_alone(
                        id("changes"),
                        compact,
                        gpui_kit::component::Size::Small,
                        "Versions and changelogs",
                        move |_, window, cx| view.update(cx, |this, cx| this.versions(pid, None, window, cx)),
                        cx,
                    )),
            )
        });

        let view = cx.entity();
        // Copy to / Move to: the other places packs of this kind can be.
        let places = self.other_places();
        // A mod of the instance: its config can be reset.
        let reset_cfg = (!global && self.class == cf::CLASS_MODS)
            .then(|| self.root.clone())
            .flatten()
            .map(|r| (self.store.clone(), r, a.project_id, a.file_name.clone()));
        let (pid_menu, update_file) = (a.project_id, update.map(|u| u.file.clone()));
        let (page, locked, mr, has_project) = (
            a.page.clone(),
            a.locked,
            !a.mr_project.is_empty(),
            a.project_id != 0,
        );
        let menu_fn = Self::row_menu(
            view,
            f_menu,
            places,
            page,
            update_file,
            reset_cfg,
            (pid_menu, locked, mr, has_project, global, on),
        );
        h_flex()
            .id(id("row"))
            .group(ROW_GROUP)
            .h(rems(ROW_H))
            .w_full()
            .pl_3()
            .pr_2()
            .gap_3()
            .relative()
            .rounded(theme.radius_lg)
            // A clear border so a selection's edge doesn't shift the row.
            .border_1()
            .border_color(theme.transparent)
            .when(picked, |r| widgets::row_selected(r, cx))
            // A quiet wash, no outline.
            .when(!picked, |r| r.hover(|s| widgets::row_hover(s, cx)))
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
                {
                    let view = cx.entity();
                    col_check()
                        .id(id("pick"))
                        .h_full()
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                                this.pick_down(&f_sel, e.modifiers.shift, cx)
                            }),
                        )
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .on_drag(PickDrag, move |_, _, _, cx| crate::drag_pick::chip_for(&view, cx))
                        .child(Checkbox::new(id("sel")).checked(picked))
                },
            )
            .children(order_cell)
            .child({
                let pid = a.project_id;
                div()
                    .id(id("thumb"))
                    .when(dim, |d| d.opacity(0.45))
                    .when(pid != 0, |d| {
                        d.clickable(cx).on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.open_peek(pid, cx);
                        }))
                    })
                    .child(widgets::thumb(icon.as_deref(), rems(2.5), cx))
            })
            .child(
                self.name_cell()
                    .flex()
                    .flex_col()
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
                                chip.bg(primary.opacity(widgets::alpha::TINT))
                                    .group_hover("file", move |s| s.bg(primary.opacity(widgets::alpha::TINT * 2.)))
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
                                let v = if global {
                                    hopper::instance_ops::pack_version_label(&a.file_name)
                                } else {
                                    hopper::scan::short_version(&a.file_name)
                                };
                                (!v.is_empty()).then(|| {
                                    chip.bg(theme.muted)
                                        .text_color(theme.muted_foreground)
                                        .group_hover("file", move |s| {
                                            s.bg(primary.opacity(widgets::alpha::TINT)).text_color(primary)
                                        })
                                        .child(div().min_w_0().truncate().child(v))
                                })
                            }
                        };
                        // The chip says the version: the file name moves to
                        // the tooltip rather than repeat it.
                        let has_chip = chip.is_some();
                        let tip: SharedString = if has_chip {
                            format!("{}
Change version", a.file_name).into()
                        } else {
                            "Change version".into()
                        };
                        h_flex()
                            .id(id("file"))
                            .min_w_0()
                            .gap_2()
                            .children(chip)
                            .when(!has_chip, |r| {
                                r.child(
                                    widgets::caption(a.file_name.clone(), cx)
                                        .text_xs()
                                        .min_w_0()
                                        .group_hover("file", move |s| s.text_color(primary)),
                                )
                            })
                            .when(pid != 0, |d| {
                                d.clickable(cx)
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
                                    .tooltip(move |w, cx| {
                                        gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx)
                                    })
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.versions(pid, upd.clone(), window, cx)
                                    }))
                            })
                    })),
            )
            .when(self.action_fits(), |r| r.child(action))
            .children(self.spacer())
            .when(self.cols.author, |r| {
                r.child(
                    col_author()
                        .text_sm()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(a.author.clone()),
                )
            })
            .when_some(side_col, |r, el| r.child(col_env().child(el)))
            .child(col_active().child(if let Some(radio) = shader_radio {
                radio
            } else {
                let order = self.order_mode();
                Switch::new(id("on"))
                    .small()
                    .checked(on)
                    .tooltip(match (order, on) {
                        (true, true) => "Stop using it in game",
                        (true, false) => "Use it in game (adds it at the top)",
                        (false, true) => "Turn off",
                        (false, false) => "Turn on",
                    })
                    .on_change(cx.listener(move |this, v: &bool, w, cx| {
                        this.toggle(vec![f_on.clone()], *v, w, cx)
                    }))
                    .into_any_element()
            }))
            .when(self.cols.date, |r| r.child({
                let exact = hopper::time::date_of(&a.date_added);
                col_date()
                    .id(id("added"))
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .when(!exact.is_empty(), |d| {
                        d.tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(exact.clone()).build(w, cx))
                    })
                    .child(hopper::time::relative(&a.date_added))
            }))
            .child(
                col_trash()
                    .child(
                        // The right-click menu, for those who don't right-click.
                        div()
                            .when(!shown, |d| d.invisible().group_hover(ROW_GROUP, |s| s.visible()))
                            .child({
                                let (m, v, f) = (menu_fn.clone(), cx.entity(), file.clone());
                                Button::new(id("more"))
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Ellipsis)
                                    .tooltip("More")
                                    .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, w, cx| m(menu, w, cx))
                                    .on_open_change(move |open, _, cx| {
                                        let f = f.clone();
                                        v.update(cx, |this, cx| {
                                            if *open {
                                                this.menu_for = Some(f);
                                            } else if this.menu_for.as_ref() == Some(&f) {
                                                this.menu_for = None;
                                            }
                                            cx.notify();
                                        })
                                    })
                            }),
                    )
                    .child(
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
            .map(|r| self.order_drop(r, &file, flags.rank, cx))
            .context_menu(move |menu, w, cx| menu_fn(menu, w, cx))
            .into_any_element()
    }

    /// The row's menu, for right-click and its ⋯ button.
    #[allow(clippy::too_many_arguments)]
    fn row_menu(
        view: Entity<Self>,
        f_menu: String,
        places: Vec<(String, PathBuf)>,
        page: String,
        update_file: Option<hopper::cf::File>,
        reset_cfg: Option<(Entity<Store>, PathBuf, u64, String)>,
        (pid_menu, locked, mr, has_project, global, on): (u64, bool, bool, bool, bool, bool),
    ) -> Rc<dyn Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu> {
            Rc::new(move |menu, _, _| {
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
                let menu = menu.when(has_project, |m| {
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
                .when_some(reset_cfg.clone(), |m, (store, root, pid, file)| {
                    m.item(
                        PopupMenuItem::new("Reset config…")
                            .icon(IconName::Settings2)
                            .on_click(move |_, window, cx| {
                                crate::versions::open_reset_config(store.clone(), root.clone(), pid, file.clone(), window, cx)
                            }),
                    )
                })
                .when(!page.is_empty(), |m| {
                    m.item(
                        PopupMenuItem::new(if mr { "Open on Modrinth" } else { "Open on CurseForge" })
                            .icon(IconName::ExternalLink)
                            .on_click(move |_, _, cx| cx.open_url(&page)),
                    )
                })
                .when(has_project && !global, |m| {
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
                .when(!global, |m| m.item(
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
                ))
                .item(
                    PopupMenuItem::new(if on { "Disable" } else { "Enable" }).on_click(
                        move |_, window, cx| {
                            v4.update(cx, |this, cx| {
                                this.toggle(vec![f4.clone()], !on, window, cx)
                            })
                        },
                    ),
                );
                // Copy to / Move to the other places this kind of pack loads from.
                crate::datapacks::copy_move_items(menu, &view, &f, &places)
                .separator()
                .item(PopupMenuItem::new("Remove").icon(IconName::Trash).on_click(
                    move |_, window, cx| {
                        v5.update(cx, |this, cx| this.ask_remove(vec![f5.clone()], window, cx))
                    },
                ))
            })
    }

    /// Whether the rows show the Update / Changelog column.
    /// Fits the optional columns to the room. When the name would get
    /// less than NAME_MIN they step aside in order: Added, Author (into
    /// the name's tooltip), Side (to an icon by the name); then Update
    /// shrinks to its icon. Wide, the name stops at NAME_MAX. Only from
    /// widths known before layout, so it never flips back and forth.
    fn fit_columns(&mut self, window: &Window) {
        let room = self.room.unwrap_or_else(|| window.viewport_size().width / window.rem_size());
        let has_side = self.class != cf::CLASS_DATAPACKS && self.table.has_side;
        let mut c = Cols { date: !self.tight, author: true, side: has_side, side_inline: false, cap: false };
        let mut compact = self.compact;
        if self.name_room(room, &c, compact) < NAME_MIN {
            c.date = false;
        }
        if self.name_room(room, &c, compact) < NAME_MIN {
            c.author = false;
        }
        if has_side && self.name_room(room, &c, compact) < NAME_MIN {
            c.side = false;
            c.side_inline = true;
        }
        if self.name_room(room, &c, compact) < NAME_MIN {
            compact = true;
        }
        c.cap = self.name_room(room, &c, compact) > NAME_MAX;
        self.cols = c;
        self.compact = compact;
    }

    /// The name column's width in rem with these columns, from the
    /// widths in `col_*` and the row's padding and gaps.
    fn name_room(&self, room: f32, c: &Cols, compact: bool) -> f32 {
        // List padding, row padding and border, tick, icon, On, ⋯ and Remove.
        let mut fixed = 3.0 + 1.4 + 1.5 + 2.5 + 4.0 + 4.0;
        // In-flow cells, the name included.
        let mut cells = 5;
        for (on, w) in [
            (self.order_mode(), 5.75),
            (self.action_fits(), if compact { widgets::SPLIT_W_COMPACT + 0.25 } else { 9.5 }),
            (c.author, 10.0),
            (c.side, 3.0),
            (c.date, 6.0),
        ] {
            if on {
                fixed += w;
                cells += 1;
            }
        }
        room - fixed - 0.75 * (cells - 1) as f32
    }

    /// The name cell. Wide, it is NAME_MAX (it may still shrink) and a
    /// spacer after the actions takes the rest; a max width on a growing
    /// cell would share the room with the spacer instead.
    pub(crate) fn name_cell(&self) -> Div {
        let d = div().min_w_0();
        if self.cols.cap { d.flex_shrink(1.).w(rems(NAME_MAX)) } else { d.flex_1() }
    }

    /// Takes the room the capped name leaves, after the actions.
    pub(crate) fn spacer(&self) -> Option<Div> {
        self.cols.cap.then(|| div().flex_1())
    }

    pub(crate) fn action_fits(&self) -> bool {
        !self.tight || !self.table.updates.is_empty()
    }

    fn versions(
        &mut self,
        pid: u64,
        update: Option<hopper::cf::File>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A global or Paxi folder isn't the instance's own content, so its
        // packs get the one changelog (the update's, else the installed
        // file's) instead of the versions to switch between.
        if self.global_dir().is_some() || self.loose_pid(pid) {
            let root = self.root.clone();
            if let Some(f) = update {
                return crate::versions::open_file(self.store.clone(), root, f, window, cx);
            }
            let found = self
                .table
                .inst
                .as_ref()
                .and_then(|i| i.addons.iter().find(|a| a.project_id == pid))
                .map(|a| (a.file_id, a.name.clone()));
            match found {
                Some((file_id, name)) if file_id != 0 => {
                    crate::versions::open_file_id(self.store.clone(), root, pid, file_id, name, window, cx)
                }
                _ => self.open_peek(pid, cx),
            }
            return;
        }
        if let Some(root) = self.root.clone() {
            crate::versions::open(self.store.clone(), root, pid, false, update, window, cx);
        }
    }

    fn show_in_folder(&self, file: &str, cx: &App) {
        if let Some(dir) = self.global_dir().or_else(|| self.own_dir().filter(|_| self.is_loose(file))) {
            let path = Self::global_path(&dir, file);
            let _ = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
            return;
        }
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

/// How big a jump an update is, for the review before Update all.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Risk {
    /// Same major and minor version.
    Patch,
    /// A new major or minor version, or one Hopper can't read.
    Major,
    /// An alpha or beta file.
    Pre,
}

impl Risk {
    /// Best effort, from the versions in the two file names.
    fn of(u: &Update) -> Self {
        if u.file.release_type > 1 {
            return Risk::Pre;
        }
        let (old, new) = u.versions();
        let (a, b) = (version_nums(&old), version_nums(&new));
        match (a.len(), b.len()) {
            (0, _) | (_, 0) => Risk::Major,
            // A lone build number counts as a patch.
            (1, 1) => Risk::Patch,
            _ if a.first() != b.first() || a.get(1) != b.get(1) => Risk::Major,
            _ => Risk::Patch,
        }
    }

    /// A tinted chip, like every chip.
    fn tag(self, cx: &App) -> gpui_kit::component::tag::Tag {
        let theme = cx.theme();
        match self {
            Risk::Patch => widgets::chip(theme.success).child("Patch"),
            Risk::Major => widgets::chip(theme.warning).child("Major"),
            Risk::Pre => widgets::chip(theme.danger).child("Alpha / beta"),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Risk::Patch => "fixes within the same version",
            Risk::Major => "new major or minor versions: worth a look at the changelog",
            Risk::Pre => "test versions, not marked as releases",
        }
    }
}

/// The numbers at the start of a version: "5.27.26-beta" gives [5, 27, 26].
fn version_nums(v: &str) -> Vec<u64> {
    let Some(start) = v.find(|c: char| c.is_ascii_digit()) else {
        return Vec::new();
    };
    v[start..]
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .next()
        .unwrap_or("")
        .split('.')
        .map_while(|p| p.parse().ok())
        .collect()
}

/// What would stop the game loading with `file` in place, known before it
/// downloads: from the dependencies the site lists for it. Returns the
/// problems with a name (an installed mod turned off, or one it refuses to
/// load beside) and the required projects the instance lacks, by id.
///
/// A newer loader or Minecraft the file needs is only in the jar, so it
/// shows after the update, in the "problems will stop the game" banner.
fn load_problems(inst: &Instance, file: &cf::File) -> (Vec<String>, Vec<u64>) {
    let required: Vec<u64> = match &file.modrinth {
        Some(mr) => mr.requires.iter().map(|p| hopper::modrinth::num_id(p)).collect(),
        None => file
            .dependencies
            .iter()
            .filter(|d| d.relation_type == cf::DEP_REQUIRED)
            .map(|d| d.mod_id)
            .collect(),
    };
    let (mut issues, mut missing) = (Vec::new(), Vec::new());
    for dep in required {
        // Updates don't add mods, nor turn them on.
        match inst.addons.iter().find(|a| a.project_id == dep) {
            Some(a) if !a.enabled => issues.push(format!("needs {}, which is turned off", a.name)),
            Some(_) => {}
            None if !missing.contains(&dep) => missing.push(dep),
            None => {}
        }
    }
    for d in file.dependencies.iter().filter(|d| d.relation_type == cf::DEP_INCOMPATIBLE) {
        if let Some(a) = inst.addons.iter().find(|a| a.project_id == d.mod_id && a.enabled) {
            issues.push(format!("won't load beside {}", a.name));
        }
    }
    (issues, missing)
}

/// One update in the review.
struct ReviewRow {
    update: Update,
    risk: Risk,
    name: String,
    icon: Option<PathBuf>,
    from: String,
    to: String,
    /// What would stop the game loading, by name (see [`load_problems`]).
    issues: Vec<String>,
    /// Required projects the instance lacks: id and, once looked up, the
    /// name (empty when the lookup failed).
    missing: Vec<(u64, Option<String>)>,
}

impl ReviewRow {
    /// One line on what would stop the game loading, if anything.
    fn problem(&self) -> Option<String> {
        let mut parts = self.issues.clone();
        for (_, name) in &self.missing {
            parts.push(match name.as_deref() {
                Some(n) if !n.is_empty() => format!("needs {n}, which isn't installed"),
                _ => "needs a mod that isn't installed".to_string(),
            });
        }
        (!parts.is_empty()).then(|| parts.join("; "))
    }
}

/// The review before Update all: every pending update, grouped, with a box
/// to skip each one or a whole group.
struct UpdateReview {
    rows: Vec<ReviewRow>,
    /// Skipped, by project.
    skip: HashSet<u64>,
    /// Skipped only for a problem, and not touched since: ticked again if
    /// the problem turns out not to be one.
    held: HashSet<u64>,
}

impl UpdateReview {
    /// Names the required projects looked up; `None` is one that's
    /// installed after all (from the other site).
    fn resolve(&mut self, found: &[(u64, Option<String>)]) {
        for r in &mut self.rows {
            r.missing.retain_mut(|(id, name)| match found.iter().find(|(f, _)| f == id) {
                Some((_, None)) => false,
                Some((_, Some(n))) => {
                    *name = Some(n.clone());
                    true
                }
                None => true,
            });
            let pid = r.update.project_id;
            if r.issues.is_empty() && r.missing.is_empty() && self.held.remove(&pid) {
                self.skip.remove(&pid);
            }
        }
    }

    fn chosen(&self) -> Vec<Update> {
        self.rows
            .iter()
            .filter(|r| !self.skip.contains(&r.update.project_id))
            .map(|r| r.update.clone())
            .collect()
    }

    fn set_group(&mut self, risk: Risk, on: bool) {
        for r in self.rows.iter().filter(|r| r.risk == risk) {
            self.held.remove(&r.update.project_id);
            if on {
                self.skip.remove(&r.update.project_id);
            } else {
                self.skip.insert(r.update.project_id);
            }
        }
    }
}

impl Render for UpdateReview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let rem = window.rem_size();
        let row_h = rems(2.);
        let groups: Vec<(Risk, Vec<&ReviewRow>)> = [Risk::Patch, Risk::Major, Risk::Pre]
            .into_iter()
            .map(|g| (g, self.rows.iter().filter(|r| r.risk == g).collect::<Vec<_>>()))
            .filter(|(_, rows)| !rows.is_empty())
            .collect();
        // Rows whose new file would stop the game loading.
        let n_bad = self.rows.iter().filter(|r| r.problem().is_some()).count();
        // As tall as the list, up to the window less the dialog's top
        // offset, title and footer (the same math as every dialog, see
        // versions.rs); then the list scrolls.
        let want = row_h.to_pixels(rem) * self.rows.len() as f32
            + rem * (4. + 3. * groups.len() as f32 + n_bad as f32 + if n_bad > 0 { 1.5 } else { 0. });
        let room = (window.viewport_size().height - rem * crate::widgets::DIALOG_CHROME_FOOTER).max(rem * 12.);
        let summary = h_flex().gap_2().flex_wrap().children(groups.iter().map(|(g, rows)| {
            let picked = rows.iter().filter(|r| !self.skip.contains(&r.update.project_id)).count();
            h_flex()
                .gap_1()
                .child(g.tag(cx))
                .child(widgets::caption(format!("{picked} of {}", rows.len()), cx).text_xs())
        }));
        let bad_note = (n_bad > 0).then(|| {
            h_flex()
                .gap_1p5()
                .text_sm()
                .text_color(theme.danger)
                .child(Icon::new(IconName::TriangleAlert).small().flex_none())
                .child(
                    div().min_w_0().child(if n_bad == 1 {
                        "1 would stop the game loading: left unticked".to_string()
                    } else {
                        format!("{n_bad} would stop the game loading: left unticked")
                    }),
                )
        });
        let sections = groups.iter().map(|(g, rows)| {
            let g = *g;
            let picked = rows.iter().filter(|r| !self.skip.contains(&r.update.project_id)).count();
            let key = match g {
                Risk::Patch => "patch",
                Risk::Major => "major",
                Risk::Pre => "pre",
            };
            v_flex()
                .child(
                    h_flex()
                        .gap_2()
                        .py_1()
                        .child(
                            Checkbox::new(ElementId::Name(format!("review-group:{key}").into()))
                                .checked(picked == rows.len())
                                .on_click(cx.listener(move |this, v: &bool, window, cx| {
                                    this.set_group(g, *v);
                                    // The footer's count is the dialog's.
                                    window.refresh();
                                    cx.notify();
                                })),
                        )
                        .child(div().w_8().text_sm().font_semibold().child(rows.len().to_string()))
                        .child(g.tag(cx))
                        .child(widgets::caption(g.label(), cx).text_xs().min_w_0()),
                )
                .children(rows.iter().map(|r| {
                    let pid = r.update.project_id;
                    let on = !self.skip.contains(&pid);
                    let problem = r.problem();
                    // Skipped rows fade; their warning stays readable.
                    let dim = |d: Div| if on { d } else { d.opacity(0.55) };
                    h_flex()
                        .min_h(row_h)
                        .pl_6()
                        .gap_2()
                        .child(
                            Checkbox::new(("review-row", pid as usize))
                                .checked(on)
                                .on_click(cx.listener(move |this, v: &bool, window, cx| {
                                    this.held.remove(&pid);
                                    if *v {
                                        this.skip.remove(&pid);
                                    } else {
                                        this.skip.insert(pid);
                                    }
                                    window.refresh();
                                    cx.notify();
                                })),
                        )
                        .child(dim(div().flex_none().child(widgets::thumb(r.icon.as_deref(), rems(1.25), cx))))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(dim(div().text_sm().truncate().child(r.name.clone())))
                                .when_some(problem, |c, p| {
                                    let tip = SharedString::from(p.clone());
                                    c.child(
                                        h_flex()
                                            .id(("review-problem", pid as usize))
                                            .gap_1()
                                            .min_w_0()
                                            .text_xs()
                                            .text_color(theme.danger)
                                            .child(Icon::new(IconName::TriangleAlert).xsmall().flex_none())
                                            .child(div().min_w_0().truncate().child(p))
                                            .tooltip(move |w, cx| {
                                                gpui_kit::component::tooltip::Tooltip::new(format!(
                                                    "{tip}. Hopper doesn't add or turn on mods with an update."
                                                ))
                                                .build(w, cx)
                                            }),
                                    )
                                }),
                        )
                        .child(dim(
                            // The versions on a tinted chip, like the mods list's.
                            h_flex()
                                .flex_none()
                                .max_w(rems(14.))
                                .min_w_0()
                                .gap_1()
                                .px_1p5()
                                .rounded(px(crate::theme::RADIUS))
                                .bg(theme.primary.opacity(widgets::alpha::TINT))
                                .text_xs()
                                .child(div().min_w_0().truncate().text_color(theme.muted_foreground).child(r.from.clone()))
                                .child(Icon::new(IconName::ArrowRight).xsmall().flex_none().text_color(theme.primary))
                                .child(div().min_w_0().truncate().text_color(theme.primary).child(r.to.clone())),
                        ))
                }))
        });
        v_flex()
            .h(want.min(room))
            .gap_3()
            .child(
                v_flex()
                    .gap_2()
                    .child(div().text_sm().text_color(cx.theme().muted_foreground).child(
                        "Old files go to .hopper-backups, and you can undo when it's done. A snapshot of the whole set is saved first: restore it from Backups.",
                    ))
                    .child(summary)
                    .children(bad_note),
            )
            .child(
                div()
                    .id("review-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(v_flex().gap_3().children(sections)),
            )
    }
}

/// A mod an update replaced: its entry and file before, and where the old
/// file went.
#[derive(Clone)]
struct Replaced {
    old: Addon,
    original: PathBuf,
    backup: PathBuf,
}

/// How a reviewed Update all went.
struct BulkSum {
    updated: usize,
    failed: usize,
    /// Waiting for Hopper's download window (website-only files).
    web: usize,
    replaced: Vec<Replaced>,
}

impl BulkSum {
    /// Reads the saved instance and the backups (off the UI thread).
    fn read(root: &Path, olds: Vec<(Addon, PathBuf)>, since: u64) -> Option<Self> {
        let after = hopper::instances::load(root)?;
        let web: HashSet<u64> = hopper::modpack::pending_downloads(&after).iter().map(|p| p.project_id).collect();
        let mut sum = BulkSum { updated: 0, failed: 0, web: 0, replaced: Vec::new() };
        for (old, original) in olds {
            let now = after.addons.iter().find(|a| a.project_id == old.project_id);
            match now {
                Some(a) if a.file_id != old.file_id || a.file_name != old.file_name => {
                    sum.updated += 1;
                    // None when the new file took the old one's name, or the
                    // game still holds the old one.
                    if let Some(backup) = find_backup(root, &original, since) {
                        sum.replaced.push(Replaced { old, original, backup });
                    }
                }
                _ if web.contains(&old.project_id) => sum.web += 1,
                _ => sum.failed += 1,
            }
        }
        Some(sum)
    }

    /// "Updated N, M failed", with Undo when the old files can go back.
    fn notify(self, store: Entity<Store>, root: PathBuf, window: &mut Window, cx: &mut App) {
        let mut text = format!("Updated {}", self.updated);
        if self.failed > 0 {
            text.push_str(&format!(", {} failed (the task list says why)", self.failed));
        }
        if self.web > 0 {
            text.push_str(&format!(", {} coming from the website", self.web));
        }
        text.push('.');
        let note = if self.failed > 0 { Notification::warning(text) } else { Notification::success(text) };
        let replaced = self.replaced;
        if replaced.is_empty() {
            window.push_notification(note, cx);
            return;
        }
        let undone = Rc::new(std::cell::Cell::new(false));
        let note = note.action(move |_, _, cx| {
            let (store, root, replaced, undone) = (store.clone(), root.clone(), replaced.clone(), undone.clone());
            Button::new("undo-update")
                .label("Undo")
                .disabled(undone.get())
                .on_click(cx.listener(move |note, _, window, cx| {
                    if !undone.replace(true) {
                        undo_update(&store, root.clone(), replaced.clone(), window, cx);
                    }
                    note.dismiss(window, cx);
                }))
        })
        // A snapshot in Backups keeps the undo; the toast shouldn't linger.
        .autohide(true);
        window.push_notification(note, cx);
    }
}

/// Puts the old files of an Update all back. The new files go to backups
/// too, so nothing is lost either way.
fn undo_update(store: &Entity<Store>, root: PathBuf, items: Vec<Replaced>, window: &mut Window, cx: &mut App) {
    let s = store.read(cx);
    if let Some(why) = s.edit_hold_reason(&root) {
        window.push_notification(Notification::error(format!("Can't undo now: {why}.")), cx);
        return;
    }
    let Some(mut inst) = s.instance(&root).cloned() else {
        window.push_notification(Notification::error("That instance is gone."), cx);
        return;
    };
    s.spawn_instance_job(&root, "Undoing update", move |job| {
        let mut back = 0;
        for it in &items {
            if !it.backup.exists() {
                job.note(format!("{}: its old file isn't in backups any more", it.old.name));
                continue;
            }
            if let Some(i) = inst.addons.iter().position(|a| a.project_id == it.old.project_id) {
                let path = inst.addon_path(&inst.addons[i]);
                if path == it.original {
                    job.note(format!("{}: the new file has the old one's name; left it", it.old.name));
                    continue;
                }
                if path.exists() {
                    hopper::sync::backup_into(&inst.root, &path)?;
                }
                inst.addons.remove(i);
            }
            if it.original.exists() {
                job.note(format!("{} is back already; left the backup alone", it.original.display()));
            } else if std::fs::rename(&it.backup, &it.original).is_err() {
                std::fs::copy(&it.backup, &it.original)?;
                std::fs::remove_file(&it.backup)?;
            }
            inst.addons.retain(|a| !a.file_name.eq_ignore_ascii_case(&it.old.file_name));
            inst.addons.push(it.old.clone());
            back += 1;
        }
        inst.save()?;
        job.detail(format!("Put back {back} files"));
        Ok(crate::store::Msg::InstanceChanged(inst.root.clone()))
    });
    // Once it's done, look for updates again so the undone ones are offered.
    let store = store.clone();
    window
        .spawn(cx, async move |cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(300)).await;
                let Ok(busy) = cx.update(|_, cx| store.read(cx).is_busy(&root)) else { return };
                if !busy {
                    break;
                }
            }
            cx.update(|_, cx| {
                store.update(cx, |s, cx| {
                    s.check_updates(&root, true);
                    cx.notify();
                })
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
    /// A resource pack in use: its place in game order.
    rank: Option<usize>,
}

impl Picks for AddonsView {
    fn pick_mut(&mut self) -> Option<&mut DragPick> {
        self.pick.as_mut()
    }
}

impl Render for AddonsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let peek_open = self.peek.read(cx).is_open();
        self.compact = peek_open;
        self.tight = peek_open && self.global_dir().is_some();
        // Built by `sync` when the store changes; here only the filters
        // may need applying again (in memory).
        self.ensure_rows();
        self.fit_columns(window);
        let Some(inst) = self.table.inst.clone() else {
            return div().into_any_element();
        };
        // Data packs with no folder to show: what to do instead.
        if self.class == cf::CLASS_DATAPACKS && self.folder.is_none() {
            return self.no_datapack_folders(cx);
        }
        let tabs = self.location_row(cx);
        let global_head = self.location_caption(cx);
        let missing = self.missing_banner(&inst, cx);
        let dups = self.duplicate_banner(&inst, cx);
        let blockers = self.blocker_banner(&inst, cx);
        let shaders = self.shader_notes(&inst, cx);
        let hunt = self.hunt_strip(&inst, cx);
        let updates = self.table.updates.clone();
        let rows = self.table.rows.clone();
        // Narrow with the instance list open: offer its room, once.
        let hint = self.class == cf::CLASS_MODS
            && self.list_open
            && !self.hint_gone
            && self.room.is_some()
            && window.viewport_size().width < px(1100.);
        let toolbar = self.toolbar(&inst, &updates, hint, cx);
        let bulk = self.bulk_bar(&inst, &updates, cx);
        let header = self.header(&rows, &inst, cx);
        let count = rows.len();
        let body: AnyElement = if count == 0 {
            let filtered = !(self.query.is_empty() && self.show == Show::All && !self.added_only);
            // Only the Updates filter on, and nothing to update.
            let up_to_date = self.show == Show::Updates && self.query.is_empty() && !self.added_only;
            let state = if up_to_date {
                widgets::empty_state(
                    IconName::CircleCheck,
                    "All up to date",
                    Some("Every item here is on its newest version.".into()),
                    cx,
                )
                .child(
                    Button::new("empty-show-all")
                        .small()
                        .outline()
                        .label("Show all")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show = Show::All;
                            cx.notify();
                        })),
                )
            } else if filtered {
                widgets::empty_state(
                    IconName::Search,
                    "Nothing matches",
                    Some("No item here fits the search and filters.".into()),
                    cx,
                )
                .child(
                    Button::new("empty-clear")
                        .small()
                        .outline()
                        .label("Clear search and filters")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.search.update(cx, |s, cx| s.set_value("", window, cx));
                            this.query.clear();
                            this.show = Show::All;
                            this.added_only = false;
                            cx.notify();
                        })),
                )
            } else if let Some(empty) = self.location_empty(cx) {
                empty
            } else {
                widgets::empty_state(
                    IconName::Package,
                    "Nothing installed here yet",
                    Some("Add content finds mods, packs and shaders for this instance.".into()),
                    cx,
                )
            };
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .child(state)
                .into_any_element()
        } else {
            div()
                .id("addon-table")
                .on_drag_move::<PickDrag>(cx.listener(|this, e: &DragMoveEvent<PickDrag>, window, cx| {
                    this.pick_move(e.event.position.y, window, cx)
                }))
                .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.pick_end(cx)))
                .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _, _, cx| this.pick_end(cx)))
                .on_hover(cx.listener(|this, h: &bool, _, _| this.settle.pointer_in = *h))
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
            .children(global_head)
            .children(hunt)
            .child(toolbar)
            .children(blockers)
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
