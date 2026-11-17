//! Whether this PC's copy of a group pack is in step with the group, and
//! why not, in words a player can act on (`design/group-sync-status.md`).
//!
//! "In step" means the copy has what the group's latest state says, mod by
//! mod: nothing another member sent waits to be taken in, nothing was left
//! out for want of trust in its source, nothing waits for the game to close
//! or for the player. A mod kept only on this PC and a mod the player locked
//! never count as behind.
//!
//! The group's latest state isn't stored in one place: each PC takes the
//! others' codes in ([`crate::groupsync`]). So this is worked out from what
//! this PC knows ([`PackCopy`]) plus a few facts the app holds (a task
//! running, a failed try, a pack version waiting for the game); the app
//! gathers them into an [`Input`] and [`status`] decides. For the other
//! members see [`member_status`].

use crate::grouppack::{Due, Wait};
use crate::groupsync::PackCopy;
use std::collections::HashSet;

/// A failed try to take the group's changes in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Failure<'a> {
    /// Why, in plain words.
    pub why: &'a str,
    /// Hopper tries again by itself.
    pub again: bool,
}

/// What the app knows about one group copy.
#[derive(Clone, Copy)]
pub struct Input<'a> {
    pub copy: &'a PackCopy,
    /// This PC reaches the group's server now.
    pub connected: bool,
    /// When this PC last heard from it (unix seconds).
    pub checked: Option<u64>,
    /// Now (unix seconds).
    pub now: u64,
    /// A task is taking the group's changes in.
    pub merging: bool,
    /// The group moved the pack to a version that isn't in yet.
    pub pack_due: Option<&'a Due>,
    /// The last try failed.
    pub failure: Option<Failure<'a>>,
    /// Settings › Mod sources.
    pub allow_sources: bool,
    /// The group moved the loader and the game still runs.
    pub loader_waits: bool,
    /// Mods the running game holds open: a removal, rename or switch that
    /// happens when it closes (`.hopper-locked.json`).
    pub jars_waiting: usize,
    /// A change made here isn't sent yet.
    pub unsent: bool,
    /// Another member's change waits for that one to go out first.
    pub held: bool,
}

/// How one copy stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Same as the group.
    InStep { checked: Option<u64> },
    /// Taking the others' changes in (`changes`: about how many, 0 when
    /// not known; `from`: whose).
    Updating { changes: usize, from: Vec<String> },
    /// A change made here is on its way to the group.
    Sending,
    /// Behind, until the game closes; `what` says which parts.
    WaitsForGame { what: Vec<String> },
    /// Behind, until the player picks (a pack update with settings both
    /// changed).
    WaitsForYou { title: String },
    /// Couldn't be done; `again`: Hopper tries by itself.
    Cant { why: String, again: bool, mods: Vec<String> },
    /// Not connected: what this PC knows may be out of date.
    Offline { checked: Option<u64> },
}

/// How loud a status is: what colour a chip gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Good,
    Busy,
    Warn,
    Bad,
    Quiet,
}

impl Status {
    /// The short words for a chip.
    pub fn label(&self) -> &'static str {
        match self {
            Status::InStep { .. } => "In step",
            Status::Updating { .. } => "Updating",
            Status::Sending => "Sending your changes",
            Status::WaitsForGame { .. } => "Behind: close the game",
            Status::WaitsForYou { .. } => "Behind: needs you",
            Status::Cant { .. } => "Can't update",
            Status::Offline { .. } => "Offline",
        }
    }

    pub fn tone(&self) -> Tone {
        match self {
            Status::InStep { .. } => Tone::Good,
            Status::Updating { .. } | Status::Sending => Tone::Busy,
            Status::WaitsForGame { .. } | Status::WaitsForYou { .. } => Tone::Warn,
            Status::Cant { .. } => Tone::Bad,
            Status::Offline { .. } => Tone::Quiet,
        }
    }

    /// Which to show first among several copies: the ones that need the
    /// player.
    pub fn rank(&self) -> u8 {
        match self {
            Status::Cant { .. } => 0,
            Status::WaitsForYou { .. } => 1,
            Status::WaitsForGame { .. } => 2,
            Status::Offline { .. } => 3,
            Status::Updating { .. } | Status::Sending => 4,
            Status::InStep { .. } => 5,
        }
    }

    /// The line under the label: what is going on and what happens next.
    pub fn detail(&self, now: u64) -> String {
        match self {
            Status::InStep { checked } => match checked {
                Some(at) => format!("Same as the group. Checked {}.", ago(now, *at)),
                None => "Same as the group.".to_string(),
            },
            Status::Updating { changes, from } => {
                let who = people(from);
                match (*changes, who.is_empty()) {
                    (0, true) => "Getting the group's changes.".to_string(),
                    (0, false) => format!("Getting {who}'s changes."),
                    (1, true) => "Taking in 1 change.".to_string(),
                    (n, true) => format!("Taking in {n} changes."),
                    (1, false) => format!("Taking in 1 change from {who}."),
                    (n, false) => format!("Taking in {n} changes from {who}."),
                }
            }
            Status::Sending => "Your changes reach the group in a moment.".to_string(),
            Status::WaitsForGame { what } => {
                format!("Waiting for the game to close: {}. They go in as soon as it does.", join(what))
            }
            Status::WaitsForYou { title } => format!("{title}. Some of your settings changed in this update too: pick what to keep."),
            Status::Cant { why, again, .. } => {
                let again = if *again { " Hopper tries again soon." } else { "" };
                format!("{}{again}", sentence(why))
            }
            Status::Offline { checked } => match checked {
                Some(at) => format!("Can't reach the group right now, so your copy may be out of date. Last checked {}.", ago(now, *at)),
                None => "Can't reach the group right now, so your copy may be out of date.".to_string(),
            },
        }
    }
}

/// Works out how the copy stands. When several things apply, what needs the
/// player comes first (see the design note).
pub fn status(i: &Input) -> Status {
    // Couldn't: a failed try, the pack version's failed update, or mods from
    // links this PC doesn't trust yet.
    if let Some(f) = i.failure {
        return Status::Cant { why: f.why.to_string(), again: f.again, mods: skipped_names(i.copy) };
    }
    if let Some(Wait::Failed(why)) = i.pack_due.map(|d| &d.wait) {
        let title = i.pack_due.map(Due::title).unwrap_or_default();
        return Status::Cant { why: format!("{title}, but the update failed: {why}"), again: true, mods: Vec::new() };
    }
    if !i.copy.skipped.is_empty() {
        return Status::Cant { why: skipped_says(i.copy, i.allow_sources), again: false, mods: skipped_names(i.copy) };
    }
    if let Some(d) = i.pack_due.filter(|d| d.wait == Wait::Pick) {
        return Status::WaitsForYou { title: d.title() };
    }
    let mut game: Vec<String> = Vec::new();
    if let Some(d) = i.pack_due.filter(|d| d.wait == Wait::Game) {
        game.push(format!("the new pack version ({})", d.title()));
    }
    if i.loader_waits {
        game.push("the group's loader".to_string());
    }
    if i.jars_waiting > 0 {
        game.push(if i.jars_waiting == 1 { "1 mod the game has open".to_string() } else { format!("{} mods the game has open", i.jars_waiting) });
    }
    if !game.is_empty() {
        return Status::WaitsForGame { what: game };
    }
    let waiting = !i.copy.waiting.is_empty();
    if i.merging || (waiting && !i.held) {
        let from: Vec<String> = i.copy.waiting.values().map(|g| g.name.clone()).filter(|n| !n.is_empty()).collect();
        return Status::Updating { changes: waiting_changes(i.copy), from };
    }
    if !i.connected {
        return Status::Offline { checked: i.checked };
    }
    if i.unsent || i.held {
        return Status::Sending;
    }
    Status::InStep { checked: i.checked }
}

/// About how many changes the codes waiting to be taken in carry: the mods
/// each member changed since the last code taken from them (their stamps
/// say so, as `groupsync::plan` reads them), each mod once. 0 when the
/// codes carry none (an older Hopper).
pub fn waiting_changes(copy: &PackCopy) -> usize {
    let mut keys: HashSet<&str> = HashSet::new();
    for (from, g) in &copy.waiting {
        let had = copy.got.get(from).map_or(0, |p| p.seq);
        for (k, s) in &g.stamps {
            let made_now = s.seq == 0 && s.by == *from;
            let since = s.seq > had && s.seq <= g.seq;
            if made_now || since {
                keys.insert(k.as_str());
            }
        }
    }
    // The loader and the pack version aren't mods but are changes.
    keys.len()
}

fn skipped_names(copy: &PackCopy) -> Vec<String> {
    copy.skipped.iter().map(|s| s.name.clone()).collect()
}

/// Why the mods left out aren't here, for the player.
fn skipped_says(copy: &PackCopy, allow_sources: bool) -> String {
    let what = match copy.skipped.as_slice() {
        [one] => format!("{} ({})", one.name, one.from),
        many => {
            let names: Vec<String> = many.iter().take(3).map(|s| s.name.clone()).collect();
            let more = many.len().saturating_sub(3);
            if more > 0 { format!("{} mods, among them {} and {}", many.len(), names[..2].join(", "), names[2]) } else { format!("{} mods ({})", many.len(), names.join(", ")) }
        }
    };
    let verb = if copy.skipped.len() == 1 { "is" } else { "are" };
    let from = if copy.skipped.len() == 1 { "a link" } else { "links" };
    let why = if allow_sources {
        format!("This PC hasn't said yes to {from} like that yet.")
    } else {
        "Mods from GitHub and other links are off on this PC.".to_string()
    };
    format!("{what} {verb} in the group but not on this PC. {why}")
}

/// What Pull now tells the player once it has looked at the group and taken
/// in what it could: `reached`: the group's server answered; `made`: how many
/// changes went in just now (`from`: whose); `after`: how the copy stands
/// then. The second value is true when it should read as a problem.
pub fn pull_says(reached: bool, made: usize, from: &[String], after: &Status, now: u64) -> (String, bool) {
    let took = match (made, people(from).as_str()) {
        (0, _) => String::new(),
        (1, "") => "Took in 1 change.".to_string(),
        (n, "") => format!("Took in {n} changes."),
        (1, who) => format!("Took in 1 change from {who}."),
        (n, who) => format!("Took in {n} changes from {who}."),
    };
    if !reached {
        let lead = if took.is_empty() { String::new() } else { format!("{took} ") };
        return (format!("{lead}Couldn't reach the group to check for newer changes. Try again in a moment."), true);
    }
    match after {
        Status::InStep { .. } | Status::Sending | Status::Updating { .. } if took.is_empty() => ("Already in step with the group.".to_string(), false),
        Status::InStep { .. } | Status::Sending | Status::Updating { .. } => (format!("{took} Now in step with the group."), false),
        other => {
            let lead = if took.is_empty() { String::new() } else { format!("{took} ") };
            (format!("{lead}{}", other.detail(now)), matches!(other, Status::Cant { .. }))
        }
    }
}

// ---- other members -------------------------------------------------------------------

/// Whether another member's PC has taken in the group's latest changes, as
/// far as this PC can tell from the last code they sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Member {
    /// Nothing from them about these packs: they don't keep a copy, or
    /// this PC hasn't heard from it yet. No chip.
    Unknown,
    InStep,
    /// `changes` changes the group made (mods, the pack version, the loader)
    /// their last message doesn't have yet.
    Behind { changes: usize },
}

impl Member {
    pub fn label(&self) -> String {
        match self {
            Member::Unknown => String::new(),
            Member::InStep => "In step".to_string(),
            Member::Behind { changes: 1 } => "Behind by 1".to_string(),
            Member::Behind { changes } => format!("Behind by {changes}"),
        }
    }

    pub fn tone(&self) -> Tone {
        match self {
            Member::Unknown => Tone::Quiet,
            Member::InStep => Tone::Good,
            Member::Behind { .. } => Tone::Warn,
        }
    }

    /// What it means, for the chip's tooltip. It can't say why: that only
    /// the member's own PC knows (see the design note).
    pub fn detail(&self, name: &str) -> String {
        match self {
            Member::Unknown => String::new(),
            Member::InStep => format!("{name}'s copy has the group's latest changes."),
            Member::Behind { changes } => {
                let n = if *changes == 1 { "1 change".to_string() } else { format!("{changes} changes") };
                format!("{name}'s copy is missing {n} the group made. They may be offline, or waiting for the game to close.")
            }
        }
    }
}

/// How `member`'s copies stand, from this PC's copies of the group's packs
/// (`copies`): each change in `marks` (the group's latest state, as this PC
/// knows it) that the member's last code doesn't carry counts once.
///
/// A mod the member's code carries no stamp for counts only when the change
/// is newer than that code: otherwise the member may keep that mod only on
/// their PC or have it locked, which is theirs to decide.
pub fn member_status(copies: &[&PackCopy], member: &str) -> Member {
    let mut known = false;
    let mut behind = 0;
    for c in copies {
        let Some(g) = c.waiting.get(member).or_else(|| c.got.get(member)) else { continue };
        known = true;
        for (k, m) in &c.marks {
            if m.by == member || m.seq == 0 {
                continue;
            }
            // "Made in this very code" is dated by the code itself.
            let has = g.stamps.get(k).map(|s| if s.seq == 0 { g.seq } else { s.seq });
            let behind_on = match has {
                Some(seq) => seq < m.seq,
                None => g.seq < m.seq,
            };
            if behind_on {
                behind += 1;
            }
        }
    }
    match (known, behind) {
        (false, _) => Member::Unknown,
        (true, 0) => Member::InStep,
        (true, n) => Member::Behind { changes: n },
    }
}

// ---- when a try is due, and Pull now --------------------------------------------------

/// How long a group pack version whose update failed waits before it is
/// tried again by itself.
pub const PACK_RETRY: std::time::Duration = std::time::Duration::from_secs(600);

/// How soon a group pack version that waited for the game looks again once
/// the game seems closed.
pub const GAME_RECHECK: std::time::Duration = std::time::Duration::from_secs(5);

/// Whether the waiting codes may be taken in now, given the last failed try
/// (`retry`, kept against the newest waiting code `newest`). `forced`: the
/// player pressed Pull now, so no wait counts, not even GitHub's limit (the
/// try says so again if it still holds).
pub fn merge_due(retry: Option<&crate::recipe::Retry>, newest: &str, now: std::time::Instant, forced: bool) -> bool {
    if forced {
        return true;
    }
    match retry {
        // GitHub's limit: a newer code would fail the same way.
        Some(r) if r.waits_for_limit(now) => false,
        Some(r) if r.code == newest => r.next.is_some_and(|t| now >= t),
        _ => true,
    }
}

/// Whether a group copy may take its members' changes in, when the pack
/// version they moved to couldn't go in last time (`waited`: since then;
/// `game_runs`: the store's map of running games). Picking per file is the
/// player's, in the update dialog: not even Pull now does it for them.
pub fn pack_due_ready(wait: &Wait, game_runs: bool, waited: std::time::Duration, forced: bool) -> bool {
    match wait {
        // The merge looks at the processes itself, so it can see a game the
        // store's map doesn't list yet: not every tick, then.
        Wait::Game => forced || (!game_runs && waited >= GAME_RECHECK),
        Wait::Pick => false,
        Wait::Failed(_) => forced || waited >= PACK_RETRY,
    }
}

/// "Mia", "Mia and Noah", "3 people".
fn people(names: &[String]) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for n in names {
        if !seen.contains(&n.as_str()) {
            seen.push(n);
        }
    }
    match seen.as_slice() {
        [] => String::new(),
        [a] => (*a).to_string(),
        [a, b] => format!("{a} and {b}"),
        many => format!("{} people", many.len()),
    }
}

/// "a, b and c".
fn join(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [a] => a.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// A sentence ends with a full stop.
fn sentence(s: &str) -> String {
    let s = s.trim();
    if s.ends_with(['.', '!', '?']) { s.to_string() } else { format!("{s}.") }
}

/// "just now", "2 min ago", "3 h ago", "2 days ago".
pub fn ago(now: u64, at: u64) -> String {
    let d = now.saturating_sub(at);
    let (n, unit) = match d {
        0..60 => return "just now".into(),
        60..3600 => (d / 60, "min"),
        3600..86400 => (d / 3600, "h"),
        86400..2_592_000 => (d / 86400, "day"),
        2_592_000..31_536_000 => (d / 2_592_000, "month"),
        _ => (d / 31_536_000, "year"),
    };
    let s = if n == 1 || unit == "min" || unit == "h" { "" } else { "s" };
    format!("{n} {unit}{s} ago")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grouppack::Version;
    use crate::groupsync::{Got, Skipped, Stamp};

    fn input(copy: &PackCopy) -> Input<'_> {
        Input {
            copy,
            connected: true,
            checked: Some(1000),
            now: 1120,
            merging: false,
            pack_due: None,
            failure: None,
            allow_sources: true,
            loader_waits: false,
            jars_waiting: 0,
            unsent: false,
            held: false,
        }
    }

    fn due(wait: Wait) -> Due {
        Due {
            to: Version::Curse { project: 1, file: 86 },
            file: None,
            version: "8.6".into(),
            pack: "All the Mods 10".into(),
            who: "Mia".into(),
            back: false,
            wait,
        }
    }

    fn waiting_from(copy: &mut PackCopy, who: &str, name: &str, seq: u64, stamps: &[(&str, u64, &str)]) {
        let stamps = stamps.iter().map(|(k, s, by)| (k.to_string(), Stamp { seq: *s, by: by.to_string() })).collect();
        copy.waiting.insert(who.into(), Got { code: "x".into(), seq, at: 0, name: name.into(), stamps });
    }

    fn skipped(name: &str) -> Skipped {
        Skipped { key: format!("s:{name}"), name: name.into(), from: "GitHub: octo/repo".into(), by: "Mia".into(), entry: None, approved: false }
    }

    #[test]
    fn nothing_waiting_is_in_step_with_the_time_it_checked() {
        let c = PackCopy::default();
        let s = status(&input(&c));
        assert_eq!(s, Status::InStep { checked: Some(1000) });
        assert_eq!(s.label(), "In step");
        assert_eq!(s.detail(1120), "Same as the group. Checked 2 min ago.");
        assert_eq!(s.tone(), Tone::Good);
    }

    #[test]
    fn waiting_codes_are_updating_and_count_the_changes() {
        let mut c = PackCopy::default();
        c.got.insert("mia".into(), Got { seq: 10, ..Default::default() });
        // One change made since the last code taken (seq 12), one made in the
        // very code (seq 0), one older than the last code taken (seq 5).
        waiting_from(&mut c, "mia", "Mia", 20, &[("p:1", 12, "mia"), ("p:2", 0, "mia"), ("p:3", 5, "mia")]);
        let s = status(&input(&c));
        assert_eq!(s, Status::Updating { changes: 2, from: vec!["Mia".into()] });
        assert_eq!(s.detail(1120), "Taking in 2 changes from Mia.");
        assert_eq!(s.label(), "Updating");
    }

    #[test]
    fn a_change_two_members_both_made_counts_once() {
        let mut c = PackCopy::default();
        waiting_from(&mut c, "mia", "Mia", 20, &[("p:1", 12, "mia")]);
        waiting_from(&mut c, "noah", "Noah", 21, &[("p:1", 12, "mia"), ("p:9", 0, "noah")]);
        assert_eq!(waiting_changes(&c), 2);
        assert_eq!(status(&input(&c)).detail(0), "Taking in 2 changes from Mia and Noah.");
    }

    #[test]
    fn a_running_task_with_nothing_counted_is_updating() {
        let c = PackCopy::default();
        let mut i = input(&c);
        i.merging = true;
        assert_eq!(status(&i).detail(0), "Getting the group's changes.");
    }

    #[test]
    fn a_pack_version_waiting_for_the_game_is_behind() {
        let c = PackCopy::default();
        let d = due(Wait::Game);
        let mut i = input(&c);
        i.pack_due = Some(&d);
        let s = status(&i);
        assert_eq!(s.label(), "Behind: close the game");
        assert_eq!(s.tone(), Tone::Warn);
        assert_eq!(
            s.detail(0),
            "Waiting for the game to close: the new pack version (Mia updated All the Mods 10 to 8.6). They go in as soon as it does."
        );
    }

    #[test]
    fn the_loader_and_jars_the_game_holds_wait_for_the_game_too() {
        let c = PackCopy::default();
        let mut i = input(&c);
        i.loader_waits = true;
        i.jars_waiting = 3;
        assert_eq!(status(&i).detail(0), "Waiting for the game to close: the group's loader and 3 mods the game has open. They go in as soon as it does.");
        i.loader_waits = false;
        i.jars_waiting = 1;
        assert_eq!(status(&i), Status::WaitsForGame { what: vec!["1 mod the game has open".into()] });
    }

    #[test]
    fn adding_a_mod_never_waits_for_the_game() {
        // Waiting codes are updating whatever the game does: only the pack
        // version, the loader and held jars wait for it.
        let mut c = PackCopy::default();
        waiting_from(&mut c, "mia", "Mia", 20, &[("p:1", 0, "mia")]);
        let s = status(&input(&c));
        assert!(matches!(s, Status::Updating { .. }));
    }

    #[test]
    fn a_pick_waits_for_the_player() {
        let c = PackCopy::default();
        let d = due(Wait::Pick);
        let mut i = input(&c);
        i.pack_due = Some(&d);
        let s = status(&i);
        assert_eq!(s.label(), "Behind: needs you");
        assert!(s.detail(0).starts_with("Mia updated All the Mods 10 to 8.6. Some of your settings changed"));
    }

    #[test]
    fn a_failed_try_says_why_and_whether_it_tries_again() {
        let c = PackCopy::default();
        let mut i = input(&c);
        i.failure = Some(Failure { why: "The mod sites wouldn't give Hopper that file.", again: true });
        let s = status(&i);
        assert_eq!(s.label(), "Can't update");
        assert_eq!(s.tone(), Tone::Bad);
        assert_eq!(s.detail(0), "The mod sites wouldn't give Hopper that file. Hopper tries again soon.");
        i.failure = Some(Failure { why: "GitHub's limit.", again: false });
        assert_eq!(status(&i).detail(0), "GitHub's limit.");
    }

    #[test]
    fn a_failed_pack_update_is_cant_update() {
        let c = PackCopy::default();
        let d = due(Wait::Failed("the pack's site doesn't have that version.".into()));
        let mut i = input(&c);
        i.pack_due = Some(&d);
        let s = status(&i);
        assert_eq!(s.label(), "Can't update");
        assert!(s.detail(0).contains("the pack's site doesn't have that version."));
    }

    #[test]
    fn a_mod_from_an_untrusted_link_is_cant_update_with_its_name() {
        let mut c = PackCopy::default();
        c.skipped.push(skipped("Sodium Extra"));
        let mut i = input(&c);
        i.allow_sources = true;
        let s = status(&i);
        assert_eq!(s.label(), "Can't update");
        assert_eq!(
            s.detail(0),
            "Sodium Extra (GitHub: octo/repo) is in the group but not on this PC. This PC hasn't said yes to a link like that yet."
        );
        i.allow_sources = false;
        assert!(status(&i).detail(0).ends_with("Mods from GitHub and other links are off on this PC."));
        c.skipped.push(skipped("Jade"));
        c.skipped.push(skipped("Xaero"));
        c.skipped.push(skipped("Create"));
        let i = input(&c);
        assert!(status(&i).detail(0).starts_with("4 mods, among them Sodium Extra, Jade and Xaero are in the group"));
    }

    #[test]
    fn failing_beats_waiting_beats_updating() {
        let mut c = PackCopy::default();
        waiting_from(&mut c, "mia", "Mia", 20, &[("p:1", 0, "mia")]);
        let d = due(Wait::Game);
        let mut i = input(&c);
        i.pack_due = Some(&d);
        assert_eq!(status(&i).label(), "Behind: close the game");
        i.failure = Some(Failure { why: "x", again: true });
        assert_eq!(status(&i).label(), "Can't update");
    }

    #[test]
    fn not_connected_is_offline_with_when_it_last_checked() {
        let c = PackCopy::default();
        let mut i = input(&c);
        i.connected = false;
        let s = status(&i);
        assert_eq!(s.label(), "Offline");
        assert_eq!(s.detail(1000 + 8 * 60), "Can't reach the group right now, so your copy may be out of date. Last checked 8 min ago.");
        assert_eq!(s.tone(), Tone::Quiet);
        i.checked = None;
        assert_eq!(status(&i).detail(0), "Can't reach the group right now, so your copy may be out of date.");
    }

    #[test]
    fn a_change_not_sent_yet_is_sending_and_a_held_one_too() {
        let mut c = PackCopy::default();
        let mut i = input(&c);
        i.unsent = true;
        assert_eq!(status(&i), Status::Sending);
        // Their change waits for ours to go out first: still sending, not
        // updating.
        waiting_from(&mut c, "mia", "Mia", 20, &[("p:1", 0, "mia")]);
        let mut i = input(&c);
        i.held = true;
        assert_eq!(status(&i), Status::Sending);
    }

    #[test]
    fn people_and_times_read_plainly() {
        assert_eq!(people(&["Mia".into(), "Mia".into()]), "Mia");
        assert_eq!(people(&["a".into(), "b".into(), "c".into()]), "3 people");
        assert_eq!(ago(100, 100), "just now");
        assert_eq!(ago(100 + 3 * 3600, 100), "3 h ago");
        assert_eq!(ago(100 + 86400, 100), "1 day ago");
    }

    #[test]
    fn pull_now_ignores_the_wait_after_a_failed_try() {
        use std::time::{Duration, Instant};
        let now = Instant::now();
        let e = anyhow::anyhow!("The mod sites wouldn't give Hopper that file.");
        let r = crate::recipe::Retry::failed(None, "20", &e, 1, now);
        assert!(r.next.is_some_and(|t| t > now), "the retry waits");
        // On its own it waits for its time, for the same code only.
        assert!(!merge_due(Some(&r), "20", now, false));
        assert!(merge_due(Some(&r), "21", now, false), "a newer code is tried at once");
        assert!(merge_due(Some(&r), "20", now + Duration::from_secs(3600), false));
        // Pull now doesn't wait.
        assert!(merge_due(Some(&r), "20", now, true));
        assert!(merge_due(None, "20", now, false));
    }

    #[test]
    fn pull_now_ignores_the_wait_for_githubs_limit() {
        use std::time::Instant;
        let now = Instant::now();
        let r = crate::recipe::Retry { code: "20".into(), failures: 1, next: Some(now), error: "limit".into(), job: 0, limited: Some(now + std::time::Duration::from_secs(600)), tell: true, gave_up: false };
        assert!(!merge_due(Some(&r), "21", now, false));
        assert!(merge_due(Some(&r), "21", now, true));
    }

    #[test]
    fn pull_now_ignores_a_given_up_try_but_not_a_pick() {
        use std::time::{Duration, Instant};
        let now = Instant::now();
        let mut r = crate::recipe::Retry::failed(None, "20", &anyhow::anyhow!("x"), 1, now);
        r.next = None;
        r.gave_up = true;
        assert!(!merge_due(Some(&r), "20", now, false));
        assert!(merge_due(Some(&r), "20", now, true));
        let none = Duration::from_secs(0);
        // The pack version waiting for the game, a failed update and a pick.
        assert!(!pack_due_ready(&Wait::Game, true, none, false), "the game still runs");
        assert!(!pack_due_ready(&Wait::Game, false, none, false), "looked at a moment ago");
        assert!(pack_due_ready(&Wait::Game, false, GAME_RECHECK, false));
        assert!(pack_due_ready(&Wait::Game, true, none, true), "Pull now looks again; the merge sees the game itself");
        assert!(!pack_due_ready(&Wait::Failed("x".into()), false, none, false));
        assert!(pack_due_ready(&Wait::Failed("x".into()), false, PACK_RETRY, false));
        assert!(pack_due_ready(&Wait::Failed("x".into()), false, none, true));
        assert!(!pack_due_ready(&Wait::Pick, false, PACK_RETRY, true), "a pick is the player's");
    }

    #[test]
    fn pull_now_says_what_it_did() {
        let mia = vec!["Mia".to_string()];
        let in_step = Status::InStep { checked: Some(100) };
        assert_eq!(pull_says(true, 0, &[], &in_step, 100), ("Already in step with the group.".to_string(), false));
        assert_eq!(pull_says(true, 3, &mia, &in_step, 100), ("Took in 3 changes from Mia. Now in step with the group.".to_string(), false));
        assert_eq!(pull_says(true, 1, &[], &in_step, 100).0, "Took in 1 change. Now in step with the group.");
        let (t, bad) = pull_says(false, 0, &[], &Status::Offline { checked: None }, 100);
        assert!(bad && t.starts_with("Couldn't reach the group"), "{t}");
        // Something is still left: the reason is said, and a failure reads as one.
        let cant = Status::Cant { why: "Sodium Extra (GitHub: octo/repo) is in the group but not on this PC.".into(), again: false, mods: vec![] };
        let (t, bad) = pull_says(true, 2, &mia, &cant, 100);
        assert!(bad && t.starts_with("Took in 2 changes from Mia. Sodium Extra"), "{t}");
        let game = Status::WaitsForGame { what: vec!["the new pack version (x)".into()] };
        let (t, bad) = pull_says(true, 0, &[], &game, 100);
        assert!(!bad && t.starts_with("Waiting for the game to close"), "{t}");
    }

    fn mark(by: &str, seq: u64) -> crate::groupsync::Mark {
        crate::groupsync::Mark { by: by.into(), seq, ..Default::default() }
    }

    fn member_copy(marks: &[(&str, &str, u64)], noah: Option<(u64, &[(&str, u64)])>) -> PackCopy {
        let mut c = PackCopy::default();
        for (k, by, seq) in marks {
            c.marks.insert((*k).to_string(), mark(by, *seq));
        }
        if let Some((seq, stamps)) = noah {
            let mut g = Got { seq, name: "Noah".into(), ..Default::default() };
            for (k, s) in stamps {
                g.stamps.insert((*k).to_string(), crate::groupsync::Stamp { seq: *s, by: "mia".into() });
            }
            c.got.insert("noah".into(), g);
        }
        c
    }

    #[test]
    fn a_member_with_every_change_is_in_step() {
        let c = member_copy(&[("p:1", "mia", 12), ("p:2", "me", 15)], Some((20, &[("p:1", 12), ("p:2", 15)])));
        assert_eq!(member_status(&[&c], "noah"), Member::InStep);
        assert_eq!(Member::InStep.label(), "In step");
    }

    #[test]
    fn a_member_missing_changes_is_behind_by_how_many() {
        // Noah's last code is older than the change to p:2, and has p:1 at an older number.
        let c = member_copy(&[("p:1", "mia", 12), ("p:2", "me", 25), ("p:3", "mia", 26)], Some((20, &[("p:1", 8)])));
        let m = member_status(&[&c], "noah");
        assert_eq!(m, Member::Behind { changes: 3 });
        assert_eq!(m.label(), "Behind by 3");
        assert!(m.detail("Noah").contains("waiting for the game"), "{}", m.detail("Noah"));
    }

    #[test]
    fn a_mod_the_member_has_no_stamp_for_counts_only_when_it_is_newer_than_their_code() {
        // Their code came after the change and carries no stamp: they keep it their way.
        let c = member_copy(&[("p:1", "mia", 12)], Some((20, &[])));
        assert_eq!(member_status(&[&c], "noah"), Member::InStep);
    }

    #[test]
    fn a_change_the_member_made_is_never_theirs_to_catch_up_on() {
        let c = member_copy(&[("p:1", "noah", 30)], Some((20, &[])));
        assert_eq!(member_status(&[&c], "noah"), Member::InStep);
    }

    #[test]
    fn a_member_not_heard_from_has_no_status() {
        let c = member_copy(&[("p:1", "mia", 12)], None);
        assert_eq!(member_status(&[&c], "noah"), Member::Unknown);
        assert_eq!(member_status(&[], "noah"), Member::Unknown);
        assert_eq!(Member::Unknown.label(), "");
    }

    #[test]
    fn a_code_made_in_the_message_itself_is_dated_by_the_message() {
        // Stamp seq 0 = made in this very code (seq 40): newer than the mark.
        let c = member_copy(&[("p:1", "mia", 30)], Some((40, &[("p:1", 0)])));
        assert_eq!(member_status(&[&c], "noah"), Member::InStep);
    }

    #[test]
    fn behind_adds_up_over_a_groups_packs() {
        let a = member_copy(&[("p:1", "mia", 50)], Some((20, &[])));
        let b = member_copy(&[("p:7", "mia", 60), ("p:8", "mia", 61)], Some((20, &[])));
        assert_eq!(member_status(&[&a, &b], "noah"), Member::Behind { changes: 3 });
    }
}
