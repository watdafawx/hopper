//! JSON, JSON5 and JSONC. A small hand-written reader keeps where every
//! value sits in the text, so an edit swaps only that value's characters
//! and every comment, comma and indent stays as it was. FTB's SNBT is close
//! enough that `snbt.rs` reads through the same tree ([`Dialect::Snbt`]).

use super::{Entry, Section, Value, hints, list_items};
use std::ops::Range;

/// Which spelling of the file is being read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Dialect {
    Json,
    Snbt,
}

/// One `key: value` in an object.
#[derive(Debug)]
pub(super) struct Member {
    pub key: String,
    /// 1-based line of the key.
    pub line: usize,
    /// The comment lines above it (and any on the same line after it).
    pub comment: Vec<String>,
    pub value: Node,
}

/// A value in the file, with where it sits.
#[derive(Debug)]
pub(super) enum Node {
    Object { members: Vec<Member> },
    /// `prefix` is SNBT's typed-array letter (`[I; 1, 2]`).
    Array { items: Vec<Node>, span: Range<usize>, prefix: Option<char> },
    /// `span` covers the whole token, quotes and number suffix included.
    Scalar { value: Value, span: Range<usize> },
}

impl Node {
    fn span(&self) -> Option<Range<usize>> {
        match self {
            Node::Object { .. } => None,
            Node::Array { span, .. } | Node::Scalar { span, .. } => Some(span.clone()),
        }
    }
}

/// Comments read between two values: those on the same line as the value
/// before (they describe it) and those on their own lines (they describe
/// the next one).
#[derive(Default)]
struct Trivia {
    trailing: Vec<String>,
    above: Vec<String>,
}

struct Reader<'a> {
    s: &'a str,
    b: &'a [u8],
    i: usize,
    depth: usize,
    d: Dialect,
    lines: Vec<usize>,
}

/// Deeper than this is not a config anyone edits by hand, and would risk
/// running out of stack.
const MAX_DEPTH: usize = 256;

impl<'a> Reader<'a> {
    fn new(s: &'a str, d: Dialect) -> Reader<'a> {
        let lines = std::iter::once(0).chain(s.match_indices('\n').map(|(i, _)| i + 1)).collect();
        let i = if s.starts_with('\u{feff}') { 3 } else { 0 };
        Reader { s, b: s.as_bytes(), i, depth: 0, d, lines }
    }

    fn line(&self, at: usize) -> usize {
        match self.lines.binary_search(&at) {
            Ok(n) => n + 1,
            Err(n) => n,
        }
    }

    fn err(&self, what: &str) -> String {
        format!("{what} (line {}).", self.line(self.i))
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn at(&self, p: &str) -> bool {
        self.s[self.i..].starts_with(p)
    }

    /// Skips spaces and comments. Comments before the first line break
    /// count as trailing (they sit after the previous value); a blank line
    /// drops the ones above it, since those belong to nothing below.
    fn trivia(&mut self) -> Trivia {
        let mut t = Trivia::default();
        let mut newlines = 0usize;
        let mut seen_break = false;
        while let Some(c) = self.peek() {
            if c == b'\n' {
                newlines += 1;
                seen_break = true;
                if newlines >= 2 {
                    t.above.clear();
                }
                self.i += 1;
            } else if c.is_ascii_whitespace() {
                self.i += 1;
            } else if c == b'#' || self.at("//") {
                let start = self.i + if c == b'#' { 1 } else { 2 };
                let end = self.s[start..].find('\n').map_or(self.s.len(), |e| start + e);
                let line = self.s[start..end].trim().to_string();
                if seen_break { t.above.push(line) } else { t.trailing.push(line) }
                self.i = end;
                newlines = 0;
            } else if self.at("/*") {
                let start = self.i + 2;
                let end = self.s[start..].find("*/").map_or(self.s.len(), |e| start + e);
                let lines = self.s[start..end].lines().map(|l| l.trim().trim_start_matches('*').trim().to_string());
                if seen_break { t.above.extend(lines) } else { t.trailing.extend(lines) }
                self.i = (end + 2).min(self.s.len());
                newlines = 0;
            } else {
                break;
            }
        }
        t
    }

    fn string(&mut self) -> Result<String, String> {
        let q = self.b[self.i];
        let start_line = self.line(self.i);
        self.i += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.s[self.i..].chars().next() else {
                return Err(format!("A text value never ends (line {start_line})."));
            };
            self.i += c.len_utf8();
            if c as u32 == q as u32 {
                return Ok(out);
            }
            if c != '\\' {
                out.push(c);
                continue;
            }
            let Some(e) = self.s[self.i..].chars().next() else { continue };
            self.i += e.len_utf8();
            match e {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                // JSON5 lets a backslash carry a string onto the next line.
                '\n' => {}
                '\r' => {
                    if self.peek() == Some(b'\n') {
                        self.i += 1;
                    }
                }
                'u' => {
                    let hex = |r: &mut Self| -> Option<u32> {
                        let h = r.s.get(r.i..r.i + 4)?;
                        let v = u32::from_str_radix(h, 16).ok()?;
                        r.i += 4;
                        Some(v)
                    };
                    let Some(hi) = hex(self) else {
                        out.push_str("\\u");
                        continue;
                    };
                    let cp = if (0xD800..0xDC00).contains(&hi) && self.at("\\u") {
                        self.i += 2;
                        match hex(self) {
                            Some(lo) if (0xDC00..0xE000).contains(&lo) => 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00),
                            _ => 0xFFFD,
                        }
                    } else {
                        hi
                    };
                    out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                }
                o => out.push(o),
            }
        }
    }

    /// A bare word: a JSON5 key, a number, `true`, an SNBT unquoted string.
    fn word(&mut self) -> &'a str {
        let start = self.i;
        while let Some(c) = self.peek() {
            if c.is_ascii_whitespace() || b",:[]{}\"'".contains(&c) || self.at("//") || self.at("/*") || (c == b'#' && self.i == start) {
                break;
            }
            self.i += 1;
        }
        &self.s[start..self.i]
    }

    fn key(&mut self) -> Result<String, String> {
        match self.peek() {
            Some(b'"' | b'\'') => self.string(),
            _ => {
                let w = self.word();
                if w.is_empty() { Err(self.err("Expected a setting name")) } else { Ok(w.to_string()) }
            }
        }
    }

    fn object(&mut self) -> Result<Vec<Member>, String> {
        self.i += 1;
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("The file is nested too deeply to read.".into());
        }
        let mut members: Vec<Member> = Vec::new();
        let mut pending = self.trivia().above;
        loop {
            match self.peek() {
                None => return Err("An object in the file is never closed.".into()),
                Some(b'}') => {
                    self.i += 1;
                    break;
                }
                _ => {}
            }
            let key_at = self.i;
            let key = self.key()?;
            self.trivia();
            if self.peek() != Some(b':') {
                return Err(self.err(&format!("Expected ':' after {key}")));
            }
            self.i += 1;
            self.trivia();
            let value = self.value()?;
            members.push(Member { key, line: self.line(key_at), comment: std::mem::take(&mut pending), value });
            let mut t = self.trivia();
            if self.peek() == Some(b',') {
                self.i += 1;
                let after = self.trivia();
                t.trailing.extend(after.trailing);
                t.above.extend(after.above);
            }
            if let Some(m) = members.last_mut() {
                m.comment.extend(t.trailing);
            }
            pending = t.above;
        }
        self.depth -= 1;
        Ok(members)
    }

    fn array(&mut self) -> Result<Node, String> {
        let start = self.i;
        self.i += 1;
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("The file is nested too deeply to read.".into());
        }
        let mut prefix = None;
        if self.d == Dialect::Snbt
            && let [c @ (b'B' | b'I' | b'L'), b';', ..] = &self.b[self.i..]
        {
            prefix = Some(*c as char);
            self.i += 2;
        }
        let mut items = Vec::new();
        loop {
            self.trivia();
            match self.peek() {
                None => return Err("A list in the file is never closed.".into()),
                Some(b']') => {
                    self.i += 1;
                    break;
                }
                Some(b',') => {
                    self.i += 1;
                    continue;
                }
                _ => items.push(self.value()?),
            }
        }
        self.depth -= 1;
        Ok(Node::Array { items, span: start..self.i, prefix })
    }

    fn value(&mut self) -> Result<Node, String> {
        let start = self.i;
        match self.peek() {
            None => Err(self.err("A value is missing")),
            Some(b'{') => Ok(Node::Object { members: self.object()? }),
            Some(b'[') => self.array(),
            Some(b'"' | b'\'') => {
                let s = self.string()?;
                Ok(Node::Scalar { value: Value::Str(s), span: start..self.i })
            }
            Some(c) => {
                let w = self.word();
                if w.is_empty() {
                    return Err(self.err(&format!("Unexpected '{}'", c as char)));
                }
                let value = match self.d {
                    Dialect::Json => json_word(w).ok_or_else(|| format!("Unexpected '{w}' (line {}).", self.line(start)))?,
                    Dialect::Snbt => snbt_word(w),
                };
                Ok(Node::Scalar { value, span: start..self.i })
            }
        }
    }
}

/// A bare JSON / JSON5 value: `true`, `null`, `12`, `0x1F`, `.5`, `+1`,
/// `Infinity`, `NaN`.
fn json_word(w: &str) -> Option<Value> {
    match w {
        "true" => return Some(Value::Bool(true)),
        "false" => return Some(Value::Bool(false)),
        "null" => return Some(Value::Text("null".into())),
        _ => {}
    }
    let (neg, body) = match w.as_bytes().first() {
        Some(b'-') => (true, &w[1..]),
        Some(b'+') => (false, &w[1..]),
        _ => (false, w),
    };
    let sign = if neg { -1.0 } else { 1.0 };
    match body {
        "Infinity" => return Some(Value::Float(sign * f64::INFINITY)),
        "NaN" => return Some(Value::Float(f64::NAN)),
        _ => {}
    }
    if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        let v = i64::from_str_radix(h, 16).ok()?;
        return Some(Value::Int(if neg { -v } else { v }));
    }
    if body.is_empty() || !body.bytes().any(|c| c.is_ascii_digit()) || !body.bytes().all(|c| c.is_ascii_digit() || b".eE+-".contains(&c)) {
        return None;
    }
    if body.bytes().all(|c| c.is_ascii_digit())
        && let Ok(i) = w.trim_start_matches('+').parse::<i64>()
    {
        return Some(Value::Int(i));
    }
    body.parse::<f64>().ok().map(|f| Value::Float(sign * f))
}

/// The number part and suffix letter of an SNBT number (`1.5d`, `10L`).
pub(super) fn snbt_number(w: &str) -> Option<(Value, Option<char>)> {
    let last = w.chars().last()?;
    let (body, suffix) = if "bBsSlLfFdD".contains(last) { (&w[..w.len() - 1], Some(last)) } else { (w, None) };
    let digits = body.trim_start_matches(['+', '-']);
    if digits.is_empty() || !digits.bytes().any(|c| c.is_ascii_digit()) || !digits.bytes().all(|c| c.is_ascii_digit() || b".eE+-".contains(&c)) {
        return None;
    }
    let whole = digits.bytes().all(|c| c.is_ascii_digit());
    let floaty = matches!(suffix, Some('f' | 'F' | 'd' | 'D'));
    if whole && !floaty {
        return body.trim_start_matches('+').parse::<i64>().ok().map(|i| (Value::Int(i), suffix));
    }
    if !whole && matches!(suffix, Some('b' | 'B' | 's' | 'S' | 'l' | 'L')) {
        return None;
    }
    body.parse::<f64>().ok().filter(|f| f.is_finite()).map(|f| (Value::Float(f), suffix))
}

/// A bare SNBT value: a switch, a number, or an unquoted word.
fn snbt_word(w: &str) -> Value {
    match w {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => snbt_number(w).map_or_else(|| Value::Str(w.to_string()), |(v, _)| v),
    }
}

/// The top object of `text`.
pub(super) fn read(text: &str, d: Dialect) -> Result<Vec<Member>, String> {
    let mut r = Reader::new(text, d);
    r.trivia();
    match r.peek() {
        Some(b'{') => {}
        None => return Ok(Vec::new()),
        _ => return Err("This file isn't a set of settings (it doesn't start with '{').".into()),
    }
    let members = r.object()?;
    r.trivia();
    if r.peek().is_some() {
        return Err(r.err("Something follows the end of the settings"));
    }
    Ok(members)
}

/// A string as JSON / SNBT writes it, in `q` quotes.
pub(super) fn quote(s: &str, q: char) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

/// An array as the editor shows it: a flat list `["a", 1]` when every item
/// is a plain value, else the raw text (edited as text).
fn array_value(text: &str, items: &[Node], span: &Range<usize>) -> Value {
    let mut parts = Vec::new();
    for it in items {
        match it {
            Node::Scalar { value: Value::Str(s), .. } => parts.push(quote(s, '"')),
            Node::Scalar { span, .. } => parts.push(text[span.clone()].to_string()),
            _ => return Value::Text(text[span.clone()].to_string()),
        }
    }
    Value::Text(format!("[{}]", parts.join(", ")))
}

/// The settings of a read file: one section per object, path = keys.
pub(super) fn sections(text: &str, members: &[Member]) -> Vec<Section> {
    let mut out = Vec::new();
    walk(text, members, Vec::new(), String::new(), &mut out);
    out
}

fn walk(text: &str, members: &[Member], path: Vec<String>, comment: String, out: &mut Vec<Section>) {
    let mut entries = Vec::new();
    let mut subs = Vec::new();
    for m in members {
        // A key written twice: the game reads one copy and an edit could
        // land on the other, so it's left to the text view.
        if members.iter().filter(|x| x.key == m.key).count() > 1 {
            continue;
        }
        let value = match &m.value {
            Node::Object { .. } => {
                subs.push(m);
                continue;
            }
            Node::Array { items, span, .. } => array_value(text, items, span),
            Node::Scalar { value, .. } => value.clone(),
        };
        entries.push(Entry::new(path.clone(), m.key.clone(), value, hints::describe(&m.comment), m.line));
    }
    out.push(Section { path: path.clone(), comment, entries });
    for m in subs {
        if let Node::Object { members } = &m.value {
            let mut p = path.clone();
            p.push(m.key.clone());
            walk(text, members, p, hints::describe(&m.comment).text, out);
        }
    }
}

/// The one member called `key`: `None` when there's none.
fn only<'n>(members: &'n [Member], key: &str) -> Result<Option<&'n Node>, String> {
    let mut it = members.iter().filter(|m| m.key == key);
    let first = it.next().map(|m| &m.value);
    if it.next().is_some() {
        return Err(format!("{key} is in the file more than once, so Hopper leaves it alone."));
    }
    Ok(first)
}

/// The value at `path`/`key`.
pub(super) fn find<'n>(members: &'n [Member], path: &[String], key: &str) -> Result<&'n Node, String> {
    let mut cur = members;
    for (n, p) in path.iter().enumerate() {
        cur = match only(cur, p)? {
            Some(Node::Object { members }) => members,
            _ => return Err(format!("No {} in the file.", path[..=n].join("."))),
        };
    }
    only(cur, key)?.ok_or_else(|| format!("No {key} in the file."))
}

/// The whitespace a line starts with, up to `at`, if only whitespace
/// comes before `at` on its line.
fn indent_before(text: &str, at: usize) -> Option<&str> {
    let ls = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let ind = &text[ls..at];
    ind.chars().all(|c| c == ' ' || c == '\t').then_some(ind)
}

/// A list's new text, laid out as the old one was: one item per line with
/// the same indent and separators when it was written that way, else on
/// one line. `toks` are the items already written in the file's syntax.
pub(super) fn list_layout(text: &str, span: &Range<usize>, items: &[Node], toks: &[String], prefix: Option<char>, d: Dialect) -> String {
    let orig = &text[span.clone()];
    let pre = prefix.map(|c| format!("{c};")).unwrap_or_default();
    let close = span.end - 1;
    let item_spans: Vec<Range<usize>> = items.iter().filter_map(Node::span).collect();
    if toks.is_empty() {
        return if !pre.is_empty() {
            format!("[{pre}]")
        } else if d == Dialect::Snbt || orig.starts_with("[ ") {
            "[ ]".into()
        } else {
            "[]".into()
        };
    }
    if orig.contains('\n') && !item_spans.is_empty() {
        let eol = if orig.contains("\r\n") { "\r\n" } else { "\n" };
        let indent = indent_before(text, item_spans[0].start).unwrap_or("\t");
        let close_indent = indent_before(text, close).unwrap_or("");
        let last = item_spans.last().unwrap();
        let trailing = text[last.end..close].contains(',');
        let sep = if item_spans.len() >= 2 {
            if text[item_spans[0].end..item_spans[1].start].contains(',') { "," } else { "" }
        } else if trailing || d == Dialect::Json {
            ","
        } else {
            ""
        };
        let mut out = format!("[{pre}{eol}");
        for (n, t) in toks.iter().enumerate() {
            out.push_str(indent);
            out.push_str(t);
            if n + 1 < toks.len() {
                out.push_str(sep);
            } else if trailing {
                out.push(',');
            }
            out.push_str(eol);
        }
        out.push_str(close_indent);
        out.push(']');
        return out;
    }
    let inner = &orig[1 + pre.len()..orig.len() - 1];
    let pad = !inner.is_empty() && inner.starts_with([' ', '\t']) && inner.ends_with([' ', '\t']) && !item_spans.is_empty();
    let sp = if pad { " " } else { "" };
    let lead = if pre.is_empty() { "" } else { " " };
    format!("[{pre}{lead}{sp}{}{sp}]", toks.join(", "))
}

/// The list items of an editor `Value::Text`, and whether they're written
/// quoted. `None` when it isn't a flat list.
pub(super) fn list_input(t: &str) -> Option<(Vec<String>, bool)> {
    let items = list_items(t)?;
    Some((items, t.contains('"') || t.contains('\'')))
}

fn json_token(raw: &str, v: &Value) -> Result<String, String> {
    Ok(match v {
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => {
            let body = raw.trim_start_matches(['-', '+']);
            if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X"))
                && *i >= 0
            {
                let x = if h.chars().any(|c| c.is_ascii_uppercase()) { format!("{i:X}") } else { format!("{i:x}") };
                format!("{}{x}", &body[..2])
            } else {
                i.to_string()
            }
        }
        Value::Float(f) if f.is_nan() => "NaN".into(),
        Value::Float(f) if f.is_infinite() => if *f > 0.0 { "Infinity".into() } else { "-Infinity".into() },
        v @ Value::Float(_) => v.text(),
        Value::Str(s) => quote(s, if raw.starts_with('\'') { '\'' } else { '"' }),
        Value::Text(t) => t.trim().to_string(),
    })
}

/// A list item typed in the editor, as JSON: numbers and switches as they
/// are, anything else as text.
fn json_item(item: &str, quoted: bool) -> String {
    if !quoted && json_word(item).is_some() { item.to_string() } else { quote(item, '"') }
}

/// Swaps the characters of the value at `path`/`key` for `v`, written by
/// `scalar` (or as a list); the rest of `text` stays byte for byte.
pub(super) fn set_in(
    text: &str,
    d: Dialect,
    path: &[String],
    key: &str,
    v: &Value,
    scalar: fn(&str, &Value) -> Result<String, String>,
    item: fn(&str, bool) -> String,
) -> Result<String, String> {
    let members = read(text, d)?;
    let node = find(&members, path, key)?;
    let (span, new) = match node {
        Node::Object { .. } => return Err(format!("{key} is a group of settings, not one setting.")),
        Node::Scalar { value, span } => {
            if value == v {
                return Ok(text.to_string());
            }
            (span.clone(), scalar(&text[span.clone()], v)?)
        }
        Node::Array { items, span, prefix } => {
            let old = array_value(text, items, span);
            if super::same(&old, v) {
                return Ok(text.to_string());
            }
            let Value::Text(t) = v else { return Err(format!("{key} is a list.")) };
            let new = match list_input(t) {
                Some((list, quoted)) => {
                    let toks: Vec<String> = list.iter().map(|i| item(i, quoted)).collect();
                    list_layout(text, span, items, &toks, *prefix, d)
                }
                None => t.trim().to_string(),
            };
            (span.clone(), new)
        }
    };
    let mut out = String::with_capacity(text.len() + new.len());
    out.push_str(&text[..span.start]);
    out.push_str(&new);
    out.push_str(&text[span.end..]);
    // The edit must leave a file that still reads.
    read(&out, d).map_err(|e| format!("That value would break the file: {e}"))?;
    Ok(out)
}

pub(super) fn parse(text: &str) -> Result<Vec<Section>, String> {
    Ok(sections(text, &read(text, Dialect::Json)?))
}

pub(super) fn set(text: &str, path: &[String], key: &str, v: &Value) -> Result<String, String> {
    set_in(text, Dialect::Json, path, key, v, json_token, json_item)
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSONC: &str = r#"{
  // How far the beam reaches.
  // Range: 1 - 64
  "reach": 8,
  "enabled": true, // Turns the mod on
  /* Particle density
     Default: 0.5 */
  "density": 0.5,
  "name": "Steve",
  "tags": ["a", "b"],
  "nums": [1, 2, 3],
  "client": {
    // Requires a game restart
    "fancy": false,
    "deeper": { "x": 1 }
  },
  "rules": [{ "id": 1 }, { "id": 2 }],
  "nothing": null,
}
"#;

    fn entry<'a>(s: &'a [Section], path: &[&str], key: &str) -> &'a Entry {
        s.iter().flat_map(|s| &s.entries).find(|e| e.path == path && e.key == key).unwrap_or_else(|| panic!("no {key}"))
    }

    #[test]
    fn reads_values_comments_and_paths() {
        let s = parse(JSONC).unwrap();
        let reach = entry(&s, &[], "reach");
        assert_eq!((&reach.value, reach.line, reach.comment.as_str()), (&Value::Int(8), 4, "How far the beam reaches."));
        assert_eq!(reach.range, Some((Some(1.0), Some(64.0))));
        let en = entry(&s, &[], "enabled");
        assert_eq!((&en.value, en.comment.as_str()), (&Value::Bool(true), "Turns the mod on"));
        let d = entry(&s, &[], "density");
        assert_eq!((&d.value, d.default.as_deref(), d.comment.as_str()), (&Value::Float(0.5), Some("0.5"), "Particle density"));
        assert_eq!(entry(&s, &[], "name").value, Value::Str("Steve".into()));
        assert_eq!(entry(&s, &[], "tags").value, Value::Text("[\"a\", \"b\"]".into()));
        assert_eq!(entry(&s, &[], "tags").kind(), super::super::Kind::List);
        assert_eq!(entry(&s, &[], "nums").value, Value::Text("[1, 2, 3]".into()));
        assert_eq!(entry(&s, &[], "rules").kind(), super::super::Kind::Opaque);
        assert_eq!(entry(&s, &[], "nothing").value, Value::Text("null".into()));
        let f = entry(&s, &["client"], "fancy");
        assert_eq!((f.restart, f.line), (super::super::Restart::Game, 14));
        assert_eq!(entry(&s, &["client", "deeper"], "x").value, Value::Int(1));
        assert_eq!(s.iter().map(|s| s.path.join(".")).collect::<Vec<_>>(), ["", "client", "client.deeper"]);
    }

    #[test]
    fn edits_only_the_value() {
        let set1 = |p: &[&str], k: &str, v: Value| set(JSONC, &p.iter().map(|s| s.to_string()).collect::<Vec<_>>(), k, &v).unwrap();
        assert_eq!(set1(&[], "reach", Value::Int(12)), JSONC.replace("\"reach\": 8", "\"reach\": 12"));
        assert_eq!(set1(&[], "enabled", Value::Bool(false)), JSONC.replace("\"enabled\": true", "\"enabled\": false"));
        assert_eq!(set1(&[], "density", Value::Float(2.0)), JSONC.replace("\"density\": 0.5", "\"density\": 2.0"));
        assert_eq!(set1(&[], "name", Value::Str("A \"b\"".into())), JSONC.replace("\"Steve\"", r#""A \"b\"""#));
        assert_eq!(set1(&[], "tags", Value::Text("[\"c\"]".into())), JSONC.replace("[\"a\", \"b\"]", "[\"c\"]"));
        assert_eq!(set1(&[], "nums", Value::Text("[4, 5]".into())), JSONC.replace("[1, 2, 3]", "[4, 5]"));
        assert_eq!(set1(&["client"], "fancy", Value::Bool(true)), JSONC.replace("\"fancy\": false", "\"fancy\": true"));
        assert_eq!(set1(&["client", "deeper"], "x", Value::Int(2)), JSONC.replace("\"x\": 1 }", "\"x\": 2 }"));
        let rules = set1(&[], "rules", Value::Text("[{ \"id\": 3 }]".into()));
        assert!(rules.contains("\"rules\": [{ \"id\": 3 }],"));
        // Unchanged values leave the text alone.
        assert_eq!(set1(&[], "reach", Value::Int(8)), JSONC);
        assert_eq!(set1(&[], "tags", Value::Text("[ \"a\",\"b\" ]".into())), JSONC);
        assert!(set(JSONC, &[], "missing", &Value::Int(1)).unwrap_err().contains("No missing"));
        assert!(set(JSONC, &["client".into()], "deeper", &Value::Int(1)).is_err());
        assert!(set(JSONC, &[], "rules", &Value::Text("[{".into())).is_err());
    }

    #[test]
    fn json5_spellings() {
        let t = "// top\n{\n  unquoted: 'single',\n  hex: 0x1F,\n  plus: +3,\n  half: .5,\n  inf: -Infinity,\n  'quoted key': 1,\n  trailing: [1, 2,],\n}\n";
        let s = parse(t).unwrap();
        assert_eq!(entry(&s, &[], "unquoted").value, Value::Str("single".into()));
        assert_eq!(entry(&s, &[], "hex").value, Value::Int(31));
        assert_eq!(entry(&s, &[], "plus").value, Value::Int(3));
        assert_eq!(entry(&s, &[], "half").value, Value::Float(0.5));
        assert_eq!(entry(&s, &[], "inf").value, Value::Float(f64::NEG_INFINITY));
        assert_eq!(entry(&s, &[], "quoted key").value, Value::Int(1));
        let out = set(t, &[], "hex", &Value::Int(255)).unwrap();
        assert_eq!(out, t.replace("0x1F", "0xFF"));
        let out = set(t, &[], "unquoted", &Value::Str("it's".into())).unwrap();
        assert_eq!(out, t.replace("'single'", "'it\\'s'"));
        assert_eq!(parse(&out).unwrap()[0].entries[0].value, Value::Str("it's".into()));
        let out = set(t, &[], "trailing", &Value::Text("[3]".into())).unwrap();
        assert_eq!(out, t.replace("[1, 2,]", "[3]"));
    }

    #[test]
    fn multi_line_lists_keep_their_layout() {
        let t = "{\r\n\t\"blocks\": [\r\n\t\t\"minecraft:stone\",\r\n\t\t\"minecraft:dirt\"\r\n\t],\r\n\t\"on\": true\r\n}\r\n";
        let out = set(t, &[], "blocks", &Value::Text("[\"a\", \"b\", \"c\"]".into())).unwrap();
        assert_eq!(out, "{\r\n\t\"blocks\": [\r\n\t\t\"a\",\r\n\t\t\"b\",\r\n\t\t\"c\"\r\n\t],\r\n\t\"on\": true\r\n}\r\n");
        let out = set(t, &[], "on", &Value::Bool(false)).unwrap();
        assert_eq!(out, t.replace("true", "false"));
        let out = set(t, &[], "blocks", &Value::Text("[]".into())).unwrap();
        assert_eq!(out, t.replace("[\r\n\t\t\"minecraft:stone\",\r\n\t\t\"minecraft:dirt\"\r\n\t]", "[]"));
    }

    #[test]
    fn bad_and_odd_files() {
        assert!(parse("[1, 2]").is_err());
        assert!(parse("{\"a\": ").is_err());
        assert!(parse("{\"a\": \"x}").is_err());
        assert!(parse("{\"a\": nope}").is_err());
        assert!(parse("{} trailing").is_err());
        assert!(parse("   ").unwrap().iter().all(|s| s.entries.is_empty()));
        // A key written twice is left alone.
        let dup = "{\"a\": 1, \"b\": true, \"a\": 2}";
        assert_eq!(parse(dup).unwrap()[0].entries.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(), ["b"]);
        assert!(set(dup, &[], "a", &Value::Int(3)).unwrap_err().contains("more than once"));
        assert_eq!(parse("\u{feff}{\"a\": 1}").unwrap()[0].entries[0].value, Value::Int(1));
        assert_eq!(parse(r#"{"e": "é😀\n"}"#).unwrap()[0].entries[0].value, Value::Str("é😀\n".into()));
        let deep = format!("{}{}", "{\"a\":".repeat(300), "}".repeat(300));
        assert!(parse(&deep).is_err());
    }

    #[test]
    fn flipping_every_switch() {
        let mut text = JSONC.to_string();
        for (p, k) in [(vec![], "enabled"), (vec!["client".to_string()], "fancy")] {
            let before = parse(&text).unwrap();
            let Value::Bool(b) = before.iter().flat_map(|s| &s.entries).find(|e| e.path == p && e.key == k).unwrap().value else { panic!() };
            text = set(&text, &p, k, &Value::Bool(!b)).unwrap();
            let after = parse(&text).unwrap();
            assert_eq!(after.iter().flat_map(|s| &s.entries).find(|e| e.path == p && e.key == k).unwrap().value, Value::Bool(!b));
        }
        assert_eq!(text, JSONC.replace("\"enabled\": true", "\"enabled\": false").replace("\"fancy\": false", "\"fancy\": true"));
    }
}
