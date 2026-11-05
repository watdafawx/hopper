//! A mod's own setting that works around a crash while keeping the mod: the
//! crash says `somemod:fe_seeds`, and somemod's config has `fe_seed = true`;
//! or the crash failed in ModernFix's `perf.dynamic_resources` mixin, and
//! `modernfix-mixins.properties` lists `mixin.perf.dynamic_resources`.
//! What finds them is in crashconfig.rs; this file holds what one is and
//! how it's changed (through the config editor's format-preserving write,
//! so only the value's own characters change, and back again).

use crate::configedit::{self, Entry, Format};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

/// What part of the crash a setting answers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwitchKind {
    /// An item or block the crash names (`somemod:fe_seeds`). Turning it
    /// off deletes the ones already in worlds.
    #[default]
    Item,
    /// A mixin the crash failed in, switched off in the mod's mixin options.
    Mixin,
    /// The mod's support for another mod (`compat.jei`), whose code crashed.
    Module,
    /// A limit the error says was hit, set in the mod's config: raised.
    Limit,
}

/// A setting that works around the crash.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Switch {
    /// The mod the setting belongs to.
    pub mod_id: String,
    /// What the crash names that the setting is about: the item
    /// (`fe_seeds`), the mixin, the module (`jei`) or the limit's number.
    pub item: String,
    /// The config file, relative to the instance or server with `/`.
    pub file: String,
    /// 1-based line of the setting (where it is added, for one the file
    /// only lists in a comment).
    pub line: usize,
    /// The setting's name (`fe_seed`, `mixin.perf.dynamic_resources`).
    pub key: String,
    /// The thing it switches, for people: from the config's own comment
    /// ("FE Seed"), else the key in words ("Dynamic resources").
    pub label: String,
    #[serde(default)]
    pub kind: SwitchKind,
    /// The table the setting is in (`["seeds", "groups", "energy"]`).
    #[serde(default)]
    pub path: Vec<String>,
    /// Its value when the crash happened, as text (empty: `true`).
    #[serde(default)]
    pub from: String,
    /// The value that works around the crash (empty: `false`).
    #[serde(default)]
    pub to: String,
    /// The file only lists the setting in a comment (ModernFix's
    /// "Available options"): turning it off adds `key=false` at the end,
    /// as the file says to.
    #[serde(default)]
    pub add: bool,
    /// What in the crash points at this setting, in words.
    #[serde(default)]
    pub why: String,
    /// How specific the match is (0–100); only strong ones are offered.
    #[serde(default)]
    pub score: u8,
    /// A server's setting that players' games read too (a common or client
    /// config): the card offers to send the change to players.
    #[serde(default)]
    pub shared: bool,
}

impl Switch {
    /// The value it had at the crash.
    pub fn from_text(&self) -> &str {
        if self.from.is_empty() { "true" } else { &self.from }
    }

    /// The value that works around the crash.
    pub fn to_text(&self) -> &str {
        if self.to.is_empty() { "false" } else { &self.to }
    }

    /// The file's name without folders.
    pub fn file_name(&self) -> &str {
        self.file.rsplit('/').next().unwrap_or(&self.file)
    }

    /// The button: "Turn off Dynamic resources", "Raise Max packet size to 2097152".
    pub fn action(&self) -> String {
        match self.kind {
            SwitchKind::Limit => format!("Raise {} to {}", self.label, self.to_text()),
            _ => format!("Turn off {}", self.label),
        }
    }

    /// The suggestion as one line: "Turn off Dynamic resources in ModernFix
    /// (modernfix-mixins.properties · mixin.perf.dynamic_resources)".
    pub fn sentence(&self, mod_name: &str) -> String {
        let what = match self.kind {
            SwitchKind::Limit => format!("Raise {} in {mod_name} to {}", self.label, self.to_text()),
            _ => format!("Turn off {} in {mod_name}", self.label),
        };
        format!("{what} ({} · {})", self.file_name(), self.key)
    }

    /// Done: "Dynamic resources is off", "Max packet size is 2097152".
    pub fn done(&self) -> String {
        match self.kind {
            SwitchKind::Limit => format!("{} is {}", self.label, self.to_text()),
            _ => format!("{} is off", self.label),
        }
    }

    /// Whether changing it deletes things already in worlds.
    pub fn deletes(&self) -> bool {
        self.kind == SwitchKind::Item
    }
}

/// `#Enable or disable the FE Seed (tier 2 to 6, …)` (the `#` already gone).
static ENABLE_COMMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^\s*#*\s*(?:enables?|turns? on)(?:\s+or\s+disables?)?(?:\s+the)?\s+(.+?)\s*(?:\(|[.,;:]|$)").expect("static regex")
});

/// Letters and digits, lower case: `Fe_Seeds` → `feseeds`.
pub(crate) fn squash(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

/// A setting's or item's name without "enable" around it or a plural `s`.
pub(crate) fn core(s: &str) -> String {
    let mut s = squash(s);
    for p in ["enabled", "enable"] {
        if let Some(r) = s.strip_prefix(p) {
            s = r.to_string();
        }
    }
    for p in ["enabled", "enable"] {
        if let Some(r) = s.strip_suffix(p) {
            s = r.to_string();
        }
    }
    if s.len() > 3 && s.ends_with('s') && !s.ends_with("ss") {
        s.pop();
    }
    s
}

/// Whether setting `key` is the switch for `item`.
fn matches(key: &str, item: &str) -> bool {
    let (k, i) = (core(key), core(item));
    if k.len() < 2 {
        return false;
    }
    if k == i {
        return true;
    }
    // `fe_seeds` and a seed list's `fe = true`: only for longer names.
    k.len() >= 4 && ["seed", "block", "ingot", "item", "ore", "essence"].iter().any(|s| i.strip_suffix(s) == Some(k.as_str()))
}

/// The items of `mod_id` that `text` names (`mod_id:path`), last path part.
fn items(text: &str, mod_id: &str) -> Vec<String> {
    let Ok(re) = Regex::new(&format!(r"\b{}:([a-z0-9_./-]+)", regex::escape(mod_id))) else { return Vec::new() };
    let mut out: Vec<String> = Vec::new();
    for m in re.captures_iter(text) {
        let item = m[1].rsplit('/').next().unwrap_or(&m[1]).trim_end_matches(['.', '-']).to_string();
        if !item.is_empty() && !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

/// What a setting turns on, for people: its comment's "Enable the X", else
/// the key in words.
pub(crate) fn label_of(e: &Entry) -> String {
    if let Some(m) = ENABLE_COMMENT.captures(&e.comment) {
        let l = m[1].trim();
        if !l.is_empty() && l.len() <= 40 {
            return l.to_string();
        }
    }
    e.label()
}

/// The last part of a dotted key (`general.fe_seed` → `fe_seed`).
pub(crate) fn last(key: &str) -> &str {
    key.rsplit('.').next().unwrap_or(key)
}

/// The switch in config `text` (file `rel`) for `item`, and whether it's on.
fn find_any(text: &str, rel: &str, mod_id: &str, item: &str) -> Option<(Switch, bool)> {
    let f = Format::of(rel)?;
    let sections = configedit::parse_as(f, text).ok()?;
    sections.iter().flat_map(|s| &s.entries).find_map(|e| {
        let configedit::Value::Bool(on) = e.value else { return None };
        let key = last(&e.key);
        matches(key, item).then(|| {
            let s = Switch {
                mod_id: mod_id.to_string(),
                item: item.to_string(),
                file: rel.to_string(),
                line: e.line,
                label: label_of(e),
                key: e.key.clone(),
                path: e.path.clone(),
                why: format!("The crash names {mod_id}:{item}."),
                score: if core(key) == core(item) { 90 } else { 75 },
                ..Default::default()
            };
            (s, on)
        })
    })
}

/// The switch for `item`, if it's on.
#[cfg(test)]
fn find_in(text: &str, rel: &str, mod_id: &str, item: &str) -> Option<Switch> {
    find_any(text, rel, mod_id, item).filter(|(_, on)| *on).map(|(s, _)| s)
}

/// The switches for the items of `mod_id` that crash `text` names, in
/// config files `files` (relative to `root`), each with whether it's on.
pub(crate) fn find_items(root: &Path, files: &[String], mod_id: &str, text: &str) -> Vec<(Switch, bool)> {
    let items = items(text, mod_id);
    if items.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for item in &items {
        for rel in files {
            let Ok(text) = fs::read_to_string(root.join(rel)) else { continue };
            if let Some(found) = find_any(&text, rel, mod_id, item) {
                out.push(found);
                break;
            }
        }
    }
    out
}

/// The switches for the items of `mod_id` that crash `text` names, in that
/// mod's config files, each with whether it's on.
pub fn find_all(root: &Path, mod_id: &str, text: &str) -> Vec<(Switch, bool)> {
    let files = crate::modconfig::files_for(root, &[mod_id.to_string()]);
    find_items(root, &files, mod_id, text)
}

/// The switches that are on, of [`find_all`].
pub fn find(root: &Path, mod_id: &str, text: &str) -> Vec<Switch> {
    find_all(root, mod_id, text).into_iter().filter(|(_, on)| *on).map(|(s, _)| s).collect()
}

/// The setting `s` names among `sections`: same table and key; an older
/// saved switch has no table, so the key nearest its line.
fn entry<'a>(sections: &'a [configedit::Section], s: &Switch) -> Option<&'a Entry> {
    let all = sections.iter().flat_map(|x| &x.entries);
    if let Some(e) = all.clone().find(|e| e.path == s.path && e.key == s.key) {
        return Some(e);
    }
    if s.path.is_empty() {
        return all.filter(|e| e.key == s.key || last(&e.key) == s.key).min_by_key(|e| e.line.abs_diff(s.line));
    }
    None
}

fn same_text(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// The value of `s` in the file's `text` now (`None`: not in the file).
pub fn current(text: &str, s: &Switch) -> Option<String> {
    let f = Format::of(&s.file)?;
    let sections = configedit::parse_as(f, text).ok()?;
    entry(&sections, s).map(|e| e.value.text())
}

/// Whether the setting is still as the crash had it (on, or the old limit):
/// the card still offers it.
pub fn is_on(root: &Path, s: &Switch) -> bool {
    let Ok(text) = fs::read_to_string(root.join(&s.file)) else { return false };
    match current(&text, s) {
        Some(v) => same_text(&v, s.from_text()),
        // Listed only in the comment: the mod's default, which is on.
        None => s.add,
    }
}

/// The value of setting `key` (its last name part) in config `text`, if
/// it's a true/false setting there.
pub fn value_in(text: &str, key: &str) -> Option<bool> {
    text.lines().find_map(|l| {
        let m = BOOL_LINE.captures(l.trim_end_matches('\r'))?;
        (last(&m[1]) == last(key)).then(|| &m[2] == "true")
    })
}

/// `key = true|false`, `"key" = false # note`.
static BOOL_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\s*"?([A-Za-z0-9_.-]+)"?\s*=\s*(true|false)\s*(?:#.*)?$"#).expect("static regex"));

/// `text` with `s` changed: to its fix (`forward`) or back to how the crash
/// had it. Only the value changes; a setting the file only listed in a
/// comment gets its own line at the end, and that line goes again on undo.
/// Fails when the setting has some other value now (changed by hand).
pub fn edited(text: &str, s: &Switch, forward: bool) -> anyhow::Result<String> {
    let f = Format::of(&s.file).ok_or_else(|| anyhow::anyhow!("Hopper can't change this kind of file."))?;
    let (want, was) = if forward { (s.to_text(), s.from_text()) } else { (s.from_text(), s.to_text()) };
    let sections = configedit::parse_as(f, text).unwrap_or_default();
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let added = format!("{}={}", s.key, s.to_text());
    match entry(&sections, s) {
        Some(e) if s.add && !forward && text.lines().nth(e.line.wrapping_sub(1)).is_some_and(|l| l.trim() == added) => {
            // Undo of an added line: the file as it was.
            let mut lines: Vec<&str> = text.split('\n').collect();
            lines.remove(e.line - 1);
            Ok(lines.join("\n"))
        }
        Some(e) => {
            let now = e.value.text();
            if same_text(&now, want) {
                return Ok(text.to_string());
            }
            if !same_text(&now, was) {
                anyhow::bail!("{} is {now} now, not {was}; open the config instead.", s.key);
            }
            let v = e.value.parse_like(want).map_err(anyhow::Error::msg)?;
            if let Some(why) = e.check(&v) {
                anyhow::bail!("{}: {why}", s.key);
            }
            configedit::set_as(f, text, &e.path, &e.key, &v).map_err(anyhow::Error::msg)
        }
        None if s.add && forward => {
            let mut t = text.to_string();
            if !t.is_empty() && !t.ends_with('\n') {
                t.push_str(nl);
            }
            t.push_str(&added);
            t.push_str(nl);
            Ok(t)
        }
        // Not there and listed only in a comment: already the default.
        None if s.add => Ok(text.to_string()),
        None => anyhow::bail!("{} isn't in {} any more; open the config instead.", s.key, s.file_name()),
    }
}

/// The lines that changed between `before` and `after`, each with whether
/// it's the new one: what the card shows after the click (`-` old, `+` new).
pub fn diff(before: &str, after: &str) -> Vec<(bool, String)> {
    use crate::configdiff::LineKind;
    let clean = |s: Option<String>| s.unwrap_or_default().trim_end_matches('\r').to_string();
    let mut out = Vec::new();
    for l in crate::configdiff::line_diff(before, after) {
        match l.kind {
            LineKind::Same => {}
            LineKind::Del => out.push((false, clean(l.server))),
            LineKind::Add => out.push((true, clean(l.client))),
            LineKind::Change => {
                out.push((false, clean(l.server)));
                out.push((true, clean(l.client)));
            }
        }
    }
    out
}

/// The lines Hopper changed for `s`, read from the file as it is now (the
/// card's diff after the click). Empty when the file isn't as Hopper left it.
pub fn applied_diff(root: &Path, s: &Switch) -> Vec<(bool, String)> {
    let Ok(now) = fs::read_to_string(root.join(&s.file)) else { return Vec::new() };
    match edited(&now, s, false) {
        Ok(before) => diff(&before, &now),
        Err(_) => Vec::new(),
    }
}

/// Turns the switch off (or raises the limit). The config as it was goes
/// to backups first (Backups can put it back). Returns the file before
/// and after, for the card's diff.
pub fn turn_off(root: &Path, s: &Switch) -> anyhow::Result<(String, String)> {
    set(root, s, true)
}

/// Puts the setting back as the crash had it (undoes [`turn_off`]).
pub fn turn_on(root: &Path, s: &Switch) -> anyhow::Result<(String, String)> {
    set(root, s, false)
}

fn set(root: &Path, s: &Switch, forward: bool) -> anyhow::Result<(String, String)> {
    if !configedit::editable(&s.file) {
        anyhow::bail!("That isn't a config file.");
    }
    let path = root.join(&s.file);
    let text = fs::read_to_string(&path)?;
    let new = edited(&text, s, forward)?;
    if new != text {
        configedit::save(root, &s.file, &new)?;
    }
    Ok((text, new))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "[seeds.groups.energy]\r\n\t\t\t#Enable or disable the FE Seed (tier 2 to 6, set by the energy it holds)\r\n\t\t\tfe_seed = true\r\n\r\n\tfe_generator = true\r\n";

    #[test]
    fn finds_the_item_s_switch() {
        let s = find_in(CONFIG, "config/mysticalagricultureplus-common.toml", "mysticalagricultureplus", "fe_seeds").unwrap();
        assert_eq!((s.key.as_str(), s.line, s.label.as_str()), ("fe_seed", 3, "FE Seed"));
        assert_eq!(s.path, ["seeds", "groups", "energy"]);
        // Not the generator, not other mods' items.
        assert!(find_in(CONFIG, "x.toml", "m", "fe_generators").is_some_and(|s| s.key == "fe_generator"));
        assert!(find_in(CONFIG, "x.toml", "m", "iron_seeds").is_none());
        // Already off: nothing to offer.
        assert!(find_in("fe_seed = false", "x.toml", "m", "fe_seeds").is_none());
    }

    #[test]
    fn names_and_items() {
        assert!(matches("enableFeSeed", "fe_seeds"));
        assert!(matches("fe_seed_enabled", "fe_seed"));
        assert!(matches("glass", "glass"));
        assert!(!matches("glas", "glass"));
        assert!(matches("ender", "ender_seeds"));
        assert!(!matches("fe", "fe_seeds"), "too short to trust");
        let t = "Itemstack 1 mysticalagricultureplus:fe_seeds does not exist; also mysticalagricultureplus:fe_seeds and minecraft:stone";
        assert_eq!(items(t, "mysticalagricultureplus"), vec!["fe_seeds"]);
    }

    #[test]
    fn an_older_saved_switch_still_works() {
        // Saved by 0.38 and before: no table, no values.
        let old: Switch = serde_json::from_str(
            r#"{"mod_id":"m","item":"fe_seeds","file":"config/m-common.toml","line":3,"key":"fe_seed","label":"FE Seed"}"#,
        )
        .unwrap();
        assert_eq!((old.kind, old.from_text(), old.to_text()), (SwitchKind::Item, "true", "false"));
        assert_eq!(edited(CONFIG, &old, true).unwrap(), CONFIG.replace("fe_seed = true", "fe_seed = false"));
    }

    #[test]
    fn adds_a_listed_option_and_takes_it_away_again() {
        let text = "# Available options:\n#  mixin.perf.dynamic_resources=true # (default)\n#  mixin.bugfix.x=true # (default)\n";
        let s = Switch {
            file: "config/modernfix-mixins.properties".into(),
            key: "mixin.perf.dynamic_resources".into(),
            kind: SwitchKind::Mixin,
            add: true,
            ..Default::default()
        };
        let on = edited(text, &s, true).unwrap();
        assert_eq!(on, format!("{text}mixin.perf.dynamic_resources=false\n"));
        assert_eq!(edited(&on, &s, true).unwrap(), on, "twice: nothing more");
        assert_eq!(current(&on, &s).as_deref(), Some("false"));
        assert_eq!(edited(&on, &s, false).unwrap(), text, "undo: the file as it was");
        assert_eq!(edited(text, &s, false).unwrap(), text);
    }

    #[test]
    fn raises_a_limit_within_its_range() {
        let text = "[network]\n\t#Range: 1024 ~ 8388608\n\tmaxPacketSize = 1048576\n";
        let mut s = Switch {
            file: "config/net-common.toml".into(),
            key: "maxPacketSize".into(),
            path: vec!["network".into()],
            kind: SwitchKind::Limit,
            from: "1048576".into(),
            to: "2097152".into(),
            ..Default::default()
        };
        let up = edited(text, &s, true).unwrap();
        assert_eq!(up, text.replace("1048576", "2097152"));
        assert_eq!(edited(&up, &s, false).unwrap(), text);
        // Changed by hand since: left alone.
        assert!(edited(&text.replace("1048576", "5000"), &s, true).is_err());
        // Over the file's own limit: refused.
        s.to = "99999999".into();
        assert!(edited(text, &s, true).is_err());
    }

    #[test]
    fn turns_off_with_a_backup() {
        let root = std::env::temp_dir().join(format!("hopper-configswitch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("config")).unwrap();
        fs::write(root.join("config/mysticalagricultureplus-common.toml"), CONFIG).unwrap();
        fs::write(root.join("config/mysticalagricultureplus-common.toml.bak"), CONFIG).unwrap();
        let found = find(&root, "mysticalagricultureplus", "x mysticalagricultureplus:fe_seeds y");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].file, "config/mysticalagricultureplus-common.toml");
        let (before, after) = turn_off(&root, &found[0]).unwrap();
        assert_eq!(before, CONFIG);
        assert_eq!(after, CONFIG.replace("fe_seed = true", "fe_seed = false"), "only that value, line endings kept");
        assert_eq!(fs::read_to_string(root.join(&found[0].file)).unwrap(), after);
        let d = diff(&before, &after);
        assert_eq!(d, vec![(false, "\t\t\tfe_seed = true".to_string()), (true, "\t\t\tfe_seed = false".to_string())]);
        assert!(root.join(crate::sync::BACKUP_DIR).is_dir());
        // Second time: off already, nothing changes.
        turn_off(&root, &found[0]).unwrap();
        assert_eq!(fs::read_to_string(root.join(&found[0].file)).unwrap(), after);
        assert!(!is_on(&root, &found[0]));
        // Still found, as off: the crash counts as worked around.
        let all = find_all(&root, "mysticalagricultureplus", "x mysticalagricultureplus:fe_seeds y");
        assert_eq!(all, vec![(found[0].clone(), false)]);
        assert!(find(&root, "mysticalagricultureplus", "mysticalagricultureplus:fe_seeds").is_empty());
        // Back on: the file as it was.
        turn_on(&root, &found[0]).unwrap();
        assert_eq!(fs::read_to_string(root.join(&found[0].file)).unwrap(), CONFIG);
        assert!(is_on(&root, &found[0]));
        // A line added above: still found by its table and key.
        fs::write(root.join(&found[0].file), format!("\n{CONFIG}")).unwrap();
        turn_off(&root, &found[0]).unwrap();
        // Set to something else by hand: says so.
        fs::write(root.join(&found[0].file), CONFIG.replace("fe_seed = true", "fe_seed = \"x\"")).unwrap();
        assert!(turn_off(&root, &found[0]).is_err());
        let _ = fs::remove_dir_all(&root);
    }
}
