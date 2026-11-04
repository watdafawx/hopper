//! What a server's crash looks like outside Logs & crashes: one line above
//! every server tab but that one ("The server crashed · X stopped it from
//! starting · Put 1.2 back · See why →"). The full card lives in Logs &
//! crashes (`server_logs.rs`); this only points there. Also the two fixes
//! both places run on a server: put the older jar back, turn mods off.

use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex};
use gpui_kit::*;
use hopper::crashdetect::{Changed, Crash};
use std::path::{Path, PathBuf};

/// Puts the older copy of `ch` back on the server (through Hopper Server
/// when it manages the folder); the newer one goes to backups. Says what
/// happened in a sentence.
pub fn put_back(root: &Path, ch: &Changed) -> anyhow::Result<String> {
    match hopper::hopperd::locate(root) {
        Some(r) => {
            let (c, id, _) = r?;
            c.crash_put_back(&id, &ch.file_name)?;
        }
        None => {
            hopper::crashdetect::put_back(root, ch)?;
        }
    }
    let old = if ch.from.is_empty() { "The older version".to_string() } else { ch.from.clone() };
    let new = if ch.to.is_empty() { "the newer one".to_string() } else { ch.to.clone() };
    Ok(format!("{old} is back; {new} is in backups. Start the server to try again."))
}

/// Turns the jars `files` off on the server.
pub fn turn_off(root: &Path, files: Vec<String>) -> anyhow::Result<String> {
    let n = files.len();
    match hopper::hopperd::locate(root) {
        Some(r) => {
            let (c, id, _) = r?;
            c.post(
                &format!("/servers/{}/mods/toggle-many", hopper::hopperd::enc(&id)),
                serde_json::json!({ "files": files, "enabled": false }),
            )?;
        }
        None => hopper::crashdetect::turn_off_in(root, &files)?,
    }
    Ok(if n == 1 { "Turned off 1 mod.".into() } else { format!("Turned off {n} mods.") })
}

/// The report's path under the server folder ("crash-reports/crash-….txt"),
/// whether the crash came from a folder or from Hopper Server.
pub fn report_rel(c: &Crash) -> Option<String> {
    let s = c.source.to_string_lossy().replace('\\', "/");
    s.find("crash-reports/").map(|i| s[i..].to_string())
}

/// The mod that stopped the server and the older version to put back, if
/// the crash names them.
pub fn stopped_change(c: &Crash) -> Option<(String, Option<Changed>)> {
    let st = c.stopped.as_ref()?;
    let s = c.suspects.iter().find(|s| s.file_name == st.file_name)?;
    Some((s.file_name.clone(), s.changed.clone()))
}

pub enum StripEvent {
    /// "See why": open Logs & crashes on this report (its path under the
    /// server folder).
    SeeWhy(Option<String>),
    /// The server's files changed.
    Fixed,
}

impl EventEmitter<StripEvent> for CrashStrip {}

pub struct CrashStrip {
    store: Entity<Store>,
    root: Option<PathBuf>,
    /// Read for `root`; `None` until the first read lands.
    crash: Option<Option<Crash>>,
    busy: bool,
    _task: Option<Task<()>>,
}

impl CrashStrip {
    pub fn new(store: Entity<Store>) -> Self {
        Self { store, root: None, crash: None, busy: false, _task: None }
    }

    /// Reads the last start's crash again for `root`, off the UI thread.
    pub fn refresh(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.root.as_ref() != Some(&root) {
            self.crash = None;
        }
        self.root = Some(root.clone());
        self._task = Some(cx.spawn(async move |this, cx| {
            let r = root.clone();
            let crash = cx.background_spawn(async move { hopper::crashscan::last_crash(&r) }).await;
            this.update(cx, |this, cx| {
                if this.root.as_ref() == Some(&root) {
                    this.crash = Some(crash);
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// The server on screen: reads its crash the first time and when it
    /// changed. Called on every draw, so it does nothing otherwise.
    pub fn show(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.root.as_ref() != Some(&root) {
            self.refresh(root, cx);
        }
    }

    /// The settings key of the "seen" mark for the crash on screen (the
    /// Logs & crashes tab's dot shows until it's set).
    pub fn unseen_key(&self) -> Option<String> {
        let (root, c) = (self.root.as_ref()?, self.crash()?);
        Some(hopper::settings::notice_key(&root.to_string_lossy(), "logs-seen", &format!("crash|{}|{}", c.source.to_string_lossy(), c.when)))
    }

    /// The crash this server last had, when it has one.
    pub fn crash(&self) -> Option<&Crash> {
        self.crash.as_ref().and_then(|c| c.as_ref())
    }

    fn fix_back(&mut self, ch: Changed, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        self.busy = true;
        cx.notify();
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let r = root.clone();
            let done = cx.background_spawn(async move { put_back(&r, &ch) }).await;
            this.update(cx, |this, cx| {
                this.busy = false;
                let msg = match done {
                    Ok(m) => gpui_kit::component::notification::Notification::success(m),
                    Err(e) => gpui_kit::component::notification::Notification::warning(format!("{e:#}")),
                };
                let _ = window_handle.update(cx, |_, window, cx| {
                    use gpui_kit::component::WindowExt as _;
                    window.push_notification(msg, cx);
                });
                cx.emit(StripEvent::Fixed);
                this.refresh(root, cx);
            })
            .ok();
        })
        .detach();
    }
}

impl Render for CrashStrip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (Some(root), Some(c)) = (self.root.clone(), self.crash().cloned()) else {
            return div().into_any_element();
        };
        let theme = cx.theme().clone();
        // Names from the pack the server runs, else the jar's own.
        let (addons, pack) = {
            let store = self.store.read(cx);
            let paired = store.server(&root).and_then(|sv| store.paired(sv));
            (paired.map(|i| i.addons.clone()).unwrap_or_default(), paired.map(|i| i.root.clone()))
        };
        let local = (!hopper::hopperd::is_remote_root(&root)).then(|| root.join("mods"));
        let name_of = |f: &str| crate::rigsync::crash_addon(&addons, f, local.as_deref());
        let stopped = stopped_change(&c);
        let what: AnyElement = match &stopped {
            Some((file, _)) => {
                let (name, a) = name_of(file);
                let icon = a.and_then(|a| self.store.read(cx).image(&a.icon)).map(Path::to_path_buf);
                let (store, pack) = (self.store.clone(), pack.clone());
                h_flex()
                    .gap_1p5()
                    .min_w_0()
                    .text_sm()
                    .child(
                        widgets::mod_link(
                            ElementId::Name("srv-strip-mod".into()),
                            name,
                            icon.as_deref(),
                            if pack.is_some() { a.map_or(0, |a| a.project_id) } else { 0 },
                            move |project, _, cx| {
                                if let Some(root) = pack.clone() {
                                    store.update(cx, |_, cx| cx.emit(crate::store::StoreEvent::Peek { root, project }));
                                }
                            },
                            cx,
                        )
                        .flex_none()
                        .font_semibold(),
                    )
                    .child(div().truncate().child("stopped it from starting"))
                    .into_any_element()
            }
            None => div().text_sm().truncate().child(crate::rigsync::crash_reason(&c)).into_any_element(),
        };
        let rel = report_rel(&c);
        let changed = stopped.and_then(|(_, ch)| ch);
        let put_back = changed.map(|ch| {
            let label = if ch.from.is_empty() { "Put the older version back".to_string() } else { format!("Put {} back", ch.from) };
            Button::new("srv-strip-put-back")
                .xsmall()
                .primary()
                .icon(IconName::Undo2)
                .label(label)
                .tooltip("Swaps in the older copy. The newer one goes to backups.")
                .disabled(self.busy)
                .on_click(cx.listener(move |this, _, window, cx| this.fix_back(ch.clone(), window, cx)))
        });
        let see = Button::new("srv-strip-see")
            .xsmall()
            .outline()
            .label("See why →")
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(StripEvent::SeeWhy(rel.clone()))));
        let place = root.to_string_lossy().into_owned();
        let key = hopper::settings::notice_key(&place, "server-crash-strip", &format!("{}|{}", c.source.to_string_lossy(), c.when));
        let body = h_flex()
            .gap_2()
            .items_center()
            .px_3()
            .py_1p5()
            .pr_8()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.danger.opacity(0.6))
            .bg(theme.danger.opacity(0.06))
            .child(Icon::new(IconName::TriangleAlert).small().text_color(theme.danger))
            .child(div().flex_none().text_sm().font_semibold().text_color(theme.danger).child("The server crashed"))
            .child(div().opacity(0.5).child("·"))
            .child(div().min_w_0().child(what))
            .child(div().flex_1())
            .children(put_back)
            .child(see);
        widgets::closable_notice(body, key, widgets::Closed::Chip, "The server crashed", theme.danger, &self.store, cx)
            .unwrap_or_else(|| div().into_any_element())
    }
}
