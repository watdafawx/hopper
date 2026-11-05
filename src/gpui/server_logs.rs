//! A server's Logs & crashes tab (Servers page): its crash reports, each
//! read the way the crash card reads the last one, and latest.log and
//! debug.log followed while the server runs, each with search, an
//! errors-only filter and sharing on mclo.gs. The same for a server in a
//! folder on this PC and one on a Hopper Server (read through its API).
//! The card carries the fixes (put the older mod back, turn mods off).

use crate::crash_help::callout;
use crate::line_select::{LineSelect, Lines};
use crate::log_files;
use crate::store::{Store, StoreEvent, open_path};
use crate::selection::{self, KeyAct, Selection};
use crate::widgets;
use crate::widgets::Clickable as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::crashdetect::Crash;
use hopper::instances::Addon;
use hopper::logs::{Level, is_problem, level};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How much of a log to show when it opens (as Hopper Server sends).
const OPEN_BYTES: u64 = 400 << 10;
/// More growth than this between two reads reloads the tail instead.
const APPEND_MAX: u64 = 4 << 20;
/// Lines kept while following; older ones drop off the top.
const KEEP_LINES: usize = 200_000;
/// New verdicts worked out per pass on this PC (each reads a report).
const VERDICTS_PER_PASS: usize = 3;

/// How many of the newest reports get a verdict in the list (as Hopper
/// Server does): reading a report means reading the mods too.
const WITH_VERDICT: usize = 25;

/// The tab's own tabs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Crashes,
    Latest,
    Debug,
    /// The older logs in logs/ (rotated files), to delete.
    Older,
}

impl Pane {
    /// The log it shows, relative to the server folder.
    fn log(self) -> Option<&'static str> {
        match self {
            Pane::Crashes | Pane::Older => None,
            Pane::Latest => Some("logs/latest.log"),
            Pane::Debug => Some("logs/debug.log"),
        }
    }
}

/// A crash report in the list.
#[derive(Clone)]
struct Report {
    /// "crash-reports/crash-….txt"
    rel: String,
    /// Written, in seconds since the epoch.
    modified: i64,
    /// Hopper's one line on it (the newest few only).
    verdict: Option<String>,
    /// Its text says it only followed from the start's real crash.
    follow_up: bool,
}

/// A report as Hopper read it, and a name for each jar it names.
#[derive(Clone)]
struct Card {
    crash: Option<Crash>,
    names: HashMap<String, String>,
}

/// A log on screen.
struct LogFile {
    rel: &'static str,
    lines: Vec<String>,
    /// Indices into `lines` of errors, warnings and stack frames.
    problems: Vec<usize>,
    /// Still reading it the first time.
    loading: bool,
    /// Why it couldn't be read (before any line came).
    error: Option<String>,
}

impl LogFile {
    fn push(&mut self, text: &str) {
        for l in text.lines() {
            if is_problem(l) {
                self.problems.push(self.lines.len());
            }
            self.lines.push(l.to_string());
        }
        if self.lines.len() > KEEP_LINES {
            let drop = self.lines.len() - KEEP_LINES * 9 / 10;
            self.lines.drain(..drop);
            self.problems.retain(|i| *i >= drop);
            self.problems.iter_mut().for_each(|i| *i -= drop);
        }
    }

    fn clear(&mut self) {
        self.lines.clear();
        self.problems.clear();
    }
}

pub struct ServerLogsView {
    store: Entity<Store>,
    root: Option<PathBuf>,
    pane: Pane,
    /// Shown now: polls run only then.
    active: bool,
    /// Crash reports, newest first; `None` while the first list loads.
    reports: Option<Result<Vec<Report>, String>>,
    /// Verdicts on this PC's reports already worked out, by report and
    /// time (a report doesn't change once written).
    verdicts: HashMap<(String, i64), String>,
    /// The report picked in the list.
    picked: Option<String>,
    /// The reports picked with Ctrl / Shift (the shown one alone else).
    sel: Selection,
    /// The older logs picked.
    old_sel: Selection,
    /// The lists, for their keys (Ctrl+A, arrows, Delete).
    list_focus: FocusHandle,
    /// Reports read so far; `None` while reading.
    cards: HashMap<String, Option<Result<Card, String>>>,
    /// The card's mods (jars) whose "Why?" list is open.
    why_open: HashSet<String>,
    /// Reports whose whole list of mods shows (else the first two).
    more_open: HashSet<String>,
    /// Reports whose folded follow-up reports are shown (by the report
    /// that explains them).
    folds_open: HashSet<String>,
    /// The older logs; `None` until the pane first opens.
    old: Option<Result<Vec<log_files::OldLog>, String>>,
    /// Reports whose older mod was put back from the card.
    fixed: HashSet<String>,
    /// The jars ticked in the open "Turn off these mods?" list.
    turn_off_pick: Option<HashSet<String>>,
    /// A fix is running on the server.
    fix_busy: bool,
    log: Option<LogFile>,
    errors_only: bool,
    query: Entity<InputState>,
    /// Lines shown when filtered (errors only or a search); `None` shows all.
    visible: Option<Vec<usize>>,
    lines: LineSelect,
    /// mclo.gs links by file; `None` while uploading.
    shared: HashMap<String, Option<Result<String, String>>>,
    _follow: Option<Task<()>>,
    _list: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ServerLogsView {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search this log"));
        let subs = vec![cx.subscribe(&query, |this, _, e: &InputEvent, cx| {
            if matches!(e, InputEvent::Change) {
                this.scroll_to_end();
                cx.notify();
            }
        })];
        Self {
            store,
            root: None,
            pane: Pane::Crashes,
            active: false,
            reports: None,
            verdicts: HashMap::new(),
            picked: None,
            sel: Selection::new(),
            old_sel: Selection::new(),
            list_focus: cx.focus_handle(),
            cards: HashMap::new(),
            why_open: HashSet::new(),
            more_open: HashSet::new(),
            folds_open: HashSet::new(),
            old: None,
            fixed: HashSet::new(),
            turn_off_pick: None,
            fix_busy: false,
            log: None,
            errors_only: false,
            query,
            visible: None,
            lines: LineSelect::new(cx),
            shared: HashMap::new(),
            _follow: None,
            _list: None,
            _subscriptions: subs,
        }
    }

    /// The tab is on screen for `root`: starts reading (again) when the
    /// server changed or the tab was away. Called on every draw, so it
    /// does nothing otherwise.
    pub fn show(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let new_root = self.root.as_ref() != Some(&root);
        if !new_root && self.active {
            return;
        }
        if new_root {
            self.root = Some(root);
            self.reports = None;
            self.picked = None;
            self.sel.clear();
            self.old_sel.clear();
            self.cards.clear();
            self.why_open.clear();
            self.more_open.clear();
            self.folds_open.clear();
            self.old = None;
            self.fixed.clear();
            self.turn_off_pick = None;
            self.shared.clear();
            self.verdicts.clear();
        }
        self.active = true;
        self.watch_reports(cx);
        let pane = self.pane;
        self.log = None;
        self.select(pane, cx);
    }

    /// Opens the crash reports on `rel` (the newest when none): where the
    /// crash notice's "See why" lands.
    pub fn open_report(&mut self, rel: Option<String>, cx: &mut Context<Self>) {
        self.select(Pane::Crashes, cx);
        if let Some(rel) = rel {
            self.pick(rel, cx);
        }
    }

    /// Runs a fix on the server off the UI thread and says how it went.
    /// `rel` is the report it was run from.
    fn run_fix(
        &mut self,
        rel: String,
        mark_fixed: bool,
        f: impl FnOnce(&Path) -> anyhow::Result<String> + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.root.clone() else { return };
        self.fix_busy = true;
        cx.notify();
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let r = root.clone();
            let done = cx.background_spawn(async move { f(&r) }).await;
            this.update(cx, |this, cx| {
                use gpui_kit::component::WindowExt as _;
                use gpui_kit::component::notification::Notification;
                this.fix_busy = false;
                let note = match &done {
                    Ok(m) => Notification::success(m.clone()),
                    Err(e) => Notification::warning(format!("{e:#}")),
                };
                if done.is_ok() && mark_fixed {
                    this.fixed.insert(rel);
                }
                let _ = handle.update(cx, |_, window, cx| window.push_notification(note, cx));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The tab went out of view: stop polling the server.
    pub fn hidden(&mut self, _cx: &mut Context<Self>) {
        self.active = false;
        self._follow = None;
        self._list = None;
    }

    /// Lists the crash reports now and every few seconds while shown, off
    /// the UI thread.
    fn watch_reports(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        self._list = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(known) = this.update(cx, |this, _| this.verdicts.clone()) else { return };
                let r = root.clone();
                let got = cx.background_spawn(async move { list_reports(&r, known) }).await;
                let alive = this.update(cx, |this, cx| {
                    if this.root.as_ref() != Some(&root) {
                        return false;
                    }
                    let list = match got {
                        Ok((list, verdicts)) => {
                            this.verdicts = verdicts;
                            Ok(list)
                        }
                        Err(e) => Err(e),
                    };
                    // A failed poll keeps the list already shown.
                    if list.is_err() && matches!(this.reports, Some(Ok(_))) {
                        return true;
                    }
                    // The newest one, unless one still in the list was picked.
                    let pick = match &list {
                        Ok(l) if !this.picked.as_ref().is_some_and(|p| l.iter().any(|r| &r.rel == p)) => l.first().map(|r| r.rel.clone()),
                        _ => None,
                    };
                    this.reports = Some(list);
                    if let Some(p) = pick {
                        this.pick(p, cx);
                    }
                    cx.notify();
                    true
                });
                if !matches!(alive, Ok(true)) {
                    return;
                }
                // Verdicts still to work out: the next pass right away.
                let more = this
                    .update(cx, |this, _| {
                        matches!(&this.reports, Some(Ok(l)) if l.iter().take(WITH_VERDICT).any(|r| !this.verdicts.contains_key(&(r.rel.clone(), r.modified))))
                    })
                    .unwrap_or(false);
                let wait = if more && !hopper::hopperd::is_remote_root(&root) { 0 } else { 10 };
                cx.background_executor().timer(Duration::from_secs(wait)).await;
            }
        }));
    }

    /// "Delete" on crash reports `rels` (one, or a report with its
    /// follow-ups); `picked` when they are the reports
    /// picked in the list (with their follow-ups), not one report's group.
    fn ask_delete_as(&mut self, rels: Vec<String>, picked: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let n = rels.len();
        let (title, what) = if n == 1 {
            ("Delete this crash report?".to_string(), "It".to_string())
        } else if picked {
            (format!("Delete these {n} crash reports?"), "They (with the follow-up reports of the ones picked)".to_string())
        } else {
            (format!("Delete these {n} crash reports?"), "They are the report and its follow-up reports; they".to_string())
        };
        let body = format!("{what} go to {}, where you can get {} back.", log_files::trash_name(&root), if n == 1 { "it" } else { "them" });
        let view = cx.entity();
        log_files::confirm(window, cx, title, body, move |window, cx| {
            let r = log_files::delete(&root, &rels, false);
            let ok = r.is_ok();
            log_files::report(window, cx, r);
            if ok {
                view.update(cx, |this, cx| this.after_delete(&rels, cx));
            }
        });
    }

    /// "Clear all" for crash reports, or the older logs.
    fn ask_clear(&mut self, kind: hopper::crashfiles::Kind, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let crashes = kind == hopper::crashfiles::Kind::Crashes;
        let n = if crashes {
            match &self.reports {
                Some(Ok(l)) => l.len(),
                _ => 0,
            }
        } else {
            match &self.old {
                Some(Ok(l)) => l.len(),
                _ => 0,
            }
        };
        if n == 0 {
            return;
        }
        let (title, what) = if crashes {
            (format!("Delete all {n} crash reports?"), "They")
        } else {
            (format!("Delete {n} older logs?"), "They (not latest.log or debug.log)")
        };
        let body = format!("{what} go to {}, where you can get them back.", log_files::trash_name(&root));
        let view = cx.entity();
        log_files::confirm(window, cx, title, body, move |window, cx| {
            let r = log_files::clear(&root, kind);
            let ok = r.is_ok();
            log_files::report(window, cx, r);
            if ok {
                view.update(cx, |this, cx| this.after_delete(&[], cx));
            }
        });
    }

    /// Files went: forget them and read the folders again.
    fn after_delete(&mut self, rels: &[String], cx: &mut Context<Self>) {
        for r in rels {
            self.cards.remove(r);
            self.shared.remove(r);
        }
        if let Some(Ok(l)) = &mut self.reports {
            l.retain(|r| !rels.contains(&r.rel));
        }
        if self.picked.as_ref().is_some_and(|p| rels.contains(p)) {
            self.picked = None;
        }
        for r in rels {
            self.sel.keys_mut().remove(r);
            self.old_sel.keys_mut().remove(r);
        }
        self.old = None;
        if self.pane == Pane::Older {
            self.load_old(cx);
        }
        // Reads the list again now; it also picks the newest report when
        // the one on screen is gone.
        self.watch_reports(cx);
        cx.notify();
    }

    /// Reads the older logs off the UI thread.
    fn load_old(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        cx.spawn(async move |this, cx| {
            let r = root.clone();
            let got = cx.background_spawn(async move { log_files::old_logs(&r) }).await;
            this.update(cx, |this, cx| {
                if this.root.as_ref() == Some(&root) {
                    this.old = Some(got);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The older logs, each with its time and size and a delete.
    fn older_body(&mut self, _root: &Path, cx: &mut Context<Self>) -> AnyElement {
        let list = match &self.old {
            None => return widgets::skeleton_rows(4, rems(2.), cx).into_any_element(),
            Some(Err(e)) => {
                return widgets::empty_state(IconName::TriangleAlert, "Can't read the logs", Some(e.clone().into()), cx).into_any_element();
            }
            Some(Ok(l)) if l.is_empty() => {
                return widgets::empty_state(IconName::CircleCheck, "No older logs", Some("The server keeps its last start in latest.log. Older ones show up here.".into()), cx)
                    .into_any_element();
            }
            Some(Ok(l)) => l,
        };
        let radius = cx.theme().radius;
        let order: Vec<String> = list.iter().map(|f| f.rel.clone()).collect();
        self.old_sel.retain(&order);
        let rows: Vec<AnyElement> = list
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let rel = f.rel.clone();
                let click_rel = f.rel.clone();
                let sel = self.old_sel.contains(&f.rel);
                let order = order.clone();
                h_flex()
                    .id(("srv-old-log", i))
                    .gap_3()
                    .px_2()
                    .py_1p5()
                    .rounded(radius)
                    .clickable(cx)
                    .map(|d| if sel { widgets::row_selected(d, cx) } else { d.hover(|s| widgets::row_hover(s, cx)) })
                    .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                        this.old_sel.click(&click_rel, e.modifiers(), &order);
                        window.focus(&this.list_focus, cx);
                        cx.notify();
                    }))
                    .child(div().flex_1().min_w_0().truncate().text_sm().child(file_name(&f.rel).to_string()))
                    .child(
                        widgets::caption(log_files::when(f.modified), cx)
                            .id(("srv-old-log-when", i))
                            .tooltip({
                                let full = hopper::time::exact_full(f.modified);
                                move |w, cx| gpui_kit::component::tooltip::Tooltip::new(full.clone()).build(w, cx)
                            })
                            .text_xs(),
                    )
                    .child(widgets::caption(log_files::size(f.size), cx).text_xs().w(rems(5.)))
                    .child(
                        Button::new(("srv-old-log-del", i))
                            .xsmall()
                            .ghost()
                            .icon(IconName::Trash)
                            .tooltip("Delete this log")
                            .on_click(cx.listener(move |this, _, window, cx| this.ask_delete_log(rel.clone(), window, cx))),
                    )
                    .into_any_element()
            })
            .collect();
        v_flex()
            .flex_1()
            .min_h_0()
            .gap_2()
            .child(
                h_flex().child(div().flex_1()).child(
                    Button::new("srv-old-clear")
                        .small()
                        .outline()
                        .icon(IconName::Trash)
                        .label("Clear old logs")
                        .tooltip("Deletes every older log; latest.log and debug.log stay")
                        .on_click(cx.listener(|this, _, window, cx| this.ask_clear(hopper::crashfiles::Kind::Logs, window, cx))),
                ),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .track_focus(&self.list_focus)
                    .on_key_down(cx.listener(move |this, e: &KeyDownEvent, window, cx| {
                        match selection::on_key(&mut this.old_sel, e, &order, cx) {
                            KeyAct::None => return,
                            KeyAct::Delete => this.ask_delete_logs(this.old_sel.in_order(&order), window, cx),
                            KeyAct::Changed | KeyAct::Moved(_) => {}
                        }
                        cx.notify();
                    }))
                    .child(div().id("srv-old-logs").size_full().child(v_flex().gap_0p5().children(rows)).overflow_y_scrollbar())
                    .children(self.old_bar(cx)),
            )
            .into_any_element()
    }

    /// The bulk bar of the older logs, with more than one picked.
    fn old_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.old_sel.many() {
            return None;
        }
        let order: Vec<String> = match &self.old {
            Some(Ok(l)) => l.iter().map(|f| f.rel.clone()).collect(),
            _ => return None,
        };
        let del = cx.listener(move |this, _: &ClickEvent, window, cx| this.ask_delete_logs(this.old_sel.in_order(&order), window, cx));
        let bar = selection::bar(
            "srv-old-bulk",
            self.old_sel.len(),
            Vec::new(),
            Some(("Delete".into(), Box::new(del))),
            cx.listener(|this, _: &ClickEvent, _, cx| {
                this.old_sel.clear();
                cx.notify();
            }),
            cx,
        );
        Some(selection::floating("srv-old-bulk", bar))
    }

    /// "Delete" on one older log.
    fn ask_delete_log(&mut self, rel: String, window: &mut Window, cx: &mut Context<Self>) {
        self.ask_delete_logs(vec![rel], window, cx)
    }

    /// "Delete" on older logs `rels` (the one pressed, or those picked).
    fn ask_delete_logs(&mut self, rels: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let (title, body) = match rels.as_slice() {
            [one] => ("Delete this log?".to_string(), format!("{} goes to {}, where you can get it back.", file_name(one), log_files::trash_name(&root))),
            _ => (format!("Delete these {} logs?", rels.len()), format!("They go to {}, where you can get them back.", log_files::trash_name(&root))),
        };
        let view = cx.entity();
        log_files::confirm(window, cx, title, body, move |window, cx| {
            let r = log_files::delete(&root, &rels, false);
            let ok = r.is_ok();
            log_files::report(window, cx, r);
            if ok {
                view.update(cx, |this, cx| this.after_delete(&rels, cx));
            }
        });
    }

    /// Shows report `rel`, reading it off the UI thread the first time.
    fn pick(&mut self, rel: String, cx: &mut Context<Self>) {
        if !self.sel.contains(&rel) {
            self.sel.set_one(rel.clone());
        }
        self.picked = Some(rel.clone());
        cx.notify();
        if self.cards.get(&rel).is_some_and(|c| !matches!(c, Some(Err(_)))) {
            return;
        }
        let Some(root) = self.root.clone() else { return };
        self.cards.insert(rel.clone(), None);
        cx.spawn(async move |this, cx| {
            let (r, p) = (root.clone(), rel.clone());
            let card = cx.background_spawn(async move { read_card(&r, &p) }).await;
            this.update(cx, |this, cx| {
                if this.root.as_ref() == Some(&root) {
                    this.cards.insert(rel, Some(card));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Shows pane `pane`, opening its log.
    fn select(&mut self, pane: Pane, cx: &mut Context<Self>) {
        self.pane = pane;
        if pane == Pane::Older {
            self.load_old(cx);
        }
        match pane.log() {
            Some(rel) if self.log.as_ref().map(|l| l.rel) != Some(rel) => self.open_log(rel, cx),
            Some(_) => {}
            // Off a log: stop following it.
            None => {
                self.log = None;
                self._follow = None;
                self.lines.clear();
            }
        }
        cx.notify();
    }

    /// Shows log `rel` and follows it: what the server appends is read
    /// every second (two on a Hopper Server), and the view stays at the
    /// bottom while it is there (scrolled up, it stays put).
    fn open_log(&mut self, rel: &'static str, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        self.lines.clear();
        self.log = Some(LogFile { rel, lines: Vec::new(), problems: Vec::new(), loading: true, error: None });
        let every = Duration::from_secs(if hopper::hopperd::is_remote_root(&root) { 2 } else { 1 });
        self._follow = Some(cx.spawn(async move |this, cx| {
            let mut from = 0u64;
            loop {
                let r = root.clone();
                let got = cx.background_spawn(async move { read_log(&r, rel, from) }).await;
                let next = this.update(cx, |this, cx| {
                    // Follow only a reader who is at the bottom already.
                    let at_end = this.lines.scroll.is_scrolled_to_end().unwrap_or(true);
                    let Some(log) = this.log.as_mut().filter(|l| l.rel == rel) else {
                        return None;
                    };
                    let first = std::mem::replace(&mut log.loading, false);
                    let (next, changed) = match got {
                        Ok((text, to, reset)) => {
                            let changed = !text.is_empty() || (reset && !log.lines.is_empty());
                            if reset {
                                log.clear();
                            }
                            log.push(&text);
                            log.error = None;
                            (to, changed)
                        }
                        // Not there yet (a server that never started): try
                        // again from the start; a later error keeps the lines.
                        Err(e) => {
                            let changed = log.lines.is_empty() && log.error.as_ref() != Some(&e);
                            if log.lines.is_empty() {
                                log.error = Some(e);
                            }
                            (if log.lines.is_empty() { 0 } else { from }, changed)
                        }
                    };
                    if changed || first {
                        if first || at_end {
                            this.scroll_to_end();
                        }
                        cx.notify();
                    }
                    Some(next)
                });
                let Ok(Some(n)) = next else { return };
                from = n;
                cx.background_executor().timer(every).await;
            }
        }));
        cx.notify();
    }

    fn scroll_to_end(&self) {
        self.lines.scroll_to_end(self.line_count());
    }

    /// How many lines the viewer shows (all, or the filtered ones).
    fn line_count(&self) -> usize {
        match (&self.log, &self.visible) {
            (None, _) => 0,
            (Some(_), Some(v)) => v.len(),
            (Some(l), None) => l.lines.len(),
        }
    }

    fn line(&self, ix: usize) -> Option<&str> {
        let l = self.log.as_ref()?;
        let i = match &self.visible {
            Some(v) => *v.get(ix)?,
            None => ix,
        };
        l.lines.get(i).map(String::as_str)
    }

    /// Which lines show: errors only and the search, both optional.
    fn filter(&self, cx: &App) -> Option<Vec<usize>> {
        let log = self.log.as_ref()?;
        let q = self.query.read(cx).value().trim().to_lowercase();
        if q.is_empty() && !self.errors_only {
            return None;
        }
        let base: Box<dyn Iterator<Item = usize>> =
            if self.errors_only { Box::new(log.problems.iter().copied()) } else { Box::new(0..log.lines.len()) };
        Some(base.filter(|i| q.is_empty() || log.lines[*i].to_lowercase().contains(&q)).collect())
    }

    fn share(&mut self, rel: String, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        self.shared.insert(rel.clone(), None);
        cx.spawn(async move |this, cx| {
            let (r, p) = (root.clone(), rel.clone());
            let got = cx.background_spawn(async move { share_log(&r, &p) }).await.map_err(|e| format!("{e:#}"));
            this.update(cx, |this, cx| {
                if this.root.as_ref() == Some(&root) {
                    this.shared.insert(rel, Some(got));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// "Share on mclo.gs" for file `rel`, or its link once shared.
    fn share_row(&self, rel: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        match self.shared.get(rel) {
            Some(None) => h_flex()
                .gap_2()
                .child(Spinner::new().small())
                .child(widgets::caption("Uploading to mclo.gs", cx))
                .into_any_element(),
            Some(Some(Ok(url))) => {
                let (u1, u2) = (url.clone(), url.clone());
                h_flex()
                    .gap_2()
                    .child(widgets::caption("Shared:", cx))
                    .child(Button::new("srv-shared-link").link().small().label(url.clone()).on_click(move |_, _, cx| cx.open_url(&u1)))
                    .child(
                        Button::new("srv-shared-copy")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Copy)
                            .tooltip("Copy link")
                            .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(u2.clone()))),
                    )
                    .into_any_element()
            }
            Some(Some(Err(e))) => div().text_sm().text_color(theme.danger).child(e.clone()).into_any_element(),
            None => {
                let p = rel.to_string();
                Button::new("srv-share-log")
                    .outline()
                    .small()
                    .icon(IconName::Share2)
                    .label("Share on mclo.gs")
                    .tooltip("Upload this file and get a link for Discord or a bug report. It becomes public. IPs, tokens and your user name are hidden first.")
                    .on_click(cx.listener(move |this, _, _, cx| this.share(p.clone(), cx)))
                    .into_any_element()
            }
        }
    }
}

impl Render for ServerLogsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(root) = self.root.clone() else {
            return div().into_any_element();
        };
        self.visible = self.filter(cx);
        let running = crate::rigsync::server_online(self.store.read(cx), None, &root);
        let n_reports = match &self.reports {
            Some(Ok(l)) => l.len(),
            _ => 0,
        };
        let ix = match self.pane {
            Pane::Crashes => 0,
            Pane::Latest => 1,
            Pane::Debug => 2,
            Pane::Older => 3,
        };
        let tabs = TabBar::new("srv-log-tabs")
            .underline()
            .small()
            .selected_index(ix)
            .child(Tab::new().label(if n_reports == 0 { "Crash reports".to_string() } else { format!("Crash reports ({n_reports})") }))
            .child(Tab::new().label(if running { "latest.log · live" } else { "latest.log" }))
            .child(Tab::new().label("debug.log"))
            .child(Tab::new().label("Older logs"))
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                let p = [Pane::Crashes, Pane::Latest, Pane::Debug, Pane::Older][*ix];
                this.select(p, cx);
            }));
        let body = match self.pane {
            Pane::Crashes => self.crashes_body(&root, cx),
            Pane::Older => self.older_body(&root, cx),
            _ => self.log_body(&root, window, cx),
        };
        v_flex().size_full().gap_3().child(tabs).child(body).into_any_element()
    }
}

impl ServerLogsView {
    /// The reports on the left, the picked one as Hopper read it beside.
    fn crashes_body(&mut self, root: &Path, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let list = match &self.reports {
            None => return widgets::skeleton_rows(6, rems(2.75), cx).into_any_element(),
            Some(Err(e)) => {
                return widgets::empty_state(IconName::TriangleAlert, "Can't read the crash reports", Some(e.clone().into()), cx)
                    .into_any_element();
            }
            Some(Ok(l)) if l.is_empty() => {
                return widgets::empty_state(
                    IconName::CircleCheck,
                    "No crash reports",
                    Some("This server hasn't crashed. If it does, the report and what Hopper makes of it show up here.".into()),
                    cx,
                )
                .into_any_element();
            }
            Some(Ok(l)) => l.clone(),
        };
        let picked = self.picked.clone();
        let items: Vec<hopper::crashgroup::Item> =
            list.iter().map(|r| hopper::crashgroup::Item { modified: r.modified, follow_up: r.follow_up, verdict: r.verdict.as_deref() }).collect();
        let roles = hopper::crashgroup::group(&items);
        // Follow-up reports sit folded under the report that explains them,
        // unless that fold is open or one of them is the picked report.
        let shown = |i: usize| match &roles[i] {
            hopper::crashgroup::Role::FollowUp(Some(lead)) => {
                picked.as_ref() == Some(&list[i].rel) || self.folds_open.contains(&list[*lead].rel)
            }
            _ => true,
        };
        // Each report's group: a report that explains a start takes its
        // follow-ups with it when deleted.
        let group_of = |i: usize| {
            let mut g = vec![list[i].rel.clone()];
            if let hopper::crashgroup::Role::Lead(f) = &roles[i] {
                g.extend(f.iter().map(|&j| list[j].rel.clone()));
            }
            g
        };
        let order: Vec<String> = (0..list.len()).filter(|&i| shown(i)).map(|i| list[i].rel.clone()).collect();
        let groups: HashMap<String, Vec<String>> = (0..list.len()).map(|i| (list[i].rel.clone(), group_of(i))).collect();
        self.sel.retain(&order);
        let many = self.sel.many();
        let mut rows: Vec<AnyElement> = Vec::new();
        for (i, r) in list.iter().enumerate().filter(|(i, _)| shown(*i)) {
            let sel = if many { self.sel.contains(&r.rel) } else { picked.as_ref() == Some(&r.rel) };
            let (title, follow) = match &roles[i] {
                hopper::crashgroup::Role::FollowUp(lead) => (
                    hopper::crashgroup::follow_up_title(lead.and_then(|l| list[l].verdict.as_deref())),
                    true,
                ),
                _ => (r.verdict.clone().unwrap_or_else(|| file_name(&r.rel).to_string()), false),
            };
            let rel = r.rel.clone();
            // Right-click on a picked row acts on every picked report.
            let menu_group = if many && sel { picked_groups(&self.sel.in_order(&order), &groups) } else { group_of(i) };
            let menu_many = many && sel;
            let (menu_view, click_order) = (cx.entity(), order.clone());
            rows.push(
                v_flex()
                    .id(("srv-crash-row", i))
                    .gap_0p5()
                    .px_2()
                    .py_1p5()
                    .when(follow, |d| d.pl_5())
                    .rounded(theme.radius)
                    .clickable(cx)
                    .map(|d| if sel { widgets::row_selected(d, cx) } else { d.hover(|s| widgets::row_hover(s, cx)) })
                    .child(div().text_sm().font_medium().when(follow, |d| d.text_color(theme.muted_foreground)).line_clamp(2).child(title))
                    .child(
                        widgets::caption(log_files::when(r.modified), cx)
                            .id(("srv-crash-when", i))
                            .tooltip({
                                let full = hopper::time::exact_full(r.modified);
                                move |w, cx| gpui_kit::component::tooltip::Tooltip::new(full.clone()).build(w, cx)
                            })
                            .text_xs(),
                    )
                    .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                        window.focus(&this.list_focus, cx);
                        if this.sel.click(&rel, e.modifiers(), &click_order) {
                            this.pick(rel.clone(), cx);
                        }
                        cx.notify();
                    }))
                    .context_menu(move |m, _, _| {
                        let (v, g) = (menu_view.clone(), menu_group.clone());
                        let label = if menu_many {
                            format!("Delete {} reports…", g.len())
                        } else if g.len() > 1 {
                            format!("Delete with its {} follow-ups…", g.len() - 1)
                        } else {
                            "Delete report…".to_string()
                        };
                        m.item(PopupMenuItem::new(label).icon(IconName::Trash).on_click(move |_, window, cx| {
                            let g = g.clone();
                            v.update(cx, |this, cx| this.ask_delete_as(g, menu_many, window, cx));
                        }))
                    })
                    .into_any_element(),
            );
            if let hopper::crashgroup::Role::Lead(follow_ups) = &roles[i] {
                let open = self.folds_open.contains(&r.rel);
                let lead = r.rel.clone();
                let label = hopper::crashgroup::fold_label(follow_ups.len());
                rows.push(
                    h_flex()
                        .id(("srv-crash-fold", i))
                        .gap_1()
                        .px_2()
                        .py_1()
                        .pl_5()
                        .rounded(theme.radius)
                        .clickable(cx)
                        .hover(|s| widgets::row_hover(s, cx))
                        .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall().text_color(theme.muted_foreground))
                        .child(widgets::caption(if open { "Hide follow-up reports".to_string() } else { label }, cx).text_xs())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.folds_open.remove(&lead) {
                                this.folds_open.insert(lead.clone());
                            }
                            cx.notify();
                        }))
                        .into_any_element(),
                );
            }
        }
        let bar = many.then(|| {
            let (o, g) = (order.clone(), groups.clone());
            let del = cx.listener(move |this, _: &ClickEvent, window, cx| {
                let rels = picked_groups(&this.sel.in_order(&o), &g);
                this.ask_delete_as(rels, true, window, cx)
            });
            let bar = selection::bar(
                "srv-crash-bulk",
                self.sel.len(),
                Vec::new(),
                Some(("Delete".into(), Box::new(del))),
                cx.listener(|this, _: &ClickEvent, _, cx| {
                    match this.picked.clone() {
                        Some(p) => this.sel.set_one(p),
                        None => this.sel.clear(),
                    }
                    cx.notify();
                }),
                cx,
            );
            selection::floating("srv-crash-bulk", bar)
        });
        let key_order = order.clone();
        let side = div()
            .id("srv-crash-list")
            .size_full()
            .child(
                v_flex()
                    .gap_0p5()
                    .pr_2()
                    .child(
                        h_flex().px_2().pb_1().child(div().flex_1()).child(
                            Button::new("srv-crash-clear")
                                .xsmall()
                                .ghost()
                                .icon(IconName::Trash)
                                .label("Clear all")
                                .tooltip("Deletes every crash report of this server")
                                .on_click(cx.listener(|this, _, window, cx| this.ask_clear(hopper::crashfiles::Kind::Crashes, window, cx))),
                        ),
                    )
                    .children(rows)
                    // Room under the last row for the bulk bar.
                    .when(many, |d| d.pb(rems(4.))),
            )
            .overflow_y_scrollbar();
        let side = div()
            .relative()
            .w(rems(20.))
            .flex_none()
            .h_full()
            .track_focus(&self.list_focus)
            .on_key_down(cx.listener(move |this, e: &KeyDownEvent, window, cx| {
                match selection::on_key(&mut this.sel, e, &key_order, cx) {
                    KeyAct::None => return,
                    KeyAct::Delete => {
                        let rels = picked_groups(&this.sel.in_order(&key_order), &groups);
                        let many = this.sel.many();
                        this.ask_delete_as(rels, many, window, cx)
                    }
                    KeyAct::Moved(k) => this.pick(k, cx),
                    KeyAct::Changed => {
                        if let Some(c) = this.sel.cursor().map(str::to_string).filter(|c| this.sel.len() == 1 && this.sel.contains(c)) {
                            this.pick(c, cx);
                        }
                    }
                }
                cx.notify();
            }))
            .child(side)
            .children(bar);
        let report = picked.and_then(|p| list.iter().find(|r| r.rel == p).cloned());
        let card = match report {
            Some(r) => self.report_card(root, &r, cx),
            None => div().into_any_element(),
        };
        h_flex()
            .flex_1()
            .min_h_0()
            .items_start()
            .gap_4()
            .child(side)
            .child(
                div()
                    .id("srv-crash-card")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    // Capped like a page of text, as the instance's card.
                    .child(div().max_w(rems(56.)).pb_2().child(card))
                    .overflow_y_scrollbar(),
            )
            .into_any_element()
    }

    /// Report `r` as Hopper read it: the mods with how sure, why and what
    /// to do, the story when a mod stopped the server loading, and advice.
    fn report_card(&mut self, root: &Path, r: &Report, cx: &mut Context<Self>) -> AnyElement {
        use crate::crash_view;
        let theme = cx.theme().clone();
        let running_now = crate::rigsync::server_online(self.store.read(cx), None, root);
        let path = root.join(&r.rel);
        let head = |title: &'static str, cx: &mut Context<Self>| {
            let p = path.clone();
            h_flex()
                .gap_2()
                .child(Icon::new(IconName::TriangleAlert).small().text_color(theme.danger))
                .child(div().font_semibold().text_color(theme.danger).child(title))
                .child(
                    widgets::caption(log_files::when(r.modified), cx)
                        .id("srv-crash-head-when")
                        .tooltip({
                            let full = hopper::time::exact_full(r.modified);
                            move |w, cx| gpui_kit::component::tooltip::Tooltip::new(full.clone()).build(w, cx)
                        }),
                )
                .child(div().flex_1())
                .child(
                    Button::new("srv-crash-open")
                        .small()
                        .outline()
                        .icon(IconName::FileText)
                        .label("Open report")
                        .tooltip("Opens the crash report file")
                        .on_click(move |_, _, _| open_path(&p)),
                )
        };
        let card = match self.cards.get(&r.rel) {
            None | Some(None) => {
                return callout(theme.danger, cx)
                    .child(head("The server crashed", cx))
                    .child(h_flex().gap_2().child(Spinner::new().small()).child(widgets::caption("Reading the report", cx)))
                    .into_any_element();
            }
            Some(Some(Err(e))) => {
                let e = e.clone();
                return callout(theme.danger, cx)
                    .child(head("The server crashed", cx))
                    .child(crate::crash_view::note_text(format!("Hopper couldn't read this report: {e}"), cx))
                    .child(div().pt_1().child(self.share_row(&r.rel, cx)))
                    .into_any_element();
            }
            Some(Some(Ok(c))) => c.clone(),
        };
        let share = self.share_row(&r.rel, cx);
        let Some(c) = card.crash else {
            return callout(theme.danger, cx)
                .child(head("The server crashed", cx))
                .child(widgets::caption("Hopper couldn't tell what went wrong. The report has the details.", cx))
                .child(div().pt_1().child(share))
                .into_any_element();
        };

        // Names and icons from the pack the server runs, else the jar's own.
        let addons: Vec<Addon> = {
            let store = self.store.read(cx);
            store.server(root).and_then(|sv| store.paired(sv)).map(|i| i.addons.clone()).unwrap_or_default()
        };
        let pack_root = {
            let store = self.store.read(cx);
            store.server(root).and_then(|sv| store.paired(sv)).map(|i| i.root.clone())
        };
        let names = card.names.clone();
        let display = |f: &str| named(&addons, &names, f).0;
        let link = {
            let (store, addons, names) = (self.store.clone(), addons.clone(), names.clone());
            move |file: &str, key: &str, cx: &App| -> AnyElement {
                let (name, a) = named(&addons, &names, file);
                let icon = a.and_then(|a| store.read(cx).image(&a.icon)).map(Path::to_path_buf);
                // The mod's page opens in the pack's mod list (the Servers
                // page has no peek of its own).
                let (store2, pack) = (store.clone(), pack_root.clone());
                widgets::mod_link(
                    ElementId::Name(format!("srv-log-crash:{key}").into()),
                    name,
                    icon.as_deref(),
                    if pack.is_some() { a.map_or(0, |a| a.project_id) } else { 0 },
                    move |project, _, cx| {
                        if let Some(root) = pack.clone() {
                            store2.update(cx, |_, cx| cx.emit(StoreEvent::Peek { root, project }));
                        }
                    },
                    cx,
                )
                .flex_none()
                .font_semibold()
                .into_any_element()
            }
        };
        // Mod ids in a reason read as names.
        let ids: Vec<(String, String)> =
            c.suspects.iter().filter(|s| !s.mod_id.is_empty()).map(|s| (s.mod_id.clone(), display(&s.file_name))).collect();
        let readable = |t: &str| {
            ids.iter().fold(t.to_string(), |w, (id, n)| w.replace(&format!(" {id},"), &format!(" {n},")).replace(&format!(" {id} "), &format!(" {n} ")))
        };

        let mut body: Vec<AnyElement> = Vec::new();
        let st = c.stopped.clone().filter(|st| c.suspects.iter().any(|s| s.file_name == st.file_name));
        match &st {
            // A mod failed while loading and the crash followed: that story.
            Some(st) => {
                body.push(crash_view::story_title(&c, st, &link, cx).into_any_element());
                let changed = c.suspects.iter().find(|s| s.file_name == st.file_name).and_then(|s| s.changed.clone());
                body.push(crash_view::what_happened(&c, st, changed.as_ref().map(|ch| (ch, ch.file_name.as_str())), &link, cx));
            }
            None => {
                if !c.headline.is_empty() {
                    body.push(div().text_sm().font_medium().line_clamp(2).child(c.headline.clone()).into_any_element());
                }
            }
        }
        // The fixes: put the older mod back, turn the ones at fault off.
        let blocked = running_now || self.fix_busy;
        let changed = st.as_ref().and_then(|st| c.suspects.iter().find(|s| s.file_name == st.file_name)).and_then(|s| s.changed.clone());
        let mods_dir = (!hopper::hopperd::is_remote_root(root)).then(|| root.join("mods"));
        let off_mods: Vec<&hopper::crashdetect::Suspect> = hopper::crashdetect::to_turn_off(&c)
            .into_iter()
            .filter(|s| mods_dir.as_ref().is_none_or(|d| d.join(s.file_name.trim_end_matches(".disabled")).exists()))
            .collect();
        let is_back = self.fixed.contains(&r.rel);
        if changed.is_some() || !off_mods.is_empty() {
            let mut fix = crash_view::fix_box(cx);
            if let (Some(ch), true) = (&changed, is_back) {
                fix = fix.child(widgets::done_mark(
                    "srv-log-put-back-done",
                    format!("{} is back", if ch.from.is_empty() { "The older version".to_string() } else { ch.from.clone() }),
                    true,
                    cx,
                ));
                fix = fix.child(widgets::caption("Start the server to try again. Backups has the newer one.", cx).text_xs());
            } else {
                let mut buttons = h_flex().gap_2().flex_wrap();
                if let Some(ch) = changed.clone() {
                    let rel = r.rel.clone();
                    buttons = buttons.child(
                        Button::new("srv-log-put-back")
                            .small()
                            .primary()
                            .icon(IconName::Undo2)
                            .label(if ch.from.is_empty() { "Put the older version back".to_string() } else { format!("Put {} back", ch.from) })
                            .tooltip("Swaps in the older copy. The newer one goes to backups.")
                            .disabled(blocked)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let ch = ch.clone();
                                this.run_fix(rel.clone(), true, move |root| crate::server_crash::put_back(root, &ch), window, cx);
                            })),
                    );
                }
                if !off_mods.is_empty() {
                    let label = match off_mods.len() {
                        1 => format!("Turn off {}…", display(&off_mods[0].file_name)),
                        n => format!("Turn off {n} mods…"),
                    };
                    let files: Vec<String> = off_mods.iter().map(|s| s.file_name.clone()).collect();
                    buttons = buttons.child(
                        Button::new("srv-log-turn-off")
                            .small()
                            .outline()
                            .icon(IconName::PowerOff)
                            .label(label)
                            .tooltip("Shows the mods first. Turn them back on in the server's Mods.")
                            .disabled(blocked || self.turn_off_pick.is_some())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.turn_off_pick = Some(files.iter().cloned().collect());
                                cx.notify();
                            })),
                    );
                }
                fix = fix.child(buttons);
            }
            body.insert(1.min(body.len()), fix.into_any_element());
            if let Some(picked) = self.turn_off_pick.clone().filter(|_| !off_mods.is_empty()) {
                let view = cx.entity().downgrade();
                let toggle: std::rc::Rc<dyn Fn(String, bool, &mut Window, &mut App)> = std::rc::Rc::new(move |file, on, _, cx| {
                    view.update(cx, |this, cx| {
                        if let Some(p) = this.turn_off_pick.as_mut() {
                            if on {
                                p.insert(file);
                            } else {
                                p.remove(&file);
                            }
                        }
                        cx.notify();
                    })
                    .ok();
                });
                let rel = r.rel.clone();
                let confirm = cx.listener(move |this, _, window, cx| {
                    let Some(picked) = this.turn_off_pick.take() else { return };
                    let files: Vec<String> = picked.into_iter().collect();
                    this.run_fix(rel.clone(), false, move |root| crate::server_crash::turn_off(root, files), window, cx);
                });
                let cancel = cx.listener(|this, _, _, cx| {
                    this.turn_off_pick = None;
                    cx.notify();
                });
                body.insert(
                    2.min(body.len()),
                    crash_view::turn_off_picker("srv-log", &off_mods, &picked, &link, &readable, toggle, confirm, cancel, blocked, "server", cx),
                );
            }
        }
        // The mods: the first two, the rest a click away, so a long list
        // doesn't push the story off the screen.
        let fold = 2;
        let all_open = self.more_open.contains(&r.rel);
        let total = c.suspects.len();
        let rows: Vec<AnyElement> = c
            .suspects
            .iter()
            .enumerate()
            .filter(|(i, _)| all_open || *i < fold)
            .map(|(i, s)| {
                let mut below: Vec<AnyElement> = Vec::new();
                below.extend(crash_view::partner_line(s, &link, cx));
                below.extend(crash_view::steps_list(s, &readable, cx));
                let key = s.file_name.clone();
                let open = self.why_open.contains(&key);
                let toggle = cx.listener(move |this, _, _, cx| {
                    if !this.why_open.remove(&key) {
                        this.why_open.insert(key.clone());
                    }
                    cx.notify();
                });
                below.extend(crash_view::why_fold(ElementId::Name(format!("srv-log-why:{i}").into()), s, open, &readable, toggle, cx));
                crash_view::suspect_row(link(&s.file_name, &format!("row{i}"), cx), s, readable(&s.why), below, cx).into_any_element()
            })
            .collect();
        if !rows.is_empty() {
            body.push(widgets::caption("Mods in this crash, most likely first:", cx).text_xs().pt_1().into_any_element());
            body.push(v_flex().children(rows).into_any_element());
            if total > fold {
                let rel = r.rel.clone();
                body.push(
                    h_flex()
                        .child(
                            Button::new("srv-log-more")
                                .ghost()
                                .small()
                                .icon(if all_open { IconName::ChevronUp } else { IconName::ChevronDown })
                                .label(if all_open { "Show fewer".to_string() } else { format!("Show {} more", total - fold) })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if !this.more_open.remove(&rel) {
                                        this.more_open.insert(rel.clone());
                                    }
                                    cx.notify();
                                })),
                        )
                        .into_any_element(),
                );
            }
        }
        if let Some(n) = crash_view::note(&c) {
            body.push(
                callout(theme.warning, cx)
                    .child(
                        h_flex()
                            .gap_2()
                            .items_start()
                            .child(Icon::new(IconName::Info).small().text_color(theme.warning))
                            .child(div().flex_1().min_w_0().text_sm().whitespace_normal().child(readable(&n))),
                    )
                    .into_any_element(),
            );
        }
        if let Some(n) = c.lingered_note() {
            body.push(crate::crash_view::note_text(n, cx).into_any_element());
        }
        callout(theme.danger, cx)
            .child(head(if c.stuck { "The start got stuck" } else { "The server crashed" }, cx))
            .children(body)
            .child(div().pt_1().child(share))
            .into_any_element()
    }

    /// A log pane: one toolbar, then the log filling the rest.
    fn log_body(&mut self, root: &Path, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(log) = self.log.as_ref() else {
            return div().into_any_element();
        };
        let (rel, loading, total) = (log.rel, log.loading, log.lines.len());
        if let Some(e) = log.error.clone().filter(|_| total == 0) {
            let name = file_name(rel).to_string();
            // Not written yet is the usual reason; say so plainly.
            let missing = e.contains("isn't one of this server's logs") || e.contains("cannot find") || e.contains("No such file");
            return if missing {
                widgets::empty_state(IconName::FileText, format!("No {name} yet"), Some("The server writes it when it starts.".into()), cx)
            } else {
                widgets::empty_state(IconName::TriangleAlert, format!("Can't read {name}"), Some(e.into()), cx)
            }
            .into_any_element();
        }
        let n = self.line_count();
        let filtered = self.visible.is_some();
        let share = self.share_row(rel, cx);
        let dir = root.join("logs");
        let toolbar = h_flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(div().w(rems(16.)).child(widgets::search_input(&self.query).small().cleanable(true)))
            .child(
                Checkbox::new("srv-errors-only")
                    .label("Errors and warnings only")
                    .checked(self.errors_only)
                    .on_change(cx.listener(|this, v: &bool, _, cx| {
                        this.errors_only = *v;
                        this.scroll_to_end();
                        cx.notify();
                    })),
            )
            .when(filtered, |r| r.child(widgets::caption(format!("{n} of {total} lines"), cx).text_xs()))
            .when(loading, |r| r.child(Spinner::new().xsmall()))
            .child(div().flex_1())
            .child(share)
            .child(
                Button::new("srv-logs-folder")
                    .small()
                    .ghost()
                    .icon(IconName::FolderOpen)
                    .tooltip("Open the logs folder")
                    .on_click(move |_, _, _| open_path(&dir)),
            );
        v_flex()
            .flex_1()
            .min_h_0()
            .gap_2()
            .child(toolbar)
            .child(crate::line_select::view(self, "srv-log-lines", None, window, cx))
            .into_any_element()
    }
}

impl Lines for ServerLogsView {
    fn line_select(&self) -> &LineSelect {
        &self.lines
    }

    fn line_select_mut(&mut self) -> &mut LineSelect {
        &mut self.lines
    }

    fn row_count(&self) -> usize {
        self.line_count()
    }

    fn row_key(&self, row: usize) -> Option<usize> {
        let l = self.log.as_ref()?;
        let i = match &self.visible {
            Some(v) => *v.get(row)?,
            None => row,
        };
        (i < l.lines.len()).then_some(i)
    }

    fn key_text(&self, key: usize) -> Option<String> {
        Some(self.log.as_ref()?.lines.get(key)?.replace('\t', "    "))
    }

    fn row_color(&self, row: usize, cx: &App) -> Hsla {
        let theme = cx.theme();
        match self.line(row).map(level) {
            Some(Level::Error) => theme.danger,
            Some(Level::Warn) => theme.warning,
            _ => theme.foreground,
        }
    }
}

/// "crash-2026-11-04_14.54.48-server.txt" from its relative path.
/// The picked reports with their follow-ups, each once, in list order.
fn picked_groups(picked: &[String], groups: &HashMap<String, Vec<String>>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in picked {
        for r in groups.get(p).cloned().unwrap_or_else(|| vec![p.clone()]) {
            if !out.contains(&r) {
                out.push(r);
            }
        }
    }
    out
}

fn file_name(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// A jar's name for people and the pack's copy of it (for its icon and
/// page): the same file, else the same name stem with the same name (a
/// stem alone can match another mod). Else the name the server side gave
/// (the jar's own), else the file name.
fn named<'a>(addons: &'a [Addon], names: &HashMap<String, String>, file: &str) -> (String, Option<&'a Addon>) {
    let bare = file.trim_end_matches(".disabled");
    if let Some(a) = addons.iter().find(|a| a.file_name.trim_end_matches(".disabled").eq_ignore_ascii_case(bare)) {
        let name = if a.name.is_empty() { bare.trim_end_matches(".jar").to_string() } else { a.name.clone() };
        return (name, Some(a));
    }
    let own = names.get(file).or_else(|| names.get(bare)).cloned().unwrap_or_else(|| bare.trim_end_matches(".jar").to_string());
    let stem = hopper::serverpack::jar_stem(bare);
    let a = addons
        .iter()
        .find(|a| !stem.is_empty() && hopper::serverpack::jar_stem(&a.file_name) == stem && a.name.eq_ignore_ascii_case(&own));
    (own, a)
}

// ---- Reading, off the UI thread ------------------------------------------

/// The server's crash reports, newest first, the newest few with a
/// verdict. On this PC the verdicts are worked out here (`known` has the
/// ones already done, returned with the new ones); Hopper Server sends its own.
#[allow(clippy::type_complexity)]
fn list_reports(root: &Path, mut known: HashMap<(String, i64), String>) -> Result<(Vec<Report>, HashMap<(String, i64), String>), String> {
    if let Some(l) = hopper::hopperd::locate(root) {
        let (c, id, _) = l.map_err(|e| format!("{e:#}"))?;
        let list = c.crashes(&id).map_err(|e| format!("{e:#}"))?;
        let reports = list.into_iter().map(|(rel, modified, verdict, follow_up)| Report { rel, modified: modified as i64, verdict, follow_up }).collect();
        return Ok((reports, known));
    }
    // Over SFTP / FTP: fetch what changed into Hopper's copy first.
    let _ = hopper::remotefs::refresh_path(&root.join("crash-reports"));
    let mods = root.join("mods");
    let mut reports: Vec<Report> = hopper::logs::crash_report_files(root)
        .into_iter()
        .filter(|p| p.is_file())
        .map(|p| {
            let modified = std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs() as i64);
            Report { rel: format!("crash-reports/{}", p.file_name().unwrap_or_default().to_string_lossy()), modified, verdict: None, follow_up: false }
        })
        .collect();
    reports.sort_by(|a, b| b.modified.cmp(&a.modified).then(b.rel.cmp(&a.rel)));
    // A few new verdicts per pass, so the list shows at once and fills in.
    // The first pass only lists them (the first verdict reads every jar).
    let mut budget = if known.is_empty() { 0 } else { VERDICTS_PER_PASS };
    for r in reports.iter_mut().take(WITH_VERDICT) {
        r.follow_up = r.rel.ends_with(".txt") && hopper::crashgroup::is_follow_up_text(&hopper::logs::tail(&root.join(&r.rel), 2 << 20));
        let key = (r.rel.clone(), r.modified);
        let v = match known.get(&key) {
            Some(v) => v.clone(),
            None if budget == 0 => continue,
            None => {
                budget -= 1;
                let v = hopper::crashdetect::server_report(root, &root.join(&r.rel))
                    .map(|c| hopper::crashdetect::verdict(&c, &|s| hopper::crashdetect::jar_name(&mods, &s.file_name)))
                    .unwrap_or_default();
                known.insert(key, v.clone());
                v
            }
        };
        r.verdict = Some(v).filter(|v| !v.is_empty());
    }
    // Marks the first pass done (no report has an empty path).
    known.entry((String::new(), 0)).or_default();
    Ok((reports, known))
}

/// Report `rel` as Hopper reads it, with a name for each jar it names.
fn read_card(root: &Path, rel: &str) -> Result<Card, String> {
    if let Some(l) = hopper::hopperd::locate(root) {
        let (c, id, _) = l.map_err(|e| format!("{e:#}"))?;
        let (crash, names) = c.crash_read(&id, rel).map_err(|e| format!("{e:#}"))?;
        return Ok(Card { crash, names });
    }
    let crash = hopper::crashdetect::server_report(root, &root.join(rel));
    let mods = root.join("mods");
    let mut names = HashMap::new();
    if let Some(c) = &crash {
        let files = c
            .suspects
            .iter()
            .flat_map(|s| std::iter::once(s.file_name.clone()).chain(s.partner.iter().map(|p| p.file_name.clone())))
            .chain(c.stopped.iter().flat_map(|st| std::iter::once(st.file_name.clone()).chain(st.skipped.iter().cloned())));
        for f in files {
            let n = hopper::crashdetect::jar_name(&mods, &f);
            names.insert(f, n);
        }
    }
    Ok(Card { crash, names })
}

/// A log's text: its end (`from` 0), or what was written since `from`.
/// (text, where to read from next, whether it started over), the same as
/// Hopper Server's `logs/text`.
fn read_log(root: &Path, rel: &str, from: u64) -> Result<(String, u64, bool), String> {
    if let Some(l) = hopper::hopperd::locate(root) {
        let (c, id, _) = l.map_err(|e| format!("{e:#}"))?;
        return c.log_text(&id, rel, from).map_err(|e| format!("{e:#}"));
    }
    let path = root.join(rel);
    let _ = hopper::remotefs::refresh_path(&path);
    local_text(&path, from).map_err(|e| e.to_string())
}

fn local_text(path: &Path, from: u64) -> std::io::Result<(String, u64, bool)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let size = f.metadata()?.len();
    let (start, reset) = if from == 0 || from > size || size - from > APPEND_MAX { (size.saturating_sub(OPEN_BYTES), true) } else { (from, false) };
    if !reset && start == size {
        return Ok((String::new(), size, false));
    }
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    f.take(size - start).read_to_end(&mut buf)?;
    // Whole lines only: a line still being written comes with the next read.
    match buf.iter().rposition(|b| *b == b'\n') {
        Some(i) => buf.truncate(i + 1),
        None => buf.clear(),
    }
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    // A tail starts mid-line: drop that part line.
    if reset
        && start > 0
        && let Some(i) = text.find('\n')
    {
        text.drain(..=i);
    }
    Ok((text, start + buf.len() as u64, reset))
}

/// Uploads file `rel` to mclo.gs, cleaned first (IPs, tokens, paths, the
/// user name): Hopper Server does it on its side.
fn share_log(root: &Path, rel: &str) -> anyhow::Result<String> {
    if let Some(l) = hopper::hopperd::locate(root) {
        let (c, id, _) = l?;
        let v = c.post(&format!("/servers/{}/logs/share", hopper::hopperd::enc(&id)), serde_json::json!({ "path": rel }))?;
        return v["url"].as_str().map(str::to_string).ok_or_else(|| anyhow::anyhow!("Hopper Server didn't send a link."));
    }
    let text = hopper::hosting::logshare::read(root, rel)?;
    let (clean, _) = hopper::hosting::logshare::redact(&text, Some(root));
    hopper::mclogs::upload(&clean)
}

#[cfg(test)]
mod tests {
    use super::local_text;
    use std::io::Write;

    #[test]
    fn follows_whole_lines_and_starts_over() {
        let dir = std::env::temp_dir().join(format!("hopper-srvlogs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("latest.log");
        std::fs::write(&path, "one\ntwo\nthr").unwrap();
        let (text, to, reset) = local_text(&path, 0).unwrap();
        assert_eq!((text.as_str(), to, reset), ("one\ntwo\n", 8, true));
        assert_eq!(local_text(&path, to).unwrap(), (String::new(), 8, false));
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"ee\nfour\n").unwrap();
        drop(f);
        let (text, end, reset) = local_text(&path, to).unwrap();
        assert_eq!((text.as_str(), reset), ("three\nfour\n", false));
        assert_eq!(end, std::fs::metadata(&path).unwrap().len());
        // A new run truncates the file: read it again from the start.
        std::fs::write(&path, "new\n").unwrap();
        assert_eq!(local_text(&path, 100).unwrap(), ("new\n".to_string(), 4, true));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
