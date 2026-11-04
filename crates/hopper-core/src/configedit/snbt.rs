//! FTB's SNBT configs (`ftbessentials.snbt`, `ftbchunks-world.snbt`): a
//! `{ key: value }` tree with `#` comments, read through the same tree as
//! JSON ([`json_like`]). Numbers keep their type letter when edited
//! (`2.0f` stays a float written `f`), and lists keep one item per line
//! when that's how the file has them.

use super::json_like::{self, Dialect, quote, snbt_number};
use super::{Section, Value};

pub(super) fn parse(text: &str) -> Result<Vec<Section>, String> {
    let mut s = json_like::sections(text, &json_like::read(text, Dialect::Snbt)?);
    // FTB writes defaults with their type letter (`Default: 2.0f`); without
    // it the editor can compare them with the value.
    for e in s.iter_mut().flat_map(|s| &mut s.entries) {
        if matches!(e.value, Value::Int(_) | Value::Float(_))
            && let Some((v, Some(_))) = e.default.as_deref().and_then(snbt_number)
        {
            e.default = Some(v.text());
        }
    }
    Ok(s)
}

/// A word SNBT reads back as the same text without quotes.
fn bare_ok(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c))
        && s != "true"
        && s != "false"
        && snbt_number(s).is_none()
}

fn token(raw: &str, v: &Value) -> Result<String, String> {
    let suffix = snbt_number(raw).and_then(|(_, s)| s);
    Ok(match v {
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => match suffix {
            // A float setting given a whole number keeps its float letter.
            Some(c) => format!("{i}{c}"),
            None => i.to_string(),
        },
        v @ Value::Float(f) => {
            if !f.is_finite() {
                return Err("Type a number.".into());
            }
            match suffix {
                Some(c @ ('f' | 'F' | 'd' | 'D')) => format!("{}{c}", v.text()),
                _ => v.text(),
            }
        }
        Value::Str(s) => {
            if raw.starts_with('\'') {
                quote(s, '\'')
            } else if !raw.starts_with('"') && bare_ok(s) {
                s.clone()
            } else {
                quote(s, '"')
            }
        }
        Value::Text(t) => t.trim().to_string(),
    })
}

/// A list item typed in the editor: numbers and switches as they are
/// (`1.5d` included), anything else as quoted text.
fn item(i: &str, quoted: bool) -> String {
    if !quoted && (i == "true" || i == "false" || snbt_number(i).is_some()) { i.to_string() } else { quote(i, '"') }
}

pub(super) fn set(text: &str, path: &[String], key: &str, v: &Value) -> Result<String, String> {
    json_like::set_in(text, Dialect::Snbt, path, key, v, token, item)
}

#[cfg(test)]
mod tests {
    use super::super::{Entry, Kind};
    use super::*;

    const FTB: &str = "# FTB Chunks config file\n# Edit defaultconfigs instead\n\n{\n\t# Forced modes won't let players change their ally settings\n\t# Default: \"default\"\n\t# Valid values: \"default\", \"forced_all\", \"forced_none\"\n\tally_mode: \"default\"\n\t\n\t# Disables all land protection\n\t# Default: false\n\tdisable_protection: false\n\t\n\t# Interval in ticks\n\t# Default: 20\n\t# Range: 0 ~ 2147483647\n\tlong_range_tracker_interval: 60\n\t\n\t# Default: []\n\tno_wilderness_dimensions: [ ]\n\t\n\tmerge_tags: [\n\t\t\"minecraft:base_stone_overworld\"\n\t\t\"forge:ores/*\"\n\t]\n\t\n\t# Claiming settings\n\tclaiming: {\n\t\t# Default: 2.0f\n\t\t# Range: 0.0 ~ 10.0\n\t\tscale: 2.0f\n\t\tspeed: 1.5d\n\t\tbig: 10L\n\t\tsmall: 1b\n\t\tids: [I; 1, 2, 3]\n\t\tshape: octagon\n\t}\n}\n";

    fn entry<'a>(s: &'a [Section], path: &[&str], key: &str) -> &'a Entry {
        s.iter().flat_map(|s| &s.entries).find(|e| e.path == path && e.key == key).unwrap_or_else(|| panic!("no {key}"))
    }

    fn set1(t: &str, p: &[&str], k: &str, v: Value) -> String {
        set(t, &p.iter().map(|s| s.to_string()).collect::<Vec<_>>(), k, &v).unwrap()
    }

    #[test]
    fn reads_ftb_files() {
        let s = parse(FTB).unwrap();
        let a = entry(&s, &[], "ally_mode");
        assert_eq!((&a.value, a.line, a.default.as_deref()), (&Value::Str("default".into()), 8, Some("default")));
        assert_eq!(a.allowed, ["default", "forced_all", "forced_none"]);
        assert_eq!(a.comment, "Forced modes won't let players change their ally settings");
        assert_eq!(entry(&s, &[], "disable_protection").value, Value::Bool(false));
        let l = entry(&s, &[], "long_range_tracker_interval");
        assert_eq!((&l.value, l.range), (&Value::Int(60), Some((Some(0.0), Some(2147483647.0)))));
        assert_eq!(entry(&s, &[], "no_wilderness_dimensions").value, Value::Text("[]".into()));
        assert_eq!(entry(&s, &[], "no_wilderness_dimensions").is_default(), Some(true));
        assert_eq!(entry(&s, &[], "merge_tags").value, Value::Text("[\"minecraft:base_stone_overworld\", \"forge:ores/*\"]".into()));
        assert_eq!(entry(&s, &[], "merge_tags").kind(), Kind::List);
        let c = &s.iter().find(|s| s.path == ["claiming"]).unwrap();
        assert_eq!(c.comment, "Claiming settings");
        let sc = entry(&s, &["claiming"], "scale");
        assert_eq!((&sc.value, sc.is_default()), (&Value::Float(2.0), Some(true)));
        assert_eq!(entry(&s, &["claiming"], "speed").value, Value::Float(1.5));
        assert_eq!(entry(&s, &["claiming"], "big").value, Value::Int(10));
        assert_eq!(entry(&s, &["claiming"], "small").value, Value::Int(1));
        assert_eq!(entry(&s, &["claiming"], "ids").value, Value::Text("[1, 2, 3]".into()));
        assert_eq!(entry(&s, &["claiming"], "shape").value, Value::Str("octagon".into()));
    }

    #[test]
    fn edits_keep_type_letters_and_layout() {
        assert_eq!(set1(FTB, &["claiming"], "scale", Value::Float(3.5)), FTB.replace("scale: 2.0f", "scale: 3.5f"));
        assert_eq!(set1(FTB, &["claiming"], "scale", Value::Int(3)), FTB.replace("scale: 2.0f", "scale: 3f"));
        assert_eq!(set1(FTB, &["claiming"], "speed", Value::Float(2.0)), FTB.replace("1.5d", "2.0d"));
        assert_eq!(set1(FTB, &["claiming"], "big", Value::Int(99)), FTB.replace("10L", "99L"));
        assert_eq!(set1(FTB, &["claiming"], "small", Value::Int(0)), FTB.replace("small: 1b", "small: 0b"));
        assert_eq!(set1(FTB, &["claiming"], "ids", Value::Text("[4, 5]".into())), FTB.replace("[I; 1, 2, 3]", "[I; 4, 5]"));
        assert_eq!(set1(FTB, &["claiming"], "shape", Value::Str("circle".into())), FTB.replace("shape: octagon", "shape: circle"));
        assert_eq!(set1(FTB, &["claiming"], "shape", Value::Str("two words".into())), FTB.replace("shape: octagon", "shape: \"two words\""));
        assert_eq!(set1(FTB, &[], "ally_mode", Value::Str("forced_all".into())), FTB.replace("ally_mode: \"default\"", "ally_mode: \"forced_all\""));
        assert_eq!(set1(FTB, &[], "long_range_tracker_interval", Value::Int(20)), FTB.replace(": 60", ": 20"));
        assert_eq!(
            set1(FTB, &[], "merge_tags", Value::Text("[\"a\", \"b\", \"c\"]".into())),
            FTB.replace("[\n\t\t\"minecraft:base_stone_overworld\"\n\t\t\"forge:ores/*\"\n\t]", "[\n\t\t\"a\"\n\t\t\"b\"\n\t\t\"c\"\n\t]")
        );
        assert_eq!(set1(FTB, &[], "no_wilderness_dimensions", Value::Text("[\"minecraft:the_nether\"]".into())), FTB.replace("[ ]", "[\"minecraft:the_nether\"]"));
        // Nothing to change: the text stays as it was.
        assert_eq!(set1(FTB, &["claiming"], "scale", Value::Float(2.0)), FTB);
        assert_eq!(set1(FTB, &[], "merge_tags", Value::Text("[\"minecraft:base_stone_overworld\", \"forge:ores/*\"]".into())), FTB);
        assert!(set(FTB, &["nope".into()], "x", &Value::Int(1)).unwrap_err().contains("No nope"));
    }

    #[test]
    fn crlf_and_quests() {
        let t = FTB.replace('\n', "\r\n");
        let out = set1(&t, &[], "disable_protection", Value::Bool(true));
        assert_eq!(out, t.replace("disable_protection: false", "disable_protection: true"));
        let out = set1(&t, &[], "merge_tags", Value::Text("[\"x\"]".into()));
        assert!(out.contains("merge_tags: [\r\n\t\t\"x\"\r\n\t]\r\n"));
        // Quest files: lists of compounds are left to the text view.
        let q = "{\n\tid: \"000000000000044C\"\n\trewards: [\n\t\t{\n\t\t\tid: \"0452\"\n\t\t\ttype: \"xp\"\n\t\t\txp: 16\n\t\t}\n\t]\n\ticon: {\n\t\tid: \"powah:energizing_orb\"\n\t}\n}\n";
        let s = parse(q).unwrap();
        assert_eq!(entry(&s, &[], "rewards").kind(), Kind::Opaque);
        assert_eq!(entry(&s, &["icon"], "id").value, Value::Str("powah:energizing_orb".into()));
        assert!(parse("[1, 2]").is_err());
        assert!(parse("{ a: [1, 2 }").is_err());
    }

    #[test]
    fn flipping_every_switch() {
        let t = "{\n\ta: true\n\tb: { c: false, d: true }\n}\n";
        let mut text = t.to_string();
        let all: Vec<_> = parse(t).unwrap().into_iter().flat_map(|s| s.entries).collect();
        for e in &all {
            let Value::Bool(b) = e.value else { continue };
            text = set(&text, &e.path, &e.key, &Value::Bool(!b)).unwrap();
            assert_eq!(entry(&parse(&text).unwrap(), &e.path.iter().map(String::as_str).collect::<Vec<_>>(), &e.key).value, Value::Bool(!b));
        }
        assert_eq!(text, "{\n\ta: false\n\tb: { c: true, d: false }\n}\n");
    }
}
