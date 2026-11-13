//! A mod's config file as settings to edit: sections, each setting with its
//! value, the comment above it, and the limits mods write there
//! (`Range: 0 ~ 10`, `Allowed Values: A, B`, `Default: 5`). Edits change
//! only the value's own characters: comments, order and spacing stay as
//! they were. Nothing is ever regenerated from a schema (there is none).
//!
//! Formats: NeoForge / Forge TOML, old Forge `.cfg`, FTB `.snbt`, JSON /
//! JSON5 / JSONC, and flat `.properties` / YAML / `.ini`. Each reads into
//! the same [`Section`]s and edits values in place.

use std::fs;
use std::path::Path;

pub mod hints;
pub mod label;
mod toml_fmt;
mod forge_cfg;
mod snbt;
mod json_like;
mod flat;

pub use label::label;

/// A setting's value, as the editor shows it.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    /// A list or anything else, as TOML-style text (`["a", "b"]`).
    Text(String),
}

impl Value {
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
            Value::Bool(_) => match t.to_ascii_lowercase().as_str() {
                "true" => Ok(Value::Bool(true)),
                "false" => Ok(Value::Bool(false)),
                _ => Err("Type true or false.".into()),
            },
            Value::Int(_) => t.parse().map(Value::Int).map_err(|_| "Type a whole number.".into()),
            Value::Float(_) => t.parse().map(Value::Float).map_err(|_| "Type a number.".into()),
            Value::Str(_) => Ok(Value::Str(text.to_string())),
            Value::Text(_) => {
                let v = Value::Text(t.to_string());
                toml_fmt::to_toml(&v).map(|_| v)
            }
        }
    }

    fn number(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }
}

pub(crate) fn first_line(s: &str) -> String {
    s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string()
}

/// When a change takes effect, if the file says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Restart {
    #[default]
    None,
    /// Rejoin the world (or restart the server).
    World,
    /// Restart the game.
    Game,
}

/// How a colour is written in the file, so an edit keeps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorFmt {
    /// A number (`16777215`), alpha in the top byte when it has one.
    Int,
    /// `"#RRGGBB"` / `"#AARRGGBB"`.
    Hash,
    /// `"0xRRGGBB"`.
    Hex0x,
}

/// Which control a setting gets.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Bool,
    Int,
    Float,
    Str,
    /// One of the file's allowed values.
    Choice(Vec<String>),
    /// A flat list of plain values.
    List,
    Color(ColorFmt),
    /// Can't be edited as a setting (inline tables, nested lists): text at its line.
    Opaque,
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
    /// "Requires a world restart" and the like.
    pub restart: Restart,
    /// The type comes from the value alone (JSON, YAML): no schema says it.
    pub guessed: bool,
}

impl Entry {
    pub(crate) fn new(path: Vec<String>, key: String, value: Value, h: hints::Hints, line: usize) -> Entry {
        Entry { path, key, value, comment: h.text, range: h.range, allowed: h.allowed, default: h.default, line, restart: h.restart, guessed: false }
    }

    /// Why `v` isn't allowed here, if it isn't.
    pub fn check(&self, v: &Value) -> Option<String> {
        if let (Some(n), Some((lo, hi))) = (v.number(), self.range) {
            let fmt = num_text;
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

    /// The friendly name ("Fan particle density").
    pub fn label(&self) -> String {
        label::label(&self.key)
    }

    /// The control it gets.
    pub fn kind(&self) -> Kind {
        let colorish = {
            let k = self.key.to_ascii_lowercase();
            k.contains("color") || k.contains("colour")
        };
        match &self.value {
            Value::Bool(_) => Kind::Bool,
            Value::Int(_) if colorish && self.range.is_none_or(|(_, hi)| hi.is_none_or(|h| h >= 0xFFFFFF as f64)) => Kind::Color(ColorFmt::Int),
            Value::Int(_) => Kind::Int,
            Value::Float(_) => Kind::Float,
            Value::Str(_) if !self.allowed.is_empty() => Kind::Choice(self.allowed.clone()),
            Value::Str(s) => match parse_color(s) {
                Some((_, f)) if colorish || f == ColorFmt::Hash => Kind::Color(f),
                _ => Kind::Str,
            },
            Value::Text(t) if list_items(t).is_some() => Kind::List,
            Value::Text(_) => Kind::Opaque,
        }
    }

    /// The two ends for a slider: both known and a sane span.
    pub fn slider(&self) -> Option<(f64, f64)> {
        let (Some(lo), Some(hi)) = self.range? else { return None };
        let whole = matches!(self.value, Value::Int(_));
        let span = hi - lo;
        (span > 0.0 && (if whole { span <= 1000.0 } else { span <= 100_000.0 })).then_some((lo, hi))
    }

    /// The default as a value of this setting's kind.
    pub fn default_value(&self) -> Option<Value> {
        let d = self.default.as_deref()?.trim();
        match &self.value {
            Value::Text(t) => {
                let items = list_items(d).or_else(|| {
                    // NeoForge writes list defaults without quotes: `[a, b]`.
                    let inner = d.strip_prefix('[')?.strip_suffix(']')?;
                    Some(inner.split(',').map(|s| s.trim().trim_matches('"').to_string()).filter(|s| !s.is_empty()).collect())
                })?;
                Some(Value::Text(list_text(&items, list_quoted(t, &items))))
            }
            Value::Str(_) => Some(Value::Str(d.trim_matches('"').to_string())),
            Value::Int(_) => {
                // `Default: 16777215` or a colour key's `Default: #FFFFFF`.
                d.parse::<i64>().ok().or_else(|| d.parse::<f64>().ok().filter(|f| f.fract() == 0.0).map(|f| f as i64)).map(Value::Int)
            }
            v => v.parse_like(d).ok(),
        }
    }

    /// Whether it still has its default: `None` when the file doesn't say.
    pub fn is_default(&self) -> Option<bool> {
        Some(same(&self.default_value()?, &self.value))
    }
}

/// Two values the same way the game reads them (`1` and `1.0`, list spacing).
pub fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => a.number() == b.number(),
        (Value::Text(x), Value::Text(y)) => match (list_items(x), list_items(y)) {
            (Some(p), Some(q)) => p == q,
            _ => x.split_whitespace().collect::<String>() == y.split_whitespace().collect::<String>(),
        },
        // An opaque value (`null`) typed as a plain one reads back as that kind.
        (Value::Text(_), _) | (_, Value::Text(_)) => a.text().trim() == b.text().trim(),
        _ => a == b,
    }
}

/// Keys whose values are passwords, tokens and the like: Hopper Server shows
/// them masked and friends' shared settings never carry them. One list for
/// both apps. `auth` only counts as its own word ("authToken", "oauth"),
/// not inside "author".
pub fn secret_key(key: &str) -> bool {
    let k = crate::configowner::squash(key);
    const WORDS: [&str; 9] = ["token", "secret", "password", "passwd", "apikey", "webhook", "privatekey", "session", "cookie"];
    if WORDS.iter().any(|w| k.contains(w)) {
        return true;
    }
    let low = key.to_ascii_lowercase();
    let words: Vec<&str> = low.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect();
    // camelCase splits too: "authKey" -> "auth", "Key".
    let camel = key.split(|c: char| c.is_ascii_uppercase() || !c.is_ascii_alphanumeric()).next().unwrap_or("").to_ascii_lowercase();
    words.iter().any(|w| matches!(*w, "auth" | "oauth" | "authkey" | "authcode")) || camel == "auth" || k.ends_with("oauth") || k.ends_with("auth")
}

pub(crate) fn num_text(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 { format!("{}", x as i64) } else { x.to_string() }
}

/// The items of a flat list (`["a", "b"]`, `[1, 2]`, `[]`), strings
/// unquoted. `None` for anything nested or not a list.
pub fn list_items(text: &str) -> Option<Vec<String>> {
    let t = text.trim();
    let inner = t.strip_prefix('[')?.strip_suffix(']')?;
    let mut out = Vec::new();
    let mut chars = inner.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace() || *c == ',') {
            chars.next();
        }
        let Some(&c) = chars.peek() else { break };
        if c == '[' || c == '{' {
            return None;
        }
        if c == '#' {
            // A comment inside a multi-line list.
            for c in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
            continue;
        }
        if c == '"' || c == '\'' {
            chars.next();
            let mut s = String::new();
            let mut esc = false;
            loop {
                let ch = chars.next()?;
                if esc {
                    s.push(match ch {
                        'n' => '\n',
                        't' => '\t',
                        o => o,
                    });
                    esc = false;
                } else if ch == '\\' && c == '"' {
                    esc = true;
                } else if ch == c {
                    break;
                } else {
                    s.push(ch);
                }
            }
            out.push(s);
        } else {
            let mut s = String::new();
            while let Some(&ch) = chars.peek() {
                if ch == ',' || ch == '\n' {
                    break;
                }
                if ch == ']' || ch == '[' || ch == '{' || ch == '"' {
                    return None;
                }
                s.push(ch);
                chars.next();
            }
            let s = s.trim().to_string();
            if !s.is_empty() {
                out.push(s);
            }
        }
    }
    Some(out)
}

/// Whether a list's items are written quoted: as the list already is, or
/// when they aren't all numbers and switches.
pub fn list_quoted(original: &str, items: &[String]) -> bool {
    if original.contains('"') || original.contains('\'') {
        return true;
    }
    if list_items(original).is_some_and(|v| !v.is_empty()) {
        return false;
    }
    items.iter().any(|i| i.parse::<f64>().is_err() && i != "true" && i != "false")
}

/// A list's text from its items, written `["a", "b"]` (or `[1, 2]`).
pub fn list_text(items: &[String], quoted: bool) -> String {
    let parts: Vec<String> = items
        .iter()
        .map(|i| if quoted { format!("\"{}\"", i.replace('\\', "\\\\").replace('"', "\\\"")) } else { i.clone() })
        .collect();
    format!("[{}]", parts.join(", "))
}

/// `#RRGGBB`, `#AARRGGBB`, `0xRRGGBB`: the colour (alpha kept) and its format.
pub fn parse_color(s: &str) -> Option<(u32, ColorFmt)> {
    let s = s.trim();
    let (hex, f) = if let Some(h) = s.strip_prefix('#') {
        (h, ColorFmt::Hash)
    } else if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        (h, ColorFmt::Hex0x)
    } else {
        return None;
    };
    if !matches!(hex.len(), 3 | 6 | 8) || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let hex = if hex.len() == 3 { hex.chars().flat_map(|c| [c, c]).collect() } else { hex.to_string() };
    u32::from_str_radix(&hex, 16).ok().map(|v| (v, f))
}

/// A setting's colour as `0xAARRGGBB` (alpha 0xFF when the file has none),
/// for the swatch.
pub fn color_of(v: &Value) -> Option<u32> {
    match v {
        Value::Int(i) if (0..=0xFFFF_FFFF).contains(i) => {
            let i = *i as u32;
            Some(if i <= 0xFF_FFFF { 0xFF00_0000 | i } else { i })
        }
        Value::Int(i) => Some(*i as u32),
        Value::Str(s) => parse_color(s).map(|(c, _)| {
            let digits = s.trim().trim_start_matches('#').trim_start_matches("0x").trim_start_matches("0X").len();
            if digits == 8 { c } else { 0xFF00_0000 | c }
        }),
        _ => None,
    }
}

/// `rgb` (`0xRRGGBB`) written the way `like` is, keeping its alpha.
pub fn color_value(rgb: u32, like: &Value) -> Value {
    let alpha = color_of(like).map_or(0xFF, |c| c >> 24);
    let has_alpha = match like {
        Value::Int(i) => *i > 0xFF_FFFF || *i < 0,
        Value::Str(s) => s.trim().trim_start_matches('#').trim_start_matches("0x").trim_start_matches("0X").len() == 8,
        _ => false,
    };
    let full = (alpha << 24) | (rgb & 0xFF_FFFF);
    match like {
        Value::Int(i) if *i < 0 => Value::Int(full as i32 as i64),
        Value::Int(_) => Value::Int(if has_alpha { full as i64 } else { (rgb & 0xFF_FFFF) as i64 }),
        Value::Str(s) => {
            let upper = s.chars().any(|c| c.is_ascii_uppercase() && c.is_ascii_hexdigit());
            let body = if has_alpha { format!("{full:08x}") } else { format!("{:06x}", rgb & 0xFF_FFFF) };
            let body = if upper { body.to_uppercase() } else { body };
            let prefix = if s.trim().starts_with('#') { "#" } else if s.trim().starts_with("0X") { "0X" } else { "0x" };
            Value::Str(format!("{prefix}{body}"))
        }
        other => other.clone(),
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

/// The ways a config file can be written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    Toml,
    /// Old Forge `.cfg` (`I:"x"=1`) and plain `key=value` `.cfg`.
    ForgeCfg,
    Snbt,
    /// JSON, JSON5, JSONC.
    Json,
    /// `.properties`, flat YAML, `.ini`.
    Flat,
}

impl Format {
    /// The format a file's name says it's in.
    pub fn of(rel: &str) -> Option<Format> {
        let l = rel.to_ascii_lowercase();
        let ext = l.rsplit('.').next().unwrap_or("");
        Some(match ext {
            "toml" => Format::Toml,
            "cfg" => Format::ForgeCfg,
            "snbt" => Format::Snbt,
            "json" | "json5" | "jsonc" => Format::Json,
            "properties" | "yml" | "yaml" | "ini" => Format::Flat,
            _ => return None,
        })
    }

    /// Whether the types come from the file itself (comments with limits,
    /// type prefixes) rather than guessed from the values.
    pub fn typed(self) -> bool {
        matches!(self, Format::Toml | Format::ForgeCfg | Format::Snbt)
    }
}

/// The settings in TOML `text`. Lists of tables are left to the text view.
pub fn parse(text: &str) -> Result<Vec<Section>, String> {
    toml_fmt::parse(text)
}

/// `text` with the TOML setting `path`/`key` set to `v`. Only the value's
/// own characters change: comments, spacing and line endings stay.
pub fn set(text: &str, path: &[String], key: &str, v: &Value) -> Result<String, String> {
    toml_fmt::set(text, path, key, v)
}

/// The settings in `text`, read as `f`.
pub fn parse_as(f: Format, text: &str) -> Result<Vec<Section>, String> {
    let mut s = match f {
        Format::Toml => toml_fmt::parse(text),
        Format::ForgeCfg => forge_cfg::parse(text),
        Format::Snbt => snbt::parse(text),
        Format::Json => json_like::parse(text),
        Format::Flat => flat::parse(text),
    }?;
    if !f.typed() {
        for e in s.iter_mut().flat_map(|s| &mut s.entries) {
            e.guessed = true;
        }
    }
    s.retain(|s| !s.entries.is_empty());
    Ok(s)
}

/// `text` with one setting set, written as `f`, the rest unchanged.
pub fn set_as(f: Format, text: &str, path: &[String], key: &str, v: &Value) -> Result<String, String> {
    match f {
        Format::Toml => toml_fmt::set(text, path, key, v),
        Format::ForgeCfg => forge_cfg::set(text, path, key, v),
        Format::Snbt => snbt::set(text, path, key, v),
        Format::Json => json_like::set(text, path, key, v),
        Format::Flat => flat::set(text, path, key, v),
    }
}

/// The settings of the file `rel` (its format from the name).
pub fn parse_file(rel: &str, text: &str) -> Result<Vec<Section>, String> {
    parse_as(Format::of(rel).ok_or("Hopper can't read this kind of file as settings.")?, text)
}

/// One change to a setting.
#[derive(Clone, Debug, PartialEq)]
pub struct Edit {
    pub path: Vec<String>,
    pub key: String,
    pub value: Value,
}

/// A setting that couldn't take its edit, and why.
#[derive(Clone, Debug, PartialEq)]
pub struct Refused {
    pub path: Vec<String>,
    pub key: String,
    pub error: String,
}

/// Applies every edit to `text`, checked together first: one bad value
/// stops them all, so nothing half-done is ever written.
pub fn apply(f: Format, text: &str, edits: &[Edit]) -> Result<String, Vec<Refused>> {
    let sections = parse_as(f, text).map_err(|e| vec![Refused { path: Vec::new(), key: String::new(), error: e }])?;
    let mut bad = Vec::new();
    for e in edits {
        let refuse = |error: String| Refused { path: e.path.clone(), key: e.key.clone(), error };
        match sections.iter().flat_map(|s| &s.entries).find(|x| x.path == e.path && x.key == e.key) {
            None => bad.push(refuse(format!("{} isn't in the file any more.", e.key))),
            Some(x) => {
                if let Some(err) = x.check(&e.value) {
                    bad.push(refuse(err));
                }
            }
        }
    }
    if !bad.is_empty() {
        return Err(bad);
    }
    let mut out = text.to_string();
    for e in edits {
        out = set_as(f, &out, &e.path, &e.key, &e.value).map_err(|error| vec![Refused { path: e.path.clone(), key: e.key.clone(), error }])?;
    }
    // The result must still read, with every edit in place.
    let after = parse_as(f, &out).map_err(|error| vec![Refused { path: Vec::new(), key: String::new(), error }])?;
    for e in edits {
        if !after.iter().flat_map(|s| &s.entries).any(|x| x.path == e.path && x.key == e.key && same(&x.value, &e.value)) {
            return Err(vec![Refused { path: e.path.clone(), key: e.key.clone(), error: "The edit didn't read back.".into() }]);
        }
    }
    Ok(out)
}

/// Whether Hopper may write `rel` as a config: under `config/`,
/// `defaultconfigs/`, `kubejs/config/`, or a world's `serverconfig/`.
pub fn editable(rel: &str) -> bool {
    let r = rel.replace('\\', "/");
    if !crate::configdiff::safe_rel(&r) {
        return false;
    }
    let parts: Vec<&str> = r.split('/').collect();
    r.starts_with("config/")
        || r.starts_with("defaultconfigs/")
        || r.starts_with("kubejs/config/")
        || matches!(parts.as_slice(), ["saves", _, "serverconfig", ..] | [_, "serverconfig", ..])
}

/// Saves `text` to `root`/`rel`; the file as it was goes to backups first
/// (Backups can put it back).
pub fn save(root: &Path, rel: &str, text: &str) -> anyhow::Result<()> {
    if !editable(rel) {
        anyhow::bail!("That isn't a config file.");
    }
    let path = root.join(rel);
    if fs::read_to_string(&path).is_ok_and(|old| old == text) {
        return Ok(());
    }
    write_safely(root, &path, text)
}

/// Writes through a temp file and a rename, so the game never reads half a
/// file; the old copy goes to backups between the two.
fn write_safely(root: &Path, path: &Path, text: &str) -> anyhow::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".hopper-tmp");
    let tmp = std::path::PathBuf::from(tmp);
    fs::write(&tmp, text)?;
    let saved = match crate::sync::backup_into(root, path) {
        Ok(s) => s,
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
    };
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        if let Some(s) = saved {
            let _ = fs::rename(&s, path);
        }
        return Err(e.into());
    }
    Ok(())
}

/// What [`save_edits`] did.
#[derive(Clone, Debug, PartialEq)]
pub struct Saved {
    /// The file's text now.
    pub text: String,
    /// The file had changed on disk since it was opened (the game wrote
    /// it): the edits went onto that newer copy.
    pub reapplied: bool,
}

/// Saves `edits` to `root`/`rel`, opened as `base`. When the file changed on
/// disk meanwhile, the edits go onto the file as it is now rather than over
/// it, so whatever the game wrote stays. Checked together before anything is
/// written; the old copy goes to backups.
pub fn save_edits(root: &Path, rel: &str, base: &str, edits: &[Edit]) -> anyhow::Result<Saved> {
    if !editable(rel) {
        anyhow::bail!("That isn't a config file.");
    }
    let f = Format::of(rel).ok_or_else(|| anyhow::anyhow!("Hopper can't read this kind of file as settings."))?;
    let path = root.join(rel);
    let disk = fs::read_to_string(&path)?;
    let reapplied = disk != base;
    let text = apply(f, &disk, edits).map_err(|bad| {
        anyhow::anyhow!(bad.iter().map(|b| if b.key.is_empty() { b.error.clone() } else { format!("{}: {}", b.key, b.error) }).collect::<Vec<_>>().join(" "))
    })?;
    if text != disk {
        write_safely(root, &path, &text)?;
    }
    Ok(Saved { text, reapplied })
}

/// The edits whose value in `rel` isn't what was saved any more (the game
/// wrote the file back), with the value it has now.
pub fn moved_back(root: &Path, rel: &str, edits: &[Edit]) -> Vec<(Edit, Value)> {
    let Some(f) = Format::of(rel) else { return Vec::new() };
    let Ok(text) = fs::read_to_string(root.join(rel)) else { return Vec::new() };
    let Ok(sections) = parse_as(f, &text) else { return Vec::new() };
    edits
        .iter()
        .filter_map(|e| {
            let now = sections.iter().flat_map(|s| &s.entries).find(|x| x.path == e.path && x.key == e.key)?;
            (!same(&now.value, &e.value)).then(|| (e.clone(), now.value.clone()))
        })
        .collect()
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
        assert_eq!(reach.slider(), Some((1.0, 64.0)));
        let mode = &s[0].entries[1];
        assert_eq!(mode.allowed, vec!["FAST", "SAFE"]);
        assert_eq!(mode.kind(), Kind::Choice(vec!["FAST".into(), "SAFE".into()]));
        assert!(mode.check(&Value::Str("slow".into())).is_some());
        assert_eq!(s[1].path, vec!["seeds", "groups", "energy"]);
        assert_eq!(s[1].entries[0].value, Value::Bool(true));
        assert_eq!(s[1].entries[0].line, 12);
        assert_eq!(s[1].entries[1].value, Value::Text("[\"a\", \"b\"]".into()));
        assert_eq!(s[1].entries[1].kind(), Kind::List);
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

    #[test]
    fn batch_is_all_or_nothing() {
        let g = vec!["general".to_string()];
        let ok = [Edit { path: g.clone(), key: "reach".into(), value: Value::Int(9) }, Edit { path: g.clone(), key: "mode".into(), value: Value::Str("FAST".into()) }];
        let out = apply(Format::Toml, NEO, &ok).unwrap();
        assert!(out.contains("reach = 9") && out.contains("mode = \"FAST\""));
        let bad = [Edit { path: g.clone(), key: "reach".into(), value: Value::Int(9) }, Edit { path: g, key: "reach".into(), value: Value::Int(99) }];
        let err = apply(Format::Toml, NEO, &bad).unwrap_err();
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].error, "Between 1 and 64.");
    }

    #[test]
    fn defaults_lists_and_colours() {
        let t = "#Range: 0.0 ~ 1.0\n#Default: 0.5\ndensity = 0.5\n#Default: [minecraft:stone, minecraft:dirt]\nblocks = [\"minecraft:stone\", \"minecraft:dirt\"]\n#Default: 1\nlevel = 2\nhighlightColor = \"#38BDF8\"\nbarColour = 16777215\nnested = [[1, 2]]\n";
        let s = parse(t).unwrap();
        let e = |k: &str| s.iter().flat_map(|s| &s.entries).find(|e| e.key == k).unwrap().clone();
        assert_eq!(e("density").is_default(), Some(true));
        assert_eq!(e("blocks").is_default(), Some(true));
        assert_eq!(e("level").is_default(), Some(false));
        assert_eq!(e("level").default_value(), Some(Value::Int(1)));
        assert_eq!(e("highlightColor").kind(), Kind::Color(ColorFmt::Hash));
        assert_eq!(e("barColour").kind(), Kind::Color(ColorFmt::Int));
        assert_eq!(e("nested").kind(), Kind::Opaque);
        assert_eq!(e("density").kind(), Kind::Float);
        assert_eq!(color_value(0xFF0000, &Value::Str("#38BDF8".into())), Value::Str("#FF0000".into()));
        assert_eq!(color_value(0xFF0000, &Value::Int(16777215)), Value::Int(0xFF0000));
        assert_eq!(color_value(0x00FF00, &Value::Str("0x80ffffff".into())), Value::Str("0x8000ff00".into()));
        assert_eq!(color_of(&Value::Str("#fff".into())), Some(0xFFFF_FFFF));
        assert_eq!(list_items("[1, 2, 3]"), Some(vec!["1".into(), "2".into(), "3".into()]));
        assert_eq!(list_items("[\n  \"a\", # one\n  \"b\"\n]"), Some(vec!["a".into(), "b".into()]));
        assert_eq!(list_text(&["a".into(), "b\"c".into()], true), "[\"a\", \"b\\\"c\"]");
        assert!(list_quoted("[]", &["minecraft:stone".into()]));
        assert!(!list_quoted("[1]", &["2".into()]));
    }

    #[test]
    fn where_it_may_write() {
        assert!(editable("config/a.toml"));
        assert!(editable("saves/T/serverconfig/a-server.toml"));
        assert!(editable("world/serverconfig/a-server.toml"));
        assert!(editable("defaultconfigs/a.toml"));
        assert!(!editable("mods/a.jar"));
        assert!(!editable("config/../options.txt"));
    }

    #[test]
    fn save_reapplies_onto_newer_disk() {
        let root = std::env::temp_dir().join(format!("hopper-cfgsave-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("config")).unwrap();
        fs::write(root.join("config/x.toml"), NEO).unwrap();
        // The game wrote another value while the editor was open.
        let newer = NEO.replace("fe_seed = true", "fe_seed = false");
        fs::write(root.join("config/x.toml"), &newer).unwrap();
        let edits = [Edit { path: vec!["general".into()], key: "reach".into(), value: Value::Int(10) }];
        let s = save_edits(&root, "config/x.toml", NEO, &edits).unwrap();
        assert!(s.reapplied);
        let disk = fs::read_to_string(root.join("config/x.toml")).unwrap();
        assert!(disk.contains("reach = 10") && disk.contains("fe_seed = false"));
        assert!(moved_back(&root, "config/x.toml", &edits).is_empty());
        fs::write(root.join("config/x.toml"), NEO).unwrap();
        assert_eq!(moved_back(&root, "config/x.toml", &edits).len(), 1);
        let _ = fs::remove_dir_all(&root);
    }
}
