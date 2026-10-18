//! The Servers page (what each server is and runs) and the Server sync page
//! (each server against its pack).

use crate::rigsync::SyncPanel;
use crate::server_control::{ControlEvent, ServerControl};
use crate::server_settings::ServerSettings;
use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::description_list::DescriptionList;
use gpui_kit::component::input::{Input, InputEvent, InputState};
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
use std::path::{Path, PathBuf};
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

/// The server list down the leading side, shared by both pages.
fn server_list(
    id: &'static str,
    store: &Entity<Store>,
    picked: Option<&PathBuf>,
    with_pairing: bool,
    rem: Pixels,
    on_pick: Rc<dyn Fn(PathBuf, &mut Window, &mut App)>,
    on_host: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    cx: &App,
) -> Stateful<Div> {
    let s = store.read(cx);
    let theme = cx.theme();
    let instances: Vec<(PathBuf, String)> = s
        .instances()
        .iter()
        .map(|i| (i.root.clone(), i.name.clone()))
        .collect();
    let mut shown_local = false;
    let rows = s.servers().iter().map(|srv| {
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
        let pairing = with_pairing.then(|| {
            let items: Vec<(String, bool)> = instances
                .iter()
                .map(|(_, n)| (n.clone(), paired.as_deref() == Some(n.as_str())))
                .collect();
            let roots: Vec<PathBuf> = instances.iter().map(|(r, _)| r.clone()).collect();
            let (store, server_root) = (store.clone(), srv.root.clone());
            widgets::picker(
                "pair",
                paired.clone().unwrap_or("Pick its pack".into()),
                items,
                rem,
                Rc::new(move |ix, _, cx| {
                    if let Some(r) = roots.get(ix) {
                        let (r, sr) = (r.clone(), server_root.clone());
                        store.update(cx, |s, cx| s.set_pairing(&sr, &r, cx));
                    }
                }),
            )
        });
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
            .cursor_pointer()
            .when(active, |r| {
                r.bg(linear_gradient(
                    90.,
                    linear_color_stop(theme.primary.opacity(0.12), 0.),
                    linear_color_stop(theme.primary.opacity(0.03), 1.),
                ))
                .border_color(theme.primary.opacity(0.3))
            })
            .when(!active, |r| r.hover(|s| s.bg(theme.list_hover).border_color(theme.border)))
            .on_click(move |_, w, cx| pick(root.clone(), w, cx))
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
                    .when_some(s.panel_stats(&srv.root), |r, p| {
                        let live = p.running && !p.starting;
                        let text = if live {
                            format!("{}/{}", p.online, p.max)
                        } else {
                            p.state().to_string()
                        };
                        let color = if live {
                            status_color(s.status(&srv.root), Some(p), cx)
                        } else {
                            theme.muted_foreground
                        };
                        r.child(widgets::pill(text, color, live))
                    }),
            )
            .when(!with_pairing, |r| {
                r.child(
                    widgets::caption(paired.clone().unwrap_or("No pack paired".into()), cx)
                        .text_xs()
                        .pl_4(),
                )
            })
            .children(pairing.map(|p| div().pl_4().child(p)));
        v_flex().children(local_head).child(row)
    });
    let empty = s.servers().is_empty();
    v_flex()
        .id(id)
        .w_72()
        .flex_none()
        .h_full()
        .bg(theme.sidebar)
        .border_r_1()
        .border_color(theme.sidebar_border)
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
                        .child(div().flex_1().text_lg().font_semibold().child("Servers"))
                        .when_some(on_host.clone(), |r, host| {
                            r.child(
                                Button::new("host-server")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Plus)
                                    .tooltip("Host a server on this PC")
                                    .on_click(move |_, window, cx| host(window, cx)),
                            )
                        }),
                )
                .child(widgets::caption(s.settings().servers_root.clone(), cx).text_xs()),
        )
        .children(rows)
        .when(empty, |r| {
            r.child(
                v_flex()
                    .px_2()
                    .py_4()
                    .gap_2()
                    .child(widgets::caption("No servers yet.", cx))
                    .when_some(on_host, |c, host| {
                        c.child(
                            Button::new("host-server-empty")
                                .small()
                                .primary()
                                .icon(IconName::Plus)
                                .label("Host a server")
                                .on_click(move |_, window, cx| host(window, cx)),
                        )
                    }),
            )
        })
}

// ---- Servers page --------------------------------------------------------

pub struct ServersView {
    store: Entity<Store>,
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
    /// Overview, Console, Backups, Settings or Mods.
    tab: usize,
    _subscriptions: Vec<Subscription>,
}

/// What the Servers page asks the app to do.
pub enum ServersEvent {
    Compare(PathBuf),
    /// Set up the server panel.
    OpenSettings,
}

impl EventEmitter<ServersEvent> for ServersView {}

impl ServersView {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter jars"));
        let control = cx.new(|cx| ServerControl::new(store.clone(), window, cx));
        let settings_view = cx.new(|cx| ServerSettings::new(store.clone(), window, cx));
        let subs = vec![
            cx.observe(&control, |_, _, cx| cx.notify()),
            cx.subscribe(&control, |_, _, e: &ControlEvent, cx| match e {
                ControlEvent::SetUp => cx.emit(ServersEvent::OpenSettings),
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
        let mut v = Self {
            store,
            picked: None,
            mods: HashMap::new(),
            filter,
            query: String::new(),
            scroll: UniformListScrollHandle::new(),
            control,
            settings_view,
            tab: 0,
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
        self.tab = tab;
        cx.notify();
    }

    pub(crate) fn pick(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.picked = Some(root.clone());
        self.scan(root.clone(), false, cx);
        self.control.update(cx, |c, cx| c.show(root.clone(), cx));
        cx.notify();
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
        let live = self.control.read(cx).live();
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
        for (label, value) in live {
            info = info.item(label, value, 1);
        }
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
        let (root, root2, server) = (s.root.clone(), s.root.clone(), s.clone());
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
                                "Runs through {}. Files sync from Server sync.",
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
                    .when(paired.is_some(), |r| {
                        r.child(
                            Button::new("compare")
                                .primary()
                                .small()
                                .icon(IconName::ArrowLeftRight)
                                .label("Compare with pack")
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(ServersEvent::Compare(root2.clone()))
                                })),
                        )
                    }),
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
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.tab = *ix;
                        let open = *ix == 1;
                        this.control.update(cx, |c, cx| c.set_console_open(open, cx));
                        cx.notify();
                    })),
            )
            .when(self.tab == 0, |c| {
                let players = self.control.update(cx, |ctl, cx| ctl.players_view(cx));
                let cards = self.control.update(cx, |ctl, cx| ctl.stat_cards(cx));
                let pack = (pack_id != 0).then(|| {
                    let store = self.store.read(cx);
                    let page = store.pack_page(pack_id);
                    let icon = match page {
                        Some(crate::store::PackPage::Ready(m, ..)) => m.icon().and_then(|u| store.image(u)),
                        _ => None,
                    };
                    v_flex()
                        .gap_3()
                        .child(div().text_base().font_weight(FontWeight::BOLD).child("Modpack"))
                        .child(widgets::pack_page(page, icon, &self.store, cx))
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
                c.child(self.control.update(cx, |ctl, cx| ctl.console_view(cx)))
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
                c.child(
                    div()
                        .id("server-settings")
                        .flex_1()
                        .min_h_0()
                        .child(self.settings_view.clone())
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
            .into_any_element()
    }
}

impl Render for ServersView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let list = server_list(
            "servers-list",
            &self.store,
            self.picked.as_ref(),
            false,
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
    }
}

// ---- Server sync page -------------------------------------------------------

pub struct SyncPage {
    store: Entity<Store>,
    panel: Entity<SyncPanel>,
    /// The first paired server was opened on its own, once.
    auto_picked: bool,
    _subscriptions: Vec<Subscription>,
}

impl SyncPage {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let panel = cx.new(|cx| SyncPanel::new(store.clone(), window, cx));
        let subs = vec![cx.observe(&store, |_, _, cx| cx.notify())];
        Self {
            store,
            panel,
            auto_picked: false,
            _subscriptions: subs,
        }
    }

    pub fn compare(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.panel.update(cx, |p, cx| p.show(root, cx));
        cx.notify();
    }

    /// The server compared, for back and forward.
    pub(crate) fn nav_loc(&self, cx: &App) -> Option<PathBuf> {
        self.panel.read(cx).server().map(Path::to_path_buf)
    }
}

impl Render for SyncPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let picked = self.panel.read(cx).server().map(|p| p.to_path_buf());
        // Nothing picked yet: open the first server that has a pack.
        if picked.is_none() && !self.auto_picked {
            let store = self.store.read(cx);
            let first = store
                .servers()
                .iter()
                .find(|s| store.paired(s).is_some())
                .map(|s| s.root.clone());
            if let Some(root) = first {
                self.auto_picked = true;
                let view = view.clone();
                window.defer(cx, move |_, cx| view.update(cx, |v, cx| v.compare(root, cx)));
            }
        }
        let list = server_list(
            "sync-list",
            &self.store,
            picked.as_ref(),
            true,
            window.rem_size(),
            Rc::new(move |root, _, cx| view.update(cx, |v, cx| v.compare(root, cx))),
            None,
            cx,
        )
        .overflow_y_scrollbar();
        h_flex()
            .size_full()
            .items_stretch()
            .child(list)
            .child(div().flex_1().min_w_0().h_full().child(self.panel.clone()))
    }
}
