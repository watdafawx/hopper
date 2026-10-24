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
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
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
        let dir = inst.mods_dir();
        let mtime = std::fs::metadata(&dir).and_then(|m| m.modified()).ok();
        if self.missing.root.as_ref() == Some(&inst.root) && self.missing.mtime == mtime {
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
                if this.missing.root.as_ref() == Some(&root) && this.missing.mtime == mtime {
                    this.missing.dups = dups;
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
        if dups.is_empty() {
            return Some(
                div()
                    .px_6()
                    .child(
                        callout(theme.info, cx).child(
                            div().text_sm().child(format!(
                                "{queued} old file{} the game still has open will be removed when it closes.",
                                if queued == 1 { "" } else { "s" }
                            )),
                        ),
                    )
                    .into_any_element(),
            );
        }
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
        Some(
            div()
                .px_6()
                .child(
                    callout(theme.warning, cx)
                        .child(
                            h_flex()
                                .gap_3()
                                .child(
                                    div()
                                        .flex_1()
                                        .font_semibold()
                                        .text_color(theme.warning)
                                        .child(format!(
                                            "{n} mod{} in twice: the game won't start with both",
                                            if n == 1 { " is" } else { "s are" }
                                        )),
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
                        ),
                )
                .into_any_element(),
        )
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
                    h_flex()
                        .gap_1p5()
                        .child(widgets::thumb(None, rems(1.25), cx))
                        .child(
                            div()
                                .id(SharedString::from(format!("dep:{}:{}", name, m.needs)))
                                .font_family(theme.mono_font_family.clone())
                                .clickable(cx)
                                .hover(|s| s.text_color(theme.primary))
                                .tooltip(|w, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new("Details").build(w, cx)
                                })
                                .on_click(cx.listener(move |this, _, _, cx| this.peek_mod_id(dep.clone(), cx)))
                                .child(m.needs.clone()),
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
        Some(
            div()
                .id("missing-banner")
                .on_hover(cx.listener(|this, h: &bool, _, _| this.settle.pointer_in = *h))
                .px_6()
                .child(
                    callout(if loaded_fine.is_some() { theme.info } else { theme.warning }, cx)
                        .child(
                            h_flex()
                                .gap_3()
                                .child(
                                    div()
                                        .flex_1()
                                        .font_semibold()
                                        .text_color(if loaded_fine.is_some() { theme.info } else { theme.warning })
                                        .child(match loaded_fine {
                                            Some(_) => format!(
                                                "{n} mod{} list{} a requirement that isn't installed",
                                                if n == 1 { "" } else { "s" },
                                                if n == 1 { "s" } else { "" }
                                            ),
                                            None => format!(
                                                "{n} required mod{} missing: the game will stop at loading",
                                                if n == 1 { " is" } else { "s are" }
                                            ),
                                        }),
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
                        ),
                )
                .into_any_element(),
        )
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

    /// "N problems will stop the game at loading": what the loader refuses
    /// besides missing mods, with Turn off where that fixes it.
    pub(crate) fn blocker_banner(&self, inst: &Instance, cx: &mut Context<Self>) -> Option<AnyElement> {
        use hopper::deps::BlockerKind;
        if self.class != cf::CLASS_MODS
            || self.missing.root.as_ref() != Some(&inst.root)
            || self.missing.blockers.is_empty()
        {
            return None;
        }
        let theme = cx.theme();
        let loaded_fine = self.missing.loaded_fine;
        let n = self.missing.blockers.len();
        let mut rows: Vec<AnyElement> = Vec::new();
        for (i, b) in self.missing.blockers.iter().enumerate() {
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
            let mut actions: Vec<AnyElement> = Vec::new();
            match &b.kind {
                BlockerKind::Invalid(_) | BlockerKind::Loader { .. } => {
                    actions.push(off(format!("blk-off:{i}"), b.file_name.clone(), "Turn off".into()));
                }
                BlockerKind::Incompatible { with, jar, .. } => {
                    actions.push(off(format!("blk-off:{i}"), b.file_name.clone(), "Turn off this".into()));
                    actions.push(off(format!("blk-off2:{i}"), jar.clone(), format!("Turn off {with}")));
                }
                BlockerKind::WrongVersion { needs, .. } => {
                    actions.push(off(format!("blk-off:{i}"), b.file_name.clone(), "Turn off this".into()));
                    let id = needs.clone();
                    actions.push(
                        Button::new(SharedString::from(format!("blk-dep:{i}")))
                            .xsmall()
                            .outline()
                            .label(format!("Update {needs}"))
                            .tooltip("Its page, to get a newer file")
                            .on_click(cx.listener(move |this, _, _, cx| this.peek_mod_id(id.clone(), cx)))
                            .into_any_element(),
                    );
                }
            }
            let view = cx.entity();
            rows.push(
                h_flex()
                    .flex_wrap()
                    .gap_x_3()
                    .gap_y_1()
                    .text_sm()
                    .child(
                        widgets::mod_link(
                            SharedString::from(format!("blk:{i}")),
                            b.mod_name.clone(),
                            icon.as_deref(),
                            pid,
                            move |p, _, cx| view.update(cx, |this, cx| this.open_peek(p, cx)),
                            cx,
                        )
                        .font_semibold(),
                    )
                    .child(widgets::caption(b.text(), cx).text_xs())
                    .children(actions)
                    .into_any_element(),
            );
        }
        let loader_note = self
            .missing
            .blockers
            .iter()
            .any(|b| matches!(&b.kind, BlockerKind::Loader { id, .. } if id != "minecraft"));
        let n_rows = rows.len();
        Some(
            div()
                .px_6()
                .child(
                    callout(if loaded_fine.is_some() { theme.info } else { theme.danger }, cx)
                        .child(div().font_semibold().text_color(if loaded_fine.is_some() { theme.info } else { theme.danger }).child(
                            match loaded_fine {
                                Some(_) => format!("{n} possible problem{} in the mods' own files", if n == 1 { "" } else { "s" }),
                                None => format!("{n} problem{} will stop the game at loading", if n == 1 { "" } else { "s" }),
                            },
                        ))
                        .when_some(loaded_fine, |c, at| c.child(widgets::caption(loaded_fine_note(at), cx)))
                        .when(loader_note, |c| {
                            c.child(widgets::caption(
                                "For a mod that needs a newer loader, pick a newer loader version in this instance's settings.",
                                cx,
                            ))
                        })
                        .child(
                            div()
                                .id("blocker-rows")
                                .when(n_rows > 6, |d| d.h(rems(10.5)))
                                .overflow_y_scrollbar()
                                .child(v_flex().gap_2().children(rows)),
                        ),
                )
                .into_any_element(),
        )
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
        Some(
            div()
                .px_6()
                .child(
                    callout(theme.warning, cx).child(
                        h_flex()
                            .gap_3()
                            .child(Icon::new(IconName::TriangleAlert).small().text_color(theme.warning))
                            .child(div().flex_1().text_sm().child("Install Iris or Oculus to use shaders"))
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
                    ),
                )
                .into_any_element(),
        )
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
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.label("Switch which mods are enabled");
                for n in &names {
                    let (v, n) = (view.clone(), n.clone());
                    menu = menu.item(PopupMenuItem::new(format!("Use “{n}”")).on_click(
                        move |_, _, cx| v.update(cx, |this, cx| this.use_profile(n.clone(), cx)),
                    ));
                }
                let (v, v2) = (view.clone(), view.clone());
                menu.separator()
                    .item(
                        PopupMenuItem::new("Save current as…")
                            .icon(IconName::Plus)
                            .on_click(move |_, window, cx| save_profile_dialog(v.clone(), window, cx)),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new("Find the mod that breaks it…")
                            .icon(IconName::ScanSearch)
                            .on_click(move |_, _, cx| v2.update(cx, |this, cx| this.hunt_setup(cx))),
                    )
            })
    }

    fn use_profile(&mut self, name: String, cx: &mut Context<Self>) {
        // A hunt owns the on/off state until it ends.
        if self.hunt_running() {
            return;
        }
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

/// Finding the mod that breaks the pack: the hunt's strip above the Mods
/// list and its row chips (the rounds are in `hopper::bisect`).
#[derive(Default)]
pub struct HuntUi {
    /// The instance `hunt` was read for, and the journal's modified time.
    pub root: Option<PathBuf>,
    pub stamp: Option<std::time::SystemTime>,
    pub hunt: Option<hopper::bisect::Hunt>,
    /// The start card is open, with what goes wrong.
    pub setup: Option<hopper::bisect::Kind>,
    /// Round 1 is being set up.
    pub starting: bool,
}

/// An instance whose Logs asked to start a hunt, picked up by its Mods tab.
static ASKED: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Opens the hunt's start card in `root`'s Mods tab, and goes there.
pub fn ask_hunt(root: PathBuf, store: &Entity<crate::store::Store>, cx: &mut App) {
    *ASKED.lock().unwrap_or_else(|e| e.into_inner()) = Some(root.clone());
    store.update(cx, |_, cx| cx.emit(crate::store::StoreEvent::ShowMods(root)));
}

/// The entry point's button, for Logs & crashes.
pub fn hunt_button(id: &'static str, root: PathBuf, store: Entity<crate::store::Store>) -> Button {
    Button::new(id)
        .small()
        .outline()
        .icon(IconName::ScanSearch)
        .label("Find the mod that breaks it")
        .tooltip("Turns mods off in rounds while you play, to find the one at fault")
        .on_click(move |_, _, cx| ask_hunt(root.clone(), &store, cx))
}

impl AddonsView {
    /// Reads the instance's hunt journal when it changed (from `sync`).
    pub(crate) fn read_hunt(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone().filter(|_| self.class == cf::CLASS_MODS && self.global_dir().is_none());
        let Some(root) = root else { return };
        let stamp = std::fs::metadata(hopper::bisect::path(&root)).and_then(|m| m.modified()).ok();
        let same = self.hunt.root.as_ref() == Some(&root);
        if !same {
            self.hunt = HuntUi { root: Some(root.clone()), ..Default::default() };
        }
        if !same || stamp != self.hunt.stamp {
            self.hunt.stamp = stamp;
            self.hunt.hunt = stamp.and_then(|_| hopper::bisect::load(&root));
            if self.hunt.hunt.is_some() {
                self.hunt.setup = None;
            }
            cx.notify();
        }
        // The start job ended (or failed).
        if self.hunt.starting && (self.hunt.hunt.is_some() || self.store.read(cx).edit_hold_reason(&root).is_none()) {
            self.hunt.starting = false;
            cx.notify();
        }
    }

    /// A hunt is running here: the On switches wait.
    pub(crate) fn hunt_running(&self) -> bool {
        self.class == cf::CLASS_MODS && self.global_dir().is_none() && self.hunt.hunt.is_some()
    }

    /// Opens the start card.
    pub(crate) fn hunt_setup(&mut self, cx: &mut Context<Self>) {
        if self.hunt.hunt.is_none() {
            self.hunt.setup = Some(hopper::bisect::Kind::Crash);
            cx.notify();
        }
    }

    fn hunt_start(&mut self, kind: hopper::bisect::Kind, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        // The report's blamed mod, as a hint only.
        let hint = self.store.update(cx, |s, _| {
            let file = s.crash(&root)?.suspects.first()?.file_name.trim_end_matches(".disabled").to_string();
            let name = s.instance(&root)?.addon_by_file(&file).map_or(file, |a| a.name.clone());
            Some(name)
        });
        let store = self.store.read(cx);
        let Some(mut inst) = store.instance(&root).cloned() else { return };
        store.spawn_instance_job(&root, format!("Finding the mod that breaks {}", inst.name), move |job| {
            job.detail("Reading what each mod needs");
            let h = hopper::bisect::begin(&mut inst, kind, hint)?;
            job.detail(format!("Round 1: {} mods parked", h.parked.len()));
            Ok(crate::store::Msg::InstanceChanged(inst.root.clone()))
        });
        self.hunt.setup = None;
        self.hunt.starting = true;
        cx.notify();
    }

    /// Changes the hunt, saves it, then renames the jars to match.
    fn hunt_change(&mut self, title: &str, edit: impl FnOnce(&mut hopper::bisect::Hunt) -> bool, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let Some(mut h) = self.hunt.hunt.clone() else { return };
        if !edit(&mut h) {
            return;
        }
        self.hunt.hunt = Some(h.clone());
        cx.notify();
        let store = self.store.read(cx);
        let Some(mut inst) = store.instance(&root).cloned() else { return };
        store.spawn_instance_job(&root, title.to_string(), move |job| {
            hopper::bisect::save(&inst.root, &h)?;
            let n = hopper::bisect::apply(&mut inst, &h)?;
            job.detail(format!("{n} mods changed"));
            Ok(crate::store::Msg::InstanceChanged(inst.root.clone()))
        });
    }

    /// Ends the hunt: every mod as before round 1 (`keep_off`: but the
    /// culprit and what needs it).
    fn hunt_finish(&mut self, keep_off: bool, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let store = self.store.read(cx);
        let Some(mut inst) = store.instance(&root).cloned() else { return };
        let title = if keep_off { "Restoring every mod but the culprit" } else { "Restoring every mod" };
        store.spawn_instance_job(&root, title, move |job| {
            let n = hopper::bisect::finish(&mut inst, keep_off)?;
            job.detail(format!("{n} mods changed"));
            Ok(crate::store::Msg::InstanceChanged(inst.root.clone()))
        });
    }

    fn hunt_name(inst: &Instance, file: &str) -> String {
        inst.addon_by_file(file).map_or_else(|| file.trim_end_matches(".jar").to_string(), |a| a.name.clone())
    }

    fn hunt_names(inst: &Instance, files: &[String], cap: usize) -> String {
        let mut v: Vec<String> = files.iter().take(cap).map(|f| Self::hunt_name(inst, f)).collect();
        if files.len() > cap {
            v.push(format!("{} more", files.len() - cap));
        }
        v.join(", ")
    }

    /// The strip above the Mods list: the start card, or the hunt's round.
    pub(crate) fn hunt_strip(&mut self, inst: &Instance, cx: &mut Context<Self>) -> Option<AnyElement> {
        use hopper::bisect::{Answer, Kind, Phase};
        if self.class != cf::CLASS_MODS || self.global_dir().is_some() {
            return None;
        }
        let asked = {
            let mut a = ASKED.lock().unwrap_or_else(|e| e.into_inner());
            if a.as_ref() == Some(&inst.root) { a.take() } else { None }
        };
        if asked.is_some() && self.hunt.hunt.is_none() && !self.hunt.starting {
            self.hunt.setup = Some(Kind::Crash);
        }
        let theme = cx.theme().clone();
        let store = self.store.read(cx);
        // A running game holds the jars; a task may be changing them.
        let hold = store.hold_reason(&inst.root);
        let strip = |color: Hsla| {
            v_flex()
                .gap_2()
                .px_3()
                .py_2p5()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(color.opacity(widgets::alpha::LINE))
                .bg(color.opacity(0.06))
        };
        let title = |icon: IconName, color: Hsla, text: String| {
            h_flex()
                .gap_2()
                .min_w_0()
                .child(Icon::new(icon).small().text_color(color))
                .child(div().font_semibold().truncate().child(text))
        };
        let note = |text: String| div().text_sm().text_color(theme.muted_foreground).child(text);

        if self.hunt.starting {
            return Some(
                strip(theme.primary)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(gpui_kit::component::spinner::Spinner::new().small())
                            .child(div().font_semibold().child("Getting round 1 ready")),
                    )
                    .child(note("Hopper saves which mods are on, reads what each one needs, then parks the first half.".into()))
                    .into_any_element(),
            );
        }

        let Some(h) = self.hunt.hunt.clone() else {
            let kind = self.hunt.setup?;
            let on = inst.addons.iter().filter(|a| a.enabled && a.folder() == "mods").count();
            let pick = |id: &'static str, label: &'static str, k: Kind| {
                Button::new(id)
                    .small()
                    .ghost()
                    .label(label)
                    .selected(kind == k)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.hunt.setup = Some(k);
                        cx.notify();
                    }))
            };
            return Some(
                strip(theme.primary)
                    .child(
                        h_flex()
                            .gap_3()
                            .child(title(IconName::ScanSearch, theme.primary, "Find the mod that breaks it".into()))
                            .child(div().flex_1())
                            .child(Button::new("hunt-cancel").small().ghost().label("Cancel").on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.hunt.setup = None;
                                    cx.notify();
                                },
                            )))
                            .child(
                                Button::new("hunt-start")
                                    .small()
                                    .primary()
                                    .icon(IconName::Play)
                                    .label("Start")
                                    .disabled(hold.is_some())
                                    .when_some(hold, |b, r| b.tooltip(r))
                                    .on_click(cx.listener(move |this, _, _, cx| this.hunt_start(kind, cx))),
                            ),
                    )
                    .child(note(format!(
                        "{on} mods are on. Each round Hopper turns about half of them off (always with the mods that need them), you play as usual and say whether it's still broken. About {} rounds. Stop & restore puts every mod back exactly as it is now.",
                        (usize::BITS - on.saturating_sub(1).leading_zeros()).max(1) + 1
                    )))
                    .child(
                        h_flex()
                            .gap_1()
                            .child(div().text_sm().mr_1().child("What goes wrong?"))
                            .child(pick("hunt-k-crash", "It crashes", Kind::Crash))
                            .child(pick("hunt-k-bad", "It misbehaves (no crash)", Kind::Misbehaves)),
                    )
                    .into_any_element(),
            );
        };

        let names = |files: &[String], cap: usize| Self::hunt_names(inst, files, cap);
        let undo = (!h.rounds.is_empty()).then(|| {
            Button::new("hunt-undo")
                .small()
                .ghost()
                .icon(IconName::Undo)
                .label("Undo answer")
                .disabled(hold.is_some())
                .on_click(cx.listener(|this, _, _, cx| this.hunt_change("Taking back the last answer", |h| h.undo(), cx)))
        });
        let restore_all = |primary: bool| {
            Button::new("hunt-restore")
                .small()
                .map(|b| if primary { b.primary() } else { b.outline() })
                .icon(IconName::RotateCcw)
                .label("Restore everything")
                .tooltip("Every mod back on or off as before the hunt")
                .disabled(hold.is_some())
                .on_click(cx.listener(|this, _, _, cx| this.hunt_finish(false, cx)))
        };
        let el = match h.phase {
            Phase::Found => {
                let along: Vec<String> = h.parked.iter().filter(|p| !h.culprits.contains(p)).cloned().collect();
                let what = names(&h.culprits, 3);
                strip(theme.success)
                    .child(
                        h_flex()
                            .gap_3()
                            .child(title(IconName::Target, theme.success, format!("Found it: {what}")))
                            .child(div().flex_1())
                            .children(undo)
                            .child(restore_all(false))
                            .child(
                                Button::new("hunt-keep")
                                    .small()
                                    .primary()
                                    .icon(IconName::Check)
                                    .label("Keep it off, restore the rest")
                                    .disabled(hold.is_some())
                                    .when_some(hold, |b, r| b.tooltip(r))
                                    .on_click(cx.listener(|this, _, _, cx| this.hunt_finish(true, cx))),
                            ),
                    )
                    .child(note(format!(
                        "After {} rounds. {} off now and everything else is back on, so the game should work.{}",
                        h.rounds.len(),
                        if h.culprits.len() == 1 { "It's" } else { "They're" },
                        if along.is_empty() {
                            String::new()
                        } else {
                            format!(" Off with it, because they need it: {}.", names(&along, 4))
                        }
                    )))
                    .into_any_element()
            }
            Phase::NotFound => strip(theme.warning)
                .child(
                    h_flex()
                        .gap_3()
                        .child(title(IconName::TriangleAlert, theme.warning, "No single mod breaks it".into()))
                        .child(div().flex_1())
                        .children(undo)
                        .child(restore_all(true)),
                )
                .child(note(
                    "It stayed broken with every suspect off. The cause is likely a config, the world, Java or the loader, or mods that only break together."
                        .into(),
                ))
                .into_any_element(),
            _ => {
                let crash = h.kind == Kind::Crash;
                let (_, parked, suspects) = h.counts();
                // On, and not a suspect, parked, marked fine or a kept library.
                let out: std::collections::HashSet<&String> = h
                    .suspects
                    .iter()
                    .chain(&h.parked)
                    .chain(&h.fine)
                    .chain(h.libraries.iter().filter(|_| h.phase == Phase::Mods))
                    .collect();
                let cleared = h.original.iter().filter(|(f, on)| **on && !out.contains(f)).count();
                let total = h.rounds_estimate().max(h.round());
                let done = h.rounds.len() as f32 / total as f32;
                let answer_tip = hold.unwrap_or("Launch and play as usual, then answer");
                let answer = |id: &'static str, label: &'static str, a: Answer, primary: bool| {
                    Button::new(id)
                        .small()
                        .map(|b| if primary { b.primary() } else { b.outline() })
                        .label(label)
                        .tooltip(answer_tip)
                        .disabled(hold.is_some())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.hunt_change("Next round of the hunt", |h| {
                                h.answer(a);
                                true
                            }, cx)
                        }))
                };
                let s_off = h.suspects.iter().all(|s| h.parked.contains(s));
                let line = if s_off {
                    format!("Checking: all {suspects} suspects are off this round. If it works, they hold the cause.")
                } else if h.phase == Phase::Libraries {
                    "The other mods are cleared: now testing libraries, the mods others need (with them off, what needs them is off too).".to_string()
                } else {
                    format!(
                        "{suspects} suspects left. Play as usual, then say whether it still {}.",
                        if crash { "crashes" } else { "misbehaves" }
                    )
                };
                strip(theme.primary)
                    .child(
                        h_flex()
                            .gap_3()
                            .child(title(
                                IconName::ScanSearch,
                                theme.primary,
                                format!(
                                    "Finding the {} · round {} of ~{total}",
                                    if crash { "crash" } else { "problem" },
                                    h.round()
                                ),
                            ))
                            .child(widgets::chip(theme.warning).child(format!("{parked} parked")))
                            .child(widgets::chip(theme.success).child(format!("{cleared} cleared")))
                            .child(div().flex_1())
                            .children(undo)
                            .child(
                                Button::new("hunt-stop")
                                    .small()
                                    .outline()
                                    .danger()
                                    .label("Stop & restore")
                                    .tooltip("Ends the hunt: every mod back on or off as before it")
                                    .disabled(hold.is_some())
                                    .on_click(cx.listener(|this, _, _, cx| this.hunt_finish(false, cx))),
                            )
                            .child(answer("hunt-broken", if crash { "Still crashes" } else { "Still broken" }, Answer::Broken, false))
                            .child(answer("hunt-works", "Works now", Answer::Works, true)),
                    )
                    .child(
                        div()
                            .h_1()
                            .w_full()
                            .rounded_full()
                            .bg(theme.muted)
                            .child(div().h_full().rounded_full().bg(theme.primary).w(relative(done.clamp(0.02, 1.0)))),
                    )
                    .child(note(line))
                    .when_some(h.hint.clone().filter(|_| crash), |s, hint| {
                        s.child(note(format!(
                            "The crash report mentions {hint}. That's often not the real cause (reports blame the last mod in the chain), so every mod is tested."
                        )))
                    })
                    .into_any_element()
            }
        };
        Some(el)
    }

    /// A row's hunt chip: its state, what needs it, and a click to mark it
    /// known good (or a suspect again).
    pub(crate) fn hunt_chip(&self, file: &str, cx: &mut Context<Self>) -> Option<AnyElement> {
        use hopper::bisect::RowState::*;
        let h = self.hunt.hunt.as_ref().filter(|_| self.hunt_running())?;
        let inst = self.table.inst.clone()?;
        let state = h.state(file)?;
        let theme = cx.theme();
        let (color, label) = match state {
            Testing => (theme.primary, "testing"),
            Parked => (theme.warning, "parked"),
            Cleared => (theme.success, "cleared"),
            Library => (theme.info, "library, kept on"),
            Fine => (theme.success, "known good"),
            Culprit => (theme.danger, "culprit"),
        };
        let by = h.needed_by(file);
        let needs: Vec<String> = h.requires.get(file).cloned().unwrap_or_default();
        let mut tip = match state {
            Testing => "On this round, still a suspect. Click if you know it's fine.".to_string(),
            Parked if h.suspects.iter().any(|s| s == file) => "Off this round, still a suspect. Click if you know it's fine.".to_string(),
            Parked if h.culprits.is_empty() => {
                let why: Vec<String> = needs.iter().filter(|n| h.parked.contains(n)).cloned().collect();
                format!("Not a suspect: off this round because it needs {}.", Self::hunt_names(&inst, &why, 3))
            }
            Parked => "Off with the culprit, because it needs it.".to_string(),
            Cleared => "Not the cause: ruled out by an earlier round.".to_string(),
            Library => "Many mods need it, so it stays on. Tested last, only if no other mod is the cause.".to_string(),
            Fine => "You marked it fine. Click to make it a suspect again.".to_string(),
            Culprit => "Breaks the game when on.".to_string(),
        };
        if !by.is_empty() {
            tip += &format!("\nNeeded by {}", Self::hunt_names(&inst, &by, 5));
        }
        let suspect = h.suspects.iter().any(|s| s == file) && h.suspects.len() > 1;
        let click = match state {
            Fine => Some(false),
            Testing | Parked if suspect => Some(true),
            _ => None,
        };
        let f = file.to_string();
        let tip: SharedString = tip.into();
        Some(
            div()
                .id(SharedString::from(format!("hunt:{file}")))
                .flex_none()
                .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
                .when_some(click, |d, fine| {
                    d.clickable(cx).on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        let f = f.clone();
                        if fine {
                            this.hunt_change("Marking a mod fine", move |h| h.mark_fine(&f), cx)
                        } else {
                            this.hunt_change("Making a mod a suspect again", move |h| h.unmark_fine(&f), cx)
                        }
                    }))
                })
                .child(widgets::chip(color).child(label))
                .into_any_element(),
        )
    }
}

/// The line under a softened banner: when the game last loaded with these
/// mods, so the user can tell the check is likely wrong.
fn loaded_fine_note(at: i64) -> String {
    format!(
        "The game loaded fine with these exact mods {}, so this is probably nothing: some mods stand in for others while the game loads, which their files don't show.",
        hopper::time::relative_secs(at)
    )
}
