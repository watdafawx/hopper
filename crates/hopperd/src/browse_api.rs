//! The panel's Browse page for modpacks, like Hopper desktop's: search
//! CurseForge and Modrinth together or one site, with Minecraft, loader,
//! category and sort filters; a pack's page (description, gallery, links)
//! and its versions with changelogs and whether each has server files.
//! Every lookup goes through hopper-core, as desktop's do.
//!
//! Besides modpacks it browses what goes into a server ("Install into"):
//! mods and data packs, marking what the server already has, and installs
//! a project (newest fitting file or a picked version) through
//! hopper-core's `browse::install_project`, as desktop's Browse does.
//!
//! Descriptions and changelogs go out as hopper-core's parsed blocks
//! (text, links, images), never as HTML, so a pack page can't inject
//! markup into the panel.

use crate::App;
use crate::api::{ApiResult, bad, require};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, Method};
use axum::routing::{get, post};
use axum::{Json, Router};
use hopper_core::cf::{self, Loader, SortField};
use hopper_core::instances::Instance;
use hopper_core::richtext::{self, Block, Kind, Span};
use hopper_core::{browse, install, instance_ops, modrinth};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/browse/search", get(search))
        .route("/api/v1/browse/categories", get(categories))
        .route("/api/v1/browse/pack", get(pack))
        .route("/api/v1/browse/versions", get(versions))
        .route("/api/v1/browse/changelog", get(changelog))
        .route("/api/v1/browse/target", get(target))
        .route("/api/v1/browse/install", post(install_into))
}

/// What's browsed: `type` = "modpacks" (the default), "mods" or "datapacks".
fn class_of(q: &HashMap<String, String>) -> u32 {
    match arg(q, "type").as_str() {
        "mods" => cf::CLASS_MODS,
        "datapacks" => cf::CLASS_DATAPACKS,
        _ => cf::CLASS_MODPACKS,
    }
}

/// The server content goes into (`into`), as an instance; none when not given.
fn target_of(app: &App, q: &HashMap<String, String>) -> ApiResult<Option<(Arc<crate::servers::Entry>, Instance)>> {
    let id = arg(q, "into");
    if id.is_empty() {
        return Ok(None);
    }
    let e = app.servers.get(&id).ok_or_else(|| crate::api::not_found("server"))?;
    let inst = crate::mods_api::instance_of(&e).map_err(|e| bad(format!("{e:#}")))?;
    Ok(Some((e, inst)))
}

/// Browse's loader key ("neoforge") for a loader.
fn loader_key(l: Loader) -> &'static str {
    match l {
        Loader::NeoForge => "neoforge",
        Loader::Forge => "forge",
        Loader::Fabric => "fabric",
        Loader::Quilt => "quilt",
        Loader::Any => "",
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(bad)?.map_err(|e| bad(format!("{e:#}")))
}

type Q = Query<HashMap<String, String>>;

fn arg(q: &HashMap<String, String>, k: &str) -> String {
    q.get(k).map(|s| s.trim().to_string()).unwrap_or_default()
}

fn has_cf_key() -> bool {
    cf::api_key().is_some()
}

/// Which sites to ask: "both" (the default), "curseforge" or "modrinth".
/// Both without a CurseForge key is Modrinth alone.
fn sites(q: &HashMap<String, String>) -> (bool, bool) {
    match arg(q, "source").as_str() {
        "curseforge" => (true, false),
        "modrinth" => (false, true),
        _ => (has_cf_key(), true),
    }
}

fn is_mr_source(q: &HashMap<String, String>) -> ApiResult<bool> {
    match arg(q, "source").as_str() {
        "modrinth" => Ok(true),
        "curseforge" => Ok(false),
        _ => Err(bad("Which site?")),
    }
}

/// Minecraft from a file's game versions ("1.21.1" among "NeoForge", ...).
fn mc_of(game_versions: &[String]) -> String {
    game_versions
        .iter()
        .find(|g| g.starts_with(|c: char| c.is_ascii_digit()) && g.contains('.'))
        .cloned()
        .unwrap_or_default()
}

/// Loader names in a file's game versions ("NeoForge").
fn loaders_of(game_versions: &[String]) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = game_versions
        .iter()
        .map(|g| Loader::from_loader_id(g))
        .filter(|l| *l != Loader::Any)
        .map(Loader::name)
        .collect();
    out.dedup();
    out
}

/// A search hit or project as the Browse cards show it. `id` is the site's
/// own id (CurseForge's number, Modrinth's string); `num` is Hopper's
/// number for it, which New server's pack endpoints take.
fn card_of(m: &cf::Mod, server_files: Option<bool>, class: u32) -> Value {
    let mr = m.modrinth.is_some();
    let mut cats: Vec<String> = m.categories.iter().map(|c| c.name.clone()).collect();
    // Modrinth lists loaders among its categories.
    cats.retain(|c| Loader::from_loader_id(c) == Loader::Any);
    json!({
        "id": m.modrinth.clone().unwrap_or_else(|| m.id.to_string()),
        "num": m.id.to_string(),
        "source": if mr { "modrinth" } else { "curseforge" },
        "name": m.name,
        "slug": m.slug,
        "summary": m.summary,
        "author": m.author(),
        "icon": m.icon().unwrap_or_default(),
        "downloads": m.download_count,
        "updated": m.date_modified,
        "created": m.date_created,
        "categories": cats,
        "loaders": m.loaders(),
        "mc_versions": m.game_versions().into_iter().take(4).collect::<Vec<_>>(),
        "server_files": server_files,
        // Modrinth's side support: "client" (client-only), "server", "both".
        "side": m.side,
        "url": if mr && class == cf::CLASS_MODPACKS { format!("https://modrinth.com/modpack/{}", m.slug) } else { m.page() },
    })
}

// ---- search ---------------------------------------------------------------------------------

async fn search(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Query(q): Q) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let (use_cf, use_mr) = sites(&q);
    let picked_cf = arg(&q, "source") == "curseforge";
    if picked_cf && !has_cf_key() {
        return Err(bad("CurseForge needs an API key: add one in Access & devices, or search Modrinth."));
    }
    let page: u32 = arg(&q, "page").parse().unwrap_or(0);
    let class = class_of(&q);
    // Marking what the server has needs its mod list: mods only (data
    // packs land as loose files with no project recorded).
    let have = if class == cf::CLASS_MODS { target_of(&app, &q)?.map(|(_, i)| i) } else { None };
    let base = cf::Search {
        class_id: class,
        text: arg(&q, "q"),
        game_version: arg(&q, "mc"),
        loader: match arg(&q, "loader").as_str() {
            "" => Loader::Any,
            l => Loader::from_loader_id(l),
        },
        category: None,
        sort: SortField::from_key(&arg(&q, "sort")).unwrap_or(SortField::Popularity),
        index: page * cf::PAGE_SIZE,
    };
    let cat_cf: Option<u64> = arg(&q, "cat_cf").parse().ok();
    let cat_mr = arg(&q, "cat_mr");
    // A category only one site has: the other site has nothing to show.
    let filtered = cat_cf.is_some() || !cat_mr.is_empty();
    let ask_cf = use_cf && (!filtered || cat_cf.is_some());
    let ask_mr = use_mr && (!filtered || !cat_mr.is_empty());

    let v = blocking(move || {
        let cf_s = cf::Search { category: cat_cf, ..base.clone() };
        let mr_s = cf::Search { category: (!cat_mr.is_empty()).then(|| modrinth::num_id(&cat_mr)), ..base };
        let (cf_r, mr_r) = std::thread::scope(|s| {
            let have = have.as_ref();
            let a = s.spawn(move || ask_cf.then(|| cf_search(&cf_s, have)));
            let b = s.spawn(move || ask_mr.then(|| mr_search(&mr_s, have)));
            (a.join().unwrap_or(None), b.join().unwrap_or(None))
        });
        let mut warnings: Vec<String> = Vec::new();
        if use_mr && !use_cf && arg_both(&q) {
            warnings.push("nokey".into());
        }
        let mut more = false;
        let mut total = 0u64;
        let mut lists: Vec<Vec<Value>> = Vec::new();
        for (site, r) in [("CurseForge", cf_r), ("Modrinth", mr_r)] {
            match r {
                None => {}
                Some(Ok((cards, p))) => {
                    total += p.total_count as u64;
                    more |= (p.index + p.page_size) < p.total_count.min(10_000);
                    lists.push(cards);
                }
                Some(Err(e)) => warnings.push(format!("{site} didn't answer: {e:#}")),
            }
        }
        if lists.is_empty() && !warnings.iter().all(|w| w == "nokey") {
            anyhow::bail!("{}", warnings.join(" "));
        }
        // Both sites: take turns, so each keeps its own order. A pack on
        // both (same name) shows once, saying where else it is.
        let name_key = |c: &Value| c["name"].as_str().unwrap_or("").to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect::<String>();
        let mut out: Vec<Value> = Vec::new();
        let n = lists.iter().map(Vec::len).max().unwrap_or(0);
        for i in 0..n {
            for l in &lists {
                let Some(c) = l.get(i) else { continue };
                let k = name_key(c);
                if let Some(first) = out.iter_mut().find(|o| !k.is_empty() && name_key(o) == k) {
                    first["also_on"] = c["source"].clone();
                    continue;
                }
                out.push(c.clone());
            }
        }
        Ok(json!({ "results": out, "more": more, "total": total, "warnings": warnings, "curseforge": has_cf_key() }))
    })
    .await?;
    Ok(Json(v))
}

fn arg_both(q: &HashMap<String, String>) -> bool {
    !matches!(arg(q, "source").as_str(), "curseforge" | "modrinth")
}

/// A search hit's card; for a target server, whether it has the project
/// already (the same project, or the same mod from the other site).
fn hit(m: &cf::Mod, server_files: Option<bool>, class: u32, have: Option<&Instance>) -> Value {
    let mut c = card_of(m, server_files, class);
    if let Some(inst) = have {
        c["installed"] = json!(browse::installed_addon(inst, m).is_some());
    }
    c
}

fn cf_search(s: &cf::Search, have: Option<&Instance>) -> anyhow::Result<(Vec<Value>, cf::Pagination)> {
    let (mods, p) = cf::search(s)?;
    let packs = s.class_id == cf::CLASS_MODPACKS;
    let cards = mods
        .iter()
        .map(|m| hit(m, packs.then(|| browse::curse_server_files(m).is_some()), s.class_id, have))
        .collect();
    Ok((cards, p))
}

fn mr_search(s: &cf::Search, have: Option<&Instance>) -> anyhow::Result<(Vec<Value>, cf::Pagination)> {
    let (mods, p) = modrinth::search(s)?;
    if s.class_id != cf::CLASS_MODPACKS {
        return Ok((mods.iter().map(|m| hit(m, None, s.class_id, have)).collect(), p));
    }
    // Server files from each hit's newest version, in one request.
    let newest: Vec<u64> = mods.iter().map(|m| m.main_file_id).filter(|v| *v != 0).collect();
    let files = browse::modrinth_server_files_for(&newest).unwrap_or_default();
    let cards = mods
        .iter()
        .map(|m| hit(m, files.get(&m.id).map(Option::is_some), s.class_id, have))
        .collect();
    Ok((cards, p))
}

// ---- categories -----------------------------------------------------------------------------

/// Categories of a type (`type`, modpacks by default) per site, and the
/// ones both share (by name), for the Both filter.
async fn categories(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Query(q): Q) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let class = class_of(&q);
    let v = blocking(move || {
        let cfc = if has_cf_key() { cf::categories(class).unwrap_or_default() } else { Vec::new() };
        let mrc = modrinth::categories(class).unwrap_or_default();
        // Modrinth's names for CurseForge's ("Technology" is "Tech").
        let key = |s: &str| {
            let k = s.to_lowercase().replace(['-', '_', ' ', '/', '&'], "").replace("and", "");
            match k.as_str() {
                "technology" => "tech".to_string(),
                "adventurerpg" => "adventure".to_string(),
                "combatpvp" => "combat".to_string(),
                "smalllight" => "lightweight".to_string(),
                "extralarge" => "kitchensink".to_string(),
                "hardcore" => "challenging".to_string(),
                _ => k,
            }
        };
        let cf_list: Vec<Value> = cfc.iter().map(|c| json!({ "id": c.id.to_string(), "name": c.name, "icon": c.icon_url })).collect();
        let mr_list: Vec<Value> = mrc
            .iter()
            .filter_map(|c| Some(json!({ "id": modrinth::real_id(c.id).ok()?, "name": c.name })))
            .collect();
        let both: Vec<Value> = cfc
            .iter()
            .filter_map(|c| {
                let m = mrc.iter().find(|m| key(&m.name) == key(&c.name))?;
                Some(json!({ "name": c.name, "cf": c.id.to_string(), "mr": modrinth::real_id(m.id).ok()?, "icon": c.icon_url }))
            })
            .collect();
        Ok(json!({ "curseforge": cf_list, "modrinth": mr_list, "both": both }))
    })
    .await?;
    Ok(Json(v))
}

// ---- a pack's page --------------------------------------------------------------------------

async fn pack(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Query(q): Q) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let mr = is_mr_source(&q)?;
    let id = arg(&q, "id");
    if id.is_empty() {
        return Err(bad("Which pack?"));
    }
    let class = class_of(&q);
    let have = target_of(&app, &q)?.map(|(_, i)| i);
    let v = blocking(move || {
        let (m, body) = if mr {
            let p = modrinth::project(&id)?;
            let owner = modrinth::team_owners(std::slice::from_ref(&p.team)).unwrap_or_default();
            let m = modrinth::project_to_mod(&p, owner.get(&p.team).map_or("", String::as_str));
            (m, richtext::parse_markdown(&p.body))
        } else {
            let n: u64 = id.parse().map_err(|_| anyhow::anyhow!("Which pack?"))?;
            let m = cf::get_mod(n)?;
            let d = cf::description(n).unwrap_or_default();
            (m, richtext::parse(&d))
        };
        let packs = class == cf::CLASS_MODPACKS;
        if packs {
            anyhow::ensure!(m.class_id.is_none_or(|c| c == cf::CLASS_MODPACKS), "{} isn't a modpack.", m.name);
        }
        let server_files = packs.then(|| {
            if mr {
                browse::modrinth_latest_server_files(m.id).ok().flatten().is_some()
            } else {
                browse::curse_server_files(&m).is_some()
            }
        });
        let mut c = card_of(&m, server_files, class);
        // What the target server has of it, for the Versions list.
        if let Some(inst) = &have {
            c["installed"] = match browse::installed_addon(inst, &m) {
                Some(a) => json!({ "file": a.file_name, "file_id": a.file_id.to_string(), "same_project": a.project_id == m.id }),
                None => Value::Null,
            };
        }
        let links: Vec<Value> = m.links.extra().into_iter().map(|(l, u)| json!({ "label": l, "url": u })).collect();
        c["links"] = json!(links);
        c["license"] = json!(m.license_type);
        c["gallery"] = json!(m
            .screenshots
            .iter()
            .filter(|s| is_http(&s.url))
            .map(|s| json!({ "url": s.url, "thumb": if is_http(&s.thumbnail_url) { &s.thumbnail_url } else { &s.url }, "title": s.title }))
            .collect::<Vec<_>>());
        c["description"] = blocks_json(&body);
        Ok(c)
    })
    .await?;
    Ok(Json(v))
}

// ---- versions and changelogs ----------------------------------------------------------------

async fn versions(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Query(q): Q) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let mr = is_mr_source(&q)?;
    let id = arg(&q, "id");
    let mc = arg(&q, "mc");
    let loader = match arg(&q, "loader").as_str() {
        "" => Loader::Any,
        l => Loader::from_loader_id(l),
    };
    let packs = class_of(&q) == cf::CLASS_MODPACKS;
    let v = blocking(move || {
        let rows: Vec<Value> = if mr {
            modrinth::versions(&id, &mc, loader)?
                .iter()
                .take(200)
                .map(|v| {
                    let f = modrinth::version_to_file(v);
                    let log = v.changelog.clone().unwrap_or_default();
                    json!({
                        "id": f.id.to_string(), "name": if v.version_number.is_empty() { &v.name } else { &v.version_number },
                        "title": v.name, "date": v.date_published, "mc_version": mc_of(&v.game_versions),
                        "loaders": loaders_of(&f.game_versions), "type": v.version_type,
                        "server_files": packs && browse::modrinth_server_files(v).is_some(),
                        "changelog": blocks_json(&richtext::parse_markdown(&log)),
                    })
                })
                .collect()
        } else {
            let n: u64 = id.parse().map_err(|_| anyhow::anyhow!("Which pack?"))?;
            cf::files(n, &mc, loader)?
                .iter()
                .filter(|f| f.is_server_pack != Some(true))
                .take(200)
                .map(|f| {
                    json!({
                        "id": f.id.to_string(), "name": f.display_name, "title": f.display_name, "date": f.file_date,
                        "mc_version": mc_of(&f.game_versions), "loaders": loaders_of(&f.game_versions),
                        "type": f.release_name().to_lowercase(),
                        "server_files": f.server_pack_file_id.is_some_and(|i| i != 0),
                        "changelog": Value::Null,
                    })
                })
                .collect()
        };
        Ok(rows)
    })
    .await?;
    Ok(Json(json!({ "versions": v })))
}

/// One CurseForge file's changelog (Modrinth's come with its versions).
async fn changelog(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Query(q): Q) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let project: u64 = arg(&q, "id").parse().map_err(|_| bad("Which pack?"))?;
    let file: u64 = arg(&q, "file").parse().map_err(|_| bad("Which version?"))?;
    let v = blocking(move || Ok(blocks_json(&richtext::parse(&cf::changelog(project, file)?)))).await?;
    Ok(Json(json!({ "changelog": v })))
}

// ---- installing into a server -------------------------------------------------------------

/// Where data packs can go on a server: each world's `datapacks/` (the
/// active one first), then global loader folders (Paxi…) the server has.
/// (label, folder relative to the server)
fn datapack_dirs(inst: &Instance) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut worlds = hopper_core::hosting::worlds::list(&inst.root);
    worlds.sort_by_key(|w| !w.active);
    for w in worlds {
        let label = if w.active { format!("World: {} (active)", w.name) } else { format!("World: {}", w.name) };
        out.push((label, format!("{}/datapacks", w.name)));
    }
    if out.is_empty() {
        let level = hopper_core::hosting::worlds::level_name(&inst.root);
        out.push((format!("World: {level} (active)"), format!("{level}/datapacks")));
    }
    // Global folders; a server's worlds aren't under saves/.
    out.extend(instance_ops::datapack_targets(inst).into_iter().filter(|(_, d)| !d.starts_with("saves/")));
    out
}

/// A server as an "Install into" target: its Minecraft and loader (which
/// set Browse's filters), whether it runs, and where data packs can go.
async fn target(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Query(q): Q) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let id = arg(&q, "into");
    let e = app.servers.get(&id).ok_or_else(|| crate::api::not_found("server"))?;
    let running = e.server.state().is_alive();
    let v = blocking(move || {
        let inst = crate::mods_api::instance_of(&e)?;
        let loader = install::loader_of(&inst);
        let dirs: Vec<Value> = datapack_dirs(&inst).into_iter().map(|(l, d)| json!({ "label": l, "path": d })).collect();
        Ok(json!({
            "id": e.id, "name": e.name, "game_version": inst.game_version,
            "loader": loader_key(loader), "loader_name": if loader == Loader::Any { "" } else { loader.name() },
            "running": running, "datapack_dirs": dirs,
        }))
    })
    .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Id {
    N(u64),
    S(String),
}

impl Id {
    fn num(&self) -> Option<u64> {
        match self {
            Id::N(n) => Some(*n),
            Id::S(s) => s.trim().parse().ok(),
        }
        .filter(|n| *n != 0)
    }
}

#[derive(Deserialize)]
struct InstallBody {
    /// The server.
    into: String,
    /// "mods" or "datapacks".
    #[serde(rename = "type")]
    kind: String,
    /// Hopper's project number (a card's `num`).
    project: Id,
    /// A version's id; none picks the newest file that fits the server.
    #[serde(default)]
    file: Option<Id>,
    /// Data packs: the folder, one of the target's `datapack_dirs`.
    #[serde(default)]
    dest: String,
}

/// Installs a mod (with its dependencies) or a data pack into a server, as
/// a job. Its result says what was added and whether the server runs (so
/// needs a restart to load it).
async fn install_into(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<InstallBody>) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let class = match b.kind.as_str() {
        "mods" => cf::CLASS_MODS,
        "datapacks" => cf::CLASS_DATAPACKS,
        _ => return Err(bad("Only mods and data packs install into a server.")),
    };
    let project = b.project.num().ok_or_else(|| bad("Which project?"))?;
    let file = b.file.as_ref().and_then(Id::num);
    let e = app.servers.get(&b.into).ok_or_else(|| crate::api::not_found("server"))?;
    if class == cf::CLASS_MODS {
        crate::mods_api::forget_updates(&e.id);
    }
    let job = crate::pack_api::spawn(&app, format!("Adding to {}", e.name), move |job| {
        let inst = crate::mods_api::instance_of(&e)?;
        anyhow::ensure!(
            !inst.game_version.is_empty() || file.is_some(),
            "Hopper doesn't know this server's Minecraft version yet. Start it once, or set the version in the Mods tab."
        );
        let dest = if class == cf::CLASS_DATAPACKS {
            let dirs = datapack_dirs(&inst);
            let d = if b.dest.is_empty() { dirs.first() } else { dirs.iter().find(|d| d.1 == b.dest) };
            let d = d.ok_or_else(|| anyhow::anyhow!("That isn't one of this server's data pack folders."))?;
            Some(inst.root.join(&d.1))
        } else {
            None
        };
        let m = hopper_core::source::get_mod(project)?;
        let kind = m.class_id.unwrap_or(class);
        anyhow::ensure!(kind == class, "{} isn't a {}.", m.name, if class == cf::CLASS_MODS { "mod" } else { "data pack" });
        let file = match file {
            Some(f) => Some(hopper_core::source::file(m.id, f)?),
            None => None,
        };
        job.detail(format!("Adding {}", m.name));
        let root = inst.root.clone();
        let place = browse::Placement { instances_root: &root, target: Some(inst), dest };
        let (_, line) = browse::install_project(&m, file, class, place, job)?;
        log::info!("{}: {line}", e.name);
        Ok(json!({ "name": m.name, "line": line, "restart_needed": e.server.state().is_alive() }))
    });
    Ok(Json(json!({ "job": job })))
}

// ---- blocks as JSON -------------------------------------------------------------------------

fn is_http(u: &str) -> bool {
    let u = u.trim().to_ascii_lowercase();
    u.starts_with("https://") || u.starts_with("http://")
}

/// Text runs: links and images only when they're http(s).
fn spans_json(spans: &[Span]) -> Value {
    json!(spans
        .iter()
        .map(|s| {
            let mut o = json!({ "t": s.text });
            if s.bold {
                o["b"] = json!(true);
            }
            if s.italic {
                o["i"] = json!(true);
            }
            if s.code {
                o["c"] = json!(true);
            }
            if let Some(l) = s.link.as_deref().filter(|l| is_http(l)) {
                o["href"] = json!(l);
            }
            if let Some(img) = s.image.as_deref().filter(|l| is_http(l)) {
                o["img"] = json!(img);
                if let Some(w) = s.width {
                    o["w"] = json!(w);
                }
            }
            o
        })
        .collect::<Vec<_>>())
}

fn blocks_json(blocks: &[Block]) -> Value {
    json!(blocks
        .iter()
        .take(2000)
        .map(|b| match &b.kind {
            Kind::Para => json!({ "k": "p", "s": spans_json(&b.spans) }),
            Kind::Heading(l) => json!({ "k": "h", "l": l, "s": spans_json(&b.spans) }),
            Kind::Bullet(d) => json!({ "k": "li", "d": d, "s": spans_json(&b.spans) }),
            Kind::Numbered(d) => json!({ "k": "ol", "d": d, "s": spans_json(&b.spans) }),
            Kind::Rule => json!({ "k": "hr" }),
            Kind::Code => json!({ "k": "code", "s": spans_json(&b.spans) }),
            Kind::Row(cells) => json!({ "k": "row", "cells": cells.iter().map(|c| spans_json(c)).collect::<Vec<_>>() }),
            Kind::Video(v) => json!({
                "k": "video",
                "url": is_http(&v.url).then_some(&v.url),
                "thumb": v.thumb.as_deref().filter(|t| is_http(t)),
                "site": v.site(),
            }),
        })
        .collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_drop_unsafe_links() {
        let b = richtext::parse(r#"<p><a href="javascript:alert(1)">x</a> <a href="https://a.b">y</a><img src="data:x"></p><script>bad()</script>"#);
        let v = blocks_json(&b).to_string();
        assert!(!v.contains("javascript:"));
        assert!(!v.contains("data:x"));
        assert!(!v.contains("bad()"));
        assert!(v.contains("https://a.b"));
    }

    #[test]
    fn loaders_from_tags() {
        let tags = vec!["1.21.1".to_string(), "NeoForge".to_string()];
        assert_eq!(loaders_of(&tags), vec!["NeoForge"]);
        assert_eq!(mc_of(&tags), "1.21.1");
    }
}
