//! An instance's Logs & crashes tab: tabs for crash help, the game's
//! live log, debug.log and crash reports, each log with search and sharing
//! on mclo.gs.

use crate::widgets::Clickable as _;
use crate::crash_help::{CrashHelp, callout};
use crate::line_select::{LineSelect, Lines};
use crate::log_files;
use crate::store::{Store, open_path};
use crate::selection::{self, KeyAct, Selection};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::logs::{Level, crash_reports, crash_summary, is_problem, level, tail};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

mod crash_story;
mod crash_switch;

/// How much of a log to show when it opens.
const OPEN_BYTES: u64 = 400_000;
/// More growth than this in one second reloads the tail instead.
const APPEND_MAX: u64 = 4_000_000;
/// Lines kept while following; older ones drop off the top.
const KEEP_LINES: usize = 200_000;

/// A log on screen.
struct LogFile {
    path: PathBuf,
    lines: Vec<String>,
    /// Indices into `lines` of errors, warnings and stack frames.
    problems: Vec<usize>,
    /// Bytes of the file read so far (always ends after a full line while
    /// following, so a half-written line is read again whole).
    read_to: u64,
    /// Still reading the file.
    loading: bool,
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

/// What a read of the log brought.
enum Chunk {
    /// Nothing new.
    Same,
    /// Lines after the old end, and the new end.
    Append(String, u64),
    /// The file shrank or jumped (a new run): its tail, and the end.
    Reset(String, u64),
}

/// The last `max` bytes of `path`; with `whole_lines`, only up to its last
/// line break (a line still being written is left for the next read).
fn read_tail(path: &Path, max: u64, whole_lines: bool) -> std::io::Result<(String, u64)> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(max);
    read_range(&mut f, start, len, whole_lines)
}

/// Bytes `start..end` of `f` as text, and where reading stopped.
fn read_range(
    f: &mut std::fs::File,
    start: u64,
    end: u64,
    whole_lines: bool,
) -> std::io::Result<(String, u64)> {
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity((end - start) as usize);
    f.take(end - start).read_to_end(&mut buf)?;
    if whole_lines {
        match buf.iter().rposition(|b| *b == b'\n') {
            Some(i) => buf.truncate(i + 1),
            None => buf.clear(),
        }
    }
    let to = start + buf.len() as u64;
    Ok((String::from_utf8_lossy(&buf).into_owned(), to))
}

/// What the log gained since byte `from` (runs off the UI thread).
fn read_new(path: &Path, from: u64) -> Chunk {
    let Ok(mut f) = std::fs::File::open(path) else {
        return Chunk::Same;
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len == from {
        return Chunk::Same;
    }
    if len < from || len - from > APPEND_MAX {
        return match read_tail(path, OPEN_BYTES, true) {
            Ok((text, to)) => Chunk::Reset(text, to),
            Err(_) => Chunk::Same,
        };
    }
    match read_range(&mut f, from, len, true) {
        Ok((text, to)) if to > from => Chunk::Append(text, to),
        _ => Chunk::Same,
    }
}

/// The Logs tab's own tabs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LogTab {
    /// Crash Assistant and the latest crash.
    Help,
    /// latest.log, followed.
    Game,
    /// debug.log, followed.
    Debug,
    Crashes,
}

/// The newest crash report, read once per file and change.
struct Crash {
    path: PathBuf,
    mtime: Option<std::time::SystemTime>,
    summary: Vec<String>,
}

pub struct LogsView {
    store: Entity<Store>,
    root: Option<PathBuf>,
    help: Entity<CrashHelp>,
    /// The file shown and its lines.
    log: Option<LogFile>,
    errors_only: bool,
    keep_names: bool,
    /// mclo.gs links by file; `None` while uploading.
    shared: HashMap<PathBuf, Option<Result<String, String>>>,
    /// The latest crash report of the instance, if any.
    crash: Option<Crash>,
    /// The crash card's mods (jars) whose "Why?" list is open.
    why_open: std::collections::HashSet<String>,
    /// "Turn off…" asked: the jars ticked in its list (nothing is turned
    /// off until its confirm).
    turn_off_pick: Option<std::collections::HashSet<String>>,
    /// Every crash report, newest first.
    crashes: Vec<PathBuf>,
    /// What each of `crashes` is: explaining report, follow-up of one, or
    /// neither (follow-ups fold under the report that explains them).
    crash_roles: Vec<hopper::crashgroup::Role>,
    /// The follow-up reports under the older ones are shown.
    folds_open: bool,
    /// The older reports picked with Ctrl / Shift, by path.
    older_sel: Selection,
    /// The older reports' list, for its keys.
    older_focus: FocusHandle,
    /// The newest report file and its time as last looked at.
    newest_seen: Option<(PathBuf, Option<std::time::SystemTime>)>,
    tab: LogTab,
    /// The user picked a tab for this instance; it stays.
    tab_picked: bool,
    query: Entity<InputState>,
    /// Lines the viewer shows when filtered (errors only or a search);
    /// `None` shows them all.
    visible: Option<Vec<usize>>,
    /// The log's lines on screen, with text picking.
    lines: LineSelect,
    _follow: Option<Task<()>>,
    _crash_watch: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl LogsView {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let help = cx.new(|cx| CrashHelp::new(store.clone(), cx));
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search this log"));
        let subs = vec![
            cx.subscribe(&query, |this, _, e: &InputEvent, cx| {
                if matches!(e, InputEvent::Change) {
                    this.scroll_to_end();
                    cx.notify();
                }
            }),
        ];
        Self {
            store,
            root: None,
            help,
            log: None,
            errors_only: false,
            keep_names: false,
            shared: HashMap::new(),
            crash: None,
            why_open: Default::default(),
            turn_off_pick: None,
            crashes: Vec::new(),
            crash_roles: Vec::new(),
            folds_open: false,
            older_sel: Selection::new(),
            older_focus: cx.focus_handle(),
            newest_seen: None,
            tab: LogTab::Game,
            tab_picked: false,
            query,
            visible: None,
            lines: LineSelect::new(cx),
            _follow: None,
            _crash_watch: None,
            _subscriptions: subs,
        }
    }

    pub fn show_for(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.store.read(cx).instance(&root).is_none() {
            return;
        }
        let new_root = self.root.as_ref() != Some(&root);
        if new_root {
            self.log = None;
            self.lines.clear();
            self._follow = None;
            self.crash = None;
            self.crashes.clear();
            self.crash_roles.clear();
            self.newest_seen = None;
            self.folds_open = false;
            self.older_sel.clear();
            self.tab_picked = false;
        }
        self.root = Some(root.clone());
        if new_root {
            // Opens on the game's log; after a crash, render switches to
            // Crash help.
            self.select(LogTab::Game, cx);
        }
        self.watch_crashes(root.clone(), cx);
        self.help.update(cx, |h, cx| h.show(root, cx));
        cx.notify();
    }

    /// Looks for a newer crash report now and every few seconds, off the
    /// UI thread; reads a report only when it is new or changed.
    fn watch_crashes(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self._crash_watch = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(known) = this.update(cx, |this, _| this.newest_seen.clone()) else {
                    return;
                };
                let r = root.clone();
                let found = cx
                    .background_spawn(async move {
                        let all = crash_reports(&r);
                        let newest = all.first()?.clone();
                        let mtime = std::fs::metadata(&newest).and_then(|m| m.modified()).ok();
                        if known.as_ref() == Some(&(newest.clone(), mtime)) {
                            return Some(None);
                        }
                        // The newest report can be a follow-up of the start's
                        // real crash: the card and the tab open on the one
                        // that names the cause.
                        let roles = hopper::crashgroup::group_files(&all[..all.len().min(20)]);
                        let shown = match roles.first() {
                            Some(hopper::crashgroup::Role::FollowUp(Some(j))) => all[*j].clone(),
                            _ => newest.clone(),
                        };
                        let shown_mtime = std::fs::metadata(&shown).and_then(|m| m.modified()).ok();
                        let summary = crash_summary(&tail(&shown, 200_000));
                        Some(Some((Crash { path: shown, mtime: shown_mtime, summary }, all, roles, (newest, mtime))))
                    })
                    .await;
                let alive = this.update(cx, |this, cx| {
                    if this.root.as_ref() != Some(&root) {
                        return;
                    }
                    match found {
                        None if this.crash.is_some() => {
                            this.crash = None;
                            this.crashes.clear();
                            this.crash_roles.clear();
                            this.newest_seen = None;
                        }
                        Some(Some((c, all, roles, seen))) => {
                            this.store.update(cx, |s, _| s.newest_crash_report(&root, &c.path));
                            this.crash = Some(c);
                            this.crashes = all;
                            this.crash_roles = roles;
                            this.newest_seen = Some(seen);
                        }
                        _ => return,
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
                cx.background_executor().timer(Duration::from_secs(5)).await;
            }
        }));
    }

    /// Shows a log, read off the UI thread. With `follow`, reads what the
    /// game appends every second and keeps the view at the bottom while it
    /// is there (scrolled up, it stays put).
    fn open_log(&mut self, path: PathBuf, follow: bool, cx: &mut Context<Self>) {
        self.lines.clear();
        self.log = Some(LogFile {
            path: path.clone(),
            lines: Vec::new(),
            problems: Vec::new(),
            read_to: 0,
            loading: true,
        });
        self._follow = Some(cx.spawn(async move |this, cx| {
            let p = path.clone();
            let first = cx
                .background_spawn(async move { read_tail(&p, OPEN_BYTES, follow) })
                .await;
            let alive = this.update(cx, |this, cx| {
                let Some(log) = this.log.as_mut().filter(|l| l.path == path) else {
                    return false;
                };
                log.loading = false;
                match first {
                    Ok((text, to)) => {
                        log.push(&text);
                        log.read_to = to;
                    }
                    Err(e) => log.push(&format!("Could not open {}: {e}", path.display())),
                }
                // A followed log opens at its end; a crash report at its top,
                // where the cause is.
                if follow {
                    this.scroll_to_end();
                } else {
                    this.lines.scroll.scroll_to_item(0, ScrollStrategy::Top);
                }
                cx.notify();
                true
            });
            if !matches!(alive, Ok(true)) || !follow {
                return;
            }
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let Ok(Some(from)) = this.update(cx, |this, _| {
                    this.log.as_ref().filter(|l| l.path == path).map(|l| l.read_to)
                }) else {
                    return;
                };
                let p = path.clone();
                let chunk = cx.background_spawn(async move { read_new(&p, from) }).await;
                if matches!(chunk, Chunk::Same) {
                    continue;
                }
                let alive = this.update(cx, |this, cx| {
                    // Follow only a reader who is at the bottom already.
                    let at_end = this.lines.scroll.is_scrolled_to_end().unwrap_or(true);
                    let Some(log) = this.log.as_mut().filter(|l| l.path == path) else {
                        return;
                    };
                    match chunk {
                        Chunk::Same => {}
                        Chunk::Append(text, to) => {
                            log.push(&text);
                            log.read_to = to;
                        }
                        Chunk::Reset(text, to) => {
                            log.clear();
                            log.push(&text);
                            log.read_to = to;
                        }
                    }
                    if at_end {
                        this.scroll_to_end();
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
        }));
        cx.notify();
    }

    fn scroll_to_end(&self) {
        self.lines.scroll_to_end(self.line_count());
    }

    /// How many lines the viewer shows (all, or the problems only).
    fn line_count(&self) -> usize {
        match (&self.log, &self.visible) {
            (None, _) => 0,
            (Some(_), Some(v)) => v.len(),
            (Some(l), None) => l.lines.len(),
        }
    }

    /// Line `ix` of the viewer.
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
        let base: Box<dyn Iterator<Item = usize>> = if self.errors_only {
            Box::new(log.problems.iter().copied())
        } else {
            Box::new(0..log.lines.len())
        };
        Some(
            base.filter(|i| q.is_empty() || log.lines[*i].to_lowercase().contains(&q))
                .collect(),
        )
    }

    /// Shows tab `tab`, opening its log.
    fn select(&mut self, tab: LogTab, cx: &mut Context<Self>) {
        self.tab = tab;
        let Some(root) = self.root.clone() else { return };
        let want = match tab {
            LogTab::Help => None,
            LogTab::Game => Some((root.join("logs").join("latest.log"), true)),
            LogTab::Debug => Some((root.join("logs").join("debug.log"), true)),
            LogTab::Crashes => self.crash.as_ref().map(|c| c.path.clone()).or_else(|| self.crashes.first().cloned()).map(|p| (p, false)),
        };
        if let Some((path, follow)) = want
            && self.log.as_ref().map(|l| &l.path) != Some(&path)
        {
            self.open_log(path, follow, cx);
        }
        cx.notify();
    }

    /// Shows crash report `p` in the Crash reports tab.
    fn open_crash(&mut self, p: PathBuf, cx: &mut Context<Self>) {
        self.tab = LogTab::Crashes;
        self.tab_picked = true;
        if self.log.as_ref().map(|l| &l.path) != Some(&p) {
            self.open_log(p, false, cx);
        }
        cx.notify();
    }

    /// The report at `p` and, when it explains a start, its follow-up
    /// reports (they go together). As paths under the instance folder.
    fn report_group(&self, p: &Path) -> Vec<String> {
        let Some(root) = self.root.as_ref() else { return Vec::new() };
        let rel = |q: &Path| format!("crash-reports/{}", q.file_name().unwrap_or_default().to_string_lossy());
        let mut out = vec![rel(p)];
        let at = self.crashes.iter().position(|c| c == p);
        if let Some(hopper::crashgroup::Role::Lead(f)) = at.and_then(|i| self.crash_roles.get(i)) {
            out.extend(f.iter().filter_map(|&j| self.crashes.get(j)).map(|q| rel(q)));
        }
        let _ = root;
        out
    }

    /// The delete buttons of the toolbar: the shown crash report (and its
    /// follow-ups) and "Clear all" on the Crash reports tab; "Clear old
    /// logs" on the game's logs.
    fn delete_buttons(&self, shown: &Path, cx: &mut Context<Self>) -> AnyElement {
        if self.tab == LogTab::Crashes {
            let group = self.report_group(shown);
            return h_flex()
                .gap_1()
                .child(
                    Button::new("crash-del")
                        .small()
                        .ghost()
                        .icon(IconName::Trash)
                        .tooltip("Delete this crash report")
                        .on_click(cx.listener(move |this, _, window, cx| this.ask_delete(group.clone(), window, cx))),
                )
                .child(
                    Button::new("crash-clear")
                        .small()
                        .ghost()
                        .label("Clear all")
                        .tooltip("Deletes every crash report of this instance")
                        .on_click(cx.listener(|this, _, window, cx| this.ask_clear(hopper::crashfiles::Kind::Crashes, window, cx))),
                )
                .into_any_element();
        }
        Button::new("logs-clear-old")
            .small()
            .ghost()
            .icon(IconName::Trash)
            .label("Clear old logs")
            .tooltip("Deletes the older logs; the current latest.log and debug.log stay")
            .on_click(cx.listener(|this, _, window, cx| this.ask_clear(hopper::crashfiles::Kind::Logs, window, cx)))
            .into_any_element()
    }

    /// "Delete" on crash reports `rels` (one, or a report with its follow-ups).
    fn ask_delete(&mut self, rels: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.ask_delete_as(rels, false, window, cx)
    }

    /// "Delete" on crash reports `rels`; `picked` when they are the reports
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
        let running = self.store.read(cx).running(&root).is_some();
        let view = cx.entity();
        log_files::confirm(window, cx, title, body, move |window, cx| {
            let r = log_files::delete(&root, &rels, running);
            let ok = r.is_ok();
            log_files::report(window, cx, r);
            if ok {
                view.update(cx, |this, cx| this.after_delete(&rels, cx));
            }
        });
    }

    /// "Clear all" for crash reports, or "Clear old logs".
    fn ask_clear(&mut self, kind: hopper::crashfiles::Kind, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let crashes = kind == hopper::crashfiles::Kind::Crashes;
        let n = hopper::crashfiles::clearable(&root, kind).len();
        if n == 0 {
            window.push_notification(gpui_kit::component::notification::Notification::info(if crashes { "No crash reports to delete." } else { "No older logs to delete." }), cx);
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

    /// Files went: look at the folders again, and leave a deleted file.
    fn after_delete(&mut self, rels: &[String], cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        self.older_sel.clear();
        self.newest_seen = None;
        let gone = |p: &Path| rels.iter().any(|r| root.join(r) == p) || !p.exists();
        if self.log.as_ref().is_some_and(|l| gone(&l.path)) {
            self.log = None;
            self.lines.clear();
            self._follow = None;
            self.tab_picked = false;
            self.crash = None;
            self.crashes.clear();
            self.crash_roles.clear();
            self.select(LogTab::Game, cx);
        }
        self.watch_crashes(root, cx);
        cx.notify();
    }

    fn share(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.shared.insert(path.clone(), None);
        let keep = self.keep_names;
        let s = self.store.read(cx).settings();
        let player = s
            .signed_in_as
            .clone()
            .or_else(|| hopper::account::active(Path::new(&s.launcher_dir)).map(|a| a.name));
        cx.spawn(async move |this, cx| {
            let p = path.clone();
            let r = cx
                .background_spawn(async move {
                    let b = std::fs::read(&p)?;
                    let text = String::from_utf8_lossy(&b).into_owned();
                    let text = if keep {
                        text
                    } else {
                        hopper::mclogs::scrub(&text, player.as_deref())
                    };
                    hopper::mclogs::upload(&text)
                })
                .await
                .map_err(|e| format!("{e:#}"));
            this.update(cx, |this, cx| {
                this.shared.insert(path, Some(r));
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn share_row(&self, path: &Path, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        match self.shared.get(path) {
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
                    .child(
                        Button::new("shared-link")
                            .link()
                            .small()
                            .label(url.clone())
                            .on_click(move |_, _, cx| cx.open_url(&u1)),
                    )
                    .child(
                        Button::new("shared-copy")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Copy)
                            .tooltip("Copy link")
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(u2.clone()))
                            }),
                    )
                    .into_any_element()
            }
            Some(Some(Err(e))) => div()
                .text_sm()
                .text_color(theme.danger)
                .child(e.clone())
                .into_any_element(),
            None => {
                let p = path.to_path_buf();
                h_flex()
                    .gap_3()
                    .child(
                        Button::new("share-log")
                            .outline()
                            .small()
                            .icon(IconName::Share2)
                            .label("Share on mclo.gs")
                            .tooltip("Upload this log and get a link for Discord or a bug report. The log becomes public.")
                            .on_click(cx.listener(move |this, _, _, cx| this.share(p.clone(), cx))),
                    )
                    .child(
                        Checkbox::new("keep-names")
                            .label("Keep my user and player name")
                            .tooltip("Off: Windows user folders become <user> and your player name becomes <player> before upload")
                            .checked(self.keep_names)
                            .on_change(cx.listener(|this, v: &bool, _, cx| {
                                this.keep_names = *v;
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            }
        }
    }
}

impl Render for LogsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(root) = self.root.clone() else {
            return div().into_any_element();
        };
        self.visible = self.filter(cx);
        let theme = cx.theme().clone();
        let running = self.store.read(cx).running(&root).is_some();
        // After a crash the tab opens on Crash help, with the crash on top.
        // So does a crash Hopper caught while the game is still open (on its
        // error screen).
        let caught = self.store.update(cx, |s, _| {
            let started = s.running(&root).map_or(i64::MAX, |r| r.started as i64);
            s.crash(&root).is_some_and(|c| c.when >= started)
        });
        // So does a game that couldn't join a server or open a world.
        let join_failed = !running && self.store.update(cx, |s, _| s.join_problem(&root).is_some());
        if !self.tab_picked && self.tab == LogTab::Game && ((!running && self.crash.is_some()) || caught || join_failed) {
            self.select(LogTab::Help, cx);
        }
        let tab_ix = match self.tab {
            LogTab::Help => 0,
            LogTab::Game => 1,
            LogTab::Debug => 2,
            LogTab::Crashes => 3,
        };
        let tabs = TabBar::new("log-tabs")
            .underline()
            .small()
            .selected_index(tab_ix)
            .child(Tab::new().label("Crash help"))
            .child(Tab::new().label(if running { "Game log · live" } else { "Game log" }))
            .child(Tab::new().label("debug.log"))
            .child(Tab::new().label(match self.crashes.len() {
                0 => "Crash reports".to_string(),
                n => format!("Crash reports ({n})"),
            }))
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                let t = [LogTab::Help, LogTab::Game, LogTab::Debug, LogTab::Crashes][*ix];
                this.tab_picked = true;
                this.select(t, cx);
            }));

        let body: AnyElement = if self.tab == LogTab::Help {
            // What Hopper read from the last crash, when it was one of
            // Hopper's games; else the newest crash report as it is.
            let detected = self.store.update(cx, |s, _| {
                // "Revert X" for a mod an older version of it fixes.
                if let Some(i) = s.instance(&root).cloned() {
                    s.want_reverts(&i.root, &i.loader, &i.game_version, &i.addons, true);
                }
                s.crash(&root).cloned()
            });
            let card_source = detected.as_ref().map(|c| c.source.clone());
            let card = detected.map(|c| self.crash_card(&root, &c, cx));
            // No crash, but the game couldn't join a server or open a world.
            let join = self
                .store
                .update(cx, |s, _| s.join_problem(&root).cloned())
                .filter(|_| card.is_none())
                .map(|p| crate::join_help::card(&self.store.clone(), &root, &p, cx));
            let warned = self.warnings_card(&root, cx);
            let crash = self.crash.as_ref().filter(|_| card.is_none()).and_then(|c| {
                let name = c.path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                let report = c.path.clone();
                // The lead: the error line under the description, and the
                // mods the report suspects.
                let error = c
                    .summary
                    .iter()
                    .find(|l| !l.starts_with("Description:") && !l.starts_with("Suspected Mod") && !l.starts_with("  "))
                    .or(c.summary.first())
                    .map(|l| l.trim_start_matches("java.lang.").to_string());
                let suspects: Vec<String> = c
                    .summary
                    .iter()
                    .filter_map(|l| l.strip_prefix("  "))
                    .take(3)
                    .map(str::to_string)
                    .collect();
                let share = self.share_row(&c.path, cx);
                // When, so an old crash doesn't read as today's.
                let secs = c.mtime.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64);
                let when = secs.map(hopper::time::exact);
                let when_full = secs.map(hopper::time::exact_full).unwrap_or_default();
                let body = callout(theme.danger, cx)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().font_semibold().text_color(theme.danger).child("Latest crash"))
                            .child(
                                widgets::caption(
                                    match when {
                                        Some(w) => format!("{w} · {name}"),
                                        None => name,
                                    },
                                    cx,
                                )
                                .id("latest-crash-when")
                                .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(when_full.clone()).build(w, cx)),
                            )
                            .child(div().flex_1())
                            .child(
                                Button::new("show-crash")
                                    .small()
                                    .outline()
                                    .icon(IconName::FileText)
                                    .label("Open report")
                                    .tooltip("Opens the crash report file")
                                    .on_click(move |_, _, _| open_path(&report)),
                            ),
                    )
                    .children(error.map(|e| div().text_sm().font_medium().line_clamp(2).child(e)))
                    .when(!suspects.is_empty(), |d| {
                        d.child(
                            h_flex()
                                .gap_1p5()
                                .text_sm()
                                .child(widgets::caption("The report suspects:", cx))
                                .child(div().font_semibold().child(suspects.join(", "))),
                        )
                    })
                    .child(
                        v_flex()
                            .font_family(theme.mono_font_family.clone())
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .children(c.summary.clone()),
                    )
                    .child(div().pt_1().child(share));
                let key = hopper::settings::notice_key(&root.to_string_lossy(), "latest-crash", &c.path.to_string_lossy());
                widgets::closable_notice(body, key, widgets::Closed::Chip, "Latest crash", theme.danger, &self.store, cx)
            });
            // No crash: say so, rather than leave the space blank.
            let calm = (card.is_none() && crash.is_none() && join.is_none()).then(|| {
                widgets::empty_state(
                    IconName::CircleCheck,
                    "No crashes",
                    Some("This instance hasn't crashed. If it does, what went wrong shows up here.".into()),
                    cx,
                )
            });
            // Older reports, past the one shown above.
            let featured = card_source.or_else(|| self.crash.as_ref().map(|c| c.path.clone()));
            // Follow-up reports fold under the report that explains them.
            let role_of = |p: &PathBuf| self.crashes.iter().position(|c| c == p).and_then(|i| self.crash_roles.get(i));
            let is_folded = |p: &PathBuf| matches!(role_of(p), Some(hopper::crashgroup::Role::FollowUp(Some(_))));
            let folded = self.crashes.iter().filter(|p| Some(*p) != featured.as_ref() && is_folded(p)).count();
            let folds_open = self.folds_open;
            let older: Vec<PathBuf> = self
                .crashes
                .iter()
                .filter(|p| Some(*p) != featured.as_ref())
                .filter(|p| folds_open || !is_folded(p))
                .take(8)
                .cloned()
                .collect();
            let more = self.crashes.len().saturating_sub(older.len() + usize::from(featured.is_some()) + if folds_open { 0 } else { folded });
            let order: Vec<String> = older.iter().map(|p| p.to_string_lossy().into_owned()).collect();
            self.older_sel.retain(&order);
            let many = self.older_sel.many();
            // The picked reports with their follow-ups, each once.
            let groups: Vec<(String, Vec<String>)> = older.iter().map(|p| (p.to_string_lossy().into_owned(), self.report_group(p))).collect();
            let picked_rels = move |sel: &Selection| {
                let mut out: Vec<String> = Vec::new();
                for (k, g) in &groups {
                    if sel.contains(k) {
                        out.extend(g.iter().filter(|r| !out.contains(r)).cloned().collect::<Vec<_>>());
                    }
                }
                out
            };
            let older_bar = many.then(|| {
                let pr = picked_rels.clone();
                let del = cx.listener(move |this, _: &ClickEvent, window, cx| {
                    let rels = pr(&this.older_sel);
                    this.ask_delete_as(rels, true, window, cx)
                });
                let bar = selection::bar(
                    "older-crash-bulk",
                    self.older_sel.len(),
                    Vec::new(),
                    Some(("Delete".into(), Box::new(del))),
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.older_sel.clear();
                        cx.notify();
                    }),
                    cx,
                );
                selection::floating("older-crash-bulk", bar)
            });
            let key_order = order.clone();
            let key_rels = picked_rels.clone();
            let older_list = (!older.is_empty() || folded > 0).then(|| {
                v_flex()
                    .relative()
                    .gap_1()
                    .when(many, |d| d.pb(rems(4.)))
                    .track_focus(&self.older_focus)
                    .on_key_down(cx.listener(move |this, e: &KeyDownEvent, window, cx| {
                        match selection::on_key(&mut this.older_sel, e, &key_order, cx) {
                            KeyAct::None => return,
                            KeyAct::Delete => {
                                let rels = key_rels(&this.older_sel);
                                let many = this.older_sel.many();
                                this.ask_delete_as(rels, many, window, cx)
                            }
                            KeyAct::Changed | KeyAct::Moved(_) => {}
                        }
                        cx.notify();
                    }))
                    .child(div().text_sm().font_semibold().child("Older crash reports"))
                    .children(older.into_iter().enumerate().map(|(i, p)| {
                        let key = p.to_string_lossy().into_owned();
                        let sel = self.older_sel.contains(&key) && (many || self.older_sel.cursor() == Some(key.as_str()));
                        let menu_pick = (many && sel).then(|| picked_rels(&self.older_sel));
                        let click_order = order.clone();
                        let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
                        let name = if is_folded(&p) { format!("Follow-up error · {name}") } else { name };
                        let secs = std::fs::metadata(&p)
                            .and_then(|m| m.modified())
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map_or(0, |d| d.as_secs() as i64);
                        let group = self.report_group(&p);
                        let menu_many = menu_pick.is_some();
                        let (menu_group, btn_group, menu_view) = (menu_pick.unwrap_or_else(|| group.clone()), group, cx.entity());
                        h_flex()
                            .id(("older-crash", i))
                            .gap_2()
                            .px_2()
                            .py_1()
                            .rounded(theme.radius)
                            .clickable(cx)
                            .map(|d| if sel { widgets::row_selected(d, cx) } else { d.hover(|s| widgets::row_hover(s, cx)) })
                            .child(Icon::new(IconName::FileText).small().text_color(theme.muted_foreground))
                            .child(div().text_sm().truncate().child(name))
                            .child(div().flex_1())
                            .child(
                                widgets::caption(log_files::when(secs), cx)
                                    .id(("older-crash-when", i))
                                    .tooltip({
                                        let full = hopper::time::exact_full(secs);
                                        move |w, cx| gpui_kit::component::tooltip::Tooltip::new(full.clone()).build(w, cx)
                                    })
                                    .text_xs(),
                            )
                            .child(
                                Button::new(("older-crash-del", i))
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::Trash)
                                    .tooltip("Delete this report")
                                    .on_click(cx.listener(move |this, _, window, cx| this.ask_delete(btn_group.clone(), window, cx))),
                            )
                            .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                                window.focus(&this.older_focus, cx);
                                // A plain click opens the report, as before;
                                // Ctrl / Shift pick several.
                                if this.older_sel.click(&key, e.modifiers(), &click_order) {
                                    this.open_crash(p.clone(), cx);
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
                    }))
                    .when(more > 0, |c| {
                        c.child(widgets::caption(format!("{more} more in Crash reports"), cx).text_xs().px_2())
                    })
                    .when(folded > 0, |c| {
                        c.child(
                            h_flex()
                                .id("older-fold")
                                .gap_1()
                                .px_2()
                                .py_1()
                                .rounded(theme.radius)
                                .clickable(cx)
                                .hover(|s| s.bg(theme.foreground.opacity(widgets::alpha::HOVER)))
                                .child(Icon::new(if folds_open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall().text_color(theme.muted_foreground))
                                .child(widgets::caption(if folds_open { "Hide follow-up reports".to_string() } else { hopper::crashgroup::fold_label(folded) }, cx).text_xs())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.folds_open = !this.folds_open;
                                    cx.notify();
                                })),
                        )
                    })
                    .children(older_bar)
            });
            div()
                .id("logs-diagnosis")
                .flex_1()
                .min_h_0()
                // Capped like a page of text: on wide windows the cards'
                // buttons stay near their titles.
                .child(
                    v_flex()
                        .max_w(rems(64.))
                        .gap_4()
                        .pb_2()
                        .children(card)
                        .children(join)
                        .children(warned)
                        .children(crash)
                        .children(calm)
                        .child(self.help.clone())
                        .children(older_list),
                )
                .overflow_y_scrollbar()
                .into_any_element()
        } else if self.tab == LogTab::Crashes && self.crashes.is_empty() {
            widgets::empty_state(
                IconName::CircleCheck,
                "No crash reports",
                Some("The game writes one here when it crashes.".into()),
                cx,
            )
            .into_any_element()
        } else {
            self.log_body(&root, window, cx)
        };

        v_flex()
            .size_full()
            .px_6()
            .pb_4()
            .gap_3()
            .child(tabs)
            .child(body)
            .into_any_element()
    }
}

impl LogsView {
    /// The last game crash, as Hopper read it: what happened, and which mods
    /// it names, each with how sure Hopper is and why.
    fn crash_card(&mut self, root: &Path, c: &hopper::crashdetect::Crash, cx: &mut Context<Self>) -> AnyElement {
        use hopper::crashdetect::Certainty;
        let theme = cx.theme().clone();
        let store = self.store.read(cx);
        let inst = store.instance(root);
        // The mod's own name and icon for each suspect.
        let addon = |file: &str| inst.and_then(|i| i.addons.iter().find(|a| a.file_name.eq_ignore_ascii_case(file)));
        let named: Vec<(String, String)> = c
            .suspects
            .iter()
            .filter(|s| !s.mod_id.is_empty())
            .filter_map(|s| Some((s.mod_id.clone(), addon(&s.file_name)?.name.clone())))
            .collect();
        let rows: Vec<AnyElement> = c
            .suspects
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let a = addon(&s.file_name);
                let name = a
                    .map(|a| a.name.clone())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| hopper::crashdetect::jar_name(&root.join("mods"), &s.file_name));
                let icon = a.and_then(|a| store.image(&a.icon)).map(Path::to_path_buf);
                let pid = a.map_or(0, |a| a.project_id);
                // Mod ids in the reason read as names.
                let mut why = s.why.clone();
                for (id, n) in &named {
                    why = why.replace(&format!(" {id},"), &format!(" {n},")).replace(&format!(" {id} "), &format!(" {n} "));
                }
                let (store2, root2) = (self.store.clone(), root.to_path_buf());
                let link = widgets::mod_link(
                            ElementId::Name(format!("crash-suspect:{i}").into()),
                            name,
                            icon.as_deref(),
                            pid,
                            move |project, _, cx| {
                                let root = root2.clone();
                                store2.update(cx, |_, cx| cx.emit(crate::store::StoreEvent::Peek { root, project }));
                            },
                            cx,
                        )
                        .flex_none()
                        .into_any_element();
                let mut below: Vec<AnyElement> = s.needs.iter().enumerate().map(|(k, (id, ver))| {
                                let loader = ["minecraft", "neoforge", "forge", "fabricloader", "quilt_loader", "java"]
                                    .contains(&id.as_str());
                                // Installed since the crash: a jar named like it that provides it.
                                let have = inst.is_some_and(|i| {
                                    let loose = id.replace(['_', '-'], "");
                                    i.addons.iter().filter(|a| a.enabled).any(|a| {
                                        a.file_name.to_lowercase().replace(['_', '-'], "").contains(&loose)
                                            && hopper::deps::provides(&i.addon_path(a)).iter().any(|p| p == id)
                                    })
                                });
                                let label = if ver.is_empty() { format!("Needs {id}") } else { format!("Needs {id} {ver}") };
                                let (root3, id3) = (root.to_path_buf(), id.clone());
                                h_flex()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_family(theme.mono_font_family.clone())
                                            .text_color(theme.warning)
                                            .child(label),
                                    )
                                    .map(|r| {
                                        if have {
                                            r.child(widgets::done_mark(
                                                ElementId::Name(format!("crash-have:{i}:{k}").into()),
                                                "Installed",
                                                true,
                                                cx,
                                            ))
                                        } else if loader {
                                            r.child(widgets::caption("Change it in Settings › Loader", cx).text_xs())
                                        } else {
                                            r.child(
                                                Button::new(ElementId::Name(format!("crash-need:{i}:{k}").into()))
                                                    .xsmall()
                                                    .primary()
                                                    .icon(IconName::Download)
                                                    .label("Install")
                                                    .tooltip("From Modrinth or CurseForge, with what it needs")
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.install_needed(&root3, id3.clone(), cx)
                                                    })),
                                            )
                                        }
                                    })
                                    .into_any_element()
                            }).collect();
                // What Hopper weighed for it, folded under the reason.
                let readable = |t: &str| {
                    let mut t = t.to_string();
                    for (id, n) in &named {
                        t = t.replace(&format!(" {id},"), &format!(" {n},")).replace(&format!(" {id} "), &format!(" {n} "));
                    }
                    t
                };
                let key = s.file_name.clone();
                let open = self.why_open.contains(&key);
                let toggle = cx.listener(move |this, _, _, cx| {
                    if !this.why_open.remove(&key) {
                        this.why_open.insert(key.clone());
                    }
                    cx.notify();
                });
                let store3 = self.store.clone();
                let link_of = |file: &str, key: &str, cx: &App| crash_story::mod_ref(&store3, root, file, key, cx);
                below.extend(crate::crash_view::partner_line(s, &link_of, cx));
                below.extend(crate::crash_view::steps_list(s, &readable, cx));
                below.extend(crate::crash_view::why_fold(ElementId::Name(format!("crash-why:{i}").into()), s, open, &readable, toggle, cx));
                crate::crash_view::suspect_row(link, s, why, below, cx).into_any_element()
            })
            .collect();
        let note = crate::crash_view::note(c);
        // The lead: what broke, and the mods most likely behind it.
        let likely: Vec<String> = c
            .suspects
            .iter()
            .filter(|s| s.certainty <= Certainty::Likely)
            .take(3)
            .map(|s| {
                addon(&s.file_name)
                    .map(|a| a.name.clone())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| hopper::crashdetect::jar_name(&root.join("mods"), &s.file_name))
            })
            .collect();
        let has_report = c.source.extension().is_some_and(|e| e == "txt") && c.source.starts_with(root.join("crash-reports"));
        // Caught while this game runs: it's sitting on its error screen.
        let still_open = self.store.read(cx).running(root).is_some_and(|r| c.when >= r.started as i64);
        let source = c.source.clone();
        let root_d = root.to_path_buf();
        // A mod failed while loading and the crash followed: tell that story
        // (with its own fix) instead of the list.
        let story = c
            .stopped
            .as_ref()
            .filter(|st| c.suspects.iter().any(|s| s.file_name == st.file_name))
            .map(|st| self.crash_story(root, c, st, cx));
        let listed = story.is_none();
        let first = crash_switch::settings_first(c);
        callout(theme.danger, cx)
            .child(
                h_flex()
                    .gap_2()
                    .child(Icon::new(IconName::TriangleAlert).small().text_color(theme.danger))
                    .child(div().font_semibold().text_color(theme.danger).child(if still_open {
                        "The game crashed and is showing its error screen"
                    } else {
                        "The game crashed"
                    }))
                    .child(widgets::caption(hopper::time::relative_secs(c.when), cx))
                    .child(div().flex_1())
                    .when(still_open, |r| {
                        let root_s = root.to_path_buf();
                        r.child(
                            Button::new("crash-close-game")
                                .small()
                                .danger()
                                .icon(IconName::X)
                                .label("Close game")
                                .tooltip("It can't go further than its error screen")
                                .on_click(cx.listener(move |this, _, _, cx| this.store.read(cx).stop(&root_s))),
                        )
                    })
                    .when(has_report, |r| {
                        let p = source.clone();
                        r.child(
                            Button::new("crash-open-report")
                                .small()
                                .outline()
                                .icon(IconName::FileText)
                                .label("Open report")
                                .tooltip("Opens the crash report file")
                                .on_click(move |_, _, _| open_path(&p)),
                        )
                    })
                    .when(!has_report && source.is_file(), |r| {
                        let p = source.clone();
                        r.child(
                            Button::new("crash-open-file")
                                .small()
                                .outline()
                                .label("Open file")
                                .on_click(move |_, _, _| open_path(&p)),
                        )
                    })
                    .child(
                        Button::new("crash-dismiss")
                            .small()
                            .ghost()
                            .icon(IconName::X)
                            .tooltip("Dismiss")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.store.update(cx, |s, cx| {
                                    s.dismiss_crash(&root_d);
                                    cx.notify();
                                });
                            })),
                    ),
            )
            .when(listed && !c.headline.is_empty(), |d| {
                d.child(div().text_sm().font_medium().line_clamp(2).child(c.headline.clone()))
            })
            .when(listed && !likely.is_empty(), |d| {
                d.child(
                    h_flex()
                        .gap_1p5()
                        .text_sm()
                        .child(widgets::caption(if likely.len() == 1 { "Likely cause:" } else { "Likely causes:" }, cx))
                        .child(div().font_semibold().child(likely.join(", "))),
                )
            })
            .when(listed && !rows.is_empty(), |d| {
                d.child(widgets::caption("Mods in this crash, most likely first:", cx).text_xs())
                    .child(v_flex().children(rows))
            })
            .children(story)
            .when_some(note, |d, n| d.child(div().text_sm().child(n)))
            // A setting that keeps the mod comes first when there is one.
            .when(listed && first, |d| d.children(self.config_switches(root, c, None, cx)).children(self.or_actions(root, c, cx)))
            .when(listed && !first, |d| d.children(self.crash_actions(root, c, cx)))
            .when(listed && !first, |d| d.children(self.config_switches(root, c, None, cx)))
            .when(source.is_file(), |d| d.child(div().pt_1().child(self.share_row(&source, cx))))
            .into_any_element()
    }

    /// Mods whose mixins failed in the current or last game: no crash (yet),
    /// but worth knowing when something misbehaves.
    fn warnings_card(&mut self, root: &Path, cx: &mut Context<Self>) -> Option<AnyElement> {
        let store = self.store.read(cx);
        let list = store.warnings(root).to_vec();
        if list.is_empty() {
            return None;
        }
        let theme = cx.theme().clone();
        let inst = store.instance(root);
        let playing = store.running(root).is_some();
        let rows: Vec<AnyElement> = list
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let a = inst.and_then(|i| i.addons.iter().find(|a| a.file_name.eq_ignore_ascii_case(s.file_name.trim_end_matches(".disabled"))));
                let name = a.map(|a| a.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| hopper::crashdetect::jar_name(&root.join("mods"), &s.file_name));
                let icon = a.and_then(|a| store.image(&a.icon)).map(Path::to_path_buf);
                let pid = a.map_or(0, |a| a.project_id);
                let (store2, root2) = (self.store.clone(), root.to_path_buf());
                h_flex()
                    .gap_3()
                    .py_0p5()
                    .child(
                        widgets::mod_link(
                            ElementId::Name(format!("warned:{i}").into()),
                            name,
                            icon.as_deref(),
                            pid,
                            move |project, _, cx| {
                                let root = root2.clone();
                                store2.update(cx, |_, cx| cx.emit(crate::store::StoreEvent::Peek { root, project }));
                            },
                            cx,
                        )
                        .w(rems(16.))
                        .flex_none()
                        .text_sm(),
                    )
                    .child(widgets::caption(s.why.clone(), cx).text_xs().flex_1().min_w_0().whitespace_normal())
                    .into_any_element()
            })
            .collect();
        let title = if playing { "While you play" } else { "While you last played" };
        let key = hopper::settings::notice_key(
            &root.to_string_lossy(),
            "mixin-warnings",
            &list.iter().map(|s| format!("{}:{}", s.file_name, s.why)).collect::<Vec<_>>().join(";"),
        );
        let body = callout(theme.warning, cx)
            .child(
                h_flex()
                    .gap_2()
                    .child(Icon::new(IconName::TriangleAlert).small().text_color(theme.warning))
                    .child(div().font_semibold().text_color(theme.warning).child(title)),
            )
            .child(v_flex().children(rows));
        widgets::closable_notice(body, key, widgets::Closed::Chip, title, theme.warning, &self.store, cx)
    }

    /// Turn the likely mods off (then play), or back on.
    fn crash_actions(&mut self, root: &Path, c: &hopper::crashdetect::Crash, cx: &mut Context<Self>) -> Option<AnyElement> {
        use hopper::crashdetect::Certainty;
        let store = self.store.read(cx);
        let inst = store.instance(root)?;
        let on = |f: &str| {
            inst.addons
                .iter()
                .any(|a| a.enabled && a.file_name.eq_ignore_ascii_case(f.trim_end_matches(".disabled")))
        };
        let turn_off_mods: Vec<&hopper::crashdetect::Suspect> =
            hopper::crashdetect::to_turn_off(c).into_iter().filter(|s| on(&s.file_name)).collect();
        let to_turn_off: Vec<&str> = turn_off_mods.iter().map(|s| s.file_name.as_str()).collect();
        let can_turn_off = to_turn_off.len();
        // One mod: its own name on the button.
        let only = (can_turn_off == 1)
            .then(|| {
                let f = to_turn_off[0].trim_end_matches(".disabled");
                inst.addons.iter().find(|a| a.file_name.eq_ignore_ascii_case(f)).map(|a| a.name.clone()).filter(|n| !n.is_empty())
            })
            .flatten();
        let turned = c.turned_off.len();
        let playing = store.running(root).is_some();
        // A mod that needs a newer mod or loader: back to a version that works.
        let reverts: Vec<crate::revert_ui::Offer> = c
            .suspects
            .iter()
            .filter(|s| s.certainty <= Certainty::Likely)
            .filter_map(|s| store.revert_offer(root, s.file_name.trim_end_matches(".disabled")))
            .collect();
        if can_turn_off == 0 && turned == 0 && reverts.is_empty() {
            return None;
        }
        let (r1, r2, r3) = (root.to_path_buf(), root.to_path_buf(), root.to_path_buf());
        let all_files: Vec<String> = to_turn_off.iter().map(|f| f.to_string()).collect();
        // The list "Turn off…" opened: each mod ticked, with why; its confirm
        // turns off only the ticked ones.
        let picker = self.turn_off_pick.as_ref().filter(|_| can_turn_off > 0).map(|picked| {
            let named: Vec<(String, String)> = c
                .suspects
                .iter()
                .filter(|s| !s.mod_id.is_empty())
                .map(|s| (s.mod_id.clone(), hopper::crashdetect::jar_name(&root.join("mods"), &s.file_name)))
                .collect();
            let readable = move |t: &str| {
                named.iter().fold(t.to_string(), |w, (id, n)| w.replace(&format!(" {id},"), &format!(" {n},")).replace(&format!(" {id} "), &format!(" {n} ")))
            };
            let store3 = self.store.clone();
            let root3 = root.to_path_buf();
            let link_of = move |file: &str, key: &str, cx: &App| crash_story::mod_ref(&store3, &root3, file, key, cx);
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
            let confirm = cx.listener(move |this, _, window, cx| {
                let Some(picked) = this.turn_off_pick.take() else { return };
                let files: Vec<String> = picked.into_iter().collect();
                let r = this.store.update(cx, |s, cx| {
                    let r = s.crash_turn_off(&r1, &files);
                    cx.notify();
                    r
                });
                if let Err(e) = r {
                    window.push_notification(gpui_kit::component::notification::Notification::warning(e), cx);
                }
            });
            let cancel = cx.listener(|this, _, _, cx| {
                this.turn_off_pick = None;
                cx.notify();
            });
            crate::crash_view::turn_off_picker("crash", &turn_off_mods, picked, &link_of, &readable, toggle, confirm, cancel, playing, "instance", cx)
        });
        let revert_buttons: Vec<AnyElement> = reverts
            .into_iter()
            .enumerate()
            .map(|(k, o)| {
                let (label, tip) = o.label();
                let root = root.to_path_buf();
                Button::new(ElementId::Name(format!("crash-revert:{k}").into()))
                    .small()
                    .outline()
                    .icon(IconName::Undo2)
                    .label(label)
                    .tooltip(tip)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let store = this.store.clone();
                        crate::revert_ui::revert_instance(&store, &root, &o, window, cx);
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();
        Some(
            h_flex()
                .gap_2()
                .pt_1()
                .flex_wrap()
                .children(revert_buttons)
                .when(can_turn_off > 0, |r| {
                    r.child(
                        Button::new("crash-turn-off")
                            .small()
                            .outline()
                            .icon(IconName::PowerOff)
                            .label(if let Some(n) = &only {
                                format!("Turn off {n}…")
                            } else if can_turn_off == 1 {
                                "Turn off 1 mod…".to_string()
                            } else {
                                format!("Turn off {can_turn_off} mods…")
                            })
                            .tooltip("Shows the mods first. You can turn them back on here.")
                            .disabled(self.turn_off_pick.is_some())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.turn_off_pick = Some(all_files.iter().cloned().collect());
                                cx.notify();
                            })),
                    )
                })
                .when(turned > 0, |r| {
                    r.child(
                        Button::new("crash-play")
                            .small()
                            .success()
                            .icon(IconName::Play)
                            .label(if turned == 1 { "Play without it" } else { "Play without them" })
                            .disabled(playing)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let native = this.store.read(cx).settings().launch_native;
                                if native && !hopper::msauth::has_saved_login() {
                                    crate::dialogs::open_sign_in(this.store.clone(), window, cx);
                                    return;
                                }
                                if let Err(e) = this.store.update(cx, |s, _| s.play(&r2)) {
                                    window.push_notification(gpui_kit::component::notification::Notification::warning(e), cx);
                                }
                            })),
                    )
                    .child(
                        Button::new("crash-turn-on")
                            .small()
                            .ghost()
                            .icon(IconName::Undo2)
                            .label(if turned == 1 { "Turn it back on".to_string() } else { format!("Turn {turned} back on") })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let r = this.store.update(cx, |s, cx| {
                                    let r = s.crash_turn_back_on(&r3);
                                    cx.notify();
                                    r
                                });
                                if let Err(e) = r {
                                    window.push_notification(gpui_kit::component::notification::Notification::warning(e), cx);
                                }
                            })),
                    )
                })
                .into_any_element(),
        )
        .map(|row| v_flex().gap_2().child(row).children(picker).into_any_element())
    }

    /// Installs a mod the crash says is missing (as the Mods tab's banner
    /// does), as a task.
    fn install_needed(&mut self, root: &Path, id: String, cx: &mut Context<Self>) {
        let store = self.store.read(cx);
        let Some(mut inst) = store.instance(root).cloned() else { return };
        store.spawn_instance_job(root, format!("Installing {id}"), move |job| {
            let before = hopper::modpack::pending_urls_at(&inst.root);
            let added = hopper::install::install_mod_id(&mut inst, &id, job)?;
            job.note(format!("Installed {}", added.join(", ")));
            Ok(crate::store::Msg::installed(inst.root.clone(), &before, None))
        });
    }

    /// A log tab: one toolbar, then the log filling the rest.
    fn log_body(&mut self, root: &Path, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(log) = self.log.as_ref() else {
            return div().into_any_element();
        };
        let (path, loading, total) = (log.path.clone(), log.loading, log.lines.len());
        let n = self.line_count();
        let filtered = self.visible.is_some();
        let share = self.share_row(&path, cx);
        // The folder of the file shown: logs/, or crash-reports/ on that tab.
        let shown_dir = path.parent().map(Path::to_path_buf).unwrap_or_else(|| root.join("logs"));
        // Older reports: a picker in the Crash reports tab.
        let crash_pick = (self.tab == LogTab::Crashes && self.crashes.len() > 1).then(|| {
            let items = self
                .crashes
                .iter()
                .map(|p| (p.file_name().unwrap_or_default().to_string_lossy().into_owned(), *p == path))
                .collect();
            let (list, view) = (self.crashes.clone(), cx.entity());
            widgets::picker(
                "crash-pick",
                path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
                items,
                window.rem_size(),
                std::rc::Rc::new(move |ix: usize, _: &mut Window, cx: &mut App| {
                    if let Some(p) = list.get(ix).cloned() {
                        view.update(cx, |this, cx| this.open_log(p, false, cx));
                    }
                }),
            )
        });
        let toolbar = h_flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .children(crash_pick)
            .child(div().w(rems(16.)).child(widgets::search_input(&self.query).small().cleanable(true)))
            .child(
                Checkbox::new("errors-only")
                    .label("Errors and warnings only")
                    .checked(self.errors_only)
                    .on_change(cx.listener(|this, v: &bool, _, cx| {
                        this.errors_only = *v;
                        this.scroll_to_end();
                        cx.notify();
                    })),
            )
            .when(filtered, |r| {
                r.child(widgets::caption(format!("{n} of {total} lines"), cx).text_xs())
            })
            .when(loading, |r| r.child(Spinner::new().xsmall()))
            .child(div().flex_1())
            .child(share)
            .child(self.delete_buttons(&path, cx))
            .child(
                Button::new("logs-folder")
                    .small()
                    .ghost()
                    .icon(IconName::FolderOpen)
                    .tooltip(format!("Open its folder ({})", shown_dir.display()))
                    .on_click(move |_, _, _| open_path(&shown_dir)),
            );
        v_flex()
            .flex_1()
            .min_h_0()
            .gap_2()
            .child(toolbar)
            .child(crate::line_select::view(self, "log-lines", None, window, cx))
            .into_any_element()
    }
}

impl Lines for LogsView {
    fn line_select(&self) -> &LineSelect {
        &self.lines
    }

    fn line_select_mut(&mut self) -> &mut LineSelect {
        &mut self.lines
    }

    fn row_count(&self) -> usize {
        self.line_count()
    }

    /// The log line (index into `lines`).
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

#[cfg(test)]
mod tests {
    use super::{Chunk, KEEP_LINES, LogFile, OPEN_BYTES, read_new, read_tail};
    use std::path::PathBuf;
    use std::io::Write;

    #[test]
    fn follows_only_whole_appended_lines() {
        let dir = std::env::temp_dir().join(format!("hopper-logs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("latest.log");
        std::fs::write(&path, "one\ntwo\nthr").unwrap();
        let (text, to) = read_tail(&path, OPEN_BYTES, true).unwrap();
        assert_eq!(text, "one\ntwo\n");
        assert_eq!(to, 8);
        assert!(matches!(read_new(&path, to), Chunk::Same));
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"ee\nfour\n").unwrap();
        drop(f);
        match read_new(&path, to) {
            Chunk::Append(t, end) => {
                assert_eq!(t, "three\nfour\n");
                assert_eq!(end, std::fs::metadata(&path).unwrap().len());
            }
            _ => panic!("expected an append"),
        }
        // A new run truncates the file: read it again from the start.
        std::fs::write(&path, "new\n").unwrap();
        assert!(matches!(read_new(&path, 100), Chunk::Reset(t, 4) if t == "new\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn problems_follow_trimmed_lines() {
        let mut l = LogFile {
            path: PathBuf::new(),
            lines: Vec::new(),
            problems: Vec::new(),
            read_to: 0,
            loading: false,
        };
        l.push("ok\n[ERROR] bad\nok\n");
        assert_eq!(l.problems, vec![1]);
        l.push(&"x\n".repeat(KEEP_LINES));
        assert!(l.lines.len() <= KEEP_LINES);
        assert!(l.problems.iter().all(|i| *i < l.lines.len()));
    }
}
