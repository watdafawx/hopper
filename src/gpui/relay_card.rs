//! The "Relay through a VPS" card on a Hopper Server server's Overview: the
//! web card's answers in the web card's words (design/vps-relay.md, B8).
//! The Relay / Ping from here / Players through it / Last test row, the
//! upload tiles, the voice row, and "Test again" and Relay setup for a PC
//! that may manage the panel. Turning the relay on or off for the server
//! (Use the relay, Play at home) stays in Hopper Server. What the numbers and
//! words are comes from `hopper::relay_view`.

use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::relay_view::{Mark, RelayView};
use std::rc::Rc;

/// What the card's buttons do, and whether they show.
#[derive(Clone)]
pub struct CardActions {
    /// This PC may manage the panel (Test again, Relay setup).
    pub may: bool,
    /// A test is running.
    pub testing: bool,
    pub on_test: Rc<dyn Fn(&mut Window, &mut App)>,
    pub on_setup: Rc<dyn Fn(&mut Window, &mut App)>,
}

/// A round mark: the tick, the warning or the cross on its tint.
pub fn mark(m: Mark, cx: &App) -> Div {
    let theme = cx.theme();
    let (icon, color) = match m {
        Mark::Ok => (IconName::Check, theme.success),
        Mark::Warn => (IconName::TriangleAlert, theme.warning),
        Mark::Fail => (IconName::X, theme.danger),
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(16.))
        .rounded_full()
        .bg(color.opacity(widgets::alpha::SELECTED + 0.06))
        .text_color(crate::theme::chip_ink(color))
        .child(Icon::new(icon).size(px(11.)))
}

/// A mark for a step that's still running.
pub fn running_mark(cx: &App) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(16.))
        .text_color(cx.theme().primary)
        .child(Icon::new(IconName::LoaderCircle).size(px(12.)))
}

pub fn relay_card(r: &RelayView, a: &CardActions, now: u64, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let color = match r.state.as_str() {
        "up" => theme.success,
        "connecting" => theme.warning,
        "off" => theme.muted_foreground,
        _ => theme.danger,
    };
    let tile = |id: &'static str, label: &str, value: String, unit: &str, note: String| {
        widgets::stat_card(id, IconName::Globe, label, cx)
            .min_w(rems(11.))
            .child(
                h_flex()
                    .items_baseline()
                    .gap_1()
                    .child(div().text_2xl().font_weight(FontWeight::BOLD).child(value))
                    .when(!unit.is_empty(), |d| {
                        d.child(div().text_sm().text_color(theme.muted_foreground).child(unit.to_string()))
                    }),
            )
            .child(widgets::caption(note, cx).text_xs())
    };
    // One of the four health answers: a small label over its value.
    let cell = |label: &str, value: AnyElement| {
        v_flex()
            .flex_1()
            .min_w(rems(7.))
            .gap_0p5()
            .child(widgets::caption(label.to_string(), cx).text_xs())
            .child(value)
    };
    let value = |m: Option<AnyElement>, text: String| -> AnyElement {
        h_flex()
            .gap_1p5()
            .items_center()
            .text_sm()
            .font_weight(FontWeight::SEMIBOLD)
            .children(m)
            .child(div().min_w_0().truncate().child(text))
            .into_any_element()
    };
    let addr = r.address.clone();
    let live = r.up() && !r.at_home();
    let (now_rate, now_unit) = RelayView::rate(r.sent_bps);
    let saved = r.saved_percent();
    let (hit_v, hit_u) = {
        let s = hopper::instance_ops::human_bytes(r.hit_bytes);
        match s.rsplit_once(' ') {
            Some((v, u)) => (v.to_string(), u.to_string()),
            None => (s, String::new()),
        }
    };
    let health = (!r.at_home()).then(|| {
        let last: AnyElement = if a.testing {
            value(Some(running_mark(cx).into_any_element()), "Testing…".into())
        } else if let Some(t) = &r.test {
            value(Some(mark(if t.ok { Mark::Ok } else { Mark::Fail }, cx).into_any_element()), hopper::relay_view::ago(now, t.at))
        } else {
            value(None, "Not yet".into())
        };
        h_flex()
            .gap_3()
            .flex_wrap()
            .px_3()
            .py_2()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(cell("Relay", value(Some(mark(r.link_mark(), cx).into_any_element()), r.relay_text(now))))
            .child(cell("Ping from here", value(None, r.ping_text().unwrap_or_else(|| "—".into()))))
            .child(cell("Players through it", value(None, r.players_now().to_string())))
            .child(cell("Last test", last))
    });
    let failed = r.failed_rows();
    let test_button = (a.may && !r.at_home() && r.state != "off").then(|| {
        let f = a.on_test.clone();
        Button::new("relay-test")
            .small()
            .when(r.up(), |b| b.outline())
            .when(!r.up(), |b| b.primary())
            .label(if a.testing { "Testing…" } else { "Test again" })
            .loading(a.testing)
            .disabled(a.testing)
            .on_click(move |_, window, cx| f(window, cx))
    });
    let setup_button = |id: &'static str, label: &'static str| {
        let f = a.on_setup.clone();
        Button::new(id).small().outline().label(label).on_click(move |_, window, cx| f(window, cx))
    };
    v_flex()
        .id("relay-card")
        .gap_3()
        .px_4()
        .py_3()
        .rounded(theme.radius_lg)
        .bg(theme.foreground.opacity(0.03))
        .border_1()
        .border_color(theme.border)
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(Icon::new(IconName::Globe).small())
                .child(div().flex_none().whitespace_nowrap().font_weight(FontWeight::SEMIBOLD).child("Relay through a VPS"))
                .child(widgets::pill(r.pill(), color, true))
                .child(div().flex_1())
                .when_some(r.retry_text(), |d, t| d.child(widgets::caption(t, cx).text_xs().min_w_0().truncate()))
                .when(a.may, |d| {
                    let f = a.on_setup.clone();
                    d.child(
                        Button::new("relay-setup")
                            .ghost()
                            .small()
                            .icon(IconName::Settings2)
                            .tooltip("Relay setup: check, repair or remove it")
                            .on_click(move |_, window, cx| f(window, cx)),
                    )
                }),
        )
        .when(r.show_address(), |c| {
            c.child(widgets::caption("Players join", cx).text_xs()).child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().font_family("Consolas").text_sm().child(addr.clone()))
                    .child(
                        Button::new("relay-copy")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Copy)
                            .tooltip("Copy the address")
                            .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(addr.clone()))),
                    ),
            )
        })
        .when(r.no_address(), |c| {
            c.child(note(
                "This server shares the relay with other servers and has no address of its own, so players can't pick it. Give it one in its Settings › Network & address.",
                true,
                None,
                cx,
            ))
        })
        .when(r.stale_address(), |c| {
            c.child(note(
                format!("Its address {} isn't on the relay yet. Run the relay setup again so players reach it there.", r.addresses_first),
                true,
                a.may.then(|| setup_button("relay-repair", "Repair").into_any_element()),
                cx,
            ))
        })
        .when(r.at_home(), |c| c.child(note(r.home_note(), false, None, cx)))
        .when(r.state != "off", |c| c.children(health))
        .when(!a.testing && !failed.is_empty(), |c| {
            c.child(
                v_flex().gap_1p5().children(failed.iter().map(|row| {
                    h_flex()
                        .gap_2()
                        .items_start()
                        .text_xs()
                        .child(mark(Mark::from_state(&row.state), cx))
                        .child(div().flex_1().min_w_0().child(row.text.clone()))
                })),
            )
        })
        .when_some(r.problem(), |c, p| c.child(note(p, r.state == "down", None, cx)))
        .children(test_button.map(|b| h_flex().child(b)))
        .when(live, |c| {
            c.child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .items_stretch()
                    .child(tile(
                        "relay-saved",
                        "Upload saved",
                        saved.map_or("—".into(), |s| s.to_string()),
                        if saved.is_some() { "%" } else { "" },
                        format!(
                            "{} sent · {} vanilla",
                            hopper::instance_ops::human_bytes(r.sent),
                            hopper::instance_ops::human_bytes(r.vanilla)
                        ),
                    ))
                    .child(tile("relay-now", "Upload now", now_rate, now_unit, {
                        let (v, u) = RelayView::rate(r.vanilla_bps);
                        format!("vanilla {v} {u}")
                    }))
                    .child(tile(
                        "relay-hits",
                        "Cache hits",
                        hit_v,
                        &hit_u,
                        r.hit_percent().map_or("nothing sent yet".to_string(), |p| format!("{p}% of what was sent")),
                    )),
            )
        })
        .when(live && r.voice.is_some(), |c| c.child(voice_row(r, cx)))
        .child(widgets::caption("Turn the relay on or off for this server in Hopper Server.", cx).text_xs())
        .into_any_element()
}

/// A note in the card: tinted red when something is wrong. `button` sits at
/// its right.
fn note(text: impl Into<SharedString>, bad: bool, button: Option<AnyElement>, cx: &App) -> Div {
    let theme = cx.theme();
    h_flex()
        .gap_2()
        .items_center()
        .px_3()
        .py_2()
        .rounded(theme.radius)
        .text_xs()
        .bg(if bad { theme.danger.opacity(widgets::alpha::TINT) } else { theme.foreground.opacity(widgets::alpha::HOVER) })
        .child(div().flex_1().min_w_0().child(text.into()))
        .children(button.map(|b| div().flex_none().child(b)))
}

/// Voice chat through the relay: a pill, one line, and a plain note when
/// UDP doesn't get through the VPS's firewall.
fn voice_row(r: &RelayView, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let color = match r.voice.as_ref().map(|v| v.state.as_str()) {
        Some("on") => theme.success,
        Some("tcp") => theme.warning,
        _ => theme.muted_foreground,
    };
    v_flex()
        .gap_2()
        .pt_3()
        .border_t_1()
        .border_color(theme.border)
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(widgets::pill(r.voice_pill().unwrap_or("Voice"), color, false))
                .child(div().min_w_0().truncate().text_sm().child(r.voice_text().unwrap_or_default()))
                .child(div().flex_1())
                .children(r.voice_right().map(|t| div().flex_none().whitespace_nowrap().child(widgets::caption(t, cx).text_xs()))),
        )
        .children(r.voice_note().map(|t| note(t, false, None, cx)))
        .into_any_element()
}
