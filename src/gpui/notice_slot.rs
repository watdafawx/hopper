//! The one notice slot above an instance's or a server's tabs. Every notice
//! the page has (a load problem, a choice the player has to make, a tip)
//! goes through here, so they look the same, sit in one place and never
//! push the page off the screen: the two that matter most show as one-line
//! strips, and the rest fold into "N more", which opens a list grouped by
//! how much they matter.
//!
//! A page builds its notices as [`Notice`]s and hands them to [`slot`]. The
//! slot orders them (see [`order`]): problems that stop the game first, then
//! the ones that need a choice, then the rest; the newest first within each.
//! A notice that stops play has no ✕: it leaves when its cause does.

use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
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

    fn icon(self) -> IconName {
        match self {
            Tier::Stop => IconName::TriangleAlert,
            Tier::Choice => IconName::CircleAlert,
            Tier::Info => IconName::Info,
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
    /// A thin bar along the strip's bottom edge while something runs.
    pub progress: Option<f32>,
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
            progress: None,
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

    pub fn progress(mut self, p: Option<f32>) -> Self {
        self.progress = p.map(|p| p.clamp(0., 1.));
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

/// How many notices show as strips; the rest fold into "N more".
pub const SHOWN: usize = 2;

/// The order notices show in: by tier (stops play first), then the newest
/// first, then as given. Returns indices into `items` (tier, since).
pub fn order(items: &[(Tier, i64)]) -> Vec<usize> {
    let mut ix: Vec<usize> = (0..items.len()).collect();
    // Stable: equal notices keep the page's order.
    ix.sort_by(|&a, &b| items[a].0.cmp(&items[b].0).then(items[b].1.cmp(&items[a].1)));
    ix
}

/// Splits ordered notices (their tiers, in order) into the strips shown and
/// the "N more" list's groups: each group's tier with its notices' positions
/// in `tiers`, in the order of [`Tier::ALL`], empty groups left out.
pub fn fold(tiers: &[Tier]) -> (Vec<usize>, Vec<(Tier, Vec<usize>)>) {
    let shown: Vec<usize> = (0..tiers.len().min(SHOWN)).collect();
    let rest = SHOWN.min(tiers.len())..tiers.len();
    let groups = Tier::ALL
        .iter()
        .filter_map(|&t| {
            let v: Vec<usize> = rest.clone().filter(|&i| tiers[i] == t).collect();
            (!v.is_empty()).then_some((t, v))
        })
        .collect();
    (shown, groups)
}

/// The slot's own state: which notices have their details open, and its
/// width (for the narrow layout).
#[derive(Default)]
struct SlotState {
    open: HashSet<SharedString>,
    width: Pixels,
}

/// Under this width the strips drop their subject line, shorten labels and
/// "N more" becomes "+N".
const NARROW_REM: f32 = 34.;

/// Under this width the strips drop only their subject line (it's in the
/// title's tooltip): a long title and a long button leave it a few letters.
const SUBJECT_REM: f32 = 46.;

/// The slot itself: `None` when there's nothing to say. `id` names the
/// page ("instance", "server") so each page keeps its own open details.
pub fn slot(id: &str, notices: Vec<Notice>, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    if notices.is_empty() {
        return None;
    }
    let state = window.use_keyed_state(ElementId::Name(format!("notice-slot:{id}").into()), cx, |_, _| SlotState::default());
    let width = state.read(cx).width;
    let narrow = width > px(0.) && width < window.rem_size() * NARROW_REM;
    let subjects = !(width > px(0.) && width < window.rem_size() * SUBJECT_REM);
    let keys: Vec<(Tier, i64)> = notices.iter().map(|n| (n.tier, n.since)).collect();
    let mut slots: Vec<Option<Notice>> = notices.into_iter().map(Some).collect();
    let sorted: Vec<Notice> = order(&keys).into_iter().filter_map(|i| slots[i].take()).collect();
    let tiers: Vec<Tier> = sorted.iter().map(|n| n.tier).collect();
    let (shown, groups) = fold(&tiers);
    let more = tiers.len() - shown.len();
    let mut sorted: Vec<Option<Notice>> = sorted.into_iter().map(Some).collect();
    let open = state.read(cx).open.clone();
    let mut strips: Vec<AnyElement> = Vec::new();
    let last = shown.len().saturating_sub(1);
    // The list first: its trigger sits on the last strip.
    let list = (more > 0).then(|| {
        let rows: Vec<AnyElement> = groups
            .iter()
            .map(|(tier, ix)| {
                let items: Vec<AnyElement> =
                    ix.iter().filter_map(|&i| sorted[i].take()).map(|n| list_row(n, &state, &open, cx)).collect();
                v_flex()
                    .gap_0p5()
                    .child(
                        div()
                            .px_2()
                            .pt_1()
                            .text_xs()
                            .font_semibold()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{} · {}", tier.heading(), items.len())),
                    )
                    .children(items)
                    .into_any_element()
            })
            .collect();
        let dots: Vec<Tier> = groups.iter().flat_map(|(t, v)| std::iter::repeat_n(*t, v.len())).collect();
        more_button(id, more, &dots, rows, narrow, window, cx)
    });
    let mut list = list;
    for (k, &i) in shown.iter().enumerate() {
        let Some(n) = sorted[i].take() else { continue };
        let trigger = if k == last { list.take() } else { None };
        strips.push(strip(n, Fit { narrow, lead: k == 0 }, subjects, trigger, &state, &open, cx));
    }
    let st = state.clone();
    Some(
        v_flex()
            .id(SharedString::from(format!("notice-slot-{id}")))
            .w_full()
            .gap_1p5()
            .children(strips)
            // Its width, for the narrow layout (from the last frame).
            .child(
                canvas(
                    move |bounds, window, cx| {
                        let w = bounds.size.width;
                        if (st.read(cx).width - w).abs() > px(0.5) {
                            st.update(cx, |s, _| s.width = w);
                            window.refresh();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0()
                .size_full(),
            )
            .relative()
            .into_any_element(),
    )
}

fn toggle(state: &Entity<SlotState>, id: SharedString, cx: &mut App) {
    state.update(cx, |s, cx| {
        if !s.open.remove(&id) {
            s.open.insert(id);
        }
        cx.notify();
    });
}

/// One notice as a one-line strip: icon, title, subject, its buttons, and
/// on the last strip the "N more" button. Its details open under it.
fn strip(mut n: Notice, fit: Fit, subjects: bool, more: Option<AnyElement>, state: &Entity<SlotState>, open: &HashSet<SharedString>, cx: &mut App) -> AnyElement {
    let theme = cx.theme().clone();
    // No room for the subject on the strip: its mod names (links with icons)
    // go in the details instead of only the tooltip.
    let rich_below = if subjects { None } else { n.rich.take() };
    let color = n.tier.color(cx);
    // The title on the strip's own tint (`bg` below), readable in any theme.
    let ink = crate::theme::ink_over(color, theme.background, 0.08);
    let id = n.id.clone();
    let narrow = fit.narrow;
    // Narrow: the other buttons go in the details, so the strip stays one line.
    let actions: Vec<AnyElement> = n.actions.into_iter().map(|a| a(Fit { narrow, lead: false })).collect();
    let tucked = narrow && !actions.is_empty();
    let (actions, tucked_actions) = if narrow { (Vec::new(), actions) } else { (actions, Vec::new()) };
    let has_details = n.details.is_some() || tucked || rich_below.is_some();
    let is_open = has_details && open.contains(&id);
    let tip: SharedString = match &n.subject {
        Some(s) => format!("{}\n{s}", n.title).into(),
        None => n.title.clone(),
    };
    let toggle_btn = has_details.then(|| {
        let (state, id2) = (state.clone(), id.clone());
        let b = Button::new(SharedString::from(format!("notice-details:{id}")))
            .small()
            .ghost()
            .icon(if is_open { IconName::ChevronUp } else { IconName::ChevronDown });
        (if narrow { b.tooltip(if is_open { "Hide details" } else { "Details" }) } else { b.label("Details") })
            .on_click(move |_, _, cx| toggle(&state, id2.clone(), cx))
    });
    let close = n.dismiss.filter(|_| n.tier != Tier::Stop).map(|f| {
        Button::new(SharedString::from(format!("notice-hide:{id}")))
            .small()
            .ghost()
            .icon(IconName::X)
            .tooltip("Hide this notice")
            .on_click(move |_, window, cx| f(window, cx))
    });
    let line = h_flex()
        .id(SharedString::from(format!("notice-strip:{id}")))
        .relative()
        .w_full()
        // One line at its own height: open details under it mustn't
        // squeeze it.
        .flex_none()
        .h(rems(2.25))
        .gap_2()
        .pl_3()
        .pr_1p5()
        .items_center()
        .overflow_hidden()
        .rounded(theme.radius)
        .border_1()
        .border_color(color.opacity(0.45))
        .bg(color.opacity(0.08))
        .child(Icon::new(n.icon.unwrap_or(n.tier.icon())).small().flex_none().text_color(color))
        .child(
            div()
                .id(SharedString::from(format!("notice-title:{id}")))
                .min_w_0()
                .when(!subjects || n.subject.is_none(), |d| d.flex_1())
                .truncate()
                .text_sm()
                .font_semibold()
                .text_color(ink)
                .child(n.title.clone())
                .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx)),
        )
        .when_some(n.subject.filter(|_| subjects), |r, s| match n.rich {
            // Mod names with their icons: one line, clipped at the edge.
            Some(rich) => r.child(
                h_flex()
                    .flex_1()
                    .min_w(rems(4.))
                    .overflow_hidden()
                    .gap_1()
                    .items_center()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(div().flex_none().child("·"))
                    .child(rich),
            ),
            None => r.child(div().flex_1().min_w(rems(4.)).truncate().text_sm().text_color(theme.muted_foreground).child(format!("· {s}"))),
        })
        .child(
            h_flex()
                .flex_none()
                .gap_1()
                .items_center()
                .children(n.primary.map(|p| p(fit)))
                .children(actions)
                .children(toggle_btn)
                .children(close)
                .children(more),
        )
        .when_some(n.progress, |r, p| {
            r.child(div().absolute().left_0().bottom_0().h(px(2.)).w(relative(p)).bg(color))
        });
    v_flex()
        .w_full()
        .gap_1()
        .child(line)
        .when(is_open, |c| {
            c.child(
                v_flex()
                    .id(SharedString::from(format!("notice-more:{id}")))
                    .w_full()
                    .max_h(rems(16.))
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(color.opacity(0.25))
                    .bg(color.opacity(0.04))
                    .children(rich_below.map(|r| h_flex().w_full().overflow_hidden().text_sm().text_color(theme.muted_foreground).child(r)))
                    .when(!tucked_actions.is_empty(), |c| c.child(h_flex().flex_wrap().gap_1().children(tucked_actions)))
                    .children(n.details)
                    .overflow_y_scrollbar(),
            )
        })
        .into_any_element()
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

/// "●●● 5 more ⌃" on the last strip (narrow: "+5"), opening the list.
fn more_button(id: &str, n: usize, dots: &[Tier], rows: Vec<AnyElement>, narrow: bool, _: &mut Window, cx: &mut App) -> AnyElement {
    let theme = cx.theme().clone();
    // One dot per notice, up to five (the label says the count).
    let marks: Vec<AnyElement> = dots
        .iter()
        .take(5)
        .map(|t| div().flex_none().size(px(6.)).rounded_full().bg(t.color(cx)).into_any_element())
        .collect();
    let label = if narrow { format!("+{n}") } else { format!("{n} more") };
    let trigger = Button::new(SharedString::from(format!("notice-more-btn:{id}")))
        .small()
        .outline()
        .tooltip(format!("{} more {}", n, if n == 1 { "notice" } else { "notices" }))
        .child(h_flex().gap_1().items_center().child(h_flex().gap(px(2.)).children(if narrow { marks.into_iter().take(1).collect() } else { marks })).child(label));
    Popover::new(SharedString::from(format!("notice-more:{id}")))
        .anchor(Anchor::TopRight)
        .trigger(trigger)
        .child(
            v_flex()
                .id(SharedString::from(format!("notice-list:{id}")))
                .w(rems(32.))
                .max_w(rems(32.))
                .max_h(rems(28.))
                .gap_1()
                .child(
                    div()
                        .px_2()
                        .text_sm()
                        .font_semibold()
                        .text_color(theme.foreground)
                        .child(format!("{n} more {}", if n == 1 { "notice" } else { "notices" })),
                )
                .children(rows)
                .overflow_y_scrollbar(),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's `test` attribute would shadow the built-in one.
    use super::{Tier, fold, order};

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

    #[test]
    fn fold_shows_two_and_groups_the_rest() {
        let tiers = [Tier::Stop, Tier::Stop, Tier::Choice, Tier::Choice, Tier::Choice, Tier::Info, Tier::Info];
        let (shown, groups) = fold(&tiers);
        assert_eq!(shown, vec![0, 1]);
        assert_eq!(groups, vec![(Tier::Choice, vec![2, 3, 4]), (Tier::Info, vec![5, 6])]);
    }

    #[test]
    fn fold_one_or_two_has_no_list() {
        assert_eq!(fold(&[Tier::Info]), (vec![0], vec![]));
        assert_eq!(fold(&[Tier::Stop, Tier::Info]), (vec![0, 1], vec![]));
        assert_eq!(fold(&[]), (vec![], vec![]));
    }

    #[test]
    fn fold_keeps_a_third_stop_in_the_list() {
        // A third problem that stops play is never dropped: it heads the list.
        let (shown, groups) = fold(&[Tier::Stop, Tier::Stop, Tier::Stop, Tier::Info]);
        assert_eq!(shown, vec![0, 1]);
        assert_eq!(groups, vec![(Tier::Stop, vec![2]), (Tier::Info, vec![3])]);
    }
}
