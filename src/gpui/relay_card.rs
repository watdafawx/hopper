//! The read-only "Relay through a VPS" card on a Hopper Server server's
//! Overview (option A of mockups/desktop-relay-card.html). The web card in
//! Hopper Server's UI is the same card with its buttons; here only the
//! answers show, the setup stays in Hopper Server. What the numbers and
//! words are comes from `hopper::relay_view`.

use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::relay_view::RelayView;

pub fn relay_card(r: &RelayView, cx: &App) -> AnyElement {
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
    let addr = r.address.clone();
    let live = r.up() && !r.at_home();
    let (now, now_unit) = RelayView::rate(r.sent_bps);
    let saved = r.saved_percent();
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
                .child(gpui_kit::component::Icon::new(IconName::Globe).small())
                .child(div().font_weight(FontWeight::SEMIBOLD).child("Relay through a VPS"))
                .child(widgets::pill(r.pill(), color, true))
                .child(div().flex_1())
                .when_some(r.ping_text(), |d, p| d.child(widgets::caption(format!("Ping from here {p}"), cx).text_xs())),
        )
        .when(r.at_home(), |c| {
            c.child(div().text_sm().child(
                "Playing at home while the relay is down. Players join with this server's own address, and the server checks their accounts.",
            ))
        })
        .when(!r.at_home() && !addr.is_empty(), |c| {
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
        .when_some(r.problem(), |c, p| c.child(div().text_sm().text_color(theme.danger).child(p)))
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
                        format!("{} sent", hopper::instance_ops::human_bytes(r.sent)),
                    ))
                    .child(tile("relay-now", "Upload now", now, now_unit, {
                        let (v, u) = RelayView::rate(r.vanilla_bps);
                        format!("a plain server: {v} {u}")
                    }))
                    .child(tile(
                        "relay-players",
                        "Players through it",
                        r.players.to_string(),
                        "",
                        "joined by the relay".into(),
                    )),
            )
        })
        .when_some(r.voice_text(), |c, v| c.child(widgets::caption(v, cx).text_xs()))
        .child(widgets::caption("Change or test the relay in Hopper Server.", cx).text_xs())
        .into_any_element()
}
