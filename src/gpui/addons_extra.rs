//! Extras around the addon table: global resource pack folders (Paxi and
//! friends), required mods that are missing, shader notes and mod profiles.

use crate::addons::{AddonsEvent, AddonsView};
use crate::crash_help::callout;
use crate::store::open_path;
use crate::widgets::{self, Clickable as _};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex,
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

    /// The packs of the global folder shown.
    fn picked_packs(&self) -> &[PackFile] {
        self.globals
            .picked
            .and_then(|p| self.globals.packs.get(p))
            .map_or(&[], Vec::as_slice)
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
        };
        let root = inst.root.clone();
        let snapshot = inst.clone();
        cx.spawn(async move |this, cx| {
            let (list, dups, blockers) = cx
                .background_spawn(async move {
                    (
                        hopper::deps::missing(&dir),
                        hopper::install::duplicates(&snapshot),
                        hopper::deps::blockers(&dir, &snapshot.loader, &snapshot.game_version),
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                if this.missing.root.as_ref() == Some(&root) && this.missing.mtime == mtime {
                    this.missing.dups = dups;
                    this.missing.blockers = blockers;
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

    /// "This instance | Paxi (12) | …" above Resource packs.
    pub(crate) fn pack_tabs(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.class != cf::CLASS_RESOURCE_PACKS || self.globals.dirs.is_empty() {
            return None;
        }
        let ix = self
            .globals
            .picked
            .and_then(|p| self.globals.dirs.iter().position(|(d, _)| *d == p))
            .map_or(0, |i| i + 1);
        let dirs = self.globals.dirs.clone();
        Some(
            h_flex()
                .px_6()
                .child(
                    TabBar::new("rp-folders")
                        .segmented()
                        .small()
                        .selected_index(ix)
                        .child(Tab::new().label("This instance"))
                        .children(dirs.iter().zip(&self.globals.counts).map(|((_, name), n)| {
                            Tab::new().label(format!("{name} {n}"))
                        }))
                        .on_click(cx.listener(move |this, ix: &usize, _, cx| {
                            let picked = ix.checked_sub(1).and_then(|i| dirs.get(i)).map(|(d, _)| *d);
                            this.globals.picked = picked;
                            this.selected.clear();
                            this.resync(cx);
                        })),
                )
                .into_any_element(),
        )
    }

    /// The global folder picked (Paxi's), when Resource packs shows one.
    pub(crate) fn global_dir(&self) -> Option<PathBuf> {
        if let Some(rel) = self.folder.as_ref().filter(|_| self.class == cf::CLASS_DATAPACKS) {
            return Some(self.root.as_ref()?.join(rel));
        }
        let rel = self.globals.picked.filter(|_| self.class == cf::CLASS_RESOURCE_PACKS)?;
        Some(self.root.as_ref()?.join(rel))
    }

    /// Shows the data packs of `rel` (a Paxi or world folder) in the table.
    pub fn show_folder(
        &mut self,
        root: PathBuf,
        rel: String,
        move_targets: Vec<(String, String)>,
        cx: &mut Context<Self>,
    ) {
        self.move_targets = move_targets;
        if self.folder.as_ref() != Some(&rel) || self.root.as_ref() != Some(&root) {
            self.folder_packs = None;
            self.selected.clear();
        }
        self.folder = Some(rel);
        self.show_for(root, cf::CLASS_DATAPACKS, cx);
        self.read_folder(cx);
    }

    /// Reads the data pack folder shown, then which projects its packs
    /// are, then their versions, filling the table at each step.
    fn read_folder(&mut self, cx: &mut Context<Self>) {
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
            let file = p.path.file_name().unwrap_or_default().to_string_lossy().to_string();
            let enabled = !file.ends_with(".disabled");
            let file_name = file.trim_end_matches(".disabled").to_string();
            let hit = crate::pack_lookup::found(p).flatten();
            let mut a = Addon {
                file_name: file_name.clone(),
                name: p.name.trim_end_matches(".disabled").to_string(),
                icon: p.icon.as_ref().map(|i| i.display().to_string()).unwrap_or_default(),
                enabled,
                from_pack: true,
                class_id: class,
                date_added: hopper::time::iso_from_secs(p.modified),
                ..Default::default()
            };
            if let Some(f) = &hit {
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
                    updates.insert(
                        m.id,
                        Update { project_id: m.id, name: m.name.clone(), from: file_name.clone(), file: u.clone() },
                    );
                }
            }
            inst.addons.push(a);
        }
        self.table.inst = Some(std::rc::Rc::new(inst));
        self.table.updates = std::rc::Rc::new(updates);
        self.table.active_rp = Default::default();
        self.table.rows_for = None;
        self.ensure_rows();
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

    /// Moves a data pack (on or off) into another data pack folder.
    pub(crate) fn move_global(&mut self, file: String, rel: String, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(dir), Some(root)) = (self.global_dir(), self.root.clone()) else { return };
        let from = Self::global_path(&dir, &file);
        let to_dir = root.join(&rel);
        let dest = to_dir.join(from.file_name().unwrap_or_default());
        let r = if dest.exists() {
            Err(anyhow::anyhow!("{file} is already in {rel}"))
        } else {
            std::fs::create_dir_all(&to_dir).and_then(|_| std::fs::rename(&from, &dest)).map_err(Into::into)
        };
        match r {
            Ok(()) => window.push_notification(Notification::success(format!("Moved {file} to {rel}")), cx),
            Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
        }
        // The Data packs tab's folder counts read the store's change.
        self.store.update(cx, |_, cx| cx.notify());
        self.reload_globals(cx);
    }

    /// Reads the global folders again (after a change made here).
    fn reload_globals(&mut self, cx: &mut Context<Self>) {
        if self.folder.is_some() && self.class == cf::CLASS_DATAPACKS {
            return self.read_folder(cx);
        }
        if let Some(inst) = self.root.as_ref().and_then(|r| self.store.read(cx).instance(r)).cloned() {
            self.load_globals(&inst, cx);
        }
    }

    /// Above the table while a global folder is picked: what the folder
    /// does, and Open folder.
    pub(crate) fn global_header(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let rel = self.globals.picked.filter(|_| self.class == cf::CLASS_RESOURCE_PACKS)?;
        let dir = self.root.as_ref()?.join(rel);
        let loading = !self.globals.packs.contains_key(rel);
        let n = self.picked_packs().len();
        Some(
            h_flex()
                .px_6()
                .gap_2()
                .child(widgets::caption(
                    if loading {
                        format!("Reading the packs in {rel}…")
                    } else {
                        format!("{n} packs in {rel}: the mod loads them for every world, in its load order. Switched off, a pack is skipped.")
                    },
                    cx,
                ))
                .child(div().flex_1())
                .child(
                    Button::new("gp-folder")
                        .outline()
                        .small()
                        .icon(IconName::FolderOpen)
                        .label("Open folder")
                        .on_click(move |_, _, _| {
                            let _ = std::fs::create_dir_all(&dir);
                            open_path(&dir)
                        }),
                )
                .into_any_element(),
        )
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
                                            "{n} required mod{} missing: the game will stop at loading",
                                            if n == 1 { " is" } else { "s are" }
                                        )),
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
                    callout(theme.danger, cx)
                        .child(div().font_semibold().text_color(theme.danger).child(format!(
                            "{n} problem{} will stop the game at loading",
                            if n == 1 { "" } else { "s" }
                        )))
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

    /// Shader packs: which is on in game, and whether a loader runs them.
    pub(crate) fn shader_notes(&self, inst: &Instance, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.class != cf::CLASS_SHADERS {
            return None;
        }
        let theme = cx.theme();
        // Read from disk when the table was built, not per frame.
        let (active, has_loader) = self.shader_state();
        let forge = inst.loader.starts_with("forge");
        let want = if forge { "Oculus" } else { "Iris" };
        let root = inst.root.clone();
        Some(
            v_flex()
                .px_6()
                .gap_2()
                .when_some(active, |c, sp| {
                    c.child(
                        h_flex()
                            .gap_2()
                            .text_sm()
                            .text_color(theme.info)
                            .child(Icon::new(IconName::Sun).small())
                            .child(format!(
                                "In use in game: {}",
                                hopper::instance_ops::strip_formatting(&sp)
                            )),
                    )
                })
                .when(!has_loader, |c| {
                    c.child(
                        h_flex()
                            .gap_2()
                            .text_sm()
                            .text_color(theme.warning)
                            .child(Icon::new(IconName::TriangleAlert).small())
                            .child(format!(
                                "Shader packs need {want} to run; this instance doesn't have it."
                            ))
                            .child(
                                Button::new("find-shader-loader")
                                    .xsmall()
                                    .outline()
                                    .label(format!("Find {want}"))
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.emit(AddonsEvent::Find {
                                            root: root.clone(),
                                            text: want.to_lowercase(),
                                        })
                                    })),
                            ),
                    )
                })
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
                let v = view.clone();
                menu.separator().item(
                    PopupMenuItem::new("Save current as…")
                        .icon(IconName::Plus)
                        .on_click(move |_, window, cx| save_profile_dialog(v.clone(), window, cx)),
                )
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
