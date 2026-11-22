//! The sync chip in an instance's or a server's header: one place that says
//! how a pack stands with its group (friends) and with its server, in the
//! same words, tones and fold order in Hopper desktop and Hopper Server.
//!
//! This is only the mapping, from a state to what the chip shows
//! ([`Look`]), and the rule for what folds when the header runs out of room
//! ([`plan`]). Drawing, the popover and the buttons are the apps' own.
//!
//! Words are for players: short sentences, no internal names. A chip that
//! says "Needs you" or "Sync failed" never folds into the header's "+N"
//! pill: the player must be able to see it.

use crate::groupstatus::{Status, Tone};

/// What a chip's icon shows. The apps pick their own icon for each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    /// A tick: all in step.
    Check,
    /// A spinner: busy right now.
    Spin,
    /// An arrow down in a circle: behind, something waits to be taken in.
    Behind,
    /// A crossed-out signal: can't reach the other side.
    Offline,
    /// A circle with an exclamation mark: failed.
    Alert,
    /// An arrow down to a tray: needs the player to download or add.
    Down,
    /// An arrow up: changes the player hasn't sent yet.
    Up,
    /// Two arrows: the mods differ.
    Differ,
}

/// Which side the chip is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The pack and its friends group.
    Group,
    /// The pack and its server.
    Server,
}

/// What one chip shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Look {
    pub source: Source,
    pub tone: Tone,
    pub glyph: Glyph,
    /// The full words ("Updating with Mia's changes").
    pub label: String,
    /// The same, shorter ("Updating…"), for a header with less room.
    pub short: String,
    /// Busy right now. The app shows it only after [`crate::settle`]'s delay,
    /// so a check that ends in a blink never flashes.
    pub busy: bool,
    /// Never folds into "+N": it needs the player, or it failed.
    pub fixed: bool,
    /// The player has a list or a choice to make: the page keeps its strip
    /// (group status bar) as well as the chip.
    pub strip: bool,
}

impl Look {
    fn new(source: Source, tone: Tone, glyph: Glyph, label: impl Into<String>, short: impl Into<String>) -> Self {
        Self { source, tone, glyph, label: label.into(), short: short.into(), busy: false, fixed: false, strip: false }
    }

    fn busy(mut self) -> Self {
        self.busy = true;
        self
    }

    fn fixed(mut self) -> Self {
        self.fixed = true;
        self
    }

    fn strip(mut self) -> Self {
        self.fixed = true;
        self.strip = true;
        self
    }
}

/// "Mia's", "Mia and Noah's", "3 people's"; "the group's" for nobody.
fn whose(from: &[String]) -> String {
    let names: Vec<&str> = from.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    match names.as_slice() {
        [] => "the group's".to_string(),
        [one] => format!("{one}'s"),
        [a, b] => format!("{a} and {b}'s"),
        many => format!("{} people's", many.len()),
    }
}

fn mods(n: usize) -> String {
    if n == 1 { "1 mod".to_string() } else { format!("{n} mods") }
}

/// The chip for a pack's copy of its group. `pulling`: Pull now (or a repair)
/// is running; `pull_failed`: the last Pull now said it didn't work;
/// `left_out`: mods wait for the player's yes to their source.
pub fn group_look(status: &Status, pulling: bool, pull_failed: bool, left_out: bool) -> Look {
    use Glyph::*;
    let g = |tone, glyph, label: &str, short: &str| Look::new(Source::Group, tone, glyph, label, short);
    if pull_failed {
        return g(Tone::Bad, Alert, "Sync failed", "Failed").fixed();
    }
    let look = match status {
        Status::InStep { .. } => g(Tone::Good, Check, "In sync", "In sync"),
        Status::Checking => g(Tone::Busy, Spin, "Checking…", "Checking…").busy(),
        Status::Updating { from, .. } => {
            Look::new(Source::Group, Tone::Busy, Spin, format!("Updating with {} changes", whose(from)), "Updating…").busy()
        }
        Status::Sending => g(Tone::Busy, Spin, "Sending your changes", "Sending…").busy(),
        Status::WaitsForGame { .. } => g(Tone::Warn, Behind, "Behind · close the game", "Behind"),
        Status::WaitsForYou { .. } => g(Tone::Warn, Down, "Needs you", "Needs you").strip(),
        Status::Cant { .. } if left_out => g(Tone::Warn, Down, "Needs you", "Needs you").strip(),
        Status::Cant { .. } => g(Tone::Bad, Alert, "Sync failed", "Failed").fixed(),
        Status::Offline { .. } => g(Tone::Quiet, Offline, "Offline", "Offline"),
        Status::NeedsMod { needs } => {
            let l = crate::groupdeps::label(needs.len());
            g(Tone::Bad, Alert, l, "Missing mod").strip()
        }
        Status::Parked { .. } => g(Tone::Warn, Down, "Changes to pull", "To pull").strip(),
        Status::ToPush { .. } => g(Tone::Warn, Up, "Not sent yet", "Not sent").strip(),
        Status::AfterYours { offline: false, .. } => g(Tone::Busy, Behind, "Your change goes first", "Waiting"),
        Status::AfterYours { offline: true, .. } => g(Tone::Quiet, Offline, "Waiting · offline", "Offline"),
        Status::Behind { mods: n, missing } => {
            let (label, short) = if *missing {
                (format!("Missing {}", mods(*n)), "Missing".to_string())
            } else {
                (format!("Behind · {}", mods(*n)), "Behind".to_string())
            };
            Look::new(Source::Group, Tone::Warn, Behind, label, short).strip()
        }
        Status::CantCheck => g(Tone::Warn, Alert, "Couldn't check", "Check failed").fixed(),
        Status::ByHand { mods: list } => {
            let n = list.len().max(1);
            let label = if n == 1 { "1 mod to download".to_string() } else { format!("{n} mods to download") };
            Look::new(Source::Group, Tone::Warn, Down, label, "To download").strip()
        }
    };
    if pulling && !look.busy {
        // Pull now is checking the group: busy, whatever the state under it.
        return Look { busy: true, ..look };
    }
    look
}

/// What a pack's server has to say, from the Server sync page's comparison.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerState {
    /// Same mods on both sides.
    InSync,
    /// `mods` differ. `blocked`: the server runs, so a sync waits for it to
    /// stop.
    Differ { mods: usize, blocked: bool },
    /// The comparison runs.
    Comparing,
    /// A push, pull or removal runs.
    Working(Work),
    /// The server can't be reached.
    Offline,
    /// The comparison or the sync failed.
    Failed,
}

/// What a running sync does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Work {
    Pushing,
    Pulling,
    Removing,
}

/// The chip for a pack and its server. `other` names the other side: the
/// server on an instance's page, the pack on a server's page.
pub fn server_look(state: &ServerState, other: &str) -> Look {
    use Glyph::*;
    let s = |tone, glyph, label: String, short: &str| Look::new(Source::Server, tone, glyph, label, short);
    let other = other.trim();
    match state {
        ServerState::InSync => {
            let label = if other.is_empty() { "In sync".to_string() } else { format!("In sync with {other}") };
            s(Tone::Good, Check, label, "In sync")
        }
        ServerState::Differ { mods: n, .. } => {
            let label = if *n == 1 { "1 mod differs".to_string() } else { format!("{n} mods differ") };
            let short = if *n == 1 { "1 differs".to_string() } else { format!("{n} differ") };
            Look::new(Source::Server, Tone::Warn, Differ, label, short)
        }
        ServerState::Comparing => {
            let label = if other.is_empty() { "Checking…".to_string() } else { format!("Comparing with {other}") };
            s(Tone::Busy, Spin, label, "Checking…").busy()
        }
        ServerState::Working(w) => {
            let (word, other_word) = match w {
                Work::Pushing => ("Pushing…", "Pushing to"),
                Work::Pulling => ("Pulling…", "Pulling from"),
                Work::Removing => ("Removing…", "Removing from"),
            };
            let label = if other.is_empty() { word.to_string() } else { format!("{other_word} {other}") };
            s(Tone::Busy, Spin, label, word).busy()
        }
        ServerState::Offline => {
            let label = if other.is_empty() { "Server offline".to_string() } else { format!("{other} offline") };
            s(Tone::Quiet, Offline, label, "Offline")
        }
        ServerState::Failed => s(Tone::Bad, Alert, "Sync failed".to_string(), "Failed").fixed(),
    }
}

/// How bad a tone is, for the one chip two fold into.
fn rank(t: Tone) -> u8 {
    match t {
        Tone::Good => 0,
        Tone::Quiet => 1,
        Tone::Busy => 2,
        Tone::Warn => 3,
        Tone::Bad => 4,
    }
}

/// The one "Sync" chip a group's and a server's fold into when the header is
/// short of room: the worse of the two sets its colour and icon.
pub fn merged(a: &Look, b: &Look) -> Look {
    let worse = if rank(b.tone) > rank(a.tone) { b } else { a };
    Look {
        source: worse.source,
        tone: worse.tone,
        glyph: worse.glyph,
        label: "Sync".into(),
        short: "Sync".into(),
        busy: worse.busy,
        fixed: a.fixed || b.fixed,
        strip: false,
    }
}

/// How much of a chip shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    Full,
    Short,
    Icon,
}

/// What the header draws at one level of room.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// One entry per chip, in order; `None`: it is folded into "+N" (or
    /// into the merged chip when `merged`).
    pub forms: Vec<Option<Form>>,
    /// The two chips are one "Sync" chip in this form (its position is the
    /// first chip's); `forms` are then all `None`.
    pub merged: Option<Form>,
    /// How many of the header's other pills fold into "+N", from the last.
    pub plain_folded: usize,
    /// How many chips fold into "+N" (both, when they were one merged chip).
    pub chips_folded: usize,
}

/// The levels there are for `fixed.len()` chips (1 or 2; `fixed[i]`: chip i
/// never folds) beside `plain` other pills that may fold (the header's first
/// pill, the run state, never does).
///
/// Order, as room runs out:
/// 1. every label shortens ("Updating with Mia's changes" → "Updating…");
/// 2. with two chips, they become one "Sync" chip; with one, it becomes its
///    icon;
/// 3. the merged chip becomes its icon;
/// 4. chips that need nothing fold into "+N", the last first, then the other
///    pills; a chip that [`Look::fixed`] stays (as its icon).
pub fn max_level(fixed: &[bool], plain: usize) -> usize {
    if fixed.is_empty() {
        return plain;
    }
    let two = fixed.len() >= 2;
    let icon_at = if two { 3 } else { 2 };
    // Two chips fold as the one merged chip.
    let foldable = if two { usize::from(!fixed.iter().any(|f| *f)) } else { usize::from(!fixed[0]) };
    icon_at + foldable + plain
}

pub fn plan(fixed: &[bool], plain: usize, level: usize) -> Plan {
    let n = fixed.len();
    let max = max_level(fixed, plain);
    let level = level.min(max);
    let two = n >= 2;
    let icon_at = if two { 3 } else { 2 };
    let merged_fixed = fixed.iter().any(|f| *f);
    let mut p = Plan { forms: vec![Some(Form::Full); n], merged: None, plain_folded: 0, chips_folded: 0 };
    if n == 0 {
        p.plain_folded = level.min(plain);
        return p;
    }
    let form = match level {
        0 => Form::Full,
        1 => Form::Short,
        _ => Form::Icon,
    };
    if two {
        match level {
            0 | 1 => p.forms = vec![Some(form); n],
            2 => {
                p.forms = vec![None; n];
                p.merged = Some(Form::Short);
            }
            _ => {
                p.forms = vec![None; n];
                p.merged = Some(Form::Icon);
            }
        }
    } else {
        p.forms = vec![Some(form)];
    }
    if level > icon_at {
        let mut steps = level - icon_at;
        // Chips that need nothing go first, the last first. A merged chip
        // that needs the player stays, and so no chip folds at all.
        if steps > 0 && !(if two { merged_fixed } else { fixed[0] }) {
            steps -= 1;
            if two {
                p.merged = None;
            } else {
                p.forms[0] = None;
            }
            p.chips_folded = n;
        }
        p.plain_folded = steps.min(plain);
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(name: &str) -> String {
        name.to_string()
    }

    #[test]
    fn updating_names_whose_changes() {
        let st = Status::Updating { changes: 3, from: vec![s("Mia")] };
        let l = group_look(&st, false, false, false);
        assert_eq!(l.label, "Updating with Mia's changes");
        assert_eq!(l.short, "Updating…");
        assert!(l.busy && !l.fixed && !l.strip);
        let two = Status::Updating { changes: 0, from: vec![s("Mia"), s("Noah")] };
        assert_eq!(group_look(&two, false, false, false).label, "Updating with Mia and Noah's changes");
        let many = Status::Updating { changes: 0, from: vec![s("A"), s("B"), s("C")] };
        assert_eq!(group_look(&many, false, false, false).label, "Updating with 3 people's changes");
        let none = Status::Updating { changes: 0, from: vec![] };
        assert_eq!(group_look(&none, false, false, false).label, "Updating with the group's changes");
    }

    #[test]
    fn rest_and_busy_states() {
        let ok = group_look(&Status::InStep { checked: None }, false, false, false);
        assert_eq!((ok.tone, ok.label.as_str(), ok.busy, ok.fixed), (Tone::Good, "In sync", false, false));
        let chk = group_look(&Status::Checking, false, false, false);
        assert_eq!((chk.tone, chk.busy), (Tone::Busy, true));
        // Pull now running makes any state busy, and keeps its words.
        let pull = group_look(&Status::InStep { checked: None }, true, false, false);
        assert!(pull.busy);
        assert_eq!(pull.label, "In sync");
    }

    #[test]
    fn needs_you_and_failed_never_fold() {
        for st in [
            Status::ByHand { mods: vec![s("A"), s("B")] },
            Status::ToPush { changes: 2, after: None },
            Status::WaitsForYou { title: s("Pack 8.6") },
            Status::Behind { mods: 3, missing: false },
        ] {
            let l = group_look(&st, false, false, false);
            assert!(l.fixed && l.strip, "{st:?}");
        }
        let failed = group_look(&Status::Cant { why: s("x"), again: false, mods: vec![] }, false, false, false);
        assert!(failed.fixed && !failed.strip);
        assert_eq!((failed.tone, failed.label.as_str()), (Tone::Bad, "Sync failed"));
        // The same failure with mods waiting for a yes is a choice: it keeps its strip.
        let choice = group_look(&Status::Cant { why: s("x"), again: false, mods: vec![] }, false, false, true);
        assert!(choice.strip && choice.label == "Needs you");
        let bad_pull = group_look(&Status::InStep { checked: None }, false, true, false);
        assert!(bad_pull.fixed && bad_pull.tone == Tone::Bad);
    }

    #[test]
    fn behind_offline_and_check_failed_get_no_strip() {
        for st in [Status::Offline { checked: None }, Status::CantCheck, Status::WaitsForGame { what: vec![] }] {
            assert!(!group_look(&st, false, false, false).strip, "{st:?}");
        }
        assert_eq!(group_look(&Status::ByHand { mods: vec![s("A")] }, false, false, false).label, "1 mod to download");
        assert_eq!(group_look(&Status::ByHand { mods: vec![s("A"), s("B")] }, false, false, false).label, "2 mods to download");
    }

    #[test]
    fn server_words() {
        let ok = server_look(&ServerState::InSync, "ATM10 Server");
        assert_eq!((ok.label.as_str(), ok.short.as_str(), ok.source), ("In sync with ATM10 Server", "In sync", Source::Server));
        let d = server_look(&ServerState::Differ { mods: 3, blocked: false }, "x");
        assert_eq!((d.label.as_str(), d.short.as_str(), d.tone), ("3 mods differ", "3 differ", Tone::Warn));
        assert_eq!(server_look(&ServerState::Differ { mods: 1, blocked: true }, "x").label, "1 mod differs");
        let w = server_look(&ServerState::Working(Work::Pushing), "ATM10");
        assert!(w.busy);
        assert_eq!((w.label.as_str(), w.short.as_str()), ("Pushing to ATM10", "Pushing…"));
        assert_eq!(server_look(&ServerState::Offline, "").label, "Server offline");
        let f = server_look(&ServerState::Failed, "");
        assert!(f.fixed && f.tone == Tone::Bad);
    }

    #[test]
    fn one_chip_folds_label_icon_then_plus_n() {
        // A quiet chip beside the run state and one more pill.
        let fixed = [false];
        assert_eq!(max_level(&fixed, 1), 2 + 1 + 1);
        assert_eq!(plan(&fixed, 1, 0).forms, vec![Some(Form::Full)]);
        assert_eq!(plan(&fixed, 1, 1).forms, vec![Some(Form::Short)]);
        assert_eq!(plan(&fixed, 1, 2).forms, vec![Some(Form::Icon)]);
        let p = plan(&fixed, 1, 3);
        assert_eq!((p.forms.clone(), p.chips_folded, p.plain_folded), (vec![None], 1, 0));
        let p = plan(&fixed, 1, 4);
        assert_eq!((p.chips_folded, p.plain_folded), (1, 1));
        // Past the last level stays at the last.
        assert_eq!(plan(&fixed, 1, 99), plan(&fixed, 1, 4));
    }

    #[test]
    fn a_fixed_chip_stays_as_its_icon() {
        let fixed = [true];
        assert_eq!(max_level(&fixed, 1), 2 + 0 + 1);
        let p = plan(&fixed, 1, 3);
        assert_eq!((p.forms.clone(), p.chips_folded, p.plain_folded), (vec![Some(Form::Icon)], 0, 1));
        // Never gone, however narrow.
        assert_eq!(plan(&fixed, 1, 99).forms, vec![Some(Form::Icon)]);
    }

    #[test]
    fn two_chips_become_one_sync_chip() {
        let fixed = [false, false];
        assert_eq!(plan(&fixed, 0, 0).forms, vec![Some(Form::Full), Some(Form::Full)]);
        assert_eq!(plan(&fixed, 0, 1).forms, vec![Some(Form::Short), Some(Form::Short)]);
        let p = plan(&fixed, 0, 2);
        assert_eq!((p.merged, p.forms.clone()), (Some(Form::Short), vec![None, None]));
        assert_eq!(plan(&fixed, 0, 3).merged, Some(Form::Icon));
        let p = plan(&fixed, 0, 4);
        assert_eq!((p.merged, p.chips_folded), (None, 2));
        assert_eq!(max_level(&fixed, 0), 3 + 1);
    }

    #[test]
    fn a_needs_you_chip_keeps_the_merged_chip() {
        let fixed = [true, false];
        assert_eq!(max_level(&fixed, 0), 3);
        assert_eq!(plan(&fixed, 0, 3).merged, Some(Form::Icon));
        assert_eq!(plan(&fixed, 0, 99).merged, Some(Form::Icon));
    }

    #[test]
    fn merged_chip_takes_the_worse_look() {
        let a = group_look(&Status::InStep { checked: None }, false, false, false);
        let b = server_look(&ServerState::Differ { mods: 2, blocked: false }, "S");
        let m = merged(&a, &b);
        assert_eq!((m.tone, m.glyph, m.label.as_str(), m.fixed), (Tone::Warn, Glyph::Differ, "Sync", false));
        let bad = server_look(&ServerState::Failed, "");
        assert!(merged(&a, &bad).fixed);
        assert_eq!(merged(&bad, &a).tone, Tone::Bad);
    }

    #[test]
    fn no_chips_only_plain_pills_fold() {
        let p = plan(&[], 3, 2);
        assert_eq!(p.plain_folded, 2);
    }
}
