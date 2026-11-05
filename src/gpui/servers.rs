//! The Servers page: what each server is and runs, and (its Sync tab) the
//! server against its pack. With server tools off, an intro to them.

use crate::panel_tabs;
use crate::widgets::Clickable as _;
use hopper::hopperd::is_remote_root;
use crate::rigsync::SyncPanel;
use crate::server_control::{ControlEvent, ServerControl};
use crate::server_settings::ServerSettings;
use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::description_list::DescriptionList;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::model::ModInfo;
use hopper::panel::PanelStats;
use hopper::server::{Manager, ServerInfo, Status, ping_text};
use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

/// The color of a server's state light: the panel's state when there is
/// one, else whether the server answers pings.
fn status_color(status: Option<&Status>, panel: Option<&PanelStats>, cx: &App) -> Hsla {
    let theme = cx.theme();
    match (panel, status) {
        (Some(p), _) if p.progress.as_ref().is_some_and(|g| g.stuck) => theme.danger,
        (Some(p), _) if p.crashed => theme.danger,
        (Some(p), _) if p.starting || p.stopping || p.updating => theme.warning,
        // Running in the panel but not answering yet: still loading.
        (Some(p), Some(Status::Online { .. })) if p.running => theme.success,
        (Some(p), _) if p.running => theme.warning,
        (Some(_), _) => theme.muted_foreground,
        (None, Some(Status::Online { .. })) => theme.success,
        (None, Some(Status::Offline)) => theme.muted_foreground,
        (None, None) => theme.border,
    }
}

/// The server's last ping as the button that pings it again: signal bars
/// and the round trip, a spinner while a ping is on its way, a red cross
/// when it didn't answer.
fn ping_button(store: &Entity<Store>, server: &ServerInfo, cx: &App) -> Button {
    let s = store.read(cx);
    let status = s.status(&server.root);
    let (store, srv) = (store.clone(), server.clone());
    let b = Button::new("ping")
        .outline()
        .small()
        .tooltip("Ping it again, like the multiplayer screen")
        .on_click(move |_, _, cx| store.update(cx, |s, cx| {
            s.ping(&srv);
            cx.notify();
        }));
    if s.pinging(&server.root) {
        return b.icon(IconName::Activity).loading(true).label("Pinging…");
    }
    match status {
        None => b.icon(IconName::Activity).label("Ping"),
        Some(st) => b
            .child(widgets::signal_bars(st.ms(), px(12.), cx))
            .child(match st.ms() {
                Some(ms) => ping_text(ms),
                None => "Didn't answer".into(),
            }),
    }
}


/// Small signal bars for a server that answers, with its ping on hover
/// (the server list's rows and the Library's Server tab).
pub(crate) fn ping_bars(root: &std::path::Path, ms: u32, cx: &App) -> Stateful<Div> {
    div()
        .id(ElementId::Name(format!("ping-bars:{}", root.display()).into()))
        .flex_none()
        .child(widgets::signal_bars(Some(ms), px(10.), cx))
        .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(ping_text(ms)).build(w, cx))
}

/// A server without a picture: its initials on a tile tinted from its name
/// (the same name always gets the same tint).
fn letter_tile(name: &str, text: Rems) -> Div {
    let initials: String = name
        .split(|c: char| !c.is_alphanumeric())
        .filter_map(|w| w.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase();
    let hue = name.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32)) % 360;
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(hsla(hue as f32 / 360., 0.32, 0.3, 1.))
        .text_color(hsla(hue as f32 / 360., 0.5, 0.88, 1.))
        .text_size(text)
        .font_weight(FontWeight::BOLD)
        .child(initials)
}

/// A server's picture (its server-icon.png or its pack's logo, else a
/// letter tile) with its state light on the corner, like a status badge.
fn server_thumb(art: Option<&std::path::Path>, name: &str, size: Rems, light: Option<Hsla>, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .relative()
        .flex_none()
        .size(size)
        .child(
            div()
                .size_full()
                .rounded(theme.radius)
                .overflow_hidden()
                .bg(theme.muted)
                .map(|d| match art {
                    Some(p) => d.child(img(p.to_path_buf()).size_full().object_fit(ObjectFit::Cover)),
                    None => d.child(letter_tile(name, size * 0.36)),
                }),
        )
        .when_some(light, |d, c| {
            d.child(
                div()
                    .absolute()
                    .right(px(-3.))
                    .bottom(px(-3.))
                    .size(px(11.))
                    .rounded_full()
                    .bg(c)
                    .border_2()
                    .border_color(theme.sidebar),
            )
        })
}

/// A MOTD as the multiplayer list draws it: Minecraft's colors, bold and
/// italic; its two lines side by side.
fn motd_line(raw: &str, cx: &App) -> Option<Div> {
    let text = hopper::instance_ops::unescape_property(raw).replace('\n', "  ");
    let spans = hopper::instance_ops::mc_spans(&text);
    if spans.iter().all(|s| s.text.trim().is_empty()) {
        return None;
    }
    let fg = cx.theme().foreground;
    Some(
        h_flex()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .children(spans.into_iter().map(move |s| {
                let color = s.color.map_or(fg, |c| rgb(c).into());
                div()
                    .flex_none()
                    .text_color(color)
                    .when(s.bold, |d| d.font_weight(FontWeight::BOLD))
                    .when(s.italic, |d| d.italic())
                    .when(s.underline, |d| d.underline())
                    .when(s.strike, |d| d.line_through())
                    .child(s.text)
            })),
    )
}

/// The state word for a header pill: short, as the list's pills say it.
fn short_state(status: Option<&Status>, panel: Option<&PanelStats>) -> String {
    match (panel, status) {
        (Some(p), _) if p.progress.is_some() => p.progress.as_ref().map(|g| g.pill()).unwrap_or_default(),
        (Some(p), _) if p.crashed => "Crashed".into(),
        (Some(p), _) if p.starting => "Starting".into(),
        (Some(p), _) if p.running => {
            let up = p.up_text();
            if up.is_empty() { "Running".into() } else { format!("Running · {}", up.trim_start_matches("Up ").trim_start_matches("up ")) }
        }
        (Some(_), _) => "Stopped".into(),
        (None, Some(Status::Online { .. })) => "Online".into(),
        (None, Some(Status::Offline)) => "Offline".into(),
        (None, None) => "Checking".into(),
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
    arts: &HashMap<PathBuf, PathBuf>,
    collapsed: bool,
    rem: Pixels,
    on_pick: Rc<dyn Fn(PathBuf, &mut Window, &mut App)>,
    on_host: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    cx: &App,
) -> Stateful<Div> {
    if collapsed {
        return server_rail(id, store, picked, arts, on_pick, cx);
    }
    let s = store.read(cx);
    let theme = cx.theme();
    let instances: Vec<(PathBuf, String)> = s
        .instances()
        .iter()
        .map(|i| (i.root.clone(), i.name.clone()))
        .collect();
    // Running servers first within each group.
    let running = |srv: &ServerInfo| {
        s.panel_stats(&srv.root).map_or_else(
            || matches!(s.status(&srv.root), Some(Status::Online { .. })),
            |p| p.running || p.starting,
        )
    };
    let row_for = |srv: &ServerInfo| {
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
                    .text_color(theme.muted_foreground)
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
        // One quiet line: loader and pack; the address and other panel
        // in its tooltip.
        let mut sub: Vec<String> = Vec::new();
        if let Some(l) = srv.loader.as_deref().filter(|l| !l.trim().is_empty()) {
            sub.push(hopper::server::loader_facts(l).0);
        }
        if let Some(p) = &paired {
            sub.push(p.clone());
        }
        let tip: Option<SharedString> = caption_for(s, srv).map(Into::into);
        let row = h_flex()
            .id(ElementId::Name(
                format!("server:{}", srv.root.display()).into(),
            ))
            .gap_3()
            .px_2()
            .py_1p5()
            .rounded(theme.radius_lg)
            .clickable(cx)
            .when(active, |r| widgets::row_selected(r, cx))
            .when(!active, |r| r.hover(|s| widgets::row_hover(s, cx)))
            .on_click(move |_, w, cx| pick(root.clone(), w, cx))
            .context_menu({
                let (store, root) = (store.clone(), srv.root.clone());
                let pack = s.paired(srv).map(|i| i.root.clone());
                move |menu, _, cx| {
                    let go = |compare: bool| {
                        let (store, root) = (store.clone(), root.clone());
                        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                            store.update(cx, |_, cx| {
                                cx.emit(crate::store::StoreEvent::ShowServer { root: root.clone(), compare })
                            })
                        }
                    };
                    let r = root.clone();
                    // Read when the menu opens: Start, or Restart and Stop,
                    // and the first address players join by (as the web list).
                    let power = crate::server_power::menu_items(&store, &root, cx);
                    let address = {
                        let s = store.read(cx);
                        match s.panel_stats(&root) {
                            Some(p) if !p.addresses.is_empty() => p.addresses.first().cloned(),
                            _ => hopper::hosting::router::RouteSettings::load(&root).addresses.into_iter().next(),
                        }
                    };
                    let menu = menu.item(PopupMenuItem::new("Open").icon(IconName::Server).on_click(go(false)));
                    let menu = if power.is_empty() {
                        menu
                    } else {
                        power.into_iter().fold(menu.separator(), |m, (label, icon, a)| {
                            let (store, root) = (store.clone(), root.clone());
                            m.item(PopupMenuItem::new(label).icon(icon).on_click(move |_, window, cx| {
                                crate::server_power::request(store.clone(), root.clone(), a, window, cx)
                            }))
                        })
                        .separator()
                    };
                    menu
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
                        .when_some(address, |m, a| {
                            m.item(PopupMenuItem::new("Copy address").icon(IconName::Copy).on_click(
                                move |_, window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(a.clone()));
                                    window.push_notification(
                                        gpui_kit::component::notification::Notification::success(format!("Copied {a}")),
                                        cx,
                                    );
                                },
                            ))
                        })
                }
            })
            .child(server_thumb(
                arts.get(&srv.root).map(PathBuf::as_path),
                &srv.name,
                rems(2.5),
                Some(status_color(s.status(&srv.root), s.panel_stats(&srv.root), cx)),
                cx,
            ))
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
                                    .child(srv.name.clone()),
                            )
                            // Its ping, while it answers.
                            .when_some(s.status(&srv.root).and_then(|st| st.ms()), |r, ms| {
                                r.child(ping_bars(&srv.root, ms, cx))
                            })
                            // A stopped server is just the grey light on its
                            // picture; pills are for running, starting and
                            // failing ones.
                            .when_some(s.panel_stats(&srv.root).filter(|p| p.running || p.starting || p.crashed), |r, p| {
                                let live = p.running && !p.starting && !p.stopping && !p.crashed;
                                let text = if let Some(g) = &p.progress {
                                    g.pill()
                                } else if live {
                                    format!("{}/{}", p.online, p.max)
                                } else {
                                    p.state().to_string()
                                };
                                r.when_some(p.tps.filter(|_| live), |r, t| {
                                    r.child(widgets::tps_chip(
                                        ElementId::Name(format!("tps:{}", srv.root.display()).into()),
                                        &t,
                                        cx,
                                    ))
                                })
                                .child(widgets::pill(text, status_color(s.status(&srv.root), Some(p), cx), live))
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .min_w_0()
                            .when(!sub.is_empty(), |r| {
                                r.child(
                                    widgets::caption(sub.join(" · "), cx)
                                        .id(ElementId::Name(format!("server-sub:{}", srv.root.display()).into()))
                                        .text_xs()
                                        .min_w_0()
                                        .truncate()
                                        .when_some(tip, |d, t| {
                                            d.tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(t.clone()).build(w, cx))
                                        }),
                                )
                            })
                            .children(pairing.map(|p| div().flex_none().child(p))),
                    ),
            );
        row.into_any_element()
    };
    let mut rows: Vec<AnyElement> = Vec::new();
    for g in groups(s) {
        let folded = s.settings().collapsed_groups.contains(&g.key);
        let mut members: Vec<&ServerInfo> = s.servers().iter().filter(|x| g.has(s, x)).collect();
        if members.is_empty() && matches!(g.kind, GroupKind::Folder) {
            continue;
        }
        members.sort_by_key(|srv| !running(srv));
        rows.push(group_head(&g, members.len(), folded, store, on_host.clone(), cx));
        if !folded {
            rows.extend(members.into_iter().map(|srv| row_for(srv)));
        }
    }
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
                        .child({
                            let store = store.clone();
                            Button::new("new-server")
                                .ghost()
                                .small()
                                .icon(IconName::Plus)
                                .tooltip("New server")
                                .on_click(move |_, window, cx| crate::host_dialog::open_on(store.clone(), None, None, window, cx))
                        })
                        .child(collapse),
                ),
        )
        .children(rows)
        .when(empty, |r| {
            r.child(no_servers(on_host, cx))
        })
}

/// Where a group of servers in the list comes from.
enum GroupKind {
    /// A paired Hopper Server, by id.
    Panel(String),
    /// The servers folder (and the Crafty / MCSS panel that runs it).
    Folder,
    /// Servers over SFTP / FTP, by site id.
    Site(String),
    /// Servers Hopper hosts on this PC.
    Local,
}

struct Group {
    kind: GroupKind,
    /// For `collapsed_groups`.
    key: String,
    name: String,
    /// What it is ("Hopper Server", "Crafty").
    what: String,
}

impl Group {
    fn has(&self, s: &Store, x: &ServerInfo) -> bool {
        match &self.kind {
            GroupKind::Panel(id) => x.remote.as_ref().is_some_and(|r| &r.panel == id),
            GroupKind::Local => s.is_hosted(&x.root),
            GroupKind::Folder => x.remote.is_none() && !s.is_hosted(&x.root) && s.site_of(&x.root).is_none(),
            GroupKind::Site(id) => s.site_of(&x.root).as_ref() == Some(id),
        }
    }
}

/// The list's groups: each paired Hopper Server, then the servers folder,
/// then this PC.
fn groups(s: &Store) -> Vec<Group> {
    let st = s.settings();
    let mut out: Vec<Group> = st
        .hopper_panels
        .iter()
        .map(|p| Group {
            kind: GroupKind::Panel(p.id.clone()),
            key: format!("panel:{}", p.id),
            name: p.name.clone(),
            what: "Hopper Server".into(),
        })
        .collect();
    let folder = st.servers_root.trim();
    out.push(Group {
        kind: GroupKind::Folder,
        key: "folder".into(),
        name: if folder.is_empty() { "Server folder".into() } else { folder.to_string() },
        what: {
            let mut kinds: Vec<&str> = hopper::panel::panels(st).iter().map(|p| hopper::panel::short_name(&p.kind)).collect();
            kinds.dedup();
            kinds.join(", ")
        },
    });
    out.extend(st.file_sites.iter().map(|f| Group {
        kind: GroupKind::Site(f.id.clone()),
        key: format!("site:{}", f.id),
        name: f.label(),
        what: f.proto.to_uppercase(),
    }));
    out.push(Group { kind: GroupKind::Local, key: "local".into(), name: "This PC".into(), what: "hosted by Hopper".into() });
    out
}

/// A group's header: fold chevron, state light, name, what it is, and a +
/// to make a server there.
fn group_head(
    g: &Group,
    n: usize,
    folded: bool,
    store: &Entity<Store>,
    on_host: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let s = store.read(cx);
    let (light, error) = match &g.kind {
        GroupKind::Panel(id) => match s.panel_error(id) {
            Some(e) => (Some(theme.danger), Some(e.to_string())),
            None if s.panel_listed(id) => (Some(theme.success), None),
            None => (Some(theme.warning), None),
        },
        GroupKind::Site(id) => match s.site_error(id) {
            Some(e) => (Some(theme.danger), Some(e.to_string())),
            None if s.site_listed(id) => (Some(theme.success), None),
            None => (Some(theme.warning), None),
        },
        _ => (None, None),
    };
    let key = g.key.clone();
    let fold = {
        let store = store.clone();
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            let key = key.clone();
            store.update(cx, |s, cx| {
                s.update_settings(
                    |st| {
                        if !st.collapsed_groups.remove(&key) {
                            st.collapsed_groups.insert(key);
                        }
                    },
                    cx,
                )
            })
        }
    };
    let what = if g.what.is_empty() { format!("{n}") } else { format!("{} · {n}", g.what) };
    let add: Option<AnyElement> = match &g.kind {
        GroupKind::Panel(id) => {
            let (store, id) = (store.clone(), id.clone());
            Some(
                Button::new(ElementId::Name(format!("group-new:{}", g.key).into()))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .tooltip(format!("New server on {}", g.name))
                    .on_click(move |_, window, cx| {
                        // Not a click on the header too (that folds it).
                        cx.stop_propagation();
                        crate::host_dialog::open_on(store.clone(), Some(id.clone()), None, window, cx)
                    })
                    .into_any_element(),
            )
        }
        GroupKind::Local => on_host.map(|host| {
            Button::new("group-new:local")
                .ghost()
                .xsmall()
                .icon(IconName::Plus)
                .tooltip("Host a server on this PC")
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    host(window, cx)
                })
                .into_any_element()
        }),
        GroupKind::Folder | GroupKind::Site(_) => None,
    };
    let web = match &g.kind {
        GroupKind::Panel(id) => s.settings().hopper_panels.iter().find(|p| &p.id == id).map(|p| p.url.clone()),
        _ => None,
    };
    v_flex()
        .child(
            h_flex()
                .id(ElementId::Name(format!("group:{}", g.key).into()))
                .px_2()
                .pt_3()
                .pb_1()
                .gap_1p5()
                .rounded(theme.radius)
                .clickable(cx)
                .hover(|st| widgets::row_hover(st, cx))
                .on_click(fold)
                .context_menu(move |menu, _, _| match web.clone() {
                    Some(url) => menu.item(
                        PopupMenuItem::new("Open in browser")
                            .icon(IconName::ExternalLink)
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    ),
                    None => menu,
                })
                .child(
                    Icon::new(if folded { IconName::ChevronRight } else { IconName::ChevronDown })
                        .xsmall()
                        .text_color(theme.muted_foreground),
                )
                .when_some(light, |r, c| r.child(div().size_1p5().flex_none().rounded_full().bg(c)))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .font_semibold()
                        .text_color(theme.foreground)
                        .child(g.name.clone()),
                )
                .child(div().flex_1().min_w_0().truncate().text_xs().text_color(theme.muted_foreground).child(what))
                .children(add),
        )
        .when_some(error.filter(|_| !folded), |c, e| {
            c.child(div().px_3().pb_1().text_xs().text_color(theme.danger).child(e))
        })
        .into_any_element()
}

/// The quiet line under a server's name: its address on a Hopper Server,
/// and whether the servers folder's panel runs it too.
fn caption_for(s: &Store, srv: &ServerInfo) -> Option<String> {
    srv.remote.as_ref()?;
    let mut parts: Vec<String> = Vec::new();
    if let Some(l) = &srv.loader {
        parts.push(l.clone());
    }
    if let Some(a) = s.panel_stats(&srv.root).and_then(|p| p.addresses.first()) {
        parts.push(a.clone());
    }
    if s.also_in_folder(srv) || !matches!(srv.manager, Manager::Unknown) {
        parts.push(format!(
            "also in {}",
            match srv.manager {
                Manager::Mcss => "MCSS",
                _ => "Crafty",
            }
        ));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// The collapsed server list: a tile per server with its status light,
/// and a button to open the list again.
fn server_rail(
    id: &'static str,
    store: &Entity<Store>,
    picked: Option<&PathBuf>,
    arts: &HashMap<PathBuf, PathBuf>,
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
            div()
                .id(ElementId::Name(format!("server-tile:{}", srv.root.display()).into()))
                .relative()
                .size(rems(2.75))
                .flex_none()
                .p_0p5()
                .rounded(theme.radius_lg)
                .clickable(cx)
                .when(active, |r| widgets::row_selected(r, cx))
                .when(!active, |r| r.hover(|s| widgets::row_hover(s, cx)))
                .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(name.clone()).build(w, cx))
                .on_click(move |_, w, cx| pick(root.clone(), w, cx))
                .child(server_thumb(
                    arts.get(&srv.root).map(PathBuf::as_path),
                    &srv.name,
                    rems(2.5),
                    Some(status_color(s.status(&srv.root), s.panel_stats(&srv.root), cx)),
                    cx,
                ))
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
    /// The Logs & crashes tab.
    logs: Entity<crate::server_logs::ServerLogsView>,
    /// The Configs tab: the instance's config editor, on the server's files.
    configs: Entity<crate::configs_tab::ConfigsTab>,
    /// The one-line crash notice above every tab but Logs & crashes.
    strip: Entity<crate::server_crash::CrashStrip>,
    /// The tabs only a Hopper Server's servers have.
    p_files: Entity<panel_tabs::files::FilesTab>,
    p_mods: Entity<panel_tabs::mods::ModsTab>,
    /// The header's History button for the picked panel server.
    history_btn: std::cell::RefCell<Option<Entity<crate::history_button::HistoryButton>>>,
    p_players: Entity<panel_tabs::players::PlayersTab>,
    p_backups: Entity<panel_tabs::backups::BackupsTab>,
    p_schedule: Entity<panel_tabs::schedule::ScheduleTab>,
    p_settings: Entity<panel_tabs::settings::SettingsTab>,
    /// The picked server's panel, when it's on a Hopper Server.
    target: Option<Result<panel_tabs::Target, String>>,
    /// The (server, tab) a panel tab was last shown for.
    p_shown: Option<(PathBuf, usize)>,
    /// Which tab (one of the `TAB_*` codes).
    tab: usize,
    /// The Sync tab was open when the page was left.
    sync_away: bool,
    /// The Overview's modpack readme shows in full.
    pack_open: bool,
    /// Each server's server-icon.png on this PC, by root: (the panel's
    /// stamp for it, the file once there).
    icons: HashMap<PathBuf, (Option<u64>, Option<PathBuf>)>,
    /// Header art blurred for its backdrop, by art path.
    blurs: HashMap<PathBuf, Option<PathBuf>>,
    /// The detail area's width in rem, for the header's figures.
    detail_w: f32,
    _subscriptions: Vec<Subscription>,
}

/// The Sync tab's code (see [`tabs_for`]).
pub const TAB_SYNC: usize = 5;
const TAB_OVERVIEW: usize = 0;
const TAB_CONSOLE: usize = 1;
const TAB_BACKUPS: usize = 2;
const TAB_SETTINGS: usize = 3;
const TAB_MODS: usize = 4;
const TAB_FILES: usize = 6;
const TAB_PLAYERS: usize = 7;
const TAB_SCHEDULE: usize = 8;
const TAB_LOGS: usize = 9;
const TAB_CONFIGS: usize = 10;

/// A server's tabs, in order, as codes: a Hopper Server's servers have the
/// panel's tabs (in the panel's order), others the ones Hopper reads itself.
fn tabs_for(remote: bool) -> &'static [usize] {
    if remote {
        &[TAB_OVERVIEW, TAB_CONSOLE, TAB_LOGS, TAB_FILES, TAB_MODS, TAB_CONFIGS, TAB_PLAYERS, TAB_BACKUPS, TAB_SCHEDULE, TAB_SETTINGS, TAB_SYNC]
    } else {
        &[TAB_OVERVIEW, TAB_CONSOLE, TAB_LOGS, TAB_BACKUPS, TAB_SETTINGS, TAB_MODS, TAB_CONFIGS, TAB_SYNC]
    }
}

fn tab_icon(code: usize) -> IconName {
    match code {
        TAB_OVERVIEW => IconName::Info,
        TAB_CONSOLE => IconName::SquareTerminal,
        TAB_BACKUPS => IconName::Archive,
        TAB_SETTINGS => IconName::Settings2,
        TAB_MODS => IconName::Puzzle,
        TAB_FILES => IconName::FolderOpen,
        TAB_PLAYERS => IconName::Users,
        TAB_SCHEDULE => IconName::Clock,
        TAB_LOGS => IconName::Stethoscope,
        // The same icon as an instance's Configs tab.
        TAB_CONFIGS => IconName::FileCog,
        _ => IconName::ArrowLeftRight,
    }
}

fn tab_name(code: usize) -> &'static str {
    match code {
        TAB_OVERVIEW => "Overview",
        TAB_CONSOLE => "Console",
        TAB_BACKUPS => "Backups",
        TAB_SETTINGS => "Settings",
        TAB_MODS => "Mods",
        TAB_FILES => "Files",
        TAB_PLAYERS => "Players",
        TAB_SCHEDULE => "Schedule",
        TAB_LOGS => "Logs & crashes",
        TAB_CONFIGS => "Configs",
        _ => "Sync",
    }
}

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
        let strip = cx.new(|_| crate::server_crash::CrashStrip::new(store.clone()));
        let subs = vec![
            cx.observe(&control, |_, _, cx| cx.notify()),
            cx.observe(&sync, |_, _, cx| cx.notify()),
            cx.observe(&strip, |_, _, cx| cx.notify()),
            cx.subscribe(&strip, |this, _, e: &crate::server_crash::StripEvent, cx| match e {
                crate::server_crash::StripEvent::SeeWhy(rel) => this.see_crash(rel.clone(), cx),
                crate::server_crash::StripEvent::Fixed => {
                    if let Some(r) = this.picked.clone() {
                        this.scan(r, true, cx);
                    }
                }
            }),
            cx.subscribe(&control, |this, _, e: &ControlEvent, cx| match e {
                ControlEvent::SetUp => cx.emit(ServersEvent::OpenSettings("panel-url")),
                // This page has a Console tab of its own.
                ControlEvent::Console => this.set_tab(TAB_CONSOLE, cx),
            }),
            cx.subscribe(&store, |this, _, e: &crate::store::StoreEvent, cx| {
                if let crate::store::StoreEvent::PanelChanged(root, areas) = e {
                    this.panel_changed(root, areas, cx);
                }
            }),
            cx.observe(&store, |this, _, cx| {
                this.ensure_pick(cx);
                this.want_icons(cx);
                this.retarget(cx);
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
        let logs = cx.new(|cx| crate::server_logs::ServerLogsView::new(store.clone(), window, cx));
        let configs = cx.new(|cx| crate::configs_tab::ConfigsTab::new(store.clone(), window, cx));
        let p_files = cx.new(|cx| panel_tabs::files::FilesTab::new(store.clone(), window, cx));
        let p_mods = cx.new(|cx| panel_tabs::mods::ModsTab::new(store.clone(), window, cx));
        let p_players = cx.new(|cx| panel_tabs::players::PlayersTab::new(store.clone(), window, cx));
        let p_backups = cx.new(|cx| panel_tabs::backups::BackupsTab::new(store.clone(), window, cx));
        let p_schedule = cx.new(|cx| panel_tabs::schedule::ScheduleTab::new(store.clone(), window, cx));
        let p_settings = cx.new(|cx| panel_tabs::settings::SettingsTab::new(store.clone(), window, cx));
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
            logs,
            configs,
            strip,
            p_files,
            p_mods,
            history_btn: Default::default(),
            p_players,
            p_backups,
            p_schedule,
            p_settings,
            target: None,
            p_shown: None,
            tab: 0,
            sync_away: false,
            pack_open: false,
            icons: HashMap::new(),
            blurs: HashMap::new(),
            detail_w: 80.0,
            _subscriptions: subs,
        };
        v.ensure_pick(cx);
        v.want_icons(cx);
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
        self.retarget(cx);
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
        if was != tab {
            crate::hero_drift::kick(1., (tab as i32 - was as i32).signum() as f32);
        }
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
        if let Some(r) = self.picked.clone() {
            self.strip.update(cx, |s, cx| s.refresh(r, cx));
        }
        // Looked at: the Logs & crashes tab's dot goes.
        if tab == TAB_LOGS
            && let Some(k) = self.strip.read(cx).unseen_key()
        {
            self.store.update(cx, |s, cx| s.hide_notice(&k, cx));
        }
        // Logs & crashes polls the server only while it's open.
        if was == TAB_LOGS && tab != TAB_LOGS {
            self.logs.update(cx, |v, cx| v.hidden(cx));
        }
        cx.notify();
    }

    /// Logs & crashes of server `root`, on crash report `rel`.
    pub(crate) fn open_logs(&mut self, root: PathBuf, rel: Option<String>, cx: &mut Context<Self>) {
        if self.picked.as_ref() != Some(&root) {
            self.picked = Some(root.clone());
            self.retarget(cx);
            self.scan(root.clone(), false, cx);
            self.control.update(cx, |c, cx| c.show(root, cx));
        }
        self.see_crash(rel, cx);
    }

    /// "See why" on the crash notice: Logs & crashes, on that report.
    fn see_crash(&mut self, rel: Option<String>, cx: &mut Context<Self>) {
        self.set_tab(TAB_LOGS, cx);
        if let Some(root) = self.picked.clone() {
            self.logs.update(cx, |v, cx| {
                v.show(root, cx);
                v.open_report(rel, cx);
            });
        }
    }

    /// A server's Sync tab (the picked one's when `root` is None).
    pub(crate) fn open_sync(&mut self, root: Option<PathBuf>, cx: &mut Context<Self>) {
        if let Some(r) = root
            && self.picked.as_ref() != Some(&r)
        {
            self.picked = Some(r.clone());
            self.retarget(cx);
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
        // Back in view, drawing the tab starts it again.
        if self.tab == TAB_LOGS {
            self.logs.update(cx, |v, cx| v.hidden(cx));
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

    /// Finds each server's picture: server-icon.png in its folder, or for a
    /// Hopper Server the panel's copy, fetched once per change into the
    /// cache. Asks for paired packs' logos too (the fallback).
    fn want_icons(&mut self, cx: &mut Context<Self>) {
        let store = self.store.read(cx);
        let cache = hopper::paths::dirs().map(|d| d.cache_dir().join("server-icons"));
        let mut fetch = Vec::new();
        let mut logos = Vec::new();
        for srv in store.servers() {
            if let Some(u) = store.paired(srv).and_then(|i| i.icon.clone())
                && store.image(&u).is_none()
            {
                logos.push(u);
            }
            // A folder's icon is looked for once; a panel's when its stamp
            // changes (or first shows).
            let stamp = match &srv.remote {
                Some(_) => store.panel_stats(&srv.root).and_then(|p| p.icon),
                None => Some(0),
            };
            if self.icons.get(&srv.root).is_some_and(|(s, _)| *s == stamp) {
                continue;
            }
            let found = match (&srv.remote, stamp) {
                (_, None) => None,
                (None, Some(_)) => Some(srv.root.join("server-icon.png")).filter(|p| p.is_file()),
                (Some(r), Some(m)) => {
                    let safe = |t: &str| t.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect::<String>();
                    let local = cache.as_ref().map(|c| c.join(format!("{}-{}-{m}.png", safe(&r.panel), safe(&r.id))));
                    match local {
                        Some(l) if l.is_file() => Some(l),
                        Some(l) => {
                            fetch.push((srv.root.clone(), r.clone(), l));
                            None
                        }
                        None => None,
                    }
                }
            };
            self.icons.insert(srv.root.clone(), (stamp, found));
        }
        if !logos.is_empty() {
            self.store.update(cx, |s, _| logos.iter().for_each(|u| s.want_image(u)));
        }
        for (root, remote, local) in fetch {
            cx.spawn(async move |this, cx| {
                let to = local.clone();
                let r = cx
                    .background_spawn(async move {
                        hopper::hopperd::Client::for_remote(&remote)?.icon(&remote.id, &to)
                    })
                    .await;
                match r {
                    Ok(()) => {
                        this.update(cx, |this, cx| {
                            if let Some(e) = this.icons.get_mut(&root) {
                                e.1 = Some(local);
                            }
                            cx.notify();
                        })
                        .ok();
                    }
                    Err(e) => log::debug!("server icon {}: {e:#}", root.display()),
                }
            })
            .detach();
        }
    }

    /// Each server's picture: its own icon, else its pack's logo.
    fn arts(&self, cx: &App) -> HashMap<PathBuf, PathBuf> {
        let store = self.store.read(cx);
        store
            .servers()
            .iter()
            .filter_map(|srv| {
                let own = self.icons.get(&srv.root).and_then(|(_, p)| p.clone());
                let logo = || {
                    let u = store.paired(srv)?.icon.clone()?;
                    store.image(&u).map(std::path::Path::to_path_buf)
                };
                own.or_else(logo).map(|p| (srv.root.clone(), p))
            })
            .collect()
    }

    /// Starts blurring `art` for the header's backdrop, once.
    fn want_blur(&mut self, art: &std::path::Path, cx: &mut Context<Self>) {
        if self.blurs.contains_key(art) {
            return;
        }
        let art = art.to_path_buf();
        self.blurs.insert(art.clone(), None);
        cx.spawn(async move |this, cx| {
            let src = art.clone();
            match cx.background_spawn(async move {
                crate::hero_drift::prepare(&src);
                hopper::imgcache::blurred(&src)
            }).await {
                Ok(p) => {
                    this.update(cx, |this, cx| {
                        this.blurs.insert(art, Some(p));
                        cx.notify();
                    })
                    .ok();
                }
                Err(e) => log::debug!("blur {}: {e:#}", art.display()),
            }
        })
        .detach();
    }

    /// The picked server's panel, made again when it changes.
    fn retarget(&mut self, cx: &mut Context<Self>) {
        let remote = self.picked.as_ref().and_then(|r| self.store.read(cx).server(r)).and_then(|s| s.remote.clone());
        let same = matches!((&self.target, &remote), (Some(Ok(t)), Some(r)) if &t.remote == r);
        if !tabs_for(remote.is_some()).contains(&self.tab) {
            self.tab = TAB_OVERVIEW;
        }
        if !same {
            self.target = remote.map(panel_tabs::Target::new);
            self.p_shown = None;
        }
    }

    /// Its server changed elsewhere (the web panel, another PC, its disk):
    /// the open panel tab loads again, unless it holds edits not saved yet.
    fn panel_changed(&mut self, root: &std::path::Path, areas: &[String], cx: &mut Context<Self>) {
        if self.picked.as_deref() != Some(root) {
            return;
        }
        let any = |want: &[&str]| areas.iter().any(|a| a == "*" || want.contains(&a.as_str()));
        let reload = match self.tab {
            TAB_FILES => any(&["files", "mods", "configs", "worlds", "backups", "server", "players", "schedule"]),
            TAB_MODS => any(&["mods"]),
            TAB_PLAYERS => any(&["players"]),
            TAB_BACKUPS => any(&["backups"]),
            TAB_SCHEDULE => any(&["schedule"]) && !self.p_schedule.read(cx).unsaved(),
            TAB_SETTINGS => any(&["server"]) && !self.p_settings.read(cx).unsaved(cx),
            _ => false,
        };
        if reload {
            // Rendering shows the tab again, which loads it.
            self.p_shown = None;
            cx.notify();
        }
    }

    /// The header's History button for the picked server, made once per
    /// server. A Hopper Server's opens Mods on History; a server on this
    /// PC opens the same History dialog as an instance, on its folder
    /// (config saves and mod changes there keep their old copies).
    fn history_button(&self, cx: &mut Context<Self>) -> Option<Entity<crate::history_button::HistoryButton>> {
        let root = self.picked.clone()?;
        if self.store.read(cx).server(&root).is_some_and(|s| s.remote.is_none()) {
            let mut slot = self.history_btn.borrow_mut();
            if let Some(b) = slot.as_ref().filter(|b| b.read(cx).is_for(&root)) {
                return Some(b.clone());
            }
            let store = self.store.clone();
            let b = cx.new(|cx| crate::history_button::HistoryButton::new(store, root, cx));
            *slot = Some(b.clone());
            return Some(b);
        }
        let Some(Ok(t)) = self.target.clone() else { return None };
        let mut slot = self.history_btn.borrow_mut();
        if let Some(b) = slot.as_ref().filter(|b| b.read(cx).is_panel(&t)) {
            return Some(b.clone());
        }
        let view = cx.entity();
        let open: std::rc::Rc<dyn Fn(&mut Window, &mut App)> = std::rc::Rc::new(move |_, cx| {
            view.update(cx, |this, cx| {
                this.set_tab(TAB_MODS, cx);
                this.p_mods.update(cx, |m, cx| m.open_history(cx));
            })
        });
        let b = cx.new(|cx| crate::history_button::HistoryButton::for_panel(t, open, cx));
        *slot = Some(b.clone());
        Some(b)
    }

    /// The picked server on a Hopper Server shows a panel tab: tell the tab
    /// (once per server and tab, so it loads on opening).
    fn show_panel_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Ok(t)) = self.target.clone() else { return };
        let Some(root) = self.picked.clone() else { return };
        let key = (root, self.tab);
        if self.p_shown.as_ref() == Some(&key) {
            return;
        }
        self.p_shown = Some(key);
        match self.tab {
            TAB_FILES => self.p_files.update(cx, |v, cx| v.show(t, window, cx)),
            TAB_MODS => self.p_mods.update(cx, |v, cx| v.show(t, window, cx)),
            TAB_PLAYERS => self.p_players.update(cx, |v, cx| v.show(t, window, cx)),
            TAB_BACKUPS => self.p_backups.update(cx, |v, cx| v.show(t, window, cx)),
            TAB_SCHEDULE => self.p_schedule.update(cx, |v, cx| v.show(t, window, cx)),
            TAB_SETTINGS => self.p_settings.update(cx, |v, cx| v.show(t, window, cx)),
            _ => {}
        }
    }

    fn scan(&mut self, root: PathBuf, again: bool, cx: &mut Context<Self>) {
        if is_remote_root(&root) {
            return;
        }
        if !again && self.mods.contains_key(&root) {
            return;
        }
        self.mods.insert(root.clone(), None);
        cx.spawn(async move |this, cx| {
            let dir = root.join("mods");
            let r = cx
                .background_spawn(async move {
                    // Over SFTP / FTP: fetch what changed into Hopper's copy first.
                    hopper::remotefs::refresh_path(&dir).map_err(|e| format!("{e:#}"))?;
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
                hopper::fuzzy::words_match(&self.query, &[&m.mod_name, &m.file_name])
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

    fn detail(&self, s: &ServerInfo, art: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
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
        // The control polls this server every second or two; the store's
        // copy is slower, so the header's pill follows the control's.
        let fresh = self.control.read(cx).stats_of(&s.root).cloned();
        let store = self.store.read(cx);
        let status = store.status(&s.root);
        let pstats = fresh.or_else(|| store.panel_stats(&s.root).cloned());
        let paired = store.paired(s).cloned();
        let remote = s.remote.is_some();
        let also = remote && (store.also_in_folder(s) || !matches!(s.manager, Manager::Unknown));
        let manager = match s.manager {
            _ if store.is_hosted(&s.root) => "Hopper, on this PC",
            _ if remote => "Hopper Server",
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
        let hosted_meta = hopper::hosting::HostedMeta::load(&s.root);
        let (loader_label, loader_mc) = match (&s.loader, &hosted_meta) {
            (Some(l), _) => {
                let (label, mc) = hopper::server::loader_facts(l);
                (Some(label), mc)
            }
            (None, Some(m)) => (Some(hopper::server::loader_facts(&m.loader).0), Some(m.minecraft.clone())),
            (None, None) => (None, None),
        };
        // The pinged version is the truth while it runs.
        let minecraft = pstats
            .as_ref()
            .map(|p| p.version.trim().to_string())
            .filter(|v| v.starts_with("1."))
            .or(loader_mc);
        let mut info = info
            .item("Managed by", manager, 1)
            .item("Loader", loader_label.clone().unwrap_or("Unknown".into()), 1)
            .item("Java", s.java.clone().unwrap_or("Unknown".into()), 1)
            .item(
                "Max memory",
                s.max_memory.clone().unwrap_or("Unknown".into()),
                1,
            )
            .item("Port", s.port().to_string(), 1);
        // Set in Hopper Server (Settings › Address).
        let addresses = match &pstats {
            Some(p) if remote => p.addresses.clone(),
            _ => hopper::hosting::router::RouteSettings::load(&s.root).addresses,
        };
        if !addresses.is_empty() {
            info = info.item("Address", addresses.join(", "), 1);
        }
        let mut pack_version = None;
        if let Some(inst) = &paired {
            let ok = s.loader.as_deref() == Some(inst.loader.replace('-', " ").as_str());
            info = info.item(
                "Pack loader",
                format!(
                    "{}{}",
                    crate::store::loader_label(&inst.loader),
                    if ok { " (matches)" } else { " (differs)" }
                ),
                1,
            );
            if let Some(p) = inst.pack.as_ref().filter(|p| !p.version.trim().is_empty()) {
                info = info.item("Pack version", p.version.clone(), 1);
                pack_version = Some(p.version.clone());
            }
        }
        // server.properties, in words (the MOTD is in the header).
        for (key, label) in [
            ("max-players", "Max players"),
            ("level-name", "World name"),
            ("difficulty", "Difficulty"),
            ("gamemode", "Game mode"),
            ("view-distance", "View distance"),
            ("online-mode", "Online mode"),
            ("white-list", "Whitelist"),
        ] {
            let Some(v) = s.properties.get(key).map(|v| v.trim()).filter(|v| !v.is_empty()) else { continue };
            let v = match key {
                "view-distance" => format!("{v} chunks"),
                "online-mode" | "white-list" => if v == "true" { "On" } else { "Off" }.to_string(),
                "level-name" | "max-players" => hopper::instance_ops::strip_formatting(v),
                // "hard" → "Hard", as the game's menus say it.
                _ => {
                    let v = hopper::instance_ops::strip_formatting(v);
                    let mut c = v.chars();
                    c.next().map_or(String::new(), |f| f.to_uppercase().collect::<String>() + c.as_str())
                }
            };
            info = info.item(label, v, 1);
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
        let theme = cx.theme().clone();
        // The header: like an instance's, with the server's picture.
        let light = status_color(status, pstats.as_ref(), cx);
        let mut pills: Vec<AnyElement> = vec![
            widgets::pill(short_state(status, pstats.as_ref()), light, true).into_any_element(),
            widgets::pill(manager, theme.muted_foreground, false).into_any_element(),
        ];
        if also {
            pills.push(
                widgets::pill(
                    if matches!(s.manager, Manager::Mcss) { "Also in MCSS" } else { "Also in Crafty" },
                    theme.muted_foreground,
                    false,
                )
                .into_any_element(),
            );
        }
        let facts = crate::hero::facts_row(cx)
            .when_some(minecraft, |r, v| r.child(crate::hero::fact(IconName::Box, theme.success, format!("Minecraft {v}"))))
            .when_some(loader_label, |r, l| r.child(crate::hero::fact(IconName::Blocks, theme.warning, l)))
            .when_some(pack_version, |r, v| {
                let v = if v.starts_with(|c: char| c.is_ascii_digit()) { format!("v{v}") } else { v };
                r.child(crate::hero::fact(IconName::Tag, theme.info, v))
            })
            .when_some(addresses.first().cloned(), |r, a| r.child(crate::hero::soft_fact(IconName::Globe, a, cx)));
        let motd = s.properties.get("motd").and_then(|m| motd_line(m, cx)).map(|m| m.text_sm().into_any_element());
        // Figures: live ones while it runs, what it's set to while not.
        let live = pstats.as_ref().filter(|p| p.running && !p.starting);
        let mut stats: Vec<(String, String)> = Vec::new();
        match live {
            Some(p) => {
                stats.push((format!("{}/{}", p.online, p.max), "Players".into()));
                if let Some(t) = p.tps {
                    stats.push((t.text(), "TPS".into()));
                }
                if !p.mem.is_empty() {
                    stats.push((p.mem.clone(), "Memory".into()));
                }
            }
            None => {
                if let Some(m) = s.properties.get("max-players").filter(|m| !m.trim().is_empty()) {
                    stats.push((m.trim().to_string(), "Slots".into()));
                }
                if !remote && matches!(self.mods.get(&s.root), Some(Some(Ok(_)))) {
                    stats.push((n_jars.to_string(), "Mods".into()));
                }
                if let Some(m) = &s.max_memory {
                    stats.push((m.clone(), "Max memory".into()));
                }
            }
        }
        stats.truncate(crate::library::hero_room(self.detail_w));
        let blur = art.as_ref().and_then(|a| self.blurs.get(a).cloned().flatten());
        // A panel that says it is stopped won't answer a ping: no button
        // that can only say so.
        let pingable = !pstats.as_ref().is_some_and(|p| !p.running && !p.starting);
        let open = {
            // How this PC reaches the panel, as the panel sees it (the web
            // panel's status bar says the same).
            let via = pstats.as_ref().and_then(|p| p.via).filter(|_| remote);
            let tls = s.remote.as_ref().is_some_and(|r| r.base.starts_with("https:"));
            let b = Button::new("open-server")
                .outline()
                .icon(if remote { IconName::ExternalLink } else { IconName::FolderOpen })
                .label(match via {
                    Some(v) => format!("Open in browser · {v}"),
                    None if remote => "Open in browser".to_string(),
                    None => "Folder".to_string(),
                })
                .on_click(move |_, _, _| crate::store::open_path(&root));
            match via {
                Some(v) => b
                    .when(tls, |b| b.child(Icon::new(IconName::Lock).xsmall().text_color(theme.muted_foreground)))
                    .tooltip(format!(
                        "{} · {}",
                        match v {
                            "LAN" => "The panel is on your network",
                            "Tailscale" => "The panel is reached over Tailscale",
                            "Local" => "The panel is on this PC",
                            _ => "The panel is reached over the internet",
                        },
                        if tls { "encrypted" } else { "not encrypted (http)" }
                    )),
                None => b,
            }
        };
        let actions = h_flex()
            .flex_none()
            .flex_nowrap()
            .gap_2()
            .child(self.control.update(cx, |c, cx| c.controls_hero(cx)))
            .child(open)
            .when(pingable, |r| r.child(ping_button(&self.store, &server, cx)))
            .children(self.history_button(cx))
            .children(self.control.update(cx, |c, cx| c.more_button(cx)));
        // Past Overview the hero folds to one row: the tab's content gets the room.
        let render_hero = crate::hero::render; // one header on every tab, so it does not jump
        let header = render_hero(
            crate::hero::Hero {
                art,
                blur,
                no_art: letter_tile(&s.name, rems(2.25)).into_any_element(),
                pills,
                title: s.name.clone().into(),
                facts,
                summary: motd,
                stats,
                actions,
            },
            cx,
        );
        let body = v_flex()
            .flex_1()
            .min_h_0()
            .px_6()
            .pt_2()
            .pb_5()
            .map(|c| if self.tab == TAB_OVERVIEW { c.gap_5() } else { c.gap_3() })
            .child({
                let codes = tabs_for(remote);
                let danger = cx.theme().danger;
                let logs_dot = self.strip.read(cx).unseen_key().is_some_and(|k| !self.store.read(cx).notice_hidden(&k));
                let online = pstats.as_ref().filter(|p| p.running).map(|p| p.online);
                TabBar::new("server-tabs")
                    .underline()
                    .selected_index(codes.iter().position(|c| *c == self.tab).unwrap_or(0))
                    .children(codes.iter().map(|&code| {
                        let label = match code {
                            TAB_MODS if !remote => format!("Mods {n_jars}"),
                            TAB_PLAYERS => online.map_or("Players".to_string(), |n| format!("Players {n}")),
                            _ => tab_name(code).to_string(),
                        };
                        Tab::new()
                            .prefix(Icon::new(tab_icon(code)).small())
                            .label(label)
                            .when(code == TAB_LOGS && logs_dot && self.tab != TAB_LOGS, |t| t.suffix(widgets::tab_dot(danger)))
                    }))
                    .on_click(cx.listener(move |this, ix: &usize, _, cx| {
                        if let Some(code) = codes.get(*ix) {
                            this.set_tab(*code, cx)
                        }
                    }))
            })
            .when(self.tab != TAB_LOGS, |c| {
                self.strip.update(cx, |st, cx| st.show(s.root.clone(), cx));
                if self.strip.read(cx).crash().is_some() { c.child(self.strip.clone()) } else { c }
            })
            .children({
                let view = cx.entity().downgrade();
                crate::stuck::server_stuck_bar(&self.store, &s.root, pstats.as_ref(), &self.control, move |rel, _, cx| {
                    view.update(cx, |this, cx| this.see_crash(rel, cx)).ok();
                }, cx)
            })
            .when(remote && matches!(self.tab, TAB_FILES | TAB_MODS | TAB_PLAYERS | TAB_BACKUPS | TAB_SCHEDULE | TAB_SETTINGS), |c| {
                let body: AnyElement = match &self.target {
                    Some(Err(e)) => widgets::empty_state(IconName::TriangleAlert, "Can't reach this panel", Some(e.clone().into()), cx)
                        .into_any_element(),
                    _ => match self.tab {
                        TAB_FILES => self.p_files.clone().into_any_element(),
                        TAB_MODS => self.p_mods.clone().into_any_element(),
                        TAB_PLAYERS => self.p_players.clone().into_any_element(),
                        TAB_BACKUPS => self.p_backups.clone().into_any_element(),
                        TAB_SCHEDULE => self.p_schedule.clone().into_any_element(),
                        _ => self.p_settings.clone().into_any_element(),
                    },
                };
                c.child(div().flex_1().min_h_0().child(body))
            })
            .when(self.tab == TAB_OVERVIEW, |c| {
                let players = self.control.update(cx, |ctl, cx| ctl.players_view(cx));
                // Stopped, the cards would be all dashes: the header's
                // figures say what there is.
                let cards = pstats
                    .as_ref()
                    .is_some_and(|p| p.running || p.starting || p.crashed)
                    .then(|| self.control.update(cx, |ctl, cx| ctl.stat_cards(cx)))
                    .flatten();
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
            .when(self.tab == TAB_CONSOLE, |c| {
                c.child(self.control.update(cx, |ctl, cx| ctl.console_view(window, cx)))
            })
            .when(self.tab == TAB_CONFIGS, |c| {
                // Read from the folder here, or through the panel of the
                // Hopper Server the server runs on.
                let source = match (&s.remote, &self.target) {
                    (None, _) => Some(crate::config_view::Source::Local),
                    (Some(_), Some(Ok(t))) => Some(crate::config_view::Source::Panel(t.clone())),
                    _ => None,
                };
                let running = pstats.as_ref().is_some_and(|p| p.running || p.starting) || self.store.read(cx).status(&s.root).is_some_and(|st| st.ms().is_some());
                let body: AnyElement = match source {
                    Some(source) => {
                        let root = s.root.clone();
                        self.configs.update(cx, |v, cx| v.show_server(root, crate::config_view::ServerSide { source, running }, cx));
                        self.configs.clone().into_any_element()
                    }
                    None => match &self.target {
                        Some(Err(e)) => widgets::empty_state(IconName::TriangleAlert, "Can't reach this panel", Some(e.clone().into()), cx).into_any_element(),
                        _ => widgets::skeleton_rows(6, rems(2.5), cx).into_any_element(),
                    },
                };
                // The editor runs to the page's edges, as on an instance.
                c.child(div().flex_1().min_h_0().mx(rems(-1.5)).mb(rems(-1.25)).child(body))
            })
            .when(self.tab == TAB_LOGS, |c| {
                let root = s.root.clone();
                self.logs.update(cx, |v, cx| v.show(root, cx));
                c.child(div().flex_1().min_h_0().child(self.logs.clone()))
            })
            .when(self.tab == TAB_BACKUPS && !remote, |c| {
                c.child(
                    div()
                        .id("server-backups")
                        .flex_1()
                        .min_h_0()
                        .child(self.control.update(cx, |ctl, cx| ctl.backups_view(cx)))
                        .overflow_y_scrollbar(),
                )
            })
            .when(self.tab == TAB_SETTINGS && !remote, |c| {
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
            .when(self.tab == TAB_MODS && !remote, |c| c.child(
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
                                    .child(widgets::search_input(&self.filter).small().cleanable(true)),
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
            });
        v_flex().size_full().child(header).child(body).into_any_element()
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
        self.detail_w = room;
        let arts = self.arts(cx);
        if let Some(a) = self.picked.as_ref().and_then(|p| arts.get(p)).cloned() {
            self.want_blur(&a, cx);
        }
        let list = server_list(
            "servers-list",
            &self.store,
            self.picked.as_ref(),
            &arts,
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
        self.show_panel_tab(window, cx);
        let detail: AnyElement = match server {
            Some(s) => widgets::enter(
                ElementId::Name(format!("server-detail:{}", s.root.display()).into()),
                div().size_full().child(self.detail(&s, arts.get(&s.root).cloned(), window, cx)),
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
