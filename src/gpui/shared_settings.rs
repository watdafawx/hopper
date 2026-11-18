//! A group's shared settings, for the Friends panel: the list of mods whose
//! settings the group shares, a summary card for when there are many, and
//! the Manage dialog to stop several at once.
//!
//! The counting, filtering and picking are plain functions at the top so
//! they can be tested; the dialog below only draws them.

use crate::drag_pick;
use crate::friends::{FriendsEvent, FriendsPanel, ago, name_or};
use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::{DialogAction, DialogFooter};
use gpui_kit::component::input::InputState;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::configsync::ModCfg;
use hopper::groupsync::addon_key;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

/// From this many shared mods on, the panel shows a summary card instead of
/// one row per mod (a row is ~60 px: twenty of them push the People list off
/// the screen).
pub(crate) const SUMMARY_FROM: usize = 3;
/// How many mod icons the summary card shows before "+K more".
pub(crate) const ICONS_SHOWN: usize = 6;
/// An icon with its 2 px ring, and how far apart the icons sit: each
/// overlaps the next by 8 px.
pub(crate) const ICON_RING: f32 = 28.;
pub(crate) const ICON_STRIDE: f32 = 20.;

/// One mod whose settings the group shares, in this PC's copy of a group
/// pack.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SharedMod {
    /// Unique among the group's shared mods: the copy's folder and the mod.
    pub id: String,
    pub root: PathBuf,
    /// The mod's key in the group's state (`groupsync::addon_key`).
    pub key: String,
    pub name: String,
    pub icon: Option<PathBuf>,
    pub project: u64,
    /// How many of its settings the group changed.
    pub changed: usize,
    /// Who started sharing it: "you" or a friend's name.
    pub who: String,
    /// The pack copy it is in.
    pub instance: String,
    /// When a setting last changed (0 when the data has no time).
    pub last: u64,
}

/// The mods the group `gid` shares, in this PC's copies of its packs.
pub(crate) fn shared_mods(s: &Store, gid: &str) -> Vec<SharedMod> {
    if !s.settings_sharing() {
        return Vec::new();
    }
    let me = s.mailbox_id();
    let mut out = Vec::new();
    for c in s.settings().group_copies.iter().filter(|c| c.group == gid) {
        let Some(inst) = s.instances().iter().find(|i| i.id == c.local) else { continue };
        for (m, mc) in c.configs.shared() {
            let a = inst.addons.iter().find(|a| addon_key(a) == *m);
            let (name, icon, project) = match a {
                Some(a) => (a.name.clone(), s.image(&a.icon).map(|p| p.to_path_buf()), a.project_id),
                None => ("A mod you don't have".to_string(), None, 0),
            };
            out.push(SharedMod {
                id: format!("{}\u{1f}{m}", inst.root.display()),
                root: inst.root.clone(),
                key: m.clone(),
                name,
                icon,
                project,
                changed: c.configs.count(m),
                who: if mc.shared.by == me { "you".to_string() } else { name_or(&mc.shared.name, "a friend") },
                instance: inst.name.clone(),
                last: last_change(mc),
            });
        }
    }
    out
}

/// When the newest setting of this mod changed (0 when none, or no time).
pub(crate) fn last_change(mc: &ModCfg) -> u64 {
    mc.set.values().flat_map(|f| f.values()).map(|v| v.at).max().unwrap_or(0)
}

/// Whether `n` shared mods are shown as a summary card.
pub(crate) fn summarised(n: usize) -> bool {
    n >= SUMMARY_FROM
}

/// Settings changed across all the mods.
pub(crate) fn total_changed(items: &[SharedMod]) -> usize {
    items.iter().map(|m| m.changed).sum()
}

/// The card's line: how many settings changed, and that friends get them.
pub(crate) fn changed_line(total: usize) -> String {
    match total {
        0 => "No settings changed yet. Your friends get changes right away.".to_string(),
        1 => "1 setting changed. Your friends get it right away.".to_string(),
        n => format!("{n} settings changed. Your friends get them right away."),
    }
}

/// "+3 more" for the mods past the icons shown; None when they all show.
pub(crate) fn more_label(mods: usize) -> Option<String> {
    (mods > ICONS_SHOWN).then(|| format!("+{} more", mods - ICONS_SHOWN))
}

/// The Changed column: "3 settings", "1 setting", or a dash.
pub(crate) fn settings_label(n: usize) -> String {
    match n {
        0 => "\u{2013}".to_string(),
        1 => "1 setting".to_string(),
        n => format!("{n} settings"),
    }
}

/// What the Manage dialog's list shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Filter {
    All,
    Changed,
    /// Shared, but nobody changed a setting yet.
    Waiting,
}

impl Filter {
    pub fn index(self) -> usize {
        match self {
            Filter::All => 0,
            Filter::Changed => 1,
            Filter::Waiting => 2,
        }
    }

    pub fn from_index(i: usize) -> Self {
        match i {
            1 => Filter::Changed,
            2 => Filter::Waiting,
            _ => Filter::All,
        }
    }

    fn keeps(self, m: &SharedMod) -> bool {
        match self {
            Filter::All => true,
            Filter::Changed => m.changed > 0,
            Filter::Waiting => m.changed == 0,
        }
    }
}

/// How many mods each filter shows: (All, Changed, No changes yet).
pub(crate) fn counts(items: &[SharedMod]) -> (usize, usize, usize) {
    let changed = items.iter().filter(|m| m.changed > 0).count();
    (items.len(), changed, items.len() - changed)
}

/// The indexes of the mods the filter and the search words show, in order.
/// A mod shows when its name holds every word.
pub(crate) fn visible(items: &[SharedMod], filter: Filter, words: &[String]) -> Vec<usize> {
    items
        .iter()
        .enumerate()
        .filter(|(_, m)| filter.keeps(m))
        .filter(|(_, m)| {
            let name = m.name.to_lowercase();
            words.iter().all(|w| name.contains(w.as_str()))
        })
        .map(|(i, _)| i)
        .collect()
}

/// The ticked mods, and where a Shift-click range starts.
#[derive(Default, Debug)]
pub(crate) struct Ticks {
    pub picked: HashSet<String>,
    anchor: Option<String>,
}

impl Ticks {
    /// Ticks or unticks one mod (its tick box, or a drag across rows).
    pub fn set(&mut self, id: &str, on: bool) {
        if on {
            self.picked.insert(id.to_string());
        } else {
            self.picked.remove(id);
        }
        self.anchor = Some(id.to_string());
    }

    /// A click on a row: Shift ticks everything from the last one clicked
    /// to this one (in `order`, the rows as shown); anything else flips
    /// this one.
    pub fn click(&mut self, id: &str, shift: bool, order: &[String]) {
        let from = self.anchor.as_deref().and_then(|a| order.iter().position(|o| o == a));
        let to = order.iter().position(|o| o == id);
        match (shift, from, to) {
            (true, Some(a), Some(b)) => {
                for o in &order[a.min(b)..=a.max(b)] {
                    self.picked.insert(o.clone());
                }
                // The anchor stays, so the next Shift-click moves the range.
            }
            _ => {
                let on = !self.picked.contains(id);
                self.set(id, on);
            }
        }
    }

    /// Ticks all of `order`, or unticks them all when they are all ticked.
    pub fn all(&mut self, order: &[String]) {
        if !order.is_empty() && order.iter().all(|o| self.picked.contains(o)) {
            for o in order {
                self.picked.remove(o);
            }
        } else {
            self.picked.extend(order.iter().cloned());
        }
    }

    /// Forgets ticks of mods that aren't in `order` (filtered out, or gone):
    /// "Stop sharing selected" only acts on what's in view.
    pub fn keep_only(&mut self, order: &[String]) {
        self.picked.retain(|p| order.contains(p));
        if self.anchor.as_ref().is_some_and(|a| !order.contains(a)) {
            self.anchor = None;
        }
    }
}

// ---- the Manage dialog -----------------------------------------------------------

/// Opens "Shared settings · <group>": every shared mod in a table, to stop
/// sharing several at once.
pub(crate) fn open(store: Entity<Store>, panel: Entity<FriendsPanel>, gid: String, gname: String, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| Manage {
        store,
        panel,
        gid,
        search: cx.new(|cx| InputState::new(window, cx).placeholder("Search mods")),
        filter: Filter::All,
        ticks: Ticks::default(),
        paint: drag_pick::Paint::default(),
    });
    let sub = view.update(cx, |m, cx| {
        cx.observe(&m.search, |m, _, cx| {
            m.prune(cx);
            cx.notify();
        })
    });
    window.open_dialog(cx, move |dialog, window, cx| {
        let _ = &sub;
        let picked = view.read(cx).ticks.picked.len();
        let v = view.clone();
        dialog
            .title(format!("Shared settings \u{b7} {gname}"))
            .w(rems(40.).to_pixels(window.rem_size()))
            .margin_top(rems(widgets::DIALOG_TOP).to_pixels(window.rem_size()))
            .child(view.clone())
            .footer(
                DialogFooter::new()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .self_center()
                            .whitespace_nowrap()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{picked} selected")),
                    )
                    .child(
                        Button::new("shared-stop-picked")
                            .outline()
                            .label("Stop sharing selected")
                            .disabled(picked == 0)
                            .on_click(move |_, window, cx| {
                                v.update(cx, |m, cx| m.stop_picked(cx));
                                window.refresh();
                            }),
                    )
                    // A wrapper that is only as wide as the button: the action
                    // fills its parent, which would crowd out the count.
                    .child(div().flex_none().child(DialogAction::new().child(Button::new("shared-done").primary().label("Done")))),
            )
            .on_ok(|_, _, _| true)
    });
}

struct Manage {
    store: Entity<Store>,
    /// The Friends panel, which opens a mod's page beside the dialog.
    panel: Entity<FriendsPanel>,
    gid: String,
    search: Entity<InputState>,
    filter: Filter,
    ticks: Ticks,
    paint: drag_pick::Paint,
}

impl Manage {
    fn items(&self, cx: &App) -> Vec<SharedMod> {
        shared_mods(self.store.read(cx), &self.gid)
    }

    fn order(&self, items: &[SharedMod], cx: &App) -> Vec<usize> {
        visible(items, self.filter, &widgets::search_words(&self.search.read(cx).value()))
    }

    /// Drops ticks on mods the list no longer shows.
    fn prune(&mut self, cx: &App) {
        let items = self.items(cx);
        let ids: Vec<String> = self.order(&items, cx).into_iter().map(|i| items[i].id.clone()).collect();
        self.ticks.keep_only(&ids);
    }

    /// Stops sharing every ticked mod: the same call as a row's Stop.
    fn stop_picked(&mut self, cx: &mut Context<Self>) {
        let items = self.items(cx);
        let todo: Vec<(PathBuf, String)> =
            items.iter().filter(|m| self.ticks.picked.contains(&m.id)).map(|m| (m.root.clone(), m.key.clone())).collect();
        self.ticks.picked.clear();
        self.store.update(cx, |s, cx| {
            for (root, key) in &todo {
                s.set_settings_shared(root, key, false, cx);
            }
        });
        cx.notify();
    }
}

impl Render for Manage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let me = cx.entity();
        let items = self.items(cx);
        let order = self.order(&items, cx);
        let ids: Rc<Vec<String>> = Rc::new(order.iter().map(|&i| items[i].id.clone()).collect());
        self.ticks.keep_only(&ids);
        let (all, changed, waiting) = counts(&items);
        let any_last = items.iter().any(|m| m.last > 0);
        let group: SharedString = "shared-pick".into();

        let all_on = !ids.is_empty() && ids.iter().all(|i| self.ticks.picked.contains(i));
        let head = h_flex()
            .gap_2()
            .px_2()
            .pb_1()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(div().w(rems(1.5)).flex_none().child({
                let (me, ids) = (me.clone(), ids.clone());
                Checkbox::new("shared-all").checked(all_on).on_click(move |_, window, cx| {
                    me.update(cx, |m, cx| {
                        m.ticks.all(&ids);
                        cx.notify();
                    });
                    window.refresh();
                })
            }))
            .child(div().flex_1().min_w_0().child("Mod"))
            .child(div().w(rems(6.5)).flex_none().child("Changed"))
            .when(any_last, |r| r.child(div().w(rems(7.5)).flex_none().child("Last change")));

        let mut rows = Vec::new();
        for (r, &ix) in order.iter().enumerate() {
            let m = items[ix].clone();
            let on = self.ticks.picked.contains(&m.id);
            let set = {
                let (me, id) = (me.clone(), m.id.clone());
                Rc::new(move |v: usize, window: &mut Window, cx: &mut App| {
                    me.update(cx, |t, cx| {
                        if t.ticks.picked.contains(&id) != (v == 1) {
                            t.ticks.set(&id, v == 1);
                            cx.notify();
                        }
                    });
                    window.refresh();
                })
            };
            // Shift or Ctrl held on a row: range or single flip.
            let click = {
                let (me, id, ids) = (me.clone(), m.id.clone(), ids.clone());
                Rc::new(move |e: &MouseDownEvent, window: &mut Window, cx: &mut App| {
                    me.update(cx, |t, cx| {
                        t.ticks.click(&id, e.modifiers.shift, &ids);
                        cx.notify();
                    });
                    window.refresh();
                })
            };
            let (s1, s2, paint, g2) = (set.clone(), set.clone(), self.paint.clone(), group.clone());
            let (c_row, c_changed, c_last) = (click.clone(), click.clone(), click);
            let panel = self.panel.clone();
            let tick = {
                let (down_paint, group, set) = (self.paint.clone(), group.clone(), set.clone());
                div()
                    .w(rems(1.5))
                    .flex_none()
                    .capture_any_mouse_down(move |e: &MouseDownEvent, window, cx| {
                        if e.button == MouseButton::Left {
                            down_paint.start(&group, usize::from(!on));
                            set(usize::from(!on), window, cx);
                        }
                    })
                    .child(Checkbox::new(("shared-tick", r)).checked(on).on_click(move |v: &bool, w, cx| {
                        if paint.active(&g2) {
                            return;
                        }
                        s2(usize::from(*v), w, cx)
                    }))
            };
            let cell_click = |f: Rc<dyn Fn(&MouseDownEvent, &mut Window, &mut App)>| {
                move |e: &MouseDownEvent, window: &mut Window, cx: &mut App| {
                    if e.button == MouseButton::Left {
                        f(e, window, cx);
                        cx.stop_propagation();
                    }
                }
            };
            rows.push(
                drag_pick::paint_row(h_flex().id(("shared-row", r)), &self.paint, group.clone(), move |v, w, cx| s1(v, w, cx))
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .items_center()
                    .rounded(theme.radius)
                    .when(on, |d| widgets::row_selected(d, cx))
                    .when(!on, |d| d.hover(|s| widgets::row_hover(s, cx)))
                    .on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, window, cx| {
                        if e.modifiers.shift || e.modifiers.secondary() {
                            c_row(e, window, cx);
                        }
                    })
                    .child(tick)
                    .child(div().flex_1().min_w_0().text_sm().child(widgets::mod_link(
                        ("shared-name", r),
                        m.name.clone(),
                        m.icon.as_deref(),
                        m.project,
                        move |p, _, cx| panel.update(cx, |_, cx| cx.emit(FriendsEvent::Project(p))),
                        cx,
                    )))
                    .child(
                        div()
                            .w(rems(6.5))
                            .flex_none()
                            .text_sm()
                            .when(m.changed == 0, |d| d.text_color(theme.muted_foreground))
                            .on_mouse_down(MouseButton::Left, cell_click(c_changed))
                            .child(settings_label(m.changed)),
                    )
                    .when(any_last, |r| {
                        r.child(
                            div()
                                .w(rems(7.5))
                                .flex_none()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .on_mouse_down(MouseButton::Left, cell_click(c_last))
                                .child(if m.last > 0 { ago(m.last) } else { "\u{2013}".to_string() }),
                        )
                    })
                    .into_any_element(),
            );
        }

        let room = (widgets::dialog_room(window) - window.rem_size() * 14.).max(window.rem_size() * 8.);
        let end = self.paint.clone();
        let body = if rows.is_empty() {
            let (title, body) = if items.is_empty() {
                ("Nothing is shared now", "No mod's settings are shared with this group.")
            } else {
                ("No mods match", "Try another word, or another filter.")
            };
            widgets::empty_state(IconName::Search, title, Some(body.into()), cx).into_any_element()
        } else {
            v_flex()
                .id("shared-list")
                .w_full()
                .max_h(room)
                .overflow_y_scroll()
                .gap_0p5()
                .on_mouse_up(MouseButton::Left, move |_, _, _| end.end())
                .children(rows)
                .into_any_element()
        };

        v_flex()
            .w_full()
            .gap_3()
            .child(widgets::search_input(&self.search).small().cleanable(true))
            .child(
                widgets::Segmented::new("shared-filter")
                    .selected_index(self.filter.index())
                    .child(widgets::SegItem::new().label(format!("All {all}")))
                    .child(widgets::SegItem::new().label(format!("Changed {changed}")))
                    .child(widgets::SegItem::new().label(format!("No changes yet {waiting}")))
                    .on_click({
                        let me = me.clone();
                        move |i: &usize, window, cx| {
                            me.update(cx, |m, cx| {
                                m.filter = Filter::from_index(*i);
                                m.prune(cx);
                                cx.notify();
                            });
                            window.refresh();
                        }
                    }),
            )
            .child(v_flex().w_full().child(head).child(body))
            .child(widgets::fine_print(
                "Drag across rows or Shift-click to pick several. Stopping keeps your own settings as they are.",
                cx,
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `super::*` brings in gpui's `test` attribute, which loops; name the standard one.
    use hopper::configsync::Val;
    use std::collections::BTreeMap;

    fn m(name: &str, changed: usize) -> SharedMod {
        SharedMod {
            id: name.to_string(),
            root: PathBuf::from("/x"),
            key: name.to_lowercase(),
            name: name.to_string(),
            icon: None,
            project: 0,
            changed,
            who: "you".into(),
            instance: "Pack".into(),
            last: 0,
        }
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[::core::prelude::v1::test]
    fn summary_starts_at_three() {
        assert!(!summarised(0));
        assert!(!summarised(2));
        assert!(summarised(3));
        assert!(summarised(4));
        assert!(summarised(20));
    }

    #[::core::prelude::v1::test]
    fn changed_line_counts_and_pluralises() {
        assert_eq!(changed_line(1), "1 setting changed. Your friends get it right away.");
        assert_eq!(changed_line(12), "12 settings changed. Your friends get them right away.");
        assert!(changed_line(0).starts_with("No settings changed yet"));
    }

    #[::core::prelude::v1::test]
    fn total_changed_sums_every_mod() {
        assert_eq!(total_changed(&[m("A", 3), m("B", 0), m("C", 4)]), 7);
        assert_eq!(total_changed(&[]), 0);
    }

    #[::core::prelude::v1::test]
    fn more_label_only_past_the_icons() {
        assert_eq!(more_label(ICONS_SHOWN), None);
        assert_eq!(more_label(ICONS_SHOWN + 8).as_deref(), Some("+8 more"));
    }

    #[::core::prelude::v1::test]
    fn settings_label_says_dash_one_or_many() {
        assert_eq!(settings_label(0), "\u{2013}");
        assert_eq!(settings_label(1), "1 setting");
        assert_eq!(settings_label(3), "3 settings");
    }

    #[::core::prelude::v1::test]
    fn filters_count_and_split_the_mods() {
        let items = [m("A", 2), m("B", 0), m("C", 1), m("D", 0), m("E", 0)];
        assert_eq!(counts(&items), (5, 2, 3));
        assert_eq!(visible(&items, Filter::All, &[]), vec![0, 1, 2, 3, 4]);
        assert_eq!(visible(&items, Filter::Changed, &[]), vec![0, 2]);
        assert_eq!(visible(&items, Filter::Waiting, &[]), vec![1, 3, 4]);
    }

    #[::core::prelude::v1::test]
    fn search_needs_every_word_in_the_name() {
        let items = [m("Create Aeronautics", 1), m("Create", 0), m("Jade", 2)];
        assert_eq!(visible(&items, Filter::All, &["create".into()]), vec![0, 1]);
        assert_eq!(visible(&items, Filter::All, &["create".into(), "aero".into()]), vec![0]);
        assert!(visible(&items, Filter::All, &["zzz".into()]).is_empty());
        // The filter and the search both apply.
        assert_eq!(visible(&items, Filter::Changed, &["create".into()]), vec![0]);
    }

    #[::core::prelude::v1::test]
    fn filter_index_round_trips() {
        for f in [Filter::All, Filter::Changed, Filter::Waiting] {
            assert_eq!(Filter::from_index(f.index()), f);
        }
    }

    #[::core::prelude::v1::test]
    fn click_flips_one_and_shift_ticks_a_range() {
        let order = ids(&["a", "b", "c", "d", "e"]);
        let mut t = Ticks::default();
        t.click("b", false, &order);
        assert!(t.picked.contains("b"));
        t.click("d", true, &order);
        let mut got: Vec<_> = t.picked.iter().cloned().collect();
        got.sort();
        assert_eq!(got, ids(&["b", "c", "d"]));
        // The range starts from the same spot again, and goes either way.
        t.click("a", true, &order);
        assert!(t.picked.contains("a") && t.picked.contains("b"));
        // A plain click on a ticked row unticks it.
        t.click("c", false, &order);
        assert!(!t.picked.contains("c"));
    }

    #[::core::prelude::v1::test]
    fn shift_without_a_start_just_ticks_the_row() {
        let order = ids(&["a", "b", "c"]);
        let mut t = Ticks::default();
        t.click("c", true, &order);
        assert_eq!(t.picked.len(), 1);
        assert!(t.picked.contains("c"));
    }

    #[::core::prelude::v1::test]
    fn select_all_toggles_and_filtering_drops_hidden_ticks() {
        let order = ids(&["a", "b", "c"]);
        let mut t = Ticks::default();
        t.set("a", true);
        t.all(&order);
        assert_eq!(t.picked.len(), 3);
        t.all(&order);
        assert!(t.picked.is_empty());
        t.all(&order);
        t.keep_only(&ids(&["a", "c"]));
        assert_eq!(t.picked.len(), 2);
        assert!(!t.picked.contains("b"));
        // Nothing in view: select all does nothing.
        let mut e = Ticks::default();
        e.all(&[]);
        assert!(e.picked.is_empty());
    }

    #[::core::prelude::v1::test]
    fn last_change_is_the_newest_setting() {
        let val = |at| Val { v: "1".into(), at, ..Default::default() };
        let mut mc = ModCfg::default();
        assert_eq!(last_change(&mc), 0);
        let mut f = BTreeMap::new();
        f.insert("a".to_string(), val(100));
        f.insert("b".to_string(), val(300));
        mc.set.insert("x.toml".to_string(), f);
        let mut g = BTreeMap::new();
        g.insert("c".to_string(), val(200));
        mc.set.insert("y.toml".to_string(), g);
        assert_eq!(last_change(&mc), 300);
    }
}
