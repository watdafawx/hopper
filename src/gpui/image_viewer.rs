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

pub struct ImageViewer {
    store: Entity<Store>,
    /// (full-size url, title)
    images: Vec<(String, String)>,
    at: usize,
    /// Touchpad scroll not yet turned into a step.
    wheel: f32,
    focus: FocusHandle,
    _store: Subscription,
}

impl ImageViewer {
    fn show(&mut self, at: usize, cx: &mut Context<Self>) {
        let n = self.images.len();
        if n == 0 {
            return;
        }
        self.at = at % n;
        // This one, and its neighbours ready for the arrows.
        let want: Vec<String> = [self.at, (self.at + 1) % n, (self.at + n - 1) % n]
            .iter()
            .map(|&i| self.images[i].0.clone())
            .collect();
        self.store.update(cx, |s, _| {
            for u in &want {
                s.want_image(u);
            }
        });
        cx.notify();
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
        let (url, title) = self.images.get(self.at).cloned().unwrap_or_default();
        let path = self.store.read(cx).image(&url).map(|p| p.to_path_buf());
        let h = (window.viewport_size().height - window.rem_size() * 12.).max(window.rem_size() * 12.);
        let picture: AnyElement = match path {
            Some(p) => img(p).size_full().object_fit(ObjectFit::Contain).into_any_element(),
            None => h_flex()
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

/// Opens `images` (full-size url, title) at `at`.
pub fn open(store: Entity<Store>, images: Vec<(String, String)>, at: usize, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| {
        let sub = cx.observe(&store, |_, _, cx| cx.notify());
        let mut v = ImageViewer { store, images, at: 0, wheel: 0., focus: cx.focus_handle(), _store: sub };
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
