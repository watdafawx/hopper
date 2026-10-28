//! A mod's own on/off setting for the item a crash names: the crash says
//! `somemod:fe_seeds`, and somemod's config has `fe_seed = true`. Turning
//! that off is a workaround that keeps the rest of the mod.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

/// A setting that turns off the item a crash names.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Switch {
    /// The mod the setting belongs to.
    pub mod_id: String,
    /// The item the crash names (`fe_seeds`).
    pub item: String,
    /// The config file, relative to the instance with `/`.
    pub file: String,
    /// 1-based line of the setting.
    pub line: usize,
    /// The setting's name (`fe_seed`).
    pub key: String,
    /// The thing it switches, for people: from the config's own comment
    /// ("FE Seed"), else the key in words ("fe seed").
    pub label: String,
}

/// `fe_seed = true`, `"fe_seed" = true # note`.
static TRUE_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\s*"?([A-Za-z0-9_.-]+)"?\s*=\s*true\s*(?:#.*)?$"#).expect("static regex"));
/// `#Enable or disable the FE Seed (tier 2 to 6, …)`
static ENABLE_COMMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*#+\s*(?:enables?|turns? on)(?:\s+or\s+disables?)?(?:\s+the)?\s+(.+?)\s*(?:\(|[.,;:]|$)")
        .expect("static regex")
});

/// Letters and digits, lower case: `Fe_Seeds` → `feseeds`.
fn squash(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

/// A setting's or item's name without "enable" around it or a plural `s`.
fn core(s: &str) -> String {
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

/// What the comment above line `ix` says the setting turns on.
fn label(lines: &[&str], ix: usize, key: &str) -> String {
    let mut k = ix;
    while k > 0 && lines[k - 1].trim_start().starts_with('#') {
        k -= 1;
        if let Some(m) = ENABLE_COMMENT.captures(lines[k]) {
            let l = m[1].trim();
            if !l.is_empty() && l.len() <= 40 {
                return l.to_string();
            }
        }
    }
    key.replace(['_', '-', '.'], " ")
}

/// The switch in config `text` (file `rel`) for `item`, and whether it's on.
fn find_any(text: &str, rel: &str, mod_id: &str, item: &str) -> Option<(Switch, bool)> {
    let lines: Vec<&str> = text.lines().collect();
    lines.iter().enumerate().find_map(|(ix, l)| {
        let m = BOOL_LINE.captures(l)?;
        let key = m[1].to_string();
        let on = &m[2] == "true";
        let last = key.rsplit('.').next().unwrap_or(&key);
        matches(last, item).then(|| {
            let s = Switch {
                mod_id: mod_id.to_string(),
                item: item.to_string(),
                file: rel.to_string(),
                line: ix + 1,
                label: label(&lines, ix, last),
                key: last.to_string(),
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

/// The switches for the items of `mod_id` that crash `text` names, in that
/// mod's config files (TOML), each with whether it's on.
pub fn find_all(root: &Path, mod_id: &str, text: &str) -> Vec<(Switch, bool)> {
    let items = items(text, mod_id);
    if items.is_empty() {
        return Vec::new();
    }
    let files: Vec<String> =
        crate::modconfig::files_for(root, &[mod_id.to_string()]).into_iter().filter(|f| f.ends_with(".toml")).collect();
    let mut out = Vec::new();
    for item in &items {
        for rel in &files {
            let Ok(text) = fs::read_to_string(root.join(rel)) else { continue };
            if let Some(found) = find_any(&text, rel, mod_id, item) {
                out.push(found);
                break;
            }
        }
    }
    out
}

/// The switches that are on, of [`find_all`].
pub fn find(root: &Path, mod_id: &str, text: &str) -> Vec<Switch> {
    find_all(root, mod_id, text).into_iter().filter(|(_, on)| *on).map(|(s, _)| s).collect()
}

/// Whether the switch is still on in its file (on its line).
pub fn is_on(root: &Path, s: &Switch) -> bool {
    fs::read_to_string(root.join(&s.file)).is_ok_and(|t| {
        t.lines().nth(s.line.wrapping_sub(1)).and_then(|l| TRUE_LINE.captures(l)).is_some_and(|m| m[1].rsplit('.').next() == Some(s.key.as_str()))
    })
}

/// The value of setting `key` (its last name part) in config `text`, if
/// it's a true/false setting there.
pub fn value_in(text: &str, key: &str) -> Option<bool> {
    text.lines().find_map(|l| {
        let m = BOOL_LINE.captures(l.trim_end_matches('\r'))?;
        (m[1].rsplit('.').next() == Some(key)).then(|| &m[2] == "true")
    })
}

/// `key = true|false`, `"key" = false # note`.
static BOOL_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\s*"?([A-Za-z0-9_.-]+)"?\s*=\s*(true|false)\s*(?:#.*)?$"#).expect("static regex"));

/// Sets the switch to `false`. The config as it was goes to backups first
/// (Backups can put it back). Fails when the line changed since.
pub fn turn_off(root: &Path, s: &Switch) -> anyhow::Result<()> {
    set(root, s, false)
}

/// Puts the switch back to `true` (undoes [`turn_off`]).
pub fn turn_on(root: &Path, s: &Switch) -> anyhow::Result<()> {
    set(root, s, true)
}

fn set(root: &Path, s: &Switch, on: bool) -> anyhow::Result<()> {
    if !crate::configdiff::safe_rel(&s.file) || !s.file.starts_with("config/") {
        anyhow::bail!("That isn't a config file.");
    }
    let path = root.join(&s.file);
    let text = fs::read_to_string(&path)?;
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    let l = lines.get_mut(s.line.wrapping_sub(1)).ok_or_else(|| anyhow::anyhow!("The setting moved; open the config instead."))?;
    let now = BOOL_LINE
        .captures(l.trim_end_matches('\r'))
        .filter(|m| m[1].rsplit('.').next() == Some(s.key.as_str()))
        .map(|m| &m[2] == "true")
        .ok_or_else(|| anyhow::anyhow!("{} isn't on line {} any more; open the config instead.", s.key, s.line))?;
    // Already so (changed in the config editor, say): nothing to do.
    if now == on {
        return Ok(());
    }
    let eq = l.find('=').unwrap_or(0);
    let (head, tail) = l.split_at(eq);
    let (from, to) = if on { ("false", "true") } else { ("true", "false") };
    *l = format!("{head}{}", tail.replacen(from, to, 1));
    crate::sync::backup_into(root, &path)?;
    fs::write(&path, lines.join("\n"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "[seeds.groups.energy]\r\n\t\t\t#Enable or disable the FE Seed (tier 2 to 6, set by the energy it holds)\r\n\t\t\tfe_seed = true\r\n\r\n\tfe_generator = true\r\n";

    #[test]
    fn finds_the_item_s_switch() {
        let s = find_in(CONFIG, "config/mysticalagricultureplus-common.toml", "mysticalagricultureplus", "fe_seeds").unwrap();
        assert_eq!((s.key.as_str(), s.line, s.label.as_str()), ("fe_seed", 3, "FE Seed"));
        // Not the generator, not other mods' items.
        assert!(find_in(CONFIG, "x", "m", "fe_generators").is_some_and(|s| s.key == "fe_generator"));
        assert!(find_in(CONFIG, "x", "m", "iron_seeds").is_none());
        // Already off: nothing to offer.
        assert!(find_in("fe_seed = false", "x", "m", "fe_seeds").is_none());
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
    fn turns_off_with_a_backup() {
        let root = std::env::temp_dir().join(format!("hopper-configswitch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("config")).unwrap();
        fs::write(root.join("config/mysticalagricultureplus-common.toml"), CONFIG).unwrap();
        fs::write(root.join("config/mysticalagricultureplus-common.toml.bak"), CONFIG).unwrap();
        let found = find(&root, "mysticalagricultureplus", "x mysticalagricultureplus:fe_seeds y");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].file, "config/mysticalagricultureplus-common.toml");
        turn_off(&root, &found[0]).unwrap();
        let after = fs::read_to_string(root.join(&found[0].file)).unwrap();
        assert_eq!(after, CONFIG.replace("fe_seed = true", "fe_seed = false"), "only that value, line endings kept");
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
        // The line moved: says so.
        fs::write(root.join(&found[0].file), format!("
{CONFIG}")).unwrap();
        assert!(turn_off(&root, &found[0]).is_err());
        let _ = fs::remove_dir_all(&root);
    }
}
