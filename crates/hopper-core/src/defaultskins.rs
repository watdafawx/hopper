//! Minecraft's default skins (Steve, Alex and the seven added in 1.19.3, each
//! with classic and slim arms) and the default elytra texture. Nothing is
//! bundled: they're read out of Mojang's current client jar with HTTP range
//! requests, so only the zip index and the few small PNGs are downloaded,
//! then kept in Hopper's cache.

use anyhow::Context as _;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const MANIFEST: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

/// In the game's order of its skin picker.
pub const NAMES: [&str; 9] = [
    "steve", "alex", "ari", "efe", "kai", "makena", "noor", "sunny", "zuri",
];

#[derive(Clone, Debug, PartialEq)]
pub struct DefaultSkin {
    /// File name without `.png`, e.g. "steve".
    pub name: &'static str,
    pub slim: bool,
    pub path: PathBuf,
}

impl DefaultSkin {
    /// "Steve", "Makena".
    pub fn label(&self) -> String {
        let mut c = self.name.chars();
        c.next()
            .map(|f| f.to_uppercase().chain(c).collect())
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Defaults {
    /// Classic then slim, each in [`NAMES`] order.
    pub skins: Vec<DefaultSkin>,
    pub elytra: Option<PathBuf>,
}

fn cache() -> Option<PathBuf> {
    crate::paths::dirs().map(|d| d.cache_dir().join("default-skins"))
}

fn file(dir: &Path, name: &str, slim: bool) -> PathBuf {
    dir.join(if slim { "slim" } else { "wide" }).join(format!("{name}.png"))
}

fn list(dir: &Path) -> Defaults {
    let skins = [false, true]
        .into_iter()
        .flat_map(|slim| {
            NAMES.into_iter().map(move |name| DefaultSkin {
                name,
                slim,
                path: file(dir, name, slim),
            })
        })
        .collect();
    let elytra = Some(dir.join("elytra.png")).filter(|p| p.is_file());
    Defaults { skins, elytra }
}

/// The default skins when they're already on disk.
pub fn cached() -> Option<Defaults> {
    let dir = cache()?;
    let d = list(&dir);
    d.skins.iter().all(|s| s.path.is_file()).then_some(d)
}

/// The default skins, downloading them on first use (blocking).
pub fn fetch() -> anyhow::Result<Defaults> {
    if let Some(d) = cached() {
        return Ok(d);
    }
    let dir = cache().context("no cache folder")?;
    let manifest = get_json(MANIFEST)?;
    let latest = manifest["latest"]["release"].as_str().context("no latest release")?;
    let url = manifest["versions"]
        .as_array()
        .and_then(|vs| vs.iter().find(|v| v["id"] == latest))
        .and_then(|v| v["url"].as_str())
        .context("the latest release is not in Mojang's version list")?;
    let version = get_json(url)?;
    let client = &version["downloads"]["client"];
    let jar_url = client["url"].as_str().context("no client download")?;
    let size = client["size"].as_u64().context("no client size")?;
    extract(RangeReader::new(jar_url, size), &dir)?;
    Ok(list(&dir))
}

/// Copies the skin and elytra PNGs out of a client jar into `dir`.
pub fn extract<R: Read + Seek>(jar: R, dir: &Path) -> anyhow::Result<()> {
    let mut zip = zip::ZipArchive::new(jar).context("reading the game jar")?;
    let names: Vec<String> = zip.file_names().map(str::to_string).collect();
    let mut found = 0;
    for slim in [false, true] {
        let sub = if slim { "slim" } else { "wide" };
        for name in NAMES {
            let entry = format!("assets/minecraft/textures/entity/player/{sub}/{name}.png");
            if !names.contains(&entry) {
                continue;
            }
            let mut bytes = Vec::new();
            zip.by_name(&entry)?.read_to_end(&mut bytes)?;
            write(&file(dir, name, slim), &bytes)?;
            found += 1;
        }
    }
    anyhow::ensure!(found == NAMES.len() * 2, "the game jar has {found} of the 18 default skins");
    // The elytra moved in 1.21.2; take whichever this jar has.
    let elytra = names.iter().find(|n| {
        n.starts_with("assets/minecraft/textures/entity/") && n.ends_with("/elytra.png")
    });
    if let Some(e) = elytra.cloned() {
        let mut bytes = Vec::new();
        zip.by_name(&e)?.read_to_end(&mut bytes)?;
        write(&dir.join("elytra.png"), &bytes)?;
    }
    Ok(())
}

fn write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension("png.part");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn get_json(url: &str) -> anyhow::Result<Value> {
    Ok(crate::cf::agent()
        .get(url)
        .call()?
        .body_mut()
        .with_config()
        .limit(16 << 20)
        .read_json()?)
}

/// A remote file read in blocks with HTTP range requests.
struct RangeReader {
    url: String,
    len: u64,
    pos: u64,
    blocks: HashMap<u64, Vec<u8>>,
}

const BLOCK: u64 = 512 * 1024;

impl RangeReader {
    fn new(url: &str, len: u64) -> Self {
        Self {
            url: url.to_string(),
            len,
            pos: 0,
            blocks: HashMap::new(),
        }
    }

    fn block(&mut self, ix: u64) -> std::io::Result<&[u8]> {
        if !self.blocks.contains_key(&ix) {
            let start = ix * BLOCK;
            let end = (start + BLOCK).min(self.len) - 1;
            let bytes = crate::cf::agent()
                .get(&self.url)
                .header("Range", format!("bytes={start}-{end}"))
                .call()
                .map_err(std::io::Error::other)?
                .body_mut()
                .with_config()
                .limit(BLOCK + 1)
                .read_to_vec()
                .map_err(std::io::Error::other)?;
            if bytes.len() as u64 != end - start + 1 {
                return Err(std::io::Error::other("the server ignored the byte range"));
            }
            self.blocks.insert(ix, bytes);
        }
        Ok(&self.blocks[&ix])
    }
}

impl Read for RangeReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let (ix, off) = (self.pos / BLOCK, (self.pos % BLOCK) as usize);
        let block = self.block(ix)?;
        let n = buf.len().min(block.len() - off);
        buf[..n].copy_from_slice(&block[off..off + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for RangeReader {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let p = match to {
            SeekFrom::Start(p) => p as i64,
            SeekFrom::End(d) => self.len as i64 + d,
            SeekFrom::Current(d) => self.pos as i64 + d,
        };
        if p < 0 {
            return Err(std::io::Error::other("seek before the start"));
        }
        self.pos = p as u64;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    #[test]
    fn extracts_skins_and_elytra_from_a_jar() {
        let mut jar = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        for sub in ["wide", "slim"] {
            for n in NAMES {
                jar.start_file(format!("assets/minecraft/textures/entity/player/{sub}/{n}.png"), opts)
                    .unwrap();
                jar.write_all(format!("{sub}/{n}").as_bytes()).unwrap();
            }
        }
        jar.start_file("assets/minecraft/textures/entity/equipment/wings/elytra.png", opts)
            .unwrap();
        jar.write_all(b"wings").unwrap();
        let bytes = jar.finish().unwrap().into_inner();
        let dir = std::env::temp_dir().join(format!("hopper-defskins-{}", std::process::id()));
        extract(Cursor::new(bytes), &dir).unwrap();
        let d = list(&dir);
        assert_eq!(d.skins.len(), 18);
        assert_eq!(std::fs::read(&d.skins[10].path).unwrap(), b"slim/alex");
        assert_eq!(d.skins[10].label(), "Alex");
        assert!(d.skins[10].slim);
        assert_eq!(std::fs::read(d.elytra.unwrap()).unwrap(), b"wings");
        let _ = std::fs::remove_dir_all(dir);
    }
}
