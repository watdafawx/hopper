//! An instance's Settings tab: name, Minecraft and loader, memory and Java,
//! window size and which updates to offer.

use crate::java_form::{FIELD_W, FORM_W, JavaForm, settings_form};
use crate::store::{Msg, Store, loader_label, open_path};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::form::Field;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::cf::{self, LoaderVersion};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

const CHANNELS: [(u32, &str); 3] = [
    (1, "Release only"),
    (2, "Beta and release"),
    (3, "Alpha, beta and release"),
];

pub struct InstanceSettings {
    store: Entity<Store>,
    root: Option<PathBuf>,
    /// An instance to load on the next frame (loading needs the window).
    pending: Option<PathBuf>,
    name: Entity<InputState>,
    width: Entity<InputState>,
    height: Entity<InputState>,
    java: Entity<JavaForm>,
    game_version: String,
    loader: String,
    channel: u32,
    /// Settings > Minecraft applies here.
    use_my_options: bool,
    has_pack: bool,
    versions: Vec<String>,
    loaders: HashMap<String, Vec<LoaderVersion>>,
    /// What changes between the installed loader and the one picked:
    /// (picked, the builds in between or why not).
    loader_changes: Option<(String, Result<Vec<hopper::loader_changes::Change>, String>)>,
    /// How CurseForge's own file differs (loader or pack version), when it does.
    cf_loader: Option<String>,
    /// Writing the loader and pack into CurseForge's file.
    cf_writing: bool,
    _subscriptions: Vec<Subscription>,
}

impl InstanceSettings {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx));
        let width = cx.new(|cx| InputState::new(window, cx));
        let height = cx.new(|cx| InputState::new(window, cx));
        let default_mem = store.read(cx).settings().default_memory_mb;
        let java = cx.new(|cx| JavaForm::new(store.clone(), Some(default_mem), window, cx));
        let mut subs = Vec::new();
        for input in [&name, &width, &height] {
            subs.push(cx.subscribe(input, |_, _, e: &InputEvent, cx| {
                if matches!(e, InputEvent::Change) {
                    cx.notify();
                }
            }));
        }
        subs.push(cx.observe(&java, |_, _, cx| cx.notify()));
        let view = Self {
            store,
            root: None,
            pending: None,
            name,
            width,
            height,
            java,
            game_version: String::new(),
            loader: String::new(),
            channel: 1,
            use_my_options: true,
            has_pack: false,
            versions: Vec::new(),
            loaders: HashMap::new(),
            loader_changes: None,
            cf_loader: None,
            cf_writing: false,
            _subscriptions: subs,
        };
        cx.spawn(async move |this, cx| {
            let v = cx
                .background_spawn(async { cf::minecraft_versions() })
                .await;
            this.update(cx, |this, cx| {
                this.versions = v.unwrap_or_default();
                cx.notify();
            })
            .ok();
        })
        .detach();
        view
    }

    /// Shows an instance's settings (dropping unsaved edits).
    pub fn show_for(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.pending = Some(root);
        cx.notify();
    }

    fn load(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(inst) = self.store.read(cx).instance(&root).cloned() else {
            return;
        };
        self.root = Some(root);
        let s = &inst.settings;
        let (name, w, h) = (inst.name.clone(), s.width.to_string(), s.height.to_string());
        self.name.update(cx, |i, cx| i.set_value(name, window, cx));
        self.width.update(cx, |i, cx| i.set_value(w, window, cx));
        self.height.update(cx, |i, cx| i.set_value(h, window, cx));
        let (mem, path, args, gv) = (
            s.memory_mb,
            s.java_path.clone(),
            s.java_args.clone(),
            inst.game_version.clone(),
        );
        self.java
            .update(cx, |j, cx| j.set(mem, &path, &args, Some(gv), window, cx));
        self.game_version = inst.game_version.clone();
        self.loader = inst.loader.clone();
        self.channel = s.release_channel;
        self.use_my_options = s.use_my_options;
        self.has_pack = inst.pack.is_some();
        self.want_loaders(cx);
        self.check_cf(inst, cx);
        cx.notify();
    }

    /// Reads CurseForge's file off the UI thread: does it name another loader?
    fn check_cf(&mut self, inst: hopper::instances::Instance, cx: &mut Context<Self>) {
        self.cf_loader = None;
        cx.spawn(async move |this, cx| {
            let root = inst.root.clone();
            let other = cx
                .background_spawn(async move {
                    let loader = hopper::cf_instance::differs(&inst)
                        .map(|(name, _)| name)
                        .filter(|n| !n.is_empty())
                        .map(|n| format!("CurseForge still starts {}.", loader_label(&n)));
                    loader
                        .or_else(|| {
                            hopper::cf_instance::pack_differs(&inst).map(|f| match f.as_str() {
                                "" => "CurseForge still lists another pack version.".to_string(),
                                f => format!("CurseForge still lists {f}."),
                            })
                        })
                        .or_else(|| match hopper::cf_instance::locks_differ(&inst) {
                            0 => None,
                            1 => Some("CurseForge disagrees about 1 locked mod.".to_string()),
                            n => Some(format!("CurseForge disagrees about {n} locked mods.")),
                        })
                })
                .await;
            this.update(cx, |this, cx| {
                if this.root.as_ref() == Some(&root) {
                    this.cf_loader = other;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Writes the saved loader into CurseForge's file.
    fn update_cf(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut inst) = self.root.as_ref().and_then(|r| self.store.read(cx).instance(r).cloned()) else {
            return;
        };
        self.cf_writing = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let r = cx
                .background_spawn(async move {
                    use hopper::cf_instance::Synced;
                    let pack = hopper::cf_instance::write_pack(&mut inst)?;
                    let loader = if pack == Synced::CurseForgeOpen {
                        pack
                    } else {
                        hopper::cf_instance::write_loader(&mut inst)?
                    };
                    let locks = if loader == Synced::CurseForgeOpen {
                        loader
                    } else {
                        hopper::cf_instance::write_locks(&mut inst)?
                    };
                    let all = [pack, loader, locks];
                    let r = if all.contains(&Synced::CurseForgeOpen) {
                        Synced::CurseForgeOpen
                    } else if all.contains(&Synced::Written) {
                        inst.save()?;
                        Synced::Written
                    } else {
                        Synced::Nothing
                    };
                    anyhow::Ok((r, inst))
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.cf_writing = false;
                use hopper::cf_instance::Synced;
                match r {
                    Ok((Synced::CurseForgeOpen, _)) => window.push_notification(
                        Notification::warning("Close CurseForge first: it writes its own copy back when it exits."),
                        cx,
                    ),
                    Ok((_, inst)) => {
                        let root = inst.root.clone();
                        this.store.read(cx).spawn(move || Msg::InstanceChanged(root));
                        this.check_cf(inst, cx);
                        window.push_notification(Notification::success("CurseForge now matches this instance."), cx);
                    }
                    Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// "What changes" between the installed loader and the picked one.
    fn loader_changes_view(&self, cx: &App) -> Option<AnyElement> {
        let (picked, r) = self.loader_changes.as_ref()?;
        if *picked != self.loader {
            return None;
        }
        let installed = self.installed_loader(cx)?;
        let theme = gpui_kit::component::ActiveTheme::theme(cx);
        let ver = |l: &str| l.split_once('-').map_or(String::new(), |(_, v)| v.to_string());
        let back = hopper::scan::compare_versions(&ver(picked), &ver(&installed)) == std::cmp::Ordering::Less;
        let title = if back {
            format!("Going back from {} undoes:", loader_label(&installed))
        } else {
            format!("New since {}:", loader_label(&installed))
        };
        let body: AnyElement = match r {
            Ok(v) if v.is_empty() => widgets::caption("Reading the changelog…", cx).text_xs().into_any_element(),
            Ok(v) => v_flex()
                .gap_1p5()
                .children(v.iter().take(60).map(|c| {
                    h_flex()
                        .gap_2()
                        .items_start()
                        .text_xs()
                        .child(
                            div()
                                .flex_none()
                                .font_family(theme.mono_font_family.clone())
                                .text_color(theme.muted_foreground)
                                .child(c.version.clone()),
                        )
                        .child(div().flex_1().min_w_0().child(c.text.clone()))
                }))
                .into_any_element(),
            Err(e) => widgets::caption(e.clone(), cx).text_xs().into_any_element(),
        };
        Some(
            v_flex()
                .mt_1()
                .gap_2()
                .p_3()
                .max_w(rems(40.))
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .child(div().text_sm().font_semibold().child(title))
                .child(
                    div()
                        .id("loader-changes")
                        // A set height to scroll in, when the list is long.
                        .when(r.as_ref().is_ok_and(|v| v.len() > 6), |d| d.h(rems(16.)))
                        .overflow_y_scrollbar()
                        .child(body),
                )
                .into_any_element(),
        )
    }

    /// The installed loader of the shown instance.
    fn installed_loader(&self, cx: &App) -> Option<String> {
        let root = self.root.as_ref()?;
        Some(self.store.read(cx).instance(root)?.loader.clone())
    }

    /// Fetches what changed between the installed loader and the one picked.
    fn want_loader_changes(&mut self, cx: &mut Context<Self>) {
        let Some(installed) = self.installed_loader(cx) else { return };
        let picked = self.loader.clone();
        if picked == installed || picked.ends_with('-') {
            self.loader_changes = None;
            return;
        }
        let (Some((kind, from)), Some((kind2, to))) = (installed.split_once('-'), picked.split_once('-')) else {
            self.loader_changes = None;
            return;
        };
        if kind != kind2 {
            self.loader_changes = None;
            return;
        }
        let (kind, from, to, mc) = (kind.to_string(), from.to_string(), to.to_string(), self.game_version.clone());
        self.loader_changes = Some((picked.clone(), Ok(Vec::new())));
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move {
                    hopper::loader_changes::between(&kind, &mc, &from, &to).map_err(|e| format!("{e:#}"))
                })
                .await;
            this.update(cx, |this, cx| {
                // Only if that's still the pick.
                if this.loader == picked {
                    this.loader_changes = Some((picked, r));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn want_loaders(&mut self, cx: &mut Context<Self>) {
        let gv = self.game_version.clone();
        if gv.is_empty() || self.loaders.contains_key(&gv) {
            return;
        }
        self.loaders.insert(gv.clone(), Vec::new());
        cx.spawn(async move |this, cx| {
            let key = gv.clone();
            let r = cx
                .background_spawn(async move { cf::loader_versions(&gv) })
                .await;
            this.update(cx, |this, cx| {
                match r {
                    Ok(v) => {
                        this.loaders.insert(key, v);
                    }
                    // Not cached: the next look tries again.
                    Err(e) => {
                        log::warn!("loader versions: {e:#}");
                        this.loaders.remove(&key);
                    }
                }
                this.pick_recommended();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// After a Minecraft change the loader is just "kind-"; take the
    /// recommended build for the new version.
    fn pick_recommended(&mut self) {
        let Some(kind) = self.loader.strip_suffix('-') else {
            return;
        };
        let prefix = format!("{kind}-");
        let options: Vec<&LoaderVersion> = self
            .loaders
            .get(&self.game_version)
            .into_iter()
            .flatten()
            .filter(|v| v.name.starts_with(&prefix))
            .collect();
        if let Some(v) = options.iter().find(|v| v.recommended).or(options.first()) {
            self.loader = v.name.clone();
        }
    }

    /// The instance as the form would save it.
    fn edited(&self, cx: &App) -> Option<hopper::instances::Instance> {
        let mut inst = self.store.read(cx).instance(self.root.as_ref()?)?.clone();
        let num = |e: &Entity<InputState>, cx: &App, lo: u32, hi: u32, fallback: u32| {
            e.read(cx)
                .value()
                .trim()
                .parse::<u32>()
                .map_or(fallback, |v| v.clamp(lo, hi))
        };
        let name = self.name.read(cx).value().trim().to_string();
        if !name.is_empty() {
            inst.name = name;
        }
        inst.settings.width = num(&self.width, cx, 640, 7680, inst.settings.width);
        inst.settings.height = num(&self.height, cx, 480, 4320, inst.settings.height);
        let (mem, path, args) = self.java.read(cx).values();
        inst.settings.memory_mb = mem;
        inst.settings.java_path = path;
        inst.settings.java_args = args;
        inst.settings.release_channel = self.channel;
        inst.settings.use_my_options = self.use_my_options;
        if !self.has_pack {
            inst.game_version = self.game_version.clone();
        }
        if !self.loader.ends_with('-') {
            inst.loader = self.loader.clone();
        }
        Some(inst)
    }

    /// The form differs from what is saved.
    fn dirty(&self, cx: &App) -> bool {
        let (Some(root), Some(edited)) = (self.root.as_ref(), self.edited(cx)) else {
            return false;
        };
        let Some(saved) = self.store.read(cx).instance(root) else {
            return false;
        };
        let (a, b) = (&edited.settings, &saved.settings);
        edited.name != saved.name
            || edited.game_version != saved.game_version
            || edited.loader != saved.loader
            || (a.memory_mb, a.width, a.height, a.release_channel, a.use_my_options)
                != (b.memory_mb, b.width, b.height, b.release_channel, b.use_my_options)
            || a.java_path != b.java_path
            || a.java_args.trim() != b.java_args.trim()
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(root), Some(inst)) = (self.root.clone(), self.edited(cx)) else {
            return;
        };
        let before = self.store.read(cx).instance(&root).map(|i| (i.loader.clone(), i.game_version.clone()));
        let new_loader = before != Some((inst.loader.clone(), inst.game_version.clone()));
        let saved = inst.clone();
        match inst.save() {
            Ok(()) => {
                let store = self.store.read(cx);
                let r = root.clone();
                store.spawn(move || Msg::InstanceChanged(r));
                if new_loader {
                    store.install_loader(saved);
                    // Its changes are now the installed version's.
                    self.loader_changes = None;
                }
                window.push_notification(Notification::success("Settings saved."), cx);
            }
            Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
        }
        cx.notify();
    }
}

impl Render for InstanceSettings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(r) = self.pending.take() {
            cx.defer_in(window, move |this, window, cx| this.load(r, window, cx));
        }
        let Some(root) = self.root.clone() else {
            return div().into_any_element();
        };
        let rem = window.rem_size();
        let view = cx.entity();
        let dirty = self.dirty(cx);

        let minecraft: AnyElement = if self.has_pack {
            widgets::caption(format!("{} (set by the modpack)", self.game_version), cx)
                .into_any_element()
        } else {
            let items = self
                .versions
                .iter()
                .map(|v| (v.clone(), *v == self.game_version))
                .collect();
            let versions = self.versions.clone();
            let view = view.clone();
            let pick = Rc::new(move |ix: usize, _: &mut Window, cx: &mut App| {
                if let Some(v) = versions.get(ix).cloned() {
                    view.update(cx, |this, cx| {
                        if this.game_version != v {
                            this.game_version = v;
                            // The loader build must match; pick one again.
                            if let Some(kind) =
                                this.loader.split('-').next().filter(|k| !k.is_empty())
                            {
                                this.loader = format!("{kind}-");
                            }
                            this.want_loaders(cx);
                            this.pick_recommended();
                            cx.notify();
                        }
                    });
                }
            });
            div()
                .w_full().max_w(rems(FIELD_W))
                .child(widgets::picker(
                    "mc",
                    self.game_version.clone(),
                    items,
                    rem,
                    pick,
                ))
                .into_any_element()
        };

        let kind = self.loader.split('-').next().unwrap_or("").to_string();
        let loader: AnyElement = if kind.is_empty() {
            widgets::caption("Vanilla", cx).into_any_element()
        } else {
            let prefix = format!("{kind}-");
            let options: Vec<LoaderVersion> = self
                .loaders
                .get(&self.game_version)
                .into_iter()
                .flatten()
                .filter(|v| v.name.starts_with(&prefix))
                .cloned()
                .collect();
            let items = options
                .iter()
                .map(|v| {
                    let tag = if v.recommended {
                        "  (recommended)"
                    } else if v.latest {
                        "  (latest)"
                    } else {
                        ""
                    };
                    (
                        format!("{}{tag}", loader_label(&v.name)),
                        v.name == self.loader,
                    )
                })
                .collect();
            let names: Vec<String> = options.iter().map(|v| v.name.clone()).collect();
            let view = view.clone();
            let pick = Rc::new(move |ix: usize, _: &mut Window, cx: &mut App| {
                if let Some(n) = names.get(ix).cloned() {
                    view.update(cx, |this, cx| {
                        this.loader = n;
                        this.want_loader_changes(cx);
                        cx.notify();
                    });
                }
            });
            v_flex()
                .gap_1p5()
                .child(
                    h_flex()
                        .w_full().max_w(rems(FIELD_W))
                        .gap_2()
                        .child(div().flex_1().min_w_0().child(widgets::picker(
                            "loader",
                            loader_label(&self.loader),
                            items,
                            rem,
                            pick,
                        )))
                        .child({
                            let form = cx.entity();
                            Button::new("loader-changelog")
                                .small()
                                .ghost()
                                .icon(IconName::ScrollText)
                                .tooltip("Loader versions and changelog")
                                .on_click(move |_, window, cx| open_loader_log(form.clone(), window, cx))
                        }),
                )
                .when(self.has_pack, |r| {
                    r.child(
                        widgets::caption(
                            "The modpack chose this; its next update sets it back.",
                            cx,
                        )
                        .text_xs(),
                    )
                })
                .child(widgets::caption("Saving installs the new version.", cx).text_xs())
                .children(self.cf_loader.clone().map(|other| {
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            widgets::caption(other, cx)
                            .text_xs()
                            .text_color(gpui_kit::component::ActiveTheme::theme(&**cx).warning),
                        )
                        .child(
                            Button::new("update-cf")
                                .xsmall()
                                .outline()
                                .label("Update CurseForge")
                                .loading(self.cf_writing)
                                .disabled(self.cf_writing || dirty)
                                .tooltip("Write this instance's saved loader, pack version and mod locks into CurseForge's minecraftinstance.json")
                                .on_click(cx.listener(|this, _, w, cx| this.update_cf(w, cx))),
                        )
                }))
                .children(self.loader_changes_view(cx))
                .into_any_element()
        };

        let channel_items = CHANNELS
            .iter()
            .map(|(c, l)| (l.to_string(), *c == self.channel))
            .collect();
        let channel_label = CHANNELS
            .iter()
            .find(|(c, _)| *c == self.channel)
            .map_or("Release only", |(_, l)| l)
            .to_string();
        let pick_channel = {
            let view = view.clone();
            Rc::new(move |ix: usize, _: &mut Window, cx: &mut App| {
                view.update(cx, |this, cx| {
                    this.channel = CHANNELS[ix].0;
                    cx.notify();
                });
            })
        };

        let top = settings_form(rem)
            .child(
                Field::new()
                    .label("Name")
                    .child(div().w_full().max_w(rems(FIELD_W)).child(Input::new(&self.name))),
            )
            .child(Field::new().label("Minecraft").child(minecraft))
            .child(Field::new().label("Loader").items_start().child(loader));
        let bottom = settings_form(rem)
            .child(
                Field::new().label("Window size").child(
                    h_flex()
                        .gap_2()
                        .child(div().w_24().child(Input::new(&self.width)))
                        .child(widgets::caption("×", cx))
                        .child(div().w_24().child(Input::new(&self.height))),
                ),
            )
            .child(
                Field::new()
                    .label("Updates")
                    .items_start()
                    .description("Least stable files offered as updates. A mod already on a beta still gets newer betas.")
                    .child(div().w_full().max_w(rems(FIELD_W)).child(widgets::picker(
                        "channel",
                        channel_label,
                        channel_items,
                        rem,
                        pick_channel,
                    ))),
            )
            .child(
                Field::new()
                    .label("Minecraft settings")
                    .items_start()
                    .description("Your sound, mouse, view and keys from Settings > Minecraft, set before the game starts. Off leaves this instance's options as they are.")
                    .child(
                        Switch::new("use-my-options")
                            .checked(self.use_my_options)
                            .label("Use my Minecraft settings")
                            .on_click(cx.listener(|this, on: &bool, _, cx| {
                                this.use_my_options = *on;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                Field::new().label("Folder").child(
                    h_flex()
                        .gap_2()
                        .child(widgets::caption(root.display().to_string(), cx))
                        .child(
                            Button::new("settings-folder")
                                .ghost()
                                .xsmall()
                                .icon(IconName::FolderOpen)
                                .tooltip("Open")
                                .on_click(move |_, _, _| open_path(&root)),
                        ),
                ),
            );
        // The Save bar stays in view under the scrolling form.
        let theme = cx.theme();
        // Its buttons line up with the centred form above.
        let save_bar = h_flex()
            .flex_none()
            .justify_center()
            .px_6()
            .py_2p5()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(
                h_flex()
                    .w_full()
                    .max_w(rems(FORM_W))
                    .gap_3()
                    .child(
                        Button::new("save-settings")
                            .primary()
                            .label("Save")
                            .disabled(!dirty)
                            .on_click(cx.listener(|this, _, w, cx| this.save(w, cx))),
                    )
                    .child(if dirty {
                        widgets::caption("Unsaved changes", cx).font_medium()
                    } else {
                        widgets::caption("All saved", cx)
                    }),
            );
        v_flex()
            .size_full()
            .child(
                // Centred in a column like the Settings page, rather than
                // left-aligned with the room to its right empty.
                v_flex()
                    .id("instance-settings")
                    .flex_1()
                    .min_h_0()
                    .px_6()
                    .pb_6()
                    .items_center()
                    .child(
                        v_flex()
                            .w_full()
                            .max_w(rems(FORM_W))
                            .gap_3()
                            .child(top)
                            .child(self.java.clone())
                            .child(bottom),
                    )
                    .overflow_y_scrollbar(),
            )
            .child(save_bar)
            .into_any_element()
    }
}

/// A loader's builds and what each changed, as a dialog like the pack and
/// mod changelogs. "Use this version" picks it in the Loader dropdown
/// (Save applies it).
struct LoaderLog {
    store: Entity<Store>,
    root: PathBuf,
    kind: String,
    installed: String,
    /// Builds offered for this Minecraft version, newest first.
    offered: Vec<String>,
    log: Option<Result<Vec<hopper::loader_changes::Change>, String>>,
    /// Told the build picked, after it's saved (the settings form follows).
    on_used: Option<Rc<dyn Fn(&str, &mut App)>>,
}

impl Render for LoaderLog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = gpui_kit::component::ActiveTheme::theme(&**cx).clone();
        // Window-tall less the dialog's top offset, title and padding.
        let total_h = (window.viewport_size().height - window.rem_size() * 9.).max(window.rem_size() * 12.);
        let loaded = matches!(self.log, Some(Ok(_)));
        let name = loader_label(&format!("{}-0", self.kind))
            .rsplit_once(' ')
            .map_or(self.kind.clone(), |(n, _)| n.to_string());
        let body: AnyElement = match &self.log {
            None => h_flex()
                .gap_2()
                .child(gpui_kit::component::spinner::Spinner::new().small())
                .child(widgets::caption("Reading the changelog", cx))
                .into_any_element(),
            Some(Err(e)) => div().text_sm().text_color(theme.danger).child(e.clone()).into_any_element(),
            Some(Ok(log)) => {
                let installed_ver = self.installed.split_once('-').map_or("", |(_, v)| v).to_string();
                let newer = |v: &str| {
                    hopper::scan::compare_versions(v, &installed_ver) == std::cmp::Ordering::Greater
                };
                let behind = log.iter().filter(|c| newer(&c.version)).count();
                let cards: Vec<AnyElement> = log
                    .iter()
                    .take(80)
                    .enumerate()
                    .map(|(i, c)| {
                        let installed = c.version == installed_ver;
                        let target = i == 0 && !installed;
                        let full = format!("{}-{}", self.kind, c.version);
                        let usable = self.offered.contains(&full);
                        let (store, root, on_used) = (self.store.clone(), self.root.clone(), self.on_used.clone());
                        v_flex()
                            .gap_1p5()
                            .p_3()
                            .rounded(theme.radius_lg)
                            .border_1()
                            .border_color(if target { theme.primary } else { theme.border })
                            .when(installed, |c| c.bg(theme.list_active))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(div().font_semibold().child(format!("{name} {}", c.version)))
                                    .when(installed, |r| {
                                        r.child(widgets::chip(cx.theme().success).xsmall().child("Installed"))
                                    })
                                    .child(div().flex_1())
                                    .when(!installed && usable, |r| {
                                        let b = Button::new(SharedString::from(format!("use:{}", c.version)))
                                            .small()
                                            .label("Use this version");
                                        let b = if target { b.primary() } else { b.outline() };
                                        r.child(b.on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            if switch_loader(&store, &root, full.clone(), window, cx)
                                                && let Some(f) = &on_used
                                            {
                                                f(&full, cx);
                                            }
                                        }))
                                    }),
                            )
                            .child(div().text_sm().child(if c.text.is_empty() {
                                "No notes for this build.".to_string()
                            } else {
                                c.text.clone()
                            }))
                            .into_any_element()
                    })
                    .collect();
                v_flex()
                    .gap_3()
                    .child(widgets::caption(
                        match behind {
                            0 => "Up to date".to_string(),
                            1 => "1 build newer than yours".to_string(),
                            n => format!("{n} builds newer than yours, newest first"),
                        },
                        cx,
                    ))
                    .children(cards)
                    .into_any_element()
            }
        };
        v_flex()
            .when(loaded, |c| c.h(total_h))
            .gap_3()
            .child(widgets::caption(format!("Installed: {}", loader_label(&self.installed)), cx).text_xs())
            // Until loaded the column has no height; a flex_1 body would
            // shrink to nothing and hide the spinner or error.
            .child(
                div()
                    .id("loader-log")
                    .when(loaded, |d| d.flex_1().min_h_0())
                    .overflow_y_scrollbar()
                    .child(body),
            )
    }
}

/// Moves an instance to `loader` ("neoforge-21.1.265"): saves it and
/// installs it, as saving a new loader in Settings does. Shared by the
/// loader's versions window and the problem card's "Update NeoForge".
pub(crate) fn switch_loader(store: &Entity<Store>, root: &std::path::Path, loader: String, window: &mut Window, cx: &mut App) -> bool {
    let Some(mut inst) = store.read(cx).instance(root).cloned() else { return false };
    if inst.loader == loader {
        return false;
    }
    inst.loader = loader;
    match inst.save() {
        Ok(()) => {
            let s = store.read(cx);
            let r = inst.root.clone();
            s.spawn(move || Msg::InstanceChanged(r));
            let label = loader_label(&inst.loader);
            s.install_loader(inst);
            window.push_notification(Notification::success(format!("Installing {label}.")), cx);
            true
        }
        Err(e) => {
            window.push_notification(Notification::error(format!("{e:#}")), cx);
            false
        }
    }
}

/// Opens an instance's loader builds and changelog over whatever is shown;
/// "Use this version" installs that build right away. `on_used` hears
/// which one (the settings form, to follow).
pub(crate) fn open_loader_versions(
    store: Entity<Store>,
    root: PathBuf,
    on_used: Option<Rc<dyn Fn(&str, &mut App)>>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(inst) = store.read(cx).instance(&root).cloned() else { return };
    let kind = inst.loader.split('-').next().unwrap_or("").to_string();
    if kind.is_empty() {
        return;
    }
    let (installed, mc) = (inst.loader.clone(), inst.game_version.clone());
    let view = cx.new(|_| LoaderLog { store, root, kind: kind.clone(), installed: installed.clone(), offered: Vec::new(), log: None, on_used });
    let v2 = view.clone();
    cx.spawn(async move |cx| {
        let prefix = format!("{kind}-");
        let (offered, log) = cx
            .background_spawn(async move {
                let offered: Vec<String> = cf::loader_versions(&mc)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|v| v.name.starts_with(&prefix))
                    .map(|v| v.name)
                    .collect();
                let newest = offered
                    .first()
                    .and_then(|n| n.split_once('-'))
                    .map(|(_, v)| v.to_string())
                    .unwrap_or_else(|| installed.split_once('-').map_or(String::new(), |(_, v)| v.to_string()));
                let log = hopper::loader_changes::history(&kind, &mc, &newest).map_err(|e| format!("{e:#}"));
                (offered, log)
            })
            .await;
        v2.update(cx, |v, cx| {
            v.offered = offered;
            v.log = Some(log);
            cx.notify();
        });
    })
    .detach();
    window.open_dialog(cx, move |dialog, window, _| {
        dialog
            .title("Loader versions and changelog")
            .w(rems(42.).to_pixels(window.rem_size()))
            .margin_top(rems(3.5).to_pixels(window.rem_size()))
            .child(view.clone())
    });
}

/// The versions window from the settings form: the form follows the
/// build picked.
fn open_loader_log(form: Entity<InstanceSettings>, window: &mut Window, cx: &mut App) {
    let f = form.read(cx);
    let Some(root) = f.root.clone() else { return };
    let store = f.store.clone();
    let on_used: Rc<dyn Fn(&str, &mut App)> = Rc::new(move |loader: &str, cx: &mut App| {
        let loader = loader.to_string();
        form.update(cx, |f, cx| {
            f.loader = loader;
            // Its changes are now the installed version's.
            f.loader_changes = None;
            cx.notify();
        });
    });
    open_loader_versions(store, root, Some(on_used), window, cx);
}
