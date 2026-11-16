//! The History tab of an instance's History dialog: what was removed,
//! updated, rolled back or edited, newest first, each with Undo. Undo
//! downloads a mod again (`hopper::undo`); when that version is gone from
//! its site, a picker offers the ones it has.

use crate::backups_view::BackupsView;
use crate::widgets;
use crate::widgets::Clickable as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::actor::Who;
use hopper::backups::{Change, Saved, Set};
use hopper::undo::Gone;
use std::rc::Rc;

/// The day header a set goes under ("Today", "Yesterday", a date).
fn day_of(when: i64) -> String {
    hopper::time::relative_secs(when)
}

/// Who a person chip stands for: "you", or the friend's (or panel
/// user's) id.
fn person_key(w: &Who) -> String {
    if w.is_you() {
        "you".into()
    } else if w.id.is_empty() {
        format!("n:{}", w.name)
    } else {
        w.id.clone()
    }
}

/// The size of the face before a row.
const FACE: f32 = 1.5;
/// The gap between faces when several people made one change.
const FACE_GAP: f32 = 0.125;
/// Room for this many faces before a row; more people fold into "+n" in
/// the last one.
const FACE_SLOTS: usize = 2;
/// The face column: one width on every row, so every row's text starts at
/// the same place however many people made the change. Faces sit side by
/// side, not overlapped: the initials on tinted tiles are see-through and
/// overlapping hid one letter under the next.
const LEAD_W: f32 = FACE_SLOTS as f32 * FACE + (FACE_SLOTS - 1) as f32 * FACE_GAP;

/// How many faces a row shows, and how many people the "+n" after them
/// stands for.
fn face_split(people: usize) -> (usize, usize) {
    if people <= FACE_SLOTS { (people, 0) } else { (FACE_SLOTS - 1, people - (FACE_SLOTS - 1)) }
}

/// How far an entry's own rows sit in: under the head's text, past the
/// face column (or chevron) and the gap (`px_2` + `gap_2`).
fn set_indent(faces: bool) -> f32 {
    (if faces { LEAD_W } else { FACE }) + 1.
}

/// Someone with no face to show (not signed in to Minecraft, or a Hopper
/// Server user): their initial on a tinted tile, yours in the primary tint,
/// as Hopper Server's History shows them.
fn letter_face(w: &Who, size: Rems, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let color = if w.is_you() { theme.primary } else { theme.muted_foreground };
    let initial: String = w.label().chars().find(|c| c.is_alphanumeric()).into_iter().flat_map(char::to_uppercase).collect();
    div()
        .flex_none()
        .size(size)
        .rounded(theme.radius)
        .bg(color.opacity(widgets::alpha::TINT * 2.))
        .text_color(crate::theme::ink(color))
        .text_size(rems(size.0 * 0.5))
        .font_semibold()
        .flex()
        .items_center()
        .justify_center()
        .child(initial)
        .into_any_element()
}

/// One person in History's filter: a tinted chip (`widgets::chip`), the
/// picked one in the primary tint, their face before the name.
fn who_chip(id: impl Into<ElementId>, face: Option<AnyElement>, label: String, on: bool, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    let wash = theme.list_hover;
    // A filter is a control: one height for every chip (Hopper Server's
    // 26px), with or without a face, and the 12px label.
    let chip = widgets::chip(if on { theme.primary } else { theme.muted_foreground })
        .small()
        .h(rems(1.625))
        .text_xs()
        .child(h_flex().gap_1().items_center().children(face).child(label));
    div()
        .id(id.into())
        .flex_none()
        .rounded(crate::theme::radius_sm())
        .clickable(cx)
        .when(!on, |d| d.hover(move |s| s.bg(wash)))
        .child(chip)
}

/// "14:02".
fn hm(when: i64) -> String {
    let c = hopper::time::clock(when);
    c.get(..5).unwrap_or(&c).to_string()
}

/// One row of History's list.
#[derive(Clone, PartialEq)]
pub(crate) enum Item {
    /// The people filter.
    Filter,
    /// A day header; `true` for the first (no space above it).
    Day(String, bool),
    /// An entry, or the head of one that touched several files.
    Set(usize),
    /// One file of an open multi-file entry.
    File(usize, usize),
    /// The configs of an open pack update, a fold of their own.
    Configs(usize),
}

/// Where a pack update's configs fold is noted as open (beside its own).
fn configs_key(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("#configs")
}

/// "368 configs", or "12 files" when they aren't all configs.
fn configs_label(set: &Set, rest: &[usize]) -> String {
    set.rest_label(rest)
}

/// Projects by file name without its version (see `BackupsView::project_of`).
pub(crate) fn stems_of(projects: &std::collections::HashMap<String, (u64, String)>) -> std::collections::HashMap<String, (u64, String)> {
    let mut out = std::collections::HashMap::new();
    for (f, p) in projects {
        out.entry(hopper::scan::strip_version_suffix(f)).or_insert_with(|| p.clone());
    }
    out
}

/// The shared-settings notes of friends' config changes, by entry file:
/// read once with History instead of from disk on every frame.
pub(crate) fn notes_of(sets: &[Set]) -> std::collections::HashMap<std::path::PathBuf, hopper::backups::SettingsNote> {
    let mut out = std::collections::HashMap::new();
    for f in sets.iter().flat_map(|s| &s.files) {
        if f.change == Change::Edited
            && f.who.as_ref().is_some_and(|w| !w.is_you())
            && let Some(n) = f.path.parent().and_then(|d| hopper::backups::settings_note(d, &f.restore_to))
        {
            out.insert(f.path.clone(), n);
        }
    }
    out
}

impl BackupsView {
    /// History, grouped by day. A list that builds only the rows in view: a
    /// pack update leaves hundreds of entries, and building them all made
    /// every scroll frame take as long as laying out the whole history.
    pub(crate) fn history_body(&self, cx: &mut Context<Self>) -> AnyElement {
        if !self.sets_loaded {
            return widgets::skeleton_rows(4, rems(2.5), cx).into_any_element();
        }
        if self.sets.is_empty() {
            return widgets::empty_state(
                IconName::Undo2,
                "Nothing yet",
                Some("Mods you remove, update or roll back, and configs Hopper changes, show up here with Undo.".into()),
                cx,
            )
            .into_any_element();
        }
        let view = cx.entity().downgrade();
        list(self.hlist.clone(), move |ix, _, cx| {
            view.update(cx, |this, cx| this.history_item(ix, cx)).unwrap_or_else(|_| div().into_any_element())
        })
        .size_full()
        .into_any_element()
    }

    /// Works out the list's rows again after History, the person filter or
    /// an open fold changed. Only the rows that changed are measured again,
    /// so the scroll spot stays.
    pub(crate) fn sync_items(&mut self) {
        let people = self.people();
        let mut items: Vec<(String, Item)> = Vec::new();
        if people.len() > 1 {
            items.push(("filter".into(), Item::Filter));
        }
        let mut day = String::new();
        for (i, set) in self.sets.iter().enumerate() {
            if !self.shown(set) {
                continue;
            }
            let d = day_of(set.when);
            if d != day {
                let first = day.is_empty();
                items.push((format!("day:{first}:{d}"), Item::Day(d.clone(), first)));
                day = d;
            }
            let dir = set.dir.to_string_lossy();
            items.push((format!("set:{dir}"), Item::Set(i)));
            if set.pack.is_some() {
                // Open: its mods, then its configs in a fold of their own.
                if self.open.contains(&set.dir) {
                    let (mods, rest) = set.pack_parts();
                    items.extend(mods.iter().map(|&j| (format!("file:{j}:{dir}"), Item::File(i, j))));
                    if !rest.is_empty() {
                        items.push((format!("configs:{dir}"), Item::Configs(i)));
                        if self.open.contains(&configs_key(&set.dir)) {
                            items.extend(rest.iter().map(|&j| (format!("file:{j}:{dir}"), Item::File(i, j))));
                        }
                    }
                }
            } else if set.files.len() > 1 && self.open.contains(&set.dir) {
                items.extend((0..set.files.len()).map(|j| (format!("file:{j}:{dir}"), Item::File(i, j))));
            }
        }
        self.hfaces = !people.is_empty();
        self.hpeople = people;
        let old = &self.hitems;
        let pre = old.iter().zip(&items).take_while(|(a, b)| a.0 == b.0).count();
        let max_suf = old.len().min(items.len()) - pre;
        let suf = old.iter().rev().zip(items.iter().rev()).take(max_suf).take_while(|(a, b)| a.0 == b.0).count();
        if pre < old.len() || old.len() != items.len() {
            self.hlist.splice(pre..old.len() - suf, items.len() - pre - suf);
            // Rows not built yet count as an entry's height, not nothing, so
            // the scrollbar's thumb is the right size and in the right place
            // from the first frame (the list shares its state: this sets it).
            let _ = self.hlist.clone().with_uniform_item_height(px(52.));
        }
        self.hitems = items;
    }

    /// Row `ix` of the list. Each carries the 2px gap below it that the rows
    /// had as one column; the files of an open fold sit flush under its head.
    fn history_item(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some((_, item)) = self.hitems.get(ix) else { return div().into_any_element() };
        let el = match item {
            Item::Filter => self.who_filter(&self.hpeople, cx),
            Item::Day(d, first) => div()
                .when(!*first, |d| d.pt_3())
                .pb_1()
                .text_xs()
                .font_semibold()
                .text_color(cx.theme().muted_foreground)
                .child(d.to_uppercase())
                .into_any_element(),
            Item::Set(i) => match self.sets.get(*i) {
                Some(set) => {
                    let row = self.set_rows(*i, set, self.hfaces, cx);
                    return div().when(self.ends_entry(ix, *i), |d| d.pb_0p5()).child(row).into_any_element();
                }
                None => div().into_any_element(),
            },
            Item::File(i, j) => match self.sets.get(*i).and_then(|s| Some((s, s.files.get(*j)?))) {
                Some((set, f)) => {
                    let row = self.entry_row(i * 10_000 + j + 1, f, set.when, Some(set_indent(self.hfaces)), self.hfaces, cx);
                    return div().when(self.ends_entry(ix, *i), |d| d.pb_0p5()).child(row).into_any_element();
                }
                None => div().into_any_element(),
            },
            Item::Configs(i) => match self.sets.get(*i) {
                Some(set) => {
                    let row = self.configs_row(*i, set, cx);
                    return div().when(self.ends_entry(ix, *i), |d| d.pb_0p5()).child(row).into_any_element();
                }
                None => div().into_any_element(),
            },
        };
        div().pb_0p5().child(el).into_any_element()
    }

    /// Whether row `ix` is the last of entry `i` (its head, files and
    /// folds): the entry's gap goes below it.
    fn ends_entry(&self, ix: usize, i: usize) -> bool {
        !matches!(self.hitems.get(ix + 1).map(|x| &x.1), Some(Item::File(k, _) | Item::Configs(k)) if *k == i)
    }

    /// Whether a set shows under the person filter.
    fn shown(&self, set: &Set) -> bool {
        match &self.who_pick {
            None => true,
            Some(k) => set.files.iter().any(|f| f.who.as_ref().is_some_and(|w| &person_key(w) == k)),
        }
    }

    /// Everyone with a change in History, you first.
    fn people(&self) -> Vec<Who> {
        let mut out: Vec<Who> = Vec::new();
        for w in self.sets.iter().flat_map(|s| s.people()) {
            if !out.iter().any(|o| person_key(o) == person_key(&w)) {
                out.push(w);
            }
        }
        out.sort_by_key(|w| !w.is_you());
        out
    }

    /// "Everyone · You · Mia · Robin": whose changes History shows. Tinted
    /// chips, the picked one in the primary tint, as Hopper Server's.
    fn who_filter(&self, people: &[Who], cx: &mut Context<Self>) -> AnyElement {
        let mut row = h_flex().gap_1p5().flex_wrap().pb_2().child(
            who_chip("hist-who-all", None, "Everyone".into(), self.who_pick.is_none(), cx)
                .on_click(cx.listener(|this, _, _, cx| this.pick_who(None, cx))),
        );
        for (i, w) in people.iter().enumerate() {
            let k = person_key(w);
            let on = self.who_pick.as_ref() == Some(&k);
            let face = self.who_face(w, rems(1.), cx);
            row = row.child(
                who_chip(("hist-who", i), Some(face), w.label(), on, cx)
                    .on_click(cx.listener(move |this, _, _, cx| this.pick_who(Some(k.clone()), cx))),
            );
        }
        row.into_any_element()
    }

    fn pick_who(&mut self, k: Option<String>, cx: &mut Context<Self>) {
        // Picked rows the filter hides would still be undone: start over.
        self.sel.clear();
        self.who_pick = k;
        self.sync_items();
        cx.notify();
    }

    /// The face of whoever made a change: a friend's or panel user's
    /// Minecraft head when this PC knows it, your account's for you, a
    /// plain person shape otherwise.
    fn who_face(&self, w: &Who, size: Rems, cx: &mut Context<Self>) -> AnyElement {
        let s = self.store.read(cx);
        let mc = if !w.mc.is_empty() {
            w.mc.clone()
        } else if w.is_you() {
            s.accounts().active().map(|a| a.id.clone()).unwrap_or_default()
        } else {
            s.mailbox_friends()
                .iter()
                .find(|f| f.id == w.id)
                .map(|f| f.mc_id.clone())
                .or_else(|| s.mailbox_groups().iter().flat_map(|g| &g.members).find(|m| m.id == w.id).map(|m| m.mc_id.clone()))
                .unwrap_or_default()
        };
        if mc.is_empty() {
            return letter_face(w, size, cx);
        }
        crate::friends::face(&self.store, &mc, &w.label(), size, cx)
    }

    /// The group a friend's change came through, by name.
    fn group_name(&self, w: &Who, cx: &App) -> Option<String> {
        (!w.group.is_empty())
            .then(|| self.store.read(cx).mailbox_groups().iter().find(|g| g.id == w.group).map(|g| g.name.clone()))
            .flatten()
            .filter(|n| !n.is_empty())
    }

    /// Row keys in display order (what Shift-click ranges run over).
    fn order(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (i, set) in self.sets.iter().enumerate() {
            if !self.shown(set) {
                continue;
            }
            if set.pack.is_some() {
                if self.open.contains(&set.dir) {
                    let (mods, rest) = set.pack_parts();
                    out.extend(mods.iter().map(|j| (i * 10_000 + j + 1).to_string()));
                    if self.open.contains(&configs_key(&set.dir)) {
                        out.extend(rest.iter().map(|j| (i * 10_000 + j + 1).to_string()));
                    }
                }
                continue;
            }
            match set.files.as_slice() {
                [_] => out.push((i * 10_000).to_string()),
                files if self.open.contains(&set.dir) => out.extend((0..files.len()).map(|j| (i * 10_000 + j + 1).to_string())),
                _ => {}
            }
        }
        out
    }

    /// The picked files, newest first (open or folded).
    fn picked_files(&self) -> Vec<Saved> {
        let mut out = Vec::new();
        for (i, set) in self.sets.iter().enumerate() {
            for (j, f) in set.files.iter().enumerate() {
                // A pack update's files are always rows of its fold.
                let key = if set.files.len() == 1 && set.pack.is_none() { i * 10_000 } else { i * 10_000 + j + 1 };
                if self.sel.contains(&key.to_string()) && f.change.undoable() {
                    out.push(f.clone());
                }
            }
        }
        out
    }

    /// A drag over a row: the row it started on and this one get picked.
    fn sweep_over(&mut self, keys: &[String], cx: &mut Context<Self>) {
        let Some(start) = self.hstart.clone() else { return };
        if start == keys {
            return;
        }
        let set = self.sel.keys_mut();
        let before = set.len();
        set.extend(start);
        set.extend(keys.iter().cloned());
        if self.sel.len() != before {
            cx.notify();
        }
    }

    /// "3 selected · Undo 3 · ✕", over the bottom of the list.
    pub(crate) fn history_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let n = self.sel.len();
        let undo = Button::new("hist-sel-undo")
            .small()
            .outline()
            .label(format!("Undo {n}"))
            .disabled(self.busy.is_some())
            .on_click(cx.listener(|this, _, window, cx| {
                let files = this.picked_files();
                this.sel.clear();
                this.undo(files, window, cx);
            }))
            .into_any_element();
        let bar = crate::selection::bar(
            "hist-sel",
            n,
            vec![undo],
            None,
            {
                let view = cx.entity();
                move |_, _, cx| {
                    view.update(cx, |this, cx| {
                        this.sel.clear();
                        cx.notify();
                    })
                }
            },
            cx,
        );
        crate::selection::floating("hist-sel", bar)
    }

    /// Makes a row take part in picking: Ctrl / Shift click picks, and a press
    /// followed by a drag over other rows picks them all.
    fn pickable(&self, el: Stateful<Div>, keys: Vec<String>, plain: Option<Rc<dyn Fn(&mut Self, &mut Context<Self>)>>, cx: &mut Context<Self>) -> Stateful<Div> {
        let view = cx.entity();
        let k2 = keys.clone();
        let k3 = keys.clone();
        let el = crate::drag_pick::paint_row(el, &self.hpaint, "hist", move |_, _, cx| {
            view.update(cx, |this, cx| this.sweep_over(&k2, cx))
        });
        el.capture_any_mouse_down(cx.listener(move |this, e: &MouseDownEvent, _, _| {
            let m = e.modifiers;
            if e.button == MouseButton::Left && !(m.control || m.platform || m.shift) {
                this.hpaint.start(&"hist".into(), 1);
                this.hstart = Some(k3.clone());
            }
        }))
        .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
            let m = e.modifiers();
            if m.control || m.platform || m.shift {
                let order = this.order();
                if keys.len() == 1 {
                    this.sel.click(&keys[0], m, &order);
                } else if keys.iter().all(|k| this.sel.contains(k)) {
                    for k in &keys {
                        this.sel.keys_mut().remove(k);
                    }
                } else {
                    this.sel.keys_mut().extend(keys.iter().cloned());
                }
                cx.notify();
            } else if let Some(f) = &plain {
                f(this, cx);
            } else {
                this.sel.set_one(keys[0].clone());
                cx.notify();
            }
        }))
    }

    /// What starts an entry of several files: whoever made it (several
    /// people stacked), or its chevron when History names nobody.
    fn set_lead(&self, set: &Set, faces: bool, chevron: gpui_kit::component::Icon, cx: &mut Context<Self>) -> AnyElement {
        if !faces {
            div().size(rems(FACE)).flex_none().flex().items_center().justify_center().child(chevron).into_any_element()
        } else {
            self.face_column(SharedString::from(format!("hist-more:{}", set.dir.display())), &set.people(), cx)
        }
    }

    /// The faces before a row, in a column of one width for every row: up
    /// to two faces, or the first and "+n" for the rest (hover names them).
    /// Empty when History doesn't know who made the change.
    fn face_column(&self, key: SharedString, people: &[Who], cx: &mut Context<Self>) -> AnyElement {
        let (shown, more) = face_split(people.len());
        let faces: Vec<AnyElement> = people.iter().take(shown).map(|w| self.who_face(w, rems(FACE), cx)).collect();
        let more = (more > 0).then(|| {
            let theme = cx.theme();
            let muted = theme.muted_foreground;
            let names: SharedString = people[shown..].iter().map(Who::label).collect::<Vec<_>>().join(", ").into();
            div()
                .id(ElementId::Name(key))
                .flex_none()
                .size(rems(FACE))
                .rounded(theme.radius)
                .bg(muted.opacity(widgets::alpha::TINT * 2.))
                .text_color(crate::theme::ink(muted))
                .text_size(rems(FACE * 0.45))
                .font_semibold()
                .flex()
                .items_center()
                .justify_center()
                .whitespace_nowrap()
                .child(format!("+{more}"))
                .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(names.clone()).build(w, cx))
        });
        h_flex().flex_none().w(rems(LEAD_W)).gap(rems(FACE_GAP)).children(faces).children(more).into_any_element()
    }

    /// A pack update: one row with what it did to how many mods, and its
    /// configs. Open, its mods are listed first and its configs stay in a
    /// fold of their own, so the mods aren't lost among hundreds of configs.
    fn pack_rows(&self, i: usize, set: &Set, pack: &hopper::backups::PackMark, faces: bool, cx: &mut Context<Self>) -> AnyElement {
        let open = self.open.contains(&set.dir);
        let dir = set.dir.clone();
        let all: Vec<Saved> = set.files.iter().filter(|f| f.change.undoable()).cloned().collect();
        let undoable = !all.is_empty();
        let mark = pack.clone();
        let busy = self.busy.is_some();
        let kids: Vec<String> = (0..set.files.len()).map(|j| (i * 10_000 + j + 1).to_string()).collect();
        let all_picked = kids.iter().all(|k| self.sel.contains(k));
        let head = h_flex()
            .id(("hist-set", i))
            .gap_2()
            .px_2()
            .py_1p5()
            .rounded(cx.theme().radius)
            .hover(|s| widgets::row_hover(s, cx))
            .clickable(cx);
        let head = self.pickable(
            head,
            kids,
            Some(Rc::new(move |this: &mut Self, cx: &mut Context<Self>| {
                if !this.open.remove(&dir) {
                    this.open.insert(dir.clone());
                }
                this.sync_items();
                cx.notify();
            })),
            cx,
        );
        let head = if all_picked { widgets::row_selected(head, cx) } else { head };
        let chevron = gpui_kit::component::Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).small();
        let lead = self.set_lead(set, faces, chevron.clone(), cx);
        let muted = cx.theme().muted_foreground;
        // "Updated pack All the Mods 10 to 1.80.0" ("Rolled back pack ..."
        // once Undo all put it back, in the muted colour of put-back rows),
        // the pack's name cut short first when the row is narrow.
        let title = h_flex()
            .gap_1()
            .min_w_0()
            .text_sm()
            .child(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .text_color(change_color(Change::Updated, !pack.undone, cx))
                    .child(if pack.undone { "Rolled back pack" } else { "Updated pack" }),
            )
            .child(div().min_w_0().truncate().whitespace_nowrap().font_semibold().child(pack.name.clone()))
            .when(!pack.version.is_empty(), |r| r.child(div().flex_none().whitespace_nowrap().child(format!("to {}", pack.version))))
            .when(faces, |r| r.child(div().flex_none().text_color(muted).child(chevron)));
        // What it did, counted: a chip per kind, its configs last.
        let (_, rest) = set.pack_parts();
        let mut chips: Vec<AnyElement> = set
            .pack_counts()
            .into_iter()
            .map(|(c, n)| widgets::chip(change_color(c, true, cx)).child(format!("{n} {}", c.verb())).into_any_element())
            .collect();
        if !rest.is_empty() {
            chips.push(widgets::chip(change_color(Change::Edited, true, cx)).child(configs_label(set, &rest)).into_any_element());
        }
        head.child(lead)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(title)
                    .child(h_flex().gap_1().min_w_0().overflow_hidden().children(chips)),
            )
            .child(widgets::caption(hm(set.when), cx).text_xs())
            .child(
                widgets::row_actions(("hist-set-actions", i)).when(undoable, |r| {
                    r.child(
                        Button::new(("hist-undo-all", i))
                            .xsmall()
                            .outline()
                            .label("Undo all")
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, window, cx| this.undo_set(all.clone(), Some(mark.clone()), window, cx))),
                    )
                }),
            )
            .into_any_element()
    }

    /// An open pack update's configs: "Changed 368 configs", a fold of its
    /// own with its own Undo all.
    fn configs_row(&self, i: usize, set: &Set, cx: &mut Context<Self>) -> AnyElement {
        let (_, rest) = set.pack_parts();
        let key = configs_key(&set.dir);
        let open = self.open.contains(&key);
        let all: Vec<Saved> = rest.iter().map(|&j| &set.files[j]).filter(|f| f.change.undoable()).cloned().collect();
        let undoable = !all.is_empty();
        let busy = self.busy.is_some();
        let kids: Vec<String> = rest.iter().map(|j| (i * 10_000 + j + 1).to_string()).collect();
        let all_picked = kids.iter().all(|k| self.sel.contains(k));
        let names = rest.iter().take(3).map(|&j| set.files[j].title()).collect::<Vec<_>>().join(", ") + if rest.len() > 3 { "…" } else { "" };
        let row = h_flex()
            .id(("hist-configs", i))
            .gap_2()
            .px_2()
            .py_1p5()
            // Under the pack's text, like its mods.
            .pl(rems(set_indent(self.hfaces)))
            .rounded(cx.theme().radius)
            .hover(|s| widgets::row_hover(s, cx))
            .clickable(cx);
        let row = self.pickable(
            row,
            kids,
            Some(Rc::new(move |this: &mut Self, cx: &mut Context<Self>| {
                if !this.open.remove(&key) {
                    this.open.insert(key.clone());
                }
                this.sync_items();
                cx.notify();
            })),
            cx,
        );
        let row = if all_picked { widgets::row_selected(row, cx) } else { row };
        let chevron = gpui_kit::component::Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).small();
        let title = h_flex()
            .gap_1p5()
            .text_sm()
            .child(div().text_color(change_color(Change::Edited, true, cx)).child("Changed"))
            .child(configs_label(set, &rest))
            .child(div().text_color(cx.theme().muted_foreground).child(chevron));
        row.child(v_flex().flex_1().min_w_0().child(title).child(widgets::caption(names, cx).text_xs().truncate()))
            .child(
                widgets::row_actions(("hist-configs-actions", i)).when(undoable, |r| {
                    r.child(
                        Button::new(("hist-undo-configs", i))
                            .xsmall()
                            .outline()
                            .label("Undo all")
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, window, cx| this.undo(all.clone(), window, cx))),
                    )
                }),
            )
            .into_any_element()
    }

    /// One action: a row, or a fold of rows when it touched several files.
    /// `faces`: History names people, so each row starts with one.
    fn set_rows(&self, i: usize, set: &Set, faces: bool, cx: &mut Context<Self>) -> AnyElement {
        if let Some(pack) = &set.pack {
            return self.pack_rows(i, set, pack, faces, cx);
        }
        if let [f] = set.files.as_slice() {
            return self.entry_row(i * 10_000, f, set.when, None, faces, cx).into_any_element();
        }
        let open = self.open.contains(&set.dir);
        let dir = set.dir.clone();
        // A whole server moved or copied can't be undone from here.
        let all: Vec<Saved> = set.files.iter().filter(|f| f.change.undoable()).cloned().collect();
        let undoable = !all.is_empty();
        let n = set.files.len();
        let mods = set.files.iter().filter(|f| f.src.is_some()).count();
        let what = if mods == n { format!("{n} mods") } else { format!("{n} files") };
        // "Removed 3 mods" when they all went the same way.
        let same = set.files.iter().all(|f| f.change == set.files[0].change);
        let label = if same { set.files[0].change.label() } else { "Changed" };
        let busy = self.busy.is_some();
        let kids: Vec<String> = (0..n).map(|j| (i * 10_000 + j + 1).to_string()).collect();
        let all_picked = kids.iter().all(|k| self.sel.contains(k));
        let head = h_flex()
            .id(("hist-set", i))
            .gap_2()
            .px_2()
            .py_1p5()
            .rounded(cx.theme().radius)
            .hover(|s| widgets::row_hover(s, cx))
            .clickable(cx);
        let head = self.pickable(
            head,
            kids,
            Some(Rc::new(move |this: &mut Self, cx: &mut Context<Self>| {
                if !this.open.remove(&dir) {
                    this.open.insert(dir.clone());
                }
                this.sync_items();
                cx.notify();
            })),
            cx,
        );
        let head = if all_picked { widgets::row_selected(head, cx) } else { head };
        let chevron = gpui_kit::component::Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).small();
        let names = set.files.iter().take(3).map(Saved::title).collect::<Vec<_>>().join(", ") + if n > 3 { "…" } else { "" };
        // Who did it, when History knows: "Updated from Robin's All the
        // Mods 10", their name in bold, and what it came to below.
        let headline = faces.then(|| set.headline()).flatten();
        let people = set.people();
        let lead = self.set_lead(set, faces, chevron.clone(), cx);
        let title = match &headline {
            Some(h) => {
                let who = set.who().or(people.first()).map(Who::label).unwrap_or_default();
                let (a, b) = match h.find(&who).filter(|_| !who.is_empty()) {
                    Some(at) => (&h[..at], &h[at + who.len()..]),
                    None => (h.as_str(), ""),
                };
                let found = a.len() < h.len();
                h_flex()
                    .gap_0()
                    .min_w_0()
                    .text_sm()
                    .child(div().flex_none().whitespace_nowrap().child(a.to_string()))
                    .when(found, |r| r.child(div().flex_none().font_semibold().child(who.clone())))
                    .child(div().min_w_0().truncate().whitespace_nowrap().child(b.to_string()))
                    .child(div().flex_none().pl_1().text_color(cx.theme().muted_foreground).child(chevron))
            }
            None => h_flex()
                .gap_1p5()
                .text_sm()
                .child(div().text_color(change_color(set.files[0].change, same, cx)).child(label))
                .child(what)
                .when(faces, |r| r.child(div().text_color(cx.theme().muted_foreground).child(chevron))),
        };
        // The counts, unless the headline already says them ("You turned
        // off 2 mods").
        let counts = headline.is_some() && (!same || set.who().is_some_and(|w| !w.pack.is_empty()));
        let caption = if counts { format!("{} · {names}", set.summary()) } else { names };
        // Its files, when open, are rows of their own in the list.
        head.child(lead)
            .child(v_flex().flex_1().min_w_0().child(title).child(widgets::caption(caption, cx).text_xs().truncate()))
            .child(widgets::caption(hm(set.when), cx).text_xs())
            .child(
                widgets::row_actions(("hist-set-actions", i)).when(undoable, |r| {
                    r.child(
                        Button::new(("hist-undo-all", i))
                            .xsmall()
                            .outline()
                            .label("Undo all")
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, window, cx| this.undo(all.clone(), window, cx))),
                    )
                }),
            )
            .into_any_element()
    }

    /// One file: what happened to it, its versions, when, and Undo.
    /// `faces`: rows start with whoever made the change ("Mia turned off
    /// Sodium"); entries from before History noted that keep a blank there.
    fn entry_row(&self, key: usize, f: &Saved, when: i64, nest: Option<f32>, faces: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let nested = nest.is_some();
        let (pid, icon, name) = match &f.src {
            Some(r) => {
                let known = self.project_of(&f.name, cx);
                let icon = r.addon.as_ref().map(|a| a.icon.clone()).filter(|s| !s.is_empty()).or_else(|| known.clone().map(|k| k.1)).unwrap_or_default();
                let name = if r.name.is_empty() { f.name.clone() } else { r.name.clone() };
                (r.project_id, icon, name)
            }
            None => {
                let (pid, icon) = self.project_of(&f.name, cx).unwrap_or_default();
                (pid, icon, f.name.clone())
            }
        };
        // A friend's shared settings: named after the mod, with what changed.
        let note = self.notes.get(&f.path);
        let name = match note {
            Some(n) if !n.mod_name.is_empty() => format!("{} settings", n.mod_name),
            _ => name,
        };
        let file = f.clone();
        let busy = self.busy.is_some();
        let picked = self.sel.contains(&key.to_string());
        let row = h_flex()
            .id(("hist-row", key))
            .gap_2()
            .px_2()
            .py_1p5()
            // Under the head's text: past its face (or chevron) and gap.
            .when_some(nest, |r, pl| r.pl(rems(pl)))
            .rounded(cx.theme().radius)
            .hover(|s| widgets::row_hover(s, cx))
            .clickable(cx);
        let row = self.pickable(row, vec![key.to_string()], None, cx);
        let row = if picked { widgets::row_selected(row, cx) } else { row };
        let who = f.who.as_ref().filter(|_| !nested);
        let lead = (faces && !nested).then(|| {
            let people: Vec<Who> = who.cloned().into_iter().collect();
            self.face_column(SharedString::from(format!("hist-more:{}", f.path.display())), &people, cx)
        });
        let what = match who {
            Some(w) => h_flex()
                .flex_none()
                .gap_1p5()
                .child(div().font_semibold().child(w.label()))
                .child(div().text_color(cx.theme().muted_foreground).child(f.verb())),
            None => h_flex().flex_none().child(div().text_color(change_color(f.change, true, cx)).child(f.change.label())),
        };
        let mut detail = match note {
            Some(n) => n
                .changes
                .iter()
                .map(|c| {
                    // Switches read as on / off, like the settings view shows them.
                    let say = |v: &str| match v {
                        "true" => "on".to_string(),
                        "false" => "off".to_string(),
                        v => v.to_string(),
                    };
                    format!("{}: {} to {}", hopper::configedit::label(&c.key), say(&c.was), say(&c.now))
                })
                .collect::<Vec<_>>()
                .join(" · "),
            None => f.detail(),
        };
        if let Some(g) = who.and_then(|w| self.group_name(w, cx)) {
            detail = if detail.is_empty() { format!("group {g}") } else { format!("{detail} · group {g}") };
        }
        row.children(lead)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .min_w_0()
                            .text_sm()
                            .child(what)
                            .child(self.mod_cell(ElementId::Name(format!("hist-mod:{key}").into()), name, pid, &icon, true, cx)),
                    )
                    .child(widgets::caption(detail, cx).text_xs().truncate()),
            )
            .when(!nested, |r| r.child(widgets::caption(hm(when), cx).text_xs()))
            .when(file.change.undoable(), |r| {
                r.child(
                    Button::new(("hist-undo", key))
                        .xsmall()
                        .outline()
                        .label("Undo")
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, window, cx| this.undo(vec![file.clone()], window, cx))),
                )
            })
    }

    /// Undoes entries, newest set first. A mod whose version its site no
    /// longer has opens the version picker.
    pub(crate) fn undo(&mut self, files: Vec<Saved>, window: &mut Window, cx: &mut Context<Self>) {
        self.undo_set(files, None, window, cx)
    }

    /// [`Self::undo`]; with `pack`, the whole pack update is being undone
    /// (Undo all on its row): when everything in it came back, the pack's own
    /// record goes back too (pack version, loader, Minecraft version), so
    /// the update is offered again.
    fn undo_set(&mut self, files: Vec<Saved>, pack: Option<hopper::backups::PackMark>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(t) = self.panel.clone() {
            return self.undo_panel(t, files, pack, window, cx);
        }
        let root = self.root.clone();
        let downloads = files.iter().any(|f| f.download);
        let roll_back = all_updates(&files);
        let Some(slot) = queue_undo(self.store.read(cx), &root, files, pack) else {
            window.push_notification(Notification::error("That instance is gone."), cx);
            return;
        };
        self.busy = Some(undo_busy(downloads, roll_back));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let got = crate::store::work_done(cx.background_executor().clone(), slot).await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                // Cancelled while it waited: nothing changed.
                let Some(UndoDone { done, gone, website, errors, pack_said }) = got else {
                    cx.notify();
                    return;
                };
                this.after_undo(&done, roll_back, window, cx);
                if let Some((name, r)) = pack_said {
                    this.say_pack(&name, &r, window, cx);
                }
                for e in errors {
                    window.push_notification(Notification::error(e), cx);
                }
                for (f, w) in website {
                    get_from_website(&this.store, &this.root, f, w, window, cx);
                }
                if let Some((f, g)) = gone {
                    this.pick_version(f, g, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// The time of the History entry a file is in (how a panel names it).
    fn when_of(&self, f: &Saved) -> Option<i64> {
        self.sets.iter().find(|s| s.files.contains(f)).map(|s| s.when)
    }

    /// Undo on a panel server: the panel does the same per file.
    fn undo_panel(
        &mut self,
        t: crate::panel_tabs::Target,
        files: Vec<Saved>,
        pack: Option<hopper::backups::PackMark>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let roll_back = all_updates(&files);
        let picked: Vec<(i64, Saved)> = files.into_iter().filter_map(|f| Some((self.when_of(&f)?, f))).collect();
        let downloads = picked.iter().any(|(_, f)| f.download);
        self.busy = Some(undo_busy(downloads, roll_back));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let (done, gone, errors, restart, pack_said) = cx
                .background_spawn(async move {
                    let mut done: Vec<String> = Vec::new();
                    let mut gone: Option<(Saved, Gone)> = None;
                    let mut errors = Vec::new();
                    let mut restart = false;
                    let mut pack_said = None;
                    // A whole pack update is one request without a name: the
                    // panel undoes the entry and, when all of it came back,
                    // puts the pack's record back too.
                    let requests: Vec<(Saved, serde_json::Value)> = match (&pack, picked.first()) {
                        (Some(_), Some((when, f))) => vec![(f.clone(), serde_json::json!({ "when": when }))],
                        _ => picked.iter().map(|(when, f)| (f.clone(), serde_json::json!({ "when": when, "name": f.name }))).collect(),
                    };
                    for (f, body) in &requests {
                        match t.client.post(&t.sp("/mod-backups/undo"), body.clone()) {
                            Ok(v) => {
                                if let Some(m) = &pack {
                                    pack_said = hopper::undo::PackRestore::from_json(&v["pack"]).map(|r| (m.name.clone(), r));
                                }
                                restart |= v["restart_needed"].as_bool().unwrap_or(false);
                                done.extend(v["restored"].as_array().into_iter().flatten().filter_map(|n| n.as_str().map(str::to_string)));
                                errors.extend(v["errors"].as_array().into_iter().flatten().filter_map(|e| e.as_str().map(str::to_string)));
                                if gone.is_none()
                                    && let Some(g) = gone_of(&v["gone"])
                                {
                                    gone = Some((f.clone(), g));
                                }
                            }
                            Err(e) => errors.push(format!("{}: {e:#}", f.name)),
                        }
                    }
                    (done, gone, errors, restart, pack_said)
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                let paths: Vec<std::path::PathBuf> = done.iter().map(std::path::PathBuf::from).collect();
                this.say_put_back(&paths, restart, roll_back, window, cx);
                if let Some((name, r)) = pack_said {
                    this.say_pack(&name, &r, window, cx);
                }
                this.changed(cx);
                for e in errors {
                    window.push_notification(Notification::error(e), cx);
                }
                if let Some((f, g)) = gone {
                    this.pick_version(f, g, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// "Rolled back X" for an undone update, "Put back X" for a removal or a
    /// config edit, "3 files" counted; with a restart hint for a running
    /// server.
    fn say_put_back(&self, done: &[std::path::PathBuf], restart: bool, roll_back: bool, window: &mut Window, cx: &mut Context<Self>) {
        let hint = if restart { ". Restart the server to load it." } else { "" };
        let verb = if roll_back { "Rolled back" } else { "Put back" };
        match done {
            [] => {}
            [one] => {
                let n = one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                window.push_notification(Notification::success(format!("{verb} {n}{hint}")), cx)
            }
            many => window.push_notification(Notification::success(format!("{verb} {} files{hint}", many.len())), cx),
        }
    }

    /// What happened to the pack's own record after Undo all on its update.
    fn say_pack(&self, name: &str, r: &hopper::undo::PackRestore, window: &mut Window, cx: &mut Context<Self>) {
        use hopper::undo::PackRestore;
        match r {
            PackRestore::Restored { version } if !version.is_empty() => {
                window.push_notification(Notification::success(format!("{name} is back at {version}.")), cx)
            }
            PackRestore::Restored { .. } => window.push_notification(Notification::success(format!("{name} is back at its old version.")), cx),
            PackRestore::Moved { version } if !version.is_empty() => window.push_notification(
                Notification::warning(format!("{name} was updated again after this; its version stays {version}.")),
                cx,
            ),
            PackRestore::Moved { .. } => window.push_notification(
                Notification::warning(format!("{name} was updated again after this; its version stays as it is.")),
                cx,
            ),
            PackRestore::Unknown => {}
        }
        // The pack is at its old version again, so its update is offered again.
        if matches!(r, PackRestore::Restored { .. }) {
            self.store.read(cx).check_pack_updates();
        }
    }

    /// Tells the store and the user what came back.
    fn after_undo(&mut self, done: &[std::path::PathBuf], roll_back: bool, window: &mut Window, cx: &mut Context<Self>) {
        undone(&self.store, &self.root, done, cx);
        self.say_put_back(done, false, roll_back, window, cx);
        self.changed(cx);
    }

    /// The version this entry was is gone from its site: offers the ones it
    /// has, the closest picked.
    fn pick_version(&mut self, f: Saved, g: Gone, window: &mut Window, cx: &mut Context<Self>) {
        if g.choices.is_empty() {
            let msg = format!("{}. It has no other version for this Minecraft and loader, so it can't be rolled back.", g);
            window.push_notification(Notification::error(format!("{}: {msg}", g.name)), cx);
            return;
        }
        let view = cx.entity();
        let picked = Rc::new(std::cell::Cell::new(g.closest));
        let pid = f.src.as_ref().map_or(0, |r| r.project_id);
        let icon = f.src.as_ref().and_then(|r| r.addon.as_ref()).map(|a| a.icon.clone()).unwrap_or_default();
        window.open_dialog(cx, move |dialog, window, cx| {
            let theme = cx.theme().clone();
            let now = picked.get();
            let label = |file: &hopper::cf::File| hopper::undo::version_of(&file.file_name);
            let ok_label = format!("Roll back to {}", g.choices.get(now).map(label).unwrap_or_default());
            let (view2, f2, files, p2) = (view.clone(), f.clone(), g.choices.clone(), picked.clone());
            let title = view.update(cx, |v, cx| {
                h_flex()
                    .gap_1p5()
                    .child("Roll back")
                    .child(v.mod_cell("pick-title".into(), g.name.clone(), pid, &icon, false, cx))
            });
            dialog
                .title(title)
                .w(rems(30.).to_pixels(window.rem_size()))
                .margin_top(rems(crate::widgets::DIALOG_TOP).to_pixels(window.rem_size()))
                .child(
                    v_flex()
                        .gap_3()
                        .child(div().text_sm().child(format!("{}. Pick another for this instance:", g)))
                        .child(
                            v_flex()
                                .id("pick-list")
                                .max_h(rems(18.))
                                .overflow_y_scroll()
                                .rounded(theme.radius)
                                .border_1()
                                .border_color(theme.border)
                                .children(g.choices.iter().enumerate().map(|(i, file)| {
                                    let p = picked.clone();
                                    let hint = if i == g.closest {
                                        "closest".to_string()
                                    } else if i == 0 {
                                        "newest".to_string()
                                    } else {
                                        String::new()
                                    };
                                    let date = hopper::time::date_of(&file.file_date);
                                    let row = h_flex()
                                        .id(("pick", i))
                                        .gap_2()
                                        .px_3()
                                        .py_1p5()
                                        .text_sm()
                                        .clickable(cx)
                                        .hover(|s| widgets::row_hover(s, cx))
                                        .on_click(move |_, window, _| {
                                            p.set(i);
                                            window.refresh();
                                        })
                                        .child(div().flex_1().min_w_0().truncate().child(label(file)))
                                        .child(widgets::caption([hint, date].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · "), cx).text_xs());
                                    if i == now { widgets::row_selected(row, cx) } else { row }
                                })),
                        ),
                )
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().child(Button::new("pick-cancel").ghost().label("Cancel")))
                        .child(DialogAction::new().child(Button::new("pick-ok").primary().label(ok_label))),
                )
                .on_ok(move |_, _, cx| {
                    let Some(file) = files.get(p2.get()).cloned() else { return true };
                    view2.update(cx, |v, cx| v.put_back_as(f2.clone(), file, cx));
                    true
                })
        });
    }

    /// Puts another version in place of a gone one: a roll back to it.
    fn put_back_as(&mut self, f: Saved, file: hopper::cf::File, cx: &mut Context<Self>) {
        if let Some(t) = self.panel.clone() {
            let Some(when) = self.when_of(&f) else { return };
            let name = f.title();
            self.store.read(cx).spawn_job(format!("Rolling back {name}"), move |job| {
                t.client.post(&t.sp("/mod-backups/put-back"), serde_json::json!({ "when": when, "name": f.name, "file_id": file.id }))?;
                job.detail(format!("Rolled back {}", file.file_name));
                Ok(crate::store::Msg::Nothing)
            });
        } else {
            let root = self.root.clone();
            let s = self.store.read(cx);
            let Some(mut inst) = s.instance(&root).cloned() else { return };
            let name = f.src.as_ref().map(|r| r.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| f.name.clone());
            let r2 = root.clone();
            s.spawn_instance_job(&root, format!("Rolling back {name}"), move |job| {
                crate::store::fresh(&mut inst);
                hopper::undo::put_back_as(&mut inst, &f, &file, Some(job))?;
                job.detail(format!("Rolled back {}", file.file_name));
                Ok(crate::store::Msg::InstanceChanged(r2))
            });
        }
        // The entry leaves History once the job is done.
        let view = cx.entity();
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(3)).await;
            let _ = view.update(cx, |v, cx| v.changed(cx));
        })
        .detach();
    }

    /// The one-time "free space" offer: old jar copies the sites still hand
    /// out become notes. Asked first, never done by itself.
    pub(crate) fn free_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let bytes = self.copies.filter(|b| *b >= 1024 * 1024)?;
        let theme = cx.theme();
        let size = hopper::instance_ops::human_bytes(bytes);
        Some(
            h_flex()
                .gap_3()
                .px_3()
                .py_2()
                .rounded(theme.radius)
                .bg(theme.warning.opacity(0.10))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .child(format!("Older History keeps full copies of mods ({size}). Most can be downloaded again instead.")),
                )
                .child(
                    div().flex_none().child(
                        Button::new("hist-free")
                            .small()
                            .primary()
                            .label("Free up space")
                            .disabled(self.busy.is_some())
                            .on_click(cx.listener(|this, _, window, cx| this.ask_free(window, cx))),
                    ),
                )
                .into_any_element(),
        )
    }

    fn ask_free(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.entity();
        let size = hopper::instance_ops::human_bytes(self.copies.unwrap_or(0));
        window.open_alert_dialog(cx, move |alert, _, _| {
            let view = view.clone();
            alert
                .title("Remove old copies of mods?")
                .description(format!(
                    "History holds {size} of mod copies from older versions of Hopper. The ones CurseForge or Modrinth still hand out are removed; History keeps where to download each, so Undo still works. Copies with no download source stay."
                ))
                .button_props(
                    gpui_kit::component::dialog::DialogButtonProps::default()
                        .ok_text("Free up space")
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    view.update(cx, |v, cx| v.free_space(window, cx));
                    true
                })
        });
    }

    fn free_space(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let panel = self.panel.clone();
        self.busy = Some("Checking which mods can be downloaded again…".into());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let r = cx
                .background_spawn(async move {
                    match panel {
                        Some(t) => t
                            .client
                            .post(&t.sp("/mod-backups/free"), serde_json::json!({}))
                            .map(|v| (v["freed"].as_u64().unwrap_or(0), v["kept"].as_u64().unwrap_or(0))),
                        None => Ok(hopper::backups::free_space(&root)),
                    }
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                let (freed, kept) = match r {
                    Ok(r) => r,
                    Err(e) => {
                        window.push_notification(Notification::error(format!("Couldn't free up space: {e:#}")), cx);
                        return;
                    }
                };
                let size = hopper::instance_ops::human_bytes;
                let msg = match (freed, kept) {
                    (0, _) => "None of the copies can be downloaded again, so they stay.".to_string(),
                    (f, 0) => format!("Freed {}", size(f)),
                    (f, k) => format!("Freed {}. {} stays: those mods have no download source.", size(f), size(k)),
                };
                window.push_notification(Notification::success(msg), cx);
                this.reload(cx);
            })
            .ok();
        })
        .detach();
    }
}

/// The version picker's question, from a panel's undo reply.
fn gone_of(v: &serde_json::Value) -> Option<Gone> {
    let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    let choices: Vec<hopper::cf::File> = v["choices"]
        .as_array()?
        .iter()
        .map(|c| hopper::cf::File {
            id: c["id"].as_u64().unwrap_or(0),
            file_name: c["file_name"].as_str().unwrap_or_default().to_string(),
            file_date: c["date"].as_str().unwrap_or_default().to_string(),
            ..Default::default()
        })
        .collect();
    Some(Gone {
        name: if s("title").is_empty() { s("name") } else { s("title") },
        version: s("version"),
        site: if v["site"].as_str() == Some("Modrinth") { "Modrinth" } else { "CurseForge" },
        choices,
        closest: v["closest"].as_u64().unwrap_or(0) as usize,
    })
}

fn change_color(c: Change, same: bool, cx: &App) -> Hsla {
    let t = cx.theme();
    if !same {
        return t.muted_foreground;
    }
    match c {
        Change::Removed => t.danger,
        Change::Updated => t.info,
        Change::RolledBack => t.warning,
        Change::Edited | Change::Switched | Change::Replaced => t.primary,
        Change::TurnedOn | Change::Added => t.success,
        Change::TurnedOff => t.muted_foreground,
        Change::Moved | Change::Copied => t.info,
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's own `test` macro would shadow the std one.
    use super::{FACE, FACE_SLOTS, LEAD_W, face_split, set_indent};

    #[test]
    fn face_column_fits_two_then_folds_into_more() {
        assert_eq!(face_split(0), (0, 0));
        assert_eq!(face_split(1), (1, 0));
        assert_eq!(face_split(2), (2, 0));
        // Three or more: the first face and "+n" for everyone else.
        assert_eq!(face_split(3), (1, 2));
        assert_eq!(face_split(7), (1, 6));
        // Whatever shows always fits the column.
        for n in 0..10 {
            let (shown, more) = face_split(n);
            assert!(shown + usize::from(more > 0) <= FACE_SLOTS);
        }
    }

    #[test]
    fn nested_rows_sit_under_the_text_whoever_made_it() {
        // One indent for every entry: past the fixed column and the gap.
        assert_eq!(set_indent(true), LEAD_W + 1.);
        assert_eq!(set_indent(false), FACE + 1.);
    }
}

/// What an Undo put back, the first entry whose version its site no longer
/// has (for the version picker), files only the CurseForge website hands
/// out (for the download window, [`get_from_website`]), and what failed.
pub(crate) struct UndoDone {
    pub done: Vec<std::path::PathBuf>,
    pub gone: Option<(Saved, Gone)>,
    pub website: Vec<(Saved, hopper::cf::WebsiteOnly)>,
    pub errors: Vec<String>,
    /// Undo all on a pack update: the pack's name and what became of its
    /// own record (version, loader).
    pub pack_said: Option<(String, hopper::undo::PackRestore)>,
}

/// The entry's file only the CurseForge website hands out, and History
/// kept just a note of it: installs it the way the mod page's Files tab
/// does ("Roll back to…"), which opens the download window for its page
/// and adds the file, hash-checked, when it arrives in Downloads.
pub(crate) fn get_from_website(
    store: &Entity<crate::store::Store>,
    root: &std::path::Path,
    f: Saved,
    w: hopper::cf::WebsiteOnly,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(r) = f.src.clone() else {
        window.push_notification(Notification::error(hopper::undo::website_message(&w, false)), cx);
        return;
    };
    let s = store.read(cx);
    let Some(inst) = s.instance(root).cloned() else {
        window.push_notification(Notification::error("That instance is gone."), cx);
        return;
    };
    let instances_root = std::path::PathBuf::from(&s.settings().instances_root);
    let title = format!("Putting back {}", f.title());
    let at = root.to_path_buf();
    s.spawn_instance_job(root, title, move |job| {
        let (m, file) = hopper::undo::website_file(&r)?;
        let before = crate::store::Before::at(&at);
        let class = m.class_id.unwrap_or(hopper::cf::CLASS_MODS);
        let place = hopper::browse::Placement { instances_root: &instances_root, target: Some(inst), dest: None };
        let (changed, line) = hopper::browse::install_project(&m, Some(file), class, place, job)?;
        job.detail(line);
        Ok(crate::store::Msg::added(changed, &before, Some(m.id)))
    });
    window.push_notification(Notification::info(hopper::undo::website_message(&w, true)), cx);
}

/// Whether Undo is a roll back: updates, with the configs a pack update
/// changed among them. A removal or a plain config edit is put back instead,
/// so a mixed set keeps that wording.
fn all_updates(files: &[Saved]) -> bool {
    files.iter().any(|f| matches!(f.change, Change::Updated))
        && files.iter().all(|f| matches!(f.change, Change::Updated | Change::Edited))
}

/// The History tab's busy line while an Undo runs.
fn undo_busy(downloads: bool, roll_back: bool) -> String {
    match (downloads, roll_back) {
        (true, true) => "Downloading to roll back…",
        (true, false) => "Downloading to put it back…",
        (false, true) => "Rolling back…",
        (false, false) => "Putting it back…",
    }
    .into()
}

/// History's Undo of `files` in an instance, as a task: it waits its turn
/// behind another task on the instance, then works on what that one left.
/// Pick the result up with [`crate::store::work_done`], then call
/// [`undone`]. `None` when the instance is gone. `pack`: Undo all on a pack
/// update; when all of it came back, the pack's own record goes back too.
pub(crate) fn queue_undo(
    s: &crate::store::Store,
    root: &std::path::Path,
    files: Vec<Saved>,
    pack: Option<hopper::backups::PackMark>,
) -> Option<std::sync::Arc<std::sync::Mutex<Option<UndoDone>>>> {
    let mut inst = s.instance(root).cloned()?;
    let verb = if all_updates(&files) { "Rolling back" } else { "Putting back" };
    let title = match files.as_slice() {
        [one] => format!("{verb} {}", one.name),
        many => format!("{verb} {}", hopper::words::count(many.len(), "file")),
    };
    Some(s.queue_work(root, title, move |_| {
        crate::store::fresh(&mut inst);
        let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let mut done = Vec::new();
        let mut gone: Option<(Saved, Gone)> = None;
        let mut website = Vec::new();
        let mut errors = Vec::new();
        for f in &files {
            match hopper::undo::undo(&mut inst, f) {
                Ok(p) => done.push(p),
                Err(e) => match e.downcast::<Gone>() {
                    Ok(g) if gone.is_none() => gone = Some((f.clone(), g)),
                    Ok(g) => errors.push(format!("{}: {g}", f.name)),
                    // Only the CurseForge website has the file: the
                    // download window gets it, as from the mod's page.
                    Err(e) => match e.downcast::<hopper::cf::WebsiteOnly>() {
                        Ok(w) => website.push((f.clone(), w)),
                        Err(e) => errors.push(format!("{}: {e:#}", f.name)),
                    },
                },
            }
        }
        // Only when all of it came back.
        let mut pack_said = None;
        if let Some(m) = &pack
            && gone.is_none()
            && website.is_empty()
            && errors.is_empty()
        {
            match hopper::undo::restore_pack(&mut inst, m, since) {
                Ok(r) => pack_said = Some((m.name.clone(), r)),
                Err(e) => errors.push(format!("{}'s pack version wasn't rolled back: {e:#}", m.name)),
            }
        }
        UndoDone { done, gone, website, errors, pack_said }
    }))
}

/// After an Undo: the instance is read again and jars with no entry
/// (unknown ones) are identified, as on any drop.
pub(crate) fn undone(store: &Entity<crate::store::Store>, root: &std::path::Path, done: &[std::path::PathBuf], cx: &mut App) {
    let mods = root.join("mods");
    let jars: Vec<String> = done
        .iter()
        .filter(|p| p.parent() == Some(mods.as_path()))
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    store.update(cx, |s, cx| {
        s.reload_instance(root, cx);
        s.identify_jars(root, jars);
    });
}
