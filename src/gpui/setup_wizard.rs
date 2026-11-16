//! The first-run setup: one dialog, "Here's what we found". It lists the
//! instance folders found on this PC (CurseForge's, and other launchers'),
//! switched on, the Discord switch, and folds the rarely changed choices
//! (where new instances go, the look, servers, the CurseForge key,
//! installing, where Hopper keeps its files) under "More". Shown on the
//! first start (no settings yet), and again from Settings or Ctrl+K.
//!
//! Nothing is written until the player acts: "Use these" saves what differs
//! from the settings, and the theme previews live but is only kept then
//! (Skip puts the old one back). The Discord switch is the exception: it
//! saves when clicked, so turning it off never waits on a later button.

use crate::app::{AppView, Page};
use crate::java_form::settings_form;
use crate::store::Store;
use crate::switch::Switch;
use crate::widgets;
use crate::widgets::Clickable as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::form::Field;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::launchers::Kind;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub(crate) const CF_CONSOLE: &str = "https://console.curseforge.com/";
/// After a restart that moved Hopper's files: open with More unfolded.
const RESUME_ARG: &str = "--setup-step=";
/// More found folders than this and the list scrolls in its card, and the
/// "New instances go in" row moves under More, so the dialog keeps its size.
const LIST_ROWS: usize = 3;

/// Where the user's servers are.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Servers {
    None,
    ThisPc,
    OtherPc,
}

/// A folder of instances found on this PC.
#[derive(Clone)]
struct Found {
    /// `None`: the instances folder Hopper reads as its main library
    /// (CurseForge's, when the CurseForge app is installed).
    kind: Option<Kind>,
    path: PathBuf,
    count: usize,
    /// Switched on (launchers only; the main folder's switch follows the
    /// "New instances go in" field).
    on: bool,
}

pub struct SetupWizard {
    store: Entity<Store>,
    app: WeakEntity<AppView>,
    /// Opened with `--setup` or over MCP: finishing or skipping doesn't
    /// mark the setup done.
    preview: bool,
    /// The first run (not yet done): Skip then leaves the Library empty.
    fresh: bool,
    /// "More" unfolded.
    more: bool,
    /// What was found: `None` while looking.
    found: Option<Vec<Found>>,
    /// Why a picked launcher folder wasn't added.
    add_note: Option<String>,
    instances_root: Entity<InputState>,
    installing: bool,
    install_result: Option<Result<String, String>>,
    cf_key: Entity<InputState>,
    cf_testing: bool,
    cf_result: Option<Result<String, String>>,
    /// "Use my own key" unfolded (builds with Hopper's own key).
    cf_own_open: bool,
    servers: Servers,
    servers_root: Entity<InputState>,
    server_host: Entity<InputState>,
    /// The theme as saved when the setup opened (name, old mode).
    theme_saved: (String, u8),
    /// A theme shown but not saved yet.
    theme_preview: Option<String>,
    /// A folder picked for Hopper's files, waiting for "Move and restart".
    files_dest: Option<PathBuf>,
    /// Copying them there (Hopper restarts after).
    files_moving: bool,
    files_error: Option<String>,
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
        for i in [&cf_key, &servers_root, &server_host, &instances_root] {
            subs.push(cx.subscribe(i, |_, _, e: &InputEvent, cx| {
                if matches!(e, InputEvent::Change) {
                    cx.notify();
                }
            }));
        }
        let resumed = std::env::args().any(|a| a.starts_with(RESUME_ARG));
        let mut this = Self {
            store,
            app,
            preview,
            fresh: !s.setup_done,
            more: resumed,
            found: None,
            add_note: None,
            instances_root,
            installing: false,
            install_result: None,
            cf_key,
            cf_testing: false,
            cf_result: None,
            cf_own_open: false,
            servers: if s.servers_root.is_empty() { Servers::None } else { Servers::OtherPc },
            servers_root,
            server_host,
            theme_saved: (s.theme_name.clone(), s.theme),
            theme_preview: None,
            files_dest: None,
            files_moving: false,
            files_error: None,
            _subscriptions: subs,
        };
        this.detect(resumed, cx);
        this
    }

    fn typed(input: &Entity<InputState>, cx: &App) -> String {
        input.read(cx).value().trim().to_string()
    }

    /// Looks for instances on this PC (in the background): the instances
    /// folder from the settings, then every other launcher's folder. A new
    /// player gets each launcher with instances switched on; running the
    /// setup again (or after a restart that moved Hopper's files) shows
    /// what is managed now.
    fn detect(&mut self, resumed: bool, cx: &mut Context<Self>) {
        let s = self.store.read(cx).settings().clone();
        let defaults = !s.setup_done && !resumed;
        cx.spawn(async move |this, cx| {
            let main = s.instances_root.clone();
            let managed = s.launcher_dirs.clone();
            let rows = cx
                .background_spawn(async move {
                    let mut rows = Vec::new();
                    let n = hopper::instances::discover(Path::new(&main)).len();
                    if n > 0 {
                        rows.push(Found { kind: None, path: PathBuf::from(&main), count: n, on: true });
                    }
                    let is_managed = |k: Kind, p: &Path| managed.iter().any(|d| d.kind == k.key() && Path::new(&d.path) == p);
                    for (k, p, n) in hopper::launchers::found() {
                        let on = if defaults { n > 0 } else { is_managed(k, &p) };
                        rows.push(Found { kind: Some(k), path: p, count: n, on });
                    }
                    // Folders added by hand (or no longer where the launcher
                    // keeps them) stay listed.
                    for d in &managed {
                        let p = PathBuf::from(&d.path);
                        if let Some(k) = Kind::from_key(&d.kind)
                            && !rows.iter().any(|r| r.path == p)
                        {
                            let n = hopper::launchers::list(k, &p).len();
                            rows.push(Found { kind: Some(k), path: p, count: n, on: true });
                        }
                    }
                    rows
                })
                .await;
            this.update(cx, |this, cx| {
                this.found = Some(rows);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Hopper's own instances folder, for a player who doesn't keep their
    /// instances in CurseForge's.
    fn own_instances() -> String {
        let root = hopper::paths::forced_root()
            .or_else(|| hopper::paths::dirs().and_then(|d| d.root()).map(Path::to_path_buf))
            .or_else(|| hopper::paths::dirs().and_then(|d| d.data_dir().parent()).map(Path::to_path_buf));
        root.map(|r| r.join("instances").to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// The main folder's row: on while new instances go there too (it is
    /// the one folder Hopper reads in place as its own library).
    fn main_on(&self, row: &Found, cx: &App) -> bool {
        Path::new(&Self::typed(&self.instances_root, cx)) == row.path
    }

    fn set_row(&mut self, i: usize, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.found.as_mut().and_then(|f| f.get_mut(i)) else { return };
        if row.kind.is_some() {
            row.on = on;
        } else {
            // Off: new instances (and the Library) use Hopper's own folder.
            let to = if on { row.path.to_string_lossy().into_owned() } else { Self::own_instances() };
            self.instances_root.update(cx, |s, cx| s.set_value(to, window, cx));
        }
        cx.notify();
    }

    /// The rows that have instances (an empty launcher folder alone isn't
    /// "found").
    fn anything_found(&self) -> bool {
        self.found.as_ref().is_some_and(|f| f.iter().any(|r| r.count > 0))
    }

    // ---- saving ----------------------------------------------------------------

    /// Saves every choice that differs from the settings.
    fn save(&mut self, cx: &mut Context<Self>) {
        let s = self.store.read(cx).settings().clone();
        let dir = Self::typed(&self.instances_root, cx);
        let dir = if dir.is_empty() { s.instances_root.clone() } else { dir };
        let mut launchers = s.launcher_dirs.clone();
        for r in self.found.iter().flatten() {
            let Some(k) = r.kind else { continue };
            let d = hopper::settings::LauncherDir { kind: k.key().to_string(), path: r.path.to_string_lossy().into_owned() };
            launchers.retain(|x| x != &d);
            if r.on {
                launchers.push(d);
            }
        }
        let (mut root, mut host) = (s.servers_root.clone(), s.server_host.clone());
        if self.servers == Servers::OtherPc {
            root = Self::typed(&self.servers_root, cx);
            let h = Self::typed(&self.server_host, cx);
            if !h.is_empty() {
                host = h;
            }
        }
        // The servers choice sets server tools: on when any servers are
        // picked; off for "None" only on a first run that hasn't chosen yet
        // (running the setup again never hides them).
        let tools = match self.servers {
            Servers::None if !self.preview && self.fresh && s.server_tools.is_none() => Some(false),
            Servers::ThisPc | Servers::OtherPc if !self.preview => Some(true),
            _ => s.server_tools,
        };
        let theme = self.theme_preview.take().filter(|n| *n != s.theme_name);
        let changed = dir != s.instances_root
            || launchers != s.launcher_dirs
            || root != s.servers_root
            || host != s.server_host
            || tools != s.server_tools
            || theme.is_some();
        if !changed {
            return;
        }
        if let Some(name) = &theme {
            self.theme_saved = (name.clone(), 0);
        }
        self.store.update(cx, |st, cx| {
            st.update_settings(
                |s| {
                    s.instances_root = dir;
                    s.launcher_dirs = launchers;
                    s.servers_root = root;
                    s.server_host = host;
                    s.server_tools = tools;
                    if let Some(name) = theme {
                        s.theme_name = name;
                        s.theme = 0;
                    }
                },
                cx,
            );
            st.refresh();
            cx.notify();
        });
    }

    /// Puts back a theme that was only being looked at.
    fn revert_theme(&mut self, cx: &mut Context<Self>) {
        if self.theme_preview.take().is_some() {
            let (name, mode) = self.theme_saved.clone();
            let zoom = self.store.read(cx).settings().zoom;
            crate::theme::apply(&name, mode, zoom, cx);
        }
    }

    fn mark_done(&mut self, cx: &mut Context<Self>) {
        if !self.preview && !self.store.read(cx).settings().setup_done {
            self.store
                .update(cx, |st, cx| st.update_settings(|s| s.setup_done = true, cx));
        }
    }

    /// "Use these" (and the nothing-found buttons): saves, then opens `page`.
    fn finish(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.save(cx);
        self.mark_done(cx);
        window.close_dialog(cx);
        self.app.update(cx, |a, cx| a.go(page, cx)).ok();
    }

    /// Skip: nothing found is added. On the first run that includes
    /// CurseForge's folder, so the Library starts empty with its "Find a
    /// modpack" button; all of it can be added later in Settings. Running
    /// the setup again, Skip changes nothing.
    fn skip(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.revert_theme(cx);
        if self.fresh && !self.preview {
            let s = self.store.read(cx).settings().clone();
            let main_found = self.found.iter().flatten().any(|r| r.kind.is_none() && Path::new(&s.instances_root) == r.path);
            let own = Self::own_instances();
            if main_found && !own.is_empty() {
                self.store.update(cx, |st, cx| {
                    st.update_settings(|s| s.instances_root = own, cx);
                    st.refresh();
                    cx.notify();
                });
            }
        }
        self.mark_done(cx);
        window.close_dialog(cx);
        self.app.update(cx, |a, cx| a.go(Page::Library, cx)).ok();
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

    /// Asks for a launcher's folder and lists it (switched on) when it has
    /// instances.
    fn add_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Use this launcher's instances".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(p))) = rx.await else { return };
            let Some(p) = p.into_iter().next() else { return };
            let probe = p.clone();
            let found = cx
                .background_spawn(async move {
                    Kind::ALL.into_iter().find_map(|k| {
                        let n = hopper::launchers::list(k, &probe).len();
                        (n > 0).then_some((k, n))
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                match found {
                    Some((k, n)) => {
                        this.add_note = None;
                        let rows = this.found.get_or_insert_with(Vec::new);
                        match rows.iter_mut().find(|r| r.path == p) {
                            Some(r) => r.on = true,
                            None => rows.push(Found { kind: Some(k), path: p, count: n, on: true }),
                        }
                    }
                    None => {
                        this.add_note = Some(format!(
                            "No Modrinth App, Prism, MultiMC, ATLauncher, GDLauncher or FTB App instances in {}.",
                            p.display()
                        ));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn pick_files_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let me = cx.entity().downgrade();
        crate::data_move::pick_folder(
            "Keep Hopper's files here",
            move |dest, _, cx| {
                me.update(cx, |this, cx| {
                    this.files_error = hopper::paths::check_dest(&dest).err().map(|e| format!("{e:#}"));
                    this.files_dest = Some(dest);
                    cx.notify();
                })
                .ok();
            },
            window,
            cx,
        )
    }

    /// Saves the choices so far, then moves Hopper's files (Hopper restarts
    /// and the setup opens again with More unfolded).
    fn move_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dest) = self.files_dest.clone() else { return };
        if let Err(e) = hopper::paths::check_dest(&dest) {
            self.files_error = Some(format!("{e:#}"));
            cx.notify();
            return;
        }
        self.save(cx);
        self.files_moving = true;
        self.files_error = None;
        cx.notify();
        let me = cx.entity().downgrade();
        crate::data_move::start(
            self.store.clone(),
            dest,
            vec![format!("{RESUME_ARG}more")],
            move |e, cx| {
                me.update(cx, |this, cx| {
                    this.files_moving = false;
                    this.files_error = Some(e);
                    cx.notify();
                })
                .ok();
            },
            window,
            cx,
        );
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

    // ---- the parts -------------------------------------------------------------

    fn heading(&self, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let (icon, title, sub) = if self.found.is_some() && !self.anything_found() {
            (IconName::Plus, "Welcome to Hopper", "No other launchers on this PC, so you start fresh.")
        } else {
            (IconName::Check, "Here's what we found", "Hopper is set up from what's on this PC. Use it, or change anything first.")
        };
        h_flex()
            .flex_none()
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

    /// One found folder: its launcher's tile, name, count and path, and
    /// its switch.
    fn found_row(&self, i: usize, row: &Found, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let on = if row.kind.is_none() { self.main_on(row, cx) } else { row.on };
        let path = row.path.display().to_string();
        let line = if row.count == 0 {
            "Folder found, no instances in it".to_string()
        } else {
            format!("{} · {path}", hopper::launchers::count_label(row.kind, row.count))
        };
        let tip = path.clone();
        h_flex()
            .id(("setup-found", i))
            .gap_3()
            .px_3()
            .py_2()
            .rounded(theme.radius)
            .hover(|s| widgets::row_hover(s, cx))
            .child(launcher_tile(row.kind, &row.path, on, rems(2.), cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_medium().truncate().child(launcher_name(row.kind, &row.path)))
                    .child(
                        div()
                            .id(("setup-found-path", i))
                            .min_w_0()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(line)
                            .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx)),
                    ),
            )
            .child(
                Switch::new(("setup-found-switch", i))
                    .checked(on)
                    .on_click(cx.listener(move |this, v: &bool, window, cx| this.set_row(i, *v, window, cx))),
            )
            .into_any_element()
    }

    /// The found list: every row, scrolling inside its card when there are
    /// more than a few; or, with More open, one summary line.
    fn found_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let rows = self.found.clone().unwrap_or_default();
        let card = || {
            v_flex()
                .rounded(theme.radius_lg)
                .bg(theme.foreground.opacity(0.035))
                .border_1()
                .border_color(theme.foreground.opacity(0.08))
        };
        if self.found.is_none() {
            return card()
                .p_3()
                .child(note("Looking for instances on this PC…", cx))
                .into_any_element();
        }
        if self.more {
            let on: Vec<&Found> = rows.iter().filter(|r| if r.kind.is_none() { self.main_on(r, cx) } else { r.on }).collect();
            let names: Vec<&str> = on.iter().map(|r| launcher_name(r.kind, &r.path)).collect();
            let total: usize = on.iter().map(|r| r.count).sum();
            let text = if on.is_empty() {
                "Nothing switched on".to_string()
            } else {
                format!("{} · {}", names.join(", "), hopper::launchers::count_label(None, total))
            };
            return card()
                .child(
                    h_flex()
                        .id("setup-found-summary")
                        .clickable(cx)
                        .gap_2()
                        .px_3()
                        .py_2()
                        .rounded(theme.radius_lg)
                        .hover(|s| widgets::row_hover(s, cx))
                        .text_sm()
                        .children(on.iter().map(|r| launcher_tile(r.kind, &r.path, true, rems(1.5), cx)))
                        .child(div().flex_1().min_w_0().truncate().child(text))
                        .child(
                            div()
                                .flex_none()
                                .text_color(theme.muted_foreground)
                                .child(format!("{} of {} on", on.len(), rows.len())),
                        )
                        .child(Icon::new(IconName::ChevronDown).small().text_color(theme.muted_foreground))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.more = false;
                            cx.notify();
                        })),
                )
                .into_any_element();
        }
        let many = rows.len() > LIST_ROWS + 2;
        let list = v_flex()
            .p_0p5()
            .children(rows.iter().enumerate().map(|(i, r)| self.found_row(i, r, cx)));
        if many {
            // Five rows tall; the rest scroll, so nothing below moves.
            card()
                .child(div().id("setup-found-scroll").h(rems(17.)).overflow_y_scrollbar().child(list))
                .into_any_element()
        } else {
            card().child(list).into_any_element()
        }
    }

    fn discord(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let on = self.store.read(cx).settings().discord.enabled;
        h_flex()
            .gap_3()
            .px_3()
            .py_2p5()
            .rounded(theme.radius_lg)
            .bg(theme.foreground.opacity(0.035))
            .border_1()
            .border_color(theme.foreground.opacity(0.08))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(div().font_medium().child("Show what I'm playing on Discord"))
                    .child(note(
                        "Friends see the modpack you're playing and what you're browsing in Hopper, and nothing else. Turn it off here or any time in Settings.",
                        cx,
                    )),
            )
            // On unless it was turned off before; only a click here changes
            // it (the full set of Discord choices stays in Settings › Discord).
            .child(
                Switch::new("setup-discord")
                    .checked(on)
                    .on_click(cx.listener(|this, v: &bool, _, cx| {
                        let v = *v;
                        this.store.update(cx, |st, cx| st.update_settings(|s| s.discord.enabled = v, cx));
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    /// The instances folder field with its Change… button.
    fn folder_field(&self, cx: &mut Context<Self>) -> AnyElement {
        let input = self.instances_root.clone();
        h_flex()
            .w_full()
            .gap_2()
            .child(div().flex_1().min_w_0().child(Input::new(&self.instances_root)))
            .child(
                Button::new("setup-browse-instances")
                    .outline()
                    .label("Change…")
                    .on_click(cx.listener(move |_, _, window, cx| {
                        Self::browse(input.clone(), "New instances go here", window, cx)
                    })),
            )
            .into_any_element()
    }

    fn more_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let open = self.more;
        let what = if self.list_long() {
            "Instances folder, look, servers, CurseForge key, install"
        } else {
            "Look, servers, CurseForge key, install"
        };
        h_flex()
            .id("setup-more")
            .clickable(cx)
            .gap_2()
            .px_3()
            .py_2()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .when(open, |d| d.bg(theme.foreground.opacity(0.035)))
            .hover(|s| widgets::row_hover(s, cx))
            .text_sm()
            .child(
                Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                    .small()
                    .text_color(theme.muted_foreground),
            )
            .child(div().flex_none().font_medium().child("More"))
            .child(div().flex_1().min_w_0().truncate().text_color(theme.muted_foreground).child(what))
            .on_click(cx.listener(|this, _, _, cx| {
                this.more = !this.more;
                cx.notify();
            }))
            .into_any_element()
    }

    /// Many folders found: the instances folder row waits under More.
    fn list_long(&self) -> bool {
        self.found.as_ref().is_some_and(|f| f.len() > LIST_ROWS) && self.anything_found()
    }

    /// Everything under More, as a settings form.
    fn more_form(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let rem = window.rem_size();
        let folder_here = self.anything_found();
        // The look: the two Hopper themes or Windows' choice, the rest in a
        // menu.
        let current = self.current_theme();
        let basics = [String::new(), crate::theme::HOPPER_DARK.to_string(), crate::theme::HOPPER_LIGHT.to_string()];
        let picked = basics.iter().position(|n| *n == current);
        let others: Vec<String> = crate::theme::names().into_iter().filter(|n| !basics.contains(n)).collect();
        let view = cx.entity().downgrade();
        let others2 = others.clone();
        let pick_other: Rc<dyn Fn(usize, &mut Window, &mut App)> = Rc::new(move |ix, _, cx| {
            if let Some(n) = others2.get(ix).cloned() {
                view.update(cx, |this, cx| this.preview_theme(n, cx)).ok();
            }
        });
        let look = h_flex()
            .gap_2()
            .child(
                widgets::Segmented::new("setup-look")
                    .when_some(picked, |t, i| t.selected_index(i))
                    .children(["Follow Windows", "Hopper Dark", "Hopper Light"].map(|l| widgets::SegItem::new().label(l)))
                    .on_click(cx.listener(move |this, ix: &usize, _, cx| {
                        let n = basics[*ix].clone();
                        this.preview_theme(n, cx)
                    })),
            )
            .child(div().w(rems(12.)).child(widgets::field_picker(
                "setup-look-more",
                if picked.is_none() && !current.is_empty() { current.clone() } else { "More themes".into() },
                others.iter().map(|n| (n.clone(), *n == current)).collect(),
                rem,
                pick_other,
            )));
        // Servers.
        let srv_ix = match self.servers {
            Servers::None => 0,
            Servers::ThisPc => 1,
            Servers::OtherPc => 2,
        };
        let servers = widgets::Segmented::new("setup-servers")
            .selected_index(srv_ix)
            .children(["None", "On this PC", "On another PC"].map(|l| widgets::SegItem::new().label(l)))
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                this.servers = [Servers::None, Servers::ThisPc, Servers::OtherPc][*ix];
                cx.notify();
            }));
        let hosted = self.store.read(cx).hosted_root();
        let srv_note = match self.servers {
            Servers::None => "Just play. Add servers any time from the Servers page.".to_string(),
            Servers::ThisPc => format!("Make one from any instance: ⋯ → Host a server from this pack. They go in {hosted}."),
            Servers::OtherPc => "The shared folder that holds their server folders, and that PC's name for status pings.".to_string(),
        };
        // The CurseForge key.
        use hopper::cf::KeySource;
        let source = hopper::cf::api_key_source().map(|(_, s)| s);
        let built_in = hopper::cf::has_built_in_key();
        let typed = !Self::typed(&self.cf_key, cx).is_empty();
        let key_field = v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(Input::new(&self.cf_key).mask_toggle()))
                    .child(
                        Button::new("setup-cf-test")
                            .outline()
                            .label(if typed { "Test and save" } else { "Test" })
                            .loading(self.cf_testing)
                            .disabled(self.cf_testing || (!typed && source.is_none()))
                            .on_click(cx.listener(|this, _, window, cx| this.test_key(window, cx))),
                    ),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_x_1()
                    .items_center()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("Free from")
                    .child(
                        Button::new("setup-cf-console")
                            .link()
                            .small()
                            .label("console.curseforge.com")
                            .on_click(|_, _, cx| cx.open_url(CF_CONSOLE)),
                    )
                    .child("(API keys, then copy). It stays on this PC."),
            )
            .when(self.cf_testing, |c| c.child(note("Asking CurseForge…", cx)))
            .when_some(self.cf_result.clone(), |c, r| c.child(result_line(r, cx)));
        let key_text = match source {
            Some(KeySource::Saved) => "Your own key is saved.",
            Some(KeySource::Env) => "Using the key from .env.",
            Some(KeySource::Session) => "Using a key for this run.",
            _ if built_in => "Hopper's own key works for downloads.",
            _ => "Optional. Without a key, Hopper uses Modrinth only.",
        };
        let own_open = self.cf_own_open || !built_in;
        let key = v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(note(key_text, cx).flex_1())
                    .when(built_in, |r| {
                        r.child(
                            Button::new("setup-cf-own")
                                .outline()
                                .small()
                                .label(if own_open { "Hide" } else { "Use my own key…" })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cf_own_open = !this.cf_own_open;
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .when(own_open, |c| c.child(key_field));
        // Installing.
        let installed_here = hopper::setup::running_installed();
        let install: AnyElement = if installed_here {
            h_flex()
                .gap_2()
                .items_center()
                .child(widgets::pill("Installed", theme.success, true))
                .child(note("Hopper updates itself.", cx))
                .into_any_element()
        } else {
            let other = hopper::setup::installed_exe().is_some();
            v_flex()
                .gap_1p5()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            Button::new("setup-install")
                                .outline()
                                .small()
                                .icon(IconName::Download)
                                .label(if other { "Install this version" } else { "Install Hopper" })
                                .loading(self.installing)
                                .disabled(self.installing)
                                .on_click(cx.listener(|this, _, window, cx| this.install(window, cx))),
                        )
                        .child(note(
                            if other { "Replaces the installed copy." } else { "Start menu shortcut, and it updates itself." },
                            cx,
                        )),
                )
                .child(note("Or keep it portable: it runs from where it is now.", cx))
                .when_some(self.install_result.clone(), |c, r| c.child(result_line(r, cx)))
                .into_any_element()
        };
        // Where Hopper keeps its files.
        let files_now = hopper::paths::dirs()
            .map(|d| d.root().unwrap_or(d.data_dir()).display().to_string())
            .unwrap_or_default();
        let files = v_flex()
            .gap_1p5()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().min_w_0().text_sm().truncate().child(files_now))
                    .child(
                        Button::new("setup-files-pick")
                            .outline()
                            .small()
                            .label("Move…")
                            .disabled(self.files_moving)
                            .on_click(cx.listener(|this, _, window, cx| this.pick_files_folder(window, cx))),
                    ),
            )
            .when_some(self.files_dest.clone(), |c, d| {
                c.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(note(format!("Hopper restarts to move them to {}.", d.display()), cx).flex_1())
                        .child(
                            Button::new("setup-files-cancel")
                                .ghost()
                                .small()
                                .label("Cancel")
                                .disabled(self.files_moving)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.files_dest = None;
                                    this.files_error = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("setup-files-move")
                                .outline()
                                .small()
                                .label("Move and restart")
                                .loading(self.files_moving)
                                .disabled(self.files_moving || self.files_error.is_some())
                                .on_click(cx.listener(|this, _, window, cx| this.move_files(window, cx))),
                        ),
                )
            })
            .when_some(self.files_error.clone(), |c, e| c.child(result_line(Err(e), cx)));
        let servers_root = self.servers_root.clone();
        settings_form(rem)
            .when(folder_here, |f| f.child(Field::new().label("New instances go in").child(self.folder_field(cx))))
            .child(Field::new().label("Look").child(look))
            .child(
                Field::new()
                    .label("Servers")
                    .child(v_flex().gap_1p5().child(servers).child(note(srv_note, cx))),
            )
            .when(self.servers == Servers::OtherPc, |f| {
                f.child(
                    Field::new().label("Servers folder").child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .child(div().flex_1().min_w_0().child(Input::new(&self.servers_root)))
                            .child(
                                Button::new("setup-browse-servers")
                                    .outline()
                                    .label("Browse…")
                                    .on_click(cx.listener(move |_, _, window, cx| {
                                        Self::browse(servers_root.clone(), "Use this folder", window, cx)
                                    })),
                            ),
                    ),
                )
                .child(Field::new().label("Server host").child(div().w(rems(16.)).child(Input::new(&self.server_host))))
            })
            .child(Field::new().label("CurseForge key").child(key))
            .child(Field::new().label("Install").child(install))
            .child(Field::new().label("Hopper's files").child(files))
            .into_any_element()
    }
}

/// A launcher's name, for showing. The main folder is CurseForge's when
/// its path says so.
fn launcher_name(kind: Option<Kind>, path: &Path) -> &'static str {
    match kind {
        Some(k) => k.name(),
        None if path.to_string_lossy().to_lowercase().contains("curseforge") => "CurseForge",
        None => "Instances folder",
    }
}

/// A launcher's tile: its initials on its colour (grey while off).
fn launcher_tile(kind: Option<Kind>, path: &Path, on: bool, size: Rems, cx: &App) -> Div {
    let theme = cx.theme();
    // Dark enough shades that the white initials pass 4.5:1.
    let (text, color): (&str, u32) = match kind {
        None if launcher_name(kind, path) == "CurseForge" => ("CF", 0xc2410c),
        None => ("", 0x52525b),
        Some(Kind::Modrinth) => ("M", 0x15803d),
        Some(Kind::Prism) => ("P", 0x4d7c0f),
        Some(Kind::MultiMC) => ("MM", 0x0e7490),
        Some(Kind::ATLauncher) => ("AT", 0x7c3aed),
        Some(Kind::GDLauncher) => ("GD", 0xbe185d),
        Some(Kind::FtbApp) => ("FTB", 0x1d4ed8),
    };
    let bg: Hsla = if on { rgb(color).into() } else { theme.muted_foreground.opacity(0.35) };
    let small = size.0 < 1.75;
    div()
        .flex_none()
        .size(size)
        .rounded(theme.radius)
        .flex()
        .items_center()
        .justify_center()
        .bg(bg)
        .text_color(white())
        .font_bold()
        .map(|d| if small { d.text_size(rems(0.5625)) } else { d.text_xs() })
        .map(|d| {
            if text.is_empty() {
                d.child(Icon::new(IconName::FolderOpen).small().text_color(white()))
            } else {
                d.child(text)
            }
        })
}

/// The section labels over the cards.
fn section(text: &'static str, cx: &App) -> Div {
    div()
        .text_xs()
        .font_semibold()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

impl Render for SetupWizard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let found = self.found.is_none() || self.anything_found();
        let long = self.list_long();
        let mut body = v_flex().gap_1p5().pr_3().py_1();
        if found {
            body = body
                .child(
                    h_flex()
                        .items_center()
                        .child(section("Your instances · shown in the Library, left where they are", cx).flex_1())
                        .child(
                            Button::new("setup-add-folder")
                                .ghost()
                                .xsmall()
                                .label("Add a folder…")
                                .tooltip("A launcher Hopper didn't find: a portable MultiMC or a moved instances folder")
                                .on_click(cx.listener(|this, _, window, cx| this.add_folder(window, cx))),
                        ),
                )
                .child(self.found_list(cx));
        } else {
            body = body
                .child(section("Your instances go in", cx))
                .child(self.folder_field(cx))
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("Use another launcher?")
                        .child(
                            Button::new("setup-add-folder")
                                .link()
                                .small()
                                .label("Point Hopper at its folder…")
                                .on_click(cx.listener(|this, _, window, cx| this.add_folder(window, cx))),
                        ),
                );
        }
        body = body
            .when_some(self.add_note.clone(), |c, n| c.child(div().text_sm().text_color(theme.warning).child(n)))
            .child(div().h_2())
            .child(section("Discord", cx))
            .child(self.discord(cx));
        if found && !long && !self.more {
            body = body
                .child(div().h_2())
                .child(section("New instances go in", cx))
                .child(self.folder_field(cx));
        }
        body = body.child(div().h_3()).child(self.more_button(cx));
        if self.more {
            body = body.child(div().pt_2().child(self.more_form(window, cx)));
        }
        let primary = |id: &'static str, label: &'static str, page: Page, cx: &mut Context<Self>| {
            widgets::glow(
                Button::new(id)
                    .primary()
                    .label(label)
                    .disabled(self.files_moving)
                    .on_click(cx.listener(move |this, _, window, cx| this.finish(page, window, cx))),
                theme.primary,
                cx,
            )
        };
        let footer = h_flex()
            .flex_none()
            .pt_3()
            .border_t_1()
            .border_color(theme.border)
            .items_center()
            .gap_2()
            .child(
                Button::new("setup-skip")
                    .ghost()
                    .label("Skip")
                    .disabled(self.files_moving)
                    .tooltip("Adds nothing it found. You can add launchers later in Settings.")
                    .on_click(cx.listener(|this, _, window, cx| this.skip(window, cx))),
            )
            .child(div().flex_1())
            .map(|f| {
                if found {
                    f.child(note("You can change all of this in Settings.", cx))
                        .child(primary("setup-use", "Use these", Page::Library, cx))
                } else {
                    f.child(
                        Button::new("setup-library")
                            .outline()
                            .label("Open the Library")
                            .disabled(self.files_moving)
                            .on_click(cx.listener(|this, _, window, cx| this.finish(Page::Library, window, cx))),
                    )
                    .child(primary("setup-find", "Find a modpack", Page::Browse, cx))
                }
            });
        // The body scrolls when the window is short; the title and the
        // buttons stay.
        let room = (widgets::dialog_room(window) - window.rem_size() * 7.).max(window.rem_size() * 10.);
        v_flex()
            .id("setup-wizard")
            .gap_3()
            .child(self.heading(cx))
            .child(
                div()
                    .id("setup-body")
                    .max_h(room)
                    .overflow_y_scrollbar()
                    .child(body),
            )
            .child(footer)
    }
}

/// A muted line that wraps (`widgets::caption` cuts long text short).
fn note(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .min_w_0()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
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
        dialog
            .w(rems(46.).to_pixels(window.rem_size()))
            .margin_top(rems(widgets::DIALOG_TOP).to_pixels(window.rem_size()))
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
