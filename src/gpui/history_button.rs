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

/// Whose History: an instance's folder here, or a server on a Hopper
/// Server panel (asked through the panel).
#[derive(Clone)]
pub enum Source {
    Folder(PathBuf),
    Panel(crate::panel_tabs::Target),
}

impl Source {
    fn key(&self) -> String {
        match self {
            Source::Folder(r) => key(r),
            Source::Panel(t) => format!("panel:{}/{}", t.remote.base, t.id()).to_lowercase(),
        }
    }

    /// (new since `since`, newest). Blocks: call it off the UI thread.
    fn count(&self, since: i64) -> (usize, i64) {
        match self {
            Source::Folder(r) => hopper::backups::count_since(r, since),
            Source::Panel(t) => t
                .client
                .get(&t.sp(&format!("/mod-backups/count?since={since}")))
                .ok()
                .map(|v| (v["new"].as_u64().unwrap_or(0) as usize, v["newest"].as_i64().unwrap_or(0)))
                .unwrap_or((0, 0)),
        }
    }

    fn mark_seen(&self) {
        let (_, newest) = self.count(i64::MAX);
        save_seen(&self.key(), newest);
    }

    /// What's new. Never looked at starts at nothing new, not at its whole past.
    fn new_entries(&self) -> usize {
        match read_seen().get(&self.key()) {
            Some(&since) => self.count(since).0,
            None => {
                self.mark_seen();
                0
            }
        }
    }
}

fn save_seen(k: &str, newest: i64) {
    let mut m = read_seen();
    m.insert(k.to_string(), newest);
    if let (Some(p), Ok(b)) = (seen_file(), serde_json::to_vec(&m)) {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, b);
    }
}

/// Notes that an instance's History was looked at: everything in it up to
/// now is seen. Also called when the dialog opens from anywhere else.
pub fn mark_seen(root: &Path) {
    Source::Folder(root.to_path_buf()).mark_seen();
}

/// [`mark_seen`] for a server on a Hopper Server (asks its panel, so off the
/// UI thread).
pub fn mark_seen_panel(t: &crate::panel_tabs::Target, cx: &mut App) {
    let s = Source::Panel(t.clone());
    cx.background_spawn(async move { s.mark_seen() }).detach();
}

type OnOpen =std::rc::Rc<dyn Fn(&mut Window, &mut App)>;

pub struct HistoryButton {
    source: Source,
    on_open: OnOpen,
    count: usize,
    /// Bumped when the count grows, to run the animation once.
    pulse: u64,
    _poll: Task<()>,
}

impl HistoryButton {
    /// An instance's button: opens its History dialog.
    pub fn new(store: Entity<Store>, root: PathBuf, cx: &mut Context<Self>) -> Self {
        let r = root.clone();
        let open: OnOpen = std::rc::Rc::new(move |window, cx| crate::backups_view::open(store.clone(), r.clone(), window, cx));
        Self::with(Source::Folder(root), open, cx)
    }

    /// A server's button: `on_open` shows its Mods › History.
    pub fn for_panel(t: crate::panel_tabs::Target, on_open: OnOpen, cx: &mut Context<Self>) -> Self {
        Self::with(Source::Panel(t), on_open, cx)
    }

    fn with(source: Source, on_open: OnOpen, cx: &mut Context<Self>) -> Self {
        let r = source.clone();
        let poll = cx.spawn(async move |this, cx| {
            let mut first = true;
            loop {
                let r2 = r.clone();
                let n = cx.background_spawn(async move { r2.new_entries() }).await;
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
        Self { source, on_open, count: 0, pulse: 0, _poll: poll }
    }

    pub fn is_for(&self, root: &Path) -> bool {
        matches!(&self.source, Source::Folder(r) if r == root)
    }

    pub fn is_panel(&self, t: &crate::panel_tabs::Target) -> bool {
        matches!(&self.source, Source::Panel(p) if p == t)
    }
}

impl Render for HistoryButton {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (count, pulse, primary) = (self.count, self.pulse, theme.primary);
        let tip = if count > 0 { format!("History ({count} new)") } else { "History".to_string() };
        let source = self.source.clone();
        let on_open = self.on_open.clone();
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
                        let s = source.clone();
                        cx.background_spawn(async move { s.mark_seen() }).detach();
                        this.count = 0;
                        cx.notify();
                        on_open(window, cx);
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
                        .flex_none()
                        .whitespace_nowrap()
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
