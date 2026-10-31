//! Notices instance folders changing outside Hopper (a pack installed or
//! deleted in the CurseForge app, mods dropped into a mods folder, a new
//! screenshot) so the desktop app can update without a manual refresh.
//!
//! Cheap by design: the instances folders are watched one level deep (new,
//! renamed and removed instance folders), and only the instance open in the
//! Library gets watches on its content folders, each one level deep. Nothing
//! watches a 250-mod pack's whole tree. A folder still being installed is
//! watched in full until it goes quiet, then reported once.
//!
//! File events can be missed (network drives often send none, and Windows
//! drops them when its buffer overflows), so a light poll compares folder
//! stamps as a safety net: every few seconds for network drives, once a
//! minute otherwise, and at once when the window comes back into focus
//! ([`DiskWatch::check`]).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant, SystemTime};

use notify::{RecursiveMode, Watcher};

/// A content folder of an instance that a tab lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Area {
    Mods,
    ResourcePacks,
    ShaderPacks,
    Worlds,
    Screenshots,
}

impl Area {
    pub const ALL: [Area; 5] = [Area::Mods, Area::ResourcePacks, Area::ShaderPacks, Area::Worlds, Area::Screenshots];

    /// The folder's name inside the instance.
    pub fn dir(self) -> &'static str {
        match self {
            Area::Mods => "mods",
            Area::ResourcePacks => "resourcepacks",
            Area::ShaderPacks => "shaderpacks",
            Area::Worlds => "saves",
            Area::Screenshots => "screenshots",
        }
    }

    fn from_dir(name: &str) -> Option<Area> {
        Area::ALL.into_iter().find(|a| a.dir().eq_ignore_ascii_case(name))
    }
}

/// What changed, for the app to act on.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Change {
    /// An instance folder appeared (and has finished being written) or its
    /// CurseForge file changed: load it again, or add it.
    Instance(PathBuf),
    /// An instance folder is gone (deleted or renamed away).
    Gone(PathBuf),
    /// Files in one of the open instance's content folders changed.
    Content(PathBuf, Area),
}

/// One raw sighting, before debouncing decides what it means.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Seen {
    /// Something happened to a folder directly under an instances folder,
    /// or to its CurseForge file.
    Folder(PathBuf),
    /// Something changed in a content folder of an instance.
    Content(PathBuf, Area),
}

/// CurseForge's file in an instance folder; it's written when an install
/// starts and again whenever the CurseForge app changes the pack.
const CF_FILE: &str = "minecraftinstance.json";

/// Whether a file or folder name is one Hopper writes itself (backups,
/// staging, its own instance file, temporary copies) or one no tab shows.
/// Changes to these never count, or Hopper's own saves would make it
/// reload what it just wrote, over and over.
pub fn ignored(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with(".hopper")
        || n.starts_with("hopper.json")
        || n == "hopper-bisect.json"
        || n.ends_with(".tmp")
        || n.ends_with(".part")
        || n.ends_with(".crdownload")
        || n.ends_with('~')
        || n == "desktop.ini"
        || n == "thumbs.db"
}

/// What a changed path means, given the watched instances folders and the
/// instance open in the Library. `None` for anything that doesn't matter.
pub fn classify(path: &Path, roots: &[PathBuf], open: Option<&Path>, installing: &HashSet<PathBuf>) -> Option<Seen> {
    // The open instance first: it may live outside every instances folder
    // (another launcher's).
    if let Some(open) = open
        && let Ok(rel) = path.strip_prefix(open)
    {
        let mut parts = rel.components().map(|c| c.as_os_str().to_string_lossy());
        if let Some(first) = parts.next() {
            if ignored(&first) {
                return None;
            }
            if let Some(area) = Area::from_dir(&first) {
                // The folder itself (created, removed) or an entry directly
                // in it; nothing deeper (a world's region files).
                return match parts.next() {
                    Some(name) if ignored(&name) || parts.next().is_some() => None,
                    _ => Some(Seen::Content(open.to_path_buf(), area)),
                };
            }
            if first.eq_ignore_ascii_case(CF_FILE) && parts.next().is_none() {
                return Some(Seen::Folder(open.to_path_buf()));
            }
        }
    }
    for root in roots {
        let Ok(rel) = path.strip_prefix(root) else { continue };
        let parts: Vec<_> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
        let Some(first) = parts.first() else { return None };
        if ignored(first) {
            return None;
        }
        let folder = root.join(first);
        return match parts.len() {
            1 => Some(Seen::Folder(folder)),
            // A folder being installed: any write inside keeps it busy.
            _ if installing.contains(&folder) => {
                if parts.iter().any(|p| ignored(p)) {
                    None
                } else {
                    Some(Seen::Folder(folder))
                }
            }
            2 if parts[1].eq_ignore_ascii_case(CF_FILE) => Some(Seen::Folder(folder)),
            _ => None,
        };
    }
    None
}

/// Whether a folder holds an instance (CurseForge's file or Hopper's).
pub fn is_instance(dir: &Path) -> bool {
    dir.join(CF_FILE).is_file() || dir.join(crate::instances::HOPPER_FILE).is_file()
}

/// How long a folder has to be left alone before it counts as settled. An
/// install writes for minutes; reporting it halfway would load a pack with
/// half its jars.
pub const FOLDER_QUIET: Duration = Duration::from_millis(2500);
/// How long content changes gather before they're reported.
pub const CONTENT_QUIET: Duration = Duration::from_millis(600);
/// Content that keeps changing (a burst of screenshots, a big copy) is
/// still reported this often.
pub const CONTENT_MAX_WAIT: Duration = Duration::from_secs(3);

/// Gathers sightings until each key has been quiet long enough.
#[derive(Default, Debug)]
pub struct Debounce {
    /// Key → (first seen, last seen).
    folders: HashMap<PathBuf, Instant>,
    content: HashMap<(PathBuf, Area), (Instant, Instant)>,
}

impl Debounce {
    pub fn push(&mut self, seen: Seen, now: Instant) {
        match seen {
            Seen::Folder(p) => {
                self.folders.insert(p, now);
            }
            Seen::Content(root, area) => {
                self.content.entry((root, area)).and_modify(|e| e.1 = now).or_insert((now, now));
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.folders.is_empty() && self.content.is_empty()
    }

    /// Whether a folder is waiting to settle.
    pub fn has_folder(&self, p: &Path) -> bool {
        self.folders.contains_key(p)
    }

    /// The sightings that have settled, taken out.
    pub fn due(&mut self, now: Instant) -> Vec<Seen> {
        let mut out = Vec::new();
        self.folders.retain(|p, last| {
            let done = now.duration_since(*last) >= FOLDER_QUIET;
            if done {
                out.push(Seen::Folder(p.clone()));
            }
            !done
        });
        self.content.retain(|(root, area), (first, last)| {
            let done = now.duration_since(*last) >= CONTENT_QUIET || now.duration_since(*first) >= CONTENT_MAX_WAIT;
            if done {
                out.push(Seen::Content(root.clone(), *area));
            }
            !done
        });
        out
    }

    /// How long until the next sighting could settle.
    pub fn next_due(&self, now: Instant) -> Option<Duration> {
        let f = self.folders.values().map(|last| (*last + FOLDER_QUIET).saturating_duration_since(now));
        let c = self.content.values().map(|(first, last)| {
            (*last + CONTENT_QUIET).min(*first + CONTENT_MAX_WAIT).saturating_duration_since(now)
        });
        f.chain(c).min()
    }
}

/// What a settled folder sighting means now: it's gone, it's an instance
/// (new or changed), or it's no instance (yet).
pub fn resolve(folder: &Path) -> Option<Change> {
    if !folder.exists() {
        // An instances folder that can't be read (a network drive dropped)
        // isn't every instance being deleted.
        let parent_there = folder.parent().is_some_and(|p| std::fs::read_dir(p).is_ok());
        return parent_there.then(|| Change::Gone(folder.to_path_buf()));
    }
    (folder.is_dir() && is_instance(folder)).then(|| Change::Instance(folder.to_path_buf()))
}

/// A file's modified time and size, to see it change.
type Stamp = Option<(SystemTime, u64)>;

fn stamp(p: &Path) -> Stamp {
    let m = std::fs::metadata(p).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// Folder stamps for the poll: what each instances folder holds (and each
/// instance's CurseForge file), and the open instance's content folders.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Snapshot {
    /// Instances folder → its folders → their CurseForge file's stamp.
    /// Missing when the instances folder couldn't be read.
    roots: HashMap<PathBuf, HashMap<PathBuf, Stamp>>,
    /// The open instance's content folders → their own stamp (a folder's
    /// modified time moves when an entry is added, removed or renamed).
    content: HashMap<(PathBuf, Area), Stamp>,
}

impl Snapshot {
    pub fn take(roots: &[PathBuf], open: Option<&Path>) -> Snapshot {
        let mut s = Snapshot::default();
        for root in roots {
            let Ok(rd) = std::fs::read_dir(root) else { continue };
            let folders = rd
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .filter(|e| !ignored(&e.file_name().to_string_lossy()))
                .map(|e| {
                    let p = e.path();
                    let st = stamp(&p.join(CF_FILE));
                    (p, st)
                })
                .collect();
            s.roots.insert(root.clone(), folders);
        }
        if let Some(open) = open {
            for a in Area::ALL {
                s.content.insert((open.to_path_buf(), a), stamp(&open.join(a.dir())));
            }
        }
        s
    }

    /// What differs from an older snapshot. An instances folder missing from
    /// either side (unreadable) is skipped rather than read as emptied.
    pub fn diff(&self, old: &Snapshot) -> Vec<Seen> {
        let mut out = Vec::new();
        for (root, now) in &self.roots {
            let Some(before) = old.roots.get(root) else { continue };
            for (p, st) in now {
                if before.get(p) != Some(st) {
                    out.push(Seen::Folder(p.clone()));
                }
            }
            for p in before.keys().filter(|p| !now.contains_key(*p)) {
                out.push(Seen::Folder(p.clone()));
            }
        }
        for (key, st) in &self.content {
            if old.content.get(key).is_some_and(|b| b != st) {
                out.push(Seen::Content(key.0.clone(), key.1));
            }
        }
        out
    }

    fn knows(&self, folder: &Path) -> bool {
        folder.parent().and_then(|r| self.roots.get(r)).is_some_and(|f| f.contains_key(folder))
    }
}

/// How often the poll runs for instances folders on a network drive, which
/// may send no file events at all.
pub const REMOTE_POLL: Duration = Duration::from_secs(5);
/// How often it runs otherwise, in case events were missed.
pub const LOCAL_POLL: Duration = Duration::from_secs(60);

enum Cmd {
    Roots(Vec<PathBuf>),
    Open(Option<PathBuf>),
    Check,
    Event(notify::Result<notify::Event>),
}

/// The watcher thread's handle. Dropping it stops the thread.
pub struct DiskWatch {
    tx: Sender<Cmd>,
}

impl DiskWatch {
    /// Starts watching; `on_change` gets each batch of settled changes, on
    /// the watcher's thread.
    pub fn start(on_change: impl Fn(Vec<Change>) + Send + 'static) -> DiskWatch {
        let (tx, rx) = channel();
        let events = tx.clone();
        std::thread::Builder::new()
            .name("disk-watch".into())
            .spawn(move || run(rx, events, on_change))
            .expect("starting the disk watcher");
        DiskWatch { tx }
    }

    /// The instances folders to watch for added and removed instances.
    pub fn set_roots(&self, roots: Vec<PathBuf>) {
        let _ = self.tx.send(Cmd::Roots(roots));
    }

    /// The instance open in the Library, whose content folders are watched.
    pub fn set_open(&self, root: Option<PathBuf>) {
        let _ = self.tx.send(Cmd::Open(root));
    }

    /// Looks for changes now (the window came back into focus).
    pub fn check(&self) {
        let _ = self.tx.send(Cmd::Check);
    }
}

struct State {
    roots: Vec<PathBuf>,
    open: Option<PathBuf>,
    remote: bool,
    snap: Snapshot,
    polled: Instant,
    debounce: Debounce,
    /// New folders that aren't instances yet: being installed, watched in
    /// full until they settle.
    installing: HashSet<PathBuf>,
    watcher: Option<notify::RecommendedWatcher>,
    watched: Vec<PathBuf>,
}

impl State {
    /// Watches what the roots and the open instance need, and nothing else.
    fn rewatch(&mut self) {
        let Some(w) = self.watcher.as_mut() else { return };
        for p in self.watched.drain(..) {
            let _ = w.unwatch(&p);
        }
        let mut want: Vec<(PathBuf, RecursiveMode)> = Vec::new();
        for r in &self.roots {
            want.push((r.clone(), RecursiveMode::NonRecursive));
        }
        for f in &self.installing {
            want.push((f.clone(), RecursiveMode::Recursive));
        }
        if let Some(open) = &self.open {
            want.push((open.clone(), RecursiveMode::NonRecursive));
            for a in Area::ALL {
                want.push((open.join(a.dir()), RecursiveMode::NonRecursive));
            }
        }
        for (p, mode) in want {
            if p.is_dir() && w.watch(&p, mode).is_ok() {
                self.watched.push(p);
            }
        }
    }

    fn poll_every(&self) -> Duration {
        if self.remote { REMOTE_POLL } else { LOCAL_POLL }
    }

    fn see(&mut self, seen: Seen, now: Instant) {
        // A folder the last poll didn't know is new: watch it in full until
        // it settles, in case it's an install in progress.
        if let Seen::Folder(f) = &seen
            && !self.installing.contains(f)
            && !self.snap.knows(f)
            && f.is_dir()
            && !is_instance(f)
        {
            self.installing.insert(f.clone());
            self.rewatch();
        }
        // A content folder made (or removed) after the instance was opened:
        // watch what's there now, rather than lean on its parent's events.
        if let Seen::Content(root, area) = &seen {
            let dir = root.join(area.dir());
            if dir.is_dir() != self.watched.contains(&dir) {
                self.rewatch();
            }
        }
        self.debounce.push(seen, now);
    }

    fn poll(&mut self, now: Instant) {
        let snap = Snapshot::take(&self.roots, self.open.as_deref());
        for seen in snap.diff(&self.snap) {
            self.see(seen, now);
        }
        self.snap = snap;
        self.polled = now;
    }
}

fn run(rx: Receiver<Cmd>, events: Sender<Cmd>, on_change: impl Fn(Vec<Change>)) {
    let watcher = notify::recommended_watcher(move |res| {
        let _ = events.send(Cmd::Event(res));
    })
    .map_err(|e| log::warn!("watching instance folders: {e}"))
    .ok();
    let mut st = State {
        roots: Vec::new(),
        open: None,
        remote: false,
        snap: Snapshot::default(),
        polled: Instant::now(),
        debounce: Debounce::default(),
        installing: HashSet::new(),
        watcher,
        watched: Vec::new(),
    };
    loop {
        let now = Instant::now();
        let until_poll = (st.polled + st.poll_every()).saturating_duration_since(now);
        let wait = st.debounce.next_due(now).map_or(until_poll, |d| d.min(until_poll));
        match rx.recv_timeout(wait) {
            Ok(Cmd::Roots(r)) => {
                if r != st.roots {
                    st.remote = r.iter().any(|p| crate::instance_ops::is_remote_drive(p));
                    st.roots = r;
                    st.installing.clear();
                    st.snap = Snapshot::take(&st.roots, st.open.as_deref());
                    st.polled = Instant::now();
                    st.rewatch();
                }
            }
            Ok(Cmd::Open(o)) => {
                if o != st.open {
                    st.open = o;
                    st.snap = Snapshot::take(&st.roots, st.open.as_deref());
                    st.rewatch();
                }
            }
            Ok(Cmd::Check) => st.poll(Instant::now()),
            Ok(Cmd::Event(Ok(ev))) => {
                if ev.need_rescan() {
                    st.poll(Instant::now());
                }
                // Reading a file isn't a change.
                if matches!(ev.kind, notify::EventKind::Access(_)) {
                    continue;
                }
                let now = Instant::now();
                for p in &ev.paths {
                    if let Some(seen) = classify(p, &st.roots, st.open.as_deref(), &st.installing) {
                        st.see(seen, now);
                    }
                }
            }
            Ok(Cmd::Event(Err(e))) => {
                log::debug!("file watch: {e}");
                st.poll(Instant::now());
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        let now = Instant::now();
        if now >= st.polled + st.poll_every() {
            st.poll(now);
        }
        let due = st.debounce.due(now);
        if due.is_empty() {
            continue;
        }
        let mut changes = Vec::new();
        let mut settled = false;
        for seen in due {
            match seen {
                Seen::Folder(f) => {
                    if st.installing.remove(&f) {
                        settled = true;
                    }
                    if let Some(c) = resolve(&f) {
                        changes.push(c);
                    }
                }
                Seen::Content(root, area) => changes.push(Change::Content(root, area)),
            }
        }
        if settled {
            st.rewatch();
        }
        // The poll compares against what's there now, so a change reported
        // by an event isn't reported again by the next poll.
        st.snap = Snapshot::take(&st.roots, st.open.as_deref());
        changes.dedup();
        if !changes.is_empty() {
            on_change(changes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopper-diskwatch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn hoppers_own_files_are_ignored() {
        for n in [
            ".hopper-backups",
            ".hopper-trash",
            ".hopper-pending.json",
            "hopper.json",
            "hopper.json.1234-0.tmp",
            "HOPPER.JSON",
            "hopper-bisect.json",
            "jei.jar.part",
            "download.tmp",
        ] {
            assert!(ignored(n), "{n}");
        }
        for n in ["mods", "sodium.jar", "sodium.jar.disabled", "minecraftinstance.json", "World 1", "2026-10-31_12.00.00.png"] {
            assert!(!ignored(n), "{n}");
        }
    }

    #[test]
    fn folders_under_an_instances_folder() {
        let roots = vec![p(r"C:\Instances")];
        let none = HashSet::new();
        let c = |s: &str| classify(&p(s), &roots, None, &none);
        assert_eq!(c(r"C:\Instances\New Pack"), Some(Seen::Folder(p(r"C:\Instances\New Pack"))));
        assert_eq!(
            c(r"C:\Instances\New Pack\minecraftinstance.json"),
            Some(Seen::Folder(p(r"C:\Instances\New Pack")))
        );
        // Deeper files of instances that aren't open don't matter.
        assert_eq!(c(r"C:\Instances\New Pack\mods\a.jar"), None);
        assert_eq!(c(r"C:\Instances\New Pack\hopper.json"), None);
        assert_eq!(c(r"C:\Instances\.hopper-trash"), None);
        assert_eq!(c(r"C:\Elsewhere\x"), None);
        assert_eq!(c(r"C:\Instances"), None);
    }

    #[test]
    fn writes_inside_a_folder_being_installed_keep_it_busy() {
        let roots = vec![p(r"C:\Instances")];
        let installing: HashSet<PathBuf> = [p(r"C:\Instances\New Pack")].into();
        let c = |s: &str| classify(&p(s), &roots, None, &installing);
        assert_eq!(c(r"C:\Instances\New Pack\mods\a.jar"), Some(Seen::Folder(p(r"C:\Instances\New Pack"))));
        assert_eq!(c(r"C:\Instances\New Pack\.hopper-backups\x"), None);
        assert_eq!(c(r"C:\Instances\Other\mods\a.jar"), None);
    }

    #[test]
    fn the_open_instances_content_folders() {
        let roots = vec![p(r"C:\Instances")];
        let open = p(r"C:\Instances\Pack");
        let none = HashSet::new();
        let c = |s: &str| classify(&p(s), &roots, Some(&open), &none);
        assert_eq!(c(r"C:\Instances\Pack\mods\a.jar"), Some(Seen::Content(open.clone(), Area::Mods)));
        assert_eq!(c(r"C:\Instances\Pack\mods\a.jar.disabled"), Some(Seen::Content(open.clone(), Area::Mods)));
        assert_eq!(c(r"C:\Instances\Pack\mods"), Some(Seen::Content(open.clone(), Area::Mods)));
        assert_eq!(c(r"C:\Instances\Pack\Mods\b.jar"), Some(Seen::Content(open.clone(), Area::Mods)));
        assert_eq!(c(r"C:\Instances\Pack\resourcepacks\x.zip"), Some(Seen::Content(open.clone(), Area::ResourcePacks)));
        assert_eq!(c(r"C:\Instances\Pack\shaderpacks\x.zip"), Some(Seen::Content(open.clone(), Area::ShaderPacks)));
        assert_eq!(c(r"C:\Instances\Pack\saves\World"), Some(Seen::Content(open.clone(), Area::Worlds)));
        assert_eq!(c(r"C:\Instances\Pack\screenshots\a.png"), Some(Seen::Content(open.clone(), Area::Screenshots)));
        // A running world writes region files all the time: not a change
        // to the list of worlds.
        assert_eq!(c(r"C:\Instances\Pack\saves\World\region\r.0.0.mca"), None);
        // Hopper's own files and temporary downloads.
        assert_eq!(c(r"C:\Instances\Pack\mods\a.jar.part"), None);
        assert_eq!(c(r"C:\Instances\Pack\mods\.hopper-staging"), None);
        assert_eq!(c(r"C:\Instances\Pack\hopper.json"), None);
        assert_eq!(c(r"C:\Instances\Pack\.hopper-backups\mods\a.jar"), None);
        // Game files written at launch.
        assert_eq!(c(r"C:\Instances\Pack\options.txt"), None);
        assert_eq!(c(r"C:\Instances\Pack\logs\latest.log"), None);
        // CurseForge changed the open pack.
        assert_eq!(c(r"C:\Instances\Pack\minecraftinstance.json"), Some(Seen::Folder(open.clone())));
    }

    #[test]
    fn an_open_instance_outside_the_instances_folders() {
        let open = p(r"D:\MultiMC\inst\Pack\.minecraft");
        let none = HashSet::new();
        assert_eq!(
            classify(&open.join("mods").join("a.jar"), &[p(r"C:\Instances")], Some(&open), &none),
            Some(Seen::Content(open.clone(), Area::Mods))
        );
    }

    #[test]
    fn folders_wait_to_settle_and_content_is_capped() {
        let t0 = Instant::now();
        let mut d = Debounce::default();
        d.push(Seen::Folder(p("a")), t0);
        d.push(Seen::Content(p("b"), Area::Mods), t0);
        assert!(d.due(t0 + Duration::from_millis(100)).is_empty());
        assert_eq!(d.due(t0 + CONTENT_QUIET), vec![Seen::Content(p("b"), Area::Mods)]);
        // More writes push the folder back.
        d.push(Seen::Folder(p("a")), t0 + Duration::from_secs(2));
        assert!(d.due(t0 + FOLDER_QUIET).is_empty());
        assert_eq!(d.due(t0 + Duration::from_secs(2) + FOLDER_QUIET), vec![Seen::Folder(p("a"))]);
        assert!(d.is_empty());
        // Content that never stops is reported anyway.
        let mut d = Debounce::default();
        let mut t = t0;
        let mut got = Vec::new();
        while t < t0 + Duration::from_secs(4) {
            d.push(Seen::Content(p("b"), Area::Screenshots), t);
            got.extend(d.due(t));
            t += Duration::from_millis(200);
        }
        assert!(!got.is_empty());
        assert!(d.next_due(t).is_some_and(|w| w <= CONTENT_QUIET));
    }

    #[test]
    fn resolve_reads_what_is_there() {
        let root = scratch("resolve");
        let pack = root.join("Pack");
        std::fs::create_dir_all(pack.join("mods")).unwrap();
        // Still empty: not an instance yet.
        assert_eq!(resolve(&pack), None);
        std::fs::write(pack.join(CF_FILE), "{}").unwrap();
        assert_eq!(resolve(&pack), Some(Change::Instance(pack.clone())));
        std::fs::remove_dir_all(&pack).unwrap();
        assert_eq!(resolve(&pack), Some(Change::Gone(pack.clone())));
        // An instances folder that's gone (a network drive dropped) isn't
        // its instances being deleted.
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(resolve(&pack), None);
    }

    #[test]
    fn the_poll_sees_added_removed_and_changed_folders() {
        let root = scratch("poll");
        let roots = vec![root.clone()];
        let keep = root.join("Keep");
        let gone = root.join("Gone");
        let open = keep.clone();
        for d in [&keep, &gone] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join(CF_FILE), "{}").unwrap();
        }
        std::fs::create_dir_all(keep.join("mods")).unwrap();
        let before = Snapshot::take(&roots, Some(&open));
        assert!(Snapshot::take(&roots, Some(&open)).diff(&before).is_empty());

        std::fs::remove_dir_all(&gone).unwrap();
        let new = root.join("New");
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(keep.join(CF_FILE), "{\"name\": \"changed\"}").unwrap();
        std::fs::create_dir_all(keep.join("screenshots")).unwrap();
        // Hopper's own folders don't count.
        std::fs::create_dir_all(root.join(".hopper-trash")).unwrap();
        let mut d = Snapshot::take(&roots, Some(&open)).diff(&before);
        d.sort_by_key(|s| format!("{s:?}"));
        let mut want = vec![
            Seen::Folder(gone.clone()),
            Seen::Folder(new.clone()),
            Seen::Folder(keep.clone()),
            Seen::Content(open.clone(), Area::Screenshots),
        ];
        want.sort_by_key(|s| format!("{s:?}"));
        assert_eq!(d, want);

        // An unreadable instances folder reports nothing.
        let after = Snapshot::take(&roots, None);
        std::fs::remove_dir_all(&root).unwrap();
        assert!(Snapshot::take(&roots, None).diff(&after).is_empty());
    }

    #[test]
    fn the_watcher_reports_an_install_once_it_settles_and_a_removal() {
        let root = scratch("live");
        let (tx, rx) = channel();
        let w = DiskWatch::start(move |c| {
            let _ = tx.send(c);
        });
        w.set_roots(vec![root.clone()]);
        std::thread::sleep(Duration::from_millis(300));
        let pack = root.join("Fresh Pack");
        std::fs::create_dir_all(pack.join("mods")).unwrap();
        std::fs::write(pack.join(CF_FILE), "{}").unwrap();
        // Still writing: nothing yet.
        for i in 0..3 {
            std::thread::sleep(Duration::from_millis(400));
            std::fs::write(pack.join("mods").join(format!("m{i}.jar")), "x").unwrap();
        }
        assert!(rx.try_recv().is_err(), "reported while still being written");
        let got = rx.recv_timeout(Duration::from_secs(10)).expect("install reported");
        assert_eq!(got, vec![Change::Instance(pack.clone())]);

        // Open it: a jar dropped into its mods folder is reported.
        w.set_open(Some(pack.clone()));
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(pack.join("mods").join("added.jar"), "x").unwrap();
        let got = rx.recv_timeout(Duration::from_secs(5)).expect("mods change reported");
        assert!(got.contains(&Change::Content(pack.clone(), Area::Mods)), "{got:?}");

        std::fs::remove_dir_all(&pack).unwrap();
        let mut seen_gone = false;
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end && !seen_gone {
            if let Ok(c) = rx.recv_timeout(Duration::from_millis(500)) {
                seen_gone = c.contains(&Change::Gone(pack.clone()));
            }
        }
        assert!(seen_gone, "removal reported");
        drop(w);
        let _ = std::fs::remove_dir_all(&root);
    }
}
