//! The appearance button in the title bar, beside the settings gear: a
//! popover with the themes, Zoom, Still header and Smooth scrolling, so
//! none of the four needs the Settings page. Pointing at a theme (or
//! arrowing to it) shows it on the whole app after a short pause; a click
//! keeps it, and leaving, Esc or clicking outside puts the saved one back.

use crate::app::AppView;
use crate::store::Store;
use crate::switch::Switch;
use crate::widgets;
use crate::widgets::Clickable as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Icon, Selectable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

gpui_kit::actions!(appearance, [Up, Down, Left, Right, Pick, Dismiss]);

const CONTEXT: &str = "Appearance";

/// How long the pointer rests on a theme before the app shows it. Long
/// enough that sweeping across the list doesn't repaint everything.
const HOVER_DELAY: Duration = Duration::from_millis(150);

/// The popover's own keys, while it is open.
pub fn bind_keys(cx: &mut App) {
    let ctx = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("up", Up, ctx),
        KeyBinding::new("down", Down, ctx),
        KeyBinding::new("left", Left, ctx),
        KeyBinding::new("right", Right, ctx),
        KeyBinding::new("enter", Pick, ctx),
        KeyBinding::new("space", Pick, ctx),
        KeyBinding::new("escape", Dismiss, ctx),
    ]);
}

/// The saved theme's name ("" follows Windows).
pub fn current_name(s: &hopper::settings::Settings) -> String {
    if s.theme_name.is_empty() && s.theme == 1 {
        crate::theme::HOPPER_DARK.to_string()
    } else if s.theme_name.is_empty() && s.theme == 2 {
        crate::theme::HOPPER_LIGHT.to_string()
    } else {
        s.theme_name.clone()
    }
}

pub struct AppearanceButton {
    store: Entity<Store>,
    app: WeakEntity<AppView>,
    pub open: bool,
    /// Where the button was last painted, in window coordinates.
    pub bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The theme the pointer or the arrow keys rest on; None = the saved one.
    want: Option<String>,
    /// The theme on screen right now when it isn't the saved one.
    shown: Option<String>,
    /// Bumped on every change of `want`, so a stale timer does nothing.
    generation: u64,
    /// The arrow keys' place in the list.
    cursor: usize,
    focus: FocusHandle,
}

impl AppearanceButton {
    pub fn new(store: Entity<Store>, app: WeakEntity<AppView>, cx: &mut Context<Self>) -> Self {
        Self {
            store,
            app,
            open: false,
            bounds: Rc::new(Cell::new(None)),
            want: None,
            shown: None,
            generation: 0,
            cursor: 0,
            focus: cx.focus_handle(),
        }
    }

    fn names() -> Vec<String> {
        std::iter::once(String::new()).chain(crate::theme::names()).collect()
    }

    pub fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open {
            self.close(cx);
            return;
        }
        let current = current_name(self.store.read(cx).settings());
        self.cursor = Self::names().iter().position(|n| *n == current).unwrap_or(0);
        self.open = true;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Closes and puts the saved theme back if one was being previewed.
    fn close(&mut self, cx: &mut Context<Self>) {
        self.want = None;
        self.generation += 1;
        self.sync(cx);
        self.open = false;
        cx.notify();
    }

    /// Puts on screen what `want` says (the saved theme when it is None).
    fn sync(&mut self, cx: &mut Context<Self>) {
        if self.want == self.shown {
            return;
        }
        let s = self.store.read(cx).settings();
        let name = self.want.clone().unwrap_or_else(|| current_name(s));
        let zoom = s.zoom;
        crate::theme::apply(&name, 0, zoom, cx);
        self.shown = self.want.clone();
        cx.notify();
    }

    /// What the pointer rests on changed: show it once it has rested.
    fn set_want(&mut self, want: Option<String>, cx: &mut Context<Self>) {
        if self.want == want {
            return;
        }
        self.want = want;
        self.generation += 1;
        let g = self.generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HOVER_DELAY).await;
            let _ = this.update(cx, |t, cx| {
                if t.generation == g {
                    t.sync(cx);
                }
            });
        })
        .detach();
    }

    fn commit(&mut self, name: String, cx: &mut Context<Self>) {
        self.generation += 1;
        self.store.update(cx, |s, cx| {
            s.update_settings(
                |st| {
                    st.theme_name = name.clone();
                    st.theme = 0;
                },
                cx,
            )
        });
        let zoom = self.store.read(cx).settings().zoom;
        crate::theme::apply(&name, 0, zoom, cx);
        self.want = None;
        self.shown = None;
        cx.notify();
    }

    /// Moves the arrow keys' place by `by` and previews what is there.
    fn step(&mut self, by: isize, cx: &mut Context<Self>) {
        let names = Self::names();
        let last = names.len() as isize - 1;
        self.cursor = (self.cursor as isize + by).clamp(0, last) as usize;
        self.set_want(Some(names[self.cursor].clone()), cx);
        cx.notify();
    }

    /// The popover, drawn over the whole window (a window-sized root at
    /// the origin).
    pub fn overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.open {
            return None;
        }
        let b = self.bounds.get()?;
        let theme = cx.theme().clone();
        let s = self.store.read(cx).settings().clone();
        let current = current_name(&s);
        let place = widgets::anchored_popover(b, window);
        let rem = window.rem_size();
        let names = Self::names();
        let this = cx.entity();
        let bounds = self.bounds.clone();

        let chips = names.iter().enumerate().map(|(ix, name)| {
            let saved = *name == current;
            let at_cursor = ix == self.cursor && self.want.is_some();
            let (n_hover, n_click) = (name.clone(), name.clone());
            div()
                .w_1_2()
                .p_0p5()
                .child(
                    h_flex()
                        .id(("theme-chip", ix))
                        .gap_1()
                        .px_2()
                        .py_1()
                        .rounded(theme.radius)
                        .text_sm()
                        .clickable(cx)
                        .hover(|st| widgets::row_hover(st, cx))
                        .when(at_cursor, |r| widgets::row_hover(r, cx))
                        .when(saved, |r| widgets::row_selected(r, cx))
                        .child(div().flex_1().min_w_0().overflow_hidden().child(crate::settings_page::theme_row(name, cx)))
                        .when(saved, |r| r.child(Icon::new(IconName::Check).xsmall().text_color(theme.primary)))
                        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                            if *hovered {
                                this.set_want(Some(n_hover.clone()), cx);
                            } else if this.want.as_deref() == Some(n_hover.as_str()) {
                                this.set_want(None, cx);
                            }
                        }))
                        .on_click(cx.listener(move |this, _, _, cx| this.commit(n_click.clone(), cx))),
                )
        });

        let zoom = s.zoom;
        let zoom_btn = |id: &'static str, icon: IconName, tip: &'static str, act: fn() -> Box<dyn Action>| {
            Button::new(id).ghost().xsmall().icon(icon).tooltip(tip).on_click(move |_, window, cx| {
                window.dispatch_action(act(), cx);
            })
        };
        let switch_row = |id: &'static str, label: &'static str, on: bool, set: fn(&mut hopper::settings::Settings, bool)| {
            let store = self.store.clone();
            h_flex().justify_between().gap_3().child(div().text_sm().child(label)).child(
                Switch::new(id).checked(on).on_click(move |v: &bool, _, cx| {
                    let v = *v;
                    store.update(cx, |s, cx| s.update_settings(|st| set(st, v), cx));
                }),
            )
        };
        let app = self.app.clone();
        let panel = v_flex()
            .id("appearance-popover")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &Up, _, cx| this.step(-2, cx)))
            .on_action(cx.listener(|this, _: &Down, _, cx| this.step(2, cx)))
            .on_action(cx.listener(|this, _: &Left, _, cx| this.step(-1, cx)))
            .on_action(cx.listener(|this, _: &Right, _, cx| this.step(1, cx)))
            .on_action(cx.listener(|this, _: &Pick, _, cx| {
                let names = Self::names();
                if let Some(n) = names.get(this.cursor) {
                    this.commit(n.clone(), cx);
                }
            }))
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.close(cx)))
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius_lg)
            .shadow_xl()
            .occlude()
            .on_mouse_down_out(cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                // The button toggles it itself.
                if bounds.get().is_some_and(|b| b.contains(&e.position)) {
                    return;
                }
                this.close(cx);
            }))
            .child(
                h_flex()
                    .px_3()
                    .pt_2p5()
                    .pb_1()
                    .gap_1()
                    .child(div().text_sm().font_semibold().child("Theme"))
                    .child(div().flex_1())
                    .child(
                        Button::new("appearance-folder")
                            .ghost()
                            .xsmall()
                            .icon(IconName::FolderOpen)
                            .tooltip("Your own themes: gpui-component theme .json files go here")
                            .on_click(|_, _, _| {
                                if let Some(d) = crate::theme::custom_dir() {
                                    let _ = std::fs::create_dir_all(&d);
                                    crate::store::open_path(&d);
                                }
                            }),
                    )
                    .child(
                        Button::new("appearance-reload")
                            .ghost()
                            .xsmall()
                            .icon(IconName::RefreshCw)
                            .tooltip("Read the theme folder again")
                            .on_click(cx.listener(|_, _, window, cx| {
                                let n = crate::theme::reload_custom();
                                window.push_notification(
                                    gpui_kit::component::notification::Notification::info(format!("{n} themes of your own.")),
                                    cx,
                                );
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .id("appearance-themes")
                    .h((rem * 15.).min(place.max_h - rem * 12.).max(rem * 6.))
                    .px_1p5()
                    .overflow_y_scrollbar()
                    .child(h_flex().flex_wrap().children(chips)),
            )
            .child(
                v_flex()
                    .gap_2()
                    .px_3()
                    .py_2p5()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .justify_between()
                            .child(div().text_sm().child("Zoom"))
                            .child(
                                h_flex()
                                    .gap_0p5()
                                    .items_center()
                                    .child(zoom_btn("zoom-out", IconName::ZoomOut, "Zoom out (Ctrl -)", || Box::new(crate::app::ZoomOut)))
                                    .child(div().w(rems(2.75)).text_center().text_sm().font_semibold().child(format!("{zoom}%")))
                                    .child(zoom_btn("zoom-in", IconName::ZoomIn, "Zoom in (Ctrl +)", || Box::new(crate::app::ZoomIn)))
                                    .child(zoom_btn("zoom-reset", IconName::RotateCcw, "Reset zoom (Ctrl 0)", || Box::new(crate::app::ZoomReset))),
                            ),
                    )
                    .child(switch_row("ap-still", "Still header", s.still_header, |st, v| st.still_header = v))
                    .child(switch_row("ap-smooth", "Smooth scrolling", s.smooth_scroll, |st, v| st.smooth_scroll = v)),
            )
            .child(
                h_flex().px_3().py_1p5().border_t_1().border_color(theme.border).child(
                    Button::new("appearance-all")
                        .link()
                        .xsmall()
                        .label("All appearance settings")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close(cx);
                            app.update(cx, |a, cx| a.open_settings_at("theme", cx)).ok();
                        })),
                ),
            );
        let _ = this;
        // 23-26rem: wide enough for "Catppuccin Macchiato" in a column.
        Some(
            place
                .apply(panel, 23., 26., window)
                .with_animation(
                    "appearance-pop",
                    Animation::new(Duration::from_millis(160)).with_easing(ease_out_quint()),
                    |el, t| el.opacity(t),
                )
                .into_any_element(),
        )
    }
}

impl Render for AppearanceButton {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dialog = window.has_active_dialog(cx);
        let bounds = self.bounds.clone();
        div()
            .relative()
            .child(
                Button::new("open-appearance")
                    .ghost()
                    .small()
                    .icon(IconName::Palette)
                    .selected(self.open)
                    .when(!dialog && !self.open, |b| b.tooltip("Appearance: theme, zoom (Ctrl+Shift+A)"))
                    .on_click(cx.listener(|this, _, window, cx| this.toggle(window, cx))),
            )
            .child(canvas(move |b, _, _| bounds.set(Some(b)), |_, _, _, _| {}).absolute().size_full())
    }
}
