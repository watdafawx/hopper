//! Extras around the addon table: global resource pack folders (Paxi and
//! friends), required mods that are missing, shader notes and mod profiles.

use crate::addons::{AddonsEvent, AddonsView};
use crate::crash_help::callout;
use crate::widgets::{self, Clickable as _};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::cf;
use hopper::deps::Missing;
use hopper::instance_ops::PackFile;
use hopper::install::Update;
use hopper::instances::{Addon, Instance};
use std::path::PathBuf;

/// Global resource pack folders and the one shown.
#[derive(Default)]
pub struct GlobalPacks {
    /// (folder relative to the instance, loader name)
    pub dirs: Vec<(&'static str, &'static str)>,
    /// `None` shows the instance's own packs.
    pub picked: Option<&'static str>,
    /// Each folder's packs, read in the background (a big Paxi folder
    /// takes a few hundred ms of zip reading), so switching tabs is instant.
    pub packs: std::collections::HashMap<&'static str, Vec<PackFile>>,
    /// How many packs each of `dirs` holds.
    pub counts: Vec<usize>,
    /// The instance `packs` was read for.
    pub root: Option<PathBuf>,
}

/// Hopper's own working files in an instance folder (backups, staging,
/// trash, pending installs), never shown as content.
pub fn is_hopper_file(path: &std::path::Path) -> bool {
    path.file_name()
        .is_some_and(|n| n.to_string_lossy().to_ascii_lowercase().starts_with(".hopper-"))
}

/// A folder's packs, without Hopper's own files.
pub fn pack_files(dir: &std::path::Path) -> Vec<PackFile> {
    let mut v = hopper::instance_ops::pack_files(dir);
    v.retain(|p| !is_hopper_file(&p.path));
    v
}

/// A folder's data packs, without Hopper's own files.
pub fn datapacks(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut v = hopper::instance_ops::datapacks(dir);
    v.retain(|p| !is_hopper_file(p));
    v
}

/// A pack file as a table row: what the lookup found it to be (project,
/// author, version) and its update. `from_pack` false marks it "Yours".
pub(crate) fn pack_addon(p: &PackFile, class: u32, from_pack: bool) -> (Addon, Option<Update>) {
    let file = p.path.file_name().unwrap_or_default().to_string_lossy().to_string();
    let enabled = !file.ends_with(".disabled");
    let file_name = file.trim_end_matches(".disabled").to_string();
    let mut a = Addon {
        file_name: file_name.clone(),
        name: p.name.trim_end_matches(".disabled").to_string(),
        icon: p.icon.as_ref().map(|i| i.display().to_string()).unwrap_or_default(),
        enabled,
        from_pack,
        class_id: class,
        date_added: hopper::time::iso_from_secs(p.modified),
        ..Default::default()
    };
    let mut update = None;
    if let Some(f) = crate::pack_lookup::found(p).flatten() {
        let m = &f.project;
        a.project_id = m.id;
        a.name = m.name.clone();
        a.author = m.authors.first().map(|x| x.name.clone()).unwrap_or_default();
        a.icon = m.icon().map(str::to_string).unwrap_or(a.icon);
        a.page = m.page();
        a.mr_project = m.modrinth.as_ref().map(|_| m.slug.clone()).unwrap_or_default();
        if let Some(i) = &f.version.installed {
            a.file_id = i.id;
            a.file_date = i.file_date.clone();
        }
        if let Some(u) = &f.version.update {
            update = Some(Update { project_id: m.id, name: m.name.clone(), from: file_name.clone(), file: u.clone() });
        }
    }
    (a, update)
}

/// Required mods that are missing, for the mods folder as of `mtime`.
#[derive(Default)]
pub struct MissingDeps {
    pub root: Option<PathBuf>,
    pub mtime: Option<std::time::SystemTime>,
    pub list: Vec<Missing>,
    /// Jars that are the same mod twice.
    pub dups: Vec<hopper::install::Duplicate>,
    /// What else the loader will refuse (too old a loader, mods that
    /// refuse each other, jars it can't read).
    pub blockers: Vec<hopper::deps::Blocker>,
    /// Old files a running game holds, leaving when it closes.
    pub queued: usize,
    /// When the game last loaded fine with these exact mods: the banners
    /// then say "may", not "will" (metadata misses runtime stand-ins).
    pub loaded_fine: Option<i64>,
    /// The loader and Minecraft the blockers were checked against: a new
    /// loader checks again.
    pub against: (String, String),
    /// Wrong-version rows whose asked-for version nobody published (mod
    /// id and range → a version that does exist, as an example).
    pub ghosts: std::collections::HashMap<(String, String), String>,
}

impl AddonsView {
    /// Reads which global pack folders an instance has.
    pub(crate) fn load_globals(&mut self, inst: &Instance, cx: &mut Context<Self>) {
        let dirs = hopper::instance_ops::global_resourcepack_dirs(inst);
        let picked = self
            .globals
            .picked
            .filter(|p| dirs.iter().any(|(d, _)| d == p));
        let counts = dirs
            .iter()
            .map(|(d, _)| datapacks(&inst.root.join(d)).len())
            .collect();
        // Same instance: the old lists stay up until the new ones are read.
        let packs = if self.globals.root.as_ref() == Some(&inst.root) {
            std::mem::take(&mut self.globals.packs)
        } else {
            Default::default()
        };
        self.globals = GlobalPacks {
            dirs: dirs.clone(),
            picked,
            packs,
            counts,
            root: Some(inst.root.clone()),
        };
        if dirs.is_empty() || self.class != cf::CLASS_RESOURCE_PACKS {
            return;
        }
        let root = inst.root.clone();
        let inst = inst.clone();
        cx.spawn(async move |this, cx| {
            let r2 = root.clone();
            let read = cx
                .background_spawn(async move {
                    dirs.iter()
                        .map(|(d, _)| (*d, pack_files(&r2.join(d))))
                        .collect::<std::collections::HashMap<_, _>>()
                })
                .await;
            this.update(cx, |this, cx| {
                if this.globals.root.as_ref() != Some(&root) {
                    return;
                }
                for (i, (d, _)) in this.globals.dirs.iter().enumerate() {
                    if let (Some(v), Some(n)) = (read.get(d), this.globals.counts.get_mut(i)) {
                        *n = v.len();
                    }
                }
                this.globals.packs = read;
                this.apply_global(cx);
            })
            .ok();
            // Then which projects they are, their versions and updates.
            let all: Vec<PackFile> = this
                .update(cx, |this, _| this.globals.packs.values().flatten().cloned().collect())
                .unwrap_or_default();
            let all2 = all.clone();
            let urls = cx
                .background_spawn(async move { crate::pack_lookup::look_up(&all2, cf::CLASS_RESOURCE_PACKS) })
                .await;
            this.update(cx, |this, cx| {
                this.store.update(cx, |s, _| urls.iter().for_each(|u| s.want_image(u)));
                this.apply_global(cx);
            })
            .ok();
            if cx.background_spawn(async move { crate::pack_lookup::check_versions(&all, &inst) }).await {
                this.update(cx, |this, cx| this.apply_global(cx)).ok();
            }
        })
        .detach();
    }

    /// Looks for missing required mods when the mods folder changed.
    pub(crate) fn want_missing(&mut self, inst: &Instance, cx: &mut Context<Self>) {
        // "Revert X" on the problem notice, worked out in the store.
        self.store.update(cx, |s, _| s.want_reverts(&inst.root, &inst.loader, &inst.game_version, &inst.addons, true));
        let dir = inst.mods_dir();
        let mtime = std::fs::metadata(&dir).and_then(|m| m.modified()).ok();
        let against = (inst.loader.clone(), inst.game_version.clone());
        if self.missing.root.as_ref() == Some(&inst.root) && self.missing.mtime == mtime && self.missing.against == against {
            return;
        }
        // Same instance: the old list stays up while the folder is read
        // again, so the banner doesn't flash on every update.
        let same = self.missing.root.as_ref() == Some(&inst.root);
        self.missing = MissingDeps {
            root: Some(inst.root.clone()),
            mtime,
            list: if same { std::mem::take(&mut self.missing.list) } else { Vec::new() },
            dups: if same { std::mem::take(&mut self.missing.dups) } else { Vec::new() },
            blockers: if same { std::mem::take(&mut self.missing.blockers) } else { Vec::new() },
            queued: hopper::locked::list(&inst.root).len(),
            loaded_fine: if same { self.missing.loaded_fine } else { None },
            against: against.clone(),
            ghosts: if same { std::mem::take(&mut self.missing.ghosts) } else { Default::default() },
        };
        let root = inst.root.clone();
        let snapshot = inst.clone();
        cx.spawn(async move |this, cx| {
            let (list, dups, blockers, loaded_fine) = cx
                .background_spawn(async move {
                    (
                        hopper::deps::missing(&dir),
                        hopper::install::duplicates(&snapshot),
                        hopper::deps::blockers(&dir, &snapshot.loader, &snapshot.game_version),
                        hopper::deps::loaded_fine(&snapshot.root),
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                if this.missing.root.as_ref() == Some(&root) && this.missing.mtime == mtime && this.missing.against == against {
                    this.missing.dups = dups;
                    this.want_ghosts(&blockers, cx);
                    this.missing.blockers = blockers;
                    this.missing.loaded_fine = loaded_fine;
                    // Wrong versions are warned about when an update causes
                    // them; packs often ship a few that run anyway.
                    let list: Vec<_> = list.into_iter().filter(|m| m.wrong_version.is_none()).collect();
                    if this.deps_holding() {
                        this.missing_next = Some(list);
                    } else {
                        this.missing.list = list;
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The global folder picked (Paxi's), when Resource packs shows one.
    pub(crate) fn global_dir(&self) -> Option<PathBuf> {
        if let Some(rel) = self.folder.as_ref().filter(|_| self.class == cf::CLASS_DATAPACKS) {
            return Some(self.root.as_ref()?.join(rel));
        }
        let rel = self.globals.picked.filter(|_| self.class == cf::CLASS_RESOURCE_PACKS)?;
        Some(self.root.as_ref()?.join(rel))
    }

    /// Reads the data pack folder shown, then which projects its packs
    /// are, then their versions, filling the table at each step.
    pub(crate) fn read_folder(&mut self, cx: &mut Context<Self>) {
        let (Some(dir), Some(inst)) = (
            self.global_dir(),
            self.root.as_ref().and_then(|r| self.store.read(cx).instance(r)).map(|i| i.clone_header()),
        ) else {
            return;
        };
        let rel = self.folder.clone();
        cx.spawn(async move |this, cx| {
            let packs = cx.background_spawn(async move { pack_files(&dir) }).await;
            let still = |this: &AddonsView| this.folder == rel;
            let ok = this
                .update(cx, |this, cx| {
                    if still(this) {
                        this.folder_packs = Some(packs.clone());
                        this.apply_global(cx);
                    }
                    still(this)
                })
                .unwrap_or(false);
            if !ok {
                return;
            }
            let p2 = packs.clone();
            let urls = cx
                .background_spawn(async move { crate::pack_lookup::look_up(&p2, cf::CLASS_DATAPACKS) })
                .await;
            this.update(cx, |this, cx| {
                this.store.update(cx, |s, _| urls.iter().for_each(|u| s.want_image(u)));
                this.apply_global(cx);
            })
            .ok();
            if cx.background_spawn(async move { crate::pack_lookup::check_versions(&packs, &inst) }).await {
                this.update(cx, |this, cx| this.apply_global(cx)).ok();
            }
        })
        .detach();
    }

    /// With a global folder picked, the table shows its packs instead of
    /// the instance's: each as a row like an installed pack, from what
    /// the lookup found (project, author, version, update).
    pub(crate) fn apply_global(&mut self, cx: &mut Context<Self>) {
        let class = self.class;
        let packs: Vec<PackFile> = if class == cf::CLASS_DATAPACKS && self.folder.is_some() {
            self.folder_packs.clone().unwrap_or_default()
        } else if let Some(rel) = self.globals.picked.filter(|_| class == cf::CLASS_RESOURCE_PACKS) {
            self.globals.packs.get(rel).cloned().unwrap_or_default()
        } else {
            return;
        };
        let Some(real) = self.root.as_ref().and_then(|r| self.store.read(cx).instance(r)) else {
            return;
        };
        let mut inst = real.clone_header();
        let mut updates = std::collections::HashMap::new();
        for p in &packs {
            let (a, u) = pack_addon(p, class, true);
            if let Some(u) = u {
                updates.insert(u.project_id, u);
            }
            inst.addons.push(a);
        }
        self.table.inst = Some(std::rc::Rc::new(inst));
        self.table.updates = std::rc::Rc::new(updates);
        self.table.active_rp = Default::default();
        self.table.rows_for = None;
        self.ensure_rows();
        self.fill_also();
        self.want_images(cx);
        cx.notify();
    }

    /// The path of `file` (as listed, without ".disabled") in the global
    /// folder, as it is on disk now.
    pub(crate) fn global_path(dir: &std::path::Path, file: &str) -> PathBuf {
        let on = dir.join(file);
        if on.exists() { on } else { dir.join(format!("{file}.disabled")) }
    }

    /// Switches packs in the global folder on or off: Paxi skips files
    /// ending in ".disabled".
    pub(crate) fn toggle_global(&mut self, dir: PathBuf, files: Vec<String>, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        for f in &files {
            let from = Self::global_path(&dir, f);
            let to = if on { dir.join(f) } else { dir.join(format!("{f}.disabled")) };
            if from != to
                && let Err(e) = std::fs::rename(&from, &to)
            {
                window.push_notification(Notification::error(format!("{f}: {e}")), cx);
            }
        }
        self.reload_globals(cx);
    }

    /// Moves packs out of the global folder into the instance's backups.
    pub(crate) fn remove_global(&mut self, dir: PathBuf, files: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let mut moved = 0;
        for f in &files {
            match hopper::sync::backup_into(&root, &Self::global_path(&dir, f)) {
                Ok(_) => moved += 1,
                Err(e) => window.push_notification(Notification::error(format!("{f}: {e:#}")), cx),
            }
        }
        if moved > 0 {
            let what = if moved == 1 { files[0].clone() } else { format!("{moved} packs") };
            window.push_notification(Notification::success(format!("Moved {what} to .hopper-backups")), cx);
        }
        self.reload_globals(cx);
    }

    /// Updates packs in the global folder in place (the old file goes to
    /// backups).
    pub(crate) fn update_global(&mut self, dir: PathBuf, updates: Vec<Update>, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let Some(inst) = self.table.inst.clone() else { return };
        for u in updates {
            let Some(a) = inst.addons.iter().find(|a| a.project_id == u.project_id) else { continue };
            let p = PackFile { path: Self::global_path(&dir, &a.file_name), ..Default::default() };
            let project = cf::Mod { id: a.project_id, name: a.name.clone(), ..Default::default() };
            let found = crate::pack_lookup::Found {
                project,
                version: hopper::instance_ops::PackVersion { installed: None, update: Some(u.file.clone()) },
            };
            crate::pack_lookup::update(&self.store, root.clone(), &p, &found, cx);
        }
    }

    /// Reads the global folders again (after a change made here).
    pub(crate) fn reload_globals(&mut self, cx: &mut Context<Self>) {
        if self.folder.is_some() && self.class == cf::CLASS_DATAPACKS {
            return self.read_folder(cx);
        }
        if let Some(inst) = self.root.as_ref().and_then(|r| self.store.read(cx).instance(r)).cloned() {
            self.load_globals(&inst, cx);
        }
    }

    /// "N mods are in twice", with Keep newest; and old files waiting for
    /// the game to let go of them.
    pub(crate) fn duplicate_banner(&self, inst: &Instance, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.class != cf::CLASS_MODS || self.missing.root.as_ref() != Some(&inst.root) {
            return None;
        }
        let dups = &self.missing.dups;
        let queued = self.missing.queued;
        if dups.is_empty() && queued == 0 {
            return None;
        }
        let theme = cx.theme();
        let place = inst.root.to_string_lossy();
        if dups.is_empty() {
            let text = format!(
                "{queued} old file{} the game still has open will be removed when it closes.",
                if queued == 1 { "" } else { "s" }
            );
            let key = hopper::settings::notice_key(&place, "queued", &text);
            let body = callout(theme.info, cx).child(div().text_sm().child(text));
            let notice = widgets::closable_notice(body, key, widgets::Closed::Gone, "", theme.info, &self.store, cx)?;
            return Some(div().px_6().child(notice).into_any_element());
        }
        let dup_key = hopper::settings::notice_key(
            &place,
            "dups",
            &dups.iter().map(|d| d.files.join(",")).collect::<Vec<_>>().join(";"),
        );
        let extra: Vec<String> = dups.iter().flat_map(|d| d.files[1..].to_vec()).collect();
        let rows = dups.iter().map(|d| {
            let older = d.files[1..].to_vec();
            let what = d.name.clone();
            let addon = inst.addons.iter().find(|a| a.file_name == d.files[0]);
            let (pid, icon) = (
                addon.map_or(0, |a| a.project_id),
                addon.and_then(|a| self.store.read(cx).image(&a.icon)).map(std::path::Path::to_path_buf),
            );
            let view = cx.entity();
            h_flex()
                .gap_3()
                .text_sm()
                .child(
                    widgets::mod_link(
                        SharedString::from(format!("dup:{}", d.files[0])),
                        d.name.clone(),
                        icon.as_deref(),
                        pid,
                        move |p, _, cx| view.update(cx, |this, cx| this.open_peek(p, cx)),
                        cx,
                    )
                    .font_semibold(),
                )
                .child(
                    widgets::caption(
                        format!("keep {}  ·  remove {}", d.files[0], older.join(", ")),
                        cx,
                    )
                    .flex_1()
                    .text_xs(),
                )
                .child(
                    Button::new(SharedString::from(format!("keep-newest:{}", d.files[0])))
                        .xsmall()
                        .outline()
                        .label("Keep newest")
                        .tooltip("Move the older jars to Hopper's backups")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.remove(older.clone(), what.clone(), window, cx)
                        })),
                )
                .into_any_element()
        });
        let n = dups.len();
        let title = format!("{n} mod{} in twice: the game won't start with both", if n == 1 { " is" } else { "s are" });
        let warning = theme.warning;
        let body = callout(theme.warning, cx)
                        .child(
                            h_flex()
                                .gap_3()
                                .child(
                                    div()
                                        .flex_1()
                                        .font_semibold()
                                        .text_color(theme.warning)
                                        .child(title.clone()),
                                )
                                .when(n > 1, |r| {
                                    r.child(
                                        Button::new("keep-newest-all")
                                            .small()
                                            .primary()
                                            .label("Keep newest of all")
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.remove(extra.clone(), format!("{n} older duplicates"), window, cx)
                                            })),
                                    )
                                }),
                        )
                        .when(queued > 0, |c| {
                            c.child(widgets::caption(
                                format!("{queued} old file{} will be removed when the game closes.", if queued == 1 { "" } else { "s" }),
                                cx,
                            ))
                        })
                        .child(
                            div()
                                .id("dup-rows")
                                .when(n > 6, |d| d.h(rems(10.5)))
                                .overflow_y_scrollbar()
                                .child(v_flex().gap_2().children(rows)),
                        );
        let notice = widgets::closable_notice(body, dup_key, widgets::Closed::Chip, title, warning, &self.store, cx)?;
        Some(div().px_6().child(notice).into_any_element())
    }

    /// "N required mods are missing", with Enable or Find for each.
    pub(crate) fn missing_banner(&self, inst: &Instance, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.class != cf::CLASS_MODS
            || self.missing.root.as_ref() != Some(&inst.root)
            || self.missing.list.is_empty()
        {
            return None;
        }
        let theme = cx.theme();
        let loaded_fine = self.missing.loaded_fine;
        let n = self.missing.list.len();
        let mut installable: Vec<String> = self
            .missing
            .list
            .iter()
            .filter(|m| m.disabled_provider.is_none())
            .map(|m| m.needs.clone())
            .collect();
        installable.dedup();
        // Not what's installing or just installed.
        installable.retain(|i| !self.dep_jobs.contains_key(i) && !self.dep_done.contains(i));
        // "Install all" is the banner's one filled button; beside it each
        // line's Install is an outline.
        let one_primary = installable.len() <= 1;
        // One row per mod, naming everything it lacks.
        let mut groups: Vec<(&str, Vec<&hopper::deps::Missing>)> = Vec::new();
        for m in &self.missing.list {
            match groups.iter_mut().find(|(name, _)| *name == m.mod_name) {
                Some((_, v)) => v.push(m),
                None => groups.push((&m.mod_name, vec![m])),
            }
        }
        // Each missing mod's project, for its name, icon and page: looked
        // up once in the background, the way Install finds it.
        let unknown: Vec<String> = {
            let known = self.dep_projects.borrow();
            let mut v: Vec<String> = self.missing.list.iter().map(|m| m.needs.clone()).filter(|id| !known.contains_key(id)).collect();
            v.dedup();
            v
        };
        if !unknown.is_empty() {
            {
                let mut known = self.dep_projects.borrow_mut();
                for id in &unknown {
                    known.insert(id.clone(), None);
                }
            }
            let inst2 = inst.clone();
            cx.spawn(async move |this, cx| {
                let found = cx
                    .background_spawn(async move {
                        unknown
                            .into_iter()
                            .map(|id| {
                                let p = hopper::install::project_for_mod_id(&inst2, &id)
                                    .ok()
                                    .flatten()
                                    .map(|m| (m.id, m.name.clone(), m.icon().unwrap_or_default().to_string()));
                                (id, p)
                            })
                            .collect::<Vec<_>>()
                    })
                    .await;
                this.update(cx, |this, cx| {
                    for (id, p) in found {
                        if let Some((_, _, icon)) = &p
                            && !icon.is_empty()
                        {
                            let icon = icon.clone();
                            this.store.update(cx, |s, _| s.want_image(&icon));
                        }
                        this.dep_projects.borrow_mut().insert(id, p);
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        let rows: Vec<AnyElement> = groups
            .iter()
            .map(|(name, needs)| {
                // The installed mod's own page, when it came from a site.
                let pid = inst
                    .addons
                    .iter()
                    .find(|a| a.file_name.eq_ignore_ascii_case(&needs[0].file_name))
                    .map_or(0, |a| a.project_id);
                let chips = needs.iter().map(|m| {
                    let action = match &m.disabled_provider {
                        Some(f) => {
                            let f = f.clone();
                            Button::new(SharedString::from(format!("enable:{}:{}", name, m.needs)))
                                .xsmall()
                                .outline()
                                .label("Enable it")
                                .tooltip(f.clone())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.enable_provider(&f, window, cx)
                                }))
                                .into_any_element()
                        }
                        None if self.dep_done.contains(&m.needs) => {
                            widgets::done_mark(SharedString::from(format!("got:{}:{}", name, m.needs)), "Installed", true, cx)
                                .into_any_element()
                        }
                        None if self.dep_jobs.contains_key(&m.needs) => {
                            let p = self
                                .dep_jobs
                                .get(&m.needs)
                                .and_then(|j| self.store.read(cx).job_state(*j))
                                .and_then(|j| j.progress);
                            let label = match p {
                                Some(p) => format!("Installing {}%", (p * 100.0).floor() as u32),
                                None => "Installing".to_string(),
                            };
                            widgets::progress_fill(
                                Button::new(SharedString::from(format!("getting:{}:{}", name, m.needs)))
                                    .xsmall()
                                    .outline()
                                    .loading(true)
                                    .label(label)
                                    .disabled(true),
                                p,
                                cx,
                            )
                            .into_any_element()
                        }
                        None => {
                            let id = m.needs.clone();
                            Button::new(SharedString::from(format!("get:{}:{}", name, m.needs)))
                                .xsmall()
                                .map(|b| if one_primary { b.primary() } else { b.outline() })
                                .icon(IconName::Download)
                                .label("Install")
                                .tooltip("Download it from Modrinth or CurseForge, with what it needs")
                                .on_click(cx.listener(move |this, _, _, cx| this.install_missing(vec![id.clone()], cx)))
                                .into_any_element()
                        }
                    };
                    let dep = m.needs.clone();
                    // Its project's name and icon once looked up; the mod id
                    // (in the tooltip then) until.
                    let project = self.dep_projects.borrow().get(&m.needs).cloned().flatten();
                    let icon = project
                        .as_ref()
                        .and_then(|(_, _, i)| self.store.read(cx).image(i))
                        .map(std::path::Path::to_path_buf);
                    let tip: SharedString = match &project {
                        Some(_) => format!("{}\nDetails", m.needs).into(),
                        None => "Details".into(),
                    };
                    h_flex()
                        .gap_1p5()
                        .child(
                            h_flex()
                                .id(SharedString::from(format!("dep:{}:{}", name, m.needs)))
                                .gap_1p5()
                                .child(widgets::thumb(icon.as_deref(), rems(1.25), cx))
                                .child(match &project {
                                    Some((_, pname, _)) => div().font_semibold().child(pname.clone()),
                                    None => div().font_family(theme.mono_font_family.clone()).child(m.needs.clone()),
                                })
                                .clickable(cx)
                                .hover(|s| s.text_color(theme.primary))
                                .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let found = this.dep_projects.borrow().get(&dep).cloned().flatten();
                                    match found {
                                        Some((pid, _, _)) => this.open_peek(pid, cx),
                                        None => this.peek_mod_id(dep.clone(), cx),
                                    }
                                })),
                        )
                        .when(!m.versions.is_empty() && m.versions != "*", |r| {
                            r.child(widgets::caption(m.versions.clone(), cx).text_xs())
                        })
                        .child(action)
                        .map(|chip| {
                            // Installed, fading out with the others once the
                            // user stops clicking.
                            if self.dep_leaving.as_ref().is_some_and(|(_, ids)| ids.contains(&m.needs)) {
                                widgets::fade_out(SharedString::from(format!("dep-leave:{}:{}", name, m.needs)), chip)
                            } else {
                                chip.into_any_element()
                            }
                        })
                });
                h_flex()
                    .flex_wrap()
                    .gap_x_4()
                    .gap_y_1()
                    .text_sm()
                    .child(
                        h_flex()
                            .gap_2()
                            .child({
                                let view = cx.entity();
                                let icon = inst
                                    .addons
                                    .iter()
                                    .find(|a| a.project_id == pid && pid != 0 || a.name == *name)
                                    .and_then(|a| self.store.read(cx).image(&a.icon))
                                    .map(std::path::Path::to_path_buf);
                                widgets::mod_link(
                                    SharedString::from(format!("needer:{name}")),
                                    name.to_string(),
                                    icon.as_deref(),
                                    pid,
                                    move |p, _, cx| view.update(cx, |this, cx| this.open_peek(p, cx)),
                                    cx,
                                )
                                .font_semibold()
                            })
                            .child(widgets::caption("needs", cx)),
                    )
                    .children(chips)
                    .into_any_element()
            })
            .collect();
        let n_rows = rows.len();
        let color = if loaded_fine.is_some() { theme.info } else { theme.warning };
        let title = match loaded_fine {
            Some(_) => format!(
                "{n} mod{} list{} a requirement that isn't installed",
                if n == 1 { "" } else { "s" },
                if n == 1 { "s" } else { "" }
            ),
            None => format!(
                "{n} required mod{} missing: the game will stop at loading",
                if n == 1 { " is" } else { "s are" }
            ),
        };
        let key = hopper::settings::notice_key(
            &inst.root.to_string_lossy(),
            "missing",
            &self.missing.list.iter().map(|m| format!("{}>{}", m.mod_name, m.needs)).collect::<Vec<_>>().join(";"),
        );
        let hover = cx.listener(|this, h: &bool, _, _| this.settle.pointer_in = *h);
        let body = callout(color, cx)
                        .child(
                            h_flex()
                                .gap_3()
                                .child(
                                    div()
                                        .flex_1()
                                        .font_semibold()
                                        .text_color(color)
                                        .child(title.clone()),
                                )
                                .when(installable.len() > 1, |r| {
                                    r.child(
                                        Button::new("install-all-missing")
                                            .small()
                                            .primary()
                                            .icon(IconName::Download)
                                            .label(format!("Install all {}", installable.len()))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.install_missing(installable.clone(), cx)
                                            })),
                                    )
                                }),
                        )
                        .when_some(loaded_fine, |c, at| c.child(widgets::caption(loaded_fine_note(at), cx)))
                        // Past six, the rows scroll so the mods table stays in view.
                        .child(
                            div()
                                .id("missing-rows")
                                .when(n_rows > 6, |d| d.h(rems(10.5)))
                                .overflow_y_scrollbar()
                                .child(v_flex().gap_2().children(rows)),
                        );
        let notice = widgets::closable_notice(body, key, widgets::Closed::Chip, title, color, &self.store, cx)?;
        Some(div().id("missing-banner").on_hover(hover).px_6().child(notice).into_any_element())
    }

    /// Downloads mods jars ask for by id; one that can't be found is
    /// reported and the rest still go in. A failure offers a search.
    pub(crate) fn install_missing(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let store = self.store.read(cx);
        let Some(mut inst) = store.instance(&root).cloned() else { return };
        let title = match ids.as_slice() {
            [one] => format!("Installing {one}"),
            many => format!("Installing {} missing mods", many.len()),
        };
        let ids: Vec<String> = ids
            .into_iter()
            .filter(|i| !self.dep_jobs.contains_key(i) && !self.dep_done.contains(i))
            .collect();
        if ids.is_empty() {
            return;
        }
        self.settle.clicked();
        let for_chips = ids.clone();
        let job = store.spawn_instance_job_id(&root, title, move |job| {
            let mut got = 0;
            for id in &ids {
                match hopper::install::install_mod_id(&mut inst, id, job) {
                    Ok(added) => {
                        got += 1;
                        job.note(format!("Installed {}", added.join(", ")));
                    }
                    Err(e) => job.note(format!("{id}: {e:#}. Try Browse to find it.")),
                }
            }
            if got == 0 {
                anyhow::bail!("Couldn't find {} on Modrinth or CurseForge. Search for it in Browse.", ids.join(", "));
            }
            Ok(crate::store::Msg::InstanceChanged(inst.root.clone()))
        });
        for id in for_chips {
            self.dep_jobs.insert(id, job);
        }
        self.start_ticker(cx);
        cx.notify();
    }

    /// The project panel for a mod id a jar asks for, looked up like
    /// Install does; one that can't be found is searched for in Browse.
    fn peek_mod_id(&mut self, mod_id: String, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let Some(inst) = self.store.read(cx).instance(&root).cloned() else { return };
        cx.spawn(async move |this, cx| {
            let id = mod_id.clone();
            let r = cx
                .background_spawn(async move { hopper::install::project_for_mod_id(&inst, &id) })
                .await;
            this.update(cx, |this, cx| match r {
                Ok(Some(m)) => this.open_peek(m.id, cx),
                _ => cx.emit(AddonsEvent::Find { root, text: mod_id }),
            })
            .ok();
        })
        .detach();
    }

    /// Turns off a jar in mods/, tracked or not.
    fn turn_off_jar(&mut self, file: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let file = file.to_string();
        let r = self.store.update(cx, |s, cx| {
            let tracked = s
                .instance(&root)
                .is_some_and(|i| i.addons.iter().any(|a| a.file_name.eq_ignore_ascii_case(&file)));
            let r = if tracked {
                s.set_enabled(&root, std::slice::from_ref(&file), false)
            } else {
                let dir = root.join("mods");
                std::fs::rename(dir.join(&file), dir.join(format!("{file}.disabled"))).map_err(|e| format!("{e:#}"))
            };
            cx.notify();
            r
        });
        self.resync(cx);
        if let Err(e) = r {
            window.push_notification(Notification::error(e), cx);
        }
    }

    /// Moves the instance to the newest `kind` loader (e.g. "neoforge") for
    /// its Minecraft that `range` accepts, and installs it, as saving a new
    /// loader in the instance's settings does.
    fn update_loader(&mut self, root: PathBuf, kind: String, range: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(inst) = self.store.read(cx).instance(&root).cloned() else { return };
        let gv = inst.game_version.clone();
        cx.spawn_in(window, async move |this, cx| {
            let r = cx.background_spawn(async move { cf::loader_versions(&gv) }).await;
            this.update_in(cx, |this, window, cx| {
                let prefix = format!("{kind}-");
                // Newest first: the newest build that fits.
                let pick = r.ok().and_then(|list| {
                    list.into_iter().find(|v| {
                        v.name
                            .strip_prefix(&prefix)
                            .is_some_and(|ver| hopper::deps::accepts(&range, ver) == Some(true))
                    })
                });
                let Some(v) = pick else {
                    window.push_notification(
                        Notification::error(format!(
                            "No {} version for Minecraft {} fits ({range}).",
                            crate::store::loader_label(&prefix).trim_end_matches(['-', ' ']),
                            inst.game_version
                        )),
                        cx,
                    );
                    return;
                };
                crate::inst_settings::switch_loader(&this.store, &inst.root, v.name.clone(), window, cx);
                this.resync(cx);
            })
            .ok();
        })
        .detach();
    }

    /// For each "only works with X a.b or newer" row: whether X ever
    /// published such a version. When it didn't, no update fixes it.
    fn want_ghosts(&mut self, blockers: &[hopper::deps::Blocker], cx: &mut Context<Self>) {
        use hopper::deps::BlockerKind;
        let Some(root) = self.missing.root.clone() else { return };
        let Some(inst) = self.store.read(cx).instance(&root).cloned() else { return };
        let asks: Vec<(String, String, u64)> = blockers
            .iter()
            .filter_map(|b| match &b.kind {
                BlockerKind::WrongVersion { needs, versions, jar, .. } => {
                    let key = (needs.clone(), versions.clone());
                    if self.missing.ghosts.contains_key(&key) {
                        return None;
                    }
                    let a = inst.addons.iter().find(|a| {
                        a.file_name.trim_end_matches(".disabled").eq_ignore_ascii_case(jar.trim_end_matches(".disabled"))
                    })?;
                    (a.project_id != 0).then(|| (needs.clone(), versions.clone(), a.project_id))
                }
                _ => None,
            })
            .collect();
        if asks.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_spawn(async move {
                    let mut out = Vec::new();
                    for (needs, versions, pid) in asks {
                        let Ok(files) = hopper::source::files(pid, "", hopper::cf::Loader::Any) else { continue };
                        let names: Vec<String> =
                            files.iter().flat_map(|f| [f.file_name.clone(), f.display_name.clone()]).collect();
                        if hopper::deps::range_published(&versions, &names) == Some(false) {
                            let example = names
                                .iter()
                                .flat_map(|n| hopper::deps::versions_in(n))
                                .max_by(|a, b| hopper::scan::compare_versions(a, b))
                                .unwrap_or_default();
                            out.push(((needs, versions), example));
                        }
                    }
                    out
                })
                .await;
            this.update(cx, |this, cx| {
                if this.missing.root.as_ref() == Some(&root) && !found.is_empty() {
                    this.missing.ghosts.extend(found);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// "N problems will stop the game at loading": what the loader refuses
    /// besides missing mods, with Turn off where that fixes it.
    pub(crate) fn blocker_banner(&self, inst: &Instance, cx: &mut Context<Self>) -> Option<AnyElement> {
        use hopper::deps::BlockerKind;
        let blockers: &[hopper::deps::Blocker] =
            if self.missing.root.as_ref() == Some(&inst.root) { &self.missing.blockers } else { &[] };
        // The last crash's "stopped loading" mod, unless a blocker row
        // names it already.
        let stop = self.crash_stop.as_ref().filter(|(st, _)| {
            !blockers.iter().any(|b| b.file_name.eq_ignore_ascii_case(st.file_name.trim_end_matches(".disabled")))
        });
        if self.class != cf::CLASS_MODS || (blockers.is_empty() && stop.is_none()) {
            return None;
        }
        let theme = cx.theme().clone();
        // A crash says loading broke, whatever the files look like.
        let loaded_fine = self.missing.loaded_fine.filter(|_| stop.is_none());
        let n = blockers.len() + usize::from(stop.is_some());
        let mut rows: Vec<AnyElement> = Vec::new();
        if let Some((st, changed)) = stop {
            rows.push(self.crash_stop_row(inst, st, changed.as_ref(), cx));
        }
        for (i, b) in blockers.iter().enumerate() {
            let addon = inst.addons.iter().find(|a| a.file_name.eq_ignore_ascii_case(&b.file_name));
            let (pid, icon) = (
                addon.map_or(0, |a| a.project_id),
                addon.and_then(|a| self.store.read(cx).image(&a.icon)).map(std::path::Path::to_path_buf),
            );
            let off = |id: String, file: String, label: String| {
                Button::new(SharedString::from(id))
                    .xsmall()
                    .outline()
                    .label(label)
                    .tooltip(file.clone())
                    .on_click(cx.listener(move |this, _, window, cx| this.turn_off_jar(&file, window, cx)))
                    .into_any_element()
            };
            // The other mod: the installed mod its jar belongs to (a jar can
            // hold several mods, so the id alone may not be a name you know).
            let parts = b.parts();
            let other = parts.other.as_ref().map(|(id, jar)| {
                let a = inst.addons.iter().find(|a| {
                    a.file_name.eq_ignore_ascii_case(jar)
                        || a.file_name.trim_end_matches(".disabled").eq_ignore_ascii_case(jar.trim_end_matches(".disabled"))
                });
                let icon = a.and_then(|a| self.store.read(cx).image(&a.icon)).map(std::path::Path::to_path_buf);
                (id.clone(), jar.clone(), a.map_or_else(|| id.clone(), |a| a.name.clone()), a.map_or(0, |a| a.project_id), icon)
            });
            let mut actions: Vec<AnyElement> = Vec::new();
            match &b.kind {
                BlockerKind::Loader { id, versions, .. } => {
                    // A newer loader fixes it, when it's this instance's
                    // loader (not Minecraft itself).
                    let kind = inst.loader.split('-').next().unwrap_or("").to_string();
                    let ours = !kind.is_empty() && (id == &kind || (id == "fabricloader" && kind == "fabric"));
                    if ours {
                        let (root, range) = (inst.root.clone(), versions.clone());
                        let min = range
                            .trim()
                            .strip_prefix('[')
                            .and_then(|r| r.split(',').next())
                            .map(|m| m.trim().to_string())
                            .filter(|m| !m.is_empty());
                        let name = crate::store::loader_label(&format!("{kind}-")).trim_end_matches(['-', ' ']).to_string();
                        let label = match &min {
                            Some(m) => format!("Update {name} to {m}+"),
                            None => format!("Update {name}"),
                        };
                        actions.push(
                            Button::new(SharedString::from(format!("blk-loader:{i}")))
                                .xsmall()
                                .outline()
                                .label(label)
                                .tooltip("Installs the newest version that fits, like picking it in the instance's settings")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.update_loader(root.clone(), kind.clone(), range.clone(), window, cx)
                                }))
                                .into_any_element(),
                        );
                    }
                    actions.push(off(format!("blk-off:{i}"), b.file_name.clone(), "Turn off".into()));
                    actions.extend(self.revert_action(i, inst, b, cx));
                }
                BlockerKind::Invalid(_) => {
                    actions.push(off(format!("blk-off:{i}"), b.file_name.clone(), "Turn off".into()));
                }
                BlockerKind::Incompatible { jar, .. } => {
                    actions.push(off(format!("blk-off:{i}"), b.file_name.clone(), format!("Turn off {}", b.mod_name)));
                    let name = other.as_ref().map_or_else(String::new, |o| o.2.clone());
                    actions.push(off(format!("blk-off2:{i}"), jar.clone(), format!("Turn off {name}")));
                }
                BlockerKind::WrongVersion { needs, versions, .. }
                    if self.missing.ghosts.contains_key(&(needs.clone(), versions.clone())) =>
                {
                    // No update fixes it: go back to a version of this mod
                    // that doesn't ask for it.
                    actions.push(off(format!("blk-off:{i}"), b.file_name.clone(), format!("Turn off {}", b.mod_name)));
                    actions.extend(self.revert_action(i, inst, b, cx));
                    let (store, root) = (self.store.clone(), inst.root.clone());
                    actions.push(
                        Button::new(SharedString::from(format!("blk-older:{i}")))
                            .xsmall()
                            .outline()
                            .label(format!("Older {}", b.mod_name))
                            .tooltip("Its versions, to go back to one before this mistake")
                            .disabled(pid == 0)
                            .on_click(move |_, window, cx| {
                                crate::versions::open(store.clone(), root.clone(), pid, false, None, window, cx)
                            })
                            .into_any_element(),
                    );
                }
                BlockerKind::WrongVersion { needs, .. } => {
                    actions.push(off(format!("blk-off:{i}"), b.file_name.clone(), format!("Turn off {}", b.mod_name)));
                    let id = needs.clone();
                    let name = other.as_ref().map_or_else(|| needs.clone(), |o| o.2.clone());
                    actions.extend(self.revert_action(i, inst, b, cx));
                    actions.push(
                        Button::new(SharedString::from(format!("blk-dep:{i}")))
                            .xsmall()
                            .outline()
                            .label(format!("Update {name}"))
                            .tooltip("Its page, to get a newer file")
                            .on_click(cx.listener(move |this, _, _, cx| this.peek_mod_id(id.clone(), cx)))
                            .into_any_element(),
                    );
                }
            }
            let view = cx.entity();
            let link = |key: String, name: String, icon: Option<std::path::PathBuf>, pid: u64, mod_id: Option<String>, cx: &Context<Self>| {
                let v = view.clone();
                let el = widgets::mod_link(
                    SharedString::from(key),
                    name,
                    icon.as_deref(),
                    pid,
                    move |p, _, cx| v.update(cx, |this, cx| this.open_peek(p, cx)),
                    cx,
                )
                .flex_none()
                .font_semibold();
                // Not a tracked project: its page is found by its mod id.
                match mod_id.filter(|_| pid == 0) {
                    Some(id) => el.clickable(cx).on_click(cx.listener(move |this, _, _, cx| this.peek_mod_id(id.clone(), cx))),
                    None => el,
                }
            };
            // A version nobody published: say so, and that the asking mod
            // is at fault.
            // Its versions, shown by the one installed.
            let ghost = match &b.kind {
                hopper::deps::BlockerKind::WrongVersion { needs, versions, have, .. }
                    if self.missing.ghosts.contains_key(&(needs.clone(), versions.clone())) =>
                {
                    Some(have.clone())
                }
                _ => None,
            };
            let mut parts = parts;
            if let (Some(example), Some(o)) = (&ghost, other.as_ref()) {
                parts.tail = format!("{}, a version that doesn't exist", parts.tail.split(';').next().unwrap_or("").trim());
                parts.why = format!(
                    "{}'s versions are numbered like {example} (yours): this is likely a mistake by {}'s author. Go back to an older {} until it's fixed.",
                    o.2, b.mod_name, b.mod_name
                );
            }
            // Under the sentence: why, and which file the other mod is in.
            let why: Vec<String> = [
                parts.why.clone(),
                other.as_ref().map_or_else(String::new, |(id, jar, name, ..)| {
                    if name.eq_ignore_ascii_case(id) { format!("in {jar}") } else { format!("in {jar} (its part \"{id}\")") }
                }),
            ]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();
            let sentence = h_flex()
                .flex_wrap()
                .gap_x_1p5()
                .gap_y_0p5()
                .items_center()
                .child(link(format!("blk:{i}"), b.mod_name.clone(), icon, pid, None, cx))
                .child(widgets::caption(parts.lead.clone(), cx))
                .when_some(other.clone(), |r, (id, _, name, opid, oicon)| {
                    r.child(link(format!("blk-other:{i}"), name, oicon, opid, Some(id), cx))
                })
                .when(!parts.tail.is_empty(), |r| r.child(widgets::caption(parts.tail.clone(), cx)));
            rows.push(
                h_flex()
                    .gap_4()
                    .items_start()
                    .text_sm()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(sentence)
                            .when(!why.is_empty(), |c| c.child(widgets::caption(why.join(" · "), cx).text_xs())),
                    )
                    .child(h_flex().flex_none().gap_2().children(actions))
                    .into_any_element(),
            );
        }
        let loader_note = blockers
            .iter()
            .any(|b| matches!(&b.kind, BlockerKind::Loader { id, .. } if id != "minecraft"));
        let n_rows = rows.len();
        let color = if loaded_fine.is_some() { theme.info } else { theme.danger };
        let title = match loaded_fine {
            Some(_) => format!("{n} possible problem{} in the mods' own files", if n == 1 { "" } else { "s" }),
            None => format!("{n} problem{} will stop the game at loading", if n == 1 { "" } else { "s" }),
        };
        let key = hopper::settings::notice_key(
            &inst.root.to_string_lossy(),
            "blockers",
            &format!("{:?}{:?}", stop.map(|(st, _)| &st.file_name), blockers),
        );
        let body = callout(color, cx)
                        .child(div().font_semibold().text_color(color).child(title.clone()))
                        .when_some(loaded_fine, |c, at| c.child(widgets::caption(loaded_fine_note(at), cx)))
                        .when(loader_note, |c| {
                            c.child(widgets::caption(
                                "For a mod that needs a newer loader, update the loader here or pick a version in this instance's settings.",
                                cx,
                            ))
                        })
                        .child(
                            div()
                                .id("blocker-rows")
                                .when(n_rows > 6, |d| d.h(rems(10.5)))
                                .overflow_y_scrollbar()
                                .child(v_flex().gap_2().children(rows)),
                        );
        let notice = widgets::closable_notice(body, key, widgets::Closed::Chip, title, color, &self.store, cx)?;
        Some(div().px_6().child(notice).into_any_element())
    }

    /// "Revert X to 1.6.3" for a problem an older version of the mod fixes
    /// (`revert_ui`); its updates are then marked with what they need.
    fn revert_action(&self, i: usize, inst: &Instance, b: &hopper::deps::Blocker, cx: &Context<Self>) -> Option<AnyElement> {
        let offer = self.store.read(cx).revert_offer(&inst.root, &b.file_name)?;
        let (label, tip) = offer.label();
        let root = inst.root.clone();
        Some(
            Button::new(SharedString::from(format!("blk-revert:{i}")))
                .xsmall()
                .outline()
                .icon(IconName::Undo2)
                .label(label)
                .tooltip(tip)
                .on_click(cx.listener(move |this, _, window, cx| {
                    let store = this.store.clone();
                    if crate::revert_ui::revert_instance(&store, &root, &offer, window, cx) {
                        this.resync(cx);
                    }
                    cx.notify();
                }))
                .into_any_element(),
        )
    }

    /// The banner's row for the last crash: "X stopped the game from
    /// loading after Y was updated (a → b)", with Put a back and Turn off X.
    fn crash_stop_row(
        &self,
        inst: &Instance,
        st: &hopper::crashdetect::Stopped,
        changed: Option<&hopper::crashdetect::Changed>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mods_dir = inst.root.join("mods");
        let find = |file: &str| {
            let bare = file.trim_end_matches(".disabled");
            inst.addons.iter().find(|a| a.file_name.eq_ignore_ascii_case(bare))
        };
        let view = cx.entity();
        let link = |key: &str, file: &str, fallback: &str, cx: &Context<Self>| {
            let a = find(file);
            let name = a
                .map(|a| a.name.clone())
                .filter(|n| !n.is_empty())
                .or_else(|| (!fallback.is_empty()).then(|| fallback.to_string()))
                .unwrap_or_else(|| hopper::crashdetect::jar_name(&mods_dir, file));
            let icon = a.and_then(|a| self.store.read(cx).image(&a.icon)).map(std::path::Path::to_path_buf);
            let v = view.clone();
            widgets::mod_link(
                SharedString::from(format!("crash-stop:{key}")),
                name,
                icon.as_deref(),
                a.map_or(0, |a| a.project_id),
                move |p, _, cx| v.update(cx, |this, cx| this.open_peek(p, cx)),
                cx,
            )
            .flex_none()
            .font_semibold()
        };
        let stopped_name = find(&st.file_name)
            .map(|a| a.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| hopper::crashdetect::jar_name(&mods_dir, &st.file_name));
        let sentence = h_flex()
            .flex_wrap()
            .gap_x_1p5()
            .gap_y_0p5()
            .items_center()
            .child(link("stopped", &st.file_name, "", cx))
            .child(widgets::caption(if changed.is_some() { "stopped the game from loading. Its error names" } else { "stopped the game from loading" }, cx))
            // Only what's known: the error names it and it changed lately,
            // not that the change caused it.
            .when_some(changed, |r, ch| {
                let versions =
                    if ch.from.is_empty() || ch.to.is_empty() { String::new() } else { format!(" ({} → {})", ch.from, ch.to) };
                r.child(link("changed", &ch.file_name, &ch.name, cx))
                    .child(widgets::caption(format!("updated {}{versions}", hopper::time::relative_secs(ch.when)), cx))
            });
        let why = if st.error.is_empty() {
            "In the last crash. Logs & crashes has more.".to_string()
        } else {
            format!("In the last crash: \"{}\"", st.error)
        };
        let mut actions: Vec<AnyElement> = Vec::new();
        if let Some(ch) = changed {
            let (root, ch) = (inst.root.clone(), ch.clone());
            actions.push(
                Button::new("crash-stop-back")
                    .xsmall()
                    .outline()
                    .icon(IconName::Undo2)
                    .label(if ch.from.is_empty() { "Put the older version back".to_string() } else { format!("Put {} back", ch.from) })
                    .tooltip("Swaps in the copy Hopper kept. The newer one goes to Backups, so you can undo this there.")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let store = this.store.clone();
                        if crate::crash_help::put_back_changed(&store, &root, &ch, window, cx) {
                            this.crash_stop = None;
                            // Untracked jars changed too: re-read the folder.
                            this.resync(cx);
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        let file = st.file_name.clone();
        actions.push(
            Button::new("crash-stop-off")
                .xsmall()
                .outline()
                .label(format!("Turn off {stopped_name}"))
                .tooltip(file.clone())
                .on_click(cx.listener(move |this, _, window, cx| this.turn_off_jar(&file, window, cx)))
                .into_any_element(),
        );
        h_flex()
            .gap_4()
            .items_start()
            .text_sm()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(sentence)
                    .child(widgets::caption(why, cx).text_xs().whitespace_normal()),
            )
            .child(h_flex().flex_none().gap_2().children(actions))
            .into_any_element()
    }

    fn enable_provider(&mut self, file: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let base = file.trim_end_matches(".disabled").to_string();
        let r = self.store.update(cx, |s, cx| {
            let tracked = s
                .instance(&root)
                .is_some_and(|i| i.addons.iter().any(|a| a.file_name.eq_ignore_ascii_case(&base)));
            let r = if tracked {
                s.set_enabled(&root, std::slice::from_ref(&base), true)
            } else {
                let dir = root.join("mods");
                std::fs::rename(dir.join(file), dir.join(&base)).map_err(|e| format!("{e:#}"))
            };
            cx.notify();
            r
        });
        // An untracked jar leaves the instance as it was: re-read the folder.
        self.resync(cx);
        if let Err(e) = r {
            window.push_notification(Notification::error(e), cx);
        }
    }

    /// Shader packs need Iris or Oculus: a callout with Find when it's missing.
    pub(crate) fn shader_notes(&self, inst: &Instance, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.class != cf::CLASS_SHADERS {
            return None;
        }
        // Read from disk when the table was built, not per frame.
        let (_, has_loader) = self.shader_state();
        if has_loader {
            return None;
        }
        let theme = cx.theme();
        let want = if inst.loader.starts_with("forge") { "Oculus" } else { "Iris" };
        let root = inst.root.clone();
        let title = "Install Iris or Oculus to use shaders";
        let key = hopper::settings::notice_key(&inst.root.to_string_lossy(), "shader-loader", title);
        let warning = theme.warning;
        let body = callout(theme.warning, cx).child(
                        h_flex()
                            .gap_3()
                            .child(Icon::new(IconName::TriangleAlert).small().text_color(theme.warning))
                            .child(div().flex_1().text_sm().child(title))
                            .child(
                                Button::new("find-shader-loader")
                                    .small()
                                    .outline()
                                    .icon(IconName::Search)
                                    .label(format!("Find {want}"))
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.emit(AddonsEvent::Find { root: root.clone(), text: want.to_lowercase() })
                                    })),
                            ),
                    );
        let notice = widgets::closable_notice(body, key, widgets::Closed::Chip, title, warning, &self.store, cx)?;
        Some(div().px_6().child(notice).into_any_element())
    }

    /// Named sets of disabled mods to switch between.
    pub(crate) fn profiles_button(&self, inst: &Instance, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let names: Vec<String> = inst.mod_profiles.iter().map(|p| p.name.clone()).collect();
        let view = cx.entity();
        Button::new("profiles")
            .small()
            .outline()
            .icon(IconName::Layers)
            .label("Profiles")
            .dropdown_caret(true)
            .dropdown_menu(move |menu, window, cx| {
                let mut menu = menu.label("Switch which mods are enabled");
                for n in &names {
                    let (v, n) = (view.clone(), n.clone());
                    menu = menu.item(PopupMenuItem::new(format!("Use “{n}”")).on_click(
                        move |_, _, cx| v.update(cx, |this, cx| this.use_profile(n.clone(), cx)),
                    ));
                }
                let v = view.clone();
                let mut menu = menu.separator().item(
                    PopupMenuItem::new("Save current as…")
                        .icon(IconName::Plus)
                        .on_click(move |_, window, cx| save_profile_dialog(v.clone(), window, cx)),
                );
                if !names.is_empty() {
                    let (names, v3) = (names.clone(), view.clone());
                    menu = menu.submenu("Delete a profile", window, cx, move |mut sub, _, _| {
                        for n in &names {
                            let (v, n) = (v3.clone(), n.clone());
                            sub = sub.item(PopupMenuItem::new(format!("Delete “{n}”")).icon(IconName::Trash).on_click(
                                move |_, window, cx| delete_profile(v.clone(), n.clone(), window, cx),
                            ));
                        }
                        sub
                    });
                }
                menu
            })
    }

    fn use_profile(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let store = self.store.read(cx);
        if let Some(mut inst) = store.instance(&root).cloned() {
            store.spawn_job(format!("Switching to profile {name}"), move |job| {
                let n = hopper::install::apply_profile(&mut inst, &name)?;
                job.detail(format!("{n} mods changed"));
                Ok(crate::store::Msg::InstanceChanged(inst.root.clone()))
            });
        }
    }
}

/// Forgets a saved profile; which mods are on stays as it is.
fn delete_profile(view: Entity<AddonsView>, name: String, window: &mut Window, cx: &mut App) {
    let (store, root) = {
        let v = view.read(cx);
        (v.store.clone(), v.root.clone())
    };
    let Some(root) = root else { return };
    let r = store.update(cx, |s, cx| {
        let r = s.delete_profile(&root, &name);
        cx.notify();
        r
    });
    match r {
        Ok(()) => window.push_notification(Notification::success(format!("Deleted profile “{name}”")), cx),
        Err(e) => window.push_notification(Notification::error(e), cx),
    }
}

fn save_profile_dialog(view: Entity<AddonsView>, window: &mut Window, cx: &mut App) {
    let name = cx.new(|cx| InputState::new(window, cx).placeholder("Profile name"));
    window.open_dialog(cx, move |dialog, window, _| {
        let (view, n2) = (view.clone(), name.clone());
        dialog
            .title("Save mod profile")
            .w(rems(26.).to_pixels(window.rem_size()))
            .margin_top(rems(crate::widgets::DIALOG_TOP).to_pixels(window.rem_size()))
            .child(
                v_flex()
                    .gap_2()
                    .child(Input::new(&name))
                    .child(div().text_sm().child(
                        "Remembers which mods are disabled now, to switch back to later.",
                    )),
            )
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("p-cancel").outline().label("Cancel")))
                    .child(DialogAction::new().child(Button::new("p-save").primary().label("Save"))),
            )
            .on_ok(move |_, window, cx| {
                let n = n2.read(cx).value().trim().to_string();
                if n.is_empty() {
                    return false;
                }
                let (store, root) = {
                    let v = view.read(cx);
                    (v.store.clone(), v.root.clone())
                };
                let Some(root) = root else { return true };
                let r = store.update(cx, |s, cx| {
                    let r = s.save_profile(&root, &n);
                    cx.notify();
                    r
                });
                if let Err(e) = r {
                    window.push_notification(Notification::error(e), cx);
                }
                true
            })
    });
}


/// The line under a softened banner: when the game last loaded with these
/// mods, so the user can tell the check is likely wrong.
fn loaded_fine_note(at: i64) -> String {
    format!(
        "The game loaded fine with these exact mods {}, so this is probably nothing: some mods stand in for others while the game loads, which their files don't show.",
        hopper::time::relative_secs(at)
    )
}
