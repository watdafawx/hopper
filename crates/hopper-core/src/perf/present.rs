//! Frames per process from Windows' own present events (ETW), the way
//! PresentMon counts them: nothing is injected into the game.
//!
//! One real-time trace session listens to the graphics kernel (DxgKrnl),
//! DXGI and D3D9 present events. A frame can show up as several of those
//! events (a present, then its flip or blit), so each process is counted
//! by whichever single event it sends most, and frame times are the gaps
//! between those.
//!
//! Starting the session needs admin or the "Performance Log Users" group.

use std::collections::HashMap;

/// Frame timestamps per event kind for one process.
#[derive(Default, Debug)]
pub struct FrameCounter {
    keys: HashMap<u32, KeyState>,
}

#[derive(Default, Debug)]
struct KeyState {
    total: u64,
    last: Option<i64>,
    pending: Vec<i64>,
}

/// Timestamps kept per kind between takes, so a stalled reader can't
/// grow memory without end.
const MAX_PENDING: usize = 200_000;

impl FrameCounter {
    /// An event of kind `key` at `ts` (performance-counter ticks).
    pub fn push(&mut self, key: u32, ts: i64) {
        let k = self.keys.entry(key).or_default();
        if k.pending.len() < MAX_PENDING {
            k.pending.push(ts);
        }
    }

    /// Frame times (ms) since the last take, from the kind seen most.
    pub fn take(&mut self, ticks_per_ms: f64) -> Vec<f64> {
        for k in self.keys.values_mut() {
            k.total += k.pending.len() as u64;
            k.pending.sort_unstable();
        }
        let dominant = self
            .keys
            .iter()
            .max_by(|a, b| a.1.total.cmp(&b.1.total).then(b.0.cmp(a.0)))
            .map(|(k, _)| *k);
        let mut out = Vec::new();
        for (key, k) in self.keys.iter_mut() {
            if Some(*key) == dominant {
                let mut prev = k.last;
                for &ts in &k.pending {
                    if let Some(p) = prev
                        && ts > p
                    {
                        out.push((ts - p) as f64 / ticks_per_ms);
                    }
                    prev = Some(ts);
                }
            }
            if let Some(&l) = k.pending.last() {
                k.last = Some(k.last.map_or(l, |o| o.max(l)));
            }
            k.pending.clear();
        }
        out
    }
}

#[cfg(windows)]
pub use win::{take, unwatch, watch};

#[cfg(not(windows))]
pub fn watch(_pid: u32) -> Result<(), String> {
    Err("FPS is only recorded on Windows".into())
}
#[cfg(not(windows))]
pub fn take(_pid: u32) -> Vec<f64> {
    Vec::new()
}
#[cfg(not(windows))]
pub fn unwatch(_pid: u32) {}

#[cfg(windows)]
mod win {
    use super::FrameCounter;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_SUCCESS};
    use windows::Win32::System::Diagnostics::Etw::*;
    use windows::core::{GUID, PCWSTR, PWSTR};

    const SESSION: &str = "Hopper-Frames";
    const DXGKRNL: GUID = GUID::from_u128(0x802ec45a_1e99_4b83_9920_87c98277ba9d);
    const DXGI: GUID = GUID::from_u128(0xca11c036_0102_4a2d_a6ad_f03cfed5d3c9);
    const D3D9: GUID = GUID::from_u128(0x783aca0a_790e_4d7f_8451_aa850511c6b9);
    /// DxgKrnl: Blit_Info, Flip_Info, Present_Info, PresentHistory_Start,
    /// PresentHistoryDetailed_Start, FlipMultiPlaneOverlay_Info.
    const DXGKRNL_IDS: [u16; 6] = [166, 168, 184, 171, 215, 252];
    /// DXGI and D3D9: Present_Start.
    const DXGI_IDS: [u16; 1] = [42];
    const D3D9_IDS: [u16; 1] = [1];
    /// DxgKrnl keywords: Base | Present | Performance.
    const DXGKRNL_KEYWORDS: u64 = 0x1 | 0x800_0000 | 0x4000_0000_0000_0000;

    struct State {
        pids: HashMap<u32, FrameCounter>,
        session: Option<Session>,
    }

    struct Session {
        handle: CONTROLTRACE_HANDLE,
    }

    fn state() -> &'static Mutex<State> {
        static S: OnceLock<Mutex<State>> = OnceLock::new();
        S.get_or_init(|| {
            Mutex::new(State {
                pids: HashMap::new(),
                session: None,
            })
        })
    }

    fn ticks_per_ms() -> f64 {
        let mut f = 0i64;
        unsafe {
            let _ = windows::Win32::System::Performance::QueryPerformanceFrequency(&mut f);
        }
        (f.max(1) as f64) / 1000.0
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// EVENT_TRACE_PROPERTIES with room for the session name after it.
    fn properties() -> Vec<u64> {
        let name_bytes = (SESSION.len() + 1) * 2;
        let size = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() + name_bytes;
        let mut buf = vec![0u64; size.div_ceil(8)];
        let p = buf.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES;
        unsafe {
            (*p).Wnode.BufferSize = size as u32;
            (*p).Wnode.Flags = WNODE_FLAG_TRACED_GUID;
            // Timestamps in performance-counter ticks.
            (*p).Wnode.ClientContext = 1;
            (*p).LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
            (*p).FlushTimer = 1;
            (*p).LoggerNameOffset = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() as u32;
        }
        buf
    }

    fn stop_named() {
        let mut props = properties();
        let name = wide(SESSION);
        unsafe {
            let _ = ControlTraceW(
                CONTROLTRACE_HANDLE::default(),
                PCWSTR(name.as_ptr()),
                props.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES,
                EVENT_TRACE_CONTROL_STOP,
            );
        }
    }

    fn enable(handle: CONTROLTRACE_HANDLE, provider: &GUID, keywords: u64, ids: &[u16]) {
        // EVENT_FILTER_EVENT_ID: FilterIn, Reserved, Count, then the ids.
        let mut filter: Vec<u16> = vec![1, ids.len() as u16];
        filter.extend_from_slice(ids);
        let mut desc = EVENT_FILTER_DESCRIPTOR {
            Ptr: filter.as_ptr() as u64,
            Size: (filter.len() * 2) as u32,
            Type: EVENT_FILTER_TYPE_EVENT_ID,
        };
        let params = ENABLE_TRACE_PARAMETERS {
            Version: ENABLE_TRACE_PARAMETERS_VERSION_2,
            EnableFilterDesc: &mut desc,
            FilterDescCount: 1,
            ..Default::default()
        };
        let r = unsafe {
            EnableTraceEx2(
                handle,
                provider,
                EVENT_CONTROL_CODE_ENABLE_PROVIDER.0,
                5,
                keywords,
                0,
                0,
                Some(&params),
            )
        };
        if r != ERROR_SUCCESS {
            log::warn!("enabling present events from {provider:?}: {r:?}");
        }
    }

    unsafe extern "system" fn on_event(rec: *mut EVENT_RECORD) {
        let Some(rec) = (unsafe { rec.as_ref() }) else { return };
        let h = &rec.EventHeader;
        let provider = if h.ProviderId == DXGKRNL {
            0u32
        } else if h.ProviderId == DXGI {
            1
        } else if h.ProviderId == D3D9 {
            2
        } else {
            return;
        };
        let key = (provider << 16) | h.EventDescriptor.Id as u32;
        if let Ok(mut s) = state().lock()
            && let Some(c) = s.pids.get_mut(&h.ProcessId)
        {
            c.push(key, h.TimeStamp);
        }
    }

    fn start() -> Result<Session, String> {
        let name = wide(SESSION);
        let mut handle = CONTROLTRACE_HANDLE::default();
        let mut r = ERROR_ALREADY_EXISTS;
        for _ in 0..2 {
            let mut props = properties();
            r = unsafe {
                StartTraceW(
                    &mut handle,
                    PCWSTR(name.as_ptr()),
                    props.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES,
                )
            };
            if r != ERROR_ALREADY_EXISTS {
                break;
            }
            // Left over from a Hopper that didn't close cleanly.
            stop_named();
        }
        if r == ERROR_ACCESS_DENIED {
            return Err(
                "FPS needs Hopper to run as admin, or your account in the Performance Log Users group"
                    .into(),
            );
        }
        if r != ERROR_SUCCESS {
            return Err(format!("Windows wouldn't start frame tracing ({})", r.0));
        }
        enable(handle, &DXGKRNL, DXGKRNL_KEYWORDS, &DXGKRNL_IDS);
        enable(handle, &DXGI, 0, &DXGI_IDS);
        enable(handle, &D3D9, 0, &D3D9_IDS);

        std::thread::Builder::new()
            .name("frame-trace".into())
            .spawn(move || {
                let mut name = wide(SESSION);
                let mut log: EVENT_TRACE_LOGFILEW = unsafe { std::mem::zeroed() };
                log.LoggerName = PWSTR(name.as_mut_ptr());
                log.Anonymous1.ProcessTraceMode =
                    PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
                log.Anonymous2.EventRecordCallback = Some(on_event);
                let trace = unsafe { OpenTraceW(&mut log) };
                if trace.Value == u64::MAX {
                    log::warn!("opening the frame trace failed");
                    return;
                }
                // Runs until the session stops.
                unsafe {
                    let _ = ProcessTrace(&[trace], None, None);
                    let _ = CloseTrace(trace);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Session { handle })
    }

    /// Starts counting frames of `pid` (and the trace, if it isn't running).
    pub fn watch(pid: u32) -> Result<(), String> {
        let mut s = state().lock().map_err(|e| e.to_string())?;
        if s.session.is_none() {
            s.session = Some(start()?);
        }
        s.pids.entry(pid).or_default();
        Ok(())
    }

    /// Frame times (ms) of `pid` since the last call.
    pub fn take(pid: u32) -> Vec<f64> {
        let tpm = ticks_per_ms();
        let Ok(mut s) = state().lock() else { return Vec::new() };
        s.pids.get_mut(&pid).map(|c| c.take(tpm)).unwrap_or_default()
    }

    /// Stops counting `pid`; the trace stops with the last one.
    pub fn unwatch(pid: u32) {
        let Ok(mut s) = state().lock() else { return };
        s.pids.remove(&pid);
        if s.pids.is_empty()
            && let Some(sess) = s.session.take()
        {
            let mut props = properties();
            unsafe {
                let _ = ControlTraceW(
                    sess.handle,
                    PCWSTR::null(),
                    props.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES,
                    EVENT_TRACE_CONTROL_STOP,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_the_busiest_event_kind() {
        let mut c = FrameCounter::default();
        // Kind 1 every 10 ticks (a frame), kind 2 every other frame.
        for i in 0..10 {
            c.push(1, i * 10);
            if i % 2 == 0 {
                c.push(2, i * 10 + 1);
            }
        }
        let t = c.take(1.0);
        assert_eq!(t, vec![10.0; 9]);
        // The gap across takes counts, from the last frame before.
        c.push(1, 100);
        c.push(1, 115);
        assert_eq!(c.take(1.0), vec![10.0, 15.0]);
        assert!(c.take(1.0).is_empty());
    }

    #[test]
    fn out_of_order_timestamps_sort() {
        let mut c = FrameCounter::default();
        for ts in [30, 10, 20, 0] {
            c.push(7, ts);
        }
        assert_eq!(c.take(2.0), vec![5.0, 5.0, 5.0]);
    }
}

/// Counts a running process's frames for 3 seconds.
/// `$env:FRAMES_PID=1234; cargo test real_frames -- --ignored --nocapture`
#[cfg(test)]
#[test]
#[ignore]
fn real_frames() {
    let pid: u32 = std::env::var("FRAMES_PID").unwrap().parse().unwrap();
    watch(pid).unwrap();
    let mut all = Vec::new();
    for _ in 0..3 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let t = take(pid);
        println!("{} frames, avg {:?} ms", t.len(), super::stats::mean(&t));
        all.extend(t);
    }
    unwatch(pid);
    println!("{:?}", super::stats::FrameStats::from_frame_times(&all));
}
