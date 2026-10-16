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
    let (store2, major) = (store.clone(), req.major);
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
                .child(
                    Button::new("download-java")
                        .small()
                        .map(|b| if none_fit { b.primary() } else { b.outline() })
                        .icon(IconName::Download)
                        .label(format!("Download Java {major}"))
                        .tooltip("Eclipse Temurin, into Hopper's own folder")
                        .on_click(move |_, _, cx| {
                            store2.update(cx, |s, cx| {
                                s.download_java(major);
                                cx.notify();
                            })
                        }),
                ),
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
        }
    }
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
    source: Source,
    base: Entity<CreateForm>,
    memory: Entity<SliderState>,
    port: Entity<InputState>,
    java: Option<PathBuf>,
    eula: bool,
    /// Build from the pack's official server pack when it has one.
    server_pack: bool,
    _subscriptions: Vec<Subscription>,
}

impl HostForm {
    fn new(store: Entity<Store>, from: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        store.update(cx, |s, _| s.want_javas());
        let base = cx.new(|cx| CreateForm::named("New server", window, cx));
        let memory = cx.new(|_| SliderState::new().min(1.).max(32.).step(0.5).default_value(6.));
        let root = PathBuf::from(store.read(cx).hosted_root());
        let port = hopper::hosting::suggest_port(&root).to_string();
        let port = cx.new(|cx| InputState::new(window, cx).default_value(port));
        let subs = vec![
            cx.observe(&base, |_, _, cx| cx.notify()),
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.subscribe(&memory, |_, _, _: &SliderEvent, cx| cx.notify()),
            cx.subscribe(&port, |_, _, _: &InputEvent, cx| cx.notify()),
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
            source,
            base,
            memory,
            port,
            java: None,
            eula: false,
            server_pack: true,
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

    /// The game version the server will run.
    fn game_version(&self, cx: &App) -> Option<String> {
        match &self.source {
            Source::Instance(r) => self.store.read(cx).instance(r).map(|i| i.game_version.clone()),
            Source::Fresh => Some(self.base.read(cx).game_version().to_string()).filter(|v| !v.is_empty()),
            Source::Files(p) => Some(p.game_version.clone()).filter(|v| !v.is_empty()),
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
        if let Source::Instance(r) = &self.source
            && let Some(name) = store.instance(r).map(|i| format!("{} server", i.name))
        {
            o.name = name;
            o.motd = o.name.clone();
        }
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

    fn create(form: &Entity<Self>, window: &mut Window, cx: &mut App) -> bool {
        let this = form.read(cx);
        let opts = match this.options(cx) {
            Ok(o) => o,
            Err(_) => return false,
        };
        match &this.source {
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
            gpui_kit::component::notification::Notification::info(
                "Creating the server. It shows up under Servers when it's ready.",
            ),
            cx,
        );
        true
    }
}

impl Render for HostForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rem = window.rem_size();
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
        let view = cx.entity();
        let roots: Vec<PathBuf> = instances.iter().map(|(r, _)| r.clone()).collect();
        let source_field = Field::new()
            .label("Start from")
            .description(if from_instance {
                "Its Minecraft, loader, server-side mods and configs. Client-only mods stay out."
            } else {
                "A fresh server: pick the game and loader."
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
                    match &p.build {
                        Build::ServerFiles(_) => format!(
                            "The pack author's server mods and configs, on {on}. Often a big download."
                        ),
                        Build::ClientPack(..) => format!(
                            "Built from the client pack on {on}, client-only mods left out."
                        ),
                    }
                })
                .child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .child(Icon::new(IconName::Server).small())
                        .child(div().font_semibold().child(p.name.clone()))
                        .child(widgets::caption(p.version.clone(), cx)),
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
        let problem = self.options(cx).err();
        let hosted_root = self.store.read(cx).hosted_root();
        v_flex()
            .gap_4()
            .child(settings_form(rem).child(source_field).children(pack_field))
            .when(!from_instance && files.is_none(), |c| c.child(self.base.clone()))
            .child(
                settings_form(rem)
                    .child(
                        Field::new()
                            .label("Memory")
                            .child(
                                h_flex()
                                    .gap_3()
                                    .child(div().w_56().child(Slider::new(&self.memory)))
                                    .child(div().text_sm().child(format!("{gb:.1} GB"))),
                            ),
                    )
                    .child(
                        Field::new()
                            .label("Port")
                            .description("Players join on this port; forward it on your router for friends outside your network.")
                            .child(div().w_32().child(Input::new(&self.port).small())),
                    )
                    .child(java)
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
            .child(widgets::caption(format!("Goes in {hosted_root}"), cx).text_xs())
            .when_some(problem, |c, p| {
                c.child(div().text_sm().text_color(theme.muted_foreground).child(p))
            })
    }
}

/// Opens the New server dialog, starting from `from` if given.
pub fn open(store: Entity<Store>, from: Option<PathBuf>, window: &mut Window, cx: &mut App) {
    let form = cx.new(|cx| HostForm::new(store, from, window, cx));
    open_form(form, window, cx);
}

/// Opens the New server dialog for a pack's server files (from Browse):
/// the server is built from them alone, no instance needed.
pub fn open_files(store: Entity<Store>, pack: PackFiles, window: &mut Window, cx: &mut App) {
    let form = cx.new(|cx| {
        let mut f = HostForm::new(store, None, window, cx);
        f.source = Source::Files(Rc::new(pack));
        f
    });
    open_form(form, window, cx);
}

/// Looks up the pack version `files` go with, then opens [`open_files`].
pub fn open_for_files(
    store: Entity<Store>,
    files: hopper::browse::ServerFiles,
    name: String,
    window: &mut Window,
    cx: &mut App,
) {
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let f2 = files.clone();
        let r = cx
            .background_spawn(async move { hopper::browse::server_files_version(&f2) })
            .await;
        handle
            .update(cx, |_, window, cx| match r {
                Ok(v) => open_files(store, PackFiles::new(Build::ServerFiles(files), name, &v), window, cx),
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
pub fn open_for_client_pack(store: Entity<Store>, pack: hopper::cf::Mod, window: &mut Window, cx: &mut App) {
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let p2 = pack.clone();
        let r = cx
            .background_spawn(async move { hopper::browse::newest_pack_file(&p2) })
            .await;
        handle
            .update(cx, |_, window, cx| match r {
                Ok(f) => {
                    let name = pack.name.clone();
                    let build = Build::ClientPack(Box::new(pack), Box::new(f.clone()));
                    open_files(store, PackFiles::new(build, name, &f), window, cx)
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
        let ready = form.read(cx).options(cx).is_ok();
        dialog
            .title("Host a server on this PC")
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
