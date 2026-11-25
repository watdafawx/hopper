//! Made-up sync chips for `--dialog sync-chip --state sy-*`: the seven states
//! of the group chip, with the real cards, "This PC differs" rows and away
//! notes inside, built from the pure structs in `hopper::synccore::cards`
//! and `hopper::syncchip::group_look`. No friend, no group, no relay.

use crate::sync_cards::{Env, ModInfo, name_of_file};
use crate::sync_chip::{Action, Chip, Run, Section};
use gpui_kit::assets::IconName;
use crate::sync_put_back::Span;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::{WindowExt as _, v_flex};
use gpui_kit::*;
use hopper::synccore::cards::{Btn, Card, CardMod, DriftRow, UndoPlan, UndoRow, What, drift_row};
use hopper::synccore::model::{Key, Value};
use hopper::synccore::{Answer, Drift, Risk};
use hopper::syncchip::{BehindStep, SyncState, group_look};
use std::rc::Rc;

/// What every `sy-*` name shows, for `sync_chip::PREVIEWS`.
pub const NAMES: &[&str] = &[
    "sy-needs", "sy-needs-one", "sy-needs-big", "sy-differs", "sy-differs-big", "sy-ood", "sy-behind", "sy-getting", "sy-game", "sy-failed", "sy-sending", "sy-held",
    "sy-offline", "sy-insync", "sy-notes",
];

/// The jar names the cards use; the long one tests wrapping.
const LONG: &str = "Additional-Placements-Extra-Long-Mod-Name-Reloaded-Edition-1.20.1-forge-4.2.0.jar";
const JARS: [&str; 12] = [
    "jade-1.2.jar", "create-6.0.4.jar", "sodium-0.5.jar", "journeymap-5.9.jar", "farmers-delight-1.2.jar", LONG, "waystones-14.1.jar", "jei-15.3.jar", "appleskin-2.5.jar",
    "xaeros-minimap-24.2.jar", "ftb-quests-2001.jar", "ae2-15.0.jar",
];

fn env() -> Env {
    Env {
        info: Rc::new(|file: &str, _: &App| ModInfo { name: name_of_file(file), icon: None, project: 0 }),
        peek: Rc::new(|_, _, _| {}),
        answer: Rc::new(|_, _, _, _, _| {}),
        put_back: Rc::new(|_, _, _| {}),
        share: Rc::new(|_, _, _| {}),
    }
}

fn mods(n: usize, what: What) -> Vec<CardMod> {
    JARS.iter().take(n).map(|f| CardMod { key: Key::Mod(f.to_string()), file: f.to_string(), what }).collect()
}

fn btn(label: &str, answer: Option<Answer>, lead: bool) -> Btn {
    Btn { label: label.into(), answer, lead, needs_tick: false, ticked: None }
}

fn card(id: u64, risk: Risk, who: &str, headline: &str, stays: &str, mods: Vec<CardMod>, buttons: Vec<Btn>) -> Card {
    let removes = mods.iter().filter(|m| matches!(m.what, What::Removed | What::TurnedOff)).count();
    Card { id, risk, who: who.into(), who_id: who.to_lowercase(), headline: headline.into(), stays: stays.into(), mods, removes, tick: None, others: 0, buttons, latest: id }
}

fn many_removals(id: u64, who: &str, n: usize) -> Card {
    let mut c = card(
        id,
        Risk::ManyRemovals { count: n },
        who,
        &format!("{who} removed {n} mods at once."),
        "They stay on this PC until you answer.",
        mods(n, What::Removed),
        vec![
            btn("Keep them", Some(Answer::Keep), true),
            Btn { label: "Remove them".into(), answer: Some(Answer::Take), lead: false, needs_tick: n > 5, ticked: (n > 5).then(|| format!("Remove {n} mods")) },
        ],
    );
    c.tick = (n > 5).then(|| format!("I looked at the list. Remove all {n} from this PC too."));
    c
}

/// The six kinds of card, in the order the chip ranks them.
pub fn cards(name: &str) -> Vec<Card> {
    let clash = card(
        1,
        Risk::Clash,
        "Mia",
        "Mia removed {mod}.",
        "Noah updated it too. It stays on for everyone until someone answers.",
        vec![CardMod { key: Key::Mod("jade".into()), file: "jade-1.2.jar".into(), what: What::Removed }],
        vec![btn("Keep {mod}", Some(Answer::Keep), true), btn("Remove it", Some(Answer::Take), false)],
    );
    let away = card(
        3,
        Risk::LongAway { secs: 3 * 86_400 },
        "Ava",
        "Ava was away for 3 days. They made 3 changes then.",
        "They hadn't seen the newest changes here, so they wait for your answer.",
        mods(3, What::Updated),
        vec![btn("Take them", Some(Answer::Take), true), btn("Keep ours", Some(Answer::Keep), false)],
    );
    let ask = card(
        4,
        Risk::AskFirst,
        "Noah",
        "Noah changed 2 mods.",
        "You asked to check Noah's changes first.",
        mods(2, What::Updated),
        vec![btn("Take them", Some(Answer::Take), true), btn("Keep ours", Some(Answer::Keep), false)],
    );
    let back = card(
        5,
        Risk::PackBack,
        "Mia",
        "Mia moved the pack back to 5.9.1.",
        "Moving back changes mods here. History can undo it.",
        mods(2, What::Other),
        vec![btn("Stay on 6.0.4", None, true), btn("Move back too", Some(Answer::Take), false)],
    );
    let source = card(
        6,
        Risk::NewSource,
        "Noah",
        "Noah added {mod} from a link.",
        "Hopper installs from a new place only when you allow it.",
        vec![CardMod { key: Key::Mod("tweaks".into()), file: "tweaks-1.0.jar".into(), what: What::Added }],
        vec![btn("Allow and install", Some(Answer::Take), true), btn("Not now", None, false)],
    );
    match name {
        "sy-needs-one" => vec![clash],
        "sy-needs-big" => {
            let mut a = many_removals(2, "Mia", 12);
            a.others = 1;
            vec![a, many_removals(7, "Noah", 6)]
        }
        _ => vec![clash, many_removals(2, "Mia", 12), away, ask, back, source],
    }
}

fn fil(file: &str, on: bool) -> Value {
    Value::File { file: file.into(), on }
}

/// Rows of "This PC differs": missing, turned off, another file, and a jar
/// added outside Hopper.
pub fn rows(big: bool) -> Vec<DriftRow> {
    let d = |key: &str, expected: Value, found: Value| drift_row(Drift { key: Key::Mod(key.into()), expected, found });
    let mut v = vec![
        d("jade", fil("jade-1.2.jar", true), Value::Gone),
        d("create", fil("create-6.0.4.jar", true), fil("create-6.0.4.jar", false)),
        d("sodium", fil("sodium-0.5.jar", true), fil("sodium-0.6.jar", true)),
        d("tweaks", Value::Gone, fil("tweaks-1.0.jar", true)),
    ];
    if big {
        for (i, f) in JARS.iter().enumerate().skip(4) {
            v.push(d(&format!("m{i}"), fil(f, true), Value::Gone));
        }
    }
    v
}

fn noop() -> Run {
    Rc::new(|_, _| {})
}

/// The state a name stands for, and how many away notes sit under it.
fn state_of(name: &str) -> (SyncState, usize) {
    let cards = cards(name).len();
    match name {
        "sy-needs" | "sy-needs-one" | "sy-needs-big" => (SyncState::NeedsYou { cards }, 0),
        "sy-differs" => (SyncState::Differs { mods: rows(false).len() }, 0),
        "sy-differs-big" => (SyncState::Differs { mods: rows(true).len() }, 0),
        "sy-ood" => (SyncState::OutOfDate { who: "Mia".into() }, 0),
        "sy-behind" => (SyncState::Behind { changes: 3, from: vec!["Mia".into()], step: BehindStep::Waiting }, 0),
        "sy-getting" => (SyncState::Behind { changes: 3, from: vec!["Mia".into()], step: BehindStep::Getting }, 0),
        "sy-game" => (SyncState::Behind { changes: 3, from: vec!["Mia".into()], step: BehindStep::Game }, 0),
        "sy-failed" => (SyncState::Behind { changes: 2, from: vec!["Mia".into()], step: BehindStep::Failed }, 0),
        "sy-sending" => (SyncState::Sending { changes: 2, held: false }, 0),
        "sy-held" => (SyncState::Sending { changes: 2, held: true }, 0),
        "sy-offline" => (SyncState::Offline { to_send: 2 }, 0),
        "sy-notes" => (SyncState::InSync { checked: None }, 2),
        _ => (SyncState::InSync { checked: None }, 0),
    }
}

/// The group's chip for a `sy-*` name; `None` for any other.
pub fn chip(name: &str) -> Option<Chip> {
    if !NAMES.contains(&name) {
        return None;
    }
    let (state, notes) = state_of(name);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let gl = group_look(&state, now);
    let actions: Vec<Action> = gl
        .button
        .iter()
        .map(|label| Action { label: label.clone().into(), icon: Some(IconName::RefreshCw), primary: true, disabled: false, run: noop() })
        .collect();
    let body: Option<crate::sync_chip::Body> = match &state {
        SyncState::NeedsYou { .. } => {
            let cs = cards(name);
            Some(Rc::new(move |window, cx| crate::sync_cards::cards_view(cs.clone(), env(), window, cx)))
        }
        SyncState::Differs { .. } => {
            let rs = rows(name == "sy-differs-big");
            Some(Rc::new(move |window, cx| {
                let narrow = window.viewport_size().width < px(720.);
                crate::sync_cards::drift_view(rs.clone(), env(), narrow, window, cx)
            }))
        }
        _ => None,
    };
    let mut sections = vec![Section { title: gl.title.clone().into(), line: gl.line.clone().into(), progress: None, body, actions }];
    if notes > 0 {
        sections.push(Section {
            title: format!("{notes} notes").into(),
            line: "".into(),
            progress: None,
            body: Some(Rc::new(move |_, cx| {
                let mut list = v_flex().w_full().gap_1();
                for (ix, (who, n)) in [("Mia", 4usize), ("Noah", 1)].into_iter().take(notes).enumerate() {
                    list = list.child(crate::sync_cards::away_row(ix, who, n, Rc::new(|_, _| {}), Rc::new(|_, _| {}), cx));
                }
                list.into_any_element()
            })),
            actions: Vec::new(),
        });
    }
    let wide = matches!(state, SyncState::NeedsYou { .. } | SyncState::Differs { .. });
    Some(Chip { id: "group".into(), look: gl.look, progress: None, sections, auto_open: Vec::new(), width: wide.then_some(27.5) })
}

/// `--dialog put-back`: Mia's changes put back, with a plan for each span:
/// today two rows, since away twelve removals (the tick), this week none.
pub fn open_put_back(window: &mut Window, cx: &mut App) {
    let plan_of: Rc<dyn Fn(Span, &App) -> UndoPlan> = Rc::new(|span, _| match span {
        Span::Today => UndoPlan {
            rows: vec![
                UndoRow { key: Key::Mod("jade".into()), file: "jade-1.2.jar".into(), says: "removed it · comes back".into(), takes_away: false },
                UndoRow { key: Key::Mod("create".into()), file: "create-6.0.4.jar".into(), says: "updated it · goes back to 6.0.3".into(), takes_away: false },
            ],
            left: vec![(Key::Mod("sodium".into()), "Noah".into())],
            removes: 0,
        },
        Span::Away => UndoPlan {
            rows: JARS
                .iter()
                .enumerate()
                .map(|(i, f)| UndoRow { key: Key::Mod(format!("m{i}")), file: f.to_string(), says: "added it · goes away for everyone".into(), takes_away: true })
                .collect(),
            left: vec![(Key::Mod("sodium".into()), "Noah".into()), (Key::Mod("jei".into()), "Ava".into())],
            removes: 12,
        },
        Span::Week => UndoPlan::default(),
    });
    let env_of: Rc<dyn Fn() -> Env> = Rc::new(env);
    crate::sync_put_back::open_with("mia".into(), "Mia".into(), plan_of, env_of, Rc::new(|_, _| {}), window, cx);
}

/// `--dialog pack-friends`: the "Your friends" block of a pack update, in a
/// stand-in dialog (the real one needs a copy kept by a live group).
pub fn open_pack_friends(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, move |dialog, window, cx| {
        let state = window.use_keyed_state(ElementId::Name("pack-friends-clean".into()), cx, |_, _| false);
        let clean = *state.read(cx);
        let mia = if clean { "Mia: Their 3 changed settings files go back to the pack's. 2 mods stay off." } else { "Mia: Keeps their 3 changed settings files. 2 mods stay off." };
        let says = vec![mia.to_string(), "Noah: Waits until their game closes.".to_string()];
        let st = state.clone();
        let block = crate::pack_update::friends_view(
            says,
            vec!["Ava".into()],
            clean,
            move |v: &bool, _, cx| {
                let v = *v;
                st.update(cx, |s, cx| {
                    *s = v;
                    cx.notify();
                });
            },
            cx,
        );
        dialog
            .title("Update Skyblock Server")
            .w(rems(34.).to_pixels(window.rem_size()))
            .margin_top(rems(crate::widgets::DIALOG_TOP).to_pixels(window.rem_size()))
            .child(v_flex().w_full().gap_3().child(div().text_sm().child("Skyblock 6.0.4 replaces 6.0.3: 3 mods update.")).child(block))
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("pf-cancel").outline().label("Cancel")))
                    .child(DialogAction::new().child(Button::new("pf-go").primary().label(if clean { "Update for everyone" } else { "Update" }))),
            )
    });
}
