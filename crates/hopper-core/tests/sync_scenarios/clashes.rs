//! Two people changing the same mod at the same time (design §3).

use super::*;
use hopper_core::synccore::{Answer, Risk, Value};

/// Covers W13, D5. A and B, both online, update mod 1 at once (A to 12,
/// B to 13) before either sees the other. Every PC ends on the same file
/// (higher Lamport clock, then author), with no card, and the author whose
/// change lost is told.
#[test]
fn w13_same_mod_both_online_ends_the_same_everywhere() {
    let mut g = three();
    g.edit("A", m(1), f(12));
    g.edit("B", m(1), f(13));
    g.settle();
    let a = g.value("A", &m(1));
    assert_eq!(g.value("B", &m(1)), a);
    assert_eq!(g.value("C", &m(1)), a);
    assert!(g.reviews("C").is_empty());
    let loser = if a == f(12) { "B" } else { "A" };
    assert!(g.notes(loser).iter().any(|n| n.contains("changed this too")));
}

/// Covers W14, R1. A and B are both offline. B updates mod 1 first, A
/// later. Whoever reconnects first, every PC ends on the same file: the
/// order they come back in decides nothing.
#[test]
fn w14_same_mod_both_offline_reconnect_order_decides_nothing() {
    for a_first in [true, false] {
        let mut g = three();
        g.offline("A");
        g.offline("B");
        g.edit("B", m(1), f(12));
        g.advance(60);
        g.edit("A", m(1), f(13));
        let (x, y) = if a_first { ("A", "B") } else { ("B", "A") };
        g.online(x);
        g.settle();
        g.online(y);
        g.settle();
        let end = g.value("C", &m(1));
        assert_eq!(g.value("A", &m(1)), end);
        assert_eq!(g.value("B", &m(1)), end);
    }
}

/// Covers R1, D1 (the audit's failing `gap_the_change_made_last_wins...`).
/// B is offline. A updates mod 1. B, not having seen it, removes mod 1 and
/// comes back. Nobody loses mod 1: every PC keeps A's update and shows a
/// card "B removed ... You updated it" until someone answers.
#[test]
fn r1_update_vs_removal_asks_and_keeps_the_mod() {
    let mut g = three();
    g.offline("B");
    g.edit("A", m(1), f(12));
    g.settle();
    g.edit("B", m(1), Value::Gone);
    g.online("B");
    g.settle();
    for p in ["A", "B", "C"] {
        assert_eq!(g.value(p, &m(1)), f(12), "{p} kept the mod");
        assert!(g.reviews(p).iter().any(|r| r.risk == Risk::Clash && r.keys.contains(&m(1))), "{p} is asked");
    }
}

/// Covers R1, D1. Same clash; C answers "Remove it". The answer is a
/// record that had seen both, so the card closes on every PC and mod 1 is
/// gone everywhere, named after C.
#[test]
fn r1_the_first_answer_settles_the_clash_for_everyone() {
    let mut g = three();
    g.offline("B");
    g.edit("A", m(1), f(12));
    g.settle();
    g.edit("B", m(1), Value::Gone);
    g.online("B");
    g.settle();
    let id = g.reviews("C")[0].id;
    g.answer("C", id, Answer::Take, false);
    g.settle();
    for p in ["A", "B", "C"] {
        assert_eq!(g.value(p, &m(1)), Value::Gone);
        assert!(g.reviews(p).is_empty());
    }
    assert_eq!(g.last_by("A", &m(1)).as_deref(), Some("C"));
}

/// Covers C4. A and B change mod 1 within a second of each other and B's
/// publish is slow (offline for a minute). The old 10 s hold dropped A's
/// change; now neither change is ever lost: B ends on the same file as A
/// and History on both lists both changes.
#[test]
fn c4_a_slow_publish_never_drops_a_friends_change() {
    let mut g = three();
    g.edit("A", m(1), f(12));
    g.offline("B");
    g.edit("B", m(1), off(11));
    g.settle();
    g.advance(60);
    g.online("B");
    g.settle();
    assert_eq!(g.value("A", &m(1)), g.value("B", &m(1)));
    for p in ["A", "B"] {
        let h = g.history(p);
        assert!(h.iter().any(|r| r.key == m(1) && r.by == "A"));
        assert!(h.iter().any(|r| r.key == m(1) && r.by == "B"));
    }
}

/// Covers R1. Folding the same records in any order gives the same group
/// state: C receives A's and B's packets in both orders across two runs.
#[test]
fn r1_arrival_order_never_changes_the_result() {
    let mut ends = Vec::new();
    for a_first in [true, false] {
        let mut g = three();
        g.offline("C");
        g.edit("A", m(2), off(21));
        g.edit("B", m(2), f(22));
        g.edit("A", m(3), f(32));
        g.deliver_order("C", if a_first { &["A", "B"] } else { &["B", "A"] });
        g.online("C");
        g.settle();
        ends.push((g.group_value("C", &m(2)), g.group_value("C", &m(3))));
    }
    assert_eq!(ends[0], ends[1]);
}
