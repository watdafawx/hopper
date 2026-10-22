//! `hopper --video URL [--owner HWND]`: plays a description's video (a
//! YouTube, Vimeo or Streamable page, or a video file) in WebView2.
//!
//! It runs as its own process: a WebView2 child window inside the app's
//! window would draw over its popovers. The page comes from a
//! `https://hopper.localhost` origin, since YouTube refuses to play embeds
//! without a referrer (error 153). Links out of the player open in the
//! browser. Exit code: 0 closed, 1 not a video, 2 no WebView2.
//!
//! With `--owner`, the window is pinned: borderless, owned by the app's
//! window (so it stays just above it and minimizes with it), and placed by
//! lines on stdin, in the owner's client pixels:
//! `place X Y W H VX VY VW VH` (the tile, then the part of it on screen),
//! `hide`, `show`. It follows the owner when that moves, and closes when
//! stdin ends or the owner goes away.

use hopper::richtext::{Video, video_of};
use std::borrow::Cow;
use std::io::BufRead as _;
use std::time::Duration;
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::windows::{WindowBuilderExtWindows as _, WindowExtWindows as _};
use tao::window::WindowBuilder;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{ClientToScreen, CreateRectRgn, SetWindowRgn};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetWindowRect, IsWindow, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE,
    SWP_NOZORDER, SetWindowPos, ShowWindow,
};
use wry::http::{Response, header::CONTENT_TYPE};
use wry::{NewWindowResponse, WebContext, WebViewBuilder, WebViewBuilderExtWindows as _};

const ORIGIN: &str = "https://hopper.localhost/";

/// What a pinned player hears from the app (stdin) and its owner watcher.
#[derive(Debug, PartialEq)]
enum Cmd {
    /// The tile and its visible part, in the owner's client pixels.
    Place([i32; 4], [i32; 4]),
    Hide,
    Show,
    /// The owner's client area moved to this screen point.
    Origin(i32, i32),
    Close,
}

fn parse_cmd(line: &str) -> Option<Cmd> {
    let mut it = line.split_whitespace();
    match it.next()? {
        "place" => {
            let n: Vec<i32> = it.filter_map(|s| s.parse().ok()).collect();
            let n: [i32; 8] = n.try_into().ok()?;
            Some(Cmd::Place(
                [n[0], n[1], n[2], n[3]],
                [n[4], n[5], n[6], n[7]],
            ))
        }
        "hide" => Some(Cmd::Hide),
        "show" => Some(Cmd::Show),
        "close" => Some(Cmd::Close),
        _ => None,
    }
}

/// Where the owner's client area starts on screen; `None` once it's gone.
fn client_origin(owner: HWND) -> Option<(i32, i32)> {
    // SAFETY: plain queries on a window handle; a stale one just fails.
    unsafe {
        if !IsWindow(Some(owner)).as_bool() {
            return None;
        }
        let mut p = POINT::default();
        let _ = ClientToScreen(owner, &mut p);
        Some((p.x, p.y))
    }
}

pub fn run(url: &str, owner: Option<isize>) -> i32 {
    let Some(video) = video_of(url) else {
        return 1;
    };
    let page = page(&video);
    let event_loop = EventLoopBuilder::<Cmd>::with_user_event().build();
    let title = match video.site() {
        "video" => "Hopper · video".to_string(),
        s => format!("Hopper · {s} video"),
    };
    let mut builder = WindowBuilder::new()
        .with_title(title)
        .with_inner_size(LogicalSize::new(960.0, 560.0))
        .with_min_inner_size(LogicalSize::new(320.0, 200.0));
    if let Some(o) = owner {
        builder = builder
            .with_owner_window(o)
            .with_decorations(false)
            .with_resizable(false)
            .with_skip_taskbar(true)
            .with_visible(false)
            .with_min_inner_size(LogicalSize::new(1.0, 1.0));
    }
    let Ok(window) = builder.build(&event_loop) else {
        return 2;
    };
    if let Some(o) = owner {
        let proxy = event_loop.create_proxy();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                if let Some(c) = parse_cmd(&line) {
                    let _ = proxy.send_event(c);
                }
            }
            let _ = proxy.send_event(Cmd::Close);
        });
        // The owner moving is only seen by asking.
        let proxy = event_loop.create_proxy();
        let o = o as usize;
        std::thread::spawn(move || {
            let mut last = None;
            loop {
                match client_origin(HWND(o as *mut _)) {
                    None => {
                        let _ = proxy.send_event(Cmd::Close);
                        return;
                    }
                    p if p != last => {
                        last = p;
                        let (x, y) = p.unwrap();
                        if proxy.send_event(Cmd::Origin(x, y)).is_err() {
                            return;
                        }
                    }
                    _ => {}
                }
                std::thread::sleep(Duration::from_millis(15));
            }
        });
    }
    // Its own profile: the --fetch window's cookies stay apart.
    let data = hopper::paths::dirs()
        .map(|d| d.data_dir().join("webview-video"));
    let mut context = WebContext::new(data);
    let webview = WebViewBuilder::new_with_web_context(&mut context)
        .with_https_scheme(true)
        .with_background_color((0, 0, 0, 255))
        .with_custom_protocol("hopper".into(), move |_, _| {
            Response::builder()
                .header(CONTENT_TYPE, "text/html; charset=utf-8")
                .body(Cow::Owned(page.clone().into_bytes()))
                .unwrap_or_default()
        })
        .with_url(ORIGIN)
        // The player's own links ("Watch on YouTube", the channel) go to
        // the browser; the window keeps the video.
        .with_navigation_handler(|u| {
            let ours = u.starts_with(ORIGIN) || u.starts_with("about:");
            if !ours {
                open_in_browser(&u);
            }
            ours
        })
        .with_new_window_req_handler(|u, _| {
            open_in_browser(&u);
            NewWindowResponse::Deny
        })
        .build(&window);
    let Ok(_webview) = webview else {
        return 2;
    };
    let hwnd = HWND(window.hwnd() as *mut _);
    let mut pin = Pin::default();
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        // Keeps the context and webview alive as long as the window.
        let _ = (&context, &_webview, &window);
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            }
            | Event::UserEvent(Cmd::Close) => std::process::exit(0),
            Event::UserEvent(c) => {
                let before = pin.clone();
                match c {
                    Cmd::Place(tile, vis) => (pin.tile, pin.visible) = (tile, vis),
                    Cmd::Hide => pin.hidden = true,
                    Cmd::Show => pin.hidden = false,
                    Cmd::Origin(x, y) => pin.origin = Some((x, y)),
                    Cmd::Close => {}
                }
                if pin != before {
                    pin.apply(hwnd, &before);
                }
            }
            _ => {}
        }
    })
}

/// Where a pinned player is to be.
#[derive(Clone, Default, PartialEq)]
struct Pin {
    tile: [i32; 4],
    visible: [i32; 4],
    hidden: bool,
    origin: Option<(i32, i32)>,
}

impl Pin {
    fn shown(&self) -> bool {
        !self.hidden
            && self.origin.is_some()
            && self.visible[2] > 0
            && self.visible[3] > 0
            && self.tile[2] > 0
    }

    fn apply(&self, hwnd: HWND, before: &Pin) {
        let Some((ox, oy)) = self.origin else { return };
        let [x, y, w, h] = self.tile;
        if !self.shown() {
            // SAFETY: our own live window.
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            return;
        }
        // Everything again after being hidden: what came then was not applied.
        let fresh = !before.shown();
        // tao keeps an invisible resize frame around a borderless window's
        // client area: the window grows by it, so the video fills the tile.
        // The frame changes with the first resize, so it is measured again
        // after moving, and the move redone if it did.
        let mut f = frame(hwnd);
        if fresh || (self.tile, self.origin) != (before.tile, before.origin) {
            for _ in 0..3 {
                let (l, t, r, b) = f;
                // SAFETY: our own live window.
                unsafe {
                    let _ = SetWindowPos(
                        hwnd,
                        None,
                        ox + x - l,
                        oy + y - t,
                        w + l + r,
                        h + t + b,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                let now = frame(hwnd);
                if now == f {
                    break;
                }
                f = now;
            }
        }
        let (l, t, ..) = f;
        // Only the part of the tile the description shows; the rest is
        // scrolled away under the app's own header. Window coordinates.
        let [vx, vy, vw, vh] = self.visible;
        let (rx, ry) = (vx - x + l, vy - y + t);
        // SAFETY: our own live window; the system owns the region once set.
        unsafe {
            let rgn = CreateRectRgn(rx, ry, rx + vw, ry + vh);
            SetWindowRgn(hwnd, Some(rgn), true);
            if fresh {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
        }
    }
}

/// How far the client area sits inside the window: left, top, right, bottom.
fn frame(hwnd: HWND) -> (i32, i32, i32, i32) {
    // SAFETY: plain queries on our own live window.
    unsafe {
        let (mut w, mut c, mut p) = (RECT::default(), RECT::default(), POINT::default());
        if GetWindowRect(hwnd, &mut w).is_err() || GetClientRect(hwnd, &mut c).is_err() {
            return (0, 0, 0, 0);
        }
        let _ = ClientToScreen(hwnd, &mut p);
        (
            p.x - w.left,
            p.y - w.top,
            w.right - (p.x + c.right),
            w.bottom - (p.y + c.bottom),
        )
    }
}

/// The page holding the player, filling the window.
fn page(v: &Video) -> String {
    let attr = |s: &str| {
        s.replace('&', "&amp;")
            .replace('"', "&quot;")
            .replace('<', "&lt;")
    };
    let player = match &v.embed {
        Some(embed) => {
            let sep = if embed.contains('?') { '&' } else { '?' };
            format!(
                r#"<iframe src="{}" allow="autoplay; encrypted-media; fullscreen; picture-in-picture" allowfullscreen referrerpolicy="strict-origin-when-cross-origin"></iframe>"#,
                attr(&format!("{embed}{sep}autoplay=1&rel=0"))
            )
        }
        None => format!(
            r#"<video src="{}" controls autoplay></video>"#,
            attr(&v.url)
        ),
    };
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><meta name="referrer" content="strict-origin-when-cross-origin">
<style>html,body{{margin:0;height:100%;background:#000;overflow:hidden}}iframe,video{{border:0;width:100%;height:100%;display:block}}</style>
</head><body>{player}</body></html>"#
    )
}

fn open_in_browser(url: &str) {
    if url.starts_with("https://") || url.starts_with("http://") {
        let _ = std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", url])
            .spawn();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn page_embeds_or_plays_the_file() {
        let yt = super::page(&hopper::richtext::video_of("https://youtu.be/abc").unwrap());
        assert!(
            yt.contains(r#"<iframe src="https://www.youtube.com/embed/abc?autoplay=1&amp;rel=0""#),
            "{yt}"
        );
        let vimeo = super::page(&hopper::richtext::video_of("https://vimeo.com/1/ff").unwrap());
        assert!(vimeo.contains("video/1?h=ff&amp;autoplay=1"), "{vimeo}");
        let file = super::page(&hopper::richtext::video_of("https://x.y/a.mp4?q=\"1\"").unwrap());
        assert!(
            file.contains(r#"<video src="https://x.y/a.mp4?q=&quot;1&quot;" controls"#),
            "{file}"
        );
    }

    #[test]
    fn commands() {
        use super::{Cmd, parse_cmd};
        assert_eq!(
            parse_cmd("place 10 20 640 360 10 80 640 300"),
            Some(Cmd::Place([10, 20, 640, 360], [10, 80, 640, 300]))
        );
        assert_eq!(parse_cmd("place 1 2 3"), None);
        assert_eq!(parse_cmd("hide"), Some(Cmd::Hide));
        assert_eq!(parse_cmd("bogus"), None);
    }
}
