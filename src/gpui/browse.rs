//! The Browse page: search CurseForge or Modrinth, read a project, and
//! install it into an instance (or a modpack as a new instance).

use crate::store::{Msg, Store, Updates};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::browse::{ServerFiles, dependencies, human_count, installed_as};
use hopper::cf::{self, Loader, SortField};
use hopper::install::Plan;
use hopper::instances::Instance;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

const CLASSES: [(u32, IconName, &str); 6] = [
    (cf::CLASS_MODS, IconName::Puzzle, "Mods"),
    (cf::CLASS_MODPACKS, IconName::Package, "Modpacks"),
    (
        cf::CLASS_RESOURCE_PACKS,
        IconName::Paintbrush,
        "Resource packs",
    ),
    (cf::CLASS_SHADERS, IconName::Sun, "Shaders"),
    (cf::CLASS_DATAPACKS, IconName::Database, "Data packs"),
    (cf::CLASS_WORLDS, IconName::Globe, "Worlds"),
];

/// Something still on its way, or what came back.
/// Everything one author made, opened from a "by …" link.
struct AuthorPanel {
    /// The project the link was on (its site and author).
    of: cf::Mod,
    name: String,
    projects: Load<Vec<cf::Mod>>,
}

/// A plan is for one project in one instance, as its mod list stood.
type PlanKey = (u64, PathBuf, usize);

enum Load<T> {
    Loading,
    Ready(T),
    Failed(String),
}

/// A project's description (`richtext::to_segments`) and its files.
struct Detail {
    segments: Rc<Vec<hopper::richtext::Segment>>,
    /// Every image the description shows, in order (cached by the store).
    images: Rc<Vec<String>>,
    files: Vec<cf::File>,
}

/// How long an ended install keeps its button busy, unless the instance
/// shows the project sooner.
const PENDING_LINGER: Duration = Duration::from_secs(4);

/// How long Browse keeps its place (results, scroll, open project) for a
/// return to the same instance's Add content, after the page was left.
const RESUME_FOR: Duration = Duration::from_secs(15 * 60);

/// (instance root, resource packs rather than data packs, mod count).
type TargetsKey = (PathBuf, bool, usize);

/// An install started from Browse.
struct Pending {
    project: u64,
    /// The instance it goes into; `None` for a modpack (a new instance).
    target: Option<PathBuf>,
    /// Its job, found right after it started.
    job: Option<u64>,
    /// When the job ended.
    ended: Option<Instant>,
}

/// What Browse asks the rest of the app to do.
pub enum BrowseEvent {
    OpenInstance(PathBuf),
    /// Add the CurseForge key.
    OpenSettings,
}

impl EventEmitter<BrowseEvent> for BrowseView {}

/// The sort a fresh Browse starts on: the one set in Settings, else the
/// one picked last.
fn start_sort(st: &hopper::settings::Settings) -> SortField {
    let key = if st.browse_sort.is_empty() { &st.last_sort } else { &st.browse_sort };
    SortField::from_key(key).unwrap_or(SortField::Popularity)
}

pub struct BrowseView {
    store: Entity<Store>,
    /// Redraws a few times a second while installs run, for the Install
    /// buttons' progress (not every frame: see `install_button`).
    install_tick: Option<Task<()>>,
    modrinth: bool,
    class: u32,
    search: Entity<InputState>,
    text: String,
    game_version: String,
    loader: Loader,
    sort: SortField,
    category: Option<u64>,
    results: Vec<cf::Mod>,
    total: u32,
    loading: bool,
    error: Option<String>,
    /// Bumped per search, so a slow answer to an old one is dropped.
    generation: u64,
    /// Bumped per new query (not per page), so a check for new entries
    /// is dropped once the query changed.
    query_gen: u64,
    /// The results list's scroll position, kept while Browse is hidden.
    scroll: ScrollHandle,
    /// When Browse was last left for another page.
    left: Option<Instant>,
    /// Entries that turned up on page one since the list was loaded,
    /// found on a return to it; shown on a click of the "new" chip.
    fresh: Vec<cf::Mod>,
    _check: Option<Task<()>>,
    selected: Option<cf::Mod>,
    detail: HashMap<u64, Load<Detail>>,
    deps: HashMap<u64, Vec<(cf::Mod, u32)>>,
    /// What installing a project into an instance also downloads.
    plans: HashMap<PlanKey, Load<Rc<Plan>>>,
    /// The author whose projects the panel lists, over the project.
    author: Option<AuthorPanel>,
    categories: HashMap<u32, Vec<cf::Category>>,
    versions: Vec<String>,
    /// The instance installs go into.
    target: Option<PathBuf>,
    /// Where data packs (or global resource packs) go, inside the target.
    dest: Option<String>,
    /// Projects already in the target's global pack folders (Paxi…).
    global: HashMap<PathBuf, HashMap<u64, &'static str>>,
    /// Pack folders of the target, for (root, resource packs?, mod count).
    pack_targets: Option<(TargetsKey, Vec<(String, String)>)>,
    /// Installs started here that are still running or just ended.
    pending: Vec<Pending>,
    searched: bool,
    /// Text to put in the search box once a window is at hand.
    pending_query: Option<String>,
    /// The file a curseforge.com Install link asked for.
    link_file: Option<cf::File>,
    /// Description, Files or Dependencies in the project panel.
    detail_tab: usize,
    /// The open project's description, a view of its own so its GIFs
    /// redraw it alone.
    desc: Entity<widgets::DescView>,
    /// The filter and results columns (made on first draw, see [`Part`]).
    parts: Option<(Entity<Part>, Entity<Part>, Entity<Part>)>,
    /// Open the first result when it arrives (`--detail`, for scripted runs).
    open_first: bool,
    /// Only the project panel, docked beside an instance's mod list.
    peek: bool,
    /// Counts [`Self::peek_project`] calls, so a click outside the peek
    /// that opened another mod doesn't also close the panel.
    peeks: u64,
    /// Set by a left press outside the peek: `peeks` at that moment. The
    /// release closes the panel unless that click opened something.
    blur_armed: Option<u64>,
    /// The panel's width while its edge is being dragged, in rem.
    drag_width: Option<f32>,
    /// The list shows the saved bookmarks instead of search results.
    bookmarked: bool,
    /// Modrinth packs' server files, once looked up (`None`: it has none).
    server_files: HashMap<u64, Option<ServerFiles>>,
    _debounce: Option<Task<()>>,
    _search: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl BrowseView {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut view = Self::build(store, window, cx);
        view.open_first = std::env::args().any(|a| a == "--detail");
        view.load_versions(cx);
        view
    }

    /// A Browse that shows only the project panel (see [`Self::peek_project`]).
    pub fn peek(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut view = Self::build(store, window, cx);
        view.peek = true;
        view
    }

    fn build(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let modrinth = store.read(cx).settings().browse_modrinth;
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder(modrinth)));
        let desc = cx.new(|cx| widgets::DescView::new(store.clone(), window, cx));
        let subs = vec![
            cx.observe(&store, |this, _, cx| {
                this.check_pending(cx);
                cx.notify()
            }),
            cx.subscribe_in(&search, window, |this, state, event, _, cx| match event {
                InputEvent::Change => {
                    this.text = state.read(cx).value().trim().to_string();
                    this.search_soon(cx);
                }
                InputEvent::PressEnter { .. } => this.run_search(false, cx),
                _ => {}
            }),
        ];
        let sort = start_sort(store.read(cx).settings());
        Self {
            install_tick: None,
            store,
            modrinth,
            class: cf::CLASS_MODPACKS,
            search,
            text: String::new(),
            game_version: String::new(),
            loader: Loader::Any,
            sort,
            category: None,
            results: Vec::new(),
            total: 0,
            loading: false,
            error: None,
            generation: 0,
            query_gen: 0,
            scroll: ScrollHandle::new(),
            left: None,
            fresh: Vec::new(),
            _check: None,
            selected: None,
            detail: HashMap::new(),
            deps: HashMap::new(),
            plans: HashMap::new(),
            author: None,
            categories: HashMap::new(),
            versions: Vec::new(),
            target: None,
            dest: None,
            global: HashMap::new(),
            pack_targets: None,
            pending: Vec::new(),
            searched: false,
            pending_query: None,
            link_file: None,
            open_first: false,
            peek: false,
            peeks: 0,
            blur_armed: None,
            drag_width: None,
            bookmarked: false,
            server_files: HashMap::new(),
            detail_tab: 0,
            desc,
            parts: None,
            _debounce: None,
            _search: None,
            _subscriptions: subs,
        }
    }

    /// The page became visible: search once, the first time. Installs go
    /// into `fallback` (the instance picked in the Library) until another
    /// is chosen.
    /// Searches Modrinth (or CurseForge), and remembers it.
    pub fn set_source(&mut self, modrinth: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.modrinth == modrinth {
            return;
        }
        self.modrinth = modrinth;
        self.category = None;
        self.search
            .update(cx, |s, cx| s.set_placeholder(placeholder(modrinth), window, cx));
        self.store
            .update(cx, |s, cx| s.update_settings(|st| st.browse_modrinth = modrinth, cx));
        self.refilter(cx);
    }

    pub fn shown(&mut self, fallback: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.target.is_none()
            && let Some(root) = fallback
        {
            if self.class == cf::CLASS_MODPACKS {
                // Don't narrow modpacks to the instance's version; its
                // filters apply once another type is picked.
                self.target = Some(root);
            } else {
                self.set_target(root, cx);
            }
            self.want_global(cx);
            self.searched = false;
        }
        if !self.searched {
            self.run_search(false, cx);
        }
    }

    /// Opens Browse for adding `class` content to an instance; data packs
    /// go to `dest` when given.
    pub fn browse_for(
        &mut self,
        root: PathBuf,
        class: u32,
        dest: Option<String>,
        cx: &mut Context<Self>,
    ) {
        // Back to the same instance's Add content soon after leaving:
        // carry on where the list was, and look for anything new.
        if self.class == class && self.dest == dest && self.can_resume(Some(&root)) {
            self.check_new(cx);
            return;
        }
        self.class = class;
        self.category = None;
        if class == cf::CLASS_WORLDS {
            // Modrinth has no worlds.
            self.modrinth = false;
        }
        self.set_target(root, cx);
        self.dest = dest;
        self.want_global(cx);
        self.run_search(false, cx);
    }

    /// Whether Browse still holds a list for `root` (the instance picked in
    /// the Library) from a visit that ended less than [`RESUME_FOR`] ago.
    fn can_resume(&self, root: Option<&Path>) -> bool {
        !self.bookmarked
            && self.error.is_none()
            && !self.results.is_empty()
            && self.target.as_deref() == root
            && self.left.is_some_and(|t| t.elapsed() < RESUME_FOR)
    }

    /// Back to Browse from the nav: where it was left when that was soon
    /// enough and for the same instance (checking for new entries), else
    /// a fresh Browse.
    pub fn resume_or_reset(&mut self, root: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if self.can_resume(root.as_deref()) {
            self.check_new(cx);
            return;
        }
        if self.target != root {
            // Installs go into the instance picked now (see `shown`).
            self.target = None;
        }
        self.reset(window, cx);
    }

    /// Another page took Browse's place.
    pub fn hidden(&mut self) {
        self.left = Some(Instant::now());
    }

    /// Asks for page one of the current query again and keeps what the
    /// list doesn't have yet: added at once when the list is at its top,
    /// else behind the "new" chip so the list doesn't move.
    fn check_new(&mut self, cx: &mut Context<Self>) {
        let query_gen = self.query_gen;
        let s = self.search_params();
        let modrinth = self.modrinth;
        self._check = Some(cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move { hopper::source::search(&s, modrinth) })
                .await;
            this.update(cx, |this, cx| {
                let Ok((mods, page)) = r else { return };
                if this.query_gen != query_gen || this.bookmarked {
                    return;
                }                let new: Vec<cf::Mod> = mods
                    .into_iter()
                    .filter(|m| !this.results.iter().any(|r| r.id == m.id))
                    .collect();
                this.total = page.total_count;
                if new.is_empty() {
                    return;
                }
                let urls: Vec<String> = new
                    .iter()
                    .filter_map(|m| m.icon().map(str::to_string))
                    .collect();
                this.store
                    .update(cx, |s, _| urls.iter().for_each(|u| s.want_image(u)));
                this.fresh = new;
                if this.scroll.offset().y >= px(-1.) {
                    this.show_fresh();
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Puts the new entries at the top of the list and scrolls there.
    fn show_fresh(&mut self) {
        let fresh = std::mem::take(&mut self.fresh);
        self.results.retain(|r| !fresh.iter().any(|m| m.id == r.id));
        self.results.splice(0..0, fresh);
        self.scroll.set_offset(point(px(0.), px(0.)));
    }

    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.focus(window, cx));
    }

    /// Opens the project a curseforge:// Install link points at, ready to
    /// install that file.
    pub fn open_link(&mut self, url: &str, cx: &mut Context<Self>) {
        let Some(link) = hopper::links::parse(url) else {
            self.error = Some(format!("Not an install link: {url}"));
            cx.notify();
            return;
        };
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move {
                    let m = cf::get_mod(link.project_id)?;
                    let f = match link.file_id {
                        Some(id) => Some(cf::file(link.project_id, id)?),
                        None => None,
                    };
                    anyhow::Ok((m, f))
                })
                .await;
            this.update(cx, |this, cx| match r {
                Ok((m, f)) => {
                    this.modrinth = false;
                    this.class = m.class_id.unwrap_or(cf::CLASS_MODS);
                    this.link_file = f;
                    this.open_detail(m, cx);
                    this.run_search(false, cx);
                }
                Err(e) => {
                    this.error = Some(format!("Couldn't open that link: {e:#}"));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Opens a project's page, installing into `root`.
    pub fn open_project(&mut self, root: PathBuf, project: u64, cx: &mut Context<Self>) {
        self.set_target(root, cx);
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move { hopper::source::get_mod(project) })
                .await;
            this.update(cx, |this, cx| match r {
                Ok(m) => {
                    this.modrinth = m.modrinth.is_some();
                    this.class = m.class_id.unwrap_or(cf::CLASS_MODS);
                    this.open_detail(m, cx);
                    if !this.peek {
                        this.run_search(false, cx);
                    }
                }
                Err(e) => {
                    this.error = Some(format!("{e:#}"));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Shows only `category` of `class`, with no search: a click on a
    /// project's category.
    fn filter_category(&mut self, class: u32, category: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.class = class;
        self.category = Some(category);
        self.text.clear();
        self.search.update(cx, |s, cx| s.set_value("", window, cx));
        self.refilter(cx);
    }

    /// Category links for `m`'s stats; none in a panel beside the Library,
    /// which has no filters.
    fn on_category(&self, m: &cf::Mod, cx: &Context<Self>) -> Option<OnCategory> {
        if self.peek {
            return None;
        }
        let (view, class) = (cx.entity(), m.class_id.unwrap_or(self.class));
        Some(Rc::new(move |id, window, cx| {
            view.update(cx, |this, cx| this.filter_category(class, id, window, cx))
        }))
    }

    /// The project whose panel is open.
    pub fn selected_id(&self) -> Option<u64> {
        self.selected.as_ref().map(|m| m.id)
    }

    /// Where Browse is, for back and forward: the type shown and the
    /// project open.
    pub(crate) fn nav_loc(&self) -> (u32, Option<&cf::Mod>) {
        (self.class, self.selected.as_ref())
    }

    /// Goes back (or forward) to a place `nav_loc` gave.
    pub(crate) fn nav_restore(&mut self, class: u32, project: Option<cf::Mod>, cx: &mut Context<Self>) {
        if class != self.class {
            self.browse_class(class, cx);
        }
        match project {
            Some(m) if self.selected_id() != Some(m.id) => self.open_detail(m, cx),
            Some(_) => {}
            None => {
                self.selected = None;
                self.author = None;
            }
        }
        cx.notify();
    }

    /// Whether the project panel is open.
    pub fn is_open(&self) -> bool {
        self.selected.is_some() || self.author.is_some() || (self.peek && self.error.is_some())
    }

    /// The project panel of `project` (`None` closes it), for an instance
    /// installed at `root`. Clicking the open one again closes it.
    pub fn peek_project(&mut self, root: PathBuf, project: Option<u64>, cx: &mut Context<Self>) {
        self.error = None;
        self.peeks += 1;
        match project {
            // The open panel stays up until the new project arrives.
            Some(p) if self.selected.as_ref().is_none_or(|m| m.id != p) => {
                self.open_project(root, p, cx)
            }
            _ => self.selected = None,
        }
        cx.notify();
    }

    /// Searches for `text` (the box shows it on the next frame).
    pub fn find(&mut self, text: String, cx: &mut Context<Self>) {
        self.text = text.clone();
        self.pending_query = Some(text);
        self.run_search(false, cx);
    }

    /// Puts `q` in the search box.
    pub fn set_query(&mut self, q: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.text = q.trim().to_string();
        let q = q.to_string();
        self.search.update(cx, |s, cx| s.set_value(q, window, cx));
        self.run_search(false, cx);
    }

    /// Back to a fresh Browse (the nav's Browse button): Modpacks, no
    /// search, every filter cleared.
    pub fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.class = cf::CLASS_MODPACKS;
        self.category = None;
        self.game_version.clear();
        self.loader = Loader::Any;
        self.sort = start_sort(self.store.read(cx).settings());
        self.dest = None;
        self.selected = None;
        self.link_file = None;
        self.text.clear();
        self.search.update(cx, |s, cx| s.set_value("", window, cx));
        self.refilter(cx);
    }

    /// Opens Browse on a class with no instance in mind.
    pub fn browse_class(&mut self, class: u32, cx: &mut Context<Self>) {
        self.class = class;
        self.category = None;
        self.run_search(false, cx);
    }

    fn set_target(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let inst = self.store.read(cx).instance(&root).cloned();
        if let Some(inst) = inst {
            self.game_version = inst.game_version.clone();
            self.loader = hopper::install::loader_of(&inst);
        }
        self.target = Some(root);
        self.dest = None;
    }

    fn target(&self, cx: &App) -> Option<Instance> {
        self.target
            .as_ref()
            .and_then(|t| self.store.read(cx).instance(t))
            .cloned()
    }

    fn category_key(&self) -> u32 {
        self.class | if self.modrinth { 1 << 31 } else { 0 }
    }

    // ---- loading -------------------------------------------------------

    fn load_versions(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move { cf::minecraft_versions() })
                .await;
            this.update(cx, |this, cx| {
                match r {
                    Ok(v) => this.versions = v,
                    Err(e) => log::warn!("Minecraft versions: {e:#}"),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn load_categories(&mut self, cx: &mut Context<Self>) {
        let key = self.category_key();
        if self.categories.contains_key(&key) {
            return;
        }
        self.categories.insert(key, Vec::new());
        let (class, modrinth) = (self.class, self.modrinth);
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move {
                    if modrinth {
                        hopper::modrinth::categories(class)
                    } else {
                        cf::categories(class)
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                if let Ok(c) = r {
                    let urls: Vec<String> = c.iter().map(|c| c.icon_url.clone()).collect();
                    this.categories.insert(key, c);
                    this.store
                        .update(cx, |s, _| urls.iter().for_each(|u| s.want_image(u)));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Searches once typing pauses.
    fn search_soon(&mut self, cx: &mut Context<Self>) {
        self._debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(350))
                .await;
            this.update(cx, |this, cx| this.run_search(false, cx)).ok();
        }));
    }

    fn run_search(&mut self, append: bool, cx: &mut Context<Self>) {
        self.searched = true;
        if self.bookmarked {
            // The saved list filters as you type; nothing to ask for.
            cx.notify();
            return;
        }
        self.load_categories(cx);
        if !self.modrinth && cf::api_key().is_none() {
            self.error = Some(
                "Searching CurseForge needs an API key: paste yours in Settings → CurseForge & Modrinth (it works right away), or switch to Modrinth."
                    .into(),
            );
            cx.notify();
            return;
        }
        self.generation += 1;
        self.loading = true;
        let generation = self.generation;
        if !append {
            // A new query starts at the top, with nothing "new" pending.
            self.query_gen += 1;
            self.fresh.clear();
            self._check = None;
            self.scroll.set_offset(point(px(0.), px(0.)));
        }
        let s = cf::Search {
            index: if append { self.results.len() as u32 } else { 0 },
            ..self.search_params()
        };
        let modrinth = self.modrinth;
        self._search = Some(cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move { hopper::source::search(&s, modrinth) })
                .await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match r {
                    Ok((mods, page)) => {
                        let urls: Vec<String> = mods
                            .iter()
                            .filter_map(|m| m.icon().map(str::to_string))
                            .collect();
                        if append {
                            // Skip any the "new" check already put on top.
                            let mods: Vec<cf::Mod> = mods
                                .into_iter()
                                .filter(|m| !this.results.iter().any(|r| r.id == m.id))
                                .collect();
                            this.results.extend(mods);
                        } else {
                            this.results = mods;
                        }
                        this.total = page.total_count;
                        this.error = None;
                        if std::mem::take(&mut this.open_first)
                            && let Some(m) = this.results.first().cloned()
                        {
                            this.open_detail(m, cx);
                        }
                        this.store
                            .update(cx, |s, _| urls.iter().for_each(|u| s.want_image(u)));
                        this.want_server_files(cx);
                    }
                    Err(e) => this.error = Some(format!("{e:#}")),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// The search the filters describe, from its first result.
    fn search_params(&self) -> cf::Search {
        cf::Search {
            class_id: self.class,
            text: self.text.clone(),
            game_version: self.game_version.clone(),
            loader: if matches!(self.class, cf::CLASS_MODS | cf::CLASS_MODPACKS) {
                self.loader
            } else {
                Loader::Any
            },
            category: self.category,
            sort: self.sort,
            index: 0,
        }
    }

    /// Looks up which Modrinth packs in the results have server files, in
    /// one request per page.
    fn want_server_files(&mut self, cx: &mut Context<Self>) {
        let versions: Vec<(u64, u64)> = self
            .results
            .iter()
            .filter(|m| {
                m.modrinth.is_some()
                    && m.class_id == Some(cf::CLASS_MODPACKS)
                    && m.main_file_id != 0
                    && !self.server_files.contains_key(&m.id)
            })
            .map(|m| (m.id, m.main_file_id))
            .collect();
        if versions.is_empty() {
            return;
        }
        // Asked for; a failed lookup shows no badge rather than asking again.
        for (p, _) in &versions {
            self.server_files.insert(*p, None);
        }
        cx.spawn(async move |this, cx| {
            let ids: Vec<u64> = versions.iter().map(|(_, v)| *v).collect();
            let r = cx
                .background_spawn(async move { hopper::browse::modrinth_server_files_for(&ids) })
                .await;
            this.update(cx, |this, cx| match r {
                Ok(found) => {
                    this.server_files.extend(found);
                    cx.notify();
                }
                Err(e) => log::warn!("Modrinth server files: {e:#}"),
            })
            .ok();
        })
        .detach();
    }

    /// The server files of a pack, as far as known: CurseForge's from the
    /// project's files, Modrinth's once looked up.
    fn server_files_of(&self, m: &cf::Mod) -> Option<ServerFiles> {
        if m.modrinth.is_some() {
            return self.server_files.get(&m.id).cloned().flatten();
        }
        if m.class_id.is_some_and(|c| c != cf::CLASS_MODPACKS) {
            return None;
        }
        hopper::browse::curse_server_files(m).or_else(|| match self.detail.get(&m.id) {
            Some(Load::Ready(d)) => {
                hopper::browse::curse_server_files_in(m.id, m.main_file_id, &d.files)
            }
            _ => None,
        })
    }

    /// The "Server files" badge: a menu to download the pack's server zip
    /// or host a server from it on this PC.
    fn server_badge(&self, m: &cf::Mod, id: ElementId, long: bool, cx: &Context<Self>) -> Option<AnyElement> {
        if !hopper::browse::is_modpack(m, self.class) {
            return None;
        }
        let Some(files) = self.server_files_of(m) else {
            return self.build_button(m, id, long, cx);
        };
        let (store, name, pack) = (self.store.clone(), m.name.clone(), m.clone());
        Some(
            Button::new(id)
                .xsmall()
                .outline()
                .icon(IconName::Server)
                .label("Server files")
                .tooltip("The pack author's server files")
                .dropdown_menu(move |menu, _, _| {
                    let (s1, f1, n1) = (store.clone(), files.clone(), name.clone());
                    let (s2, f2, n2) = (store.clone(), files.clone(), name.clone());
                    let (s3, p3) = (store.clone(), pack.clone());
                    menu.item(
                        PopupMenuItem::new("Download the server pack…")
                            .icon(IconName::Download)
                            .on_click(move |_, _, cx| {
                                crate::dialogs::save_server_files(s1.clone(), f1.clone(), n1.clone(), cx)
                            }),
                    )
                    .item(
                        PopupMenuItem::new("Host a server on this PC…")
                            .icon(IconName::Server)
                            .on_click(move |_, window, cx| {
                                crate::host_dialog::open_for_files(s2.clone(), f2.clone(), n2.clone(), window, cx)
                            }),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new("Build a server from this pack instead…")
                            .icon(IconName::ServerCog)
                            .on_click(move |_, window, cx| {
                                crate::host_dialog::open_for_client_pack(s3.clone(), p3.clone(), window, cx)
                            }),
                    )
                })
                .into_any_element(),
        )
    }

    /// For a pack with no server files (as far as known): build a server
    /// from its client pack, leaving client-only mods out.
    fn build_button(&self, m: &cf::Mod, id: ElementId, long: bool, _: &Context<Self>) -> Option<AnyElement> {
        // A Modrinth pack not looked up yet may still have server files.
        if m.modrinth.is_some() && !self.server_files.contains_key(&m.id) {
            return None;
        }
        let (store, pack) = (self.store.clone(), m.clone());
        Some(
            Button::new(id)
                .xsmall()
                .ghost()
                .icon(IconName::ServerCog)
                .label(if long { "Build a server from this pack" } else { "Build a server" })
                .tooltip("No official server files: build a server on this PC from this pack, leaving client-only mods out")
                .on_click(move |_, window, cx| {
                    // Not a click on the card: the page stays as it is.
                    cx.stop_propagation();
                    crate::host_dialog::open_for_client_pack(store.clone(), pack.clone(), window, cx)
                })
                .into_any_element(),
        )
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        self.results.clear();
        self.run_search(false, cx);
    }

    fn want_global(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.class, cf::CLASS_RESOURCE_PACKS | cf::CLASS_DATAPACKS) {
            return;
        }
        let Some(inst) = self.target(cx) else { return };
        if self.global.contains_key(&inst.root) {
            return;
        }
        self.global.insert(inst.root.clone(), HashMap::new());
        cx.spawn(async move |this, cx| {
            let root = inst.root.clone();
            let ids = cx
                .background_spawn(async move { hopper::instance_ops::global_pack_projects(&inst) })
                .await;
            this.update(cx, |this, cx| {
                this.global.insert(root, ids);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn open_detail(&mut self, m: cf::Mod, cx: &mut Context<Self>) {
        let id = m.id;
        // Files for the current Minecraft and loader filters (they follow the
        // target instance); the newest 50 overall are mostly other loaders.
        let gv = self.game_version.clone();
        let loader = if m.class_id.unwrap_or(cf::CLASS_MODS) == cf::CLASS_MODS {
            self.loader
        } else {
            Loader::Any
        };
        let urls: Vec<String> = m
            .screenshots
            .iter()
            .map(|s| s.thumbnail_url.clone())
            .chain(m.icon().map(str::to_string))
            .collect();
        self.store
            .update(cx, |s, _| urls.iter().for_each(|u| s.want_image(u)));
        self.want_plan(&m, cx);
        if m.modrinth.is_some()
            && m.class_id == Some(cf::CLASS_MODPACKS)
            && !matches!(self.server_files.get(&id), Some(Some(_)))
        {
            // A bookmark (or a hit not looked up yet): ask its versions.
            self.server_files.insert(id, None);
            cx.spawn(async move |this, cx| {
                let r = cx
                    .background_spawn(async move { hopper::browse::modrinth_latest_server_files(id) })
                    .await;
                this.update(cx, |this, cx| {
                    if let Ok(found) = r {
                        this.server_files.insert(id, found);
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
        self.author = None;
        self.selected = Some(m);
        self.detail_tab = 0;
        self.detail.insert(id, Load::Loading);
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move {
                    let desc = hopper::source::description(id)?;
                    let files = hopper::source::files(id, &gv, loader)?;
                    // Dependencies of the newest release that fits, like
                    // CurseForge's Relations tab.
                    let deps = files
                        .iter()
                        .find(|f| f.release_type == 1)
                        .or(files.first())
                        .map(dependencies)
                        .unwrap_or_default();
                    let images = hopper::richtext::images(&desc);
                    anyhow::Ok((hopper::richtext::to_segments(&desc), images, files, deps))
                })
                .await;
            this.update(cx, |this, cx| {
                match r {
                    Ok((segments, images, files, deps)) => {
                        let urls: Vec<String> = deps
                            .iter()
                            .filter_map(|(d, _)| d.icon().map(str::to_string))
                            .collect();
                        // The description's GIFs play (see `anim_tick`).
                        this.store.update(cx, |s, _| {
                            urls.iter().for_each(|u| s.want_image(u));
                            images.iter().for_each(|u| s.want_anim(u));
                        });
                        this.deps.insert(id, deps);
                        this.detail.insert(
                            id,
                            Load::Ready(Detail {
                                segments: Rc::new(segments),
                                images: Rc::new(images),
                                files,
                            }),
                        );
                    }
                    Err(e) => {
                        this.detail.insert(id, Load::Failed(format!("{e:#}")));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Lists everything by the author of `m` in the panel.
    fn open_author(&mut self, m: cf::Mod, cx: &mut Context<Self>) {
        let name = m.author().to_string();
        if name.is_empty() {
            return;
        }
        self.author = Some(AuthorPanel {
            of: m.clone(),
            name: name.clone(),
            projects: Load::Loading,
        });
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move { hopper::source::author_projects(&m) })
                .await;
            this.update(cx, |this, cx| {
                let Some(a) = this.author.as_mut().filter(|a| a.name == name) else { return };
                a.projects = match r {
                    Ok(v) => {
                        let urls: Vec<String> =
                            v.iter().filter_map(|m| m.icon().map(str::to_string)).collect();
                        this.store
                            .update(cx, |s, _| urls.iter().for_each(|u| s.want_image(u)));
                        Load::Ready(v)
                    }
                    Err(e) => Load::Failed(format!("{e:#}")),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// "by …", opening the author's projects.
    fn author_link(&self, m: &cf::Mod, id: ElementId, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        if m.author().is_empty() {
            return div().into_any_element();
        }
        let (m1, hover) = (m.clone(), theme.primary);
        div()
            .id(id)
            .flex_none()
            .text_sm()
            .text_color(theme.muted_foreground)
            .cursor_pointer()
            .hover(move |s| s.text_color(hover).underline())
            .child(format!("by {}", m.author()))
            .on_click(cx.listener(move |this, _, _, cx| {
                // Not a click on the card under it.
                cx.stop_propagation();
                this.open_author(m1.clone(), cx);
            }))
            .into_any_element()
    }

    /// The plan key for installing `m` into `target`, when the install
    /// resolves dependencies (not modpacks, worlds or data packs).
    fn plan_key(&self, m: &cf::Mod, target: &Instance) -> Option<PlanKey> {
        let class = m.class_id.unwrap_or(self.class);
        let plain = !hopper::browse::is_modpack(m, self.class)
            && !matches!(class, cf::CLASS_WORLDS | cf::CLASS_DATAPACKS);
        plain.then(|| (m.id, target.root.clone(), target.addons.len()))
    }

    fn plan_for(&self, m: &cf::Mod, target: Option<&Instance>) -> Option<&Load<Rc<Plan>>> {
        self.plans.get(&self.plan_key(m, target?)?)
    }

    /// Works out what installing `m` into the target would add, once.
    fn want_plan(&mut self, m: &cf::Mod, cx: &mut Context<Self>) {
        let Some(inst) = self.target(cx) else { return };
        let Some(key) = self.plan_key(m, &inst) else { return };
        if self.plans.contains_key(&key) || installed_as(&inst, m).is_some() {
            return;
        }
        self.plans.insert(key.clone(), Load::Loading);
        let (m, class) = (m.clone(), Some(m.class_id.unwrap_or(self.class)));
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move {
                    let f = hopper::install::best_file(m.id, class, &inst)?
                        .ok_or_else(|| anyhow::anyhow!("no file for this version"))?;
                    hopper::install::plan(&inst, &m, &f, None)
                })
                .await;
            this.update(cx, |this, cx| {
                let load = match r {
                    Ok(p) => {
                        let urls: Vec<String> = p
                            .extra()
                            .iter()
                            .filter_map(|(d, _)| d.icon().map(str::to_string))
                            .collect();
                        this.store
                            .update(cx, |s, _| urls.iter().for_each(|u| s.want_image(u)));
                        Load::Ready(Rc::new(p))
                    }
                    Err(e) => Load::Failed(format!("{e:#}")),
                };
                this.plans.insert(key, load);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn install(
        &mut self,
        m: cf::Mod,
        file: Option<cf::File>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let browsing = m.class_id.unwrap_or(self.class);
        let pack = hopper::browse::is_modpack(&m, browsing);
        let (pending, hold, _) = self.install_state(m.id, pack, cx);
        if pending || hold.is_some() {
            return;
        }
        let target = self.target(cx);
        if !pack && target.is_none() {
            window.push_notification(
                Notification::warning("Pick an instance to install into."),
                cx,
            );
            return;
        }
        let dest = target
            .as_ref()
            .and_then(|t| self.dest_for_class(t, browsing, cx).map(|d| t.root.join(d)));
        let title = match &target {
            Some(t) if !pack => format!("Installing {} into {}", m.name, t.name),
            _ => format!("Installing {}", m.name),
        };
        let root = PathBuf::from(&self.store.read(cx).settings().instances_root);
        // Into an instance: it counts as busy until done, so nothing else
        // changes it meanwhile. A modpack makes a new instance.
        let into = target.as_ref().filter(|_| !pack).map(|t| t.root.clone());
        let project = m.id;
        let work = move |job: &hopper::jobs::Job| {
            if pack {
                job.announce();
            }
            let place = hopper::browse::Placement {
                instances_root: &root,
                target,
                dest,
            };
            // Website-only files (the project or a dependency) land on the
            // instance's pending list; the download window gets their pages.
            let pending_urls = |i: &Option<Instance>| -> Vec<String> {
                i.as_ref()
                    .map(|i| hopper::modpack::pending_downloads(i).into_iter().map(|p| p.url).collect())
                    .unwrap_or_default()
            };
            let before = pending_urls(&place.target);
            let after_inst = place.target.clone();
            let (changed, line) = hopper::browse::install_project(&m, file, browsing, place, job)?;
            job.detail(line);
            let pages: Vec<String> = pending_urls(&after_inst)
                .into_iter()
                .filter(|u| !before.contains(u))
                .collect();
            if pages.is_empty() {
                Ok(Msg::InstanceChanged(changed))
            } else {
                Ok(Msg::Updated(changed, Vec::new(), pages))
            }
        };
        let store = self.store.read(cx);
        match &into {
            Some(r) => store.spawn_instance_job(r, title.clone(), work),
            None => store.spawn_job(title.clone(), work),
        }
        // The job just started: the newest unfinished one with this title.
        let job = store
            .jobs()
            .snapshot()
            .iter()
            .filter(|j| j.title == title && !j.done)
            .map(|j| j.id)
            .max();
        self.pending.push(Pending {
            project,
            target: into,
            job,
            ended: None,
        });
        if self.install_tick.is_none() {
            self.install_tick = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
                    let more = this
                        .update(cx, |this, cx| {
                            let more = this.pending.iter().any(|p| p.ended.is_none());
                            if more {
                                cx.notify();
                            } else {
                                this.install_tick = None;
                            }
                            more
                        })
                        .unwrap_or(false);
                    if !more {
                        break;
                    }
                }
            }));
        }
        cx.notify();
    }

    /// Whether an install of `project` into the current target (or as a new
    /// instance, for a modpack) is under way, and why one can't start now.
    /// (installing now, why it can't, how far the install is).
    fn install_state(&self, project: u64, pack: bool, cx: &App) -> (bool, Option<&'static str>, Option<f32>) {
        let into = if pack { None } else { self.target.as_ref() };
        let p = self
            .pending
            .iter()
            .find(|p| p.project == project && p.target.as_ref() == into && p.ended.is_none());
        let pending = self
            .pending
            .iter()
            .any(|p| p.project == project && p.target.as_ref() == into);
        let progress = p
            .and_then(|p| p.job)
            .and_then(|j| self.store.read(cx).job_state(j))
            .filter(|j| !j.done)
            .and_then(|j| j.progress);
        let hold = into.and_then(|r| self.store.read(cx).edit_hold_reason(r));
        (pending, hold, progress)
    }

    /// Lets go of installs that ended: at once when they failed or the
    /// instance shows the project, else a few seconds after.
    fn check_pending(&mut self, cx: &mut Context<Self>) {
        if self.pending.is_empty() {
            return;
        }
        let store = self.store.read(cx);
        let jobs = store.jobs().snapshot();
        let now = Instant::now();
        let mut ended_now = false;
        let mut failed = Vec::new();
        for (i, p) in self.pending.iter_mut().enumerate() {
            if p.ended.is_some() {
                continue;
            }
            let state = p.job.and_then(|id| jobs.iter().find(|j| j.id == id));
            if state.is_none_or(|j| j.done) {
                p.ended = Some(now);
                ended_now = true;
                if state.is_some_and(|j| j.error.is_some()) {
                    failed.push(i);
                }
            }
        }
        let installed = |p: &Pending| match &p.target {
            Some(r) => store
                .instance(r)
                .is_some_and(|i| i.addons.iter().any(|a| a.project_id == p.project)),
            None => store
                .instances()
                .iter()
                .any(|i| i.pack.as_ref().is_some_and(|k| k.project_id == p.project)),
        };
        let mut i = 0;
        self.pending.retain(|p| {
            let keep = match p.ended {
                None => true,
                Some(t) => !failed.contains(&i) && now - t < PENDING_LINGER && !installed(p),
            };
            i += 1;
            keep
        });
        if ended_now {
            // Look again once the linger is over, even if nothing else
            // redraws the page.
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(PENDING_LINGER).await;
                this.update(cx, |this, cx| {
                    this.check_pending(cx);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    /// The data pack / resource pack folder for `inst`: the saved choice,
    /// else the first suggestion.
    fn dest_for(&mut self, inst: &Instance, cx: &App) -> Option<String> {
        self.dest_for_class(inst, self.class, cx)
    }

    /// [`Self::dest_for`] for a project of `class`.
    fn dest_for_class(&mut self, inst: &Instance, class: u32, cx: &App) -> Option<String> {
        let rp = match class {
            cf::CLASS_RESOURCE_PACKS => true,
            cf::CLASS_DATAPACKS => false,
            _ => return None,
        };
        if let Some(d) = self.dest.as_ref().filter(|_| class == self.class) {
            return Some(d.clone());
        }
        let key = format!("{}{}", if rp { "rp:" } else { "" }, inst.root.display());
        let saved = self
            .store
            .read(cx)
            .settings()
            .datapack_dest
            .get(&key)
            .cloned();
        saved.or_else(|| self.pack_targets(inst, rp).first().map(|(_, d)| d.clone()))
    }

    /// Where resource packs (`rp`) or data packs can go in `inst`. Reads
    /// the mods folder, so it is kept until the instance or class changes.
    fn pack_targets(&mut self, inst: &Instance, rp: bool) -> Vec<(String, String)> {
        let key = (inst.root.clone(), rp, inst.addons.len());
        if let Some((k, t)) = &self.pack_targets
            && *k == key
        {
            return t.clone();
        }
        let mut t = if rp {
            hopper::instance_ops::resourcepack_targets(inst)
        } else {
            hopper::instance_ops::datapack_targets(inst)
        };
        t.retain(|(_, d)| !d.split('/').any(|part| part.starts_with(".hopper-")));
        self.pack_targets = Some((key, t.clone()));
        t
    }

    // ---- filters -------------------------------------------------------

    /// CurseForge | Modrinth, the picked one in its site's color.
    fn source_switch(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let (muted, fg, hover, track, radius) =
            (theme.muted_foreground, theme.foreground, theme.list_hover, theme.muted, theme.radius);
        let side = |modrinth: bool, label: &'static str| {
            let on = self.modrinth == modrinth;
            let color = site_color(modrinth);
            h_flex()
                .id(if modrinth { "source-mr" } else { "source-cf" })
                .flex_1()
                .h_8()
                .justify_center()
                .gap_1p5()
                .rounded(radius)
                .text_sm()
                .cursor_pointer()
                .child(crate::brand::site_icon(modrinth).small().text_color(if on { gpui_kit::white() } else { color }))
                .child(label)
                .map(|d| {
                    if on {
                        d.bg(color).text_color(gpui_kit::white()).font_semibold()
                    } else {
                        d.text_color(muted).hover(move |s| s.bg(hover).text_color(fg))
                    }
                })
                .on_click(cx.listener(move |this, _, window, cx| this.set_source(modrinth, window, cx)))
        };
        h_flex()
            .p_0p5()
            .gap_0p5()
            .rounded(radius * 1.5)
            .bg(track)
            .child(side(false, "CurseForge"))
            .child(side(true, "Modrinth"))
    }

    fn section(title: &'static str, cx: &App) -> Div {
        v_flex().gap_1p5().child(
            div()
                .text_xs()
                .font_semibold()
                .text_color(cx.theme().muted_foreground)
                .child(title),
        )
    }

    fn filters(
        &mut self,
        target: Option<&Instance>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let rem = window.rem_size();
        // Each instance with its icon, for the Install into list.
        let instances: Vec<(PathBuf, String, Option<PathBuf>)> = {
            let store = self.store.read(cx);
            store
                .instances()
                .iter()
                .map(|i| {
                    let icon = i.icon.as_deref().and_then(|u| store.image(u)).map(Path::to_path_buf);
                    (i.root.clone(), i.name.clone(), icon)
                })
                .collect()
        };
        let classes = v_flex()
            .gap_0p5()
            .children(CLASSES.iter().map(|(id, icon, name)| {
                let (id, active) = (*id, self.class == *id && !self.bookmarked);
                h_flex()
                    .id(ElementId::Name(format!("class:{name}").into()))
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .rounded(theme.radius)
                    .text_sm()
                    .cursor_pointer()
                    .when(active, |r| r.bg(theme.list_active).font_medium())
                    .when(!active, |r| r.hover(|s| s.bg(theme.list_hover)))
                    .child(Icon::new(*icon).small())
                    .child(*name)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if std::mem::take(&mut this.bookmarked) && this.class == id {
                            this.refilter(cx);
                        } else if this.class != id {
                            if this.class == cf::CLASS_MODPACKS
                                && this.game_version.is_empty()
                                && this.loader == Loader::Any
                                && let Some(root) = this.target.clone()
                            {
                                this.set_target(root, cx);
                            }
                            this.class = id;
                            this.category = None;
                            this.dest = None;
                            this.want_global(cx);
                            this.refilter(cx);
                        }
                    }))
            }))
            .child({
                let n = hopper::bookmarks::with(|b| b.list().len());
                let active = self.bookmarked;
                h_flex()
                    .id("class:bookmarked")
                    .mt_1()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .rounded(theme.radius)
                    .text_sm()
                    .cursor_pointer()
                    .when(active, |r| r.bg(theme.list_active).font_medium())
                    .when(!active, |r| r.hover(|s| s.bg(theme.list_hover)))
                    .child(Icon::new(IconName::Bookmark).small())
                    .child(div().flex_1().child("Bookmarked"))
                    .when(n > 0, |r| {
                        r.child(div().text_xs().text_color(theme.muted_foreground).child(n.to_string()))
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.show_bookmarks(cx)))
            });

        let view = cx.entity();
        let pick_target = {
            let view = view.clone();
            Rc::new(move |ix: usize, _: &mut Window, cx: &mut App| {
                let root = instances_root_at(&view, ix, cx);
                view.update(cx, |this, cx| {
                    if let Some(root) = root {
                        this.set_target(root, cx);
                        this.want_global(cx);
                        this.refilter(cx);
                    }
                });
            })
        };
        let target_items: Vec<(String, bool, Option<widgets::PickIcon>)> = instances
            .iter()
            .map(|(r, n, icon)| (n.clone(), self.target.as_ref() == Some(r), icon.clone().map(widgets::PickIcon::Image)))
            .collect();

        let dest_picker = target.and_then(|inst| {
            let rp = match self.class {
                cf::CLASS_RESOURCE_PACKS => true,
                cf::CLASS_DATAPACKS => false,
                _ => return None,
            };
            let mut targets = self.pack_targets(inst, rp);
            let current = self.dest_for(inst, cx);
            if let Some(c) = &current
                && !targets.iter().any(|(_, d)| d == c)
            {
                targets.push((format!("Folder: {c}"), c.clone()));
            }
            let label = current
                .as_ref()
                .and_then(|c| targets.iter().find(|(_, d)| d == c).map(|(l, _)| l.clone()))
                .unwrap_or_else(|| "Pick a folder".into());
            let items: Vec<(String, bool)> = targets
                .iter()
                .map(|(l, d)| (l.clone(), current.as_ref() == Some(d)))
                .chain([("Other folder…".to_string(), false)])
                .collect();
            let (view, root, key) = (
                view.clone(),
                inst.root.clone(),
                format!("{}{}", if rp { "rp:" } else { "" }, inst.root.display()),
            );
            let dirs: Vec<String> = targets.into_iter().map(|(_, d)| d).collect();
            let pick = Rc::new(move |ix: usize, _: &mut Window, cx: &mut App| {
                let (view, root, key) = (view.clone(), root.clone(), key.clone());
                match dirs.get(ix).cloned() {
                    Some(d) => view.update(cx, |this, cx| this.set_dest(key, d, cx)),
                    None => {
                        let paths = cx.prompt_for_paths(PathPromptOptions {
                            files: false,
                            directories: true,
                            multiple: false,
                            prompt: Some("Use this folder".into()),
                        });
                        cx.spawn(async move |cx| {
                            let Ok(Ok(Some(paths))) = paths.await else {
                                return;
                            };
                            let Some(p) = paths.into_iter().next() else {
                                return;
                            };
                            view.update(cx, |this, cx| match p.strip_prefix(&root) {
                                Ok(rel) => {
                                    this.set_dest(key, rel.to_string_lossy().replace('\\', "/"), cx)
                                }
                                Err(_) => {
                                    this.error = Some("Pick a folder inside the instance.".into());
                                    cx.notify();
                                }
                            });
                        })
                        .detach();
                    }
                }
            });
            Some(
                Self::section(
                    if rp {
                        "RESOURCE PACKS GO TO"
                    } else {
                        "DATA PACKS GO TO"
                    },
                    cx,
                )
                .child(widgets::picker("dest", label, items, rem, pick)),
            )
        });

        let versions: Vec<(String, bool)> =
            std::iter::once(("Any version".to_string(), self.game_version.is_empty()))
                .chain(
                    self.versions
                        .iter()
                        .map(|v| (v.clone(), *v == self.game_version)),
                )
                .collect();
        let version_list: Vec<String> = self.versions.clone();
        let pick_version = {
            let view = view.clone();
            Rc::new(move |ix: usize, _: &mut Window, cx: &mut App| {
                let v = if ix == 0 {
                    String::new()
                } else {
                    version_list.get(ix - 1).cloned().unwrap_or_default()
                };
                view.update(cx, |this, cx| {
                    this.game_version = v;
                    this.refilter(cx);
                });
            })
        };
        let loaders: Vec<(String, bool, Option<widgets::PickIcon>)> = Loader::ALL
            .iter()
            .map(|l| (l.name().to_string(), *l == self.loader, crate::brand::loader(l.name())))
            .collect();
        let pick_loader = {
            let view = view.clone();
            Rc::new(move |ix: usize, _: &mut Window, cx: &mut App| {
                view.update(cx, |this, cx| {
                    this.loader = Loader::ALL[ix];
                    this.refilter(cx);
                });
            })
        };

        let cats = self
            .categories
            .get(&self.category_key())
            .cloned()
            .unwrap_or_default();
        let store = self.store.read(cx);
        let cat_row = |id: Option<u64>, name: String, icon: Option<PathBuf>| {
            let active = self.category == id;
            h_flex()
                .id(ElementId::Name(format!("cat:{name}").into()))
                .gap_2()
                .px_2()
                .py_1()
                .rounded(theme.radius)
                .text_sm()
                .cursor_pointer()
                .when(active, |r| r.bg(theme.list_active).font_medium())
                .when(!active, |r| r.hover(|s| s.bg(theme.list_hover)))
                .when_some(icon, |r, p| r.child(img(p).size_4().flex_none()))
                .child(div().truncate().child(name))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.category = id;
                    this.refilter(cx);
                }))
        };
        let categories = v_flex()
            .gap_0p5()
            .child(cat_row(None, "All".into(), None))
            .children(cats.into_iter().map(|c| {
                let icon = store.image(&c.icon_url).map(Path::to_path_buf);
                cat_row(Some(c.id), c.name, icon)
            }));

        v_flex()
            .id("browse-filters")
            .w_64()
            .flex_none()
            .h_full()
            // Lets the page's site color through (see `render`).
            .bg(theme.sidebar.opacity(0.6))
            .border_r_1()
            .border_color(theme.sidebar_border)
            .px_4()
            .py_4()
            .gap_5()
            .child(self.source_switch(cx))
            .child(Self::section("TYPE", cx).child(classes))
            // A modpack installs as its own new instance.
            .when(self.class != cf::CLASS_MODPACKS || self.bookmarked, |r| {
                r.child(Self::section("INSTALL INTO", cx).child(widgets::picker_with(
                    "target",
                    target
                        .as_ref()
                        .map_or("Pick an instance".into(), |t| t.name.clone()),
                    None,
                    target_items,
                    rem,
                    pick_target,
                )))
            })
            .when(!self.bookmarked, |r| r.children(dest_picker))
            .when(!self.bookmarked, |r| r.child(Self::section("MINECRAFT", cx).child(widgets::picker(
                "version",
                if self.game_version.is_empty() {
                    "Any version".into()
                } else {
                    self.game_version.clone()
                },
                versions,
                rem,
                pick_version,
            ))))
            .when(
                !self.bookmarked && matches!(self.class, cf::CLASS_MODS | cf::CLASS_MODPACKS),
                |r| {
                    r.child(Self::section("LOADER", cx).child(widgets::picker_with(
                        "loader",
                        self.loader.name().into(),
                        crate::brand::loader(self.loader.name()),
                        loaders,
                        rem,
                        pick_loader,
                    )))
                },
            )
            .when(!self.bookmarked, |r| r.child(Self::section("CATEGORY", cx).child(categories)))
            .overflow_y_scrollbar()
    }

    fn set_dest(&mut self, key: String, dir: String, cx: &mut Context<Self>) {
        self.dest = Some(dir.clone());
        self.store.update(cx, |s, cx| {
            s.update_settings(
                |st| {
                    st.datapack_dest.insert(key, dir);
                },
                cx,
            )
        });
        cx.notify();
    }

    // ---- results -------------------------------------------------------

    fn result_card(
        &self,
        m: &cf::Mod,
        target: Option<&Instance>,
        global: &HashMap<u64, &'static str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let store = self.store.read(cx);
        let theme = cx.theme();
        let icon = m.icon().and_then(|u| store.image(u)).map(Path::to_path_buf);
        let picked = self.selected.as_ref().is_some_and(|s| s.id == m.id);
        let is_pack = hopper::browse::is_modpack(m, self.class);
        let have_pack = is_pack
            .then(|| {
                store
                    .instances()
                    .iter()
                    .find(|i| i.pack.as_ref().is_some_and(|p| p.project_id == m.id))
                    .map(|i| i.root.clone())
            })
            .flatten();
        let tracked = !matches!(m.class_id, Some(cf::CLASS_WORLDS | cf::CLASS_DATAPACKS));
        let installed = target
            .filter(|_| tracked)
            .and_then(|t| installed_as(t, m))
            .or_else(|| global.get(&m.id).copied());
        // Installed here, but there's a newer file: the instance's update
        // list has it, or (fresher) the search hit's own file index does.
        let update = target.filter(|_| installed.is_some()).and_then(|t| {
            let a = t.addons.iter().find(|a| a.project_id == m.id && !a.locked)?;
            let listed = match store.updates(&t.root) {
                Some(Updates::Ready(v)) => v.iter().find(|u| u.project_id == m.id).cloned(),
                _ => None,
            };
            let newer = listed.is_some()
                || (m.modrinth.is_none()
                    && hopper::install::newer_file_id(t, a, m, hopper::install::loader_of(t).api_id())
                        .is_some_and(|b| b > a.file_id));
            newer.then(|| (t.root.clone(), listed))
        });
        // Installed here but turned off: updates keep it off.
        let off = target
            .filter(|_| tracked && installed.is_some())
            .is_some_and(|t| t.addons.iter().any(|a| a.project_id == m.id && !a.enabled));
        let id = |p: &str| ElementId::Name(format!("{p}:{}", m.id).into());
        let (pending, hold, progress) = self.install_state(m.id, is_pack, cx);

        let action: AnyElement = if let Some(root) = have_pack {
            let (m1, m2) = (m.clone(), root);
            // A newer pack version: update it here, with its changelog.
            let pack_update = store
                .instance(&m2)
                .and_then(|i| store.pack_update(i))
                .cloned()
                .map(|f| {
                    let (s1, s2, r1, r2, f1, pid) =
                        (self.store.clone(), self.store.clone(), m2.clone(), m2.clone(), f.clone(), m.id);
                    widgets::split(
                        widgets::split_lead(
                            site_primary(Button::new(id("pack-update")), m.modrinth.is_some(), cx)
                                .small()
                                .icon(IconName::Download)
                                .label("Update")
                                .tooltip(format!("Update the pack to {}", f.display_name))
                                .disabled(hold.is_some())
                                .on_click(move |_, window, cx| {
                                    cx.stop_propagation();
                                    crate::pack_update::open(s1.clone(), r1.clone(), f1.clone(), window, cx)
                                }),
                        ),
                        id("pack-changes"),
                        site_tint(m.modrinth.is_some()),
                        gpui_kit::component::Size::Small,
                        "What changed in the pack",
                        move |_, window, cx| {
                            cx.stop_propagation();
                            crate::versions::open(s2.clone(), r2.clone(), pid, true, Some(f.clone()), window, cx)
                        },
                        cx,
                    )
                });
            h_flex()
                .gap_2()
                .map(|r| match pack_update {
                    Some(u) => r.child(u),
                    None => r.child(Tag::success().outline().small().child("Installed")),
                })
                .child(
                    Button::new(id("open"))
                        .small()
                        .outline()
                        .label("Open")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(BrowseEvent::OpenInstance(m2.clone()))
                        })),
                )
                .child({
                    // Rarely needed: kept out of the row, behind a menu.
                    let view = cx.entity();
                    let busy = pending || hold.is_some();
                    Button::new(id("pack-more"))
                        .small()
                        .ghost()
                        .icon(IconName::Ellipsis)
                        .tooltip("More")
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .dropdown_menu(move |menu, _, _| {
                            let (view, m1) = (view.clone(), m1.clone());
                            menu.item(
                                PopupMenuItem::new("Install again")
                                    .icon(IconName::Download)
                                    .disabled(busy)
                                    .on_click(move |_, window, cx| {
                                        view.update(cx, |this, cx| this.install(m1.clone(), None, window, cx))
                                    }),
                            )
                        })
                })
                .into_any_element()
        } else if let Some((root, listed)) = update {
            let m1 = m.clone();
            let (store, log_root, pid) = (self.store.clone(), root.clone(), m.id);
            let log_file = listed.as_ref().map(|u| u.file.clone());
            let lead = install_button(
                widgets::split_lead(site_primary(Button::new(id("update")), m.modrinth.is_some(), cx).small().icon(IconName::Download)),
                "Update",
                pending,
                progress,
                hold,
                cx,
            )
            .tooltip(if off {
                "A newer file is out; replaces the installed one, which stays disabled"
            } else {
                "A newer file is out; replaces the installed one"
            })
            .on_click(cx.listener(move |this, _, w, cx| {
                // Not a click on the card: the page stays as it is.
                cx.stop_propagation();
                match &listed {
                    Some(u) => this.store.update(cx, |s, cx| {
                        s.run_updates(&root, vec![u.clone()]);
                        cx.notify();
                    }),
                    // Not in the instance's list yet: install the newest
                    // file over the old one.
                    None => this.install(m1.clone(), None, w, cx),
                }
            }));
            widgets::split(
                lead,
                id("changes"),
                site_tint(m.modrinth.is_some()),
                gpui_kit::component::Size::Small,
                "What changed",
                move |_, window, cx| {
                    cx.stop_propagation();
                    crate::versions::open(store.clone(), log_root.clone(), pid, false, log_file.clone(), window, cx)
                },
                cx,
            )
            .into_any_element()
        } else if let Some(src) = installed {
            let site = matches!(src, "Modrinth" | "CurseForge");
            let same = (src == "Modrinth") == m.modrinth.is_some();
            let text = if !site {
                format!("In {src}")
            } else if same {
                "Installed".into()
            } else {
                format!("Installed ({src})")
            };
            Tag::success().outline().small().child(text).into_any_element()
        } else {
            let m1 = m.clone();
            // What else it downloads, once hovering or the panel worked it out.
            let extra: Vec<String> = match self.plan_for(m, target) {
                Some(Load::Ready(p)) => p.extra().iter().map(|(d, _)| d.name.clone()).collect(),
                _ => Vec::new(),
            };
            let tip: SharedString = format!("Also downloads {}", and_list(&extra)).into();
            h_flex()
                .gap_2()
                .when(!extra.is_empty(), |r| {
                    let tip = tip.clone();
                    r.child(
                        h_flex()
                            .id(id("deps"))
                            .gap_1()
                            .px_1p5()
                            .py_0p5()
                            .rounded_full()
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.muted)
                            .text_xs()
                            .text_color(theme.foreground)
                            .child(Icon::new(IconName::Layers).xsmall().text_color(theme.muted_foreground))
                            .child(match extra.len() {
                                1 => "+1 dep".to_string(),
                                n => format!("+{n} deps"),
                            })
                            .tooltip(move |w, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx)
                            }),
                    )
                })
                .child(
                    install_button(
                        site_primary(Button::new(id("install")), m.modrinth.is_some(), cx).small().icon(IconName::Download),
                        "Install",
                        pending,
                        progress,
                        hold,
                        cx,
                    )
                    .when(!is_pack && target.is_none(), |b| b.disabled(true))
                    .when(!extra.is_empty() && hold.is_none(), |b| b.tooltip(tip))
                    .on_click(cx.listener(move |this, _, w, cx| this.install(m1.clone(), None, w, cx))),
                )
                .into_any_element()
        };

        let open = m.clone();
        let hovered = m.clone();
        h_flex()
            .id(id("card"))
            .on_hover(cx.listener(move |this, over: &bool, _, cx| {
                if *over {
                    this.want_plan(&hovered, cx);
                }
            }))
            .gap_4()
            .p_3()
            .rounded(theme.radius_lg)
            .border_1()
            .cursor_pointer()
            .when(picked, |r| match m.modrinth {
                Some(_) => r.border_color(theme.success).bg(theme.success.opacity(0.08)),
                None => r.border_color(theme.primary).bg(theme.list_active),
            })
            .when(!picked, |r| {
                r.border_color(theme.border)
                    .hover(|s| s.bg(theme.list_hover))
            })
            .on_click(cx.listener(move |this, _, _, cx| this.open_detail(open.clone(), cx)))
            .child(widgets::thumb(icon.as_deref(), rems(3.5), cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().font_semibold().truncate().child(m.name.clone()))
                            .child(self.author_link(m, id("author"), cx))
                            // The saved list mixes both sites.
                            .when(self.bookmarked, |r| {
                                r.child(widgets::site_tag(m.modrinth.is_some()))
                            }),
                    )
                    .child(div().text_sm().truncate().child(m.summary.clone()))
                    .child(stats(m, false, self.on_category(m, cx), cx).pt_0p5()),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .children(self.server_badge(m, id("server"), false, cx))
                    .when(off, |r| {
                        let tip: SharedString = match target {
                            Some(t) => format!("Off in {}. Updating keeps it off.", t.name),
                            None => "Disabled. Updating keeps it disabled.".to_string(),
                        }
                        .into();
                        r.child(
                            widgets::pill("Off", theme.warning, false)
                                .id(id("off"))
                                .mr_1()
                                .tooltip(move |w, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx)
                                }),
                        )
                    })
                    .child(action)
                    .child(self.bookmark_button(m, id("bookmark"), cx)),
            )
            .context_menu({
                let (view, m) = (cx.entity(), m.clone());
                let saved = hopper::bookmarks::with(|b| b.has(&m));
                let site = if m.modrinth.is_some() { "Modrinth" } else { "CurseForge" };
                move |menu, _, _| {
                    let (v1, v2, m1, m2) = (view.clone(), view.clone(), m.clone(), m.clone());
                    let (url1, url2) = (m.page(), m.page());
                    menu.item(
                        PopupMenuItem::new("Open").icon(IconName::PanelRight).on_click(move |_, _, cx| {
                            v1.update(cx, |this, cx| this.open_detail(m1.clone(), cx))
                        }),
                    )
                    .item(
                        PopupMenuItem::new(if saved { "Remove bookmark" } else { "Bookmark" })
                            .icon(if saved { IconName::BookmarkCheck } else { IconName::Bookmark })
                            .on_click(move |_, window, cx| {
                                v2.update(cx, |this, cx| this.toggle_bookmark(&m2, window, cx))
                            }),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new(format!("Open on {site}"))
                            .icon(IconName::ExternalLink)
                            .on_click(move |_, _, cx| cx.open_url(&url1)),
                    )
                    .item(
                        PopupMenuItem::new("Copy link").icon(IconName::Copy).on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(url2.clone()))
                        }),
                    )
                }
            })
            .into_any_element()
    }

    /// Saves `m` for later, or forgets it.
    fn toggle_bookmark(&mut self, m: &cf::Mod, window: &mut Window, cx: &mut Context<Self>) {
        let class = self.class;
        if let Err(e) = hopper::bookmarks::with(|b| b.toggle(m, class)) {
            window.push_notification(
                Notification::error(format!("Couldn't save the bookmark: {e:#}")),
                cx,
            );
        }
        cx.notify();
    }

    /// The bookmark toggle for `m`: filled once it's saved.
    fn bookmark_button(&self, m: &cf::Mod, id: ElementId, cx: &Context<Self>) -> Button {
        let saved = hopper::bookmarks::with(|b| b.has(m));
        let m = m.clone();
        Button::new(id)
            .small()
            .ghost()
            .icon(if saved {
                IconName::BookmarkCheck
            } else {
                IconName::Bookmark
            })
            .when(saved, |b| b.text_color(cx.theme().primary))
            .tooltip(if saved {
                "Bookmarked: click to remove"
            } else {
                "Bookmark for later"
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                // Not a click on the card: the page stays as it is.
                cx.stop_propagation();
                this.toggle_bookmark(&m, window, cx)
            }))
    }

    /// Lists the bookmarks instead of search results.
    fn show_bookmarks(&mut self, cx: &mut Context<Self>) {
        self.bookmarked = true;
        let urls: Vec<String> = hopper::bookmarks::with(|b| {
            b.list().iter().map(|b| b.icon.clone()).filter(|u| !u.is_empty()).collect()
        });
        self.store
            .update(cx, |s, _| urls.iter().for_each(|u| s.want_image(u)));
        cx.notify();
    }

    /// The bookmarks matching the search box, newest first.
    fn bookmark_results(&self) -> Vec<cf::Mod> {
        let q = self.text.to_lowercase();
        hopper::bookmarks::with(|b| {
            b.list()
                .iter()
                .filter(|b| {
                    q.is_empty()
                        || [&b.name, &b.summary, &b.author]
                            .iter()
                            .any(|s| s.to_lowercase().contains(&q))
                })
                .map(|b| b.to_mod())
                .collect()
        })
    }

    fn results(&self, target: Option<&Instance>, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let global = target
            .and_then(|t| self.global.get(&t.root))
            .cloned()
            .unwrap_or_default();
        let saved = self.bookmarked.then(|| self.bookmark_results());
        let is_pack = self.class == cf::CLASS_MODPACKS;
        let note = match &saved {
            Some(list) if list.is_empty() && self.text.is_empty() => {
                "No bookmarks yet: click the bookmark beside a project's Install to save it here.".to_string()
            }
            Some(list) => format!("{} bookmarked", list.len()),
            None => format!("{} results on ", self.total),
        };
        // The site's name, in its color, after "results on".
        let site_name = saved.is_none().then(|| {
            div()
                .flex_none()
                .text_sm()
                .text_color(site_color(self.modrinth))
                .child(if self.modrinth { "Modrinth" } else { "CurseForge" })
        });
        let mut tail = String::new();
        // Browsing the other site than the target's pack: what's installed
        // from here doesn't come from where the pack's mods do.
        let off_site = target
            .filter(|_| !self.bookmarked && !is_pack)
            .and_then(|t| t.pack.as_ref().map(|p| (t.name.clone(), hopper::modrinth::is_mr(p.project_id))))
            .filter(|(_, pack_mr)| *pack_mr != self.modrinth);
        let some_need_target = match &saved {
            Some(list) => list.iter().any(|m| !hopper::browse::is_modpack(m, self.class)),
            None => !is_pack,
        };
        if some_need_target && target.is_none() {
            tail.push_str(" · pick an instance on the left to install");
        }
        let cards: Vec<AnyElement> = saved
            .as_ref()
            .unwrap_or(&self.results)
            .iter()
            .map(|m| self.result_card(m, target, &global, cx))
            .collect();
        let more = saved.is_none()
            && !self.loading
            && !self.results.is_empty()
            && (self.results.len() as u32) < self.total.min(10_000);
        let sort_bar = saved.is_none().then(|| self.sort_bar(cx));
        let theme = cx.theme();
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            // A hint of the site's color, so it's clear where results come
            // from (the saved list mixes both).
            .child(
                v_flex()
                    .px_6()
                    .pt_5()
                    .pb_3()
                    .gap_2()
                    .child(
                        Input::new(&self.search)
                            .prefix(Icon::new(IconName::Search).small())
                            .cleanable(true),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .child(widgets::caption(note, cx).flex_none().whitespace_nowrap())
                                    .children(site_name)
                                    .when(!tail.is_empty(), |r| r.child(widgets::caption(tail, cx))),
                            )
                            .when(self.loading && saved.is_none(), |r| r.child(Spinner::new().xsmall()))
                            .when_some(sort_bar, |r, b| r.child(div().flex_1()).child(b)),
                    )
                    .when_some(self.error.clone(), |r, e| {
                        r.child(div().text_sm().text_color(theme.danger).child(e))
                    })
                    .when_some(off_site, |r, (name, pack_mr)| {
                        let (here, home) = if self.modrinth {
                            ("Modrinth", "CurseForge")
                        } else {
                            ("CurseForge", "Modrinth")
                        };
                        r.child(
                            h_flex()
                                .gap_2()
                                .px_3()
                                .py_2()
                                .rounded(theme.radius_lg)
                                .border_1()
                                .border_color(theme.warning.opacity(0.4))
                                .bg(theme.warning.opacity(0.08))
                                .text_sm()
                                .text_color(theme.warning)
                                .child(Icon::new(IconName::TriangleAlert).small().flex_none())
                                .child(div().flex_1().min_w_0().child(format!(
                                    "You're browsing {here}. {name} is a {home} pack: mods installed from here (and what they need) come from {here}."
                                )))
                                .child(
                                    Button::new("browse-switch-site")
                                        .small()
                                        .outline()
                                        .label(format!("Switch to {home}"))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.set_source(pack_mr, window, cx)
                                        })),
                                ),
                        )
                    })
                    .when(!self.modrinth && cf::api_key().is_none(), |r| {
                        r.child(
                            Button::new("browse-open-settings")
                                .small()
                                .outline()
                                .label("Open Settings")
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(BrowseEvent::OpenSettings))),
                        )
                    })
                    .when(!self.bookmarked && self.modrinth && self.class == cf::CLASS_WORLDS, |r| {
                        r.child(widgets::caption(
                            "Modrinth doesn't host worlds; switch to CurseForge for maps.",
                            cx,
                        ))
                    }),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(self.result_list(cards, more, saved.is_none(), cx))
                    .vertical_scrollbar(&self.scroll)
                    .when(!self.fresh.is_empty() && saved.is_none(), |r| {
                        let n = self.fresh.len();
                        r.child(
                            h_flex().absolute().top_2().left_0().right_0().justify_center().child(
                                // Occluded, so the click doesn't also open the card below.
                                div().occlude().child(
                                    site_primary(Button::new("browse-fresh"), self.modrinth, cx)
                                        .small()
                                        .icon(IconName::ArrowUp)
                                        .label(if n == 1 { "1 new result".to_string() } else { format!("{n} new results") })
                                        .tooltip("Back to the top, with the new ones")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.show_fresh();
                                            cx.notify();
                                        })),
                                ),
                            ),
                        )
                    }),
            )
    }

    /// The result cards, scrolled by [`Self::scroll`] so the place is kept
    /// while Browse is hidden.
    fn result_list(&self, cards: Vec<AnyElement>, more: bool, searching: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        v_flex()
            .id("browse-results")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .px_6()
            .pb_6()
            .gap_2()
            .children(cards)
            // The next page loads once the list's end nears the view.
            .when(more, |r| {
                let view = cx.entity().downgrade();
                r.child(
                    canvas(
                        move |bounds, window, cx| {
                            if bounds.top() < window.viewport_size().height + px(800.) {
                                cx.defer(move |cx| {
                                    view.update(cx, |this, cx| {
                                        if !this.loading {
                                            this.run_search(true, cx);
                                        }
                                    })
                                    .ok();
                                });
                            }
                        },
                        |_, _, _, _| {},
                    )
                    .w_full()
                    .h(px(1.)),
                )
            })
            .when(searching && self.loading && !self.results.is_empty(), |r| {
                r.child(
                    h_flex()
                        .justify_center()
                        .gap_2()
                        .py_3()
                        .child(Spinner::new().small())
                        .child(widgets::caption("Loading more", cx)),
                )
            })
    }

    /// The sorts as a row of icons right of the results count: the picked
    /// one in the site's color with its name, the rest named on hover.
    fn sort_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let (muted, fg, hover, track, radius) =
            (theme.muted_foreground, theme.foreground, theme.list_hover, theme.muted, theme.radius);
        let color = site_color(self.modrinth);
        let buttons: Vec<AnyElement> = SortField::ALL
            .iter()
            .map(|&f| {
                let on = f == self.sort;
                let icon = match f {
                    SortField::Popularity => IconName::Flame,
                    SortField::Featured => IconName::Star,
                    SortField::TotalDownloads => IconName::Download,
                    SortField::LastUpdated => IconName::RefreshCw,
                    SortField::Released => IconName::Sparkles,
                    SortField::Name => IconName::ArrowDownAZ,
                };
                let name = f.name();
                h_flex()
                    .id(ElementId::Name(format!("sort-{name}").into()))
                    .h_7()
                    .px_2()
                    .gap_1p5()
                    .rounded(radius)
                    .text_sm()
                    .cursor_pointer()
                    .child(Icon::new(icon).small())
                    .map(|d| {
                        if on {
                            d.bg(color.opacity(0.15)).text_color(color).font_medium().child(name)
                        } else {
                            d.text_color(muted)
                                .hover(move |s| s.bg(hover).text_color(fg))
                                .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(name).build(w, cx))
                        }
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.sort != f {
                            this.sort = f;
                            this.store.update(cx, |s, cx| {
                                s.update_settings(|st| st.last_sort = f.key().to_string(), cx)
                            });
                            this.refilter(cx);
                        }
                    }))
                    .into_any_element()
            })
            .collect();
        h_flex()
            .flex_none()
            .gap_2()
            .child(div().text_xs().font_semibold().text_color(muted).child("SORT"))
            .child(
                h_flex()
                    .p_0p5()
                    .gap_0p5()
                    .rounded(radius * 1.5)
                    .bg(track)
                    .children(buttons),
            )
    }

    // ---- detail --------------------------------------------------------

    fn detail(&self, m: &cf::Mod, target: Option<&Instance>, cx: &mut Context<Self>) -> AnyElement {
        let banner = self.link_banner(m, target, cx);
        let store = self.store.read(cx);
        let theme = cx.theme();
        let icon = m.icon().and_then(|u| store.image(u)).map(Path::to_path_buf);
        let page = m.page();
        let site = if m.modrinth.is_some() {
            "Modrinth"
        } else {
            "CurseForge"
        };
        let gallery: Vec<(String, String)> =
            m.screenshots.iter().map(|s| (s.url.clone(), s.title.clone())).collect();
        let store_e = self.store.clone();
        let shots: Vec<AnyElement> = m
            .screenshots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                let p = store.image(&s.thumbnail_url)?.to_path_buf();
                let (gallery, store_e) = (gallery.clone(), store_e.clone());
                Some(
                    div()
                        .id(ElementId::Name(format!("shot:{}", s.url).into()))
                        .flex_none()
                        .w_40()
                        .h_24()
                        .rounded(theme.radius)
                        .overflow_hidden()
                        .cursor_pointer()
                        .child(img(p).size_full().object_fit(ObjectFit::Cover))
                        .on_click(move |_, window, cx| {
                            crate::image_viewer::open(store_e.clone(), gallery.clone(), i, window, cx)
                        })
                        .into_any_element(),
                )
            })
            .collect();

        let header = h_flex()
            .gap_4()
            .items_start()
            .child(widgets::thumb(icon.as_deref(), rems(4.), cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(div().text_xl().font_semibold().child(m.name.clone()))
                    .child(self.author_link(m, "detail-author".into(), cx))
                    .child(stats(m, true, self.on_category(m, cx), cx).flex_wrap().pt_0p5())
                    .children(facts(m, target, cx))
                    .child(
                        h_flex()
                            .pt_1()
                            .gap_1()
                            .flex_wrap()
                            .child(
                                Button::new("site")
                                    .small()
                                    .outline()
                                    .text_color(site_color(m.modrinth.is_some()))
                                    .icon(IconName::ExternalLink)
                                    .label(format!("Open on {site}"))
                                    .on_click(move |_, _, cx| cx.open_url(&page)),
                            )
                            .children(self.server_badge(m, "detail-server".into(), true, cx))
                            .children(self.server_files_of(m).map(|files| {
                                let (store, name) = (self.store.clone(), m.name.clone());
                                Button::new("detail-host")
                                    .small()
                                    .ghost()
                                    .icon(IconName::Server)
                                    .label("Host a server")
                                    .tooltip("Make a server on this PC from the pack's server files")
                                    .on_click(move |_, window, cx| {
                                        crate::host_dialog::open_for_files(
                                            store.clone(),
                                            files.clone(),
                                            name.clone(),
                                            window,
                                            cx,
                                        )
                                    })
                            }))

                            .children(m.links.extra().into_iter().map(|(label, url)| {
                                let url = url.to_string();
                                let icon = match label {
                                    "Source" => IconName::Code,
                                    "Issues" => IconName::Bug,
                                    "Wiki" => IconName::BookOpen,
                                    _ => IconName::MessageCircle,
                                };
                                Button::new(ElementId::Name(format!("link:{label}").into()))
                                    .small()
                                    .ghost()
                                    .icon(icon)
                                    .label(label)
                                    .tooltip(url.clone())
                                    .on_click(move |_, _, cx| cx.open_url(&url))
                            })),
                    ),
            )
            .child(self.bookmark_button(m, "detail-bookmark".into(), cx))
            .child(
                Button::new("close-detail")

                    .ghost()
                    .small()
                    .icon(IconName::X)
                    .tooltip("Close")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.selected = None;
                        this.author = None;
                        cx.notify();
                    })),
            );

        let body: AnyElement = match self.detail.get(&m.id) {
            None | Some(Load::Loading) => h_flex()
                .gap_2()
                .child(Spinner::new().small())
                .child(widgets::caption("Loading", cx))
                .into_any_element(),
            Some(Load::Failed(e)) => div()
                .text_sm()
                .text_color(theme.danger)
                .child(e.clone())
                .into_any_element(),
            Some(Load::Ready(d)) => {
                let installed = target
                    .as_ref()
                    .and_then(|t| t.addons.iter().find(|a| a.project_id == m.id));
                let installed_file = installed.map(|a| a.file_id);
                let locked = installed.is_some_and(|a| a.locked);
                let is_pack = hopper::browse::is_modpack(m, self.class);
                let can = is_pack || target.is_some();
                let (pending, hold, progress) = self.install_state(m.id, is_pack, cx);
                let files = v_flex()
                    .gap_0p5()
                    .children(d.files.iter().take(40).map(|f| {
                        let fits = target
                            .as_ref()
                            .is_none_or(|t| f.fits(&t.game_version, hopper::install::loader_of(t)));
                        let versions = f
                            .game_versions
                            .iter()
                            .filter(|v| v.starts_with(|c: char| c.is_ascii_digit()))
                            .take(2)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ");
                        let (m1, f1) = (m.clone(), f.clone());
                        let (store, log_root, log_file) =
                            (self.store.clone(), target.as_ref().map(|t| t.root.clone()), f.clone());
                        let log_id = ElementId::Name(format!("log:{}", f.id).into());
                        let open_log = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                            crate::versions::open_file(store.clone(), log_root.clone(), log_file.clone(), window, cx)
                        };
                        let action: AnyElement = if installed_file == Some(f.id) {
                            widgets::split(
                                widgets::split_lead(Tag::success().xsmall().h_5().child("Installed")),
                                log_id,
                                widgets::Tint::Success,
                                gpui_kit::component::Size::XSmall,
                                "Changelog",
                                open_log,
                                cx,
                            )
                            .into_any_element()
                        } else {
                            let lead = install_button(
                                widgets::split_lead(
                                    Button::new(ElementId::Name(format!("file:{}", f.id).into()))
                                        .xsmall()
                                        .outline(),
                                ),
                                if installed_file.is_some() { "Switch" } else { "Install" },
                                pending,
                                progress,
                                hold,
                                cx,
                            )
                            .when(!can || locked, |b| b.disabled(true))
                            .when(locked, |b| b.tooltip("Locked: unlock it in the Mods tab to switch versions"))
                                .on_click(cx.listener(move |this, _, w, cx| {
                                    this.install(m1.clone(), Some(f1.clone()), w, cx)
                                }));
                            widgets::split(lead, log_id, widgets::Tint::Outline, gpui_kit::component::Size::XSmall, "Changelog", open_log, cx)
                                .into_any_element()
                        };
                        // A pack version's own server zip.
                        let server_pack = f.server_pack_file_id.filter(|id| *id != 0).map(|sp| {
                            let (store, pid, name) = (self.store.clone(), m.id, f.display_name.clone());
                            Button::new(ElementId::Name(format!("sp:{}", f.id).into()))
                                .xsmall()
                                .ghost()
                                .icon(IconName::Server)
                                .tooltip("Download the server pack for this version")
                                .on_click(move |_, _, cx| {
                                    crate::dialogs::save_server_pack(store.clone(), pid, sp, name.clone(), cx)
                                })
                        });
                        h_flex()
                            .gap_3()
                            .px_2()
                            .py_1p5()
                            .rounded(theme.radius)
                            .hover(|s| s.bg(theme.list_hover))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .text_sm()
                                            .truncate()
                                            .when(!fits, |d| d.text_color(theme.muted_foreground))
                                            .child(f.display_name.clone()),
                                    )
                                    .child(
                                        widgets::caption(
                                            // When it came out, as the stats say it ("2 days ago", a date).
                                            match hopper::time::relative(&f.file_date) {
                                                d if d.is_empty() => format!("{} · {versions}", f.release_name()),
                                                d => format!("{} · {versions} · {d}", f.release_name()),
                                            },
                                            cx,
                                        )
                                        .text_xs(),
                                    ),
                            )
                            .children(server_pack)
                            .child(action)
                    }));
                let deps = self.deps.get(&m.id).cloned().unwrap_or_default();
                let dep_rows = v_flex()
                    .gap_0p5()
                    .children(deps.into_iter().map(|(d, rel)| {
                        let icon = d.icon().and_then(|u| store.image(u)).map(Path::to_path_buf);
                        let have = target
                            .as_ref()
                            .is_some_and(|t| installed_as(t, &d).is_some());
                        let tag = match rel {
                            cf::DEP_REQUIRED => Tag::warning().xsmall().child("Required"),
                            cf::DEP_INCOMPATIBLE => Tag::danger().xsmall().child("Incompatible"),
                            _ => Tag::secondary().xsmall().child("Optional"),
                        };
                        let open = d.clone();
                        h_flex()
                            .id(ElementId::Name(format!("dep:{}", d.id).into()))
                            .gap_3()
                            .px_2()
                            .py_1p5()
                            .rounded(theme.radius)
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.list_hover))
                            .on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.open_detail(open.clone(), cx)
                                }),
                            )
                            .child(widgets::thumb(icon.as_deref(), rems(1.75), cx))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .truncate()
                                    .child(d.name.clone()),
                            )
                            .child(tag)
                            .when(have, |r| {
                                r.child(Tag::success().xsmall().child("Installed"))
                            })
                    }));
                let n_files = d.files.len();
                let n_deps = self.deps.get(&m.id).map_or(0, |d| d.len());
                let tab = self.detail_tab;
                v_flex()
                    .gap_4()
                    .child(
                        TabBar::new("detail-tabs")
                            .underline()
                            .selected_index(tab)
                            .child(Tab::new().label("Description"))
                            .child(Tab::new().label(format!("Files {n_files}")))
                            .child(Tab::new().label(format!("Dependencies {n_deps}")))
                            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                this.detail_tab = *ix;
                                cx.notify();
                            })),
                    )
                    .map(|c| match tab {
                        1 => c.child(files),
                        2 if n_deps == 0 => c.child(widgets::caption("No dependencies.", cx)),
                        2 => c.child(dep_rows),
                        _ => c.child(self.desc.clone()),
                    })
                    .into_any_element()
            }
        };

        v_flex()
            .id("browse-detail")
            .size_full()
            .px_5()
            .py_5()
            .gap_5()
            .child(header)
            .when(!shots.is_empty(), |r| {
                // A set height: the scroll wrapper otherwise stretches to
                // fill the column, leaving gaps around the thumbnails.
                r.child(
                    div().flex_none().w_full().h_24().child(
                        h_flex()
                            .id("shots")
                            .size_full()
                            .gap_2()
                            .children(shots)
                            .overflow_x_scrollbar(),
                    ),
                )
            })
            .children(banner)
            // Turned off in the instance: updating or switching keeps it off.
            .children(
                target
                    .as_ref()
                    .filter(|t| t.addons.iter().any(|a| a.project_id == m.id && !a.enabled))
                    .map(|t| {
                        h_flex()
                            .gap_2()
                            .text_sm()
                            .text_color(theme.warning)
                            .items_start()
                            .child(Icon::new(IconName::Info).small().flex_none())
                            .child(div().flex_1().min_w_0().child(format!(
                                "Off in {}. Updating or switching versions keeps it off; turn it on in the Mods tab.",
                                t.name
                            )))
                    }),
            )
            .when(m.allow_mod_distribution == Some(false), |r| {
                r.child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .text_color(theme.warning)
                        .items_start()
                        .child(Icon::new(IconName::TriangleAlert).small().flex_none())
                        .child(div().flex_1().min_w_0().child(
                            "The author only allows downloads from the CurseForge website. Install opens its page in Hopper's download window and adds the file when it arrives.",
                        )),
                )
            })
            .children(self.install_card(m, target, cx))
            .child(body)
            .overflow_y_scrollbar()
            .into_any_element()
    }
}

impl BrowseView {
    /// The panel listing everything one author made.
    fn author_panel(&self, target: Option<&Instance>, cx: &mut Context<Self>) -> AnyElement {
        let Some(a) = self.author.as_ref() else { return div().into_any_element() };
        let store = self.store.read(cx);
        let theme = cx.theme();
        let site = if a.of.modrinth.is_some() { "Modrinth" } else { "CurseForge" };
        let url = a.of.authors.first().map(|x| x.url.clone()).filter(|u| !u.is_empty());
        let initial: String = a.name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
        let summary = match &a.projects {
            Load::Ready(v) => format!(
                "{} projects · {} downloads",
                v.len(),
                human_count(v.iter().map(|m| m.download_count).sum())
            ),
            Load::Loading => "Loading projects".into(),
            Load::Failed(_) => format!("On {site}"),
        };

        let back = self.selected.as_ref().map(|s| {
            h_flex().child(
                Button::new("author-back")
                    .ghost()
                    .small()
                    .icon(IconName::ArrowLeft)
                    .label(format!("Back to {}", s.name))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.author = None;
                        cx.notify();
                    })),
            )
        });
        let header = h_flex()
            .gap_4()
            .items_start()
            .child(
                div()
                    .size(rems(4.))
                    .flex_none()
                    .rounded_full()
                    .bg(theme.primary.opacity(0.15))
                    .border_1()
                    .border_color(theme.primary.opacity(0.4))
                    .text_color(theme.primary)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_2xl()
                    .font_semibold()
                    .child(initial),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(div().text_xl().font_semibold().truncate().child(a.name.clone()))
                    .child(widgets::caption(summary, cx))
                    .children(url.map(|u| {
                        h_flex().pt_1().child(
                            Button::new("author-site")
                                .small()
                                .outline()
                                .text_color(site_color(a.of.modrinth.is_some()))
                                .icon(IconName::ExternalLink)
                                .label(format!("Open on {site}"))
                                .on_click(move |_, _, cx| cx.open_url(&u)),
                        )
                    })),
            )
            .child(
                Button::new("close-author")
                    .ghost()
                    .small()
                    .icon(IconName::X)
                    .tooltip("Close")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.author = None;
                        this.selected = None;
                        cx.notify();
                    })),
            );

        let body: AnyElement = match &a.projects {
            Load::Loading => h_flex()
                .gap_2()
                .child(Spinner::new().small())
                .child(widgets::caption("Loading", cx))
                .into_any_element(),
            Load::Failed(e) => div().text_sm().text_color(theme.danger).child(e.clone()).into_any_element(),
            Load::Ready(v) if v.is_empty() => widgets::caption("No projects found.", cx).into_any_element(),
            Load::Ready(v) => v_flex()
                .gap_1()
                .children(v.iter().map(|m| {
                    let icon = m.icon().and_then(|u| store.image(u)).map(Path::to_path_buf);
                    let current = m.id == a.of.id;
                    let have = target.is_some_and(|t| installed_as(t, m).is_some());
                    let (cls_icon, cls) = class_label(m.class_id.unwrap_or(cf::CLASS_MODS));
                    let open = m.clone();
                    h_flex()
                        .id(ElementId::Name(format!("by:{}", m.id).into()))
                        .gap_3()
                        .p_2()
                        .rounded(theme.radius)
                        .cursor_pointer()
                        .when(current, |r| r.bg(theme.list_active))
                        .hover(|s| s.bg(theme.list_hover))
                        .on_click(cx.listener(move |this, _, _, cx| this.open_detail(open.clone(), cx)))
                        .child(widgets::thumb(icon.as_deref(), rems(2.75), cx))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_0p5()
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .child(div().text_sm().font_semibold().truncate().child(m.name.clone()))
                                        .when(have, |r| {
                                            r.child(Tag::success().outline().xsmall().child("Installed"))
                                        }),
                                )
                                .child(widgets::caption(m.summary.clone(), cx).text_xs())
                                .child(
                                    h_flex()
                                        .gap_3()
                                        .text_xs()
                                        .child(stat(cls_icon, cls, cx))
                                        .child(stat(IconName::Download, human_count(m.download_count), cx))
                                        .child(stat(
                                            IconName::RefreshCw,
                                            hopper::time::relative(&m.date_modified),
                                            cx,
                                        )),
                                ),
                        )
                }))
                .into_any_element(),
        };

        v_flex()
            .id("browse-author")
            .size_full()
            .px_5()
            .py_5()
            .gap_5()
            .children(back)
            .child(header)
            .child(body)
            .overflow_y_scrollbar()
            .into_any_element()
    }

    /// The panel's install box: where it goes and every mod it pulls in.
    fn install_card(&self, m: &cf::Mod, target: Option<&Instance>, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = target?;
        let plan = self.plan_for(m, target)?;
        if installed_as(t, m).is_some() {
            return None;
        }
        let store = self.store.read(cx);
        let theme = cx.theme();
        let (pending, hold, progress) = self.install_state(m.id, false, cx);
        let size = |b: u64| hopper::instance_ops::human_bytes(b);

        let summary: AnyElement = match plan {
            Load::Loading => h_flex()
                .gap_2()
                .child(Spinner::new().xsmall())
                .child(widgets::caption("Checking what it needs", cx))
                .into_any_element(),
            Load::Failed(e) => widgets::caption(e.clone(), cx).into_any_element(),
            Load::Ready(p) => {
                let n = p.extra().len();
                let total: u64 = p.steps.iter().map(|(_, f)| f.file_length).sum();
                let text = match n {
                    0 => format!("Nothing else needed · {}", size(total)),
                    1 => format!("Also downloads 1 required mod · {} total", size(total)),
                    n => format!("Also downloads {n} required mods · {} total", size(total)),
                };
                h_flex()
                    .gap_1p5()
                    .text_sm()
                    .text_color(if n == 0 { theme.success } else { theme.muted_foreground })
                    .child(
                        Icon::new(if n == 0 { IconName::CircleCheck } else { IconName::Layers })
                            .xsmall(),
                    )
                    .child(text)
                    .into_any_element()
            }
        };

        let m1 = m.clone();
        let head = h_flex()
            .gap_3()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        div()
                            .font_semibold()
                            .truncate()
                            .child(format!("Install into {}", t.name)),
                    )
                    .child(summary),
            )
            .child(
                install_button(
                    site_primary(Button::new("panel-install"), m.modrinth.is_some(), cx).icon(IconName::Download),
                    "Install",
                    pending,
                    progress,
                    hold,
                    cx,
                )
                .on_click(cx.listener(move |this, _, w, cx| this.install(m1.clone(), None, w, cx))),
            );

        let mut card = v_flex()
            .gap_3()
            .p_4()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(head);

        if let Load::Ready(p) = plan {
            if !p.extra().is_empty() {
                let rows = p.extra().iter().map(|(d, f)| {
                    let icon = d.icon().and_then(|u| store.image(u)).map(Path::to_path_buf);
                    let by = p.by.get(&d.id).filter(|b| **b != m.name);
                    let open = d.clone();
                    h_flex()
                        .id(ElementId::Name(format!("plan:{}", d.id).into()))
                        .gap_3()
                        .px_2()
                        .py_1p5()
                        .rounded(theme.radius)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.list_hover))
                        .on_click(cx.listener(move |this, _, _, cx| this.open_detail(open.clone(), cx)))
                        .child(widgets::thumb(icon.as_deref(), rems(1.75), cx))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_sm().truncate().child(d.name.clone()))
                                .child(
                                    widgets::caption(
                                        match by {
                                            Some(b) => format!("Needed by {b}"),
                                            None => "Required".to_string(),
                                        },
                                        cx,
                                    )
                                    .text_xs(),
                                ),
                        )
                        .child(widgets::caption(size(f.file_length), cx).text_xs().flex_none())
                });
                card = card.child(
                    v_flex()
                        .gap_0p5()
                        .pt_3()
                        .border_t_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .px_2()
                                .pb_1()
                                .text_xs()
                                .font_semibold()
                                .text_color(theme.muted_foreground)
                                .child("ALSO DOWNLOADS"),
                        )
                        .children(rows),
                );
            }
            let line = |icon: IconName, color: Hsla, text: String| {
                h_flex()
                    .gap_1p5()
                    .px_2()
                    .text_xs()
                    .text_color(color)
                    .items_start()
                    .child(Icon::new(icon).xsmall().flex_none())
                    .child(div().flex_1().min_w_0().child(text))
            };
            let on: Vec<String> = p.kept.iter().filter(|(_, e)| *e).map(|(n, _)| n.clone()).collect();
            let off: Vec<String> = p.kept.iter().filter(|(_, e)| !*e).map(|(n, _)| n.clone()).collect();
            if !on.is_empty() {
                card = card.child(line(
                    IconName::Check,
                    theme.muted_foreground,
                    format!("Already in this instance: {}", and_list(&on)),
                ));
            }
            if !off.is_empty() {
                card = card.child(line(
                    IconName::RotateCcw,
                    theme.warning,
                    format!("Turns back on: {}", and_list(&off)),
                ));
            }
            for n in p.notes.iter().chain(&p.suggest) {
                card = card.child(line(IconName::TriangleAlert, theme.warning, n.clone()));
            }
        }
        Some(card.into_any_element())
    }

    /// "Opened from curseforge.com": the linked file, ready to install.
    fn link_banner(
        &self,
        m: &cf::Mod,
        target: Option<&Instance>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let f = self.link_file.clone().filter(|f| f.mod_id == m.id)?;
        let pack = hopper::browse::is_modpack(m, self.class);
        let have = target.is_some_and(|t| {
            t.addons
                .iter()
                .any(|a| a.file_id == f.id && a.project_id == m.id)
        });
        let ready = pack || target.is_some();
        let (pending, hold, progress) = self.install_state(m.id, pack, cx);
        let theme = cx.theme();
        let m2 = m.clone();
        Some(
            crate::crash_help::callout(theme.primary, cx)
                .child(div().font_semibold().child("Opened from curseforge.com"))
                .child(div().text_sm().child(f.display_name.clone()))
                .child(if have {
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .text_color(theme.success)
                        .child(Icon::new(IconName::CircleCheck).small())
                        .child("Already installed")
                        .into_any_element()
                } else {
                    h_flex()
                        .gap_3()
                        .child(
                            install_button(
                                site_primary(Button::new("link-install"), m.modrinth.is_some(), cx).small().icon(IconName::Download),
                                if pack { "Install as a new instance" } else { "Install this file" },
                                pending,
                                progress,
                                hold,
                                cx,
                            )
                            .when(!ready, |b| b.disabled(true))
                                .on_click(cx.listener(move |this, _, w, cx| {
                                    this.link_file = None;
                                    this.install(m2.clone(), Some(f.clone()), w, cx)
                                })),
                        )
                        .when(!ready, |r| {
                            r.child(widgets::caption("Pick an instance under Install into", cx))
                        })
                        .into_any_element()
                })
                .into_any_element(),
        )
    }
}

/// An install button: "Installing…" with an hourglass while its install
/// runs, and off with the reason while the instance can't be changed.
/// Not a spinner: an animation redraws the whole column it sits in (every
/// card, or the panel and its description) each frame, which dropped
/// frames through every install. The tasks button shows it's moving.
fn install_button(b: Button, label: &'static str, pending: bool, progress: Option<f32>, hold: Option<&'static str>, cx: &App) -> Button {
    let fill = cx.theme().primary.opacity(0.28);
    b.label(match (pending, progress) {
        (true, Some(p)) => format!("Installing {}%", (p * 100.0).floor() as u32),
        (true, None) => "Installing…".to_string(),
        _ => label.to_string(),
    })
        .when(pending, |b| b.icon(IconName::Hourglass))
        // The download fills the button from the left.
        .when_some(progress.filter(|_| pending), |b, p| {
            let p = p.clamp(0.0, 1.0);
            b.bg(linear_gradient(
                90.,
                linear_color_stop(fill, p),
                linear_color_stop(gpui_kit::transparent_black(), p),
            ))
        })
        .disabled(pending || hold.is_some())
        .when_some(hold.filter(|_| !pending), |b, why| b.tooltip(why))
}

/// An icon and a value, for a project's stats, colored by what it counts
/// so a glance finds the downloads (blue), last update (green) or age
/// (yellow) without reading a row of gray.
fn stat(icon: IconName, text: impl Into<SharedString>, cx: &App) -> Div {
    let theme = cx.theme();
    let color = match icon {
        IconName::Download => theme.info,
        IconName::RefreshCw => theme.success,
        IconName::Clock => theme.warning,
        _ => theme.muted_foreground,
    };
    h_flex()
        .gap_1()
        .flex_none()
        .text_color(color.opacity(0.9))
        .child(Icon::new(icon).xsmall().text_color(color))
        .child(text.into())
}

/// What a click on a category does: filters Browse by it.
type OnCategory = Rc<dyn Fn(u64, &mut Window, &mut App)>;

/// Downloads, last update and categories; `full` adds when it came out
/// and words for each. With `on_cat`, each category is a link.
fn stats(m: &cf::Mod, full: bool, on_cat: Option<OnCategory>, cx: &App) -> Div {
    let updated = hopper::time::relative(&m.date_modified);
    let created = hopper::time::relative(&m.date_created);
    let cats: Vec<(u64, &str)> = m.categories.iter().take(3).map(|c| (c.id, c.name.as_str())).collect();
    let (muted, fg) = (cx.theme().muted_foreground, cx.theme().foreground);
    let n = cats.len();
    let names = cats.into_iter().enumerate().map(|(i, (id, name))| {
        let text = if i + 1 < n { format!("{name},") } else { name.to_string() };
        match &on_cat {
            Some(f) => {
                let f = f.clone();
                div()
                    .id(ElementId::Name(format!("cat:{}:{id}", m.id).into()))
                    .flex_none()
                    .cursor_pointer()
                    .hover(move |s| s.text_color(fg).underline())
                    .child(text)
                    .on_click(move |_, window, cx| {
                        // Not a click on the card: it doesn't open.
                        cx.stop_propagation();
                        f(id, window, cx)
                    })
                    .into_any_element()
            }
            None => div().flex_none().child(text).into_any_element(),
        }
    });
    let cats: Vec<AnyElement> = names.collect();
    let tip = |label: &str, v: String| if full { format!("{label} {v}") } else { v };
    h_flex()
        .gap_3()
        .text_xs()
        .min_w_0()
        .child(stat(
            IconName::Download,
            if full {
                format!("{} downloads", human_count(m.download_count))
            } else {
                human_count(m.download_count)
            },
            cx,
        ))
        .when(!updated.is_empty(), |r| {
            r.child(stat(IconName::RefreshCw, tip("Updated", updated.to_lowercase()), cx))
        })
        .when(full && !created.is_empty(), |r| {
            r.child(stat(IconName::Clock, tip("Created", created.to_lowercase()), cx))
        })
        .when(!cats.is_empty(), |r| {
            r.child(
                h_flex()
                    .gap_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_color(muted)
                    .child(Icon::new(IconName::Tag).xsmall().flex_none())
                    .children(cats),
            )
        })
}

/// Game versions, loaders, environment and license as small tags, with the
/// project id in a tooltip-free caption. `None` when nothing is known.
fn facts(m: &cf::Mod, target: Option<&Instance>, cx: &App) -> Option<Div> {
    let theme = cx.theme();
    let versions = m.game_versions();
    let loaders = m.loaders();
    // For an instance: the installed file's tags, else the newest file for
    // its version and loader. Authors tag each build separately.
    let installed = target
        .and_then(|i| i.addons.iter().find(|a| a.project_id == m.id && m.modrinth.is_none()))
        .map(|a| a.env.as_str())
        .filter(|e| !e.is_empty());
    let env = match (installed, target) {
        (Some(e), _) => Some(match e {
            "client" => "client",
            "server" => "server",
            _ => "both",
        }),
        (None, Some(i)) => {
            let loader = hopper::install::loader_of(i);
            m.side_for(&i.game_version,(loader != Loader::Any).then(|| loader.name()))
        }
        (None, None) => m.side(),
    };
    let side = match env {
        Some("client") => Some("Client"),
        Some("server") => Some("Server"),
        Some("both") => Some("Client & server"),
        _ => None,
    };
    let license = m.license_type.as_deref().filter(|l| !l.is_empty());
    if versions.is_empty() && loaders.is_empty() && side.is_none() && license.is_none() {
        return None;
    }
    const SHOW: usize = 5;
    let more = versions.len().saturating_sub(SHOW);
    let all: String = versions.join(", ");
    let tag = |t: String| Tag::secondary().xsmall().child(t);
    let group = |icon: IconName, tags: Vec<Tag>| {
        // Versions blue, loaders yellow, sides green; the rest muted.
        let color = match icon {
            IconName::Box => theme.info,
            IconName::Blocks => theme.warning,
            IconName::Monitor => theme.success,
            _ => theme.muted_foreground,
        };
        h_flex()
            .gap_1()
            .items_center()
            .flex_wrap()
            .child(Icon::new(icon).xsmall().text_color(color).flex_none())
            .children(tags)
    };
    let mut row = h_flex().gap_3().pt_1().flex_wrap().text_xs();
    if !versions.is_empty() {
        let mut tags: Vec<Tag> = versions.iter().take(SHOW).cloned().map(tag).collect();
        if more > 0 {
            tags.push(tag(format!("+{more}")));
        }
        row = row.child(group(IconName::Box, tags).id("fact-versions").tooltip(move |w, cx| {
            gpui_kit::component::tooltip::Tooltip::new(all.clone()).build(w, cx)
        }));
    }
    if !loaders.is_empty() {
        row = row.child(group(
            IconName::Blocks,
            loaders.into_iter().map(|l| tag(l.to_string())).collect(),
        ));
    }
    if let Some(s) = side {
        row = row.child(group(IconName::Monitor, vec![tag(s.to_string())]));
    }
    if let Some(l) = license {
        row = row.child(
            group(IconName::Scale, Vec::new()).child(
                div()
                    .text_color(theme.muted_foreground)
                    .child(l.to_string()),
            ),
        );
    }
    if m.modrinth.is_none() {
        row = row.child(group(IconName::Hash, Vec::new()).child(
            div().text_color(theme.muted_foreground).child(m.id.to_string()),
        ));
    }
    Some(row)
}

/// A project kind's icon and name, singular.
fn class_label(class: u32) -> (IconName, &'static str) {
    let icon = CLASSES
        .iter()
        .find(|(c, _, _)| *c == class)
        .map_or(IconName::Puzzle, |(_, i, _)| i.clone());
    let name = match class {
        cf::CLASS_MODPACKS => "Modpack",
        cf::CLASS_RESOURCE_PACKS => "Resource pack",
        cf::CLASS_SHADERS => "Shader",
        cf::CLASS_DATAPACKS => "Data pack",
        cf::CLASS_WORLDS => "World",
        _ => "Mod",
    };
    (icon, name)
}

/// "A", "A and B", "A, B and C".
fn and_list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [a] => a.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The root of the instance at position `ix` in the store's list.
fn instances_root_at(view: &Entity<BrowseView>, ix: usize, cx: &App) -> Option<PathBuf> {
    let store = view.read(cx).store.read(cx);
    store.instances().get(ix).map(|i| i.root.clone())
}

/// The project panel's default width, in rem.
pub const PANEL_W: f32 = 32.;
/// The narrowest and widest the panel's edge can be dragged, in rem; it
/// also never takes more than [`PANEL_MAX_SHARE`] of the window.
const PANEL_MIN: f32 = 26.;
const PANEL_MAX: f32 = 64.;
const PANEL_MAX_SHARE: f32 = 0.7;

/// Dragging the project panel's left edge.
pub(crate) struct PanelResize;

/// What a drag shows under the pointer: nothing.
struct Blank;

impl Render for Blank {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// A filled button in the site's color: the theme's primary for
/// CurseForge, its green for Modrinth.
fn site_primary(b: Button, modrinth: bool, _cx: &App) -> Button {
    widgets::site_button(b, modrinth)
}

/// A split button's changelog half to go with [`site_primary`].
fn site_tint(modrinth: bool) -> widgets::Tint {
    if modrinth { widgets::Tint::Success } else { widgets::Tint::Primary }
}

use widgets::site_color;

fn placeholder(modrinth: bool) -> &'static str {
    if modrinth {
        "Search Modrinth"
    } else {
        "Search CurseForge"
    }
}

/// A column of the Browse page (the filters, the results) as a view of
/// its own, drawn from the Browse view's state and redrawn whenever that
/// changes. A redraw that leaves Browse alone (a GIF in the description
/// moving on) reuses it instead of building every card again, which was
/// most of what such a redraw cost.
struct Part {
    browse: WeakEntity<BrowseView>,
    draw: DrawPart,
}

type DrawPart = fn(&mut BrowseView, Option<&Instance>, &mut Window, &mut Context<BrowseView>) -> AnyElement;

impl Part {
    /// The part, redrawn whenever `browse` changes.
    fn new(browse: &Entity<BrowseView>, draw: DrawPart, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            cx.observe(browse, |_, _, cx| cx.notify()).detach();
            Part { browse: browse.downgrade(), draw }
        })
    }
}

impl Render for Part {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let draw = self.draw;
        let part = self
            .browse
            .update(cx, |b, cx| {
                let target = b.target(cx);
                draw(b, target.as_ref(), window, cx)
            })
            .unwrap_or_else(|_| div().into_any_element());
        // A cached view is laid out on its own: this row gives the column
        // the flex parent it sizes itself by.
        h_flex().size_full().child(part)
    }
}

impl Render for BrowseView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.peek {
            let target = self.target(cx);
            return self.panel(target.as_ref(), window, cx).unwrap_or_else(|| div().into_any_element());
        }
        if let Some(q) = self.pending_query.take() {
            cx.defer_in(window, move |this, window, cx| {
                this.search.update(cx, |s, cx| s.set_value(q, window, cx))
            });
        }
        let view = cx.entity();
        let (filters, results, panel) = self
            .parts
            .get_or_insert_with(|| {
                (
                    Part::new(&view, |b, t, w, cx| b.filters(t, w, cx).into_any_element(), cx),
                    Part::new(&view, |b, t, _, cx| b.results(t, cx).into_any_element(), cx),
                    Part::new(
                        &view,
                        |b, t, w, cx| b.panel(t, w, cx).unwrap_or_else(|| div().into_any_element()),
                        cx,
                    ),
                )
            })
            .clone();
        let filters = AnyView::from(filters).cached(StyleRefinement::default().w_64().flex_none().h_full());
        let results = AnyView::from(results).cached(StyleRefinement::default().flex_1().min_w_0().h_full());
        // The panel too, so a redraw from above (a file flying into the
        // tasks button) doesn't rebuild the project and its description
        // every frame. Outside a peek it's open only with a project or an
        // author picked.
        let detail = (self.selected.is_some() || self.author.is_some()).then(|| {
            let w = self.panel_width(window, cx);
            AnyView::from(panel).cached(StyleRefinement::default().w(rems(w)).flex_none().h_full())
        });
        h_flex()
            .size_full()
            .items_stretch()
            // The site's color fades in from the top across the whole page,
            // under the filters and the project panel too, which let it
            // show through.
            .when(!self.bookmarked, |r| {
                r.bg(linear_gradient(
                    180.,
                    linear_color_stop(site_color(self.modrinth).opacity(0.07), 0.),
                    linear_color_stop(site_color(self.modrinth).opacity(0.), 0.55),
                ))
            })
            .on_drop(cx.listener(|this, _: &PanelResize, _, cx| this.end_resize(cx)))
            .child(filters)
            .child(results)
            .children(detail)
            .into_any_element()
    }
}

impl BrowseView {
    /// The project panel's width in rem: the live drag, else the saved
    /// width, kept within the limits and the window.
    pub fn panel_width(&self, window: &Window, cx: &App) -> f32 {
        let w = self.drag_width.unwrap_or(self.store.read(cx).settings().browse_panel_width);
        let window_w = window.viewport_size().width / window.rem_size();
        w.clamp(PANEL_MIN, PANEL_MAX).min(window_w * PANEL_MAX_SHARE).max(PANEL_MIN.min(window_w))
    }

    /// The panel's edge was let go: keep the new width.
    pub(crate) fn end_resize(&mut self, cx: &mut Context<Self>) {
        let Some(w) = self.drag_width.take() else { return };
        self.save_panel_width(w, cx);
    }

    fn save_panel_width(&mut self, w: f32, cx: &mut Context<Self>) {
        let w = w.clamp(PANEL_MIN, PANEL_MAX);
        self.store.update(cx, |s, cx| s.update_settings(|st| st.browse_panel_width = w, cx));
        cx.notify();
    }

    /// The project panel down the right side, when one is open.
    fn panel(
        &mut self,
        target: Option<&Instance>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let peek = self.peek;
        let width = self.panel_width(window, cx);
        let dragging = self.drag_width.is_some();
        // Moves are seen during the capture phase wherever the pointer is;
        // the width is measured from the window's right edge.
        let drag_move = cx.listener(|this, e: &DragMoveEvent<PanelResize>, window, cx| {
            let right = window.viewport_size().width;
            this.drag_width = Some((right - e.event.position.x) / window.rem_size());
            cx.notify();
        });
        let reset = cx.listener(|this, e: &ClickEvent, _, cx| {
            if e.click_count() == 2 {
                this.drag_width = None;
                this.save_panel_width(PANEL_W, cx);
            }
        });
        let drop = cx.listener(|this, _: &PanelResize, _, cx| this.end_resize(cx));
        // A peek closes on a click anywhere else (not in a dialog it opened),
        // on release and after the click's own handlers: a click on another
        // mod swaps the panel's project instead of closing it first.
        fn in_dialog(window: &mut Window, cx: &mut App) -> bool {
            window.has_active_dialog(cx) || window.has_active_sheet(cx)
        }
        let arm = cx.listener(move |this, e: &MouseDownEvent, window, cx| {
            if in_dialog(window, cx) {
                return;
            }
            if e.button == MouseButton::Left {
                this.blur_armed = Some(this.peeks);
            } else {
                this.selected = None;
                this.error = None;
                cx.notify();
            }
        });
        let blur = cx.listener(move |this, _: &MouseUpEvent, window, cx| {
            let Some(at) = this.blur_armed.take() else { return };
            if in_dialog(window, cx) {
                return;
            }
            cx.defer_in(window, move |this, _, cx| {
                if this.peeks == at {
                    this.selected = None;
                    this.error = None;
                    cx.notify();
                }
            });
        });
        let frame = |id: ElementId, body: AnyElement| {
            div()
                .when(peek, |d| d.on_mouse_down_out(arm).on_mouse_up_out(MouseButton::Left, blur))
                .relative()
                .w(rems(width))
                .flex_none()
                .h_full()
                .border_l_1()
                .border_color(theme.border)
                // In Browse, the page's site color shows through.
                .bg(if peek { theme.sidebar } else { theme.sidebar.opacity(0.6) })
                .on_drag_move(drag_move)
                .on_drop(drop)
                .child(widgets::slide_in(id, div().size_full().child(body)))
                .child(
                    div()
                        .id("panel-resize")
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(rems(-0.25))
                        .w(rems(0.5))
                        .cursor_col_resize()
                        .when(dragging, |d| d.bg(theme.primary.opacity(0.5)))
                        .hover(|s| s.bg(theme.primary.opacity(0.35)))
                        .on_click(reset)
                        .on_drag(PanelResize, |_, _, _, cx| cx.new(|_| Blank)),
                )
                .into_any_element()
        };
        if let Some(name) = self.author.as_ref().map(|a| a.name.clone()) {
            let body = self.author_panel(target, cx);
            return Some(frame(ElementId::Name(format!("author:{name}").into()), body));
        }
        if let Some(m) = self.selected.clone() {
            if let (0, Some(Load::Ready(d))) = (self.detail_tab, self.detail.get(&m.id)) {
                let (id, segments, images) = (m.id, d.segments.clone(), d.images.clone());
                self.desc.update(cx, |v, _| v.show(id, segments, images));
            }
            let body = self.detail(&m, target, cx);
            return Some(frame(ElementId::Name(format!("detail:{}", m.id).into()), body));
        }
        // A peek whose project didn't load says why.
        let e = self.error.clone().filter(|_| self.peek)?;
        Some(frame(
            "detail:error".into(),
            v_flex()
                .p_5()
                .gap_3()
                .child(div().text_sm().text_color(theme.danger).child(e))
                .child(
                    h_flex().child(
                        Button::new("close-peek")
                            .small()
                            .outline()
                            .label("Close")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.error = None;
                                cx.notify();
                            })),
                    ),
                )
                .into_any_element(),
        ))
    }
}
