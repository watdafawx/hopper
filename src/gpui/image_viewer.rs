//! A gallery viewer inside Hopper: a project's images, full size, with the
//! previous and next ones (buttons or the arrow keys), instead of the
//! browser.

use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::*;

/// An image the viewer shows: the full-size one, a small copy to show
/// while it loads, and its title.
#[derive(Clone, Default)]
pub struct Shot {
    pub url: String,
    pub thumb: Option<String>,
    pub title: String,
}

pub struct ImageViewer {
    store: Entity<Store>,
    images: Vec<Shot>,
    at: usize,
    /// The widest a still image is decoded, in pixels: the screen's width,
    /// so a huge upload doesn't hold hundreds of MB.
    cap: u32,
    /// Touchpad scroll not yet turned into a step.
    wheel: f32,
    focus: FocusHandle,
    /// A redraw for an animated image's next frame is on its way.
    anim_due: bool,
    /// The big clip of the image shown (`store::view_key`), freed when the
    /// viewer moves on or closes.
    playing: Option<String>,
    _store: Subscription,
    /// A paused GIF picks up again when the window is focused.
    _focus: Subscription,
}

impl ImageViewer {
    fn show(&mut self, at: usize, cx: &mut Context<Self>) {
        let n = self.images.len();
        if n == 0 {
            return;
        }
        self.at = at % n;
        // This one, and its neighbours ready for the arrows.
        let want: Vec<&Shot> = [self.at, (self.at + 1) % n, (self.at + n - 1) % n]
            .iter()
            .map(|&i| &self.images[i])
            .collect();
        // The one shown plays if it's an animated GIF or WebP, decoded big;
        // the one before is let go. The neighbours just load. Its small
        // copy shows while it does.
        let url = want[0].url.clone();
        let key = crate::store::view_key(&url);
        let old = self.playing.replace(key.clone()).filter(|k| *k != key);
        let stills: Vec<String> = want.iter().map(|s| still_key(&s.url, self.cap)).collect();
        let thumbs: Vec<String> = want.iter().filter_map(|s| s.thumb.clone()).collect();
        self.store.update(cx, |s, cx| {
            if let Some(old) = old {
                s.forget_anim(&old, cx);
            }
            if animated(&url) {
                s.want_anim_view(&url);
            }
            for u in stills.iter().chain(&thumbs) {
                s.want_image(u);
            }
        });
        cx.notify();
    }

    /// Redraws when the animated image shown has its next frame due, and
    /// frees the GPU copies of frames already shown.
    fn anim_tick(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        for f in self.store.read(cx).take_shown_frames() {
            window.drop_image(f).ok();
        }
        if self.anim_due || !window.is_window_active() {
            return;
        }
        let key = crate::store::view_key(url);
        let Some(wait) = self.store.read(cx).anim(&key).map(|c| c.wait_now()) else {
            return;
        };
        // At most 25 redraws a second.
        let wait = wait.max(std::time::Duration::from_millis(40));
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

    /// The wheel moves like the arrows: down is next, up is previous. A
    /// mouse notch is one step; a touchpad steps every so many pixels.
    fn on_wheel(&mut self, e: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let n = self.images.len();
        if n < 2 {
            return;
        }
        let dy = match e.delta {
            ScrollDelta::Lines(l) => {
                self.wheel = 0.;
                if l.y == 0. {
                    return;
                }
                l.y
            }
            ScrollDelta::Pixels(p) => {
                self.wheel += f32::from(p.y);
                if self.wheel.abs() < 60. {
                    return;
                }
                std::mem::take(&mut self.wheel)
            }
        };
        // Positive is scrolling up.
        if dy > 0. {
            self.show(self.at + n - 1, cx);
        } else {
            self.show(self.at + 1, cx);
        }
    }
}

impl Render for ImageViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let n = self.images.len();
        let Shot { url, thumb, title } = self.images.get(self.at).cloned().unwrap_or_default();
        self.anim_tick(&url, window, cx);
        let store = self.store.read(cx);
        let frame = store.anim(&crate::store::view_key(&url)).map(|c| c.frame_now());
        let path = store.image(&still_key(&url, self.cap)).map(|p| p.to_path_buf());
        let small = thumb.as_deref().and_then(|t| store.image(t)).map(|p| p.to_path_buf());
        let h = (window.viewport_size().height - window.rem_size() * 12.).max(window.rem_size() * 12.);
        // Room for the picture: the dialog's width less its padding.
        let rem = f32::from(window.rem_size());
        let room_w = (f32::from(window.viewport_size().width) - rem * 6.).min(rem * 80.) - rem * 2.;
        let room_h = f32::from(h);
        // Whole image inside the frame, never cropped. Small pictures (pixel
        // art) grow by whole steps only, up to a sensible size; big ones
        // shrink to fit.
        let fit = |w: f32, h: f32| -> (Pixels, Pixels) {
            let s = (room_w / w).min(room_h / h);
            let s = if s >= 1. { s.floor().min(4.).min((room_h * 0.6 / h).floor().max(1.)).max(1.) } else { s };
            (px(w * s), px(h * s))
        };
        let placeholder = small.and_then(|p| Some((p.clone(), widgets::image_size(&p)?)));
        let picture: AnyElement = match (frame, path, placeholder) {
            (Some(f), _, _) => {
                let sz = f.size(0);
                let (w, h) = fit(sz.width.0.max(1) as f32, sz.height.0.max(1) as f32);
                h_flex().size_full().justify_center().items_center().child(img(ImageSource::Render(f)).w(w).h(h).object_fit(ObjectFit::Contain)).into_any_element()
            }
            (None, Some(p), _) => match widgets::image_size(&p) {
                Some((iw, ih)) if iw > 0 && ih > 0 => {
                    let (w, h) = fit(iw as f32, ih as f32);
                    h_flex().size_full().justify_center().items_center().child(img(p).w(w).h(h).object_fit(ObjectFit::Contain)).into_any_element()
                }
                _ => img(p).size_full().object_fit(ObjectFit::Contain).into_any_element(),
            },
            // The full image is on its way: its small copy, as big as the
            // full one will show, with a spinner over it.
            (None, None, Some((p, (iw, ih)))) => {
                let s = (room_w / iw.max(1) as f32).min(room_h / ih.max(1) as f32);
                h_flex()
                    .size_full()
                    .justify_center()
                    .items_center()
                    .relative()
                    .child(img(p).w(px(iw as f32 * s)).h(px(ih as f32 * s)).object_fit(ObjectFit::Contain))
                    .child(
                        div()
                            .absolute()
                            .bottom_3()
                            .right_3()
                            .p_1p5()
                            .rounded(theme.radius)
                            .bg(theme.background.opacity(0.7))
                            .child(Spinner::new().small()),
                    )
                    .into_any_element()
            }
            (None, None, None) => h_flex()
                .size_full()
                .justify_center()
                .gap_2()
                .child(Spinner::new().small())
                .child(widgets::caption("Loading the image", cx))
                .into_any_element(),
        };
        let u2 = url.clone();
        v_flex()
            .id("image-viewer")
            .track_focus(&self.focus)
            .gap_3()
            .on_key_down(cx.listener(move |this, e: &KeyDownEvent, _, cx| match e.keystroke.key.as_str() {
                "left" => this.show(this.at + n.max(1) - 1, cx),
                "right" => this.show(this.at + 1, cx),
                _ => {}
            }))
            .child(
                div()
                    .h(h)
                    .w_full()
                    .rounded(theme.radius_lg)
                    .bg(theme.muted)
                    .overflow_hidden()
                    .relative()
                    // One image per notch: keep the window's smoother off it.
                    .child(crate::smooth_scroll::stepped_area())
                    .on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, _, cx| this.on_wheel(e, cx)))
                    .child(picture),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("img-prev")
                            .small()
                            .ghost()
                            .icon(IconName::ChevronLeft)
                            .disabled(n < 2)
                            .on_click(cx.listener(move |this, _, _, cx| this.show(this.at + n.max(1) - 1, cx))),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().font_semibold().truncate().child(if title.is_empty() {
                                "Image".to_string()
                            } else {
                                title
                            }))
                            .child(widgets::caption(format!("{} of {n} · ← → or scroll to move", self.at + 1), cx).text_xs()),
                    )
                    .child(
                        Button::new("img-browser")
                            .small()
                            .ghost()
                            .icon(IconName::ExternalLink)
                            .tooltip("Open in the browser")
                            .on_click(move |_, _, cx| cx.open_url(&u2)),
                    )
                    .child(
                        Button::new("img-next")
                            .small()
                            .ghost()
                            .icon(IconName::ChevronRight)
                            .disabled(n < 2)
                            .on_click(cx.listener(move |this, _, _, cx| this.show(this.at + 1, cx))),
                    ),
            )
    }
}

/// Whether `url` may be animated (a GIF or WebP): it plays from the full
/// file, decoded by the store.
fn animated(url: &str) -> bool {
    hopper::imgcache::path_for(url).is_some_and(|p| hopper::imgcache::maybe_animated(&p))
}

/// The store key of a still image's copy no wider than `cap` pixels.
fn still_key(url: &str, cap: u32) -> String {
    hopper::imgcache::width_key(url, (!animated(url)).then_some(cap))
}

/// Opens `images` at `at`.
pub fn open(store: Entity<Store>, images: Vec<Shot>, at: usize, window: &mut Window, cx: &mut App) {
    // The screen's width in pixels, in steps so a resized window reuses the
    // copy already made.
    let wide = f32::from(window.viewport_size().width) * window.scale_factor();
    let cap = ((wide / 512.).ceil() as u32).max(2) * 512;
    let view = cx.new(|cx| {
        let sub = cx.observe(&store, |_, _, cx| cx.notify());
        let refocus = cx.observe_window_activation(window, |_, _, cx| cx.notify());
        // Closed: its big clip goes.
        cx.on_release(|this: &mut ImageViewer, cx| {
            if let Some(key) = this.playing.take() {
                this.store.update(cx, |s, cx| s.forget_anim(&key, cx));
            }
        })
        .detach();
        let mut v = ImageViewer {
            store,
            images,
            at: 0,
            cap,
            wheel: 0.,
            focus: cx.focus_handle(),
            anim_due: false,
            playing: None,
            _store: sub,
            _focus: refocus,
        };
        v.show(at, cx);
        v
    });
    let focus = view.read(cx).focus.clone();
    window.open_dialog(cx, move |dialog, window, _| {
        dialog
            .w((window.viewport_size().width - window.rem_size() * 6.).min(window.rem_size() * 80.))
            .child(view.clone())
    });
    window.focus(&focus, cx);
}
