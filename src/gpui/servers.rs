//! The Servers page: what each server is and runs, and (its Sync tab) the
//! server against its pack. With server tools off, an intro to them.

use crate::widgets::Clickable as _;
use crate::rigsync::SyncPanel;
use crate::server_control::{ControlEvent, ServerControl};
use crate::server_settings::ServerSettings;
use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::description_list::DescriptionList;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::model::ModInfo;
use hopper::panel::PanelStats;
use hopper::server::{Manager, ServerInfo, Status};
use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

/// A small round status light: the panel's state when there is one, else
/// whether the server answers pings.
fn status_dot(status: Option<&Status>, panel: Option<&PanelStats>, cx: &App) -> Div {
    div()
        .size_2()
        .flex_none()
        .rounded_full()
        .bg(status_color(status, panel, cx))
}

/// The color of a server's state (see [`status_dot`]).
fn status_color(status: Option<&Status>, panel: Option<&PanelStats>, cx: &App) -> Hsla {
    let theme = cx.theme();
    match (panel, status) {
        (Some(p), _) if p.crashed => theme.danger,
        (Some(p), _) if p.starting || p.updating => theme.warning,
        // Running in the panel but not answering yet: still loading.
        (Some(p), Some(Status::Online { .. })) if p.running => theme.success,
        (Some(p), _) if p.running => theme.warning,
        (Some(_), _) => theme.muted_foreground,
        (None, Some(Status::Online { .. })) => theme.success,
        (None, Some(Status::Offline)) => theme.muted_foreground,
        (None, None) => theme.border,
    }
}

fn status_text(status: Option<&Status>, panel: Option<&PanelStats>) -> String {
    match (panel, status) {
        (Some(p), _) if p.crashed => return "Crashed".into(),
        (Some(p), _) if p.starting => return "Starting".into(),
        (Some(p), Some(Status::Offline) | None) if p.running => {
            return "Starting up (running, not answering pings yet)".into();
        }
        (Some(p), _) if !p.running => return "Stopped".into(),
        _ => {}
    }
    match status {
        Some(Status::Online {
            players,
            max,
            version,
        }) => format!("Online · {players}/{max} players · {version}"),
        Some(Status::Offline) => "Offline".into(),
        None => "Checking".into(),
    }
}

/// "No servers yet", with the Host action when the page has one.
fn no_servers(on_host: Option<Rc<dyn Fn(&mut Window, &mut App)>>, cx: &App) -> Div {
    widgets::empty_state(
        IconName::Server,
        "No servers yet",
        Some("Host one on this PC, or put server folders in the servers folder.".into()),
        cx,
    )
    .when_some(on_host, |c, host| {
        c.child(
            Button::new("host-server-empty")
                .small()
                .primary()
                .icon(IconName::Plus)
                .label("Host a server")
                .on_click(move |_, window, cx| host(window, cx)),
        )
    })
}

/// The server list's width: a rail of status tiles when dragged narrow.
const LIST_PANEL: widgets::PanelSpec = widgets::PanelSpec::new("servers.list", 18., 14., 30.).rail(10., 4.5);

/// The server list down the leading side, or a rail of tiles when
/// `collapsed`.
fn server_list(
    id: &'static str,
    store: &Entity<Store>,
    picked: Option<&PathBuf>,
    collapsed: bool,
    rem: Pixels,
    on_pick: Rc<dyn Fn(PathBuf, &mut Window, &mut App)>,
    on_host: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    cx: &App,
) -> Stateful<Div> {
    if collapsed {
        return server_rail(id, store, picked, on_pick, cx);
    }
    let s = store.read(cx);
    let theme = cx.theme();
    let instances: Vec<(PathBuf, String)> = s
        .instances()
        .iter()
        .map(|i| (i.root.clone(), i.name.clone()))
        .collect();
    let mut shown_local = false;
    // Running servers first, within the rig's and this PC's.
    let running = |srv: &ServerInfo| {
        s.panel_stats(&srv.root).map_or_else(
            || matches!(s.status(&srv.root), Some(Status::Online { .. })),
            |p| p.running || p.starting,
        )
    };
    let mut ordered: Vec<&ServerInfo> = s.servers().iter().collect();
    ordered.sort_by_key(|srv| (s.is_hosted(&srv.root), !running(srv)));
    let rows = ordered.into_iter().map(|srv| {
        // Servers hosted here come after the rig's, under their own label.
        let local_head = (s.is_hosted(&srv.root) && !std::mem::replace(&mut shown_local, true)).then(|| {
            div()
                .px_3()
                .pt_4()
                .pb_1()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.muted_foreground)
                .child("THIS PC")
        });
        let active = picked == Some(&srv.root);
        let root = srv.root.clone();
        let pick = on_pick.clone();
        let paired = s.paired(srv).map(|i| i.name.clone());
        // Not paired yet: a quiet link that lists the packs (a paired
        // server changes its pack on its Sync tab).
        let pairing: Option<AnyElement> = if paired.is_none() {
            let names: Vec<String> = instances.iter().map(|(_, n)| n.clone()).collect();
            let roots: Vec<PathBuf> = instances.iter().map(|(r, _)| r.clone()).collect();
            let (store, server_root) = (store.clone(), srv.root.clone());
            Some(
                Button::new(ElementId::Name(format!("pair-link:{}", srv.root.display()).into()))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Link)
                    .label("Pair a pack…")
                    .tooltip("Choose the instance this server runs, to compare their mods")
                    .dropdown_menu(move |menu, _, _| {
                        let mut menu = menu.min_w(rems(13.).to_pixels(rem)).max_h(rems(26.).to_pixels(rem)).scrollable(true);
                        for (ix, n) in names.iter().enumerate() {
                            let (store, roots, sr) = (store.clone(), roots.clone(), server_root.clone());
                            menu = menu.item(PopupMenuItem::new(n.clone()).on_click(move |_, _, cx| {
                                if let Some(r) = roots.get(ix) {
                                    let r = r.clone();
                                    store.update(cx, |s, cx| s.set_pairing(&sr, &r, cx));
                                }
                            }));
                        }
                        menu
                    })
                    .into_any_element(),
            )
        } else {
            None
        };
        let row = v_flex()
            .id(ElementId::Name(
                format!("server:{}", srv.root.display()).into(),
            ))
            .gap_2()
            .px_3()
            .py_2()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.transparent)
            .clickable(cx)
            .when(active, |r| widgets::row_selected(r, cx))
            .when(!active, |r| r.hover(|s| widgets::row_hover(s, cx)))

            .on_click(move |_, w, cx| pick(root.clone(), w, cx))
            .context_menu({
                let (store, root) = (store.clone(), srv.root.clone());
                let pack = s.paired(srv).map(|i| i.root.clone());
                move |menu, _, _| {
                    let go = |compare: bool| {
                        let (store, root) = (store.clone(), root.clone());
                        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                            store.update(cx, |_, cx| {
                                cx.emit(crate::store::StoreEvent::ShowServer { root: root.clone(), compare })
                            })
                        }
                    };
                    let r = root.clone();
                    menu.item(PopupMenuItem::new("Open").icon(IconName::Server).on_click(go(false)))
                        .when(pack.is_some(), |m| {
                            let (store, pack) = (store.clone(), pack.clone().unwrap_or_default());
                            m.item(
                                PopupMenuItem::new("Sync with pack")
                                    .icon(IconName::ArrowLeftRight)
                                    .on_click(go(true)),
                            )
                            .item(PopupMenuItem::new("Open its pack").icon(IconName::Package).on_click(
                                move |_, _, cx| {
                                    store.update(cx, |_, cx| {
                                        cx.emit(crate::store::StoreEvent::ShowInstance(pack.clone()))
                                    })
                                },
                            ))
                        })
                        .separator()
                        .item(
                            PopupMenuItem::new("Open folder")
                                .icon(IconName::FolderOpen)
                                .on_click(move |_, _, _| crate::store::open_path(&r)),
                        )
                }
            })
            .child(
                h_flex()
                    .gap_2()
                    .child(status_dot(s.status(&srv.root), s.panel_stats(&srv.root), cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .font_medium()
                            .truncate()
                            .child(srv.name.clone()),
                    )
                    // A stopped server is just its grey dot; pills are for
                    // running, starting and failing ones.
                    .when_some(s.panel_stats(&srv.root).filter(|p| p.running || p.starting || p.crashed), |r, p| {
                        let live = p.running && !p.starting && !p.crashed;
                        let text = if live {
                            format!("{}/{}", p.online, p.max)
                        } else {
                            p.state().to_string()
                        };
                        r.child(widgets::pill(text, status_color(s.status(&srv.root), Some(p), cx), live))
                    }),
            )
            .when(paired.is_some(), |r| {
                r.child(
                    widgets::caption(paired.clone().unwrap_or_default(), cx)
                        .text_xs()
                        .pl_4(),
                )
            })
            .children(pairing.map(|p| div().pl_4().child(p)));
        v_flex().children(local_head).child(row)
    });
    let empty = s.servers().is_empty();
    let collapse = {
        let store = store.clone();
        Button::new("servers-collapse")
            .ghost()
            .small()
            .icon(IconName::PanelLeftClose)
            .tooltip("Hide server list")
            .on_click(move |_, _, cx| widgets::SidePanel::new(LIST_PANEL).set_collapsed(true, &store, cx))
    };
    v_flex()
        .id(id)
        .size_full()
        .px_2()
        .py_4()
        .gap_1()
        .child(
            v_flex()
                .px_2()
                .pb_2()
                .gap_0p5()
                .child(
                    h_flex()
                        .child(div().flex_1().min_w_0().truncate().text_lg().font_semibold().child("Servers"))
                        .when_some(on_host.clone(), |r, host| {
                            r.child(
                                Button::new("host-server")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Plus)
                                    .tooltip("Host a server on this PC")
                                    .on_click(move |_, window, cx| host(window, cx)),
                            )
                        })
                        .child(collapse),
                )
                .child(widgets::caption(s.settings().servers_root.clone(), cx).text_xs()),
        )
        .children(rows)
        .when(empty, |r| {
            r.child(no_servers(on_host, cx))
        })
}

/// The collapsed server list: a tile per server with its status light,
/// and a button to open the list again.
fn server_rail(
    id: &'static str,
    store: &Entity<Store>,
    picked: Option<&PathBuf>,
    on_pick: Rc<dyn Fn(PathBuf, &mut Window, &mut App)>,
    cx: &App,
) -> Stateful<Div> {
    let s = store.read(cx);
    let theme = cx.theme();
    let mut ordered: Vec<&ServerInfo> = s.servers().iter().collect();
    ordered.sort_by_key(|srv| s.is_hosted(&srv.root));
    let tiles: Vec<AnyElement> = ordered
        .into_iter()
        .map(|srv| {
            let active = picked == Some(&srv.root);
            let (root, pick) = (srv.root.clone(), on_pick.clone());
            let name: SharedString = srv.name.clone().into();
            let initials: String = srv
                .name
                .split(|c: char| !c.is_alphanumeric())
                .filter_map(|w| w.chars().next())
                .take(2)
                .collect::<String>()
                .to_uppercase();
            div()
                .id(ElementId::Name(format!("server-tile:{}", srv.root.display()).into()))
                .relative()
                .size(rems(2.75))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .text_sm()
                .font_semibold()
                .clickable(cx)
                .when(active, |r| widgets::row_selected(r, cx))
                .when(!active, |r| r.hover(|s| widgets::row_hover(s, cx)))
                .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(name.clone()).build(w, cx))
                .on_click(move |_, w, cx| pick(root.clone(), w, cx))
                .child(initials)
                .child(
                    div()
                        .absolute()
                        .top(px(3.))
                        .right(px(3.))
                        .child(status_dot(s.status(&srv.root), s.panel_stats(&srv.root), cx)),
                )
                .into_any_element()
        })
        .collect();
    let store = store.clone();
    v_flex()
        .id(id)
        .size_full()
        .items_center()
        .py_3()
        .gap_2()
        .child(
            Button::new("servers-expand")
                .ghost()
                .small()
                .icon(IconName::PanelLeftOpen)
                .tooltip("Show server list")
                .on_click(move |_, _, cx| widgets::SidePanel::new(LIST_PANEL).set_collapsed(false, &store, cx)),
        )
        .children(tiles)
}

// ---- Servers page --------------------------------------------------------

pub struct ServersView {
    store: Entity<Store>,
    /// The server list's width.
    list_panel: widgets::SidePanel,
    /// The Overview's modpack description, its GIFs playing.
    pack_desc: Entity<widgets::DescView>,
    picked: Option<PathBuf>,
    /// Jars in each server's mods folder.
    mods: HashMap<PathBuf, Option<Result<Vec<ModInfo>, String>>>,
    filter: Entity<InputState>,
    query: String,
    scroll: UniformListScrollHandle,
    /// Start, stop and the console, through the server panel.
    control: Entity<ServerControl>,
    /// server.properties and JVM settings.
    settings_view: Entity<ServerSettings>,
    /// The Sync tab: the server against its pack.
    sync: Entity<SyncPanel>,
    /// Overview, Console, Backups, Settings, Mods or Sync.
    tab: usize,
    /// The Sync tab was open when the page was left.
    sync_away: bool,
    /// The Overview's modpack readme shows in full.
    pack_open: bool,
    _subscriptions: Vec<Subscription>,
}

/// The Sync tab's index among a server's tabs.
pub const TAB_SYNC: usize = 5;
const TAB_CONSOLE: usize = 1;

/// What the Servers page asks the app to do.
pub enum ServersEvent {
    /// Settings › Servers, at one setting (the panel's address, the
    /// servers folder).
    OpenSettings(&'static str),
    /// "Not now" on the intro: back to where the user was.
    Leave,
}

impl EventEmitter<ServersEvent> for ServersView {}

impl ServersView {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter jars"));
        let control = cx.new(|cx| {
            let mut c = ServerControl::new(store.clone(), window, cx);
            // The Servers page is about the server: the full rate.
            c.set_active(true, cx);
            c
        });
        let settings_view = cx.new(|cx| ServerSettings::new(store.clone(), window, cx));
        let sync = cx.new(|cx| {
            let mut p = SyncPanel::new(store.clone(), window, cx);
            p.set_in_server_page(true);
            p
        });
        let subs = vec![
            cx.observe(&control, |_, _, cx| cx.notify()),
            cx.observe(&sync, |_, _, cx| cx.notify()),
            cx.subscribe(&control, |this, _, e: &ControlEvent, cx| match e {
                ControlEvent::SetUp => cx.emit(ServersEvent::OpenSettings("panel-url")),
                // This page has a Console tab of its own.
                ControlEvent::Console => this.set_tab(TAB_CONSOLE, cx),
            }),
            cx.observe(&store, |this, _, cx| {
                this.ensure_pick(cx);
                cx.notify();
            }),
            cx.subscribe_in(&filter, window, |this, state, e, _, cx| {
                if matches!(e, InputEvent::Change) {
                    this.query = state.read(cx).value().trim().to_lowercase();
                    cx.notify();
                }
            }),
        ];
        let pack_desc = cx.new(|cx| widgets::DescView::new(store.clone(), window, cx));
        let mut v = Self {
            store,
            list_panel: widgets::SidePanel::new(LIST_PANEL),
            pack_desc,
            picked: None,
            mods: HashMap::new(),
            filter,
            query: String::new(),
            scroll: UniformListScrollHandle::new(),
            control,
            settings_view,
            sync,
            tab: 0,
            sync_away: false,
            pack_open: false,
            _subscriptions: subs,
        };
        v.ensure_pick(cx);
        v
    }

    fn ensure_pick(&mut self, cx: &mut Context<Self>) {
        let first = self
            .store
            .read(cx)
            .servers()
            .first()
            .map(|s| s.root.clone());
        if self.picked.is_none()
            && let Some(r) = first
        {
            self.pick(r, cx);
        }
    }

    /// Where the Servers page is, for back and forward.
    pub(crate) fn nav_loc(&self) -> (Option<PathBuf>, usize) {
        (self.picked.clone(), self.tab)
    }

    /// Goes back (or forward) to a place `nav_loc` gave.
    pub(crate) fn nav_restore(&mut self, root: Option<PathBuf>, tab: usize, cx: &mut Context<Self>) {
        if let Some(r) = root
            && self.picked.as_ref() != Some(&r)
        {
            self.pick(r, cx);
        }
        self.set_tab(tab, cx);
    }

    pub(crate) fn pick(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.picked = Some(root.clone());
        self.scan(root.clone(), false, cx);
        self.control.update(cx, |c, cx| c.show(root.clone(), cx));
        if self.tab == TAB_SYNC {
            self.sync.update(cx, |p, cx| p.show(root, cx));
        }
        cx.notify();
    }

    /// Shows one of the server's tabs: the console polls only on its own,
    /// and Sync compares again each time it opens.
    pub(crate) fn set_tab(&mut self, tab: usize, cx: &mut Context<Self>) {
        let was = self.tab;
        self.tab = tab;
        self.control.update(cx, |c, cx| c.set_console_open(tab == TAB_CONSOLE, cx));
        if tab == TAB_SYNC {
            self.sync_away = false;
            if let Some(r) = self.picked.clone() {
                self.sync.update(cx, |p, cx| p.show(r, cx));
            }
        } else if was == TAB_SYNC {
            self.sync.update(cx, |p, cx| p.hidden(cx));
        }
        cx.notify();
    }

    /// A server's Sync tab (the picked one's when `root` is None).
    pub(crate) fn open_sync(&mut self, root: Option<PathBuf>, cx: &mut Context<Self>) {
        if let Some(r) = root
            && self.picked.as_ref() != Some(&r)
        {
            self.picked = Some(r.clone());
            self.scan(r.clone(), false, cx);
            self.control.update(cx, |c, cx| c.show(r, cx));
        }
        self.set_tab(TAB_SYNC, cx);
    }

    /// The page went out of view: the Sync tab's polls slow down.
    pub(crate) fn hidden(&mut self, cx: &mut Context<Self>) {
        if self.tab == TAB_SYNC {
            self.sync.update(cx, |p, cx| p.hidden(cx));
            self.sync_away = true;
        }
    }

    /// Back in view: an open Sync tab compares again.
    pub(crate) fn shown(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.sync_away)
            && self.tab == TAB_SYNC
            && let Some(r) = self.picked.clone()
        {
            self.sync.update(cx, |p, cx| p.show(r, cx));
        }
    }

    fn scan(&mut self, root: PathBuf, again: bool, cx: &mut Context<Self>) {
        if !again && self.mods.contains_key(&root) {
            return;
        }
        self.mods.insert(root.clone(), None);
        cx.spawn(async move |this, cx| {
            let dir = root.join("mods");
            let r = cx
                .background_spawn(async move {
                    hopper::scan::scan_folder(&dir).map_err(|e| format!("{e:#}"))
                })
                .await;
            this.update(cx, |this, cx| {
                this.mods.insert(root, Some(r));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn rows(&self) -> Vec<ModInfo> {
        let Some(Some(Ok(mods))) = self.picked.as_ref().and_then(|p| self.mods.get(p)) else {
            return Vec::new();
        };
        mods.iter()
            .filter(|m| {
                self.query.is_empty()
                    || m.mod_name.to_lowercase().contains(&self.query)
                    || m.file_name.to_lowercase().contains(&self.query)
            })
            .cloned()
            .collect()
    }

    fn render_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let rows = self.rows();
        let theme = cx.theme();
        rows[range.start.min(rows.len())..range.end.min(rows.len())]
            .iter()
            .map(|m| {
                h_flex()
                    .w_full()
                    .h_9()
                    .px_3()
                    .gap_4()
                    .rounded(theme.radius)
                    .hover(|s| s.bg(theme.list_hover))
                    .child(
                        div()
                            .w_64()
                            .flex_none()
                            .text_sm()
                            .font_medium()
                            .truncate()
                            .child(m.mod_name.clone()),
                    )
                    .child(
                        div()
                            .w_40()
                            .flex_none()
                            .text_sm()
                            .truncate()
                            .text_color(theme.muted_foreground)
                            .child(m.version.clone()),
                    )
                    .child(
                        div()
                            .w_16()
                            .flex_none()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(m.side.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .truncate()
                            .text_color(theme.muted_foreground)
                            .child(m.file_name.clone()),
                    )
                    .into_any_element()
            })
            .collect()
    }

    fn detail(&self, s: &ServerInfo, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // The paired pack's page, for the Overview.
        let pack_id = self
            .store
            .read(cx)
            .paired(s)
            .and_then(|i| i.pack.as_ref())
            .map_or(0, |p| p.project_id);
        if self.tab == 0 && pack_id != 0 {
            self.store.update(cx, |st, _| st.want_pack_page(pack_id));
        }
        let store = self.store.read(cx);
        let status = store.status(&s.root);
        let pstats = store.panel_stats(&s.root).cloned();
        let paired = store.paired(s).cloned();
        let configured = self.control.read(cx).configured(cx);
        let manager = match s.manager {
            _ if store.is_hosted(&s.root) => "Hopper, on this PC",
            Manager::Crafty => "Crafty",
            Manager::Mcss => "MCSS",
            Manager::Unknown => "Unknown",
        };
        let mut info = DescriptionList::horizontal()
            .columns(2)
            .label_width(rems(8.));
        // Live numbers (CPU, memory, players, world) are the cards' job;
        // this table is for what stays put.
        if store.is_hosted(&s.root) {
            let sched = hopper::hosting::Schedule::load(&s.root);
            if let Some(n) = hopper::hosting::schedule::describe_next_restart(
                &sched,
                hopper::hosting::schedule::LocalTime::now(),
            ) {
                info = info.item("Next restart", n, 1);
            }
            if let Some(why) = store.schedule_gave_up(&s.root) {
                info = info.item("Crash restarts", why, 1);
            }
        }
        let mut info = info
            .item("Managed by", manager, 1)
            .item(
                "Loader",
                s.loader.clone().unwrap_or_else(|| {
                    hopper::hosting::HostedMeta::load(&s.root)
                        .map(|m| format!("{} · Minecraft {}", m.loader, m.minecraft))
                        .unwrap_or("Unknown".into())
                }),
                1,
            )
            .item("Java", s.java.clone().unwrap_or("Unknown".into()), 1)
            .item(
                "Max memory",
                s.max_memory.clone().unwrap_or("Unknown".into()),
                1,
            )
            .item("Port", s.port().to_string(), 1);
        // Set in Hopper Server (Settings › Address).
        let route = hopper::hosting::router::RouteSettings::load(&s.root);
        if !route.addresses.is_empty() {
            info = info.item("Address", route.addresses.join(", "), 1);
        }
        if let Some(inst) = &paired {
            let ok = s.loader.as_deref() == Some(inst.loader.replace('-', " ").as_str());
            info = info.item(
                "Pack loader",
                format!(
                    "{}{}",
                    inst.loader,
                    if ok { " (matches)" } else { " (differs)" }
                ),
                1,
            );
            if let Some(p) = &inst.pack {
                info = info.item("Pack version", p.version.clone(), 1);
            }
        }
        for key in [
            "motd",
            "max-players",
            "level-name",
            "difficulty",
            "view-distance",
        ] {
            if let Some(v) = s.properties.get(key) {
                info = info.item(key, hopper::instance_ops::strip_formatting(v), 1);
            }
        }
        let (root, server) = (s.root.clone(), s.clone());
        let jars: AnyElement = match self.mods.get(&s.root) {
            None | Some(None) => h_flex()
                .gap_2()
                .child(Spinner::new().small())
                .child(widgets::caption("Reading jars", cx))
                .into_any_element(),
            Some(Some(Err(e))) => div()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(e.clone())
                .into_any_element(),
            Some(Some(Ok(_))) => {
                let n = self.rows().len();
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        uniform_list(
                            "server-jars",
                            n,
                            cx.processor(|this, r: Range<usize>, _, cx| this.render_rows(r, cx)),
                        )
                        .track_scroll(&self.scroll)
                        .size_full(),
                    )
                    .vertical_scrollbar(&self.scroll)
                    .into_any_element()
            }
        };
        let n_jars = self.rows().len();
        v_flex()
            .size_full()
            .px_6()
            .py_5()
            .gap_5()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_1p5()
                                    .child(
                                        h_flex()
                                            .gap_1p5()
                                            .flex_wrap()
                                            // A panel shows its state beside Start.
                                            .when(!configured, |r| {
                                                r.child(widgets::pill(
                                                    status_text(status, pstats.as_ref()),
                                                    status_color(status, pstats.as_ref(), cx),
                                                    true,
                                                ))
                                            })
                                            .child(widgets::pill(
                                                manager,
                                                cx.theme().muted_foreground,
                                                false,
                                            )),
                                    )
                                    .child(
                                        div()
                                            .text_size(rems(1.75))
                                            .font_weight(FontWeight::EXTRA_BOLD)
                                            .truncate()
                                            .child(s.name.clone()),
                                    ),
                            )
                            .child(self.control.update(cx, |c, cx| c.controls(cx))),
                    )
                    .child(widgets::caption(
                        if configured {
                            format!(
                                "Runs through {}. Its mods sync with its pack on the Sync tab.",
                                self.control.read(cx).panel_name(cx)
                            )
                        } else {
                            "Start and stop stay in the server panel unless Hopper is connected to it."
                                .to_string()
                        },
                        cx,
                    )),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("open-server")
                            .outline()
                            .small()
                            .icon(IconName::FolderOpen)
                            .label("Folder")
                            .on_click(move |_, _, _| crate::store::open_path(&root)),
                    )
                    .child(
                        Button::new("ping")
                            .outline()
                            .small()
                            .icon(IconName::Activity)
                            .label("Ping")
                            .on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.store.read(cx).ping(&server)
                                }),
                            ),
                    )
            )
            .child(
                TabBar::new("server-tabs")
                    .underline()
                    .selected_index(self.tab)
                    .child(Tab::new().prefix(Icon::new(IconName::Info).small()).label("Overview"))
                    .child(Tab::new().prefix(Icon::new(IconName::SquareTerminal).small()).label("Console"))
                    .child(Tab::new().prefix(Icon::new(IconName::Archive).small()).label("Backups"))
                    .child(Tab::new().prefix(Icon::new(IconName::Settings2).small()).label("Settings"))
                    .child(
                        Tab::new()
                            .prefix(Icon::new(IconName::Puzzle).small())
                            .label(format!("Mods {n_jars}")),
                    )
                    .child(Tab::new().prefix(Icon::new(IconName::ArrowLeftRight).small()).label("Sync"))
                    .on_click(cx.listener(|this, ix: &usize, _, cx| this.set_tab(*ix, cx))),
            )
            .when(self.tab == 0, |c| {
                let players = self.control.update(cx, |ctl, cx| ctl.players_view(cx));
                let cards = self.control.update(cx, |ctl, cx| ctl.stat_cards(cx));
                if pack_id != 0 {
                    self.pack_desc.update(cx, |d, _| d.show_pack(pack_id));
                }
                let pack = (pack_id != 0).then(|| {
                    let store = self.store.read(cx);
                    let page = store.pack_page(pack_id);
                    let icon = match page {
                        Some(crate::store::PackPage::Ready(m, ..)) => m.icon().and_then(|u| store.image(u)),
                        _ => None,
                    };
                    let ready = matches!(page, Some(crate::store::PackPage::Ready(..)));
                    let open = self.pack_open;
                    let theme = cx.theme().clone();
                    v_flex()
                        .gap_3()
                        .child(div().text_base().font_weight(FontWeight::BOLD).child("Modpack"))
                        // A long readme is clipped until asked for.
                        .child(
                            div()
                                .when(ready && !open, |d| d.max_h(rems(24.)).overflow_hidden())
                                .child(widgets::pack_page(page, icon, &self.pack_desc, true, cx)),
                        )
                        .when(ready, |c| {
                            c.child(
                                h_flex().justify_center().child(
                                    Button::new("pack-more")
                                        .ghost()
                                        .small()
                                        .text_color(theme.muted_foreground)
                                        .icon(if open { IconName::ChevronUp } else { IconName::ChevronDown })
                                        .label(if open { "Show less" } else { "Show more" })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.pack_open = !this.pack_open;
                                            cx.notify();
                                        })),
                                ),
                            )
                        })
                });
                c.child(
                    v_flex()
                        .id("server-overview")
                        .flex_1()
                        .min_h_0()
                        .gap_5()
                        .children(cards)
                        .child(div().text_sm().child(info))
                        .children(players)
                        .children(pack)
                        .overflow_y_scrollbar(),
                )
            })
            .when(self.tab == 1, |c| {
                c.child(self.control.update(cx, |ctl, cx| ctl.console_view(window, cx)))
            })
            .when(self.tab == 2, |c| {
                c.child(
                    div()
                        .id("server-backups")
                        .flex_1()
                        .min_h_0()
                        .child(self.control.update(cx, |ctl, cx| ctl.backups_view(cx)))
                        .overflow_y_scrollbar(),
                )
            })
            .when(self.tab == 3, |c| {
                let root = s.root.clone();
                self.settings_view.update(cx, |v, cx| v.show(root, window, cx));
                let panel = self.control.update(cx, |ctl, cx| ctl.options_view(window, cx));
                c.child(
                    div()
                        .id("server-settings")
                        .flex_1()
                        .min_h_0()
                        .child(
                            v_flex()
                                .gap_6()
                                .children(panel)
                                .child(self.settings_view.clone()),
                        )
                        .overflow_y_scrollbar(),
                )
            })
            .when(self.tab == 4, |c| c.child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .gap_3()
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                div()
                                    .w_56()
                                    .child(Input::new(&self.filter).small().cleanable(true)),
                            )
                            .child(
                                Button::new("rescan-jars")
                                    .ghost()
                                    .small()
                                    .icon(IconName::RefreshCw)
                                    .tooltip("Read the jars again")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(p) = this.picked.clone() {
                                            this.scan(p, true, cx);
                                        }
                                    })),
                            ),
                    )
                    .child(jars),
            ))
            .when(self.tab == TAB_SYNC, |c| {
                c.child(div().flex_1().min_h_0().child(self.sync.clone()))
            })
            .into_any_element()
    }
}

impl ServersView {
    /// Server tools are off: what they do and three ways in. Each way turns
    /// them on and carries on with the job; "Not now" leaves them off.
    fn intro(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let way = |id: &'static str, icon: IconName, title: &'static str, text: &'static str, button: Button| {
            v_flex()
                .id(id)
                .flex_1()
                .min_w_0()
                .gap_1()
                .p_4()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .bg(theme.secondary.opacity(0.4))
                .child(
                    h_flex()
                        .gap_2()
                        .child(Icon::new(icon).small().text_color(theme.muted_foreground))
                        .child(div().text_sm().font_semibold().child(title)),
                )
                .child(div().flex_1().text_xs().text_color(theme.muted_foreground).pb_2().child(text))
                .child(h_flex().child(button))
        };
        let host = Button::new("intro-host")
            .small()
            .primary()
            .label("Pick a pack…")
            .on_click(cx.listener(|this, _, window, cx| {
                this.store.update(cx, |s, cx| s.set_server_tools(true, cx));
                crate::host_dialog::open(this.store.clone(), None, window, cx)
            }));
        let panel = Button::new("intro-panel")
            .small()
            .outline()
            .label("Connect…")
            .on_click(cx.listener(|this, _, _, cx| {
                this.store.update(cx, |s, cx| s.set_server_tools(true, cx));
                cx.emit(ServersEvent::OpenSettings("panel-kind"))
            }));
        let folder = Button::new("intro-folder")
            .small()
            .outline()
            .label("Choose folder…")
            .on_click(cx.listener(|this, _, _, cx| {
                this.store.update(cx, |s, cx| s.set_server_tools(true, cx));
                cx.emit(ServersEvent::OpenSettings("servers-root"))
            }));
        v_flex()
            .id("servers-intro")
            .size_full()
            .items_center()
            .overflow_y_scrollbar()
            .child(
                v_flex()
                    .w_full()
                    .max_w(rems(44.))
                    .px_6()
                    .pt(rems(5.))
                    .pb_6()
                    .gap_5()
                    .items_center()
                    .child(
                        div()
                            .size_12()
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(theme.muted)
                            .child(Icon::new(IconName::Server).text_color(theme.muted_foreground)),
                    )
                    .child(
                        v_flex()
                            .gap_1p5()
                            .items_center()
                            .child(div().text_lg().font_semibold().child("Run a server for your packs"))
                            .child(
                                div()
                                    .text_sm()
                                    .text_center()
                                    .text_color(theme.muted_foreground)
                                    .child("Host one on this PC, connect a server panel you already use, or keep a server's mods in step with its pack."),
                            ),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .gap_3()
                            .items_stretch()
                            .child(way(
                                "intro-way-host",
                                IconName::Monitor,
                                "Host on this PC",
                                "From a pack's server files, Java picked for you.",
                                host,
                            ))
                            .child(way(
                                "intro-way-panel",
                                IconName::SquareTerminal,
                                "Connect a panel",
                                "Crafty, Pterodactyl / Pelican, MCSS or Hopper Server.",
                                panel,
                            ))
                            .child(way(
                                "intro-way-folder",
                                IconName::FolderOpen,
                                "Add a server folder",
                                "A server you run yourself, to sync with its pack.",
                                folder,
                            )),
                    )
                    .child(
                        Button::new("intro-not-now")
                            .ghost()
                            .small()
                            .label("Not now")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ServersEvent::Leave))),
                    ),
            )
            .into_any_element()
    }
}

impl Render for ServersView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.store.read(cx).server_tools() {
            return self.intro(cx);
        }
        let view = cx.entity();
        let layout = self.list_panel.layout(self.store.read(cx).settings(), window);
        let collapsed = layout.collapsed;
        // The Sync tab fits its rows to the room beside the list.
        let room = window.viewport_size().width / window.rem_size() - layout.width;
        self.sync.update(cx, |p, _| p.room = Some(room));
        let list = server_list(
            "servers-list",
            &self.store,
            self.picked.as_ref(),
            collapsed,
            window.rem_size(),
            Rc::new(move |root, _, cx| view.update(cx, |v, cx| v.pick(root, cx))),
            {
                let store = self.store.clone();
                Some(Rc::new(move |window: &mut Window, cx: &mut App| {
                    crate::host_dialog::open(store.clone(), None, window, cx)
                }))
            },
            cx,
        )
        .overflow_y_scrollbar();
        let theme = cx.theme();
        let (bg, border) = (theme.sidebar, theme.sidebar_border);
        let list = widgets::side_panel("servers-list-resize", &self.list_panel, |p| &mut p.list_panel, &self.store, list, window, cx)
            .bg(bg)
            .border_r_1()
            .border_color(border);
        let server = self
            .picked
            .as_ref()
            .and_then(|p| self.store.read(cx).server(p))
            .cloned();
        let detail: AnyElement = match server {
            Some(s) => widgets::enter(
                ElementId::Name(format!("server-detail:{}", s.root.display()).into()),
                div().size_full().child(self.detail(&s, window, cx)),
            ),
            None if self.store.read(cx).servers().is_empty() => {
                let store = self.store.clone();
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .child(no_servers(
                        Some(Rc::new(move |window: &mut Window, cx: &mut App| {
                            crate::host_dialog::open(store.clone(), None, window, cx)
                        })),
                        cx,
                    ))
                    .into_any_element()
            }
            None => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(widgets::caption("Pick a server.", cx))
                .into_any_element(),
        };
        h_flex()
            .size_full()
            .items_stretch()
            .child(list)
            .child(div().flex_1().min_w_0().h_full().child(detail))
            .into_any_element()
    }
}
