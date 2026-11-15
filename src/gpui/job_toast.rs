//! Toasts for work that runs in the background once its dialog closed (a
//! pack update, a repair, a server pack update): how it ended, and when the
//! download window behind Hopper needs the player. The words come from the
//! job (`hopper::jobs::toast_of`), so every job that sets them says it the
//! same way.

use crate::store::Store;
use crate::widgets::ToastAction as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Sizable as _, WindowExt as _};
use gpui_kit::*;
use hopper::jobs::{JobState, ToastKind};
use std::path::PathBuf;

/// Marks the "waiting for you" toasts, one per file page, so the one for a
/// page closes when the page moves on.
struct WaitingOnPlayer;

/// Shows the toast for job `j` that just ended, if it asked for one.
pub fn ended(store: &Entity<Store>, j: &JobState, window: &mut Window, cx: &mut App) {
    let Some((kind, title, body)) = hopper::jobs::toast_of(j) else { return };
    // With nothing more to say, the title alone is the message: a title over
    // an empty body leaves a blank line in the toast.
    if body.is_empty() && kind == ToastKind::Done {
        window.push_notification(Notification::success(title), cx);
        return;
    }
    let n = match kind {
        ToastKind::Done => Notification::success(body).title(title),
        ToastKind::Failed => Notification::error(body).title(title),
        // Some website files didn't come: Retry opens the window again for
        // the ones the instance still waits for.
        ToastKind::Missing => {
            let n = Notification::warning(body).title(title);
            match j.toast.as_ref().and_then(|t| t.root.clone()) {
                Some(root) => with_retry(n, store.clone(), root),
                None => n,
            }
        }
    };
    window.push_notification(n, cx);
}

fn with_retry(n: Notification, store: Entity<Store>, root: PathBuf) -> Notification {
    n.action_below(move |_, _, cx| {
        let (store, root) = (store.clone(), root.clone());
        Button::new("job-toast-retry")
            .label("Retry")
            .small()
            .outline()
            .tooltip("Opens the download window again for the files that didn't come")
            .on_click(cx.listener(move |note, _, window, cx| {
                let urls = hopper::modpack::pending_urls_at(&root);
                if !urls.is_empty() {
                    store.read(cx).fetch_pages(&root, urls, None);
                }
                note.dismiss(window, cx);
            }))
    })
}

/// The download window opens behind Hopper and never takes focus, so when
/// CurseForge asks the player to prove they're not a bot, Hopper says so.
pub fn waiting(inst: &str, url: &str, file: &str, window: &mut Window, cx: &mut App) {
    let title = format!("{} is waiting for you", crate::widgets::short_label(inst, 40));
    let body = format!(
        "CurseForge wants to check you're not a bot before it hands over {file}. Tick the box on its page."
    );
    let n = Notification::warning(body).title(title).id1::<WaitingOnPlayer>(SharedString::from(url.to_string())).action_below(
        |_, _, cx| {
            Button::new("job-toast-show").label("Show window").small().primary().on_click(cx.listener(|note, _, window, cx| {
                crate::fetch_progress::show_window();
                note.dismiss(window, cx);
            }))
        },
    );
    window.push_notification(n, cx);
}

/// Closes the "waiting for you" toast of a page that moved on.
pub fn not_waiting(url: &str, window: &mut Window, cx: &mut App) {
    window.remove_notification1::<WaitingOnPlayer>(SharedString::from(url.to_string()), cx);
}
