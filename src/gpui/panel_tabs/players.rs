//! A Hopper Server server's Players tab: who is online, the operators, the
//! whitelist and the bans, with the actions the panel's Players tab has.

use super::{get, q, call, load_state, Load, Target};
use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::*;
use serde_json::{json, Value};
use std::time::Duration;

/// One name in a list.
#[derive(Clone, Default)]
struct Person {
    name: String,
    level: Option<u64>,
    reason: String,
}

#[derive(Default)]
struct Data {
    running: bool,
    whitelist_on: bool,
    online: Vec<String>,
    ops: Vec<Person>,
    whitelist: Vec<Person>,
    banned: Vec<Person>,
}

fn people(v: &Value) -> Vec<Person> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|p| {
                    Some(Person {
                        name: p["name"].as_str()?.to_string(),
                        level: p["level"].as_u64(),
                        reason: p["reason"].as_str().unwrap_or("").to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse(v: &Value) -> Data {
    Data {
        running: v["running"].as_bool().unwrap_or(false),
        whitelist_on: v["whitelist_on"].as_bool().unwrap_or(false),
        online: v["online"]
            .as_array()
            .map(|a| a.iter().filter_map(|n| n.as_str().map(String::from)).collect())
            .unwrap_or_default(),
        ops: people(&v["ops"]),
        whitelist: people(&v["whitelist"]),
        banned: people(&v["banned"]),
    }
}

/// A player's head, as the panel's web UI builds it.
fn head_url(name: &str) -> String {
    format!("https://mc-heads.net/avatar/{}/44", q(name))
}

pub struct PlayersTab {
    store: Entity<Store>,
    target: Option<Target>,
    data: Load<Data>,
    name: Entity<InputState>,
    error: Option<String>,
    /// Bumped whenever the shown server changes; stops the older refresh loop.
    generation: u64,
    _subs: Vec<Subscription>,
}

impl PlayersTab {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Minecraft name"));
        let subs = vec![
            // Heads arriving.
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.subscribe_in(&name, window, |this, _, e: &InputEvent, window, cx| {
                if matches!(e, InputEvent::PressEnter { .. }) {
                    this.add("whitelist_add", window, cx);
                }
            }),
        ];
        Self { store, target: None, data: Load::Waiting, name, error: None, generation: 0, _subs: subs }
    }

    /// Shows `target`'s Players (again when it's the same server: a refresh).
    pub fn show(&mut self, target: Target, _window: &mut Window, cx: &mut Context<Self>) {
        if self.target.as_ref() != Some(&target) {
            self.data = Load::Waiting;
            self.error = None;
        }
        self.target = Some(target);
        self.generation += 1;
        let generation = self.generation;
        self.reload(cx);
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(10)).await;
            let again = this
                .update(cx, |t, cx| {
                    if t.generation != generation {
                        return false;
                    }
                    t.reload(cx);
                    true
                })
                .unwrap_or(false);
            if !again {
                break;
            }
        })
        .detach();
        cx.notify();
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let generation = self.generation;
        get(cx, &t, t.sp("/players"), move |this, r, cx| {
            if this.generation != generation {
                return;
            }
            match r {
                Ok(v) => {
                    let d = parse(&v);
                    let urls: Vec<String> = d
                        .online
                        .iter()
                        .chain(d.ops.iter().map(|p| &p.name))
                        .chain(d.whitelist.iter().map(|p| &p.name))
                        .chain(d.banned.iter().map(|p| &p.name))
                        .map(|n| head_url(n))
                        .collect();
                    this.store.update(cx, |s, _| {
                        for u in &urls {
                            s.want_image(u);
                        }
                    });
                    this.data = Load::Ready(d);
                }
                // Keep what is on screen when a refresh fails.
                Err(e) if this.data.ready().is_some() => this.error = Some(e),
                Err(e) => this.data = Load::Failed(e),
            }
        });
    }

    /// Runs a player action, then refreshes the lists shortly after (the
    /// server saves its files itself).
    fn act(&mut self, action: &str, who: &str, reason: &str, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let path = t.sp("/players");
        let body = json!({ "action": action, "name": who, "reason": reason });
        self.error = None;
        call(cx, &t, move |c| c.post(&path, body), |this, r, cx| {
            if let Err(e) = r {
                this.error = Some(e);
            }
            let generation = this.generation;
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(700)).await;
                this.update(cx, |t, cx| {
                    if t.generation == generation {
                        t.reload(cx);
                    }
                })
                .ok();
            })
            .detach();
        });
    }

    /// Acts on the name typed in the box, then empties it.
    fn add(&mut self, action: &str, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.name.read(cx).value().trim().to_string();
        if n.is_empty() {
            return;
        }
        self.name.update(cx, |s, cx| s.set_value("", window, cx));
        self.act(action, &n, "", cx);
    }

    /// Asks before a kick or a ban, with an optional reason for the player.
    fn confirm(&mut self, action: &'static str, who: String, window: &mut Window, cx: &mut Context<Self>) {
        let reason = cx.new(|cx| InputState::new(window, cx).placeholder("Reason (optional)"));
        let view = cx.entity();
        let (verb, tail) = if action == "kick" { ("Kick", "to leave now") } else { ("Ban", "from joining again") };
        window.open_dialog(cx, move |dialog, window, _| {
            let (view, reason2, who2) = (view.clone(), reason.clone(), who.clone());
            let mut ok = Button::new("pl-ok").label(verb);
            ok = if action == "ban" { ok.danger() } else { ok.primary() };
            dialog
                .title(format!("{verb} {who}"))
                .w(rems(26.).to_pixels(window.rem_size()))
                .margin_top(rems(widgets::DIALOG_TOP).to_pixels(window.rem_size()))
                .child(
                    v_flex()
                        .gap_2()
                        .child(div().text_sm().child(format!("This asks {who} {tail}. The reason is shown to them.")))
                        .child(Input::new(&reason)),
                )
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().child(Button::new("pl-cancel").outline().label("Cancel")))
                        .child(DialogAction::new().child(ok)),
                )
                .on_ok(move |_, _, cx| {
                    let r = reason2.read(cx).value().trim().to_string();
                    let who = who2.clone();
                    view.update(cx, |t, cx| t.act(action, &who, &r, cx));
                    true
                })
        });
    }
}

impl Render for PlayersTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = window;
        let theme = cx.theme().clone();
        let root = div().id("players-tab").size_full().flex().flex_col();
        if let Some(s) = load_state(&self.data, cx) {
            return root.p_4().child(s).into_any_element();
        }
        let Some(d) = self.data.ready() else { return root.into_any_element() };
        let store = self.store.read(cx);

        let face = |name: &str| -> AnyElement {
            let frame = div().size(rems(1.5)).flex_none().rounded_sm().overflow_hidden().bg(theme.muted);
            match store.image(&head_url(name)) {
                Some(p) => frame.child(img(p.to_path_buf()).size_full()).into_any_element(),
                None => frame.into_any_element(),
            }
        };
        let row = |id: String, name: &str| -> Stateful<Div> {
            div()
                .id(SharedString::from(id))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded(theme.radius)
                .hover(|s| s.bg(theme.list_hover))
                .child(face(name))
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(name.to_string()))
        };
        let section = |icon: IconName, title: &str, note: Option<AnyElement>, body: Vec<AnyElement>, empty: &str| {
            v_flex()
                .gap_1()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .pb_1()
                        .child(Icon::new(icon).small().text_color(theme.muted_foreground))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(title.to_string()))
                        .children(note),
                )
                .children(if body.is_empty() { vec![widgets::caption(empty.to_string(), cx).px_2().into_any_element()] } else { body })
        };
        let spacer = || div().flex_1();
        let op_names: Vec<String> = d.ops.iter().map(|p| p.name.to_lowercase()).collect();

        // Online now.
        let online: Vec<AnyElement> = d
            .online
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let is_op = op_names.contains(&p.to_lowercase());
                let (a, b, c) = (p.clone(), p.clone(), p.clone());
                row(format!("on-{i}"), p)
                    .children(is_op.then(|| widgets::chip(theme.primary).child("op")))
                    .child(spacer())
                    .child(
                        Button::new(SharedString::from(format!("on-op-{i}")))
                            .ghost()
                            .small()
                            .icon(IconName::Crown)
                            .label(if is_op { "Deop" } else { "Op" })
                            .on_click(cx.listener(move |t, _, _, cx| t.act(if is_op { "deop" } else { "op" }, &a, "", cx))),
                    )
                    .child(
                        Button::new(SharedString::from(format!("on-kick-{i}")))
                            .ghost()
                            .small()
                            .icon(IconName::LogOut)
                            .label("Kick")
                            .on_click(cx.listener(move |t, _, w, cx| t.confirm("kick", b.clone(), w, cx))),
                    )
                    .child(
                        Button::new(SharedString::from(format!("on-ban-{i}")))
                            .ghost()
                            .small()
                            .icon(IconName::Ban)
                            .label("Ban")
                            .text_color(theme.danger)
                            .on_click(cx.listener(move |t, _, w, cx| t.confirm("ban", c.clone(), w, cx))),
                    )
                    .into_any_element()
            })
            .collect();
        let online_empty = if d.running { "Nobody online." } else { "The server is stopped." };

        // Operators.
        let ops: Vec<AnyElement> = d
            .ops
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let n = p.name.clone();
                row(format!("op-{i}"), &p.name)
                    .child(widgets::caption(format!("level {}", p.level.unwrap_or(4)), cx))
                    .child(spacer())
                    .child(
                        Button::new(SharedString::from(format!("op-rm-{i}")))
                            .ghost()
                            .small()
                            .label("Remove op")
                            .on_click(cx.listener(move |t, _, _, cx| t.act("deop", &n, "", cx))),
                    )
                    .into_any_element()
            })
            .collect();

        // Whitelist.
        let whitelist: Vec<AnyElement> = d
            .whitelist
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let n = p.name.clone();
                row(format!("wl-{i}"), &p.name)
                    .child(spacer())
                    .child(
                        Button::new(SharedString::from(format!("wl-rm-{i}")))
                            .ghost()
                            .small()
                            .label("Remove")
                            .on_click(cx.listener(move |t, _, _, cx| t.act("whitelist_remove", &n, "", cx))),
                    )
                    .into_any_element()
            })
            .collect();
        let wl_note = widgets::chip(if d.whitelist_on { theme.success } else { theme.muted_foreground })
            .child(if d.whitelist_on { "on" } else { "off" })
            .into_any_element();
        let wl_empty = if d.whitelist_on {
            "Nobody yet."
        } else {
            "Nobody yet. Turn it on in Settings (white-list)."
        };

        // Banned.
        let banned: Vec<AnyElement> = d
            .banned
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let n = p.name.clone();
                row(format!("ban-{i}"), &p.name)
                    .child(div().min_w_0().truncate().child(widgets::caption(p.reason.clone(), cx)))
                    .child(spacer())
                    .child(
                        Button::new(SharedString::from(format!("ban-rm-{i}")))
                            .ghost()
                            .small()
                            .icon(IconName::Undo2)
                            .label("Unban")
                            .on_click(cx.listener(move |t, _, _, cx| t.act("pardon", &n, "", cx))),
                    )
                    .into_any_element()
            })
            .collect();

        let typed = !self.name.read(cx).value().trim().is_empty();
        let online_note = widgets::caption(format!("{}", d.online.len()), cx).into_any_element();
        let running = d.running;
        let add_row = v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().w(rems(16.)).child(Input::new(&self.name)))
                    .child(
                        Button::new("pl-wl-add")
                            .primary()
                            .small()
                            .icon(IconName::UserPlus)
                            .label("Add to whitelist")
                            .disabled(!typed)
                            .on_click(cx.listener(|t, _, w, cx| t.add("whitelist_add", w, cx))),
                    )
                    .child(
                        Button::new("pl-op-add")
                            .outline()
                            .small()
                            .icon(IconName::Crown)
                            .label("Op")
                            .disabled(!typed)
                            .on_click(cx.listener(|t, _, w, cx| t.add("op", w, cx))),
                    )
                    .child(
                        Button::new("pl-ban-add")
                            .outline()
                            .small()
                            .icon(IconName::Ban)
                            .label("Ban")
                            .disabled(!typed)
                            .on_click(cx.listener(|t, _, w, cx| {
                                let n = t.name.read(cx).value().trim().to_string();
                                if !n.is_empty() {
                                    t.confirm("ban", n, w, cx);
                                }
                            })),
                    ),
            )
            .children((!running).then(|| {
                widgets::caption("Start the server to change players: it looks names up and saves its lists itself.", cx)
            }));

        let err = self.error.clone();
        root.child(
            v_flex()
                .id("players-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .p_4()
                .gap_5()
                .children(err.map(|e| div().text_sm().text_color(theme.danger).child(e)))
                .child(section(IconName::Users, "Online now", Some(online_note), online, online_empty))
                .child(section(IconName::Crown, "Operators", None, ops, "None."))
                .child(
                    v_flex()
                        .gap_3()
                        .child(section(IconName::ShieldCheck, "Whitelist", Some(wl_note), whitelist, wl_empty))
                        .child(add_row),
                )
                .child(section(IconName::Ban, "Banned", None, banned, "Nobody.")),
        )
        .into_any_element()
    }
}
