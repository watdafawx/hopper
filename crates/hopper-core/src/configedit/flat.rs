//! `.properties`, `.ini` and simple YAML, read line by line. All three are
//! `key = value` / `key: value` lines with `#` (or `;`, `!`) comments;
//! `.ini` adds `[section]` headers and YAML nests by indent. Anything YAML
//! can do that isn't a plain value (`- ` lists, anchors, `|` blocks, maps
//! written inline) is skipped rather than guessed at, so it's never
//! rewritten wrongly.

use super::json_like::list_input;
use super::{Entry, Section, Value, hints, list_items, list_text, same};
use std::ops::Range;

/// What a bare value most likely is: `true`, `12`, `0.5`, else text. Kept
/// strict (`1.20.1` and `007` stay text) so writing it back can't change it.
pub(super) fn guess(raw: &str) -> Value {
    let t = raw.trim();
    if t.eq_ignore_ascii_case("true") {
        return Value::Bool(true);
    }
    if t.eq_ignore_ascii_case("false") {
        return Value::Bool(false);
    }
    let digits = t.strip_prefix(['-', '+']).unwrap_or(t);
    let whole = !digits.is_empty() && digits.bytes().all(|c| c.is_ascii_digit());
    if whole && (digits.len() == 1 || !digits.starts_with('0'))
        && let Ok(i) = t.trim_start_matches('+').parse::<i64>()
    {
        return Value::Int(i);
    }
    let (mant, exp) = match digits.find(['e', 'E']) {
        Some(p) => (&digits[..p], Some(&digits[p + 1..])),
        None => (digits, None),
    };
    let mant_ok = mant.matches('.').count() <= 1 && mant.bytes().any(|c| c.is_ascii_digit()) && mant.bytes().all(|c| c.is_ascii_digit() || c == b'.');
    let exp_ok = exp.is_none_or(|e| {
        let e = e.strip_prefix(['-', '+']).unwrap_or(e);
        !e.is_empty() && e.bytes().all(|c| c.is_ascii_digit())
    });
    if mant_ok && exp_ok && (mant.contains('.') || exp.is_some())
        && let Ok(f) = t.parse::<f64>()
    {
        return Value::Float(f);
    }
    Value::Str(t.to_string())
}

/// `true` written the way the file already writes switches (`True`, `TRUE`).
pub(super) fn bool_like(old: &str, b: bool) -> String {
    let s = b.to_string();
    if old.len() > 1 && old.chars().all(|c| c.is_ascii_uppercase()) {
        s.to_uppercase()
    } else if old.starts_with(|c: char| c.is_ascii_uppercase()) {
        let mut c = s.chars();
        c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default()
    } else {
        s
    }
}

/// One setting found in the file.
#[derive(Clone)]
struct Item {
    path: Vec<String>,
    key: String,
    value: Value,
    comment: Vec<String>,
    line: usize,
    /// The value's own characters (inside its quotes).
    span: Range<usize>,
    quote: Option<char>,
    /// Written `key: value` (YAML rules: `#` starts a comment, quoting).
    yaml: bool,
}

struct Scan {
    items: Vec<Item>,
    /// Every table in order, with the comment above its header.
    tables: Vec<(Vec<String>, String)>,
}

/// The lines of `text`: start, end without the line break.
pub(super) fn lines(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    for l in text.split_inclusive('\n') {
        let body = l.trim_end_matches('\n').trim_end_matches('\r');
        out.push((start, start + body.len()));
        start += l.len();
    }
    out
}

fn indent_of(l: &str) -> usize {
    l.len() - l.trim_start_matches([' ', '\t']).len()
}

fn is_comment(t: &str) -> bool {
    t.starts_with(['#', ';', '!'])
}

/// Where the key ends and the value starts: `=`, `: ` (YAML) or `:`.
/// The flag says whether YAML rules apply.
fn separator(l: &str) -> Option<(usize, usize, bool)> {
    let b = l.as_bytes();
    let colon = (0..b.len()).find(|&i| b[i] == b':' && b.get(i + 1).is_none_or(|c| *c == b' ' || *c == b'\t'));
    let eq = l.find('=');
    match (eq, colon) {
        (Some(e), c) if c.is_none_or(|c| e < c) => Some((e, e + 1, false)),
        (_, Some(c)) => Some((c, c + 1, true)),
        _ => l.find(':').map(|c| (c, c + 1, false)),
    }
}

/// A YAML double-quoted string from `s` (after its opening quote): the
/// text and the length used, closing quote included.
fn yaml_double(s: &str) -> Option<(String, usize)> {
    let mut out = String::new();
    let mut it = s.char_indices();
    while let Some((i, c)) = it.next() {
        match c {
            '"' => return Some((out, i + 1)),
            '\\' => match it.next()?.1 {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                '0' => out.push('\0'),
                o => out.push(o),
            },
            c => out.push(c),
        }
    }
    None
}

/// A YAML single-quoted string (`''` is one quote).
fn yaml_single(s: &str) -> Option<(String, usize)> {
    let mut out = String::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\'' {
            if b.get(i + 1) == Some(&b'\'') {
                out.push('\'');
                i += 2;
                continue;
            }
            return Some((out, i + 1));
        }
        let c = s[i..].chars().next()?;
        out.push(c);
        i += c.len_utf8();
    }
    None
}

fn scan(text: &str) -> Scan {
    let ls = lines(text);
    let mut items = Vec::new();
    let mut tables: Vec<(Vec<String>, String)> = vec![(Vec::new(), String::new())];
    let mut pending: Vec<String> = Vec::new();
    let mut base: Vec<String> = Vec::new();
    let mut stack: Vec<(usize, String)> = Vec::new();
    // Lines deeper than this belong to something skipped (a YAML list,
    // block text); a `- ` line at the same depth does too.
    let mut skip: Option<usize> = None;
    let mut n = 0;
    while n < ls.len() {
        let (start, end) = ls[n];
        let l = &text[start..end];
        let t = l.trim();
        let indent = indent_of(l);
        n += 1;
        if let Some(lim) = skip {
            if t.is_empty() || indent > lim || indent == lim && t.starts_with('-') {
                continue;
            }
            skip = None;
        }
        if t.is_empty() || t == "---" || t == "..." {
            pending.clear();
            continue;
        }
        if is_comment(t) {
            pending.push(t.trim_start_matches(['#', ';', '!']).trim().to_string());
            continue;
        }
        if t.starts_with('[') && t.ends_with(']') && !t.contains('=') && !t.contains(',') {
            base = vec![t[1..t.len() - 1].trim().to_string()];
            stack.clear();
            tables.push((base.clone(), hints::describe(&pending).text));
            pending.clear();
            continue;
        }
        if t == "-" || t.starts_with("- ") {
            // A list at the top or one under a key read earlier: not a setting.
            skip = Some(indent);
            pending.clear();
            continue;
        }
        while stack.last().is_some_and(|(i, _)| *i >= indent) {
            stack.pop();
        }
        let comment = std::mem::take(&mut pending);
        let body = &l[indent..];
        let body_at = start + indent;
        // The key: quoted (YAML), or up to the separator.
        let (key, rest_at, yaml) = if body.starts_with(['"', '\'']) {
            let q = body.as_bytes()[0];
            let parsed = if q == b'"' { yaml_double(&body[1..]) } else { yaml_single(&body[1..]) };
            let Some((k, used)) = parsed else { continue };
            let after = &body[1 + used..];
            let lead = after.len() - after.trim_start().len();
            match after.trim_start().as_bytes().first() {
                Some(b':') => (k, 1 + used + lead + 1, true),
                Some(b'=') => (k, 1 + used + lead + 1, false),
                _ => continue,
            }
        } else {
            let Some((k_end, v_at, yaml)) = separator(body) else { continue };
            (body[..k_end].trim().to_string(), v_at, yaml)
        };
        if key.is_empty() {
            continue;
        }
        let line = n;
        let rest = &body[rest_at..];
        let lead = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        let v_at = body_at + rest_at + lead;
        let mut raw = &rest[lead..];
        let path: Vec<String> = base.iter().cloned().chain(stack.iter().map(|(_, k)| k.clone())).collect();
        let push = |items: &mut Vec<Item>, value, span, quote| items.push(Item { path: path.clone(), key: key.clone(), value, comment: comment.clone(), line, span, quote, yaml });
        if !yaml {
            // Properties: a trailing backslash carries on to the next line.
            if raw.ends_with('\\') {
                while n < ls.len() && text[ls[n - 1].0..ls[n - 1].1].ends_with('\\') {
                    n += 1;
                }
                continue;
            }
            let v = raw.trim_end();
            let q = v.chars().next().filter(|c| (*c == '"' || *c == '\'') && v.len() >= 2 && v.ends_with(*c));
            match q {
                Some(q) => push(&mut items, Value::Str(v[1..v.len() - 1].to_string()), v_at + 1..v_at + v.len() - 1, Some(q)),
                None => push(&mut items, guess(v), v_at..v_at + v.len(), None),
            }
            continue;
        }
        // YAML from here.
        if raw.starts_with('"') || raw.starts_with('\'') {
            let q = raw.as_bytes()[0] as char;
            let parsed = if q == '"' { yaml_double(&raw[1..]) } else { yaml_single(&raw[1..]) };
            let Some((s, used)) = parsed else {
                // Quoted text running onto the next lines.
                skip = Some(indent);
                continue;
            };
            let after = raw[1 + used..].trim();
            if !after.is_empty() && !after.starts_with('#') {
                skip = Some(indent);
                continue;
            }
            push(&mut items, Value::Str(s), v_at + 1..v_at + used, Some(q));
            continue;
        }
        if let Some(c) = raw.find(" #").or_else(|| raw.find("\t#")) {
            raw = &raw[..c];
        }
        if raw.starts_with('#') {
            raw = "";
        }
        let raw = raw.trim_end();
        if raw.is_empty() {
            // `key:` alone: a table when deeper lines follow, a list when
            // `- ` lines do, else an empty value.
            let next = ls[n..].iter().map(|&(s, e)| &text[s..e]).find(|l| !l.trim().is_empty() && !is_comment(l.trim()));
            match next {
                Some(nl) if indent_of(nl) >= indent && nl.trim_start().starts_with('-') => skip = Some(indent),
                Some(nl) if indent_of(nl) > indent => {
                    let mut p = path.clone();
                    p.push(key.clone());
                    tables.push((p, hints::describe(&comment).text));
                    stack.push((indent, key.clone()));
                }
                _ => push(&mut items, Value::Str(String::new()), v_at..v_at, None),
            }
            continue;
        }
        if raw.starts_with(['&', '*', '!', '|', '>', '{', '%', '@', '`']) {
            skip = Some(indent);
            continue;
        }
        if raw.starts_with('[') {
            match list_items(raw) {
                Some(list) if raw.ends_with(']') => push(&mut items, Value::Text(list_text(&list, raw.contains(['"', '\'']))), v_at..v_at + raw.len(), None),
                // Nested, or running onto the next lines.
                _ => skip = Some(indent),
            }
            continue;
        }
        push(&mut items, guess(raw), v_at..v_at + raw.len(), None);
    }
    Scan { items, tables }
}

/// How often `path`/`key` is written. A key written twice is left out:
/// the game reads one copy and an edit could land on the other.
fn count(items: &[Item], path: &[String], key: &str) -> usize {
    items.iter().filter(|i| i.path == path && i.key == key).count()
}

pub(super) fn parse(text: &str) -> Result<Vec<Section>, String> {
    let s = scan(text);
    let mut out: Vec<Section> = Vec::new();
    for (path, comment) in s.tables {
        if !out.iter().any(|x| x.path == path) {
            out.push(Section { path, comment, entries: Vec::new() });
        }
    }
    for it in s.items.iter().filter(|i| count(&s.items, &i.path, &i.key) == 1).cloned() {
        let e = Entry::new(it.path.clone(), it.key, it.value, hints::describe(&it.comment), it.line);
        match out.iter_mut().find(|x| x.path == it.path) {
            Some(sec) => sec.entries.push(e),
            None => out.push(Section { path: it.path, comment: String::new(), entries: vec![e] }),
        }
    }
    Ok(out)
}

/// Whether YAML would read `s` unquoted as something other than this text.
fn yaml_needs_quotes(s: &str) -> bool {
    s.is_empty()
        || !matches!(guess(s), Value::Str(_))
        || s.trim() != s
        || s.starts_with(['&', '*', '!', '|', '>', '{', '[', '%', '@', '`', '"', '\'', '#', '?', ',', '-', ':'])
        || s.contains(": ")
        || s.contains(" #")
        || s.ends_with(':')
}

fn yaml_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\t', "\\t"))
}

pub(super) fn set(text: &str, path: &[String], key: &str, v: &Value) -> Result<String, String> {
    let s = scan(text);
    if count(&s.items, path, key) > 1 {
        return Err(format!("{key} is in the file more than once, so Hopper leaves it alone."));
    }
    let Some(it) = s.items.iter().find(|i| i.path == path && i.key == key) else {
        if !path.is_empty() && !s.tables.iter().any(|(p, _)| p == path) {
            return Err(format!("No [{}] in the file.", path.join(".")));
        }
        return Err(format!("No {key} in the file."));
    };
    let is_list = matches!(&it.value, Value::Text(t) if list_items(t).is_some());
    if it.value == *v || is_list && same(&it.value, v) {
        return Ok(text.to_string());
    }
    let old = &text[it.span.clone()];
    let one_line = |s: &str| if s.contains(['\n', '\r']) { Err("This setting's text has to fit on one line.".to_string()) } else { Ok(()) };
    let mut new = match v {
        Value::Bool(b) => bool_like(old, *b),
        Value::Int(i) => i.to_string(),
        Value::Float(f) if !f.is_finite() => return Err("Type a number.".into()),
        v @ Value::Float(_) => v.text(),
        Value::Str(s) => match (it.quote, it.yaml) {
            (Some('"'), true) => {
                let q = yaml_quote(s);
                q[1..q.len() - 1].to_string()
            }
            (Some(_), true) => s.replace('\'', "''"),
            (Some(q), false) => {
                one_line(s)?;
                if s.contains(q) {
                    return Err(format!("This setting's text can't contain {q}."));
                }
                s.clone()
            }
            (None, true) if yaml_needs_quotes(s) => yaml_quote(s),
            (None, _) => {
                one_line(s)?;
                s.clone()
            }
        },
        Value::Text(t) => match (is_list, list_input(t)) {
            (true, Some((list, quoted))) => {
                let toks: Vec<String> = list.iter().map(|i| if quoted { yaml_quote(i) } else { i.clone() }).collect();
                format!("[{}]", toks.join(", "))
            }
            _ => {
                one_line(t)?;
                t.trim().to_string()
            }
        },
    };
    // `key:` with nothing after it needs a space before the new value.
    if it.span.is_empty() && it.yaml && text[..it.span.start].ends_with(':') {
        new.insert(0, ' ');
    }
    let mut out = String::with_capacity(text.len() + new.len());
    out.push_str(&text[..it.span.start]);
    out.push_str(&new);
    out.push_str(&text[it.span.end..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::Kind;
    use super::*;

    fn entry<'a>(s: &'a [Section], path: &[&str], key: &str) -> &'a Entry {
        s.iter().flat_map(|s| &s.entries).find(|e| e.path == path && e.key == key).unwrap_or_else(|| panic!("no {key}"))
    }

    fn set1(t: &str, p: &[&str], k: &str, v: Value) -> String {
        set(t, &p.iter().map(|s| s.to_string()).collect::<Vec<_>>(), k, &v).unwrap()
    }

    const PROPS: &str = "#Minecraft server properties\n#Mon Oct 27 2026\nallow-flight=false\n! old style comment\n\n# How far players see\n# Range: 2 - 32\nview-distance=10\nmotd=A Minecraft Server \\u00A7\nlevel-seed=\nspawn-protection = 16\nratio:0.75\nversion=1.20.1\ncode=007\nquoted=\"keep me\"\nlong=one \\\n  two\nafter=1\n";

    #[test]
    fn properties() {
        let s = parse(PROPS).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(entry(&s, &[], "allow-flight").value, Value::Bool(false));
        let vd = entry(&s, &[], "view-distance");
        assert_eq!((&vd.value, vd.line, vd.range, vd.comment.as_str()), (&Value::Int(10), 8, Some((Some(2.0), Some(32.0))), "How far players see"));
        assert_eq!(entry(&s, &[], "motd").value, Value::Str("A Minecraft Server \\u00A7".into()));
        assert_eq!(entry(&s, &[], "level-seed").value, Value::Str(String::new()));
        assert_eq!(entry(&s, &[], "spawn-protection").value, Value::Int(16));
        assert_eq!(entry(&s, &[], "ratio").value, Value::Float(0.75));
        assert_eq!(entry(&s, &[], "version").value, Value::Str("1.20.1".into()));
        assert_eq!(entry(&s, &[], "code").value, Value::Str("007".into()));
        assert_eq!(entry(&s, &[], "quoted").value, Value::Str("keep me".into()));
        assert!(s[0].entries.iter().all(|e| e.key != "long" && e.key != "two"));
        assert_eq!(entry(&s, &[], "after").value, Value::Int(1));

        assert_eq!(set1(PROPS, &[], "allow-flight", Value::Bool(true)), PROPS.replace("allow-flight=false", "allow-flight=true"));
        assert_eq!(set1(PROPS, &[], "view-distance", Value::Int(12)), PROPS.replace("=10", "=12"));
        assert_eq!(set1(PROPS, &[], "level-seed", Value::Str("abc".into())), PROPS.replace("level-seed=", "level-seed=abc"));
        assert_eq!(set1(PROPS, &[], "spawn-protection", Value::Int(0)), PROPS.replace("= 16", "= 0"));
        assert_eq!(set1(PROPS, &[], "quoted", Value::Str("new".into())), PROPS.replace("\"keep me\"", "\"new\""));
        assert_eq!(set1(PROPS, &[], "motd", Value::Str("A Minecraft Server \\u00A7".into())), PROPS);
        assert!(set(PROPS, &[], "quoted", &Value::Str("a\"b".into())).is_err());
        assert!(set(PROPS, &[], "motd", &Value::Str("two\nlines".into())).is_err());
        assert!(set(PROPS, &[], "nope", &Value::Int(1)).unwrap_err().contains("No nope"));
    }

    const INI: &str = "; Global settings\r\nname = Steve\r\n\r\n; Rendering\r\n[render]\r\n; Requires a game restart\r\nfancy = TRUE\r\ndistance=8\r\n\r\n[sound]\r\nvolume = 0.5\r\n";

    #[test]
    fn ini_sections_and_crlf() {
        let s = parse(INI).unwrap();
        assert_eq!(s.iter().map(|s| s.path.join(".")).collect::<Vec<_>>(), ["", "render", "sound"]);
        assert_eq!(s[1].comment, "Rendering");
        assert_eq!(entry(&s, &[], "name").value, Value::Str("Steve".into()));
        let f = entry(&s, &["render"], "fancy");
        assert_eq!((&f.value, f.restart, f.line), (&Value::Bool(true), super::super::Restart::Game, 7));
        assert_eq!(entry(&s, &["sound"], "volume").value, Value::Float(0.5));
        assert_eq!(set1(INI, &["render"], "fancy", Value::Bool(false)), INI.replace("TRUE", "FALSE"));
        assert_eq!(set1(INI, &["sound"], "volume", Value::Float(1.0)), INI.replace("0.5", "1.0"));
        assert_eq!(set1(INI, &["render"], "distance", Value::Int(8)), INI);
        assert!(set(INI, &["nope".into()], "x", &Value::Int(1)).unwrap_err().contains("No [nope]"));
        // A list written as repeated keys (`\=modid`) isn't settings.
        let nav = "[visible]\n\\=herbalbrews\n\\=wildernature\nshown=true\n";
        assert_eq!(parse(nav).unwrap().iter().flat_map(|s| &s.entries).map(|e| e.key.as_str()).collect::<Vec<_>>(), ["shown"]);
        assert!(set(nav, &["visible".into()], "\\", &Value::Str("x".into())).unwrap_err().contains("more than once"));
    }

    const YAML: &str = "# Server settings\nenabled: true # on or off\nname: \"The Hub\"\nsingle: 'it''s'\ncount: 5\n\n# Messages shown to players\nmessages:\n  # Shown on join\n  welcome: Hello there\n  bye: 'Bye'\n  nested:\n    deep: 1.5\nworlds:\n  - world\n  - nether\nalso:\n- a\n- b\ntags: [a, b]\nquoted_tags: [\"x\", \"y\"]\nanchor: &base\n  a: 1\nblock: |\n  line one\n  line two\nempty:\nflow: {a: 1}\nlast: yes\n";

    #[test]
    fn yaml() {
        let s = parse(YAML).unwrap();
        let e = entry(&s, &[], "enabled");
        assert_eq!((&e.value, e.line), (&Value::Bool(true), 2));
        assert_eq!(entry(&s, &[], "name").value, Value::Str("The Hub".into()));
        assert_eq!(entry(&s, &[], "single").value, Value::Str("it's".into()));
        let w = entry(&s, &["messages"], "welcome");
        assert_eq!((&w.value, w.comment.as_str(), w.line), (&Value::Str("Hello there".into()), "Shown on join", 10));
        assert_eq!(s.iter().find(|s| s.path == ["messages"]).unwrap().comment, "Messages shown to players");
        assert_eq!(entry(&s, &["messages"], "bye").value, Value::Str("Bye".into()));
        assert_eq!(entry(&s, &["messages", "nested"], "deep").value, Value::Float(1.5));
        assert_eq!(entry(&s, &[], "tags").value, Value::Text("[a, b]".into()));
        assert_eq!(entry(&s, &[], "tags").kind(), Kind::List);
        assert_eq!(entry(&s, &[], "quoted_tags").value, Value::Text("[\"x\", \"y\"]".into()));
        assert_eq!(entry(&s, &[], "empty").value, Value::Str(String::new()));
        assert_eq!(entry(&s, &[], "last").value, Value::Str("yes".into()));
        let keys: Vec<&str> = s.iter().flat_map(|s| &s.entries).map(|e| e.key.as_str()).collect();
        for skipped in ["worlds", "also", "anchor", "a", "block", "flow", "line one"] {
            assert!(!keys.contains(&skipped), "{skipped} should be skipped");
        }

        assert_eq!(set1(YAML, &[], "enabled", Value::Bool(false)), YAML.replace("enabled: true", "enabled: false"));
        assert_eq!(set1(YAML, &[], "name", Value::Str("Say \"hi\"".into())), YAML.replace("\"The Hub\"", "\"Say \\\"hi\\\"\""));
        assert_eq!(set1(YAML, &[], "single", Value::Str("don't".into())), YAML.replace("'it''s'", "'don''t'"));
        assert_eq!(set1(YAML, &["messages"], "welcome", Value::Str("Hi".into())), YAML.replace("Hello there", "Hi"));
        // Text that YAML would read as something else gets quotes.
        assert_eq!(set1(YAML, &["messages"], "welcome", Value::Str("123".into())), YAML.replace("Hello there", "\"123\""));
        assert_eq!(set1(YAML, &["messages"], "welcome", Value::Str("a: b".into())), YAML.replace("Hello there", "\"a: b\""));
        assert_eq!(set1(YAML, &["messages", "nested"], "deep", Value::Float(2.5)), YAML.replace("1.5", "2.5"));
        assert_eq!(set1(YAML, &[], "tags", Value::Text("[a, b, c]".into())), YAML.replace("[a, b]", "[a, b, c]"));
        assert_eq!(set1(YAML, &[], "quoted_tags", Value::Text("[\"z\"]".into())), YAML.replace("[\"x\", \"y\"]", "[\"z\"]"));
        assert_eq!(set1(YAML, &[], "empty", Value::Str("now".into())), YAML.replace("empty:\n", "empty: now\n"));
        assert_eq!(set1(YAML, &[], "tags", Value::Text("[ a,b ]".into())), YAML);
        assert!(set(YAML, &[], "worlds", &Value::Int(1)).is_err());
        // The new text reads back.
        let out = set1(YAML, &["messages"], "welcome", Value::Str("a: b".into()));
        assert_eq!(entry(&parse(&out).unwrap(), &["messages"], "welcome").value, Value::Str("a: b".into()));
    }

    /// Every `.cfg`, `.snbt`, JSON, `.properties`, YAML and `.ini` under the
    /// instance in `HOPPER_CONFIG_ROOT` (`config/`, `defaultconfigs/`):
    /// reads, setting each value to itself changes nothing, and flipping a
    /// switch changes that value and nothing else. Read only.
    #[test]
    #[ignore]
    fn real_configs() {
        use super::super::{Format, parse_as, set_as};
        use std::collections::BTreeMap;
        let Ok(root) = std::env::var("HOPPER_CONFIG_ROOT") else { return };
        let root = std::path::PathBuf::from(root);
        let mut stats: BTreeMap<String, (usize, usize, usize, usize)> = BTreeMap::new();
        let mut bad = Vec::new();
        let mut stack = vec![root.join("config"), root.join("defaultconfigs")];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                let name = p.to_string_lossy().to_string();
                let Some(f) = Format::of(&name).filter(|f| *f != Format::Toml) else { continue };
                let Ok(text) = std::fs::read_to_string(&p) else { continue };
                let ext = p.extension().map(|x| x.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
                let st = stats.entry(ext).or_default();
                st.0 += 1;
                let secs = match parse_as(f, &text) {
                    Ok(s) => s,
                    Err(e) => {
                        st.3 += 1;
                        bad.push(format!("{name}: {e}"));
                        continue;
                    }
                };
                // Every edit re-reads the file, so a big data file is only
                // sampled: its first settings, which still cover the reader.
                for en in secs.iter().flat_map(|s| &s.entries).take(if text.len() > 64 * 1024 { 20 } else { 400 }) {
                    st.1 += 1;
                    match set_as(f, &text, &en.path, &en.key, &en.value) {
                        Ok(out) if out == text => {}
                        Ok(_) => bad.push(format!("{name}: {} changed on a no-op", en.key)),
                        Err(e) => bad.push(format!("{name}: {}: {e}", en.key)),
                    }
                    if let Value::Bool(b) = en.value {
                        st.2 += 1;
                        match set_as(f, &text, &en.path, &en.key, &Value::Bool(!b)) {
                            Ok(out) => {
                                let again = parse_as(f, &out).ok().and_then(|ss| ss.into_iter().flat_map(|s| s.entries).find(|x| x.path == en.path && x.key == en.key && x.line == en.line));
                                let delta = out.len() as i64 - text.len() as i64;
                                let pre = text.bytes().zip(out.bytes()).take_while(|(a, b)| a == b).count();
                                let post = text.bytes().rev().zip(out.bytes().rev()).take_while(|(a, b)| a == b).count();
                                // One stretch of at most 5 bytes changed (`true` -> `false`).
                                let one_spot = pre + post + 5 >= text.len();
                                if again.map(|x| x.value) != Some(Value::Bool(!b)) || delta.abs() != 1 || !one_spot {
                                    bad.push(format!("{name}: {} flip went wrong", en.key));
                                }
                            }
                            Err(e) => bad.push(format!("{name}: {} flip: {e}", en.key)),
                        }
                    }
                }
            }
        }
        for (ext, (files, entries, flips, failed)) in &stats {
            println!("{ext}: {files} files, {entries} settings, {flips} switches, {failed} unreadable");
        }
        println!("{} problems", bad.len());
        for b in bad.iter().filter(|b| !b.contains("Something follows") && !b.contains("never closed") && !b.contains("Unexpected") && !b.contains("doesn't start with")).take(80) {
            println!("  {b}");
        }
    }

    #[test]
    fn guesses() {
        assert_eq!(guess("12"), Value::Int(12));
        assert_eq!(guess("-3"), Value::Int(-3));
        assert_eq!(guess("0"), Value::Int(0));
        assert_eq!(guess("1e3"), Value::Float(1000.0));
        assert_eq!(guess(".5"), Value::Float(0.5));
        assert_eq!(guess("inf"), Value::Str("inf".into()));
        assert_eq!(guess("NaN"), Value::Str("NaN".into()));
        assert_eq!(guess("1.2.3"), Value::Str("1.2.3".into()));
        assert_eq!(guess("False"), Value::Bool(false));
        assert_eq!(bool_like("True", false), "False");
        assert_eq!(bool_like("true", false), "false");
    }

    #[test]
    fn flipping_every_switch() {
        for t in [PROPS, INI, YAML] {
            let mut text = t.to_string();
            let all: Vec<_> = parse(t).unwrap().into_iter().flat_map(|s| s.entries).collect();
            let mut flipped = 0;
            for e in &all {
                let Value::Bool(b) = e.value else { continue };
                text = set(&text, &e.path, &e.key, &Value::Bool(!b)).unwrap();
                let again = parse(&text).unwrap();
                assert_eq!(entry(&again, &e.path.iter().map(String::as_str).collect::<Vec<_>>(), &e.key).value, Value::Bool(!b));
                flipped += 1;
            }
            assert!(flipped > 0);
            assert_eq!(parse(&text).unwrap().iter().flat_map(|s| &s.entries).count(), all.len());
        }
    }
}
