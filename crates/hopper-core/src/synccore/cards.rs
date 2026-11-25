//! The words and numbers behind the friend-sync screens (design/sync-rewrite-ui.md):
//! the "Needs you" cards, the rows of "This PC differs", the plain-language
//! "Why" of a History row, and the rows of "Put back a friend's changes".
//!
//! Pure: every function reads an [`Engine`] and returns values, so the
//! wording, the order of the cards and the thresholds are unit tests, and a
//! preview can build the same structs without a friend.
//!
//! A mod's name is not in a record (only its key and file), so the texts here
//! carry `{mod}` where the screen puts the mod's icon and name as a link.

use super::engine::{Answer, Engine, Review};
use super::gate::Risk;
use super::local::{Drift, Snapshot};
use super::model::{Change, Key, Value, Why};
use std::collections::BTreeSet;

/// Where the screen puts a mod's icon and name in a text.
pub const MOD: &str = "{mod}";

/// What one record did to one mod, for a row of a card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum What {
    Updated,
    Removed,
    TurnedOff,
    TurnedOn,
    Added,
    Other,
}

impl What {
    /// "turned off", for the small grey text after the mod's name.
    pub fn says(&self) -> &'static str {
        match self {
            What::Updated => "updated",
            What::Removed => "removed",
            What::TurnedOff => "turned off",
            What::TurnedOn => "turned on",
            What::Added => "added",
            What::Other => "changed",
        }
    }
}

/// What a record did, from what the author had and what they made it.
pub fn what_of(c: &Change) -> What {
    match (&c.was, &c.now) {
        (_, Value::Gone | Value::PackRemoved) => What::Removed,
        (_, Value::PackOff | Value::File { on: false, .. }) => What::TurnedOff,
        (Value::PackOff | Value::File { on: false, .. }, Value::PackDefault | Value::File { on: true, .. }) => What::TurnedOn,
        (Value::Gone | Value::PackRemoved, Value::File { .. } | Value::PackDefault) => What::Added,
        (Value::File { file: a, .. }, Value::File { file: b, .. }) if a != b => What::Updated,
        _ => What::Other,
    }
}

/// The file a value is, when it is one.
fn file_of(v: &Value) -> &str {
    match v {
        Value::File { file, .. } => file,
        _ => "",
    }
}

/// One mod of a card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardMod {
    pub key: Key,
    /// The author's file (or the one it replaced): a name when the mod isn't
    /// known here.
    pub file: String,
    pub what: What,
}

/// One button of a card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Btn {
    pub label: String,
    /// `None`: only hides the card (it comes back when the friend sends
    /// something new, and on the next start).
    pub answer: Option<Answer>,
    /// The card's one filled button: the safe answer.
    pub lead: bool,
    /// Off until the tick is ticked.
    pub needs_tick: bool,
    /// The label once ticked ("Remove 12 mods").
    pub ticked: Option<String>,
}

/// One "Needs you" card: a friend and a batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    pub id: u64,
    pub risk: Risk,
    pub who: String,
    pub who_id: String,
    /// What happened, with [`MOD`] for the first mod when the card is about
    /// one mod.
    pub headline: String,
    /// What stays meanwhile, in grey.
    pub stays: String,
    pub mods: Vec<CardMod>,
    /// How many of them take a mod away or turn it off.
    pub removes: usize,
    /// "I looked at the list. Remove all 12 from this PC too.", when the
    /// answer that removes needs it.
    pub tick: Option<String>,
    /// How many other cards of the same kind a tick can answer together.
    pub others: usize,
    pub buttons: Vec<Btn>,
    /// The highest record number in it: a hidden card shows again when its
    /// friend sends something newer.
    pub latest: u64,
}

fn who_name(r: &Review) -> String {
    if r.who.trim().is_empty() { "A friend".to_string() } else { r.who.clone() }
}

fn count_mods(n: usize) -> String {
    if n == 1 { "1 mod".to_string() } else { format!("{n} mods") }
}

/// "3 days", or hours under a day.
pub fn span(secs: u64) -> String {
    let days = secs / 86_400;
    if days >= 1 {
        format!("{days} {}", if days == 1 { "day" } else { "days" })
    } else {
        let h = (secs / 3600).max(1);
        format!("{h} {}", if h == 1 { "hour" } else { "hours" })
    }
}

/// How a kind of card ranks: clashes, many removals, pack moves, "away",
/// new sources, "Ask me first".
fn rank(r: &Risk) -> u8 {
    match r {
        Risk::Clash => 0,
        Risk::ManyRemovals { .. } => 1,
        Risk::PackBack | Risk::OtherPack => 2,
        Risk::LongAway { .. } => 3,
        Risk::NewSource => 4,
        Risk::AskFirst => 5,
    }
}

fn version(v: &Value) -> Option<&str> {
    match v {
        Value::Text(t) if !t.is_empty() => Some(t),
        _ => None,
    }
}

/// The cards of the copy, in the order they show: clashes, many removals,
/// pack moves, "away", new sources, "Ask me first"; oldest first within
/// each. Same friend and batch is one card (the engine groups them).
pub fn cards(e: &Engine) -> Vec<Card> {
    let reviews = e.reviews();
    let many = e.limits.many_removals;
    let mut out: Vec<(u8, u64, Card)> = Vec::new();
    for r in &reviews {
        let changes: Vec<&Change> = r.changes.iter().filter_map(|d| e.log.changes.get(d).map(|(c, _)| c)).collect();
        let who = who_name(r);
        let who_id = r.changes.first().map(|d| d.author.clone()).unwrap_or_default();
        let mods: Vec<CardMod> = changes
            .iter()
            .filter(|c| matches!(c.key, Key::Mod(_)))
            .map(|c| CardMod {
                key: c.key.clone(),
                file: if file_of(&c.now).is_empty() { file_of(&c.was).to_string() } else { file_of(&c.now).to_string() },
                what: what_of(c),
            })
            .collect();
        let removes = changes.iter().filter(|c| matches!(c.key, Key::Mod(_)) && c.now.takes_away(&c.was)).count();
        let latest = r.changes.iter().map(|d| d.n).max().unwrap_or(0);
        let first_lamport = changes.iter().map(|c| c.lamport).min().unwrap_or(0);
        let others = reviews.iter().filter(|o| o.id != r.id && rank(&o.risk) == rank(&r.risk)).count();
        let tick_label = |n: usize| format!("I looked at the list. Remove all {n} from this PC too.");
        let big = removes > many;
        let btn = |label: &str, answer: Option<Answer>, lead: bool| Btn { label: label.to_string(), answer, lead, needs_tick: false, ticked: None };
        let (headline, stays, buttons, tick) = match &r.risk {
            Risk::Clash => {
                let winner = changes.first();
                let other = changes.get(1);
                let pack = r.keys.first() == Some(&Key::Pack);
                let off = other.is_some_and(|o| matches!(o.now, Value::File { on: false, .. } | Value::PackOff));
                let by_me = winner.is_some_and(|w| w.dot.author == e.me);
                let winner_name = if by_me { "You".to_string() } else { winner.map(|w| w.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| "Someone".to_string()) };
                if pack {
                    (
                        format!("{who} and {winner_name} moved the pack at the same time."),
                        "The newer version stays for everyone until someone answers.".to_string(),
                        vec![btn("Keep the newer one", Some(Answer::Keep), true), btn(&format!("Use {who}'s"), Some(Answer::Take), false)],
                        None,
                    )
                } else {
                    let verb = if off { "turned off" } else { "removed" };
                    (
                        format!("{who} {verb} {MOD}."),
                        format!("{winner_name} changed it too. It stays on for everyone until someone answers."),
                        vec![btn(&format!("Keep {MOD}"), Some(Answer::Keep), true), btn(if off { "Turn it off" } else { "Remove it" }, Some(Answer::Take), false)],
                        None,
                    )
                }
            }
            Risk::ManyRemovals { count } => {
                let n = (*count).max(removes);
                let take = Btn { label: "Remove them".into(), answer: Some(Answer::Take), lead: false, needs_tick: big, ticked: big.then(|| format!("Remove {}", count_mods(n))) };
                (
                    format!("{who} removed {} at once.", count_mods(n)),
                    "They stay on this PC until you answer.".to_string(),
                    vec![btn("Keep them", Some(Answer::Keep), true), take],
                    big.then(|| tick_label(n)),
                )
            }
            Risk::PackBack => {
                let to = changes.first().and_then(|c| version(&c.now)).map(str::to_string);
                let here = changes.first().and_then(|c| version(&c.was)).map(str::to_string);
                let stay = here.as_deref().map_or("Stay here".to_string(), |v| format!("Stay on {v}"));
                (
                    match &to {
                        Some(v) => format!("{who} moved the pack back to {v}."),
                        None => format!("{who} moved the pack back to an older version."),
                    },
                    "Moving back changes mods here. History can undo it.".to_string(),
                    vec![btn(&stay, None, true), btn("Move back too", Some(Answer::Take), false)],
                    None,
                )
            }
            Risk::OtherPack => (
                format!("{who} switched to another pack."),
                "Your copy of this pack stays as it is.".to_string(),
                vec![btn("Stay here", None, true), btn("Switch here too", Some(Answer::Take), false)],
                None,
            ),
            Risk::LongAway { secs } => {
                let n = r.changes.len();
                let take = Btn { label: "Take them".into(), answer: Some(Answer::Take), lead: true, needs_tick: big, ticked: big.then(|| format!("Remove {}", count_mods(removes))) };
                (
                    format!("{who} was away for {}. They made {} then.", span(*secs), if n == 1 { "1 change".to_string() } else { format!("{n} changes") }),
                    "They hadn't seen the newest changes here, so they wait for your answer.".to_string(),
                    vec![take, btn("Keep ours", Some(Answer::Keep), false)],
                    big.then(|| tick_label(removes)),
                )
            }
            Risk::NewSource => (
                if mods.len() == 1 { format!("{who} added {MOD} from a link.") } else { format!("{who} added {} from links.", mods.len()) },
                "Hopper installs from a new place only when you allow it.".to_string(),
                vec![btn("Allow and install", Some(Answer::Take), true), btn("Not now", None, false)],
                None,
            ),
            Risk::AskFirst => {
                let n = r.changes.len();
                let take = Btn { label: "Take them".into(), answer: Some(Answer::Take), lead: true, needs_tick: big, ticked: big.then(|| format!("Remove {}", count_mods(removes))) };
                (
                    format!("{who} changed {}.", if n == 1 { "1 mod".to_string() } else { format!("{n} mods") }),
                    format!("You asked to check {who}'s changes first."),
                    vec![take, btn("Keep ours", Some(Answer::Keep), false)],
                    big.then(|| tick_label(removes)),
                )
            }
        };
        out.push((rank(&r.risk), first_lamport, Card { id: r.id, risk: r.risk.clone(), who, who_id, headline, stays, mods, removes, tick, others, buttons, latest }));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    out.into_iter().map(|(_, _, c)| c).collect()
}

/// A card's button as it reads now: ticked cards say how many.
pub fn button_label(b: &Btn, ticked: bool) -> String {
    match (&b.ticked, ticked) {
        (Some(t), true) => t.clone(),
        _ => b.label.clone(),
    }
}

/// Whether a card's button can be pressed.
pub fn button_ready(b: &Btn, ticked: bool) -> bool {
    !b.needs_tick || ticked
}

// ---- This PC differs ----------------------------------------------------------

/// One row of "This PC differs".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriftRow {
    pub drift: Drift,
    /// The jar's file name, for a mod Hopper has no name for.
    pub file: String,
    /// "missing from the mods folder".
    pub reason: String,
    /// The one word for a narrow row.
    pub short: &'static str,
    /// A jar added outside Hopper, only here ("Take it out" / "Add for everyone").
    pub added: bool,
    pub put_back: &'static str,
    pub share: &'static str,
}

/// The rows of the copy's differences, in the order of the mods' keys.
pub fn drift_rows(e: &Engine, disk: &Snapshot) -> Vec<DriftRow> {
    e.drift(disk).into_iter().map(drift_row).collect()
}

pub fn drift_row(d: Drift) -> DriftRow {
    let file = match (&d.found, &d.expected) {
        (Value::File { file, .. }, _) | (_, Value::File { file, .. }) => file.clone(),
        _ => String::new(),
    };
    let (reason, short, added) = match (&d.expected, &d.found) {
        (Value::Gone | Value::PackRemoved, _) => ("added outside Hopper, only here".to_string(), "added", true),
        (_, Value::Gone) => ("missing from the mods folder".to_string(), "missing", false),
        (_, Value::File { on: false, .. } | Value::PackOff) => ("turned off outside Hopper".to_string(), "turned off", false),
        (_, Value::File { on: true, .. }) if d.expected.is_away() => ("turned on outside Hopper".to_string(), "turned on", false),
        (Value::File { .. }, Value::File { .. }) => ("another file here than the group's".to_string(), "other file", false),
        _ => ("differs from the group's".to_string(), "differs", false),
    };
    DriftRow { file, reason, short, added, put_back: if added { "Take it out" } else { "Put back" }, share: if added { "Add for everyone" } else { "Remove for everyone" }, drift: d }
}

/// "Remove Jade for everyone" / "Add it for everyone", the row's second
/// button once the tick is in; `name` is the mod's name.
pub fn share_label(row: &DriftRow, name: &str) -> String {
    if row.added { format!("Add {name} for everyone") } else { format!("Remove {name} for everyone") }
}

// ---- History's Why ---------------------------------------------------------------

/// The reason line under a History row, only when Hopper made the change for
/// someone (an answer, a pack update, a put back, a missing file); a change
/// the player made by hand has none. `None` for those.
pub fn why_line(e: &Engine, c: &Change) -> Option<String> {
    let name = |ch: &Change| if ch.dot.author == e.me { "You".to_string() } else if ch.name.is_empty() { "Someone".to_string() } else { ch.name.clone() };
    let whose = |ch: &Change| if ch.dot.author == e.me { "your".to_string() } else { format!("{}'s", name(ch)) };
    match &c.why {
        Why::Player => None,
        Why::Answer { .. } => {
            let other = e
                .log
                .changes
                .values()
                .map(|(o, _)| o)
                .filter(|o| o.key == c.key && o.dot.author != c.dot.author && o.lamport < c.lamport)
                .max_by_key(|o| o.lamport);
            Some(match other {
                Some(o) if o.now == c.now => format!("{} let {} change in when asked.", name(c), whose(o)),
                Some(o) => format!("{} kept it as it was after {} change, when asked.", name(c), whose(o)),
                None => format!("{} answered a question about it.", name(c)),
            })
        }
        Why::PackUpdate { mode } => Some(match mode.friends {
            super::model::ConfigMode::Keep => "It came with a pack update. Everyone kept their own settings files.".to_string(),
            super::model::ConfigMode::Clean => "It came with a pack update. Everyone got the pack's own settings files.".to_string(),
        }),
        Why::Undo { of } => {
            let of = e.log.changes.get(of).map(|(o, _)| o);
            Some(match of {
                Some(o) if o.dot.author == c.dot.author => "It undoes an earlier change of the same person.".to_string(),
                Some(o) => format!("It undoes {} change.", whose(o)),
                None => "It undoes an earlier change.".to_string(),
            })
        }
        Why::SharedDrift => Some(if matches!(c.now, Value::Gone | Value::PackRemoved) {
            format!("It was missing from {} mods folder, and {} chose Remove for everyone.", whose(c), name(c))
        } else {
            format!("{} added it outside Hopper and chose Add for everyone.", name(c))
        }),
    }
}

// ---- Put back a friend's changes ---------------------------------------------------

/// One row of the "Put back" dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UndoRow {
    pub key: Key,
    pub file: String,
    /// "removed it · comes back".
    pub says: String,
    pub takes_away: bool,
}

/// What putting back a friend's changes since `since` does.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UndoPlan {
    pub rows: Vec<UndoRow>,
    /// Keys someone changed after the friend: left alone, with who.
    pub left: Vec<(Key, String)>,
    /// How many of the rows take a mod away from everyone.
    pub removes: usize,
}

/// The rows of `Engine::undo_member_plan`, in words.
pub fn undo_plan(e: &Engine, member: &str, since: u64) -> UndoPlan {
    let (edits, left) = e.undo_member_plan(member, since);
    let name = e
        .log
        .changes
        .values()
        .map(|(c, _)| c)
        .find(|c| c.dot.author == member && !c.name.is_empty())
        .map_or_else(|| "They".to_string(), |c| c.name.clone());
    let mut rows = Vec::new();
    for ed in &edits {
        let now = e.value(&ed.key);
        let file = match (&ed.now, &now) {
            (Value::File { file, .. }, _) | (_, Value::File { file, .. }) => file.clone(),
            _ => String::new(),
        };
        let takes_away = matches!(ed.key, Key::Mod(_)) && ed.now.takes_away(&now);
        let says = match (&now, &ed.now) {
            (_, Value::Gone | Value::PackRemoved) => format!("{name} added it · goes away"),
            (Value::Gone | Value::PackRemoved, _) => format!("{name} removed it · comes back"),
            (n, t) if t.is_away() && !n.is_away() => format!("{name} turned it on · back off"),
            (n, t) if n.is_away() && !t.is_away() => format!("{name} turned it off · back on"),
            _ => format!("{name} changed it · back as it was"),
        };
        rows.push(UndoRow { key: ed.key.clone(), file, says, takes_away });
    }
    let removes = rows.iter().filter(|r| r.takes_away).count();
    let left: Vec<(Key, String)> = left
        .into_iter()
        .map(|k| {
            let by = e.last_change(&k).map(|c| c.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| "Someone".to_string());
            (k, by)
        })
        .collect();
    UndoPlan { rows, left, removes }
}

/// "Put back 5 changes" (the dialog's button) and its tick sentence.
pub fn undo_button(n: usize, removes: usize, many: usize) -> (String, Option<String>) {
    let label = if n == 1 { "Put back 1 change".to_string() } else { format!("Put back {n} changes") };
    let tick = (removes > many).then(|| format!("I looked at the list. Remove {removes} mods for everyone."));
    (label, tick)
}

/// Members (id, name) who have records in the log, for the Friends page and
/// the History filter.
pub fn authors(e: &Engine) -> BTreeSet<(String, String)> {
    e.log.changes.values().map(|(c, _)| (c.dot.author.clone(), c.name.clone())).filter(|(id, _)| *id != e.me).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synccore::model::{Arrival, Dot, Edit, Seen};
    use crate::synccore::wire::{Body, Packet, WIRE_VERSION};

    fn change(author: &str, n: u64, key: &str, was: Value, now: Value, batch: &str, online_at: u64) -> Change {
        Change {
            dot: Dot { author: author.to_string(), n },
            key: Key::Mod(key.to_string()),
            was,
            now,
            name: author.to_string(),
            lamport: n,
            seen: Seen::default(),
            online_at,
            batch: batch.to_string(),
            why: Why::Player,
        }
    }

    fn file(f: &str) -> Value {
        Value::File { file: f.to_string(), on: true }
    }

    /// An engine for "me" that took in `changes` from Noah at relay time `at`.
    fn engine_with(changes: Vec<Change>, at: u64) -> Engine {
        let mut e = Engine::new("c", "me", "Me");
        let mut copies = std::collections::BTreeMap::new();
        copies.insert("c".to_string(), (Body::Tail(changes), Default::default(), String::new()));
        let packet = Packet { version: WIRE_VERSION, from: "noah".into(), group: "g".into(), copies };
        e.receive(packet, Arrival { seq: 1, at });
        e
    }

    #[test]
    fn many_removals_need_a_tick_and_say_how_many() {
        let ch: Vec<Change> = (1..=12).map(|i| change("noah", i, &format!("cf:{i}"), file("a.jar"), Value::Gone, "b", 100)).collect();
        let e = engine_with(ch, 200);
        let cards = cards(&e);
        assert_eq!(cards.len(), 1);
        let c = &cards[0];
        assert_eq!(c.removes, 12);
        assert_eq!(c.headline, "noah removed 12 mods at once.");
        assert_eq!(c.tick.as_deref(), Some("I looked at the list. Remove all 12 from this PC too."));
        let take = &c.buttons[1];
        assert!(take.needs_tick && !button_ready(take, false) && button_ready(take, true));
        assert_eq!(button_label(take, false), "Remove them");
        assert_eq!(button_label(take, true), "Remove 12 mods");
        // The safe answer is the filled one.
        assert!(c.buttons[0].lead && !take.lead);
        assert_eq!(c.buttons[0].label, "Keep them");
    }

    #[test]
    fn five_removals_need_no_tick() {
        let ch: Vec<Change> = (1..=6).map(|i| change("noah", i, &format!("cf:{i}"), file("a.jar"), Value::Gone, "b", 100)).collect();
        let e = engine_with(ch, 200);
        // Six is more than five: a card with a tick; five is below it.
        assert!(cards(&e)[0].tick.is_some());
        let ch: Vec<Change> = (1..=5).map(|i| change("noah", i, &format!("cf:{i}"), file("a.jar"), Value::Gone, "b", 100)).collect();
        let e = engine_with(ch, 200);
        assert!(cards(&e).is_empty());
    }

    #[test]
    fn a_friend_away_for_days_gets_a_card_with_the_changes() {
        let ch = vec![
            change("noah", 1, "cf:1", file("a-1.jar"), file("a-2.jar"), "b", 1_000),
            change("noah", 2, "cf:2", file("b.jar"), Value::File { file: "b.jar".into(), on: false }, "b", 1_000),
        ];
        let e = engine_with(ch, 1_000 + 3 * 86_400 + 5);
        let c = &cards(&e)[0];
        assert_eq!(c.headline, "noah was away for 3 days. They made 2 changes then.");
        assert_eq!(c.mods.iter().map(|m| m.what.clone()).collect::<Vec<_>>(), vec![What::Updated, What::TurnedOff]);
        assert_eq!(c.buttons.iter().map(|b| b.label.as_str()).collect::<Vec<_>>(), vec!["Take them", "Keep ours"]);
        assert!(c.buttons[0].lead);
        assert_eq!(c.tick, None);
    }

    #[test]
    fn cards_come_clashes_first_then_by_kind() {
        assert!(rank(&Risk::Clash) < rank(&Risk::ManyRemovals { count: 9 }));
        assert!(rank(&Risk::ManyRemovals { count: 9 }) < rank(&Risk::PackBack));
        assert_eq!(rank(&Risk::PackBack), rank(&Risk::OtherPack));
        assert!(rank(&Risk::PackBack) < rank(&Risk::LongAway { secs: 1 }));
        assert!(rank(&Risk::LongAway { secs: 1 }) < rank(&Risk::NewSource));
        assert!(rank(&Risk::NewSource) < rank(&Risk::AskFirst));
    }

    #[test]
    fn span_says_days_or_hours() {
        assert_eq!(span(3 * 86_400 + 5), "3 days");
        assert_eq!(span(86_400), "1 day");
        assert_eq!(span(5 * 3600), "5 hours");
    }

    #[test]
    fn drift_rows_say_what_differs_and_name_the_buttons() {
        let gone = drift_row(Drift { key: Key::Mod("cf:1".into()), expected: file("a.jar"), found: Value::Gone });
        assert_eq!((gone.short, gone.reason.as_str(), gone.put_back, gone.share), ("missing", "missing from the mods folder", "Put back", "Remove for everyone"));
        let off = drift_row(Drift { key: Key::Mod("cf:2".into()), expected: file("b.jar"), found: Value::File { file: "b.jar".into(), on: false } });
        assert_eq!(off.short, "turned off");
        let added = drift_row(Drift { key: Key::Mod("src:j.jar".into()), expected: Value::Gone, found: file("j.jar") });
        assert!(added.added);
        assert_eq!((added.put_back, added.share, added.file.as_str()), ("Take it out", "Add for everyone", "j.jar"));
        assert_eq!(share_label(&added, "Jade"), "Add Jade for everyone");
        assert_eq!(share_label(&gone, "Jade"), "Remove Jade for everyone");
    }

    #[test]
    fn why_is_only_for_changes_hopper_made_for_someone() {
        let mut e = Engine::new("c", "me", "Me");
        let by_hand = Change { why: Why::Player, ..change("noah", 1, "cf:1", file("a.jar"), Value::Gone, "b", 0) };
        assert_eq!(why_line(&e, &by_hand), None);
        let drift = Change { why: Why::SharedDrift, ..by_hand.clone() };
        assert_eq!(why_line(&e, &drift).as_deref(), Some("It was missing from noah's mods folder, and noah chose Remove for everyone."));
        let pack = Change { why: Why::PackUpdate { mode: Default::default() }, ..by_hand.clone() };
        assert_eq!(why_line(&e, &pack).as_deref(), Some("It came with a pack update. Everyone kept their own settings files."));
        // An answer says whose change it answered.
        let theirs = change("noah", 2, "cf:9", file("s.jar"), Value::Gone, "x", 0);
        e.log.insert(theirs.clone(), None);
        let answer = Change { why: Why::Answer { review: 1 }, lamport: 9, dot: Dot { author: "me".into(), n: 1 }, name: "Me".into(), ..change("me", 1, "cf:9", Value::Gone, file("s.jar"), "y", 0) };
        assert_eq!(why_line(&e, &answer).as_deref(), Some("You kept it as it was after noah's change, when asked."));
        let edit = Edit { key: Key::Mod("cf:9".into()), now: Value::Gone, why: Why::Player, batch: String::new() };
        let _ = edit;
    }

    #[test]
    fn undo_button_asks_for_a_tick_over_five_removals() {
        assert_eq!(undo_button(5, 0, 5), ("Put back 5 changes".to_string(), None));
        assert_eq!(undo_button(1, 0, 5).0, "Put back 1 change");
        let (label, tick) = undo_button(9, 8, 5);
        assert_eq!(label, "Put back 9 changes");
        assert_eq!(tick.as_deref(), Some("I looked at the list. Remove 8 mods for everyone."));
    }
}
