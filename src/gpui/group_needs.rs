//! "Add it to the group too?": asked after a player adds or updates a mod
//! in a friends group's pack and that mod needs a mod the pack lacks
//! (`hopper::groupdeps`). Yes downloads it; the group's sync then carries it
//! to everyone. Not now leaves it to the sync status, which says the pack
//! is missing a required mod until it is added.

use crate::store::{Ask, Store, StoreEvent};
use crate::widgets;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::*;

/// Opens the question for `ask`.
pub fn open(store: Entity<Store>, ask: Ask, window: &mut Window, cx: &mut App) {
    let needs: Vec<_> = ask.needs.iter().map(|(n, _, _)| n.clone()).collect();
    // Only what can be downloaded is offered; a mod that is there but off
    // is for the Mods tab.
    let get: Vec<_> = needs.iter().filter(|n| n.to_get()).cloned().collect();
    if get.is_empty() {
        return;
    }
    let words = hopper::groupdeps::ask(&get, &ask.group);
    let ids: Vec<String> = get.iter().map(|n| n.id.clone()).collect();
    store.update(cx, |s, _| {
        for (_, _, icon) in &ask.needs {
            if !icon.is_empty() {
                s.want_image(icon);
            }
        }
    });
    window.open_dialog(cx, move |dialog, window, cx| {
        let (s1, root, ids) = (store.clone(), ask.root.clone(), ids.clone());
        let theme = cx.theme().clone();
        let rows: Vec<AnyElement> = ask
            .needs
            .iter()
            .filter(|(n, _, _)| n.to_get())
            .enumerate()
            .map(|(i, (need, project, icon))| {
                let st = store.read(cx);
                let dep_icon = st.image(icon).map(std::path::Path::to_path_buf);
                let (store2, root2) = (store.clone(), ask.root.clone());
                let dep = widgets::mod_link(
                    SharedString::from(format!("need-dep:{i}")),
                    need.name.clone(),
                    dep_icon.as_deref(),
                    *project,
                    move |project, _, cx| {
                        let root = root2.clone();
                        store2.update(cx, |_, cx| cx.emit(StoreEvent::Peek { root, project }));
                    },
                    cx,
                )
                .font_semibold();
                // Who needs it, with their icons.
                let by = need.by.iter().enumerate().map(|(k, (name, file))| {
                    let a = st.instance(&ask.root).and_then(|inst| inst.addons.iter().find(|a| a.file_name.eq_ignore_ascii_case(file)));
                    let icon = a.and_then(|a| st.image(&a.icon)).map(std::path::Path::to_path_buf);
                    let pid = a.map_or(0, |a| a.project_id);
                    let (store3, root3) = (store.clone(), ask.root.clone());
                    widgets::mod_link(
                        SharedString::from(format!("need-by:{i}:{k}")),
                        name.clone(),
                        icon.as_deref(),
                        pid,
                        move |project, _, cx| {
                            let root = root3.clone();
                            store3.update(cx, |_, cx| cx.emit(StoreEvent::Peek { root, project }));
                        },
                        cx,
                    )
                });
                h_flex()
                    .flex_wrap()
                    .items_center()
                    .gap_x_2()
                    .gap_y_1()
                    .child(dep)
                    .child(widgets::caption("needed by", cx))
                    .children(by)
                    .into_any_element()
            })
            .collect();
        dialog
            .title(words.title.clone())
            .w(rems(32.).to_pixels(window.rem_size()))
            .margin_top(rems(widgets::DIALOG_TOP).to_pixels(window.rem_size()))
            .child(
                v_flex()
                    .gap_3()
                    .child(div().text_sm().child(words.body.clone()))
                    .child(
                        v_flex()
                            .gap_1p5()
                            .p_3()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.border)
                            .children(rows),
                    ),
            )
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("group-needs-no").outline().label(words.no.clone())))
                    .child(DialogAction::new().child(Button::new("group-needs-yes").primary().label(words.yes.clone()))),
            )
            .on_ok(move |_, _, cx| {
                let (s, root, ids) = (s1.clone(), root.clone(), ids.clone());
                s.update(cx, |st, _| {
                    st.add_group_needs(&root, ids);
                });
                true
            })
    });
}
