//! Gives free heap memory back to Windows after a burst of work.
//!
//! Scanning a modpack (every jar opened, nested jars read whole) makes the
//! heap grow by a few hundred MB for a few seconds. When the scan ends the
//! memory is free again, but the Windows heap keeps the pages committed, so
//! the process stays that big (Task Manager shows it as in use) until the app
//! closes. `HeapCompact` decommits the free pages. [`trim_soon`] asks for one
//! a few seconds after the last burst, once, however many bursts ask.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long after the last request the heap is compacted: long enough for a
/// scan's worker threads to have let go of their buffers.
const QUIET: Duration = Duration::from_secs(3);

static DEADLINE_MS: AtomicU64 = AtomicU64::new(0);
static WORKER: AtomicBool = AtomicBool::new(false);

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// Compacts the heap now (Windows; elsewhere nothing). Returns the size of
/// the largest free block, as `HeapCompact` does.
pub fn trim_now() -> usize {
    #[cfg(windows)]
    {
        unsafe extern "system" {
            fn GetProcessHeap() -> *mut core::ffi::c_void;
            fn HeapCompact(heap: *mut core::ffi::c_void, flags: u32) -> usize;
        }
        // SAFETY: both are plain kernel32 calls on the process's own heap.
        unsafe { HeapCompact(GetProcessHeap(), 0) }
    }
    #[cfg(not(windows))]
    {
        0
    }
}

/// Compacts the heap [`QUIET`] after the last call. Cheap to call often.
pub fn trim_soon() {
    DEADLINE_MS.store(now_ms() + QUIET.as_millis() as u64, Ordering::Relaxed);
    if WORKER.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new().name("heap-trim".into()).spawn(|| {
        loop {
            let due = DEADLINE_MS.load(Ordering::Relaxed);
            let now = now_ms();
            if now < due {
                std::thread::sleep(Duration::from_millis(due - now));
                continue;
            }
            let t = Instant::now();
            trim_now();
            log::debug!("heap compacted in {} ms", t.elapsed().as_millis());
            // Idle until asked again; a request that came in meanwhile has
            // moved the deadline and is seen here.
            WORKER.store(false, Ordering::Release);
            if DEADLINE_MS.load(Ordering::Relaxed) > now_ms() && !WORKER.swap(true, Ordering::AcqRel) {
                continue;
            }
            return;
        }
    });
    if spawned.is_err() {
        WORKER.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn trimming_is_harmless_and_repeatable() {
        super::trim_now();
        super::trim_soon();
        super::trim_soon();
        super::trim_now();
    }
}
