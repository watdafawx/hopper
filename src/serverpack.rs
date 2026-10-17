//! Which pack mods belong on a server. Ported from mod-sync's `serverpack.go`.
//!
//! A CurseForge modpack usually ships a companion server pack that the author
//! curates: client-only mods are left out. That exclusion list is exactly what
//! Hopper should never push to a server. Server packs run to gigabytes, so we
//! range-read only the zip's central directory (the file list at its end).

use crate::cf;
use anyhow::{anyhow, bail};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ServerPackSet {
    /// Lower-case jar names under `mods/`.
    pub jars: HashSet<String>,
    /// Project ids, when the server pack carries a `manifest.json` instead.
    pub project_ids: HashSet<u64>,
}

impl ServerPackSet {
    fn stems(&self) -> HashSet<String> {
        self.jars.iter().map(|j| jar_stem(j)).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.jars.is_empty() && self.project_ids.is_empty()
    }
}

/// Answers "is this client jar in the server pack?" quickly.
pub struct Matcher {
    set: ServerPackSet,
    stems: HashSet<String>,
}

impl Matcher {
    pub fn new(set: ServerPackSet) -> Self {
        Self {
            stems: set.stems(),
            set,
        }
    }

    pub fn has(&self, file_name: &str, project_id: u64) -> bool {
        if !self.set.project_ids.is_empty() {
            return project_id != 0 && self.set.project_ids.contains(&project_id);
        }
        let f = file_name.to_lowercase();
        self.set.jars.contains(&f) || self.stems.contains(&jar_stem(&f))
    }
}

/// Loader- and version-free slug: "papi-neoforge-1.21.1-1.2.1.jar" and
/// "papi-1.21.1-neoforge-1.1.3.jar" both become "papi".
pub fn jar_stem(file_name: &str) -> String {
    let name = file_name.to_lowercase();
    let name = name.trim_end_matches(".jar");
    let parts: Vec<&str> = name.split('-').collect();
    let end = (1..parts.len())
        .find(|&i| {
            let p = parts[i];
            crate::scan::looks_like_version(p)
                || matches!(p, "neoforge" | "forge" | "fabric" | "quilt" | "neo")
                || p.starts_with("mc1.")
                || p.starts_with("mc1_")
        })
        .unwrap_or(parts.len());
    parts[..end].join("-")
}

/// "jei" from "https://www.curseforge.com/minecraft/mc-mods/jei".
fn cf_slug(page: &str) -> Option<&str> {
    let rest = page.split_once("curseforge.com/minecraft/")?.1;
    let slug = rest.split('/').nth(1)?.split(['?', '#']).next()?;
    (!slug.is_empty()).then_some(slug)
}

/// What [`annotate`] found out, for the UI.
#[derive(Clone, Debug, Default)]
pub struct Annotation {
    /// Pack mods marked won't-sync.
    pub flagged: usize,
    /// Mods in the server pack, when it could be read.
    pub server_pack_jars: Option<usize>,
    /// Why the server pack was not used, if it wasn't.
    pub note: Option<String>,
}

/// Fills CurseForge identity from the instance and marks the pack mods the
/// author keeps off servers. Authoritative signal: the pack's own server pack.
/// Fallback: the mod's "Client"-only tag on CurseForge. Mods the user added
/// are flagged only when their jar is silent and CurseForge's tags (or the
/// same mod on Modrinth) say client-only; `sync_anyway` holds project ids the
/// user overrode.
pub fn annotate(
    mods: &mut [crate::model::ModInfo],
    inst: &crate::instances::Instance,
    sync_anyway: &HashSet<u64>,
) -> Annotation {
    let mut out = Annotation::default();
    let matcher = match &inst.pack {
        Some(p) if p.server_pack_file_id != 0 => match get(p.project_id, p.server_pack_file_id) {
            Ok(set) => {
                out.server_pack_jars = Some(set.jars.len());
                Some(Matcher::new(set))
            }
            Err(e) => {
                out.note = Some(format!(
                    "Server pack not read ({e:#}); using CurseForge's client tags."
                ));
                None
            }
        },
        Some(_) => {
            out.note = Some("This pack has no server pack; using the mods' client-only tags.".into());
            None
        }
        None => None,
    };

    // Mods the user added that don't say which side they run on: ask
    // Modrinth (mod-sync scraped CurseForge's page; that is blocked now).
    let unknown: Vec<std::path::PathBuf> = mods
        .iter()
        .filter(|m| {
            m.side.is_empty()
                && !inst
                    .addon_by_file(&m.file_name)
                    .is_some_and(|a| a.from_pack)
        })
        .map(|m| inst.mods_dir().join(&m.file_name))
        .collect();
    // Sides from the authors' tags (Modrinth's, CurseForge's), sometimes
    // wrong, so for a tracked mod a client-only verdict is a won't-sync the
    // user can override rather than a hard side.
    let mut guessed: HashMap<String, (String, &str)> = HashMap::new();
    let sides = crate::modrinth::sides_of(&unknown);
    for m in mods.iter_mut() {
        if m.side.is_empty()
            && let Some(s) = sides.get(&m.file_name)
        {
            if inst.addon_by_file(&m.file_name).is_some() {
                guessed.insert(m.file_name.clone(), (s.clone(), "Client-only on Modrinth"));
            } else {
                m.side = s.clone();
            }
        }
    }

    // Still unknown: the Client/Server tags on the CurseForge file, then the
    // same mod on Modrinth by slug (its jar may only be on CurseForge).
    let unknown_added = |m: &crate::model::ModInfo| {
        inst.addon_by_file(&m.file_name)
            .filter(|a| !a.from_pack && m.side.is_empty() && !guessed.contains_key(&m.file_name))
    };
    let mut more: Vec<(String, (String, &str))> = Vec::new();
    let mut by_slug: Vec<(String, String)> = Vec::new();
    for m in mods.iter() {
        let Some(a) = unknown_added(m) else { continue };
        if !a.env.is_empty() {
            more.push((m.file_name.clone(), (a.env.clone(), "Tagged client-only on CurseForge")));
        } else if let Some(slug) = cf_slug(&a.page) {
            by_slug.push((slug.to_string(), a.name.clone()));
        }
    }
    if !by_slug.is_empty() {
        let sides = crate::modrinth::sides_by_slug(&by_slug);
        for m in mods.iter() {
            if let Some(a) = unknown_added(m)
                && a.env.is_empty()
                && let Some(s) = cf_slug(&a.page).and_then(|slug| sides.get(slug))
            {
                more.push((m.file_name.clone(), (s.clone(), "Client-only on Modrinth")));
            }
        }
    }
    guessed.extend(more);

    for m in mods.iter_mut() {
        let Some(a) = inst.addon_by_file(&m.file_name) else {
            continue;
        };
        m.project_id = a.project_id;
        m.icon = a.icon.clone();
        m.from_pack = a.from_pack;
        if !a.name.is_empty() {
            m.mod_name = a.name.clone();
        }
        if let Some((side, why)) = guessed.remove(&m.file_name) {
            // Overridden: no side, so nothing holds it back.
            let anyway = side == "client" && sync_anyway.contains(&a.project_id);
            if side == "client" && !anyway {
                m.wont_sync = Some(why.into());
            }
            if !anyway {
                m.side = side;
            }
        }
        if !a.from_pack || sync_anyway.contains(&a.project_id) {
            continue;
        }
        m.wont_sync = match &matcher {
            Some(mt) if !mt.has(&m.file_name, m.project_id) => {
                Some("The pack author left this out of the server pack".into())
            }
            None if a.env == "client" => Some(if a.mr_project.is_empty() {
                "Tagged client-only on CurseForge".into()
            } else {
                "Client-only on Modrinth".into()
            }),
            _ => None,
        };
        out.flagged += m.wont_sync.is_some() as usize;
    }
    out
}

// ── cache: the file id changes whenever the pack updates ────────────

static MEM: Mutex<Option<HashMap<u64, ServerPackSet>>> = Mutex::new(None);

fn cache_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "Hopper").map(|d| d.cache_dir().join("serverpacks.json"))
}

fn with_cache<T>(f: impl FnOnce(&mut HashMap<u64, ServerPackSet>) -> T) -> T {
    let mut g = MEM.lock().unwrap_or_else(|e| e.into_inner());
    let c = g.get_or_insert_with(|| {
        cache_path()
            .and_then(|p| fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    });
    f(c)
}

/// The server pack's contents, fetched once per file id and cached on disk.
pub fn get(project_id: u64, server_pack_file_id: u64) -> anyhow::Result<ServerPackSet> {
    if let Some(s) = with_cache(|c| c.get(&server_pack_file_id).cloned()) {
        return Ok(s);
    }
    let set = fetch(project_id, server_pack_file_id)?;
    with_cache(|c| {
        c.insert(server_pack_file_id, set.clone());
        if let (Some(p), Ok(bytes)) = (cache_path(), serde_json::to_vec(c)) {
            let _ = fs::create_dir_all(p.parent().unwrap_or(&p));
            let _ = fs::write(p, bytes);
        }
    });
    Ok(set)
}

fn fetch(project_id: u64, file_id: u64) -> anyhow::Result<ServerPackSet> {
    let file = cf::file(project_id, file_id)?;
    let url = cf::download_url(&file)?;
    let names = remote_zip_names(&url, file.file_length)?;

    let mut set = ServerPackSet::default();
    let mut manifest = None;
    for name in &names {
        let lower = name.to_lowercase();
        if lower == "manifest.json" || lower.ends_with("/manifest.json") {
            manifest = Some(name.clone());
            continue;
        }
        let parts: Vec<&str> = name.split('/').collect();
        // "mods/<x>.jar" or "<root>/mods/<x>.jar"
        if (2..=3).contains(&parts.len())
            && parts[parts.len() - 2].eq_ignore_ascii_case("mods")
            && lower.ends_with(".jar")
        {
            set.jars.insert(parts[parts.len() - 1].to_lowercase());
        }
    }
    if set.jars.is_empty() && manifest.is_some() {
        // A manifest-only server pack would need the file body; rare enough
        // that we report it and fall back to the env rule.
        anyhow::bail!("server pack lists its mods in manifest.json only");
    }
    if set.is_empty() {
        anyhow::bail!("server pack has no mods/ folder");
    }
    Ok(set)
}

/// Entry names of a remote zip, from its central directory.
pub fn remote_zip_names(url: &str, size: u64) -> anyhow::Result<Vec<String>> {
    names_from_ranges(size, |start, end| cf::fetch_range(url, start, end))
}

/// The central directory is read from the zip's last 4 MB, or fetched on
/// its own when it starts earlier. Larger than this is not a server pack.
const MAX_CD: u64 = 256 << 20;

/// [`remote_zip_names`] over `fetch(start, end_inclusive)`. Bad or hostile
/// data gives an error, never a panic.
fn names_from_ranges(
    size: u64,
    fetch: impl Fn(u64, u64) -> anyhow::Result<Vec<u8>>,
) -> anyhow::Result<Vec<String>> {
    const TAIL: u64 = 4 * 1024 * 1024;
    if size < EOCD_LEN as u64 {
        bail!("not a zip ({size} bytes)");
    }
    let tail_start = size.saturating_sub(TAIL);
    let tail = fetch(tail_start, size - 1)?;
    let (cd_offset, cd_size) = find_central_directory(&tail, tail_start)?;
    if cd_size == 0 {
        return Ok(Vec::new());
    }
    let cd_end = cd_offset
        .checked_add(cd_size)
        .filter(|&end| end <= size)
        .ok_or_else(|| anyhow!("the zip's file list runs past its end"))?;
    let cd = if cd_offset >= tail_start {
        let lo = usize::try_from(cd_offset - tail_start)?;
        let hi = lo
            .checked_add(usize::try_from(cd_size)?)
            .ok_or_else(|| anyhow!("bad zip file list"))?;
        tail.get(lo..hi)
            .ok_or_else(|| anyhow!("the zip's file list runs past the data read"))?
            .to_vec()
    } else {
        if cd_size > MAX_CD {
            bail!("the zip's file list is too large ({cd_size} bytes)");
        }
        fetch(cd_offset, cd_end - 1)?
    };
    parse_central_directory(&cd)
}

const EOCD_LEN: usize = 22;

fn le<const N: usize>(b: &[u8], i: usize) -> anyhow::Result<[u8; N]> {
    i.checked_add(N)
        .and_then(|end| b.get(i..end))
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| anyhow!("truncated zip record"))
}
fn u16_at(b: &[u8], i: usize) -> anyhow::Result<u64> {
    Ok(u16::from_le_bytes(le(b, i)?) as u64)
}
fn u32_at(b: &[u8], i: usize) -> anyhow::Result<u64> {
    Ok(u32::from_le_bytes(le(b, i)?) as u64)
}
fn u64_at(b: &[u8], i: usize) -> anyhow::Result<u64> {
    Ok(u64::from_le_bytes(le(b, i)?))
}
fn sig_at(b: &[u8], i: usize, sig: [u8; 4]) -> bool {
    le::<4>(b, i).is_ok_and(|s| s == sig)
}

/// (offset, size) of the central directory, handling ZIP64.
fn find_central_directory(tail: &[u8], tail_start: u64) -> anyhow::Result<(u64, u64)> {
    let last = tail
        .len()
        .checked_sub(EOCD_LEN)
        .ok_or_else(|| anyhow!("not a zip (too short)"))?;
    let eocd = (0..=last)
        .rev()
        .find(|&i| sig_at(tail, i, [0x50, 0x4b, 0x05, 0x06]))
        .ok_or_else(|| anyhow!("not a zip (no end record)"))?;
    let (mut size, mut offset) = (u32_at(tail, eocd + 12)?, u32_at(tail, eocd + 16)?);
    if (offset == 0xFFFF_FFFF || size == 0xFFFF_FFFF) && eocd >= 20 {
        // ZIP64 locator sits right before the EOCD and points at the ZIP64 EOCD.
        let loc = eocd - 20;
        if sig_at(tail, loc, [0x50, 0x4b, 0x06, 0x07]) {
            let z64 = u64_at(tail, loc + 8)?;
            let i = z64
                .checked_sub(tail_start)
                .and_then(|i| usize::try_from(i).ok())
                .ok_or_else(|| anyhow!("zip64 record outside tail"))?;
            if !sig_at(tail, i, [0x50, 0x4b, 0x06, 0x06]) {
                bail!("bad zip64 end record");
            }
            size = u64_at(tail, i.saturating_add(40))?;
            offset = u64_at(tail, i.saturating_add(48))?;
        }
    }
    Ok((offset, size))
}

fn parse_central_directory(cd: &[u8]) -> anyhow::Result<Vec<String>> {
    let mut names = Vec::new();
    let mut i = 0usize;
    while i.checked_add(46).is_some_and(|end| end <= cd.len())
        && sig_at(cd, i, [0x50, 0x4b, 0x01, 0x02])
    {
        let (n, x, c) = (
            u16_at(cd, i + 28)? as usize,
            u16_at(cd, i + 30)? as usize,
            u16_at(cd, i + 32)? as usize,
        );
        let start = i + 46;
        let name = cd
            .get(start..start + n)
            .ok_or_else(|| anyhow!("truncated central directory"))?;
        names.push(String::from_utf8_lossy(name).replace('\\', "/"));
        i = start + n + x + c;
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn slug_from_page() {
        assert_eq!(cf_slug("https://www.curseforge.com/minecraft/mc-mods/jei"), Some("jei"));
        assert_eq!(cf_slug("https://www.curseforge.com/minecraft/mc-mods/jei/files?x=1"), Some("jei"));
        assert_eq!(cf_slug("https://modrinth.com/mod/jei"), None);
        assert_eq!(cf_slug(""), None);
    }

    #[test]
    fn stems() {
        assert_eq!(jar_stem("papi-neoforge-1.21.1-1.2.1.jar"), "papi");
        assert_eq!(jar_stem("papi-1.21.1-neoforge-1.1.3.jar"), "papi");
        assert_eq!(
            jar_stem("ExtendedAE-1.21-2.2.36-neoforge.jar"),
            "extendedae"
        );
    }

    #[test]
    fn reads_central_directory_of_a_real_zip() {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default();
            for n in ["mods/a.jar", "config/b.toml", "mods/c-1.0.jar"] {
                z.start_file(n, o).unwrap();
                z.write_all(b"x").unwrap();
            }
            z.finish().unwrap();
        }
        let bytes = buf.into_inner();
        let (off, size) = find_central_directory(&bytes, 0).unwrap();
        let names = parse_central_directory(&bytes[off as usize..(off + size) as usize]).unwrap();
        assert_eq!(names, ["mods/a.jar", "config/b.toml", "mods/c-1.0.jar"]);
    }

    fn small_zip() -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default();
            for n in ["mods/a.jar", "mods/b.jar"] {
                z.start_file(n, o).unwrap();
                z.write_all(b"x").unwrap();
            }
            z.finish().unwrap();
        }
        buf.into_inner()
    }

    /// Serves ranges of `data` the way a CDN would (clamped at the end).
    fn ranges(data: &[u8]) -> impl Fn(u64, u64) -> anyhow::Result<Vec<u8>> + '_ {
        |start, end| {
            let s = (start as usize).min(data.len());
            let e = (end as usize + 1).min(data.len()).max(s);
            Ok(data[s..e].to_vec())
        }
    }

    #[test]
    fn remote_names_from_ranges() {
        let zip = small_zip();
        let names = names_from_ranges(zip.len() as u64, ranges(&zip)).unwrap();
        assert_eq!(names, ["mods/a.jar", "mods/b.jar"]);
    }

    #[test]
    fn bad_remote_data_is_an_error_not_a_panic() {
        // Size 0 used to underflow `size - 1`.
        assert!(names_from_ranges(0, |_, _| Ok(Vec::new())).is_err());
        // The server sends less than asked, or nothing.
        assert!(names_from_ranges(1000, |_, _| Ok(Vec::new())).is_err());
        assert!(names_from_ranges(1000, |_, _| Ok(vec![0x50, 0x4b, 0x05, 0x06])).is_err());

        let zip = small_zip();
        let eocd = zip.len() - 22;
        // Central directory offset and size pointing anywhere.
        for (off, size) in [
            (0xFFFF_FFF0u32, 10u32),
            (0, 0xFFFF_FFF0),
            (zip.len() as u32 - 5, 100),
        ] {
            let mut z = zip.clone();
            z[eocd + 12..eocd + 16].copy_from_slice(&size.to_le_bytes());
            z[eocd + 16..eocd + 20].copy_from_slice(&off.to_le_bytes());
            let _ = names_from_ranges(z.len() as u64, ranges(&z));
        }
        // ZIP64 markers with a locator that points past the data.
        let mut z = vec![0u8; 64];
        z.extend_from_slice(&[0x50, 0x4b, 0x06, 0x07, 0, 0, 0, 0]);
        z.extend_from_slice(&u64::MAX.to_le_bytes());
        z.extend_from_slice(&[0, 0, 0, 0]);
        z.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 0, 0, 0, 0]);
        z.extend_from_slice(&[0xFF; 8]);
        z.extend_from_slice(&[0, 0]);
        assert!(names_from_ranges(z.len() as u64, ranges(&z)).is_err());
        // Locator pointing inside the tail but at too few bytes.
        let n = z.len();
        let at = (n - 22 - 20 + 8) as usize;
        z[at..at + 8].copy_from_slice(&((n - 10) as u64).to_le_bytes());
        assert!(find_central_directory(&z, 0).is_err());

        // Truncated and garbage central directories.
        let mut cd = vec![0x50, 0x4b, 0x01, 0x02];
        cd.extend_from_slice(&[0xFF; 60]);
        assert!(parse_central_directory(&cd).is_err());
        assert!(parse_central_directory(&[0x50, 0x4b, 0x01]).unwrap().is_empty());
        assert!(find_central_directory(&[1, 2, 3], 0).is_err());
    }
}
