//! An instance's Worlds and Screenshots tabs; a server's Worlds tab (on
//! this PC or a Hopper Server) is the same view.

use crate::widgets::Clickable as _;
use crate::store::{Msg, Store, open_folder, open_path};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::instance_ops::{self, World};
use std::collections::{HashMap, HashSet};
use crate::addons_extra::{datapacks, is_hopper_file};
use crate::selection::{self, KeyAct, Selection};
use crate::panel_tabs::Target;
use gpui_kit::component::input::{Input, InputState};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// What these tabs ask the Library to do.
pub enum ContentEvent {
    /// Find content of a class; data packs go to `dest` (instance-relative).
    Browse { class: u32, dest: Option<String> },
    /// A server's folder on a Hopper Server, in its Files tab (relative to
    /// the server's folder).
    OpenFiles(String),
}

/// "1 world", "3 worlds".
fn plural(n: usize, what: &str) -> String {
    format!("{n} {what}{}", if n == 1 { "" } else { "s" })
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// A rounded card for one group of things.
fn card(cx: &App) -> Div {
    let theme = cx.theme();
    v_flex()
        .gap_3()
        .p_4()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
}

// ---- Worlds --------------------------------------------------------------

/// Where a Worlds tab reads from: an instance's `saves/`, or a server
/// through its tabs' API (a Hopper Server's panel, or a folder on this PC
/// answered in process), so local and remote servers share one path.
#[derive(Clone)]
enum WorldsSource {
    Instance,
    Server(Target),
}

pub struct WorldsView {
    store: Entity<Store>,
    root: Option<PathBuf>,
    source: WorldsSource,
    worlds: Vec<World>,
    /// Data packs per world folder (a server's on a Hopper Server: names).
    packs: HashMap<PathBuf, Vec<PathBuf>>,
    /// A server's worlds together, in bytes.
    total: u64,
    /// The server runs: switching worlds waits for it to stop.
    running: bool,
    /// A server's list: None while it's on its way, else whether it came.
    load: Option<Result<(), String>>,
    /// What the last server change did (false) or why it failed (true).
    notice: Option<(bool, String)>,
    open: HashSet<PathBuf>,
    /// Picked worlds, by folder.
    sel: Selection,
    focus: FocusHandle,
    /// The New world row (servers): open, its name and seed.
    creating: bool,
    new_name: Entity<InputState>,
    new_seed: Entity<InputState>,
}

impl EventEmitter<ContentEvent> for WorldsView {}

impl WorldsView {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            store,
            root: None,
            source: WorldsSource::Instance,
            worlds: Vec::new(),
            packs: HashMap::new(),
            total: 0,
            running: false,
            load: None,
            notice: None,
            open: HashSet::new(),
            sel: Selection::new(),
            focus: cx.focus_handle(),
            creating: false,
            new_name: cx.new(|cx| InputState::new(window, cx).placeholder("World name")),
            new_seed: cx.new(|cx| InputState::new(window, cx).placeholder("Seed (empty: random)")),
        }
    }

    fn target(&self) -> Option<Target> {
        match &self.source {
            WorldsSource::Server(t) => Some(t.clone()),
            WorldsSource::Instance => None,
        }
    }

    fn is_server(&self) -> bool {
        matches!(self.source, WorldsSource::Server(_))
    }

    /// The worlds' keys in the order they're shown.
    fn order(&self) -> Vec<String> {
        self.worlds.iter().map(|w| w.dir.display().to_string()).collect()
    }

    fn picked_dirs(&self) -> Vec<PathBuf> {
        self.worlds.iter().filter(|w| self.sel.contains(&w.dir.display().to_string())).map(|w| w.dir.clone()).collect()
    }

    fn world_at(&self, dir: &Path) -> Option<&World> {
        self.worlds.iter().find(|w| w.dir == dir)
    }

    /// `/worlds/{name}{rest}` on the server's API.
    fn world_path(t: &Target, name: &str, rest: &str) -> String {
        t.sp(&format!("/worlds/{}{rest}", hopper::hopperd::enc(name)))
    }

    /// Asks once, then deletes `dirs`: an instance's the way the setting
    /// says (Recycle Bin unless it's set to delete for good), a server's
    /// into its trash (Files › Trash), as Hopper Server does.
    fn ask_delete(&mut self, dirs: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        // A server's active world stays: it's the one it loads.
        let dirs: Vec<PathBuf> = dirs.into_iter().filter(|d| self.world_at(d).is_none_or(|w| !w.active)).collect();
        let n = dirs.len();
        if n == 0 {
            return;
        }
        let permanent = self.store.read(cx).settings().delete_permanently;
        let title = match self.worlds.iter().find(|w| n == 1 && w.dir == dirs[0]) {
            Some(w) => format!("Delete {}?", w.name),
            None => format!("Delete {n} worlds?"),
        };
        let target = self.target();
        let names: Vec<String> = dirs.iter().filter_map(|d| self.world_at(d).map(|w| w.name.clone())).collect();
        let view = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (dirs, view, target, names) = (dirs.clone(), view.clone(), target.clone(), names.clone());
            alert
                .title(title.clone())
                .description(match (&target, permanent, n) {
                    (Some(_), _, 1) => "The world moves to the server's trash (Files › Trash), where it can be put back.",
                    (Some(_), _, _) => "The worlds move to the server's trash (Files › Trash), where they can be put back.",
                    (None, true, 1) => "The world is deleted for good.",
                    (None, true, _) => "The worlds are deleted for good.",
                    (None, false, 1) => "The world moves to the Recycle Bin.",
                    (None, false, _) => "The worlds move to the Recycle Bin.",
                })
                .confirm()
                .ok_text(if n == 1 { "Delete world" } else { "Delete worlds" })
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    if let Some(t) = &target {
                        let paths: Vec<String> = names.iter().map(|n| Self::world_path(t, n, "")).collect();
                        let t = t.clone();
                        view.update(cx, |_, cx| {
                            crate::panel_tabs::call(cx, &t, move |c| paths.iter().try_for_each(|p| c.delete(p).map(|_| ())), |this: &mut Self, r, cx| {
                                this.after(r, None, cx)
                            })
                        });
                        return true;
                    }
                    for dir in &dirs {
                        let r = if permanent {
                            hopper::instance_ops::remove_for_good(dir)
                        } else {
                            trash::delete(dir).map_err(Into::into)
                        };
                        if let Err(e) = r {
                            window.push_notification(Notification::error(format!("{e:#}")), cx);
                        }
                    }
                    view.update(cx, |v, cx| v.reload(cx));
                    true
                })
        });
    }

    /// After a server change: says what failed (or `ok`), then lists again.
    fn after(&mut self, r: Result<(), String>, ok: Option<String>, cx: &mut Context<Self>) {
        self.notice = match r {
            Err(e) => Some((true, e)),
            Ok(()) => ok.map(|m| (false, m)),
        };
        self.reload(cx);
    }

    /// An instance's worlds zip into .hopper-backups/worlds; a server's go
    /// to its Backups (restored from there), one job per world.
    fn back_up(&self, dirs: &[PathBuf], cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        if let Some(t) = self.target() {
            for w in self.worlds.iter().filter(|w| dirs.contains(&w.dir) && w.created) {
                let (title, path, name) = (format!("Backing up {}", w.name), Self::world_path(&t, &w.name, "/backup"), w.name.clone());
                crate::panel_tabs::job(cx, &self.store, &t, title, path, Value::Null, move |this: &mut Self, r, cx| {
                    this.after(r.map(|_| ()), Some(format!("{name} is backed up. It's in Backups.")), cx)
                });
            }
            return;
        }
        let s = self.store.read(cx);
        let Some(full) = s.instance(&root).cloned() else { return };
        for world in self.worlds.iter().filter(|w| dirs.contains(&w.dir)).cloned() {
            let full = full.clone();
            s.spawn_job(format!("Backing up {}", world.name), move |job| {
                let out = instance_ops::backup_world(&full, &world, job)?;
                job.detail(format!("Saved {}", out.display()));
                Ok(Msg::Nothing)
            });
        }
    }

    /// Makes a server's world the one it loads on its next start.
    fn make_active(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(t) = self.target() else { return };
        let path = Self::world_path(&t, &name, "/activate");
        crate::panel_tabs::call(cx, &t, move |c| c.post(&path, Value::Null).map(|_| ()), move |this: &mut Self, r, cx| {
            this.after(r, Some(format!("{name} loads on the server's next start.")), cx)
        });
    }

    fn create(&mut self, cx: &mut Context<Self>) {
        let Some(t) = self.target() else { return };
        let name = self.new_name.read(cx).value().trim().to_string();
        let seed = self.new_seed.read(cx).value().trim().to_string();
        if name.is_empty() {
            return;
        }
        let path = t.sp("/worlds");
        let body = json!({ "name": name, "seed": seed, "activate": false });
        self.creating = false;
        crate::panel_tabs::call(cx, &t, move |c| c.post(&path, body).map(|_| ()), move |this: &mut Self, r, cx| {
            this.after(r, Some(format!("{name} is made when the server starts with it active.")), cx)
        });
    }

    fn start_create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.creating = true;
        for i in [&self.new_name, &self.new_seed] {
            i.update(cx, |s, cx| s.set_value("", window, cx));
        }
        self.new_name.update(cx, |s, cx| s.focus(window, cx));
        cx.notify();
    }

    pub fn show_for(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.source = WorldsSource::Instance;
        self.root = Some(root);
        self.notice = None;
        self.reload(cx);
    }

    /// A server's worlds through `target`; `running` holds back switching.
    /// Read again each time the tab is opened (`again`).
    pub fn show_server(&mut self, root: PathBuf, target: Target, running: bool, again: bool, cx: &mut Context<Self>) {
        let same = self.root.as_ref() == Some(&root) && matches!(&self.source, WorldsSource::Server(t) if *t == target);
        if running != self.running {
            self.running = running;
            cx.notify();
        }
        if same && !again {
            return;
        }
        if !same {
            self.worlds.clear();
            self.packs.clear();
            self.creating = false;
            self.notice = None;
            self.load = None;
            self.sel.clear();
        }
        self.source = WorldsSource::Server(target);
        self.root = Some(root);
        self.reload(cx);
    }

    /// Reads worlds and data packs: an instance's from disk (quick: folder
    /// listings), a server's from its API.
    fn reload(&mut self, cx: &mut Context<Self>) {
        if let Some(t) = self.target() {
            crate::panel_tabs::get(cx, &t, t.sp("/worlds"), |this: &mut Self, r, cx| {
                match r {
                    Ok(v) => {
                        this.take_server_list(&v);
                        this.load = Some(Ok(()));
                    }
                    Err(e) => this.load = Some(Err(e)),
                }
                let order = this.order();
                this.sel.retain(&order);
                cx.notify();
            });
            return;
        }
        let Some(inst) = self
            .root
            .as_ref()
            .and_then(|r| self.store.read(cx).instance(r))
            .map(|i| i.clone_header())
        else {
            return;
        };
        self.worlds = instance_ops::worlds(&inst);
        self.worlds.retain(|w| !is_hopper_file(&w.dir));
        self.packs = self
            .worlds
            .iter()
            .map(|w| {
                (
                    w.dir.clone(),
                    datapacks(&w.dir.join("datapacks")),
                )
            })
            .collect();
        let order = self.order();
        self.sel.retain(&order);
        cx.notify();
    }

    /// The `/worlds` reply as worlds: folders under the server's folder on
    /// this PC, or bare names on a Hopper Server.
    fn take_server_list(&mut self, v: &Value) {
        let local = self.target().and_then(|t| t.local);
        let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
        self.total = v["total_size"].as_u64().unwrap_or(0);
        if let Some(r) = v["running"].as_bool() {
            self.running = r;
        }
        self.worlds.clear();
        self.packs.clear();
        for w in v["worlds"].as_array().into_iter().flatten() {
            let name = s(&w["name"]);
            let dir = match &local {
                Some(root) => root.join(&name),
                None => PathBuf::from(&name),
            };
            let icon = local.as_ref().filter(|_| w["icon"].as_bool() == Some(true)).map(|_| dir.join("icon.png"));
            let packs = w["datapacks"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|p| match &local {
                    Some(_) => dir.join("datapacks").join(s(&p["name"])),
                    None => PathBuf::from(s(&p["name"])),
                })
                .collect();
            self.packs.insert(dir.clone(), packs);
            self.worlds.push(World {
                name,
                icon,
                last_played: w["last_played"].as_i64().unwrap_or(0),
                active: w["active"].as_bool().unwrap_or(false),
                size: w["size"].as_u64().unwrap_or(0),
                seed: w["seed"].as_str().map(str::to_string).filter(|s| !s.is_empty()),
                created: w["created"].as_bool().unwrap_or(true),
                dir,
            });
        }
    }

    fn remove_pack(&mut self, world: &Path, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            return;
        };
        if let Some(t) = self.target() {
            let Some(name) = self.world_at(world).map(|w| w.name.clone()) else { return };
            let file = file_name(&path);
            let p = Self::world_path(&t, &name, &format!("/datapacks/{}", hopper::hopperd::enc(&file)));
            crate::panel_tabs::call(cx, &t, move |c| c.delete(&p).map(|_| ()), move |this: &mut Self, r, cx| {
                this.after(r, Some(format!("{file} is in the server's trash (Files › Trash).")), cx)
            });
            return;
        }
        if let Err(e) = hopper::sync::backup_into(&root, &path) {
            window.push_notification(Notification::error(format!("{e:#}")), cx);
        }
        self.reload(cx);
    }

    /// Shows `dir` (a world or one of its folders): Explorer for a folder
    /// on this PC, the Files tab for a server on a Hopper Server.
    fn open_dir(&self, dir: &Path, cx: &mut Context<Self>) {
        match self.target() {
            Some(t) if t.local.is_none() => cx.emit(ContentEvent::OpenFiles(dir.to_string_lossy().replace('\\', "/"))),
            _ => open_path(dir),
        }
    }

    fn pack_rows(&self, world: &Path, packs: &[PathBuf], cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let tip = if self.is_server() { "Remove (to the server's trash)" } else { "Remove (undo from History)" };
        v_flex().gap_0p5().children(packs.iter().map(|p| {
            let (path, world) = (p.clone(), world.to_path_buf());
            h_flex()
                .id(ElementId::Name(format!("dp:{}", p.display()).into()))
                .gap_3()
                .px_2()
                .py_1()
                .rounded(theme.radius)
                .hover(|s| s.bg(theme.list_hover))
                .child(
                    Icon::new(IconName::Package)
                        .small()
                        .text_color(theme.muted_foreground),
                )
                // Like the world's title: Remove stays near the name.
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .max_w(rems(38.5))
                        .text_sm()
                        .truncate()
                        .child(file_name(p)),
                )
                .child(
                    Button::new(ElementId::Name(format!("dp-rm:{}", p.display()).into()))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Trash)
                        .tooltip(tip)
                        .on_click(
                            cx.listener(move |this, _, w, cx| {
                                // A button in the card doesn't pick the world.
                                cx.stop_propagation();
                                this.remove_pack(&world, path.clone(), w, cx)
                            }),
                        ),
                )
        }))
    }

    fn pack_actions(
        &self,
        id: &str,
        dir: PathBuf,
        rel: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        // Browse installs into folders on this PC only.
        let can_add = self.target().is_none_or(|t| t.local.is_some());
        h_flex()
            .gap_2()
            .when(can_add, |r| {
                r.child(
                    Button::new(ElementId::Name(format!("dp-add:{id}").into()))
                        .xsmall()
                        .outline()
                        .icon(IconName::Plus)
                        .label("Add data packs")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.stop_propagation();
                            cx.emit(ContentEvent::Browse {
                                class: hopper::cf::CLASS_DATAPACKS,
                                dest: Some(rel.clone()),
                            })
                        })),
                )
            })
            .child(
                Button::new(ElementId::Name(format!("dp-open:{id}").into()))
                    .xsmall()
                    .ghost()
                    .icon(IconName::FolderOpen)
                    .label("Open folder")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        if this.target().is_none_or(|t| t.local.is_some()) {
                            let _ = std::fs::create_dir_all(&dir);
                        }
                        this.open_dir(&dir, cx)
                    })),
            )
    }

    /// "Last played 2 days ago", plus a server world's size and seed.
    fn world_caption(&self, w: &World) -> String {
        if !w.created {
            return "Made when the server next starts with it active".into();
        }
        let mut parts = vec![format!("Last played {}", hopper::time::relative_secs(w.last_played))];
        if self.is_server() {
            if w.size > 0 {
                parts.push(hopper::instance_ops::human_bytes(w.size));
            }
            if let Some(seed) = &w.seed {
                parts.push(format!("seed {seed}"));
            }
        }
        parts.join(" · ")
    }

    fn world_card(&self, w: &World, cx: &mut Context<Self>) -> AnyElement {
        let server = self.is_server();
        let root = self.root.clone().unwrap_or_default();
        let packs = self.packs.get(&w.dir).cloned().unwrap_or_default();
        let expanded = self.open.contains(&w.dir);
        let id = |p: &str| ElementId::Name(format!("{p}:{}", w.dir.display()).into());
        let (dir1, dir2, dir3, dir4) = (w.dir.clone(), w.dir.clone(), w.dir.clone(), w.dir.clone());
        let key = w.dir.display().to_string();
        let picked = self.sel.contains(&key);
        let permanent = self.store.read(cx).settings().delete_permanently;
        let rel = w
            .dir
            .join("datapacks")
            .strip_prefix(&root)
            .map(|r| r.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let caption = self.world_caption(w);
        let running = self.running;
        let name = w.name.clone();
        let success = cx.theme().success;
        let icon = match &w.icon {
            Some(p) => div()
                .size_12()
                .flex_none()
                .rounded(cx.theme().radius)
                .overflow_hidden()
                .child(img(p.clone()).size_full())
                .into_any_element(),
            None => widgets::thumb(None, rems(3.), cx),
        };
        let (muted, fg) = (cx.theme().muted_foreground, cx.theme().foreground);
        card(cx)
            .id(id("w-card"))
            .clickable(cx)
            .when(picked, |c| widgets::row_selected(c, cx))
            .when(!picked, |c| c.hover(|s| widgets::row_hover(s, cx)))
            // A click picks the world; Ctrl and Shift pick several, as in
            // a file explorer.
            .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                let order = this.order();
                this.sel.click(&key, e.modifiers(), &order);
                cx.notify();
            }))
            .child(
                h_flex()
                    .gap_4()
                    .child(icon)
                    // The title stops growing at 40rem so the actions sit
                    // by it on wide windows, not at the far edge.
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .max_w(rems(40.))
                            .gap_0p5()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .min_w_0()
                                    .child(div().font_semibold().truncate().child(w.name.clone()))
                                    .when(w.active, |r| r.child(widgets::chip(success).child("Active"))),
                            )
                            .child(widgets::caption(caption, cx)),
                    )
                    .when(server && !w.active, |r| {
                        r.child(
                            Button::new(id("w-active"))
                                .outline()
                                .small()
                                .label("Make active")
                                .disabled(running)
                                .tooltip(if running { "Stop the server first" } else { "The server loads this world on its next start" })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.make_active(name.clone(), cx)
                                })),
                        )
                    })
                    .when(w.created, |r| {
                        r.child(
                            Button::new(id("w-open"))
                                .ghost()
                                .small()
                                .icon(IconName::FolderOpen)
                                .tooltip("Open world folder")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.open_dir(&dir1, cx)
                                })),
                        )
                        .child(
                            Button::new(id("w-backup"))
                                .outline()
                                .small()
                                .icon(IconName::Archive)
                                .label("Back up")
                                .tooltip(if server { "Back up into the server's Backups" } else { "Zip into .hopper-backups/worlds" })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.back_up(&[dir4.clone()], cx)
                                })),
                        )
                    })
                    .child(
                        Button::new(id("w-delete"))
                            .ghost()
                            .small()
                            .icon(IconName::Trash)
                            .disabled(w.active)
                            .tooltip(match (w.active, server, permanent) {
                                (true, ..) => "The active world can't be deleted. Make another one active first.",
                                (_, true, _) => "Delete (to the server's trash)",
                                (_, false, true) => "Delete",
                                (_, false, false) => "Delete (to the Recycle Bin)",
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.ask_delete(vec![dir2.clone()], window, cx)
                            })),
                    ),
            )
            .when(w.created, |c| {
                c.child(
                    h_flex()
                        .id(id("w-packs"))
                        .gap_2()
                        .clickable(cx)
                        .text_sm()
                        .text_color(muted)
                        .hover(|s| s.text_color(fg))
                        .child(
                            Icon::new(if expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .small(),
                        )
                        .child(format!("Data packs ({})", packs.len()))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            // Folding the packs doesn't pick the world.
                            cx.stop_propagation();
                            if !this.open.remove(&dir3) {
                                this.open.insert(dir3.clone());
                            }
                            cx.notify();
                        })),
                )
            })
            .when(expanded && w.created, |c| {
                let dir = w.dir.join("datapacks");
                c.child(widgets::enter(
                    id("w-packs-body"),
                    v_flex()
                        .gap_2()
                        .pl_6()
                        .child(self.pack_rows(&w.dir, &packs, cx))
                        .child(self.pack_actions(&w.dir.display().to_string(), dir, rel, cx)),
                ))
            })
            .into_any_element()
    }

    /// The row a server's New world opens: name, seed, Create.
    fn create_row(&self, cx: &mut Context<Self>) -> AnyElement {
        card(cx)
            .child(div().font_semibold().child("New world"))
            .child(
                h_flex()
                    .gap_2()
                    .child(div().w_64().child(Input::new(&self.new_name).small()))
                    .child(div().w_64().child(Input::new(&self.new_seed).small()))
                    .child(
                        Button::new("world-create")
                            .primary()
                            .small()
                            .label("Create")
                            .on_click(cx.listener(|this, _, _, cx| this.create(cx))),
                    )
                    .child(
                        Button::new("world-create-cancel")
                            .ghost()
                            .small()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.creating = false;
                                cx.notify();
                            })),
                    ),
            )
            .child(widgets::caption(
                "The server makes it on its first start with it active (Make active).",
                cx,
            ))
            .into_any_element()
    }

    /// The bar above the list: what's there and the ways to add more.
    fn toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(root) = self.root.clone() else { return div().into_any_element() };
        let count = plural(self.worlds.len(), "world");
        if let Some(t) = self.target() {
            let caption = if self.total > 0 { format!("{count} · {}", hopper::instance_ops::human_bytes(self.total)) } else { count };
            return h_flex()
                .gap_2()
                .child(widgets::caption(caption, cx))
                .child(div().flex_1())
                .when_some(t.local.clone(), |r, dir| {
                    r.child(
                        Button::new("server-folder")
                            .outline()
                            .small()
                            .icon(IconName::FolderOpen)
                            .label("Server folder")
                            .on_click(move |_, _, _| open_path(&dir)),
                    )
                })
                .child(
                    Button::new("new-world")
                        .primary()
                        .small()
                        .icon(IconName::Plus)
                        .label("New world")
                        .on_click(cx.listener(|this, _, window, cx| this.start_create(window, cx))),
                )
                .into_any_element();
        }
        let (saves, backups) = (
            root.join("saves"),
            root.join(hopper::sync::BACKUP_DIR).join("worlds"),
        );
        h_flex()
            .gap_2()
            .child(widgets::caption(count, cx))
            .child(div().flex_1())
            .child(
                Button::new("saves")
                    .outline()
                    .small()
                    .icon(IconName::FolderOpen)
                    .label("Saves folder")
                    .on_click(move |_, _, _| open_path(&saves)),
            )
            .child(
                // Only the folder; the instance's Backups dialog is
                // in the header's menu.
                Button::new("world-backups")
                    .outline()
                    .small()
                    .icon(IconName::ExternalLink)
                    .label("Open backups folder")
                    .on_click(move |_, _, _| {
                        let _ = std::fs::create_dir_all(&backups);
                        open_path(&backups)
                    }),
            )
            .child(
                Button::new("get-worlds")
                    .primary()
                    .small()
                    .icon(IconName::Plus)
                    .label("Get worlds")
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(ContentEvent::Browse {
                            class: hopper::cf::CLASS_WORLDS,
                            dest: None,
                        })
                    })),
            )
            .into_any_element()
    }
}

impl Render for WorldsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.root.is_none() {
            return div().into_any_element();
        }
        let server = self.is_server();
        // A server's list on its way, or why it didn't come.
        let state: Option<AnyElement> = match &self.load {
            _ if !server => None,
            None => Some(widgets::skeleton_rows(4, rems(5.), cx).into_any_element()),
            Some(Err(e)) => Some(
                widgets::empty_state(IconName::TriangleAlert, "Couldn't list the worlds", Some(e.clone().into()), cx).into_any_element(),
            ),
            Some(Ok(())) => None,
        };
        let worlds: Vec<AnyElement> = if state.is_some() {
            Vec::new()
        } else {
            self.worlds.clone().iter().map(|w| self.world_card(w, cx)).collect()
        };
        let empty = worlds.is_empty() && state.is_none();
        let bar = self.sel.many().then(|| {
            let view = cx.entity();
            let v2 = view.clone();
            selection::floating(
                "worlds-bar",
                selection::bar(
                    "worlds-bar",
                    self.sel.len(),
                    vec![
                        Button::new("worlds-bar-backup")
                            .small()
                            .ghost()
                            .icon(IconName::Archive)
                            .label("Back up")
                            .tooltip(if server { "Back up into the server's Backups" } else { "Zip into .hopper-backups/worlds" })
                            .on_click(cx.listener(|this, _, _, cx| {
                                let dirs = this.picked_dirs();
                                this.back_up(&dirs, cx)
                            }))
                            .into_any_element(),
                    ],
                    Some((
                        "Delete".into(),
                        Box::new(move |_, window, cx| {
                            view.update(cx, |this, cx| {
                                let dirs = this.picked_dirs();
                                this.ask_delete(dirs, window, cx)
                            })
                        }),
                    )),
                    move |_, _, cx| {
                        v2.update(cx, |this, cx| {
                            this.sel.clear();
                            cx.notify();
                        })
                    },
                    cx,
                ),
            )
        });
        let create = (server && self.creating).then(|| self.create_row(cx));
        let notice = self.notice.clone().map(|(bad, text)| {
            let theme = cx.theme();
            div().text_sm().text_color(if bad { theme.danger } else { theme.muted_foreground }).child(text)
        });
        let list = v_flex()
            .id("worlds")
            .size_full()
            .px_6()
            .pb_6()
            .gap_4()
            .child(self.toolbar(cx))
            .children(notice)
            .children(create)
            .children(state)
            .children(worlds)
            .when(empty, |r| {
                if server {
                    return r.child(widgets::empty_state(
                        IconName::Globe,
                        "No worlds yet",
                        Some("The server makes one on its first start, or make one with New world.".into()),
                        cx,
                    ));
                }
                r.child(
                    widgets::empty_state(
                        IconName::Globe,
                        "No worlds yet",
                        Some("Play once to create one, or get a world to start from.".into()),
                        cx,
                    )
                    .child(
                        Button::new("get-worlds-empty")
                            .outline()
                            .small()
                            .icon(IconName::Plus)
                            .label("Get worlds")
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(ContentEvent::Browse {
                                    class: hopper::cf::CLASS_WORLDS,
                                    dest: None,
                                })
                            })),
                    ),
                )
            })
            .overflow_y_scrollbar();
        div()
            .relative()
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                if this.creating {
                    match e.keystroke.key.as_str() {
                        "enter" => this.create(cx),
                        "escape" => {
                            this.creating = false;
                            cx.notify();
                        }
                        _ => {}
                    }
                    return;
                }
                let order = this.order();
                match selection::on_key(&mut this.sel, e, &order, cx) {
                    KeyAct::None => return,
                    KeyAct::Delete => {
                        let dirs = this.picked_dirs();
                        this.ask_delete(dirs, window, cx)
                    }
                    KeyAct::Changed | KeyAct::Moved(_) => {}
                }
                cx.notify();
            }))
            .child(list)
            .children(bar)
            .into_any_element()
    }
}

// ---- Screenshots ---------------------------------------------------------

pub struct ScreenshotsView {
    store: Entity<Store>,
    root: Option<PathBuf>,
    shots: Vec<PathBuf>,
    thumbs: HashMap<PathBuf, PathBuf>,
    /// The screenshot shown large.
    open: Option<usize>,
    focus: FocusHandle,
    /// Picked screenshots in the grid, by path.
    sel: Selection,
    grid_focus: FocusHandle,
}

impl ScreenshotsView {
    pub fn new(store: Entity<Store>, _: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            store,
            root: None,
            shots: Vec::new(),
            thumbs: HashMap::new(),
            open: None,
            focus: cx.focus_handle(),
            sel: Selection::new(),
            grid_focus: cx.focus_handle(),
        }
    }

    fn order(&self) -> Vec<String> {
        self.shots.iter().map(|p| p.display().to_string()).collect()
    }

    fn picked(&self) -> Vec<PathBuf> {
        self.shots.iter().filter(|p| self.sel.contains(&p.display().to_string())).cloned().collect()
    }

    /// Asks once, then sends `paths` to the Recycle Bin (as the viewer's
    /// Delete does).
    fn ask_delete(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let n = paths.len();
        if n == 0 {
            return;
        }
        let title = if n == 1 { format!("Delete {}?", file_name(&paths[0])) } else { format!("Delete {n} screenshots?") };
        let view = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (paths, view) = (paths.clone(), view.clone());
            alert
                .title(title.clone())
                .description(if n == 1 { "It moves to the Recycle Bin." } else { "They move to the Recycle Bin." })
                .confirm()
                .ok_text("Delete")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    for p in &paths {
                        if let Err(e) = trash::delete(p) {
                            window.push_notification(Notification::error(format!("{e:#}")), cx);
                        }
                    }
                    view.update(cx, |v, cx| v.reload(cx));
                    true
                })
        });
    }

    pub fn show_for(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.root.as_ref() != Some(&root) {
            self.open = None;
        }
        self.root = Some(root);
        self.reload(cx);
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(inst) = self
            .root
            .as_ref()
            .and_then(|r| self.store.read(cx).instance(r))
            .map(|i| i.clone_header())
        else {
            return;
        };
        self.shots = instance_ops::screenshots(&inst);
        self.shots.retain(|p| !is_hopper_file(p));
        let order = self.order();
        self.sel.retain(&order);
        let missing: Vec<PathBuf> = self
            .shots
            .iter()
            .filter(|p| !self.thumbs.contains_key(*p))
            .cloned()
            .collect();
        for p in missing {
            cx.spawn(async move |this, cx| {
                let src = p.clone();
                let t = cx
                    .background_spawn(async move { hopper::imgcache::thumbnail(&src, 480) })
                    .await;
                if let Ok(t) = t {
                    this.update(cx, |this, cx| {
                        this.thumbs.insert(p, t);
                        cx.notify();
                    })
                    .ok();
                }
            })
            .detach();
        }
        cx.notify();
    }

    fn go(&mut self, to: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        self.open = to.filter(|i| *i < self.shots.len());
        if self.open.is_some() {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    fn copy(&self, p: &Path, window: &mut Window, cx: &mut App) {
        let format = match p
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
            .as_deref()
        {
            Some("jpg" | "jpeg") => ImageFormat::Jpeg,
            _ => ImageFormat::Png,
        };
        match std::fs::read(p) {
            Ok(bytes) => {
                cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(format, bytes)));
                window.push_notification(Notification::success("Copied the screenshot."), cx);
            }
            Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
        }
    }

    fn delete(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.shots.get(i).cloned() else {
            return;
        };
        match trash::delete(&p) {
            Ok(()) => {
                self.reload(cx);
                // Stay in the viewer, on the neighbour.
                let n = i.min(self.shots.len().saturating_sub(1));
                self.open = (!self.shots.is_empty()).then_some(n);
            }
            Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
        }
        cx.notify();
    }

    fn viewer(&self, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let p = self.shots[i].clone();
        let n = self.shots.len();
        let (p1, p2, p3) = (p.clone(), p.clone(), p.clone());
        v_flex()
            .id("shot-viewer")
            .track_focus(&self.focus)
            .size_full()
            .px_6()
            .pb_6()
            .gap_3()
            .on_key_down(cx.listener(move |this, e: &KeyDownEvent, window, cx| {
                match e.keystroke.key.as_str() {
                    "left" if i > 0 => this.go(Some(i - 1), window, cx),
                    "right" => this.go(Some(i + 1), window, cx),
                    "escape" => this.go(None, window, cx),
                    _ => {}
                }
            }))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("shot-back")
                            .ghost()
                            .small()
                            .icon(IconName::ChevronLeft)
                            .label("All screenshots")
                            .on_click(cx.listener(|this, _, w, cx| this.go(None, w, cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("shot-prev")
                            .outline()
                            .small()
                            .icon(IconName::ArrowLeft)
                            .tooltip("Previous (←)")
                            .disabled(i == 0)
                            .on_click(
                                cx.listener(move |this, _, w, cx| this.go(i.checked_sub(1), w, cx)),
                            ),
                    )
                    .child(widgets::caption(format!("{} of {n}", i + 1), cx).px_2())
                    .child(
                        Button::new("shot-next")
                            .outline()
                            .small()
                            .icon(IconName::ArrowRight)
                            .tooltip("Next (→)")
                            .disabled(i + 1 >= n)
                            .on_click(
                                cx.listener(move |this, _, w, cx| this.go(Some(i + 1), w, cx)),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("shot-copy")
                            .outline()
                            .small()
                            .icon(IconName::Copy)
                            .label("Copy")
                            .on_click(cx.listener(move |this, _, w, cx| this.copy(&p1, w, cx))),
                    )
                    .child(
                        Button::new("shot-open")
                            .outline()
                            .small()
                            .icon(IconName::ExternalLink)
                            .label("Open")
                            .on_click(move |_, _, _| open_path(&p2)),
                    )
                    .child(
                        Button::new("shot-folder")
                            .ghost()
                            .small()
                            .icon(IconName::FolderOpen)
                            .tooltip("Show in folder")
                            .on_click(move |_, _, _| {
                                crate::store::reveal_path(&p3);
                            }),
                    )
                    .child(
                        Button::new("shot-delete")
                            .ghost()
                            .small()
                            .icon(IconName::Trash)
                            .tooltip("Delete (to the Recycle Bin)")
                            .on_click(cx.listener(move |this, _, w, cx| this.delete(i, w, cx))),
                    ),
            )
            .child(widgets::caption(file_name(&p), cx).text_xs())
            .child(widgets::enter(
                ElementId::Name(format!("shot:{}", p.display()).into()),
                div()
                    .flex_1()
                    .min_h_0()
                    .rounded(cx.theme().radius_lg)
                    .overflow_hidden()
                    // Pinned to the box rather than sized by it: an img in
                    // flow takes its own height and the bottom gets cut off.
                    .child(img(p).absolute().inset_0().size_full().object_fit(ObjectFit::Contain)),
            ))
            .into_any_element()
    }
}

impl Render for ScreenshotsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(root) = self.root.clone() else {
            return div().into_any_element();
        };
        if let Some(i) = self.open.filter(|i| *i < self.shots.len()) {
            return self.viewer(i, cx);
        }
        let dir = root.join("screenshots");
        let theme = cx.theme();
        let tiles = self.shots.iter().enumerate().map(|(i, p)| {
            let thumb = self.thumbs.get(p).cloned();
            let key = p.display().to_string();
            let picked = self.sel.contains(&key);
            div()
                .id(ElementId::Name(format!("tile:{}", p.display()).into()))
                .w_64()
                .h(rems(9.))
                .flex_none()
                .rounded(theme.radius_lg)
                .overflow_hidden()
                .bg(theme.muted)
                .clickable(cx)
                .border_1()
                .border_color(theme.border)
                // A tile isn't a row: picked ones get the primary edge (and
                // keep it under the pointer).
                .when(picked, |d| d.border_2().border_color(theme.primary))
                .when(!picked, |d| d.hover(|s| s.border_color(theme.foreground.opacity(widgets::alpha::LINE))))
                .tooltip({
                    let name = file_name(p);
                    move |w, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(name.clone()).build(w, cx)
                    }
                })
                // A click opens it large; Ctrl and Shift pick several.
                .on_click(cx.listener(move |this, e: &ClickEvent, w, cx| {
                    let order = this.order();
                    if this.sel.click(&key, e.modifiers(), &order) {
                        this.go(Some(i), w, cx)
                    } else {
                        cx.notify()
                    }
                }))
                .when_some(thumb, |d, t| {
                    d.child(img(t).size_full().object_fit(ObjectFit::Cover))
                })
        });
        let empty = self.shots.is_empty();
        let bar = self.sel.many().then(|| {
            let view = cx.entity();
            let v2 = view.clone();
            selection::floating(
                "shots-bar",
                selection::bar(
                    "shots-bar",
                    self.sel.len(),
                    Vec::new(),
                    Some((
                        "Delete".into(),
                        Box::new(move |_, window, cx| {
                            view.update(cx, |this, cx| {
                                let paths = this.picked();
                                this.ask_delete(paths, window, cx)
                            })
                        }),
                    )),
                    move |_, _, cx| {
                        v2.update(cx, |this, cx| {
                            this.sel.clear();
                            cx.notify();
                        })
                    },
                    cx,
                ),
            )
        });
        let grid = v_flex()
            .id("screenshots")
            .size_full()
            .px_6()
            .pb_6()
            .gap_4()
            .child(
                h_flex()
                    .gap_2()
                    .child(widgets::caption(plural(self.shots.len(), "screenshot"), cx))
                    .child(div().flex_1())
                    .child(
                        Button::new("shots-folder")
                            .outline()
                            .small()
                            .icon(IconName::FolderOpen)
                            .label("Screenshots folder")
                            .on_click(move |_, _, _| open_folder(&dir)),
                    ),
            )
            .child(h_flex().flex_wrap().gap_3().children(tiles))
            .when(empty, |r| {
                r.child(widgets::empty_state(
                    IconName::Image,
                    "No screenshots yet",
                    Some("Press F2 in game to take one.".into()),
                    cx,
                ))
            })
            .overflow_y_scrollbar();
        div()
            .relative()
            .size_full()
            .track_focus(&self.grid_focus)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                // Up and Down walk a list; the tiles wrap, so they'd jump
                // about. The other keys work as in every list.
                if matches!(e.keystroke.key.as_str(), "up" | "down") {
                    return;
                }
                let order = this.order();
                match selection::on_key(&mut this.sel, e, &order, cx) {
                    KeyAct::None => return,
                    KeyAct::Delete => {
                        let paths = this.picked();
                        this.ask_delete(paths, window, cx)
                    }
                    KeyAct::Changed | KeyAct::Moved(_) => {}
                }
                cx.notify();
            }))
            .child(grid)
            .children(bar)
            .into_any_element()
    }
}
