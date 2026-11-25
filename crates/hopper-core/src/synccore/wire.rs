//! What travels through the relay (design §10). Pure.
//!
//! One event per sender and group, cumulative (every record some member
//! hasn't acked), so the relay keeping only the newest event loses nothing.

use super::digest::StateEntry;
use super::model::{Change, Key, MemberId, Seen};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};

/// Packets of any other version are refused ("Mia's Hopper is out of date").
pub const WIRE_VERSION: u8 = 1;

/// A packet over this many bytes (after deflate + base64url) goes in parts.
pub const MAX_PART: usize = 60 * 1024;

/// A member's state, sent with every packet: acks, and what the pack plan
/// and the status need to know about their PC.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemberStatus {
    pub member: MemberId,
    pub name: String,
    /// What they have taken in (acks).
    pub seen: Seen,
    /// What they have applied on disk.
    pub applied: Seen,
    pub pack: String,
    pub edited_configs: Vec<String>,
    pub off_pack_mods: Vec<Key>,
    /// Keys the group has that their disk lacks.
    pub missing: Vec<Key>,
    pub game_running: bool,
    /// Their register hash differed: send them a checkpoint.
    pub needs_full: bool,
    /// Hash of the mods this PC has applied ([`super::digest`]). Empty from
    /// a Hopper that doesn't send one: nothing is compared with it.
    pub state_hash: String,
    /// Hash of the sender's register, so a friend can tell the two PCs hold
    /// the same records (a difference is then no one's change in flight).
    pub reg_hash: String,
    /// Nothing waits to land on the sender's disk (no download, retry,
    /// game running or review pending).
    pub settled: bool,
    /// The sender's state hash differs from the receiver's with the records
    /// level and both settled: send me your list of mods.
    pub needs_state: bool,
    /// The sender's mods, mod by mod. Only in a packet to a friend who set
    /// `needs_state`; a hash alone is 16 bytes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub state: Vec<StateEntry>,
}

/// One copy's part of a packet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Body {
    /// The records some member hasn't acked yet.
    Tail(Vec<Change>),
    /// The winning record (and open clash records) of every key.
    Checkpoint(Vec<Change>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Packet {
    pub version: u8,
    pub from: MemberId,
    pub group: String,
    /// By copy id.
    pub copies: BTreeMap<String, (Body, MemberStatus, String)>,
}

/// Why a packet could not be taken in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WireError {
    /// Another wire version: the sender's Hopper is older (or newer).
    /// Nothing in it is read.
    OutOfDate { version: u8 },
    /// Parts of the set are still missing.
    Incomplete { have: usize, need: usize },
    /// Cut off, damaged or not a packet.
    Corrupt(&'static str),
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireError::OutOfDate { version } => write!(f, "out of date (wire version {version})"),
            WireError::Incomplete { have, need } => write!(f, "{have} of {need} parts"),
            WireError::Corrupt(why) => write!(f, "damaged packet: {why}"),
        }
    }
}

impl std::error::Error for WireError {}

/// Room kept for a part's header (`version.set.i.n.`) inside [`MAX_PART`].
const HEADER_ROOM: usize = 64;
/// A part count above this is damage, not a packet.
const MAX_PARTS: usize = 4096;
/// A packet that inflates past this is damage (a zip bomb), not a packet.
const MAX_INFLATED: u64 = 64 * 1024 * 1024;

/// FNV-1a, so the set id is the same on every PC and build.
fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn set_id(text: &str) -> String {
    format!("{:016x}", fnv(text.as_bytes()))
}

/// The relay bodies for a packet: one, or parts under [`MAX_PART`].
/// Each body starts `version.set.index.total.`, so an old version is
/// refused before anything else is read.
pub fn encode(p: &Packet) -> Vec<String> {
    encode_with(p, MAX_PART)
}

/// [`encode`] with another part size: the real relay keeps an event's body
/// in a field of 20 000 bytes, so the desktop cuts to that.
pub fn encode_with(p: &Packet, max_part: usize) -> Vec<String> {
    let json = serde_json::to_vec(p).expect("a packet is plain data");
    let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
    z.write_all(&json).expect("writing to memory");
    let packed = z.finish().expect("writing to memory");
    let text = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(packed);
    let set = set_id(&text);
    // base64 is ASCII, so any byte cut is a character cut.
    let chunks: Vec<&str> = text
        .as_bytes()
        .chunks(max_part.saturating_sub(HEADER_ROOM).max(1))
        .map(|c| std::str::from_utf8(c).expect("ascii"))
        .collect();
    let n = chunks.len();
    chunks
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{}.{set}.{i}.{n}.{c}", p.version))
        .collect()
}

/// The relay key of part `i` of `n` of a group's event.
pub fn part_key(group: &str, i: usize, n: usize) -> String {
    if n <= 1 {
        group.to_string()
    } else {
        format!("{group}#part-{i}")
    }
}

struct Part<'a> {
    set: &'a str,
    i: usize,
    n: usize,
    body: &'a str,
}

fn parse_part(s: &str) -> Result<Part<'_>, WireError> {
    let bad = WireError::Corrupt("part header");
    let mut it = s.splitn(5, '.');
    let ver: u8 = it.next().and_then(|v| v.parse().ok()).ok_or(bad.clone())?;
    if ver != WIRE_VERSION {
        return Err(WireError::OutOfDate { version: ver });
    }
    let set = it.next().ok_or(bad.clone())?;
    let i: usize = it.next().and_then(|v| v.parse().ok()).ok_or(bad.clone())?;
    let n: usize = it.next().and_then(|v| v.parse().ok()).ok_or(bad.clone())?;
    let body = it.next().ok_or(bad.clone())?;
    if set.len() != 16 || n == 0 || n > MAX_PARTS || i >= n {
        return Err(bad);
    }
    Ok(Part { set, i, n, body })
}

/// Collects parts as they arrive, in any order, and yields the packet once
/// the set is whole. A newer set replaces an unfinished older one (the
/// relay keeps the newest event only).
#[derive(Debug, Default)]
pub struct Reassembly {
    set: String,
    need: usize,
    parts: BTreeMap<usize, String>,
}

impl Reassembly {
    /// Takes one part. `Ok(None)` while parts are missing; a duplicate is
    /// ignored; a part of another set starts that set.
    pub fn push(&mut self, part: &str) -> Result<Option<Packet>, WireError> {
        let p = parse_part(part)?;
        if p.set != self.set || p.n != self.need {
            self.set = p.set.to_string();
            self.need = p.n;
            self.parts.clear();
        }
        match self.parts.get(&p.i) {
            Some(old) if old != p.body => return Err(WireError::Corrupt("part differs")),
            Some(_) => {}
            None => {
                self.parts.insert(p.i, p.body.to_string());
            }
        }
        if self.parts.len() < self.need {
            return Ok(None);
        }
        let text: String = self.parts.values().map(String::as_str).collect();
        if set_id(&text) != self.set {
            return Err(WireError::Corrupt("set hash"));
        }
        open(&text).map(Some)
    }

    /// How many parts of the current set are in, and how many it needs.
    pub fn progress(&self) -> (usize, usize) {
        (self.parts.len(), self.need)
    }
}

fn open(text: &str) -> Result<Packet, WireError> {
    let packed = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|_| WireError::Corrupt("base64"))?;
    let mut json = Vec::new();
    flate2::read::DeflateDecoder::new(&packed[..])
        .take(MAX_INFLATED)
        .read_to_end(&mut json)
        .map_err(|_| WireError::Corrupt("deflate"))?;
    let p: Packet = serde_json::from_slice(&json).map_err(|_| WireError::Corrupt("packet"))?;
    if p.version != WIRE_VERSION {
        return Err(WireError::OutOfDate { version: p.version });
    }
    Ok(p)
}

/// A packet from its parts (any order, duplicates fine); an error for
/// another version ([`WireError::OutOfDate`]), a part missing
/// ([`WireError::Incomplete`]) or damage. Never panics.
pub fn decode(parts: &[String]) -> anyhow::Result<Packet> {
    let mut r = Reassembly::default();
    for part in parts {
        if let Some(p) = r.push(part)? {
            return Ok(p);
        }
    }
    let (have, need) = r.progress();
    Err(WireError::Incomplete { have, need }.into())
}

/// The records of `mine` that some member hasn't acked: not covered by one
/// of `acks` (each member's latest `seen`). Cumulative, so a newer event
/// replacing an older one at the relay loses nothing. Ordered by dot.
pub fn tail(mine: &[Change], acks: &[&Seen]) -> Vec<Change> {
    let mut out: Vec<Change> = mine
        .iter()
        .filter(|c| acks.iter().any(|s| !s.covers(&c.dot)))
        .cloned()
        .collect();
    out.sort_by(|a, b| a.dot.cmp(&b.dot));
    out
}

/// Whether a receiver's register hash (after folding, nothing held)
/// disagrees with the sender's, so it must set `needs_full`.
pub fn hash_mismatch(mine: &str, theirs: &str) -> bool {
    mine != theirs
}

/// A checkpoint is sent to a member who asked (`needs_full`), to a new
/// member, and on every 50th publish; otherwise the tail is enough.
pub fn send_checkpoint(asked: bool, new_member: bool, publishes: u64) -> bool {
    asked || new_member || (publishes > 0 && publishes % 50 == 0)
}

/// What the relay enforces (design §10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RelayLimits {
    /// Largest event body, bytes.
    pub max_body: usize,
    /// Events held per sender per recipient.
    pub max_events: usize,
    /// Bytes one sender may keep in groups, all packs together
    /// (`MAX_PACK_BYTES`): past it the relay answers 409 `storage_full`.
    pub group_bytes: usize,
    /// Keys one sender may hold in one group (`MAX_GROUP_PACKS`): every part
    /// of every copy is one. Past it: 409 `group_packs_full`.
    pub group_packs: usize,
    /// What the relay counts beside a part's code (its key, name, marks).
    pub part_overhead: usize,
}

impl Default for RelayLimits {
    fn default() -> Self {
        RelayLimits { max_body: 64 * 1024, max_events: 150, group_bytes: 1024 * 1024, group_packs: 100, part_overhead: 400 }
    }
}

/// A packet cut to what the relay's group storage takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fitted {
    pub parts: Vec<String>,
    /// How many records of the copy go in this post, and how many there were.
    /// The rest are not lost: nobody has acked them, so the next packet
    /// carries them (cumulative tails), as soon as the friend acks these.
    pub kept: usize,
    pub of: usize,
}

impl RelayLimits {
    /// What the relay counts against the sender for these parts.
    pub fn cost(&self, parts: &[String]) -> usize {
        parts.iter().map(|p| p.len() + self.part_overhead).sum()
    }
}

/// `packet` (one copy's records), cut by whole records until it fits what
/// this copy may keep at the relay: its share of the sender's group bytes
/// and of a group's keys, `copies` being how many copies this PC holds.
/// The oldest records go first and every author's counter stays gapless
/// (records are ordered by `lamport`, which follows each author's own
/// counter), so an ack of the part never claims a record that was cut. A
/// copy's records are never dropped for good: `None` says even one record
/// doesn't fit, which is a plain message for the player, not a retry.
pub fn fit(packet: &Packet, limits: &RelayLimits, part_bytes: usize, copies: usize) -> Option<Fitted> {
    let copies = copies.max(1);
    let (bytes, keys) = (limits.group_bytes / copies, (limits.group_packs / copies).max(1));
    let fits = |parts: &[String]| {
        parts.iter().all(|p| p.len() + 4 <= limits.max_body) && parts.len() <= keys && limits.cost(parts) <= bytes
    };
    let of = packet.copies.values().map(|(b, ..)| records(b).len()).max().unwrap_or(0);
    let mut keep = of;
    loop {
        let mut cut = packet.clone();
        for (body, ..) in cut.copies.values_mut() {
            let list = records_mut(body);
            list.sort_by(|a, b| (a.lamport, &a.dot).cmp(&(b.lamport, &b.dot)));
            list.truncate(keep);
        }
        let parts = encode_with(&cut, part_bytes);
        if fits(&parts) {
            return Some(Fitted { parts, kept: keep, of });
        }
        if keep <= 1 {
            return None;
        }
        keep = (keep / 2).max(1);
    }
}

fn records(b: &Body) -> &Vec<Change> {
    match b {
        Body::Tail(c) | Body::Checkpoint(c) => c,
    }
}

fn records_mut(b: &mut Body) -> &mut Vec<Change> {
    match b {
        Body::Tail(c) | Body::Checkpoint(c) => c,
    }
}

/// A sender's events held for one recipient, as the relay keeps them:
/// newest per key, at most `max_events`, the oldest dropped first.
#[derive(Clone, Debug, Default)]
pub struct RelayQueue {
    /// (key, body), oldest first.
    events: Vec<(String, String)>,
}

impl RelayQueue {
    /// Stores an event. Refused (`false`) over the body cap.
    pub fn put(&mut self, key: &str, body: String, limits: &RelayLimits) -> bool {
        if body.len() > limits.max_body {
            return false;
        }
        self.events.retain(|(k, _)| k != key);
        self.events.push((key.to_string(), body));
        while self.events.len() > limits.max_events {
            self.events.remove(0);
        }
        true
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// The bodies held, oldest first.
    pub fn bodies(&self) -> Vec<String> {
        self.events.iter().map(|(_, b)| b.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synccore::model::{Dot, Value, Why};

    fn change(author: &str, n: u64, key: Key) -> Change {
        Change {
            dot: Dot { author: author.into(), n },
            key,
            was: Value::Gone,
            now: Value::File { file: format!("f{n}.jar"), on: true },
            name: author.into(),
            lamport: n,
            seen: Seen::default(),
            online_at: 0,
            batch: format!("b{n}"),
            why: Why::Player,
        }
    }

    /// `copies` packs with `per_copy` records each; ids look random so the
    /// deflate can't shrink them away.
    fn packet(copies: usize, per_copy: usize) -> Packet {
        let mut map = BTreeMap::new();
        for c in 0..copies {
            let recs = (0..per_copy)
                .map(|i| {
                    let id = fnv(format!("{c}/{i}").as_bytes());
                    let mut c = change("mia", i as u64 + 1, Key::Mod(format!("cf:{id:x}")));
                    c.now = Value::File { file: format!("{:x}{:x}.jar", fnv(&id.to_le_bytes()), id), on: true };
                    c
                })
                .collect();
            map.insert(format!("pack-{c}"), (Body::Tail(recs), MemberStatus::default(), "h".to_string()));
        }
        Packet { version: WIRE_VERSION, from: "mia".into(), group: "g".into(), copies: map }
    }

    #[test]
    fn round_trips_single_and_parts() {
        let small = packet(2, 3);
        let e = encode(&small);
        assert_eq!(e.len(), 1);
        assert_eq!(decode(&e).unwrap(), small);
        let big = packet(150, 120);
        let e = encode(&big);
        assert!(e.len() > 1);
        assert!(e.iter().all(|p| p.len() <= MAX_PART));
        assert_eq!(decode(&e).unwrap(), big);
    }

    #[test]
    fn parts_reassemble_out_of_order_with_duplicates() {
        let big = packet(150, 120);
        let mut e = encode(&big);
        e.reverse();
        let dup = e[0].clone();
        e.insert(1, dup);
        assert_eq!(decode(&e).unwrap(), big);
    }

    #[test]
    fn a_missing_part_is_incomplete_not_a_packet() {
        let big = packet(150, 120);
        let mut e = encode(&big);
        assert!(e.len() > 2);
        e.remove(1);
        let err = decode(&e).unwrap_err();
        assert!(matches!(err.downcast_ref::<WireError>(), Some(WireError::Incomplete { .. })));
        let mut r = Reassembly::default();
        for p in &e {
            assert!(r.push(p).unwrap().is_none());
        }
    }

    #[test]
    fn a_newer_set_replaces_an_unfinished_older_one() {
        let a = encode(&packet(150, 120));
        let b = encode(&packet(150, 121));
        let mut r = Reassembly::default();
        assert!(r.push(&a[0]).unwrap().is_none());
        let mut got = None;
        for p in &b {
            got = r.push(p).unwrap();
        }
        assert_eq!(got.unwrap(), packet(150, 121));
    }

    #[test]
    fn another_version_is_refused_as_out_of_date() {
        let mut p = packet(1, 1);
        p.version = 2;
        let err = decode(&encode(&p)).unwrap_err();
        assert_eq!(err.downcast_ref::<WireError>(), Some(&WireError::OutOfDate { version: 2 }));
        // The header alone decides: the rest is never read.
        let err = decode(&["0.not-even-a-packet".to_string()]).unwrap_err();
        assert_eq!(err.downcast_ref::<WireError>(), Some(&WireError::OutOfDate { version: 0 }));
    }

    #[test]
    fn truncated_and_corrupt_input_is_an_error_not_a_panic() {
        let good = encode(&packet(3, 5)).remove(0);
        for cut in 0..good.len() {
            assert!(decode(&[good[..cut].to_string()]).is_err(), "cut {cut}");
        }
        for at in (good.len() - 40)..good.len() {
            let mut b = good.clone().into_bytes();
            b[at] = if b[at] == b'A' { b'B' } else { b'A' };
            let s = String::from_utf8(b).unwrap();
            assert!(decode(&[s]).is_err(), "flip {at}");
        }
        let junk = [
            "",
            "1",
            "1.",
            "1.....",
            "1.0123456789abcdef.9.2.xx",
            "1.0123456789abcdef.0.0.",
            "\u{0}\u{7f}\u{ff}",
            "99999999999.a.b.c.d",
        ];
        for j in junk {
            assert!(decode(&[j.to_string()]).is_err(), "{j:?}");
        }
        assert!(decode(&[]).is_err());
    }

    #[test]
    fn tail_is_everything_some_member_has_not_acked() {
        let mine: Vec<_> = (1..=4).map(|n| change("mia", n, Key::Mod(format!("cf:{n}")))).collect();
        let mut noah = Seen::default();
        let mut zoe = Seen::default();
        noah.0.insert("mia".into(), 4);
        zoe.0.insert("mia".into(), 2);
        let t = tail(&mine, &[&noah, &zoe]);
        assert_eq!(t.iter().map(|c| c.dot.n).collect::<Vec<_>>(), vec![3, 4]);
        zoe.0.insert("mia".into(), 4);
        assert!(tail(&mine, &[&noah, &zoe]).is_empty());
        // Offline Noah never acked: the newest packet still holds all four.
        assert_eq!(tail(&mine, &[&Seen::default()]).len(), 4);
    }

    #[test]
    fn checkpoint_rules() {
        assert!(send_checkpoint(true, false, 3));
        assert!(send_checkpoint(false, true, 3));
        assert!(send_checkpoint(false, false, 50));
        assert!(!send_checkpoint(false, false, 0));
        assert!(!send_checkpoint(false, false, 49));
        assert!(hash_mismatch("a", "b"));
        assert!(!hash_mismatch("a", "a"));
    }

    #[test]
    fn a_hundred_fifty_one_packs_are_one_cumulative_event() {
        let p = packet(151, 1);
        let parts = encode(&p);
        let limits = RelayLimits::default();
        let mut q = RelayQueue::default();
        for (i, body) in parts.iter().enumerate() {
            assert!(q.put(&part_key("g", i, parts.len()), body.clone(), &limits));
        }
        assert!(q.len() <= 2);
        assert_eq!(decode(&q.bodies()).unwrap().copies.len(), 151);
        // The old way, one event per pack, pushes the first one out.
        let mut old = RelayQueue::default();
        for i in 0..151 {
            old.put(&format!("pack-{i}"), "x".into(), &limits);
        }
        assert_eq!(old.len(), 150);
    }

    #[test]
    fn a_big_checkpoint_is_cut_to_what_the_group_storage_takes() {
        // The relay's cap is 1 MiB; a smaller one keeps the fixture quick.
        let limits = RelayLimits { group_bytes: 40_000, ..RelayLimits::default() };
        let mut p = packet(1, 3000);
        for (body, ..) in p.copies.values_mut() {
            *body = Body::Checkpoint(records(body).clone());
        }
        let whole = encode_with(&p, 19_000);
        assert!(limits.cost(&whole) > limits.group_bytes, "the fixture must be too big: {}", limits.cost(&whole));
        let f = fit(&p, &limits, 19_000, 1).expect("some of it fits");
        assert!(f.kept < f.of && f.kept >= 1);
        assert!(limits.cost(&f.parts) <= limits.group_bytes);
        assert!(f.parts.len() <= limits.group_packs);
        // What went is a whole prefix: it reads back, and holds the oldest.
        let back = decode(&f.parts).unwrap();
        let got: Vec<u64> = back.copies.values().flat_map(|(b, ..)| records(b).iter().map(|c| c.lamport)).collect();
        assert_eq!(got.len(), f.kept);
        assert!(got.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn a_copy_gets_its_share_when_a_pc_holds_many() {
        let limits = RelayLimits::default();
        let p = packet(1, 400);
        let alone = fit(&p, &limits, 19_000, 1).unwrap();
        let of_ten = fit(&p, &limits, 19_000, 10).unwrap();
        assert!(of_ten.kept <= alone.kept);
        assert!(limits.cost(&of_ten.parts) <= limits.group_bytes / 10);
        assert!(of_ten.parts.len() <= limits.group_packs / 10);
    }

    #[test]
    fn a_packet_that_fits_goes_whole_and_one_that_never_can_says_so() {
        let limits = RelayLimits::default();
        let small = packet(1, 5);
        let f = fit(&small, &limits, 19_000, 1).unwrap();
        assert_eq!((f.kept, f.of), (5, 5));
        // A budget no single record fits.
        let tiny = RelayLimits { group_bytes: 100, ..limits };
        assert!(fit(&small, &tiny, 19_000, 1).is_none());
    }

    #[test]
    fn the_relay_keeps_the_newest_per_key_and_refuses_big_bodies() {
        let limits = RelayLimits::default();
        let mut q = RelayQueue::default();
        assert!(q.put("g", "one".into(), &limits));
        assert!(q.put("g", "two".into(), &limits));
        assert_eq!(q.bodies(), vec!["two".to_string()]);
        assert!(!q.put("g", "x".repeat(limits.max_body + 1), &limits));
        assert_eq!(q.len(), 1);
        assert!(encode(&packet(150, 120)).iter().all(|p| p.len() <= limits.max_body));
        assert_eq!(part_key("g", 0, 1), "g");
        assert_eq!(part_key("g", 2, 3), "g#part-2");
    }
}
