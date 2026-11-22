//! The instance page's strip for a pack kept in step with a friends group:
//! whether this PC's copy has what the group has, what it waits for if not,
//! and "Pull now" (`hopper::groupstatus`, `design/group-sync-status.md`).
//!
//! The updates themselves run by themselves when they can
//! (`groupsync::merge`). The strip's main button is for what only the
//! player can do: open the update dialog for a pack version (the game
//! closed, settings both sides changed, or a failed try), or say yes to
//! mods from links this PC doesn't trust yet. "Pull now" is always there:
//! it checks the group and takes everything in at once.
//!
//! With the sync choices of `hopper::syncmode` it also carries "Pull from
//! Mia" (one button per member whose changes wait for the player) and
//! "Push 3 changes" (this PC's own, held back until pressed).

use crate::incoming_sources;
use crate::notice_slot::{self, Notice, Tier};
use crate::store::Store;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, h_flex, v_flex};
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

/// What a row of the check says under the mod's name, in plain words.
pub fn why_says(r: &hopper::groupverify::Row) -> String {
    use hopper::groupverify::Why;
    let base = match r.why {
        Why::Missing => "The group has it. You don't.".to_string(),
        Why::Different if r.older => "Yours is older than the group's.".to_string(),
        Why::Different => "Yours is another version than the group's.".to_string(),
        Why::OffHere => "Turned off here, on in the group.".to_string(),
        Why::OnHere => "Turned on here, off in the group.".to_string(),
        Why::LeftOut => "From a link you haven't allowed yet.".to_string(),
        Why::OnlyHere => "Kept only on this PC. Hopper leaves it as it is.".to_string(),
        Why::Locked => "You locked its version. Hopper leaves it as it is.".to_string(),
    };
    if r.why.repairable() && !r.by_name.is_empty() { format!("{base} Last changed by {}.", r.by_name) } else { base }
}

/// The list under the strip when the copy differs from the group: one row
/// per mod with why, and its own button. Mods the player keeps come last
/// and have none.
fn verify_list<V: 'static>(store: &Entity<Store>, root: &Path, v: &hopper::groupverify::Verified, busy: bool, cx: &mut Context<V>) -> AnyElement {
    use hopper::groupverify::Why;
    let icons: Vec<String> = v.rows.iter().filter_map(|r| store.read(cx).verify_name(&r.key).map(|n| n.2.clone())).filter(|i| !i.is_empty()).collect();
    store.update(cx, |s, _| icons.iter().for_each(|i| s.want_image(i)));
    let s = store.read(cx);
    let mut list = v_flex().w_full().gap_0p5();
    for (ix, r) in v.rows.iter().enumerate() {
        let (name, project, icon) = s.verify_name(&r.key).cloned().unwrap_or((r.name.clone(), r.project, String::new()));
        let name = if r.name.is_empty() { name } else { r.name.clone() };
        let name = if name.is_empty() { "A mod".to_string() } else { name };
        let project = if r.project != 0 { r.project } else { project };
        let (st, rt) = (store.clone(), root.to_path_buf());
        let link = crate::widgets::mod_link(
            SharedString::from(format!("verify-mod:{ix}")),
            name,
            s.image(&icon),
            project,
            move |project, _, cx| {
                let root = rt.clone();
                st.update(cx, |_, cx| cx.emit(crate::store::StoreEvent::Peek { root, project }));
            },
            cx,
        )
        .font_semibold()
        .into_any_element();
        let button = r.fixable().then(|| {
            let (st, rt, key, pin) = (store.clone(), root.to_path_buf(), r.key.clone(), r.pin.clone());
            let action = r.why.action();
            let left_out = r.why == Why::LeftOut;
            let b = Button::new(("verify-fix", ix)).small().outline().label(action).disabled(busy).tooltip(match r.why {
                Why::LeftOut => "Say yes to this source, then get the mod",
                Why::Missing => "Gets this mod, as the group has it",
                _ => "Makes this mod as the group has it",
            });
            b.on_click(move |_, window, cx| {
                let (st, rt, key) = (st.clone(), rt.clone(), key.clone());
                match pin.clone() {
                    Some(pin) if left_out => {
                        let (st2, rt2, key2) = (st.clone(), rt.clone(), key.clone());
                        incoming_sources::gate(st, vec![pin.clone()], window, cx, move |leave_out, _, cx| {
                            let yes: Vec<_> = if leave_out.contains(&pin) { Vec::new() } else { vec![pin.clone()] };
                            if !yes.is_empty() {
                                st2.update(cx, |s, cx| s.repair_group(&rt2, Some(vec![key2.clone()]), yes, cx));
                            }
                        });
                    }
                    _ => st.update(cx, |s, cx| s.repair_group(&rt, Some(vec![key]), Vec::new(), cx)),
                }
            })
        });
        list = list.child(
            h_flex()
                .id(("verify-row", ix))
                .w_full()
                .gap_2()
                .px_1()
                .py_1()
                .items_center()
                .rounded(cx.theme().radius)
                .hover(|d| crate::widgets::row_hover(d, cx))
                .child(h_flex().flex_none().max_w(rems(16.)).overflow_hidden().text_sm().child(link))
                .child(div().flex_1().min_w_0().truncate().text_sm().text_color(cx.theme().muted_foreground).child(why_says(r)))
                .children(button),
        );
    }
    list.into_any_element()
}

/// "Pull from Mia": takes in that member's waiting changes. Just the name
/// when the page is narrow.
pub fn pull_from_button(store: &Entity<Store>, root: &Path, who: &hopper::syncmode::Parked, pulling: bool, fit: notice_slot::Fit, ix: usize) -> Button {
    let (store, root, id) = (store.clone(), root.to_path_buf(), who.id.clone());
    // Two people's buttons must never both read "Pull".
    let label = if fit.narrow { who.name.clone() } else { format!("Pull from {}", who.name) };
    notice_slot::main_button(Button::new(("group-pull-from", ix)).small(), fit)
        .icon(IconName::Download)
        .label(label)
        .disabled(pulling)
        .tooltip(format!("Takes in {}'s changes now. Other people's keep waiting", who.name))
        .on_click(move |_, _, cx| store.update(cx, |s, cx| s.pull_group_from(&root, &id, cx)))
}

/// "Push 3 changes": sends the changes held back at `roots`. Just "Push"
/// when narrow.
pub fn push_button(store: &Entity<Store>, roots: Vec<std::path::PathBuf>, changes: usize, pushing: bool, fit: notice_slot::Fit, id: &'static str) -> Button {
    let store = store.clone();
    let label = if pushing {
        "Pushing...".to_string()
    } else if fit.narrow {
        "Push".to_string()
    } else {
        hopper::syncmode::push_label(changes)
    };
    notice_slot::main_button(Button::new(id).small(), fit)
        .icon(IconName::Upload)
        .label(label)
        .disabled(pushing)
        .tooltip("Sends your changes to the people you share with")
        .on_click(move |_, _, cx| store.update(cx, |s, cx| s.push_changes(&roots, cx)))
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
/// state that needs nothing (busy, behind, offline, failed); a state that
/// needs the player keeps its strip ([`group_status_bar`]) too.
pub fn group_chip<V: 'static>(store: &Entity<Store>, root: &Path, cx: &mut Context<V>) -> Option<crate::sync_chip::Chip> {
    use crate::sync_chip::{Action, Chip, Section};
    let s = store.read(cx);
    let status = s.group_status(root)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let pulling = s.group_pulling(root) || s.group_repairing(root);
    let pulled = s.group_pulled(root).map(|(t, bad)| (t.to_string(), bad));
    let pulled_bad = pulled.as_ref().is_some_and(|(_, bad)| *bad);
    let left_out = !s.left_out_pins(root).is_empty();
    let pushing = s.pushing_at(root);
    let look = hopper::syncchip::group_look(&status, pulling, pulled_bad, left_out);
    let in_step = matches!(status, Status::InStep { .. });
    let (title, line) = match (&status, &pulled) {
        (Status::InStep { .. }, Some((t, false))) => ("Your copy matches the group.".to_string(), t.clone()),
        (Status::InStep { .. }, _) => ("Your copy matches the group.".to_string(), in_step_line(&status.detail(now), false)),
        (_, Some((t, true))) => ("Couldn't take the changes in".to_string(), t.clone()),
        _ => (status.title(), status.detail(now)),
    };
    let progress = None;
    let mut actions: Vec<Action> = Vec::new();
    fn act(actions: &mut Vec<Action>, label: String, icon: IconName, primary: bool, disabled: bool, run: crate::sync_chip::Run) {
        actions.push(Action { label: label.into(), icon: Some(icon), primary, disabled, run });
    }
    let (st, rt) = (store.clone(), root.to_path_buf());
    match &status {
        Status::Parked { from, .. } => {
            for (ix, who) in from.iter().take(3).enumerate() {
                let (st, rt, id) = (st.clone(), rt.clone(), who.id.clone());
                act(&mut actions, format!("Pull from {}", who.name), IconName::Download, ix == 0, pulling, Rc::new(move |_, cx| st.update(cx, |s, cx| s.pull_group_from(&rt, &id, cx))));
            }
        }
        Status::ToPush { changes, .. } => {
            let (st, rt, n) = (st.clone(), rt.clone(), *changes);
            act(&mut actions, hopper::syncmode::push_label(n), IconName::Upload, true, pushing, Rc::new(move |_, cx| st.update(cx, |s, cx| s.push_changes(std::slice::from_ref(&rt), cx))));
        }
        Status::CantCheck | Status::Cant { .. } | Status::Offline { .. } => {
            let (st, rt) = (st.clone(), rt.clone());
            act(&mut actions, "Try again".into(), IconName::RefreshCw, true, pulling, Rc::new(move |_, cx| st.update(cx, |s, cx| s.recheck_group(&rt, cx))));
        }
        _ => {}
    }
    // Pull now is always there; in step it only checks.
    if !matches!(status, Status::CantCheck | Status::Cant { .. } | Status::Offline { .. }) || pulled_bad {
        let (st, rt) = (st.clone(), rt.clone());
        let label = if pulling { "Checking..." } else if in_step || matches!(status, Status::Parked { .. } | Status::ToPush { .. }) { "Check now" } else { "Pull now" };
        let primary = actions.is_empty();
        act(&mut actions, label.into(), IconName::RefreshCw, primary, pulling, Rc::new(move |_, cx| st.update(cx, |s, cx| s.pull_group_now(&rt, cx))));
    }
    Some(Chip {
        id: "group".into(),
        look,
        progress,
        sections: vec![Section { title: title.into(), line: line.into(), progress: None, actions }],
    })
}

/// The line under "Same as the group": the status line without its first
/// sentence (the title says it), or what Pull now just did, as it is.
fn in_step_line(detail: &str, from_pull: bool) -> String {
    if from_pull {
        return detail.to_string();
    }
    detail.strip_prefix("Same as the group. ").unwrap_or("Nothing new to take in.").to_string()
}

pub fn group_status_bar<V: 'static>(store: &Entity<Store>, root: &Path, cx: &mut Context<V>) -> Option<Notice> {
    let s = store.read(cx);
    let status = s.group_status(root)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let due = s.group_pack_due(root).cloned();
    let busy = s.is_busy(root);
    let pulling = s.group_pulling(root);
    let pulled_bad = s.group_pulled(root).is_some_and(|(_, bad)| bad);
    let pulled = s.group_pulled(root).map(|(t, _)| t.to_string());
    let left_out = s.left_out_pins(root);
    // Read before `cx` is borrowed mutably for the mod links below.
    let pushing = s.pushing_at(root);
    let repairing = s.group_repairing(root);
    let by_hand_pending = if matches!(status, Status::ByHand { .. }) { s.group_pending(root) } else { Vec::new() };
    let by_hand_fetch = if matches!(status, Status::ByHand { .. }) { s.fetch_files(root).0 } else { None };
    let by_hand_running = by_hand_fetch.as_ref().filter(|v| v.running());
    let in_step = matches!(status, Status::InStep { .. });
    // A strip only for what needs the player (a choice, mods to get). Busy,
    // behind, offline and failed say it in the hero's sync chip
    // ([`group_chip`]): a strip would push the page down and up again.
    if !hopper::syncchip::group_look(&status, pulling, pulled_bad, !left_out.is_empty()).strip && by_hand_running.is_none() {
        return None;
    }
    let key = hopper::settings::notice_key(&root.to_string_lossy(), "group-in-step", "in step");
    let (mut icon, tier) = look(&status);
    if by_hand_running.is_some() {
        icon = IconName::Hourglass;
    }
    let from_pull = pulled.is_some();
    let detail = match &by_hand_fetch {
        Some(v) if v.running() || !from_pull => v.detail(),
        _ => pulled.unwrap_or_else(|| status.detail(now)),
    };
    // "In step" is a word only we use: say what it means, and don't repeat it
    // in the line under it. Mods match the group; updates are a separate thing.
    // What Pull now just did ("Took in 2 changes ...") is kept as it is: it
    // says what the player asked for.
    let behind = matches!(status, Status::Behind { .. });
    let (title, detail) = if in_step {
        ("Your copy matches the group.".to_string(), in_step_line(&detail, from_pull))
    } else if behind && !from_pull {
        // The headline says it all; the list under it says which mods.
        (detail, String::new())
    } else if matches!(status, Status::CantCheck) && !from_pull {
        // The sentence goes under the strip, open: on a narrow page a subject
        // on the strip would be cut to nothing.
        ("Couldn't check".to_string(), String::new())
    } else if let Some(v) = by_hand_running {
        (
            format!("Downloading from CurseForge…{}", if v.total > 1 { format!(" {} of {}", (v.done + 1).min(v.total), v.total) } else { String::new() }),
            detail,
        )
    } else {
        (status.title(), detail)
    };
    let mut n = Notice::new("group-status", tier, title)
        .icon(icon)
        .progress(by_hand_running.map(|v| v.progress.unwrap_or(0.0)));
    n = match &status {
        // The mods it names are links with their icons.
        Status::NeedsMod { needs } if !from_pull => n.subject_rich(detail, needs_line(store, root, needs, cx)),
        _ => n.subject(detail),
    };
    // What the copy lacks, one row per mod with its own button.
    // When the copy matches, the mods Hopper leaves alone (kept only on this
    // PC, locked) are still listed, closed behind Details.
    let verified = s_verified(store, root, cx).filter(|v| behind || (in_step && !v.rows.is_empty()));
    if let Some(v) = &verified {
        let list = n.details(verify_list(store, root, v, busy || repairing, cx));
        n = if behind { list.open_new() } else { list };
    }
    if matches!(status, Status::CantCheck) && !from_pull {
        let said = status.detail(now);
        let muted = cx.theme().muted_foreground;
        n = n.details(div().w_full().text_sm().text_color(muted).child(said)).open_new();
    }
    if in_step {
        n = n.dismiss_key(store, key);
    }
    // What only the player can do.
    let update_file = due.as_ref().and_then(|d| d.file.clone().map(|f| (f, d.action())));
    if let Some((file, action)) = update_file {
        let (store, root) = (store.clone(), root.to_path_buf());
        n = n.primary(move |fit| {
            notice_slot::main_button(Button::new("group-pack-update").small(), fit)
                .label(action)
                .disabled(busy)
                .tooltip("Opens the update, where you can see what changes and pick what to keep")
                .on_click(move |_, window, cx| crate::pack_update::open(store.clone(), root.clone(), file.clone(), window, cx))
        });
    } else if let Status::NeedsMod { needs } = &status
        && needs.iter().any(|n| n.to_get())
    {
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
    } else if let Some(v) = verified.as_ref().filter(|v| !v.to_fix().is_empty()) {
        // Exactly the rows of the list: the ones that need no yes first;
        // when only mods from links are left, the yes.
        let plain: Vec<String> = v.to_fix().iter().filter(|r| r.why != hopper::groupverify::Why::LeftOut).map(|r| r.key.clone()).collect();
        let pins: Vec<hopper::sources::carry::Pin> = v.to_fix().iter().filter(|r| r.why == hopper::groupverify::Why::LeftOut).filter_map(|r| r.pin.clone()).collect();
        let left_keys: Vec<String> = v.to_fix().iter().filter(|r| r.why == hopper::groupverify::Why::LeftOut).map(|r| r.key.clone()).collect();
        let (store, root) = (store.clone(), root.to_path_buf());
        n = n.primary(move |fit| {
            if !plain.is_empty() {
                notice_slot::main_button(Button::new("group-make-match").small(), fit)
                    .icon(IconName::Download)
                    .label(if repairing { "Making it match..." } else if fit.narrow { "Make match" } else { "Make my copy match" })
                    .disabled(busy || repairing)
                    .tooltip("Gets, updates or switches exactly the mods in this list. Nothing else changes")
                    .on_click(move |_, _, cx| {
                        let (store, root, plain) = (store.clone(), root.clone(), plain.clone());
                        store.update(cx, |s, cx| s.repair_group(&root, Some(plain), Vec::new(), cx));
                    })
            } else {
                notice_slot::main_button(Button::new("group-allow-get").small(), fit)
                    .label("Allow and get it")
                    .disabled(busy || repairing)
                    .tooltip("Say yes to these mods' sources, then get them")
                    .on_click(move |_, window, cx| {
                        let (store2, root2, all, keys) = (store.clone(), root.clone(), pins.clone(), left_keys.clone());
                        incoming_sources::gate(store.clone(), pins.clone(), window, cx, move |leave_out, _, cx| {
                            let yes: Vec<_> = all.iter().filter(|p| !leave_out.contains(p)).cloned().collect();
                            if !yes.is_empty() {
                                store2.update(cx, |s, cx| s.repair_group(&root2, Some(keys.clone()), yes, cx));
                            }
                        });
                    })
            }
        });
    } else if matches!(status, Status::CantCheck) {
        let (store, root) = (store.clone(), root.to_path_buf());
        n = n.primary(move |fit| {
            notice_slot::main_button(Button::new("group-check-again").small(), fit)
                .icon(IconName::RefreshCw)
                .label("Try again")
                .tooltip("Compares your copy with the group again")
                .on_click(move |_, _, cx| store.update(cx, |s, cx| s.recheck_group(&root, cx)))
        });
    } else if let Status::Parked { from, .. } = &status {
        // One button per member whose changes wait; the first leads.
        for (ix, who) in from.iter().take(3).enumerate() {
            let (store, root, who) = (store.clone(), root.to_path_buf(), who.clone());
            if ix == 0 {
                n = n.primary(move |fit| pull_from_button(&store, &root, &who, pulling, fit, ix));
            } else {
                n = n.action_fit(move |fit| pull_from_button(&store, &root, &who, pulling, notice_slot::Fit { lead: false, ..fit }, ix));
            }
        }
    } else if let Status::ToPush { changes, .. } = status {
        let (store, root) = (store.clone(), root.to_path_buf());
        n = n.primary(move |fit| push_button(&store, vec![root.clone()], changes, pushing, fit, "group-push"));
    } else if matches!(status, Status::ByHand { .. }) && !by_hand_pending.is_empty() {
        let urls: Vec<String> = by_hand_pending.iter().map(|p| p.url.clone()).filter(|u| !u.is_empty()).collect();
        let (store1, root1) = (store.clone(), root.to_path_buf());
        let fetch = by_hand_fetch.clone();
        n = n
            .primary(move |fit| {
                crate::fetch_progress::download_button("group-pending-download", store1.clone(), root1.clone(), urls.clone(), fetch.clone(), fit)
            })
            .action(crate::fetch_progress::show_files_button(
                "group-pending-show",
                store.clone(),
                root.to_path_buf(),
                by_hand_pending,
            ));
    } else if matches!(status, Status::Cant { .. }) && !left_out.is_empty() {
        let (store, root) = (store.clone(), root.to_path_buf());
        n = n.primary(move |fit| {
            notice_slot::main_button(Button::new("group-allow").small(), fit)
                .label("Allow and install")
                .tooltip("Say yes to these mods, then put them in")
                .on_click(move |_, window, cx| {
                    let (store2, root2) = (store.clone(), root.clone());
                    incoming_sources::gate(store.clone(), left_out.clone(), window, cx, move |leave_out, _, cx| {
                        store2.update(cx, |s, cx| s.approve_left_out(&root2, &leave_out, cx));
                    });
                })
        });
    }
    // Only the player's buttons bring the rest in: Pull now checks.
    let nothing_to_pull = in_step || matches!(status, Status::Parked { .. } | Status::ToPush { .. });
    let (store, root) = (store.clone(), root.to_path_buf());
    n = n.action_fit(move |fit| pull_button(&store, &root, pulling, nothing_to_pull, fit.narrow, "group-pull"));
    Some(n)
}

/// The check's last answer for the copy at `root`, when it is up to date.
fn s_verified<V: 'static>(store: &Entity<Store>, root: &Path, cx: &mut Context<V>) -> Option<hopper::groupverify::Verified> {
    store.read(cx).group_verified(root).cloned()
}

/// The "Missing a required mod" strip's line as elements: the mods it names
/// (the missing ones and the ones that need them) with their icons, each
/// opening its page beside the instance. A missing mod no site has a page
/// for shows its id and doesn't open.
fn needs_line<V: 'static>(store: &Entity<Store>, root: &Path, needs: &[hopper::groupdeps::Need], cx: &mut Context<V>) -> AnyElement {
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
    let mut line = h_flex().min_w_0().items_center();
    for (i, piece) in hopper::groupdeps::detail_pieces(needs).into_iter().enumerate() {
        line = match piece {
            Piece::Text(t) => line.child(div().flex_none().whitespace_nowrap().child(t)),
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
