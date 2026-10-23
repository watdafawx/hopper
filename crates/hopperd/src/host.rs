//! The machine's own load: CPU, memory and the servers drive, sampled once
//! a second in the background (CPU needs two samples to mean anything).

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use sysinfo::{Disks, System};

#[derive(Clone, Debug, Default, Serialize)]
pub struct HostStats {
    pub hostname: String,
    pub os: String,
    pub cpu_name: String,
    pub threads: usize,
    pub cpu: f32,
    pub mem_used: u64,
    pub mem_total: u64,
    /// The drive holding the servers folder.
    pub disk_mount: String,
    pub disk_free: u64,
    pub disk_total: u64,
    pub uptime_secs: u64,
}

pub type Shared = Arc<RwLock<HostStats>>;

/// The disk whose mount point is the longest prefix of `path`.
fn disk_for<'a>(disks: &'a Disks, path: &Path) -> Option<&'a sysinfo::Disk> {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let p = path.to_string_lossy().trim_start_matches(r"\\?\").to_lowercase();
    disks
        .list()
        .iter()
        .filter(|d| p.starts_with(&d.mount_point().to_string_lossy().to_lowercase()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
}

/// Starts sampling; the stats in the returned handle stay current.
pub fn start(servers_root: Option<PathBuf>) -> Shared {
    let shared: Shared = Default::default();
    let out = shared.clone();
    std::thread::Builder::new()
        .name("host-stats".into())
        .spawn(move || {
            let mut sys = System::new();
            let mut disks = Disks::new_with_refreshed_list();
            sys.refresh_cpu_all();
            let mut n = 0u64;
            loop {
                std::thread::sleep(Duration::from_secs(1));
                sys.refresh_cpu_all();
                sys.refresh_memory();
                if n.is_multiple_of(30) {
                    disks.refresh(true);
                }
                n += 1;
                let disk = servers_root.as_deref().and_then(|r| disk_for(&disks, r));
                let cpus = sys.cpus();
                let stats = HostStats {
                    hostname: crate::config::hostname(),
                    os: System::long_os_version().unwrap_or_default(),
                    cpu_name: cpus.first().map(|c| c.brand().trim().to_string()).unwrap_or_default(),
                    threads: cpus.len(),
                    cpu: sys.global_cpu_usage(),
                    mem_used: sys.used_memory(),
                    mem_total: sys.total_memory(),
                    disk_mount: disk.map(|d| d.mount_point().to_string_lossy().into_owned()).unwrap_or_default(),
                    disk_free: disk.map_or(0, |d| d.available_space()),
                    disk_total: disk.map_or(0, |d| d.total_space()),
                    uptime_secs: System::uptime(),
                };
                *shared.write().unwrap() = stats;
            }
        })
        .expect("host stats thread");
    out
}
