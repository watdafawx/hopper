//! Group modpack updates: one stated plan, shown before Update (design §6).

use super::*;
use hopper_core::synccore::{ConfigMode, Key, PackMode, Risk, Value};

fn packed() -> Group {
    let mut mods = base();
    mods.push((Key::Pack, Value::Text("cf:100/8600".into())));
    mods.push((m(4), Value::PackDefault));
    Group::with(&["A", "B", "C"], &mods)
}

const KEEP: PackMode = PackMode { here: ConfigMode::Keep, friends: ConfigMode::Keep, keep_off: true };
const CLEAN_HERE: PackMode = PackMode { here: ConfigMode::Clean, friends: ConfigMode::Keep, keep_off: true };

/// Covers W6, R4. B changed 3 config files. Before A presses Update, A's
/// plan says B keeps those 3 files; after it, B is on the new version with
/// the 3 files still B's, and never stuck at "Behind: needs you".
#[test]
fn w06_keep_my_changes_is_stated_and_done_per_friend() {
    let mut g = packed();
    for f in ["config/a.toml", "config/b.toml", "config/c.toml"] {
        g.edit_config("B", f);
    }
    g.settle();
    let plan = g.pack_plan("A", "cf:100/8700", KEEP);
    let b = plan.friends.iter().find(|p| p.member == "B").unwrap();
    assert_eq!(b.kept_files, 3);
    assert_eq!(b.configs, ConfigMode::Keep);
    g.update_pack("A", "cf:100/8700", KEEP);
    g.settle();
    assert_eq!(g.value("B", &Key::Pack), Value::Text("cf:100/8700".into()));
    assert!(g.edited_config("B", "config/a.toml"));
    assert!(g.reviews("B").is_empty());
}

/// Covers W7, R4, D3. B turned pack mod 4 off. A runs a clean update: A's
/// configs are cleaned, B's are not, and mod 4 stays off for B. The plan
/// said both before Update.
#[test]
fn w07_clean_update_cleans_only_this_pc_and_keeps_friends_off_mods() {
    let mut g = packed();
    g.edit("B", m(4), Value::PackOff);
    g.edit_config("B", "config/b.toml");
    g.settle();
    let plan = g.pack_plan("A", "cf:100/8700", CLEAN_HERE);
    let b = plan.friends.iter().find(|p| p.member == "B").unwrap();
    assert_eq!(b.stays_off, vec![m(4)]);
    g.update_pack("A", "cf:100/8700", CLEAN_HERE);
    g.settle();
    assert_eq!(g.value("B", &m(4)), Value::PackOff);
    assert!(g.edited_config("B", "config/b.toml"));
}

/// Covers W8. A moves the pack back to an older version. B and C get a
/// "Move back too / Stay" card (a big move asks), and stay until they answer.
#[test]
fn w08_moving_the_pack_back_asks() {
    let mut g = packed();
    g.update_pack("A", "cf:100/8400", KEEP);
    g.settle();
    assert_eq!(g.value("B", &Key::Pack), Value::Text("cf:100/8600".into()));
    assert_eq!(g.reviews("B")[0].risk, Risk::PackBack);
}

/// Covers W19. A switches the instance to another modpack. B is offered
/// "Get it as a new instance" and B's copy is left as it is.
#[test]
fn w19_switching_to_another_pack_offers_a_new_instance() {
    let mut g = packed();
    g.update_pack("A", "cf:200/100", KEEP);
    g.settle();
    assert_eq!(g.value("B", &Key::Pack), Value::Text("cf:100/8600".into()));
    assert_eq!(g.reviews("B")[0].risk, Risk::OtherPack);
}

/// Covers R4. C's game is running: A's plan says "C: waits until the game
/// closes" before A presses Update.
#[test]
fn r4_the_plan_says_who_waits_for_their_game() {
    let mut g = packed();
    g.game("C", true);
    g.settle();
    let plan = g.pack_plan("A", "cf:100/8700", KEEP);
    assert!(plan.friends.iter().find(|p| p.member == "C").unwrap().waits_for_game);
}

/// Covers C1 (CC: Tweaked was deleted on followers). The new pack version
/// ships mod 4 as a pack jar that A had added by hand. Moving to it is no
/// removal of mod 4 anywhere.
#[test]
fn c1_a_jar_the_pack_now_ships_is_not_removed() {
    let mut g = packed();
    g.edit("A", m(9), f(91));
    g.settle();
    g.pack_ships("cf:100/8700", &[m(9)]);
    g.update_pack("A", "cf:100/8700", KEEP);
    g.settle();
    for p in ["A", "B", "C"] {
        assert_ne!(g.value(p, &m(9)), Value::Gone, "{p} still has mod 9");
    }
}

/// A member's hand-added jar (put in the mods folder in Explorer) stays on
/// that member's copy through the group's pack update, is shown to them as
/// "This PC differs", and reaches nobody.
#[test]
fn a_members_hand_added_jar_survives_a_pack_update_and_stays_theirs() {
    let mut g = packed();
    g.outside("B", m(9), f(91));
    g.settle();
    assert_eq!(g.drift("B").len(), 1, "B sees the jar as a difference");
    g.update_pack("A", "cf:100/8700", KEEP);
    g.settle();
    assert_eq!(g.value("B", &Key::Pack), Value::Text("cf:100/8700".into()));
    assert_eq!(g.value("B", &m(9)), f(91), "B's own jar is still on B's disk");
    for p in ["A", "C"] {
        assert_eq!(g.value(p, &m(9)), Value::Gone, "{p} never got it");
        assert_eq!(g.group_value(p, &m(9)), Value::Gone);
    }
    assert_eq!(g.drift("B").len(), 1, "it is still B's to put back or share");
}
