//! Where the heap goes: a counting global allocator that attributes live
//! bytes to the call stack that allocated them. Only built with the
//! `memprof` cargo feature (a profiling build, never shipped), and only
//! reports with `HOPPER_MEMLOG=2` (see `memlog`).
//!
//! Every allocation gets a 16-byte header holding the id of its call stack
//! (hashed from up to 20 return addresses into a fixed table); freeing
//! subtracts it again, so the table holds what is alive now, by origin.
//! Reports resolve the top stacks to function names and add them up by the
//! first Hopper / GPUI / dependency frame that is not the standard library.
//! A thread table (name, count, CPU time) shows the pools.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering::Relaxed};

const FRAMES: usize = 20;
const SLOTS: usize = 1 << 16;

struct Slot {
    hash: AtomicU64,
    live_bytes: AtomicI64,
    live_count: AtomicI64,
    total_count: AtomicU64,
    total_bytes: AtomicU64,
    frames: [AtomicUsize; FRAMES],
}

#[allow(clippy::declare_interior_mutable_const)]
const ZERO: Slot = Slot {
    hash: AtomicU64::new(0),
    live_bytes: AtomicI64::new(0),
    live_count: AtomicI64::new(0),
    total_count: AtomicU64::new(0),
    total_bytes: AtomicU64::new(0),
    frames: [const { AtomicUsize::new(0) }; FRAMES],
};

static TABLE: [Slot; SLOTS] = [ZERO; SLOTS];
static LIVE: AtomicI64 = AtomicI64::new(0);
static PEAK: AtomicI64 = AtomicI64::new(0);

thread_local! {
    /// Set while the profiler itself allocates (reports), so that is not counted.
    static INSIDE: Cell<bool> = const { Cell::new(false) };
}

pub struct Alloc;

#[cfg(windows)]
fn capture(out: &mut [usize; FRAMES]) -> usize {
    use windows::Win32::System::Diagnostics::Debug::RtlCaptureStackBackTrace;
    let mut raw = [std::ptr::null_mut::<core::ffi::c_void>(); FRAMES];
    // Skips this function and the allocator's own two frames.
    let n = unsafe { RtlCaptureStackBackTrace(3, &mut raw, None) } as usize;
    for i in 0..n {
        out[i] = raw[i] as usize;
    }
    n
}

#[cfg(not(windows))]
fn capture(_: &mut [usize; FRAMES]) -> usize {
    0
}

/// The table slot for the current stack (0 is "untracked").
fn slot_for_here() -> usize {
    let inside = INSIDE.try_with(|c| c.replace(true)).unwrap_or(true);
    if inside {
        return 0;
    }
    let mut frames = [0usize; FRAMES];
    let n = capture(&mut frames);
    let mut h: u64 = 0xcbf29ce484222325;
    for f in &frames[..n] {
        h = (h ^ *f as u64).wrapping_mul(0x100000001b3);
    }
    let h = h | 1;
    let mut i = (h as usize) % SLOTS;
    let mut found = 0;
    for _ in 0..64 {
        if i == 0 {
            i = 1;
        }
        let s = &TABLE[i];
        let cur = s.hash.load(Relaxed);
        if cur == h {
            found = i;
            break;
        }
        if cur == 0 && s.hash.compare_exchange(0, h, Relaxed, Relaxed).is_ok() {
            for (k, f) in frames[..n].iter().enumerate() {
                s.frames[k].store(*f, Relaxed);
            }
            found = i;
            break;
        }
        if s.hash.load(Relaxed) == h {
            found = i;
            break;
        }
        i = (i + 1) % SLOTS;
    }
    let _ = INSIDE.try_with(|c| c.set(false));
    found
}

unsafe impl GlobalAlloc for Alloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let off = layout.align().max(16);
        let Ok(l) = Layout::from_size_align(layout.size() + off, off) else { return std::ptr::null_mut() };
        let raw = unsafe { System.alloc(l) };
        if raw.is_null() {
            return raw;
        }
        let slot = slot_for_here();
        let user = unsafe { raw.add(off) };
        unsafe { (user.sub(8) as *mut u64).write(slot as u64) };
        let s = &TABLE[slot];
        let size = layout.size() as i64;
        s.live_bytes.fetch_add(size, Relaxed);
        s.live_count.fetch_add(1, Relaxed);
        s.total_count.fetch_add(1, Relaxed);
        s.total_bytes.fetch_add(size as u64, Relaxed);
        let now = LIVE.fetch_add(size, Relaxed) + size;
        if now > PEAK.load(Relaxed) {
            PEAK.fetch_max(now, Relaxed);
        }
        user
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let off = layout.align().max(16);
        let slot = unsafe { (ptr.sub(8) as *const u64).read() } as usize;
        let s = &TABLE[slot.min(SLOTS - 1)];
        let size = layout.size() as i64;
        s.live_bytes.fetch_sub(size, Relaxed);
        s.live_count.fetch_sub(1, Relaxed);
        LIVE.fetch_sub(size, Relaxed);
        let l = Layout::from_size_align(layout.size() + off, off).expect("layout was valid at alloc");
        unsafe { System.dealloc(ptr.sub(off), l) };
    }
}

fn clean(name: &str) -> String {
    // Drops the trailing `::h0123456789abcdef` hash.
    match name.rfind("::h") {
        Some(i) if name.len() - i == 19 => name[..i].to_string(),
        _ => name.to_string(),
    }
}

fn is_plumbing(name: &str) -> bool {
    const SKIP: [&str; 16] = [
        "alloc::", "core::", "std::", "hashbrown::", "<alloc::", "<core::", "<std::", "<hashbrown::", "__rust", "hopper::memprof", "<hopper::memprof",
        "memprof::", "RtlCapture", "serde_json::", "<serde", "serde::",
    ];
    SKIP.iter().any(|p| name.starts_with(p))
}

fn resolve(ip: usize) -> String {
    let mut out = String::new();
    backtrace::resolve(ip as *mut _, |sym| {
        if out.is_empty() {
            out = sym.name().map_or_else(|| format!("{ip:#x}"), |n| clean(&n.to_string()));
        }
    });
    if out.is_empty() { format!("{ip:#x}") } else { out }
}

pub fn live_and_peak() -> (i64, i64) {
    (LIVE.load(Relaxed), PEAK.load(Relaxed))
}

/// Live bytes by origin: the top stacks and their sums by owner frame.
pub fn report(top: usize) -> String {
    let _ = INSIDE.try_with(|c| c.set(true));
    let mut rows: Vec<(i64, i64, u64, u64, [usize; FRAMES])> = Vec::new();
    for s in TABLE.iter() {
        let b = s.live_bytes.load(Relaxed);
        if b > 0 || s.live_count.load(Relaxed) > 0 {
            let mut f = [0usize; FRAMES];
            for (k, x) in s.frames.iter().enumerate() {
                f[k] = x.load(Relaxed);
            }
            rows.push((b, s.live_count.load(Relaxed), s.total_count.load(Relaxed), s.total_bytes.load(Relaxed), f));
        }
    }
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    let (live, peak) = live_and_peak();
    let mut out = format!("heap live {:.1} MB, peak {:.1} MB, {} stacks\n", live as f64 / 1048576.0, peak as f64 / 1048576.0, rows.len());
    let mut owners: std::collections::HashMap<String, (i64, i64, u64)> = Default::default();
    let mut names: std::collections::HashMap<usize, String> = Default::default();
    for r in rows.iter().take(3000) {
        let frames: Vec<String> = r.4.iter().take_while(|f| **f != 0).map(|f| names.entry(*f).or_insert_with(|| resolve(*f)).clone()).collect();
        let owner = frames.iter().find(|n| !is_plumbing(n)).cloned().unwrap_or_else(|| "(std only)".into());
        let e = owners.entry(owner).or_default();
        e.0 += r.0;
        e.1 += r.1;
        e.2 += r.2;
    }
    let mut by_owner: Vec<_> = owners.into_iter().collect();
    by_owner.sort_by(|a, b| b.1.0.cmp(&a.1.0));
    out.push_str("-- by owner frame (live MB, live allocs, allocs ever)\n");
    for (name, (b, c, t)) in by_owner.iter().take(top) {
        out.push_str(&format!("{:8.2} MB {:9} live {:10} ever  {}\n", *b as f64 / 1048576.0, c, t, name));
    }
    out.push_str("-- top stacks\n");
    for r in rows.iter().take(8) {
        out.push_str(&format!("{:8.2} MB {} live\n", r.0 as f64 / 1048576.0, r.1));
        for f in r.4.iter().take_while(|f| **f != 0).take(10) {
            out.push_str(&format!("      {}\n", names.entry(*f).or_insert_with(|| resolve(*f))));
        }
    }
    let _ = INSIDE.try_with(|c| c.set(false));
    out
}

