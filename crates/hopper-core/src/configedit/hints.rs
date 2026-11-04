//! What a setting's comment says about it, for every config format: the
//! description, and the limits mods write there in many spellings
//! (`Range: 0 ~ 10`, `[range: 0 ~ 10, default: 5]`, `Range: 1 - 10`,
//! `Allowed Values: A, B`, `Valid values: [A, B]`, `Default value: 5`,
//! `Requires a world restart`).

use super::Restart;

/// A comment, read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hints {
    /// The description, limits lines taken out.
    pub text: String,
    pub range: Option<(Option<f64>, Option<f64>)>,
    pub allowed: Vec<String>,
    pub default: Option<String>,
    pub restart: Restart,
}

/// A number from a hint. Huge ends (`3.4028234663852886E38`, `2.1E308`)
/// mean "no limit" and read as none.
pub(crate) fn num(s: &str) -> Option<f64> {
    let s = s.trim().trim_end_matches(['.', ',', ']', ')']).trim_start_matches(['[', '(']);
    s.trim().parse::<f64>().ok().filter(|n| n.is_finite() && n.abs() < 1e30)
}

fn big(s: &str) -> bool {
    s.trim().parse::<f64>().is_ok_and(|n| !n.is_finite() || n.abs() >= 1e30)
}

/// `0 ~ 10`, `> 0`, `>= 1`, `< 5`, `[1 ~ 64]`, `1 - 10`, `-5 - 10`.
pub(crate) fn range(s: &str) -> Option<(Option<f64>, Option<f64>)> {
    let s = s.trim().trim_start_matches(['[', '(']).trim_end_matches(['.', ']', ')']).trim();
    if let Some(r) = s.strip_prefix('>') {
        return Some((num(r.trim_start_matches('=')), None));
    }
    if let Some(r) = s.strip_prefix('<') {
        return Some((None, num(r.trim_start_matches('='))));
    }
    let (a, b) = match s.split_once('~') {
        Some(p) => p,
        // `1 - 10`: a dash between spaces, so `-5` stays a number.
        None => s.split_once(" - ").or_else(|| s.split_once(" to "))?,
    };
    let (lo, hi) = (num(a), num(b));
    // Something that isn't a number at all isn't a range.
    if lo.is_none() && !big(a) || hi.is_none() && !big(b) {
        return None;
    }
    Some((lo, hi))
}

/// `A, B, C` or `[A, B, C]` or `"A", "B"`.
fn choices(s: &str) -> Vec<String> {
    let s = s.trim().trim_end_matches('.').trim();
    let s = s.strip_prefix('[').and_then(|x| x.strip_suffix(']')).unwrap_or(s);
    s.split(',').map(|v| v.trim().trim_matches('"').trim().to_string()).filter(|v| !v.is_empty()).collect()
}

/// The text after `label:` when `l` starts with one of `labels` (any case).
fn after<'a>(l: &'a str, labels: &[&str]) -> Option<&'a str> {
    let low = l.to_ascii_lowercase();
    labels.iter().find(|p| low.starts_with(*p)).map(|p| l[p.len()..].trim())
}

fn restart_of(low: &str) -> Restart {
    if !(low.contains("restart") || low.contains("reload") || low.contains("relog")) || !low.contains("require") && !low.contains("need") {
        return Restart::None;
    }
    if low.contains("world") || low.contains("rejoin") || low.contains("relog") || low.contains("reload") {
        Restart::World
    } else {
        Restart::Game
    }
}

/// One `key: value` part of a hint line, if it's a limit.
fn take(h: &mut Hints, part: &str) -> bool {
    let p = part.trim();
    if let Some(r) = after(p, &["range:"]) {
        h.range = range(r).or(h.range);
        return true;
    }
    if let Some(v) = after(p, &["allowed values:", "valid values:", "allowed:", "valid:", "possible values:", "options:"]) {
        let c = choices(v);
        if !c.is_empty() {
            h.allowed = c;
        }
        return true;
    }
    if let Some(v) = after(p, &["default value:", "default:", "defaults to:", "default is:"]) {
        let v = v.trim();
        // A sentence's full stop, but not a list's bracket.
        let v = if v.ends_with(']') && !v.contains('[') { v.trim_end_matches(']') } else { v };
        let v = v.trim_end_matches('.').trim();
        h.default = Some(v.trim_matches('"').to_string()).filter(|v| !v.is_empty()).or(Some(String::new()));
        return true;
    }
    false
}

/// Reads comment lines (`#`/`//` already trimmed).
pub fn describe<S: AsRef<str>>(lines: &[S]) -> Hints {
    let mut h = Hints::default();
    let mut text: Vec<String> = Vec::new();
    for l in lines {
        let l = l.as_ref().trim();
        // Create writes `#.` lines as spacers.
        if l.is_empty() || l.chars().all(|c| c == '.' || c == '-' || c == '=' || c == '#') {
            continue;
        }
        let low = l.to_ascii_lowercase();
        let r = restart_of(&low);
        if r != Restart::None {
            h.restart = h.restart.max(r);
            // A line that's only the restart note isn't description.
            if l.len() < 48 {
                continue;
            }
        }
        // Old Forge `.cfg`: `Some text [range: 0 ~ 10, default: 5]`.
        if let Some(open) = low.rfind("[range:").or_else(|| low.rfind("[default:"))
            && l.ends_with(']')
        {
            let inner = &l[open + 1..l.len() - 1];
            // `default` may hold commas itself; read range first.
            let mut rest = inner;
            if let Some(r) = after(rest, &["range:"]) {
                let (rg, tail) = r.split_once(", default:").map_or((r, None), |(a, b)| (a, Some(b)));
                h.range = range(rg).or(h.range);
                rest = tail.unwrap_or("");
                if tail.is_some() {
                    h.default = Some(rest.trim().trim_matches('"').to_string());
                }
            } else {
                take(&mut h, rest);
            }
            let before = l[..open].trim();
            if !before.is_empty() {
                text.push(before.to_string());
            }
            continue;
        }
        // `Default: 1; Range: [-1 ~ 127]` (Forge cfg) and the like.
        let parts: Vec<&str> = l.split("; ").collect();
        if parts.iter().all(|p| {
            let mut probe = Hints::default();
            take(&mut probe, p)
        }) {
            for p in parts {
                take(&mut h, p);
            }
            continue;
        }
        text.push(l.to_string());
    }
    h.text = text.join(" ");
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Hints {
        describe(&s.lines().collect::<Vec<_>>())
    }

    #[test]
    fn spellings() {
        let h = d("How far\nRange: 1 ~ 64\nDefault: 8");
        assert_eq!((h.text.as_str(), h.range, h.default.as_deref()), ("How far", Some((Some(1.0), Some(64.0))), Some("8")));
        assert_eq!(d("Range: > 1").range, Some((Some(1.0), None)));
        assert_eq!(d("Range: 0.0 ~ 3.4028234663852886E38").range, Some((Some(0.0), None)));
        assert_eq!(d("Range: 1 - 10").range, Some((Some(1.0), Some(10.0))));
        assert_eq!(d("Range: -5 - 10").range, Some((Some(-5.0), Some(10.0))));
        let h = d("Default: 1; Range: [-1 ~ 127]");
        assert_eq!((h.range, h.default.as_deref(), h.text.as_str()), (Some((Some(-1.0), Some(127.0))), Some("1"), ""));
        let h = d("Turns it on [default: true]");
        assert_eq!((h.text.as_str(), h.default.as_deref()), ("Turns it on", Some("true")));
        let h = d("Max size [range: 1 ~ 32, default: 4]");
        assert_eq!((h.range, h.default.as_deref()), (Some((Some(1.0), Some(32.0))), Some("4")));
        assert_eq!(d("Allowed Values: FAST, SAFE").allowed, ["FAST", "SAFE"]);
        assert_eq!(d("Valid Values: [A, B, C]").allowed, ["A", "B", "C"]);
        assert_eq!(d("Default value: \"x\"").default.as_deref(), Some("x"));
        assert_eq!(d("Requires a world restart").restart, Restart::World);
        assert_eq!(d("This requires a game restart to apply").restart, Restart::Game);
        assert_eq!(d(".\nA line\n.").text, "A line");
        // A sentence mentioning a range isn't a limit.
        assert_eq!(d("The range of the wand").range, None);
        assert_eq!(d("Range: far").range, None);
    }
}
