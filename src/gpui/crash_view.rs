//! The parts of a crash card both cards share: the game's (Logs & crashes,
//! `logs.rs` / `logs/crash_story.rs`) and a server's (`rigsync.rs`), so
//! they read the same: the certainty tags, a mod's row, and the story when
//! a mod stopped loading and the crash came after. See
//! design/crash-detection.md. Each card brings its own mod names (`link`)
//! and its own fixes.

use crate::widgets;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::crashdetect::{Certainty, Changed, Crash, Stopped, Suspect};

/// A mod by its jar, with a key unique on the card: its icon and name,
/// opening the mod peek.
pub type Link<'a> = &'a dyn Fn(&str, &str, &App) -> AnyElement;

/// Words and mod names in one line that wraps between them.
pub fn sentence(parts: Vec<AnyElement>) -> Div {
    h_flex().flex_wrap().items_center().gap_x_1().gap_y_0p5().children(parts)
}

pub fn words(t: impl Into<SharedString>) -> AnyElement {
    div().child(t.into()).into_any_element()
}

/// "game" or "server", for the card's sentences.
pub fn what(c: &Crash) -> &'static str {
    if c.server { "server" } else { "game" }
}

/// The Certain / Likely / Possible tag.
pub fn tag(c: Certainty, cx: &App) -> AnyElement {
    let color = match c {
        Certainty::Certain => cx.theme().danger,
        Certainty::Likely => cx.theme().warning,
        Certainty::Possible => cx.theme().muted_foreground,
    };
    widgets::chip(color).xsmall().outline().child(c.label()).into_any_element()
}

/// One mod in the list: its name, how sure, and why (with anything under
/// it: what it needs, a button).
pub fn suspect_row(link: AnyElement, s: &Suspect, why: String, below: Vec<AnyElement>, cx: &App) -> Div {
    h_flex()
        .gap_3()
        .items_start()
        .py_1()
        .child(div().w(rems(16.)).flex_none().text_sm().font_semibold().child(link))
        .child(div().flex_none().w(rems(5.)).child(tag(s.certainty, cx)))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(widgets::caption(why, cx).text_xs().whitespace_normal())
                .children(below),
        )
}

/// "<mod> stopped the game from loading [Certain]".
pub fn story_title(c: &Crash, st: &Stopped, link: Link, cx: &App) -> Div {
    let words_ = if c.server { "stopped the server from starting" } else { "stopped the game from loading" };
    sentence(vec![link(&st.file_name, "title", cx), words(words_), tag(Certainty::Certain, cx)]).text_sm()
}

/// The tinted box the fixes sit in.
pub fn fix_box(cx: &App) -> Div {
    let theme = cx.theme();
    v_flex()
        .gap_2()
        .p_3()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.primary.opacity(0.35))
        .bg(theme.primary.opacity(0.07))
}

/// " (2.6.1 → 2.6.2)" when both versions are known.
pub fn versions(ch: &Changed) -> String {
    if ch.from.is_empty() || ch.to.is_empty() { String::new() } else { format!(" ({} → {})", ch.from, ch.to) }
}

/// "What happened", in order: the update the error names (`changed`, with
/// the jar as it's named now), the mod's own error, the mods skipped
/// because of it, and the report's error as a knock-on effect.
pub fn what_happened(c: &Crash, st: &Stopped, changed: Option<(&Changed, &str)>, link: Link, cx: &App) -> AnyElement {
    let theme = cx.theme().clone();
    let thing = what(c);
    let mut steps: Vec<(bool, AnyElement)> = Vec::new();
    if let Some((ch, file)) = changed {
        let when = hopper::time::relative_secs(ch.when);
        let w = if ch.from.is_empty() || ch.to.is_empty() {
            format!("was updated {when}.")
        } else {
            format!("was updated from {} to {}, {when}.", ch.from, ch.to)
        };
        steps.push((false, sentence(vec![link(file, "s1", cx), words(w)]).into_any_element()));
    }
    steps.push((
        true,
        v_flex()
            .gap_0p5()
            .child(sentence(vec![
                link(&st.file_name, "s2", cx),
                tag(Certainty::Certain, cx),
                words(if st.error.is_empty() {
                    format!("failed while the {thing} was loading.")
                } else {
                    "stopped loading with this error:".to_string()
                }),
            ]))
            .when(!st.error.is_empty(), |d| d.child(widgets::caption(format!("\"{}\"", st.error), cx).text_xs().whitespace_normal()))
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
    let knock: Vec<&str> = c.suspects.iter().filter(|s| s.certainty == Certainty::Possible).map(|s| s.file_name.as_str()).collect();
    if !knock.is_empty() {
        let error = c.headline.split_once(" · ").map_or(c.headline.as_str(), |(_, e)| e).to_string();
        let mut parts = vec![words(format!("Then the {thing} crashed in"))];
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
        .child(div().pt_1().font_semibold().text_sm().child("What happened"))
        .child(v_flex().gap_1().children(rows))
        .into_any_element()
}

/// The note for a crash that names no mod.
pub fn note(c: &Crash) -> Option<String> {
    use hopper::crashdetect::CrashKind;
    // A cause that isn't one mod, said by the crash itself, comes first.
    if !c.advice.is_empty() {
        return Some(c.advice.clone());
    }
    let n = match (c.kind, c.server) {
        (CrashKind::Mods, _) => None,
        (CrashKind::Memory, false) => Some("Java ran out of memory. Give this instance more in Settings › Memory."),
        (CrashKind::Memory, true) => Some("Java ran out of memory. Give the server more in its Java settings."),
        (CrashKind::Native, false) => Some("Java itself crashed. Try another Java in Settings › Java, or update your graphics driver."),
        (CrashKind::Native, true) => Some("Java itself crashed. Try another Java in the server's Java settings."),
        (CrashKind::Unknown, _) => Some("Hopper couldn't tell which mod. The crash report has the details."),
    };
    n.map(str::to_string)
}
