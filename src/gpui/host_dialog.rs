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
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::hosting::{CreateOptions, ServerLoader};
use hopper::java::{JavaInstall, Side};
use std::path::{Path, PathBuf};
use std::rc::Rc;

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
                .child(div().w_72().child(widgets::picker(
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
        PackFiles {
            build,
            name,
            version: version.display_name.clone(),
            game_version,
            loader,
            check: Default::default(),
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
    /// What the port clashes with: (where, port) it was asked for, and the answer.
    port_check: Option<((Option<String>, u16), hopper::hosting::ports::PortCheck)>,
    /// The machine the server goes on: its name and RAM in GB, once known.
    host: Option<(Option<String>, String, f64)>,
    source: Source,
    base: Entity<CreateForm>,
    memory: Entity<SliderState>,
    port: Entity<InputState>,
    java: Option<PathBuf>,
    eula: bool,
    /// Build from the pack's official server pack when it has one.
    server_pack: bool,
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
            cx.subscribe(&memory, |_, _, _: &SliderEvent, cx| cx.notify()),
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
                .first()
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
            name_hint: String::new(),
            source,
            base,
            memory,
            port,
            java: None,
            eula: false,
            server_pack: true,
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
                .is_some_and(|i| i.pack.is_some()),
            Source::Fresh | Source::Files(_) => false,
        }
    }

    /// The name a server from the picked source gets unless one is typed.
    fn default_name(&self, cx: &App) -> String {
        match &self.source {
            Source::Instance(r) => self.store.read(cx).instance(r).map(|i| format!("{} server", i.name)).unwrap_or_default(),
            Source::Fresh => self.base.read(cx).choice(cx).map(|(n, _, _)| n).unwrap_or_default(),
            Source::Files(p) => format!("{} server", p.name),
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
                .filter(|(p, _, _)| p.as_ref() == Some(id))
                .map(|(_, n, _)| n.clone())
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
        if self.host.as_ref().is_some_and(|(p, _, _)| *p == place) {
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
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move {
                    match client {
                        None => Some(("this PC".to_string(), hopper::hosting::local_panel::total_memory() as f64)),
                        Some(c) => c.get("/host").ok().map(|v| {
                            (v["hostname"].as_str().unwrap_or("").to_string(), v["mem_total"].as_f64().unwrap_or(0.0))
                        }),
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                if this.place == place
                    && let Some((name, bytes)) = r.filter(|(_, b)| *b > 0.0)
                {
                    let name = if name.is_empty() { this.host_label(cx) } else { name };
                    this.host = Some((place, name, bytes / 1_073_741_824.0));
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
    }

    /// The game version the server will run.
    fn game_version(&self, cx: &App) -> Option<String> {
        match &self.source {
            Source::Instance(r) => self.store.read(cx).instance(r).map(|i| i.game_version.clone()),
            Source::Fresh => Some(self.base.read(cx).game_version().to_string()).filter(|v| !v.is_empty()),
            Source::Files(p) => Some(p.game_version.clone()).filter(|v| !v.is_empty()),
        }
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
            "memory_mb": (self.memory.read(cx).value().start() * 1024.0).round() as u32,
            "port": port,
            "motd": self.motd_text(cx),
            "address": self.address.read(cx).value().trim(),
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
            "memory_mb": (self.memory.read(cx).value().start() * 1024.0).round() as u32,
            "port": port,
            "address": self.address.read(cx).value().trim(),
            "accept_eula": true,
        }))
    }

    /// Ready to create, or why not.
    fn problem(&self, cx: &App) -> Option<String> {
        match &self.place {
            Some(_) => self.panel_body(cx).err(),
            None => self.options(cx).err(),
        }
    }

    fn options(&self, cx: &App) -> Result<CreateOptions, String> {
        let gv = self.game_version(cx).unwrap_or_default();
        let java = self
            .java
            .clone()
            .or_else(|| best_java(&self.store, &gv, cx))
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
        };
        o.name = self.server_name(cx);
        if o.name.trim().is_empty() {
            return Err("Name the server.".into());
        }
        o.motd = self.motd_text(cx);
        o.memory_mb = (self.memory.read(cx).value().start() * 1024.0).round() as u32;
        o.port = self
            .port
            .read(cx)
            .value()
            .trim()
            .parse()
            .map_err(|_| "The port must be a number.")?;
        if o.folder().exists() {
            return Err(format!("{} already exists.", o.folder().display()));
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
            };
            (i, o, store.settings().sync_anyway.clone(), store.settings().sync_blacklist.clone())
        });
        let job = store.start_job(format!("Making {name} on {}", p.name));
        let store = store_entity;
        cx.spawn(async move |cx| {
            let r = cx
                .background_spawn(async move {
                    let r = match from {
                        Some((i, o, anyway, hidden)) => {
                            hopper::hosting::panel_create::create_from_instance(&client, &i, &o, &anyway, &hidden, &job)
                                .map(|r| r.made)
                        }
                        None => client.run_job(route, body, &job).inspect(|made| {
                            // The pack route has no MOTD of its own.
                            hopper::hosting::panel_create::set_motd(&client, made, &motd, &name)
                        }),
                    };
                    job.finish(&r);
                    r.map(|v| (v, client.panel.clone()))
                })
                .await;
            if let Ok((v, panel)) = r {
                let folder = hopper::hosting::panel_create::PanelReport { made: v, content: None }
                    .folder()
                    .unwrap_or_default();
                store
                    .update(cx, |s, cx| {
                        if let Some(inst) = inst
                            && !folder.is_empty()
                        {
                            s.set_pairing(&hopper::hopperd::panel_root(&panel, &folder), &inst, cx);
                        }
                        s.rediscover_servers();
                    });
            }
        })
        .detach();
        window.push_notification(
            gpui_kit::component::notification::Notification::info(format!(
                "Making the server on {}. It shows up in its group when it's ready.",
                p.name
            )),
            cx,
        );
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
        match &this.source {
            Source::Files(p) if this.zip.is_some() => {
                let files = match &p.build {
                    Build::ServerFiles(f) => Some(f.clone()),
                    Build::ClientPack(..) => None,
                };
                this.store.read(cx).create_hosted_from_zip(opts, this.zip.clone().unwrap(), files, |_| {})
            }
            Source::Files(p) => match &p.build {
                Build::ServerFiles(files) => {
                    this.store.read(cx).create_hosted_from_files(opts, files.clone(), |_| {})
                }
                Build::ClientPack(pack, file) => this.store.read(cx).create_hosted_from_client_pack(
                    opts,
                    (**pack).clone(),
                    (**file).clone(),
                    |_| {},
                ),
            },
            Source::Instance(r) => {
                let pack = this.server_pack && this.has_pack(cx);
                this.store.read(cx).create_hosted(opts, Some(r.clone()), pack, |_| {})
            }
            Source::Fresh => this.store.read(cx).create_hosted(opts, None, false, |_| {}),
        }
        window.push_notification(
            gpui_kit::component::notification::Notification::info(if via_window {
                "Creating the server. Hopper's download window gets the pack from CurseForge first: follow it from the top bar or the task list. The server shows up under Servers when it's ready."
            } else {
                "Creating the server. It shows up under Servers when it's ready."
            }),
            cx,
        );
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
        let hint = self.default_name(cx);
        if hint != self.name_hint {
            self.name.update(cx, |i, cx| i.set_placeholder(hint.clone(), window, cx));
            self.name_hint = hint;
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
            Source::Fresh | Source::Files(_) => None,
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
            .description(if from_instance {
                "Its loader, server-side mods, config, kubejs and datapacks. Client-only mods stay out."
            } else {
                "A fresh server: pick the game and server type."
            })
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        TabBar::new("host-source")
                            .segmented()
                            .small()
                            .selected_index(usize::from(!from_instance))
                            .child(Tab::new().label("An instance"))
                            .child(Tab::new().label("Fresh"))
                            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                let first = this.store.read(cx).instances().first().map(|i| i.root.clone());
                                this.source = match (*ix, first) {
                                    (0, Some(r)) => Source::Instance(r),
                                    _ => Source::Fresh,
                                };
                                this.java = None;
                                cx.notify();
                            })),
                    )
                    .when(from_instance, |c| {
                        c.child(widgets::picker(
                            "host-instance",
                            picked_name.unwrap_or_else(|| "Pick an instance".into()),
                            instances.iter().map(|(r, n)| (n.clone(), Some(r) == match &self.source {
                                Source::Instance(p) => Some(p),
                                Source::Fresh | Source::Files(_) => None,
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
                        _ if on_panel => format!(
                            "Its server files, or built from the pack if it has none, on {on}."
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
                                .child(widgets::caption(p.version.clone(), cx)),
                        )
                        .when(!on_panel, |c| c.children(self.website_only_notice(p, cx))),
                ),
            None => source_field,
        };
        let pack_inst = match &self.source {
            Source::Instance(r) => self.store.read(cx).instance(r).cloned().filter(|i| i.pack.is_some()),
            Source::Fresh | Source::Files(_) => None,
        };
        let pack_field = pack_inst.map(|i| {
            let known = hopper::hosting::has_server_pack(&i);
            Field::new()
                .label("Server pack")
                .description(if known {
                    "The pack author's server files: exactly their server mods and configs. Often a big download."
                } else {
                    "If the author published one for this version, Hopper uses it; otherwise it builds from the instance."
                })
                .child(
                    Checkbox::new("host-server-pack")
                        .label("Use the pack's official server pack")
                        .checked(self.server_pack)
                        .on_change(cx.listener(|this, v: &bool, _, cx| {
                            this.server_pack = *v;
                            cx.notify();
                        })),
                )
        });
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
        // "of 48 GB on rig", and a word when that leaves the machine little.
        let host_ram = self.host.as_ref().filter(|(p, _, _)| *p == self.place).map(|(_, n, total)| {
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
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .p_2()
                    .rounded(px(crate::theme::RADIUS))
                    .bg(theme.warning.opacity(0.08))
                    .text_sm()
                    .child(div().flex_1().min_w_0().child(msg))
                    .when_some(suggest, |r, s| {
                        r.child(
                            Button::new("host-port-free")
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
            .hopper_panels
            .iter()
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
                .child(widgets::picker(
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
        v_flex()
            .gap_4()
            .child(settings_form(rem).children(place_field).child(source_field).children(pack_field))
            .when(!from_instance && files.is_none(), |c| c.child(self.base.clone()))
            .child(
                settings_form(rem)
                    .when(files.is_some() || from_instance, |f| {
                        f.child(
                            Field::new()
                                .label("Name")
                                .child(div().w_80().child(Input::new(&self.name).small())),
                        )
                    })
                    .child(
                        Field::new()
                            .label("MOTD")
                            .description("The line players see under the server in their list.")
                            .child(div().w_80().child(Input::new(&self.motd).small())),
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
                            .description(if on_panel {
                                "Empty: the panel picks a free one."
                            } else {
                                "Players join on this port; forward it on your router for friends outside your network."
                            })
                            .child(
                                v_flex()
                                    .gap_1p5()
                                    .child(div().w_32().child(Input::new(&self.port).small()))
                                    .children(port_warning),
                            ),
                    )
                    .when(on_panel, |f| {
                        f.child(
                            Field::new()
                                .label("Address")
                                .description("A name players join by, when the panel's server addresses are on.")
                                .child(div().w_56().child(Input::new(&self.address).small())),
                        )
                    })
                    .when(!on_panel, |f| f.child(java))
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
                    ),
            )
            .when(!on_panel, |c| c.child(widgets::caption(format!("Goes in {hosted_root}"), cx).text_xs()))
            .when_some(problem, |c, p| {
                c.child(div().text_sm().text_color(theme.muted_foreground).child(p))
            })
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
                let c = match f2.file {
                    Some(_) => Default::default(),
                    None => hopper::cf::get_mod(f2.project).map(|m| check(&m, &v)).unwrap_or_default(),
                };
                anyhow::Ok((v, c))
            })
            .await;
        handle
            .update(cx, |_, window, cx| match r {
                Ok((v, c)) => {
                    let mut p = PackFiles::new(Build::ServerFiles(files), name, &v);
                    p.check = c;
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
                anyhow::Ok((f, c))
            })
            .await;
        handle
            .update(cx, |_, window, cx| match r {
                Ok((f, c)) => {
                    let name = pack.name.clone();
                    let build = Build::ClientPack(Box::new(pack), Box::new(f.clone()));
                    let mut p = PackFiles::new(build, name, &f);
                    p.check = c;
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
        let title = if form.read(cx).place.is_some() { "New server" } else { "Host a server on this PC" };
        dialog
            .title(title)
            .w(rems(44.).to_pixels(window.rem_size()))
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
