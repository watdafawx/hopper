//! Joining, leaving and ending a group.

use super::*;
use hopper_core::synccore::Value;

/// Covers W16 (relay scenarios s07, s19). D joins after A, B and C made
/// changes: D's first packet in is a checkpoint and D ends in step, with
/// each mod named after who changed it last.
#[test]
fn w16_a_late_joiner_gets_the_latest_state() {
    let mut g = three();
    g.edit("A", m(1), f(12));
    g.edit("B", m(2), off(21));
    g.settle();
    g.join("D");
    g.settle();
    assert_eq!(g.value("D", &m(1)), f(12));
    assert_eq!(g.value("D", &m(2)), off(21));
    assert_eq!(g.last_by("D", &m(2)).as_deref(), Some("B"));
}

/// Covers W16. D's own changes made before D's first packet reach the group.
#[test]
fn w16_a_joiners_own_changes_reach_the_group() {
    let mut g = three();
    g.join("D");
    g.edit("D", m(7), f(71));
    g.settle();
    assert_eq!(g.value("A", &m(7)), f(71));
}

/// Covers W17 (relay scenarios s08, s09). B leaves while offline. B's copy
/// keeps its last version and stops following; the others are told, and
/// B's later changes reach nobody.
#[test]
fn w17_a_member_who_leaves_keeps_their_copy() {
    let mut g = three();
    g.offline("B");
    g.remove("A", "B");
    g.online("B");
    g.settle();
    assert!(!g.following("B"));
    assert_eq!(g.value("B", &m(1)), f(11));
    g.edit("B", m(1), f(12));
    g.settle();
    assert_eq!(g.value("A", &m(1)), f(11));
    assert!(g.notes("A").iter().any(|n| n.contains('B')));
}

/// Covers W18 (relay scenarios s10, s11). The group is deleted while C is
/// offline. On return C stops following and keeps its last version.
#[test]
fn w18_group_deleted_while_away_keeps_the_last_version() {
    let mut g = three();
    g.edit("A", m(1), f(12));
    g.settle();
    g.offline("C");
    g.delete_group("A");
    g.online("C");
    g.settle();
    assert!(!g.following("C"));
    assert_eq!(g.value("C", &m(1)), f(12));
}

/// Covers W37 (Hopper Server follower, out of scope until P10). A member
/// that only follows and never makes a change ends equal to the group and
/// sends no records of its own: the shape a server follower will use.
#[test]
fn w37_a_read_only_follower_ends_equal_and_sends_nothing() {
    let mut g = three();
    g.edit("A", m(1), f(12));
    g.edit("B", m(2), Value::Gone);
    g.settle();
    assert_eq!(g.value("C", &m(1)), f(12));
    assert_eq!(g.value("C", &m(2)), Value::Gone);
    assert_eq!(g.records_by("A", "C"), 0);
}

/// A new follower who joins after the sharer's pack recipe was already
/// sent (it arrived before the follower existed) is brought to the same
/// pack version and mods by the checkpoint, not left on the first state.
#[test]
fn a_new_follower_catches_up_on_a_recipe_that_arrived_first() {
    let mut g = Group::with(&["A", "B"], &{
        let mut v = base();
        v.push((Key::Pack, Value::Text("cf:100/8600".into())));
        v
    });
    g.update_pack("A", "cf:100/8700", hopper_core::synccore::PackMode { here: hopper_core::synccore::ConfigMode::Keep, friends: hopper_core::synccore::ConfigMode::Keep, keep_off: true });
    g.edit("A", m(8), f(81));
    g.settle();
    g.join("D");
    g.settle();
    assert_eq!(g.value("D", &Key::Pack), Value::Text("cf:100/8700".into()));
    assert_eq!(g.value("D", &m(8)), f(81));
    assert!(g.reviews("D").is_empty(), "a first catch-up asks nothing");
}
