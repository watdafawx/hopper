//! The group chip of the instance hero for a pack kept in step with a friends
//! group: whether this PC's copy has what the group has, and "Pull now"
//! (`hopper::groupstatus`, `hopper::syncchip`). The changes themselves are
//! taken in by the sync core (`hopper::synccore`); the notice here is only
//! for a pack that lacks a mod another one needs.

use crate::notice_slot::{self, Notice, Tier};
use crate::store::Store;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::{Disableable as _, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::*;
use hopper::groupstatus::Status;
use std::path::Path;
use std::rc::Rc;

/// The one-line status of a group copy at `root`: icon, tier and tooltip
/// that go with each state.
pub fn look(status: &Status) -> (IconName, Tier) {
    match status {
        Status::InStep { .. } => (IconName::CircleCheck, Tier::Info),
        Status::Updating { .. } | Status::Sending => (IconName::RefreshCw, Tier::Info),
        Status::WaitsForGame { .. } => (IconName::Clock, Tier::Info),
        Status::WaitsForYou { .. } => (IconName::CircleAlert, Tier::Choice),
        Status::Cant { .. } => (IconName::CircleAlert, Tier::Choice),
        Status::Offline { .. } => (IconName::CircleOff, Tier::Info),
        Status::NeedsMod { .. } => (IconName::PackageX, Tier::Stop),
        Status::Parked { .. } => (IconName::Users, Tier::Choice),
        Status::ToPush { .. } => (IconName::Upload, Tier::Info),
        Status::AfterYours { .. } => (IconName::Clock, Tier::Info),
        Status::Behind { .. } | Status::CantCheck => (IconName::CircleAlert, Tier::Choice),
        Status::Checking => (IconName::RefreshCw, Tier::Info),
        Status::ByHand { .. } => (IconName::Download, Tier::Choice),
    }
}

/// The "Pull now" button; icon only when the page is narrow. In step there
/// is nothing to pull, so it reads "Check now".
pub fn pull_button(store: &Entity<Store>, root: &Path, pulling: bool, in_step: bool, narrow: bool, id: &'static str) -> Button {
    let (store, root) = (store.clone(), root.to_path_buf());
    let b = Button::new(id)
        .small()
        .outline()
        .icon(IconName::RefreshCw)
        .disabled(pulling)
        .tooltip("Checks the group for newer changes and takes them in now");
    let b = if narrow { b } else { b.label(if pulling { "Checking..." } else if in_step { "Check now" } else { "Pull now" }) };
    b.on_click(move |_, _, cx| store.update(cx, |s, cx| s.pull_group_now(&root, cx)))
}

/// The sync chip of the hero for the group copy at `root`: how it stands,
/// and in its popover what that means and what can be done. `None` when
/// the instance isn't a group copy. The chip is the whole story for a
/// state that needs nothing (busy, behind, offline, failed); the one state
/// the sync core doesn't say, a required mod the pack lacks, puts a notice
/// ([`group_status_bar`]) in the popover, with its button.
pub fn group_chip(store: &Entity<Store>, root: &Path, cx: &mut App) -> Option<crate::sync_chip::Chip> {
    use crate::sync_chip::{Action, Chip, Section};
    let s = store.read(cx);
    let status = s.group_status(root)?;
    // Files only the website gives are the Downloads chip's.
    if matches!(status, Status::ByHand { .. }) {
        return None;
    }
    // A copy kept by the sync core says its state with the seven of
    // `hopper::syncchip::group_look`.
    if !matches!(status, Status::NeedsMod { .. })
        && let Some(state) = s.sync_state(root, s.sync_connected())
    {
        return Some(sync_group_chip(store, root, state, cx));
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let pulling = s.group_pulling(root);
    let pulled = s.group_pulled(root).map(|(t, bad)| (t.to_string(), bad));
    let pulled_bad = pulled.as_ref().is_some_and(|(_, bad)| *bad);
    let look = hopper::syncchip::status_look(&status, pulling, pulled_bad, false);
    if look.strip
        && let Some(notice) = group_status_bar(store, root, cx)
    {
        // The notice is asked for again whenever the popover draws, so its
        // buttons and lists are current. A problem that stops play opens it
        // once by itself.
        let auto_open = notice_slot::stop_ids(std::slice::from_ref(&notice));
        let (st, rt) = (store.clone(), root.to_path_buf());
        let producer: notice_slot::Producer = Rc::new(move |_, cx| group_status_bar(&st, &rt, cx).into_iter().collect());
        let body = notice_slot::body(&format!("group:{}", root.display()), producer);
        return Some(Chip {
            id: "group".into(),
            look,
            progress: None,
            sections: vec![Section { title: "".into(), line: "".into(), progress: None, body: Some(body), actions: Vec::new() }],
            auto_open,
            // As wide as the "Needs you" list: the rows hold mod names and a button.
            width: Some(32.),
        });
    }
    let in_step = matches!(status, Status::InStep { .. });
    let (title, line) = match (&status, &pulled) {
        (Status::InStep { .. }, Some((t, false))) => ("Your copy matches the group.".to_string(), t.clone()),
        (Status::InStep { .. }, _) => ("Your copy matches the group.".to_string(), in_step_line(&status.detail(now), false)),
        (_, Some((t, true))) => ("Couldn't take the changes in".to_string(), t.clone()),
        _ => (status.title(), status.detail(now)),
    };
    // Pull now is always there; in step it only checks.
    let (st, rt) = (store.clone(), root.to_path_buf());
    let label = if pulling { "Checking..." } else if in_step { "Check now" } else { "Pull now" };
    let pull = Action {
        label: label.into(),
        icon: Some(IconName::RefreshCw),
        primary: true,
        disabled: pulling,
        run: Rc::new(move |_, cx| st.update(cx, |s, cx| s.pull_group_now(&rt, cx))),
    };
    Some(Chip {
        id: "group".into(),
        look,
        progress: None,
        sections: vec![Section { title: title.into(), line: line.into(), progress: None, body: None, actions: vec![pull] }],
        auto_open: Vec::new(),
        width: None,
    })
}

/// The Env of the sync views for the copy at `root`: who a mod is, and what
/// each button does in the store.
pub fn sync_env(store: &Entity<Store>, root: &Path) -> crate::sync_cards::Env {
    use crate::sync_cards::{Env, ModInfo, name_of_file};
    use hopper::synccore::Answer;
    let (s1, r1) = (store.clone(), root.to_path_buf());
    let (s2, r2) = (store.clone(), root.to_path_buf());
    let (s3, r3) = (store.clone(), root.to_path_buf());
    let (s4, r4) = (store.clone(), root.to_path_buf());
    let (s5, r5) = (store.clone(), root.to_path_buf());
    Env {
        info: Rc::new(move |file: &str, cx: &App| {
            use hopper::synccore::{link::LinkRef, local::file_of};
            let s = s1.read(cx);
            let bare = file.trim_end_matches(".disabled");
            // Cards name a mod by its file id ("link:...", "cf:123"), so look
            // for it by id first, then by jar name, then in History (a mod
            // this PC already took out is only there).
            let a = s.instance(&r1).and_then(|i| {
                // A Modrinth mod's card id is "mr:<version>" (groupkey), but file_of
                // gives it a "cf:" id, so match that form too.
                i.addons.iter().find(|a| file_of(a) == bare || (!a.mr_version.is_empty() && bare.strip_prefix("mr:") == Some(a.mr_version.as_str()))).or_else(|| i.addons.iter().find(|a| a.file_name.trim_end_matches(".disabled").eq_ignore_ascii_case(bare)))
            });
            let past = if a.is_none() {
                std::fs::read_dir(r1.join(hopper::sync::BACKUP_DIR)).ok().and_then(|d| {
                    d.flatten().find_map(|e| hopper::actor::read_records(&e.path()).into_iter().map(|r| r.addon).find(|a| file_of(a) == bare))
                })
            } else {
                None
            };
            match a.or(past.as_ref()) {
                Some(a) => ModInfo { name: if a.name.is_empty() { name_of_file(&a.file_name) } else { a.name.clone() }, icon: s.image(&a.icon).map(Path::to_path_buf), project: a.project_id },
                None => {
                    // Nothing known on this PC: a link id still carries the
                    // jar's name; any other id gets a plain word.
                    let name = match LinkRef::parse(bare) {
                        Some(l) => name_of_file(&l.file_name),
                        None if bare.starts_with("cf:") || bare.starts_with("mr:") => "a mod".to_string(),
                        None => name_of_file(file),
                    };
                    ModInfo { name, icon: None, project: 0 }
                }
            }
        }),
        peek: Rc::new(move |project, _, cx| {
            let root = r2.clone();
            s2.update(cx, |_, cx| cx.emit(crate::store::StoreEvent::Peek { root, project }));
        }),
        answer: Rc::new(move |card, answer: Option<Answer>, others, _, cx| {
            let (root, card) = (r3.clone(), card.clone());
            s3.update(cx, |s, cx| match answer {
                Some(a) => s.sync_answer_card(&root, &card, a, others, cx),
                None => s.sync_hide_card(&root, &card, cx),
            });
        }),
        put_back: Rc::new(move |row, _, cx| {
            let (root, key) = (r4.clone(), row.drift.key.clone());
            s4.update(cx, |s, cx| s.sync_put_back_keys(&root, Some(vec![key]), cx));
        }),
        share: Rc::new(move |row, _, cx| {
            let (root, key) = (r5.clone(), row.drift.key.clone());
            s5.update(cx, |s, cx| s.sync_share_keys(&root, Some(vec![key]), cx));
        }),
    }
}

/// The chip of a copy kept by the sync core: the state's words come from
/// `group_look`; the popover holds the cards of "Needs you", the rows of
/// "This PC differs", the friends' notes, and the state's one button.
fn sync_group_chip(store: &Entity<Store>, root: &Path, state: hopper::syncchip::SyncState, cx: &mut App) -> crate::sync_chip::Chip {
    use crate::sync_chip::{Action, Chip, Section};
    use hopper::syncchip::SyncState;
    let s = store.read(cx);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let gl = hopper::syncchip::group_look(&state, now);
    let pulling = s.group_pulling(root);
    let pushing = s.pushing_at(root);
    let notes = s.sync_away_notes(root);
    let line = match s.group_pulled(root) {
        Some((t, _)) => t.to_string(),
        None => gl.line.clone(),
    };
    let mut actions: Vec<Action> = Vec::new();
    let (st, rt) = (store.clone(), root.to_path_buf());
    if let Some(label) = &gl.button {
        let run: crate::sync_chip::Run = match &state {
            SyncState::Differs { .. } => {
                let (st, rt) = (st.clone(), rt.clone());
                Rc::new(move |_, cx| st.update(cx, |s, cx| s.sync_put_back_keys(&rt, None, cx)))
            }
            SyncState::OutOfDate { .. } => {
                let st = st.clone();
                Rc::new(move |_, cx| {
                    st.update(cx, |s, cx| {
                        if matches!(s.self_update(), Some(crate::store::SelfUpdate::Available(_))) {
                            s.install_self_update();
                        } else {
                            s.check_self_update();
                        }
                        cx.notify();
                    })
                })
            }
            SyncState::Sending { held: true, .. } => {
                let (st, rt) = (st.clone(), rt.clone());
                Rc::new(move |_, cx| st.update(cx, |s, cx| s.push_changes(std::slice::from_ref(&rt), cx)))
            }
            SyncState::Offline { .. } | SyncState::Behind { step: hopper::syncchip::BehindStep::Failed, .. } => {
                let (st, rt) = (st.clone(), rt.clone());
                Rc::new(move |_, cx| st.update(cx, |s, cx| s.recheck_group(&rt, cx)))
            }
            _ => {
                let (st, rt) = (st.clone(), rt.clone());
                Rc::new(move |_, cx| st.update(cx, |s, cx| s.pull_group_now(&rt, cx)))
            }
        };
        let disabled = pulling || (matches!(state, SyncState::Sending { held: true, .. }) && pushing);
        let label = if pulling { "Checking...".to_string() } else { label.clone() };
        actions.push(Action { label: label.into(), icon: Some(IconName::RefreshCw), primary: true, disabled, run });
    }
    let body: Option<crate::sync_chip::Body> = match &state {
        SyncState::NeedsYou { .. } => {
            let (st, rt) = (store.clone(), root.to_path_buf());
            Some(Rc::new(move |window, cx| {
                let cards = st.read(cx).sync_cards(&rt);
                crate::sync_cards::cards_view(cards, sync_env(&st, &rt), window, cx)
            }))
        }
        SyncState::Differs { .. } => {
            let (st, rt) = (store.clone(), root.to_path_buf());
            Some(Rc::new(move |window, cx| {
                let rows = st.read(cx).sync_drift_rows(&rt);
                let narrow = crate::sync_cards::is_narrow(window);
                crate::sync_cards::drift_view(rows, sync_env(&st, &rt), narrow, window, cx)
            }))
        }
        _ => None,
    };
    let mut sections = vec![Section { title: gl.title.clone().into(), line: line.into(), progress: None, body, actions }];
    if !notes.is_empty() {
        let (st, rt) = (store.clone(), root.to_path_buf());
        let count = notes.len();
        sections.push(Section {
            title: if count == 1 { "1 note".into() } else { format!("{count} notes").into() },
            line: "".into(),
            progress: None,
            body: Some(Rc::new(move |_, cx| {
                let mut list = v_flex().w_full().gap_1();
                for (ix, n) in st.read(cx).sync_away_notes(&rt).into_iter().enumerate() {
                    let (s1, r1) = (st.clone(), rt.clone());
                    let (s2, r2, id, name, since) = (st.clone(), rt.clone(), n.id.clone(), n.name.clone(), n.since);
                    let see: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, cx| {
                        let root = r1.clone();
                        s1.update(cx, |_, cx| cx.emit(crate::store::StoreEvent::ShowInstance(root)));
                    });
                    let put: Rc<dyn Fn(&mut Window, &mut App)> =
                        Rc::new(move |window, cx| crate::sync_put_back::open(s2.clone(), r2.clone(), id.clone(), name.clone(), since, window, cx));
                    list = list.child(crate::sync_cards::away_row(ix, &n.name, n.changes, see, put, cx));
                }
                list.into_any_element()
            })),
            actions: Vec::new(),
        });
    }
    let wide = matches!(state, SyncState::NeedsYou { .. } | SyncState::Differs { .. });
    Chip {
        id: "group".into(),
        look: gl.look,
        progress: None,
        sections,
        auto_open: Vec::new(),
        // As wide as the mock's cards (440 px): mod names, a chip and buttons fit a row.
        width: wide.then_some(27.5),
    }
}
/// The line under "Same as the group": the status line without its first
/// sentence (the title says it), or what Pull now just did, as it is.
fn in_step_line(detail: &str, from_pull: bool) -> String {
    if from_pull {
        return detail.to_string();
    }
    detail.strip_prefix("Same as the group. ").unwrap_or("Nothing new to take in.").to_string()
}

/// names are links with their icons, and one button gets what is missing.
/// Every other state is the chip's own words ([`sync_group_chip`]).
pub fn group_status_bar(store: &Entity<Store>, root: &Path, cx: &mut App) -> Option<Notice> {
    let s = store.read(cx);
    let status = s.group_status(root)?;
    let Status::NeedsMod { needs } = &status else { return None };
    let busy = s.is_busy(root);
    let pulling = s.group_pulling(root);
    let pulled = s.group_pulled(root).map(|(t, _)| t.to_string());
    let (icon, tier) = look(&status);
    let from_pull = pulled.is_some();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let detail = pulled.unwrap_or_else(|| status.detail(now));
    let mut n = Notice::new("group-status", tier, status.title()).icon(icon);
    // The mods it names are links with their icons.
    n = if from_pull { n.subject(detail) } else { n.subject_rich(detail, needs_line(store, root, needs, cx)) };
    if needs.iter().any(|n| n.to_get()) {
        // Download what the pack lacks; the group's sync then carries it to
        // everyone.
        let ids: Vec<String> = needs.iter().filter(|n| n.to_get()).map(|n| n.id.clone()).collect();
        let label = if ids.len() == 1 { "Add it" } else { "Add them" };
        let (store, root) = (store.clone(), root.to_path_buf());
        n = n.primary(move |fit| {
            notice_slot::main_button(Button::new("group-add-needed").small(), fit)
                .icon(IconName::Download)
                .label(label)
                .disabled(busy)
                .tooltip("Downloads it from Modrinth or CurseForge. It goes to the group's pack too")
                .on_click(move |_, _, cx| {
                    let (store, root, ids) = (store.clone(), root.clone(), ids.clone());
                    store.update(cx, |s, _| {
                        s.add_group_needs(&root, ids);
                    });
                })
        });
    }
    // Pull now checks; the group's changes come in by themselves.
    let (store, root) = (store.clone(), root.to_path_buf());
    n = n.action_fit(move |fit| pull_button(&store, &root, pulling, false, fit.narrow, "group-pull"));
    Some(n)
}

/// The "Missing a required mod" notice's line as elements: the mods it names
/// (the missing ones and the ones that need them) with their icons, each
/// opening its page beside the instance. A missing mod no site has a page
/// for shows its id and doesn't open.
fn needs_line(store: &Entity<Store>, root: &Path, needs: &[hopper::groupdeps::Need], cx: &mut App) -> AnyElement {
    use hopper::groupdeps::Piece;
    let wanted: Vec<String> = needs.iter().filter_map(|n| store.read(cx).group_need_project(&n.id)).map(|(_, icon)| icon).filter(|i| !i.is_empty()).collect();
    store.update(cx, |s, _| wanted.iter().for_each(|i| s.want_image(i)));
    let s = store.read(cx);
    let inst = s.instance(root);
    let peek = |id: String, name: String, icon: Option<std::path::PathBuf>, project: u64, cx: &App| {
        let (store, root) = (store.clone(), root.to_path_buf());
        crate::widgets::mod_link(
            SharedString::from(id),
            name,
            icon.as_deref(),
            project,
            move |project, _, cx| {
                let root = root.clone();
                store.update(cx, |_, cx| cx.emit(crate::store::StoreEvent::Peek { root, project }));
            },
            cx,
        )
        .font_semibold()
        .into_any_element()
    };
    // No gap: the words carry their own spaces, so a comma sits against the
    // mod before it.
    // The line wraps: in the chip's popover a long sentence must not squeeze
    // the mod names to nothing and leave their icons on top of the words.
    let mut line = h_flex().w_full().min_w_0().flex_wrap().items_center();
    for (i, piece) in hopper::groupdeps::detail_pieces(needs).into_iter().enumerate() {
        line = match piece {
            Piece::Text(t) => line.child(div().min_w_0().child(t)),
            Piece::Need(k) => {
                let n = &needs[k];
                let (project, icon) = s.group_need_project(&n.id).unwrap_or_default();
                line.child(peek(format!("need-dep:{i}"), n.name.clone(), s.image(&icon).map(Path::to_path_buf), project, cx))
            }
            Piece::By(name) => {
                let jar = needs.iter().flat_map(|n| n.by.iter()).find(|(by, _)| *by == name).map(|(_, f)| f.as_str());
                let a = jar.and_then(|f| inst.and_then(|i| i.addons.iter().find(|a| a.file_name.eq_ignore_ascii_case(f))));
                line.child(peek(format!("need-by:{i}"), name, a.and_then(|a| s.image(&a.icon)).map(Path::to_path_buf), a.map_or(0, |a| a.project_id), cx))
            }
        };
    }
    line.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::in_step_line;

    #[test]
    fn in_step_line_drops_the_repeated_sentence() {
        assert_eq!(in_step_line("Same as the group. Checked 2 min ago.", false), "Checked 2 min ago.");
        assert_eq!(in_step_line("Same as the group.", false), "Nothing new to take in.");
    }

    #[test]
    fn in_step_line_keeps_what_pull_now_did() {
        let said = "Took in 3 changes from Mia. Now the same as the group.";
        assert_eq!(in_step_line(said, true), said);
    }
}
