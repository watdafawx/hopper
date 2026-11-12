//! The parts of a crash card both cards share: the game's (Logs & crashes,
//! `logs.rs` / `logs/crash_story.rs`) and a server's (`rigsync.rs`), so
//! they read the same: the certainty tags, a mod's row, and the story when
//! a mod stopped loading and the crash came after. See
//! design/crash-detection.md. Each card brings its own mod names (`link`)
//! and its own fixes.

use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, h_flex, v_flex};
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

/// Small muted text that wraps (`widgets::caption` keeps to one line).
pub fn note_text(t: impl Into<SharedString>, cx: &App) -> Div {
    div().text_xs().text_color(cx.theme().muted_foreground).whitespace_normal().child(t.into())
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

/// A suspect's tag: how sure, or "Knock-on" for an error that only
/// followed from another mod's failure (ignore it).
pub fn suspect_tag(s: &Suspect, cx: &App) -> AnyElement {
    if s.knock_on {
        return widgets::chip(cx.theme().muted_foreground)
            .xsmall()
            .outline()
            .child("Knock-on")
            .into_any_element();
    }
    tag(s.certainty, cx)
}

/// "Made for another version of <mod> 1.18.0": the other mod a suspect's
/// error is about, by its link.
pub fn partner_line(s: &Suspect, link: Link, cx: &App) -> Option<AnyElement> {
    let p = s.partner.as_ref()?;
    let mut parts = vec![words("Doesn't match")];
    parts.push(link(&p.file_name, "partner", cx));
    if !p.version.is_empty() {
        parts.push(words(format!("{} (installed)", p.version)));
    }
    Some(sentence(parts).text_xs().into_any_element())
}

/// The suspect's next steps, numbered: what to update, change or remove.
/// `readable` turns mod ids in them into names.
pub fn steps_list(s: &Suspect, mods_dir: &std::path::Path, newer: Option<bool>, readable: &dyn Fn(&str) -> String, cx: &App) -> Option<AnyElement> {
    let steps = hopper::crashdetect::steps_shown(s, mods_dir, newer);
    if steps.is_empty() {
        return None;
    }
    let theme = cx.theme();
    let rows = steps.iter().enumerate().map(|(i, t)| {
        h_flex()
            .gap_1p5()
            .items_start()
            .text_xs()
            .child(div().flex_none().w(rems(1.)).text_color(theme.muted_foreground).child(format!("{}.", i + 1)))
            .child(div().flex_1().min_w_0().whitespace_normal().child(readable(t)))
    });
    Some(
        v_flex()
            .gap_0p5()
            .child(div().text_xs().font_semibold().text_color(theme.muted_foreground).child("What to do"))
            .children(rows)
            .into_any_element(),
    )
}

/// Whether a newer version of a mod is out, from the instance's last
/// update check: None when there's been no check or the mod isn't from a
/// site, so an "Update it" step stays.
pub fn newer_out(updates: Option<&crate::store::Updates>, project_id: u64) -> Option<bool> {
    let list = updates?.ready()?;
    (project_id != 0).then(|| list.iter().any(|u| u.project_id == project_id))
}

/// The mods "Turn off…" would turn off, before it does: each with a tick
/// box, its name (icon and link), how sure and why, then a confirm. `picked`
/// holds the jars ticked. Nothing is turned off until the confirm button.
#[allow(clippy::too_many_arguments)]
pub fn turn_off_picker(
    key: &str,
    suspects: &[&Suspect],
    picked: &std::collections::HashSet<String>,
    link: Link,
    readable: &dyn Fn(&str) -> String,
    toggle: std::rc::Rc<dyn Fn(String, bool, &mut Window, &mut App)>,
    confirm: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cancel: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    blocked: bool,
    what: &str,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let n = suspects.iter().filter(|s| picked.contains(&s.file_name)).count();
    let rows = suspects.iter().enumerate().map(|(i, s)| {
        let file = s.file_name.clone();
        let on = picked.contains(&file);
        let t = toggle.clone();
        h_flex()
            .gap_2()
            .items_start()
            .py_1()
            .child(
                gpui_kit::component::checkbox::Checkbox::new(SharedString::from(format!("{key}-pick-{i}")))
                    .checked(on)
                    .on_change(move |v: &bool, w, cx| t(file.clone(), *v, w, cx)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(sentence(vec![link(&s.file_name, &format!("pick{i}"), cx), suspect_tag(s, cx)]).text_sm())
                    .child(crate::crash_view::note_text(readable(&s.why), cx)),
            )
    });
    let label = match n {
        0 => "Turn off".to_string(),
        1 => "Turn off 1 mod".to_string(),
        n => format!("Turn off {n} mods"),
    };
    v_flex()
        .gap_1()
        .p_3()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .child(div().text_sm().font_semibold().child("Turn off these mods?"))
        .child(note_text(format!("Untick any to keep. Mods that need them stop loading too. Turn them back on in the {what}'s Mods."), cx))
        .children(rows)
        .child(
            h_flex()
                .gap_2()
                .pt_1()
                .child(
                    Button::new(SharedString::from(format!("{key}-pick-confirm")))
                        .small()
                        .danger()
                        .icon(IconName::PowerOff)
                        .label(label)
                        .disabled(blocked || n == 0)
                        .on_click(confirm),
                )
                .child(Button::new(SharedString::from(format!("{key}-pick-cancel"))).small().ghost().label("Cancel").on_click(cancel)),
        )
        .into_any_element()
}

/// One mod in the list: its name, how sure, and why (with anything under
/// it: what it needs, a button).
pub fn suspect_row(link: AnyElement, s: &Suspect, why: String, below: Vec<AnyElement>, cx: &App) -> Div {
    h_flex()
        .gap_3()
        .items_start()
        .py_1()
        .when(s.knock_on, |d| d.opacity(0.75))
        .child(div().w(rems(16.)).flex_none().text_sm().font_semibold().child(link))
        .child(div().flex_none().w(rems(5.)).child(suspect_tag(s, cx)))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(crate::crash_view::note_text(why, cx))
                .children(below),
        )
}

/// "<mod> stopped the game from loading [Certain]".
/// The "Why?" fold under a mod's reason: what Hopper weighed for and
/// against it, strongest first, and where it sits in the error's trace
/// (see `hopper::crashengine`). `None` when there is nothing beyond the
/// reason itself (a card saved before Hopper kept these). `readable` turns
/// mod ids in the sentences into names, as the reason does.
pub fn why_fold(
    id: ElementId,
    s: &Suspect,
    open: bool,
    readable: &dyn Fn(&str) -> String,
    toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Option<AnyElement> {
    // What was weighed, less what the sentence above already says.
    let facts: Vec<_> = s.facts.iter().filter(|f| !s.why.contains(&f.text)).collect();
    if s.facts.len() < 2 || facts.is_empty() {
        return None;
    }
    let theme = cx.theme();
    let button = Button::new(id)
        .xsmall()
        .ghost()
        .icon(if open { IconName::ChevronDown } else { IconName::ChevronRight })
        .label(if open { "Hide why" } else { "Why?" })
        .on_click(toggle);
    let mut col = v_flex().gap_1().child(h_flex().child(button));
    if open {
        let rows = facts.into_iter().map(|f| {
            let (mark, color) = match f.weight {
                w if w > 0 => ("+", theme.success),
                w if w < 0 => ("−", theme.danger),
                _ => ("·", theme.muted_foreground),
            };
            h_flex()
                .gap_2()
                .items_start()
                .text_xs()
                .child(div().w(rems(0.75)).flex_none().text_color(color).child(mark))
                .child(div().flex_1().min_w_0().whitespace_normal().text_color(theme.muted_foreground).child(readable(&f.text)))
        });
        col = col.child(v_flex().gap_0p5().px_2().py_1p5().rounded(theme.radius).bg(theme.list_hover).children(rows));
    }
    Some(col.into_any_element())
}

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
    // Mods the loader named only because they read the stopped mod's value.
    let readers: Vec<&str> = c.suspects.iter().filter(|s| hopper::crashdetect::only_read(s)).map(|s| s.file_name.as_str()).collect();
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
            .when(!st.error.is_empty(), |d| d.child(crate::crash_view::note_text(format!("\"{}\"", st.error), cx)))
            .children(readers.iter().enumerate().map(|(i, f)| {
                sentence(vec![link(f, &format!("read{i}"), cx), words("was reading it when it failed."), tag(Certainty::Possible, cx)])
                    .text_color(theme.muted_foreground)
            }))
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
        .filter(|s| (s.knock_on || s.certainty == Certainty::Possible) && !hopper::crashdetect::only_read(s) && s.file_name != st.file_name)
        .map(|s| s.file_name.as_str())
        .collect();
    // The step the knock-on followed from: the mod's own error.
    let cause = if changed.is_some() { 2 } else { 1 };
    if !knock.is_empty() {
        let error = c.headline.split_once(" · ").map_or(c.headline.as_str(), |(_, e)| e).to_string();
        let mut parts = vec![words(format!("Then the {thing} crashed in"))];
        parts.extend(knock.iter().enumerate().map(|(i, f)| link(f, &format!("knock{i}"), cx)));
        parts.push(words(format!("(knock-on, ignore). That's the report's own error, which only followed from step {cause}:")));
        steps.push((
            false,
            v_flex()
                .gap_0p5()
                .text_color(theme.muted_foreground)
                .child(sentence(parts))
                .when(!error.is_empty(), |d| d.child(crate::crash_view::note_text(error, cx)))
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
                    .min_w(rems(1.25)).h(rems(1.25)).px_1().whitespace_nowrap()
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
