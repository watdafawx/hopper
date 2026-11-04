//! Resource packs and data packs as one list: every place a pack can be
//! (the instance's folder, Paxi, Global Packs, OpenLoader, each world) in
//! one table, grouped by where the pack counts (In game, Always on, In
//! worlds, Off). Each row has a Where pill, and dragging a row to another
//! group changes its Where the same way. The file work is planned in
//! `hopper::pack_where`; here are the rows, the pill, dragging and the
//! warning before a modpack's own Paxi pack leaves Paxi.

use crate::addons::{AddonsView, ROW_H, Row, in_class};
use crate::addons_extra::{pack_addon, pack_files};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::cf;
use hopper::instance_ops::PackFile;
use hopper::instances::Instance;
use hopper::pack_where::{self as pw, Entry, Group, Step, Target};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Why resource pack changes wait while the game runs.
const RUNNING_TIP: &str = "Close the game first: it rewrites options.txt when it exits";

/// The Where pill's width (room for "Always on · Global Packs").
pub(crate) const PILL_W: f32 = 10.5;

/// What the list knows beyond the table's rows.
#[derive(Default)]
pub(crate) struct WhereState {
    /// Each pack by lower-case file name.
    pub entries: Rc<HashMap<String, Entry>>,
    /// The packs read in each location other than the instance's own
    /// folder (names, icons), by location key.
    pub packs: HashMap<String, Vec<PackFile>>,
    /// A row whose file isn't the instance's: the folder it's in.
    pub rep: Rc<HashMap<String, PathBuf>>,
    /// The folders' times when `packs` was read, and for which instance.
    pub stamp: Vec<Option<std::time::SystemTime>>,
    pub root: Option<PathBuf>,
    /// The pack being dragged and its group, while a drag is on.
    pub dragging: Option<(String, Group)>,
}

/// A row dragged by its grip, to another place or group.
#[derive(Clone)]
pub(crate) struct WhereDrag {
    file: String,
    name: SharedString,
}

impl Render for WhereDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .ml_4()
            .mt_2()
            .px_2p5()
            .py_1()
            .rounded(theme.radius)
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border)
            .shadow_md()
            .text_sm()
            .child(self.name.clone())
    }
}

fn mtime(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// A loader's short name ("Paxi", "Global Packs").
fn loader_name(key: &str, label: &str) -> String {
    match key {
        "paxi" => "Paxi".into(),
        "paxi-optional" => "Paxi optional".into(),
        _ => label.to_string(),
    }
}

impl AddonsView {
    /// Resource packs and data packs are one list with a Where per pack.
    pub(crate) fn where_mode(&self) -> bool {
        matches!(self.class, cf::CLASS_RESOURCE_PACKS | cf::CLASS_DATAPACKS)
    }

    pub(crate) fn where_entry(&self, file: &str) -> Option<&Entry> {
        self.wh.entries.get(&file.trim_end_matches(".disabled").to_lowercase())
    }

    /// The folder a row's file is in, when it isn't the instance's own.
    pub(crate) fn rep_dir(&self, file: &str) -> Option<PathBuf> {
        if !self.where_mode() {
            return None;
        }
        self.wh.rep.get(&file.trim_end_matches(".disabled").to_lowercase()).cloned()
    }

    /// Whether project `pid` is a row whose file isn't the instance's.
    pub(crate) fn rep_pid(&self, pid: u64) -> bool {
        pid != 0
            && self.where_mode()
            && self
                .table
                .inst
                .as_ref()
                .is_some_and(|i| i.addons.iter().any(|a| a.project_id == pid && self.rep_dir(&a.file_name).is_some()))
    }

    /// The locations whose packs are read here (all but the instance's
    /// own folder, which the table reads itself).
    fn where_dirs(&self) -> Vec<(String, PathBuf)> {
        self.locs.iter().filter(|l| l.key != pw::INSTANCE).map(|l| (l.key.clone(), l.dir.clone())).collect()
    }

    fn where_stamp(&self) -> Vec<Option<std::time::SystemTime>> {
        self.locs.iter().map(|l| mtime(&l.dir)).collect()
    }

    /// Whether a pack folder changed since the list was read.
    pub(crate) fn where_moved(&self) -> bool {
        self.where_mode() && self.wh.root.is_some() && self.where_stamp() != self.wh.stamp
    }

    /// Reads each location's packs in the background (a big Paxi folder
    /// takes a few hundred ms of zip reading), then which projects they
    /// are and their updates, filling the table at each step.
    pub(crate) fn load_where(&mut self, inst: &Instance, cx: &mut Context<Self>) {
        let stamp = self.where_stamp();
        let same = self.wh.root.as_ref() == Some(&inst.root);
        if same && stamp == self.wh.stamp {
            return;
        }
        if !same {
            self.wh = WhereState::default();
        }
        self.wh.root = Some(inst.root.clone());
        self.wh.stamp = stamp;
        let dirs = self.where_dirs();
        let class = self.class;
        let root = inst.root.clone();
        let header = inst.clone_header();
        cx.spawn(async move |this, cx| {
            let read = cx
                .background_spawn(async move {
                    dirs.into_iter().map(|(k, d)| (k, pack_files(&d))).collect::<HashMap<_, _>>()
                })
                .await;
            let all: Vec<PackFile> = read.values().flatten().cloned().collect();
            let ok = this
                .update(cx, |this, cx| {
                    if this.wh.root.as_ref() != Some(&root) {
                        return false;
                    }
                    this.wh.packs = read;
                    this.apply_where(cx);
                    true
                })
                .unwrap_or(false);
            if !ok || all.is_empty() {
                return;
            }
            let a2 = all.clone();
            let urls = cx.background_spawn(async move { crate::pack_lookup::look_up(&a2, class) }).await;
            this.update(cx, |this, cx| {
                this.store.update(cx, |s, _| urls.iter().for_each(|u| s.want_image(u)));
                this.apply_where(cx);
            })
            .ok();
            if cx.background_spawn(async move { crate::pack_lookup::check_versions(&all, &header) }).await {
                this.update(cx, |this, cx| this.apply_where(cx)).ok();
            }
        })
        .detach();
    }

    /// The table as one list: the instance's packs (tracked and not), then
    /// one row for each pack only other places hold, from the copy in
    /// effect. Each row's group comes from `pack_where::list`.
    pub(crate) fn apply_where(&mut self, cx: &mut Context<Self>) {
        let class = self.class;
        let Some(real) = self.root.as_ref().and_then(|r| self.store.read(cx).instance(r)).cloned() else {
            return;
        };
        let entries = pw::list(&real, class, &self.locs);
        let rp = class == cf::CLASS_RESOURCE_PACKS;
        // Resource packs: the instance's own (tracked, then the untracked
        // ones read from its folder); data packs live only in folders.
        let mut inst = if rp { real.clone() } else { real.clone_header() };
        let mut updates = if rp { self.updates(&inst, cx) } else { HashMap::new() };
        let mut loose = HashSet::new();
        if rp && let (Some(dir), Some((d, _, packs))) = (self.own_dir(), self.loose_packs.as_ref()) && *d == dir {
            let tracked: HashSet<String> =
                inst.addons.iter().filter(|a| in_class(a, class)).map(|a| a.file_name.to_lowercase()).collect();
            for p in packs {
                // A copy of one the modpack put in Paxi is the pack's too.
                let shipped = entries.iter().any(|e| e.from_pack && e.file.eq_ignore_ascii_case(&p.path.file_name().unwrap_or_default().to_string_lossy().trim_end_matches(".disabled")));
                let (a, u) = pack_addon(p, class, shipped);
                let key = a.file_name.to_lowercase();
                if tracked.contains(&key) {
                    continue;
                }
                if let Some(u) = u {
                    updates.insert(u.project_id, u);
                }
                loose.insert(key);
                inst.addons.push(a);
            }
        }
        let have: HashSet<String> = inst
            .addons
            .iter()
            .filter(|a| in_class(a, class))
            .map(|a| a.file_name.trim_end_matches(".disabled").to_lowercase())
            .collect();
        let mut rep = HashMap::new();
        for e in &entries {
            let k = e.file.to_lowercase();
            if have.contains(&k) {
                continue;
            }
            // The copy in effect, else one that's on, else any.
            let copy = match &e.group {
                Group::Global(key) => e.copy_in(key),
                _ => None,
            }
            .or_else(|| e.copies.iter().find(|c| c.on))
            .or(e.copies.first());
            let Some(copy) = copy else { continue };
            let p = self
                .wh
                .packs
                .get(&copy.key)
                .and_then(|v| v.iter().find(|p| p.path == copy.path))
                .cloned()
                .unwrap_or_else(|| PackFile { path: copy.path.clone(), name: e.file.clone(), ..Default::default() });
            let (mut a, u) = pack_addon(&p, class, e.from_pack);
            a.file_name = e.file.clone();
            if let Some(u) = u {
                updates.insert(u.project_id, u);
            }
            if let Some(dir) = copy.path.parent() {
                rep.insert(k, dir.to_path_buf());
            }
            inst.addons.push(a);
        }
        self.loose_set = loose;
        self.wh.entries = Rc::new(entries.into_iter().map(|e| (e.file.to_lowercase(), e)).collect());
        self.wh.rep = Rc::new(rep);
        self.table.inst = Some(Rc::new(inst));
        self.table.updates = Rc::new(updates);
        self.table.active_rp = Default::default();
        self.table.also = Rc::default();
        self.table.rows_for = None;
        self.ensure_rows();
        self.want_images(cx);
        cx.notify();
    }

    /// The groups in the order shown: resource packs In game, each loader,
    /// Off; data packs each loader, In worlds, Off.
    fn where_groups(&self) -> Vec<Group> {
        let loaders = self
            .locs
            .iter()
            .filter(|l| l.key != pw::INSTANCE && !pw::is_world(&l.key))
            .map(|l| Group::Global(l.key.clone()));
        if self.class == cf::CLASS_RESOURCE_PACKS {
            std::iter::once(Group::InGame).chain(loaders).chain([Group::Off]).collect()
        } else {
            let worlds = self.locs.iter().any(|l| pw::is_world(&l.key));
            loaders.chain(worlds.then_some(Group::InWorlds)).chain([Group::Off]).collect()
        }
    }

    /// A row's group, by its addon.
    pub(crate) fn where_group(&self, file: &str) -> Group {
        self.where_entry(file).map_or(Group::Off, |e| e.group.clone())
    }

    /// The rows under group headings; In game and Paxi in their order.
    /// With no search or filter on, a group with nothing in it still shows
    /// its heading so packs can be dragged to it.
    pub(crate) fn where_rows(&self, inst: &Instance, ixs: Vec<usize>, all: bool) -> Vec<Row> {
        let mut out = Vec::new();
        for g in self.where_groups() {
            let mut v: Vec<usize> =
                ixs.iter().copied().filter(|i| self.where_group(&inst.addons[*i].file_name) == g).collect();
            if v.is_empty() && !all {
                continue;
            }
            let ranked = matches!(&g, Group::InGame) || matches!(&g, Group::Global(k) if k == pw::PAXI);
            if ranked {
                v.sort_by_key(|i| self.where_entry(&inst.addons[*i].file_name).and_then(|e| e.rank).unwrap_or(usize::MAX));
            }
            out.push(Row::Group(g, v.len()));
            out.extend(v.into_iter().map(Row::Addon));
        }
        out
    }

    /// Each ranked row's place in its group, by addon index.
    pub(crate) fn where_ranks(&self) -> HashMap<usize, usize> {
        let Some(inst) = self.table.inst.as_ref() else { return HashMap::new() };
        inst.addons
            .iter()
            .enumerate()
            .filter(|(_, a)| in_class(a, self.class))
            .filter_map(|(i, a)| Some((i, self.where_entry(&a.file_name)?.rank?)))
            .collect()
    }

    // ---- names and looks ----------------------------------------------

    fn loc_label(&self, key: &str) -> String {
        self.locs.iter().find(|l| l.key == key).map_or_else(|| key.to_string(), |l| loader_name(&l.key, &l.label))
    }

    /// A group's heading and the line beside it.
    fn group_title(&self, g: &Group) -> (String, &'static str) {
        let dp = self.class == cf::CLASS_DATAPACKS;
        match g {
            Group::InGame => ("In game".into(), "Top wins. Drag to reorder."),
            Group::InWorlds => (
                "In worlds".into(),
                "Copied into each chosen world's datapacks folder. Order is per world, set in game.",
            ),
            Group::Global(k) if k == pw::PAXI => (
                "Always on · Paxi".into(),
                if dp {
                    "Every world, new ones too, in this load order."
                } else {
                    "Every world, in this load order. Later overrides earlier."
                },
            ),
            Group::Global(k) if k == "paxi-optional" => {
                ("Paxi optional".into(), "Paxi offers these when a new world is made.")
            }
            Group::Global(k) => (format!("Always on · {}", self.loc_label(k)), "Every world."),
            Group::Off => ("Off".into(), "Drag up, or use the pill, to turn on."),
        }
    }

    /// The pill's words for a group.
    fn pill_label(&self, g: &Group) -> String {
        match g {
            Group::InGame => "In game".into(),
            Group::InWorlds => "In worlds".into(),
            Group::Global(k) if k == pw::PAXI => "Always on".into(),
            Group::Global(k) => format!("Always on · {}", self.loc_label(k)),
            Group::Off => "Off".into(),
        }
    }

    fn group_color(g: &Group, cx: &App) -> Hsla {
        let t = cx.theme();
        match g {
            Group::InGame => t.primary,
            Group::Global(_) => t.info,
            Group::InWorlds => t.success,
            Group::Off => t.muted_foreground,
        }
    }

    /// A group's folder, for Open folder.
    fn group_dir(&self, g: &Group) -> Option<PathBuf> {
        let key = match g {
            Group::InGame => pw::INSTANCE,
            Group::Global(k) => k.as_str(),
            _ => return None,
        };
        self.locs.iter().find(|l| l.key == key).map(|l| l.dir.clone())
    }

    /// Where a drop on group `g` sends a pack.
    fn group_target(&self, g: &Group, file: &str) -> Target {
        match g {
            Group::InGame => Target::InGame,
            Group::Global(k) => Target::Global(k.clone()),
            Group::Off => Target::Off,
            Group::InWorlds => {
                // The worlds it's in already, else every world.
                let had: Vec<String> = self
                    .where_entry(file)
                    .map(|e| e.copies.iter().filter(|c| pw::is_world(&c.key)).map(|c| c.key.clone()).collect())
                    .unwrap_or_default();
                Target::Worlds(if had.is_empty() { self.world_keys() } else { had })
            }
        }
    }

    fn world_keys(&self) -> Vec<String> {
        self.locs.iter().filter(|l| pw::is_world(&l.key)).map(|l| l.key.clone()).collect()
    }

    /// What a drop on group `g` will do, for its heading while dragging.
    fn drop_hint(&self, g: &Group, name: &str) -> String {
        match g {
            Group::InGame => format!("Drop here: {name} goes on top in game"),
            Group::Global(k) => {
                let rel = self
                    .group_dir(g)
                    .zip(self.root.as_ref())
                    .map(|(d, r)| d.strip_prefix(r).unwrap_or(&d).to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                format!("Drop here: Hopper copies {name} into {rel}. It's on in every world ({}).", self.loc_label(k))
            }
            Group::InWorlds => format!("Drop here: {name} goes into the worlds it was in, else every world"),
            Group::Off => format!("Drop here: {name} stays, switched off"),
        }
    }

    /// A group's heading row: its name, count, what it means, Open folder.
    /// While a pack is dragged from another group, it says what dropping
    /// here does, with a dashed outline.
    pub(crate) fn group_row(&self, g: &Group, n: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let color = Self::group_color(g, cx);
        let (title, hint) = self.group_title(g);
        let running = self.class == cf::CLASS_RESOURCE_PACKS && self.where_running(cx);
        let dragging = self.wh.dragging.clone().filter(|(_, from)| from != g);
        let drop_name = dragging.as_ref().map(|(f, _)| {
            self.table
                .inst
                .as_ref()
                .and_then(|i| i.addon_by_file(f))
                .map_or_else(|| f.clone(), |a| a.name.clone())
        });
        let hint: SharedString = match &drop_name {
            Some(name) => self.drop_hint(g, name).into(),
            None if running => RUNNING_TIP.into(),
            None => hint.into(),
        };
        let dir = self.group_dir(g);
        let gid = match g {
            Group::Global(k) => format!("group-{k}"),
            other => format!("group-{other:?}"),
        };
        let target_g = g.clone();
        let over_bg = theme.info.opacity(0.14);
        h_flex()
            .id(ElementId::Name(gid.clone().into()))
            .h(rems(ROW_H))
            .w_full()
            .px_3()
            .gap_2()
            .items_center()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.transparent)
            .when(dragging.is_some(), |r| r.border_dashed().border_color(theme.info.opacity(0.5)).bg(theme.info.opacity(0.06)))
            .drag_over::<WhereDrag>(move |s, _, _, _| s.bg(over_bg))
            .on_drop(cx.listener(move |this, d: &WhereDrag, window, cx| {
                let to = this.group_target(&target_g, &d.file);
                this.wh.dragging = None;
                this.set_where(vec![d.file.clone()], to, Some(0), window, cx)
            }))
            .child(div().text_sm().font_semibold().child(title))
            .child(widgets::chip(color).child(n.to_string()))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(if drop_name.is_some() {
                        theme.info
                    } else if running {
                        theme.warning
                    } else {
                        theme.muted_foreground
                    })
                    .child(hint),
            )
            .child(div().flex_1())
            .when_some(dir, |r, dir| {
                r.child(
                    Button::new(ElementId::Name(format!("{gid}-folder").into()))
                        .link()
                        .xsmall()
                        .label("Open folder")
                        .on_click(move |_, _, _| crate::store::open_folder(&dir)),
                )
            })
            .into_any_element()
    }

    /// Chips by a pack's name: its copies, the modpack's, its worlds.
    pub(crate) fn where_chips(&self, file: &str, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(e) = self.where_entry(file).cloned() else { return Vec::new() };
        let theme = cx.theme().clone();
        let id = |p: &str| ElementId::Name(format!("{p}:{file}").into());
        let mut out = Vec::new();
        if e.copies.len() > 1 {
            let n = e.copies.len();
            let text = match &e.using {
                Some(u) => format!("{n} copies, using {u}'s"),
                None => format!("{n} copies"),
            };
            let tip: SharedString = e
                .copies
                .iter()
                .map(|c| format!("{}{}", c.label, if c.on { "" } else { " (off)" }))
                .collect::<Vec<_>>()
                .join(", ")
                .into();
            out.push(
                div()
                    .id(id("copies"))
                    .flex_none()
                    .child(widgets::chip(theme.muted_foreground).child(text))
                    .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
                    .into_any_element(),
            );
        }
        if e.from_pack {
            out.push(
                div()
                    .id(id("from-pack"))
                    .flex_none()
                    .child(widgets::chip(theme.muted_foreground).child("from pack"))
                    .tooltip(|w, cx| {
                        gpui_kit::component::tooltip::Tooltip::new("Came with the modpack")
                            .build(w, cx)
                    })
                    .into_any_element(),
            );
        }
        if e.group == Group::InWorlds {
            let n = e.worlds.len();
            let names: SharedString =
                e.worlds.iter().map(|k| self.loc_label(k)).collect::<Vec<_>>().join(", ").into();
            out.push(
                div()
                    .id(id("worlds"))
                    .flex_none()
                    .child(widgets::chip(theme.success).child(if n == 1 { "1 world".to_string() } else { format!("{n} worlds") }))
                    .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(names.clone()).build(w, cx))
                    .into_any_element(),
            );
        }
        out
    }

    /// The Where pill: a tinted button whose menu picks Off, In game (or
    /// the worlds), or a loader.
    pub(crate) fn where_pill(&self, file: &str, cx: &mut Context<Self>) -> AnyElement {
        let Some(e) = self.where_entry(file).cloned() else { return div().into_any_element() };
        let theme = cx.theme().clone();
        let color = Self::group_color(&e.group, cx);
        let (bg, hover) = if e.group == Group::Off {
            (theme.secondary, theme.secondary_hover)
        } else {
            (color.opacity(widgets::alpha::TINT), color.opacity(widgets::alpha::TINT * 2.))
        };
        let variant = ButtonCustomVariant::new(cx).color(bg).foreground(color).hover(hover).active(hover);
        let running = self.class == cf::CLASS_RESOURCE_PACKS && self.where_running(cx);
        let view = cx.entity();
        let dp = self.class == cf::CLASS_DATAPACKS;
        // (target, label, what it does)
        let mut choices: Vec<(Target, String, String)> = vec![(Target::Off, "Off".into(), "Kept, but not loaded".into())];
        if !dp {
            choices.push((Target::InGame, "In game".into(), "Your own packs, top wins".into()));
        }
        for l in self.locs.iter().filter(|l| l.key != pw::INSTANCE && !pw::is_world(&l.key)) {
            let name = loader_name(&l.key, &l.label);
            let rel = self
                .root
                .as_ref()
                .map(|r| l.dir.strip_prefix(r).unwrap_or(&l.dir).to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            let label = if l.key == "paxi-optional" { name } else { format!("Always on ({name})") };
            choices.push((Target::Global(l.key.clone()), label, format!("Every world · {rel}")));
        }
        let worlds: Vec<(String, String)> = self
            .locs
            .iter()
            .filter(|l| pw::is_world(&l.key))
            .map(|l| (l.key.clone(), l.label.clone()))
            .collect();
        let now = e.target();
        let f = file.to_string();
        Button::new(ElementId::Name(format!("where:{file}").into()))
            .small()
            .custom(variant)
            .w(rems(PILL_W))
            .label(self.pill_label(&e.group))
            .dropdown_caret(true)
            .disabled(running)
            .tooltip(if running { RUNNING_TIP } else { "Where this pack is loaded" })
            .dropdown_menu(move |menu: PopupMenu, _, _| {
                let mut menu = menu.label("Where");
                for (to, label, note) in choices.clone() {
                    let (v, f) = (view.clone(), f.clone());
                    let checked = match (&to, &now) {
                        (Target::Worlds(_), _) => false,
                        (a, b) => a == b,
                    };
                    menu = menu.item(
                        PopupMenuItem::element(move |_, cx| {
                            v_flex()
                                .py_0p5()
                                .child(div().text_sm().child(label.clone()))
                                .child(div().text_xs().text_color(cx.theme().muted_foreground).child(note.clone()))
                        })
                        .checked(checked)
                        .on_click(move |_, window, cx| {
                            v.update(cx, |this, cx| this.set_where(vec![f.clone()], to.clone(), None, window, cx))
                        }),
                    );
                }
                if !worlds.is_empty() {
                    let on: Vec<String> = match &now {
                        Target::Worlds(w) => w.clone(),
                        _ => Vec::new(),
                    };
                    menu = menu.separator().label("In worlds (pick which)");
                    for (key, label) in worlds.clone() {
                        let (v, f) = (view.clone(), f.clone());
                        let has = on.contains(&key);
                        let mut next: Vec<String> = on.iter().filter(|k| **k != key).cloned().collect();
                        if !has {
                            next.push(key.clone());
                        }
                        let to = if next.is_empty() { Target::Off } else { Target::Worlds(next) };
                        menu = menu.item(PopupMenuItem::new(label).checked(has).on_click(move |_, window, cx| {
                            v.update(cx, |this, cx| this.set_where(vec![f.clone()], to.clone(), None, window, cx))
                        }));
                    }
                    if worlds.len() > 1 {
                        let (v, f) = (view.clone(), f.clone());
                        let all: Vec<String> = worlds.iter().map(|(k, _)| k.clone()).collect();
                        menu = menu.item(PopupMenuItem::new("Every world").on_click(move |_, window, cx| {
                            v.update(cx, |this, cx| {
                                this.set_where(vec![f.clone()], Target::Worlds(all.clone()), None, window, cx)
                            })
                        }));
                    }
                    menu = menu.label("New worlds: use Always on (Paxi)");
                }
                menu
            })
            .into_any_element()
    }

    /// The grip (drag to another place or group), the place in a ranked
    /// group, and ↑↓ on hover there.
    pub(crate) fn where_order_cell(&self, file: &str, name: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let e = self.where_entry(file).cloned();
        let group = e.as_ref().map_or(Group::Off, |e| e.group.clone());
        let rank = e.as_ref().and_then(|e| e.rank);
        let running = self.class == cf::CLASS_RESOURCE_PACKS && self.where_running(cx);
        let id = |p: &str| ElementId::Name(format!("{p}:{file}").into());
        let drag = WhereDrag { file: file.to_string(), name: name.to_string().into() };
        let view = cx.entity();
        let (f, g) = (file.to_string(), group.clone());
        let tint = Self::group_color(&group, cx);
        let in_group = rank.map(|_| self.group_len(&group)).unwrap_or(0);
        let (f_up, f_down) = (file.to_string(), file.to_string());
        crate::datapacks::col_order()
            .child(
                div()
                    .id(id("grip"))
                    .flex_none()
                    .text_color(theme.muted_foreground)
                    .when(!running, |d| {
                        d.cursor_grab().on_drag(drag, move |d, _, _, cx| {
                            let (f, g) = (f.clone(), g.clone());
                            view.update(cx, |this, cx| {
                                this.wh.dragging = Some((f, g));
                                cx.notify();
                            });
                            cx.new(|_| d.clone())
                        })
                    })
                    .tooltip(move |w, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(if running {
                            RUNNING_TIP
                        } else {
                            "Drag to reorder, or to another group"
                        })
                        .build(w, cx)
                    })
                    .child(Icon::new(IconName::GripVertical).small()),
            )
            .when_some(rank, |c, n| {
                c.child(
                    div()
                        .size_5()
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(tint.opacity(widgets::alpha::TINT))
                        .text_color(tint)
                        .text_xs()
                        .font_bold()
                        .child(n.to_string()),
                )
                .child(
                    h_flex()
                        .invisible()
                        .group_hover(crate::addons::ROW_GROUP, |s| s.visible())
                        .child(
                            Button::new(id("up"))
                                .ghost()
                                .xsmall()
                                .icon(IconName::ChevronUp)
                                .tooltip("Move up")
                                .disabled(running || n == 1)
                                .on_click(cx.listener(move |this, _, window, cx| this.where_step(&f_up, -1, window, cx))),
                        )
                        .child(
                            Button::new(id("down"))
                                .ghost()
                                .xsmall()
                                .icon(IconName::ChevronDown)
                                .tooltip("Move down")
                                .disabled(running || n >= in_group)
                                .on_click(cx.listener(move |this, _, window, cx| this.where_step(&f_down, 1, window, cx))),
                        ),
                )
            })
            .into_any_element()
    }

    fn group_len(&self, g: &Group) -> usize {
        self.wh.entries.values().filter(|e| e.group == *g).count()
    }

    /// A row takes drops: from its own group, to take its place; from
    /// another, to join its group there.
    pub(crate) fn where_drop<E: InteractiveElement + Styled>(&self, row: E, file: &str, cx: &mut Context<Self>) -> E {
        let tint = cx.theme().info.opacity(widgets::alpha::TINT);
        let target = file.to_string();
        row.drag_over::<WhereDrag>(move |s, _, _, _| s.bg(tint))
            .on_drop(cx.listener(move |this, d: &WhereDrag, window, cx| {
                this.wh.dragging = None;
                this.where_dropped(&d.file, &target, window, cx)
            }))
    }

    fn where_dropped(&mut self, dragged: &str, target: &str, window: &mut Window, cx: &mut Context<Self>) {
        if dragged.eq_ignore_ascii_case(target) {
            return cx.notify();
        }
        let (Some(d), Some(t)) = (self.where_entry(dragged).cloned(), self.where_entry(target).cloned()) else { return };
        let at = t.rank.map(|r| r - 1);
        if d.group == t.group {
            if let Some(at) = at {
                self.where_place(dragged, &d.group, at, window, cx);
            }
            return;
        }
        let to = self.group_target(&t.group, dragged);
        self.set_where(vec![dragged.to_string()], to, at, window, cx)
    }

    /// The files of a ranked group, in its order.
    fn ranked_files(&self, g: &Group) -> Vec<String> {
        let mut v: Vec<&Entry> = self.wh.entries.values().filter(|e| e.group == *g && e.rank.is_some()).collect();
        v.sort_by_key(|e| e.rank);
        v.into_iter().map(|e| e.file.clone()).collect()
    }

    /// Moves a pack one place up or down in its group.
    fn where_step(&mut self, file: &str, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(e) = self.where_entry(file).cloned() else { return };
        let Some(r) = e.rank else { return };
        let at = (r as isize - 1 + step).max(0) as usize;
        self.where_place(file, &e.group, at, window, cx)
    }

    /// Puts a pack at `at` (0-based) in its ranked group: the game's order
    /// for In game, Paxi's load order file for Paxi. Undo puts it back.
    fn where_place(&mut self, file: &str, g: &Group, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        let before = self.ranked_files(g);
        let after = pw::reorder(&before, file, at);
        if after == before {
            return;
        }
        let msg = format!("Moved {}", self.describe_packs(&[file.to_string()]));
        match g {
            Group::InGame => {
                if self.where_running(cx) {
                    window.push_notification(Notification::info(RUNNING_TIP), cx);
                    return;
                }
                // Packs the game lists that aren't shown keep their place.
                let mut order: Vec<String> = (*self.rp_order).clone();
                let shown: HashSet<String> = before.iter().map(|f| f.to_lowercase()).collect();
                let mut it = after.into_iter();
                for e in order.iter_mut() {
                    if shown.contains(&e.to_lowercase())
                        && let Some(n) = it.next()
                    {
                        *e = n;
                    }
                }
                self.write_order(order, Some(msg), window, cx);
            }
            Group::Global(k) if k == pw::PAXI => {
                self.write_paxi_order(after, Some((msg, before)), window, cx);
            }
            _ => {}
        }
    }

    fn write_paxi_order(
        &mut self,
        names: Vec<String>,
        undo: Option<(String, Vec<String>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let class = self.class;
        let r = self.root.as_ref().and_then(|root| self.store.read(cx).instance(root)).map(|inst| {
            // Packs listed before but not in the group now (switched off)
            // keep their entries, after the group's.
            let mut all = names.clone();
            for n in pw::paxi_order(inst, class).unwrap_or_default() {
                if !all.iter().any(|x| x.eq_ignore_ascii_case(&n)) {
                    all.push(n);
                }
            }
            pw::set_paxi_order(inst, class, &all)
        });
        match r {
            Some(Err(e)) => window.push_notification(Notification::error(format!("{e:#}")), cx),
            Some(Ok(())) => {
                self.resync(cx);
                if let Some((msg, before)) = undo {
                    self.undo_toast(
                        msg,
                        Rc::new(move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                            this.write_paxi_order(before.clone(), None, window, cx)
                        }),
                        window,
                        cx,
                    );
                }
            }
            None => {}
        }
    }

    pub(crate) fn where_running(&self, cx: &App) -> bool {
        self.root.as_ref().is_some_and(|r| self.store.read(cx).running(r).is_some())
    }

    /// The on target for Turn on (the switch keys, the bulk bar): back to
    /// the loader that has a copy, else In game, else the worlds it was in.
    fn on_target(&self, e: &Entry) -> Target {
        if let Some(c) = e.copies.iter().find(|c| c.key != pw::INSTANCE && !pw::is_world(&c.key)) {
            return Target::Global(c.key.clone());
        }
        if self.class == cf::CLASS_RESOURCE_PACKS {
            return Target::InGame;
        }
        self.group_target(&Group::InWorlds, &e.file)
    }

    /// Turn on / off (Space, the bulk bar).
    pub(crate) fn where_toggle(&mut self, files: Vec<String>, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !on {
            return self.set_where(files, Target::Off, None, window, cx);
        }
        // Each to its own on place.
        let mut by: Vec<(Target, Vec<String>)> = Vec::new();
        for f in files {
            let Some(e) = self.where_entry(&f) else { continue };
            if e.group != Group::Off {
                continue;
            }
            let t = self.on_target(e);
            match by.iter_mut().find(|(x, _)| *x == t) {
                Some((_, v)) => v.push(f),
                None => by.push((t, vec![f])),
            }
        }
        for (t, files) in by {
            self.set_where(files, t, None, window, cx);
        }
    }

    /// Sends packs to `to` (`at`: a place in a ranked group). A pack the
    /// modpack put in Paxi asks first, the same for the pill and dragging.
    pub(crate) fn set_where(&mut self, files: Vec<String>, to: Target, at: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let entries: Vec<Entry> = files.iter().filter_map(|f| self.where_entry(f).cloned()).collect();
        let entries: Vec<Entry> = entries.into_iter().filter(|e| e.target() != to || at.is_some()).collect();
        if entries.is_empty() {
            return cx.notify();
        }
        if self.class == cf::CLASS_RESOURCE_PACKS && self.where_running(cx) {
            window.push_notification(Notification::info(RUNNING_TIP), cx);
            return cx.notify();
        }
        let leaving: Vec<String> = entries
            .iter()
            .filter(|e| e.from_pack && matches!(&e.group, Group::Global(k) if Target::Global(k.clone()) != to))
            .map(|e| self.describe_packs(std::slice::from_ref(&e.file)))
            .collect();
        if leaving.is_empty() {
            return self.move_where(entries, to, at, true, window, cx);
        }
        let view = cx.entity();
        let what = if leaving.len() == 1 { leaving[0].clone() } else { format!("{} packs", leaving.len()) };
        window.open_dialog(cx, move |dialog, window, _| {
            let (v, entries, to) = (view.clone(), entries.clone(), to.clone());
            dialog
                .title(format!("Move {what} out of Paxi?"))
                .w(rems(30.).to_pixels(window.rem_size()))
                .margin_top(rems(widgets::DIALOG_TOP).to_pixels(window.rem_size()))
                .child(div().text_sm().child(format!(
                    "{what} came with the modpack. Taking {} out of Paxi can undo textures or changes the pack relies on, and a pack update may put {} back.",
                    if leaving.len() == 1 { "it" } else { "them" },
                    if leaving.len() == 1 { "it" } else { "them" },
                )))
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().child(Button::new("where-keep").outline().label("Keep in Paxi")))
                        .child(DialogAction::new().child(Button::new("where-anyway").primary().label("Move anyway"))),
                )
                .on_ok(move |_, window, cx| {
                    v.update(cx, |this, cx| this.move_where(entries.clone(), to.clone(), at, true, window, cx));
                    true
                })
        });
    }

    /// Does the move: each pack's plan's files in the background, then the
    /// switches of tracked packs (so the instance's list agrees), the
    /// game's order and Paxi's. With `toast`, Undo sends each back.
    fn move_where(&mut self, entries: Vec<Entry>, to: Target, at: Option<usize>, toast: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let class = self.class;
        let locs = self.locs.clone();
        let tracked: HashSet<String> = self
            .store
            .read(cx)
            .instance(&root)
            .map(|i| i.addons.iter().filter(|a| in_class(a, class)).map(|a| a.file_name.to_lowercase()).collect())
            .unwrap_or_default();
        let own = locs.iter().find(|l| l.key == pw::INSTANCE).map(|l| l.dir.clone());
        let mut files_steps: Vec<Step> = Vec::new();
        let mut order_steps: Vec<Step> = Vec::new();
        // Tracked packs of the instance switch through the store: (file, on).
        let mut switch: Vec<(String, bool)> = Vec::new();
        for e in &entries {
            for s in pw::plan(e, class, &to, &locs) {
                match &s {
                    Step::OrderAdd { .. } | Step::OrderRemove { .. } => order_steps.push(s),
                    Step::Enable { path, on }
                        if own.as_deref().is_some_and(|d| path.parent() == Some(d))
                            && tracked.contains(&e.file.to_lowercase()) =>
                    {
                        switch.push((e.file.clone(), *on))
                    }
                    _ => files_steps.push(s),
                }
            }
        }
        let undo: Vec<(String, Target)> = entries.iter().map(|e| (e.file.clone(), e.target())).collect();
        let what = self.describe_packs(&entries.iter().map(|e| e.file.clone()).collect::<Vec<_>>());
        let place = match &to {
            Target::Off => "Off".to_string(),
            Target::InGame => "In game".to_string(),
            Target::Global(k) if k == pw::PAXI => "Always on (Paxi)".to_string(),
            Target::Global(k) => format!("Always on ({})", self.loc_label(k)),
            Target::Worlds(w) if w.len() == 1 => format!("In {}", self.loc_label(&w[0])),
            Target::Worlds(w) => format!("In {} worlds", w.len()),
        };
        let one = (entries.len() == 1).then(|| entries[0].file.clone());
        cx.spawn_in(window, async move |this, cx| {
            let r2 = root.clone();
            let r = cx.background_spawn(async move { pw::run_files(&r2, &files_steps) }).await;
            this.update_in(cx, |this, window, cx| {
                if let Err(e) = r {
                    window.push_notification(Notification::error(format!("{what}: {e:#}")), cx);
                    this.resync(cx);
                    return;
                }
                for on in [true, false] {
                    let fs: Vec<String> = switch.iter().filter(|(_, o)| *o == on).map(|(f, _)| f.clone()).collect();
                    if !fs.is_empty() {
                        this.set_enabled(fs, on, window, cx);
                    }
                }
                if !order_steps.is_empty() || (to == Target::InGame && at.is_some()) {
                    let mut order = pw::order_after(&this.rp_order, &order_steps);
                    if let (Target::InGame, Some(at), Some(f)) = (&to, at, &one) {
                        order = pw::reorder(&order, f, at);
                    }
                    this.write_order(order, None, window, cx);
                }
                if let (Target::Global(k), Some(at), Some(f)) = (&to, at, &one)
                    && k == pw::PAXI
                {
                    let mut names = this.ranked_files(&Group::Global(k.clone()));
                    names.retain(|n| !n.eq_ignore_ascii_case(f));
                    let names = pw::reorder(&names, f, at);
                    this.write_paxi_order(names, None, window, cx);
                }
                this.resync(cx);
                this.store.update(cx, |_, cx| cx.notify());
                if toast {
                    this.undo_toast(
                        format!("{what}: {place}"),
                        Rc::new(move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                            for (f, t) in undo.clone() {
                                if let Some(e) = this.where_entry(&f).cloned() {
                                    this.move_where(vec![e], t, None, false, window, cx);
                                }
                            }
                        }),
                        window,
                        cx,
                    );
                }
            })
            .ok();
        })
        .detach();
    }

    /// Remove: every copy outside the instance's own folder goes to
    /// backups; returns the files that also have one there, for the
    /// usual removal.
    pub(crate) fn where_remove(&mut self, files: Vec<String>, window: &mut Window, cx: &mut Context<Self>) -> Vec<String> {
        let Some(root) = self.root.clone() else { return files };
        let mut rest = Vec::new();
        let mut moved = 0;
        for f in &files {
            let Some(e) = self.where_entry(f).cloned() else {
                rest.push(f.clone());
                continue;
            };
            for c in e.copies.iter().filter(|c| c.key != pw::INSTANCE) {
                match hopper::sync::backup_into(&root, &c.path) {
                    Ok(_) => moved += 1,
                    Err(err) => window.push_notification(Notification::error(format!("{f}: {err:#}")), cx),
                }
            }
            if e.copy_in(pw::INSTANCE).is_some() {
                rest.push(f.clone());
            }
        }
        if moved > 0 && rest.is_empty() {
            let what = self.describe_packs(&files);
            window.push_notification(Notification::success(format!("Removed {what}. It's in ⋯ › Backups.")), cx);
        }
        self.selected.clear();
        self.resync(cx);
        rest
    }

    /// Updates for rows whose file is in another place: updated there, in
    /// place. Returns the rest.
    pub(crate) fn where_updates(&mut self, updates: Vec<hopper::install::Update>, cx: &mut Context<Self>) -> Vec<hopper::install::Update> {
        let Some(inst) = self.table.inst.clone() else { return updates };
        let mut by: HashMap<PathBuf, Vec<hopper::install::Update>> = HashMap::new();
        let mut rest = Vec::new();
        for u in updates {
            let dir = inst.addons.iter().find(|a| a.project_id == u.project_id).and_then(|a| self.rep_dir(&a.file_name));
            match dir {
                Some(d) => by.entry(d).or_default().push(u),
                None => rest.push(u),
            }
        }
        for (dir, ups) in by {
            self.update_global(dir, ups, cx);
        }
        rest
    }
}
