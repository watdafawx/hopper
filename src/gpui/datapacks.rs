//! The Packs sub-tabs on the addon table: where packs live (the location
//! row, the World picker, + Location), data pack folders, resource packs in
//! game order, the one shader choice, and copy or move between places.
//! Data packs used to have their own page; they use the Mods format now.

use crate::addons::{
    AddonsEvent, AddonsView, ROW_GROUP, ROW_H, Removed, Row, col_action, col_active, col_author, col_check, col_date,
    col_env, col_trash, find_backup, in_class, undo_remove,
};
use crate::addons_extra::is_hopper_file;
use crate::crash_help::callout;
use crate::store::open_path;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::radio::Radio;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::cf;
use hopper::instances::{Addon, Instance};
use hopper::packs::{self, Location};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Why order and the shader choice wait while the game runs.
const RUNNING_TIP: &str = "Close the game first: it rewrites options.txt when it exits";
const RUNNING_SHADER_TIP: &str = "Close the game first: it saves its shader choice when it exits";

/// The grip, place and ↑↓ of a resource pack in use (or its Use button).
pub(crate) fn col_order() -> Div {
    h_flex().w(rems(5.75)).flex_none().gap_1()
}

fn mtime(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// `dir` relative to the instance, with forward slashes.
fn rel_of(root: &Path, dir: &Path) -> String {
    dir.strip_prefix(root).unwrap_or(dir).to_string_lossy().replace('\\', "/")
}

/// An options.txt entry as a lower-case file name ("file/x.zip" → "x.zip").
fn norm(entry: &str) -> String {
    entry.strip_prefix("file/").unwrap_or(entry).trim_end_matches(".disabled").to_lowercase()
}

/// Where packs of `class` can be, without Hopper's own folders.
fn places(inst: &Instance, class: u32) -> Vec<Location> {
    let mut v = packs::locations(inst, class);
    v.retain(|l| !is_hopper_file(&l.dir) && l.world.as_ref().is_none_or(|w| !is_hopper_file(w)));
    v
}

/// What a location's folder does, for the line under the row.
fn what_it_does(l: &Location) -> &'static str {
    if l.key == "instance" {
        "Your own packs: the game uses the ones in use, top wins"
    } else if l.dir.ends_with("optional_data") {
        "Optional packs Paxi offers when a world is made"
    } else if l.world.is_some() {
        "Loaded into this world only"
    } else if l.every_world {
        "Loaded into every world, always on"
    } else {
        "Loaded by the game"
    }
}

/// One line on the loader behind a location, for + Location.
fn loader_note(l: &Location) -> &'static str {
    let k = format!("{} {}", l.label, l.dir.display()).to_lowercase();
    if k.contains("optional") {
        "Paxi offers these when a new world is made"
    } else if k.contains("paxi") {
        "Paxi loads these into every world, always on"
    } else if k.contains("global") {
        "Global Packs loads required packs into every world"
    } else if k.contains("openloader") {
        "OpenLoader loads these into every world"
    } else {
        "Another folder the game loads packs from"
    }
}

/// Dragging a resource pack by its grip to another place in game order.
#[derive(Clone)]
pub(crate) struct OrderDrag {
    file: String,
    name: SharedString,
}

impl Render for OrderDrag {
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

/// "Copy to" and "Move to" with each other place, in a row's menu.
pub(crate) fn copy_move_items(
    menu: PopupMenu,
    view: &Entity<AddonsView>,
    file: &str,
    places: &[(String, PathBuf)],
) -> PopupMenu {
    if places.is_empty() {
        return menu;
    }
    let mut menu = menu.separator().label("Copy to");
    for (label, dir) in places {
        let (v, f, d) = (view.clone(), file.to_string(), dir.clone());
        menu = menu.item(PopupMenuItem::new(label.clone()).icon(IconName::Copy).on_click(move |_, window, cx| {
            v.update(cx, |this, cx| this.copy_move(f.clone(), d.clone(), false, window, cx))
        }));
    }
    menu = menu.label("Move to");
    for (label, dir) in places {
        let (v, f, d) = (view.clone(), file.to_string(), dir.clone());
        menu = menu.item(PopupMenuItem::new(label.clone()).icon(IconName::ArrowRightLeft).on_click(
            move |_, window, cx| v.update(cx, |this, cx| this.copy_move(f.clone(), d.clone(), true, window, cx)),
        ));
    }
    menu
}

impl AddonsView {
    // ---- locations -------------------------------------------------------

    /// Reads where this class's packs can be, the game's resource pack
    /// order, and for data packs which folder is shown (then its packs).
    pub(crate) fn load_locations(&mut self, inst: &Instance, cx: &mut Context<Self>) {
        let class = self.class;
        self.locs = if matches!(class, cf::CLASS_RESOURCE_PACKS | cf::CLASS_DATAPACKS) {
            places(inst, class)
        } else {
            Vec::new()
        };
        if class == cf::CLASS_RESOURCE_PACKS {
            self.rp_order = Rc::new(packs::resourcepack_order(inst));
        }
        if class != cf::CLASS_DATAPACKS {
            return;
        }
        let ok = self.folder.as_ref().is_some_and(|f| self.locs.iter().any(|l| l.dir == inst.root.join(f)));
        if !ok {
            // The first place with packs, else the first.
            let first = self.locs.iter().find(|l| l.count > 0).or(self.locs.first());
            self.folder = first.map(|l| rel_of(&inst.root, &l.dir));
            self.folder_packs = None;
            self.selected.clear();
        }
        self.folder_stamp = (self.global_dir().and_then(|d| mtime(&d)), mtime(&inst.root.join("saves")));
        self.read_folder(cx);
    }

    /// Whether the data pack folder shown, or the worlds, changed on disk
    /// since they were read (files dropped, installs from Browse).
    pub(crate) fn folder_moved(&mut self, root: &Path) -> bool {
        // The instance's own folder: packs added by hand or copied in.
        if let Some(dir) = self.own_dir() {
            return self.loose_packs.as_ref().is_none_or(|(d, t, _)| *d != dir || *t != mtime(&dir));
        }
        if self.class != cf::CLASS_DATAPACKS {
            return false;
        }
        let now = (self.global_dir().and_then(|d| mtime(&d)), mtime(&root.join("saves")));
        now != self.folder_stamp
    }

    /// The location shown.
    pub(crate) fn current_loc(&self) -> Option<&Location> {
        match self.global_dir() {
            Some(d) => self.locs.iter().find(|l| l.dir == d),
            None => self.locs.iter().find(|l| l.key == "instance"),
        }
    }

    /// The other places a pack of this kind can go: (menu label, folder).
    pub(crate) fn other_places(&self) -> Vec<(String, PathBuf)> {
        if !matches!(self.class, cf::CLASS_RESOURCE_PACKS | cf::CLASS_DATAPACKS) {
            return Vec::new();
        }
        let cur = self.current_loc().map(|l| l.key.clone());
        let dp = self.class == cf::CLASS_DATAPACKS;
        self.locs
            .iter()
            .filter(|l| Some(&l.key) != cur.as_ref())
            .map(|l| {
                let label = if l.world.is_some() && !l.label.starts_with("World") {
                    format!("World · {}", l.label)
                } else if dp && l.every_world {
                    format!("Every world · {}", l.label)
                } else {
                    l.label.clone()
                };
                (label, l.dir.clone())
            })
            .collect()
    }

    /// Shows the location `key`.
    fn go_to(&mut self, key: &str, cx: &mut Context<Self>) {
        let (Some(root), Some(l)) = (self.root.clone(), self.locs.iter().find(|l| l.key == key)) else { return };
        if self.class == cf::CLASS_DATAPACKS {
            let rel = rel_of(&root, &l.dir);
            if self.folder.as_ref() != Some(&rel) {
                self.folder = Some(rel);
                self.folder_packs = None;
            }
        } else if l.key == "instance" {
            self.globals.picked = None;
        } else {
            let dir = l.dir.clone();
            self.globals.picked = self.globals.dirs.iter().find(|(d, _)| root.join(d) == dir).map(|(d, _)| *d);
        }
        self.selected.clear();
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.resync(cx);
    }

    /// "Also in: …" for each pack whose file is in other places too, as
    /// `packs::also_in` says, but each folder read once (on or off alike).
    pub(crate) fn fill_also(&mut self) {
        let mut out: HashMap<String, Vec<String>> = HashMap::new();
        let class = self.class;
        if matches!(class, cf::CLASS_RESOURCE_PACKS | cf::CLASS_DATAPACKS)
            && self.locs.len() > 1
            && let (Some(inst), Some(cur)) = (self.table.inst.clone(), self.current_loc().map(|l| l.key.clone()))
        {
            let here: std::collections::HashSet<String> = inst
                .addons
                .iter()
                .filter(|a| in_class(a, class))
                .map(|a| a.file_name.trim_end_matches(".disabled").to_lowercase())
                .collect();
            for l in self.locs.iter().filter(|l| l.key != cur && l.count > 0) {
                for e in std::fs::read_dir(&l.dir).into_iter().flatten().flatten() {
                    let n = e.file_name().to_string_lossy().trim_end_matches(".disabled").to_lowercase();
                    if here.contains(&n) {
                        out.entry(n).or_default().push(l.label.clone());
                    }
                }
            }
        }
        self.table.also = Rc::new(out.into_iter().map(|(k, v)| (k, v.join(", "))).collect());
    }

    /// Paxi optional · Global Packs … as one segmented control, worlds in
    /// a picker, and + Location for the empty ones.
    pub(crate) fn location_row(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dp = self.class == cf::CLASS_DATAPACKS;
        if !(dp || self.class == cf::CLASS_RESOURCE_PACKS) || self.locs.len() < if dp { 1 } else { 2 } {
            return None;
        }
        let cur = self.current_loc().map(|l| l.key.clone());
        let is_cur = |l: &Location| Some(&l.key) == cur.as_ref();
        // Empty places hide, but the one open and This instance stay.
        let segs: Vec<&Location> = self
            .locs
            .iter()
            .filter(|l| l.world.is_none() && (l.key == "instance" || l.count > 0 || is_cur(l)))
            .collect();
        let hidden: Vec<(String, String, &'static str)> = self
            .locs
            .iter()
            .filter(|l| l.world.is_none() && !segs.iter().any(|s| s.key == l.key))
            .map(|l| (l.key.clone(), l.label.clone(), loader_note(l)))
            .collect();
        let worlds: Vec<(String, String, usize)> =
            self.locs.iter().filter(|l| l.world.is_some()).map(|l| (l.key.clone(), l.label.clone(), l.count)).collect();
        let ix = segs.iter().position(|l| is_cur(l)).unwrap_or(usize::MAX);
        let keys: Vec<String> = segs.iter().map(|l| l.key.clone()).collect();
        let view = cx.entity();
        let world_now = self.locs.iter().find(|l| l.world.is_some() && is_cur(l));
        let picker = (!worlds.is_empty()).then(|| {
            // The world shown, else the one played last.
            let (label, n) = world_now
                .map(|l| (l.label.clone(), l.count))
                .unwrap_or_else(|| (worlds[0].1.clone(), worlds[0].2));
            let (v, cur) = (view.clone(), cur.clone());
            Button::new("loc-world")
                .small()
                .outline()
                .selected(world_now.is_some())
                .icon(IconName::Globe)
                .label(format!("World: {label} ({n})"))
                .dropdown_caret(true)
                .dropdown_menu(move |menu, _, _| {
                    let mut menu = menu.label("Worlds, last played first");
                    for (key, label, n) in &worlds {
                        let (v, k) = (v.clone(), key.clone());
                        menu = menu.item(
                            PopupMenuItem::new(format!("{label}  ·  {n}"))
                                .checked(Some(key) == cur.as_ref())
                                .on_click(move |_, _, cx| v.update(cx, |this, cx| this.go_to(&k, cx))),
                        );
                    }
                    menu
                })
        });
        let add = (!hidden.is_empty()).then(|| {
            let v = view.clone();
            Button::new("loc-add")
                .small()
                .ghost()
                .icon(IconName::Plus)
                .label("Location")
                .tooltip("Folders with no packs yet")
                .dropdown_menu(move |menu, _, _| {
                    let mut menu = menu;
                    for (key, label, note) in &hidden {
                        let (v, k, label, note) = (v.clone(), key.clone(), label.clone(), *note);
                        menu = menu.item(
                            PopupMenuItem::element(move |_, cx| {
                                v_flex()
                                    .py_0p5()
                                    .child(div().text_sm().child(label.clone()))
                                    .child(div().text_xs().text_color(cx.theme().muted_foreground).child(note))
                            })
                            .on_click(move |_, _, cx| v.update(cx, |this, cx| this.go_to(&k, cx))),
                        );
                    }
                    menu
                })
        });
        Some(
            h_flex()
                .px_6()
                .gap_2()
                .when(!segs.is_empty(), |r| {
                    r.child(
                        TabBar::new("pack-places")
                            .segmented()
                            .small()
                            .selected_index(ix)
                            .children(segs.iter().map(|l| Tab::new().label(format!("{} {}", l.label, l.count))))
                            .on_click(cx.listener(move |this, ix: &usize, _, cx| {
                                if let Some(k) = keys.get(*ix) {
                                    this.go_to(&k.clone(), cx)
                                }
                            })),
                    )
                })
                .children(picker)
                .children(add)
                .into_any_element(),
        )
    }

    /// Under the row: where the shown location loads, with Open folder;
    /// for data packs without a loader, Paxi's pitch.
    pub(crate) fn location_caption(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dp = self.class == cf::CLASS_DATAPACKS;
        if !(dp || self.class == cf::CLASS_RESOURCE_PACKS && self.locs.len() > 1) {
            return None;
        }
        let root = self.root.clone()?;
        let l = self.current_loc()?;
        let dir = l.dir.clone();
        let text = format!("{} · {}", what_it_does(l), rel_of(&root, &dir));
        let no_loader = dp && !self.locs.iter().any(|l| l.every_world);
        Some(
            v_flex()
                .px_6()
                .gap_2()
                .child(
                    h_flex()
                        .gap_2()
                        .child(widgets::caption(text, cx).text_xs().truncate())
                        .child(
                            Button::new("loc-folder")
                                .link()
                                .xsmall()
                                .label("Open folder")
                                .on_click(move |_, _, _| {
                                    let _ = std::fs::create_dir_all(&dir);
                                    open_path(&dir)
                                }),
                        ),
                )
                .when(no_loader, |c| c.child(self.paxi_callout(&root, cx)))
                .into_any_element(),
        )
    }

    /// "Paxi loads data packs into every world", with Find Paxi.
    fn paxi_callout(&self, root: &Path, cx: &mut Context<Self>) -> AnyElement {
        let root = root.to_path_buf();
        callout(cx.theme().info, cx)
            .child(
                h_flex()
                    .gap_3()
                    .child(div().flex_1().text_sm().child(
                        "Paxi loads data packs into every world, new ones included. Without it, each world has its own.",
                    ))
                    .child(
                        Button::new("dp-get-paxi")
                            .small()
                            .outline()
                            .icon(IconName::Search)
                            .label("Find Paxi")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(AddonsEvent::Find { root: root.clone(), text: "paxi".into() })
                            })),
                    ),
            )
            .into_any_element()
    }

    /// Data packs with no folder at all: no loader and no world yet.
    pub(crate) fn no_datapack_folders(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(root) = self.root.clone() else { return div().into_any_element() };
        v_flex()
            .size_full()
            .gap_3()
            .child(div().px_6().child(self.paxi_callout(&root, cx)))
            .child(
                v_flex().flex_1().items_center().justify_center().child(widgets::empty_state(
                    IconName::Database,
                    "No data pack folders",
                    Some("Install Paxi, or play once to make a world.".into()),
                    cx,
                )),
            )
            .into_any_element()
    }

    /// The empty table of a global folder or world: Add.
    pub(crate) fn location_empty(&self, cx: &mut Context<Self>) -> Option<Div> {
        let dir = self.global_dir()?;
        let root = self.root.clone()?;
        let class = self.class;
        let dest = rel_of(&root, &dir);
        let dp = class == cf::CLASS_DATAPACKS;
        Some(
            widgets::empty_state(
                IconName::Database,
                if dp { "No data packs here" } else { "No packs here" },
                Some("Add some, or drop .zip files on the window.".into()),
                cx,
            )
            .child(
                Button::new("loc-empty-add")
                    .small()
                    .primary()
                    .icon(IconName::Plus)
                    .label(if dp { "Add data packs" } else { "Add resource packs" })
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(AddonsEvent::AddTo { root: root.clone(), class, dest: dest.clone() })
                    })),
            ),
        )
    }

    // ---- copy and move -------------------------------------------------

    /// Where `file` (as listed) is on disk now.
    fn pack_path(&self, file: &str, cx: &App) -> Option<PathBuf> {
        if let Some(dir) = self.global_dir().or_else(|| self.own_dir().filter(|_| self.is_loose(file))) {
            return Some(Self::global_path(&dir, file));
        }
        let inst = self.store.read(cx).instance(self.root.as_ref()?)?;
        inst.addon_by_file(file).map(|a| inst.addon_path(a))
    }

    /// Copies or moves a pack to another place in the background, then
    /// offers Undo. Moving the instance's own pack backs it up with its
    /// entry, like Remove.
    pub(crate) fn copy_move(&mut self, file: String, dest: PathBuf, mv: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(root), Some(src)) = (self.root.clone(), self.pack_path(&file, cx)) else { return };
        // A tracked pack of the instance (an untracked one just moves).
        let from_instance = self.global_dir().is_none() && !self.is_loose(&file);
        let label = self
            .other_places()
            .into_iter()
            .find(|(_, d)| *d == dest)
            .map_or_else(|| rel_of(&root, &dest), |(l, _)| l);
        let what = self
            .table
            .inst
            .as_ref()
            .and_then(|i| i.addon_by_file(&file))
            .map_or(file.clone(), |a| a.name.clone());
        let addon: Option<Addon> = from_instance
            .then(|| self.store.read(cx).instance(&root).and_then(|i| i.addon_by_file(&file).cloned()))
            .flatten();
        let store = self.store.clone();
        cx.spawn_in(window, async move |this, cx| {
            let (s2, d2, r2) = (src.clone(), dest.clone(), root.clone());
            let r = cx
                .background_spawn(async move {
                    if mv && !from_instance { packs::move_pack(&r2, &s2, &d2) } else { packs::copy_pack(&s2, &d2) }
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                let new = match r {
                    Ok(p) => p,
                    Err(e) => {
                        window.push_notification(Notification::error(format!("{what}: {e:#}")), cx);
                        return;
                    }
                };
                // The instance's own copy goes to backups with its entry.
                let mut removed: Option<Removed> = None;
                if mv && from_instance {
                    let since = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs())
                        .saturating_sub(1);
                    let r = store.update(cx, |s, cx| {
                        let r = s.remove_files(&root, std::slice::from_ref(&file));
                        cx.notify();
                        r
                    });
                    if let Err(e) = r {
                        window.push_notification(Notification::error(format!("Copied, but {e}")), cx);
                    }
                    removed = find_backup(&root, &src, since).map(|backup| Removed { original: src.clone(), backup, addon });
                }
                let undone = Rc::new(std::cell::Cell::new(false));
                let view = cx.entity().downgrade();
                let (store2, root2) = (store.clone(), root.clone());
                let note = Notification::success(format!("{} {what} to {label}", if mv { "Moved" } else { "Copied" }))
                    .action(move |_, _, cx| {
                        let (undone, view, store, root, new, src, removed) = (
                            undone.clone(),
                            view.clone(),
                            store2.clone(),
                            root2.clone(),
                            new.clone(),
                            src.clone(),
                            removed.clone(),
                        );
                        Button::new("undo-copy-move").label("Undo").disabled(undone.get()).on_click(cx.listener(
                            move |note, _, window, cx| {
                                if !undone.replace(true) {
                                    let r = if mv && !from_instance {
                                        // Back where it was.
                                        let back = src.parent().map(Path::to_path_buf).unwrap_or_default();
                                        packs::move_pack(&root, &new, &back).map(|_| ())
                                    } else {
                                        // The copy goes to backups.
                                        hopper::sync::backup_into(&root, &new).map(|_| ())
                                    };
                                    match r {
                                        Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
                                        Ok(()) => {
                                            if let Some(it) = removed.clone() {
                                                undo_remove(&store, root.clone(), vec![it], window, cx);
                                            }
                                        }
                                    }
                                    view.update(cx, |this, cx| this.resync(cx)).ok();
                                }
                                note.dismiss(window, cx);
                            },
                        ))
                    })
                    .autohide(true);
                window.push_notification(note, cx);
                this.resync(cx);
                // Other views count packs too.
                this.store.update(cx, |_, cx| cx.notify());
            })
            .ok();
        })
        .detach();
    }

    // ---- resource pack order -------------------------------------------

    /// Resource packs of the instance itself: listed in game order.
    pub(crate) fn order_mode(&self) -> bool {
        self.class == cf::CLASS_RESOURCE_PACKS && self.global_dir().is_none()
    }

    fn game_running(&self, cx: &App) -> bool {
        self.root.as_ref().is_some_and(|r| self.store.read(cx).running(r).is_some())
    }

    /// Whether a row counts as on: in game order for the instance's
    /// resource packs, else not switched off.
    pub(crate) fn row_on(&self, a: &Addon) -> bool {
        if !self.order_mode() {
            return a.enabled;
        }
        let f = a.file_name.trim_end_matches(".disabled").to_lowercase();
        a.enabled && self.rp_order.iter().any(|e| norm(e) == f)
    }

    /// Each in-use pack's place in game order (1 = top), by addon index.
    pub(crate) fn order_ranks(&self) -> HashMap<usize, usize> {
        let (true, Some(inst)) = (self.order_mode(), self.table.inst.as_ref()) else { return HashMap::new() };
        let pos: HashMap<String, usize> = self.rp_order.iter().enumerate().map(|(i, e)| (norm(e), i)).collect();
        let mut used: Vec<(usize, usize)> = inst
            .addons
            .iter()
            .enumerate()
            .filter(|(_, a)| in_class(a, self.class) && a.enabled)
            .filter_map(|(i, a)| Some((*pos.get(&a.file_name.trim_end_matches(".disabled").to_lowercase())?, i)))
            .collect();
        used.sort_unstable();
        used.into_iter().enumerate().map(|(n, (_, i))| (i, n + 1)).collect()
    }

    /// The rows as "In game · top wins" in game order, then "Not in use".
    pub(crate) fn order_rows(&self, inst: &Instance, ixs: Vec<usize>) -> Vec<Row> {
        let pos: HashMap<String, usize> = self.rp_order.iter().enumerate().map(|(i, e)| (norm(e), i)).collect();
        let at = |i: usize| {
            let a = &inst.addons[i];
            a.enabled.then(|| pos.get(&a.file_name.trim_end_matches(".disabled").to_lowercase()).copied()).flatten()
        };
        let (mut used, rest): (Vec<usize>, Vec<usize>) = ixs.into_iter().partition(|i| at(*i).is_some());
        used.sort_by_key(|i| at(*i));
        let mut out = Vec::new();
        if !used.is_empty() {
            out.push(Row::Section("In game · top wins", "Higher packs win where two change the same thing"));
            out.extend(used.into_iter().map(Row::Addon));
        }
        if !rest.is_empty() {
            out.push(Row::Section("Not in use", "Use adds a pack at the top"));
            out.extend(rest.into_iter().map(Row::Addon));
        }
        out
    }

    /// An options.txt entry for `file`, written the way the list already is.
    fn order_entry(&self, file: &str) -> String {
        if self.rp_order.iter().any(|e| e.starts_with("file/")) { format!("file/{file}") } else { file.to_string() }
    }

    /// Puts packs in game use (at the top) or takes them out.
    pub(crate) fn set_in_use(&mut self, files: Vec<String>, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.game_running(cx) {
            window.push_notification(Notification::info(RUNNING_TIP), cx);
            cx.notify();
            return;
        }
        let mut order: Vec<String> = (*self.rp_order).clone();
        if on {
            // Switched-off files come back first.
            let off: Vec<String> = files
                .iter()
                .filter(|f| self.table.inst.as_ref().and_then(|i| i.addon_by_file(f)).is_some_and(|a| !a.enabled))
                .cloned()
                .collect();
            if !off.is_empty() {
                self.set_enabled(off, true, window, cx);
            }
            for f in files.iter().rev() {
                let k = f.to_lowercase();
                if !order.iter().any(|e| norm(e) == k) {
                    order.insert(0, self.order_entry(f));
                }
            }
        } else {
            let gone: Vec<String> = files.iter().map(|f| f.to_lowercase()).collect();
            order.retain(|e| !gone.contains(&norm(e)));
        }
        let what = self.describe_packs(&files);
        let msg = if on { format!("Using {what} in game") } else { format!("Stopped using {what}") };
        self.write_order(order, Some(msg), window, cx);
    }

    /// Lower-case files of the packs in use (shown rows).
    fn used_files(&self) -> Vec<String> {
        let Some(inst) = self.table.inst.as_ref() else { return Vec::new() };
        self.table
            .rank
            .keys()
            .filter_map(|i| inst.addons.get(*i))
            .map(|a| a.file_name.trim_end_matches(".disabled").to_lowercase())
            .collect()
    }

    /// Moves a pack one place up (`-1`) or down past the next shown pack.
    fn step_in_order(&mut self, file: &str, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let used = self.used_files();
        let mut order: Vec<String> = (*self.rp_order).clone();
        let k = file.to_lowercase();
        let Some(i) = order.iter().position(|e| norm(e) == k) else { return };
        let shown = |e: &String| used.contains(&norm(e));
        let to = if step < 0 {
            (0..i).rev().find(|j| shown(&order[*j]))
        } else {
            (i + 1..order.len()).find(|j| shown(&order[*j]))
        };
        let Some(to) = to else { return };
        let e = order.remove(i);
        order.insert(to, e);
        let msg = format!("Moved {} {}", self.describe_packs(&[file.to_string()]), if step < 0 { "up" } else { "down" });
        self.write_order(order, Some(msg), window, cx);
    }

    /// A pack dragged onto another: it takes that one's place.
    fn drop_in_order(&mut self, dragged: &str, target: &str, window: &mut Window, cx: &mut Context<Self>) {
        if dragged.eq_ignore_ascii_case(target) {
            return;
        }
        if self.game_running(cx) {
            window.push_notification(Notification::info(RUNNING_TIP), cx);
            return;
        }
        let mut order: Vec<String> = (*self.rp_order).clone();
        let (d, t) = (dragged.to_lowercase(), target.to_lowercase());
        let (Some(i), Some(ti)) = (order.iter().position(|e| norm(e) == d), order.iter().position(|e| norm(e) == t))
        else {
            return;
        };
        let e = order.remove(i);
        let t2 = order.iter().position(|e| norm(e) == t).unwrap_or(0);
        // Dragged down: after the target; up: before it.
        order.insert(if i < ti { t2 + 1 } else { t2 }, e);
        let msg = format!("Moved {}", self.describe_packs(&[dragged.to_string()]));
        self.write_order(order, Some(msg), window, cx);
    }

    /// Writes options.txt's resource pack list and shows it.
    /// With `msg`, a toast whose Undo writes the order before back.
    fn write_order(&mut self, order: Vec<String>, msg: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let before: Vec<String> = (*self.rp_order).clone();
        let r = self.store.read(cx).instance(&root).map(|inst| packs::set_resourcepack_order(inst, &order));
        match r {
            Some(Ok(())) => {
                self.rp_order = Rc::new(order);
                self.table.rows_for = None;
                self.ensure_rows();
                cx.notify();
                if let Some(msg) = msg {
                    self.undo_toast(
                        msg,
                        Rc::new(move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                            if this.game_running(cx) {
                                window.push_notification(Notification::info(RUNNING_TIP), cx);
                            } else {
                                this.write_order(before.clone(), None, window, cx)
                            }
                        }),
                        window,
                        cx,
                    );
                }
            }
            Some(Err(e)) => window.push_notification(Notification::error(format!("{e:#}")), cx),
            None => {}
        }
    }

    /// The grip, place and ↑↓ of a pack in use; Use for one that isn't.
    pub(crate) fn order_cell(&self, file: &str, name: &str, rank: Option<usize>, cx: &mut Context<Self>) -> AnyElement {
        let running = self.game_running(cx);
        let theme = cx.theme().clone();
        let id = |p: &str| ElementId::Name(format!("{p}:{file}").into());
        let tip = move |t: &'static str| if running { RUNNING_TIP } else { t };
        let f = file.to_string();
        let Some(n) = rank else {
            return col_order()
                .child(
                    Button::new(id("use"))
                        .xsmall()
                        .outline()
                        .label("Use")
                        .disabled(running)
                        .tooltip(tip("Use it in game, at the top"))
                        .on_click(cx.listener(move |this, _, window, cx| this.set_in_use(vec![f.clone()], true, window, cx))),
                )
                .into_any_element();
        };
        let last = n >= self.table.rank.len();
        let drag = OrderDrag { file: file.to_string(), name: name.to_string().into() };
        let (f_up, f_down) = (f.clone(), f);
        col_order()
            .child(
                div()
                    .id(id("grip"))
                    .flex_none()
                    .text_color(theme.muted_foreground)
                    .when(!running, |d| {
                        d.cursor_grab().on_drag(drag, |d, _, _, cx| cx.new(|_| d.clone()))
                    })
                    .tooltip(move |w, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(tip("Drag to reorder")).build(w, cx)
                    })
                    .child(Icon::new(IconName::GripVertical).small()),
            )
            .child(
                div()
                    .size_5()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(theme.primary.opacity(widgets::alpha::TINT))
                    .text_color(theme.primary)
                    .text_xs()
                    .font_bold()
                    .child(n.to_string()),
            )
            .child(
                h_flex()
                    .invisible()
                    .group_hover(ROW_GROUP, |s| s.visible())
                    .child(
                        Button::new(id("up"))
                            .ghost()
                            .xsmall()
                            .icon(IconName::ChevronUp)
                            .tooltip(tip("Move up"))
                            .disabled(running || n == 1)
                            .on_click(cx.listener(move |this, _, window, cx| this.step_in_order(&f_up, -1, window, cx))),
                    )
                    .child(
                        Button::new(id("down"))
                            .ghost()
                            .xsmall()
                            .icon(IconName::ChevronDown)
                            .tooltip(tip("Move down"))
                            .disabled(running || last)
                            .on_click(cx.listener(move |this, _, window, cx| this.step_in_order(&f_down, 1, window, cx))),
                    ),
            )
            .into_any_element()
    }

    /// A pack in use takes drops of another, to reorder.
    pub(crate) fn order_drop<E: InteractiveElement + Styled>(
        &self,
        row: E,
        file: &str,
        rank: Option<usize>,
        cx: &mut Context<Self>,
    ) -> E {
        if !self.order_mode() || rank.is_none() {
            return row;
        }
        let tint = cx.theme().primary.opacity(widgets::alpha::TINT);
        let target = file.to_string();
        row.drag_over::<OrderDrag>(move |s, _, _, _| s.bg(tint))
            .on_drop(cx.listener(move |this, d: &OrderDrag, window, cx| this.drop_in_order(&d.file, &target, window, cx)))
    }

    /// A group heading in the table.
    pub(crate) fn section_row(&self, title: &'static str, hint: &'static str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let running = title.starts_with("In game") && self.game_running(cx);
        h_flex()
            .h(rems(ROW_H))
            .w_full()
            .px_3()
            .pb_1p5()
            .gap_3()
            .items_end()
            .child(
                div()
                    .text_xs()
                    .font_semibold()
                    .text_color(theme.muted_foreground)
                    .child(title.to_uppercase()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(if running { theme.warning } else { theme.muted_foreground })
                    .child(if running { RUNNING_TIP } else { hint }),
            )
            .into_any_element()
    }

    // ---- shaders ---------------------------------------------------------

    /// Whether `file` is the shader pack Iris or Oculus has picked.
    pub(crate) fn is_active_shader(&self, file: &str) -> bool {
        let f = file.trim_end_matches(".disabled").to_lowercase();
        self.table
            .shader
            .as_ref()
            .map(|s| s.to_lowercase())
            .is_some_and(|s| s == f || s == f.trim_end_matches(".zip"))
    }

    /// The choice's radio, for a pack or (`None`) for Off.
    pub(crate) fn shader_radio(&self, id: ElementId, file: Option<String>, checked: bool, cx: &mut Context<Self>) -> AnyElement {
        let running = self.game_running(cx);
        let tip = if running {
            RUNNING_SHADER_TIP
        } else if file.is_none() {
            "Play without shaders"
        } else {
            "Use this shader pack (from the next launch)"
        };
        div()
            .id(ElementId::Name(format!("{id}-wrap").into()))
            .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip).build(w, cx))
            .child(
                Radio::new(id)
                    .checked(checked)
                    .disabled(running)
                    .on_click(cx.listener(move |this, _: &bool, window, cx| this.set_shader(file.clone(), window, cx))),
            )
            .into_any_element()
    }

    /// Picks the shader pack Iris or Oculus runs (`None`: off).
    fn set_shader(&mut self, file: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.pick_shader(file, true, window, cx)
    }

    /// With `toast`, offers Undo, which picks the one before again.
    fn pick_shader(&mut self, file: Option<String>, toast: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.game_running(cx) {
            window.push_notification(Notification::info(RUNNING_SHADER_TIP), cx);
            return;
        }
        let Some(root) = self.root.clone() else { return };
        // A switched-off pack is switched on to be used.
        if let Some(f) = &file
            && self.table.inst.as_ref().and_then(|i| i.addon_by_file(f)).is_some_and(|a| !a.enabled)
        {
            self.set_enabled(vec![f.clone()], true, window, cx);
        }
        let before = self.table.shader.clone();
        let r = self.store.read(cx).instance(&root).map(|inst| packs::set_active_shader(inst, file.as_deref()));
        match r {
            Some(Err(e)) => window.push_notification(Notification::error(format!("{e:#}")), cx),
            Some(Ok(())) if toast => {
                let msg = match &file {
                    Some(f) => format!("Shaders: {} (from the next launch)", self.describe_packs(std::slice::from_ref(f))),
                    None => "Shaders off (from the next launch)".to_string(),
                };
                self.undo_toast(
                    msg,
                    Rc::new(move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                        this.pick_shader(before.clone(), false, window, cx)
                    }),
                    window,
                    cx,
                );
            }
            _ => {}
        }
        self.resync(cx);
    }

    /// The "Off" choice above the shader packs, in the table's columns.
    pub(crate) fn shader_off_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let off = self.table.shader.is_none();
        let radio = self.shader_radio(ElementId::Name("shader-off".into()), None, off, cx);
        h_flex()
            .id("shader-off-row")
            .h(rems(ROW_H))
            .w_full()
            .pl_3()
            .pr_2()
            .gap_3()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.transparent)
            .when(off, |r| widgets::row_selected(r, cx))
            .when(!off, |r| r.hover(|s| widgets::row_hover(s, cx)))
            .child(col_check())
            .child(
                div()
                    .w_10()
                    .flex_none()
                    .flex()
                    .justify_center()
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(IconName::CircleOff).small()),
            )
            .child(
                self.name_cell()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_sm().font_medium().child("Off"))
                    .child(widgets::caption("Vanilla look, best FPS", cx).text_xs()),
            )
            .when(self.action_fits(), |r| r.child(col_action(self.compact)))
            .children(self.spacer())
            .when(self.cols.author, |r| r.child(col_author()))
            .when(self.cols.side, |r| r.child(col_env()))
            .child(col_active().child(radio))
            .when(self.cols.date, |r| r.child(col_date()))
            .child(col_trash())
            .into_any_element()
    }

    // ---- undo and untracked packs --------------------------------------

    /// A pack's name, or "N packs".
    fn describe_packs(&self, files: &[String]) -> String {
        match files {
            [one] => self.table.inst.as_ref().and_then(|i| i.addon_by_file(one)).map_or(one.clone(), |a| a.name.clone()),
            many => format!("{} packs", many.len()),
        }
    }

    /// A success toast with Undo, like the others (it hides by itself).
    fn undo_toast(
        &self,
        text: String,
        undo: Rc<dyn Fn(&mut Self, &mut Window, &mut Context<Self>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.entity().downgrade();
        let undone = Rc::new(std::cell::Cell::new(false));
        let note = Notification::success(text)
            .action(move |_, _, cx| {
                let (view, undo, undone) = (view.clone(), undo.clone(), undone.clone());
                Button::new("undo-pack-change").label("Undo").disabled(undone.get()).on_click(cx.listener(
                    move |note, _, window, cx| {
                        if !undone.replace(true) {
                            let undo = undo.clone();
                            view.update(cx, |this, cx| undo(this, window, cx)).ok();
                        }
                        note.dismiss(window, cx);
                    },
                ))
            })
            .autohide(true);
        window.push_notification(note, cx);
    }

    /// The instance's own folder for this class, when it's shown.
    pub(crate) fn own_dir(&self) -> Option<PathBuf> {
        let own = self.class == cf::CLASS_SHADERS || self.order_mode();
        own.then(|| self.root.as_ref().map(|r| r.join(hopper::instances::folder_for_class(self.class)))).flatten()
    }

    /// A row for a pack the instance doesn't track.
    pub(crate) fn is_loose(&self, file: &str) -> bool {
        !self.loose_set.is_empty() && self.loose_set.contains(&file.trim_end_matches(".disabled").to_lowercase())
    }

    /// Whether project `pid` is an untracked pack's.
    pub(crate) fn loose_pid(&self, pid: u64) -> bool {
        pid != 0
            && self
                .table
                .inst
                .as_ref()
                .is_some_and(|i| i.addons.iter().any(|a| a.project_id == pid && self.is_loose(&a.file_name)))
    }

    /// Lists the packs in the instance's folder it doesn't track, like
    /// Mods lists loose jars (marked Yours), looked up on CurseForge and
    /// Modrinth by hash for names, icons and updates.
    pub(crate) fn want_loose(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.own_dir() else {
            self.loose_set.clear();
            return;
        };
        let stamp = mtime(&dir);
        let fresh = self.loose_packs.as_ref().is_some_and(|(d, t, _)| *d == dir && *t == stamp);
        // Same folder: the last list shows until the new one is read.
        if self.loose_packs.as_ref().is_some_and(|(d, _, _)| *d == dir) {
            self.merge_loose(cx);
        } else {
            self.loose_set.clear();
        }
        if fresh {
            return;
        }
        // Marked as read now, so redraws meanwhile don't read it again.
        match self.loose_packs.as_mut() {
            Some((d, t, _)) if *d == dir => *t = stamp,
            _ => self.loose_packs = Some((dir.clone(), stamp, Vec::new())),
        }
        let class = self.class;
        let inst = self.root.as_ref().and_then(|r| self.store.read(cx).instance(r)).map(|i| i.clone_header());
        let Some(inst) = inst else { return };
        cx.spawn(async move |this, cx| {
            let d2 = dir.clone();
            let all = cx.background_spawn(async move { crate::addons_extra::pack_files(&d2) }).await;
            let packs = this
                .update(cx, |this, cx| {
                    if this.own_dir().as_ref() != Some(&dir) {
                        return None;
                    }
                    this.loose_packs = Some((dir.clone(), stamp, all.clone()));
                    this.merge_loose(cx);
                    // Only the untracked ones are looked up.
                    let tracked: std::collections::HashSet<String> = this
                        .store
                        .read(cx)
                        .instance(&inst.root)
                        .map(|i| {
                            i.addons.iter().filter(|a| in_class(a, class)).map(|a| a.file_name.to_lowercase()).collect()
                        })
                        .unwrap_or_default();
                    Some(
                        all.into_iter()
                            .filter(|p| {
                                let n = p.path.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
                                !tracked.contains(n.trim_end_matches(".disabled"))
                            })
                            .collect::<Vec<_>>(),
                    )
                })
                .ok()
                .flatten();
            let Some(packs) = packs.filter(|p| !p.is_empty()) else { return };
            let p2 = packs.clone();
            let urls = cx.background_spawn(async move { crate::pack_lookup::look_up(&p2, class) }).await;
            this.update(cx, |this, cx| {
                this.store.update(cx, |s, _| urls.iter().for_each(|u| s.want_image(u)));
                this.merge_loose(cx);
            })
            .ok();
            if cx.background_spawn(async move { crate::pack_lookup::check_versions(&packs, &inst) }).await {
                this.update(cx, |this, cx| this.merge_loose(cx)).ok();
            }
        })
        .detach();
    }

    /// The table: the instance's packs plus the untracked ones read.
    fn merge_loose(&mut self, cx: &mut Context<Self>) {
        let (Some(dir), Some((d, _, packs))) = (self.own_dir(), self.loose_packs.as_ref()) else { return };
        if *d != dir {
            return;
        }
        let packs = packs.clone();
        let Some(mut inst) = self.root.as_ref().and_then(|r| self.store.read(cx).instance(r)).cloned() else {
            return;
        };
        let class = self.class;
        let tracked: std::collections::HashSet<String> =
            inst.addons.iter().filter(|a| in_class(a, class)).map(|a| a.file_name.to_lowercase()).collect();
        let mut updates = self.updates(&inst, cx);
        let mut set = std::collections::HashSet::new();
        for p in &packs {
            let (a, u) = crate::addons_extra::pack_addon(p, class, false);
            let key = a.file_name.to_lowercase();
            if tracked.contains(&key) {
                continue;
            }
            if let Some(u) = u {
                updates.insert(u.project_id, u);
            }
            set.insert(key);
            inst.addons.push(a);
        }
        if set.is_empty() && self.loose_set.is_empty() {
            return;
        }
        self.loose_set = set;
        self.table.inst = Some(Rc::new(inst));
        self.table.updates = Rc::new(updates);
        self.table.rows_for = None;
        self.ensure_rows();
        self.fill_also();
        self.want_images(cx);
        cx.notify();
    }

    /// Renames untracked packs on or off (".disabled") where they are.
    pub(crate) fn set_loose_enabled(&mut self, files: Vec<String>, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = self.own_dir() else { return };
        for f in &files {
            let from = Self::global_path(&dir, f);
            let to = if on { dir.join(f) } else { dir.join(format!("{f}.disabled")) };
            if from != to
                && let Err(e) = std::fs::rename(&from, &to)
            {
                window.push_notification(Notification::error(format!("{f}: {e}")), cx);
            }
        }
        self.resync(cx);
    }

    /// Moves untracked packs to the instance's backups, with Undo.
    pub(crate) fn remove_loose(&mut self, files: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(dir), Some(root)) = (self.own_dir(), self.root.clone()) else { return };
        let what = if files.len() == 1 { self.describe_packs(&files) } else { format!("{} packs", files.len()) };
        let mut moved: Vec<Removed> = Vec::new();
        for f in &files {
            let original = Self::global_path(&dir, f);
            match hopper::sync::backup_into(&root, &original) {
                Ok(Some(backup)) => moved.push(Removed { original, backup, addon: None }),
                Ok(None) => {}
                Err(e) => window.push_notification(Notification::error(format!("{f}: {e:#}")), cx),
            }
        }
        self.selected.clear();
        self.resync(cx);
        if moved.is_empty() {
            return;
        }
        let store = self.store.clone();
        self.undo_toast(
            format!("Removed {what}. It's in ⋯ › Backups."),
            Rc::new(move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                undo_remove(&store, root.clone(), moved.clone(), window, cx);
                this.resync(cx);
            }),
            window,
            cx,
        );
    }
}
