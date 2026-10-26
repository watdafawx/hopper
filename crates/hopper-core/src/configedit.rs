//! A mod's TOML config as settings to edit: sections, each setting with its
//! value, the comment above it, and the limits NeoForge writes there
//! (`Range: 0 ~ 10`, `Allowed Values: A, B`, `Default: 5`). Edits keep the
//! rest of the file (comments, order, spacing) as it was.

use std::fs;
use std::path::Path;
use toml_edit::{DocumentMut, ImDocument, Item, Table, Value as TValue};

/// A setting's value, as the editor shows it.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    /// A list or anything else, as TOML text (`["a", "b"]`).
    Text(String),
}

impl Value {
    fn of(v: &TValue) -> Value {
        match v {
            TValue::Boolean(b) => Value::Bool(*b.value()),
            TValue::Integer(i) => Value::Int(*i.value()),
            TValue::Float(f) => Value::Float(*f.value()),
            TValue::String(s) => Value::Str(s.value().clone()),
            other => Value::Text(other.to_string().trim().to_string()),
        }
    }

    /// The value as TOML.
    fn to_toml(&self) -> Result<TValue, String> {
        Ok(match self {
            Value::Bool(b) => TValue::from(*b),
            Value::Int(i) => TValue::from(*i),
            Value::Float(f) => TValue::from(*f),
            Value::Str(s) => TValue::from(s.as_str()),
            Value::Text(t) => t.trim().parse::<TValue>().map_err(|e| format!("Not a valid value: {}", first_line(&e.to_string())))?,
        })
    }

    /// The value as the text field shows it.
    pub fn text(&self) -> String {
        match self {
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => {
                let s = f.to_string();
                if s.contains(['.', 'e', 'E', 'n', 'i']) { s } else { format!("{s}.0") }
            }
            Value::Str(s) => s.clone(),
            Value::Text(t) => t.clone(),
        }
    }

    /// The same kind of value from the text typed for it.
    pub fn parse_like(&self, text: &str) -> Result<Value, String> {
        let t = text.trim();
        match self {
            Value::Bool(_) => match t {
                "true" => Ok(Value::Bool(true)),
                "false" => Ok(Value::Bool(false)),
                _ => Err("Type true or false.".into()),
            },
            Value::Int(_) => t.parse().map(Value::Int).map_err(|_| "Type a whole number.".into()),
            Value::Float(_) => t.parse().map(Value::Float).map_err(|_| "Type a number.".into()),
            Value::Str(_) => Ok(Value::Str(text.to_string())),
            Value::Text(_) => {
                let v = Value::Text(t.to_string());
                v.to_toml().map(|_| v)
            }
        }
    }
}

fn first_line(s: &str) -> String {
    s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string()
}

/// One setting.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// The table it's in (`["seeds", "groups", "energy"]`).
    pub path: Vec<String>,
    pub key: String,
    pub value: Value,
    /// Its comment, without the limits lines.
    pub comment: String,
    /// `Range: a ~ b` (either end may be open).
    pub range: Option<(Option<f64>, Option<f64>)>,
    /// `Allowed Values: A, B, C`.
    pub allowed: Vec<String>,
    /// `Default: x`.
    pub default: Option<String>,
    /// 1-based line of the setting in the file.
    pub line: usize,
}

impl Entry {
    /// Why `v` isn't allowed here, if it isn't.
    pub fn check(&self, v: &Value) -> Option<String> {
        let n = match v {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        };
        if let (Some(n), Some((lo, hi))) = (n, self.range) {
            let fmt = |x: f64| if x.fract() == 0.0 && x.abs() < 1e15 { format!("{}", x as i64) } else { x.to_string() };
            match (lo, hi) {
                (Some(l), Some(h)) if n < l || n > h => return Some(format!("Between {} and {}.", fmt(l), fmt(h))),
                (Some(l), None) if n < l => return Some(format!("At least {}.", fmt(l))),
                (None, Some(h)) if n > h => return Some(format!("At most {}.", fmt(h))),
                _ => {}
            }
        }
        if let Value::Str(s) = v
            && !self.allowed.is_empty()
            && !self.allowed.iter().any(|a| a == s)
        {
            return Some(format!("One of: {}.", self.allowed.join(", ")));
        }
        None
    }
}

/// A table of settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    /// Empty for the top of the file.
    pub path: Vec<String>,
    /// The comment above its header.
    pub comment: String,
    pub entries: Vec<Entry>,
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

fn num(s: &str) -> Option<f64> {
    let s = s.trim().trim_end_matches(['.', ',']);
    s.parse::<f64>().ok().filter(|n| n.is_finite() && n.abs() < 1e300)
}

/// `0 ~ 10`, `> 0`, `< 5`, `-2.1E308 ~ 2.1E308` (as good as open).
fn range(s: &str) -> Option<(Option<f64>, Option<f64>)> {
    let s = s.trim();
    if let Some(r) = s.strip_prefix('>') {
        return Some((num(r.trim_start_matches('=')), None));
    }
    if let Some(r) = s.strip_prefix('<') {
        return Some((None, num(r.trim_start_matches('='))));
    }
    let (a, b) = s.split_once('~')?;
    Some((num(a), num(b)))
}

/// The comment and its limits.
fn describe(lines: Vec<String>) -> (String, Option<(Option<f64>, Option<f64>)>, Vec<String>, Option<String>) {
    let (mut text, mut rng, mut allowed, mut default) = (Vec::new(), None, Vec::new(), None);
    for l in lines {
        let low = l.to_ascii_lowercase();
        if let Some(r) = low.strip_prefix("range:") {
            rng = range(r);
        } else if low.starts_with("allowed values:") {
            allowed = l["allowed values:".len()..].split(',').map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).collect();
        } else if low.starts_with("default:") {
            default = Some(l["default:".len()..].trim().to_string());
        } else if !l.is_empty() {
            text.push(l);
        }
    }
    (text.join(" "), rng, allowed, default)
}

/// A parsed bit of the file (comments, spacing): held as a span of `text`.
fn raw<'a>(text: &'a str, r: Option<&'a toml_edit::RawString>) -> &'a str {
    let Some(r) = r else { return "" };
    r.as_str().or_else(|| r.span().and_then(|s| text.get(s))).unwrap_or("")
}

fn line_of(text: &str, offset: usize) -> usize {
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
                let (comment, range, allowed, default) = describe(comment_lines(decor));
                let line = key.and_then(|k| k.span()).map_or(0, |s| line_of(text, s.start));
                entries.push(Entry { path: path.clone(), key: k.to_string(), value: Value::of(v), comment, range, allowed, default, line });
            }
            Item::Table(sub) => subs.push((k.to_string(), sub)),
            _ => {}
        }
    }
    let comment = describe(comment_lines(raw(text, t.decor().prefix()))).0;
    if !entries.is_empty() || path.is_empty() {
        out.push(Section { path: path.clone(), comment, entries });
    }
    for (k, sub) in subs {
        let mut p = path.clone();
        p.push(k);
        walk(text, sub, p, out);
    }
}

/// The settings in TOML `text`. Lists of tables are left to the text view.
pub fn parse(text: &str) -> Result<Vec<Section>, String> {
    let doc = ImDocument::parse(text).map_err(|e| first_line(&e.to_string()))?;
    let mut out = Vec::new();
    walk(text, doc.as_table(), Vec::new(), &mut out);
    out.retain(|s| !s.entries.is_empty());
    Ok(out)
}

/// `text` with the setting `path`/`key` set to `v`. Only the value's own
/// characters change: comments, spacing and line endings stay as they were.
pub fn set(text: &str, path: &[String], key: &str, v: &Value) -> Result<String, String> {
    let doc = ImDocument::parse(text).map_err(|e| first_line(&e.to_string()))?;
    let mut t: &dyn toml_edit::TableLike = doc.as_table();
    for p in path {
        t = t.get(p).and_then(Item::as_table_like).ok_or_else(|| format!("No [{}] in the file.", path.join(".")))?;
    }
    let old = t.get(key).and_then(Item::as_value).ok_or_else(|| format!("No {key} in the file."))?;
    if Value::of(old) == *v {
        return Ok(text.to_string());
    }
    let span = old.span().ok_or_else(|| format!("Can't find {key} in the file."))?;
    let mut new = v.to_toml()?;
    new.decor_mut().clear();
    let mut out = String::with_capacity(text.len() + 8);
    out.push_str(&text[..span.start]);
    out.push_str(new.to_string().trim());
    out.push_str(&text[span.end..]);
    // The edit must leave a file that still reads.
    out.parse::<DocumentMut>().map_err(|e| first_line(&e.to_string()))?;
    Ok(out)
}

/// Saves `text` to `root`/`rel`; the file as it was goes to backups first
/// (Backups can put it back).
pub fn save(root: &Path, rel: &str, text: &str) -> anyhow::Result<()> {
    if !crate::configdiff::safe_rel(rel) || !(rel.starts_with("config/") || rel.starts_with("defaultconfigs/")) {
        anyhow::bail!("That isn't a config file.");
    }
    let path = root.join(rel);
    if fs::read_to_string(&path).is_ok_and(|old| old == text) {
        return Ok(());
    }
    crate::sync::backup_into(root, &path)?;
    fs::write(&path, text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEO: &str = "#General settings\n[general]\n\t#How far it reaches\n\t#Range: 1 ~ 64\n\treach = 8\n\t#Mode of operation\n\t#Allowed Values: FAST, SAFE\n\tmode = \"SAFE\"\n\n[seeds.groups.energy]\n\t\t#Enable or disable the FE Seed\n\t\tfe_seed = true\n\t\tlist = [\"a\", \"b\"] # trailing\n\t\tchance = 0.5\n";

    #[test]
    fn reads_sections_values_and_limits() {
        let s = parse(NEO).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].path, vec!["general"]);
        assert_eq!(s[0].comment, "General settings");
        let reach = &s[0].entries[0];
        assert_eq!((reach.key.as_str(), &reach.value, reach.line), ("reach", &Value::Int(8), 5));
        assert_eq!(reach.comment, "How far it reaches");
        assert_eq!(reach.range, Some((Some(1.0), Some(64.0))));
        assert_eq!(reach.check(&Value::Int(65)).as_deref(), Some("Between 1 and 64."));
        assert!(reach.check(&Value::Int(64)).is_none());
        let mode = &s[0].entries[1];
        assert_eq!(mode.allowed, vec!["FAST", "SAFE"]);
        assert!(mode.check(&Value::Str("slow".into())).is_some());
        assert_eq!(s[1].path, vec!["seeds", "groups", "energy"]);
        assert_eq!(s[1].entries[0].value, Value::Bool(true));
        assert_eq!(s[1].entries[0].line, 12);
        assert_eq!(s[1].entries[1].value, Value::Text("[\"a\", \"b\"]".into()));
        assert_eq!(s[1].entries[2].value, Value::Float(0.5));
    }

    #[test]
    fn edits_keep_the_rest() {
        let path = vec!["seeds".to_string(), "groups".into(), "energy".into()];
        let out = set(NEO, &path, "fe_seed", &Value::Bool(false)).unwrap();
        assert_eq!(out, NEO.replace("fe_seed = true", "fe_seed = false"));
        let out = set(NEO, &path, "list", &Value::Text("[\"c\"]".into())).unwrap();
        assert_eq!(out, NEO.replace("[\"a\", \"b\"] # trailing", "[\"c\"] # trailing"));
        let out = set(NEO, &["general".to_string()], "mode", &Value::Str("FAST".into())).unwrap();
        assert!(out.contains("\tmode = \"FAST\"\n"));
        assert!(set(NEO, &["nope".to_string()], "x", &Value::Int(1)).is_err());
        assert!(Value::Text(String::new()).parse_like("[1,").is_err());
        assert_eq!(Value::Int(0).parse_like(" 12 "), Ok(Value::Int(12)));
        assert!(Value::Int(0).parse_like("1.5").is_err());
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

    #[test]
    fn open_ranges_and_bad_files() {
        assert_eq!(range(" > 0"), Some((Some(0.0), None)));
        assert_eq!(range("-2.1E308 ~ 2.1E308"), Some((None, None)));
        assert!(parse("a = [").is_err());
    }
}
