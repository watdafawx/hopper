//! Undo: one History row, or one friend's changes at once (design §9).

use super::*;
use hopper_core::synccore::{Key, Status, Value, Why, cards};

/// Covers W15. A removes mod 2; B undoes it from History. Mod 2 is back on
/// every PC, named B, and a later change by C still reaches A.
#[test]
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

/// Two friends. A updates mod 1; B undoes that update from History (the
/// app's save only knows "undo"). It travels like any change: A goes back
/// to B's file with no card, and A's History says B put it back, as an
/// undo of A's own update. B's PC never reads as differing.
#[test]
fn a_history_undo_of_an_update_reaches_the_friend() {
    let mut g = Group::with(&["A", "B"], &base());
    g.edit("A", m(1), f(12));
    g.settle();
    assert_eq!(g.value("B", &m(1)), f(12));
    g.history_undo("B", &m(1), f(11));
    g.settle();
    assert_eq!(g.value("A", &m(1)), f(11));
    assert!(g.reviews("A").is_empty());
    assert_eq!(g.status("B"), Status::InSync);
    assert_eq!(g.status("A"), Status::InSync);
    let last = g.engine("A").last_change(&m(1)).cloned().unwrap();
    assert_eq!(last.name, "B");
    assert!(matches!(&last.why, Why::Undo { of } if of.author == "A"));
    assert_eq!(cards::why_line(g.engine("A"), &last).as_deref(), Some("B went back to the earlier version."));
    assert_eq!(g.history("A").last().map(|h| h.says.clone()).as_deref(), Some("Put back by B"));
}

/// The same for a mod from a GitHub link ("src:" key): A already has the
/// link's mod, so B going back to its older file is not a new source to
/// allow; it lands on A.
#[test]
fn a_history_undo_of_a_link_mod_lands_without_asking() {
    let key = Key::Mod("src:github:fox/tweaks".into());
    let file = |n: u64| Value::File { file: format!("link:{n}"), on: true };
    let mut mods = base();
    mods.push((key.clone(), file(142)));
    let mut g = Group::with(&["A", "B"], &mods);
    g.edit("A", key.clone(), file(143));
    g.settle();
    assert_eq!(g.value("B", &key), file(143));
    g.history_undo("B", &key, file(142));
    g.settle();
    assert!(g.reviews("A").is_empty(), "{:?}", g.reviews("A"));
    assert_eq!(g.value("A", &key), file(142));
}

/// B swapped mod 2 for an older file outside the sync (it reads "This PC
/// differs"). "Use mine for everyone" makes B's file the group's: A gets it,
/// B is in step, and A's History says why.
#[test]
fn use_mine_for_everyone_turns_another_file_into_the_groups() {
    let mut g = Group::with(&["A", "B"], &base());
    g.outside("B", m(2), f(20));
    g.settle();
    assert_eq!(g.status("B"), Status::Differs(1));
    assert_eq!(g.value("A", &m(2)), f(21));
    g.use_mine("B");
    g.settle();
    assert_eq!(g.value("A", &m(2)), f(20));
    assert_eq!(g.status("B"), Status::InSync);
    assert_eq!(g.status("A"), Status::InSync);
    let last = g.engine("A").last_change(&m(2)).cloned().unwrap();
    assert_eq!(cards::why_line(g.engine("A"), &last).as_deref(), Some("B had another version of it and chose Use mine for everyone."));
}
