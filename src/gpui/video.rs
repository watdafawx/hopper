//! Playing a description's video, pinned over its tile: a borderless player
//! window (`hopper --video URL --owner HWND`, see `video_window`) that the
//! tile places each time it is drawn. Without WebView2, or off Windows, the
//! browser plays it.
//!
//! One video plays at a time. It stops when its tile is no longer drawn
//! (another project, another page) and hides while a dialog is open.

use gpui_kit::*;
use hopper::richtext::Video;
use std::cell::RefCell;

#[cfg_attr(not(windows), allow(dead_code))]
struct Player {
    id: u64,
    url: String,
    stdin: std::process::ChildStdin,
    /// The last placement sent, so a redraw in place sends nothing.
    sent: String,
    hidden: bool,
}

thread_local! {
    static PLAYER: RefCell<Option<Player>> = const { RefCell::new(None) };
}

impl Player {
    fn send(&mut self, line: &str) {
        use std::io::Write as _;
        let _ = writeln!(self.stdin, "{line}").and_then(|_| self.stdin.flush());
    }
}

/// Whether the video at `url` is playing in Hopper.
pub fn playing(url: &str) -> bool {
    PLAYER.with_borrow(|p| p.as_ref().is_some_and(|p| p.url == url))
}

/// Stops the video playing (its window closes when its input ends).
pub fn stop() {
    PLAYER.take();
}

/// Plays `v` over its tile, or in the browser when that can't run.
pub fn play(v: &Video, window: &mut Window, cx: &mut App) {
    stop();
    #[cfg(windows)]
    if let Some(owner) = hwnd(window) {
        use std::process::{Command, Stdio};
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let child = std::env::current_exe().and_then(|exe| {
            Command::new(exe)
                .args(["--video", &v.url, "--owner", &owner.to_string()])
                .stdin(Stdio::piped())
                .spawn()
        });
        if let Ok(mut child) = child
            && let Some(stdin) = child.stdin.take()
        {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            PLAYER.set(Some(Player {
                id,
                url: v.url.clone(),
                stdin,
                sent: String::new(),
                hidden: false,
            }));
            let url = v.url.clone();
            let handle = window.window_handle();
            let exited = cx.background_executor().spawn(async move { child.wait() });
            cx.spawn(async move |cx| {
                let code = exited.await.map(|s| s.code());
                // 2: no WebView2. One closed or killed mid-video stays shut.
                if matches!(code, Err(_) | Ok(Some(1 | 2))) {
                    log::warn!("video window: {code:?}");
                    let _ = cx.update(|cx| cx.open_url(&url));
                }
                // The tile shows its poster again.
                if PLAYER.with_borrow(|p| p.as_ref().is_some_and(|p| p.id == id)) {
                    stop();
                }
                let _ = cx.update_window(handle, |_, window, _| window.refresh());
            })
            .detach();
            window.refresh();
            return;
        }
    }
    cx.open_url(&v.url);
}

/// The main window's handle, as the player's owner.
#[cfg(windows)]
fn hwnd(window: &Window) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

/// Places the player of `url` over its tile: `tile` in window coordinates,
/// `visible` the part of it the page shows. Called as the tile is drawn.
pub fn place(url: &str, tile: Bounds<Pixels>, visible: Bounds<Pixels>, scale: f32) {
    let px = |b: Bounds<Pixels>| {
        let x = (f32::from(b.origin.x) * scale).round() as i32;
        let y = (f32::from(b.origin.y) * scale).round() as i32;
        let r = (f32::from(b.origin.x + b.size.width) * scale).round() as i32;
        let bt = (f32::from(b.origin.y + b.size.height) * scale).round() as i32;
        format!("{x} {y} {} {}", (r - x).max(0), (bt - y).max(0))
    };
    let line = format!("place {} {}", px(tile), px(visible));
    PLAYER.with_borrow_mut(|p| {
        if let Some(p) = p
            && p.url == url
            && p.sent != line
        {
            p.send(&line);
            p.sent = line;
        }
    });
}

/// Once per frame of the main window: the player hides under dialogs, and
/// when `covered` (the palette is open).
pub fn frame(covered: bool, window: &mut Window, cx: &mut App) {
    use gpui_kit::component::WindowExt as _;
    if PLAYER.with_borrow(Option::is_none) {
        return;
    }
    let hide = covered || window.has_active_dialog(cx);
    PLAYER.with_borrow_mut(|p| {
        if let Some(p) = p
            && p.hidden != hide
        {
            p.hidden = hide;
            p.send(if hide { "hide" } else { "show" });
        }
    });
}

/// Kept in a playing tile's element state: the video stops once the tile
/// stops being drawn.
pub struct TileKeep(pub String);

impl Drop for TileKeep {
    fn drop(&mut self) {
        if playing(&self.0) {
            stop();
        }
    }
}
