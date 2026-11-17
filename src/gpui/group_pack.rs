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
use gpui_kit::component::{Disableable as _, Sizable as _};
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
    }
}

/// The "Pull now" button; icon only when the page is narrow.
pub fn pull_button(store: &Entity<Store>, root: &Path, pulling: bool, narrow: bool, id: &'static str) -> Button {
    let (store, root) = (store.clone(), root.to_path_buf());
    let b = Button::new(id)
        .small()
        .outline()
        .icon(IconName::RefreshCw)
        .disabled(pulling)
        .tooltip("Checks the group for newer changes and takes them in now");
    let b = if narrow { b } else { b.label(if pulling { "Checking..." } else { "Pull now" }) };
    b.on_click(move |_, _, cx| store.update(cx, |s, cx| s.pull_group_now(&root, cx)))
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
    let detail = pulled.unwrap_or_else(|| status.detail(now));
    let mut n = Notice::new("group-status", tier, status.label()).icon(icon).subject(detail);
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
    n = n.action_fit(move |fit| pull_button(&store, &root, pulling, fit.narrow, "group-pull"));
    Some(n)
}
