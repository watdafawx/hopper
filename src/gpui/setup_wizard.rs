//! The first-run setup: a few steps in a dialog (install, the CurseForge
//! key, instances, servers, the theme). Shown on the first start (no
//! settings yet), and again from Settings or Ctrl+K.
//!
//! Nothing is written until the user acts: a step's choices are saved on
//! Next, only when they differ from the settings, and the theme previews
//! live but is only kept on Next (Skip puts the old one back).

use crate::app::{AppView, Page};
use crate::launchers_list::LaunchersList;
use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::Path;

const STEPS: [&str; 6] = ["Welcome", "CurseForge", "Instances", "Servers", "Look", "Done"];
const CF_CONSOLE: &str = "https://console.curseforge.com/";

/// Where the user's servers are.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Servers {
    None,
    ThisPc,
    OtherPc,
}

pub struct SetupWizard {
    store: Entity<Store>,
    app: WeakEntity<AppView>,
    /// Opened with `--setup` or over MCP: finishing or skipping doesn't
    /// mark the setup done.
    preview: bool,
    step: usize,
    installing: bool,
    install_result: Option<Result<String, String>>,
    cf_key: Entity<InputState>,
    cf_testing: bool,
    cf_result: Option<Result<String, String>>,
    instances_root: Entity<InputState>,
    /// Instances found in a folder: (folder, count), None while counting.
    counted: Option<(String, Option<usize>)>,
    launchers: Entity<LaunchersList>,
    servers: Servers,
    servers_root: Entity<InputState>,
    server_host: Entity<InputState>,
    /// The theme as saved when the setup opened (name, old mode).
    theme_saved: (String, u8),
    /// A theme shown but not saved yet.
    theme_preview: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl SetupWizard {
    fn new(
        store: Entity<Store>,
        app: WeakEntity<AppView>,
        preview: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let s = store.read(cx).settings().clone();
        let input = |v: &str, placeholder: &str, window: &mut Window, cx: &mut Context<Self>| {
            let (v, p) = (v.to_string(), placeholder.to_string());
            cx.new(|cx| InputState::new(window, cx).placeholder(p).default_value(v))
        };
        let instances_root = input(&s.instances_root, "C:\\…\\Instances", window, cx);
        let servers_root = input(&s.servers_root, "\\\\PC-NAME\\servers  or  S:\\servers", window, cx);
        let host = if s.server_host == "localhost" { "" } else { s.server_host.as_str() };
        let server_host = input(host, "PC-NAME", window, cx);
        let cf_key = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("Paste your CurseForge API key")
        });
        let mut subs = Vec::new();
        for i in [&cf_key, &servers_root, &server_host] {
            subs.push(cx.subscribe(i, |_, _, e: &InputEvent, cx| {
                if matches!(e, InputEvent::Change) {
                    cx.notify();
                }
            }));
        }
        subs.push(cx.subscribe(&instances_root, |this, _, e: &InputEvent, cx| {
            if matches!(e, InputEvent::Change) {
                this.count_instances(cx);
            }
        }));
        let mut this = Self {
            launchers: cx.new(|_| LaunchersList::new(store.clone())),
            store,
            app,
            preview,
            step: 0,
            installing: false,
            install_result: None,
            cf_key,
            cf_testing: false,
            cf_result: None,
            instances_root,
            counted: None,
            servers: if s.servers_root.is_empty() { Servers::None } else { Servers::OtherPc },
            servers_root,
            server_host,
            theme_saved: (s.theme_name.clone(), s.theme),
            theme_preview: None,
            _subscriptions: subs,
        };
        this.count_instances(cx);
        this
    }

    fn typed(input: &Entity<InputState>, cx: &App) -> String {
        input.read(cx).value().trim().to_string()
    }

    /// Counts the instances in the typed folder (in the background).
    fn count_instances(&mut self, cx: &mut Context<Self>) {
        let dir = Self::typed(&self.instances_root, cx);
        if self.counted.as_ref().is_some_and(|(d, _)| *d == dir) {
            return;
        }
        self.counted = Some((dir.clone(), None));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let probe = dir.clone();
            let n = cx
                .background_spawn(async move { hopper::instances::discover(Path::new(&probe)).len() })
                .await;
            this.update(cx, |this, cx| {
                if this.counted.as_ref().is_some_and(|(d, _)| *d == dir) {
                    this.counted = Some((dir, Some(n)));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    // ---- moving between steps ------------------------------------------------

    /// Keeps what this step chose, then shows step `to`.
    fn go(&mut self, to: usize, cx: &mut Context<Self>) {
        if to == self.step || to >= STEPS.len() {
            return;
        }
        self.commit(cx);
        self.step = to;
        cx.notify();
    }

    fn next(&mut self, cx: &mut Context<Self>) {
        self.go(self.step + 1, cx);
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        self.go(self.step.saturating_sub(1), cx);
    }

    /// Saves the current step's choices that differ from the settings.
    fn commit(&mut self, cx: &mut Context<Self>) {
        let s = self.store.read(cx).settings().clone();
        match STEPS[self.step] {
            "Instances" => {
                let dir = Self::typed(&self.instances_root, cx);
                if !dir.is_empty() && dir != s.instances_root {
                    self.store.update(cx, |st, cx| {
                        st.update_settings(|s| s.instances_root = dir, cx);
                        st.refresh();
                        cx.notify();
                    });
                }
            }
            "Servers" if self.servers == Servers::OtherPc => {
                let root = Self::typed(&self.servers_root, cx);
                let host = Self::typed(&self.server_host, cx);
                let host = if host.is_empty() { s.server_host.clone() } else { host };
                if root != s.servers_root || host != s.server_host {
                    self.store.update(cx, |st, cx| {
                        st.update_settings(
                            |s| {
                                s.servers_root = root;
                                s.server_host = host;
                            },
                            cx,
                        );
                        st.refresh();
                        cx.notify();
                    });
                }
            }
            "Look" => {
                if let Some(name) = self.theme_preview.take()
                    && name != s.theme_name
                {
                    self.store.update(cx, |st, cx| {
                        st.update_settings(
                            |s| {
                                s.theme_name = name.clone();
                                s.theme = 0;
                            },
                            cx,
                        )
                    });
                    self.theme_saved = (name, 0);
                }
            }
            _ => {}
        }
    }

    /// Puts back a theme that was only being looked at.
    fn revert_theme(&mut self, cx: &mut Context<Self>) {
        if self.theme_preview.take().is_some() {
            let (name, mode) = self.theme_saved.clone();
            let zoom = self.store.read(cx).settings().zoom;
            crate::theme::apply(&name, mode, zoom, cx);
        }
    }

    /// Ends the setup: marks it done (unless previewing) and closes.
    fn close(&mut self, library: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.revert_theme(cx);
        if !self.preview && !self.store.read(cx).settings().setup_done {
            self.store
                .update(cx, |st, cx| st.update_settings(|s| s.setup_done = true, cx));
        }
        window.close_dialog(cx);
        if library {
            self.app.update(cx, |a, cx| a.go(Page::Library, cx)).ok();
        }
    }

    // ---- actions ---------------------------------------------------------------

    fn install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.installing = true;
        self.install_result = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let r = cx
                .background_spawn(async move { hopper::setup::install(true) })
                .await;
            this.update(cx, |this, cx| {
                this.installing = false;
                this.install_result = Some(match r {
                    Ok(_) => Ok("Installed. Find Hopper in the Start menu or on the desktop.".into()),
                    Err(e) => Err(format!("{e:#}")),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Tests the pasted key (or the current one), and saves a pasted key
    /// that works.
    fn test_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = Self::typed(&self.cf_key, cx);
        let key = if typed.is_empty() {
            match hopper::cf::api_key() {
                Some(k) => k.to_string(),
                None => {
                    self.cf_result = Some(Err("Paste your key first.".into()));
                    cx.notify();
                    return;
                }
            }
        } else {
            typed.clone()
        };
        self.cf_testing = true;
        self.cf_result = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let r = cx
                .background_spawn(async move { hopper::cf::test_key(&key).map(|()| key) })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.cf_testing = false;
                this.cf_result = Some(match r {
                    Ok(key) if !typed.is_empty() => match hopper::cf::set_api_key(&key) {
                        Ok(()) => {
                            this.cf_key.update(cx, |s, cx| s.set_value(String::new(), window, cx));
                            // Browse and updates can run now.
                            this.store.update(cx, |s, cx| {
                                s.refresh();
                                cx.notify();
                            });
                            Ok("It works. Saved in Windows Credential Manager.".into())
                        }
                        Err(e) => Err(format!("The key works, but saving it failed: {e:#}")),
                    },
                    Ok(_) => Ok("It works.".into()),
                    Err(e) => Err(format!("{e:#}")),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn browse(
        input: Entity<InputState>,
        prompt: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(prompt.into()),
        });
        cx.spawn_in(window, async move |_, cx| {
            if let Ok(Ok(Some(p))) = rx.await
                && let Some(p) = p.into_iter().next()
            {
                let p = p.to_string_lossy().into_owned();
                cx.update(|window, cx| input.update(cx, |s, cx| s.set_value(p, window, cx)))
                    .ok();
            }
        })
        .detach();
    }

    fn preview_theme(&mut self, name: String, cx: &mut Context<Self>) {
        let zoom = self.store.read(cx).settings().zoom;
        crate::theme::apply(&name, 0, zoom, cx);
        self.theme_preview = Some(name);
        cx.notify();
    }

    /// The theme picked now: the one being previewed, else the saved one.
    fn current_theme(&self) -> String {
        match &self.theme_preview {
            Some(n) => n.clone(),
            None => match self.theme_saved {
                (ref n, _) if !n.is_empty() => n.clone(),
                (_, 1) => crate::theme::HOPPER_DARK.to_string(),
                (_, 2) => crate::theme::HOPPER_LIGHT.to_string(),
                _ => String::new(),
            },
        }
    }

    // ---- the steps ---------------------------------------------------------------

    fn welcome(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let installed_here = hopper::setup::running_installed();
        let other = hopper::setup::installed_exe();
        let dir = hopper::setup::install_dir()
            .map(|d| d.display().to_string())
            .unwrap_or_default();
        let hero = v_flex()
            .items_center()
            .gap_3()
            .pt_2()
            .child(widgets::glow(widgets::logo().size(rems(4.)), theme.primary, cx))
            .child(div().text_2xl().font_bold().child("Welcome to Hopper"))
            .child(
                div()
                    .max_w(rems(32.))
                    .text_center()
                    .text_color(theme.muted_foreground)
                    .child("Play and update your Minecraft modpacks, from CurseForge and Modrinth, and keep your servers in step with them. A minute of setup and you're in."),
            );
        let body: AnyElement = if installed_here {
            h_flex()
                .justify_center()
                .gap_2()
                .child(widgets::pill("Installed", theme.success, true))
                .child(note(format!("in {dir}. Hopper updates itself."), cx))
                .into_any_element()
        } else {
            let busy = self.installing;
            let install = choice_card(
                "setup-install",
                IconName::Download,
                if other.is_some() { "Install this version" } else { "Install Hopper" },
                if other.is_some() {
                    "A copy is already installed. This replaces it with this one."
                } else {
                    "Start menu and desktop shortcuts, and Hopper updates itself. No admin rights."
                },
                false,
                cx,
            )
            .when(busy, |c| c.opacity(0.6))
            .when(!busy, |c| {
                c.on_click(cx.listener(|this, _, window, cx| this.install(window, cx)))
            });
            let portable = choice_card(
                "setup-portable",
                IconName::Box,
                "Keep it portable",
                "Run Hopper from where it is. You can install it later in Settings.",
                false,
                cx,
            )
            .on_click(cx.listener(|this, _, _, cx| this.next(cx)));
            v_flex()
                .gap_3()
                .child(h_flex().w_full().gap_3().items_stretch().child(install).child(portable))
                .when(busy, |c| c.child(note("Installing…", cx)))
                .when_some(self.install_result.clone(), |c, r| c.child(result_line(r, cx)))
                .into_any_element()
        };
        v_flex().gap_6().child(hero).child(body).into_any_element()
    }

    fn curseforge(&mut self, cx: &mut Context<Self>) -> AnyElement {
        use hopper::cf::KeySource;
        let theme = cx.theme().clone();
        let source = hopper::cf::api_key_source().map(|(_, s)| s);
        let status = match source {
            Some(KeySource::Env) => Some("Already set (from .env)"),
            Some(KeySource::Saved) => Some("Already set (saved)"),
            Some(KeySource::BuiltIn) => Some("Already set (built in)"),
            None => None,
        };
        let typed = !Self::typed(&self.cf_key, cx).is_empty();
        let step = |n: &'static str, text: AnyElement| {
            h_flex()
                .gap_2p5()
                .items_center()
                .child(
                    div()
                        .flex_none()
                        .size(rems(1.375))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(theme.primary.opacity(0.15))
                        .text_color(theme.primary)
                        .text_xs()
                        .font_bold()
                        .child(n),
                )
                .child(div().flex_1().min_w_0().child(text))
        };
        let how = v_flex()
            .text_sm()
            .gap_2()
            .p_3()
            .rounded(theme.radius_lg)
            .bg(theme.foreground.opacity(0.035))
            .border_1()
            .border_color(theme.foreground.opacity(0.08))
            .child(step(
                "1",
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child("Open")
                    .child(
                        Button::new("setup-cf-console")
                            .small()
                            .outline()
                            .icon(IconName::ExternalLink)
                            .label("console.curseforge.com")
                            .on_click(|_, _, cx| cx.open_url(CF_CONSOLE)),
                    )
                    .child("and sign in.")
                    .into_any_element(),
            ))
            .child(step("2", div().child("Go to API keys, and create or copy your key.").into_any_element()))
            .child(step("3", div().child("Paste it here, then Test.").into_any_element()));
        let field = h_flex()
            .gap_2()
            .child(div().flex_1().min_w_0().child(Input::new(&self.cf_key).mask_toggle()))
            .child(
                Button::new("setup-cf-test")
                    .primary()
                    .label(if typed { "Test and save" } else { "Test" })
                    .loading(self.cf_testing)
                    .disabled(self.cf_testing || (!typed && source.is_none()))
                    .on_click(cx.listener(|this, _, window, cx| this.test_key(window, cx))),
            );
        v_flex()
            .gap_4()
            .child(
                v_flex()
                    .gap_1p5()
                    .text_sm()
                    .child("CurseForge requires every app to use an API key for searching, installing and updating. The downloads themselves come from CurseForge's CDN.")
                    .child(
                        div().text_color(theme.muted_foreground).child(
                            "Hopper doesn't ship a shared key, so you use your own: it's free, takes a minute, stays on this PC (in Windows Credential Manager), and only your Hopper uses it.",
                        ),
                    ),
            )
            .child(how)
            .when_some(status, |c, s| {
                c.child(
                    h_flex()
                        .gap_2()
                        .child(widgets::pill(s, theme.success, true))
                        .child(note("Paste another to replace it, or go on.", cx)),
                )
            })
            .child(field)
            .when(self.cf_testing, |c| c.child(note("Asking CurseForge…", cx)))
            .when_some(self.cf_result.clone(), |c, r| c.child(result_line(r, cx)))
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_x_3()
                    .gap_y_1()
                    .items_center()
                    .child(
                        Button::new("setup-cf-skip")
                            .ghost()
                            .small()
                            .label("Skip — use Modrinth only for now")
                            .on_click(cx.listener(|this, _, _, cx| this.next(cx))),
                    )
                    .child(note("Modrinth works without a key.", cx)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(IconName::Info).xsmall().flex_none().mt_0p5())
                    .child(div().flex_1().min_w_0().child(
                        "Some authors only allow downloads from the CurseForge website. Hopper opens those pages, picks the files up from your Downloads folder by itself, then cleans them up.",
                    )),
            )
            .into_any_element()
    }

    fn instances(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let typed = Self::typed(&self.instances_root, cx);
        let found = match &self.counted {
            Some((d, Some(n))) if *d == typed => Some(*n),
            _ => None,
        };
        let missing = !typed.is_empty() && !Path::new(&typed).is_dir();
        let input = self.instances_root.clone();
        let status: AnyElement = if missing {
            h_flex()
                .gap_2()
                .child(widgets::pill("New folder", theme.warning, false))
                .child(note("It doesn't exist yet: Hopper makes it for your first instance.", cx))
                .into_any_element()
        } else {
            match found {
                None => note("Looking…", cx).into_any_element(),
                Some(0) => h_flex()
                    .gap_2()
                    .child(widgets::pill("Start fresh", theme.primary, false))
                    .child(note(
                        "No instances here yet. Create one or install a modpack from Browse.",
                        cx,
                    ))
                    .into_any_element(),
                Some(n) => h_flex()
                    .gap_2()
                    .child(widgets::pill(
                        format!("{n} instance{} found", if n == 1 { "" } else { "s" }),
                        theme.success,
                        true,
                    ))
                    .child(note("They show in your Library as they are.", cx))
                    .into_any_element(),
            }
        };
        v_flex()
            .gap_4()
            .child(
                v_flex()
                    .gap_2()
                    .child(label("Instances folder"))
                    .child(note(
                        "Where your instances live. CurseForge's Instances folder works, so its packs show up as they are.",
                        cx,
                    ))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().flex_1().min_w_0().child(Input::new(&self.instances_root)))
                            .child(
                                Button::new("setup-browse-instances")
                                    .outline()
                                    .icon(IconName::FolderOpen)
                                    .label("Browse…")
                                    .on_click(cx.listener(move |_, _, window, cx| {
                                        Self::browse(input.clone(), "Use this folder", window, cx)
                                    })),
                            ),
                    )
                    .child(status),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(label("Other launchers"))
                    .child(self.launchers.clone()),
            )
            .into_any_element()
    }

    fn servers(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let pick = self.servers;
        let card = |id: &'static str, icon, title, text, v: Servers, cx: &mut Context<Self>| {
            choice_card(id, icon, title, text, pick == v, cx)
                .flex_1()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.servers = v;
                    cx.notify();
                }))
        };
        let cards = h_flex()
            .w_full()
            .gap_3()
            .items_stretch()
            .child(card(
                "setup-srv-none",
                IconName::CircleOff,
                "None for now",
                "Just play. Add servers any time.",
                Servers::None,
                cx,
            ))
            .child(card(
                "setup-srv-here",
                IconName::Monitor,
                "Host on this PC",
                "Hopper runs them: start, stop, console, backups.",
                Servers::ThisPc,
                cx,
            ))
            .child(card(
                "setup-srv-other",
                IconName::HardDrive,
                "Another PC at home",
                "Their folder is shared on your network.",
                Servers::OtherPc,
                cx,
            ));
        let hosted = self.store.read(cx).hosted_root();
        let detail: AnyElement = match pick {
            Servers::None => note(
                "Nothing to set up. The Servers page and Settings → Library & folders are there when you want them.",
                cx,
            )
            .into_any_element(),
            Servers::ThisPc => v_flex()
                .gap_1()
                .text_sm()
                .child("Create a server from any instance later: open it, then ⋯ → Host a server on this PC, or + on the Servers page.")
                .child(note(format!("Hosted servers go in {hosted}."), cx))
                .into_any_element(),
            Servers::OtherPc => {
                let input = self.servers_root.clone();
                v_flex()
                    .gap_3()
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(label("Servers folder"))
                            .child(note(
                                "The shared folder that holds your server folders: a mapped drive or \\\\PC-NAME\\share.",
                                cx,
                            ))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(div().flex_1().min_w_0().child(Input::new(&self.servers_root)))
                                    .child(
                                        Button::new("setup-browse-servers")
                                            .outline()
                                            .icon(IconName::FolderOpen)
                                            .label("Browse…")
                                            .on_click(cx.listener(move |_, _, window, cx| {
                                                Self::browse(input.clone(), "Use this folder", window, cx)
                                            })),
                                    ),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(label("Server host"))
                            .child(note("That PC's name, for status pings.", cx))
                            .child(div().w(rems(16.)).child(Input::new(&self.server_host))),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_start()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(Icon::new(IconName::Info).xsmall().flex_none().mt_0p5())
                            .child(div().flex_1().min_w_0().child(
                                "Runs them with Crafty, MCSS or Pterodactyl? Add the panel later in Settings → Servers for start, stop, console and backups.",
                            )),
                    )
                    .into_any_element()
            }
        };
        v_flex()
            .gap_4()
            .child(cards)
            .child(
                div()
                    .p_3()
                    .rounded(theme.radius_lg)
                    .bg(theme.foreground.opacity(0.035))
                    .border_1()
                    .border_color(theme.foreground.opacity(0.08))
                    .child(detail),
            )
            .into_any_element()
    }

    fn look(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.current_theme();
        let names: Vec<String> = std::iter::once(String::new())
            .chain(crate::theme::names())
            .collect();
        v_flex()
            .gap_3()
            .child(note(
                "Pick one to see it right away. Change it any time in Settings or Ctrl+K.",
                cx,
            ).whitespace_normal())
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .children(names.into_iter().enumerate().map(|(i, name)| {
                        let on = name == current;
                        let n2 = name.clone();
                        theme_card(&name, on, cx)
                            .id(("setup-theme", i))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.preview_theme(n2.clone(), cx)
                            }))
                    })),
            )
            .into_any_element()
    }

    fn done(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let s = self.store.read(cx).settings().clone();
        let installed = hopper::setup::running_installed() || hopper::setup::installed_exe().is_some();
        let key = hopper::cf::api_key().is_some();
        let count = match &self.counted {
            Some((d, Some(n))) if *d == s.instances_root => Some(*n),
            _ => None,
        };
        let (managed, _) = self.launchers.read(cx).counts(cx);
        let theme_name = if s.theme_name.is_empty() { "Follow Windows".to_string() } else { s.theme_name.clone() };
        let row = |icon: IconName, what: &'static str, value: String, ok: bool, cx: &App| {
            let t = cx.theme();
            h_flex()
                .gap_3()
                .px_3()
                .py_2p5()
                .rounded(t.radius_lg)
                .bg(t.foreground.opacity(0.035))
                .border_1()
                .border_color(t.foreground.opacity(0.08))
                .child(
                    Icon::new(icon)
                        .small()
                        .text_color(if ok { t.primary } else { t.muted_foreground }),
                )
                .child(div().w(rems(7.)).flex_none().font_medium().child(what))
                .child(div().flex_1().min_w_0().text_sm().text_color(t.muted_foreground).child(value))
                .child(if ok {
                    Icon::new(IconName::CircleCheck).small().text_color(t.success)
                } else {
                    Icon::new(IconName::CircleOff).small().text_color(t.muted_foreground)
                })
        };
        let servers = match (self.servers, s.servers_root.is_empty()) {
            (Servers::OtherPc, false) | (Servers::None, false) => format!("{} on {}", s.servers_root, s.server_host),
            (Servers::ThisPc, true) => "Host them on this PC, from any instance".to_string(),
            (Servers::ThisPc, false) => format!(
                "Host on this PC, and {} on {}",
                s.servers_root, s.server_host
            ),
            _ => "None for now".to_string(),
        };
        let mut inst = s.instances_root.clone();
        if let Some(n) = count {
            inst = format!("{n} in {inst}");
        }
        if managed > 0 {
            inst.push_str(&format!(" · {managed} other launcher{}", if managed == 1 { "" } else { "s" }));
        }
        v_flex()
            .gap_4()
            .child(
                v_flex()
                    .items_center()
                    .gap_2()
                    .pt_1()
                    .child(
                        div()
                            .size(rems(3.))
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(theme.success.opacity(0.15))
                            .child(Icon::new(IconName::Check).large().text_color(theme.success)),
                    )
                    .child(div().text_xl().font_semibold().child("You're all set"))
                    .child(note("All of this stays in Settings, and setup runs again from there.", cx)),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(row(
                        IconName::Download,
                        "Hopper",
                        if installed { "Installed, updates itself".into() } else { "Portable".into() },
                        installed,
                        cx,
                    ))
                    .child(row(
                        IconName::Lock,
                        "CurseForge",
                        if key { "API key set".into() } else { "No key: Modrinth only for now".into() },
                        key,
                        cx,
                    ))
                    .child(row(IconName::LibraryBig, "Instances", inst, true, cx))
                    .child(row(
                        IconName::Server,
                        "Servers",
                        servers,
                        self.servers != Servers::None || !s.servers_root.is_empty(),
                        cx,
                    ))
                    .child(row(IconName::Palette, "Theme", theme_name, true, cx)),
            )
            .into_any_element()
    }

    fn heading(&self, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let (icon, title, sub) = match self.step {
            0 => (IconName::Sparkles, "Set up Hopper", "About a minute. All of it can change later in Settings."),
            1 => (IconName::Lock, "Your CurseForge key", "The one step that matters most."),
            2 => (IconName::LibraryBig, "Your instances", "Where Hopper finds what you play."),
            3 => (IconName::Server, "Servers", "Optional. Skip it if you just play."),
            4 => (IconName::Palette, "Make it yours", "A theme for Hopper."),
            _ => (IconName::CircleCheck, "Done", "Here's what's set."),
        };
        h_flex()
            .flex_1()
            .min_w_0()
            .gap_3()
            .child(
                div()
                    .flex_none()
                    .size(rems(2.5))
                    .rounded(theme.radius_lg)
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme.primary.opacity(0.14))
                    .border_1()
                    .border_color(theme.primary.opacity(0.25))
                    .child(Icon::new(icon).text_color(theme.primary)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_xl().font_semibold().child(title))
                    .child(note(sub, cx)),
            )
            .into_any_element()
    }

    /// The steps as dots: the current one long, finished ones in the accent
    /// color, later ones faint. Each jumps to its step.
    fn dots(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        h_flex()
            .flex_none()
            .items_center()
            .children((0..STEPS.len()).map(|i| {
                let current = i == self.step;
                let dot = div().h(rems(0.5)).rounded_full().map(|d| {
                    if current {
                        d.w(rems(1.5)).bg(theme.primary)
                    } else if i < self.step {
                        d.w(rems(0.5)).bg(theme.primary.opacity(0.55))
                    } else {
                        d.w(rems(0.5)).bg(theme.muted_foreground.opacity(0.35))
                    }
                });
                let name = format!("{}. {}", i + 1, STEPS[i]);
                div()
                    .id(("setup-dot", i))
                    .p(rems(0.3125))
                    .rounded_full()
                    .when(!current, |d| {
                        d.cursor_pointer()
                            .hover(|d| d.bg(theme.foreground.opacity(0.1)))
                    })
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(name.clone()).build(window, cx)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.go(i, cx)))
                    .child(dot)
            }))
    }
}

/// The wizard's height: what the window has room for, up to 40rem.
fn height(window: &Window) -> Pixels {
    let room = window.viewport_size().height - px(DIALOG_CHROME + 2. * DIALOG_MARGIN);
    room.min(rems(40.).to_pixels(window.rem_size())).max(px(260.))
}

/// The dialog's own padding, top and bottom.
const DIALOG_CHROME: f32 = 32.;
/// Space kept clear above and below the dialog.
const DIALOG_MARGIN: f32 = 24.;

impl Render for SetupWizard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let body = match self.step {
            0 => self.welcome(cx),
            1 => self.curseforge(cx),
            2 => self.instances(cx),
            3 => self.servers(cx),
            4 => self.look(cx),
            _ => self.done(cx),
        };
        let last = self.step + 1 == STEPS.len();
        let header = h_flex()
            .flex_none()
            .gap_4()
            .pb_3()
            .border_b_1()
            .border_color(theme.border)
            .child(self.heading(cx))
            .child(
                v_flex()
                    .flex_none()
                    .items_end()
                    .gap_0p5()
                    .child(self.dots(cx))
                    .child(
                        div()
                            .pr_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("{} of {} · {}", self.step + 1, STEPS.len(), STEPS[self.step])),
                    ),
            );
        let footer = h_flex()
            .flex_none()
            .pt_3()
            .border_t_1()
            .border_color(theme.border)
            .items_center()
            .gap_2()
            .child(
                h_flex().flex_1().child(if last {
                    div().into_any_element()
                } else {
                    Button::new("setup-skip")
                        .ghost()
                        .small()
                        .label("Skip setup")
                        .on_click(cx.listener(|this, _, window, cx| this.close(false, window, cx)))
                        .into_any_element()
                }),
            )
            .when(self.step > 0, |r| {
                r.child(
                    Button::new("setup-back")
                        .outline()
                        .icon(IconName::ArrowLeft)
                        .label("Back")
                        .on_click(cx.listener(|this, _, _, cx| this.back(cx))),
                )
            })
            .child(if last {
                widgets::glow(
                    Button::new("setup-library")
                        .primary()
                        .icon(IconName::LibraryBig)
                        .label("Open Library")
                        .on_click(cx.listener(|this, _, window, cx| this.close(true, window, cx))),
                    theme.primary,
                    cx,
                )
            } else {
                widgets::glow(
                    Button::new("setup-next")
                        .primary()
                        .label(if self.step == 0 { "Get started" } else { "Next" })
                        .icon(IconName::ArrowRight)
                        .on_click(cx.listener(|this, _, _, cx| this.next(cx))),
                    theme.primary,
                    cx,
                )
            });
        v_flex()
            .id("setup-wizard")
            .h(height(window))
            .gap_3()
            .child(header)
            .child(
                div()
                    .id(("setup-body", self.step))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(widgets::enter(
                        ("setup-step", self.step),
                        v_flex().pr_3().py_1().child(body),
                    )),
            )
            .child(footer)
    }
}

/// A big clickable choice: an icon, a title and a line, outlined in the
/// accent color when picked.
fn choice_card(
    id: &'static str,
    icon: IconName,
    title: &'static str,
    text: &'static str,
    picked: bool,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    v_flex()
        .id(id)
        .flex_1()
        .flex_basis(px(0.))
        .min_w_0()
        .gap_2()
        .p_4()
        .rounded(theme.radius_lg)
        .cursor_pointer()
        .border_1()
        .map(|d| {
            if picked {
                d.border_color(theme.primary)
                    .bg(theme.primary.opacity(0.1))
                    .shadow(vec![BoxShadow::new(px(0.), px(4.), theme.primary.opacity(0.25)).blur_radius(px(16.))])
            } else {
                d.border_color(theme.foreground.opacity(0.1))
                    .bg(theme.foreground.opacity(0.035))
                    .hover(|d| d.bg(theme.foreground.opacity(0.07)).border_color(theme.foreground.opacity(0.18)))
            }
        })
        .child(
            h_flex()
                .justify_between()
                .child(
                    div()
                        .size(rems(2.))
                        .rounded(theme.radius)
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(theme.primary.opacity(0.14))
                        .child(Icon::new(icon).small().text_color(theme.primary)),
                )
                .when(picked, |r| {
                    r.child(Icon::new(IconName::CircleCheck).small().text_color(theme.primary))
                }),
        )
        .child(div().font_semibold().child(title))
        .child(div().text_sm().text_color(theme.muted_foreground).child(text))
}

/// A theme as a tiny window: its sidebar, page, text and accent.
fn theme_card(name: &str, picked: bool, cx: &App) -> Div {
    let theme = cx.theme();
    let mini = |n: &str| {
        let [bg, panel, accent, fg] = crate::theme::swatch(n);
        h_flex()
            .flex_1()
            .h_full()
            .child(
                v_flex()
                    .w(rems(1.5))
                    .h_full()
                    .bg(panel)
                    .p_1()
                    .gap_1()
                    .child(div().h(rems(0.25)).w_full().rounded_full().bg(fg.opacity(0.35)))
                    .child(div().h(rems(0.25)).w_full().rounded_full().bg(accent))
                    .child(div().h(rems(0.25)).w_full().rounded_full().bg(fg.opacity(0.35))),
            )
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .bg(bg)
                    .p_1p5()
                    .gap_1()
                    .child(div().h(rems(0.3)).w(rems(2.5)).rounded_full().bg(fg.opacity(0.85)))
                    .child(div().h(rems(0.25)).w(rems(3.5)).rounded_full().bg(fg.opacity(0.35)))
                    .child(div().mt_auto().h(rems(0.5)).w(rems(1.75)).rounded_full().bg(accent)),
            )
    };
    let preview = if name.is_empty() {
        // Follow Windows: Hopper Dark beside Hopper Light.
        h_flex()
            .size_full()
            .child(mini(crate::theme::HOPPER_DARK))
            .child(mini(crate::theme::HOPPER_LIGHT))
    } else {
        h_flex().size_full().child(mini(name))
    };
    v_flex()
        .w(rems(7.625))
        .gap_1p5()
        .p_1p5()
        .rounded(theme.radius_lg)
        .cursor_pointer()
        .border_1()
        .map(|d| {
            if picked {
                d.border_color(theme.primary).bg(theme.primary.opacity(0.1))
            } else {
                d.border_color(theme.foreground.opacity(0.08))
                    .hover(|d| d.bg(theme.foreground.opacity(0.06)))
            }
        })
        .child(
            div()
                .h(rems(3.75))
                .rounded(theme.radius)
                .overflow_hidden()
                .border_1()
                .border_color(theme.foreground.opacity(0.1))
                .child(preview),
        )
        .child(
            h_flex()
                .gap_1()
                .px_0p5()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .font_medium()
                        .child(if name.is_empty() { "Follow Windows".to_string() } else { name.to_string() }),
                )
                .when(crate::theme::is_glass(name), |r| {
                    r.child(div().text_xs().text_color(theme.muted_foreground).child("glass"))
                })
                .when(picked, |r| {
                    r.child(Icon::new(IconName::Check).xsmall().text_color(theme.primary))
                }),
        )
}

/// A muted line that wraps (`widgets::caption` cuts long text short).
fn note(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .min_w_0()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

fn label(text: &'static str) -> Div {
    div().font_semibold().child(text)
}

/// A test or install outcome: green when it worked, red with why when not.
fn result_line(r: Result<String, String>, cx: &App) -> Div {
    let theme = cx.theme();
    let (icon, color, text) = match r {
        Ok(t) => (IconName::CircleCheck, theme.success, t),
        Err(e) => (IconName::CircleX, theme.danger, e),
    };
    h_flex()
        .gap_2()
        .items_start()
        .text_sm()
        .text_color(color)
        .child(Icon::new(icon).small().flex_none().mt_0p5())
        .child(div().flex_1().min_w_0().child(text))
}

/// Opens the setup. `preview` (dev and MCP runs) never marks it done.
pub fn open(
    store: Entity<Store>,
    app: WeakEntity<AppView>,
    preview: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let view = cx.new(|cx| SetupWizard::new(store, app, preview, window, cx));
    window.open_dialog(cx, move |dialog, window, _| {
        // Centered in the window, at whatever height fits.
        let spare = window.viewport_size().height - height(window) - px(DIALOG_CHROME);
        dialog
            .w(rems(46.).to_pixels(window.rem_size()))
            .margin_top((spare / 2.).max(px(DIALOG_MARGIN)))
            .close_button(false)
            .overlay_closable(false)
            .keyboard(false)
            .child(view.clone())
    });
}

/// At start: the setup for a new user (no settings yet), or with `--setup`
/// (a dev preview that leaves the setup's done mark alone).
pub fn on_start(app: &AppView, window: &mut Window, cx: &mut Context<AppView>) {
    let preview = std::env::args().any(|a| a == "--setup");
    if !preview && !app.store.read(cx).settings().needs_setup() {
        return;
    }
    let store = app.store.clone();
    cx.defer_in(window, move |_, window, cx| {
        let me = cx.entity().downgrade();
        open(store, me, preview, window, cx)
    });
}
