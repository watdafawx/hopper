//! A scrolling view of text lines (a log, a console) whose text can be
//! picked with the mouse and copied: drag, double-click for a word,
//! triple-click for a line, Ctrl+A, Ctrl+C and a right-click menu.
//!
//! The view only builds the lines in sight, so a pick is kept as stable
//! line keys and characters, not as rows: it survives scrolling, filtering
//! and lines arriving, and copying reads the text from the owner.

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

/// Row height, in rem.
const ROW_H: f32 = 1.25;

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
    focus: FocusHandle,
    pub scroll: UniformListScrollHandle,
}

impl LineSelect {
    pub fn new(cx: &mut App) -> Self {
        Self {
            sel: None,
            dragging: false,
            bounds: Rc::new(Cell::new(Bounds::default())),
            char_w: px(7.),
            gutter_w: px(0.),
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
        }
    }

    /// Drops the pick (another file is shown, say).
    pub fn clear(&mut self) {
        self.sel = None;
        self.dragging = false;
    }

    /// Text is picked or being picked: a live view shouldn't jump away.
    pub fn is_active(&self) -> bool {
        self.sel.is_some() || self.dragging
    }

    pub fn scroll_to_end(&self, rows: usize) {
        if rows > 0 {
            self.scroll.scroll_to_item(rows - 1, ScrollStrategy::Bottom);
        }
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
    let ls = this.line_select();
    let (focus, scroll, has) = (ls.focus.clone(), ls.scroll.clone(), ls.sel.is_some());
    let view = cx.entity();
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
                    uniform_list(id, n, cx.processor(|this: &mut T, r: Range<usize>, _, cx| rows(this, r, cx)))
                        .track_scroll(&scroll)
                        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                        .size_full(),
                )
                .child(drag_watch(this, cx)),
        )
        .vertical_scrollbar(&scroll)
        .into_any_element()
}

fn rows<T: Lines>(this: &mut T, range: Range<usize>, cx: &mut Context<T>) -> Vec<AnyElement> {
    let (sel_bg, muted) = (cx.theme().selection, cx.theme().muted_foreground);
    let (cw, gw) = (this.line_select().char_w, this.line_select().gutter_w);
    range
        .filter_map(|row| {
            let key = this.row_key(row)?;
            let text = this.key_text(key)?;
            let picked = picked_cols(this.line_select().sel, key, text.chars().count());
            Some(
                h_flex()
                    .w_full()
                    .h(rems(ROW_H))
                    .whitespace_nowrap()
                    .child(
                        div()
                            .w(gw)
                            .flex_none()
                            .pr(cw * 2.)
                            .text_right()
                            .text_color(muted.opacity(0.6))
                            .child((key + 1).to_string()),
                    )
                    .child(
                        div()
                            .relative()
                            .h_full()
                            .flex_none()
                            .text_color(this.row_color(row, cx))
                            .when_some(picked, |d, r| {
                                d.child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .bottom_0()
                                        .left(cw * r.start as f32)
                                        .w(cw * (r.end - r.start) as f32)
                                        .bg(sel_bg),
                                )
                            })
                            .child(div().relative().child(text)),
                    )
                    .into_any_element(),
            )
        })
        .collect()
}

/// A row's height as laid out: rounded to whole pixels, so taken from the
/// list's own content height rather than the rem size.
fn row_height(scroll: &UniformListScrollHandle, rows: usize, window: &Window) -> Pixels {
    scroll
        .0
        .borrow()
        .last_item_size
        .filter(|_| rows > 0)
        .map(|s| s.contents.height / rows as f32)
        .filter(|h| *h > px(0.))
        .unwrap_or_else(|| rems(ROW_H).to_pixels(window.rem_size()))
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

/// The line key and character under `pos`, clamped to the lines.
fn hit<T: Lines>(this: &T, pos: Point<Pixels>, window: &Window) -> Option<(usize, usize)> {
    let n = this.row_count();
    if n == 0 {
        return None;
    }
    let ls = this.line_select();
    let b = ls.bounds.get();
    let off = ls.scroll.0.borrow().base_handle.offset();
    let lh = row_height(&ls.scroll, this.row_count(), window);
    let y = pos.y - b.origin.y - off.y;
    let row = if y < px(0.) { 0 } else { ((y / lh) as usize).min(n - 1) };
    let x = (pos.x - b.origin.x - off.x - ls.gutter_w).max(px(0.));
    let col = (x / ls.char_w).round() as usize;
    let key = this.row_key(row)?;
    let len = this.key_text(key).map_or(0, |t| t.chars().count());
    Some((key, col.min(len)))
}

fn mouse_down<T: Lines>(this: &mut T, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<T>) {
    window.focus(&this.line_select().focus, cx);
    let Some(at) = hit(this, e.position, window) else { return };
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
    let b = ls.bounds.get();
    let lh = row_height(&ls.scroll, this.row_count(), window);
    let base = ls.scroll.0.borrow().base_handle.clone();
    let mut off = base.offset();
    if pos.y < b.origin.y {
        off.y = (off.y + lh).min(px(0.));
        base.set_offset(off);
    } else if pos.y > b.bottom() {
        off.y -= lh;
        base.set_offset(off);
    }
    let at = hit(this, pos, window);
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
        move |b, _, _| bounds.set(b),
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
