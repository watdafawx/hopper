//! Two PCs whose mods already differ when sync starts (the first start after
//! the upgrade begins from each disk with an empty log and posts nothing).
//! No record explains the difference, so each PC's "Needs you" shows one
//! "Differs" card per friend, and the answer is a record that everyone
//! follows. Ordinary life (changes in flight, an offline friend, a download
//! that fails and backs off, a running game) never raises one.

use super::*;
use hopper_core::synccore::apply::Outcome;
use hopper_core::synccore::cards::cards;
use hopper_core::synccore::engine::Engine;
use hopper_core::synccore::wire::{self, Body, MemberStatus};
use hopper_core::synccore::{Answer, Arrival, Review, Risk, Snapshot, Status};

fn differs(g: &Group, who: &str) -> Vec<Review> {
    g.reviews(who).into_iter().filter(|r| r.risk == Risk::Differs).collect()
}

/// No PC of the group holds a "Differs" card.
fn quiet(g: &Group, names: &[&str]) {
    for who in names {
        assert!(differs(g, who).is_empty(), "{who} got a Differs card: {:?}", differs(g, who));
    }
}

/// The card of `who` about `friend`, as the screen gets it.
fn card_about(g: &Group, who: &str, friend: &str) -> hopper_core::synccore::cards::Card {
    cards(g.engine(who)).into_iter().find(|c| c.risk == Risk::Differs && c.who == friend).unwrap_or_else(|| panic!("{who} has no Differs card about {friend}"))
}

/// The drill: Alice (A) has mod 7 on, Bob (B) has it off, and neither start
/// made a record. Each gets one card, nothing moves until someone answers,
/// and one answer makes both agree and closes both cards.
#[test]
fn the_drill_disks_that_differ_at_the_start_raise_one_card_each() {
    let mut g = Group::with(&["A", "B"], &base());
    g.diverge("A", m(7), f(71));
    g.diverge("B", m(7), off(71));
    g.settle();
    // Both chips would have read In sync; now they ask. Nothing was changed.
    for who in ["A", "B"] {
        assert_eq!(g.status(who), Status::NeedsYou(1), "{who}");
        assert_eq!(differs(&g, who).len(), 1, "{who}");
    }
    assert_eq!(g.value("A", &m(7)), f(71));
    assert_eq!(g.value("B", &m(7)), off(71));
    assert_eq!(g.records_by("A", "A") + g.records_by("A", "B"), 0);

    // The words, and the safe answer filled: the one that keeps the mod.
    let (ca, cb) = (card_about(&g, "A", "B"), card_about(&g, "B", "A"));
    assert_eq!(ca.headline, "B has {mod} off, this PC has it on.");
    assert_eq!(cb.headline, "A has {mod} on, this PC has it off.");
    assert_eq!(ca.buttons.iter().map(|b| (b.label.as_str(), b.lead)).collect::<Vec<_>>(), vec![("Use mine", true), ("Use B's", false)]);
    assert_eq!(cb.buttons.iter().map(|b| (b.label.as_str(), b.lead)).collect::<Vec<_>>(), vec![("Use A's", true), ("Use mine", false)]);
    assert!(cb.tick.is_none() && ca.tick.is_none(), "one mod needs no tick");

    // Bob takes Alice's: it is a record of his, applied the normal way.
    g.answer("B", cb.id, Answer::Take, false);
    g.settle();
    assert_eq!(g.value("B", &m(7)), f(71));
    assert_eq!(g.value("A", &m(7)), f(71));
    for who in ["A", "B"] {
        quiet(&g, &[who]);
        assert_eq!(g.status(who), Status::InSync, "{who}");
    }
    assert_eq!(g.records_by("A", "B"), 1);
    assert_eq!(g.last_by("A", &m(7)).as_deref(), Some("B"));
    // Settling again makes nothing new.
    g.advance(3600);
    g.settle();
    assert_eq!(g.records_by("B", "B"), 1);
    assert_eq!(g.status("A"), Status::InSync);
}

/// The other answer: Alice keeps hers and everyone follows it.
#[test]
fn use_mine_makes_everyone_follow() {
    let mut g = Group::with(&["A", "B"], &base());
    g.diverge("A", m(7), f(71));
    g.diverge("B", m(7), off(71));
    g.settle();
    let ca = card_about(&g, "A", "B");
    g.answer("A", ca.id, Answer::Keep, false);
    g.settle();
    assert_eq!(g.value("B", &m(7)), f(71));
    // Bob's card closed by itself: the group has a value for the mod now.
    quiet(&g, &["A", "B"]);
    assert_eq!(g.status("B"), Status::InSync);
    assert_eq!(g.last_by("B", &m(7)).as_deref(), Some("A"));
}

/// Both press the filled button: two records that say the same thing.
#[test]
fn both_pressing_the_filled_button_still_agree() {
    let mut g = Group::with(&["A", "B"], &base());
    g.diverge("A", m(7), f(71));
    g.diverge("B", m(7), off(71));
    g.settle();
    let (ca, cb) = (card_about(&g, "A", "B"), card_about(&g, "B", "A"));
    g.answer("A", ca.id, Answer::Keep, false);
    g.answer("B", cb.id, Answer::Take, false);
    g.settle();
    assert_eq!((g.value("A", &m(7)), g.value("B", &m(7))), (f(71), f(71)));
    quiet(&g, &["A", "B"]);
    assert_eq!((g.status("A"), g.status("B")), (Status::InSync, Status::InSync));
    // Same value from both: no clash card.
    assert!(g.reviews("A").is_empty() && g.reviews("B").is_empty());
}

/// A mod one PC lacks entirely is a difference too.
#[test]
fn a_mod_only_one_pc_has_is_a_difference() {
    let mut g = Group::with(&["A", "B"], &base());
    g.diverge("A", m(8), f(81));
    g.settle();
    assert_eq!(card_about(&g, "B", "A").headline, "A has {mod} on, this PC doesn't have it.");
    assert_eq!(card_about(&g, "A", "B").headline, "B doesn't have {mod}, this PC has it on.");
    // Safe answers keep the mod: Alice keeps hers, Bob takes it.
    assert_eq!(card_about(&g, "A", "B").buttons[0].label, "Use mine");
    assert_eq!(card_about(&g, "B", "A").buttons[0].label, "Use A's");
    let id = card_about(&g, "B", "A").id;
    g.answer("B", id, Answer::Take, false);
    g.settle();
    assert_eq!(g.value("B", &m(8)), f(81));
    quiet(&g, &["A", "B"]);
}

// ---- Ordinary life never raises the card ---------------------------------------

/// Changes in flight and landing: not a difference.
#[test]
fn ordinary_edits_never_raise_the_card() {
    let mut g = three();
    g.edit("A", m(2), off(21));
    g.settle();
    quiet(&g, &["A", "B", "C"]);
    g.edits("B", &[(m(3), f(32)), (m(4), Value::Gone)]);
    g.edit("C", m(5), f(52));
    g.settle();
    quiet(&g, &["A", "B", "C"]);
    for who in ["A", "B", "C"] {
        assert_eq!(g.status(who), Status::InSync, "{who}");
    }
}

/// A friend offline for a while: their old state against ours is no one's
/// difference, before, while and after they are back.
#[test]
fn an_offline_friend_never_raises_it() {
    let mut g = three();
    g.offline("B");
    g.edit("A", m(1), f(12));
    g.edit("B", m(2), off(21));
    g.advance(3600);
    g.settle();
    quiet(&g, &["A", "B", "C"]);
    g.online("B");
    g.settle();
    quiet(&g, &["A", "B", "C"]);
    for who in ["A", "B", "C"] {
        assert_eq!(g.status(who), Status::InSync, "{who}");
        assert_eq!(g.value(who, &m(1)), f(12), "{who}");
    }
}

/// A download that fails and backs off: the PC is behind, which is not a
/// difference nobody explains, on either side, until it lands.
#[test]
fn a_failing_download_with_back_off_never_raises_it() {
    let mut g = three();
    g.fail_fetch("B", &m(3), Outcome::Failed("connection reset".into()));
    g.edit("A", m(3), f(32));
    g.settle();
    assert_eq!(g.value("B", &m(3)), f(31));
    quiet(&g, &["A", "B", "C"]);
    g.advance(30);
    g.settle();
    quiet(&g, &["A", "B", "C"]);
    g.advance(3600);
    g.settle();
    assert_eq!(g.value("B", &m(3)), f(32));
    quiet(&g, &["A", "B", "C"]);
    assert_eq!(g.status("B"), Status::InSync);
}

/// A site that rate-limits, and a download only the player can make.
#[test]
fn a_rate_limit_or_a_by_hand_download_never_raises_it() {
    let mut g = three();
    g.fail_fetch("B", &m(1), Outcome::Retry { after_secs: 300 });
    g.fail_fetch("C", &m(7), Outcome::Blocked);
    g.edits("A", &[(m(1), f(12)), (m(7), f(71))]);
    g.settle();
    quiet(&g, &["A", "B", "C"]);
    g.advance(301);
    g.settle();
    quiet(&g, &["A", "B", "C"]);
    assert_eq!(g.by_hand("C"), vec![m(7)]);
}

/// A running game holds jar changes back: waiting, not different.
#[test]
fn a_running_game_never_raises_it() {
    let mut g = three();
    g.game("B", true);
    g.edit("A", m(1), f(12));
    g.settle();
    assert_eq!(g.value("B", &m(1)), f(11));
    quiet(&g, &["A", "B", "C"]);
    g.game("B", false);
    g.settle();
    assert_eq!(g.value("B", &m(1)), f(12));
    quiet(&g, &["A", "B", "C"]);
}

/// A friend on an older wire version is refused and named; their disk is
/// not compared.
#[test]
fn a_friend_on_an_older_wire_is_not_compared() {
    let mut g = Group::with(&["A", "B"], &base());
    g.diverge("B", m(7), off(71));
    g.diverge("A", m(7), f(71));
    g.old_hopper("B");
    g.settle();
    quiet(&g, &["A", "B"]);
}

/// A friend's Hopper that sends no digest (a build from before it) is not
/// compared either, and the same records and statuses without it change
/// nothing.
#[test]
fn a_friend_without_a_digest_is_not_compared() {
    let mut e = Engine::new("pack", "me", "Me");
    e.applied.insert(m(1), f(11));
    let old = MemberStatus { member: "mia".into(), name: "Mia".into(), settled: true, ..MemberStatus::default() };
    e.ingest("mia", Vec::new(), old, Arrival { seq: 1, at: 10 });
    assert!(e.reviews().is_empty());
    assert!(!e.my_status().needs_state);
    // A saved status from before the digest reads, with the new fields empty.
    let s: MemberStatus = serde_json::from_str(r#"{"member":"mia","name":"Mia","pack":"cf:1/2"}"#).unwrap();
    assert!(s.state_hash.is_empty() && s.state.is_empty() && !s.settled);
}

/// A friend who sets a mod to "Only on this PC" keeps it out of the
/// comparison on both sides.
#[test]
fn a_mod_kept_only_here_is_never_compared() {
    let mut g = Group::with(&["A", "B"], &base());
    g.only_here("B", &m(2));
    g.diverge("B", m(2), off(21));
    g.settle();
    quiet(&g, &["A", "B"]);
}

// ---- Many mods, one card ------------------------------------------------------------

fn forty() -> Vec<(hopper_core::synccore::Key, Value)> {
    (1..=40).map(|n| (m(n), f(n * 10 + 1))).collect()
}

/// 30 differing mods are one card with a list, per friend. Taking the
/// friend's turns 30 mods on (safe); keeping these would turn 30 off for
/// everyone, so it needs the tick.
#[test]
fn thirty_differing_mods_are_one_card() {
    let mut g = Group::with(&["A", "B"], &forty());
    for n in 1..=30 {
        g.diverge("B", m(n), off(n * 10 + 1));
    }
    g.settle();
    assert_eq!((differs(&g, "A").len(), differs(&g, "B").len()), (1, 1));
    assert_eq!(g.status("B"), Status::NeedsYou(1));
    let cb = card_about(&g, "B", "A");
    assert_eq!(cb.mods.len(), 30);
    assert_eq!(cb.headline, "A and this PC differ on 30 mods.");
    assert_eq!(cb.mods[0].note, "off here, on for A");
    assert_eq!(cb.buttons[0].label, "Use A's");
    assert!(cb.buttons[0].lead && !cb.buttons[0].needs_tick);
    assert_eq!(cb.buttons[1].label, "Use mine");
    assert!(cb.buttons[1].needs_tick && !cb.buttons[1].lead);
    assert_eq!(cb.buttons[1].ticked.as_deref(), Some("Use mine, remove 30 mods"));
    assert_eq!(cb.tick.as_deref(), Some("I looked at the list. Remove all 30 for everyone."));
    // The other way round: Alice keeps hers (30 mods on), nothing removed.
    let ca = card_about(&g, "A", "B");
    assert_eq!(ca.buttons[0].label, "Use mine");
    assert!(ca.buttons[1].needs_tick);
    assert_eq!(ca.tick.as_deref(), Some("I looked at the list. Remove all 30 from this PC too."));
    // One answer, one batch of 30 records, and the cards close everywhere.
    g.answer("B", cb.id, Answer::Take, false);
    g.settle();
    for n in 1..=30 {
        assert_eq!(g.value("B", &m(n)), f(n * 10 + 1));
    }
    quiet(&g, &["A", "B"]);
    assert_eq!((g.status("A"), g.status("B")), (Status::InSync, Status::InSync));
}

/// Only some of 40 differ: the card lists those, and a long list scrolls.
#[test]
fn a_few_differing_mods_among_many_are_listed() {
    let mut g = Group::with(&["A", "B"], &forty());
    g.diverge("B", m(7), off(71));
    g.diverge("B", m(33), Value::Gone);
    g.settle();
    let c = card_about(&g, "A", "B");
    assert_eq!(c.mods.iter().map(|x| x.key.clone()).collect::<Vec<_>>(), vec![m(33), m(7)]);
}

// ---- A late joiner -----------------------------------------------------------------

/// C joins late with mod 5 turned off on its disk while A and B (who made
/// changes before) have it on: C gets a card about each friend, A and B one
/// about C, and A and B none about each other. One answer from C closes all.
#[test]
fn a_late_joiner_with_a_differing_disk_gets_cards_and_converges() {
    let mut g = Group::with(&["A", "B"], &base());
    g.edit("A", m(2), off(21));
    g.settle();
    g.join("C");
    g.diverge("C", m(5), off(51));
    g.settle();
    // C took in A's earlier change like any joiner.
    assert_eq!(g.value("C", &m(2)), off(21));
    assert_eq!(differs(&g, "C").len(), 2);
    assert_eq!((differs(&g, "A").len(), differs(&g, "B").len()), (1, 1));
    assert_eq!(card_about(&g, "A", "C").headline, "C has {mod} off, this PC has it on.");
    assert_eq!(card_about(&g, "C", "A").others, 1);
    // Nothing moved on C's disk while it waits.
    assert_eq!(g.value("C", &m(5)), off(51));
    let id = card_about(&g, "C", "A").id;
    g.answer("C", id, Answer::Take, true);
    g.settle();
    assert_eq!(g.value("C", &m(5)), f(51));
    quiet(&g, &["A", "B", "C"]);
    for who in ["A", "B", "C"] {
        assert_eq!(g.status(who), Status::InSync, "{who}");
    }
}

// ---- Size on the wire ------------------------------------------------------------------

/// What the digest costs for a 1,172-mod pack: 16 bytes of hash in every
/// packet, and the list only to a friend who asked.
#[test]
fn the_digest_is_small_until_a_friend_asks_for_the_list() {
    let mut e = Engine::new("pack", "me", "Me");
    let mut x: u64 = 88172645463325252;
    while e.applied.len() < 1172 {
        // Project and file ids of the size CurseForge hands out.
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let (project, file) = (100_000 + x % 900_000, 3_000_000 + (x >> 20) % 4_000_000);
        e.applied.insert(hopper_core::synccore::Key::Mod(format!("cf:{project}")), Value::File { file: format!("cf:{file}"), on: x % 11 != 0 });
    }
    let size = |status: MemberStatus| -> usize {
        let mut copies = std::collections::BTreeMap::new();
        copies.insert("pack".to_string(), (Body::Tail(Vec::new()), status, "0123456789abcdef".to_string()));
        let p = wire::Packet { version: wire::WIRE_VERSION, from: "me".into(), group: "g".into(), copies };
        wire::encode(&p).iter().map(String::len).sum()
    };
    let none = e.status_for("friend", &Snapshot::default());
    assert!(none.state.is_empty() && !none.state_hash.is_empty());
    let mut bare = none.clone();
    // A friend asks.
    e.members.insert("friend".into(), MemberStatus { member: "friend".into(), needs_state: true, ..MemberStatus::default() });
    let with = e.status_for("friend", &Snapshot::default());
    assert_eq!(with.state.len(), 1172);
    let (hash_only, listed) = (size(none), size(with));
    bare.state_hash.clear();
    bare.reg_hash.clear();
    bare.settled = false;
    let before = size(bare);
    eprintln!("1,172 mods: packet {before} bytes before the digest, {hash_only} with its hashes, {listed} with the list");
    assert!(hash_only < before + 100, "the hashes add {} bytes", hash_only - before);
    assert!(listed < 40 * 1024, "the list is {listed} bytes");
}
