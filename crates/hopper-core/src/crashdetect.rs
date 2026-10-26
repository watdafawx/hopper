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
    let log = tail(&root.join("logs").join("latest.log"), 2 << 20);
    let suspects = analyze(root, &text, &log, mods, &ids);
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
    Some(Crash { when: now, source, headline, kind, suspects, turned_off: Vec::new() })
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
    let suspects = analyze(root, &tail(&saved.source, 2 << 20), "", mods, &ids);
    if suspects.is_empty() || suspects == saved.suspects {
        return None;
    }
    Some(Crash { suspects, ..saved.clone() })
}

/// The mods a crash report (and the game log) points at, most likely first.
fn analyze(root: &Path, text: &str, log: &str, mods: &[String], ids: &impl Fn(&str) -> Vec<String>) -> Vec<Suspect> {
    let mut raws = signals(text);
    for r in signals(log).into_iter().filter(|r| r.certainty != Certainty::Possible) {
        add(&mut raws, r);
    }
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
        suspects.push(Suspect { file_name: file, mod_id: r.mod_id, certainty: r.certainty, why: r.why, needs: r.needs });
    }
    demote_libraries(&mut suspects, &crate::deps::dependents(&root.join("mods")), ids);
    suspects
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
        Suspect { file_name: file.into(), mod_id: id.into(), certainty: c, why: String::new(), needs: Vec::new() }
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
            Some(c) => println!("reread: {:?}", c.suspects.iter().map(|s| (s.certainty, s.file_name.clone())).collect::<Vec<_>>()),
            None => println!("reread: unchanged"),
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
