//! Instance dialogs: New instance, Export, Delete, and importing a pack file.

use crate::java_form::settings_form;
use crate::store::{Msg, Store};
use crate::widgets;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::form::Field;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::cf::{self, Loader, LoaderVersion};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

const KINDS: [(Loader, &str); 5] = [
    (Loader::Any, "Vanilla"),
    (Loader::NeoForge, "NeoForge"),
    (Loader::Forge, "Forge"),
    (Loader::Fabric, "Fabric"),
    (Loader::Quilt, "Quilt"),
];

pub(crate) fn footer(action: &'static str, ready: bool) -> DialogFooter {
    DialogFooter::new()
        .child(DialogClose::new().child(Button::new("dialog-cancel").outline().label("Cancel")))
        .child(
            DialogAction::new().child(
                Button::new("dialog-ok")
                    .primary()
                    .label(action)
                    .disabled(!ready),
            ),
        )
}

// ---- New instance --------------------------------------------------------

pub struct CreateForm {
    name: Entity<InputState>,
    versions: Vec<String>,
    game_version: String,
    kind: Loader,
    loaders: HashMap<String, Vec<LoaderVersion>>,
    /// Minecraft versions whose loader list has arrived.
    loaded: std::collections::HashSet<String>,
    loader: String,
}

impl CreateForm {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::named("New instance", window, cx)
    }

    /// The form with `default_name` in its name field (the server dialog
    /// uses it too).
    pub(crate) fn named(default_name: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let default_name = default_name.to_string();
        let name = cx.new(|cx| InputState::new(window, cx).default_value(default_name));
        cx.spawn(async move |this, cx| {
            let v = cx.background_spawn(async { cf::minecraft_versions() }).await;
            this.update(cx, |this, cx| {
                this.versions = v.unwrap_or_default();
                if this.game_version.is_empty() {
                    this.game_version = this.versions.first().cloned().unwrap_or_default();
                }
                this.want_loaders(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        Self {
            name,
            versions: Vec::new(),
            game_version: String::new(),
            kind: Loader::NeoForge,
            loaders: HashMap::new(),
            loaded: Default::default(),
            loader: String::new(),
        }
    }

    fn want_loaders(&mut self, cx: &mut Context<Self>) {
        let gv = self.game_version.clone();
        if gv.is_empty() || self.loaders.contains_key(&gv) {
            self.pick_loader();
            return;
        }
        self.loaders.insert(gv.clone(), Vec::new());
        cx.spawn(async move |this, cx| {
            let key = gv.clone();
            let r = cx.background_spawn(async move { cf::loader_versions(&gv) }).await;
            this.update(cx, |this, cx| {
                this.loaded.insert(key.clone());
                this.loaders.insert(key, r.unwrap_or_default());
                this.pick_loader();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The loader list for the picked Minecraft has arrived.
    fn loaded(&self) -> bool {
        self.loaded.contains(&self.game_version)
    }

    fn options(&self) -> Vec<LoaderVersion> {
        let prefix = format!("{}-", self.kind.name().to_lowercase());
        self.loaders
            .get(&self.game_version)
            .into_iter()
            .flatten()
            .filter(|v| v.name.starts_with(&prefix))
            .cloned()
            .collect()
    }

    /// The recommended build, unless one that fits is already picked.
    fn pick_loader(&mut self) {
        let options = self.options();
        if options.iter().any(|v| v.name == self.loader) {
            return;
        }
        self.loader = options
            .iter()
            .find(|v| v.recommended)
            .or(options.first())
            .map(|v| v.name.clone())
            .unwrap_or_default();
    }

    /// (name, Minecraft version, loader id like "neoforge-21.1.256" or ""
    /// for vanilla) once the form is complete.
    pub(crate) fn choice(&self, cx: &App) -> Option<(String, String, String)> {
        if !self.ready(cx) {
            return None;
        }
        let loader = if self.kind == Loader::Any { String::new() } else { self.loader.clone() };
        Some((
            self.name.read(cx).value().trim().to_string(),
            self.game_version.clone(),
            loader,
        ))
    }

    /// The picked Minecraft version, even before the form is complete.
    pub(crate) fn game_version(&self) -> &str {
        &self.game_version
    }

    pub(crate) fn ready(&self, cx: &App) -> bool {
        !self.name.read(cx).value().trim().is_empty()
            && !self.game_version.is_empty()
            && (self.kind == Loader::Any || !self.loader.is_empty())
    }

    fn create(form: &Entity<Self>, store: &Entity<Store>, window: &mut Window, cx: &mut App) -> bool {
        let this = form.read(cx);
        if !this.ready(cx) {
            return false;
        }
        let loader = if this.kind == Loader::Any {
            String::new()
        } else {
            this.loader.clone()
        };
        let game_version = this.game_version.clone();
        let name = this.name.read(cx).value().trim().to_string();
        let root = PathBuf::from(&store.read(cx).settings().instances_root);
        match hopper::modpack::create(&root, &name, &game_version, &loader) {
            Ok(inst) => {
                store.read(cx).spawn(move || Msg::InstanceChanged(inst.root));
                true
            }
            Err(e) => {
                window.push_notification(Notification::error(format!("{e:#}")), cx);
                false
            }
        }
    }
}

impl Render for CreateForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rem = window.rem_size();
        let view = cx.entity();
        let versions = self.versions.clone();
        let pick_version = {
            let view = view.clone();
            Rc::new(move |ix: usize, _: &mut Window, cx: &mut App| {
                if let Some(v) = versions.get(ix).cloned() {
                    view.update(cx, |this, cx| {
                        this.game_version = v;
                        this.want_loaders(cx);
                        cx.notify();
                    });
                }
            })
        };
        let options = self.options();
        let names: Vec<String> = options.iter().map(|v| v.name.clone()).collect();
        let pick_loader = {
            let view = view.clone();
            Rc::new(move |ix: usize, _: &mut Window, cx: &mut App| {
                if let Some(n) = names.get(ix).cloned() {
                    view.update(cx, |this, cx| {
                        this.loader = n;
                        cx.notify();
                    });
                }
            })
        };
        let kind_ix = KINDS.iter().position(|(k, _)| *k == self.kind).unwrap_or(0);
        settings_form(rem)
            .child(Field::new().label("Name").child(Input::new(&self.name)))
            .child(
                Field::new().label("Minecraft").child(widgets::picker(
                    "new-mc",
                    if self.game_version.is_empty() {
                        "Loading versions".into()
                    } else {
                        self.game_version.clone()
                    },
                    self.versions
                        .iter()
                        .map(|v| (v.clone(), *v == self.game_version))
                        .collect(),
                    rem,
                    pick_version,
                )),
            )
            .child(
                Field::new().label("Loader").child(
                    TabBar::new("new-kind")
                        .segmented()
                        .small()
                        .selected_index(kind_ix)
                        .children(KINDS.iter().map(|(_, l)| Tab::new().label(*l)))
                        .on_click(cx.listener(|this, ix: &usize, _, cx| {
                            this.kind = KINDS[*ix].0;
                            this.loader.clear();
                            this.pick_loader();
                            cx.notify();
                        })),
                ),
            )
            .when(self.kind != Loader::Any, |f| {
                f.child(
                    Field::new().label("Loader version").child(widgets::picker(
                        "new-loader",
                        match (self.loader.is_empty(), self.loaded()) {
                            (false, _) => self.loader.clone(),
                            (true, false) => "Loading builds".into(),
                            (true, true) => format!(
                                "No {} builds for {}",
                                self.kind.name(),
                                self.game_version
                            ),
                        },
                        options
                            .iter()
                            .map(|v| {
                                let tag = if v.recommended {
                                    "  ★ recommended"
                                } else if v.latest {
                                    "  latest"
                                } else {
                                    ""
                                };
                                (format!("{}{tag}", v.name), v.name == self.loader)
                            })
                            .collect(),
                        rem,
                        pick_loader,
                    )),
                )
            })
    }
}

pub fn open_create(store: Entity<Store>, window: &mut Window, cx: &mut App) {
    let form = cx.new(|cx| CreateForm::new(window, cx));
    window.open_dialog(cx, move |dialog, window, cx| {
        let (store, f2) = (store.clone(), form.clone());
        let ready = form.read(cx).ready(cx);
        dialog
            .title("New instance")
            .w(rems(36.).to_pixels(window.rem_size()))
            .child(form.clone())
            .footer(footer("Create", ready))
            .on_ok(move |_, window, cx| CreateForm::create(&f2, &store, window, cx))
    });
}

// ---- Import --------------------------------------------------------------

/// Asks for a CurseForge zip or Modrinth .mrpack and installs it.
pub fn import_pack(store: Entity<Store>, cx: &mut App) {
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some("Import modpack".into()),
    });
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(paths))) = paths.await else { return };
        let Some(path) = paths.into_iter().next() else { return };
        store.update(cx, |s, _| {
            let root = PathBuf::from(&s.settings().instances_root);
            s.spawn_job(format!("Importing {}", path.display()), move |job| {
                let mr = path
                    .extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("mrpack"));
                let inst = if mr {
                    hopper::modpack::import_mrpack(&root, &path, job)?
                } else {
                    hopper::modpack::import_zip(&root, &path, job)?
                };
                Ok(Msg::InstanceChanged(inst.root))
            });
        });
    })
    .detach();
}

// ---- Delete --------------------------------------------------------------

pub fn confirm_delete(store: Entity<Store>, root: PathBuf, window: &mut Window, cx: &mut App) {
    let Some(inst) = store.read(cx).instance(&root).cloned() else {
        return;
    };
    // Another launcher's instance is deleted there, or it would break in it.
    if let Some(k) = hopper::launchers::Kind::from_key(&inst.source) {
        window.push_notification(
            gpui_kit::component::notification::Notification::info(format!(
                "{} is a {} instance. Delete it in {}.",
                inst.name,
                k.name(),
                k.name()
            )),
            cx,
        );
        return;
    }
    // A network drive has no Recycle Bin: Hopper moves the folder aside.
    let remote = hopper::instance_ops::is_remote_drive(&inst.root);
    let permanent = store.read(cx).settings().delete_permanently;
    let whereto = if permanent {
        "is deleted for good (Settings > General sends deletes to the Recycle Bin instead)"
    } else if remote {
        "moves to .hopper-trash next to it (network drives have no Recycle Bin)"
    } else {
        "moves to the Recycle Bin"
    };
    window.open_alert_dialog(cx, move |alert, _, _| {
        let (store, inst) = (store.clone(), inst.clone());
        alert
            .title(format!("Delete {}?", inst.name))
            .description(format!("{}, worlds and all, {whereto}.", inst.root.display()))
            .confirm()
            .ok_text("Delete instance")
            .ok_variant(ButtonVariant::Danger)
            .on_ok(move |_, _, cx| {
                // Big folders take a while: off the UI thread.
                let inst = inst.clone();
                store.read(cx).spawn_instance_job(&inst.root.clone(), format!("Deleting {}", inst.name), move |job| {
                    match hopper::instance_ops::delete_instance(&inst, permanent)? {
                        hopper::instance_ops::DeleteOutcome::Deleted => job.detail("Deleted"),
                        hopper::instance_ops::DeleteOutcome::RecycleBin => job.detail("In the Recycle Bin"),
                        hopper::instance_ops::DeleteOutcome::MovedAside(p) => {
                            job.detail(format!("Moved to {}", p.display()))
                        }
                    }
                    Ok(Msg::Forget(inst.root.clone()))
                });
                true
            })
    });
}

// ---- Export --------------------------------------------------------------

pub struct ExportForm {
    root: PathBuf,
    version: Entity<InputState>,
    author: Entity<InputState>,
    out: Entity<InputState>,
    folders: Vec<(&'static str, bool)>,
}

impl ExportForm {
    fn new(inst: &hopper::instances::Instance, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let version = inst
            .pack
            .as_ref()
            .map(|p| p.version.clone())
            .unwrap_or("1.0.0".into());
        let downloads = directories::UserDirs::new()
            .and_then(|u| u.download_dir().map(|d| d.to_path_buf()))
            .unwrap_or_default();
        let out = downloads
            .join(format!(
                "{}-{version}.zip",
                hopper::instances::Instance::folder_name(&inst.name)
            ))
            .to_string_lossy()
            .into_owned();
        Self {
            root: inst.root.clone(),
            version: cx.new(|cx| InputState::new(window, cx).default_value(version)),
            author: cx.new(|cx| InputState::new(window, cx).placeholder("Your name")),
            out: cx.new(|cx| InputState::new(window, cx).default_value(out)),
            folders: hopper::modpack::EXPORT_FOLDERS
                .iter()
                .map(|f| (*f, inst.root.join(f).exists()))
                .collect(),
        }
    }

    fn export(form: &Entity<Self>, store: &Entity<Store>, cx: &mut App) -> bool {
        let this = form.read(cx);
        let Some(inst) = store.read(cx).instance(&this.root).cloned() else {
            return true;
        };
        let folders: Vec<&'static str> =
            this.folders.iter().filter(|(_, on)| *on).map(|(f, _)| *f).collect();
        let version = this.version.read(cx).value().trim().to_string();
        let author = this.author.read(cx).value().trim().to_string();
        let out = PathBuf::from(this.out.read(cx).value().trim());
        store.read(cx).spawn_job(format!("Exporting {}", inst.name), move |job| {
            hopper::modpack::export(&inst, &version, &author, &folders, &out)?;
            job.detail(format!("Saved {}", out.display()));
            Ok(Msg::Nothing)
        });
        true
    }
}

impl Render for ExportForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rem = window.rem_size();
        v_flex()
            .gap_4()
            .child(
                settings_form(rem)
                    .child(Field::new().label("Version").child(Input::new(&self.version)))
                    .child(Field::new().label("Author").child(Input::new(&self.author)))
                    .child(
                        Field::new().label("Include").items_start().child(
                            v_flex().gap_2().children(self.folders.iter().enumerate().map(
                                |(i, (f, on))| {
                                    Checkbox::new(SharedString::from(format!("inc:{f}")))
                                        .label(*f)
                                        .checked(*on)
                                        .on_change(cx.listener(move |this, v: &bool, _, cx| {
                                            this.folders[i].1 = *v;
                                            cx.notify();
                                        }))
                                },
                            )),
                        ),
                    )
                    .child(
                        Field::new().label("Save to").child(
                            h_flex()
                                .gap_2()
                                .child(div().flex_1().child(Input::new(&self.out)))
                                .child(
                                    Button::new("export-browse")
                                        .outline()
                                        .label("Browse…")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            let current = PathBuf::from(this.out.read(cx).value().as_ref());
                                            let dir = current
                                                .parent()
                                                .map(|d| d.to_path_buf())
                                                .unwrap_or_default();
                                            let name = current
                                                .file_name()
                                                .map(|n| n.to_string_lossy().into_owned());
                                            let rx = cx.prompt_for_new_path(&dir, name.as_deref());
                                            let out = this.out.clone();
                                            cx.spawn_in(window, async move |_, cx| {
                                                if let Ok(Ok(Some(p))) = rx.await {
                                                    let p = p.to_string_lossy().into_owned();
                                                    cx.update(|window, cx| {
                                                        out.update(cx, |s, cx| s.set_value(p, window, cx))
                                                    })
                                                    .ok();
                                                }
                                            })
                                            .detach();
                                        })),
                                ),
                        ),
                    ),
            )
            .child(widgets::caption(
                "CurseForge mods go in manifest.json; other jars and the folders above go in overrides.",
                cx,
            ))
    }
}

pub fn open_export(store: Entity<Store>, root: PathBuf, window: &mut Window, cx: &mut App) {
    let Some(inst) = store.read(cx).instance(&root).cloned() else {
        return;
    };
    let form = cx.new(|cx| ExportForm::new(&inst, window, cx));
    window.open_dialog(cx, move |dialog, window, _| {
        let (store, f2) = (store.clone(), form.clone());
        dialog
            .title("Export as a CurseForge pack")
            .w(rems(40.).to_pixels(window.rem_size()))
            .child(form.clone())
            .footer(footer("Export", true))
            .on_ok(move |_, _, cx| ExportForm::export(&f2, &store, cx))
    });
}

// ---- Server pack ---------------------------------------------------------

/// Asks where to save a pack's server zip, then downloads it.
pub fn save_server_pack(store: Entity<Store>, project: u64, file: u64, name: String, cx: &mut App) {
    let dir = directories::UserDirs::new()
        .and_then(|u| u.download_dir().map(|d| d.to_path_buf()))
        .unwrap_or_default();
    let suggested = format!(
        "{} server.zip",
        hopper::instances::Instance::folder_name(&name)
    );
    let rx = cx.prompt_for_new_path(&dir, Some(&suggested));
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(dest))) = rx.await else { return };
        store.update(cx, |s, _| {
            s.spawn_job(format!("Downloading the {name} server pack"), move |job| {
                hopper::instance_ops::download_server_pack(project, file, &dest, job)?;
                job.detail(format!("Saved {}", dest.display()));
                Ok(Msg::Nothing)
            });
        });
    })
    .detach();
}

/// [`save_server_pack`] for server files found in Browse (either site).
pub fn save_server_files(
    store: Entity<Store>,
    files: hopper::browse::ServerFiles,
    name: String,
    cx: &mut App,
) {
    let dir = directories::UserDirs::new()
        .and_then(|u| u.download_dir().map(|d| d.to_path_buf()))
        .unwrap_or_default();
    // Modrinth's zip comes with its own name.
    let suggested = files.file.as_ref().map_or_else(
        || format!("{} server.zip", hopper::instances::Instance::folder_name(&name)),
        |f| f.file_name.clone(),
    );
    let rx = cx.prompt_for_new_path(&dir, Some(&suggested));
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(dest))) = rx.await else { return };
        store.update(cx, |s, _| {
            s.spawn_job(format!("Downloading the {name} server files"), move |job| {
                hopper::browse::download_server_files(&files, &dest, job)?;
                job.detail(format!("Saved {}", dest.display()));
                Ok(Msg::Nothing)
            });
        });
    })
    .detach();
}

// ---- Microsoft sign-in -----------------------------------------------------

/// The device-code sign-in: show a code, the user enters it in the browser.
pub struct SignIn {
    store: Entity<Store>,
    code: Option<hopper::msauth::DeviceCode>,
    error: Option<String>,
    done: Option<String>,
    _task: Option<Task<()>>,
}

impl SignIn {
    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.code = None;
        self.error = None;
        let store = self.store.clone();
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let dc = cx
                .background_spawn(async { hopper::msauth::start_device_code() })
                .await;
            let dc = match dc {
                Ok(dc) => dc,
                Err(e) => {
                    this.update(cx, |this, cx| {
                        this.error = Some(format!("{e:#}"));
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            this.update(cx, |this, cx| {
                // The code goes on the clipboard: paste it on the page.
                cx.write_to_clipboard(ClipboardItem::new_string(dc.user_code.clone()));
                cx.open_url(&dc.verification_uri);
                this.code = Some(dc.clone());
                cx.notify();
            })
            .ok();
            let r = cx
                .background_spawn(async move {
                    let (refresh, access) = hopper::msauth::poll_device_code(&dc)?;
                    let session = hopper::msauth::minecraft_session(&access)?;
                    hopper::accounts::add(&refresh, &session)?;
                    // Read the profile now so the skin shows without opening
                    // the account page first. A miss here isn't fatal.
                    if let Ok(p) = hopper::mcprofile::Api::new(session.access_token.clone()).profile() {
                        let skin = p.active_skin().map(|s| s.url.as_str());
                        hopper::accounts::note_profile(&p.id, &p.name, skin).ok();
                    }
                    anyhow::Ok(session.profile.name)
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                match r {
                    Ok(name) => {
                        let n = name.clone();
                        store.update(cx, |s, cx| {
                            s.reload_accounts();
                            s.update_settings(
                                |st| {
                                    st.signed_in_as = Some(n);
                                    // Signed in: Play starts the game directly.
                                    if !st.launch_picked {
                                        st.launch_native = true;
                                    }
                                },
                                cx,
                            )
                        });
                        window.close_dialog(cx);
                        window.push_notification(Notification::success(format!("Signed in as {name}.")), cx);
                        this.done = Some(name);
                    }
                    Err(e) => this.error = Some(format!("{e:#}")),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }
}

impl Render for SignIn {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        if hopper::msauth::client_id().is_none() {
            return v_flex()
                .gap_2()
                .child(div().font_semibold().child("Launching without Mojang's launcher needs a Microsoft app id."))
                .child(widgets::caption("Register an app in Azure, get it approved for the Minecraft API, and put its id in .env as MS_CLIENT_ID. Until then, keep Launch with on Mojang's launcher.", cx))
                .into_any_element();
        }
        if let Some(name) = &self.done {
            return h_flex()
                .gap_2()
                .text_color(theme.success)
                .child(gpui_kit::component::Icon::new(gpui_kit::assets::IconName::CircleCheck))
                .child(format!("Signed in as {name}."))
                .into_any_element();
        }
        if let Some(e) = &self.error {
            return v_flex()
                .gap_3()
                .child(div().text_sm().text_color(theme.danger).child(e.clone()))
                .child(
                    Button::new("sign-in-again")
                        .outline()
                        .label("Try again")
                        .on_click(cx.listener(|this, _, window, cx| this.start(window, cx))),
                )
                .into_any_element();
        }
        match &self.code {
            None => h_flex()
                .gap_2()
                .child(gpui_kit::component::spinner::Spinner::new().small())
                .child(widgets::caption("Asking Microsoft for a code", cx))
                .into_any_element(),
            Some(dc) => {
                let (code, uri) = (dc.user_code.clone(), dc.verification_uri.clone());
                let (c2, c3) = (code.clone(), code.clone());
                v_flex()
                    .gap_3()
                    .child(widgets::caption(
                        "The code is copied: paste it on the page that opened (Ctrl+V).",
                        cx,
                    ))
                    .child(
                        Button::new("sign-in-page")
                            .link()
                            .label(uri.clone())
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(c3.clone()));
                                cx.open_url(&uri)
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                div()
                                    .text_3xl()
                                    .font_semibold()
                                    .font_family(theme.mono_font_family.clone())
                                    .child(code),
                            )
                            .child(
                                Button::new("copy-code")
                                    .outline()
                                    .small()
                                    .label("Copy")
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(c2.clone()))
                                    }),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(gpui_kit::component::spinner::Spinner::new().small())
                            .child(widgets::caption("Waiting for you to finish in the browser", cx)),
                    )
                    .into_any_element()
            }
        }
    }
}

pub fn open_sign_in(store: Entity<Store>, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| {
        let mut s = SignIn {
            store,
            code: None,
            error: None,
            done: None,
            _task: None,
        };
        if hopper::msauth::client_id().is_some() {
            s.start(window, cx);
        }
        s
    });
    window.open_dialog(cx, move |dialog, window, _| {
        dialog
            .title("Sign in with Microsoft")
            .w(rems(30.).to_pixels(window.rem_size()))
            .child(view.clone())
    });
}

// ---- Share codes ---------------------------------------------------------

/// Shows an instance's share code, worked out in the background.
pub struct ShareView {
    pack: String,
    made: Option<Result<hopper::sharecode::Made, String>>,
    copied: bool,
    _task: Task<()>,
}

impl ShareView {
    fn new(inst: hopper::instances::Instance, cx: &mut Context<Self>) -> Self {
        let pack = match &inst.pack {
            Some(p) if p.project_id != 0 => format!("{} {}", p.name, p.version).trim().to_string(),
            _ => format!("Minecraft {} with {}", inst.game_version, nice_loader(&inst.loader)),
        };
        let task = cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move {
                    // A private job: fetching the pack's file list isn't
                    // worth a line in the task list.
                    let job = hopper::jobs::Jobs::default().start("share code");
                    hopper::sharecode::make(&inst, &job).map_err(|e| format!("{e:#}"))
                })
                .await;
            this.update(cx, |this, cx| {
                if let Ok(m) = &r {
                    cx.write_to_clipboard(ClipboardItem::new_string(m.code.clone()));
                    this.copied = true;
                }
                this.made = Some(r);
                cx.notify();
            })
            .ok();
        });
        Self { pack, made: None, copied: false, _task: task }
    }
}

/// A muted line that wraps (`widgets::caption` cuts off).
fn note(text: impl Into<SharedString>, cx: &App) -> Div {
    div().text_sm().text_color(cx.theme().muted_foreground).child(text.into())
}

/// "neoforge-21.1.256" as "NeoForge 21.1.256".
fn nice_loader(loader: &str) -> String {
    if loader.is_empty() {
        return "no loader".into();
    }
    let (kind, ver) = loader.split_once('-').unwrap_or((loader, ""));
    let kind = match kind {
        "neoforge" => "NeoForge",
        "forge" => "Forge",
        "fabric" => "Fabric",
        "quilt" => "Quilt",
        k => k,
    };
    format!("{kind} {ver}").trim().to_string()
}

impl Render for ShareView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let made = match &self.made {
            None => {
                return h_flex()
                    .gap_2()
                    .child(gpui_kit::component::spinner::Spinner::new().small())
                    .child(widgets::caption("Comparing the instance with its pack", cx))
                    .into_any_element();
            }
            Some(Err(e)) => {
                return div().text_sm().text_color(theme.danger).child(e.clone()).into_any_element();
            }
            Some(Ok(m)) => m,
        };
        let code = made.code.clone();
        // Codes have no spaces to wrap at, so they're cut into lines.
        let chars: Vec<char> = code.chars().collect();
        let lines: Vec<String> = chars.chunks(56).map(|c| c.iter().collect()).collect();
        let long = lines.len() > 8;
        v_flex()
            .gap_3()
            .child(note(
                "Send this code to a friend. They paste it in Hopper under Install from a share code (the link button above the instance list, or Ctrl+K) and get this exact setup.",
                cx,
            ))
            .child(
                v_flex()
                    .id("share-code")
                    .p_3()
                    .rounded(theme.radius)
                    .bg(theme.muted)
                    .font_family(theme.mono_font_family.clone())
                    .text_sm()
                    .when(long, |d| d.max_h(rems(12.)).overflow_y_scroll())
                    .children(lines.into_iter().map(|l| div().child(l))),
            )
            .child(note(
                format!("{} · {} · {} characters", self.pack, made.share.summary(), made.code.len()),
                cx,
            ))
            .child(
                h_flex().child(
                    Button::new("share-copy")
                        .primary()
                        .icon(IconName::Copy)
                        .label(if self.copied { "Copied" } else { "Copy" })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                            this.copied = true;
                            cx.notify();
                        })),
                ),
            )
            .when(!made.left_out.is_empty(), |d| {
                d.child(div().text_sm().text_color(theme.warning).child(format!(
                    "Not in the code (not on CurseForge or Modrinth, so your friend adds these by hand): {}",
                    made.left_out.join(", ")
                )))
            })
            .child(note(
                "Configs come from the pack. Settings and worlds you changed are not in the code.",
                cx,
            ))
            .into_any_element()
    }
}

pub fn open_share(store: Entity<Store>, root: PathBuf, window: &mut Window, cx: &mut App) {
    let Some(inst) = store.read(cx).instance(&root).cloned() else {
        return;
    };
    let title = format!("Share {}", inst.name);
    let view = cx.new(|cx| ShareView::new(inst, cx));
    window.open_dialog(cx, move |dialog, window, _| {
        dialog
            .title(title.clone())
            .w(rems(38.).to_pixels(window.rem_size()))
            .child(view.clone())
    });
}

/// The box a share code is pasted into.
pub struct RedeemForm {
    input: Entity<InputState>,
    parsed: Option<Result<hopper::sharecode::Share, String>>,
    /// The pack's name and version, by the code's pack.
    pack: Option<(hopper::sharecode::Base, String)>,
    _sub: Subscription,
    _lookup: Option<Task<()>>,
}

impl RedeemForm {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // A code on the clipboard goes straight in.
        let clip = cx
            .read_from_clipboard()
            .and_then(|c| c.text())
            .map(|t| t.trim().to_string())
            .filter(|t| t.starts_with(hopper::sharecode::PREFIX) && hopper::sharecode::decode(t).is_ok())
            .unwrap_or_default();
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("HOP-…").default_value(clip));
        let sub = cx.subscribe_in(&input, window, |this: &mut Self, _, e: &InputEvent, _, cx| {
            if matches!(e, InputEvent::Change) {
                this.parse(cx);
                cx.notify();
            }
        });
        let mut f = Self { input, parsed: None, pack: None, _sub: sub, _lookup: None };
        f.parse(cx);
        f
    }

    fn parse(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().trim().to_string();
        self.parsed =
            (!text.is_empty()).then(|| hopper::sharecode::decode(&text).map_err(|e| format!("{e:#}")));
        self.look_up_pack(cx);
    }

    /// Names the code's pack, so it's clear what installs.
    fn look_up_pack(&mut self, cx: &mut Context<Self>) {
        use hopper::sharecode::Base;
        let Some(Ok(share)) = &self.parsed else { return };
        let base = share.base.clone();
        if matches!(base, Base::Plain { .. }) || self.pack.as_ref().is_some_and(|(b, _)| *b == base) {
            return;
        }
        let b2 = base.clone();
        self._lookup = Some(cx.spawn(async move |this, cx| {
            let name = cx
                .background_spawn(async move {
                    let (m, f) = match &b2 {
                        Base::Curse { project, file } => {
                            (hopper::cf::get_mod(*project)?, hopper::cf::file(*project, *file)?)
                        }
                        Base::Modrinth { project, version } => {
                            let (p, v) = (hopper::modrinth::num_id(project), hopper::modrinth::num_id(version));
                            (hopper::source::get_mod(p)?, hopper::source::file(p, v)?)
                        }
                        Base::Plain { .. } => anyhow::bail!("no pack"),
                    };
                    anyhow::Ok(format!("{} ({})", m.name, f.display_name))
                })
                .await;
            this.update(cx, |this, cx| {
                if let Ok(n) = name {
                    this.pack = Some((base, n));
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn ready(&self) -> bool {
        matches!(self.parsed, Some(Ok(_)))
    }

    fn install(form: &Entity<Self>, store: &Entity<Store>, cx: &mut App) -> bool {
        let this = form.read(cx);
        if !this.ready() {
            return false;
        }
        let code = this.input.read(cx).value().to_string();
        let s = store.read(cx);
        let root = PathBuf::from(&s.settings().instances_root);
        s.spawn_job("Setting up a shared pack", move |job| {
            job.announce();
            let inst = hopper::sharecode::install(&root, &code, job)?;
            job.detail(format!("Installed {}", inst.name));
            Ok(Msg::InstanceChanged(inst.root))
        });
        true
    }
}

impl Render for RedeemForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let status = match &self.parsed {
            None => note(
                "Paste the code a friend sent you. Hopper installs the pack and the same mods as a new instance.",
                cx,
            )
            .into_any_element(),
            Some(Err(e)) => div().text_sm().text_color(theme.danger).child(e.clone()).into_any_element(),
            Some(Ok(s)) => {
                use hopper::sharecode::Base;
                let what = match &s.base {
                    Base::Plain { game_version, loader } => {
                        format!("Minecraft {game_version} with {}", nice_loader(loader))
                    }
                    b => match self.pack.as_ref().filter(|(pb, _)| pb == b) {
                        Some((_, n)) => n.clone(),
                        None if matches!(b, Base::Curse { .. }) => "A CurseForge modpack".into(),
                        None => "A Modrinth modpack".into(),
                    },
                };
                let name = s
                    .name
                    .as_ref()
                    .map(|n| format!(" as \u{201c}{n}\u{201d}"))
                    .unwrap_or_default();
                div()
                    .text_sm()
                    .text_color(theme.success)
                    .child(format!("{what}{name}: {}.", s.summary()))
                    .into_any_element()
            }
        };
        v_flex().gap_3().child(Input::new(&self.input)).child(status)
    }
}

pub fn open_redeem(store: Entity<Store>, window: &mut Window, cx: &mut App) {
    let form = cx.new(|cx| RedeemForm::new(window, cx));
    window.open_dialog(cx, move |dialog, window, cx| {
        let (store, f2) = (store.clone(), form.clone());
        let ready = form.read(cx).ready();
        dialog
            .title("Install from a share code")
            .w(rems(36.).to_pixels(window.rem_size()))
            .child(form.clone())
            .footer(footer("Install", ready))
            .on_ok(move |_, _, cx| RedeemForm::install(&f2, &store, cx))
    });
}
