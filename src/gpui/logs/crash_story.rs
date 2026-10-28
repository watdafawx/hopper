//! The crash card's story when a mod failed while the game was loading and
//! the crash came after it (a knock-on effect): the fix first, then what
//! happened, in order. See design/crash-detection.md, "Root cause first".

use super::LogsView;
use crate::store::{Store, StoreEvent};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::crashdetect::{Crash, Stopped};
use std::path::Path;

/// A mod by its jar: icon and name, opening the mod peek on click.
pub(super) fn mod_ref(store: &Entity<Store>, root: &Path, file: &str, key: &str, cx: &App) -> AnyElement {
    let s = store.read(cx);
    let bare = file.trim_end_matches(".disabled");
    let a = s.instance(root).and_then(|i| i.addons.iter().find(|a| a.file_name.eq_ignore_ascii_case(bare)));
    let name = a
        .map(|a| a.name.clone())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| hopper::crashdetect::jar_name(&root.join("mods"), file));
    let icon = a.and_then(|a| s.image(&a.icon)).map(Path::to_path_buf);
    let pid = a.map_or(0, |a| a.project_id);
    let (store2, root2) = (store.clone(), root.to_path_buf());
    widgets::mod_link(
        ElementId::Name(format!("crash-story:{key}").into()),
        name,
        icon.as_deref(),
        pid,
        move |project, _, cx| {
            let root = root2.clone();
            store2.update(cx, |_, cx| cx.emit(StoreEvent::Peek { root, project }));
        },
        cx,
    )
    .flex_none()
    .font_semibold()
    .into_any_element()
}

pub(super) use crate::crash_view::{sentence, words};

impl LogsView {
    /// The body of the crash card for `st`: what to do, then what happened.
    pub(super) fn crash_story(&mut self, root: &Path, c: &Crash, st: &Stopped, cx: &mut Context<Self>) -> AnyElement {
        let store = self.store.clone();
        let mods = root.join("mods");
        let link = |file: &str, key: &str, cx: &App| mod_ref(&store, root, file, key, cx);
        let root_suspect = c.suspects.iter().find(|s| s.file_name == st.file_name);
        let changed = root_suspect.and_then(|s| s.changed.clone());
        // Put back already: the older copy left backups for the mods folder.
        let back = changed.as_ref().is_some_and(|ch| {
            !ch.backup.exists() && ch.backup.file_name().is_some_and(|n| mods.join(n).exists())
        });
        let changed_file = changed.as_ref().map(|ch| {
            if back { ch.backup.file_name().unwrap_or_default().to_string_lossy().into_owned() } else { ch.file_name.clone() }
        });
        let versions = crate::crash_view::versions;
        let title = crate::crash_view::story_title(c, st, &link, cx);

        // The fix: put the changed mod back, or turn this one off.
        let actions = self.crash_actions(root, c, cx);
        let fix = crate::crash_view::fix_box(cx)
            .when_some(changed.clone().zip(changed_file.clone()), |d, (ch, file)| {
                let when = hopper::time::relative_secs(ch.when);
                d.child(
                    sentence(vec![link(&file, "fix", cx), words(format!("was updated {when}{}", versions(&ch)))])
                        .text_sm()
                        .font_semibold(),
                )
                .child(if back {
                    h_flex()
                        .gap_2()
                        .child(widgets::done_mark(
                            "crash-put-back-done",
                            format!("{} is back", if ch.from.is_empty() { "The older version".to_string() } else { ch.from.clone() }),
                            true,
                            cx,
                        ))
                        .child(widgets::caption("Play to try again. Backups has the newer one.", cx).text_xs())
                        .into_any_element()
                } else {
                    let (root_p, ch_p) = (root.to_path_buf(), ch.clone());
                    let label = if ch.from.is_empty() { "Put the older version back".to_string() } else { format!("Put {} back", ch.from) };
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .child(
                            Button::new("crash-put-back")
                                .small()
                                .primary()
                                .icon(IconName::Undo2)
                                .label(label)
                                .tooltip("Swaps in the copy Hopper kept. The newer one goes to Backups, so you can undo this there.")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let store = this.store.clone();
                                    crate::crash_help::put_back_changed(&store, &root_p, &ch_p, window, cx);
                                    cx.notify();
                                })),
                        )
                        .children(actions.map(|a| div().child(a)))
                        .child(widgets::caption(
                            if ch.to.is_empty() {
                                "The newer one goes to Backups.".to_string()
                            } else {
                                format!("{} goes to Backups.", ch.to)
                            },
                            cx,
                        ).text_xs())
                        .into_any_element()
                })
            })
            .when(changed.is_none(), |d| d.children(self.crash_actions(root, c, cx)))
            .children({
                let above = changed.is_some() || self.crash_actions(root, c, cx).is_some();
                self.config_switches(root, c, Some(above), cx)
            });
        // A box with nothing to offer (all turned off and played) isn't shown.
        let has_fix = changed.is_some() || self.crash_actions(root, c, cx).is_some() || !c.switches.is_empty() || !c.switched_off.is_empty();

        let happened = crate::crash_view::what_happened(c, st, changed.as_ref().zip(changed_file.as_deref()), &link, cx);

        v_flex()
            .gap_2()
            .child(title)
            .when(has_fix, |d| d.child(fix))
            .child(happened)
            .into_any_element()
    }
}
