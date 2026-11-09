//! How a mod that CurseForge and Modrinth don't describe shows in lists:
//! its logo, authors, version and side, read from its own jar. A mod from
//! GitHub whose jar has no logo shows the repo owner's picture instead.
//!
//! Only read as a zip; nothing in the jar runs.

use crate::instances::Instance;
use std::collections::HashMap;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// What a jar says about how it looks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Looks {
    /// The logo, saved in the image cache (a local path); empty when the
    /// jar has none.
    pub icon: String,
    /// "A, B" from the metadata; empty when it lists none.
    pub authors: String,
    /// The mod's version, with `${file.jarVersion}` filled from the
    /// manifest; empty when the jar doesn't say.
    pub version: String,
    /// "client" | "server" | "both", or empty when the jar doesn't say.
    pub side: String,
}

/// What [`read_from`] finds before the logo is saved.
#[derive(Clone, Debug, Default)]
pub struct JarLooks {
    /// The logo's path inside the jar and its bytes.
    pub logo: Option<(String, Vec<u8>)>,
    pub authors: Vec<String>,
    pub version: String,
    pub side: String,
}

/// The biggest logo kept: lists show it at 40px, the peek at 64.
const LOGO_MAX: u32 = 128;
/// A logo file bigger than this isn't read (a jar can name anything).
const LOGO_BYTES_MAX: u64 = 4 << 20;

/// Reads a jar's looks.
pub fn read_from<R: Read + Seek>(r: R) -> anyhow::Result<JarLooks> {
    let mut zip = zip::ZipArchive::new(r)?;
    let mut info = crate::model::ModInfo::default();
    crate::scan::read_zip_metadata(&mut zip, &mut info);
    let mut out = JarLooks { version: info.version, side: info.side, ..Default::default() };
    let text = |zip: &mut zip::ZipArchive<R>, name: &str| -> Option<String> {
        let mut f = zip.by_name(name).ok()?;
        let mut s = String::new();
        f.by_ref().take(512 * 1024).read_to_string(&mut s).ok()?;
        Some(s)
    };
    let mut logo_at = None;
    for toml_name in ["META-INF/neoforge.mods.toml", "META-INF/mods.toml"] {
        if let Some(t) = text(&mut zip, toml_name)
            && let Ok(v) = t.parse::<toml::Table>()
        {
            let first = v.get("mods").and_then(|m| m.as_array()).and_then(|a| a.first());
            // A mod's own logo wins over the file's.
            logo_at = first
                .and_then(|m| m.get("logoFile"))
                .or_else(|| v.get("logoFile"))
                .and_then(|x| x.as_str())
                .map(str::to_string);
            if let Some(a) = first.and_then(|m| m.get("authors")).or_else(|| v.get("authors")).and_then(|x| x.as_str()) {
                out.authors = split_authors(a);
            }
            break;
        }
    }
    if logo_at.is_none() && out.authors.is_empty() {
        for json_name in ["fabric.mod.json", "quilt.mod.json"] {
            if let Some(t) = text(&mut zip, json_name)
                && let Ok(v) = serde_json::from_str::<serde_json::Value>(&t)
            {
                // quilt.mod.json keeps these under "quilt_loader.metadata".
                let meta = v
                    .get("quilt_loader")
                    .and_then(|q| q.get("metadata"))
                    .cloned()
                    .unwrap_or(v.clone());
                logo_at = meta.get("icon").and_then(fabric_icon);
                out.authors = fabric_authors(&meta);
                // Quilt: "environment" sits in "minecraft".
                if out.side.is_empty()
                    && let Some(env) = v.pointer("/minecraft/environment").and_then(|e| e.as_str())
                {
                    out.side = side_word(env);
                }
                break;
            }
        }
    }
    if let Some(at) = logo_at.filter(|p| !p.trim().is_empty()) {
        let at = at.trim().trim_start_matches(['/', '\\']).replace('\\', "/");
        if let Ok(f) = zip.by_name(&at)
            && f.size() <= LOGO_BYTES_MAX
        {
            let mut bytes = Vec::new();
            if f.take(LOGO_BYTES_MAX).read_to_end(&mut bytes).is_ok() && !bytes.is_empty() {
                out.logo = Some((at, bytes));
            }
        }
    }
    Ok(out)
}

fn side_word(env: &str) -> String {
    match env.trim().to_ascii_lowercase().as_str() {
        "client" => "client".into(),
        "server" | "dedicated_server" => "server".into(),
        "*" | "both" => "both".into(),
        _ => String::new(),
    }
}

/// "A, B and C" / "A & B": one name each.
fn split_authors(s: &str) -> Vec<String> {
    s.replace(" and ", ",")
        .replace('&', ",")
        .split([',', ';'])
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(str::to_string)
        .collect()
}

/// Fabric's `icon`: a path, or sizes to paths. The largest at most
/// [`LOGO_MAX`], else the smallest.
pub fn fabric_icon(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Object(m) => {
            let mut sizes: Vec<(u32, &str)> =
                m.iter().filter_map(|(k, p)| Some((k.parse().ok()?, p.as_str()?))).collect();
            sizes.sort_by_key(|(n, _)| *n);
            sizes
                .iter()
                .rev()
                .find(|(n, _)| *n <= LOGO_MAX)
                .or(sizes.first())
                .map(|(_, p)| p.to_string())
        }
        _ => None,
    }
}

/// Fabric's `authors` (names or `{ "name": … }`); Quilt's `contributors`
/// (name → role).
fn fabric_authors(meta: &serde_json::Value) -> Vec<String> {
    if let Some(a) = meta.get("authors").and_then(|a| a.as_array()) {
        return a
            .iter()
            .filter_map(|x| x.as_str().or_else(|| x.get("name")?.as_str()))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    meta.get("contributors")
        .and_then(|c| c.as_object())
        .map(|c| c.keys().cloned().collect())
        .unwrap_or_default()
}

fn logo_dir() -> Option<PathBuf> {
    crate::paths::dirs().map(|d| d.cache_dir().join("images").join("jar"))
}

/// A logo saved by [`read`]: a missing one is read again.
pub fn is_jar_logo(icon: &str) -> bool {
    logo_dir().is_some_and(|d| Path::new(icon).starts_with(d))
}

/// Saves a logo to the image cache as a PNG no bigger than [`LOGO_MAX`],
/// named after its contents (two jars with one logo share the file).
pub fn save_logo(bytes: &[u8]) -> anyhow::Result<PathBuf> {
    let dir = logo_dir().ok_or_else(|| anyhow::anyhow!("no cache folder"))?;
    let out = dir.join(format!("{}.png", sha1_smol::Sha1::from(bytes).digest()));
    if out.is_file() {
        return Ok(out);
    }
    let img = image::load_from_memory(bytes)?;
    let img = if img.width() > LOGO_MAX || img.height() > LOGO_MAX { img.thumbnail(LOGO_MAX, LOGO_MAX) } else { img };
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    std::fs::create_dir_all(&dir)?;
    let tmp = out.with_extension("tmp");
    std::fs::write(&tmp, &png)?;
    std::fs::rename(&tmp, &out)?;
    Ok(out)
}

/// Looks read this run, by jar path, with the size and time they were
/// read at.
static SEEN: Mutex<Option<HashMap<PathBuf, (u64, u64, Looks)>>> = Mutex::new(None);

fn stamp(path: &Path) -> Option<(u64, u64)> {
    let m = std::fs::metadata(path).ok()?;
    let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some((m.len(), t))
}

/// The jar's looks, read once per file version.
pub fn read(path: &Path) -> Looks {
    let Some((len, t)) = stamp(path) else { return Looks::default() };
    if let Some((l, tt, looks)) = SEEN.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(path))
        && *l == len
        && *tt == t
        && (looks.icon.is_empty() || looks.icon.starts_with("http") || Path::new(&looks.icon).is_file())
    {
        return looks.clone();
    }
    let jar = std::fs::File::open(path).map_err(anyhow::Error::from).and_then(read_from).unwrap_or_default();
    let icon = jar
        .logo
        .and_then(|(at, b)| {
            save_logo(&b)
                .map_err(|e| log::debug!("{}: logo {at}: {e:#}", path.display()))
                .ok()
        })
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let looks = Looks { icon, authors: jar.authors.join(", "), version: jar.version, side: jar.side };
    SEEN.lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(path.to_path_buf(), (len, t, looks.clone()));
    looks
}

/// The looks [`read`] found for `path` this run, without opening it.
pub fn known(path: &Path) -> Option<Looks> {
    SEEN.lock().unwrap_or_else(|e| e.into_inner()).as_ref()?.get(path).map(|(_, _, l)| l.clone())
}

/// The GitHub owner's picture for a mod from a repo ("owner/repo").
pub fn avatar(repo: &str) -> Option<String> {
    let owner = repo.split('/').next().filter(|o| !o.is_empty())?;
    Some(format!("https://github.com/{owner}.png?size=128"))
}

/// The jar of `file_name` in the instance's mods folder (on or off).
pub fn jar_of(inst: &Instance, file_name: &str) -> Option<PathBuf> {
    let mods = inst.mods_dir();
    [mods.join(file_name), mods.join(format!("{file_name}.disabled"))].into_iter().find(|p| p.is_file())
}

/// The version to show for a jar whose file name doesn't say one: its
/// metadata's, else the release it came from. Empty when neither does.
pub fn version_for(inst: &Instance, file_name: &str) -> String {
    let jar = jar_of(inst, file_name).and_then(|p| known(&p)).map(|l| l.version).unwrap_or_default();
    if !jar.is_empty() {
        return jar;
    }
    inst.addon_by_file(file_name)
        .and_then(|a| a.source.as_ref())
        .map(|s| super::tag_version(&s.seen))
        .unwrap_or_default()
}

/// Fills in what the lists show for mods no site describes (no icon, or
/// from a source): the jar's logo (else the GitHub owner's picture), its
/// authors and side. In memory only; reads each jar once per run.
pub fn fill(inst: &mut Instance) {
    let mods = inst.mods_dir();
    for a in inst.addons.iter_mut().filter(|a| a.folder() == "mods") {
        let ours = a.project_id == 0 || a.source.is_some();
        // A logo that came from a jar (or the owner's picture standing in for
        // one) is re-read from the jar there now: an update may bring a new one.
        let needs_icon = a.icon.is_empty() || is_jar_logo(&a.icon) || (ours && a.icon.starts_with("https://github.com/") && a.icon.ends_with(".png?size=128"));
        if !ours && !needs_icon {
            continue;
        }
        let Some(jar) = [mods.join(&a.file_name), mods.join(format!("{}.disabled", a.file_name))]
            .into_iter()
            .find(|p| p.is_file())
        else {
            continue;
        };
        let looks = read(&jar);
        if needs_icon {
            a.icon = if !looks.icon.is_empty() {
                looks.icon.clone()
            } else {
                a.source
                    .as_ref()
                    .filter(|s| s.kind == super::Kind::GitHub)
                    .and_then(|s| avatar(&s.url))
                    .unwrap_or_default()
            };
        }
        if ours {
            // The repo stays on the source; the people who made it show. The
            // jar is read again every run, so a rebuilt file whose authors
            // changed (a rename) replaces the name saved when it was installed.
            if !looks.authors.is_empty() {
                a.author = looks.authors.clone();
            }
            if a.env.is_empty() {
                a.env = looks.side.clone();
            }
        }
        if a.source.is_some() {
            super::remember_looks(a.project_id, &a.icon, &a.author);
        }
    }
}

/// Looks for a jar no list knows (a loose file): its logo, else the
/// picture of the GitHub owner its own links point to.
pub fn loose(path: &Path) -> Looks {
    let mut looks = read(path);
    if looks.icon.is_empty()
        && let Ok(info) = super::jar::read(path)
        && let Some(repo) = info.links.iter().find_map(|l| super::jar::github_repo(l))
    {
        looks.icon = avatar(&repo).unwrap_or_default();
        // Kept, so [`known`] and the next read have it.
        if let Some(e) = SEEN.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|m| m.get_mut(path)) {
            e.2.icon = looks.icon.clone();
        }
    }
    looks
}
