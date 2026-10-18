//! Small presentational pieces shared by the screens.

use gpui_kit::component::button::Button;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, v_flex};
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
            r.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("+{more}")),
            )
        })
        .when(t.modrinth, |r| r.child(tag(Tag::secondary(), "Modrinth".into())))
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
    Button::new(id)
        .outline()
        .small()
        .w_full()
        .label(label)
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
            for (ix, (l, checked)) in items.iter().enumerate() {
                let f = on_pick.clone();
                menu = menu.item(
                    PopupMenuItem::new(l.clone())
                        .checked(*checked)
                        .on_click(move |_, w, cx| f(ix, w, cx)),
                );
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
    // Each draw shows the frame for now; the page redraws on the next one.
    let frames: Arc<std::collections::HashMap<String, Arc<RenderImage>>> = Arc::new(
        images
            .iter()
            .filter_map(|k| Some((k.clone(), s.anim(k)?.frame_now())))
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
                let frames = frames.clone();
                let open = open.clone();
                gpui_kit::base::TextView::markdown(
                    ElementId::Name(format!("{id}:{i}").into()),
                    SharedString::from(md.clone()),
                )
                .scrollable(false)
                .selectable(true)
                .image_source(move |uri| {
                    if let Some(f) = frames.get(uri.as_ref()) {
                        return ImageSource::Render(f.clone());
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
                    let frame = frames.get(&b.key).cloned();
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
/// 16:9 with a play button; a click plays it.
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
        .on_click(move |_, _, cx| crate::video::play(&video, cx))
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
