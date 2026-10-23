//! Share codes: a short string that sets up the exact pack someone else has.
//!
//! A code names the modpack and its version (CurseForge or Modrinth), then
//! only what differs from it: mods added or moved to another file, pack
//! mods removed or turned off, and the loader when it was changed. Without
//! a pack it names the Minecraft version, the loader and every mod. Nothing
//! is uploaded anywhere; the code itself carries all of it.
//!
//! Layout (then base64url, after the `HOP-` prefix):
//! `version, flags, pack, [name], [loader], mask, groups…, check`. Numbers
//! are LEB128 varints, strings a varint length and UTF-8. `mask` has a bit
//! per group present, and each present group is a count and its items:
//! CurseForge files and projects as sorted deltas (a file id is enough,
//! CurseForge says whose it is), Modrinth versions (the same) and file
//! names as strings. `check` is the first two bytes of the SHA-1 of
//! everything before it. When deflate makes the body smaller, the version
//! byte has its top bit set and the rest is deflated.

use crate::cf;
use crate::instances::{Addon, Instance};
use crate::jobs::Job;
use crate::modrinth::{self, is_mr, num_id};
use anyhow::{Context, bail, ensure};
use base64::Engine as _;
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub const PREFIX: &str = "HOP-";
const VERSION: u8 = 1;

const PACK_NONE: u8 = 0;
const PACK_CF: u8 = 1;
const PACK_MR: u8 = 2;
const HAS_NAME: u8 = 1 << 2;
const HAS_LOADER: u8 = 1 << 3;

const DEFLATED: u8 = 0x80;

/// The pack a code starts from.
#[derive(Clone, Debug, PartialEq)]
pub enum Base {
    /// No pack: an empty instance with this Minecraft and loader.
    Plain { game_version: String, loader: String },
    Curse { project: u64, file: u64 },
    Modrinth { project: String, version: String },
}

/// A pack mod, by CurseForge project or by file name.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Curse(u64),
    Named(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    /// This CurseForge file, in place of whatever file of its project is there.
    CurseFile { file: u64, enabled: bool },
    /// This Modrinth version, the same way.
    ModrinthFile { version: String, enabled: bool },
    /// A pack mod that isn't there.
    Remove(Target),
    /// A pack mod that's turned off.
    Off(Target),
}

/// What a code describes.
#[derive(Clone, Debug, PartialEq)]
pub struct Share {
    pub base: Base,
    /// The instance's name, when it isn't the pack's.
    pub name: Option<String>,
    /// The loader, when it isn't the pack's.
    pub loader: Option<String>,
    pub entries: Vec<Entry>,
}

impl Share {
    /// "3 mods added or changed, 1 removed, 2 off".
    pub fn summary(&self) -> String {
        let (mut files, mut removed, mut off) = (0, 0, 0);
        for e in &self.entries {
            match e {
                Entry::CurseFile { .. } | Entry::ModrinthFile { .. } => files += 1,
                Entry::Remove(_) => removed += 1,
                Entry::Off(_) => off += 1,
            }
        }
        let mut parts = Vec::new();
        let plain = matches!(self.base, Base::Plain { .. });
        if files > 0 {
            let s = if files == 1 { "" } else { "s" };
            parts.push(if plain {
                format!("{files} mod{s}")
            } else {
                format!("{files} mod{s} added or changed")
            });
        }
        if removed > 0 {
            parts.push(format!("{removed} removed"));
        }
        if off > 0 {
            parts.push(format!("{off} turned off"));
        }
        if parts.is_empty() {
            return if plain { "no mods".into() } else { "the pack as published".into() };
        }
        parts.join(", ")
    }
}

// ── the text ────────────────────────────────────────────────────────

pub fn encode(s: &Share) -> String {
    let mut b = Vec::new();
    let mut flags = match s.base {
        Base::Plain { .. } => PACK_NONE,
        Base::Curse { .. } => PACK_CF,
        Base::Modrinth { .. } => PACK_MR,
    };
    if s.name.is_some() {
        flags |= HAS_NAME;
    }
    if s.loader.is_some() {
        flags |= HAS_LOADER;
    }
    b.push(flags);
    match &s.base {
        Base::Plain { game_version, loader } => {
            put_str(&mut b, game_version);
            put_str(&mut b, loader);
        }
        Base::Curse { project, file } => {
            put_num(&mut b, *project);
            put_num(&mut b, *file);
        }
        Base::Modrinth { project, version } => {
            put_str(&mut b, project);
            put_str(&mut b, version);
        }
    }
    if let Some(n) = &s.name {
        put_str(&mut b, n);
    }
    if let Some(l) = &s.loader {
        put_str(&mut b, l);
    }
    // One bit per group that has anything, then those groups.
    let g = Groups::of(&s.entries);
    let nums = [&g.cf_on, &g.cf_off, &g.remove_cf, &g.off_cf];
    let strs = [&g.mr_on, &g.mr_off, &g.remove_named, &g.off_named];
    let mut mask = 0u8;
    for (i, v) in nums.iter().enumerate() {
        mask |= u8::from(!v.is_empty()) << i;
    }
    for (i, v) in strs.iter().enumerate() {
        mask |= u8::from(!v.is_empty()) << (4 + i);
    }
    b.push(mask);
    for v in nums.iter().filter(|v| !v.is_empty()) {
        put_sorted(&mut b, v);
    }
    for v in strs.iter().filter(|v| !v.is_empty()) {
        put_strs(&mut b, v);
    }

    let mut out = vec![VERSION];
    let packed = deflate(&b);
    if packed.len() < b.len() {
        out[0] |= DEFLATED;
        out.extend_from_slice(&packed);
    } else {
        out.extend_from_slice(&b);
    }
    let check = sha1_smol::Sha1::from(&out).digest().bytes();
    out.extend_from_slice(&check[..2]);
    format!(
        "{PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&out)
    )
}

/// Entries by kind, in the order the code stores them.
#[derive(Default)]
struct Groups {
    cf_on: Vec<u64>,
    cf_off: Vec<u64>,
    mr_on: Vec<String>,
    mr_off: Vec<String>,
    remove_cf: Vec<u64>,
    remove_named: Vec<String>,
    off_cf: Vec<u64>,
    off_named: Vec<String>,
}

impl Groups {
    fn of(entries: &[Entry]) -> Self {
        let mut g = Groups::default();
        for e in entries {
            match e {
                Entry::CurseFile { file, enabled: true } => g.cf_on.push(*file),
                Entry::CurseFile { file, enabled: false } => g.cf_off.push(*file),
                Entry::ModrinthFile { version, enabled: true } => g.mr_on.push(version.clone()),
                Entry::ModrinthFile { version, enabled: false } => g.mr_off.push(version.clone()),
                Entry::Remove(Target::Curse(p)) => g.remove_cf.push(*p),
                Entry::Remove(Target::Named(n)) => g.remove_named.push(n.clone()),
                Entry::Off(Target::Curse(p)) => g.off_cf.push(*p),
                Entry::Off(Target::Named(n)) => g.off_named.push(n.clone()),
            }
        }
        g
    }

    fn entries(self) -> Vec<Entry> {
        let mut v = Vec::new();
        v.extend(self.cf_on.into_iter().map(|file| Entry::CurseFile { file, enabled: true }));
        v.extend(self.cf_off.into_iter().map(|file| Entry::CurseFile { file, enabled: false }));
        v.extend(self.mr_on.into_iter().map(|version| Entry::ModrinthFile { version, enabled: true }));
        v.extend(self.mr_off.into_iter().map(|version| Entry::ModrinthFile { version, enabled: false }));
        v.extend(self.remove_cf.into_iter().map(|p| Entry::Remove(Target::Curse(p))));
        v.extend(self.remove_named.into_iter().map(|n| Entry::Remove(Target::Named(n))));
        v.extend(self.off_cf.into_iter().map(|p| Entry::Off(Target::Curse(p))));
        v.extend(self.off_named.into_iter().map(|n| Entry::Off(Target::Named(n))));
        v
    }
}

/// Reads a code. Spaces and line breaks (from chat apps wrapping it) are
/// ignored, and the `HOP-` prefix is optional.
pub fn decode(text: &str) -> anyhow::Result<Share> {
    let text: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let body = text
        .strip_prefix(PREFIX)
        .or_else(|| text.strip_prefix(&PREFIX.to_lowercase()))
        .unwrap_or(&text);
    ensure!(!body.is_empty(), "Paste a share code");
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(body.trim_end_matches('='))
        .ok()
        .context("That isn't a share code (it has characters a code never has)")?;
    ensure!(raw.len() > 4, "That share code is cut short");
    let (b, check) = raw.split_at(raw.len() - 2);
    ensure!(
        sha1_smol::Sha1::from(b).digest().bytes()[..2] == *check,
        "That share code is damaged or incomplete; copy it again"
    );
    ensure!(
        b[0] & !DEFLATED == VERSION,
        "This share code is from a newer Hopper; update Hopper to use it"
    );
    let body = if b[0] & DEFLATED != 0 {
        inflate(&b[1..]).context("That share code is damaged")?
    } else {
        b[1..].to_vec()
    };
    let mut r = Reader { b: &body, at: 0 };
    let flags = r.byte()?;
    let base = match flags & 3 {
        PACK_NONE => Base::Plain { game_version: r.str()?, loader: r.str()? },
        PACK_CF => Base::Curse { project: r.num()?, file: r.num()? },
        PACK_MR => Base::Modrinth { project: r.str()?, version: r.str()? },
        _ => bail!("That share code is damaged"),
    };
    let name = (flags & HAS_NAME != 0).then(|| r.str()).transpose()?;
    let loader = (flags & HAS_LOADER != 0).then(|| r.str()).transpose()?;
    let mask = r.byte()?;
    let mut nums: [Vec<u64>; 4] = Default::default();
    let mut strs: [Vec<String>; 4] = Default::default();
    for (i, v) in nums.iter_mut().enumerate() {
        if mask & (1 << i) != 0 {
            *v = r.sorted()?;
        }
    }
    for (i, v) in strs.iter_mut().enumerate() {
        if mask & (1 << (4 + i)) != 0 {
            *v = r.strs()?;
        }
    }
    let [cf_on, cf_off, remove_cf, off_cf] = nums;
    let [mr_on, mr_off, remove_named, off_named] = strs;
    let g = Groups { cf_on, cf_off, mr_on, mr_off, remove_cf, remove_named, off_cf, off_named };
    ensure!(r.at == body.len(), "That share code is damaged");
    Ok(Share { base, name, loader, entries: g.entries() })
}

fn deflate(b: &[u8]) -> Vec<u8> {
    use std::io::Write as _;
    let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
    let _ = z.write_all(b);
    z.finish().unwrap_or_default()
}

fn inflate(b: &[u8]) -> anyhow::Result<Vec<u8>> {
    use std::io::Read as _;
    let mut out = Vec::new();
    // A real code is far smaller; a damaged one can't balloon.
    flate2::read::DeflateDecoder::new(b)
        .take(4 << 20)
        .read_to_end(&mut out)?;
    Ok(out)
}

fn put_num(b: &mut Vec<u8>, mut n: u64) {
    loop {
        let low = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            b.push(low);
            return;
        }
        b.push(low | 0x80);
    }
}

fn put_str(b: &mut Vec<u8>, s: &str) {
    put_num(b, s.len() as u64);
    b.extend_from_slice(s.as_bytes());
}

fn put_strs(b: &mut Vec<u8>, v: &[String]) {
    put_num(b, v.len() as u64);
    for s in v {
        put_str(b, s);
    }
}

/// Numbers sorted, each as the step from the one before: neighbouring
/// CurseForge ids are close, so most steps fit in two bytes.
fn put_sorted(b: &mut Vec<u8>, v: &[u64]) {
    let mut v = v.to_vec();
    v.sort_unstable();
    v.dedup();
    put_num(b, v.len() as u64);
    let mut last = 0;
    for n in v {
        put_num(b, n - last);
        last = n;
    }
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> anyhow::Result<u8> {
        let v = *self.b.get(self.at).context("That share code is cut short")?;
        self.at += 1;
        Ok(v)
    }

    fn num(&mut self) -> anyhow::Result<u64> {
        let mut n = 0u64;
        for shift in (0..64).step_by(7) {
            let v = self.byte()?;
            n |= ((v & 0x7f) as u64) << shift;
            if v & 0x80 == 0 {
                return Ok(n);
            }
        }
        bail!("That share code is damaged")
    }

    fn count(&mut self) -> anyhow::Result<usize> {
        let n = self.num()?;
        ensure!(n <= 20_000, "That share code is damaged");
        Ok(n as usize)
    }

    fn str(&mut self) -> anyhow::Result<String> {
        let len = self.num()? as usize;
        let end = self.at.checked_add(len).filter(|&e| e <= self.b.len());
        let end = end.context("That share code is cut short")?;
        let s = std::str::from_utf8(&self.b[self.at..end]).context("That share code is damaged")?;
        self.at = end;
        Ok(s.to_string())
    }

    fn strs(&mut self) -> anyhow::Result<Vec<String>> {
        (0..self.count()?).map(|_| self.str()).collect()
    }

    fn sorted(&mut self) -> anyhow::Result<Vec<u64>> {
        let mut last = 0u64;
        (0..self.count()?)
            .map(|_| {
                last = last.checked_add(self.num()?).context("That share code is damaged")?;
                Ok(last)
            })
            .collect()
    }
}

// ── making one from an instance ─────────────────────────────────────

/// A code for `inst`, and the names of files it can't carry (jars that
/// aren't on CurseForge or Modrinth).
pub struct Made {
    pub code: String,
    pub share: Share,
    pub left_out: Vec<String>,
}

/// What the pack itself puts in an instance.
struct PackFiles {
    /// CurseForge pack: project → file.
    curse: HashMap<u64, u64>,
    /// Modrinth pack: sha1 → file name.
    sha1: HashMap<String, String>,
    loader: String,
}

pub fn make(inst: &Instance, job: &Job) -> anyhow::Result<Made> {
    let (base, pack) = match inst.pack.as_ref().filter(|p| p.project_id != 0) {
        None => (
            Base::Plain {
                game_version: inst.game_version.clone(),
                loader: inst.loader.clone(),
            },
            None,
        ),
        Some(p) if is_mr(p.project_id) => {
            let (project, version) = modrinth_pack_ids(inst, job)?;
            let file = crate::source::file(num_id(&project), num_id(&version))?;
            let zip = crate::modpack::fetch_mrpack(&file, job)?;
            let index = crate::modpack::read_mr_index(&zip)?;
            let sha1 = index
                .files
                .iter()
                .filter(|f| !f.env.as_ref().and_then(|e| e.get("client")).is_some_and(|c| c == "unsupported"))
                .filter_map(|f| {
                    let h = f.hashes.get("sha1")?.to_ascii_lowercase();
                    Some((h, f.file_name().to_string()))
                })
                .collect();
            let pf = PackFiles { curse: HashMap::new(), sha1, loader: index.loader() };
            (Base::Modrinth { project, version }, Some(pf))
        }
        Some(p) => {
            ensure!(
                p.file_id != 0,
                "{} was imported from a zip, so there's no published pack version to point to",
                inst.name
            );
            let manifest = curse_manifest(inst, p.project_id, p.file_id, job)?;
            let pf = PackFiles {
                curse: manifest.files.iter().map(|f| (f.project_id, f.file_id)).collect(),
                sha1: HashMap::new(),
                loader: manifest.loader(),
            };
            (Base::Curse { project: p.project_id, file: p.file_id }, Some(pf))
        }
    };

    let mut entries = Vec::new();
    let mut left_out = Vec::new();
    // Pack files still there, so the rest count as removed.
    let mut kept_cf: HashSet<u64> = HashSet::new();
    let mut kept_sha1: HashSet<String> = HashSet::new();
    let pack_project = inst.pack.as_ref().map_or(0, |p| p.project_id);
    for a in &inst.addons {
        // CurseForge lists the pack's own zip among an instance's files.
        if a.class_id == cf::CLASS_MODPACKS || (pack_project != 0 && a.project_id == pack_project) {
            continue;
        }
        if let Some(pf) = &pack {
            let in_pack = if !is_mr(a.project_id) && a.project_id != 0 && pf.curse.get(&a.project_id) == Some(&a.file_id) {
                kept_cf.insert(a.project_id);
                Some(Target::Curse(a.project_id))
            } else if let Some(h) = addon_sha1(inst, a).filter(|h| pf.sha1.contains_key(h)) {
                kept_sha1.insert(h);
                Some(Target::Named(a.file_name.clone()))
            } else {
                None
            };
            if let Some(t) = in_pack {
                if !a.enabled {
                    entries.push(Entry::Off(t));
                }
                continue;
            }
        }
        match file_entry(a) {
            Some(e) => entries.push(e),
            None => left_out.push(if a.name.is_empty() { a.file_name.clone() } else { a.name.clone() }),
        }
    }
    let mut loader = None;
    if let Some(pf) = &pack {
        // A pack mod moved to another file is in the list already, and
        // replaces the pack's; only the ones with nothing there go.
        let present: HashSet<u64> = inst.addons.iter().map(|a| a.project_id).collect();
        let mut removed: Vec<u64> = pf
            .curse
            .keys()
            .filter(|p| !kept_cf.contains(p) && !present.contains(p))
            .copied()
            .collect();
        removed.sort_unstable();
        entries.extend(removed.into_iter().map(|p| Entry::Remove(Target::Curse(p))));
        let present_names: HashSet<String> =
            inst.addons.iter().map(|a| a.file_name.to_lowercase()).collect();
        let mut gone: Vec<&String> = pf
            .sha1
            .iter()
            .filter(|(h, name)| !kept_sha1.contains(*h) && !present_names.contains(&name.to_lowercase()))
            .map(|(_, name)| name)
            .collect();
        gone.sort();
        entries.extend(gone.into_iter().map(|n| Entry::Remove(Target::Named(n.clone()))));
        if !inst.loader.is_empty() && inst.loader != pf.loader {
            loader = Some(inst.loader.clone());
        }
    }
    let pack_name = inst.pack.as_ref().map(|p| p.name.as_str()).unwrap_or("");
    let name = (inst.name != pack_name && !inst.name.is_empty()).then(|| inst.name.clone());
    let share = Share { base, name, loader, entries };
    Ok(Made { code: encode(&share), share, left_out })
}

/// The file entry for an addon, or `None` when it's on neither site.
fn file_entry(a: &Addon) -> Option<Entry> {
    if is_mr(a.project_id) || (!a.mr_version.is_empty() && a.project_id == 0) {
        let version = non_empty(&a.mr_version).or_else(|| modrinth::real_id(a.file_id).ok())?;
        return Some(Entry::ModrinthFile { version, enabled: a.enabled });
    }
    (a.project_id != 0 && a.file_id != 0).then_some(Entry::CurseFile { file: a.file_id, enabled: a.enabled })
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

/// An addon's SHA-1: recorded, or read from the file.
fn addon_sha1(inst: &Instance, a: &Addon) -> Option<String> {
    if !a.mr_sha1.is_empty() {
        return Some(a.mr_sha1.to_ascii_lowercase());
    }
    let bytes = std::fs::read(inst.addon_path(a)).ok()?;
    Some(modrinth::sha1(&bytes))
}

/// The CurseForge pack's manifest: the copy beside the instance when it's
/// for this version, else the pack zip's.
fn curse_manifest(
    inst: &Instance,
    project: u64,
    file: u64,
    job: &Job,
) -> anyhow::Result<crate::instances::PackManifest> {
    let local = std::fs::read(inst.root.join("manifest.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<crate::instances::PackManifest>(strip_bom(&b)).ok())
        .filter(|m| !m.files.is_empty());
    // An instance updated outside Hopper may keep an older manifest; the
    // zip is cached, so checking against it is cheap after the first time.
    if let Some(m) = local
        && m.project_id == project
        && let Some(p) = inst.pack.as_ref()
        && (m.version == p.version || p.version.is_empty())
    {
        return Ok(m);
    }
    let f = cf::file(project, file)?;
    let zip = crate::modpack::fetch_pack_zip(&f, job)?;
    crate::modpack::read_manifest(&zip)
}

fn strip_bom(b: &[u8]) -> &[u8] {
    b.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(b)
}

/// The Modrinth project and version of a Modrinth pack instance. Packs
/// installed before Hopper kept them are looked up by name and version
/// number, and the instance remembers what was found.
fn modrinth_pack_ids(inst: &Instance, job: &Job) -> anyhow::Result<(String, String)> {
    let p = inst.pack.as_ref().context("no pack")?;
    if !p.mr_project.is_empty() && !p.mr_version.is_empty() {
        return Ok((p.mr_project.clone(), p.mr_version.clone()));
    }
    if let (Ok(proj), Ok(ver)) = (modrinth::real_id(p.project_id), modrinth::real_id(p.file_id)) {
        return Ok((proj, ver));
    }
    job.detail(format!("Looking {} up on Modrinth", p.name));
    let search = cf::Search {
        class_id: cf::CLASS_MODPACKS,
        text: p.name.clone(),
        ..Default::default()
    };
    let (hits, _) = modrinth::search(&search)?;
    for m in hits.iter().filter(|m| m.id == p.project_id || m.name.eq_ignore_ascii_case(&p.name)) {
        let Some(project) = m.modrinth.clone() else { continue };
        let versions = modrinth::versions(&project, "", cf::Loader::Any)?;
        let hit = versions.iter().find(|v| {
            v.id == p.mr_version || num_id(&v.id) == p.file_id || v.version_number == p.version
        });
        if let Some(v) = hit {
            let mut fixed = inst.clone();
            if let Some(fp) = fixed.pack.as_mut() {
                fp.mr_project = project.clone();
                fp.mr_version = v.id.clone();
            }
            let _ = fixed.save();
            return Ok((project, v.id.clone()));
        }
    }
    bail!(
        "Couldn't find {} {} on Modrinth to point the code at",
        p.name,
        p.version
    )
}

// ── setting one up ──────────────────────────────────────────────────

/// Installs what `code` describes as a new instance under `instances_root`.
pub fn install(instances_root: &Path, code: &str, job: &Job) -> anyhow::Result<Instance> {
    let share = decode(code)?;
    let mut inst = match &share.base {
        Base::Plain { game_version, loader } => {
            let name = share.name.clone().unwrap_or_else(|| format!("Shared {game_version}"));
            crate::modpack::create(instances_root, &name, game_version, loader)?
        }
        Base::Curse { project, file } => {
            job.detail("Looking up the pack on CurseForge");
            let m = cf::get_mod(*project)?;
            let f = cf::file(*project, *file)?;
            crate::modpack::install(instances_root, &m, &f, job)?
        }
        Base::Modrinth { project, version } => {
            job.detail("Looking up the pack on Modrinth");
            let (p, v) = (num_id(project), num_id(version));
            let m = crate::source::get_mod(p)?;
            let f = crate::source::file(p, v)?;
            crate::modpack::install_mrpack(instances_root, &m, &f, job)?
        }
    };
    job.check_cancelled()?;
    if let (Some(name), false) = (&share.name, matches!(share.base, Base::Plain { .. })) {
        inst.name = name.clone();
    }
    if let Some(l) = &share.loader {
        inst.loader = l.clone();
    }
    inst.save()?;
    apply(&mut inst, &share, job)?;
    Ok(inst)
}

/// Brings a freshly installed pack in line with the code's entries. A file
/// that fails is noted and the rest still go in.
fn apply(inst: &mut Instance, share: &Share, job: &Job) -> anyhow::Result<()> {
    let find = |inst: &Instance, t: &Target| {
        inst.addons.iter().position(|a| match t {
            Target::Curse(p) => a.project_id == *p,
            Target::Named(n) => a.file_name.eq_ignore_ascii_case(n),
        })
    };
    for e in &share.entries {
        if let Entry::Remove(t) = e
            && let Some(i) = find(inst, t)
        {
            let a = inst.addons.remove(i);
            let _ = std::fs::remove_file(inst.addon_path(&a));
        }
    }
    // Whose each CurseForge file is, a few hundred per request.
    let cf_ids: Vec<u64> = share
        .entries
        .iter()
        .filter_map(|e| match e {
            Entry::CurseFile { file, .. } => Some(*file),
            _ => None,
        })
        .collect();
    let mut owner: HashMap<u64, u64> = HashMap::new();
    for chunk in cf_ids.chunks(500) {
        job.detail("Looking up the mods on CurseForge");
        for f in cf::get_files(chunk)? {
            owner.insert(f.id, f.mod_id);
        }
    }
    let files: Vec<(u64, u64, bool)> = share
        .entries
        .iter()
        .filter_map(|e| match e {
            Entry::CurseFile { file, enabled } => match owner.get(file) {
                Some(&p) => Some((p, *file, *enabled)),
                None => {
                    job.note(format!("CurseForge has no file {file} any more"));
                    None
                }
            },
            Entry::ModrinthFile { version, enabled } => match modrinth::version(version) {
                Ok(v) => Some((num_id(&v.project_id), num_id(&v.id), *enabled)),
                Err(e) => {
                    job.note(format!("Modrinth version {version}: {e:#}"));
                    None
                }
            },
            _ => None,
        })
        .collect();
    for (i, (project, file, enabled)) in files.iter().enumerate() {
        job.check_cancelled()?;
        job.progress(i, files.len());
        match crate::install::install_file(inst, *project, *file, job) {
            Err(e) => job.note(format!("Couldn't add a file: {e:#}")),
            Ok(_) if !enabled => {
                if let Some(i) = inst.addons.iter().position(|a| a.project_id == *project)
                    && let Err(e) = crate::install::set_enabled(inst, i, false)
                {
                    job.note(format!("Couldn't turn {} off: {e:#}", inst.addons[i].name));
                }
            }
            Ok(_) => {}
        }
    }
    for e in &share.entries {
        if let Entry::Off(t) = e
            && let Some(i) = find(inst, t)
        {
            crate::install::set_enabled(inst, i, false)?;
        }
    }
    inst.save()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let s = Share {
            base: Base::Curse { project: 1_032_178, file: 6_731_254 },
            name: Some("Our server ✨".into()),
            loader: Some("neoforge-21.1.256".into()),
            entries: vec![
                Entry::CurseFile { file: 6_000_001, enabled: true },
                Entry::CurseFile { file: 5_000_001, enabled: true },
                Entry::CurseFile { file: 6_000_002, enabled: false },
                Entry::ModrinthFile { version: "u8pXa1bx".into(), enabled: false },
                Entry::Remove(Target::Curse(12)),
                Entry::Remove(Target::Named("foo-1.0.jar".into())),
                Entry::Off(Target::Curse(99)),
                Entry::Off(Target::Named("bar.jar".into())),
            ],
        };
        let code = encode(&s);
        assert!(code.starts_with(PREFIX));
        // The code keeps entries grouped and sorted.
        let mut s = s;
        s.entries.swap(0, 1);
        assert_eq!(decode(&code).unwrap(), s);
        // Wrapped by a chat app, or pasted without the prefix.
        let wrapped = format!("  {}\n{} ", &code[..20], &code[20..]);
        assert_eq!(decode(&wrapped).unwrap(), s);
        assert_eq!(decode(&code[PREFIX.len()..]).unwrap(), s);
    }

    #[test]
    fn a_plain_pack_is_short() {
        let s = Share {
            base: Base::Curse { project: 1_032_178, file: 6_731_254 },
            name: None,
            loader: None,
            entries: vec![],
        };
        let code = encode(&s);
        assert!(code.len() < 24, "{code}");
        assert_eq!(s.summary(), "the pack as published");
    }

    #[test]
    fn rejects_damage() {
        let s = Share {
            base: Base::Plain { game_version: "1.21.1".into(), loader: "fabric-0.16.9".into() },
            name: None,
            loader: None,
            entries: vec![Entry::CurseFile { file: 2, enabled: true }],
        };
        let code = encode(&s);
        let mut bad: Vec<char> = code.chars().collect();
        let i = bad.len() - 5;
        bad[i] = if bad[i] == 'A' { 'B' } else { 'A' };
        let bad: String = bad.into_iter().collect();
        assert!(decode(&bad).is_err());
        assert!(decode(&code[..code.len() - 3]).is_err());
        assert!(decode("hello there").is_err());
        assert!(decode("").is_err());
    }
}

#[cfg(test)]
mod real_tests {
    use super::*;

    /// `SHARE_FROM=<instance folder> SHARE_TO=<empty folder>`: makes a code
    /// for a real instance (read only) and, with SHARE_TO, installs it there
    /// and checks the mod list matches.
    #[test]
    #[ignore = "network: needs a real instance"]
    fn real_round_trip() {
        let from = std::env::var("SHARE_FROM").expect("SHARE_FROM");
        let jobs = crate::jobs::Jobs::default();
        let inst = crate::instances::load(Path::new(&from)).expect("instance");
        let made = make(&inst, &jobs.start("make")).unwrap();
        println!("{} chars: {}", made.code.len(), made.share.summary());
        println!("left out: {:?}", made.left_out);
        println!("{}", made.code);
        let Ok(to) = std::env::var("SHARE_TO") else { return };
        let job = jobs.start("install");
        let got = install(Path::new(&to), &made.code, &job).unwrap();
        for s in jobs.snapshot() {
            for n in s.notes {
                println!("note: {n}");
            }
        }
        let key = |i: &Instance| {
            let mut v: Vec<(String, bool)> = i
                .addons
                .iter()
                .map(|a| (a.file_name.to_lowercase(), a.enabled))
                .collect();
            v.sort();
            v
        };
        let (want, have) = (key(&inst), key(&got));
        // Website-only files wait in the browser-download list, as with
        // any pack install.
        let by_hand: HashSet<String> = crate::modpack::pending_downloads(&got)
            .into_iter()
            .map(|p| p.file_name.to_lowercase())
            .collect();
        let missing: Vec<_> = want
            .iter()
            .filter(|x| !have.contains(x) && !by_hand.contains(&x.0))
            .collect();
        let extra: Vec<_> = have.iter().filter(|x| !want.contains(x)).collect();
        println!("missing {missing:?}\nextra {extra:?}");
        assert_eq!(got.loader, inst.loader);
        assert!(missing.is_empty() && extra.is_empty());
    }

    /// A code without a pack: one CurseForge mod, one Modrinth mod turned
    /// off. `SHARE_TO=<empty folder>`.
    #[test]
    #[ignore = "network: downloads two mods"]
    fn plain_install() {
        let to = std::env::var("SHARE_TO").expect("SHARE_TO");
        let mc = "1.21.1";
        let jei = cf::files(238_222, mc, cf::Loader::Fabric).unwrap().remove(0);
        let sodium = modrinth::versions("AANobbMI", mc, cf::Loader::Fabric).unwrap().remove(0);
        let share = Share {
            base: Base::Plain { game_version: mc.into(), loader: "fabric-0.16.9".into() },
            name: Some("Share test".into()),
            loader: None,
            entries: vec![
                Entry::CurseFile { file: jei.id, enabled: true },
                Entry::ModrinthFile { version: sodium.id.clone(), enabled: false },
            ],
        };
        let code = encode(&share);
        println!("{code}");
        let jobs = crate::jobs::Jobs::default();
        let inst = install(Path::new(&to), &code, &jobs.start("install")).unwrap();
        for s in jobs.snapshot() {
            println!("notes: {:?}", s.notes);
        }
        assert_eq!(inst.name, "Share test");
        assert_eq!(inst.loader, "fabric-0.16.9");
        let j = inst.addons.iter().find(|a| a.project_id == 238_222).expect("JEI");
        assert!(j.enabled && inst.addon_path(j).exists());
        let s = inst.addons.iter().find(|a| a.mr_project == "AANobbMI").expect("Sodium");
        assert!(!s.enabled && inst.addon_path(s).exists(), "{}", inst.addon_path(s).display());
        // And back: the installed instance makes the same code.
        let again = make(&crate::instances::load(&inst.root).unwrap(), &jobs.start("make")).unwrap();
        assert_eq!(again.share, share);
    }
}
