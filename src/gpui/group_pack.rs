//! The instance page's notice for a group pack kept both ways whose group
//! moved the pack to another version that this PC couldn't follow yet
//! (the game runs, the update needs the player to pick, or it failed). The
//! update itself runs by itself when it can (`groupsync::merge`); the
//! button opens the usual update dialog for that version, and a failed
//! try can be tried again at once.

use crate::notice_slot::{self, Notice, Tier};
use crate::store::Store;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::{Disableable as _, Sizable as _};
use gpui_kit::*;
use hopper::grouppack::Wait;
use std::path::Path;

pub fn group_pack_bar<V: 'static>(store: &Entity<Store>, root: &Path, cx: &mut Context<V>) -> Option<Notice> {
    let s = store.read(cx);
    let due = s.group_pack_due(root)?.clone();
    let busy = s.is_busy(root);
    let trying = s.group_pack_trying(root);
    let failed = matches!(due.wait, Wait::Failed(_));
    let (store2, root2) = (store.clone(), root.to_path_buf());
    let (store, root) = (store.clone(), root.to_path_buf());
    // Waiting is news; a failed try needs the player.
    let mut n = Notice::new("group-pack", if failed { Tier::Choice } else { Tier::Info }, due.title())
        .icon(IconName::RefreshCw)
        .subject(due.detail());
    if let Some(file) = due.file.clone() {
        let action = due.action();
        n = n.primary(move |fit| {
            notice_slot::main_button(Button::new("group-pack-update").small(), fit)
                .label(action)
                .disabled(busy)
                .tooltip("Opens the update, where you can see what changes and pick what to keep")
                .on_click(move |_, window, cx| crate::pack_update::open(store.clone(), root.clone(), file.clone(), window, cx))
        });
    }
    if failed {
        n = n.action(
            Button::new("group-pack-retry")
                .small()
                .outline()
                .label(if trying { "Trying..." } else { "Try again" })
                .disabled(busy || trying)
                .on_click(move |_, _, cx| store2.update(cx, |s, cx| s.retry_group_pack(&root2, cx))),
        );
    }
    Some(n)
}
