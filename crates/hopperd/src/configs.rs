//! The server's Configs page, like Hopper desktop's Configs tab: every
//! config file with the mod it belongs to and who reads it, a search across
//! all their settings, several files read at once, and a batch of edits to
//! one file checked together and saved with the old copy in
//! `.hopper-backups`. The same access as the Files routes, and every path
//! is checked against the server folder.

use crate::App;
use crate::api::{ApiResult, bad, not_found, require};
use crate::servers::Entry as Server;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, Method};
use axum::routing::{get, post};
use axum::{Json, Router};
use hopper_core::configedit::{self, Edit, Entry, Format, Kind, Restart, Section, Value as CValue};
use hopper_core::configindex::{self, CfgFile};
use hopper_core::configowner::{self, ModRef, Owners};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// The text of a file travels with each batch (as `base`).
const MAX_BODY: usize = 8 * 1024 * 1024;

/// What a secret shows as; sent back unchanged, it leaves the value alone.
pub(crate) const MASK: &str = "••••••";

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/servers/{id}/configs/index", get(index))
        .route("/api/v1/servers/{id}/configs/search", get(search))
        .route("/api/v1/servers/{id}/configs/parse", post(parse).layer(DefaultBodyLimit::max(MAX_BODY)))
        .route("/api/v1/servers/{id}/configs/batch", post(batch).layer(DefaultBodyLimit::max(MAX_BODY)))
}

fn server(app: &App, headers: &HeaderMap, method: &Method, id: &str) -> ApiResult<Arc<Server>> {
    require(app, headers, method)?;
    app.servers.get(id).ok_or_else(|| not_found("server"))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(bad)
}

// ---- mods and files ------------------------------------------------------------------

/// The server's jars as configs are matched to them, with the panel's names
/// and icons for the ones Hopper knows. Keys are the jar's name without
/// `.disabled`, as the Mods list keys its rows.
pub(crate) fn mod_refs(root: &std::path::Path) -> (Vec<ModRef>, HashMap<String, String>) {
    let Ok(dir) = crate::files::resolve(root, "mods") else { return Default::default() };
    let (refs, known) = configowner::folder_mods(root, &dir);
    let icons = known.into_iter().filter(|(_, (_, i))| !i.is_empty()).map(|(k, (_, i))| (k, i)).collect();
    (refs, icons)
}

/// The config files Hopper reads as settings, each in a folder that stays
/// inside the server folder (a linked `config/` leading elsewhere is left out).
fn files_of(root: &std::path::Path, owners: &Owners) -> Vec<CfgFile> {
    let mut ok_dirs: HashMap<String, bool> = HashMap::new();
    configindex::index(root, owners)
        .into_iter()
        .filter(|f| {
            let dir = f.rel.rsplit_once('/').map_or("", |(d, _)| d).to_string();
            *ok_dirs.entry(dir.clone()).or_insert_with(|| crate::files::resolve(root, &dir).is_ok())
        })
        .collect()
}

/// A path the page may read as settings: a config file, inside the server folder.
fn readable(root: &std::path::Path, rel: &str) -> Result<(), String> {
    if !configedit::editable(rel) || Format::of(rel).is_none() {
        return Err("That isn't a config file Hopper can read as settings.".into());
    }
    crate::files::resolve(root, rel).map(|_| ()).map_err(|e| e.to_string())
}

fn format_name(f: Option<Format>) -> &'static str {
    match f {
        Some(Format::Toml) => "toml",
        Some(Format::ForgeCfg) => "cfg",
        Some(Format::Snbt) => "snbt",
        Some(Format::Json) => "json",
        Some(Format::Flat) => "flat",
        None => "",
    }
}

// ---- one setting as JSON --------------------------------------------------------------

/// Keys whose values are passwords, tokens and the like: shown masked. The
/// list is hopper-core's, shared with the desktop and friends' settings sync.
pub(crate) fn secret(key: &str) -> bool {
    configedit::secret_key(key)
}

fn masked(e: &Entry) -> bool {
    secret(&e.key) && matches!(&e.value, CValue::Str(s) if !s.is_empty())
}

/// Whether a file has values that are only ever shown masked.
pub(crate) fn has_secrets(sections: &[Section]) -> bool {
    sections.iter().flat_map(|s| &s.entries).any(masked)
}

/// `text` with every secret's value replaced by the mask (only the value's
/// own characters, written as the format writes a string), so a token never
/// reaches the web page. The text this gives is for showing: saves go onto
/// the panel's own copy of the file.
pub(crate) fn mask_text(f: Format, text: &str, sections: &[Section]) -> String {
    let secrets: Vec<&Entry> = sections.iter().flat_map(|s| &s.entries).filter(|e| masked(e)).collect();
    let mut out = text.to_string();
    for e in &secrets {
        if let Ok(t) = configedit::set_as(f, &out, &e.path, &e.key, &CValue::Str(MASK.into())) {
            out = t;
        }
    }
    // The same value anywhere else (a comment, a second copy) goes too.
    for e in &secrets {
        if let CValue::Str(v) = &e.value
            && v.chars().count() >= 6
        {
            out = out.replace(v.as_str(), MASK);
        }
    }
    out
}

/// One setting, the same for every route that shows settings (here and the
/// Files editor's), so they agree.
pub(crate) fn entry_json(e: &Entry) -> Value {
    let kind = e.kind();
    let (kind_name, choices, color_fmt) = match &kind {
        Kind::Bool => ("bool", Vec::new(), None),
        Kind::Int => ("int", Vec::new(), None),
        Kind::Float => ("float", Vec::new(), None),
        Kind::Str => ("str", Vec::new(), None),
        Kind::Choice(c) => ("choice", c.clone(), None),
        Kind::List => ("list", Vec::new(), None),
        Kind::Color(f) => (
            "color",
            Vec::new(),
            Some(match f {
                configedit::ColorFmt::Int => "int",
                configedit::ColorFmt::Hash => "hash",
                configedit::ColorFmt::Hex0x => "0x",
            }),
        ),
        Kind::Opaque => ("opaque", Vec::new(), None),
    };
    let hide = masked(e);
    let text = e.value.text();
    json!({
        "key": e.key,
        "label": e.label(),
        "path": e.path,
        "kind": kind_name,
        "choices": choices,
        // The older Files editor reads `allowed`.
        "allowed": e.allowed,
        "color_fmt": color_fmt,
        "value": if hide { MASK.to_string() } else { text.clone() },
        "masked": hide,
        "list": if kind == Kind::List { configedit::list_items(&text) } else { None },
        "color": if matches!(kind, Kind::Color(_)) { configedit::color_of(&e.value) } else { None },
        "comment": e.comment,
        "range": e.range.map(|(lo, hi)| json!([lo, hi])),
        "slider": e.slider().map(|(lo, hi)| json!([lo, hi])),
        "default": if hide { None } else { e.default.clone() },
        // The default as this setting's value would be written (Reset sends it).
        "default_value": if hide { None } else { e.default_value().map(|v| v.text()) },
        "is_default": if hide { None } else { e.is_default() },
        "restart": match e.restart {
            Restart::None => "none",
            Restart::World => "world",
            Restart::Game => "game",
        },
        "guessed": e.guessed,
        "line": e.line,
    })
}

pub(crate) fn sections_json(sections: &[Section]) -> Value {
    Value::Array(
        sections
            .iter()
            .map(|s| json!({ "path": s.path, "comment": s.comment, "entries": s.entries.iter().map(entry_json).collect::<Vec<_>>() }))
            .collect(),
    )
}

// ---- index and search -------------------------------------------------------------------

/// `{ mods: [{ key, name, icon, files, changed }], files: [{ rel, format,
/// side, side_label, size, owner, changed, settings }], unmatched: [rel],
/// settings }`: every config file, its mod (the one rule desktop uses), who
/// reads it, and how many settings aren't at the default the file states.
async fn index(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let e = server(&app, &headers, &method, &id)?;
    let v = blocking(move || build_index(&e.root)).await?;
    Ok(Json(v))
}

pub(crate) fn build_index(root: &std::path::Path) -> Value {
    let (refs, icons) = mod_refs(root);
    let owners = Owners::new(refs);
    let files = files_of(root, &owners);
    let rels: Vec<String> = files.iter().map(|f| f.rel.clone()).collect();
    let sums: HashMap<String, configindex::Summary> = configindex::summaries(root, &rels).into_iter().collect();
    // The counts come from a cache; read the files for search behind the page.
    let warm_root = root.to_path_buf();
    std::thread::spawn(move || configindex::warm(&warm_root, &rels));
    let mut total = 0;
    let mut by_mod: BTreeMap<String, (Vec<String>, usize)> = BTreeMap::new();
    let mut unmatched = Vec::new();
    let level = level_name(root);
    let rows: Vec<Value> = files
        .iter()
        .map(|f| {
            let (changed, settings, error) = sums.get(&f.rel).map_or((0, 0, None), |s| (s.changed, s.settings, s.error.clone()));
            total += settings;
            match &f.owner {
                Some(k) => {
                    let m = by_mod.entry(k.clone()).or_default();
                    m.0.push(f.rel.clone());
                    m.1 += changed;
                }
                None => unmatched.push(f.rel.clone()),
            }
            json!({
                "rel": f.rel, "format": format_name(f.format), "side": format!("{:?}", f.side).to_lowercase(), "side_label": f.side.label(),
                "size": f.size, "owner": f.owner, "changed": changed, "settings": settings, "error": error,
                "live": live_of(root, &f.rel, &level).map(|(label, used)| json!({ "label": label, "used": used })),
            })
        })
        .collect();
    let mods: Vec<Value> = owners
        .mods()
        .iter()
        .filter_map(|m| {
            let (files, changed) = by_mod.get(&m.key)?;
            let icon = icons.get(&m.key).map(|i| crate::mods_api::icon_url(i)).unwrap_or_default();
            let name = if m.name.is_empty() { m.key.trim_end_matches(".jar").to_string() } else { m.name.clone() };
            Some(json!({ "key": m.key, "name": name, "icon": icon, "files": files, "changed": changed }))
        })
        .collect();
    json!({ "mods": mods, "files": rows, "unmatched": unmatched, "settings": total })
}

/// The server's world folder, named in server.properties (`world` by default).
pub(crate) fn level_name(root: &std::path::Path) -> String {
    std::fs::read_to_string(root.join("server.properties"))
        .ok()
        .and_then(|t| t.lines().find_map(|l| l.strip_prefix("level-name=").map(|s| s.trim().to_string())))
        .filter(|l| !l.is_empty() && hopper_core::configdiff::safe_rel(l))
        .unwrap_or_else(|| "world".into())
}

/// Which copy of a server config NeoForge reads, as the file's chip says
/// it: the world's `serverconfig/` copy when there is one, else `config/`;
/// `defaultconfigs/` only fills in a missing world copy and is never read
/// live. `None` for files that aren't server configs. (NeoForge 21.1,
/// ConfigTracker; the same rule as configcompare's.)
fn live_of(root: &std::path::Path, rel: &str, level: &str) -> Option<(&'static str, bool)> {
    if configindex::side_of(rel) != configindex::Side::Server {
        return None;
    }
    // Another world's copy: not what this server reads.
    if rel.contains("/serverconfig/") && !rel.starts_with(&format!("{level}/serverconfig/")) {
        return Some(("Another world's copy", false));
    }
    let world = format!("{level}/serverconfig/{}", configowner::inner(rel));
    let overridden = rel.starts_with("config/") && crate::files::resolve(root, &world).is_ok_and(|p| p.is_file());
    // The same words as desktop (hopper-core's one rule).
    let label = hopper_core::configcompare::live_label(rel, overridden)?;
    Some((label, matches!(label, "In use" | "In use for this world")))
}

/// `{ hits: [{ rel, owner, entry }] }`: settings whose name or key (and with
/// `desc=1` description) holds every word of `q`.
async fn search(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let e = server(&app, &headers, &method, &id)?;
    let query = q.get("q").cloned().unwrap_or_default();
    let desc = q.get("desc").is_some_and(|d| d == "1" || d == "true");
    let limit = q.get("limit").and_then(|l| l.parse().ok()).unwrap_or(200usize).clamp(1, 2000);
    let v = blocking(move || {
        let root = &e.root;
        let (refs, _) = mod_refs(root);
        let owners = Owners::new(refs);
        let files = files_of(root, &owners);
        let owner_of: HashMap<&str, Option<&String>> = files.iter().map(|f| (f.rel.as_str(), f.owner.as_ref())).collect();
        let rels: Vec<String> = files.iter().map(|f| f.rel.clone()).collect();
        let hits: Vec<Value> = configindex::search(root, &rels, &query, desc, limit)
            .iter()
            .map(|h| json!({ "rel": h.rel, "owner": owner_of.get(h.rel.as_str()).copied().flatten(), "entry": entry_json(&h.entry) }))
            .collect();
        json!({ "hits": hits })
    })
    .await?;
    Ok(Json(v))
}

// ---- parse and batch ---------------------------------------------------------------------

#[derive(Deserialize)]
struct ParseBody {
    rels: Vec<String>,
}

/// `{ files: { rel: { ok, text, format, side, side_label, sections } | { ok:
/// false, error } } }` for each file asked for.
async fn parse(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<ParseBody>) -> ApiResult<Json<Value>> {
    let e = server(&app, &headers, &method, &id)?;
    if b.rels.len() > 500 {
        return Err(bad("Ask for fewer files at once."));
    }
    let v = blocking(move || {
        let mut out = serde_json::Map::new();
        for rel in b.rels {
            out.insert(rel.clone(), parse_one(&e.root, &rel));
        }
        json!({ "files": out })
    })
    .await?;
    Ok(Json(v))
}

pub(crate) fn parse_one(root: &std::path::Path, rel: &str) -> Value {
    if let Err(error) = readable(root, rel) {
        return json!({ "ok": false, "error": error });
    }
    let side = configindex::side_of(rel);
    match &*configindex::read(root, rel) {
        Ok((text, sections)) => json!({
            "ok": true, "text": if has_secrets(sections) { mask_text(Format::of(rel).unwrap_or(Format::Toml), text, sections) } else { text.clone() }, "format": format_name(Format::of(rel)),
            "side": format!("{side:?}").to_lowercase(), "side_label": side.label(), "sections": sections_json(sections),
        }),
        Err(error) => json!({ "ok": false, "error": error }),
    }
}

#[derive(Deserialize)]
pub(crate) struct BatchEdit {
    #[serde(default)]
    path: Vec<String>,
    key: String,
    /// As typed (`true`, `8`, `SAFE`, `["a", "b"]`).
    value: String,
}

#[derive(Deserialize)]
pub(crate) struct BatchBody {
    rel: String,
    /// The text the edits were made on. Optional: without it, or when the
    /// file holds secrets (the page only ever had a masked copy), the edits
    /// go onto the panel's own copy.
    #[serde(default)]
    base: Option<String>,
    edits: Vec<BatchEdit>,
}

/// Saves several settings in one file: each value read as its setting's
/// kind, all checked together, then written with the old copy in
/// `.hopper-backups`. `{ ok, text, sections, reapplied, backup }`, or `{ ok:
/// false, errors: [{ path, key, error }] }` with nothing written.
async fn batch(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>, Json(b): Json<BatchBody>) -> ApiResult<Json<Value>> {
    let e = server(&app, &headers, &method, &id)?;
    let v = blocking(move || batch_save(&e.root, b)).await?;
    Ok(Json(v))
}

fn refused(path: &[String], key: &str, error: impl Into<String>) -> Value {
    json!({ "path": path, "key": key, "error": error.into() })
}

fn failed(errors: Vec<Value>) -> Value {
    json!({ "ok": false, "errors": errors })
}

pub(crate) fn batch_save(root: &std::path::Path, b: BatchBody) -> Value {
    let rel = b.rel.replace('\\', "/");
    if let Err(e) = readable(root, &rel) {
        return failed(vec![refused(&[], "", e)]);
    }
    let Some(f) = Format::of(&rel) else { return failed(vec![refused(&[], "", "Hopper can't read this kind of file as settings.")]) };
    let disk = match std::fs::read_to_string(root.join(&rel)) {
        Ok(t) => t,
        Err(e) => return failed(vec![refused(&[], "", format!("Couldn't read it: {e}"))]),
    };
    // Kinds from the file as it is now: the edits go onto that copy.
    let sections = match configedit::parse_as(f, &disk) {
        Ok(s) => s,
        Err(e) => return failed(vec![refused(&[], "", e)]),
    };
    let mut errors = Vec::new();
    let mut edits = Vec::new();
    for x in &b.edits {
        let Some(entry) = sections.iter().flat_map(|s| &s.entries).find(|e| e.path == x.path && e.key == x.key) else {
            errors.push(refused(&x.path, &x.key, format!("{} isn't in the file any more.", x.key)));
            continue;
        };
        // The mask came back: the secret wasn't touched.
        if masked(entry) && x.value == MASK {
            continue;
        }
        match entry.value.parse_like(&x.value) {
            Ok(value) => edits.push(Edit { path: x.path.clone(), key: x.key.clone(), value }),
            Err(e) => errors.push(refused(&x.path, &x.key, e)),
        }
    }
    if !errors.is_empty() {
        return failed(errors);
    }
    if let Err(bad) = configedit::apply(f, &disk, &edits) {
        return failed(bad.iter().map(|r| refused(&r.path, &r.key, r.error.clone())).collect());
    }
    let base = match b.base {
        Some(t) if !has_secrets(&sections) => t,
        _ => disk.clone(),
    };
    let started = now_secs();
    match configedit::save_edits(root, &rel, &base, &edits) {
        Ok(saved) => {
            let after = configedit::parse_as(f, &saved.text).unwrap_or_default();
            let text = if has_secrets(&after) { mask_text(f, &saved.text, &after) } else { saved.text.clone() };
            let backup = backup_of(root, &rel, started);
            json!({
                "ok": true, "text": text, "sections": sections_json(&after), "reapplied": saved.reapplied,
                "backup": backup.as_ref().map(|b| format!("The old copy is in {b}. Backups can put it back.")),
                "backup_path": backup,
            })
        }
        Err(e) => failed(vec![refused(&[], "", format!("{e:#}"))]),
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Where the save put the old copy of `rel`: the newest `.hopper-backups`
/// set since `since` holding a file of that name.
fn backup_of(root: &std::path::Path, rel: &str, since: u64) -> Option<String> {
    let name = rel.rsplit('/').next()?;
    let dir = root.join(hopper_core::sync::BACKUP_DIR);
    let mut sets: Vec<u64> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u64>().ok())
        .filter(|s| *s >= since)
        .collect();
    sets.sort_unstable();
    for s in sets.into_iter().rev() {
        let set = dir.join(s.to_string());
        let found = std::fs::read_dir(&set).ok()?.flatten().filter_map(|e| e.file_name().to_str().map(str::to_string)).find(|n| n == name || n.ends_with(&format!("-{name}")));
        if let Some(n) = found {
            return Some(format!("{}/{s}/{n}", hopper_core::sync::BACKUP_DIR));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const CFG: &str = "[general]\n\t#How far it reaches\n\t#Range: 1 ~ 64\n\t#Default: 8\n\treach = 8\n\t#Allowed Values: FAST, SAFE\n\tmode = \"SAFE\"\n\tbotToken = \"abc123\"\n";

    fn scratch(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("hopperd-configs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::write(root.join("config/x-common.toml"), CFG).unwrap();
        root
    }

    fn body(edits: &[(&str, &str)]) -> BatchBody {
        BatchBody {
            rel: "config/x-common.toml".into(),
            base: Some(CFG.into()),
            edits: edits.iter().map(|(k, v)| BatchEdit { path: vec!["general".into()], key: k.to_string(), value: v.to_string() }).collect(),
        }
    }

    #[test]
    fn batch_checks_everything_before_writing() {
        let root = scratch("check");
        // One bad value (out of range) and one of the wrong kind: nothing written.
        let r = batch_save(&root, body(&[("reach", "99"), ("mode", "FAST"), ("nope", "1")]));
        assert_eq!(r["ok"], false);
        let errs = r["errors"].as_array().unwrap();
        assert_eq!(errs.len(), 1, "{r}");
        assert_eq!(errs[0]["key"], "nope");
        let r = batch_save(&root, body(&[("reach", "99"), ("mode", "FAST")]));
        assert_eq!(r["errors"][0]["error"], "Between 1 and 64.");
        let r = batch_save(&root, body(&[("reach", "x")]));
        assert_eq!(r["errors"][0]["error"], "Type a whole number.");
        assert_eq!(std::fs::read_to_string(root.join("config/x-common.toml")).unwrap(), CFG);
        assert!(!root.join(".hopper-backups").exists());
        // Outside the config folders, or above the server folder.
        assert_eq!(batch_save(&root, BatchBody { rel: "../x.toml".into(), ..body(&[]) })["ok"], false);
        assert_eq!(batch_save(&root, BatchBody { rel: "server.properties".into(), ..body(&[]) })["ok"], false);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn batch_saves_with_a_backup() {
        let root = scratch("save");
        let r = batch_save(&root, body(&[("reach", "12"), ("mode", "FAST")]));
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(r["reapplied"], false);
        let disk = std::fs::read_to_string(root.join("config/x-common.toml")).unwrap();
        assert!(disk.contains("reach = 12") && disk.contains("mode = \"FAST\"") && disk.contains("#How far it reaches"));
        let backup = r["backup_path"].as_str().unwrap().to_string();
        assert!(backup.starts_with(".hopper-backups/"), "{backup}");
        assert_eq!(std::fs::read_to_string(root.join(&backup)).unwrap(), CFG);
        assert!(r["backup"].as_str().unwrap().contains(&backup));
        // A file with a secret always takes edits onto the panel's own copy.
        let r = batch_save(&root, body(&[("reach", "20")]));
        assert_eq!(r["reapplied"], false);
        // Without secrets, a base older than the disk: the edit goes onto the newer copy.
        let plain = CFG.replace("\tbotToken = \"abc123\"\n", "");
        std::fs::write(root.join("config/x-common.toml"), plain.replace("reach = 8", "reach = 20").replace("\"SAFE\"", "\"FAST\"")).unwrap();
        let r = batch_save(&root, BatchBody { base: Some(plain), ..body(&[("reach", "30")]) });
        assert_eq!(r["reapplied"], true, "{r}");
        let disk = std::fs::read_to_string(root.join("config/x-common.toml")).unwrap();
        assert!(disk.contains("reach = 30") && disk.contains("mode = \"FAST\""));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn secrets_are_masked_and_kept() {
        let root = scratch("mask");
        let p = parse_one(&root, "config/x-common.toml");
        let entries = p["sections"][0]["entries"].as_array().unwrap();
        let tok = entries.iter().find(|e| e["key"] == "botToken").unwrap();
        assert_eq!(tok["value"], MASK);
        assert_eq!(tok["masked"], true);
        let reach = entries.iter().find(|e| e["key"] == "reach").unwrap();
        assert_eq!((reach["kind"].as_str(), reach["value"].as_str(), reach["is_default"].as_bool()), (Some("int"), Some("8"), Some(true)));
        assert_eq!(reach["slider"], json!([1.0, 64.0]));
        assert_eq!(entries.iter().find(|e| e["key"] == "mode").unwrap()["kind"], "choice");
        // No secret bytes anywhere in the reply, the text included.
        let all = p.to_string();
        assert!(!all.contains("abc123"), "{all}");
        let shown = p["text"].as_str().unwrap();
        assert!(shown.contains(&format!("botToken = \"{MASK}\"")) && shown.contains("reach = 8"));
        // The page's masked text as the base, the mask sent back: the token stays.
        let r = batch_save(&root, BatchBody { base: Some(shown.to_string()), ..body(&[("botToken", MASK), ("reach", "9")]) });
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(r["reapplied"], false);
        assert!(!r.to_string().contains("abc123"));
        let disk = std::fs::read_to_string(root.join("config/x-common.toml")).unwrap();
        assert!(disk.contains("botToken = \"abc123\"") && disk.contains("reach = 9"), "{disk}");
        // No base at all; a new value replaces the token.
        let r = batch_save(&root, BatchBody { base: None, ..body(&[("botToken", "new-token-value")]) });
        assert_eq!(r["ok"], true, "{r}");
        assert!(!r.to_string().contains("new-token-value"));
        assert!(std::fs::read_to_string(root.join("config/x-common.toml")).unwrap().contains("botToken = \"new-token-value\""));
        assert!(secret("discord_webhook_url") && secret("apiKey") && !secret("maxBeltLength"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn which_server_copy_is_live() {
        let root = scratch("live");
        let w = |rel: &str| {
            std::fs::create_dir_all(root.join(rel).parent().unwrap()).unwrap();
            std::fs::write(root.join(rel), "a = 1
").unwrap();
        };
        std::fs::write(root.join("server.properties"), "level-name=T
").unwrap();
        w("config/a-server.toml");
        w("config/b-server.toml");
        w("T/serverconfig/a-server.toml");
        w("defaultconfigs/a-server.toml");
        let l = |rel: &str| live_of(&root, rel, &level_name(&root));
        assert_eq!(l("T/serverconfig/a-server.toml"), Some(("In use for this world", true)));
        assert_eq!(l("config/a-server.toml"), Some(("Not used: the world's copy wins", false)));
        assert_eq!(l("config/b-server.toml"), Some(("In use", true)));
        assert_eq!(l("defaultconfigs/a-server.toml"), Some(("Only fills in a missing file", false)));
        assert_eq!(l("config/x-common.toml"), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn index_groups_files() {
        let root = scratch("index");
        std::fs::write(root.join("config/async.toml"), "a = 1\n").unwrap();
        let v = build_index(&root);
        assert_eq!(v["files"].as_array().unwrap().len(), 2);
        // No jars here: both are unmatched.
        assert_eq!(v["unmatched"].as_array().unwrap().len(), 2);
        let x = v["files"].as_array().unwrap().iter().find(|f| f["rel"] == "config/x-common.toml").unwrap().clone();
        assert_eq!((x["side_label"].as_str(), x["format"].as_str(), x["changed"].as_u64()), (Some("Both"), Some("toml"), Some(0)));
        assert_eq!(v["settings"], 4);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Times the Configs page on a real server folder (read only):
    /// `HOPPERD_BENCH_ROOT=<folder> cargo test -p hopperd configs_timing -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn configs_timing() {
        let Some(root) = std::env::var_os("HOPPERD_BENCH_ROOT").map(std::path::PathBuf::from) else { return };
        for run in ["first", "again"] {
            let t = std::time::Instant::now();
            let v = build_index(&root);
            println!("{run}: index {:?}, {} files, {} settings", t.elapsed(), v["files"].as_array().map_or(0, Vec::len), v["settings"]);
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
        for q in ["spawn rate", "max"] {
            // What the search route does on each query.
            let t = std::time::Instant::now();
            let (refs, _) = mod_refs(&root);
            let owners = Owners::new(refs);
            let rels: Vec<String> = files_of(&root, &owners).iter().map(|f| f.rel.clone()).collect();
            let hits = configindex::search(&root, &rels, q, false, 200);
            println!("search {q:?}: {:?}, {} hits", t.elapsed(), hits.len());
        }
    }
}
