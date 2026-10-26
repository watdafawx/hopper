//! The app's shared state: settings, instances, running games, background
//! jobs and cached images. Views read it and call its methods; worker
//! threads report back through `Msg`.

use gpui_kit::*;
use hopper::game::Running;
use hopper::install::{self, Update};
use hopper::instances::{self, Instance};
use hopper::jobs::{Job, Jobs};
use hopper::server::{ServerInfo, Status};
use hopper::settings::Settings;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Results from worker threads.
/// Hopper's own update, as far as it has got.
#[derive(Clone, Debug, PartialEq)]
pub enum SelfUpdate {
    Checking,
    UpToDate,
    Available(hopper::selfupdate::Release),
    Installing(String),
    /// Installed; runs from the next start.
    Ready(String),
    Failed(String),
}

pub enum Msg {
    SelfUpdate(SelfUpdate),
    /// What the background check found; applies only if nothing newer did.
    SelfUpdateRecheck(SelfUpdate),
    /// Look for pack updates again (after a failed update took the offer).
    RecheckPacks,
    /// An instance was deleted.
    Forget(PathBuf),
    /// The rig's servers, read off the UI thread (it's a network share).
    RigServers(Vec<ServerInfo>),
    /// A Hopper Server's servers (by panel id), or why it couldn't list them.
    PanelServers(String, Result<Vec<ServerInfo>, String>),
    /// An instance changed on disk; reload it.
    InstanceChanged(PathBuf),
    /// Jars were identified after an older copy went back: reload, then
    /// ask for updates again (the record now names the older file).
    Reidentified(PathBuf),
    /// Mods were updated: the instance is read again, and only those
    /// projects leave its list of available updates (no full re-check).
    /// Pages to open in the browser (Hopper's download window couldn't).
    OpenUrls(Vec<String>),
    /// Projects updated, and download pages to fetch.
    Updated(PathBuf, Vec<u64>, Vec<String>),
    Running(HashMap<PathBuf, Running>),
    /// Categories looked up for installed projects, by [`hopper::modmeta::key`].
    Categories(HashMap<String, Vec<String>>),
    Image {
        url: String,
        path: Option<PathBuf>,
    },
    /// An animated image decoded for playback (`None`: show the still).
    Anim {
        key: String,
        clip: Option<Arc<crate::anim::Clip>>,
    },
    /// What each update would stop loading: project → (mod, the range it
    /// needs).
    Breaks(PathBuf, HashMap<u64, Vec<(String, String)>>),
    Updates {
        root: PathBuf,
        result: Result<Vec<Update>, String>,
        /// The list before this check, to keep what it found.
        prev: Vec<Update>,
    },
    Status {
        key: PathBuf,
        status: Status,
    },
    /// Re-read the servers folder (a server was updated).
    ServersChanged,
    Javas(Vec<hopper::java::JavaInstall>),
    Link(String),
    /// Newer modpack files, by pack project.
    PackUpdates(Vec<(u64, hopper::cf::File)>),
    /// A modpack's project and description, by pack project.
    PackPage(u64, Result<(hopper::cf::Mod, Vec<hopper::richtext::Segment>, Vec<String>), String>),
    /// A play session was recorded for this instance.
    SessionSaved(PathBuf),
    /// What an instance's last game crash names (`None`: it ended fine).
    Crashed(PathBuf, Option<hopper::crashdetect::Crash>),
    /// A saved crash read again with today's rules (no notification).
    CrashReread(PathBuf, hopper::crashdetect::Crash),
    /// A running game wrote a crash report and is still open (its error
    /// screen).
    LiveCrash(PathBuf, hopper::crashdetect::Crash),
    /// Mods whose mixins failed while a game runs.
    LiveWarnings(PathBuf, Vec<hopper::crashdetect::Suspect>),
    /// A translation finished (see `crate::translation`).
    Translated,
    /// A translation model finished downloading (by language code).
    TrDownloaded(&'static str),
    /// A job that reports only through its own progress line.
    Nothing,
}

/// What the store tells the window about.
pub enum StoreEvent {
    /// A game started (none was running).
    GameStarted,
    /// The last running game ended.
    GamesEnded,
    /// A curseforge:// link arrived (from the website or a second Hopper).
    Link(String),
    /// Another Hopper asks to pair.
    PairRequest(hopper::peer::PairRequest),
    /// An instance's game crashed: (its name, the mod Hopper suspects most,
    /// if any, whether the game is still open on its error screen).
    GameCrashed(String, Option<String>, bool),
    /// The hosted-server scheduler did something: (message, is a problem).
    Schedule(String, bool),
    /// A play session of this instance was saved.
    SessionSaved(PathBuf),
    /// Show a project's page beside an instance (a link in a changelog).
    Peek { root: PathBuf, project: u64 },
    /// Open a server's page (`compare`: on its Sync tab), from a menu
    /// that isn't on that page.
    ShowServer { root: PathBuf, compare: bool },
    /// Open an instance in the Library.
    ShowInstance(PathBuf),
}

impl EventEmitter<StoreEvent> for Store {}

/// Pending updates for one instance.
/// How long an instance's update list counts as current.
pub const UPDATES_MAX_AGE: Duration = Duration::from_secs(30 * 60);
/// How often a running Hopper looks for a newer release of itself
/// (GitHub allows 60 unauthenticated API calls an hour).
pub const SELF_UPDATE_EVERY: Duration = Duration::from_secs(30 * 60);

pub enum Updates {
    Checking,
    Ready(Vec<Update>),
    Failed(String),
}

/// A modpack's page from CurseForge or Modrinth, for the Overviews.
pub enum PackPage {
    Loading,
    /// The project, its description (`richtext::to_segments`) and the
    /// description's image keys (see `imgcache::split_width`).
    Ready(hopper::cf::Mod, Vec<hopper::richtext::Segment>, Vec<String>),
    Failed(String),
}

/// A cached remote image.
#[derive(Clone)]
enum Image {
    Loading,
    Ready(PathBuf),
    Failed,
}

/// An image key wanted animated (see [`Store::want_anim`]).
enum Anim {
    /// Waiting for the image itself to be cached.
    Waiting,
    Decoding,
    Ready(Arc<crate::anim::Clip>),
    /// One frame, too big, or failed: the still is shown.
    Still,
}

/// Decoded clips kept at once; the oldest goes (and frees its GPU copies).
const ANIMS_KEPT: usize = 12;

/// Marks the key of a clip decoded big for the image viewer (see
/// [`Store::want_anim_view`]), apart from the page's copy of the same image.
const VIEW_SUFFIX: &str = "#view";

/// The key of image `url`'s big clip for the image viewer.
pub fn view_key(url: &str) -> String {
    format!("{url}{VIEW_SUFFIX}")
}

/// What a clip key decodes: the URL, the width to show it at, and how big
/// it may get.
fn anim_parts(key: &str) -> (&str, Option<u32>, crate::anim::Limits) {
    match key.strip_suffix(VIEW_SUFFIX) {
        Some(url) => (url, None, crate::anim::VIEWER),
        None => {
            let (url, width) = hopper::imgcache::split_width(key);
            (url, width, crate::anim::PAGE)
        }
    }
}

pub struct Store {
    settings: Settings,
    instances: Vec<Instance>,
    running: HashMap<PathBuf, Running>,
    /// Each instance's last game crash, read from its file once asked for.
    crashes: HashMap<PathBuf, Option<hopper::crashdetect::Crash>>,
    /// Crash reports already told while their game was still open.
    live_told: HashMap<PathBuf, PathBuf>,
    /// Mixin failures of each instance's current or last game.
    warnings: HashMap<PathBuf, Vec<hopper::crashdetect::Suspect>>,
    jobs: Jobs,
    /// Notified when only a running job's detail or progress moved, which
    /// the tasks button shows: observing the store would redraw whole pages
    /// up to 20 times a second through a download.
    job_ticks: Entity<JobTicks>,
    images: HashMap<String, Image>,
    anims: HashMap<String, Anim>,
    /// Ready clips' keys, oldest first.
    anims_ready: std::collections::VecDeque<String>,
    /// Installed projects' categories (`None` until the disk cache is read);
    /// keys being fetched are in `cats_pending`.
    cats: Option<HashMap<String, Vec<String>>>,
    cats_pending: std::collections::HashSet<String>,
    updates: HashMap<PathBuf, Updates>,
    /// Per instance, the mods each update would stop loading (see
    /// [`Store::update_breaks`]).
    breaks: HashMap<PathBuf, HashMap<u64, Vec<(String, String)>>>,
    /// When each instance's update check last started.
    updates_at: HashMap<PathBuf, Instant>,
    servers: Vec<ServerInfo>,
    /// Hopper's own Microsoft accounts (for launching directly).
    accounts: hopper::accounts::Accounts,
    /// The old single sign-in is saved but not yet in the list.
    legacy_login: bool,
    pack_updates: HashMap<u64, hopper::cf::File>,
    pack_pages: HashMap<u64, PackPage>,
    /// Java installs on this PC, once detected.
    javas: Option<Vec<hopper::java::JavaInstall>>,
    javas_asked: bool,
    statuses: HashMap<PathBuf, Status>,
    /// Each server's state in the server panel, when one is set up.
    panel_stats: HashMap<PathBuf, hopper::panel::PanelStats>,
    /// The servers folder's servers, before merging.
    folder_servers: Vec<ServerInfo>,
    /// Each Hopper Server's servers (by panel id), or why it couldn't list them.
    panel_servers: HashMap<String, Result<Vec<ServerInfo>, String>>,
    /// Pack files that couldn't be downloaded automatically, per instance.
    pending: HashMap<PathBuf, Vec<hopper::modpack::Pending>>,
    /// Servers Hopper hosts on this PC, and the panel that runs them.
    local: Arc<hopper::hosting::LocalPanel>,
    /// Daily restarts, crash restarts and autostart for hosted servers.
    scheduler: Option<hopper::hosting::Scheduler>,
    /// Instances jobs are changing, with how many jobs.
    busy: Arc<std::sync::Mutex<HashMap<PathBuf, u32>>>,
    /// Instances whose game is being launched (the Play button says so).
    launching: Arc<std::sync::Mutex<std::collections::HashSet<PathBuf>>>,
    /// Mod updates of instances an update job is running on: the batch
    /// it's on and what waits for it.
    update_queue: Arc<Mutex<HashMap<PathBuf, UpdateQueue>>>,
    /// Mods updated by a job, by instance, until the mods table has shown
    /// them as Updated and let them go ([`Store::forget_updates`]).
    recent_updates: Arc<Mutex<HashMap<PathBuf, Vec<Update>>>>,
    /// Java downloads by key ("java-21", "graalvm-25"): their jobs.
    java_jobs: HashMap<String, u64>,
    /// Pages for the download window, and whether it's open.
    fetch_queue: Arc<Mutex<(bool, Vec<String>)>>,
    /// Image URLs waiting for the download workers.
    image_queue: Arc<(std::sync::Mutex<std::collections::VecDeque<String>>, std::sync::Condvar)>,
    /// Hopper's own update, once checked.
    self_update: Option<SelfUpdate>,
    _panel_poll: Option<Task<()>>,
    /// Watches Downloads for files to download by hand.
    _download_watch: Option<Task<()>>,
    _update_watch: Option<Task<()>>,
    _self_update_watch: Option<Task<()>>,
    /// Bumped whenever servers change on disk.
    server_rev: u64,
    peer_hub: Option<std::sync::Arc<hopper::peer::Hub>>,
    peer_error: Option<String>,
    /// What the hub last shared, to update it when instances change.
    shared_sig: u64,
    /// The pair request already shown.
    asked: Option<String>,
    /// Another Hopper asked to pair this run: a sign the user runs servers.
    pair_asked: bool,
    /// Modpacks (project ids) whose server files were downloaded this run.
    server_files_got: std::collections::HashSet<u64>,
    /// Instance roots the game watcher looks for.
    watched: Arc<Mutex<Vec<PathBuf>>>,
    /// Translated changelogs and descriptions.
    translations: crate::translation::Translations,
    /// Notified when a translation finishes, for views that are cached.
    tr_tick: Entity<TrTick>,
    tx: Sender<Msg>,
    wake: hopper::waker::Waker,
    /// Settings as last written, to save only real changes.
    saved_settings: String,
    _poll: Task<()>,
}

impl Store {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut settings = Settings::load_or_default();
        // Signed in before sign-in switched this: start games directly.
        if !settings.launch_native
            && !settings.launch_picked
            && hopper::msauth::client_id().is_some()
            && !crate::accounts::mock()
            && hopper::msauth::has_saved_login()
        {
            settings.launch_native = true;
            if let Err(e) = settings.save() {
                log::warn!("saving settings: {e:#}");
            }
        }
        let (tx, rx) = channel();
        let dirty = Arc::new(AtomicBool::new(true));
        let wake: hopper::waker::Waker = {
            let dirty = dirty.clone();
            Arc::new(move || dirty.store(true, Ordering::Relaxed))
        };
        let ticked = Arc::new(AtomicBool::new(false));
        let tick: hopper::waker::Waker = {
            let ticked = ticked.clone();
            Arc::new(move || ticked.store(true, Ordering::Relaxed))
        };
        let watched = Arc::new(Mutex::new(Vec::new()));
        {
            let (watched, tx, wake) = (watched.clone(), tx.clone(), wake.clone());
            let (tx2, wake2) = (tx.clone(), wake.clone());
            // Running games' files: a crash report while the game is still
            // open, and mixin failures as they're logged.
            let live = Mutex::new(hopper::crashdetect::LiveWatch::default());
            hopper::game::watch(
                move || watched.lock().map(|w| w.clone()).unwrap_or_default(),
                move |r| {
                    let _ = tx.send(Msg::Running(r));
                    wake();
                },
                move |running| {
                    let mut live = live.lock().unwrap_or_else(|e| e.into_inner());
                    live.retain(&running.keys().cloned().collect::<Vec<_>>());
                    for (root, r) in running {
                        let dir = root.join("mods");
                        let mods: Vec<String> = std::fs::read_dir(&dir)
                            .into_iter()
                            .flatten()
                            .flatten()
                            .map(|e| e.file_name().to_string_lossy().into_owned())
                            .filter(|n| n.ends_with(".jar") || n.ends_with(".jar.disabled"))
                            .collect();
                        let news = live.look(root, r.started as i64, &mods, |f| hopper::deps::own_ids(&dir.join(f)).0);
                        if let Some(c) = news.crash {
                            let _ = tx2.send(Msg::LiveCrash(root.clone(), c));
                            wake2();
                        }
                        if !news.warnings.is_empty() {
                            let _ = tx2.send(Msg::LiveWarnings(root.clone(), news.warnings));
                            wake2();
                        }
                    }
                },
            );
        }
        {
            let (tx, wake) = (tx.clone(), wake.clone());
            hopper::links::listen(move |url| {
                let _ = tx.send(Msg::Link(url));
                wake();
            });
        }
        let mut store = Self {
            saved_settings: serde_json::to_string(&settings).unwrap_or_default(),
            settings,
            instances: Vec::new(),
            running: HashMap::new(),
            crashes: HashMap::new(),
            live_told: HashMap::new(),
            warnings: HashMap::new(),
            jobs: Jobs::with_ticker(wake.clone(), tick),
            job_ticks: cx.new(|_| JobTicks),
            images: HashMap::new(),
            anims: HashMap::new(),
            anims_ready: Default::default(),
            cats: None,
            cats_pending: Default::default(),
            updates: HashMap::new(),
            breaks: HashMap::new(),
            updates_at: HashMap::new(),
            servers: Vec::new(),
            accounts: Default::default(),
            legacy_login: false,
            pack_updates: HashMap::new(),
            pack_pages: HashMap::new(),
            javas: None,
            javas_asked: false,
            statuses: HashMap::new(),
            panel_stats: HashMap::new(),
            folder_servers: Vec::new(),
            panel_servers: HashMap::new(),
            busy: Default::default(),
            launching: Default::default(),
            update_queue: Default::default(),
            recent_updates: Default::default(),
            java_jobs: HashMap::new(),
            translations: crate::translation::Translations::new({
                let (tx, wake) = (tx.clone(), wake.clone());
                Arc::new(move || {
                    let _ = tx.send(Msg::Translated);
                    wake();
                })
            }),
            tr_tick: cx.new(|_| TrTick),
            fetch_queue: Default::default(),
            pending: HashMap::new(),
            local: Arc::new(hopper::hosting::LocalPanel::new()),
            scheduler: None,
            image_queue: Default::default(),
            self_update: None,
            _panel_poll: None,
            _download_watch: None,
            _update_watch: None,
            _self_update_watch: None,
            server_rev: 0,
            peer_hub: None,
            peer_error: None,
            shared_sig: 0,
            asked: None,
            pair_asked: false,
            server_files_got: Default::default(),
            watched,
            tx,
            wake,
            _poll: Self::poll(rx, dirty, ticked, cx),
        };
        store.refresh();
        store.reload_accounts();
        store._panel_poll = Some(Self::panel_poll(cx));
        store._download_watch = Some(Self::download_watch(cx));
        store._update_watch = Some(Self::update_watch(cx));
        // Hosted servers were found by refresh(): start their scheduler.
        let scheduler = hopper::hosting::Scheduler::start(store.local.clone());
        scheduler.start_autostart(&store.local);
        store.scheduler = Some(scheduler);
        store.start_image_workers();
        hopper::selfupdate::clean_up();
        // Debug builds are dev copies: they don't nag about releases.
        if !cfg!(debug_assertions) {
            store.check_self_update();
        }
        store._self_update_watch = Self::self_update_watch(cx);
        if store.settings.peer_sync {
            store.start_peers();
        }
        // Packs kept from updates that weren't used for a while go.
        let keep = store.settings.pack_keep_days;
        std::thread::spawn(move || hopper::modpack::prune_packs_older_than(keep));
        store
    }

    /// Checks mod updates again for every instance whose list is older than
    /// [`UPDATES_MAX_AGE`], so counts and buttons catch files released
    /// while Hopper stays open.
    fn update_watch(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(60)).await;
                let ok = this.update(cx, |s, cx| {
                    let stale: Vec<PathBuf> = s
                        .updates
                        .keys()
                        .filter(|r| s.updates_stale(r) && !s.is_busy(r))
                        .cloned()
                        .collect();
                    for r in &stale {
                        s.check_updates(r, false);
                    }
                    if !stale.is_empty() {
                        cx.notify();
                    }
                });
                if ok.is_err() {
                    return;
                }
            }
        })
    }

    /// Looks for a newer Hopper every [`SELF_UPDATE_EVERY`] while Hopper
    /// stays open, so a release cut mid-session shows up in the title bar.
    /// `HOPPER_SELF_UPDATE_EVERY` (seconds) overrides the interval and also
    /// turns it on in debug builds, for testing.
    fn self_update_watch(cx: &mut Context<Self>) -> Option<Task<()>> {
        let every = std::env::var("HOPPER_SELF_UPDATE_EVERY")
            .ok()
            .and_then(|v| v.parse().ok())
            .map(Duration::from_secs);
        if every.is_none() && cfg!(debug_assertions) {
            return None;
        }
        let every = every.unwrap_or(SELF_UPDATE_EVERY);
        Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(every).await;
                if this.update(cx, |s, _| s.recheck_self_update()).is_err() {
                    return;
                }
            }
        }))
    }

    /// Asks the server panel for every server's state every few seconds.
    /// While any instance waits on files to download by hand, looks in
    /// Downloads every few seconds and puts each one in place as it lands
    /// (like Prism), then deletes it from Downloads.
    fn download_watch(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(3)).await;
                let Ok(roots) = this.update(cx, |s, _| {
                    s.pending
                        .keys()
                        .filter(|r| s.hold_reason(r).is_none() && !s.is_busy(r))
                        .cloned()
                        .collect::<Vec<_>>()
                }) else {
                    return;
                };
                if roots.is_empty() {
                    continue;
                }
                let ready = cx
                    .background_spawn(async move {
                        let dirs = hopper::modpack::landing_dirs();
                        roots
                            .into_iter()
                            .filter(|r| dirs.iter().any(|d| !hopper::modpack::ready_downloads(r, d).is_empty()))
                            .collect::<Vec<_>>()
                    })
                    .await;
                if ready.is_empty() {
                    continue;
                }
                let ok = this.update(cx, |s, _| {
                    for root in ready {
                        let Some(mut inst) = s.instance(&root).cloned() else { continue };
                        s.spawn_instance_job(&root, format!("Adding downloaded files to {}", inst.name), move |job| {
                            let mut placed = Vec::new();
                            for d in hopper::modpack::landing_dirs() {
                                placed.extend(hopper::modpack::place_downloads(&mut inst, &d)?);
                            }
                            let left = hopper::modpack::pending_downloads(&inst).len();
                            job.detail(if left == 0 {
                                format!("Added {}. Every file is in place", placed.join(", "))
                            } else {
                                format!("Added {}. {left} still to download", placed.join(", "))
                            });
                            Ok(Msg::InstanceChanged(inst.root.clone()))
                        });
                    }
                });
                if ok.is_err() {
                    return;
                }
            }
        })
    }

    fn panel_poll(cx: &mut Context<Self>) -> Task<()> {
        use hopper::panel::{PanelServer, ServerPanel};
        cx.spawn(async move |this, cx| {
            // The panel's server list, re-read every minute or when the
            // panel settings change.
            let mut listed: Option<(String, std::time::Instant, Vec<PanelServer>)> = None;
            loop {
                let Ok(setup) = this.update(cx, |s, _| {
                    let panel: Arc<dyn ServerPanel> =
                        hopper::panel::from_settings(&s.settings)?.into();
                    let key = format!(
                        "{}|{}|{}|{}",
                        s.settings.panel_kind,
                        s.settings.panel_url,
                        s.settings.panel_self_signed,
                        hopper::panel::token().map_or(0, |t| t.len())
                    );
                    // Servers on a Hopper Server are asked their own panel.
                    let servers: Vec<ServerInfo> = s.servers.iter().filter(|x| x.remote.is_none()).cloned().collect();
                    Some((panel, key, servers))
                }) else {
                    return;
                };
                let remotes: Vec<(hopper::hopperd::Remote, Vec<(String, PathBuf)>)> = this
                    .update(cx, |s, _| {
                        let mut by: Vec<(hopper::hopperd::Remote, Vec<(String, PathBuf)>)> = Vec::new();
                        for x in &s.servers {
                            let Some(r) = &x.remote else { continue };
                            match by.iter_mut().find(|(k, _)| k.panel == r.panel) {
                                Some((_, v)) => v.push((r.id.clone(), x.root.clone())),
                                None => by.push((r.clone(), vec![(r.id.clone(), x.root.clone())])),
                            }
                        }
                        by
                    })
                    .unwrap_or_default();
                let remote_found: Vec<(PathBuf, hopper::panel::PanelStats)> = cx
                    .background_spawn(async move {
                        let mut out = Vec::new();
                        for (r, ids) in remotes {
                            let Ok(c) = hopper::hopperd::Client::for_remote(&r) else { continue };
                            let Ok(all) = hopper::panel::HopperServer::from_client(c).all_stats() else { continue };
                            for (id, root) in ids {
                                if let Some((_, st)) = all.iter().find(|(i, _)| *i == id) {
                                    out.push((root, st.clone()));
                                }
                            }
                        }
                        out
                    })
                    .await;
                let found = match setup {
                    None => Vec::new(),
                    Some((panel, key, servers)) => {
                        let stale = listed.as_ref().is_none_or(|(k, at, _)| {
                            *k != key || at.elapsed() > Duration::from_secs(60)
                        });
                        let known = if stale { None } else { listed.as_ref().map(|l| l.2.clone()) };
                        let (list, stats) = cx
                            .background_spawn(async move {
                                let list = match known {
                                    Some(l) => l,
                                    None => panel.servers().unwrap_or_default(),
                                };
                                let stats: Vec<(PathBuf, hopper::panel::PanelStats)> = servers
                                    .iter()
                                    .filter_map(|srv| {
                                        let id = &hopper::panel::match_server(&list, srv)?.id;
                                        Some((srv.root.clone(), panel.stats(id).ok()?))
                                    })
                                    .collect();
                                (list, stats)
                            })
                            .await;
                        if stale {
                            listed = Some((key, std::time::Instant::now(), list));
                        }
                        stats
                    }
                };
                // Hosted servers: straight from the local panel, no network.
                let local = this
                    .update(cx, |s, _| {
                        let l = s.local.clone();
                        s.servers
                            .iter()
                            .filter(|x| s.is_hosted(&x.root))
                            .filter_map(|x| {
                                let id = x.root.to_string_lossy().to_string();
                                hopper::panel::ServerPanel::stats(&*l, &id)
                                    .ok()
                                    .map(|st| (x.root.clone(), st))
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let found: Vec<_> = found.into_iter().chain(local).chain(remote_found).collect();
                let alive = this.update(cx, |s, cx| {
                    let events = s.scheduler.as_ref().map(|x| x.events()).unwrap_or_default();
                    for e in events {
                        use hopper::hosting::ScheduleEvent as E;
                        let bad = matches!(e, E::GaveUp { .. } | E::Failed { .. } | E::CrashRestarted { .. });
                        cx.emit(StoreEvent::Schedule(e.message(), bad));
                    }
                    let next: HashMap<_, _> = found.into_iter().collect();
                    if next != s.panel_stats {
                        s.panel_stats = next;
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    return;
                }
                cx.background_executor()
                    .timer(Duration::from_secs(5))
                    .await;
            }
        })
    }

    /// Drains worker messages and redraws when anything changed.
    fn poll(rx: Receiver<Msg>, dirty: Arc<AtomicBool>, ticked: Arc<AtomicBool>, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                let msgs: Vec<Msg> = rx.try_iter().collect();
                if msgs.is_empty() && !dirty.swap(false, Ordering::Relaxed) {
                    if ticked.swap(false, Ordering::Relaxed) {
                        let alive = this.update(cx, |s, cx| s.job_ticks.update(cx, |_, cx| cx.notify()));
                        if alive.is_err() {
                            break;
                        }
                    }
                    continue;
                }
                ticked.store(false, Ordering::Relaxed);
                let alive = this.update(cx, |store, cx| {
                    for m in msgs {
                        store.handle(m, cx);
                    }
                    store.peer_tick(cx);
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
    }

    fn handle(&mut self, msg: Msg, cx: &mut Context<Self>) {
        match msg {
            Msg::Updated(root, done, pages) => {
                // Website-only files: the download window gets them, as the
                // user asked to update them.
                self.fetch_pages(pages);
                let kept = match self.updates.remove(&root) {
                    Some(Updates::Ready(v)) => Some(Updates::Ready(
                        v.into_iter().filter(|u| !done.contains(&u.project_id)).collect(),
                    )),
                    other => other,
                };
                self.handle(Msg::InstanceChanged(root.clone()), cx);
                if let Some(k) = kept {
                    self.updates.insert(root, k);
                }
            }
            Msg::Reidentified(root) => {
                self.handle(Msg::InstanceChanged(root.clone()), cx);
                self.check_updates(&root, true);
            }
            Msg::OpenUrls(urls) => {
                for u in &urls {
                    cx.open_url(u);
                }
            }
            Msg::InstanceChanged(root) => {
                if let Some(new) = instances::load(&root) {
                    self.read_pending(&new);
                    // Updates stay listed while their old file is still
                    // there; asking CurseForge again would blank the list.
                    let kept = match self.updates.remove(&root) {
                        Some(Updates::Ready(v)) => Some(Updates::Ready(
                            v.into_iter()
                                .filter(|u| new.addons.iter().any(|a| a.project_id == u.project_id && a.file_name == u.from))
                                .collect(),
                        )),
                        _ => None,
                    };
                    let icon = new.icon.clone();
                    match self.instances.iter_mut().find(|i| i.root == root) {
                        Some(slot) => *slot = new,
                        None => {
                            self.instances.push(new);
                            self.sync_watched();
                        }
                    }
                    if let Some(k) = kept {
                        self.updates.insert(root.clone(), k);
                    }
                    if let Some(url) = icon {
                        self.want_image(&url);
                    }
                }
            }
            Msg::Link(url) => cx.emit(StoreEvent::Link(url)),
            Msg::Running(now) => {
                if self.running.is_empty() && !now.is_empty() {
                    cx.emit(StoreEvent::GameStarted);
                } else if !self.running.is_empty() && now.is_empty() {
                    cx.emit(StoreEvent::GamesEnded);
                }
                for root in now.keys().filter(|k| !self.running.contains_key(*k)) {
                    self.warnings.remove(root);
                }
                if self.settings.record_sessions {
                    for (root, r) in now.iter().filter(|(k, _)| !self.running.contains_key(*k)) {
                        self.record_session(root, r);
                    }
                }
                let ended: Vec<(PathBuf, u64)> = self
                    .running
                    .iter()
                    .filter(|(k, _)| !now.contains_key(*k))
                    .map(|(k, v)| (k.clone(), v.started))
                    .collect();
                self.running = now;
                let secs_now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                for (root, started) in ended {
                    // Files the game held open during an update can go now.
                    if hopper::locked::flush(&root) > 0 {
                        self.handle(Msg::InstanceChanged(root.clone()), cx);
                    }
                    // Did it crash, and which mods does the crash name? Read
                    // in the background: crash reports and logs run to MBs.
                    if let Some(i) = self.instances.iter().find(|i| i.root == root) {
                        let mods: Vec<String> = std::fs::read_dir(i.mods_dir())
                            .into_iter()
                            .flatten()
                            .flatten()
                            .map(|e| e.file_name().to_string_lossy().into_owned())
                            .filter(|n| n.ends_with(".jar") || n.ends_with(".jar.disabled"))
                            .collect();
                        let (tx, wake, root, dir) = (self.tx.clone(), self.wake.clone(), root.clone(), i.mods_dir());
                        std::thread::spawn(move || {
                            let c = hopper::crashdetect::detect(&root, started as i64, &mods, |f| {
                                hopper::deps::own_ids(&dir.join(f)).0
                            });
                            let _ = tx.send(Msg::Crashed(root, c));
                            wake();
                        });
                    }
                    // Count the session (sanity-capped at a day).
                    if let Some(i) = self.instances.iter_mut().find(|i| i.root == root) {
                        i.play_seconds += secs_now.saturating_sub(started).min(86_400);
                        i.last_played = Some(hopper::time::now_iso());
                        if let Err(e) = i.save() {
                            log::warn!("saving play time: {e:#}");
                        }
                    }
                }
            }
            Msg::LiveCrash(root, c) => {
                hopper::crashdetect::save(&root, &c);
                self.live_told.insert(root.clone(), c.source.clone());
                if let Some(i) = self.instances.iter().find(|i| i.root == root) {
                    let top = self.top_suspect(i, &c);
                    cx.emit(StoreEvent::GameCrashed(i.name.clone(), top, true));
                }
                self.crashes.insert(root, Some(c));
                cx.notify();
            }
            Msg::LiveWarnings(root, w) => {
                let list = self.warnings.entry(root).or_default();
                list.extend(w);
                cx.notify();
            }
            // A saved crash read again: no news, just a better guess.
            Msg::CrashReread(root, c) => {
                hopper::crashdetect::save(&root, &c);
                self.crashes.insert(root, Some(c));
                cx.notify();
            }
            Msg::Crashed(root, c) => {
                // Told already while the game was open: just keep it.
                let told = self.live_told.remove(&root);
                match &c {
                    Some(c) => hopper::crashdetect::save(&root, c),
                    // A game that ended fine clears the last crash.
                    None => hopper::crashdetect::clear(&root),
                }
                if let (Some(c), Some(i)) = (&c, self.instances.iter().find(|i| i.root == root))
                    && told.as_ref() != Some(&c.source)
                {
                    let top = self.top_suspect(i, c);
                    cx.emit(StoreEvent::GameCrashed(i.name.clone(), top, false));
                }
                self.crashes.insert(root, c);
                cx.notify();
            }
            Msg::Categories(found) => {
                for k in found.keys() {
                    self.cats_pending.remove(k);
                }
                self.cats.get_or_insert_default().extend(found);
            }
            Msg::Image { url, path } => {
                let ok = path.is_some();
                self.images
                    .insert(url.clone(), path.map_or(Image::Failed, Image::Ready));
                for key in [view_key(&url), url] {
                    if matches!(self.anims.get(&key), Some(Anim::Waiting)) {
                        if ok {
                            self.decode_anim(key);
                        } else {
                            self.anims.insert(key, Anim::Still);
                        }
                    }
                }
            }
            Msg::Anim { key, clip } => {
                // Forgotten while it decoded (the viewer moved on).
                if !matches!(self.anims.get(&key), Some(Anim::Decoding)) {
                    for f in clip.iter().flat_map(|c| c.frames()) {
                        cx.drop_image(f, None);
                    }
                    return;
                }
                let Some(clip) = clip else {
                    self.anims.insert(key, Anim::Still);
                    return;
                };
                self.anims.insert(key.clone(), Anim::Ready(clip));
                self.anims_ready.push_back(key);
                while self.anims_ready.len() > ANIMS_KEPT {
                    let Some(old) = self.anims_ready.pop_front() else { break };
                    if let Some(Anim::Ready(c)) = self.anims.remove(&old) {
                        for f in c.frames() {
                            cx.drop_image(f, None);
                        }
                    }
                }
            }
            Msg::Updates { root, result, prev } => {
                let result = result.map(|mut u| {
                    // CurseForge answers from cached copies of a project, and
                    // a stale copy hides a fresh file: what an earlier check
                    // (or `note_update`) found stays listed until installed,
                    // and the newer of two offers wins.
                    if let Some(inst) = self.instance(&root) {
                        for o in &prev {
                            let current = inst
                                .addons
                                .iter()
                                .any(|a| a.project_id == o.project_id && !a.locked && a.file_id < o.file.id);
                            if !current {
                                continue;
                            }
                            match u.iter_mut().find(|n| n.project_id == o.project_id) {
                                Some(n) if n.file.id < o.file.id => *n = o.clone(),
                                Some(_) => {}
                                None => u.push(o.clone()),
                            }
                        }
                    }
                    u
                });
                // What each update would stop loading, read from the other
                // mods' own requirements in the background.
                if let (Ok(u), Some(inst)) = (&result, self.instance(&root)) {
                    let (dir, r2) = (inst.mods_dir(), root.clone());
                    let list: Vec<(u64, String, String, String)> = u
                        .iter()
                        .map(|u| {
                            let (from, to) = u.versions();
                            (u.project_id, u.from.clone(), from, to)
                        })
                        .collect();
                    self.spawn(move || {
                        let map = list
                            .into_iter()
                            .filter_map(|(pid, file, from, to)| {
                                let b = hopper::deps::broken_by(&dir, &file, &from, &to);
                                (!b.is_empty()).then_some((pid, b))
                            })
                            .collect();
                        Msg::Breaks(r2, map)
                    });
                }
                self.updates.insert(
                    root,
                    match result {
                        Ok(u) => Updates::Ready(u),
                        Err(e) => Updates::Failed(e),
                    },
                );
            }
            Msg::Breaks(root, map) => {
                self.breaks.insert(root, map);
            }
            Msg::Status { key, status } => {
                self.statuses.insert(key, status);
            }
            Msg::ServersChanged => {
                self.discover_servers();
                self.server_rev += 1;
                self.settle_server_tools(cx);
            }
            Msg::Javas(j) => self.javas = Some(j),
            Msg::SelfUpdate(u) => self.self_update = Some(u),
            // A manual check or an install may have started meanwhile.
            Msg::SelfUpdateRecheck(u) => match (&self.self_update, &u) {
                (None | Some(SelfUpdate::UpToDate | SelfUpdate::Failed(_)), _) => self.self_update = Some(u),
                // A newer release than the one already offered.
                (Some(SelfUpdate::Available(old)), SelfUpdate::Available(new))
                    if hopper::selfupdate::is_newer(&new.version, &old.version) =>
                {
                    self.self_update = Some(u)
                }
                _ => {}
            },
            Msg::PackUpdates(u) => self.pack_updates = u.into_iter().collect(),
            Msg::Translated => self.tr_tick.update(cx, |_, cx| cx.notify()),
            Msg::TrDownloaded(code) => {
                self.translations.jobs.remove(code);
                if let (Some(texts), Some(lang)) = (self.translations.after.remove(code), hopper::translate::by_code(code)) {
                    self.translations.request(texts, lang);
                }
                self.tr_tick.update(cx, |_, cx| cx.notify());
            }
            Msg::PackPage(id, r) => {
                let page = match r {
                    Ok((m, md, images)) => {
                        if let Some(u) = m.icon() {
                            self.want_image(&u.to_string());
                        }
                        // The description's GIFs play (see `widgets::DescView`).
                        for u in &images {
                            self.want_anim(u);
                        }
                        PackPage::Ready(m, md, images)
                    }
                    Err(e) => PackPage::Failed(e),
                };
                self.pack_pages.insert(id, page);
            }
            Msg::RecheckPacks => self.check_pack_updates(),
            Msg::Forget(root) => self.forget(&root),
            Msg::RigServers(rig) => {
                self.folder_servers = rig;
                self.merge_servers(cx);
            }
            Msg::PanelServers(panel, list) => {
                if let Err(e) = &list {
                    log::warn!("Hopper Server {panel:?}: {e}");
                }
                self.panel_servers.insert(panel, list);
                self.merge_servers(cx);
            }
            Msg::SessionSaved(root) => cx.emit(StoreEvent::SessionSaved(root)),
            Msg::Nothing => {}
        }
    }

    pub fn refresh(&mut self) {
        // Images that failed (a network blip) get another try.
        self.images.retain(|_, v| !matches!(v, Image::Failed));
        self.instances = instances::discover(Path::new(&self.settings.instances_root));
        // Other launchers' instances, managed where they are.
        let main: Vec<PathBuf> = self.instances.iter().map(|i| i.root.clone()).collect();
        let mut foreign = hopper::launchers::instances(&self.settings.launcher_dirs);
        foreign.retain(|f| !main.contains(&f.root));
        self.instances.extend(foreign);
        self.instances.sort_by_key(|i| i.name.to_lowercase());
        // A mod hunt an older Hopper left running: its mods go back on or
        // off as before it (the hunt was removed).
        for i in self.instances.iter_mut().filter(|i| hopper::bisect::path(&i.root).is_file()) {
            match hopper::bisect::restore_leftover(i) {
                Ok(n) => log::info!("{}: put back {} mods from an unfinished mod hunt", i.name, n.unwrap_or(0)),
                Err(e) => log::warn!("{}: putting back the mod hunt's mods: {e:#}", i.name),
            }
        }
        self.pending.clear();
        for i in self.instances.clone() {
            self.read_pending(&i);
        }
        self.sync_watched();
        self.discover_servers();
        self.check_pack_updates();
        // Loader versions for the Minecraft versions in use, ready before
        // anyone opens a Loader dropdown.
        let mut mcs: Vec<String> = self.instances.iter().map(|i| i.game_version.clone()).collect();
        mcs.sort();
        mcs.dedup();
        mcs.retain(|m| !m.is_empty());
        std::thread::spawn(move || {
            for mc in mcs {
                let _ = hopper::cf::loader_versions(&mc);
            }
        });
        // Mod updates for every instance, so the list can show counts.
        self.updates.clear();
        for root in self.instances.iter().map(|i| i.root.clone()).collect::<Vec<_>>() {
            self.check_updates(&root, false);
        }
        let icons: Vec<String> = self
            .instances
            .iter()
            .filter_map(|i| i.icon.clone())
            .collect();
        for url in icons {
            self.want_image(&url);
        }
    }

    fn sync_watched(&self) {
        if let Ok(mut w) = self.watched.lock() {
            *w = self.instances.iter().map(|i| i.root.clone()).collect();
        }
    }

    /// Hopper's Microsoft accounts, as last read.
    pub fn accounts(&self) -> &hopper::accounts::Accounts {
        &self.accounts
    }

    /// Whether the old single sign-in waits to be copied into the list.
    pub fn legacy_login(&self) -> bool {
        self.legacy_login
    }

    /// Reads the account list again (after a sign-in, switch or change) and
    /// fetches the skins for their heads.
    pub fn reload_accounts(&mut self) {
        self.accounts = if crate::accounts::mock() {
            crate::accounts::mock_accounts()
        } else {
            hopper::accounts::Accounts::load()
        };
        self.legacy_login = !crate::accounts::mock() && hopper::accounts::legacy_pending();
        for a in self.accounts.list.clone() {
            match &a.skin_url {
                Some(u) => self.want_image(u),
                None => self.want_image(&a.fallback_avatar()),
            }
        }
    }

    /// Changes the account list (the mock list in a mock run).
    pub fn set_accounts(&mut self, a: hopper::accounts::Accounts, cx: &mut Context<Self>) {
        self.accounts = a;
        for a in self.accounts.list.clone() {
            if let Some(u) = &a.skin_url {
                self.want_image(u);
            }
        }
        let name = self.accounts.active().map(|a| a.name.clone());
        if !crate::accounts::mock() && name.is_some() {
            self.update_settings(|s| s.signed_in_as = name, cx);
        }
        cx.notify();
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Changes settings and writes them when they differ from the saved copy.
    pub fn update_settings(&mut self, edit: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        edit(&mut self.settings);
        let now = serde_json::to_string(&self.settings).unwrap_or_default();
        if now != self.saved_settings {
            if let Err(e) = self.settings.save() {
                log::warn!("saving settings: {e:#}");
            }
            self.saved_settings = now;
        }
        cx.notify();
    }

    /// Drops an instance that no longer exists (deleted).
    pub fn forget(&mut self, root: &Path) {
        self.instances.retain(|i| i.root != root);
        self.updates.remove(root);
        self.sync_watched();
        // Its settings go with it.
        let key = root.to_string_lossy().to_string();
        self.update_settings_quietly(|st| {
            st.groups.remove(&key);
            st.datapack_dest.remove(&key);
            st.pairings.retain(|_, inst| *inst != key);
        });
    }

    /// Changes settings and saves them, without a redraw.
    fn update_settings_quietly(&mut self, edit: impl FnOnce(&mut Settings)) {
        edit(&mut self.settings);
        let now = serde_json::to_string(&self.settings).unwrap_or_default();
        if now != self.saved_settings {
            if let Err(e) = self.settings.save() {
                log::warn!("saving settings: {e:#}");
            }
            self.saved_settings = now;
        }
    }

    pub fn instances(&self) -> &[Instance] {
        &self.instances
    }

    pub fn instance(&self, root: &Path) -> Option<&Instance> {
        self.instances.iter().find(|i| i.root == root)
    }

    /// What keeps Hopper from restarting by itself now: running tasks.
    /// `None` when it's idle.
    pub fn busy_reason(&self) -> Option<String> {
        let tasks = self.jobs.snapshot().iter().filter(|j| !j.done).count();
        if tasks > 0 {
            return Some(format!("{tasks} task{} running", if tasks == 1 { "" } else { "s" }));
        }
        // Games and servers hosted here aren't in this list: they run on
        // their own and a restarted Hopper finds them again.
        None
    }

    /// The instance's last game crash, until a game ends fine.
    pub fn crash(&mut self, root: &Path) -> Option<&hopper::crashdetect::Crash> {
        if !self.crashes.contains_key(root) {
            let saved = hopper::crashdetect::load(root);
            // A newer report than the saved one: the game crashed again
            // while Hopper wasn't watching. Read that one instead.
            let latest = hopper::logs::crash_reports(root).into_iter().next();
            let newer = match (&saved, &latest) {
                (Some(c), Some(l)) => hopper::crashdetect::is_newer(l, c),
                _ => false,
            };
            // Else read its report again in the background with today's
            // rules, so an older card doesn't keep an old guess.
            if let Some(c) = saved.clone().filter(|_| !newer)
                && let Some(i) = self.instance(root)
            {
                let (tx, wake, root, dir) = (self.tx.clone(), self.wake.clone(), root.to_path_buf(), i.mods_dir());
                std::thread::spawn(move || {
                    let mods: Vec<String> = std::fs::read_dir(&dir)
                        .into_iter()
                        .flatten()
                        .flatten()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .filter(|n| n.ends_with(".jar") || n.ends_with(".jar.disabled"))
                        .collect();
                    if let Some(fresh) =
                        hopper::crashdetect::reread(&root, &c, &mods, |f| hopper::deps::own_ids(&dir.join(f)).0)
                    {
                        let _ = tx.send(Msg::CrashReread(root, fresh));
                        wake();
                    }
                });
            }
            self.crashes.insert(root.to_path_buf(), saved);
            if let Some(l) = latest.filter(|_| newer) {
                self.newest_crash_report(root, &l);
            }
        }
        self.crashes.get(root).and_then(Option::as_ref)
    }

    /// The instance's newest crash report is `latest`: when it is newer than
    /// the saved crash (the game crashed again outside Hopper's watch), read
    /// it in the background so the card shows that crash, not the old one.
    pub fn newest_crash_report(&mut self, root: &Path, latest: &Path) {
        let Some(saved) = self.crash(root).cloned() else { return };
        if !hopper::crashdetect::is_newer(latest, &saved) {
            return;
        }
        let Some(i) = self.instance(root) else { return };
        let (tx, wake, root, dir, latest) = (self.tx.clone(), self.wake.clone(), root.to_path_buf(), i.mods_dir(), latest.to_path_buf());
        std::thread::spawn(move || {
            let mods: Vec<String> = std::fs::read_dir(&dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".jar") || n.ends_with(".jar.disabled"))
                .collect();
            if let Some(c) = hopper::crashdetect::from_report(&root, &latest, &mods, |f| hopper::deps::own_ids(&dir.join(f)).0) {
                let _ = tx.send(Msg::CrashReread(root, c));
                wake();
            }
        });
    }

    /// The name of the mod a crash suspects most.
    fn top_suspect(&self, i: &Instance, c: &hopper::crashdetect::Crash) -> Option<String> {
        c.suspects.first().map(|s| {
            i.addons
                .iter()
                .find(|a| a.file_name.eq_ignore_ascii_case(s.file_name.trim_end_matches(".disabled")))
                .map(|a| a.name.clone())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| hopper::crashdetect::jar_name(&i.mods_dir(), &s.file_name))
        })
    }

    /// Mods whose mixins failed in the instance's current or last game.
    pub fn warnings(&self, root: &Path) -> &[hopper::crashdetect::Suspect] {
        self.warnings.get(root).map_or(&[], Vec::as_slice)
    }

    /// Turns off the crash's Certain and Likely suspects that are on, and
    /// remembers them on the crash to turn back on. The files turned off.
    pub fn crash_turn_off(&mut self, root: &Path) -> Result<Vec<String>, String> {
        use hopper::crashdetect::Certainty;
        let Some(mut c) = self.crash(root).cloned() else { return Ok(Vec::new()) };
        let inst = self.instance(root).ok_or("That instance is gone.")?;
        let files: Vec<String> = c
            .suspects
            .iter()
            .filter(|s| s.certainty <= Certainty::Likely)
            .filter_map(|s| {
                inst.addons
                    .iter()
                    .find(|a| a.enabled && a.file_name.eq_ignore_ascii_case(s.file_name.trim_end_matches(".disabled")))
                    .map(|a| a.file_name.clone())
            })
            .collect();
        self.set_enabled(root, &files, false)?;
        for f in &files {
            if !c.turned_off.contains(f) {
                c.turned_off.push(f.clone());
            }
        }
        hopper::crashdetect::save(root, &c);
        self.crashes.insert(root.to_path_buf(), Some(c));
        Ok(files)
    }

    /// Turns back on what [`crash_turn_off`] turned off.
    pub fn crash_turn_back_on(&mut self, root: &Path) -> Result<(), String> {
        let Some(mut c) = self.crash(root).cloned() else { return Ok(()) };
        let files = std::mem::take(&mut c.turned_off);
        self.set_enabled(root, &files, true)?;
        hopper::crashdetect::save(root, &c);
        self.crashes.insert(root.to_path_buf(), Some(c));
        Ok(())
    }

    /// Forgets the instance's last crash (the player dismissed it).
    pub fn dismiss_crash(&mut self, root: &Path) {
        hopper::crashdetect::clear(root);
        self.crashes.insert(root.to_path_buf(), None);
    }

    pub fn running(&self, root: &Path) -> Option<&Running> {
        self.running.get(root)
    }

    pub fn jobs(&self) -> &Jobs {
        &self.jobs
    }

    /// See [`Store::job_ticks`](Self#structfield.job_ticks): observe it to
    /// follow running jobs' progress.
    pub fn job_ticks(&self) -> &Entity<JobTicks> {
        &self.job_ticks
    }

    /// The local file for an image URL once it is cached.
    pub fn image(&self, url: &str) -> Option<&Path> {
        match self.images.get(url) {
            Some(Image::Ready(p)) => Some(p),
            _ => None,
        }
    }

    /// A project's categories, once known.
    pub fn categories(&self, a: &instances::Addon) -> &[String] {
        hopper::modmeta::key(a)
            .and_then(|k| self.cats.as_ref()?.get(&k))
            .map_or(&[], Vec::as_slice)
    }

    /// Looks up the categories of these addons that aren't known yet.
    pub fn want_categories<'a>(&mut self, addons: impl IntoIterator<Item = &'a instances::Addon>) {
        let known = self.cats.get_or_insert_with(hopper::modmeta::load);
        let mut keys: Vec<String> = addons
            .into_iter()
            .filter_map(hopper::modmeta::key)
            .filter(|k| !known.contains_key(k) && !self.cats_pending.contains(k))
            .collect();
        keys.sort_unstable();
        keys.dedup();
        if keys.is_empty() {
            return;
        }
        self.cats_pending.extend(keys.iter().cloned());
        self.spawn(move || {
            let found = hopper::modmeta::fetch(&keys);
            hopper::modmeta::save(&found);
            Msg::Categories(found)
        });
    }

    /// The playable frames of image key `key`, once decoded (see
    /// [`Self::want_anim`]).
    pub fn anim(&self, key: &str) -> Option<&Arc<crate::anim::Clip>> {
        match self.anims.get(key) {
            Some(Anim::Ready(c)) => Some(c),
            _ => None,
        }
    }

    /// Frames streamed clips have moved past, whose GPU copies can go
    /// (`window.drop_image`).
    pub fn take_shown_frames(&self) -> Vec<Arc<gpui_kit::RenderImage>> {
        self.anims
            .values()
            .filter_map(|a| match a {
                Anim::Ready(c) => Some(c.take_shown()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    /// Like [`Self::want_image`], and if it's an animated GIF or WebP also
    /// decodes its frames to play (others show a still, see the workers).
    pub fn want_anim(&mut self, key: &str) {
        if key.is_empty() || self.anims.contains_key(key) {
            return;
        }
        let base = hopper::imgcache::split_width(key).0;
        let animated = hopper::imgcache::path_for(base)
            .is_some_and(|p| hopper::imgcache::maybe_animated(&p));
        if !animated {
            self.anims.insert(key.to_string(), Anim::Still);
            return self.want_image(key);
        }
        self.anims.insert(key.to_string(), Anim::Waiting);
        match self.images.get(key) {
            Some(Image::Ready(_)) => self.decode_anim(key.to_string()),
            Some(Image::Failed) => {
                self.anims.insert(key.to_string(), Anim::Still);
            }
            _ => self.want_image(key),
        }
    }

    /// Like [`Self::want_anim`] for the image viewer: image `url` decoded
    /// big, as [`view_key`]. Let it go with [`Self::forget_anim`] once the
    /// viewer moves on.
    pub fn want_anim_view(&mut self, url: &str) {
        let key = view_key(url);
        if url.is_empty() || self.anims.contains_key(&key) {
            return;
        }
        let animated = hopper::imgcache::path_for(url).is_some_and(|p| hopper::imgcache::maybe_animated(&p));
        if !animated {
            self.anims.insert(key, Anim::Still);
            return self.want_image(url);
        }
        self.anims.insert(key.clone(), Anim::Waiting);
        match self.images.get(url) {
            Some(Image::Ready(_)) => self.decode_anim(key),
            Some(Image::Failed) => {
                self.anims.insert(key, Anim::Still);
            }
            _ => self.want_image(url),
        }
    }

    /// Drops clip `key` and frees its GPU copies.
    pub fn forget_anim(&mut self, key: &str, cx: &mut App) {
        self.anims_ready.retain(|k| k != key);
        if let Some(Anim::Ready(c)) = self.anims.remove(key) {
            for f in c.frames() {
                cx.drop_image(f, None);
            }
        }
    }

    /// Decodes the cached original of `key` on a thread.
    fn decode_anim(&mut self, key: String) {
        self.anims.insert(key.clone(), Anim::Decoding);
        self.spawn(move || {
            let (base, width, limits) = anim_parts(&key);
            let clip = hopper::imgcache::cached(base)
                .ok_or_else(|| anyhow::anyhow!("not cached"))
                .and_then(|p| crate::anim::decode(&p, width, limits))
                .inspect_err(|e| log::debug!("animating {key}: {e:#}"))
                .ok()
                .flatten()
                .map(Arc::new);
            Msg::Anim { key, clip }
        });
    }

    /// Starts caching `url` unless it is cached or on its way.
    pub fn want_image(&mut self, url: &str) {
        if url.is_empty() || self.images.contains_key(url) {
            return;
        }
        let still = hopper::imgcache::cached(url).filter(|p| !hopper::imgcache::maybe_animated(p));
        if let Some(p) = still {
            self.images.insert(url.to_string(), Image::Ready(p));
            return;
        }
        self.images.insert(url.to_string(), Image::Loading);
        let (queue, ready) = &*self.image_queue;
        queue.lock().unwrap_or_else(|e| e.into_inner()).push_back(url.to_string());
        ready.notify_one();
    }

    /// A few threads fetch images from a queue, rather than a thread (and a
    /// connection) per image when a big pack opens.
    fn start_image_workers(&self) {
        for _ in 0..6 {
            let (q, tx, wake) = (self.image_queue.clone(), self.tx.clone(), self.wake.clone());
            std::thread::spawn(move || {
                let (queue, ready) = &*q;
                loop {
                    let url = {
                        let mut list = queue.lock().unwrap_or_else(|e| e.into_inner());
                        loop {
                            if let Some(u) = list.pop_front() {
                                break u;
                            }
                            list = ready.wait(list).unwrap_or_else(|e| e.into_inner());
                        }
                    };
                    // Animated icons show their first frame: GPUI redraws the
                    // whole window for every GIF frame on screen. A width
                    // in the key (`#w=156`) scales the copy down to it.
                    let (base, width) = hopper::imgcache::split_width(&url);
                    let path = hopper::imgcache::fetch(base)
                        .and_then(|p| hopper::imgcache::still(&p).or(Ok(p)))
                        .and_then(|p| match width {
                            Some(w) => hopper::imgcache::resized(&p, w),
                            None => Ok(p),
                        })
                        .inspect_err(|e| log::debug!("image {url}: {e:#}"))
                        .ok();
                    if tx.send(Msg::Image { url, path }).is_err() {
                        return;
                    }
                    wake();
                }
            });
        }
    }

    /// Runs `work` on a thread and handles the message it returns.
    pub fn spawn(&self, work: impl FnOnce() -> Msg + Send + 'static) {
        let (tx, wake) = (self.tx.clone(), self.wake.clone());
        std::thread::spawn(move || {
            let _ = tx.send(work());
            wake();
        });
    }

    /// Runs `work` with a progress job, then handles the message it returns.
    pub fn spawn_job(
        &self,
        title: impl Into<String>,
        work: impl FnOnce(&Job) -> anyhow::Result<Msg> + Send + 'static,
    ) {
        let _ = self.run_job(None, title.into(), work);
    }

    /// [`spawn_job`] for work that changes an instance: it counts as busy
    /// until the job ends, so the UI holds off other changes to it.
    pub fn spawn_instance_job(
        &self,
        root: &Path,
        title: impl Into<String>,
        work: impl FnOnce(&Job) -> anyhow::Result<Msg> + Send + 'static,
    ) {
        let _ = self.run_job(Some(root.to_path_buf()), title.into(), work);
    }

    /// [`spawn_job`], returning the job's id (to follow its progress).
    pub fn spawn_job_id(
        &self,
        title: impl Into<String>,
        work: impl FnOnce(&Job) -> anyhow::Result<Msg> + Send + 'static,
    ) -> u64 {
        self.run_job(None, title.into(), work)
    }

    /// [`spawn_instance_job`], returning the job's id.
    pub fn spawn_instance_job_id(
        &self,
        root: &Path,
        title: impl Into<String>,
        work: impl FnOnce(&Job) -> anyhow::Result<Msg> + Send + 'static,
    ) -> u64 {
        self.run_job(Some(root.to_path_buf()), title.into(), work)
    }

    /// A progress job the caller runs and finishes itself (shows in Tasks).
    pub fn start_job(&self, title: impl Into<String>) -> Job {
        self.jobs.start(title)
    }

    /// A job's progress (0..=1, when known) and whether it ended (and how).
    pub fn job_state(&self, id: u64) -> Option<hopper::jobs::JobState> {
        self.jobs.snapshot().into_iter().find(|j| j.id == id)
    }

    fn run_job(
        &self,
        root: Option<PathBuf>,
        title: String,
        work: impl FnOnce(&Job) -> anyhow::Result<Msg> + Send + 'static,
    ) -> u64 {
        let job = self.jobs.start(title);
        let id = job.id();
        if let Some(r) = &root {
            *self.busy.lock().unwrap_or_else(|e| e.into_inner()).entry(r.clone()).or_default() += 1;
        }
        let (tx, wake, busy) = (self.tx.clone(), self.wake.clone(), self.busy.clone());
        std::thread::spawn(move || {
            // A bug in the work must still end the job, not leave it spinning.
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&job)))
                .unwrap_or_else(|p| {
                    let why = p
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| p.downcast_ref::<String>().cloned())
                        .unwrap_or_default();
                    Err(anyhow::anyhow!("Hopper hit a bug and stopped this task. {why}"))
                });
            job.finish(&r);
            if let Some(r) = &root {
                let mut b = busy.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(n) = b.get_mut(r) {
                    *n = n.saturating_sub(1);
                    if *n == 0 {
                        b.remove(r);
                    }
                }
            }
            if let Ok(msg) = r {
                let _ = tx.send(msg);
            }
            wake();
        });
        id
    }

    fn read_pending(&mut self, inst: &Instance) {
        let p = hopper::modpack::pending_downloads(inst);
        if p.is_empty() {
            self.pending.remove(&inst.root);
        } else {
            self.pending.insert(inst.root.clone(), p);
        }
    }

    /// Stops waiting for an instance's files to download by hand.
    pub fn forget_pending(&mut self, root: &Path) {
        if let Some(inst) = self.instance(root).cloned() {
            let _ = hopper::modpack::clear_pending(&inst);
        }
        self.pending.remove(root);
    }

    /// Pack files still to download by hand.
    pub fn pending(&self, root: &Path) -> &[hopper::modpack::Pending] {
        self.pending.get(root).map_or(&[], Vec::as_slice)
    }

    /// Runs the installed pack version's update again: picks up files the
    /// user downloaded by hand into Downloads.
    pub fn finish_pack(&self, root: &Path) {
        let Some(mut inst) = self.instance(root).cloned() else { return };
        let Some(pack) = inst.pack.clone() else { return };
        self.spawn_instance_job(root, format!("Finishing {}", inst.name), move |job| {
            let file = hopper::cf::file(pack.project_id, pack.file_id)?;
            hopper::modpack::update_any(&mut inst, pack.project_id, &file, None, job)?;
            let left = hopper::modpack::pending_downloads(&inst).len();
            job.detail(if left == 0 {
                "Every file is in place".to_string()
            } else {
                format!("{left} still to download")
            });
            // Website-only files: the download window gets them.
            let pages = hopper::modpack::pending_urls_at(&inst.root);
            Ok(Msg::Updated(inst.root.clone(), Vec::new(), pages))
        });
    }

    /// Whether a job is changing this instance.
    /// Whether a Play on this instance is still starting the game.
    pub fn is_launching(&self, root: &Path) -> bool {
        self.launching.lock().unwrap_or_else(|e| e.into_inner()).contains(root)
    }

    /// Starts a game: a quiet job (not in the task list unless it fails),
    /// with the instance marked as launching until it ends.
    fn spawn_launch(
        &self,
        root: PathBuf,
        title: String,
        work: impl FnOnce(&Job) -> anyhow::Result<Msg> + Send + 'static,
    ) {
        let job = self.jobs.start_quiet(title);
        self.launching.lock().unwrap_or_else(|e| e.into_inner()).insert(root.clone());
        let (tx, wake, launching) = (self.tx.clone(), self.wake.clone(), self.launching.clone());
        std::thread::spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&job)))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("Hopper hit a bug while starting the game.")));
            job.finish(&r);
            launching.lock().unwrap_or_else(|e| e.into_inner()).remove(&root);
            if let Ok(msg) = r {
                let _ = tx.send(msg);
            }
            wake();
        });
    }

    pub fn is_busy(&self, root: &Path) -> bool {
        self.busy.lock().unwrap_or_else(|e| e.into_inner()).contains_key(root)
    }

    /// Why mods, packs and shaders can't be added, updated or removed right
    /// now, if they can't. A running game doesn't stop that (as in
    /// CurseForge): the changes apply on its next start.
    pub fn edit_hold_reason(&self, root: &Path) -> Option<&'static str> {
        self.is_busy(root).then_some("Another task is changing this instance")
    }

    /// Why the instance can't be changed right now, if it can't.
    pub fn hold_reason(&self, root: &Path) -> Option<&'static str> {
        if self.running(root).is_some() {
            Some("Close the game first")
        } else if self.is_busy(root) {
            Some("Another task is changing this instance")
        } else {
            None
        }
    }

    fn launch_defaults(&self) -> hopper::launch::Defaults {
        hopper::launch::Defaults {
            memory_mb: self.settings.default_memory_mb,
            java_args: self.settings.default_java_args.clone(),
            java_path: self.settings.default_java_path.clone(),
            gc_log: self.settings.record_sessions,
        }
    }

    /// Records how a game that just started runs, until it exits.
    fn record_session(&self, root: &Path, r: &Running) {
        let Some(base) = hopper::perf::sessions_dir() else { return };
        let Some(inst) = self.instances.iter().find(|i| i.root == root) else { return };
        let d = self.launch_defaults();
        let mem = if inst.settings.memory_mb > 0 { inst.settings.memory_mb } else { d.memory_mb };
        let xms = hopper::launch::initial_memory(&inst.settings.java_args, &d.java_args, mem);
        let custom = hopper::launch::custom_args(&inst.settings.java_args, &d.java_args);
        let start = hopper::perf::recorder::Start {
            pid: r.pid,
            started: r.started,
            instance: inst.name.clone(),
            root: inst.root.clone(),
            pack: inst
                .pack
                .as_ref()
                .map(|p| format!("{} {}", p.name, p.version).trim().to_string())
                .unwrap_or_default(),
            game_version: inst.game_version.clone(),
            loader: inst.loader.clone(),
            configured_args: format!("-Xms{xms}m -Xmx{mem}m {custom}"),
            overlay: false,
        };
        let (tx, wake) = (self.tx.clone(), self.wake.clone());
        hopper::perf::recorder::record(start, base, move |s| {
            let _ = tx.send(Msg::SessionSaved(PathBuf::from(s.instance_root)));
            wake();
        });
    }

    /// Starts the instance through Hopper with the saved Microsoft sign-in,
    /// or through Mojang's launcher. `Err` explains why it could not start.
    pub fn play(&mut self, root: &Path) -> Result<(), String> {
        // Old jars a previous game held open, so the new game doesn't load
        // two versions of a mod.
        hopper::locked::flush(root);
        let Some(mut full) = self.instance(root).cloned() else {
            return Err("That instance is gone.".into());
        };
        let dir = PathBuf::from(&self.settings.launcher_dir);
        let defaults = self.launch_defaults();
        if self.settings.launch_native {
            if !hopper::msauth::has_saved_login() {
                return Err("Sign in with Microsoft in Settings first.".into());
            }
            self.spawn_launch(root.to_path_buf(), format!("Starting {}", full.name), move |job| {
                job.detail("Signing in");
                let session = hopper::msauth::session_from_saved()?;
                let player = hopper::launch_native::Player::from_session(&session);
                let child = hopper::launch_native::launch(&full, &dir, &defaults, &player, job)?;
                job.detail(format!("Minecraft is starting (process {})", child.id()));
                full.last_played = Some(hopper::time::now_iso());
                full.save()?;
                Ok(Msg::InstanceChanged(full.root.clone()))
            });
        } else {
            self.spawn_launch(root.to_path_buf(), format!("Launching {}", full.name), move |job| {
                hopper::launch::launch(&full, &dir, &defaults, job)?;
                full.last_played = Some(hopper::time::now_iso());
                full.save()?;
                Ok(Msg::InstanceChanged(full.root.clone()))
            });
        }
        Ok(())
    }

    /// Installs the instance's loader (and, launching through Hopper, the
    /// libraries and game files) now, so Play doesn't wait on it. Called
    /// when its loader or Minecraft version changes, as CurseForge does.
    pub fn install_loader(&self, inst: hopper::instances::Instance) {
        if inst.loader.is_empty() {
            return;
        }
        let dir = PathBuf::from(&self.settings.launcher_dir);
        let defaults = self.launch_defaults();
        let native = self.settings.launch_native;
        let title = format!("Installing {} for {}", loader_label(&inst.loader), inst.name);
        let root = inst.root.clone();
        self.spawn_instance_job(&root, title, move |job| {
            prepare_loader(&inst, &dir, &defaults, native, job)?;
            let mut inst = inst;
            sync_cf_loader(&mut inst, job);
            job.detail("Ready to play");
            Ok(Msg::InstanceChanged(inst.root.clone()))
        });
    }

    /// Looks for newer versions of every installed modpack.
    pub fn check_pack_updates(&self) {
        if hopper::cf::api_key().is_none() {
            return;
        }
        let packs: Vec<_> = self
            .instances
            .iter()
            .filter_map(|i| {
                i.pack
                    .clone()
                    .map(|p| (p, i.game_version.clone(), i.settings.release_channel))
            })
            .collect();
        self.spawn(move || match hopper::instance_ops::pack_updates(&packs) {
            Ok(u) => Msg::PackUpdates(u),
            Err(e) => {
                log::warn!("pack updates: {e:#}");
                Msg::Nothing
            }
        });
    }

    /// The page of the modpack with project `id`, fetched once per run from
    /// whichever site it came from.
    pub fn pack_page(&self, id: u64) -> Option<&PackPage> {
        self.pack_pages.get(&id)
    }

    pub fn want_pack_page(&mut self, id: u64) {
        if id == 0 || self.pack_pages.contains_key(&id) {
            return;
        }
        self.pack_pages.insert(id, PackPage::Loading);
        self.spawn(move || {
            let r = (|| {
                let m = hopper::source::get_mod(id)?;
                let desc = hopper::source::description(id)?;
                let images = hopper::richtext::images(&desc);
                anyhow::Ok((m, hopper::richtext::to_segments(&desc), images))
            })();
            Msg::PackPage(id, r.map_err(|e| format!("{e:#}")))
        });
    }

    /// A newer file of the instance's modpack, if there is one.
    pub fn pack_update(&self, inst: &Instance) -> Option<&hopper::cf::File> {
        self.pack_updates.get(&inst.pack.as_ref()?.project_id)
    }

    /// Moves an instance to another version of its modpack with the given
    /// override decisions (the safe merge when `None`), then (when enabled)
    /// brings its paired server along while it is offline.
    /// Updates the instance's pack to `file`, backing up every world first
    /// when `backup_worlds`.
    pub fn update_pack(
        &mut self,
        root: &Path,
        file: hopper::cf::File,
        overrides: Option<hopper::modpack::Overrides>,
        backup_worlds: bool,
    ) {
        let Some(mut full) = self.instance(root).cloned() else {
            return;
        };
        let Some(pid) = full.pack.as_ref().map(|p| p.project_id) else {
            return;
        };
        self.pack_updates.remove(&pid);
        let server = self
            .server_of(root)
            .cloned()
            .filter(|_| self.settings.auto_update_servers);
        let host = self.settings.server_host.clone();
        let anyway = self.settings.sync_anyway.clone();
        let java = hopper::launch::installer_java(Path::new(&self.settings.launcher_dir));
        let panel = match &server {
            Some(s) => hopper::panel::for_server(&self.settings, s),
            None => hopper::panel::from_settings(&self.settings),
        };
        // The panel knows a server is up even while it's still loading and
        // doesn't answer pings yet.
        let server_up = server
            .as_ref()
            .and_then(|s| self.panel_stats(&s.root))
            .is_some_and(|p| p.running || p.starting);
        let tx = self.tx.clone();
        let dir = PathBuf::from(&self.settings.launcher_dir);
        let defaults = self.launch_defaults();
        let native = self.settings.launch_native;
        self.spawn_instance_job(&full.root.clone(), format!("{} → {}", full.name, file.display_name), move |job| {
            job.announce();
            let loader_before = (full.loader.clone(), full.game_version.clone());
            if backup_worlds {
                for w in hopper::instance_ops::worlds(&full) {
                    job.detail(format!("Backing up {}", w.name));
                    let p = hopper::instance_ops::backup_world(&full, &w, job)?;
                    job.note(format!("{} backed up to {}", w.name, p.display()));
                }
            }
            if let Err(e) = hopper::modpack::update_any(&mut full, pid, &file, overrides.as_ref(), job) {
                // Offer the update again.
                let _ = tx.send(Msg::RecheckPacks);
                return Err(e);
            }
            // A new loader is installed now, not on the next Play.
            if (full.loader.clone(), full.game_version.clone()) != loader_before
                && !full.loader.is_empty()
                && let Err(e) = prepare_loader(&full, &dir, &defaults, native, job)
            {
                job.note(format!("{} will install on Play: {e:#}", loader_label(&full.loader)));
            }
            let cf_open = !sync_cf_pack(&mut full, job);
            if (full.loader.clone(), full.game_version.clone()) != loader_before && !cf_open {
                sync_cf_loader(&mut full, job);
            }
            if server_up && let Some(s) = &server {
                job.note(format!("{} is running, so it wasn't updated. Stop it, then use Server sync.", s.name));
            } else if let Some(server) = server {
                match hopper::server_update::update_server(&server, &full, &host, &anyway, &java, panel.as_deref(), job) {
                    Ok(r) => job.note(format!("{}: {}", server.name, r.summary())),
                    Err(e) => job.note(format!("{} not updated: {e:#}", server.name)),
                }
            }
            // Website-only files the pack wants: the download window gets
            // them, as for mod updates.
            let pages = hopper::modpack::pending_urls_at(&full.root);
            Ok(Msg::Updated(full.root.clone(), Vec::new(), pages))
        });
    }

    pub fn updates(&self, root: &Path) -> Option<&Updates> {
        self.updates.get(root)
    }

    /// No update check yet, or the last one is older than
    /// [`UPDATES_MAX_AGE`] (mods get new files during the day).
    pub fn updates_stale(&self, root: &Path) -> bool {
        !self.updates.contains_key(root)
            || self.updates_at.get(root).is_none_or(|t| t.elapsed() > UPDATES_MAX_AGE)
    }

    /// Looks for newer files of everything in the instance when the last
    /// look is stale (again anyway, with `force`).
    pub fn check_updates(&mut self, root: &Path, force: bool) {
        if hopper::cf::api_key().is_none() || (!force && !self.updates_stale(root)) {
            return;
        }
        let Some(inst) = self.instance(root).cloned() else {
            return;
        };
        self.updates_at.insert(root.to_path_buf(), Instant::now());
        let prev = match self.updates.get(root) {
            Some(Updates::Ready(v)) => v.clone(),
            _ => Vec::new(),
        };
        // A stale list stays up while the new one loads, so the update
        // buttons don't blink out.
        if force || !matches!(self.updates.get(root), Some(Updates::Ready(_))) {
            self.updates.insert(root.to_path_buf(), Updates::Checking);
        }
        let root = root.to_path_buf();
        self.spawn(move || Msg::Updates {
            result: install::check_updates(&inst).map_err(|e| format!("{e:#}")),
            root,
            prev,
        });
    }

    /// A newer file seen in the project's file list that the update list
    /// missed: CurseForge often answers the check from a stale cached copy
    /// of the project that lacks a fresh file. Joins the list so the mod
    /// shows as an update, and later checks keep it until it's installed.
    pub fn note_update(&mut self, root: &Path, files: &[hopper::cf::File], project_id: u64) -> bool {
        let Some(inst) = self.instance(root) else { return false };
        let Some(a) = inst.addons.iter().find(|a| a.project_id == project_id && !a.locked) else {
            return false;
        };
        let max = inst.settings.release_channel.max(a.release_type).max(1);
        let Some(f) = files
            .iter()
            .filter(|f| f.id > a.file_id && f.release_type <= max)
            .max_by_key(|f| f.id)
        else {
            return false;
        };
        let new = Update {
            project_id,
            name: a.name.clone(),
            from: a.file_name.clone(),
            file: f.clone(),
        };
        let Some(Updates::Ready(list)) = self.updates.get_mut(root) else {
            return false;
        };
        if list.iter().any(|u| u.project_id == project_id && u.file.id >= new.file.id) {
            return false;
        }
        list.retain(|u| u.project_id != project_id);
        list.push(new);
        true
    }

    pub fn run_updates(&self, root: &Path, updates: Vec<Update>) {
        // A locked mod stays on its version, whichever button asked.
        let Some(inst) = self.instance(root) else { return };
        let updates: Vec<Update> = updates
            .into_iter()
            .filter(|u| !inst.addons.iter().any(|a| a.project_id == u.project_id && a.locked))
            .collect();
        if updates.is_empty() {
            return;
        }
        let mut q = self.update_queue.lock().unwrap_or_else(|e| e.into_inner());
        // An update job is already on this instance: it takes these next.
        if let Some(e) = q.get_mut(root) {
            for u in updates {
                if !e.has(u.project_id) {
                    e.waiting.push(u);
                }
            }
            return;
        }
        let Some(mut inst) = self.instance(root).cloned() else {
            return;
        };
        let n = updates.len();
        q.insert(root.to_path_buf(), UpdateQueue { active: Vec::new(), waiting: updates, progress: HashMap::new() });
        drop(q);
        let queue = self.update_queue.clone();
        let recent = self.recent_updates.clone();
        let wake = self.wake.clone();
        self.spawn_instance_job(&inst.root.clone(), format!("Updating mods in {}", inst.name), move |job| {
            // A snapshot first, so Backups can put the whole set back.
            if n > 1
                && let Err(e) = hopper::snapshots::take_in(&inst.root, hopper::snapshots::Reason::BulkUpdate { count: n }, job)
            {
                log::warn!("snapshot: {e:#}");
            }
            let mut clear = ClearQueue(queue.clone(), Some(inst.root.clone()));
            let mut done: Vec<u64> = Vec::new();
            let mut pages: Vec<String> = Vec::new();
            let mods = inst.mods_dir();
            // Version needs before and after: warn about what the
            // update broke, not what the pack already had. Reading every
            // jar is slow on a big pack, so neither scan holds the
            // instance: "before" runs beside the downloads, "after" once
            // the job has let go.
            fn key(m: &hopper::deps::Missing) -> String {
                format!("{}|{}|{}", m.file_name, m.needs, m.versions)
            }
            let before = {
                let mods = mods.clone();
                std::thread::spawn(move || hopper::deps::missing(&mods).iter().map(key).collect::<std::collections::HashSet<String>>())
            };
            // Batch after batch until nothing more was queued meanwhile.
            loop {
                let batch = {
                    let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
                    let e = q.entry(inst.root.clone()).or_default();
                    if e.waiting.is_empty() {
                        q.remove(&inst.root);
                        clear.1 = None;
                        break;
                    }
                    e.active = e.waiting.iter().map(|u| u.project_id).collect();
                    std::mem::take(&mut e.waiting)
                };
                job.detail(format!("Downloading {} file{}", batch.len(), if batch.len() == 1 { "" } else { "s" }));
                // Each mod's progress for its Update button, and the
                // finished ones for the table to show as Updated.
                let by_id: HashMap<u64, Update> = batch.iter().map(|u| (u.project_id, u.clone())).collect();
                let (q2, r2, root2, wake2) = (queue.clone(), recent.clone(), inst.root.clone(), wake.clone());
                let on = move |pid: u64, p: Option<f32>| match p {
                    Some(f) => {
                        if let Some(e) = q2.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&root2) {
                            e.progress.insert(pid, f);
                        }
                    }
                    None => {
                        if let Some(u) = by_id.get(&pid) {
                            let mut r = r2.lock().unwrap_or_else(|e| e.into_inner());
                            let list = r.entry(root2.clone()).or_default();
                            list.retain(|x| x.project_id != pid);
                            list.push(u.clone());
                        }
                        wake2();
                    }
                };
                pages.extend(install::apply_updates_with(&mut inst, &batch, job, &on).map_err(install::explain_locked)?);
                done.extend(batch.iter().map(|u| u.project_id));
            }
            job.detail(format!("Updated {} file{}", done.len(), if done.len() == 1 { "" } else { "s" }));
            // Notes on a finished job still show in the task list.
            let job = job.clone();
            std::thread::spawn(move || {
                let before = before.join().unwrap_or_default();
                for m in hopper::deps::missing(&mods) {
                    if before.contains(&key(&m)) {
                        continue;
                    }
                    let warn = match &m.wrong_version {
                        Some((jar, have)) => format!(
                            "{} needs {} {} but {jar} is {have}. Update {} too, or the game may stop at loading.",
                            m.mod_name, m.needs, m.versions, m.needs
                        ),
                        None => format!("{} now needs {} {}, which isn't installed.", m.mod_name, m.needs, m.versions),
                    };
                    job.note(warn);
                }
            });
            Ok(Msg::Updated(inst.root.clone(), done, pages))
        });
    }

    /// Gets website-only files through Hopper's download window, one page
    /// after another; pages asked for while it's open go next. Without
    /// WebView2 the pages open in the browser instead.
    pub fn fetch_pages(&self, urls: Vec<String>) {
        let mut q = self.fetch_queue.lock().unwrap_or_else(|e| e.into_inner());
        for u in urls.into_iter().filter(|u| !u.is_empty()) {
            if !q.1.contains(&u) {
                q.1.push(u);
            }
        }
        if q.0 || q.1.is_empty() {
            return;
        }
        q.0 = true;
        drop(q);
        let (queue, tx, wake) = (self.fetch_queue.clone(), self.tx.clone(), self.wake.clone());
        std::thread::spawn(move || loop {
            let urls = {
                let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
                if q.1.is_empty() {
                    q.0 = false;
                    return;
                }
                std::mem::take(&mut q.1)
            };
            // The download window is WebView2 (Windows); elsewhere the browser
            // downloads them and the download watcher picks them up.
            let (true, Ok(exe), Some(dir)) = (cfg!(windows), std::env::current_exe(), hopper::modpack::fetch_dir()) else {
                let _ = tx.send(Msg::OpenUrls(urls));
                wake();
                continue;
            };
            let code = std::process::Command::new(exe).arg("--fetch").arg(&dir).args(&urls).status().map(|s| s.code());
            if !matches!(code, Ok(Some(0 | 1))) {
                log::warn!("download window: {code:?}");
                let _ = tx.send(Msg::OpenUrls(urls));
            }
            wake();
        });
    }

    /// Whether an update job on this instance takes more updates.
    pub fn queues_updates(&self, root: &Path) -> bool {
        self.update_queue.lock().unwrap_or_else(|e| e.into_inner()).contains_key(root)
    }

    /// `Some(true)` while this project's update downloads, `Some(false)`
    /// while it waits its turn.
    pub fn update_state(&self, root: &Path, project_id: u64) -> Option<bool> {
        let q = self.update_queue.lock().unwrap_or_else(|e| e.into_inner());
        let e = q.get(root)?;
        if e.active.contains(&project_id) {
            Some(true)
        } else {
            e.waiting.iter().any(|u| u.project_id == project_id).then_some(false)
        }
    }

    /// How far this project's update download is (0..=1), while it runs.
    pub fn update_progress(&self, root: &Path, project_id: u64) -> Option<f32> {
        let q = self.update_queue.lock().unwrap_or_else(|e| e.into_inner());
        q.get(root)?.progress.get(&project_id).copied()
    }

    /// Mods an update job finished on this instance, not yet let go.
    pub fn recent_updates(&self, root: &Path) -> Vec<Update> {
        let r = self.recent_updates.lock().unwrap_or_else(|e| e.into_inner());
        r.get(root).cloned().unwrap_or_default()
    }

    /// Lets go of finished updates the table no longer shows as Updated.
    pub fn forget_updates(&self, root: &Path, ids: &std::collections::HashSet<u64>) {
        let mut r = self.recent_updates.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(list) = r.get_mut(root) {
            list.retain(|u| !ids.contains(&u.project_id));
            if list.is_empty() {
                r.remove(root);
            }
        }
    }

    /// Switches files on or off (renames to and from `.disabled`).
    pub fn set_enabled(&mut self, root: &Path, files: &[String], on: bool) -> Result<(), String> {
        let inst = self
            .instances
            .iter_mut()
            .find(|i| i.root == root)
            .ok_or("That instance is gone.")?;
        for f in files {
            if let Some(i) = inst.addons.iter().position(|a| &a.file_name == f) {
                install::set_enabled(inst, i, on).map_err(|e| format!("{:#}", install::explain_locked(e)))?;
            }
        }
        Ok(())
    }

    /// Moves files to the instance's `.hopper-backups`.
    pub fn remove_files(&mut self, root: &Path, files: &[String]) -> Result<(), String> {
        let inst = self
            .instances
            .iter_mut()
            .find(|i| i.root == root)
            .ok_or("That instance is gone.")?;
        // The projects these files are, to drop from the update list.
        let ids: Vec<u64> = inst
            .addons
            .iter()
            .filter(|a| files.contains(&a.file_name))
            .map(|a| a.project_id)
            .collect();
        for f in files {
            match inst.addons.iter().position(|a| &a.file_name == f) {
                Some(i) => install::remove(inst, i),
                None => install::remove_loose_file(inst, f),
            }
            .map_err(|e| format!("{:#}", install::explain_locked(e)))?;
        }
        self.drop_updates(root, &ids);
        Ok(())
    }

    /// Deletes files (to the Recycle Bin) without keeping a backup.
    /// Deletes files to the Recycle Bin, or for good with `permanent`.
    pub fn delete_files(&mut self, root: &Path, files: &[String], permanent: bool) -> Result<(), String> {
        let inst = self
            .instances
            .iter_mut()
            .find(|i| i.root == root)
            .ok_or("That instance is gone.")?;
        // The projects these files are, to drop from the update list.
        let ids: Vec<u64> = inst
            .addons
            .iter()
            .filter(|a| files.contains(&a.file_name))
            .map(|a| a.project_id)
            .collect();
        for f in files {
            match (inst.addons.iter().position(|a| &a.file_name == f), permanent) {
                (Some(i), false) => install::delete(inst, i),
                (Some(i), true) => install::purge(inst, i),
                (None, false) => install::delete_loose_file(inst, f),
                (None, true) => install::purge_loose_file(inst, f),
            }
            .map_err(|e| format!("{e:#}"))?;
        }
        self.drop_updates(root, &ids);
        Ok(())
    }

    /// Remembers which mods are disabled now as a named profile.
    pub fn save_profile(&mut self, root: &Path, name: &str) -> Result<(), String> {
        let inst = self
            .instances
            .iter_mut()
            .find(|i| i.root == root)
            .ok_or("That instance is gone.")?;
        install::save_profile(inst, name).map_err(|e| format!("{e:#}"))
    }

    /// Forgets a saved profile (no mod changes).
    pub fn delete_profile(&mut self, root: &Path, name: &str) -> Result<(), String> {
        let inst = self
            .instances
            .iter_mut()
            .find(|i| i.root == root)
            .ok_or("That instance is gone.")?;
        install::delete_profile(inst, name).map(|_| ()).map_err(|e| format!("{e:#}"))
    }

    /// The mods `project`'s offered update would stop loading: (mod, the
    /// version range it needs). Empty when none, or not checked yet.
    pub fn update_breaks(&self, root: &Path, project: u64) -> &[(String, String)] {
        self.breaks.get(root).and_then(|m| m.get(&project)).map_or(&[], Vec::as_slice)
    }

    /// Locks one update's file out (`Some(file name)`) or lifts it (`None`):
    /// the update stays listed but is only installed on purpose.
    pub fn set_version_lock(&mut self, root: &Path, file: &str, version: Option<String>) -> Result<(), String> {
        let inst = self
            .instances
            .iter_mut()
            .find(|i| i.root == root)
            .ok_or("That instance is gone.")?;
        let a = inst.addons.iter_mut().find(|a| a.file_name == file).ok_or("That mod is gone.")?;
        a.locked_version = version.unwrap_or_default();
        inst.save().map_err(|e| format!("{e:#}"))
    }

    /// Keeps files out of (or back in) update offers.
    pub fn set_locked_many(
        &mut self,
        root: &Path,
        files: &[String],
        locked: bool,
    ) -> Result<(), String> {
        let inst = self
            .instances
            .iter_mut()
            .find(|i| i.root == root)
            .ok_or("That instance is gone.")?;
        let mut ids = Vec::new();
        for a in inst.addons.iter_mut().filter(|a| files.contains(&a.file_name)) {
            a.locked = locked;
            ids.push(a.project_id);
        }
        inst.save().map_err(|e| format!("{e:#}"))?;
        // Locking only takes them off the list; unlocking may add offers back.
        if locked {
            self.drop_updates(root, &ids);
        } else {
            self.updates.remove(root);
            self.check_updates(root, true);
        }
        Ok(())
    }

    /// Takes projects off an instance's list of available updates, keeping
    /// the rest (instead of checking every mod again).
    fn drop_updates(&mut self, root: &Path, ids: &[u64]) {
        if let Some(Updates::Ready(v)) = self.updates.get_mut(root) {
            v.retain(|u| !ids.contains(&u.project_id));
        }
    }

    /// Matches jars that aren't tracked to CurseForge projects.
    pub fn identify(&self, root: &Path) {
        if let Some(mut inst) = self.instance(root).cloned() {
            self.spawn_instance_job(&inst.root.clone(), format!("Identifying jars in {}", inst.name), move |job| {
                let n = install::identify_loose(&mut inst, job)?;
                job.detail(format!("Identified {n} jars"));
                Ok(Msg::InstanceChanged(inst.root.clone()))
            });
        }
    }

    /// Identifies just `jars` (put back from Backups), so the mod's record
    /// follows its jar; then checks updates again.
    pub fn identify_jars(&self, root: &Path, jars: Vec<String>) {
        if jars.is_empty() {
            return;
        }
        if let Some(mut inst) = self.instance(root).cloned() {
            self.spawn_instance_job(&inst.root.clone(), format!("Identifying {}", jars.join(", ")), move |job| {
                let n = install::identify_jars(&mut inst, &jars, job)?;
                job.detail(format!("Identified {n} jars"));
                Ok(Msg::Reidentified(inst.root.clone()))
            });
        }
    }

    /// Replaces a file with the same project from the other site.
    pub fn switch_source(&self, root: &Path, file: String) {
        if let Some(mut inst) = self.instance(root).cloned() {
            self.spawn_instance_job(&inst.root.clone(), format!("Switching source of {file}"), move |job| {
                let done = install::switch_source(&mut inst, &file, job)?;
                job.detail(done);
                Ok(Msg::InstanceChanged(inst.root.clone()))
            });
        }
    }

    pub fn javas(&self) -> Option<&[hopper::java::JavaInstall]> {
        self.javas.as_deref()
    }

    /// Looks for Java installs once.
    pub fn want_javas(&mut self) {
        if std::mem::replace(&mut self.javas_asked, true) {
            return;
        }
        let dir = PathBuf::from(&self.settings.launcher_dir);
        self.spawn(move || Msg::Javas(hopper::java::detect(&dir)));
    }

    pub fn peer_hub(&self) -> Option<std::sync::Arc<hopper::peer::Hub>> {
        self.peer_hub.clone()
    }

    pub fn peer_error(&self) -> Option<String> {
        self.peer_error.clone()
    }

    pub fn set_peer_sync(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_settings(|st| st.peer_sync = on, cx);
        if on {
            self.start_peers();
        } else if let Some(h) = self.peer_hub.take() {
            h.stop();
        }
        cx.notify();
    }

    fn start_peers(&mut self) {
        if self.peer_hub.is_some() {
            return;
        }
        match hopper::peer::Hub::start(self.wake.clone()) {
            Ok(h) => {
                self.peer_hub = Some(h);
                self.peer_error = None;
                self.shared_sig = 0;
            }
            Err(e) => self.peer_error = Some(format!("{e:#}")),
        }
    }

    /// Keeps the hub's instance list current and surfaces pair requests.
    fn peer_tick(&mut self, cx: &mut Context<Self>) {
        let Some(hub) = self.peer_hub.clone() else { return };
        let sig = self.instances.iter().fold(self.instances.len() as u64, |a, i| {
            a.wrapping_mul(31)
                .wrapping_add(i.addons.len() as u64 + i.name.len() as u64)
        });
        if sig != self.shared_sig {
            self.shared_sig = sig;
            let list = self.instances.iter().map(hopper::peer::SharedInst::of).collect();
            let names = self
                .instances
                .iter()
                .map(|i| {
                    let m = i
                        .addons
                        .iter()
                        .map(|a| (a.file_name.to_lowercase(), a.name.clone()))
                        .collect();
                    (hopper::peer::SharedInst::of(i).key, m)
                })
                .collect();
            hub.set_shared(list, names);
        }
        match hub.pending() {
            Some(req) if self.asked.as_deref() != Some(req.code.as_str()) => {
                self.asked = Some(req.code.clone());
                self.pair_asked = true;
                cx.emit(StoreEvent::PairRequest(req));
            }
            None => self.asked = None,
            _ => {}
        }
    }

    pub fn server_rev(&self) -> u64 {
        self.server_rev
    }

    pub fn servers(&self) -> &[ServerInfo] {
        &self.servers
    }

    pub fn server(&self, root: &Path) -> Option<&ServerInfo> {
        self.servers.iter().find(|s| s.root == root)
    }

    pub fn self_update(&self) -> Option<&SelfUpdate> {
        self.self_update.as_ref()
    }

    /// Looks for a newer Hopper in the update repository.
    pub fn check_self_update(&mut self) {
        let repo = self.update_repo();
        self.self_update = Some(SelfUpdate::Checking);
        self.spawn(move || {
            Msg::SelfUpdate(match hopper::selfupdate::check(&repo) {
                Ok(Some(r)) => SelfUpdate::Available(r),
                Ok(None) => SelfUpdate::UpToDate,
                Err(e) => SelfUpdate::Failed(format!("{e:#}")),
            })
        });
    }

    /// The quiet background check: skipped while a check or install is
    /// under way; a found update is checked again, so a release cut since
    /// replaces it. A failure (offline, rate limit) leaves the last state
    /// alone rather than showing an error.
    fn recheck_self_update(&mut self) {
        if matches!(
            self.self_update,
            Some(SelfUpdate::Checking | SelfUpdate::Installing(_) | SelfUpdate::Ready(_))
        ) {
            return;
        }
        let repo = self.update_repo();
        let (tx, wake) = (self.tx.clone(), self.wake.clone());
        std::thread::spawn(move || {
            let found = match hopper::selfupdate::check(&repo) {
                Ok(Some(r)) => SelfUpdate::Available(r),
                Ok(None) => SelfUpdate::UpToDate,
                Err(_) => return,
            };
            let _ = tx.send(Msg::SelfUpdateRecheck(found));
            wake();
        });
    }

    pub fn update_repo(&self) -> String {
        Some(self.settings.update_repo.trim().to_string())
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| hopper::settings::DEFAULT_UPDATE_REPO.to_string())
    }

    /// Downloads and installs the available update.
    pub fn install_self_update(&mut self) {
        let Some(SelfUpdate::Available(r)) = self.self_update.clone() else {
            return;
        };
        self.self_update = Some(SelfUpdate::Installing(r.version.clone()));
        let tx = self.tx.clone();
        let repo = self.update_repo();
        self.spawn_job(format!("Updating Hopper to {}", r.version), move |job| {
            // Straight to the newest release, even one cut after the
            // update was found, rather than one version at a time.
            let r = match hopper::selfupdate::check(&repo) {
                Ok(Some(latest)) if hopper::selfupdate::is_newer(&latest.version, &r.version) => latest,
                _ => r,
            };
            match hopper::selfupdate::install(&r, job) {
                Ok(_) => Ok(Msg::SelfUpdate(SelfUpdate::Ready(r.version.clone()))),
                Err(e) => {
                    let _ = tx.send(Msg::SelfUpdate(SelfUpdate::Failed(format!("{e:#}"))));
                    Err(e)
                }
            }
        });
    }

    /// What a fresh read of every release says about updates: the newest,
    /// if newer than this build, is on offer; otherwise this is up to date.
    /// Leaves an install under way (or done) alone.
    pub fn saw_releases(&mut self, newest: Option<&hopper::selfupdate::Release>) {
        if matches!(self.self_update, Some(SelfUpdate::Installing(_) | SelfUpdate::Ready(_))) {
            return;
        }
        self.self_update = Some(match newest {
            Some(r) if hopper::selfupdate::is_newer(&r.version, hopper::selfupdate::CURRENT) => {
                SelfUpdate::Available(r.clone())
            }
            _ => SelfUpdate::UpToDate,
        });
    }

    /// Installs one chosen release, older or newer: it runs from the next
    /// start, like an update.
    pub fn install_hopper_version(&mut self, r: hopper::selfupdate::Release) {
        if matches!(self.self_update, Some(SelfUpdate::Installing(_))) {
            return;
        }
        self.self_update = Some(SelfUpdate::Installing(r.version.clone()));
        let tx = self.tx.clone();
        let verb = if hopper::selfupdate::is_newer(&r.version, hopper::selfupdate::CURRENT) {
            "Updating"
        } else {
            "Rolling back"
        };
        self.spawn_job(format!("{verb} Hopper to {}", r.version), move |job| {
            match hopper::selfupdate::install(&r, job) {
                Ok(_) => Ok(Msg::SelfUpdate(SelfUpdate::Ready(r.version.clone()))),
                Err(e) => {
                    let _ = tx.send(Msg::SelfUpdate(SelfUpdate::Failed(format!("{e:#}"))));
                    Err(e)
                }
            }
        });
    }

    /// Reads the server list again (after pairing with a Hopper Server, or
    /// changing the panel).
    pub fn rediscover_servers(&mut self) {
        self.discover_servers();
    }

    /// The rig's servers, then the ones Hopper hosts here (run by Hopper
    /// itself through the local panel).
    fn discover_servers(&mut self) {
        // Hosted servers are on this PC: read now. The rig's folder is a
        // network share that can hang for seconds: read it off the UI thread.
        let hosted_root = self.hosted_root();
        self.servers.retain(|s| !s.root.starts_with(&hosted_root) || hosted_root.trim().is_empty());
        let rig_root = PathBuf::from(&self.settings.servers_root);
        if !hosted_root.trim().is_empty() && Path::new(&hosted_root) != rig_root {
            for s in hopper::server::discover(Path::new(&hosted_root)) {
                self.local.add(&s.root);
                self.ping(&s);
                self.servers.push(s);
            }
        }
        // Each Hopper Server lists its servers itself; no shared drive needed.
        let ids: Vec<String> = self.settings.hopper_panels.iter().map(|p| p.id.clone()).collect();
        self.panel_servers.retain(|k, _| ids.contains(k));
        for p in self.settings.hopper_panels.clone() {
            let id = p.id.clone();
            match hopper::hopperd::Client::for_panel(&p) {
                Some(client) => self.spawn(move || {
                    Msg::PanelServers(id, client.server_infos().map_err(|e| format!("{e:#}")))
                }),
                None => {
                    self.panel_servers.insert(id, Err("Not paired. Pair it again in Settings â€º Servers.".into()));
                }
            }
        }
        if rig_root.as_os_str().is_empty() {
            self.folder_servers.clear();
            self.merge_servers_quiet();
            return;
        }
        self.spawn(move || Msg::RigServers(hopper::server::discover(&rig_root)));
    }

    /// The server list from its sources: each Hopper Server's, then the
    /// servers folder's (minus the ones a Hopper Server also lists: the same
    /// folder shows once, under the panel), then the hosted ones.
    fn merge_servers(&mut self, cx: &mut Context<Self>) {
        self.merge_servers_quiet();
        for s in self.servers.clone() {
            self.ping(&s);
        }
        self.settle_server_tools(cx);
    }

    fn merge_servers_quiet(&mut self) {
        let hosted_root = self.hosted_root();
        let hosted_root = hosted_root.trim().to_string();
        let hosted: Vec<ServerInfo> = std::mem::take(&mut self.servers)
            .into_iter()
            .filter(|s| !hosted_root.is_empty() && s.root.starts_with(&hosted_root))
            .collect();
        let mut out: Vec<ServerInfo> = Vec::new();
        for p in &self.settings.hopper_panels {
            if let Some(Ok(list)) = self.panel_servers.get(&p.id) {
                out.extend(list.iter().cloned());
            }
        }
        let folder_of = |s: &ServerInfo| s.root.file_name().map(|f| f.to_string_lossy().to_lowercase());
        let mut moved: Vec<(String, String)> = Vec::new();
        for f in &self.folder_servers {
            let twin = out.iter().find(|r| r.remote.is_some() && folder_of(r) == folder_of(f) && r.port() == f.port());
            match twin {
                // Its pairing goes with it.
                Some(r) => moved.push((f.root.to_string_lossy().to_string(), r.root.to_string_lossy().to_string())),
                None => out.push(f.clone()),
            }
        }
        for (from, to) in moved {
            if let Some(inst) = self.settings.pairings.get(&from).cloned()
                && !self.settings.pairings.contains_key(&to)
            {
                self.update_settings_quietly(|st| {
                    st.pairings.insert(to, inst);
                });
            }
        }
        out.extend(hosted);
        self.servers = out;
        self.server_rev += 1;
    }

    /// The servers folder's servers that a Hopper Server also lists are
    /// shown once; this says whether `s` (on a panel) is also in that folder.
    pub fn also_in_folder(&self, s: &ServerInfo) -> bool {
        s.remote.is_some()
            && self.folder_servers.iter().any(|f| f.root.file_name() == s.root.file_name() && f.port() == s.port())
    }

    /// Why a paired Hopper Server couldn't list its servers, if it couldn't.
    pub fn panel_error(&self, panel: &str) -> Option<&str> {
        match self.panel_servers.get(panel) {
            Some(Err(e)) => Some(e),
            _ => None,
        }
    }

    /// Whether a paired panel's list has come back (either way).
    pub fn panel_listed(&self, panel: &str) -> bool {
        self.panel_servers.contains_key(panel)
    }

    /// Where hosted servers live (HOPPER_HOSTED_ROOT wins, for tests).
    pub fn hosted_root(&self) -> String {
        std::env::var("HOPPER_HOSTED_ROOT").unwrap_or_else(|_| self.settings.hosted_root.clone())
    }

    /// Whether Hopper hosts this server on this PC.
    pub fn is_hosted(&self, root: &Path) -> bool {
        let h = self.hosted_root();
        !h.trim().is_empty() && root.starts_with(h.trim())
    }

    /// Why crash restarts stopped for a hosted server, if they did.
    pub fn schedule_gave_up(&self, root: &Path) -> Option<String> {
        self.scheduler.as_ref()?.gave_up(root)
    }

    /// Stops the scheduler (before hosted servers are stopped on quit, so
    /// it doesn't start them again).
    pub fn stop_scheduler(&mut self) {
        if let Some(mut s) = self.scheduler.take() {
            s.stop();
        }
    }

    pub fn local_panel(&self) -> Arc<hopper::hosting::LocalPanel> {
        self.local.clone()
    }

    /// Makes a server to host here (from an instance or from scratch), then
    /// lists it.
    pub fn create_hosted(
        &self,
        opts: hopper::hosting::CreateOptions,
        from: Option<PathBuf>,
        server_pack: bool,
        done: impl FnOnce(PathBuf) + Send + 'static,
    ) {
        let inst = from.and_then(|r| self.instance(&r).cloned());
        let anyway = self.settings.sync_anyway.clone();
        let title = format!("Creating {}", opts.name);
        let meta = opts.hosted_meta();
        self.spawn_job(title, move |job| {
            let root = match inst {
                // The pack author's own server pack: exactly their mod set.
                Some(i) if server_pack => {
                    let r = hopper::hosting::create_from_server_pack(&i, &opts, job)?;
                    job.note(format!(
                        "From {}: {} mods{}",
                        r.server_pack,
                        r.create.mods_copied,
                        if r.used_server_pack_mods { "" } else { " (from the instance; the server pack downloads its mods itself)" }
                    ));
                    if let Some(n) = r.create.note.clone() {
                        job.note(n);
                    }
                    r.create.root
                }
                Some(i) => {
                    let r = hopper::hosting::create_from_instance(&i, &opts, &anyway, job)?;
                    job.note(format!(
                        "Copied {} mods; left out {} client-only.",
                        r.mods_copied,
                        r.skipped.len()
                    ));
                    r.root
                }
                None => hopper::hosting::create_server(&opts, job)?,
            };
            let _ = meta.save(&root);
            done(root);
            Ok(Msg::ServersChanged)
        });
    }

    /// Makes a server to host here straight from a pack's server files
    /// (found in Browse, no instance needed), then lists it. The pack's
    /// Minecraft and loader replace whatever `opts` holds.
    pub fn create_hosted_from_files(
        &self,
        mut opts: hopper::hosting::CreateOptions,
        files: hopper::browse::ServerFiles,
        done: impl FnOnce(PathBuf) + Send + 'static,
    ) {
        let title = format!("Creating {}", opts.name);
        self.spawn_job(title, move |job| {
            let t = hopper::hosting::pack_target(&files, job)?;
            opts.mc_version = t.mc_version;
            opts.loader = hopper::hosting::ServerLoader::from_instance(&t.loader)?;
            let r = hopper::hosting::create_from_server_files(&files, &opts, job)?;
            job.note(format!("From {}: {} mods", r.server_pack, r.create.mods_copied));
            let _ = opts.hosted_meta().save(&r.create.root);
            done(r.create.root);
            Ok(Msg::ServersChanged)
        });
    }

    /// Makes a server to host here from a modpack that has no server
    /// files, built from its client pack (see
    /// `hosting::create_from_client_pack`), then lists it. The mods left
    /// out as client-only are listed in the task and in
    /// `left-out-client-mods.txt` in the server folder.
    pub fn create_hosted_from_client_pack(
        &self,
        opts: hopper::hosting::CreateOptions,
        pack: hopper::cf::Mod,
        file: hopper::cf::File,
        done: impl FnOnce(PathBuf) + Send + 'static,
    ) {
        let title = format!("Creating {}", opts.name);
        self.spawn_job(title, move |job| {
            let (r, used) = hopper::hosting::create_from_client_pack(&pack, &file, &opts, job)?;
            job.note(format!(
                "Built from the client pack: {} mods copied, {} left out as client-only.",
                r.mods_copied,
                r.skipped.len()
            ));
            if !r.skipped.is_empty() {
                let lines: Vec<String> = r.skipped.iter().map(|(f, why)| format!("{f}: {why}")).collect();
                let _ = std::fs::write(
                    r.root.join("left-out-client-mods.txt"),
                    format!(
                        "Hopper left these mods out of the server as client-only. If the server needs one, copy it from the pack into mods.

{}
",
                        lines.join("
")
                    ),
                );
                job.note(format!("Left out: {}", lines.join("; ")));
            }
            if let Some(n) = r.note.clone() {
                job.note(n);
            }
            let _ = used.hosted_meta().save(&r.root);
            done(r.root);
            Ok(Msg::ServersChanged)
        });
    }

    /// Downloads Java `major` (Temurin) into Hopper's runtimes, then looks
    /// for Java again.
    pub fn download_java(&mut self, major: u32) {
        if self.java_download(&format!("java-{major}")).is_some() {
            return;
        }
        let dir = PathBuf::from(&self.settings.launcher_dir);
        let id = self.spawn_job_id(format!("Downloading Java {major}"), move |job| {
            hopper::javadl::install_temurin(major, hopper::javadl::Package::Jre, job)?;
            Ok(Msg::Javas(hopper::java::detect(&dir)))
        });
        self.java_jobs.insert(format!("java-{major}"), id);
    }

    pub fn translations(&self) -> &crate::translation::Translations {
        &self.translations
    }

    pub fn tr_tick(&self) -> &Entity<TrTick> {
        &self.tr_tick
    }

    /// The download of `lang`'s model, while it runs or after it failed.
    pub fn tr_download(&self, lang: &hopper::translate::Lang) -> Option<hopper::jobs::JobState> {
        self.job_state(*self.translations.jobs.get(lang.code)?)
            .filter(|j| !j.done || j.error.is_some())
    }

    /// Translates `texts` from `lang`, downloading its model first when
    /// needed (the user asked, with the size in front of them).
    pub fn tr_start(&mut self, texts: Vec<String>, lang: &'static hopper::translate::Lang, cx: &mut Context<Self>) {
        self.translations.jobs.remove(lang.code);
        if hopper::translate::installed(lang) {
            self.translations.request(texts, lang);
        } else {
            self.translations.after.insert(lang.code, texts);
            let notify = self.translations.notifier();
            let id = self.spawn_job_id(format!("Downloading the {} translation model", lang.name), move |job| {
                job.detail(format!("{} MB, once. Translation runs on this PC.", lang.download >> 20));
                // The bar shows the percentage: redraw it at each new one.
                let mut shown = u64::MAX;
                hopper::translate::download(lang, Some(job), |done, total| {
                    job.progress((done >> 10) as usize, (total >> 10).max(1) as usize);
                    let pct = done * 100 / total.max(1);
                    if pct != shown {
                        shown = pct;
                        notify();
                    }
                })?;
                Ok(Msg::TrDownloaded(lang.code))
            });
            self.translations.jobs.insert(lang.code, id);
        }
        cx.notify();
    }

    /// Deletes `lang`'s model and what was translated with it.
    pub fn tr_remove(&mut self, lang: &'static hopper::translate::Lang, cx: &mut Context<Self>) {
        if let Err(e) = hopper::translate::remove(lang) {
            log::warn!("removing the {} model: {e:#}", lang.name);
        }
        self.translations.forget_all();
        self.update_settings(|st| st.translate_always.retain(|c| c != lang.code), cx);
    }

    /// A Java download running now ("java-21", "graalvm-25"): its job.
    pub fn java_download(&self, key: &str) -> Option<hopper::jobs::JobState> {
        self.job_state(*self.java_jobs.get(key)?).filter(|j| !j.done)
    }

    /// Downloads Oracle GraalVM `major` (21 or 25) into Hopper's runtimes.
    pub fn download_graalvm(&mut self, major: u32) {
        if self.java_download(&format!("graalvm-{major}")).is_some() {
            return;
        }
        let dir = PathBuf::from(&self.settings.launcher_dir);
        let id = self.spawn_job_id(format!("Downloading GraalVM {major}"), move |job| {
            hopper::javadl::install_graalvm(major, job)?;
            Ok(Msg::Javas(hopper::java::detect(&dir)))
        });
        self.java_jobs.insert(format!("graalvm-{major}"), id);
    }

    /// The server's state in the server panel, when one is set up.
    pub fn panel_stats(&self, root: &Path) -> Option<&hopper::panel::PanelStats> {
        self.panel_stats.get(root)
    }

    pub fn status(&self, root: &Path) -> Option<&Status> {
        self.statuses.get(root)
    }

    pub fn ping(&self, s: &ServerInfo) {
        // Hosted servers answer on this PC.
        let host = if self.is_hosted(&s.root) {
            "127.0.0.1".to_string()
        } else if let Some(r) = &s.remote {
            // On a Hopper Server: the machine the panel runs on.
            hopper::hopperd::host_name(&r.base)
        } else {
            self.settings.server_host.clone()
        };
        let (port, key) = (s.port(), s.root.clone());
        self.spawn(move || Msg::Status {
            key,
            status: hopper::server::ping(&host, port),
        });
    }

    /// The pack a server runs.
    pub fn paired(&self, s: &ServerInfo) -> Option<&Instance> {
        hopper::rigsync::paired(&self.settings.pairings, s, &self.instances)
    }

    /// The server an instance is paired with.
    pub fn server_of(&self, inst_root: &Path) -> Option<&ServerInfo> {
        self.servers
            .iter()
            .find(|s| self.paired(s).is_some_and(|i| i.root == inst_root))
    }

    /// Puts an instance in a group (or takes it out with `None`).
    pub fn set_group(&mut self, root: &Path, group: Option<String>, cx: &mut Context<Self>) {
        let key = root.to_string_lossy().into_owned();
        self.update_settings(
            |st| {
                match group {
                    Some(g) => st.groups.insert(key, g),
                    None => st.groups.remove(&key),
                };
            },
            cx,
        );
    }

    /// Group names in use, sorted.
    pub fn groups(&self) -> Vec<String> {
        let mut g: Vec<String> = self.settings.groups.values().cloned().collect();
        g.sort_by_key(|n| n.to_lowercase());
        g.dedup();
        g
    }

    /// Copies an image into the instance and uses it as the icon.
    pub fn set_icon(&mut self, root: &Path, pick: &Path) -> Result<(), String> {
        let ext = pick
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or("png".into());
        // A new name each time, so cached images show the new picture.
        let stamp = hopper::time::now_iso().replace([':', '.', '-'], "");
        let dest = root.join(format!("hopper-icon-{stamp}.{ext}"));
        std::fs::copy(pick, &dest).map_err(|e| format!("{e:#}"))?;
        for old in std::fs::read_dir(root).into_iter().flatten().flatten() {
            let n = old.file_name().to_string_lossy().into_owned();
            if n.starts_with("hopper-icon") && old.path() != dest {
                let _ = std::fs::remove_file(old.path());
            }
        }
        let inst = self
            .instances
            .iter_mut()
            .find(|i| i.root == root)
            .ok_or("That instance is gone.")?;
        let url = dest.to_string_lossy().into_owned();
        inst.icon = Some(url.clone());
        inst.save().map_err(|e| format!("{e:#}"))?;
        self.images.remove(&url);
        self.want_image(&url);
        Ok(())
    }

    /// Adds dropped files: pack zips and .mrpacks become instances; jars go
    /// to `target`'s mods, anything else to its `folder`.
    pub fn add_dropped(&self, paths: Vec<PathBuf>, target: Option<PathBuf>, folder: String) {
        let instances_root = PathBuf::from(&self.settings.instances_root);
        let target = target.and_then(|r| self.instance(&r).cloned());
        self.spawn_job(format!("Adding {} dropped files", paths.len()), move |job| {
            let mut jars = 0;
            let mut last = None;
            let mut failed = 0;
            for p in &paths {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                // One bad file is reported; the rest still go in.
                let r = (|| -> anyhow::Result<()> {
                    let lower = name.to_lowercase();
                    let is_pack = lower.ends_with(".zip")
                        && zip::ZipArchive::new(std::fs::File::open(p)?)
                            .is_ok_and(|mut z| z.by_name("manifest.json").is_ok());
                    if is_pack || lower.ends_with(".mrpack") {
                        let inst = if is_pack {
                            hopper::modpack::import_zip(&instances_root, p, job)?
                        } else {
                            hopper::modpack::import_mrpack(&instances_root, p, job)?
                        };
                        job.note(format!("Imported {} as a new instance", inst.name));
                        last = Some(inst.root);
                        return Ok(());
                    }
                    let Some(inst) = &target else {
                        job.note(format!("{name}: pick an instance first"));
                        return Ok(());
                    };
                    // A world zip on the Worlds tab becomes a world.
                    if folder == "saves" && lower.ends_with(".zip") {
                        let w = hopper::instance_ops::import_world_zip(inst, p)?;
                        job.note(format!("Added the world {}", w.display()));
                        return Ok(());
                    }
                    // Jars are mods, except on the Data packs tab.
                    let packs = folder.ends_with("datapacks")
                        || folder.starts_with("config/")
                        || folder.starts_with("global_packs/");
                    let dir = if lower.ends_with(".jar") && !packs { "mods" } else { folder.as_str() };
                    let dest = inst.root.join(dir).join(&name);
                    std::fs::create_dir_all(inst.root.join(dir))?;
                    // Something already there with that name is kept aside.
                    if dest.exists() {
                        let stamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(0, |d| d.as_secs());
                        let keep = inst.root.join(".hopper-backups").join(stamp.to_string()).join(dir);
                        std::fs::create_dir_all(&keep)?;
                        std::fs::rename(&dest, keep.join(&name))?;
                        job.note(format!("{name} replaced; the old one is in .hopper-backups"));
                    }
                    std::fs::copy(p, &dest)?;
                    jars += usize::from(dir == "mods");
                    Ok(())
                })();
                if let Err(e) = r {
                    failed += 1;
                    job.note(format!("{name}: {e:#}"));
                }
            }
            if failed == paths.len() && failed > 0 {
                anyhow::bail!("None of the dropped files could be added.");
            }
            match target {
                Some(mut inst) => {
                    if jars > 0 {
                        let n = hopper::install::identify_loose(&mut inst, job)?;
                        job.detail(format!(
                            "Added {jars} jars; identified {n} untracked jars on CurseForge"
                        ));
                    }
                    Ok(Msg::InstanceChanged(inst.root.clone()))
                }
                None => Ok(last.map_or(Msg::Nothing, Msg::InstanceChanged)),
            }
        });
    }

    pub fn set_pairing(&mut self, server: &Path, inst: &Path, cx: &mut Context<Self>) {
        let (k, v) = (
            server.to_string_lossy().into_owned(),
            inst.to_string_lossy().into_owned(),
        );
        self.update_settings(
            |st| {
                st.pairings.insert(k, v);
                // Pairing is using server tools.
                st.server_tools = Some(true);
            },
            cx,
        );
    }

    /// Whether server tools show (the Servers page, server buttons on
    /// instances, Peers): the user's choice, else on when anything
    /// server-related is set up and off for a fresh install.
    pub fn server_tools(&self) -> bool {
        self.settings.server_tools.unwrap_or_else(|| self.server_tools_used())
    }

    /// Servers, a panel, a servers folder, pairings or peers are set up.
    fn server_tools_used(&self) -> bool {
        let s = &self.settings;
        !self.servers.is_empty()
            || !s.servers_root.trim().is_empty()
            || !s.panel_url.trim().is_empty()
            || !s.hopper_panels.is_empty()
            || !s.pairings.is_empty()
            || s.peer_sync
    }

    /// Not chosen yet but in use: remembered as on, so it stays on if the
    /// servers later go.
    fn settle_server_tools(&mut self, cx: &mut Context<Self>) {
        if self.settings.server_tools.is_none() && self.server_tools_used() {
            self.update_settings(|st| st.server_tools = Some(true), cx);
        }
    }

    /// Turns server tools on or off (the Settings switch, and every server
    /// action used while they are off).
    pub fn set_server_tools(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_settings(|st| st.server_tools = Some(on), cx);
    }

    /// Notes that a modpack's server files were downloaded (a sign for the
    /// instance's server hint).
    pub fn note_server_files(&mut self, project_id: u64) {
        if project_id != 0 {
            self.server_files_got.insert(project_id);
        }
    }

    /// Whether the "Running a server for this pack?" hint shows for an
    /// instance: server tools are off, it wasn't dismissed, and there is a
    /// clear sign: its server files were downloaded, a server hosted here
    /// is for it, or another Hopper asked to pair.
    pub fn server_hint(&self, inst: &Instance) -> bool {
        if self.server_tools() {
            return false;
        }
        let key = inst.root.to_string_lossy();
        if self.settings.server_hint_dismissed.iter().any(|k| *k == key) {
            return false;
        }
        let files = inst
            .pack
            .as_ref()
            .is_some_and(|p| self.server_files_got.contains(&p.project_id));
        let name = inst.name.to_lowercase();
        let hosted = self.server_of(&inst.root).is_some_and(|s| self.is_hosted(&s.root))
            || (!name.is_empty()
                && self
                    .servers
                    .iter()
                    .any(|s| self.is_hosted(&s.root) && s.name.to_lowercase().contains(&name)));
        files || hosted || self.pair_asked
    }

    /// Hides an instance's server hint for good.
    pub fn dismiss_server_hint(&mut self, root: &Path, cx: &mut Context<Self>) {
        let key = root.to_string_lossy().into_owned();
        self.update_settings(
            |st| {
                if !st.server_hint_dismissed.contains(&key) {
                    st.server_hint_dismissed.push(key);
                }
            },
            cx,
        );
    }

    /// Ends a running game.
    pub fn stop(&self, root: &Path) {
        if let Some(r) = self.running.get(root) {
            let pid = r.pid;
            self.spawn_job("Stopping Minecraft", move |_| {
                hopper::game::stop(pid)?;
                Ok(Msg::Nothing)
            });
        }
    }
}

/// "NeoForge 21.1.256" from "neoforge-21.1.256".
/// Tells CurseForge's own file about a loader Hopper changed, so the
/// instance starts the same loader from CurseForge.
fn sync_cf_loader(inst: &mut hopper::instances::Instance, job: &Job) {
    use hopper::cf_instance::Synced;
    match hopper::cf_instance::write_loader(inst) {
        Ok(Synced::Written) => {
            if let Err(e) = inst.save() {
                log::warn!("saving {}: {e:#}", inst.name);
            }
            job.note(format!("CurseForge will start {} too.", loader_label(&inst.loader)));
        }
        Ok(Synced::CurseForgeOpen) => job.note(format!(
            "CurseForge is open, so it still starts its old loader. Close it, then use Update CurseForge in {}'s Settings.",
            inst.name
        )),
        Ok(Synced::Nothing) => {}
        Err(e) => job.note(format!("CurseForge's copy of the loader wasn't updated: {e:#}")),
    }
}

/// Tells CurseForge's own file about a pack update, so CurseForge doesn't
/// offer or reinstall the old version. False when CurseForge is open and
/// nothing could be written.
fn sync_cf_pack(inst: &mut hopper::instances::Instance, job: &Job) -> bool {
    use hopper::cf_instance::Synced;
    match hopper::cf_instance::write_pack(inst) {
        Ok(Synced::Written) => {
            if let Err(e) = inst.save() {
                log::warn!("saving {}: {e:#}", inst.name);
            }
            if let Some(p) = &inst.pack {
                job.note(format!("CurseForge now lists {} {} too.", p.name, p.version));
            }
        }
        Ok(Synced::CurseForgeOpen) => {
            job.note(format!(
                "CurseForge is open, so it still lists the old pack version. Close it, then use Update CurseForge in {}'s Settings.",
                inst.name
            ));
            return false;
        }
        Ok(Synced::Nothing) => {}
        Err(e) => job.note(format!("CurseForge's copy of the pack version wasn't updated: {e:#}")),
    }
    true
}

/// What Play would install for `inst`'s loader, done ahead of time.
fn prepare_loader(
    inst: &hopper::instances::Instance,
    dir: &Path,
    defaults: &hopper::launch::Defaults,
    native: bool,
    job: &Job,
) -> anyhow::Result<()> {
    if native {
        hopper::launch_native::prepare(inst, dir, defaults, job)?;
    } else {
        hopper::launch::ensure_loader(inst, dir, job)?;
    }
    Ok(())
}

pub fn loader_label(loader: &str) -> String {
    if loader.is_empty() {
        return "Vanilla".into();
    }
    let (kind, ver) = loader.split_once('-').unwrap_or((loader, ""));
    let kind = match kind.to_ascii_lowercase().as_str() {
        "neoforge" => "NeoForge",
        "forge" => "Forge",
        "fabric" => "Fabric",
        "quilt" => "Quilt",
        _ => kind,
    };
    format!("{kind} {ver}").trim().to_string()
}

/// "12h 5m" or "40m".
pub fn played(secs: u64) -> Option<String> {
    (secs >= 60).then(|| {
        let (h, m) = (secs / 3600, secs / 60 % 60);
        if h > 0 {
            format!("{h}h {m}m")
        } else {
            format!("{m}m")
        }
    })
}

pub fn open_path(path: &Path) {
    // A server on a Hopper Server: open the spot in the panel's web UI.
    if let Some(url) = hopper::hopperd::web_url(path) {
        let _ = std::process::Command::new("explorer").arg(url).spawn();
        return;
    }
    let _ = std::process::Command::new("explorer").arg(path).spawn();
}

/// An update job's work: the batch it's on and what was queued after.
#[derive(Default)]
struct UpdateQueue {
    active: Vec<u64>,
    waiting: Vec<Update>,
    /// How far each active download is, 0..=1.
    progress: HashMap<u64, f32>,
}

impl UpdateQueue {
    fn has(&self, project_id: u64) -> bool {
        self.active.contains(&project_id) || self.waiting.iter().any(|u| u.project_id == project_id)
    }
}

/// Drops an update job's queue if the job stops early (error, cancel, bug),
/// so later updates start a new job instead of waiting on a gone one.
struct ClearQueue(Arc<Mutex<HashMap<PathBuf, UpdateQueue>>>, Option<PathBuf>);

impl Drop for ClearQueue {
    fn drop(&mut self) {
        if let Some(r) = self.1.take() {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(&r);
        }
    }
}

/// Notified as running jobs' detail and progress move (see `Store::job_ticks`).
pub struct JobTicks;

/// Notified when a translation finishes (see `Store::tr_tick`).
pub struct TrTick;
