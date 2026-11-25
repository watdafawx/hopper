//! Making a plan on disk (design §7). IO: staged fetches, a journal, then
//! the swap; one outcome per step so a failure never loses whose a change is.
//!
//! 1. Every file the plan needs is fetched into `.hopper-sync/stage/`. A
//!    failed fetch fails only its own step.
//! 2. A journal (`.hopper-sync/journal.json`) lists every move the swap will
//!    make, written atomically before the first one.
//! 3. The swap runs the moves, old jar to `.hopper-backups` first, and
//!    saves its progress in the journal after each. Then the journal and the
//!    stage are removed.
//!
//! A crash anywhere leaves either nothing changed (before the journal) or a
//! journal [`recover`] rolls forward on the next start, so a cut-short
//! install never leaves two copies of a mod. Nothing here makes a record:
//! every result keeps the dot of the friend whose change it is.

use super::model::{Dot, Key, Value};
use super::plan::{ApplyPlan, Step};
use crate::jobs::Job;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

/// Gets one file of a step into the staging folder.
pub trait Fetch {
    /// Downloads `file` (a `Value::File` id) to the path `into`. Fail with
    /// [`FetchError`] (inside the `anyhow::Error`) to say why: anything else
    /// is a plain failure.
    fn fetch(&self, file: &str, into: &Path) -> anyhow::Result<()>;

    /// The file's name on disk, without the ".disabled" a turned-off mod
    /// gets. The same file must always give the same name: the old jar is
    /// found by it.
    fn file_name(&self, key: &Key, file: &str) -> String {
        let _ = key;
        format!("{}.jar", safe(file))
    }

    /// A step that isn't a plain jar (a pack version, a loader, a pack's
    /// own mods): the app does it. Runs after the swap; its result is the
    /// step's.
    fn apply_other(&self, step: &Step) -> anyhow::Result<()> {
        let _ = step;
        Ok(())
    }
}

/// Why a fetch failed, when the app can say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchError {
    /// The author blocked downloads (CurseForge 403): by hand.
    Blocked,
    /// A rate limit or a site that is down: again later.
    Retry { after_secs: u64 },
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Blocked => write!(f, "the author blocks downloads"),
            FetchError::Retry { after_secs } => write!(f, "try again in {after_secs} s"),
        }
    }
}

impl std::error::Error for FetchError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Done,
    Failed(String),
    /// The author blocked downloads (CurseForge 403): by hand.
    Blocked,
    Retry { after_secs: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepResult {
    pub key: Key,
    pub by: Dot,
    pub outcome: Outcome,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApplyReport {
    pub results: Vec<StepResult>,
}

/// The file operations the swap uses, so tests can make any of them fail.
pub trait Fs {
    fn create_dir_all(&self, p: &Path) -> io::Result<()>;
    fn write(&self, p: &Path, bytes: &[u8]) -> io::Result<()>;
    fn read(&self, p: &Path) -> io::Result<Vec<u8>>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn remove_file(&self, p: &Path) -> io::Result<()>;
    fn remove_dir_all(&self, p: &Path) -> io::Result<()>;
    fn exists(&self, p: &Path) -> bool;
}

/// The real disk.
pub struct RealFs;

impl Fs for RealFs {
    fn create_dir_all(&self, p: &Path) -> io::Result<()> {
        std::fs::create_dir_all(p)
    }
    fn write(&self, p: &Path, bytes: &[u8]) -> io::Result<()> {
        std::fs::write(p, bytes)
    }
    fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(p)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }
    fn remove_file(&self, p: &Path) -> io::Result<()> {
        std::fs::remove_file(p)
    }
    fn remove_dir_all(&self, p: &Path) -> io::Result<()> {
        match std::fs::remove_dir_all(p) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            r => r,
        }
    }
    fn exists(&self, p: &Path) -> bool {
        p.exists()
    }
}

/// The folder a key's file lives in, under the instance.
pub fn folder_of(key: &Key) -> &'static str {
    let Key::Mod(k) = key else { return "mods" };
    match k.split(':').next() {
        Some("shader") => "shaderpacks",
        Some("rp") => "resourcepacks",
        Some("dp") => "datapacks",
        _ => "mods",
    }
}

fn safe(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).collect()
}

/// The name an old jar is set aside under in `.hopper-backups` by a swap:
/// the author's dot is part of it, so two friends' swaps never collide.
pub fn set_aside_name(file: &str, by: &Dot) -> String {
    format!("{file}.{}-{}", safe(&by.author), by.n)
}

fn sync_dir(root: &Path) -> PathBuf {
    root.join(".hopper-sync")
}
fn stage_dir(root: &Path) -> PathBuf {
    sync_dir(root).join("stage")
}
fn journal_path(root: &Path) -> PathBuf {
    sync_dir(root).join("journal.json")
}
fn journal_tmp(root: &Path) -> PathBuf {
    sync_dir(root).join("journal.json.tmp")
}

/// One move of the swap, paths relative to the instance. A move that was
/// already made (its source gone, its target there) is skipped, so a
/// replay after a crash never moves anything twice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Move {
    from: String,
    to: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum State {
    Pending,
    Done,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    key: Key,
    by: Dot,
    moves: Vec<Move>,
    /// How many moves are made.
    made: usize,
    state: State,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Journal {
    version: u8,
    entries: Vec<Entry>,
}

pub fn run(root: &Path, plan: &ApplyPlan, fetch: &dyn Fetch, job: &Job) -> ApplyReport {
    run_with(&RealFs, root, plan, fetch, job)
}

/// [`run`] on any [`Fs`].
pub fn run_with(fs: &dyn Fs, root: &Path, plan: &ApplyPlan, fetch: &dyn Fetch, job: &Job) -> ApplyReport {
    let total = plan.steps.len();
    let mut results: Vec<Option<Outcome>> = vec![None; total];
    let mut entries: Vec<(usize, Entry)> = Vec::new();

    // 1. Stage every file first; a failed fetch fails only its own step.
    let stage = stage_dir(root);
    for (i, step) in plan.steps.iter().enumerate() {
        job.progress(i, total);
        let Some(shape) = file_shape(step) else { continue };
        if job.is_cancelled() {
            results[i] = Some(Outcome::Failed("Cancelled".into()));
            continue;
        }
        let mut staged = None;
        if let Some(file) = shape.fetch {
            let name = format!("{i}-{}", fetch.file_name(&step.key, file));
            let path = stage.join(&name);
            if let Err(e) = fs.create_dir_all(&stage) {
                results[i] = Some(Outcome::Failed(format!("could not prepare the download folder: {e}")));
                continue;
            }
            match fetch.fetch(file, &path) {
                Err(e) => {
                    results[i] = Some(outcome_of(&e));
                    continue;
                }
                Ok(()) if !fs.exists(&path) => {
                    results[i] = Some(Outcome::Failed("the download left no file".into()));
                    continue;
                }
                Ok(()) => staged = Some(format!(".hopper-sync/stage/{name}")),
            }
        }
        let moves = moves_of(step, shape, staged, fetch);
        entries.push((i, Entry { key: step.key.clone(), by: step.by.clone(), moves, made: 0, state: State::Pending }));
    }

    // 2. The journal, then 3. the swap.
    if !entries.is_empty() {
        let mut journal = Journal { version: 1, entries: entries.iter().map(|(_, e)| e.clone()).collect() };
        let started = fs.create_dir_all(&sync_dir(root)).and_then(|_| save(fs, root, &journal));
        match started {
            Err(e) => {
                for (i, _) in &entries {
                    results[*i] = Some(Outcome::Failed(format!("could not start the swap: {e}")));
                }
                let _ = fs.remove_dir_all(&stage);
            }
            Ok(()) => {
                let mut dead = None;
                for (n, (i, _)) in entries.iter().enumerate() {
                    if let Some(e) = &dead {
                        results[*i] = Some(Outcome::Failed(format!("the swap was cut short: {e}")));
                        continue;
                    }
                    match swap(fs, root, &mut journal, n) {
                        Ok(()) => results[*i] = Some(entry_outcome(&journal.entries[n])),
                        Err(e) => {
                            // The journal can't be saved: leave it for `recover`.
                            results[*i] = Some(Outcome::Failed(format!("the swap was cut short: {e}")));
                            dead = Some(e);
                        }
                    }
                }
                if dead.is_none() {
                    finish(fs, root);
                }
            }
        }
    } else if fs.exists(&stage) {
        let _ = fs.remove_dir_all(&stage);
    }

    // The app's own steps (a pack version, a loader, a pack's mods).
    for (i, step) in plan.steps.iter().enumerate() {
        if results[i].is_none() {
            results[i] = Some(match fetch.apply_other(step) {
                Ok(()) => Outcome::Done,
                Err(e) => outcome_of(&e),
            });
        }
    }
    job.progress(total, total);
    ApplyReport {
        results: plan
            .steps
            .iter()
            .zip(results)
            .map(|(s, o)| StepResult {
                key: s.key.clone(),
                by: s.by.clone(),
                outcome: o.unwrap_or(Outcome::Failed("not done".into())),
            })
            .collect(),
    }
}

/// Finishes a swap a crash cut short: rolls the journal forward, then
/// clears it and the stage. `None`: nothing was left.
pub fn recover(root: &Path) -> anyhow::Result<Option<ApplyReport>> {
    recover_with(&RealFs, root)
}

/// [`recover`] on any [`Fs`].
pub fn recover_with(fs: &dyn Fs, root: &Path) -> anyhow::Result<Option<ApplyReport>> {
    let (path, stage) = (journal_path(root), stage_dir(root));
    if !fs.exists(&path) {
        // A crash before the journal: nothing was swapped; the stage is junk.
        fs.remove_dir_all(&stage)?;
        if fs.exists(&journal_tmp(root)) {
            fs.remove_file(&journal_tmp(root))?;
        }
        return Ok(None);
    }
    let bytes = fs.read(&path)?;
    let Ok(mut journal) = serde_json::from_slice::<Journal>(&bytes) else {
        fs.remove_file(&path)?;
        fs.remove_dir_all(&stage)?;
        anyhow::bail!("the swap journal was unreadable and was dropped");
    };
    for n in 0..journal.entries.len() {
        if journal.entries[n].state == State::Pending {
            swap(fs, root, &mut journal, n)?;
        }
    }
    let report = ApplyReport {
        results: journal
            .entries
            .iter()
            .map(|e| StepResult { key: e.key.clone(), by: e.by.clone(), outcome: entry_outcome(e) })
            .collect(),
    };
    finish(fs, root);
    Ok(Some(report))
}

/// Removes the journal and the stage once every entry is settled.
fn finish(fs: &dyn Fs, root: &Path) {
    if fs.remove_file(&journal_path(root)).is_ok() || !fs.exists(&journal_path(root)) {
        let _ = fs.remove_dir_all(&stage_dir(root));
    }
}

fn entry_outcome(e: &Entry) -> Outcome {
    match &e.state {
        State::Done => Outcome::Done,
        State::Failed(m) => Outcome::Failed(m.clone()),
        State::Pending => Outcome::Failed("not done".into()),
    }
}

fn outcome_of(e: &anyhow::Error) -> Outcome {
    match e.downcast_ref::<FetchError>() {
        Some(FetchError::Blocked) => Outcome::Blocked,
        Some(FetchError::Retry { after_secs }) => Outcome::Retry { after_secs: *after_secs },
        None => Outcome::Failed(format!("{e:#}")),
    }
}

fn save(fs: &dyn Fs, root: &Path, journal: &Journal) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(journal).map_err(io::Error::other)?;
    fs.write(&journal_tmp(root), &bytes)?;
    fs.rename(&journal_tmp(root), &journal_path(root))
}

/// Runs the moves of entry `n` that aren't made yet. A move that fails
/// puts this entry's made moves back and marks it failed; the other
/// entries go on. `Err` only when the journal itself can't be saved.
fn swap(fs: &dyn Fs, root: &Path, journal: &mut Journal, n: usize) -> io::Result<()> {
    while journal.entries[n].made < journal.entries[n].moves.len() {
        let mv = journal.entries[n].moves[journal.entries[n].made].clone();
        if let Err(msg) = make_move(fs, root, &mv) {
            while journal.entries[n].made > 0 {
                let back = journal.entries[n].moves[journal.entries[n].made - 1].clone();
                if undo_move(fs, root, &back).is_err() {
                    break;
                }
                journal.entries[n].made -= 1;
            }
            journal.entries[n].state = State::Failed(msg);
            return save(fs, root, journal);
        }
        journal.entries[n].made += 1;
        save(fs, root, journal)?;
    }
    journal.entries[n].state = State::Done;
    save(fs, root, journal)
}

fn make_move(fs: &dyn Fs, root: &Path, mv: &Move) -> Result<(), String> {
    let (from, to) = (root.join(&mv.from), root.join(&mv.to));
    if fs.exists(&from) {
        if let Some(dir) = to.parent() {
            fs.create_dir_all(dir).map_err(|e| format!("{}: {e}", mv.to))?;
        }
        fs.rename(&from, &to).map_err(|e| format!("{}: {e}", mv.to))
    } else if fs.exists(&to) {
        Ok(())
    } else {
        Err(format!("{} is gone", mv.from))
    }
}

fn undo_move(fs: &dyn Fs, root: &Path, mv: &Move) -> io::Result<()> {
    let (from, to) = (root.join(&mv.from), root.join(&mv.to));
    if fs.exists(&to) && !fs.exists(&from) {
        if let Some(dir) = from.parent() {
            fs.create_dir_all(dir)?;
        }
        fs.rename(&to, &from)?;
    }
    Ok(())
}

/// What a step does to files: the file to fetch (if any) and the old and
/// new file on disk. `None`: not a jar step.
struct Shape<'a> {
    fetch: Option<&'a str>,
    old: Option<(&'a str, bool)>,
    new: Option<(&'a str, bool)>,
}

/// `None`: not a file; `Some(None)`: no file; `Some(Some(..))`: a file.
fn side(v: &Value) -> Option<Option<(&str, bool)>> {
    match v {
        Value::Gone => Some(None),
        Value::File { file, on } => Some(Some((file.as_str(), *on))),
        _ => None,
    }
}

fn file_shape(step: &Step) -> Option<Shape<'_>> {
    if !matches!(step.key, Key::Mod(_)) {
        return None;
    }
    let (old, new) = (side(&step.from)?, side(&step.to)?);
    let fetch = new.filter(|(f, _)| old.is_none_or(|(o, _)| o != *f)).map(|(f, _)| f);
    Some(Shape { fetch, old, new })
}

fn moves_of(step: &Step, shape: Shape<'_>, staged: Option<String>, fetch: &dyn Fetch) -> Vec<Move> {
    let folder = folder_of(&step.key);
    let path = |file: &str, on: bool| {
        let name = fetch.file_name(&step.key, file);
        format!("{folder}/{name}{}", if on { "" } else { ".disabled" })
    };
    let mut moves = Vec::new();
    match (shape.old, shape.new) {
        // Same file, turned on or off.
        (Some((a, oa)), Some((b, ob))) if a == b => {
            if oa != ob {
                moves.push(Move { from: path(a, oa), to: path(b, ob) });
            }
        }
        (old, new) => {
            if let Some((a, oa)) = old {
                let from = path(a, oa);
                let name = from.rsplit('/').next().unwrap_or(&from).to_string();
                moves.push(Move { from, to: format!(".hopper-backups/{}", set_aside_name(&name, &step.by)) });
            }
            if let (Some((b, ob)), Some(from)) = (new, staged) {
                moves.push(Move { from, to: path(b, ob) });
            }
        }
    }
    moves
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::collections::BTreeMap;

    /// A scratch folder, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            let p = std::env::temp_dir().join(format!("hopper-apply-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(p.join("mods")).unwrap();
            Scratch(p)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The real disk until `left` mutating calls are used, then every one
    /// fails: a power cut.
    struct Cut {
        left: Cell<usize>,
    }

    impl Cut {
        fn after(n: usize) -> Cut {
            Cut { left: Cell::new(n) }
        }
        fn tick(&self) -> io::Result<()> {
            if self.left.get() == 0 {
                return Err(io::Error::other("power cut"));
            }
            self.left.set(self.left.get() - 1);
            Ok(())
        }
    }

    impl Fs for Cut {
        fn create_dir_all(&self, p: &Path) -> io::Result<()> {
            self.tick()?;
            RealFs.create_dir_all(p)
        }
        fn write(&self, p: &Path, b: &[u8]) -> io::Result<()> {
            self.tick()?;
            RealFs.write(p, b)
        }
        fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
            RealFs.read(p)
        }
        fn rename(&self, a: &Path, b: &Path) -> io::Result<()> {
            self.tick()?;
            RealFs.rename(a, b)
        }
        fn remove_file(&self, p: &Path) -> io::Result<()> {
            self.tick()?;
            RealFs.remove_file(p)
        }
        fn remove_dir_all(&self, p: &Path) -> io::Result<()> {
            self.tick()?;
            RealFs.remove_dir_all(p)
        }
        fn exists(&self, p: &Path) -> bool {
            RealFs.exists(p)
        }
    }

    /// Fetches "contents of <file>"; some files fail.
    #[derive(Default)]
    struct Shop {
        fail: BTreeMap<String, &'static str>,
    }

    impl Fetch for Shop {
        fn fetch(&self, file: &str, into: &Path) -> anyhow::Result<()> {
            match self.fail.get(file) {
                Some(&"blocked") => Err(FetchError::Blocked.into()),
                Some(&"retry") => Err(FetchError::Retry { after_secs: 300 }.into()),
                Some(m) => anyhow::bail!("{m}"),
                None => Ok(std::fs::write(into, format!("contents of {file}"))?),
            }
        }
    }

    fn job() -> Job {
        crate::jobs::Jobs::new(crate::waker::noop()).start_quiet("test")
    }

    fn dot(author: &str, n: u64) -> Dot {
        Dot { author: author.into(), n }
    }

    fn on(file: &str) -> Value {
        Value::File { file: file.into(), on: true }
    }

    fn step(n: u64, from: Value, to: Value, by: &str) -> Step {
        Step {
            key: Key::Mod(format!("cf:{n}")),
            from,
            to,
            by: dot(by, n),
            name: by.into(),
            why: Default::default(),
        }
    }

    fn put(s: &Scratch, file: &str) {
        std::fs::write(s.0.join("mods").join(format!("{file}.jar")), "old").unwrap();
    }

    fn mods(s: &Scratch) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(s.0.join("mods"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn three_updates(s: &Scratch) -> ApplyPlan {
        for n in 1..=3 {
            put(s, &format!("cf_{n}1"));
        }
        ApplyPlan {
            steps: (1..=3).map(|n| step(n, on(&format!("cf:{n}1")), on(&format!("cf:{n}2")), "A")).collect(),
            waits: Vec::new(),
        }
    }

    fn new_names() -> Vec<String> {
        (1..=3).map(|n| format!("cf_{n}2.jar")).collect()
    }

    fn left_over(s: &Scratch) -> bool {
        s.0.join(".hopper-sync/journal.json").exists() || s.0.join(".hopper-sync/stage").exists()
    }

    #[test]
    fn a_plan_lands_every_step_named_for_its_author() {
        let s = Scratch::new("lands");
        let plan = three_updates(&s);
        let r = run(&s.0, &plan, &Shop::default(), &job());
        assert!(r.results.iter().all(|x| x.outcome == Outcome::Done && x.by.author == "A"));
        assert_eq!(mods(&s), new_names());
        assert!(!left_over(&s));
        // The old jars were set aside, not deleted.
        assert!(s.0.join(".hopper-backups/cf_11.jar.A-1").exists());
    }

    #[test]
    fn a_failed_download_fails_only_its_own_step() {
        let s = Scratch::new("failed");
        let plan = three_updates(&s);
        let mut shop = Shop::default();
        shop.fail.insert("cf:22".into(), "connection reset");
        shop.fail.insert("cf:12".into(), "blocked");
        let r = run(&s.0, &plan, &shop, &job());
        assert_eq!(r.results[0].outcome, Outcome::Blocked);
        assert!(matches!(&r.results[1].outcome, Outcome::Failed(m) if m.contains("connection reset")));
        assert_eq!(r.results[2].outcome, Outcome::Done);
        // The failed ones keep their old jar and their author.
        assert_eq!(mods(&s), vec!["cf_11.jar", "cf_21.jar", "cf_32.jar"]);
        assert_eq!(r.results[1].by, dot("A", 2));
        shop.fail.clear();
        shop.fail.insert("cf:22".into(), "retry");
        let r = run(&s.0, &ApplyPlan { steps: vec![plan.steps[1].clone()], waits: vec![] }, &shop, &job());
        assert_eq!(r.results[0].outcome, Outcome::Retry { after_secs: 300 });
    }

    #[test]
    fn turning_off_and_removing_and_adding() {
        let s = Scratch::new("kinds");
        put(&s, "cf_11");
        put(&s, "cf_21");
        let plan = ApplyPlan {
            steps: vec![
                step(1, on("cf:11"), Value::File { file: "cf:11".into(), on: false }, "B"),
                step(2, on("cf:21"), Value::Gone, "B"),
                step(3, Value::Gone, on("cf:31"), "B"),
            ],
            waits: vec![],
        };
        let r = run(&s.0, &plan, &Shop::default(), &job());
        assert!(r.results.iter().all(|x| x.outcome == Outcome::Done));
        assert_eq!(mods(&s), vec!["cf_11.jar.disabled", "cf_31.jar"]);
        // And turned back on.
        let back = ApplyPlan {
            steps: vec![step(1, Value::File { file: "cf:11".into(), on: false }, on("cf:11"), "B")],
            waits: vec![],
        };
        run(&s.0, &back, &Shop::default(), &job());
        assert_eq!(mods(&s), vec!["cf_11.jar", "cf_31.jar"]);
    }

    #[test]
    fn a_move_that_cannot_be_made_puts_that_step_back() {
        let s = Scratch::new("rollback");
        let mut plan = three_updates(&s);
        // Step 2's old jar is gone from disk (deleted in Explorer): its
        // backup move fails; the others land.
        std::fs::remove_file(s.0.join("mods/cf_21.jar")).unwrap();
        plan.steps[1].from = on("cf:21");
        let r = run(&s.0, &plan, &Shop::default(), &job());
        assert_eq!(r.results[0].outcome, Outcome::Done);
        assert!(matches!(r.results[1].outcome, Outcome::Failed(_)));
        assert_eq!(r.results[2].outcome, Outcome::Done);
        assert_eq!(mods(&s), vec!["cf_12.jar", "cf_32.jar"]);
        assert!(!left_over(&s));
    }

    /// The power goes after every possible number of file operations. Until
    /// the journal is saved nothing has changed; after it, `recover` lands
    /// everything. Never a mix, never two copies of a mod.
    #[test]
    fn a_crash_at_every_step_is_finished_on_the_next_start() {
        let mut cuts = 0;
        for budget in 0..80 {
            let s = Scratch::new(&format!("crash{budget}"));
            let plan = three_updates(&s);
            let fs = Cut::after(budget);
            let r = run_with(&fs, &s.0, &plan, &Shop::default(), &job());
            let clean = r.results.iter().all(|x| x.outcome == Outcome::Done);
            // The next start.
            let rec = recover(&s.0).unwrap_or_else(|e| panic!("budget {budget}: {e}"));
            let now = mods(&s);
            let old = vec!["cf_11.jar", "cf_21.jar", "cf_31.jar"];
            let journal_written = rec.is_some();
            // All done and nothing left to recover: the cut came too late to matter.
            if clean && rec.is_none() {
                assert_eq!(now, new_names(), "budget {budget}");
                assert!(!left_over(&s));
                break;
            }
            cuts += 1;
            if journal_written {
                assert_eq!(now, new_names(), "budget {budget}: journal rolled forward");
                let rep = rec.unwrap();
                assert!(rep.results.iter().all(|x| x.outcome == Outcome::Done && x.by.author == "A"));
                assert_eq!(rep.results.len(), 3);
            } else {
                assert_eq!(now, old, "budget {budget}: nothing changed before the journal");
            }
            assert!(!left_over(&s), "budget {budget}: journal and stage cleared");
            // One file per mod, whichever side of the cut.
            for n in 1..=3 {
                let own = now.iter().filter(|f| f.starts_with(&format!("cf_{n}"))).count();
                assert_eq!(own, 1, "budget {budget}: mod {n}");
            }
        }
        assert!(cuts > 10, "the loop covered the journal's steps ({cuts})");
    }

    #[test]
    fn recover_with_nothing_left_does_nothing() {
        let s = Scratch::new("none");
        assert!(recover(&s.0).unwrap().is_none());
    }

    #[test]
    fn an_unreadable_journal_is_dropped_with_a_note() {
        let s = Scratch::new("junk");
        std::fs::create_dir_all(s.0.join(".hopper-sync/stage")).unwrap();
        std::fs::write(s.0.join(".hopper-sync/journal.json"), "{ not json").unwrap();
        assert!(recover(&s.0).is_err());
        assert!(!left_over(&s));
        assert!(recover(&s.0).unwrap().is_none());
    }

    #[test]
    fn a_replayed_journal_is_the_same_as_the_first_run() {
        // A journal saved after its moves all ran but before it was removed.
        let s = Scratch::new("replay");
        let plan = three_updates(&s);
        let fs = Cut::after(1_000);
        run_with(&fs, &s.0, &plan, &Shop::default(), &job());
        let after = mods(&s);
        // Put a done journal back by hand and recover: nothing moves twice.
        let j = Journal {
            version: 1,
            entries: vec![Entry {
                key: Key::Mod("cf:1".into()),
                by: dot("A", 1),
                moves: vec![Move { from: "mods/cf_11.jar".into(), to: ".hopper-backups/cf_11.jar.A-1".into() }],
                made: 0,
                state: State::Pending,
            }],
        };
        std::fs::create_dir_all(s.0.join(".hopper-sync")).unwrap();
        std::fs::write(s.0.join(".hopper-sync/journal.json"), serde_json::to_vec(&j).unwrap()).unwrap();
        let rep = recover(&s.0).unwrap().unwrap();
        assert_eq!(rep.results[0].outcome, Outcome::Done);
        assert_eq!(mods(&s), after);
    }
}
