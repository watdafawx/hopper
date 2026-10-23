//! How each play session ran, to compare Java argument presets: FPS and
//! frame times, CPU, RAM and GPU while the game runs, GC pauses and peak
//! heap from Java's GC log after it exits, and how long it took to load.
//!
//! Sessions are JSON files under Hopper's data folder,
//! `sessions/<instance folder>/<id>/session.json`, with the run's `gc.log`
//! beside them.

pub mod gclog;
pub mod gpu;
pub mod present;
pub mod recorder;
pub mod stats;

use gclog::GcStats;
use serde::{Deserialize, Serialize};
use stats::FrameStats;
use std::path::{Path, PathBuf};

/// Where Java writes the GC log, relative to the game folder (Java runs
/// there with both launchers). Relative because `-Xlog` splits on `:`, so
/// a `C:\…` path can't go in it. Moved into the session when the game ends.
pub const GC_LOG: &str = "logs/hopper-gc.log";

/// The `-Xlog` argument that writes [`GC_LOG`]: every GC event, with
/// uptime, and no rotation (one file per run).
pub fn gc_log_arg() -> String {
    format!("-Xlog:gc*:file={GC_LOG}:uptime,level,tags:filecount=0")
}

/// Whether to add [`gc_log_arg`]: Java 9+ only (8 refuses `-Xlog` and
/// won't start), and not when the arguments already log GC themselves.
pub fn wants_gc_log(java_major: u32, args: &str) -> bool {
    java_major >= 9 && !args.contains("-Xlog:gc") && !args.contains("-Xloggc") && !args.contains("-verbose:gc")
}

/// One second of a session.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sample {
    /// Seconds since the game started.
    pub t: u32,
    pub fps: Option<f32>,
    /// Average and slowest frame time in this second, ms.
    pub frame_ms: Option<f32>,
    pub frame_max_ms: Option<f32>,
    /// Percent of the whole CPU (all cores).
    pub cpu: f32,
    /// Working set, MB.
    pub ram_mb: f32,
    pub gpu: Option<f32>,
    pub vram_mb: Option<f32>,
}

/// One play session.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// "20261015-152021", the folder name.
    pub id: String,
    pub instance: String,
    pub instance_root: String,
    /// "TNP Limitless 8 8.1.2", empty for instances without a pack.
    pub pack: String,
    pub game_version: String,
    pub loader: String,
    /// Java version ("21.0.5+9-LTS") and the java.exe that ran.
    pub java: String,
    pub java_path: String,
    /// The Java arguments that set how it runs (see [`tuning_args`]).
    pub args: Vec<String>,
    /// Short hash of `args`: runs with the same one compare together.
    pub fingerprint: String,
    pub xms_mb: Option<u32>,
    pub xmx_mb: Option<u32>,
    /// Hopper's overlay was showing.
    pub overlay: bool,
    /// Seconds since the epoch.
    pub started: u64,
    /// 0 while it runs (or when Hopper closed first).
    pub ended: u64,
    /// Seconds from the process starting until the game finished loading.
    pub load_secs: Option<f64>,
    /// Frames after loading.
    pub frames: Option<FrameStats>,
    /// Why there's no FPS, when there isn't.
    pub frames_note: String,
    pub cpu_avg: f64,
    pub cpu_max: f64,
    pub ram_avg_mb: f64,
    pub ram_peak_mb: f64,
    pub gpu_avg: Option<f64>,
    pub gpu_max: Option<f64>,
    pub vram_peak_mb: Option<f64>,
    /// "NVML" or "Windows".
    pub gpu_source: String,
    pub gc: Option<GcStats>,
    /// The game crashed (a crash report appeared).
    pub crashed: bool,
    pub samples: Vec<Sample>,
}

impl Session {
    pub fn seconds(&self) -> u64 {
        self.ended.saturating_sub(self.started)
    }
}

/// Hopper's `sessions` folder.
pub fn sessions_dir() -> Option<PathBuf> {
    crate::paths::dirs().map(|d| d.data_dir().join("sessions"))
}

/// The folder an instance's sessions go in.
pub fn instance_dir(base: &Path, root: &Path) -> PathBuf {
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    base.join(crate::instances::Instance::folder_name(&name))
}

pub fn save(base: &Path, s: &Session) -> anyhow::Result<PathBuf> {
    let dir = instance_dir(base, Path::new(&s.instance_root)).join(&s.id);
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join("session.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(s)?)?;
    std::fs::rename(&tmp, dir.join("session.json"))?;
    Ok(dir)
}

/// An instance's sessions, newest first.
pub fn load_all(base: &Path, root: &Path) -> Vec<Session> {
    let mut out: Vec<Session> = std::fs::read_dir(instance_dir(base, root))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| std::fs::read(e.path().join("session.json")).ok())
        .filter_map(|b| serde_json::from_slice(&b).ok())
        .collect();
    out.sort_by(|a: &Session, b| b.started.cmp(&a.started));
    out
}

/// Deletes one session's folder.
pub fn delete(base: &Path, root: &Path, id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!id.is_empty() && !id.contains(['/', '\\', '.']), "bad session id");
    std::fs::remove_dir_all(instance_dir(base, root).join(id))?;
    Ok(())
}

/// The arguments that change how Java runs: `-X…`/`-XX:…` flags and the
/// few `-D` properties the generator sets for speed (Graal, NIO), without
/// Hopper's GC logging. Memory is written the same way however it was
/// given ("-Xmx8G" and "-Xmx8192m" match). Sorted, so order doesn't count.
pub fn tuning_args(tokens: &[String]) -> Vec<String> {
    let mut v: Vec<String> = tokens
        .iter()
        .filter(|t| {
            (t.starts_with("-X") && !t.starts_with("-Xlog") && !t.starts_with("-Xloggc"))
                || t.starts_with("-Dgraal.")
                || t.starts_with("-Djdk.graal.")
                || t.starts_with("-Djdk.nio.")
        })
        .map(|t| {
            for p in ["-Xmx", "-Xms"] {
                if let Some(mb) = t.strip_prefix(p).and_then(crate::jvm_args::parse_mb) {
                    return format!("{p}{}", crate::jvm_args::format_mb(mb));
                }
            }
            t.clone()
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

/// The JVM's own arguments in a Java command line: everything before the
/// main class (or `-cp`'s value and the class after it).
pub fn jvm_part(cmd: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 1; // skip java.exe
    while i < cmd.len() {
        let a = &cmd[i];
        if a == "-cp" || a == "-classpath" || a == "-p" || a == "--module-path" || a == "--add-modules" {
            i += 2;
            continue;
        }
        if !a.starts_with('-') {
            break;
        }
        out.push(a.clone());
        i += 1;
    }
    out
}

/// Short stable hash of tuning args.
pub fn fingerprint(tuning: &[String]) -> String {
    let mut h = sha1_smol::Sha1::new();
    h.update(tuning.join(" ").as_bytes());
    h.digest().to_string()[..8].to_string()
}

/// `tuning` without memory flags.
fn without_memory(tuning: &[String]) -> Vec<String> {
    tuning
        .iter()
        .filter(|t| !t.starts_with("-Xmx") && !t.starts_with("-Xms"))
        .cloned()
        .collect()
}

/// The saved preset whose arguments these are. A preset with memory flags
/// must match them too; one without matches any memory.
pub fn preset_name<'a>(tuning: &[String], presets: &'a [crate::settings::JavaPreset]) -> Option<&'a str> {
    let ours = without_memory(tuning);
    presets
        .iter()
        .find(|p| {
            let theirs = tuning_args(&crate::jvm_args::split(&p.args));
            let (xms, xmx) = crate::jvm_args::memory(&p.args);
            if xms.is_some() || xmx.is_some() {
                let mut mine = tuning.to_vec();
                // A preset without -Xms leaves the run's -Xms free.
                if xms.is_none() {
                    mine.retain(|t| !t.starts_with("-Xms"));
                }
                if xmx.is_none() {
                    mine.retain(|t| !t.starts_with("-Xmx"));
                }
                theirs == mine
            } else {
                without_memory(&theirs) == ours
            }
        })
        .map(|p| p.name.as_str())
}

/// What to call a set of arguments: its preset's name, else its collector
/// ("ZGC", or "G1" for Java's default).
pub fn args_label(tuning: &[String], presets: &[crate::settings::JavaPreset]) -> String {
    if let Some(p) = preset_name(tuning, presets) {
        return p.to_string();
    }
    let gc = [
        ("-XX:+UseZGC", "ZGC"),
        ("-XX:+UseShenandoahGC", "Shenandoah"),
        ("-XX:+UseParallelGC", "Parallel GC"),
        ("-XX:+UseSerialGC", "Serial GC"),
    ]
    .iter()
    .find(|(f, _)| tuning.iter().any(|t| t == f))
    .map_or("G1", |(_, n)| n);
    gc.to_string()
}

/// Runs with the same arguments, summed up.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Group {
    pub fingerprint: String,
    /// The preset's name, or empty for arguments no preset has.
    pub preset: String,
    pub xms_mb: Option<u32>,
    pub xmx_mb: Option<u32>,
    pub args: Vec<String>,
    pub runs: usize,
    /// Played time over all runs, seconds.
    pub seconds: u64,
    /// Means over the runs that have them.
    pub avg_fps: Option<f64>,
    pub low1_fps: Option<f64>,
    pub frame_p99_ms: Option<f64>,
    pub p99_pause_ms: Option<f64>,
    /// The worst over all runs.
    pub worst_pause_ms: Option<f64>,
    pub peak_heap_mb: Option<f64>,
    /// Median over the runs.
    pub load_secs: Option<f64>,
    pub cpu_avg: Option<f64>,
    pub gpu_avg: Option<f64>,
    pub last_started: u64,
}

/// Sessions grouped by arguments, most recently used first. Runs shorter
/// than `min_secs` (crashed on start, closed at once) are left out.
pub fn compare(sessions: &[Session], presets: &[crate::settings::JavaPreset], min_secs: u64) -> Vec<Group> {
    let mut groups: Vec<(String, Vec<&Session>)> = Vec::new();
    for s in sessions.iter().filter(|s| s.ended > 0 && s.seconds() >= min_secs) {
        match groups.iter_mut().find(|(f, _)| *f == s.fingerprint) {
            Some((_, v)) => v.push(s),
            None => groups.push((s.fingerprint.clone(), vec![s])),
        }
    }
    let mut out: Vec<Group> = groups
        .into_iter()
        .map(|(fp, runs)| {
            let first = runs[0];
            let vals = |f: &dyn Fn(&Session) -> Option<f64>| -> Vec<f64> { runs.iter().filter_map(|s| f(s)).collect() };
            let max = |v: Vec<f64>| v.into_iter().reduce(f64::max);
            Group {
                preset: preset_name(&first.args, presets).unwrap_or_default().to_string(),
                fingerprint: fp,
                xms_mb: first.xms_mb,
                xmx_mb: first.xmx_mb,
                args: first.args.clone(),
                runs: runs.len(),
                seconds: runs.iter().map(|s| s.seconds()).sum(),
                avg_fps: stats::mean(&vals(&|s| s.frames.as_ref().map(|f| f.avg_fps))),
                low1_fps: stats::mean(&vals(&|s| s.frames.as_ref().map(|f| f.low1_fps))),
                frame_p99_ms: stats::mean(&vals(&|s| s.frames.as_ref().map(|f| f.p99_ms))),
                p99_pause_ms: stats::mean(&vals(&|s| s.gc.as_ref().filter(|g| g.pauses > 0).map(|g| g.p99_pause_ms))),
                worst_pause_ms: max(vals(&|s| s.gc.as_ref().filter(|g| g.pauses > 0).map(|g| g.max_pause_ms))),
                peak_heap_mb: max(vals(&|s| s.gc.as_ref().map(|g| g.peak_heap_mb).filter(|m| *m > 0.0))),
                load_secs: stats::median(&mut vals(&|s| s.load_secs)),
                cpu_avg: stats::mean(&vals(&|s| Some(s.cpu_avg))),
                gpu_avg: stats::mean(&vals(&|s| s.gpu_avg)),
                last_started: runs.iter().map(|s| s.started).max().unwrap_or(0),
            }
        })
        .collect();
    out.sort_by(|a, b| b.last_started.cmp(&a.last_started));
    out
}

/// How long loading took, from one line of `latest.log`: ModernFix says
/// it outright ("Game took 96.296 seconds to start"); otherwise the sound
/// engine starting, near the end of loading, marks it at `elapsed`.
pub fn load_mark(line: &str, elapsed: f64) -> Option<f64> {
    if let Some(i) = line.find("Game took ") {
        let rest = &line[i + 10..];
        if let Some(n) = rest.split(' ').next().and_then(|n| n.parse::<f64>().ok())
            && rest.contains("seconds to start")
        {
            return Some(n);
        }
    }
    line.contains("Sound engine started").then_some(elapsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::JavaPreset;

    fn toks(s: &str) -> Vec<String> {
        crate::jvm_args::split(s)
    }

    #[test]
    fn tuning_and_fingerprints() {
        let a = tuning_args(&toks("-Xmx8192m -Xms4G -XX:+UseZGC -Dfml.x=1 -Xlog:gc*:file=x -Djdk.graal.A=1 -Xss4M"));
        assert_eq!(a, ["-Djdk.graal.A=1", "-XX:+UseZGC", "-Xms4G", "-Xmx8G", "-Xss4M"]);
        let b = tuning_args(&toks("-XX:+UseZGC -Xss4M -Xmx8G -Xms4096M -Djdk.graal.A=1"));
        assert_eq!(fingerprint(&a), fingerprint(&b));
        assert_ne!(fingerprint(&a), fingerprint(&tuning_args(&toks("-Xmx8G"))));
        assert_eq!(fingerprint(&a).len(), 8);
    }

    #[test]
    fn jvm_args_from_a_command_line() {
        let cmd = toks(r#"C:\java\javaw.exe -Xmx8G -XX:+UseG1GC -cp a.jar;b.jar -Dx=1 net.minecraft.client.main.Main --gameDir G:\x"#);
        assert_eq!(jvm_part(&cmd), ["-Xmx8G", "-XX:+UseG1GC", "-Dx=1"]);
    }

    #[test]
    fn presets_match_with_or_without_memory() {
        let presets = vec![
            JavaPreset { name: "Fixed".into(), args: "-Xms6G -Xmx6G -XX:+UseZGC".into() },
            JavaPreset { name: "ZGC".into(), args: "-XX:+UseZGC".into() },
            JavaPreset { name: "Aikar".into(), args: "-XX:+UseG1GC -XX:+ParallelRefProcEnabled".into() },
        ];
        let t = |s: &str| tuning_args(&toks(s));
        assert_eq!(preset_name(&t("-Xms6144m -Xmx6G -XX:+UseZGC"), &presets), Some("Fixed"));
        assert_eq!(preset_name(&t("-Xms2G -Xmx8G -XX:+UseZGC"), &presets), Some("ZGC"));
        assert_eq!(preset_name(&t("-Xmx8G -XX:+ParallelRefProcEnabled -XX:+UseG1GC"), &presets), Some("Aikar"));
        assert_eq!(preset_name(&t("-Xmx8G -XX:+UseG1GC"), &presets), None);
    }

    fn run(fp: &str, started: u64, secs: u64, fps: f64, pause: f64, load: f64) -> Session {
        Session {
            fingerprint: fp.into(),
            args: vec!["-XX:+UseZGC".into()],
            started,
            ended: started + secs,
            load_secs: Some(load),
            frames: Some(FrameStats { avg_fps: fps, low1_fps: fps / 2.0, p99_ms: 1000.0 / fps, ..Default::default() }),
            gc: Some(GcStats { pauses: 3, max_pause_ms: pause, p99_pause_ms: pause / 2.0, peak_heap_mb: pause * 10.0, ..Default::default() }),
            cpu_avg: 20.0,
            ..Default::default()
        }
    }

    #[test]
    fn labels() {
        let presets = vec![JavaPreset { name: "Mine".into(), args: "-XX:+UseZGC -XX:+AlwaysPreTouch".into() }];
        let t = |s: &str| tuning_args(&toks(s));
        assert_eq!(args_label(&t("-Xmx8G -XX:+AlwaysPreTouch -XX:+UseZGC"), &presets), "Mine");
        assert_eq!(args_label(&t("-Xmx8G -XX:+UseZGC"), &presets), "ZGC");
        assert_eq!(args_label(&t("-Xmx8G"), &presets), "G1");
    }

    #[test]
    fn groups_runs_by_arguments() {
        let sessions = vec![
            run("aaaa", 100, 600, 60.0, 10.0, 90.0),
            run("bbbb", 200, 600, 100.0, 2.0, 80.0),
            run("aaaa", 300, 600, 80.0, 30.0, 100.0),
            run("aaaa", 400, 600, 80.0, 30.0, 110.0),
            // Too short to count.
            run("bbbb", 500, 5, 1.0, 900.0, 1.0),
        ];
        let presets = vec![JavaPreset { name: "ZGC".into(), args: "-XX:+UseZGC".into() }];
        let g = compare(&sessions, &presets, 60);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].fingerprint, "aaaa");
        assert_eq!(g[0].preset, "ZGC");
        assert_eq!(g[0].runs, 3);
        assert_eq!(g[0].seconds, 1800);
        assert!((g[0].avg_fps.unwrap() - 220.0 / 3.0).abs() < 1e-9);
        assert_eq!(g[0].worst_pause_ms, Some(30.0));
        assert_eq!(g[0].peak_heap_mb, Some(300.0));
        assert_eq!(g[0].load_secs, Some(100.0));
        assert_eq!(g[1].runs, 1);
        assert_eq!(g[1].worst_pause_ms, Some(2.0));
    }

    #[test]
    fn load_marks() {
        let mf = "[15Oct2026 15:20:15.924] [Render thread/WARN] [ModernFix/]: Game took 476.133 seconds to start";
        assert_eq!(load_mark(mf, 10.0), Some(476.133));
        let snd = "[15:19:19] [Render thread/INFO]: Sound engine started";
        assert_eq!(load_mark(snd, 42.0), Some(42.0));
        assert_eq!(load_mark("[main/INFO]: Loading", 3.0), None);
    }

    #[test]
    fn gc_log_only_for_new_java() {
        assert!(wants_gc_log(21, "-XX:+UseZGC"));
        assert!(!wants_gc_log(8, ""));
        assert!(!wants_gc_log(17, "-Xlog:gc:file=x.log"));
        assert!(gc_log_arg().starts_with("-Xlog:gc*:file=logs/"));
    }

    #[test]
    fn saves_and_loads() {
        let base = std::env::temp_dir().join(format!("hopper-sessions-{}", std::process::id()));
        let root = PathBuf::from(r"G:\Instances\My Pack");
        let mut s = run("aaaa", 100, 600, 60.0, 10.0, 90.0);
        s.id = "20261015-101010".into();
        s.instance_root = root.to_string_lossy().into_owned();
        save(&base, &s).unwrap();
        let mut s2 = s.clone();
        s2.id = "20261015-111111".into();
        s2.started = 900;
        save(&base, &s2).unwrap();
        let all = load_all(&base, &root);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, "20261015-111111");
        assert_eq!(all[1], s);
        delete(&base, &root, &s.id).unwrap();
        assert_eq!(load_all(&base, &root).len(), 1);
        assert!(delete(&base, &root, "..").is_err());
        let _ = std::fs::remove_dir_all(&base);
    }
}
