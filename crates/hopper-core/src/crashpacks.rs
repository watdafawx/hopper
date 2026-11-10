//! A resource pack turned on just before the game crashed while drawing
//! blocks or models.
//!
//! Case: All the Mods 10 crashed three times building chunk meshes with
//! `"modelData" is null` in FramedBlocks, reached through Fusion's model
//! wrapper. The report named FramedBlocks (it threw) and Fusion and Sodium
//! (they called it). The real change was the player's: each crash came
//! about 30 seconds after turning on an overlay resource pack in game (a
//! pack whose connected-texture models wrap other mods' blocks), and the
//! start with the pack removed ran fine. The mods only drew what the pack
//! changed; the pack is the thing to turn off.
//!
//! The game logs every resource reload with the packs it loads
//! (`Reloading ResourceManager: vanilla, …, file/Some Pack.zip`), so the log
//! says which of the player's own packs (`file/…`) came on, and when. Only a
//! crash while drawing counts (the report's description or a model frame in
//! its trace): a pack changes models and textures, not game logic.
//!
//! Certainty, from the timing alone (the report never names a pack):
//! - Likely: exactly one pack was turned on in game up to two minutes before
//!   the crash, or the last start crashed the same way right after it was
//!   turned on and this start has it on again;
//! - Possible: otherwise (more packs, more time, or only "on since the last
//!   start").

use crate::crashdetect::{Certainty, Fact, Suspect};

/// The rule's name on the facts it leaves.
pub const RULE: &str = "resource_pack";

/// A reload this long before the crash still counts (seconds).
const RECENT: i64 = 600;
/// A reload this close to the crash, with one pack, points at it (seconds).
const JUST_BEFORE: i64 = 120;
/// More packs than this changed at once: too many to point at.
const MAX_PACKS: usize = 3;

/// What the report's description says when the game was drawing.
const DRAWING: [&str; 7] = [
    "building chunk meshes",
    "Tesselating block",
    "Rendering Block Entity",
    "Rendering block entity",
    "Rendering item",
    "Batching chunks",
    "Baking model",
];
/// Frames that mean a model was being drawn or baked.
const MODEL_FRAMES: [&str; 3] = ["BakedModel", "ModelBakery", "ModelManager"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    /// Turned on in game, `secs` before the crash.
    InGame,
    /// On since the last start (it wasn't when that one began).
    SinceLastStart,
    /// The last start crashed the same way right after it was turned on.
    Again,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Trigger {
    /// The packs' names as the player sees them ("Some Pack", no `.zip`).
    pub packs: Vec<String>,
    pub certainty: Certainty,
    pub how: How,
    /// Seconds from turning it on to the crash (in game, when the log has times).
    pub secs: Option<i64>,
}

/// One `Reloading ResourceManager:` line: when, and the player's own packs.
struct Reload {
    at: Option<i64>,
    packs: Vec<String>,
}

fn reloads(log: &str) -> Vec<Reload> {
    log.lines()
        .filter_map(|l| {
            let list = &l[l.find("Reloading ResourceManager: ")? + "Reloading ResourceManager: ".len()..];
            let packs = list.split(", ").map(str::trim).filter(|p| p.starts_with("file/")).map(str::to_string).collect();
            Some(Reload { at: crate::runlog::line_wall(l), packs })
        })
        .collect()
}

fn added(now: &[String], before: &[String]) -> Vec<String> {
    now.iter().filter(|p| !before.contains(p)).cloned().collect()
}

/// "file/Some Pack.zip" → "Some Pack".
fn shown(entry: &str) -> String {
    let n = entry.strip_prefix("file/").unwrap_or(entry);
    n.strip_suffix(".zip").unwrap_or(n).to_string()
}

fn description(report: &str) -> &str {
    report.lines().find_map(|l| l.strip_prefix("Description: ")).unwrap_or("").trim()
}

/// Whether the game crashed while drawing blocks or models.
pub fn drawing(report: &str) -> bool {
    let desc = description(report);
    if DRAWING.iter().any(|m| desc.contains(m)) {
        return true;
    }
    // The first trace only: the report's later sections list the whole game.
    let trace = report.lines().skip_while(|l| !l.starts_with("Description: ")).skip(1).skip_while(|l| l.trim().is_empty());
    trace.take_while(|l| !l.trim().is_empty()).any(|l| l.trim_start().starts_with("at ") && MODEL_FRAMES.iter().any(|m| l.contains(m)))
}

/// The report's own time as written (`Time: 2026-11-10 21:56:48`), no time zone.
fn report_time(report: &str) -> Option<i64> {
    let t = report.lines().find_map(|l| l.strip_prefix("Time: "))?.trim();
    let n = |a: usize, b: usize| t.get(a..b)?.parse::<i64>().ok();
    Some(crate::time::wall_secs(n(0, 4)?, n(5, 7)?, n(8, 10)?, n(11, 13)?, n(14, 16)?, n(17, 19)?))
}

/// The last reload of `rs` that turned packs on, and those packs.
fn last_turned_on(rs: &[Reload]) -> Option<(&Reload, Vec<String>)> {
    (1..rs.len()).rev().find_map(|i| {
        let a = added(&rs[i].packs, &rs[i - 1].packs);
        (!a.is_empty()).then(|| (&rs[i], a))
    })
}

/// The pack a drawing crash points at, from the log of its start (`log`,
/// up to the crash) and of the start before (`prev`).
pub fn find(report: &str, log: &str, prev: Option<&str>) -> Option<Trigger> {
    if !drawing(report) {
        return None;
    }
    let rs = reloads(log);
    let crash_at = report_time(report);
    // Turned on in game, not long before.
    if let Some((r, packs)) = last_turned_on(&rs) {
        let secs = match (r.at, crash_at) {
            (Some(a), Some(c)) => Some((c - a).max(0)),
            _ => None,
        };
        if secs.is_some_and(|s| s <= RECENT) && packs.len() <= MAX_PACKS {
            let certainty =
                if packs.len() == 1 && secs.is_some_and(|s| s <= JUST_BEFORE) { Certainty::Likely } else { Certainty::Possible };
            return Some(Trigger { packs: packs.iter().map(|p| shown(p)).collect(), certainty, how: How::InGame, secs });
        }
        // Turned on long before, and the game drew fine since.
        if secs.is_some() {
            return None;
        }
    }
    // On at this start, but not at the last one.
    let prev = prev?;
    let before = reloads(prev);
    let packs = added(&rs.first()?.packs, &before.first()?.packs);
    if packs.is_empty() || packs.len() > MAX_PACKS {
        return None;
    }
    // The last start crashed the same way just after this pack came on.
    let desc = description(report);
    let same_crash = !desc.is_empty() && prev.contains(&format!("Description: {desc}"));
    let turned_on_then = last_turned_on(&before).is_some_and(|(_, a)| a == packs);
    let (certainty, how) =
        if packs.len() == 1 && same_crash && turned_on_then { (Certainty::Likely, How::Again) } else { (Certainty::Possible, How::SinceLastStart) };
    Some(Trigger { packs: packs.iter().map(|p| shown(p)).collect(), certainty, how, secs: None })
}

fn ago(secs: i64) -> String {
    match secs {
        0..=1 => "a moment".into(),
        2..=89 => format!("{secs} seconds"),
        _ => match (secs + 30) / 60 {
            1 => "a minute".into(),
            m => format!("{m} minutes"),
        },
    }
}

fn names(packs: &[String]) -> String {
    let q: Vec<String> = packs.iter().map(|p| format!("\"{p}\"")).collect();
    match q.len() {
        0 => String::new(),
        1 => q[0].clone(),
        n => format!("{} and {}", q[..n - 1].join(", "), q[n - 1]),
    }
}

/// The card's advice line for it.
pub fn advice(t: &Trigger) -> String {
    let one = t.packs.len() == 1;
    let pack = if one { format!("the resource pack {}", names(&t.packs)) } else { format!("the resource packs {}", names(&t.packs)) };
    let it = if one { "that pack" } else { "those packs" };
    let what = match (t.how, t.secs) {
        (How::InGame, Some(s)) => format!("You turned on {pack} {} before this crash, and the game crashed while drawing blocks.", ago(s)),
        (How::InGame, None) => format!("You turned on {pack} shortly before this crash, and the game crashed while drawing blocks."),
        (How::SinceLastStart, _) => {
            format!("{} on since the last start, and the game crashed while drawing blocks.", capital(&pack) + if one { " is" } else { " are" })
        }
        (How::Again, _) => format!("The game crashed while drawing blocks, as the last start did right after you turned on {pack}."),
    };
    match t.certainty {
        Certainty::Likely | Certainty::Certain => {
            format!("{what} Turn {it} off (Resource packs tab) and play again: the mods below only drew what it changed.")
        }
        Certainty::Possible => format!("{what} Try once with {it} turned off (Resource packs tab)."),
    }
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// Puts a trigger on a crash: the advice line (unless the crash already
/// has a cause that isn't a mod), and, when the pack is the likely one, the
/// mods that only drew it drop to possible, so the card doesn't offer to
/// turn them off.
pub fn apply(t: &Trigger, suspects: &mut [Suspect], advice: &mut String) {
    if !advice.is_empty() {
        return;
    }
    *advice = self::advice(t);
    if t.certainty > Certainty::Likely {
        return;
    }
    for s in suspects.iter_mut().filter(|s| s.certainty == Certainty::Likely && !s.knock_on) {
        s.certainty = Certainty::Possible;
        s.facts.push(Fact {
            rule: RULE.into(),
            weight: 0,
            text: format!("It failed drawing blocks right after {} came on: the pack is the likelier cause.", names(&t.packs)),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = include_str!("crashdetect_fixtures/atm10-pack-overlay-crash.txt");
    const LOG: &str = include_str!("crashdetect_fixtures/atm10-pack-overlay-latest.log");
    const AGAIN_LOG: &str = include_str!("crashdetect_fixtures/atm10-pack-overlay-again.log");

    #[test]
    fn reads_the_packs_of_each_reload() {
        let rs = reloads(LOG);
        assert_eq!(rs.len(), 2);
        assert!(rs[0].packs.iter().any(|p| p.contains("Lz-PlanksOverlay")));
        assert!(!rs[0].packs.iter().any(|p| p.contains("vanilla") || p.contains("mod_resources")));
        assert_eq!(added(&rs[1].packs, &rs[0].packs), vec!["file/Matcha Block Overlays 2-b1+fusion.zip".to_string()]);
        assert!(rs[1].at.is_some());
    }

    #[test]
    fn a_drawing_crash() {
        assert!(drawing(REPORT));
        assert!(!drawing("Description: Ticking entity\n\njava.lang.NullPointerException\n\tat a.b.C.d(C.java:1)\n"));
        // A model frame in the first trace counts without the words.
        assert!(drawing("Description: Unexpected error\n\njava.lang.NullPointerException\n\tat net.minecraft.client.resources.model.BakedModel.emitBlockQuads(BakedModel.java:521)\n"));
    }

    #[test]
    fn a_pack_turned_on_in_game_just_before_is_likely() {
        let t = find(REPORT, LOG, None).unwrap();
        assert_eq!(t.packs, vec!["Matcha Block Overlays 2-b1+fusion".to_string()]);
        assert_eq!((t.certainty, t.how), (Certainty::Likely, How::InGame));
        assert_eq!(t.secs, Some(40));
        let a = advice(&t);
        assert!(a.starts_with("You turned on the resource pack \"Matcha Block Overlays 2-b1+fusion\" 40 seconds before this crash"), "{a}");
        assert!(a.contains("Turn that pack off (Resource packs tab)"), "{a}");
        // The same with either line ending.
        let lf = |s: &str| s.replace("\r\n", "\n");
        let crlf = |s: &str| lf(s).replace('\n', "\r\n");
        assert_eq!(find(&crlf(REPORT), &crlf(LOG), None), Some(t.clone()));
        assert_eq!(find(&lf(REPORT), &lf(LOG), None), Some(t));
    }

    #[test]
    fn the_start_after_the_same_crash_is_likely_too() {
        // The next start had the pack on from the beginning: its log has one
        // reload, and the last start's log has the crash after turning it on.
        let t = find(REPORT, AGAIN_LOG, Some(LOG)).unwrap();
        assert_eq!(t.packs, vec!["Matcha Block Overlays 2-b1+fusion".to_string()]);
        assert_eq!((t.certainty, t.how), (Certainty::Likely, How::Again));
        // Without the last start's crash it's only on since then.
        let calm = LOG.replace("Description: ", "Said: ");
        let t = find(REPORT, AGAIN_LOG, Some(&calm)).unwrap();
        assert_eq!((t.certainty, t.how), (Certainty::Possible, How::SinceLastStart));
        assert!(advice(&t).starts_with("The resource pack \"Matcha Block Overlays 2-b1+fusion\" is on since the last start"), "{}", advice(&t));
    }

    #[test]
    fn long_ago_or_not_drawing_names_no_pack() {
        // Turned on twenty minutes before: the game drew fine since.
        let late = REPORT.replace("Time: 2026-11-10 21:56:48", "Time: 2026-11-10 22:16:48");
        assert_eq!(find(&late, LOG, None), None);
        // Not a drawing crash.
        let ticking = REPORT.replace("Encountered exception while building chunk meshes", "Ticking entity").replace("BakedModel", "Entity");
        assert_eq!(find(&ticking, LOG, None), None);
        // Nothing turned on in game and no earlier start to compare with.
        assert_eq!(find(REPORT, AGAIN_LOG, None), None);
    }

    #[test]
    fn a_likely_pack_takes_the_drawing_mods_down() {
        let t = find(REPORT, LOG, None).unwrap();
        let mut s = vec![
            Suspect { mod_id: "framedblocks".into(), certainty: Certainty::Likely, ..Default::default() },
            Suspect { mod_id: "fusion".into(), certainty: Certainty::Possible, ..Default::default() },
            Suspect { mod_id: "gone".into(), certainty: Certainty::Certain, ..Default::default() },
        ];
        let mut advice = String::new();
        apply(&t, &mut s, &mut advice);
        assert!(advice.contains("Matcha"));
        assert_eq!(s[0].certainty, Certainty::Possible);
        assert!(s[0].facts.iter().any(|f| f.rule == RULE));
        assert_eq!(s[2].certainty, Certainty::Certain);
        // A cause the crash already has (the wrong Java, …) stays.
        let mut s2 = vec![Suspect { certainty: Certainty::Likely, ..Default::default() }];
        let mut other = "Choose Java 21.".to_string();
        apply(&t, &mut s2, &mut other);
        assert_eq!((other.as_str(), s2[0].certainty), ("Choose Java 21.", Certainty::Likely));
    }
}
