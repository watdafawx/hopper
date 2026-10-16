//! Which instances have Minecraft running, found by the `--gameDir` the
//! launcher passes to Java, or by the game's working folder when the
//! command line was rewritten (Sodium does that on Windows).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Running {
    pub pid: u32,
    /// Seconds since the epoch.
    pub started: u64,
}

fn norm(p: &str) -> String {
    p.trim_matches(|c: char| c == '"' || c.is_whitespace())
        .trim_end_matches(['\\', '/'])
        .replace('/', "\\")
        .to_lowercase()
}

/// `--gameDir <dir>` from a Java command line.
pub fn game_dir(args: &[String]) -> Option<String> {
    let i = args.iter().position(|a| a == "--gameDir")?;
    args.get(i + 1).map(|d| norm(d))
}

/// The instance folder of a Java process: its `--gameDir`, or, for a game
/// whose command line no longer has one (Sodium replaces it with
/// `net.caffeinemc.sodium / net.minecraft.client.main.Main /`), the folder
/// it runs in, which launchers set to the game folder.
pub fn instance_dir(args: &[String], cwd: Option<&std::path::Path>) -> Option<String> {
    if let Some(d) = game_dir(args) {
        return Some(d);
    }
    let is_game = args.iter().any(|a| a == "net.minecraft.client.main.Main");
    is_game.then(|| cwd.map(|c| norm(&c.to_string_lossy()))).flatten()
}

fn scan(sys: &mut System, roots: &[PathBuf]) -> HashMap<PathBuf, Running> {
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::OnlyIfNotSet)
            .with_cwd(UpdateKind::OnlyIfNotSet),
    );
    let wanted: Vec<(String, &PathBuf)> = roots
        .iter()
        .map(|r| (norm(&r.to_string_lossy()), r))
        .collect();
    let mut out = HashMap::new();
    for (pid, p) in sys.processes() {
        let name = p.name().to_string_lossy().to_lowercase();
        if !name.starts_with("java") {
            continue;
        }
        let args: Vec<String> = p
            .cmd()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let Some(dir) = instance_dir(&args, p.cwd()) else { continue };
        if let Some((_, root)) = wanted.iter().find(|(w, _)| *w == dir) {
            out.insert(
                (*root).clone(),
                Running {
                    pid: pid.as_u32(),
                    started: p.start_time(),
                },
            );
        }
    }
    out
}

/// Watches for running instances every few seconds; `send` gets the map
/// whenever it changes. `roots` is read fresh each round.
pub fn watch(
    roots: impl Fn() -> Vec<PathBuf> + Send + 'static,
    send: impl Fn(HashMap<PathBuf, Running>) + Send + 'static,
) {
    std::thread::Builder::new()
        .name("game-watch".into())
        .spawn(move || {
            let mut sys = System::new();
            let mut last = HashMap::new();
            loop {
                let now = scan(&mut sys, &roots());
                if now != last {
                    send(now.clone());
                    last = now;
                }
                std::thread::sleep(Duration::from_secs(3));
            }
        })
        .ok();
}

/// Ends a running game.
pub fn stop(pid: u32) -> anyhow::Result<()> {
    let mut sys = System::new();
    let p = Pid::from_u32(pid);
    sys.refresh_processes(ProcessesToUpdate::Some(&[p]), true);
    let proc = sys
        .process(p)
        .ok_or_else(|| anyhow::anyhow!("the game is no longer running"))?;
    anyhow::ensure!(proc.kill(), "Windows refused to end the game process");
    Ok(())
}

/// `true` when `root` matches the game directory `dir`.
#[cfg(test)]
pub fn same_dir(root: &std::path::Path, dir: &str) -> bool {
    norm(&root.to_string_lossy()) == norm(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_game_dir() {
        let args: Vec<String> = [
            "javaw.exe",
            "-Xmx8G",
            "net.minecraft.client.main.Main",
            "--gameDir",
            r"G:\Games\Curse\Instances\TNP\",
            "--username",
            "x",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let dir = game_dir(&args).unwrap();
        assert!(same_dir(
            std::path::Path::new(r"g:\games\curse\instances\tnp"),
            &dir
        ));
    }

    #[test]
    fn sodium_rewritten_command_line_uses_cwd() {
        let args: Vec<String> = ["net.caffeinemc.sodium", "/", "net.minecraft.client.main.Main", "/"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let cwd = std::path::Path::new(r"G:\Games\Curse\Instances\TNP\");
        let dir = instance_dir(&args, Some(cwd)).unwrap();
        assert!(same_dir(std::path::Path::new(r"g:\games\curse\instances\tnp"), &dir));
        // Other Java in the instance folder (a crash helper) doesn't count.
        let helper: Vec<String> = ["javaw.exe", "-cp", "app.jar", "Main"].iter().map(|s| s.to_string()).collect();
        assert!(instance_dir(&helper, Some(cwd)).is_none());
        // --gameDir wins over the working folder.
        let mut both = args.clone();
        both.extend(["--gameDir".into(), r"D:\Other".into()]);
        assert_eq!(instance_dir(&both, Some(cwd)).unwrap(), r"d:\other");
    }
}

/// Prints what the scanner sees. `cargo test real_scan -- --ignored --nocapture`
#[cfg(test)]
#[test]
#[ignore]
fn real_scan() {
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::OnlyIfNotSet)
            .with_cwd(UpdateKind::OnlyIfNotSet),
    );
    for p in sys.processes().values().filter(|p| {
        p.name()
            .to_string_lossy()
            .to_lowercase()
            .starts_with("java")
    }) {
        println!("{:?} {:?} cwd={:?}", p.name(), p.cmd(), p.cwd());
    }
    let roots = vec![PathBuf::from(
        r"G:\Games\Minecraft\Curse\Instances\TNP Limitless 8",
    )];
    println!("scan: {:?}", scan(&mut sys, &roots));
}
