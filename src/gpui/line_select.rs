//! A scrolling view of text lines (a log, a console) whose text can be
//! picked with the mouse and copied: drag, double-click for a word,
//! triple-click for a line, Ctrl+A, Ctrl+C and a right-click menu.
//!
//! Long lines wrap inside the view (a log line is cut into rows of as many
//! characters as fit, the font being monospace) and the lines the list
//! builds are only those in sight, so a pick is kept as stable line keys
//! and characters, not as rows: it survives scrolling, filtering, resizing
//! and lines arriving, and copying reads the text from the owner.

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

/// Height of one row of text, in rem. A line that wraps is several rows.
const ROW_H: f32 = 1.25;

/// Room kept free at the right of the text, so a full row never touches
/// the scrollbar.
const RIGHT_ROOM: f32 = 6.;

/// A view's lines, as its owner keeps them.
pub trait Lines: 'static + Sized {
    fn line_select(&self) -> &LineSelect;
    fn line_select_mut(&mut self) -> &mut LineSelect;
    /// How many rows the view shows.
    fn row_count(&self) -> usize;
    /// A key for row `row` that stays with its line while rows come and go
    /// (later rows have larger keys).
    fn row_key(&self, row: usize) -> Option<usize>;
    /// The text of the line with key `key`, as shown (tabs as spaces).
    fn key_text(&self, key: usize) -> Option<String>;
    /// The color of row `row`'s text.
    fn row_color(&self, row: usize, cx: &App) -> Hsla;
}

/// The picking state a [`Lines`] owner keeps.
pub struct LineSelect {
    sel: Option<Sel>,
    /// The mouse button is down on the lines, picking text.
    dragging: bool,
    /// Where the lines are on screen, from the last frame.
    bounds: Rc<Cell<Bounds<Pixels>>>,
    /// A character's width in the lines' font, from the last frame.
    char_w: Pixels,
    /// The line number column's width, from the last frame.
    gutter_w: Pixels,
    /// Characters per row, from the last frame (a line longer than this
    /// wraps onto more rows).
    cols: usize,
    /// How many lines the list was told about, and the first and last line
    /// key, to tell lines arriving from lines replaced.
    seen: Option<(usize, Option<usize>, Option<usize>)>,
    /// Where the owner asked to scroll since the last frame (true: the
    /// end), applied again once the list has been told the new lines.
    goto: Cell<Option<bool>>,
    focus: FocusHandle,
    /// The list: its items are lines, of whatever height their wrapping
    /// makes them.
    pub scroll: ListState,
}

impl LineSelect {
    pub fn new(cx: &mut App) -> Self {
        Self {
            sel: None,
            dragging: false,
            bounds: Rc::new(Cell::new(Bounds::default())),
            char_w: px(7.),
            gutter_w: px(0.),
            cols: usize::MAX,
            seen: None,
            goto: Cell::new(None),
            focus: cx.focus_handle(),
            scroll: ListState::new(0, ListAlignment::Top, px(400.)),
        }
    }

    /// Drops the pick and what is known about the lines (another file is
    /// shown, say).
    pub fn clear(&mut self) {
        self.sel = None;
        self.dragging = false;
        self.seen = None;
    }

    /// Text is picked or being picked: a live view shouldn't jump away.
    pub fn is_active(&self) -> bool {
        self.sel.is_some() || self.dragging
    }

    /// Scrolls to the last row.
    pub fn scroll_to_end(&self) {
        self.goto.set(Some(true));
        self.scroll.scroll_to_end();
    }

    /// Scrolls to the first row.
    pub fn scroll_to_start(&self) {
        self.goto.set(Some(false));
        self.scroll.scroll_to(ListOffset { item_ix: 0, offset_in_item: px(0.) });
    }

    /// The view shows its last row (or isn't laid out yet, or has nothing
    /// to scroll): a live view follows new lines only then.
    pub fn at_end(&self) -> bool {
        if self.scroll.viewport_bounds().size.height <= px(0.) {
            return true;
        }
        let max = self.scroll.max_offset_for_scrollbar().y;
        let at = -self.scroll.scroll_px_offset_for_scrollbar().y;
        max <= px(0.) || at >= max - px(2.)
    }
}

/// Tells the list what the owner's rows are now. Lines that only arrived at
/// the end, or went from the top, keep the reader's place; anything else
/// is read again from where the reader was.
fn sync<T: Lines>(this: &mut T, hint: Pixels) {
    let n = this.row_count();
    let first = this.row_key(0);
    let last = n.checked_sub(1).and_then(|r| this.row_key(r));
    let ls = this.line_select_mut();
    let now = (n, first, last);
    let before = ls.seen.replace(now);
    let list = &ls.scroll;
    match before {
        Some(b) if b == now => {}
        Some((n0, Some(f0), _)) if first == Some(f0) && n >= n0 => list.splice(n0..n0, n - n0),
        Some((n0, Some(f0), _))
            if first.is_some_and(|f| f > f0) && last.zip(first).is_some_and(|(l, f)| l + 1 - f == n) =>
        {
            // The oldest lines went (a console's window moved on).
            let gone = (first.unwrap_or(f0) - f0).min(n0);
            list.splice(0..gone, 0);
            let kept = n0 - gone;
            if n >= kept {
                list.splice(kept..kept, n - kept);
            } else {
                list.reset(n);
            }
        }
        Some(_) => {
            let at = list.logical_scroll_top();
            list.reset_with_uniform_height(n, hint);
            list.scroll_to(ListOffset { item_ix: at.item_ix.min(n), ..at });
        }
        None => list.reset_with_uniform_height(n, hint),
    }
    match ls.goto.take() {
        Some(true) => list.scroll_to_end(),
        Some(false) => list.scroll_to(ListOffset { item_ix: 0, offset_in_item: px(0.) }),
        None => {}
    }
}

/// The lines in a scrolling box filling the space it gets. `empty` shows
/// when there are no lines.
pub fn view<T: Lines>(
    this: &mut T,
    id: &'static str,
    empty: Option<AnyElement>,
    window: &mut Window,
    cx: &mut Context<T>,
) -> AnyElement {
    let theme = cx.theme().clone();
    // The character width, for mapping the mouse to characters.
    let size = rems(0.75).to_pixels(window.rem_size());
    let ts = window.text_system();
    let font_id = ts.resolve_font(&font(theme.mono_font_family.clone()));
    if let Ok(adv) = ts.advance(font_id, size, 'm') {
        this.line_select_mut().char_w = adv.width;
    }
    let n = this.row_count();
    // Line numbers: as wide as the largest one, three digits at least.
    let digits = n
        .checked_sub(1)
        .and_then(|r| this.row_key(r))
        .map_or(1, |k| (k + 1).to_string().len())
        .max(3);
    let ls = this.line_select_mut();
    ls.gutter_w = ls.char_w * (digits + 2) as f32;
    // Characters per row: what the box holds beside the numbers. Until the
    // box has been laid out once, nothing wraps.
    let room = ls.bounds.get().size.width - ls.gutter_w - px(RIGHT_ROOM);
    let cols = if room > ls.char_w * 4. { ((room / ls.char_w) as usize).max(4) } else { usize::MAX };
    let ls = this.line_select_mut();
    if ls.cols != cols {
        ls.cols = cols;
        ls.scroll.remeasure();
    }
    sync(this, rems(ROW_H).to_pixels(window.rem_size()));
    let ls = this.line_select();
    let (focus, scroll, has) = (ls.focus.clone(), ls.scroll.clone(), ls.sel.is_some());
    let view = cx.entity();
    let weak = view.downgrade();
    div()
        .id(id)
        .flex_1()
        .min_h_0()
        .relative()
        .rounded(theme.radius_lg)
        .bg(theme.muted)
        .p_3()
        .font_family(theme.mono_font_family.clone())
        .text_xs()
        .cursor_text()
        .track_focus(&focus)
        .on_key_down(cx.listener(|this: &mut T, e: &KeyDownEvent, _, cx| {
            let m = &e.keystroke.modifiers;
            if !(m.control || m.platform) {
                return;
            }
            match e.keystroke.key.as_str() {
                "c" => copy(this, cx),
                "a" => select_all(this, cx),
                _ => return,
            }
            cx.stop_propagation();
        }))
        .when_some(empty.filter(|_| n == 0), |d, e| d.child(e))
        .child(
            div()
                .id(ElementId::Name(format!("{id}-lines").into()))
                .size_full()
                .relative()
                .on_mouse_down(MouseButton::Left, cx.listener(|this: &mut T, e: &MouseDownEvent, window, cx| {
                    mouse_down(this, e, window, cx)
                }))
                .context_menu(move |menu, _, _| {
                    let (v1, v2) = (view.clone(), view.clone());
                    menu.when(has, |m| {
                        m.item(
                            PopupMenuItem::new("Copy")
                                .icon(IconName::Copy)
                                .on_click(move |_, _, cx| v1.update(cx, |this, cx| copy(this, cx))),
                        )
                    })
                    .item(
                        PopupMenuItem::new("Select all")
                            .on_click(move |_, _, cx| v2.update(cx, |this, cx| select_all(this, cx))),
                    )
                })
                .child(
                    list(scroll.clone(), move |ix, _, cx| {
                        weak.update(cx, |this: &mut T, cx| row(this, ix, cx))
                            .unwrap_or_else(|_| div().into_any_element())
                    })
                    .size_full(),
                )
                .child(drag_watch(this, cx)),
        )
        .vertical_scrollbar(&scroll)
        .into_any_element()
}

/// Row `ix` of the list: one line, as the rows its wrapping makes, the
/// number beside the first. The text goes in the column after the numbers,
/// so the rows after the first line up under the first one's text.
fn row<T: Lines>(this: &mut T, ix: usize, cx: &mut Context<T>) -> AnyElement {
    let (sel_bg, muted) = (cx.theme().selection, cx.theme().muted_foreground);
    let ls = this.line_select();
    let (cw, gw, cols) = (ls.char_w, ls.gutter_w, ls.cols.max(1));
    let Some(key) = this.row_key(ix) else { return div().into_any_element() };
    let Some(text) = this.key_text(key) else { return div().into_any_element() };
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let picked = picked_cols(this.line_select().sel, key, len);
    let color = this.row_color(ix, cx);
    let rows = len.div_ceil(cols).max(1);
    h_flex()
        .w_full()
        .items_start()
        .child(
            div()
                .w(gw)
                .flex_none()
                .h(rems(ROW_H))
                .pr(cw * 2.)
                .text_right()
                .text_color(muted.opacity(0.6))
                .child((key + 1).to_string()),
        )
        .child(
            v_flex().flex_1().min_w_0().text_color(color).children((0..rows).map(|k| {
                let (start, end) = (k * cols, ((k + 1) * cols).min(len));
                // The pick runs one past a line's last character when it
                // goes on to the next line.
                let mark = picked.as_ref().and_then(|r| {
                    let (a, b) = (r.start.max(start), r.end.min(if k + 1 == rows { end + 1 } else { end }));
                    (a < b).then_some((a - start, b - start))
                });
                div()
                    .relative()
                    .h(rems(ROW_H))
                    .whitespace_nowrap()
                    .when_some(mark, |d, (a, b)| {
                        d.child(div().absolute().top_0().bottom_0().left(cw * a as f32).w(cw * (b - a) as f32).bg(sel_bg))
                    })
                    .child(div().relative().child(chars[start..end].iter().collect::<String>()))
            })),
        )
        .into_any_element()
}

/// The picked characters of the line with key `key` (`len` long); one
/// past the end when the pick runs on to the next line.
fn picked_cols(sel: Option<Sel>, key: usize, len: usize) -> Option<Range<usize>> {
    let (a, b) = sel?.ordered();
    if key < a.0 || key > b.0 {
        return None;
    }
    let start = if key == a.0 { a.1.min(len) } else { 0 };
    let end = if key == b.0 { b.1.min(len) } else { len + 1 };
    (start < end).then_some(start..end)
}

/// The line key and character under `pos`, clamped to the lines. Rows
/// are found from where the list put the lines on screen, so wrapped
/// lines and any zoom hit the right character.
fn hit<T: Lines>(this: &T, pos: Point<Pixels>) -> Option<(usize, usize)> {
    let n = this.row_count();
    if n == 0 {
        return None;
    }
    let ls = this.line_select();
    let view = ls.scroll.viewport_bounds();
    let y = pos.y.max(view.top()).min(view.bottom() - px(1.));
    let mut found = None;
    for ix in ls.scroll.logical_scroll_top().item_ix.min(n - 1)..n {
        let Some(b) = ls.scroll.bounds_for_item(ix) else { break };
        found = Some((ix, b));
        if y < b.bottom() {
            break;
        }
    }
    let (ix, b) = found?;
    let key = this.row_key(ix)?;
    let len = this.key_text(key).map_or(0, |t| t.chars().count());
    let cols = ls.cols.max(1);
    let rows = len.div_ceil(cols).max(1);
    let lh = b.size.height / rows as f32;
    let k = if y < b.top() || lh <= px(0.) { 0 } else { (((y - b.top()) / lh) as usize).min(rows - 1) };
    let x = (pos.x - view.left() - ls.gutter_w).max(px(0.));
    let col = ((x / ls.char_w).round() as usize).min(cols);
    Some((key, (k * cols + col).min(len)))
}

fn mouse_down<T: Lines>(this: &mut T, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<T>) {
    window.focus(&this.line_select().focus, cx);
    let Some(at) = hit(this, e.position) else { return };
    let text = this.key_text(at.0).unwrap_or_default();
    let ls = this.line_select_mut();
    ls.dragging = true;
    ls.sel = Some(match (e.click_count, ls.sel) {
        (1, Some(old)) if e.modifiers.shift => Sel { head: at, ..old },
        (1, _) => Sel { anchor: at, head: at },
        (2, _) => {
            let r = word_at(&text, at.1);
            Sel { anchor: (at.0, r.start), head: (at.0, r.end) }
        }
        _ => Sel { anchor: (at.0, 0), head: (at.0, text.chars().count()) },
    });
    cx.notify();
}

fn drag_to<T: Lines>(this: &mut T, pos: Point<Pixels>, window: &mut Window, cx: &mut Context<T>) {
    // Past the top or bottom, the lines scroll along.
    let ls = this.line_select();
    let view = ls.scroll.viewport_bounds();
    let step = rems(ROW_H).to_pixels(window.rem_size());
    if pos.y < view.top() {
        ls.scroll.scroll_by(-step);
    } else if pos.y > view.bottom() {
        ls.scroll.scroll_by(step);
    }
    let at = hit(this, pos);
    if let (Some(at), Some(sel)) = (at, this.line_select_mut().sel.as_mut()) {
        sel.head = at;
    }
    cx.notify();
}

/// The picked text, over the rows the view shows.
fn picked_text<T: Lines>(this: &T) -> String {
    let Some((a, b)) = this.line_select().sel.map(|s| s.ordered()) else {
        return String::new();
    };
    let mut out = Vec::new();
    for row in 0..this.row_count() {
        let Some(key) = this.row_key(row) else { continue };
        if key < a.0 {
            continue;
        }
        if key > b.0 {
            break;
        }
        let text = this.key_text(key).unwrap_or_default();
        let len = text.chars().count();
        let start = if key == a.0 { a.1.min(len) } else { 0 };
        let end = if key == b.0 { b.1.min(len) } else { len };
        out.push(text.chars().skip(start).take(end.saturating_sub(start)).collect::<String>());
    }
    out.join("\n")
}

fn copy<T: Lines>(this: &mut T, cx: &mut Context<T>) {
    let text = picked_text(this);
    if !text.is_empty() {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }
}

fn select_all<T: Lines>(this: &mut T, cx: &mut Context<T>) {
    let last = this.row_count().checked_sub(1).and_then(|r| this.row_key(r));
    if let (Some(first), Some(last)) = (this.row_key(0), last) {
        let len = this.key_text(last).map_or(0, |t| t.chars().count());
        this.line_select_mut().sel = Some(Sel { anchor: (first, 0), head: (last, len) });
        cx.notify();
    }
}

/// Notes where the lines are, and while text is being picked, follows the
/// mouse anywhere in the window (also past the view's edges).
fn drag_watch<T: Lines>(this: &T, cx: &mut Context<T>) -> impl IntoElement + use<T> {
    let bounds = this.line_select().bounds.clone();
    let view = cx.entity().downgrade();
    canvas(
        // A new width changes how lines wrap: draw again with it.
        move |b, window, _| {
            if bounds.get().size.width != b.size.width {
                window.refresh();
            }
            bounds.set(b)
        },
        move |_, _, window, _| {
            let v = view.clone();
            window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble && e.pressed_button == Some(MouseButton::Left) {
                    v.update(cx, |this, cx| {
                        if this.line_select().dragging {
                            drag_to(this, e.position, window, cx)
                        }
                    })
                    .ok();
                }
            });
            let v = view.clone();
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble {
                    v.update(cx, |this, cx| {
                        let ls = this.line_select_mut();
                        if !ls.dragging {
                            return;
                        }
                        ls.dragging = false;
                        if ls.sel.is_some_and(|s| s.anchor == s.head) {
                            ls.sel = None;
                        }
                        cx.notify();
                    })
                    .ok();
                }
            });
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// Picked text: where the mouse went down and where it is now, as (line
/// key, character).
#[derive(Clone, Copy, PartialEq, Debug)]
struct Sel {
    anchor: (usize, usize),
    head: (usize, usize),
}

impl Sel {
    fn ordered(self) -> ((usize, usize), (usize, usize)) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }
}

/// The word (run of non-space characters) around character `col`.
fn word_at(text: &str, col: usize) -> Range<usize> {
    let chars: Vec<char> = text.chars().collect();
    let col = col.min(chars.len().saturating_sub(1));
    if chars.get(col).is_none_or(|c| c.is_whitespace()) {
        return col..col;
    }
    let mut start = col;
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }
    let mut end = col;
    while end < chars.len() && !chars[end].is_whitespace() {
        end += 1;
    }
    start..end
}

/// How far a re-read window of lines moved: the fewest lines `old` lost
/// from its top so that what is left starts `new`. `old.len()` when they
/// share nothing (keys then move past every old line).
pub fn shift(old: &[String], new: &[String]) -> usize {
    (0..old.len())
        .find(|&k| {
            let rest = &old[k..];
            rest.len() <= new.len() && new[..rest.len()] == *rest
        })
        .unwrap_or(old.len())
}

#[cfg(test)]
mod tests {
    use super::{Sel, picked_cols, shift, word_at};

    #[test]
    fn picks_cover_lines_between() {
        let s = Some(Sel { anchor: (5, 3), head: (3, 2) });
        assert_eq!(picked_cols(s, 3, 10), Some(2..11));
        assert_eq!(picked_cols(s, 4, 10), Some(0..11));
        assert_eq!(picked_cols(s, 5, 10), Some(0..3));
        assert_eq!(picked_cols(s, 6, 10), None);
    }

    #[test]
    fn words_are_runs_of_non_space() {
        assert_eq!(word_at("at java.lang.Foo(x)", 6), 3..19);
        assert_eq!(word_at("a  b", 1), 1..1);
    }

    #[test]
    fn finds_how_far_the_window_moved() {
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(shift(&v(&["a", "b", "c"]), &v(&["b", "c", "d"])), 1);
        assert_eq!(shift(&v(&["a", "b"]), &v(&["a", "b", "c"])), 0);
        assert_eq!(shift(&v(&["a", "b"]), &v(&["x", "y"])), 2);
    }
}
