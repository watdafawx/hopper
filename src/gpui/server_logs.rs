//! A server's Logs & crashes tab (Servers page): its crash reports, each
//! read the way the crash card reads the last one, and latest.log and
//! debug.log followed while the server runs, each with search, an
//! errors-only filter and sharing on mclo.gs. The same for a server in a
//! folder on this PC and one on a Hopper Server (read through its API).
//! The card carries the fixes (put the older mod back, turn mods off).
//! The tabs, the list and the card's frame are shared with an instance's
//! tab (crash_list.rs).

use crate::crash_help::callout;
use crate::crash_list::{self, CrashList, Pane};
use crate::line_select::{LineSelect, Lines};
use crate::store::{Store, StoreEvent, open_path};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::spinner::Spinner;
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
    /// The crash reports, the older logs and sharing (shared with an
    /// instance's tab).
    crashes: CrashList,
    /// Reports read so far; `None` while reading.
    cards: HashMap<String, Option<Result<Card, String>>>,
    /// The card's mods (jars) whose "Why?" list is open.
    why_open: HashSet<String>,
    /// Reports whose whole list of mods shows (else the first two).
    more_open: HashSet<String>,
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
    _follow: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl crash_list::Host for ServerLogsView {
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

    fn running(&self, _cx: &App) -> bool {
        // Hopper Server and crashfiles refuse the log a server writes.
        false
    }

    fn keep_label(&self) -> &'static str {
        // A server's log has no player of yours to hide (as Hopper Server).
        "Keep user and folder names"
    }

    fn after_delete(&mut self, rels: &[String], cx: &mut Context<Self>) {
        for r in rels {
            self.cards.remove(r);
        }
        self.crashes.forget(rels);
        if self.pane == Pane::Older {
            crash_list::load_old(self, cx);
        }
        // Reads the list again now; it also picks the newest report when
        // the one on screen is gone.
        crash_list::watch(self, cx);
        cx.notify();
    }
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
            crashes: CrashList::new(true, cx),
            cards: HashMap::new(),
            why_open: HashSet::new(),
            more_open: HashSet::new(),
            fixed: HashSet::new(),
            turn_off_pick: None,
            fix_busy: false,
            log: None,
            errors_only: false,
            query,
            visible: None,
            lines: LineSelect::new(cx),
            _follow: None,
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
            self.crashes.reset();
            self.cards.clear();
            self.why_open.clear();
            self.more_open.clear();
            self.fixed.clear();
            self.turn_off_pick = None;
        }
        self.active = true;
        crash_list::watch(self, cx);
        let pane = self.pane;
        self.log = None;
        self.select(pane, cx);
    }

    /// Opens the crash reports on `rel` (the newest when none): where the
    /// crash notice's "See why" lands.
    pub fn open_report(&mut self, rel: Option<String>, cx: &mut Context<Self>) {
        self.select(Pane::Crashes, cx);
        if let Some(rel) = rel {
            self.pick_report(rel, cx);
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
        self.run_fix_then(rel, mark_fixed, false, f, window, cx)
    }

    /// [`Self::run_fix`]; `reread`: the card is read again after it (a
    /// config setting changed, which the card shows).
    fn run_fix_then(
        &mut self,
        rel: String,
        mark_fixed: bool,
        reread: bool,
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
                if done.is_ok() && reread {
                    this.cards.remove(&rel);
                    this.pick_report(rel.clone(), cx);
                }
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
        self.crashes.stop();
    }

    /// Shows report `rel`, reading it off the UI thread the first time.
    fn pick_report(&mut self, rel: String, cx: &mut Context<Self>) {
        if !self.crashes.sel.contains(&rel) {
            self.crashes.sel.set_one(rel.clone());
        }
        self.crashes.picked = Some(rel.clone());
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
            crash_list::load_old(self, cx);
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
                    let at_end = this.lines.at_end();
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
        self.lines.scroll_to_end();
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
}

impl Render for ServerLogsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(root) = self.root.clone() else {
            return div().into_any_element();
        };
        self.visible = self.filter(cx);
        let running = crate::rigsync::server_online(self.store.read(cx), None, &root);
        let tabs = crash_list::tabs("srv-log-tabs", self.pane, self.crashes.count(), running, cx, |this, p, cx| this.select(p, cx));
        let body = match self.pane {
            Pane::Crashes => {
                let report = self.crashes.picked.clone().and_then(|p| self.crashes.report(&p).cloned());
                let card = match report {
                    Some(r) => self.report_card(&root, &r, cx),
                    None => div().into_any_element(),
                };
                crash_list::split(self, card, "This server hasn't crashed. If it does, the report and what Hopper makes of it show up here.", cx)
            }
            Pane::Older => crash_list::older_pane(self, "The server keeps its last start in latest.log. Older ones show up here.", cx),
            _ => self.log_body(&root, window, cx),
        };
        v_flex().size_full().gap_3().child(tabs).child(body).into_any_element()
    }
}

impl ServerLogsView {
    /// Report `r` as Hopper read it: the mods with how sure, why and what
    /// to do, the story when a mod stopped the server loading, and advice.
    fn report_card(&mut self, root: &Path, r: &crash_list::Report, cx: &mut Context<Self>) -> AnyElement {
        use crate::crash_view;
        let theme = cx.theme().clone();
        let running_now = crate::rigsync::server_online(self.store.read(cx), None, root);
        let path = root.join(&r.rel);
        let times = self.crashes.times(&r.rel);
        let head = |title: &'static str, cx: &mut Context<Self>| crash_list::card_head(title, r.modified, times, vec![crash_list::open_button(path.clone())], cx);
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
                    .child(div().pt_1().child(crash_list::crash_share_row(self, &r.rel, "", cx)))
                    .into_any_element();
            }
            Some(Some(Ok(c))) => c.clone(),
        };
        let Some(c) = card.crash else {
            return callout(theme.danger, cx)
                .child(head("The server crashed", cx))
                .child(widgets::caption("Hopper couldn't tell what went wrong. The report has the details.", cx))
                .child(div().pt_1().child(crash_list::crash_share_row(self, &r.rel, "", cx)))
                .into_any_element();
        };
        let share = crash_list::crash_share_row(self, &r.rel, &c.headline, cx);

        // Names and icons from the pack the server runs, else the jar's own.
        let addons: Vec<Addon> = {
            let store = self.store.read(cx);
            store.server(root).and_then(|sv| store.paired(sv)).map(|i| i.addons.clone()).unwrap_or_default()
        };
        let pack_root = {
            let store = self.store.read(cx);
            store.server(root).and_then(|sv| store.paired(sv)).map(|i| i.root.clone())
        };
        let pack_updates = pack_root.clone();
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
        // A setting in the crash's mods' configs that works around it: the
        // same box as an instance's crash card.
        if !c.switches.is_empty() || !c.switched_off.is_empty() {
            let who: Vec<AnyElement> = c
                .switches
                .iter()
                .enumerate()
                .map(|(i, sw)| match c.suspects.iter().find(|s| s.mod_id == sw.mod_id) {
                    Some(s) => link(&s.file_name, &format!("switch{i}"), cx),
                    None => div().font_semibold().child(sw.mod_id.clone()).into_any_element(),
                })
                .collect();
            // A Hopper Server's files aren't here to compare: the card says
            // the new value instead.
            let remote = hopper::hopperd::is_remote_root(root);
            let diffs: Vec<Vec<(bool, String)>> =
                c.switched_off.iter().map(|sw| if remote { Vec::new() } else { hopper::configswitch::applied_diff(root, sw) }).collect();
            let view = cx.entity().downgrade();
            let run = move |rel: String, on: bool| -> std::rc::Rc<dyn Fn(hopper::configswitch::Switch, &mut Window, &mut App)> {
                let view = view.clone();
                std::rc::Rc::new(move |sw, window, cx| {
                    let rel = rel.clone();
                    view.update(cx, |this, cx| {
                        this.run_fix_then(
                            rel,
                            false,
                            true,
                            move |root| if on { crate::server_crash::switch_on(root, &sw) } else { crate::server_crash::switch_off(root, &sw) },
                            window,
                            cx,
                        )
                    })
                    .ok();
                })
            };
            // The config editor reads files on this PC; a Hopper Server's are
            // edited in its Configs tab.
            let open: Option<std::rc::Rc<dyn Fn(hopper::configswitch::Switch, &mut Window, &mut App)>> = (!remote).then(|| {
                let (store, root) = (self.store.clone(), root.to_path_buf());
                std::rc::Rc::new(move |sw: hopper::configswitch::Switch, window: &mut Window, cx: &mut App| {
                    let focus = crate::config_editor::Focus { line: sw.line, note: Some(sw.why.clone()).filter(|w| !w.is_empty()) };
                    crate::config_editor::open(store.clone(), root.clone(), vec![sw.file.clone()], sw.file.clone(), Some(focus), window, cx)
                }) as std::rc::Rc<dyn Fn(hopper::configswitch::Switch, &mut Window, &mut App)>
            });
            let host = crate::logs::crash_switch::SwitchHost {
                running: blocked,
                running_tip: "Stop the server first: it may write its config back when it stops.",
                check: "Start the server to check the crash is gone.",
                off: run(r.rel.clone(), false),
                on: run(r.rel.clone(), true),
                open,
                play: None,
            };
            let or = changed.is_some() || !off_mods.is_empty();
            let at = if crate::logs::crash_switch::settings_first(&c) { 1 } else { 2 };
            if let Some(b) = crate::logs::crash_switch::switch_list(&c, who, diffs, or, None, host, cx) {
                body.insert(at.min(body.len()), b);
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
                // The pack's last update check says whether "Update it" can help.
                let newer = pack_updates.as_ref().and_then(|p| {
                    let pid = named(&addons, &names, &s.file_name).1.map_or(0, |a| a.project_id);
                    crash_view::newer_out(self.store.read(cx).updates(p), pid)
                });
                below.extend(crash_view::steps_list(s, &root.join("mods"), newer, &readable, cx));
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
        body.extend(crash_list::follow_up_note(&c, cx));
        callout(theme.danger, cx)
            .child(head(if c.stuck { "The server got stuck starting" } else { "The server crashed" }, cx))
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
        let share = crash_list::share_row(self, rel, cx);
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
