//! What's new: the notes of the releases since the last one shown, once
//! after an update, and on demand from Settings → Hopper updates.

use crate::app::AppView;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::changelog::Entry;
use hopper::selfupdate::{CURRENT, is_newer};

/// Shows What's new after an update, once per version. A first start
/// (or one before this existed) only notes the version.
/// Whether it opened.
pub fn on_start(app: &AppView, window: &mut Window, cx: &mut Context<AppView>) -> bool {
    let store = app.store.clone();
    let (seen, setup) = {
        let s = store.read(cx).settings();
        (s.seen_version.clone(), s.needs_setup())
    };
    // --whats-new[=N] shows the last N releases (3) whatever was seen.
    let preview = std::env::args().find_map(|a| match a.strip_prefix("--whats-new") {
        Some("") => Some(3),
        Some(n) => n.strip_prefix('=').and_then(|n| n.parse().ok()),
        None => None,
    });
    if preview.is_none() && !seen.is_empty() && !is_newer(CURRENT, &seen) {
        return false;
    }
    store.update(cx, |s, cx| s.update_settings(|st| st.seen_version = CURRENT.to_string(), cx));
    let entries = if let Some(n) = preview {
        hopper::changelog::all().into_iter().take(n).collect()
    } else if seen.is_empty() || setup {
        return false;
    } else {
        hopper::changelog::since(&seen)
    };
    if entries.is_empty() {
        return false;
    }
    let repo = store.read(cx).settings().update_repo.clone();
    cx.defer_in(window, move |_, window, cx| {
        open(entries, format!("Hopper {CURRENT}: what's new"), repo, window, cx)
    });
    true
}

/// The notes of `entries` (newest first) as a dialog: the newest open,
/// the rest behind "Older releases".
pub fn open(entries: Vec<Entry>, title: String, repo: String, window: &mut Window, cx: &mut App) {
    let older_open = cx.new(|_| false);
    window.open_dialog(cx, move |dialog, window, cx| {
        let theme = cx.theme().clone();
        let show_older = *older_open.read(cx);
        let visible: Vec<&Entry> = entries.iter().take(if show_older { entries.len() } else { 1 }).collect();
        let shown = |e: &Entry| -> Vec<String> { e.paragraphs.clone() };
        // GPUI scrolls only a set height, so a list that may not fit gets
        // the window's height and scrolls; a short one (a rough, generous
        // guess at its height) keeps its own height and never clips.
        let rem = window.rem_size();
        let max_h = (window.viewport_size().height - rem * 12.).max(rem * 12.);
        let guess: f32 = visible
            .iter()
            .map(|e| 5. + shown(e).iter().map(|p| (p.len() as f32 / 70.).ceil() + 0.5).sum::<f32>() * 1.5)
            .sum();
        let scrolls = rem * guess > max_h;
        let cards = visible.iter().map(|e| {
            let this = e.version == CURRENT;
            let paragraphs = shown(e);
            v_flex()
                .gap_1p5()
                .p_3()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(if this { theme.primary } else { theme.border })
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().font_semibold().child(format!("Hopper {}", e.version)))
                        .when(this, |r| {
                            r.child(widgets::chip(cx.theme().primary).xsmall().child("This version"))
                        }),
                )
                .when(paragraphs.is_empty(), |c| c.child(widgets::caption("No notes for this release.", cx)))
                .children(paragraphs.into_iter().map(|p| div().text_sm().child(p)))
        });
        let older_count = entries.len().saturating_sub(1);
        let older_toggle = (older_count > 0).then(|| {
            let o = older_open.clone();
            Button::new("whats-new-older")
                .small()
                .ghost()
                .icon(if show_older { IconName::ChevronUp } else { IconName::ChevronDown })
                .label(if show_older { "Hide older releases".to_string() } else { format!("Older releases ({older_count})") })
                .on_click(move |_, window, cx| {
                    o.update(cx, |v, _| *v = !*v);
                    window.refresh();
                })
        });
        let releases = format!("https://github.com/{}/releases", repo.trim().trim_matches('/'));
        dialog
            .title(title.clone())
            .w(rems(40.).to_pixels(window.rem_size()))
            .child(
                v_flex()
                    .gap_3()
                    .map(|c| {
                        let list = v_flex().gap_3().children(cards).children(older_toggle.map(|b| h_flex().child(b)));
                        if scrolls {
                            // Room on the right so the scrollbar doesn't sit on the cards' border.
                            c.child(
                                v_flex().h(max_h).child(
                                    div()
                                        .id("whats-new")
                                        .flex_1()
                                        .min_h_0()
                                        .overflow_y_scrollbar()
                                        .child(list.pr_3().pb_px()),
                                ),
                            )
                        } else {
                            c.child(list)
                        }
                    })
                    .child(
                        h_flex().justify_end().child(
                            Button::new("whats-new-all")
                                .small()
                                .ghost()
                                .icon(IconName::ExternalLink)
                                .label("All releases on GitHub")
                                .on_click({
                                    let releases = releases.clone();
                                    move |_, _, cx| cx.open_url(&releases)
                                }),
                        ),
                    ),
            )
    });
}

/// The last releases' notes, from Settings.
pub fn open_recent(repo: String, window: &mut Window, cx: &mut App) {
    let entries: Vec<Entry> = hopper::changelog::all().into_iter().take(20).collect();
    open(entries, "What's new in Hopper".into(), repo, window, cx)
}
