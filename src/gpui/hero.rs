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
    /// Fits the pills and the actions to the room the page has (see
    /// [`HeroFit`]); without it they keep their full width.
    pub fit: Option<HeroFit>,
}

/// How a hero shrinks when its page is narrow (a side panel open, a small
/// window): the pills past the first few fold into a "+N" pill, and the
/// actions step through the page's own levels (`actions_max` of them,
/// see `widgets::Fit`). The name column keeps [`TITLE_MIN`] for itself.
pub struct HeroFit {
    pub pills: widgets::Fit,
    /// The pills' labels, for the "+N" pill's tooltip.
    pub pill_names: Vec<SharedString>,
    pub actions: widgets::Fit,
    pub actions_max: usize,
    /// A key for the actions' content (`widgets::fit_key`).
    pub actions_key: u64,
    /// The figures' box over the actions, fitted to the same room: its
    /// cells narrow, then the last figures go one at a time, then the box.
    /// Without it a wide action row (an "Update pack to …") left the box
    /// pushing the column past the hero's edge.
    pub stats: widgets::Fit,
    /// The main facts' texts (Minecraft, loader, version): the name column
    /// keeps room for them and the name before the actions get any.
    pub facts: Vec<String>,
    pub view: EntityId,
}

/// The name column's narrowest width in rem: the actions give way before
/// the name goes below it.
pub const TITLE_MIN: f32 = 12.;

/// The room in rem the name column asks for before the actions give way:
/// the whole name and the main facts on one line, within reason (a very
/// long name still ellipsizes rather than pushing every action into ⋯).
/// Estimated from the texts, a little generous.
pub fn name_room(title: &str, facts: &[String]) -> f32 {
    let title = title.chars().count() as f32 * 0.95;
    let facts: f32 = facts.iter().map(|f| f.chars().count() as f32 * 0.5 + 1.5).sum::<f32>()
        + facts.len().saturating_sub(1) as f32;
    title.max(facts).clamp(TITLE_MIN, 28.)
}

/// One fact under a hero's title: a colored icon and its text. When the
/// name column is at its least (the actions took the rest), a fact that
/// runs out of room ends in "…" instead of being cut mid-word.
pub fn fact(icon: IconName, color: Hsla, text: impl Into<SharedString>) -> Div {
    h_flex()
        .flex_shrink(1.)
        .min_w_0()
        .gap_1p5()
        .child(Icon::new(icon).xsmall().text_color(color).flex_none())
        .child(div().min_w_0().truncate().child(text.into()))
}

/// A quieter fact (author, downloads, address): icon and text both muted.
/// Last in the row, it ellipsizes when the row runs out of room instead of
/// being cut mid-word.
pub fn soft_fact(icon: IconName, text: impl Into<SharedString>, cx: &App) -> Div {
    h_flex()
        .flex_shrink(1.)
        .min_w_0()
        .gap_1p5()
        .text_color(cx.theme().muted_foreground)
        .child(Icon::new(icon).xsmall().flex_none())
        .child(div().min_w_0().truncate().child(text.into()))
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
    let fit = h.fit;
    // One level past the page's own: every action is as small as it gets
    // and still doesn't fit, so the name gives up the room it kept and
    // ellipsizes instead of the actions being cut off at the edge.
    let squeezed = fit.as_ref().is_some_and(|f| f.actions.level() > f.actions_max);
    let title_min = fit
        .as_ref()
        .filter(|_| !squeezed)
        .map_or(TITLE_MIN, |f| name_room(h.title.as_ref(), &f.facts));
    let keep = fit.as_ref().map_or(TITLE_MIN, |f| name_room(h.title.as_ref(), &f.facts));
    // Past that (a side panel at its widest): the name at its least still
    // leaves the actions short, so the art gives its room too.
    let no_art = squeezed && fit.as_ref().is_some_and(|f| f.actions.short_by() > keep - TITLE_MIN);
    // Pills that don't fit fold into "+N", which names them on hover.
    let pills: AnyElement = match &fit {
        Some(f) => {
            let n = h.pills.len();
            let max = n.saturating_sub(1);
            // The pills' room is what the actions and the art leave the
            // name, both picked above.
            f.pills.context(f.actions.level() as u64 * 2 + no_art as u64);
            let hidden = f.pills.level().min(max);
            let rest: Vec<SharedString> = f.pill_names.iter().skip(n - hidden).cloned().collect();
            let key = widgets::fit_key(f.pill_names.iter().map(|s| s.as_ref()));
            f.pills
                .wrap(
                    "hero-pills",
                    h_flex()
                        .gap_1p5()
                        .children(h.pills.into_iter().take(n - hidden))
                        .when(hidden > 0, |r| {
                            let tip: SharedString = rest.join(", ").into();
                            r.child(
                                widgets::pill(format!("+{hidden}"), theme.muted_foreground, false)
                                    .id("hero-pills-more")
                                    .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx)),
                            )
                        }),
                    max,
                    key,
                    f.view,
                )
                .into_any_element()
        }
        None => h_flex().gap_1p5().flex_wrap().children(h.pills).into_any_element(),
    };
    let fx = crate::hero_drift::backdrop(h.art.as_deref(), h.title.as_ref(), cx);

    // The art: large, rounded, lifted off the backdrop.
    let art = div()
        .flex_none()
        .size(rems(6.5))
        .rounded(theme.radius_lg)
        .overflow_hidden()
        .bg(theme.muted)
        .border_1()
        .border_color(white().opacity(0.12))
        .shadow(vec![
            BoxShadow::new(px(0.), px(14.), black().opacity(if dark { 0.5 } else { 0.25 })).blur_radius(px(32.)),
        ])
        .map(|d| match h.art.clone() {
            Some(p) => d.child(img(crate::thumbs::source_in(crate::thumbs::Cat::Banner, &p, crate::thumbs::Want::Rems(rems(6.5)), cx)).size_full().rounded(theme.radius_lg - px(1.)).object_fit(ObjectFit::Cover)),
            None => d.flex().items_center().justify_center().child(h.no_art),
        });

    let line = white().opacity(0.08);
    // The figures' levels: 0 as they are, 1 narrower cells, then one
    // figure fewer per level, the last level none.
    let n_stats = h.stats.len();
    let stats_max = n_stats + 1;
    let stats_key = widgets::fit_key(h.stats.iter().flat_map(|(v, l)| [v.as_str(), l.as_str()]));
    let stats_lv = fit.as_ref().filter(|_| n_stats > 0).map_or(0, |f| f.stats.level().min(stats_max));
    let tight = stats_lv >= 1;
    let acts_last = fit.as_ref().is_some_and(|f| f.actions.level() > f.actions_max);
    let keep_n = if stats_lv <= 1 { n_stats } else { n_stats.saturating_sub(stats_lv - 1) };
    let stats = (keep_n > 0).then(|| {
        h_flex()
            .flex_none()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(line)
            .bg(black().opacity(if dark { 0.3 } else { 0.08 }))
            .children(h.stats.into_iter().take(keep_n).enumerate().map(|(i, (v, l))| {
                v_flex()
                    .when(!tight, |c| c.min_w(rems(5.75)).px_4())
                    .when(tight, |c| c.px_3())
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
        .when(!no_art, |r| r.child(art))
        .child(
            v_flex()
                .flex_1()
                .min_w(rems(title_min))
                .gap_1p5()
                .child(pills)
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
        .child(
            v_flex()
                // At the actions' last level they take the room they're
                // given (a server's one-line note is cut short): the column
                // gives way too, else it keeps their full width past the
                // hero's edge.
                .map(|c| if acts_last { c.flex_shrink(1.).min_w_0() } else { c.flex_none() })
                .items_end()
                .map(|c| match &fit {
                    // The figures fit to the actions' room too; at their
                    // last level the box is gone, the gap with it.
                    Some(f) if n_stats > 0 => c.child(
                        f.stats
                            .wrap_end("hero-stats", h_flex().children(stats), stats_max, stats_key, f.view)
                            .when(keep_n > 0, |d| d.mb_3()),
                    ),
                    _ => c.children(stats.map(|s| s.mb_3())),
                })
                .child(match &fit {
                    Some(f) => f
                        .actions
                        .wrap("hero-actions", h.actions, f.actions_max + 1, f.actions_key, f.view)
                        .into_any_element(),
                    None => h.actions.into_any_element(),
                }),
        )
        // The room the actions may take: the row less its padding, the art,
        // the gaps and the room the name keeps (the same at every level, so
        // the level a room settles on doesn't move the room). The figures
        // get the same room.
        .when_some(fit.as_ref(), |r, f| {
            let minus = 1.5 * 2. + 6.5 + 1.25 * 2. + keep;
            r.child(f.actions.room_probe(minus, f.actions_max + 1, f.view))
                .when(n_stats > 0, |r| r.child(f.stats.room_probe(minus, stats_max, f.view)))
        });

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
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(white().opacity(if dark { 0.1 } else { 0.5 }))
                    .bg(theme.foreground.opacity(0.03))
            } else {
                d.border_b_1().border_color(theme.border)
            }
        })
        .child(backdrop)
        .child(content)
        // Its box, for layout checks: the actions must end inside it.
        .child(widgets::bounds_probe("hero"))
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
            (None, Some(p)) => d.child(img(crate::thumbs::source_in(crate::thumbs::Cat::Banner, &p, crate::thumbs::Want::Window, cx)).size_full().object_fit(ObjectFit::Cover).opacity(if dark { 0.75 } else { 0.55 })),
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
#[allow(dead_code)] // unused since every server tab shows the full header
pub fn render_compact(h: Hero, cx: &App) -> AnyElement {
    let fx = crate::hero_drift::backdrop(h.art.as_deref(), &format!("compact:{}", h.title), cx);
    let theme = cx.theme();
    let glass = widgets::glass(cx);
    let dark = theme.mode.is_dark();
    let art = div()
        .flex_none()
        .size(rems(2.5))
        .rounded(theme.radius)
        .overflow_hidden()
        .bg(theme.muted)
        .border_1()
        .border_color(white().opacity(0.12))
        .map(|d| match h.art.clone() {
            Some(p) => d.child(img(crate::thumbs::source_in(crate::thumbs::Cat::Banner, &p, crate::thumbs::Want::Rems(rems(2.5)), cx)).size_full().rounded(theme.radius - px(1.)).object_fit(ObjectFit::Cover)),
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
                    .rounded(theme.radius_lg)
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
