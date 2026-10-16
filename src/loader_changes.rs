//! What changed between two builds of a loader, from where each loader
//! publishes it: NeoForge's and Forge's changelog files on their mavens (one
//! line per build), Fabric's GitHub release notes. Quilt publishes none.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

/// One build and what it changed (a line, or a few).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub version: String,
    pub text: String,
}

fn get_text(url: &str) -> anyhow::Result<String> {
    Ok(crate::cf::agent()
        .get(url)
        .header("User-Agent", "Hopper")
        .call()?
        .body_mut()
        .read_to_string()?)
}

/// Parses " - `21.1.259` text" (NeoForge) or " - 47.4.27 text" (Forge)
/// lines, with indented lines after one belonging to it. Newest first, as
/// the files list them.
fn parse_maven_changelog(text: &str) -> Vec<Change> {
    let mut out: Vec<Change> = Vec::new();
    for line in text.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("- ") {
            let rest = rest.trim();
            let (ver, msg) = match rest.strip_prefix('`') {
                Some(r) => match r.split_once('`') {
                    Some((v, m)) => (v.to_string(), m.trim().to_string()),
                    None => continue,
                },
                None => match rest.split_once(' ') {
                    Some((v, m)) if v.chars().next().is_some_and(|c| c.is_ascii_digit()) => {
                        (v.to_string(), m.trim().to_string())
                    }
                    _ => continue,
                },
            };
            out.push(Change { version: ver, text: msg });
        } else if !t.is_empty()
            && line.starts_with(' ')
            && let Some(last) = out.last_mut()
            // Credits after a change aren't news.
            && !t.starts_with("Co-authored-by")
        {
            last.text.push('\n');
            last.text.push_str(t);
        }
    }
    out
}

fn newer(a: &str, b: &str) -> bool {
    crate::scan::compare_versions(a, b) == std::cmp::Ordering::Greater
}

/// The builds after `from` up to and including `to` (newest first), for a
/// loader string's kind ("neoforge", "forge", "fabric", "quilt"). Going
/// back (`to` older than `from`), the builds being undone.
pub fn between(kind: &str, mc: &str, from: &str, to: &str) -> anyhow::Result<Vec<Change>> {
    static CACHE: LazyLock<Mutex<HashMap<String, Vec<Change>>>> = LazyLock::new(Default::default);
    let (lo, hi) = if newer(to, from) { (from, to) } else { (to, from) };
    let key = format!("{kind}|{mc}|{hi}");
    // The lock is let go before fetching (it's taken again to store).
    let cached = CACHE.lock().unwrap_or_else(|e| e.into_inner()).get(&key).cloned();
    let all = match cached {
        Some(v) => v,
        None => {
            let v = fetch(kind, mc, hi)?;
            CACHE.lock().unwrap_or_else(|e| e.into_inner()).insert(key, v.clone());
            v
        }
    };
    Ok(all
        .into_iter()
        .filter(|c| newer(&c.version, lo) && !newer(&c.version, hi))
        .collect())
}

/// Every build's changes up to `newest` (newest first), for a changelog
/// of the whole loader.
pub fn history(kind: &str, mc: &str, newest: &str) -> anyhow::Result<Vec<Change>> {
    between(kind, mc, "0", newest)
}

/// Everything up to build `hi`, newest first.
fn fetch(kind: &str, mc: &str, hi: &str) -> anyhow::Result<Vec<Change>> {
    match kind {
        "neoforge" => Ok(parse_maven_changelog(&get_text(&format!(
            "https://maven.neoforged.net/releases/net/neoforged/neoforge/{hi}/neoforge-{hi}-changelog.txt"
        ))?)),
        "forge" => Ok(parse_maven_changelog(&get_text(&format!(
            "https://maven.minecraftforge.net/net/minecraftforge/forge/{mc}-{hi}/forge-{mc}-{hi}-changelog.txt"
        ))?)),
        "fabric" => {
            let body = get_text("https://api.github.com/repos/FabricMC/fabric-loader/releases?per_page=100")?;
            let v: serde_json::Value = serde_json::from_str(&body)?;
            Ok(v.as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| {
                    let version = r["tag_name"].as_str()?.trim_start_matches('v').to_string();
                    let text = r["body"]
                        .as_str()
                        .unwrap_or("")
                        .lines()
                        .map(|l| l.trim().trim_start_matches(['-', '*']).trim())
                        .filter(|l| !l.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n");
                    Some(Change { version, text })
                })
                .collect())
        }
        _ => anyhow::bail!("{kind} doesn't publish a changelog Hopper can read"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEO: &str = " - `21.1.259` Backport: Fix spin attacks (#3627)

   Co-authored-by: Someone <x@y>
 - `21.1.258` Update Gradle (#3622)
 - `21.1.257` New Crowdin updates (#3482)
 - `21.1.256` Remove shader color set call (#3590)
";
    const FORGE: &str = "1.20.1 Changelog
47.4
====
 - 47.4.27 Store object holder handlers per registry name (#10899)
           Co-authored-by: LexManos <LexManos@gmail.com>
 - 47.4.26 Group the status ping channels (#10893)
 - 47.4.25 Skip the MessageFormat parse (#10894)
";

    #[test]
    fn reads_neoforge_and_forge_changelogs() {
        let n = parse_maven_changelog(NEO);
        assert_eq!(n.len(), 4);
        assert_eq!(n[0].version, "21.1.259");
        assert_eq!(n[0].text, "Backport: Fix spin attacks (#3627)");
        let f = parse_maven_changelog(FORGE);
        assert_eq!(f.iter().map(|c| c.version.as_str()).collect::<Vec<_>>(), ["47.4.27", "47.4.26", "47.4.25"]);
        assert_eq!(f[0].text, "Store object holder handlers per registry name (#10899)");
    }

    #[test]
    fn picks_the_range() {
        let all = parse_maven_changelog(NEO);
        let pick = |lo: &str, hi: &str| {
            all.iter()
                .filter(|c| newer(&c.version, lo) && !newer(&c.version, hi))
                .map(|c| c.version.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(pick("21.1.256", "21.1.259"), ["21.1.259", "21.1.258", "21.1.257"]);
        assert!(pick("21.1.259", "21.1.259").is_empty());
    }

    /// `cargo test --lib real_loader_changes -- --ignored --nocapture` (network)
    #[test]
    #[ignore]
    fn real_loader_changes() {
        for (k, mc, a, b) in [
            ("neoforge", "1.21.1", "21.1.256", "21.1.259"),
            ("forge", "1.20.1", "47.4.10", "47.4.27"),
            ("fabric", "1.21.1", "0.16.9", "0.19.5"),
        ] {
            let v = between(k, mc, a, b).unwrap();
            println!("{k} {a}→{b}: {} builds; newest {:?}", v.len(), v.first());
        }
    }
}
