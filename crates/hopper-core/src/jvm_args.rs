//! Java arguments as choices: splits an argument string, says what each flag
//! does, builds a string from the generator's options (keeping flags it
//! doesn't know as they are) and spots flags that clash.
//!
//! Memory (`-Xmx`/`-Xms`) isn't an option here: it comes from the memory
//! slider, which Hopper puts before these arguments.

/// The garbage collector the generator picks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gc {
    /// No collector flag: the JVM's own choice (G1).
    Default,
    /// `-XX:+UseG1GC` alone.
    G1,
    /// G1 with Aikar's tuning flags.
    Aikar,
    /// ZGC, generational where the Java needs asking.
    Zgc,
    Shenandoah,
}

/// The Java the arguments are for, as far as it's known.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Target {
    pub major: Option<u32>,
    pub graalvm: bool,
    /// Built by Oracle (Oracle JDK or Oracle GraalVM).
    pub oracle: bool,
}

impl Target {
    pub fn of(j: &crate::java::JavaInstall) -> Self {
        Self {
            major: Some(j.major).filter(|m| *m > 0),
            graalvm: j.vendor_kind() == crate::java::Vendor::GraalVm,
            oracle: j.vendor.to_lowercase().contains("oracle"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Locale {
    pub language: String,
    pub country: Option<String>,
    /// Written as the older `-Duser.region` rather than `-Duser.country`.
    pub region_key: bool,
}

/// What the generator shows, read from an argument string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    pub gc: Gc,
    pub pre_touch: bool,
    pub disable_explicit_gc: bool,
    pub string_dedup: bool,
    pub compact_headers: bool,
    /// `-XX:ReservedCodeCacheSize` value, e.g. "512M".
    pub code_cache: Option<String>,
    pub locale: Option<Locale>,
    /// `-Djdk.nio.maxCachedBufferSize` in bytes.
    pub nio_cache: Option<u64>,
    pub jvmci: bool,
    pub eager_jvmci: bool,
    pub enterprise: bool,
    /// The property prefix the enterprise flag was written with.
    graal_prefix: Option<&'static str>,
    /// `-XX:+UnlockExperimentalVMOptions` was there.
    unlock: bool,
    /// Flags the generator doesn't cover, in their order, as written.
    pub extra: Vec<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            gc: Gc::Default,
            pre_touch: false,
            disable_explicit_gc: false,
            string_dedup: false,
            compact_headers: false,
            code_cache: None,
            locale: None,
            nio_cache: None,
            jvmci: false,
            eager_jvmci: false,
            enterprise: false,
            graal_prefix: None,
            unlock: false,
            extra: Vec::new(),
        }
    }
}

const UNLOCK: &str = "-XX:+UnlockExperimentalVMOptions";
const ZGEN: &str = "-XX:+ZGenerational";
const GC_FLAGS: [(&str, &str); 7] = [
    ("-XX:+UseG1GC", "G1"),
    ("-XX:+UseZGC", "ZGC"),
    ("-XX:+UseShenandoahGC", "Shenandoah"),
    ("-XX:+UseParallelGC", "Parallel"),
    ("-XX:+UseSerialGC", "Serial"),
    ("-XX:+UseConcMarkSweepGC", "CMS"),
    ("-XX:+UseEpsilonGC", "Epsilon"),
];

/// Aikar's G1 flags past `-XX:+UseG1GC`, without the ones the generator
/// has its own switches for (pre-touch, explicit GC) and the unlock.
pub fn aikar_tuning() -> Vec<&'static str> {
    crate::java::PRESETS[0]
        .1
        .split_whitespace()
        .filter(|f| {
            !matches!(
                *f,
                "-XX:+UseG1GC" | "-XX:+DisableExplicitGC" | "-XX:+AlwaysPreTouch" | UNLOCK
            )
        })
        .collect()
}

fn is_gc_flag(t: &str) -> bool {
    GC_FLAGS.iter().any(|(f, _)| *f == t)
}

/// Flags that only tune one collector, dropped when the collector changes.
fn is_gc_tuning(t: &str) -> bool {
    let name = t
        .strip_prefix("-XX:")
        .map(|s| s.trim_start_matches(['+', '-']))
        .unwrap_or("");
    t == ZGEN
        || name.starts_with("G1")
        || name.starts_with("Shenandoah")
        || name.starts_with('Z')
        || aikar_tuning().iter().any(|f| key(f) == key(t))
}

/// Splits on spaces, keeping a "quoted part" inside one argument.
pub fn split(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in args.chars() {
        if c == '"' {
            quoted = !quoted;
            cur.push(c);
        } else if c.is_whitespace() && !quoted {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn prop<'a>(t: &'a str, key: &str) -> Option<&'a str> {
    t.strip_prefix("-D")?.strip_prefix(key)?.strip_prefix('=')
}

impl Options {
    pub fn parse(args: &str) -> Self {
        let tokens = split(args);
        let mut o = Options::default();
        let gcs: Vec<&String> = tokens.iter().filter(|t| is_gc_flag(t)).collect();
        let tuning = aikar_tuning();
        // One known collector: it's the option. Two or more stay as written,
        // for the warning to point at.
        let gc = match gcs.as_slice() {
            [one] => match one.as_str() {
                "-XX:+UseG1GC" if tuning.iter().all(|f| tokens.iter().any(|t| t == f)) => {
                    Some(Gc::Aikar)
                }
                "-XX:+UseG1GC" => Some(Gc::G1),
                "-XX:+UseZGC" => Some(Gc::Zgc),
                "-XX:+UseShenandoahGC" => Some(Gc::Shenandoah),
                _ => None,
            },
            _ => None,
        };
        if let Some(gc) = gc {
            o.gc = gc;
        }
        let mut language = None;
        let mut country = None;
        let mut region_key = false;
        for t in &tokens {
            let s = t.as_str();
            let consumed = match s {
                _ if gc.is_some() && is_gc_flag(s) => true,
                ZGEN if gc == Some(Gc::Zgc) => true,
                _ if gc == Some(Gc::Aikar) && tuning.contains(&s) => true,
                UNLOCK => {
                    o.unlock = true;
                    true
                }
                "-XX:+AlwaysPreTouch" => {
                    o.pre_touch = true;
                    true
                }
                "-XX:+DisableExplicitGC" => {
                    o.disable_explicit_gc = true;
                    true
                }
                "-XX:+UseStringDeduplication" => {
                    o.string_dedup = true;
                    true
                }
                "-XX:+UseCompactObjectHeaders" => {
                    o.compact_headers = true;
                    true
                }
                "-XX:+UseJVMCICompiler" => {
                    o.jvmci = true;
                    true
                }
                "-XX:+EagerJVMCI" => {
                    o.eager_jvmci = true;
                    true
                }
                "-Djdk.graal.CompilerConfiguration=enterprise" => {
                    o.enterprise = true;
                    o.graal_prefix = Some("jdk.graal.");
                    true
                }
                "-Dgraal.CompilerConfiguration=enterprise" => {
                    o.enterprise = true;
                    o.graal_prefix = Some("graal.");
                    true
                }
                _ => {
                    if let Some(v) = s.strip_prefix("-XX:ReservedCodeCacheSize=") {
                        o.code_cache = Some(v.to_string());
                        true
                    } else if let Some(v) = prop(s, "jdk.nio.maxCachedBufferSize")
                        .and_then(|v| v.parse().ok())
                    {
                        o.nio_cache = Some(v);
                        true
                    } else if let Some(v) = prop(s, "user.language") {
                        language = Some(v.to_string());
                        true
                    } else if let Some(v) = prop(s, "user.country") {
                        country = Some(v.to_string());
                        true
                    } else if let Some(v) = prop(s, "user.region") {
                        country = Some(v.to_string());
                        region_key = true;
                        true
                    } else {
                        false
                    }
                }
            };
            if !consumed {
                o.extra.push(t.clone());
            }
        }
        match (language, country) {
            (Some(language), country) => {
                o.locale = Some(Locale {
                    language,
                    country,
                    region_key,
                })
            }
            // A country alone isn't a choice the generator offers.
            (None, Some(c)) => o.extra.push(format!(
                "-Duser.{}={c}",
                if region_key { "region" } else { "country" }
            )),
            (None, None) => {}
        }
        o
    }

    /// Changes the collector, dropping flags that only tuned the old one.
    pub fn set_gc(&mut self, gc: Gc) {
        // The unlock was there for Aikar's experimental G1 flags.
        if self.gc == Gc::Aikar && gc != Gc::Aikar {
            self.unlock = false;
        }
        self.gc = gc;
        self.extra.retain(|t| !is_gc_flag(t) && !is_gc_tuning(t));
    }

    fn needs_unlock(&self, target: Target) -> bool {
        self.gc == Gc::Aikar || (self.compact_headers && target.major.is_none_or(|m| m < 25))
    }

    /// The argument string: the generator's flags, then the rest as they were.
    pub fn build(&self, target: Target) -> String {
        let mut v: Vec<String> = Vec::new();
        if self.unlock || self.needs_unlock(target) {
            v.push(UNLOCK.into());
        }
        match self.gc {
            Gc::Default => {}
            Gc::G1 => v.push("-XX:+UseG1GC".into()),
            Gc::Aikar => {
                v.push("-XX:+UseG1GC".into());
                v.extend(aikar_tuning().into_iter().map(String::from));
            }
            Gc::Zgc => {
                v.push("-XX:+UseZGC".into());
                // Java 23 made it the only ZGC; 24 ignores the flag.
                if target.major.is_none_or(|m| m < 23) {
                    v.push(ZGEN.into());
                }
            }
            Gc::Shenandoah => v.push("-XX:+UseShenandoahGC".into()),
        }
        let mut flag = |on: bool, f: &str| {
            if on {
                v.push(f.into());
            }
        };
        flag(self.pre_touch, "-XX:+AlwaysPreTouch");
        flag(self.disable_explicit_gc, "-XX:+DisableExplicitGC");
        flag(self.string_dedup, "-XX:+UseStringDeduplication");
        flag(self.compact_headers, "-XX:+UseCompactObjectHeaders");
        flag(self.jvmci, "-XX:+UseJVMCICompiler");
        flag(self.eager_jvmci, "-XX:+EagerJVMCI");
        if self.enterprise {
            let prefix = self.graal_prefix.unwrap_or(match target.major {
                Some(m) if m < 21 => "graal.",
                _ => "jdk.graal.",
            });
            v.push(format!("-D{prefix}CompilerConfiguration=enterprise"));
        }
        if let Some(c) = &self.code_cache {
            v.push(format!("-XX:ReservedCodeCacheSize={c}"));
        }
        if let Some(l) = &self.locale {
            v.push(format!("-Duser.language={}", l.language));
            if let Some(c) = &l.country {
                let key = if l.region_key { "region" } else { "country" };
                v.push(format!("-Duser.{key}={c}"));
            }
        }
        if let Some(n) = self.nio_cache {
            v.push(format!("-Djdk.nio.maxCachedBufferSize={n}"));
        }
        v.extend(self.extra.iter().cloned());
        v.join(" ")
    }
}

/// A memory size as Java writes it ("12G", "512m", "1048576k", bytes) in MB.
pub fn parse_mb(s: &str) -> Option<u32> {
    let s = s.trim();
    let (num, unit) = match s.char_indices().last()? {
        (i, c) if c.is_ascii_alphabetic() => (&s[..i], c.to_ascii_lowercase()),
        _ => (s, 'b'),
    };
    let n: u64 = num.parse().ok()?;
    let mb = match unit {
        'g' => n * 1024,
        'm' => n,
        'k' => n / 1024,
        'b' => n / (1024 * 1024),
        _ => return None,
    };
    u32::try_from(mb).ok()
}

/// MB as Java reads it: whole gigabytes as "12G", else "1536M".
pub fn format_mb(mb: u32) -> String {
    if mb % 1024 == 0 {
        format!("{}G", mb / 1024)
    } else {
        format!("{mb}M")
    }
}

/// The `-Xms` and `-Xmx` in `args`, in MB (the last of each counts, as in Java).
pub fn memory(args: &str) -> (Option<u32>, Option<u32>) {
    let (mut xms, mut xmx) = (None, None);
    for t in split(args) {
        if let Some(v) = t.strip_prefix("-Xms").and_then(parse_mb) {
            xms = Some(v);
        } else if let Some(v) = t.strip_prefix("-Xmx").and_then(parse_mb) {
            xmx = Some(v);
        }
    }
    (xms, xmx)
}

pub(crate) fn is_memory(t: &str) -> bool {
    t.starts_with("-Xms") || t.starts_with("-Xmx")
}

/// `args` without its `-Xms`/`-Xmx`.
pub fn without_memory(args: &str) -> String {
    split(args)
        .into_iter()
        .filter(|t| !is_memory(t))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `args` with `-Xms`/`-Xmx` set to these (None drops it), first.
pub fn set_memory(args: &str, xms: Option<u32>, xmx: Option<u32>) -> String {
    let mut v: Vec<String> = Vec::new();
    if let Some(m) = xms {
        v.push(format!("-Xms{}", format_mb(m)));
    }
    if let Some(m) = xmx {
        v.push(format!("-Xmx{}", format_mb(m)));
    }
    v.extend(split(args).into_iter().filter(|t| !is_memory(t)));
    v.join(" ")
}

/// One plain sentence per flag, keyed by a template: the flag as the editor
/// inserts it (on for a switch, a typical value for the rest). Looked up by
/// name (`key`), so `-XX:-Name`, other values and the old `graal.` prefix
/// find the same sentence.
const FLAGS: &[(&str, &str)] = &[
    ("-XX:+UseG1GC", "G1 garbage collector: Java's default and a safe all-rounder for most packs."),
    ("-XX:+UseZGC", "ZGC garbage collector: very short pauses even with big heaps, for a bit more memory and CPU."),
    ("-XX:+ZGenerational", "Makes ZGC generational on Java 21 and 22, which is much faster; Java 23+ does this on its own."),
    ("-XX:+UseShenandoahGC", "Shenandoah garbage collector: short pauses like ZGC; Oracle's Java builds don't include it."),
    ("-XX:ShenandoahGCMode=generational", "Picks how Shenandoah works (satb, iu or generational)."),
    ("-XX:+UseParallelGC", "Parallel collector: high throughput but long pauses, which show as lag spikes in games."),
    ("-XX:+UseSerialGC", "Single-threaded collector for tiny heaps; too slow for Minecraft."),
    ("-XX:+UseConcMarkSweepGC", "The old CMS collector; removed in Java 14, so newer Java won't start with it."),
    ("-XX:+UseEpsilonGC", "A collector that never frees memory, for testing only."),
    ("-XX:+UnlockExperimentalVMOptions", "Allows experimental flags; must come before them or Java won't start."),
    ("-XX:+UnlockDiagnosticVMOptions", "Allows diagnostic flags; must come before them or Java won't start."),
    ("-XX:+AlwaysPreTouch", "Claims all of the memory up front, so startup is slower but there are fewer hitches later."),
    ("-XX:+DisableExplicitGC", "Ignores mods that ask for a full garbage collection, which would freeze the game for a moment."),
    ("-XX:+ParallelRefProcEnabled", "Cleans up references on several threads, shortening G1 pauses."),
    ("-XX:MaxGCPauseMillis=200", "The pause time G1 aims for, in milliseconds."),
    ("-XX:G1NewSizePercent=30", "Smallest share of the heap G1 keeps for new objects; Aikar raises it because Minecraft makes lots of short-lived objects."),
    ("-XX:G1MaxNewSizePercent=40", "Largest share of the heap G1 gives to new objects."),
    ("-XX:G1HeapRegionSize=8M", "Size of G1's heap regions; bigger regions suit big heaps."),
    ("-XX:G1ReservePercent=20", "Memory G1 keeps free so it doesn't run out in a burst."),
    ("-XX:G1HeapWastePercent=5", "How much garbage G1 leaves behind rather than spend time collecting."),
    ("-XX:G1MixedGCCountTarget=4", "How many collections G1 spreads old-memory cleanup over."),
    ("-XX:InitiatingHeapOccupancyPercent=15", "How full the heap gets before G1 starts cleaning old memory."),
    ("-XX:G1MixedGCLiveThresholdPercent=90", "Regions fuller than this are skipped by G1's old-memory cleanup."),
    ("-XX:G1RSetUpdatingPauseTimePercent=5", "Share of a pause G1 spends on bookkeeping."),
    ("-XX:SurvivorRatio=32", "Size of the survivor spaces next to new memory; Aikar makes them small."),
    ("-XX:MaxTenuringThreshold=1", "How many collections an object survives before it moves to old memory."),
    ("-XX:+PerfDisableSharedMem", "Stops Java writing stats to a file, which can stall it on slow disks."),
    ("-XX:+UseStringDeduplication", "Stores identical text only once, saving some memory for a little CPU."),
    ("-XX:+UseCompactObjectHeaders", "Smaller object headers, saving memory and often speed; Java 24+ (24 needs the experimental unlock)."),
    ("-XX:ReservedCodeCacheSize=512M", "Room for compiled code; big modpacks can fill the default 240 MB, then Java stops compiling and slows down."),
    ("-XX:+UseJVMCICompiler", "Uses the Graal compiler for hot code; GraalVM only, where it's usually on already."),
    ("-XX:+EnableJVMCI", "Turns on the interface Graal compiles through; GraalVM only."),
    ("-XX:+EagerJVMCI", "Starts the Graal compiler at launch instead of on first use, so early gameplay is smoother; GraalVM only."),
    ("-Djdk.graal.CompilerConfiguration=enterprise", "Picks the Graal compiler's optimizations; \"enterprise\" needs Oracle GraalVM, the Community build won't start with it."),
    ("-Djdk.graal.TuneInlinerExploration=1", "Lets Graal try more inlining, for faster code at the cost of compile time; GraalVM only."),
    ("-Djdk.graal.UsePriorityInlining=true", "Graal inlines the most useful calls first; GraalVM only."),
    ("-Djdk.graal.Vectorization=true", "Graal uses SIMD instructions for loops where it can; GraalVM only."),
    ("-Djdk.graal.OptDuplication=true", "Graal duplicates code paths to optimize each one; GraalVM only."),
    ("-Duser.language=en", "The language Java reports to mods; set to en when a mod breaks with your system language (Turkish is the classic one)."),
    ("-Duser.country=US", "The country Java reports; with user.language it fixes number and date formats some mods misread."),
    ("-Duser.region=US", "Older name for user.country: the country Java reports to mods."),
    ("-Djdk.nio.maxCachedBufferSize=262144", "Caps the memory each thread keeps for file and network buffers, stopping slow memory growth outside the heap."),
    ("-Dfile.encoding=UTF-8", "The text encoding Java uses for files; UTF-8 is the default from Java 18 on."),
    ("-Djava.net.preferIPv4Stack=true", "Uses IPv4 only, which fixes connection trouble on some networks."),
    ("-Dlog4j2.formatMsgNoLookups=true", "Turns off the Log4Shell lookup hole in old Minecraft versions."),
    ("-XX:MaxDirectMemorySize=4G", "Caps memory outside the heap that mods use for buffers."),
    ("-XX:+UseLargePages", "Uses large memory pages; on Windows it needs the Lock Pages in Memory right, or Java warns and ignores it."),
    ("-XX:+UseTransparentHugePages", "Large memory pages on Linux; does nothing on Windows."),
    ("-XX:+UseNUMA", "Spreads memory across CPU nodes on multi-socket or Threadripper machines."),
    ("-Xmx8G", "Most memory Java may use; the right end of the memory slider sets it."),
    ("-Xms4G", "Memory Java claims at start; the left end of the memory slider sets it (256 MB when it's at the start)."),
    ("-Xss4M", "Stack size per thread; raise it only if a crash says StackOverflowError."),
    ("-XX:MaxMetaspaceSize=2G", "Caps the memory used for loaded classes; big modpacks load a lot of them, so set it high or leave it out."),
    ("-XX:SoftRefLRUPolicyMSPerMB=50", "How long Java keeps cached objects that are only softly held; lower frees memory sooner."),
    ("-XX:ParallelGCThreads=4", "Threads the collector uses while the game is paused; Java picks from your CPU count."),
    ("-XX:ConcGCThreads=2", "Threads the collector uses while the game keeps running; Java picks from your CPU count."),
    ("-XX:+OmitStackTraceInFastThrow", "Drops the details from errors Java throws often; turn it off (-XX:-) to get full error logs while debugging."),
    ("-XX:+HeapDumpOnOutOfMemoryError", "Writes the whole memory to a file when Java runs out, for finding the cause; the file is as big as -Xmx."),
    ("-XX:+ExitOnOutOfMemoryError", "Quits at once when Java runs out of memory, instead of limping on."),
    ("-XX:+UseCompressedOops", "Smaller object pointers; on by default below 32 GB of memory."),
    ("-XX:+UseFastUnorderedTimeStamps", "Cheaper timing for logs and stats; experimental, needs the unlock."),
];

pub(crate) fn key(token: &str) -> &str {
    if let Some(x) = token.strip_prefix("-XX:") {
        let x = x.trim_start_matches(['+', '-']);
        return x.split('=').next().unwrap_or(x);
    }
    if let Some(d) = token.strip_prefix("-D") {
        let d = d.split('=').next().unwrap_or(d);
        return d
            .strip_prefix("jdk.graal.")
            .or_else(|| d.strip_prefix("graal."))
            .unwrap_or(d);
    }
    for x in ["Xmx", "Xms", "Xss"] {
        if token.strip_prefix('-').is_some_and(|t| t.starts_with(x)) {
            return x;
        }
    }
    token
}

/// What `token` does, or None for a flag Hopper doesn't know.
pub fn explain(token: &str) -> Option<String> {
    let k = key(token);
    let (_, text) = FLAGS.iter().find(|(t, _)| key(t) == k)?;
    Some(if token.starts_with("-XX:-") {
        format!("Turned off here. {text}")
    } else {
        (*text).to_string()
    })
}

/// Flags in `args` that clash with each other or with the Java.
pub fn conflicts(args: &str, target: Target) -> Vec<String> {
    clashes(&split(args), target).into_iter().map(|(m, _)| m).collect()
}

/// Each clash with the tokens (by index) it's about, so the editor can mark
/// them. Indices may be empty for a clash no single flag owns.
pub(crate) fn clashes(tokens: &[String], target: Target) -> Vec<(String, Vec<usize>)> {
    let at = |f: &str| -> Vec<usize> { (0..tokens.len()).filter(|&i| tokens[i] == f).collect() };
    let has = |f: &str| tokens.iter().any(|t| t == f);
    let mut out: Vec<(String, Vec<usize>)> = Vec::new();

    let gcs: Vec<(&str, &str)> = GC_FLAGS.iter().filter(|(f, _)| has(f)).copied().collect();
    if gcs.len() > 1 {
        let names: Vec<&str> = gcs.iter().map(|(_, n)| *n).collect();
        out.push((
            format!("Two garbage collectors are on ({}). Java won't start; keep one.", names.join(" and ")),
            gcs.iter().flat_map(|(f, _)| at(f)).collect(),
        ));
    }
    let gc = gcs.first().map(|(_, n)| *n);
    let major = target.major;
    if has(ZGEN) {
        match major {
            Some(m) if m < 21 => out.push((
                format!("-XX:+ZGenerational needs Java 21 or newer; this is Java {m}, which won't start with it."),
                at(ZGEN),
            )),
            Some(m) if m >= 24 => out.push((
                format!("Java {m} ignores -XX:+ZGenerational: ZGC is always generational now."),
                at(ZGEN),
            )),
            _ => {}
        }
        if gc.is_some() && gc != Some("ZGC") {
            out.push(("-XX:+ZGenerational only applies to ZGC.".into(), at(ZGEN)));
        }
    }
    if has("-XX:+UseZGC") && major.is_some_and(|m| m < 15) {
        out.push(("ZGC needs Java 15 or newer.".into(), at("-XX:+UseZGC")));
    }
    if has("-XX:+UseShenandoahGC") && target.oracle {
        out.push((
            "Oracle's Java builds don't include Shenandoah; this Java won't start with it.".into(),
            at("-XX:+UseShenandoahGC"),
        ));
    }
    if has("-XX:+UseCompactObjectHeaders") && major.is_some_and(|m| m < 24) {
        out.push((
            "-XX:+UseCompactObjectHeaders needs Java 24 or newer.".into(),
            at("-XX:+UseCompactObjectHeaders"),
        ));
    }
    let g1: Vec<usize> = (0..tokens.len()).filter(|&i| key(&tokens[i]).starts_with("G1")).collect();
    if !g1.is_empty() && gc.is_some_and(|g| g != "G1") {
        out.push((format!("G1 flags (-XX:G1…) do nothing with {}.", gc.unwrap_or_default()), g1));
    }

    // Experimental flags without the unlock before them.
    let unlock_at = tokens.iter().position(|t| t == UNLOCK);
    let needs_unlock = ["G1NewSizePercent", "G1MaxNewSizePercent", "UseFastUnorderedTimeStamps"];
    if let Some(i) = tokens.iter().position(|t| needs_unlock.contains(&key(t)))
        && unlock_at.is_none_or(|u| u > i)
    {
        out.push((
            format!("{} needs -XX:+UnlockExperimentalVMOptions before it, or Java won't start.", tokens[i]),
            (0..tokens.len())
                .filter(|&j| needs_unlock.contains(&key(&tokens[j])) && unlock_at.is_none_or(|u| u > j))
                .collect(),
        ));
    }

    let graal_at: Vec<usize> = (0..tokens.len())
        .filter(|&i| {
            let t = &tokens[i];
            t.contains("JVMCI") || t.starts_with("-Dgraal.") || t.starts_with("-Djdk.graal.")
        })
        .collect();
    if !graal_at.is_empty() && !target.graalvm {
        out.push((
            "These arguments are for GraalVM. Pick a GraalVM Java, or the game may not start.".into(),
            graal_at,
        ));
    } else if !graal_at.is_empty() && !target.oracle {
        let ent: Vec<usize> = (0..tokens.len())
            .filter(|&i| tokens[i].ends_with("CompilerConfiguration=enterprise"))
            .collect();
        if !ent.is_empty() {
            out.push((
                "CompilerConfiguration=enterprise needs Oracle GraalVM; the Community build won't start with it.".into(),
                ent,
            ));
        }
    }

    if let (Some(xms), Some(xmx)) = memory(&tokens.join(" "))
        && xms > xmx
    {
        let mem: Vec<usize> = (0..tokens.len()).filter(|&i| is_memory(&tokens[i])).collect();
        out.push(("-Xms is bigger than -Xmx; Java won't start.".into(), mem));
    }
    out
}

/// Every flag Hopper knows, as (template, sentence): the template is the
/// flag as the editor inserts it.
pub fn catalog() -> &'static [(&'static str, &'static str)] {
    FLAGS
}

// ---- The generator's rows, shared by Hopper desktop and Hopper Server ----
//
// Both apps show the same rows with the same choices and apply a pick the
// same way, so the same clicks write the same flags in either.

/// The collectors the generator offers, in the order it shows them.
pub const GC_CHOICES: [(Gc, &str); 5] = [
    (Gc::Default, "Default"),
    (Gc::G1, "G1"),
    (Gc::Aikar, "G1 (Aikar)"),
    (Gc::Zgc, "ZGC"),
    (Gc::Shenandoah, "Shenandoah"),
];
const CODE_CACHE_CHOICES: [&str; 4] = ["Default", "256M", "512M", "1G"];
const LOCALE_CHOICES: [&str; 2] = ["System", "en-US"];
const NIO_CHOICES: [(&str, Option<u64>); 3] = [("Off", None), ("256 KB", Some(262_144)), ("1 MB", Some(1_048_576))];

impl Gc {
    /// A sentence on what the collector is, for the generator's row.
    pub fn note(self) -> &'static str {
        match self {
            Gc::Default => "No collector flag: Java picks G1 by itself.",
            Gc::G1 => "G1 with Java's own settings: a safe all-rounder.",
            Gc::Aikar => "G1 with Aikar's tuning for Minecraft: fewer, shorter pauses. The usual choice up to about 12 GB.",
            Gc::Zgc => "ZGC: pauses under a millisecond even with big heaps, for a bit more memory and CPU. Java 21+ recommended.",
            Gc::Shenandoah => "Shenandoah: short pauses like ZGC. Not in Oracle's builds.",
        }
    }
}

/// What a generator row's control is.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Control {
    Switch { on: bool },
    /// Segmented choices. A value written by hand that isn't one of them
    /// shows as one more label (picking it changes nothing); `selected` is
    /// None when nothing fits (two collectors, or one the generator lacks).
    Choice { labels: Vec<String>, selected: Option<usize> },
}

/// One option of the generator.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Row {
    /// What [`Options::set`] takes.
    pub id: &'static str,
    pub label: &'static str,
    pub note: String,
    /// A heading the row sits under ("GraalVM"), None for the main list.
    pub section: Option<&'static str>,
    #[serde(flatten)]
    pub control: Control,
}

/// A pick on a row: a switch turned on or off, or a choice by index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    On(bool),
    Index(usize),
}

/// The labels for `values` and which one `current` is ("Default"/the first
/// when None); a value that isn't one of them is added as the last label.
fn choice(values: &[&str], current: Option<&str>) -> (Vec<String>, usize) {
    let mut labels: Vec<String> = values.iter().map(|s| s.to_string()).collect();
    let ix = match current {
        None => 0,
        Some(c) => match values.iter().position(|v| v.eq_ignore_ascii_case(c)) {
            Some(i) => i,
            None => {
                labels.push(c.to_string());
                labels.len() - 1
            }
        },
    };
    (labels, ix)
}

impl Options {
    /// The generator's rows for these options and this Java.
    pub fn rows(&self, target: Target) -> Vec<Row> {
        let explain = |flag: &str| explain(flag).unwrap_or_default();
        let switch = |id, label, flag: &str, on: bool| Row {
            id,
            label,
            note: explain(flag),
            section: None,
            control: Control::Switch { on },
        };
        let pick = |id, label, note: String, (labels, ix): (Vec<String>, usize)| Row {
            id,
            label,
            note,
            section: None,
            control: Control::Choice { labels, selected: Some(ix) },
        };
        // A collector that isn't a choice, or two: none selected.
        let other_gc = self.extra.iter().any(|t| t.starts_with("-XX:+Use") && t.ends_with("GC"));
        let gc_ix = if self.gc == Gc::Default && other_gc {
            None
        } else {
            GC_CHOICES.iter().position(|(g, _)| *g == self.gc)
        };
        let mut rows = vec![
            Row {
                id: "gc",
                label: "Garbage collector",
                note: self.gc.note().into(),
                section: None,
                control: Control::Choice { labels: GC_CHOICES.iter().map(|(_, l)| l.to_string()).collect(), selected: gc_ix },
            },
            switch("pre_touch", "Pre-touch memory", "-XX:+AlwaysPreTouch", self.pre_touch),
            switch("disable_explicit_gc", "Ignore forced GCs", "-XX:+DisableExplicitGC", self.disable_explicit_gc),
            switch("string_dedup", "String deduplication", "-XX:+UseStringDeduplication", self.string_dedup),
        ];
        if target.major.is_none_or(|m| m >= 24) || self.compact_headers {
            rows.push(switch("compact_headers", "Compact object headers", "-XX:+UseCompactObjectHeaders", self.compact_headers));
        }
        rows.push(pick(
            "code_cache",
            "Code cache",
            explain("-XX:ReservedCodeCacheSize=512M"),
            choice(&CODE_CACHE_CHOICES, self.code_cache.as_deref()),
        ));
        let locale_now = self.locale.as_ref().map(|l| match &l.country {
            Some(c) => format!("{}-{c}", l.language),
            None => l.language.clone(),
        });
        rows.push(pick(
            "locale",
            "Locale",
            "Reports English (US) to mods, which fixes crashes and misread numbers under some system languages.".into(),
            choice(&LOCALE_CHOICES, locale_now.as_deref()),
        ));
        let nio_now = self.nio_cache.map(|n| {
            NIO_CHOICES
                .iter()
                .find(|(_, v)| *v == Some(n))
                .map(|(l, _)| l.to_string())
                .unwrap_or_else(|| format!("{} KB", n / 1024))
        });
        rows.push(pick(
            "nio_cache",
            "Buffer cache cap",
            explain("-Djdk.nio.maxCachedBufferSize=262144"),
            choice(&NIO_CHOICES.map(|(l, _)| l), nio_now.as_deref()),
        ));
        if target.graalvm {
            let graal = |r: Row| Row { section: Some("GraalVM"), ..r };
            rows.push(graal(switch("jvmci", "Graal compiler", "-XX:+UseJVMCICompiler", self.jvmci)));
            rows.push(graal(switch("eager_jvmci", "Start Graal early", "-XX:+EagerJVMCI", self.eager_jvmci)));
            if target.oracle || self.enterprise {
                rows.push(graal(Row {
                    note: "Oracle GraalVM's stronger optimizations; usually a few percent faster.".into(),
                    ..switch("enterprise", "Enterprise optimizations", "", self.enterprise)
                }));
            }
        }
        rows
    }

    /// Applies a pick on the row `id`. False when it changes nothing (an
    /// unknown row, or the extra label of a value written by hand).
    pub fn set(&mut self, id: &str, pick: Pick) -> bool {
        match (id, pick) {
            ("gc", Pick::Index(i)) if i < GC_CHOICES.len() => self.set_gc(GC_CHOICES[i].0),
            ("pre_touch", Pick::On(v)) => self.pre_touch = v,
            ("disable_explicit_gc", Pick::On(v)) => self.disable_explicit_gc = v,
            ("string_dedup", Pick::On(v)) => self.string_dedup = v,
            ("compact_headers", Pick::On(v)) => self.compact_headers = v,
            ("jvmci", Pick::On(v)) => self.jvmci = v,
            ("eager_jvmci", Pick::On(v)) => self.eager_jvmci = v,
            ("enterprise", Pick::On(v)) => self.enterprise = v,
            ("code_cache", Pick::Index(i)) if i < CODE_CACHE_CHOICES.len() => {
                self.code_cache = (i > 0).then(|| CODE_CACHE_CHOICES[i].to_string())
            }
            ("locale", Pick::Index(i)) if i < LOCALE_CHOICES.len() => {
                let region_key = self.locale.as_ref().is_some_and(|l| l.region_key);
                self.locale = (i == 1).then(|| Locale { language: "en".into(), country: Some("US".into()), region_key });
            }
            ("nio_cache", Pick::Index(i)) if i < NIO_CHOICES.len() => self.nio_cache = NIO_CHOICES[i].1,
            _ => return false,
        }
        true
    }
}

/// `args` after a pick on a generator row: the generator's flags rebuilt,
/// flags it doesn't cover kept after them, and `-Xms`/`-Xmx` first as they
/// were. What both apps run when a generator option changes.
pub fn regenerate(args: &str, target: Target, id: &str, pick: Pick) -> String {
    let mut o = Options::parse(args);
    if !o.set(id, pick) {
        return args.to_string();
    }
    let (xms, xmx) = memory(args);
    set_memory(&o.build(target), xms, xmx)
}

/// The `-Xms` and `-Xmx` (MB) a two-thumb memory slider gives, so Java
/// always starts: the maximum at least 1 MB, the minimum never above it
/// (0 or None leaves it out).
pub fn memory_pair(xms: Option<u32>, xmx: u32) -> (Option<u32>, u32) {
    let xmx = xmx.max(1);
    (xms.filter(|m| *m > 0).map(|m| m.min(xmx)), xmx)
}

/// The top of a server's memory slider (MB) on a PC with `host_mb`: its
/// memory less 1 GB for the system, at most 64 GB, at least 2 GB, in half
/// gigabytes. Hopper desktop and the panel use the same.
pub fn memory_max_mb(host_mb: u64) -> u32 {
    let host = if host_mb > 0 { host_mb } else { 65536 };
    let mb = host.saturating_sub(1024).clamp(2048, 65536) as u32;
    mb / 512 * 512
}

#[cfg(test)]
mod tests {
    use super::*;

    const LL8: &str = "-Xms12G -Xmx18G -XX:+UseZGC -XX:+ZGenerational -XX:+UseJVMCICompiler -XX:+AlwaysPreTouch -XX:+DisableExplicitGC -Djdk.graal.CompilerConfiguration=enterprise -XX:ReservedCodeCacheSize=512M -Duser.language=en -Duser.region=US -Djdk.nio.maxCachedBufferSize=262144";

    const GRAAL21: Target = Target { major: Some(21), graalvm: true, oracle: true };
    const TEMURIN21: Target = Target { major: Some(21), graalvm: false, oracle: false };

    #[test]
    fn splits_with_quotes() {
        assert_eq!(split("  -a  -b\t-c "), ["-a", "-b", "-c"]);
        assert_eq!(
            split(r#"-Dpath="C:\My Games\x" -Xss4M"#),
            [r#"-Dpath="C:\My Games\x""#, "-Xss4M"]
        );
        assert!(split("").is_empty());
    }

    #[test]
    fn parses_ll8() {
        let o = Options::parse(LL8);
        assert_eq!(o.gc, Gc::Zgc);
        assert!(o.pre_touch && o.disable_explicit_gc && o.jvmci && o.enterprise);
        assert!(!o.eager_jvmci && !o.string_dedup);
        assert_eq!(o.code_cache.as_deref(), Some("512M"));
        assert_eq!(o.nio_cache, Some(262144));
        assert_eq!(
            o.locale,
            Some(Locale { language: "en".into(), country: Some("US".into()), region_key: true })
        );
        // Memory flags aren't the generator's: kept as written.
        assert_eq!(o.extra, ["-Xms12G", "-Xmx18G"]);
    }

    #[test]
    fn rebuilds_ll8_with_same_flags() {
        let built = Options::parse(LL8).build(GRAAL21);
        let mut a = split(&built);
        let mut b = split(LL8);
        a.sort();
        b.sort();
        assert_eq!(a, b);
        // And building what it built changes nothing.
        assert_eq!(Options::parse(&built).build(GRAAL21), built);
    }

    #[test]
    fn aikar_round_trip() {
        let aikar = crate::java::PRESETS[0].1;
        let o = Options::parse(aikar);
        assert_eq!(o.gc, Gc::Aikar);
        assert!(o.extra.is_empty(), "{:?}", o.extra);
        let mut a = split(&o.build(TEMURIN21));
        let mut b = split(aikar);
        a.sort();
        b.sort();
        assert_eq!(a, b);
        // The unlock comes before the experimental G1 flags.
        assert!(conflicts(&o.build(TEMURIN21), TEMURIN21).is_empty());
    }

    #[test]
    fn partial_g1_stays_g1() {
        let o = Options::parse("-XX:+UseG1GC -XX:MaxGCPauseMillis=50");
        assert_eq!(o.gc, Gc::G1);
        assert_eq!(o.extra, ["-XX:MaxGCPauseMillis=50"]);
    }

    #[test]
    fn switching_gc_drops_old_tuning() {
        let mut o = Options::parse("-XX:+UseG1GC -XX:G1HeapRegionSize=16M -XX:MaxGCPauseMillis=50 -Dfoo=1");
        o.set_gc(Gc::Zgc);
        assert_eq!(o.build(TEMURIN21), "-XX:+UseZGC -XX:+ZGenerational -Dfoo=1");
        // Two collectors: neither is the option; picking one drops both.
        let mut o = Options::parse("-XX:+UseG1GC -XX:+UseZGC");
        assert_eq!(o.gc, Gc::Default);
        o.set_gc(Gc::Shenandoah);
        assert_eq!(o.build(TEMURIN21), "-XX:+UseShenandoahGC");
    }

    #[test]
    fn leaving_aikar_drops_unlock() {
        let mut o = Options::parse(crate::java::PRESETS[0].1);
        o.set_gc(Gc::Zgc);
        assert_eq!(
            o.build(TEMURIN21),
            "-XX:+UseZGC -XX:+ZGenerational -XX:+AlwaysPreTouch -XX:+DisableExplicitGC"
        );
    }

    #[test]
    fn memory_flags() {
        assert_eq!(parse_mb("12G"), Some(12288));
        assert_eq!(parse_mb("512m"), Some(512));
        assert_eq!(parse_mb("1048576k"), Some(1024));
        assert_eq!(parse_mb("2147483648"), Some(2048));
        assert_eq!(parse_mb("x"), None);
        assert_eq!(format_mb(18432), "18G");
        assert_eq!(format_mb(1536), "1536M");
        assert_eq!(memory(LL8), (Some(12288), Some(18432)));
        assert_eq!(memory("-Xmx4G -Xmx6G"), (None, Some(6144)));
        assert_eq!(
            set_memory("-Xmx2G -XX:+UseZGC -Xms1G", Some(4096), Some(8192)),
            "-Xms4G -Xmx8G -XX:+UseZGC"
        );
        assert_eq!(set_memory("-Xms1G -XX:+UseZGC", None, Some(1536)), "-Xmx1536M -XX:+UseZGC");
        assert_eq!(without_memory(LL8), split(LL8)[2..].join(" "));
    }

    #[test]
    fn zgc_generational_by_java() {
        let mut o = Options::default();
        o.gc = Gc::Zgc;
        assert_eq!(o.build(TEMURIN21), "-XX:+UseZGC -XX:+ZGenerational");
        let j25 = Target { major: Some(25), ..TEMURIN21 };
        assert_eq!(o.build(j25), "-XX:+UseZGC");
    }

    #[test]
    fn builds_options() {
        let o = Options {
            gc: Gc::G1,
            string_dedup: true,
            code_cache: Some("1G".into()),
            locale: Some(Locale { language: "en".into(), country: Some("US".into()), region_key: false }),
            nio_cache: Some(1048576),
            extra: vec!["-Dmy.flag=1".into()],
            ..Options::default()
        };
        assert_eq!(
            o.build(TEMURIN21),
            "-XX:+UseG1GC -XX:+UseStringDeduplication -XX:ReservedCodeCacheSize=1G -Duser.language=en -Duser.country=US -Djdk.nio.maxCachedBufferSize=1048576 -Dmy.flag=1"
        );
        assert_eq!(Options::parse(&o.build(TEMURIN21)), o);
    }

    #[test]
    fn compact_headers_unlock_before_25() {
        let o = Options { compact_headers: true, ..Options::default() };
        assert_eq!(
            o.build(Target { major: Some(24), ..TEMURIN21 }),
            "-XX:+UnlockExperimentalVMOptions -XX:+UseCompactObjectHeaders"
        );
        assert_eq!(o.build(Target { major: Some(25), ..TEMURIN21 }), "-XX:+UseCompactObjectHeaders");
    }

    #[test]
    fn old_graal_prefix_kept() {
        let o = Options::parse("-Dgraal.CompilerConfiguration=enterprise");
        assert!(o.enterprise);
        assert_eq!(o.build(GRAAL21), "-Dgraal.CompilerConfiguration=enterprise");
        // New ones get the prefix their GraalVM reads.
        let new = Options { enterprise: true, ..Options::default() };
        assert_eq!(new.build(GRAAL21), "-Djdk.graal.CompilerConfiguration=enterprise");
        let graal17 = Target { major: Some(17), ..GRAAL21 };
        assert_eq!(new.build(graal17), "-Dgraal.CompilerConfiguration=enterprise");
    }

    #[test]
    fn explains_known_flags() {
        assert!(explain("-XX:+UseZGC").unwrap().contains("ZGC"));
        assert!(explain("-XX:ReservedCodeCacheSize=512M").unwrap().contains("compiled code"));
        assert!(explain("-Djdk.graal.CompilerConfiguration=enterprise").unwrap().contains("Oracle"));
        assert!(explain("-Dgraal.TuneInlinerExploration=1").is_some());
        assert!(explain("-Xmx18G").unwrap().contains("slider"));
        assert!(explain("-Xms12G").unwrap().contains("slider"));
        assert!(explain("-XX:-UseJVMCICompiler").unwrap().starts_with("Turned off"));
        assert_eq!(explain("-Dsome.mod.thing=1"), None);
        assert_eq!(explain("--add-opens"), None);
        // Every flag the generator writes has a sentence.
        for f in split(crate::java::PRESETS[0].1) {
            assert!(explain(&f).is_some(), "{f}");
        }
        for f in split(LL8) {
            assert!(explain(&f).is_some(), "{f}");
        }
    }

    /// What desktop's generator did before the rows moved here: parse, change
    /// one option, rebuild, memory first. `regenerate` must match it.
    fn old_desktop_edit(args: &str, target: Target, f: impl FnOnce(&mut Options)) -> String {
        let mut o = Options::parse(args);
        f(&mut o);
        let (xms, xmx) = memory(args);
        set_memory(&o.build(target), xms, xmx)
    }

    #[test]
    fn regenerate_matches_desktop() {
        let start = "-Xms4G -Xmx8G -XX:+UseG1GC -Dfml.readTimeout=180";
        let cases: [(&str, Pick, Box<dyn Fn(&mut Options)>); 8] = [
            ("gc", Pick::Index(2), Box::new(|o| o.set_gc(Gc::Aikar))),
            ("gc", Pick::Index(3), Box::new(|o| o.set_gc(Gc::Zgc))),
            ("pre_touch", Pick::On(true), Box::new(|o| o.pre_touch = true)),
            ("code_cache", Pick::Index(2), Box::new(|o| o.code_cache = Some("512M".into()))),
            ("locale", Pick::Index(1), Box::new(|o| {
                let region_key = o.locale.as_ref().is_some_and(|l| l.region_key);
                o.locale = Some(Locale { language: "en".into(), country: Some("US".into()), region_key })
            })),
            ("nio_cache", Pick::Index(1), Box::new(|o| o.nio_cache = Some(262_144))),
            ("compact_headers", Pick::On(true), Box::new(|o| o.compact_headers = true)),
            ("jvmci", Pick::On(true), Box::new(|o| o.jvmci = true)),
        ];
        for (id, pick, f) in cases {
            for target in [TEMURIN21, GRAAL21, Target { major: Some(25), ..TEMURIN21 }] {
                assert_eq!(regenerate(start, target, id, pick), old_desktop_edit(start, target, &*f), "{id}");
                assert_eq!(regenerate(LL8, target, id, pick), old_desktop_edit(LL8, target, &*f), "{id} on LL8");
            }
        }
        // Aikar from scratch is the preset's flags, memory kept first.
        let aikar = regenerate("-Xms6G -Xmx6G", TEMURIN21, "gc", Pick::Index(2));
        assert!(aikar.starts_with("-Xms6G -Xmx6G -XX:+UnlockExperimentalVMOptions -XX:+UseG1GC"), "{aikar}");
        assert_eq!(Options::parse(&aikar).gc, Gc::Aikar);
        // A pick that changes nothing leaves the text as typed.
        assert_eq!(regenerate("  -Xmx2G  -Dx=1 ", TEMURIN21, "nope", Pick::On(true)), "  -Xmx2G  -Dx=1 ");
        assert_eq!(regenerate("-XX:ReservedCodeCacheSize=384M", TEMURIN21, "code_cache", Pick::Index(4)), "-XX:ReservedCodeCacheSize=384M");
    }

    #[test]
    fn rows_follow_the_java() {
        let ids = |t: Target, args: &str| Options::parse(args).rows(t).into_iter().map(|r| r.id).collect::<Vec<_>>();
        let t21 = ids(TEMURIN21, "");
        assert!(!t21.contains(&"compact_headers") && !t21.contains(&"jvmci"), "{t21:?}");
        assert!(ids(Target { major: Some(25), ..TEMURIN21 }, "").contains(&"compact_headers"));
        let g = ids(GRAAL21, "");
        assert!(g.contains(&"jvmci") && g.contains(&"enterprise"));
        assert!(!ids(Target { oracle: false, ..GRAAL21 }, "").contains(&"enterprise"));

        let rows = Options::parse(LL8).rows(GRAAL21);
        let row = |id: &str| rows.iter().find(|r| r.id == id).unwrap().control.clone();
        assert_eq!(row("gc"), Control::Choice { labels: GC_CHOICES.iter().map(|(_, l)| l.to_string()).collect(), selected: Some(3) });
        assert_eq!(row("pre_touch"), Control::Switch { on: true });
        assert!(matches!(row("code_cache"), Control::Choice { selected: Some(2), .. }));
        // A hand-written size shows as an extra choice.
        let odd = Options::parse("-XX:ReservedCodeCacheSize=384M").rows(TEMURIN21);
        let cc = odd.iter().find(|r| r.id == "code_cache").unwrap();
        assert_eq!(cc.control, Control::Choice { labels: vec!["Default".into(), "256M".into(), "512M".into(), "1G".into(), "384M".into()], selected: Some(4) });
        // Two collectors: none picked.
        let two = Options::parse("-XX:+UseG1GC -XX:+UseZGC").rows(TEMURIN21);
        assert!(matches!(two[0].control, Control::Choice { selected: None, .. }));
        let par = Options::parse("-XX:+UseParallelGC").rows(TEMURIN21);
        assert!(matches!(par[0].control, Control::Choice { selected: None, .. }));
    }

    #[test]
    fn server_memory_range() {
        assert_eq!(memory_max_mb(62_566), 61_440);
        assert_eq!(memory_max_mb(0), 64_512);
        assert_eq!(memory_max_mb(128 * 1024), 65_536);
        assert_eq!(memory_max_mb(2048), 2048);
    }

    #[test]
    fn memory_pair_keeps_xms_at_most_xmx() {
        assert_eq!(memory_pair(Some(8192), 16384), (Some(8192), 16384));
        assert_eq!(memory_pair(Some(20480), 16384), (Some(16384), 16384));
        assert_eq!(memory_pair(Some(0), 4096), (None, 4096));
        assert_eq!(memory_pair(None, 0), (None, 1));
        for xms in [0u32, 512, 4096, 9999, 70000] {
            for xmx in [0u32, 1024, 4096, 65536] {
                let (a, b) = memory_pair(Some(xms), xmx);
                assert!(a.is_none_or(|a| a <= b));
            }
        }
    }

    #[test]
    fn warns_about_clashes() {
        let two = conflicts("-XX:+UseG1GC -XX:+UseZGC", TEMURIN21);
        assert!(two.iter().any(|w| w.contains("G1 and ZGC")), "{two:?}");

        let j17 = Target { major: Some(17), ..TEMURIN21 };
        let zgen = conflicts("-XX:+UseZGC -XX:+ZGenerational", j17);
        assert!(zgen.iter().any(|w| w.contains("needs Java 21")), "{zgen:?}");
        assert!(conflicts("-XX:+UseZGC -XX:+ZGenerational", TEMURIN21).is_empty());

        let graal = conflicts("-XX:+UseJVMCICompiler", TEMURIN21);
        assert!(graal.iter().any(|w| w.contains("GraalVM")), "{graal:?}");
        assert!(conflicts("-XX:+UseJVMCICompiler", GRAAL21).is_empty());
        let ce = Target { oracle: false, ..GRAAL21 };
        let ent = conflicts("-Djdk.graal.CompilerConfiguration=enterprise", ce);
        assert!(ent.iter().any(|w| w.contains("Oracle GraalVM")), "{ent:?}");

        let g1z = conflicts("-XX:+UseZGC -XX:G1HeapRegionSize=8M", TEMURIN21);
        assert!(g1z.iter().any(|w| w.contains("do nothing with ZGC")), "{g1z:?}");

        let unlock = conflicts("-XX:+UseG1GC -XX:G1NewSizePercent=30", TEMURIN21);
        assert!(unlock.iter().any(|w| w.contains("Unlock")), "{unlock:?}");

        let shen = conflicts("-XX:+UseShenandoahGC", Target { oracle: true, ..TEMURIN21 });
        assert!(shen.iter().any(|w| w.contains("Shenandoah")), "{shen:?}");

        assert!(conflicts(LL8, GRAAL21).is_empty());
        let big = conflicts("-Xms8G -Xmx4G", TEMURIN21);
        assert!(big.iter().any(|w| w.contains("bigger")), "{big:?}");
    }
}
