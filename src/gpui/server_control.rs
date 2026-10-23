//! Running a rig server from Hopper through its panel (Crafty, MCSS or
//! Pterodactyl):
//! live state and stats, start / stop / restart / kill, and the console.

use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::panel::{BackupSet, PanelAction, PanelServer, PanelStats, ServerPanel};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// How often the stats (and the console, while it shows) refresh.
const POLL: Duration = Duration::from_secs(2);

/// Samples kept for the sparklines (a minute and a half at [`POLL`]).
const HISTORY: usize = 45;

/// What the control asks its page to do.
pub enum ControlEvent {
    /// Open Settings to set up the panel.
    SetUp,
}

impl EventEmitter<ControlEvent> for ServerControl {}

pub struct ServerControl {
    store: Entity<Store>,
    root: Option<PathBuf>,
    /// The panel, and the settings it was made from.
    panel: Option<(String, Arc<dyn ServerPanel>)>,
    /// The panel's servers, once listed.
    listed: Option<Result<Vec<PanelServer>, String>>,
    /// This server in the panel.
    id: Option<String>,
    stats: Option<Result<PanelStats, String>>,
    /// Recent (CPU %, memory %) samples while running, oldest first, for
    /// the Overview's sparklines.
    history: std::collections::VecDeque<(f32, f32)>,
    console: Vec<String>,
    /// Whether the console shows (it only polls then).
    console_open: bool,
    /// An action on its way.
    busy: Option<PanelAction>,
    /// The server's backup setups and files, once asked for.
    backups: Option<Result<Vec<BackupSet>, String>>,
    /// Backups a backup mod in the server made, once looked for.
    mod_backups: Option<Vec<hopper::backupmods::ModBackups>>,
    command: Entity<InputState>,
    /// The console's lines on screen, with text picking.
    lines: crate::line_select::LineSelect,
    /// Console lines that scrolled off the panel's window so far, so a
    /// line keeps its key (and a pick stays put) as new ones arrive.
    console_base: usize,
    _poll: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ServerControl {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let command = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Console command, e.g. say Restarting soon")
        });
        let subs = vec![
            cx.subscribe_in(&command, window, |this, _, e: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = e {
                    this.send(window, cx);
                }
            }),
            // Settings may have set the panel up (or changed it).
            cx.observe(&store, |this, _, cx| {
                if this.panel_key(cx) != this.panel.as_ref().map(|(k, _)| k.clone()) {
                    this.panel = None;
                    this.listed = None;
                    this.reload(cx);
                }
            }),
        ];
        Self {
            store,
            root: None,
            panel: None,
            listed: None,
            id: None,
            stats: None,
            history: Default::default(),
            console: Vec::new(),
            console_open: false,
            busy: None,
            backups: None,
            mod_backups: None,
            command,
            lines: crate::line_select::LineSelect::new(cx),
            console_base: 0,
            _poll: None,
            _subscriptions: subs,
        }
    }

    /// The settings a panel is made from; None when there is none to make.
    /// Whether the shown server is one Hopper hosts (its own panel).
    fn hosted(&self, cx: &App) -> bool {
        self.root.as_ref().is_some_and(|r| self.store.read(cx).is_hosted(r))
    }

    fn panel_key(&self, cx: &App) -> Option<String> {
        if self.hosted(cx) {
            return Some("local".into());
        }
        let s = self.store.read(cx).settings();
        let token = hopper::panel::token()?;
        let url = std::env::var("HOPPER_PANEL_URL").unwrap_or_else(|_| s.panel_url.clone());
        (!url.trim().is_empty()).then(|| {
            format!("{}|{}|{}|{}", s.panel_kind, url.trim(), s.panel_self_signed, token.len())
        })
    }

    fn the_panel(&mut self, cx: &App) -> Option<Arc<dyn ServerPanel>> {
        if self.panel.is_none() {
            let key = self.panel_key(cx)?;
            let p: Arc<dyn ServerPanel> = if self.hosted(cx) {
                self.store.read(cx).local_panel()
            } else {
                hopper::panel::from_settings(self.store.read(cx).settings())?.into()
            };
            self.panel = Some((key, p));
        }
        self.panel.as_ref().map(|(_, p)| p.clone())
    }

    /// The panel's short name ("Crafty").
    pub fn panel_name(&self, cx: &App) -> &'static str {
        if self.hosted(cx) {
            return "Hopper";
        }
        hopper::panel::short_name(&self.store.read(cx).settings().panel_kind)
    }

    pub fn configured(&self, cx: &App) -> bool {
        self.panel_key(cx).is_some()
    }

    /// Shows the server in `root`.
    pub fn show(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.root.as_ref() == Some(&root) {
            return;
        }
        // Hosted and rig servers have different panels.
        self.panel = None;
        self.listed = None;
        self.root = Some(root);
        self.reload(cx);
    }

    pub fn set_console_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.console_open != open {
            self.console_open = open;
            if open {
                self.poll_now(cx);
            }
        }
    }

    /// Finds the server in the panel, then polls it.
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.id = None;
        self.stats = None;
        self.history.clear();
        self.backups = None;
        self.console.clear();
        self.console_base = 0;
        self.lines.clear();
        self._poll = None;
        let Some(panel) = self.the_panel(cx) else {
            cx.notify();
            return;
        };
        let need_list = !matches!(self.listed, Some(Ok(_)));
        let p = panel.clone();
        self._poll = Some(cx.spawn(async move |this, cx| {
            if need_list {
                let r = cx
                    .background_spawn(async move { p.servers().map_err(|e| format!("{e:#}")) })
                    .await;
                let ok = this
                    .update(cx, |this, cx| {
                        this.listed = Some(r);
                        cx.notify();
                    })
                    .is_ok();
                if !ok {
                    return;
                }
            }
            let found = this
                .update(cx, |this, cx| {
                    let server = this
                        .root
                        .as_ref()
                        .and_then(|r| this.store.read(cx).server(r))
                        .cloned();
                    this.id = match (&this.listed, server) {
                        (Some(Ok(list)), Some(server)) => {
                            hopper::panel::match_server(list, &server).map(|s| s.id.clone())
                        }
                        _ => None,
                    };
                    cx.notify();
                    this.id.is_some()
                })
                .unwrap_or(false);
            if !found {
                return;
            }
            loop {
                let Ok(Some((id, console))) =
                    this.update(cx, |this, _| this.id.clone().map(|id| (id, this.console_open)))
                else {
                    return;
                };
                let p = panel.clone();
                let (stats, lines) = cx
                    .background_spawn(async move {
                        let stats = p.stats(&id).map_err(|e| format!("{e:#}"));
                        let lines = console.then(|| p.console(&id).ok()).flatten();
                        (stats, lines)
                    })
                    .await;
                if this
                    .update(cx, |this, cx| this.got(stats, lines, cx))
                    .is_err()
                {
                    return;
                }
                // Faster while the console shows, so it reads as live.
                let wait = if console { Duration::from_secs(1) } else { POLL };
                cx.background_executor().timer(wait).await;
            }
        }));
        cx.notify();
    }

    fn got(
        &mut self,
        stats: Result<PanelStats, String>,
        lines: Option<Vec<String>>,
        cx: &mut Context<Self>,
    ) {
        // An action is done once the state moves past it.
        if let (Some(a), Ok(s)) = (self.busy, &stats) {
            let settled = match a {
                PanelAction::Start => s.running && !s.starting,
                PanelAction::Stop | PanelAction::Kill => !s.running,
                PanelAction::Restart | PanelAction::Backup => true,
            };
            if settled {
                self.busy = None;
            }
        }
        if let Some(lines) = lines
            && lines != self.console
        {
            // Follows new lines while at the bottom and nothing is picked.
            let follow = !self.lines.is_active() && self.lines.scroll.is_scrolled_to_end().unwrap_or(true);
            self.console_base += crate::line_select::shift(&self.console, &lines);
            self.console = lines;
            if follow {
                self.lines.scroll_to_end(self.console.len());
            }
        }
        match &stats {
            Ok(s) if s.running && !s.starting => {
                if self.history.len() >= HISTORY {
                    self.history.pop_front();
                }
                self.history.push_back((s.cpu, s.mem_percent));
            }
            Ok(_) => self.history.clear(),
            Err(_) => {}
        }
        self.stats = Some(stats);
        cx.notify();
    }

    /// One poll now, between the regular ones.
    fn poll_now(&mut self, cx: &mut Context<Self>) {
        let (Some(panel), Some(id)) = (self.the_panel(cx), self.id.clone()) else {
            return;
        };
        let console = self.console_open;
        cx.spawn(async move |this, cx| {
            let (stats, lines) = cx
                .background_spawn(async move {
                    (
                        panel.stats(&id).map_err(|e| format!("{e:#}")),
                        console.then(|| panel.console(&id).ok()).flatten(),
                    )
                })
                .await;
            this.update(cx, |this, cx| this.got(stats, lines, cx)).ok();
        })
        .detach();
    }

    fn server_name(&self, cx: &App) -> String {
        self.root
            .as_ref()
            .and_then(|r| self.store.read(cx).server(r))
            .map(|s| s.name.clone())
            .unwrap_or_default()
    }

    /// Asks first where it costs something: a kill always, a stop or
    /// restart while players are on.
    fn request(&mut self, action: PanelAction, window: &mut Window, cx: &mut Context<Self>) {
        let online = match &self.stats {
            Some(Ok(s)) => s.online,
            _ => 0,
        };
        let name = self.server_name(cx);
        let ask = match action {
            PanelAction::Kill => Some(format!(
                "Kill {name}? The process ends at once: the world isn't saved, so anything since the last autosave is lost."
            )),
            PanelAction::Stop | PanelAction::Restart if online > 0 => Some(format!(
                "{} {name}? {online} player{} online.",
                action.label(),
                if online == 1 { " is" } else { "s are" }
            )),
            _ => None,
        };
        let Some(text) = ask else {
            self.run(action, window, cx);
            return;
        };
        let view = cx.entity();
        let danger = action == PanelAction::Kill;
        window.open_alert_dialog(cx, move |alert, _, _| {
            let view = view.clone();
            alert
                .title(format!("{} the server?", action.label()))
                .description(text.clone())
                .button_props(
                    gpui_kit::component::dialog::DialogButtonProps::default()
                        .ok_text(action.label())
                        .ok_variant(if danger {
                            gpui_kit::component::button::ButtonVariant::Danger
                        } else {
                            gpui_kit::component::button::ButtonVariant::Primary
                        })
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    view.update(cx, |this, cx| this.run(action, window, cx));
                    true
                })
        });
    }

    fn run(&mut self, action: PanelAction, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(panel), Some(id)) = (self.the_panel(cx), self.id.clone()) else {
            return;
        };
        self.busy = Some(action);
        cx.notify();
        let name = self.server_name(cx);
        let pname = self.panel_name(cx);
        cx.spawn_in(window, async move |this, cx| {
            let r = cx
                .background_spawn(async move { panel.act(&id, action) })
                .await;
            this.update_in(cx, |this, window, cx| {
                match r {
                    Ok(()) => {
                        if action == PanelAction::Backup {
                            window.push_notification(
                                Notification::success(format!("Backing up {name} in {pname}.")),
                                cx,
                            );
                        }
                    }
                    Err(e) => {
                        this.busy = None;
                        window.push_notification(Notification::error(format!("{e:#}")), cx);
                    }
                }
                this.poll_now(cx);
            })
            .ok();
        })
        .detach();
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let line = self.command.read(cx).value().trim().to_string();
        let (Some(panel), Some(id)) = (self.the_panel(cx), self.id.clone()) else {
            return;
        };
        if line.is_empty() {
            return;
        }
        self.command
            .update(cx, |s, cx| s.set_value(String::new(), window, cx));
        cx.spawn_in(window, async move |this, cx| {
            let r = cx
                .background_spawn(async move { panel.command(&id, &line) })
                .await;
            this.update_in(cx, |this, window, cx| {
                if let Err(e) = r {
                    window.push_notification(Notification::error(format!("{e:#}")), cx);
                }
                this.poll_now(cx);
            })
            .ok();
        })
        .detach();
    }

    /// The state and the buttons, for the server's header.
    pub fn controls(&self, cx: &mut Context<Self>) -> AnyElement {
        self.controls_in(false, cx)
    }

    /// The same, narrow: Stop and Restart as icons with tooltips, for a
    /// side column.
    pub fn controls_compact(&self, cx: &mut Context<Self>) -> AnyElement {
        self.controls_in(true, cx)
    }

    fn controls_in(&self, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let pname = self.panel_name(cx);
        if !self.configured(cx) {
            return Button::new("panel-set-up")
                .small()
                .outline()
                .icon(IconName::Settings)
                .label("Control from Hopper…")
                .tooltip("Start, stop and see the console from Hopper")
                .on_click(cx.listener(|_, _, _, cx| cx.emit(ControlEvent::SetUp)))
                .into_any_element();
        }
        let note = |t: String| widgets::caption(t, &*cx).text_xs().into_any_element();
        match (&self.listed, &self.id) {
            (None, _) => return note(format!("Asking {pname}…")),
            (Some(Err(e)), _) => {
                return div()
                    .id("panel-error")
                    .max_w(rems(28.))
                    .text_xs()
                    .text_color(theme.danger)
                    .truncate()
                    .child(e.clone())
                    .into_any_element();
            }
            (Some(Ok(_)), None) => {
                return note(format!("{pname} doesn't list this server (folder, port or name)"));
            }
            _ => {}
        }
        let stats = match &self.stats {
            Some(Ok(s)) => Some(s.clone()),
            _ => None,
        };
        let busy = self.busy;
        let running = stats.as_ref().is_some_and(|s| s.running);
        let (tag, state) = match (&stats, busy) {
            (_, Some(a)) => (
                Tag::warning(),
                match a {
                    PanelAction::Start => "Starting",
                    PanelAction::Stop => "Stopping",
                    PanelAction::Restart => "Restarting",
                    PanelAction::Kill => "Killing",
                    PanelAction::Backup => "Backing up",
                },
            ),
            (Some(s), None) if s.crashed => (Tag::danger(), "Crashed"),
            (Some(s), None) if s.starting => (Tag::warning(), "Starting"),
            (Some(s), None) if s.running => (Tag::success(), "Running"),
            (Some(_), None) => (Tag::secondary(), "Stopped"),
            (None, None) => (Tag::secondary(), "…"),
        };
        let menu_view = cx.entity();
        h_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_1p5()
                    .when(busy.is_some(), |r| r.child(Spinner::new().xsmall()))
                    .child(tag.small().child(state)),
            )
            .when(!compact, |r| r.child(div().w_px().h_5().bg(theme.border)))
            .when(compact, |r| r.child(div().flex_1()))
            .map(|r| {
                if running {
                    r.child(
                        Button::new("panel-stop")
                            .small()
                            .outline()
                            .icon(IconName::Square)
                            .map(|b| if compact { b.tooltip("Stop") } else { b.label("Stop") })
                            .disabled(busy.is_some())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.request(PanelAction::Stop, window, cx)
                            })),
                    )
                    .child(
                        Button::new("panel-restart")
                            .small()
                            .outline()
                            .icon(IconName::RotateCcw)
                            .map(|b| if compact { b.tooltip("Restart") } else { b.label("Restart") })
                            .disabled(busy.is_some())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.request(PanelAction::Restart, window, cx)
                            })),
                    )
                } else {
                    r.child(
                        Button::new("panel-start")
                            .small()
                            .success()
                            .icon(IconName::Play)
                            .label("Start")
                            .disabled(busy.is_some())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.request(PanelAction::Start, window, cx)
                            })),
                    )
                }
            })
            .child(
                Button::new("panel-more")
                    .small()
                    .ghost()
                    .icon(IconName::Ellipsis)
                    .dropdown_menu(move |menu, _, _| {
                        let (v1, v2) = (menu_view.clone(), menu_view.clone());
                        menu.item(PopupMenuItem::new("Back up now").on_click(
                            move |_, window, cx| {
                                v1.update(cx, |this, cx| {
                                    this.request(PanelAction::Backup, window, cx)
                                })
                            },
                        ))
                        .separator()
                        .item(
                            PopupMenuItem::new("Kill (no save)")
                                .disabled(!running)
                                .on_click(move |_, window, cx| {
                                    v2.update(cx, |this, cx| {
                                        this.request(PanelAction::Kill, window, cx)
                                    })
                                }),
                        )
                    }),
            )
            .into_any_element()
    }

    /// Up or starting, by the panel's last poll; None before one came back.
    pub fn running(&self) -> Option<bool> {
        match &self.stats {
            Some(Ok(s)) if self.id.is_some() => Some(s.running || s.starting),
            _ => None,
        }
    }

    /// Live numbers for the Overview: (label, value).
    pub fn live(&self) -> Vec<(&'static str, String)> {
        let Some(Ok(s)) = &self.stats else {
            return Vec::new();
        };
        // The state itself shows as Status.
        let mut rows = Vec::new();
        if s.running {
            rows.push(("CPU", format!("{:.0}%", s.cpu)));
            rows.push((
                "Memory",
                if s.mem.is_empty() {
                    format!("{:.0}%", s.mem_percent)
                } else {
                    format!("{} ({:.0}%)", s.mem, s.mem_percent)
                },
            ));
            // Named players get their own section below.
            if s.players.is_empty() {
                rows.push(("Players", format!("{}/{}", s.online, s.max)));
            }
            if !s.started.is_empty() {
                rows.push(("Started", s.started.clone()));
            }
        }
        if !s.world_size.is_empty() {
            rows.push(("World size", s.world_size.clone()));
        }
        rows
    }

    /// The Overview's cards: CPU and memory with sparklines of the last
    /// minute or so, players and world size.
    pub fn stat_cards(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Some(Ok(s)) = &self.stats else { return None };
        let theme = cx.theme();
        let live = s.running && !s.starting;
        let card = |id: &'static str, icon: IconName, label: &str| {
            v_flex()
                .id(id)
                .flex_1()
                .min_w_0()
                .gap_1()
                .px_4()
                .py_3()
                .rounded(theme.radius_lg)
                .bg(theme.foreground.opacity(0.03))
                .border_1()
                .border_color(theme.border)
                .hover(|st| {
                    st.border_color(theme.muted_foreground.opacity(0.45)).shadow(vec![
                        BoxShadow::new(px(0.), px(8.), black().opacity(0.25)).blur_radius(px(24.)),
                    ])
                })
                .child(
                    h_flex()
                        .gap_1p5()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.muted_foreground)
                        .child(gpui_kit::component::Icon::new(icon).xsmall())
                        .child(label.to_uppercase()),
                )
        };
        let value = |v: String, unit: String| {
            h_flex()
                .items_baseline()
                .gap_1()
                .child(div().text_2xl().font_weight(FontWeight::BOLD).child(v))
                .when(!unit.is_empty(), |r| {
                    r.child(div().text_sm().text_color(theme.muted_foreground).child(unit))
                })
        };
        let cpu: Vec<f32> = self.history.iter().map(|h| h.0).collect();
        let mem: Vec<f32> = self.history.iter().map(|h| h.1).collect();
        let cpu_top = cpu.iter().cloned().fold(100.0, f32::max);
        let dash = || "—".to_string();
        let mem_value = if !live {
            (dash(), String::new())
        } else if s.mem.is_empty() {
            (format!("{:.0}", s.mem_percent), "%".to_string())
        } else {
            // "6.1GB" → ("6.1", "GB")
            let split = s.mem.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.mem.len());
            (s.mem[..split].trim().to_string(), s.mem[split..].trim().to_string())
        };
        let players: Vec<String> = s.players.iter().take(5).cloned().collect();
        // A sparkline once there are two samples; a note while stopped.
        let (app, state): (&App, &str) = (cx, s.state());
        let trend = |v: Vec<f32>, top: f32, color: Hsla| -> AnyElement {
            if live && v.len() >= 2 {
                widgets::sparkline(v, top, color).into_any_element()
            } else {
                widgets::caption(state, app).text_xs().into_any_element()
            }
        };
        Some(
            h_flex()
                .gap_3()
                .items_stretch()
                .child(
                    card("card-cpu", IconName::Cpu, "CPU")
                        .child(value(
                            if live { format!("{:.0}", s.cpu) } else { dash() },
                            if live { "%".into() } else { String::new() },
                        ))
                        .child(trend(cpu, cpu_top, theme.success)),
                )
                .child(
                    card("card-mem", IconName::MemoryStick, "Memory")
                        .child(value(mem_value.0, mem_value.1))
                        .child(trend(mem, 100.0, theme.primary)),
                )
                .child(
                    card("card-players", IconName::Users, "Players")
                        .child(value(
                            if live { s.online.to_string() } else { dash() },
                            if live { format!("/ {}", s.max) } else { String::new() },
                        ))
                        .child(
                            h_flex()
                                .gap_1()
                                .pt_1()
                                .flex_wrap()
                                .children(players.into_iter().map(|p| {
                                    div()
                                        .px_1p5()
                                        .rounded(theme.radius)
                                        .bg(theme.foreground.opacity(0.06))
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(p)
                                })),
                        ),
                )
                .child(
                    card("card-world", IconName::HardDrive, "World")
                        .child(value(
                            if s.world_size.is_empty() { dash() } else { s.world_size.clone() },
                            String::new(),
                        ))
                        .when(live && !s.started.is_empty(), |c| {
                            c.child(widgets::caption(format!("Up since {}", s.started), cx).text_xs())
                        }),
                )
                .into_any_element(),
        )
    }

    /// Online players, each with a menu of moderator commands.
    pub fn players_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Some(Ok(s)) = &self.stats else { return None };
        if !s.running || s.players.is_empty() {
            return None;
        }
        let theme = cx.theme().clone();
        let chips = s.players.iter().enumerate().map(|(i, p)| {
            let (view, name) = (cx.entity(), p.clone());
            Button::new(("player", i))
                .small()
                .outline()
                .label(p.clone())
                .dropdown_caret(true)
                .dropdown_menu(move |menu, _, _| {
                    let item = |label: &str, cmd: String| {
                        let view = view.clone();
                        PopupMenuItem::new(label.to_string()).on_click(move |_, window, cx| {
                            let cmd = cmd.clone();
                            view.update(cx, |this, cx| this.run_command(cmd, window, cx))
                        })
                    };
                    menu.item(item("Kick", format!("kick {name}")))
                        .item(item("Make operator", format!("op {name}")))
                        .item(item("Remove operator", format!("deop {name}")))
                        .separator()
                        .item(item("Add to whitelist", format!("whitelist add {name}")))
                        .item(item("Remove from whitelist", format!("whitelist remove {name}")))
                })
        });
        Some(
            v_flex()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child(format!("Players online ({}/{})", s.online, s.max)),
                )
                .child(h_flex().flex_wrap().gap_2().children(chips))
                .into_any_element(),
        )
    }

    /// Lists the backups (again).
    pub fn load_backups(&mut self, cx: &mut Context<Self>) {
        self.load_mod_backups(cx);
        if self.hosted(cx) {
            self.backups = Some(Ok(Vec::new()));
            return;
        }
        let (Some(panel), Some(id)) = (self.the_panel(cx), self.id.clone()) else {
            return;
        };
        self.backups = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move { panel.backups(&id).map_err(|e| format!("{e:#}")) })
                .await;
            this.update(cx, |this, cx| {
                this.backups = Some(r);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Looks for a backup mod's backups in the server's folder.
    fn load_mod_backups(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        self.mod_backups = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_spawn(async move { hopper::backupmods::detect(&root) })
                .await;
            this.update(cx, |this, cx| {
                this.mod_backups = Some(found);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The world folder of the shown server (server.properties' level-name).
    fn world_dir(&self) -> Option<PathBuf> {
        let root = self.root.as_ref()?;
        let level = hopper::hosting::Properties::load(&root.join("server.properties"))
            .ok()
            .and_then(|p| p.get("level-name").map(str::to_string))
            .filter(|l| !l.trim().is_empty())
            .unwrap_or_else(|| "world".into());
        Some(root.join(level))
    }

    /// Asks, then restores a backup mod's backup over the (stopped) hosted
    /// server's world.
    fn ask_restore_mod(&mut self, file: hopper::backupmods::BackupFile, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(root), Some(world)) = (self.root.clone(), self.world_dir()) else {
            return;
        };
        let name = self.server_name(cx);
        let view = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (view, root, world, file) = (view.clone(), root.clone(), world.clone(), file.clone());
            alert
                .title(format!("Restore {name}'s world?"))
                .description(format!(
                    "The world is replaced with {}. The current world is kept in .hopper-backups/before-restore.",
                    file.name
                ))
                .button_props(
                    gpui_kit::component::dialog::DialogButtonProps::default()
                        .ok_text("Restore")
                        .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    let (root, world, file) = (root.clone(), world.clone(), file.clone());
                    view.update(cx, |_, cx| {
                        cx.spawn_in(window, async move |this, cx| {
                            let shown = file.name.clone();
                            let r = cx
                                .background_spawn(async move {
                                    hopper::backupmods::restore(&root, &world, &file)
                                })
                                .await;
                            this.update_in(cx, |this, window, cx| {
                                let n = match r {
                                    Ok(_) => Notification::success(format!("Restored the world from {shown}.")),
                                    Err(e) => Notification::error(format!("{e:#}")),
                                };
                                window.push_notification(n, cx);
                                this.load_mod_backups(cx);
                            })
                            .ok();
                        })
                        .detach();
                    });
                    true
                })
        });
    }

    /// One backup mod's backups, with what can be done with them.
    fn mod_backups_section(
        &self,
        mi: usize,
        m: &hopper::backupmods::ModBackups,
        hosted: bool,
        running: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let dir = m.dir.clone();
        let command = m.command.clone();
        let files: Vec<_> = m.files.iter().take(50).enumerate().map(|(fi, f)| {
            let file = f.clone();
            let id = 500_000 + mi * 1_000 + fi;
            let why_not = if running {
                Some("Stop the server first")
            } else if f.partial() {
                Some("Only the files changed since an earlier backup: restore it with the mod")
            } else if !f.restorable() {
                Some("Hopper restores zips; restore this one with the mod")
            } else {
                None
            };
            h_flex()
                .id(("mod-backup", id))
                .gap_2()
                .px_3()
                .py_1p5()
                .rounded(theme.radius)
                .hover(|s| s.bg(theme.list_hover))
                .child(div().flex_1().min_w_0().text_sm().truncate().child(f.name.clone()))
                .child(
                    widgets::caption(
                        format!(
                            "{} · {}",
                            hopper::hosting::local_panel::format_bytes(f.size),
                            hopper::time::relative_secs(f.modified)
                        ),
                        cx,
                    )
                    .text_xs()
                    .flex_none(),
                )
                .when(hosted, |row| {
                    row.child(
                        Button::new(("mod-restore", id))
                            .xsmall()
                            .outline()
                            .label("Restore")
                            .disabled(why_not.is_some())
                            .tooltip(why_not.unwrap_or("Replace the world with this backup"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.ask_restore_mod(file.clone(), window, cx)
                            })),
                    )
                })
        }).collect();
        let total: u64 = m.files.iter().map(|f| f.size).sum();
        let mod_name = m.mod_name.clone();
        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .gap_3()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(m.mod_name.clone()))
                    .child(
                        widgets::caption(
                            format!(
                                "{} backups · {}",
                                m.files.len(),
                                hopper::hosting::local_panel::format_bytes(total)
                            ),
                            cx,
                        )
                        .text_xs(),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new(("mod-backup-folder", mi))
                            .small()
                            .ghost()
                            .icon(IconName::FolderOpen)
                            .tooltip(format!("Open {}", dir.display()))
                            .on_click(move |_, _, _| crate::store::open_path(&dir)),
                    )
                    .when_some(command, |row, line| {
                        row.child(
                            Button::new(("mod-backup-now", mi))
                                .small()
                                .outline()
                                .icon(IconName::Archive)
                                .label("Back up now")
                                .disabled(!running)
                                .tooltip(if running {
                                    format!("Runs /{line}")
                                } else {
                                    format!("{mod_name} backs up while the server runs")
                                })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.run_command(line.clone(), window, cx);
                                    // The mod writes its zip over the next moments.
                                    cx.spawn(async move |this, cx| {
                                        cx.background_executor().timer(Duration::from_secs(15)).await;
                                        this.update(cx, |this, cx| this.load_mod_backups(cx)).ok();
                                    })
                                    .detach();
                                })),
                        )
                    }),
            )
            .when(m.files.is_empty(), |c| {
                c.child(widgets::caption("No backups yet.", cx).px_3())
            })
            .children(files)
            .into_any_element()
    }

    fn backup_now(&mut self, set: String, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(panel), Some(id)) = (self.the_panel(cx), self.id.clone()) else {
            return;
        };
        let pname = self.panel_name(cx);
        cx.spawn_in(window, async move |this, cx| {
            let r = cx
                .background_spawn(async move { panel.backup_now(&id, &set) })
                .await;
            this.update_in(cx, |this, window, cx| {
                let n = match r {
                    Ok(()) => Notification::success(format!("{pname} is backing it up.")),
                    Err(e) => Notification::error(format!("{e:#}")),
                };
                window.push_notification(n, cx);
                this.load_backups(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Asks, then restores `file` over the (stopped) server.
    fn ask_restore(&mut self, set: String, file: String, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.server_name(cx);
        let view = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (view, set, file) = (view.clone(), set.clone(), file.clone());
            alert
                .title(format!("Restore {name}?"))
                .description(format!(
                    "The server's files are replaced with {file}. Anything since that backup is lost."
                ))
                .button_props(
                    gpui_kit::component::dialog::DialogButtonProps::default()
                        .ok_text("Restore")
                        .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    let (set, file) = (set.clone(), file.clone());
                    view.update(cx, |this, cx| this.restore(set, file, window, cx));
                    true
                })
        });
    }

    fn restore(&mut self, set: String, file: String, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(panel), Some(id)) = (self.the_panel(cx), self.id.clone()) else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let shown = file.clone();
            let r = cx
                .background_spawn(async move { panel.restore(&id, &set, &file) })
                .await;
            this.update_in(cx, |this, window, cx| {
                let n = match r {
                    Ok(()) => Notification::success(format!("Restoring {shown}.")),
                    Err(e) => Notification::error(format!("{e:#}")),
                };
                window.push_notification(n, cx);
                this.poll_now(cx);
            })
            .ok();
        })
        .detach();
    }

    /// The Backups tab.
    pub fn backups_view(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let hosted = self.hosted(cx);
        let running = matches!(&self.stats, Some(Ok(s)) if s.running);
        if self.mod_backups.is_none() && self.root.is_some() {
            self.load_mod_backups(cx);
        }
        // What a backup mod in the server keeps, listed first: it's what most
        // packs back up with.
        let mods = self.mod_backups.clone();
        let mod_body: Option<AnyElement> = match &mods {
            None => Some(
                h_flex()
                    .gap_2()
                    .child(Spinner::new().small())
                    .child(widgets::caption("Looking for a backup mod", cx))
                    .into_any_element(),
            ),
            Some(found) if found.is_empty() => None,
            Some(found) => Some(
                v_flex()
                    .gap_4()
                    .children(
                        found
                            .iter()
                            .enumerate()
                            .map(|(mi, m)| self.mod_backups_section(mi, m, hosted, running, cx))
                            .collect::<Vec<_>>(),
                    )
                    .into_any_element(),
            ),
        };
        let no_mod = matches!(&mods, Some(f) if f.is_empty());
        if hosted || !self.configured(cx) || self.id.is_none() {
            let hint = if hosted {
                "No backup mod in this server. Most packs have one (SimpleBackups, FTB Backups, Advanced Backups…); add one in Content and its backups show here."
            } else {
                "No backup mod in this server. Panel backups show here once one is set up in Settings → Servers."
            };
            return v_flex()
                .gap_3()
                .children(mod_body)
                .when(no_mod, |c| c.child(widgets::caption(hint, cx)))
                .into_any_element();
        }
        if self.backups.is_none() {
            self.load_backups(cx);
        }
        let body: AnyElement = match &self.backups {
            None => h_flex()
                .gap_2()
                .child(Spinner::new().small())
                .child(widgets::caption("Reading backups", cx))
                .into_any_element(),
            // A panel that doesn't list backups is fine when a mod does.
            Some(Err(_)) if !no_mod && mods.is_some() => div().into_any_element(),
            Some(Err(e)) => div().text_sm().text_color(theme.danger).child(e.clone()).into_any_element(),
            Some(Ok(sets)) if sets.is_empty() => {
                widgets::caption("No backups are set up for this server in the panel.", cx).into_any_element()
            }
            Some(Ok(sets)) => v_flex()
                .gap_4()
                .children(sets.iter().enumerate().map(|(si, set)| {
                    let set_id = set.id.clone();
                    let files = set.files.iter().enumerate().map(|(fi, f)| {
                        let (set_id, file) = (set.id.clone(), f.clone());
                        h_flex()
                            .id(("backup-file", si * 10_000 + fi))
                            .gap_2()
                            .px_3()
                            .py_1p5()
                            .rounded(theme.radius)
                            .hover(|s| s.bg(theme.list_hover))
                            .child(div().flex_1().min_w_0().text_sm().truncate().child(f.clone()))
                            .child(
                                Button::new(("restore", si * 10_000 + fi))
                                    .xsmall()
                                    .outline()
                                    .label("Restore")
                                    .disabled(running)
                                    .tooltip(if running {
                                        "Stop the server first"
                                    } else {
                                        "Replace the server's files with this backup"
                                    })
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.ask_restore(set_id.clone(), file.clone(), window, cx)
                                    })),
                            )
                    });
                    v_flex()
                        .gap_1()
                        .child(
                            h_flex()
                                .gap_3()
                                .child(div().font_weight(FontWeight::SEMIBOLD).child(set.name.clone()))
                                .child(widgets::caption(format!("{} files", set.files.len()), cx).text_xs())
                                .child(div().flex_1())
                                .child(
                                    Button::new(("backup-now", si))
                                        .small()
                                        .outline()
                                        .icon(IconName::Archive)
                                        .label("Back up now")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.backup_now(set_id.clone(), window, cx)
                                        })),
                                ),
                        )
                        .children(files)
                }))
                .into_any_element(),
        };
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(widgets::caption(
                        if running {
                            "Restoring needs the server stopped."
                        } else {
                            "Restoring replaces the server's files with the backup."
                        },
                        cx,
                    ))
                    .child(div().flex_1())
                    .child(
                        Button::new("backups-refresh")
                            .ghost()
                            .small()
                            .icon(IconName::RefreshCw)
                            .tooltip("List the backups again")
                            .on_click(cx.listener(|this, _, _, cx| this.load_backups(cx))),
                    ),
            )
            .children(mod_body)
            .child(body)
            .into_any_element()
    }

    /// Sends one console command and refreshes.
    fn run_command(&mut self, line: String, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(panel), Some(id)) = (self.the_panel(cx), self.id.clone()) else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let shown = line.clone();
            let r = cx
                .background_spawn(async move { panel.command(&id, &line) })
                .await;
            this.update_in(cx, |this, window, cx| {
                let n = match r {
                    Ok(()) => Notification::success(format!("Sent: {shown}")),
                    Err(e) => Notification::error(format!("{e:#}")),
                };
                window.push_notification(n, cx);
                this.poll_now(cx);
            })
            .ok();
        })
        .detach();
    }

    /// The Console tab.
    pub fn console_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if !self.configured(cx) || self.id.is_none() {
            return v_flex()
                .gap_3()
                .items_start()
                .child(widgets::caption(
                    "The console comes from the server panel. Set one up in Settings → Servers.",
                    cx,
                ))
                .child(
                    Button::new("console-set-up")
                        .small()
                        .outline()
                        .label("Open Settings")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(ControlEvent::SetUp))),
                )
                .into_any_element();
        }
        let running = matches!(&self.stats, Some(Ok(s)) if s.running);
        let empty = widgets::caption("No console output yet.", cx).into_any_element();
        v_flex()
            .flex_1()
            .min_h_0()
            .gap_2()
            .child(crate::line_select::view(self, "console", Some(empty), window, cx))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div().flex_1().child(
                            Input::new(&self.command)
                                .small()
                                .disabled(!running)
                                .prefix(widgets::caption("/", cx)),
                        ),
                    )
                    .child(
                        Button::new("console-send")
                            .small()
                            .primary()
                            .label("Send")
                            .disabled(!running)
                            .on_click(cx.listener(|this, _, window, cx| this.send(window, cx))),
                    ),
            )
            .when(!running, |c| {
                c.child(widgets::caption("Start the server to send commands.", cx).text_xs())
            })
            .into_any_element()
    }
}

impl crate::line_select::Lines for ServerControl {
    fn line_select(&self) -> &crate::line_select::LineSelect {
        &self.lines
    }

    fn line_select_mut(&mut self) -> &mut crate::line_select::LineSelect {
        &mut self.lines
    }

    fn row_count(&self) -> usize {
        self.console.len()
    }

    fn row_key(&self, row: usize) -> Option<usize> {
        (row < self.console.len()).then_some(self.console_base + row)
    }

    fn key_text(&self, key: usize) -> Option<String> {
        let line = self.console.get(key.checked_sub(self.console_base)?)?;
        Some(line.replace('\t', "    "))
    }

    fn row_color(&self, row: usize, cx: &App) -> Hsla {
        let theme = cx.theme();
        let l = self.console.get(row).map(String::as_str).unwrap_or("");
        if l.contains("ERROR") || l.contains("FATAL") {
            theme.danger
        } else if l.contains("WARN") {
            theme.warning
        } else {
            theme.foreground
        }
    }
}

impl Render for ServerControl {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // Its parts render inside the Servers page.
        div()
    }
}
