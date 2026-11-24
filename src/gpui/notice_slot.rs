//! The notices of an instance's or a server's page (a load problem, a choice
//! the player has to make, a tip). They look the same everywhere and never
//! push the page off the screen: they live in the hero's chips. [`chip`] is
//! the "Needs you" chip, its popover the list grouped by how much they
//! matter; [`run_chip`] is the run state's problem (a start that looks
//! stuck, a crash) next to its pill.
//!
//! A page builds its notices as [`Notice`]s. They are ordered by [`order`]:
//! problems that stop the game first, then the ones that need a choice, then
//! the rest; the newest first within each. A notice that stops play has no
//! ✕: it leaves when its cause does.

use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::HashSet;
use std::rc::Rc;

/// How much a notice matters, which also picks its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// The game won't start or load until it's fixed (red).
    Stop,
    /// Something the player has to decide (amber).
    Choice,
    /// For the player's information; nothing breaks (blue).
    Info,
}

impl Tier {
    pub const ALL: [Tier; 3] = [Tier::Stop, Tier::Choice, Tier::Info];

    pub fn color(self, cx: &App) -> Hsla {
        let theme = cx.theme();
        match self {
            Tier::Stop => theme.danger,
            Tier::Choice => theme.warning,
            Tier::Info => theme.info,
        }
    }

    /// The heading of its group in the "N more" list.
    pub fn heading(self) -> &'static str {
        match self {
            Tier::Stop => "Stops play",
            Tier::Choice => "Needs a choice",
            Tier::Info => "For your information",
        }
    }
}

/// How a notice's button is drawn, known only once the slot is laid out.
#[derive(Clone, Copy, Debug, Default)]
pub struct Fit {
    /// The page is narrow: a long label shortens (its full text then goes
    /// in the tooltip).
    pub narrow: bool,
    /// The slot's first main action: the one filled button of the region.
    /// Every other main action is outlined.
    pub lead: bool,
}

/// A button for a notice, built when the slot knows its [`Fit`].
pub type Act = Box<dyn FnOnce(Fit) -> AnyElement>;

/// A notice's main button: filled when it leads the slot, else outlined.
pub fn main_button(b: Button, fit: Fit) -> Button {
    if fit.lead { b.primary() } else { b.outline() }
}

/// A dismiss handler (the ✕ of a notice that doesn't stop play).
pub type Dismiss = Rc<dyn Fn(&mut Window, &mut App)>;

/// One notice, as a page hands it to the slot.
pub struct Notice {
    /// Stable for as long as the notice is about the same thing: it keys
    /// its "Details" being open.
    pub id: SharedString,
    pub tier: Tier,
    pub icon: Option<IconName>,
    pub title: SharedString,
    /// A short line after the title ("· Create 6.0.6 needs Flywheel…");
    /// dropped when the page is narrow (it's in the title's tooltip then).
    pub subject: Option<SharedString>,
    /// The subject drawn with inline elements (mod names with their icons,
    /// each opening the mod's page) in place of its plain text, which stays
    /// for the tooltip and narrow layouts. See [`Notice::subject_rich`].
    pub rich: Option<AnyElement>,
    /// Its main action: the one kept in the "N more" list too.
    pub primary: Option<Act>,
    /// Its other actions, on the strip.
    pub actions: Vec<Act>,
    /// What "Details" opens under the strip (rows, more text).
    pub details: Option<AnyElement>,
    /// Its details open the first time it shows (the player can close them
    /// and they stay closed): for a list the player needs to see.
    pub open_new: bool,
    /// The ✕. Ignored for [`Tier::Stop`]: those stay until fixed.
    pub dismiss: Option<Dismiss>,
    /// When it happened (unix seconds), for the order within a tier; 0 when
    /// unknown.
    pub since: i64,
}

impl Notice {
    pub fn new(id: impl Into<SharedString>, tier: Tier, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            tier,
            icon: None,
            title: title.into(),
            subject: None,
            rich: None,
            primary: None,
            actions: Vec::new(),
            details: None,
            open_new: false,
            dismiss: None,
            since: 0,
        }
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn subject(mut self, s: impl Into<SharedString>) -> Self {
        let s: SharedString = s.into();
        self.subject = (!s.is_empty()).then_some(s);
        self
    }

    /// A subject whose words include mods: `plain` is its text (tooltip,
    /// and where the strip is too narrow for a subject), `rich` is what the
    /// strip draws, one line of inline elements that is clipped, not
    /// wrapped, at the strip's edge. Build the mod names with
    /// `widgets::mod_link` so each has its icon and opens its page.
    pub fn subject_rich(mut self, plain: impl Into<SharedString>, rich: impl IntoElement) -> Self {
        self = self.subject(plain);
        self.rich = self.subject.is_some().then(|| rich.into_any_element());
        self
    }

    /// Its main action, built with [`main_button`] so only the slot's first
    /// one is filled.
    pub fn primary<E: IntoElement>(mut self, f: impl FnOnce(Fit) -> E + 'static) -> Self {
        self.primary = Some(Box::new(move |fit| f(fit).into_any_element()));
        self
    }

    pub fn action(mut self, el: impl IntoElement + 'static) -> Self {
        self.actions.push(Box::new(move |_: Fit| el.into_any_element()));
        self
    }

    pub fn action_fit<E: IntoElement>(mut self, f: impl FnOnce(Fit) -> E + 'static) -> Self {
        self.actions.push(Box::new(move |fit| f(fit).into_any_element()));
        self
    }

    pub fn details(mut self, el: impl IntoElement) -> Self {
        self.details = Some(el.into_any_element());
        self
    }

    /// Opens the details the first time the notice shows ([`Notice::open_new`]).
    pub fn open_new(mut self) -> Self {
        self.open_new = true;
        self
    }

    pub fn dismiss(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.dismiss = Some(Rc::new(f));
        self
    }

    /// The ✕ hides it through the store's notice keys
    /// (`hopper::settings::notice_key`): it shows again when what it says
    /// changes, as before.
    pub fn dismiss_key(self, store: &Entity<crate::store::Store>, key: String) -> Self {
        let store = store.clone();
        self.dismiss(move |window, cx| {
            store.update(cx, |s, cx| s.hide_notice(&key, cx));
            window.refresh();
        })
    }

    pub fn since(mut self, t: i64) -> Self {
        self.since = t;
        self
    }
}

/// True when the notice keyed `key` was closed (and its text hasn't changed
/// since): a page leaves it out of the slot and the list.
pub fn hidden(store: &Entity<crate::store::Store>, key: &str, cx: &App) -> bool {
    store.read(cx).notice_hidden(key)
}

/// The order notices show in: by tier (stops play first), then the newest
/// first, then as given. Returns indices into `items` (tier, since).
pub fn order(items: &[(Tier, i64)]) -> Vec<usize> {
    let mut ix: Vec<usize> = (0..items.len()).collect();
    // Stable: equal notices keep the page's order.
    ix.sort_by(|&a, &b| items[a].0.cmp(&items[b].0).then(items[b].1.cmp(&items[a].1)));
    ix
}

/// The list's own state: which notices have their details open.
#[derive(Default)]
struct SlotState {
    open: HashSet<SharedString>,
    /// Notices whose details were opened by themselves ([`Notice::open_new`]),
    /// so closing them sticks.
    seen: HashSet<SharedString>,
}

/// Builds the page's notices again, for the popover (each frame it is open,
/// so its buttons and lists are current). A page makes it from the entities
/// that own its notices.
pub type Producer = Rc<dyn Fn(&mut Window, &mut App) -> Vec<Notice>>;

fn need(t: Tier) -> hopper::syncchip::Need {
    match t {
        Tier::Stop => hopper::syncchip::Need::Stop,
        Tier::Choice => hopper::syncchip::Need::Choice,
        Tier::Info => hopper::syncchip::Need::Info,
    }
}

/// The notices' groups for the list, one per tier in the order of
/// [`Tier::ALL`], empty ones left out: each tier with its notices' positions
/// in `tiers` (already ordered, see [`order`]).
pub fn group(tiers: &[Tier]) -> Vec<(Tier, Vec<usize>)> {
    Tier::ALL
        .iter()
        .filter_map(|&t| {
            let v: Vec<usize> = (0..tiers.len()).filter(|&i| tiers[i] == t).collect();
            (!v.is_empty()).then_some((t, v))
        })
        .collect()
}

/// The "Needs you" chip for the hero: every notice the page has that no
/// other chip says, in one popover (grouped Stops play / Needs a choice /
/// For your information, Details open in place, ✕ where there is one). `None`
/// when there are none. `id` names the page ("instance", "server") so each
/// keeps its own open details. A problem that stops play makes the chip red,
/// it never folds, and its popover opens once by itself.
///
/// `notices` only say what the chip looks like; the popover asks `producer`
/// for fresh ones whenever it draws.
pub fn chip(id: &str, notices: Vec<Notice>, producer: Producer) -> Option<crate::sync_chip::Chip> {
    use crate::sync_chip::{Chip, Section};
    let keys: Vec<(Tier, i64)> = notices.iter().map(|n| (n.tier, n.since)).collect();
    let ix = order(&keys);
    let needs: Vec<_> = ix.iter().map(|&i| need(notices[i].tier)).collect();
    let look = hopper::syncchip::attention_look(&needs, ix.first().map_or("", |&i| notices[i].title.as_ref()))?;
    // Every problem that stops play opens the popover once, each on its own.
    let stops: Vec<SharedString> = ix.iter().filter(|&&i| notices[i].tier == Tier::Stop).map(|&i| notices[i].id.clone()).collect();
    let key: SharedString = id.to_string().into();
    let body: crate::sync_chip::Body = Rc::new(move |window, cx| list(&key, &producer, true, window, cx));
    Some(Chip {
        id: format!("needs-{id}").into(),
        look,
        progress: None,
        sections: vec![Section { title: "".into(), line: "".into(), progress: None, body: Some(body), actions: Vec::new() }],
        auto_open: stops,
        width: Some(32.),
    })
}

/// The chip beside the run state pill while the game or server has a problem
/// (its start looks stuck, or it crashed): red, never folds, and its popover
/// opens once by itself each time a new problem shows. `notices` are the
/// problem's own (Restart, See why, Wait); the popover asks `producer` for
/// fresh ones whenever it draws. `None` when there is no problem.
pub fn run_chip(id: &str, problem: hopper::syncchip::RunProblem, notices: Vec<Notice>, producer: Producer) -> Option<crate::sync_chip::Chip> {
    use crate::sync_chip::{Chip, Section};
    if notices.is_empty() {
        return None;
    }
    // A stuck start that comes back after "Wait" is a new problem: it opens
    // again, once.
    let auto_open: Vec<SharedString> = notices.iter().map(|n| format!("{}@{}", n.id, n.since).into()).collect();
    let key: SharedString = format!("run:{id}").into();
    let body: crate::sync_chip::Body = Rc::new(move |window, cx| list(&key, &producer, false, window, cx));
    Some(Chip {
        id: format!("run-{id}").into(),
        look: hopper::syncchip::run_look(&problem),
        progress: None,
        sections: vec![Section { title: "".into(), line: "".into(), progress: None, body: Some(body), actions: Vec::new() }],
        auto_open,
        width: Some(38.),
    })
}

/// The popover's content: the page's notices as the grouped list (`grouped`),
/// or just their rows (the run state chip's one problem).
fn list(id: &SharedString, producer: &Producer, grouped: bool, window: &mut Window, cx: &mut App) -> AnyElement {
    let notices = producer(window, cx);
    let state = window.use_keyed_state(ElementId::Name(format!("notice-list:{id}").into()), cx, |_, _| SlotState::default());
    // Details that open by themselves, once each.
    for n in notices.iter().filter(|n| n.open_new && n.details.is_some()) {
        if !state.read(cx).seen.contains(&n.id) {
            let id = n.id.clone();
            state.update(cx, |s, _| {
                s.seen.insert(id.clone());
                s.open.insert(id);
            });
        }
    }
    let keys: Vec<(Tier, i64)> = notices.iter().map(|n| (n.tier, n.since)).collect();
    let mut slots: Vec<Option<Notice>> = notices.into_iter().map(Some).collect();
    let sorted: Vec<Notice> = order(&keys).into_iter().filter_map(|i| slots[i].take()).collect();
    let tiers: Vec<Tier> = sorted.iter().map(|n| n.tier).collect();
    let groups = group(&tiers);
    let n = sorted.len();
    let only_notes = tiers.iter().all(|t| *t == Tier::Info);
    let mut sorted: Vec<Option<Notice>> = sorted.into_iter().map(Some).collect();
    let open = state.read(cx).open.clone();
    let theme = cx.theme().clone();
    let rows: Vec<AnyElement> = groups
        .iter()
        .map(|(tier, ix)| {
            let items: Vec<AnyElement> = ix.iter().filter_map(|&i| sorted[i].take()).map(|n| list_row(n, &state, &open, cx)).collect();
            v_flex()
                .gap_0p5()
                .when(grouped, |g| {
                    g.child(
                        div()
                            .px_2()
                            .pt_1()
                            .text_xs()
                            .font_semibold()
                            .text_color(theme.muted_foreground)
                            .child(format!("{} · {}", tier.heading(), items.len())),
                    )
                })
                .children(items)
                .into_any_element()
        })
        .collect();
    v_flex()
        .id(SharedString::from(format!("notice-list:{id}")))
        .w_full()
        .max_h(rems(28.))
        .gap_1()
        .when(grouped, |l| {
            l.child(
                div()
                    .px_2()
                    .text_sm()
                    .font_semibold()
                    .text_color(theme.foreground)
                    .child(if only_notes { format!("{n} {}", if n == 1 { "note" } else { "notes" }) } else { "Needs you".to_string() }),
            )
        })
        .children(rows)
        .overflow_y_scrollbar()
        .into_any_element()
}

fn toggle(state: &Entity<SlotState>, id: SharedString, cx: &mut App) {
    state.update(cx, |s, cx| {
        if !s.open.remove(&id) {
            s.open.insert(id);
        }
        cx.notify();
    });
}

/// One row of the "N more" list: its tier's dot, title and subject, and its
/// main action; its details open under it.
fn list_row(n: Notice, state: &Entity<SlotState>, open: &HashSet<SharedString>, cx: &mut App) -> AnyElement {
    let theme = cx.theme().clone();
    let color = n.tier.color(cx);
    let id = n.id.clone();
    let actions: Vec<AnyElement> = n.actions.into_iter().map(|a| a(Fit::default())).collect();
    let has_details = n.details.is_some() || !actions.is_empty();
    let is_open = has_details && open.contains(&id);
    let toggle_btn = has_details.then(|| {
        let (state, id2) = (state.clone(), id.clone());
        Button::new(SharedString::from(format!("notice-details:{id}")))
            .xsmall()
            .ghost()
            .icon(if is_open { IconName::ChevronUp } else { IconName::ChevronDown })
            .tooltip(if is_open { "Hide details" } else { "Details" })
            .on_click(move |_, _, cx| toggle(&state, id2.clone(), cx))
    });
    let close = n.dismiss.filter(|_| n.tier != Tier::Stop).map(|f| {
        Button::new(SharedString::from(format!("notice-hide:{id}")))
            .xsmall()
            .ghost()
            .icon(IconName::X)
            .tooltip("Hide this notice")
            .on_click(move |_, window, cx| f(window, cx))
    });
    v_flex()
        .id(SharedString::from(format!("notice-row:{id}")))
        .gap_1()
        .px_2()
        .py_1p5()
        .rounded(theme.radius)
        .hover(|s| widgets::row_hover(s, cx))
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(div().flex_none().size(px(8.)).rounded_full().bg(color))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().font_semibold().truncate().child(n.title.clone()))
                        .when_some(n.subject.clone(), |c, s| match n.rich {
                            Some(rich) => c.child(
                                h_flex().min_w_0().overflow_hidden().items_center().gap_1().text_xs().text_color(theme.muted_foreground).child(rich),
                            ),
                            None => c.child(widgets::caption(s, cx).text_xs()),
                        }),
                )
                .child(
                    h_flex()
                        .flex_none()
                        .gap_1()
                        .items_center()
                        .children(n.primary.map(|p| p(Fit::default())))
                        // A row without details or ✕ keeps their room, so the
                        // buttons line up in one column down the list.
                        .child(toggle_btn.map_or_else(|| blank("notice-blank-d", &id), IntoElement::into_any_element))
                        .child(close.map_or_else(|| blank("notice-blank-x", &id), IntoElement::into_any_element)),
                ),
        )
        .when(is_open, |c| {
            c.child(
                v_flex()
                    .pl(px(16.))
                    .gap_2()
                    .when(!actions.is_empty(), |c| c.child(h_flex().flex_wrap().gap_1().children(actions)))
                    .children(n.details),
            )
        })
        .into_any_element()
}

/// An unseen icon button: holds a list row's missing ✕ or details place.
fn blank(key: &str, id: &SharedString) -> AnyElement {
    div()
        .invisible()
        .child(Button::new(SharedString::from(format!("{key}:{id}"))).xsmall().ghost().icon(IconName::X))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's `test` attribute would shadow the built-in one.
    use super::{Tier, order};

    #[test]
    fn order_puts_stops_first_then_newest() {
        let items = [(Tier::Info, 50), (Tier::Stop, 10), (Tier::Choice, 99), (Tier::Stop, 20), (Tier::Info, 60)];
        assert_eq!(order(&items), vec![3, 1, 2, 4, 0]);
    }

    #[test]
    fn order_keeps_page_order_for_ties() {
        let items = [(Tier::Choice, 0), (Tier::Choice, 0), (Tier::Stop, 0), (Tier::Choice, 0)];
        assert_eq!(order(&items), vec![2, 0, 1, 3]);
    }
}
