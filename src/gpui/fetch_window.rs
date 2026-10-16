//! `hopper --fetch DIR URL...`: a small window that opens each CurseForge
//! download page in WebView2 and saves the file the page starts into DIR,
//! then closes. Files whose authors allow website downloads only come in
//! this way; the app's download watcher puts them in place from DIR.
//!
//! It runs as its own process so a stuck page can't touch the app's window
//! loop. Exit code: 0 every file came, 1 some didn't, 2 no WebView2.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
use wry::{NewWindowResponse, WebContext, WebViewBuilder};

/// How long a page gets to start its download (Cloudflare's check, then
/// CurseForge's five-second countdown).
const START_WITHIN: Duration = Duration::from_secs(45);

enum Ev {
    /// The download of page `n` finished (ok or not).
    Done(usize, bool),
    /// Page `n` didn't start a download in time.
    Timeout(usize),
}

pub fn run(dir: PathBuf, urls: Vec<String>) -> i32 {
    if urls.is_empty() {
        return 0;
    }
    // WebView2 wants a plain Windows path to save to.
    let dir = PathBuf::from(dir.to_string_lossy().replace('/', "\\"));
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::remove_dir_all(dir.join(".partial"));
    let n = urls.len();
    let title = move |i: usize| {
        if n == 1 {
            "Hopper · getting a file from CurseForge".to_string()
        } else {
            format!("Hopper · getting files from CurseForge ({} of {n})", i + 1)
        }
    };
    let event_loop = EventLoopBuilder::<Ev>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let Ok(window) = WindowBuilder::new()
        .with_title(title(0))
        .with_inner_size(LogicalSize::new(960.0, 680.0))
        .build(&event_loop)
    else {
        return 2;
    };
    let current = Rc::new(Cell::new(0usize));
    let started = Rc::new(Cell::new(false));
    // Cookies (Cloudflare's clearance) kept between runs, not next to the exe.
    let data = directories::ProjectDirs::from("", "", "Hopper").map(|d| d.data_dir().join("webview"));
    let mut context = WebContext::new(data);
    let webview = WebViewBuilder::new_with_web_context(&mut context)
        .with_url(&urls[0])
        .with_download_started_handler({
            let (dir, started, current, proxy) = (dir.clone(), started.clone(), current.clone(), proxy.clone());
            move |url, path| {
                let name = path
                    .file_name()
                    .map(|n| n.to_os_string())
                    .or_else(|| url.split('?').next()?.rsplit('/').next().map(Into::into))
                    .unwrap_or_else(|| "download.jar".into());
                // WebView2 holds a .jar as unconfirmed and deletes it when the
                // window closes, so it downloads aside and the finished file
                // is copied out.
                let part = dir.join(".partial");
                let _ = std::fs::create_dir_all(&part);
                let done_at = dir.join(&name);
                *path = part.join(name);
                log::info!("fetch: {url} -> {}", path.display());
                started.set(true);
                // WebView2 doesn't always say a download finished: the file
                // being there and done growing is enough.
                let (path, i, proxy) = (path.clone(), current.get(), proxy.clone());
                std::thread::spawn(move || {
                    let (mut last, mut same) = (None, 0);
                    for _ in 0..1200 {
                        std::thread::sleep(Duration::from_millis(500));
                        let len = std::fs::metadata(&path).ok().map(|m| m.len()).filter(|l| *l > 0);
                        same = if len.is_some() && len == last { same + 1 } else { 0 };
                        last = len;
                        // Done growing for 1.5 s. The app checks size and
                        // SHA-1 before using it, so a stalled copy is caught.
                        if same >= 3 {
                            let tmp = done_at.with_extension("hopper-tmp");
                            let ok = std::fs::copy(&path, &tmp).is_ok() && std::fs::rename(&tmp, &done_at).is_ok();
                            let _ = proxy.send_event(Ev::Done(i, ok));
                            return;
                        }
                    }
                    let _ = proxy.send_event(Ev::Done(i, false));
                });
                true
            }
        })
        .with_download_completed_handler({
            // Logged only: the copy above ends the page.
            move |url, _, ok| log::info!("fetch: {url} done, ok={ok}")
        })
        // Ads and "get the app" links open nothing.
        .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
        .build(&window);
    let Ok(webview) = webview else {
        return 2;
    };
    let timeout = {
        let proxy = proxy.clone();
        move |i: usize| {
            let proxy = proxy.clone();
            std::thread::spawn(move || {
                std::thread::sleep(START_WITHIN);
                let _ = proxy.send_event(Ev::Timeout(i));
            });
        }
    };
    timeout(0);
    let mut failed = 0;
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        // Keeps the context alive as long as the webview.
        let _ = &context;
        let next = match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                std::process::exit(if failed == 0 && current.get() + 1 >= n { 0 } else { 1 });
            }
            Event::UserEvent(Ev::Done(i, ok)) if i == current.get() => {
                if !ok {
                    failed += 1;
                }
                true
            }
            Event::UserEvent(Ev::Timeout(i)) if i == current.get() && !started.get() => {
                failed += 1;
                true
            }
            _ => false,
        };
        if next {
            let i = current.get() + 1;
            if i >= n {
                std::process::exit(if failed == 0 { 0 } else { 1 });
            }
            current.set(i);
            started.set(false);
            window.set_title(&title(i));
            let _ = webview.load_url(&urls[i]);
            timeout(i);
        }
    })
}
