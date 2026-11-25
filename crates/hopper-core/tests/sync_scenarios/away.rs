//! Friends back after time away, and the gate for risky changes (design §4, §8).

use super::*;
use hopper_core::synccore::{Answer, Risk, Value};

/// Covers W11, R1, D2. B goes offline for three days and turns off mod 2
/// and updates mod 3 while away (nobody else touched them). On return,
/// A and C get one card "B was away for 3 days. B made 2 changes." and
/// nothing lands until they answer; "Take them" lands both.
#[test]
fn w11_changes_from_a_long_away_friend_wait_for_ok() {
    let mut g = three();
    g.offline("B");
    g.advance(3 * DAY);
    g.edits("B", &[(m(2), off(21)), (m(3), f(32))]);
    g.online("B");
    g.settle();
    assert_eq!(g.value("A", &m(2)), f(21));
    let r = g.reviews("A");
    assert_eq!(r.len(), 1);
    assert!(matches!(r[0].risk, Risk::LongAway { .. }));
    g.answer("A", r[0].id, Answer::Take, false);
    g.settle();
    assert_eq!(g.value("A", &m(2)), off(21));
    assert_eq!(g.value("A", &m(3)), f(32));
}

/// Covers W11, R5. Same as above; A answers "Keep ours". A follow-up record
/// puts mods 2 and 3 back for the whole group, B included, named A.
#[test]
fn w11_keep_ours_puts_it_back_for_everyone() {
    let mut g = three();
    g.offline("B");
    g.advance(3 * DAY);
    g.edits("B", &[(m(2), off(21)), (m(3), f(32))]);
    g.online("B");
    g.settle();
    let id = g.reviews("A")[0].id;
    g.answer("A", id, Answer::Keep, false);
    g.settle();
    for p in ["A", "B", "C"] {
        assert_eq!(g.value(p, &m(2)), f(21));
        assert_eq!(g.value(p, &m(3)), f(31));
    }
}

/// Covers R5, S27 of the audit. Online, B removes 12 mods in one action.
/// A and C get one "B removed 12 mods at once" card; nothing is removed
/// on their PCs until they answer.
#[test]
fn r5_many_removals_at_once_ask_first() {
    let mut g = Group::with(&["A", "B", "C"], &(1..=12).map(|n| (m(n), f(n * 10 + 1))).collect::<Vec<_>>());
    let gone: Vec<_> = (1..=12).map(|n| (m(n), Value::Gone)).collect();
    g.edits("B", &gone);
    g.settle();
    let r = g.reviews("A");
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].risk, Risk::ManyRemovals { count: 12 });
    assert_eq!(g.value("A", &m(1)), f(11));
}

/// Covers R5, D6. Five removals in one go is not "many": they land.
#[test]
fn r5_five_removals_land_by_themselves() {
    let mut g = three();
    let gone: Vec<_> = (1..=5).map(|n| (m(n), Value::Gone)).collect();
    g.edits("B", &gone);
    g.settle();
    assert!(g.reviews("A").is_empty());
    assert_eq!(g.value("A", &m(5)), Value::Gone);
}

/// Covers design §8. B is away two days while A makes 5 changes and C 2.
/// On return B gets a summary note naming both, and the changes land.
#[test]
fn back_from_away_shows_a_summary() {
    let mut g = three();
    g.offline("B");
    g.edits("A", &[(m(1), f(12)), (m(2), f(22)), (m(3), f(32)), (m(4), f(42)), (m(5), f(52))]);
    g.edits("C", &[(m(6), f(62)), (m(7), f(71))]);
    g.settle();
    g.advance(2 * DAY);
    g.online("B");
    g.settle();
    let notes = g.notes("B");
    assert!(notes.iter().any(|n| n.contains("A made 5 changes") && n.contains("C 2")));
    assert_eq!(g.value("B", &m(7)), f(71));
}

/// Covers R5. C sets B to "Ask me first": every record from B waits on C,
/// even a harmless add, while A gets it by itself.
#[test]
fn r5_ask_me_first_per_friend() {
    use hopper_core::synccore::Trust;
    let mut g = three();
    g.set_trust("C", "B", Trust::AskFirst);
    g.edit("B", m(7), f(71));
    g.settle();
    assert_eq!(g.value("A", &m(7)), f(71));
    assert_eq!(g.value("C", &m(7)), Value::Gone);
    assert_eq!(g.reviews("C")[0].risk, Risk::AskFirst);
}

/// Covers R1. A's own changes made offline are still A's when they go out
/// a day later, with A's name and the `seen` A had then (so receivers can
/// tell they were made before A saw B's newer change).
#[test]
fn r1_offline_changes_keep_what_their_author_had_seen() {
    let mut g = three();
    g.offline("A");
    g.edit("A", m(4), off(41));
    g.edit("B", m(4), f(42));
    g.settle();
    g.advance(DAY + 60);
    g.online("A");
    g.settle();
    for p in ["A", "B", "C"] {
        assert_eq!(g.value(p, &m(4)), f(42), "{p} keeps B's newer update");
        assert!(!g.reviews(p).is_empty(), "{p} is asked about A's old turn-off");
    }
}
