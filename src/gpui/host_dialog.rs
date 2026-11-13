//! Hosting a server on this PC: the New server dialog (from an instance or
//! from scratch), and the Java picker it shares with a server's Settings.

use crate::dialogs::CreateForm;
use crate::java_form::settings_form;
use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::form::Field;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::hosting::{CreateOptions, ServerLoader};
use hopper::java::{JavaInstall, Side};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// The width of the form's field column (the 44rem dialog less its label
/// column and padding). The form's rows don't shrink text to fit, so long
/// lines and boxes in it are held to this.
const FIELD_W: Rems = Rems(30.);

/// The Java a server should run: detected installs ranked for its
/// Minecraft version, the picked one, and a download when none fits.
pub fn java_field(
    store: &Entity<Store>,
    picked: Option<PathBuf>,
    game_version: Option<String>,
    rem: Pixels,
    on_pick: Rc<dyn Fn(PathBuf, &mut Window, &mut App)>,
    cx: &App,
) -> Field {
    let theme = cx.theme();
    let s = store.read(cx);
    let Some(javas) = s.javas() else {
        return Field::new()
            .label("Java")
            .child(widgets::caption("Looking for Java on this PC…", cx));
    };
    let gv = game_version.unwrap_or_else(|| "1.21.1".into());
    // java.exe and javaw.exe in one folder are the same Java.
    let same = |a: &Path, b: &Path| {
        hopper::hosting::script::console_java(a) == hopper::hosting::script::console_java(b)
    };
    let req = hopper::java::requirement(&gv, Side::Server);
    let ranked = hopper::java::rank(javas, &req, Side::Server);
    let best = ranked.iter().find(|r| r.fits).map(|r| r.install.exe.clone());
    let current = picked.clone().or(best.clone());
    let items: Vec<(String, bool)> = ranked
        .iter()
        .map(|r| {
            let star = if Some(&r.install.exe) == best.as_ref() { "  ★" } else { "" };
            let warn = if r.fits { "" } else { "  (won't run it)" };
            (
                format!("{}{star}{warn}", r.install.label()),
                current.as_ref().is_some_and(|c| same(&r.install.exe, c)),
            )
        })
        .collect();
    let exes: Vec<PathBuf> = ranked.iter().map(|r| r.install.exe.clone()).collect();
    let label = current
        .as_ref()
        .and_then(|c| javas.iter().find(|j| same(&j.exe, c)).map(JavaInstall::label).or_else(|| {
            // Not one Hopper found: show its path.
            Some(c.display().to_string())
        }))
        .unwrap_or_else(|| "Pick a Java".into());
    let pick = on_pick.clone();
    let reason = ranked
        .iter()
        .find(|r| current.as_ref().is_some_and(|c| same(&r.install.exe, c)))
        .and_then(|r| r.reasons.first().cloned())
        .unwrap_or_else(|| req.reason.clone());
    let none_fit = best.is_none();
    let major = req.major;
    Field::new()
        .label("Java")
        .description(reason)
        .child(
            h_flex()
                .gap_2()
                .child(div().w_72().child(widgets::field_picker(
                    "server-java",
                    label,
                    items,
                    rem,
                    Rc::new(move |ix, w, cx| {
                        if let Some(e) = exes.get(ix) {
                            pick(e.clone(), w, cx)
                        }
                    }),
                )))
                .children(crate::java_form::java_get(major, false, true, none_fit, cx)),
        )
        .when(none_fit, |f| {
            f.child(
                div()
                    .text_sm()
                    .text_color(theme.warning)
                    .child(format!("No Java {major} on this PC yet: download it first.")),
            )
        })
}

/// The best Java on this PC for a server of `game_version`, if one fits.
pub fn best_java(store: &Entity<Store>, game_version: &str, cx: &App) -> Option<PathBuf> {
    let javas = store.read(cx).javas()?;
    let req = hopper::java::requirement(game_version, Side::Server);
    hopper::java::rank(javas, &req, Side::Server)
        .into_iter()
        .find(|r| r.fits)
        .map(|r| r.install.exe)
}

/// What a server for a pack from Browse is built from.
#[derive(Clone)]
pub enum Build {
    /// The pack author's server files.
    ServerFiles(hopper::browse::ServerFiles),
    /// The client pack (this version of it), for packs without server
    /// files: client-only mods are left out.
    ClientPack(Box<hopper::cf::Mod>, Box<hopper::cf::File>),
}

/// A pack from Browse to host, and the pack version it builds from.
#[derive(Clone)]
pub struct PackFiles {
    pub build: Build,
    /// The pack's name.
    pub name: String,
    /// The pack version's name, e.g. "All the Mods 10-6.0".
    pub version: String,
    pub game_version: String,
    /// "NeoForge", "Fabric"…; empty when the site doesn't say.
    pub loader: String,
    /// What of this version Hopper may download (CurseForge), checked
    /// before the dialog opens.
    pub check: hopper::hosting::PackCheck,
    /// The pack's newest version when it has no server files yet (these
    /// files are older): the warning, and that version ready to build from
    /// the client pack.
    pub newer: Option<(String, Rc<PackFiles>)>,
    /// What building it downloads first (bytes): the server pack, or the
    /// pack zip (its mods come on top). 0: unknown.
    pub download: u64,
    /// This version's file id (CurseForge), to mark it in the version list.
    pub file_id: u64,
    /// The pack and its versions, newest first (CurseForge), for picking
    /// another version in the dialog. Empty when the site's list isn't known.
    pub pack: Option<Rc<hopper::cf::Mod>>,
    pub versions: Rc<Vec<hopper::cf::File>>,
}

impl PackFiles {
    /// From the pack version's file (see `browse::server_files_version`).
    pub fn new(build: Build, name: String, version: &hopper::cf::File) -> Self {
        let tags = &version.game_versions;
        let game_version = tags
            .iter()
            .filter(|v| v.starts_with(|c: char| c.is_ascii_digit()))
            .max_by(|a, b| hopper::scan::compare_versions(a, b))
            .cloned()
            .unwrap_or_default();
        let loader = tags
            .iter()
            .find(|t| matches!(t.as_str(), "NeoForge" | "Forge" | "Fabric" | "Quilt"))
            .cloned()
            .unwrap_or_default();
        let download = match &build {
            Build::ClientPack(..) => version.file_length,
            Build::ServerFiles(f) => f.file.as_ref().map_or(0, |f| f.file_length),
        };
        PackFiles {
            build,
            name,
            version: version.display_name.clone(),
            game_version,
            loader,
            check: Default::default(),
            newer: None,
            download,
            file_id: version.id,
            pack: None,
            versions: Rc::new(Vec::new()),
        }
    }

    /// The pack zip itself is website-only: only a zip the player
    /// downloads will do, unless Hopper's download window gets it.
    fn needs_zip(&self) -> bool {
        matches!(self.build, Build::ClientPack(..)) && self.check.pack.is_some()
    }

    /// [`Self::needs_zip`] with no download window to get it either.
    fn must_pick_zip(&self) -> bool {
        self.needs_zip() && !hopper::modpack::can_fetch_from_website()
    }
}

/// How a CurseForge pack version builds: from its server files when it
/// has them, else from the client pack.
fn build_for(m: &hopper::cf::Mod, f: &hopper::cf::File) -> Build {
    match f.server_pack_file_id.filter(|id| *id != 0 && f.has_server_files()) {
        Some(sp) => Build::ServerFiles(hopper::browse::ServerFiles { project: m.id, pack_file: f.id, server_file: sp, file: None }),
        None => Build::ClientPack(Box::new(m.clone()), Box::new(f.clone())),
    }
}

/// What a pack version's [`PackFiles`] needs from CurseForge: what Hopper
/// may download, and the size of what it downloads first (the server pack
/// when it builds from one). Blocking: call from the background.
fn look_up_version(m: &hopper::cf::Mod, f: &hopper::cf::File) -> (hopper::hosting::PackCheck, u64) {
    let c = check(m, f);
    let download = if c.download > 0 { c.download } else { f.file_length };
    (c, download)
}

/// [`hopper::hosting::check_pack`] for a pack version, logged and treated
/// as "all downloadable" when CurseForge can't be asked: the build then
/// fails or falls back on its own.
fn check(pack: &hopper::cf::Mod, f: &hopper::cf::File) -> hopper::hosting::PackCheck {
    hopper::hosting::check_pack(pack, f).unwrap_or_else(|e| {
        log::warn!("checking {} on CurseForge: {e:#}", pack.name);
        Default::default()
    })
}

/// What the server starts from.
#[derive(Clone)]
enum Source {
    /// An instance's game, loader, mods and configs.
    Instance(PathBuf),
    /// A fresh server of a loader and version.
    Fresh,
    /// A pack's server files, with no instance.
    Files(Rc<PackFiles>),
    /// The Modpack tab before a pack is picked (in Browse).
    Pack,
    /// A server pack zip or .mrpack on this PC.
    Zip(Option<PathBuf>),
    /// A server folder on this PC, copied into the servers folder.
    Folder(Option<PathBuf>),
}

/// What a picked zip holds, read when it's picked: its Minecraft and
/// loader (when it says) and a name for the server, or why it can't be used.
#[derive(Clone)]
struct ZipRead {
    path: PathBuf,
    mc: String,
    loader: String,
    name: String,
    error: Option<String>,
}

/// What the picked source will make, looked up before anything is made.
#[derive(Clone, Default)]
struct Plan {
    /// Still asking.
    loading: bool,
    /// The pack's server pack for the instance's version (CurseForge): its
    /// name and size. None: it has none (or isn't a CurseForge pack).
    server_pack: Option<(String, u64)>,
    /// Mods a server from the instance keeps, and leaves out as client-only.
    counts: Option<(usize, usize)>,
}

pub struct HostForm {
    store: Entity<Store>,
    /// Where it's made: None on this PC, else a paired Hopper Server's id.
    place: Option<String>,
    /// Players join by this name (Hopper Server's addresses); optional.
    address: Entity<InputState>,
    /// The server's name; empty: [`Self::default_name`].
    name: Entity<InputState>,
    /// The line in players' server lists; empty: the name.
    motd: Entity<InputState>,
    /// The default name shown in the empty Name field.
    name_hint: String,
    /// The port shown in the empty Port field ("Auto: 25567").
    port_hint: String,
    /// What the port clashes with: (where, port) it was asked for, and the answer.
    port_check: Option<((Option<String>, u16), hopper::hosting::ports::PortCheck)>,
    /// The machine the server goes on: its name, RAM in GB and free disk
    /// space in bytes (0: unknown), once known.
    host: Option<(Option<String>, String, f64, u64)>,
    /// The player moved the memory slider: their choice stays. Until then it
    /// follows the source's default (`hosting::default_memory_mb`).
    memory_touched: bool,
    /// What the picked source will make, looked up in the background: the
    /// key it was asked for (see [`Self::plan_key`]) and the answer.
    plan: Option<(String, Plan)>,
    /// Where players will join, for the place it was asked for: the port an
    /// empty Port box gets (panel), the machine's LAN address, and the
    /// panel's address domain when its addresses are on.
    join: Option<(Option<String>, Option<u16>, Option<String>, Option<String>)>,
    source: Source,
    base: Entity<CreateForm>,
    memory: Entity<SliderState>,
    port: Entity<InputState>,
    java: Option<PathBuf>,
    eula: bool,
    /// Build from the pack's official server pack when it has one.
    server_pack: bool,
    /// Start the server once it's made.
    start: bool,
    /// The Zip source's file, once read.
    zip_read: Option<ZipRead>,
    /// A zip the player downloaded from CurseForge for a website-only pack
    /// or server pack; the server is made from it instead.
    zip: Option<PathBuf>,
    _subscriptions: Vec<Subscription>,
}

impl HostForm {
    fn new(store: Entity<Store>, from: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        store.update(cx, |s, _| s.want_javas());
        let base = cx.new(|cx| CreateForm::for_server("New server", window, cx));
        let memory = cx.new(|_| SliderState::new().min(1.).max(32.).step(0.5).default_value(6.));
        let root = PathBuf::from(store.read(cx).hosted_root());
        let port = hopper::hosting::suggest_port(&root).to_string();
        let port = cx.new(|cx| InputState::new(window, cx).default_value(port));
        let address = cx.new(|cx| InputState::new(window, cx).placeholder("Optional, like survival"));
        let name = cx.new(|cx| InputState::new(window, cx));
        let motd = cx.new(|cx| InputState::new(window, cx).placeholder("Same as the name"));
        let subs = vec![
            cx.subscribe(&address, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&name, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&motd, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.observe(&base, |_, _, cx| cx.notify()),
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.subscribe(&memory, |this: &mut Self, _, _: &SliderEvent, cx| {
                this.memory_touched = true;
                cx.notify()
            }),
            cx.subscribe(&port, |this: &mut Self, _, _: &InputEvent, cx| {
                this.check_port(cx);
                cx.notify()
            }),
        ];
        let source = match from {
            Some(r) => Source::Instance(r),
            None => store
                .read(cx)
                .instances()
                .iter()
                .find(|i| ServerLoader::from_instance(&i.loader).is_ok())
                .map_or(Source::Fresh, |i| Source::Instance(i.root.clone())),
        };
        Self {
            store,
            place: None,
            address,
            name,
            motd,
            port_check: None,
            host: None,
            memory_touched: false,
            plan: None,
            join: None,
            name_hint: String::new(),
            port_hint: String::new(),
            source,
            base,
            memory,
            port,
            java: None,
            eula: false,
            server_pack: true,
            start: false,
            zip_read: None,
            zip: None,
            _subscriptions: subs,
        }
    }

    /// Whether the picked instance came from a CurseForge pack with a
    /// server pack.
    fn has_pack(&self, cx: &App) -> bool {
        match &self.source {
            Source::Instance(r) => self
                .store
                .read(cx)
                .instance(r)
                .is_some_and(hopper::hosting::may_have_server_pack),
            _ => false,
        }
    }

    /// The name a server from the picked source gets unless one is typed.
    fn default_name(&self, cx: &App) -> String {
        let name = match &self.source {
            Source::Instance(r) => self.store.read(cx).instance(r).map(|i| format!("{} server", i.name)).unwrap_or_default(),
            Source::Fresh => self.base.read(cx).choice(cx).map(|(n, _, _)| n).unwrap_or_default(),
            Source::Pack => String::new(),
            Source::Zip(_) => self.zip_read.as_ref().map(|z| z.name.clone()).unwrap_or_default(),
            Source::Folder(p) => p.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            Source::Files(p) => format!("{} server", p.name),
        };
        // On this PC a taken name gets " 2", " 3"...; a panel checks its own.
        match self.place {
            None if !name.is_empty() => hopper::hosting::free_server_name(Path::new(&self.store.read(cx).hosted_root()), &name),
            _ => name,
        }
    }

    /// The server's name: the typed one, else [`Self::default_name`].
    fn server_name(&self, cx: &App) -> String {
        match self.name.read(cx).value().trim() {
            "" => self.default_name(cx),
            n => n.to_string(),
        }
    }

    /// The MOTD: the typed one, else the name.
    fn motd_text(&self, cx: &App) -> String {
        match self.motd.read(cx).value().trim() {
            "" => self.server_name(cx),
            m => m.to_string(),
        }
    }

    /// The port typed, when it is one.
    fn port_value(&self, cx: &App) -> Option<u16> {
        self.port.read(cx).value().trim().parse().ok().filter(|p| *p > 0)
    }

    /// Where the server goes, for players: "this PC" or the panel's name.
    fn host_label(&self, cx: &App) -> String {
        match &self.place {
            None => "this PC".into(),
            Some(id) => self
                .host
                .as_ref()
                .filter(|(p, ..)| p.as_ref() == Some(id))
                .map(|(_, n, ..)| n.clone())
                .or_else(|| self.store.read(cx).settings().hopper_panels.iter().find(|p| &p.id == id).map(|p| p.name.clone()))
                .unwrap_or_else(|| "the panel".into()),
        }
    }

    /// Asks whether the port clashes with another server on the same machine
    /// (or anything listening there): here directly, on a panel through its
    /// port check (an older panel without one: the servers this PC knows there).
    fn check_port(&mut self, cx: &mut Context<Self>) {
        let Some(port) = self.port_value(cx) else {
            self.port_check = None;
            return;
        };
        let key = (self.place.clone(), port);
        if self.port_check.as_ref().is_some_and(|(k, _)| *k == key) {
            return;
        }
        let store = self.store.read(cx);
        let used: Vec<(String, u16)> = store
            .servers()
            .iter()
            .filter(|s| match &self.place {
                None => store.is_hosted(&s.root),
                Some(id) => s.remote.as_ref().is_some_and(|r| &r.panel == id),
            })
            .map(|s| (s.name.clone(), s.port()))
            .collect();
        let Some(id) = self.place.clone() else {
            self.port_check = Some((key, hopper::hosting::ports::check_here(port, &used)));
            return;
        };
        let client = store
            .settings()
            .hopper_panels
            .iter()
            .find(|p| p.id == id)
            .and_then(hopper::hopperd::Client::for_panel);
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move {
                    let asked = client.and_then(|c| c.get(&format!("/servers/port-check?port={port}")).ok());
                    match asked.and_then(|v| serde_json::from_value(v).ok()) {
                        Some(c) => c,
                        None => hopper::hosting::ports::check(port, &used, |_| true),
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                // Only when it's still the port and place asked about.
                if (this.place.clone(), this.port_value(cx)) == (key.0.clone(), Some(key.1)) {
                    this.port_check = Some((key, r));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Looks up the machine's name and RAM: this PC's, or the panel's.
    fn look_up_host(&mut self, cx: &mut Context<Self>) {
        let place = self.place.clone();
        if self.host.as_ref().is_some_and(|(p, ..)| *p == place) {
            return;
        }
        let client = place.as_ref().and_then(|id| {
            self.store
                .read(cx)
                .settings()
                .hopper_panels
                .iter()
                .find(|p| &p.id == id)
                .and_then(hopper::hopperd::Client::for_panel)
        });
        let root = PathBuf::from(self.store.read(cx).hosted_root());
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move {
                    match client {
                        None => Some((
                            "this PC".to_string(),
                            hopper::hosting::local_panel::total_memory() as f64,
                            hopper::hosting::disk_free(&root),
                        )),
                        Some(c) => c.get("/host").ok().map(|v| {
                            (
                                v["hostname"].as_str().unwrap_or("").to_string(),
                                v["mem_total"].as_f64().unwrap_or(0.0),
                                v["disk_free"].as_u64().unwrap_or(0),
                            )
                        }),
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                if this.place == place
                    && let Some((name, bytes, free)) = r.filter(|(_, b, _)| *b > 0.0)
                {
                    let name = if name.is_empty() { this.host_label(cx) } else { name };
                    this.host = Some((place, name, bytes / 1_073_741_824.0, free));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Moves the server to `place` (None: this PC): the port and RAM there.
    fn set_place(&mut self, place: Option<String>, cx: &mut Context<Self>) {
        self.place = place;
        self.check_port(cx);
        self.look_up_host(cx);
        self.look_up_join(cx);
    }

    /// Asks where players will join: on a panel, the port it would pick and
    /// its address domain; here, this PC's LAN address.
    fn look_up_join(&mut self, cx: &mut Context<Self>) {
        let place = self.place.clone();
        if self.join.as_ref().is_some_and(|(p, ..)| *p == place) {
            return;
        }
        let client = place.as_ref().and_then(|id| {
            self.store.read(cx).settings().hopper_panels.iter().find(|p| &p.id == id).and_then(hopper::hopperd::Client::for_panel)
        });
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move {
                    match client {
                        None => (None, hopper::hosting::ports::lan_ip(), None),
                        Some(c) => {
                            let p = c.get("/servers/port-check").ok();
                            let router = c.get("/router").ok();
                            let domain = router
                                .filter(|r| r["config"]["enabled"].as_bool() == Some(true))
                                .and_then(|r| r["config"]["domain"].as_str().map(str::to_string))
                                .filter(|d| !d.is_empty());
                            (
                                p.as_ref().and_then(|v| v["auto"].as_u64()).map(|n| n as u16),
                                p.as_ref().and_then(|v| v["lan"].as_str().map(str::to_string)),
                                domain,
                            )
                        }
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                if this.place == place {
                    this.join = Some((place, r.0, r.1, r.2));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The panel's address domain, when its server addresses are on.
    fn domain(&self) -> Option<&str> {
        self.join.as_ref().filter(|(p, ..)| *p == self.place).and_then(|(.., d)| d.as_deref())
    }

    /// The address players join by, as the panel saves it: the typed name
    /// (with the panel's domain when it has none), else, with addresses on,
    /// the server's name as a subdomain (as Hopper Server's form does).
    fn join_address(&self, cx: &App) -> String {
        let typed = self.address.read(cx).value().trim().to_lowercase();
        match (typed.as_str(), self.domain()) {
            ("", Some(d)) => {
                let slug = hopper::hosting::ports::slug(&self.server_name(cx));
                if slug.is_empty() { String::new() } else { format!("{slug}.{d}") }
            }
            ("", None) => String::new(),
            (t, Some(d)) if !t.contains('.') => format!("{t}.{d}"),
            (t, _) => t.to_string(),
        }
    }

    /// "Players join at …" for under the port.
    fn join_line(&self, cx: &App) -> Option<String> {
        let (_, auto, lan, _) = self.join.as_ref().filter(|(p, ..)| *p == self.place)?;
        let port = self.port_value(cx).or(*auto);
        let addr = self.join_address(cx);
        let at = if !addr.is_empty() {
            addr
        } else {
            format!("{}:{}", lan.as_ref()?, port?)
        };
        Some(format!("Players join at {at}."))
    }

    /// The game version the server will run.
    fn game_version(&self, cx: &App) -> Option<String> {
        match &self.source {
            Source::Instance(r) => self.store.read(cx).instance(r).map(|i| i.game_version.clone()),
            Source::Fresh => Some(self.base.read(cx).game_version().to_string()).filter(|v| !v.is_empty()),
            Source::Zip(_) => self.zip_read.as_ref().map(|z| z.mc.clone()).filter(|v| !v.is_empty()),
            Source::Pack | Source::Folder(_) => None,
            Source::Files(p) => Some(p.game_version.clone()).filter(|v| !v.is_empty()),
        }
    }

    /// Builds from another version of the Browse pack: looks up what Hopper
    /// may download of it and its size, then shows it.
    fn pick_version(&mut self, f: hopper::cf::File, cx: &mut Context<Self>) {
        let Source::Files(p) = &self.source else { return };
        let Some(m) = p.pack.clone() else { return };
        let (name, versions) = (p.name.clone(), p.versions.clone());
        let (m2, f2) = ((*m).clone(), f.clone());
        cx.spawn(async move |this, cx| {
            let (c, download) = cx.background_spawn(async move { look_up_version(&m2, &f2) }).await;
            this.update(cx, |this, cx| {
                let mut np = PackFiles::new(build_for(&m, &f), name, &f);
                np.check = c;
                np.download = download;
                np.pack = Some(m);
                np.versions = versions;
                this.source = Source::Files(Rc::new(np));
                this.zip = None;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Which source [`Plan`] answers for: the picked instance (its server
    /// pack and mod counts). Fresh servers and pack files need no lookup.
    fn plan_key(&self) -> Option<String> {
        match &self.source {
            Source::Instance(r) => Some(r.to_string_lossy().into_owned()),
            _ => None,
        }
    }

    /// The [`Plan`] for the picked source, when it's the one shown.
    fn plan(&self) -> Option<&Plan> {
        let key = self.plan_key()?;
        self.plan.as_ref().filter(|(k, _)| *k == key).map(|(_, p)| p)
    }

    /// Asks, in the background, what a server from the picked instance gets:
    /// the pack's server pack for its version (CurseForge) and how many mods
    /// it keeps and leaves out.
    fn look_up_plan(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.plan_key() else { return };
        if self.plan.as_ref().is_some_and(|(k, _)| *k == key) {
            return;
        }
        let Source::Instance(r) = &self.source else { return };
        let store = self.store.read(cx);
        let Some(inst) = store.instance(r).cloned() else { return };
        let (anyway, hidden) = (store.settings().sync_anyway.clone(), store.settings().sync_blacklist.clone());
        self.plan = Some((key.clone(), Plan { loading: true, ..Default::default() }));
        cx.spawn(async move |this, cx| {
            let plan = cx
                .background_spawn(async move {
                    let server_pack = if hopper::hosting::may_have_server_pack(&inst) {
                        hopper::hosting::find_server_pack(&inst)
                            .unwrap_or_else(|e| {
                                log::warn!("looking up {}'s server pack: {e:#}", inst.name);
                                None
                            })
                            .map(|f| (f.display_name.clone(), f.file_length))
                    } else {
                        None
                    };
                    let counts = hopper::hosting::count_server_mods(&inst, &anyway, &hidden);
                    Plan { loading: false, server_pack, counts: Some(counts) }
                })
                .await;
            this.update(cx, |this, cx| {
                if this.plan.as_ref().is_some_and(|(k, _)| *k == key) {
                    this.plan = Some((key, plan));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Whether the server from the picked source runs mods.
    fn modded(&self, cx: &App) -> bool {
        let loader = |l: &str| ServerLoader::from_instance(l).is_ok_and(|l| l.is_modded());
        match &self.source {
            Source::Instance(r) => self.store.read(cx).instance(r).is_some_and(|i| loader(&i.loader)),
            Source::Fresh => self.base.read(cx).choice(cx).is_some_and(|(_, _, l)| loader(&l)),
            Source::Zip(_) => self.zip_read.as_ref().is_none_or(|z| !z.loader.is_empty()),
            Source::Pack | Source::Folder(_) => true,
            Source::Files(p) => !p.loader.is_empty(),
        }
    }

    /// Whether the server will be built from the pack's server pack.
    fn uses_server_pack(&self) -> bool {
        self.server_pack && self.plan().is_some_and(|p| p.server_pack.is_some())
    }

    /// Roughly what making it downloads (bytes), when Hopper can tell: the
    /// server pack or pack zip. A fresh server's loader is small.
    fn download(&self) -> Option<u64> {
        match &self.source {
            Source::Instance(_) if self.uses_server_pack() => self.plan()?.server_pack.as_ref().map(|(_, n)| *n),
            Source::Instance(_) | Source::Fresh | Source::Pack | Source::Folder(_) => None,
            // Uploaded to a Hopper Server; read in place on this PC.
            Source::Zip(Some(z)) if self.place.is_some() => std::fs::metadata(z).ok().map(|m| m.len()),
            Source::Zip(_) => None,
            Source::Files(_) if self.zip.is_some() => None,
            Source::Files(p) => Some(p.download).filter(|n| *n > 0),
        }
    }

    /// Free space where the server goes, once known (0: unknown).
    fn free(&self) -> u64 {
        self.host.as_ref().filter(|(p, ..)| *p == self.place).map_or(0, |(.., free)| *free)
    }

    /// The memory to ask for (MB): the slider's, or 0 when the player left
    /// it on the default and the pack's own recommendation (read once its
    /// files are in) should decide.
    fn memory_mb(&self, cx: &App) -> u32 {
        let mb = (self.memory.read(cx).value().start() * 1024.0).round() as u32;
        if !self.memory_touched && matches!(self.source, Source::Files(_)) { 0 } else { mb }
    }

    /// What to ask a Hopper Server to make: the route and its body
    /// (`/servers/create`, or `/servers/create-from-pack` for a pack).
    fn panel_body(&self, cx: &App) -> Result<(&'static str, serde_json::Value), String> {
        if let Source::Files(p) = &self.source {
            return self.panel_pack_body(p, cx).map(|b| ("/servers/create-from-pack", b));
        }
        let store = self.store.read(cx);
        let (gv, loader) = match &self.source {
            Source::Instance(r) => {
                let inst = store.instance(r).ok_or("That instance is gone.")?;
                (inst.game_version.clone(), inst.loader.clone())
            }
            Source::Fresh => self.base.read(cx).choice(cx).map(|(_, g, l)| (g, l)).ok_or("Pick a version and loader.")?,
            Source::Pack => return Err("Find a modpack in Browse first.".into()),
            Source::Folder(_) => {
                return Err("A folder is added on this PC. On a Hopper Server, use its own New server › Folder.".into());
            }
            Source::Zip(_) => {
                // The zip is uploaded when Create is pressed; the panel reads it.
                let z = self.zip_read.as_ref().ok_or("Pick a zip.")?;
                if let Some(e) = &z.error {
                    return Err(e.clone());
                }
                let name = self.server_name(cx);
                if name.trim().is_empty() {
                    return Err("Name the server.".into());
                }
                let port = self.port.read(cx).value().trim().to_string();
                let port: Option<u16> = if port.is_empty() { None } else { Some(port.parse().map_err(|_| "The port must be a number.")?) };
                if !self.eula {
                    return Err("Agree to the Minecraft EULA to make a server.".into());
                }
                return Ok(("/servers/import-zip", serde_json::json!({
                    "name": name,
                    "memory_mb": self.memory_mb(cx),
                    "port": port,
                    "motd": self.motd_text(cx),
                    "address": self.join_address(cx),
                    "accept_eula": true,
                })));
            }
            Source::Files(_) => unreachable!("handled above"),
        };
        let name = self.server_name(cx);
        if name.trim().is_empty() {
            return Err("Name the server.".into());
        }
        let (kind, version) = ServerLoader::from_instance(&loader).map_err(|e| format!("{e:#}"))?.panel_kind();
        let port = self.port.read(cx).value().trim().to_string();
        let port: Option<u16> = if port.is_empty() { None } else { Some(port.parse().map_err(|_| "The port must be a number.")?) };
        if !self.eula {
            return Err("Agree to the Minecraft EULA to make a server.".into());
        }
        Ok(("/servers/create", serde_json::json!({
            "name": name,
            "mc_version": gv,
            "loader": kind,
            "loader_version": version,
            "memory_mb": self.memory_mb(cx),
            "port": port,
            "motd": self.motd_text(cx),
            "address": self.join_address(cx),
            "accept_eula": true,
        })))
    }

    /// The body of `/servers/create-from-pack` for a pack version: the panel
    /// uses its server files when it has them, else builds from the client
    /// pack, as this PC does.
    fn panel_pack_body(&self, p: &PackFiles, cx: &App) -> Result<serde_json::Value, String> {
        // The panel has no download window for a website-only pack.
        if p.needs_zip() {
            return Err(format!(
                "{} can only be downloaded from the CurseForge website, so a Hopper Server can't get it. Host it on this PC instead.",
                p.name
            ));
        }
        let (mr, project, file) = match &p.build {
            // Modrinth server files carry no CurseForge server pack id.
            Build::ServerFiles(f) => (f.server_file == 0 && hopper::modrinth::is_mr(f.project), f.project, f.pack_file),
            Build::ClientPack(pack, file) => (pack.modrinth.is_some(), pack.id, file.id),
        };
        // Modrinth's own ids: the numbers Hopper keys them by only map back
        // in this process.
        let id = |n: u64| if mr { hopper::modrinth::real_id(n).unwrap_or_default() } else { n.to_string() };
        let (project, file) = (id(project), id(file));
        if project.is_empty() || file.is_empty() {
            return Err(format!("Couldn't look up {} on Modrinth. Open it again from Browse.", p.name));
        }
        let port = self.port.read(cx).value().trim().to_string();
        let port: Option<u16> = if port.is_empty() { None } else { Some(port.parse().map_err(|_| "The port must be a number.")?) };
        if !self.eula {
            return Err("Agree to the Minecraft EULA to make a server.".into());
        }
        Ok(serde_json::json!({
            "source": if mr { "modrinth" } else { "curseforge" },
            "project": project,
            "file": file,
            "name": self.server_name(cx),
            // Null: the panel takes the pack's recommended memory.
            "memory_mb": Some(self.memory_mb(cx)).filter(|m| *m > 0),
            "motd": self.motd_text(cx),
            "port": port,
            "address": self.join_address(cx),
            "accept_eula": true,
        }))
    }

    /// Ready to create, or why not.
    fn problem(&self, cx: &App) -> Option<String> {
        let p = match &self.place {
            Some(_) => self.panel_body(cx).err(),
            None => self.options(cx).err(),
        };
        p.or_else(|| hopper::hosting::room_problem(self.download().unwrap_or(0), self.free(), &self.host_label(cx)))
    }

    fn options(&self, cx: &App) -> Result<CreateOptions, String> {
        let gv = self.game_version(cx).unwrap_or_default();
        match &self.source {
            Source::Pack => return Err("Find a modpack in Browse first.".into()),
            Source::Zip(None) => return Err("Pick a zip.".into()),
            Source::Zip(_) => match &self.zip_read {
                None => return Err("Reading the zip…".into()),
                Some(z) if z.error.is_some() => return Err(z.error.clone().unwrap_or_default()),
                _ => {}
            },
            Source::Folder(None) => return Err("Pick the server folder.".into()),
            _ => {}
        }
        // A folder keeps its own start script, and its own Java.
        let java = match &self.source {
            Source::Folder(_) => Some(PathBuf::from("java")),
            _ => self.java.clone().or_else(|| best_java(&self.store, &gv, cx)),
        }
        .ok_or("Pick a Java (or download one).")?;
        let store = self.store.read(cx);
        let root = PathBuf::from(store.hosted_root());
        if root.as_os_str().is_empty() {
            return Err("Set where hosted servers go in Settings.".into());
        }
        let mut o = match &self.source {
            Source::Instance(r) => {
                let inst = store.instance(r).ok_or("That instance is gone.")?;
                CreateOptions::for_instance(inst, &root, &java).map_err(|e| format!("{e:#}"))?
            }
            Source::Fresh => {
                let (name, gv, loader) =
                    self.base.read(cx).choice(cx).ok_or("Pick a version and loader.")?;
                let l = ServerLoader::from_instance(&loader).map_err(|e| format!("{e:#}"))?;
                CreateOptions::new(&name, &root, &gv, l, &java)
            }
            Source::Files(p) if p.must_pick_zip() && self.zip.is_none() => {
                return Err("Download the pack from CurseForge, then pick the zip.".into());
            }
            Source::Files(p) => {
                // The pack's own loader version replaces this once its
                // manifest is read.
                let l = match p.loader.as_str() {
                    "" => ServerLoader::Vanilla,
                    l => ServerLoader::from_instance(&format!("{}-", l.to_ascii_lowercase()))
                        .unwrap_or(ServerLoader::Vanilla),
                };
                CreateOptions::new(&format!("{} server", p.name), &root, &p.game_version, l, &java)
            }
            Source::Zip(_) => {
                let z = self.zip_read.as_ref().ok_or("Pick a zip.")?;
                let l = ServerLoader::from_instance(&z.loader).map_err(|e| format!("{e:#}"))?;
                CreateOptions::new(&z.name, &root, &z.mc, l, &java)
            }
            Source::Folder(Some(src)) => {
                if !src.join("server.properties").is_file() {
                    return Err("That folder has no server.properties, so it isn't a server folder.".into());
                }
                CreateOptions::new("", &root, "", ServerLoader::Vanilla, &java)
            }
            Source::Pack | Source::Folder(None) => unreachable!("checked above"),
        };
        o.name = self.server_name(cx);
        if o.name.trim().is_empty() {
            return Err("Name the server.".into());
        }
        o.motd = self.motd_text(cx);
        o.memory_mb = self.memory_mb(cx);
        o.port = self
            .port
            .read(cx)
            .value()
            .trim()
            .parse()
            .map_err(|_| "The port must be a number.")?;
        if o.folder().exists() {
            return Err(format!("A server called {} already exists here. Pick another name.", o.name));
        }
        if !self.eula {
            return Err("Agree to the Minecraft EULA to host a server.".into());
        }
        o.accept_eula = true;
        Ok(o)
    }

    /// Makes it on the Hopper Server: a job there, followed in Tasks. A
    /// server made from an instance is paired with it, ready for Sync.
    fn create_on_panel(form: &Entity<Self>, panel: &str, window: &mut Window, cx: &mut App) -> bool {
        let this = form.read(cx);
        let Ok((route, body)) = this.panel_body(cx) else { return false };
        let inst = match &this.source {
            Source::Instance(r) => Some(r.clone()),
            _ => None,
        };
        let store_entity = this.store.clone();
        let store = store_entity.read(cx);
        let Some(p) = store.settings().hopper_panels.iter().find(|p| p.id == panel).cloned() else { return false };
        let Some(client) = hopper::hopperd::Client::for_panel(&p) else {
            window.push_notification(
                gpui_kit::component::notification::Notification::error(format!("Pair with {} again first.", p.name)),
                cx,
            );
            return false;
        };
        let name = body["name"].as_str().unwrap_or("server").to_string();
        let motd = this.motd_text(cx);
        // From an instance its server-side mods and config go over too (or
        // the pack's server pack), as on this PC.
        let from = inst.as_ref().and_then(|r| store.instance(r).cloned()).map(|i| {
            let o = hopper::hosting::panel_create::PanelServer {
                name: name.clone(),
                motd: this.motd_text(cx),
                memory_mb: body["memory_mb"].as_u64().unwrap_or(6144) as u32,
                port: body["port"].as_u64().map(|p| p as u16),
                address: body["address"].as_str().unwrap_or("").to_string(),
                server_pack: this.server_pack && this.has_pack(cx),
                start: this.start,
            };
            (i, o, store.settings().sync_anyway.clone(), store.settings().sync_blacklist.clone())
        });
        let job = store.start_job(format!("Making {name} on {}", p.name));
        let job_id = job.id();
        store.track_making(crate::store::Making {
            job: job_id,
            name: name.clone(),
            place: Some(panel.to_string()),
            root: None,
            panel_id: None,
            start: this.start,
        });
        let mut body = body;
        body["start"] = serde_json::json!(this.start);
        // The Zip source's file goes up first; the panel makes the server from it.
        let zip_path = match &this.source {
            Source::Zip(Some(z)) => Some(z.clone()),
            _ => None,
        };
        let store = store_entity;
        cx.spawn(async move |cx| {
            let r = cx
                .background_spawn(async move {
                    let r = match from {
                        Some((i, o, anyway, hidden)) => {
                            hopper::hosting::panel_create::create_from_instance(&client, &i, &o, &anyway, &hidden, &job)
                                .map(|r| r.made)
                        }
                        None => (|| {
                            let mut body = body;
                            if let Some(z) = &zip_path {
                                job.detail("Uploading the zip");
                                let size = std::fs::metadata(z).map_or(0, |m| m.len()).max(1);
                                let up = client.upload_incoming(z, |sent| {
                                    job.progress(sent as usize, size as usize);
                                })?;
                                body["upload"] = serde_json::json!(up);
                            }
                            client.run_job(route, body, &job)
                        })()
                        .inspect(|made| {
                            // The pack route has no MOTD of its own.
                            hopper::hosting::panel_create::set_motd(&client, made, &motd, &name)
                        }),
                    };
                    job.finish(&r);
                    r.map(|v| (v, client.panel.clone()))
                })
                .await;
            if let Ok((v, panel)) = r {
                let id = v["id"].as_str().map(str::to_string);
                let folder = hopper::hosting::panel_create::PanelReport { made: v, content: None }
                    .folder()
                    .unwrap_or_default();
                store
                    .update(cx, |s, cx| {
                        if !folder.is_empty() {
                            let root = hopper::hopperd::panel_root(&panel, &folder);
                            s.made_on_panel(job_id, root.clone(), id);
                            if let Some(inst) = inst {
                                s.set_pairing(&root, &inst, cx);
                            }
                        }
                        s.rediscover_servers();
                        cx.notify();
                    });
            }
        })
        .detach();
        true
    }

    fn create(form: &Entity<Self>, window: &mut Window, cx: &mut App) -> bool {
        let this = form.read(cx);
        if let Some(panel) = this.place.clone() {
            return Self::create_on_panel(form, &panel, window, cx);
        }
        let opts = match this.options(cx) {
            Ok(o) => o,
            Err(_) => return false,
        };
        // A website-only pack or server pack comes through Hopper's download
        // window first; the dialog closes, so say where to follow it.
        let via_window = hopper::modpack::can_fetch_from_website()
            && matches!(&this.source, Source::Files(p) if this.zip.is_none() && (p.check.pack.is_some() || p.check.server_files.is_some()));
        let store = this.store.read(cx);
        // Its row in the server list learns where it went (and starts it).
        let done = store.made_here(this.start);
        let name = opts.name.clone();
        let job = match &this.source {
            Source::Files(p) if this.zip.is_some() => {
                let files = match &p.build {
                    Build::ServerFiles(f) => Some(f.clone()),
                    Build::ClientPack(..) => None,
                };
                store.create_hosted_from_zip(opts, this.zip.clone().unwrap(), files, done)
            }
            Source::Files(p) => match &p.build {
                Build::ServerFiles(files) => store.create_hosted_from_files(opts, files.clone(), done),
                Build::ClientPack(pack, file) => {
                    store.create_hosted_from_client_pack(opts, (**pack).clone(), (**file).clone(), done)
                }
            },
            Source::Instance(r) => {
                let pack = this.server_pack && this.has_pack(cx);
                store.create_hosted(opts, Some(r.clone()), pack, done)
            }
            Source::Fresh => store.create_hosted(opts, None, false, done),
            Source::Zip(Some(z)) => store.create_hosted_from_zip(opts, z.clone(), None, done),
            Source::Folder(Some(src)) => store.create_hosted_from_folder(opts, src.clone(), done),
            Source::Pack | Source::Zip(None) | Source::Folder(None) => return false,
        };
        store.track_making(crate::store::Making { job, name, place: None, root: None, panel_id: None, start: this.start });
        // A website-only pack goes through the download window first.
        if via_window {
            window.push_notification(
                gpui_kit::component::notification::Notification::info(
                    "Hopper's download window gets the pack from CurseForge first: follow it from the top bar or the server list.",
                ),
                cx,
            );
        }
        true
    }
}

impl HostForm {
    /// The notice for a website-only pack or server pack: what Hopper does
    /// instead, and a way to use the file anyway (its CurseForge page, then
    /// a picker for the downloaded zip).
    fn website_only_notice(&self, p: &PackFiles, cx: &mut Context<Self>) -> Option<AnyElement> {
        let window = hopper::modpack::can_fetch_from_website();
        let (text, url, button) = if p.needs_zip() {
            let w = p.check.pack.as_ref()?;
            (
                if window {
                    format!(
                        "{} can only be downloaded from the CurseForge website. Hopper opens its page in a download window to get {}.",
                        p.name, w.file_name
                    )
                } else {
                    format!(
                        "{} can only be downloaded from the CurseForge website. Download {} there, then pick the zip.",
                        p.name, w.file_name
                    )
                },
                w.url.clone(),
                if window { "Download it yourself" } else { "Download on CurseForge" },
            )
        } else if matches!(p.build, Build::ServerFiles(_)) {
            let w = p.check.server_files.as_ref()?;
            (
                if window {
                    "The pack's server files can only be downloaded from the CurseForge website. Hopper opens their page in a download window to get them, or builds the server from the pack itself if that fails.".to_string()
                } else {
                    "The pack's server files can't be downloaded outside CurseForge, so Hopper builds the server from the pack itself.".to_string()
                },
                w.url.clone(),
                "Use the server files anyway",
            )
        } else {
            return None;
        };
        let theme = cx.theme();
        let picked = self.zip.is_some();
        Some(
            v_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .p_2()
                .rounded(px(crate::theme::RADIUS))
                .bg(theme.warning.opacity(0.08))
                .text_sm()
                .child(div().w_full().min_w_0().child(text))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("host-website-only")
                                .outline()
                                .small()
                                .icon(IconName::ExternalLink)
                                .label(button)
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.open_url(&url);
                                    Self::pick_zip(cx);
                                })),
                        )
                        .child(
                            Button::new("host-pick-zip")
                                .ghost()
                                .small()
                                .label(if picked { "Pick another zip" } else { "I have the zip" })
                                .on_click(cx.listener(|_, _, _, cx| Self::pick_zip(cx))),
                        )
                        .when(picked, |r| {
                            r.child(
                                Button::new("host-zip-clear")
                                    .ghost()
                                    .small()
                                    .label("Don't use it")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.zip = None;
                                        cx.notify();
                                    })),
                            )
                        }),
                )
                .into_any_element(),
        )
    }

    /// Asks for the Zip source's file, then reads what it is.
    fn pick_source_zip(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Use this zip".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let p2 = path.clone();
            let read = cx
                .background_spawn(async move {
                    let stem = p2.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    match hopper::hosting::zip_import::inspect(&p2) {
                        Ok(i) => match i.target {
                            Some(t) => ZipRead { path: p2, mc: t.mc_version, loader: t.loader, name: i.name, error: None },
                            None => ZipRead {
                                path: p2,
                                mc: String::new(),
                                loader: String::new(),
                                name: i.name,
                                error: Some("Hopper can't tell which Minecraft and loader this zip is for. Make the server from its pack in Browse instead.".into()),
                            },
                        },
                        Err(e) => ZipRead { path: p2, mc: String::new(), loader: String::new(), name: stem, error: Some(format!("{e:#}")) },
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                if matches!(this.source, Source::Zip(_)) {
                    this.source = Source::Zip(Some(path));
                    this.zip_read = Some(read);
                    this.java = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Asks for the Folder source's server folder.
    fn pick_source_folder(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Use this folder".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            this.update(cx, |this, cx| {
                if matches!(this.source, Source::Folder(_)) {
                    this.source = Source::Folder(Some(path));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Asks for the zip the player downloaded from CurseForge.
    fn pick_zip(cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Use this zip".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            this.update(cx, |this, cx| {
                this.zip = Some(path);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for HostForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rem = window.rem_size();
        self.look_up_plan(cx);
        // Until the player moves it, memory follows the source: 2 GB for a
        // vanilla server, 6 GB for a modded one (a pack's own recommendation
        // is applied when its files are read).
        if !self.memory_touched {
            let gb = hopper::hosting::default_memory_mb(self.modded(cx), 0) as f32 / 1024.0;
            if (self.memory.read(cx).value().start() - gb).abs() > 0.01 {
                self.memory.update(cx, |s, cx| s.set_value(gb, window, cx));
            }
        }
        let hint = self.default_name(cx);
        if hint != self.name_hint {
            self.name.update(cx, |i, cx| i.set_placeholder(hint.clone(), window, cx));
            self.name_hint = hint;
        }
        let hint = match self.join.as_ref().filter(|(p, ..)| p.is_some() && *p == self.place).and_then(|(_, a, ..)| *a) {
            Some(a) => format!("Auto: {a}"),
            None => String::new(),
        };
        if hint != self.port_hint {
            self.port.update(cx, |i, cx| i.set_placeholder(hint.clone(), window, cx));
            self.port_hint = hint;
        }
        let theme = cx.theme().clone();
        let store = self.store.read(cx);
        let instances: Vec<(PathBuf, String)> = store
            .instances()
            .iter()
            .filter(|i| ServerLoader::from_instance(&i.loader).is_ok())
            .map(|i| (i.root.clone(), i.name.clone()))
            .collect();
        let from_instance = matches!(self.source, Source::Instance(_));
        let picked_name = match &self.source {
            Source::Instance(r) => instances.iter().find(|(p, _)| p == r).map(|(_, n)| n.clone()),
            _ => None,
        };
        let fresh = matches!(self.source, Source::Fresh);
        let folder_src = matches!(self.source, Source::Folder(_));
        // Tabs: what the server starts from. Folder is for this PC only (a
        // Hopper Server adds folders on its own machine).
        let mut tabs: Vec<(SharedString, Option<usize>)> =
            vec![("An instance".into(), None), ("Modpack".into(), None), ("Fresh".into(), None), ("Zip".into(), None)];
        if self.place.is_none() {
            tabs.push(("Folder".into(), None));
        }
        let tab_ix = match &self.source {
            Source::Instance(_) => 0,
            Source::Pack | Source::Files(_) => 1,
            Source::Fresh => 2,
            Source::Zip(_) => 3,
            Source::Folder(_) => 4,
        };
        let files = match &self.source {
            Source::Files(p) => Some(p.clone()),
            _ => None,
        };
        let on_panel = self.place.is_some();
        let view = cx.entity();
        let roots: Vec<PathBuf> = instances.iter().map(|(r, _)| r.clone()).collect();
        let source_field = Field::new()
            .label("Start from")
            .description(match &self.source {
                Source::Instance(_) => "Its loader, server-side mods, config, kubejs and datapacks. Client-only mods stay out.",
                Source::Pack => "A CurseForge or Modrinth pack: its server files if it has them, otherwise built from the client pack.",
                Source::Zip(_) => "A server pack zip or .mrpack on this PC.",
                Source::Folder(_) => "A server folder on this PC: a copy goes in the servers folder; the original stays as it is.",
                Source::Fresh | Source::Files(_) => "A fresh server: pick the game and server type.",
            })
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        // What the dialog shows depends on it: tabs, not a
                        // value picker (see widgets::underline_tabs).
                        widgets::underline_tabs(
                            "host-source",
                            tabs,
                            tab_ix,
                            cx.listener(|this, ix: &usize, _, cx| {
                                let first = this
                                    .store
                                    .read(cx)
                                    .instances()
                                    .iter()
                                    .find(|i| ServerLoader::from_instance(&i.loader).is_ok())
                                    .map(|i| i.root.clone());
                                this.source = match (*ix, first) {
                                    (0, Some(r)) => Source::Instance(r),
                                    (1, _) => Source::Pack,
                                    (3, _) => Source::Zip(None),
                                    (4, _) => Source::Folder(None),
                                    _ => Source::Fresh,
                                };
                                this.zip_read = None;
                                this.java = None;
                                cx.notify();
                            }),
                            cx,
                        ),
                    )
                    .when(from_instance, |c| {
                        c.child(widgets::field_picker(
                            "host-instance",
                            picked_name.unwrap_or_else(|| "Pick an instance".into()),
                            instances.iter().map(|(r, n)| (n.clone(), Some(r) == match &self.source {
                                Source::Instance(p) => Some(p),
                                _ => None,
                            })).collect(),
                            rem,
                            Rc::new(move |ix, _, cx| {
                                if let Some(r) = roots.get(ix) {
                                    view.update(cx, |this, cx| {
                                        this.source = Source::Instance(r.clone());
                                        this.java = None;
                                        cx.notify();
                                    });
                                }
                            }),
                        ))
                    })
                    .when(matches!(self.source, Source::Pack), |c| {
                        let (store, place) = (self.store.clone(), self.place.clone());
                        c.child(
                            h_flex()
                                .gap_2()
                                .flex_wrap()
                                .child(
                                    Button::new("host-find-pack")
                                        .outline()
                                        .icon(IconName::Compass)
                                        .label("Find a modpack…")
                                        .on_click(move |_, window, cx| {
                                            // Browse's Make a server opens this form again, with the pack.
                                            store.update(cx, |_, cx| cx.emit(crate::store::StoreEvent::BrowsePacksFor(place.clone())));
                                            window.close_dialog(cx);
                                        }),
                                )
                                .child(widgets::caption("Its Make a server button brings it back here.", cx).text_xs()),
                        )
                    })
                    .when(matches!(self.source, Source::Zip(_)), |c| {
                        let read = self.zip_read.clone();
                        c.child(
                            v_flex()
                                .w_full()
                                .max_w(FIELD_W)
                                .gap_1p5()
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .child(
                                            Button::new("host-pick-zip")
                                                .outline()
                                                .icon(IconName::FileArchive)
                                                .label(if read.is_some() { "Pick another zip…" } else { "Pick a zip…" })
                                                .on_click(cx.listener(|this, _, _, cx| this.pick_source_zip(cx))),
                                        )
                                        .children(read.as_ref().map(|z| {
                                            widgets::caption(z.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), cx)
                                                .text_xs()
                                                .truncate()
                                        })),
                                )
                                .children(read.map(|z| match z.error {
                                    Some(e) => div().text_sm().text_color(theme.danger).child(e),
                                    None => widgets::caption(
                                        format!(
                                            "Minecraft {}{}",
                                            z.mc,
                                            if z.loader.is_empty() { String::new() } else { format!(" · {}", z.loader) }
                                        ),
                                        cx,
                                    ),
                                })),
                        )
                    })
                    .when(folder_src, |c| {
                        let picked = match &self.source {
                            Source::Folder(Some(p)) => Some(p.display().to_string()),
                            _ => None,
                        };
                        c.child(
                            h_flex()
                                .w_full()
                                .max_w(FIELD_W)
                                .gap_2()
                                .child(
                                    Button::new("host-pick-folder")
                                        .outline()
                                        .icon(IconName::FolderOpen)
                                        .label(if picked.is_some() { "Pick another folder…" } else { "Pick a folder…" })
                                        .on_click(cx.listener(|this, _, _, cx| this.pick_source_folder(cx))),
                                )
                                .children(picked.map(|p| widgets::caption(p, cx).text_xs().truncate().min_w_0())),
                        )
                    }),
            );
        // From a pack's server files: no instance to pick.
        let source_field = match &files {
            Some(p) => Field::new()
                .label("Start from")
                .description({
                    let on = format!(
                        "{} {}",
                        if p.loader.is_empty() { "Minecraft" } else { p.loader.as_str() },
                        p.game_version
                    );
                    match (&self.zip, &p.build) {
                        (_, Build::ServerFiles(_)) if on_panel => format!(
                            "The pack author's server mods and configs, on {on}."
                        ),
                        _ if on_panel => format!(
                            "Built from the client pack on {on}, client-only mods left out."
                        ),
                        (Some(z), _) => format!(
                            "From {}, the zip you downloaded.",
                            z.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                        ),
                        (None, _) if p.must_pick_zip() => format!(
                            "Made from the pack's zip once you download it, on {on}; client-only mods left out."
                        ),
                        (None, Build::ServerFiles(_)) if hopper::modpack::can_fetch_from_website() => format!(
                            "The pack author's server mods and configs, on {on}. Often a big download."
                        ),
                        (None, Build::ServerFiles(_)) if p.check.server_files.is_none() => format!(
                            "The pack author's server mods and configs, on {on}. Often a big download."
                        ),
                        (None, _) => format!(
                            "Built from the client pack on {on}, client-only mods left out."
                        ),
                    }
                })
                .child(
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .gap_2()
                        .child(
                            h_flex()
                                .gap_2()
                                .text_sm()
                                .child(Icon::new(IconName::Server).small())
                                .child(div().font_semibold().child(p.name.clone()))
                                // The same chip as Browse's card.
                                .child(match &p.build {
                                    Build::ServerFiles(_) => widgets::chip(cx.theme().success).child("Server files"),
                                    Build::ClientPack(..) => widgets::chip(cx.theme().warning).child("From client pack"),
                                })
                                .when(p.versions.is_empty(), |r| r.child(widgets::caption(p.version.clone(), cx))),
                        )
                        .when(!p.versions.is_empty(), |c| {
                            let view = cx.entity();
                            let list = p.versions.clone();
                            c.child(widgets::field_picker(
                                "host-version",
                                p.version.clone(),
                                list.iter()
                                    .map(|f| {
                                        let tag = if f.has_server_files() { " · server files" } else { "" };
                                        (format!("{}{tag}", f.display_name), f.id == p.file_id)
                                    })
                                    .collect(),
                                rem,
                                Rc::new(move |ix, _, cx| {
                                    if let Some(f) = list.get(ix).cloned() {
                                        view.update(cx, |this, cx| this.pick_version(f, cx));
                                    }
                                }),
                            ))
                        })
                        .when(!on_panel, |c| c.children(self.website_only_notice(p, cx)))
                        .children(p.newer.clone().map(|(warning, alt)| {
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .gap_2()
                                .p_2()
                                .rounded(px(crate::theme::RADIUS))
                                .bg(cx.theme().warning.opacity(0.08))
                                .text_sm()
                                .child(div().flex_1().min_w_0().child(warning))
                                .child(
                                    Button::new("host-build-newer")
                                        .outline()
                                        .small()
                                        .label(format!("Build {} from the client pack", alt.version))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.source = Source::Files(alt.clone());
                                            this.zip = None;
                                            cx.notify();
                                        })),
                                )
                        })),
                ),
            None => source_field,
        };
        // What the server gets, by case: a choice only when the pack has
        // server files for this version; otherwise what Hopper does instead.
        let pack_field = match (&self.source, self.plan().cloned()) {
            (Source::Instance(r), Some(plan)) => {
                let inst = self.store.read(cx).instance(r).cloned();
                let curse = inst.as_ref().is_some_and(hopper::hosting::may_have_server_pack);
                let body = v_flex().w_full().max_w(FIELD_W).gap_1p5().text_sm();
                let body = if plan.loading {
                    body.child(widgets::caption("Checking the pack's server files…", cx))
                } else if let Some((name, size)) = plan.server_pack.clone() {
                    body.child(
                        Checkbox::new("host-server-pack")
                            .label(format!("Use the author's server files ({name})"))
                            .checked(self.server_pack)
                            .on_change(cx.listener(|this, v: &bool, _, cx| {
                                this.server_pack = *v;
                                cx.notify();
                            })),
                    )
                    .child(widgets::caption(
                        format!(
                            "About {}. Untick to build from your instance instead: the mods you play with, client-only ones left out.",
                            hopper::instance_ops::human_bytes(size)
                        ),
                        cx,
                    ))
                } else {
                    body.child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(widgets::chip(theme.warning).child("From your instance"))
                            .child(widgets::caption(
                                if curse {
                                    "No server pack for this version, so Hopper builds it from your instance."
                                } else {
                                    "Hopper builds it from your instance."
                                },
                                cx,
                            )),
                    )
                };
                let counts = plan.counts.filter(|_| !self.uses_server_pack());
                let body = body.children(counts.map(|(keep, left)| {
                    let mods = |n: usize| if n == 1 { "1 mod".to_string() } else { format!("{n} mods") };
                    widgets::caption(format!("Keeps {} · leaves out {left} client-only", mods(keep)), cx)
                }));
                // Every mod is client-only: the server would be plain Minecraft.
                let body = body.children(counts.filter(|(k, l)| *k == 0 && *l > 0).zip(inst).map(|((_, l), i)| {
                    div().text_color(theme.warning).child(format!(
                        "All {l} mods are client-only, so this server would be plain Minecraft {}.",
                        i.game_version
                    ))
                }));
                Some(Field::new().label("Server files").child(body))
            }
            _ => None,
        };
        let gb = self.memory.read(cx).value().start();
        let view = cx.entity();
        let java = java_field(
            &self.store,
            self.java.clone(),
            self.game_version(cx),
            rem,
            Rc::new(move |p: PathBuf, _: &mut Window, cx: &mut App| {
                view.update(cx, |this, cx| {
                    this.java = Some(p);
                    cx.notify();
                })
            }),
            cx,
        );
        let problem = self.problem(cx);
        let hosted_root = self.store.read(cx).hosted_root();
        let host = self.host_label(cx);
        // What Create will do, in one line: download, room, RCON, mods.
        let summary = {
            let mut parts = Vec::new();
            match self.download() {
                Some(n) => parts.push(format!("About {} to download", hopper::instance_ops::human_bytes(n))),
                None if matches!(self.source, Source::Fresh) => parts.push("Downloads the server".to_string()),
                None => {}
            }
            match self.free() {
                0 => {}
                n => parts.push(format!("{} free on {host}", hopper::instance_ops::human_bytes(n))),
            }
            parts.push("RCON on".to_string());
            let client_pack = matches!(&self.source, Source::Files(p) if matches!(p.build, Build::ClientPack(..)));
            if (client_pack || matches!(self.source, Source::Instance(_) | Source::Zip(_))) && !self.uses_server_pack() {
                parts.push("client-only mods stay out".to_string());
            }
            parts.join(" · ")
        };
        // "of 48 GB on rig", and a word when that leaves the machine little.
        let host_ram = self.host.as_ref().filter(|(p, ..)| *p == self.place).map(|(_, n, total, _)| {
            (
                format!("of {total:.0} GB on {n}"),
                (gb as f64 > total * 0.75).then(|| format!("That leaves little for {n} and its other servers.")),
            )
        });
        let port_warning = self
            .port_check
            .as_ref()
            .filter(|((p, port), _)| *p == self.place && self.port_value(cx) == Some(*port))
            .and_then(|((_, port), c)| Some((c.message(*port, &host)?, c.suggest)))
            .map(|(msg, suggest)| {
                // The form's field column doesn't shrink text to fit, so the
                // box is held to the column's width (the dialog is 44rem).
                h_flex()
                    .w_full()
                    .max_w(FIELD_W)
                    .min_w_0()
                    .gap_2()
                    .p_2()
                    .rounded(px(crate::theme::RADIUS))
                    .bg(theme.warning.opacity(0.08))
                    .text_sm()
                    .items_start()
                    .child(div().flex_1().min_w_0().child(msg))
                    .when_some(suggest, |r, s| {
                        r.child(
                            Button::new("host-port-free")
                                .flex_none()
                                .outline()
                                .small()
                                .label(format!("Use {s}"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.port.update(cx, |i, cx| i.set_value(s.to_string(), window, cx));
                                    this.check_port(cx);
                                    cx.notify();
                                })),
                        )
                    })
            });
        // This PC, then each paired Hopper Server.
        let panels: Vec<(String, String)> = self
            .store
            .read(cx)
            .settings()
            .hopper_panels_on()
            .map(|p| (p.id.clone(), p.name.clone()))
            .collect();
        let place_field = (!panels.is_empty()).then(|| {
            let current = match &self.place {
                Some(id) => panels.iter().find(|(i, _)| i == id).map_or("Hopper Server".to_string(), |(_, n)| format!("{n} · Hopper Server")),
                None => "This PC".to_string(),
            };
            let mut items = vec![("This PC".to_string(), self.place.is_none())];
            items.extend(panels.iter().map(|(i, n)| (format!("{n} · Hopper Server"), self.place.as_ref() == Some(i))));
            let ids: Vec<Option<String>> = std::iter::once(None).chain(panels.iter().map(|(i, _)| Some(i.clone()))).collect();
            let view = cx.entity();
            Field::new()
                .label("Where")
                .description(if on_panel && files.is_some() {
                    "The panel downloads the pack, the server and Java itself."
                } else if on_panel && from_instance {
                    "Hopper sends the instance's server files there; the panel gets the loader and Java itself."
                } else if on_panel {
                    "The panel downloads the server and Java itself."
                } else {
                    "Runs on this PC while Hopper is open."
                })
                .child(widgets::field_picker(
                    "host-place",
                    current,
                    items,
                    rem,
                    Rc::new(move |ix, window, cx| {
                        if let Some(p) = ids.get(ix) {
                            let p = p.clone();
                            view.update(cx, |this, cx| {
                                if p != this.place {
                                    // A panel picks a free port itself; this PC suggests one.
                                    let port = match &p {
                                        Some(_) => String::new(),
                                        None => hopper::hosting::suggest_port(Path::new(&this.store.read(cx).hosted_root())).to_string(),
                                    };
                                    this.port.update(cx, |i, cx| i.set_value(port, window, cx));
                                }
                                this.set_place(p, cx);
                                cx.notify();
                            });
                        }
                    }),
                ))
        });
        // The form scrolls when the window is short; what Create will do
        // (and why it can't yet) stays in sight above the buttons.
        let fields = v_flex()
            .id("host-form")
            .gap_4()
            .max_h(widgets::dialog_room(window) - rem * 11.)
            .overflow_y_scroll()
            .child(settings_form(rem).children(place_field).child(source_field).children(pack_field))
            .when(fresh, |c| c.child(self.base.clone()))
            // A modpack is picked in Browse first: nothing to fill in yet.
            .when(!matches!(self.source, Source::Pack), |c| c.child(
                settings_form(rem)
                    .when(!fresh, |f| {
                        f.child(
                            Field::new()
                                .label("Name")
                                .child(div().w_80().child(Input::new(&self.name))),
                        )
                    })
                    .child(
                        Field::new()
                            .label("MOTD")
                            .description("The line players see under the server in their list.")
                            .child(div().w_80().child(Input::new(&self.motd))),
                    )
                    .child(
                        Field::new()
                            .label("Memory")
                            .when_some(host_ram.clone().and_then(|(_, w)| w), |f, w| f.description(w))
                            .child(
                                h_flex()
                                    .gap_3()
                                    .child(div().w_56().child(Slider::new(&self.memory)))
                                    .child(div().text_sm().child(format!("{gb:.1} GB")))
                                    .when_some(host_ram.map(|(of, _)| of), |r, of| {
                                        r.child(div().text_sm().text_color(theme.muted_foreground).child(of))
                                    }),
                            ),
                    )
                    .child(
                        Field::new()
                            .label("Port")
                            .description_fn({
                                let auto = self.join.as_ref().filter(|(p, ..)| *p == self.place).and_then(|(_, a, ..)| *a);
                                let join = self.join_line(cx).map(|j| format!(" {j}")).unwrap_or_default();
                                let text = match (on_panel, auto) {
                                    (true, Some(a)) => format!("Empty: the panel uses {a}.{join}"),
                                    (true, None) => format!("Empty: the panel picks a free one.{join}"),
                                    (false, _) => format!("{join} Forward the port on your router for friends outside your network.").trim_start().to_string(),
                                };
                                move |_, _| div().max_w(FIELD_W).child(text.clone())
                            })
                            .child(
                                v_flex()
                                    .w_full()
                                    .min_w_0()
                                    .gap_1p5()
                                    .child(div().w_32().child(Input::new(&self.port)))
                                    .children(port_warning),
                            ),
                    )
                    .when(on_panel, |f| {
                        f.child(
                            Field::new()
                                .label("Address")
                                .description(match self.domain() {
                                    Some(d) => format!("A name players join by, under {d}. Empty: the server's name."),
                                    None => "A name players join by, when the panel's server addresses are on.".to_string(),
                                })
                                .child(div().w_56().child(Input::new(&self.address))),
                        )
                    })
                    .when(!on_panel && !folder_src, |f| f.child(java))
                    .child(
                        Field::new().label("EULA").child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Checkbox::new("host-eula")
                                        .label("I agree to the Minecraft EULA")
                                        .checked(self.eula)
                                        .on_change(cx.listener(|this, v: &bool, _, cx| {
                                            this.eula = *v;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("host-eula-read")
                                        .ghost()
                                        .xsmall()
                                        .label("Read it")
                                        .on_click(|_, _, cx| cx.open_url(hopper::hosting::EULA_URL)),
                                ),
                        ),
                    )
                    .child(
                        Field::new().child(
                            Checkbox::new("host-start")
                                .label("Start it when it's ready")
                                .checked(self.start)
                                .on_change(cx.listener(|this, v: &bool, _, cx| {
                                    this.start = *v;
                                    cx.notify();
                                })),
                        ),
                    ),
            ))
            ;
        v_flex()
            .gap_2()
            .child(fields)
            .child(
                v_flex()
                    .gap_1()
                    .pt_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .when(!on_panel, |c| c.child(widgets::caption(format!("Goes in {hosted_root}"), cx).text_xs().truncate()))
                    .child(widgets::caption(summary, cx).text_xs())
                    .when_some(problem, |c, p| c.child(div().text_sm().text_color(theme.muted_foreground).child(p))),
            )
    }
}

/// Opens the New server dialog, starting from `from` if given.
/// The New server dialog, making it on a paired Hopper Server (`panel`, by
/// id) or, with None, asking where.
pub fn open_on(store: Entity<Store>, panel: Option<String>, from: Option<PathBuf>, window: &mut Window, cx: &mut App) {
    let form = cx.new(|cx| {
        let mut f = HostForm::new(store, from, window, cx);
        if panel.is_some() {
            // The panel picks a free port.
            f.port.update(cx, |p, cx| p.set_value("", window, cx));
        }
        f.set_place(panel, cx);
        f
    });
    open_form(form, window, cx);
}

pub fn open(store: Entity<Store>, from: Option<PathBuf>, window: &mut Window, cx: &mut App) {
    let form = cx.new(|cx| {
        let mut f = HostForm::new(store, from, window, cx);
        f.set_place(None, cx);
        f
    });
    open_form(form, window, cx);
}

/// Opens the New server dialog for a pack's server files (from Browse):
/// the server is built from them alone, no instance needed. On a paired
/// Hopper Server (`panel`, by id) when given, else on this PC.
pub fn open_files(store: Entity<Store>, pack: PackFiles, panel: Option<String>, window: &mut Window, cx: &mut App) {
    let form = cx.new(|cx| {
        let mut f = HostForm::new(store, None, window, cx);
        f.source = Source::Files(Rc::new(pack));
        if panel.is_some() {
            // The panel picks a free port.
            f.port.update(cx, |p, cx| p.set_value("", window, cx));
        }
        f.set_place(panel, cx);
        f
    });
    open_form(form, window, cx);
}

/// Looks up the pack version `files` go with, then opens [`open_files`].
pub fn open_for_files(
    store: Entity<Store>,
    files: hopper::browse::ServerFiles,
    name: String,
    panel: Option<String>,
    window: &mut Window,
    cx: &mut App,
) {
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let f2 = files.clone();
        let r = cx
            .background_spawn(async move {
                let v = hopper::browse::server_files_version(&f2)?;
                // Modrinth server zips are always downloadable.
                let m = match f2.file {
                    Some(_) => None,
                    None => hopper::cf::get_mod(f2.project).ok(),
                };
                let c = m.as_ref().map(|m| check(m, &v)).unwrap_or_default();
                // What it downloads first: the server pack.
                let download = match &f2.file {
                    Some(f) => f.file_length,
                    None => c.download,
                };
                // The pack's versions (CurseForge), for the dialog's Version list.
                let versions: Vec<hopper::cf::File> = m
                    .as_ref()
                    .and_then(|m| hopper::source::files(m.id, "", hopper::cf::Loader::Any).ok())
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|f| f.is_server_pack != Some(true))
                    .take(50)
                    .collect();
                // A newer version without server files (CurseForge): say so,
                // and have it ready to build from the client pack.
                let newer = m.as_ref().and_then(|m| {
                    let rows: Vec<(&str, bool)> = versions
                        .iter()
                        .map(|f| (f.display_name.as_str(), f.has_server_files()))
                        .collect();
                    let view = hopper::hosting::pack_versions::view(&rows);
                    let newest = versions.get(view.newest_client?)?.clone();
                    let c = check(m, &newest);
                    Some((view.warning?, newest, c))
                });
                anyhow::Ok((v, c, newer, download, m, versions))
            })
            .await;
        handle
            .update(cx, |_, window, cx| match r {
                Ok((v, c, newer, download, m, versions)) => {
                    let (m, versions) = (m.map(Rc::new), Rc::new(versions));
                    let mut p = PackFiles::new(Build::ServerFiles(files), name.clone(), &v);
                    p.check = c;
                    p.download = download;
                    p.pack = m.clone();
                    p.versions = versions.clone();
                    p.newer = newer.zip(m.clone()).map(|((warning, f, c), m)| {
                        let mut alt = PackFiles::new(Build::ClientPack(Box::new((*m).clone()), Box::new(f.clone())), name, &f);
                        alt.check = c;
                        alt.pack = Some(m);
                        alt.versions = versions;
                        (warning, Rc::new(alt))
                    });
                    open_files(store, p, panel, window, cx)
                }
                Err(e) => window.push_notification(
                    gpui_kit::component::notification::Notification::error(format!(
                        "Couldn't look up {name}: {e:#}"
                    )),
                    cx,
                ),
            })
            .ok();
    })
    .detach();
}

/// Looks up the newest version of `pack` (a modpack without server
/// files), then opens [`open_files`] to build a server from the client pack.
pub fn open_for_client_pack(
    store: Entity<Store>,
    pack: hopper::cf::Mod,
    panel: Option<String>,
    window: &mut Window,
    cx: &mut App,
) {
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let p2 = pack.clone();
        let r = cx
            .background_spawn(async move {
                let f = hopper::browse::newest_pack_file(&p2)?;
                let c = check(&p2, &f);
                // The pack's versions (CurseForge), for the Version list.
                let versions: Vec<hopper::cf::File> = if p2.modrinth.is_some() {
                    Vec::new()
                } else {
                    hopper::source::files(p2.id, "", hopper::cf::Loader::Any)
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|f| f.is_server_pack != Some(true))
                        .take(50)
                        .collect()
                };
                anyhow::Ok((f, c, versions))
            })
            .await;
        handle
            .update(cx, |_, window, cx| match r {
                Ok((f, c, versions)) => {
                    let name = pack.name.clone();
                    let rc = Rc::new(pack.clone());
                    let build = Build::ClientPack(Box::new(pack), Box::new(f.clone()));
                    let mut p = PackFiles::new(build, name, &f);
                    p.check = c;
                    if !versions.is_empty() {
                        p.pack = Some(rc);
                        p.versions = Rc::new(versions);
                    }
                    open_files(store, p, panel, window, cx)
                }
                Err(e) => window.push_notification(
                    gpui_kit::component::notification::Notification::error(format!(
                        "Couldn't look up {}: {e:#}",
                        pack.name
                    )),
                    cx,
                ),
            })
            .ok();
    })
    .detach();
}

fn open_form(form: Entity<HostForm>, window: &mut Window, cx: &mut App) {

    window.open_dialog(cx, move |dialog, window, cx| {
        let f2 = form.clone();
        let ready = form.read(cx).problem(cx).is_none();
        // One title wherever it goes: "Where" says the rest.
        let title = "New server";
        dialog
            .title(title)
            .w(rems(44.).to_pixels(window.rem_size()))
            .margin_top(rems(widgets::DIALOG_TOP).to_pixels(window.rem_size()))
            .child(form.clone())
            .footer(crate::dialogs::footer("Create server", ready))
            .on_ok(move |_, window, cx| HostForm::create(&f2, window, cx))
    });
}

/// Whether a folder is somewhere Hopper hosts servers.
#[allow(dead_code)]
pub fn under(root: &Path, hosted_root: &str) -> bool {
    !hosted_root.is_empty() && root.starts_with(hosted_root)
}
