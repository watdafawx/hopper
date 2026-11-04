//! Weighs the evidence for each mod a crash points at, the way a person
//! reads a crash: what the loader and the older rules in
//! [`crate::crashdetect`] found are hints with a weight, and general rules
//! add or take weight from the facts in the report and the log. No rule
//! names a mod or a message of one mod. See `design/crash-detection.md`.
//!
//! Every rule leaves a [`Fact`] on the mod it weighs, in plain words: the
//! card's "Why" list. The certainty comes from the total and the gap to
//! the runner-up, not from one rule.

use crate::crashdetect::{Certainty, Fact, Suspect};
use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

fn re(p: &str) -> Regex {
    Regex::new(p).expect("static regex")
}

/// What an older rule's certainty is worth.
const CERTAIN: i32 = 100;
const LIKELY: i32 = 40;
const POSSIBLE: i32 = 10;

/// Rule names (also the facts' `rule`).
pub const HINT: &str = "hint";
pub const IN_MESSAGE: &str = "in_message";
pub const PATH_IN_MESSAGE: &str = "path_in_message";
pub const TOO_EARLY: &str = "too_early";
pub const WRAPPER: &str = "wrapper";
pub const ENVIRONMENT: &str = "environment";
pub const CHANGED: &str = "changed";
pub const FRAME: &str = "frame";
pub const ELSEWHERE: &str = "elsewhere";

/// What the older rules say of a mod whose mixin failed in the log.
const MIXIN_ELSEWHERE: &str = "Its changes to the game's code (a mixin) failed to apply.";

/// What the engine reads, besides the suspects found so far.
pub struct Input<'a> {
    /// The crash report.
    pub text: &'a str,
    /// The game log up to the crash (may be empty).
    pub log: &'a str,
    /// The instance (or server) folder.
    pub root: &'a Path,
    /// The jars in its mods folder.
    pub mods: &'a [String],
}

/// One frame of the report's trace, as the rules see it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frame {
    pub class: String,
    pub method: String,
    /// The mod whose code it is (its module), empty for the game, the
    /// loader and Java.
    pub module: String,
    /// The mod whose mixin runs here, and how (`wrapOperation`, `handler`…).
    pub mixin: Option<(String, String)>,
}

impl Frame {
    /// The mod the frame belongs to: its mixin's, else its module's.
    pub fn owner(&self) -> &str {
        match &self.mixin {
            Some((m, _)) => m,
            None => &self.module,
        }
    }

    /// A mixin that only wraps or redirects a call of the game's: every
    /// call goes through it, so it's on the way, not the cause.
    fn wraps(&self) -> bool {
        self.mixin.as_ref().is_some_and(|(_, how)| {
            matches!(how.as_str(), "wrapOperation" | "wrapWithCondition" | "wrapMethod" | "redirect") || how.starts_with("modify")
        })
    }
}

static FRAME_RE: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*at\s+(?:[\w.@+-]+/)*([\w$]+(?:\.[\w$]+)+?)\.([\w$<>]+)\("));
static MIXIN_HOW: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\.(handler|redirect|modify\w*|wrapOperation|wrapWithCondition|wrapMethod|localvar|constant)\$\w+\$([a-z][\w-]*)\$")
});

/// The frames of a trace, throw first.
pub fn frames(lines: &[&str]) -> Vec<Frame> {
    lines
        .iter()
        .filter_map(|l| {
            let m = FRAME_RE.captures(l)?;
            let module = crate::crashtrace::frame_mod(l).map(|(id, _)| id).unwrap_or_default();
            let mixin = MIXIN_HOW.captures(l).map(|x| (x[2].to_string(), x[1].to_string()));
            Some(Frame { class: m[1].to_string(), method: m[2].to_string(), module, mixin })
        })
        .collect()
}

/// The report's own trace: the frames under its error.
fn report_frames(text: &str) -> (Option<String>, Vec<Frame>) {
    let error = crate::crashtrace::report_error(text);
    let lines = match &error {
        Some(e) => crate::crashtrace::frames_after(text, e),
        None => Vec::new(),
    };
    let lines = if lines.is_empty() {
        // The first trace in the report.
        let all: Vec<&str> = text.lines().collect();
        match all.iter().position(|l| l.trim_start().starts_with("at ")) {
            Some(i) => all[i..].iter().take_while(|l| l.trim_start().starts_with("at ")).copied().collect(),
            None => Vec::new(),
        }
    } else {
        lines
    };
    (error, frames(&lines))
}

/// The error messages a crash report gives: its error, each "Caused by"
/// and the loader's "Failure message", before the report's details.
pub fn messages(text: &str) -> Vec<String> {
    let head = text.split("-- System Details --").next().unwrap_or(text);
    let mut out: Vec<String> = Vec::new();
    if let Some(e) = crate::crashtrace::report_error(head) {
        out.push(e);
    }
    for l in head.lines() {
        let t = l.trim();
        let msg = t
            .strip_prefix("Caused by: ")
            .or_else(|| t.strip_prefix("Caused by ").and_then(|r| r.split_once(": ").map(|(_, m)| m)))
            .or_else(|| t.strip_prefix("Failure message:"))
            .or_else(|| t.strip_prefix("Exception message:"));
        if let Some(m) = msg {
            let m = m.trim().to_string();
            if !m.is_empty() && !out.contains(&m) {
                out.push(m);
            }
        }
    }
    out
}

/// Ids written as `namespace:path` (`mythsandlegends:graveyard`), with
/// the whole id.
static NAMESPACED: LazyLock<Regex> = LazyLock::new(|| re(r"(?:^|[^\w.:/-])([a-z][a-z0-9_-]{1,63}):([a-z0-9_][a-z0-9_./-]*)(?:[^A-Za-z0-9]|$)"));
/// Namespaces that are the game's or shared by everyone.
const SHARED_NS: [&str; 10] = ["minecraft", "neoforge", "forge", "fabric", "c", "java", "realms", "brigadier", "http", "https"];

/// The mod-made ids an error message names, in order.
pub fn namespaced_ids(msg: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for m in NAMESPACED.captures_iter(msg) {
        let ns = m[1].to_string();
        if SHARED_NS.contains(&ns.as_str()) || out.iter().any(|(n, _)| *n == ns) {
            continue;
        }
        out.push((ns.clone(), format!("{ns}:{}", m[2].trim_end_matches(['.', '/']))));
    }
    out
}

/// A mod's folder or file named in a message: `libraries-integratedscripting\…`,
/// `config/somemod/…`.
static PATH_ID: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)(?:libraries-|[\\/]config[\\/]|^config[\\/])([a-z][a-z0-9_-]{2,63})"));

/// Messages that say something was asked for before the game had made it.
static TOO_EARLY_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r"(?i)No model for layer|not (?:yet )?(?:been )?initiali[sz]ed|before (?:it was|it's|the game was) (?:loaded|ready|initiali[sz]ed)",
        r"|Registry is already frozen|too early|Can't access registr|Tags? (?:\S+ )?(?:were|was) not bound|not bound yet",
        r"|accessed before|called before",
    ))
});

/// The game's frames where it starts up: the chain below them is the
/// game being made.
fn is_startup(f: &Frame) -> bool {
    (f.method == "<init>"
        && matches!(f.class.rsplit('.').next(), Some("Minecraft" | "MinecraftServer" | "DedicatedServer" | "IntegratedServer")))
        || (f.method == "main" && f.class.ends_with(".Main"))
}

/// A mod (not the game, not a library that runs others' code).
fn is_mod(id: &str) -> bool {
    !id.is_empty() && !crate::crashdetect::NOT_MODS.contains(&id) && !crate::crashtrace::is_pass_through(id)
}

/// A candidate while it's weighed.
struct Cand {
    s: Suspect,
    score: i32,
    facts: Vec<Fact>,
    /// Facts from the general rules (not the older hint).
    new: bool,
}

impl Cand {
    fn fact(&mut self, rule: &str, weight: i32, text: String) {
        self.score += weight;
        if rule != HINT && rule != FRAME {
            self.new = true;
        }
        self.facts.push(Fact { rule: rule.into(), weight, text });
    }

    fn is(&self, id: &str) -> bool {
        !id.is_empty() && self.s.mod_id.eq_ignore_ascii_case(id)
    }
}

fn base(c: Certainty) -> i32 {
    match c {
        Certainty::Certain => CERTAIN,
        Certainty::Likely => LIKELY,
        Certainty::Possible => POSSIBLE,
    }
}

/// The certainty a score earns, given the next one down.
pub fn certainty(score: i32, runner_up: i32) -> Certainty {
    if score >= 90 {
        Certainty::Certain
    } else if score >= LIKELY && score * 2 >= runner_up * 3 {
        Certainty::Likely
    } else {
        Certainty::Possible
    }
}

/// Re-weighs `suspects` (from the older rules) with the general rules,
/// adds mods only the evidence names, and orders them, most likely first.
/// `ids` gives each jar's mod ids; `index` the jars' contents, asked only
/// when a rule needs it.
pub fn weigh(
    inp: &Input,
    suspects: &mut Vec<Suspect>,
    ids: &impl Fn(&str) -> Vec<String>,
    index: &mut dyn FnMut() -> std::rc::Rc<crate::modindex::ModIndex>,
) {
    let mut cands: Vec<Cand> = suspects
        .drain(..)
        .map(|s| {
            let score = base(s.certainty);
            let facts = if s.why.is_empty() { Vec::new() } else { vec![Fact { rule: HINT.into(), weight: score, text: s.why.clone() }] };
            Cand { s, score, facts, new: false }
        })
        .collect();
    let (error, frames) = report_frames(inp.text);
    let msgs = messages(inp.text);
    // The mod whose own code threw: the first mod frame that isn't a mixin
    // or a library.
    let thrower = frames.iter().position(|f| f.mixin.is_none() && is_mod(&f.module));
    let thrower_id = thrower.map(|i| frames[i].module.clone()).unwrap_or_default();
    let any_frame = |id: &str| frames.iter().any(|f| f.owner().eq_ignore_ascii_case(id));

    // Where each mod is in the trace (for the "Why" list).
    for c in cands.iter_mut() {
        if let Some((i, f)) = frames.iter().enumerate().find(|(_, f)| f.owner().eq_ignore_ascii_case(&c.s.mod_id)) {
            let how = match &f.mixin {
                Some((_, how)) => format!(" (its {how} mixin in {})", short(&f.class)),
                None => String::new(),
            };
            let at = if i == 0 { "where the error was thrown".to_string() } else { format!("{} frames below the throw", i) };
            c.fact(FRAME, 0, format!("In the error's trace {at}: {}.{}{how}.", short(&f.class), f.method));
        }
    }

    // A mod's name for people: the one its jar gives itself, else its id.
    let name = |id: &str| -> String {
        inp.mods
            .iter()
            .find(|m| ids(m).iter().any(|i| i.eq_ignore_ascii_case(id)))
            .map(|m| crate::deps::own_ids(&inp.root.join("mods").join(m)).1)
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| id.to_string())
    };
    // The jar a mod id or namespace means.
    let jar_for = |ns: &str, index: &mut dyn FnMut() -> std::rc::Rc<crate::modindex::ModIndex>| -> Option<(String, String)> {
        let by_id = inp.mods.iter().find(|m| m.ends_with(".jar") && ids(m).iter().any(|i| i.eq_ignore_ascii_case(ns)));
        if let Some(f) = by_id {
            return Some((f.clone(), ns.to_string()));
        }
        let idx = index();
        let f = idx.jar_of_namespace(ns)?.to_string();
        let id = ids(&f).into_iter().next().unwrap_or_else(|| ns.to_string());
        Some((f, id))
    };
    let find = |cands: &mut Vec<Cand>, file: &str, id: &str| -> usize {
        if let Some(i) = cands.iter().position(|c| c.s.file_name == file || c.is(id)) {
            return i;
        }
        cands.push(Cand {
            s: Suspect {
                file_name: file.to_string(),
                mod_id: id.to_string(),
                certainty: Certainty::Possible,
                why: String::new(),
                needs: Vec::new(),
                changed: None,
                client_only: false,
                facts: Vec::new(),
            },
            score: 0,
            facts: Vec::new(),
            new: false,
        });
        cands.len() - 1
    };

    // A mod's id in the error message: it is what the game couldn't use.
    let mut named_outside: Vec<String> = Vec::new();
    let mut seen_ns: Vec<String> = Vec::new();
    for m in &msgs {
        for (ns, full) in namespaced_ids(m) {
            if seen_ns.contains(&ns) {
                continue;
            }
            seen_ns.push(ns.clone());
            let Some((file, id)) = jar_for(&ns, index) else { continue };
            let i = find(&mut cands, &file, &id);
            let c = &mut cands[i];
            if c.is(&thrower_id) {
                c.fact(IN_MESSAGE, 10, format!("The error names {full}, which is its own."));
            } else if any_frame(&id) {
                c.fact(IN_MESSAGE, 35, format!("The error names {full}, which comes from it."));
            } else {
                let extra = if thrower.is_none() { 15 } else { 0 };
                c.fact(IN_MESSAGE, 35 + extra, format!("The error names {full}, which comes from it, though none of its code was running: the game couldn't use something it adds."));
                named_outside.push(id);
            }
        }
        // A mod's folder in the message.
        for p in PATH_ID.captures_iter(m) {
            let id = p[1].to_ascii_lowercase();
            if !is_mod(&id) {
                continue;
            }
            let Some(file) = inp.mods.iter().find(|f| f.ends_with(".jar") && ids(f).iter().any(|i| i.eq_ignore_ascii_case(&id))) else { continue };
            let i = find(&mut cands, file, &id);
            if !cands[i].facts.iter().any(|f| f.rule == PATH_IN_MESSAGE) {
                cands[i].fact(PATH_IN_MESSAGE, 25, format!("The error is about one of its files ({}).", cut(&p[0], 60)));
            }
        }
    }
    // Something asked for too early: the mod whose code made that step run
    // early (below the one that asked, above the game's startup) did it.
    let mut early_found: Option<String> = None;
    if let (Some(t), Some(e)) = (thrower, &error)
        && msgs.iter().any(|m| TOO_EARLY_RE.is_match(m))
    {
        let start = frames.iter().position(is_startup);
        let dispatch = frames.iter().enumerate().skip(t + 1).find(|(_, f)| crate::crashtrace::is_dispatch_frame(&format!("at {}.{}(", f.class, f.method)) || f.class.contains(".bus.")).map(|(i, _)| i);
        if let (Some(s), Some(d)) = (start, dispatch)
            && d < s
        {
            // The lowest mod with its own code (or an injected handler) between
            // the dispatch and the startup frame started it.
            let early = frames[d..s]
                .iter()
                .rev()
                .find(|f| {
                    let id = f.owner();
                    is_mod(id) && !id.eq_ignore_ascii_case(&thrower_id) && (f.mixin.is_none() || !f.wraps())
                })
                .map(|f| f.owner().to_string());
            if let Some(id) = early
                && let Some(file) = inp.mods.iter().find(|f| f.ends_with(".jar") && ids(f).iter().any(|i| i.eq_ignore_ascii_case(&id))).cloned()
            {
                let what = cut(e.split_once(": ").map_or(e.as_str(), |(_, m)| m), 80);
                let i = find(&mut cands, &file, &id);
                let (thrower_name, early_name) = (name(&thrower_id), name(&id));
                cands[i].fact(
                    TOO_EARLY,
                    45,
                    format!("{thrower_name} asked for something before the game had made it (\"{what}\"). The code of {early_name} ran while the game was still starting and set off that step early."),
                );
                early_found = Some(id.clone());
                if let Some(tc) = cands.iter_mut().find(|c| c.is(&thrower_id)) {
                    tc.fact(TOO_EARLY, -20, format!("Its code asked too early, but only because {early_name} set off that step while the game was still starting."));
                }
            }
        }
    }

    // Once the cause is a mod the message names that wasn't running, or the
    // mod that set a step off too early, the mods whose only part in the
    // trace is wrapping one of the game's calls were only on the way.
    let found = named_outside.first().cloned().or_else(|| early_found.clone());
    if let Some(cause) = &found {
        for c in cands.iter_mut().filter(|c| !c.is(cause)) {
            let mine: Vec<&Frame> = frames.iter().filter(|f| f.owner().eq_ignore_ascii_case(&c.s.mod_id)).collect();
            if !mine.is_empty() && mine.iter().all(|f| f.wraps()) && c.score >= LIKELY && c.score < CERTAIN {
                let drop = 3 - c.score;
                c.fact(
                    WRAPPER,
                    drop,
                    format!("It only wraps a step of the game (a mixin), so every call goes through it; {} is what the error comes from, not its code.", name(cause)),
                );
            }
        }
    }

    // A mixin that failed to apply somewhere else in the log, with nothing
    // of that mod in this error, is a side note when the error itself
    // points at another mod.
    let direct = cands.iter().any(|c| {
        c.score >= LIKELY && (any_frame(&c.s.mod_id) || c.facts.iter().any(|f| f.rule == IN_MESSAGE || f.rule == TOO_EARLY))
    });
    if direct {
        for c in cands.iter_mut() {
            if c.score == LIKELY && c.s.why == MIXIN_ELSEWHERE && !any_frame(&c.s.mod_id) {
                c.fact(ELSEWHERE, -20, "Its change to the game's code (a mixin) failed to apply, but none of its code is in this error: it may misbehave, but it isn't what the error shows.".into());
            }
        }
    }

    // The computer, not a mod: a file another program holds, a full disk,
    // in the report's error or in what a mod failed with before it.
    let env_all = crate::crashsigs::environment(inp.text);
    for c in cands.iter_mut().filter(|c| c.score > 20) {
        if let Some(why) = env_all.or_else(|| crate::crashsigs::environment(&c.s.why)) {
            let drop = 20 - c.score;
            c.fact(ENVIRONMENT, drop, format!("It's only where it showed: {why}"));
        }
    }

    // Mods added or updated since the last game that worked.
    if let Some(good) = crate::crashdetect::last_good_run(inp.root) {
        let added = crate::instances::added_dates(inp.root);
        let sets: Vec<crate::backups::Set> = crate::backups::list(inp.root).into_iter().filter(|s| s.when > good).collect();
        for c in cands.iter_mut().filter(|c| c.score >= POSSIBLE) {
            let bare = c.s.file_name.trim_end_matches(".disabled");
            if added.get(bare).is_some_and(|t| *t > good) {
                c.fact(CHANGED, 15, "It was added after your last game that worked.".into());
            } else if sets.iter().any(|s| {
                s.files.iter().any(|f| {
                    f.restore_to.starts_with("mods/") && crate::deps::own_ids(&f.path).0.iter().any(|i| i.eq_ignore_ascii_case(&c.s.mod_id))
                })
            }) {
                c.fact(CHANGED, 15, "It was updated after your last game that worked.".into());
            }
        }
    }

    // Order and certainty. Ties keep the older rules' order.
    cands.sort_by(|a, b| b.score.cmp(&a.score));
    let scores: Vec<i32> = cands.iter().map(|c| c.score).collect();
    for (i, c) in cands.iter_mut().enumerate() {
        let next = scores.get(i + 1).copied().unwrap_or(0);
        let mut got = certainty(c.score, next);
        // The older rules' word stands where no general rule weighed in.
        if !c.new {
            got = c.s.certainty;
        }
        c.s.certainty = got;
        if c.new {
            c.s.why = why(&c.facts, &c.s.why);
        }
        let mut facts = std::mem::take(&mut c.facts);
        facts.sort_by(|a, b| b.weight.abs().cmp(&a.weight.abs()));
        c.s.facts = facts;
    }
    // The card shows the most certain first.
    cands.sort_by_key(|c| c.s.certainty);
    suspects.extend(cands.into_iter().map(|c| c.s));
}

/// The card's sentence: the strongest general rule's facts first, then
/// what the older rule said when it still holds.
fn why(facts: &[Fact], old: &str) -> String {
    let mut new: Vec<&Fact> = facts.iter().filter(|f| f.rule != HINT && f.rule != FRAME).collect();
    new.sort_by(|a, b| b.weight.abs().cmp(&a.weight.abs()));
    let lowered = new.first().is_some_and(|f| f.weight < 0);
    // The loader's own word (or a known error explained) stays the
    // sentence when the rules only agreed with it.
    let certain = facts.iter().any(|f| f.rule == HINT && f.weight >= 90);
    if certain && !new.iter().any(|f| f.weight < 0) && !old.is_empty() {
        return old.to_string();
    }
    let mut out: Vec<String> = new.iter().take(2).map(|f| f.text.clone()).collect();
    if !lowered && !old.is_empty() && new.first().is_some_and(|f| f.weight < 40) {
        out.push(old.to_string());
    }
    out.join(" ")
}

/// `twilightforest.client.ISTER` → `ISTER`.
fn short(class: &str) -> &str {
    class.rsplit('.').next().unwrap_or(class)
}

fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n).collect::<String>()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespaced_ids_skip_the_game_and_times() {
        let m = "Trying to access unbound value 'ResourceKey[minecraft:worldgen/structure / mythsandlegends:graveyard]' at 18:38:11";
        assert_eq!(namespaced_ids(m), [("mythsandlegends".to_string(), "mythsandlegends:graveyard".to_string())]);
        assert_eq!(namespaced_ids("No model for layer twilightforest:knightmetal_shield#main")[0].1, "twilightforest:knightmetal_shield");
        assert!(namespaced_ids("see https://github.com/x and Mixin [x.mixins.json:camera.ChunkMapMixin").is_empty());
    }

    #[test]
    fn certainty_needs_a_gap() {
        assert_eq!(certainty(100, 100), Certainty::Certain);
        assert_eq!(certainty(55, 30), Certainty::Likely);
        assert_eq!(certainty(50, 40), Certainty::Possible);
        assert_eq!(certainty(30, 0), Certainty::Possible);
    }

    #[test]
    fn frames_know_their_mod_and_mixins() {
        let f = frames(&[
            "\tat TRANSFORMER/twilightforest@4.8/twilightforest.client.ISTER.<init>(ISTER.java:106) ~[tf.jar%231!/:?]",
            "\tat TRANSFORMER/minecraft@1.21.1/net.minecraft.server.MinecraftServer.wrapOperation$zhe000$biolith$prependSurfaceRules(MinecraftServer.java:3708) ~[server.jar%231!/:?]",
            "\tat java.base/java.lang.Thread.run(Thread.java:1583) [?:?] {}",
        ]);
        assert_eq!((f[0].module.as_str(), f[0].method.as_str()), ("twilightforest", "<init>"));
        assert_eq!(f[1].owner(), "biolith");
        assert!(f[1].wraps());
        assert_eq!(f[2].class, "java.lang.Thread");
    }
}
