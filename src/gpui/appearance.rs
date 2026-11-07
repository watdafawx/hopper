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

/// Chips per row in the popover.
const COLUMNS: usize = 2;

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

// ---- the preview, as plain state ---------------------------------------------

/// Which theme the app shows while the popover is open: the saved one, or
/// one being tried. No timers or painting in here; the button asks it what
/// to do, so the rules can be tested.
#[derive(Default, Debug)]
pub(crate) struct Preview {
    /// What the pointer or arrow keys rest on; None = the saved theme.
    want: Option<String>,
    /// What is painted when it isn't the saved theme.
    shown: Option<String>,
    /// Bumped whenever `want` changes, so a timer from before does nothing.
    generation: u64,
}

impl Preview {
    /// The pointer or keys moved. Returns the generation to settle after the
    /// delay, or None when nothing changed.
    pub fn point(&mut self, want: Option<String>) -> Option<u64> {
        if self.want == want {
            return None;
        }
        self.want = want;
        self.generation += 1;
        Some(self.generation)
    }

    /// The delay for `generation` ran out. Returns what to paint (Some(None)
    /// = the saved theme again), or None when it was stale or already shown.
    pub fn settle(&mut self, generation: u64) -> Option<Option<String>> {
        if generation != self.generation || self.want == self.shown {
            return None;
        }
        self.shown = self.want.clone();
        Some(self.want.clone())
    }

    /// The popover closed (Esc, outside click, the button): what to paint to
    /// undo a preview, or None when none was on screen.
    pub fn close(&mut self) -> Option<Option<String>> {
        self.want = None;
        self.generation += 1;
        self.shown.take().map(|_| None)
    }

    /// A theme was kept: nothing is being tried any more.
    pub fn keep(&mut self) {
        self.want = None;
        self.shown = None;
        self.generation += 1;
    }

    #[cfg(test)]
    pub fn is_previewing(&self) -> bool {
        self.shown.is_some()
    }
}

/// Where an arrow key goes in blocks of chips (`sizes` per block, `COLUMNS`
/// wide), from the flat position `at`. Left and right walk the list; up and
/// down keep the column, step to the neighbouring block at its edge, and land
/// on the last chip of a short row.
pub(crate) fn grid_move(sizes: &[usize], at: usize, dx: isize, dy: isize) -> usize {
    let total: usize = sizes.iter().sum();
    if total == 0 {
        return 0;
    }
    let at = at.min(total - 1);
    if dy == 0 {
        return (at as isize + dx).clamp(0, total as isize - 1) as usize;
    }
    // Which block, and where in it.
    let (mut block, mut start) = (0, 0);
    while at >= start + sizes[block] {
        start += sizes[block];
        block += 1;
    }
    let i = at - start;
    let col = i % COLUMNS;
    if dy < 0 {
        if i >= COLUMNS {
            return start + i - COLUMNS;
        }
        // The row above is the last row of the nearest non-empty block.
        let Some(prev) = (0..block).rev().find(|b| sizes[*b] > 0) else { return at };
        let pstart: usize = sizes[..prev].iter().sum();
        let last_row = (sizes[prev] - 1) / COLUMNS * COLUMNS;
        pstart + (last_row + col).min(sizes[prev] - 1)
    } else {
        let n = sizes[block];
        if i + COLUMNS < n {
            return start + i + COLUMNS;
        }
        // A short last row below: its last chip.
        if i / COLUMNS < (n - 1) / COLUMNS {
            return start + n - 1;
        }
        let Some(next) = (block + 1..sizes.len()).find(|b| sizes[*b] > 0) else { return at };
        let nstart: usize = sizes[..next].iter().sum();
        nstart + col.min(sizes[next] - 1)
    }
}

pub struct AppearanceButton {
    store: Entity<Store>,
    app: WeakEntity<AppView>,
    pub open: bool,
    /// Where the button was last painted, in window coordinates.
    pub bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    preview: Preview,
    /// The arrow keys' place in the flat list.
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
            preview: Preview::default(),
            cursor: 0,
            focus: cx.focus_handle(),
        }
    }

    fn flat() -> Vec<String> {
        crate::theme::sections().into_iter().flat_map(|s| s.names).collect()
    }

    pub fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open {
            self.close(cx);
            return;
        }
        let current = current_name(self.store.read(cx).settings());
        self.cursor = Self::flat().iter().position(|n| *n == current).unwrap_or(0);
        self.open = true;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Closes and puts the saved theme back if one was being previewed.
    fn close(&mut self, cx: &mut Context<Self>) {
        if let Some(paint) = self.preview.close() {
            self.paint(paint, cx);
        }
        self.open = false;
        cx.notify();
    }

    /// Puts a theme on screen (None = the saved one) without saving it.
    fn paint(&mut self, name: Option<String>, cx: &mut Context<Self>) {
        let s = self.store.read(cx).settings();
        let name = name.unwrap_or_else(|| current_name(s));
        let zoom = s.zoom;
        crate::theme::apply(&name, 0, zoom, cx);
        cx.notify();
    }

    /// What the pointer rests on changed: show it once it has rested.
    fn point(&mut self, want: Option<String>, cx: &mut Context<Self>) {
        let Some(g) = self.preview.point(want) else { return };
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HOVER_DELAY).await;
            let _ = this.update(cx, |t, cx| {
                if let Some(paint) = t.preview.settle(g) {
                    t.paint(paint, cx);
                }
            });
        })
        .detach();
    }

    fn commit(&mut self, name: String, cx: &mut Context<Self>) {
        self.preview.keep();
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
        cx.notify();
    }

    /// Moves the arrow keys' place and previews what is there.
    fn step(&mut self, dx: isize, dy: isize, cx: &mut Context<Self>) {
        let sizes: Vec<usize> = crate::theme::sections().iter().map(|s| s.names.len()).collect();
        self.cursor = grid_move(&sizes, self.cursor, dx, dy);
        if let Some(n) = Self::flat().get(self.cursor) {
            self.point(Some(n.clone()), cx);
        }
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
        let cursor = self.cursor;
        let moved = self.preview.want.is_some();

        // The chips, block by block, with a heading over each but the first.
        let mut flat_ix = 0;
        let blocks = crate::theme::sections().into_iter().enumerate().map(|(si, sec)| {
            let chips = sec.names.iter().map(|name| {
                let ix = flat_ix;
                flat_ix += 1;
                let saved = *name == current;
                let at_cursor = ix == cursor && moved;
                let (n_hover, n_click) = (name.clone(), name.clone());
                div().w_1_2().p_0p5().child(
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
                        // The arrow keys' place gets the keyboard focus ring (keeping
                        // the picked row's bar).
                        .when(at_cursor, |r| {
                            let mut ring = widgets::focus_shadow(cx);
                            if saved {
                                ring.push(BoxShadow::new(px(3.), px(0.), theme.primary).inset());
                            }
                            r.shadow(ring)
                        })
                        .child(div().flex_1().min_w_0().child(crate::settings_page::theme_row(name, cx)))
                        .when(saved, |r| r.child(Icon::new(IconName::Check).xsmall().text_color(theme.foreground)))
                        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                            if *hovered {
                                this.point(Some(n_hover.clone()), cx);
                            } else if this.preview.want.as_deref() == Some(n_hover.as_str()) {
                                this.point(None, cx);
                            }
                        }))
                        .on_click(cx.listener(move |this, _, _, cx| this.commit(n_click.clone(), cx))),
                )
            });
            v_flex()
                .when(si > 0, |v| {
                    v.child(
                        div()
                            .px_2()
                            .pt_2()
                            .pb_0p5()
                            .text_xs()
                            .font_semibold()
                            .text_color(theme.foreground)
                            .child(sec.title.to_uppercase()),
                    )
                })
                .child(h_flex().flex_wrap().children(chips))
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
        let bounds = self.bounds.clone();
        let panel = v_flex()
            .id("appearance-popover")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &Up, _, cx| this.step(0, -1, cx)))
            .on_action(cx.listener(|this, _: &Down, _, cx| this.step(0, 1, cx)))
            .on_action(cx.listener(|this, _: &Left, _, cx| this.step(-1, 0, cx)))
            .on_action(cx.listener(|this, _: &Right, _, cx| this.step(1, 0, cx)))
            .on_action(cx.listener(|this, _: &Pick, _, cx| {
                if let Some(n) = Self::flat().get(this.cursor) {
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
                            .tooltip("Open your theme folder: gpui-component theme .json files go here")
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
                            .tooltip("Reload themes from the folder")
                            .on_click(cx.listener(|_, _, window, cx| {
                                let n = crate::theme::reload_custom();
                                window.push_notification(
                                    gpui_kit::component::notification::Notification::info(crate::theme::own_themes_text(n)),
                                    cx,
                                );
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .id("appearance-themes")
                    .h((rem * 18.).min(place.max_h - rem * 12.).max(rem * 6.))
                    .px_1p5()
                    .overflow_y_scrollbar()
                    .child(v_flex().children(blocks)),
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
                        .ghost()
                        .xsmall()
                        .label("All appearance settings")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close(cx);
                            app.update(cx, |a, cx| a.open_settings_at("theme", cx)).ok();
                        })),
                ),
            );
        // 27-30rem: "Catppuccin Macchiato" and a check fit a column with room
        // to spare (measured at 23 rem it touched the column's edge).
        Some(
            place
                .apply(panel, 27., 30., window)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn s(n: &str) -> Option<String> {
        Some(n.to_string())
    }

    #[::core::prelude::v1::test]
    fn resting_on_a_theme_shows_it_only_after_the_delay_runs_out() {
        let mut p = Preview::default();
        let g = p.point(s("Nord")).expect("a change");
        // Nothing on screen yet: the timer hasn't fired.
        assert!(!p.is_previewing());
        assert_eq!(p.settle(g), Some(s("Nord")));
        assert!(p.is_previewing());
        // Settling the same generation twice paints once.
        assert_eq!(p.settle(g), None);
    }

    #[::core::prelude::v1::test]
    fn sweeping_across_themes_paints_only_where_the_pointer_stops() {
        let mut p = Preview::default();
        let a = p.point(s("Nord")).unwrap();
        let b = p.point(s("Dracula")).unwrap();
        let c = p.point(s("Kanagawa")).unwrap();
        // The timers of the themes it moved past do nothing.
        assert_eq!(p.settle(a), None);
        assert_eq!(p.settle(b), None);
        assert_eq!(p.settle(c), Some(s("Kanagawa")));
        // The same theme again is no change at all.
        assert_eq!(p.point(s("Kanagawa")), None);
    }

    #[::core::prelude::v1::test]
    fn leaving_the_list_puts_the_saved_theme_back_after_the_delay() {
        let mut p = Preview::default();
        let g = p.point(s("Nord")).unwrap();
        p.settle(g);
        let leave = p.point(None).unwrap();
        // Still showing Nord until the delay, so moving to a neighbour in
        // between never flashes the old theme.
        assert!(p.is_previewing());
        assert_eq!(p.settle(leave), Some(None));
        assert!(!p.is_previewing());
    }

    #[::core::prelude::v1::test]
    fn leaving_and_coming_back_quickly_changes_nothing() {
        let mut p = Preview::default();
        let g = p.point(s("Nord")).unwrap();
        p.settle(g);
        let leave = p.point(None).unwrap();
        let back = p.point(s("Nord")).unwrap();
        assert_eq!(p.settle(leave), None);
        // Nord is still the one shown, so there is nothing to repaint.
        assert_eq!(p.settle(back), None);
        assert!(p.is_previewing());
    }

    #[::core::prelude::v1::test]
    fn closing_undoes_a_preview_and_cancels_a_pending_one() {
        // Esc or an outside click while a preview is on screen.
        let mut p = Preview::default();
        let g = p.point(s("Nord")).unwrap();
        p.settle(g);
        assert_eq!(p.close(), Some(None));
        assert!(!p.is_previewing());
        // Closing before the delay ran out: nothing was painted, nothing to undo,
        // and the late timer does nothing.
        let g = p.point(s("Dracula")).unwrap();
        assert_eq!(p.close(), None);
        assert_eq!(p.settle(g), None);
    }

    #[::core::prelude::v1::test]
    fn keeping_a_theme_ends_the_preview_without_a_revert() {
        let mut p = Preview::default();
        let g = p.point(s("Nord")).unwrap();
        p.settle(g);
        p.keep();
        assert!(!p.is_previewing());
        // Closing afterwards must not repaint the old saved theme over it.
        assert_eq!(p.close(), None);
    }

    #[::core::prelude::v1::test]
    fn arrow_keys_walk_the_blocks_like_the_grid_looks() {
        // System 1, Dark 5 (2 columns: 2+2+1), Light 4, Yours 1.
        let sizes = [1, 5, 4, 1];
        // Flat positions: System 0 | Dark 1..=5 | Light 6..=9 | Yours 10.
        assert_eq!(grid_move(&sizes, 0, 1, 0), 1);
        assert_eq!(grid_move(&sizes, 0, -1, 0), 0);
        assert_eq!(grid_move(&sizes, 10, 1, 0), 10);
        // Down from System goes to the top of Dark in the same column.
        assert_eq!(grid_move(&sizes, 0, 0, 1), 1);
        // Inside Dark, down keeps the column: first chip -> third.
        assert_eq!(grid_move(&sizes, 1, 0, 1), 3);
        // The short last row (one chip) is reached from either column.
        assert_eq!(grid_move(&sizes, 4, 0, 1), 5);
        assert_eq!(grid_move(&sizes, 3, 0, 1), 5);
        // Down from the last Dark chip enters Light's first row.
        assert_eq!(grid_move(&sizes, 5, 0, 1), 6);
        // Up from Light's top row lands in Dark's last (short) row.
        assert_eq!(grid_move(&sizes, 6, 0, -1), 5);
        assert_eq!(grid_move(&sizes, 7, 0, -1), 5);
        // Up from the very top stays; down from the very bottom stays.
        assert_eq!(grid_move(&sizes, 0, 0, -1), 0);
        assert_eq!(grid_move(&sizes, 10, 0, 1), 10);
        // Yours has one chip, in the left column: up lands on Light's last
        // row, left column.
        assert_eq!(grid_move(&sizes, 10, 0, -1), 8);
    }

    #[::core::prelude::v1::test]
    fn the_saved_name_follows_the_old_dark_light_setting() {
        let mut st = hopper::settings::Settings::default();
        st.theme_name = String::new();
        st.theme = 0;
        assert_eq!(current_name(&st), "");
        st.theme = 1;
        assert_eq!(current_name(&st), crate::theme::HOPPER_DARK);
        st.theme = 2;
        assert_eq!(current_name(&st), crate::theme::HOPPER_LIGHT);
        st.theme_name = "Nord".to_string();
        assert_eq!(current_name(&st), "Nord");
    }
}
