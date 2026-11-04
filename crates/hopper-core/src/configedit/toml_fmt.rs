//! NeoForge / Forge TOML, through `toml_edit` so the text stays as it was.

use super::{Entry, Section, Value, first_line, hints};
use toml_edit::{DocumentMut, ImDocument, Item, Table, Value as TValue};

fn value_of(v: &TValue) -> Value {
    match v {
        TValue::Boolean(b) => Value::Bool(*b.value()),
        TValue::Integer(i) => Value::Int(*i.value()),
        TValue::Float(f) => Value::Float(*f.value()),
        TValue::String(s) => Value::Str(s.value().clone()),
        other => Value::Text(other.to_string().trim().to_string()),
    }
}

/// The value as TOML.
pub(super) fn to_toml(v: &Value) -> Result<TValue, String> {
    Ok(match v {
        Value::Bool(b) => TValue::from(*b),
        Value::Int(i) => TValue::from(*i),
        Value::Float(f) => TValue::from(*f),
        Value::Str(s) => TValue::from(s.as_str()),
        Value::Text(t) => t.trim().parse::<TValue>().map_err(|e| format!("Not a valid value: {}", first_line(&e.to_string())))?,
    })
}

/// Comment lines in `decor` text, `#` and spaces trimmed.
fn comment_lines(decor: &str) -> Vec<String> {
    decor
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('#'))
        .map(|l| l.trim_start_matches('#').trim().to_string())
        .collect()
}

/// A parsed bit of the file (comments, spacing): held as a span of `text`.
fn raw<'a>(text: &'a str, r: Option<&'a toml_edit::RawString>) -> &'a str {
    let Some(r) = r else { return "" };
    r.as_str().or_else(|| r.span().and_then(|s| text.get(s))).unwrap_or("")
}

pub(super) fn line_of(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())].matches('\n').count() + 1
}

fn walk(text: &str, t: &Table, path: Vec<String>, out: &mut Vec<Section>) {
    let mut entries = Vec::new();
    let mut subs: Vec<(String, &Table)> = Vec::new();
    for (k, item) in t.iter() {
        match item {
            Item::Value(v) => {
                let key = t.key(k);
                let decor = raw(text, key.and_then(|k| k.leaf_decor().prefix()));
                let h = hints::describe(&comment_lines(decor));
                let line = key.and_then(|k| k.span()).map_or(0, |s| line_of(text, s.start));
                entries.push(Entry::new(path.clone(), k.to_string(), value_of(v), h, line));
            }
            Item::Table(sub) => subs.push((k.to_string(), sub)),
            _ => {}
        }
    }
    let comment = hints::describe(&comment_lines(raw(text, t.decor().prefix()))).text;
    if !entries.is_empty() || path.is_empty() {
        out.push(Section { path: path.clone(), comment, entries });
    }
    for (k, sub) in subs {
        let mut p = path.clone();
        p.push(k);
        walk(text, sub, p, out);
    }
}

pub(super) fn parse(text: &str) -> Result<Vec<Section>, String> {
    let doc = ImDocument::parse(text).map_err(|e| first_line(&e.to_string()))?;
    let mut out = Vec::new();
    walk(text, doc.as_table(), Vec::new(), &mut out);
    out.retain(|s| !s.entries.is_empty());
    Ok(out)
}

pub(super) fn set(text: &str, path: &[String], key: &str, v: &Value) -> Result<String, String> {
    let doc = ImDocument::parse(text).map_err(|e| first_line(&e.to_string()))?;
    let mut t: &dyn toml_edit::TableLike = doc.as_table();
    for p in path {
        t = t.get(p).and_then(Item::as_table_like).ok_or_else(|| format!("No [{}] in the file.", path.join(".")))?;
    }
    let old = t.get(key).and_then(Item::as_value).ok_or_else(|| format!("No {key} in the file."))?;
    if value_of(old) == *v {
        return Ok(text.to_string());
    }
    let span = old.span().ok_or_else(|| format!("Can't find {key} in the file."))?;
    let mut new = to_toml(v)?;
    new.decor_mut().clear();
    let mut out = String::with_capacity(text.len() + 8);
    out.push_str(&text[..span.start]);
    out.push_str(new.to_string().trim());
    out.push_str(&text[span.end..]);
    // The edit must leave a file that still reads.
    out.parse::<DocumentMut>().map_err(|e| first_line(&e.to_string()))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn bad_files() {
        assert!(parse("a = [").is_err());
    }

    /// Every TOML config of the instance in `HOPPER_CONFIG_ROOT`: parses, and
    /// setting each value to itself changes nothing. Read only.
    #[test]
    #[ignore]
    fn real_configs() {
        let Ok(root) = std::env::var("HOPPER_CONFIG_ROOT") else { return };
        let (mut files, mut entries, mut bad) = (0, 0, Vec::new());
        let mut stack = vec![std::path::PathBuf::from(root).join("config")];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                if p.extension().is_none_or(|x| x != "toml") {
                    continue;
                }
                let Ok(text) = fs::read_to_string(&p) else { continue };
                files += 1;
                match parse(&text) {
                    Err(e) => bad.push(format!("{}: {e}", p.display())),
                    Ok(secs) => {
                        for s in &secs {
                            for en in &s.entries {
                                entries += 1;
                                match set(&text, &en.path, &en.key, &en.value) {
                                    Ok(out) if out == text => {}
                                    Ok(_) => bad.push(format!("{}: {} changed on a no-op", p.display(), en.key)),
                                    Err(e) => bad.push(format!("{}: {}: {e}", p.display(), en.key)),
                                }
                                // Flipping a switch changes that value and nothing else.
                                if let Value::Bool(b) = en.value {
                                    match set(&text, &en.path, &en.key, &Value::Bool(!b)) {
                                        Ok(out) => {
                                            let again = parse(&out).ok().and_then(|ss| {
                                                ss.into_iter().flat_map(|s| s.entries).find(|x| x.path == en.path && x.key == en.key && x.line == en.line)
                                            });
                                            let delta = out.len() as i64 - text.len() as i64;
                                            if again.map(|x| x.value) != Some(Value::Bool(!b)) || delta.abs() != 1 {
                                                bad.push(format!("{}: {} flip went wrong", p.display(), en.key));
                                            }
                                        }
                                        Err(e) => bad.push(format!("{}: {} flip: {e}", p.display(), en.key)),
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        println!("{files} files, {entries} settings, {} problems", bad.len());
        for b in bad.iter().take(30) {
            println!("  {b}");
        }
    }
}
