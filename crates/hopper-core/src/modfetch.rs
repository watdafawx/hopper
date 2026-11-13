//! Letting a Hopper Server download a mod itself instead of Hopper
//! uploading it. A home connection often uploads at a few hundred KB/s
//! while the server's machine downloads at tens of MB/s, so a jar that has
//! a public download link is fetched by the server (and checked against
//! the jar's sha1 and size), and only files without one are uploaded:
//! custom jars, and CurseForge files whose author allows downloads only
//! from the website.
//!
//! The server only follows `https` links to the hosts below, on every
//! redirect too, so a paired PC can't make it fetch anything else.

use crate::cf::Expect;
use crate::instances::Instance;
use anyhow::{Context, bail};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where mod files are served from: CurseForge's CDN, Modrinth's CDN, and
/// GitHub releases (which redirect to its asset hosts).
pub const HOSTS: [&str; 6] = [
    "edge.forgecdn.net",
    "mediafilez.forgecdn.net",
    "cdn.modrinth.com",
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

/// `url` is https, to one of [`HOSTS`], with no user name or port.
pub fn allowed(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    HOSTS.contains(&host)
}

/// A download link for a file, and what the file must be.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub url: String,
    /// Lower-case hex.
    pub sha1: String,
    pub size: u64,
    /// Where it comes from, for the player: "Modrinth", "CurseForge", "GitHub".
    pub from: &'static str,
}

fn agent() -> &'static ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| {
        crate::net::agent(
            ureq::Agent::config_builder()
                .timeout_connect(Some(Duration::from_secs(30)))
                .timeout_recv_response(Some(Duration::from_secs(60)))
                // Each redirect is checked against the hosts by hand.
                .max_redirects(0)
                .user_agent(concat!("Hopper/", env!("CARGO_PKG_VERSION")))
                .build(),
        )
    })
}

/// Where a redirect from `from` to `location` goes.
fn redirect(from: &str, location: &str) -> String {
    if location.starts_with("https://") || location.starts_with("http://") {
        return location.to_string();
    }
    let origin_end = from.strip_prefix("https://").and_then(|r| r.find('/')).map_or(from.len(), |i| i + "https://".len());
    format!("{}/{}", &from[..origin_end], location.trim_start_matches('/'))
}

/// Downloads `url` to `dest`, following up to five redirects as long as
/// each stays on [`HOSTS`], and checks the file against `expect` before it
/// lands (nothing is left behind otherwise).
pub fn fetch(url: &str, dest: &Path, expect: &Expect) -> anyhow::Result<()> {
    let mut at = url.to_string();
    for _ in 0..6 {
        if !allowed(&at) {
            bail!("Hopper Server only downloads mods from CurseForge, Modrinth and GitHub, not {at}");
        }
        let resp = agent()
            .get(&at)
            .config()
            .timeout_recv_body(Some(crate::cf::body_budget(expect.len)))
            .build()
            .call()
            .with_context(|| format!("download {at}"))?;
        if resp.status().is_redirection() {
            let loc = resp.headers().get("location").and_then(|l| l.to_str().ok()).context("a redirect without a place")?;
            at = redirect(&at, loc);
            continue;
        }
        let reader = resp.into_body().into_reader();
        return crate::cf::write_checked(reader, dest, expect, &|| false, &mut |_| {}).with_context(|| format!("download {at}"));
    }
    bail!("download {url}: too many redirects")
}

/// The links of the jars in `jars` that can be downloaded from a public
/// host, by path. Modrinth is asked by sha1 (an exact file match, and no
/// key needed), CurseForge by the instance's file ids (a file its author
/// keeps to the website has no link), and a GitHub release by the tag it
/// came from. Lookups that fail leave the jar to be uploaded.
pub fn links(inst: &Instance, jars: &[PathBuf]) -> HashMap<PathBuf, Link> {
    let mut out = HashMap::new();
    let mut want: Vec<(PathBuf, String, u64)> = Vec::new();
    for j in jars {
        let (Ok(sha1), Ok(meta)) = (crate::backups::sha1_of(j), std::fs::metadata(j)) else { continue };
        want.push((j.clone(), sha1, meta.len()));
    }
    if want.is_empty() {
        return out;
    }
    let hashes: Vec<String> = want.iter().map(|w| w.1.clone()).collect();
    match crate::modrinth::versions_by_sha1(&hashes) {
        Ok(found) => {
            for (path, sha1, size) in &want {
                let file = found.get(sha1).and_then(|v| v.files.iter().find(|f| f.hashes.get("sha1") == Some(sha1)));
                if let Some(f) = file.filter(|f| allowed(&f.url)) {
                    out.insert(path.clone(), Link { url: f.url.clone(), sha1: sha1.clone(), size: *size, from: "Modrinth" });
                }
            }
        }
        Err(e) => log::info!("server download links: Modrinth lookup failed: {e:#}"),
    }
    // The rest, by what the instance knows of each jar.
    let addon = |p: &Path| {
        let name = p.file_name().unwrap_or_default().to_string_lossy();
        let name = name.strip_suffix(".disabled").unwrap_or(&name).to_string();
        inst.addons.iter().find(|a| a.file_name.eq_ignore_ascii_case(&name))
    };
    let rest: Vec<&(PathBuf, String, u64)> = want.iter().filter(|w| !out.contains_key(&w.0)).collect();
    let cf_ids: Vec<u64> = rest
        .iter()
        .filter_map(|w| addon(&w.0))
        .filter(|a| a.file_id != 0 && !crate::modrinth::is_mr(a.project_id))
        .map(|a| a.file_id)
        .collect();
    let cf_files: HashMap<u64, crate::cf::File> = if cf_ids.is_empty() || crate::cf::api_key().is_none() {
        HashMap::new()
    } else {
        match crate::cf::get_files(&cf_ids) {
            Ok(fs) => fs.into_iter().map(|f| (f.id, f)).collect(),
            Err(e) => {
                log::info!("server download links: CurseForge lookup failed: {e:#}");
                HashMap::new()
            }
        }
    };
    for (path, sha1, size) in rest {
        let Some(a) = addon(path) else { continue };
        let url = if let Some(u) = cf_files.get(&a.file_id).and_then(|f| f.download_url.clone()).filter(|u| !u.is_empty()) {
            Some((u, "CurseForge"))
        } else {
            a.source.as_ref().and_then(|s| match s.kind {
                crate::sources::Kind::GitHub if !s.seen.is_empty() => {
                    Some((format!("https://github.com/{}/releases/download/{}/{}", s.url, s.seen, a.file_name), "GitHub"))
                }
                crate::sources::Kind::Url => Some((s.url.clone(), "the mod's link")),
                _ => None,
            })
        };
        if let Some((url, from)) = url.filter(|(u, _)| allowed(u)) {
            out.insert(path.clone(), Link { url: url.replace(' ', "%20"), sha1: sha1.clone(), size: *size, from });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_to_the_mod_hosts() {
        assert!(allowed("https://cdn.modrinth.com/data/AANobbMI/versions/x/sodium.jar"));
        assert!(allowed("https://edge.forgecdn.net/files/5123/456/jei.jar"));
        assert!(allowed("https://github.com/owner/repo/releases/download/v1/x.jar"));
        for bad in [
            "http://cdn.modrinth.com/x.jar",
            "https://cdn.modrinth.com.evil.net/x.jar",
            "https://evil.net/cdn.modrinth.com/x.jar",
            "https://user@cdn.modrinth.com/x.jar",
            "https://cdn.modrinth.com:8443/x.jar",
            "https://127.0.0.1/x.jar",
            "file:///C:/x.jar",
            "",
        ] {
            assert!(!allowed(bad), "{bad}");
        }
    }

    #[test]
    fn redirects_resolve_against_the_host() {
        assert_eq!(
            redirect("https://github.com/o/r/releases/download/v1/x.jar", "https://release-assets.githubusercontent.com/a?b=1"),
            "https://release-assets.githubusercontent.com/a?b=1"
        );
        assert_eq!(redirect("https://edge.forgecdn.net/files/1/2/x.jar", "/files/1/2/y.jar"), "https://edge.forgecdn.net/files/1/2/y.jar");
        // A redirect off the hosts is still checked before it's followed.
        assert!(!allowed(&redirect("https://github.com/x", "https://evil.net/x.jar")));
    }

    #[test]
    fn a_refused_link_downloads_nothing() {
        let dest = std::env::temp_dir().join(format!("hopper-modfetch-{}.jar", std::process::id()));
        let e = fetch("https://evil.net/x.jar", &dest, &Expect::default()).unwrap_err();
        assert!(format!("{e:#}").contains("only downloads mods from"));
        assert!(!dest.exists());
    }
}
