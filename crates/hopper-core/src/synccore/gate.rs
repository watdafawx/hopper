//! What waits for the player before it lands on this PC (design §4). Pure.
//!
//! All times are relay seconds (`Arrival::at`, `Change::online_at`); the
//! local clock is never read, so a PC with a wrong clock gates the same.

use super::model::{Arrival, Change, Key, MemberId, Value, Why};
use super::register::Register;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// When to ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// Relay seconds between a member's last contact and their change
    /// arriving that count as "was away".
    pub away_secs: u64,
    /// More removals or turn-offs than this in one batch ask first.
    pub many_removals: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits { away_secs: 24 * 3600, many_removals: 5 }
    }
}

/// How this PC takes one member's changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Trust {
    #[default]
    Auto,
    AskFirst,
    Ignore,
}

/// Why a record waits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Risk {
    /// A removal or turn-off made at the same time as someone's update.
    Clash,
    /// The author was away this long (relay seconds).
    LongAway { secs: u64 },
    ManyRemovals { count: usize },
    /// The pack moved to an older version.
    PackBack,
    /// The pack moved to another project.
    OtherPack,
    /// A file from GitHub or a link this PC hasn't allowed.
    NewSource,
    /// The member is set to "Ask me first".
    AskFirst,
    /// A friend put a mod back to the file it had before someone else's
    /// update of it, after taking that update in: "QuietCarrot put FTB
    /// Quests back to an older version". Landing it by itself rolled the
    /// update back on every PC (user report 2026-11-26). An undo from
    /// History says so in as many words and lands as before.
    MovedBack,
    /// Not a held record: this PC and a friend's have the same records and
    /// have both landed everything, yet their mods differ (the first start
    /// after an upgrade began from each disk with an empty log). The gate
    /// never returns it; `Engine::reviews` raises it from the friends'
    /// state digests.
    Differs,
}

/// What the gate knows besides the record.
pub struct GateCtx<'a> {
    pub limits: Limits,
    pub trust: &'a BTreeMap<MemberId, Trust>,
    pub registers: &'a BTreeMap<super::model::Key, Register>,
    /// The other records of the same batch.
    pub batch: &'a [Change],
    /// Source keys ("src:<file>") the player has allowed on this PC.
    pub allowed_sources: &'a BTreeSet<String>,
}

/// One held record and every reason it waits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    pub dot: super::model::Dot,
    pub risks: Vec<Risk>,
}

/// The risks of one incoming record; empty: it lands by itself.
pub fn risks(change: &Change, arrival: &Arrival, ctx: &GateCtx) -> Vec<Risk> {
    let mut out = Vec::new();
    if is_clash(change, ctx) {
        out.push(Risk::Clash);
    }
    // Relay time only: the author's last contact against when this arrived.
    let away = arrival.at.saturating_sub(change.online_at);
    if away > ctx.limits.away_secs {
        out.push(Risk::LongAway { secs: away });
    }
    if removes_a_mod(change) {
        let count = removals_in_batch(change, ctx);
        if count > ctx.limits.many_removals {
            out.push(Risk::ManyRemovals { count });
        }
    }
    if change.key == Key::Pack {
        match pack_move(&change.was, &change.now) {
            Some(PackMove::Back) => out.push(Risk::PackBack),
            Some(PackMove::Other) => out.push(Risk::OtherPack),
            None => {}
        }
    }
    if let (Key::Mod(k), Value::File { .. }) = (&change.key, &change.now) {
        if is_link_source(k) && !ctx.allowed_sources.contains(k) {
            out.push(Risk::NewSource);
        }
    }
    if ctx.trust.get(&change.dot.author) == Some(&Trust::AskFirst) {
        out.push(Risk::AskFirst);
    }
    out
}

/// The records of a whole arrival that wait, each with its reasons. The
/// batch counts are taken from `items` itself, so the caller needn't fill
/// `ctx.batch`.
pub fn held(items: &[(Change, Arrival)], ctx: &GateCtx) -> Vec<Held> {
    let batch: Vec<Change> = items.iter().map(|(c, _)| c.clone()).collect();
    let ctx = GateCtx { batch: &batch, ..*ctx };
    items
        .iter()
        .filter_map(|(c, a)| {
            let risks = risks(c, a, &ctx);
            (!risks.is_empty()).then(|| Held { dot: c.dot.clone(), risks })
        })
        .collect()
}

/// Whether this record is one side of an open clash in its key's register.
fn is_clash(change: &Change, ctx: &GateCtx) -> bool {
    ctx.registers.get(&change.key).is_some_and(|r| {
        r.is_clash() && (r.winner.dot == change.dot || r.open.iter().any(|o| o.dot == change.dot))
    })
}

/// A mod taken away or turned off (locks and versions don't count).
fn removes_a_mod(c: &Change) -> bool {
    matches!(c.key, Key::Mod(_)) && c.now.takes_away(&c.was)
}

/// Removals in this record's batch from its author, itself included once.
fn removals_in_batch(change: &Change, ctx: &GateCtx) -> usize {
    1 + ctx
        .batch
        .iter()
        .filter(|o| {
            o.dot != change.dot
                && o.dot.author == change.dot.author
                && o.batch == change.batch
                && removes_a_mod(o)
        })
        .count()
}

/// GitHub files ("src:<file>"), with or without a folder prefix such as
/// "shader:" or "rp:".
pub(crate) fn is_link_source(key: &str) -> bool {
    key.starts_with("src:") || key.split(':').skip(1).any(|p| p == "src")
}

/// Whether `x` puts a mod back to what it had before `update`, another
/// member's update that `x`'s author had taken in ([`Risk::MovedBack`]).
/// Only a plain change counts: an undo from History, an answer to a card and
/// "Use mine for everyone" say what they do in as many words.
pub fn moves_back(x: &Change, update: &Change) -> bool {
    matches!(x.key, Key::Mod(_))
        && x.key == update.key
        && x.why == Why::Player
        && x.dot.author != update.dot.author
        && x.seen.covers(&update.dot)
        && x.now == update.was
        && x.was == update.now
        && is_update(&update.was, &update.now)
}

/// A move of a mod that stays on to another file: a newer CurseForge file,
/// or another file of any kind (Modrinth versions and links have no order to
/// read), or off the pack's own file.
pub(crate) fn is_update(from: &Value, to: &Value) -> bool {
    let cf = |f: &str| f.strip_prefix("cf:").and_then(|n| n.parse::<u64>().ok());
    match (from, to) {
        (Value::File { file: a, on: true }, Value::File { file: b, on: true }) if a != b => match (cf(a), cf(b)) {
            (Some(a), Some(b)) => b > a,
            _ => true,
        },
        (Value::PackDefault, Value::File { on: true, .. }) => true,
        _ => false,
    }
}

enum PackMove {
    Back,
    Other,
}

/// "cf:<project>/<file>" -> (project, file id).
fn pack_parts(v: &Value) -> Option<(&str, u64)> {
    let Value::Text(t) = v else { return None };
    let (project, file) = t.rsplit_once('/')?;
    Some((project, file.parse().ok()?))
}

fn pack_move(was: &Value, now: &Value) -> Option<PackMove> {
    let (wp, wf) = pack_parts(was)?;
    let (np, nf) = pack_parts(now)?;
    if wp != np {
        Some(PackMove::Other)
    } else if nf < wf {
        Some(PackMove::Back)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synccore::model::{Dot, Seen};
    use crate::synccore::register::{fold, Log};

    const H: u64 = 3600;

    fn change(author: &str, n: u64, key: Key, was: Value, now: Value) -> Change {
        Change {
            dot: Dot { author: author.into(), n },
            key,
            was,
            now,
            name: author.into(),
            lamport: n,
            seen: Seen::default(),
            online_at: 1000,
            batch: "b1".into(),
            why: Default::default(),
        }
    }

    fn file(f: &str) -> Value {
        Value::File { file: f.into(), on: true }
    }

    fn modk(n: u32) -> Key {
        Key::Mod(format!("cf:{n}"))
    }

    fn removal(author: &str, n: u64) -> Change {
        change(author, n, modk(n as u32), file("a.jar"), Value::Gone)
    }

    fn at(secs: u64) -> Arrival {
        Arrival { seq: 1, at: secs }
    }

    struct Env {
        trust: BTreeMap<MemberId, Trust>,
        registers: BTreeMap<Key, Register>,
        batch: Vec<Change>,
        allowed: BTreeSet<String>,
    }

    impl Env {
        fn new() -> Env {
            Env {
                trust: BTreeMap::new(),
                registers: BTreeMap::new(),
                batch: Vec::new(),
                allowed: BTreeSet::new(),
            }
        }
        fn run(&self, c: &Change, a: &Arrival) -> Vec<Risk> {
            let ctx = GateCtx {
                limits: Limits::default(),
                trust: &self.trust,
                registers: &self.registers,
                batch: &self.batch,
                allowed_sources: &self.allowed,
            };
            risks(c, a, &ctx)
        }
    }

    #[test]
    fn plain_change_from_a_present_friend_lands() {
        let env = Env::new();
        let c = change("mia", 1, modk(1), Value::Gone, file("a.jar"));
        assert!(env.run(&c, &at(1100)).is_empty());
    }

    #[test]
    fn clash_holds_both_sides() {
        let mut env = Env::new();
        let was = file("a.jar");
        let remove = change("mia", 1, modk(1), was.clone(), Value::Gone);
        let update = change("noah", 1, modk(1), was, file("b.jar"));
        let mut log = Log::default();
        log.insert(remove.clone(), None);
        log.insert(update.clone(), None);
        env.registers = fold(&log);
        assert!(env.registers[&modk(1)].is_clash());
        assert_eq!(env.run(&remove, &at(1100)), vec![Risk::Clash]);
        assert_eq!(env.run(&update, &at(1100)), vec![Risk::Clash]);
    }

    #[test]
    fn settled_register_is_not_a_clash() {
        let mut env = Env::new();
        let c = change("mia", 1, modk(1), file("a.jar"), file("b.jar"));
        let mut log = Log::default();
        log.insert(c.clone(), None);
        env.registers = fold(&log);
        assert!(env.run(&c, &at(1100)).is_empty());
    }

    #[test]
    fn away_boundary_is_exactly_24_hours() {
        let env = Env::new();
        let c = change("mia", 1, modk(1), Value::Gone, file("a.jar"));
        // online_at is 1000: exactly 24 h later passes, one second more holds.
        assert!(env.run(&c, &at(1000 + 24 * H)).is_empty());
        assert_eq!(
            env.run(&c, &at(1000 + 24 * H + 1)),
            vec![Risk::LongAway { secs: 24 * H + 1 }]
        );
    }

    #[test]
    fn arrival_before_last_contact_is_not_away() {
        let env = Env::new();
        let c = change("mia", 1, modk(1), Value::Gone, file("a.jar"));
        assert!(env.run(&c, &at(10)).is_empty());
    }

    fn batch_of(n: u64) -> Vec<Change> {
        (1..=n).map(|i| removal("noah", i)).collect()
    }

    #[test]
    fn five_removals_pass_six_hold() {
        let mut env = Env::new();
        env.batch = batch_of(5);
        assert!(env.run(&env.batch[0], &at(1100)).is_empty());
        env.batch = batch_of(6);
        assert_eq!(
            env.run(&env.batch[0], &at(1100)),
            vec![Risk::ManyRemovals { count: 6 }]
        );
    }

    #[test]
    fn removals_of_other_authors_or_batches_do_not_count() {
        let mut env = Env::new();
        let mut batch = batch_of(3);
        batch.extend((10..14).map(|i| removal("mia", i)));
        let mut other = removal("noah", 20);
        other.batch = "b2".into();
        batch.push(other);
        env.batch = batch;
        assert!(env.run(&env.batch[0], &at(1100)).is_empty());
    }

    #[test]
    fn many_removals_hold_the_removals_not_the_rest() {
        let mut env = Env::new();
        env.batch = batch_of(6);
        let add = change("noah", 50, modk(50), Value::Gone, file("n.jar"));
        env.batch.push(add.clone());
        assert!(env.run(&add, &at(1100)).is_empty());
    }

    #[test]
    fn pack_back_other_pack_and_forward() {
        let env = Env::new();
        let pack = |was: &str, now: &str| {
            change("mia", 1, Key::Pack, Value::Text(was.into()), Value::Text(now.into()))
        };
        assert_eq!(env.run(&pack("cf:9/200", "cf:9/100"), &at(1100)), vec![Risk::PackBack]);
        assert_eq!(env.run(&pack("cf:9/200", "cf:7/300"), &at(1100)), vec![Risk::OtherPack]);
        assert!(env.run(&pack("cf:9/100", "cf:9/200"), &at(1100)).is_empty());
        // First pack of a copy (nothing before it) is not a move.
        let first = change("mia", 1, Key::Pack, Value::Gone, Value::Text("cf:9/100".into()));
        assert!(env.run(&first, &at(1100)).is_empty());
    }

    #[test]
    fn new_link_source_holds_until_allowed() {
        let mut env = Env::new();
        let key = Key::Mod("src:thing.jar".into());
        let add = change("mia", 1, key.clone(), Value::Gone, file("thing.jar"));
        assert_eq!(env.run(&add, &at(1100)), vec![Risk::NewSource]);
        env.allowed.insert("src:thing.jar".into());
        assert!(env.run(&add, &at(1100)).is_empty());
        // Removing a source file is never a source risk.
        let rm = change("mia", 2, key, file("thing.jar"), Value::Gone);
        env.allowed.clear();
        assert!(env.run(&rm, &at(1100)).is_empty());
    }

    #[test]
    fn ask_me_first_holds_every_record_from_that_friend() {
        let mut env = Env::new();
        env.trust.insert("mia".into(), Trust::AskFirst);
        let c = change("mia", 1, modk(1), Value::Gone, file("a.jar"));
        assert_eq!(env.run(&c, &at(1100)), vec![Risk::AskFirst]);
        let other = change("noah", 1, modk(1), Value::Gone, file("a.jar"));
        assert!(env.run(&other, &at(1100)).is_empty());
    }

    #[test]
    fn held_names_each_record_with_all_its_reasons() {
        let mut env = Env::new();
        env.trust.insert("noah".into(), Trust::AskFirst);
        let items: Vec<(Change, Arrival)> = (1..=6)
            .map(|i| (removal("noah", i), at(1000 + 25 * H)))
            .chain([(change("mia", 1, modk(90), Value::Gone, file("m.jar")), at(1100))])
            .collect();
        let ctx = GateCtx {
            limits: Limits::default(),
            trust: &env.trust,
            registers: &env.registers,
            batch: &[],
            allowed_sources: &env.allowed,
        };
        let out = held(&items, &ctx);
        assert_eq!(out.len(), 6);
        assert_eq!(
            out[0].risks,
            vec![
                Risk::LongAway { secs: 25 * H },
                Risk::ManyRemovals { count: 6 },
                Risk::AskFirst
            ]
        );
        assert!(out.iter().all(|h| h.dot.author == "noah"));
    }
}
