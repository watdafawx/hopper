//! The modpack update preview: what the update will do to mods and config
//! files, with a choice per file where both you and the pack changed it.

use crate::widgets::Clickable as _;
use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::cf;
use hopper::instances::Instance;
use hopper::packdiff::{FileAction, ModAction, UpdatePlan};
use hopper::server::ServerInfo;
use hopper::server_pack_update::ServerPlan;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::widgets::{DIALOG_CHROME_FOOTER as DIALOG_CHROME, DIALOG_TOP};

/// Collapsible groups of the preview.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Group {
    Files(FileAction),
}

/// The summary lines that open a list (one at a time).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Mods,
    Files,
    Dropped,
    Stays,
    Edited,
}

/// Rows a list draws before "…N more".
const LIST_CAP: usize = 300;

/// The bulk choice for files both you and the pack changed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Bulk {
    /// Pack content takes the pack's copy, settings keep yours.
    Suggested,
    Mine,
    Pack,
    /// Picked per file.
    Review,
}

/// Finished comparisons by the job that made them: (instance, pack file,
/// result), so the task list can reopen a review whose dialog was closed.
type Review = (PathBuf, cf::File, Result<UpdatePlan, String>);
static REVIEWS: std::sync::Mutex<Option<HashMap<u64, Review>>> = std::sync::Mutex::new(None);

/// Server comparisons by job, from the moment they start: (what is
/// compared, the result once there). The task list reopens one while it is
/// still downloading, and the dialog follows the job instead of owning it.
/// The pack versions, the plan, and the server it was made on (the share's
/// server, or the same folder on its Hopper Server).
type ServerResult = Result<(Vec<cf::File>, ServerPlan, ServerInfo), String>;
type ServerReview = (ServerTarget, Option<ServerResult>);
static SERVER_REVIEWS: std::sync::Mutex<Option<HashMap<u64, ServerReview>>> = std::sync::Mutex::new(None);

fn server_reviews<T>(f: impl FnOnce(&mut HashMap<u64, ServerReview>) -> T) -> T {
    let mut g = SERVER_REVIEWS.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(HashMap::new))
}

/// Whether job `id` has a review to open: a finished pack comparison, or
/// a server comparison, finished or still running.
pub fn has_review(id: u64, done: bool) -> bool {
    (done && REVIEWS.lock().ok().and_then(|g| g.as_ref().map(|m| m.contains_key(&id))).unwrap_or(false))
        || server_reviews(|m| m.contains_key(&id))
}

/// Opens the review job `id` made (or is making), without comparing again.
pub fn reopen(store: Entity<Store>, id: u64, window: &mut Window, cx: &mut App) {
    if let Some((t, _)) = server_reviews(|m| m.get(&id).cloned()) {
        show_server(store, t, Some(id), window, cx);
        return;
    }
    let review = REVIEWS.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(&id).cloned()));
    if let Some((root, file, plan)) = review {
        show(store, root, file, Some(plan), window, cx);
    }
}

/// Server mode: a server updated from its pack's server pack.
#[derive(Clone)]
struct ServerTarget {
    server: ServerInfo,
    inst: Instance,
    /// A newer pack version than the instance's, when there is one.
    latest: Option<cf::File>,
    /// Pack versions to pick from (newest first), once looked up.
    targets: Vec<cf::File>,
    pick: usize,
    splan: Option<ServerPlan>,
    /// Where `splan` was made, when not `server` itself: the same folder on
    /// its Hopper Server, which downloads and applies the update there.
    via: Option<ServerInfo>,
    /// The comparison job this dialog follows.
    job: Option<u64>,
    /// That job's detail and progress while it runs.
    status: Option<(String, Option<f32>)>,
}

pub struct UpdatePreview {
    store: Entity<Store>,
    /// A choice dragged down a list (Keep mine… on several rows).
    paint: crate::drag_pick::Paint,
    /// The folder being updated: the instance, or the server.
    root: PathBuf,
    server: Option<ServerTarget>,
    from: String,
    plan: Option<Result<UpdatePlan, String>>,
    /// Conflicting files where the pack's copy wins.
    take: HashSet<String>,
    /// How the conflicting files were picked, all at once or per file.
    bulk: Bulk,
    /// Mods you turned off to turn on, and removed ones to add back (their
    /// keys); the rest stay off or out.
    turn_on: HashSet<String>,
    /// A mod's page, beside the lists, once one was clicked.
    peek: Option<Entity<crate::browse::BrowseView>>,
    _peek_watch: Option<Subscription>,
    open: HashSet<Group>,
    /// Diffs shown under conflicting files (`None` while not text).
    diffs: HashMap<String, Option<String>>,
    /// (Minecraft now, Minecraft after) when the update changes it.
    mc_change: Option<(String, String)>,
    /// Back up every world before updating (on when Minecraft changes).
    backup_worlds: bool,
    /// Reinstalling the installed version: compared without a base, so
    /// every file you changed is a conflict you can reset to the pack's.
    reinstall: bool,
    /// Compare with your changes (downloads the installed version too).
    /// Off: a quick update, like the CurseForge app.
    compare: bool,
    /// The pack version updated to (instance mode), to compare again.
    file: Option<cf::File>,
    /// The summary line whose list is open.
    section: Option<Section>,
    /// The open list's filter tab, and (What stays) which tab.
    filter: usize,
    /// Folders opened in a file list.
    folders: HashSet<String>,
    search: Entity<InputState>,
    query: String,
    /// Dropped mods one of yours needs that you keep (file, lower case).
    keep_needed: HashSet<String>,
    _search_sub: Subscription,
}

impl UpdatePreview {
    fn new(
        store: Entity<Store>,
        root: PathBuf,
        server: Option<ServerTarget>,
        from: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let sub = cx.subscribe_in(&search, window, |this, state, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.query = state.read(cx).value().trim().to_lowercase();
                cx.notify();
            }
        });
        UpdatePreview {
            store,
            root,
            server,
            from,
            plan: None,
            take: HashSet::new(),
            bulk: Bulk::Pack,
            turn_on: HashSet::new(),
            paint: Default::default(),
            peek: None,
            _peek_watch: None,
            open: HashSet::new(),
            diffs: HashMap::new(),
            mc_change: None,
            backup_worlds: false,
            reinstall: false,
            compare: false,
            file: None,
            section: None,
            filter: 0,
            folders: HashSet::new(),
            search,
            query: String::new(),
            keep_needed: HashSet::new(),
            _search_sub: sub,
        }
    }

    /// A fresh plan's state: picks at their defaults, lists closed.
    fn loaded(&mut self, p: &UpdatePlan, cx: &mut Context<Self>) {
        self.set_defaults(p);
        self.turn_on = p.default_choices();
        self.compare = p.compared;
        self.keep_needed.clear();
        self.want_icons(p, cx);
    }

    /// Switches between the quick update and comparing with your changes,
    /// and plans again.
    fn set_compare(&mut self, on: bool, cx: &mut Context<Self>) {
        self.compare = on;
        self.section = None;
        self.open.clear();
        self.diffs.clear();
        if self.server.is_some() {
            if let Some(old) = self.server.as_mut().and_then(|t| t.job.take()) {
                server_reviews(|m| m.remove(&old));
                self.store.read(cx).jobs().cancel(old);
            }
            self.plan_server(cx);
        } else if let Some(f) = self.file.clone() {
            self.plan = None;
            self.plan(f, cx);
        }
        cx.notify();
    }

    fn plan(&mut self, file: cf::File, cx: &mut Context<Self>) {
        let Some(inst) = self.store.read(cx).instance(&self.root).cloned() else {
            return;
        };
        let compare = self.compare;
        let job = self
            .store
            .read(cx)
            .jobs()
            .start(format!("Comparing {} with {}", inst.name, file.display_name));
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move {
                    let r = hopper::packdiff::plan(&inst, &file, compare, &job);
                    job.finish(&r.as_ref().map(|_| ()).map_err(|e| anyhow::anyhow!("{e:#}")));
                    if let Ok(p) = &r {
                        // Kept even if the dialog closes meanwhile: the
                        // task reopens it.
                        if let Ok(mut g) = REVIEWS.lock() {
                            g.get_or_insert_with(HashMap::new)
                                .insert(job.id(), (inst.root.clone(), file.clone(), Ok(p.clone())));
                        }
                        job.detail("Ready to review: click to open");
                    }
                    r.map_err(|e| format!("{e:#}"))
                })
                .await;
            this.update(cx, |this, cx| {
                if let Ok(p) = &r {
                    // Another mode was picked meanwhile: its plan is coming.
                    if p.compared != this.compare {
                        return;
                    }
                    this.loaded(p, cx);
                }
                this.plan = Some(r);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Server mode: looks up the pack versions (first time), then compares
    /// the picked one's server pack with the server.
    fn plan_server(&mut self, cx: &mut Context<Self>) {
        let panels = self.store.read(cx).settings().hopper_panels.clone();
        let compare = self.compare;
        let Some(t) = self.server.as_mut() else { return };
        t.splan = None;
        t.via = None;
        t.status = None;

        self.plan = None;
        self.diffs.clear();
        let jobs = self.store.read(cx).jobs().clone();
        let job = jobs.start(format!("Comparing {} with its server pack", t.server.name));
        let id = job.id();
        t.job = Some(id);
        let t = t.clone();
        server_reviews(|m| m.insert(id, (t.clone(), None)));
        // Runs on its own: closing the dialog doesn't stop it.
        cx.background_spawn(async move {
            let r = (|| {
                let targets = if t.targets.is_empty() {
                    hopper::server_pack_update::targets(&t.inst, t.latest.clone())?
                } else {
                    t.targets.clone()
                };
                let pick = t.pick.min(targets.len() - 1);
                // A server reached through the share that its Hopper Server
                // also runs: the server's PC downloads and compares.
                let via = hopper::hopperd::twin(&t.server, &panels);
                if via.is_some() {
                    job.detail("Comparing on the Hopper Server");
                }
                let server = via.unwrap_or_else(|| t.server.clone());
                let p = hopper::server_pack_update::plan(&server, &t.inst, &targets[pick], compare, &job)?;
                Ok::<_, anyhow::Error>((targets, p, server))
            })();
            job.finish(&r.as_ref().map(|_| ()).map_err(|e| anyhow::anyhow!("{e:#}")));
            match r {
                Ok(r) => {
                    server_reviews(|m| m.get_mut(&id).map(|e| e.1 = Some(Ok(r))));
                    job.detail("Ready to review: click to open");
                }
                // A failed job isn't reopened; the open dialog shows why.
                Err(e) => {
                    server_reviews(|m| m.get_mut(&id).map(|e2| e2.1 = Some(Err(format!("{e:#}")))));
                }
            }
        })
        .detach();
        self.follow(cx);
    }

    /// Shows the followed job's progress until its result is in.
    fn follow(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.server.as_ref().and_then(|t| t.job) else { return };
        cx.spawn(async move |this, cx| {
            loop {
                let done = this
                    .update(cx, |this, cx| {
                        let Some(t) = this.server.as_mut() else { return true };
                        if t.job != Some(id) {
                            return true; // another version was picked
                        }
                        let state = this.store.read(cx).jobs().snapshot().into_iter().find(|j| j.id == id);
                        t.status = state.map(|j| (j.detail, j.progress));
                        let result = server_reviews(|m| m.get(&id).and_then(|e| e.1.clone()));
                        if let Some(r) = result {
                            this.take_result(id, r);
                            if let Some(Ok(p)) = this.plan.clone() {
                                this.loaded(&p, cx);
                            }
                        }
                        cx.notify();
                        this.plan.is_some()
                    })
                    .unwrap_or(true);
                if done {
                    break;
                }
                cx.background_executor().timer(std::time::Duration::from_millis(300)).await;
            }
        })
        .detach();
    }

    fn take_result(&mut self, id: u64, r: ServerResult) {
        match r {
            Ok((targets, p, on)) => {
                self.from = p.base.clone().unwrap_or_else(|| "this server".into());
                self.plan = Some(Ok(p.plan.clone()));
                if let Some(t) = self.server.as_mut() {
                    t.targets = targets;
                    t.splan = Some(p);
                    t.via = (on.root != t.server.root).then_some(on);
                    t.status = None;
                }
            }
            Err(e) => {
                server_reviews(|m| m.remove(&id));
                self.plan = Some(Err(e));
            }
        }
    }

    fn pick_target(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(t) = self.server.as_mut() else { return };
        if t.pick == ix || t.targets.len() <= ix {
            return;
        }
        t.pick = ix;
        // The other version's comparison is no longer wanted.
        if let Some(old) = t.job.take() {
            server_reviews(|m| m.remove(&old));
            self.store.read(cx).jobs().cancel(old);
        }
        self.plan_server(cx);
        cx.notify();
    }

    fn apply_server(view: &Entity<Self>, cx: &mut App) -> bool {
        let v = view.read(cx);
        let Some(t) = &v.server else { return false };
        let Some(mut p) = t.splan.clone() else { return false };
        // Dropped mods one of the server's own mods needs, kept.
        p.plan
            .mods
            .retain(|m| !(m.action == ModAction::Remove && v.keep_needed.contains(&m.from.to_lowercase())));
        if let Some(id) = t.job {
            server_reviews(|m| m.remove(&id));
        }
        // Applied where it was compared.
        let server = t.via.clone().unwrap_or_else(|| t.server.clone());
        let (take, client_root) = (v.take.clone(), t.inst.root.clone());
        let s = v.store.read(cx);
        let host = s.settings().server_host.clone();
        let java = hopper::launch::installer_java(std::path::Path::new(&s.settings().launcher_dir));
        let panel = hopper::panel::for_server(s.settings(), &server);
        s.spawn_job(format!("{} → {}", server.name, p.plan.file.display_name), move |job| {
            job.announce();
            let r = hopper::server_pack_update::apply(&server, &p, &take, &host, &java, panel.as_deref(), job)?;
            job.detail(r.summary());
            if let Some(b) = &r.backups {
                job.note(format!("Replaced and removed files: {}", b.display()));
            }
            // Server packs often leave out global datapacks and the
            // configs of mods new to the pack.
            if let Ok(jars) = hopper::rigsync::server_mods(&server) {
                let mods: Vec<(String, String)> = jars
                    .into_iter()
                    .filter(|m| m.side != "client" && !m.mod_id.is_empty())
                    .map(|m| (m.mod_id, m.mod_name))
                    .collect();
                let d = hopper::configdiff::drift(&client_root, &server.root, &mods);
                if !d.is_empty() {
                    job.note(format!(
                        "Still different from your instance: {}. Review them in Server sync.",
                        d.summary()
                    ));
                }
            }
            Ok(crate::store::Msg::ServersChanged)
        });
        true
    }

    /// Server mode: the version choice, loader change and caveats.
    fn server_notes(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = self.server.as_ref()?;
        let theme = cx.theme();
        let warn = |text: String| {
            h_flex()
                .gap_2()
                .text_sm()
                .text_color(theme.warning)
                .child(Icon::new(IconName::TriangleAlert).small())
                .child(text)
        };
        let inst_file = t.inst.pack.as_ref().map_or(0, |p| p.file_id);
        let mut c = v_flex().gap_2();
        if t.targets.len() > 1 {
            let labels: Vec<String> = t
                .targets
                .iter()
                .map(|f| {
                    let whose = if f.id == inst_file { "your instance" } else { "latest" };
                    format!("{} ({whose})", f.display_name)
                })
                .collect();
            c = c.child(
                h_flex()
                    .gap_3()
                    .child(div().text_sm().child("Pack version"))
                    .child(
                        TabBar::new("server-target")
                            .segmented()
                            .xsmall()
                            .selected_index(t.pick)
                            .children(labels.into_iter().map(|l| Tab::new().label(l)))
                            .on_click(cx.listener(|this, ix: &usize, _, cx| this.pick_target(*ix, cx))),
                    ),
            );
        }
        if let Some(f) = t.targets.get(t.pick).filter(|f| f.id != inst_file) {
            c = c.child(warn(format!(
                "Your instance is on another pack version. Players need {} to join: update the instance too.",
                f.display_name
            )));
        }
        if let Some(p) = &t.splan {
            if let Some((from, to)) = &p.loader {
                c = c.child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .child(Icon::new(IconName::ServerCog).small().text_color(theme.info))
                        .child(format!("NeoForge {from} → {to}: installed on the server, run.bat pointed at it.")),
                );
            }
            if let Some(n) = &p.mods_note {
                c = c.child(warn(n.clone()));
            }
            if p.plan.base_known && p.base_assumed {
                let base = p.base.clone().unwrap_or_default();
                c = c.child(
                    widgets::caption(
                        format!("No record of the server pack this server came from, so it's compared with {base} (your instance's version)."),
                        cx,
                    )
                    .text_xs(),
                );
            }
        }
        Some(c.into_any_element())
    }

    /// The plan's default picks for conflicting files. The per-file list
    /// starts folded behind the bulk choice.
    fn set_defaults(&mut self, p: &UpdatePlan) {
        self.take = p.default_take();
        let n = p.count(FileAction::Conflict);
        // A quick update: the pack's copy, like the CurseForge app.
        self.bulk = if !p.base_known {
            Bulk::Pack
        } else if self.take.is_empty() {
            Bulk::Mine
        } else if self.take.len() == n {
            Bulk::Pack
        } else {
            Bulk::Suggested
        };
    }

    /// Picks every conflicting file at once, or unfolds the list to pick
    /// each.
    fn pick_bulk(&mut self, b: Bulk, cx: &mut Context<Self>) {
        let Some(Ok(plan)) = &self.plan else { return };
        let all: Vec<String> = plan
            .files
            .iter()
            .filter(|f| f.action == FileAction::Conflict)
            .map(|f| f.rel.clone())
            .collect();
        match b {
            Bulk::Suggested => self.take = plan.default_take(),
            Bulk::Mine => {
                for r in &all {
                    self.take.remove(r);
                }
            }
            // Without a base, your own settings (options.txt…) stay.
            Bulk::Pack if !plan.base_known => self.take = plan.default_take(),
            Bulk::Pack => self.take.extend(all),
            Bulk::Review => {
                self.open.insert(Group::Files(FileAction::Conflict));
            }
        }
        self.bulk = b;
        cx.notify();
    }

    /// "Your choice": what happens to the pack files that differ from
    /// yours, all at once or per file, with the per-file list folded below.
    /// A quick update offers to compare with your changes; a compared one
    /// to go back.
    fn choice_card(&self, plan: &UpdatePlan, cx: &mut Context<Self>) -> Option<AnyElement> {
        let rows: Vec<String> = plan
            .files
            .iter()
            .filter(|f| f.action == FileAction::Conflict)
            .map(|f| f.rel.clone())
            .collect();
        let n = rows.len();
        let server = self.server.is_some();
        let quick = !plan.base_known;
        // A server with no old server pack to compare with: comparing would
        // change nothing.
        let has_base = self
            .server
            .as_ref()
            .is_none_or(|t| t.splan.as_ref().is_some_and(|p| p.base.is_some()));
        let can_compare = quick && !plan.compared && !self.reinstall && has_base;
        if n == 0 && !plan.compared {
            return None;
        }
        let files = if n == 1 { "file" } else { "files" };
        let to = if server {
            "the server pack".to_string()
        } else {
            plan.file.display_name.clone()
        };
        let title = if n == 0 {
            "Compared with your changes: none clash with the pack's".to_string()
        } else if quick {
            if server {
                format!("{n} server {files} differ from the server pack")
            } else {
                format!("{n} pack {files} differ from {to}")
            }
        } else {
            format!("{n} {files} you and the pack both changed")
        };
        let mine_label = if server { "Keep the server's" } else { "Keep my edits" };
        let mut choices = Vec::new();
        if quick {
            choices.push((Bulk::Pack, "Use the pack's"));
            choices.push((Bulk::Mine, mine_label));
        } else {
            let suggested = plan.default_take().len();
            // "Suggested" only when it differs from both "all" choices.
            if suggested > 0 && suggested < n {
                choices.push((Bulk::Suggested, "Suggested"));
            }
            choices.push((Bulk::Mine, mine_label));
            choices.push((Bulk::Pack, "Use all the pack's"));
        }
        choices.push((Bulk::Review, "Review each"));
        let selected = choices
            .iter()
            .position(|(b, _)| *b == self.bulk)
            .unwrap_or(choices.len() - 1);
        let mine = rows.iter().filter(|r| !self.take.contains(*r)).count();
        let backups = if server {
            "The server's copies go to .hopper-backups."
        } else {
            "Yours go to .hopper-backups, and a snapshot is saved first."
        };
        let note = match self.bulk {
            Bulk::Pack if quick && mine > 0 => format!(
                "The new version's copy replaces each one, like the CurseForge app. {backups} Your own settings ({}) stay.",
                rows.iter().filter(|r| !self.take.contains(*r)).cloned().collect::<Vec<_>>().join(", ")
            ),
            Bulk::Pack if quick => format!("The new version's copy replaces each one, like the CurseForge app. {backups}"),
            Bulk::Pack => format!("The pack's copy replaces every one. {backups}"),
            Bulk::Suggested => format!(
                "Pack content (quests, scripts) takes the pack's copy ({}); settings keep yours ({mine}). {backups}",
                n - mine
            ),
            Bulk::Mine if server => "The server's copy of every one stays; the pack's changes to them are skipped.".to_string(),
            Bulk::Mine => "Your copy of every one stays; the pack's changes to them are skipped.".to_string(),
            Bulk::Review => format!("Picked per file: {mine} keep yours, {} take the pack's. {backups}", n - mine),
        };
        let open = self.open.contains(&Group::Files(FileAction::Conflict));
        let picks: Vec<Bulk> = choices.iter().map(|(b, _)| *b).collect();
        let theme = cx.theme();
        let (warning, primary, muted) = (theme.warning, theme.primary, theme.muted_foreground);
        let (radius, border) = (theme.radius, theme.border);
        let compare_note = if server {
            "Finds the files you edited on the server so you can keep them, and merges edits where the pack changed other lines. Reads the old server pack's file list (no full download)."
                .to_string()
        } else {
            format!(
                "Finds the files you edited yourself so you can keep them, and merges edits where the pack changed other lines. Downloads the version you have ({}) too.",
                self.from
            )
        };
        let card = v_flex()
            .gap_2()
            .px_3()
            .py_2p5()
            .rounded(radius)
            .border_1()
            .border_color(primary.opacity(widgets::alpha::LINE))
            .bg(primary.opacity(0.04))
            .child(
                h_flex()
                    .gap_2()
                    .child(widgets::chip(primary).xsmall().child("Your choice"))
                    .child(div().text_sm().font_semibold().child(title)),
            )
            .when(n > 0, |c| {
                c.child(
                    h_flex().child(
                        TabBar::new("bulk-pick")
                            .segmented()
                            .small()
                            .selected_index(selected)
                            .children(choices.iter().map(|(_, l)| Tab::new().label(*l)))
                            .on_click(cx.listener(move |this, ix: &usize, _, cx| {
                                if let Some(b) = picks.get(*ix) {
                                    this.pick_bulk(*b, cx);
                                }
                            })),
                    ),
                )
                .child(widgets::fine_print(note, cx))
                .child(
                    h_flex()
                        .id("review-files")
                        .gap_1()
                        .text_sm()
                        .text_color(muted)
                        .clickable(cx)
                        .hover(move |s| s.text_color(primary))
                        .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).small())
                        .child(if open { "Hide the files".to_string() } else { format!("Review {n} {files}") })
                        .on_click(cx.listener(|this, _, _, cx| this.toggle(Group::Files(FileAction::Conflict), cx))),
                )
                .when(open, |c| c.child(self.conflicts(rows, cx)))
            })
            .when(can_compare, |c| {
                c.child(
                    h_flex()
                        .gap_3()
                        .items_start()
                        .px_3()
                        .py_2()
                        .rounded(radius)
                        .border_1()
                        .border_dashed()
                        .border_color(border)
                        .child(
                            Button::new("compare-changes")
                                .small()
                                .outline()
                                .icon(IconName::GitCompare)
                                .label("Compare with my changes")
                                .flex_none()
                                .on_click(cx.listener(|this, _, _, cx| this.set_compare(true, cx))),
                        )
                        .child(widgets::fine_print(compare_note, cx).flex_1().min_w_0()),
                )
            })
            .when(plan.compared, |c| {
                c.child(
                    h_flex().child(div().flex_1()).child(
                        Button::new("back-quick")
                            .xsmall()
                            .ghost()
                            .label("Back to the quick update")
                            .on_click(cx.listener(|this, _, _, cx| this.set_compare(false, cx))),
                    ),
                )
            })
            .when(plan.compared && !plan.base_known && !self.reinstall, |c| {
                c.child(
                    h_flex()
                        .gap_2()
                        .text_xs()
                        .text_color(warning)
                        .child(Icon::new(IconName::TriangleAlert).xsmall())
                        .child(if server {
                            "No record of the server pack this server came from, so your edits can't be told from the pack's."
                        } else {
                            "The version you have couldn't be downloaded, so your edits can't be told from the pack's."
                        }),
                )
            });
        Some(card.into_any_element())
    }

    fn toggle(&mut self, g: Group, cx: &mut Context<Self>) {
        if !self.open.remove(&g) {
            self.open.insert(g);
        }
        cx.notify();
    }

    fn toggle_diff(&mut self, rel: String, cx: &mut Context<Self>) {
        if self.diffs.remove(&rel).is_some() {
            cx.notify();
            return;
        }
        let d = match &self.plan {
            Some(Ok(p)) => p.diff(&self.root, &rel).unwrap_or(None),
            _ => None,
        };
        self.diffs.insert(rel, d);
        cx.notify();
    }

    fn apply(view: &Entity<Self>, cx: &mut App) -> bool {
        let v = view.read(cx);
        let Some(Ok(plan)) = &v.plan else {
            return false;
        };
        let mut overrides = plan.decisions_with(&v.take, &v.turn_on);
        overrides.keep_dropped = v.keep_needed.clone();
        let (store, root, file) = (v.store.clone(), v.root.clone(), plan.file.clone());
        let backup = v.backup_worlds;
        if let Ok(mut g) = REVIEWS.lock()
            && let Some(m) = g.as_mut()
        {
            m.retain(|_, (r, _, _)| *r != root);
        }
        // A snapshot first (off the UI thread), then the update.
        let from = store
            .read(cx)
            .instance(&root)
            .and_then(|i| i.pack.as_ref())
            .map(|p| p.version.clone())
            .unwrap_or_default();
        let reason = hopper::snapshots::Reason::PackUpdate { from, to: file.display_name.clone() };
        let job = store.read(cx).jobs().start(format!("Snapshot before updating {}", file.display_name));
        cx.spawn(async move |cx| {
            let r2 = root.clone();
            let r = cx
                .background_spawn(async move {
                    let r = hopper::snapshots::take_in(&r2, reason, &job);
                    job.finish(&r);
                    r
                })
                .await;
            if let Err(e) = &r {
                // The update still runs: its replaced files go to backups as before.
                log::warn!("snapshot before pack update failed: {e:#}");
            }
            store.update(cx, |s, cx| {
                s.update_pack(&root, file, Some(overrides), backup);
                cx.notify();
            });
        })
        .detach();
        true
    }

    /// Opens a summary line's list (closing any other), or closes it.
    fn toggle_section(&mut self, s: Section, window: &mut Window, cx: &mut Context<Self>) {
        self.section = if self.section == Some(s) { None } else { Some(s) };
        self.filter = 0;
        self.folders.clear();
        if !self.query.is_empty() {
            self.query.clear();
            self.search.update(cx, |i, cx| i.set_value("", window, cx));
        }
        cx.notify();
    }

    /// Filter tabs and the search box over an open list.
    fn list_tools(&self, id: &'static str, tabs: Vec<String>, cx: &mut Context<Self>) -> Div {
        h_flex()
            .gap_2()
            .when(tabs.len() > 1, |r| {
                r.child(
                    TabBar::new(id)
                        .segmented()
                        .xsmall()
                        .selected_index(self.filter)
                        .children(tabs.into_iter().map(|l| Tab::new().label(l)))
                        .on_click(cx.listener(|this, ix: &usize, _, cx| {
                            this.filter = *ix;
                            cx.notify();
                        })),
                )
            })
            .child(div().flex_1())
            .child(
                div()
                    .w(rems(13.))
                    // Enter in the box would take the dialog's Update.
                    .on_action(|_: &gpui_kit::component::dialog::Confirm, _, _| {})
                    .child(Input::new(&self.search).small().cleanable(true)),
            )
    }

    fn matches(&self, text: &str) -> bool {
        self.query.is_empty() || text.to_lowercase().contains(&self.query)
    }

    /// A unified diff under a file row.
    fn diff_block(d: Option<String>, cx: &App) -> Div {
        let theme = cx.theme();
        let lines: Vec<String> = d
            .unwrap_or("Not a text file.".into())
            .lines()
            .take(400)
            .map(str::to_string)
            .collect();
        v_flex()
            .my_1()
            .p_3()
            .rounded(theme.radius)
            .bg(theme.muted)
            .font_family(theme.mono_font_family.clone())
            .text_xs()
            .children(lines.into_iter().map(|l| {
                let color = if l.starts_with('+') && !l.starts_with("+++") {
                    theme.success
                } else if l.starts_with('-') && !l.starts_with("---") {
                    theme.danger
                } else if l.starts_with("@@") {
                    theme.info
                } else {
                    theme.muted_foreground
                };
                div()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_color(color)
                    .child(l.replace('\t', "    "))
            }))
    }

    /// Files grouped by their top folder, folders closed until clicked
    /// (open while searching). Each row: (path, chip, has a diff).
    fn file_tree(&self, id: &str, rows: Vec<(String, Option<(&'static str, Hsla)>, bool)>, cx: &mut Context<Self>) -> AnyElement {
        let mut by: std::collections::BTreeMap<String, Vec<(String, Option<(&'static str, Hsla)>, bool)>> =
            std::collections::BTreeMap::new();
        for r in rows.into_iter().filter(|r| self.matches(&r.0)) {
            let top = match r.0.split_once('/') {
                Some((t, _)) => format!("{t}/"),
                None => "other".to_string(),
            };
            by.entry(top).or_default().push(r);
        }
        if by.is_empty() {
            return widgets::caption(if self.query.is_empty() { "Nothing here." } else { "No match." }, cx)
                .text_xs()
                .into_any_element();
        }
        let theme = cx.theme();
        let (muted, primary, border) = (theme.muted_foreground, theme.primary, theme.border);
        let searching = !self.query.is_empty();
        let mut out = v_flex().rounded(theme.radius).border_1().border_color(border);
        for (folder, rows) in by {
            let open = searching || self.folders.contains(&folder);
            // "1,412 replaced · 640 new", or "6 files" without chips.
            let mut counts: Vec<(&'static str, usize)> = Vec::new();
            for r in &rows {
                let l = r.1.map_or("files", |c| c.0);
                match counts.iter_mut().find(|c| c.0 == l) {
                    Some(c) => c.1 += 1,
                    None => counts.push((l, 1)),
                }
            }
            let counts = counts
                .iter()
                .map(|(l, n)| match *l {
                    "files" if *n == 1 => "1 file".to_string(),
                    l => format!("{n} {}", l.to_lowercase()),
                })
                .collect::<Vec<_>>()
                .join(" · ");
            let f2 = folder.clone();
            out = out.child(
                h_flex()
                    .id(SharedString::from(format!("{id}-fold-{folder}")))
                    .gap_2()
                    .px_2()
                    .py_1()
                    .text_sm()
                    .border_b_1()
                    .border_color(border)
                    .clickable(cx)
                    .hover(move |s| s.text_color(primary))
                    .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall().text_color(muted))
                    .child(div().flex_1().min_w_0().font_semibold().child(folder.clone()))
                    .child(div().text_xs().text_color(muted).child(counts))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.folders.remove(&f2) {
                            this.folders.insert(f2.clone());
                        }
                        cx.notify();
                    })),
            );
            if !open {
                continue;
            }
            let more = rows.len().saturating_sub(LIST_CAP);
            for (rel, chip, diffable) in rows.into_iter().take(LIST_CAP) {
                let diff = self.diffs.get(&rel).cloned();
                let r1 = rel.clone();
                out = out.child(
                    v_flex().child(
                        h_flex()
                            .gap_2()
                            .pl_6()
                            .pr_2()
                            .py_0p5()
                            .border_b_1()
                            .border_color(border.opacity(0.5))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .truncate()
                                    .child(rel.strip_prefix(folder.as_str()).unwrap_or(&rel).to_string()),
                            )
                            .children(chip.map(|(l, c)| widgets::chip(c).xsmall().child(l)))
                            .when(diffable, |r| {
                                r.child(
                                    h_flex().w(rems(6.5)).flex_none().justify_end().child(
                                        Button::new(SharedString::from(format!("{id}-diff:{rel}")))
                                            .xsmall()
                                            .ghost()
                                            .label(if diff.is_some() { "Hide changes" } else { "Changes" })
                                            .on_click(cx.listener(move |this, _, _, cx| this.toggle_diff(r1.clone(), cx))),
                                    ),
                                )
                            }),
                    )
                    .when_some(diff, |c, d| c.child(Self::diff_block(d, cx).mx_2())),
                );
            }
            if more > 0 {
                out = out.child(
                    widgets::caption(format!("…{more} more in {folder}: search to narrow"), cx)
                        .text_xs()
                        .pl_6()
                        .py_1(),
                );
            }
        }
        out.into_any_element()
    }

    /// A dropped mod one of yours needs: a warning, and a button to keep it.
    fn needed_card(&self, n: &hopper::packdiff::Needed, cx: &mut Context<Self>) -> AnyElement {
        let kept = self.keep_needed.contains(&n.file.to_lowercase());
        let theme = cx.theme();
        let warning = theme.warning;
        let view = cx.entity();
        let open = move |p: u64, w: &mut Window, cx: &mut App| {
            view.update(cx, |this, cx| this.open_peek(p, w, cx));
        };
        let img = |u: &str, cx: &Context<Self>| self.store.read(cx).image(u).map(|p| p.to_path_buf());
        let (by_img, img2) = (img(&n.by_icon, cx), img(&n.icon, cx));
        let to = if self.server.is_some() { "the new server pack".to_string() } else { self.plan_name() };
        let file = n.file.to_lowercase();
        v_flex()
            .gap_1p5()
            .px_3()
            .py_2()
            .rounded(theme.radius)
            .border_1()
            .border_color(warning.opacity(0.4))
            .bg(warning.opacity(0.05))
            .child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .font_semibold()
                    .text_color(warning)
                    .child(Icon::new(IconName::TriangleAlert).small())
                    .child("The pack drops a mod one of yours needs"),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_1()
                    .text_sm()
                    .child(widgets::mod_link(
                        SharedString::from(format!("need-by:{}", n.file)),
                        n.by.clone(),
                        by_img.as_deref(),
                        n.by_project,
                        open.clone(),
                        cx,
                    ))
                    .child("needs")
                    .child(widgets::mod_link(
                        SharedString::from(format!("need:{}", n.file)),
                        n.name.clone(),
                        img2.as_deref(),
                        n.project,
                        open,
                        cx,
                    ))
                    .child("."),
            )
            .child(div().text_sm().child(format!("{to} no longer includes it. Without it the game won't start.")))
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        Button::new(SharedString::from(format!("keep-need:{}", n.file)))
                            .xsmall()
                            .outline()
                            .label(if kept { "Remove it with the pack".to_string() } else { format!("Keep {}", n.name) })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.keep_needed.remove(&file) {
                                    this.keep_needed.insert(file.clone());
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        widgets::fine_print(
                            if kept {
                                "Kept: it counts as one of your mods from now on, so later updates leave it alone."
                            } else {
                                "Removed with the pack unless you keep it."
                            },
                            cx,
                        ),
                    ),
            )
            .into_any_element()
    }

    /// The version updated to, by name.
    fn plan_name(&self) -> String {
        match &self.plan {
            Some(Ok(p)) => p.file.display_name.clone(),
            _ => "the new version".into(),
        }
    }

    /// A list of mods with a chip each: (change, chip, detail).
    fn mod_rows(&self, id: &str, rows: Vec<(hopper::packdiff::ModChange, (&'static str, Hsla), String)>, cx: &mut Context<Self>) -> AnyElement {
        let rows: Vec<_> = rows
            .into_iter()
            .filter(|(m, _, d)| self.matches(&m.name) || self.matches(d))
            .collect();
        if rows.is_empty() {
            return widgets::caption(if self.query.is_empty() { "Nothing here." } else { "No match." }, cx)
                .text_xs()
                .into_any_element();
        }
        let theme = cx.theme();
        let border = theme.border;
        let more = rows.len().saturating_sub(LIST_CAP);
        v_flex()
            .rounded(theme.radius)
            .border_1()
            .border_color(border)
            .children(rows.into_iter().take(LIST_CAP).map(|(m, (label, color), detail)| {
                widgets::row_hover(
                    h_flex()
                        .gap_3()
                        .px_2()
                        .py_0p5()
                        .border_b_1()
                        .border_color(border.opacity(0.5))
                        .child(self.mod_cell(format!("{id}:{}:{}:{}", m.name, m.from, m.to), &m.name, &m.icon, m.project, cx))
                        .child(widgets::caption(detail, cx).text_xs().flex_1().min_w_0().truncate())
                        .child(widgets::chip(color).xsmall().child(label)),
                    cx,
                )
            }))
            .when(more > 0, |c| {
                c.child(widgets::caption(format!("…{more} more: search to narrow"), cx).text_xs().px_2().py_1())
            })
            .into_any_element()
    }

    /// "What changes": mods, pack files and (compared) dropped files, each
    /// a line that opens its list.
    fn changes(&self, plan: &UpdatePlan, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (success, info, danger, warning, primary, muted) =
            (theme.success, theme.info, theme.danger, theme.warning, theme.primary, theme.muted_foreground);
        let count = |a: ModAction| plan.mods.iter().filter(|m| m.action == a).count();
        let (added, updated, dropped) = (count(ModAction::Add), count(ModAction::Update), count(ModAction::Remove));
        let total = added + updated + dropped;
        let open = self.section;
        let mut out = v_flex().gap_0p5().child(div().font_semibold().child("What changes"));

        // Mods.
        let mut body = h_flex().gap_1p5().flex_wrap();
        if total == 0 {
            body = body.child(div().text_color(muted).child("No mod changes"));
        } else {
            body = body.child(format!("{total} mods:"));
            for (n, l, c) in [(added, "added", success), (updated, "updated", info), (dropped, "dropped", danger)] {
                if n > 0 {
                    body = body.child(widgets::chip(c).xsmall().child(format!("{n} {l}")));
                }
            }
            if !plan.needed.is_empty() {
                body = body.child(widgets::chip(warning).xsmall().child(format!("⚠ {} needed by yours", plan.needed.len())));
            }
        }
        out = out.child(
            widgets::summary_row("sum-mods", IconName::Package, info, body, (total > 0).then_some(open == Some(Section::Mods)), cx)
                .on_click(cx.listener(move |this, _, w, cx| {
                    if total > 0 {
                        this.toggle_section(Section::Mods, w, cx)
                    }
                })),
        );
        if open == Some(Section::Mods) {
            let needed: HashSet<String> = plan.needed.iter().map(|n| n.file.to_lowercase()).collect();
            let want = match self.filter {
                1 => Some(ModAction::Add),
                2 => Some(ModAction::Update),
                3 => Some(ModAction::Remove),
                _ => None,
            };
            let mut rows: Vec<_> = plan
                .mods
                .iter()
                .filter(|m| want.is_none_or(|a| m.action == a))
                .map(|m| {
                    let need = m.action == ModAction::Remove && needed.contains(&m.from.to_lowercase());
                    let kept = need && self.keep_needed.contains(&m.from.to_lowercase());
                    let chip = match m.action {
                        ModAction::Add => ("Added", success),
                        ModAction::Update => ("Updated", info),
                        ModAction::Remove if kept => ("Kept", warning),
                        ModAction::Remove => ("Dropped", danger),
                    };
                    let detail = match m.action {
                        ModAction::Update => format!("{}  →  {}", m.from, m.to),
                        ModAction::Add => m.to.clone(),
                        ModAction::Remove if need => format!("⚠ one of yours needs it · {}", m.from),
                        ModAction::Remove => m.from.clone(),
                    };
                    (need, (m.clone(), chip, detail))
                })
                .collect();
            rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.0.name.to_lowercase().cmp(&b.1.0.name.to_lowercase())));
            let tabs = vec![
                format!("All {total}"),
                format!("Added {added}"),
                format!("Updated {updated}"),
                format!("Dropped {dropped}"),
            ];
            let needed_cards: Vec<AnyElement> = plan.needed.iter().map(|n| self.needed_card(n, cx)).collect();
            out = out.child(
                widgets::expand_area(cx)
                    .children(needed_cards)
                    .child(self.list_tools("mods-filter", tabs, cx))
                    .child(self.mod_rows("chg", rows.into_iter().map(|r| r.1).collect(), cx))
                    .when(dropped > 0, |c| c.child(widgets::caption("Dropped mods go to .hopper-backups.", cx).text_xs())),
            );
        }

        // Pack files.
        let take = &self.take;
        let written: Vec<&hopper::packdiff::FileChange> = plan
            .files
            .iter()
            .filter(|f| match f.action {
                FileAction::Add | FileAction::Update | FileAction::Merge => true,
                FileAction::Conflict => take.contains(&f.rel),
                _ => false,
            })
            .collect();
        let new_n = written.iter().filter(|f| f.action == FileAction::Add).count();
        let merged_n = written.iter().filter(|f| f.action == FileAction::Merge).count();
        let replaced_n = written.len() - new_n;
        let what = if self.server.is_some() { "server pack files" } else { "pack files" };
        let text = if written.is_empty() {
            format!("No {what} change")
        } else if !plan.base_known {
            format!("{} {what} written: {replaced_n} replaced, {new_n} new", written.len())
        } else if merged_n > 0 {
            format!("{} {what} updated ({new_n} new, {merged_n} merged)", written.len())
        } else {
            format!("{} {what} updated ({new_n} new)", written.len())
        };
        let n_written = written.len();
        out = out.child(
            widgets::summary_row(
                "sum-files",
                IconName::FileText,
                primary,
                div().when(n_written == 0, |d| d.text_color(muted)).child(text),
                (n_written > 0).then_some(open == Some(Section::Files)),
                cx,
            )
            .on_click(cx.listener(move |this, _, w, cx| {
                if n_written > 0 {
                    this.toggle_section(Section::Files, w, cx)
                }
            })),
        );
        if open == Some(Section::Files) {
            let rows: Vec<_> = written
                .iter()
                .filter(|f| match self.filter {
                    1 => f.action != FileAction::Add,
                    2 => f.action == FileAction::Add,
                    _ => true,
                })
                .map(|f| {
                    let chip = match f.action {
                        FileAction::Add => ("New", success),
                        FileAction::Merge => ("Merged", info),
                        _ => ("Replaced", primary),
                    };
                    (f.rel.clone(), Some(chip), f.action != FileAction::Add)
                })
                .collect();
            let tabs = vec!["All".to_string(), format!("Replaced {replaced_n}"), format!("New {new_n}")];
            out = out.child(
                widgets::expand_area(cx)
                    .child(self.list_tools("files-filter", tabs, cx))
                    .child(self.file_tree("chg-files", rows, cx))
                    .child(widgets::caption("\"Changes\" shows yours against the pack's.", cx).text_xs()),
            );
        }

        // Compared: files the pack dropped that you never changed.
        let gone: Vec<String> = plan
            .files
            .iter()
            .filter(|f| f.action == FileAction::Remove)
            .map(|f| f.rel.clone())
            .collect();
        if !gone.is_empty() {
            let n = gone.len();
            out = out.child(
                widgets::summary_row(
                    "sum-dropped",
                    IconName::Archive,
                    danger,
                    div().child(format!("{n} files the pack dropped, unedited: to backups")),
                    Some(open == Some(Section::Dropped)),
                    cx,
                )
                .on_click(cx.listener(|this, _, w, cx| this.toggle_section(Section::Dropped, w, cx))),
            );
            if open == Some(Section::Dropped) {
                let rows = gone.into_iter().map(|r| (r, Some(("Removed", danger)), false)).collect();
                out = out.child(
                    widgets::expand_area(cx)
                        .child(self.list_tools("dropped-filter", Vec::new(), cx))
                        .child(self.file_tree("chg-dropped", rows, cx)),
                );
            }
        }
        out.into_any_element()
    }

    /// "What stays": your mods and files, (compared) the files you edited,
    /// and what an update never touches.
    fn stays(&self, plan: &UpdatePlan, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (success, warning, muted) = (theme.success, theme.warning, theme.muted_foreground);
        let server = self.server.is_some();
        let open = self.section;
        let (m, f) = (plan.your_mods.len(), plan.your_files.len());
        let mut out = v_flex().gap_0p5().child(div().font_semibold().child("What stays"));
        let mut parts = Vec::new();
        if m > 0 {
            parts.push(if server { format!("{m} mods not in the server pack") } else { format!("{m} mods you added") });
        }
        if f > 0 {
            parts.push(if server {
                format!("{f} files of the server's own in pack folders")
            } else {
                format!("{f} of your files in pack folders")
            });
        }
        let any = !parts.is_empty();
        out = out.child(
            widgets::summary_row(
                "sum-stays",
                IconName::Check,
                success,
                div().when(!any, |d| d.text_color(muted)).child(if any {
                    parts.join(" · ")
                } else {
                    "Nothing of yours in the pack's folders".to_string()
                }),
                any.then_some(open == Some(Section::Stays)),
                cx,
            )
            .on_click(cx.listener(move |this, _, w, cx| {
                if any {
                    this.toggle_section(Section::Stays, w, cx)
                }
            })),
        );
        if open == Some(Section::Stays) {
            let by: HashMap<String, String> = plan.needed.iter().map(|n| (n.by.to_lowercase(), n.name.clone())).collect();
            let tabs = vec![format!("Your mods {m}"), format!("Your files {f}")];
            let list = if self.filter == 0 {
                let mut rows: Vec<_> = plan
                    .your_mods
                    .iter()
                    .map(|y| {
                        let need = by.get(&y.name.to_lowercase());
                        let detail = match need {
                            Some(d) => format!("⚠ needs {d}, which the pack drops"),
                            None => y.from.clone(),
                        };
                        (need.is_some(), (y.clone(), ("Yours", if need.is_some() { warning } else { muted }), detail))
                    })
                    .collect();
                rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.0.name.to_lowercase().cmp(&b.1.0.name.to_lowercase())));
                self.mod_rows("stay", rows.into_iter().map(|r| r.1).collect(), cx)
            } else {
                let rows = plan.your_files.iter().map(|r| (r.clone(), None, false)).collect();
                v_flex()
                    .gap_1()
                    .child(self.file_tree("stay-files", rows, cx))
                    .child(
                        widgets::fine_print(
                            "Files in folders the pack also uses that the pack doesn't ship: yours, and ones mods made. The update writes next to them, never over them.",
                            cx,
                        ),
                    )
                    .into_any_element()
            };
            out = out.child(widgets::expand_area(cx).child(self.list_tools("stay-tabs", tabs, cx)).child(list));
        }
        if let Some(k) = self.server.as_ref().and_then(|t| t.splan.as_ref()).map(|p| p.kept) {
            let mut lines = Vec::new();
            if k.yours > 0 {
                lines.push(format!("{} mods where the server runs another version and the pack didn't change it keep the server's.", k.yours));
            }
            if k.left_off > 0 {
                lines.push(format!("{} pack mods the server doesn't have stay off.", k.left_off));
            }
            for l in lines {
                out = out.child(widgets::caption(l, cx).text_xs().pl(rems(1.9)));
            }
        }

        // Compared: files you edited, kept.
        let take = &self.take;
        let edited: Vec<String> = plan
            .files
            .iter()
            .filter(|f| match f.action {
                FileAction::KeepYours | FileAction::KeepDropped => true,
                FileAction::Conflict => !take.contains(&f.rel),
                _ => false,
            })
            .map(|f| f.rel.clone())
            .collect();
        if !edited.is_empty() {
            let n = edited.len();
            let text = if server {
                format!("{n} pack files the server changed: kept")
            } else {
                format!("{n} pack files you changed: kept")
            };
            out = out.child(
                widgets::summary_row("sum-edited", IconName::Check, success, div().child(text), Some(open == Some(Section::Edited)), cx)
                    .on_click(cx.listener(|this, _, w, cx| this.toggle_section(Section::Edited, w, cx))),
            );
            if open == Some(Section::Edited) {
                let rows = edited.into_iter().map(|r| (r, Some(("Yours", muted)), true)).collect();
                out = out.child(
                    widgets::expand_area(cx)
                        .child(self.list_tools("edited-filter", Vec::new(), cx))
                        .child(self.file_tree("stay-edited", rows, cx)),
                );
            }
        }
        out = out.child(widgets::summary_row(
            "sum-never",
            IconName::ShieldCheck,
            muted,
            div().text_color(muted).child(if server {
                "The world, ops, whitelist and bans: never touched"
            } else {
                "Worlds, screenshots, logs, shader settings and backups: never touched"
            }),
            None,
            cx,
        ));
        out.into_any_element()
    }

    /// Fetches the icons of every mod the plan lists.
    fn want_icons(&self, p: &UpdatePlan, cx: &mut Context<Self>) {
        let urls: Vec<String> = p
            .mods
            .iter()
            .map(|m| m.icon.clone())
            .chain(p.off.iter().chain(&p.removed).chain(&p.updated).map(|m| m.icon.clone()))
            .filter(|u| !u.is_empty())
            .collect();
        self.store.update(cx, |s, _| {
            for u in &urls {
                s.want_image(u);
            }
        });
    }

    /// Opens a mod's page beside the lists.
    fn open_peek(&mut self, project: u64, window: &mut Window, cx: &mut Context<Self>) {
        if project == 0 {
            return;
        }
        let root = match &self.server {
            Some(t) => t.inst.root.clone(),
            None => self.root.clone(),
        };
        let store = self.store.clone();
        if self.peek.is_none() {
            let p = cx.new(|cx| crate::browse::BrowseView::peek(store, window, cx));
            // Its own close button empties it: then the panel goes.
            // (Only once it showed one: it's empty while the first loads.)
            let mut shown = false;
            self._peek_watch = Some(cx.observe(&p, move |this, p, cx| {
                match p.read(cx).selected_id() {
                    Some(_) => shown = true,
                    None if shown => {
                        this.peek = None;
                        this._peek_watch = None;
                        cx.notify();
                    }
                    None => {}
                }
            }));
            self.peek = Some(p);
        }
        let Some(peek) = self.peek.clone() else { return };
        peek.update(cx, |p, cx| p.peek_project(root, Some(project), cx));
        cx.notify();
    }

    /// A mod's icon and name; a click opens its page beside the lists.
    fn mod_cell(&self, id: String, name: &str, icon: &str, project: u64, cx: &mut Context<Self>) -> Stateful<Div> {
        let path = self.store.read(cx).image(icon).map(|p| p.to_path_buf());
        let theme = cx.theme();
        h_flex()
            .id(SharedString::from(id))
            .w_56()
            .flex_none()
            .min_w_0()
            .gap_2()
            .child(widgets::thumb(path.as_deref(), rems(1.25), cx))
            .child(div().min_w_0().text_sm().truncate().child(name.to_string()))
            .when(project != 0, |d| {
                d.clickable(cx)
                    .hover(|s| s.text_color(theme.primary))

                    .on_click(cx.listener(move |this, _, window, cx| this.open_peek(project, window, cx)))
            })
    }

    /// Mods you turned off, or removed, that the update touches: each row
    /// picks between keeping your choice (the default) and the pack's.
    fn your_choices(&self, plan: &UpdatePlan, cx: &mut Context<Self>) -> Option<AnyElement> {
        if plan.off.is_empty() && plan.removed.is_empty() && plan.updated.is_empty() {
            return None;
        }
        let list = |title: &'static str,
                    note: &'static str,
                    labels: [&'static str; 2],
                    all: [&'static str; 2],
                    rows: &[hopper::packdiff::OffMod],
                    this: &Self,
                    cx: &mut Context<Self>| {
            let keys: Vec<String> = rows.iter().map(|m| m.key.clone()).collect();
            let (k1, k2) = (keys.clone(), keys);
            v_flex()
                .gap_1()
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().font_semibold().child(title))
                        .child(div().flex_1())
                        .child(
                            Button::new(SharedString::from(format!("{title}-keep")))
                                .xsmall()
                                .ghost()
                                .label(all[0])
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    for k in &k1 {
                                        this.turn_on.remove(k);
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new(SharedString::from(format!("{title}-flip")))
                                .xsmall()
                                .ghost()
                                .label(all[1])
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.turn_on.extend(k2.iter().cloned());
                                    cx.notify();
                                })),
                        ),
                )
                .child(widgets::caption(note, cx).text_xs())
                .children(rows.iter().map(|m| {
                    let on = this.turn_on.contains(&m.key);
                    let key = m.key.clone();
                    let detail = if m.from.is_empty() || m.from == m.to {
                        m.to.clone()
                    } else {
                        format!("{}  →  {}", m.from, m.to)
                    };
                    let detail = if m.key.starts_with("u:") {
                        format!(
                            "yours {}  ·  pack's {}{}",
                            m.from,
                            m.to,
                            if m.pack_newer { " (newer)" } else { " (older)" }
                        )
                    } else {
                        detail
                    };
                    // Pressed and dragged down the list: the same choice on each row.
                    let view = cx.entity();
                    let set = {
                        let key = key.clone();
                        std::rc::Rc::new(move |v: usize, _: &mut Window, cx: &mut App| {
                            let key = key.clone();
                            view.update(cx, |this, cx| {
                                let changed = if v == 1 { this.turn_on.insert(key) } else { this.turn_on.remove(&key) };
                                if changed {
                                    cx.notify();
                                }
                            })
                        })
                    };
                    let paint = &this.paint;
                    let opt = |ix: usize, label: &'static str| {
                        let set = set.clone();
                        crate::drag_pick::paint_option(Tab::new().label(label), paint, title, ix, move |v, w, cx| set(v, w, cx))
                    };
                    let row_set = set.clone();
                    crate::drag_pick::paint_row(h_flex(), paint, title, move |v, w, cx| row_set(v, w, cx))
                        .gap_3()
                        .py_0p5()
                        .child(this.mod_cell(format!("cell:{}", m.key), &m.name, &m.icon, m.project, cx))
                        .child(widgets::caption(detail, cx).text_xs().flex_1().min_w_0())
                        .child(
                            TabBar::new(SharedString::from(format!("pick:{}", m.key)))
                                .segmented()
                                .xsmall()
                                .selected_index(usize::from(on))
                                .child(opt(0, labels[0]))
                                .child(opt(1, labels[1]))
                                .on_click(cx.listener(move |this, ix: &usize, _, cx| {
                                    if *ix == 1 {
                                        this.turn_on.insert(key.clone());
                                    } else {
                                        this.turn_on.remove(&key);
                                    }
                                    cx.notify();
                                })),
                        )
                }))
                .into_any_element()
        };
        let mut out = v_flex().gap_4();
        if !plan.off.is_empty() {
            out = out.child(list(
                "Mods you turned off",
                "The update has new files for these. They stay off unless you turn them on.",
                ["Keep off", "Turn on"],
                ["Keep all off", "Turn all on"],
                &plan.off,
                self,
                cx,
            ));
        }
        if !plan.updated.is_empty() {
            out = out.child(list(
                "Mods you updated",
                "You updated these yourself. Yours stay where the pack's file is older; where it's newer, the pack's is picked.",
                ["Keep mine", "Use the pack's"],
                ["Keep all mine", "Use all the pack's"],
                &plan.updated,
                self,
                cx,
            ));
        }
        if !plan.removed.is_empty() {
            out = out.child(list(
                "Mods you removed",
                "The pack still has these and would put them back. They stay out unless you add them back.",
                ["Leave out", "Add back"],
                ["Leave all out", "Add all back"],
                &plan.removed,
                self,
                cx,
            ));
        }
        Some(out.into_any_element())
    }

    fn conflicts(&self, rows: Vec<String>, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .pl(rems(1.25))
            .gap_1()
            .children(rows.into_iter().map(|rel| {
                let theirs = self.take.contains(&rel);
                let diff = self.diffs.get(&rel).cloned();
                let (r1, r2) = (rel.clone(), rel.clone());
                // Pressed and dragged down the list: the same side on each file.
                let view = cx.entity();
                let set = {
                    let rel = rel.clone();
                    std::rc::Rc::new(move |v: usize, _: &mut Window, cx: &mut App| {
                        let rel = rel.clone();
                        view.update(cx, |this, cx| {
                            let changed = if v == 1 { this.take.insert(rel) } else { this.take.remove(&rel) };
                            if changed {
                                this.bulk = Bulk::Review;
                                cx.notify();
                            }
                        })
                    })
                };
                let paint = &self.paint;
                let opt = |ix: usize, label: &'static str| {
                    let set = set.clone();
                    crate::drag_pick::paint_option(Tab::new().label(label), paint, "conflicts", ix, move |v, w, cx| set(v, w, cx))
                };
                let row_set = set.clone();
                v_flex()
                    .child(
                        crate::drag_pick::paint_row(h_flex(), paint, "conflicts", move |v, w, cx| row_set(v, w, cx))
                            .gap_2()
                            .py_0p5()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .truncate()
                                    .child(rel.clone()),
                            )
                            .child(
                                Button::new(SharedString::from(format!("diff:{rel}")))
                                    .xsmall()
                                    .ghost()
                                    .label(if diff.is_some() { "Hide changes" } else { "Changes" })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.toggle_diff(r1.clone(), cx)
                                    })),
                            )
                            .child(
                                TabBar::new(SharedString::from(format!("pick:{rel}")))
                                    .segmented()
                                    .xsmall()
                                    .selected_index(usize::from(theirs))
                                    .child(opt(0, "Mine"))
                                    .child(opt(1, "Pack's"))
                                    .on_click(cx.listener(move |this, ix: &usize, _, cx| {
                                        if *ix == 1 {
                                            this.take.insert(r2.clone());
                                        } else {
                                            this.take.remove(&r2);
                                        }
                                        this.bulk = Bulk::Review;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .when_some(diff, |c, d| c.child(Self::diff_block(d, cx)))
            }))
            .into_any_element()
    }
}

impl Render for UpdatePreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Window-tall once loaded, less the dialog's top offset, title and
        // footer; the list takes the rest and scrolls (a max height on the
        // scroll area alone doesn't scroll).
        let total_h = (window.viewport_size().height - window.rem_size() * DIALOG_CHROME).max(window.rem_size() * 12.);
        let loaded = matches!(self.plan, Some(Ok(_)));
        let theme = cx.theme();
        let body: AnyElement = match &self.plan {
            None if self.server.is_some() => {
                let (detail, progress) = self
                    .server
                    .as_ref()
                    .and_then(|t| t.status.clone())
                    .unwrap_or_else(|| ("Finding the server pack".into(), None));
                let id = self.server.as_ref().and_then(|t| t.job).unwrap_or(0);
                v_flex()
                    .gap_2()
                    .py_3()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Spinner::new().small())
                            .child(div().text_sm().min_w_0().truncate().child(detail)),
                    )
                    .child(
                        gpui_kit::component::progress::Progress::new(ElementId::Name(
                            format!("server-pack-bar-{id}").into(),
                        ))
                        .loading(progress.is_none())
                        .value(progress.unwrap_or(0.) * 100.),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .pt_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(Icon::new(IconName::Info).xsmall())
                            .child("You can close this window: the download keeps going. Open it again from the task list (the download icon, top right), or with Server pack update."),
                    )
                    .into_any_element()
            }
            None => h_flex()
                .gap_2()
                .py_4()
                .child(Spinner::new().small())
                .child(widgets::caption(
                    if self.reinstall {
                        "Downloading this pack version and comparing it with your instance"
                    } else if self.compare {
                        "Downloading both pack versions and comparing them with your changes"
                    } else {
                        "Downloading the new version and comparing it with your instance"
                    },
                    cx,
                ))
                .into_any_element(),
            Some(Err(e)) => div()
                .text_sm()
                .text_color(theme.danger)
                .child(e.clone())
                .into_any_element(),
            Some(Ok(plan)) => {
                let plan = plan.clone();
                let server_mode = self.server.is_some();
                let theme = cx.theme();
                let mc_change = self.mc_change.clone();
                let backup = self.backup_worlds;
                let (warning, muted, border) = (theme.warning, theme.muted_foreground, theme.border);
                let radius = theme.radius;
                let choice = self.choice_card(&plan, cx);
                let mine = self.your_choices(&plan, cx);
                let changes = self.changes(&plan, cx);
                let stays = self.stays(&plan, cx);
                v_flex()
                    .gap_4()
                    .when_some(mc_change, |c, (from, to)| {
                        c.child(
                            v_flex()
                                .gap_2()
                                .px_3()
                                .py_2p5()
                                .rounded(radius)
                                .border_1()
                                .border_color(warning.opacity(0.5))
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .text_sm()
                                        .text_color(warning)
                                        .child(Icon::new(IconName::TriangleAlert).small())
                                        .child(format!("Minecraft {from} → {to}: worlds are converted and can't go back.")),
                                )
                                .child(
                                    gpui_kit::component::checkbox::Checkbox::new("backup-worlds")
                                        .label("Back up every world first")
                                        .checked(backup)
                                        .on_change(cx.listener(|this, v: &bool, _, cx| {
                                            this.backup_worlds = *v;
                                            cx.notify();
                                        })),
                                ),
                        )
                    })
                    .children(choice)
                    .children(mine)
                    .child(changes)
                    .child(stays)
                    .when(!server_mode, |c| {
                        c.child(
                            h_flex()
                                .gap_2()
                                .text_xs()
                                .text_color(muted)
                                .child(Icon::new(IconName::Layers).xsmall().flex_none())
                                .child("A snapshot of mods and configs is saved first. Restore it any time from Backups."),
                        )
                    })
                    .when(server_mode, |c| {
                        c.child(
                            div()
                                .pl_3()
                                .border_l_2()
                                .border_color(border)
                                .text_xs()
                                .text_color(muted)
                                .child("Server packs often leave out global datapacks (Paxi, OpenLoader) and configs of mods new to the pack. Server sync flags what still differs afterwards."),
                        )
                    })
                    .into_any_element()
            }
        };
        let notes = self.server_notes(cx);
        let peek = self.peek.clone().map(|p| {
            v_flex()
                .w(rems(30.))
                .flex_none()
                .h(total_h)
                .gap_2()
                .child(div().flex_1().min_h_0().child(p))
        });
        let main = v_flex()
            .flex_1()
            .min_w_0()
            .when(loaded, |c| c.h(total_h))
            .gap_3()
            .children({
                let caption = match &self.plan {
                    Some(Ok(p)) => format!("{}  →  {}", self.from, p.file.file_name),
                    _ => self.from.clone(),
                };
                (!caption.is_empty()).then(|| widgets::caption(caption, cx))
            })
            .children(notes)
            // Loaded, the list fills the window-tall dialog and scrolls;
            // before that the "comparing" line sizes itself (a flex child
            // with no height to share collapses to nothing).
            .child(if loaded {
                div()
                    .id("update-preview")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(body)
                    .into_any_element()
            } else {
                body
            });
        h_flex().items_start().gap_4().child(main).children(peek)
    }
}

/// Previews updating an instance's modpack to `file`, then updates it.
pub fn open(store: Entity<Store>, root: PathBuf, file: cf::File, window: &mut Window, cx: &mut App) {
    show(store, root, file, None, window, cx)
}

/// The preview dialog, comparing first unless `plan` is already known.
fn show(
    store: Entity<Store>,
    root: PathBuf,
    file: cf::File,
    plan: Option<Result<UpdatePlan, String>>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(inst) = store.read(cx).instance(&root).cloned() else {
        return;
    };
    let from = inst
        .pack
        .as_ref()
        .map(|p| if p.file_name.is_empty() { p.version.clone() } else { p.file_name.clone() })
        .unwrap_or_default();
    // Minecraft versions look like 1.21.1 or 26.3; the file's list also
    // names loaders.
    let to_mc = file
        .game_versions
        .iter()
        .find(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .cloned();
    let mc_change = to_mc
        .filter(|to| !inst.game_version.is_empty() && *to != inst.game_version)
        .map(|to| (inst.game_version.clone(), to));
    let backup_worlds = mc_change.is_some();
    let reinstall = inst.pack.as_ref().is_some_and(|p| hopper::packdiff::is_reinstall(p, &file));
    let view = cx.new(|cx| {
        let mut v = UpdatePreview::new(store, root, None, from, window, cx);
        v.mc_change = mc_change;
        v.backup_worlds = backup_worlds;
        v.reinstall = reinstall;
        v.file = Some(file.clone());
        match plan {
            Some(r) => {
                if let Ok(p) = &r {
                    v.loaded(p, cx);
                }
                v.plan = Some(r);
            }
            None => v.plan(file, cx),
        }
        v
    });
    let title = format!("{} {}", if reinstall { "Reinstall" } else { "Update" }, inst.name);
    window.open_dialog(cx, move |dialog, window, cx| {
        let ready = matches!(view.read(cx).plan, Some(Ok(_)));
        let v2 = view.clone();
        dialog
            .title(title.clone())
            .w(rems(if view.read(cx).peek.is_some() { 86. } else { 54. }).to_pixels(window.rem_size()))
            .margin_top(rems(DIALOG_TOP).to_pixels(window.rem_size()))
            .child(view.clone())
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("upd-cancel").outline().label("Cancel")))
                    .child(DialogAction::new().child(
                        Button::new("upd-go")
                            .primary()
                            .icon(if reinstall { IconName::RotateCcw } else { IconName::Download })
                            .label(if reinstall { "Reinstall" } else { "Update" })
                            .disabled(!ready),
                    )),
            )
            .on_ok(move |_, _, cx| UpdatePreview::apply(&v2, cx))
    });
}

/// Previews updating `server` (which runs `inst`) from the pack's server
/// pack, then updates it.
pub fn open_server(store: Entity<Store>, server: ServerInfo, inst: Instance, window: &mut Window, cx: &mut App) {
    // A comparison for this server still running: show that one rather
    // than downloading again. A finished one may be stale: compare anew.
    let running = server_reviews(|m| {
        m.iter()
            .filter(|(_, (t, r))| t.server.root == server.root && r.is_none())
            .max_by_key(|(id, _)| **id)
            .map(|(id, (t, _))| (*id, t.clone()))
    });
    if let Some((id, t)) = running {
        show_server(store, t, Some(id), window, cx);
        return;
    }
    let latest = store.read(cx).pack_update(&inst).cloned();
    let t = ServerTarget {
        server,
        inst,
        latest,
        targets: Vec::new(),
        pick: 0,
        splan: None,
        via: None,
        job: None,
        status: None,
    };
    show_server(store, t, None, window, cx);
}

/// The server pack dialog for `t`: following comparison `job`, or starting
/// one when `None`.
fn show_server(store: Entity<Store>, t: ServerTarget, job: Option<u64>, window: &mut Window, cx: &mut App) {
    let server = t.server.clone();
    let title = format!("Update {} from the server pack", server.name);
    let view = cx.new(|cx| {
        let mut v = UpdatePreview::new(store, server.root.clone(), Some(ServerTarget { job, ..t }), String::new(), window, cx);
        match job {
            Some(_) => v.follow(cx),
            None => v.plan_server(cx),
        }
        v
    });
    window.open_dialog(cx, move |dialog, window, cx| {
        let ready = view.read(cx).server.as_ref().is_some_and(|t| t.splan.is_some());
        let v2 = view.clone();
        dialog
            .title(title.clone())
            .w(rems(if view.read(cx).peek.is_some() { 86. } else { 54. }).to_pixels(window.rem_size()))
            .margin_top(rems(DIALOG_TOP).to_pixels(window.rem_size()))
            .child(view.clone())
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("supd-cancel").outline().label("Cancel")))
                    .child(DialogAction::new().child(
                        Button::new("supd-go")
                            .primary()
                            .icon(IconName::ServerCog)
                            .label("Update server")
                            .disabled(!ready),
                    )),
            )
            .on_ok(move |_, _, cx| UpdatePreview::apply_server(&v2, cx))
    });
}
