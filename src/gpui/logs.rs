//! An instance's Logs & crashes tab: tabs for crash help, the game's
//! live log, debug.log and crash reports, each log with search and sharing
//! on mclo.gs.

use crate::crash_help::{CrashHelp, callout};
use crate::store::{Store, open_path};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::logs::{Level, crash_reports, crash_summary, is_problem, level, tail};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

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
    /// Every crash report, newest first.
    crashes: Vec<PathBuf>,
    tab: LogTab,
    /// The user picked a tab for this instance; it stays.
    tab_picked: bool,
    query: Entity<InputState>,
    /// Lines the viewer shows when filtered (errors only or a search);
    /// `None` shows them all.
    visible: Option<Vec<usize>>,
    scroll: UniformListScrollHandle,
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
            crashes: Vec::new(),
            tab: LogTab::Game,
            tab_picked: false,
            query,
            visible: None,
            scroll: UniformListScrollHandle::new(),
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
            self._follow = None;
            self.crash = None;
            self.crashes.clear();
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
                let Ok(known) = this.update(cx, |this, _| {
                    this.crash.as_ref().map(|c| (c.path.clone(), c.mtime))
                }) else {
                    return;
                };
                let r = root.clone();
                let found = cx
                    .background_spawn(async move {
                        let all = crash_reports(&r);
                        let latest = all.first()?.clone();
                        let mtime = std::fs::metadata(&latest).and_then(|m| m.modified()).ok();
                        if known.as_ref() == Some(&(latest.clone(), mtime)) {
                            return Some(None);
                        }
                        let summary = crash_summary(&tail(&latest, 200_000));
                        Some(Some((
                            Crash {
                                path: latest,
                                mtime,
                                summary,
                            },
                            all,
                        )))
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
                        }
                        Some(Some((c, all))) => {
                            this.crash = Some(c);
                            this.crashes = all;
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
                    this.scroll.scroll_to_item(0, ScrollStrategy::Top);
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
                    let at_end = this.scroll.is_scrolled_to_end().unwrap_or(true);
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
        let n = self.line_count();
        if n > 0 {
            self.scroll.scroll_to_item(n - 1, ScrollStrategy::Bottom);
        }
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
            LogTab::Crashes => self.crashes.first().map(|p| (p.clone(), false)),
        };
        if let Some((path, follow)) = want
            && self.log.as_ref().map(|l| &l.path) != Some(&path)
        {
            self.open_log(path, follow, cx);
        }
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

    fn render_lines(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        range
            .filter_map(|ix| self.line(ix))
            .map(|l| {
                div()
                    .w_full()
                    .h_5()
                    .whitespace_nowrap()
                    .text_color(match level(l) {
                        Level::Error => theme.danger,
                        Level::Warn => theme.warning,
                        Level::Plain => theme.foreground,
                    })
                    .child(l.replace('\t', "    "))
                    .into_any_element()
            })
            .collect()
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
        if !self.tab_picked && !running && self.tab == LogTab::Game && self.crash.is_some() {
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
            let crash = self.crash.as_ref().map(|c| {
                let name = c.path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                // When, so an old crash doesn't read as today's.
                let when = c
                    .mtime
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| hopper::time::relative_secs(d.as_secs() as i64));
                callout(theme.danger, cx)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().font_semibold().text_color(theme.danger).child("Latest crash"))
                            .child(widgets::caption(
                                match when {
                                    Some(w) => format!("{w} · {name}"),
                                    None => name,
                                },
                                cx,
                            ))
                            .child(div().flex_1())
                            .child(
                                Button::new("show-crash")
                                    .small()
                                    .outline()
                                    .label("Open")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.tab_picked = true;
                                        this.select(LogTab::Crashes, cx)
                                    })),
                            ),
                    )
                    .child(
                        v_flex()
                            .font_family(theme.mono_font_family.clone())
                            .text_xs()
                            .children(c.summary.clone()),
                    )
            });
            div()
                .id("logs-diagnosis")
                .flex_1()
                .min_h_0()
                .child(v_flex().gap_4().pb_2().children(crash).child(self.help.clone()))
                .overflow_y_scrollbar()
                .into_any_element()
        } else if self.tab == LogTab::Crashes && self.crashes.is_empty() {
            widgets::caption("No crash reports.", cx).into_any_element()
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
    /// A log tab: one toolbar, then the log filling the rest.
    fn log_body(&mut self, root: &Path, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(log) = self.log.as_ref() else {
            return div().into_any_element();
        };
        let (path, loading, total) = (log.path.clone(), log.loading, log.lines.len());
        let n = self.line_count();
        let filtered = self.visible.is_some();
        let share = self.share_row(&path, cx);
        let logs_dir = root.join("logs");
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
            .child(div().w(rems(16.)).child(Input::new(&self.query).small().cleanable(true)))
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
            .child(
                Button::new("logs-folder")
                    .small()
                    .ghost()
                    .icon(IconName::FolderOpen)
                    .tooltip(format!("Open the logs folder ({})", path.display()))
                    .on_click(move |_, _, _| open_path(&logs_dir)),
            );
        v_flex()
            .flex_1()
            .min_h_0()
            .gap_2()
            .child(toolbar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .rounded(theme.radius_lg)
                    .bg(theme.muted)
                    .p_3()
                    .font_family(theme.mono_font_family.clone())
                    .text_xs()
                    .child(
                        uniform_list(
                            "log-lines",
                            n,
                            cx.processor(|this, r: Range<usize>, _, cx| this.render_lines(r, cx)),
                        )
                        .track_scroll(&self.scroll)
                        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                        .size_full(),
                    )
                    .vertical_scrollbar(&self.scroll),
            )
            .into_any_element()
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
