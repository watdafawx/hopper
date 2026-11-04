//! Old Forge `.cfg` files and the plain `key=value` `.cfg`s some mods
//! write. Forge's own: `# comment` lines, categories `general { ... }`
//! (nested, names may be quoted), typed keys `I:"Max Level"=4`,
//! `B:enabled=true`, and lists `S:names <` one item per line `>`. Also
//! read: HammerLib's `C "Client"={` / `I "Port"=0` with `/* */` comments,
//! Carbon Config's `[general]` headers, and Drippy's `B:x = 'true';`. A
//! `.cfg` that is really JSON is read as JSON.

use super::flat::{bool_like, guess, lines};
use super::json_like::{self, list_input};
use super::{Entry, Section, Value, hints, list_text, same};
use std::ops::Range;

#[derive(Clone)]
enum Spot {
    /// The value's own characters (inside its quotes).
    Scalar { span: Range<usize>, quote: Option<char> },
    /// The lines between `<` and `>`: one item per line.
    List { body: Range<usize>, indent: String, eol: &'static str },
}

#[derive(Clone)]
struct Item {
    path: Vec<String>,
    key: String,
    value: Value,
    comment: Vec<String>,
    line: usize,
    spot: Spot,
}

struct Scan {
    items: Vec<Item>,
    tables: Vec<(Vec<String>, String)>,
}

/// Where `sub` (a slice of `text`) starts in it.
fn off(text: &str, sub: &str) -> usize {
    sub.as_ptr() as usize - text.as_ptr() as usize
}

/// `"x"` without its quotes, if it's quoted.
fn unquote(s: &str) -> Option<&str> {
    let s = s.trim();
    (s.len() >= 2 && s.starts_with('"') && s.ends_with('"')).then(|| &s[1..s.len() - 1])
}

/// The value of a typed key: `I` and `L` whole numbers, `D` and `F`
/// numbers, `B` switches, `S` (and anything else) text. A value that
/// doesn't fit its letter is read as it looks rather than refused.
fn typed(prefix: Option<char>, raw: &str, quoted: bool) -> Value {
    match prefix.map(|c| c.to_ascii_uppercase()) {
        Some('B') if raw.eq_ignore_ascii_case("true") || raw.eq_ignore_ascii_case("false") => Value::Bool(raw.eq_ignore_ascii_case("true")),
        Some('I' | 'L') => raw.parse().map(Value::Int).unwrap_or_else(|_| guess(raw)),
        Some('D' | 'F') => match guess(raw) {
            Value::Int(i) => Value::Float(i as f64),
            v => v,
        },
        Some(_) => Value::Str(raw.to_string()),
        None if quoted => Value::Str(raw.to_string()),
        None => guess(raw),
    }
}

/// Whether a list's items are numbers or switches (written bare in the
/// editor), from the key's letter.
fn bare_items(prefix: Option<char>) -> bool {
    matches!(prefix.map(|c| c.to_ascii_uppercase()), Some('I' | 'L' | 'D' | 'F' | 'B'))
}

fn scan(text: &str) -> Result<Scan, String> {
    let ls = lines(text);
    let mut items = Vec::new();
    let mut tables: Vec<(Vec<String>, String)> = vec![(Vec::new(), String::new())];
    let mut pending: Vec<String> = Vec::new();
    let mut base: Vec<String> = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut in_block = false;
    let mut n = 0;
    while n < ls.len() {
        let (start, end) = ls[n];
        let l = &text[start..end];
        let t = l.trim();
        n += 1;
        if in_block {
            let (body, done) = match t.find("*/") {
                Some(e) => (&t[..e], true),
                None => (t, false),
            };
            pending.push(body.trim().trim_start_matches('*').trim().to_string());
            in_block = !done;
            continue;
        }
        if t.is_empty() {
            pending.clear();
            continue;
        }
        // Drippy's `##[general]` and Carbon's `[general]` start a table;
        // other bracketed lines are Drippy's comments (`[What it does.]`).
        let header = t.strip_prefix("##").unwrap_or(t);
        if header.starts_with('[') && header.ends_with(']') {
            let inner = header[1..header.len() - 1].trim();
            if !inner.is_empty() && !inner.contains(char::is_whitespace) && !inner.contains('=') {
                base = vec![inner.to_string()];
                stack.clear();
                tables.push((joined(&base, &stack), hints::describe(&pending).text));
                pending.clear();
            } else if !inner.contains('=') {
                pending.push(inner.to_string());
            }
            continue;
        }
        if t.starts_with('#') || t.starts_with("//") {
            pending.push(t.trim_start_matches(['#', '/']).trim().to_string());
            continue;
        }
        if let Some(r) = t.strip_prefix("/*") {
            match r.find("*/") {
                Some(e) => pending.push(r[..e].trim().to_string()),
                None => {
                    pending.push(r.trim().to_string());
                    in_block = true;
                }
            }
            continue;
        }
        if t.starts_with('}') {
            stack.pop();
            pending.clear();
            continue;
        }
        if let Some(head) = t.strip_suffix('{') {
            let head = head.trim();
            // HammerLib writes `C "Name"={`, Forge `name {` or `"my name" {`.
            let name = match head.strip_prefix("C:").or_else(|| head.strip_prefix("C ")) {
                Some(r) => {
                    let r = r.trim().trim_end_matches('=').trim();
                    Some(unquote(r).unwrap_or(r))
                }
                None => unquote(head).or((!head.is_empty() && !head.contains(['=', ':', '"'])).then_some(head)),
            };
            if let Some(name) = name {
                stack.push(name.to_string());
                tables.push((joined(&base, &stack), hints::describe(&pending).text));
                pending.clear();
                continue;
            }
        }
        // A setting: `T:key=value`, `T "key"=value`, `key=value`, `T:key <`.
        let b = t.as_bytes();
        let (prefix, rest) = if b.len() > 2 && b[0].is_ascii_alphabetic() && (b[1] == b':' || b[1] == b' ' && b[2] == b'"') {
            (Some(b[0] as char), &t[2..])
        } else {
            (None, t)
        };
        let (key, after) = if let Some(r) = rest.strip_prefix('"') {
            let Some(q) = r.find('"') else {
                pending.clear();
                continue;
            };
            (&r[..q], r[q + 1..].trim_start())
        } else if !rest.contains('=') && rest.ends_with('<') {
            (rest[..rest.len() - 1].trim(), "<")
        } else if let Some(e) = rest.find('=') {
            (rest[..e].trim(), &rest[e..])
        } else {
            pending.clear();
            continue;
        };
        if key.is_empty() {
            pending.clear();
            continue;
        }
        let comment = std::mem::take(&mut pending);
        let line = n;
        if after.trim() == "<" {
            let key_indent = &l[..l.len() - l.trim_start().len()];
            let eol = if text[end..].starts_with("\r\n") { "\r\n" } else { "\n" };
            let body_start = ls.get(n).map_or(text.len(), |x| x.0);
            let mut list = Vec::new();
            let mut indent = None;
            let mut close = None;
            while n < ls.len() {
                let (s, e) = ls[n];
                let il = &text[s..e];
                n += 1;
                if il.trim().starts_with('>') {
                    close = Some(s);
                    break;
                }
                if !il.trim().is_empty() {
                    indent.get_or_insert_with(|| il[..il.len() - il.trim_start().len()].to_string());
                    list.push(il.trim().to_string());
                }
            }
            let Some(close) = close else { return Err(format!("The list {key} is never closed with '>'.")) };
            let value = Value::Text(list_text(&list, !bare_items(prefix)));
            let indent = indent.unwrap_or_else(|| format!("{key_indent}    "));
            items.push(Item { path: joined(&base, &stack), key: key.to_string(), value, comment, line, spot: Spot::List { body: body_start..close, indent, eol } });
            continue;
        }
        let Some(v) = after.strip_prefix('=') else { continue };
        let mut raw = v.trim();
        // Drippy ends values with `;` after quotes: `'true';`.
        if let Some(r) = raw.strip_suffix(';') {
            let r = r.trim_end();
            if r.len() >= 2 && (r.starts_with('\'') && r.ends_with('\'') || r.starts_with('"') && r.ends_with('"')) {
                raw = r;
            }
        }
        let q = raw.chars().next().filter(|c| (*c == '"' || *c == '\'') && raw.len() >= 2 && raw.ends_with(*c));
        let inner = if q.is_some() { &raw[1..raw.len() - 1] } else { raw };
        let at = off(text, inner);
        let value = typed(prefix, inner, q.is_some());
        items.push(Item { path: joined(&base, &stack), key: key.to_string(), value, comment, line, spot: Spot::Scalar { span: at..at + inner.len(), quote: q } });
    }
    Ok(Scan { items, tables })
}

fn joined(a: &[String], b: &[String]) -> Vec<String> {
    a.iter().chain(b).cloned().collect()
}

fn is_json(text: &str) -> bool {
    text.trim_start_matches('\u{feff}').trim_start().starts_with('{')
}

/// How often `path`/`key` is written. A key written twice is left out:
/// the game reads one copy and an edit could land on the other.
fn count(items: &[Item], path: &[String], key: &str) -> usize {
    items.iter().filter(|i| i.path == path && i.key == key).count()
}

pub(super) fn parse(text: &str) -> Result<Vec<Section>, String> {
    if is_json(text) {
        return json_like::parse(text);
    }
    let s = scan(text)?;
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

pub(super) fn set(text: &str, path: &[String], key: &str, v: &Value) -> Result<String, String> {
    if is_json(text) {
        return json_like::set(text, path, key, v);
    }
    let s = scan(text)?;
    if count(&s.items, path, key) > 1 {
        return Err(format!("{key} is in the file more than once, so Hopper leaves it alone."));
    }
    let Some(it) = s.items.iter().find(|i| i.path == path && i.key == key) else {
        if !path.is_empty() && !s.tables.iter().any(|(p, _)| p == path) {
            return Err(format!("No {} in the file.", path.join(" > ")));
        }
        return Err(format!("No {key} in the file."));
    };
    let one_line = |s: &str| if s.contains(['\n', '\r']) { Err("This setting's text has to fit on one line.".to_string()) } else { Ok(()) };
    let (span, new) = match &it.spot {
        Spot::List { body, indent, eol } => {
            if same(&it.value, v) {
                return Ok(text.to_string());
            }
            let Value::Text(t) = v else { return Err(format!("{key} is a list.")) };
            let (list, _) = list_input(t).ok_or("Type a list like [\"a\", \"b\"].")?;
            let mut new = String::new();
            for i in &list {
                one_line(i)?;
                // Forge trims each line, so an empty item would vanish.
                if i.trim().is_empty() {
                    continue;
                }
                new.push_str(indent);
                new.push_str(i.trim());
                new.push_str(eol);
            }
            (body.clone(), new)
        }
        Spot::Scalar { span, quote } => {
            if it.value == *v {
                return Ok(text.to_string());
            }
            let old = &text[span.clone()];
            let new = match v {
                Value::Bool(b) => bool_like(old, *b),
                Value::Int(i) => i.to_string(),
                Value::Float(f) if !f.is_finite() => return Err("Type a number.".into()),
                v @ Value::Float(_) => v.text(),
                Value::Str(s) | Value::Text(s) => {
                    one_line(s)?;
                    if let Some(q) = quote
                        && s.contains(*q)
                    {
                        return Err(format!("This setting's text can't contain {q}."));
                    }
                    s.clone()
                }
            };
            (span.clone(), new)
        }
    };
    let mut out = String::with_capacity(text.len() + new.len());
    out.push_str(&text[..span.start]);
    out.push_str(&new);
    out.push_str(&text[span.end..]);
    // The edit must leave a file that still reads.
    scan(&out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::{Kind, Restart};
    use super::*;

    fn entry<'a>(s: &'a [Section], path: &[&str], key: &str) -> &'a Entry {
        s.iter().flat_map(|s| &s.entries).find(|e| e.path == path && e.key == key).unwrap_or_else(|| panic!("no {key}"))
    }

    fn set1(t: &str, p: &[&str], k: &str, v: Value) -> String {
        set(t, &p.iter().map(|s| s.to_string()).collect::<Vec<_>>(), k, &v).unwrap()
    }

    const FORGE: &str = "# Configuration file\n\nEmitter {\n    # If true, the smoke goes aside. [default: true]\n    B:canEmitAside=true\n\n    # Defines a color in ARGB hex format.\n    #  [default: 0xffffffff]\n    S:color=0xffffffff\n\n    # Defines the maximum air gap. [range: 0 ~ 16, default: 2]\n    I:maxGapLength=2\n\n    # Defines a list of blocks.\n    S:relatedBlocks <\n     >\n\n    # Particle amount [range: 0.0 ~ 16.0, default: 2.0]\n    D:amount=2.0\n\n    inner {\n        S:names <\n            Biscuit\n            Michael Morbius\n         >\n        I:levels <\n            1\n            2\n         >\n    }\n}\n\n\n\"minecraft:aqua_affinity\" {\n    # The max level of this enchantment - originally 1.\n    # Default: 1; Range: [1 ~ 127]\n    I:\"Max Level\"=1\n\n    # Default: \n    S:\"Max Power Function\"=\n}\n";

    #[test]
    fn reads_forge_cfg() {
        let s = parse(FORGE).unwrap();
        let a = entry(&s, &["Emitter"], "canEmitAside");
        assert_eq!((&a.value, a.line, a.default.as_deref(), a.comment.as_str()), (&Value::Bool(true), 5, Some("true"), "If true, the smoke goes aside."));
        assert_eq!(entry(&s, &["Emitter"], "color").value, Value::Str("0xffffffff".into()));
        assert_eq!(entry(&s, &["Emitter"], "color").default.as_deref(), Some("0xffffffff"));
        let g = entry(&s, &["Emitter"], "maxGapLength");
        assert_eq!((&g.value, g.range, g.is_default()), (&Value::Int(2), Some((Some(0.0), Some(16.0))), Some(true)));
        assert_eq!(entry(&s, &["Emitter"], "relatedBlocks").value, Value::Text("[]".into()));
        assert_eq!(entry(&s, &["Emitter"], "amount").value, Value::Float(2.0));
        let names = entry(&s, &["Emitter", "inner"], "names");
        assert_eq!((&names.value, names.kind(), names.line), (&Value::Text("[\"Biscuit\", \"Michael Morbius\"]".into()), Kind::List, 22));
        assert_eq!(entry(&s, &["Emitter", "inner"], "levels").value, Value::Text("[1, 2]".into()));
        let m = entry(&s, &["minecraft:aqua_affinity"], "Max Level");
        assert_eq!((&m.value, m.range, m.default.as_deref()), (&Value::Int(1), Some((Some(1.0), Some(127.0))), Some("1")));
        assert_eq!(entry(&s, &["minecraft:aqua_affinity"], "Max Power Function").value, Value::Str(String::new()));
    }

    #[test]
    fn edits_forge_cfg() {
        assert_eq!(set1(FORGE, &["Emitter"], "canEmitAside", Value::Bool(false)), FORGE.replace("canEmitAside=true", "canEmitAside=false"));
        assert_eq!(set1(FORGE, &["Emitter"], "maxGapLength", Value::Int(5)), FORGE.replace("maxGapLength=2", "maxGapLength=5"));
        assert_eq!(set1(FORGE, &["Emitter"], "amount", Value::Float(3.5)), FORGE.replace("amount=2.0", "amount=3.5"));
        assert_eq!(set1(FORGE, &["minecraft:aqua_affinity"], "Max Level", Value::Int(3)), FORGE.replace("\"Max Level\"=1", "\"Max Level\"=3"));
        assert_eq!(set1(FORGE, &["minecraft:aqua_affinity"], "Max Power Function", Value::Str("x*2".into())), FORGE.replace("Function\"=\n", "Function\"=x*2\n"));
        assert_eq!(
            set1(FORGE, &["Emitter"], "relatedBlocks", Value::Text("[\"minecraft:stone\", \"minecraft:dirt\"]".into())),
            FORGE.replace("S:relatedBlocks <\n     >", "S:relatedBlocks <\n        minecraft:stone\n        minecraft:dirt\n     >")
        );
        assert_eq!(
            set1(FORGE, &["Emitter", "inner"], "names", Value::Text("[\"Steve\"]".into())),
            FORGE.replace("            Biscuit\n            Michael Morbius\n", "            Steve\n")
        );
        assert_eq!(set1(FORGE, &["Emitter", "inner"], "levels", Value::Text("[]".into())), FORGE.replace("            1\n            2\n", ""));
        assert_eq!(set1(FORGE, &["Emitter"], "maxGapLength", Value::Int(2)), FORGE);
        assert_eq!(set1(FORGE, &["Emitter", "inner"], "names", Value::Text("[\"Biscuit\",\"Michael Morbius\"]".into())), FORGE);
        assert!(set(FORGE, &["Emitter".into()], "color", &Value::Str("a\nb".into())).is_err());
        assert!(set(FORGE, &["Nope".into()], "x", &Value::Int(1)).unwrap_err().contains("No Nope"));
        assert!(parse("S:open <\n a\n").is_err());
    }

    #[test]
    fn plain_and_other_dialects() {
        let mouse = "RMBTweak=1\r\nWheelTweak=1\r\nname = foo\r\nDebug=0\r\n";
        let s = parse(mouse).unwrap();
        assert_eq!(entry(&s, &[], "RMBTweak").value, Value::Int(1));
        assert_eq!(entry(&s, &[], "name").value, Value::Str("foo".into()));
        assert_eq!(set1(mouse, &[], "WheelTweak", Value::Int(0)), mouse.replace("WheelTweak=1", "WheelTweak=0"));
        assert_eq!(set1(mouse, &[], "name", Value::Str("bar".into())), mouse.replace("foo", "bar"));

        let hammer = "[CFG=1.0.0]\n\n/* Client-side features */\nC \"Client-Side\"={\n\t/* Item render size (Range: [16; 16384]) */\n\tI \"Gui Item Render Resolution\"=1024\n\t\n\t/*\n\t\tEnable stencils?\n\t\t-1 never, 1 always (Range: [-1; 1])\n\t*/\n\tI \"OpenGL Stencil\"=0\n\tD \"Height\"=6.0\n}\n";
        let s = parse(hammer).unwrap();
        assert_eq!(s.iter().find(|s| s.path == ["Client-Side"]).unwrap().comment, "Client-side features");
        let r = entry(&s, &["Client-Side"], "Gui Item Render Resolution");
        assert_eq!((&r.value, r.range, r.comment.as_str()), (&Value::Int(1024), Some((Some(16.0), Some(16384.0))), "Item render size"));
        let st = entry(&s, &["Client-Side"], "OpenGL Stencil");
        assert_eq!((st.range, st.line), (Some((Some(-1.0), Some(1.0))), 12));
        assert_eq!(set1(hammer, &["Client-Side"], "Height", Value::Float(8.0)), hammer.replace("6.0", "8.0"));

        let carbon = "[general]\n\n  # Enables forge support\n  #\u{200b} Requires Restart\n  B:enable-forge-support=true\n\n  #\u{200b} Must be one of [PLANKS, STONE, DIRT]\n  E:custom-background=PLANKS\n";
        let s = parse(carbon).unwrap();
        let f = entry(&s, &["general"], "enable-forge-support");
        assert_eq!((&f.value, f.restart, f.comment.as_str()), (&Value::Bool(true), Restart::Game, "Enables forge support"));
        assert_eq!(entry(&s, &["general"], "custom-background").kind(), Kind::Choice(vec!["PLANKS".into(), "STONE".into(), "DIRT".into()]));

        let drippy = "##[general]\n\nB:early_fade_out_elements = 'true';\n\n##[forge]\n\n[If you disable this, Drippy turns off the early screen.]\nB:enable_early_loading = 'true';\nS:resource_path = 'resources/';\n";
        let s = parse(drippy).unwrap();
        assert_eq!(entry(&s, &["general"], "early_fade_out_elements").value, Value::Bool(true));
        let e = entry(&s, &["forge"], "enable_early_loading");
        assert_eq!((&e.value, e.comment.as_str()), (&Value::Bool(true), "If you disable this, Drippy turns off the early screen."));
        assert_eq!(entry(&s, &["forge"], "resource_path").value, Value::Str("resources/".into()));
        assert_eq!(set1(drippy, &["forge"], "enable_early_loading", Value::Bool(false)), drippy.replace("loading = 'true'", "loading = 'false'"));
        assert!(set(drippy, &["forge".into()], "resource_path", &Value::Str("it's".into())).is_err());

        let json = "{\n  \"Description\": \"Stores the layout\",\n  \"on\": true\n}";
        assert_eq!(entry(&parse(json).unwrap(), &[], "on").value, Value::Bool(true));
        assert_eq!(set1(json, &[], "on", Value::Bool(false)), json.replace("true", "false"));
    }

    #[test]
    fn crlf_and_flips() {
        let t = FORGE.replace('\n', "\r\n");
        let out = set1(&t, &["Emitter", "inner"], "names", Value::Text("[\"a\", \"b\", \"c\"]".into()));
        assert_eq!(out, t.replace("            Biscuit\r\n            Michael Morbius\r\n", "            a\r\n            b\r\n            c\r\n"));
        let mut text = t.clone();
        let all: Vec<_> = parse(&t).unwrap().into_iter().flat_map(|s| s.entries).collect();
        for e in &all {
            let Value::Bool(b) = e.value else { continue };
            text = set(&text, &e.path, &e.key, &Value::Bool(!b)).unwrap();
            assert_eq!(entry(&parse(&text).unwrap(), &e.path.iter().map(String::as_str).collect::<Vec<_>>(), &e.key).value, Value::Bool(!b));
        }
        assert_eq!(text, t.replace("canEmitAside=true", "canEmitAside=false"));
    }
}
