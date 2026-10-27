//! The crash card's narrower fix: the blamed mod's own setting that turns
//! off just the item the crash names (`fe_seed = true`), with the warning
//! that items already in worlds go away. See configswitch.rs.

use super::LogsView;
use super::crash_story::{mod_ref, sentence, words};
use crate::config_editor::{self, Focus};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::component::StyledExt as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::configswitch::Switch;
use hopper::crashdetect::Crash;
use std::path::Path;

/// What turning `label` off does to the items already in worlds.
fn deletes(label: &str) -> String {
    format!(
        "Every {label} already in your worlds goes away: in chests, inventories and planted crops, the next time a world loads. \
         Back up your worlds first if you want to keep them. This works around the crash until the mod is fixed."
    )
}

impl LogsView {
    /// The card's config switches, if the crash's mods have one for the item
    /// it names. `inside`: in the fix box, `Some(true)` under other fixes (a
    /// rule between); `None`: a box of its own.
    pub(super) fn config_switches(&mut self, root: &Path, c: &Crash, inside: Option<bool>, cx: &mut Context<Self>) -> Option<AnyElement> {
        if c.switches.is_empty() && c.switched_off.is_empty() {
            return None;
        }
        let theme = cx.theme().clone();
        let running = self.store.read(cx).running(root).is_some();
        // "Or", when the card also offers turning the whole mod off.
        let or = self.crash_actions(root, c, cx).is_some();
        let mut list = v_flex().gap_3();
        for (i, sw) in c.switches.iter().enumerate() {
            let jar = c.suspects.iter().find(|s| s.mod_id == sw.mod_id).map(|s| s.file_name.clone()).unwrap_or_default();
            let who = if jar.is_empty() { words(sw.mod_id.clone()) } else { mod_ref(&self.store, root, &jar, &format!("switch-{i}"), cx) };
            let file = sw.file.rsplit('/').next().unwrap_or(&sw.file).to_string();
            let (s_off, s_open, r_off, r_open) = (sw.clone(), sw.clone(), root.to_path_buf(), root.to_path_buf());
            list = list.child(
                v_flex()
                    .gap_1p5()
                    .child(
                        sentence(vec![
                            words(if or { "Or keep" } else { "Keep" }),
                            who,
                            words("and turn off only the item that broke:"),
                            div()
                                .px_1p5()
                                .rounded(theme.radius)
                                .bg(theme.muted)
                                .font_family("Consolas")
                                .text_xs()
                                .child(format!("{} = true", sw.key))
                                .into_any_element(),
                            widgets::caption(format!("in {file}, line {}", sw.line), cx).text_xs().into_any_element(),
                        ])
                        .text_sm(),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                Button::new(("crash-switch-off", i))
                                    .small()
                                    .outline()
                                    .icon(IconName::PowerOff)
                                    .label(format!("Turn off {}…", sw.label))
                                    .disabled(running)
                                    .tooltip(if running {
                                        "Close the game first: it may write its config back when it closes."
                                    } else {
                                        "Sets it to false after you confirm. The old config goes to Backups."
                                    })
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        confirm_off(this.store.clone(), r_off.clone(), s_off.clone(), window, cx)
                                    })),
                            )
                            .child(
                                Button::new(("crash-switch-open", i))
                                    .small()
                                    .ghost()
                                    .icon(IconName::FileText)
                                    .label("Open config")
                                    .tooltip("Opens the file in Hopper's config editor, at this setting")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        let focus = Focus { line: s_open.line, note: Some(deletes(&s_open.label)) };
                                        config_editor::open(
                                            this.store.clone(),
                                            r_open.clone(),
                                            vec![s_open.file.clone()],
                                            s_open.file.clone(),
                                            Some(focus),
                                            window,
                                            cx,
                                        )
                                    })),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_start()
                            .px_2()
                            .py_1p5()
                            .rounded(theme.radius)
                            .bg(theme.warning.opacity(0.08))
                            .text_xs()
                            .child(Icon::new(IconName::TriangleAlert).xsmall().flex_none().mt_0p5().text_color(theme.warning))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .whitespace_normal()
                                    .child(div().font_semibold().text_color(theme.warning).child(format!("Existing {} will be deleted.", sw.label)))
                                    .child(deletes(&sw.label)),
                            ),
                    ),
            );
        }
        // Turned off already: say so, play, or put it back.
        for (i, sw) in c.switched_off.iter().enumerate() {
            let (s_on, r_on, r_play) = (sw.clone(), root.to_path_buf(), root.to_path_buf());
            let file = sw.file.rsplit('/').next().unwrap_or(&sw.file).to_string();
            list = list.child(
                v_flex()
                    .gap_1p5()
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(widgets::done_mark(("crash-switch-done", i), format!("{} is off", sw.label), false, cx))
                            .child(widgets::caption(format!("{} = false in {file}. Play to check the crash is gone.", sw.key), cx).text_xs()),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new(("crash-switch-play", i))
                                    .small()
                                    .success()
                                    .icon(IconName::Play)
                                    .label("Play")
                                    .disabled(running)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        let native = this.store.read(cx).settings().launch_native;
                                        if native && !hopper::msauth::has_saved_login() {
                                            crate::dialogs::open_sign_in(this.store.clone(), window, cx);
                                            return;
                                        }
                                        if let Err(e) = this.store.update(cx, |s, _| s.play(&r_play)) {
                                            window.push_notification(Notification::warning(e), cx);
                                        }
                                    })),
                            )
                            .child(
                                Button::new(("crash-switch-on", i))
                                    .small()
                                    .ghost()
                                    .icon(IconName::Undo2)
                                    .label("Turn it back on")
                                    .disabled(running)
                                    .tooltip(format!("Sets {} = true again. The config as it is now goes to Backups.", s_on.key))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        let r = this.store.update(cx, |s, cx| {
                                            let r = s.crash_switch_on(&r_on, &s_on);
                                            cx.notify();
                                            r
                                        });
                                        if let Err(e) = r {
                                            window.push_notification(Notification::warning(e), cx);
                                        }
                                    })),
                            ),
                    ),
            );
        }
        Some(if let Some(rule) = inside {
            div()
                .when(rule, |d| d.pt_2().border_t_1().border_color(theme.primary.opacity(0.25)))
                .child(list)
                .into_any_element()
        } else {
            div()
                .p_3()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.primary.opacity(0.35))
                .bg(theme.primary.opacity(0.07))
                .child(list)
                .into_any_element()
        })
    }
}

/// Asks, with the warning, then turns the switch off.
fn confirm_off(store: Entity<crate::store::Store>, root: std::path::PathBuf, sw: Switch, window: &mut Window, cx: &mut App) {
    let file = sw.file.rsplit('/').next().unwrap_or(&sw.file).to_string();
    window.open_alert_dialog(cx, move |alert, _, _| {
        let (store, root, sw) = (store.clone(), root.clone(), sw.clone());
        alert
            .title(format!("Turn off {}?", sw.label))
            .description(format!(
                "Sets {} = false in {file}. {} The config as it was is kept in Backups, so you can undo this.",
                sw.key,
                deletes(&sw.label)
            ))
            .confirm()
            .ok_text(format!("Turn off {}", sw.label))
            .ok_variant(ButtonVariant::Danger)
            .on_ok(move |_, window, cx| {
                let r = store.update(cx, |s, cx| {
                    let r = s.crash_switch_off(&root, &sw);
                    cx.notify();
                    r
                });
                match r {
                    Ok(()) => window.push_notification(Notification::success(format!("{} is off. Play to check the crash is gone.", sw.label)), cx),
                    Err(e) => window.push_notification(Notification::warning(e), cx),
                }
                true
            })
    });
}
