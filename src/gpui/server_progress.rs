//! A server's start or stop under way, drawn where its buttons were: what
//! it is doing, a bar, and the time. The Library's server card and the
//! Servers page header use it; Hopper Server's web panel shows the same
//! (`ui/src/Progress.svelte`).

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::hosting::progress::{Progress, ProgressKind};

/// Blue while starting, amber while stopping (or a start slower than the
/// last), red for a stop that has gone quiet.
pub fn color(p: &Progress, cx: &App) -> Hsla {
    let theme = cx.theme();
    if p.stuck {
        theme.danger
    } else if p.kind == ProgressKind::Stop || p.slow {
        theme.warning
    } else {
        theme.info
    }
}

/// The bar alone, `h` tall.
pub fn bar(p: &Progress, h: Pixels, cx: &App) -> Div {
    let theme = cx.theme();
    div().h(h).w_full().rounded(h).bg(theme.muted).overflow_hidden().child(
        div().h_full().rounded(h).w(relative(p.percent as f32 / 100.0)).bg(color(p, cx)),
    )
}

/// What it is doing, the bar, and the time. `compact`: one short line under
/// the bar (the Library card); else the step on top and the time in a
/// sentence under it (the header). Hovering lists the steps.
pub fn block(id: impl Into<ElementId>, p: &Progress, compact: bool, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    let steps = p.steps_text();
    let under = if compact { format!("{} · {}", p.label, p.short) } else { p.detail.clone() };
    v_flex()
        .id(id.into())
        .min_w_0()
        .flex_1()
        .gap_1()
        .when(!compact, |c| {
            c.child(div().text_sm().truncate().text_color(theme.foreground).child(p.label.clone()))
        })
        .child(bar(p, px(if compact { 4. } else { 6. }), cx))
        .child(div().text_xs().truncate().text_color(theme.muted_foreground).child(under))
        .when(!steps.is_empty(), |c| {
            c.tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(steps.clone()).build(w, cx))
        })
}

/// A row: the block and, beside it, whatever button belongs (Stop, Kill…).
pub fn row(id: impl Into<ElementId>, p: &Progress, compact: bool, button: Option<AnyElement>, cx: &App) -> Div {
    h_flex().w_full().min_w_0().gap_2().items_center().child(block(id, p, compact, cx)).children(button)
}
