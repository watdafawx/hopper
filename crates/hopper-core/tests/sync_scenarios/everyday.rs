//! Everyday changes with everyone online: the workflows that work today
//! must keep working on the new core.

use super::*;
use hopper_core::synccore::{Key, Value};

/// Covers W1. A adds mod 7. B and C have it after one settle, named "A".
#[test]
#[ignore = "sync-rewrite"]
fn w01_an_added_mod_reaches_everyone() {
    let mut g = three();
    g.edit("A", m(7), f(71));
    g.settle();
    for p in ["B", "C"] {
        assert_eq!(g.value(p, &m(7)), f(71));
        assert_eq!(g.last_by(p, &m(7)).as_deref(), Some("A"));
    }
}

/// Covers W2. A turns mod 2 off, then on again. Both reach B and C; History
/// on B shows "Turned off by A" then "Turned on by A".
#[test]
#[ignore = "sync-rewrite"]
fn w02_off_then_on_reaches_everyone() {
    let mut g = three();
    g.edit("A", m(2), off(21));
    g.settle();
    assert_eq!(g.value("B", &m(2)), off(21));
    g.edit("A", m(2), f(21));
    g.settle();
    assert_eq!(g.value("C", &m(2)), f(21));
    let says: Vec<String> = g.history("B").into_iter().map(|h| h.says).collect();
    assert!(says.iter().any(|s| s == "Turned off by A"));
    assert!(says.iter().any(|s| s == "Turned on by A"));
}

/// Covers W3. A updates mod 3 from 31 to 32. B and C swap the file.
#[test]
#[ignore = "sync-rewrite"]
fn w03_an_update_reaches_everyone() {
    let mut g = three();
    g.edit("A", m(3), f(32));
    g.settle();
    assert_eq!(g.value("B", &m(3)), f(32));
    assert_eq!(g.value("C", &m(3)), f(32));
}

/// Covers W4, D1. A removes mod 4 with nobody else touching it. A plain
/// follow-up (no clash, not away, one removal): it lands everywhere with no
/// card, and B can put it back from History.
#[test]
#[ignore = "sync-rewrite"]
fn w04_a_plain_removal_lands_without_a_question() {
    let mut g = three();
    g.edit("A", m(4), Value::Gone);
    g.settle();
    assert_eq!(g.value("B", &m(4)), Value::Gone);
    assert!(g.reviews("B").is_empty());
    assert!(g.history("B").iter().any(|h| h.key == m(4) && h.says == "Removed by A"));
}

/// Covers W5, C7. A locks mod 5 and changes nothing else. The lock alone
/// travels (a publish that only changed a lock used to send nothing).
#[test]
#[ignore = "sync-rewrite"]
fn w05_a_lock_alone_reaches_everyone() {
    let mut g = three();
    g.edit("A", Key::Lock("cf:5".into()), Value::Text("cf:51".into()));
    g.settle();
    assert_eq!(g.value("B", &Key::Lock("cf:5".into())), Value::Text("cf:51".into()));
}

/// Covers W9. A changes a shared JEI setting. B gets it, named "A"
/// (configsync rides along with the new core unchanged).
#[test]
#[ignore = "sync-rewrite"]
fn w09_a_shared_setting_still_travels() {
    let mut g = three();
    g.share_settings("A", "jei");
    g.settle();
    g.set_setting("A", "jei", "cheatMode", "true");
    g.settle();
    assert_eq!(g.setting("B", "jei", "cheatMode").as_deref(), Some("true"));
}

/// Covers W10. A edits a config file of a mod whose settings aren't
/// shared. Nothing reaches B, and A sees it marked "Stays on this PC".
#[test]
#[ignore = "sync-rewrite"]
fn w10_other_config_files_stay_on_this_pc() {
    let mut g = three();
    g.edit_config("A", "config/create-common.toml");
    g.settle();
    assert!(!g.edited_config("B", "config/create-common.toml"));
}

/// Covers W12. B goes offline, A makes six changes, B comes back with no
/// changes of its own: B ends on the group's state with no card.
#[test]
#[ignore = "sync-rewrite"]
fn w12_back_from_away_with_no_changes_catches_up() {
    let mut g = three();
    g.offline("B");
    g.edits("A", &[(m(1), f(12)), (m(2), off(21)), (m(7), f(71))]);
    g.settle();
    g.advance(3 * DAY);
    g.online("B");
    g.settle();
    assert_eq!(g.value("B", &m(1)), f(12));
    assert_eq!(g.value("B", &m(2)), off(21));
    assert_eq!(g.value("B", &m(7)), f(71));
    assert!(g.reviews("B").is_empty());
}

/// Covers W20. A bumps the loader within the same kind: B moves too. A
/// switch to another kind (Forge to NeoForge) isn't followed; B is told.
#[test]
#[ignore = "sync-rewrite"]
fn w20_loader_bump_same_kind_follows_other_kind_tells() {
    let mut g = Group::with(&["A", "B"], &[(Key::Loader, Value::Text("neoforge 21.1.77".into()))]);
    g.edit("A", Key::Loader, Value::Text("neoforge 21.1.80".into()));
    g.settle();
    assert_eq!(g.value("B", &Key::Loader), Value::Text("neoforge 21.1.80".into()));
    g.edit("A", Key::Loader, Value::Text("forge 52.0.1".into()));
    g.settle();
    assert_eq!(g.value("B", &Key::Loader), Value::Text("neoforge 21.1.80".into()));
    assert!(g.notes("B").iter().any(|n| n.contains("Forge")));
}

/// Covers W21, R6. A plain instance (no modpack): A moves Minecraft from
/// 1.21.1 to 1.21.4. B moves too (the old core never sent the version, so
/// B got 1.21.4 mods on 1.21.1).
#[test]
#[ignore = "sync-rewrite"]
fn w21_minecraft_version_of_a_plain_instance_travels() {
    let mut g = Group::with(&["A", "B"], &[(Key::Game, Value::Text("1.21.1".into()))]);
    g.edit("A", Key::Game, Value::Text("1.21.4".into()));
    g.settle();
    assert_eq!(g.value("B", &Key::Game), Value::Text("1.21.4".into()));
}

/// Covers W22. A adds a shader pack. It lands in B's `shaderpacks` folder.
#[test]
#[ignore = "sync-rewrite"]
fn w22_shaders_and_resource_packs_land_in_their_folders() {
    let mut g = three();
    let shader = Key::Mod("shader:cf:9".into());
    let rp = Key::Mod("rp:cf:8".into());
    g.edit("A", shader.clone(), f(91));
    g.edit("A", rp.clone(), f(81));
    g.settle();
    assert_eq!(g.folder("B", &shader).as_deref(), Some("shaderpacks"));
    assert_eq!(g.folder("B", &rp).as_deref(), Some("resourcepacks"));
}

/// Covers W23. A makes a world. Worlds never travel.
#[test]
#[ignore = "sync-rewrite"]
fn w23_worlds_never_travel() {
    let mut g = three();
    g.make_world("A", "New World");
    g.settle();
    assert!(!g.has_world("B", "New World"));
}

/// Covers W24. B renames the instance. Sync carries on (the copy is found
/// by its id, not its name).
#[test]
#[ignore = "sync-rewrite"]
fn w24_renaming_the_instance_keeps_syncing() {
    let mut g = three();
    g.rename("B", "Our pack (B)");
    g.edit("A", m(7), f(71));
    g.settle();
    assert_eq!(g.value("B", &m(7)), f(71));
}

/// Covers W30. B's PC clock is two days ahead, C's an hour behind. Order
/// and outcomes are the same as with right clocks: no PC clock decides.
#[test]
#[ignore = "sync-rewrite"]
fn w30_pc_clocks_never_decide() {
    let mut g = three();
    g.clock_skew("B", 2 * DAY as i64);
    g.clock_skew("C", -3600);
    g.edit("B", m(1), f(12));
    g.settle();
    g.edit("C", m(1), f(13));
    g.settle();
    for p in ["A", "B", "C"] {
        assert_eq!(g.value(p, &m(1)), f(13));
    }
}

/// Covers W33. A adds a mod from GitHub. B gets a "Allow and install" card
/// and nothing lands until B allows the source.
#[test]
#[ignore = "sync-rewrite"]
fn w33_a_mod_from_a_link_waits_for_allow() {
    let mut g = three();
    let k = Key::Mod("src:coolmod-1.0.jar".into());
    let v = Value::File { file: "src:coolmod-1.0.jar".into(), on: true };
    g.edit("A", k.clone(), v.clone());
    g.settle();
    assert_eq!(g.value("B", &k), Value::Gone);
    g.allow_source("B", &k);
    g.settle();
    assert_eq!(g.value("B", &k), v);
}

/// Covers W34. B marks mod 6 "Only on this PC" and turns it off. A keeps it
/// on; A's later update of mod 6 still doesn't change B's.
#[test]
#[ignore = "sync-rewrite"]
fn w34_only_on_this_pc_stays_here() {
    let mut g = three();
    g.only_here("B", &m(6));
    g.edit("B", m(6), off(61));
    g.settle();
    assert_eq!(g.value("A", &m(6)), f(61));
    g.edit("A", m(6), f(62));
    g.settle();
    assert_eq!(g.value("B", &m(6)), off(61));
}

/// Covers W35. C takes changes from A only ("Ignore" B) and B shares only
/// on Push. B's change waits on B's PC until Push, then reaches A but not C.
#[test]
#[ignore = "sync-rewrite"]
fn w35_trust_and_push_by_hand() {
    use hopper_core::synccore::Trust;
    let mut g = three();
    g.set_trust("C", "B", Trust::Ignore);
    g.push_by_hand("B", true);
    g.edit("B", m(1), f(12));
    g.settle();
    assert_eq!(g.value("A", &m(1)), f(11));
    g.push("B");
    g.settle();
    assert_eq!(g.value("A", &m(1)), f(12));
    assert_eq!(g.value("C", &m(1)), f(11));
}

/// Covers C1. B can't look up mod 3's project (CurseForge down) when A's
/// update of it arrives. The record carries its key, so B still knows
/// which mod it is: nothing on B reads as removed, and mod 3 updates once
/// the download works.
#[test]
#[ignore = "sync-rewrite"]
fn c1_a_lookup_failure_is_never_a_removal() {
    use hopper_core::synccore::apply::Outcome;
    let mut g = three();
    g.fail_fetch("B", &m(3), Outcome::Retry { after_secs: 300 });
    g.edit("A", m(3), f(32));
    g.settle();
    assert_eq!(g.value("B", &m(3)), f(31));
    assert_eq!(g.group_value("B", &m(3)), f(32));
    g.advance(300);
    g.settle();
    assert_eq!(g.value("B", &m(3)), f(32));
    assert!(g.history("A").iter().all(|h| h.says != "Removed by B"));
}

/// Covers C1. A's very first packet in a new group reaches everyone with
/// its changes (a first code used to have no stamps, so they reached no one).
#[test]
#[ignore = "sync-rewrite"]
fn c1_a_first_packet_carries_its_changes() {
    let mut g = Group::with(&["A"], &base());
    g.edit("A", m(1), f(12));
    g.join("B");
    g.settle();
    assert_eq!(g.value("B", &m(1)), f(12));
    assert_eq!(g.last_by("B", &m(1)).as_deref(), Some("A"));
}
