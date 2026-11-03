//! A frame-time log for measuring how smooth the window is (scrolling in
//! particular). Off unless `HOPPER_FRAME_LOG=<file>` is set: then each
//! frame the app view draws appends one line to that file: when it was
//! drawn (ms since the first logged frame), then the milliseconds from the
//! start of its render to the end of its paint (building, laying out and
//! painting every page under it).

use gpui_kit::*;
use std::cell::Cell;
use std::io::Write;
use std::sync::OnceLock;
use std::time::Instant;

static BOOT: OnceLock<Instant> = OnceLock::new();

thread_local! {
    static START: Cell<Option<Instant>> = const { Cell::new(None) };
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
                    let _ = writeln!(f, "{since:.1} {:.3}", t.elapsed().as_secs_f64() * 1000.);
                }
            },
        )
        .absolute()
        .size_0()
        .into_any_element(),
    )
}
