//! Which mods made a server crash on start. A client-only mod on a server
//! usually dies with a recognisable signature ("Attempted to load class …
//! for invalid dist DEDICATED_SERVER", a client mixin, a client class not
//! found, or NeoForge's loading-errors list). Ported from mod-sync's
//! `crashscan.go`.

use regex::Regex;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::LazyLock;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Hit {
    pub mod_id: String,
    pub file_name: String,
    pub reason: String,
    pub class: String,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ServerCrash {
    pub source: String,
    pub headline: String,
    pub hits: Vec<Hit>,
    /// At least one hit looks like a client-only mod.
    pub client_only: bool,
}

fn re(p: &str) -> Regex {
    Regex::new(p).expect("static regex")
}

static INVALID_DIST: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)attempted to load class\s+([\w./$]+)\s+for invalid dist DEDICATED_SERVER")
});
static MOD_BLOCK: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^\s*-- MOD ([\w-]+) --"));
static MOD_FILE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)Mod File:\s*.*?([^\\/\s]+\.jar)"));
static NEO_LOAD_ERR: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)^\s*[-*]\s*(?:Mod\s+)?.+?\(([\w-]+)\)"));
static MIXIN_JSON: LazyLock<Regex> = LazyLock::new(|| re(r"([\w-]+)\.mixins\.json"));
static NO_CLASS: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)(?:NoClassDefFoundError|ClassNotFoundException):\s*([\w./$]+)"));
static JAR: LazyLock<Regex> = LazyLock::new(|| re(r"([A-Za-z0-9_.+\-]+\.jar)"));
static CLIENT_CLASS: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)(?:net[./]minecraft[./]client|com[./]mojang[./]blaze3d|/client/)"));

fn near_jar(lines: &[&str], i: usize) -> String {
    for d in 0..=6usize {
        for j in [i.checked_sub(d), Some(i + d)].into_iter().flatten() {
            if let Some(m) = lines.get(j).and_then(|l| JAR.captures(l)) {
                return m[1].to_string();
            }
        }
    }
    String::new()
}

fn near_mod_id(lines: &[&str], i: usize) -> String {
    (0..=10usize)
        .filter_map(|d| i.checked_sub(d))
        .find_map(|j| MOD_BLOCK.captures(lines[j]).map(|m| m[1].to_string()))
        .unwrap_or_default()
}

/// Mods a crash report or log implicates, merged by mod id or jar.
pub fn hits(text: &str) -> (Vec<Hit>, bool) {
    let mut out: Vec<Hit> = Vec::new();
    let mut client_only = false;
    let mut add = |h: Hit| {
        if h.mod_id.is_empty() && h.file_name.is_empty() {
            return;
        }
        let same = out.iter_mut().find(|e| {
            (!h.mod_id.is_empty() && e.mod_id.eq_ignore_ascii_case(&h.mod_id))
                || (!h.file_name.is_empty() && e.file_name.eq_ignore_ascii_case(&h.file_name))
        });
        match same {
            Some(e) => {
                if h.reason.len() > e.reason.len() {
                    e.reason = h.reason;
                }
                for (a, b) in [
                    (&mut e.class, h.class),
                    (&mut e.file_name, h.file_name),
                    (&mut e.mod_id, h.mod_id),
                ] {
                    if a.is_empty() {
                        *a = b;
                    }
                }
            }
            None => out.push(h),
        }
    };
    let lines: Vec<&str> = text.lines().collect();
    for (i, l) in lines.iter().enumerate() {
        if let Some(m) = INVALID_DIST.captures(l) {
            client_only = true;
            add(Hit {
                mod_id: near_mod_id(&lines, i),
                file_name: near_jar(&lines, i),
                class: m[1].to_string(),
                reason: "loads a client-only class on the dedicated server".into(),
            });
        }
        if let Some(m) = MIXIN_JSON.captures(l)
            && l.to_lowercase().contains("mixin")
            && (l.contains("ERROR") || l.contains("Mixin apply") || l.contains("failed"))
        {
            let client = CLIENT_CLASS.is_match(l);
            client_only |= client;
            add(Hit {
                mod_id: m[1].to_string(),
                file_name: near_jar(&lines, i),
                class: String::new(),
                reason: if client {
                    "client-side mixin failed to apply on the server".into()
                } else {
                    "mixin failed to apply".into()
                },
            });
        }
        if let Some(m) = NO_CLASS.captures(l)
            && CLIENT_CLASS.is_match(&m[1])
        {
            client_only = true;
            add(Hit {
                mod_id: near_mod_id(&lines, i),
                file_name: near_jar(&lines, i),
                class: m[1].to_string(),
                reason: "references a client-only class".into(),
            });
        }
    }
    for m in MOD_BLOCK.captures_iter(text) {
        let whole = m.get(0).unwrap();
        let rest = &text[whole.end()..];
        let block = &rest[..rest.find("\n\n").unwrap_or(rest.len())];
        add(Hit {
            mod_id: m[1].to_string(),
            file_name: MOD_FILE
                .captures(block)
                .map(|f| f[1].to_string())
                .unwrap_or_default(),
            class: String::new(),
            reason: "named in the crash report's failed-mod list".into(),
        });
    }
    if let Some(i) = text.find("Loading errors encountered") {
        for l in text[i..].lines().skip(1).take(20) {
            if l.trim().is_empty() {
                break;
            }
            if let Some(m) = NEO_LOAD_ERR.captures(l) {
                add(Hit {
                    mod_id: m[1].to_string(),
                    file_name: String::new(),
                    class: String::new(),
                    reason: "in NeoForge's loading-errors list".into(),
                });
            }
        }
    }
    out.sort_by(|a, b| (&a.mod_id, &a.file_name).cmp(&(&b.mod_id, &b.file_name)));
    (out, client_only)
}

/// The last start's crash, if the server's last start crashed: its newest
/// crash report was written as latest.log stopped (a server that kept
/// running, or started again, writes to its log long after).
pub fn last_crash(root: &Path) -> Option<ServerCrash> {
    if let Some(r) = crate::hopperd::locate(root) {
        return r
            .and_then(|(c, id, _)| c.crash(&id))
            .inspect_err(|e| log::warn!("crash scan: {e:#}"))
            .ok()
            .flatten();
    }
    let newest = std::fs::read_dir(root.join("crash-reports"))
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "txt"))
        .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
        .max_by_key(|(t, _)| *t);
    let log_end = std::fs::metadata(root.join("logs").join("latest.log")).ok()?.modified().ok()?;
    match &newest {
        Some((t, _)) if *t + std::time::Duration::from_secs(60) >= log_end => {}
        _ => return None,
    }
    let mut sources: BTreeMap<u8, (String, String)> = BTreeMap::new();
    if let Some((_, p)) = newest
        && let Ok(t) = std::fs::read_to_string(&p)
    {
        sources.insert(
            0,
            (
                p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                t,
            ),
        );
    }
    if let Ok(t) = std::fs::read_to_string(root.join("logs/latest.log")) {
        let tail: Vec<&str> = t.lines().rev().take(800).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        sources.insert(1, ("logs/latest.log".into(), tail.join("\n")));
    }
    for (_, (name, text)) in sources {
        let (hits, client_only) = hits(&text);
        if hits.is_empty() {
            continue;
        }
        let headline = text
            .find("Description:")
            .and_then(|i| text[i + 12..].lines().next())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "Mod loading failed".into());
        return Some(ServerCrash {
            source: name,
            headline,
            hits,
            client_only,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_client_only_mods_in_a_server_crash() {
        let report = "\
Description: Mod loading error has occurred

java.lang.RuntimeException: Attempted to load class net/minecraft/client/gui/screens/Screen for invalid dist DEDICATED_SERVER

-- MOD fancymenu --
Details:
\tMod File: /server/mods/fancymenu_neoforge_3.2.jar
\tFailure message: FancyMenu has failed to load correctly

-- MOD other --
Details:
\tMod File: /server/mods/other-1.0.jar
";
        let (h, client) = hits(report);
        assert!(client);
        let ids: Vec<(&str, &str)> = h
            .iter()
            .map(|h| (h.mod_id.as_str(), h.file_name.as_str()))
            .collect();
        assert_eq!(
            ids,
            [
                ("fancymenu", "fancymenu_neoforge_3.2.jar"),
                ("other", "other-1.0.jar")
            ]
        );
    }

    #[test]
    fn reads_neoforge_loading_errors() {
        let log = "Loading errors encountered:\n\t- Mod Cool Thing (coolthing) failed\n\n";
        let (h, client) = hits(log);
        assert!(!client);
        assert_eq!(h[0].mod_id, "coolthing");
    }

    #[test]
    #[ignore = "reads the real servers"]
    fn real_servers() {
        for e in std::fs::read_dir(r"Q:\Minecraft\packs").unwrap().flatten() {
            let c = last_crash(&e.path());
            println!(
                "{}: {:?}",
                e.file_name().to_string_lossy(),
                c.map(|c| (c.headline, c.hits.len(), c.client_only))
            );
        }
    }
}
