//! The group as a drop box (user report 2026-11-26): a change a member makes
//! reaches friends who are offline and lands when they come back, and a
//! friend's older state never silently rolls back a newer change.
//!
//! The report: the player updated five FTB mods of the pack (FTB Filter
//! System 21.1.4 to 21.1.5 and four more), then closed Hopper. When they came
//! back, History had five rows "QuietCarrot rolled back <mod> 21.1.5 ->
//! 21.1.4", all in the same minute. The player's saved sync state showed why:
//! QuietCarrot's PC had taken the five updates in (its records' `seen`
//! covered them) and then wrote five records of its own putting each mod back
//! to the pack's file. Those were follow-ups, so they landed everywhere.
//! They came from applying the update itself: the installer's save inside a
//! friend's apply was read as the player's own change ("the pack's own file
//! again" on an older Hopper), see `bridge::InstFetch::apply_other`.

use super::*;
use hopper_core::synccore::{Answer, Risk, Value};

/// Mods 1 to 5 as a modpack ships them.
fn pack_group() -> Group {
    let mods: Vec<_> = (1..=5).map(|n| (m(n), Value::PackDefault)).collect();
    Group::with(&["A", "B", "C"], &mods)
}

/// A's update of every pack mod to its second file.
fn updates() -> Vec<(hopper_core::synccore::Key, Value)> {
    (1..=5).map(|n| (m(n), f(n * 10 + 2))).collect()
}

/// A updates five mods while offline and closes Hopper before anything was
/// sent. On the next start the records are still there and go out; B, who
/// was away two days, gets them from the relay while A is offline again,
/// with the "While you were away" summary.
#[test]
fn dropbox_changes_made_before_closing_reach_an_offline_friend() {
    let mut g = pack_group();
    g.offline("B");
    g.offline("A");
    g.edits("A", &updates());
    g.settle();
    g.restart("A");
    g.advance(DAY);
    g.online("A");
    g.settle();
    assert_eq!(g.value("C", &m(1)), f(12), "C, online, has A's update");
    // A closes Hopper again; B comes back a day later.
    g.offline("A");
    g.advance(DAY + 60);
    g.online("B");
    g.settle();
    for n in 1..=5 {
        assert_eq!(g.value("B", &m(n)), f(n * 10 + 2), "B took A's update of mod {n} from the relay");
    }
    assert!(g.notes("B").iter().any(|s| s.contains("While you were away: A made 5 changes")), "{:?}", g.notes("B"));
    assert!(g.reviews("B").is_empty(), "nothing of A's waits on B: {:?}", g.reviews("B"));
}

/// The report itself. B took A's five updates in, then B's PC wrote five
/// records of its own that put each mod back to the pack's file (what an
/// older Hopper did when it applied the update). On A and C nothing rolls
/// back by itself: one card asks, and "Keep ours" puts the updates back for
/// the whole group.
#[test]
fn dropbox_a_friends_put_back_of_an_update_asks_first() {
    let mut g = pack_group();
    g.edits("A", &updates());
    g.settle();
    assert_eq!(g.value("B", &m(1)), f(12));
    g.edits("B", &(1..=5).map(|n| (m(n), Value::PackDefault)).collect::<Vec<_>>());
    g.settle();
    for p in ["A", "C"] {
        for n in 1..=5 {
            assert_eq!(g.value(p, &m(n)), f(n * 10 + 2), "{p}'s mod {n} was rolled back without asking");
        }
        let r = g.reviews(p);
        assert_eq!(r.len(), 1, "{p}: one card for B's batch: {r:?}");
        assert_eq!(r[0].risk, Risk::MovedBack);
        assert_eq!(r[0].keys.len(), 5);
    }
    let id = g.reviews("A")[0].id;
    g.answer("A", id, Answer::Keep, false);
    g.settle();
    for p in ["A", "B", "C"] {
        for n in 1..=5 {
            assert_eq!(g.value(p, &m(n)), f(n * 10 + 2), "{p} keeps A's update of mod {n}");
        }
        assert!(g.reviews(p).is_empty(), "{p}: {:?}", g.reviews(p));
    }
}

/// "Take it" on the same card lets B's put back land.
#[test]
fn dropbox_taking_a_friends_put_back_lands_it() {
    let mut g = pack_group();
    g.edit("A", m(1), f(12));
    g.settle();
    g.edit("B", m(1), Value::PackDefault);
    g.settle();
    let id = g.reviews("A")[0].id;
    g.answer("A", id, Answer::Take, false);
    g.settle();
    assert_eq!(g.value("A", &m(1)), Value::PackDefault);
}

/// An undo from History is the friend saying so in as many words: it lands
/// as before (design §9), only a plain put back asks.
#[test]
fn dropbox_a_friends_history_undo_still_lands() {
    let mut g = three();
    g.edit("A", m(1), f(12));
    g.settle();
    g.undo_last("B", &m(1));
    g.settle();
    assert_eq!(g.value("A", &m(1)), f(11));
    assert!(g.reviews("A").is_empty());
}

/// B's own later change of the same mod is no put back of anyone's: B
/// updates mod 1 and goes back to the old file; nothing asks.
#[test]
fn dropbox_moving_back_your_own_update_asks_nobody() {
    let mut g = three();
    g.edit("B", m(1), f(12));
    g.settle();
    g.edit("B", m(1), f(11));
    g.settle();
    assert_eq!(g.value("A", &m(1)), f(11));
    assert!(g.reviews("A").is_empty());
}

/// B updated mod 1 to an older file while offline, before A's newer update
/// reached B. When B comes back (the same day, so no away card), the newer
/// file stays on every PC and B is told; A's mod is never rolled back.
#[test]
fn dropbox_a_returning_friends_older_file_never_rolls_back_a_newer_one() {
    let mut g = three();
    g.offline("B");
    g.edit("B", m(1), f(12));
    g.edit("A", m(1), f(13));
    g.settle();
    g.advance(3600);
    g.online("B");
    g.settle();
    for p in ["A", "B", "C"] {
        assert_eq!(g.value(p, &m(1)), f(13), "{p} keeps the newer file");
    }
    assert!(g.notes("B").iter().any(|n| n.contains("A changed this too")), "{:?}", g.notes("B"));
}

/// The same, the other way round in ids: whoever is named higher, the newer
/// file wins (the old rule let the higher member id win a tie).
#[test]
fn dropbox_the_newer_file_wins_whoever_made_it() {
    let mut g = three();
    g.offline("A");
    g.edit("A", m(1), f(12));
    g.edit("B", m(1), f(13));
    g.settle();
    g.advance(3600);
    g.online("A");
    g.settle();
    for p in ["A", "B", "C"] {
        assert_eq!(g.value(p, &m(1)), f(13), "{p} keeps the newer file");
    }
}

/// B, away three days, comes back with a put back of A's update made while
/// away: it is a clash-free follow-up of nothing new (B never saw A's update),
/// so A's update stays and B is moved to it.
#[test]
fn dropbox_a_long_away_friends_old_state_stays_stale() {
    let mut g = pack_group();
    g.offline("B");
    g.edit("A", m(1), f(12));
    g.settle();
    g.advance(3 * DAY);
    g.online("B");
    g.settle();
    for p in ["A", "B", "C"] {
        assert_eq!(g.value(p, &m(1)), f(12), "{p} has A's update");
    }
    assert!(g.reviews("A").is_empty());
}

/// B put mod 1 back after A's update of it. The card names the mod (by the
/// file this PC has, not the old one B put it back to), says "1 mod", and
/// shows the mod's key so the app can find it by that too.
#[test]
fn dropbox_a_put_back_card_names_its_one_mod() {
    let mut g = pack_group();
    g.edit("A", m(1), f(12));
    g.settle();
    g.edit("B", m(1), Value::PackDefault);
    g.settle();
    let r = g.reviews("A");
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].says, "B put 1 mod back to an older version.");
    let cards = hopper_core::synccore::cards::cards(g.engine("A"));
    assert_eq!(cards.len(), 1);
    let c = &cards[0];
    assert_eq!(c.headline, format!("B put {} back to an older version.", hopper_core::synccore::cards::MOD));
    assert_eq!(c.mods.len(), 1);
    assert_eq!(c.mods[0].key, m(1));
    assert_eq!(c.mods[0].file, "cf:12", "named by the file this PC has");
    assert_eq!(c.mods[0].note, "put back");
    let labels: Vec<&str> = c.buttons.iter().map(|b| b.label.as_str()).collect();
    assert_eq!(labels, ["Keep the update", "Move back too"]);
}

/// B put three mods back one by one (each from the mods tab is an action of
/// its own): one card, "3 mods", the three names, and "Do the same for the
/// other N" is for cards of other friends, not for B's own.
#[test]
fn dropbox_put_backs_made_one_by_one_are_one_card_with_every_name() {
    let mut g = pack_group();
    g.edits("A", &(1..=3).map(|n| (m(n), f(n * 10 + 2))).collect::<Vec<_>>());
    g.settle();
    for n in 1..=3 {
        g.edit("B", m(n), Value::PackDefault);
    }
    g.settle();
    for p in ["A", "C"] {
        let r = g.reviews(p);
        assert_eq!(r.len(), 1, "{p}: {r:?}");
        assert_eq!(r[0].says, "B put 3 mods back to older versions.");
        let cards = hopper_core::synccore::cards::cards(g.engine(p));
        assert_eq!(cards.len(), 1);
        let c = &cards[0];
        assert_eq!(c.headline, "B put 3 mods back to older versions.");
        assert_eq!(c.others, 0);
        let files: Vec<&str> = c.mods.iter().map(|x| x.file.as_str()).collect();
        assert_eq!(files, ["cf:12", "cf:22", "cf:32"], "{p}: every row has a file this PC knows");
        let labels: Vec<&str> = c.buttons.iter().map(|b| b.label.as_str()).collect();
        assert_eq!(labels, ["Keep the updates", "Move back too"]);
    }
    // A later put-back by B joins the same card.
    g.edit("A", m(4), f(42));
    g.settle();
    g.edit("B", m(4), Value::PackDefault);
    g.settle();
    let r = g.reviews("A");
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].keys.len(), 4);
}

/// "Move back too" is the player's own answer: A's History says A moved the
/// mods back (a record of A's with the reason "moved it back too, as B did"),
/// not that B rolled them back, and an Undo of it is still possible. It moves
/// the group, like "Keep the updates" does.
#[test]
fn dropbox_moving_back_too_is_the_players_own_answer() {
    let mut g = pack_group();
    g.edits("A", &(1..=3).map(|n| (m(n), f(n * 10 + 2))).collect::<Vec<_>>());
    g.settle();
    g.edits("B", &(1..=3).map(|n| (m(n), Value::PackDefault)).collect::<Vec<_>>());
    g.settle();
    let id = g.reviews("A")[0].id;
    g.answer("A", id, Answer::Take, false);
    g.settle();
    for n in 1..=3 {
        assert_eq!(g.value("A", &m(n)), Value::PackDefault, "mod {n} moved back on A");
        let last = g.engine("A").last_change(&m(n)).cloned().unwrap();
        assert_eq!(last.dot.author, "A", "the move back is A's own record, not B's");
        assert_eq!(last.name, "A");
        assert!(matches!(last.why, hopper_core::synccore::Why::Answer { review } if review == id), "{:?}", last.why);
        assert_eq!(
            hopper_core::synccore::cards::why_line(g.engine("A"), &last).as_deref(),
            Some("You moved it back too, as B did, when asked.")
        );
    }
    assert!(g.reviews("A").is_empty());
    assert!(g.history("A").iter().rev().take(3).all(|h| h.by == "A"), "{:?}", g.history("A"));
    // The group follows the answer.
    assert_eq!(g.value("C", &m(1)), Value::PackDefault);
    assert!(g.reviews("C").is_empty(), "{:?}", g.reviews("C"));
    // Undo of it puts the update back, as for any record.
    g.history_undo("A", &m(1), f(12));
    g.settle();
    assert_eq!(g.value("A", &m(1)), f(12));
}

/// "Keep the updates" writes records that restore what this PC has, and never
/// a roll back: no row of A's History moves a mod to an older file.
#[test]
fn dropbox_keeping_the_updates_rolls_nothing_back() {
    let mut g = pack_group();
    g.edits("A", &(1..=3).map(|n| (m(n), f(n * 10 + 2))).collect::<Vec<_>>());
    g.settle();
    g.edits("B", &(1..=3).map(|n| (m(n), Value::PackDefault)).collect::<Vec<_>>());
    g.settle();
    let id = g.reviews("A")[0].id;
    g.answer("A", id, Answer::Keep, false);
    g.settle();
    for n in 1..=3 {
        assert_eq!(g.value("A", &m(n)), f(n * 10 + 2));
        let last = g.engine("A").last_change(&m(n)).cloned().unwrap();
        assert_eq!((last.was.clone(), last.now.clone()), (f(n * 10 + 2), f(n * 10 + 2)), "a kept row changes nothing");
    }
}
