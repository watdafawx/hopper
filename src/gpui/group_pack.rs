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

use crate::incoming_sources;
use crate::notice_slot::{self, Notice, Tier};
use crate::store::Store;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::{Disableable as _, Sizable as _, StyledExt as _, h_flex};
use gpui_kit::*;
use hopper::groupstatus::Status;
use std::path::Path;

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
    let pulled = s.group_pulled(root).map(|(t, _)| t.to_string());
    let left_out = s.left_out_pins(root);
    let in_step = matches!(status, Status::InStep { .. });
    // In step is good news the player may not want to see every time.
    let key = hopper::settings::notice_key(&root.to_string_lossy(), "group-in-step", "in step");
    if in_step && notice_slot::hidden(store, &key, cx) {
        return None;
    }
    let (icon, tier) = look(&status);
    let from_pull = pulled.is_some();
    let detail = pulled.unwrap_or_else(|| status.detail(now));
    // "In step" is a word only we use: say what it means, and don't repeat it
    // in the line under it. Mods match the group; updates are a separate thing.
    // What Pull now just did ("Took in 2 changes ...") is kept as it is: it
    // says what the player asked for.
    let (title, detail) = if in_step {
        ("Same as the group", in_step_line(&detail, from_pull))
    } else {
        (status.label(), detail)
    };
    let mut n = Notice::new("group-status", tier, title).icon(icon);
    n = match &status {
        // The mods it names are links with their icons.
        Status::NeedsMod { needs } if !from_pull => n.subject_rich(detail, needs_line(store, root, needs, cx)),
        _ => n.subject(detail),
    };
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
    let (store, root) = (store.clone(), root.to_path_buf());
    n = n.action_fit(move |fit| pull_button(&store, &root, pulling, in_step, fit.narrow, "group-pull"));
    Some(n)
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
