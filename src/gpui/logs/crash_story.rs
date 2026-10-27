//! The crash card's story when a mod failed while the game was loading and
//! the crash came after it (a knock-on effect): the fix first, then what
//! happened, in order. See design/crash-detection.md, "Root cause first".

use super::LogsView;
use crate::store::{Store, StoreEvent};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::crashdetect::{Certainty, Crash, Stopped};
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

/// Words and mod names in one line that wraps between them.
pub(super) fn sentence(parts: Vec<AnyElement>) -> Div {
    h_flex().flex_wrap().items_center().gap_x_1().gap_y_0p5().children(parts)
}

pub(super) fn words(t: impl Into<SharedString>) -> AnyElement {
    div().child(t.into()).into_any_element()
}

impl LogsView {
    /// The body of the crash card for `st`: what to do, then what happened.
    pub(super) fn crash_story(&mut self, root: &Path, c: &Crash, st: &Stopped, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
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
        let versions = |ch: &hopper::crashdetect::Changed| {
            if ch.from.is_empty() || ch.to.is_empty() { String::new() } else { format!(" ({} → {})", ch.from, ch.to) }
        };

        let certain = || widgets::chip(theme.danger).xsmall().outline().child(Certainty::Certain.label());
        let title = sentence(vec![link(&st.file_name, "title", cx), words("stopped the game from loading"), certain().into_any_element()])
            .text_sm();

        // The fix: put the changed mod back, or turn this one off.
        let actions = self.crash_actions(root, c, cx);
        let fix = v_flex()
            .gap_2()
            .p_3()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.primary.opacity(0.35))
            .bg(theme.primary.opacity(0.07))
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

        // What happened, in order.
        let mut steps: Vec<(bool, AnyElement)> = Vec::new();
        if let (Some(ch), Some(file)) = (&changed, &changed_file) {
            let when = hopper::time::relative_secs(ch.when);
            let what = if ch.from.is_empty() || ch.to.is_empty() {
                format!("was updated {when}.")
            } else {
                format!("was updated from {} to {}, {when}.", ch.from, ch.to)
            };
            steps.push((false, sentence(vec![link(file, "s1", cx), words(what)]).into_any_element()));
        }
        steps.push((
            true,
            v_flex()
                .gap_0p5()
                .child(sentence(vec![link(&st.file_name, "s2", cx), certain().into_any_element(), words(if st.error.is_empty() {
                    "failed while the game was loading."
                } else {
                    "stopped loading with this error:"
                })]))
                .when(!st.error.is_empty(), |d| {
                    d.child(widgets::caption(format!("\"{}\"", st.error), cx).text_xs().whitespace_normal())
                })
                .into_any_element(),
        ));
        if !st.skipped.is_empty() {
            let n = st.skipped.len();
            let mut parts = vec![words(format!(
                "Loading went on broken. {} that need{} it {} skipped:",
                if n == 1 { "1 mod".to_string() } else { format!("{n} mods") },
                if n == 1 { "s" } else { "" },
                if n == 1 { "was" } else { "were" },
            ))];
            parts.extend(st.skipped.iter().enumerate().map(|(i, f)| link(f, &format!("skip{i}"), cx)));
            steps.push((false, sentence(parts).into_any_element()));
        }
        let knock: Vec<&str> = c
            .suspects
            .iter()
            .filter(|s| s.certainty == Certainty::Possible)
            .map(|s| s.file_name.as_str())
            .collect();
        if !knock.is_empty() {
            let error = c.headline.split_once(" · ").map_or(c.headline.as_str(), |(_, e)| e).to_string();
            let mut parts = vec![words("Then the game crashed in")];
            parts.extend(knock.iter().enumerate().map(|(i, f)| link(f, &format!("knock{i}"), cx)));
            parts.push(words("(Possible). That's the report's own error, a knock-on effect:"));
            steps.push((
                false,
                v_flex()
                    .gap_0p5()
                    .text_color(theme.muted_foreground)
                    .child(sentence(parts))
                    .when(!error.is_empty(), |d| d.child(widgets::caption(error, cx).text_xs().whitespace_normal()))
                    .into_any_element(),
            ));
        }
        let rows = steps.into_iter().enumerate().map(|(i, (cause, body))| {
            h_flex()
                .gap_2()
                .items_start()
                .py_0p5()
                .child(
                    div()
                        .flex_none()
                        .size_5()
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_xs()
                        .bg(if cause { theme.danger } else { theme.muted_foreground.opacity(0.2) })
                        .text_color(if cause { gpui_kit::white() } else { theme.muted_foreground })
                        .child((i + 1).to_string()),
                )
                .child(div().flex_1().min_w_0().text_sm().child(body))
        });

        v_flex()
            .gap_2()
            .child(title)
            .when(has_fix, |d| d.child(fix))
            .child(div().pt_1().font_semibold().text_sm().child("What happened"))
            .child(v_flex().gap_1().children(rows))
            .into_any_element()
    }
}
