//! Small presentational pieces shared by the screens.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
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

/// A muted, small caption line.
pub fn caption(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .truncate()
        .child(text.into())
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
}

/// Categories shown before the rest fold into "+n".
const MOD_TAG_CATEGORIES: usize = 2;

/// A mod's badges on one line, the same everywhere a mod is listed: new,
/// added, where it runs, its categories and where it came from.
pub fn mod_tags(t: ModTags, cx: &App) -> Div {
    use gpui_kit::component::tag::Tag;
    let tag = |t: Tag, text: SharedString| t.outline().xsmall().flex_none().child(text);
    let env = match t.env {
        "client" => Some(tag(Tag::warning(), "Client only".into())),
        "server" => Some(tag(Tag::info(), "Server only".into())),
        "both" => Some(tag(Tag::secondary(), "Client + server".into())),
        _ => None,
    };
    let more = t.categories.len().saturating_sub(MOD_TAG_CATEGORIES);
    h_flex_row()
        .gap_1()
        .min_w_0()
        .overflow_hidden()
        .when(t.in_use, |r| r.child(tag(Tag::info(), "In use".into())))
        .when(t.new, |r| r.child(tag(Tag::success(), "New".into())))
        .when(t.added, |r| r.child(tag(Tag::warning(), "Added".into())))
        .children(env)
        .children(
            t.categories
                .iter()
                .take(MOD_TAG_CATEGORIES)
                .map(|c| Tag::secondary().xsmall().flex_none().child(c.clone())),
        )
        .when(more > 0, |r| {
            let rest: SharedString = t.categories[MOD_TAG_CATEGORIES..].join(", ").into();
            r.child(
                div()
                    .id(ElementId::Name(format!("more-tags:{}", t.key).into()))
                    .flex_none()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("+{more}"))
                    .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(rest.clone()).build(w, cx)),
            )
        })
        .when(t.modrinth, |r| r.child(site_tag(true)))
}

/// CurseForge's orange or Modrinth's green, wherever a site is named.
pub fn site_color(modrinth: bool) -> Hsla {
    if modrinth {
        hsla(145. / 360., 0.78, 0.48, 1.)
    } else {
        hsla(14. / 360., 0.87, 0.58, 1.)
    }
}

/// A filled button in a site's color: green for Modrinth, else the theme's.
pub fn site_button(b: Button, modrinth: bool) -> Button {
    if modrinth { b.success() } else { b.primary() }
}

/// A small tag naming a site, in its color.
pub fn site_tag(modrinth: bool) -> gpui_kit::component::tag::Tag {
    let c = site_color(modrinth);
    gpui_kit::component::tag::Tag::custom(c.opacity(0.12), c, c.opacity(0.4))
        .xsmall()
        .flex_none()
        .child(if modrinth { "Modrinth" } else { "CurseForge" })
}

fn h_flex_row() -> Div {
    div().flex().flex_row().items_center()
}

/// Whether the theme shows the desktop through the window (glass themes
/// have a see-through page color).
pub fn glass(cx: &App) -> bool {
    cx.theme().background.a < 0.99
}

/// A rounded status pill: tinted fill and border in `color`, with a small
/// glowing dot when `dot`.
pub fn pill(text: impl Into<SharedString>, color: Hsla, dot: bool) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_1p5()
        .h(rems(1.375))
        .px_2p5()
        .rounded_full()
        .border_1()
        .border_color(color.opacity(0.3))
        .bg(color.opacity(0.1))
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
        .bg(theme.foreground.opacity(0.045))
        .border_1()
        .border_color(theme.foreground.opacity(0.08))
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
    id: &'static str,
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
    id: &'static str,
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

/// The page of the modpack an instance came from, as CurseForge or
/// Modrinth shows it: name, author, summary and the full description, with
/// a button to the site. `None` while nothing has been asked for yet.
pub fn pack_page(
    page: Option<&crate::store::PackPage>,
    icon: Option<&Path>,
    store: &Entity<crate::store::Store>,
    cx: &App,
) -> AnyElement {
    use crate::store::PackPage;
    use gpui_kit::assets::IconName;
    use gpui_kit::component::h_flex;
    let theme = cx.theme();
    let (m, md, images) = match page {
        Some(PackPage::Ready(m, md, images)) => (m, md, images),
        Some(PackPage::Failed(e)) => {
            return caption(format!("Couldn't load the pack's page: {e}"), cx).into_any_element();
        }
        _ => return caption("Loading the pack's page…", cx).into_any_element(),
    };
    let site = if m.modrinth.is_some() { "Modrinth" } else { "CurseForge" };
    let url = m.page();
    let downloads = match m.download_count as u64 {
        n if n >= 1_000_000 => format!("{:.1}M downloads", n as f64 / 1e6),
        n if n >= 1_000 => format!("{:.1}K downloads", n as f64 / 1e3),
        n => format!("{n} downloads"),
    };
    v_flex()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
        .bg(theme.foreground.opacity(0.025))
        .child(
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
        )
        .child(div().p_4().child(description_view(
            ElementId::Name(format!("pack-desc:{}", m.id).into()),
            md,
            images,
            store,
            cx,
        )))
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
    v_flex()
        .w_full()
        .children(segments.iter().enumerate().map(|(i, seg)| match seg {
            // GPUI Base's own view: the component wrapper has no image hook.
            Segment::Markdown(md) => {
                let cached = cached.clone();
                let clips = clips.clone();
                let open = open.clone();
                gpui_kit::base::TextView::markdown(
                    ElementId::Name(format!("{id}:{i}").into()),
                    SharedString::from(md.clone()),
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
                        .cursor_pointer()
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
        .cursor_pointer()
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
