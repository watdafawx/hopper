//! Only Hopper-made changes count as the player's; the disk is drift
//! (design §5), and status comes from the disk.

use super::*;
use hopper_core::synccore::{Status, Value};

/// Covers W31, R2. A jar is deleted from A's mods folder in Explorer.
/// Nothing reaches B or C; A sees "This PC differs" for mod 2.
#[test]
#[ignore = "sync-rewrite"]
fn w31_a_jar_deleted_outside_hopper_is_never_sent() {
    let mut g = three();
    g.outside("A", m(2), Value::Gone);
    g.settle();
    assert_eq!(g.value("B", &m(2)), f(21));
    assert_eq!(g.status("A"), Status::Differs(1));
    assert_eq!(g.drift("A").len(), 1);
}

/// Covers W31, R2. "Put them back" restores the group's file on A and
/// sends nothing.
#[test]
#[ignore = "sync-rewrite"]
fn w31_put_back_restores_without_sending() {
    let mut g = three();
    g.outside("A", m(2), Value::Gone);
    g.put_back("A");
    g.settle();
    assert_eq!(g.value("A", &m(2)), f(21));
    assert_eq!(g.status("A"), Status::InSync);
    assert!(g.history("B").iter().all(|h| h.key != m(2)));
}

/// Covers W31. "Remove for everyone" turns the drift into A's change.
#[test]
#[ignore = "sync-rewrite"]
fn w31_remove_for_everyone_shares_the_drift() {
    let mut g = three();
    g.outside("A", m(2), Value::Gone);
    g.share_drift("A");
    g.settle();
    assert_eq!(g.value("B", &m(2)), Value::Gone);
    assert_eq!(g.last_by("B", &m(2)).as_deref(), Some("A"));
}

/// Covers R2, S27 of the audit. A crash leaves A's mods folder half empty
/// (6 of 12 gone). Nothing goes out; A sees one "6 mods are missing" card.
#[test]
#[ignore = "sync-rewrite"]
fn r2_a_half_empty_mods_folder_sends_nothing() {
    let all: Vec<_> = (1..=12).map(|n| (m(n), f(n * 10 + 1))).collect();
    let mut g = Group::with(&["A", "B"], &all);
    for n in 1..=6 {
        g.outside("A", m(n), Value::Gone);
    }
    g.settle();
    assert_eq!(g.value("B", &m(1)), f(11));
    assert_eq!(g.status("A"), Status::Differs(6));
}

/// Covers W31. The CurseForge app swaps mod 3's file on A. Drift, not A's
/// update.
#[test]
#[ignore = "sync-rewrite"]
fn w31_another_launcher_swapping_a_file_is_drift() {
    let mut g = three();
    g.outside("A", m(3), f(32));
    g.settle();
    assert_eq!(g.value("B", &m(3)), f(31));
    assert_eq!(g.status("A"), Status::Differs(1));
}

/// Covers C2. B removes mod 5; A then saves an older in-memory mod list
/// over its instance (the stale-list bug). Mod 5 does not come back for
/// anyone: nothing publishes lists, and the stale write is drift on A.
#[test]
#[ignore = "sync-rewrite"]
fn c2_a_stale_list_written_to_disk_brings_nothing_back() {
    let mut g = three();
    g.edit("B", m(5), Value::Gone);
    g.settle();
    g.outside("A", m(5), f(51));
    g.settle();
    assert_eq!(g.value("B", &m(5)), Value::Gone);
    assert_eq!(g.value("C", &m(5)), Value::Gone);
}

/// Covers C2, R3. A friend's change taken in on B is B's disk now, but it
/// stays the friend's: B's next packet doesn't send it as B's, and History
/// everywhere names A only.
#[test]
fn c2_a_taken_in_change_never_echoes_as_mine() {
    let mut g = three();
    g.edit("A", m(1), f(12));
    g.settle();
    g.settle();
    for p in ["A", "B", "C"] {
        assert!(g.history(p).iter().filter(|h| h.key == m(1)).all(|h| h.by == "A"));
    }
    assert_eq!(g.last_by("C", &m(1)).as_deref(), Some("A"));
}

/// Covers C6. B's download of mod 7 is blocked by its author. B's status
/// is not "in sync" (it says what waits), and A sees that B lacks mod 7.
#[test]
#[ignore = "sync-rewrite"]
fn c6_status_comes_from_the_disk_not_bookkeeping() {
    use hopper_core::synccore::apply::Outcome;
    let mut g = three();
    g.fail_fetch("B", &m(7), Outcome::Blocked);
    g.edit("A", m(7), f(71));
    g.settle();
    assert_ne!(g.status("B"), Status::InSync);
    assert!(g.member_missing("A", "B").contains(&m(7)));
}

/// Covers C7. B's instance can't be read (folder locked). B sends nothing,
/// not a blank list; A and C keep everything.
#[test]
#[ignore = "sync-rewrite"]
fn c7_an_unreadable_instance_sends_nothing() {
    let mut g = three();
    g.unreadable("B", true);
    g.settle();
    assert_eq!(g.value("A", &m(1)), f(11));
    assert!(g.reviews("A").is_empty());
}
