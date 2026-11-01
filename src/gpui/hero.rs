//! The big header over an instance (Library) or a server (Servers): its art
//! large over a blurred copy of itself, state pills, name, facts, a box of
//! figures and the actions. One look for both pages.

use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::PathBuf;

/// The hero's height in rem, the same on every tab.
pub const HERO: f32 = 9.5;
/// The folded hero's height in rem (see [`render_compact`]).
pub const HERO_COMPACT: f32 = 4.0;

/// What a hero shows.
pub struct Hero {
    /// The art, when there is a picture.
    pub art: Option<PathBuf>,
    /// The art blurred, for the backdrop (made off the UI thread).
    pub blur: Option<PathBuf>,
    /// What stands in for missing art (an icon or a letter tile).
    pub no_art: AnyElement,
    pub pills: Vec<AnyElement>,
    pub title: SharedString,
    /// The facts line under the title (see [`fact`]).
    pub facts: Div,
    /// One more line under the facts (a pack's summary, a server's MOTD).
    pub summary: Option<AnyElement>,
    /// (value, label) figures, shown in one box over the actions.
    pub stats: Vec<(String, String)>,
    pub actions: Div,
}

/// One fact under a hero's title: a colored icon and its text.
pub fn fact(icon: IconName, color: Hsla, text: impl Into<SharedString>) -> Div {
    h_flex()
        .flex_none()
        .gap_1p5()
        .child(Icon::new(icon).xsmall().text_color(color))
        .child(text.into())
}

/// A quieter fact (author, downloads, address): icon and text both muted.
pub fn soft_fact(icon: IconName, text: impl Into<SharedString>, cx: &App) -> Div {
    h_flex()
        .flex_none()
        .gap_1p5()
        .text_color(cx.theme().muted_foreground)
        .child(Icon::new(icon).xsmall())
        .child(text.into())
}

/// The facts row a hero's facts go in.
pub fn facts_row(cx: &App) -> Div {
    h_flex()
        .gap_4()
        .min_w_0()
        .overflow_hidden()
        .text_sm()
        .text_color(cx.theme().foreground.opacity(0.8))
}

pub fn render(h: Hero, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let glass = widgets::glass(cx);
    let dark = theme.mode.is_dark();
    let taller = h.summary.is_some();
    let fx = crate::hero_drift::backdrop(h.art.as_deref(), h.title.as_ref(), cx);

    // The art: large, rounded, lifted off the backdrop.
    let art = div()
        .flex_none()
        .size(rems(6.5))
        .rounded(rems(1.))
        .overflow_hidden()
        .bg(theme.muted)
        .border_1()
        .border_color(white().opacity(0.12))
        .shadow(vec![
            BoxShadow::new(px(0.), px(14.), black().opacity(if dark { 0.5 } else { 0.25 })).blur_radius(px(32.)),
        ])
        .map(|d| match h.art.clone() {
            Some(p) => d.child(img(p).size_full().object_fit(ObjectFit::Cover)),
            None => d.flex().items_center().justify_center().child(h.no_art),
        });

    let line = white().opacity(0.08);
    let stats = (!h.stats.is_empty()).then(|| {
        h_flex()
            .flex_none()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(line)
            .bg(black().opacity(if dark { 0.3 } else { 0.08 }))
            .children(h.stats.into_iter().enumerate().map(|(i, (v, l))| {
                v_flex()
                    .min_w(rems(5.75))
                    .px_4()
                    .py_1p5()
                    .when(i > 0, |c| c.border_l_1().border_color(line))
                    .child(div().text_base().font_weight(FontWeight::BOLD).child(v))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.muted_foreground)
                            .child(l.to_uppercase()),
                    )
            }))
    });

    // Pills over the title, the facts under it; on the right the figures
    // in one box over the actions.
    let content = h_flex()
        .absolute()
        .inset_0()
        .items_center()
        .gap_5()
        .px_6()
        .child(art)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1p5()
                .child(h_flex().gap_1p5().flex_wrap().children(h.pills))
                .child(
                    div()
                        .text_size(rems(1.65))
                        .font_weight(FontWeight::EXTRA_BOLD)
                        .line_height(relative(1.15))
                        .truncate()
                        .child(h.title),
                )
                .child(h.facts)
                .children(h.summary),
        )
        .child(v_flex().flex_none().items_end().gap_3().children(stats).child(h.actions));

    let backdrop = backdrop(h.blur, fx, cx);

    div()
        .relative()
        .flex_none()
        .overflow_hidden()
        .map(|d| {
            if glass {
                // A floating translucent panel, lit along its top edge.
                d.mx_3()
                    .mt_3()
                    .rounded(rems(1.))
                    .border_1()
                    .border_color(white().opacity(if dark { 0.1 } else { 0.5 }))
                    .bg(theme.foreground.opacity(0.03))
            } else {
                d.border_b_1().border_color(theme.border)
            }
        })
        .child(backdrop)
        .child(content)
        .h(rems(if taller { HERO + 1.5 } else { HERO }))
        .into_any_element()
}

/// The backdrop: the art blurred and stretched, faded into the page
/// (or, on glass, into a tint that keeps text readable).
/// The art blurred (or the moving drift, `fx`, when it's on) faded into
/// the page, or on glass into a tint that keeps text readable.
fn backdrop(blur: Option<PathBuf>, fx: Option<AnyElement>, cx: &App) -> Div {
    let theme = cx.theme();
    let glass = widgets::glass(cx);
    let dark = theme.mode.is_dark();
    let base = if glass {
        if dark { black() } else { white() }
    } else {
        theme.background
    };
    let (top_a, bottom_a, side_a) = if glass {
        (0.05, 0.6, 0.5)
    } else if fx.is_some() {
        (0.1, 0.9, 0.6)
    } else {
        (0.15, 1.0, 0.85)
    };
    div()
        .absolute()
        .inset_0()
        .map(|d| match (fx, blur) {
            (Some(fx), _) => d.child(fx),
            (None, Some(p)) => d.child(img(p).size_full().object_fit(ObjectFit::Cover).opacity(if dark { 0.75 } else { 0.55 })),
            (None, None) => d.bg(linear_gradient(
                135.,
                linear_color_stop(theme.primary.opacity(0.22), 0.),
                linear_color_stop(theme.primary.opacity(0.), 1.),
            )),
        })
        .child(div().absolute().inset_0().bg(linear_gradient(
            180.,
            linear_color_stop(base.opacity(top_a), 0.),
            linear_color_stop(base.opacity(bottom_a), 1.),
        )))
        .child(div().absolute().inset_0().bg(linear_gradient(
            90.,
            linear_color_stop(base.opacity(side_a), 0.),
            linear_color_stop(base.opacity(0.), 0.7),
        )))
}

/// The hero folded to one row, for the tabs past Overview: the same art,
/// name, state, facts, figures and actions, without the height. The
/// summary line and all but the first pill (the state) are left out:
/// Overview shows them.
pub fn render_compact(h: Hero, cx: &App) -> AnyElement {
    let fx = crate::hero_drift::backdrop(h.art.as_deref(), &format!("compact:{}", h.title), cx);
    let theme = cx.theme();
    let glass = widgets::glass(cx);
    let dark = theme.mode.is_dark();
    let art = div()
        .flex_none()
        .size(rems(2.5))
        .rounded(rems(0.5))
        .overflow_hidden()
        .bg(theme.muted)
        .border_1()
        .border_color(white().opacity(0.12))
        .map(|d| match h.art.clone() {
            Some(p) => d.child(img(p).size_full().object_fit(ObjectFit::Cover)),
            None => d.flex().items_center().justify_center().child(h.no_art),
        });
    // Figures as one quiet line: "0/16 players · 20.0 TPS · 17.1 GB memory".
    let stats = (!h.stats.is_empty()).then(|| {
        h_flex()
            .flex_none()
            .gap_3()
            .text_sm()
            .text_color(theme.muted_foreground)
            .children(h.stats.into_iter().map(|(v, l)| {
                h_flex()
                    .gap_1()
                    .child(div().font_weight(FontWeight::BOLD).text_color(theme.foreground).child(v))
                    .child(l.to_lowercase())
            }))
    });
    let content = h_flex()
        .absolute()
        .inset_0()
        .items_center()
        .gap_3()
        .px_6()
        .child(art)
        .child(
            div()
                .flex_none()
                .max_w(rems(18.))
                .text_size(rems(1.15))
                .font_weight(FontWeight::EXTRA_BOLD)
                .truncate()
                .child(h.title),
        )
        .child(h_flex().flex_none().gap_1p5().children(h.pills.into_iter().take(1)))
        .child(h.facts.flex_1())
        .children(stats)
        .child(h.actions);
    div()
        .relative()
        .flex_none()
        .overflow_hidden()
        .map(|d| {
            if glass {
                d.mx_3()
                    .mt_3()
                    .rounded(rems(1.))
                    .border_1()
                    .border_color(white().opacity(if dark { 0.1 } else { 0.5 }))
                    .bg(theme.foreground.opacity(0.03))
            } else {
                d.border_b_1().border_color(theme.border)
            }
        })
        .child(backdrop(h.blur, fx, cx))
        .child(content)
        .h(rems(HERO_COMPACT))
        .into_any_element()
}
