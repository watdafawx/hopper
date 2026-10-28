//! Small presentational pieces shared by the screens.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Hopper's logo as an image.
pub fn logo() -> Img {
    static LOGO: OnceLock<Arc<Image>> = OnceLock::new();
    let image = LOGO
        .get_or_init(|| {
            Arc::new(Image::from_bytes(
                ImageFormat::Png,
                include_bytes!("../../assets/hopper-64.png").to_vec(),
            ))
        })
        .clone();
    img(image)
}

/// A square project or instance icon, or a placeholder while it loads (or
/// when there is none).
pub fn thumb(path: Option<&Path>, size: Rems, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let frame = div()
        .size(size)
        .flex_none()
        .rounded(theme.radius)
        .overflow_hidden()
        .bg(theme.muted);
    match path {
        Some(p) => frame
            .child(
                img(p.to_path_buf())
                    .size_full()
                    .object_fit(ObjectFit::Cover),
            )
            .into_any_element(),
        None => frame
            .flex()
            .items_center()
            .justify_center()
            .text_color(theme.muted_foreground)
            .child(Icon::new(gpui_kit::assets::IconName::Package).small())
            .into_any_element(),
    }
}

/// A container that fades and rises into place when it first appears. A
/// new `id` replays the motion (e.g. another instance or tab).
pub fn enter(id: impl Into<ElementId>, container: Div) -> AnyElement {
    container
        .relative()
        .with_animation(
            id,
            Animation::new(Duration::from_millis(220)).with_easing(ease_out_quint()),
            |el, t| el.opacity(t).top(rems(0.5 * (1.0 - t))),
        )
        .into_any_element()
}

/// A side panel's content sliding in from the trailing edge.
pub fn slide_in(id: impl Into<ElementId>, container: Div) -> AnyElement {
    container
        .relative()
        .with_animation(
            id,
            Animation::new(Duration::from_millis(240)).with_easing(ease_out_quint()),
            |el, t| el.opacity(t).left(rems(1.5 * (1.0 - t))),
        )
        .into_any_element()
}

/// What a split button's changelog half sits beside: it takes that
/// button's color for its icon and border.
#[derive(Clone, Copy)]
pub enum Tint {
    Primary,
    Outline,
    Success,
}

/// The action half of a split button: square on the right, so the
/// changelog half joins it.
pub fn split_lead<E: Styled>(lead: E) -> E {
    lead.rounded_tr(px(0.)).rounded_br(px(0.))
}

/// A split button: the action, and a neutral changelog half on its right
/// whose icon and border pick up the action's color.
pub fn split(
    lead: impl IntoElement,
    id: impl Into<ElementId>,
    tint: Tint,
    size: gpui_kit::component::Size,
    tip: impl Into<SharedString>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let theme = cx.theme();
    let color = match tint {
        Tint::Primary => theme.primary,
        Tint::Outline => theme.muted_foreground,
        Tint::Success => theme.success,
    };
    let border = match tint {
        Tint::Outline => theme.border,
        _ => color.opacity(0.45),
    };
    let tail = Button::new(id)
        .icon(Icon::new(IconName::ScrollText))
        .custom(
            gpui_kit::component::button::ButtonCustomVariant::new(cx)
                .color(theme.secondary)
                .foreground(color)
                .hover(theme.secondary_hover)
                .active(theme.secondary_active),
        )
        .with_size(size)
        .border_1()
        .border_l_0()
        .border_color(border)
        .rounded_tl(px(0.))
        .rounded_bl(px(0.))
        .tooltip(tip)
        .on_click(on_click);
    h_flex().flex_none().items_center().child(lead).child(tail)
}

/// How wide a row's update pill is, so the changelog button that stands in
/// for it when there's no update takes the same space.
pub const SPLIT_W: f32 = 9.45;
/// The same pill with icons only, for narrow lists.
pub const SPLIT_W_COMPACT: f32 = 4.5;

/// The changelog half of a split button on its own, as wide as the pill
/// (`SPLIT_W`, or `SPLIT_W_COMPACT` and icon-only when `compact`), for rows
/// with nothing to do but read what changed.
pub fn split_alone(
    id: impl Into<ElementId>,
    compact: bool,
    size: gpui_kit::component::Size,
    tip: impl Into<SharedString>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Button {
    let theme = cx.theme();
    Button::new(id)
        .icon(Icon::new(IconName::ScrollText))
        .when(!compact, |b| b.label("Changelog"))
        .custom(
            gpui_kit::component::button::ButtonCustomVariant::new(cx)
                .color(theme.secondary)
                .foreground(theme.muted_foreground)
                .hover(theme.secondary_hover)
                .active(theme.secondary_active),
        )
        .with_size(size)
        .border_1()
        .border_color(theme.border)
        .w(rems(if compact { SPLIT_W_COMPACT } else { SPLIT_W }))
        .tooltip(tip)
        .on_click(on_click)
}

/// A muted, small caption line.
pub fn caption(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .truncate()
        .child(text.into())
}

/// Small muted text that wraps (a [`caption`] stays on one line).
pub fn fine_print(text: impl Into<SharedString>, cx: &App) -> Div {
    div().text_xs().text_color(cx.theme().muted_foreground).child(text.into())
}

/// The dialogs' top offset in rems, clear of the title bar: shared so every
/// dialog lines up.
pub const DIALOG_TOP: f32 = 3.5;
/// Everything around the content of a dialog without a footer, in rems:
/// the top offset, the title, padding and a bottom margin.
pub const DIALOG_CHROME: f32 = DIALOG_TOP + 6.;
/// The same with a footer of buttons.
pub const DIALOG_CHROME_FOOTER: f32 = DIALOG_TOP + 9.;

/// The tallest a dialog's content may be in this window.
pub fn dialog_room(window: &Window) -> Pixels {
    window.viewport_size().height - window.rem_size() * DIALOG_CHROME
}

/// Shared alpha steps for tints over the theme's colors, so hovers,
/// selections and tinted chips look the same on every page.
pub mod alpha {
    /// Hovered row or control: `foreground.opacity(HOVER)`.
    pub const HOVER: f32 = 0.06;
    /// Selected row: `primary.opacity(SELECTED)`.
    pub const SELECTED: f32 = 0.12;
    /// Tinted chip fill.
    pub const TINT: f32 = 0.10;
    /// Tinted chip or selection border.
    pub const LINE: f32 = 0.30;
}

/// A centered empty state: icon, a title, and an optional line saying
/// what to do next. Add the action button with `.child(...)`.
pub fn empty_state(icon: IconName, title: impl Into<SharedString>, body: Option<SharedString>, cx: &App) -> Div {
    let theme = cx.theme();
    v_flex()
        .w_full()
        .items_center()
        .justify_center()
        .gap_2()
        .px_4()
        .py_8()
        .child(
            div()
                .size(rems(2.5))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(theme.foreground.opacity(alpha::HOVER))
                .child(Icon::new(icon).size_5().text_color(theme.muted_foreground)),
        )
        .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(title.into()))
        .when_some(body, |d, b| {
            d.child(
                div()
                    .max_w(rems(28.))
                    .text_xs()
                    .text_center()
                    .text_color(theme.muted_foreground)
                    .child(b),
            )
        })
}

/// `n` placeholder rows of `height` while a list loads.
pub fn skeleton_rows(n: usize, height: Rems, cx: &App) -> Div {
    let theme = cx.theme();
    let mut col = v_flex().w_full().gap_1();
    for i in 0..n {
        col = col.child(
            h_flex()
                .h(height)
                .px_2()
                .gap_3()
                .child(div().size(rems(1.5)).rounded(theme.radius).bg(theme.skeleton))
                .child(
                    div()
                        .h(rems(0.625))
                        .w(relative(0.25 + 0.15 * ((i * 7) % 4) as f32))
                        .rounded_full()
                        .bg(theme.skeleton),
                ),
        );
    }
    col
}

/// What a mod's badge row says. Anything empty or false is left out.
#[derive(Clone, Copy, Default)]
pub struct ModTags<'a> {
    /// Unique among the rows (a file name): ids the "+n" chip's tooltip.
    pub key: &'a str,
    pub in_use: bool,
    pub new: bool,
    /// Added on top of the pack.
    pub added: bool,
    /// "client" | "server" | "both" | "".
    pub env: &'a str,
    pub categories: &'a [String],
    pub modrinth: bool,
    /// Named in the instance's last game crash: why.
    pub crashed: Option<&'a str>,
    /// Categories shown before the rest fold into "+n" (default 2). Fewer
    /// when the line has other chips to fit, so "+n" isn't clipped off.
    pub max_categories: Option<usize>,
}

/// Categories shown before the rest fold into "+n".
const MOD_TAG_CATEGORIES: usize = 2;

/// A mod's badges on one line, the same everywhere a mod is listed: new,
/// added, where it runs, its categories and where it came from.
pub fn mod_tags(t: ModTags, cx: &App) -> Div {
    let theme = cx.theme();
    let (warn, info, ok, bad, muted) =
        (theme.warning, theme.info, theme.success, theme.danger, theme.muted_foreground);
    let tag = |c: Hsla, text: SharedString| chip(c).child(text);
    let env = match t.env {
        "client" => Some(tag(warn, "Client only".into())),
        "server" => Some(tag(info, "Server only".into())),
        "both" => Some(tag(muted, "Client + server".into())),
        _ => None,
    };
    let shown = t.max_categories.unwrap_or(MOD_TAG_CATEGORIES).min(t.categories.len());
    let more = t.categories.len() - shown;
    let crashed = t.crashed.map(|why| {
        let why = SharedString::from(why.to_string());
        div()
            .id(ElementId::Name(format!("crashed:{}", t.key).into()))
            .flex_none()
            .child(chip(bad).child("Crashed"))
            .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(why.clone()).build(w, cx))
    });
    h_flex_row()
        .gap_1()
        .min_w_0()
        .overflow_hidden()
        .children(crashed)
        .when(t.in_use, |r| r.child(tag(info, "In use".into())))
        .when(t.new, |r| r.child(tag(ok, "New".into())))
        .when(t.added, |r| r.child(tag(warn, "Yours".into())))
        .children(env)
        .children(
            t.categories
                .iter()
                .take(shown)
                .map(|c| chip(muted).child(c.clone())),
        )
        .when(more > 0, |r| {
            let rest: SharedString = t.categories[shown..].join(", ").into();
            r.child(
                div()
                    .id(ElementId::Name(format!("more-tags:{}", t.key).into()))
                    .flex_none()
                    .text_xs()
                    .text_color(muted)
                    .child(format!("+{more}"))
                    .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(rest.clone()).build(w, cx)),
            )
        })
        .when(t.modrinth, |r| r.child(site_tag(true)))
}

/// CurseForge's orange or Modrinth's green, wherever a site is named.
pub fn site_color(modrinth: bool) -> Hsla {
    if modrinth { crate::theme::palette::MODRINTH } else { crate::theme::palette::CURSEFORGE }
}

/// A filled button in a site's color: green for Modrinth, else the theme's.
pub fn site_button(b: Button, modrinth: bool) -> Button {
    if modrinth { b.success() } else { b.primary() }
}

/// A small tag naming a site, in its color.
pub fn site_tag(modrinth: bool) -> gpui_kit::component::tag::Tag {
    chip(site_color(modrinth)).child(if modrinth { "Modrinth" } else { "CurseForge" })
}

fn h_flex_row() -> Div {
    div().flex().flex_row().items_center()
}

/// Whether the theme shows the desktop through the window (glass themes
/// have a see-through page color).
pub fn glass(cx: &App) -> bool {
    cx.theme().background.a < 0.99
}

/// A rounded status pill: `color` text on a tinted fill, no border, with a
/// small glowing dot when `dot`.
pub fn pill(text: impl Into<SharedString>, color: Hsla, dot: bool) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_1p5()
        .h(rems(1.375))
        .px_2p5()
        // The theme's button radius (theme.json "radius"), like every chip.
        .rounded(px(crate::theme::RADIUS))
        .bg(color.opacity(alpha::TINT))
        .text_color(color)
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .when(dot, |d| {
            d.child(
                div()
                    .size_1p5()
                    .rounded_full()
                    .bg(color)
                    .shadow(vec![BoxShadow::new(px(0.), px(0.), color.opacity(0.8)).blur_radius(px(6.))]),
            )
        })
        .child(text.into())
}

/// A small figure with a caption under it, on a faint tile.
pub fn stat_tile(value: impl Into<SharedString>, label: &str, cx: &App) -> Div {
    let theme = cx.theme();
    v_flex()
        .flex_none()
        .min_w(rems(6.5))
        .px_3p5()
        .py_2p5()
        .rounded(theme.radius_lg)
        .bg(theme.foreground.opacity(alpha::HOVER))
        .border_1()
        .border_color(theme.foreground.opacity(alpha::SELECTED))
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::BOLD)
                .line_height(rems(1.5))
                .child(value.into()),
        )
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.muted_foreground)
                .child(label.to_uppercase()),
        )
}

/// A hovered row, nav item or icon button: the theme's quiet list wash
/// (about `foreground.opacity(alpha::HOVER)`). Use it inside `.hover(..)`.
pub fn row_hover<E: Styled>(el: E, cx: &App) -> E {
    el.bg(cx.theme().list_hover)
}

/// A selected row in a vertical list: a primary wash with a 3px primary
/// bar on the left, painted inside so nothing moves.
pub fn row_selected<E: Styled>(el: E, cx: &App) -> E {
    let theme = cx.theme();
    el.bg(theme.list_active).shadow(vec![BoxShadow::new(px(3.), px(0.), theme.primary).inset()])
}

/// `row_selected` in another color (a site's, say).
pub fn row_selected_in<E: Styled>(el: E, color: Hsla) -> E {
    el.bg(color.opacity(alpha::SELECTED)).shadow(vec![BoxShadow::new(px(3.), px(0.), color).inset()])
}

/// A selected tab or top-nav item: full-strength text and a 2px primary
/// underline, painted inside so nothing moves. No fill.
pub fn tab_selected<E: Styled>(el: E, cx: &App) -> E {
    let theme = cx.theme();
    el.text_color(theme.foreground).shadow(vec![BoxShadow::new(px(0.), px(-2.), theme.primary).inset()])
}

/// The focus ring: 2px in the theme's `ring` color, just inside the edge
/// (an outer shadow would show through see-through elements as a fill).
/// For a custom `.focus_visible(..)` style; `clickable` already adds it.
pub fn focus_shadow(cx: &App) -> Vec<BoxShadow> {
    vec![BoxShadow::new(px(0.), px(0.), cx.theme().ring).spread_radius(px(2.)).inset()]
}

/// The theme's focus ring on `el` when `focused`.
pub fn focus_ring<E: Styled>(el: E, focused: bool, cx: &App) -> E {
    if !focused {
        return el;
    }
    el.shadow(focus_shadow(cx))
}

/// A hand-built clickable element (a div with an id and `on_click`):
/// a pointer cursor, a Tab stop, and the focus ring while it has keyboard
/// focus. GPUI runs its `on_click` on Enter or Space when focused.
pub fn clickable<E: InteractiveElement + Styled>(el: E, cx: &App) -> E {
    let ring = focus_shadow(cx);
    el.cursor_pointer().tab_index(0).focus_visible(move |s| s.shadow(ring))
}

/// `widgets::clickable` as a method: `div().id(..).clickable(cx).on_click(..)`.
pub trait Clickable: InteractiveElement + Styled + Sized {
    fn clickable(self, cx: &App) -> Self {
        clickable(self, cx)
    }
}

impl<E: InteractiveElement + Styled> Clickable for E {}

/// A chip: `color` text on a faint fill of it, no border, the theme's
/// radius. gpui_kit's xsmall tag size; `.small()` still works.
pub fn chip(color: Hsla) -> gpui_kit::component::tag::Tag {
    gpui_kit::component::tag::Tag::custom(color.opacity(alpha::TINT), color, gpui_kit::transparent_black())
        .xsmall()
        .flex_none()
        .rounded(px(crate::theme::RADIUS))
}

/// A tinted bar across a page with an icon at its start, for a notice or
/// an offer (`color`: info blue, warning…); add its text and buttons.
pub fn notice_bar(color: Hsla, icon: IconName, cx: &App) -> Div {
    let theme = cx.theme();
    h_flex()
        .px_4()
        .py_2()
        .gap_3()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(color.opacity(0.3))
        .bg(color.opacity(0.08))
        .child(Icon::new(icon).small().flex_none().text_color(color))
}

/// One line of a summary ("242 mods: 17 added…"): a tinted icon, `body`,
/// and "Show ›" / "Hide ⌄" at the end when it opens a list (`open` is
/// `Some`). Add the click with `.on_click`.
pub fn summary_row(
    id: impl Into<ElementId>,
    icon: IconName,
    color: Hsla,
    body: impl IntoElement,
    open: Option<bool>,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    let (muted, accent) = (theme.muted_foreground, theme.primary);
    h_flex()
        .id(id)
        .gap_2p5()
        .py_1()
        .text_sm()
        .child(
            div()
                .flex_none()
                .size(rems(1.25))
                .rounded(px(crate::theme::RADIUS))
                .bg(color.opacity(alpha::TINT))
                .flex()
                .items_center()
                .justify_center()
                .child(Icon::new(icon).xsmall().text_color(color)),
        )
        .child(h_flex().flex_1().min_w_0().gap_1p5().flex_wrap().child(body))
        .when_some(open, |r, open| {
            clickable(r, cx)
                .hover(move |s| s.text_color(accent))
                .child(
                    h_flex()
                        .flex_none()
                        .gap_0p5()
                        .text_xs()
                        .text_color(if open { accent } else { muted })
                        .child(if open { "Hide" } else { "Show" })
                        .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall()),
                )
        })
}

/// The list a [`summary_row`] opens, indented under it.
pub fn expand_area(cx: &App) -> Div {
    v_flex()
        .ml(rems(1.9))
        .mb_2()
        .pt_2()
        .gap_2()
        .border_t_1()
        .border_color(cx.theme().border)
}

/// A small round status dot (a server's state: green running, grey
/// stopped, red crashed).
pub fn status_dot(color: Hsla) -> Div {
    div().flex_none().size_2().rounded_full().bg(color)
}

/// A soft colored glow under `el` (a button), stronger on glass themes.
pub fn glow(el: impl IntoElement, color: Hsla, cx: &App) -> Div {
    let strength = if glass(cx) { 0.55 } else { 0.3 };
    div()
        .flex_none()
        .rounded(cx.theme().radius)
        .shadow(vec![
            BoxShadow::new(px(0.), px(6.), color.opacity(strength)).blur_radius(px(20.)),
        ])
        .child(el)
}

/// A tiny line chart of `values` (oldest first), scaled to `max` (or to
/// the largest value when `max` is 0), with a faint fill under the line.
pub fn sparkline(values: Vec<f32>, max: f32, color: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            if values.len() < 2 {
                return;
            }
            let top = if max > 0.0 {
                max
            } else {
                values.iter().cloned().fold(0.0, f32::max).max(1.0)
            };
            let (w, h) = (bounds.size.width, bounds.size.height);
            let step = w / (values.len() - 1) as f32;
            let at = |i: usize, v: f32| {
                let y = 1.0 - (v / top).clamp(0.0, 1.0);
                point(
                    bounds.origin.x + step * i as f32,
                    bounds.origin.y + px(2.) + (h - px(4.)) * y,
                )
            };
            let mut fill = PathBuilder::fill();
            fill.move_to(point(bounds.origin.x, bounds.origin.y + h));
            for (i, v) in values.iter().enumerate() {
                fill.line_to(at(i, *v));
            }
            fill.line_to(point(bounds.origin.x + w, bounds.origin.y + h));
            fill.close();
            if let Ok(p) = fill.build() {
                window.paint_path(
                    p,
                    linear_gradient(
                        180.,
                        linear_color_stop(color.opacity(0.3), 0.),
                        linear_color_stop(color.opacity(0.), 1.),
                    ),
                );
            }
            let mut line = PathBuilder::stroke(px(1.5));
            line.move_to(at(0, values[0]));
            for (i, v) in values.iter().enumerate().skip(1) {
                line.line_to(at(i, *v));
            }
            if let Ok(p) = line.build() {
                window.paint_path(p, color);
            }
        },
    )
    .w_full()
    .h(rems(2.25))
}

/// A full-width dropdown button listing `items` (label, checked).
pub fn picker(
    id: impl Into<ElementId>,
    label: String,
    items: Vec<(String, bool)>,
    rem: Pixels,
    on_pick: Rc<dyn Fn(usize, &mut Window, &mut App)>,
) -> impl IntoElement {
    picker_with(id, label, None, items.into_iter().map(|(l, c)| (l, c, None)).collect(), rem, on_pick)
}

/// What a [`picker_with`] entry shows before its name: a single-color
/// logo (asset path and color) or a picture file.
#[derive(Clone)]
pub enum PickIcon {
    Svg(&'static str, Hsla),
    Image(std::path::PathBuf),
}

impl PickIcon {
    fn icon(&self) -> Option<Icon> {
        match self {
            PickIcon::Svg(path, color) => Some(Icon::new(IconName::Box).path(*path).text_color(*color)),
            PickIcon::Image(_) => None,
        }
    }
}

/// [`picker`] with an icon or picture beside each entry, and the picked
/// one's logo on the button.
pub fn picker_with(
    id: impl Into<ElementId>,
    label: String,
    lead: Option<PickIcon>,
    items: Vec<(String, bool, Option<PickIcon>)>,
    rem: Pixels,
    on_pick: Rc<dyn Fn(usize, &mut Window, &mut App)>,
) -> impl IntoElement {
    Button::new(id)
        .outline()
        .small()
        .w_full()
        // Logo and name as one child, so the caret's spacing keeps them
        // together on the left.
        .map(|b| match lead.and_then(|l| l.icon()) {
            Some(i) => b.child(
                h_flex()
                    .min_w_0()
                    .gap_1p5()
                    .child(i.small().flex_none())
                    .child(div().min_w_0().truncate().child(label)),
            ),
            None => b.label(label),
        })
        .dropdown_caret(true)
        .dropdown_menu(move |menu: PopupMenu, _, _| {
            let mut menu = menu
                .min_w(rems(13.).to_pixels(rem))
                .max_h(rems(26.).to_pixels(rem))
                .scrollable(true);
            // Still being fetched: say so rather than open empty.
            if items.is_empty() {
                menu = menu.item(PopupMenuItem::new("Loading versions…").disabled(true));
            }
            for (ix, (l, checked, icon)) in items.iter().enumerate() {
                let f = on_pick.clone();
                let item = match icon {
                    Some(PickIcon::Image(path)) => {
                        let (path, l) = (path.clone(), l.clone());
                        PopupMenuItem::element(move |_, _| {
                            h_flex()
                                .gap_2()
                                .child(img(path.clone()).size(rems(1.25)).rounded_sm().flex_none())
                                .child(l.clone())
                        })
                    }
                    Some(i) => PopupMenuItem::new(l.clone()).when_some(i.icon(), |it, i| it.icon(i)),
                    None => PopupMenuItem::new(l.clone()),
                };
                menu = menu.item(item.checked(*checked).on_click(move |_, w, cx| f(ix, w, cx)));
            }
            menu
        })
}

/// A small filter dropdown ("Side ▾"): `name` while the first (no filter)
/// option is picked, else the picked option; a check by the picked one.
pub fn filter_menu(
    id: impl Into<ElementId>,
    name: &'static str,
    options: Vec<&'static str>,
    picked: usize,
    on_pick: Rc<dyn Fn(usize, &mut Window, &mut App)>,
) -> impl IntoElement {
    let label = match options.get(picked).filter(|_| picked != 0) {
        Some(o) => o.to_string(),
        None => name.to_string(),
    };
    Button::new(id)
        .small()
        .outline()
        .label(label)
        .dropdown_caret(true)
        .dropdown_menu(move |menu: PopupMenu, _, _| {
            options.iter().enumerate().fold(menu.label(name), |m, (ix, l)| {
                let f = on_pick.clone();
                m.item(PopupMenuItem::new(*l).checked(ix == picked).on_click(move |_, w, cx| f(ix, w, cx)))
            })
        })
}

/// "332.8K downloads".
pub fn downloads_label(count: f64) -> String {
    match count as u64 {
        n if n >= 1_000_000 => format!("{:.1}M downloads", n as f64 / 1e6),
        n if n >= 1_000 => format!("{:.1}K downloads", n as f64 / 1e3),
        n => format!("{n} downloads"),
    }
}

/// The page of the modpack an instance came from, as CurseForge or
/// Modrinth shows it: name, author, summary and the full description, with
/// a button to the site. `None` while nothing has been asked for yet.
/// Without `head`, only the description (the instance banner has the rest).
/// `desc` draws the description, its GIFs playing: give it the pack with
/// `DescView::show_pack` first.
pub fn pack_page(
    page: Option<&crate::store::PackPage>,
    icon: Option<&Path>,
    desc: &Entity<DescView>,
    head: bool,
    cx: &App,
) -> AnyElement {
    use crate::store::PackPage;
    use gpui_kit::assets::IconName;
    use gpui_kit::component::h_flex;
    let theme = cx.theme();
    let m = match page {
        Some(PackPage::Ready(m, ..)) => m,
        Some(PackPage::Failed(e)) => {
            return caption(format!("Couldn't load the pack's page: {e}"), cx).into_any_element();
        }
        _ => return caption("Loading the pack's page…", cx).into_any_element(),
    };
    let site = if m.modrinth.is_some() { "Modrinth" } else { "CurseForge" };
    let url = m.page();
    let downloads = downloads_label(m.download_count);
    v_flex()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
        .bg(theme.foreground.opacity(0.025))
        .when(head, |c| c.child(
            h_flex()
                .gap_4()
                .p_4()
                .items_center()
                .border_b_1()
                .border_color(theme.border)
                .child(thumb(icon, rems(3.5), cx))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(
                            h_flex()
                                .gap_2()
                                .child(div().text_lg().font_weight(FontWeight::BOLD).truncate().child(m.name.clone()))
                                .child(pill(site, theme.primary, false)),
                        )
                        .child(caption(
                            format!("by {} · {downloads}", m.author()),
                            cx,
                        ))
                        .when(!m.summary.is_empty(), |c| {
                            c.child(div().text_sm().line_clamp(2).child(m.summary.clone()))
                        }),
                )
                .child(
                    Button::new("pack-site")
                        .small()
                        .outline()
                        .text_color(site_color(m.modrinth.is_some()))
                        .icon(IconName::ExternalLink)
                        .label(format!("Open on {site}"))
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                ),
        ))
        .child(div().p_4().child(desc.clone()))
        .into_any_element()
}

/// A 1x1 transparent PNG: what a description image shows until the store
/// has cached it (GPUI would otherwise fetch it itself, uncached).
fn blank_image() -> Arc<Image> {
    static BLANK: OnceLock<Arc<Image>> = OnceLock::new();
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x60,
        0x00, 0x02, 0x00, 0x00, 0x05, 0x00, 0x01, 0x7a, 0x5e, 0xab, 0x3f, 0x00, 0x00, 0x00, 0x00,
        0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    BLANK
        .get_or_init(|| Arc::new(Image::from_bytes(ImageFormat::Png, PNG.to_vec())))
        .clone()
}

/// The pixel size of a local image, read from its header once per path.
fn image_size(path: &Path) -> Option<(u32, u32)> {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Mutex;
    static SIZES: OnceLock<Mutex<HashMap<PathBuf, Option<(u32, u32)>>>> = OnceLock::new();
    let mut sizes = SIZES.get_or_init(Default::default).lock().ok()?;
    *sizes
        .entry(path.to_path_buf())
        .or_insert_with(|| image::image_dimensions(path).ok())
}

/// A bar over author-written text (changelogs, descriptions) that isn't in
/// English: translate it on this PC, see how far along that is, or go back
/// to the original. `what` names the text ("This changelog"). Nothing shows
/// (and nothing is detected) unless translation is on in Settings.
///
/// The texts themselves show translated through
/// `store.translations().shown(md)`.
pub fn translate_bar(
    id: impl Into<SharedString>,
    what: &str,
    texts: Vec<SharedString>,
    store: &Entity<crate::store::Store>,
    cx: &App,
) -> Option<AnyElement> {
    use crate::translation::BarState;
    let s = store.read(cx);
    if !s.settings().translate || texts.iter().all(|t| t.trim().is_empty()) {
        return None;
    }
    let tr = s.translations();
    let lang = tr.lang(&texts)?;
    let installed = hopper::translate::installed(lang);
    let always = s.settings().translate_always.iter().any(|c| c == lang.code);
    let download = s.tr_download(lang);
    // "Always translate": straight to English once the model is here.
    if always && installed && download.is_none() && !tr.asked(&texts) {
        tr.request(texts.iter().map(|t| t.to_string()).collect(), lang);
    }
    let state = match &download {
        Some(j) if j.done => BarState::Failed(j.error.clone().unwrap_or_default()),
        Some(j) => BarState::Downloading(j.progress),
        None => tr.bar(&texts),
    };
    let id: SharedString = id.into();
    let theme = cx.theme();
    let color = theme.info;
    let texts = Rc::new(texts);
    let start = {
        let (store, texts) = (store.clone(), texts.clone());
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            store.read(cx).translations().clear_failed(&texts);
            let t = texts.iter().map(|t| t.to_string()).collect();
            store.update(cx, |s, cx| s.tr_start(t, lang, cx));
        }
    };
    let busy = |label: String| {
        h_flex()
            .gap_2()
            .child(gpui_kit::component::spinner::Spinner::new().small())
            .child(div().text_sm().child(label))
    };
    // Text first, controls after it; they wrap under it in a narrow panel.
    let row = || h_flex().flex_1().min_w_0().flex_wrap().gap_x_3().gap_y_1();
    let text = |t: String| div().flex_1().min_w(rems(12.)).text_sm().child(t);
    let body = match state {
        BarState::Idle => row()
            .child(text(format!("{what} is in {}.", lang.name)))
            .child(
                gpui_kit::component::checkbox::Checkbox::new(SharedString::from(format!("{id}-always")))
                    .label(format!("Always translate {}", lang.name))
                    .small()
                    .checked(always)
                    .on_click({
                        let (store, texts) = (store.clone(), texts.clone());
                        move |v: &bool, _, cx| {
                            let v = *v;
                            store.update(cx, |s, cx| {
                                // Ticking it translates what's shown now too.
                                if v {
                                    s.tr_start(texts.iter().map(|t| t.to_string()).collect(), lang, cx);
                                }
                                s.update_settings(
                                    |st| {
                                        st.translate_always.retain(|c| c != lang.code);
                                        if v {
                                            st.translate_always.push(lang.code.to_string());
                                        }
                                    },
                                    cx,
                                )
                            });
                        }
                    }),
            )
            .child(
                Button::new(SharedString::from(format!("{id}-go")))
                    .label(if installed {
                        "Translate".to_string()
                    } else {
                        format!("Translate ({} MB download)", lang.download >> 20)
                    })
                    .when(!installed, |b| {
                        b.tooltip(format!(
                            "Downloads the {} to English model once from Hugging Face. Translation runs on this PC; no text is sent anywhere.",
                            lang.name
                        ))
                    })
                    .outline()
                    .small()
                    .on_click(start),
            )
            .into_any_element(),
        BarState::Downloading(p) => busy(match p {
            Some(p) => format!("Downloading the {} model, {:.0}%", lang.name, p * 100.),
            None => format!("Downloading the {} model", lang.name),
        })
        .into_any_element(),
        BarState::Working => busy("Translating on this PC".into()).into_any_element(),
        BarState::Done => row()
            .child(text(format!(
                "Translated from {} on this PC. Machine translation can get game terms wrong.",
                lang.name
            )))
            .child(
                Button::new(SharedString::from(format!("{id}-orig")))
                    .label("Show original")
                    .ghost()
                    .small()
                    .on_click({
                        let (store, texts) = (store.clone(), texts.clone());
                        move |_, _, cx| store.read(cx).translations().show_original(&texts)
                    }),
            )
            .into_any_element(),
        BarState::Failed(e) => row()
            .child(text(format!("Couldn't translate: {e}")).text_color(theme.danger))
            .child(
                Button::new(SharedString::from(format!("{id}-retry")))
                    .label("Try again")
                    .outline()
                    .small()
                    .on_click(start),
            )
            .into_any_element(),
    };
    Some(
        h_flex()
            .id(id)
            .w_full()
            .gap_2()
            .px_3()
            .py_1p5()
            .rounded(theme.radius)
            .border_1()
            .border_color(color.opacity(0.3))
            .bg(color.opacity(0.08))
            .child(Icon::new(IconName::Languages).small().text_color(color))
            .child(body)
            .into_any_element(),
    )
}

/// How long until an animated image of this description (one the store
/// decoded, see `Store::want_anim`) shows its next frame: when the page
/// showing it should redraw.
pub fn description_wait(images: &[String], store: &crate::store::Store) -> Option<std::time::Duration> {
    images.iter().filter_map(|k| store.anim(k)).map(|c| c.wait_now()).min()
}

/// A project description (from `richtext::to_segments`) with its images
/// served from the store's cache (stills of animated ones, unless the store
/// decoded them to play, scaled to the width the page gave them) and opened in Hopper's image viewer on click;
/// other links open in the browser. Rows of badges sit side by side,
/// wrapping. `images` are the description's image keys, as
/// `richtext::images` lists them. It does not scroll by itself, so it can
/// sit in a scrolling column.
pub fn description_view(
    id: ElementId,
    segments: &[hopper::richtext::Segment],
    images: &[String],
    store: &Entity<crate::store::Store>,
    cx: &App,
) -> AnyElement {
    use hopper::richtext::Segment;
    let s = store.read(cx);
    let cached: Arc<std::collections::HashMap<String, Arc<Path>>> = Arc::new(
        images
            .iter()
            .filter_map(|u| Some((u.clone(), Arc::from(s.image(u)?))))
            .collect(),
    );
    // Each draw shows the frame for now (asked only for images drawn); the
    // page redraws on the next one.
    let clips: Arc<std::collections::HashMap<String, Arc<crate::anim::Clip>>> = Arc::new(
        images
            .iter()
            .filter_map(|k| Some((k.clone(), s.anim(k)?.clone())))
            .collect(),
    );
    // The viewer shows the full-size originals, video posters aside.
    let posters: Vec<&str> = segments
        .iter()
        .filter_map(|s| match s {
            Segment::Video(v) => v.thumb.as_deref(),
            _ => None,
        })
        .collect();
    let gallery: Arc<Vec<(String, String)>> = Arc::new(
        images
            .iter()
            .filter(|k| !posters.contains(&k.as_str()))
            .map(|k| (hopper::imgcache::split_width(k).0.to_string(), String::new()))
            .collect(),
    );
    let open = {
        let store = store.clone();
        let gallery = gallery.clone();
        move |url: &str, window: &mut Window, cx: &mut App| {
            match gallery.iter().position(|(u, _)| u == url) {
                Some(i) => {
                    crate::image_viewer::open(store.clone(), (*gallery).clone(), i, window, cx)
                }
                None => cx.open_url(url),
            }
        }
    };
    let texts: Vec<SharedString> = segments
        .iter()
        .filter_map(|s| match s {
            Segment::Markdown(md) => Some(SharedString::from(md.clone())),
            _ => None,
        })
        .collect();
    let bar = translate_bar(format!("{id}:tr"), "This description", texts, store, cx);
    let tr = store.read(cx).translations();
    v_flex()
        .w_full()
        .when_some(bar, |v, bar| v.child(div().pb_2().child(bar)))
        .children(segments.iter().enumerate().map(|(i, seg)| match seg {
            // GPUI Base's own view: the component wrapper has no image hook.
            Segment::Markdown(md) => {
                let cached = cached.clone();
                let clips = clips.clone();
                let open = open.clone();
                let shown = tr.shown(md);
                gpui_kit::base::TextView::markdown(
                    ElementId::Name(format!("{id}:{i}{}", if shown.is_some() { ":en" } else { "" }).into()),
                    shown.unwrap_or_else(|| SharedString::from(md.clone())),
                )
                .scrollable(false)
                .selectable(true)
                .image_source(move |uri| {
                    if let Some(c) = clips.get(uri.as_ref()) {
                        return ImageSource::Render(c.frame_now());
                    }
                    match cached.get(uri.as_ref()) {
                        Some(p) => ImageSource::Resource(Resource::Path(p.clone())),
                        None => ImageSource::Image(blank_image()),
                    }
                })
                .on_link_click(move |url, _, window, cx| open(url.as_ref(), window, cx))
                .into_any_element()
            }
            Segment::Badges(badges) => gpui_kit::component::h_flex()
                .flex_wrap()
                .gap_2()
                .py_2()
                .children(badges.iter().enumerate().map(|(j, b)| {
                    let path = cached.get(&b.key).cloned();
                    let natural = path.as_deref().and_then(image_size);
                    let w = b.width.or(natural.map(|n| n.0)).unwrap_or(88).max(1) as f32;
                    let h = match natural {
                        Some((nw, nh)) if nw > 0 => w * nh as f32 / nw as f32,
                        _ => 20.,
                    };
                    let target = b.link.clone().unwrap_or_else(|| b.url.clone());
                    let frame = clips.get(&b.key).map(|c| c.frame_now());
                    let open = open.clone();
                    div()
                        .id(ElementId::NamedInteger(format!("{id}:{i}").into(), j as u64))
                        .flex_none()
                        .w(px(w))
                        .h(px(h))
                        .max_w_full()
                        .clickable(cx)
                        .hover(|s| s.opacity(0.85))
                        .on_click(move |_, window, cx| open(&target, window, cx))
                        .when_some(path, |d, p| {
                            let src = match frame {
                                Some(f) => ImageSource::Render(f),
                                None => ImageSource::Resource(Resource::Path(p)),
                            };
                            d.child(img(src).size_full().object_fit(ObjectFit::Contain))
                        })
                }))
                .into_any_element(),
            Segment::Video(v) => video_tile(
                ElementId::NamedInteger(format!("{id}:video").into(), i as u64),
                v,
                v.thumb.as_ref().and_then(|t| cached.get(t).cloned()),
                cx,
            ),
        }))
        .into_any_element()
}

/// A description's video: its poster (or a plain panel naming the site) at
/// 16:9 with a play button; a click plays it right there, in a player
/// window kept over the tile (`crate::video`), with a bar below to stop it.
fn video_tile(
    id: ElementId,
    v: &hopper::richtext::Video,
    poster: Option<Arc<Path>>,
    cx: &App,
) -> AnyElement {
    use gpui_kit::assets::IconName;
    let theme = cx.theme();
    let video = v.clone();
    let site = v.site();
    if crate::video::playing(&v.url) {
        let url = v.url.clone();
        let keep_id = ElementId::Name(format!("{id}:keep").into());
        let page = v.url.clone();
        return v_flex()
            .id(id.clone())
            .my_2()
            .w_full()
            .max_w(px(640.))
            .gap_1()
            .child(
                div()
                    .w_full()
                    .aspect_ratio(16. / 9.)
                    .rounded(theme.radius)
                    .overflow_hidden()
                    .bg(gpui_kit::black())
                    .child(
                        canvas(
                            move |bounds, window, cx| {
                                window.use_keyed_state(keep_id, cx, {
                                    let url = url.clone();
                                    |_, _| crate::video::TileKeep(url)
                                });
                                let mask = window.content_mask().bounds;
                                let visible = bounds.intersect(&mask);
                                crate::video::place(&url, bounds, visible, window.scale_factor());
                            },
                            |_, _, _, _| {},
                        )
                        .size_full(),
                    ),
            )
            .child(
                gpui_kit::component::h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(match site {
                                "video" => "Playing".to_string(),
                                s => format!("Playing from {s}"),
                            }),
                    )
                    .child(
                        Button::new(ElementId::Name(format!("{id}:browser").into()))
                            .ghost()
                            .small()
                            .icon(IconName::ExternalLink)
                            .label("Open in browser")
                            .on_click(move |_, _, cx| {
                                crate::video::stop();
                                cx.open_url(&page);
                            }),
                    )
                    .child(
                        Button::new(ElementId::Name(format!("{id}:stop").into()))
                            .ghost()
                            .small()
                            .icon(IconName::X)
                            .label("Close")
                            .on_click(|_, window, _| {
                                crate::video::stop();
                                window.refresh();
                            }),
                    ),
            )
            .into_any_element();
    }
    div()
        .id(id)
        .relative()
        .my_2()
        .w_full()
        .max_w(px(640.))
        .aspect_ratio(16. / 9.)
        .rounded(theme.radius)
        .overflow_hidden()
        .bg(theme.muted)
        .clickable(cx)
        .on_click(move |_, window, cx| crate::video::play(&video, window, cx))
        .when_some(poster, |d, p| {
            d.child(
                img(ImageSource::Resource(Resource::Path(p)))
                    .absolute()
                    .size_full()
                    .object_fit(ObjectFit::Cover),
            )
        })
        .child(
            v_flex()
                .absolute()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .child(
                    div()
                        .size(px(64.))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(gpui_kit::black().opacity(0.6))
                        .border_2()
                        .border_color(gpui_kit::white().opacity(0.85))
                        .child(
                            Icon::new(IconName::Play)
                                .size(px(28.))
                                .text_color(gpui_kit::white()),
                        ),
                )
                .when(v.thumb.is_none(), |d| {
                    d.child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(match site {
                                "video" => "Video".to_string(),
                                s => format!("{s} video"),
                            }),
                    )
                }),
        )
        .hover(|s| s.opacity(0.9))
        .into_any_element()
}

// ---- downloads and finished actions in lists --------------------------------
//
// One way everywhere a list has a button per row that downloads or changes
// something: the button fills as it works, turns into a green mark when
// done, and done rows stay put while the user keeps clicking; once they
// stop (or move off the list), the done rows fade out together.

/// How long without a click before done rows go.
pub const SETTLE: Duration = Duration::from_millis(1200);
/// Done rows fade out over this long.
pub const FADE: Duration = Duration::from_millis(350);

/// When the user last clicked a row's action, and whether the pointer is
/// over the list: done rows wait while they're busy clicking.
#[derive(Clone, Copy, Default)]
pub struct Settle {
    pub last_click: Option<std::time::Instant>,
    pub pointer_in: bool,
}

impl Settle {
    pub fn clicked(&mut self) {
        self.last_click = Some(std::time::Instant::now());
    }

    /// Done rows may go now.
    pub fn settled(&self) -> bool {
        !self.pointer_in || self.last_click.is_none_or(|t| t.elapsed() >= SETTLE)
    }
}

/// `el` with progress `p` (0..=1) drawn over it from the left, while known.
pub fn progress_fill(el: impl IntoElement, p: Option<f32>, cx: &App) -> Div {
    let color = cx.theme().primary.opacity(0.28);
    div().relative().child(el).when_some(p, |d, p| {
        d.child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .w(relative(p.clamp(0.0, 1.0)))
                .rounded(px(5.))
                .bg(color),
        )
    })
}

/// A running job's detail as a button label, with its percent:
/// "Downloading 42% · 311 MB of 740 MB · 13.5 MB/s" (the file name and time
/// left dropped, for room), or "Updating mods 83% · 40/48".
pub fn job_label(detail: &str, p: Option<f32>) -> String {
    let pct = p.map(|p| format!(" {}%", (p.clamp(0.0, 1.0) * 100.0).floor() as u32)).unwrap_or_default();
    // "Downloading <file>: <size> · <speed> · about N s left"
    if let Some(rest) = detail.strip_prefix("Downloading ").and_then(|d| d.split_once(": ").map(|(_, r)| r)) {
        let parts: Vec<&str> = rest.split(" · ").filter(|s| !s.starts_with("about ")).collect();
        return format!("Downloading{pct} · {}", parts.join(" · "));
    }
    // "Updating mods (40/48)"
    if let Some((verb, count)) = detail.strip_suffix(')').and_then(|d| d.rsplit_once(" (")) {
        return format!("{verb}{pct} · {count}");
    }
    format!("{detail}{pct}")
}

/// A finished action's mark where its button was: a check and a word.
pub fn done_mark(id: impl Into<ElementId>, label: impl Into<SharedString>, small: bool, cx: &App) -> Stateful<Div> {
    let ok = cx.theme().success;
    h_flex()
        .id(id)
        .flex_none()
        .map(|d| if small { d.h(rems(1.375)).px_2().text_xs() } else { d.h(rems(1.75)).px_3().text_sm() })
        .gap_1p5()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(ok.opacity(0.5))
        .bg(ok.opacity(0.14))
        .text_color(ok)
        .font_medium()
        .child(Icon::new(IconName::Check).small())
        .child(label.into())
}

/// `el` fading out (and nudged right) over [`FADE`]: a done row leaving.
pub fn fade_out(id: impl Into<ElementId>, el: impl IntoElement) -> AnyElement {
    div()
        .relative()
        .child(el)
        .with_animation(id, Animation::new(FADE).with_easing(ease_in_out), |d, t| {
            d.opacity(1.0 - t).left(rems(1.0 * t))
        })
        .into_any_element()
}

/// A mod's icon and name, the way every list shows a mod. Callers add the
/// click that opens its page (`.on_click`) where it has one.
pub fn mod_name(id: impl Into<ElementId>, name: impl Into<SharedString>, icon: Option<&Path>, cx: &App) -> Stateful<Div> {
    h_flex()
        .id(id)
        .min_w_0()
        .gap_1p5()
        .child(thumb(icon, rems(1.25), cx))
        .child(div().min_w_0().truncate().child(name.into()))
}

/// [`mod_name`] that opens the mod's page on click (when `project` isn't 0).
pub fn mod_link(
    id: impl Into<ElementId>,
    name: impl Into<SharedString>,
    icon: Option<&Path>,
    project: u64,
    open: impl Fn(u64, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let accent = cx.theme().primary;
    mod_name(id, name, icon, cx).when(project != 0, |d| {
        d.clickable(cx)
            .hover(move |s| s.text_color(accent))
            .on_click(move |_, window, cx| open(project, window, cx))
    })
}

/// What a [`DescView`] shows.
enum Shown {
    /// A project, its description and the description's images.
    Desc(u64, Rc<Vec<hopper::richtext::Segment>>, Rc<Vec<String>>),
    /// The description of the modpack with this project id, from the
    /// store's pack pages (see [`pack_page`]).
    Pack(u64),
}

/// A project description (see [`description_view`]) whose GIFs play:
/// it redraws itself when one changes frame, only while it is drawn in a
/// focused window. GPUI's own GIFs redraw every display frame; these, at
/// the GIF's pace, and without redrawing the page around them.
pub struct DescView {
    store: Entity<crate::store::Store>,
    shown: Option<Shown>,
    /// A redraw is set for the next GIF frame.
    anim_due: bool,
    _focus: Subscription,
    /// Redraws when a translation of the description finishes.
    _translated: Subscription,
}

impl DescView {
    pub fn new(store: Entity<crate::store::Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let tick = store.read(cx).tr_tick().clone();
        Self {
            store,
            shown: None,
            anim_due: false,
            // Paused GIFs pick up again when the window is focused.
            _focus: cx.observe_window_activation(window, |_, _, cx| cx.notify()),
            _translated: cx.observe(&tick, |_, _, cx| cx.notify()),
        }
    }

    /// Sets what to show; called as the page draws, so the view draws it
    /// straight after without being told.
    pub fn show(&mut self, id: u64, segments: Rc<Vec<hopper::richtext::Segment>>, images: Rc<Vec<String>>) {
        self.shown = Some(Shown::Desc(id, segments, images));
    }

    /// Shows the page of modpack `id` once the store has it (see
    /// `Store::want_pack_page`); like [`Self::show`], called as the page draws.
    pub fn show_pack(&mut self, id: u64) {
        self.shown = Some(Shown::Pack(id));
    }

    fn anim_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Frames replaced last draw are off screen now.
        // (This window's: `cx.drop_image` would reach into it mid-draw.)
        for f in self.store.read(cx).take_shown_frames() {
            window.drop_image(f).ok();
        }
        if self.anim_due || !window.is_window_active() {
            return;
        }
        let store = self.store.read(cx);
        let images: &[String] = match &self.shown {
            Some(Shown::Desc(_, _, images)) => images,
            Some(Shown::Pack(id)) => match store.pack_page(*id) {
                Some(crate::store::PackPage::Ready(_, _, images)) => images,
                _ => return,
            },
            None => return,
        };
        let Some(wait) = description_wait(images, store) else {
            return;
        };
        // At most 25 redraws a second, however many GIFs are out of step.
        let wait = wait.max(Duration::from_millis(40));
        self.anim_due = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            this.update(cx, |this, cx| {
                this.anim_due = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for DescView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.anim_tick(window, cx);
        match &self.shown {
            Some(Shown::Desc(id, segments, images)) => description_view(
                ElementId::Name(format!("desc:{id}").into()),
                segments,
                images,
                &self.store,
                cx,
            ),
            Some(Shown::Pack(id)) => match self.store.read(cx).pack_page(*id) {
                Some(crate::store::PackPage::Ready(m, md, images)) => description_view(
                    ElementId::Name(format!("pack-desc:{}", m.id).into()),
                    md,
                    images,
                    &self.store,
                    cx,
                ),
                _ => div().into_any_element(),
            },
            None => div().into_any_element(),
        }
    }
}

// ---- resizable side panels -------------------------------------------------

/// Which edge of a side panel its drag handle sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelEdge {
    /// A panel on the leading side: the handle is on its right.
    Right,
    /// A panel on the trailing side: the handle is on its left.
    Left,
}

/// A resizable side panel: where its width is saved and its limits, in rem.
#[derive(Clone, Copy, Debug)]
pub struct PanelSpec {
    /// Key in `Settings::panel_widths`.
    pub key: &'static str,
    pub default: f32,
    pub min: f32,
    pub max: f32,
    /// Let go narrower than `.0`, it collapses to a rail `.1` wide.
    pub rail: Option<(f32, f32)>,
    pub edge: PanelEdge,
}

impl PanelSpec {
    pub const fn new(key: &'static str, default: f32, min: f32, max: f32) -> Self {
        Self { key, default, min, max, rail: None, edge: PanelEdge::Right }
    }

    /// Collapses to a rail `width` wide when let go narrower than `below`.
    pub const fn rail(mut self, below: f32, width: f32) -> Self {
        self.rail = Some((below, width));
        self
    }

    /// Docked on the trailing side, its handle on the left.
    #[allow(dead_code)]
    pub const fn trailing(mut self) -> Self {
        self.edge = PanelEdge::Left;
        self
    }
}

/// The most of the window an open side panel takes, so the page beside it
/// keeps room at the 960px minimum.
const PANEL_MAX_SHARE: f32 = 0.4;

/// Dragging a side panel's edge; carries the panel's key.
pub struct PanelDrag(pub &'static str);

/// What a panel drag shows under the pointer: nothing.
struct PanelGhost;

impl Render for PanelGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// A side panel's width right now, in rem.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanelLayout {
    pub width: f32,
    pub collapsed: bool,
}

/// A resizable side panel's live state, kept in the view that draws it
/// with [`side_panel`]. The width is saved per key in the settings; a
/// negative value means collapsed, remembering the open width.
pub struct SidePanel {
    pub spec: PanelSpec,
    /// The width while the edge is dragged.
    drag: Option<f32>,
}

impl SidePanel {
    pub const fn new(spec: PanelSpec) -> Self {
        Self { spec, drag: None }
    }

    /// Saved (collapsed, open width).
    fn saved(&self, s: &hopper::settings::Settings) -> (bool, f32) {
        let v = s.panel_widths.get(self.spec.key).copied().unwrap_or(self.spec.default);
        (v < 0. && self.spec.rail.is_some(), v.abs().clamp(self.spec.min, self.spec.max))
    }

    /// The width to draw: the live drag, else the saved one, kept within
    /// the limits and the window.
    pub fn layout(&self, s: &hopper::settings::Settings, window: &Window) -> PanelLayout {
        let spec = &self.spec;
        let (collapsed, w) = match self.drag {
            Some(w) if spec.rail.is_some_and(|(below, _)| w < below) => (true, w),
            Some(w) => (false, w),
            None => self.saved(s),
        };
        if let (true, Some((_, rail))) = (collapsed, spec.rail) {
            return PanelLayout { width: rail, collapsed: true };
        }
        let window_w = window.viewport_size().width / window.rem_size();
        let cap = (window_w * PANEL_MAX_SHARE).max(spec.min);
        PanelLayout { width: w.clamp(spec.min, spec.max).min(cap), collapsed: false }
    }

    /// The edge is being dragged.
    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Collapses to the rail, or opens again at the width it had.
    pub fn set_collapsed(&self, collapsed: bool, store: &Entity<crate::store::Store>, cx: &mut App) {
        let (spec, open) = (self.spec, self.saved(store.read(cx).settings()).1);
        save_panel(spec, collapsed, open, store, cx);
    }

    /// The edge was let go: collapse below the threshold, else keep the width.
    fn end_drag(&mut self, store: &Entity<crate::store::Store>, cx: &mut App) {
        let Some(w) = self.drag.take() else { return };
        let spec = self.spec;
        match spec.rail {
            Some((below, _)) if w < below => {
                let open = self.saved(store.read(cx).settings()).1;
                save_panel(spec, true, open, store, cx)
            }
            _ => save_panel(spec, false, w.clamp(spec.min, spec.max), store, cx),
        }
    }
}

fn save_panel(spec: PanelSpec, collapsed: bool, open: f32, store: &Entity<crate::store::Store>, cx: &mut App) {
    let v = if collapsed { -open } else { open };
    store.update(cx, |s, cx| {
        s.update_settings(
            |st| {
                // The default width needs no entry.
                if v == spec.default {
                    st.panel_widths.remove(spec.key);
                } else {
                    st.panel_widths.insert(spec.key.to_string(), v);
                }
            },
            cx,
        )
    });
}

/// A side panel `panel.layout(..)` wide holding `content`, with the shared
/// handle on its edge: a thin bar that lights up on hover and while
/// dragged. Drag between the limits (below the rail threshold it
/// collapses); double-click resets. `lens` finds the panel in the view.
/// Add the background and border to what this returns; pass the rail as
/// `content` when the layout says collapsed.
pub fn side_panel<V: 'static>(
    id: impl Into<ElementId>,
    panel: &SidePanel,
    lens: fn(&mut V) -> &mut SidePanel,
    store: &Entity<crate::store::Store>,
    content: impl IntoElement,
    window: &Window,
    cx: &mut Context<V>,
) -> Div {
    let spec = panel.spec;
    let layout = panel.layout(store.read(cx).settings(), window);
    let (bar, active) = (cx.theme().primary, panel.dragging());
    let left = spec.edge == PanelEdge::Left;
    // Moves are seen wherever the pointer is; the width is measured from
    // the panel's fixed edge.
    let drag_move = cx.listener(move |this: &mut V, e: &DragMoveEvent<PanelDrag>, window, cx| {
        if e.drag(cx).0 != spec.key {
            return;
        }
        let x = e.event.position.x;
        let w = if left { e.bounds.right() - x } else { x - e.bounds.left() };
        lens(this).drag = Some(w / window.rem_size());
        cx.notify();
    });
    // Let go anywhere, over the panel or not.
    let end = {
        let store = store.clone();
        move |this: &mut V, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<V>| {
            if lens(this).dragging() {
                lens(this).end_drag(&store, cx);
                cx.notify();
            }
        }
    };
    let reset = {
        let store = store.clone();
        cx.listener(move |this: &mut V, e: &ClickEvent, _, cx| {
            if e.click_count() == 2 {
                lens(this).drag = None;
                save_panel(spec, false, spec.default, &store, cx);
                cx.notify();
            }
        })
    };
    div()
        .relative()
        .flex_none()
        .h_full()
        .w(rems(layout.width))
        .on_drag_move(drag_move)
        .capture_any_mouse_up(cx.listener(end.clone()))
        .on_mouse_up_out(MouseButton::Left, cx.listener(end))
        .child(div().size_full().overflow_hidden().child(content))
        .child(
            div()
                .id(id)
                .absolute()
                .top_0()
                .bottom_0()
                .map(|d| if left { d.left(rems(-0.25)) } else { d.right(rems(-0.25)) })
                .w(rems(0.5))
                .cursor_col_resize()
                .when(active, |d| d.bg(bar.opacity(0.5)))
                .hover(move |s| s.bg(bar.opacity(0.35)))
                .on_click(reset)
                .on_drag(PanelDrag(spec.key), |_, _, _, cx| cx.new(|_| PanelGhost)),
        )
}

// ---- anchored popovers -------------------------------------------------------

/// Kept between a popover and the window's edge.
const POPOVER_MARGIN: f32 = 8.;
/// Between the control and its popover.
const POPOVER_GAP: f32 = 6.;

/// Where a popover goes beside the control that opened it, in window
/// coordinates: under a control in the top half (above one lower down),
/// right edges lined up for a control on the right half (left edges
/// otherwise), always 8px inside the window.
#[derive(Clone, Copy, Debug)]
pub struct Anchored {
    pub left: Option<Pixels>,
    pub right: Option<Pixels>,
    pub top: Option<Pixels>,
    pub bottom: Option<Pixels>,
    /// The widest and tallest it can be and stay in the window.
    pub max_w: Pixels,
    pub max_h: Pixels,
}

/// The place for a popover opened by the control at `anchor` (window
/// coordinates, e.g. from a `canvas` in the control). Draw the popover
/// absolute in a window-sized root at the origin and [`Anchored::apply`] it.
pub fn anchored_popover(anchor: Bounds<Pixels>, window: &Window) -> Anchored {
    let view = window.viewport_size();
    let (m, gap) = (px(POPOVER_MARGIN), px(POPOVER_GAP));
    let (left, right, max_w) = if anchor.center().x > view.width / 2. {
        let r = (view.width - anchor.right()).max(m);
        (None, Some(r), view.width - r - m)
    } else {
        let l = anchor.left().max(m);
        (Some(l), None, view.width - l - m)
    };
    let (top, bottom, max_h) = if anchor.center().y <= view.height / 2. {
        let t = anchor.bottom() + gap;
        (Some(t), None, view.height - t - m)
    } else {
        let b = view.height - anchor.top() + gap;
        (None, Some(b), view.height - b - m)
    };
    Anchored { left, right, top, bottom, max_w: max_w.max(px(0.)), max_h: max_h.max(px(0.)) }
}

impl Anchored {
    /// Positions `el` and caps it to the window: its width follows the
    /// content between `min_w` and `max_w` rem, never past the window.
    pub fn apply<E: Styled>(&self, el: E, min_w: f32, max_w: f32, window: &Window) -> E {
        let rem = window.rem_size();
        let max = (rem * max_w).min(self.max_w);
        let min = (rem * min_w).min(max);
        let mut el = el.absolute().min_w(min).max_w(max).max_h(self.max_h);
        if let Some(l) = self.left {
            el = el.left(l);
        }
        if let Some(r) = self.right {
            el = el.right(r);
        }
        if let Some(t) = self.top {
            el = el.top(t);
        }
        if let Some(b) = self.bottom {
            el = el.bottom(b);
        }
        el
    }
}

#[cfg(test)]
mod tests {
    use super::job_label;

    #[test]
    fn job_label_shortens_job_details() {
        assert_eq!(
            job_label("Downloading LL8 Server Files v1.78.1.zip: 311.4 MB of 740.2 MB · 13.5 MB/s · about 32 s left", Some(0.42)),
            "Downloading 42% · 311.4 MB of 740.2 MB · 13.5 MB/s"
        );
        assert_eq!(job_label("Updating mods (40/48)", Some(0.833)), "Updating mods 83% · 40/48");
        assert_eq!(job_label("Comparing files", None), "Comparing files");
    }
}
