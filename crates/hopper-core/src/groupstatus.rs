//! How a group copy stands, in words a player can act on: the shared
//! status type (a chip's label, tone and detail line) and the sentences
//! Pull now and the notices say. The state itself is worked out by the
//! sync core (`synccore`, `syncchip`); see `design/group-sync-status.md`.

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
    /// The pack has a mod that needs another mod the pack lacks, so it
    /// can't start. Group sync can still be "in step": nobody added it.
    NeedsMod { needs: Vec<crate::groupdeps::Need> },
    /// Other members' changes wait for the player to pull them: the group
    /// takes changes only from its maker (`only`, their name).
    Parked { from: Vec<crate::syncmode::Parked>, only: String },
    /// Changes made here wait for the player to press Push (`after`: other
    /// members' changes of the same mods go in once they are out).
    ToPush { changes: usize, after: Option<After> },
    /// Other members changed mods this PC changed too and hasn't shared
    /// yet. Their changes go in once this PC's own are out
    /// (it is kept aside); `offline`: and that can't happen right now.
    AfterYours { after: After, offline: bool },
    /// Everything is taken in, but the mods folder doesn't have what the
    /// group has: `mods` of them (`missing`: all plainly not here). The check
    /// of the copy found it ([`crate::groupverify`]); "Make my copy match"
    /// fixes it.
    Behind { mods: usize, missing: bool },
    /// The check of the copy couldn't finish (a site didn't answer), so it
    /// can't say the copy matches.
    CantCheck,
    /// The copy is being compared with the group.
    Checking,
    /// Group mods whose authors only allow downloads from the CurseForge
    /// website wait for the player to download them by hand.
    ByHand { mods: Vec<String> },
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
            Status::InStep { .. } => "Same as group",
            Status::Updating { .. } => "Updating",
            Status::Sending => "Sending your changes",
            Status::WaitsForGame { .. } => "Behind: close the game",
            Status::WaitsForYou { .. } => "Behind: needs you",
            Status::Cant { .. } => "Can't update",
            Status::Offline { .. } => "Offline",
            Status::NeedsMod { needs } => crate::groupdeps::label(needs.len()),
            Status::Parked { .. } => "Changes to pull",
            Status::ToPush { .. } => "Not sent yet",
            Status::AfterYours { .. } => "Your change goes first",
            Status::Behind { missing: true, .. } => "Missing mods",
            Status::Behind { .. } => "Differs from group",
            Status::CantCheck => "Couldn't check",
            Status::Checking => "Checking",
            Status::ByHand { .. } => "Download by hand",
        }
    }

    pub fn tone(&self) -> Tone {
        match self {
            Status::InStep { .. } => Tone::Good,
            Status::Updating { .. } | Status::Sending | Status::ToPush { .. } | Status::Checking | Status::AfterYours { offline: false, .. } => Tone::Busy,
            Status::WaitsForGame { .. } | Status::WaitsForYou { .. } | Status::Parked { .. } | Status::Behind { .. } | Status::CantCheck | Status::ByHand { .. } => Tone::Warn,
            Status::Cant { .. } | Status::NeedsMod { .. } => Tone::Bad,
            Status::Offline { .. } | Status::AfterYours { offline: true, .. } => Tone::Quiet,
        }
    }

    /// Which to show first among several copies: the ones that need the
    /// player.
    pub fn rank(&self) -> u8 {
        match self {
            Status::Cant { .. } => 0,
            Status::NeedsMod { .. } | Status::Behind { .. } => 1,
            Status::WaitsForYou { .. } | Status::ByHand { .. } => 2,
            Status::WaitsForGame { .. } => 3,
            Status::Parked { .. } | Status::CantCheck => 4,
            Status::ToPush { .. } => 5,
            Status::Offline { .. } | Status::AfterYours { offline: true, .. } => 6,
            Status::Updating { .. } | Status::Sending | Status::Checking | Status::AfterYours { .. } => 7,
            Status::InStep { .. } => 8,
        }
    }

    /// The line under the label: what is going on and what happens next.
    pub fn detail(&self, now: u64) -> String {
        match self {
            Status::InStep { checked } => match checked {
                Some(at) => format!("Same as the group. Checked {}.", ago(now, *at)),
                None => "Same as the group.".to_string(),
            },
            Status::Behind { mods, missing } => behind_says(*mods, *missing),
            Status::CantCheck => CANT_CHECK.to_string(),
            Status::Checking => "Comparing your copy with the group.".to_string(),
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
            Status::NeedsMod { needs } => crate::groupdeps::detail(needs),
            Status::Parked { from, only } => parked_says(from, only),
            Status::ToPush { changes, after } => {
                let yours = match changes {
                    0 => "You changed this pack. The group gets your changes when you press Push.".to_string(),
                    n => format!("You made {}. The group gets them when you press Push.", crate::syncmode::changes_words(*n)),
                };
                // What waits for the push comes second, and short: the strip
                // cuts the line with an ellipsis first, and the old wording
                // (counts, then "the group gets them…", then this) lost the
                // whole sentence in the middle widths.
                match after {
                    Some(a) => format!("Push to share yours, then {}.", a.then_body()),
                    None => yours,
                }
            }
            Status::AfterYours { after, offline: false } => after.waiting_says(),
            Status::AfterYours { after, offline: true } => format!("{} Can't reach the group right now.", after.waiting_says()),
            Status::ByHand { mods } => by_hand_says(mods),
        }
    }

    /// The short title for the notice strip: "Changes from Mia", "Changes
    /// from 2 people"; the label otherwise.
    pub fn title(&self) -> String {
        match self {
            Status::Parked { from, .. } => {
                let names: Vec<String> = from.iter().map(|p| p.name.clone()).collect();
                format!("Changes from {}", people(&names))
            }
            Status::ByHand { mods } => match mods.as_slice() {
                [one] if !one.trim().is_empty() => format!("{} to download in your browser", one.trim()),
                many if !many.is_empty() => format!("{} mods to download in your browser", many.len()),
                _ => "Download by hand".to_string(),
            },
            other => other.label().to_string(),
        }
    }
}

/// Other members' changes that wait for this PC's own change of the same
/// mod to be shared, as the status tells them. A wait that ran out used to
/// drop such a change without a word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct After {
    /// The mods' names, each once ("" for one whose name isn't known).
    pub mods: Vec<String>,
    /// The members who changed them, each once.
    pub by: Vec<String>,
    /// Every one of the changes brings another file (an update, an added
    /// mod): "Mia's version". A switch or a removal among them: "change".
    pub versions: bool,
}

impl After {
    /// "Sodium", "Sodium and Lithium", "3 mods"; "a mod" for one with no
    /// name.
    fn mods_say(&self) -> String {
        let named = self.mods.iter().all(|n| !n.is_empty());
        match self.mods.as_slice() {
            [one] if named => one.clone(),
            [_] => "a mod".to_string(),
            [a, b] if named => format!("{a} and {b}"),
            many => format!("{} mods", many.len()),
        }
    }

    /// "Mia's", "Mia and Noah's", "the group's".
    fn whose(&self) -> String {
        match self.by.as_slice() {
            [a] => format!("{a}'s"),
            [a, b] => format!("{a} and {b}'s"),
            _ => "the group's".to_string(),
        }
    }

    /// "version goes in", "changes go in".
    fn goes_in(&self) -> String {
        let noun = if self.versions { "version" } else { "change" };
        if self.mods.len() == 1 { format!("{noun} goes in") } else { format!("{noun}s go in") }
    }

    /// "Waiting for your change to Sodium to be shared, then Mia's version
    /// goes in."
    pub fn waiting_says(&self) -> String {
        let yours = if self.mods.len() == 1 { "change" } else { "changes" };
        format!("Waiting for your {yours} to {} to be shared, then {} {}.", self.mods_say(), self.whose(), self.goes_in())
    }

    /// After "The group gets them when you press Push.": "Then Mia's
    /// version of Sodium goes in."
    pub fn then_says(&self) -> String {
        format!("Then {}.", self.then_body())
    }

    /// [`then_says`] without the "Then" and the full stop, for a sentence
    /// that already leads into it: "Push to share yours, then Mia's version
    /// of Sodium goes in."
    pub fn then_body(&self) -> String {
        let of = if self.versions { "of" } else { "to" };
        let (noun, verb) = match (self.versions, self.mods.len() == 1) {
            (true, true) => ("version", "goes in"),
            (true, false) => ("versions", "go in"),
            (false, true) => ("change", "goes in"),
            (false, false) => ("changes", "go in"),
        };
        format!("{} {noun} {of} {} {verb}", self.whose(), self.mods_say())
    }
}

/// "Mia made 3 changes. You take changes only from Noah, so they wait
/// until you pull them." / "Mia made 3 changes, Ava 1. ..."
fn parked_says(from: &[crate::syncmode::Parked], only: &str) -> String {
    let what = |p: &crate::syncmode::Parked, first: bool| match p.changes {
        0 if first => format!("{} made changes", p.name),
        0 => format!("{} some", p.name),
        n if first => format!("{} made {}", p.name, crate::syncmode::changes_words(n)),
        n => format!("{} {n}", p.name),
    };
    let lead: Vec<String> = from.iter().take(3).enumerate().map(|(i, p)| what(p, i == 0)).collect();
    let more = from.len().saturating_sub(3);
    let lead = if more > 0 { format!("{} and {more} more", lead.join(", ")) } else { lead.join(", ") };
    let only = if only.is_empty() { "the group's maker".to_string() } else { only.to_string() };
    format!("{lead}. You take changes only from {only}, so they wait until you pull them.")
}


/// What the strip says when the check couldn't finish.
pub const CANT_CHECK: &str = "Hopper couldn't check CurseForge just now, so your copy might be missing something. Try again.";

/// "Your copy is missing 2 mods the group has." (`missing`: all of them are
/// plainly not here); "Your copy differs in 2 mods." when some are another
/// version or switched the other way. The second is short on purpose: the strip
/// cuts a long title with an ellipsis at the narrowest window, and the cut must
/// not take the word "mods" with it.
pub fn behind_says(mods: usize, missing: bool) -> String {
    let n = if mods == 1 { "1 mod".to_string() } else { format!("{mods} mods") };
    if missing { format!("Your copy is missing {n} the group has.") } else { format!("Your copy differs in {n}.") }
}

/// "Entity Culling can only be downloaded from the CurseForge website."
/// Long lists name the first three ("A, B, C and 2 more ...").
pub fn by_hand_says(mods: &[String]) -> String {
    let names: Vec<String> = mods.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    if names.is_empty() {
        return "Some mods can only be downloaded from the CurseForge website.".to_string();
    }
    const SHOWN: usize = 3;
    let mut list: Vec<String> = names.iter().take(SHOWN).cloned().collect();
    if names.len() > SHOWN {
        list.push(format!("{} more", names.len() - SHOWN));
    }
    format!("{} can only be downloaded from the CurseForge website.", join(&list))
}

/// Notice raised once when automatic group sync queues website-only mods for
/// `pack` (empty when the pack's name isn't known).
pub fn by_hand_notice(pack: &str, mods: &[String]) -> String {
    let them = if mods.len() == 1 { "it" } else { "them" };
    let base = by_hand_says(mods);
    let pack = pack.trim();
    if pack.is_empty() {
        format!("{base} Open the pack to download {them}.")
    } else {
        format!("{pack}: {base} Open the pack to download {them}.")
    }
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
        Status::InStep { .. } | Status::Sending | Status::Updating { .. } | Status::Checking if took.is_empty() => ("Already the same as the group.".to_string(), false),
        Status::InStep { .. } | Status::Sending | Status::Updating { .. } | Status::Checking => (format!("{took} Now the same as the group."), false),
        // Only this PC's own changes wait, to go out: the copy has the
        // group's.
        // (With a member's change kept aside for one of them, the copy
        // doesn't have it yet: the status's own line says so, below.)
        Status::ToPush { after: None, .. } if took.is_empty() => ("You have the group's latest changes. Yours go out when you press Push.".to_string(), false),
        Status::ToPush { after: None, .. } => (format!("{took} Yours go out when you press Push."), false),
        other => {
            let lead = if took.is_empty() { String::new() } else { format!("{took} ") };
            (format!("{lead}{}", other.detail(now)), matches!(other, Status::Cant { .. } | Status::CantCheck))
        }
    }
}

// ---- other members -------------------------------------------------------------------

/// Whether another member's PC has taken in the group's latest changes, as
/// far as this PC can tell from the last code they sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Member {
    /// Nothing from them about these packs: they don't keep a copy, or
    /// this PC hasn't heard from it yet. It gets a quiet chip of its own so
    /// the row never looks like "in step" by saying nothing.
    Unknown,
    InStep,
    /// `changes` changes the group made (mods, the pack version, the loader)
    /// their last message doesn't have yet.
    Behind { changes: usize },
}

impl Member {
    pub fn label(&self) -> String {
        match self {
            Member::Unknown => "Not heard from yet".to_string(),
            Member::InStep => "Same as group".to_string(),
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
            Member::Unknown => format!("Hopper hasn't heard from {name}'s copy of this pack yet, so it can't tell whether it is in step."),
            Member::InStep => format!("{name}'s copy has the group's latest changes."),
            Member::Behind { changes } => {
                let n = if *changes == 1 { "1 change".to_string() } else { format!("{changes} changes") };
                format!("{name}'s copy is missing {n} the group made. They may be offline, or waiting for the game to close.")
            }
        }
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

    #[test]
    fn people_and_times_read_plainly() {
        assert_eq!(people(&["Mia".into(), "Mia".into()]), "Mia");
        assert_eq!(people(&["a".into(), "b".into(), "c".into()]), "3 people");
        assert_eq!(ago(100, 100), "just now");
        assert_eq!(ago(100 + 3 * 3600, 100), "3 h ago");
        assert_eq!(ago(100 + 86400, 100), "1 day ago");
    }

    #[test]
    fn pull_now_says_what_it_did() {
        let mia = vec!["Mia".to_string()];
        let in_step = Status::InStep { checked: Some(100) };
        assert_eq!(pull_says(true, 0, &[], &in_step, 100), ("Already the same as the group.".to_string(), false));
        assert_eq!(pull_says(true, 3, &mia, &in_step, 100), ("Took in 3 changes from Mia. Now the same as the group.".to_string(), false));
        assert_eq!(pull_says(true, 1, &[], &in_step, 100).0, "Took in 1 change. Now the same as the group.");
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

    #[test]
    fn website_only_mods_say_by_hand_in_the_chip_and_pull_says() {
        let one = vec!["Entity Culling".to_string()];
        let s = Status::ByHand { mods: one.clone() };
        assert_eq!(s.label(), "Download by hand");
        assert_eq!(s.title(), "Entity Culling to download in your browser");
        assert_eq!(s.tone(), Tone::Warn);
        assert_eq!(s.detail(0), "Entity Culling can only be downloaded from the CurseForge website.");

        let (text, bad) = pull_says(true, 0, &[], &s, 0);
        assert!(!bad);
        assert_eq!(text, "Entity Culling can only be downloaded from the CurseForge website.");

        let mia = vec!["Mia".to_string()];
        let (text, bad) = pull_says(true, 2, &mia, &s, 0);
        assert!(!bad);
        assert_eq!(text, "Took in 2 changes from Mia. Entity Culling can only be downloaded from the CurseForge website.");

        let many: Vec<String> = ["Entity Culling", "Mod B", "Mod C", "Mod D"].into_iter().map(String::from).collect();
        let s_many = Status::ByHand { mods: many.clone() };
        assert_eq!(s_many.title(), "4 mods to download in your browser");
        assert_eq!(s_many.detail(0), "Entity Culling, Mod B, Mod C and 1 more can only be downloaded from the CurseForge website.");
        assert_eq!(
            by_hand_notice("Skyblock", &one),
            "Skyblock: Entity Culling can only be downloaded from the CurseForge website. Open the pack to download it."
        );
        assert_eq!(
            by_hand_notice("", &many[..2]),
            "Entity Culling and Mod B can only be downloaded from the CurseForge website. Open the pack to download them."
        );
    }

    #[test]
    fn what_needs_the_player_ranks_first() {
        let cant = Status::Cant { why: "x".into(), again: false, mods: vec![] };
        assert!(cant.rank() < Status::Behind { mods: 1, missing: false }.rank());
        assert!(Status::Behind { mods: 1, missing: false }.rank() < Status::InStep { checked: None }.rank());
    }

    #[test]
    fn a_change_kept_aside_is_said_with_whose_and_what() {
        let a = After { mods: vec!["Sodium".into()], by: vec!["Mia".into()], versions: true };
        assert_eq!(a.waiting_says(), "Waiting for your change to Sodium to be shared, then Mia's version goes in.");
        assert_eq!(a.then_says(), "Then Mia's version of Sodium goes in.");
        let b = After { mods: vec!["Sodium".into(), "Lithium".into()], by: vec!["Mia".into(), "Noah".into()], versions: false };
        assert_eq!(b.waiting_says(), "Waiting for your changes to Sodium and Lithium to be shared, then Mia and Noah's changes go in.");
    }
}
