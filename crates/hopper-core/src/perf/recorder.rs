//! Records one running game: samples it every second until it exits, then
//! reads its GC log and saves the session.

use super::{Sample, Session, gclog, gpu::GpuSampler, present, stats};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// What's known about a game when it's found running.
#[derive(Clone, Debug, Default)]
pub struct Start {
    pub pid: u32,
    /// Process start, seconds since the epoch.
    pub started: u64,
    pub instance: String,
    pub root: PathBuf,
    pub pack: String,
    pub game_version: String,
    pub loader: String,
    /// The arguments Hopper would start it with, for when the process's
    /// command line can't be read (Sodium rewrites it).
    pub configured_args: String,
    pub overlay: bool,
}

/// Saves the session so far this often, so closing Hopper mid-game keeps it.
const SAVE_EVERY: u32 = 60;

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// "20261015-152021" (UTC) for seconds since the epoch.
fn session_id(secs: u64) -> String {
    let iso = crate::time::iso_from_secs(secs as i64);
    let digits: String = iso.chars().take(19).filter(char::is_ascii_digit).collect();
    format!("{}-{}", &digits[..8], &digits[8..])
}

/// Reads new lines of `latest.log` for the load-finished mark.
struct LoadWatch {
    path: PathBuf,
    from: u64,
    started: u64,
    partial: String,
}

impl LoadWatch {
    /// Seconds to load, once the log shows it.
    fn poll(&mut self, elapsed: f64) -> Option<f64> {
        let mut f = std::fs::File::open(&self.path).ok()?;
        let meta = f.metadata().ok()?;
        // Last run's log until the game rewrites it.
        let fresh = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .is_some_and(|t| t.as_secs() + 1 >= self.started);
        if !fresh {
            return None;
        }
        if meta.len() < self.from {
            self.from = 0;
            self.partial.clear();
        }
        f.seek(SeekFrom::Start(self.from)).ok()?;
        let mut buf = Vec::new();
        f.take(8 << 20).read_to_end(&mut buf).ok()?;
        self.from += buf.len() as u64;
        self.partial.push_str(&String::from_utf8_lossy(&buf));
        let cut = self.partial.rfind('\n').map_or(0, |i| i + 1);
        let done: String = self.partial.drain(..cut).collect();
        done.lines().find_map(|l| super::load_mark(l, elapsed))
    }
}

/// The java.exe's Java arguments and path.
fn process_args(sys: &System, pid: Pid) -> (Vec<String>, String) {
    let Some(p) = sys.process(pid) else { return (Vec::new(), String::new()) };
    let cmd: Vec<String> = p.cmd().iter().map(|a| a.to_string_lossy().into_owned()).collect();
    let exe = p.exe().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
    (super::jvm_part(&cmd), exe)
}

/// Starts recording in the background; `done` gets the saved session.
pub fn record(start: Start, base: PathBuf, done: impl FnOnce(Session) + Send + 'static) {
    let _ = std::thread::Builder::new()
        .name(format!("record-{}", start.pid))
        .spawn(move || {
            let s = run(start, &base);
            done(s);
        });
}

fn run(start: Start, base: &Path) -> Session {
    let pid = Pid::from_u32(start.pid);
    let mut sys = System::new();
    let refresh = |sys: &mut System, first: bool| {
        let mut kind = ProcessRefreshKind::nothing().with_cpu().with_memory();
        if first {
            kind = kind.with_cmd(UpdateKind::Always).with_exe(UpdateKind::Always);
        }
        sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, kind);
        sys.process(pid).is_some()
    };
    refresh(&mut sys, true);
    let (mut jvm, java_path) = process_args(&sys, pid);
    if !jvm.iter().any(|a| a.starts_with("-Xmx")) {
        jvm = crate::jvm_args::split(&start.configured_args);
    }
    let args = super::tuning_args(&jvm);
    let (xms, xmx) = crate::jvm_args::memory(&args.join(" "));
    let mut s = Session {
        id: session_id(start.started),
        instance: start.instance.clone(),
        instance_root: start.root.to_string_lossy().into_owned(),
        pack: start.pack.clone(),
        game_version: start.game_version.clone(),
        loader: start.loader.clone(),
        java_path,
        fingerprint: super::fingerprint(&args),
        args,
        xms_mb: xms,
        xmx_mb: xmx,
        overlay: start.overlay,
        started: start.started,
        ..Default::default()
    };

    let frames_ok = match present::watch(start.pid) {
        Ok(()) => true,
        Err(e) => {
            s.frames_note = e;
            false
        }
    };
    let mut gpu = GpuSampler::new(start.pid);
    s.gpu_source = gpu.source().to_string();
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f32;
    let mut load = LoadWatch {
        path: start.root.join("logs").join("latest.log"),
        from: 0,
        started: start.started,
        partial: String::new(),
    };
    // Every frame time, and where play started (the loading screen isn't
    // play). With no load mark at all, every frame counts.
    let mut frame_times: Vec<f64> = Vec::new();
    let mut play_from: Option<usize> = None;
    let mut tick = Instant::now();
    let mut n = 0u32;
    loop {
        tick += Duration::from_secs(1);
        if let Some(wait) = tick.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        if !refresh(&mut sys, false) {
            break;
        }
        n += 1;
        let Some(p) = sys.process(pid) else { break };
        let elapsed = now().saturating_sub(start.started);
        let frames = present::take(start.pid);
        let g = gpu.sample();
        if s.load_secs.is_none() {
            s.load_secs = load.poll(elapsed as f64);
        }
        if s.load_secs.is_some() && play_from.is_none() {
            play_from = Some(frame_times.len());
        }
        let mean = stats::mean(&frames);
        s.samples.push(Sample {
            t: elapsed as u32,
            fps: frames_ok.then_some(frames.len() as f32),
            frame_ms: mean.map(|m| m as f32),
            frame_max_ms: frames.iter().copied().reduce(f64::max).map(|m| m as f32),
            cpu: p.cpu_usage() / cores,
            ram_mb: (p.memory() as f64 / (1024.0 * 1024.0)) as f32,
            gpu: g.util.map(|v| v as f32),
            vram_mb: g.vram_mb.map(|v| v as f32),
        });
        frame_times.extend(frames);
        if n.is_multiple_of(SAVE_EVERY) {
            summarize(&mut s, &frame_times[play_from.unwrap_or(0)..]);
            if let Err(e) = super::save(base, &s) {
                log::warn!("saving session: {e:#}");
            }
        }
    }
    present::unwatch(start.pid);
    s.ended = now();
    if s.load_secs.is_none() {
        s.load_secs = load.poll(s.ended.saturating_sub(start.started) as f64);
    }
    summarize(&mut s, &frame_times[play_from.unwrap_or(0)..]);
    if frames_ok && s.frames.is_none() && s.frames_note.is_empty() {
        s.frames_note = "No frames seen".into();
    }
    finish_gc(&mut s, &start.root, base);
    s.crashed = crate::logs::crash_reports(&start.root).first().is_some_and(|p| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .is_some_and(|t| t.as_secs() >= start.started)
    });
    if let Err(e) = super::save(base, &s) {
        log::warn!("saving session: {e:#}");
    }
    s
}

/// Fills the session's totals from its samples and play frame times.
fn summarize(s: &mut Session, play_frames: &[f64]) {
    s.frames = stats::FrameStats::from_frame_times(play_frames);
    let cpu: Vec<f64> = s.samples.iter().map(|x| x.cpu as f64).collect();
    let ram: Vec<f64> = s.samples.iter().map(|x| x.ram_mb as f64).collect();
    let gpu: Vec<f64> = s.samples.iter().filter_map(|x| x.gpu.map(f64::from)).collect();
    s.cpu_avg = stats::mean(&cpu).unwrap_or(0.0);
    s.cpu_max = cpu.iter().copied().fold(0.0, f64::max);
    s.ram_avg_mb = stats::mean(&ram).unwrap_or(0.0);
    s.ram_peak_mb = ram.iter().copied().fold(0.0, f64::max);
    s.gpu_avg = stats::mean(&gpu);
    s.gpu_max = gpu.iter().copied().reduce(f64::max);
    s.vram_peak_mb = s.samples.iter().filter_map(|x| x.vram_mb.map(f64::from)).reduce(f64::max);
}

/// Parses this run's GC log and moves it into the session folder.
fn finish_gc(s: &mut Session, root: &Path, base: &Path) {
    let log = root.join(super::GC_LOG);
    let fresh = std::fs::metadata(&log)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .is_some_and(|t| t.as_secs() + 1 >= s.started);
    if !fresh {
        return;
    }
    let Ok(text) = std::fs::read_to_string(&log) else { return };
    let gc = gclog::parse(&text);
    if !gc.java_version.is_empty() {
        s.java = gc.java_version.clone();
    }
    s.gc = Some(gc);
    let dir = super::instance_dir(base, root).join(&s.id);
    if std::fs::create_dir_all(&dir).is_ok() {
        let dest = dir.join("gc.log");
        if std::fs::rename(&log, &dest).is_err() && std::fs::copy(&log, &dest).is_ok() {
            let _ = std::fs::remove_file(&log);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_from_samples() {
        let mut s = Session::default();
        for (i, (cpu, ram, gpu)) in [(10.0, 1000.0, Some(50.0)), (30.0, 3000.0, None)].into_iter().enumerate() {
            s.samples.push(Sample {
                t: i as u32,
                fps: Some(2.0),
                frame_ms: Some(500.0),
                cpu,
                ram_mb: ram,
                gpu,
                ..Default::default()
            });
        }
        summarize(&mut s, &[10.0, 10.0]);
        assert_eq!(s.cpu_avg, 20.0);
        assert_eq!(s.cpu_max, 30.0);
        assert_eq!(s.ram_peak_mb, 3000.0);
        assert_eq!(s.gpu_avg, Some(50.0));
        assert_eq!(s.gpu_max, Some(50.0));
        assert_eq!(s.frames.as_ref().unwrap().avg_fps, 100.0);
        summarize(&mut s, &[]);
        assert!(s.frames.is_none());
        assert_eq!(session_id(1_760_541_621), "20251015-152021");
    }

    #[test]
    fn load_watch_reads_new_lines() {
        let dir = std::env::temp_dir().join(format!("hopper-loadwatch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("latest.log");
        std::fs::write(&p, "[main/INFO]: starting\n[Render thread/INFO]: Sound eng").unwrap();
        let mut w = LoadWatch { path: p.clone(), from: 0, started: 0, partial: String::new() };
        assert_eq!(w.poll(5.0), None);
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        std::io::Write::write_all(&mut f, b"ine started\n").unwrap();
        assert_eq!(w.poll(7.0), Some(7.0));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Records a short real Java run (allocating, so it collects) started with
/// Hopper's GC log argument, and prints the session.
/// `cargo test real_record -- --ignored --nocapture` (needs a JDK; set
/// HOPPER_TEST_JAVA to its java.exe when `java` is a JRE).
#[cfg(test)]
#[test]
#[ignore]
fn real_record() {
    let root = std::env::temp_dir().join(format!("hopper-record-{}", std::process::id()));
    std::fs::create_dir_all(root.join("logs")).unwrap();
    std::fs::write(
        root.join("Alloc.java"),
        "public class Alloc { public static void main(String[] a) throws Exception {
            java.util.List<byte[]> keep = new java.util.ArrayList<>();
            System.out.println(\"Game took 2.5 seconds to start\");
            long end = System.currentTimeMillis() + 8000;
            while (System.currentTimeMillis() < end) {
                keep.add(new byte[1 << 16]);
                if (keep.size() > 2000) keep.clear();
            }
        } }",
    )
    .unwrap();
    let java = std::env::var("HOPPER_TEST_JAVA").unwrap_or_else(|_| "java".into());
    let child = std::process::Command::new(java)
        .args(["-Xms64m", "-Xmx256m", "-XX:+UseG1GC", &super::gc_log_arg(), "Alloc.java"])
        .current_dir(&root)
        .stdout(std::fs::File::create(root.join("logs").join("latest.log")).unwrap())
        .spawn()
        .unwrap();
    let start = Start {
        pid: child.id(),
        started: now(),
        instance: "Record Test".into(),
        root: root.clone(),
        configured_args: "-Xmx1G".into(),
        ..Default::default()
    };
    let base = root.join("sessions");
    let (tx, rx) = std::sync::mpsc::channel();
    record(start, base.clone(), move |s| tx.send(s).unwrap());
    let s = rx.recv_timeout(Duration::from_secs(60)).unwrap();
    println!("{}", serde_json::to_string_pretty(&Session { samples: s.samples[..2.min(s.samples.len())].to_vec(), ..s.clone() }).unwrap());
    assert!(s.gc.as_ref().is_some_and(|g| g.pauses > 0));
    assert_eq!(s.load_secs, Some(2.5));
    assert!(s.args.contains(&"-XX:+UseG1GC".to_string()));
    assert!(super::instance_dir(&base, &root).join(&s.id).join("gc.log").exists());
    let _ = std::fs::remove_dir_all(&root);
}
