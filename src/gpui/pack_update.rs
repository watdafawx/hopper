//! The modpack update preview: what the update will do to mods and config
//! files, with a choice per file where both you and the pack changed it.

use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tag::Tag;
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

/// Collapsible groups of the preview.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Group {
    Mods(ModAction),
    Files(FileAction),
}

/// Finished comparisons by the job that made them: (instance, pack file,
/// result), so the task list can reopen a review whose dialog was closed.
type Review = (PathBuf, cf::File, Result<UpdatePlan, String>);
static REVIEWS: std::sync::Mutex<Option<HashMap<u64, Review>>> = std::sync::Mutex::new(None);

/// Server comparisons by job, from the moment they start: (what is
/// compared, the result once there). The task list reopens one while it is
/// still downloading, and the dialog follows the job instead of owning it.
type ServerResult = Result<(Vec<cf::File>, ServerPlan), String>;
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
    /// The comparison job this dialog follows.
    job: Option<u64>,
    /// That job's detail and progress while it runs.
    status: Option<(String, Option<f32>)>,
}

pub struct UpdatePreview {
    store: Entity<Store>,
    /// The folder being updated: the instance, or the server.
    root: PathBuf,
    server: Option<ServerTarget>,
    from: String,
    plan: Option<Result<UpdatePlan, String>>,
    /// Conflicting files where the pack's copy wins.
    take: HashSet<String>,
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
}

impl UpdatePreview {
    fn plan(&mut self, file: cf::File, cx: &mut Context<Self>) {
        let Some(inst) = self.store.read(cx).instance(&self.root).cloned() else {
            return;
        };
        let job = self
            .store
            .read(cx)
            .jobs()
            .start(format!("Comparing {} with {}", inst.name, file.display_name));
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move {
                    let r = hopper::packdiff::plan(&inst, &file, &job);
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
                    this.take = p.default_take();
                    this.turn_on = p.default_choices();
                    this.want_icons(p, cx);
                    if p.count(FileAction::Conflict) > 0 {
                        this.open.insert(Group::Files(FileAction::Conflict));
                    }
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
        let Some(t) = self.server.as_mut() else { return };
        t.splan = None;
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
                let p = hopper::server_pack_update::plan(&t.server, &t.inst, &targets[pick], &job)?;
                Ok::<_, anyhow::Error>((targets, p))
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
                                this.want_icons(&p, cx);
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
            Ok((targets, p)) => {
                self.take = p.plan.default_take();
                if p.plan.count(FileAction::Conflict) > 0 {
                    self.open.insert(Group::Files(FileAction::Conflict));
                }
                self.from = p.base.clone().unwrap_or_else(|| "this server".into());
                self.plan = Some(Ok(p.plan.clone()));
                if let Some(t) = self.server.as_mut() {
                    t.targets = targets;
                    t.splan = Some(p);
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
        let Some(p) = t.splan.clone() else { return false };
        if let Some(id) = t.job {
            server_reviews(|m| m.remove(&id));
        }
        let (server, take, client_root) = (t.server.clone(), v.take.clone(), t.inst.root.clone());
        let s = v.store.read(cx);
        let host = s.settings().server_host.clone();
        let java = hopper::launch::installer_java(std::path::Path::new(&s.settings().launcher_dir));
        let panel = hopper::panel::from_settings(s.settings());
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
        c = c.child(
            widgets::caption(
                "Server packs often leave out global datapacks (Paxi, OpenLoader) and configs of mods new to the pack.                  Server sync flags what still differs afterwards.",
                cx,
            )
            .text_xs(),
        );
        Some(c.into_any_element())
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
        let overrides = plan.decisions_with(&v.take, &v.turn_on);
        let (store, root, file) = (v.store.clone(), v.root.clone(), plan.file.clone());
        let backup = v.backup_worlds;
        if let Ok(mut g) = REVIEWS.lock()
            && let Some(m) = g.as_mut()
        {
            m.retain(|_, (r, _, _)| *r != root);
        }
        store.update(cx, |s, cx| {
            s.update_pack(&root, file, Some(overrides), backup);
            cx.notify();
        });
        true
    }

    fn group_header(
        &self,
        g: Group,
        tag: Tag,
        label: String,
        n: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let open = self.open.contains(&g);
        let theme = cx.theme();
        h_flex()
            .id(ElementId::Name(format!("group:{label}").into()))
            .gap_2()
            .py_1()
            .cursor_pointer()
            .when(n > 0, |r| r.hover(|s| s.text_color(theme.primary)))
            .when(n == 0, |r| r.opacity(0.5))
            .child(
                Icon::new(if open && n > 0 {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .small()
                .text_color(theme.muted_foreground),
            )
            .child(div().w_8().text_sm().font_semibold().child(n.to_string()))
            .child(tag)
            .child(div().text_sm().child(label))
            .on_click(cx.listener(move |this, _, _, cx| {
                if n > 0 {
                    this.toggle(g, cx)
                }
            }))
    }

    fn mods_group(&self, plan: &UpdatePlan, a: ModAction, cx: &mut Context<Self>) -> AnyElement {
        let (tag, label) = match a {
            ModAction::Add => (Tag::success().xsmall().child("Added"), "mods the pack adds"),
            ModAction::Update => (Tag::info().xsmall().child("Updated"), "mods updated"),
            ModAction::Remove => (
                Tag::danger().xsmall().child("Removed"),
                "mods the pack drops (moved to backups)",
            ),
        };
        let g = Group::Mods(a);
        let rows: Vec<_> = plan.mods.iter().filter(|m| m.action == a).cloned().collect();
        let header = self.group_header(g, tag, label.to_string(), rows.len(), cx);
        let open = self.open.contains(&g);
        v_flex()
            .child(header)
            .when(open, |c| {
                c.child(
                    v_flex().pl(rems(4.25)).pb_2().children(rows.into_iter().map(|m| {
                        let detail = match a {
                            ModAction::Update => format!("{}  →  {}", m.from, m.to),
                            ModAction::Add => m.to.clone(),
                            ModAction::Remove => m.from.clone(),
                        };
                        h_flex()
                            .gap_3()
                            .py_0p5()
                            .child(self.mod_cell(format!("mod:{}:{}", m.name, m.to), &m.name, &m.icon, m.project, cx))
                            .child(widgets::caption(detail, cx).text_xs().min_w_0())
                    })),
                )
            })
            .into_any_element()
    }

    fn files_group(&self, plan: &UpdatePlan, a: FileAction, cx: &mut Context<Self>) -> AnyElement {
        let (tag, label) = match a {
            FileAction::Add => (Tag::success().xsmall().child("New"), "new files from the pack"),
            FileAction::Update => (
                Tag::info().xsmall().child("Updated"),
                "files the pack changed and you didn't",
            ),
            FileAction::Merge => (
                Tag::info().xsmall().child("Merged"),
                "files both changed in different places: combined",
            ),
            FileAction::KeepYours => (
                Tag::secondary().xsmall().child("Yours"),
                "files only you changed: kept",
            ),
            FileAction::Remove => (
                Tag::danger().xsmall().child("Removed"),
                "files the pack dropped (moved to backups)",
            ),
            FileAction::KeepDropped => (
                Tag::secondary().xsmall().child("Kept"),
                "files the pack dropped but you changed: kept",
            ),
            FileAction::Conflict => (
                Tag::warning().xsmall().child("Both changed"),
                "files you and the pack both changed",
            ),
        };
        let g = Group::Files(a);
        let rows: Vec<String> = plan
            .files
            .iter()
            .filter(|f| f.action == a)
            .map(|f| f.rel.clone())
            .collect();
        let header = self.group_header(g, tag, label.to_string(), rows.len(), cx);
        let open = self.open.contains(&g);
        let body: Option<AnyElement> = open.then(|| {
            if a == FileAction::Conflict {
                self.conflicts(rows, cx)
            } else {
                v_flex()
                    .pl(rems(4.25))
                    .pb_2()
                    .children(rows.into_iter().map(|r| {
                        widgets::caption(r, cx).text_xs().py_0p5()
                    }))
                    .into_any_element()
            }
        });
        v_flex().child(header).children(body).into_any_element()
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
                d.cursor_pointer()
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
                    h_flex()
                        .gap_3()
                        .py_0p5()
                        .child(this.mod_cell(format!("cell:{}", m.key), &m.name, &m.icon, m.project, cx))
                        .child(widgets::caption(detail, cx).text_xs().flex_1().min_w_0())
                        .child(
                            TabBar::new(SharedString::from(format!("pick:{}", m.key)))
                                .segmented()
                                .xsmall()
                                .selected_index(usize::from(on))
                                .child(Tab::new().label(labels[0]))
                                .child(Tab::new().label(labels[1]))
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
        let theme = cx.theme();
        let all = rows.clone();
        let all2 = rows.clone();
        v_flex()
            .pl(rems(4.25))
            .pb_2()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .pb_1()
                    .child(widgets::caption(
                        "Pack content folders (quests, scripts) default to the pack's copy; settings default to yours. Replaced files go to .hopper-backups.",
                        cx,
                    ).text_xs().flex_1())
                    .child(
                        Button::new("keep-all")
                            .xsmall()
                            .ghost()
                            .label("Keep all mine")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                for r in &all {
                                    this.take.remove(r);
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("take-all")
                            .xsmall()
                            .ghost()
                            .label("Take all the pack's")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.take.extend(all2.iter().cloned());
                                cx.notify();
                            })),
                    ),
            )
            .children(rows.into_iter().map(|rel| {
                let theirs = self.take.contains(&rel);
                let diff = self.diffs.get(&rel).cloned();
                let (r1, r2) = (rel.clone(), rel.clone());
                v_flex()
                    .child(
                        h_flex()
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
                                    .child(Tab::new().label("Mine"))
                                    .child(Tab::new().label("Pack's"))
                                    .on_click(cx.listener(move |this, ix: &usize, _, cx| {
                                        if *ix == 1 {
                                            this.take.insert(r2.clone());
                                        } else {
                                            this.take.remove(&r2);
                                        }
                                        cx.notify();
                                    })),
                            ),
                    )
                    .when_some(diff, |c, d| {
                        let lines: Vec<String> = d
                            .unwrap_or("Not a text file.".into())
                            .lines()
                            .take(400)
                            .map(str::to_string)
                            .collect();
                        c.child(
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
                                })),
                        )
                    })
            }))
            .into_any_element()
    }
}

impl Render for UpdatePreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Window-tall once loaded; the list takes the rest and scrolls (a
        // max height on the scroll area alone doesn't scroll).
        let total_h = (window.viewport_size().height - window.rem_size() * 8.).max(window.rem_size() * 16.);
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
                    "Downloading both pack versions and comparing them with your instance",
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
                let mods: Vec<AnyElement> = [ModAction::Add, ModAction::Update, ModAction::Remove]
                    .into_iter()
                    .map(|a| self.mods_group(&plan, a, cx))
                    .collect();
                let files: Vec<AnyElement> = [
                    FileAction::Conflict,
                    FileAction::Merge,
                    FileAction::Update,
                    FileAction::Add,
                    FileAction::KeepYours,
                    FileAction::Remove,
                    FileAction::KeepDropped,
                ]
                .into_iter()
                .map(|a| self.files_group(&plan, a, cx))
                .collect();
                let theme = cx.theme();
                let mc_change = self.mc_change.clone();
                let backup = self.backup_worlds;
                v_flex()
                    .gap_4()
                    .when_some(mc_change, |c, (from, to)| {
                        c.child(
                            v_flex()
                                .gap_2()
                                .px_3()
                                .py_2p5()
                                .rounded(theme.radius)
                                .border_1()
                                .border_color(theme.warning.opacity(0.5))
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .text_sm()
                                        .text_color(theme.warning)
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
                    .when(!plan.base_known, |c| {
                        c.child(
                            h_flex()
                                .gap_2()
                                .text_sm()
                                .text_color(theme.warning)
                                .child(Icon::new(IconName::TriangleAlert).small())
                                .child(if server_mode {
                                    "Hopper can't tell which server pack this server came from, so every file that differs counts as both changed."
                                } else {
                                    "The installed pack version couldn't be downloaded, so every file that differs counts as both changed."
                                }),
                        )
                    })
                    .children(self.your_choices(&plan, cx))
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().font_semibold().child("Mods"))
                            .children(mods)
                            .children({
                                let kept_text = if let Some(k) = self.server.as_ref().and_then(|t| t.splan.as_ref()).map(|p| p.kept) {
                                    let mut t = format!("{} mods that aren't in the server pack stay.", k.added);
                                    if k.yours > 0 {
                                        t.push_str(&format!("\n{} mods where the server runs another version and the pack didn't change it keep the server's.", k.yours));
                                    }
                                    if k.left_off > 0 {
                                        t.push_str(&format!("\n{} pack mods the server doesn't have stay off.", k.left_off));
                                    }
                                    t
                                } else {
                                    format!("{} mods you added stay as they are.", plan.kept_mods)
                                };
                                kept_text
                                    .lines()
                                    .map(|l| widgets::caption(l.to_string(), cx).text_xs().pl(rems(1.5)))
                                    .collect::<Vec<_>>()
                            }),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().font_semibold().child("Config and other files"))
                            .children(files),
                    )
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
    let view = cx.new(|cx| {
        let mut v = UpdatePreview {
            store,
            root,
            server: None,
            from,
            plan: None,
            take: HashSet::new(),
            turn_on: HashSet::new(),
            peek: None,
            _peek_watch: None,
            open: HashSet::new(),
            diffs: HashMap::new(),
            mc_change,
            backup_worlds,
        };
        match plan {
            Some(r) => {
                if let Ok(p) = &r {
                    v.take = p.default_take();
                    v.turn_on = p.default_choices();
                    v.want_icons(p, cx);
                    if p.count(FileAction::Conflict) > 0 {
                        v.open.insert(Group::Files(FileAction::Conflict));
                    }
                }
                v.plan = Some(r);
            }
            None => v.plan(file, cx),
        }
        v
    });
    let title = format!("Update {}", inst.name);
    window.open_dialog(cx, move |dialog, window, cx| {
        let ready = matches!(view.read(cx).plan, Some(Ok(_)));
        let v2 = view.clone();
        dialog
            .title(title.clone())
            .w(rems(if view.read(cx).peek.is_some() { 86. } else { 54. }).to_pixels(window.rem_size()))
            .child(view.clone())
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("upd-cancel").outline().label("Cancel")))
                    .child(DialogAction::new().child(
                        Button::new("upd-go")
                            .primary()
                            .icon(IconName::Download)
                            .label("Update")
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
        let mut v = UpdatePreview {
            store,
            root: server.root.clone(),
            server: Some(ServerTarget { job, ..t }),
            from: String::new(),
            plan: None,
            take: HashSet::new(),
            turn_on: HashSet::new(),
            peek: None,
            _peek_watch: None,
            open: HashSet::new(),
            diffs: HashMap::new(),
            mc_change: None,
            backup_worlds: false,
        };
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
