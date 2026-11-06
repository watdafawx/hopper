//! The crash card's narrower fix: a setting in the crash's mods' own
//! configs that works around it and keeps the mod (crashconfig.rs finds
//! them). A mixin option, a mod's support for another mod or a limit is
//! the card's first option: one click changes it, then the card shows the
//! change and can undo it. Turning off an item (`fe_seed = true`) deletes
//! the ones in worlds, so that one asks first, with the warning.

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
use hopper::configswitch::{Switch, SwitchKind};
use hopper::crashdetect::Crash;
use std::path::Path;

/// What turning `label` off does to the items already in worlds.
fn deletes(label: &str) -> String {
    format!(
        "Every {label} already in your worlds goes away: in chests, inventories and planted crops, the next time a world loads. \
         Back up your worlds first if you want to keep them. This works around the crash until the mod is fixed."
    )
}

/// Whether the card's settings come before turning mods off: a setting that
/// keeps the whole mod and loses nothing.
/// A changed one keeps its place, so the card doesn't jump after the click.
pub(crate) fn settings_first(c: &Crash) -> bool {
    c.switches.iter().chain(&c.switched_off).any(|s| s.kind != SwitchKind::Item)
}

/// The jar of mod `id`: a suspect's, else one in the mods folder named after it.
fn jar_of(store: &Entity<crate::store::Store>, root: &Path, c: &Crash, id: &str, cx: &App) -> Option<String> {
    if let Some(s) = c.suspects.iter().find(|s| s.mod_id == id) {
        return Some(s.file_name.clone());
    }
    let sq = |s: &str| s.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect::<String>();
    let want = sq(id);
    let st = store.read(cx);
    let inst = st.instance(root)?;
    inst.addons.iter().find(|a| sq(&a.file_name).starts_with(&want)).map(|a| a.file_name.clone())
}

/// The lines that changed, red and green.
fn diff_box(file: &str, lines: &[(bool, String)], cx: &App) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .gap_0p5()
        .px_2()
        .py_1p5()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .font_family("Consolas")
        .text_xs()
        .child(div().text_color(theme.muted_foreground).child(file.to_string()))
        .children(lines.iter().map(|(new, l)| {
            let (sign, color) = if *new { ("+ ", theme.success) } else { ("- ", theme.danger) };
            div()
                .px_1()
                .rounded(crate::theme::radius_sm())
                .bg(color.opacity(0.08))
                .text_color(color)
                .whitespace_normal()
                .child(format!("{sign}{}", l.trim()))
        }))
        .into_any_element()
}

/// Where the card is and what its buttons do: an instance's crash card, or
/// a server's (on this PC or on a Hopper Server).
pub(crate) struct SwitchHost {
    /// The game or server is running: the buttons wait.
    pub running: bool,
    /// Why they wait.
    pub running_tip: &'static str,
    /// After a change: "Play to check the crash is gone."
    pub check: &'static str,
    pub off: std::rc::Rc<dyn Fn(Switch, &mut Window, &mut App)>,
    pub on: std::rc::Rc<dyn Fn(Switch, &mut Window, &mut App)>,
    /// Opens the file at the setting; `None` where there is no editor for it.
    pub open: Option<std::rc::Rc<dyn Fn(Switch, &mut Window, &mut App)>>,
    /// Play after a change (an instance's card only).
    pub play: Option<std::rc::Rc<dyn Fn(&mut Window, &mut App)>>,
}

impl LogsView {
    /// The card's config settings, if the crash's mods have one that works
    /// around it. `inside`: in the fix box, `Some(true)` under other fixes
    /// (a rule between); `None`: a box of its own.
    pub(super) fn config_switches(&mut self, root: &Path, c: &Crash, inside: Option<bool>, cx: &mut Context<Self>) -> Option<AnyElement> {
        if c.switches.is_empty() && c.switched_off.is_empty() {
            return None;
        }
        let running = self.store.read(cx).running(root).is_some();
        // "Or", when the card offers turning the whole mod off above.
        let or = !settings_first(c) && self.crash_actions(root, c, cx).is_some();
        let who: Vec<AnyElement> = c
            .switches
            .iter()
            .enumerate()
            .map(|(i, sw)| match jar_of(&self.store, root, c, &sw.mod_id, cx) {
                Some(jar) => mod_ref(&self.store, root, &jar, &format!("switch-{i}"), cx),
                None => words(sw.mod_id.clone()),
            })
            .collect();
        let diffs: Vec<Vec<(bool, String)>> = c.switched_off.iter().map(|sw| hopper::configswitch::applied_diff(root, sw)).collect();
        let (s1, s2, s3, s4, r1, r2, r3, r4) = (
            self.store.clone(),
            self.store.clone(),
            self.store.clone(),
            self.store.clone(),
            root.to_path_buf(),
            root.to_path_buf(),
            root.to_path_buf(),
            root.to_path_buf(),
        );
        let host = SwitchHost {
            running,
            running_tip: "Close the game first: it may write its config back when it closes.",
            check: "Play to check the crash is gone.",
            off: std::rc::Rc::new(move |sw, window, cx| turn_off(s1.clone(), r1.clone(), sw, window, cx)),
            on: std::rc::Rc::new(move |sw, window, cx| {
                let r = s2.update(cx, |s, cx| {
                    let r = s.crash_switch_on(&r2, &sw);
                    cx.notify();
                    r
                });
                if let Err(e) = r {
                    window.push_notification(Notification::warning(e), cx);
                }
            }),
            open: Some(std::rc::Rc::new(move |sw, window, cx| {
                let note = if sw.deletes() { Some(deletes(&sw.label)) } else { Some(sw.why.clone()).filter(|w| !w.is_empty()) };
                let focus = Focus { line: sw.line, note };
                config_editor::open(s3.clone(), r3.clone(), vec![sw.file.clone()], sw.file.clone(), Some(focus), window, cx)
            })),
            play: Some(std::rc::Rc::new(move |window, cx| {
                let native = s4.read(cx).settings().launch_native;
                if native && !hopper::msauth::has_saved_login() {
                    crate::dialogs::open_sign_in(s4.clone(), window, cx);
                    return;
                }
                if let Err(e) = s4.update(cx, |s, _| s.play(&r4)) {
                    window.push_notification(Notification::warning(e), cx);
                }
            })),
        };
        switch_list(c, who, diffs, or, inside, host, cx)
    }
}

/// The card's settings box, shared by instance and server crash cards.
/// `who`: each switch's mod (a link); `diffs`: what each changed one changed.
pub(crate) fn switch_list(c: &Crash, who: Vec<AnyElement>, diffs: Vec<Vec<(bool, String)>>, or: bool, inside: Option<bool>, host: SwitchHost, cx: &mut App) -> Option<AnyElement> {
    if c.switches.is_empty() && c.switched_off.is_empty() {
        return None;
    }
    let theme = cx.theme().clone();
    let running = host.running;
    let first = settings_first(c);
    let mut who = who.into_iter();
    let mut diffs = diffs.into_iter();
    let mut list = v_flex().gap_3();
        for (i, sw) in c.switches.iter().enumerate() {
            let who = who.next().unwrap_or_else(|| words(sw.mod_id.clone()));
            let open = host.open.clone().map(|f| {
                let s_open = sw.clone();
                Button::new(("crash-switch-open", i))
                    .small()
                    .ghost()
                    .icon(IconName::FileText)
                    .label("Open config")
                    .tooltip("Opens the file in Hopper's config editor, at this setting")
                    .on_click(move |_, window, cx| f(s_open.clone(), window, cx))
            });
            if sw.kind != SwitchKind::Item {
                // "Turn off Dynamic resources in [ModernFix] (modernfix-mixins.properties · mixin.perf.dynamic_resources)"
                let (verb, tail) = match sw.kind {
                    SwitchKind::Limit => (format!("Raise {} in", sw.label), format!("to {}", sw.to_text())),
                    _ => (format!("Turn off {} in", sw.label), String::new()),
                };
                let (s_off, off) = (sw.clone(), host.off.clone());
                // One filled button per region: the strongest setting's.
                let mut apply = Button::new(("crash-switch-off", i)).small().icon(if sw.kind == SwitchKind::Limit { IconName::ChevronUp } else { IconName::PowerOff });
                apply = if i == 0 { apply.primary() } else { apply.outline() };
                list = list.child(
                    v_flex()
                        .gap_1p5()
                        .child(
                            sentence(vec![
                                words(verb),
                                who,
                                words(tail),
                                widgets::caption(format!("({} · {})", sw.file_name(), sw.key), cx).text_xs().into_any_element(),
                            ])
                            .text_sm()
                            .font_semibold(),
                        )
                        .when(!sw.why.is_empty(), |d| d.child(crate::crash_view::note_text(sw.why.clone(), cx)))
                        .child(
                            h_flex()
                                .gap_2()
                                .flex_wrap()
                                .child(
                                    apply
                                        .label(sw.action())
                                        .disabled(running)
                                        .tooltip(if running { host.running_tip } else { "Changes this one setting. Undo it from History." })
                                        .on_click(move |_, window, cx| off(s_off.clone(), window, cx)),
                                )
                                .children(open),
                        ),
                );
                continue;
            }
            let file = sw.file_name().to_string();
            let (s_off, off) = (sw.clone(), host.off.clone());
            list = list.child(
                v_flex()
                    .gap_1p5()
                    .child(
                        sentence(vec![
                            words(if or || (first && i > 0) { "Or keep" } else { "Keep" }),
                            who,
                            words("and turn off only the item that broke:"),
                            div()
                                .px_1p5()
                                .rounded(crate::theme::radius_sm())
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
                                    .tooltip(if running { host.running_tip } else { "Sets it to false after you confirm. Undo it from History." })
                                    .on_click(move |_, window, cx| confirm_off(off.clone(), s_off.clone(), window, cx)),
                            )
                            .children(open),
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
        // Changed already: say so with the change, play, or undo.
        for (i, sw) in c.switched_off.iter().enumerate() {
            let (s_on, on) = (sw.clone(), host.on.clone());
            let changed = diffs.next().unwrap_or_default();
            let item = sw.kind == SwitchKind::Item;
            list = list.child(
                v_flex()
                    .gap_1p5()
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(widgets::done_mark(("crash-switch-done", i), sw.done(), false, cx))
                            .child(
                                widgets::caption(
                                    if changed.is_empty() {
                                        format!("{} = {} in {}. {}", sw.key, sw.to_text(), sw.file_name(), host.check)
                                    } else {
                                        format!("{} Undo it from History.", host.check)
                                    },
                                    cx,
                                )
                                .text_xs(),
                            ),
                    )
                    .when(!changed.is_empty(), |d| d.child(diff_box(&sw.file, &changed, cx)))
                    .child(
                        h_flex()
                            .gap_2()
                            .children(host.play.clone().map(|play| {
                                Button::new(("crash-switch-play", i))
                                    .small()
                                    .success()
                                    .icon(IconName::Play)
                                    .label("Play")
                                    .disabled(running)
                                    .on_click(move |_, window, cx| play(window, cx))
                            }))
                            .child(
                                Button::new(("crash-switch-on", i))
                                    .small()
                                    .ghost()
                                    .icon(IconName::Undo2)
                                    .label(if item { "Turn it back on" } else { "Undo" })
                                    .disabled(running)
                                    .tooltip(format!("Sets {} = {} again. Undo it from History.", s_on.key, s_on.from_text()))
                                    .on_click(move |_, window, cx| on(s_on.clone(), window, cx)),
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

impl LogsView {
    /// The card's "turn the mod off" buttons under a setting offered first:
    /// "Or" in front.
    pub(super) fn or_actions(&mut self, root: &Path, c: &Crash, cx: &mut Context<Self>) -> Option<AnyElement> {
        let a = self.crash_actions(root, c, cx)?;
        let theme = cx.theme().clone();
        Some(
            h_flex()
                .pt_2()
                .gap_2()
                .items_start()
                .border_t_1()
                .border_color(theme.primary.opacity(0.25))
                .child(widgets::caption("Or", cx).pt_1())
                .child(div().flex_1().min_w_0().child(a))
                .into_any_element(),
        )
    }
}

/// Changes the setting (one click: it loses nothing and Undo puts it back).
fn turn_off(store: Entity<crate::store::Store>, root: std::path::PathBuf, sw: Switch, window: &mut Window, cx: &mut App) {
    let r = store.update(cx, |s, cx| {
        let r = s.crash_switch_off(&root, &sw);
        cx.notify();
        r
    });
    match r {
        Ok(()) => window.push_notification(Notification::success(format!("{}. Play to check the crash is gone.", sw.done())), cx),
        Err(e) => window.push_notification(Notification::warning(e), cx),
    }
}

/// Asks, with the warning, then turns the item's switch off.
fn confirm_off(off: std::rc::Rc<dyn Fn(Switch, &mut Window, &mut App)>, sw: Switch, window: &mut Window, cx: &mut App) {
    let file = sw.file_name().to_string();
    window.open_alert_dialog(cx, move |alert, _, _| {
        let (off, sw) = (off.clone(), sw.clone());
        alert
            .title(format!("Turn off {}?", sw.label))
            .description(format!(
                "Sets {} = false in {file}. {} You can undo this from History.",
                sw.key,
                deletes(&sw.label)
            ))
            .confirm()
            .ok_text(format!("Turn off {}", sw.label))
            .ok_variant(ButtonVariant::Danger)
            .on_ok(move |_, window, cx| {
                off(sw.clone(), window, cx);
                true
            })
    });
}
