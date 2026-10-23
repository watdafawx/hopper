//! Ticking many rows at once: press a row's tick box and drag. Every row the
//! pointer crosses takes the state the first box got (ticked, or unticked
//! when it started ticked). Held near the list's top or bottom edge, the
//! list scrolls by itself and keeps picking up rows.
//!
//! The view keeps a `DragPick` while the button is down, puts
//! [`cell`]-style handlers on each tick box, and on its list area:
//! `on_drag_move::<PickDrag>` (pointer moved), `on_mouse_up` and
//! `on_mouse_up_out` (done).

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;
use std::collections::HashSet;

/// What GPUI carries while a pick drag runs.
#[derive(Clone, Copy)]
pub struct PickDrag;

/// The small label beside the pointer: "Ticking 5".
pub struct PickChip {
    pub text: SharedString,
}

impl Render for PickChip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        if self.text.is_empty() {
            return div();
        }
        div()
            .ml_4()
            .mt_3()
            .px_2p5()
            .py_0p5()
            .rounded_full()
            .bg(theme.primary)
            .text_color(theme.primary_foreground)
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .shadow_md()
            .child(self.text.clone())
    }
}

pub struct DragPick {
    pub start: usize,
    pub cur: usize,
    /// Ticking (true) or unticking.
    pub on: bool,
    /// The picks before the drag began.
    base: HashSet<String>,
    /// The pointer's height in the window, for the edge scroll.
    y: Option<Pixels>,
    pub chip: Option<Entity<PickChip>>,
}

impl DragPick {
    pub fn new(start: usize, on: bool, base: HashSet<String>) -> Self {
        DragPick { start, cur: start, on, base, y: None, chip: None }
    }

    /// The picks with rows `start..=cur` set to the drag's state.
    pub fn apply(&self, key_of: impl Fn(usize) -> Option<String>) -> HashSet<String> {
        let mut out = self.base.clone();
        for ix in self.start.min(self.cur)..=self.start.max(self.cur) {
            if let Some(k) = key_of(ix) {
                if self.on {
                    out.insert(k);
                } else {
                    out.remove(&k);
                }
            }
        }
        out
    }

    pub fn label(&self) -> SharedString {
        if self.cur == self.start {
            return SharedString::default();
        }
        let n = self.start.abs_diff(self.cur) + 1;
        format!("{} {n}", if self.on { "Ticking" } else { "Unticking" }).into()
    }

    /// The pointer moved to `y`: the row under it (by the list's scroll and
    /// row height) becomes the drag's end. True when that changed.
    pub fn moved(&mut self, y: Pixels, scroll: &UniformListScrollHandle, row_h: Pixels, count: usize) -> bool {
        self.y = Some(y);
        self.track(scroll, row_h, count)
    }

    fn track(&mut self, scroll: &UniformListScrollHandle, row_h: Pixels, count: usize) -> bool {
        let Some(y) = self.y else { return false };
        let Some(ix) = row_at(scroll, y, row_h, count) else { return false };
        let changed = ix != self.cur;
        self.cur = ix;
        changed
    }

    /// One frame of the edge scroll: scrolls when the pointer is near or past
    /// the list's top or bottom, then moves the drag's end along. True when
    /// it scrolled.
    pub fn edge_scroll(&mut self, scroll: &UniformListScrollHandle, row_h: Pixels, count: usize) -> bool {
        let Some(y) = self.y else { return false };
        let handle = scroll.0.borrow().base_handle.clone();
        let b = handle.bounds();
        let zone = px(40.);
        let step = if y < b.top() + zone {
            -((b.top() + zone - y) / 3.).min(px(24.))
        } else if y > b.bottom() - zone {
            ((y - (b.bottom() - zone)) / 3.).min(px(24.))
        } else {
            px(0.)
        };
        if step == px(0.) {
            return false;
        }
        let off = handle.offset();
        let max = handle.max_offset().y;
        // The offset runs from 0 (top) down to -max.
        let to = (off.y - step).clamp(-max, px(0.));
        if to == off.y {
            return false;
        }
        handle.set_offset(point(off.x, to));
        self.track(scroll, row_h, count);
        true
    }
}

/// The row under window height `y` in a uniform list (clamped to the list).
pub fn row_at(scroll: &UniformListScrollHandle, y: Pixels, row_h: Pixels, count: usize) -> Option<usize> {
    if count == 0 || row_h <= px(0.) {
        return None;
    }
    let handle = scroll.0.borrow().base_handle.clone();
    let b = handle.bounds();
    if b.size.height <= px(1.) {
        return None;
    }
    let y = y.clamp(b.top(), b.bottom() - px(1.));
    let ix = ((y - b.top() - handle.offset().y) / row_h).floor();
    Some((ix.max(0.) as usize).min(count - 1))
}

/// A view with a pick drag, so the drag's label can find it.
pub trait Picks {
    fn pick_mut(&mut self) -> Option<&mut DragPick>;
}

/// The label a pick drag shows beside the pointer, made when GPUI starts
/// the drag (after the pointer moves a little) and kept by the view's pick
/// so its count updates.
pub fn chip_for<V: Picks + 'static>(view: &Entity<V>, cx: &mut App) -> Entity<PickChip> {
    let chip = cx.new(|_| PickChip { text: SharedString::default() });
    view.update(cx, |v, _| {
        if let Some(p) = v.pick_mut() {
            p.chip = Some(chip.clone());
        }
    });
    chip
}
