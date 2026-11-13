//! The instance's Configs tab and the side panel a mod's config button
//! opens. Both show the shared config view; the tab adds the list of mods
//! with config files ("Changed by you", "All mods", "Not matched to a
//! mod"), filters, and (for an instance linked to a server) a compare with
//! the server's copies.
//!
//! Search works like the game's own config menus: the box on top finds
//! mods by name, and the open mod has its own box for its settings.
//! Settings of other mods that mention the words wait in a folded group
//! under the mods, for "which mod changes X" questions.
//!
//! A server's Configs tab is the same tab: its files are read from the
//! server folder on this PC, or through the panel of the Hopper Server it
//! runs on.

use crate::config_view::{self, ConfigView, ConfigViewEvent, Host, InstIndex, ServerSide, Source};
use crate::store::Store;
use crate::widgets::{self, Clickable as _};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::configindex::Side;
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// A mod's name and icon from the instance (or for a server, its index), by
/// its jar's file name.
fn mod_info(store: &Store, root: &Path, key: &str, idx: Option<&InstIndex>) -> (String, Option<PathBuf>, u64) {
    if let Some(i) = idx.filter(|i| i.server) {
        return match i.names.get(key) {
            Some((n, icon)) => (if n.is_empty() { key.trim_end_matches(".jar").to_string() } else { n.clone() }, store.image(icon).map(Path::to_path_buf), 0),
            None => (key.trim_end_matches(".jar").to_string(), None, 0),
        };
    }
    let a = store.instance(root).and_then(|i| i.addons.iter().find(|a| a.file_name == key));
    match a {
        Some(a) => (a.name.clone(), store.image(&a.icon).map(Path::to_path_buf), a.project_id),
        None => (key.trim_end_matches(".jar").to_string(), None, 0),
    }
}

/// Who a list entry is: a mod, or a file no mod matched.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum Pick {
    Mod(String),
    File(String),
}

// ---- the side panel ---------------------------------------------------------------------

const PANEL: widgets::PanelSpec = widgets::PanelSpec::new("config.panel", 30., 24., 48.).trailing();

pub enum ConfigPanelEvent {
    Close,
    /// Open the Configs tab at this mod.
    OpenInTab(String),
}

impl EventEmitter<ConfigPanelEvent> for ConfigPanel {}

/// One mod's config beside the Mods tab, from its config button.
pub struct ConfigPanel {
    store: Entity<Store>,
    view: Entity<ConfigView>,
    panel: widgets::SidePanel,
    /// The mod (its jar's file name).
    pub key: String,
    _sub: Subscription,
}

impl ConfigPanel {
    #[allow(clippy::too_many_arguments)]
    pub fn new(store: Entity<Store>, root: PathBuf, key: String, name: String, icon: Option<PathBuf>, files: Vec<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let jar = key.clone();
        let view = cx.new(|cx| {
            let mut v = ConfigView::new(store.clone(), root, Host::Panel, name, icon, files, None, None, window, cx);
            v.jar = Some(jar);
            v
        });
        let k = key.clone();
        let sub = cx.subscribe(&view, move |_, _, e: &ConfigViewEvent, cx| match e {
            ConfigViewEvent::Close => cx.emit(ConfigPanelEvent::Close),
            ConfigViewEvent::OpenInTab => cx.emit(ConfigPanelEvent::OpenInTab(k.clone())),
            ConfigViewEvent::Changed => cx.notify(),
        });
        Self { store, view, panel: widgets::SidePanel::new(PANEL), key, _sub: sub }
    }

    pub fn unsaved(&self, cx: &App) -> usize {
        self.view.read(cx).unsaved()
    }

    pub fn root(&self, cx: &App) -> PathBuf {
        self.view.read(cx).root().to_path_buf()
    }
}

impl Render for ConfigPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let content = div().size_full().bg(theme.background).border_l_1().border_color(theme.border).child(self.view.clone());
        widgets::side_panel("cfg-panel-edge", &self.panel, |t: &mut Self| &mut t.panel, &self.store, content, window, cx)
    }
}

// ---- the tab ---------------------------------------------------------------------------

/// The filter chips over the list.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Changed,
    Unsaved,
}

pub struct ConfigsTab {
    store: Entity<Store>,
    root: Option<PathBuf>,
    index: Option<Arc<InstIndex>>,
    search: Entity<InputState>,
    query: String,
    desc: bool,
    filter: Filter,
    side: Option<Side>,
    /// Settings of every mod that match the top search, per entry of the
    /// list, and the files they're in (the folded "other mods" group).
    hits: Option<HashMap<Pick, (usize, Vec<String>)>>,
    /// That group is unfolded.
    others_open: bool,
    search_serial: u64,
    picked: Option<Pick>,
    /// Open views by entry, so edits survive picking another mod.
    views: HashMap<Pick, Entity<ConfigView>>,
    /// The compare with the linked server, when it's on.
    compare: Option<Entity<crate::config_compare::CompareView>>,
    compare_on: bool,
    /// The mod list, built when something it shows changes, never by a
    /// scroll frame (it sorts every mod by name and counts every setting).
    cache: Option<ListCache>,
    /// Bumped whenever the list's inputs change.
    ver: u64,
    placeholder_total: Option<usize>,
    scroll: UniformListScrollHandle,
    /// The tab is a server's: where its files are, whether it runs.
    server: Option<ServerSide>,
    /// Mods whose files a server's panel is still sending.
    fetching: std::collections::HashSet<Pick>,
    /// On a Hopper Server: how many settings players' copies differ in,
    /// as its panel compares them (the server, the count, whose Hopper sent
    /// the copy, when).
    players: Option<(PathBuf, usize, String, i64)>,
    _subs: Vec<Subscription>,
}

/// One line of the mod list.
#[derive(Clone)]
enum Item {
    Head(SharedString),
    Row(Pick),
    /// The folded group's title: how many mods, how many settings.
    Others(usize, usize),
    /// A mod in that group.
    Hit(Pick),
}

/// Height of every line of the mod list, so only the visible ones are built.
const LINE_H: f32 = 1.9;

struct ListCache {
    /// What it was built from: the index it read (by address) and `ver`.
    key: (usize, u64),
    items: Rc<Vec<Item>>,
    changed_n: usize,
    unsaved_n: usize,
}

impl ConfigsTab {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).clean_on_escape().placeholder("Search mods"));
        let idx = config_view::indexes(cx);
        let subs = vec![
            cx.subscribe_in(&search, window, |this, s, e: &InputEvent, window, cx| match e {
                InputEvent::Change => {
                    this.query = s.read(cx).value().to_string();
                    this.ver += 1;
                    this.run_search(cx);
                }
                // Enter opens the first mod that matches, ready to search its settings.
                InputEvent::PressEnter { .. } => this.open_first(window, cx),
                _ => {}
            }),
            cx.observe(&idx, |this, _, cx| this.refresh_index(cx)),
        ];
        Self {
            store,
            root: None,
            index: None,
            search,
            query: String::new(),
            desc: false,
            filter: Filter::All,
            side: None,
            hits: None,
            others_open: false,
            search_serial: 0,
            picked: None,
            views: HashMap::new(),
            compare: None,
            compare_on: false,
            cache: None,
            ver: 0,
            placeholder_total: None,
            scroll: UniformListScrollHandle::new(),
            server: None,
            fetching: Default::default(),
            players: None,
            _subs: subs,
        }
    }

    pub fn show_for(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.server.is_some() {
            self.root = None;
            self.server = None;
        }
        self.show_root(root, cx);
    }

    /// Shows a server's configs (its Configs tab). Called on every frame
    /// the tab is drawn: only a new server or a new running state does
    /// anything.
    pub fn show_server(&mut self, root: PathBuf, side: ServerSide, cx: &mut Context<Self>) {
        let same_place = match (&self.server, &side.source) {
            (Some(ServerSide { source: Source::Local, .. }), Source::Local) => true,
            (Some(ServerSide { source: Source::Panel(a), .. }), Source::Panel(b)) => a == b,
            _ => false,
        };
        if same_place && self.root.as_ref() == Some(&root) {
            if self.server.as_ref().is_some_and(|s| s.running != side.running) {
                for v in self.views.values() {
                    v.update(cx, |v, cx| {
                        if let Some(s) = v.server.as_mut() {
                            s.running = side.running;
                        }
                        cx.notify();
                    });
                }
                self.server = Some(side);
                cx.notify();
            }
            return;
        }
        if !same_place {
            self.root = None;
        }
        if let Source::Panel(t) = &side.source {
            self.load_players(root.clone(), t.clone(), cx);
        }
        self.server = Some(side);
        self.show_root(root, cx);
    }

    /// Asks the panel how far players' copies of the shared configs are
    /// from the server's (a paired Hopper sends its copy now and then).
    fn load_players(&mut self, root: PathBuf, t: crate::panel_tabs::Target, cx: &mut Context<Self>) {
        self.players = None;
        cx.spawn(async move |this, cx| {
            let v = cx.background_spawn(async move { t.client.get(&t.sp("/configs/players")) }).await;
            this.update(cx, |this, cx| {
                let Ok(v) = v else { return };
                let n = v["differs"].as_u64().unwrap_or(0) as usize;
                if n > 0 && this.root.as_ref().is_none_or(|r| *r == root) {
                    let snap = &v["snapshot"];
                    this.players = Some((root, n, snap["name"].as_str().unwrap_or_default().to_string(), snap["at"].as_i64().unwrap_or(0)));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn show_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.root.as_ref() != Some(&root) {
            self.fetching.clear();
            // Unsaved edits of the old instance stay with its views only while it's shown.
            self.views.clear();
            self.picked = None;
            self.hits = None;
            self.compare = None;
            self.compare_on = false;
            self.index = None;
            self.ver += 1;
        }
        self.root = Some(root);
        self.refresh_index(cx);
    }

    /// Opens a mod (from its panel's "Open in Configs tab").
    pub fn pick_mod(&mut self, key: String, cx: &mut Context<Self>) {
        self.picked = Some(Pick::Mod(key));
        cx.notify();
    }

    fn refresh_index(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let store = self.store.clone();
        let idx = match self.server.as_ref().map(|s| s.source.clone()) {
            Some(src) => config_view::indexes(cx).update(cx, |i, cx| i.get_server(&root, &src, cx)),
            None => config_view::indexes(cx).update(cx, |i, cx| i.get(&root, &store, cx)),
        };
        if let Some(i) = idx.as_ref().filter(|i| i.server) {
            // The mods' icons, for the list.
            let icons: Vec<String> = i.names.values().map(|(_, icon)| icon.clone()).filter(|i| !i.is_empty()).collect();
            self.store.update(cx, |s, _| icons.iter().for_each(|i| s.want_image(i)));
        }
        if idx.is_some() {
            self.index = idx;
            self.ver += 1;
            if !self.query.trim().is_empty() {
                self.run_search(cx);
            }
        }
        cx.notify();
    }

    /// Searches every file off the UI thread, a moment after typing stops.
    fn run_search(&mut self, cx: &mut Context<Self>) {
        self.search_serial += 1;
        let serial = self.search_serial;
        let q = self.query.trim().to_string();
        if q.is_empty() {
            self.hits = None;
            self.ver += 1;
            cx.notify();
            return;
        }
        let (Some(root), Some(idx)) = (self.root.clone(), self.index.clone()) else { return };
        let desc = self.desc;
        let panel = match &self.server {
            Some(ServerSide { source: Source::Panel(t), .. }) => Some(t.clone()),
            _ => None,
        };
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(180)).await;
            if this.update(cx, |t, _| t.search_serial != serial).unwrap_or(true) {
                return;
            }
            let hits = cx
                .background_spawn(async move {
                    let rels: Vec<String> = idx.files.iter().map(|f| f.rel.clone()).collect();
                    let owner: HashMap<&str, &str> = idx.files.iter().filter_map(|f| Some((f.rel.as_str(), f.owner.as_deref()?))).collect();
                    let mut out: HashMap<Pick, (usize, Vec<String>)> = HashMap::new();
                    // On a Hopper Server its panel searches (it has the files).
                    let found: Vec<String> = match &panel {
                        Some(t) => crate::panel_tabs::http(&*t.client)
                            .and_then(|c| c.configs_search(t.id(), &q, desc, 2000))
                            .map(|v| v["hits"].as_array().into_iter().flatten().filter_map(|h| h["rel"].as_str().map(str::to_string)).collect())
                            .unwrap_or_default(),
                        None => hopper::configindex::search(&root, &rels, &q, desc, 20_000).into_iter().map(|h| h.rel).collect(),
                    };
                    for rel in found {
                        let pick = match owner.get(rel.as_str()) {
                            Some(o) => Pick::Mod(o.to_string()),
                            None => Pick::File(rel.clone()),
                        };
                        let e = out.entry(pick).or_default();
                        e.0 += 1;
                        if !e.1.contains(&rel) {
                            e.1.push(rel);
                        }
                    }
                    out
                })
                .await;
            let _ = this.update(cx, |t, cx| {
                if t.search_serial == serial {
                    t.hits = Some(hits);
                    t.ver += 1;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn files_of(&self, p: &Pick) -> Vec<String> {
        match (p, &self.index) {
            (Pick::Mod(k), Some(i)) => i.mod_files(k),
            (Pick::File(r), _) => vec![r.clone()],
            _ => Vec::new(),
        }
    }

    fn changed_of(&self, p: &Pick) -> usize {
        match (p, &self.index) {
            (Pick::Mod(k), Some(i)) => i.mod_changed(k),
            (Pick::File(r), Some(i)) => i.changed.get(r).copied().unwrap_or(0),
            _ => 0,
        }
    }

    fn unsaved_of(&self, p: &Pick, cx: &App) -> usize {
        self.views.get(p).map_or(0, |v| v.read(cx).unsaved())
    }

    /// The view for `p`, made the first time it's shown.
    fn view_for(&mut self, p: &Pick, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<ConfigView>> {
        if let Some(v) = self.views.get(p) {
            return Some(v.clone());
        }
        let root = self.root.clone()?;
        let files = self.files_of(p);
        let (title, icon) = match p {
            Pick::Mod(k) => {
                let (n, i, _) = mod_info(self.store.read(cx), &root, k, self.index.as_deref());
                (n, i)
            }
            Pick::File(r) => (r.rsplit('/').next().unwrap_or(r).to_string(), None),
        };
        // Opened from the "other mods" group: on the file the hits are in.
        let first = self.hits.as_ref().and_then(|h| h.get(p)).and_then(|(_, f)| f.first().cloned()).filter(|_| self.is_other(p, cx));
        let (desc, changed, side) = (self.desc, self.filter == Filter::Changed, self.side);
        let all_files: Vec<String> = self.index.as_ref().map(|i| i.files.iter().map(|f| f.rel.clone()).collect()).unwrap_or_default();
        if let Some(side) = self.server.clone() {
            return self.server_view(p, side, root, title, icon, files, first, all_files, window, cx);
        }
        let view = cx.new(|cx| {
            let mut v = ConfigView::new(self.store.clone(), root, Host::Tab, title, icon, files, first, None, window, cx);
            v.desc = desc;
            v.changed_only = changed;
            v.side = side;
            v.all_files = all_files;
            if let Pick::Mod(k) = p {
                v.jar = Some(k.clone());
            }
            v
        });
        let sub = cx.subscribe(&view, |this, _, e: &ConfigViewEvent, cx| {
            if let ConfigViewEvent::Changed = e {
                this.ver += 1;
                cx.notify();
            }
        });
        self._subs.push(sub);
        self.views.insert(p.clone(), view.clone());
        Some(view)
    }

    /// A server's view for `p`. On a Hopper Server the mod's files come
    /// from its panel first: None until they're here.
    #[allow(clippy::too_many_arguments)]
    fn server_view(
        &mut self,
        p: &Pick,
        side: ServerSide,
        root: PathBuf,
        title: String,
        icon: Option<PathBuf>,
        files: Vec<String>,
        first: Option<String>,
        all_files: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<ConfigView>> {
        let (desc, changed, s) = (self.desc, self.filter == Filter::Changed, self.side);
        let source = side.source.clone();
        let want_files: Vec<String> = files.iter().filter(|f| hopper::configedit::Format::of(f).is_some()).cloned().collect();
        let make = move |this: &mut Self, texts: HashMap<String, Result<String, String>>, window: &mut Window, cx: &mut Context<Self>| {
            let store = this.store.clone();
            let view = cx.new(|cx| {
                let mut v = ConfigView::for_server(store, root.clone(), side.clone(), texts, title.clone(), icon.clone(), files.clone(), first.clone(), window, cx);
                v.desc = desc;
                v.changed_only = changed;
                v.side = s;
                v.all_files = all_files.clone();
                v
            });
            let sub = cx.subscribe(&view, |this, _, e: &ConfigViewEvent, cx| {
                if let ConfigViewEvent::Changed = e {
                    this.ver += 1;
                    cx.notify();
                }
            });
            this._subs.push(sub);
            view
        };
        let Source::Panel(t) = source else {
            let view = make(self, HashMap::new(), window, cx);
            self.views.insert(p.clone(), view.clone());
            return Some(view);
        };
        if !self.fetching.insert(p.clone()) {
            return None;
        }
        let pick = p.clone();
        let want = want_files;
        let for_root = self.root.clone();
        crate::panel_tabs::call_in(
            window,
            cx,
            &t,
            {
                let id = t.id().to_string();
                move |c| crate::panel_tabs::http(c)?.configs_parse(&id, &want)
            },
            move |this, r, window, cx| {
                this.fetching.remove(&pick);
                // Another server was picked meanwhile.
                if this.root != for_root {
                    return;
                }
                let texts = match r {
                    Ok(t) => t,
                    Err(e) => {
                        use gpui_kit::component::WindowExt as _;
                        window.push_notification(gpui_kit::component::notification::Notification::error(format!("Couldn't read the files from the panel: {e}")), cx);
                        return;
                    }
                };
                let view = make(this, texts, window, cx);
                this.views.insert(pick, view);
            },
        );
        None
    }

    /// The top search's words, lower-case.
    fn words(&self) -> Vec<String> {
        widgets::search_words(&self.query)
    }

    /// Whether `p`'s name (or its jar's file name) has every word of the top search.
    fn name_hit(&self, p: &Pick, names: &HashMap<String, String>) -> bool {
        let words = self.words();
        let hay = match p {
            Pick::Mod(k) => format!("{} {k}", names.get(k.as_str()).map_or("", String::as_str)).to_lowercase(),
            Pick::File(r) => r.to_lowercase(),
        };
        words.iter().all(|w| hay.contains(w.as_str()))
    }

    /// `p` is listed only because its settings mention the search.
    fn is_other(&self, p: &Pick, cx: &App) -> bool {
        if self.words().is_empty() {
            return false;
        }
        let names = self.names(cx);
        !self.name_hit(p, &names)
    }

    /// Every mod's name by its jar's file name.
    fn names(&self, cx: &App) -> HashMap<String, String> {
        if let Some(i) = self.index.as_ref().filter(|i| i.server) {
            return i.names.iter().filter(|(_, (n, _))| !n.is_empty()).map(|(k, (n, _))| (k.clone(), n.clone())).collect();
        }
        let root = self.root.as_deref().unwrap_or(Path::new(""));
        self.store.read(cx).instance(root).map(|i| i.addons.iter().map(|a| (a.file_name.clone(), a.name.clone())).collect()).unwrap_or_default()
    }

    /// Opens `p`; one from the "other mods" group opens on the settings
    /// that mention the search, in its own search box.
    fn open(&mut self, p: Pick, window: &mut Window, cx: &mut Context<Self>) {
        let other = self.is_other(&p, cx);
        self.picked = Some(p.clone());
        if other
            && let Some(v) = self.view_for(&p, window, cx)
        {
            let q = self.query.trim().to_string();
            v.update(cx, |v, cx| v.set_search(&q, window, cx));
        }
        cx.notify();
    }

    /// Enter in the top search: the first mod the list shows, with the
    /// cursor in its own search.
    fn open_first(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(idx) = self.index.clone() else { return };
        self.sync_list(&idx, cx);
        let first = self.cache.as_ref().and_then(|c| c.items.iter().find_map(|i| if let Item::Row(p) = i { Some(p.clone()) } else { None }));
        let Some(p) = first else { return };
        self.open(p.clone(), window, cx);
        if let Some(v) = self.view_for(&p, window, cx) {
            v.update(cx, |v, cx| v.focus_search(window, cx));
        }
    }

    fn set_filter(&mut self, f: Filter, cx: &mut Context<Self>) {
        self.filter = f;
        self.ver += 1;
        let changed = f == Filter::Changed;
        for v in self.views.values() {
            v.update(cx, |v, cx| {
                v.changed_only = changed;
                cx.notify();
            });
        }
        cx.notify();
    }

    fn set_side(&mut self, s: Option<Side>, cx: &mut Context<Self>) {
        self.side = s;
        self.ver += 1;
        for v in self.views.values() {
            v.update(cx, |v, cx| {
                v.side = s;
                cx.notify();
            });
        }
        cx.notify();
    }

    /// The entries of the list, grouped, as the filters and search leave them.
    /// With a search, the mods whose name matches come first; the ones
    /// whose settings mention it are the last group, "other mods".
    fn groups(&self, cx: &App) -> (Vec<(&'static str, Vec<Pick>)>, Vec<Pick>) {
        let Some(idx) = &self.index else { return (Vec::new(), Vec::new()) };
        let names = self.names(cx);
        let searching = !self.words().is_empty();
        let side_ok = |files: &[String]| self.side.is_none_or(|s| files.iter().any(|f| hopper::configindex::side_of(f) == s));
        let keep = |p: &Pick| {
            let files = self.files_of(p);
            if !side_ok(&files) {
                return false;
            }
            match self.filter {
                Filter::All => true,
                Filter::Changed => self.changed_of(p) > 0 || self.unsaved_of(p, cx) > 0,
                Filter::Unsaved => self.unsaved_of(p, cx) > 0,
            }
        };
        let sort_key = |p: &Pick| match p {
            Pick::Mod(k) => names.get(k.as_str()).map_or_else(|| k.trim_end_matches(".jar").to_lowercase(), |n| n.to_lowercase()),
            Pick::File(r) => r.clone(),
        };
        let mut mods: Vec<Pick> = idx.by_owner.keys().map(|k| Pick::Mod(k.clone())).filter(|p| keep(p) && (!searching || self.name_hit(p, &names))).collect();
        mods.sort_by_cached_key(sort_key);
        let unmatched: Vec<Pick> = idx.unmatched().into_iter().map(Pick::File).filter(|p| keep(p) && (!searching || self.name_hit(p, &names))).collect();
        let groups: Vec<(&'static str, Vec<Pick>)> = if searching {
            vec![("Mods", mods), ("Not matched to a mod", unmatched)]
        } else {
            let (changed, rest): (Vec<Pick>, Vec<Pick>) = mods.into_iter().partition(|p| self.changed_of(p) > 0 || self.unsaved_of(p, cx) > 0);
            vec![("Changed", changed), ("All mods", rest), ("Not matched to a mod", unmatched)]
        };
        let groups = groups.into_iter().filter(|(_, v)| !v.is_empty()).collect();
        // Most matches first: the mod that's most about it.
        let mut others: Vec<Pick> = self.hits.as_ref().map(|h| h.keys().filter(|p| keep(p) && !self.name_hit(p, &names)).cloned().collect()).unwrap_or_default();
        let count = |p: &Pick| self.hits.as_ref().and_then(|h| h.get(p)).map_or(0, |h| h.0);
        others.sort_by_cached_key(|p| (std::cmp::Reverse(count(p)), sort_key(p)));
        (groups, others)
    }

    /// Rebuilds the mod list when what it shows changed since the last frame.
    fn sync_list(&mut self, idx: &Arc<InstIndex>, cx: &App) {
        let key = (Arc::as_ptr(idx) as usize, self.ver);
        if self.cache.as_ref().is_some_and(|c| c.key == key) {
            return;
        }
        let (groups, others) = self.groups(cx);
        // The first entry when nothing is picked. Typing doesn't move the
        // pick: the open mod stays open until another is clicked (or Enter).
        if self.picked.is_none() {
            self.picked = groups.iter().flat_map(|(_, v)| v.first()).next().cloned();
        }
        let mut items = Vec::new();
        let mut others = Some(others).filter(|o| !o.is_empty());
        for (i, (title, picks)) in groups.iter().enumerate() {
            // The other mods come right under the mods, above loose files.
            if *title != "Mods" || i > 0 {
                self.push_others(&mut items, &mut others);
            }
            items.push(Item::Head(format!("{} · {}", title.to_uppercase(), picks.len()).into()));
            items.extend(picks.iter().take(1500).cloned().map(Item::Row));
        }
        self.push_others(&mut items, &mut others);
        self.cache = Some(ListCache {
            key,
            items: Rc::new(items),
            changed_n: idx.changed.values().sum(),
            unsaved_n: self.views.values().map(|v| v.read(cx).unsaved()).sum(),
        });
    }

    /// The folded "other mods" group, once, where `sync_list` puts it.
    fn push_others(&self, items: &mut Vec<Item>, others: &mut Option<Vec<Pick>>) {
        let Some(others) = others.take() else { return };
        let n: usize = others.iter().filter_map(|p| self.hits.as_ref()?.get(p)).map(|h| h.0).sum();
        items.push(Item::Others(others.len(), n));
        if self.others_open {
            items.extend(others.into_iter().take(1500).map(Item::Hit));
        }
    }

    /// Lines `range` of the mod list.
    fn list_lines(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(items) = self.cache.as_ref().map(|c| c.items.clone()) else { return Vec::new() };
        let theme = cx.theme().clone();
        range
            .filter_map(|i| items.get(i).cloned())
            .map(|it| match it {
                Item::Head(t) => div().w_full().h(rems(LINE_H)).flex_none().px_2().pb_1().flex().items_end().text_xs().font_semibold().text_color(theme.muted_foreground).child(t).into_any_element(),
                Item::Row(p) => self.list_row(&p, false, cx),
                Item::Hit(p) => self.list_row(&p, true, cx),
                Item::Others(mods, n) => self.others_head(mods, n, cx),
            })
            .collect()
    }

    /// "Settings in other mods mentioning …": click to unfold.
    fn others_head(&self, mods: usize, n: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let open = self.others_open;
        let tip = format!("{} setting{} in {mods} other mod{} mention “{}”", fmt_n(n), if n == 1 { "" } else { "s" }, if mods == 1 { "" } else { "s" }, self.query.trim());
        h_flex()
            .id("cfg-others")
            .gap_1p5()
            .px_2()
            .w_full()
            .h(rems(LINE_H))
            .flex_none()
            .rounded(theme.radius)
            .text_xs()
            .text_color(theme.muted_foreground)
            .clickable(cx)
            .hover(|s| widgets::row_hover(s, cx))
            .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall().flex_none())
            .child(div().flex_1().min_w_0().truncate().child("Settings in other mods"))
            .child(div().flex_none().child(fmt_n(n)))
            .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
            .on_click(cx.listener(|this, _, _, cx| {
                this.others_open = !this.others_open;
                this.ver += 1;
                cx.notify();
            }))
            .into_any_element()
    }

    fn list_row(&self, p: &Pick, other: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let root = self.root.clone().unwrap_or_default();
        let (name, icon, unmatched) = match p {
            Pick::Mod(k) => {
                let (n, i, _) = mod_info(self.store.read(cx), &root, k, self.index.as_deref());
                (n, i, false)
            }
            Pick::File(r) => (r.trim_start_matches("config/").to_string(), None, true),
        };
        let files = self.files_of(p);
        let (changed, unsaved) = (self.changed_of(p), self.unsaved_of(p, cx));
        let hits = self.hits.as_ref().and_then(|h| h.get(p)).map(|h| h.0).filter(|_| other);
        let right = match (hits, unsaved, changed) {
            (Some(h), _, _) => format!("{h} match{}", if h == 1 { "" } else { "es" }),
            (_, u, _) if u > 0 => format!("{u} unsaved"),
            (_, _, c) if c > 0 => c.to_string(),
            _ if files.len() == 1 => files[0].rsplit('.').next().unwrap_or("").to_string(),
            _ => format!("{} files", files.len()),
        };
        let selected = self.picked.as_ref() == Some(p);
        let shared = matches!(p, Pick::Mod(k) if self.store.read(cx).settings_shared_jar(&root, k));
        let pick = p.clone();
        let row = h_flex()
            .id(SharedString::from(format!("cfg-mod:{}{p:?}", if other { "o:" } else { "" })))
            .gap_2()
            .px_2()
            .w_full()
            .h(rems(LINE_H))
            .flex_none()
            .rounded(theme.radius)
            .text_sm()
            .clickable(cx)
            .child(if unmatched {
                div().size(rems(1.25)).flex_none().rounded(theme.radius).bg(theme.secondary).flex().items_center().justify_center().text_xs().text_color(theme.muted_foreground).child("?").into_any_element()
            } else {
                widgets::thumb(icon.as_deref(), rems(1.25), cx)
            })
            .child(div().flex_1().min_w_0().truncate().when(unmatched, |d| d.text_color(theme.muted_foreground)).child(if other { StyledText::new(name) } else { widgets::marked(name, &self.words(), cx) }))
            .when(shared, |d| d.child(widgets::chip(theme.success).child("Shared")))
            .when(changed > 0 || unsaved > 0, |d| d.child(div().size(px(6.)).flex_none().rounded_full().bg(theme.primary)))
            .child(div().flex_none().text_xs().text_color(theme.muted_foreground).child(right))
            .on_click(cx.listener(move |this, _, window, cx| this.open(pick.clone(), window, cx)));
        let row = if selected { widgets::row_selected(row, cx) } else { row.hover(|s| widgets::row_hover(s, cx)) };
        row.into_any_element()
    }
}

impl ConfigsTab {
    /// "Players' copies differ in 3 settings · from Alex's Hopper, sent 2 h
    /// ago · Push in the panel". Pushing is the panel owner's (or a
    /// manager's): a paired PC opens the panel's page to do it.
    fn players_line(&self, root: &Path, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (r, n, from, at) = self.players.as_ref()?;
        if r != root {
            return None;
        }
        let t = match &self.server {
            Some(ServerSide { source: Source::Panel(t), .. }) => t,
            _ => return None,
        };
        let theme = cx.theme().clone();
        let url = format!("{}/#/server/{}/configs", t.remote.base, hopper::hopperd::enc(t.id()));
        let mut said = format!("Players' copies differ in {n} setting{}", if *n == 1 { "" } else { "s" });
        let mut when = Vec::new();
        if !from.is_empty() {
            when.push(format!("from {from}'s Hopper"));
        }
        if *at > 0 {
            let c = hopper::time::clock(*at);
            when.push(format!("sent {} at {}", hopper::time::relative_secs(*at).to_lowercase(), c.get(..5).unwrap_or(&c)));
        }
        if !when.is_empty() {
            said = format!("{said} · {}", when.join(", "));
        }
        Some(
            h_flex()
                .mx_6()
                .mt_2()
                .px_3()
                .py_2()
                .gap_2()
                .rounded(theme.radius)
                .bg(theme.secondary)
                .text_sm()
                .child(Icon::new(IconName::Users).small().flex_none().text_color(theme.muted_foreground))
                .child(div().flex_1().min_w_0().child(said))
                .child(
                    gpui_kit::component::button::Button::new("cfg-push-panel")
                        .small()
                        .outline()
                        .icon(IconName::ExternalLink)
                        .label(format!("Push {n} in the panel"))
                        .tooltip("The panel's owner or a manager sends the server's values to players' Hopper from its Configs page")
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                )
                .into_any_element(),
        )
    }
}

impl Render for ConfigsTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let Some(root) = self.root.clone() else { return div().into_any_element() };
        let Some(idx) = self.index.clone() else {
            if let Some(e) = config_view::indexes(cx).read(cx).failed(&root) {
                return div().p_6().child(widgets::empty_state(IconName::TriangleAlert, "Can't read the config files", Some(e.into()), cx)).into_any_element();
            }
            return v_flex()
                .size_full()
                .p_6()
                .gap_2()
                .child(widgets::caption("Reading the config files…", cx))
                .child(widgets::skeleton_rows(8, rems(2.5), cx))
                .into_any_element();
        };
        self.sync_list(&idx, cx);
        let picked = self.picked.clone();
        let view = picked.as_ref().and_then(|p| self.view_for(p, window, cx));
        let (changed_n, unsaved_n) = self.cache.as_ref().map_or((0, 0), |c| (c.changed_n, c.unsaved_n));
        let mods_n = idx.by_owner.len();
        if self.placeholder_total != Some(mods_n) {
            self.placeholder_total = Some(mods_n);
            self.search.update(cx, |s, cx| s.set_placeholder(format!("Search {} mods", fmt_n(mods_n)), window, cx));
        }
        // A server compares with nothing; an instance with its linked server.
        let linked = if self.server.is_some() { None } else { self.store.read(cx).server_of(&root).cloned() };
        let running = self.server.as_ref().is_some_and(|s| s.running);

        let chips = h_flex()
            .gap_1p5()
            .child(widgets::toggle_chip("cfg-f-all", "All", self.filter == Filter::All, cx).on_click(cx.listener(|this, _, _, cx| this.set_filter(Filter::All, cx))))
            .child(widgets::toggle_chip("cfg-f-changed", format!("Changed {}", fmt_n(changed_n)), self.filter == Filter::Changed, cx).tooltip("Settings that aren't at the default their file states").on_click(cx.listener(|this, _, _, cx| this.set_filter(Filter::Changed, cx))))
            .child(widgets::toggle_chip("cfg-f-unsaved", format!("Unsaved {unsaved_n}"), self.filter == Filter::Unsaved, cx).on_click(cx.listener(|this, _, _, cx| this.set_filter(Filter::Unsaved, cx))));
        let sides: Vec<(Option<Side>, &str)> = vec![(None, "All"), (Some(Side::Client), "This PC"), (Some(Side::Common), "Both"), (Some(Side::Server), "Server decides")];
        let side_label = sides.iter().find(|(s, _)| *s == self.side).map_or("All", |(_, l)| l);
        let view_e = cx.entity();
        let side_pick = div().w(rems(11.)).child(widgets::picker(
            "cfg-side",
            format!("Side: {side_label}"),
            sides.iter().map(|(s, l)| (l.to_string(), *s == self.side)).collect(),
            window.rem_size(),
            Rc::new(move |i, _, cx| {
                let s = [None, Some(Side::Client), Some(Side::Common), Some(Side::Server)][i.min(3)];
                view_e.update(cx, |this, cx| this.set_side(s, cx));
            }),
        ));
        let toolbar = h_flex()
            .gap_2()
            .px_6()
            .py_2()
            .flex_wrap()
            .child(div().w(rems(24.)).child(widgets::search_input(&self.search).small().cleanable(true)))
            .child(
                widgets::toggle_chip("cfg-desc", "Also in descriptions", self.desc, cx)
                    .tooltip("Look in each setting's description too, in the open mod and in other mods")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.desc = !this.desc;
                        let d = this.desc;
                        for view in this.views.values() {
                            view.update(cx, |view, cx| {
                                view.desc = d;
                                cx.notify();
                            });
                        }
                        this.run_search(cx);
                    })),
            )
            .child(div().flex_1())
            .child(chips)
            .child(side_pick)
            .when_some(linked.clone(), |d, s| {
                d.child(
                    widgets::toggle_chip("cfg-compare", "Compare with server", self.compare_on, cx)
                        .tooltip(format!("Settings that differ from {}'s copies, side by side", s.name))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.compare_on = !this.compare_on;
                            cx.notify();
                        })),
                )
            });

        let n = self.cache.as_ref().map_or(0, |c| c.items.len());
        let list: AnyElement = if n == 0 {
            div()
                .p_2()
                .child(widgets::empty_state(
                    IconName::FileCog,
                    if !self.words().is_empty() { "No mods match" } else if self.filter != Filter::All { "Nothing here" } else { "No config files yet" },
                    if !self.words().is_empty() { Some("No mod's name or settings have those words.".into()) } else { (self.filter == Filter::All).then(|| "Most mods write theirs on the first launch.".into()) },
                    cx,
                ))
                .into_any_element()
        } else {
            div()
                .size_full()
                .relative()
                .p_2()
                .child(uniform_list("cfg-mods-list", n, cx.processor(|this, range: Range<usize>, _, cx| this.list_lines(range, cx))).track_scroll(&self.scroll).size_full())
                .vertical_scrollbar(&self.scroll)
                .into_any_element()
        };

        let right: AnyElement = if self.compare_on
            && let Some(server) = linked
        {
            let c = match &self.compare {
                Some(c) => c.clone(),
                None => {
                    let c = cx.new(|cx| crate::config_compare::CompareView::new(self.store.clone(), root.clone(), server.clone(), cx));
                    self.compare = Some(c.clone());
                    c
                }
            };
            let only = picked.as_ref().map(|p| self.files_of(p));
            c.update(cx, |c, cx| c.set_files(only, cx));
            c.into_any_element()
        } else {
            match view {
                Some(v) => v.into_any_element(),
                None => div().into_any_element(),
            }
        };

        v_flex()
            .size_full()
            .when(running, |d| {
                d.child(
                    h_flex()
                        .mx_6()
                        .mt_2()
                        .px_3()
                        .py_2()
                        .gap_2()
                        .rounded(theme.radius)
                        .bg(theme.secondary)
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(Icon::new(IconName::Info).small().flex_none())
                        .child("The server is running. Settings you save here apply when it restarts."),
                )
            })
            .children(self.players_line(&root, cx))
            .child(toolbar)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(div().id("cfg-mods").w(rems(17.)).flex_none().h_full().border_r_1().border_color(theme.border).child(list))
                    .child(div().flex_1().min_w_0().h_full().child(right)),
            )
            .into_any_element()
    }
}

fn fmt_n(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}
