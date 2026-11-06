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
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::radio::Radio;
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

/// "12,345".
fn fmt_thousands(n: u64) -> String {
    let d = n.to_string();
    let mut out = String::new();
    for (i, c) in d.chars().enumerate() {
        if i > 0 && (d.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
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
    /// A server's world folders (with Bukkit-style dimension folders),
    /// what Download zips.
    folders: HashMap<PathBuf, Vec<String>>,
    /// The rest of a server's `/worlds` reply: Chunky, pre-generation,
    /// players online (a Hopper Server's).
    info: Value,
    /// An upload, reset or pre-generation on its way: what it's doing.
    busy: Option<String>,
    /// The active world's Pre-generate section is open (it starts open).
    pg_open: bool,
    /// Counts lists asked for, so only the newest one keeps polling.
    poll: u64,
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
            folders: HashMap::new(),
            info: Value::Null,
            busy: None,
            pg_open: true,
            poll: 0,
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
            self.info = Value::Null;
            self.busy = None;
        }
        self.source = WorldsSource::Server(target);
        self.root = Some(root);
        self.reload(cx);
    }

    /// Reads worlds and data packs: an instance's from disk (quick: folder
    /// listings), a server's from its API.
    fn reload(&mut self, cx: &mut Context<Self>) {
        if let Some(t) = self.target() {
            self.poll += 1;
            let poll = self.poll;
            crate::panel_tabs::get(cx, &t, t.sp("/worlds"), move |this: &mut Self, r, cx| {
                match r {
                    Ok(v) => {
                        this.take_server_list(&v);
                        this.load = Some(Ok(()));
                    }
                    Err(e) => this.load = Some(Err(e)),
                }
                let order = this.order();
                this.sel.retain(&order);
                // While Chunky runs its progress is read again every 2
                // seconds, as Hopper Server's page does; a newer list (or
                // another server) ends this round.
                if this.info["pregen"]["state"] == "running" {
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(std::time::Duration::from_secs(2)).await;
                        this.update(cx, |this, cx| {
                            if this.poll == poll {
                                this.reload(cx);
                            }
                        })
                        .ok();
                    })
                    .detach();
                }
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
        self.folders.clear();
        self.info = json!({
            "chunky": v["chunky"], "pregen": v["pregen"], "last_pregen": v["last_pregen"],
            "players": v["players"], "level": v["level"],
        });
        for w in v["worlds"].as_array().into_iter().flatten() {
            let name = s(&w["name"]);
            let dir = match &local {
                Some(root) => root.join(&name),
                None => PathBuf::from(&name),
            };
            let folders: Vec<String> = w["folders"].as_array().into_iter().flatten().map(s).filter(|f| !f.is_empty()).collect();
            self.folders.insert(dir.clone(), if folders.is_empty() { vec![name.clone()] } else { folders });
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
        let busy = self.busy.is_some();
        // Chunky runs in a Hopper Server's own process; a server on this PC
        // has no pre-generation here.
        let pregen = (server && w.active && self.target().is_some_and(|t| t.local.is_none()) && !self.info["chunky"].is_null())
            .then(|| self.pregen_section(w, cx));
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
                        .when(server, |r| {
                            let w = w.clone();
                            r.child(
                                Button::new(id("w-zip"))
                                    .ghost()
                                    .small()
                                    .icon(IconName::Download)
                                    .tooltip("Download as zip")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.download(&w, window, cx)
                                    })),
                            )
                        })
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
                    .when(server && w.active && w.created, |r| {
                        let w = w.clone();
                        r.child(
                            Button::new(id("w-reset"))
                                .outline()
                                .small()
                                .icon(IconName::RotateCcw)
                                .label("Reset…")
                                .disabled(running || busy)
                                .tooltip(if running { "Stop the server first" } else { "Start this world over" })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.ask_reset(&w, window, cx)
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
            .children(pregen)
            .into_any_element()
    }

    /// Asks where to save the world as a zip (the Downloads folder first),
    /// then saves it as a job in Tasks, as a backup's Download does. The
    /// zip holds all the world's folders, dimension folders too.
    fn download(&mut self, w: &World, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.target() else { return };
        let folders = self.folders.get(&w.dir).cloned().unwrap_or_else(|| vec![w.name.clone()]);
        let query: Vec<String> = folders.iter().map(|f| format!("path={}", crate::panel_tabs::q(f))).collect();
        let path = t.fp(&format!("/zip?{}", query.join("&")));
        let dir = directories::UserDirs::new().and_then(|u| u.download_dir().map(|d| d.to_path_buf())).unwrap_or_default();
        let name = w.name.clone();
        let rx = cx.prompt_for_new_path(&dir, Some(format!("{name}.zip").as_str()));
        let store = self.store.clone();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(local))) = rx.await else { return };
            this.update(cx, |this, cx| {
                let job = store.read(cx).start_job(format!("Downloading {name}"));
                let shown = local.display().to_string();
                this.notice = None;
                crate::panel_tabs::call(
                    cx,
                    &t,
                    move |c| {
                        let r = c.save(&path, &local);
                        job.finish(&r);
                        r
                    },
                    move |this: &mut Self, r, _| {
                        this.notice = Some(match r {
                            Ok(()) => (false, format!("Saved {name} to {shown}")),
                            Err(e) => (true, e),
                        })
                    },
                );
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Picks a world zip to add to the server.
    fn pick_upload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Upload world".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            this.update(cx, |this, cx| {
                if let Some(p) = paths.into_iter().next() {
                    this.upload_world(p, cx)
                }
            })
            .ok();
        })
        .detach();
    }

    /// Sends the zip into the server folder, then has the server unpack it
    /// into a new world, as Hopper Server's Upload world does: one job in
    /// Tasks for both.
    fn upload_world(&mut self, local: PathBuf, cx: &mut Context<Self>) {
        let Some(t) = self.target() else { return };
        if !local.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
            self.notice = Some((true, "Pick a world saved as a .zip file.".into()));
            cx.notify();
            return;
        }
        if self.busy.is_some() {
            return;
        }
        let taken: Vec<String> = self.worlds.iter().map(|w| w.name.clone()).collect();
        let name = hopper::hosting::worlds::upload_name(&file_name(&local), &taken);
        let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
        // The name Hopper Server knows to clear away when unpacking fails.
        let rel = format!("world-upload-{ms}.zip");
        let job = self.store.read(cx).start_job(format!("Adding world {name}"));
        let (id, path) = (t.id().to_string(), t.sp("/worlds/import"));
        let body = json!({ "zip": rel, "name": name });
        self.busy = Some(format!("Uploading {name}"));
        self.notice = None;
        crate::panel_tabs::call(
            cx,
            &t,
            move |c| {
                job.detail("Uploading the zip");
                let r = c.upload(&id, &local, &rel).and_then(|_| {
                    job.detail(format!("Unpacking {}", body["name"].as_str().unwrap_or_default()));
                    c.run_job(&path, body, &job)
                });
                job.finish(&r);
                r
            },
            move |this: &mut Self, r, cx| {
                this.busy = None;
                this.after(r.map(|_| ()), Some(format!("{name} added. Make it active to play it.")), cx)
            },
        );
        cx.notify();
    }

    /// The Reset dialog: same seed, a new random one or one typed in; the
    /// Nether and End kept or not; a backup first only when ticked.
    fn ask_reset(&mut self, w: &World, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.target() else { return };
        #[derive(Clone, Copy, PartialEq)]
        enum Mode {
            Same,
            New,
            Custom,
        }
        struct Form {
            mode: Mode,
            keep: bool,
            backup: bool,
        }
        let seed = w.seed.clone();
        let form = std::rc::Rc::new(std::cell::RefCell::new(Form {
            mode: if seed.is_some() { Mode::Same } else { Mode::New },
            keep: false,
            backup: false,
        }));
        let custom = cx.new(|cx| InputState::new(window, cx).placeholder("Seed"));
        // Typing a seed picks "This seed" and wakes the Reset button.
        let fc = form.clone();
        cx.subscribe_in(&custom, window, move |_, _, e: &InputEvent, window, _| {
            if matches!(e, InputEvent::Change) {
                fc.borrow_mut().mode = Mode::Custom;
                window.refresh();
            }
        })
        .detach();
        let (name, size) = (w.name.clone(), hopper::instance_ops::human_bytes(w.size));
        let (view, store, running) = (cx.entity(), self.store.clone(), self.running);
        window.open_dialog(cx, move |dialog, window, cx| {
            let f = form.borrow();
            let (mode, keep, backup) = (f.mode, f.keep, f.backup);
            drop(f);
            let set = |m: Mode| {
                let form = form.clone();
                move |_: &bool, window: &mut Window, _: &mut App| {
                    form.borrow_mut().mode = m;
                    window.refresh();
                }
            };
            let (fk, fb) = (form.clone(), form.clone());
            let typed = custom.read(cx).value().trim().to_string();
            let can = !running && (mode != Mode::Custom || !typed.is_empty());
            let muted = cx.theme().muted_foreground;
            let (form2, custom2, view, store, t, name2) = (form.clone(), custom.clone(), view.clone(), store.clone(), t.clone(), name.clone());
            dialog
                .title(format!("Reset {name}?"))
                .w(rems(30.).to_pixels(window.rem_size()))
                .margin_top(rems(widgets::DIALOG_TOP).to_pixels(window.rem_size()))
                .child(
                    v_flex()
                        .id("reset-body")
                        .gap_3()
                        .max_h(widgets::dialog_room(window) - window.rem_size() * 3.)
                        .overflow_y_scroll()
                        .text_sm()
                        .child(div().text_color(muted).child("Players get a fresh world on the next start."))
                        .child(
                            v_flex()
                                .gap_2()
                                .child(
                                    Radio::new("reset-same")
                                        .label(match &seed {
                                            Some(s) => format!("Same seed ({s}: same terrain, everything built is gone)"),
                                            None => "Same seed (seed unknown)".into(),
                                        })
                                        .checked(mode == Mode::Same)
                                        .disabled(seed.is_none())
                                        .on_click(set(Mode::Same)),
                                )
                                .child(Radio::new("reset-new").label("New random seed").checked(mode == Mode::New).on_click(set(Mode::New)))
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .child(Radio::new("reset-custom").label("This seed").checked(mode == Mode::Custom).on_click(set(Mode::Custom)))
                                        .child(div().w_48().child(Input::new(&custom).small())),
                                ),
                        )
                        .child(
                            Checkbox::new("reset-keep").label("Keep the Nether and the End").checked(keep).on_click(move |v: &bool, window, _| {
                                fk.borrow_mut().keep = *v;
                                window.refresh();
                            }),
                        )
                        .child(
                            Checkbox::new("reset-backup")
                                .label(format!("Back up the world first ({size}, Backups tab)"))
                                .checked(backup)
                                .on_click(move |v: &bool, window, _| {
                                    fb.borrow_mut().backup = *v;
                                    window.refresh();
                                }),
                        )
                        .child(widgets::caption("The old folder goes to the server's trash (Files › Trash) for 30 days.", cx))
                        .when(running, |c| c.child(div().text_color(cx.theme().danger).child("Stop the server first."))),
                )
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().child(Button::new("reset-cancel").outline().label("Cancel")))
                        .child(
                            DialogAction::new().child(
                                Button::new("reset-ok")
                                    .danger()
                                    .icon(IconName::RotateCcw)
                                    .label(if backup { "Back up and reset" } else { "Reset" })
                                    .disabled(!can),
                            ),
                        ),
                )
                .on_ok(move |_, _, cx| {
                    let f = form2.borrow();
                    let typed = custom2.read(cx).value().trim().to_string();
                    let seed = match f.mode {
                        Mode::Same => json!({ "mode": "same" }),
                        Mode::New => json!({ "mode": "new" }),
                        Mode::Custom if typed.is_empty() => return false,
                        Mode::Custom => json!({ "mode": "custom", "seed": typed }),
                    };
                    let body = json!({ "seed": seed, "keep_dims": f.keep, "backup_first": f.backup });
                    let name = name2.clone();
                    let path = Self::world_path(&t, &name, "/reset");
                    let t = t.clone();
                    view.update(cx, |this, cx| {
                        this.busy = Some(format!("Resetting {name}"));
                        this.notice = None;
                        crate::panel_tabs::job(cx, &store, &t, format!("Resetting {name}"), path, body, move |this: &mut Self, r, cx| {
                            this.busy = None;
                            this.after(r.map(|_| ()), Some(format!("{name} reset. A fresh one is made on the next start.")), cx)
                        });
                        cx.notify();
                    });
                    true
                })
        });
    }

    /// The Pre-generate dialog: a radius and where to center it, then
    /// Hopper Server adds Chunky when it's missing, starts (or restarts)
    /// the server and runs it, as its own page does.
    fn ask_pregen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.target() else { return };
        let last = self.info["last_pregen"]["radius"].as_u64().filter(|r| *r > 0).unwrap_or(3000);
        let radius = cx.new(|cx| {
            let mut s = InputState::new(window, cx);
            s.set_value(last.to_string(), window, cx);
            s
        });
        // The chunk count and the Start button follow the radius typed.
        cx.subscribe_in(&radius, window, |_, _, _: &InputEvent, window, _| window.refresh()).detach();
        let x = cx.new(|cx| InputState::new(window, cx).placeholder("x"));
        let z = cx.new(|cx| InputState::new(window, cx).placeholder("z"));
        // 0: world spawn, 1: 0, 0, 2: typed in.
        let center = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let info = self.info.clone();
        let (view, store, running) = (cx.entity(), self.store.clone(), self.running);
        window.open_dialog(cx, move |dialog, window, cx| {
            let theme = cx.theme().clone();
            let r: u64 = radius.read(cx).value().trim().parse().unwrap_or(0);
            let chunks = (2 * r).div_ceil(16).pow(2);
            let jar = info["chunky"]["jar"].as_str().is_some();
            let players = info["players"].as_u64().unwrap_or(0);
            let blocked = !jar && running && players > 0;
            let c = center.get();
            let (rad2, center2, c3) = (radius.clone(), center.clone(), center.clone());
            let presets = [1000u64, 3000, 5000, 10000];
            let warn = |text: String| {
                h_flex()
                    .gap_2()
                    .items_start()
                    .p_2()
                    .rounded(theme.radius)
                    .bg(theme.warning.opacity(0.12))
                    .child(Icon::new(IconName::TriangleAlert).small().text_color(theme.warning))
                    .child(div().flex_1().min_w_0().child(text))
            };
            let (radius2, x2, z2, center4, view, store, t) = (radius.clone(), x.clone(), z.clone(), center.clone(), view.clone(), store.clone(), t.clone());
            dialog
                .title(format!("Pre-generate {}", info["level"].as_str().unwrap_or("the world")))
                .w(rems(32.).to_pixels(window.rem_size()))
                .margin_top(rems(widgets::DIALOG_TOP).to_pixels(window.rem_size()))
                .child(
                    v_flex()
                        .id("pregen-body")
                        .gap_3()
                        .max_h(widgets::dialog_room(window) - window.rem_size() * 3.)
                        .overflow_y_scroll()
                        .text_sm()
                        .child(div().text_color(theme.muted_foreground).child("Generates the overworld around a point now, so players don't wait for chunks later."))
                        .child(div().font_semibold().child("Radius (blocks)"))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    widgets::Segmented::new("pregen-radius")
                                        .selected_index(presets.iter().position(|p| *p == r).unwrap_or(usize::MAX))
                                        .children(presets.iter().map(|p| widgets::SegItem::new().label(format!("{}k", p / 1000))))
                                        .on_click(move |ix: &usize, window, cx| {
                                            let v = presets.get(*ix).copied().unwrap_or(3000).to_string();
                                            rad2.update(cx, |s, cx| s.set_value(v, window, cx));
                                            window.refresh();
                                        }),
                                )
                                .child(div().w_24().child(Input::new(&radius).small()))
                                .child(widgets::caption(format!("≈ {} chunks", fmt_thousands(chunks)), cx)),
                        )
                        .child(div().font_semibold().child("Center"))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    widgets::Segmented::new("pregen-center")
                                        .selected_index(c)
                                        .child(widgets::SegItem::new().label("World spawn"))
                                        .child(widgets::SegItem::new().label("0, 0"))
                                        .child(widgets::SegItem::new().label("Custom"))
                                        .on_click(move |ix: &usize, window, _| {
                                            center2.set(*ix);
                                            window.refresh();
                                        }),
                                )
                                .when(c3.get() == 2, |r| {
                                    r.child(div().w_24().child(Input::new(&x).small())).child(div().w_24().child(Input::new(&z).small()))
                                }),
                        )
                        .when(!jar, |b| {
                            let kind = &info["chunky"]["kind"];
                            let what = if kind["kind"] == "plugin" {
                                "the Paper plugin".to_string()
                            } else {
                                let l = kind["loader"].as_str().unwrap_or_default();
                                let mut ch = l.chars();
                                format!("the {}{} mod", ch.next().map(|c| c.to_uppercase().to_string()).unwrap_or_default(), ch.as_str())
                            };
                            let mc = info["chunky"]["minecraft"].as_str().filter(|m| !m.is_empty()).unwrap_or("?");
                            b.child(warn(format!(
                                "Chunky isn't installed. Hopper adds {what} for Minecraft {mc} from Modrinth, then {} the server to load it and starts pre-generating.",
                                if running { "restarts" } else { "starts" }
                            )))
                            .when(blocked, |b| {
                                b.child(div().text_color(theme.danger).child(format!(
                                    "{players} {} online. Installing needs a restart; try again when they've left.",
                                    if players == 1 { "player is" } else { "players are" }
                                )))
                            })
                        })
                        .when(jar && !running, |b| b.child(widgets::caption("The server starts first; pre-generation begins once it's up.", cx)))
                        .when(jar && running && players > 0, |b| {
                            b.child(warn(format!("{players} online. Chunky runs alongside them but the server may lag a bit while it works.")))
                        }),
                )
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().child(Button::new("pregen-cancel").outline().label("Cancel")))
                        .child(
                            DialogAction::new().child(
                                Button::new("pregen-ok")
                                    .primary()
                                    .label(match (jar, running) {
                                        (false, true) => "Install, restart and start",
                                        (false, false) => "Install, start and run",
                                        (true, true) => "Start",
                                        (true, false) => "Start server and run",
                                    })
                                    .disabled(!(16..=100_000).contains(&r) || blocked),
                            ),
                        ),
                )
                .on_ok(move |_, _, cx| {
                    let r: u64 = radius2.read(cx).value().trim().parse().unwrap_or(0);
                    if !(16..=100_000).contains(&r) {
                        return false;
                    }
                    let num = |i: &Entity<InputState>| i.read(cx).value().trim().parse::<i64>().unwrap_or(0);
                    let center = match center4.get() {
                        0 => Value::Null,
                        1 => json!([0, 0]),
                        _ => json!([num(&x2), num(&z2)]),
                    };
                    let (path, body, t) = (t.sp("/worlds/pregen"), json!({ "radius": r, "center": center }), t.clone());
                    view.update(cx, |this, cx| {
                        this.busy = Some("Starting pre-generation".into());
                        this.notice = None;
                        crate::panel_tabs::job(cx, &store, &t, "Starting pre-generation", path, body, |this: &mut Self, r, cx| {
                            this.busy = None;
                            this.after(r.map(|_| ()), Some("Pre-generation started.".into()), cx)
                        });
                        cx.notify();
                    });
                    true
                })
        });
    }

    /// Pause, Continue or Cancel on the running pre-generation.
    fn pregen_op(&mut self, op: &'static str, cx: &mut Context<Self>) {
        let Some(t) = self.target() else { return };
        let path = t.sp(&format!("/worlds/pregen/{op}"));
        crate::panel_tabs::call(cx, &t, move |c| c.post(&path, Value::Null).map(|_| ()), |this: &mut Self, r, cx| this.after(r, None, cx));
    }

    /// The active world's Pre-generate section (a server on a Hopper
    /// Server: Chunky runs in its process): what it can do, or how far a
    /// run has got with Pause / Continue / Cancel.
    fn pregen_section(&self, w: &World, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let info = &self.info;
        let pg = &info["pregen"];
        let state = pg["state"].as_str().unwrap_or_default();
        let percent = pg["percent"].as_f64().unwrap_or(0.) as f32;
        let last = &info["last_pregen"];
        let last_here = last["world"].as_str() == Some(w.name.as_str());
        let last_radius = fmt_thousands(last["radius"].as_u64().unwrap_or(0));
        let jar = info["chunky"]["jar"].as_str().map(|j| j.rsplit('/').next().unwrap_or(j).to_string());
        let busy = self.busy.is_some();
        let open = self.pg_open;
        let head = h_flex()
            .id("pg-head")
            .gap_2()
            .clickable(cx)
            .text_sm()
            .text_color(theme.muted_foreground)
            .hover(|s| s.text_color(theme.foreground))
            .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).small())
            .child("Pre-generate")
            .when(!open && state == "running", |r| r.child(widgets::chip(theme.primary).child(format!("{percent:.1}%"))))
            .when(!open && state != "running" && last_here, |r| r.child(widgets::chip(theme.muted_foreground).child(format!("Pre-generated {last_radius} blocks"))))
            .on_click(cx.listener(|this, _, _, cx| {
                // Folding it doesn't pick the world.
                cx.stop_propagation();
                this.pg_open = !this.pg_open;
                cx.notify();
            }));
        if !open {
            return head.into_any_element();
        }
        let body = v_flex().gap_2().p_3().rounded(theme.radius_lg).border_1().border_color(theme.border).text_sm();
        let body = if info["chunky"]["kind"].is_null() {
            body.child(widgets::caption(
                "Pre-generation uses Chunky, a mod or plugin. This server runs plain Minecraft, which can't load either; a Fabric, NeoForge, Forge, Quilt or Paper server can.",
                cx,
            ))
        } else if state == "running" || state == "paused" {
            let radius = if last_here { format!(" · radius {last_radius} blocks") } else { String::new() };
            let rate = pg["rate"].as_f64().unwrap_or(0.).round();
            let eta = pg["eta"].as_str().filter(|e| !e.is_empty()).map(|e| format!("ETA {e} · ")).unwrap_or_default();
            body.child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().min_w_0().truncate().child(format!("{}{radius}", pg["dimension"].as_str().unwrap_or_default())))
                    .child(if state == "running" {
                        div().text_color(theme.muted_foreground).child(format!("{eta}{rate} chunks/s"))
                    } else {
                        div().text_color(theme.warning).child("Paused")
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(gpui_kit::component::progress::Progress::new("pg-bar").value(percent.min(100.))))
                    .child(div().font_semibold().child(format!("{percent:.1}%")))
                    .child(if state == "running" {
                        Button::new("pg-pause").outline().small().icon(IconName::Pause).label("Pause").disabled(busy).on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.pregen_op("pause", cx)
                        }))
                    } else {
                        Button::new("pg-continue").outline().small().icon(IconName::Play).label("Continue").disabled(busy).on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.pregen_op("continue", cx)
                        }))
                    })
                    .child(Button::new("pg-cancel").outline().small().icon(IconName::Close).label("Cancel").disabled(busy).on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.pregen_op("cancel", cx)
                    }))),
            )
            .child(widgets::caption(
                format!(
                    "{}{} chunks done · keeps going if you leave this page",
                    jar.map(|j| format!("{j} · ")).unwrap_or_default(),
                    fmt_thousands(pg["chunks"].as_u64().unwrap_or(0))
                ),
                cx,
            ))
        } else {
            let mut text = if state == "done" {
                format!("Done: {} chunks generated.", fmt_thousands(pg["chunks"].as_u64().unwrap_or(0)))
            } else if state == "cancelled" {
                "Cancelled.".to_string()
            } else if last_here {
                format!("Last run: radius {last_radius} blocks, {}.", hopper::time::relative_secs(last["at"].as_i64().unwrap_or(0)))
            } else {
                "Generate the land around spawn now, so players don't wait for it (and the server doesn't lag) later.".to_string()
            };
            if jar.is_none() {
                text.push_str(" Adds Chunky first.");
            }
            body.child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().min_w_0().text_color(theme.muted_foreground).child(text))
                    .child(Button::new("pg-start").outline().small().label("Pre-generate…").disabled(busy).on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.ask_pregen(window, cx)
                    }))),
            )
        };
        v_flex()
            .gap_2()
            .child(head)
            .child(body.id("pg-body").on_click(|_, _, cx| cx.stop_propagation()))
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
                .when_some(self.busy.clone(), |r, b| {
                    r.child(
                        h_flex()
                            .gap_2()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(gpui_kit::component::spinner::Spinner::new().small())
                            .child(b),
                    )
                })
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
                    Button::new("upload-world")
                        .outline()
                        .small()
                        .icon(IconName::Upload)
                        .label(if self.busy.as_deref().is_some_and(|b| b.starts_with("Uploading")) { "Uploading…" } else { "Upload world" })
                        .tooltip("Upload a world as a zip")
                        .disabled(self.busy.is_some())
                        .on_click(cx.listener(|this, _, window, cx| this.pick_upload(window, cx))),
                )
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
                    return r.child(
                        widgets::empty_state(
                            IconName::Globe,
                            "No worlds yet",
                            Some("The server makes one on its first start. Make one with New world, or upload a world to start from.".into()),
                            cx,
                        )
                        .child(
                            Button::new("upload-world-empty")
                                .outline()
                                .small()
                                .icon(IconName::Upload)
                                .label("Upload world")
                                .disabled(self.busy.is_some())
                                .on_click(cx.listener(|this, _, window, cx| this.pick_upload(window, cx))),
                        ),
                    );
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
