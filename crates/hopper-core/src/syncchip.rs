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
    /// An arrow round a circle: restart to load what changed.
    Restart,
    /// A lower-case "i": notes for the player's information.
    Note,
    /// A clock: a start that makes no progress.
    Stuck,
}

/// Which side the chip is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The pack and its friends group.
    Group,
    /// The pack and its server.
    Server,
    /// Files the pack waits for from the CurseForge website.
    Downloads,
    /// The game or server runs with what it had when it started.
    Restart,
    /// Every notice no other chip says ("Needs you").
    Attention,
    /// The game or server itself has a problem: its start looks stuck, or it
    /// crashed. It sits right after the run state's own pill.
    Run,
}

impl Source {
    /// A sync chip (the group's or the server's): two of them can become
    /// one "Sync" chip. The others stand on their own.
    pub fn is_sync(self) -> bool {
        matches!(self, Source::Group | Source::Server)
    }
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

// ---- The seven states of a friends group (design/sync-rewrite-ui.md) -----------------

/// Where a pack stands with its friends group, worst first. [`group_look`] is
/// the only place that words it: the chip, its popover, the Friends page row
/// and the library dot all print what it returns, so they can't disagree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncState {
    /// Changes wait for the player's answer (`cards` of them).
    NeedsYou { cards: usize },
    /// The mods folder differs from what the group has (`mods` of them).
    Differs { mods: usize },
    /// This Hopper is too old for the group (`who`'s is newer).
    OutOfDate { who: String },
    /// The group's changes aren't all on this PC yet.
    Behind { changes: usize, from: Vec<String>, step: BehindStep },
    /// This PC's own changes are saved, but everyone who could still take them
    /// in is on an older Hopper (`who`: their names, blank when unknown). Nothing
    /// is in flight; it waits for them to update.
    WaitingForUpdate { who: Vec<String> },
    /// This PC's own changes are on their way (`held`: kept for Push).
    Sending { changes: usize, held: bool },
    /// Can't reach the group (`to_send` of this PC's changes wait).
    Offline { to_send: usize },
    /// The copy matches (`checked`: when the group was last looked at).
    InSync { checked: Option<u64> },
}

/// Why a copy is behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BehindStep {
    /// Changes are being taken in right now.
    Getting,
    /// They are known and wait for the next try (a download, a start).
    Waiting,
    /// They wait for the game to close.
    Game,
    /// Some didn't go in; Hopper tries again.
    Failed,
}

impl SyncState {
    /// 0 is the worst: the first row that is true wins in the chip table.
    pub fn rank(&self) -> u8 {
        match self {
            SyncState::NeedsYou { .. } => 0,
            SyncState::Differs { .. } => 1,
            SyncState::OutOfDate { .. } => 2,
            SyncState::Behind { .. } => 3,
            SyncState::WaitingForUpdate { .. } => 4,
            SyncState::Sending { .. } => 5,
            SyncState::Offline { .. } => 6,
            SyncState::InSync { .. } => 7,
        }
    }
}

/// What the screens know about a copy, to pick its [`SyncState`].
#[derive(Clone, Debug)]
pub struct SyncIn {
    /// The engine's status, asked without "out of date" counting
    /// (`Engine::status_counting(.., |_| false)`).
    pub status: crate::synccore::Status,
    /// The cards the player hasn't put aside.
    pub cards: usize,
    /// A friend's newer Hopper sends what this one can't read (their name).
    pub too_old: Option<String>,
    /// Names of the group's friends whose Hopper is older (it can't read what
    /// this one sends, so they never take this PC's changes in). Blank for a
    /// friend with no name yet.
    pub friends_old: Vec<String>,
    /// A friend on a current Hopper hasn't taken in all of this PC's changes
    /// yet (or hasn't been heard from): the changes can still be acked, so
    /// "Sending" stays true.
    pub current_unacked: bool,
    pub connected: bool,
    /// Changes are being taken in right now.
    pub applying: bool,
    /// The game runs and holds changes back.
    pub game: bool,
    /// Changes failed and Hopper tries again.
    pub failing: bool,
    /// Whose changes are coming in.
    pub from: Vec<String>,
    /// Sends are held for Push.
    pub push_by_hand: bool,
    pub checked: Option<u64>,
}

/// The state of a copy: the first true row of the table, worst first.
pub fn sync_state(i: &SyncIn) -> SyncState {
    use crate::synccore::Status as S;
    match &i.status {
        S::NeedsYou(_) if i.cards > 0 => return SyncState::NeedsYou { cards: i.cards },
        S::Differs(n) => return SyncState::Differs { mods: *n },
        _ => {}
    }
    if let Some(who) = &i.too_old {
        return SyncState::OutOfDate { who: who.clone() };
    }
    match &i.status {
        S::Behind(n) => {
            let step = if i.failing {
                BehindStep::Failed
            } else if i.game {
                BehindStep::Game
            } else if i.applying {
                BehindStep::Getting
            } else {
                BehindStep::Waiting
            };
            SyncState::Behind { changes: *n, from: i.from.clone(), step }
        }
        S::Sending(n) if !i.connected => SyncState::Offline { to_send: *n },
        // Held for Push is the player's to press, whoever is old. Otherwise,
        // when nobody who could still ack is on a current Hopper, nothing is
        // in flight: say what it waits for.
        S::Sending(_) if !i.push_by_hand && !i.friends_old.is_empty() && !i.current_unacked => {
            SyncState::WaitingForUpdate { who: i.friends_old.clone() }
        }
        S::Sending(n) => SyncState::Sending { changes: *n, held: i.push_by_hand },
        _ if !i.connected => SyncState::Offline { to_send: 0 },
        _ => SyncState::InSync { checked: i.checked },
    }
}

/// What a state says, everywhere it is shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupLook {
    pub look: Look,
    /// The popover's heading, and the Friends page row's first line.
    pub title: String,
    /// The popover's line under it, and the Friends page row's second line.
    pub line: String,
    /// The popover's one filled button, when there is one.
    pub button: Option<String>,
}

/// The friends on an older Hopper, in a sentence's subject, and whether it
/// takes "are": "Alice", "Alice and Bob", "Alice, Bob and Cara"; "A friend"
/// when none has a name, and "Alice and 2 more" when only some do.
fn old_friends(who: &[String]) -> (String, bool) {
    let named: Vec<&str> = who.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    let unnamed = who.len() - named.len();
    let mut items: Vec<String> = named.iter().map(|s| s.to_string()).collect();
    if unnamed > 0 {
        items.push(match (named.is_empty(), unnamed) {
            (true, 1) => "A friend".to_string(),
            (true, n) => format!("{n} friends"),
            (false, n) => format!("{n} more"),
        });
    }
    let plural = who.len() > 1;
    let text = match items.as_slice() {
        [] => "A friend".to_string(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    };
    (text, plural)
}

fn changes_n(n: usize) -> String {
    if n == 1 { "1 change".to_string() } else { format!("{n} changes") }
}

/// The one function that words a sync state (`now`: unix seconds, for "checked
/// 2 min ago").
pub fn group_look(state: &SyncState, now: u64) -> GroupLook {
    use Glyph::*;
    let mk = |tone, glyph, label: String, short: &str| Look::new(Source::Group, tone, glyph, label, short);
    let (look, title, line, button) = match state {
        SyncState::NeedsYou { cards } => (
            mk(Tone::Warn, Alert, format!("Needs you · {cards}"), "Needs you").strip(),
            if *cards == 1 { "A change waits for your answer".to_string() } else { format!("{cards} changes wait for your answer") },
            "Hopper settles it when you answer.".to_string(),
            None,
        ),
        SyncState::Differs { mods } => (
            mk(Tone::Warn, Differ, format!("This PC differs · {mods}"), "Differs").strip(),
            if *mods == 1 { "1 mod here differs from the group's".to_string() } else { format!("{mods} mods here differ from the group's") },
            "They changed outside Hopper. Only you see this.".to_string(),
            Some("Put all back".to_string()),
        ),
        SyncState::OutOfDate { who } => {
            let who = if who.trim().is_empty() { "A friend's".to_string() } else { format!("{}'s", who.trim()) };
            (
                mk(Tone::Bad, Alert, "Update Hopper to sync".to_string(), "Out of date").fixed(),
                "This Hopper is too old for the group".to_string(),
                format!("{who} Hopper is newer. Update Hopper to keep up with the group."),
                Some("Update Hopper".to_string()),
            )
        }
        SyncState::Behind { changes, from, step } => {
            let by = match from.as_slice() {
                [] => String::new(),
                [one] => format!(" from {one}"),
                [a, b] => format!(" from {a} and {b}"),
                many => format!(" from {} people", many.len()),
            };
            match step {
                BehindStep::Getting => (
                    mk(Tone::Busy, Spin, "Getting changes".to_string(), "Getting…").busy(),
                    format!("Getting {}{by}", changes_n(*changes)),
                    "They go in one after the other.".to_string(),
                    Some("Pull now".to_string()),
                ),
                BehindStep::Waiting => (
                    mk(Tone::Warn, Behind, format!("Behind · {}", changes_n(*changes)), "Behind"),
                    format!("{} to take in{by}", changes_n(*changes)),
                    "Hopper takes them in by itself.".to_string(),
                    Some("Pull now".to_string()),
                ),
                BehindStep::Game => (
                    mk(Tone::Warn, Behind, "Behind · close the game".to_string(), "Behind"),
                    "Waiting for the game to close".to_string(),
                    format!("{} land when it closes.", changes_n(*changes)),
                    None,
                ),
                BehindStep::Failed => (
                    mk(Tone::Warn, Alert, format!("Behind · {} didn't go in", changes_n(*changes)), "Behind").fixed(),
                    format!("{} of the group's didn't go in", changes_n(*changes)),
                    "Hopper tries again by itself.".to_string(),
                    Some("Try again".to_string()),
                ),
            }
        }
        SyncState::WaitingForUpdate { who } => {
            let (names, plural) = old_friends(who);
            (
                // Quiet and still: nothing is in flight, so no spinner.
                mk(Tone::Quiet, Behind, "Waiting for friends to update".to_string(), "Waiting"),
                "Friends need to update Hopper".to_string(),
                format!(
                    "{names} {} on an older Hopper. Your changes are saved and reach them once they update.",
                    if plural { "are" } else { "is" },
                ),
                None,
            )
        }
        SyncState::Sending { changes, held: false } => (
            mk(Tone::Busy, Up, format!("Sending {}", changes_n(*changes)), "Sending").busy(),
            "Sending your changes".to_string(),
            "Your friends get them in a moment.".to_string(),
            None,
        ),
        SyncState::Sending { changes, held: true } => (
            mk(Tone::Warn, Up, format!("Not sent · {}", changes_n(*changes)), "Not sent").strip(),
            format!("{} wait for Push", changes_n(*changes)),
            "They go out when you press Push.".to_string(),
            Some(crate::syncmode::push_label(*changes)),
        ),
        SyncState::Offline { to_send } => (
            mk(Tone::Quiet, Offline, if *to_send > 0 { format!("Offline · {to_send} to send") } else { "Offline".to_string() }, "Offline"),
            "Can't reach your friends".to_string(),
            if *to_send > 0 {
                "Your changes wait here and go out when you're back online. What this PC knows may be old.".to_string()
            } else {
                "What this PC knows may be old.".to_string()
            },
            Some("Try again".to_string()),
        ),
        SyncState::InSync { checked } => (
            mk(Tone::Good, Check, "In sync".to_string(), "In sync"),
            "Your copy matches the group's".to_string(),
            match checked {
                Some(at) if *at > 0 && now >= *at => format!("Nothing new to take in. Checked {}.", crate::groupstatus::ago(now, *at)),
                _ => "Nothing new to take in.".to_string(),
            },
            Some("Check now".to_string()),
        ),
    };
    GroupLook { look, title, line, button }
}

/// The chip for the group states that aren't a sync state (a mod the pack
/// needs, files only the website gives, the old statuses). Every sync state
/// is [`group_look`]'s. `pulling`: Pull now (or a repair)
/// is running; `pull_failed`: the last Pull now said it didn't work;
/// `left_out`: mods wait for the player's yes to their source.
pub fn status_look(status: &Status, pulling: bool, pull_failed: bool, left_out: bool) -> Look {
    use Glyph::*;
    let g = |tone, glyph, label: &str, short: &str| Look::new(Source::Group, tone, glyph, label, short);
    if pull_failed {
        return g(Tone::Bad, Alert, "Sync failed", "Failed").fixed();
    }
    let look = match status {
        // Beside the server's "In sync with <server>": say who this one is with.
        Status::InStep { .. } => g(Tone::Good, Check, "In sync with friends", "In sync"),
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
    /// The server runs another NeoForge than the pack: it would turn the
    /// game away at the door. Needs the player, so it never folds.
    WontJoin,
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
        ServerState::WontJoin => s(Tone::Bad, Alert, "Won't let you join".to_string(), "Won't join").fixed(),
    }
}

/// Where the files a pack waits for stand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadState {
    /// `files` wait for the player: their authors only allow the website.
    Waiting { files: usize },
    /// Hopper's download window works on them: `done` of `total` are in.
    Working { done: usize, total: usize },
    /// Some didn't come; the window is closed.
    Stuck,
}

/// The Downloads chip: files from the CurseForge website. Busy while the
/// window works (the app adds a line along the chip's foot); amber and
/// fixed while the player has to act.
pub fn download_look(state: &DownloadState) -> Look {
    use Glyph::*;
    let d = |tone, glyph, label: String, short: &str| Look::new(Source::Downloads, tone, glyph, label, short);
    match state {
        DownloadState::Waiting { files } => {
            let label = if *files == 1 { "1 file to download".to_string() } else { format!("{files} files to download") };
            d(Tone::Warn, Down, label, "To download").fixed()
        }
        DownloadState::Working { done, total } => {
            let label = if *total > 1 { format!("Downloading {} of {total}", (done + 1).min(*total)) } else { "Downloading…".to_string() };
            d(Tone::Busy, Spin, label, "Downloading…").busy()
        }
        DownloadState::Stuck => d(Tone::Warn, Alert, "Download didn't finish".to_string(), "Failed").fixed(),
    }
}

/// The Restart chip while the game (or the server) runs: what changed since
/// it started loads only after a restart. `changed`: mods or packs changed
/// under it (the chip asks for a restart); else only old files and switches
/// wait for the game to close (a quiet "waiting"). `what` is "game" or
/// "server". It needs nothing now, so it may fold.
pub fn restart_look(changed: bool, what: &str) -> Look {
    use Glyph::*;
    let r = |tone, glyph, label: String, short: &str| Look::new(Source::Restart, tone, glyph, label, short);
    if changed {
        r(Tone::Busy, Restart, "Restart to load".to_string(), "Restart")
    } else {
        r(Tone::Quiet, Behind, format!("Waiting for the {what} to close"), "Waiting")
    }
}

/// How much a notice matters (the page's notice tiers, most first).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Need {
    /// The game or server won't start or load until it's fixed.
    Stop,
    /// The player has to decide something.
    Choice,
    /// For the player's information; nothing breaks.
    Info,
}

/// The longest title the chip shows in full; a longer one is cut with "…"
/// (its full text is in the popover and the tooltip).
const ATTENTION_TITLE: usize = 28;

/// The "Needs you" chip: every notice the page has that no other chip says.
/// `needs` are their tiers, `title` the first one's (the most important). One
/// notice shows its own title ("2 mods in twice"), more say "N need you"; the
/// colour is the worst tier's (red for a problem that stops play, amber for a
/// choice). Only notes left make a quiet grey "N notes" that may fold. `None`
/// when there is nothing to say.
pub fn attention_look(needs: &[Need], title: &str) -> Option<Look> {
    use Glyph::*;
    let worst = needs.iter().copied().min()?;
    let n = needs.len();
    if worst == Need::Info {
        let label = if n == 1 { "1 note".to_string() } else { format!("{n} notes") };
        return Some(Look::new(Source::Attention, Tone::Quiet, Note, label.clone(), label));
    }
    let (tone, short) = if worst == Need::Stop { (Tone::Bad, "Stops play") } else { (Tone::Warn, "Needs you") };
    let label = if n == 1 {
        let t = title.trim();
        if t.chars().count() > ATTENTION_TITLE {
            let cut: String = t.chars().take(ATTENTION_TITLE - 1).collect();
            format!("{}…", cut.trim_end())
        } else {
            t.to_string()
        }
    } else {
        format!("{n} need you")
    };
    Some(Look::new(Source::Attention, tone, Alert, label, if n == 1 { short } else { "Needs you" }).fixed())
}

/// What is wrong with the game or server that runs (or ran).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunProblem {
    /// The start makes no progress; `minutes` is how long, when known.
    Stuck { minutes: Option<u32> },
    /// It stopped with a crash.
    Crashed,
    /// The paired server stopped with a crash (said on its pack's page).
    ServerCrashed,
}

/// The whole minutes in a stuck report's headline ("... (no progress for 4
/// min, CPU idle)"); `None` when it only counts seconds or doesn't say.
pub fn stuck_minutes(headline: &str) -> Option<u32> {
    let rest = headline.split("no progress for ").nth(1)?;
    let mut words = rest.split_whitespace();
    let n: u32 = words.next()?.parse().ok()?;
    words.next()?.trim_end_matches([',', ')']).eq("min").then_some(n)
}

/// The chip next to the run state pill while the game or server has a
/// problem: red, never folds, its notice (with Restart, See why) in the
/// popover.
pub fn run_look(problem: &RunProblem) -> Look {
    use Glyph::*;
    match problem {
        RunProblem::Stuck { minutes } => {
            let label = match minutes {
                Some(m) if *m >= 1 => format!("Looks stuck · {m} min"),
                _ => "Looks stuck".to_string(),
            };
            Look::new(Source::Run, Tone::Bad, Stuck, label, "Stuck").fixed()
        }
        RunProblem::Crashed => Look::new(Source::Run, Tone::Bad, Alert, "Crashed", "Crashed").fixed(),
        RunProblem::ServerCrashed => Look::new(Source::Run, Tone::Bad, Alert, "Server crashed", "Crashed").fixed(),
    }
}

/// A problem marked on the mod's own row in the mods list: the mod that needs
/// something that isn't installed, the one the loader refuses, a pack mod the
/// paired server lacks. A click on the marker opens the popover of the chip
/// that lists the problem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowProblem {
    /// Needs these mods (names), which aren't installed.
    Needs(Vec<String>),
    /// The loader refuses it, or the last crash stopped at it.
    StopsLoading,
    /// In the pack, not on this server.
    NotOnServer(String),
}

impl RowProblem {
    /// The marker's words: "Needs Sophisticated Core", "Needs Create +1".
    pub fn label(&self) -> String {
        match self {
            RowProblem::Needs(names) => match names.as_slice() {
                [] => "Needs a mod".to_string(),
                [one] => format!("Needs {one}"),
                [first, rest @ ..] => format!("Needs {first} +{}", rest.len()),
            },
            RowProblem::StopsLoading => "Stops loading".to_string(),
            RowProblem::NotOnServer(server) => format!("Not on {server}"),
        }
    }

    /// Red for what stops play, amber for a difference the player may want.
    pub fn tone(&self) -> Tone {
        match self {
            RowProblem::Needs(_) | RowProblem::StopsLoading => Tone::Bad,
            RowProblem::NotOnServer(_) => Tone::Warn,
        }
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
    /// The same for the stand-alone chips (Downloads, ...), in order.
    pub extra_forms: Vec<Option<Form>>,
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
    max_level_with(fixed, &[], plain)
}

/// [`max_level`] with chips that stand alone beside the sync chips
/// (`extra[i]`: chip i never folds): they shorten and become icons with the
/// others, and fold into "+N" with them, but are never merged.
pub fn max_level_with(fixed: &[bool], extra: &[bool], plain: usize) -> usize {
    if fixed.is_empty() && extra.is_empty() {
        return plain;
    }
    let icon_at = if fixed.len() >= 2 { 3 } else { 2 };
    // The chips fold together, and only when none needs the player.
    let foldable = usize::from(!fixed.iter().chain(extra).any(|f| *f));
    icon_at + foldable + plain
}

pub fn plan(fixed: &[bool], plain: usize, level: usize) -> Plan {
    plan_with(fixed, &[], plain, level)
}

/// [`plan`] with chips that stand alone (see [`max_level_with`]).
pub fn plan_with(fixed: &[bool], extra: &[bool], plain: usize, level: usize) -> Plan {
    let n = fixed.len();
    let m = extra.len();
    let max = max_level_with(fixed, extra, plain);
    let level = level.min(max);
    let two = n >= 2;
    let icon_at = if two { 3 } else { 2 };
    let any_fixed = fixed.iter().chain(extra).any(|f| *f);
    let mut p = Plan { forms: vec![Some(Form::Full); n], extra_forms: vec![Some(Form::Full); m], merged: None, plain_folded: 0, chips_folded: 0 };
    if n == 0 && m == 0 {
        p.plain_folded = level.min(plain);
        return p;
    }
    let form = match level {
        0 => Form::Full,
        1 => Form::Short,
        _ => Form::Icon,
    };
    p.extra_forms = vec![Some(form); m];
    if n == 0 {
        // Only stand-alone chips.
    } else if two {
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
        if steps > 0 && !any_fixed {
            steps -= 1;
            p.merged = None;
            p.forms.iter_mut().for_each(|f| *f = None);
            p.extra_forms.iter_mut().for_each(|f| *f = None);
            p.chips_folded = n + m;
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
        let l = status_look(&st, false, false, false);
        assert_eq!(l.label, "Updating with Mia's changes");
        assert_eq!(l.short, "Updating…");
        assert!(l.busy && !l.fixed && !l.strip);
        let two = Status::Updating { changes: 0, from: vec![s("Mia"), s("Noah")] };
        assert_eq!(status_look(&two, false, false, false).label, "Updating with Mia and Noah's changes");
        let many = Status::Updating { changes: 0, from: vec![s("A"), s("B"), s("C")] };
        assert_eq!(status_look(&many, false, false, false).label, "Updating with 3 people's changes");
        let none = Status::Updating { changes: 0, from: vec![] };
        assert_eq!(status_look(&none, false, false, false).label, "Updating with the group's changes");
    }

    #[test]
    fn rest_and_busy_states() {
        let ok = status_look(&Status::InStep { checked: None }, false, false, false);
        assert_eq!((ok.tone, ok.label.as_str(), ok.busy, ok.fixed), (Tone::Good, "In sync with friends", false, false));
        assert_eq!(ok.short, "In sync");
        let chk = status_look(&Status::Checking, false, false, false);
        assert_eq!((chk.tone, chk.busy), (Tone::Busy, true));
        // Pull now running makes any state busy, and keeps its words.
        let pull = status_look(&Status::InStep { checked: None }, true, false, false);
        assert!(pull.busy);
        assert_eq!(pull.label, "In sync with friends");
    }

    #[test]
    fn needs_you_and_failed_never_fold() {
        for st in [
            Status::ByHand { mods: vec![s("A"), s("B")] },
            Status::ToPush { changes: 2, after: None },
            Status::WaitsForYou { title: s("Pack 8.6") },
            Status::Behind { mods: 3, missing: false },
        ] {
            let l = status_look(&st, false, false, false);
            assert!(l.fixed && l.strip, "{st:?}");
        }
        let failed = status_look(&Status::Cant { why: s("x"), again: false, mods: vec![] }, false, false, false);
        assert!(failed.fixed && !failed.strip);
        assert_eq!((failed.tone, failed.label.as_str()), (Tone::Bad, "Sync failed"));
        // The same failure with mods waiting for a yes is a choice: it keeps its strip.
        let choice = status_look(&Status::Cant { why: s("x"), again: false, mods: vec![] }, false, false, true);
        assert!(choice.strip && choice.label == "Needs you");
        let bad_pull = status_look(&Status::InStep { checked: None }, false, true, false);
        assert!(bad_pull.fixed && bad_pull.tone == Tone::Bad);
    }

    #[test]
    fn behind_offline_and_check_failed_get_no_strip() {
        for st in [Status::Offline { checked: None }, Status::CantCheck, Status::WaitsForGame { what: vec![] }] {
            assert!(!status_look(&st, false, false, false).strip, "{st:?}");
        }
        assert_eq!(status_look(&Status::ByHand { mods: vec![s("A")] }, false, false, false).label, "1 mod to download");
        assert_eq!(status_look(&Status::ByHand { mods: vec![s("A"), s("B")] }, false, false, false).label, "2 mods to download");
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
    fn a_server_that_refuses_the_pack_is_red_and_never_folds() {
        let l = server_look(&ServerState::WontJoin, "ATM10 Server");
        assert_eq!((l.label.as_str(), l.short.as_str(), l.tone), ("Won't let you join", "Won't join", Tone::Bad));
        assert!(l.fixed && !l.busy && !l.strip);
        // Whatever the server's name, the words are the same.
        assert_eq!(server_look(&ServerState::WontJoin, "").label, "Won't let you join");
        // Beside a quiet group chip the merged chip keeps it: it is fixed.
        let g = status_look(&Status::InStep { checked: None }, false, false, false);
        let m = merged(&g, &l);
        assert!(m.fixed);
        assert_eq!(m.tone, Tone::Bad);
        assert_eq!(plan(&[false, true], 0, 99).merged, Some(Form::Icon));
    }

    #[test]
    fn restart_words() {
        let r = restart_look(true, "game");
        assert_eq!((r.label.as_str(), r.short.as_str(), r.tone, r.source), ("Restart to load", "Restart", Tone::Busy, Source::Restart));
        // It asks for nothing urgent: not busy, and it may fold.
        assert!(!r.busy && !r.fixed && !r.strip);
        let w = restart_look(false, "game");
        assert_eq!((w.label.as_str(), w.tone), ("Waiting for the game to close", Tone::Quiet));
        assert_eq!(restart_look(false, "server").label, "Waiting for the server to close");
        // It stands alone beside the sync chips, never merged with them.
        assert!(!r.source.is_sync());
        // And it folds with the others when there is no room.
        let p = plan_with(&[false], &[false], 0, 3);
        assert_eq!((p.forms, p.extra_forms), (vec![None], vec![None]));
    }

    #[test]
    fn needs_you_words_tone_and_folding() {
        assert_eq!(attention_look(&[], ""), None);
        // One notice says its own title; a problem that stops play is red and never folds.
        let one = attention_look(&[Need::Stop], "2 mods in twice").unwrap();
        assert_eq!((one.label.as_str(), one.short.as_str(), one.tone, one.source), ("2 mods in twice", "Stops play", Tone::Bad, Source::Attention));
        assert!(one.fixed && !one.busy && !one.strip && !one.source.is_sync());
        // A choice is amber and never folds either.
        let choice = attention_look(&[Need::Info, Need::Choice], "x").unwrap();
        assert_eq!((choice.label.as_str(), choice.short.as_str(), choice.tone), ("2 need you", "Needs you", Tone::Warn));
        assert!(choice.fixed);
        // Many: the worst tier sets the colour.
        let many = attention_look(&[Need::Info, Need::Stop, Need::Choice], "x").unwrap();
        assert_eq!((many.label.as_str(), many.tone), ("3 need you", Tone::Bad));
        // Only notes: quiet grey, and it may fold.
        let notes = attention_look(&[Need::Info, Need::Info], "x").unwrap();
        assert_eq!((notes.label.as_str(), notes.tone, notes.fixed, notes.glyph), ("2 notes", Tone::Quiet, false, Glyph::Note));
        assert_eq!(attention_look(&[Need::Info], "x").unwrap().label, "1 note");
        // A long title is cut, not left to push the row.
        let long = attention_look(&[Need::Choice], "Create 6.0.6 needs Flywheel 1.0.0 or newer to load").unwrap();
        assert!(long.label.chars().count() <= ATTENTION_TITLE && long.label.ends_with('…'), "{}", long.label);
        // Beside a sync chip: a fixed one keeps every chip; a quiet one folds with them.
        assert_eq!(plan_with(&[false], &[one.fixed], 0, 9).extra_forms, vec![Some(Form::Icon)]);
        assert_eq!(plan_with(&[false], &[notes.fixed], 0, 3).extra_forms, vec![None]);
    }

    #[test]
    fn downloads_words_and_folds() {
        let w = download_look(&DownloadState::Waiting { files: 3 });
        assert_eq!((w.label.as_str(), w.short.as_str(), w.tone), ("3 files to download", "To download", Tone::Warn));
        assert!(w.fixed && !w.busy && w.source == Source::Downloads);
        assert_eq!(download_look(&DownloadState::Waiting { files: 1 }).label, "1 file to download");
        let r = download_look(&DownloadState::Working { done: 1, total: 3 });
        assert_eq!(r.label, "Downloading 2 of 3");
        assert!(r.busy && !r.fixed);
        // The count never runs past the total while the last one finishes.
        assert_eq!(download_look(&DownloadState::Working { done: 3, total: 3 }).label, "Downloading 3 of 3");
        assert_eq!(download_look(&DownloadState::Working { done: 0, total: 1 }).label, "Downloading…");
        assert!(download_look(&DownloadState::Stuck).fixed);
        // Alone, a busy chip shortens, becomes an icon, then folds.
        let p = plan_with(&[], &[false], 0, 0);
        assert_eq!(p.extra_forms, vec![Some(Form::Full)]);
        assert_eq!(plan_with(&[], &[false], 0, 1).extra_forms, vec![Some(Form::Short)]);
        assert_eq!(plan_with(&[], &[false], 0, 2).extra_forms, vec![Some(Form::Icon)]);
        let gone = plan_with(&[], &[false], 0, 3);
        assert_eq!((gone.extra_forms, gone.chips_folded), (vec![None], 1));
        // One that needs the player never folds, and nothing else does for it.
        assert_eq!(max_level_with(&[], &[true], 0), 2);
        assert_eq!(plan_with(&[], &[true], 0, 9).extra_forms, vec![Some(Form::Icon)]);
        // Beside a sync chip: they fold together, last of all.
        assert_eq!(max_level_with(&[false], &[false], 0), 3);
        let both = plan_with(&[false], &[false], 0, 3);
        assert_eq!((both.forms, both.extra_forms, both.chips_folded), (vec![None], vec![None], 2));
        // A fixed one keeps them all.
        assert_eq!(plan_with(&[false], &[true], 0, 9).forms, vec![Some(Form::Icon)]);
        // Without stand-alone chips nothing changes.
        assert_eq!(plan(&[false, true], 0, 0), plan_with(&[false, true], &[], 0, 0));
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
        let a = status_look(&Status::InStep { checked: None }, false, false, false);
        let b = server_look(&ServerState::Differ { mods: 2, blocked: false }, "S");
        let m = merged(&a, &b);
        assert_eq!((m.tone, m.glyph, m.label.as_str(), m.fixed), (Tone::Warn, Glyph::Differ, "Sync", false));
        let bad = server_look(&ServerState::Failed, "");
        assert!(merged(&a, &bad).fixed);
        assert_eq!(merged(&bad, &a).tone, Tone::Bad);
    }

    #[test]
    fn run_problem_words_tone_and_folding() {
        let stuck = run_look(&RunProblem::Stuck { minutes: Some(4) });
        assert_eq!((stuck.label.as_str(), stuck.short.as_str(), stuck.tone, stuck.glyph, stuck.source), ("Looks stuck · 4 min", "Stuck", Tone::Bad, Glyph::Stuck, Source::Run));
        assert!(stuck.fixed && !stuck.busy && !stuck.strip && !stuck.source.is_sync());
        // Under a minute, or not known: no number.
        assert_eq!(run_look(&RunProblem::Stuck { minutes: Some(0) }).label, "Looks stuck");
        assert_eq!(run_look(&RunProblem::Stuck { minutes: None }).label, "Looks stuck");
        let crashed = run_look(&RunProblem::Crashed);
        assert_eq!((crashed.label.as_str(), crashed.tone, crashed.fixed), ("Crashed", Tone::Bad, true));
        assert_eq!(run_look(&RunProblem::ServerCrashed).label, "Server crashed");
        // It never folds, even with no room.
        assert_eq!(plan_with(&[false], &[stuck.fixed], 0, 3).extra_forms, vec![Some(Form::Icon)]);
    }

    #[test]
    fn row_problem_words_and_tone() {
        assert_eq!(RowProblem::Needs(vec!["Sophisticated Core".into()]).label(), "Needs Sophisticated Core");
        assert_eq!(RowProblem::Needs(vec!["Create".into(), "Flywheel".into(), "Jade".into()]).label(), "Needs Create +2");
        assert_eq!(RowProblem::Needs(Vec::new()).label(), "Needs a mod");
        assert_eq!(RowProblem::StopsLoading.label(), "Stops loading");
        assert_eq!(RowProblem::NotOnServer("TNP-LL8".into()).label(), "Not on TNP-LL8");
        assert_eq!(RowProblem::StopsLoading.tone(), Tone::Bad);
        assert_eq!(RowProblem::Needs(vec!["A".into()]).tone(), Tone::Bad);
        assert_eq!(RowProblem::NotOnServer("S".into()).tone(), Tone::Warn);
    }

    #[test]
    fn stuck_minutes_from_the_headline() {
        assert_eq!(stuck_minutes("TNP looks stuck while loading mods (no progress for 4 min, CPU idle)"), Some(4));
        assert_eq!(stuck_minutes("looks stuck while loading mods (no progress for 45 s, CPU idle)"), None);
        assert_eq!(stuck_minutes("looks stuck while loading mods"), None);
    }

    #[test]
    fn no_chips_only_plain_pills_fold() {
        let p = plan(&[], 3, 2);
        assert_eq!(p.plain_folded, 2);
    }
}

#[cfg(test)]
mod sync_state_tests {
    use super::*;
    use crate::synccore::Status as S;

    fn input(status: S) -> SyncIn {
        SyncIn {
            status,
            cards: 0,
            too_old: None,
            friends_old: vec![],
            current_unacked: false,
            connected: true,
            applying: false,
            game: false,
            failing: false,
            from: vec![],
            push_by_hand: false,
            checked: None,
        }
    }

    #[test]
    fn worst_state_wins() {
        // Needs you beats This PC differs beats Out of date beats Behind...
        let mut i = input(S::NeedsYou(2));
        i.cards = 2;
        i.too_old = Some("Mia".into());
        assert_eq!(sync_state(&i), SyncState::NeedsYou { cards: 2 });
        assert_eq!(sync_state(&input(S::Differs(3))), SyncState::Differs { mods: 3 });
        let mut i = input(S::Behind(4));
        i.too_old = Some("Mia".into());
        assert_eq!(sync_state(&i).rank(), 2);
        let mut i = input(S::Sending(1));
        i.connected = false;
        assert_eq!(sync_state(&i), SyncState::Offline { to_send: 1 });
        assert_eq!(sync_state(&input(S::InSync)).rank(), 7);
    }

    #[test]
    fn hidden_cards_dont_hold_the_chip() {
        // Every card put aside: the status still says NeedsYou, the chip doesn't.
        let i = input(S::NeedsYou(1));
        assert_eq!(sync_state(&i), SyncState::InSync { checked: None });
    }

    #[test]
    fn behind_says_why() {
        let mut i = input(S::Behind(2));
        i.from = vec!["Mia".into()];
        let at = |f: &dyn Fn(&mut SyncIn)| {
            let mut j = i.clone();
            f(&mut j);
            group_look(&sync_state(&j), 0)
        };
        assert_eq!(at(&|j| j.applying = true).look.label, "Getting changes");
        assert!(at(&|j| j.applying = true).look.busy);
        assert_eq!(at(&|_| {}).look.label, "Behind · 2 changes");
        assert_eq!(at(&|j| j.game = true).look.label, "Behind · close the game");
        assert!(at(&|j| j.failing = true).look.fixed);
        assert_eq!(at(&|_| {}).title, "2 changes to take in from Mia");
    }

    #[test]
    fn no_state_says_syncing() {
        let states = [
            SyncState::NeedsYou { cards: 2 },
            SyncState::Differs { mods: 1 },
            SyncState::OutOfDate { who: "Mia".into() },
            SyncState::Behind { changes: 1, from: vec![], step: BehindStep::Getting },
            SyncState::Behind { changes: 1, from: vec![], step: BehindStep::Waiting },
            SyncState::Behind { changes: 1, from: vec![], step: BehindStep::Game },
            SyncState::Behind { changes: 1, from: vec![], step: BehindStep::Failed },
            SyncState::WaitingForUpdate { who: vec!["Mia".into()] },
            SyncState::WaitingForUpdate { who: vec![] },
            SyncState::Sending { changes: 3, held: false },
            SyncState::Sending { changes: 3, held: true },
            SyncState::Offline { to_send: 0 },
            SyncState::Offline { to_send: 2 },
            SyncState::InSync { checked: Some(5) },
        ];
        for s in &states {
            let l = group_look(s, 100);
            for text in [&l.look.label, &l.look.short, &l.title, &l.line] {
                assert!(!text.to_lowercase().contains("syncing"), "{s:?}: {text}");
            }
        }
    }

    #[test]
    fn only_needs_you_and_differs_keep_the_strip() {
        assert!(group_look(&SyncState::NeedsYou { cards: 1 }, 0).look.strip);
        assert!(group_look(&SyncState::Differs { mods: 1 }, 0).look.strip);
        assert!(!group_look(&SyncState::InSync { checked: None }, 0).look.strip);
    }

    #[test]
    fn in_sync_says_when_it_checked() {
        let l = group_look(&SyncState::InSync { checked: Some(100) }, 220);
        assert_eq!(l.line, "Nothing new to take in. Checked 2 min ago.");
        let l = group_look(&SyncState::InSync { checked: None }, 220);
        assert_eq!(l.line, "Nothing new to take in.");
    }

    fn old(names: &[&str]) -> SyncIn {
        let mut i = input(S::Sending(4));
        i.friends_old = names.iter().map(|n| n.to_string()).collect();
        i
    }

    #[test]
    fn all_friends_on_an_older_hopper_wait_for_them() {
        let st = sync_state(&old(&["Alice", "Bob", "Cara"]));
        assert_eq!(st, SyncState::WaitingForUpdate { who: vec!["Alice".into(), "Bob".into(), "Cara".into()] });
        // Between Behind and Sending in the table.
        assert!(st.rank() > SyncState::Behind { changes: 1, from: vec![], step: BehindStep::Waiting }.rank());
        assert!(st.rank() < SyncState::Sending { changes: 1, held: false }.rank());
        let l = group_look(&st, 0);
        assert_eq!(l.look.label, "Waiting for friends to update");
        assert_eq!(l.title, "Friends need to update Hopper");
        assert_eq!(l.line, "Alice, Bob and Cara are on an older Hopper. Your changes are saved and reach them once they update.");
        // Nothing is in flight: no spinner, a quiet tone, nothing the player must do.
        assert_eq!(l.look.tone, Tone::Quiet);
        assert!(!l.look.busy && !l.look.fixed && !l.look.strip);
        assert_eq!(l.button, None);
    }

    #[test]
    fn waiting_for_update_words_count_the_friends() {
        let line = |who: &[&str]| {
            let who = who.iter().map(|n| n.to_string()).collect();
            group_look(&SyncState::WaitingForUpdate { who }, 0).line
        };
        assert_eq!(line(&["Alice"]), "Alice is on an older Hopper. Your changes are saved and reach them once they update.");
        assert_eq!(line(&["Alice", "Bob"]), "Alice and Bob are on an older Hopper. Your changes are saved and reach them once they update.");
        // No name known yet.
        assert_eq!(line(&[""]), "A friend is on an older Hopper. Your changes are saved and reach them once they update.");
        assert_eq!(line(&["", ""]), "2 friends are on an older Hopper. Your changes are saved and reach them once they update.");
        assert_eq!(line(&["Alice", ""]), "Alice and 1 more are on an older Hopper. Your changes are saved and reach them once they update.");
    }

    #[test]
    fn a_current_friend_who_has_not_acked_keeps_sending() {
        let mut i = old(&["Alice"]);
        i.current_unacked = true;
        assert_eq!(sync_state(&i), SyncState::Sending { changes: 4, held: false });
    }

    #[test]
    fn offline_beats_waiting_for_update() {
        let mut i = old(&["Alice"]);
        i.connected = false;
        assert_eq!(sync_state(&i), SyncState::Offline { to_send: 4 });
    }

    #[test]
    fn no_old_friends_known_stays_sending() {
        assert_eq!(sync_state(&old(&[])), SyncState::Sending { changes: 4, held: false });
    }

    #[test]
    fn changes_held_for_push_stay_not_sent() {
        let mut i = old(&["Alice"]);
        i.push_by_hand = true;
        assert_eq!(sync_state(&i), SyncState::Sending { changes: 4, held: true });
    }

    #[test]
    fn old_friends_who_are_not_the_cause_do_not_nag() {
        // The rest acked everything: the engine says in sync, and the older
        // friend doesn't change that.
        let mut i = input(S::InSync);
        i.friends_old = vec!["Alice".into()];
        assert_eq!(sync_state(&i), SyncState::InSync { checked: None });
    }

    #[test]
    fn friends_out_of_date_hopper_names_them() {
        let l = group_look(&SyncState::OutOfDate { who: "Mia".into() }, 0);
        assert_eq!(l.line, "Mia's Hopper is newer. Update Hopper to keep up with the group.");
    }
}
