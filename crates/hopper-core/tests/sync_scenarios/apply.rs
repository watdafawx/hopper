//! Applying friends' changes on disk: per-step outcomes, authors kept,
//! crashes and the game (design §7).

use super::*;
use hopper_core::synccore::apply::Outcome;
use hopper_core::synccore::{Status, Value};

/// Covers W25, R3. A makes three changes; B's download for the second one
/// fails. The first and third land on B named A, the second retries, and
/// nothing goes out as B's change.
#[test]
#[ignore = "sync-rewrite"]
fn w25_one_failed_file_keeps_everyones_names() {
    let mut g = three();
    g.fail_fetch("B", &m(2), Outcome::Failed("connection reset".into()));
    g.edits("A", &[(m(1), f(12)), (m(2), f(22)), (m(3), f(32))]);
    g.settle();
    assert_eq!(g.value("B", &m(1)), f(12));
    assert_eq!(g.value("B", &m(2)), f(21));
    assert_eq!(g.value("B", &m(3)), f(32));
    assert_eq!(g.last_by("C", &m(1)).as_deref(), Some("A"));
    assert_eq!(g.last_by("C", &m(3)).as_deref(), Some("A"));
    assert!(g.history("A").iter().all(|h| h.by != "B"));
}

/// Covers W26, C5. A mod site is down for B: the step retries after its
/// back-off and lands, and A's other changes weren't held up meanwhile.
#[test]
#[ignore = "sync-rewrite"]
fn w26_a_site_down_retries_without_blocking_others() {
    let mut g = three();
    g.fail_fetch("B", &m(1), Outcome::Retry { after_secs: 300 });
    g.edits("A", &[(m(1), f(12)), (m(2), off(21))]);
    g.settle();
    assert_eq!(g.value("B", &m(2)), off(21));
    assert_eq!(g.value("B", &m(1)), f(11));
    g.advance(301);
    g.settle();
    assert_eq!(g.value("B", &m(1)), f(12));
}

/// Covers W27, C5. The author blocks downloads (CDN 403): the step goes to
/// B's by-hand queue once, is not retried forever, and B's other changes
/// land.
#[test]
#[ignore = "sync-rewrite"]
fn w27_blocked_download_goes_to_by_hand_once() {
    let mut g = three();
    g.fail_fetch("B", &m(7), Outcome::Blocked);
    g.edits("A", &[(m(7), f(71)), (m(1), f(12))]);
    g.settle();
    g.advance(3600);
    g.settle();
    assert_eq!(g.value("B", &m(1)), f(12));
    assert_eq!(g.by_hand("B"), vec![m(7)]);
    assert_eq!(g.fetch_tries("B", &m(7)), 1);
}

/// Covers W32, C4. B's game is running: jar changes wait, then land when
/// it closes, named A. No pack update runs under a running game.
#[test]
#[ignore = "sync-rewrite"]
fn w32_game_running_waits_then_lands() {
    let mut g = three();
    g.game("B", true);
    g.edit("A", m(1), f(12));
    g.settle();
    assert_eq!(g.value("B", &m(1)), f(11));
    assert!(matches!(g.status("B"), Status::Behind(1)));
    g.game("B", false);
    g.settle();
    assert_eq!(g.value("B", &m(1)), f(12));
}

/// Covers C4. B's PC loses power halfway through swapping three files. On
/// restart the journal is finished: each mod has exactly one file, none is
/// doubled, and the authors are A's.
#[test]
#[ignore = "sync-rewrite"]
fn c4_a_crash_mid_swap_leaves_no_duplicate_copies() {
    let mut g = three();
    g.crash_mid_apply("B", 1);
    g.edits("A", &[(m(1), f(12)), (m(2), f(22)), (m(3), f(32))]);
    g.settle();
    g.restart("B");
    g.settle();
    for n in 1..=3 {
        assert_eq!(g.value("B", &m(n)), f(n * 10 + 2));
        assert_eq!(g.files_of("B", &m(n)), 1);
    }
}

/// Covers C4 (relay scenario s12). B restarts after receiving A's packet
/// but before applying it: it is applied once after the restart, not twice
/// and not lost.
#[test]
#[ignore = "sync-rewrite"]
fn c4_a_restart_before_applying_applies_once() {
    let mut g = three();
    g.game("B", true);
    g.edit("A", m(1), f(12));
    g.settle();
    g.restart("B");
    g.game("B", false);
    g.settle();
    assert_eq!(g.value("B", &m(1)), f(12));
    assert_eq!(g.history("B").iter().filter(|h| h.key == m(1)).count(), 1);
}

/// Covers C5. GitHub's hourly limit: B's link-sourced mod waits with the
/// limit's back-off, and the group's CurseForge changes keep landing.
#[test]
#[ignore = "sync-rewrite"]
fn c5_a_rate_limit_backs_off_per_key() {
    use hopper_core::synccore::Key;
    let mut g = three();
    let k = Key::Mod("src:coolmod-1.0.jar".into());
    g.allow_source("B", &k);
    g.fail_fetch("B", &k, Outcome::Retry { after_secs: 3600 });
    g.edit("A", k.clone(), Value::File { file: "src:coolmod-1.0.jar".into(), on: true });
    g.edit("A", m(1), f(12));
    g.settle();
    assert_eq!(g.value("B", &m(1)), f(12));
    assert_eq!(g.value("B", &k), Value::Gone);
    assert_eq!(g.fetch_tries("B", &k), 1);
}
