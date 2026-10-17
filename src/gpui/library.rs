//! The Library page: instances on the leading side, the picked instance's
//! header and tabs in the work area.

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
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex,
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

/// Hero heights in rem: on most tabs, and the taller one on Overview.
const HERO: f32 = 13.0;
const HERO_TALL: f32 = 17.5;

#[derive(Clone, Copy, PartialEq, Eq)]
enum InstTab {
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
    const ALL: [InstTab; 12] = [
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

    fn label(self) -> &'static str {
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

    /// The addon class a tab lists, for the tabs that list addons.
    fn class(self) -> Option<u32> {
        match self {
            InstTab::Mods => Some(cf::CLASS_MODS),
            InstTab::ResourcePacks => Some(cf::CLASS_RESOURCE_PACKS),
            InstTab::Shaders => Some(cf::CLASS_SHADERS),
            _ => None,
        }
    }
}

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
    /// Compare a server with its pack in Server sync.
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
    /// Blurred pack art per icon file (None while it's being made).
    blurs: HashMap<PathBuf, Option<PathBuf>>,
    /// Folder size per instance (None while it's being counted).
    sizes: HashMap<PathBuf, Option<u64>>,
    /// The work area's width in rem, as of the last render.
    detail_w: f32,
    addons: Entity<AddonsView>,
    server: Entity<SyncPanel>,
    worlds: Entity<WorldsView>,
    datapacks: Entity<crate::datapacks::DatapacksView>,
    shots: Entity<ScreenshotsView>,
    settings: Entity<InstanceSettings>,
    logs: Entity<LogsView>,
    perf: Entity<crate::perf_view::PerfView>,
    peers: Entity<PeersView>,
    search: Entity<InputState>,
    query: String,
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
                if let Some(f) = self.datapacks.read(cx).folder() {
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

    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search instances"));
        let addons = cx.new(|cx| AddonsView::new(store.clone(), window, cx));
        let server = cx.new(|cx| {
            let mut p = SyncPanel::new(store.clone(), window, cx);
            p.set_in_instance(true);
            p
        });
        let worlds = cx.new(|cx| WorldsView::new(store.clone(), window, cx));
        let datapacks = cx.new(|cx| crate::datapacks::DatapacksView::new(store.clone(), window, cx));
        let shots = cx.new(|cx| ScreenshotsView::new(store.clone(), window, cx));
        let settings = cx.new(|cx| InstanceSettings::new(store.clone(), window, cx));
        let logs = cx.new(|cx| LogsView::new(store.clone(), window, cx));
        let perf = cx.new(|cx| crate::perf_view::PerfView::new(store.clone(), window, cx));
        let peers = cx.new(|cx| PeersView::new(store.clone(), window, cx));
        let peek = addons.read(cx).peek().clone();
        server.update(cx, |s, _| s.set_peek(peek.clone()));
        let subs = vec![
            cx.observe(&peek, |_, _, cx| cx.notify()),
            cx.subscribe(&addons, |_, _, event, cx| match event {
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
            }),
            cx.subscribe(&worlds, |this, _, event, cx| this.content_event(event, cx)),
            cx.subscribe(&datapacks, |this, _, event, cx| this.content_event(event, cx)),
            cx.observe(&store, |this, _, cx| {
                this.ensure_selection(cx);
                cx.notify();
            }),
            cx.subscribe_in(&search, window, |this, state, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.query = state.read(cx).value().trim().to_lowercase();
                    cx.notify();
                }
            }),
        ];
        let first_tab = if store.read(cx).settings().instance_opens_mods {
            InstTab::Mods
        } else {
            InstTab::Overview
        };
        let mut view = Self {
            drag_width: None,
            folded: Default::default(),
            store,
            selected: None,
            tab: first_tab,
            tab_from: first_tab,
            tab_moved: None,
            tab_serial: 0,
            tab_bounds: Default::default(),
            blurs: HashMap::new(),
            sizes: HashMap::new(),
            detail_w: 80.0,
            addons,
            server,
            worlds,
            datapacks,
            shots,
            settings,
            logs,
            perf,
            peers,
            search,
            query: String::new(),
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
            p.peek_project(root.clone(), None, cx);
            p.peek_project(root, Some(project), cx);
        });
        cx.notify();
    }

    fn set_tab(&mut self, tab: InstTab, cx: &mut Context<Self>) {
        if tab != self.tab {
            self.tab_from = self.tab;
            self.tab_moved = Some(Instant::now());
            self.tab_serial += 1;
        }
        self.tab = tab;
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
            ContentEvent::Find { text } => cx.emit(LibraryEvent::Find {
                root,
                text: text.clone(),
            }),
            ContentEvent::Peek { project } => {
                let peek = self.addons.read(cx).peek().clone();
                peek.update(cx, |p, cx| p.peek_project(root, Some(*project), cx));
                cx.notify();
            }
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
            InstTab::Datapacks => {
                // A mod page left open on another tab doesn't follow here.
                let peek = self.addons.read(cx).peek().clone();
                peek.update(cx, |p, cx| p.peek_project(root.clone(), None, cx));
                self.datapacks.update(cx, |w, cx| w.show_for(root, cx));
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
            if !self.query.is_empty() && !inst.name.to_lowercase().contains(&self.query) {
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
                        .cursor_pointer()
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
                    .child(
                        TabBar::new("sort")
                            .segmented()
                            .xsmall()
                            .selected_index(usize::from(recent))
                            .child(Tab::new().label("Name"))
                            .child(Tab::new().label("Recently played"))
                            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                let by_recent = *ix == 1;
                                this.store.update(cx, |s, cx| {
                                    s.update_settings(|st| st.library_by_recent = by_recent, cx)
                                });
                            })),
                    ),
            )
            .child(self.instance_list(cx))
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
                        .cursor_pointer()
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
            list = list.child(div().px_2().py_6().child(widgets::caption(
                if self.query.is_empty() {
                    "No instances yet. Make one or install a modpack."
                } else {
                    "No instance matches."
                },
                cx,
            )));
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
                        .cursor_pointer()
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
        let groups = store.groups();
        let group_now = store
            .settings()
            .groups
            .get(root.to_string_lossy().as_ref())
            .cloned();
        let name_for_menu = inst.name.clone();
        let menu_view = cx.entity();
        h_flex()
            .id(id("row", root))
            .group(group)
            .relative()
            .gap_3()
            .px_2()
            .py_1p5()
            .rounded(theme.radius_lg)
            .cursor_pointer()
            .when(picked, |r| {
                // A lit bar on the leading edge marks the open instance.
                r.bg(theme.list_active).child(
                    div()
                        .absolute()
                        .left_0()
                        .top_3()
                        .bottom_3()
                        .w(px(3.))
                        .rounded_full()
                        .bg(theme.primary)
                        .shadow(vec![
                            BoxShadow::new(px(0.), px(0.), theme.primary.opacity(0.7)).blur_radius(px(8.)),
                        ]),
                )
            })
            .when(!picked, |r| r.hover(|s| s.bg(theme.list_hover)))
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
                                r.child(Tag::success().xsmall().rounded_full().child("Playing"))
                            })
                            .when(pack_update, |r| {
                                r.child(Tag::primary().xsmall().rounded_full().child("Update"))
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_1p5()
                            .min_w_0()
                            .child(widgets::caption(sub, cx).text_xs())
                            .when(mod_updates > 0, |r| {
                                r.child(
                                    div()
                                        .flex_none()
                                        .text_xs()
                                        .text_color(theme.primary)
                                        .child(format!("· {mod_updates} mod updates")),
                                )
                            }),
                    ),
            )
            .context_menu(move |menu, _, _| {
                let (groups, group_now, name_for_menu) =
                    (groups.clone(), group_now.clone(), name_for_menu.clone());
                let (v1, v3, v4) = (menu_view.clone(), menu_view.clone(), menu_view.clone());
                let (vb, rb) = (menu_view.clone(), menu_root.clone());
                let (r1, r2, r3, r4) = (
                    menu_root.clone(),
                    menu_root.clone(),
                    menu_root.clone(),
                    menu_root.clone(),
                );
                menu.item(
                    PopupMenuItem::new("Play")
                        .icon(IconName::Play)
                        .disabled(playing)
                        .on_click(move |_, window, cx| {
                            v1.update(cx, |this, cx| this.play(&r1, window, cx))
                        }),
                )
                .item(
                    PopupMenuItem::new("Open folder")
                        .icon(IconName::FolderOpen)
                        .on_click(move |_, _, _| store::open_path(&r2)),
                )
                .item(
                    PopupMenuItem::new("Change icon…")
                        .icon(IconName::Image)
                        .on_click({
                            let (v, r) = (vb.clone(), rb.clone());
                            move |_, _, cx| {
                                let store = v.read(cx).store.clone();
                                change_icon(store, r.clone(), cx)
                            }
                        }),
                )
                .item(
                    PopupMenuItem::new("Create desktop shortcut")
                        .icon(IconName::ExternalLink)
                        .on_click(move |_, window, cx| {
                            if let Err(e) = hopper::shortcut::desktop_shortcut(&name_for_menu) {
                                window.push_notification(
                                    gpui_kit::component::notification::Notification::error(
                                        format!("{e:#}"),
                                    ),
                                    cx,
                                );
                            } else {
                                window.push_notification(
                                    gpui_kit::component::notification::Notification::success(
                                        "Shortcut created on the desktop.",
                                    ),
                                    cx,
                                );
                            }
                        }),
                )
                .separator()
                .label("Group")
                .map(|m| {
                    groups.iter().cloned().fold(m, |m, g| {
                        let (v, r, g2) = (vb.clone(), rb.clone(), g.clone());
                        m.item(
                            PopupMenuItem::new(g.clone())
                                .checked(group_now.as_deref() == Some(g.as_str()))
                                .on_click(move |_, _, cx| {
                                    let store = v.read(cx).store.clone();
                                    store.update(cx, |s, cx| s.set_group(&r, Some(g2.clone()), cx));
                                }),
                        )
                    })
                })
                .item(PopupMenuItem::new("New group…").icon(IconName::Plus).on_click({
                    let (v, r) = (vb.clone(), rb.clone());
                    move |_, window, cx| {
                        let store = v.read(cx).store.clone();
                        new_group_dialog(store, r.clone(), window, cx)
                    }
                }))
                .when(group_now.is_some(), |m| {
                    let (v, r) = (vb.clone(), rb.clone());
                    m.item(PopupMenuItem::new("Remove from group").on_click(move |_, _, cx| {
                        let store = v.read(cx).store.clone();
                        store.update(cx, |s, cx| s.set_group(&r, None, cx));
                    }))
                })
                .separator()
                .item(
                    PopupMenuItem::new("Duplicate")
                        .icon(IconName::Copy)
                        .on_click(move |_, _, cx| {
                            v3.update(cx, |this, cx| this.duplicate(&r3, cx))
                        }),
                )
                .separator()
                .item(
                    PopupMenuItem::new("Delete…")
                        .icon(IconName::Trash)
                        .disabled(playing)
                        .on_click(move |_, window, cx| {
                            let store = v4.read(cx).store.clone();
                            crate::dialogs::confirm_delete(store, r4.clone(), window, cx)
                        }),
                )
            })
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
        let launcher = hopper::launchers::Kind::from_key(&inst.source).map(|k| k.name());
        let root = inst.root.clone();
        let (r_play, r_folder, r_more) = (root.clone(), root.clone(), root.clone());
        // Running or being changed by a job: hold off other changes.
        let hold = store.hold_reason(&root);
        let busy = store.is_busy(&root);
        let pack_update = store.pack_update(inst).cloned().map(|f| {
            let (r1, r2, f1, f2) = (root.clone(), root.clone(), f.clone(), f.clone());
            h_flex()
                .gap_1()
                .child(
                    Button::new("update-pack")
                        .primary()
                        .icon(IconName::Download)
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
                        })),
                )
                .child(
                    Button::new("pack-changes")
                        .ghost()
                        .icon(IconName::ScrollText)
                        .tooltip("What changed in the pack")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let pid = this
                                .store
                                .read(cx)
                                .instance(&r2)
                                .and_then(|i| i.pack.as_ref().map(|p| p.project_id))
                                .unwrap_or(0);
                            crate::versions::open(
                                this.store.clone(),
                                r2.clone(),
                                pid,
                                true,
                                Some(f2.clone()),
                                window,
                                cx,
                            )
                        })),
                )
        });
        let has_pack = inst.pack.is_some();
        let sync_to = store
            .server_of(&inst.root)
            .map(|s| {
                // The server's state from the panel, when there is one.
                let state = store.panel_stats(&s.root).map(|p| {
                    if p.running && !p.starting {
                        format!("{} is running, {}/{} players. ", s.name, p.online, p.max)
                    } else {
                        format!("{}: {}. ", s.name, p.state().to_lowercase())
                    }
                });
                (s.root.clone(), s.name.clone(), state.unwrap_or_default())
            })
            .map(|(server, name, state)| {
                Button::new("sync-to")
                    .outline()
                    .icon(IconName::ArrowLeftRight)
                    .label(format!("Sync to {name}"))
                    .tooltip(format!("{state}Compare with the paired rig server"))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(LibraryEvent::Compare(server.clone()))
                    }))
            });
        let server_pack = inst
            .pack
            .as_ref()
            .filter(|p| p.server_pack_file_id != 0)
            .map(|p| (p.project_id, p.server_pack_file_id, p.name.clone()));
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
                .label(if busy { "Working…" } else { "Play" })
                .loading(busy)
                .disabled(busy)
                .when(busy, |b| b.tooltip("A task is changing this instance"))
                .on_click(cx.listener(move |this, _, window, cx| this.play(&r_play, window, cx)))
        };

        let theme = cx.theme();
        let play_color = if playing { theme.danger } else { theme.success };
        // First in the row on every tab, so it never moves.
        let play_small = Some(widgets::glow(play.min_w_24(), play_color, cx));
        let play_big: Option<Div> = None;
        let actions = h_flex()
                            .flex_wrap()
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
                            .child(
                                Button::new("more")
                                    .outline()
                                    .icon(IconName::Ellipsis)
                                    .tooltip("More")
                                    .dropdown_menu(move |menu, _, _| {
                                        let (v1, v2) = (view.clone(), view.clone());
                                        let (r1, r2) = (r_more.clone(), r_more.clone());
                                        let (v3, v4, v5) = (view.clone(), view.clone(), view.clone());
                                        let (r3, r4) = (r_more.clone(), r_more.clone());
                                        let v6 = view.clone();
                                        let r6 = r_more.clone();
                                        let (v7, r7) = (view.clone(), r_more.clone());
                                        menu.when(has_pack, |m| {
                                            m.item(
                                                PopupMenuItem::new("Pack versions")
                                                    .icon(IconName::Clock)
                                                    .on_click(move |_, window, cx| {
                                                        let store = v6.read(cx).store.clone();
                                                        let s = store.read(cx);
                                                        let (pid, upd) = s
                                                            .instance(&r6)
                                                            .map(|i| {
                                                                (
                                                                    i.pack.as_ref().map_or(0, |p| p.project_id),
                                                                    s.pack_update(i).cloned(),
                                                                )
                                                            })
                                                            .unwrap_or_default();
                                                        crate::versions::open(
                                                            store, r6.clone(), pid, true, upd, window, cx,
                                                        )
                                                    }),
                                            )
                                        })
                                        .item(
                                            PopupMenuItem::new("Export…")
                                                .icon(IconName::Upload)
                                                .on_click(move |_, window, cx| {
                                                    let store = v3.read(cx).store.clone();
                                                    crate::dialogs::open_export(store, r3.clone(), window, cx)
                                                }),
                                        )
                                        .item(
                                            PopupMenuItem::new("Share code…")
                                                .icon(IconName::Share2)
                                                .on_click({
                                                    let (v, r) = (v4.clone(), r4.clone());
                                                    move |_, window, cx| {
                                                        let store = v.read(cx).store.clone();
                                                        crate::dialogs::open_share(store, r.clone(), window, cx)
                                                    }
                                                }),
                                        )
                                        .when(server_pack.is_some(), |m| {
                                            let (pid, fid, name) = server_pack.clone().unwrap_or_default();
                                            m.item(
                                                PopupMenuItem::new("Download server pack…")
                                                    .icon(IconName::Server)
                                                    .on_click(move |_, _, cx| {
                                                        let store = v5.read(cx).store.clone();
                                                        crate::dialogs::save_server_pack(store, pid, fid, name.clone(), cx)
                                                    }),
                                            )
                                        })
                                        .item(
                                            // A pack with a server pack builds from it.
                                            PopupMenuItem::new(if server_pack.is_some() {
                                                "Host a server from its server pack…"
                                            } else {
                                                "Host a server on this PC…"
                                            })
                                                .icon(IconName::Server)
                                                .on_click({
                                                    let (v, r) = (v1.clone(), r1.clone());
                                                    move |_, window, cx| {
                                                        let store = v.read(cx).store.clone();
                                                        crate::host_dialog::open(store, Some(r.clone()), window, cx)
                                                    }
                                                }),
                                        )
                                        .separator()
                                        .item(PopupMenuItem::new(if launcher.is_some() { "Copy into Hopper" } else { "Duplicate" }).icon(IconName::Copy).on_click(
                                            move |_, _, cx| {
                                                v1.update(cx, |this, cx| this.duplicate(&r1, cx))
                                            },
                                        ))
                                        .item(
                                            PopupMenuItem::new("Repair")
                                                .icon(IconName::Wrench)
                                                .on_click(move |_, _, cx| {
                                                    v2.update(cx, |this, cx| this.repair(&r2, cx))
                                                }),
                                        )
                                        .item(
                                            PopupMenuItem::new("Backups…")
                                                .icon(IconName::Archive)
                                                .on_click(move |_, window, cx| {
                                                    let store = v7.read(cx).store.clone();
                                                    crate::backups_view::open(store, r7.clone(), window, cx)
                                                }),
                                        )
                                        .separator()
                                        .item(
                                            PopupMenuItem::new(match launcher {
                                                // Deleting here would break it in its launcher.
                                                Some(l) => format!("Delete it in {l}"),
                                                None => "Delete…".to_string(),
                                            })
                                                .icon(IconName::Trash)
                                                .disabled(playing || launcher.is_some())
                                                .on_click(move |_, window, cx| {
                                                    let store = v4.read(cx).store.clone();
                                                    crate::dialogs::confirm_delete(store, r4.clone(), window, cx)
                                                }),
                                        )

                                    }),
                            );
        self.hero(inst, icon, playing, actions, play_big, cx)
    }

    /// The instance's header: its art large over a blurred copy of itself
    /// (made once, off the UI thread), state pills, name, actions and a
    /// few figures. Taller on Overview, with the big Play button.
    fn hero(
        &self,
        inst: &Instance,
        icon: Option<PathBuf>,
        playing: bool,
        actions: Div,
        play_big: Option<Div>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let store = self.store.read(cx);
        let theme = cx.theme();
        let glass = widgets::glass(cx);
        let dark = theme.mode.is_dark();
        let overview = self.tab == InstTab::Overview;

        // State pills.
        let mut pills: Vec<Div> = Vec::new();
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
            pills.push(widgets::pill(format!("Running · {up}"), theme.success, true));
        } else if playing {
            pills.push(widgets::pill("Running", theme.success, true));
        }
        if store.is_busy(&inst.root) {
            pills.push(widgets::pill("Working", theme.warning, true));
        }
        if store.pack_update(inst).is_some() {
            pills.push(widgets::pill("Pack update", theme.primary, false));
        }
        if let Some(store::Updates::Ready(u)) = store.updates(&inst.root)
            && !u.is_empty()
        {
            let n = u.len();
            pills.push(widgets::pill(
                format!("{n} mod update{}", if n == 1 { "" } else { "s" }),
                theme.info,
                false,
            ));
        }
        // Another launcher's instance says whose it is.
        if let Some(k) = hopper::launchers::Kind::from_key(&inst.source) {
            pills.push(widgets::pill(k.name(), theme.accent_foreground, false));
        }
        pills.push(widgets::pill(
            match &inst.pack {
                Some(p) if hopper::modrinth::is_mr(p.project_id) => "Modrinth",
                Some(_) => "CurseForge",
                None => "Custom",
            },
            theme.muted_foreground,
            false,
        ));

        // Minecraft, loader, pack version.
        let fact = |icon: IconName, text: String| {
            h_flex()
                .flex_none()
                .gap_1p5()
                .child(Icon::new(icon).xsmall().text_color(theme.muted_foreground.opacity(0.8)))
                .child(text)
        };
        let facts = h_flex()
            .gap_4()
            .min_w_0()
            .overflow_hidden()
            .text_sm()
            .text_color(theme.muted_foreground)
            .child(fact(IconName::Box, format!("Minecraft {}", inst.game_version)))
            .child({
                let f = fact(IconName::Blocks, store::loader_label(&inst.loader));
                if inst.loader.is_empty() {
                    f.into_any_element()
                } else {
                    // Opens the loader's builds and changelog in Settings.
                    let root = inst.root.clone();
                    let fg = theme.foreground;
                    f.id("hero-loader")
                        .cursor_pointer()
                        .hover(move |s| s.text_color(fg))
                        .tooltip(|window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new("Loader versions and changelog")
                                .build(window, cx)
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_tab(InstTab::Settings, cx);
                            let root = root.clone();
                            this.settings.update(cx, |s, cx| s.show_loader_log(root, cx));
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
                r.child(fact(IconName::Tag, v))
            });

        // Figures, on the trailing side.
        let tiles = {
            let mods = inst.addons.iter().filter(|a| in_class(a, cf::CLASS_MODS)).count();
            let time = match store::played(inst.play_seconds) {
                Some(t) => widgets::stat_tile(t, "Played", cx),
                None => widgets::stat_tile(
                    inst.last_played
                        .as_deref()
                        .map(hopper::time::relative)
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "Never".into()),
                    "Last played",
                    cx,
                ),
            };
            let size = match self.sizes.get(&inst.root) {
                Some(Some(n)) => Some(widgets::stat_tile(hopper::instance_ops::human_bytes(*n), "Size", cx)),
                _ => None,
            };
            // As many as fit beside the actions.
            let room = match self.detail_w {
                w if w >= 74.0 => 3,
                w if w >= 58.0 => 2,
                _ => 0,
            };
            h_flex()
                .flex_none()
                .gap_2p5()
                .children(
                    [Some(time), Some(widgets::stat_tile(mods.to_string(), "Mods", cx)), size]
                        .into_iter()
                        .flatten()
                        .take(room),
                )
        };

        // The art: large, rounded, lifted off the backdrop.
        let (art_to, art_from) = if overview { (9.5, 7.5) } else { (7.5, 9.5) };
        let (h_to, h_from) = if overview { (HERO_TALL, HERO) } else { (HERO, HERO_TALL) };
        let grow = self.moving() && (self.tab_from == InstTab::Overview) != overview;
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
        let art = if grow {
            art.with_animation(
                ElementId::Name(format!("hero-art:{}", self.tab_serial).into()),
                Animation::new(MOTION).with_easing(ease_out_quint()),
                move |el, t| el.size(rems(art_from + (art_to - art_from) * t)),
            )
            .into_any_element()
        } else {
            art.size(rems(art_to)).into_any_element()
        };

        let content = h_flex()
            .absolute()
            .inset_0()
            .items_end()
            .gap_6()
            .px_8()
            .pb_6()
            .child(art)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .child(h_flex().gap_1p5().flex_wrap().children(pills))
                    .child(
                        div()
                            .text_size(rems(if overview { 2.4 } else { 1.9 }))
                            .font_weight(FontWeight::EXTRA_BOLD)
                            .line_height(relative(1.15))
                            .truncate()
                            .child(inst.name.clone()),
                    )
                    .child(facts)
                    .child(actions.pt_2()),
            )
            .child(tiles)
            .children(play_big);

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
        if grow {
            hero.with_animation(
                ElementId::Name(format!("hero-h:{}", self.tab_serial).into()),
                Animation::new(MOTION).with_easing(ease_out_quint()),
                move |el, t| el.h(rems(h_from + (h_to - h_from) * t)),
            )
            .into_any_element()
        } else {
            hero.h(rems(h_to)).into_any_element()
        }
    }

    /// The tab strip, with an underline that slides from the last tab to
    /// the picked one. Tabs report where they were laid out; the underline
    /// is placed from that (one frame late the very first time).
    fn tabs(&self, inst: &Instance, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let pos = |tab: InstTab| InstTab::ALL.iter().position(|t| *t == tab).unwrap_or(0);
        let (to, from) = (pos(self.tab), pos(self.tab_from));
        let items = InstTab::ALL.iter().enumerate().map(|(i, t)| {
            let active = *t == self.tab;
            let count = t
                .class()
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
                .cursor_pointer()
                .text_color(if active { theme.foreground } else { theme.muted_foreground })
                .hover(|s| s.text_color(theme.foreground))
                .child(
                    Icon::new(t.icon())
                        .small()
                        .when(active, |i| i.text_color(theme.primary)),
                )
                .child(t.label())
                .when_some(count, |d, n| {
                    d.child(
                        div()
                            .px_1p5()
                            .rounded_full()
                            .text_xs()
                            .bg(if active {
                                theme.primary.opacity(0.15)
                            } else {
                                theme.foreground.opacity(0.06)
                            })
                            .text_color(if active { theme.primary } else { theme.muted_foreground })
                            .child(n.to_string()),
                    )
                })
                .on_click(cx.listener(move |this, _, _, cx| this.set_tab(tab, cx)))
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
        let place = |i: usize| laid.get(i).copied();
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
        div()
            .flex_none()
            .px_6()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .id("inst-tabs")
                    .overflow_x_scroll()
                    .child(div().relative().child(strip).children(underline)),
            )
            .into_any_element()
    }

    /// Overview: the page of the modpack the instance came from, from
    /// CurseForge or Modrinth.
    fn overview_body(&self, inst: &Instance, cx: &mut Context<Self>) -> AnyElement {
        let store = self.store.read(cx);
        let body = match inst.pack.as_ref().filter(|p| p.project_id != 0) {
            Some(p) => {
                let page = store.pack_page(p.project_id);
                let icon = match page {
                    Some(store::PackPage::Ready(m, ..)) => m.icon().and_then(|u| store.image(u)),
                    _ => None,
                };
                widgets::pack_page(page, icon, &self.store, cx)
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
            None if self.tab == InstTab::Datapacks => cached(self.datapacks.clone()),
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
        let (store, view) = (self.store.clone(), cx.entity());
        let pick = std::rc::Rc::new(move |ix: usize, _: &mut Window, cx: &mut App| {
            if let Some((s, _)) = servers.get(ix) {
                let (s, r) = (s.clone(), root.clone());
                store.update(cx, |st, cx| st.set_pairing(&s, &r, cx));
                view.update(cx, |v, cx| v.want_tab_images(cx));
            }
        });
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .child(widgets::caption(
                "This pack isn't paired with a rig server yet.",
                cx,
            ))
            .child(div().w_64().child(widgets::picker(
                "pair-server",
                "Pick its server".into(),
                items,
                cx.theme().font_size,
                pick,
            )))
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
        let pending = self.pending_banner(&inst, cx);
        let tabs = self.tabs(&inst, cx);
        let body = self.tab_body(&inst, cx);
        let anim = format!("{}:{}", inst.root.display(), self.tab.label());
        v_flex()
            .size_full()
            .child(widgets::enter(
                id("detail", &inst.root),
                div().flex_none().child(header),
            ))
            .children(pending)
            .child(tabs)
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
