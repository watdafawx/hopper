//! An instance's Logs & crashes tab: its crash reports, each read the way
//! the crash card reads the last one, the game's live log and debug.log,
//! each with search and sharing on mclo.gs, and the older logs. The tabs,
//! the list and the card's frame are the same as a server's
//! (crash_list.rs); the instance adds its own notices above the list
//! (Crash Assistant, a failed join, mixins that failed while playing).

use crate::crash_help::{CrashHelp, callout};
use crate::crash_list::{self, CrashList, Pane};
use crate::line_select::{LineSelect, Lines};
use crate::store::{Store, open_path};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::logs::{Level, is_problem, level};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

mod crash_story;
pub(crate) use crash_story::mod_ref;
pub(crate) mod crash_switch;

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

pub struct LogsView {
    store: Entity<Store>,
    root: Option<PathBuf>,
    help: Entity<CrashHelp>,
    /// The file shown and its lines.
    log: Option<LogFile>,
    errors_only: bool,
    /// The crash reports, the older logs and sharing (shared with a
    /// server's tab).
    crashes: CrashList,
    /// Reports read so far (`None` while reading; inside, `None` when
    /// Hopper couldn't tell what went wrong).
    cards: HashMap<String, Option<Option<hopper::crashdetect::Crash>>>,
    /// The newest report as last told to the store (it rereads the saved
    /// crash when a newer report came).
    newest_told: Option<String>,
    /// The crash card's mods (jars) whose "Why?" list is open.
    why_open: std::collections::HashSet<String>,
    /// "Turn off…" asked: the jars ticked in its list (nothing is turned
    /// off until its confirm).
    turn_off_pick: Option<std::collections::HashSet<String>>,
    pane: Pane,
    /// The user picked a tab for this instance; it stays.
    tab_picked: bool,
    query: Entity<InputState>,
    /// Lines the viewer shows when filtered (errors only or a search);
    /// `None` shows them all.
    visible: Option<Vec<usize>>,
    /// The log's lines on screen, with text picking.
    lines: LineSelect,
    _follow: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl crash_list::Host for LogsView {
    fn list(&self) -> &CrashList {
        &self.crashes
    }

    fn list_mut(&mut self) -> &mut CrashList {
        &mut self.crashes
    }

    fn root(&self) -> Option<PathBuf> {
        self.root.clone()
    }

    fn pick(&mut self, rel: String, cx: &mut Context<Self>) {
        self.pick_report(rel, cx);
    }

    fn running(&self, cx: &App) -> bool {
        self.root.as_ref().is_some_and(|r| self.store.read(cx).running(r).is_some())
    }

    fn after_delete(&mut self, rels: &[String], cx: &mut Context<Self>) {
        for r in rels {
            self.cards.remove(r);
        }
        self.crashes.forget(rels);
        if self.pane == Pane::Older {
            crash_list::load_old(self, cx);
        }
        crash_list::watch(self, cx);
        cx.notify();
    }

    fn player(&self, cx: &App) -> Option<String> {
        let s = self.store.read(cx).settings();
        s.signed_in_as.clone().or_else(|| hopper::account::active(Path::new(&s.launcher_dir)).map(|a| a.name))
    }
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
            crashes: CrashList::new(false, cx),
            cards: HashMap::new(),
            newest_told: None,
            why_open: Default::default(),
            turn_off_pick: None,
            pane: Pane::Latest,
            tab_picked: false,
            query,
            visible: None,
            lines: LineSelect::new(cx),
            _follow: None,
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
            self.crashes.reset();
            self.cards.clear();
            self.newest_told = None;
            self.why_open.clear();
            self.turn_off_pick = None;
            self.tab_picked = false;
        }
        self.root = Some(root.clone());
        if new_root {
            // Opens on the game's log; after a crash, render switches to
            // the crash reports.
            self.select(Pane::Latest, cx);
        }
        crash_list::watch(self, cx);
        self.help.update(cx, |h, cx| h.show(root, cx));
        cx.notify();
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
                    this.lines.scroll_to_start();
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
                    let at_end = this.lines.at_end();
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
        self.lines.scroll_to_end();
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

    /// Shows pane `pane`, opening its log.
    fn select(&mut self, pane: Pane, cx: &mut Context<Self>) {
        self.pane = pane;
        let Some(root) = self.root.clone() else { return };
        if pane == Pane::Older {
            crash_list::load_old(self, cx);
        }
        match pane.log() {
            Some(rel) => {
                let path = root.join(rel);
                if self.log.as_ref().map(|l| &l.path) != Some(&path) {
                    self.open_log(path, true, cx);
                }
            }
            None => {
                self.log = None;
                self._follow = None;
                self.lines.clear();
            }
        }
        cx.notify();
    }

    /// Opens the crash reports on `rel` (a crash report or a stuck start's
    /// dump, under the instance folder): where the stuck start's strip's
    /// "See why" lands.
    pub fn open_report(&mut self, rel: String, cx: &mut Context<Self>) {
        self.tab_picked = true;
        self.select(Pane::Crashes, cx);
        self.pick_report(rel, cx);
    }

    /// Shows report `rel` beside the list, reading it off the UI thread the
    /// first time (the crash Hopper saw the game end with is read already).
    fn pick_report(&mut self, rel: String, cx: &mut Context<Self>) {
        if !self.crashes.sel.contains(&rel) {
            self.crashes.sel.set_one(rel.clone());
        }
        self.crashes.picked = Some(rel.clone());
        self.turn_off_pick = None;
        cx.notify();
        if self.cards.contains_key(&rel) {
            return;
        }
        let Some(root) = self.root.clone() else { return };
        self.cards.insert(rel.clone(), None);
        cx.spawn(async move |this, cx| {
            let (r, p) = (root.clone(), root.join(&rel));
            let c = cx.background_spawn(async move { hopper::crashdetect::game_report(&r, &p) }).await;
            this.update(cx, |this, cx| {
                if this.root.as_ref() == Some(&root) {
                    this.cards.insert(rel, Some(c));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

}

impl Render for LogsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(root) = self.root.clone() else {
            return div().into_any_element();
        };
        self.visible = self.filter(cx);
        let running = self.store.read(cx).running(&root).is_some();
        // A newer report than the crash Hopper saved: the store reads it.
        if let Some(Ok(l)) = &self.crashes.reports
            && let Some(first) = l.first().map(|r| r.rel.clone())
            && self.newest_told.as_ref() != Some(&first)
        {
            self.store.update(cx, |s, _| s.newest_crash_report(&root, &root.join(&first)));
            self.newest_told = Some(first);
        }
        // After a crash the tab opens on the crash reports. So does a crash
        // Hopper caught while the game is still open (on its error screen),
        // and a game that couldn't join a server or open a world.
        let caught = self.store.update(cx, |s, _| {
            let started = s.running(&root).map_or(i64::MAX, |r| r.started as i64);
            s.crash(&root).is_some_and(|c| c.when >= started)
        });
        let join_failed = !running && self.store.update(cx, |s, _| s.join_problem(&root).is_some());
        let crashed = !running && self.crashes.count() > 0 && self.store.update(cx, |s, _| s.crash(&root).is_some());
        if !self.tab_picked && self.pane == Pane::Latest && (crashed || caught || join_failed) {
            self.select(Pane::Crashes, cx);
        }
        let tabs = crash_list::tabs("log-tabs", self.pane, self.crashes.count(), running, cx, |this, p, cx| {
            this.tab_picked = true;
            this.select(p, cx);
        });
        let body = match self.pane {
            Pane::Crashes => self.crashes_pane(&root, cx),
            Pane::Older => crash_list::older_pane(self, "The game keeps its last start in latest.log. Older ones show up here.", cx),
            _ => self.log_body(&root, window, cx),
        };
        v_flex().size_full().px_6().pb_4().gap_3().child(tabs).child(body).into_any_element()
    }
}

impl LogsView {
    /// The crash reports: the instance's notices on top, then the list with
    /// the picked report's card beside it.
    fn crashes_pane(&mut self, root: &Path, cx: &mut Context<Self>) -> AnyElement {
        // What Hopper read from the last crash, when it was one of Hopper's
        // games ("Revert X" for a mod an older version of it fixes).
        let detected = self.store.update(cx, |s, _| {
            if let Some(i) = s.instance(root).cloned() {
                s.want_reverts(&i.root, &i.loader, &i.game_version, &i.addons, true);
            }
            s.crash(root).cloned()
        });
        let rel_of = |p: &Path| p.strip_prefix(root).ok().map(|r| r.to_string_lossy().replace('\\', "/"));
        // A crash with no report of its own (Java's own crash file, or the
        // log) has no row: its card sits above the list.
        let unlisted = detected.clone().filter(|c| rel_of(&c.source).is_none_or(|r| self.crashes.report(&r).is_none()));
        let mut notices: Vec<AnyElement> = Vec::new();
        if let Some(c) = unlisted.filter(|_| self.crashes.reports.is_some()) {
            notices.push(self.crash_card(root, &c, true, cx));
        }
        // No crash, but the game couldn't join a server or open a world.
        if detected.is_none()
            && let Some(p) = self.store.update(cx, |s, _| s.join_problem(root).cloned())
        {
            notices.push(crate::join_help::card(&self.store.clone(), root, &p, cx));
        }
        notices.extend(self.warnings_card(root, cx));
        let theme = cx.theme().clone();
        let card = match self.crashes.picked.clone() {
            None => div().into_any_element(),
            Some(rel) => {
                let path = root.join(&rel);
                let saved = detected.as_ref().filter(|c| c.source == path).cloned();
                match (saved, self.cards.get(&rel).cloned()) {
                    (Some(c), _) => self.crash_card(root, &c, true, cx),
                    (None, Some(Some(Some(c)))) => self.crash_card(root, &c, false, cx),
                    (None, Some(Some(None))) => {
                        let when = self.crashes.report(&rel).map_or(0, |r| r.modified);
                        let times = self.crashes.times(&rel);
                        callout(theme.danger, cx)
                            .child(crash_list::card_head("The game crashed", when, times, vec![crash_list::open_button(path)], cx))
                            .child(widgets::caption("Hopper couldn't tell what went wrong. The report has the details.", cx))
                            .child(div().pt_1().child(crash_list::crash_share_row(self, &rel, "", cx)))
                            .into_any_element()
                    }
                    _ => {
                        let when = self.crashes.report(&rel).map_or(0, |r| r.modified);
                        callout(theme.danger, cx)
                            .child(crash_list::card_head("The game crashed", when, None, vec![crash_list::open_button(path)], cx))
                            .child(h_flex().gap_2().child(Spinner::new().small()).child(widgets::caption("Reading the report", cx)))
                            .into_any_element()
                    }
                }
            }
        };
        let split = crash_list::split(self, card, "This instance hasn't crashed. If it does, the report and what Hopper makes of it show up here.", cx);
        v_flex()
            .flex_1()
            .min_h_0()
            .gap_3()
            // No cap: the notices span the page like the cards above and below.
            .when(!notices.is_empty(), |d| d.child(v_flex().w_full().gap_3().children(notices)))
            .child(self.help.clone())
            .child(split)
            .into_any_element()
    }

    /// A game crash, as Hopper read it: what happened, and which mods it
    /// names, each with how sure Hopper is and why. `latest`: the crash
    /// Hopper saw the game end with (it can be dismissed, and the game can
    /// still be on its error screen).
    fn crash_card(&mut self, root: &Path, c: &hopper::crashdetect::Crash, latest: bool, cx: &mut Context<Self>) -> AnyElement {
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
        let still_open = latest && self.store.read(cx).running(root).is_some_and(|r| c.when >= r.started as i64);
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
        let rel = source.strip_prefix(root).ok().map(|r| r.to_string_lossy().replace('\\', "/"));
        let times = rel.as_deref().and_then(|r| self.crashes.times(r));
        let mut buttons: Vec<AnyElement> = Vec::new();
        if still_open {
            let root_s = root.to_path_buf();
            buttons.push(
                Button::new("crash-close-game")
                    .small()
                    .danger()
                    .icon(IconName::X)
                    .label("Close game")
                    .tooltip("It can't go further than its error screen")
                    .on_click(cx.listener(move |this, _, _, cx| this.store.read(cx).stop(&root_s)))
                    .into_any_element(),
            );
        }
        if has_report {
            buttons.push(crash_list::open_button(source.clone()));
        } else if source.is_file() {
            let p = source.clone();
            buttons.push(Button::new("crash-open-file").small().outline().label("Open file").on_click(move |_, _, _| open_path(&p)).into_any_element());
        }
        if latest {
            buttons.push(
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
                    }))
                    .into_any_element(),
            );
        }
        let title = if c.stuck {
            "The game got stuck starting"
        } else if still_open {
            "The game crashed and is showing its error screen"
        } else {
            "The game crashed"
        };
        let share = rel.filter(|_| source.is_file()).map(|r| crash_list::crash_share_row(self, &r, &c.headline, cx));
        let follow = crash_list::follow_up_note(c, cx);
        callout(theme.danger, cx)
            .child(crash_list::card_head(title, c.when, times, buttons, cx))
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
            .children(follow)
            .when_some(share, |d, s| d.child(div().pt_1().child(s)))
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
        let rel = path.strip_prefix(root).map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_default();
        let share = crash_list::share_row(self, &rel, cx);
        // The folder of the file shown.
        let shown_dir = path.parent().map(Path::to_path_buf).unwrap_or_else(|| root.join("logs"));
        let toolbar = h_flex()
            .flex_wrap()
            .items_center()
            .gap_2()
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
