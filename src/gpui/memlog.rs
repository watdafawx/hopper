//! A memory log for finding what holds memory, off unless `HOPPER_MEMLOG=1`.
//!
//! Every 15 seconds and on each page change it appends one line to
//! `memlog.txt` in Hopper's data folder: the process's working set and
//! private bytes, the pictures held by the icon cache per kind, the
//! animated logos, the image table, each loaded instance (mods, file sizes,
//! size of what is parsed) and the lengths of the store's caches. Nothing
//! runs per frame; with the variable unset nothing runs at all.

use crate::store::Store;
use gpui_kit::*;
use std::io::Write as _;
use std::time::Duration;

const EVERY: Duration = Duration::from_secs(15);
/// The file is cut back to its newest half when it grows past this.
const MAX_BYTES: u64 = 2 << 20;

struct Holder(Entity<Store>);

impl gpui_kit::Global for Holder {}

pub fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("HOPPER_MEMLOG").is_ok_and(|v| v == "1"))
}

/// Starts the timer (when the log is on).
pub fn start(store: Entity<Store>, cx: &mut App) {
    if !enabled() {
        return;
    }
    cx.set_global(Holder(store));
    write("start", cx);
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(EVERY).await;
            cx.update(|cx| write("tick", cx));
        }
    })
    .detach();
}

/// A line now, for `why` (a page change).
pub fn mark(why: &str, cx: &mut App) {
    if enabled() {
        write(why, cx);
    }
}

/// (working set, private bytes) of this process.
#[cfg(windows)]
pub fn process_bytes() -> Option<(u64, u64)> {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
    use windows::Win32::System::Threading::GetCurrentProcess;
    let mut c = PROCESS_MEMORY_COUNTERS_EX::default();
    c.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut c as *mut _ as *mut PROCESS_MEMORY_COUNTERS, c.cb) }
        .ok()
        .map(|_| (c.WorkingSetSize as u64, c.PrivateUsage as u64))
}

#[cfg(not(windows))]
pub fn process_bytes() -> Option<(u64, u64)> {
    None
}

fn mb(b: u64) -> String {
    format!("{:.1}", b as f64 / 1048576.0)
}

fn write(why: &str, cx: &mut App) {
    let Some(dir) = hopper::paths::dirs().map(|d| d.data_dir().to_path_buf()) else { return };
    let started = std::time::Instant::now();
    let (ws, private) = process_bytes().unwrap_or_default();
    let mut line = format!("{} [{why}] ws={}MB private={}MB", hopper::time::iso_from_secs(now_secs() as i64), mb(ws), mb(private));
    let mut pics = 0u64;
    let mut parts = Vec::new();
    for (cat, n, bytes) in crate::thumbs::stats(cx) {
        pics += bytes as u64;
        parts.push(format!("{}={n}/{}MB", cat.name(), mb(bytes as u64)));
    }
    line.push_str(&format!(" | pictures held {}MB: {} | private minus pictures {}MB", mb(pics), parts.join(" "), mb(private.saturating_sub(pics))));
    if let Some(h) = cx.try_global::<Holder>() {
        line.push_str(&h.0.read(cx).mem_report());
    }
    // How long the report took, to show it is cheap.
    line.push_str(&format!(" | report took {} ms\n", started.elapsed().as_millis()));
    let path = dir.join("memlog.txt");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES)
        && let Ok(text) = std::fs::read_to_string(&path)
    {
        let _ = std::fs::write(&path, &text[text.len() / 2..]);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}
