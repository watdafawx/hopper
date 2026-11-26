//! The notices of an instance's or a server's page (a load problem, a choice
//! the player has to make, a tip). They look the same everywhere and never
//! move the page: they are docked at the bottom edge of its content
//! ([`dock`]), under the list, so the hero, the tabs, the toolbar and the
//! list's first row stay where they are. The hero keeps a summary: [`chip`]
//! is the "Needs you" chip, [`run_chip`] the run state's problem (a start
//! that looks stuck, a crash) next to its pill; clicking either flashes its
//! strip in the dock.
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

/// The "Needs you" chip for the hero: a summary of the notices in the page's
/// dock ([`dock`]) that no other chip says. `None` when there are none.
/// `dock_key` names the page's dock ("instance:<root>", "server:<root>"). A
/// problem that stops play makes the chip red and it never folds. Clicking it
/// scrolls the dock to the notice it names and flashes it: it opens nothing
/// over the page.
pub fn chip(dock_key: &str, notices: &[Notice]) -> Option<crate::sync_chip::Chip> {
    let keys: Vec<(Tier, i64)> = notices.iter().map(|n| (n.tier, n.since)).collect();
    let ix = order(&keys);
    let needs: Vec<_> = ix.iter().map(|&i| need(notices[i].tier)).collect();
    let look = hopper::syncchip::attention_look(&needs, ix.first().map_or("", |&i| notices[i].title.as_ref()))?;
    let first = ix.first().map(|&i| notices[i].id.clone());
    Some(summary_chip(format!("needs-{dock_key}"), look, dock_key, first))
}

/// The chip beside the run state pill while the game or server has a problem
/// (its start looks stuck, or it crashed): red and never folds. Its notices
/// (Restart, See why, Wait) are strips in the page's dock like every other;
/// the chip says what it is and a click brings its strip into view. `chip_key`
/// tells this chip from another run chip on the same page (the instance's own
/// stuck start and its paired server's crash). `None` when there is no problem.
pub fn run_chip(dock_key: &str, chip_key: &str, problem: hopper::syncchip::RunProblem, notices: &[Notice]) -> Option<crate::sync_chip::Chip> {
    let first = notices.first()?.id.clone();
    Some(summary_chip(format!("run-{chip_key}"), hopper::syncchip::run_look(&problem), dock_key, Some(first)))
}

/// A hero chip that stands for strips in the dock: no popover, a click
/// flashes the strip.
fn summary_chip(id: String, look: hopper::syncchip::Look, dock_key: &str, notice: Option<SharedString>) -> crate::sync_chip::Chip {
    let dock_key = dock_key.to_string();
    crate::sync_chip::Chip {
        id: id.into(),
        look,
        progress: None,
        sections: Vec::new(),
        auto_open: Vec::new(),
        width: None,
        on_click: Some(Rc::new(move |_, cx| flash(&dock_key, notice.as_deref(), false, cx))),
    }
}

/// The strip a chip or a row marker asked the dock to show: (dock key, notice
/// id or the first one, open its details, request number). The number lets the
/// dock act on each request once. It lives in an entity, not a plain static:
/// the dock reads it while it draws, so a request redraws the page's cached
/// view (a window refresh alone reuses the cached picture).
#[derive(Default)]
struct FlashAsk {
    ask: Option<(String, Option<SharedString>, bool, u64)>,
    count: u64,
}

fn flash_ask(cx: &mut App) -> Entity<FlashAsk> {
    if let Some(e) = FLASH.with(|f| f.borrow().clone()) {
        return e;
    }
    let e = cx.new(|_| FlashAsk::default());
    FLASH.with(|f| *f.borrow_mut() = Some(e.clone()));
    e
}

thread_local! {
    static FLASH: std::cell::RefCell<Option<Entity<FlashAsk>>> = const { std::cell::RefCell::new(None) };
    /// Every `dock|notice@since` of a problem that stops play whose details
    /// already opened by themselves. It outlives the dock's own state (dropped
    /// when the player visits another page), so coming back doesn't open them
    /// a second time.
    static STOP_SEEN: std::cell::RefCell<HashSet<String>> = std::cell::RefCell::new(HashSet::new());
}

/// Asks the dock keyed `dock_key` to scroll to the notice `notice` (the first
/// one when `None` or when it has no such notice) and flash it; `open` also
/// opens its details.
pub fn flash(dock_key: &str, notice: Option<&str>, open: bool, cx: &mut App) {
    let want = (dock_key.to_string(), notice.map(|s| SharedString::from(s.to_string())), open);
    flash_ask(cx).update(cx, |f, cx| {
        f.count += 1;
        f.ask = Some((want.0, want.1, want.2, f.count));
        cx.notify();
    });
}

/// The dock's own state: open details, the strip asked for and its scroll.
#[derive(Default)]
struct DockState {
    slot: SlotState,
    scroll: ScrollHandle,
    /// The last flash request this dock acted on.
    done: u64,
    /// The strip flashing now, with its request number (it keys the
    /// animation, so every click flashes again).
    flashing: Option<(SharedString, u64)>,
}

/// How long a strip flashes after its chip is clicked.
const FLASH_MS: u64 = 1400;

/// The page's notices docked at the bottom edge of its content, under the
/// scrolling list: one strip each, ordered by [`order`] (what stops play
/// first). Nothing above it moves when a notice comes or goes; only the list's
/// bottom edge gets shorter. A strip says its title and main button on one
/// line; its other buttons and details open under it (the dock grows upward).
/// A problem that stops play opens its details by itself once. `None` when the
/// page has no notice.
pub fn dock(dock_key: &str, notices: Vec<Notice>, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    if notices.is_empty() {
        return None;
    }
    let state = window.use_keyed_state(ElementId::Name(format!("notice-dock:{dock_key}").into()), cx, |_, _| DockState::default());
    let keys: Vec<(Tier, i64)> = notices.iter().map(|n| (n.tier, n.since)).collect();
    let mut slots: Vec<Option<Notice>> = notices.into_iter().map(Some).collect();
    let sorted: Vec<Notice> = order(&keys).into_iter().filter_map(|i| slots[i].take()).collect();
    // Details that open by themselves, once each: a list the player needs to
    // see (`open_new`), and every problem that stops play.
    for n in &sorted {
        if n.details.is_none() && n.actions.is_empty() {
            continue;
        }
        let fresh = if n.tier == Tier::Stop {
            STOP_SEEN.with(|s| s.borrow_mut().insert(format!("{dock_key}|{}@{}", n.id, n.since)))
        } else {
            n.open_new && !state.read(cx).slot.seen.contains(&n.id)
        };
        if fresh {
            let id = n.id.clone();
            state.update(cx, |s, _| {
                s.slot.seen.insert(id.clone());
                s.slot.open.insert(id);
            });
        }
    }
    // A chip or a row marker asked for one of these strips.
    let ask = flash_ask(cx).read(cx).ask.clone().filter(|(k, _, _, n)| k == dock_key && *n != state.read(cx).done);
    if let Some((_, want, open, n)) = ask {
        let at = want.as_ref().and_then(|w| sorted.iter().position(|x| x.id == *w)).unwrap_or(0);
        let id = sorted[at].id.clone();
        state.update(cx, |s, _| {
            s.done = n;
            s.flashing = Some((id.clone(), n));
            if open {
                s.slot.open.insert(id);
            }
            s.scroll.scroll_to_item(at);
        });
    }
    let theme = cx.theme().clone();
    let narrow = window.viewport_size().width < px(1000.);
    let (open, flashing, scroll) = {
        let s = state.read(cx);
        (s.slot.open.clone(), s.flashing.clone(), s.scroll.clone())
    };
    // The region's one filled button: the first strip's main action.
    let lead = sorted.iter().position(|n| n.primary.is_some());
    let strips: Vec<AnyElement> = sorted
        .into_iter()
        .enumerate()
        .map(|(i, n)| {
            let fit = Fit { narrow, lead: Some(i) == lead };
            let flash = flashing.as_ref().filter(|(id, _)| *id == n.id).map(|(_, g)| *g);
            strip(n, fit, &state, &open, flash, cx)
        })
        .collect();
    // The dock never takes more than about half the page: past that it
    // scrolls inside itself.
    let room = (window.viewport_size().height * 0.45).max(px(160.));
    Some(
        div()
            .id(SharedString::from(format!("notice-dock:{dock_key}")))
            .flex_none()
            .w_full()
            .max_h(room)
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.background)
            .px_3()
            .py_2()
            .child(v_flex().w_full().gap_1p5().children(strips))
            .into_any_element(),
    )
}

/// One strip of the dock: its colour bar (red: stops play, amber: the rest),
/// the title and subject on one line, the main button, and the chevron and ✕;
/// its other buttons and details open under it. In a narrow page the sentence
/// keeps its width and the button wraps under it.
fn strip(n: Notice, fit: Fit, state: &Entity<DockState>, open: &HashSet<SharedString>, flash: Option<u64>, cx: &mut App) -> AnyElement {
    let theme = cx.theme().clone();
    let color = match n.tier {
        Tier::Stop => theme.danger,
        _ => theme.warning,
    };
    let id = n.id.clone();
    let actions: Vec<AnyElement> = n.actions.into_iter().map(|a| a(Fit { narrow: fit.narrow, lead: false })).collect();
    let has_more = n.details.is_some() || !actions.is_empty();
    let is_open = has_more && open.contains(&id);
    let toggle_btn = has_more.then(|| {
        let (state, id2) = (state.clone(), id.clone());
        Button::new(SharedString::from(format!("notice-details:{id}")))
            .xsmall()
            .ghost()
            // The details open upward-looking from the dock: the chevron says
            // which way the strip grows.
            .icon(if is_open { IconName::ChevronDown } else { IconName::ChevronUp })
            .tooltip(if is_open { "Hide details" } else { "Details" })
            .on_click(move |_, _, cx| {
                state.update(cx, |s, cx| {
                    if !s.slot.open.remove(&id2) {
                        s.slot.open.insert(id2.clone());
                    }
                    cx.notify();
                })
            })
    });
    let close = n.dismiss.filter(|_| n.tier != Tier::Stop).map(|f| {
        Button::new(SharedString::from(format!("notice-hide:{id}")))
            .xsmall()
            .ghost()
            .icon(IconName::X)
            .tooltip("Hide this notice")
            .on_click(move |_, window, cx| f(window, cx))
    });
    let tip: SharedString = match &n.subject {
        Some(s) => format!("{}. {s}", n.title).into(),
        None => n.title.clone(),
    };
    // Narrow, the subject goes (it's in the tooltip) so the title keeps its room.
    let subject = n.subject.clone().filter(|_| !fit.narrow);
    let rich = n.rich;
    let line = h_flex()
        .id(SharedString::from(format!("notice-line:{id}")))
        .flex_1()
        .min_w(rems(12.))
        .h(rems(1.75))
        .overflow_hidden()
        .gap_1p5()
        .items_center()
        .whitespace_nowrap()
        .text_sm()
        .when_some(n.icon, |l, ic| l.child(gpui_kit::component::Icon::new(ic).size(px(14.)).flex_none().text_color(color)))
        .child(div().min_w_0().font_semibold().truncate().child(n.title.clone()))
        .when_some(subject, |l, s| {
            l.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .truncate()
                    .text_color(theme.muted_foreground)
                    .map(|d| match rich {
                        Some(r) => d.child(r),
                        None => d.child(s),
                    }),
            )
        })
        .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx));
    let head = h_flex()
        .w_full()
        .gap_1()
        .items_start()
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .flex_wrap()
                .gap_x_3()
                .gap_y_1()
                .items_center()
                .child(line)
                .children(n.primary.map(|p| h_flex().flex_none().child(p(fit)))),
        )
        .children(toggle_btn.map(|b| div().flex_none().pt(px(2.)).child(b)))
        .children(close.map(|b| div().flex_none().pt(px(2.)).child(b)));
    let base = theme.secondary;
    let body = v_flex()
        .id(SharedString::from(format!("notice-strip:{id}")))
        .relative()
        .w_full()
        .gap_2()
        .pl(px(14.))
        .pr_1p5()
        .py_1()
        .rounded(theme.radius)
        .bg(base)
        .overflow_hidden()
        .child(div().absolute().left_0().top_0().bottom_0().w(px(3.)).bg(color))
        .child(head)
        .when(is_open, |c| {
            c.child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .pb_1()
                    .pr_1p5()
                    .when(!actions.is_empty(), |c| c.child(h_flex().flex_wrap().gap_1().children(actions)))
                    .children(n.details),
            )
        });
    match flash {
        // The chip asked for this strip: a tint of its colour that swells
        // and fades.
        Some(g) => body
            .with_animation(
                ElementId::Name(format!("notice-flash:{id}:{g}").into()),
                Animation::new(std::time::Duration::from_millis(FLASH_MS)).with_easing(ease_in_out),
                move |el, t| {
                    let pulse = if t < 0.5 { t * 2. } else { (1. - t) * 2. };
                    el.bg(base.blend(color.opacity(0.35 * pulse)))
                },
            )
            .into_any_element(),
        None => body.into_any_element(),
    }
}

/// The popover body of a chip that owns one page notice (the group chip): its
/// rows as the list shows them, without the tier headings.
pub fn body(id: &str, producer: Producer) -> crate::sync_chip::Body {
    let key: SharedString = format!("body:{id}").into();
    Rc::new(move |window, cx| list(&key, &producer, false, window, cx))
}

/// The ids of the notices that stop play: a chip that has one opens its
/// popover once by itself.
pub fn stop_ids(notices: &[Notice]) -> Vec<SharedString> {
    notices.iter().filter(|n| n.tier == Tier::Stop).map(|n| n.id.clone()).collect()
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
                                div().w_full().min_w_0().text_xs().text_color(theme.muted_foreground).child(rich),
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
