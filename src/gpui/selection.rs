//! Picking several rows of a list the way every file explorer does, shared
//! by every list that holds things (crash reports, logs, mods, worlds,
//! backups, files…):
//!
//! - a click picks that row alone;
//! - Ctrl+click adds or takes away one row;
//! - Shift+click picks the rows from the last plain or Ctrl click
//!   (the anchor) to here; Ctrl+Shift+click adds them to what was picked;
//! - Ctrl+A picks every row the list shows (after its filter);
//! - Esc lets go of the picks;
//! - Up / Down move the pick, Shift+Up / Down stretch it;
//! - Delete asks the list to delete what is picked.
//!
//! A view keeps a [`Selection`], hands its rows' keys in display order to
//! [`Selection::click`] / [`on_key`], draws picked rows with
//! `widgets::row_selected` and, with more than one picked, shows [`bar`]
//! ("3 selected · Delete · ✕" plus the list's own bulk actions).

use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, StyledExt as _, h_flex};
use gpui_kit::*;
use std::collections::HashSet;

/// The picked rows of one list, by key.
#[derive(Default, Clone, Debug)]
pub struct Selection {
    keys: HashSet<String>,
    /// Where a Shift range starts.
    anchor: Option<String>,
    /// The row the arrow keys move from (the last one clicked or moved to).
    cursor: Option<String>,
}

/// What a key did to a list.
#[derive(Debug, PartialEq, Eq)]
pub enum KeyAct {
    /// Not a selection key: the list handles it, or nothing does.
    None,
    /// The picks changed (Ctrl+A, Esc).
    Changed,
    /// The pick moved to this row (arrows): show it, scroll to it.
    Moved(String),
    /// Delete was pressed with something picked.
    Delete,
}

impl Selection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// More than one row picked: the bulk bar shows.
    pub fn many(&self) -> bool {
        self.keys.len() > 1
    }

    pub fn contains(&self, key: &str) -> bool {
        self.keys.contains(key)
    }

    /// The picked keys, as a set (for lists that keep ticks this way).
    pub fn keys(&self) -> &HashSet<String> {
        &self.keys
    }

    /// The picks, for lists that change them directly (tick boxes).
    pub fn keys_mut(&mut self) -> &mut HashSet<String> {
        &mut self.keys
    }

    /// The picked keys in the order the list shows them.
    pub fn in_order(&self, order: &[String]) -> Vec<String> {
        order.iter().filter(|k| self.keys.contains(*k)).cloned().collect()
    }

    /// The row the keys act on.
    pub fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }

    /// Picks `key` alone (a plain click, or the list opening on a row).
    pub fn set_one(&mut self, key: impl Into<String>) {
        let key = key.into();
        self.keys.clear();
        self.keys.insert(key.clone());
        self.anchor = Some(key.clone());
        self.cursor = Some(key);
    }

    pub fn clear(&mut self) {
        self.keys.clear();
    }

    /// Picks every row the list shows.
    pub fn select_all(&mut self, order: &[String]) {
        self.keys = order.iter().cloned().collect();
    }

    /// Lets go of rows the list no longer has (deleted, filtered out).
    pub fn retain(&mut self, order: &[String]) {
        let have: HashSet<&String> = order.iter().collect();
        self.keys.retain(|k| have.contains(k));
        if self.anchor.as_ref().is_some_and(|a| !have.contains(a)) {
            self.anchor = None;
        }
        if self.cursor.as_ref().is_some_and(|a| !have.contains(a)) {
            self.cursor = None;
        }
    }

    /// A click on row `key` with `m` held. True for a plain click (the
    /// list then opens or shows that row, as before).
    pub fn click(&mut self, key: &str, m: Modifiers, order: &[String]) -> bool {
        let ctrl = m.control || m.platform;
        match (ctrl, m.shift) {
            (false, false) => {
                self.set_one(key);
                return true;
            }
            (true, false) => {
                if !self.keys.remove(key) {
                    self.keys.insert(key.to_string());
                }
                self.anchor = Some(key.to_string());
            }
            (add, true) => {
                let from = self.anchor.clone().unwrap_or_else(|| key.to_string());
                if !add {
                    self.keys.clear();
                }
                self.keys.extend(range(order, &from, key));
                self.anchor = Some(from);
            }
        }
        self.cursor = Some(key.to_string());
        false
    }

    /// Up (-1) or Down (+1) from the cursor; with `extend` (Shift) the
    /// picks stretch from the anchor instead of moving. The new row.
    pub fn step(&mut self, dir: isize, extend: bool, order: &[String]) -> Option<String> {
        if order.is_empty() {
            return None;
        }
        let at = self.cursor.as_ref().and_then(|c| order.iter().position(|k| k == c));
        let next = match at {
            Some(i) => (i as isize + dir).clamp(0, order.len() as isize - 1) as usize,
            None if dir < 0 => order.len() - 1,
            None => 0,
        };
        let key = order[next].clone();
        if extend {
            let from = self.anchor.clone().unwrap_or_else(|| order[at.unwrap_or(next)].clone());
            self.keys = range(order, &from, &key).collect();
            self.anchor = Some(from);
            self.cursor = Some(key.clone());
        } else {
            self.set_one(key.clone());
        }
        Some(key)
    }
}

/// The keys from `a` to `b` in `order`, either way round.
fn range<'a>(order: &'a [String], a: &str, b: &str) -> impl Iterator<Item = String> + 'a {
    let ia = order.iter().position(|k| k == a);
    let ib = order.iter().position(|k| k == b);
    let (lo, hi) = match (ia, ib) {
        (Some(x), Some(y)) => (x.min(y), x.max(y)),
        (None, Some(y)) | (Some(y), None) => (y, y),
        (None, None) => (1, 0),
    };
    order.iter().enumerate().filter(move |(i, _)| *i >= lo && *i <= hi).map(|(_, k)| k.clone())
}

/// The selection keys of a list: Ctrl+A, Esc, Up/Down (with Shift),
/// Delete. Stops the key going further when it used it.
pub fn on_key(sel: &mut Selection, e: &KeyDownEvent, order: &[String], cx: &mut App) -> KeyAct {
    let m = &e.keystroke.modifiers;
    let ctrl = m.control || m.platform;
    let act = match e.keystroke.key.as_str() {
        "a" if ctrl && !m.shift && !m.alt => {
            sel.select_all(order);
            KeyAct::Changed
        }
        "escape" if !sel.is_empty() && sel.many() => {
            // One row picked is just the one shown; Esc lets go of a group.
            let keep = sel.cursor.clone();
            sel.clear();
            if let Some(k) = keep {
                sel.keys.insert(k);
            }
            KeyAct::Changed
        }
        "up" | "down" if !ctrl && !m.alt => {
            let dir = if e.keystroke.key == "up" { -1 } else { 1 };
            match sel.step(dir, m.shift, order) {
                Some(k) => KeyAct::Moved(k),
                None => KeyAct::None,
            }
        }
        "delete" if !sel.is_empty() => KeyAct::Delete,
        _ => KeyAct::None,
    };
    if act != KeyAct::None {
        cx.stop_propagation();
    }
    act
}

/// The bulk bar: "3 selected", the list's own actions (`extra`), Delete
/// (when the list can) and a clear button. Placed with [`floating`].
pub fn bar(
    id: &'static str,
    n: usize,
    extra: Vec<AnyElement>,
    delete: Option<(SharedString, Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>)>,
    on_clear: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let mut row = h_flex()
        .id(id)
        .px_2()
        .py_1p5()
        .gap_1()
        .items_center()
        .rounded(theme.radius_lg)
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .border_1()
        .border_color(theme.border)
        .shadow_xl()
        .occlude()
        .child(div().text_sm().font_semibold().px_2().child(format!("{n} selected")))
        .child(div().w_px().h_5().bg(theme.border))
        .children(extra);
    if let Some((label, f)) = delete {
        row = row.child(
            Button::new(SharedString::from(format!("{id}-delete")))
                .small()
                .ghost()
                .danger()
                .icon(IconName::Trash)
                .label(label)
                .on_click(move |e, w, cx| f(e, w, cx)),
        );
    }
    row.child(div().w_px().h_5().bg(theme.border))
        .child(
            Button::new(SharedString::from(format!("{id}-clear")))
                .small()
                .ghost()
                .icon(IconName::X)
                .tooltip("Clear selection (Esc)")
                .on_click(on_clear),
        )
        .into_any_element()
}

/// `bar` floated over the bottom of its list (the list needs `.relative()`).
pub fn floating(id: &'static str, bar: AnyElement) -> AnyElement {
    h_flex()
        .absolute()
        .bottom_4()
        .left_0()
        .right_0()
        .justify_center()
        .child(widgets::enter(SharedString::from(format!("{id}-in")), div().child(bar)))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's own `test` macro would shadow `#[test]`.
    use super::Selection;
    use gpui_kit::Modifiers;

    fn order() -> Vec<String> {
        ["a", "b", "c", "d", "e"].iter().map(|s| s.to_string()).collect()
    }
    fn ctrl() -> Modifiers {
        Modifiers { control: true, ..Default::default() }
    }
    fn shift() -> Modifiers {
        Modifiers { shift: true, ..Default::default() }
    }
    fn picked(s: &Selection) -> Vec<String> {
        s.in_order(&order())
    }

    #[test]
    fn clicks_pick_like_a_file_explorer() {
        let o = order();
        let mut s = Selection::new();
        assert!(s.click("b", Modifiers::default(), &o));
        assert_eq!(picked(&s), ["b"]);
        assert!(!s.click("d", ctrl(), &o));
        assert_eq!(picked(&s), ["b", "d"]);
        s.click("d", ctrl(), &o);
        assert_eq!(picked(&s), ["b"]);
        // Shift: from the anchor (the last Ctrl click, d) to here.
        s.click("a", shift(), &o);
        assert_eq!(picked(&s), ["a", "b", "c", "d"]);
        // Shift again from the same anchor replaces the range.
        s.click("e", shift(), &o);
        assert_eq!(picked(&s), ["d", "e"]);
        // Ctrl+Shift adds a range from a new anchor.
        s.click("a", Modifiers::default(), &o);
        s.click("b", Modifiers { control: true, shift: true, ..Default::default() }, &o);
        assert_eq!(picked(&s), ["a", "b"]);
        s.click("d", ctrl(), &o);
        s.click("e", Modifiers { control: true, shift: true, ..Default::default() }, &o);
        assert_eq!(picked(&s), ["a", "b", "d", "e"]);
    }

    #[test]
    fn arrows_move_and_shift_stretches() {
        let o = order();
        let mut s = Selection::new();
        s.click("b", Modifiers::default(), &o);
        assert_eq!(s.step(1, false, &o).as_deref(), Some("c"));
        assert_eq!(picked(&s), ["c"]);
        s.step(1, true, &o);
        s.step(1, true, &o);
        assert_eq!(picked(&s), ["c", "d", "e"]);
        // Past the end stays on the last row.
        s.step(1, true, &o);
        assert_eq!(picked(&s), ["c", "d", "e"]);
        s.step(-1, true, &o);
        s.step(-1, true, &o);
        s.step(-1, true, &o);
        assert_eq!(picked(&s), ["b", "c"]);
    }

    #[test]
    fn select_all_and_retain() {
        let o = order();
        let mut s = Selection::new();
        s.select_all(&o[1..3]);
        assert_eq!(picked(&s), ["b", "c"]);
        s.retain(&["c".to_string(), "e".to_string()]);
        assert_eq!(picked(&s), ["c"]);
        assert!(!s.many());
    }
}
