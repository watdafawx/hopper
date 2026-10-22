//! Which mods a game crash involves, read from what Minecraft, the mod
//! loader and Java write themselves: the crash report, `logs/latest.log`
//! and Java's own crash file (`hs_err_pid*.log`). See
//! `design/crash-detection.md`.
//!
//! The signals:
//! - NeoForge's "Mod loading issue for: <id>" blocks (with the mod's file
//!   and the loader's own failure message): certain;
//! - the exception's stack frames, which NeoForge and Forge print with the
//!   mod and its jar (`at TRANSFORMER/<id>@<ver>/… ~[<jar>%23…]`): the first
//!   mod frame is likely, the mods that led to it possible;
//! - Mixin's "Mixin apply for mod <id> failed" lines: likely;
//! - `OutOfMemoryError` and a Java crash file: a note, no mod.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// Kept beside the instance until the next game that ends normally.
pub const FILE: &str = ".hopper-crash.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Certainty {
    Certain,
    Likely,
    Possible,
}

impl Certainty {
    pub fn label(self) -> &'static str {
        match self {
            Certainty::Certain => "Certain",
            Certainty::Likely => "Likely",
            Certainty::Possible => "Possible",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CrashKind {
    /// Mods are named.
    Mods,
    /// Java ran out of memory.
    Memory,
    /// Java itself crashed (a driver or native library).
    Native,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Suspect {
    /// The jar in the instance's mods folder (as named on disk).
    pub file_name: String,
    /// Its mod id, when the crash says it.
    pub mod_id: String,
    pub certainty: Certainty,
    /// One plain sentence.
    pub why: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Crash {
    /// When the game ended (seconds since the epoch).
    pub when: i64,
    /// The file the findings came from.
    pub source: PathBuf,
    /// The report's description and its error line.
    pub headline: String,
    pub kind: CrashKind,
    /// Most certain first.
    pub suspects: Vec<Suspect>,
}

fn re(p: &str) -> Regex {
    Regex::new(p).expect("static regex")
}

/// `-- Mod loading issue for: somemod --`
static LOADING_ISSUE: LazyLock<Regex> = LazyLock::new(|| re(r"^-- Mod loading issue for: ([\w.-]+) --"));
/// `Mod file: /G:/…/mods/somemod-1.0.jar`
static MOD_FILE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^\s*Mod file:\s*.*?([^\\/]+\.jar)\s*$"));
/// `at TRANSFORMER/somemod@1.0/com.x.Y.z(Y.java:1) ~[somemod-1.0.jar%23123!/:1.0]`
static FRAME: LazyLock<Regex> =
    LazyLock::new(|| re(r"^\s*at\s+(?:[\w-]+/)?(?:([\w.-]+)@[^/\s]+/)?\S+\(.*?\)\s*~?\[([^\]%!]+?\.jar)"));
/// Mixin's own message.
static MIXIN_FAILED: LazyLock<Regex> = LazyLock::new(|| re(r"Mixin apply for mod ([\w.-]+) failed"));

/// Mod ids that are the game or the loader, not a mod someone installed.
const NOT_MODS: [&str; 6] = ["minecraft", "neoforge", "forge", "fml", "fabricloader", "java"];

/// One finding in a crash text, before it's matched to the instance.
#[derive(Clone, Debug, PartialEq)]
pub struct Raw {
    pub mod_id: String,
    pub jar: String,
    pub certainty: Certainty,
    pub why: String,
}

/// The findings in a crash report or log, most certain first, one per mod.
pub fn signals(text: &str) -> Vec<Raw> {
    let mut out: Vec<Raw> = Vec::new();
    let lines: Vec<&str> = text.lines().collect();

    // NeoForge's loading issues: the mod, its file and the loader's reason.
    for (i, l) in lines.iter().enumerate() {
        let Some(m) = LOADING_ISSUE.captures(l.trim_start()) else { continue };
        let id = m[1].to_string();
        let mut jar = String::new();
        let mut why = String::new();
        for (k, l) in lines.iter().enumerate().skip(i + 1).take(30) {
            let t = l.trim();
            if t.starts_with("-- ") {
                break;
            }
            if let Some(f) = MOD_FILE.captures(l) {
                jar = f[1].to_string();
            } else if let Some(msg) = t.strip_prefix("Failure message:") {
                why = msg.trim().to_string();
                // The message goes on in deeper-indented lines.
                for more in lines.iter().skip(k + 1).take(4) {
                    let m = more.trim();
                    if m.is_empty() || m.contains(':') && !more.starts_with("\t\t") {
                        break;
                    }
                    why.push(' ');
                    why.push_str(m);
                }
            }
        }
        add(
            &mut out,
            Raw {
                mod_id: id,
                jar,
                certainty: Certainty::Certain,
                why: if why.is_empty() {
                    "The loader couldn't load it.".into()
                } else {
                    format!("The loader couldn't load it: {why}")
                },
            },
        );
    }

    // The exception's own trace: the first stack, up to the first blank
    // line or the report's details.
    if let Some(start) = lines.iter().position(|l| FRAME.is_match(l)) {
        let mut first = true;
        let mut first_name = String::new();
        for l in lines[start..].iter().take_while(|l| !l.trim().is_empty() && !l.starts_with("A detailed")) {
            let Some(m) = FRAME.captures(l) else { continue };
            let id = m.get(1).map_or("", |g| g.as_str()).to_string();
            let jar = m[2].to_string();
            if NOT_MODS.contains(&id.as_str()) || is_loader_jar(&jar) {
                continue;
            }
            if out.iter().any(|r| same(r, &id, &jar)) {
                continue;
            }
            let name = if id.is_empty() { jar.clone() } else { id.clone() };
            let (certainty, why) = if first {
                first_name = name;
                (Certainty::Likely, "The error happened in its code.".to_string())
            } else {
                (Certainty::Possible, format!("Its code called {first_name}, where the error happened."))
            };
            first = false;
            add(&mut out, Raw { mod_id: id, jar, certainty, why });
        }
    }

    // Mixin's failures.
    for l in &lines {
        if let Some(m) = MIXIN_FAILED.captures(l) {
            add(
                &mut out,
                Raw {
                    mod_id: m[1].to_string(),
                    jar: String::new(),
                    certainty: Certainty::Likely,
                    why: "Its changes to the game's code (a mixin) failed to apply.".into(),
                },
            );
        }
    }
    out.sort_by_key(|r| r.certainty);
    out
}

fn same(r: &Raw, id: &str, jar: &str) -> bool {
    (!id.is_empty() && r.mod_id.eq_ignore_ascii_case(id)) || (!jar.is_empty() && r.jar.eq_ignore_ascii_case(jar))
}

/// Adds `r`, or fills in what an earlier finding for the same mod lacked
/// (keeping the more certain one).
fn add(out: &mut Vec<Raw>, r: Raw) {
    match out.iter_mut().find(|e| same(e, &r.mod_id, &r.jar)) {
        Some(e) => {
            if e.jar.is_empty() {
                e.jar = r.jar;
            }
            if e.mod_id.is_empty() {
                e.mod_id = r.mod_id;
            }
            if r.certainty < e.certainty {
                e.certainty = r.certainty;
                e.why = r.why;
            }
        }
        None => out.push(r),
    }
}

/// The game's and the loader's own jars.
fn is_loader_jar(jar: &str) -> bool {
    let j = jar.to_ascii_lowercase();
    ["client-", "server-", "neoforge-", "forge-", "fmlcore", "fmlloader", "javafmllanguage", "lowcodelanguage",
        "mclanguage", "bus-", "eventbus", "securejarhandler", "bootstraplauncher", "modlauncher", "mixin-"]
        .iter()
        .any(|p| j.starts_with(p))
}

/// The report's "Description:" and the error line under it.
pub fn headline(text: &str) -> String {
    let mut lines = text.lines().map(str::trim);
    let Some(desc) = lines.find_map(|l| l.strip_prefix("Description:")) else {
        return String::new();
    };
    let err = lines.find(|l| !l.is_empty()).unwrap_or_default();
    let err = err.strip_prefix("java.lang.").unwrap_or(err);
    if err.is_empty() { desc.trim().to_string() } else { format!("{} · {}", desc.trim(), cut(err, 200)) }
}

fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

fn modified_secs(p: &Path) -> Option<i64> {
    let t = fs::metadata(p).ok()?.modified().ok()?;
    Some(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64)
}

/// The tail of a file (up to `max` bytes), as text.
fn tail(p: &Path, max: u64) -> String {
    crate::logs::tail(p, max)
}

/// Whether the game in `root` that started at `started` crashed, and what
/// its crash names. `mods` are the jars in the mods folder (file names) and
/// `ids` gives each jar's own mod ids, to match what the crash says.
pub fn detect(root: &Path, started: i64, mods: &[String], ids: impl Fn(&str) -> Vec<String>) -> Option<Crash> {
    let report = crate::logs::crash_reports(root)
        .into_iter()
        .find(|p| modified_secs(p).is_some_and(|t| t >= started));
    let native = fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("hs_err_pid")))
        .find(|p| modified_secs(p).is_some_and(|t| t >= started));
    if report.is_none() && native.is_none() {
        return None;
    }
    let now = crate::time::now();
    let text = report.as_deref().map(|p| tail(p, 2 << 20)).unwrap_or_default();
    let log = tail(&root.join("logs").join("latest.log"), 2 << 20);
    let mut raws = signals(&text);
    for r in signals(&log).into_iter().filter(|r| r.certainty != Certainty::Possible) {
        add(&mut raws, r);
    }
    raws.sort_by_key(|r| r.certainty);
    let suspects: Vec<Suspect> = raws
        .into_iter()
        .filter_map(|r| {
            let file = match_jar(&r, mods, &ids)?;
            Some(Suspect { file_name: file, mod_id: r.mod_id, certainty: r.certainty, why: r.why })
        })
        .collect();
    let out_of_memory = text.contains("OutOfMemoryError") || log.contains("java.lang.OutOfMemoryError");
    let kind = if !suspects.is_empty() {
        CrashKind::Mods
    } else if out_of_memory {
        CrashKind::Memory
    } else if native.is_some() && report.is_none() {
        CrashKind::Native
    } else {
        CrashKind::Unknown
    };
    let source = report.clone().or(native.clone()).unwrap_or_default();
    let headline = match (&report, &native) {
        (Some(_), _) => headline(&text),
        (None, Some(n)) => native_line(&tail(n, 64 << 10)),
        _ => String::new(),
    };
    Some(Crash { when: now, source, headline, kind, suspects })
}

/// A Java crash file's "Problematic frame" line.
fn native_line(text: &str) -> String {
    let mut lines = text.lines();
    lines
        .find(|l| l.contains("Problematic frame"))
        .and_then(|_| lines.next())
        .map(|l| format!("Java crashed in {}", l.trim_start_matches('#').trim()))
        .unwrap_or_else(|| "Java itself crashed".into())
}

/// The jar in the mods folder a finding means: by file name, else by the
/// mod ids the jars say they are.
fn match_jar(r: &Raw, mods: &[String], ids: &impl Fn(&str) -> Vec<String>) -> Option<String> {
    if !r.jar.is_empty() {
        let want = r.jar.to_ascii_lowercase();
        if let Some(m) = mods.iter().find(|m| {
            let m = m.to_ascii_lowercase();
            m == want || m.strip_suffix(".disabled") == Some(want.as_str())
        }) {
            return Some(m.clone());
        }
    }
    if r.mod_id.is_empty() || NOT_MODS.contains(&r.mod_id.as_str()) {
        return None;
    }
    mods.iter()
        .find(|m| ids(m).iter().any(|i| i.eq_ignore_ascii_case(&r.mod_id)))
        .cloned()
}

pub fn load(root: &Path) -> Option<Crash> {
    serde_json::from_slice(&fs::read(root.join(FILE)).ok()?).ok()
}

pub fn save(root: &Path, c: &Crash) {
    if let Ok(b) = serde_json::to_vec_pretty(c) {
        let _ = fs::write(root.join(FILE), b);
    }
}

pub fn clear(root: &Path) {
    let _ = fs::remove_file(root.join(FILE));
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOADING: &str = "---- Minecraft Crash Report ----
Description: Mod loading failures have occurred; consult the issue messages for more details

net.neoforged.neoforge.logging.CrashReportExtender$ModLoadingCrashException: Mod loading has failed

-- Mod loading issue for: mobflowutilities --
Details:
\tMod file: /G:/Games/Instances/Pack/mods/mobflowutilities-3.1.10.10-mc-1.21.1.jar
\tFailure message: Mod mobflowutilities requires neoforge 21.1.259 or above
\t\tCurrently, neoforge is 21.1.256
\t\t
\tMod version: 3.1.10.10-mc-1.21.1

-- System Details --
";

    const TRACE: &str = "Description: Unexpected error

java.lang.IllegalStateException: There is no recipe category registered
\tat TRANSFORMER/jei@19.64.0.454/mezz.jei.A.get(A.java:39) ~[jei-1.21.1-neoforge-19.64.0.454.jar%231834!/:19.64.0.454] {re:classloading}
\tat TRANSFORMER/jei@19.64.0.454/mezz.jei.B.hide(B.java:360) ~[jei-1.21.1-neoforge-19.64.0.454.jar%231834!/:19.64.0.454] {re:classloading}
\tat TRANSFORMER/irons_nouveau@0.17.22/dev.x.Plugin.tick(Plugin.java:37) ~[irons-nouveau-0.17.22-mc1.21.1-neoforge.jar%231817!/:?] {re:classloading}
\tat MC-BOOTSTRAP/net.neoforged.bus/net.neoforged.bus.EventBus.post(EventBus.java:360) ~[bus-8.0.5.jar%23110!/:?] {}
\tat TRANSFORMER/neoforge@21.1.256/net.neoforged.neoforge.client.ClientHooks.tick(ClientHooks.java:1077) ~[neoforge-21.1.256-universal.jar%231276!/:?] {re:mixin}
\tat TRANSFORMER/minecraft@1.21.1/net.minecraft.client.Minecraft.tick(Minecraft.java:1915) ~[client-1.21.1-srg.jar%231275!/:?] {re:mixin}
\tat java.base/java.lang.Thread.run(Thread.java:1583) [?:?] {}


A detailed walkthrough of the error
-- Head --
\tat TRANSFORMER/other@1/x.Y.z(Y.java:1) ~[other-1.jar%231!/:?] {}
";

    #[test]
    fn loader_issues_are_certain_with_the_reason() {
        let s = signals(LOADING);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].mod_id, "mobflowutilities");
        assert_eq!(s[0].jar, "mobflowutilities-3.1.10.10-mc-1.21.1.jar");
        assert_eq!(s[0].certainty, Certainty::Certain);
        assert!(s[0].why.contains("requires neoforge 21.1.259 or above Currently, neoforge is 21.1.256"), "{}", s[0].why);
        assert!(headline(LOADING).starts_with("Mod loading failures have occurred"));
    }

    #[test]
    fn the_first_mod_in_the_trace_is_likely_the_rest_possible() {
        let s = signals(TRACE);
        let ids: Vec<(&str, Certainty)> = s.iter().map(|r| (r.mod_id.as_str(), r.certainty)).collect();
        assert_eq!(ids, [("jei", Certainty::Likely), ("irons_nouveau", Certainty::Possible)], "loader, game and later stacks skipped");
        assert!(s[1].why.contains("jei"));
        assert_eq!(headline(TRACE), "Unexpected error · IllegalStateException: There is no recipe category registered");
    }

    #[test]
    fn mixin_failures_and_matching_to_jars() {
        let log = "[12:00:00] [main/ERROR] [mixin/]: Mixin apply for mod coolmod failed coolmod.mixins.json:AMixin from mod coolmod -> net.minecraft.X";
        let s = signals(log);
        assert_eq!((s[0].mod_id.as_str(), s[0].certainty), ("coolmod", Certainty::Likely));
        let mods = vec!["CoolMod-2.0.jar".to_string(), "jei-1.21.1-neoforge-19.64.0.454.jar.disabled".to_string()];
        let ids = |f: &str| if f.starts_with("CoolMod") { vec!["coolmod".to_string()] } else { Vec::new() };
        assert_eq!(match_jar(&s[0], &mods, &ids).as_deref(), Some("CoolMod-2.0.jar"));
        let jei = &signals(TRACE)[0];
        assert_eq!(match_jar(jei, &mods, &ids).as_deref(), Some("jei-1.21.1-neoforge-19.64.0.454.jar.disabled"));
    }

    #[test]
    fn a_game_only_trace_names_no_mod() {
        let t = "Description: Rendering screen\n\njava.lang.NullPointerException\n\tat TRANSFORMER/minecraft@1.21.1/net.minecraft.client.X.y(X.java:1) ~[client-1.21.1-srg.jar%231!/:?] {}\n";
        assert!(signals(t).is_empty());
    }

    #[test]
    fn detects_a_crash_written_after_the_start() {
        let root = std::env::temp_dir().join(format!("hopper-crashdetect-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("crash-reports")).unwrap();
        fs::create_dir_all(root.join("logs")).unwrap();
        fs::write(root.join("logs/latest.log"), "done\n").unwrap();
        let mods = vec!["mobflowutilities-3.1.10.10-mc-1.21.1.jar".to_string()];
        let started = crate::time::now() - 5;
        assert!(detect(&root, started, &mods, |_| Vec::new()).is_none(), "no report: no crash");
        fs::write(root.join("crash-reports/crash-2026-10-22_12.00.00-fml.txt"), LOADING).unwrap();
        let c = detect(&root, started, &mods, |_| Vec::new()).unwrap();
        assert_eq!(c.kind, CrashKind::Mods);
        assert_eq!(c.suspects[0].file_name, mods[0]);
        assert!(detect(&root, crate::time::now() + 60, &mods, |_| Vec::new()).is_none(), "older report: not this game's");
        save(&root, &c);
        assert_eq!(load(&root), Some(c));
        clear(&root);
        assert!(load(&root).is_none());
        let _ = fs::remove_dir_all(&root);
    }

    /// Prints what the real crash reports of every instance name.
    /// `cargo test real_crash_reports -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_crash_reports() {
        for inst in crate::instances::discover(Path::new(r"G:\Games\Minecraft\Curse\Instances")) {
            for p in crate::logs::crash_reports(&inst.root).into_iter().take(4) {
                let text = fs::read_to_string(&p).unwrap_or_default();
                println!("{} / {}", inst.name, p.file_name().unwrap().to_string_lossy());
                println!("   {}", headline(&text));
                for r in signals(&text) {
                    println!("   {:?} {} [{}] {}", r.certainty, r.mod_id, r.jar, r.why);
                }
            }
        }
    }
}
