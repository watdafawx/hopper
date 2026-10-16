//! A literal `.env` reader. CurseForge keys look like `$2a$10$…`, and the
//! usual dotenv crates expand `$name` inside values, which mangles them.

use std::path::Path;

/// Sets each `KEY=value` from `path` that the environment doesn't already
/// have. Quotes around a value are removed; nothing else is interpreted.
pub fn load(path: &Path) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    for (k, v) in parse(&text) {
        if std::env::var_os(&k).is_none() {
            // SAFETY: called once at startup, before any other thread exists.
            unsafe { std::env::set_var(k, v) };
        }
    }
}

fn parse(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("export ").unwrap_or(l).split_once('='))
        .map(|(k, v)| {
            let v = v.trim();
            let unquoted = v
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| v.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(v);
            (k.trim().to_string(), unquoted.to_string())
        })
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dollar_signs_stay_literal() {
        let p = parse("# comment\nCF_API_KEY=$2a$10$abc/def\nQUOTED=\"x y\"\n");
        assert_eq!(p[0], ("CF_API_KEY".into(), "$2a$10$abc/def".into()));
        assert_eq!(p[1], ("QUOTED".into(), "x y".into()));
    }
}
