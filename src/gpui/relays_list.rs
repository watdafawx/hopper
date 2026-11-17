//! Settings › Relays: the relays of every paired Hopper Server this PC may
//! manage, and under each the servers that go through it (the web's
//! Settings › Relays, design/vps-relay.md B8). A server is set up for a
//! relay from its own page (More › Relay through a VPS), where its Overview
//! card has the details. Same words as the web page; see `relay_view::relays`.

use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::hopperd::Client;
use hopper::relay_view::{RelayRow, RelayServerRow, relays, unix_now};
use serde_json::json;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// One paired Hopper Server this PC may manage, and its relays.
struct PanelRelays {
    panel: String,
    name: String,
    relays: Vec<RelayRow>,
}

pub enum RelaysEvent {
    /// A server's name was clicked: show it on the Servers page.
    OpenServer(PathBuf),
}

pub struct RelaysList {
    store: Entity<Store>,
    panels: Vec<PanelRelays>,
    /// The first answer (or failure) is in.
    loaded: bool,
    /// Several paired panels can't be reached or may not be managed from here.
    err: Option<String>,
    /// Last drawn: the list only asks again while it is on screen.
    seen: Instant,
    polling: bool,
    /// "Take off" was clicked once: (panel id, relay address, server id).
    ask: Option<(String, String, String)>,
    busy: bool,
}

impl EventEmitter<RelaysEvent> for RelaysList {}

impl RelaysList {
    pub fn new(store: Entity<Store>, cx: &mut Context<Self>) -> Self {
        let mut this = Self { store, panels: Vec::new(), loaded: false, err: None, seen: Instant::now(), polling: false, ask: None, busy: false };
        this.load(cx);
        this
    }

    /// Asks every paired panel that is on, owner-level ones only (the
    /// routes are `require_owner`), in the background.
    fn load(&mut self, cx: &mut Context<Self>) {
        let panels: Vec<_> = self.store.read(cx).settings().hopper_panels.iter().filter(|p| !p.off).cloned().collect();
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move {
                    let mut out = Vec::new();
                    let mut first_err = None;
                    for p in panels {
                        let Some(c) = Client::for_panel(&p) else { continue };
                        match c.get("/auth/me") {
                            Ok(me) if hopper::hopperd::may_manage(&me) => {}
                            Ok(_) => continue,
                            Err(e) => {
                                first_err.get_or_insert(format!("{}: {e:#}", p.name));
                                continue;
                            }
                        }
                        match c.get("/vps-relays") {
                            Ok(v) => out.push(PanelRelays { panel: p.id.clone(), name: p.name.clone(), relays: relays(&v, unix_now()) }),
                            Err(e) => {
                                first_err.get_or_insert(format!("{}: {e:#}", p.name));
                            }
                        }
                    }
                    (out, first_err)
                })
                .await;
            this.update(cx, |this, cx| {
                let (list, err) = r;
                // A failed ask keeps what was shown.
                if err.is_none() || !list.is_empty() || !this.loaded {
                    this.panels = list;
                }
                this.err = err;
                this.loaded = true;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Asks again every few seconds while the list is on screen.
    fn follow(&mut self, cx: &mut Context<Self>) {
        if self.polling {
            return;
        }
        self.polling = true;
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(5)).await;
            let alive = this
                .update(cx, |this, cx| {
                    if this.seen.elapsed() < Duration::from_secs(7) && !this.busy {
                        this.load(cx);
                    }
                })
                .is_ok();
            if !alive {
                return;
            }
        })
        .detach();
    }

    fn take_off(&mut self, panel: String, address: String, server: String, cx: &mut Context<Self>) {
        let Some(p) = self.panels.iter().find(|p| p.panel == panel) else { return };
        let Some(r) = p.relays.iter().find(|r| r.address == address) else { return };
        let keep: Vec<String> = r.raw_servers.iter().filter(|id| **id != server).cloned().collect();
        let Some(hp) = self.store.read(cx).settings().hopper_panels.iter().find(|x| x.id == panel).cloned() else { return };
        self.ask = None;
        self.busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_spawn(async move {
                    let c = Client::for_panel(&hp).ok_or_else(|| anyhow::anyhow!("Can't reach {}", hp.name))?;
                    c.put("/vps-relay", json!({ "servers": keep }))
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match res {
                    Ok(_) => {
                        this.err = None;
                        this.load(cx);
                    }
                    Err(e) => this.err = Some(format!("{e:#}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn open_server(&self, panel: &str, id: &str, cx: &mut Context<Self>) {
        let root = self
            .store
            .read(cx)
            .servers()
            .iter()
            .find(|s| s.remote.as_ref().is_some_and(|r| r.panel == panel && r.id == id))
            .map(|s| s.root.clone());
        if let Some(root) = root {
            cx.emit(RelaysEvent::OpenServer(root));
        }
    }

    fn relay_view(&self, panel: &PanelRelays, r: &RelayRow, rix: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let color = match r.state.as_str() {
            "up" => theme.success,
            "connecting" => theme.warning,
            "off" => theme.muted_foreground,
            _ => theme.danger,
        };
        let hover = theme.list_hover;
        let count = if r.servers.len() == 1 { "1 server".to_string() } else { format!("{} servers", r.servers.len()) };
        v_flex()
            .id(("relay", rix))
            .py_2()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .px_2()
                    .pb_2()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).truncate().child(r.address.clone()))
                            .child(div().text_xs().text_color(if r.bad { theme.danger } else { theme.muted_foreground }).child(r.line.clone())),
                    )
                    .child(widgets::pill(r.chip, color, true)),
            )
            .child(div().px_2().pb_1().child(widgets::caption(count, cx).text_xs()))
            .children(r.servers.iter().enumerate().map(|(i, s)| {
                self.server_row(panel, r, s, (rix, i), hover, cx)
            }))
            .into_any_element()
    }

    fn server_row(&self, panel: &PanelRelays, r: &RelayRow, s: &RelayServerRow, ix: (usize, usize), hover: Hsla, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let asking = self.ask.as_ref().is_some_and(|(p, a, id)| *p == panel.panel && *a == r.address && *id == s.id);
        let (p, id) = (panel.panel.clone(), s.id.clone());
        let (p2, a2, id2) = (panel.panel.clone(), r.address.clone(), s.id.clone());
        let (p3, a3, id3) = (p2.clone(), a2.clone(), id2.clone());
        v_flex()
            .id(("relay-server", ix.0 * 1000 + ix.1))
            .gap_1()
            .px_2()
            .py_2()
            .border_t_1()
            .border_color(theme.border)
            .hover(move |d| d.bg(hover))
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                div()
                                    .id(("relay-server-name", ix.0 * 1000 + ix.1))
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .truncate()
                                    .cursor_pointer()
                                    .hover(|d| d.text_color(cx.theme().primary))
                                    .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new("Open the server").build(window, cx))
                                    .on_click(cx.listener(move |this, _, _, cx| this.open_server(&p, &id, cx)))
                                    .child(s.name.clone()),
                            )
                            .child(div().text_xs().text_color(if s.bad { theme.danger } else { theme.muted_foreground }).child(s.line.clone())),
                    )
                    .child(widgets::chip(theme.muted_foreground).child(s.chip))
                    .child(div().min_w(rems(4.)).text_right().whitespace_nowrap().child(widgets::caption(s.players.clone(), cx).text_xs()))
                    .child(div().w(rems(5.)).flex().justify_end().when(s.can_take_off && !asking, |d| {
                        d.child(
                            Button::new(("relay-take-off", ix.0 * 1000 + ix.1))
                                .small()
                                .outline()
                                .label("Take off")
                                .disabled(self.busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.ask = Some((p2.clone(), a2.clone(), id2.clone()));
                                    cx.notify();
                                })),
                        )
                    })),
            )
            .when(asking, |d| {
                d.child(
                    v_flex()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .mt_1()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border)
                        .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(format!("Take {} off the relay?", s.name)))
                        .child(div().text_sm().text_color(theme.muted_foreground).child(format!(
                            "Players can't join {} through {} any more; they join with its own address. The other servers on the relay keep going as they are. You can set it up again from its page (More › Relay through a VPS).",
                            s.name, r.address
                        )))
                        .child(
                            h_flex()
                                .gap_2()
                                .justify_end()
                                .child(Button::new(("relay-ask-no", ix.0 * 1000 + ix.1)).outline().small().label("Cancel").on_click(cx.listener(|this, _, _, cx| {
                                    this.ask = None;
                                    cx.notify();
                                })))
                                .child(
                                    Button::new(("relay-ask-yes", ix.0 * 1000 + ix.1))
                                        .primary()
                                        .small()
                                        .label("Take it off")
                                        .disabled(self.busy)
                                        .on_click(cx.listener(move |this, _, _, cx| this.take_off(p3.clone(), a3.clone(), id3.clone(), cx))),
                                ),
                        ),
                )
            })
            .into_any_element()
    }
}

impl Render for RelaysList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.seen = Instant::now();
        self.follow(cx);
        let theme = cx.theme().clone();
        let many = self.panels.len() > 1;
        let any = self.panels.iter().any(|p| !p.relays.is_empty());
        let body: AnyElement = if !self.loaded {
            widgets::skeleton_rows(2, rems(2.5), cx).into_any_element()
        } else if self.panels.is_empty() {
            // Nothing to manage from here: the web shows nothing to a PC that may not.
            match &self.err {
                Some(e) => widgets::caption(format!("Can't read the relays: {e}"), cx).text_xs().into_any_element(),
                None => widgets::caption("Pair a Hopper Server this PC manages to see its relays here.", cx).text_xs().into_any_element(),
            }
        } else if !any {
            widgets::empty_state(
                IconName::Globe,
                "No relay yet",
                Some("A relay is a VPS your players join through. Set one up from a server's page: More › Relay through a VPS.".into()),
                cx,
            )
            .into_any_element()
        } else {
            let mut ix = 0;
            v_flex()
                .gap_3()
                .children(self.panels.iter().filter(|p| !p.relays.is_empty()).map(|p| {
                    v_flex()
                        .gap_2()
                        .when(many, |d| d.child(widgets::caption(p.name.clone(), cx).text_xs()))
                        .children(p.relays.iter().map(|r| {
                            ix += 1;
                            self.relay_view(p, r, ix, cx)
                        }))
                }))
                .into_any_element()
        };
        v_flex()
            .gap_2()
            .child(body)
            .when(self.loaded && !self.panels.is_empty() && self.err.is_some(), |d| {
                d.child(div().text_xs().text_color(theme.danger).child(self.err.clone().unwrap_or_default()))
            })
    }
}
