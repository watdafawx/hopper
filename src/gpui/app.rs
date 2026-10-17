//! The window's root view: title bar with navigation, the current page and
//! the background-jobs strip.

use crate::browse::{BrowseEvent, BrowseView};
use crate::library::{LibraryEvent, LibraryView};
use crate::servers::{ServersEvent, ServersView, SyncPage};
use crate::settings_page::SettingsPage;
use crate::store::{Store, StoreEvent};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{
    ActiveTheme as _, Icon, Selectable as _, Sizable as _, StyledExt as _, TitleBar, WindowExt as _,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

gpui_kit::actions!(
    hopper,
    [
        Find,
        Refresh,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        GoLibrary,
        GoBrowse,
        GoServers,
        GoSync,
        PrevInstance,
        NextInstance,
        CommandPalette,
        OpenSettings
    ]
);

/// Where the window opens: where it was last, if that is still on a
/// display, else centered.
pub fn start_bounds(saved: Option<hopper::settings::SavedWindow>, cx: &App) -> WindowBounds {
    let fallback = || WindowBounds::Windowed(Bounds::centered(None, size(px(1440.), px(900.)), cx));
    let Some(w) = saved else { return fallback() };
    let b = Bounds::new(
        point(px(w.x), px(w.y)),
        size(px(w.width.max(960.)), px(w.height.max(600.))),
    );
    // Some of its title bar must be on a display, or it can't be moved back.
    let grab = Bounds::new(b.origin + point(px(40.), px(8.)), size(px(120.), px(20.)));
    let visible = cx.displays().iter().any(|d| d.bounds().intersects(&grab));
    match (visible, w.maximized) {
        (false, _) => fallback(),
        (true, true) => WindowBounds::Maximized(b),
        (true, false) => WindowBounds::Windowed(b),
    }
}

/// App-wide keys.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-f", Find, None),
        KeyBinding::new("f5", Refresh, None),
        KeyBinding::new("ctrl-=", ZoomIn, None),
        KeyBinding::new("ctrl-+", ZoomIn, None),
        KeyBinding::new("ctrl-shift-=", ZoomIn, None),
        KeyBinding::new("ctrl--", ZoomOut, None),
        KeyBinding::new("ctrl-0", ZoomReset, None),
        KeyBinding::new("ctrl-1", GoLibrary, None),
        KeyBinding::new("ctrl-2", GoBrowse, None),
        KeyBinding::new("ctrl-3", GoServers, None),
        KeyBinding::new("ctrl-4", GoSync, None),
        KeyBinding::new("ctrl-up", PrevInstance, None),
        KeyBinding::new("ctrl-down", NextInstance, None),
        KeyBinding::new("ctrl-k", CommandPalette, None),
        KeyBinding::new("ctrl-p", CommandPalette, None),
        KeyBinding::new("ctrl-,", OpenSettings, None),
    ]);
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Library,
    Browse,
    Servers,
    Sync,
    Settings,
}

impl Page {
    pub(crate) const ALL: [Page; 4] = [Page::Library, Page::Browse, Page::Servers, Page::Sync];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Page::Library => "Library",
            Page::Browse => "Browse",
            Page::Servers => "Servers",
            Page::Sync => "Server sync",
            Page::Settings => "Settings",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Page::Library => IconName::LibraryBig,
            Page::Browse => IconName::Compass,
            Page::Servers => IconName::Server,
            Page::Sync => IconName::ArrowLeftRight,
            Page::Settings => IconName::Settings,
        }
    }
}

pub struct AppView {
    pub(crate) store: Entity<Store>,
    pub(crate) page: Page,
    pub(crate) library: Entity<LibraryView>,
    pub(crate) browse: Entity<BrowseView>,
    servers: Entity<ServersView>,
    pub(crate) sync: Entity<SyncPage>,
    settings: Entity<SettingsPage>,
    focus: FocusHandle,
    /// The tasks button and its popover.
    tasks: Entity<crate::tasks::TasksButton>,
    /// Saves the window's place once it settles.
    _save_bounds: Option<Task<()>>,
    /// Restarts into a downloaded update once nothing is going on.
    _auto_restart: Option<Task<()>>,
    /// Jobs already reported, and servers already reported as crashed.
    told_jobs: std::collections::HashSet<u64>,
    told_crashes: std::collections::HashSet<std::path::PathBuf>,
    /// The Ctrl+K command palette, while open.
    pub(crate) palette: Option<crate::palette::Open>,
    _subscriptions: Vec<Subscription>,
}

impl AppView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = cx.new(Store::new);
        let library = cx.new(|cx| LibraryView::new(store.clone(), window, cx));
        let browse = cx.new(|cx| BrowseView::new(store.clone(), window, cx));
        let servers = cx.new(|cx| ServersView::new(store.clone(), window, cx));
        let sync = cx.new(|cx| SyncPage::new(store.clone(), window, cx));
        let settings = cx.new(|cx| SettingsPage::new(store.clone(), window, cx));
        let tasks = cx.new(|cx| crate::tasks::TasksButton::new(store.clone(), cx));
        let subs = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.observe(&tasks, |_, _, cx| cx.notify()),
            cx.subscribe_in(&store, window, |this, _, event, window, cx| match event {
                StoreEvent::GameStarted => {
                    if this.store.read(cx).settings().minimize_while_playing {
                        window.minimize_window();
                    }
                }
                StoreEvent::Schedule(msg, bad) => {
                    let n = if *bad {
                        gpui_kit::component::notification::Notification::warning(msg.clone())
                    } else {
                        gpui_kit::component::notification::Notification::info(msg.clone())
                    };
                    window.push_notification(n, cx);
                    if *bad && !window.is_window_active() {
                        crate::tray::notify("Hopper", msg, true);
                    }
                }
                StoreEvent::GameCrashed(name) => {
                    crate::tray::notify(
                        &format!("{name} crashed"),
                        "Open Hopper → Logs & crashes to see why.",
                        true,
                    );
                }
                StoreEvent::GamesEnded => {
                    if this.store.read(cx).settings().minimize_while_playing {
                        window.activate_window();
                    }
                }
                StoreEvent::PairRequest(req) => {
                    let Some(hub) = this.store.read(cx).peer_hub() else { return };
                    let req = req.clone();
                    // Don't jump in front of whatever the user is doing:
                    // the dialog waits in Hopper, and Windows says it's there.
                    if !window.is_window_active() {
                        crate::tray::notify(
                            "Pairing request",
                            &format!("{} wants to pair with this PC. Open Hopper to answer.", req.name),
                            false,
                        );
                    }
                    window.open_alert_dialog(cx, move |alert, _, _| {
                        let (h1, h2) = (hub.clone(), hub.clone());
                        alert
                            .title(format!("Pair with {}?", req.name))
                            .description(format!(
                                "From {}. Accept only if this code matches the one on their screen: {}. Once paired, they can read and write this PC's instance folders (mods, config…); replaced files are backed up.",
                                req.addr, req.code
                            ))
                            .confirm()
                            .ok_text("Accept")
                            .cancel_text("Decline")
                            .on_ok(move |_, _, _| {
                                h1.answer(true);
                                true
                            })
                            .on_cancel(move |_, _, _| {
                                h2.answer(false);
                                true
                            })
                    });
                }
                // The Performance tab listens for its own instance.
                StoreEvent::SessionSaved(_) => {}
                StoreEvent::Peek { root, project } => {
                    let (root, project) = (root.clone(), *project);
                    this.library.update(cx, |l, cx| l.peek(root, project, cx));
                    this.go(Page::Library, cx);
                }
                StoreEvent::Link(url) => {
                    window.activate_window();
                    let url = url.clone();
                    this.browse.update(cx, |b, cx| b.open_link(&url, cx));
                    this.go(Page::Browse, cx);
                }
            }),
            cx.subscribe(&library, |this, _, event, cx| match event {
                LibraryEvent::Browse { root, class, dest } => {
                    let (root, class, dest) = (root.clone(), *class, dest.clone());
                    this.browse
                        .update(cx, |b, cx| b.browse_for(root, class, dest, cx));
                    this.go(Page::Browse, cx);
                }
                LibraryEvent::Find { root, text } => {
                    let (root, text) = (root.clone(), text.clone());
                    this.browse.update(cx, |b, cx| {
                        b.browse_for(root, hopper::cf::CLASS_MODS, None, cx);
                        b.find(text, cx);
                    });
                    this.go(Page::Browse, cx);
                }
                LibraryEvent::OpenProject { root, project } => {
                    let (root, project) = (root.clone(), *project);
                    this.browse.update(cx, |b, cx| b.open_project(root, project, cx));
                    this.go(Page::Browse, cx);
                }
                LibraryEvent::Compare(server) => {
                    let server = server.clone();
                    this.sync.update(cx, |p, cx| p.compare(server, cx));
                    this.go(Page::Sync, cx);
                }
                LibraryEvent::BrowsePacks => {
                    this.browse
                        .update(cx, |b, cx| b.browse_class(hopper::cf::CLASS_MODPACKS, cx));
                    this.go(Page::Browse, cx);
                }
            }),
            cx.subscribe(&servers, |this, _, event, cx| match event {
                ServersEvent::Compare(root) => {
                    let root = root.clone();
                    this.sync.update(cx, |p, cx| p.compare(root, cx));
                    this.go(Page::Sync, cx);
                }
                ServersEvent::OpenSettings => this.open_settings_at("panel-url", cx),
            }),
            cx.subscribe(&browse, |this, _, event, cx| match event {
                BrowseEvent::OpenInstance(root) => {
                    let root = root.clone();
                    this.library.update(cx, |l, cx| l.open(root, cx));
                    this.go(Page::Library, cx);
                }
                BrowseEvent::OpenSettings => this.open_settings_at("cf-key", cx),
            }),
            cx.subscribe_in(&settings, window, |this, _, e: &crate::settings_page::SettingsEvent, window, cx| match e {
                crate::settings_page::SettingsEvent::RunSetup => {
                    crate::setup_wizard::open(this.store.clone(), cx.entity().downgrade(), false, window, cx)
                }
                crate::settings_page::SettingsEvent::BrowseSource(modrinth) => {
                    let modrinth = *modrinth;
                    this.browse.update(cx, |b, cx| b.set_source(modrinth, window, cx));
                }
            }),
        ];
        let mut view = Self {
            store,
            page: Page::Library,
            library,
            browse,
            servers,
            sync,
            settings,
            focus: cx.focus_handle(),
            tasks,
            _save_bounds: None,
            told_jobs: Default::default(),
            told_crashes: Default::default(),
            palette: None,
            _auto_restart: None,
            _subscriptions: subs,
        };
        view.apply_args(window, cx);
        view._auto_restart = Some(Self::auto_restart(window, cx));
        crate::setup_wizard::on_start(&view, window, cx);
        crate::whats_new::on_start(&view, window, cx);
        // settings.json that didn't parse was kept aside: say so.
        if let Some(e) = hopper::settings::Settings::load_error() {
            cx.defer_in(window, move |_, window, cx| {
                window.push_notification(gpui_kit::component::notification::Notification::warning(e), cx)
            });
        }
        // Closing with work in flight asks first.
        window.on_window_should_close(cx, |window, cx| quit_ok(window, cx));
        view.start_tray(window, cx);
        // "Follow Windows" follows it live.
        view._subscriptions.push(cx.observe_window_appearance(window, |this, _, cx| {
            let s = this.store.read(cx).settings();
            let forced = std::env::args().any(|a| a == "--theme");
            if s.theme_name.is_empty() && s.theme == 0 && !forced {
                let zoom = s.zoom;
                crate::theme::apply("", 0, zoom, cx);
            }
        }));
        // Remember where the window is, a moment after it stops moving.
        view._subscriptions.push(cx.observe_window_bounds(window, |this, window, cx| {
            // Not for dev runs at a given size or with the setup preview.
            if crate::dev_size().is_some() || std::env::args().any(|a| a == "--setup") {
                return;
            }
            let (b, maximized) = match window.window_bounds() {
                WindowBounds::Windowed(b) => (b, false),
                WindowBounds::Maximized(b) | WindowBounds::Fullscreen(b) => (b, true),
            };
            let saved = hopper::settings::SavedWindow {
                x: f32::from(b.origin.x),
                y: f32::from(b.origin.y),
                width: f32::from(b.size.width),
                height: f32::from(b.size.height),
                maximized,
            };
            let store = this.store.clone();
            this._save_bounds = Some(cx.spawn(async move |_, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(600))
                    .await;
                store
                    .update(cx, |s, cx| s.update_settings(|st| st.window = Some(saved), cx));
            }));
        }));
        crate::mcp::start(window, cx);
        view
    }

    /// `--page browse|servers|sync`, `--search text` and `--server name`,
    /// for scripted runs; a curseforge:// link opens in Browse.
    fn apply_args(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let args: Vec<String> = std::env::args().collect();
        if let Some(url) = args.iter().find(|a| hopper::links::is_link(a)).cloned() {
            self.browse.update(cx, |b, cx| b.open_link(&url, cx));
            self.go(Page::Browse, cx);
        }
        window.focus(&self.focus, cx);
        let value = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .and_then(|i| args.get(i + 1).cloned())
        };
        // A theme for this run only (screenshots).
        if let Some(t) = value("--theme").map(|t| t.trim_matches('"').to_string()) {
            let zoom = self.store.read(cx).settings().zoom;
            crate::theme::apply(&t, 0, zoom, cx);
            crate::theme::set_backdrop(window, crate::theme::backdrop(&t, 0, cx), cx.theme().mode.is_dark());
        }
        if let Some(q) = value("--search") {
            self.browse.update(cx, |b, cx| b.set_query(&q, window, cx));
        }
        if let Some(name) = value("--server") {
            // The rig's servers arrive a moment after start.
            cx.spawn(async move |this, cx| {
                for _ in 0..40 {
                    let done = this
                        .update(cx, |this, cx| {
                            let root = this
                                .store
                                .read(cx)
                                .servers()
                                .iter()
                                .find(|s| s.name.eq_ignore_ascii_case(&name))
                                .map(|s| s.root.clone());
                            if let Some(root) = root {
                                this.sync.update(cx, |p, cx| p.compare(root, cx));
                                true
                            } else {
                                false
                            }
                        })
                        .unwrap_or(true);
                    if done {
                        break;
                    }
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(250))
                        .await;
                }
            })
            .detach();
        }
        // The update preview for the first instance with a pack update, once
        // the update check has answered.
        if value("--dialog").as_deref() == Some("update") {
            let want = value("--instance").unwrap_or_default().to_lowercase();
            cx.spawn_in(window, async move |this, cx| {
                for _ in 0..60 {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(500))
                        .await;
                    let found = this
                        .update_in(cx, |this, window, cx| {
                            let s = this.store.read(cx);
                            let hit = s
                                .instances()
                                .iter()
                                .filter(|i| i.name.to_lowercase().contains(&want))
                                .find_map(|i| s.pack_update(i).map(|f| (i.root.clone(), f.clone())));
                            if let Some((root, f)) = hit {
                                crate::pack_update::open(this.store.clone(), root, f, window, cx);
                                true
                            } else {
                                false
                            }
                        })
                        .unwrap_or(true);
                    if found {
                        break;
                    }
                }
            })
            .detach();
        }
        // Dialogs need the window's root, which exists after this view.
        if let Some(d) = value("--dialog") {
            let want = value("--instance").unwrap_or_default().to_lowercase();
            cx.defer_in(window, move |this, window, cx| match d.as_str() {
                "new" => crate::dialogs::open_create(this.store.clone(), window, cx),
                // After the server comparison has had time to finish.
                "side" => cx
                    .spawn_in(window, async move |this, cx| {
                        cx.background_executor().timer(std::time::Duration::from_secs(8)).await;
                        this.update_in(cx, |this, window, cx| {
                            let addons = this.library.read(cx).addons().clone();
                            addons.update(cx, |a, cx| a.preview_side_card(window, cx));
                        })
                        .ok();
                    })
                    .detach(),
                "versions" => {
                    let s = this.store.read(cx);
                    let pick = s.instances().iter().find(|i| i.pack.is_some()).map(|i| {
                        (
                            i.root.clone(),
                            i.pack.as_ref().map_or(0, |p| p.project_id),
                            s.pack_update(i).cloned(),
                        )
                    });
                    if let Some((root, pid, upd)) = pick {
                        crate::versions::open(this.store.clone(), root, pid, true, upd, window, cx);
                    }
                }
                "export" => {
                    let root = this.store.read(cx).instances().first().map(|i| i.root.clone());
                    if let Some(r) = root {
                        crate::dialogs::open_export(this.store.clone(), r, window, cx);
                    }
                }
                // `--dialog share --instance "Star"`: the first match, else the first.
                "share" => {
                    let s = this.store.read(cx);
                    let root = s
                        .instances()
                        .iter()
                        .find(|i| i.name.to_lowercase().contains(&want))
                        .or(s.instances().first())
                        .map(|i| i.root.clone());
                    if let Some(r) = root {
                        crate::dialogs::open_share(this.store.clone(), r, window, cx);
                    }
                }
                "redeem" => crate::dialogs::open_redeem(this.store.clone(), window, cx),
                "accounts" => crate::accounts::open(this.store.clone(), window, cx),
                _ => {}
            });
        }
        if let Some(p) = value("--page") {
            let page = match p.as_str() {
                "browse" => Page::Browse,
                "servers" => Page::Servers,
                "sync" => Page::Sync,
                "settings" => Page::Settings,
                _ => Page::Library,
            };
            self.go(page, cx);
        }
    }

    fn find(&mut self, _: &Find, window: &mut Window, cx: &mut Context<Self>) {
        match self.page {
            Page::Browse => self.browse.update(cx, |b, cx| b.focus_search(window, cx)),
            Page::Settings => self.settings.update(cx, |s, cx| s.focus_search(window, cx)),
            _ => {
                self.go(Page::Library, cx);
                self.library.update(cx, |l, cx| l.focus_search(window, cx));
            }
        }
    }

    /// Zoom steps in percent.
    pub(crate) const ZOOMS: [u16; 10] = [67, 75, 80, 90, 100, 110, 125, 150, 175, 200];

    fn zoom_to(&mut self, zoom: u16, window: &mut Window, cx: &mut Context<Self>) {
        crate::theme::set_zoom(zoom, cx);
        self.store
            .update(cx, |s, cx| s.update_settings(|st| st.zoom = zoom, cx));
        window.push_notification(
            gpui_kit::component::notification::Notification::info(format!("Zoom {zoom}%")),
            cx,
        );
    }

    fn zoom_in(&mut self, _: &ZoomIn, window: &mut Window, cx: &mut Context<Self>) {
        let z = self.store.read(cx).settings().zoom;
        let next = Self::ZOOMS.iter().copied().find(|s| *s > z).unwrap_or(200);
        self.zoom_to(next, window, cx);
    }

    fn zoom_out(&mut self, _: &ZoomOut, window: &mut Window, cx: &mut Context<Self>) {
        let z = self.store.read(cx).settings().zoom;
        let next = Self::ZOOMS.iter().rev().copied().find(|s| *s < z).unwrap_or(67);
        self.zoom_to(next, window, cx);
    }

    fn zoom_reset(&mut self, _: &ZoomReset, window: &mut Window, cx: &mut Context<Self>) {
        self.zoom_to(100, window, cx);
    }

    fn refresh(&mut self, _: &Refresh, _: &mut Window, cx: &mut Context<Self>) {
        self.store.update(cx, |s, cx| {
            s.refresh();
            cx.notify();
        });
    }

    fn dropped(&mut self, paths: &ExternalPaths, _: &mut Window, cx: &mut Context<Self>) {
        let (target, folder) = self.library.read(cx).drop_target(cx);
        let paths = paths.paths().to_vec();
        self.store.read(cx).add_dropped(paths, target, folder);
        self.go(Page::Library, cx);
    }

    /// A page picked from the nav (or its shortcut). Browse picks up where
    /// it was left, for the same instance within a while; clicked again
    /// while showing, it starts fresh.
    pub(crate) fn nav(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        if page == Page::Browse {
            if self.page == Page::Browse {
                self.browse.update(cx, |b, cx| b.reset(window, cx));
            } else {
                let root = self.library.read(cx).drop_target(cx).0;
                self.browse.update(cx, |b, cx| b.resume_or_reset(root, window, cx));
            }
        }
        self.go(page, cx);
    }

    pub(crate) fn go(&mut self, page: Page, cx: &mut Context<Self>) {
        if self.page == Page::Browse && page != Page::Browse {
            self.browse.update(cx, |b, _| b.hidden());
        }
        self.page = page;
        if page == Page::Browse {
            let fallback = self.library.read(cx).drop_target(cx).0;
            self.browse.update(cx, |b, cx| b.shown(fallback, cx));
        }
        cx.notify();
    }

    /// Ctrl+,: Settings, ready to search.
    fn open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        self.go(Page::Settings, cx);
        self.settings.update(cx, |s, cx| s.focus_search(window, cx));
    }

    /// Settings, showing a category.
    pub(crate) fn settings_category(
        &mut self,
        category: crate::settings_page::Category,
        cx: &mut Context<Self>,
    ) {
        self.settings.update(cx, |s, cx| s.show(category, cx));
        self.go(Page::Settings, cx);
    }

    /// Settings, showing one setting (lit up for a moment).
    pub(crate) fn open_settings_at(&mut self, id: &'static str, cx: &mut Context<Self>) {
        self.settings.update(cx, |s, cx| s.jump(id, cx));
        self.go(Page::Settings, cx);
    }

    /// Shows a server on the Servers page.
    pub(crate) fn open_server(&mut self, root: std::path::PathBuf, cx: &mut Context<Self>) {
        self.servers.update(cx, |s, cx| s.pick(root, cx));
        self.go(Page::Servers, cx);
    }

    /// Once an update is installed and nothing is going on (no tasks, no
    /// game, no hosted server), restarts into it right away; otherwise
    /// waits until things are idle.
    fn auto_restart(window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
            let go = this.update(cx, |this, cx| {
                let store = this.store.read(cx);
                matches!(store.self_update(), Some(crate::store::SelfUpdate::Ready(_)))
                    && store.busy_reason().is_none()
            });
            match go {
                Ok(true) => {
                    let _ = cx.update(|_, cx| crate::settings_page::restart(cx));
                    return;
                }
                Ok(false) => {}
                Err(_) => return,
            }
        })
    }

    /// Ctrl+K: opens the command palette, or closes it.
    fn toggle_palette(&mut self, _: &CommandPalette, window: &mut Window, cx: &mut Context<Self>) {
        match self.palette.take() {
            Some(open) => open.close(window, cx),
            None => self.palette = Some(crate::palette::open(self, self.focus.clone(), window, cx)),
        }
        cx.notify();
    }

    /// The notification-area icon: its clicks, and notifications for what
    /// finishes or crashes while Hopper isn't in front.
    fn start_tray(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.store.read(cx).settings().tray_icon {
            return;
        }
        // What's already done isn't news.
        self.told_jobs = self.store.read(cx).jobs().snapshot().iter().map(|j| j.id).collect();
        let events = crate::tray::start();
        self._subscriptions.push(cx.on_app_quit(|this, cx| {
            // Hosted servers stop (and save) rather than being orphaned;
            // the scheduler goes first so it doesn't restart them.
            this.store.update(cx, |s, _| s.stop_scheduler());
            this.store.read(cx).local_panel().stop_all();
            crate::tray::remove();
            async {}
        }));
        if std::env::var_os("HOPPER_TEST_NOTIFY").is_some() {
            cx.background_executor()
                .spawn(async {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    crate::tray::notify("Hopper", "Test notification from Hopper.", false);
                })
                .detach();
        }
        cx.spawn_in(window, async move |_, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(150))
                    .await;
                for e in events.try_iter() {
                    let ok = cx.update(|window, cx| match e {
                        crate::tray::TrayEvent::Show => window.activate_window(),
                        crate::tray::TrayEvent::Quit => {
                            if quit_ok(window, cx) {
                                cx.quit()
                            }
                        }
                    });
                    if ok.is_err() {
                        return;
                    }
                }
            }
        })
        .detach();
        self._subscriptions
            .push(cx.observe_in(&self.store.clone(), window, |this, store, window, cx| {
                let away = !window.is_window_active();
                let store = store.read(cx);
                for j in store.jobs().snapshot() {
                    if j.done && this.told_jobs.insert(j.id) && away {
                        match &j.error {
                            Some(e) => crate::tray::notify(&j.title, e, true),
                            None if j.announce => crate::tray::notify(&j.title, "Done.", false),
                            None => {}
                        }
                    }
                }
                for s in store.servers() {
                    let crashed = store.panel_stats(&s.root).is_some_and(|p| p.crashed);
                    if crashed && this.told_crashes.insert(s.root.clone()) {
                        crate::tray::notify(&format!("{} crashed", s.name), "Its panel reports a crash.", true);
                    } else if !crashed {
                        this.told_crashes.remove(&s.root);
                    }
                }
            }));
    }

    /// "Update to x" or "Restart to update" in the title bar.
    fn update_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        use crate::store::SelfUpdate;
        match self.store.read(cx).self_update()? {
            SelfUpdate::Available(r) => Some(
                Button::new("title-update")
                    .xsmall()
                    .primary()
                    .icon(IconName::Download)
                    .label(format!("Update to {}", r.version))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.store.update(cx, |s, cx| {
                            s.install_self_update();
                            cx.notify();
                        })
                    }))
                    .into_any_element(),
            ),
            SelfUpdate::Ready(_) => Some(
                Button::new("title-restart")
                    .xsmall()
                    .primary()
                    .label("Restart to update")
                    .on_click(|_, _, cx| crate::settings_page::restart(cx))
                    .into_any_element(),
            ),
            _ => None,
        }
    }

    fn title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        // Who plays: the Microsoft sign-in when Hopper launches directly,
        // else the launcher's account.
        let store = self.store.read(cx);
        // A name and face that open the Accounts dialog.
        let chip = |id: &'static str, name: String, face: AnyElement, tip: &'static str, cx: &mut Context<Self>| {
            let hover = cx.theme().list_hover;
            h_flex()
                .id(id)
                .gap_2()
                .pl_2()
                .pr_1()
                .py_0p5()
                .rounded_full()
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip).build(w, cx))
                .child(div().text_xs().font_semibold().child(name))
                .child(face)
                .on_click(cx.listener(|this, _, window, cx| {
                    crate::accounts::open(this.store.clone(), window, cx)
                }))
                .into_any_element()
        };
        // (name, face, tooltip) of who plays, or None for a sign-in button.
        // Only accounts signed in to Hopper itself; otherwise "Sign in".
        const TIP: &str = "Accounts, skin, cape and name";
        let who: Option<Option<(String, AnyElement, &'static str)>> = Some(if let Some(a) = store.accounts().active() {
            Some((a.name.clone(), crate::accounts::head(store, a, rems(1.25), cx), TIP))
        } else if store.legacy_login() {
            let name = store.settings().signed_in_as.clone().unwrap_or_else(|| "Signed in".into());
            Some((name, widgets::thumb(None, rems(1.25), cx), TIP))
        } else {
            None
        });
        let account: Option<AnyElement> = who.map(|w| match w {
            Some((name, face, tip)) => chip("account", name, face, tip, cx),
            None => Button::new("title-sign-in")
                .xsmall()
                .outline()
                .label("Sign in")
                .on_click(cx.listener(|this, _, window, cx| {
                    crate::dialogs::open_sign_in(this.store.clone(), window, cx)
                }))
                .into_any_element(),
        });
        // Settings sits apart from the page tabs, at the trailing edge.
        let selected = Page::ALL.iter().position(|p| *p == self.page);
        TitleBar::new().child(
            h_flex()
                .flex_1()
                .pr_2()
                .gap_4()
                .child(
                    h_flex()
                        .gap_2()
                        .child(widgets::logo().size_5())
                        .child(div().text_sm().font_semibold().child("Hopper")),
                )
                // Interactive parts occlude the bar's drag area: otherwise Windows
                // reads a press on them as a window drag and the click never comes.
                .child(h_flex().gap_1().occlude().children(Page::ALL.iter().enumerate().map(|(ix, p)| {
                    // Icon and label share one pill; the page you're on is
                    // tinted with the accent.
                    let page = *p;
                    let active = selected == Some(ix);
                    let theme = cx.theme();
                    let (accent, hover, muted, fg) =
                        (theme.primary, theme.list_hover, theme.muted_foreground, theme.foreground);
                    h_flex()
                        .id(SharedString::from(format!("nav:{}", p.label())))
                        .gap_1p5()
                        .px_3()
                        .py_1()
                        .rounded_full()
                        .text_sm()
                        .cursor_pointer()
                        .map(|r| {
                            if active {
                                r.bg(accent.opacity(0.16)).text_color(accent).font_semibold()
                            } else {
                                r.text_color(muted)
                                    .hover(move |s| s.bg(hover).text_color(fg))
                            }
                        })
                        .child(Icon::new(p.icon()).small())
                        .child(p.label())
                        .on_click(cx.listener(move |this, _, window, cx| this.nav(page, window, cx)))
                })))
                .child(div().flex_1())
                .child(
                    h_flex()
                        .gap_4()
                        .items_center()
                        .occlude()
                .when(hopper::cf::api_key().is_none(), |r| {
                    r.child(
                        div()
                            .id("no-key")
                            .text_xs()
                            .text_color(cx.theme().warning)
                            .tooltip(|w, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(
                                    "Add your CurseForge API key in Settings → CurseForge & Modrinth.",
                                )
                                .build(w, cx)
                            })
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| this.open_settings_at("cf-key", cx)))
                            .child("No CurseForge key"),
                    )
                })
                .children(self.update_button(cx))
                // One place on every page, beside the account.
                .child(self.tasks.clone())
                .children(account)
                .child(
                    Button::new("open-palette")
                        .ghost()
                        .small()
                        .icon(IconName::Search)
                        .label("Ctrl K")
                        .tooltip("Search or run a command (Ctrl+K)")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.toggle_palette(&CommandPalette, window, cx)
                        })),
                )
                .child(
                    Button::new("refresh")
                        .ghost()
                        .small()
                        .icon(IconName::RefreshCw)
                        .tooltip("Re-read instances and servers from disk (F5)")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.refresh(&Refresh, window, cx)
                        })),
                )
                .child(
                    Button::new("open-settings")
                        .ghost()
                        .small()
                        .icon(IconName::Settings)
                        .selected(self.page == Page::Settings)
                        .tooltip("Settings")
                        .on_click(cx.listener(|this, _, _, cx| this.go(Page::Settings, cx))),
                ),
                ),
        )
    }
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Cached: a page re-renders only when it (or what it reads) changes,
        // not on every frame of a dialog or spinner animating above it.
        let page: AnyView = match self.page {
            Page::Library => self.library.clone().into(),
            Page::Browse => self.browse.clone().into(),
            Page::Servers => self.servers.clone().into(),
            Page::Sync => self.sync.clone().into(),
            Page::Settings => self.settings.clone().into(),
        };
        let page = page.cached(StyleRefinement::default().size_full());
        let title = self.title_bar(cx);
        let overlay = self.tasks.update(cx, |t, cx| t.overlay(window, cx));
        let theme = cx.theme();
        let ring = theme.primary;
        v_flex()
            .id("app")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::find))
            .on_action(cx.listener(Self::refresh))
            .on_action(cx.listener(Self::zoom_in))
            .on_action(cx.listener(Self::zoom_out))
            .on_action(cx.listener(Self::zoom_reset))
            .on_action(cx.listener(Self::toggle_palette))
            .on_action(cx.listener(Self::open_settings))
            .on_action(cx.listener(|this, _: &GoLibrary, _, cx| this.go(Page::Library, cx)))
            .on_action(cx.listener(|this, _: &GoBrowse, window, cx| {
                this.nav(Page::Browse, window, cx)
            }))
            .on_action(cx.listener(|this, _: &GoServers, _, cx| this.go(Page::Servers, cx)))
            .on_action(cx.listener(|this, _: &GoSync, _, cx| this.go(Page::Sync, cx)))
            .on_action(cx.listener(|this, _: &PrevInstance, _, cx| {
                this.go(Page::Library, cx);
                this.library.update(cx, |l, cx| l.step_instance(-1, cx));
            }))
            .on_action(cx.listener(|this, _: &NextInstance, _, cx| {
                this.go(Page::Library, cx);
                this.library.update(cx, |l, cx| l.step_instance(1, cx));
            }))
            .on_drop(cx.listener(Self::dropped))
            .drag_over::<ExternalPaths>(move |s, _, _, _| s.border_2().border_color(ring))
            .size_full()
            .relative()
            // No background here: the root under this paints it once, and a
            // second coat would make glass themes opaque.
            .text_color(theme.foreground)
            .child(title)
            .child(div().flex_1().min_h_0().child(widgets::enter(
                ElementId::Name(format!("page-{}", self.page.label()).into()),
                div().size_full().child(page),
            )))
            .children(overlay)
            .children(self.palette.as_ref().map(|p| p.view()))
    }
}

/// Whether quitting can go ahead now. With tasks running or servers hosted
/// here up, asks instead (and quits from the dialog) and returns false.
fn quit_ok(window: &mut Window, cx: &mut App) -> bool {
    let Some(app) = window.root::<gpui_kit::component::Root>().flatten().and_then(|r| {
        r.read(cx).view().clone().downcast::<AppView>().ok()
    }) else {
        return true;
    };
    let store = app.read(cx).store.clone();
    let tasks = store.read(cx).jobs().snapshot().iter().filter(|j| !j.done).count();
    let servers = store.read(cx).local_panel().all().iter().filter(|s| s.state().is_alive()).count();
    if tasks == 0 && servers == 0 {
        return true;
    }
    let mut what = Vec::new();
    if tasks > 0 {
        what.push(format!("{tasks} task{} still running", if tasks == 1 { " is" } else { "s are" }));
    }
    if servers > 0 {
        what.push(format!(
            "{servers} server{} hosted here will be stopped (saving the world)",
            if servers == 1 { "" } else { "s" }
        ));
    }
    let text = what.join("; ") + ".";
    window.open_alert_dialog(cx, move |alert, _, _| {
        let store = store.clone();
        alert
            .title("Quit Hopper?")
            .description(text.clone())
            .button_props(
                gpui_kit::component::dialog::DialogButtonProps::default()
                    .ok_text("Quit")
                    .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
                    .show_cancel(true),
            )
            .on_ok(move |_, _, cx| {
                store.update(cx, |s, _| s.stop_scheduler());
                let local = store.read(cx).local_panel();
                // Stop the servers off the UI thread, then quit.
                cx.spawn(async move |cx| {
                    cx.background_spawn(async move { local.stop_all() }).await;
                    cx.update(|cx| cx.quit());
                })
                .detach();
                true
            })
    });
    false
}
