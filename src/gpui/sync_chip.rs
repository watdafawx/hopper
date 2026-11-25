//! The sync chip in an instance's or a server's hero: where a pack stands
//! with its friends group or its server, in one 22 px chip beside the run
//! state. What it says and when it folds is decided in `hopper::syncchip`
//! (one function for both apps); this draws it.
//!
//! Click opens a popover with the details and the actions (Pull now, Push,
//! ...). A state that needs the player (a choice, mods to get, changes to
//! pull or push) shows its notice (`group_pack::group_status_bar`) there, with
//! its buttons.
//!
//! The chip is the same width in a state and its next one only by
//! accident: it sits last in the pill row, so a new width moves nothing
//! below the hero.

use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Selectable, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::groupstatus::Tone;
use hopper::syncchip::{Form, Glyph, Look};
use std::collections::HashSet;
use std::rc::Rc;

/// The chip's height in rem: the same as `widgets::pill`, so the row is one
/// height with or without it.
const HEIGHT: f32 = 1.375;

pub type Run = Rc<dyn Fn(&mut Window, &mut App)>;

/// A button in the popover.
#[derive(Clone)]
pub struct Action {
    pub label: SharedString,
    pub icon: Option<IconName>,
    /// The one filled button of the popover.
    pub primary: bool,
    pub disabled: bool,
    pub run: Run,
}

/// One side's part of the popover: what it says and what can be done.
#[derive(Clone)]
pub struct Section {
    pub title: SharedString,
    pub line: SharedString,
    /// A bar under the line (0..=1) while something is taken in.
    pub progress: Option<f32>,
    /// More to show under the line (a list with mod links, say), drawn when
    /// the popover opens.
    pub body: Option<Body>,
    pub actions: Vec<Action>,
}

pub type Body = Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;

/// One chip: how it looks and what its popover says.
#[derive(Clone)]
pub struct Chip {
    pub id: SharedString,
    pub look: Look,
    /// 0..=1: a thin line along the chip's foot while it is busy.
    pub progress: Option<f32>,
    pub sections: Vec<Section>,
    /// Keys that open the popover by themselves the first time each shows
    /// (a problem that stops play). It floats over the page and pushes
    /// nothing, and it doesn't take the keyboard from a field the player is
    /// typing in. Closing it sticks until a key it hasn't seen comes.
    pub auto_open: Vec<SharedString>,
    /// The popover's width in rem, when the default doesn't fit its content.
    pub width: Option<f32>,
}

impl Chip {
    /// The one chip standing for two (a group's and a server's) when the
    /// header has no room for both: the worse tone, "Sync", both sections.
    pub fn merge(a: &Chip, b: &Chip) -> Chip {
        Chip {
            id: "sync-both".into(),
            look: hopper::syncchip::merged(&a.look, &b.look),
            progress: a.progress.or(b.progress),
            sections: a.sections.iter().chain(b.sections.iter()).cloned().collect(),
            auto_open: Vec::new(),
            width: None,
        }
    }
}

pub fn tone_color(tone: Tone, cx: &App) -> Hsla {
    let t = cx.theme();
    match tone {
        Tone::Good => t.success,
        Tone::Busy => t.info,
        Tone::Warn => t.warning,
        Tone::Bad => t.danger,
        Tone::Quiet => t.muted_foreground,
    }
}

pub fn glyph_icon(g: Glyph) -> IconName {
    match g {
        Glyph::Check => IconName::CircleCheck,
        Glyph::Spin => IconName::LoaderCircle,
        Glyph::Behind => IconName::Clock,
        Glyph::Offline => IconName::CircleOff,
        Glyph::Alert => IconName::CircleAlert,
        Glyph::Down => IconName::Download,
        Glyph::Up => IconName::Upload,
        Glyph::Differ => IconName::ArrowLeftRight,
        Glyph::Restart => IconName::RefreshCw,
        Glyph::Note => IconName::Info,
        Glyph::Stuck => IconName::Clock,
    }
}

/// How long a busy state has to last before it shows, and how long it stays
/// (`hopper::settle`), so a check that ends in a blink changes nothing.
struct Settled {
    settle: hopper::settle::Settle,
    t0: std::time::Instant,
    /// A wake-up is already waiting: one is enough.
    waiting: bool,
    busy: Option<Chip>,
    rest: Option<Chip>,
}

/// `chip` as the header should draw it now: a busy state only once it has
/// lasted, and for a while after; until then the state it came from.
/// `None` when there is nothing settled to show yet.
pub fn settled(chip: Chip, window: &mut Window, cx: &mut App) -> Option<Chip> {
    let state = window.use_keyed_state(ElementId::Name(format!("sync-chip-settle:{}", chip.id).into()), cx, |_, _| Settled {
        settle: hopper::settle::Settle::default(),
        t0: std::time::Instant::now(),
        waiting: false,
        busy: None,
        rest: None,
    });
    let busy_now = chip.look.busy;
    let (step, shown) = state.update(cx, |s, _| {
        let step = s.settle.step(busy_now, s.t0.elapsed());
        if busy_now {
            s.busy = Some(chip.clone());
        } else {
            s.rest = Some(chip.clone());
        }
        let shown = if step.show { s.busy.clone() } else { s.rest.clone() };
        (step, shown)
    });
    // Ask again when the answer changes by itself (the delay ran out, or
    // the hold is over).
    if let Some(wait) = step.wake_in
        && !state.read(cx).waiting
    {
        state.update(cx, |s, _| s.waiting = true);
        let st = state.clone();
        window
            .spawn(cx, async move |cx| {
                cx.background_executor().timer(wait + std::time::Duration::from_millis(20)).await;
                cx.update(|window, cx| {
                    st.update(cx, |s, _| s.waiting = false);
                    window.refresh();
                })
                .ok();
            })
            .detach();
    }
    // The state that is showing is current for its text and actions, but a
    // held busy look keeps its own words.
    shown.map(|mut s| {
        if !step.show && !s.look.busy {
            s.sections = chip.sections.clone();
        }
        s
    })
}

/// The trigger the popover wraps: the chip itself.
#[derive(IntoElement)]
struct Trigger {
    id: ElementId,
    look: Look,
    form: Form,
    progress: Option<f32>,
    selected: bool,
    open: bool,
}

impl Selectable for Trigger {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
    fn is_selected(&self) -> bool {
        self.selected
    }
    fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }
    fn is_open(&self) -> bool {
        self.open
    }
}

impl RenderOnce for Trigger {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = tone_color(self.look.tone, cx);
        let ink = crate::theme::chip_ink(color);
        let text: Option<SharedString> = match self.form {
            Form::Full => Some(self.look.label.clone().into()),
            Form::Short => Some(self.look.short.clone().into()),
            Form::Icon => None,
        };
        let tip: SharedString = self.look.label.clone().into();
        let icon = Icon::new(glyph_icon(self.look.glyph)).size(px(14.)).flex_none();
        let icon = if self.look.glyph == Glyph::Spin {
            icon.with_animation(
                "sync-chip-spin",
                Animation::new(std::time::Duration::from_millis(900)).repeat(),
                |i, t| i.transform(Transformation::rotate(percentage(t))),
            )
            .into_any_element()
        } else {
            icon.into_any_element()
        };
        let tint = if self.open { 0.26 } else { widgets::alpha::TINT };
        let busy = self.look.busy;
        let progress = self.progress.filter(|_| busy);
        div()
            .id(self.id)
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .overflow_hidden()
            .whitespace_nowrap()
            .gap_1p5()
            .h(rems(HEIGHT))
            .map(|d| if text.is_some() { d.px_2p5() } else { d.px_1p5() })
            .rounded(crate::theme::radius_sm())
            .bg(color.opacity(tint))
            .text_color(ink)
            .text_xs()
            // The UI font's tall ascent leaves centred capitals about a
            // pixel low; a little more room below lifts them to the middle.
            .pb(px(2.))
            .font_weight(FontWeight::SEMIBOLD)
            .clickable(cx)
            .child(icon)
            .children(text.map(|t| div().child(t)))
            .when_some(progress, |d, p| {
                d.child(
                    div()
                        .absolute()
                        .left_0()
                        .bottom_0()
                        .h(px(2.))
                        .w(relative(p.clamp(0.0, 1.0)))
                        .bg(color),
                )
            })
            .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
    }
}

use widgets::Clickable as _;

/// The chip in `form`, opening its popover on click.
pub fn render(chip: &Chip, form: Form, _cx: &App) -> AnyElement {
    ChipEl { chip: chip.clone(), form }.into_any_element()
}

/// Whether a chip's popover is open. Only tracked for a chip that has
/// `auto_open` keys, or that something asked to open ([`request_open`]).
#[derive(Default)]
struct Auto {
    open: bool,
    /// The chip keeps its popover's state itself from now on.
    controlled: bool,
}

thread_local! {
    /// The chip a row marker asked to open, until that chip draws.
    static REQUEST: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Asks the chip with this id to open its popover when it next draws: what a
/// marker on a mod's row does. The caller redraws the window.
pub fn request_open(chip_id: &str) {
    REQUEST.with(|r| *r.borrow_mut() = Some(chip_id.to_string()));
}

fn take_request(chip_id: &str) -> bool {
    REQUEST.with(|r| {
        let mut r = r.borrow_mut();
        let mine = r.as_deref() == Some(chip_id);
        if mine {
            *r = None;
        }
        mine
    })
}

thread_local! {
    /// Every `chip id|key` that already opened its popover. It outlives the
    /// chip's own state, which is dropped when the player visits a page
    /// without the chip, so coming back doesn't open it a second time.
    static SEEN: std::cell::RefCell<HashSet<String>> = std::cell::RefCell::new(HashSet::new());
}

thread_local! {
    /// The chip whose popover opened by itself and is still open, with when it
    /// last drew. Two chips that both have something new at once (a stuck
    /// start and a missing mod) must not open two popovers over each other:
    /// the first drawn (the run state's) opens, the other stays a red chip.
    static OPENED: std::cell::RefCell<Option<(String, std::time::Instant)>> = const { std::cell::RefCell::new(None) };
}

/// Another chip's popover opened by itself is still up.
fn another_is_open(me: &str) -> bool {
    OPENED.with(|o| o.borrow().as_ref().is_some_and(|(id, at)| id != me && at.elapsed() < std::time::Duration::from_secs(2)))
}

/// `me` holds the one automatic popover while it is open; frees it when closed.
fn hold(me: &str, open: bool) {
    OPENED.with(|o| {
        let mut o = o.borrow_mut();
        if open {
            *o = Some((me.to_string(), std::time::Instant::now()));
        } else if o.as_ref().is_some_and(|(id, _)| id == me) {
            *o = None;
        }
    });
}

/// Marks the keys as shown and says whether any of them was new.
fn first_sight(chip: &str, keys: &[SharedString]) -> bool {
    SEEN.with(|s| {
        let mut s = s.borrow_mut();
        let mut fresh = false;
        for k in keys {
            fresh |= s.insert(format!("{chip}|{k}"));
        }
        fresh
    })
}

/// The chip and its popover. A chip with `auto_open` keys keeps the popover's
/// open state itself, so it can open once by itself.
#[derive(IntoElement)]
struct ChipEl {
    chip: Chip,
    form: Form,
}

impl RenderOnce for ChipEl {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { chip, form } = self;
        let sections = chip.sections.clone();
        let id = SharedString::from(format!("sync-chip:{}", chip.id));
        let width = chip.width;
        let pop = Popover::new(SharedString::from(format!("sync-chip-pop:{}", chip.id)))
            .anchor(Anchor::TopLeft)
            .appearance(true)
            .trigger(Trigger {
                id: ElementId::Name(id),
                look: chip.look.clone(),
                form,
                progress: chip.progress,
                selected: false,
                open: false,
            })
            .content(move |_, window, cx| body(&sections, width, window, cx));
        let auto = window.use_keyed_state(ElementId::Name(format!("sync-chip-auto:{}", chip.id).into()), cx, |_, _| Auto::default());
        if take_request(&chip.id) {
            auto.update(cx, |a, _| {
                a.open = true;
                a.controlled = true;
            });
        }
        if chip.auto_open.is_empty() && !auto.read(cx).controlled {
            return pop.into_any_element();
        }
        if !chip.auto_open.is_empty() && first_sight(&chip.id, &chip.auto_open) && !another_is_open(&chip.id) {
            auto.update(cx, |a, _| a.open = true);
            // Opening focuses the popover; give the keyboard back to what
            // had it (a search box the player is typing in) once drawn.
            if let Some(prev) = window.focused(cx) {
                window.defer(cx, move |window, cx| prev.focus(window, cx));
            }
        }
        let open = auto.read(cx).open;
        hold(&chip.id, open);
        let (a2, me) = (auto.clone(), chip.id.clone());
        pop.open(open)
            .on_open_change(move |o, _, cx| {
                hold(&me, *o);
                a2.update(cx, |a, _| a.open = *o)
            })
            .into_any_element()
    }
}

/// The popover's content: one block per side, each with its line, its
/// optional bar and its buttons (one filled at most).
fn body(sections: &[Section], width: Option<f32>, window: &mut Window, cx: &mut App) -> AnyElement {
    let theme = cx.theme().clone();
    // A section with a list of its own needs more room for it.
    let width = width.unwrap_or(if sections.iter().any(|s| s.body.is_some()) { 24.0 } else { 19.5 });
    v_flex()
        .w(rems(width))
        .max_w(rems(width))
        .gap_3()
        .children(sections.iter().enumerate().map(|(i, s)| {
            v_flex()
                .gap_1p5()
                .when(i > 0, |c| c.pt_3().border_t_1().border_color(theme.border))
                .when(!s.title.is_empty(), |c| c.child(div().text_sm().font_semibold().text_color(theme.foreground).child(s.title.clone())))
                .when(!s.line.is_empty(), |c| c.child(div().text_sm().text_color(theme.muted_foreground).child(s.line.clone())))
                .when_some(s.progress, |c, p| {
                    c.child(
                        div().h(px(4.)).w_full().rounded_full().bg(theme.muted).child(
                            div().h_full().w(relative(p.clamp(0.0, 1.0))).rounded_full().bg(theme.info),
                        ),
                    )
                })
                .when_some(s.body.clone(), |c, b| c.child(b(window, cx)))
                .when(!s.actions.is_empty(), |c| {
                    c.child(h_flex().flex_wrap().gap_2().pt_1().children(s.actions.iter().enumerate().map(|(j, a)| {
                        let run = a.run.clone();
                        let b = Button::new(("sync-chip-action", i * 8 + j)).small().label(a.label.clone()).disabled(a.disabled);
                        let b = if a.primary { b.primary() } else { b.outline() };
                        b.when_some(a.icon, |b, ic| b.icon(ic)).on_click(move |_, w, cx| run(w, cx))
                    })))
                })
        }))
        .into_any_element()
}

/// "+N" for what folded into it: the chips' labels first, tooltip only.
pub fn more_pill(hidden: usize, names: String, cx: &App) -> AnyElement {
    widgets::pill(format!("+{hidden}"), cx.theme().muted_foreground, false)
        .id("hero-pills-more")
        .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(names.clone()).build(w, cx))
        .into_any_element()
}

/// The states `--dialog sync-chip --state NAME` shows (and `cycle` walks):
/// g-*: the group's chip, s-*: the server's, both-*: the two together.
pub const PREVIEWS: &[&str] = &[
    "g-in-sync", "g-checking", "g-updating", "g-sending", "g-behind", "g-offline", "g-failed", "g-needs", "s-in-sync", "s-differ", "s-comparing",
    "s-pushing", "s-offline", "s-blocked", "s-failed", "s-wont", "dl-waiting", "dl-working", "dl-stuck", "rs-changed", "rs-waiting", "both-ok", "both-busy", "both-needs", "both-long",
    "sy-needs", "sy-needs-one", "sy-needs-big", "sy-differs", "sy-differs-big", "sy-ood", "sy-behind", "sy-getting", "sy-game", "sy-failed", "sy-sending", "sy-held",
    "sy-offline", "sy-insync", "sy-notes",
];

/// Made-up chips for a preview state, no network, no real group or server.
pub fn preview(name: &str) -> Vec<Chip> {
    if let Some(chip) = crate::sync_preview::chip(name) {
        return vec![chip];
    }
    use hopper::groupstatus::Status;
    use hopper::syncchip::{DownloadState, ServerState, Work, download_look, status_look, server_look};
    let noop: Run = Rc::new(|_, _| {});
    let make = |id: &str, look: Look, title: &str, line: &str, progress: Option<f32>, primary: &str| Chip {
        id: id.to_string().into(),
        progress,
        auto_open: Vec::new(),
        width: None,
        sections: vec![Section {
            title: title.to_string().into(),
            line: line.to_string().into(),
            progress,
            body: None,
            actions: vec![Action { label: primary.to_string().into(), icon: Some(IconName::RefreshCw), primary: true, disabled: false, run: noop.clone() }],
        }],
        look,
    };
    let mia = || vec!["Mia".to_string()];
    let group = |status: Status, title: &str, line: &str, progress: Option<f32>| make("group", status_look(&status, false, false, false), title, line, progress, "Pull now");
    let g = |n: &str| -> Chip {
        match n {
            "in-sync" => group(Status::InStep { checked: None }, "Your copy matches the group.", "Nothing new to take in.", None),
            "checking" => group(Status::Checking, "Checking the group", "Looking at what Mia and Noah changed.", None),
            "updating" => group(Status::Updating { changes: 2, from: mia() }, "Taking in changes", "2 changes from Mia are coming in.", Some(0.62)),
            "sending" => group(Status::Sending, "Sending your changes", "The group gets them in a moment.", None),
            "behind" => group(Status::Behind { mods: 2, missing: false }, "Behind the group", "2 mods differ from the group's.", None),
            "offline" => group(Status::Offline { checked: None }, "Offline", "Can't reach the group. What this PC knows may be old.", None),
            "failed" => make(
                "group",
                status_look(&Status::Checking, false, true, false),
                "Couldn't take the changes in",
                "A mod didn't download. Try again.",
                None,
                "Try again",
            ),
            _ => group(Status::ByHand { mods: vec!["Create".into(), "Jade".into()] }, "2 mods to download", "Their authors only allow downloads from the CurseForge website.", None),
        }
    };
    let server = |state: ServerState, other: &str| {
        let look = server_look(&state, other);
        let (title, line) = match &state {
            ServerState::InSync => (format!("Same mods as {other}"), "Nothing to send or take."),
            ServerState::Differ { blocked: true, .. } => (format!("Mods differ from {other}"), "It is running. Stop it first, or allow syncing while it runs in Settings."),
            ServerState::Differ { .. } => (format!("3 mods differ from {other}"), "Sync makes the server's mods match this instance."),
            ServerState::Comparing => (format!("Comparing with {other}"), "Looking at both mod lists."),
            ServerState::Working(_) => (format!("Sending to {other}"), "It takes a moment. Nothing else to do."),
            ServerState::Offline => (format!("{other} is offline"), "Can't reach it. Hopper tries again by itself."),
            ServerState::WontJoin => (format!("{other} turns this pack away"), "NeoForge: 21.1.172 in this pack, 21.1.200 on the server"),
            ServerState::Failed => ("Couldn't compare".to_string(), "The server didn't answer the way Hopper expected."),
        };
        let progress = matches!(state, ServerState::Working(_)).then_some(0.4);
        make("server", look, &title, line, progress, "Sync to server")
    };
    const NAME: &str = "Skyblock Server";
    const LONG: &str = "All the Mods 10 - To the Sky Survival";
    // The Downloads chip: files waiting, the window working, files that didn't come.
    let downloads = |state: DownloadState| {
        let look = download_look(&state);
        let (title, line, progress) = match &state {
            DownloadState::Waiting { files } => (format!("{files} files to download in your browser"), "Their authors only allow downloads from the CurseForge website.", None),
            DownloadState::Working { done, total } => (
                format!("Downloading from CurseForge… {} of {total}", done + 1),
                "Downloading Create · 62%",
                Some(0.4),
            ),
            DownloadState::Stuck => ("Some files didn't come".to_string(), "Create didn't download. Retry opens the window again.", None),
        };
        let label = match state {
            DownloadState::Working { .. } => "Show window",
            DownloadState::Stuck => "Retry",
            DownloadState::Waiting { .. } => "Download 3",
        };
        let mut chip = make("downloads", look, &title, line, progress, label);
        for a in &mut chip.sections[0].actions {
            a.icon = None;
        }
        chip
    };
    // The Restart chip: what changed while the game runs.
    let restart = |changed: bool| {
        let look = hopper::syncchip::restart_look(changed, "game");
        let (title, line) = if changed {
            ("Changed while the game runs", "Restart it to load the changes.")
        } else {
            ("Waiting for the game to close", "2 old files the game still has open will be removed when it closes.")
        };
        let mut chip = make("restart", look, title, line, None, "Hide");
        for a in &mut chip.sections[0].actions {
            a.icon = None;
            a.primary = false;
        }
        chip
    };
    // The run state's problem: a start that looks stuck, a crash. Opens by
    // itself the first time, like the real one.
    let run = |problem: hopper::syncchip::RunProblem, title: &str, line: &str| {
        let mut chip = make("run", hopper::syncchip::run_look(&problem), title, line, None, "Stop and start again…");
        chip.auto_open = vec!["preview".into()];
        chip.sections[0].actions.push(Action { label: "See why →".into(), icon: None, primary: false, disabled: false, run: noop.clone() });
        for a in &mut chip.sections[0].actions {
            a.icon = None;
        }
        chip
    };
    let stuck = || run(hopper::syncchip::RunProblem::Stuck { minutes: Some(4) }, "Skyblock Server looks stuck while loading mods", "Nothing has happened for 4 minutes.");
    match name.split_once('-') {
        Some(("run", "stuck")) => vec![stuck()],
        Some(("run", "crashed")) => vec![run(hopper::syncchip::RunProblem::Crashed, "The server crashed", "Create stopped it while it loaded.")],
        Some(("run", "all")) => vec![stuck(), g("in-sync"), server(ServerState::Differ { mods: 3, blocked: false }, NAME), restart(true)],
        Some(("rs", "changed")) => vec![restart(true)],
        Some(("rs", _)) => vec![restart(false)],
        Some(("dl", "working")) => vec![downloads(DownloadState::Working { done: 1, total: 3 })],
        Some(("dl", "stuck")) => vec![downloads(DownloadState::Stuck)],
        Some(("dl", _)) => vec![downloads(DownloadState::Waiting { files: 3 })],
        Some(("g", n)) => vec![g(n)],
        Some(("s", n)) => vec![server(
            match n {
                "differ" => ServerState::Differ { mods: 3, blocked: false },
                "comparing" => ServerState::Comparing,
                "pushing" => ServerState::Working(Work::Pushing),
                "offline" => ServerState::Offline,
                "blocked" => ServerState::Differ { mods: 3, blocked: true },
                "failed" => ServerState::Failed,
                "wont" => ServerState::WontJoin,
                _ => ServerState::InSync,
            },
            NAME,
        )],
        Some(("both", "ok")) => vec![g("in-sync"), server(ServerState::InSync, NAME)],
        Some(("both", "busy")) => vec![g("updating"), server(ServerState::Differ { mods: 3, blocked: false }, NAME)],
        Some(("both", "needs")) => vec![g("needs"), server(ServerState::Differ { mods: 3, blocked: false }, NAME)],
        Some(("both", _)) => {
            let mut group = g("updating");
            group.look = status_look(
                &Status::Updating { changes: 2, from: vec!["Additional Placements Extra Long Name".into()] },
                false,
                false,
                false,
            );
            vec![group, server(ServerState::Differ { mods: 3, blocked: false }, LONG)]
        }
        _ => Vec::new(),
    }
}
