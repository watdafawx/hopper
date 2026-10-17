//! Modrinth API client (https://docs.modrinth.com/api/). No key needed.
//!
//! Projects and versions are converted into the CurseForge types the rest of
//! Hopper already uses: a project becomes a `cf::Mod` with `modrinth` set,
//! a version a `cf::File` with `modrinth` set.

use crate::cf::{self, Loader};
use anyhow::Context;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::collections::HashMap;

const BASE: &str = "https://api.modrinth.com/v2";

/// Modrinth ids are strings; Hopper keys projects and files by `u64`.
/// A Modrinth id maps to a number with the top bit set (CurseForge ids fit
/// in 32 bits), and this registry maps it back.
static IDS: std::sync::Mutex<Option<HashMap<u64, String>>> = std::sync::Mutex::new(None);

/// Stable numeric id for a Modrinth id (FNV-1a, top bit set).
pub fn num_id(id: &str) -> u64 {
    if id.is_empty() {
        return 0;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in id.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    let n = h | (1 << 63);
    IDS.lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(n, id.to_string());
    n
}

/// True for a number made by [`num_id`].
pub fn is_mr(id: u64) -> bool {
    id >> 63 == 1
}

/// The Modrinth id behind a number from [`num_id`].
pub fn real_id(n: u64) -> anyhow::Result<String> {
    IDS.lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(&n).cloned())
        .context("unknown Modrinth id")
}

/// Modrinth identity of a file.
#[derive(Clone, Debug, Default)]
pub struct MrFile {
    pub project_id: String,
    pub version_id: String,
    pub sha1: String,
    /// Required projects.
    pub requires: Vec<String>,
    /// Markdown.
    pub changelog: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Project {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub body: String,
    pub icon_url: Option<String>,
    pub downloads: f64,
    pub project_type: String,
    pub client_side: String,
    pub server_side: String,
    pub categories: Vec<String>,
    pub updated: String,
    pub published: String,
    pub team: String,
    pub gallery: Vec<Gallery>,
    pub issues_url: Option<String>,
    pub source_url: Option<String>,
    pub wiki_url: Option<String>,
    pub discord_url: Option<String>,
    pub license: Option<License>,
    pub loaders: Vec<String>,
    pub game_versions: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct License {
    pub id: String,
    pub name: String,
}

/// What a search hit or project knows beyond the basics.
#[derive(Clone, Debug, Default)]
struct Extra {
    links: cf::Links,
    license: Option<String>,
    side: Option<&'static str>,
    loaders: Vec<String>,
    versions: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Gallery {
    pub url: String,
    pub title: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Version {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub version_number: String,
    pub version_type: String,
    pub date_published: String,
    pub changelog: Option<String>,
    pub loaders: Vec<String>,
    pub game_versions: Vec<String>,
    pub files: Vec<VersionFile>,
    pub dependencies: Vec<VersionDependency>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct VersionFile {
    pub url: String,
    pub filename: String,
    pub primary: bool,
    pub size: u64,
    pub hashes: HashMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct VersionDependency {
    pub project_id: Option<String>,
    pub dependency_type: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct SearchHit {
    project_id: String,
    slug: String,
    title: String,
    description: String,
    icon_url: Option<String>,
    downloads: f64,
    author: String,
    project_type: String,
    categories: Vec<String>,
    date_modified: String,
    date_created: String,
    gallery: Vec<String>,
    client_side: String,
    server_side: String,
    license: String,
    versions: Vec<String>,
    latest_version: String,
}

#[derive(Deserialize)]
struct SearchResult {
    hits: Vec<SearchHit>,
    total_hits: u32,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Member {
    user: User,
    role: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct User {
    username: String,
}

/// Every project a user is a member of, most downloaded first.
pub fn user_projects(username: &str) -> anyhow::Result<Vec<cf::Mod>> {
    let mut ps: Vec<Project> = get(&format!("/user/{username}/projects"), &[])?;
    ps.sort_by(|a, b| b.downloads.total_cmp(&a.downloads));
    Ok(ps.iter().map(|p| project_to_mod(p, username)).collect())
}

fn get<T: DeserializeOwned>(path: &str, query: &[(&str, String)]) -> anyhow::Result<T> {
    let mut req = cf::agent().get(format!("{BASE}{path}"));
    for (k, v) in query {
        req = req.query(*k, v);
    }
    let mut resp = req.call().with_context(|| format!("Modrinth GET {path}"))?;
    resp.body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_json()
        .with_context(|| format!("reading {path}"))
}

fn post<T: DeserializeOwned>(path: &str, body: &serde_json::Value) -> anyhow::Result<T> {
    let mut resp = cf::agent()
        .post(format!("{BASE}{path}"))
        .send_json(body)
        .with_context(|| format!("Modrinth POST {path}"))?;
    resp.body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_json()
        .with_context(|| format!("reading {path}"))
}

/// Modrinth's loader name.
pub fn loader_name(l: Loader) -> Option<&'static str> {
    match l {
        Loader::Any => None,
        Loader::Forge => Some("forge"),
        Loader::Fabric => Some("fabric"),
        Loader::Quilt => Some("quilt"),
        Loader::NeoForge => Some("neoforge"),
    }
}

fn class_for(project_type: &str) -> u32 {
    match project_type {
        "modpack" => cf::CLASS_MODPACKS,
        "resourcepack" => cf::CLASS_RESOURCE_PACKS,
        "shader" => cf::CLASS_SHADERS,
        "datapack" => cf::CLASS_DATAPACKS,
        _ => cf::CLASS_MODS,
    }
}

fn project_type_for(class_id: u32) -> &'static str {
    match class_id {
        cf::CLASS_MODPACKS => "modpack",
        cf::CLASS_RESOURCE_PACKS => "resourcepack",
        cf::CLASS_SHADERS => "shader",
        cf::CLASS_DATAPACKS => "datapack",
        _ => "mod",
    }
}

/// "client" | "server" | "both" from Modrinth's side support.
pub fn env(client_side: &str, server_side: &str) -> &'static str {
    match (client_side, server_side) {
        ("unsupported", _) => "server",
        (_, "unsupported") => "client",
        ("required" | "optional", "required" | "optional") => "both",
        _ => "",
    }
}

fn page(project_type: &str, slug: &str) -> String {
    format!(
        "https://modrinth.com/{}/{slug}",
        if project_type.is_empty() {
            "mod"
        } else {
            project_type
        }
    )
}

// Search hits and full projects carry the same fields under different names.
#[allow(clippy::too_many_arguments)]
fn to_mod(
    id: String,
    slug: String,
    title: String,
    summary: String,
    icon: Option<String>,
    downloads: f64,
    author: String,
    project_type: &str,
    categories: &[String],
    updated: String,
    created: String,
    gallery: Vec<(String, String)>,
    extra: Extra,
) -> cf::Mod {
    // Loaders ride along in a hit's categories.
    let is_loader = |c: &str| cf::Loader::from_loader_id(c) != cf::Loader::Any;
    let mut loaders = extra.loaders;
    if loaders.is_empty() {
        loaders = categories.iter().filter(|c| is_loader(c)).cloned().collect();
    }
    cf::Mod {
        name: title,
        summary,
        download_count: downloads,
        class_id: Some(class_for(project_type)),
        logo: icon.map(|u| cf::Logo {
            thumbnail_url: u.clone(),
            url: u,
        }),
        authors: vec![cf::Author {
            url: format!("https://modrinth.com/user/{author}"),
            name: author,
            ..Default::default()
        }],
        links: cf::Links {
            website_url: page(project_type, &slug),
            ..extra.links
        },
        categories: categories
            .iter()
            .filter(|c| !is_loader(c))
            .map(|c| cf::Category {
                name: title_case(c),
                ..Default::default()
            })
            .collect(),
        license_type: extra.license.filter(|l| !l.is_empty()),
        side: extra.side,
        mr_loaders: loaders,
        mr_versions: extra.versions,
        date_modified: updated,
        date_created: created,
        allow_mod_distribution: Some(true),
        screenshots: gallery
            .into_iter()
            .map(|(url, title)| cf::Screenshot {
                thumbnail_url: url.clone(),
                url,
                title,
            })
            .collect(),
        slug,
        id: num_id(&id),
        modrinth: Some(id),
        ..Default::default()
    }
}

pub fn project_to_mod(p: &Project, author: &str) -> cf::Mod {
    to_mod(
        p.id.clone(),
        p.slug.clone(),
        p.title.clone(),
        p.description.clone(),
        p.icon_url.clone(),
        p.downloads,
        author.to_string(),
        &p.project_type,
        &p.categories,
        p.updated.clone(),
        p.published.clone(),
        p.gallery
            .iter()
            .map(|g| (g.url.clone(), g.title.clone().unwrap_or_default()))
            .collect(),
        Extra {
            links: cf::Links {
                issues_url: p.issues_url.clone(),
                source_url: p.source_url.clone(),
                wiki_url: p.wiki_url.clone(),
                discord_url: p.discord_url.clone(),
                ..Default::default()
            },
            license: p.license.as_ref().map(|l| {
                if l.name.is_empty() {
                    l.id.clone()
                } else {
                    l.name.clone()
                }
            }),
            side: Some(env(&p.client_side, &p.server_side)).filter(|s| !s.is_empty()),
            loaders: p.loaders.clone(),
            versions: p.game_versions.clone(),
        },
    )
}

/// A version as a `cf::File`.
pub fn version_to_file(v: &Version) -> cf::File {
    let f = v
        .files
        .iter()
        .find(|f| f.primary)
        .or(v.files.first())
        .cloned()
        .unwrap_or_default();
    let mut tags: Vec<String> = v.game_versions.clone();
    tags.extend(v.loaders.iter().map(|l| match l.as_str() {
        "neoforge" => "NeoForge".to_string(),
        "forge" => "Forge".to_string(),
        "fabric" => "Fabric".to_string(),
        "quilt" => "Quilt".to_string(),
        other => other.to_string(),
    }));
    cf::File {
        display_name: if v.name.is_empty() {
            v.version_number.clone()
        } else {
            v.name.clone()
        },
        file_name: f.filename.clone(),
        release_type: match v.version_type.as_str() {
            "beta" => 2,
            "alpha" => 3,
            _ => 1,
        },
        file_date: v.date_published.clone(),
        file_length: f.size,
        download_url: Some(f.url.clone()),
        game_versions: tags,
        id: num_id(&v.id),
        mod_id: num_id(&v.project_id),
        modrinth: Some(MrFile {
            project_id: v.project_id.clone(),
            version_id: v.id.clone(),
            sha1: f.hashes.get("sha1").cloned().unwrap_or_default(),
            requires: v
                .dependencies
                .iter()
                .filter(|d| d.dependency_type == "required")
                .filter_map(|d| d.project_id.clone())
                .collect(),
            changelog: v.changelog.clone().unwrap_or_default(),
        }),
        ..Default::default()
    }
}

pub fn search(s: &cf::Search) -> anyhow::Result<(Vec<cf::Mod>, cf::Pagination)> {
    let mut facets = vec![vec![format!(
        "project_type:{}",
        project_type_for(s.class_id)
    )]];
    if !s.game_version.is_empty() {
        facets.push(vec![format!("versions:{}", s.game_version)]);
    }
    if let Some(l) =
        loader_name(s.loader).filter(|_| matches!(s.class_id, cf::CLASS_MODS | cf::CLASS_MODPACKS))
    {
        facets.push(vec![format!("categories:{l}")]);
    }
    if let Some(name) = s.category.and_then(|c| real_id(c).ok()) {
        facets.push(vec![format!("categories:{name}")]);
    }
    let index = match s.sort {
        cf::SortField::TotalDownloads => "downloads",
        cf::SortField::LastUpdated => "updated",
        cf::SortField::Released => "newest",
        cf::SortField::Popularity => "follows",
        _ => "relevance",
    };
    let q = [
        ("query", s.text.trim().to_string()),
        ("facets", serde_json::to_string(&facets)?),
        ("index", index.to_string()),
        ("offset", s.index.to_string()),
        ("limit", cf::PAGE_SIZE.to_string()),
    ];
    let r: SearchResult = get("/search", &q)?;
    let mods = r
        .hits
        .into_iter()
        .map(|h| {
            let latest = h.latest_version.clone();
            let mut m = to_mod(
                h.project_id,
                h.slug,
                h.title,
                h.description,
                h.icon_url,
                h.downloads,
                h.author,
                &h.project_type,
                &h.categories,
                h.date_modified,
                h.date_created,
                h.gallery.into_iter().map(|u| (u, String::new())).collect(),
                Extra {
                    license: Some(h.license),
                    side: Some(env(&h.client_side, &h.server_side)).filter(|s| !s.is_empty()),
                    versions: h.versions,
                    ..Default::default()
                },
            );
            // The newest version, so a page of hits can be looked up at once.
            m.main_file_id = num_id(&latest);
            m
        })
        .collect();
    Ok((
        mods,
        cf::Pagination {
            index: s.index,
            page_size: cf::PAGE_SIZE,
            result_count: 0,
            total_count: r.total_hits,
        },
    ))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Tag {
    name: String,
    project_type: String,
    header: String,
}

/// Categories for a project type, as `cf::Category` (id from [`num_id`]).
pub fn categories(class_id: u32) -> anyhow::Result<Vec<cf::Category>> {
    let kind = project_type_for(class_id);
    let tags: Vec<Tag> = get("/tag/category", &[])?;
    let mut out: Vec<cf::Category> = tags
        .into_iter()
        .filter(|t| t.project_type == kind && t.header == "categories")
        .map(|t| cf::Category {
            id: num_id(&t.name),
            name: title_case(&t.name),
            ..Default::default()
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn title_case(s: &str) -> String {
    s.split('-')
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn project(id: &str) -> anyhow::Result<Project> {
    get(&format!("/project/{id}"), &[])
}

pub fn projects(ids: &[String]) -> anyhow::Result<Vec<Project>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for chunk in ids.chunks(200) {
        out.extend(get::<Vec<Project>>(
            "/projects",
            &[("ids", serde_json::to_string(chunk)?)],
        )?);
    }
    Ok(out)
}

/// Owner (or first member) of each team, by team id.
pub fn team_owners(team_ids: &[String]) -> anyhow::Result<HashMap<String, String>> {
    let mut out = HashMap::new();
    for chunk in team_ids.chunks(100) {
        let teams: Vec<Vec<Member>> = get("/teams", &[("ids", serde_json::to_string(chunk)?)])?;
        for (id, members) in chunk.iter().zip(teams) {
            let owner = members
                .iter()
                .find(|m| m.role.eq_ignore_ascii_case("owner"))
                .or(members.first());
            if let Some(m) = owner {
                out.insert(id.clone(), m.user.username.clone());
            }
        }
    }
    Ok(out)
}

/// Versions of a project, newest first, optionally filtered.
pub fn versions(project: &str, game_version: &str, loader: Loader) -> anyhow::Result<Vec<Version>> {
    let mut q = Vec::new();
    if let Some(l) = loader_name(loader) {
        q.push(("loaders", serde_json::to_string(&[l])?));
    }
    if !game_version.is_empty() {
        q.push(("game_versions", serde_json::to_string(&[game_version])?));
    }
    get(&format!("/project/{project}/version"), &q)
}

/// Versions by id, in one request.
pub fn versions_by_id(ids: &[String]) -> anyhow::Result<Vec<Version>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    get("/versions", &[("ids", serde_json::to_string(ids)?)])
}

/// Whether a file beside a pack's .mrpack is its server files.
pub fn is_server_zip(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.ends_with(".zip") && n.contains("server")
}

/// The server zip a pack version carries beside its .mrpack, as a file to
/// download (Modrinth has no server pack field; authors add it as an extra
/// file).
pub fn server_zip(v: &Version) -> Option<cf::File> {
    let f = v.files.iter().find(|f| !f.primary && is_server_zip(&f.filename))?;
    let mut out = version_to_file(v);
    out.id = num_id(&format!("{}/{}", v.id, f.filename));
    out.file_name = f.filename.clone();
    out.file_length = f.size;
    out.download_url = Some(f.url.clone());
    out.is_server_pack = Some(true);
    out.hashes = f
        .hashes
        .get("sha1")
        .map(|h| vec![cf::FileHash { value: h.clone(), algo: 1 }])
        .unwrap_or_default();
    // Its sha1 is the zip's, not the .mrpack's.
    out.modrinth = None;
    Some(out)
}

/// Versions by file sha1.
pub fn versions_by_sha1(hashes: &[String]) -> anyhow::Result<HashMap<String, Version>> {
    if hashes.is_empty() {
        return Ok(HashMap::new());
    }
    post(
        "/version_files",
        &serde_json::json!({ "hashes": hashes, "algorithm": "sha1" }),
    )
}

/// Newest version that fits, for each installed file's sha1.
pub fn latest_by_sha1(
    hashes: &[String],
    game_version: &str,
    loader: Loader,
) -> anyhow::Result<HashMap<String, Version>> {
    if hashes.is_empty() {
        return Ok(HashMap::new());
    }
    let mut body = serde_json::json!({ "hashes": hashes, "algorithm": "sha1", "game_versions": [game_version] });
    if let Some(l) = loader_name(loader) {
        body["loaders"] = serde_json::json!([l]);
    }
    post("/version_files/update", &body)
}

pub fn version(id: &str) -> anyhow::Result<Version> {
    get(&format!("/version/{id}"), &[])
}

/// "client" | "server" | "both" from Modrinth's declared sides, for jars
/// Modrinth knows, by file name. Cached per sha1 (misses too), so only new
/// jars cost a request.
pub fn sides_of(paths: &[std::path::PathBuf]) -> HashMap<String, String> {
    use std::sync::Mutex;
    #[derive(Default, serde::Serialize, serde::Deserialize)]
    struct Cache {
        /// sha1 -> side ("" when Modrinth doesn't know the file).
        sides: HashMap<String, String>,
        /// "name|size|mtime" -> sha1, so unchanged jars aren't re-read.
        hashes: HashMap<String, String>,
    }
    static CACHE: Mutex<Option<Cache>> = Mutex::new(None);
    let file = directories::ProjectDirs::from("", "", "Hopper")
        .map(|d| d.cache_dir().join("mrsides2.json"));
    let mut g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cache = g.get_or_insert_with(|| {
        file.as_ref()
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    });
    let mut dirty = false;
    let hashed: Vec<(String, String)> = paths
        .iter()
        .filter_map(|p| {
            let md = std::fs::metadata(p).ok()?;
            let mtime = md
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_secs();
            let name = p.file_name()?.to_string_lossy().into_owned();
            let key = format!("{name}|{}|{mtime}", md.len());
            let h = match cache.hashes.get(&key) {
                Some(h) => h.clone(),
                None => {
                    let h = sha1(&std::fs::read(p).ok()?);
                    cache.hashes.insert(key, h.clone());
                    dirty = true;
                    h
                }
            };
            Some((name, h))
        })
        .collect();
    let missing: Vec<String> = hashed
        .iter()
        .filter(|(_, h)| !cache.sides.contains_key(h))
        .map(|(_, h)| h.clone())
        .collect();
    if !missing.is_empty()
        && let Ok(versions) = versions_by_sha1(&missing)
    {
        let ids: Vec<String> = versions
            .values()
            .map(|v| v.project_id.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let sides: HashMap<String, &'static str> = projects(&ids)
            .unwrap_or_default()
            .iter()
            .map(|p| (p.id.clone(), env(&p.client_side, &p.server_side)))
            .collect();
        for h in &missing {
            let side = versions
                .get(h)
                .and_then(|v| sides.get(&v.project_id))
                .copied()
                .unwrap_or("");
            cache.sides.insert(h.clone(), side.to_string());
        }
        dirty = true;
    }
    if dirty
        && let Some(f) = &file
        && let Ok(b) = serde_json::to_vec(&*cache)
    {
        let _ = std::fs::write(f, b);
    }
    hashed
        .into_iter()
        .filter_map(|(name, h)| {
            cache
                .sides
                .get(&h)
                .filter(|s| !s.is_empty())
                .map(|s| (name, s.clone()))
        })
        .collect()
}

/// Sides for CurseForge mods whose jar Modrinth doesn't have: the Modrinth
/// project with the same slug, when its title also matches the mod's name
/// (a shared slug alone can be a different mod). Takes (slug, name) pairs;
/// returns slug -> "client" | "server" | "both". Cached on disk, misses too.
pub fn sides_by_slug(mods: &[(String, String)]) -> HashMap<String, String> {
    use std::sync::Mutex;
    static CACHE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
    let file = directories::ProjectDirs::from("", "", "Hopper")
        .map(|d| d.cache_dir().join("mrslugsides.json"));
    let mut g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cache = g.get_or_insert_with(|| {
        file.as_ref()
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    });
    let key = |slug: &str, name: &str| format!("{slug}|{name}");
    let missing: Vec<&(String, String)> = mods
        .iter()
        .filter(|(s, n)| !s.is_empty() && !cache.contains_key(&key(s, n)))
        .collect();
    let slugs: Vec<String> = missing.iter().map(|(s, _)| s.clone()).collect();
    if !slugs.is_empty()
        && let Ok(found) = projects(&slugs)
    {
        for (slug, name) in &missing {
            let side = found
                .iter()
                .find(|p| p.slug.eq_ignore_ascii_case(slug) && same_title(&p.title, name))
                .map_or("", |p| env(&p.client_side, &p.server_side));
            cache.insert(key(slug, name), side.to_string());
        }
        if let Some(f) = &file
            && let Ok(b) = serde_json::to_vec(&*cache)
        {
            let _ = std::fs::write(f, b);
        }
    }
    mods.iter()
        .filter_map(|(s, n)| {
            cache
                .get(&key(s, n))
                .filter(|v| !v.is_empty())
                .map(|v| (s.clone(), v.clone()))
        })
        .collect()
}

/// "Just Enough Items (JEI)" ~ "Just Enough Items": equal or one leads the
/// other, ignoring case and punctuation.
fn same_title(a: &str, b: &str) -> bool {
    let norm = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    let (a, b) = (norm(a), norm(b));
    let (short, long) = if a.len() <= b.len() { (&a, &b) } else { (&b, &a) };
    short.len() >= 4 && long.starts_with(short.as_str()) || (!a.is_empty() && a == b)
}

pub fn sha1(bytes: &[u8]) -> String {
    sha1_smol::Sha1::from(bytes).digest().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_from_sides() {
        assert_eq!(env("required", "unsupported"), "client");
        assert_eq!(env("unsupported", "required"), "server");
        assert_eq!(env("optional", "required"), "both");
    }

    #[test]
    fn sha1_matches_known_value() {
        assert_eq!(sha1(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    /// `cargo test real_modrinth -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_modrinth() {
        let dir = std::path::Path::new(r"G:\Games\Minecraft\Curse\Instances\TNP Limitless 8\mods");
        let h = sha1(&std::fs::read(dir.join("respackopts-4.14.0+1.21.1.forge.4.jar")).unwrap());
        let found = versions_by_sha1(std::slice::from_ref(&h)).unwrap();
        let v = &found[&h];
        let p = project(&v.project_id).unwrap();
        println!(
            "respackopts -> {} ({}) env {}",
            p.title,
            v.version_number,
            env(&p.client_side, &p.server_side)
        );
        let (hits, page) = search(&cf::Search {
            class_id: cf::CLASS_MODS,
            text: "sodium".into(),
            game_version: "1.21.1".into(),
            loader: Loader::NeoForge,
            ..Default::default()
        })
        .unwrap();
        println!(
            "search sodium: {} of {} -> {:?}",
            hits.len(),
            page.total_count,
            hits.first().map(|m| &m.name)
        );
        let latest = latest_by_sha1(std::slice::from_ref(&h), "1.21.1", Loader::NeoForge).unwrap();
        println!("latest: {:?}", latest.get(&h).map(|v| &v.version_number));
    }
}

/// Modrinth end to end in a temp folder:
/// `cargo test real_modrinth_end_to_end -- --ignored --nocapture`
#[cfg(test)]
#[test]
#[ignore]
fn real_modrinth_end_to_end() {
    use std::fs;
    crate::env::load(std::path::Path::new(".env"));
    let root = std::env::temp_dir().join("hopper-mr-e2e");
    let _ = fs::remove_dir_all(&root);
    let jobs = crate::jobs::Jobs::default();
    let mut inst = crate::modpack::create(&root, "MR", "1.21.1", "neoforge-21.1.256").unwrap();

    // Identify jars CurseForge does not know.
    let tnp = std::path::Path::new(r"G:\Games\Minecraft\Curse\Instances\TNP Limitless 8\mods");
    for j in [
        "respackopts-4.14.0+1.21.1.forge.4.jar",
        "libjf-3.17.7+forge.jar",
    ] {
        fs::copy(tnp.join(j), inst.mods_dir().join(j)).unwrap();
    }
    let n = crate::install::identify_loose(&mut inst, &jobs.start("identify")).unwrap();
    println!(
        "identified {n}: {:?}",
        inst.addons
            .iter()
            .map(|a| (&a.name, &a.author, &a.env))
            .collect::<Vec<_>>()
    );
    assert!(n >= 1 && inst.addons.iter().all(|a| !a.mr_project.is_empty()));

    // Install a Modrinth mod with its dependencies.
    let (hits, _) = search(&cf::Search {
        class_id: cf::CLASS_MODS,
        text: "Jade".into(),
        game_version: "1.21.1".into(),
        loader: Loader::NeoForge,
        ..Default::default()
    })
    .unwrap();
    let jade = hits
        .into_iter()
        .find(|m| m.name == "Jade 🔍" || m.name.starts_with("Jade"))
        .unwrap();
    let f = crate::install::best_file(jade.id, jade.class_id, &inst)
        .unwrap()
        .unwrap();
    let added = crate::install::install(&mut inst, &jade, &f, &jobs.start("jade")).unwrap();
    println!("installed {added:?} -> {}", f.file_name);
    assert!(inst.mods_dir().join(&f.file_name).exists());

    // Updates and changelogs come from Modrinth too.
    let ups = crate::install::check_updates(&inst).unwrap();
    println!(
        "updates: {:?}",
        ups.iter().map(|u| &u.file.file_name).collect::<Vec<_>>()
    );
    let a = inst
        .addons
        .iter()
        .find(|a| a.name.starts_with("Jade"))
        .unwrap()
        .clone();
    let (logs, all) = crate::install::changelogs(
        &inst,
        a.project_id,
        crate::modrinth::num_id(&a.mr_version),
        None,
        false,
    )
    .unwrap();
    println!(
        "jade versions: {}, installed changelog blocks: {}",
        all.len(),
        logs.last().map_or(0, |e| e.blocks.len())
    );
    assert!(!all.is_empty());

    // Switch Jade to CurseForge.
    let jade_file = inst
        .addons
        .iter()
        .find(|a| a.name.starts_with("Jade"))
        .unwrap()
        .file_name
        .clone();
    let msg = crate::install::switch_source(&mut inst, &jade_file, &jobs.start("switch")).unwrap();
    println!("{msg}");
    let cf_jade = inst
        .addons
        .iter()
        .find(|a| a.name.starts_with("Jade"))
        .unwrap();
    assert!(cf_jade.mr_project.is_empty() && !is_mr(cf_jade.project_id));
    assert_eq!(
        inst.addons
            .iter()
            .filter(|a| a.name.starts_with("Jade"))
            .count(),
        1
    );

    // A Modrinth modpack.
    let (packs, _) = search(&cf::Search {
        class_id: cf::CLASS_MODPACKS,
        text: "Simply Optimized".into(),
        game_version: "1.21.1".into(),
        ..Default::default()
    })
    .unwrap();
    let pack = packs.into_iter().next().unwrap();
    let pf = crate::source::files(pack.id, "1.21.1", Loader::Any)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let versions = crate::source::files(pack.id, "1.21.1", Loader::Any).unwrap();
    let older = versions.get(1).cloned().unwrap_or(pf.clone());
    let mut p = crate::modpack::install_mrpack(&root, &pack, &older, &jobs.start("pack")).unwrap();
    println!(
        "mrpack {} {} -> {} addons, loader {}, mc {}",
        p.name,
        older.display_name,
        p.addons.len(),
        p.loader,
        p.game_version
    );
    assert!(!p.addons.is_empty());
    // A newer version shows as a pack update, then applies in place.
    let packs = vec![(p.pack.clone().unwrap(), p.game_version.clone(), 1)];
    let ups = crate::instance_ops::pack_updates(&packs).unwrap();
    println!(
        "pack updates: {:?}",
        ups.iter().map(|(_, f)| &f.display_name).collect::<Vec<_>>()
    );
    if let Some((_, newer)) = ups.first() {
        crate::modpack::update_any(&mut p, pack.id, newer, None, &jobs.start("pack update")).unwrap();
        println!(
            "updated to {} -> {} addons",
            p.pack.as_ref().unwrap().version,
            p.addons.len()
        );
        assert_eq!(p.pack.as_ref().unwrap().file_id, newer.id);
    }
    let _ = fs::remove_dir_all(&root);
}

#[cfg(test)]
mod side_tests {
    use super::*;

    #[test]
    fn titles_match_loosely() {
        assert!(same_title("Just Enough Items (JEI)", "Just Enough Items"));
        assert!(same_title("AppleSkin", "appleskin"));
        assert!(!same_title("JEI", "Just Enough Items"));
        assert!(!same_title("Waystones", "Fabric Waystones Port"));
    }

    #[test]
    #[ignore = "network, reads the real pack"]
    fn real_sides() {
        let dir = std::path::Path::new(r"G:\Games\Minecraft\Curse\Instances\TNP Limitless 8\mods");
        let mut jars: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "jar"))
            .collect();
        jars.sort();
        jars.truncate(60);
        let t = std::time::Instant::now();
        let s = sides_of(&jars);
        println!("{} of {} known in {:?}", s.len(), jars.len(), t.elapsed());
        for (k, v) in s.iter().filter(|(_, v)| *v != "both") {
            println!("  {k}: {v}");
        }
        let t = std::time::Instant::now();
        assert_eq!(sides_of(&jars).len(), s.len());
        println!("cached in {:?}", t.elapsed());
    }
}
