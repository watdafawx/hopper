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
    /// An instance changed on disk; reload it.
    InstanceChanged(PathBuf),
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
    Updates {
        root: PathBuf,
        result: Result<Vec<Update>, String>,
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
    /// An instance's game ended with a crash report (its name).
    GameCrashed(String),
    /// The hosted-server scheduler did something: (message, is a problem).
    Schedule(String, bool),
    /// A play session of this instance was saved.
    SessionSaved(PathBuf),
    /// Show a project's page beside an instance (a link in a changelog).
    Peek { root: PathBuf, project: u64 },
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

pub struct Store {
    settings: Settings,
    instances: Vec<Instance>,
    running: HashMap<PathBuf, Running>,
    jobs: Jobs,
    images: HashMap<String, Image>,
    anims: HashMap<String, Anim>,
    /// Ready clips' keys, oldest first.
    anims_ready: std::collections::VecDeque<String>,
    /// Installed projects' categories (`None` until the disk cache is read);
    /// keys being fetched are in `cats_pending`.
    cats: Option<HashMap<String, Vec<String>>>,
    cats_pending: std::collections::HashSet<String>,
    updates: HashMap<PathBuf, Updates>,
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
    /// Pack files that couldn't be downloaded automatically, per instance.
    pending: HashMap<PathBuf, Vec<hopper::modpack::Pending>>,
    /// Servers Hopper hosts on this PC, and the panel that runs them.
    local: Arc<hopper::hosting::LocalPanel>,
    /// Daily restarts, crash restarts and autostart for hosted servers.
    scheduler: Option<hopper::hosting::Scheduler>,
    /// Instances jobs are changing, with how many jobs.
    busy: Arc<std::sync::Mutex<HashMap<PathBuf, u32>>>,
    /// Mod updates of instances an update job is running on: the batch
    /// it's on and what waits for it.
    update_queue: Arc<Mutex<HashMap<PathBuf, UpdateQueue>>>,
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
    /// Instance roots the game watcher looks for.
    watched: Arc<Mutex<Vec<PathBuf>>>,
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
        let watched = Arc::new(Mutex::new(Vec::new()));
        {
            let (watched, tx, wake) = (watched.clone(), tx.clone(), wake.clone());
            hopper::game::watch(
                move || watched.lock().map(|w| w.clone()).unwrap_or_default(),
                move |r| {
                    let _ = tx.send(Msg::Running(r));
                    wake();
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
            jobs: Jobs::new(wake.clone()),
            images: HashMap::new(),
            anims: HashMap::new(),
            anims_ready: Default::default(),
            cats: None,
            cats_pending: Default::default(),
            updates: HashMap::new(),
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
            busy: Default::default(),
            update_queue: Default::default(),
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
            watched,
            tx,
            wake,
            _poll: Self::poll(rx, dirty, cx),
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
                    Some((panel, key, s.servers.clone()))
                }) else {
                    return;
                };
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
                let found: Vec<_> = found.into_iter().chain(local).collect();
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
    fn poll(rx: Receiver<Msg>, dirty: Arc<AtomicBool>, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                let msgs: Vec<Msg> = rx.try_iter().collect();
                if msgs.is_empty() && !dirty.swap(false, Ordering::Relaxed) {
                    continue;
                }
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
                    // A crash report written since it started: it crashed.
                    let crashed = hopper::logs::crash_reports(&root).first().is_some_and(|p| {
                        std::fs::metadata(p)
                            .and_then(|m| m.modified())
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .is_some_and(|t| t.as_secs() >= started)
                    });
                    if crashed && let Some(i) = self.instances.iter().find(|i| i.root == root) {
                        cx.emit(StoreEvent::GameCrashed(i.name.clone()));
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
                if matches!(self.anims.get(&url), Some(Anim::Waiting)) {
                    if ok {
                        self.decode_anim(url);
                    } else {
                        self.anims.insert(url, Anim::Still);
                    }
                }
            }
            Msg::Anim { key, clip } => {
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
                            cx.drop_image(f.clone(), None);
                        }
                    }
                }
            }
            Msg::Updates { root, result } => {
                self.updates.insert(
                    root,
                    match result {
                        Ok(u) => Updates::Ready(u),
                        Err(e) => Updates::Failed(e),
                    },
                );
            }
            Msg::Status { key, status } => {
                self.statuses.insert(key, status);
            }
            Msg::ServersChanged => {
                self.discover_servers();
                self.server_rev += 1;
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
            Msg::PackPage(id, r) => {
                let page = match r {
                    Ok((m, md, images)) => {
                        if let Some(u) = m.icon() {
                            self.want_image(&u.to_string());
                        }
                        for u in &images {
                            self.want_image(u);
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
                // Keep hosted ones; the rig's come first.
                let hosted_root = self.hosted_root();
                let hosted_root = hosted_root.trim();
                let hosted: Vec<ServerInfo> = std::mem::take(&mut self.servers)
                    .into_iter()
                    .filter(|s| !hosted_root.is_empty() && s.root.starts_with(hosted_root))
                    .collect();
                self.servers = rig;
                self.servers.extend(hosted);
                self.server_rev += 1;
                for s in self.servers.clone() {
                    self.ping(&s);
                }
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

    /// What keeps Hopper from restarting by itself now: tasks, a game, or
    /// servers hosted here. `None` when it's idle.
    pub fn busy_reason(&self) -> Option<String> {
        let tasks = self.jobs.snapshot().iter().filter(|j| !j.done).count();
        if tasks > 0 {
            return Some(format!("{tasks} task{} running", if tasks == 1 { "" } else { "s" }));
        }
        if !self.running.is_empty() {
            return Some("a game is running".into());
        }
        if self.local.all().iter().any(|s| s.state().is_alive()) {
            return Some("a server hosted here is running".into());
        }
        None
    }

    pub fn running(&self, root: &Path) -> Option<&Running> {
        self.running.get(root)
    }

    pub fn jobs(&self) -> &Jobs {
        &self.jobs
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

    /// Decodes the cached original of `key` on a thread.
    fn decode_anim(&mut self, key: String) {
        self.anims.insert(key.clone(), Anim::Decoding);
        self.spawn(move || {
            let (base, width) = hopper::imgcache::split_width(&key);
            let clip = hopper::imgcache::cached(base)
                .ok_or_else(|| anyhow::anyhow!("not cached"))
                .and_then(|p| crate::anim::decode(&p, width))
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
        self.run_job(None, title.into(), work);
    }

    /// [`spawn_job`] for work that changes an instance: it counts as busy
    /// until the job ends, so the UI holds off other changes to it.
    pub fn spawn_instance_job(
        &self,
        root: &Path,
        title: impl Into<String>,
        work: impl FnOnce(&Job) -> anyhow::Result<Msg> + Send + 'static,
    ) {
        self.run_job(Some(root.to_path_buf()), title.into(), work);
    }

    fn run_job(
        &self,
        root: Option<PathBuf>,
        title: String,
        work: impl FnOnce(&Job) -> anyhow::Result<Msg> + Send + 'static,
    ) {
        let job = self.jobs.start(title);
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
            Ok(Msg::InstanceChanged(inst.root.clone()))
        });
    }

    /// Whether a job is changing this instance.
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
            self.spawn_job(format!("Starting {}", full.name), move |job| {
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
            self.spawn_job(format!("Launching {}", full.name), move |job| {
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
        let panel = hopper::panel::from_settings(&self.settings);
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
            Ok(Msg::InstanceChanged(full.root.clone()))
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
        // A stale list stays up while the new one loads, so the update
        // buttons don't blink out.
        if force || !matches!(self.updates.get(root), Some(Updates::Ready(_))) {
            self.updates.insert(root.to_path_buf(), Updates::Checking);
        }
        let root = root.to_path_buf();
        self.spawn(move || Msg::Updates {
            result: install::check_updates(&inst).map_err(|e| format!("{e:#}")),
            root,
        });
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
        q.insert(root.to_path_buf(), UpdateQueue { active: Vec::new(), waiting: updates });
        drop(q);
        let queue = self.update_queue.clone();
        self.spawn_instance_job(&inst.root.clone(), format!("Updating mods in {}", inst.name), move |job| {
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
                pages.extend(install::apply_updates(&mut inst, &batch, job).map_err(install::explain_locked)?);
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
            let (Ok(exe), Some(dir)) = (std::env::current_exe(), hopper::modpack::fetch_dir()) else {
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
    pub fn delete_files(&mut self, root: &Path, files: &[String]) -> Result<(), String> {
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
                Some(i) => install::delete(inst, i),
                None => install::delete_loose_file(inst, f),
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
        self.spawn(move || Msg::RigServers(hopper::server::discover(&rig_root)));
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
        let meta = hosted_meta(&opts);
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
            let _ = hosted_meta(&opts).save(&r.create.root);
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
            let _ = hosted_meta(&used).save(&r.root);
            done(r.root);
            Ok(Msg::ServersChanged)
        });
    }

    /// Downloads Java `major` (Temurin) into Hopper's runtimes, then looks
    /// for Java again.
    pub fn download_java(&mut self, major: u32) {
        let dir = PathBuf::from(&self.settings.launcher_dir);
        self.spawn_job(format!("Downloading Java {major}"), move |job| {
            hopper::javadl::install_temurin(major, hopper::javadl::Package::Jre, job)?;
            Ok(Msg::Javas(hopper::java::detect(&dir)))
        });
    }

    /// Downloads Oracle GraalVM `major` (21 or 25) into Hopper's runtimes.
    pub fn download_graalvm(&mut self, major: u32) {
        let dir = PathBuf::from(&self.settings.launcher_dir);
        self.spawn_job(format!("Downloading GraalVM {major}"), move |job| {
            hopper::javadl::install_graalvm(major, job)?;
            Ok(Msg::Javas(hopper::java::detect(&dir)))
        });
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
    let _ = std::process::Command::new("explorer").arg(path).spawn();
}

/// An update job's work: the batch it's on and what was queued after.
#[derive(Default)]
struct UpdateQueue {
    active: Vec<u64>,
    waiting: Vec<Update>,
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

/// What the Servers page shows for a server Hopper made.
fn hosted_meta(opts: &hopper::hosting::CreateOptions) -> hopper::hosting::HostedMeta {
    use hopper::hosting::ServerLoader as L;
    hopper::hosting::HostedMeta {
        minecraft: opts.mc_version.clone(),
        loader: match &opts.loader {
            L::Vanilla => "Vanilla".into(),
            L::Fabric(v) | L::Quilt(v) => {
                format!("{} {}", opts.loader.name(), v.clone().unwrap_or_default()).trim().to_string()
            }
            L::NeoForge(v) | L::Forge(v) => format!("{} {v}", opts.loader.name()),
        },
    }
}
