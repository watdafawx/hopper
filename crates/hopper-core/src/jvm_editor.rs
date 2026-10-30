//! What the Java arguments editor shows: each flag's parts for colouring,
//! whether it's known, clashing or malformed, its sentence for the hover
//! tooltip, and the suggestions while typing. Hopper desktop's editor calls
//! this directly; Hopper Server's panel gets the same through its API, so
//! both mark the same flags the same way.

use crate::jvm_args::{self, Target};
use serde::Serialize;
use std::ops::Range;

/// How a flag reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// A flag Hopper knows, nothing wrong.
    Ok,
    /// A flag Hopper doesn't know: kept as it is.
    Unknown,
    /// Works, but not as written (set twice, overridden by the slider…).
    Warn,
    /// Java won't start (or the flag can't work) as written.
    Error,
}

/// One argument in the text, with byte ranges into it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Span {
    /// The whole argument.
    pub range: Range<usize>,
    /// The flag's name, e.g. `UseG1GC` in `-XX:+UseG1GC`.
    pub name: Range<usize>,
    /// Its value, e.g. `200` in `-XX:MaxGCPauseMillis=200` or `14G` in `-Xmx14G`.
    pub value: Option<Range<usize>>,
    pub level: Level,
    /// What the flag does (known flags only).
    pub explain: Option<String>,
    /// What's wrong, for Warn and Error (and the Unknown note).
    pub problems: Vec<String>,
}

/// How `-Xms`/`-Xmx` in the text relate to the memory slider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Memory {
    /// Nothing sets memory apart from the text.
    Free,
    /// The slider writes them into the text (Hopper desktop): one that
    /// differs from the slider is overridden.
    Slider { xmx_mb: u32 },
    /// Typed ones are dropped on save (Hopper Server's panel).
    Dropped,
}

pub const UNKNOWN: &str = "Hopper doesn't know this flag. It's kept as it is.";

/// Arguments as Java reads them (split on spaces, a "quoted part" stays in
/// one), with their byte ranges. A `#` starting an argument comments out
/// the rest of its line, as in user_jvm_args.txt.
pub fn tokens(text: &str) -> Vec<(Range<usize>, &str)> {
    let mut out = Vec::new();
    let mut start = None;
    let mut quoted = false;
    let mut comment = false;
    for (i, c) in text.char_indices() {
        if comment {
            comment = c != '\n';
            continue;
        }
        if c == '#' && start.is_none() {
            comment = true;
            continue;
        }
        if c == '"' {
            quoted = !quoted;
        }
        if c.is_whitespace() && !quoted {
            if let Some(s) = start.take() {
                out.push((s..i, &text[s..i]));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        out.push((s..text.len(), &text[s..]));
    }
    out
}

/// The arguments and `#` comments in `text`, in order; `true` marks a
/// comment (it runs to the end of its line).
fn pieces(text: &str) -> Vec<(&str, bool)> {
    fn comments(gap: &str) -> impl Iterator<Item = (&str, bool)> {
        gap.lines().filter(|l| l.trim_start().starts_with('#')).map(|l| (l.trim(), true))
    }
    let mut out = Vec::new();
    let mut at = 0;
    for (r, t) in tokens(text) {
        // Comments sit in the gaps between arguments.
        out.extend(comments(&text[at..r.start]));
        out.push((t, false));
        at = r.end;
    }
    out.extend(comments(&text[at..]));
    out
}

/// The arguments one per line, as the editor shows them. Comments keep a
/// line of their own.
pub fn one_per_line(text: &str) -> String {
    pieces(text).into_iter().map(|(t, _)| t).collect::<Vec<_>>().join("\n")
}

/// The arguments on one line, split by single spaces, as they're saved.
/// A comment keeps a line of its own, so it hides nothing after it.
pub fn compact(text: &str) -> String {
    let mut out = String::new();
    let mut after_comment = false;
    for (t, comment) in pieces(text) {
        if !out.is_empty() {
            out.push(if after_comment || comment { '\n' } else { ' ' });
        }
        out.push_str(t);
        after_comment = comment;
    }
    out
}

/// Where the name and value sit in one argument (offsets within `t`).
fn parts(t: &str) -> (Range<usize>, Option<Range<usize>>) {
    let eq = |from: usize| -> (Range<usize>, Option<Range<usize>>) {
        match t[from..].find('=') {
            Some(e) => (from..from + e, Some(from + e + 1..t.len())),
            None => (from..t.len(), None),
        }
    };
    if let Some(rest) = t.strip_prefix("-XX:") {
        let sign = rest.len() - rest.trim_start_matches(['+', '-']).len();
        return eq(4 + sign);
    }
    if t.starts_with("-D") {
        return eq(2);
    }
    for x in ["Xmx", "Xms", "Xss", "Xmn"] {
        if t[1.min(t.len())..].starts_with(x) && t.starts_with('-') {
            return (1..1 + x.len(), (t.len() > 1 + x.len()).then(|| 1 + x.len()..t.len()));
        }
    }
    let lead = t.len() - t.trim_start_matches('-').len();
    eq(lead)
}

/// A `-XX:` flag Hopper knows: does it take a value (`=`), or is it a switch (+/-)?
fn takes_value(key: &str) -> Option<bool> {
    jvm_args::catalog()
        .iter()
        .find(|(t, _)| t.starts_with("-XX:") && jvm_args::key(t) == key)
        .map(|(t, _)| t.contains('='))
}

/// Arguments after these are the option's value, not flags.
fn takes_next(t: &str) -> bool {
    t.starts_with("--add-") || matches!(t, "-cp" | "-classpath" | "--class-path" | "-jar" | "--module-path" | "-p")
}

/// Every argument in `text` with its colouring parts and what's wrong.
pub fn analyze(text: &str, target: Target, memory: Memory) -> Vec<Span> {
    let toks = tokens(text);
    let owned: Vec<String> = toks.iter().map(|(_, t)| t.to_string()).collect();
    let mut spans: Vec<Span> = toks
        .iter()
        .enumerate()
        .map(|(i, (r, t))| {
            let (n, v) = parts(t);
            let explain = jvm_args::explain(t);
            let mut problems = Vec::new();
            let mut level = if explain.is_some() { Level::Ok } else { Level::Unknown };
            let prev_takes = i > 0 && takes_next(toks[i - 1].1);
            if !t.starts_with('-') {
                if prev_takes {
                    level = Level::Ok;
                } else {
                    level = Level::Error;
                    problems.push(format!(
                        "Not a flag: Java arguments start with -. Java reads \"{t}\" as the program to run and won't start."
                    ));
                }
            } else if let Some(rest) = t.strip_prefix("-XX:") {
                let k = jvm_args::key(t);
                let signed = rest.starts_with(['+', '-']);
                match takes_value(k) {
                    Some(false) if !signed => {
                        level = Level::Error;
                        problems.push(format!("Needs + or - before the name: -XX:+{k} turns it on, -XX:-{k} off."));
                    }
                    Some(false) if rest.contains('=') => {
                        level = Level::Error;
                        problems.push(format!("-XX:{k} is on or off, with no value: -XX:+{k}."));
                    }
                    Some(true) if signed || !rest.contains('=') => {
                        level = Level::Error;
                        problems.push(format!("Needs a value with no + or -: -XX:{k}=…"));
                    }
                    _ => {}
                }
                if k.is_empty() {
                    level = Level::Error;
                    problems.push("Missing the flag's name after -XX:.".into());
                }
            } else if t.starts_with("-D") && jvm_args::key(t).is_empty() {
                level = Level::Error;
                problems.push("Missing the property's name after -D.".into());
            }
            let base = r.start;
            Span {
                range: r.clone(),
                name: base + n.start..base + n.end,
                value: v.map(|v| base + v.start..base + v.end),
                level,
                explain,
                problems,
            }
        })
        .collect();

    let mut raise = |i: usize, level: Level, msg: String| {
        let s = &mut spans[i];
        if !s.problems.contains(&msg) {
            s.problems.push(msg);
        }
        if level == Level::Error || s.level != Level::Error {
            s.level = level;
        }
    };

    // The same flag twice: Java uses the last one.
    for i in 0..owned.len() {
        let t = &owned[i];
        if !t.starts_with('-') || t.starts_with("--") {
            continue;
        }
        let k = jvm_args::key(t);
        if k == t.as_str() && !t.starts_with("-X") {
            continue;
        }
        let same: Vec<usize> = (0..owned.len())
            .filter(|&j| owned[j].starts_with('-') && jvm_args::key(&owned[j]) == k)
            .filter(|&j| owned[j].starts_with("-XX:") == t.starts_with("-XX:") && owned[j].starts_with("-D") == t.starts_with("-D"))
            .collect();
        if same.len() < 2 {
            continue;
        }
        let last = *same.last().unwrap();
        if i == last {
            raise(i, Level::Warn, "Set more than once: Java uses this one, the last.".into());
        } else if owned[i] == owned[last] {
            raise(i, Level::Warn, "Set again later: this one is extra.".into());
        } else {
            raise(i, Level::Warn, format!("Set again later as {}: Java uses that one.", owned[last]));
        }
    }

    for (msg, at) in jvm_args::clashes(&owned, target) {
        let level = if msg.contains("won't start") || msg.contains("may not start") { Level::Error } else { Level::Warn };
        for i in at {
            raise(i, level, msg.clone());
        }
    }

    for i in 0..owned.len() {
        let t = &owned[i];
        if !jvm_args::is_memory(t) {
            continue;
        }
        match memory {
            Memory::Free => {}
            Memory::Slider { xmx_mb } => {
                if let Some(v) = t.strip_prefix("-Xmx").and_then(jvm_args::parse_mb)
                    && v != xmx_mb
                {
                    raise(i, Level::Warn, format!(
                        "Not used: the memory slider's maximum ({}) is. Move the slider to change it.",
                        jvm_args::format_mb(xmx_mb)
                    ));
                }
            }
            Memory::Dropped => raise(i, Level::Warn, "Not used: the memory slider sets -Xms and -Xmx. Move the slider instead.".into()),
        }
    }

    for s in &mut spans {
        if s.level == Level::Unknown && s.problems.is_empty() {
            s.problems.push(UNKNOWN.into());
        }
    }
    spans
}

/// One suggestion while typing a flag.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Suggestion {
    /// What gets typed in place of the word, e.g. `-XX:MaxGCPauseMillis=200`.
    pub insert: String,
    /// The sentence from the flag list.
    pub detail: &'static str,
}

/// The argument being typed: the text from the last space before `offset`
/// up to it, with where it starts.
pub fn word_at(text: &str, offset: usize) -> (usize, &str) {
    let offset = offset.min(text.len());
    let start = text[..offset].rfind(char::is_whitespace).map(|i| i + 1).unwrap_or(0);
    (start, &text[start..offset])
}

/// Known flags matching `word` (what's typed of the argument so far): ones
/// starting with it first, then ones whose name contains the typed name.
/// Suggests nothing until a `-` and one more character are typed.
pub fn complete(word: &str) -> Vec<Suggestion> {
    if !word.starts_with('-') || word.len() < 2 {
        return Vec::new();
    }
    let lower = word.to_ascii_lowercase();
    let off = word.starts_with("-XX:-");
    // The typed name with the prefix and sign taken off.
    let typed_name = jvm_args::key(word).to_ascii_lowercase();
    let family = |t: &str| {
        if t.starts_with("-XX:") { 0 } else if t.starts_with("-D") { 1 } else { 2 }
    };
    let word_family = if lower.starts_with("-xx") { Some(0) } else if word.starts_with("-D") { Some(1) } else if lower == "-x" { None } else if word.starts_with("-X") { Some(2) } else { None };
    let mut hits: Vec<(u8, Suggestion)> = Vec::new();
    for (t, text) in jvm_args::catalog() {
        // -XX:-Name turns a switch off.
        let insert = if off && t.starts_with("-XX:+") { format!("-XX:-{}", &t[5..]) } else { t.to_string() };
        let rank = if insert.to_ascii_lowercase().starts_with(&lower) {
            0
        } else if typed_name.len() >= 3
            && word_family.is_none_or(|f| f == family(t))
            && jvm_args::key(t).to_ascii_lowercase().contains(&typed_name)
        {
            1
        } else {
            continue;
        };
        if insert == word {
            continue;
        }
        hits.push((rank, Suggestion { insert, detail: text }));
    }
    hits.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.insert.cmp(&b.1.insert)));
    hits.into_iter().map(|(_, s)| s).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_round_trip() {
        let saved = "-Xmx14G -XX:+UseG1GC -XX:MaxGCPauseMillis=50 -Dfoo=\"a b\" -XX:+UnlockExperimentalVMOptions";
        let shown = one_per_line(saved);
        assert_eq!(shown.lines().count(), 5);
        assert_eq!(shown.lines().nth(3), Some("-Dfoo=\"a b\""));
        assert_eq!(compact(&shown), saved);
        // Typing: Enter, pasted newlines, tabs and extra spaces are just separators.
        assert_eq!(compact("  -Xmx4G\n\n\t-XX:+UseG1GC   -Da=1\r\n"), "-Xmx4G -XX:+UseG1GC -Da=1");
        assert_eq!(one_per_line("-Xmx4G  -XX:+UseG1GC\n"), "-Xmx4G\n-XX:+UseG1GC");
        assert_eq!(compact(""), "");
        assert_eq!(one_per_line("   "), "");
        // Comments keep their own line, so nothing after them is commented out.
        let file = "# memory\n-Xmx4G -Xms1G\n# gc\n-XX:+UseG1GC";
        assert_eq!(one_per_line(file), "# memory\n-Xmx4G\n-Xms1G\n# gc\n-XX:+UseG1GC");
        assert_eq!(compact(file), "# memory\n-Xmx4G -Xms1G\n# gc\n-XX:+UseG1GC");
        assert_eq!(compact(&one_per_line(file)), compact(file));
        // The same arguments either way.
        let toks = |t: &str| tokens(t).into_iter().map(|(_, s)| s.to_string()).collect::<Vec<_>>();
        assert_eq!(toks(&one_per_line(file)), toks(file));
        assert_eq!(toks(&compact(file)), toks(file));
    }

    const T21: Target = Target { major: Some(21), graalvm: false, oracle: false };

    fn span<'a>(spans: &'a [Span], text: &str, t: &str) -> &'a Span {
        let at = text.find(t).unwrap();
        spans.iter().find(|s| s.range.start == at).unwrap()
    }

    #[test]
    fn splits_with_ranges() {
        let text = "  -Xmx4G\n# a comment -Xmx2G\n-Dx=\"a b\"  -XX:+UseG1GC ";
        let t: Vec<&str> = tokens(text).into_iter().map(|(_, t)| t).collect();
        assert_eq!(t, ["-Xmx4G", "-Dx=\"a b\"", "-XX:+UseG1GC"]);
        for (r, t) in tokens(text) {
            assert_eq!(&text[r], t);
        }
    }

    #[test]
    fn parts_for_colouring() {
        let text = "-XX:+UseG1GC -XX:MaxGCPauseMillis=50 -Xmx14G -Duser.language=en";
        let s = analyze(text, T21, Memory::Free);
        assert_eq!(&text[s[0].name.clone()], "UseG1GC");
        assert_eq!(s[0].value, None);
        assert_eq!(&text[s[1].name.clone()], "MaxGCPauseMillis");
        assert_eq!(&text[s[1].value.clone().unwrap()], "50");
        assert_eq!(&text[s[2].name.clone()], "Xmx");
        assert_eq!(&text[s[2].value.clone().unwrap()], "14G");
        assert_eq!(&text[s[3].name.clone()], "user.language");
        assert!(s.iter().all(|s| s.level == Level::Ok), "{s:?}");
        assert!(s[0].explain.as_deref().unwrap().contains("G1"));
    }

    #[test]
    fn marks_problems() {
        let text = "-XX:+UseG1GC -XX:+UseZGC -XX:+UseFooBar -XX:AlwaysPreTouch -XX:+MaxGCPauseMillis=5 UseG1GC -Xmx8G";
        let s = analyze(text, T21, Memory::Slider { xmx_mb: 4096 });
        assert_eq!(span(&s, text, "-XX:+UseG1GC").level, Level::Error);
        assert!(span(&s, text, "-XX:+UseZGC").problems[0].contains("Two garbage collectors"));
        let unknown = span(&s, text, "-XX:+UseFooBar");
        assert_eq!(unknown.level, Level::Unknown);
        assert_eq!(unknown.problems, [UNKNOWN]);
        assert!(span(&s, text, "-XX:AlwaysPreTouch").problems[0].contains("+ or -"));
        assert!(span(&s, text, "-XX:+MaxGCPauseMillis").problems[0].contains("value"));
        assert_eq!(span(&s, text, "UseG1GC -Xmx").level, Level::Error);
        let xmx = span(&s, text, "-Xmx8G");
        assert_eq!(xmx.level, Level::Warn);
        assert!(xmx.problems[0].contains("slider"));
        // The slider's own -Xmx is fine.
        let s = analyze("-Xmx4G", T21, Memory::Slider { xmx_mb: 4096 });
        assert_eq!(s[0].level, Level::Ok);
        let s = analyze("-Xms2G", T21, Memory::Dropped);
        assert_eq!(s[0].level, Level::Warn);
    }

    #[test]
    fn marks_duplicates_and_keeps_option_values() {
        let text = "-XX:MaxGCPauseMillis=50 -XX:MaxGCPauseMillis=200 --add-opens java.base/java.lang=ALL-UNNAMED";
        let s = analyze(text, T21, Memory::Free);
        assert!(s[0].problems[0].contains("-XX:MaxGCPauseMillis=200"));
        assert_eq!(s[1].level, Level::Warn);
        assert_eq!(s[3].level, Level::Ok);
        assert!(s[3].problems.is_empty());
    }

    #[test]
    fn unlock_order_marks_the_flag() {
        let text = "-XX:G1NewSizePercent=30 -XX:+UnlockExperimentalVMOptions";
        let s = analyze(text, T21, Memory::Free);
        assert_eq!(s[0].level, Level::Error);
        assert_eq!(s[1].level, Level::Ok);
    }

    #[test]
    fn completes_flags() {
        let c = complete("-XX:+Alw");
        assert_eq!(c[0].insert, "-XX:+AlwaysPreTouch");
        let c = complete("-XX:MaxG");
        assert_eq!(c[0].insert, "-XX:MaxGCPauseMillis=200");
        // A switch typed with - stays off.
        assert_eq!(complete("-XX:-UseJVMCI")[0].insert, "-XX:-UseJVMCICompiler");
        // A name typed in the middle still finds it.
        assert!(complete("-XX:+PreTouch").iter().any(|s| s.insert == "-XX:+AlwaysPreTouch"));
        assert!(complete("-Duser").iter().any(|s| s.insert == "-Duser.language=en"));
        assert!(complete("-Xm").iter().any(|s| s.insert == "-Xmx8G"));
        assert!(complete("-X").len() > 20);
        assert!(complete("-").is_empty());
        assert!(complete("Use").is_empty());
        assert!(complete("-XX:+AlwaysPreTouch").is_empty());
        assert_eq!(word_at("-Xmx4G -XX:+Al", 14), (7, "-XX:+Al"));
    }
}
