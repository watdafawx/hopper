//! A game that hangs while it starts: the watch (run from the store's
//! running-games tick), what it found, and the one-line strip on the
//! instance's (and a server's) page, like the crash strip: what's stuck,
//! the mod most likely behind it, "See why →" to Logs & crashes (the dump
//! and the full list are there), restart and wait. The finding itself is
//! `hopper::stuckstart`'s; servers watch themselves in `LocalServer`.

use crate::store::{Msg, Store};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex};
use gpui_kit::*;
use hopper::stuckstart::{Games, StuckReport};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// How long "Wait" waits before asking again.
const SNOOZE_SECS: i64 = 300;

/// What to do once a stuck game has ended.
#[derive(Clone, Debug, Default)]
struct AfterStop {
    /// Turn this jar off first.
    turn_off: Option<String>,
}

#[derive(Default)]
pub struct StuckState {
    /// Shared with the running-games tick, which looks at each game.
    pub games: Arc<Mutex<Games>>,
    reports: HashMap<PathBuf, StuckReport>,
    after_stop: HashMap<PathBuf, AfterStop>,
    /// The strip asks "Stop it?" before stopping (its turn-off jar, if any).
    confirm: HashMap<PathBuf, Option<String>>,
    /// Servers' stuck notices the player hid: (folder, report time).
    hidden: std::collections::HashSet<(PathBuf, i64)>,
}

/// One look at each running game, from the store's tick (a background
/// thread): a game that just went stuck is dumped and read on a thread of
/// its own, then reported with [`Msg::Stuck`].
pub fn look(
    games: &Mutex<Games>,
    running: &HashMap<PathBuf, hopper::game::Running>,
    tx: &std::sync::mpsc::Sender<Msg>,
    wake: &hopper::waker::Waker,
) {
    let mut g = games.lock().unwrap_or_else(|e| e.into_inner());
    g.retain(&running.keys().cloned().collect::<Vec<_>>());
    let now = hopper::time::now();
    for (root, r) in running {
        let Some(stall) = g.look(root, r.pid, r.started, now) else { continue };
        let (root, pid, tx, wake) = (root.clone(), r.pid, tx.clone(), wake.clone());
        std::thread::spawn(move || {
            let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let report = hopper::stuckstart::investigate(&root, &name, pid, &stall, now);
            log::info!("{}: {} ({})", root.display(), report.headline, report.summary);
            let _ = tx.send(Msg::Stuck(root, report));
            wake();
        });
    }
}

impl Store {
    pub(crate) fn stuck_found(&mut self, root: PathBuf, report: StuckReport, cx: &mut Context<Self>) {
        // The game may have ended (or started over) while the dump was read.
        if self.running(&root).is_none_or(|r| r.pid != report.pid) {
            return;
        }
        let name = self.instance(&root).map(|i| i.name.clone()).unwrap_or_default();
        let headline = if name.is_empty() { report.headline.clone() } else { report.headline.replacen(&file_name(&root), &name, 1) };
        crate::tray::notify(&headline, &report.summary, true);
        self.stuck.reports.insert(root, StuckReport { headline, ..report });
        cx.notify();
    }

    /// Games that ended: their notice goes, and a stop the notice asked for
    /// finishes (turn the mod off, start again).
    pub(crate) fn stuck_ended(&mut self, ended: &[PathBuf], cx: &mut Context<Self>) {
        for root in ended {
            self.stuck.reports.remove(root);
            self.stuck.confirm.remove(root);
            let Some(next) = self.stuck.after_stop.remove(root) else { continue };
            if let Some(jar) = next.turn_off
                && let Err(e) = self.set_enabled(root, &[jar.clone()], false)
            {
                log::warn!("turning off {jar} after a stuck start: {e}");
            }
            if let Err(e) = self.play(root) {
                log::warn!("starting {} again after a stuck start: {e}", root.display());
                crate::tray::notify("Couldn't start it again", &e, true);
            }
        }
        cx.notify();
    }

    /// The game in `root` looks stuck: what its thread dump showed.
    pub fn stuck(&self, root: &Path) -> Option<&StuckReport> {
        self.stuck.reports.get(root)
    }

    /// "Wait": hides the notice and asks again after more quiet time.
    pub fn snooze_stuck(&mut self, root: &Path) {
        self.stuck.reports.remove(root);
        self.stuck.confirm.remove(root);
        self.stuck.games.lock().unwrap_or_else(|e| e.into_inner()).snooze(root, hopper::time::now(), SNOOZE_SECS);
    }

    /// Ends the stuck game, then (once it's gone) turns `turn_off` off and
    /// starts it again.
    pub fn restart_stuck(&mut self, root: &Path, turn_off: Option<String>) {
        self.stuck.confirm.remove(root);
        self.stuck.after_stop.insert(root.to_path_buf(), AfterStop { turn_off });
        self.stop(root);
    }
}

fn file_name(root: &Path) -> String {
    root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The mod a report blames first (not one only waiting), for the strip.
fn top(r: &StuckReport) -> Option<&hopper::crashdetect::Suspect> {
    r.suspects.iter().find(|x| !x.knock_on)
}

/// The strip's frame: warning colours, icon, "<name> looks stuck …", the
/// mod, then the buttons on the right. Same shape as the crash strip
/// (`server_crash.rs`).
fn strip(title: String, what: Option<AnyElement>, buttons: Vec<AnyElement>, cx: &App) -> Div {
    let theme = cx.theme();
    h_flex()
        .gap_2()
        .items_center()
        .px_3()
        .py_1p5()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.warning.opacity(0.6))
        .bg(theme.warning.opacity(0.06))
        .child(Icon::new(IconName::Clock).small().text_color(theme.warning))
        .child(div().flex_none().text_sm().font_semibold().text_color(theme.warning).child(title))
        .children(what.map(|w| h_flex().min_w_0().gap_2().child(div().opacity(0.5).child("·")).child(w)))
        .child(div().flex_1())
        .children(buttons)
}

/// "<mod> [Likely]"; the reason is in its tooltip.
fn blamed(link: AnyElement, s: &hopper::crashdetect::Suspect, cx: &App) -> AnyElement {
    let why = SharedString::from(s.why.clone());
    h_flex()
        .id("stuck-blamed")
        .gap_1p5()
        .min_w_0()
        .text_sm()
        .child(link)
        .child(crate::crash_view::suspect_tag(s, cx))
        .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(why.clone()).build(w, cx))
        .into_any_element()
}

/// The strip on the instance's page while its game looks stuck. `see`
/// opens its Logs & crashes on the saved dump.
pub fn stuck_bar<V: 'static>(
    store: &Entity<Store>,
    root: &Path,
    see: impl Fn(Option<PathBuf>, &mut Window, &mut App) + 'static,
    cx: &mut Context<V>,
) -> Option<AnyElement> {
    let s = store.read(cx);
    let r = s.stuck(root)?.clone();
    let confirming = s.stuck.confirm.contains_key(root);
    let title = r.headline.split(" (").next().unwrap_or(&r.headline).to_string();
    let what = top(&r).map(|x| blamed(crate::logs::mod_ref(store, root, &x.file_name, "stuck-top", cx), x, cx));
    let (st, rt) = (store.clone(), root.to_path_buf());
    let (st2, rt2) = (store.clone(), root.to_path_buf());
    let dump = r.dump.clone();
    let buttons: Vec<AnyElement> = if confirming {
        vec![
            div().text_sm().child("Stop the game and start it again?").into_any_element(),
            Button::new("stuck-confirm")
                .xsmall()
                .danger()
                .label("Stop and start again")
                .on_click(move |_, _, cx| {
                    st.update(cx, |s, cx| {
                        s.restart_stuck(&rt, None);
                        cx.notify();
                    })
                })
                .into_any_element(),
            Button::new("stuck-cancel")
                .xsmall()
                .ghost()
                .label("Cancel")
                .on_click(move |_, _, cx| {
                    st2.update(cx, |s, cx| {
                        s.stuck.confirm.remove(&rt2);
                        cx.notify();
                    })
                })
                .into_any_element(),
        ]
    } else {
        vec![
            Button::new("stuck-restart")
                .xsmall()
                .primary()
                .icon(IconName::RotateCcw)
                .label("Stop and start again…")
                .tooltip("It won't finish on its own; starting again usually gets past it")
                .on_click(move |_, _, cx| {
                    st.update(cx, |s, cx| {
                        s.stuck.confirm.insert(rt.clone(), None);
                        cx.notify();
                    })
                })
                .into_any_element(),
            Button::new("stuck-see").xsmall().outline().label("See why →").on_click(move |_, w, cx| see(dump.clone(), w, cx)).into_any_element(),
            Button::new("stuck-wait")
                .xsmall()
                .ghost()
                .icon(IconName::X)
                .tooltip("Wait: hides this and asks again in 5 minutes if it's still stuck")
                .on_click(move |_, _, cx| {
                    st2.update(cx, |s, cx| {
                        s.snooze_stuck(&rt2);
                        cx.notify();
                    })
                })
                .into_any_element(),
        ]
    };
    Some(div().px_6().pt_2().child(strip(title, what, buttons, cx)).into_any_element())
}

impl Store {
    /// Hides a server's stuck strip for this report (the server watches
    /// itself and reports again if it's still stuck later).
    pub fn hide_server_stuck(&mut self, root: &Path, when: i64) {
        self.stuck.hidden.insert((root.to_path_buf(), when));
    }

    fn server_stuck_hidden(&self, root: &Path, when: i64) -> bool {
        self.stuck.hidden.contains(&(root.to_path_buf(), when))
    }
}

/// The dump's path under its folder ("crash-reports/stuck-….txt").
fn dump_rel(r: &StuckReport) -> Option<String> {
    let p = r.dump.as_ref()?.to_string_lossy().replace('\\', "/");
    p.rfind("crash-reports/").map(|i| p[i..].to_string())
}

/// The same strip on a server's page (Hopper's own servers and Hopper
/// Server's), from its stats. Restart goes through the server page's own
/// confirm (`ServerControl::request`); `see` opens its Logs & crashes on
/// the saved dump (its path under the server folder).
pub fn server_stuck_bar(
    store: &Entity<Store>,
    root: &Path,
    stats: Option<&hopper::panel::PanelStats>,
    control: &Entity<crate::server_control::ServerControl>,
    see: impl Fn(Option<String>, &mut Window, &mut App) + 'static,
    cx: &mut App,
) -> Option<AnyElement> {
    let r = stats.filter(|s| s.starting)?.stuck.clone()?;
    if store.read(cx).server_stuck_hidden(root, r.when) {
        return None;
    }
    let doing = r.headline.split(" looks stuck ").nth(1).and_then(|t| t.split(" (").next()).unwrap_or("while starting");
    let title = format!("{} looks stuck {doing}", file_name(root));
    let what = top(&r).map(|x| blamed(widgets::mod_name("srv-stuck-top", r.name_of(x), None, cx).into_any_element(), x, cx));
    let rel = dump_rel(&r);
    let ctl = control.clone();
    let (st, rt, when) = (store.clone(), root.to_path_buf(), r.when);
    let buttons = vec![
        Button::new("srv-stuck-restart")
            .xsmall()
            .primary()
            .icon(IconName::RotateCcw)
            .label("Restart server…")
            .on_click(move |_, window, cx| ctl.update(cx, |c, cx| c.request(hopper::panel::PanelAction::Restart, window, cx)))
            .into_any_element(),
        Button::new("srv-stuck-see").xsmall().outline().label("See why →").on_click(move |_, w, cx| see(rel.clone(), w, cx)).into_any_element(),
        Button::new("srv-stuck-hide")
            .xsmall()
            .ghost()
            .icon(IconName::X)
            .tooltip("Hide: the server keeps starting")
            .on_click(move |_, _, cx| {
                st.update(cx, |s, cx| {
                    s.hide_server_stuck(&rt, when);
                    cx.notify();
                })
            })
            .into_any_element(),
    ];
    Some(div().pb_2().child(strip(title, what, buttons, cx)).into_any_element())
}
