//! Animated project logos (GIF, animated WebP) in lists and headers.
//!
//! A list of results is a cached view: redrawing it builds every card
//! again, so it can't redraw for each frame of a logo. Instead a logo marks
//! where it sits as the list lays out ([`thumb`] records its bounds and
//! clip in [`Slots`]), and one small view over the page ([`LogoLayer`])
//! paints the frame for "now" on top of each mark that is on screen. Only
//! that view redraws as logos move on: the list underneath is reused.
//!
//! Like description GIFs (`widgets::DescView`), logos play at their own
//! pace, at most 25 redraws a second in all, and only while the window is
//! focused. Logos off screen (scrolled away) aren't marked, so they neither
//! decode nor redraw anything.

use crate::store::Store;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

/// Logos are decoded this wide: sharp at 2x in the 4rem panel header, and
/// small enough that a list's worth of them stays a few MB.
pub const WIDTH: u32 = 128;

/// At most 25 redraws a second, however many logos are out of step.
const MIN_WAIT: Duration = Duration::from_millis(40);

/// How often the layer looks again while a logo it marked is decoding.
const PENDING_WAIT: Duration = Duration::from_millis(250);

/// The image key a logo animates under, when its URL may be animated.
fn key(url: &str) -> Option<String> {
    hopper::imgcache::path_for(url)
        .filter(|p| hopper::imgcache::maybe_animated(p))
        .map(|_| hopper::imgcache::width_key(url, Some(WIDTH)))
}

/// Where a logo was laid out, in window coordinates.
#[derive(Clone)]
struct Slot {
    key: String,
    bounds: Bounds<Pixels>,
    /// The clip it was drawn under (the scrolling list's edges).
    mask: ContentMask<Pixels>,
    bg: Hsla,
    radius: Pixels,
}

#[derive(Default)]
struct Inner {
    /// Each view's marks, from its last draw.
    by_view: HashMap<EntityId, Vec<Slot>>,
    /// The views drawn this frame (see [`Slots::show`]).
    live: Vec<EntityId>,
}

/// The logos a page drew, by the view that drew them. A view's marks are
/// replaced each time it draws; a cached view reused as it was keeps its
/// marks, which are still where it put them.
#[derive(Clone, Default)]
pub struct Slots(Rc<RefCell<Inner>>);

impl Slots {
    /// Forgets view `view`'s marks: called as it draws again.
    pub fn clear(&self, view: EntityId) {
        if let Some(v) = self.0.borrow_mut().by_view.get_mut(&view) {
            v.clear();
        }
    }

    /// The views on screen this frame, whose marks the layer paints. A
    /// cached column that isn't drawn (a closed panel) keeps old marks.
    pub fn show(&self, views: impl IntoIterator<Item = EntityId>) {
        self.0.borrow_mut().live = views.into_iter().collect();
    }

    fn add(&self, view: EntityId, slot: Slot) {
        self.0.borrow_mut().by_view.entry(view).or_default().push(slot);
    }

    fn live(&self) -> Vec<Slot> {
        let inner = self.0.borrow();
        inner
            .live
            .iter()
            .filter_map(|v| inner.by_view.get(v))
            .flatten()
            .cloned()
            .collect()
    }
}

/// A project logo like `widgets::thumb` (`still` is its first frame); when
/// `url` may be animated it also marks itself in `slots`, so the page's
/// [`LogoLayer`] plays it on top.
pub fn thumb(slots: &Slots, url: Option<&str>, still: Option<&Path>, size: Rems, cx: &App) -> AnyElement {
    let thumb = crate::widgets::thumb(still, size, cx);
    let Some(key) = url.and_then(key) else {
        return thumb;
    };
    let theme = cx.theme();
    let (bg, radius, slots) = (theme.muted, theme.radius, slots.clone());
    div()
        .relative()
        .flex_none()
        .size(size)
        .child(thumb)
        .child(
            canvas(
                move |bounds, window, _| {
                    let mask = window.content_mask();
                    // Scrolled out of sight: nothing to play.
                    if mask.bounds.intersects(&bounds) {
                        slots.add(window.current_view(), Slot { key, bounds, mask, bg, radius });
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0(),
        )
        .into_any_element()
}

/// The size `image` takes to cover `bounds`, centered (like
/// `ObjectFit::Cover`).
fn cover(bounds: Bounds<Pixels>, image: Size<DevicePixels>) -> Bounds<Pixels> {
    let (iw, ih) = (image.width.0.max(1) as f32, image.height.0.max(1) as f32);
    let scale = (bounds.size.width / px(iw)).max(bounds.size.height / px(ih));
    let size = size(px(iw * scale), px(ih * scale));
    Bounds::centered_at(bounds.center(), size)
}

/// Plays the animated logos a page marked (see [`thumb`]), drawn over the
/// page. Its redraws leave the page's cached columns alone.
pub struct LogoLayer {
    store: Entity<Store>,
    slots: Slots,
    /// A redraw is set for the next frame change.
    due: Rc<Cell<bool>>,
    _focus: Subscription,
}

impl LogoLayer {
    pub fn new(store: Entity<Store>, slots: Slots, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            store,
            slots,
            due: Rc::default(),
            // Paused logos pick up again when the window is focused.
            _focus: cx.observe_window_activation(window, |_, _, cx| cx.notify()),
        }
    }
}

impl Render for LogoLayer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (store, slots, due) = (self.store.clone(), self.slots.clone(), self.due.clone());
        let view = cx.entity().downgrade();
        // Painted after the page has laid out (and marked) its logos.
        canvas(
            |_, _, _| {},
            move |_, _, window, cx| {
                let mut wait: Option<Duration> = None;
                let mut want = Vec::new();
                for slot in slots.live() {
                    let s = store.read(cx);
                    let Some(clip) = s.anim(&slot.key).cloned() else {
                        if s.anim_known(&slot.key) {
                            if s.anim_pending(&slot.key) {
                                wait = Some(wait.map_or(PENDING_WAIT, |w| w.min(PENDING_WAIT)));
                            }
                        } else {
                            want.push(slot.key);
                        }
                        continue;
                    };
                    let frame = clip.frame_now();
                    let fit = cover(slot.bounds, frame.size(0));
                    window.with_content_mask(Some(slot.mask), |window| {
                        // Over the first frame, which the list drew.
                        window.paint_quad(fill(slot.bounds, slot.bg).corner_radii(slot.radius));
                        window
                            .paint_image(slot.bounds, fit, Corners::all(slot.radius), frame, 0, false)
                            .ok();
                    });
                    let w = clip.wait_now();
                    wait = Some(wait.map_or(w, |o| o.min(w)));
                }
                if !want.is_empty() {
                    let store = store.clone();
                    cx.defer(move |cx| store.update(cx, |s, _| want.iter().for_each(|k| s.want_anim(k))));
                    wait = Some(wait.map_or(PENDING_WAIT, |w| w.min(PENDING_WAIT)));
                }
                // Frames of streamed clips replaced since: off screen now.
                for f in store.read(cx).take_shown_frames() {
                    window.drop_image(f).ok();
                }
                let Some(wait) = wait.filter(|_| window.is_window_active() && !due.get()) else {
                    return;
                };
                due.set(true);
                let due = due.clone();
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(wait.max(MIN_WAIT)).await;
                    due.set(false);
                    view.update(cx, |_, cx| cx.notify()).ok();
                })
                .detach();
            },
        )
        .absolute()
        .inset_0()
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's own `test` attribute would shadow the std one.
    use super::{WIDTH, cover, key};
    use gpui_kit::{Bounds, DevicePixels, point, px, size};

    #[test]
    fn only_gifs_and_webps_animate_and_at_logo_width() {
        let gif = "https://media.forgecdn.net/avatars/thumbnails/1/2/256/256/x_animated.gif";
        assert_eq!(key(gif), Some(format!("{gif}#w={WIDTH}")));
        assert!(key("https://cdn.modrinth.com/data/x/icon.webp").is_some());
        assert_eq!(key("https://media.forgecdn.net/avatars/1/2/x.png"), None);
        assert_eq!(key(""), None);
    }

    #[test]
    fn frames_cover_their_box_centered() {
        let b = Bounds::new(point(px(10.), px(20.)), size(px(56.), px(56.)));
        // Square: fills the box exactly.
        assert_eq!(cover(b, size(DevicePixels(128), DevicePixels(128))), b);
        // Wide: as tall as the box, overflowing both sides equally.
        let w = cover(b, size(DevicePixels(256), DevicePixels(128)));
        assert_eq!(w.size, size(px(112.), px(56.)));
        assert_eq!(w.center(), b.center());
    }
}
