//! The History button in an instance's header: an icon-only button that
//! opens History, with a badge counting what was added since History was
//! last opened for that instance, and a short turn of the icon when a new
//! entry lands.
//!
//! It is its own small view, so the polling and the animation redraw only
//! this button, never the whole header.

use crate::store::Store;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How often the instance's History folder is looked at while its page is
/// open. A directory listing, so cheap.
const POLL: Duration = Duration::from_secs(3);
/// The turn of the icon and the ring when something lands.
const PULSE: Duration = Duration::from_millis(550);

fn seen_file() -> Option<PathBuf> {
    hopper::paths::dirs().map(|d| d.data_dir().join("history_seen.json"))
}

fn read_seen() -> HashMap<String, i64> {
    seen_file()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn key(root: &Path) -> String {
    root.to_string_lossy().to_lowercase()
}

/// Notes that History was looked at: everything in it up to now is seen.
/// Also called when the dialog opens from the ⋯ menu.
pub fn mark_seen(root: &Path) {
    let (_, newest) = hopper::backups::count_since(root, i64::MAX);
    let mut m = read_seen();
    m.insert(key(root), newest);
    if let (Some(p), Ok(b)) = (seen_file(), serde_json::to_vec(&m)) {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, b);
    }
}

/// What's new in `root`'s History. An instance never looked at starts at
/// nothing new, not at its whole past.
fn new_entries(root: &Path) -> usize {
    match read_seen().get(&key(root)) {
        Some(&since) => hopper::backups::count_since(root, since).0,
        None => {
            mark_seen(root);
            0
        }
    }
}

pub struct HistoryButton {
    store: Entity<Store>,
    root: PathBuf,
    count: usize,
    /// Bumped when the count grows, to run the animation once.
    pulse: u64,
    _poll: Task<()>,
}

impl HistoryButton {
    pub fn new(store: Entity<Store>, root: PathBuf, cx: &mut Context<Self>) -> Self {
        let r = root.clone();
        let poll = cx.spawn(async move |this, cx| {
            let mut first = true;
            loop {
                let r2 = r.clone();
                let n = cx.background_spawn(async move { new_entries(&r2) }).await;
                let alive = this
                    .update(cx, |t, cx| {
                        // The first read sets the count without a pulse.
                        if !first && n > t.count && !cx.reduce_motion() {
                            t.pulse += 1;
                        }
                        first = false;
                        if n != t.count {
                            t.count = n;
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
                cx.background_executor().timer(POLL).await;
            }
        });
        Self { store, root, count: 0, pulse: 0, _poll: poll }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl Render for HistoryButton {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (count, pulse, primary) = (self.count, self.pulse, theme.primary);
        let tip = if count > 0 { format!("History ({count} new)") } else { "History".to_string() };
        let store = self.store.clone();
        let root = self.root.clone();
        div()
            .id("history-button")
            .relative()
            .flex_none()
            // The same outline icon button as the more menu beside it.
            .child(
                Button::new("history")
                    .outline()
                    .icon(IconName::RotateCcwClock)
                    .tooltip(tip)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        mark_seen(&root);
                        this.count = 0;
                        cx.notify();
                        crate::backups_view::open(store.clone(), root.clone(), window, cx);
                    })),
            )
            .when(count > 0, |d| {
                let label = if count > 99 { "99+".to_string() } else { count.to_string() };
                d.child(
                    div()
                        .absolute()
                        .top(px(1.))
                        .right(px(-2.))
                        .min_w(px(14.))
                        .h(px(14.))
                        .px(px(3.))
                        .rounded_full()
                        .bg(primary)
                        .text_color(theme.primary_foreground)
                        .text_xs()
                        .font_semibold()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(label),
                )
            })
            // A ring that swells and fades with the turn.
            .when(pulse > 0, |d| {
                d.child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(theme.radius)
                        .border_2()
                        .border_color(primary)
                        .with_animation(
                            ElementId::Name(format!("history-ring-{pulse}").into()),
                            Animation::new(PULSE).with_easing(ease_out_quint()),
                            move |el, t| el.opacity(1.0 - t).border_color(primary.opacity(1.0 - t)),
                        ),
                )
            })
    }
}
