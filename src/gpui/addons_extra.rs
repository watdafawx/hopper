//! Extras around the addon table: global resource pack folders (Paxi and
//! friends), required mods that are missing, shader notes and mod profiles.

use crate::addons::{AddonsEvent, AddonsView};
use crate::notice_slot::{self, Notice, Tier};
use crate::widgets::{self, Clickable as _};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
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
/// author, version) and its update. `from_pack` false marks it "Your mod".
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

/// A server's mods as its Hopper Server lists them: files and on / off.
fn listed_key(inst: &Instance) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for a in &inst.addons {
        (&a.file_name, a.enabled).hash(&mut h);
    }
    h.finish()
}

/// What a Hopper Server's panel finds wrong with a server's mods.
fn remote_problems(r: &hopper::hopperd::Remote) -> anyhow::Result<hopper::deps::Problems> {
    let c = hopper::hopperd::Client::for_remote(r)?;
    let v = c.get(&hopper::hopperd::Client::server_path(&r.id, "/mods/problems"))?;
    Ok(serde_json::from_value(v)?)
}

/// What the load problem's "Roll back" button puts back.
enum RollBack {
    /// The copy Hopper kept before the update the last crash names.
    Crash { name: String, ch: hopper::crashdetect::Changed },
    /// An older version that works with what the mod needs.
    Offer(crate::revert_ui::Offer),
    /// History's Undo of the update that put the blocked jar in.
    Undo { name: String, file: String, saved: hopper::backups::Saved, when: i64 },
}

/// What a notice's subject names of `items`: each distinct one by its `key`
/// (an empty key drops it), all of them up to three, else the first two and
/// how many more there are.
pub(crate) fn brief_split<T>(items: Vec<T>, key: impl Fn(&T) -> String) -> (Vec<T>, usize) {
    let mut seen = std::collections::HashSet::new();
    let mut items: Vec<T> = items
        .into_iter()
        .filter(|i| {
            let k = key(i);
            !k.is_empty() && seen.insert(k)
        })
        .collect();
    match items.len() {
        0..=3 => (items, 0),
        k => {
            items.truncate(2);
            (items, k - 2)
        }
    }
}

/// A notice's subject from a list of names: "A, B, C" or "A, B and 4 more".
pub(crate) fn brief_names(names: Vec<String>) -> String {
    let (shown, more) = brief_split(names, |n| n.clone());
    match more {
        0 => shown.join(", "),
        k => format!("{}, {} and {k} more", shown[0], shown[1]),
    }
}

/// A notice's subject naming mods by their jars: the plain text for its
/// tooltip ("A, B and 4 more", as [`brief_names`]) and the line the strip
/// draws, each mod with its icon, opening its page beside the list. `mods` are
/// (name, jar file) pairs; `key` keeps the elements' ids apart.
pub(crate) fn mods_subject(
    store: &Entity<crate::store::Store>,
    root: &std::path::Path,
    mods: Vec<(String, String)>,
    key: &str,
    cx: &App,
) -> (String, AnyElement) {
    let plain = brief_names(mods.iter().map(|(n, _)| n.clone()).collect());
    let (shown, more) = brief_split(mods, |(n, _)| n.clone());
    let n = shown.len();
    // No gap: the words carry their own spaces, so a comma sits against the
    // mod before it.
    let mut line = h_flex().min_w_0().items_center();
    for (i, (_, file)) in shown.iter().enumerate() {
        line = line.child(crate::logs::mod_ref(store, root, file, &format!("{key}:{i}"), cx));
        if i + 1 < n || more > 0 {
            line = line.child(div().flex_none().whitespace_nowrap().child(if i + 1 < n { ", " } else { " and " }));
        }
    }
    if more > 0 {
        line = line.child(div().flex_none().whitespace_nowrap().child(format!("{more} more")));
    }
    (plain, line.into_any_element())
}

/// One thing a "required mod is missing" notice names: a mod the pack lacks
/// and the mod that asks for it.
struct Lack {
    /// The missing mod's id, as the jar names it.
    id: String,
    /// Its project's name once looked up, else its id.
    name: String,
    /// The jar that asks for it: its name and file.
    by: String,
    by_file: String,
}

impl Lack {
    /// "Sophisticated Core (for Sophisticated Backpacks)".
    fn text(&self) -> String {
        format!("{} (for {})", self.name, self.by)
    }
}

/// Required mods that are missing, for the mods folder as of `mtime`.
#[derive(Default)]
pub struct MissingDeps {
    pub root: Option<PathBuf>,
    pub mtime: Option<std::time::SystemTime>,
    /// On a Hopper Server (no folder here to date): the mods as listed,
    /// which change after every change made to them.
    pub listed: u64,
    pub list: Vec<Missing>,
    /// Jars that are the same mod twice.
    pub dups: Vec<hopper::install::Duplicate>,
    /// What else the loader will refuse (too old a loader, mods that
    /// refuse each other, jars it can't read).
    pub blockers: Vec<hopper::deps::Blocker>,
    /// Old files a running game holds, leaving when it closes.
    pub queued: usize,
    /// Mods a running game holds that turn off / on when it closes (switched
    /// here, or by a followed or group pack, meanwhile).
    pub switch_off: usize,
    pub switch_on: usize,
    /// When the game last loaded fine with these exact mods: the banners
    /// then say "may", not "will" (metadata misses runtime stand-ins).
    pub loaded_fine: Option<i64>,
    /// The loader and Minecraft the blockers were checked against: a new
    /// loader checks again.
    pub against: (String, String),
    /// Wrong-version rows whose asked-for version nobody published (mod
    /// id and range → a version that does exist, as an example).
    pub ghosts: std::collections::HashMap<(String, String), String>,
    /// Blocked jars an update put in, by file name: History's entry for the
    /// file it replaced and when ("Roll back" undoes it).
    pub rollbacks: std::collections::HashMap<String, (i64, hopper::backups::Saved)>,
    /// Roll backs running now, by file name.
    pub rolling: std::collections::HashSet<String>,
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
        // The one list reads every place itself (`pack_where`).
        if dirs.is_empty() || self.class != cf::CLASS_RESOURCE_PACKS || self.where_mode() {
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
                if this.globals.root.as_ref() != Some(&root) || this.class != cf::CLASS_RESOURCE_PACKS {
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

    /// Asks Modrinth where the mods CurseForge doesn't tag run, off the UI
    /// thread. The answers are cached (disk and memory), so the table only
    /// waits on mods never asked before; it redraws when they land.
    pub(crate) fn want_sides(&mut self, inst: &Instance, cx: &mut Context<Self>) {
        // A Hopper Server's list carries its own sides.
        if self.remote || self.sides_busy || crate::store::on_hopper_server(&inst.root).is_some() || !hopper::modsides::needs_look_up(inst) {
            return;
        }
        self.sides_busy = true;
        let snapshot = inst.clone();
        cx.spawn(async move |this, cx| {
            let new = cx.background_spawn(async move { hopper::modsides::look_up(&snapshot) }).await;
            this.update(cx, |this, cx| {
                this.sides_busy = false;
                if new {
                    this.sync(false, cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Looks for missing required mods when the mods folder changed.
    pub(crate) fn want_missing(&mut self, inst: &Instance, cx: &mut Context<Self>) {
        // A Hopper Server's jars are read by its panel.
        let panel = crate::store::on_hopper_server(&inst.root);
        // "Roll back X" on the problem notice, worked out in the store.
        if panel.is_none() {
            self.store.update(cx, |s, _| s.want_reverts(&inst.root, &inst.loader, &inst.game_version, &inst.addons, true));
        }
        let mtime = std::fs::metadata(inst.mods_dir()).and_then(|m| m.modified()).ok();
        let listed = if panel.is_some() { listed_key(inst) } else { 0 };
        let against = (inst.loader.clone(), inst.game_version.clone());
        // A switch waiting for the game changes no file in the mods folder.
        let switch_off = self.waiting.values().filter(|on| !**on).count();
        let switch_on = self.waiting.len() - switch_off;
        if self.missing.root.as_ref() == Some(&inst.root) && self.missing.mtime == mtime && self.missing.listed == listed && self.missing.against == against {
            self.missing.switch_off = switch_off;
            self.missing.switch_on = switch_on;
            return;
        }
        // Same instance: the old list stays up while the folder is read
        // again, so the banner doesn't flash on every update.
        let same = self.missing.root.as_ref() == Some(&inst.root);
        let waiting = hopper::locked::list(&inst.root);
        self.missing = MissingDeps {
            root: Some(inst.root.clone()),
            mtime,
            listed,
            list: if same { std::mem::take(&mut self.missing.list) } else { Vec::new() },
            dups: if same { std::mem::take(&mut self.missing.dups) } else { Vec::new() },
            blockers: if same { std::mem::take(&mut self.missing.blockers) } else { Vec::new() },
            queued: waiting.iter().filter(|q| q.to.is_none()).count(),
            switch_off,
            switch_on,
            loaded_fine: if same { self.missing.loaded_fine } else { None },
            against: against.clone(),
            ghosts: if same { std::mem::take(&mut self.missing.ghosts) } else { Default::default() },
            rollbacks: if same { std::mem::take(&mut self.missing.rollbacks) } else { Default::default() },
            rolling: if same { std::mem::take(&mut self.missing.rolling) } else { Default::default() },
        };
        let root = inst.root.clone();
        let snapshot = inst.clone();
        cx.spawn(async move |this, cx| {
            let (hopper::deps::Problems { missing: list, dups, blockers, loaded_fine }, rollbacks) = cx
                .background_spawn(async move {
                    match panel {
                        Some(r) => (
                            remote_problems(&r).unwrap_or_else(|e| {
                                log::warn!("mod problems of {}: {e:#}", snapshot.root.display());
                                Default::default()
                            }),
                            Default::default(),
                        ),
                        None => {
                            let p = hopper::deps::problems(&snapshot);
                            // An update that put a blocked jar in: its
                            // History entry, for "Roll back".
                            let sets = if p.blockers.is_empty() { Vec::new() } else { hopper::backups::list(&snapshot.root) };
                            let rollbacks = p
                                .blockers
                                .iter()
                                .filter_map(|b| Some((b.file_name.clone(), hopper::undo::brought_by(&sets, &b.file_name)?)))
                                .collect();
                            (p, rollbacks)
                        }
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                if this.missing.root.as_ref() == Some(&root) && this.missing.mtime == mtime && this.missing.listed == listed && this.missing.against == against {
                    this.missing.rollbacks = rollbacks;
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
            let still = |this: &AddonsView| this.folder == rel && this.class == cf::CLASS_DATAPACKS;
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
        if self.where_mode() {
            return self.apply_where(cx);
        }
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
        // By file: a folder can hold two files of one project.
        let mut updates = crate::addons::Offers::by_file(std::collections::HashMap::new());
        for p in &packs {
            let (a, u) = pack_addon(p, class, true);
            if let Some(u) = u {
                updates.add(u);
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
            window.push_notification(Notification::success(format!("Removed {what}. Undo from the instance's History.")), cx);
        }
        self.reload_globals(cx);
    }

    /// Updates packs in the global folder in place (the old file goes to
    /// backups). Each update replaces the file it was offered for, not
    /// whichever file of the project comes first.
    pub(crate) fn update_global(&mut self, dir: PathBuf, updates: Vec<Update>, cx: &mut Context<Self>) {
        let jobs = updates.into_iter().map(|u| (Self::global_path(&dir, u.from.trim_end_matches(".disabled")), u, None)).collect();
        self.update_paths(jobs, cx);
    }

    /// Updates each file at its path, in place: `(path, update, folder name
    /// for the job's title)`.
    pub(crate) fn update_paths(&mut self, jobs: Vec<(PathBuf, Update, Option<String>)>, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        for (path, u, place) in jobs {
            let p = PackFile { path, ..Default::default() };
            // The project the lookup found (its slug and class name the
            // website's download page); else a stand-in the update looks up.
            let project = crate::pack_lookup::found_project(&p.path, u.project_id).unwrap_or_else(|| cf::Mod {
                id: u.project_id,
                name: u.name.clone(),
                class_id: Some(self.class),
                ..Default::default()
            });
            let found = crate::pack_lookup::Found {
                project,
                version: hopper::instance_ops::PackVersion { installed: None, update: Some(u.file.clone()) },
            };
            crate::pack_lookup::update(&self.store, root.clone(), &p, &found, place.as_deref(), cx);
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
    pub(crate) fn duplicate_banner(&self, inst: &Instance, cx: &mut Context<Self>) -> Vec<Notice> {
        let mut out = Vec::new();
        if self.missing.root.as_ref() != Some(&inst.root) {
            return out;
        }
        // Fabric and Quilt refuse to start with a mod in twice: the warning.
        // NeoForge and Forge load the highest version: a copy the player
        // added gets a quiet note, and a pack that ships it so says nothing.
        let dups: Vec<&hopper::install::Duplicate> = self.missing.dups.iter().filter(|d| d.is_problem()).collect();
        let notes: Vec<&hopper::install::Duplicate> = self.missing.dups.iter().filter(|d| d.is_note()).collect();
        let queued = self.missing.queued;
        let place = inst.root.to_string_lossy();
        if !dups.is_empty() {
            let rows = self.dup_rows(inst, &dups, false, cx);
            let extra: Vec<String> = dups.iter().flat_map(|d| d.files[1..].to_vec()).collect();
            let n = dups.len();
            let title = format!("{n} mod{} in twice: the game won't start with both", if n == 1 { " is" } else { "s are" });
            let what = if n == 1 { dups[0].name.clone() } else { format!("{n} older duplicates") };
            let keep = cx.listener(move |this, _: &ClickEvent, window, cx| this.remove(extra.clone(), what.clone(), window, cx));
            let (subject, rich) = mods_subject(
                &self.store,
                &inst.root,
                dups.iter().map(|d| (d.name.clone(), d.files.first().cloned().unwrap_or_default())).collect(),
                "dups-strip",
                cx,
            );
            let details = v_flex()
                .gap_2()
                .when(queued > 0, |c| {
                    c.child(widgets::caption(
                        format!("{queued} old file{} will be removed when the game closes.", if queued == 1 { "" } else { "s" }),
                        cx,
                    ))
                })
                .children(rows);
            out.push(
                Notice::new("dups", Tier::Stop, title)
                    .subject_rich(subject, rich)
                    .primary(move |fit| {
                        notice_slot::main_button(Button::new("keep-newest-all").small(), fit)
                            .label(if n > 1 { "Keep newest of all" } else { "Keep newest" })
                            .tooltip("Removes the older jars (undo from History)")
                            .on_click(keep)
                    })
                    .details(details),
            );
        }
        if !notes.is_empty() {
            out.extend(self.dup_note(inst, &notes, &place, cx));
        }
        out
    }

    /// One row per mod in twice: its icon and name, which file stays, and the
    /// button that removes the other copies. `loads`: the copy kept is the
    /// one the game loads (NeoForge, Forge), not just the newest file.
    fn dup_rows(&self, inst: &Instance, list: &[&hopper::install::Duplicate], loads: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        list.iter()
            .map(|d| {
                let older = d.files[1..].to_vec();
                let what = d.name.clone();
                let addon = inst.addons.iter().find(|a| a.file_name == d.files[0]);
                let (pid, icon) = (
                    addon.map_or(0, |a| a.project_id),
                    addon.and_then(|a| self.store.read(cx).image(&a.icon)).map(std::path::Path::to_path_buf),
                );
                let view = cx.entity();
                let text = v_flex()
                    .min_w_0()
                    .gap_0p5()
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
                            format!("{} {}  ·  remove {}", if loads { "loads" } else { "keep" }, d.files[0], older.join(", ")),
                            cx,
                        )
                        .text_xs()
                        .whitespace_normal(),
                    );
                let keep = Button::new(SharedString::from(format!("keep-newest:{}", d.files[0])))
                    .xsmall()
                    .outline()
                    .label(if loads { "Remove other copy" } else { "Keep newest" })
                    .tooltip(if loads {
                        "Remove the copy that doesn't load (undo from History)"
                    } else {
                        "Remove the older jars (undo from History)"
                    })
                    .on_click(cx.listener(move |this, _, window, cx| this.remove(older.clone(), what.clone(), window, cx)))
                    .into_any_element();
                widgets::detail_row(text, vec![keep]).into_any_element()
            })
            .collect()
    }

    /// NeoForge and Forge load the highest version of a mod that is in
    /// twice, so a copy the player added beside the pack's isn't a problem:
    /// a quiet note says which one loads, with the remove button.
    fn dup_note(&self, inst: &Instance, notes: &[&hopper::install::Duplicate], place: &str, cx: &mut Context<Self>) -> Option<Notice> {
        let n = notes.len();
        let key = hopper::settings::notice_key(place, "dups-note", &notes.iter().map(|d| d.files.join(",")).collect::<Vec<_>>().join(";"));
        if notice_slot::hidden(&self.store, &key, cx) {
            return None;
        }
        let (title, subject) = if n == 1 {
            let note = notes[0].note();
            (format!("{} is in twice", notes[0].name), note.split_once(". ").map_or(note.clone(), |(_, rest)| rest.to_string()))
        } else {
            (format!("{n} mods are in twice"), "Only the highest version of each loads; the other copies do nothing.".to_string())
        };
        let rows = self.dup_rows(inst, notes, true, cx);
        Some(
            Notice::new("dups-note", Tier::Info, title)
                .icon(IconName::Copy)
                .subject(subject)
                .details(v_flex().gap_2().children(rows))
                .dismiss_key(&self.store, key),
        )
    }

    /// "N required mods are missing", with Enable or Find for each.
    pub(crate) fn missing_banner(&self, inst: &Instance, cx: &mut Context<Self>) -> Option<Notice> {
        if self.missing.root.as_ref() != Some(&inst.root) || self.missing.list.is_empty() {
            return None;
        }
        // A group copy already says this in its sync strip, with a button that
        // also sends the mod to the group: one notice, not two.
        if matches!(self.store.read(cx).group_status(&inst.root), Some(hopper::groupstatus::Status::NeedsMod { .. })) {
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
        // The strip's Install is the notice's main button; each line's
        // Install under Details is an outline.
        let one_primary = false;
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
                                    // Loading already blocks clicks; disabled
                                    // would fade the label.
                                    .loading(true)
                                    .label(label),
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
                                .hover(|s| s.text_color(crate::theme::ink(theme.primary)))
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
        let title = match loaded_fine {
            Some(_) => format!(
                "{n} mod{} list{} a requirement that isn't installed",
                if n == 1 { "" } else { "s" },
                if n == 1 { "s" } else { "" }
            ),
            None => format!(
                "{n} required mod{} missing",
                if n == 1 { " is" } else { "s are" }
            ),
        };
        let key = hopper::settings::notice_key(
            &inst.root.to_string_lossy(),
            "missing",
            &self.missing.list.iter().map(|m| format!("{}>{}", m.mod_name, m.needs)).collect::<Vec<_>>().join(";"),
        );
        // It loaded fine anyway: only a note, which can be hidden.
        if loaded_fine.is_some() && notice_slot::hidden(&self.store, &key, cx) {
            return None;
        }
        // "Sophisticated Core (for Sophisticated Backpacks), …": what's
        // missing and what asks for it. Every mod named has its icon and
        // opens its page beside the list; the plain text is the tooltip.
        let lacks: Vec<Lack> = {
            let known = self.dep_projects.borrow();
            self.missing
                .list
                .iter()
                .map(|m| Lack {
                    id: m.needs.clone(),
                    name: known.get(&m.needs).cloned().flatten().map_or_else(|| m.needs.clone(), |(_, n, _)| n),
                    by: m.mod_name.clone(),
                    by_file: m.file_name.clone(),
                })
                .collect()
        };
        let subject = brief_names(lacks.iter().map(Lack::text).collect());
        let rich = self.lacks_line(inst, lacks, cx);
        let hover = cx.listener(|this, h: &bool, _, _| this.settle.pointer_in = *h);
        let details = div()
            .id("missing-rows")
            .on_hover(hover)
            .child(
                v_flex()
                    .gap_2()
                    .when_some(loaded_fine, |c, at| c.child(widgets::caption(loaded_fine_note(at), cx).whitespace_normal()))
                    .children(rows),
            );
        let mut notice = Notice::new("missing", if loaded_fine.is_some() { Tier::Info } else { Tier::Stop }, title)
            .icon(IconName::PackageX)
            .subject_rich(subject, rich)
            .details(details);
        if !installable.is_empty() {
            let k = installable.len();
            let get = cx.listener(move |this, _: &ClickEvent, _, cx| this.install_missing(installable.clone(), cx));
            notice = notice.primary(move |fit| {
                notice_slot::main_button(Button::new("install-all-missing").small(), fit)
                    .icon(IconName::Download)
                    .label(if k > 1 { format!("Install all {k}") } else { "Install".to_string() })
                    .tooltip("Downloads them from Modrinth or CurseForge, with what they need")
                    .on_click(get)
            });
        }
        if loaded_fine.is_some() {
            notice = notice.dismiss_key(&self.store, key);
        }
        Some(notice)
    }

    /// The notice's subject line as elements: "[icon] Arcane Framework (for
    /// [icon] Spell Atlas), …". A mod whose project is known opens it beside
    /// the list; one that isn't yet shows its id and looks the project up on
    /// click (Browse searches for it when there is none). A jar the pack didn't
    /// get from a site has no page: its name and icon only.
    fn lacks_line(&self, inst: &Instance, lacks: Vec<Lack>, cx: &mut Context<Self>) -> AnyElement {
        let (shown, more) = brief_split(lacks, Lack::text);
        let view = cx.entity();
        let root = inst.root.clone();
        let store = self.store.clone();
        // The page opens through the store, as the dialogs' links do: the
        // Overview tab has no panel of its own, so the Library moves to Mods.
        let open = move |root: std::path::PathBuf, project: u64, cx: &mut App| {
            store.update(cx, |_, cx| cx.emit(crate::store::StoreEvent::Peek { root, project }));
        };
        let st = self.store.read(cx);
        let known = self.dep_projects.borrow();
        // No gap: the words carry their own spaces.
        let mut line = h_flex().min_w_0().items_center();
        let n = shown.len();
        for (i, l) in shown.into_iter().enumerate() {
            let project = known.get(&l.id).cloned().flatten();
            let dep_icon = project.as_ref().and_then(|(_, _, ic)| st.image(ic)).map(std::path::Path::to_path_buf);
            let v = view.clone();
            let (open1, root1) = (open.clone(), root.clone());
            let dep = match project {
                Some((pid, ..)) => widgets::mod_link(
                    SharedString::from(format!("miss-dep:{i}")),
                    l.name.clone(),
                    dep_icon.as_deref(),
                    pid,
                    move |p, _, cx| open1(root1.clone(), p, cx),
                    cx,
                )
                .font_semibold(),
                None => {
                    let id = l.id.clone();
                    widgets::mod_link_lookup(
                        SharedString::from(format!("miss-dep:{i}")),
                        l.name.clone(),
                        dep_icon.as_deref(),
                        move |_, cx| v.update(cx, |this, cx| this.peek_mod_id(id.clone(), cx)),
                        cx,
                    )
                    .font_semibold()
                }
            };
            let jar = inst
                .addons
                .iter()
                .find(|a| a.file_name.eq_ignore_ascii_case(&l.by_file))
                .or_else(|| inst.addons.iter().find(|a| a.name == l.by));
            let by_icon = jar.and_then(|a| st.image(&a.icon)).map(std::path::Path::to_path_buf);
            let (open2, root2) = (open.clone(), root.clone());
            let by = widgets::mod_link(
                SharedString::from(format!("miss-by:{i}")),
                l.by.clone(),
                by_icon.as_deref(),
                jar.map_or(0, |a| a.project_id),
                move |p, _, cx| open2(root2.clone(), p, cx),
                cx,
            )
            .font_semibold();
            line = line
                .child(dep)
                .child(div().flex_none().whitespace_nowrap().child(" (for "))
                .child(by)
                .child(div().flex_none().whitespace_nowrap().child(if i + 1 < n || more > 0 { "), " } else { ")" }));
        }
        if more > 0 {
            line = line.child(div().flex_none().whitespace_nowrap().child(format!("and {more} more")));
        }
        line.into_any_element()
    }

    /// Downloads mods jars ask for by id; one that can't be found is
    /// reported and the rest still go in. A failure offers a search.
    pub(crate) fn install_missing(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let ids: Vec<String> = ids
            .into_iter()
            .filter(|i| !self.dep_jobs.contains_key(i) && !self.dep_done.contains(i))
            .collect();
        if ids.is_empty() {
            return;
        }
        let Some(job) = self.store.read(cx).install_mod_ids(&root, ids.clone()) else { return };
        self.settle.clicked();
        for id in ids {
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
                Ok(Some(m)) => {
                    let project = m.id;
                    this.store.update(cx, |_, cx| cx.emit(crate::store::StoreEvent::Peek { root, project }));
                }
                // Browse matches whole words: "arcane framework", not "arcane_framework".
                _ => cx.emit(AddonsEvent::Find { root, text: hopper::install::search_text_for_mod_id(&mod_id) }),
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

    /// Problems the loader will hit (missing or doubled mods, blockers):
    /// the Logs & crashes tab's amber dot.
    pub(crate) fn load_problems(&self, root: &std::path::Path) -> usize {
        if self.missing.root.as_deref() != Some(root) {
            return 0;
        }
        // A mod in twice that NeoForge or Forge loads the newest of isn't one.
        self.missing.list.len() + self.missing.dups.iter().filter(|d| d.is_problem()).count() + self.missing.blockers.len()
    }

    /// What stops play about this mod, for the marker on its row: it needs a
    /// mod that isn't installed, or the loader refuses it (or the last crash
    /// stopped at it). Only for problems the "Needs you" chip lists as
    /// stopping play: a requirement the game loaded fine without is only a note.
    pub(crate) fn row_stop(&self, file_name: &str) -> Option<hopper::syncchip::RowProblem> {
        use hopper::syncchip::RowProblem;
        let root = self.root.as_ref()?;
        let bare = file_name.trim_end_matches(".disabled");
        let same = self.missing.root.as_ref() == Some(root);
        if same && self.missing.loaded_fine.is_none() {
            let known = self.dep_projects.borrow();
            let names: Vec<String> = self
                .missing
                .list
                .iter()
                .filter(|m| m.file_name.eq_ignore_ascii_case(bare))
                .map(|m| known.get(&m.needs).cloned().flatten().map_or_else(|| m.needs.clone(), |(_, n, _)| n))
                .collect();
            if !names.is_empty() {
                return Some(RowProblem::Needs(names));
            }
        }
        let crash = self.crash_stop.as_ref().is_some_and(|(st, _)| st.file_name.trim_end_matches(".disabled").eq_ignore_ascii_case(bare));
        let blocked = same && self.missing.loaded_fine.is_none() && self.missing.blockers.iter().any(|b| b.file_name.eq_ignore_ascii_case(bare));
        (crash || blocked).then_some(RowProblem::StopsLoading)
    }

    /// "N problems will stop the game at loading": what the loader refuses
    /// besides missing mods, with Turn off where that fixes it.
    pub(crate) fn blocker_banner(&self, inst: &Instance, cx: &mut Context<Self>) -> Option<Notice> {
        use hopper::deps::BlockerKind;
        let blockers: &[hopper::deps::Blocker] =
            if self.missing.root.as_ref() == Some(&inst.root) { &self.missing.blockers } else { &[] };
        // The last crash's "stopped loading" mod, unless a blocker row
        // names it already.
        let stop = self.crash_stop.as_ref().filter(|(st, _)| {
            !blockers.iter().any(|b| b.file_name.eq_ignore_ascii_case(st.file_name.trim_end_matches(".disabled")))
        });
        // The crash is this table's instance's (`sync`).
        let stop = stop.filter(|_| self.root.as_ref() == Some(&inst.root));
        if blockers.is_empty() && stop.is_none() {
            return None;
        }
        // A crash says loading broke, whatever the files look like.
        let loaded_fine = self.missing.loaded_fine.filter(|_| stop.is_none());
        let n = blockers.len() + usize::from(stop.is_some());
        let mut rows: Vec<AnyElement> = Vec::new();
        // The strip's line: what the first problem is, in words.
        let mut subject = String::new();
        // Its main fix: rolling the mod back, when an older copy fixes it.
        let mut roll: Option<RollBack> = None;
        if let Some((st, changed)) = stop {
            rows.push(self.crash_stop_row(inst, st, changed.as_ref(), cx));
            let name = self.jar_label(inst, &st.file_name);
            subject = match changed {
                Some(ch) => format!(
                    "{name} stopped the game from loading. {} was updated {}",
                    if ch.name.is_empty() { self.jar_label(inst, &ch.file_name) } else { ch.name.clone() },
                    hopper::time::relative_secs(ch.when)
                ),
                None => format!("{name} stopped the game from loading"),
            };
            if let Some(ch) = changed.clone() {
                let changed_name = if ch.name.is_empty() { self.jar_label(inst, &ch.file_name) } else { ch.name.clone() };
                roll = Some(RollBack::Crash { name: changed_name, ch });
            }
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
            if roll.is_none()
                && let Some(offer) = self.store.read(cx).revert_offer(&inst.root, &b.file_name)
            {
                roll = Some(RollBack::Offer(offer));
            }
            // An update just put this jar in: undoing it is the main fix.
            if roll.is_none()
                && let Some((when, saved)) = self.missing.rollbacks.get(&b.file_name)
            {
                roll = Some(RollBack::Undo { name: self.jar_label(inst, &b.file_name), file: b.file_name.clone(), saved: saved.clone(), when: *when });
            }
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
            // An update just put this file in: undo that update, unless a
            // Revert already offers an older version.
            // (The notice's main button already does it for this jar.)
            let main_undo = matches!(&roll, Some(RollBack::Undo { file, .. }) if file == &b.file_name);
            if !main_undo
                && self.store.read(cx).revert_offer(&inst.root, &b.file_name).is_none()
                && let Some(el) = self.rollback_action(i, inst, &b.file_name, cx)
            {
                // Beside the Turn off for this jar (the first button).
                actions.insert(actions.len().min(1), el);
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
            if subject.is_empty() {
                let o = other.as_ref().map_or_else(String::new, |o| o.2.clone());
                subject = [b.mod_name.as_str(), parts.lead.as_str(), o.as_str(), parts.tail.as_str()]
                    .into_iter()
                    .filter(|p| !p.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
            }
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
                widgets::detail_row(
                    v_flex()
                        .min_w_0()
                        .gap_0p5()
                        .child(sentence)
                        .when(!why.is_empty(), |c| c.child(widgets::caption(why.join(" · "), cx).text_xs().whitespace_normal())),
                    actions,
                )
                .into_any_element(),
            );
        }
        let loader_note = blockers
            .iter()
            .any(|b| matches!(&b.kind, BlockerKind::Loader { id, .. } if id != "minecraft"));
        let title = match loaded_fine {
            Some(_) => format!("{n} possible problem{} in the mods' own files", if n == 1 { "" } else { "s" }),
            None if stop.is_some() && blockers.is_empty() => "The game stopped at loading".to_string(),
            None => format!("{n} problem{} will stop the game at loading", if n == 1 { "" } else { "s" }),
        };
        let key = hopper::settings::notice_key(
            &inst.root.to_string_lossy(),
            "blockers",
            &format!("{:?}{:?}", stop.map(|(st, _)| &st.file_name), blockers),
        );
        if loaded_fine.is_some() && notice_slot::hidden(&self.store, &key, cx) {
            return None;
        }
        let details = v_flex()
            .gap_2()
            .when_some(loaded_fine, |c, at| c.child(widgets::caption(loaded_fine_note(at), cx).whitespace_normal()))
            .when(loader_note, |c| {
                c.child(
                    widgets::caption(
                        "For a mod that needs a newer loader, update the loader here or pick a version in this instance's settings.",
                        cx,
                    )
                    .whitespace_normal(),
                )
            })
            .children(rows);
        let mut notice = Notice::new("blockers", if loaded_fine.is_some() { Tier::Info } else { Tier::Stop }, title)
            .subject(subject)
            .since(stop.and_then(|(_, ch)| ch.as_ref().map(|c| c.when)).unwrap_or(0))
            .details(details);
        if let Some(roll) = roll {
            notice = notice.primary(self.roll_back_button(inst, roll, cx));
        }
        // The crash's own page: what happened, in full.
        if stop.is_some() {
            notice = notice.action(
                Button::new("blockers-see")
                    .small()
                    .outline()
                    .label("See why →")
                    .tooltip("Opens Logs & crashes on the last crash")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(AddonsEvent::SeeCrash))),
            );
        }
        if loaded_fine.is_some() {
            notice = notice.dismiss_key(&self.store, key);
        }
        Some(notice)
    }

    /// A jar's mod name, from the instance's list, else from the file.
    fn jar_label(&self, inst: &Instance, file: &str) -> String {
        let bare = file.trim_end_matches(".disabled");
        inst.addons
            .iter()
            .find(|a| a.file_name.eq_ignore_ascii_case(bare))
            .map(|a| a.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| hopper::crashdetect::jar_name(&inst.root.join("mods"), file))
    }

    /// The load problem's main button: "Roll back Create to 6.0.4", from
    /// the copy Hopper kept before an update (the crash's), a revert offer
    /// (a blocker an older version fixes) or History's Undo of the update
    /// that put a blocked jar in. Narrow: "Roll back Create".
    fn roll_back_button(&self, inst: &Instance, roll: RollBack, cx: &mut Context<Self>) -> impl FnOnce(notice_slot::Fit) -> AnyElement + 'static {
        let root = inst.root.clone();
        let view = cx.entity().downgrade();
        let (label, short, tip) = match &roll {
            RollBack::Crash { name, ch } => (
                if ch.from.is_empty() { format!("Roll back {name}") } else { format!("Roll back {name} to {}", ch.from) },
                format!("Roll back {name}"),
                "Swaps in the copy Hopper kept. The newer one goes to History, so you can undo this there.".to_string(),
            ),
            RollBack::Offer(o) => {
                let (label, tip) = o.label();
                (label, format!("Roll back {}", o.name), tip)
            }
            RollBack::Undo { name, saved, when, .. } => (
                format!("Roll back {name} to {}", hopper::undo::version_of(&saved.name)),
                format!("Roll back {name}"),
                format!("Undoes the update on {}, as History's Undo does", hopper::time::day_month(*when)),
            ),
        };
        let busy = matches!(&roll, RollBack::Undo { file, .. } if self.missing.rolling.contains(file));
        move |fit| {
            let tooltip = if fit.narrow { format!("{label}. {tip}") } else { tip };
            notice_slot::main_button(Button::new("notice-roll-back").small(), fit)
                .icon(IconName::Undo2)
                .label(if fit.narrow { short } else { label })
                .tooltip(tooltip)
                .loading(busy)
                .disabled(busy)
                .on_click(move |_, window, cx| {
                    view.update(cx, |this, cx| {
                        let store = this.store.clone();
                        let done = match &roll {
                            RollBack::Crash { ch, .. } => {
                                let ok = crate::crash_help::put_back_changed(&store, &root, ch, window, cx);
                                if ok {
                                    this.crash_stop = None;
                                }
                                ok
                            }
                            RollBack::Offer(o) => crate::revert_ui::revert_instance(&store, &root, o, window, cx),
                            RollBack::Undo { file, saved, .. } => {
                                // Re-reads the folder itself when it's done.
                                this.roll_back(&root, file, saved.clone(), window, cx);
                                false
                            }
                        };
                        if done {
                            // Untracked jars changed too: re-read the folder.
                            this.resync(cx);
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .into_any_element()
        }
    }

    /// "Roll back" for a blocked jar an update put in: History's Undo of that
    /// update ([`crate::history_view::queue_undo`]), then the notice checks
    /// the folder again.
    fn rollback_action(&self, i: usize, inst: &Instance, file: &str, cx: &Context<Self>) -> Option<AnyElement> {
        let (when, saved) = self.missing.rollbacks.get(file)?.clone();
        let version = hopper::undo::version_of(&saved.name);
        let busy = self.missing.rolling.contains(file);
        let (root, file) = (inst.root.clone(), file.to_string());
        Some(
            Button::new(SharedString::from(format!("blk-rollback:{i}")))
                .xsmall()
                .outline()
                .label("Roll back")
                .loading(busy)
                .disabled(busy)
                .tooltip(format!("Back to {version}, from before the update on {}", hopper::time::day_month(when)))
                .on_click(cx.listener(move |this, _, window, cx| this.roll_back(&root, &file, saved.clone(), window, cx)))
                .into_any_element(),
        )
    }

    /// Undoes the update that put `file` in, as History's Undo does.
    fn roll_back(&mut self, root: &std::path::Path, file: &str, saved: hopper::backups::Saved, window: &mut Window, cx: &mut Context<Self>) {
        let Some(slot) = crate::history_view::queue_undo(self.store.read(cx), root, vec![saved], None) else { return };
        self.missing.rolling.insert(file.to_string());
        cx.notify();
        let (root, file) = (root.to_path_buf(), file.to_string());
        cx.spawn_in(window, async move |this, cx| {
            let got = crate::store::work_done(cx.background_executor().clone(), slot).await;
            this.update_in(cx, |this, window, cx| {
                this.missing.rolling.remove(&file);
                if let Some(crate::history_view::UndoDone { done, gone, website, errors, .. }) = got {
                    crate::history_view::undone(&this.store, &root, &done, cx);
                    // Only the CurseForge website has the old file: the
                    // download window gets it, as History's Undo does.
                    for (f, w) in website {
                        crate::history_view::get_from_website(&this.store, &root, f, w, window, cx);
                    }
                    if let Some(p) = done.first().and_then(|p| p.file_name()) {
                        window.push_notification(Notification::success(format!("Rolled back {}", p.to_string_lossy())), cx);
                    }
                    if let Some((f, g)) = gone {
                        window.push_notification(Notification::error(format!("{}: {g}. History can roll back to another version.", f.name)), cx);
                    }
                    for e in errors {
                        window.push_notification(Notification::error(e), cx);
                    }
                }
                // The notice checks the folder again.
                this.resync(cx);
            })
            .ok();
        })
        .detach();
    }

    /// "Roll back X to 1.6.3" for a problem an older version of the mod fixes
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
    /// loading after Y was updated (a → b)", with Turn off X.
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
        // Putting the older copy back is the strip's own main button
        // ("Roll back X to a"), so the row only offers turning it off.
        let mut actions: Vec<AnyElement> = Vec::new();
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
        widgets::detail_row(
            v_flex().min_w_0().gap_0p5().child(sentence).child(widgets::caption(why, cx).text_xs().whitespace_normal()),
            actions,
        )
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

    /// Shader packs need Iris or Oculus: a notice with Find when it's missing.
    pub(crate) fn shader_notes(&self, inst: &Instance, cx: &mut Context<Self>) -> Option<Notice> {
        // Servers don't draw shaders.
        if self.server || self.remote {
            return None;
        }
        // Read from disk when the table was built, not per frame (on the
        // Shaders tab always, elsewhere only once packs are there).
        let (_, has_loader) = self.shader_state();
        if has_loader {
            return None;
        }
        let want = if inst.loader.starts_with("forge") { "Oculus" } else { "Iris" };
        let root = inst.root.clone();
        let title = "Install Iris or Oculus to use shaders";
        let key = hopper::settings::notice_key(&inst.root.to_string_lossy(), "shader-loader", title);
        if notice_slot::hidden(&self.store, &key, cx) {
            return None;
        }
        let view = cx.entity().downgrade();
        Some(
            Notice::new("shader-loader", Tier::Choice, title)
                .subject("Shader packs here do nothing without one of them")
                .primary(move |fit| {
                    notice_slot::main_button(Button::new("find-shader-loader").small(), fit)
                        .icon(IconName::Search)
                        .label(format!("Find {want}"))
                        .on_click(move |_, _, cx| {
                            view.update(cx, |_, cx| cx.emit(AddonsEvent::Find { root: root.clone(), text: want.to_lowercase() })).ok();
                        })
                })
                .dismiss_key(&self.store, key),
        )
    }

    /// Old files and switches waiting for the game to close, in words (the
    /// problem notice says it for its own files). `None` when nothing
    /// waits, a problem says it, or the player hid it.
    fn waiting_line(&self, inst: &Instance, cx: &App) -> Option<(String, String)> {
        if self.missing.root.as_ref() != Some(&inst.root) || self.missing.dups.iter().any(|d| d.is_problem()) {
            return None;
        }
        let (queued, off, on) = (self.missing.queued, self.missing.switch_off, self.missing.switch_on);
        if queued + off + on == 0 {
            return None;
        }
        let s = |n: usize| if n == 1 { "" } else { "s" };
        let mut said = Vec::new();
        if queued > 0 {
            said.push(format!("{queued} old file{} the game still has open will be removed when it closes.", s(queued)));
        }
        if off > 0 {
            said.push(format!("{off} mod{} the game has open will be turned off when it closes.", s(off)));
        }
        if on > 0 {
            said.push(format!("{on} mod{} the game has open will be turned on when it closes.", s(on)));
        }
        let text = said.join(" ");
        let key = hopper::settings::notice_key(&inst.root.to_string_lossy(), "queued", &text);
        (!notice_slot::hidden(&self.store, &key, cx)).then_some((text, key))
    }

    /// The Restart chip while the game runs: mods or packs changed under it
    /// ("restart to load"), and old files and switches that wait for it to
    /// close. Each part has its Hide, as the strips had their ✕.
    pub(crate) fn restart_chip(&self, inst: &Instance, cx: &App) -> Option<crate::sync_chip::Chip> {
        use crate::sync_chip::{Action, Chip, Section};
        let changed = self.store.read(cx).changed_while_running(&inst.root);
        let waiting = self.waiting_line(inst, cx);
        if !changed && waiting.is_none() {
            return None;
        }
        let hide = |label: &str, run: crate::sync_chip::Run| Action { label: label.to_string().into(), icon: None, primary: false, disabled: false, run };
        let mut sections = Vec::new();
        if changed {
            let (store, root) = (self.store.clone(), inst.root.clone());
            sections.push(Section {
                title: "Changed while the game runs".into(),
                line: "Restart it to load the changes.".into(),
                progress: None,
                body: None,
                actions: vec![hide(
                    "Hide",
                    std::rc::Rc::new(move |_, cx| {
                        store.update(cx, |s, cx| {
                            s.dismiss_changed_while_running(&root);
                            cx.notify();
                        })
                    }),
                )],
            });
        }
        if let Some((text, key)) = waiting {
            let store = self.store.clone();
            sections.push(Section {
                title: "Waiting for the game to close".into(),
                line: text.into(),
                progress: None,
                body: None,
                actions: vec![hide(
                    "Hide",
                    std::rc::Rc::new(move |w, cx| {
                        store.update(cx, |s, cx| s.hide_notice(&key, cx));
                        w.refresh();
                    }),
                )],
            });
        }
        Some(Chip { id: "restart".into(), look: hopper::syncchip::restart_look(changed, "game"), progress: None, sections, auto_open: Vec::new(), width: None, on_click: None })
    }

    /// Named sets of disabled mods to switch between.
    pub(crate) fn profiles_button(&self, inst: &Instance, icon_only: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let names: Vec<String> = inst.mod_profiles.iter().map(|p| p.name.clone()).collect();
        let view = cx.entity();
        Button::new("profiles")
            .small()
            .outline()
            .icon(IconName::Layers)
            .map(|b| if icon_only { b.tooltip("Profiles") } else { b.label("Profiles") })
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
        if self.remote {
            self.store.update(cx, |s, _| s.use_remote_profile(&root, &name));
            return;
        }
        let store = self.store.read(cx);
        if let Some(mut inst) = store.instance(&root).cloned() {
            store.spawn_job(format!("Switching to profile {name}"), move |job| {
                let _one = hopper::actor::batch(&inst.root, hopper::actor::Who::you());
                let n = hopper::install::apply_profile(&mut inst, &name)?;
                job.detail(format!("{} changed", hopper::words::count(n, "mod")));
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

#[cfg(test)]
mod tests {
    use super::{Lack, brief_names, brief_split};

    fn lack(id: &str, name: &str, by: &str) -> Lack {
        Lack { id: id.into(), name: name.into(), by: by.into(), by_file: format!("{by}.jar") }
    }

    #[test]
    fn a_short_list_names_everything_once() {
        let names: Vec<String> = ["A", "B", "A", ""].iter().map(|s| s.to_string()).collect();
        assert_eq!(brief_names(names), "A, B");
    }

    #[test]
    fn a_long_list_names_two_and_counts_the_rest() {
        let names: Vec<String> = ["A", "B", "C", "D", "E"].iter().map(|s| s.to_string()).collect();
        assert_eq!(brief_names(names.clone()), "A, B and 3 more");
        let (shown, more) = brief_split(names, |n| n.clone());
        assert_eq!((shown.len(), more), (2, 3));
    }

    /// The strip draws these items as links; the tooltip says the same words.
    #[test]
    fn the_links_and_the_plain_text_name_the_same_mods() {
        let lacks = vec![
            lack("arcane_framework", "arcane_framework", "Spell Atlas"),
            lack("arcane_framework", "arcane_framework", "Spell Atlas"),
            lack("owo-lib", "owo-lib", "Shapewright"),
        ];
        let plain = brief_names(lacks.iter().map(Lack::text).collect());
        let (shown, more) = brief_split(lacks, Lack::text);
        assert_eq!(more, 0);
        assert_eq!(shown.len(), 2, "the same pair is named once");
        let drawn: Vec<String> = shown.iter().map(Lack::text).collect();
        assert_eq!(drawn.join(", "), plain);
        assert_eq!(plain, "arcane_framework (for Spell Atlas), owo-lib (for Shapewright)");
    }
}
