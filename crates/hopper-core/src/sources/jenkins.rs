//! Jenkins jobs: the jars of the last successful build.

use super::{Candidate, Kind, ModSource};
use serde::Deserialize;

/// The job's URL (a link to a build or its artifacts is cut back to the job).
pub fn parse_link(link: &str) -> ModSource {
    let link = link.trim().trim_end_matches('/');
    // Cut after the last "/job/<name>".
    let job = match link.rfind("/job/") {
        Some(i) => {
            let after = &link[i + 5..];
            let name_end = after.find('/').map_or(link.len(), |j| i + 5 + j);
            &link[..name_end]
        }
        None => link,
    };
    ModSource { kind: Kind::Jenkins, url: job.to_string(), ..Default::default() }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Build {
    pub number: u64,
    pub timestamp: u64,
    pub url: String,
    pub artifacts: Vec<Artifact>,
    #[serde(rename = "changeSet")]
    pub change_set: Option<ChangeSet>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Artifact {
    #[serde(rename = "fileName")]
    pub file_name: String,
    #[serde(rename = "relativePath")]
    pub relative_path: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ChangeSet {
    pub items: Vec<Change>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Change {
    pub msg: String,
}

/// Candidates from a build's JSON.
pub fn from_build(json: &str) -> anyhow::Result<Vec<Candidate>> {
    let b: Build = serde_json::from_str(json)?;
    let notes: String = b
        .change_set
        .map(|c| c.items.iter().map(|i| format!("- {}\n", i.msg.trim())).collect())
        .unwrap_or_default();
    let date = if b.timestamp > 0 { crate::time::iso_from_secs((b.timestamp / 1000) as i64) } else { String::new() };
    let base = if b.url.ends_with('/') { b.url.clone() } else { format!("{}/", b.url) };
    Ok(b.artifacts
        .iter()
        .filter(|a| a.file_name.to_ascii_lowercase().ends_with(".jar"))
        .map(|a| Candidate {
            file_name: a.file_name.clone(),
            url: format!("{base}artifact/{}", a.relative_path),
            version: format!("#{}", b.number),
            title: format!("Build #{}", b.number),
            date: date.clone(),
            notes: notes.clone(),
            page: base.clone(),
            ..Default::default()
        })
        .collect())
}

pub fn candidates(src: &ModSource) -> anyhow::Result<Vec<Candidate>> {
    let url = format!(
        "{}/lastSuccessfulBuild/api/json?tree=number,timestamp,url,artifacts[fileName,relativePath],changeSet[items[msg]]",
        src.url
    );
    from_build(&super::http::get_text(&url, false)?)
}
