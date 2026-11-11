//! The parts of Logs & crashes an instance and a server share: the tab bar
//! (Crash reports · latest.log · debug.log · Older logs), the list of crash
//! reports with the same failure and its follow-ups folded under one row,
//! the older logs, deleting, sharing on mclo.gs and the card's frame. Each
//! view keeps a [`CrashList`] and says how to show a report ([`Host`]); the
//! card inside the frame is its own.

use crate::log_files;
use crate::selection::{self, KeyAct, Selection};
use crate::widgets;
use crate::widgets::Clickable as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::crashgroup::Role;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How many of the newest reports get a verdict in the list (as Hopper
/// Server does): reading a report means reading the mods too.
const WITH_VERDICT: usize = 25;
/// New verdicts worked out per pass on this PC (each reads a report).
const VERDICTS_PER_PASS: usize = 3;

/// The tab's own tabs, the same for instances and servers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pane {
    Crashes,
    Latest,
    Debug,
    /// The older logs in logs/ (packed away by later starts), to delete.
    Older,
}

impl Pane {
    pub const ALL: [Pane; 4] = [Pane::Crashes, Pane::Latest, Pane::Debug, Pane::Older];

    /// The log it shows, relative to the game's or server's folder.
    pub fn log(self) -> Option<&'static str> {
        match self {
            Pane::Crashes | Pane::Older => None,
            Pane::Latest => Some("logs/latest.log"),
            Pane::Debug => Some("logs/debug.log"),
        }
    }
}

/// A crash report in the list.
#[derive(Clone)]
pub struct Report {
    /// "crash-reports/crash-….txt"
    pub rel: String,
    /// Written, in seconds since the epoch.
    pub modified: i64,
    /// Hopper's one line on it (the newest few only).
    pub verdict: Option<String>,
    /// Its text says it only followed from an earlier error.
    pub follow_up: bool,
}

/// What a game's or server's crash list holds; each view keeps one.
pub struct CrashList {
    /// A server's (else a game's): how reports are read.
    server: bool,
    /// Crash reports, newest first; `None` while the first list loads.
    pub reports: Option<Result<Vec<Report>, String>>,
    /// Verdicts on this PC's reports already worked out, by report and
    /// time (a report doesn't change once written).
    verdicts: HashMap<(String, i64), String>,
    /// The report shown beside the list.
    pub picked: Option<String>,
    /// The reports picked with Ctrl / Shift (the shown one alone else).
    pub sel: Selection,
    /// Rows whose folded reports show.
    folds_open: HashSet<String>,
    pub focus: FocusHandle,
    /// The older logs; `None` until the pane first opens.
    pub old: Option<Result<Vec<log_files::OldLog>, String>>,
    old_sel: Selection,
    old_focus: FocusHandle,
    /// mclo.gs links by file; `None` while uploading.
    shared: HashMap<String, Option<Result<String, String>>>,
    /// Share with the user and player name left in.
    keep_names: bool,
    /// How many times each list row's failure came, and the first time
    /// (for the card's header).
    times: HashMap<String, (usize, i64)>,
    _list: Option<Task<()>>,
}

impl CrashList {
    pub fn new(server: bool, cx: &mut App) -> Self {
        Self {
            server,
            reports: None,
            verdicts: HashMap::new(),
            picked: None,
            sel: Selection::new(),
            folds_open: HashSet::new(),
            focus: cx.focus_handle(),
            old: None,
            old_sel: Selection::new(),
            old_focus: cx.focus_handle(),
            shared: HashMap::new(),
            keep_names: false,
            times: HashMap::new(),
            _list: None,
        }
    }

    /// Another game or server: forget everything.
    pub fn reset(&mut self) {
        self.reports = None;
        self.verdicts.clear();
        self.picked = None;
        self.sel.clear();
        self.folds_open.clear();
        self.old = None;
        self.old_sel.clear();
        self.shared.clear();
        self.times.clear();
        self._list = None;
    }

    /// The tab went away: stop looking.
    pub fn stop(&mut self) {
        self._list = None;
    }

    pub fn count(&self) -> usize {
        match &self.reports {
            Some(Ok(l)) => l.len(),
            _ => 0,
        }
    }

    pub fn report(&self, rel: &str) -> Option<&Report> {
        match &self.reports {
            Some(Ok(l)) => l.iter().find(|r| r.rel == rel),
            _ => None,
        }
    }

    /// How many times the shown row's failure came and the first time,
    /// when more than once.
    pub fn times(&self, rel: &str) -> Option<(usize, i64)> {
        self.times.get(rel).copied().filter(|(n, _)| *n > 1)
    }

    /// Files went: forget them.
    pub fn forget(&mut self, rels: &[String]) {
        for r in rels {
            self.shared.remove(r);
            self.sel.keys_mut().remove(r);
            self.old_sel.keys_mut().remove(r);
        }
        if let Some(Ok(l)) = &mut self.reports {
            l.retain(|r| !rels.contains(&r.rel));
        }
        if self.picked.as_ref().is_some_and(|p| rels.contains(p)) {
            self.picked = None;
        }
        self.old_sel.clear();
        self.old = None;
    }
}

/// A view that shows a [`CrashList`].
pub trait Host: Sized + 'static {
    fn list(&self) -> &CrashList;
    fn list_mut(&mut self) -> &mut CrashList;
    fn root(&self) -> Option<PathBuf>;
    /// Shows report `rel` beside the list (reading it the first time).
    fn pick(&mut self, rel: String, cx: &mut Context<Self>);
    /// The game or server runs (the log it writes can't be deleted).
    fn running(&self, cx: &App) -> bool;
    /// Files went (`rels`, or a whole kind when empty): look again.
    fn after_delete(&mut self, rels: &[String], cx: &mut Context<Self>);
    /// The player's name, hidden when sharing.
    fn player(&self, _cx: &App) -> Option<String> {
        None
    }
    /// The share row's tick box: what it keeps.
    fn keep_label(&self) -> &'static str {
        "Keep my user and player name"
    }
}

/// "crash-2026-11-04_14.54.48-server.txt" from its relative path.
pub fn file_name(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// The tab bar: "Crash reports (17) · latest.log · debug.log · Older logs".
pub fn tabs<V: Host>(id: &'static str, pane: Pane, count: usize, live: bool, cx: &mut Context<V>, on: impl Fn(&mut V, Pane, &mut Context<V>) + 'static) -> TabBar {
    let ix = Pane::ALL.iter().position(|p| *p == pane).unwrap_or(0);
    let muted = cx.theme().muted_foreground;
    TabBar::new(id)
        .underline()
        .small()
        .selected_index(ix)
        .child(
            Tab::new()
                .label("Crash reports")
                // The count as plain muted text, like the other tab bars.
                .when(count > 0, |t| t.suffix(div().text_xs().text_color(muted).child(count.to_string()))),
        )
        .child(Tab::new().label(if live { "latest.log · live" } else { "latest.log" }))
        .child(Tab::new().label("debug.log"))
        .child(Tab::new().label("Older logs"))
        .on_click(cx.listener(move |this, ix: &usize, _, cx| on(this, Pane::ALL[*ix], cx)))
}

// ---- Reading, off the UI thread ------------------------------------------

/// Lists the crash reports now and every few seconds while shown. A new
/// list keeps the report shown when it is still there, else shows the
/// newest one that isn't folded away.
pub fn watch<V: Host>(this: &mut V, cx: &mut Context<V>) {
    let Some(root) = this.root() else { return };
    let server = this.list().server;
    let task = cx.spawn(async move |view: WeakEntity<V>, cx: &mut AsyncApp| {
        loop {
            let Ok(known) = view.update(cx, |this, _| this.list().verdicts.clone()) else { return };
            let r = root.clone();
            let got = cx.background_spawn(async move { list_reports(&r, known, server) }).await;
            let alive = view.update(cx, |this, cx| {
                if this.root().as_ref() != Some(&root) {
                    return false;
                }
                let l = this.list_mut();
                let list = match got {
                    Ok((list, verdicts)) => {
                        l.verdicts = verdicts;
                        Ok(list)
                    }
                    Err(e) => Err(e),
                };
                // A failed poll keeps the list already shown.
                if list.is_err() && matches!(l.reports, Some(Ok(_))) {
                    return true;
                }
                let pick = match &list {
                    Ok(v) if !l.picked.as_ref().is_some_and(|p| v.iter().any(|r| &r.rel == p)) => first_shown(v),
                    _ => None,
                };
                l.reports = Some(list);
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
            let more = view
                .update(cx, |this, _| {
                    let l = this.list();
                    matches!(&l.reports, Some(Ok(v)) if v.iter().take(WITH_VERDICT).any(|r| !l.verdicts.contains_key(&(r.rel.clone(), r.modified))))
                })
                .unwrap_or(false);
            let wait = if more && !hopper::hopperd::is_remote_root(&root) { 0 } else { 10 };
            cx.background_executor().timer(Duration::from_secs(wait)).await;
        }
    });
    this.list_mut()._list = Some(task);
}

/// The report a list opens on: the newest row (a folded report's row is
/// the one that explains it).
fn first_shown(list: &[Report]) -> Option<String> {
    let roles = roles(list);
    let i = (0..list.len()).find(|&i| matches!(roles[i], Role::Plain | Role::Lead(_)))?;
    Some(list[i].rel.clone())
}

fn roles(list: &[Report]) -> Vec<Role> {
    let items: Vec<hopper::crashgroup::Item> =
        list.iter().map(|r| hopper::crashgroup::Item { modified: r.modified, follow_up: r.follow_up, verdict: r.verdict.as_deref() }).collect();
    hopper::crashgroup::group(&items)
}

/// The crash reports of a game or server, newest first, the newest few with
/// a verdict. On this PC the verdicts are worked out here (`known` has the
/// ones already done, returned with the new ones); Hopper Server sends its
/// own.
#[allow(clippy::type_complexity)]
fn list_reports(root: &Path, mut known: HashMap<(String, i64), String>, server: bool) -> Result<(Vec<Report>, HashMap<(String, i64), String>), String> {
    if let Some(l) = hopper::hopperd::locate(root) {
        let (c, id, _) = l.map_err(|e| format!("{e:#}"))?;
        let list = c.crashes(&id).map_err(|e| format!("{e:#}"))?;
        let reports = list.into_iter().map(|(rel, modified, verdict, follow_up)| Report { rel, modified: modified as i64, verdict, follow_up }).collect();
        return Ok((reports, known));
    }
    // Over SFTP / FTP: fetch what changed into Hopper's copy first.
    let _ = hopper::remotefs::refresh_path(&root.join("crash-reports"));
    let mods = root.join("mods");
    // Crash reports and stuck starts' saved dumps, each once.
    let mut files = hopper::logs::crash_reports(root);
    files.extend(hopper::stuckfile::dumps(root));
    files.sort();
    files.dedup();
    let mut reports: Vec<Report> = files
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
                let path = root.join(&r.rel);
                let c = if server { hopper::crashdetect::server_report(root, &path) } else { hopper::crashdetect::game_report(root, &path) };
                let v = c.map(|c| hopper::crashdetect::verdict(&c, &|s| hopper::crashdetect::jar_name(&mods, &s.file_name))).unwrap_or_default();
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

// ---- Deleting ------------------------------------------------------------

/// "Delete" on crash reports `rels` (one, or a row with what folds under
/// it); `picked` when they are the rows picked in the list.
pub fn ask_delete<V: Host>(this: &mut V, rels: Vec<String>, picked: bool, window: &mut Window, cx: &mut Context<V>) {
    let Some(root) = this.root() else { return };
    let n = rels.len();
    let (title, what) = if n == 1 {
        ("Delete this crash report?".to_string(), "It".to_string())
    } else if picked {
        (format!("Delete these {n} crash reports?"), "They (with the reports folded under the ones picked)".to_string())
    } else {
        (format!("Delete these {n} crash reports?"), "They are the report and the ones folded under it; they".to_string())
    };
    let body = format!("{what} go to {}, where you can get {} back.", log_files::trash_name(&root), if n == 1 { "it" } else { "them" });
    let running = this.running(cx);
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

/// "Clear all" for crash reports, or the older logs.
pub fn ask_clear<V: Host>(this: &mut V, kind: hopper::crashfiles::Kind, window: &mut Window, cx: &mut Context<V>) {
    let Some(root) = this.root() else { return };
    let crashes = kind == hopper::crashfiles::Kind::Crashes;
    let l = this.list();
    let n = if crashes {
        l.count()
    } else {
        match &l.old {
            Some(Ok(v)) => v.len(),
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

/// "Delete" on older logs `rels` (the one pressed, or those picked).
fn ask_delete_logs<V: Host>(this: &mut V, rels: Vec<String>, window: &mut Window, cx: &mut Context<V>) {
    let Some(root) = this.root() else { return };
    let (title, body) = match rels.as_slice() {
        [one] => ("Delete this log?".to_string(), format!("{} goes to {}, where you can get it back.", file_name(one), log_files::trash_name(&root))),
        _ => (format!("Delete these {} logs?", rels.len()), format!("They go to {}, where you can get them back.", log_files::trash_name(&root))),
    };
    let running = this.running(cx);
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

/// The picked rows with what folds under them, each once, in list order.
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

// ---- The list ------------------------------------------------------------

/// The list of crash reports (the left column): Clear all, a row per
/// failure with how many times it came and its follow-ups folded under it,
/// Ctrl / Shift picking, Delete and right-click.
pub fn list_column<V: Host>(this: &mut V, cx: &mut Context<V>) -> AnyElement {
    let theme = cx.theme().clone();
    let Some(Ok(list)) = this.list().reports.clone() else { return div().into_any_element() };
    let picked = this.list().picked.clone();
    let roles = roles(&list);
    let folds_open = this.list().folds_open.clone();
    // Folded reports show only when their row is open, or one is the
    // report shown.
    let shown = |i: usize| match &roles[i] {
        Role::FollowUp(Some(lead)) | Role::Repeat(lead) => picked.as_ref() == Some(&list[i].rel) || folds_open.contains(&list[*lead].rel),
        _ => true,
    };
    // Each row's group: deleting a row takes what folds under it.
    let group_of = |i: usize| {
        let mut g = vec![list[i].rel.clone()];
        if let Role::Lead(f) = &roles[i] {
            g.extend(f.iter().map(|&j| list[j].rel.clone()));
        }
        g
    };
    let order: Vec<String> = (0..list.len()).filter(|&i| shown(i)).map(|i| list[i].rel.clone()).collect();
    let groups: HashMap<String, Vec<String>> = (0..list.len()).map(|i| (list[i].rel.clone(), group_of(i))).collect();
    // The card's "4 times since 03:49", for each row and what folds under it.
    let mut times = HashMap::new();
    for i in 0..list.len() {
        if let Role::Lead(f) = &roles[i] {
            let n = hopper::crashgroup::times(&roles, i);
            let first = f.iter().filter(|&&j| matches!(roles[j], Role::Repeat(_))).map(|&j| list[j].modified).min().unwrap_or(list[i].modified);
            for r in std::iter::once(i).chain(f.iter().copied()) {
                times.insert(list[r].rel.clone(), (n, first));
            }
        }
    }
    let l = this.list_mut();
    l.times = times;
    l.sel.retain(&order);
    let many = l.sel.many();
    let sel_order = l.sel.in_order(&order);
    let is_sel = |rel: &str| l.sel.contains(rel);
    let mut rows: Vec<AnyElement> = Vec::new();
    for (i, r) in list.iter().enumerate().filter(|(i, _)| shown(*i)) {
        let sel = if many { is_sel(&r.rel) } else { picked.as_ref() == Some(&r.rel) };
        let folded = matches!(roles[i], Role::FollowUp(Some(_)) | Role::Repeat(_));
        let title = match &roles[i] {
            Role::FollowUp(lead) => hopper::crashgroup::follow_up_title(lead.and_then(|l| list[l].verdict.as_deref())),
            _ => r.verdict.clone().unwrap_or_else(|| file_name(&r.rel).to_string()),
        };
        let n = hopper::crashgroup::times(&roles, i);
        let rel = r.rel.clone();
        // Right-click on a picked row acts on every picked row.
        let menu_group = if many && sel { picked_groups(&sel_order, &groups) } else { group_of(i) };
        let menu_many = many && sel;
        let (menu_view, click_order) = (cx.entity(), order.clone());
        rows.push(
            v_flex()
                .id(("crash-row", i))
                .gap_0p5()
                .px_2()
                .py_1p5()
                .when(folded, |d| d.pl_5())
                .rounded(theme.radius)
                .clickable(cx)
                .map(|d| if sel { widgets::row_selected(d, cx) } else { d.hover(|s| widgets::row_hover(s, cx)) })
                .child(
                    h_flex()
                        .gap_1p5()
                        .items_start()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .font_medium()
                                .when(folded || r.verdict.is_none(), |d| d.text_color(theme.muted_foreground))
                                .line_clamp(2)
                                .child(title),
                        )
                        .when(n > 1, |d| {
                            let tip = format!("The same crash {n} times in a row");
                            d.child(
                                div()
                                    .id(("crash-times", i))
                                    .flex_none()
                                    .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
                                    .child(widgets::chip(theme.danger).child(format!("×{n}"))),
                            )
                        }),
                )
                .child(
                    widgets::caption(log_files::when(r.modified), cx)
                        .id(("crash-when", i))
                        .tooltip({
                            let full = hopper::time::exact_full(r.modified);
                            move |w, cx| gpui_kit::component::tooltip::Tooltip::new(full.clone()).build(w, cx)
                        })
                        .text_xs(),
                )
                .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                    window.focus(&this.list().focus, cx);
                    if this.list_mut().sel.click(&rel, e.modifiers(), &click_order) {
                        this.pick(rel.clone(), cx);
                    }
                    cx.notify();
                }))
                .context_menu(move |m, _, _| {
                    let (v, g) = (menu_view.clone(), menu_group.clone());
                    let label = if menu_many {
                        format!("Delete {} reports…", g.len())
                    } else if g.len() > 1 {
                        format!("Delete with the {} folded under it…", g.len() - 1)
                    } else {
                        "Delete report…".to_string()
                    };
                    m.item(PopupMenuItem::new(label).icon(IconName::Trash).on_click(move |_, window, cx| {
                        let g = g.clone();
                        v.update(cx, |this, cx| ask_delete(this, g, menu_many, window, cx));
                    }))
                })
                .into_any_element(),
        );
        if let Some(label) = hopper::crashgroup::fold_text(&roles, i) {
            let open = folds_open.contains(&r.rel);
            let lead = r.rel.clone();
            rows.push(
                h_flex()
                    .id(("crash-fold", i))
                    .gap_1()
                    .px_2()
                    .py_1()
                    .pl_5()
                    .rounded(theme.radius)
                    .clickable(cx)
                    .hover(|s| widgets::row_hover(s, cx))
                    .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall().text_color(theme.muted_foreground))
                    .child(widgets::caption(if open { "Hide them".to_string() } else { label }, cx).text_xs())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let f = &mut this.list_mut().folds_open;
                        if !f.remove(&lead) {
                            f.insert(lead.clone());
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
    }
    let bar = many.then(|| {
        let (o, g) = (order.clone(), groups.clone());
        let del = cx.listener(move |this: &mut V, _: &ClickEvent, window, cx| {
            let rels = picked_groups(&this.list().sel.in_order(&o), &g);
            ask_delete(this, rels, true, window, cx)
        });
        let n = this.list().sel.len();
        let bar = selection::bar(
            "crash-bulk",
            n,
            Vec::new(),
            Some(("Delete".into(), Box::new(del))),
            cx.listener(|this: &mut V, _: &ClickEvent, _, cx| {
                let l = this.list_mut();
                match l.picked.clone() {
                    Some(p) => l.sel.set_one(p),
                    None => l.sel.clear(),
                }
                cx.notify();
            }),
            cx,
        );
        selection::floating("crash-bulk", bar)
    });
    let key_order = order.clone();
    let side = div()
        .id("crash-list")
        .size_full()
        .child(
            v_flex()
                .gap_0p5()
                .pr_2()
                .child(
                    h_flex().px_2().pb_1().child(div().flex_1()).child(
                        Button::new("crash-clear")
                            .xsmall()
                            .ghost()
                            .icon(IconName::Trash)
                            .label("Clear all")
                            .tooltip("Deletes every crash report")
                            .on_click(cx.listener(|this, _, window, cx| ask_clear(this, hopper::crashfiles::Kind::Crashes, window, cx))),
                    ),
                )
                .children(rows)
                // Room under the last row for the bulk bar.
                .when(many, |d| d.pb(rems(4.))),
        )
        .overflow_y_scrollbar();
    div()
        .relative()
        .w(rems(20.))
        .flex_none()
        .h_full()
        .track_focus(&this.list().focus)
        .on_key_down(cx.listener(move |this, e: &KeyDownEvent, window, cx| {
            match selection::on_key(&mut this.list_mut().sel, e, &key_order, cx) {
                KeyAct::None => return,
                KeyAct::Delete => {
                    let l = this.list();
                    let rels = picked_groups(&l.sel.in_order(&key_order), &groups);
                    let many = l.sel.many();
                    ask_delete(this, rels, many, window, cx)
                }
                KeyAct::Moved(k) => this.pick(k, cx),
                KeyAct::Changed => {
                    let l = this.list();
                    if let Some(c) = l.sel.cursor().map(str::to_string).filter(|c| l.sel.len() == 1 && l.sel.contains(c)) {
                        this.pick(c, cx);
                    }
                }
            }
            cx.notify();
        }))
        .child(side)
        .children(bar)
        .into_any_element()
}

/// The list beside the card, or what to say instead (loading, can't read,
/// none). `none` is the empty state's line.
pub fn split<V: Host>(this: &mut V, card: AnyElement, none: &'static str, cx: &mut Context<V>) -> AnyElement {
    match &this.list().reports {
        None => return widgets::skeleton_rows(6, rems(2.75), cx).into_any_element(),
        Some(Err(e)) => {
            return widgets::empty_state(IconName::TriangleAlert, "Can't read the crash reports", Some(e.clone().into()), cx).into_any_element();
        }
        Some(Ok(l)) if l.is_empty() => {
            return widgets::empty_state(IconName::CircleCheck, "No crash reports", Some(none.into()), cx).into_any_element();
        }
        Some(Ok(_)) => {}
    }
    let side = list_column(this, cx);
    h_flex()
        .flex_1()
        .min_h_0()
        .items_start()
        .gap_4()
        .child(side)
        .child(
            div()
                .id("crash-card")
                .flex_1()
                .min_w_0()
                .h_full()
                // No cap: the top cards and the list fill the page.
                .child(div().w_full().pb_2().child(card))
                .overflow_y_scrollbar(),
        )
        .into_any_element()
}

// ---- The older logs --------------------------------------------------------

/// Reads the older logs off the UI thread.
pub fn load_old<V: Host>(this: &mut V, cx: &mut Context<V>) {
    let Some(root) = this.root() else { return };
    cx.spawn(async move |view: WeakEntity<V>, cx: &mut AsyncApp| {
        let r = root.clone();
        let got = cx.background_spawn(async move { log_files::old_logs(&r) }).await;
        view.update(cx, |this, cx| {
            if this.root().as_ref() == Some(&root) {
                this.list_mut().old = Some(got);
                cx.notify();
            }
        })
        .ok();
    })
    .detach();
}

/// The older logs, each with its time and size and a delete.
pub fn older_pane<V: Host>(this: &mut V, none: &'static str, cx: &mut Context<V>) -> AnyElement {
    let list = match &this.list().old {
        None => return widgets::skeleton_rows(4, rems(2.), cx).into_any_element(),
        Some(Err(e)) => {
            return widgets::empty_state(IconName::TriangleAlert, "Can't read the logs", Some(e.clone().into()), cx).into_any_element();
        }
        Some(Ok(l)) if l.is_empty() => {
            return widgets::empty_state(IconName::CircleCheck, "No older logs", Some(none.into()), cx).into_any_element();
        }
        Some(Ok(l)) => l.iter().map(|f| (f.rel.clone(), f.modified, f.size)).collect::<Vec<_>>(),
    };
    let radius = cx.theme().radius;
    let order: Vec<String> = list.iter().map(|f| f.0.clone()).collect();
    this.list_mut().old_sel.retain(&order);
    let rows: Vec<AnyElement> = list
        .iter()
        .enumerate()
        .map(|(i, (rel, modified, size))| {
            let (del_rel, click_rel) = (rel.clone(), rel.clone());
            let sel = this.list().old_sel.contains(rel);
            let order = order.clone();
            h_flex()
                .id(("old-log", i))
                .gap_3()
                .px_2()
                .py_1p5()
                .rounded(radius)
                .clickable(cx)
                .map(|d| if sel { widgets::row_selected(d, cx) } else { d.hover(|s| widgets::row_hover(s, cx)) })
                .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                    this.list_mut().old_sel.click(&click_rel, e.modifiers(), &order);
                    window.focus(&this.list().old_focus, cx);
                    cx.notify();
                }))
                .child(div().flex_1().min_w_0().truncate().text_sm().child(file_name(rel).to_string()))
                .child(
                    widgets::caption(log_files::when(*modified), cx)
                        .id(("old-log-when", i))
                        .tooltip({
                            let full = hopper::time::exact_full(*modified);
                            move |w, cx| gpui_kit::component::tooltip::Tooltip::new(full.clone()).build(w, cx)
                        })
                        .text_xs(),
                )
                .child(widgets::caption(log_files::size(*size), cx).text_xs().w(rems(5.)))
                .child(
                    Button::new(("old-log-del", i))
                        .xsmall()
                        .ghost()
                        .icon(IconName::Trash)
                        .tooltip("Delete this log")
                        .on_click(cx.listener(move |this, _, window, cx| ask_delete_logs(this, vec![del_rel.clone()], window, cx))),
                )
                .into_any_element()
        })
        .collect();
    let bar = this.list().old_sel.many().then(|| {
        let o = order.clone();
        let del = cx.listener(move |this: &mut V, _: &ClickEvent, window, cx| {
            let rels = this.list().old_sel.in_order(&o);
            ask_delete_logs(this, rels, window, cx)
        });
        let bar = selection::bar(
            "old-bulk",
            this.list().old_sel.len(),
            Vec::new(),
            Some(("Delete".into(), Box::new(del))),
            cx.listener(|this: &mut V, _: &ClickEvent, _, cx| {
                this.list_mut().old_sel.clear();
                cx.notify();
            }),
            cx,
        );
        selection::floating("old-bulk", bar)
    });
    let key_order = order.clone();
    v_flex()
        .flex_1()
        .min_h_0()
        .w_full()
        .gap_2()
        .child(
            h_flex().child(div().flex_1()).child(
                Button::new("old-clear")
                    .small()
                    .outline()
                    .icon(IconName::Trash)
                    .label("Clear old logs")
                    .tooltip("Deletes every older log; latest.log and debug.log stay")
                    .on_click(cx.listener(|this, _, window, cx| ask_clear(this, hopper::crashfiles::Kind::Logs, window, cx))),
            ),
        )
        .child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .track_focus(&this.list().old_focus)
                .on_key_down(cx.listener(move |this, e: &KeyDownEvent, window, cx| {
                    match selection::on_key(&mut this.list_mut().old_sel, e, &key_order, cx) {
                        KeyAct::None => return,
                        KeyAct::Delete => {
                            let rels = this.list().old_sel.in_order(&key_order);
                            ask_delete_logs(this, rels, window, cx)
                        }
                        KeyAct::Changed | KeyAct::Moved(_) => {}
                    }
                    cx.notify();
                }))
                .child(div().id("old-logs").size_full().child(v_flex().gap_0p5().children(rows)).overflow_y_scrollbar())
                .children(bar),
        )
        .into_any_element()
}

// ---- Sharing -------------------------------------------------------------

/// Uploads file `rel` to mclo.gs, cleaned first (IPs and secrets always;
/// user folders and the player's name unless `keep`). Hopper Server does
/// it on its side.
fn share_file(root: &Path, rel: &str, keep: bool, player: Option<&str>) -> anyhow::Result<String> {
    if let Some(l) = hopper::hopperd::locate(root) {
        let (c, id, _) = l?;
        let v = c.post(&format!("/servers/{}/logs/share", hopper::hopperd::enc(&id)), serde_json::json!({ "path": rel, "keep_names": keep }))?;
        return v["url"].as_str().map(str::to_string).ok_or_else(|| anyhow::anyhow!("Hopper Server didn't send a link."));
    }
    let _ = hopper::remotefs::refresh_path(&root.join(rel));
    // A game's crash can come from a file beside the logs (Java's
    // hs_err_pid….log in the instance folder): on this PC, read it as is.
    let text = match hopper::hosting::logshare::read(root, rel) {
        Ok(t) => t,
        Err(_) if !rel.contains("..") && root.join(rel).is_file() => String::from_utf8_lossy(&std::fs::read(root.join(rel))?).into_owned(),
        Err(e) => return Err(e),
    };
    hopper::mclogs::upload(&hopper::hosting::logshare::for_share(&text, Some(root), keep, player))
}

fn share<V: Host>(this: &mut V, rel: String, cx: &mut Context<V>) {
    let Some(root) = this.root() else { return };
    let keep = this.list().keep_names;
    let player = this.player(cx);
    this.list_mut().shared.insert(rel.clone(), None);
    cx.spawn(async move |view: WeakEntity<V>, cx: &mut AsyncApp| {
        let (r, p) = (root.clone(), rel.clone());
        let got = cx.background_spawn(async move { share_file(&r, &p, keep, player.as_deref()) }).await.map_err(|e| format!("{e:#}"));
        view.update(cx, |this, cx| {
            if this.root().as_ref() == Some(&root) {
                this.list_mut().shared.insert(rel, Some(got));
                cx.notify();
            }
        })
        .ok();
    })
    .detach();
    cx.notify();
}

/// "Share on mclo.gs" with "Keep my user and player name" for file `rel`
/// (under the folder), or its link once shared.
pub fn share_row<V: Host>(this: &V, rel: &str, cx: &mut Context<V>) -> AnyElement {
    let theme = cx.theme();
    match this.list().shared.get(rel) {
        Some(None) => h_flex().gap_2().child(Spinner::new().small()).child(widgets::caption("Uploading to mclo.gs", cx)).into_any_element(),
        Some(Some(Ok(url))) => {
            let (u1, u2) = (url.clone(), url.clone());
            h_flex()
                .gap_2()
                .child(widgets::caption("Shared:", cx))
                .child(Button::new("shared-link").link().small().label(url.clone()).on_click(move |_, _, cx| cx.open_url(&u1)))
                .child(
                    Button::new("shared-copy")
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
            h_flex()
                .gap_3()
                .child(
                    Button::new("share-log")
                        .outline()
                        .small()
                        .icon(IconName::Share2)
                        .label("Share on mclo.gs")
                        .tooltip("Upload this file and get a link for Discord or a bug report. It becomes public. IPs and passwords are always hidden first.")
                        .on_click(cx.listener(move |this, _, _, cx| share(this, p.clone(), cx))),
                )
                .child(
                    Checkbox::new("keep-names")
                        .label(this.keep_label())
                        .tooltip("Off: user folders and the folder it's in become <user> and <server>, and your player name <player>, before upload")
                        .checked(this.list().keep_names)
                        .on_change(cx.listener(|this, v: &bool, _, cx| {
                            this.list_mut().keep_names = *v;
                            cx.notify();
                        })),
                )
                .into_any_element()
        }
    }
}

/// A crash card's share row plus "Report", which opens Hopper's bug form
/// titled "Crash: <headline>". Nothing is attached: the form asks for the
/// mclo.gs link the row makes.
pub fn crash_share_row<V: Host>(this: &V, rel: &str, headline: &str, cx: &mut Context<V>) -> AnyElement {
    let headline = headline.trim();
    let title = format!("Crash: {}", if headline.is_empty() { "the game or server crashed" } else { headline });
    h_flex()
        .gap_3()
        .flex_wrap()
        .child(share_row(this, rel, cx))
        .child(
            Button::new("crash-report")
                .link()
                .small()
                .label("Report")
                .tooltip("Report it on GitHub if Hopper caused this crash or got its cause wrong")
                .on_click(move |_, _, cx| {
                    use hopper::report::{issue_url, App};
                    cx.open_url(&issue_url(App::Hopper, hopper::selfupdate::CURRENT, Some(&title)))
                }),
        )
        .into_any_element()
}

// ---- The card's frame -------------------------------------------------------

/// The card's first line: "⚠ The server crashed · Today · 07:56:15 · 4
/// times since 03:49", then `buttons` on the right.
pub fn card_head(title: impl Into<SharedString>, when: i64, times: Option<(usize, i64)>, buttons: Vec<AnyElement>, cx: &App) -> Div {
    let theme = cx.theme();
    h_flex()
        .gap_2()
        .flex_wrap()
        .child(Icon::new(IconName::TriangleAlert).small().text_color(theme.danger))
        .child(div().font_semibold().text_color(theme.danger).child(title.into()))
        .when(when > 0, |d| {
            d.child(
                widgets::caption(log_files::when(when), cx)
                    .id("crash-head-when")
                    .tooltip({
                        let full = hopper::time::exact_full(when);
                        move |w, cx| gpui_kit::component::tooltip::Tooltip::new(full.clone()).build(w, cx)
                    }),
            )
        })
        .when_some(times, |d, (n, first)| {
            d.child(widgets::chip(theme.danger).child(format!("{n} times since {}", hopper::time::exact(first).trim_start_matches("Today · "))))
        })
        .child(div().flex_1())
        .children(buttons)
}

/// "Open report" for a crash report file.
pub fn open_button(path: PathBuf) -> AnyElement {
    Button::new("crash-open-report")
        .small()
        .outline()
        .icon(IconName::FileText)
        .label("Open report")
        .tooltip("Opens the crash report file")
        .on_click(move |_, _, _| crate::store::open_path(&path))
        .into_any_element()
}

/// The quiet line under a card whose report only has a follow-up error:
/// where the cause shown came from.
pub fn follow_up_note(c: &hopper::crashdetect::Crash, cx: &App) -> Option<AnyElement> {
    let n = hopper::crashdetect::follow_up_note(c)?;
    let theme = cx.theme();
    Some(
        h_flex()
            .gap_2()
            .items_start()
            .child(Icon::new(IconName::Info).xsmall().mt_0p5().text_color(theme.muted_foreground))
            .child(div().flex_1().min_w_0().text_xs().text_color(theme.muted_foreground).whitespace_normal().child(n))
            .into_any_element(),
    )
}
