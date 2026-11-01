//! A plain download link. It updates when the server says the file
//! changed (its ETag or Last-Modified).

use super::{Candidate, Kind, ModSource};

pub fn parse_link(link: &str) -> ModSource {
    ModSource { kind: Kind::Url, url: link.trim().to_string(), ..Default::default() }
}

/// The file name a link ends in, without its query.
pub fn name_in(link: &str) -> String {
    let path = link.split(['?', '#']).next().unwrap_or(link);
    path.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string()
}

pub fn candidates(src: &ModSource) -> anyhow::Result<Vec<Candidate>> {
    let (tag, size, named) = super::http::head(&src.url)?;
    let file_name = named.unwrap_or_else(|| name_in(&src.url));
    Ok(vec![Candidate {
        file_name,
        url: src.url.clone(),
        title: "Download link".into(),
        version: tag,
        size,
        page: src.url.clone(),
        ..Default::default()
    }])
}
