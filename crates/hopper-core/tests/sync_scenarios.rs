//! Friends group sync scenarios against the real mailbox
//! (`relay/hopper_mailbox.py`), with several people on one PC.
//!
//! Each test starts its own scratch mailbox on 127.0.0.1 (a free port, a
//! state file in a temp folder) with `python`, so nothing ever talks to
//! the public one. Every person reaches it through their own little proxy,
//! which is how a test turns their internet off and on: going offline
//! drops their open connections and refuses new ones, the way a pulled
//! cable or a closed tunnel does.
//!
//! A person mirrors what the desktop does with a group's pack
//! (`src/gpui/store/friends.rs`): publish the copy when its code changed
//! (stamped, marking its own changes once the mailbox numbered it), read
//! the feed (`arrive`), take the waiting codes in (`groupsync::plan`), and
//! start again from saved settings (the whole feed read once more). The
//! mods are CurseForge files: file n belongs to project n / 10, so 31 and
//! 32 are two versions of mod 3. No mod is downloaded.
//!
//! Run (Python 3 on PATH):
//!
//! ```text
//! cargo test -p hopper-core --test sync_scenarios -- --ignored --test-threads=1
//! ```
//!
//! Tests named `gap_*` describe behaviour the desktop doesn't have yet and
//! fail until it does (see `.claude/changes/sync-scenarios/card.md`).

use hopper_core as hopper;

use hopper::groupsync::{self, Got, Mods, PackCopy, State};
use hopper::mailbox::{self, Event, Failure, Kind, Mailbox};
use hopper::sharecode::{self, Base, Entry, Share};
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PACK: &str = "pack-1";

// ── the scratch mailbox ──────────────────────────────────────────────

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A scratch mailbox: its own process, port and state file.
struct Relay {
    port: u16,
    dir: PathBuf,
    child: Option<Child>,
}

impl Relay {
    fn start() -> Relay {
        let dir = std::env::temp_dir().join(format!("hopper-sync-scn-{}-{}", std::process::id(), free_port()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut r = Relay { port: free_port(), dir, child: None };
        r.up();
        r
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn script() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../relay/hopper_mailbox.py")
    }

    /// Starts the mailbox on its port with its state file (as it was).
    fn up(&mut self) {
        assert!(self.child.is_none());
        let python = std::env::var("PYTHON").unwrap_or_else(|_| "python".into());
        let child = Command::new(python)
            .arg("-B")
            .arg(Self::script())
            .env("HOPPER_MAILBOX_HOST", "127.0.0.1")
            .env("HOPPER_MAILBOX_PORT", self.port.to_string())
            .env("HOPPER_MAILBOX_STATE", self.dir.join("state.json"))
            .env("HOPPER_MAILBOX_REGISTER_LIMIT", "100000")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("python on PATH (or PYTHON=...) to run relay/hopper_mailbox.py");
        self.child = Some(child);
        let until = Instant::now() + Duration::from_secs(15);
        while Instant::now() < until {
            if let Ok(mut s) = TcpStream::connect(("127.0.0.1", self.port)) {
                let _ = s.write_all(b"GET /v1/health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
                let mut out = String::new();
                let _ = s.read_to_string(&mut out);
                if out.contains("\"ok\"") {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("the scratch mailbox didn't start");
    }

    /// The process dies at once, the way a crash or a power cut does: what
    /// it hadn't saved yet is gone.
    fn crash(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    /// Stopped after its saver had time to write (it saves about 1.5 s
    /// after a change). On Windows there is no SIGTERM to send, so this is
    /// a stop after the save rather than the service's own shutdown.
    fn stop_saved(&mut self) {
        std::thread::sleep(Duration::from_millis(2500));
        self.crash();
    }

    /// The saved state, once the saver has written the latest changes.
    fn state(&self) -> serde_json::Value {
        std::thread::sleep(Duration::from_millis(2500));
        serde_json::from_slice(&std::fs::read(self.dir.join("state.json")).unwrap_or_default()).unwrap_or_default()
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.crash();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// One person's internet: a proxy between them and the mailbox.
struct Line {
    port: u16,
    up: Arc<AtomicBool>,
    open: Arc<Mutex<Vec<TcpStream>>>,
}

impl Line {
    fn to(target: u16) -> Line {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let up = Arc::new(AtomicBool::new(true));
        let open: Arc<Mutex<Vec<TcpStream>>> = Arc::default();
        let (up2, open2) = (up.clone(), open.clone());
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(c) = c else { continue };
                if !up2.load(Ordering::SeqCst) {
                    let _ = c.shutdown(Shutdown::Both);
                    continue;
                }
                let Ok(s) = TcpStream::connect(("127.0.0.1", target)) else {
                    let _ = c.shutdown(Shutdown::Both);
                    continue;
                };
                {
                    let mut o = open2.lock().unwrap();
                    o.push(c.try_clone().unwrap());
                    o.push(s.try_clone().unwrap());
                }
                let pipe = |mut from: TcpStream, mut to: TcpStream| {
                    std::thread::spawn(move || {
                        let _ = std::io::copy(&mut from, &mut to);
                        let _ = to.shutdown(Shutdown::Write);
                    });
                };
                pipe(c.try_clone().unwrap(), s.try_clone().unwrap());
                pipe(s, c);
            }
        });
        Line { port, up, open }
    }

    fn set(&self, on: bool) {
        self.up.store(on, Ordering::SeqCst);
        if !on {
            for s in self.open.lock().unwrap().drain(..) {
                let _ = s.shutdown(Shutdown::Both);
            }
        }
    }
}

// ── one person ───────────────────────────────────────────────────────

fn cf(file: u64, on: bool) -> Entry {
    Entry::CurseFile { file, enabled: on }
}

fn project_of(e: &Entry) -> Option<u64> {
    match e {
        Entry::CurseFile { file, .. } => Some(file / 10),
        _ => None,
    }
}

fn dec(c: &str) -> Option<Mods> {
    sharecode::decode(c).ok().map(|s| groupsync::mods(&s, project_of))
}

type ModList = BTreeMap<u64, (u64, bool)>;

/// The pack everyone starts from: mods 1 to 6, on.
fn base() -> ModList {
    (1..=6).map(|p| (p, (p * 10 + 1, true))).collect()
}

/// What a test checks a person against.
fn list(items: &[(u64, u64, bool)]) -> ModList {
    items.iter().map(|(p, f, on)| (*p, (*f, *on))).collect()
}

struct Pc {
    name: String,
    mb: Mailbox,
    line: Line,
    sharer: bool,
    group: String,
    mods: ModList,
    loader: String,
    copy: PackCopy,
    /// The feed's newest number read (the desktop's `mailbox_seq`).
    cursor: u64,
    /// Every event read, for what the window would say.
    seen: Vec<Event>,
    /// Merges that changed something.
    merges: Vec<Vec<groupsync::Change>>,
}

impl Pc {
    fn new(relay: &Relay, name: &str) -> Pc {
        let line = Line::to(relay.port);
        let mb = Mailbox::register(&format!("http://127.0.0.1:{}", line.port), name).unwrap();
        Pc {
            name: name.into(),
            mb,
            line,
            sharer: false,
            group: String::new(),
            mods: base(),
            loader: "neoforge-21.1.200".into(),
            copy: PackCopy::default(),
            cursor: 0,
            seen: Vec::new(),
            merges: Vec::new(),
        }
    }

    fn id(&self) -> String {
        self.mb.account().id.clone()
    }

    fn offline(&self) {
        self.line.set(false);
    }

    fn online(&self) {
        self.line.set(true);
    }

    fn share(&self) -> Share {
        Share {
            base: Base::Plain { game_version: "1.21.1".into(), loader: self.loader.clone() },
            name: None,
            loader: None,
            entries: self.mods.values().map(|(f, on)| cf(*f, *on)).collect(),
        }
    }

    fn code(&self) -> String {
        sharecode::encode(&self.share())
    }

    fn here(&self) -> Mods {
        groupsync::mods(&self.share(), project_of)
    }

    /// Sets the copy up the way the desktop does once the pack is shared
    /// into the group (sharer) or followed through it (member).
    fn join_copy(&mut self, group: &str, sharer: bool) {
        self.group = group.into();
        self.sharer = sharer;
        let local = if sharer { PACK.to_string() } else { format!("{}-local", self.name) };
        self.copy = PackCopy { inst: PACK.into(), group: group.into(), local, ..Default::default() };
    }

    /// Publishes the copy when its code changed since the last publish
    /// (`publish_work` / `PublishWork::run` / `publish_done`). Err: it
    /// didn't reach the mailbox, and goes on the next try.
    fn publish(&mut self) -> anyhow::Result<bool> {
        let now = self.code();
        if now == self.copy.sent {
            return Ok(false);
        }
        let now_mods = self.here();
        let sent = dec(&self.copy.sent);
        let stamps = groupsync::stamps_out(&self.copy.marks, sent.as_ref(), &now_mods, &self.id());
        let key = if self.sharer { format!("{}-folder", self.name) } else { groupsync::COPY_KEY.to_string() };
        let r = self.mb.publish_to(&[], &[self.group.clone()], PACK, &key, "Test pack", &now, &[], &stamps)?;
        if r.skipped.contains(&self.group) {
            // The group isn't this PC's any more; the desktop drops the copy
            // once it reads the groups.
            return Ok(false);
        }
        let mut marks = self.copy.marks.clone();
        groupsync::mark_own(&mut marks, sent.as_ref(), &now_mods, &self.id(), r.seq, r.seq);
        for (k, m) in marks {
            if self.copy.marks.get(&k).is_none_or(|x| x.seq <= m.seq && x.state != m.state) {
                self.copy.marks.insert(k, m);
            }
        }
        self.copy.sent = now;
        self.copy.sent_seq = r.seq;
        Ok(true)
    }

    /// Reads the feed after the cursor (`events`, `newest_codes_only`,
    /// `got_recipe` / `got_copy`).
    fn read(&mut self) -> anyhow::Result<usize> {
        let (seq, events) = self.mb.events(self.cursor, 0)?;
        self.cursor = self.cursor.max(seq);
        let mut latest: HashMap<(String, String), u64> = HashMap::new();
        for e in events.iter().filter(|e| e.kind() == Kind::Recipe) {
            latest.insert((e.from.clone(), e.inst.clone()), e.seq);
        }
        let n = events.len();
        for e in events {
            if e.kind() == Kind::Recipe && latest.get(&(e.from.clone(), e.inst.clone())) != Some(&e.seq) {
                continue;
            }
            if e.kind() == Kind::Recipe && e.inst == PACK && e.from != self.id() && !self.copy.group.is_empty() {
                // The sharer's own copy takes members' copies; a member's
                // copy takes the sharer's code and every member's copy.
                if e.key == groupsync::COPY_KEY || !self.sharer {
                    self.copy.arrive(&e.from, Got { code: e.code.clone(), seq: e.seq, at: e.at, name: e.from_name.clone(), stamps: e.marks.clone() });
                }
            }
            self.seen.push(e);
        }
        Ok(n)
    }

    /// Takes the waiting codes in (`groupsync::merge` / `on_group_merged`).
    fn merge(&mut self) {
        if self.copy.waiting.is_empty() {
            return;
        }
        let here = self.here();
        let sent = dec(&self.copy.sent);
        let (todo, merged) = groupsync::plan(&self.copy, &here, sent.as_ref(), false, dec);
        for c in &todo {
            if c.key == groupsync::LOADER_KEY {
                if let groupsync::LoaderStep::Set(l) = groupsync::loader_step(&self.loader, c, None) {
                    self.loader = l;
                }
                continue;
            }
            let p: u64 = c.key.trim_start_matches("p:").parse().unwrap();
            match &c.now {
                State::File { file, on } => {
                    let f: u64 = file.trim_start_matches("cf:").parse().unwrap();
                    self.mods.insert(p, (f, *on));
                }
                _ => {
                    self.mods.remove(&p);
                }
            }
        }
        for (from, g) in merged.taken {
            if self.copy.waiting.get(&from).is_some_and(|w| w.seq == g.seq) {
                self.copy.waiting.remove(&from);
            }
            self.copy.got.insert(from, g);
        }
        for (k, m) in merged.marks {
            if self.copy.marks.get(&k).is_none_or(|x| x.seq <= m.seq) {
                self.copy.marks.insert(k, m);
            }
        }
        if !todo.is_empty() {
            self.merges.push(todo);
        }
    }

    /// One turn of a PC that is on: read, take in, send what changed.
    fn tick(&mut self) {
        self.read().unwrap();
        self.merge();
        self.publish().unwrap();
    }

    /// Hopper closed and opened again: the copy as saved in settings, and
    /// the whole feed read from the start.
    fn restart(&mut self) {
        self.copy = serde_json::from_str(&serde_json::to_string(&self.copy).unwrap()).unwrap();
        self.cursor = 0;
        self.seen.clear();
    }

    /// Who the row of mod `p` names ("Turned off by A").
    fn who(&self, p: u64) -> String {
        self.copy.marks.get(&format!("p:{p}")).map(|m| m.kind.says(&m.name)).unwrap_or_default()
    }

    fn revokes(&self) -> Vec<(String, String)> {
        self.seen.iter().filter(|e| e.kind() == Kind::Revoke).map(|e| (e.inst.clone(), e.reason.clone())).collect()
    }
}

fn befriend(a: &Pc, b: &Pc) {
    let (code, _) = a.mb.invite().unwrap();
    b.mb.join(&code).unwrap();
}

/// A shares the pack into a new group with B and C (and any more), who say
/// yes and follow it: each starts from A's code and publishes its copy,
/// and everyone takes in the others' first codes (no change anywhere).
fn group_of(relay: &Relay, names: &[&str]) -> (String, Vec<Pc>) {
    let mut pcs: Vec<Pc> = names.iter().map(|n| Pc::new(relay, n)).collect();
    for i in 1..pcs.len() {
        befriend(&pcs[0], &pcs[i]);
    }
    let members: Vec<String> = pcs[1..].iter().map(|p| p.id()).collect();
    let g = pcs[0].mb.create_group("Crew", &members).unwrap();
    for p in &pcs[1..] {
        p.mb.accept_group(&g.id).unwrap();
    }
    pcs[0].join_copy(&g.id, true);
    pcs[0].publish().unwrap();
    for p in pcs.iter_mut().skip(1) {
        p.join_copy(&g.id, false);
        p.publish().unwrap();
    }
    for p in pcs.iter_mut() {
        p.read().unwrap();
        p.merge();
        assert!(p.merges.is_empty(), "{}: a first code changed something", p.name);
    }
    (g.id, pcs)
}

/// A's six changes from the user's example: two mods added, one removed,
/// two updated, one turned off.
fn six_changes(a: &mut Pc) {
    a.mods.insert(7, (71, true));
    a.mods.insert(8, (81, true));
    a.mods.remove(&2);
    a.mods.insert(3, (32, true));
    a.mods.insert(5, (52, true));
    a.mods.insert(4, (41, false));
}

fn after_six() -> ModList {
    list(&[(1, 11, true), (3, 32, true), (4, 41, false), (5, 52, true), (6, 61, true), (7, 71, true), (8, 81, true)])
}

fn queued_from(relay: &Relay, to: &Pc, from: &Pc) -> usize {
    let st = relay.state();
    st["events"][to.id()].as_array().map_or(0, |q| q.iter().filter(|e| e["from"] == from.id().as_str() && e["kind"] == "recipe").count())
}

// ── scenarios ────────────────────────────────────────────────────────

/// S1. Everyone online: A changes six mods, B and C get all six, each row
/// says A did it, and their echoes change nothing.
#[test]
#[ignore = "starts relay/hopper_mailbox.py; run with --ignored"]
fn s01_six_changes_reach_everyone_online() {
    let relay = Relay::start();
    let (_, mut pcs) = group_of(&relay, &["A", "B", "C"]);
    six_changes(&mut pcs[0]);
    assert!(pcs[0].publish().unwrap());
    for i in 1..3 {
        pcs[i].tick();
        assert_eq!(pcs[i].mods, after_six(), "{} has all six", pcs[i].name);
        assert_eq!(pcs[i].merges.len(), 1, "one batch");
        assert_eq!(pcs[i].merges[0].len(), 6);
        assert_eq!(pcs[i].who(4), "Turned off by A");
        assert_eq!(pcs[i].who(3), "Updated by A");
        assert_eq!(pcs[i].who(7), "Added by A");
        assert_eq!(pcs[i].who(2), "Removed by A");
    }
    // The echoes reach A and each other and change nothing.
    for _ in 0..2 {
        for p in pcs.iter_mut() {
            p.tick();
        }
    }
    assert!(pcs[0].merges.is_empty(), "A got nothing back");
    for p in &pcs {
        assert_eq!(p.mods, after_six(), "{}", p.name);
    }
}

/// S2. The user's example: A changes six, it goes out, A goes offline.
/// B and C were offline the whole time and come back while A still is.
#[test]
#[ignore = "starts relay/hopper_mailbox.py; run with --ignored"]
fn s02_six_changes_then_everyone_offline_members_catch_up_without_the_sharer() {
    let relay = Relay::start();
    let (_, mut pcs) = group_of(&relay, &["A", "B", "C"]);
    pcs[1].offline();
    pcs[2].offline();
    six_changes(&mut pcs[0]);
    pcs[0].publish().unwrap();
    pcs[0].offline();
    // Hopper closed on B and C meanwhile, too.
    pcs[1].restart();
    pcs[2].restart();
    pcs[1].online();
    pcs[2].online();
    for i in 1..3 {
        pcs[i].tick();
        assert_eq!(pcs[i].mods, after_six(), "{} got all six with A offline", pcs[i].name);
    }
    // Their copies went out; B and C agree, and A gets nothing new back.
    pcs[1].tick();
    pcs[2].tick();
    assert_eq!(pcs[1].mods, pcs[2].mods);
    pcs[0].online();
    pcs[0].tick();
    assert!(pcs[0].merges.is_empty());
}

/// S3. A changes mods while offline: nothing goes out (the window says
/// it can't connect); B comes on and goes again in between and sees
/// nothing; A's next try once online sends all six, and B gets them later.
#[test]
#[ignore = "starts relay/hopper_mailbox.py; run with --ignored"]
fn s03_changes_made_offline_go_out_on_reconnect() {
    let relay = Relay::start();
    let (_, mut pcs) = group_of(&relay, &["A", "B", "C"]);
    pcs[0].offline();
    six_changes(&mut pcs[0]);
    let e = pcs[0].publish().unwrap_err();
    assert_eq!(mailbox::failure(&e), Failure::Unreachable);
    assert_eq!(mailbox::says(&e), mailbox::CANT_CONNECT);
    // Hopper closed and opened again while still offline: the change is
    // still unsent (the copy's `sent` is the old code).
    pcs[0].restart();
    assert!(pcs[0].publish().is_err());
    // B comes on, sees nothing new, and goes.
    pcs[1].tick();
    assert_eq!(pcs[1].mods, base());
    pcs[1].offline();
    pcs[0].online();
    assert!(pcs[0].publish().unwrap(), "the unsent change goes out once online");
    pcs[2].tick();
    assert_eq!(pcs[2].mods, after_six());
    pcs[1].online();
    pcs[1].tick();
    assert_eq!(pcs[1].mods, after_six());
}

/// S4. Two-way: B changes mods while A (the sharer) is offline. C gets
/// them at once; A gets them on reconnect, each row naming B.
#[test]
#[ignore = "starts relay/hopper_mailbox.py; run with --ignored"]
fn s04_a_members_changes_reach_the_sharer_on_reconnect() {
    let relay = Relay::start();
    let (_, mut pcs) = group_of(&relay, &["A", "B", "C"]);
    pcs[0].offline();
    pcs[1].mods.insert(1, (11, false));
    pcs[1].mods.insert(6, (62, true));
    pcs[1].mods.insert(9, (91, true));
    pcs[1].publish().unwrap();
    pcs[2].tick();
    let want = list(&[(1, 11, false), (2, 21, true), (3, 31, true), (4, 41, true), (5, 51, true), (6, 62, true), (9, 91, true)]);
    assert_eq!(pcs[2].mods, want);
    assert_eq!(pcs[2].who(1), "Turned off by B");
    pcs[0].restart();
    pcs[0].online();
    pcs[0].tick();
    assert_eq!(pcs[0].mods, want);
    assert_eq!(pcs[0].who(6), "Updated by B");
    assert_eq!(pcs[0].who(9), "Added by B");
}

/// Both change while offline: A updates mod 1 and turns 3 off, B turns 1
/// off and adds 9. `a_first`: A comes back first.
fn both_offline(a_first: bool) -> (Relay, Vec<Pc>) {
    let relay = Relay::start();
    let (_, mut pcs) = group_of(&relay, &["A", "B", "C"]);
    pcs[0].offline();
    pcs[1].offline();
    // B changes first...
    pcs[1].mods.insert(1, (11, false));
    pcs[1].mods.insert(9, (91, true));
    // ...and A a minute later.
    pcs[0].mods.insert(1, (12, true));
    pcs[0].mods.insert(3, (31, false));
    let (first, second) = if a_first { (0, 1) } else { (1, 0) };
    pcs[first].online();
    pcs[first].tick();
    pcs[second].online();
    pcs[second].tick();
    for _ in 0..3 {
        for p in pcs.iter_mut() {
            p.tick();
        }
    }
    (relay, pcs)
}

/// S5. Both offline, A back first: they end the same everywhere, the
/// different mods both land, and mod 1 is the one changed by whoever came
/// back last (B), named on every row.
#[test]
#[ignore = "starts relay/hopper_mailbox.py; run with --ignored"]
fn s05_both_offline_a_back_first_ends_the_same_everywhere() {
    let (_relay, pcs) = both_offline(true);
    for p in &pcs {
        assert_eq!(p.mods, pcs[0].mods, "{} differs", p.name);
        assert_eq!(p.mods[&3], (31, false), "A's other change landed on {}", p.name);
        assert_eq!(p.mods[&9], (91, true), "B's other change landed on {}", p.name);
    }
    // Today: the last one to reach the mailbox wins mod 1.
    assert_eq!(pcs[0].mods[&1], (11, false));
    assert_eq!(pcs[2].who(1), "Turned off by B");
    assert_eq!(pcs[0].who(1), "Turned off by B", "A's row says B changed it");
}

/// S5b. Both offline, B back first: A, back last, wins mod 1.
#[test]
#[ignore = "starts relay/hopper_mailbox.py; run with --ignored"]
fn s05b_both_offline_b_back_first_ends_the_same_everywhere() {
    let (_relay, pcs) = both_offline(false);
    for p in &pcs {
        assert_eq!(p.mods, pcs[0].mods, "{} differs", p.name);
        assert_eq!((p.mods[&3], p.mods[&9]), ((31, false), (91, true)));
    }
    assert_eq!(pcs[0].mods[&1], (12, true));
    assert_eq!(pcs[1].who(1), "Updated by A");
}

/// GAP. "Latest change wins" means the latest to reach the mailbox, not the
/// latest made: A changed mod 1 after B, but B came back online later and
/// B's older change won everywhere, with no word to A that theirs was
/// replaced. Fails until a change carries when it was made.
#[test]
#[ignore = "gap: offline changes are ordered by reconnect, not by when they were made"]
fn gap_the_change_made_last_wins_whoever_comes_back_first() {
    let (_relay, pcs) = both_offline(true);
    assert_eq!(pcs[2].mods[&1], (12, true), "A changed mod 1 last, so A's update should win on C");
}

/// S6. A member offline through several changes in a row (six, two
/// more, then one of them undone): the mailbox keeps one code of A's
/// for them, and they get the final state, never a broken one between.
#[test]
#[ignore = "starts relay/hopper_mailbox.py; run with --ignored"]
fn s06_a_row_of_changes_while_away_arrives_as_the_final_state() {
    let relay = Relay::start();
    let (_, mut pcs) = group_of(&relay, &["A", "B", "C"]);
    pcs[1].offline();
    six_changes(&mut pcs[0]);
    pcs[0].publish().unwrap();
    pcs[2].tick();
    pcs[0].mods.insert(6, (62, true));
    pcs[0].mods.insert(1, (11, false));
    pcs[0].publish().unwrap();
    pcs[2].tick();
    // Undo: mod 3 back to the old file.
    pcs[0].mods.insert(3, (31, true));
    pcs[0].publish().unwrap();
    pcs[2].tick();
    // C's copy went out each time; A takes those echoes in (nothing).
    pcs[0].tick();
    assert_eq!(queued_from(&relay, &pcs[1], &pcs[0]), 1, "one code of A's waits for B");
    let want = list(&[(1, 11, false), (3, 31, true), (4, 41, false), (5, 52, true), (6, 62, true), (7, 71, true), (8, 81, true)]);
    assert_eq!(pcs[2].mods, want);
    pcs[1].restart();
    pcs[1].online();
    pcs[1].tick();
    assert_eq!(pcs[1].mods, want, "B has the final state");
    assert_eq!(pcs[1].merges.len(), 1, "in one batch");
}

/// S6b. B took the first six, then went away; A undoes one of them: B
/// gets the undo (the code says the change was made in it).
#[test]
#[ignore = "starts relay/hopper_mailbox.py; run with --ignored"]
fn s06b_an_undo_while_a_member_is_away_reaches_them() {
    let relay = Relay::start();
    let (_, mut pcs) = group_of(&relay, &["A", "B", "C"]);
    six_changes(&mut pcs[0]);
    pcs[0].publish().unwrap();
    pcs[1].tick();
    assert_eq!(pcs[1].mods, after_six());
    pcs[1].offline();
    pcs[0].tick();
    pcs[0].mods.insert(3, (31, true));
    pcs[0].mods.insert(2, (21, true));
    pcs[0].publish().unwrap();
    pcs[1].online();
    pcs[1].tick();
    assert_eq!((pcs[1].mods[&3], pcs[1].mods.get(&2).copied()), ((31, true), Some((21, true))));
}

/// S7. Someone joins after the changes: the group hands them A's latest
/// code and B's (they follow A's code, so only B's stamped changes are
/// new to them), and the joiner ends like everyone else.
#[test]
#[ignore = "starts relay/hopper_mailbox.py; run with --ignored"]
fn s07_a_late_joiner_gets_the_latest_pack() {
    let relay = Relay::start();
    let (gid, mut pcs) = group_of(&relay, &["A", "B"]);
    six_changes(&mut pcs[0]);
    pcs[0].publish().unwrap();
    pcs[1].tick();
    pcs[1].mods.insert(6, (61, false));
    pcs[1].publish().unwrap();
    pcs[0].tick();
    let mut d = Pc::new(&relay, "D");
    befriend(&pcs[0], &d);
    pcs[0].mb.edit_group(&gid, None, &[d.id()], &[]).unwrap();
    d.mb.accept_group(&gid).unwrap();
    // The follow installs A's latest code (from the group).
    let (_, evs) = d.mb.events(0, 0).unwrap();
    let a_code = evs.iter().find(|e| e.kind() == Kind::Recipe && e.from == pcs[0].id()).expect("D gets A's pack").code.clone();
    let s = sharecode::decode(&a_code).unwrap();
    d.mods = s.entries.iter().map(|e| match e { Entry::CurseFile { file, enabled } => (file / 10, (*file, *enabled)), _ => unreachable!() }).collect();
    d.join_copy(&gid, false);
    d.tick();
    assert_eq!(d.mods, pcs[0].mods);
    assert_eq!(d.mods[&6], (61, false), "B's change is in A's code already");
}

/// S8. C is removed from the group while offline. Back online, C hears
/// the pack isn't shared with them any more; what C changed meanwhile
/// stays on C's PC and doesn't reach the others.
#[test]
#[ignore = "starts relay/hopper_mailbox.py; run with --ignored"]
fn s08_removed_while_offline() {
    let relay = Relay::start();
    let (gid, mut pcs) = group_of(&relay, &["A", "B", "C"]);
    pcs[2].offline();
    pcs[2].mods.insert(1, (11, false));
    pcs[0].mb.edit_group(&gid, None, &[], &[pcs[2].id()]).unwrap();
    pcs[2].online();
    pcs[2].read().unwrap();
    assert!(pcs[2].revokes().contains(&(PACK.to_string(), "left_group".to_string())), "C is told: {:?}", pcs[2].revokes());
    assert!(pcs[2].mb.groups().unwrap().iter().all(|g| g.id != gid));
    assert!(!pcs[2].publish().unwrap(), "C's copy has nowhere to go");
    pcs[0].tick();
    pcs[1].tick();
    assert_eq!(pcs[0].mods[&1], (11, true));
    // A and B were told C's copy is gone.
    assert!(pcs[1].revokes().iter().any(|(i, r)| i == PACK && r == "left_group"));
}

/// S9. A friend removed (A removes B): B leaves A's group, gets a revoke
/// for the pack and an `unfriended` event.
#[test]
#[ignore = "starts relay/hopper_mailbox.py; run with --ignored"]
fn s09_unfriended_while_offline() {
    let relay = Relay::start();
    let (gid, mut pcs) = group_of(&relay, &["A", "B", "C"]);
    pcs[1].offline();
    pcs[0].mb.remove(&pcs[1].id()).unwrap();
    pcs[1].online();
    pcs[1].read().unwrap();
    assert!(pcs[1].seen.iter().any(|e| e.kind() == Kind::Unfriended));
    assert!(pcs[1].revokes().contains(&(PACK.to_string(), "left_group".to_string())));
    assert!(pcs[1].mb.groups().unwrap().iter().all(|g| g.id != gid));
}

/// S10. The group deleted, and S11 the pack unshared, while members are
/// offline: each member gets a revoke for each group pack on return, and
/// someone who joins later gets nothing from an unshared pack.
#[test]
#[ignore = "starts relay/hopper_mailbox.py; run with --ignored"]
fn s10_group_deleted_while_offline() {
    let relay = Relay::start();
    let (gid, mut pcs) = group_of(&relay, &["A", "B", "C"]);
    pcs[1].offline();
    pcs[0].mb.delete_group(&gid).unwrap();
    pcs[1].online();
    pcs[1].read().unwrap();
    assert!(pcs[1].revokes().contains(&(PACK.to_string(), "left_group".to_string())));
    assert!(pcs[1].mb.groups().unwrap().is_empty());
}

#[test]
#[ignore = "starts relay/hopper_mailbox.py; run with --ignored"]
fn s11_pack_unshared_while_offline() {
    let relay = Relay::start();
    let (gid, mut pcs) = group_of(&relay, &["A", "B", "C"]);
    pcs[1].offline();
    assert!(pcs[0].mb.revoke(PACK, &[], &[gid.clone()], "unshared").unwrap());
    pcs[1].online();
    pcs[1].read().unwrap();
    assert!(pcs[1].revokes().contains(&(PACK.to_string(), "unshared".to_string())));
    let mut d = Pc::new(&relay, "D");
    befriend(&pcs[0], &d);
    pcs[0].mb.edit_group(&gid, None, &[d.id()], &[]).unwrap();
    d.mb.accept_group(&gid).unwrap();
    d.read().unwrap();
    assert!(!d.seen.iter().any(|e| e.kind() == Kind::Recipe && e.from == pcs[0].id()), "an unshared pack isn't handed to a new member");
}

/// S12. Hopper restarted before taking a code in (it was read and kept
/// as waiting): after the restart it goes in once.
#[test]
#[ignore = "starts relay/hopper_mailbox.py; run with --ignored"]
fn s12_a_restart_before_taking_a_code_in_takes_it_in_once() {
    let relay = Relay::start();
    let (_, mut pcs) = group_of(&relay, &["A", "B"]);
    six_changes(&mut pcs[0]);
    pcs[0].publish().unwrap();
    pcs[1].read().unwrap();
    assert!(!pcs[1].copy.waiting.is_empty());
    pcs[1].restart();
    pcs[1].tick();
    pcs[1].restart();
    pcs[1].tick();
    assert_eq!(pcs[1].mods, after_six());
    assert_eq!(pcs[1].merges.len(), 1, "taken in once");
}

/// S13. The mailbox restarts (stopped after saving): every account,
/// group and waiting code is still there.
#[test]
#[ignore = "starts relay/hopper_mailbox.py; run with --ignored"]
fn s13_a_mailbox_restart_keeps_what_waits() {
    let mut relay = Relay::start();
    let (_, mut pcs) = group_of(&relay, &["A", "B"]);
    pcs[1].offline();
    six_changes(&mut pcs[0]);
    pcs[0].publish().unwrap();
    relay.stop_saved();
    relay.up();
    pcs[1].online();
    pcs[1].tick();
    assert_eq!(pcs[1].mods, after_six());
}

/// S14. The mailbox is down for a while: calls fail as "can't connect",
/// the change waits on A's PC and goes out once it's back.
#[test]
#[ignore = "starts relay/hopper_mailbox.py; run with --ignored"]
fn s14_mailbox_down_then_back() {
    let mut relay = Relay::start();
    let (_, mut pcs) = group_of(&relay, &["A", "B"]);
    relay.stop_saved();
    six_changes(&mut pcs[0]);
    let e = pcs[0].publish().unwrap_err();
    assert_eq!(mailbox::failure(&e), Failure::Unreachable);
    assert_eq!(mailbox::failure(&pcs[1].read().unwrap_err()), Failure::Unreachable);
    relay.up();
    assert!(pcs[0].publish().unwrap());
    pcs[1].tick();
    assert_eq!(pcs[1].mods, after_six());
}

/// GAP. The mailbox answers a publish before it saves it (it saves about
/// 1.5 s later). If it crashes in between, the change is gone from it,
/// but A's PC counts it as sent and never sends it again: B never gets
/// it, until A changes something else.
#[test]
#[ignore = "gap: a publish the mailbox lost in a crash is never sent again"]
fn gap_a_publish_lost_in_a_mailbox_crash_is_sent_again() {
    let mut relay = Relay::start();
    let (_, mut pcs) = group_of(&relay, &["A", "B"]);
    relay.stop_saved();
    relay.up();
    six_changes(&mut pcs[0]);
    pcs[0].publish().unwrap();
    relay.crash();
    relay.up();
    // A keeps running as the desktop does: a tick sends only a change.
    for _ in 0..3 {
        let _ = pcs[0].publish();
        pcs[1].tick();
    }
    assert_eq!(pcs[1].mods, after_six(), "B never got A's six changes");
}

/// S15. B's queue under a flood from many other friends (1000 events / 2
/// MB per queue): the one group code A sent survives; the flood's
/// biggest sender loses its oldest.
#[test]
#[ignore = "starts relay/hopper_mailbox.py; run with --ignored"]
fn s15_a_full_queue_keeps_a_small_senders_pack() {
    let relay = Relay::start();
    let (_, mut pcs) = group_of(&relay, &["A", "B"]);
    let mut floods = Vec::new();
    for n in 0..8 {
        let f = Pc::new(&relay, &format!("F{n}"));
        befriend(&f, &pcs[1]);
        floods.push(f);
    }
    pcs[1].offline();
    six_changes(&mut pcs[0]);
    pcs[0].publish().unwrap();
    let b = pcs[1].id();
    let code = pcs[0].code();
    for (n, f) in floods.iter().enumerate() {
        for i in 0..140 {
            f.mb.publish_to(&[b.clone()], &[], &format!("flood-{n}-{i}"), "k", "Flood", &code, &[], &Default::default()).unwrap();
        }
    }
    let st = relay.state();
    let q = st["events"][&b].as_array().unwrap();
    assert!(q.len() <= 1000, "queue capped: {}", q.len());
    pcs[1].online();
    pcs[1].tick();
    assert_eq!(pcs[1].mods, after_six(), "A's code survived the flood");
}

/// GAP. Past 150 events from one sender in one person's queue, the
/// oldest is dropped without a word: the sender got "sent" for each, and
/// the friend never hears of the first pack.
#[test]
#[ignore = "gap: a queue over the per-sender limit drops packs silently"]
fn gap_a_pack_pushed_out_of_a_queue_is_not_lost_silently() {
    let relay = Relay::start();
    let a = Pc::new(&relay, "A");
    let mut b = Pc::new(&relay, "B");
    befriend(&a, &b);
    b.offline();
    let code = a.code();
    for i in 0..151 {
        let r = a.mb.publish_to(&[b.id()], &[], &format!("p{i}"), "k", "P", &code, &[], &Default::default()).unwrap();
        assert_eq!(r.sent, 1, "the mailbox said it was sent");
    }
    b.online();
    b.read().unwrap();
    assert!(b.seen.iter().any(|e| e.inst == "p0"), "pack p0 vanished from B's queue and nobody was told");
}

/// S16. What the sharer is told when the mailbox refuses a publish for
/// good reasons. Today these come back as errors from a background
/// publish (logged, retried every minute, never shown); this checks the
/// sentences `says` would give.
#[test]
#[ignore = "starts relay/hopper_mailbox.py; run with --ignored"]
fn s16_a_too_big_code_is_refused_whole() {
    let relay = Relay::start();
    let a = Pc::new(&relay, "A");
    let b = Pc::new(&relay, "B");
    befriend(&a, &b);
    let huge = format!("HOP-{}", "x".repeat(20001));
    let e = a.mb.publish_to(&[b.id()], &[], "big", "k", "Big", &huge, &[], &Default::default()).unwrap_err();
    assert_eq!(mailbox::says(&e), "This pack has too many mods to share with friends.");
}

/// GAP. A sender over the 3 MB quota gets `storage_full`, which reads
/// "Try again in a few minutes": it won't work in a few minutes, and the
/// player isn't told what to do.
#[test]
#[ignore = "gap: storage_full says try again later"]
fn gap_storage_full_says_what_to_do() {
    let relay = Relay::start();
    let a = Pc::new(&relay, "A");
    let mut friends = Vec::new();
    for n in 0..12 {
        let f = Pc::new(&relay, &format!("F{n}"));
        befriend(&a, &f);
        friends.push(f.id());
    }
    // 12 friends x 15 packs x ~19 KB is over 3 MB waiting from A.
    let code = format!("HOP-{}", "y".repeat(19000));
    let mut refused = None;
    'out: for i in 0..15 {
        if let Err(e) = a.mb.publish_to(&friends, &[], &format!("p{i}"), "k", "P", &code, &[], &Default::default()) {
            refused = Some(e);
            break 'out;
        }
    }
    let e = refused.expect("the mailbox refuses past 3 MB");
    let said = mailbox::says(&e);
    assert!(!said.contains("Try again in a few minutes"), "{said}");
}

/// S17. A loader version change goes through the mailbox like a mod's.
#[test]
#[ignore = "starts relay/hopper_mailbox.py; run with --ignored"]
fn s17_a_loader_bump_reaches_members() {
    let relay = Relay::start();
    let (_, mut pcs) = group_of(&relay, &["A", "B"]);
    pcs[0].loader = "neoforge-21.1.228".into();
    pcs[0].publish().unwrap();
    pcs[1].tick();
    assert_eq!(pcs[1].loader, "neoforge-21.1.228");
    // Another kind of loader isn't followed.
    pcs[0].loader = "fabric-0.16.9".into();
    pcs[0].publish().unwrap();
    pcs[1].tick();
    assert_eq!(pcs[1].loader, "neoforge-21.1.228");
}

/// S18. A hand-added jar (no source) in the sharer's pack travels as
/// `left_out` to members who follow it.
#[test]
#[ignore = "starts relay/hopper_mailbox.py; run with --ignored"]
fn s18_a_hand_added_jar_is_named_to_members() {
    let relay = Relay::start();
    let (gid, mut pcs) = group_of(&relay, &["A", "B"]);
    let code = pcs[0].code();
    pcs[0]
        .mb
        .publish_to(&[], &[gid.clone()], PACK, "A-folder", "Test pack", &code, &["my-own-mod.jar".into()], &Default::default())
        .unwrap();
    pcs[1].read().unwrap();
    let e = pcs[1].seen.iter().rev().find(|e| e.kind() == Kind::Recipe && e.from == pcs[0].id()).unwrap();
    assert_eq!(e.left_out, ["my-own-mod.jar"]);
}

/// S19. A member who left and is added again is handed the group's
/// latest code again.
#[test]
#[ignore = "starts relay/hopper_mailbox.py; run with --ignored"]
fn s19_a_group_member_who_rejoins_gets_the_latest_again() {
    let relay = Relay::start();
    let (gid, mut pcs) = group_of(&relay, &["A", "B"]);
    six_changes(&mut pcs[0]);
    pcs[0].publish().unwrap();
    pcs[1].mb.leave_group(&gid).unwrap();
    pcs[0].mb.edit_group(&gid, None, &[pcs[1].id()], &[]).unwrap();
    pcs[1].mb.accept_group(&gid).unwrap();
    pcs[1].restart();
    pcs[1].copy = PackCopy { inst: PACK.into(), group: gid.clone(), local: "B-local".into(), ..Default::default() };
    pcs[1].read().unwrap();
    let e = pcs[1].seen.iter().rev().find(|e| e.kind() == Kind::Recipe && e.from == pcs[0].id()).expect("B is handed A's pack again");
    assert_eq!(e.code, pcs[0].code());
}
