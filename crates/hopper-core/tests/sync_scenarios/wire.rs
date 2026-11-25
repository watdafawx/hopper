//! What travels: cumulative packets, acks, hashes, parts, limits (design §10).

use super::*;
use hopper_core::synccore::Status;

/// Covers design §10 (speed). In step, A turns one mod off in a 400-mod
/// pack: A's packet carries one record, not the list, and is small.
#[test]
fn a_small_change_sends_a_small_packet() {
    let all: Vec<_> = (1..=400).map(|n| (m(n), f(n * 10 + 1))).collect();
    let mut g = Group::with(&["A", "B"], &all);
    g.settle();
    g.edit("A", m(7), off(71));
    assert!(g.outbox_bytes("A") < 2 * 1024, "{} bytes", g.outbox_bytes("A"));
    g.settle();
    assert_eq!(g.value("B", &m(7)), off(71));
}

/// Covers design §10 (hash). B's saved state is damaged (a record lost).
/// The hashes differ, B asks for a checkpoint, and B ends in step.
#[test]
fn a_hash_mismatch_heals_with_a_checkpoint() {
    let mut g = three();
    g.edit("A", m(1), f(12));
    g.settle();
    g.forget_record("B", "A", 1);
    g.edit("A", m(2), f(22));
    g.settle();
    g.settle();
    assert_eq!(g.group_value("B", &m(1)), f(12));
    assert_eq!(g.group_hash("A"), g.group_hash("B"));
}

/// Covers W29, R7 (audit's failing `gap_a_pack_pushed_out_of_a_queue...`).
/// A shares 151 packs in one group with B, who is offline. Every pack
/// reaches B: one cumulative event per sender and group, not one per pack.
#[test]
fn w29_r7_151_packs_to_an_offline_friend_all_arrive() {
    let mut g = Group::with(&["A", "B"], &base());
    g.offline("B");
    for i in 0..151 {
        g.share_copy("A", &format!("pack-{i}"));
    }
    g.settle();
    g.online("B");
    g.settle();
    assert_eq!(g.copies("B"), 152);
    assert!(g.relay_events("B") <= 2);
}

/// Covers C3. While B is offline A sends four packets; the relay keeps only
/// the newest. It still holds all four records (packets are cumulative
/// until B acks).
#[test]
fn c3_the_newest_packet_carries_everything_unacked() {
    let mut g = three();
    g.offline("B");
    for n in 1..=4 {
        g.edit("A", m(n), f(n * 10 + 2));
        g.settle();
    }
    g.online("B");
    g.settle();
    for n in 1..=4 {
        assert_eq!(g.value("B", &m(n)), f(n * 10 + 2));
    }
}

/// Covers relay scenario s16. A checkpoint of a 3000-mod pack is over the
/// relay's body cap: it goes in parts and B folds it only once whole.
#[test]
fn a_big_checkpoint_goes_in_parts() {
    let all: Vec<_> = (1..=3000).map(|n| (m(n), f(n * 10 + 1))).collect();
    let mut g = Group::with(&["A"], &all);
    g.relay_body_cap(64 * 1024);
    g.join("B");
    g.settle();
    assert_eq!(g.value("B", &m(3000)), f(30001));
}

/// Covers W28 (relay scenarios s13, s14). The relay restarts and is down
/// a while: A's change waits in A's outbox and reaches B when it's back.
#[test]
fn w28_relay_down_then_back() {
    let mut g = three();
    g.relay_down();
    g.edit("A", m(1), f(12));
    g.settle();
    assert!(matches!(g.status("A"), Status::Sending(1)));
    g.relay_up();
    g.settle();
    assert_eq!(g.value("B", &m(1)), f(12));
}

/// Covers W36. C runs an older Hopper that sends another format. A and B
/// refuse C's packets, change nothing, and say "C's Hopper is out of date".
#[test]
fn w36_an_old_hopper_is_refused_and_named() {
    let mut g = three();
    g.old_hopper("C");
    g.edit("C", m(1), f(12));
    g.settle();
    assert_eq!(g.value("A", &m(1)), f(11));
    assert_eq!(g.status("A"), Status::OutOfDate("C".into()));
}
