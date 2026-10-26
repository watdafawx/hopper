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
//! - Fabric and Quilt: the loader's "Mod 'Name' (id) … requires …" lines
//!   (certain); stack frames carry no jar there, so a frame's class is
//!   matched to the jar that has its package (see [`PackageIndex`]), and a
//!   mixin's injected method names its mod (`handler$…$modid$…`);
//! - missing dependencies the loader names, to install;
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
    /// What it needs that isn't installed: (mod id, versions).
    #[serde(default)]
    pub needs: Vec<(String, String)>,
    /// Another mod its error names that changed lately, with its older copy.
    #[serde(default)]
    pub changed: Option<Changed>,
}

/// A mod that changed before the crash, which a suspect's error names:
/// "Forbidden & Arcanus was updated Oct 24 (2.6.1 → 2.6.2)".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Changed {
    /// The jar in the mods folder now.
    pub file_name: String,
    /// Its display name (or mod id).
    pub name: String,
    /// The older copy's version and the installed one's (either may be empty).
    pub from: String,
    pub to: String,
    /// When the older copy was set aside (seconds since the epoch).
    pub when: i64,
    /// The older copy, in `.hopper-backups`.
    pub backup: PathBuf,
}

/// A mod that failed while the game was loading, when the crash came
/// after (a knock-on effect): the story the crash card tells.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Stopped {
    /// Its jar in the mods folder.
    pub file_name: String,
    pub mod_id: String,
    /// Its own error message (empty when the log has none).
    pub error: String,
    /// Mods the loader skipped because they need it (jars), from
    /// `logs/debug.log`.
    pub skipped: Vec<String>,
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
    /// Suspects the player turned off from the crash card, to turn back on.
    #[serde(default)]
    pub turned_off: Vec<String>,
    /// The mod that stopped loading, when the crash followed from that.
    #[serde(default)]
    pub stopped: Option<Stopped>,
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
    /// A frame's class, when the frame names no jar (Fabric): matched to a
    /// jar by its package.
    pub class: String,
    /// (missing mod id, versions).
    pub needs: Vec<(String, String)>,
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
                    // Each line is its own sentence.
                    if !why.ends_with(['.', '!', ':']) {
                        why.push('.');
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
                needs: needs_in(&why),
                why: if why.is_empty() {
                    "The loader couldn't load it.".into()
                } else {
                    format!("The loader couldn't load it: {why}")
                },
                class: String::new(),
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
            // A mod's change to the game's own code (a mixin) runs in the
            // game's frames: `Minecraft.handler$jip000$omegaconfig$onReturn`.
            if let Some(mx) = MIXIN_METHOD.captures(l) {
                let mid = mx[1].to_string();
                if !out.iter().any(|r| r.mod_id == mid) {
                    let (certainty, why) = if first {
                        first_name = mid.clone();
                        (Certainty::Likely, "Its change to the game's code (a mixin) was running when it crashed.".to_string())
                    } else {
                        (
                            Certainty::Possible,
                            format!("Its change to the game's code (a mixin) called {first_name}, where the error happened."),
                        )
                    };
                    first = false;
                    add(&mut out, Raw { mod_id: mid, jar: String::new(), certainty, why, class: String::new(), needs: Vec::new() });
                }
                continue;
            }
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
            add(&mut out, Raw { mod_id: id, jar, certainty, why, class: String::new(), needs: Vec::new() });
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
                    class: String::new(),
                    needs: Vec::new(),
                },
            );
        }
    }
    fabric_signals(&lines, &mut out);
    out.sort_by_key(|r| r.certainty);
    out
}

/// `- Mod 'Sodium Extra' (sodium-extra) 0.5.1 requires version 0.5.0 of mod 'Sodium' (sodium), …`
static FABRIC_MOD: LazyLock<Regex> =
    LazyLock::new(|| re(r"Mod '[^']+' \(([\w.-]+)\) \S+ (requires|is incompatible with|conflicts with|breaks) (.+)$"));
/// `mod 'Sodium' (sodium)` inside such a line.
static FABRIC_OTHER: LazyLock<Regex> = LazyLock::new(|| re(r"mod '[^']+' \(([\w.-]+)\)"));
/// A frame with no jar: `at com.x.Y.z(Y.java:1)` (any module prefix).
static PLAIN_FRAME: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*at\s+(?:[\w.@-]+/)*([\w$]+(?:\.[\w$]+)+)\.[\w$<>]+\("));
/// A mixin's injected method: `handler$zza000$sodium$onRender`.
static MIXIN_METHOD: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\.(?:handler|redirect|modify\w*|wrapOperation|wrapWithCondition|localvar|constant)\$\w+\$([a-z][\w-]*)\$")
});

/// Classes that are the game, the loaders or common libraries.
const LIBRARY_PACKAGES: [&str; 18] = [
    "java.", "javax.", "jdk.", "sun.", "com.sun.", "net.minecraft.", "com.mojang.", "net.fabricmc.",
    "org.quiltmc.", "net.neoforged.", "net.minecraftforge.", "cpw.mods.", "org.spongepowered.", "com.google.",
    "org.apache.", "io.netty.", "it.unimi.", "org.lwjgl.",
];

/// Fabric and Quilt: the loader's refusals, and the exception's frames,
/// which name no jar there.
fn fabric_signals(lines: &[&str], out: &mut Vec<Raw>) {
    for l in lines {
        let Some(m) = FABRIC_MOD.captures(l) else { continue };
        let rest = m[3].trim_end_matches(['!', '.']).to_string();
        let missing = rest.contains("which is missing");
        let needs = match (missing, FABRIC_OTHER.captures(&rest)) {
            (true, Some(o)) => {
                let versions = rest.split(" of mod ").next().unwrap_or("").trim_start_matches("version ").trim();
                vec![(o[1].to_string(), if versions.starts_with("any") { String::new() } else { versions.to_string() })]
            }
            _ => Vec::new(),
        };
        add(
            out,
            Raw {
                mod_id: m[1].to_string(),
                jar: String::new(),
                certainty: Certainty::Certain,
                why: format!("The loader refused it: it {} {}.", &m[2], strip_names(&rest)),
                class: String::new(),
                needs,
            },
        );
    }
    // The first stack, if its frames name no jar.
    let Some(start) = lines.iter().position(|l| PLAIN_FRAME.is_match(l)) else { return };
    if FRAME.is_match(lines[start]) {
        return; // NeoForge/Forge: handled with the jar names.
    }
    let mut first = out.iter().all(|r| r.certainty != Certainty::Likely);
    for l in lines[start..].iter().take_while(|l| !l.trim().is_empty() && !l.starts_with("A detailed")) {
        if let Some(m) = MIXIN_METHOD.captures(l) {
            add(
                out,
                Raw {
                    mod_id: m[1].to_string(),
                    jar: String::new(),
                    certainty: if first { Certainty::Likely } else { Certainty::Possible },
                    why: "Its change to the game's code (a mixin) was running when it crashed.".into(),
                    class: String::new(),
                    needs: Vec::new(),
                },
            );
            first = false;
            continue;
        }
        let Some(m) = PLAIN_FRAME.captures(l) else { continue };
        let class = m[1].to_string();
        if LIBRARY_PACKAGES.iter().any(|p| class.starts_with(p)) {
            continue;
        }
        out.push(Raw {
            mod_id: String::new(),
            jar: String::new(),
            certainty: if first { Certainty::Likely } else { Certainty::Possible },
            why: if first {
                "The error happened in its code.".into()
            } else {
                "Its code was in the chain that led to the error.".into()
            },
            class,
            needs: Vec::new(),
        });
        first = false;
    }
}

/// A library (a mod at least [`crate::bisect::LIBRARY_MIN`] others need,
/// like Architectury or Cloth Config) is rarely the cause just because the
/// error passed through its code: it does work for the mods that call it.
/// Such a suspect drops to possible, and the first other mod in the chain
/// becomes the likely one. What the loader itself names stays certain.
fn demote_libraries(suspects: &mut [Suspect], dependents: &std::collections::HashMap<String, usize>, ids: &impl Fn(&str) -> Vec<String>) {
    let users = |s: &Suspect| -> usize {
        let mut own = ids(&s.file_name);
        if !s.mod_id.is_empty() {
            own.push(s.mod_id.clone());
        }
        own.iter().map(|i| dependents.get(i).copied().unwrap_or(0)).max().unwrap_or(0)
    };
    let mut library: Option<String> = None;
    for s in suspects.iter_mut().filter(|s| s.certainty == Certainty::Likely) {
        let n = users(s);
        if n >= crate::bisect::LIBRARY_MIN {
            s.certainty = Certainty::Possible;
            s.why = format!(
                "A library {n} of your mods use: the error happened in its code, but the mod that used it is more often the cause."
            );
            library.get_or_insert_with(|| if s.mod_id.is_empty() { s.file_name.clone() } else { s.mod_id.clone() });
        }
    }
    let Some(lib) = library else { return };
    if suspects.iter().any(|s| s.certainty <= Certainty::Likely) {
        return;
    }
    // The first mod in the chain that isn't a library.
    if let Some(s) = suspects.iter_mut().find(|s| s.certainty == Certainty::Possible && users(s) < crate::bisect::LIBRARY_MIN) {
        s.certainty = Certainty::Likely;
        s.why = format!("Its code called {lib}, where the error happened: more likely the cause than the library.");
    }
    suspects.sort_by_key(|s| s.certainty);
}

/// "mod 'Sodium' (sodium)" read as "Sodium".
fn strip_names(s: &str) -> String {
    FABRIC_OTHER
        .replace_all(s, |c: &regex::Captures| {
            let whole = &c[0];
            whole.split('\'').nth(1).unwrap_or(whole).to_string()
        })
        .into_owned()
}

/// NeoForge's "Mod x requires y 1.0 or above … Currently, y is not
/// installed": (y, "1.0 or above").
fn needs_in(why: &str) -> Vec<(String, String)> {
    static NEEDS: LazyLock<Regex> =
        LazyLock::new(|| re(r"Mod [\w.-]+ requires ([\w.-]+) (.+?)\.?\s+Currently, ([\w.-]+) is not installed"));
    NEEDS
        .captures_iter(why)
        .filter(|c| c[1] == c[3])
        .map(|c| (c[1].to_string(), c[2].trim().to_string()))
        .collect()
}

/// Which jar has each class package, from the jars' file lists: a package
/// in more than one jar (a shaded library) belongs to none.
#[derive(Default)]
pub struct PackageIndex {
    owner: std::collections::HashMap<String, Option<String>>,
}

impl PackageIndex {
    /// Reads the file list of each jar in `dir` named in `mods`.
    pub fn build(dir: &Path, mods: &[String]) -> Self {
        let mut idx = Self::default();
        for m in mods.iter().filter(|m| m.ends_with(".jar")) {
            let Ok(f) = fs::File::open(dir.join(m)) else { continue };
            let Ok(z) = zip::ZipArchive::new(f) else { continue };
            let pkgs: std::collections::HashSet<String> = z
                .file_names()
                .filter(|n| n.ends_with(".class") && !n.starts_with("META-INF/"))
                .filter_map(|n| n.rsplit_once('/').map(|(p, _)| p.replace('/', ".")))
                .collect();
            for p in pkgs {
                idx.owner
                    .entry(p)
                    .and_modify(|o| {
                        if o.as_deref() != Some(m.as_str()) {
                            *o = None;
                        }
                    })
                    .or_insert_with(|| Some(m.clone()));
            }
        }
        idx
    }

    /// The jar a class (`com.x.Y$Z`) is in.
    pub fn jar_of(&self, class: &str) -> Option<&str> {
        let pkg = class.rsplit_once('.')?.0;
        self.owner.get(pkg)?.as_deref()
    }
}

fn same(r: &Raw, id: &str, jar: &str) -> bool {
    (!id.is_empty() && r.mod_id.eq_ignore_ascii_case(id)) || (!jar.is_empty() && r.jar.eq_ignore_ascii_case(jar))
}

/// Adds `r`, or fills in what an earlier finding for the same mod lacked
/// (keeping the more certain one).
fn add(out: &mut Vec<Raw>, r: Raw) {
    match out.iter_mut().find(|e| same(e, &r.mod_id, &r.jar)) {
        Some(e) => {
            for n in r.needs {
                if !e.needs.contains(&n) {
                    e.needs.push(n);
                }
            }
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
    let log_path = root.join("logs").join("latest.log");
    let log = tail(&log_path, 2 << 20);
    let loading = loading_lines(&log_path);
    // Backups made after this crash (by a fix) aren't what changed before it.
    let before = report.as_deref().and_then(modified_secs).unwrap_or(now);
    let (suspects, stopped) = analyze(root, &text, &log, &loading, before, mods, &ids);
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
    Some(Crash { when: now, source, headline, kind, suspects, turned_off: Vec::new(), stopped })
}

/// A saved crash read again from its report with today's rules (which mods
/// count as libraries, which frames name a mod), so an older card doesn't
/// keep an old guess. `None` when its report is gone or isn't a crash
/// report, or nothing changed.
pub fn reread(root: &Path, saved: &Crash, mods: &[String], ids: impl Fn(&str) -> Vec<String>) -> Option<Crash> {
    let is_report = saved.source.extension().is_some_and(|e| e == "txt") && saved.source.is_file();
    if !is_report || saved.kind != CrashKind::Mods {
        return None;
    }
    // The game log still from the same run (written within minutes of the
    // report) says whether loading had already failed.
    let log_path = root.join("logs").join("latest.log");
    let same_run = match (modified_secs(&saved.source), modified_secs(&log_path)) {
        (Some(a), Some(b)) => (a - b).abs() < 300,
        _ => false,
    };
    let loading = if same_run { loading_lines(&log_path) } else { String::new() };
    let before = modified_secs(&saved.source).unwrap_or(saved.when);
    let (mut suspects, stopped) = analyze(root, &tail(&saved.source, 2 << 20), "", &loading, before, mods, &ids);
    // A change already put back (its older copy left backups for the mods
    // folder) stays on the card, to say so.
    for s in suspects.iter_mut().filter(|s| s.changed.is_none()) {
        let kept = saved.suspects.iter().find(|o| o.file_name == s.file_name).and_then(|o| o.changed.clone());
        if let Some(ch) = kept
            && !ch.backup.exists()
            && ch.backup.file_name().is_some_and(|n| root.join("mods").join(n).exists())
        {
            s.changed = Some(ch);
        }
    }
    if suspects.is_empty() || (suspects == saved.suspects && stopped == saved.stopped) {
        return None;
    }
    Some(Crash { suspects, stopped, ..saved.clone() })
}

/// The mods a crash report (and the game log) points at, most likely first.
/// `loading` is what [`loading_lines`] kept of the whole game log.
/// Only backups set aside by `before` (the crash) count as what changed.
fn analyze(
    root: &Path,
    text: &str,
    log: &str,
    loading: &str,
    before: i64,
    mods: &[String],
    ids: &impl Fn(&str) -> Vec<String>,
) -> (Vec<Suspect>, Option<Stopped>) {
    let mut raws = signals(text);
    for r in signals(log).into_iter().filter(|r| r.certainty != Certainty::Possible) {
        add(&mut raws, r);
    }
    knock_on(&mut raws, loading);
    // Frames that name no jar (Fabric): the jar that has the class's package.
    if raws.iter().any(|r| !r.class.is_empty()) {
        let idx = PackageIndex::build(&root.join("mods"), mods);
        let mut resolved: Vec<Raw> = Vec::new();
        for mut r in std::mem::take(&mut raws) {
            if !r.class.is_empty() {
                match idx.jar_of(&r.class) {
                    Some(j) => r.jar = j.to_string(),
                    None => continue,
                }
            }
            add(&mut resolved, r);
        }
        raws = resolved;
    }
    raws.sort_by_key(|r| r.certainty);
    let mut suspects: Vec<Suspect> = Vec::new();
    for r in raws {
        let Some(file) = match_jar(&r, mods, ids) else { continue };
        if suspects.iter().any(|s| s.file_name == file) {
            continue;
        }
        suspects.push(Suspect { file_name: file, mod_id: r.mod_id, certainty: r.certainty, why: r.why, needs: r.needs, changed: None });
    }
    demote_libraries(&mut suspects, &crate::deps::dependents(&root.join("mods")), ids);
    // What the loader names may say which other mod it wanted: if that one
    // changed lately, the change is the likely trigger.
    let sets: Vec<crate::backups::Set> = crate::backups::list(root).into_iter().filter(|s| s.when <= before).collect();
    for s in suspects.iter_mut().filter(|s| s.certainty == Certainty::Certain) {
        s.changed = what_changed(root, s, mods, &sets);
    }
    let stopped = stopped(root, loading, &suspects, mods, ids);
    (suspects, stopped)
}

/// A jar's name for people when the instance's list has none: the name the
/// jar gives itself, else its file name without ".jar".
pub fn jar_name(mods_dir: &Path, file: &str) -> String {
    let bare = file.trim_end_matches(".disabled");
    let (_, name) = crate::deps::own_ids(&mods_dir.join(file));
    if name.is_empty() { bare.trim_end_matches(".jar").to_string() } else { name }
}

/// Puts the older copy of a changed mod back: the jar there now goes to
/// `.hopper-backups` first (so Backups can undo this), then the older copy
/// returns to the mods folder. The restored jar's path.
pub fn put_back(root: &Path, c: &Changed) -> anyhow::Result<PathBuf> {
    let saved = crate::backups::list(root)
        .into_iter()
        .flat_map(|s| s.files)
        .find(|f| f.path == c.backup)
        .ok_or_else(|| anyhow::anyhow!("The older copy of {} isn't in backups any more.", c.name))?;
    let now = root.join("mods").join(&c.file_name);
    if now.is_file() {
        let dir = root.join(crate::sync::BACKUP_DIR).join(crate::time::now().to_string());
        fs::create_dir_all(&dir)?;
        let aside = dir.join(&c.file_name);
        if fs::rename(&now, &aside).is_err() {
            fs::copy(&now, &aside)?;
            fs::remove_file(&now)?;
        }
        crate::backups::record(&dir, &aside, &format!("mods/{}", c.file_name));
    }
    crate::backups::restore(root, &saved)
}

/// `Skipping Mod Construction task for mod x because a dependency threw an exception.`
static SKIPPED: LazyLock<Regex> =
    LazyLock::new(|| re(r"Skipping .+? task for mod ([\w.-]+) because a dependency threw an exception"));

/// The first mod the loader says failed, as a story for the card, with
/// the mods skipped because of it. Those lines are only in `debug.log`
/// (of the same run as `latest.log`).
fn stopped(root: &Path, loading: &str, suspects: &[Suspect], mods: &[String], ids: &impl Fn(&str) -> Vec<String>) -> Option<Stopped> {
    let (_, failed) = loading_failures(loading);
    let (raw, error) = failed.into_iter().next()?;
    let s = suspects.iter().find(|s| s.certainty == Certainty::Certain && s.mod_id.eq_ignore_ascii_case(&raw.mod_id))?;
    let logs = root.join("logs");
    let same_run = match (modified_secs(&logs.join("debug.log")), modified_secs(&logs.join("latest.log"))) {
        (Some(a), Some(b)) => (a - b).abs() < 300,
        _ => false,
    };
    let mut skipped: Vec<String> = Vec::new();
    if same_run {
        for id in skipped_ids(&logs.join("debug.log")) {
            let r = Raw { mod_id: id, jar: String::new(), certainty: Certainty::Possible, why: String::new(), class: String::new(), needs: Vec::new() };
            if let Some(f) = match_jar(&r, mods, ids)
                && f != s.file_name
                && !skipped.contains(&f)
            {
                skipped.push(f);
            }
        }
    }
    Some(Stopped { file_name: s.file_name.clone(), mod_id: s.mod_id.clone(), error, skipped })
}

/// The mod ids `debug.log` says were skipped because a mod they need
/// failed (the whole file, up to 256 MB, line by line).
fn skipped_ids(p: &Path) -> Vec<String> {
    use std::io::{BufRead, Read};
    let Ok(f) = fs::File::open(p) else { return Vec::new() };
    let mut r = std::io::BufReader::new(f.take(256 << 20));
    let (mut out, mut buf) = (Vec::new(), Vec::new());
    while r.read_until(b'\n', &mut buf).is_ok_and(|n| n > 0) {
        // Cheap check before the regex: most lines aren't these.
        if buf.windows(9).any(|w| w == b"Skipping ") {
            let l = String::from_utf8_lossy(&buf);
            if let Some(c) = SKIPPED.captures(&l) {
                let id = c[1].to_string();
                if !out.contains(&id) {
                    out.push(id);
                }
            }
        }
        buf.clear();
    }
    out
}

/// The loader's own words that loading had already failed when the game
/// went on (and then crashed somewhere else).
static BROKEN: LazyLock<Regex> = LazyLock::new(|| {
    re(r"Failed to wait for future .+?, \d+ errors? found|Cowardly refusing to send event \S+ to a broken mod state")
});
/// The loader naming the mod that failed while loading (NeoForge, Forge).
static LOAD_FAILED: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r"Failed to create mod instance\. ModID: ([\w.-]+)",
        r"|Caught exception during event \S+ dispatch for modid ([\w.-]+)",
        r"|Failed to register automatic subscribers\. ModID: ([\w.-]+)",
        r"|Failed to apply mixin\. Mixin Class: \S+, ModID: ([\w.-]+)",
    ))
});
/// `java.lang.IllegalStateException: message`, or `Caused by: …`.
static EXCEPTION: LazyLock<Regex> =
    LazyLock::new(|| re(r"^\s*(?:Caused by: )?(?:[\w$]+\.)+[\w$]*(?:Exception|Error|Throwable)[\w$]*: (.+)$"));

/// What [`loading_lines`] looks for.
const LOADING_MARKS: [&str; 6] = [
    "Failed to wait for future",
    "to a broken mod state",
    "Failed to create mod instance",
    "dispatch for modid",
    "Failed to register automatic subscribers",
    "Failed to apply mixin. Mixin Class",
];

/// The lines of a whole game log that say loading failed, each with the
/// error under it. They come early in a long log, before the last 2 MB
/// the rest of the scan reads, so the whole file (up to 64 MB) is read
/// line by line and only these are kept.
pub fn loading_lines(p: &Path) -> String {
    use std::io::{BufRead, Read};
    let Ok(f) = fs::File::open(p) else { return String::new() };
    let mut r = std::io::BufReader::new(f.take(64 << 20));
    let (mut out, mut buf, mut keep) = (String::new(), Vec::new(), 0usize);
    while r.read_until(b'\n', &mut buf).is_ok_and(|n| n > 0) {
        let l = String::from_utf8_lossy(&buf);
        if LOADING_MARKS.iter().any(|m| l.contains(m)) {
            keep = 60;
        } else if l.starts_with('[') {
            // The next log entry: the error under the mark has ended.
            keep = 0;
        }
        if keep > 0 {
            out.push_str(l.trim_end_matches(['\r', '\n']));
            out.push('\n');
            keep -= 1;
        }
        buf.clear();
    }
    out
}

/// Whether loading had already failed, and the mods the loader says
/// failed, each with its error as the reason.
fn loading_failures(text: &str) -> (bool, Vec<(Raw, String)>) {
    let lines: Vec<&str> = text.lines().collect();
    let broken = lines.iter().any(|l| BROKEN.is_match(l));
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let Some(c) = LOAD_FAILED.captures(l) else { continue };
        let Some(id) = (1..=4).find_map(|g| c.get(g)).map(|g| g.as_str().to_string()) else { continue };
        if NOT_MODS.contains(&id.as_str()) {
            continue;
        }
        // The error under it; the deepest "Caused by" is nearest the cause.
        let mut msg = String::new();
        for (k, n) in lines.iter().enumerate().skip(i + 1).take(60) {
            if n.starts_with('[') {
                break;
            }
            if let Some(m) = EXCEPTION.captures(n)
                && (k == i + 1 || n.trim_start().starts_with("Caused by:"))
            {
                msg = m[1].trim().to_string();
            }
        }
        let why = if msg.is_empty() {
            "It failed while the game was loading, so the rest couldn't finish loading.".to_string()
        } else {
            format!("It failed while the game was loading: \"{}\"", cut(&msg, 300))
        };
        if !out.iter().any(|(r, _): &(Raw, String)| r.mod_id == id) {
            out.push((Raw { mod_id: id, jar: String::new(), certainty: Certainty::Certain, why, class: String::new(), needs: Vec::new() }, msg));
        }
    }
    (broken, out)
}

/// When a mod had already failed to load, the game can go on a little and
/// crash somewhere else: that crash report's error is a knock-on effect.
/// The mod that failed becomes certain, and what the report's trace names
/// drops to possible. If the log only says loading was broken, the trace
/// still drops to possible (the cause is earlier in the log).
fn knock_on(raws: &mut Vec<Raw>, loading: &str) {
    let (broken, failed) = loading_failures(loading);
    let failed: Vec<Raw> = failed.into_iter().map(|(r, _)| r).collect();
    if failed.is_empty() && !broken {
        return;
    }
    let why = match failed.first() {
        Some(f) => format!("It crashed after {} had already failed to load, so this error most likely follows from that one.", f.mod_id),
        None => "Loading had already failed before this crash, so this error most likely follows from an earlier one in the game log.".to_string(),
    };
    for r in raws.iter_mut().filter(|r| r.certainty != Certainty::Certain && !failed.iter().any(|f| same(f, &r.mod_id, &r.jar))) {
        r.certainty = Certainty::Possible;
        r.why = why.clone();
    }
    for f in failed {
        add(raws, f);
    }
    raws.sort_by_key(|r| r.certainty);
}

/// Text as plain words, to match names however a mod writes them:
/// "Forbidden & Arcanus", "Forbidden Arcanus" and "forbidden_arcanus" all
/// read " forbidden arcanus ".
fn words(s: &str) -> String {
    let lower = s.to_lowercase();
    let plain: String = lower.chars().map(|c| if c.is_alphanumeric() || c == '.' { c } else { ' ' }).collect();
    let kept: Vec<&str> = plain
        .split_whitespace()
        .map(|w| w.trim_matches('.'))
        .filter(|w| !w.is_empty() && *w != "and" && *w != "the")
        .collect();
    format!(" {} ", kept.join(" "))
}

/// Whether `text` names `name` (a display name or a mod id) as whole
/// words. A one-word name counts only written with a capital, as a name
/// ("requires Create 6.0", not "failed to create").
fn mentions(text: &str, name: &str) -> bool {
    let n = words(name);
    let bare = n.trim();
    if bare.chars().count() < 4 {
        return false;
    }
    if bare.contains(' ') {
        return words(text).contains(&n);
    }
    text.split(|c: char| !c.is_alphanumeric())
        .any(|w| w.eq_ignore_ascii_case(bare) && w.chars().next().is_some_and(char::is_uppercase))
}

/// Another installed mod that `s`'s reason names (by display name or mod
/// id) and that Hopper set an older copy of aside in `.hopper-backups`
/// (it was updated or replaced), newest copy first.
fn what_changed(root: &Path, s: &Suspect, mods: &[String], sets: &[crate::backups::Set]) -> Option<Changed> {
    if sets.is_empty() || s.why.is_empty() {
        return None;
    }
    let why = &s.why;
    let dir = root.join("mods");
    for m in mods.iter().filter(|m| m.to_ascii_lowercase().ends_with(".jar") && **m != s.file_name) {
        let (ids, name) = crate::deps::own_ids(&dir.join(m));
        let named = mentions(why, &name) || ids.iter().any(|i| !NOT_MODS.contains(&i.as_str()) && mentions(why, i));
        if !named || ids.is_empty() {
            continue;
        }
        let to = crate::deps::own_version(&dir.join(m));
        for set in sets {
            for f in set.files.iter().filter(|f| f.restore_to.starts_with("mods/") && f.name.to_ascii_lowercase().ends_with(".jar")) {
                if !crate::deps::own_ids(&f.path).0.iter().any(|i| ids.contains(i)) {
                    continue;
                }
                let from = crate::deps::own_version(&f.path);
                if !from.is_empty() && from == to {
                    continue; // the same version: not a change
                }
                return Some(Changed {
                    file_name: m.clone(),
                    name: if name.is_empty() { ids[0].clone() } else { name },
                    from,
                    to,
                    when: set.when,
                    backup: f.path.clone(),
                });
            }
        }
    }
    None
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

/// Watches running games, every few seconds: a crash report written while
/// the game is still open (NeoForge keeps its error screen up until it's
/// closed), and mixin failures the log reports while you play.
#[derive(Default)]
pub struct LiveWatch {
    /// Per running game (by folder): (start, crash report already told,
    /// log bytes read, mods already warned about).
    games: std::collections::HashMap<PathBuf, LiveGame>,
}

#[derive(Default)]
struct LiveGame {
    started: i64,
    reported: Option<PathBuf>,
    log_read: u64,
    warned: Vec<String>,
}

/// What one look at a running game found.
#[derive(Default)]
pub struct LiveNews {
    /// A crash report the game wrote while still open.
    pub crash: Option<Crash>,
    /// Mods whose mixins failed to apply, new since the last look.
    pub warnings: Vec<Suspect>,
}

impl LiveWatch {
    /// Forgets games that ended.
    pub fn retain(&mut self, running: &[PathBuf]) {
        self.games.retain(|k, _| running.contains(k));
    }

    /// Whether this game's crash was already told while it ran.
    pub fn reported(&self, root: &Path) -> Option<&Path> {
        self.games.get(root)?.reported.as_deref()
    }

    /// One look at the game in `root`, which started at `started`.
    pub fn look(&mut self, root: &Path, started: i64, mods: &[String], ids: impl Fn(&str) -> Vec<String>) -> LiveNews {
        let g = self.games.entry(root.to_path_buf()).or_default();
        if g.started != started {
            *g = LiveGame { started, ..Default::default() };
        }
        let mut news = LiveNews::default();
        // A crash report while it's open.
        let newest = crate::logs::crash_reports(root)
            .into_iter()
            .find(|p| modified_secs(p).is_some_and(|t| t >= started));
        if let Some(p) = newest
            && g.reported.as_ref() != Some(&p)
        {
            g.reported = Some(p);
            news.crash = detect(root, started, mods, &ids);
        }
        // New log lines: mixin failures.
        let log = root.join("logs").join("latest.log");
        let len = fs::metadata(&log).map_or(0, |m| m.len());
        if len < g.log_read {
            g.log_read = 0; // a new log
        }
        if len > g.log_read {
            let text = read_from(&log, g.log_read, 1 << 20);
            g.log_read = len;
            for r in signals(&text).into_iter().filter(|r| r.why.contains("mixin")) {
                let Some(file) = match_jar(&r, mods, &ids) else { continue };
                if g.warned.contains(&file) {
                    continue;
                }
                g.warned.push(file.clone());
                news.warnings.push(Suspect {
                    file_name: file,
                    mod_id: r.mod_id,
                    certainty: Certainty::Possible,
                    why: "Its changes to the game's code (a mixin) failed to apply while you played. It may misbehave or crash later.".into(),
                    needs: Vec::new(),
                    changed: None,
                });
            }
        }
        news
    }
}

/// Up to `max` bytes of `p` from `from` (the newest ones when there are
/// more), as text starting at a line.
fn read_from(p: &Path, from: u64, max: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = fs::File::open(p) else { return String::new() };
    let len = f.metadata().map_or(0, |m| m.len());
    let start = from.max(len.saturating_sub(max));
    let _ = f.seek(SeekFrom::Start(start));
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf).into_owned();
    if start > from {
        // Cut mid-line: drop the partial first line.
        text.split_once('\n').map(|(_, r)| r.to_string()).unwrap_or_default()
    } else {
        text
    }
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

    fn sus(file: &str, id: &str, c: Certainty) -> Suspect {
        Suspect { file_name: file.into(), mod_id: id.into(), certainty: c, why: String::new(), needs: Vec::new(), changed: None }
    }

    #[test]
    fn a_library_is_not_the_likely_cause() {
        let deps: std::collections::HashMap<String, usize> = [("architectury".to_string(), 40)].into();
        let ids = |f: &str| if f.starts_with("arch") { vec!["architectury".to_string()] } else { vec![] };
        // The library first in the chain, a mod that called it next.
        let mut s = vec![sus("architectury.jar", "architectury", Certainty::Likely), sus("caller.jar", "caller", Certainty::Possible)];
        demote_libraries(&mut s, &deps, &ids);
        assert_eq!((s[0].file_name.as_str(), s[0].certainty), ("caller.jar", Certainty::Likely));
        assert_eq!(s[1].certainty, Certainty::Possible);
        assert!(s[0].why.contains("architectury"));
        // Only the library: nothing is called likely.
        let mut s = vec![sus("architectury.jar", "architectury", Certainty::Likely)];
        demote_libraries(&mut s, &deps, &ids);
        assert_eq!(s[0].certainty, Certainty::Possible);
        // What the loader names stays certain, library or not.
        let mut s = vec![sus("architectury.jar", "architectury", Certainty::Certain)];
        demote_libraries(&mut s, &deps, &ids);
        assert_eq!(s[0].certainty, Certainty::Certain);
    }

    /// The newest crash of the instance in `HOPPER_CRASH_ROOT`, read again
    /// (read only: nothing is saved).
    #[test]
    #[ignore]
    fn detect_live() {
        let Ok(root) = std::env::var("HOPPER_CRASH_ROOT") else { return };
        let root = PathBuf::from(root);
        let dir = root.join("mods");
        let mods: Vec<String> = fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".jar") || n.ends_with(".jar.disabled"))
            .collect();
        let c = detect(&root, 0, &mods, |f| crate::deps::own_ids(&dir.join(f)).0).expect("a crash");
        println!("headline: {}", c.headline);
        for s in &c.suspects {
            println!("{:?} {} ({}): {}", s.certainty, s.file_name, s.mod_id, s.why);
            if let Some(ch) = &s.changed {
                println!("   changed: {} {} -> {} at {} (old copy {})", ch.name, ch.from, ch.to, ch.when, ch.backup.display());
            }
        }
        if let Some(st) = &c.stopped {
            println!("stopped: {} | error: {} | skipped: {:?}", st.file_name, st.error, st.skipped);
        }
    }

    /// The saved crash of the instance in `HOPPER_CRASH_ROOT` read again
    /// (read only: printed, not saved).
    #[test]
    #[ignore]
    fn reread_live() {
        let Ok(root) = std::env::var("HOPPER_CRASH_ROOT") else { return };
        let root = PathBuf::from(root);
        let dir = root.join("mods");
        let mods: Vec<String> = fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".jar") || n.ends_with(".jar.disabled"))
            .collect();
        let saved = load(&root).expect("a saved crash");
        println!("saved: {:?}", saved.suspects.iter().map(|s| (s.certainty, s.file_name.clone())).collect::<Vec<_>>());
        match reread(&root, &saved, &mods, |f| crate::deps::own_ids(&dir.join(f)).0) {
            Some(c) => println!(
                "reread: {:?}",
                c.suspects.iter().map(|s| (s.certainty, s.file_name.clone(), s.changed.as_ref().map(|ch| (ch.from.clone(), ch.to.clone())))).collect::<Vec<_>>()
            ),
            None => println!("reread: unchanged"),
        }
    }

    /// How many mods in `HOPPER_DEPS_DIR` need each of a few libraries (read only).
    #[test]
    #[ignore]
    fn dependents_live() {
        let Ok(dir) = std::env::var("HOPPER_DEPS_DIR") else { return };
        let d = crate::deps::dependents(Path::new(&dir));
        for id in ["architectury", "cloth_config", "geckolib", "sable", "create"] {
            println!("{id}: {}", d.get(id).copied().unwrap_or(0));
        }
    }

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
        assert!(s[0].why.contains("requires neoforge 21.1.259 or above. Currently, neoforge is 21.1.256"), "{}", s[0].why);
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
    fn neoforge_missing_dependency_is_offered() {
        let t = "-- Mod loading issue for: coolmod --\nDetails:\n\tMod file: /x/mods/coolmod-1.0.jar\n\tFailure message: Mod coolmod requires balm 21.0.0 or above\n\t\tCurrently, balm is not installed\n\t\t\n\tMod version: 1.0\n";
        let s = signals(t);
        assert_eq!(s[0].needs, [("balm".to_string(), "21.0.0 or above".to_string())]);
    }

    #[test]
    fn fabric_refusals_and_missing_mods() {
        let log = "Incompatible mods found!\n - Mod 'Sodium Extra' (sodium-extra) 0.5.1 requires version 0.5.0 of mod 'Sodium' (sodium), but only the wrong version is present: 0.4.10!\n - Mod 'Cool Mod' (coolmod) 1.0 requires any version of mod 'Cloth Config API' (cloth-config), which is missing!\n";
        let s = signals(log);
        let ids: Vec<&str> = s.iter().map(|r| r.mod_id.as_str()).collect();
        assert_eq!(ids, ["sodium-extra", "coolmod"]);
        assert!(s.iter().all(|r| r.certainty == Certainty::Certain));
        assert!(s[0].why.contains("version 0.5.0 of Sodium"), "{}", s[0].why);
        assert!(s[0].needs.is_empty(), "wrong version, not missing");
        assert_eq!(s[1].needs, [("cloth-config".to_string(), String::new())]);
    }

    #[test]
    fn fabric_frames_go_through_packages_and_mixin_names() {
        let t = "Description: Rendering overlay\n\njava.lang.NullPointerException: oops\n\tat com.cool.render.Thing.draw(Thing.java:10)\n\tat net.minecraft.class_310.handler$zza000$sodium$onRender(class_310.java:1)\n\tat com.other.lib.Util.call(Util.java:3)\n\tat net.minecraft.class_310.method_1523(class_310.java:2)\n";
        let s = signals(t);
        assert_eq!(s[0].class, "com.cool.render.Thing");
        assert_eq!(s[0].certainty, Certainty::Likely);
        assert_eq!((s[1].mod_id.as_str(), s[1].certainty), ("sodium", Certainty::Possible));
        assert_eq!(s[2].class, "com.other.lib.Util");

        // Packages to jars, from real zip file lists.
        let dir = std::env::temp_dir().join(format!("hopper-pkgidx-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let jar = |name: &str, classes: &[&str]| {
            use std::io::Write;
            let mut z = zip::ZipWriter::new(fs::File::create(dir.join(name)).unwrap());
            for c in classes {
                z.start_file(*c, zip::write::SimpleFileOptions::default()).unwrap();
                z.write_all(b"x").unwrap();
            }
            z.finish().unwrap();
        };
        jar("cool-1.0.jar", &["com/cool/render/Thing.class", "shaded/lib/A.class"]);
        jar("other-2.0.jar", &["com/other/lib/Util.class", "shaded/lib/B.class"]);
        let mods = vec!["cool-1.0.jar".to_string(), "other-2.0.jar".to_string()];
        let idx = PackageIndex::build(&dir, &mods);
        assert_eq!(idx.jar_of("com.cool.render.Thing"), Some("cool-1.0.jar"));
        assert_eq!(idx.jar_of("com.other.lib.Util$Inner"), Some("other-2.0.jar"));
        assert_eq!(idx.jar_of("shaded.lib.A"), None, "a package in two jars is no one's");

        // And through detect.
        fs::create_dir_all(dir.join("root/crash-reports")).unwrap();
        fs::create_dir_all(dir.join("root/logs")).unwrap();
        fs::create_dir_all(dir.join("root/mods")).unwrap();
        for m in &mods {
            fs::copy(dir.join(m), dir.join("root/mods").join(m)).unwrap();
        }
        fs::write(dir.join("root/logs/latest.log"), "").unwrap();
        fs::write(dir.join("root/crash-reports/crash-x-client.txt"), t).unwrap();
        let c = detect(&dir.join("root"), crate::time::now() - 5, &mods, |_| Vec::new()).unwrap();
        let files: Vec<&str> = c.suspects.iter().map(|s| s.file_name.as_str()).collect();
        assert_eq!(files, ["cool-1.0.jar", "other-2.0.jar"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn live_watch_tells_a_crash_once_and_new_mixin_failures() {
        let root = std::env::temp_dir().join(format!("hopper-live-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("crash-reports")).unwrap();
        fs::create_dir_all(root.join("logs")).unwrap();
        let mods = vec!["coolmod-1.0.jar".to_string(), "mobflowutilities-3.1.10.10-mc-1.21.1.jar".to_string()];
        let ids = |f: &str| if f.starts_with("coolmod") { vec!["coolmod".to_string()] } else { Vec::new() };
        let started = crate::time::now() - 5;
        let mut w = LiveWatch::default();
        fs::write(root.join("logs/latest.log"), "[main/INFO]: starting
").unwrap();
        let n = w.look(&root, started, &mods, ids);
        assert!(n.crash.is_none() && n.warnings.is_empty());
        // A mixin failure appears.
        let mut log = fs::read_to_string(root.join("logs/latest.log")).unwrap();
        log.push_str("[main/ERROR] [mixin/]: Mixin apply for mod coolmod failed coolmod.mixins.json:A
");
        fs::write(root.join("logs/latest.log"), &log).unwrap();
        let n = w.look(&root, started, &mods, ids);
        assert_eq!(n.warnings.len(), 1);
        assert_eq!(n.warnings[0].file_name, "coolmod-1.0.jar");
        // Not again.
        log.push_str("[main/ERROR] [mixin/]: Mixin apply for mod coolmod failed coolmod.mixins.json:B
");
        fs::write(root.join("logs/latest.log"), &log).unwrap();
        assert!(w.look(&root, started, &mods, ids).warnings.is_empty());
        // A crash report while it's open: told once.
        fs::write(root.join("crash-reports/crash-now-fml.txt"), LOADING).unwrap();
        let n = w.look(&root, started, &mods, ids);
        assert_eq!(n.crash.unwrap().suspects[0].file_name, mods[1]);
        assert!(w.look(&root, started, &mods, ids).crash.is_none());
        assert!(w.reported(&root).is_some());
        let _ = fs::remove_dir_all(&root);
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

    /// TNP Limitless 8, 2026-10-26, cut down: the report blames a mixin
    /// calling Architectury, but Mekanism Magic had already stopped loading.
    const KNOCK_ON_REPORT: &str = "Description: Initializing game

java.lang.IllegalStateException: Mod 'architectury' is not available!
\tat TRANSFORMER/architectury@13.0.11/dev.architectury.platform.hooks.EventBusesHooks.lambda$whenAvailable$0(EventBusesHooks.java:34) ~[architectury-13.0.11-neoforge.jar%231397!/:?] {re:classloading}
\tat java.base/java.util.Optional.orElseThrow(Optional.java:403) ~[?:?] {}
\tat TRANSFORMER/architectury@13.0.11/dev.architectury.networking.NetworkManager.registerReceiver(NetworkManager.java:97) ~[architectury-13.0.11-neoforge.jar%231397!/:?] {re:mixin,re:classloading}
\tat TRANSFORMER/minecraft@1.21.1/net.minecraft.client.Minecraft.handler$jip000$omegaconfig$onReturn(Minecraft.java:38520) ~[client-1.21.1-20240808.144430-srg.jar%231295!/:?] {re:mixin}
\tat TRANSFORMER/minecraft@1.21.1/net.minecraft.client.Minecraft.<init>(Minecraft.java:636) ~[client-1.21.1-20240808.144430-srg.jar%231295!/:?] {re:mixin}

A detailed walkthrough of the error
";
    const KNOCK_ON_LOG: &str = "[26Oct2026 16:46:26.698] [modloading-worker-0/ERROR] [Cristel Lib/]: Structure sets are missing from config: [goety:crypt_wing_a]
[26Oct2026 16:46:39.600] [modloading-worker-0/ERROR] [net.neoforged.fml.javafmlmod.FMLModContainer/LOADING]: Failed to create mod instance. ModID: mekanism_magic, class com.example.mekanismmagic.MekanismMagic
java.lang.IllegalStateException: Forbidden & Arcanus machine content requires 2.6.1 + Valhelsia Core 1.1.5; found 2.6.2 + 1.1.5. Startup was stopped rather than dropping saved Clibano machine registrations.
\tat TRANSFORMER/mekanism_magic@1.0.7/com.example.mekanismmagic.integration.ModCompatibility.requireForbiddenContentVersions(ModCompatibility.java:72) ~[?:?]
\tat MC-BOOTSTRAP/fml_loader@4.0.45/net.neoforged.fml.javafmlmod.FMLModContainer.constructMod(FMLModContainer.java:122) ~[loader-4.0.45.jar%23107!/:4.0]
[26Oct2026 16:46:39.620] [Render thread/INFO] [com.mojang.blaze3d.platform.GlDebug/]: OpenGL debug message
[26Oct2026 16:46:46.036] [Render thread/FATAL] [net.neoforged.fml.ModLoader/LOADING]: Failed to wait for future Mod Construction, 1 errors found
[26Oct2026 16:46:47.853] [Render thread/ERROR] [net.neoforged.fml.ModLoader/]: Cowardly refusing to send event net.neoforged.neoforge.client.event.sound.SoundEngineLoadEvent to a broken mod state
";

    #[test]
    fn a_mod_that_failed_to_load_comes_before_the_knock_on_crash() {
        let mut raws = signals(KNOCK_ON_REPORT);
        knock_on(&mut raws, KNOCK_ON_LOG);
        let got: Vec<(&str, Certainty)> = raws.iter().map(|r| (r.mod_id.as_str(), r.certainty)).collect();
        assert_eq!(
            got,
            [("mekanism_magic", Certainty::Certain), ("architectury", Certainty::Possible), ("omegaconfig", Certainty::Possible)]
        );
        assert!(raws[0].why.contains("requires 2.6.1 + Valhelsia Core 1.1.5; found 2.6.2"), "{}", raws[0].why);
        assert!(raws[1].why.contains("after mekanism_magic had already failed to load"), "{}", raws[1].why);

        // Only the loader's "broken" lines, no mod named: the trace still drops.
        let mut raws = signals(KNOCK_ON_REPORT);
        knock_on(&mut raws, "[x] [Render thread/FATAL] [ModLoader/]: Failed to wait for future Mod Construction, 2 errors found\n");
        assert!(raws.iter().all(|r| r.certainty == Certainty::Possible));
        assert!(raws[0].why.contains("earlier one in the game log"));

        // A clean log changes nothing.
        let mut raws = signals(KNOCK_ON_REPORT);
        let before = raws.clone();
        knock_on(&mut raws, "");
        assert_eq!(raws, before);
    }

    #[test]
    fn skipped_mods_come_from_debug_log() {
        let dir = std::env::temp_dir().join(format!("hopper-crash-sk-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("debug.log");
        fs::write(
            &p,
            "[26Oct2026 16:46:39.900] [modloading-worker-0/DEBUG] [net.neoforged.fml.ModLoader/]: Skipping Mod Construction task for mod mekenergistics because a dependency threw an exception.
[26Oct2026 16:46:39.901] [modloading-worker-0/DEBUG] [x/]: Skipping nothing here
[26Oct2026 16:46:39.901] [modloading-worker-0/DEBUG] [net.neoforged.fml.ModLoader/]: Skipping Mod Construction task for mod omni_upgrades because a dependency threw an exception.
[26Oct2026 16:46:40.001] [modloading-worker-0/DEBUG] [net.neoforged.fml.ModLoader/]: Skipping Common setup task for mod omni_upgrades because a dependency threw an exception.
",
        )
        .unwrap();
        assert_eq!(skipped_ids(&p), ["mekenergistics", "omni_upgrades"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn loading_lines_keep_the_marks_from_anywhere_in_the_log() {
        let dir = std::env::temp_dir().join(format!("hopper-crash-ll-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("latest.log");
        // 3 MB of noise after the failure: past the 2 MB the tail reads.
        let noise = "[26Oct2026 16:46:49.590] [Worker/INFO] [x/]: nothing to see here\n".repeat(50_000);
        fs::write(&p, format!("{KNOCK_ON_LOG}{noise}")).unwrap();
        let kept = loading_lines(&p);
        assert!(kept.contains("ModID: mekanism_magic"));
        assert!(kept.contains("found 2.6.2 + 1.1.5"));
        assert!(!kept.contains("Cristel Lib") && !kept.contains("nothing to see here"));
        let (broken, failed) = loading_failures(&kept);
        assert!(broken);
        assert_eq!(failed.len(), 1);
        assert!(failed[0].1.starts_with("Forbidden & Arcanus machine content requires 2.6.1"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_mod_an_error_names_and_its_older_copy() {
        use std::io::Write;
        let root = std::env::temp_dir().join(format!("hopper-crash-wc-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mods = root.join("mods");
        let set = root.join(".hopper-backups").join("1792845542");
        fs::create_dir_all(&mods).unwrap();
        fs::create_dir_all(&set).unwrap();
        let jar = |p: &Path, id: &str, name: &str, ver: &str| {
            let mut z = zip::ZipWriter::new(fs::File::create(p).unwrap());
            z.start_file::<_, ()>("META-INF/neoforge.mods.toml", Default::default()).unwrap();
            write!(z, "modLoader=\"javafml\"\n[[mods]]\nmodId=\"{id}\"\nversion=\"{ver}\"\ndisplayName=\"{name}\"\n").unwrap();
            z.finish().unwrap();
        };
        jar(&mods.join("mekanism_magic-1.0.7.jar"), "mekanism_magic", "Mekanism Magic", "1.0.7");
        // Its own name has no "&": the error writes it "Forbidden & Arcanus".
        jar(&mods.join("forbidden_arcanus-1.21.1-2.6.2.jar"), "forbidden_arcanus", "Forbidden Arcanus", "2.6.2");
        jar(&mods.join("valhelsia_core-1.1.5.jar"), "valhelsia_core", "Valhelsia Core", "1.1.5");
        jar(&set.join("forbidden_arcanus-2.6.1.jar"), "forbidden_arcanus", "Forbidden Arcanus", "2.6.1");
        fs::write(set.join(crate::backups::ORIGINS), r#"{"forbidden_arcanus-2.6.1.jar":"mods/forbidden_arcanus-2.6.1.jar"}"#).unwrap();
        let list: Vec<String> = ["mekanism_magic-1.0.7.jar", "forbidden_arcanus-1.21.1-2.6.2.jar", "valhelsia_core-1.1.5.jar"]
            .map(String::from)
            .to_vec();
        let (_, failed) = loading_failures(KNOCK_ON_LOG);
        let s = Suspect {
            file_name: list[0].clone(),
            mod_id: "mekanism_magic".into(),
            certainty: Certainty::Certain,
            why: failed[0].0.why.clone(),
            needs: Vec::new(),
            changed: None,
        };
        let c = what_changed(&root, &s, &list, &crate::backups::list(&root)).expect("a change");
        assert_eq!(
            (c.file_name.as_str(), c.name.as_str(), c.from.as_str(), c.to.as_str(), c.when),
            ("forbidden_arcanus-1.21.1-2.6.2.jar", "Forbidden Arcanus", "2.6.1", "2.6.2", 1792845542)
        );
        // Putting it back swaps the two: 2.6.1 in mods, 2.6.2 in backups.
        let back = put_back(&root, &c).unwrap();
        assert_eq!(back, mods.join("forbidden_arcanus-2.6.1.jar"));
        assert!(!mods.join("forbidden_arcanus-1.21.1-2.6.2.jar").exists());
        let aside: Vec<String> = crate::backups::list(&root).into_iter().flat_map(|s| s.files).map(|f| f.restore_to).collect();
        assert_eq!(aside, ["mods/forbidden_arcanus-1.21.1-2.6.2.jar"]);
        assert!(put_back(&root, &c).is_err(), "the older copy is used up");
        fs::rename(mods.join("forbidden_arcanus-2.6.1.jar"), set.join("forbidden_arcanus-2.6.1.jar")).ok();
        // Valhelsia Core is named too, but has no older copy: no change.
        let _ = fs::remove_dir_all(root.join(".hopper-backups"));
        assert_eq!(what_changed(&root, &s, &list, &crate::backups::list(&root)), None);
        // Names and ids as whole words, however they're written.
        assert!(mentions("needs Valhelsia Core 1.1.", "valhelsia_core"));
        assert!(mentions("Forbidden & Arcanus requires", "Forbidden Arcanus"));
        assert!(mentions("requires Create 6.0", "create"));
        assert!(!mentions("Failed to create mod instance", "Create"), "a word, not a name");
        assert!(!mentions("needs JEI", "jei"), "too short to trust");
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
