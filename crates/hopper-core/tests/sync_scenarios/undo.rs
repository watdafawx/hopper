//! Undo: one History row, or one friend's changes at once (design §9).

use super::*;
use hopper_core::synccore::Value;

/// Covers W15. A removes mod 2; B undoes it from History. Mod 2 is back on
/// every PC, named B, and a later change by C still reaches A.
#[test]
#[ignore = "sync-rewrite"]
fn w15_undo_a_friends_change_reaches_everyone() {
    let mut g = three();
    g.edit("A", m(2), Value::Gone);
    g.settle();
    g.undo_last("B", &m(2));
    g.settle();
    assert_eq!(g.value("A", &m(2)), f(21));
    assert_eq!(g.last_by("C", &m(2)).as_deref(), Some("B"));
    g.edit("C", m(3), f(32));
    g.settle();
    assert_eq!(g.value("A", &m(3)), f(32));
}

/// Covers R5. C makes 3 changes today. A presses "Put back C's changes":
/// all three go back everywhere as one batch named A.
#[test]
#[ignore = "sync-rewrite"]
fn r5_put_back_one_friends_changes_in_one_click() {
    let mut g = three();
    g.advance(DAY);
    let since = g.now();
    g.edits("C", &[(m(1), f(12)), (m(2), off(21)), (m(3), Value::Gone)]);
    g.settle();
    g.undo_member("A", "C", since);
    g.settle();
    for p in ["A", "B", "C"] {
        assert_eq!(g.value(p, &m(1)), f(11));
        assert_eq!(g.value(p, &m(2)), f(21));
        assert_eq!(g.value(p, &m(3)), f(31));
    }
}

/// Covers R5. Putting back C's changes leaves alone a key B changed after
/// C, and says so.
#[test]
#[ignore = "sync-rewrite"]
fn r5_put_back_skips_what_someone_changed_since() {
    let mut g = three();
    let since = g.now();
    g.edits("C", &[(m(1), f(12)), (m(2), f(22))]);
    g.settle();
    g.edit("B", m(2), f(23));
    g.settle();
    g.undo_member("A", "C", since);
    g.settle();
    assert_eq!(g.value("A", &m(1)), f(11));
    assert_eq!(g.value("A", &m(2)), f(23));
    assert!(g.notes("A").iter().any(|n| n.contains("changed since")));
}

/// Covers C3. While B is offline, A turns mod 1 off and back on before B
/// returns (the relay only holds A's newest event). B ends on mod 1 on,
/// and its History shows both changes.
#[test]
#[ignore = "sync-rewrite"]
fn c3_made_and_undone_while_away_both_arrive() {
    let mut g = three();
    g.offline("B");
    g.edit("A", m(1), off(11));
    g.settle();
    g.edit("A", m(1), f(11));
    g.settle();
    g.online("B");
    g.settle();
    assert_eq!(g.value("B", &m(1)), f(11));
    assert_eq!(g.history("B").iter().filter(|h| h.key == m(1)).count(), 2);
}

/// Covers C1, C3 (audit's `gap_a_members_undo_then_another_change...`). B
/// undoes A's update of mod 1 and then A changes mod 2: the undo still
/// reaches A (an undo used to match the last code, so nobody saw it).
#[test]
#[ignore = "sync-rewrite"]
fn c3_an_undo_followed_by_another_change_still_arrives() {
    let mut g = three();
    g.edit("A", m(1), f(12));
    g.settle();
    g.undo_last("B", &m(1));
    g.edit("A", m(2), f(22));
    g.settle();
    assert_eq!(g.value("A", &m(1)), f(11));
    assert_eq!(g.value("B", &m(2)), f(22));
}
