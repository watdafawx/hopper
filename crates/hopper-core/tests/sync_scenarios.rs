//! Friends sync against the real mailbox (`relay/hopper_mailbox.py`), with
//! several people on one PC: what the mailbox client sees of the relay
//! (revokes, unfriending, refusals, being down) with nobody's mods involved.
//!
//! Each test starts its own scratch mailbox on 127.0.0.1 (a free port, a
//! state file in a temp folder) with `python`, so nothing ever talks to
//! the public one. Every person reaches it through their own little proxy,
//! which is how a test turns their internet off and on: going offline
//! drops their open connections and refuses new ones, the way a pulled
//! cable or a closed tunnel does.
//!
//! Run (Python 3 on PATH):
//!
//! ```text
//! cargo test -p hopper-core --test sync_scenarios -- --ignored --test-threads=1
//! ```
//!
//! What the sync core does with changes (taking them in, clashes, undo,
//! away, packs) is in `rewrite::`, the scenarios of the rewritten core
//! (design/sync-rewrite.md), in `tests/sync_scenarios/`. They are in-memory
//! and ignored as `sync-rewrite` until their phase lands; add
//! `--skip rewrite::` to the run above meanwhile.

#[path = "sync_scenarios/mod.rs"]
mod rewrite;

use hopper_core as hopper;

use hopper::mailbox::{self, Event, Failure, Kind, Mailbox};

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

struct Pc {
    mb: Mailbox,
    line: Line,
    /// The feed's newest number read (the desktop's `mailbox_seq`).
    cursor: u64,
    /// Every event read, for what the window would say.
    seen: Vec<Event>,
}

impl Pc {
    fn new(relay: &Relay, name: &str) -> Pc {
        let line = Line::to(relay.port);
        let mb = Mailbox::register(&format!("http://127.0.0.1:{}", line.port), name).unwrap();
        Pc { mb, line, cursor: 0, seen: Vec::new() }
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

    /// Reads the feed after the cursor, keeping what it says.
    fn read(&mut self) -> anyhow::Result<usize> {
        let (seq, events) = self.mb.events(self.cursor, 0)?;
        self.cursor = self.cursor.max(seq);
        let n = events.len();
        self.seen.extend(events);
        Ok(n)
    }

    /// The newest pack code the mailbox handed over from `from`.
    fn latest_from(&self, from: &Pc) -> Option<&Event> {
        self.seen.iter().rev().find(|e| e.kind() == Kind::Recipe && e.from == from.id())
    }

    fn revokes(&self) -> Vec<(String, String)> {
        self.seen.iter().filter(|e| e.kind() == Kind::Revoke).map(|e| (e.inst.clone(), e.reason.clone())).collect()
    }
}

fn befriend(a: &Pc, b: &Pc) {
    let (code, _) = a.mb.invite().unwrap();
    b.mb.join(&code).unwrap();
}

/// A pack's code the mailbox accepts as one ("HOP-" and something). `n`
/// tells two apart.
fn share_code(n: u32) -> String {
    format!("HOP-test-code-{n}")
}

/// A shares the pack into a new group with the others, who say yes. Everyone
/// reads what the mailbox has so far.
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
    pcs[0].mb.publish_to(&[], std::slice::from_ref(&g.id), PACK, "A-folder", "Test pack", &share_code(0), &[], &Default::default()).unwrap();
    for p in pcs.iter_mut() {
        p.read().unwrap();
    }
    (g.id, pcs)
}

// ── scenarios ────────────────────────────────────────────────────────

/// S9. Unfriended while offline: the member is told on return, and the
/// group is gone from their list.
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
    assert!(d.latest_from(&pcs[0]).is_none(), "an unshared pack isn't handed to a new member");
}

/// S14. The mailbox is down for a while: calls fail as "can't connect",
/// and work again once it's back.
#[test]
#[ignore = "starts relay/hopper_mailbox.py; run with --ignored"]
fn s14_mailbox_down_then_back() {
    let mut relay = Relay::start();
    let (gid, mut pcs) = group_of(&relay, &["A", "B"]);
    relay.stop_saved();
    let code = share_code(1);
    let groups = std::slice::from_ref(&gid);
    let e = pcs[0].mb.publish_to(&[], groups, PACK, "A-folder", "Test pack", &code, &[], &Default::default()).unwrap_err();
    assert_eq!(mailbox::failure(&e), Failure::Unreachable);
    assert_eq!(mailbox::failure(&pcs[1].read().unwrap_err()), Failure::Unreachable);
    relay.up();
    pcs[0].mb.publish_to(&[], groups, PACK, "A-folder", "Test pack", &code, &[], &Default::default()).unwrap();
    pcs[1].read().unwrap();
    assert_eq!(pcs[1].latest_from(&pcs[0]).unwrap().code, code);
}

/// S16. What the sharer is told when the mailbox refuses a publish for
/// good reasons: this checks the sentences `says` gives.
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

/// S18. A hand-added jar (no source) in the sharer's pack travels as
/// `left_out` to members who follow it.
#[test]
#[ignore = "starts relay/hopper_mailbox.py; run with --ignored"]
fn s18_a_hand_added_jar_is_named_to_members() {
    let relay = Relay::start();
    let (gid, mut pcs) = group_of(&relay, &["A", "B"]);
    pcs[0]
        .mb
        .publish_to(&[], &[gid], PACK, "A-folder", "Test pack", &share_code(2), &["my-own-mod.jar".into()], &Default::default())
        .unwrap();
    pcs[1].read().unwrap();
    let e = pcs[1].latest_from(&pcs[0]).unwrap();
    assert_eq!(e.left_out, ["my-own-mod.jar"]);
}

/// S19. A member who left and is added again is handed the group's
/// latest code again.
#[test]
#[ignore = "starts relay/hopper_mailbox.py; run with --ignored"]
fn s19_a_group_member_who_rejoins_gets_the_latest_again() {
    let relay = Relay::start();
    let (gid, mut pcs) = group_of(&relay, &["A", "B"]);
    let code = share_code(3);
    pcs[0].mb.publish_to(&[], std::slice::from_ref(&gid), PACK, "A-folder", "Test pack", &code, &[], &Default::default()).unwrap();
    pcs[1].mb.leave_group(&gid).unwrap();
    pcs[0].mb.edit_group(&gid, None, &[pcs[1].id()], &[]).unwrap();
    pcs[1].mb.accept_group(&gid).unwrap();
    pcs[1].cursor = 0;
    pcs[1].seen.clear();
    pcs[1].read().unwrap();
    let e = pcs[1].latest_from(&pcs[0]).expect("B is handed A's pack again");
    assert_eq!(e.code, code);
}

/// A settings file no mod owns, shared by A, reaches B through the real
/// mailbox and is written into B's copy of the file. A mod's own settings
/// travel the same way in the same run, so the two are checked together.
#[test]
#[ignore = "starts relay/hopper_mailbox.py; run with --ignored"]
fn shared_settings_of_a_loose_file_and_a_mod_reach_the_other_pc() {
    use hopper::configsync as cs;
    use std::collections::BTreeMap;
    let relay = Relay::start();
    let (gid, mut pcs) = group_of(&relay, &["A", "B"]);
    let base = std::env::temp_dir().join(format!("hopper-cfg-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (rel_loose, rel_mod) = ("config/flywheel-client.toml", "config/jei-client.toml");
    let text = "[general]\n\tmaxColumns = 9\n\tbackend = \"default\"\n";
    let (da, db) = (base.join("a"), base.join("b"));
    for d in [&da, &db] {
        std::fs::create_dir_all(d.join("config")).unwrap();
        std::fs::write(d.join(rel_loose), text).unwrap();
        std::fs::write(d.join(rel_mod), text).unwrap();
    }
    let (kl, km) = (cs::file_key(rel_loose), "p:1".to_string());
    let files: BTreeMap<String, (String, Vec<String>)> = BTreeMap::from([
        (kl.clone(), ("flywheel-client.toml".to_string(), vec![rel_loose.to_string()])),
        (km.clone(), ("JEI".to_string(), vec![rel_mod.to_string()])),
    ]);
    // Both share both; each PC takes its first snapshot.
    let turned = |me: &str| {
        let mut s = cs::Configs::default();
        for k in [&kl, &km] {
            s.turn(k, true, me);
        }
        s
    };
    let (mut sa, mut sb) = (turned("a"), turned("b"));
    cs::tick(&da, &sa, &files, "a", &gid).unwrap();
    cs::tick(&db, &sb, &files, "b", &gid).unwrap();
    // A changes both files and sends what its tick found, as the app does.
    for rel in [rel_loose, rel_mod] {
        std::fs::write(da.join(rel), text.replace("maxColumns = 9", "maxColumns = 12")).unwrap();
    }
    let t = cs::tick(&da, &sa, &files, "a", &gid).unwrap();
    assert_eq!(t.own.len(), 2, "{:?}", t.own);
    sa.own(&t.own, "a");
    let wire = cs::encode(&sa).unwrap();
    pcs[0].mb.publish_with(&[], std::slice::from_ref(&gid), PACK, "A-folder", "Test pack", &share_code(7), &[], &Default::default(), &wire).unwrap();
    // B reads the feed and takes what came with the newest pack code.
    pcs[1].read().unwrap();
    let e = pcs[1].latest_from(&pcs[0]).expect("B is handed A's pack");
    assert!(!e.configs.is_empty(), "the mailbox kept the settings");
    let theirs = cs::decode(&e.configs).expect("settings decode");
    assert!(theirs.is_shared(&kl) && theirs.is_shared(&km));
    cs::take(&mut sb, &pcs[0].id(), "A", e.seq, 1, &theirs, "b", |_| "A".into());
    let t = cs::tick(&db, &sb, &files, "b", &gid).unwrap();
    assert_eq!(t.wrote.len(), 2, "{:?}", t.wrote);
    let want = text.replace("maxColumns = 9", "maxColumns = 12");
    for rel in [rel_loose, rel_mod] {
        assert_eq!(std::fs::read_to_string(db.join(rel)).unwrap(), want, "{rel}");
    }
    let _ = std::fs::remove_dir_all(&base);
}
