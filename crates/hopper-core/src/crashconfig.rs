//! Settings in the crash's mods' own configs that work around it, found
//! from what the crash itself names, never from who it blames:
//!
//! - an item or block id (`somemod:fe_seeds` → `fe_seed = true`),
//! - a mixin that failed (`perf.dynamic_resources.ModelBakeryMixin` →
//!   ModernFix's `mixin.perf.dynamic_resources` option),
//! - a mod's support code for another mod that crashed
//!   (`…examplebags.compat.jei.…` → `jeiCompat = true`),
//! - a limit the error says was hit, when the mod's config holds exactly
//!   that number (`larger than 1048576` → `maxPacketSize = 1048576`).
//!
//! Each match gets a score for how specific it is; only strong ones are
//! offered, and only settings the file really has (or lists as an option).

use crate::configedit::{self, Format, Value};
use crate::configindex::Side;
use crate::configswitch::{Switch, SwitchKind, core, label_of, last, squash};
use regex::Regex;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

/// Matches below this aren't offered.
pub const STRONG: u8 = 75;
/// At most this many suggestions per crash.
const MAX: usize = 3;

fn re(p: &str) -> Regex {
    Regex::new(p).expect("static regex")
}

/// `modernfix-modernfix.mixins.json:perf.dynamic_resources.ModelBakeryMixin from mod modernfix`
static MIXIN_REF: LazyLock<Regex> = LazyLock::new(|| re(r"([\w.-]+\.json):([\w.$]+)(?:\s+from mod ([\w.-]+))?"));
/// The lines where a mixin is the error itself, not just in the trace.
static MIXIN_FAIL: LazyLock<Regex> = LazyLock::new(|| {
    re(r"FAILED during|InvalidInjectionException|InvalidMixinException|MixinApplyError|MixinTransformerError|Critical injection failure|Mixin apply for mod|InjectionError|MixinPreProcessorException")
});
/// A stack frame: `at TRANSFORMER/somemod@1.0/com.x.Y.z(Y.java:1)`, `at com.x.Y.z(…)`.
static FRAME: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^\s*at\s+(?:[\w-]+/)?(?:([\w.-]+)@[^/\s]+/)?([A-Za-z_$][\w$]*(?:\.[\w$]+)+)\.[\w$<>]+\("));
/// `#  mixin.perf.dynamic_resources=true # (default)`: an option the file
/// lists in a comment (not the header's example line, which has no note).
static LISTED: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^\s*#+\s*(mixin\.[\w.$-]+)\s*=\s*(true|false)\s*#\s*\((?:default|overridden)"));
/// A limit in an error: `may not be larger than 1048576`, `max allowed: 2097152`.
static LIMIT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)\b(?:max(?:imum)?|limit(?:ed)?|larger than|bigger than|longer than|more than|exceed(?:s|ed|ing)?|too (?:big|large|many|long)|at most|capped)\b[^\d\n]{0,40}?(\d{3,12})\b")
});

/// Package parts that come before the name of the mod a module supports.
const COMPAT_WORDS: [&str; 9] = ["compat", "compats", "compatibility", "integration", "integrations", "addon", "addons", "module", "modules"];
/// Words a module name never is.
const NOT_A_MOD: [&str; 22] = [
    "client", "common", "server", "api", "impl", "util", "utils", "base", "core", "mixin", "mixins", "network", "forge", "neoforge",
    "fabric", "minecraft", "vanilla", "init", "registry", "data", "event", "events",
];
/// Words a setting that is a limit has in its name.
const LIMIT_WORDS: [&str; 9] = ["max", "limit", "size", "cap", "budget", "threshold", "length", "count", "distance"];

/// A mixin the crash names.
#[derive(Debug, Clone, PartialEq)]
struct Mixin {
    /// Its name below the mod's mixin package (`perf.dynamic_resources.ModelBakeryMixin`).
    rel: String,
    /// Its mixin config (`modernfix-modernfix.mixins.json`), when named.
    config: String,
    /// The mod it's from, when the crash says.
    owner: String,
    /// 95: the error is the mixin failing; 90: its code is in the trace;
    /// 60: only listed among the mixins that touched the trace's classes.
    strength: u8,
}

/// A mod's support code for another mod, named by a frame.
#[derive(Debug, Clone, PartialEq)]
struct Module {
    /// The other mod (`jei`).
    name: String,
    /// The frame's class (`com.example.examplebags.compat.jei.JeiPlugin`).
    class: String,
    /// The mod the frame says the class is in, if it says.
    owner: String,
}

/// A number the error says is a limit, with its line.
#[derive(Debug, Clone, PartialEq)]
struct Limit {
    n: i64,
    line: String,
}

/// The mixin's name below its mod's `mixin` package.
fn below_mixin_package(class: &str) -> Option<String> {
    let parts: Vec<&str> = class.split('.').collect();
    let i = parts.iter().rposition(|p| *p == "mixin" || *p == "mixins")?;
    (i + 1 < parts.len()).then(|| parts[i + 1..].join("."))
}

fn mixins(text: &str) -> Vec<Mixin> {
    let mut out: Vec<Mixin> = Vec::new();
    let mut add = |m: Mixin| {
        if let Some(o) = out.iter_mut().find(|o| o.rel == m.rel) {
            o.strength = o.strength.max(m.strength);
            if o.owner.is_empty() {
                o.owner = m.owner;
            }
            if o.config.is_empty() {
                o.config = m.config;
            }
        } else {
            out.push(m);
        }
    };
    for line in text.lines() {
        let fail = MIXIN_FAIL.is_match(line);
        for c in MIXIN_REF.captures_iter(line) {
            let rel = below_mixin_package(&c[2]).unwrap_or_else(|| c[2].to_string());
            let owner = c.get(3).map(|m| m.as_str().to_string()).unwrap_or_default();
            add(Mixin { rel, config: c[1].to_string(), owner, strength: if fail { 95 } else { 60 } });
        }
        // MixinTraceReforged's list: `org.x.mixin.perf.Foo (x.mixins.json)`.
        let t = line.trim();
        if let Some((class, cfg)) = t.strip_suffix(')').and_then(|t| t.rsplit_once(" (")) {
            if cfg.ends_with(".json") && !class.contains(' ') {
                if let Some(rel) = below_mixin_package(class) {
                    add(Mixin { rel, config: cfg.to_string(), owner: String::new(), strength: 60 });
                }
            }
        }
    }
    for c in FRAME.captures_iter(text) {
        // Mixin's own code and MixinExtras aren't a mod's mixin.
        if c[2].starts_with("org.spongepowered.") || c[2].starts_with("com.llamalad7.") {
            continue;
        }
        if let Some(rel) = below_mixin_package(&c[2]) {
            // A frame's method is the mixin's; its class is the mixin.
            let owner = c.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
            add(Mixin { rel, config: String::new(), owner, strength: 90 });
        }
    }
    out
}

fn modules(text: &str) -> Vec<Module> {
    let mut out: Vec<Module> = Vec::new();
    for c in FRAME.captures_iter(text) {
        let class = &c[2];
        let parts: Vec<&str> = class.split('.').collect();
        let Some(i) = parts.iter().position(|p| COMPAT_WORDS.contains(&p.to_ascii_lowercase().as_str())) else { continue };
        let Some(name) = parts.get(i + 1).filter(|n| i + 2 < parts.len() && n.chars().next().is_some_and(|c| c.is_ascii_lowercase())) else {
            continue;
        };
        let name = name.to_ascii_lowercase();
        if name.len() < 3 || NOT_A_MOD.contains(&name.as_str()) {
            continue;
        }
        let owner = c.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
        if !out.iter().any(|m| m.name == name && m.owner == owner) {
            out.push(Module { name, class: class.to_string(), owner });
        }
    }
    out
}

fn limits(text: &str) -> Vec<Limit> {
    let mut out: Vec<Limit> = Vec::new();
    for line in text.lines().filter(|l| !l.trim_start().starts_with("at ")) {
        for c in LIMIT.captures_iter(line) {
            let Ok(n) = c[1].parse::<i64>() else { continue };
            if n >= 64 && !out.iter().any(|l| l.n == n) {
                out.push(Limit { n, line: line.trim().chars().take(300).collect() });
            }
        }
    }
    out
}

/// The world a server reads its server configs from (server.properties' `level-name`).
fn level(root: &Path) -> String {
    fs::read_to_string(root.join("server.properties"))
        .ok()
        .and_then(|t| t.lines().find_map(|l| l.strip_prefix("level-name=").map(|s| s.trim().to_string())))
        .filter(|l| crate::configdiff::safe_rel(l))
        .unwrap_or_else(|| "world".into())
}

/// The config files of mod `id` that the game reads. On a server, a server
/// config comes from the world's `serverconfig/` when it's there (NeoForge
/// 21.1 reads that copy; `defaultconfigs/` only fills in a missing file).
pub fn files_of(root: &Path, id: &str, server: bool) -> Vec<String> {
    let mut out = crate::modconfig::files_for(root, &[id.to_string()]);
    // Mixin options files (`modernfix-mixins.properties`,
    // `ferritecore-mixin.toml`) aren't tied to their mod by name elsewhere.
    if let Ok(rd) = fs::read_dir(root.join("config")) {
        for e in rd.flatten().filter(|e| e.path().is_file()) {
            let name = e.file_name().to_string_lossy().to_lowercase();
            let stem = name.split('.').next().unwrap_or("");
            let mod_part = ["-mixins", "_mixins", "-mixin", "_mixin"].iter().find_map(|s| stem.strip_suffix(s));
            let rel = format!("config/{}", e.file_name().to_string_lossy());
            if mod_part.is_some_and(|m| squash(m) == squash(id)) && !out.contains(&rel) {
                out.push(rel);
            }
        }
    }
    if !server {
        return out;
    }
    let level = level(root);
    let dir = format!("{level}/serverconfig");
    if let Ok(rd) = fs::read_dir(root.join(&dir)) {
        for e in rd.flatten().filter(|e| e.path().is_file()) {
            let rel = format!("{dir}/{}", e.file_name().to_string_lossy());
            if crate::configowner::stem(&rel) == squash(id) && !out.contains(&rel) {
                out.push(rel);
            }
        }
    }
    let mut live: Vec<String> = Vec::new();
    for rel in out {
        let pick = if rel.starts_with("config/") {
            crate::configcompare::server_rel(&rel, &level, |r| root.join(r).is_file()).unwrap_or(rel)
        } else {
            rel
        };
        if !live.contains(&pick) {
            live.push(pick);
        }
    }
    live
}

/// One parsed config file of a mod.
struct File {
    rel: String,
    text: String,
    sections: Vec<configedit::Section>,
}

fn read(root: &Path, files: &[String]) -> Vec<File> {
    files
        .iter()
        .filter(|r| Format::of(r).is_some())
        .filter_map(|r| {
            let text = fs::read_to_string(root.join(r)).ok()?;
            let sections = configedit::parse_file(r, &text).unwrap_or_default();
            Some(File { rel: r.clone(), text, sections })
        })
        .collect()
}

/// Whether mod `id` is the one a mixin is from.
fn owns_mixin(id: &str, m: &Mixin) -> bool {
    let id = squash(id);
    if !m.owner.is_empty() {
        return squash(&m.owner) == id;
    }
    !m.config.is_empty() && squash(&m.config).contains(&id)
}

/// Mixin options (`mixin.perf.dynamic_resources`) in `f` for the mixins.
fn mixin_switches(id: &str, f: &File, mixins: &[Mixin], suspects: &HashSet<String>) -> Vec<(Switch, bool)> {
    // The options: lines in effect, then options the file only lists.
    let mut opts: Vec<(String, Vec<String>, bool, usize, bool, String)> = Vec::new(); // key, path, on, line, add, label
    for e in f.sections.iter().flat_map(|s| &s.entries) {
        if let (true, Value::Bool(on)) = (e.key.starts_with("mixin."), &e.value) {
            opts.push((e.key.clone(), e.path.clone(), *on, e.line, false, String::new()));
        }
    }
    let lines = f.text.lines().count();
    for c in LISTED.captures_iter(&f.text) {
        match opts.iter_mut().find(|o| o.0 == c[1]) {
            // Listed, and the line in effect is one Hopper added: undo
            // takes that line away again.
            Some(o) => {
                let added = format!("{}=false", &c[1]);
                o.4 = f.text.lines().nth(o.3.wrapping_sub(1)).is_some_and(|l| l.trim() == added);
            }
            None => opts.push((c[1].to_string(), Vec::new(), &c[2] == "true", lines + 1, true, String::new())),
        }
    }
    let mut out = Vec::new();
    for m in mixins.iter().filter(|m| owns_mixin(id, m)) {
        let pkg = m.rel.rsplit_once('.').map(|(p, _)| p).unwrap_or("");
        let best = opts
            .iter()
            .filter(|o| {
                let opt = &o.0["mixin.".len()..];
                !opt.is_empty() && (pkg == opt || pkg.starts_with(&format!("{opt}.")) || m.rel == opt)
            })
            .max_by_key(|o| o.0.len());
        let Some((key, path, on, line, add, _)) = best else { continue };
        let mut score = m.strength;
        // Only in the mixin list, and not a mod the crash blames: too weak.
        if score < 90 && !suspects.contains(&squash(id)) {
            score = score.min(50);
        }
        let opt = &key["mixin.".len()..];
        let label = configedit::label(opt.rsplit('.').next().unwrap_or(opt));
        let class = m.rel.rsplit('.').next().unwrap_or(&m.rel);
        let why = if m.strength >= 95 {
            format!("The crash is this part of {id} failing to load ({class}).")
        } else {
            format!("The crash went through this part of {id} ({class}).")
        };
        let s = Switch {
            mod_id: id.to_string(),
            item: m.rel.clone(),
            file: f.rel.clone(),
            line: *line,
            key: key.clone(),
            label,
            kind: SwitchKind::Mixin,
            path: path.clone(),
            add: *add,
            why,
            score,
            ..Default::default()
        };
        out.push((s, *on));
    }
    out
}

/// A setting's name without the words that say it's about support for a mod.
fn bare(key: &str) -> (String, bool) {
    let k = core(last(key));
    for w in ["compatibility", "integrations", "integration", "compat", "support", "plugin", "module", "addon"] {
        if let Some(r) = k.strip_suffix(w).filter(|r| !r.is_empty()) {
            return (core(r), true);
        }
        if let Some(r) = k.strip_prefix(w).filter(|r| !r.is_empty()) {
            return (core(r), true);
        }
    }
    (k, false)
}

/// Settings in `f` that turn off mod `id`'s support for a module the trace went through.
fn module_switches(id: &str, f: &File, modules: &[Module]) -> Vec<(Switch, bool)> {
    let sid = squash(id);
    let mut out = Vec::new();
    for m in modules {
        let ours = if m.owner.is_empty() { m.class.split('.').any(|p| squash(p) == sid) } else { squash(&m.owner) == sid };
        if !ours || squash(&m.name) == sid {
            continue;
        }
        let want = squash(&m.name);
        for e in f.sections.iter().flat_map(|s| &s.entries) {
            let Value::Bool(on) = e.value else { continue };
            let (name, worded) = bare(&e.key);
            // In a `[compat]` table, or a dotted key under one.
            let compat_part = |p: &str| {
                let p = squash(p);
                COMPAT_WORDS.iter().any(|w| p.contains(w)) || p.contains("support")
            };
            let in_compat = e.path.iter().any(|p| compat_part(p)) || e.key.split('.').rev().skip(1).any(compat_part);
            if name != want {
                continue;
            }
            let score = if worded || in_compat { 85 } else { 60 };
            let label = {
                let l = label_of(e);
                if l == e.label() { format!("{} support", configedit::label(&m.name)) } else { l }
            };
            let s = Switch {
                mod_id: id.to_string(),
                item: m.name.clone(),
                file: f.rel.clone(),
                line: e.line,
                key: e.key.clone(),
                label,
                kind: SwitchKind::Module,
                path: e.path.clone(),
                why: format!("The crash went through {id}'s code for {} ({}).", m.name, m.class),
                score,
                ..Default::default()
            };
            out.push((s, on));
        }
    }
    out
}

/// Limits in `f` that hold exactly a number the error says was hit: raised.
fn limit_switches(id: &str, f: &File, limits: &[Limit]) -> Vec<Switch> {
    let mut out = Vec::new();
    for l in limits {
        for e in f.sections.iter().flat_map(|s| &s.entries) {
            let Value::Int(v) = e.value else { continue };
            let k = squash(last(&e.key));
            if v != l.n || !LIMIT_WORDS.iter().any(|w| k.contains(w)) {
                continue;
            }
            let mut to = l.n.saturating_mul(2);
            if let Some((_, Some(hi))) = e.range {
                to = to.min(hi as i64);
            }
            if to <= l.n {
                continue;
            }
            // The setting's other words in the error too (`packet`): surer.
            let line = l.line.to_ascii_lowercase();
            let words: Vec<String> = e.label().to_ascii_lowercase().split(' ').map(str::to_string).collect();
            let named = words.iter().any(|w| w.len() >= 4 && !LIMIT_WORDS.contains(&w.as_str()) && line.contains(w.as_str()));
            out.push(Switch {
                mod_id: id.to_string(),
                item: l.n.to_string(),
                file: f.rel.clone(),
                line: e.line,
                key: e.key.clone(),
                label: e.label(),
                kind: SwitchKind::Limit,
                path: e.path.clone(),
                from: l.n.to_string(),
                to: to.to_string(),
                why: format!("The error says a limit of {} was hit, and this setting is {}.", l.n, l.n),
                score: if named { 90 } else { 80 },
                ..Default::default()
            });
        }
    }
    out
}

/// The settings that work around the crash: those still as the crash had
/// them (to offer, strongest first) and those changed since (to say so).
/// `text` is the crash report, `log` the game log before it; `suspects` the
/// mod ids the crash blames (Certain and Likely). `server`: a dedicated
/// server, whose live copy of a server config is the world's.
pub fn suggest(root: &Path, text: &str, log: &str, suspects: &[String], server: bool) -> (Vec<Switch>, Vec<Switch>) {
    let both = if log.is_empty() { text.to_string() } else { format!("{text}\n{log}") };
    let mixins = mixins(&both);
    let modules = modules(&both);
    let limits = limits(&both);
    let blamed: HashSet<String> = suspects.iter().map(|s| squash(s)).collect();
    // The mods to look in: those blamed, and those whose mixin is the error.
    let mut ids: Vec<String> = suspects.to_vec();
    for m in mixins.iter().filter(|m| m.strength >= 90 && !m.owner.is_empty()) {
        if !ids.iter().any(|i| squash(i) == squash(&m.owner)) {
            ids.push(m.owner.clone());
        }
    }
    let mut found: Vec<(Switch, bool)> = Vec::new();
    for id in ids.iter().filter(|i| !i.is_empty()) {
        let rels = files_of(root, id, server);
        if rels.is_empty() {
            continue;
        }
        if blamed.contains(&squash(id)) {
            found.extend(crate::configswitch::find_items(root, &rels, id, text));
        }
        let files = read(root, &rels);
        for f in &files {
            found.extend(mixin_switches(id, f, &mixins, &blamed));
            if blamed.contains(&squash(id)) {
                found.extend(module_switches(id, f, &modules));
                found.extend(limit_switches(id, f, &limits).into_iter().map(|s| (s, true)));
            }
        }
    }
    found.sort_by(|a, b| b.0.score.cmp(&a.0.score));
    let (mut on, mut off): (Vec<Switch>, Vec<Switch>) = (Vec::new(), Vec::new());
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for (mut s, is_on) in found {
        if s.score < STRONG || !seen.insert((s.file.clone(), s.key.clone())) {
            continue;
        }
        s.shared = server && crate::configindex::side_of(&s.file) != Side::Server;
        if is_on {
            if on.len() < MAX {
                on.push(s);
            }
        } else {
            off.push(s);
        }
    }
    (on, off)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("hopper-crashconfig-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for (rel, text) in files {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
        root
    }

    const FIX: &str = "crashconfig_fixtures";

    fn fixture(name: &str) -> String {
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(FIX).join(name)).unwrap()
    }

    #[test]
    fn reads_mixins_modules_and_limits() {
        let t = "Caused by: org.spongepowered.asm.mixin.transformer.throwables.MixinTransformerError: An unexpected critical error was encountered\n\
                 Caused by: org.spongepowered.asm.mixin.injection.throwables.InjectionError: Critical injection failure: @Inject annotation on onBake could not find any targets matching 'bake' in net.minecraft.client.resources.model.ModelBakery. Using refmap modernfix.refmap.json [PREINJECT Applicator Phase -> modernfix-modernfix.mixins.json:perf.dynamic_resources.ModelBakeryMixin from mod modernfix -> Prepare Injections -> handler$zza000$onBake]\n\
                 \tat TRANSFORMER/examplebags@3.20.5/com.example.examplebags.compat.jei.JeiCompat.registerRecipes(JeiCompat.java:61)\n\
                 \tat TRANSFORMER/minecraft@1.21.1/net.minecraft.client.Minecraft.tick(Minecraft.java:1)\n\
                 java.lang.IllegalArgumentException: Payload may not be larger than 1048576 bytes\n";
        let m = mixins(t);
        assert_eq!(m.len(), 1);
        assert_eq!((m[0].rel.as_str(), m[0].owner.as_str(), m[0].strength), ("perf.dynamic_resources.ModelBakeryMixin", "modernfix", 95));
        let md = modules(t);
        assert_eq!(md.len(), 1);
        assert_eq!((md[0].name.as_str(), md[0].owner.as_str()), ("jei", "examplebags"));
        assert_eq!(limits(t).iter().map(|l| l.n).collect::<Vec<_>>(), [1048576]);
        // Not modules: the mod's own core, the client package.
        assert!(modules("\tat com.x.mymod.compat.client.Foo.bar(Foo.java:1)\n\tat com.x.mymod.api.Thing.run(T.java:1)\n").is_empty());
        // A line number, a year: not limits.
        assert!(limits("\tat a.b.C.d(C.java:1048576)\nTime: 2026-11-05").is_empty());
    }

    #[test]
    fn modernfix_mixin_option_from_a_real_shape_crash() {
        let root = scratch("mf", &[("config/modernfix-mixins.properties", &fixture("modernfix-mixins.properties"))]);
        let crash = fixture("modernfix-mixin-crash.txt");
        // ModernFix isn't even blamed: its mixin failing is enough.
        let (on, off) = suggest(&root, &crash, "", &["somemod".into()], false);
        assert!(off.is_empty());
        assert_eq!(on.len(), 1, "{on:?}");
        let s = &on[0];
        assert_eq!((s.kind, s.mod_id.as_str(), s.key.as_str(), s.add), (SwitchKind::Mixin, "modernfix", "mixin.perf.dynamic_resources", true));
        assert_eq!(s.label, "Dynamic resources");
        assert!(s.score >= 90);
        assert_eq!(s.sentence("ModernFix"), "Turn off Dynamic resources in ModernFix (modernfix-mixins.properties · mixin.perf.dynamic_resources)");
        // Turned off: listed as off, no longer offered.
        crate::configswitch::turn_off(&root, s).unwrap();
        let (on, off) = suggest(&root, &crash, "", &["somemod".into()], false);
        assert!(on.is_empty());
        assert_eq!(off.len(), 1);
        crate::configswitch::turn_on(&root, &off[0]).unwrap();
        assert_eq!(fs::read_to_string(root.join(&s.file)).unwrap(), fixture("modernfix-mixins.properties"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn compat_module_toggle_from_a_real_shape_crash() {
        let root = scratch("jei", &[("config/examplebags-common.toml", &fixture("examplebags-common.toml"))]);
        let crash = fixture("compat-jei-crash.txt");
        let (on, _) = suggest(&root, &crash, "", &["examplebags".into()], false);
        assert_eq!(on.len(), 1, "{on:?}");
        assert_eq!((on[0].kind, on[0].key.as_str()), (SwitchKind::Module, "jeiCompat"));
        assert_eq!(on[0].path, ["compat"]);
        // Not blamed: its compat code alone doesn't point at its config.
        assert!(suggest(&root, &crash, "", &["jei".into()], false).0.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn limit_from_a_real_shape_server_crash_goes_to_the_live_copy() {
        let cfg = "[network]\n\t#The largest packet it sends, in bytes\n\t#Range: 1024 ~ 16777216\n\tmaxPacketSize = 1048576\n";
        let root = scratch(
            "limit",
            &[
                ("server.properties", "level-name=world\n"),
                ("config/somenet-server.toml", "[network]\n\tmaxPacketSize = 1048576\n"),
                ("world/serverconfig/somenet-server.toml", cfg),
                ("defaultconfigs/somenet-server.toml", cfg),
            ],
        );
        let crash = fixture("payload-limit-server-crash.txt");
        let (on, _) = suggest(&root, &crash, "", &["somenet".into()], true);
        assert_eq!(on.len(), 1, "{on:?}");
        let s = &on[0];
        assert_eq!(s.file, "world/serverconfig/somenet-server.toml", "the copy the world reads");
        assert_eq!((s.kind, s.from.as_str(), s.to.as_str(), s.shared), (SwitchKind::Limit, "1048576", "2097152", false));
        assert_eq!(s.action(), "Raise Max packet size to 2097152");
        assert_eq!(s.score, 80);
        // The setting's own word in the error too: surer.
        let (on, _) = suggest(&root, &crash.replace("Payload may", "Packet payload may"), "", &["somenet".into()], true);
        assert_eq!(on[0].score, 90);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_common_config_on_a_server_is_shared_with_players() {
        let root = scratch("shared", &[("config/examplebags-common.toml", &fixture("examplebags-common.toml"))]);
        let (on, _) = suggest(&root, &fixture("compat-jei-crash.txt"), "", &["examplebags".into()], true);
        assert!(on[0].shared);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn nothing_when_the_match_is_weak() {
        let mf = fixture("modernfix-mixins.properties");
        let jei = fixture("examplebags-common.toml");
        let root = scratch("weak", &[("config/modernfix-mixins.properties", &mf), ("config/examplebags-common.toml", &jei)]);
        // ModernFix's mixin only listed among those that touched the
        // trace's classes, ModernFix not blamed: no.
        let listed = "java.lang.NullPointerException: x\n\tat net.minecraft.client.Minecraft.tick(Minecraft.java:1)\n\
                      [MixinTraceReforged] Mixins in Stack Trace:\n\tnet.minecraft.client.Minecraft:\n\t\torg.embeddedt.modernfix.common.mixin.bugfix.world_leaks.MinecraftMixin (modernfix-modernfix.mixins.json)\n";
        assert!(suggest(&root, listed, "", &["othermod".into()], false).0.is_empty());
        // The real TNP client mixin crash: other mods' mixins.
        assert!(suggest(&root, &fixture("../crashdetect_fixtures/tnp-client-mixin-crash.txt"), "", &["modernfix".into()], false).0.is_empty());
        // A number that isn't a limit setting's value.
        assert!(suggest(&root, "Payload may not be larger than 4096 bytes", "", &["examplebags".into()], false).0.is_empty());
        // A compat package for a mod the setting isn't about.
        let other = "\tat TRANSFORMER/examplebags@1/com.example.examplebags.compat.patchouli.BookCompat.x(C.java:1)\n";
        assert!(suggest(&root, other, "", &["examplebags".into()], false).0.is_empty());
        // No configs at all.
        let empty = scratch("empty", &[]);
        assert!(suggest(&empty, &fixture("modernfix-mixin-crash.txt"), "", &["modernfix".into()], false).0.is_empty());
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&empty);
    }
}
