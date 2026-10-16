//! Runs a loader's installer jar (NeoForge, Forge, Quilt) with no console
//! window, feeding what it prints to a job as progress.

use crate::jobs::Job;
use anyhow::Context as _;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

/// How an installer run ended.
pub struct Run {
    pub status: ExitStatus,
    /// The last line it printed, for error messages.
    pub last_line: String,
}

/// Runs `java -jar installer args…` in `cwd`. `label` heads the job detail
/// ("Installing NeoForge 21.1.262").
pub fn run(java: &Path, installer: &Path, args: &[&std::ffi::OsStr], cwd: &Path, label: &str, job: &Job) -> anyhow::Result<Run> {
    let total = steps(installer);
    let mut cmd = Command::new(java);
    cmd.arg("-jar")
        .arg(installer)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().with_context(|| format!("running {}", java.display()))?;
    let mut stderr = child.stderr.take().expect("piped");
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });

    job.detail(format!("{label}: starting the installer"));
    job.progress(0, 0);
    let mut done = 0usize;
    let mut last_line = String::new();
    // The installer ends a failure with a generic line; the cause is earlier.
    let mut problem: Option<String> = None;
    let stdout = child.stdout.take().expect("piped");
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        last_line = t.to_string();
        if is_problem(t) {
            problem = Some(t.to_string());
        }
        if let Some(phase) = phase(t) {
            if phase.counts {
                done += 1;
            }
            match total {
                Some(n) if n > 0 => {
                    job.detail(format!("{label}: {}", phase.text));
                    job.progress(done.min(n - 1), n);
                }
                _ => job.detail(format!("{label}: {}", phase.text)),
            }
        }
        if job.is_cancelled() {
            let _ = child.kill();
            break;
        }
    }
    let status = child.wait()?;
    let err = err_thread.join().unwrap_or_default();
    if let Some(l) = err.lines().rev().find(|l| !l.trim().is_empty()) {
        if !status.success() || last_line.is_empty() {
            last_line = l.trim().to_string();
        }
    }
    if last_line.is_empty() {
        last_line = "no output".into();
    }
    if !status.success() || last_line.starts_with("There was an error") {
        if let Some(p) = problem.filter(|p| *p != last_line) {
            last_line = format!("{last_line} ({p})");
        }
    }
    job.check_cancelled()?;
    Ok(Run { status, last_line })
}

struct Phase {
    text: &'static str,
    /// One of the steps [`steps`] counted.
    counts: bool,
}

/// What an installer line says it's doing. Forge and NeoForge print
/// "Considering library …" per library and "MainClass: …" per processor.
fn phase(line: &str) -> Option<Phase> {
    let p = |text, counts| Some(Phase { text, counts });
    if line.starts_with("Considering library") {
        p("downloading libraries", true)
    } else if line.starts_with("MainClass:") {
        p("running processors", true)
    } else if line.starts_with("Extracting") {
        p("extracting", false)
    } else if line.starts_with("Considering minecraft") || line.contains("minecraft server") {
        p("getting the Minecraft server", false)
    } else if line.starts_with("Downloading") {
        p("downloading", false)
    } else {
        None
    }
}

/// A line that names what went wrong, not the installer's closing message
/// or the class-scan noise ("Can't Find Class: …") it prints on success.
fn is_problem(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    (l.contains("fail") || l.contains("exception") || l.contains("error"))
        && !l.starts_with("there was an error")
        && !l.starts_with("can't find class")
        && !l.starts_with("considering library")
        && !l.starts_with("downloading library")
        && !l.starts_with("file ")
}

/// How many counted steps a Forge-style installer will print: its libraries
/// and server processors. None for other installers.
fn steps(installer: &Path) -> Option<usize> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(installer).ok()?).ok()?;
    let json = |z: &mut zip::ZipArchive<std::fs::File>, name: &str| -> Option<serde_json::Value> {
        let mut s = String::new();
        z.by_name(name).ok()?.read_to_string(&mut s).ok()?;
        serde_json::from_str(&s).ok()
    };
    let profile = json(&mut z, "install_profile.json")?;
    let count = |v: &serde_json::Value| v["libraries"].as_array().map_or(0, Vec::len);
    let libs = count(&profile) + json(&mut z, "version.json").as_ref().map_or(0, count);
    let procs = profile["processors"].as_array().map_or(0, |a| {
        a.iter()
            .filter(|p| {
                p["sides"]
                    .as_array()
                    .is_none_or(|s| s.iter().any(|s| s.as_str() == Some("server")))
            })
            .count()
    });
    Some(libs + procs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_forge_style_lines() {
        assert!(phase("Considering library net.neoforged:neoforge:21.1.262").unwrap().counts);
        assert!(phase("MainClass: net.neoforged.installertools.ConsoleTool").unwrap().counts);
        assert!(!phase("Extracting main jar:").unwrap().counts);
        assert!(phase("  some path").is_none());
        assert!(is_problem("Failed to download library from https://x"));
        assert!(!is_problem("There was an error during installation"));
        assert!(!is_problem("Considering library com.google.errorprone:error_prone_annotations:2.18.0"));
        assert!(!is_problem("Can't Find Class: com/mojang/brigadier/exceptions/CommandSyntaxException"));
    }
}
