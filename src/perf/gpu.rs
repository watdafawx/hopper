//! GPU load while a game runs: NVML on NVIDIA cards, else Windows' GPU
//! engine counters for the game's process. Video memory comes from the
//! process's own counter when Windows has it, else NVML's card total.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GpuSample {
    /// Percent busy.
    pub util: Option<f64>,
    /// Video memory in use, in MB.
    pub vram_mb: Option<f64>,
}

#[cfg(windows)]
pub use win::GpuSampler;

#[cfg(not(windows))]
pub struct GpuSampler;
#[cfg(not(windows))]
impl GpuSampler {
    pub fn new(_pid: u32) -> Self {
        Self
    }
    pub fn sample(&mut self) -> GpuSample {
        GpuSample::default()
    }
    pub fn source(&self) -> &'static str {
        ""
    }
}

#[cfg(windows)]
mod win {
    use super::GpuSample;
    use std::sync::OnceLock;
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows::Win32::System::Performance::*;
    use windows::core::{PCWSTR, s};

    type Device = *mut core::ffi::c_void;

    #[repr(C)]
    #[derive(Default)]
    struct Utilization {
        gpu: u32,
        memory: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct Memory {
        total: u64,
        free: u64,
        used: u64,
    }

    struct Nvml {
        devices: Vec<usize>,
        utilization: unsafe extern "C" fn(Device, *mut Utilization) -> i32,
        memory: unsafe extern "C" fn(Device, *mut Memory) -> i32,
    }

    /// NVML, loaded once; `None` without an NVIDIA driver.
    fn nvml() -> Option<&'static Nvml> {
        static N: OnceLock<Option<Nvml>> = OnceLock::new();
        N.get_or_init(|| unsafe { load_nvml() }).as_ref()
    }

    unsafe fn load_nvml() -> Option<Nvml> {
        let name: Vec<u16> = "nvml.dll\0".encode_utf16().collect();
        let lib = unsafe { LoadLibraryW(PCWSTR(name.as_ptr())) }.ok().or_else(|| {
            let p: Vec<u16> = r"C:\Program Files\NVIDIA Corporation\NVSMI\nvml.dll"
                .encode_utf16()
                .chain([0])
                .collect();
            unsafe { LoadLibraryW(PCWSTR(p.as_ptr())) }.ok()
        })?;
        unsafe {
            let init: unsafe extern "C" fn() -> i32 =
                std::mem::transmute(GetProcAddress(lib, s!("nvmlInit_v2"))?);
            let count: unsafe extern "C" fn(*mut u32) -> i32 =
                std::mem::transmute(GetProcAddress(lib, s!("nvmlDeviceGetCount_v2"))?);
            let by_index: unsafe extern "C" fn(u32, *mut Device) -> i32 =
                std::mem::transmute(GetProcAddress(lib, s!("nvmlDeviceGetHandleByIndex_v2"))?);
            let utilization = std::mem::transmute::<_, unsafe extern "C" fn(Device, *mut Utilization) -> i32>(
                GetProcAddress(lib, s!("nvmlDeviceGetUtilizationRates"))?,
            );
            let memory = std::mem::transmute::<_, unsafe extern "C" fn(Device, *mut Memory) -> i32>(
                GetProcAddress(lib, s!("nvmlDeviceGetMemoryInfo"))?,
            );
            if init() != 0 {
                return None;
            }
            let mut n = 0u32;
            if count(&mut n) != 0 || n == 0 {
                return None;
            }
            let devices = (0..n)
                .filter_map(|i| {
                    let mut d: Device = std::ptr::null_mut();
                    (by_index(i, &mut d) == 0).then_some(d as usize)
                })
                .collect();
            Some(Nvml {
                devices,
                utilization,
                memory,
            })
        }
    }

    impl Nvml {
        /// The busiest card's load and memory in use.
        fn sample(&self) -> Option<(f64, f64)> {
            self.devices
                .iter()
                .filter_map(|&d| unsafe {
                    let mut u = Utilization::default();
                    let mut m = Memory::default();
                    ((self.utilization)(d as Device, &mut u) == 0).then_some(())?;
                    let used = if (self.memory)(d as Device, &mut m) == 0 {
                        m.used as f64 / (1024.0 * 1024.0)
                    } else {
                        0.0
                    };
                    Some((u.gpu as f64, used))
                })
                .max_by(|a, b| a.0.total_cmp(&b.0))
        }
    }

    /// Windows' per-process GPU counters.
    struct Counters {
        query: PDH_HQUERY,
        engine: PDH_HCOUNTER,
        memory: PDH_HCOUNTER,
    }

    impl Counters {
        fn new(pid: u32) -> Option<Self> {
            unsafe {
                let mut query = PDH_HQUERY::default();
                if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0 {
                    return None;
                }
                let add = |path: String| {
                    let w: Vec<u16> = path.encode_utf16().chain([0]).collect();
                    let mut c = PDH_HCOUNTER::default();
                    (PdhAddEnglishCounterW(query, PCWSTR(w.as_ptr()), 0, &mut c) == 0).then_some(c)
                };
                let engine = add(format!(
                    r"\GPU Engine(pid_{pid}_*engtype_3D)\Utilization Percentage"
                ));
                let memory = add(format!(r"\GPU Process Memory(pid_{pid}_*)\Dedicated Usage"));
                let (Some(engine), Some(memory)) = (engine, memory) else {
                    PdhCloseQuery(query);
                    return None;
                };
                PdhCollectQueryData(query);
                Some(Self {
                    query,
                    engine,
                    memory,
                })
            }
        }

        /// The sum over every instance of a wildcard counter.
        fn sum(c: PDH_HCOUNTER) -> Option<f64> {
            unsafe {
                let (mut size, mut count) = (0u32, 0u32);
                PdhGetFormattedCounterArrayW(c, PDH_FMT_DOUBLE, &mut size, &mut count, None);
                if size == 0 {
                    return None;
                }
                // u64s keep the items aligned.
                let mut buf = vec![0u64; (size as usize).div_ceil(8)];
                let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
                if PdhGetFormattedCounterArrayW(c, PDH_FMT_DOUBLE, &mut size, &mut count, Some(items))
                    != 0
                {
                    return None;
                }
                let items = std::slice::from_raw_parts(items, count as usize);
                Some(
                    items
                        .iter()
                        .filter(|i| i.FmtValue.CStatus == 0)
                        .map(|i| i.FmtValue.Anonymous.doubleValue)
                        .sum(),
                )
            }
        }

        fn sample(&self) -> (Option<f64>, Option<f64>) {
            unsafe {
                PdhCollectQueryData(self.query);
            }
            (
                Self::sum(self.engine).map(|v| v.min(100.0)),
                Self::sum(self.memory).map(|b| b / (1024.0 * 1024.0)),
            )
        }
    }

    impl Drop for Counters {
        fn drop(&mut self) {
            unsafe {
                PdhCloseQuery(self.query);
            }
        }
    }

    /// Samples one game's GPU use.
    pub struct GpuSampler {
        counters: Option<Counters>,
    }

    // PDH handles are used from the one recording thread.
    unsafe impl Send for GpuSampler {}

    impl GpuSampler {
        pub fn new(pid: u32) -> Self {
            Self {
                counters: Counters::new(pid),
            }
        }

        /// Where the load figure comes from: "NVML", "Windows" or "".
        pub fn source(&self) -> &'static str {
            if nvml().is_some() {
                "NVML"
            } else if self.counters.is_some() {
                "Windows"
            } else {
                ""
            }
        }

        pub fn sample(&mut self) -> GpuSample {
            let (pdh_util, pdh_mem) = self.counters.as_ref().map_or((None, None), |c| c.sample());
            match nvml().and_then(Nvml::sample) {
                Some((util, used)) => GpuSample {
                    util: Some(util),
                    vram_mb: pdh_mem.filter(|m| *m > 0.0).or(Some(used)),
                },
                None => GpuSample {
                    util: pdh_util,
                    vram_mb: pdh_mem,
                },
            }
        }
    }
}
