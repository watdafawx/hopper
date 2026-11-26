//! "Send before you leave": the dialog when the player closes Hopper while
//! this PC still has group changes the friends service has not taken. It
//! offers to send them now (waiting a few seconds), or to leave them queued:
//! they are saved on this PC and go out the next time Hopper starts.

use crate::widgets;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogFooter};
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// How long "Send now and close" waits for the service before it closes anyway.
pub const WAIT: Duration = Duration::from_secs(10);

/// What the dialog needs from the app, so a preview can stand in for it.
#[derive(Clone)]
pub struct Leaving {
    /// How many changes have not been taken by the service yet.
    pub unsent: Rc<dyn Fn(&App) -> usize>,
    /// Post them now.
    pub send: Rc<dyn Fn(&mut App)>,
    /// Close Hopper (what is left stays queued).
    pub close: Rc<dyn Fn(&mut App)>,
    /// A line about the other things the close will end (tasks, servers).
    pub also: Option<String>,
}

#[derive(Clone, Copy)]
struct Wait {
    started: Instant,
    total: usize,
    /// 0..=1, as the bar draws it.
    progress: f32,
}

fn changes_n(n: usize) -> String {
    hopper::syncchip::changes_n(n)
}

/// The title of the first step ("2 changes haven't reached your friends yet").
pub fn ask_title(n: usize) -> String {
    if n == 1 { "1 change hasn't reached your friends yet".to_string() } else { format!("{n} changes haven't reached your friends yet") }
}

/// How far the bar is: what went out, or how much of the wait is used up,
/// whichever is further, so it never sits still.
pub fn progress(total: usize, left: usize, elapsed: Duration) -> f32 {
    let sent = if total == 0 { 1.0 } else { total.saturating_sub(left) as f32 / total as f32 };
    sent.max((elapsed.as_secs_f32() / WAIT.as_secs_f32()).min(1.0)).clamp(0.04, 1.0)
}

/// `--dialog leave-unsent [--state also]`: the dialog with made-up counts and
/// nothing behind it (two changes that never go out, Close does nothing).
pub fn open_preview(also: bool, window: &mut Window, cx: &mut App) {
    open(
        Leaving {
            unsent: Rc::new(|_| 2),
            send: Rc::new(|_| {}),
            close: Rc::new(|_| {}),
            also: also.then(|| "1 task is still running; 1 server hosted here will be stopped (saving the world).".to_string()),
        },
        window,
        cx,
    );
}

pub fn open(leaving: Leaving, window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, move |dialog, window, cx| {
        let wait = window.use_keyed_state(ElementId::Name("send-before-leave".into()), cx, |_, _| None::<Wait>);
        let sending = *wait.read(cx);
        let theme = cx.theme().clone();
        let left = (leaving.unsent)(cx);
        let total = sending.map_or(left, |w| w.total);
        let also = leaving.also.clone();

        let (title, body, line) = match sending {
            None => (
                ask_title(left),
                None,
                "Hopper can send them now. If you close first, they stay saved on this PC and go out the next time Hopper starts.".to_string(),
            ),
            Some(w) => (
                format!("Sending {}…", changes_n(total)),
                Some(w.progress),
                "Hopper closes as soon as they are sent. If they don't go in 10 seconds they stay saved and go out next time Hopper starts.".to_string(),
            ),
        };
        let close_anyway = {
            let close = leaving.close.clone();
            Button::new("sbl-close-anyway").outline().label("Close anyway").on_click(move |_, _, cx| close(cx))
        };
        let mut footer = DialogFooter::new().child(close_anyway);
        if sending.is_none() {
            footer = footer.child(DialogAction::new().child(Button::new("sbl-send").primary().label("Send now and close")));
        }
        let start = leaving.clone();
        let wait_start = wait.clone();
        dialog
            .title(title)
            .w(rems(30.).to_pixels(window.rem_size()))
            .margin_top(rems(widgets::DIALOG_TOP).to_pixels(window.rem_size()))
            .child(
                v_flex()
                    .w_full()
                    .gap_3()
                    .when_some(body, |c, p| {
                        c.child(div().h(px(4.)).w_full().rounded_full().bg(theme.muted).child(div().h_full().w(relative(p)).rounded_full().bg(theme.info)))
                    })
                    .child(div().text_sm().text_color(theme.muted_foreground).child(line))
                    .when_some(also, |c, t| c.child(div().text_sm().text_color(theme.muted_foreground).child(t))),
            )
            .footer(footer)
            .on_ok(move |_, window, cx| {
                if wait_start.read(cx).is_some() {
                    return false;
                }
                let total = (start.unsent)(cx);
                wait_start.update(cx, |w, cx| {
                    *w = Some(Wait { started: Instant::now(), total, progress: progress(total, total, Duration::ZERO) });
                    cx.notify();
                });
                (start.send)(cx);
                let (leaving, wait) = (start.clone(), wait_start.clone());
                let handle = window.window_handle();
                cx.spawn(async move |cx| loop {
                    cx.background_executor().timer(Duration::from_millis(250)).await;
                    let done = cx.update(|cx| {
                        // The player dismissed the dialog: they changed their mind.
                        let open = handle.update(cx, |_, window, cx| window.has_active_dialog(cx)).unwrap_or(false);
                        if !open {
                            return true;
                        }
                        let (left, w) = ((leaving.unsent)(cx), wait.read(cx).unwrap_or(Wait { started: Instant::now(), total, progress: 1.0 }));
                        let elapsed = w.started.elapsed();
                        if left == 0 || elapsed >= WAIT {
                            (leaving.close)(cx);
                            return true;
                        }
                        wait.update(cx, |s, cx| {
                            if let Some(s) = s {
                                s.progress = progress(total, left, elapsed);
                            }
                            cx.notify();
                        });
                        false
                    });
                    if done {
                        break;
                    }
                })
                .detach();
                false
            })
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn the_title_counts_the_changes() {
        assert_eq!(ask_title(1), "1 change hasn't reached your friends yet");
        assert_eq!(ask_title(2), "2 changes haven't reached your friends yet");
    }

    #[::core::prelude::v1::test]
    fn the_bar_follows_what_went_out_or_the_wait_whichever_is_further() {
        // Starts as a sliver, never empty.
        assert!(progress(2, 2, Duration::ZERO) > 0.0);
        // Half the wait gone, nothing sent: half.
        assert!((progress(2, 2, WAIT / 2) - 0.5).abs() < 0.01);
        // One of two sent early: at least half.
        assert!(progress(2, 1, Duration::from_secs(1)) >= 0.5);
        // All sent, or the wait used up: full.
        assert_eq!(progress(2, 0, Duration::ZERO), 1.0);
        assert_eq!(progress(2, 2, WAIT * 2), 1.0);
    }
}
