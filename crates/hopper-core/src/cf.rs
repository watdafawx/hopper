//! CurseForge Core API client (https://docs.curseforge.com/rest-api/).
//!
//! The key comes from Settings (Credential Manager), else `CF_API_KEY` in
//! `.env`, else one built into the exe (see [`api_key_source`]). Every call
//! blocks, so the UI runs them on worker threads.

use anyhow::{Context, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use std::time::Duration;

const BASE: &str = "https://api.curseforge.com";
pub const MINECRAFT: u32 = 432;
pub const CLASS_MODS: u32 = 6;
pub const CLASS_MODPACKS: u32 = 4471;
pub const CLASS_RESOURCE_PACKS: u32 = 12;
pub const CLASS_SHADERS: u32 = 6552;
pub const CLASS_WORLDS: u32 = 17;
pub const CLASS_DATAPACKS: u32 = 6945;

/// Dependency `relationType` values.
pub const DEP_REQUIRED: u32 = 3;
pub const DEP_INCOMPATIBLE: u32 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Loader {
    #[default]
    Any,
    Forge,
    Fabric,
    Quilt,
    NeoForge,
}

impl Loader {
    pub const ALL: [Loader; 5] = [
        Loader::Any,
        Loader::NeoForge,
        Loader::Forge,
        Loader::Fabric,
        Loader::Quilt,
    ];

    pub fn api_id(self) -> Option<u32> {
        match self {
            Loader::Any => None,
            Loader::Forge => Some(1),
            Loader::Fabric => Some(4),
            Loader::Quilt => Some(5),
            Loader::NeoForge => Some(6),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Loader::Any => "Any loader",
            Loader::Forge => "Forge",
            Loader::Fabric => "Fabric",
            Loader::Quilt => "Quilt",
            Loader::NeoForge => "NeoForge",
        }
    }

    /// From an instance loader id such as "neoforge-21.1.256".
    pub fn from_loader_id(id: &str) -> Loader {
        let l = id.to_ascii_lowercase();
        if l.starts_with("neoforge") {
            Loader::NeoForge
        } else if l.starts_with("forge") {
            Loader::Forge
        } else if l.starts_with("fabric") {
            Loader::Fabric
        } else if l.starts_with("quilt") {
            Loader::Quilt
        } else {
            Loader::Any
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SortField {
    Featured = 1,
    #[default]
    Popularity = 2,
    LastUpdated = 3,
    Name = 4,
    TotalDownloads = 6,
    Released = 11,
}

impl SortField {
    pub const ALL: [SortField; 6] = [
        SortField::Popularity,
        SortField::Featured,
        SortField::TotalDownloads,
        SortField::LastUpdated,
        SortField::Released,
        SortField::Name,
    ];

    pub fn name(self) -> &'static str {
        match self {
            SortField::Featured => "Featured",
            SortField::Popularity => "Popularity",
            SortField::LastUpdated => "Last updated",
            SortField::Name => "Name",
            SortField::TotalDownloads => "Downloads",
            SortField::Released => "Newest",
        }
    }

    /// Its key in settings.
    pub fn key(self) -> &'static str {
        match self {
            SortField::Featured => "featured",
            SortField::Popularity => "popularity",
            SortField::LastUpdated => "updated",
            SortField::Name => "name",
            SortField::TotalDownloads => "downloads",
            SortField::Released => "newest",
        }
    }

    pub fn from_key(key: &str) -> Option<SortField> {
        Self::ALL.into_iter().find(|f| f.key() == key)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Mod {
    pub id: u64,
    pub name: String,
    pub slug: String,
    pub summary: String,
    pub download_count: f64,
    pub class_id: Option<u32>,
    pub logo: Option<Logo>,
    pub authors: Vec<Author>,
    pub links: Links,
    pub categories: Vec<Category>,
    pub latest_files_indexes: Vec<FileIndex>,
    /// The newest files, as search hits carry them (with server pack ids).
    pub latest_files: Vec<File>,
    pub main_file_id: u64,
    pub date_modified: String,
    pub date_created: String,
    /// `false` when the author blocks third-party downloads.
    pub allow_mod_distribution: Option<bool>,
    pub screenshots: Vec<Screenshot>,
    /// "MIT License", "All Rights Reserved"...; Modrinth's SPDX name.
    pub license_type: Option<String>,
    /// Set for a Modrinth project (its id); CurseForge fields stay empty.
    #[serde(skip)]
    pub modrinth: Option<String>,
    /// Modrinth's side support: "client", "server" or "both".
    #[serde(skip)]
    pub side: Option<&'static str>,
    /// Modrinth's loaders and game versions (CurseForge's come from
    /// [`Self::latest_files_indexes`]).
    #[serde(skip)]
    pub mr_loaders: Vec<String>,
    #[serde(skip)]
    pub mr_versions: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Screenshot {
    pub title: String,
    pub thumbnail_url: String,
    pub url: String,
}

impl Screenshot {
    /// The small copy, else the full image (the thumbnail can be empty).
    pub fn thumb(&self) -> &str {
        if self.thumbnail_url.is_empty() { &self.url } else { &self.thumbnail_url }
    }
}

impl Mod {
    pub fn icon(&self) -> Option<&str> {
        self.logo.as_ref().and_then(Logo::icon)
    }

    pub fn author(&self) -> &str {
        self.authors.first().map_or("", |a| a.name.as_str())
    }

    pub fn page(&self) -> String {
        if self.links.website_url.is_empty() {
            format!("https://www.curseforge.com/minecraft/mc-mods/{}", self.slug)
        } else {
            self.links.website_url.clone()
        }
    }

    /// Minecraft versions with a file, newest first.
    pub fn game_versions(&self) -> Vec<String> {
        let mut out: Vec<String> = if self.modrinth.is_some() {
            self.mr_versions.clone()
        } else {
            self.latest_files_indexes
                .iter()
                .map(|f| f.game_version.clone())
                .collect()
        };
        out.retain(|v| v.starts_with(|c: char| c.is_ascii_digit()));
        out.sort_by(|a, b| crate::scan::compare_versions(b, a));
        out.dedup();
        out
    }

    /// "client", "server" or "both": Modrinth's side support, else the
    /// "Client"/"Server" environment tags on CurseForge's newest files.
    pub fn side(&self) -> Option<&'static str> {
        if self.modrinth.is_some() {
            return self.side;
        }
        let tags = |f: &File| {
            let has = |t: &str| f.game_versions.iter().any(|v| v.eq_ignore_ascii_case(t));
            (has("Client"), has("Server"))
        };
        // The main file speaks for the project; otherwise any newest file.
        let (client, server) = match self.latest_files.iter().find(|f| f.id == self.main_file_id) {
            Some(f) if tags(f) != (false, false) => tags(f),
            _ => self
                .latest_files
                .iter()
                .map(tags)
                .fold((false, false), |a, b| (a.0 || b.0, a.1 || b.1)),
        };
        match (client, server) {
            (true, true) => Some("both"),
            (true, false) => Some("client"),
            (false, true) => Some("server"),
            _ => None,
        }
    }

    /// [`Self::side`] for one Minecraft version and loader ("NeoForge"): the
    /// newest such file's tags, since authors tag each build on its own.
    /// Falls back to [`Self::side`] when no file matches or it's untagged.
    pub fn side_for(&self, game_version: &str, loader: Option<&str>) -> Option<&'static str> {
        if self.modrinth.is_some() || game_version.is_empty() {
            return self.side();
        }
        let has = |f: &File, t: &str| f.game_versions.iter().any(|v| v.eq_ignore_ascii_case(t));
        let newest = self
            .latest_files
            .iter()
            .filter(|f| has(f, game_version) && loader.is_none_or(|l| has(f, l)))
            .max_by(|a, b| a.file_date.cmp(&b.file_date).then(a.id.cmp(&b.id)));
        match newest.map(|f| (has(f, "Client"), has(f, "Server"))) {
            Some((true, true)) => Some("both"),
            Some((true, false)) => Some("client"),
            Some((false, true)) => Some("server"),
            _ => self.side(),
        }
    }

    /// Loader names with a file, in Hopper's usual order.
    pub fn loaders(&self) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = if self.modrinth.is_some() {
            self.mr_loaders
                .iter()
                .map(|l| Loader::from_loader_id(l))
                .filter(|l| *l != Loader::Any)
                .map(Loader::name)
                .collect()
        } else {
            self.latest_files_indexes
                .iter()
                .filter_map(|f| f.mod_loader)
                .filter_map(|id| Loader::ALL.into_iter().find(|l| l.api_id() == Some(id)))
                .map(Loader::name)
                .collect()
        };
        out.sort_by_key(|n| Loader::ALL.iter().position(|l| l.name() == *n));
        out.dedup();
        out
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Logo {
    pub thumbnail_url: String,
    pub url: String,
}

impl Logo {
    /// The small copy, else the full image. CurseForge leaves the
    /// thumbnail empty for some animated logos (Applied Energistics 2).
    pub fn icon(&self) -> Option<&str> {
        [&self.thumbnail_url, &self.url].into_iter().map(String::as_str).find(|u| !u.is_empty())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Author {
    /// CurseForge's author id; 0 for Modrinth, where the name is the user.
    pub id: u64,
    pub name: String,
    pub url: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Links {
    pub website_url: String,
    pub wiki_url: Option<String>,
    pub issues_url: Option<String>,
    pub source_url: Option<String>,
    /// Modrinth only.
    pub discord_url: Option<String>,
}

impl Links {
    /// (label, url) for every link but the project page, in display order.
    pub fn extra(&self) -> Vec<(&'static str, &str)> {
        [
            ("Source", &self.source_url),
            ("Issues", &self.issues_url),
            ("Wiki", &self.wiki_url),
            ("Discord", &self.discord_url),
        ]
        .into_iter()
        .filter_map(|(l, u)| {
            let u = u.as_deref()?.trim();
            u.starts_with("http").then_some((l, u))
        })
        .collect()
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Category {
    pub id: u64,
    pub name: String,
    pub icon_url: String,
    pub class_id: Option<u32>,
    pub is_class: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FileIndex {
    pub game_version: String,
    pub file_id: u64,
    pub filename: String,
    pub release_type: u32,
    pub mod_loader: Option<u32>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct File {
    pub id: u64,
    pub mod_id: u64,
    pub display_name: String,
    pub file_name: String,
    /// 1 release, 2 beta, 3 alpha.
    pub release_type: u32,
    pub file_date: String,
    pub file_length: u64,
    /// `None` when the author blocks third-party downloads.
    pub download_url: Option<String>,
    /// Minecraft versions plus tags like "NeoForge", "Client", "Server".
    pub game_versions: Vec<String>,
    pub dependencies: Vec<Dependency>,
    pub file_fingerprint: u64,
    pub server_pack_file_id: Option<u64>,
    pub is_server_pack: Option<bool>,
    /// Checksums CurseForge lists for the file (algo 1 sha1, 2 md5).
    pub hashes: Vec<FileHash>,
    /// Set for a Modrinth version.
    #[serde(skip)]
    pub modrinth: Option<crate::modrinth::MrFile>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct FileHash {
    pub value: String,
    pub algo: u32,
}

impl File {
    /// The file's sha1 (lower-case hex), when known.
    pub fn sha1(&self) -> Option<String> {
        self.hashes
            .iter()
            .find(|h| h.algo == 1 && !h.value.is_empty())
            .map(|h| h.value.to_ascii_lowercase())
            .or_else(|| {
                self.modrinth
                    .as_ref()
                    .map(|m| m.sha1.to_ascii_lowercase())
                    .filter(|h| !h.is_empty())
            })
    }

    /// What a download of this file must come to.
    pub fn expect(&self) -> Expect {
        Expect {
            len: self.file_length,
            sha1: self.sha1().unwrap_or_default(),
        }
    }

    pub fn release_name(&self) -> &'static str {
        match self.release_type {
            1 => "Release",
            2 => "Beta",
            3 => "Alpha",
            _ => "?",
        }
    }

    /// Where a user downloads the file by hand when the API gives no URL.
    pub fn manual_url(&self, m: &Mod) -> String {
        format!("{}/download/{}", m.page().trim_end_matches('/'), self.id)
    }

    pub fn fits(&self, game_version: &str, loader: Loader) -> bool {
        let has = |t: &str| self.game_versions.iter().any(|v| v.eq_ignore_ascii_case(t));
        (game_version.is_empty() || has(game_version))
            && (loader == Loader::Any || has(loader.name()))
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Dependency {
    pub mod_id: u64,
    pub relation_type: u32,
}

#[derive(Deserialize)]
struct Data<T> {
    data: T,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Pagination {
    pub index: u32,
    pub page_size: u32,
    pub result_count: u32,
    pub total_count: u32,
}

#[derive(Deserialize)]
struct Page<T> {
    data: Vec<T>,
    #[serde(default)]
    pagination: Pagination,
}

#[derive(Clone, Debug, Default)]
pub struct Search {
    pub class_id: u32,
    pub text: String,
    pub game_version: String,
    pub loader: Loader,
    pub category: Option<u64>,
    pub sort: SortField,
    pub index: u32,
}

/// Where the CurseForge key came from, for Settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeySource {
    /// Given for this run only ([`set_session_key`]; Hopper Server's own key).
    Session,
    Saved,
    Env,
    BuiltIn,
}

static KEY: std::sync::Mutex<Option<Option<(&'static str, KeySource)>>> =
    std::sync::Mutex::new(None);
static SESSION: std::sync::Mutex<Option<&'static str>> = std::sync::Mutex::new(None);

/// The CurseForge API key in use. See [`api_key_source`] for the order.
pub fn api_key() -> Option<&'static str> {
    api_key_source().map(|(k, _)| k)
}

/// The key in use and where it came from: a key given for this run, else
/// the one saved in Settings (Windows Credential Manager), else
/// `CF_API_KEY` from `.env`, else one built into this exe. A saved key wins
/// over `.env` so a key the player pastes in Settings always takes effect;
/// `.env` still serves dev builds with nothing saved.
pub fn api_key_source() -> Option<(&'static str, KeySource)> {
    let mut cache = KEY.lock().unwrap_or_else(|e| e.into_inner());
    *cache.get_or_insert_with(|| {
        let session = SESSION.lock().unwrap_or_else(|e| e.into_inner()).map(String::from);
        let found = pick_key(
            session,
            crate::secrets::get(crate::secrets::CF_KEY),
            std::env::var("CF_API_KEY").ok(),
            option_env!("HOPPER_CF_KEY").map(String::from),
        );
        // Leaked once per change of key, so callers can hold it freely.
        found.map(|(k, s)| (&*Box::leak(k.into_boxed_str()), s))
    })
}

/// The first non-blank key, in order of precedence.
fn pick_key(
    session: Option<String>,
    saved: Option<String>,
    env: Option<String>,
    built_in: Option<String>,
) -> Option<(String, KeySource)> {
    [
        (session, KeySource::Session),
        (saved, KeySource::Saved),
        (env, KeySource::Env),
        (built_in, KeySource::BuiltIn),
    ]
    .into_iter()
    .find_map(|(k, s)| Some((k?.trim().to_string(), s)).filter(|(k, _)| !k.is_empty()))
}

/// Whether `CF_API_KEY` in `.env` holds a different key from the saved one
/// in use, so Settings can say the `.env` one is being ignored.
pub fn env_key_ignored() -> bool {
    match api_key_source() {
        Some((k, KeySource::Saved)) => std::env::var("CF_API_KEY")
            .ok()
            .is_some_and(|e| !e.trim().is_empty() && e.trim() != k),
        _ => false,
    }
}

fn forget_key() {
    *KEY.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Uses `key` for this run only (Hopper Server gets the desktop's key for a
/// server pack update when it has none of its own). Empty forgets it.
pub fn set_session_key(key: &str) {
    let key = key.trim();
    *SESSION.lock().unwrap_or_else(|e| e.into_inner()) =
        (!key.is_empty()).then(|| &*Box::leak(key.to_string().into_boxed_str()));
    forget_key();
}

/// Saves the key in Credential Manager (empty removes it). Takes effect at
/// once: the next call reads the key again.
pub fn set_api_key(key: &str) -> anyhow::Result<()> {
    crate::secrets::set(crate::secrets::CF_KEY, key)?;
    forget_key();
    Ok(())
}

/// What a player reads when CurseForge turns the key down (401 or 403).
pub const KEY_REJECTED: &str = "CurseForge rejected the API key. Check it in Settings → CurseForge & Modrinth.";

/// The error for a key CurseForge turned down, so callers can spot it
/// ([`key_rejected`]) and offer a way to Settings.
#[derive(Debug)]
pub struct KeyRejected;

impl std::fmt::Display for KeyRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(KEY_REJECTED)
    }
}

impl std::error::Error for KeyRejected {}

/// Whether `e` (or anything it wraps) is CurseForge rejecting the key.
pub fn key_rejected(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.is::<KeyRejected>())
}

/// Whether an error already turned into text is CurseForge rejecting the key.
pub fn key_rejected_text(e: &str) -> bool {
    e.contains(KEY_REJECTED)
}

/// `e` for a player: the plain key message when CurseForge rejected the
/// key (whatever `other` would have said), else `other(e)`.
pub fn explain(e: &anyhow::Error, other: impl FnOnce(&anyhow::Error) -> String) -> String {
    if key_rejected(e) { KEY_REJECTED.to_string() } else { other(e) }
}

/// A failed API call: a turned-down key becomes [`KeyRejected`], anything
/// else keeps the request in its message.
fn call_error(e: ureq::Error, what: impl FnOnce() -> String) -> anyhow::Error {
    match e {
        ureq::Error::StatusCode(401 | 403) => KeyRejected.into(),
        e => anyhow::Error::new(e).context(what()),
    }
}

/// Asks CurseForge whether `key` works (one small request for Minecraft's
/// game entry), without saving it.
pub fn test_key(key: &str) -> anyhow::Result<()> {
    let key = key.trim();
    anyhow::ensure!(!key.is_empty(), "Paste a key first.");
    match agent()
        .get(format!("{BASE}/v1/games/{MINECRAFT}"))
        .header("x-api-key", key)
        .call()
    {
        Ok(_) => Ok(()),
        Err(ureq::Error::StatusCode(401 | 403)) => anyhow::bail!(
            "CurseForge didn't accept this key. Copy it again from console.curseforge.com → API keys."
        ),
        Err(ureq::Error::StatusCode(c)) => anyhow::bail!("CurseForge answered {c}. Try again in a moment."),
        Err(e) => Err(anyhow::anyhow!(e).context("Couldn't reach CurseForge")),
    }
}

/// The agent for API calls: a whole call, body included, gets 60 s.
pub fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(60)))
            // A connection that stalls gives up soon, so a retry gets its
            // turn instead of the whole minute going to one attempt.
            .timeout_connect(Some(Duration::from_secs(10)))
            .user_agent(concat!("Hopper/", env!("CARGO_PKG_VERSION")))
            .build()
            .into()
    })
}

/// The agent for file downloads. A download may take as long as it needs,
/// so there is no overall cap; connecting and the response headers have
/// their own limits. ureq has no per-read timeout, so each download also
/// gets a body budget from its size (see [`body_budget`]).
pub fn download_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_send_request(Some(Duration::from_secs(30)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            .user_agent(concat!("Hopper/", env!("CARGO_PKG_VERSION")))
            .build()
            .into()
    })
}

/// How long reading a body of `len` bytes may take: 15 minutes, plus a
/// minute per 2 MB beyond 64 MB (about 32 KB/s). Unknown sizes get 2 hours.
pub fn body_budget(len: u64) -> Duration {
    if len == 0 {
        return Duration::from_secs(2 * 3600);
    }
    let extra = len.saturating_sub(64 << 20) / (2 << 20);
    Duration::from_secs(15 * 60 + extra * 60)
}

fn key() -> anyhow::Result<&'static str> {
    api_key().context("No CurseForge API key. Add one in Settings → CurseForge & Modrinth.")
}

fn get<T: DeserializeOwned>(path: &str, query: &[(&str, String)]) -> anyhow::Result<T> {
    let mut req = agent()
        .get(format!("{BASE}{path}"))
        .header("x-api-key", key()?);
    for (k, v) in query {
        req = req.query(*k, v);
    }
    let mut resp = req.call().map_err(|e| call_error(e, || format!("GET {path}")))?;
    resp.body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_json()
        .with_context(|| format!("reading {path}"))
}

fn post<T: DeserializeOwned>(path: &str, body: &serde_json::Value) -> anyhow::Result<T> {
    let mut resp = agent()
        .post(format!("{BASE}{path}"))
        .header("x-api-key", key()?)
        .send_json(body)
        .map_err(|e| call_error(e, || format!("POST {path}")))?;
    resp.body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_json()
        .with_context(|| format!("reading {path}"))
}

pub const PAGE_SIZE: u32 = 40;

pub fn search(s: &Search) -> anyhow::Result<(Vec<Mod>, Pagination)> {
    let mut q = vec![
        ("gameId", MINECRAFT.to_string()),
        ("classId", s.class_id.to_string()),
        ("sortField", (s.sort as u32).to_string()),
        (
            "sortOrder",
            if s.sort == SortField::Name {
                "asc"
            } else {
                "desc"
            }
            .into(),
        ),
        ("pageSize", PAGE_SIZE.to_string()),
        ("index", s.index.to_string()),
    ];
    if !s.text.trim().is_empty() {
        q.push(("searchFilter", s.text.trim().to_string()));
    }
    if !s.game_version.is_empty() {
        q.push(("gameVersion", s.game_version.clone()));
    }
    if let Some(l) = s.loader.api_id() {
        q.push(("modLoaderType", l.to_string()));
    }
    if let Some(c) = s.category {
        q.push(("categoryId", c.to_string()));
    }
    let page: Page<Mod> = get("/v1/mods/search", &q)?;
    Ok((page.data, page.pagination))
}

/// Every project by one author, most downloaded first.
pub fn author_mods(author_id: u64) -> anyhow::Result<Vec<Mod>> {
    let q = [
        ("gameId", MINECRAFT.to_string()),
        ("authorId", author_id.to_string()),
        ("sortField", (SortField::TotalDownloads as u32).to_string()),
        ("sortOrder", "desc".into()),
        ("pageSize", "50".into()),
    ];
    let page: Page<Mod> = get("/v1/mods/search", &q)?;
    // Only the author's own, should the filter ever be ignored.
    Ok(page
        .data
        .into_iter()
        .filter(|m| m.authors.iter().any(|a| a.id == author_id))
        .collect())
}

/// The project with this page slug (in `class`, when known).
pub fn by_slug(class: Option<u32>, slug: &str) -> anyhow::Result<Option<Mod>> {
    let mut q = vec![("gameId", MINECRAFT.to_string()), ("slug", slug.to_string())];
    if let Some(c) = class {
        q.push(("classId", c.to_string()));
    }
    let page: Page<Mod> = get("/v1/mods/search", &q)?;
    let mut hits: Vec<Mod> = page.data.into_iter().filter(|m| m.slug == slug).collect();
    // Without a class, a mod beats a pack or world of the same slug.
    hits.sort_by_key(|m| m.class_id != Some(CLASS_MODS));
    Ok(hits.into_iter().next())
}

pub fn get_mod(id: u64) -> anyhow::Result<Mod> {
    Ok(get::<Data<Mod>>(&format!("/v1/mods/{id}"), &[])?.data)
}

pub fn get_mods(ids: &[u64]) -> anyhow::Result<Vec<Mod>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(post::<Data<Vec<Mod>>>("/v1/mods", &serde_json::json!({ "modIds": ids }))?.data)
}

pub fn description(id: u64) -> anyhow::Result<String> {
    Ok(get::<Data<String>>(&format!("/v1/mods/{id}/description"), &[])?.data)
}

/// Most files [`files`] returns.
pub const MAX_FILES: usize = 1000;

/// Newest first, up to [`MAX_FILES`]. Filters are optional.
pub fn files(mod_id: u64, game_version: &str, loader: Loader) -> anyhow::Result<Vec<File>> {
    const PAGE: usize = 50;
    let mut out: Vec<File> = Vec::new();
    loop {
        let mut q = vec![
            ("pageSize", PAGE.to_string()),
            ("index", out.len().to_string()),
        ];
        if !game_version.is_empty() {
            q.push(("gameVersion", game_version.to_string()));
        }
        if let Some(l) = loader.api_id() {
            q.push(("modLoaderType", l.to_string()));
        }
        let page: Page<File> = get(&format!("/v1/mods/{mod_id}/files"), &q)?;
        let got = page.data.len();
        out.extend(page.data);
        if !more_pages(got, out.len(), page.pagination.total_count as usize) {
            break;
        }
    }
    out.truncate(MAX_FILES);
    Ok(out)
}

/// Whether to ask for another page after one of `got` items, with `have`
/// collected out of `total`.
fn more_pages(got: usize, have: usize, total: usize) -> bool {
    got > 0 && have < total && have < MAX_FILES
}

pub fn file(mod_id: u64, file_id: u64) -> anyhow::Result<File> {
    Ok(get::<Data<File>>(&format!("/v1/mods/{mod_id}/files/{file_id}"), &[])?.data)
}

/// A file's record as the API sends it, every field kept.
pub fn file_value(mod_id: u64, file_id: u64) -> anyhow::Result<serde_json::Value> {
    Ok(get::<Data<serde_json::Value>>(&format!("/v1/mods/{mod_id}/files/{file_id}"), &[])?.data)
}

/// A file's changelog (HTML).
pub fn changelog(mod_id: u64, file_id: u64) -> anyhow::Result<String> {
    Ok(get::<Data<String>>(&format!("/v1/mods/{mod_id}/files/{file_id}/changelog"), &[])?.data)
}

pub fn get_files(ids: &[u64]) -> anyhow::Result<Vec<File>> {
    let mut out = Vec::new();
    for chunk in ids.chunks(500) {
        out.extend(
            post::<Data<Vec<File>>>("/v1/mods/files", &serde_json::json!({ "fileIds": chunk }))?
                .data,
        );
    }
    Ok(out)
}

pub fn categories(class_id: u32) -> anyhow::Result<Vec<Category>> {
    let q = [
        ("gameId", MINECRAFT.to_string()),
        ("classId", class_id.to_string()),
    ];
    let mut cats = get::<Data<Vec<Category>>>("/v1/categories", &q)?.data;
    cats.retain(|c| c.is_class != Some(true));
    cats.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(cats)
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GameVersion {
    pub version_string: String,
    pub approved: bool,
}

/// CurseForge's record for a loader version ("neoforge-21.1.262"), as its
/// launcher stores it in `minecraftinstance.json`.
pub fn modloader(name: &str) -> anyhow::Result<serde_json::Value> {
    Ok(get::<Data<serde_json::Value>>(&format!("/v1/minecraft/modloader/{name}"), &[])?.data)
}

/// Release Minecraft versions, newest first.
pub fn minecraft_versions() -> anyhow::Result<Vec<String>> {
    let v = get::<Data<Vec<GameVersion>>>("/v1/minecraft/version", &[])?.data;
    Ok(v.into_iter()
        .map(|v| v.version_string)
        .filter(|s| s.chars().all(|c| c.is_ascii_digit() || c == '.'))
        .collect())
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LoaderVersion {
    pub name: String,
    pub game_version: String,
    pub latest: bool,
    pub recommended: bool,
}

/// Loader versions for one Minecraft version, newest first per loader,
/// from each loader's own list (as Prism's meta server collects them):
/// NeoForge's maven, Forge's metadata, Fabric's and Quilt's meta. No key.
/// Lists are kept for the session, per Minecraft version.
pub fn loader_versions(game_version: &str) -> anyhow::Result<Vec<LoaderVersion>> {
    static CACHE: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, Vec<LoaderVersion>>>> =
        std::sync::LazyLock::new(Default::default);
    if let Some(v) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).get(game_version) {
        return Ok(v.clone());
    }
    let v = open_loader_versions(game_version);
    anyhow::ensure!(!v.is_empty(), "Couldn't list loader versions for Minecraft {game_version}");
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(game_version.to_string(), v.clone());
    Ok(v)
}

/// NeoForge's version for a Minecraft version starts with: 1.21.1 → "21.1.",
/// 1.21 → "21.0.", and newer versions like 26.3 → "26.3.".
fn neoforge_prefix(mc: &str) -> String {
    match mc.strip_prefix("1.") {
        Some(rest) if rest.contains('.') => format!("{rest}."),
        Some(rest) => format!("{rest}.0."),
        None => format!("{mc}."),
    }
}

/// The loaders' own version lists (no key needed), newest first per loader.
pub fn open_loader_versions(mc: &str) -> Vec<LoaderVersion> {
    fn json(url: &str) -> Option<serde_json::Value> {
        agent().get(url).call().ok()?.body_mut().read_json().ok()
    }
    // The four lists at once.
    let urls = [
        "https://maven.neoforged.net/api/maven/versions/releases/net/neoforged/neoforge".to_string(),
        "https://files.minecraftforge.net/net/minecraftforge/forge/maven-metadata.json".to_string(),
        format!("https://meta.fabricmc.net/v2/versions/loader/{mc}"),
        format!("https://meta.quiltmc.org/v3/versions/loader/{mc}"),
    ];
    let [neo, forge, fabric, quilt]: [Option<serde_json::Value>; 4] = std::thread::scope(|s| {
        let hs: Vec<_> = urls.iter().map(|u| s.spawn(move || json(u))).collect();
        let mut out = hs.into_iter().map(|h| h.join().ok().flatten());
        [out.next().flatten(), out.next().flatten(), out.next().flatten(), out.next().flatten()]
    });
    let mut out: Vec<LoaderVersion> = Vec::new();
    let mut push = |kind: &str, versions: Vec<String>| {
        let mut versions = versions;
        versions.sort_by(|a, b| crate::scan::compare_versions(b, a));
        for (i, v) in versions.into_iter().enumerate() {
            out.push(LoaderVersion {
                name: format!("{kind}-{v}"),
                game_version: mc.to_string(),
                latest: i == 0,
                recommended: i == 0,
            });
        }
    };
    // NeoForge (1.20.2 and newer).
    if let Some(v) = neo {
        let prefix = neoforge_prefix(mc);
        push(
            "neoforge",
            v["versions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|x| x.as_str())
                .filter(|x| x.starts_with(&prefix))
                .map(String::from)
                .collect(),
        );
    }
    // Forge: "1.20.1-47.4.10" under the Minecraft version.
    if let Some(v) = forge {
        push(
            "forge",
            v[mc]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|x| x.as_str())
                .filter_map(|x| x.strip_prefix(&format!("{mc}-")).map(String::from))
                .collect(),
        );
    }
    for (kind, v) in [("fabric", fabric), ("quilt", quilt)] {
        if let Some(v) = v {
            push(
                kind,
                v.as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|x| x["loader"]["version"].as_str())
                    .filter(|x| !x.contains("beta") && !x.contains("pre"))
                    .map(String::from)
                    .collect(),
            );
        }
    }
    out
}

#[cfg(test)]
mod open_loaders {
    #[test]
    fn neoforge_prefixes() {
        assert_eq!(super::neoforge_prefix("1.21.1"), "21.1.");
        assert_eq!(super::neoforge_prefix("1.21"), "21.0.");
        assert_eq!(super::neoforge_prefix("26.3"), "26.3.");
    }

    /// `cargo test --lib real_open_loaders -- --ignored --nocapture` (network)
    #[test]
    #[ignore]
    fn real_open_loaders() {
        let v = super::open_loader_versions("1.21.1");
        for kind in ["neoforge-", "forge-", "fabric-", "quilt-"] {
            let n = v.iter().filter(|l| l.name.starts_with(kind)).count();
            let first = v.iter().find(|l| l.name.starts_with(kind)).map(|l| l.name.clone());
            println!("{kind}: {n} versions, newest {first:?}");
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FingerprintMatch {
    pub id: u64,
    pub file: File,
    pub latest_files: Vec<File>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct FingerprintResult {
    exact_matches: Vec<FingerprintMatch>,
}

/// Identifies local jars by CurseForge's murmur2 fingerprint.
pub fn match_fingerprints(fps: &[u32]) -> anyhow::Result<Vec<FingerprintMatch>> {
    let mut out = Vec::new();
    for chunk in fps.chunks(1000) {
        let r: Data<FingerprintResult> = post(
            &format!("/v1/fingerprints/{MINECRAFT}"),
            &serde_json::json!({ "fingerprints": chunk }),
        )?;
        out.extend(r.data.exact_matches);
    }
    Ok(out)
}

/// CurseForge's file fingerprint: murmur2 (seed 1) over the bytes with
/// tab, LF, CR and space removed.
pub fn fingerprint(bytes: &[u8]) -> u32 {
    const M: u32 = 0x5bd1_e995;
    let data: Vec<u8> = bytes
        .iter()
        .copied()
        .filter(|b| !matches!(b, 9 | 10 | 13 | 32))
        .collect();
    let mut h: u32 = 1 ^ data.len() as u32;
    let (chunks, rest) = data.as_chunks::<4>();
    for c in chunks {
        let mut k = u32::from_le_bytes(*c);
        k = k.wrapping_mul(M);
        k ^= k >> 24;
        k = k.wrapping_mul(M);
        h = h.wrapping_mul(M) ^ k;
    }
    if !rest.is_empty() {
        for (i, b) in rest.iter().enumerate() {
            h ^= (*b as u32) << (8 * i);
        }
        h = h.wrapping_mul(M);
    }
    h ^= h >> 13;
    h = h.wrapping_mul(M);
    h ^ (h >> 15)
}

/// The CDN URL for a file. Errors when the author blocks third-party
/// downloads, so the caller can send the user to the website instead.
pub fn download_url(f: &File) -> anyhow::Result<String> {
    if let Some(u) = f.download_url.as_ref().filter(|u| !u.is_empty()) {
        return Ok(u.clone());
    }
    let r: anyhow::Result<Data<Option<String>>> = get(
        &format!("/v1/mods/{}/files/{}/download-url", f.mod_id, f.id),
        &[],
    );
    match r {
        Ok(Data { data: Some(u) }) if !u.is_empty() => Ok(u),
        _ => bail!(
            "{} can only be downloaded from the CurseForge website",
            f.file_name
        ),
    }
}

/// What a finished download must be; zero or empty skips that check.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Expect {
    pub len: u64,
    /// Lower-case hex.
    pub sha1: String,
}

/// Streams `url` into `dest` through a temp file. `progress` gets bytes so far.
pub fn download(
    url: &str,
    dest: &std::path::Path,
    progress: impl FnMut(u64),
) -> anyhow::Result<()> {
    download_checked(url, dest, &Expect::default(), None, progress)
}

/// [`download`], checking the result against `expect` and stopping with
/// "Cancelled" when `job` is cancelled. Retries once after a server error
/// (5xx) or a timeout. Nothing is left behind on failure.
pub fn download_checked(
    url: &str,
    dest: &std::path::Path,
    expect: &Expect,
    job: Option<&crate::jobs::Job>,
    mut progress: impl FnMut(u64),
) -> anyhow::Result<()> {
    let cancelled = || job.is_some_and(|j| j.is_cancelled());
    let mut attempt = 0;
    loop {
        attempt += 1;
        match download_once(url, dest, expect, &cancelled, &mut progress) {
            Err(e) if attempt == 1 && !cancelled() && is_transient(&e) => {
                log::warn!("retrying {url}: {e:#}");
                std::thread::sleep(Duration::from_secs(2));
            }
            r => return r,
        }
    }
}

fn download_once(
    url: &str,
    dest: &std::path::Path,
    expect: &Expect,
    cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(u64),
) -> anyhow::Result<()> {
    if cancelled() {
        bail!("Cancelled");
    }
    let resp = download_agent()
        .get(url)
        .config()
        .timeout_recv_body(Some(body_budget(expect.len)))
        .build()
        .call()
        .with_context(|| format!("download {url}"))?;
    let mut expect = expect.clone();
    // A plain (not re-encoded) body says how long it is.
    if expect.len == 0
        && resp.headers().get("content-encoding").is_none()
        && let Some(n) = resp.body().content_length()
    {
        expect.len = n;
    }
    let reader = resp.into_body().into_reader();
    write_checked(reader, dest, &expect, cancelled, progress)
        .with_context(|| format!("download {url}"))
}

/// Writes `reader` to `dest` through `<dest>.hopper-part`, checking length
/// and sha1 against `expect` before the rename. The temp file is removed
/// on any error.
pub(crate) fn write_checked(
    mut reader: impl std::io::Read,
    dest: &std::path::Path,
    expect: &Expect,
    cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(u64),
) -> anyhow::Result<()> {
    use std::io::Write;
    if let Some(p) = dest.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = dest.with_extension("hopper-part");
    let r = (|| -> anyhow::Result<()> {
        let mut out = std::fs::File::create(&tmp)?;
        let mut sha = sha1_smol::Sha1::new();
        let mut buf = vec![0u8; 256 * 1024];
        let mut total = 0u64;
        loop {
            if cancelled() {
                bail!("Cancelled");
            }
            let n = match reader.read(&mut buf) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            };
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            sha.update(&buf[..n]);
            total += n as u64;
            progress(total);
        }
        out.flush()?;
        drop(out);
        if expect.len != 0 && total != expect.len {
            bail!("{} is {total} bytes, expected {}", file_label(dest), expect.len);
        }
        if !expect.sha1.is_empty() {
            let got = sha.digest().to_string();
            if !got.eq_ignore_ascii_case(&expect.sha1) {
                bail!("{} has sha1 {got}, expected {}", file_label(dest), expect.sha1);
            }
        }
        std::fs::rename(&tmp, dest)?;
        Ok(())
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

fn file_label(p: &std::path::Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// The HTTP status of a failed request, when it got that far.
pub fn http_status(e: &anyhow::Error) -> Option<u16> {
    e.chain().find_map(|c| match c.downcast_ref::<ureq::Error>() {
        Some(ureq::Error::StatusCode(s)) => Some(*s),
        // A turned-down key loses its ureq error but is still a refusal.
        _ => c.is::<KeyRejected>().then_some(403),
    })
}

/// A server error (5xx) or a timeout: worth one more try.
pub fn is_transient(e: &anyhow::Error) -> bool {
    if http_status(e).is_some_and(|s| s >= 500) {
        return true;
    }
    let ureq_timeout = |u: &ureq::Error| matches!(u, ureq::Error::Timeout(_));
    e.chain().any(|c| {
        c.downcast_ref::<ureq::Error>().is_some_and(ureq_timeout)
            || c.downcast_ref::<std::io::Error>().is_some_and(|io| {
                io.kind() == std::io::ErrorKind::TimedOut
                    || io
                        .get_ref()
                        .and_then(|i| i.downcast_ref::<ureq::Error>())
                        .is_some_and(ureq_timeout)
            })
    })
}

/// Bytes `start..=end` of a remote file.
pub fn fetch_range(url: &str, start: u64, end: u64) -> anyhow::Result<Vec<u8>> {
    // edge.forgecdn.net 404s on ranged GETs; mediafilez serves the same path.
    let url = &url.replace("://edge.forgecdn.net/", "://mediafilez.forgecdn.net/");
    let mut resp = agent()
        .get(url)
        .header("Range", format!("bytes={start}-{end}"))
        .call()?;
    if resp.status() != 206 {
        bail!("server ignored the range request ({})", resp.status());
    }
    Ok(resp
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_vec()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn some(k: &str) -> Option<String> {
        Some(k.to_string())
    }

    #[test]
    fn a_saved_key_wins_over_env_and_built_in() {
        assert_eq!(
            pick_key(None, some("saved"), some("env"), some("built")),
            Some(("saved".into(), KeySource::Saved))
        );
    }

    #[test]
    fn env_then_built_in_fill_in_when_nothing_is_saved() {
        assert_eq!(pick_key(None, None, some("env"), some("built")), Some(("env".into(), KeySource::Env)));
        assert_eq!(pick_key(None, some("  "), some(" env "), None), Some(("env".into(), KeySource::Env)));
        assert_eq!(pick_key(None, None, some(""), some("built")), Some(("built".into(), KeySource::BuiltIn)));
        assert_eq!(pick_key(None, None, None, None), None);
    }

    #[test]
    fn a_session_key_wins_over_everything() {
        assert_eq!(
            pick_key(some("panel"), some("saved"), some("env"), some("built")),
            Some(("panel".into(), KeySource::Session))
        );
    }

    #[test]
    fn a_rejected_key_reads_plainly_and_is_spotted_through_context() {
        let e = call_error(ureq::Error::StatusCode(403), || "POST /v1/mods".into()).context("checking updates");
        assert!(key_rejected(&e));
        assert!(key_rejected_text(&format!("{e:#}")));
        assert_eq!(explain(&e, |e| format!("{e:#}")), KEY_REJECTED);
        let other = call_error(ureq::Error::StatusCode(500), || "POST /v1/mods".into());
        assert!(!key_rejected(&other));
        assert!(format!("{other:#}").starts_with("POST /v1/mods"));
    }

    #[test]
    fn an_empty_logo_thumbnail_falls_back_to_the_full_image() {
        // Applied Energistics 2's animated logo, as the API returns it.
        let m: Mod = serde_json::from_str(
            r#"{"logo":{"thumbnailUrl":"","url":"https://media.forgecdn.net/avatars/1025/127/638548475358792693.webp"}}"#,
        )
        .unwrap();
        assert_eq!(m.icon(), Some("https://media.forgecdn.net/avatars/1025/127/638548475358792693.webp"));
        let m: Mod = serde_json::from_str(r#"{"logo":{"thumbnailUrl":"https://t/x.png","url":"https://u/x.png"}}"#).unwrap();
        assert_eq!(m.icon(), Some("https://t/x.png"));
        assert_eq!(Mod::default().icon(), None);
        let s = Screenshot { url: "https://u/s.png".into(), ..Default::default() };
        assert_eq!(s.thumb(), "https://u/s.png");
    }

    #[test]
    fn side_from_environment_tags() {
        let file = |id: u64, tags: &[&str]| File {
            id,
            game_versions: tags.iter().map(|t| t.to_string()).collect(),
            ..Default::default()
        };
        let m = |main: u64, files: Vec<File>| Mod {
            main_file_id: main,
            latest_files: files,
            ..Default::default()
        };
        assert_eq!(m(1, vec![file(1, &["1.21.1", "NeoForge", "Client", "Server"])]).side(), Some("both"));
        assert_eq!(m(1, vec![file(1, &["1.21.1", "Client"])]).side(), Some("client"));
        // The main file wins over older ones.
        assert_eq!(m(2, vec![file(1, &["Client"]), file(2, &["Server"])]).side(), Some("server"));
        // An untagged main file falls back to the rest.
        assert_eq!(m(2, vec![file(1, &["Client"]), file(2, &["1.20.1"])]).side(), Some("client"));
        assert_eq!(m(1, vec![file(1, &["1.20.1", "Forge"])]).side(), None);

        // Per version: Advancement Plaques tags its 1.21.1 builds for both
        // sides and its newest (main) build client-only.
        let mut old = file(1, &["1.21.1", "NeoForge", "Client", "Server"]);
        old.file_date = "2025-01-01".into();
        let mut new = file(2, &["26.3", "Forge", "Client"]);
        new.file_date = "2026-09-22".into();
        let plaques = m(2, vec![old, new]);
        assert_eq!(plaques.side(), Some("client"));
        assert_eq!(plaques.side_for("1.21.1", Some("NeoForge")), Some("both"));
        assert_eq!(plaques.side_for("1.21.1", Some("Fabric")), Some("client"));
    }

    #[test]
    fn fingerprint_ignores_whitespace() {
        assert_eq!(fingerprint(b"a b\tc\r\n"), fingerprint(b"abc"));
        assert_ne!(fingerprint(b"abc"), fingerprint(b"abd"));
    }

    fn write(bytes: &[u8], expect: &Expect, tag: &str) -> (std::path::PathBuf, anyhow::Result<()>) {
        let dir = std::env::temp_dir().join(format!("hopper-dl-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let dest = dir.join("mod.jar");
        let r = write_checked(
            std::io::Cursor::new(bytes.to_vec()),
            &dest,
            expect,
            &|| false,
            &mut |_| {},
        );
        (dest, r)
    }

    #[test]
    fn downloads_are_checked() {
        let body = b"hello jar";
        let sha = sha1_smol::Sha1::from(body).digest().to_string();
        let good = Expect {
            len: body.len() as u64,
            sha1: sha.to_uppercase(),
        };
        let (dest, r) = write(body, &good, "good");
        r.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        let _ = std::fs::remove_dir_all(dest.parent().unwrap());

        for (i, bad) in [
            Expect {
                len: 3,
                sha1: String::new(),
            },
            Expect {
                len: 0,
                sha1: "0".repeat(40),
            },
            Expect {
                len: body.len() as u64,
                sha1: "1".repeat(40),
            },
        ]
        .iter()
        .enumerate()
        {
            let (dest, r) = write(body, bad, &format!("bad{i}"));
            assert!(r.is_err(), "{bad:?}");
            assert!(!dest.exists());
            assert!(
                !dest.with_extension("hopper-part").exists(),
                "temp file left"
            );
            let _ = std::fs::remove_dir_all(dest.parent().unwrap());
        }

        // Nothing to check: anything goes.
        let (dest, r) = write(body, &Expect::default(), "none");
        r.unwrap();
        let _ = std::fs::remove_dir_all(dest.parent().unwrap());
    }

    #[test]
    fn cancel_stops_a_download() {
        let dir = std::env::temp_dir().join(format!("hopper-dl-cancel-{}", std::process::id()));
        let dest = dir.join("x.jar");
        let r = write_checked(
            std::io::Cursor::new(vec![1u8; 10]),
            &dest,
            &Expect::default(),
            &|| true,
            &mut |_| {},
        );
        assert_eq!(r.unwrap_err().to_string(), "Cancelled");
        assert!(!dest.exists() && !dest.with_extension("hopper-part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transient_errors() {
        assert!(is_transient(&anyhow::Error::new(ureq::Error::StatusCode(
            503
        ))));
        assert!(!is_transient(&anyhow::Error::new(ureq::Error::StatusCode(
            404
        ))));
        assert!(is_transient(
            &anyhow::Error::new(ureq::Error::Timeout(ureq::Timeout::RecvBody)).context("download")
        ));
        assert_eq!(
            http_status(&anyhow::Error::new(ureq::Error::StatusCode(403)).context("x")),
            Some(403)
        );
    }

    #[test]
    fn file_hashes() {
        let f: File = serde_json::from_str(
            r#"{"id":1,"fileLength":5,"hashes":[{"value":"ABC","algo":1},{"value":"m","algo":2}]}"#,
        )
        .unwrap();
        assert_eq!(
            f.expect(),
            Expect {
                len: 5,
                sha1: "abc".into()
            }
        );
    }

    #[test]
    fn paging_stops() {
        assert!(more_pages(50, 50, 120));
        assert!(!more_pages(20, 120, 120));
        assert!(!more_pages(0, 50, 120));
        assert!(!more_pages(50, MAX_FILES, 5000));
    }

    #[test]
    fn loader_from_id() {
        assert_eq!(
            Loader::from_loader_id("neoforge-21.1.256"),
            Loader::NeoForge
        );
        assert_eq!(Loader::from_loader_id("forge-47.3.0"), Loader::Forge);
        assert_eq!(Loader::from_loader_id("fabric-loader-0.16"), Loader::Fabric);
    }
}

/// The real jar CurseForge recorded as fingerprint 515050638.
#[cfg(test)]
#[test]
#[ignore]
fn real_fingerprint() {
    let b = std::fs::read(r"G:\Games\Minecraft\Curse\Instances\TNP Limitless 8\mods\create-enchantment-industry-2.4.2.jar").unwrap();
    assert_eq!(fingerprint(&b), 515050638);
}

/// Looks up TNP's untracked jars by fingerprint.
/// `cargo test real_loose_fingerprints -- --ignored --nocapture`
#[cfg(test)]
#[test]
#[ignore]
fn real_loose_fingerprints() {
    crate::env::load(std::path::Path::new(".env"));
    let dir = std::path::Path::new(r"G:\Games\Minecraft\Curse\Instances\TNP Limitless 8\mods");
    for name in [
        "libjf-3.17.7+forge.jar",
        "respackopts-4.14.0+1.21.1.forge.4.jar",
        "foxstweaks-neoforge-1.21.1-1.11.0.jar",
    ] {
        let fp = fingerprint(&std::fs::read(dir.join(name)).unwrap());
        let m = match_fingerprints(&[fp]).unwrap();
        println!(
            "{name}: fingerprint {fp} -> {:?}",
            m.iter()
                .map(|m| (&m.id, &m.file.file_name))
                .collect::<Vec<_>>()
        );
    }
}
