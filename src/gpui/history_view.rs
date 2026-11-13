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

impl BackupsView {
    /// History, grouped by day.
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
        let mut col = v_flex().gap_0p5();
        let people = self.people();
        if people.len() > 1 {
            col = col.child(self.who_filter(&people, cx));
        }
        let faces = !people.is_empty();
        let mut day = String::new();
        for (i, set) in self.sets.iter().enumerate() {
            if !self.shown(set) {
                continue;
            }
            let d = day_of(set.when);
            if d != day {
                col = col.child(
                    div()
                        .when(!day.is_empty(), |d| d.pt_3())
                        .pb_1()
                        .text_xs()
                        .font_semibold()
                        .text_color(cx.theme().muted_foreground)
                        .child(d.to_uppercase()),
                );
                day = d;
            }
            col = col.child(self.set_rows(i, set, faces, cx));
        }
        col.into_any_element()
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
                let key = if set.files.len() == 1 { i * 10_000 } else { i * 10_000 + j + 1 };
                if self.sel.contains(&key.to_string()) {
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

    /// One action: a row, or a fold of rows when it touched several files.
    /// `faces`: History names people, so each row starts with one.
    fn set_rows(&self, i: usize, set: &Set, faces: bool, cx: &mut Context<Self>) -> AnyElement {
        if let [f] = set.files.as_slice() {
            return self.entry_row(i * 10_000, f, set.when, false, faces, cx).into_any_element();
        }
        let open = self.open.contains(&set.dir);
        let dir = set.dir.clone();
        let all = set.files.clone();
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
        let lead = if !faces {
            div().size(rems(FACE)).flex_none().flex().items_center().justify_center().child(chevron.clone()).into_any_element()
        } else if let [w] = people.as_slice() {
            self.who_face(w, rems(FACE), cx)
        } else if !people.is_empty() {
            let small = rems(FACE * 0.8);
            let fs: Vec<AnyElement> = people.iter().take(3).map(|w| self.who_face(w, small, cx)).collect();
            div()
                .size(rems(FACE))
                .flex_none()
                .flex()
                .items_center()
                .child(widgets::face_stack(fs, cx.theme().background, small, cx.theme().radius))
                .into_any_element()
        } else {
            div().size(rems(FACE)).flex_none().into_any_element()
        };
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
        v_flex()
            .child(
                head
                    .child(lead)
                    .child(v_flex().flex_1().min_w_0().child(title).child(widgets::caption(caption, cx).text_xs().truncate()))
                    .child(widgets::caption(hm(set.when), cx).text_xs())
                    .child(
                        widgets::row_actions(("hist-set-actions", i)).child(
                            Button::new(("hist-undo-all", i))
                                .xsmall()
                                .outline()
                                .label("Undo all")
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, window, cx| this.undo(all.clone(), window, cx))),
                        ),
                    ),
            )
            .when(open, |c| {
                c.children(set.files.iter().enumerate().map(|(j, f)| self.entry_row(i * 10_000 + j + 1, f, set.when, true, faces, cx)))
            })
            .into_any_element()
    }

    /// One file: what happened to it, its versions, when, and Undo.
    /// `faces`: rows start with whoever made the change ("Mia turned off
    /// Sodium"); entries from before History noted that keep a blank there.
    fn entry_row(&self, key: usize, f: &Saved, when: i64, nested: bool, faces: bool, cx: &mut Context<Self>) -> Stateful<Div> {
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
        let note = f
            .who
            .as_ref()
            .filter(|w| !w.is_you() && f.change == hopper::backups::Change::Edited)
            .and_then(|_| hopper::backups::settings_note(f.path.parent()?, &f.restore_to));
        let name = match &note {
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
            .when(nested, |r| r.pl(rems(FACE + 1.)))
            .rounded(cx.theme().radius)
            .hover(|s| widgets::row_hover(s, cx))
            .clickable(cx);
        let row = self.pickable(row, vec![key.to_string()], None, cx);
        let row = if picked { widgets::row_selected(row, cx) } else { row };
        let who = f.who.as_ref().filter(|_| !nested);
        let lead = (faces && !nested).then(|| match who {
            Some(w) => self.who_face(w, rems(FACE), cx),
            None => div().size(rems(FACE)).flex_none().into_any_element(),
        });
        let what = match who {
            Some(w) => h_flex()
                .flex_none()
                .gap_1p5()
                .child(div().font_semibold().child(w.label()))
                .child(div().text_color(cx.theme().muted_foreground).child(f.change.verb())),
            None => h_flex().flex_none().child(div().text_color(change_color(f.change, true, cx)).child(f.change.label())),
        };
        let mut detail = match &note {
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
            .child(
                Button::new(("hist-undo", key))
                    .xsmall()
                    .outline()
                    .label("Undo")
                    .disabled(busy)
                    .on_click(cx.listener(move |this, _, window, cx| this.undo(vec![file.clone()], window, cx))),
            )
    }

    /// Undoes entries, newest set first. A mod whose version its site no
    /// longer has opens the version picker.
    pub(crate) fn undo(&mut self, files: Vec<Saved>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(t) = self.panel.clone() {
            return self.undo_panel(t, files, window, cx);
        }
        let root = self.root.clone();
        let s = self.store.read(cx);
        if let Some(why) = s.edit_hold_reason(&root) {
            window.push_notification(Notification::error(format!("Can't undo now: {why}.")), cx);
            return;
        }
        let Some(mut inst) = s.instance(&root).cloned() else {
            window.push_notification(Notification::error("That instance is gone."), cx);
            return;
        };
        let downloads = files.iter().any(|f| f.download);
        self.busy = Some(if downloads { "Downloading to put it back…".into() } else { "Putting it back…".into() });
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let (done, gone, errors) = cx
                .background_spawn(async move {
                    let mut done = Vec::new();
                    let mut gone: Option<(Saved, Gone)> = None;
                    let mut errors = Vec::new();
                    for f in &files {
                        match hopper::undo::undo(&mut inst, f) {
                            Ok(p) => done.push(p),
                            Err(e) => match e.downcast::<Gone>() {
                                Ok(g) if gone.is_none() => gone = Some((f.clone(), g)),
                                Ok(g) => errors.push(format!("{}: {g}", f.name)),
                                Err(e) => errors.push(format!("{}: {e:#}", f.name)),
                            },
                        }
                    }
                    (done, gone, errors)
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                this.after_undo(&done, window, cx);
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

    /// The time of the History entry a file is in (how a panel names it).
    fn when_of(&self, f: &Saved) -> Option<i64> {
        self.sets.iter().find(|s| s.files.contains(f)).map(|s| s.when)
    }

    /// Undo on a panel server: the panel does the same per file.
    fn undo_panel(&mut self, t: crate::panel_tabs::Target, files: Vec<Saved>, window: &mut Window, cx: &mut Context<Self>) {
        let picked: Vec<(i64, Saved)> = files.into_iter().filter_map(|f| Some((self.when_of(&f)?, f))).collect();
        let downloads = picked.iter().any(|(_, f)| f.download);
        self.busy = Some(if downloads { "Downloading to put it back…".into() } else { "Putting it back…".into() });
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let (done, gone, errors, restart) = cx
                .background_spawn(async move {
                    let mut done: Vec<String> = Vec::new();
                    let mut gone: Option<(Saved, Gone)> = None;
                    let mut errors = Vec::new();
                    let mut restart = false;
                    for (when, f) in &picked {
                        match t.client.post(&t.sp("/mod-backups/undo"), serde_json::json!({ "when": when, "name": f.name })) {
                            Ok(v) => {
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
                    (done, gone, errors, restart)
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                let paths: Vec<std::path::PathBuf> = done.iter().map(std::path::PathBuf::from).collect();
                this.say_put_back(&paths, restart, window, cx);
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

    /// "Put back X" / "Put back 3 files", with a restart hint for a running
    /// server.
    fn say_put_back(&self, done: &[std::path::PathBuf], restart: bool, window: &mut Window, cx: &mut Context<Self>) {
        let hint = if restart { ". Restart the server to load it." } else { "" };
        match done {
            [] => {}
            [one] => {
                let n = one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                window.push_notification(Notification::success(format!("Put back {n}{hint}")), cx)
            }
            many => window.push_notification(Notification::success(format!("Put back {} files{hint}", many.len())), cx),
        }
    }

    /// Tells the store and the user what came back.
    fn after_undo(&mut self, done: &[std::path::PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let mods = root.join("mods");
        // Jars with no entry (unknown ones): identified, as on any drop.
        let jars: Vec<String> = done
            .iter()
            .filter(|p| p.parent() == Some(mods.as_path()))
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        self.store.update(cx, |s, cx| {
            s.reload_instance(&root, cx);
            s.identify_jars(&root, jars);
        });
        self.say_put_back(done, false, window, cx);
        self.changed(cx);
    }

    /// The version this entry was is gone from its site: offers the ones it
    /// has, the closest picked.
    fn pick_version(&mut self, f: Saved, g: Gone, window: &mut Window, cx: &mut Context<Self>) {
        if g.choices.is_empty() {
            let msg = format!("{}. It has no other version for this Minecraft and loader, so it can't be put back.", g);
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
            let ok_label = format!("Put back {}", g.choices.get(now).map(label).unwrap_or_default());
            let (view2, f2, files, p2) = (view.clone(), f.clone(), g.choices.clone(), picked.clone());
            let title = view.update(cx, |v, cx| {
                h_flex()
                    .gap_1p5()
                    .child("Put back")
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

    /// Puts another version back in place of a gone one.
    fn put_back_as(&mut self, f: Saved, file: hopper::cf::File, cx: &mut Context<Self>) {
        if let Some(t) = self.panel.clone() {
            let Some(when) = self.when_of(&f) else { return };
            let name = f.title();
            self.store.read(cx).spawn_job(format!("Putting back {name}"), move |job| {
                t.client.post(&t.sp("/mod-backups/put-back"), serde_json::json!({ "when": when, "name": f.name, "file_id": file.id }))?;
                job.detail(format!("Put back {}", file.file_name));
                Ok(crate::store::Msg::Nothing)
            });
        } else {
            let root = self.root.clone();
            let s = self.store.read(cx);
            let Some(mut inst) = s.instance(&root).cloned() else { return };
            let name = f.src.as_ref().map(|r| r.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| f.name.clone());
            let r2 = root.clone();
            s.spawn_instance_job(&root, format!("Putting back {name}"), move |job| {
                hopper::undo::put_back_as(&mut inst, &f, &file, Some(job))?;
                job.detail(format!("Put back {}", file.file_name));
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
    }
}
