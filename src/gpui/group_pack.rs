//! The instance page's notice for a group pack kept both ways whose group
//! moved the pack to another version that this PC couldn't follow yet
//! (the game runs, the update needs the player to pick, or it failed). The
//! update itself runs by itself when it can (`groupsync::merge`); the
//! button opens the usual update dialog for that version.

use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::grouppack::Wait;
use std::path::Path;

pub fn group_pack_bar<V: 'static>(store: &Entity<Store>, root: &Path, cx: &mut Context<V>) -> Option<AnyElement> {
    let s = store.read(cx);
    let due = s.group_pack_due(root)?.clone();
    let busy = s.is_busy(root);
    let theme = cx.theme();
    let color = match due.wait {
        Wait::Failed(_) => theme.warning,
        _ => theme.info,
    };
    let (store, root) = (store.clone(), root.to_path_buf());
    Some(
        widgets::notice_bar(color, IconName::RefreshCw, cx)
            .mx_6()
            .mt_3()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).truncate().child(due.title()))
                    .child(widgets::caption(due.detail(), cx)),
            )
            .when_some(due.file.clone(), |bar, file| {
                bar.child(
                    Button::new("group-pack-update")
                        .small()
                        .outline()
                        .label(due.action())
                        .disabled(busy)
                        .tooltip("Opens the update, where you can see what changes and pick what to keep")
                        .on_click(move |_, window, cx| crate::pack_update::open(store.clone(), root.clone(), file.clone(), window, cx)),
                )
            })
            .into_any_element(),
    )
}
