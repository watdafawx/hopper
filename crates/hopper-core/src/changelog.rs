//! What changed in each release, built into the exe from the release
//! commits (see `embed_changelog` in build.rs), for What's new.

const RAW: &str = include_str!(concat!(env!("OUT_DIR"), "/changelog.txt"));

/// One release's notes.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub version: String,
    /// Paragraphs, each with its lines joined.
    pub paragraphs: Vec<String>,
}

fn parse(raw: &str) -> Vec<Entry> {
    raw.split('\x1e')
        .filter_map(|r| {
            let (version, notes) = r.split_once('\x1f')?;
            let paragraphs = notes
                .split("\n\n")
                .map(|p| p.lines().map(str::trim).collect::<Vec<_>>().join(" "))
                .filter(|p| !p.is_empty())
                .collect();
            Some(Entry { version: version.to_string(), paragraphs })
        })
        .collect()
}

/// Every release built in, newest first.
pub fn all() -> Vec<Entry> {
    parse(RAW)
}

/// Releases after `seen` up to this build, newest first.
pub fn since(seen: &str) -> Vec<Entry> {
    let current = crate::selfupdate::CURRENT;
    all()
        .into_iter()
        .filter(|e| crate::selfupdate::is_newer(&e.version, seen) && !crate::selfupdate::is_newer(&e.version, current))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_records() {
        let e = parse("0.2.0\x1fFirst line\nwraps here.\n\nSecond.\x1e0.1.0\x1f");
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].version, "0.2.0");
        assert_eq!(e[0].paragraphs, ["First line wraps here.", "Second."]);
        assert!(e[1].paragraphs.is_empty());
        assert!(parse("").is_empty());
    }
}
