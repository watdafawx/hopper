//! `hopper --fetch DIR [--parallel=N] URL...`: a small window that opens
//! each CurseForge download page in WebView2 and saves the file the page
//! starts into DIR, then closes. With N above 1 it shows N pages side by
//! side and works through the list with all of them. Files whose authors
//! allow website downloads only come in this way; the app's download
//! watcher puts them in place from DIR.
//!
//! It runs as its own process so a stuck page can't touch the app's window
//! loop. Exit code: 0 every file came, 1 some didn't, 2 no WebView2.
//! Each step goes to stdout as a [`hopper::webfetch::Event`] line, so the
//! app can show the player what it's waiting for.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::io::Write;
use std::time::{Duration, Instant};
use hopper::webfetch::Event as Say;
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::windows::WindowExtWindows;
use tao::window::{Window, WindowBuilder};
use wry::{NewWindowResponse, Rect, WebContext, WebView, WebViewBuilder};

/// How long a page gets to start its download (Cloudflare's check, then
/// CurseForge's five-second countdown).
const START_WITHIN: Duration = Duration::from_secs(45);

/// Pages start this far apart, so CurseForge doesn't see a burst.
const STAGGER: Duration = Duration::from_millis(1500);

enum Ev {
    /// The download of page `n` finished (ok or not).
    Done(usize, bool),
    /// Page `n`'s try `t` didn't start a download in time.
    Timeout(usize, u32),
    /// Look whether an idle page can take the next file.
    Tick,
}

/// One of the pages working through the list.
#[derive(Default)]
struct Slot {
    /// The file (index into the URL list) it's on.
    page: Cell<Option<usize>>,
    /// Its download started.
    started: Cell<bool>,
    /// It's on Cloudflare's "Just a moment" check.
    checking: Cell<bool>,
    /// It takes no file before this.
    not_before: Cell<Option<Instant>>,
}

/// Tells the app a step. A closed pipe (the app quit) is no reason to stop.
fn say(e: Say) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", e.line());
    let _ = out.flush();
}

/// Columns and rows for `n` pages side by side.
fn grid(n: usize) -> (usize, usize) {
    let cols = (n as f64).sqrt().ceil().max(1.0) as usize;
    (cols, n.div_ceil(cols))
}

/// Where page `i` of `n` sits in the window.
fn bounds(window: &Window, i: usize, n: usize) -> Rect {
    let size = window.inner_size().to_logical::<f64>(window.scale_factor());
    let (cols, rows) = grid(n);
    let (w, h) = (size.width / cols as f64, size.height / rows as f64);
    let (c, r) = (i % cols, i / cols);
    Rect {
        position: wry::dpi::LogicalPosition::new(c as f64 * w, r as f64 * h).into(),
        size: wry::dpi::LogicalSize::new(w, h).into(),
    }
}

pub fn run(dir: PathBuf, urls: Vec<String>, parallel: usize) -> i32 {
    if urls.is_empty() {
        return 0;
    }
    // WebView2 wants a plain Windows path to save to.
    let dir = PathBuf::from(dir.to_string_lossy().replace('/', "\\"));
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::remove_dir_all(dir.join(".partial"));
    let n = urls.len();
    let k = parallel.clamp(1, 8).min(n);
    let title = move |finished: usize| {
        if n == 1 {
            "Hopper · getting a file from CurseForge".to_string()
        } else {
            format!("Hopper · getting files from CurseForge ({} of {n})", (finished + 1).min(n))
        }
    };
    let event_loop = EventLoopBuilder::<Ev>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let size = if k == 1 { LogicalSize::new(960.0, 680.0) } else { LogicalSize::new(1280.0, 860.0) };
    let Ok(window) = WindowBuilder::new()
        .with_title(title(0))
        .with_inner_size(size)
        .build(&event_loop)
    else {
        return 2;
    };
    say(Say::Window(window.hwnd() as i64));
    // Cookies (Cloudflare's clearance) kept between runs, not next to the exe.
    let data = hopper::paths::dirs().map(|d| d.data_dir().join("webview"));
    let mut context = WebContext::new(data);
    let slots: Vec<Rc<Slot>> = (0..k).map(|_| Rc::new(Slot::default())).collect();
    let mut views: Vec<WebView> = Vec::with_capacity(k);
    for (si, slot) in slots.iter().enumerate() {
        let builder = WebViewBuilder::new_with_web_context(&mut context)
            .with_url("about:blank")
            // Cloudflare's check page is "Just a moment..."; CurseForge's own
            // page counts down to the download.
            .with_document_title_changed_handler({
                let slot = slot.clone();
                move |title| {
                    let Some(i) = slot.page.get() else { return };
                    if slot.started.get() || title.trim().is_empty() || title.starts_with("http") || title == "about:blank" {
                        return;
                    }
                    let check = title.to_lowercase().contains("just a moment");
                    slot.checking.set(check);
                    say(if check { Say::Check(i) } else { Say::Countdown(i) });
                }
            })
            .with_download_started_handler({
                let (dir, slot, proxy) = (dir.clone(), slot.clone(), proxy.clone());
                move |url, path| {
                    let Some(i) = slot.page.get() else { return false };
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
                    slot.started.set(true);
                    slot.checking.set(false);
                    // WebView2 doesn't always say a download finished: the file
                    // being there and done growing is enough.
                    let (path, proxy) = (path.clone(), proxy.clone());
                    std::thread::spawn(move || {
                        let (mut last, mut same) = (None, 0);
                        for _ in 0..1200 {
                            std::thread::sleep(Duration::from_millis(500));
                            let len = std::fs::metadata(&path).ok().map(|m| m.len()).filter(|l| *l > 0);
                            same = if len.is_some() && len == last { same + 1 } else { 0 };
                            if let Some(n) = len.filter(|_| same == 0) {
                                say(Say::Bytes(i, n));
                            }
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
            .with_new_window_req_handler(|_, _| NewWindowResponse::Deny);
        let built = if k == 1 {
            builder.build(&window)
        } else {
            builder.with_bounds(bounds(&window, si, k)).build_as_child(&window)
        };
        let Ok(view) = built else {
            return 2;
        };
        views.push(view);
    }
    // Each page starts a little after the one before.
    let now = Instant::now();
    for (si, slot) in slots.iter().enumerate() {
        slot.not_before.set(Some(now + STAGGER * si as u32));
    }
    // Wakes the loop now and then for staggered starts and pauses.
    {
        let proxy = proxy.clone();
        std::thread::spawn(move || {
            while proxy.send_event(Ev::Tick).is_ok() {
                std::thread::sleep(Duration::from_millis(500));
            }
        });
    }
    let timeout = {
        let proxy = proxy.clone();
        move |i: usize, t: u32| {
            let proxy = proxy.clone();
            std::thread::spawn(move || {
                std::thread::sleep(START_WITHIN);
                let _ = proxy.send_event(Ev::Timeout(i, t));
            });
        }
    };
    let queue: RefCell<VecDeque<usize>> = RefCell::new((0..n).collect());
    let tries: RefCell<Vec<u32>> = RefCell::new(vec![0; n]);
    // How many pages may work at once. A file CurseForge doesn't start
    // while others run costs one, down to a single page.
    let limit = Cell::new(k);
    let (mut finished, mut failed, mut shown) = (0usize, 0usize, 0usize);
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        // Keeps the context alive as long as the webviews.
        let _ = &context;
        let slot_of = |i: usize| slots.iter().position(|s| s.page.get() == Some(i));
        // The file page `si` was on ended; the page is free.
        let free = |si: usize| {
            let s = &slots[si];
            s.page.set(None);
            s.started.set(false);
            s.checking.set(false);
            s.not_before.set(Some(Instant::now() + STAGGER));
            let _ = views[si].load_url("about:blank");
        };
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                say(Say::Closed);
                std::process::exit(if failed == 0 && finished >= n { 0 } else { 1 });
            }
            Event::WindowEvent { event: WindowEvent::Resized(_), .. } if k > 1 => {
                for (si, v) in views.iter().enumerate() {
                    let _ = v.set_bounds(bounds(&window, si, k));
                }
            }
            Event::UserEvent(Ev::Done(i, ok)) => {
                if let Some(si) = slot_of(i) {
                    finished += 1;
                    if !ok {
                        failed += 1;
                    }
                    say(if ok { Say::Done(i) } else { Say::Failed(i) });
                    free(si);
                }
            }
            Event::UserEvent(Ev::Timeout(i, t)) => {
                if let Some(si) = slot_of(i).filter(|si| !slots[*si].started.get() && tries.borrow()[i] == t) {
                    free(si);
                    if limit.get() > 1 && t == 1 {
                        // Likely CurseForge holding back several pages at
                        // once: one page fewer, and this file again later.
                        limit.set(limit.get() - 1);
                        log::warn!("fetch: page {i} didn't start; down to {} at once", limit.get());
                        queue.borrow_mut().push_back(i);
                    } else {
                        finished += 1;
                        failed += 1;
                        say(Say::Failed(i));
                    }
                }
            }
            _ => {}
        }
        if finished >= n {
            std::process::exit(if failed == 0 { 0 } else { 1 });
        }
        // Idle pages take the next files, unless one is on Cloudflare's
        // check (more pages then only make it stricter).
        let paused = slots.iter().any(|s| s.checking.get());
        let busy = slots.iter().filter(|s| s.page.get().is_some()).count();
        let mut room = limit.get().saturating_sub(busy);
        for (si, s) in slots.iter().enumerate() {
            if room == 0 || paused {
                break;
            }
            if s.page.get().is_some() || s.not_before.get().is_some_and(|t| Instant::now() < t) {
                continue;
            }
            let Some(i) = queue.borrow_mut().pop_front() else { break };
            let t = {
                let mut tr = tries.borrow_mut();
                tr[i] += 1;
                tr[i]
            };
            s.page.set(Some(i));
            s.started.set(false);
            s.checking.set(false);
            say(Say::Opening(i));
            let _ = views[si].load_url(&urls[i]);
            timeout(i, t);
            room -= 1;
        }
        if shown != finished {
            shown = finished;
            window.set_title(&title(finished));
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn grid_fits_the_pages() {
        assert_eq!(super::grid(1), (1, 1));
        assert_eq!(super::grid(2), (2, 1));
        assert_eq!(super::grid(4), (2, 2));
        assert_eq!(super::grid(5), (3, 2));
        assert_eq!(super::grid(8), (3, 3));
    }
}
