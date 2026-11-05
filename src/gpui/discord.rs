//! Discord Rich Presence: what the player is doing, shown on their Discord
//! profile through the Discord app on this PC (its local pipe; no login).
//!
//! The UI looks at what's on screen every few seconds and hands the result
//! to a background thread, which owns the connection: it connects only
//! when there's something to show, retries with a growing wait while
//! Discord isn't running, and sends at most one update every 15 s (a game
//! starting or stopping goes out at once). The fields and privacy rules are
//! in `hopper::discord`.

use crate::app::{AppView, Page};
use crate::library::InstTab;
use discord_rich_presence::{DiscordIpc, DiscordIpcClient, activity as rp};
use gpui_kit::{App, Context, Subscription, Task};
use hopper::discord::{Activity, Doing, OpenServer, Pack, Times};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

/// How often the UI checks what the player is doing.
const LOOK: Duration = Duration::from_secs(3);
/// Discord's limit on updates.
const GAP: Duration = Duration::from_secs(15);
/// Waits between tries while Discord isn't running.
const RETRY: [u64; 5] = [5, 10, 20, 40, 60];

/// The connection, for Settings: 0 not tried, 1 Discord not found, 2 connected.
static STATE: AtomicU8 = AtomicU8::new(0);

/// The line under the main switch in Settings › Discord.
pub(crate) fn status(enabled: bool) -> Option<String> {
    if !enabled {
        return None;
    }
    Some(match STATE.load(Ordering::Relaxed) {
        2 => "Connected to Discord. Friends see what you're doing.".into(),
        1 => "Discord isn't running. Hopper connects when you open it.".into(),
        _ => "Looking for Discord…".into(),
    })
}

/// The thread's mailbox, for [`shutdown`].
static QUIT: std::sync::OnceLock<Sender<Msg>> = std::sync::OnceLock::new();

/// Clears the status and closes the connection, waiting a moment so the
/// status doesn't linger after Hopper closes. Safe to call twice.
pub(crate) fn shutdown() {
    let Some(tx) = QUIT.get() else { return };
    let (done_tx, done_rx) = mpsc::channel();
    if tx.send(Msg::Quit(done_tx)).is_ok() {
        let _ = done_rx.recv_timeout(Duration::from_secs(1));
    }
}

enum Msg {
    /// What to show (None: nothing), and whether it can't wait out the gap.
    Show(Option<Activity>, bool),
    /// Clear the status and close, then reply.
    Quit(Sender<()>),
}

/// Kept by the app: the poll and the quit hook.
pub(crate) struct Hook {
    _poll: Task<()>,
    _quit: Subscription,
}

pub(crate) fn start(cx: &mut Context<AppView>) -> Hook {
    let (tx, rx) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("discord".into())
        .spawn(move || run(rx));
    let app_started = now();
    let poll_tx = tx.clone();
    let poll = cx.spawn(async move |this, cx| {
        let mut last: Option<(Doing, Option<Activity>)> = None;
        loop {
            let alive = this
                .update(cx, |this, cx| {
                    let prefs = this.store.read(cx).settings().discord.clone();
                    let (doing, game_started) = doing(this, cx);
                    let act = hopper::discord::activity(&doing, &prefs, Times { app_started, game_started });
                    if last.as_ref().is_some_and(|(_, a)| *a == act) {
                        return;
                    }
                    let urgent = last.as_ref().is_none_or(|(d, _)| doing.big_change(d));
                    let _ = poll_tx.send(Msg::Show(act.clone(), urgent));
                    last = Some((doing, act));
                })
                .is_ok();
            if !alive {
                break;
            }
            cx.background_executor().timer(LOOK).await;
        }
    });
    let _ = QUIT.set(tx);
    let quit = cx.on_app_quit(|_, _| {
        shutdown();
        async {}
    });
    Hook { _poll: poll, _quit: quit }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// What the player is doing, and when their game started if one runs.
fn doing(app: &AppView, cx: &App) -> (Doing, Option<u64>) {
    let store = app.store.read(cx);
    // A game running wins over whatever page is open: the newest one.
    if let Some((root, run)) = store.running_games().max_by_key(|(_, r)| r.started)
        && let Some(inst) = store.instance(root)
    {
        let d = Doing::Playing { pack: pack_of(inst), game_version: inst.game_version.clone(), loader: inst.loader.clone() };
        return (d, Some(run.started));
    }
    if let Some(job) = store.jobs().snapshot().into_iter().find(|j| !j.done && j.error.is_none() && j.presence.is_some()) {
        let (name, update) = job.presence.clone().unwrap_or_default();
        let pack = store
            .instances()
            .iter()
            .find(|i| i.name == name)
            .map(pack_of)
            .unwrap_or(Pack { name, ..Default::default() });
        let percent = job.progress.map(|p| (p.clamp(0.0, 1.0) * 100.0) as u8);
        return (Doing::Installing { pack, update, percent }, None);
    }
    let d = match app.page {
        Page::Library => {
            let (inst, tab, peek) = app.library.read(cx).nav_loc(cx);
            let inst = inst.as_deref().and_then(|r| store.instance(r));
            if peek.is_some() {
                app.library.read(cx).peek_view(cx).read(cx).presence()
            } else {
                match (inst, tab) {
                    (Some(i), InstTab::Configs) => Doing::Configs { pack: pack_of(i) },
                    (Some(i), InstTab::Logs) if store.crashed(&i.root) => Doing::Crash { pack: pack_of(i) },
                    _ => Doing::Idle,
                }
            }
        }
        Page::Browse => app.browse.read(cx).presence(),
        Page::Servers => {
            let servers = store.servers();
            let running = |root: &std::path::Path| store.panel_stats(root).is_some_and(|p| p.running);
            let picked = app.servers.read(cx).nav_loc().0;
            let open = picked.and_then(|r| servers.iter().find(|s| s.root == r)).map(|s| {
                let st = store.panel_stats(&s.root);
                OpenServer {
                    name: s.name.clone(),
                    running: running(&s.root),
                    online: st.map_or(0, |p| p.online),
                    max: st.map_or(0, |p| p.max),
                }
            });
            Doing::Servers { count: servers.len(), running: servers.iter().filter(|s| running(&s.root)).count(), open }
        }
        Page::Settings => Doing::Idle,
    };
    (d, None)
}

/// An instance as Discord may show it: only site links, never its folder
/// or a local icon (`hopper::discord` checks again before sending).
fn pack_of(inst: &hopper::instances::Instance) -> Pack {
    let p = inst.pack.as_ref();
    let page_url = p.and_then(|p| {
        if !p.mr_project.is_empty() {
            Some(format!("https://modrinth.com/modpack/{}", p.mr_project))
        } else if p.project_id > 0 {
            Some(format!("https://www.curseforge.com/projects/{}", p.project_id))
        } else {
            None
        }
    });
    Pack {
        name: inst.name.clone(),
        version: p.map(|p| p.version.clone()).unwrap_or_default(),
        icon_url: inst.icon.clone().filter(|u| u.starts_with("https://")),
        page_url,
        mods: inst.addons.iter().filter(|a| a.file_name.ends_with(".jar")).count(),
    }
}

/// The connection thread.
fn run(rx: Receiver<Msg>) {
    let mut client: Option<DiscordIpcClient> = None;
    // What should show, and what Discord has (None: not known since connecting).
    let mut want: Option<Activity> = None;
    let mut sent: Option<Option<Activity>> = None;
    let mut urgent = false;
    let mut last_send: Option<Instant> = None;
    let mut tries = 0usize;
    let mut next_try = Instant::now();
    loop {
        let now = Instant::now();
        // Nothing to show and not connected: no reason to touch Discord.
        if client.is_none() && want.is_some() && now >= next_try {
            let mut c = DiscordIpcClient::new(hopper::discord::APP_ID);
            match c.connect() {
                Ok(()) => {
                    log::info!("discord: connected");
                    STATE.store(2, Ordering::Relaxed);
                    client = Some(c);
                    sent = None;
                    tries = 0;
                    urgent = true;
                }
                Err(_) => {
                    STATE.store(1, Ordering::Relaxed);
                    // Not running (or not installed): wait longer each time.
                    next_try = now + Duration::from_secs(RETRY[tries.min(RETRY.len() - 1)]);
                    tries += 1;
                }
            }
        }
        if let Some(c) = client.as_mut()
            && sent.as_ref() != Some(&want)
        {
            let due = urgent || last_send.is_none_or(|t| now.duration_since(t) >= GAP);
            if due {
                let r = match &want {
                    Some(a) => c.set_activity(to_rp(a)),
                    None => c.clear_activity(),
                };
                // Read Discord's answer so the pipe doesn't fill up.
                let r = r.and_then(|_| c.recv()).map(|(_, v)| {
                    // A field Discord doesn't take refuses the whole update.
                    if v["evt"] == "ERROR" {
                        log::warn!("discord: update refused: {}", v["data"]["message"]);
                    } else {
                        log::debug!("discord: showing {:?}", want.as_ref().map(|a| (&a.details, &a.state)));
                    }
                });
                last_send = Some(Instant::now());
                urgent = false;
                match r {
                    Ok(()) => sent = Some(want.clone()),
                    Err(e) => {
                        // Discord closed: start over with a new client
                        // (the crate's reconnect keeps the dead pipe).
                        log::info!("discord: lost ({e})");
                        STATE.store(1, Ordering::Relaxed);
                        client = None;
                        next_try = Instant::now() + Duration::from_secs(RETRY[0]);
                    }
                }
            }
        }
        // Sleep until a message, the next send slot, or the next try.
        let mut wait = Duration::from_secs(3600);
        if client.is_some() && sent.as_ref() != Some(&want) {
            let ready = last_send.map_or(Instant::now(), |t| t + GAP);
            wait = wait.min(ready.saturating_duration_since(Instant::now()));
        }
        if client.is_none() && want.is_some() {
            wait = wait.min(next_try.saturating_duration_since(Instant::now()));
        }
        match rx.recv_timeout(wait.max(Duration::from_millis(50))) {
            Ok(Msg::Show(a, big)) => {
                urgent |= big;
                want = a;
            }
            Ok(Msg::Quit(done)) => {
                if let Some(mut c) = client.take() {
                    let _ = c.clear_activity().and_then(|_| c.recv().map(|_| ()));
                    let _ = c.close();
                    log::info!("discord: cleared on exit");
                }
                let _ = done.send(());
                return;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn to_rp(a: &Activity) -> rp::Activity<'_> {
    let mut assets = rp::Assets::new().large_image(a.large_image.as_str()).large_text(a.large_text.as_str());
    if let Some(s) = &a.small_image {
        assets = assets.small_image(s.as_str());
    }
    if let Some(s) = &a.small_text {
        assets = assets.small_text(s.as_str());
    }
    let mut out = rp::Activity::new()
        .activity_type(rp::ActivityType::Playing)
        .details(a.details.as_str())
        .assets(assets)
        .timestamps(rp::Timestamps::new().start(a.start as i64 * 1000))
        .buttons(a.buttons.iter().map(|(l, u)| rp::Button::new(l.as_str(), u.as_str())).collect())
        .status_display_type(if a.show_details { rp::StatusDisplayType::Details } else { rp::StatusDisplayType::Name });
    if let Some(s) = &a.state {
        out = out.state(s.as_str());
    }
    out
}
