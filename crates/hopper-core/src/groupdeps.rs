//! Required mods a friends group's pack lacks.
//!
//! A mod added to the group can need another mod nobody added (Shapewright
//! needs owo-lib). Group sync only carries what members added, so the pack
//! ends up one mod short and cannot start, while every PC says "In step".
//!
//! What a jar needs is read from the jars themselves ([`crate::deps`]: what
//! the loader checks at startup), the same list the Mods tab's "required mod
//! is missing" notice shows. This module turns that list into:
//!
//! - [`needs`]: what the pack lacks, by mod, for the sync status
//!   ([`crate::groupstatus::Status::NeedsMod`]);
//! - [`fresh`]: what an add or an update just made the pack lack, which the
//!   person who did it is asked about (the dialog adds it to the group);
//! - the words for both.
//!
//! Only the mods named by a plain "missing" count, as on the Mods tab: a
//! mod that is there in the wrong version is an update (Mods tab) and not
//! something to add.

use crate::deps::Missing;
use std::collections::HashSet;

/// One mod the pack needs and lacks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Need {
    /// Its mod id, as the jars that need it name it.
    pub id: String,
    /// What to show for it: the project's name once looked up, else the id.
    pub name: String,
    /// The versions the jars accept.
    pub versions: String,
    /// Who needs it: (the mod's name, its jar).
    pub by: Vec<(String, String)>,
    /// A jar that provides it is in the pack, turned off.
    pub off: Option<String>,
}

impl Need {
    /// The names of the mods that need it, each once, in order.
    pub fn needed_by(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for (name, _) in &self.by {
            if !out.contains(name) {
                out.push(name.clone());
            }
        }
        out
    }

    /// Adding it means downloading it (turning on a jar that is there
    /// needs no download).
    pub fn to_get(&self) -> bool {
        self.off.is_none()
    }
}

/// What a pack lacks, from [`crate::deps::missing`]'s list, one entry per
/// missing mod, sorted by id.
pub fn needs(missing: &[Missing]) -> Vec<Need> {
    let mut out: Vec<Need> = Vec::new();
    for m in missing.iter().filter(|m| m.wrong_version.is_none()) {
        let by = (m.mod_name.clone(), m.file_name.clone());
        match out.iter_mut().find(|n| n.id == m.needs) {
            Some(n) => {
                if !n.by.contains(&by) {
                    n.by.push(by);
                }
                if n.off.is_none() {
                    n.off = m.disabled_provider.clone();
                }
            }
            None => out.push(Need {
                id: m.needs.clone(),
                name: m.needs.clone(),
                versions: m.versions.clone(),
                by: vec![by],
                off: m.disabled_provider.clone(),
            }),
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// A need of one jar, for remembering what was asked about.
pub fn key(file: &str, id: &str) -> String {
    format!("{file}|{id}")
}

/// What an add or update made the pack lack: the needs in `after` of jars
/// that weren't in the mods folder before (`before`: the jar file names then)
/// and weren't asked about already (`told`: [`key`]s). A need an older jar
/// had stays out: the player didn't just cause it.
pub fn fresh(before: &HashSet<String>, after: &[Need], told: &HashSet<String>) -> Vec<Need> {
    after
        .iter()
        .filter_map(|n| {
            let by: Vec<(String, String)> =
                n.by.iter().filter(|(_, file)| !before.contains(file) && !told.contains(&key(file, &n.id))).cloned().collect();
            (!by.is_empty()).then(|| Need { by, ..n.clone() })
        })
        .collect()
}

/// "Shapewright", "Shapewright and Mekanism", "Shapewright, Mekanism and
/// Create".
pub fn and_list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
    }
}

/// The mod names behind a list of needs, each once.
fn needers(needs: &[Need]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in needs {
        for name in n.needed_by() {
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

/// The dialog that asks whether to add what a new mod needs to the group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    pub title: String,
    pub body: String,
    /// The button that says yes (the default).
    pub yes: String,
    pub no: String,
}

/// The words to ask `needs` (what an add made the pack lack) about, for
/// the group named `group`.
pub fn ask(needs: &[Need], group: &str) -> Ask {
    let names: Vec<String> = needs.iter().map(|n| n.name.clone()).collect();
    let who = and_list(&needers(needs));
    let it = if needs.len() == 1 { "it" } else { "them" };
    let verb = if needers(needs).len() == 1 { "needs" } else { "need" };
    let title = match needs {
        [one] => format!("Add {} to {group} too?", one.name),
        many => format!("Add {} mods to {group} too?", many.len()),
    };
    let body = format!(
        "{who} {verb} {}, and nobody has added {it} to {group}. The pack can't start without {it}. Add {it} so everyone in the group gets {it}.",
        and_list(&names)
    );
    Ask { title, body, yes: if needs.len() == 1 { "Add it".into() } else { "Add them".into() }, no: "Not now".into() }
}

/// The strip's label for a pack that lacks `n` mods.
pub fn label(n: usize) -> &'static str {
    if n == 1 { "Missing a required mod" } else { "Missing required mods" }
}

/// One piece of the strip's line: words, a missing mod (an index into the
/// needs the line was made from) or the mod that needs it (by its name).
/// The strip draws mods as links with their icons; [`detail`] is the same
/// line as plain text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Piece {
    Text(String),
    /// The need at this index.
    Need(usize),
    /// A mod in the pack that needs one, by name.
    By(String),
}

/// "A", "A and B", "A, B and C" as pieces.
fn list_pieces(items: Vec<Piece>) -> Vec<Piece> {
    let n = items.len();
    let mut out = Vec::new();
    for (i, p) in items.into_iter().enumerate() {
        if i > 0 {
            out.push(Piece::Text(if i + 1 == n { " and ".into() } else { ", ".into() }));
        }
        out.push(p);
    }
    out
}

/// The strip's line under the label as pieces: what is missing, what needs
/// it, and what to do.
pub fn detail_pieces(needs: &[Need]) -> Vec<Piece> {
    let text = |s: &str| Piece::Text(s.to_string());
    let get: Vec<(usize, &Need)> = needs.iter().enumerate().filter(|(_, n)| n.to_get()).collect();
    let off: Vec<(usize, &Need)> = needs.iter().enumerate().filter(|(_, n)| !n.to_get()).collect();
    let mut out: Vec<Piece> = Vec::new();
    if !get.is_empty() {
        let owned: Vec<Need> = get.iter().map(|(_, n)| (*n).clone()).collect();
        let by = needers(&owned);
        let (isnt, it) = if get.len() == 1 { ("isn't", "it") } else { ("aren't", "them") };
        out.extend(list_pieces(by.into_iter().map(Piece::By).collect()));
        out.push(text(if needers(&owned).len() == 1 { " needs " } else { " need " }));
        out.extend(list_pieces(get.iter().map(|(i, _)| Piece::Need(*i)).collect()));
        out.push(Piece::Text(format!(", which {isnt} in the pack. The pack can't start until you add {it}.")));
    }
    if !off.is_empty() {
        if !out.is_empty() {
            out.push(text(" "));
        }
        out.extend(list_pieces(off.iter().map(|(i, _)| Piece::Need(*i)).collect()));
        out.push(Piece::Text(format!(
            " {} in the pack but turned off. Turn {} on in the Mods tab.",
            if off.len() == 1 { "is" } else { "are" },
            if off.len() == 1 { "it" } else { "them" }
        )));
    }
    out
}

/// The strip's line under the label: what is missing, what needs it, and
/// what to do.
pub fn detail(needs: &[Need]) -> String {
    detail_pieces(needs)
        .into_iter()
        .map(|p| match p {
            Piece::Text(t) | Piece::By(t) => t,
            Piece::Need(i) => needs[i].name.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn missing(by: &str, file: &str, needs: &str) -> Missing {
        Missing {
            mod_name: by.into(),
            file_name: file.into(),
            needs: needs.into(),
            versions: "[0.12.15,)".into(),
            disabled_provider: None,
            wrong_version: None,
        }
    }

    /// A real jar like Shapewright's: a NeoForge mod that requires owo-lib.
    /// Adding owo-lib to the folder settles it.
    #[test]
    fn a_jar_that_needs_owo_lib_is_a_need_until_owo_lib_is_added() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("hopper-groupdeps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let jar = |name: &str, toml: &str| {
            let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            z.start_file::<_, ()>("META-INF/neoforge.mods.toml", Default::default()).unwrap();
            z.write_all(toml.as_bytes()).unwrap();
            std::fs::write(dir.join(name), z.finish().unwrap().into_inner()).unwrap();
        };
        jar(
            "shapewright-5.1.0+neoforge-1.21.1.jar",
            "modLoader=\"javafml\"
[[mods]]
modId=\"shapewright\"
version=\"5.1.0\"
displayName=\"Shapewright\"
             [[dependencies.shapewright]]
modId=\"owo-lib\"
type=\"required\"
versionRange=\"[0.12.15,)\"
side=\"BOTH\"
",
        );
        let got = needs(&crate::deps::missing(&dir));
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].id, "owo-lib");
        assert_eq!(got[0].needed_by(), vec!["Shapewright"]);
        assert!(got[0].to_get());
        jar("owo-lib-0.12.15.jar", "modLoader=\"javafml\"
[[mods]]
modId=\"owo-lib\"
version=\"0.12.15\"
");
        assert!(needs(&crate::deps::missing(&dir)).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_strip_line_is_words_and_mods() {
        let mut n = needs(&[missing("Spell Atlas", "atlas.jar", "arcane_framework")]);
        n[0].name = "Arcane Framework".into();
        assert_eq!(
            detail_pieces(&n),
            vec![
                Piece::By("Spell Atlas".into()),
                Piece::Text(" needs ".into()),
                Piece::Need(0),
                Piece::Text(", which isn't in the pack. The pack can't start until you add it.".into()),
            ]
        );
        assert_eq!(detail(&n), "Spell Atlas needs Arcane Framework, which isn't in the pack. The pack can't start until you add it.");
    }

    #[test]
    fn the_strip_line_lists_several_and_names_what_is_turned_off() {
        let mut off = missing("Create", "create.jar", "flywheel");
        off.disabled_provider = Some("flywheel.jar.disabled".into());
        let mut n = needs(&[
            missing("Shapewright", "s.jar", "owo-lib"),
            missing("Other", "o.jar", "owo-lib"),
            missing("Shapewright", "s.jar", "azurelib"),
            off,
        ]);
        for x in &mut n {
            x.name = x.id.clone();
        }
        let line = detail(&n);
        assert_eq!(
            line,
            "Shapewright and Other need azurelib and owo-lib, which aren't in the pack. The pack can't start until you add them. flywheel is in the pack but turned off. Turn it on in the Mods tab."
        );
        // The mods in the line are the needs, each once, in order.
        let pieces = detail_pieces(&n);
        let needs_in_line: Vec<usize> = pieces.iter().filter_map(|p| if let Piece::Need(i) = p { Some(*i) } else { None }).collect();
        assert_eq!(needs_in_line.len(), 3);
        assert!(pieces.contains(&Piece::By("Other".into())));
    }

    #[test]
    fn needs_group_by_missing_mod() {
        let list = vec![
            missing("Shapewright", "shapewright.jar", "owo-lib"),
            missing("Other", "other.jar", "owo-lib"),
            missing("Shapewright", "shapewright.jar", "azurelib"),
        ];
        let got = needs(&list);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].id, "azurelib");
        assert_eq!(got[1].id, "owo-lib");
        assert_eq!(got[1].needed_by(), vec!["Shapewright", "Other"]);
    }

    #[test]
    fn a_wrong_version_is_an_update_not_a_missing_mod() {
        let mut m = missing("Shapewright", "shapewright.jar", "owo-lib");
        m.wrong_version = Some(("owo-lib-0.10.jar".into(), "0.10.0".into()));
        assert!(needs(&[m]).is_empty());
    }

    #[test]
    fn a_turned_off_provider_is_kept_and_not_downloaded() {
        let mut m = missing("Shapewright", "shapewright.jar", "owo-lib");
        m.disabled_provider = Some("owo-lib.jar.disabled".into());
        let got = needs(&[m]);
        assert_eq!(got[0].off.as_deref(), Some("owo-lib.jar.disabled"));
        assert!(!got[0].to_get());
    }

    fn files(names: &[&str]) -> HashSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn fresh_is_what_the_add_caused() {
        let after = needs(&[missing("Old", "old.jar", "geckolib"), missing("Shapewright", "shapewright.jar", "owo-lib")]);
        let got = fresh(&files(&["old.jar"]), &after, &HashSet::new());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "owo-lib");
    }

    #[test]
    fn fresh_keeps_a_new_mod_that_needs_what_an_old_mod_lacked_too() {
        // geckolib was already missing for Old; the new mod needs it too.
        let after = needs(&[missing("Old", "old.jar", "geckolib"), missing("New", "new.jar", "geckolib")]);
        let got = fresh(&files(&["old.jar"]), &after, &HashSet::new());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].by, vec![("New".to_string(), "new.jar".to_string())]);
    }

    #[test]
    fn fresh_skips_what_was_asked_already() {
        let after = needs(&[missing("Shapewright", "shapewright.jar", "owo-lib")]);
        let told: HashSet<String> = [key("shapewright.jar", "owo-lib")].into();
        assert!(fresh(&HashSet::new(), &after, &told).is_empty());
    }

    #[test]
    fn fresh_is_empty_when_nothing_is_missing() {
        assert!(fresh(&HashSet::new(), &[], &HashSet::new()).is_empty());
    }

    #[test]
    fn ask_names_the_mod_what_needs_it_and_the_group() {
        let mut n = needs(&[missing("Shapewright", "shapewright.jar", "owo-lib")]);
        n[0].name = "owo-lib".into();
        let a = ask(&n, "Testers");
        assert_eq!(a.title, "Add owo-lib to Testers too?");
        assert!(a.body.starts_with("Shapewright needs owo-lib, and nobody has added it to Testers."), "{}", a.body);
        assert_eq!(a.yes, "Add it");
    }

    #[test]
    fn ask_says_them_for_several() {
        let n = needs(&[missing("Shapewright", "s.jar", "owo-lib"), missing("Shapewright", "s.jar", "azurelib")]);
        let a = ask(&n, "Testers");
        assert_eq!(a.title, "Add 2 mods to Testers too?");
        assert!(a.body.starts_with("Shapewright needs azurelib and owo-lib, and nobody has added them"), "{}", a.body);
        assert!(a.body.contains("added them to Testers"));
        assert_eq!(a.yes, "Add them");
    }

    #[test]
    fn detail_tells_what_to_do() {
        let n = needs(&[missing("Shapewright", "s.jar", "owo-lib")]);
        assert_eq!(detail(&n), "Shapewright needs owo-lib, which isn't in the pack. The pack can't start until you add it.");
        let mut off = missing("Shapewright", "s.jar", "owo-lib");
        off.disabled_provider = Some("x.jar.disabled".into());
        assert!(detail(&needs(&[off])).contains("turned off"));
    }

    #[test]
    fn label_counts() {
        assert_eq!(label(1), "Missing a required mod");
        assert_eq!(label(2), "Missing required mods");
    }
}
