//! Press a row control and drag across rows: every row the pointer crosses
//! gets the same.
//!
//! - Tick boxes ([`Sweep::Tick`]): rows take the state the first box got
//!   (ticked, or unticked when it started ticked), at once.
//! - On/off switches ([`Sweep::Switch`]): the crossed switches show the new
//!   state while the button is held; letting go switches them all in one go,
//!   so the dependency check runs once, for the lot.
//! - Row buttons such as Update ([`Sweep::Action`]): the crossed buttons light
//!   up; letting go runs them all.
//!
//! Held near the list's top or bottom edge, the list scrolls by itself and
//! keeps picking up rows.
//!
//! A view keeps a [`PickState`] and implements [`Picks`]. Each control gets
//! `on_mouse_down` (start the drag) and `on_drag(PickDrag, chip_for)`; the
//! list area gets `on_drag_move::<PickDrag>` → [`moved`] and `on_mouse_up` /
//! `on_mouse_up_out` → [`released`]. A swept control's own click handler
//! asks [`click_swallowed`] first, so letting go over it doesn't run it twice.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;
use std::collections::HashSet;
use std::ops::RangeInclusive;

/// What GPUI carries while a pick drag runs.
#[derive(Clone, Copy)]
pub struct PickDrag;

/// What a drag does to the rows it crosses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sweep {
    /// Ticks or unticks them, as it goes.
    Tick,
    /// Switches them on or off, on letting go.
    Switch,
    /// Presses their button (Update, Add…), on letting go.
    Action,
}

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
            .rounded(theme.radius)
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
    pub kind: Sweep,
    /// Ticking or switching on (true), or the opposite.
    pub on: bool,
    /// The pointer has been on another row than the first.
    pub spread: bool,
    /// The picks before the drag began (ticks only).
    base: HashSet<String>,
    /// The pointer's height in the window, for the edge scroll.
    y: Option<Pixels>,
    /// The chip's text.
    text: SharedString,
    pub chip: Option<Entity<PickChip>>,
}

impl DragPick {
    /// A tick drag.
    pub fn new(start: usize, on: bool, base: HashSet<String>) -> Self {
        DragPick {
            start,
            cur: start,
            kind: Sweep::Tick,
            on,
            spread: false,
            base,
            y: None,
            text: SharedString::default(),
            chip: None,
        }
    }

    /// A switch or button drag.
    pub fn sweep(start: usize, kind: Sweep, on: bool) -> Self {
        DragPick { kind, ..Self::new(start, on, HashSet::new()) }
    }

    /// The rows crossed so far.
    pub fn rows(&self) -> RangeInclusive<usize> {
        self.start.min(self.cur)..=self.start.max(self.cur)
    }

    /// GPUI started the drag (the pointer moved while held).
    pub fn dragged(&self) -> bool {
        self.chip.is_some()
    }

    /// The picks with rows `start..=cur` set to the drag's state.
    pub fn apply(&self, key_of: impl Fn(usize) -> Option<String>) -> HashSet<String> {
        let mut out = self.base.clone();
        for ix in self.rows() {
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

    /// A tick drag's chip: "Ticking 5" (nothing on the first row alone).
    pub fn tick_label(&self) -> SharedString {
        if self.cur == self.start {
            return SharedString::default();
        }
        let n = self.start.abs_diff(self.cur) + 1;
        format!("{} {n}", if self.on { "Ticking" } else { "Unticking" }).into()
    }

    /// A sweep's chip: "Updating 5" ("Nothing to update" when none fit).
    pub fn count_label(verb: &str, none: &str, n: usize) -> SharedString {
        if n == 0 { none.to_string().into() } else { format!("{verb} {n}").into() }
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
        self.spread |= ix != self.start;
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

    /// Puts the chip's text on the chip, once it exists.
    fn show(&self, cx: &mut App) {
        if let Some(c) = &self.chip {
            let t = self.text.clone();
            c.update(cx, |c, cx| {
                if c.text != t {
                    c.text = t;
                    cx.notify();
                }
            });
        }
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

/// A view's drag: the drag itself, and the frame loop that scrolls at the
/// list's edges and notices the drag ending.
#[derive(Default)]
pub struct PickState {
    pub drag: Option<DragPick>,
    row_h: Pixels,
    tick: Option<Task<()>>,
}

impl PickState {
    /// The running sweep's kind, once the pointer has crossed rows.
    pub fn sweeping(&self) -> Option<Sweep> {
        self.drag.as_ref().filter(|p| p.dragged()).map(|p| p.kind)
    }
}

/// A view with a pick drag.
pub trait Picks: Sized + 'static {
    fn picks(&mut self) -> &mut PickState;
    /// The list's scroll handle and its number of lines.
    fn pick_list(&self) -> (UniformListScrollHandle, usize);
    /// The drag's rows changed: tick them, or show what letting go will do.
    /// Returns the chip's text.
    fn pick_changed(&mut self, p: &DragPick, cx: &mut Context<Self>) -> SharedString;
    /// Let go after a switch or button sweep: do it to the crossed rows.
    fn pick_commit(&mut self, p: DragPick, window: &mut Window, cx: &mut Context<Self>);
}

/// Starts a drag (from a control's mouse down).
pub fn start<V: Picks>(this: &mut V, p: DragPick, cx: &mut Context<V>) {
    let s = this.picks();
    s.drag = Some(p);
    s.tick = None;
    cx.notify();
}

/// The label a pick drag shows beside the pointer, made when GPUI starts
/// the drag (after the pointer moves a little) and kept by the view's pick
/// so its count updates.
pub fn chip_for<V: Picks>(view: &Entity<V>, cx: &mut App) -> Entity<PickChip> {
    let chip = cx.new(|_| PickChip { text: SharedString::default() });
    view.update(cx, |v, cx| {
        if let Some(mut p) = v.picks().drag.take() {
            p.chip = Some(chip.clone());
            // A sweep shows its first row at once.
            if p.kind != Sweep::Tick {
                p.text = v.pick_changed(&p, cx);
                cx.notify();
            }
            p.show(cx);
            v.picks().drag = Some(p);
        }
    });
    chip
}

/// Makes `el` (a row's tick box, switch or button) start a drag when
/// pressed: `down` sets the drag up with [`start`].
pub fn handle<V: Picks>(
    el: Stateful<Div>,
    cx: &Context<V>,
    down: impl Fn(&mut V, &MouseDownEvent, &mut Context<V>) + 'static,
) -> Stateful<Div> {
    let view = cx.entity();
    el.on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| down(this, e, cx)))
        .on_drag(PickDrag, move |_, _, _, cx| chip_for(&view, cx))
}

/// The pointer moved during a drag (the list's `on_drag_move::<PickDrag>`).
pub fn moved<V: Picks>(this: &mut V, y: Pixels, row_h: Pixels, window: &mut Window, cx: &mut Context<V>) {
    let Some(mut p) = this.picks().drag.take() else { return };
    this.picks().row_h = row_h;
    let (scroll, n) = this.pick_list();
    if p.moved(y, &scroll, row_h, n) {
        p.text = this.pick_changed(&p, cx);
        cx.notify();
    }
    p.show(cx);
    this.picks().drag = Some(p);
    if this.picks().tick.is_none() {
        let task = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(16)).await;
                let go = this.update_in(cx, |this, window, cx| frame(this, window, cx)).unwrap_or(false);
                if !go {
                    break;
                }
            }
        });
        this.picks().tick = Some(task);
    }
}

/// One frame while a drag runs: scrolls at the edges. False once it ended.
fn frame<V: Picks>(this: &mut V, window: &mut Window, cx: &mut Context<V>) -> bool {
    // Let go where the list didn't hear it (a control that stops the
    // event): finish here.
    if !cx.has_active_drag() {
        released(this, window, cx);
        return false;
    }
    let Some(mut p) = this.picks().drag.take() else {
        this.picks().tick = None;
        return false;
    };
    let (scroll, n) = this.pick_list();
    let row_h = this.picks().row_h;
    if p.edge_scroll(&scroll, row_h, n) {
        p.text = this.pick_changed(&p, cx);
        p.show(cx);
        cx.notify();
    }
    this.picks().drag = Some(p);
    true
}

/// The button was let go (the list's `on_mouse_up` / `on_mouse_up_out`):
/// a sweep that crossed rows runs.
pub fn released<V: Picks>(this: &mut V, window: &mut Window, cx: &mut Context<V>) {
    this.picks().tick = None;
    let Some(p) = this.picks().drag.take() else { return };
    if p.kind != Sweep::Tick && p.dragged() {
        this.pick_commit(p, window, cx);
    }
    cx.notify();
}

/// For a swept control's click: true when a drag that crossed rows is
/// ending over it, so the sweep does the work. A plain click (or a drag
/// that stayed on its row) ends the sweep here and runs as a click.
pub fn click_swallowed<V: Picks>(this: &mut V) -> bool {
    let s = this.picks();
    match &s.drag {
        Some(p) if p.spread => true,
        Some(p) if p.kind != Sweep::Tick => {
            s.drag = None;
            s.tick = None;
            false
        }
        _ => false,
    }
}

/// A drag that crossed rows is running (for a row's own click, which
/// shouldn't fire when a drag ends on it).
pub fn spreading<V: Picks>(this: &mut V) -> bool {
    this.picks().drag.as_ref().is_some_and(|p| p.spread)
}

// ---- painting a choice down a plain list ---------------------------------------

/// Press an option on one row of a plain (not virtual) list and move over
/// other rows with the button held: each row takes the same option. For
/// choice pairs (Keep mine / Use the pack's) and tick boxes in dialogs.
/// Cheap to clone; every row of a list shares one.
#[derive(Clone, Default)]
pub struct Paint(std::rc::Rc<std::cell::RefCell<Option<(SharedString, usize)>>>);

impl Paint {
    /// Pressed option `value` on a row of list `group`.
    pub fn start(&self, group: &SharedString, value: usize) {
        *self.0.borrow_mut() = Some((group.clone(), value));
    }

    /// The pointer moved over a row of `group`: the option to give it while
    /// painting. A move without the button ends the paint.
    fn over(&self, group: &SharedString, pressed: bool) -> Option<usize> {
        let mut s = self.0.borrow_mut();
        if !pressed {
            *s = None;
            return None;
        }
        s.as_ref().filter(|(g, _)| g == group).map(|(_, v)| *v)
    }

    pub fn end(&self) {
        *self.0.borrow_mut() = None;
    }

    /// A paint in `group` is running (a tick box's own click, which comes
    /// with the release, skips itself then: the press already set it).
    pub fn active(&self, group: &SharedString) -> bool {
        self.0.borrow().as_ref().is_some_and(|(g, _)| g == group)
    }
}

/// `el` (one option of a row) starts a paint when pressed: `set(value)`
/// gives this row the option at once.
pub fn paint_option<E: InteractiveElement>(
    el: E,
    paint: &Paint,
    group: impl Into<SharedString>,
    value: usize,
    set: impl Fn(usize, &mut Window, &mut App) + 'static,
) -> E {
    let (p, g) = (paint.clone(), group.into());
    el.on_mouse_down(MouseButton::Left, move |_, window, cx| {
        p.start(&g, value);
        set(value, window, cx);
    })
}

/// `el` (a whole row) takes the running paint when the pointer moves over
/// it with the button held. `set` should do nothing when the row already
/// has that option.
pub fn paint_row<E: InteractiveElement>(
    el: E,
    paint: &Paint,
    group: impl Into<SharedString>,
    set: impl Fn(usize, &mut Window, &mut App) + 'static,
) -> E {
    let (p, g, p2) = (paint.clone(), group.into(), paint.clone());
    el.on_mouse_move(move |e: &MouseMoveEvent, window, cx| {
        if let Some(v) = p.over(&g, e.pressed_button == Some(MouseButton::Left)) {
            set(v, window, cx);
        }
    })
    .on_mouse_up(MouseButton::Left, move |_, _, _| p2.end())
}
