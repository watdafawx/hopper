//! A game or server that stops making progress while it starts.
//!
//! A start that hangs never crashes, so there is no crash report: the
//! window sits on "Loading mods" (or the server never says "Done (") and
//! nothing tells the player why. This watches the log and the process
//! while it starts and calls it stuck when both go quiet: no new log line
//! for a while ([`StallSettings::secs`], 90 s by default) and almost no
//! CPU over the same time. Quiet log with busy CPU is slow work (a big
//! pack building its caches), not a hang.
//!
//! "No new log line" ignores lines that repeat one already seen lately:
//! a hung NeoForge client keeps drawing its loading screen, and its
//! OpenGL driver warns about the same texture thousands of times while
//! nothing else moves (TNP Limitless 8, 2026-11-05: 49k identical lines
//! over 16 minutes of a deadlock).
//!
//! When it fires, Hopper takes a thread dump with the instance's own
//! `jcmd` (or `jstack`), which doesn't stop the game, and reads it for
//! threads waiting on each other. A deadlock while mods load is most often
//! two classes whose static set-up needs the other, run on two mod-loading
//! threads at once; the JVM doesn't report those as a deadlock (the threads
//! show as RUNNABLE "waiting on the Class initialization monitor"), so the
//! cycle is found here. The mod whose classes form the cycle is the likely
//! cause; a thread that only waits behind it is a victim and is named as
//! such, never blamed.

use crate::crashdetect::{Certainty, Suspect};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Below this much CPU (in whole cores, averaged over the quiet time) a
/// quiet start counts as stuck. A hung client still draws its loading
/// screen (~0.1 core); one thread doing real work is ~1 core.
pub const IDLE_CORES: f32 = 0.3;

/// How long a quiet log counts as stuck, by default.
pub const DEFAULT_SECS: u64 = 90;

/// How many recent distinct lines count as "seen lately".
const RECENT: usize = 64;

/// Prefix of the thread dumps saved in `crash-reports`.
pub const DUMP_PREFIX: &str = "stuck-";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StallSettings {
    /// Seconds without progress before a start counts as stuck (0: off).
    pub secs: u64,
}

impl Default for StallSettings {
    fn default() -> Self {
        Self { secs: DEFAULT_SECS }
    }
}

/// What the start was doing when it went quiet, for the notice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Starting,
    LoadingMods,
    LoadingResources,
}

impl Phase {
    /// Ends "looks stuck …".
    pub fn words(self) -> &'static str {
        match self {
            Phase::Starting => "while starting",
            Phase::LoadingMods => "while loading mods",
            Phase::LoadingResources => "while loading resources",
        }
    }
}

/// Log progress of one start, fed with new log text as it arrives.
#[derive(Debug)]
pub struct Progress {
    server: bool,
    loaded: bool,
    last_progress: i64,
    recent: VecDeque<u64>,
    recent_set: HashSet<u64>,
    phase: Phase,
    partial: String,
    /// The last line that counted as progress (message only).
    pub last_line: String,
}

/// The message part of a log line: after the `[time] [thread/LEVEL] [logger]: `
/// prefix, so the same message at another time is the same line.
fn message(line: &str) -> &str {
    match line.find("]: ") {
        Some(i) if line.starts_with('[') => &line[i + 3..],
        _ => line,
    }
}

fn hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

impl Progress {
    pub fn new(server: bool, now: i64) -> Self {
        Self {
            server,
            loaded: false,
            last_progress: now,
            recent: VecDeque::new(),
            recent_set: HashSet::new(),
            phase: Phase::Starting,
            partial: String::new(),
            last_line: String::new(),
        }
    }

    /// Whether the start finished (the title screen, or the server's
    /// "Done ("): nothing to watch after that.
    pub fn loaded(&self) -> bool {
        self.loaded
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Seconds since the last line that was progress.
    pub fn quiet_for(&self, now: i64) -> i64 {
        (now - self.last_progress).max(0)
    }

    /// New log text (any chunk; a line cut in two is joined).
    pub fn feed(&mut self, text: &str, now: i64) {
        self.partial.push_str(text);
        let Some(cut) = self.partial.rfind('\n') else { return };
        let done: String = self.partial.drain(..=cut).collect();
        for line in done.lines() {
            self.line(line.trim_end(), now);
        }
    }

    fn line(&mut self, line: &str, now: i64) {
        if line.is_empty() {
            return;
        }
        if self.server {
            if line.contains("]: Done (") || line.starts_with("Done (") {
                self.loaded = true;
            }
        } else if line.contains("Sound engine started") {
            self.loaded = true;
        }
        let msg = message(line);
        let h = hash(msg);
        if self.recent_set.contains(&h) {
            return; // a repeat: the loading screen talking, not progress
        }
        self.recent.push_back(h);
        self.recent_set.insert(h);
        if self.recent.len() > RECENT
            && let Some(old) = self.recent.pop_front()
        {
            self.recent_set.remove(&old);
        }
        self.last_progress = now;
        self.last_line = msg.chars().take(200).collect();
        if line.contains("[modloading-worker") {
            self.phase = Phase::LoadingMods;
        } else if line.contains("Reloading ResourceManager") || line.contains("Created: ") {
            self.phase = Phase::LoadingResources;
        }
    }
}

/// CPU samples of the process: (when, total CPU time used in ms).
#[derive(Debug, Default)]
pub struct CpuSamples(VecDeque<(i64, u64)>);

impl CpuSamples {
    pub fn push(&mut self, now: i64, cpu_ms: u64) {
        self.0.push_back((now, cpu_ms));
        // Keep about ten minutes.
        while self.0.front().is_some_and(|(t, _)| now - t > 600) {
            self.0.pop_front();
        }
    }

    /// Cores used on average over the last `secs` seconds, when the
    /// samples cover at least most of that time.
    pub fn cores_over(&self, now: i64, secs: i64) -> Option<f32> {
        let (t1, c1) = *self.0.back()?;
        let (t0, c0) = *self.0.iter().find(|(t, _)| now - t <= secs)?;
        let span = t1 - t0;
        if span < secs * 2 / 3 || span <= 0 {
            return None;
        }
        Some(c1.saturating_sub(c0) as f32 / 1000.0 / span as f32)
    }
}

/// Total CPU time the process has used, in ms.
pub fn cpu_time_ms(pid: u32) -> Option<u64> {
    let mut sys = sysinfo::System::new();
    let p = sysinfo::Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[p]),
        true,
        sysinfo::ProcessRefreshKind::nothing().with_cpu(),
    );
    Some(sys.process(p)?.accumulated_cpu_time())
}

/// One start being watched: its log progress and CPU.
#[derive(Debug)]
pub struct Watch {
    pub progress: Progress,
    cpu: CpuSamples,
    log: PathBuf,
    log_read: u64,
    /// No notice before this time (the player chose to wait).
    snoozed_until: i64,
    /// Already told for this quiet spell.
    fired: bool,
}

/// A start that looks stuck.
#[derive(Clone, Debug, PartialEq)]
pub struct Stall {
    pub quiet_secs: i64,
    /// Cores used over the quiet time.
    pub cores: f32,
    pub phase: Phase,
    pub last_line: String,
}

impl Stall {
    /// "no progress for 2 min, CPU idle"
    pub fn words(&self) -> String {
        let m = self.quiet_secs / 60;
        let time = if m >= 1 { format!("{m} min") } else { format!("{} s", self.quiet_secs) };
        format!("no progress for {time}, CPU idle")
    }
}

impl Watch {
    /// Watches `log` (read from its current end when `from_end`, so an old
    /// log isn't taken for this start's).
    pub fn new(log: &Path, server: bool, now: i64, from_end: bool) -> Self {
        let log_read = if from_end { std::fs::metadata(log).map_or(0, |m| m.len()) } else { 0 };
        Self {
            progress: Progress::new(server, now),
            cpu: CpuSamples::default(),
            log: log.to_path_buf(),
            log_read,
            snoozed_until: 0,
            fired: false,
        }
    }

    /// Reads what the log gained since the last look.
    pub fn read_log(&mut self, now: i64) {
        let len = std::fs::metadata(&self.log).map_or(0, |m| m.len());
        if len < self.log_read {
            self.log_read = 0; // a new log
        }
        if len > self.log_read {
            let take = (len - self.log_read).min(8 << 20);
            if let Some(text) = read_range(&self.log, self.log_read, take) {
                self.progress.feed(&text, now);
            }
            self.log_read += take;
        }
    }

    /// Feeds console lines directly (servers Hopper runs itself).
    pub fn feed_line(&mut self, line: &str, now: i64) {
        self.progress.feed(line, now);
        self.progress.feed("\n", now);
    }

    /// The player chose to wait: ask again after `secs` more quiet seconds.
    pub fn snooze(&mut self, now: i64, secs: i64) {
        self.snoozed_until = now + secs;
        self.fired = false;
    }

    /// One look: CPU used so far (`cpu_ms`, from [`cpu_time_ms`]) and
    /// whether the start now counts as stuck. Fires once per quiet spell.
    pub fn check(&mut self, now: i64, cpu_ms: Option<u64>, set: StallSettings) -> Option<Stall> {
        if let Some(c) = cpu_ms {
            self.cpu.push(now, c);
        }
        if set.secs == 0 || self.progress.loaded() {
            return None;
        }
        let quiet = self.progress.quiet_for(now);
        if quiet < set.secs as i64 {
            self.fired = false;
            return None;
        }
        if self.fired || now < self.snoozed_until {
            return None;
        }
        let cores = self.cpu.cores_over(now, set.secs as i64)?;
        if cores >= IDLE_CORES {
            return None; // quiet but working
        }
        self.fired = true;
        Some(Stall { quiet_secs: quiet, cores, phase: self.progress.phase(), last_line: self.progress.last_line.clone() })
    }
}

fn read_range(p: &Path, from: u64, len: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(p).ok()?;
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut b = Vec::with_capacity(len as usize);
    f.take(len).read_to_end(&mut b).ok()?;
    Some(String::from_utf8_lossy(&b).into_owned())
}

// ---------------------------------------------------------------------
// Taking the dump

/// Why no thread dump could be taken, in words for the player.
#[derive(Debug)]
pub struct NoDump(pub String);

/// The JDK tool next to `java` (`jcmd`, `jstack`), if this Java has it.
fn tool_beside(java: &Path, tool: &str) -> Option<PathBuf> {
    let dir = java.parent()?;
    let exe = if cfg!(windows) { format!("{tool}.exe") } else { tool.to_string() };
    let p = dir.join(exe);
    p.is_file().then_some(p)
}

/// A thread dump of the running JVM `pid`, from the `jcmd` (else `jstack`)
/// of the Java that runs it (`java`: its java/javaw). Neither stops the
/// game. A plain JRE has neither, and the attach API can't be reached
/// from outside Java, so then there's no dump and the reason says so.
pub fn take_dump(pid: u32, java: &Path) -> Result<String, NoDump> {
    let pid = pid.to_string();
    let tries: [(&str, Vec<&str>); 2] = [("jcmd", vec![&pid, "Thread.print", "-l"]), ("jstack", vec!["-l", &pid])];
    let mut tried = Vec::new();
    for (tool, args) in tries {
        let Some(exe) = tool_beside(java, tool) else { continue };
        tried.push(tool);
        let mut cmd = crate::proc::command(&exe);
        cmd.args(&args);
        cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        let Ok(child) = cmd.spawn() else { continue };
        if let Some(out) = wait_output(child, Duration::from_secs(30))
            && out.contains("Full thread dump")
        {
            return Ok(out);
        }
    }
    if tried.is_empty() {
        Err(NoDump(format!(
            "This Java ({}) has no jcmd or jstack, so Hopper can't see what the game is waiting on. Use a full JDK for this instance to get that.",
            java.parent().map_or_else(|| java.display().to_string(), |d| d.display().to_string())
        )))
    } else {
        Err(NoDump(format!("{} didn't answer, so Hopper couldn't see what the game is waiting on.", tried.join(" and "))))
    }
}

fn wait_output(mut child: std::process::Child, limit: Duration) -> Option<String> {
    use std::io::Read;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b);
        b
    });
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let b = reader.join().ok()?;
    Some(String::from_utf8_lossy(&b).into_owned())
}

// ---------------------------------------------------------------------
// Reading the dump

/// One thread of a dump.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Thread {
    pub name: String,
    /// `#187` in the header (unique where names repeat).
    pub id: String,
    pub state: String,
    /// Frames, top first: `pkg.Class.method` plus the module (`modid@1.0`)
    /// when the frame names one.
    pub frames: Vec<Frame>,
    /// `- waiting to lock <0x…> (a com.x.Y)`: (address, class).
    pub waiting_lock: Option<(String, String)>,
    /// `- locked <0x…>`.
    pub locked: Vec<String>,
    /// `- waiting on the Class initialization monitor for com.x.Y`.
    pub waiting_init: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frame {
    pub class: String,
    pub method: String,
    pub module: String,
}

impl Thread {
    /// Classes whose static set-up this thread is running (their
    /// `<clinit>` is on its stack), so other threads wait for it.
    pub fn initializing(&self) -> Vec<&str> {
        self.frames.iter().filter(|f| f.method == "<clinit>").map(|f| f.class.as_str()).collect()
    }

    fn label(&self) -> String {
        if self.id.is_empty() { self.name.clone() } else { format!("{} {}", self.name, self.id) }
    }
}

fn parse_frame(s: &str) -> Option<Frame> {
    let s = s.strip_prefix("at ")?;
    let (call, rest) = s.split_once('(')?;
    let (class, method) = call.rsplit_once('.')?;
    // `(relics_thirteenflames@0.1.7.5/ThirteenFlames.java:38)`;
    // JDK modules (`java.base@21`) and loaders (`fml_loader@4`) too.
    let module = rest
        .split_once('/')
        .map(|(m, _)| m)
        .filter(|m| m.contains('@'))
        .map(|m| m.split('@').next().unwrap_or("").to_string())
        .unwrap_or_default();
    // Hidden lambda classes: `Foo$$Lambda/0x…` comes out as class `Foo$$Lambda/0x…`.
    let class = class.split('/').next().unwrap_or(class).to_string();
    Some(Frame { class, method: method.to_string(), module })
}

/// Reads a HotSpot thread dump (jcmd Thread.print / jstack).
pub fn parse_dump(text: &str) -> Vec<Thread> {
    let mut out: Vec<Thread> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.starts_with('"') {
            let end = line[1..].find('"').map_or(line.len(), |i| i + 1);
            let name = line[1..end].to_string();
            let id = line[end..].split_whitespace().find(|w| w.starts_with('#')).unwrap_or("").to_string();
            out.push(Thread { name, id, ..Default::default() });
            continue;
        }
        let Some(t) = out.last_mut() else { continue };
        let l = line.trim_start();
        if let Some(s) = l.strip_prefix("java.lang.Thread.State: ") {
            t.state = s.split_whitespace().next().unwrap_or("").to_string();
        } else if l.starts_with("at ") {
            if let Some(f) = parse_frame(l) {
                t.frames.push(f);
            }
        } else if let Some(s) = l.strip_prefix("- waiting to lock <") {
            let (addr, rest) = s.split_once('>').unwrap_or((s, ""));
            let class = rest.trim().trim_start_matches("(a ").trim_end_matches(')').to_string();
            t.waiting_lock = Some((addr.to_string(), class));
        } else if let Some(s) = l.strip_prefix("- locked <") {
            t.locked.push(s.split('>').next().unwrap_or("").to_string());
        } else if let Some(s) = l.strip_prefix("- waiting on the Class initialization monitor for ") {
            t.waiting_init = Some(s.trim().to_string());
        } else if l.starts_with("Found one Java-level deadlock") || l.starts_with("Found ") && l.contains("deadlock") {
            // The JVM's own summary repeats the threads below it; the
            // cycle is found from the threads themselves.
            break;
        }
    }
    out
}

/// What a dump says about a stuck start.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reading {
    /// Threads that wait on each other in a circle (by label), if any.
    pub cycle: Vec<String>,
    /// The cycle is classes' static set-up (not plain locks).
    pub class_init: bool,
    /// What's held up, in classes: for a class-init cycle the classes in
    /// it, for a lock cycle the lock objects' classes.
    pub held: Vec<String>,
    /// Mods: cause first (Likely), then the one that set it off
    /// (Possible), then victims (knock-on).
    pub suspects: Vec<Suspect>,
    /// One plain sentence for the notice.
    pub summary: String,
}

/// The mod a class belongs to: the jar from the mods folder's package
/// index, else the module name the frame carries (a NeoForge mod's module
/// is its mod id).
pub struct Owner<'a> {
    pub jar_of_class: &'a dyn Fn(&str) -> Option<String>,
    /// Mod id → jar file in the mods folder.
    pub jar_of_id: &'a dyn Fn(&str) -> Option<String>,
}

const NOT_MOD_MODULES: [&str; 8] = ["java.base", "minecraft", "neoforge", "fml_loader", "forge", "jdk.internal", "fml", "java"];

impl Owner<'_> {
    fn jar(&self, class: &str, module: &str) -> Option<String> {
        if let Some(j) = (self.jar_of_class)(class) {
            return Some(j);
        }
        if module.is_empty() || NOT_MOD_MODULES.contains(&module) || module.starts_with("java.") || module.starts_with("jdk.") {
            return None;
        }
        (self.jar_of_id)(module)
    }

    fn jar_of_frame(&self, f: &Frame) -> Option<String> {
        self.jar(&f.class, &f.module)
    }

    /// The jar of a class named alone (no frame): by package, else by the
    /// module of any frame in the dump that runs code of that class.
    fn jar_of_named(&self, class: &str, threads: &[Thread]) -> Option<String> {
        if let Some(j) = (self.jar_of_class)(class) {
            return Some(j);
        }
        threads.iter().flat_map(|t| &t.frames).find(|f| f.class == class).and_then(|f| self.jar_of_frame(f))
    }
}

/// The mod a thread was started for: the last mod frame above the loader
/// (the mod being constructed, the plugin being loaded).
fn bottom_mod(t: &Thread, own: &Owner) -> Option<String> {
    t.frames.iter().rev().find_map(|f| own.jar_of_frame(f))
}

/// Finds a circle in "thread waits for thread" edges.
fn find_cycle(edges: &HashMap<usize, usize>) -> Vec<usize> {
    for &start in edges.keys() {
        let mut seen: Vec<usize> = vec![start];
        let mut at = start;
        while let Some(&next) = edges.get(&at) {
            if let Some(i) = seen.iter().position(|&s| s == next) {
                let mut c = seen[i..].to_vec();
                c.sort();
                return c;
            }
            seen.push(next);
            at = next;
        }
    }
    Vec::new()
}

fn suspect(file: String, certainty: Certainty, why: &str, knock_on: bool) -> Suspect {
    Suspect { file_name: file, certainty, why: why.to_string(), knock_on, ..Default::default() }
}

fn push(out: &mut Vec<Suspect>, s: Suspect) {
    if let Some(e) = out.iter_mut().find(|e| e.file_name == s.file_name) {
        if s.certainty < e.certainty {
            *e = s;
        }
    } else {
        out.push(s);
    }
}

/// Reads `threads` for why the start hangs.
pub fn read(threads: &[Thread], own: &Owner) -> Reading {
    // Who holds what.
    let mut init_holder: HashMap<&str, usize> = HashMap::new();
    let mut lock_holder: HashMap<&str, usize> = HashMap::new();
    for (i, t) in threads.iter().enumerate() {
        for c in t.initializing() {
            init_holder.insert(c, i);
        }
        for l in &t.locked {
            lock_holder.insert(l.as_str(), i);
        }
    }
    // Who waits for whom.
    let mut edges: HashMap<usize, usize> = HashMap::new();
    for (i, t) in threads.iter().enumerate() {
        let holder = t
            .waiting_init
            .as_deref()
            .and_then(|c| init_holder.get(c))
            .or_else(|| t.waiting_lock.as_ref().and_then(|(a, _)| lock_holder.get(a.as_str())));
        if let Some(&h) = holder
            && h != i
        {
            edges.insert(i, h);
        }
    }
    let cycle = find_cycle(&edges);
    let mut r = Reading::default();
    let mut suspects = Vec::new();
    if !cycle.is_empty() {
        r.cycle = cycle.iter().map(|&i| threads[i].label()).collect();
        r.class_init = cycle.iter().any(|&i| threads[i].waiting_init.is_some());
        // What's held in the circle: the classes (or locks) each one waits for.
        for &i in &cycle {
            let t = &threads[i];
            if let Some(c) = &t.waiting_init {
                r.held.push(c.clone());
            } else if let Some((_, c)) = &t.waiting_lock {
                r.held.push(c.clone());
            }
        }
        // The held classes' mod is the likely cause: its code is what
        // the threads wait on.
        let held_jars: Vec<Option<String>> = r
            .held
            .iter()
            .map(|c| own.jar_of_named(c, threads))
            .collect();
        let mut cause: Option<String> = None;
        if let Some(Some(first)) = held_jars.first()
            && held_jars.iter().all(|j| j.as_deref() == Some(first))
        {
            let why = if r.class_init {
                "Two of its classes wait on each other while they set up"
            } else {
                "Its code holds locks that wait on each other"
            };
            push(&mut suspects, suspect(first.clone(), Certainty::Likely, why, false));
            cause = Some(first.clone());
        } else {
            for j in held_jars.iter().flatten() {
                push(&mut suspects, suspect(j.clone(), Certainty::Likely, "Its code is part of the circle of threads waiting on each other", false));
            }
        }
        // The mods that started each thread of the circle: they set it off
        // by touching that code from another thread at the same time.
        for &i in &cycle {
            if let Some(j) = bottom_mod(&threads[i], own)
                && cause.as_deref() != Some(j.as_str())
            {
                push(&mut suspects, suspect(j, Certainty::Possible, "Started that wait from another loading thread", false));
            }
        }
        // Everyone else waiting behind the circle: victims.
        let in_cycle: HashSet<usize> = cycle.iter().copied().collect();
        for (i, t) in threads.iter().enumerate() {
            if in_cycle.contains(&i) || !waits_on(i, &edges, &in_cycle) {
                continue;
            }
            if let Some(j) = bottom_mod(t, own) {
                push(&mut suspects, suspect(j, Certainty::Possible, "Only waiting behind the others, not the cause", true));
            }
        }
        let first = suspects.first().map(|s| s.file_name.clone());
        r.summary = match (r.class_init, first) {
            (true, Some(_)) => "Two mods are waiting on each other while they start, so it won't finish on its own. Starting again usually gets past it.".into(),
            (true, None) => "Two parts of the game are waiting on each other while they start, so it won't finish on its own. Starting again usually gets past it.".into(),
            (false, _) => "Threads are waiting on each other (a deadlock), so it won't finish on its own.".into(),
        };
    } else {
        // No circle: name what the loading threads wait in, gently.
        for t in threads.iter().filter(|t| is_loading_thread(&t.name)) {
            let waiting = t.waiting_init.is_some() || t.waiting_lock.is_some() || t.state == "BLOCKED";
            let in_mod = t.frames.iter().take(12).find_map(|f| own.jar_of_frame(f));
            if let (true, Some(j)) = (waiting || in_io(t), in_mod) {
                let why = if in_io(t) { "Its code is waiting on a file or the network" } else { "Its code is waiting for something" };
                push(&mut suspects, suspect(j, Certainty::Possible, why, false));
            }
        }
        r.summary = if suspects.is_empty() {
            "Nothing in the game is working and no mod stands out. The thread dump has the details.".into()
        } else {
            "Nothing in the game is working. The mod below is where the loading waits.".into()
        };
    }
    // Cause before trigger before victims.
    suspects.sort_by_key(|s| (s.knock_on, s.certainty));
    r.suspects = suspects;
    r
}

/// Whether thread `i` ends up waiting on the circle.
fn waits_on(mut i: usize, edges: &HashMap<usize, usize>, cycle: &HashSet<usize>) -> bool {
    for _ in 0..64 {
        match edges.get(&i) {
            Some(&n) if cycle.contains(&n) => return true,
            Some(&n) => i = n,
            None => return false,
        }
    }
    false
}

fn is_loading_thread(name: &str) -> bool {
    name.starts_with("modloading-worker")
        || name == "main"
        || name == "Render thread"
        || name == "Server thread"
        || name.starts_with("Worker-Main")
}

fn in_io(t: &Thread) -> bool {
    t.frames.iter().take(6).any(|f| {
        f.class.starts_with("sun.nio.ch.")
            || f.class.starts_with("java.net.")
            || f.class.starts_with("sun.net.")
            || f.class.contains("SocketDispatcher")
            || f.class.starts_with("java.io.FileInputStream")
    })
}

// ---------------------------------------------------------------------
// One call for the apps

/// How long a quiet start counts as stuck, set by the app from its
/// settings (0: off). Servers watch themselves, so they read it here.
static SECS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(DEFAULT_SECS);

pub fn set_secs(secs: u64) {
    SECS.store(secs, std::sync::atomic::Ordering::Relaxed);
}

pub fn settings() -> StallSettings {
    StallSettings { secs: SECS.load(std::sync::atomic::Ordering::Relaxed) }
}

/// A stuck start, read and saved: what the notice shows.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StuckReport {
    /// The game or server folder.
    pub root: PathBuf,
    pub pid: u32,
    /// When it was found (unix seconds).
    pub when: i64,
    /// "looks stuck while loading mods (no progress for 2 min, CPU idle)"
    pub headline: String,
    /// One plain sentence on what the dump shows.
    pub summary: String,
    /// Cause first, then what set it off, then victims (`knock_on`).
    pub suspects: Vec<Suspect>,
    /// Display names, one per suspect.
    pub names: Vec<String>,
    /// The saved thread dump.
    pub dump: Option<PathBuf>,
    /// Why there's no dump, when there isn't.
    pub no_dump: Option<String>,
    /// CPU over the quiet time, percent of the whole machine.
    pub cpu_pct: f32,
}

impl StuckReport {
    /// The mod to offer turning off: the one that set the wait off (it's
    /// optional more often than the library it called), else the cause.
    pub fn turn_off(&self) -> Option<&Suspect> {
        let real: Vec<&Suspect> = self.suspects.iter().filter(|s| !s.knock_on).collect();
        real.iter().find(|s| s.certainty == Certainty::Possible).or(real.first()).copied()
    }

    pub fn name_of(&self, s: &Suspect) -> String {
        self.suspects
            .iter()
            .position(|x| x.file_name == s.file_name)
            .and_then(|i| self.names.get(i).cloned())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| s.file_name.trim_end_matches(".disabled").trim_end_matches(".jar").to_string())
    }
}

/// The java(w) a process runs from.
pub fn process_exe(pid: u32) -> Option<PathBuf> {
    let mut sys = sysinfo::System::new();
    let p = sysinfo::Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[p]),
        true,
        sysinfo::ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::Always),
    );
    sys.process(p)?.exe().map(Path::to_path_buf)
}

/// Takes and reads a thread dump of the stuck start in `root` (its mods in
/// `root/mods`), saves it beside the crash reports, and says what it found.
/// Slow (a second or more, longer the first time a big pack's jars are
/// indexed): call it off the UI thread.
pub fn investigate(root: &Path, name: &str, pid: u32, stall: &Stall, now: i64) -> StuckReport {
    let mods_dir = root.join("mods");
    let mods: Vec<String> = std::fs::read_dir(&mods_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(".jar") || n.ends_with(".jar.disabled"))
        .collect();
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f32;
    let mut report = StuckReport {
        root: root.to_path_buf(),
        pid,
        when: now,
        headline: format!("{name} looks stuck {} ({})", stall.phase.words(), stall.words()),
        cpu_pct: stall.cores / cores * 100.0,
        ..Default::default()
    };
    let dump = process_exe(pid).ok_or_else(|| NoDump("Hopper couldn't find the game's Java.".into())).and_then(|java| take_dump(pid, &java));
    let reading = match &dump {
        Ok(text) => {
            let idx = crate::modindex::ModIndex::load(root, &mods);
            let cls = |c: &str| idx.jar_of_class(c).map(str::to_string);
            // Mod ids are read from the jars only when a frame's package
            // didn't name one (shaded or shared packages).
            let ids = std::cell::OnceCell::<HashMap<String, String>>::new();
            let by_id = |id: &str| {
                ids.get_or_init(|| {
                    let mut m = HashMap::new();
                    for f in &mods {
                        for i in crate::deps::own_ids(&mods_dir.join(f)).0 {
                            m.entry(i.to_ascii_lowercase()).or_insert_with(|| f.clone());
                        }
                    }
                    m
                })
                .get(&id.to_ascii_lowercase())
                .cloned()
            };
            read(&parse_dump(text), &Owner { jar_of_class: &cls, jar_of_id: &by_id })
        }
        Err(NoDump(why)) => {
            report.no_dump = Some(why.clone());
            Reading { summary: "It won't say why without a thread dump.".into(), ..Default::default() }
        }
    };
    report.summary = reading.summary.clone();
    report.names = reading.suspects.iter().map(|s| crate::crashdetect::jar_name(&mods_dir, &s.file_name)).collect();
    report.suspects = reading.suspects.clone();
    for s in report.suspects.iter_mut().filter(|s| s.mod_id.is_empty()) {
        s.mod_id = crate::deps::own_ids(&mods_dir.join(&s.file_name)).0.into_iter().next().unwrap_or_default();
    }
    if let Ok(text) = &dump {
        match save_dump(root, name, stall, &reading, text, now) {
            Ok(p) => report.dump = Some(p),
            Err(e) => log::warn!("saving the thread dump: {e}"),
        }
    }
    report
}

/// Watches the games Hopper sees running, one look every few seconds.
#[derive(Default)]
pub struct Games {
    games: HashMap<PathBuf, (u64, Watch)>,
}

impl Games {
    /// Forgets games that ended.
    pub fn retain(&mut self, running: &[PathBuf]) {
        self.games.retain(|k, _| running.contains(k));
    }

    /// One look at the game in `root` (process `pid`, started at `started`
    /// unix seconds): a stall when it just went stuck.
    pub fn look(&mut self, root: &Path, pid: u32, started: u64, now: i64) -> Option<Stall> {
        let log = root.join("logs").join("latest.log");
        let (s, w) = self.games.entry(root.to_path_buf()).or_insert_with(|| {
            // A log older than the game is the last run's: read from its
            // end, and from the start once the game makes a new one.
            let old = std::fs::metadata(&log)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .is_some_and(|t| t.as_secs() + 2 < started);
            (started, Watch::new(&log, false, now, old))
        });
        if *s != started {
            *s = started;
            *w = Watch::new(&log, false, now, false);
        }
        w.read_log(now);
        w.check(now, cpu_time_ms(pid), settings())
    }

    /// The player chose to wait: ask again after `secs` more quiet seconds.
    pub fn snooze(&mut self, root: &Path, now: i64, secs: i64) {
        if let Some((_, w)) = self.games.get_mut(root) {
            w.snooze(now, secs);
        }
    }
}

// ---------------------------------------------------------------------
// Saving

/// Saves the dump to `<root>/crash-reports/stuck-<time>.txt` with a short
/// header in words, beside the crash reports, and returns its path.
pub fn save_dump(root: &Path, name: &str, stall: &Stall, reading: &Reading, dump: &str, now: i64) -> std::io::Result<PathBuf> {
    let dir = root.join("crash-reports");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{DUMP_PREFIX}{}.txt", crate::time::file_stamp(now)));
    let mut head = format!(
        "---- Hopper: stuck start ----\n{name} looks stuck {} ({}: {:.2} of a core).\n{}\n",
        stall.phase.words(),
        stall.words(),
        stall.cores,
        reading.summary
    );
    if !reading.cycle.is_empty() {
        head.push_str(&format!("Waiting on each other: {}\n", reading.cycle.join(", ")));
        head.push_str(&format!("Held: {}\n", reading.held.join(", ")));
    }
    for s in &reading.suspects {
        let tag = if s.knock_on { "waiting (not the cause)" } else { s.certainty.label() };
        head.push_str(&format!("{tag}: {} ({})\n", s.file_name, s.why));
    }
    if !stall.last_line.is_empty() {
        head.push_str(&format!("Last log line: {}\n", stall.last_line));
    }
    head.push_str("\n");
    std::fs::write(&path, format!("{head}{dump}"))?;
    Ok(path)
}

/// A saved dump read back as a crash card's findings (its header block):
/// the "Stuck start" entry in Logs & crashes shows the same mods, with how
/// sure and why, as the notice did.
pub fn as_crash(path: &Path, server: bool) -> Option<crate::crashdetect::Crash> {
    use std::io::Read;
    let mut head = String::new();
    std::fs::File::open(path).ok()?.take(16 << 10).read_to_string(&mut head).ok();
    let mut lines = head.lines();
    if lines.next()? != "---- Hopper: stuck start ----" {
        return None;
    }
    let title = lines.next().unwrap_or_default();
    let summary = lines.next().unwrap_or_default();
    let mut suspects = Vec::new();
    for l in lines.take_while(|l| !l.is_empty()) {
        let Some((tag, rest)) = l.split_once(": ") else { continue };
        let certainty = match tag {
            "Certain" => Certainty::Certain,
            "Likely" => Certainty::Likely,
            "Possible" | "waiting (not the cause)" => Certainty::Possible,
            _ => continue,
        };
        let (file, why) = rest.split_once(" (").map_or((rest, ""), |(f, w)| (f, w.trim_end_matches(')')));
        suspects.push(suspect(file.to_string(), certainty, why, tag.starts_with("waiting")));
    }
    let when = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    // "X looks stuck while loading mods (…)." -> "Stuck while loading mods"
    let doing = title.split(" looks stuck ").nth(1).and_then(|t| t.split(" (").next()).unwrap_or("while starting");
    Some(crate::crashdetect::Crash {
        when,
        source: path.to_path_buf(),
        headline: format!("Stuck {doing}: no crash, it stopped making progress"),
        kind: if suspects.is_empty() { crate::crashdetect::CrashKind::Unknown } else { crate::crashdetect::CrashKind::Mods },
        suspects,
        server,
        stuck: true,
        advice: summary.to_string(),
        ..Default::default()
    })
}

/// Whether a file in `crash-reports` is one of these dumps, not a crash.
pub fn is_dump(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(DUMP_PREFIX))
}

/// The stuck-start dumps of `root`, newest first.
pub fn dumps(root: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(root.join("crash-reports"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_dump(p))
        .collect();
    v.sort();
    v.reverse();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/stuck_dumps").join(name);
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
    }

    /// The jars of the TNP Limitless 8 case, by package and by mod id.
    fn tnp_owner<'a>(cls: &'a dyn Fn(&str) -> Option<String>, ids: &'a dyn Fn(&str) -> Option<String>) -> Owner<'a> {
        Owner { jar_of_class: cls, jar_of_id: ids }
    }

    fn tnp_class(c: &str) -> Option<String> {
        let pkgs = [
            ("org.zeith.hammerlib.", "HammerLib-1.21.1-21.0.17.jar"),
            ("org.zeith.hammeranims.", "HammerAnimations-1.21.1-21.1.57.1.jar"),
            ("com.qurenie.", "relics_thirteenflames-0.1.7.5.jar"),
            ("dev.nolij.toomanyrecipeviewers.", "toomanyrecipeviewers-0.9.2.jar"),
            ("me.decce.ixeris.", "ixeris-4.6.11.jar"),
        ];
        pkgs.iter().find(|(p, _)| c.starts_with(p)).map(|(_, j)| j.to_string())
    }

    fn no_id(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn class_init_deadlock_names_the_library_not_the_victims() {
        // Real dump from TNP Limitless 8 (2026-11-05), trimmed to the threads
        // that matter. The JVM printed no "Java-level deadlock".
        let threads = parse_dump(&fixture("neoforge-clinit-deadlock.txt"));
        assert!(threads.iter().any(|t| t.waiting_init.as_deref() == Some("org.zeith.hammerlib.proxy.HLConstants")));
        let r = read(&threads, &tnp_owner(&tnp_class, &no_id));
        assert!(r.class_init);
        assert_eq!(r.cycle.len(), 2, "{:?}", r.cycle);
        let names: Vec<(&str, Certainty, bool)> =
            r.suspects.iter().map(|s| (s.file_name.as_str(), s.certainty, s.knock_on)).collect();
        assert_eq!(names[0], ("HammerLib-1.21.1-21.0.17.jar", Certainty::Likely, false), "{names:?}");
        assert_eq!(names[1], ("relics_thirteenflames-0.1.7.5.jar", Certainty::Possible, false), "{names:?}");
        // Too Many Recipe Viewers and HammerAnimations only wait behind it.
        for victim in ["toomanyrecipeviewers-0.9.2.jar", "HammerAnimations-1.21.1-21.1.57.1.jar"] {
            let s = r.suspects.iter().find(|s| s.file_name == victim).unwrap_or_else(|| panic!("{victim} missing: {names:?}"));
            assert!(s.knock_on, "{victim} must be a victim");
        }
    }

    #[test]
    fn module_names_find_the_mod_without_an_index() {
        let threads = parse_dump(&fixture("neoforge-clinit-deadlock.txt"));
        let none = |_: &str| None;
        let ids = |id: &str| (id == "hammerlib").then(|| "HammerLib.jar".to_string());
        let r = read(&threads, &Owner { jar_of_class: &none, jar_of_id: &ids });
        assert_eq!(r.suspects.first().map(|s| s.file_name.as_str()), Some("HammerLib.jar"));
    }

    #[test]
    fn monitor_deadlock_found_from_locks() {
        let threads = parse_dump(&fixture("monitor-deadlock.txt"));
        let cls = |c: &str| {
            if c.starts_with("com.alpha.") {
                Some("alpha.jar".to_string())
            } else if c.starts_with("com.beta.") {
                Some("beta.jar".to_string())
            } else {
                None
            }
        };
        let r = read(&threads, &tnp_owner(&cls, &no_id));
        assert!(!r.class_init);
        assert_eq!(r.cycle.len(), 2);
        let likely: Vec<&str> = r.suspects.iter().filter(|s| s.certainty == Certainty::Likely).map(|s| s.file_name.as_str()).collect();
        assert!(likely.contains(&"alpha.jar") && likely.contains(&"beta.jar"), "{:?}", r.suspects);
    }

    #[test]
    fn busy_dump_has_no_cycle() {
        let threads = parse_dump(&fixture("slow-but-busy.txt"));
        let r = read(&threads, &tnp_owner(&tnp_class, &no_id));
        assert!(r.cycle.is_empty());
    }

    fn feed_log(w: &mut Progress, lines: &[&str], now: i64) {
        for l in lines {
            w.feed(&format!("{l}\n"), now);
        }
    }

    #[test]
    fn repeated_lines_are_not_progress() {
        let mut p = Progress::new(false, 0);
        feed_log(&mut p, &["[17:16:42] [modloading-worker-0/INFO] [villagerapi/]: Content registration complete"], 10);
        assert_eq!(p.phase(), Phase::LoadingMods);
        let gl = "[17:16:43] [Render thread/INFO] [mojang/GlDebug]: OpenGL debug message: id=131204, texture state usage warning";
        feed_log(&mut p, &[gl], 11);
        for t in 12..200 {
            feed_log(&mut p, &[&gl.replace("17:16:43", &format!("17:17:{:02}", t % 60))], t);
        }
        assert_eq!(p.quiet_for(200), 189);
        assert!(!p.loaded());
    }

    #[test]
    fn stall_fires_only_when_quiet_and_idle() {
        let dir = std::env::temp_dir().join(format!("hopper-stuck-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let log = dir.join("latest.log");
        std::fs::write(&log, "[1] [modloading-worker-0/INFO] [x/]: constructing a\n").unwrap();
        let set = StallSettings { secs: 90 };

        // Deadlock: no new lines, ~0.1 core.
        let mut w = Watch::new(&log, false, 0, false);
        w.read_log(0);
        let mut fired = None;
        for t in (0..=120).step_by(5) {
            if let Some(s) = w.check(t, Some(t as u64 * 100), set) {
                fired.get_or_insert((t, s));
            }
        }
        let (t, s) = fired.expect("must fire");
        assert!((90..=100).contains(&t), "fired at {t}");
        assert_eq!(s.phase, Phase::LoadingMods);
        // Once per quiet spell.
        assert!(w.check(125, Some(12_500), set).is_none());

        // Slow but busy: same quiet log, a full core busy: never fires.
        let mut w = Watch::new(&log, false, 0, false);
        w.read_log(0);
        for t in (0..=300).step_by(5) {
            assert!(w.check(t, Some(t as u64 * 1000), set).is_none(), "fired while busy at {t}");
        }

        // Snooze: quiet again later asks again.
        let mut w = Watch::new(&log, false, 0, false);
        w.read_log(0);
        let fired_at = (0..=200).step_by(5).find(|&t| w.check(t, Some(t as u64 * 50), set).is_some()).unwrap();
        w.snooze(fired_at, 300);
        assert!((fired_at + 5..fired_at + 300).step_by(5).all(|t| w.check(t, Some(t as u64 * 50), set).is_none()));
        assert!(w.check(fired_at + 305, Some((fired_at as u64 + 305) * 50), set).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn server_done_ends_the_watch() {
        let mut p = Progress::new(true, 0);
        feed_log(&mut p, &["[10:00:01] [main/INFO] [minecraft/RecipeManager]: Loaded 57019 recipes"], 1);
        assert!(!p.loaded());
        feed_log(&mut p, &["[10:00:31] [Server thread/INFO] [minecraft/DedicatedServer]: Done (28.512s)! For help, type \"help\""], 31);
        assert!(p.loaded());
        let mut w = Watch { progress: p, ..Watch::new(Path::new("nope.log"), true, 0, false) };
        for t in (31..=400).step_by(5) {
            assert!(w.check(t, Some(0), StallSettings::default()).is_none());
        }
    }

    #[test]
    fn client_title_screen_ends_the_watch() {
        let mut p = Progress::new(false, 0);
        feed_log(&mut p, &["[15:19:19] [Render thread/INFO] [mojang/SoundEngine]: Sound engine started"], 5);
        assert!(p.loaded());
    }

    #[test]
    fn saved_dump_reads_back_as_a_card() {
        let dir = std::env::temp_dir().join(format!("hopper-stuck-card-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let threads = parse_dump(&fixture("neoforge-clinit-deadlock.txt"));
        let r = read(&threads, &tnp_owner(&tnp_class, &no_id));
        let stall = Stall { quiet_secs: 960, cores: 0.1, phase: Phase::LoadingMods, last_line: "x".into() };
        let p = save_dump(&dir, "TNP", &stall, &r, "Full thread dump", 1_700_000_000).unwrap();
        assert!(is_dump(&p));
        let c = as_crash(&p, false).unwrap();
        assert_eq!(c.headline, "Stuck while loading mods: no crash, it stopped making progress");
        assert_eq!(c.suspects.len(), r.suspects.len());
        assert_eq!(c.suspects[0].file_name, "HammerLib-1.21.1-21.0.17.jar");
        assert_eq!(c.suspects[0].certainty, Certainty::Likely);
        assert!(c.suspects.iter().any(|s| s.knock_on && s.file_name == "toomanyrecipeviewers-0.9.2.jar"));
        assert!(c.advice.starts_with("Two mods are waiting on each other"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dumps_are_told_apart_from_crashes() {
        assert!(is_dump(Path::new("crash-reports/stuck-2026-11-05_17.33.04.txt")));
        assert!(!is_dump(Path::new("crash-reports/crash-2026-11-05_17.33.04-client.txt")));
    }
}
