//! A frame-time log for measuring how smooth the window is (scrolling in
//! particular). Off unless `HOPPER_FRAME_LOG=<file>` is set: then each
//! frame the app view draws appends one line to that file: when it was
//! drawn (ms since the first logged frame), then the milliseconds from the
//! start of its render to the end of its paint (building, laying out and
//! painting every page under it). After those two numbers come the
//! [`span`]s timed during that frame (`name=ms/count`); a navigate from
//! the MCP writes a line of its own (`# nav ...`) with what it ran.

use gpui_kit::*;
use std::cell::{Cell, RefCell};
use std::io::Write;
use std::sync::OnceLock;
use std::time::Instant;

static BOOT: OnceLock<Instant> = OnceLock::new();

thread_local! {
    static START: Cell<Option<Instant>> = const { Cell::new(None) };
    /// Time and count per span name since the last line was written.
    static SPANS: RefCell<Vec<(&'static str, f64, u32)>> = const { RefCell::new(Vec::new()) };
}

/// Times its own life under a name when the log is on; free when it's off.
pub struct Span(Option<(&'static str, Instant)>);

/// Times the rest of the scope it is bound in (`let _t = span("x");`).
pub fn span(name: &'static str) -> Span {
    Span(file().map(|_| (name, Instant::now())))
}

impl Drop for Span {
    fn drop(&mut self) {
        let Some((name, t)) = self.0 else { return };
        let ms = t.elapsed().as_secs_f64() * 1000.;
        SPANS.with(|s| {
            let mut s = s.borrow_mut();
            match s.iter_mut().find(|x| x.0 == name) {
                Some(x) => {
                    x.1 += ms;
                    x.2 += 1;
                }
                None => s.push((name, ms, 1)),
            }
        });
    }
}

/// The spans since the last line, as ` name=ms/count`; forgets them.
fn take_spans() -> String {
    SPANS.with(|s| s.borrow_mut().drain(..).map(|(n, ms, k)| format!(" {n}={ms:.2}/{k}")).collect())
}

/// A line for work done outside a frame (an MCP navigate), with its spans.
pub fn note(what: &str, ms: f64) {
    if let Some(Ok(mut f)) = file().map(|f| f.lock()) {
        let _ = writeln!(f, "# {what} {ms:.3}{}", take_spans());
    }
}

fn file() -> Option<&'static std::sync::Mutex<std::fs::File>> {
    static F: OnceLock<Option<std::sync::Mutex<std::fs::File>>> = OnceLock::new();
    F.get_or_init(|| {
        let path = std::env::var_os("HOPPER_FRAME_LOG")?;
        let f = std::fs::OpenOptions::new().create(true).append(true).open(path).ok()?;
        Some(std::sync::Mutex::new(f))
    })
    .as_ref()
}

/// Called first thing in the app view's render.
pub fn start() {
    if file().is_some() {
        START.with(|s| s.set(Some(Instant::now())));
    }
}

/// The app view's last child: its paint ends the frame's measure.
pub fn end() -> Option<AnyElement> {
    file()?;
    Some(
        canvas(
            |_, _, _| {},
            |_, _, _, _| {
                let Some(t) = START.with(|s| s.take()) else { return };
                if let Some(Ok(mut f)) = file().map(|f| f.lock()) {
                    let since = BOOT.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.;
                    let ms = t.elapsed().as_secs_f64() * 1000.;
                    let _ = writeln!(f, "{since:.1} {ms:.3}{}", take_spans());
                }
            },
        )
        .absolute()
        .size_0()
        .into_any_element(),
    )
}
