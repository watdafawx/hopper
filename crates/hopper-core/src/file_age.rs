//! Which of two mod or pack files is newer, for any source.
//!
//! Only CurseForge file ids grow with each upload. A Modrinth file's id and
//! a GitHub / Maven / Jenkins / link file's id are hashes of the version, so
//! "bigger id = newer" is a coin toss for them: it showed the newest GitHub
//! release as "4 versions behind" and offered "Update to" for old versions
//! next to "Roll back to" for others. Every "is this newer?" goes through
//! [`Age`] instead.
//!
//! The order, for files that are not both CurseForge:
//! 1. both have a release tag that reads as a version ("v1.23.1"): compare
//!    the versions. A source lists releases by tag, and a release's asset
//!    can be re-uploaded later (which moves its date), so the tag is the
//!    steadier clue;
//! 2. otherwise the publish date (what Modrinth goes by; GitHub asset dates
//!    are the fallback for tags like "build-7");
//! 3. otherwise they count as the same age.
//!
//! A [`Age`] is plain data so the installed file can be described from what
//! an instance stores (an [`Addon`]) when its [`cf::File`] isn't to hand.

use crate::cf;
use crate::instances::Addon;
use crate::sources::{Kind, ModSource, tag_version, tree};
use std::cmp::Ordering;

/// What decides how old a file is. Build with [`Age::of`], [`Age::of_addon`]
/// or [`Age::of_id`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Age {
    id: u64,
    /// The id is a CurseForge file id, which grows with time.
    by_id: bool,
    date: String,
    /// The release tag as a version ("1.23.1"), when it reads as one.
    version: String,
}

/// The tag of a source's release as a plain version, when it is one. A
/// link's ETag or a branch's commit hash only looks like a version.
fn version_of(src: &ModSource) -> String {
    if src.kind == Kind::Url || tree::of_url(&src.url).is_some() {
        return String::new();
    }
    let v = tag_version(&src.seen);
    if v.starts_with(|c: char| c.is_ascii_digit()) { v } else { String::new() }
}

impl Age {
    pub fn of(f: &cf::File) -> Age {
        let by_id = f.source.is_none() && f.modrinth.is_none() && !crate::modrinth::is_mr(f.id);
        Age {
            id: f.id,
            by_id,
            date: f.file_date.clone(),
            version: f.source.as_ref().map(|s| version_of(&s.source)).unwrap_or_default(),
        }
    }

    /// The installed file of a mod, from what the instance stores.
    pub fn of_addon(a: &Addon) -> Age {
        let by_id = a.source.is_none() && a.mr_version.is_empty() && !crate::modrinth::is_mr(a.file_id);
        Age {
            id: a.file_id,
            by_id,
            date: a.file_date.clone(),
            version: a.source.as_ref().map(version_of).unwrap_or_default(),
        }
    }

    /// Only an id is known (a pack's file in `pack.json`). It reads as a
    /// CurseForge id unless it is a Modrinth one; a pack from another
    /// source has no clue left but the date of the file found by the id
    /// (see [`Age::of_id_in`]).
    pub fn of_id(id: u64) -> Age {
        Age { id, by_id: !crate::modrinth::is_mr(id), ..Default::default() }
    }

    /// The file with this id in `files` when listed, else [`Age::of_id`].
    pub fn of_id_in(id: u64, files: &[cf::File]) -> Age {
        files.iter().find(|f| f.id == id).map(Age::of).unwrap_or_else(|| Age::of_id(id))
    }

    /// The installed file `file_id` of a mod, as well as can be told: the
    /// instance's own record first (it has the release tag), then the file
    /// in `files`, else just the id.
    pub fn installed(inst: &crate::instances::Instance, project_id: u64, file_id: u64, files: &[cf::File]) -> Age {
        match inst.addons.iter().find(|a| a.project_id == project_id && a.file_id == file_id) {
            Some(a) => {
                let mut age = Age::of_addon(a);
                if age.date.is_empty() {
                    age.date = Age::of_id_in(file_id, files).date;
                }
                age
            }
            None => Age::of_id_in(file_id, files),
        }
    }

    /// Another file of the same project as `self`, known by id and date only
    /// (an update offer saved as text): the same kind of id, no tag.
    pub fn offer_like(&self, id: u64, date: &str) -> Age {
        Age { id, by_id: self.by_id, date: date.to_string(), version: String::new() }
    }

    /// The file's id is a CurseForge one, so the id alone says how old it is.
    pub fn id_orders(&self) -> bool {
        self.by_id
    }

    /// How `self` compares with `other`: `Less` is older.
    pub fn cmp(&self, other: &Age) -> Ordering {
        // An id of 0 is "unknown", not the oldest file.
        if self.by_id && other.by_id && self.id != 0 && other.id != 0 {
            return self.id.cmp(&other.id);
        }
        if !self.version.is_empty() && !other.version.is_empty() {
            let o = crate::scan::compare_versions(&self.version, &other.version);
            if o != Ordering::Equal {
                return o;
            }
        }
        cmp_dates(&self.date, &other.date)
    }

    /// `self` is a later upload than `other`.
    pub fn newer_than(&self, other: &Age) -> bool {
        self.cmp(other) == Ordering::Greater
    }
}

/// Dates as ISO 8601 text. Whole minutes through [`crate::time::parse_iso`],
/// then the text up to the second so two uploads in one minute still order.
/// A missing date equals everything.
fn cmp_dates(a: &str, b: &str) -> Ordering {
    if a.is_empty() || b.is_empty() {
        return Ordering::Equal;
    }
    match (crate::time::parse_iso(a), crate::time::parse_iso(b)) {
        (Some(x), Some(y)) if x != y => x.cmp(&y),
        _ => a.get(..19).unwrap_or(a).cmp(b.get(..19).unwrap_or(b)),
    }
}

impl cf::File {
    /// This file as a [`Age`].
    pub fn age(&self) -> Age {
        Age::of(self)
    }

    /// Whether this file was uploaded after `other`, whichever site they
    /// are from.
    pub fn newer_than(&self, other: &cf::File) -> bool {
        Age::of(self).newer_than(&Age::of(other))
    }
}

/// The newest of `files`; the first of equals.
pub fn newest<'a>(files: impl IntoIterator<Item = &'a cf::File>) -> Option<&'a cf::File> {
    let mut best: Option<&cf::File> = None;
    for f in files {
        if best.is_none_or(|b| f.newer_than(b)) {
            best = Some(f);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::SrcFile;

    fn gh(version: &str, date: &str) -> cf::File {
        // A hash id, like `sources::file_id`: unrelated to the order.
        let id = crate::sources::src_id(&format!("{version}|mod-{version}.jar|")) & !(1 << 62) | 1;
        cf::File {
            id,
            display_name: format!("v{version}"),
            file_name: format!("mod-{version}.jar"),
            file_date: date.into(),
            source: Some(Box::new(SrcFile {
                source: ModSource { kind: Kind::GitHub, url: "foxs/tweaks".into(), seen: format!("v{version}"), ..Default::default() },
                ..Default::default()
            })),
            ..Default::default()
        }
    }

    /// 1.18.0 .. 1.23.1 in an order unrelated to their hash ids.
    fn releases() -> Vec<cf::File> {
        let v = [
            ("1.23.1", "2026-11-01T10:00:00Z"),
            ("1.20.0", "2026-03-01T10:00:00Z"),
            ("1.22.0", "2026-09-01T10:00:00Z"),
            ("1.18.0", "2025-12-01T10:00:00Z"),
            ("1.20.2", "2026-05-01T10:00:00Z"),
            ("1.19.0", "2026-01-01T10:00:00Z"),
            ("1.21.0", "2026-06-01T10:00:00Z"),
            ("1.20.1", "2026-04-01T10:00:00Z"),
            ("1.23.0", "2026-10-01T10:00:00Z"),
        ];
        v.iter().map(|(v, d)| gh(v, d)).collect()
    }

    fn names(fs: &[&cf::File]) -> Vec<String> {
        fs.iter().map(|f| f.display_name.clone()).collect()
    }

    #[test]
    fn github_installed_newest_has_nothing_newer() {
        let all = releases();
        let installed = all.iter().find(|f| f.display_name == "v1.23.1").unwrap().age();
        assert!(all.iter().all(|f| !f.age().newer_than(&installed)));
        // Every other row is older: "Roll back to".
        assert_eq!(all.iter().filter(|f| installed.newer_than(&f.age())).count(), all.len() - 1);
    }

    #[test]
    fn github_installed_middle_gets_the_newest_as_target() {
        let all = releases();
        let installed = all.iter().find(|f| f.display_name == "v1.20.0").unwrap().age();
        let mut newer: Vec<&cf::File> = all.iter().filter(|f| f.age().newer_than(&installed)).collect();
        newer.sort_by(|a, b| a.age().cmp(&b.age()));
        assert_eq!(names(&newer), ["v1.20.1", "v1.20.2", "v1.21.0", "v1.22.0", "v1.23.0", "v1.23.1"]);
        assert_eq!(newest(all.iter().filter(|f| f.age().newer_than(&installed))).unwrap().display_name, "v1.23.1");
    }

    /// The ids are no help: shuffling them must not change a thing.
    #[test]
    fn hash_ids_do_not_matter() {
        let mut all = releases();
        for (i, f) in all.iter_mut().enumerate() {
            f.id = (all_len_hash(i)) | 1;
        }
        assert_eq!(newest(all.iter()).unwrap().display_name, "v1.23.1");
    }
    fn all_len_hash(i: usize) -> u64 {
        (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 2
    }

    /// A re-uploaded old asset has a fresh date: the tag still says it's old.
    #[test]
    fn tag_beats_a_moved_date() {
        let old = gh("1.18.0", "2026-11-05T00:00:00Z");
        let new = gh("1.23.1", "2026-11-01T00:00:00Z");
        assert!(new.newer_than(&old));
        assert!(!old.newer_than(&new));
    }

    /// Tags that are not versions go by date.
    #[test]
    fn non_version_tags_go_by_date() {
        let a = |tag: &str, date: &str| {
            let mut f = gh("1.0.0", date);
            f.source.as_mut().unwrap().source.seen = tag.into();
            f
        };
        let (old, new) = (a("build-7", "2026-01-01T00:00:00Z"), a("nightly", "2026-02-01T00:00:00Z"));
        assert!(new.newer_than(&old));
        assert!(!old.newer_than(&new));
        // A link's ETag or a branch's commit that happens to start with a
        // digit is not a version.
        let mut url = gh("1.0.0", "2026-01-01T00:00:00Z");
        url.source.as_mut().unwrap().source.kind = Kind::Url;
        url.source.as_mut().unwrap().source.seen = "9f2".into();
        assert!(url.age().version.is_empty());
    }

    #[test]
    fn curseforge_ids_still_order() {
        let cf_file = |id| cf::File { id, file_date: "2020-01-01T00:00:00Z".into(), ..Default::default() };
        let (a, b) = (cf_file(4_000_000), cf_file(6_500_000));
        assert!(b.newer_than(&a));
        assert!(!a.newer_than(&b));
        assert!(!a.newer_than(&a));
        assert_eq!(newest([&a, &b]).unwrap().id, 6_500_000);
        // An installed CurseForge file known only by id.
        assert!(b.age().newer_than(&Age::of_id(4_000_000)));
        assert!(!a.age().newer_than(&Age::of_id(6_500_000)));
    }

    #[test]
    fn modrinth_goes_by_date() {
        let mr = |name: &str, date: &str| {
            let v = crate::modrinth::Version {
                id: name.into(),
                project_id: "P".into(),
                name: name.into(),
                version_number: name.into(),
                date_published: date.into(),
                ..Default::default()
            };
            crate::modrinth::version_to_file(&v)
        };
        // Ids are hashes: build both orders of names to catch an id compare.
        for (x, y) in [("aaa", "zzz"), ("zzz", "aaa")] {
            let (old, new) = (mr(x, "2026-01-01T10:00:00.100Z"), mr(y, "2026-03-01T10:00:00.250Z"));
            assert!(new.newer_than(&old), "{x} {y}");
            assert!(!old.newer_than(&new), "{x} {y}");
        }
        // Same minute: the seconds decide.
        let (a, b) = (mr("a", "2026-01-01T10:00:05Z"), mr("b", "2026-01-01T10:00:40Z"));
        assert!(b.newer_than(&a) && !a.newer_than(&b));
        // A CurseForge file against a Modrinth one: by date too.
        let cff = cf::File { id: 9_000_000, file_date: "2020-01-01T00:00:00Z".into(), ..Default::default() };
        assert!(a.newer_than(&cff));
    }

    #[test]
    fn installed_addon_matches_its_file() {
        let all = releases();
        let f = all.iter().find(|f| f.display_name == "v1.20.0").unwrap();
        let addon = Addon {
            file_id: f.id,
            file_date: f.file_date.clone(),
            source: f.source.as_ref().map(|s| s.source.clone()),
            ..Default::default()
        };
        assert_eq!(Age::of_addon(&addon), f.age());
        assert_eq!(Age::of_id_in(f.id, &all), f.age());
    }
}
