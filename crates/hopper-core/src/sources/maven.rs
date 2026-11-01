//! Maven repositories: an artifact's versions from its
//! `maven-metadata.xml`, each one jar named `<artifact>-<version>.jar`.

use super::{Candidate, Kind, ModSource};

/// The artifact folder of a Maven link: the folder itself, its
/// `maven-metadata.xml`, or a jar inside a version folder. `None` when the
/// link doesn't look like a Maven layout.
pub fn parse_link(link: &str) -> Option<ModSource> {
    let link = link.trim().trim_end_matches('/');
    let base = if let Some(b) = link.strip_suffix("/maven-metadata.xml") {
        b.to_string()
    } else if link.to_ascii_lowercase().ends_with(".jar") {
        // .../group/artifact/1.2.3/artifact-1.2.3.jar
        let mut parts: Vec<&str> = link.split('/').collect();
        let file = parts.pop()?;
        let version = parts.pop()?;
        let artifact = *parts.last()?;
        if !file.starts_with(&format!("{artifact}-{version}")) {
            return None;
        }
        parts.join("/")
    } else if link.to_ascii_lowercase().contains("maven") {
        link.to_string()
    } else {
        return None;
    };
    Some(ModSource { kind: Kind::Maven, url: base, ..Default::default() })
}

/// The versions a metadata file lists, as written (oldest first, usually).
pub fn parse_versions(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find("<version>") {
        rest = &rest[i + 9..];
        let Some(j) = rest.find("</version>") else { break };
        let v = rest[..j].trim();
        if !v.is_empty() {
            out.push(v.to_string());
        }
        rest = &rest[j..];
    }
    out
}

/// The tag `<lastUpdated>` as ISO 8601 ("20240101123000" → "2024-01-01T12:30:00Z").
pub fn last_updated(xml: &str) -> String {
    let Some(i) = xml.find("<lastUpdated>") else { return String::new() };
    let v: String = xml[i + 13..].chars().take_while(|c| c.is_ascii_digit()).collect();
    if v.len() < 14 {
        return String::new();
    }
    format!("{}-{}-{}T{}:{}:{}Z", &v[0..4], &v[4..6], &v[6..8], &v[8..10], &v[10..12], &v[12..14])
}

fn is_pre(v: &str) -> bool {
    let v = v.to_ascii_lowercase();
    ["alpha", "beta", "-rc", ".rc", "snapshot", "pre"].iter().any(|p| v.contains(p))
}

/// Candidates from a metadata file: newest version first, 25 at most.
pub fn from_metadata(base: &str, xml: &str) -> Vec<Candidate> {
    let artifact = base.rsplit('/').next().unwrap_or("").to_string();
    let mut versions = parse_versions(xml);
    versions.sort_by(|a, b| crate::scan::compare_versions(b, a));
    let date = last_updated(xml);
    versions
        .into_iter()
        .take(25)
        .enumerate()
        .map(|(i, v)| {
            let file_name = format!("{artifact}-{v}.jar");
            Candidate {
                url: format!("{base}/{v}/{file_name}"),
                file_name,
                title: v.clone(),
                prerelease: is_pre(&v),
                // Only the newest version's date is in the metadata.
                date: if i == 0 { date.clone() } else { String::new() },
                page: format!("{base}/{v}/"),
                version: v,
                ..Default::default()
            }
        })
        .collect()
}

pub fn candidates(src: &ModSource) -> anyhow::Result<Vec<Candidate>> {
    let xml = super::http::get_text(&format!("{}/maven-metadata.xml", src.url), false)?;
    Ok(from_metadata(&src.url, &xml))
}
