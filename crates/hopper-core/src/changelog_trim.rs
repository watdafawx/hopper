//! Cuts a mod file's changelog down to what is new since the installed file.
//!
//! Many mods paste every earlier release under the newest one, so the 5.3.6
//! file's text holds 5.3.6 down to 5.3.1 while 5.3.5 is installed. Only the
//! part above the installed version's heading is news; the rest is kept as
//! `earlier` so the dialogs can show it behind a collapsed row. When the
//! installed version can't be found with some confidence, nothing is cut:
//! hiding text on a guess is worse than showing too much.

/// A changelog split into what is new and what the installed file already had.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Trimmed {
    pub newer: String,
    /// Empty when nothing was cut.
    pub earlier: String,
    /// Version sections (at least 1 when `earlier` isn't empty) in `earlier`.
    pub earlier_count: usize,
}

impl Trimmed {
    fn whole(text: &str) -> Trimmed {
        Trimmed { newer: text.to_string(), earlier: String::new(), earlier_count: 0 }
    }
}

/// Dotted version numbers in `s` ("5.3.6", "1.21.1"), in order.
fn version_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        let t = cur.trim_matches('.');
        if t.contains('.') && t.split('.').all(|p| !p.is_empty()) {
            out.push(t.to_string());
        }
        cur.clear();
    };
    for c in s.chars() {
        if c.is_ascii_digit() || (c == '.' && !cur.is_empty()) {
            cur.push(c);
        } else {
            flush(&mut cur, &mut out);
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// The version of a file, from its display name or file name: the last
/// dotted number in each ("Mod-NeoForge-1.21.1-5.3.5.jar" is 5.3.5).
pub fn installed_versions<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in names {
        if let Some(v) = version_tokens(n).pop()
            && !out.contains(&v)
        {
            out.push(v);
        }
    }
    out
}

/// How a heading is marked: the kind (`#` lines, bold lines, bare lines) and
/// how many `#`. Sections are the headings of one level, so a sub-heading
/// that mentions a version ("### Fixes for 1.21.1") stays inside its section.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Level(u8, usize);

struct Head {
    line: usize,
    level: Level,
    versions: Vec<String>,
}

fn heads_of(lines: &[&str]) -> Vec<Head> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(line, l)| heading_versions(l).map(|(level, versions)| Head { line, level, versions }))
        .collect()
}

/// The headings that start a section like `heads[k]` does: its kind, with
/// as many `#` or fewer.
fn same_level(heads: &[Head], k: usize) -> Vec<&Head> {
    let me = heads[k].level;
    heads.iter().filter(|h| h.level.0 == me.0 && h.level.1 <= me.1).collect()
}

/// A heading line's level and version numbers, or `None` when the line
/// isn't a version heading: a `#` line, a line that is all bold, or a short
/// bare line like `5.3.6` or `v5.3.6 Release`.
fn heading_versions(line: &str) -> Option<(Level, Vec<String>)> {
    let t = line.trim();
    if t.is_empty() {
        return None;
    }
    let mut level = Level(0, t.len() - t.trim_start_matches('#').len());
    let text = if t.starts_with('#') {
        t.trim_start_matches('#').trim()
    } else if (t.starts_with("**") && t.ends_with("**") && t.len() > 4) || (t.starts_with("__") && t.ends_with("__") && t.len() > 4) {
        level = Level(1, 0);
        t[2..t.len() - 2].trim()
    } else {
        level = Level(2, 0);
        // Bare: short, no list marker, no sentence.
        if t.starts_with(['-', '*', '+', '•', '>']) || t.split_whitespace().count() > 4 || t.ends_with(['.', ':', ';', ',']) && version_tokens(t).is_empty() {
            return None;
        }
        t
    };
    let v = version_tokens(text);
    (!v.is_empty()).then_some((level, v))
}

fn join(lines: &[&str]) -> String {
    lines.join("\n").trim_matches('\n').trim_end().to_string()
}

/// If every non-blank line of `installed` follows, in order, from some line
/// of `lines` on (nothing else between), returns where that starts.
fn overlap_start(lines: &[&str], installed: &str) -> Option<usize> {
    let want: Vec<&str> = installed.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let first = *want.first()?;
    // Too little text to be a sign of anything ("Bug fixes").
    if want.iter().map(|l| l.len()).sum::<usize>() < 24 {
        return None;
    }
    'start: for (i, l) in lines.iter().enumerate() {
        if l.trim() != first {
            continue;
        }
        let mut have = lines[i..].iter().map(|l| l.trim()).filter(|l| !l.is_empty());
        for w in &want {
            if have.next() != Some(*w) {
                continue 'start;
            }
        }
        return Some(i);
    }
    None
}

/// `text` is the changelog of a file newer than the installed one;
/// `installed_versions` are version numbers of the installed file (see
/// [`installed_versions`]) and `installed_text` its own changelog, if any.
pub fn trim_to_newer(text: &str, installed_versions: &[String], installed_text: Option<&str>) -> Trimmed {
    let lines: Vec<&str> = text.lines().collect();
    let heads = heads_of(&lines);

    let mut end = lines.len();
    let mut earlier_count = 0;
    // 1. By headings: the installed version's section and everything below.
    if let Some(k) = heads.iter().position(|h| h.versions.iter().any(|t| installed_versions.contains(t))) {
        let secs = same_level(&heads, k);
        if let Some(pos) = secs.iter().position(|h| h.line == heads[k].line)
            && secs.len() >= 2
            && pos > 0
        {
            end = heads[k].line;
            earlier_count = secs.len() - pos;
        }
    }
    // 2. By overlap: the installed file's own text sits inside this one.
    if let Some(it) = installed_text
        && let Some(i) = overlap_start(&lines[..end], it)
        && i < end
        && lines[..i].iter().any(|l| !l.trim().is_empty())
    {
        // Sections in what is cut: the headings of the shallowest level in it.
        let below: Vec<&Head> = heads.iter().filter(|h| h.line >= i).collect();
        let top = below.iter().map(|h| (h.level.0, h.level.1)).min();
        let n = below.iter().filter(|h| Some((h.level.0, h.level.1)) == top).count();
        earlier_count = earlier_count.max(n).max(1);
        end = i;
    }
    if end == lines.len() {
        return Trimmed::whole(text);
    }
    let (newer, earlier) = (join(&lines[..end]), join(&lines[end..]));
    if newer.trim().is_empty() {
        return Trimmed::whole(text);
    }
    Trimmed { newer, earlier, earlier_count }
}

/// A file's own changelog: what sits below its own version's section
/// (older releases a cumulative text repeats) is cut into `earlier`, so the
/// installed file, or a newest one with no older file to compare to, shows
/// just its own notes. Whole when `own_versions` isn't a heading or there are
/// fewer than 2 sections, or the own section is the last.
pub fn trim_to_own(text: &str, own_versions: &[String]) -> Trimmed {
    let lines: Vec<&str> = text.lines().collect();
    let heads = heads_of(&lines);
    let Some(k) = heads.iter().position(|h| h.versions.iter().any(|t| own_versions.contains(t))) else {
        return Trimmed::whole(text);
    };
    let secs = same_level(&heads, k);
    let pos = secs.iter().position(|h| h.line == heads[k].line).unwrap_or(0);
    // Fewer than 2 sections, or its own is the last: nothing below to cut.
    let Some(next) = secs.get(pos + 1).filter(|_| secs.len() >= 2) else {
        return Trimmed::whole(text);
    };
    let end = next.line;
    Trimmed { newer: join(&lines[..end]), earlier: join(&lines[end..]), earlier_count: secs.len() - pos - 1 }
}

/// A file's changelog (markdown) for a card opened on its own, as in the
/// "Other versions" list: only its own version's notes, the rest in `earlier`.
pub fn trim_file(md: &str, file: &crate::cf::File) -> Trimmed {
    let own = installed_versions([file.display_name.as_str(), file.file_name.as_str()]);
    trim_to_own(md, &own)
}

/// What to show for a file's card: only its own version's notes (see
/// [`trim_to_own`]), and for a file newer than the installed one also
/// nothing the installed file had (see [`trim_to_newer`]). `installed` is
/// that file's versions and changelog; `None` for the installed file itself.
pub fn trim_card(text: &str, own_versions: &[String], installed: Option<(&[String], Option<&str>)>) -> Trimmed {
    let own = trim_to_own(text, own_versions);
    let Some((inst_versions, inst_text)) = installed else { return own };
    let n = trim_to_newer(&own.newer, inst_versions, inst_text);
    if n.earlier.is_empty() {
        return own;
    }
    let earlier = [n.earlier, own.earlier].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join("

");
    Trimmed { newer: n.newer, earlier, earlier_count: n.earlier_count + own.earlier_count }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Vec<String> {
        vec![s.to_string()]
    }

    const TREES: &str = "\
## Oh-The-Trees-You'll-Grow-NeoForge-1.21.1-5.3.6
- Fixed saplings not growing in the nether

## Oh-The-Trees-You'll-Grow-NeoForge-1.21.1-5.3.5
- Added birch variants

## Oh-The-Trees-You'll-Grow-NeoForge-1.21.1-5.3.4
- Config cleanup

## Oh-The-Trees-You'll-Grow-NeoForge-1.21.1-5.3.3
- Fixed a crash

## Oh-The-Trees-You'll-Grow-NeoForge-1.21.1-5.3.2
- Tweaks

## Oh-The-Trees-You'll-Grow-NeoForge-1.21.1-5.3.1
- First release";

    #[test]
    fn oh_the_trees() {
        let inst = installed_versions(["Oh-The-Trees-You'll-Grow-NeoForge-1.21.1-5.3.5.jar"]);
        assert_eq!(inst, v("5.3.5"));
        let t = trim_to_newer(TREES, &inst, None);
        assert!(t.newer.contains("5.3.6") && t.newer.contains("nether") && !t.newer.contains("5.3.5"));
        assert!(t.earlier.starts_with("## Oh-The-Trees-You'll-Grow-NeoForge-1.21.1-5.3.5"));
        assert!(t.earlier.contains("First release"));
        assert_eq!(t.earlier_count, 5);
    }

    #[test]
    fn plain_and_bold_headings() {
        let t = trim_to_newer("5.3.6\n- new\n\n5.3.5\n- old\n\n5.3.4\n- older", &v("5.3.5"), None);
        assert_eq!((t.newer.as_str(), t.earlier_count), ("5.3.6\n- new", 2));
        let t = trim_to_newer("**v2.1.0 Release**\n- new\n\n**v2.0.0 (hotfix)**\n- old", &v("2.0.0"), None);
        assert_eq!((t.newer.as_str(), t.earlier_count), ("**v2.1.0 Release**\n- new", 1));
    }

    #[test]
    fn newer_versions_between_kept() {
        let t = trim_to_newer("# 3.0\n- c\n# 2.0\n- b\n# 1.0\n- a", &v("1.0"), None);
        assert_eq!(t.newer, "# 3.0\n- c\n# 2.0\n- b");
        assert_eq!(t.earlier, "# 1.0\n- a");
    }

    #[test]
    fn no_headings_no_overlap_untouched() {
        let txt = "Fixed a crash.\nAdded a thing.";
        assert_eq!(trim_to_newer(txt, &v("5.3.5"), Some("Something else entirely here.")), Trimmed::whole(txt));
    }

    #[test]
    fn installed_missing_untouched() {
        let t = trim_to_newer(TREES, &v("9.9.9"), None);
        assert_eq!(t, Trimmed::whole(TREES));
    }

    #[test]
    fn one_section_untouched() {
        let txt = "## 5.3.6\n- only";
        assert_eq!(trim_to_newer(txt, &v("5.3.6"), None), Trimmed::whole(txt));
    }

    #[test]
    fn installed_on_top_untouched() {
        // Newest heading is the installed one: nothing to cut above it.
        assert_eq!(trim_to_newer("# 1.0\n- a\n# 0.9\n- b", &v("1.0"), None), Trimmed::whole("# 1.0\n- a\n# 0.9\n- b"));
    }

    #[test]
    fn overlap_with_odd_headings() {
        let old = "The big update\n- Added trees everywhere\n- Fixed the lag";
        let new = "Shiny new thing\n- Does stuff\n\nThe big update\n- Added trees everywhere\n- Fixed the lag";
        let t = trim_to_newer(new, &v("1.0"), Some(old));
        assert_eq!(t.newer, "Shiny new thing\n- Does stuff");
        assert_eq!(t.earlier, old);
        assert_eq!(t.earlier_count, 1);
    }

    #[test]
    fn overlap_needs_whole_text_and_news_above() {
        let old = "- Added trees everywhere\n- Fixed the lag";
        // Identical text: nothing new above it, so keep it all.
        assert_eq!(trim_to_newer(old, &v("1.0"), Some(old)), Trimmed::whole(old));
        // Only partly shared: no cut.
        let new = "- a new line here\n- Added trees everywhere\n- something different";
        assert_eq!(trim_to_newer(new, &v("1.0"), Some(old)), Trimmed::whole(new));
    }

    #[test]
    fn sub_headings_stay_in_their_section() {
        let txt = "## 2.0\n### Fixes\n- x\n## 1.0\n### Fixes\n- y";
        let t = trim_to_newer(txt, &v("1.0"), None);
        assert_eq!(t.newer, "## 2.0\n### Fixes\n- x");
        assert_eq!(t.earlier_count, 1);
    }

    #[test]
    fn installed_card_cut_at_own_section() {
        // Installed 5.3.6 is the newest: nothing to compare to, cut below its own section.
        let text = TREES;
        let inst = installed_versions(["Oh-The-Trees-You'll-Grow-NeoForge-1.21.1-5.3.6.jar"]);
        let t = trim_card(text, &inst, None);
        assert!(t.newer.contains("nether") && !t.newer.contains("5.3.5"));
        assert!(t.earlier.contains("5.3.5") && t.earlier.contains("First release"));
        assert_eq!(t.earlier_count, 5);
    }

    #[test]
    fn own_cut_exact_trees() {
        // The file's own version is the top section of a cumulative text.
        let t = trim_card(TREES, &v("5.3.6"), None);
        assert!(t.newer.contains("nether") && !t.newer.contains("5.3.5"));
        assert_eq!(t.earlier_count, 5);
        // Own section last, missing, or only one section: whole.
        assert_eq!(trim_card(TREES, &v("5.3.1"), None), Trimmed::whole(TREES));
        assert_eq!(trim_card(TREES, &v("7.0.0"), None), Trimmed::whole(TREES));
        assert_eq!(trim_card("## 1.0
- a", &v("1.0"), None), Trimmed::whole("## 1.0
- a"));
    }

    #[test]
    fn newer_card_cut_both_ways() {
        // 5.3.7 over installed 5.3.5: own cut and installed cut agree; nothing between is lost.
        let text = format!("## x-5.3.7
- seven

## x-5.3.6
- six

{}", TREES.replace("-NeoForge-1.21.1", ""));
        let t = trim_card(&text, &v("5.3.7"), Some((&v("5.3.5"), None)));
        assert!(t.newer.contains("seven") && !t.newer.contains("six"));
        assert!(t.earlier.contains("six") && t.earlier.contains("First release"));
    }

    #[test]
    fn version_mentions_in_subheadings_are_not_sections() {
        // Sections are the `##` headings; "### 1.21.1" lines inside them are not.
        let mut txt = String::new();
        for v in ["5.3.6", "5.3.5", "5.3.4", "5.3.3"] {
            txt += &format!("## {v}
### Fixes for 1.21.1
- x
### Added in 1.20.1
- y

");
        }
        let t = trim_card(txt.trim_end(), &v("5.3.5"), None);
        assert!(t.newer.contains("## 5.3.6") && t.newer.contains("## 5.3.5") && !t.newer.contains("5.3.4"));
        assert_eq!(t.earlier_count, 2);
        // And against the installed 5.3.5 from a newer file.
        let t = trim_to_newer(txt.trim_end(), &v("5.3.5"), None);
        assert_eq!(t.earlier_count, 3);
        assert!(t.newer.contains("5.3.6") && !t.newer.contains("## 5.3.5"));
    }

    #[test]
    fn older_file_case() {
        // "Roll back to 5.3.5" card, installed 5.3.6: its own text is 5.3.5 down to 5.3.1.
        let file = TREES.split_once("

").unwrap().1;
        let t = trim_card(file, &v("5.3.5"), None);
        assert!(t.newer.contains("birch") && !t.newer.contains("5.3.4"));
        assert_eq!(t.earlier_count, 4);
    }

    #[test]
    fn versions_from_names() {
        assert_eq!(installed_versions(["jei-1.21.1-neoforge-19.21.0.247.jar", "JEI 19.21.0.247"]), v("19.21.0.247"));
    }
}
