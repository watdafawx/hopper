//! What the operating system says about this process: its threads (name,
//! count, CPU time) and its memory by kind. For the memory log
//! (`HOPPER_MEMLOG=2`); nothing here runs unless that is set.

/// Threads of this process: count, CPU time, by name (`Worker-3` -> `Worker-#`).
#[cfg(windows)]
pub fn threads() -> String {
    use windows::Win32::Foundation::{CloseHandle, FILETIME, LocalFree, HLOCAL};
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next};
    use windows::Win32::System::Threading::{GetCurrentProcessId, GetThreadDescription, GetThreadTimes, OpenThread, THREAD_QUERY_LIMITED_INFORMATION};
    let pid = unsafe { GetCurrentProcessId() };
    let mut groups: std::collections::BTreeMap<String, (u32, u64)> = Default::default();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) else { return String::new() };
        let mut e = THREADENTRY32 { dwSize: size_of::<THREADENTRY32>() as u32, ..Default::default() };
        let mut ok = Thread32First(snap, &mut e).is_ok();
        while ok {
            if e.th32OwnerProcessID == pid {
                let mut name = String::from("(unnamed)");
                let mut cpu = 0u64;
                if let Ok(h) = OpenThread(THREAD_QUERY_LIMITED_INFORMATION, false, e.th32ThreadID) {
                    if let Ok(d) = GetThreadDescription(h) {
                        let s = d.to_string().unwrap_or_default();
                        if !s.is_empty() {
                            name = s.chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect();
                            name = name.replace("##", "#").replace("##", "#");
                        }
                        let _ = LocalFree(Some(HLOCAL(d.0 as *mut _)));
                    }
                    let (mut c, mut x, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
                    if GetThreadTimes(h, &mut c, &mut x, &mut k, &mut u).is_ok() {
                        let t = |f: FILETIME| ((f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64) / 10_000;
                        cpu = t(k) + t(u);
                    }
                    let _ = CloseHandle(h);
                }
                let g = groups.entry(name).or_default();
                g.0 += 1;
                g.1 += cpu;
            }
            ok = Thread32Next(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    let total: u32 = groups.values().map(|g| g.0).sum();
    let cpu: u64 = groups.values().map(|g| g.1).sum();
    let mut v: Vec<_> = groups.into_iter().collect();
    v.sort_by(|a, b| b.1.0.cmp(&a.1.0));
    let mut out = format!("threads {total}, CPU so far {:.1} s\n", cpu as f64 / 1000.0);
    for (n, (c, ms)) in v {
        out.push_str(&format!("{c:5} threads {:8.2} s CPU  {n}\n", ms as f64 / 1000.0));
    }
    out
}

#[cfg(not(windows))]
pub fn threads() -> String {
    String::new()
}

/// The process's memory as Windows sees it: each heap's allocated and
/// committed bytes (the Rust allocator and C libraries share the process
/// heap), and committed address space by kind, with the biggest private
/// regions. What is committed but not in the heap is stacks, GPU / driver
/// memory and mapped files.
#[cfg(windows)]
pub fn windows_memory() -> String {
    use windows::Win32::System::Memory::{
        GetProcessHeaps, HEAP_SUMMARY, HeapSummary, MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_IMAGE, MEM_MAPPED, MEM_PRIVATE, VirtualQuery,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;
    let mut out = String::new();
    unsafe {
        let mut heaps = [windows::Win32::Foundation::HANDLE::default(); 256];
        let n = GetProcessHeaps(&mut heaps) as usize;
        let _ = GetCurrentProcess();
        out.push_str(&format!("{n} heaps (allocated / committed MB):"));
        let mut tot = (0usize, 0usize);
        for h in heaps.iter().take(n.min(256)) {
            let mut s = HEAP_SUMMARY { cb: size_of::<HEAP_SUMMARY>() as u32, ..Default::default() };
            if HeapSummary(*h, 0, &mut s).as_bool() && s.cbCommitted > 1 << 20 {
                out.push_str(&format!(" [{:.1}/{:.1}]", s.cbAllocated as f64 / 1048576.0, s.cbCommitted as f64 / 1048576.0));
            }
            tot.0 += s.cbAllocated;
            tot.1 += s.cbCommitted;
        }
        out.push_str(&format!(" total {:.1}/{:.1}
", tot.0 as f64 / 1048576.0, tot.1 as f64 / 1048576.0));
        let (mut private, mut mapped, mut image) = (0usize, 0usize, 0usize);
        let mut big: Vec<(usize, usize, u32)> = Vec::new();
        let mut addr = 0usize;
        loop {
            let mut m = MEMORY_BASIC_INFORMATION::default();
            if VirtualQuery(Some(addr as *const _), &mut m, size_of::<MEMORY_BASIC_INFORMATION>()) == 0 {
                break;
            }
            if m.State == MEM_COMMIT {
                match m.Type {
                    MEM_PRIVATE => {
                        private += m.RegionSize;
                        if m.RegionSize >= 4 << 20 {
                            big.push((m.RegionSize, m.BaseAddress as usize, m.Protect.0));
                        }
                    }
                    MEM_MAPPED => mapped += m.RegionSize,
                    MEM_IMAGE => image += m.RegionSize,
                    _ => {}
                }
            }
            addr = m.BaseAddress as usize + m.RegionSize;
            if addr == 0 {
                break;
            }
        }
        big.sort_by(|a, b| b.0.cmp(&a.0));
        out.push_str(&format!(
            "committed: private {:.1} MB, mapped files {:.1} MB, images (exe/dlls) {:.1} MB; private regions >= 4 MB: {} totalling {:.1} MB; biggest:",
            private as f64 / 1048576.0,
            mapped as f64 / 1048576.0,
            image as f64 / 1048576.0,
            big.len(),
            big.iter().map(|b| b.0).sum::<usize>() as f64 / 1048576.0
        ));
        for b in big.iter().take(8) {
            out.push_str(&format!(" {:.1}MB@{:#x}(prot {:#x})", b.0 as f64 / 1048576.0, b.1, b.2));
        }
        out.push_str("\n");
    }
    out
}

#[cfg(not(windows))]
pub fn windows_memory() -> String {
    String::new()
}
