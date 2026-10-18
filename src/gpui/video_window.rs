//! `hopper --video URL`: a small window that plays a description's video
//! (a YouTube, Vimeo or Streamable page, or a video file) in WebView2.
//!
//! It runs as its own process: a WebView2 child window inside the app's
//! window would draw over its popovers. The page comes from a
//! `https://hopper.localhost` origin, since YouTube refuses to play embeds
//! without a referrer (error 153). Links out of the player open in the
//! browser. Exit code: 0 closed, 1 not a video, 2 no WebView2.

use hopper::richtext::{Video, video_of};
use std::borrow::Cow;
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use wry::http::{Response, header::CONTENT_TYPE};
use wry::{NewWindowResponse, WebContext, WebViewBuilder, WebViewBuilderExtWindows as _};

const ORIGIN: &str = "https://hopper.localhost/";

pub fn run(url: &str) -> i32 {
    let Some(video) = video_of(url) else {
        return 1;
    };
    let page = page(&video);
    let event_loop = EventLoop::new();
    let title = match video.site() {
        "video" => "Hopper · video".to_string(),
        s => format!("Hopper · {s} video"),
    };
    let Ok(window) = WindowBuilder::new()
        .with_title(title)
        .with_inner_size(LogicalSize::new(960.0, 560.0))
        .with_min_inner_size(LogicalSize::new(320.0, 200.0))
        .build(&event_loop)
    else {
        return 2;
    };
    // Its own profile: the --fetch window's cookies stay apart.
    let data = directories::ProjectDirs::from("", "", "Hopper").map(|d| d.data_dir().join("webview-video"));
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
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        // Keeps the context and webview alive as long as the window.
        let _ = (&context, &_webview);
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            std::process::exit(0);
        }
    })
}

/// The page holding the player, filling the window.
fn page(v: &Video) -> String {
    let attr = |s: &str| s.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;");
    let player = match &v.embed {
        Some(embed) => {
            let sep = if embed.contains('?') { '&' } else { '?' };
            format!(
                r#"<iframe src="{}" allow="autoplay; encrypted-media; fullscreen; picture-in-picture" allowfullscreen referrerpolicy="strict-origin-when-cross-origin"></iframe>"#,
                attr(&format!("{embed}{sep}autoplay=1&rel=0"))
            )
        }
        None => format!(r#"<video src="{}" controls autoplay></video>"#, attr(&v.url)),
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
        assert!(yt.contains(r#"<iframe src="https://www.youtube.com/embed/abc?autoplay=1&amp;rel=0""#), "{yt}");
        let vimeo = super::page(&hopper::richtext::video_of("https://vimeo.com/1/ff").unwrap());
        assert!(vimeo.contains("video/1?h=ff&amp;autoplay=1"), "{vimeo}");
        let file = super::page(&hopper::richtext::video_of("https://x.y/a.mp4?q=\"1\"").unwrap());
        assert!(file.contains(r#"<video src="https://x.y/a.mp4?q=&quot;1&quot;" controls"#), "{file}");
    }
}
