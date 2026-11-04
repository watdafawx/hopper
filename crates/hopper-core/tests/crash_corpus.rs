//! The crash corpus: real crashes (anonymised), each replayed through the
//! same code the crash card uses, so a new rule can't quietly break an old
//! case. See `tests/crash_corpus/README.md`.
//!
//! `cargo test -p hopper-core --test crash_corpus -- --nocapture` prints the
//! table. A case with `pass = true` must name its expected mod first (at
//! least as sure as `at_least`); the others are tracked, not enforced.

use hopper_core::crashdetect::{self, Certainty, Crash};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
struct Case {
    side: String,
    /// The mod list (a file in `_mods`), as the report listed it.
    mods: String,
    /// The report's file name.
    report: String,
    /// Which of the distinct problems it is (duplicates share one).
    #[serde(default)]
    problem: u32,
    /// Mod ids that count as the right answer for the top suspect. Empty:
    /// no mod is to blame (advice only).
    #[serde(default)]
    expect: Vec<String>,
    /// The top suspect is at least this sure.
    #[serde(default)]
    at_least: Option<String>,
    /// A word the advice must have.
    #[serde(default)]
    advice: String,
    /// Enforced: a failure fails the test.
    #[serde(default)]
    pass: bool,
    /// Counted in the hit rate (false for crashes nobody can pin on a mod yet).
    #[serde(default = "yes")]
    scored: bool,
    #[serde(default)]
    note: String,
}

fn yes() -> bool {
    true
}

fn gunzip(p: &Path) -> String {
    let mut s = String::new();
    flate2::read::GzDecoder::new(fs::File::open(p).unwrap()).read_to_string(&mut s).unwrap();
    s
}

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("crash_corpus")
}

/// One row of a mod list: a jar and what Hopper reads from it.
struct Jar {
    file: String,
    ids: Vec<String>,
    name: String,
    version: String,
    deps: Vec<String>,
    namespaces: Vec<String>,
    packages: Vec<String>,
    mixins: Vec<String>,
}

fn mod_list(hash: &str) -> Vec<Jar> {
    let text = gunzip(&corpus().join("_mods").join(format!("{hash}.txt.gz")));
    let list = |s: &str| s.split(',').filter(|x| !x.is_empty()).map(str::to_string).collect::<Vec<_>>();
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            let g = |i: usize| c.get(i).copied().unwrap_or("");
            Jar {
                file: g(0).to_string(),
                ids: list(g(1)),
                name: g(2).to_string(),
                version: g(3).to_string(),
                deps: list(g(4)),
                namespaces: list(g(5)),
                packages: list(g(6)),
                mixins: list(g(7)),
            }
        })
        .collect()
}

/// A jar with the same metadata, folders, packages and mixin configs as
/// the real one (but no code).
fn fake_jar(dir: &Path, j: &Jar) {
    let mut z = zip::ZipWriter::new(fs::File::create(dir.join(&j.file)).unwrap());
    let opts = || zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let mut toml = String::from("modLoader=\"javafml\"\nloaderVersion=\"[1,)\"\nlicense=\"x\"\n");
    for id in &j.ids {
        let name = if j.name.is_empty() { id.as_str() } else { j.name.as_str() };
        toml.push_str(&format!("[[mods]]\nmodId=\"{id}\"\nversion=\"{}\"\ndisplayName=\"{}\"\n", j.version, name.replace('"', "'")));
    }
    if let Some(owner) = j.ids.first() {
        for d in &j.deps {
            toml.push_str(&format!("[[dependencies.{owner}]]\nmodId=\"{d}\"\ntype=\"required\"\nversionRange=\"[0,)\"\nside=\"BOTH\"\n"));
        }
    }
    for m in &j.mixins {
        toml.push_str(&format!("[[mixins]]\nconfig=\"{m}\"\n"));
    }
    z.start_file("META-INF/neoforge.mods.toml", opts()).unwrap();
    z.write_all(toml.as_bytes()).unwrap();
    for ns in &j.namespaces {
        z.start_file(format!("assets/{ns}/x.json"), opts()).unwrap();
    }
    for p in &j.packages {
        z.start_file(format!("{}/X.class", p.replace('.', "/")), opts()).unwrap();
        z.write_all(b"\xca\xfe\xba\xbe").unwrap();
    }
    for m in &j.mixins {
        z.start_file(m.as_str(), opts()).unwrap();
        z.write_all(b"{}").unwrap();
    }
    z.finish().unwrap();
}

/// What one case gave.
struct Row {
    name: String,
    case: Case,
    crash: Option<Crash>,
}

impl Row {
    fn top(&self) -> Option<&crashdetect::Suspect> {
        self.crash.as_ref()?.suspects.first()
    }

    fn hit(&self) -> bool {
        let floor = match self.case.at_least.as_deref() {
            Some("Certain") => Certainty::Certain,
            Some("Likely") => Certainty::Likely,
            _ => Certainty::Possible,
        };
        let advice_ok = self.case.advice.is_empty()
            || self.crash.as_ref().is_some_and(|c| c.advice.to_lowercase().contains(&self.case.advice.to_lowercase()));
        let top_ok = if self.case.expect.is_empty() {
            // Nothing to blame: no mod above "possible".
            self.top().is_none_or(|s| s.certainty == Certainty::Possible)
        } else {
            self.top().is_some_and(|s| self.case.expect.iter().any(|e| e.eq_ignore_ascii_case(&s.mod_id)) && s.certainty <= floor)
        };
        advice_ok && top_ok
    }

    fn top3(&self) -> bool {
        let Some(c) = &self.crash else { return self.case.expect.is_empty() };
        self.case.expect.is_empty()
            || c.suspects.iter().take(3).any(|s| self.case.expect.iter().any(|e| e.eq_ignore_ascii_case(&s.mod_id)))
    }
}

fn run(engine: bool) -> Vec<Row> {
    // The only test in this binary: nothing else reads the variable meanwhile.
    unsafe { std::env::set_var("HOPPER_CRASH_ENGINE", if engine { "on" } else { "off" }) };
    let base = std::env::temp_dir().join(format!("hopper-crash-corpus-{}-{engine}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let mut cases: Vec<(String, Case)> = fs::read_dir(corpus())
        .unwrap()
        .flatten()
        .filter(|e| e.path().join("case.toml").is_file())
        // CORPUS_CASE=<part of a name> runs only the matching cases.
        .filter(|e| std::env::var("CORPUS_CASE").map_or(true, |c| e.file_name().to_string_lossy().contains(&c)))
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let case: Case = toml::from_str(&fs::read_to_string(e.path().join("case.toml")).unwrap()).unwrap();
            (name, case)
        })
        .collect();
    cases.sort_by(|a, b| a.0.cmp(&b.0));
    // One instance folder per mod list: its jars are made once.
    let mut roots: BTreeMap<String, (PathBuf, Vec<String>)> = BTreeMap::new();
    let mut rows = Vec::new();
    for (name, case) in cases {
        let (root, mods) = roots
            .entry(case.mods.clone())
            .or_insert_with(|| {
                let root = base.join(&case.mods);
                let dir = root.join("mods");
                fs::create_dir_all(&dir).unwrap();
                let list = mod_list(&case.mods);
                for j in &list {
                    fake_jar(&dir, j);
                }
                (root, list.into_iter().map(|j| j.file).collect())
            })
            .clone();
        for d in ["crash-reports", "logs"] {
            let _ = fs::remove_dir_all(root.join(d));
            fs::create_dir_all(root.join(d)).unwrap();
        }
        let src = corpus().join(&name);
        let report = root.join("crash-reports").join(&case.report);
        fs::copy(src.join("report.txt"), &report).unwrap();
        for log in ["latest.log", "debug.log"] {
            let gz = src.join(format!("{log}.gz"));
            if gz.is_file() {
                fs::write(root.join("logs").join(log), gunzip(&gz)).unwrap();
            }
        }
        // A case whose log is lost: a server's run names its report in the log.
        if case.side == "server" && !root.join("logs/latest.log").is_file() {
            fs::write(root.join("logs/latest.log"), format!("[Server thread/ERROR]: This crash report has been saved to: crash-reports/{}
", case.report)).unwrap();
        }
        // The report and its logs were written in one run: same time, so the
        // "same run" checks hold whatever the copy kept.
        let now = std::time::SystemTime::now();
        for f in [report.clone(), root.join("logs/latest.log"), root.join("logs/debug.log")] {
            if let Ok(h) = fs::File::options().write(true).open(&f) {
                h.set_modified(now).unwrap();
            }
        }
        let dir = root.join("mods");
        let crash = if case.side == "server" {
            crashdetect::server(&root)
        } else {
            crashdetect::from_report(&root, &report, &mods, |f| hopper_core::deps::own_ids(&dir.join(f)).0)
        };
        rows.push(Row { name, case, crash });
    }
    let _ = fs::remove_dir_all(&base);
    rows
}

/// The table's cell for one result.
fn got(r: &Row) -> String {
    r.crash
        .as_ref()
        .map(|c| {
            let mut s: Vec<String> = c.suspects.iter().take(3).map(|s| format!("{} {}", s.mod_id, s.certainty.label())).collect();
            if !c.advice.is_empty() {
                s.push(format!("advice: {}", c.advice.chars().take(50).collect::<String>()));
            }
            s.join(" · ")
        })
        .unwrap_or_else(|| "(no crash found)".into())
}

/// Right mod first, and in the top three, over the distinct problems (a
/// problem counts by its first case).
fn rate(rows: &[Row]) -> (usize, usize, usize) {
    let mut problems: BTreeMap<u32, (bool, bool)> = BTreeMap::new();
    for r in rows.iter().filter(|r| r.case.scored && r.case.problem > 0) {
        problems.entry(r.case.problem).or_insert((r.hit(), r.top3()));
    }
    (problems.len(), problems.values().filter(|p| p.0).count(), problems.values().filter(|p| p.1).count())
}

#[test]
fn crash_corpus() {
    let before = run(false);
    let rows = run(true);
    let mark = |r: &Row| if !r.case.scored { "-" } else if r.hit() { "yes" } else { "NO" };
    let mut failed = Vec::new();
    println!("
{:<22} {:<3} {:<4} {:<4} {:<26} engine (top three) / older rules alone", "case", "#", "old", "new", "expected");
    for (r, b) in rows.iter().zip(&before) {
        let want = if r.case.expect.is_empty() { "(no mod)".to_string() } else { r.case.expect.join("|") };
        println!("{:<22} {:<3} {:<4} {:<4} {:<26} {}", r.name, r.case.problem, mark(b), mark(r), want, got(r));
        if got(b) != got(r) {
            println!("{:<63} was: {}", "", got(b));
        }
        if std::env::var("CORPUS_WHY").is_ok() {
            for (label, c) in [("old", &b.crash), ("new", &r.crash)] {
                let Some(c) = c else { continue };
                println!("  {label}: {}", c.headline);
                for s in &c.suspects {
                    println!("    {:?} {} [{}] {}", s.certainty, s.mod_id, s.file_name, s.why);
                    for f in &s.facts {
                        println!("        {:+} {} {}", f.weight, f.rule, f.text);
                    }
                }
            }
        }
        if r.case.pass && !r.hit() {
            failed.push(format!("{} ({})", r.name, r.case.note));
        }
        // No case the older rules got right may go wrong.
        if r.case.scored && b.hit() && !r.hit() {
            failed.push(format!("{}: right before the engine, wrong after", r.name));
        }
    }
    let (n, o1, o3) = rate(&before);
    let (_, e1, e3) = rate(&rows);
    println!("
distinct problems: {n}");
    println!("older rules alone: right mod first {o1}, in the top three {o3}");
    println!("with the engine:   right mod first {e1}, in the top three {e3}");
    assert!(failed.is_empty(), "{failed:#?}");
}
