//! What changed in each release, built into the exe from the release
//! commits (see `embed_changelog` in build.rs), for What's new.

const RAW: &str = include_str!(concat!(env!("OUT_DIR"), "/changelog.txt"));

/// One release's notes.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub version: String,
    /// Paragraphs, each with its lines joined; each "- " line of a list is
    /// its own, starting "• ". The release commit's body, or the shipped
    /// commits' subjects when it has none.
    pub paragraphs: Vec<String>,
}

fn parse(raw: &str) -> Vec<Entry> {
    raw.split('\x1e')
        .filter_map(|r| {
            let (version, notes) = r.split_once('\x1f')?;
            let (body, subjects) = notes.split_once('\x1d').unwrap_or((notes, ""));
            let paragraphs = if body.trim().is_empty() { split(subjects) } else { split(body) };
            Some(Entry { version: version.to_string(), paragraphs })
        })
        .collect()
}

/// Paragraphs of `text`: blank-line separated, lines joined, except that
/// each "- " or "* " list line stands alone as "• …" (a wrapped one keeps
/// its following lines).
fn split(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in text.split("\n\n") {
        let mut cur: Vec<&str> = Vec::new();
        let mut in_item = false;
        for line in p.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if let Some(item) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
                if !cur.is_empty() {
                    out.push(cur.join(" "));
                    cur.clear();
                }
                out.push(format!("• {}", item.trim()));
                in_item = true;
            } else if in_item && let Some(last) = out.last_mut() {
                last.push(' ');
                last.push_str(line);
            } else {
                cur.push(line);
            }
        }
        if !cur.is_empty() {
            out.push(cur.join(" "));
        }
    }
    out
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
        let e = parse("0.2.0\x1fFirst line\nwraps here.\n\nSecond.\x1dA commit\x1e0.1.0\x1f\x1dOnly a commit\x1e0.0.1\x1f");
        assert_eq!(e.len(), 3);
        assert_eq!(e[0].version, "0.2.0");
        assert_eq!(e[0].paragraphs, ["First line wraps here.", "Second."]);
        assert_eq!(e[1].paragraphs, ["Only a commit"]);
        assert!(e[2].paragraphs.is_empty());
        let l = parse("1.0\x1fFixes:\n- One\n- Two that\n  wraps\n\nAfter.\x1d");
        assert_eq!(l[0].paragraphs, ["Fixes:", "• One", "• Two that wraps", "After."]);
        assert!(parse("").is_empty());
    }
}
