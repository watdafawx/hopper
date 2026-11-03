//! Resource packs, shader packs and data packs from GitHub (a release's
//! zip, or a folder on a branch: [`super::tree`]) and other links.
//!
//! Packs often sit in loose folders (Paxi's, Global Packs', a world's
//! `datapacks/`) that have no records of their own, so what Hopper knows
//! about a pack from a source is kept on this PC: its file name, its
//! sha256 and the source. A pack file with a matching name and hash is
//! that pack wherever it is, switched on or off, so the Packs tab shows it
//! and its update check asks the source ([`identify`]).

use super::{Candidate, ModSource, Resolved, tree};
use crate::cf;
use crate::jobs::Job;
use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// One pack Hopper put in place from a source.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Record {
    pub file_name: String,
    pub sha256: String,
    pub name: String,
    /// `seen` is the commit or release the file is.
    pub source: ModSource,
    /// The commit's or release's date (ISO 8601), so a newer one is told
    /// apart from the same one.
    pub date: String,
}

fn records_file() -> Option<PathBuf> {
    crate::paths::dirs().map(|d| d.data_dir().join("pack-sources.json"))
}

static RECORDS: Mutex<Option<Vec<Record>>> = Mutex::new(None);

fn with_records<T>(f: impl FnOnce(&mut Vec<Record>) -> T) -> T {
    let mut g = RECORDS.lock().unwrap_or_else(|e| e.into_inner());
    let list = g.get_or_insert_with(|| {
        records_file()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    });
    f(list)
}

/// Every pack recorded on this PC.
pub fn records() -> Vec<Record> {
    with_records(|l| l.clone())
}

/// Remembers the pack file at `path` as coming from `src`.
pub fn record(path: &Path, src: &ModSource, name: &str, date: &str) -> anyhow::Result<()> {
    let sha = super::sha256_file(path)?;
    let file_name = crate::instance_ops::enabled_path(path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let rec = Record { file_name, sha256: sha, name: name.to_string(), source: src.clone(), date: date.to_string() };
    let body = with_records(|l| {
        l.retain(|r| !(r.sha256 == rec.sha256 && r.file_name.eq_ignore_ascii_case(&rec.file_name)));
        l.push(rec);
        // Old versions pile up as packs update; the newest few hundred do.
        if l.len() > 400 {
            let extra = l.len() - 400;
            l.drain(..extra);
        }
        serde_json::to_vec_pretty(l)
    })?;
    if let Some(f) = records_file() {
        if let Some(p) = f.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(f, body)?;
    }
    Ok(())
}

/// The record of the pack file at `path` (switched on or off), when its
/// name and contents are those of a pack from a source.
pub fn find(path: &Path) -> Option<Record> {
    if !path.is_file() {
        return None;
    }
    let name = crate::instance_ops::enabled_path(path).file_name()?.to_string_lossy().into_owned();
    let named: Vec<Record> = with_records(|l| l.iter().filter(|r| r.file_name.eq_ignore_ascii_case(&name)).cloned().collect());
    if named.is_empty() {
        return None;
    }
    let sha = super::sha256_file(path).ok()?;
    named.into_iter().rev().find(|r| r.sha256 == sha)
}

/// The project id a pack from `src` goes by.
pub fn id_of(src: &ModSource) -> u64 {
    super::src_id(&format!("{}|{}", src.key(), src.pattern))
}

/// The installed file of a recorded pack, as the update code compares it.
pub fn installed_file(id: u64, r: &Record) -> cf::File {
    let c = Candidate {
        file_name: r.file_name.clone(),
        version: r.source.seen.clone(),
        title: title_of(&r.source, &r.source.seen, &r.date),
        date: r.date.clone(),
        page: r.source.page(),
        ..Default::default()
    };
    super::to_file(id, &r.source, &c, "", cf::Loader::Any, false)
}

/// A version's name: the commit for a branch folder, else the tag.
pub fn title_of(src: &ModSource, seen: &str, date: &str) -> String {
    if tree::of_url(&src.url).is_some() { tree::label(seen, date) } else { seen.to_string() }
}

/// The packs among `paths` that came from a source: their project (for
/// the row) and installed file (for the update check).
pub fn identify(paths: &[PathBuf]) -> HashMap<PathBuf, (cf::Mod, cf::File)> {
    let mut out = HashMap::new();
    for p in paths {
        let Some(r) = find(p) else { continue };
        let id = id_of(&r.source);
        super::remember(id, &r.source, &r.name);
        out.insert(p.clone(), (super::mod_for(id, &r.source, &r.name), installed_file(id, &r)));
    }
    out
}

/// A recorded pack's source, its version, and the newer file its source
/// has (asks the source: one cached call for a branch folder).
pub fn update_of(path: &Path) -> Option<(Record, cf::File, Option<cf::File>)> {
    let r = find(path)?;
    let id = id_of(&r.source);
    super::remember(id, &r.source, &r.name);
    let installed = installed_file(id, &r);
    let newer = super::files(id, "", cf::Loader::Any)
        .map_err(|e| log::warn!("{}: {e:#}", r.file_name))
        .ok()
        .and_then(|l| l.into_iter().next())
        .filter(|f| f.id != installed.id && f.file_date > installed.file_date);
    Some((r, installed, newer))
}

/// A pack class Hopper adds from sources.
pub fn is_pack(class: u32) -> bool {
    matches!(class, cf::CLASS_RESOURCE_PACKS | cf::CLASS_SHADERS | cf::CLASS_DATAPACKS)
}

/// Looks up a pasted link for a pack of `class`: a branch folder, or the
/// zips a source's releases have.
pub fn resolve(link: &str, mc: &str, class: u32, prerelease: bool) -> anyhow::Result<Resolved> {
    let mut src = super::parse_link(link)?;
    src.prerelease = prerelease;
    src.class = class;
    if let Some(t) = tree::of_url(&src.url) {
        let candidates = tree::resolve(&t, class)?;
        let one = candidates.len() == 1;
        let name = if t.path.is_empty() { t.repo.clone() } else { t.path.rsplit('/').next().unwrap_or(&t.repo).to_string() };
        let mut r = Resolved { source: src, name, candidates, pick: one.then_some(0), ambiguous: !one };
        if one {
            r.source.pattern = super::pattern_of(&r.candidates[0].file_name);
        }
        return Ok(r);
    }
    super::resolve_class(link, src, mc, cf::Loader::Any)
}

/// The project and file for candidate `idx` of `r`. A folder picked one
/// level down becomes the source itself, with its own newest commit.
fn project_and_file(r: &Resolved, idx: usize) -> anyhow::Result<(cf::Mod, cf::File, ModSource, String)> {
    let mut c = r.candidates.get(idx).context("no such file")?.clone();
    let mut src = r.source.clone();
    let mut name = r.name.clone();
    if let Some(t) = tree::of_url(&src.url) {
        let picked = tree::parse_link(&c.page).unwrap_or(t.clone());
        if picked != t {
            let head = tree::head(&picked)?;
            let size = c.size;
            c = tree::candidate(&picked, &head, size);
            name = picked.path.rsplit('/').next().unwrap_or(&name).to_string();
        }
        src.url = picked.url();
    }
    src.pattern = super::pattern_of(&c.file_name);
    let id = id_of(&src);
    super::remember(id, &src, &name);
    let f = super::to_file(id, &src, &c, "", cf::Loader::Any, false);
    Ok((super::mod_for(id, &src, &name), f, src, name))
}

/// Puts pack `idx` of a resolved link into `dir` of the instance at
/// `root` (its own pack folder, Paxi's, a world's `datapacks/`). A pack
/// with the same file name there is replaced. Returns the new file.
pub fn install(root: &Path, dir: &Path, r: &Resolved, idx: usize, job: &Job) -> anyhow::Result<PathBuf> {
    let (m, f, _, _) = project_and_file(r, idx)?;
    let old = dir.join(&f.file_name);
    let old = [old.clone(), PathBuf::from(format!("{}.disabled", old.display()))].into_iter().find(|p| p.is_file());
    match crate::instance_ops::put_pack(root, dir, &m, &f, old.as_deref(), job)? {
        crate::instance_ops::Placed::File(p) => Ok(p),
        crate::instance_ops::Placed::Pending(_) => bail!("{} can't be downloaded by Hopper", f.file_name),
    }
}

/// Gets a pack file from its source into Hopper's download folder: a
/// branch folder is built from the repo at its commit, anything else
/// downloaded. Checks it's a pack (and the stated sha256). Called by the
/// pack install code for files from sources.
pub fn fetch(f: &cf::File, s: &super::SrcFile, job: &Job) -> anyhow::Result<PathBuf> {
    let dir = crate::paths::temp_dir().join("hopper-downloads");
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(&f.file_name);
    let class = if is_pack(s.source.class) { s.source.class } else { cf::CLASS_RESOURCE_PACKS };
    let total = f.file_length;
    let tick = |n: u64| {
        if total > 0 {
            job.progress(n.min(total) as usize, total as usize);
        }
    };
    if let Some(t) = tree::of_url(&s.source.url) {
        job.detail(format!("Downloading {} at {}", t.repo, &s.source.seen[..s.source.seen.len().min(7)]));
        tree::build(&t, &s.source.seen, class, &dest, tick)?;
    } else {
        let url = f.download_url.clone().context("no download link")?;
        job.detail(format!("Downloading {}", f.file_name));
        cf::download(&url, &dest, tick)?;
    }
    if let Err(e) = check(&dest, s, class) {
        let _ = std::fs::remove_file(&dest);
        return Err(e.context(format!("{} wasn't added", f.file_name)));
    }
    Ok(dest)
}

/// A downloaded pack is a zip of its kind, with the sha256 the source
/// states (when it states one).
pub fn check(path: &Path, s: &super::SrcFile, class: u32) -> anyhow::Result<()> {
    let sha = super::sha256_file(path)?;
    if !s.digest.is_empty() && !s.digest.eq_ignore_ascii_case(&sha) {
        bail!("the download doesn't match the checksum the source lists, so Hopper didn't keep it");
    }
    let z = zip::ZipArchive::new(std::fs::File::open(path)?).context("the download isn't a zip")?;
    let names: Vec<&str> = z.file_names().collect();
    let ok = if class == cf::CLASS_SHADERS {
        names.iter().any(|n| n.starts_with("shaders/"))
    } else {
        names.contains(&"pack.mcmeta")
    };
    if !ok {
        if class == cf::CLASS_SHADERS {
            bail!("it isn't a shader pack (no shaders folder at the top)");
        }
        bail!("it isn't a pack (no pack.mcmeta at the top)");
    }
    Ok(())
}
