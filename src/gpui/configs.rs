//! Server sync's Configs and History sections. Configs works like the Mods
//! section beside it: files sorted into Needs action / Kept off / Not needed
//! / Comments only / In sync, an action per row, and one "Push N to server".
//! A folder can also mirror or live-link (Folder rules). History lists and
//! undoes every sync.

use crate::store::{Store, open_path};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::configdiff::{self, ConfigDiff, FileDiff, FileStatus, Kind, Line, LineKind, MergedField};
use hopper::folderrules::{self as fr, Mode, Rule};
use hopper::instances::Instance;
use hopper::model::SyncOp;
use hopper::server::ServerInfo;
use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const MODES: [Mode; 4] = [Mode::Review, Mode::MirrorExcept, Mode::MirrorOnly, Mode::Link];

/// A preview line's height, in rem.
const LINE_H: f32 = 1.25;

/// A file row's height, in rem.
const ROW_H: f32 = 2.25;

/// What the compare is doing, for its progress line.
const PHASE_SCAN: usize = 0;
const PHASE_CONTENTS: usize = 1;
const PHASE_READ: usize = 2;

/// A file opened under its row: the side-by-side diff and its settings.
struct Preview {
    rel: String,
    rows: Result<Vec<Line>, String>,
    fields: Vec<MergedField>,
}

fn load_preview(rel: &str, status: FileStatus, c_dir: &Path, s_dir: &Path) -> Preview {
    let client = (status != FileStatus::OnlyServer).then(|| configdiff::read_text(&c_dir.join(rel)));
    let server = (status != FileStatus::OnlyClient).then(|| configdiff::read_text(&s_dir.join(rel)));
    let (ct, st) = match (client, server) {
        (Some(Err(e)), _) | (_, Some(Err(e))) => {
            return Preview {
                rel: rel.to_string(),
                rows: Err(e),
                fields: Vec::new(),
            };
        }
        (c, s) => (
            c.and_then(Result::ok).unwrap_or_default(),
            s.and_then(Result::ok).unwrap_or_default(),
        ),
    };
    Preview {
        rel: rel.to_string(),
        rows: Ok(configdiff::line_diff(&st, &ct)),
        fields: configdiff::merge_fields(&ct, &st),
    }
}

/// Where a file sits in the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Grp {
    /// Settings that differ.
    Changed,
    /// On the client, not on the server.
    Missing,
    /// Only on the server: the server's own, left alone.
    ServerOnly,
    /// Client settings or files a mod writes for itself.
    NotNeeded,
    /// Same settings, other comments or spacing.
    Format,
    /// Same on both sides.
    Same,
}

impl Grp {
    fn of(f: &FileDiff) -> Grp {
        match (f.kind, f.status) {
            (Kind::ClientSide | Kind::Generated, _) => Grp::NotNeeded,
            (Kind::FormatOnly, _) => Grp::Format,
            (_, FileStatus::Modified) => Grp::Changed,
            (_, FileStatus::OnlyClient) => Grp::Missing,
            (_, FileStatus::OnlyServer) => Grp::ServerOnly,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Grp::Changed => "Settings differ",
            Grp::Missing => "Not on the server",
            Grp::ServerOnly => "Only on the server",
            Grp::NotNeeded => "Not needed on a server",
            Grp::Format => "Comments only",
            Grp::Same => "In sync",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Grp::Changed => "the server has other values",
            Grp::Missing => "the pack has them, the server doesn't",
            Grp::ServerOnly => "left alone; copy or remove one by one",
            Grp::NotNeeded => "client settings and files mods write for themselves; syncing them changes nothing",
            Grp::Format => "same settings, only comments or spacing differ",
            Grp::Same => "identical on both sides",
        }
    }

    fn color(self, cx: &App) -> Hsla {
        let t = cx.theme();
        match self {
            Grp::Changed => t.info,
            Grp::Missing => t.warning,
            Grp::ServerOnly | Grp::NotNeeded | Grp::Format => t.muted_foreground,
            Grp::Same => t.success,
        }
    }
}

/// The filter over the list, like the Mods section's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Show {
    Needs,
    Kept,
    NotNeeded,
    Format,
    Same,
}

impl Show {
    const ALL: [Show; 5] = [Show::Needs, Show::Kept, Show::NotNeeded, Show::Format, Show::Same];

    fn label(self) -> &'static str {
        match self {
            Show::Needs => "Needs action",
            Show::Kept => "Kept off",
            Show::NotNeeded => "Not needed",
            Show::Format => "Comments only",
            Show::Same => "In sync",
        }
    }

    fn groups(self) -> &'static [Grp] {
        match self {
            Show::Needs => &[Grp::Changed, Grp::Missing],
            Show::Kept => &[Grp::ServerOnly],
            Show::NotNeeded => &[Grp::NotNeeded],
            Show::Format => &[Grp::Format],
            Show::Same => &[Grp::Same],
        }
    }
}

/// A row of the file list.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Item {
    /// A group's heading, with how many files it has.
    Head(Grp, usize),
    /// A differing file, by its index in the comparison.
    File(usize),
    /// A matching file, by its index in `same_files`.
    Same(usize),
    /// The open file's preview.
    Preview,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Act {
    Push,
    Pull,
    Delete,
}

/// Emitted when the number of files needing action changes, so the Configs
/// tab's count follows.
pub struct NeedsChanged;

impl EventEmitter<NeedsChanged> for ConfigsPanel {}

pub struct ConfigsPanel {
    store: Entity<Store>,
    /// The server's panel controls, whose running state is the freshest.
    control: Option<Entity<crate::server_control::ServerControl>>,
    server: Option<PathBuf>,
    folder: String,
    /// Config folders found on either side (None while they're looked
    /// for). Read once per server: the server sits on a network share,
    /// too slow to ask on every frame.
    present: Option<Vec<String>>,
    /// Whether the server was up as of the last store change.
    was_online: bool,
    diff: Option<Arc<ConfigDiff>>,
    running: bool,
    /// (done, total, phase) of the compare.
    progress: Arc<(AtomicUsize, AtomicUsize, AtomicUsize)>,
    selected: HashSet<String>,
    filter: Entity<InputState>,
    query: String,
    new_folder: Entity<InputState>,
    adding: bool,
    /// The filter picked by hand (else Needs action when anything needs it).
    show: Option<Show>,
    rules_open: bool,
    preview: Option<Preview>,
    preview_scroll: UniformListScrollHandle,
    values: bool,
    show_all: bool,
    mirror: Option<fr::MirrorPlan>,
    message: Option<Result<String, String>>,
    /// The file list's rows and its scroll state, and the comparison they
    /// were built from.
    items: Vec<Item>,
    list: ListState,
    items_for: Option<(Arc<ConfigDiff>, Show)>,
    needs_seen: Option<usize>,
    /// Mod keys of mods the server doesn't run: their configs aren't needed.
    client_mods: Arc<HashSet<String>>,
    _ticker: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ConfigsPanel {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Search files"));
        let new_folder = cx.new(|cx| InputState::new(window, cx).placeholder("Folder, like world or scripts"));
        let subs = vec![
            // Only the online strip depends on the store's frequent
            // changes: redraw when it flips, not on every tick.
            cx.observe(&store, |this, _, cx| this.online_changed(cx)),
            cx.subscribe(&filter, |this, state, e: &InputEvent, cx| {
                if matches!(e, InputEvent::Change) {
                    this.query = state.read(cx).value().trim().to_lowercase();
                    cx.notify();
                }
            }),
            cx.subscribe(&new_folder, |this, _, e: &InputEvent, cx| {
                if matches!(e, InputEvent::PressEnter { .. }) {
                    this.add_folder(cx);
                }
            }),
        ];
        Self {
            store,
            control: None,
            server: None,
            folder: String::new(),
            present: None,
            was_online: false,
            diff: None,
            running: false,
            progress: Arc::new((AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0))),
            selected: HashSet::new(),
            filter,
            query: String::new(),
            new_folder,
            adding: false,
            show: None,
            rules_open: false,
            preview: None,
            preview_scroll: UniformListScrollHandle::new(),
            values: true,
            show_all: false,
            mirror: None,
            message: None,
            items: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            items_for: None,
            needs_seen: None,
            client_mods: Arc::default(),
            _ticker: None,
            _subscriptions: subs,
        }
    }

    /// The server's panel controls, so "running" here matches the status
    /// card's.
    pub fn set_control(&mut self, control: Entity<crate::server_control::ServerControl>, cx: &mut Context<Self>) {
        self._subscriptions.push(cx.observe(&control, |this, _, cx| this.online_changed(cx)));
        self.control = Some(control);
    }

    /// Mods the server doesn't run (client-only or left off), from the Mods
    /// comparison: their config files go under Not needed.
    pub fn set_client_mods(&mut self, mods: HashSet<String>, cx: &mut Context<Self>) {
        if *self.client_mods == mods {
            return;
        }
        let shrank = !self.client_mods.is_subset(&mods);
        self.client_mods = Arc::new(mods);
        if shrank {
            if self.diff.is_some() && !self.running {
                let deep = self.diff.as_ref().is_some_and(|d| d.deep);
                self.compare(deep, cx);
            }
        } else if let Some(d) = self.diff.as_mut() {
            configdiff::mark_client_mods(Arc::make_mut(d), &self.client_mods);
            self.needs_changed(cx);
            cx.notify();
        }
    }

    fn online_changed(&mut self, cx: &mut Context<Self>) {
        let on = self.online(cx);
        if on != self.was_online {
            self.was_online = on;
            cx.notify();
        }
    }

    fn pair(&self, cx: &App) -> Option<(ServerInfo, Instance)> {
        let store = self.store.read(cx);
        let s = store.server(self.server.as_ref()?)?.clone();
        // Only the roots are used: skip copying the mod list.
        let inst = store.paired(&s)?.clone_header();
        Some((s, inst))
    }

    fn key(&self) -> String {
        self.server
            .as_ref()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Config folders on either side, plus folders added by hand.
    fn folders(&self, cx: &App) -> Vec<String> {
        let mut folders = self.present.clone().unwrap_or_default();
        for r in self
            .store
            .read(cx)
            .settings()
            .folder_rules
            .get(&self.key())
            .into_iter()
            .flatten()
        {
            if !folders.contains(&r.folder) {
                folders.push(r.folder.clone());
            }
        }
        folders
    }

    /// Opens `folder` with the file search set to `filter`, from a notice
    /// that points at these files.
    pub fn focus(&mut self, folder: &str, filter: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.filter.update(cx, |s, cx| s.set_value(filter.to_string(), window, cx));
        self.query = filter.trim().to_lowercase();
        self.folder = folder.to_string();
        self.reset(cx);
    }

    pub fn show(&mut self, server: PathBuf, cx: &mut Context<Self>) {
        if self.server.as_ref() == Some(&server) && self.diff.is_some() {
            return;
        }
        let fresh = self.server.as_ref() != Some(&server);
        self.server = Some(server.clone());
        self.was_online = self.online(cx);
        if fresh {
            self.present = None;
            self.diff = None;
        }
        let Some((s, inst)) = self.pair(cx) else {
            cx.notify();
            return;
        };
        cx.spawn(async move |this, cx| {
            let present = cx
                .background_spawn(async move { configdiff::present(&inst.root, &s.root) })
                .await;
            this.update(cx, |this, cx| {
                if this.server.as_ref() != Some(&server) {
                    return;
                }
                this.present = Some(present);
                let folders = this.folders(cx);
                if !folders.contains(&this.folder) {
                    this.folder = folders.first().cloned().unwrap_or_default();
                }
                this.reset(cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn reset(&mut self, cx: &mut Context<Self>) {
        self.diff = None;
        self.selected.clear();
        self.message = None;
        self.mirror = None;
        self.preview = None;
        self.show = None;
        if !self.folder.is_empty() {
            self.compare(false, cx);
        }
        cx.notify();
    }

    fn compare(&mut self, deep: bool, cx: &mut Context<Self>) {
        let Some((s, inst)) = self.pair(cx) else { return };
        self.running = true;
        self.preview = None;
        self.progress.0.store(0, Ordering::Relaxed);
        self.progress.1.store(0, Ordering::Relaxed);
        self.progress.2.store(PHASE_SCAN, Ordering::Relaxed);
        let (client, server, folder) = (inst.root.clone(), s.root.clone(), self.folder.clone());
        let progress = self.progress.clone();
        let client_mods = self.client_mods.clone();
        // Redraw while the content check counts up.
        self._ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(250))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        }));
        cx.spawn(async move |this, cx| {
            let f = folder.clone();
            let r = cx
                .background_spawn(async move {
                    let p = &progress;
                    let mut d = configdiff::compare(&client, &server, &f, deep, &|d, t| {
                        p.2.store(PHASE_CONTENTS, Ordering::Relaxed);
                        p.0.store(d, Ordering::Relaxed);
                        p.1.store(t, Ordering::Relaxed);
                    });
                    // Then which differences matter: reads the changed files.
                    configdiff::classify(&client, &server, &mut d, &client_mods, &|d, t| {
                        p.2.store(PHASE_READ, Ordering::Relaxed);
                        p.0.store(d, Ordering::Relaxed);
                        p.1.store(t, Ordering::Relaxed);
                    });
                    d
                })
                .await;
            this.update(cx, |this, cx| {
                if this.folder == folder {
                    this.running = false;
                    this._ticker = None;
                    this.diff = Some(Arc::new(r));
                    this.needs_changed(cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Files in Needs action for the open folder (whatever the search),
    /// for the Configs tab's count. None until compared.
    pub fn needs_count(&self) -> Option<usize> {
        let d = self.diff.as_ref()?;
        Some(d.files.iter().filter(|f| Show::Needs.groups().contains(&Grp::of(f))).count())
    }

    fn needs_changed(&mut self, cx: &mut Context<Self>) {
        let n = self.needs_count();
        if n != self.needs_seen {
            self.needs_seen = n;
            cx.emit(NeedsChanged);
        }
    }

    fn rule(&self, cx: &App) -> Rule {
        self.store
            .read(cx)
            .settings()
            .folder_rules
            .get(&self.key())
            .and_then(|v| v.iter().find(|r| r.folder == self.folder))
            .cloned()
            .unwrap_or_else(|| Rule::new(&self.folder))
    }

    fn set_rule(&mut self, rule: Rule, cx: &mut Context<Self>) {
        let key = self.key();
        self.mirror = None;
        self.store.update(cx, |s, cx| {
            s.update_settings(
                |st| {
                    let rules = st.folder_rules.entry(key).or_default();
                    match rules.iter_mut().find(|r| r.folder == rule.folder) {
                        Some(r) => *r = rule,
                        None => rules.push(rule),
                    }
                },
                cx,
            )
        });
        cx.notify();
    }

    fn add_folder(&mut self, cx: &mut Context<Self>) {
        let name = self
            .new_folder
            .read(cx)
            .value()
            .trim()
            .trim_matches(['/', '\\'])
            .to_string();
        if !configdiff::safe_rel(&name) || name.is_empty() {
            return;
        }
        self.adding = false;
        self.folder = name.clone();
        let rule = self.rule(cx);
        self.set_rule(rule, cx);
        self.reset(cx);
    }

    fn run(&mut self, act: Act, files: Vec<String>, cx: &mut Context<Self>) {
        let Some((s, inst)) = self.pair(cx) else { return };
        let folder = self.folder.clone();
        let (c_dir, s_dir) = (inst.root.join(&folder), s.root.join(&folder));
        let ops: Vec<SyncOp> = files
            .iter()
            .filter(|r| configdiff::safe_rel(r))
            .map(|r| match act {
                Act::Push => SyncOp::Copy {
                    src: c_dir.join(r),
                    dst: s_dir.join(r),
                },
                Act::Pull => SyncOp::Copy {
                    src: s_dir.join(r),
                    dst: c_dir.join(r),
                },
                Act::Delete => SyncOp::Delete {
                    path: s_dir.join(r),
                },
            })
            .collect();
        let (server, client) = (s.root.clone(), inst.root.clone());
        let deep = self.diff.as_ref().is_some_and(|d| d.deep);
        let restart = self.online(cx) && act != Act::Pull;
        self.running = true;
        for f in &files {
            self.selected.remove(f);
        }
        cx.spawn(async move |this, cx| {
            let message = cx
                .background_spawn(async move {
                    let (side, root) = if act == Act::Pull {
                        ("client", client.clone())
                    } else {
                        ("server", server.clone())
                    };
                    let results = hopper::sync::execute(ops, &root);
                    hopper::history::record(&server, side, &root, &results);
                    configdiff::forget_walk(&c_dir);
                    configdiff::forget_walk(&s_dir);
                    let failed: Vec<String> = results
                        .iter()
                        .filter(|r| !r.ok)
                        .map(|r| r.error.clone().unwrap_or_default())
                        .collect();
                    let n = results.len();
                    let files = if n == 1 { "1 file".to_string() } else { format!("{n} files") };
                    let line = match act {
                        Act::Push => format!("Sent {files} to the server"),
                        Act::Pull => format!("Copied {files} to the client"),
                        Act::Delete => format!("Moved {files} off the server, into its backups"),
                    };
                    if failed.is_empty() {
                        Ok(format!(
                            "{line}. {}Undo from History.",
                            if restart { "Restart the server to apply. " } else { "" }
                        ))
                    } else {
                        Err(format!("{} failed: {}", failed.len(), failed[0]))
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                this.message = Some(message);
                this.compare(deep, cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn confirm(&mut self, act: Act, files: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let n = files.len();
        let many = |one: &str, more: &str| if n == 1 { one.to_string() } else { more.replace("{n}", &n.to_string()) };
        let (title, body, ok) = match act {
            Act::Push => (
                many("Send 1 file to the server?", "Send {n} files to the server?"),
                "The server's copies are backed up first; undo from History.",
                "Push to server",
            ),
            Act::Pull => (
                many("Copy 1 file from the server to the client?", "Copy {n} files from the server to the client?"),
                "The client's copies are backed up first; undo from History.",
                "Copy to client",
            ),
            Act::Delete => (
                many("Move 1 file off the server?", "Move {n} files off the server?"),
                "They go to the server's backups; undo from History. Files only on the server are often its own settings.",
                "Move off server",
            ),
        };
        let view = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (view, files) = (view.clone(), files.clone());
            alert
                .title(title.clone())
                .description(body)
                .confirm()
                .ok_text(ok)
                .ok_variant(if act == Act::Delete {
                    ButtonVariant::Danger
                } else {
                    ButtonVariant::Primary
                })
                .on_ok(move |_, _, cx| {
                    view.update(cx, |this, cx| this.run(act, files.clone(), cx));
                    true
                })
        });
    }

    fn mirror_plan(&mut self, cx: &mut Context<Self>) {
        let Some((s, inst)) = self.pair(cx) else { return };
        let rule = self.rule(cx);
        self.running = true;
        cx.spawn(async move |this, cx| {
            let plan = cx
                .background_spawn(async move { fr::plan_mirror(&rule, &inst.root, &s.root) })
                .await;
            this.update(cx, |this, cx| {
                this.running = false;
                this.mirror = Some(plan);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn mirror_now(&mut self, cx: &mut Context<Self>) {
        let Some((s, inst)) = self.pair(cx) else { return };
        let Some(plan) = self.mirror.take() else { return };
        let rule = self.rule(cx);
        self.running = true;
        cx.spawn(async move |this, cx| {
            let message = cx
                .background_spawn(async move {
                    let (side, root) = if rule.pull {
                        ("client", inst.root.clone())
                    } else {
                        ("server", s.root.clone())
                    };
                    let results = hopper::sync::execute(plan.ops, &root);
                    hopper::history::record(&s.root, side, &root, &results);
                    let failed: Vec<String> = results
                        .iter()
                        .filter(|r| !r.ok)
                        .map(|r| r.error.clone().unwrap_or_default())
                        .collect();
                    if failed.is_empty() {
                        Ok(format!("Mirrored {} changes. Undo from History.", results.len()))
                    } else {
                        Err(format!("{} failed: {}", failed.len(), failed[0]))
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                this.running = false;
                this.message = Some(message);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn link(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some((s, inst)) = self.pair(cx) else { return };
        let mut rule = self.rule(cx);
        let (from, to) = rule.ends(&inst.root, &s.root);
        let r = if on {
            let backup = if rule.pull { inst.root.clone() } else { s.root.clone() };
            fr::link(&from, &to, &backup).map(|d| rule.displaced = d)
        } else {
            fr::unlink(&to, rule.displaced.as_deref()).map(|()| rule.displaced = None)
        };
        match r {
            Ok(()) => self.set_rule(rule, cx),
            Err(e) => window.push_notification(
                gpui_kit::component::notification::Notification::error(format!("{e:#}")),
                cx,
            ),
        }
    }

    /// Up, by the panel's state (the freshest) or else by ping, the same
    /// answer as the status card's.
    fn online(&self, cx: &App) -> bool {
        let Some(r) = self.server.as_ref() else { return false };
        crate::rigsync::server_online(self.store.read(cx), self.control.as_ref().map(|c| c.read(cx)), r)
    }

    // ---- the list --------------------------------------------------------

    fn matches(&self, rel: &str) -> bool {
        hopper::fuzzy::words_match(&self.query, &[rel])
    }

    /// Files per filter, after the search: what the filter tabs count.
    fn counts(&self, diff: &ConfigDiff) -> [usize; 5] {
        let mut n = [0; 5];
        for f in diff.files.iter().filter(|f| self.matches(&f.rel)) {
            let g = Grp::of(f);
            if let Some(i) = Show::ALL.iter().position(|s| s.groups().contains(&g)) {
                n[i] += 1;
            }
        }
        n[4] = diff.same_files.iter().filter(|r| self.matches(r)).count();
        n
    }

    /// The filter as it applies now.
    fn shown(&self) -> Show {
        self.show.unwrap_or_else(|| {
            let needs = self
                .diff
                .as_ref()
                .is_some_and(|d| d.files.iter().any(|f| Show::Needs.groups().contains(&Grp::of(f))));
            if needs { Show::Needs } else { Show::Same }
        })
    }

    /// The differing files the filter and search let through.
    fn visible<'a>(&self, diff: &'a ConfigDiff) -> Vec<&'a FileDiff> {
        let groups = self.shown().groups();
        diff.files
            .iter()
            .filter(|f| groups.contains(&Grp::of(f)) && self.matches(&f.rel))
            .collect()
    }

    /// Ticked files in view; ticks hidden by the filter or search don't count.
    fn picked<'a>(&self, diff: &'a ConfigDiff) -> Vec<&'a FileDiff> {
        self.visible(diff)
            .into_iter()
            .filter(|f| self.selected.contains(&f.rel))
            .collect()
    }

    /// The list's rows: per group a heading, then its files, with the open
    /// file's preview under its row.
    fn build_items(&self, diff: &ConfigDiff) -> Vec<Item> {
        let show = self.shown();
        let mut items = Vec::new();
        for &g in show.groups() {
            if g == Grp::Same {
                let rows: Vec<usize> = diff
                    .same_files
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| self.matches(r))
                    .map(|(i, _)| i)
                    .collect();
                if !rows.is_empty() {
                    items.push(Item::Head(g, rows.len()));
                    items.extend(rows.into_iter().map(Item::Same));
                }
                continue;
            }
            let files: Vec<usize> = diff
                .files
                .iter()
                .enumerate()
                .filter(|(_, f)| Grp::of(f) == g && self.matches(&f.rel))
                .map(|(i, _)| i)
                .collect();
            if files.is_empty() {
                continue;
            }
            items.push(Item::Head(g, files.len()));
            for i in files {
                items.push(Item::File(i));
                if self.preview.as_ref().is_some_and(|p| p.rel == diff.files[i].rel) {
                    items.push(Item::Preview);
                }
            }
        }
        items
    }

    // ---- rendering -------------------------------------------------------

    /// The Folder rules button and its popover: how the folder syncs
    /// (review, mirror, live link), which way, and the patterns.
    fn rules_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let rule = self.rule(cx);
        let label = match rule.mode {
            Mode::Review => "Folder rules".to_string(),
            m => format!("Folder rules: {}", m.label()),
        };
        let help = match rule.mode {
            Mode::Review => "Nothing moves until you sync: compare file by file and send or copy what you pick.",
            Mode::MirrorExcept => "Each mirror makes the destination match the source, except files matching the patterns, which stay as they are. Everything replaced or removed is backed up (undo from History).",
            Mode::MirrorOnly => "Each mirror copies only files matching the patterns and removes everything else at the destination. Everything replaced or removed is backed up (undo from History).",
            Mode::Link => "The destination folder becomes a link to the source: always identical, no second copy. What was there is moved to backups and put back on unlink. Both folders must be on this PC.",
        };
        let mode_ix = MODES.iter().position(|m| *m == rule.mode).unwrap_or(0);
        let (r1, r2) = (rule.clone(), rule.clone());
        let directed = rule.mode != Mode::Review;
        let patterns = rule.patterns.clone();
        let content = v_flex()
            .gap_3()
            .w(rems(30.))
            .child(div().text_sm().font_semibold().child(format!("How {}/ syncs", rule.folder)))
            .child(
                TabBar::new("rule-mode")
                    .segmented()
                    .small()
                    .selected_index(mode_ix)
                    .children(MODES.iter().map(|m| {
                        Tab::new().label(match m {
                            Mode::Review => "Review each file",
                            Mode::MirrorExcept => "Mirror all except…",
                            Mode::MirrorOnly => "Mirror only…",
                            Mode::Link => "Live link",
                        })
                    }))
                    .on_click(cx.listener(move |this, ix: &usize, _, cx| {
                        let mut r = r1.clone();
                        r.mode = MODES[(*ix).min(3)];
                        this.set_rule(r, cx);
                    })),
            )
            .child(widgets::caption(help, cx).text_xs().whitespace_normal())
            .when(directed, |c| {
                c.child(
                    h_flex()
                        .gap_2()
                        .child(widgets::caption("From", cx).text_xs())
                        .child(
                            TabBar::new("rule-dir")
                                .segmented()
                                .small()
                                .selected_index(usize::from(rule.pull))
                                .child(Tab::new().label("Client → server"))
                                .child(Tab::new().label("Server → client"))
                                .on_click(cx.listener(move |this, ix: &usize, _, cx| {
                                    let mut r = r2.clone();
                                    r.pull = *ix == 1;
                                    this.set_rule(r, cx);
                                })),
                        ),
                )
                .when(matches!(rule.mode, Mode::MirrorExcept | Mode::MirrorOnly), |c| {
                    c.child(widgets::caption(
                        if patterns.is_empty() { "Patterns: none".to_string() } else { format!("Patterns: {patterns}") },
                        cx,
                    ))
                })
            });
        Popover::new("folder-rules")
            .anchor(Anchor::TopRight)
            .open(self.rules_open)
            .on_open_change(cx.listener(|this, open: &bool, _, cx| {
                this.rules_open = *open;
                cx.notify();
            }))
            .trigger(
                Button::new("folder-rules-btn")
                    .small()
                    .ghost()
                    .icon(IconName::Settings)
                    .label(label)
                    .tooltip("Mirror or live-link this folder instead of reviewing it file by file"),
            )
            .child(content)
            .into_any_element()
    }

    /// Under the header when the folder mirrors or links: the controls for
    /// that, in place of the file list. None when it's reviewed.
    fn rule_controls(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let rule = self.rule(cx);
        if rule.mode == Mode::Review {
            return None;
        }
        let theme = cx.theme().clone();
        let remote = self.pair(cx).is_some_and(|(s, _)| s.remote.is_some());
        if remote {
            return Some(
                widgets::caption(
                    "Mirror and link rules need the server's folder on this PC. With Hopper Server, set Folder rules back to Review each file and sync file by file.",
                    cx,
                )
                .whitespace_normal()
                .into_any_element(),
            );
        }
        Some(match rule.mode {
            Mode::MirrorExcept | Mode::MirrorOnly => {
                let plan = self.mirror.clone();
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("mirror-preview")
                                    .small()
                                    .outline()
                                    .label("Preview mirror")
                                    .disabled(self.running)
                                    .on_click(cx.listener(|this, _, _, cx| this.mirror_plan(cx))),
                            )
                            .when_some(plan.as_ref(), |r, p| {
                                r.child(widgets::caption(
                                    format!("{} to copy, {} to delete", p.copy.len(), p.delete.len()),
                                    cx,
                                ))
                                .child(
                                    Button::new("mirror-now")
                                        .small()
                                        .primary()
                                        .label("Mirror now")
                                        .disabled(self.running || p.ops.is_empty())
                                        .on_click(cx.listener(|this, _, _, cx| this.mirror_now(cx))),
                                )
                            })
                            .when(self.running, |r| r.child(Spinner::new().small())),
                    )
                    .when_some(plan, |c, p| {
                        c.child(
                            v_flex()
                                .id("mirror-plan")
                                // A set height once long: a max height alone doesn't scroll.
                                .when(p.copy.len() + p.delete.len() > 16, |l| l.h(rems(20.)))
                                .p_3()
                                .rounded(theme.radius)
                                .bg(theme.muted)
                                .font_family(theme.mono_font_family.clone())
                                .text_xs()
                                .children(p.copy.iter().map(|f| div().child(format!("copy    {f}"))))
                                .children(p.delete.iter().map(|f| {
                                    div().text_color(theme.danger).child(format!("delete  {f}"))
                                }))
                                .overflow_y_scrollbar(),
                        )
                    })
                    .into_any_element()
            }
            _ => {
                let online = self.online(cx);
                match self.pair(cx) {
                    None => div().into_any_element(),
                    Some((s, inst)) => {
                        let (from, to) = rule.ends(&inst.root, &s.root);
                        if fr::on_network(&inst.root) || fr::on_network(&s.root) {
                            div()
                                .text_sm()
                                .text_color(theme.warning)
                                .child("Not possible here: the server is on a network drive, and a link can only point within one PC. Use a mirror rule instead.")
                                .into_any_element()
                        } else if fr::is_link(&to) {
                            h_flex()
                                .gap_2()
                                .child(div().text_sm().text_color(theme.success).child(format!(
                                    "Live-linked: {} points at {}",
                                    to.display(),
                                    from.display()
                                )))
                                .child(
                                    Button::new("unlink")
                                        .small()
                                        .outline()
                                        .label("Unlink")
                                        .disabled(online)
                                        .on_click(cx.listener(|this, _, w, cx| this.link(false, w, cx))),
                                )
                                .into_any_element()
                        } else {
                            Button::new("link")
                                .small()
                                .primary()
                                .label("Link")
                                .disabled(online)
                                .on_click(cx.listener(|this, _, w, cx| this.link(true, w, cx)))
                                .into_any_element()
                        }
                    }
                }
            }
        })
    }

    fn preview_el(&self, p: &Preview, c_dir: &Path, s_dir: &Path, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let rows = match &p.rows {
            Ok(r) => r,
            Err(e) => {
                return div().text_sm().text_color(theme.warning).child(e.clone()).into_any_element();
            }
        };
        let (c_file, s_file) = (c_dir.join(&p.rel), s_dir.join(&p.rel));
        // Diff or Values, on the preview it switches.
        let toggle = widgets::underline_tabs(
            "cfg-view",
            vec![("Values".into(), None), ("Diff".into(), None)],
            usize::from(!self.values),
            cx.listener(|this, ix: &usize, _, cx| {
                this.values = *ix == 0;
                cx.notify();
            }),
            cx,
        );
        let open_btns = h_flex()
            .gap_1()
            .child(
                Button::new("open-client-file")
                    .xsmall()
                    .ghost()
                    .label("Open client")
                    .on_click(move |_, _, _| open_path(&c_file)),
            )
            .child(
                Button::new("open-server-file")
                    .xsmall()
                    .ghost()
                    .label("Open server")
                    .on_click(move |_, _, _| open_path(&s_file)),
            );
        if self.values {
            let changed = p.fields.iter().filter(|f| f.changed()).count();
            let shown: Vec<&MergedField> =
                p.fields.iter().filter(|f| self.show_all || f.changed()).collect();
            return v_flex()
                .gap_1()
                .p_3()
                .ml_8()
                .rounded(theme.radius)
                .bg(theme.muted)
                .child(
                    h_flex()
                        .gap_2()
                        .pb_1()
                        .child(toggle)
                        .child(widgets::caption(
                            format!("{changed} of {} settings differ", p.fields.len()),
                            cx,
                        ))
                        .child(
                            Button::new("show-all")
                                .xsmall()
                                .ghost()
                                .label(if self.show_all { "Changed only" } else { "Show all" })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_all = !this.show_all;
                                    cx.notify();
                                })),
                        )
                        .child(div().flex_1())
                        .child(open_btns),
                )
                .when(p.fields.is_empty(), |c| {
                    c.child(widgets::caption("No key = value settings in this file; see Diff.", cx))
                })
                .children(shown.into_iter().take(400).map(|f| {
                    let key = if f.section.is_empty() {
                        f.key.clone()
                    } else {
                        format!("[{}] {}", f.section, f.key)
                    };
                    let value: AnyElement = match (&f.client, &f.server) {
                        (Some(c), Some(s)) if c != s => h_flex()
                            .gap_2()
                            .child(div().text_color(theme.danger).line_through().child(s.clone()))
                            .child(widgets::caption("→", cx).text_xs())
                            .child(div().text_color(theme.success).child(c.clone()))
                            .into_any_element(),
                        (Some(c), None) => h_flex()
                            .gap_2()
                            .child(div().text_color(theme.success).child(c.clone()))
                            .child(widgets::caption("not on the server", cx).text_xs())
                            .into_any_element(),
                        (None, Some(s)) => h_flex()
                            .gap_2()
                            .child(div().text_color(theme.muted_foreground).child(s.clone()))
                            .child(widgets::caption("only on the server", cx).text_xs())
                            .into_any_element(),
                        (Some(c), _) => div().text_color(theme.muted_foreground).child(c.clone()).into_any_element(),
                        (None, None) => div().into_any_element(),
                    };
                    h_flex()
                        .gap_3()
                        .font_family(theme.mono_font_family.clone())
                        .text_xs()
                        .child(div().w_64().flex_none().truncate().child(key))
                        .child(value)
                }))
                .into_any_element();
        }
        let (a, d, ch) = configdiff::stats(rows);
        let n = rows.len();
        v_flex()
            .gap_1()
            .p_3()
            .ml_8()
            .rounded(theme.radius)
            .bg(theme.muted)
            .child(
                h_flex()
                    .gap_2()
                    .pb_1()
                    .child(toggle)
                    .child(widgets::caption(format!("+{a}  -{d}  ~{ch}"), cx).text_xs())
                    .child(div().flex_1())
                    .child(open_btns),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().text_xs().font_semibold().child("Client"))
                    .child(div().flex_1().text_xs().font_semibold().child("Server")),
            )
            .child(
                // Only the lines in view are built: a big file has
                // thousands, too many to lay out on every frame.
                div()
                    .relative()
                    .h(rems(LINE_H * n.min(20) as f32))
                    .font_family(theme.mono_font_family.clone())
                    .text_xs()
                    .child(
                        uniform_list(
                            "cfg-preview",
                            n,
                            cx.processor(|this, range: Range<usize>, _, cx| this.preview_lines(range, cx)),
                        )
                        .track_scroll(&self.preview_scroll)
                        .size_full(),
                    )
                    .vertical_scrollbar(&self.preview_scroll),
            )
            .into_any_element()
    }

    /// Lines `range` of the open file's side-by-side diff.
    fn preview_lines(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(Ok(rows)) = self.preview.as_ref().map(|p| &p.rows) else {
            return Vec::new();
        };
        let theme = cx.theme();
        let side = |no: Option<usize>, text: &Option<String>, fill: Option<Hsla>| {
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .when_some(fill, |r, f| r.bg(f.opacity(0.18)))
                .child(
                    div()
                        .w_10()
                        .flex_none()
                        .text_right()
                        .text_color(theme.muted_foreground)
                        .child(no.map(|n| (n + 1).to_string()).unwrap_or_default()),
                )
                .child(
                    div()
                        .min_w_0()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .child(text.clone().unwrap_or_default().replace('\t', "    ")),
                )
        };
        rows[range.start.min(rows.len())..range.end.min(rows.len())]
            .iter()
            .map(|r| {
                let (lc, rc) = match r.kind {
                    LineKind::Same => (None, None),
                    LineKind::Add => (Some(theme.success), None),
                    LineKind::Del => (None, Some(theme.danger)),
                    LineKind::Change => (Some(theme.warning), Some(theme.warning)),
                };
                h_flex()
                    .w_full()
                    .h(rems(LINE_H))
                    .gap_2()
                    .child(side(r.client_no, &r.client, lc))
                    .child(side(r.server_no, &r.server, rc))
                    .into_any_element()
            })
            .collect()
    }

    /// The folder chips, + Folder and Folder rules.
    fn header(&self, folders: &[String], cx: &mut Context<Self>) -> AnyElement {
        let folder_ix = folders.iter().position(|f| *f == self.folder).unwrap_or(0);
        let fl = folders.to_vec();
        let needs = self.needs_count().filter(|n| *n > 0);
        let rules = self.rules_button(cx);
        h_flex()
            .gap_2()
            .flex_wrap()
            .when(!folders.is_empty(), |r| {
                let cur = self.folder.clone();
                r.child(widgets::underline_tabs(
                    "cfg-folders",
                    // The open folder shows what it needs.
                    folders.iter().map(|f| (format!("{f}/").into(), needs.filter(|_| *f == cur))).collect(),
                    folder_ix,
                    cx.listener(move |this, ix: &usize, _, cx| {
                        if let Some(f) = fl.get(*ix) {
                            this.folder = f.clone();
                            this.reset(cx);
                        }
                    }),
                    cx,
                ))
            })
            .map(|r| {
                if self.adding {
                    r.child(div().w_56().child(Input::new(&self.new_folder).small()))
                        .child(
                            Button::new("folder-add")
                                .small()
                                .outline()
                                .label("Add")
                                .on_click(cx.listener(|this, _, _, cx| this.add_folder(cx))),
                        )
                        .child(
                            Button::new("folder-cancel")
                                .small()
                                .ghost()
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.adding = false;
                                    cx.notify();
                                })),
                        )
                } else {
                    r.child(
                        Button::new("folder-new")
                            .small()
                            .ghost()
                            .icon(IconName::Plus)
                            .label("Folder")
                            .tooltip("Sync another folder, like world, resourcepacks or scripts")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.adding = true;
                                cx.notify();
                            })),
                    )
                }
            })
            .child(div().flex_1())
            .child(rules)
            .into_any_element()
    }

    /// Search, the filter tabs and the main Sync button, like the Mods
    /// section's toolbar.
    fn toolbar(&self, diff: &Arc<ConfigDiff>, cx: &mut Context<Self>) -> AnyElement {
        let counts = self.counts(diff);
        let shown = self.shown();
        let show_ix = Show::ALL.iter().position(|s| *s == shown).unwrap_or(0);
        let picked = self.picked(diff);
        let push: Vec<String> = if picked.is_empty() {
            // Nothing ticked: Sync sends what needs it (after the search).
            diff.files
                .iter()
                .filter(|f| Show::Needs.groups().contains(&Grp::of(f)) && self.matches(&f.rel))
                .map(|f| f.rel.clone())
                .collect()
        } else {
            picked.iter().filter(|f| f.status != FileStatus::OnlyServer).map(|f| f.rel.clone()).collect()
        };
        let pull: Vec<String> =
            picked.iter().filter(|f| f.status != FileStatus::OnlyClient).map(|f| f.rel.clone()).collect();
        let remove: Vec<String> =
            picked.iter().filter(|f| f.status == FileStatus::OnlyServer).map(|f| f.rel.clone()).collect();
        let (np, nl, nr) = (push.len(), pull.len(), remove.len());
        let n_sel = picked.len();
        h_flex()
            .gap_2()
            .flex_wrap()
            .child(
                div().w_48().child(
                    widgets::search_input(&self.filter)
                        .small()
                        .prefix(Icon::new(IconName::Search).small())
                        .cleanable(true),
                ),
            )
            .child(
                widgets::underline_tabs(
                    "cfg-show",
                    Show::ALL.iter().zip(counts).map(|(s, n)| (s.label().into(), Some(n))).collect(),
                    show_ix,
                    cx.listener(|this, ix: &usize, _, cx| {
                        this.show = Show::ALL.get(*ix).copied();
                        this.list.scroll_to(ListOffset { item_ix: 0, offset_in_item: px(0.) });
                        cx.notify();
                    }),
                    cx,
                ),
            )
            .child(div().flex_1())
            .when(n_sel > 0, |r| {
                r.child(
                    Button::new("sel-clear")
                        .xsmall()
                        .ghost()
                        .label("Clear selection")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.selected.clear();
                            cx.notify();
                        })),
                )
                .when(nr > 0, |r| {
                    r.child(
                        Button::new("cfg-remove")
                            .small()
                            .outline()
                            .with_variant(ButtonVariant::Danger)
                            .icon(IconName::Trash)
                            .label(format!("Move {nr} off server"))
                            .tooltip("Moves them into the server's backups (undo from History)")
                            .disabled(self.running)
                            .on_click(cx.listener(move |this, _, w, cx| this.confirm(Act::Delete, remove.clone(), w, cx))),
                    )
                })
                .when(nl > 0, |r| {
                    r.child(
                        Button::new("cfg-pull")
                            .small()
                            .outline()
                            .icon(IconName::Download)
                            .label(format!("Copy {nl} to client"))
                            .tooltip("Copies the server's file over the client's. The client's copy is backed up first.")
                            .disabled(self.running)
                            .on_click(cx.listener(move |this, _, w, cx| this.confirm(Act::Pull, pull.clone(), w, cx))),
                    )
                })
            })
            .child(
                Button::new("cfg-push")
                    .small()
                    .icon(IconName::ServerCog)
                    .label(hopper::rigsync::sync_label(np, 0).unwrap_or_else(|| "Push to server".to_string()))
                    .map(|b| if np > 0 { b.primary() } else { b.outline() })
                    .tooltip(if n_sel > 0 {
                        "Sends the ticked files to the server. The server's copies are backed up first."
                    } else {
                        "Sends every file in Needs action to the server. The server's copies are backed up first."
                    })
                    .disabled(np == 0 || self.running)
                    .on_click(cx.listener(move |this, _, w, cx| this.confirm(Act::Push, push.clone(), w, cx))),
            )
            .into_any_element()
    }

    /// The line under the toolbar: ticks the search hides, the content
    /// check, rescan.
    fn status_line(&self, diff: &Arc<ConfigDiff>, cx: &mut Context<Self>) -> AnyElement {
        let hidden = self.selected.len() - self.picked(diff).len();
        let deep = diff.deep;
        h_flex()
            .gap_2()
            .flex_wrap()
            .text_xs()
            .when(hidden > 0, |r| {
                r.child(widgets::caption(format!("{hidden} ticked but hidden by the filter or search (left out)"), cx).text_xs())
            })
            .map(|r| {
                if deep {
                    r.child(widgets::caption(format!("{} files identical (contents checked)", diff.same), cx).text_xs())
                } else if diff.same > 0 {
                    r.child(widgets::caption(format!("{} files match by size; contents not read", diff.same), cx).text_xs())
                        .child(
                            Button::new("deep")
                                .xsmall()
                                .ghost()
                                .label("Check contents")
                                .tooltip("Also read files whose size matches, to catch edits that kept the size. Slower: reads each one off the server disk. Cached afterwards.")
                                .disabled(self.running)
                                .on_click(cx.listener(|this, _, _, cx| this.compare(true, cx))),
                        )
                } else {
                    r
                }
            })
            .child(
                Button::new("cfg-rescan")
                    .xsmall()
                    .ghost()
                    .icon(IconName::RefreshCw)
                    .tooltip("Compare again")
                    .disabled(self.running)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some((s, inst)) = this.pair(cx) {
                            configdiff::forget_walk(&inst.root.join(&this.folder));
                            configdiff::forget_walk(&s.root.join(&this.folder));
                        }
                        this.compare(deep, cx)
                    })),
            )
            .into_any_element()
    }

    fn render_item(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let (Some(item), Some(diff)) = (self.items.get(ix).cloned(), self.diff.clone()) else {
            return div().into_any_element();
        };
        let Some((s, inst)) = self.pair(cx) else {
            return div().into_any_element();
        };
        let (c_dir, s_dir) = (inst.root.join(&diff.folder), s.root.join(&diff.folder));
        let theme = cx.theme().clone();
        match item {
            Item::Head(g, n) => self.group_header(g, n, &diff, cx),
            Item::Preview => match &self.preview {
                Some(p) => div().w_full().pb_2().child(self.preview_el(p, &c_dir, &s_dir, cx)).into_any_element(),
                None => div().into_any_element(),
            },
            Item::Same(i) => {
                let Some(rel) = diff.same_files.get(i).cloned() else {
                    return div().into_any_element();
                };
                h_flex()
                    .h(rems(ROW_H))
                    .w_full()
                    .gap_3()
                    .px_3()
                    .rounded(theme.radius)
                    .hover(|s| widgets::row_hover(s, cx))
                    .child(div().w_4().flex_none())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .font_family(theme.mono_font_family.clone())
                            .truncate()
                            .child(rel),
                    )
                    .child(widgets::caption(if diff.deep { "Same contents" } else { "Same size" }, cx).text_xs())
                    .into_any_element()
            }
            Item::File(i) => {
                let Some(f) = diff.files.get(i).cloned() else {
                    return div().into_any_element();
                };
                self.file_row(&f, &c_dir, &s_dir, cx)
            }
        }
    }

    fn group_header(&self, g: Grp, n: usize, diff: &ConfigDiff, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let keys: Vec<String> = if g == Grp::Same {
            Vec::new()
        } else {
            diff.files
                .iter()
                .filter(|f| Grp::of(f) == g && self.matches(&f.rel))
                .map(|f| f.rel.clone())
                .collect()
        };
        let all = !keys.is_empty() && keys.iter().all(|k| self.selected.contains(k));
        h_flex()
            .h(rems(ROW_H + 0.75))
            .w_full()
            .items_end()
            .pb_1()
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded(theme.radius_lg)
                    .bg(theme.secondary)
                    .child(div().size_2().rounded_full().bg(g.color(cx)))
                    .child(div().text_sm().font_semibold().child(g.title()))
                    .child(div().text_sm().text_color(theme.muted_foreground).child(n.to_string()))
                    .child(widgets::caption(format!("· {}", g.hint()), cx).text_xs().flex_1().min_w_0())
                    .when(keys.len() > 1, |r| {
                        r.child(
                            Checkbox::new(SharedString::from(format!("cfg-all:{g:?}")))
                                .label("Select all")
                                .checked(all)
                                .on_change(cx.listener(move |this, v: &bool, _, cx| {
                                    for k in &keys {
                                        if *v {
                                            this.selected.insert(k.clone());
                                        } else {
                                            this.selected.remove(k);
                                        }
                                    }
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .into_any_element()
    }

    fn file_row(&self, f: &FileDiff, c_dir: &Path, s_dir: &Path, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let g = Grp::of(f);
        let rel = f.rel.clone();
        let id = |p: &str| SharedString::from(format!("{p}:{rel}"));
        let picked = self.selected.contains(&f.rel);
        let open = self.preview.as_ref().is_some_and(|p| p.rel == f.rel);
        let running = self.running;
        // What differs, in a word.
        let chip = match (g, f.status) {
            (Grp::NotNeeded, _) if f.kind == Kind::Generated => widgets::chip(theme.muted_foreground).child("written by the mod"),
            (Grp::NotNeeded, _) if f.rel.to_lowercase().ends_with(".zip") => {
                widgets::chip(theme.muted_foreground).child("client resource")
            }
            (Grp::NotNeeded, _) => widgets::chip(theme.muted_foreground).child("client settings"),
            (Grp::Format, _) => widgets::chip(theme.muted_foreground).child("comments only"),
            (_, FileStatus::OnlyClient) => widgets::chip(theme.warning).child("new file"),
            (_, FileStatus::OnlyServer) => widgets::chip(theme.muted_foreground).child("server only"),
            _ if f.binary => widgets::chip(theme.info).child("binary"),
            _ => match f.settings {
                Some(1) => widgets::chip(theme.info).child("1 setting"),
                Some(n) if n > 1 => widgets::chip(theme.info).child(format!("{n} settings")),
                _ => widgets::chip(theme.info).child("changed"),
            },
        };
        // One action per row, named for what it does; the rarer one in ⋯.
        let push = |label: &'static str, tip: &'static str| {
            let r = rel.clone();
            Button::new(id("push"))
                .small()
                .outline()
                .icon(IconName::Upload)
                .label(label)
                .tooltip(tip)
                .disabled(running)
                .on_click(cx.listener(move |this, _, _, cx| this.run(Act::Push, vec![r.clone()], cx)))
        };
        let pull_btn = |label: &'static str| {
            let r = rel.clone();
            Button::new(id("pull"))
                .small()
                .outline()
                .icon(IconName::Download)
                .label(label)
                .tooltip("Copies the server's file to the client (the client's copy is backed up first)")
                .disabled(running)
                .on_click(cx.listener(move |this, _, _, cx| this.run(Act::Pull, vec![r.clone()], cx)))
        };
        let action: AnyElement = match (g, f.status) {
            (_, FileStatus::OnlyServer) => pull_btn("Copy to client").into_any_element(),
            (Grp::Changed, _) => push("Send to server", "Puts the client's file on the server (the server's copy is backed up first)").into_any_element(),
            (Grp::Missing, _) => push("Add to server", "Copies the file to the server").into_any_element(),
            _ => push("Send anyway", "A server doesn't need it, but you can send it (backed up first)").into_any_element(),
        };
        // ⋯: the other direction, or moving a server-only file off.
        let view = cx.entity();
        let (r_pull, r_del) = (rel.clone(), rel.clone());
        let status = f.status;
        let more = (status != FileStatus::OnlyClient).then(|| {
            div().id(id("more-wrap")).child(
                Button::new(id("more"))
                    .small()
                    .ghost()
                    .icon(IconName::Ellipsis)
                    .tooltip("More")
                    .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                        let (v1, v2, rp, rd) = (view.clone(), view.clone(), r_pull.clone(), r_del.clone());
                        if status == FileStatus::OnlyServer {
                            menu.item(
                                PopupMenuItem::new("Move off server")
                                    .icon(IconName::Trash)
                                    .disabled(running)
                                    .on_click(move |_, window, cx| {
                                        v2.update(cx, |this, cx| this.confirm(Act::Delete, vec![rd.clone()], window, cx))
                                    }),
                            )
                        } else {
                            menu.item(
                                PopupMenuItem::new("Use the server's copy")
                                    .icon(IconName::Download)
                                    .disabled(running)
                                    .on_click(move |_, _, cx| v1.update(cx, |this, cx| this.run(Act::Pull, vec![rp.clone()], cx))),
                            )
                        }
                    }),
            )
        });
        let (r1, r2, st) = (rel.clone(), rel.clone(), f.status);
        let (c_dir, s_dir) = (c_dir.to_path_buf(), s_dir.to_path_buf());
        let viewable = !f.binary && !f.rel.to_lowercase().ends_with(".zip") && !f.rel.to_lowercase().ends_with(".jar");
        h_flex()
            .id(id("cfgrow"))
            .h(rems(ROW_H))
            .w_full()
            .gap_3()
            .px_3()
            .rounded(theme.radius)
            .when(picked, |r| widgets::row_selected(r, cx))
            .when(!picked, |r| r.hover(|s| widgets::row_hover(s, cx)))
            .child(
                Checkbox::new(id("cfgsel"))
                    .checked(picked)
                    .on_change(cx.listener(move |this, v: &bool, _, cx| {
                        if *v {
                            this.selected.insert(r1.clone());
                        } else {
                            this.selected.remove(&r1);
                        }
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .font_family(theme.mono_font_family.clone())
                    .truncate()
                    .child(f.rel.clone()),
            )
            .child(h_flex().w(rems(7.)).flex_none().justify_end().child(chip))
            .child(
                Button::new(id("view"))
                    .small()
                    .ghost()
                    .label(if open { "Hide" } else { "View" })
                    .when(!viewable, |b| b.disabled(true).tooltip("Not a text file"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.preview.as_ref().is_some_and(|p| p.rel == r2) {
                            this.preview = None;
                        } else {
                            let p = load_preview(&r2, st, &c_dir, &s_dir);
                            // Values for settings files, else the diff.
                            this.values = !p.fields.is_empty();
                            this.show_all = st != FileStatus::Modified;
                            this.preview = Some(p);
                            this.preview_scroll = UniformListScrollHandle::new();
                        }
                        cx.notify();
                    })),
            )
            .child(h_flex().w(rems(9.5)).flex_none().justify_end().child(action))
            .child(div().w_8().flex_none().children(more))
            .into_any_element()
    }
}

impl Render for ConfigsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(_) = self.pair(cx) else {
            return div().into_any_element();
        };
        let folders = self.folders(cx);
        let online = self.online(cx);
        let header = self.header(&folders, cx);
        if folders.is_empty() {
            return v_flex()
                .gap_3()
                .child(header)
                .child(if self.present.is_none() {
                    widgets::skeleton_rows(6, rems(ROW_H), cx).into_any_element()
                } else {
                    widgets::empty_state(
                        IconName::Settings,
                        "No config folders yet",
                        Some("Neither side has a config folder. Add one with + Folder.".into()),
                        cx,
                    )
                    .into_any_element()
                })
                .into_any_element();
        }
        let theme = cx.theme().clone();
        let message: Option<AnyElement> = self.message.as_ref().map(|m| match m {
            Ok(t) => h_flex()
                .gap_2()
                .text_sm()
                .text_color(theme.success)
                .child(Icon::new(IconName::CircleCheck).small())
                .child(t.clone())
                .into_any_element(),
            Err(t) => div().text_sm().text_color(theme.danger).child(t.clone()).into_any_element(),
        });
        let rule_controls = self.rule_controls(cx);
        let mut body = v_flex()
            .flex_1()
            .min_h_0()
            .gap_3()
            .child(header)
            .when(online, |c| {
                c.child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .text_color(theme.warning)
                        .child(Icon::new(IconName::TriangleAlert).small())
                        .child("Server is running: mods read configs at startup, so changes apply after a restart."),
                )
            })
            .children(message);
        // A folder that mirrors or links shows those controls instead.
        if let Some(c) = rule_controls {
            return body.child(c).into_any_element();
        }
        let progress_line = self.running.then(|| {
            let (d, t, phase) = (
                self.progress.0.load(Ordering::Relaxed),
                self.progress.1.load(Ordering::Relaxed),
                self.progress.2.load(Ordering::Relaxed),
            );
            h_flex()
                .gap_2()
                .child(Spinner::new().small())
                .child(widgets::caption(
                    match phase {
                        PHASE_READ => format!("Reading changed files {d}/{t}"),
                        PHASE_CONTENTS if t > 0 => format!("Checking contents {d}/{t}"),
                        _ => "Scanning folders (the server disk can be slow)".into(),
                    },
                    cx,
                ))
        });
        let Some(diff) = self.diff.clone() else {
            return body
                .children(progress_line)
                .child(widgets::skeleton_rows(8, rems(ROW_H), cx))
                .into_any_element();
        };
        let toolbar = self.toolbar(&diff, cx);
        let status = self.status_line(&diff, cx);
        body = body.child(toolbar).child(status).children(progress_line);
        // Only the rows in view are built: a folder can differ in hundreds
        // of files, too many to lay out on every scroll frame.
        let items = self.build_items(&diff);
        let show = self.shown();
        let fresh = !self.items_for.as_ref().is_some_and(|(d, s)| Arc::ptr_eq(d, &diff) && *s == show);
        if fresh {
            self.list.reset(items.len());
            self.items_for = Some((diff.clone(), show));
        } else if items != self.items {
            // Splice just what changed, so the list keeps its scroll spot.
            let old = &self.items;
            let pre = old.iter().zip(&items).take_while(|(a, b)| a == b).count();
            let max_suf = old.len().min(items.len()) - pre;
            let suf = old
                .iter()
                .rev()
                .zip(items.iter().rev())
                .take(max_suf)
                .take_while(|(a, b)| a == b)
                .count();
            self.list.splice(pre..old.len() - suf, items.len() - pre - suf);
        }
        let empty = items.is_empty();
        self.items = items;
        if empty {
            let counts = self.counts(&diff);
            let hidden = counts[2] + counts[3];
            let (title, body_text): (String, String) = match show {
                Show::Needs => (
                    format!("Nothing to sync in {}/", diff.folder),
                    if hidden > 0 {
                        format!("{hidden} differing files don't matter to a server (Not needed, Comments only).")
                    } else {
                        "The server has what the pack has.".into()
                    },
                ),
                Show::Same => ("No identical files".into(), String::new()),
                _ => ("Nothing here".into(), String::new()),
            };
            return body
                .child(widgets::empty_state(
                    IconName::CircleCheck,
                    title,
                    (!body_text.is_empty()).then(|| body_text.into()),
                    cx,
                ))
                .into_any_element();
        }
        body.child(
            div()
                .flex_1()
                .min_h_0()
                .relative()
                .child(
                    list(
                        self.list.clone(),
                        cx.processor(|this, ix: usize, _, cx| this.render_item(ix, cx)),
                    )
                    .size_full(),
                )
                .vertical_scrollbar(&self.list),
        )
        .into_any_element()
    }
}

// ---- History -------------------------------------------------------------

pub struct HistoryPanel {
    server: Option<PathBuf>,
    running: bool,
    errors: Vec<String>,
    open: HashSet<u64>,
}

impl HistoryPanel {
    pub fn new(_: &mut Window, _: &mut Context<Self>) -> Self {
        Self {
            server: None,
            running: false,
            errors: Vec::new(),
            open: HashSet::new(),
        }
    }

    pub fn show(&mut self, server: PathBuf, cx: &mut Context<Self>) {
        self.server = Some(server);
        cx.notify();
    }

    fn apply(&mut self, ids: Vec<u64>, undo: bool, cx: &mut Context<Self>) {
        self.running = true;
        self.errors.clear();
        cx.spawn(async move |this, cx| {
            let errors = cx
                .background_spawn(async move { hopper::history::apply(&ids, undo) })
                .await;
            this.update(cx, |this, cx| {
                this.running = false;
                this.errors = errors;
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}

impl Render for HistoryPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(server) = self.server.clone() else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let changes = hopper::history::for_server(&server);
        let mut batches: Vec<(u64, Vec<hopper::history::Change>)> = Vec::new();
        for c in changes {
            match batches.last_mut() {
                Some((b, list)) if *b == c.batch => list.push(c),
                _ => batches.push((c.batch, vec![c])),
            }
        }
        let empty = batches.is_empty();
        let cards = batches.into_iter().map(|(batch, list)| {
            let first = list[0].clone();
            let all_undone = list.iter().all(|c| c.undone);
            let any_live = list.iter().any(|c| !c.undone);
            let ids: Vec<u64> = list.iter().map(|c| c.id).collect();
            let ids2 = ids.clone();
            let open = self.open.contains(&batch) || list.len() <= 6;
            v_flex()
                .gap_2()
                .p_3()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .gap_3()
                        .child(div().text_sm().font_semibold().child(hopper::time::relative(&first.time)))
                        .child(widgets::caption(
                            match list.len() {
                                1 => format!("1 file on the {}", first.side),
                                n => format!("{n} files on the {}", first.side),
                            },
                            cx,
                        ))
                        .child(div().flex_1())
                        .when(list.len() > 6, |r| {
                            r.child(
                                Button::new(SharedString::from(format!("files:{batch}")))
                                    .xsmall()
                                    .ghost()
                                    .label(if open { "Hide files" } else { "Show files" })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if !this.open.remove(&batch) {
                                            this.open.insert(batch);
                                        }
                                        cx.notify();
                                    })),
                            )
                        })
                        .when(any_live, |r| {
                            r.child(
                                Button::new(SharedString::from(format!("undoall:{batch}")))
                                    .xsmall()
                                    .outline()
                                    .label("Undo all")
                                    .disabled(self.running)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.apply(ids.clone(), true, cx)
                                    })),
                            )
                        })
                        .when(all_undone, |r| {
                            r.child(
                                Button::new(SharedString::from(format!("redoall:{batch}")))
                                    .xsmall()
                                    .outline()
                                    .label("Redo all")
                                    .disabled(self.running)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.apply(ids2.clone(), false, cx)
                                    })),
                            )
                        }),
                )
                .when(open, |c| {
                    c.children(list.into_iter().map(|ch| {
                        let tag = match ch.action {
                            hopper::history::Action::Copied if ch.backup.is_some() => {
                                widgets::chip(cx.theme().info).xsmall().child("Replaced")
                            }
                            hopper::history::Action::Copied => widgets::chip(cx.theme().success).xsmall().child("Added"),
                            hopper::history::Action::Removed => widgets::chip(cx.theme().danger).xsmall().child("Removed"),
                        };
                        let can_undo = ch.action == hopper::history::Action::Copied || ch.backup.is_some();
                        let id = ch.id;
                        h_flex()
                            .gap_3()
                            .child(div().w_20().flex_none().child(tag))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .font_family(theme.mono_font_family.clone())
                                    .truncate()
                                    .when(ch.undone, |d| d.line_through().text_color(theme.muted_foreground))
                                    .child(ch.label.clone()),
                            )
                            .child(if ch.undone {
                                Button::new(SharedString::from(format!("redo:{id}")))
                                    .xsmall()
                                    .ghost()
                                    .label("Redo")
                                    .disabled(self.running)
                                    .on_click(cx.listener(move |this, _, _, cx| this.apply(vec![id], false, cx)))
                            } else {
                                Button::new(SharedString::from(format!("undo:{id}")))
                                    .xsmall()
                                    .ghost()
                                    .label("Undo")
                                    .tooltip(if can_undo { "Put back what was there" } else { "No backup was kept" })
                                    .disabled(self.running || !can_undo)
                                    .on_click(cx.listener(move |this, _, _, cx| this.apply(vec![id], true, cx)))
                            })
                    }))
                })
        });
        if empty {
            return widgets::empty_state(
                IconName::Clock,
                "Nothing synced yet",
                Some("Every file Hopper sends to or removes from this server shows here, with Undo.".into()),
                cx,
            )
            .into_any_element();
        }
        let s2 = server.clone();
        v_flex()
            .flex_1()
            .min_h_0()
            .gap_3()
            .child(
                h_flex()
                    .gap_3()
                    .child(widgets::caption(
                        "Every file Hopper copied or removed for this server. Undo puts back what was there before, from the backup taken at the time.",
                        cx,
                    ).flex_1())
                    .when(self.running, |r| r.child(Spinner::new().small()))
                    .when(!empty, |r| {
                        r.child(
                            Button::new("clear-history")
                                .xsmall()
                                .ghost()
                                .label("Clear history")
                                .on_click(cx.listener(move |_, _, window, cx| {
                                    let s = s2.clone();
                                    window.open_alert_dialog(cx, move |alert, _, _| {
                                        let s = s.clone();
                                        alert
                                            .title("Forget this server's history?")
                                            .description("Backups stay on disk.")
                                            .confirm()
                                            .ok_text("Forget")
                                            .on_ok(move |_, _, _| {
                                                hopper::history::clear(&s);
                                                true
                                            })
                                    });
                                })),
                        )
                    }),
            )
            .children(self.errors.iter().map(|e| div().text_sm().text_color(theme.danger).child(e.clone())))
            .child(
                v_flex()
                    .id("history")
                    .flex_1()
                    .min_h_0()
                    .gap_2()
                    .children(cards)
                    .overflow_y_scrollbar(),
            )
            .into_any_element()
    }
}
