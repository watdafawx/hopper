//! The merge rule (design §3): every record of a copy, folded per key into
//! what the group agrees. Pure; order-free and idempotent.
//!
//! `fold` is a function of the *set* of records, not of the order they came
//! in: per key it takes the records no other record follows (the frontier),
//! and picks the winner from those by a fixed order. So every PC that holds
//! the same records agrees, whatever order the relay delivered them in.

use super::model::{Arrival, Change, Dot, Key};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BTreeMap;

/// Every record of one copy this PC has seen, with how the relay numbered
/// the event that brought it (None: made here, not sent yet).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Log {
    pub changes: BTreeMap<Dot, (Change, Option<Arrival>)>,
}

impl Log {
    /// Adds a record. Returns whether it was new; the same dot twice keeps
    /// the first copy (a relay retry or a re-sent checkpoint), only filling
    /// in an arrival it didn't have.
    pub fn insert(&mut self, change: Change, arrival: Option<Arrival>) -> bool {
        match self.changes.get_mut(&change.dot) {
            Some((_, have)) => {
                if have.is_none() {
                    *have = arrival;
                }
                false
            }
            None => {
                self.changes.insert(change.dot.clone(), (change, arrival));
                true
            }
        }
    }

    /// Takes in every record of `other`.
    pub fn absorb(&mut self, other: &Log) {
        for (change, arrival) in other.changes.values() {
            self.insert(change.clone(), *arrival);
        }
    }
}

/// One key's state in the group: the winning record, and records made at
/// the same time that aren't settled yet (a clash waiting for an answer, or
/// the loser of a concurrent update, so its author can be told).
#[derive(Clone, Debug, PartialEq)]
pub struct Register {
    pub winner: Change,
    /// Concurrent records that say something other than the winner, newest
    /// first. Empty when one record stands alone.
    pub open: Vec<Change>,
}

impl Register {
    /// Whether the open records need an answer from a player (a removal or
    /// turn-off against a keep, or two pack versions), not just a note.
    pub fn is_clash(&self) -> bool {
        self.open.iter().any(|o| merge(&self.winner, o) == Merge::Clash)
    }
}

/// How an incoming record relates to a key's current record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Merge {
    /// It had seen the current one: it replaces it.
    Follows,
    /// The current one had seen it: History only.
    Stale,
    /// Both say the same.
    Same,
    /// Made at the same time, both keep the mod: the higher
    /// (lamport, author) wins, the other author is told.
    Newest,
    /// Made at the same time, one takes the mod away: keep it and ask.
    Clash,
}

pub fn merge(current: &Change, incoming: &Change) -> Merge {
    if current.dot == incoming.dot {
        return Merge::Same;
    }
    if incoming.seen.covers(&current.dot) {
        return Merge::Follows;
    }
    if current.seen.covers(&incoming.dot) {
        return Merge::Stale;
    }
    // Neither saw the other.
    if current.now == incoming.now {
        return Merge::Same;
    }
    let cur_away = current.now.takes_away(&current.was);
    let inc_away = incoming.now.takes_away(&incoming.was);
    // A removal of a mod that was already off took nothing away by its own
    // `was`, but against someone who turned it back on it still ends the mod
    // for the player who did that, so it counts as a clash too.
    let one_ends_away = current.now.is_away() != incoming.now.is_away();
    let pack_moves = current.key == Key::Pack && incoming.key == Key::Pack;
    if cur_away != inc_away || one_ends_away || pack_moves {
        Merge::Clash
    } else {
        Merge::Newest
    }
}

/// The order concurrent records are ranked in: higher lamport, then the
/// author's id, then the counter (so two records of one author can never
/// tie). Greater wins.
fn rank(a: &Change, b: &Change) -> Ordering {
    (a.lamport, &a.dot.author, a.dot.n).cmp(&(b.lamport, &b.dot.author, b.dot.n))
}

/// One key's register from all its records (at least one).
fn settle(records: &[&Change]) -> Register {
    // Frontier: records no other record follows. A follow-up, a stale
    // record and a repeat all drop out here.
    let mut front: Vec<&Change> = records
        .iter()
        .copied()
        .filter(|c| !records.iter().any(|x| x.dot != c.dot && x.seen.covers(&c.dot)))
        .collect();
    front.sort_by(|a, b| rank(b, a));
    let first = front[0];
    // A clash keeps the non-destructive side; otherwise the newest wins.
    let clash = front.iter().any(|c| merge(first, c) == Merge::Clash);
    let winner = if clash && first.key != Key::Pack && first.now.is_away() {
        front
            .iter()
            .copied()
            .find(|c| !c.now.is_away())
            .unwrap_or(first)
    } else {
        first
    };
    let open = front
        .iter()
        .copied()
        .filter(|c| c.dot != winner.dot && c.now != winner.now)
        .cloned()
        .collect();
    Register { winner: winner.clone(), open }
}

/// The register of every key, from the whole log.
pub fn fold(log: &Log) -> BTreeMap<Key, Register> {
    let mut by_key: BTreeMap<&Key, Vec<&Change>> = BTreeMap::new();
    for (change, _) in log.changes.values() {
        by_key.entry(&change.key).or_default().push(change);
    }
    by_key.into_iter().map(|(k, v)| (k.clone(), settle(&v))).collect()
}

/// A hash of the winning dot per key, to tell two PCs agree (design §10).
/// FNV-1a over a fixed byte form, so it is the same on every PC and build.
pub fn hash(registers: &BTreeMap<Key, Register>) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut eat = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x100000001b3);
        }
    };
    for (key, reg) in registers {
        let (kind, id) = match key {
            Key::Mod(s) => ("mod", s.as_str()),
            Key::Lock(s) => ("lock", s.as_str()),
            Key::Loader => ("loader", ""),
            Key::Pack => ("pack", ""),
            Key::Game => ("game", ""),
        };
        // Lengths first, so "a" + "bc" and "ab" + "c" can't collide.
        eat(kind.as_bytes());
        eat(&(id.len() as u64).to_le_bytes());
        eat(id.as_bytes());
        let author = reg.winner.dot.author.as_bytes();
        eat(&(author.len() as u64).to_le_bytes());
        eat(author);
        eat(&reg.winner.dot.n.to_le_bytes());
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synccore::model::{Seen, Value, Why};

    fn file(name: &str) -> Value {
        Value::File { file: name.into(), on: true }
    }

    fn seen(pairs: &[(&str, u64)]) -> Seen {
        Seen(pairs.iter().map(|(a, n)| (a.to_string(), *n)).collect())
    }

    fn ch(
        author: &str,
        n: u64,
        key: &str,
        was: Value,
        now: Value,
        lamport: u64,
        sn: &[(&str, u64)],
    ) -> Change {
        Change {
            dot: Dot { author: author.into(), n },
            key: Key::Mod(key.into()),
            was,
            now,
            name: author.into(),
            lamport,
            seen: seen(sn),
            online_at: 0,
            batch: format!("{author}{n}"),
            why: Why::Player,
        }
    }

    fn log_of(changes: &[Change]) -> Log {
        let mut log = Log::default();
        for c in changes {
            log.insert(c.clone(), None);
        }
        log
    }

    fn register_of(changes: &[Change]) -> Register {
        fold(&log_of(changes)).remove(&changes[0].key).unwrap()
    }

    #[test]
    fn follow_up_replaces() {
        let a = ch("mia", 1, "cf:1", Value::Gone, file("a1"), 1, &[]);
        let b = ch("noah", 1, "cf:1", file("a1"), file("a2"), 2, &[("mia", 1)]);
        assert_eq!(merge(&a, &b), Merge::Follows);
        assert_eq!(merge(&b, &a), Merge::Stale);
        let r = register_of(&[a, b.clone()]);
        assert_eq!(r.winner, b);
        assert!(r.open.is_empty());
    }

    #[test]
    fn same_dot_twice_changes_nothing() {
        let a = ch("mia", 1, "cf:1", Value::Gone, file("a1"), 1, &[]);
        assert_eq!(merge(&a, &a), Merge::Same);
        let mut log = Log::default();
        assert!(log.insert(a.clone(), None));
        assert!(!log.insert(a.clone(), Some(Arrival { seq: 4, at: 9 })));
        assert_eq!(log.changes.len(), 1);
        assert_eq!(log.changes[&a.dot].1, Some(Arrival { seq: 4, at: 9 }));
    }

    #[test]
    fn concurrent_updates_higher_lamport_wins_and_loser_is_open() {
        let a = ch("mia", 1, "cf:1", file("a1"), file("a2"), 5, &[]);
        let b = ch("noah", 1, "cf:1", file("a1"), file("a3"), 7, &[]);
        assert_eq!(merge(&a, &b), Merge::Newest);
        let r = register_of(&[a.clone(), b.clone()]);
        assert_eq!(r.winner, b);
        assert_eq!(r.open, vec![a]);
        assert!(!r.is_clash());
    }

    #[test]
    fn lamport_tie_goes_to_the_higher_author() {
        let a = ch("mia", 1, "cf:1", file("a1"), file("a2"), 5, &[]);
        let b = ch("noah", 1, "cf:1", file("a1"), file("a3"), 5, &[]);
        assert_eq!(register_of(&[a, b.clone()]).winner, b);
    }

    #[test]
    fn concurrent_same_value_is_one_value_no_question() {
        let a = ch("mia", 1, "cf:1", file("a1"), file("a2"), 5, &[]);
        let b = ch("noah", 1, "cf:1", file("a1"), file("a2"), 6, &[]);
        assert_eq!(merge(&a, &b), Merge::Same);
        let r = register_of(&[a, b]);
        assert!(r.open.is_empty());
        assert!(!r.is_clash());
    }

    #[test]
    fn update_against_removal_keeps_the_mod_and_asks() {
        let rm = ch("mia", 1, "cf:1", file("a1"), Value::Gone, 9, &[]);
        let up = ch("noah", 1, "cf:1", file("a1"), file("a2"), 3, &[]);
        assert_eq!(merge(&rm, &up), Merge::Clash);
        assert_eq!(merge(&up, &rm), Merge::Clash);
        // The removal has the higher clock but the update still stands.
        let r = register_of(&[rm.clone(), up.clone()]);
        assert_eq!(r.winner, up);
        assert_eq!(r.open, vec![rm]);
        assert!(r.is_clash());
    }

    #[test]
    fn turn_off_against_update_is_a_clash() {
        let off = ch(
            "mia",
            1,
            "cf:1",
            file("a1"),
            Value::File { file: "a1".into(), on: false },
            4,
            &[],
        );
        let up = ch("noah", 1, "cf:1", file("a1"), file("a2"), 2, &[]);
        let r = register_of(&[off, up.clone()]);
        assert_eq!(r.winner, up);
        assert!(r.is_clash());
    }

    #[test]
    fn two_removals_are_not_a_clash() {
        let a = ch("mia", 1, "cf:1", file("a1"), Value::Gone, 4, &[]);
        let b = ch("noah", 1, "cf:1", file("a1"), Value::Gone, 6, &[]);
        let r = register_of(&[a, b]);
        assert_eq!(r.winner.now, Value::Gone);
        assert!(!r.is_clash());
    }

    #[test]
    fn delete_of_an_off_mod_against_a_turn_on_keeps_the_mod_and_asks() {
        // Noah (away) deletes a mod that was off; Mia turned it on meanwhile.
        // The delete takes nothing away by its own `was`, but it must not
        // silently end Mia's newer turn-on, even with the higher clock.
        let off = Value::File { file: "a1".into(), on: false };
        let on = file("a1");
        let rm = ch("noah", 1, "cf:1", off.clone(), Value::Gone, 9, &[]);
        let turn_on = ch("mia", 1, "cf:1", off, on, 3, &[]);
        assert_eq!(merge(&rm, &turn_on), Merge::Clash);
        assert_eq!(merge(&turn_on, &rm), Merge::Clash);
        let r = register_of(&[rm.clone(), turn_on.clone()]);
        assert_eq!(r.winner, turn_on);
        assert_eq!(r.open, vec![rm]);
        assert!(r.is_clash());
    }

    #[test]
    fn answer_closes_the_clash_everywhere() {
        let rm = ch("mia", 1, "cf:1", file("a1"), Value::Gone, 9, &[]);
        let up = ch("noah", 1, "cf:1", file("a1"), file("a2"), 3, &[]);
        let mut answer =
            ch("noah", 2, "cf:1", file("a2"), file("a2"), 10, &[("mia", 1), ("noah", 1)]);
        answer.why = Why::Answer { review: 1 };
        let r = register_of(&[rm, up, answer.clone()]);
        assert_eq!(r.winner, answer);
        assert!(r.open.is_empty());
    }

    #[test]
    fn answer_removing_also_closes_it() {
        let rm = ch("mia", 1, "cf:1", file("a1"), Value::Gone, 9, &[]);
        let up = ch("noah", 1, "cf:1", file("a1"), file("a2"), 3, &[]);
        let answer =
            ch("mia", 2, "cf:1", file("a2"), Value::Gone, 10, &[("mia", 1), ("noah", 1)]);
        let r = register_of(&[up, answer.clone(), rm]);
        assert_eq!(r.winner, answer);
        assert!(!r.is_clash());
    }

    #[test]
    fn away_friend_cannot_silently_overwrite() {
        // Noah updates; Mia (who never saw it) removes. Not a follow-up.
        let up = ch("noah", 1, "cf:1", file("a1"), file("a2"), 3, &[]);
        let rm = ch("mia", 1, "cf:1", file("a1"), Value::Gone, 2, &[]);
        assert_ne!(merge(&up, &rm), Merge::Follows);
        assert_eq!(merge(&up, &rm), Merge::Clash);
    }

    #[test]
    fn pack_moves_clash_and_keep_the_newer() {
        let mk = |a: &str, v: &str, l: u64| {
            let mut c = ch(a, 1, "x", Value::Text("8.0".into()), Value::Text(v.into()), l, &[]);
            c.key = Key::Pack;
            c
        };
        let (a, b) = (mk("mia", "8.4", 3), mk("noah", "8.6", 5));
        assert_eq!(merge(&a, &b), Merge::Clash);
        let r = fold(&log_of(&[a.clone(), b.clone()])).remove(&Key::Pack).unwrap();
        assert_eq!(r.winner, b);
        assert_eq!(r.open, vec![a]);
        assert!(r.is_clash());
    }

    #[test]
    fn keys_are_independent() {
        let a = ch("mia", 1, "cf:1", Value::Gone, file("a"), 1, &[]);
        let b = ch("mia", 2, "cf:2", Value::Gone, file("b"), 2, &[("mia", 1)]);
        assert_eq!(fold(&log_of(&[a, b])).len(), 2);
    }

    #[test]
    fn hash_follows_winners_only() {
        let a = ch("mia", 1, "cf:1", Value::Gone, file("a"), 1, &[]);
        let b = ch("mia", 2, "cf:1", file("a"), file("b"), 2, &[("mia", 1)]);
        let h1 = hash(&fold(&log_of(&[a.clone()])));
        let h2 = hash(&fold(&log_of(&[a.clone(), b])));
        assert_ne!(h1, h2);
        assert_eq!(h1, hash(&fold(&log_of(&[a]))));
        assert_eq!(hash(&BTreeMap::new()).len(), 16);
    }

    // --- property-style: random causal histories, shuffled ---

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        fn shuffle<T>(&mut self, v: &mut [T]) {
            for i in (1..v.len()).rev() {
                v.swap(i, self.below(i + 1));
            }
        }
    }

    const KEYS: [&str; 3] = ["cf:1", "cf:2", "mr:x"];

    /// A random group history: members make changes to a few keys, and now
    /// and then take in everything another member knows (so `seen` is a
    /// real version vector and lamport follows the causal rule).
    fn history(rng: &mut Rng) -> Vec<Change> {
        let members = ["mia", "noah", "zed"];
        let mut known: Vec<Vec<Change>> = vec![Vec::new(); members.len()];
        let mut counter = [0u64; 3];
        let mut all = Vec::new();
        for step in 0..rng.below(25) + 10 {
            let m = rng.below(members.len());
            if rng.below(3) == 0 {
                let o = rng.below(members.len());
                for c in known[o].clone() {
                    if !known[m].iter().any(|k| k.dot == c.dot) {
                        known[m].push(c);
                    }
                }
            }
            let mut sn = BTreeMap::new();
            let mut lamport = 0;
            for c in &known[m] {
                let e = sn.entry(c.dot.author.clone()).or_insert(0);
                *e = (*e).max(c.dot.n);
                lamport = lamport.max(c.lamport);
            }
            counter[m] += 1;
            let now = match rng.below(4) {
                0 => Value::Gone,
                1 => Value::File { file: format!("f{}", rng.below(3)), on: false },
                _ => Value::File { file: format!("f{}", rng.below(3)), on: true },
            };
            let key = KEYS[rng.below(KEYS.len())];
            let mut c = ch(members[m], counter[m], key, Value::PackDefault, now, lamport + 1, &[]);
            c.seen = Seen(sn);
            c.batch = format!("b{step}");
            known[m].push(c.clone());
            all.push(c);
        }
        all
    }

    #[test]
    fn every_arrival_order_gives_the_same_registers() {
        for seed in 1..=60u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1);
            let all = history(&mut rng);
            let want = fold(&log_of(&all));
            let want_hash = hash(&want);
            for _ in 0..20 {
                let mut order = all.clone();
                rng.shuffle(&mut order);
                // Arrive one by one, with repeats, like a relay with retries.
                let mut log = Log::default();
                for (i, c) in order.iter().enumerate() {
                    log.insert(c.clone(), Some(Arrival { seq: i as u64, at: i as u64 }));
                    if rng.below(4) == 0 {
                        log.insert(c.clone(), None);
                    }
                }
                let got = fold(&log);
                assert_eq!(got, want, "seed {seed}");
                assert_eq!(hash(&got), want_hash);
            }
        }
    }

    #[test]
    fn union_of_logs_is_commutative_associative_idempotent() {
        for seed in 1..=40u64 {
            let mut rng = Rng(seed.wrapping_mul(0xD1B54A32D192ED03) | 1);
            let all = history(&mut rng);
            // Split into three overlapping logs that together hold everything.
            let mut parts = [Log::default(), Log::default(), Log::default()];
            for c in &all {
                let mut put = false;
                for p in parts.iter_mut() {
                    if rng.below(2) == 0 {
                        p.insert(c.clone(), None);
                        put = true;
                    }
                }
                if !put {
                    parts[rng.below(3)].insert(c.clone(), None);
                }
            }
            let fold_of = |order: &[usize]| {
                let mut log = Log::default();
                for i in order {
                    log.absorb(&parts[*i]);
                }
                fold(&log)
            };
            let want = fold_of(&[0, 1, 2]);
            for order in [
                [0, 2, 1],
                [1, 0, 2],
                [1, 2, 0],
                [2, 0, 1],
                [2, 1, 0],
            ] {
                assert_eq!(fold_of(&order), want, "seed {seed} order {order:?}");
            }
            // Idempotent: absorbing the same logs again changes nothing.
            assert_eq!(fold_of(&[0, 1, 2, 2, 1, 0]), want, "seed {seed}");
            // Associative: (0+1)+2 equals 0+(1+2).
            let mut left = Log::default();
            left.absorb(&parts[0]);
            left.absorb(&parts[1]);
            left.absorb(&parts[2]);
            let mut right_inner = Log::default();
            right_inner.absorb(&parts[1]);
            right_inner.absorb(&parts[2]);
            let mut right = parts[0].clone();
            right.absorb(&right_inner);
            assert_eq!(fold(&left), fold(&right), "seed {seed}");
        }
    }

    #[test]
    fn a_follow_up_to_everything_always_wins_alone() {
        for seed in 1..=40u64 {
            let mut rng = Rng(seed.wrapping_mul(0xA24BAED4963EE407) | 1);
            let mut all = history(&mut rng);
            let mut sn = BTreeMap::new();
            let mut lamport = 0;
            for c in &all {
                let e = sn.entry(c.dot.author.clone()).or_insert(0);
                *e = (*e).max(c.dot.n);
                lamport = lamport.max(c.lamport);
            }
            // One answer per key that saw everything.
            for (i, key) in KEYS.iter().enumerate() {
                let mut a =
                    ch("answer", i as u64 + 1, key, Value::PackDefault, file("final"), lamport + 1, &[]);
                a.seen = Seen(sn.clone());
                all.push(a);
            }
            rng.shuffle(&mut all);
            for reg in fold(&log_of(&all)).values() {
                assert_eq!(reg.winner.dot.author, "answer", "seed {seed}");
                assert!(reg.open.is_empty());
            }
        }
    }
}
