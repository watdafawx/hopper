//! Routes added to bring the panel's browse area closer to Hopper desktop:
//! what installing a mod would also download (desktop's install card and
//! "+N deps"), a project's dependencies (its Dependencies tab) and
//! everything its author made (the "by …" link).

use crate::App;
use crate::api::{ApiResult, bad, require};
use crate::browse_api::{Have, Q, arg, blocking, card_of, has_cf_key};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, Method};
use axum::routing::get;
use axum::{Json, Router};
use hopper_core::cf::{self, Loader};
use hopper_core::{browse, install, source};
use serde_json::{Value, json};
use std::sync::Arc;

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/browse/plan", get(plan))
        .route("/api/v1/browse/deps", get(deps))
        .route("/api/v1/browse/author", get(author))
}

fn project_of(q: &std::collections::HashMap<String, String>) -> ApiResult<u64> {
    arg(q, "project").parse().ok().filter(|n| *n != 0).ok_or_else(|| bad("Which project?"))
}

/// A short card for a project in a list (a dependency, an author's project).
fn mini(m: &cf::Mod) -> Value {
    card_of(m, None, m.class_id.unwrap_or(cf::CLASS_MODS))
}

/// What installing a mod into a server adds: the file it picks (the newest
/// that fits, or `file`), each required mod it also downloads (and which
/// mod asks for it), what the server has already, and anything left out.
async fn plan(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Query(q): Q) -> ApiResult<Json<Value>> {
    let who = require(&app, &headers, &method)?;
    let project = project_of(&q)?;
    let file: Option<u64> = arg(&q, "file").parse().ok().filter(|n| *n != 0);
    let have = Have::of(&app, &who, &q)?.ok_or_else(|| bad("Which server?"))?;
    let v = blocking(move || {
        let inst = have.inst;
        let m = source::get_mod(project)?;
        let f = match file {
            Some(f) => source::file(m.id, f)?,
            None => install::best_file(m.id, Some(m.class_id.unwrap_or(cf::CLASS_MODS)), &inst)?
                .ok_or_else(|| anyhow::anyhow!("No file for this version."))?,
        };
        let p = install::plan(&inst, &m, &f, None)?;
        let extra: Vec<Value> = p
            .extra()
            .iter()
            .map(|(d, df)| {
                let mut c = mini(d);
                c["file"] = json!(df.display_name);
                c["size"] = json!(df.file_length);
                c["by"] = json!(p.by.get(&d.id).filter(|b| **b != m.name));
                c
            })
            .collect();
        let total: u64 = p.steps.iter().map(|(_, f)| f.file_length).sum();
        let kept: Vec<Value> = p.kept.iter().map(|(n, on)| json!({ "name": n, "on": on })).collect();
        let notes: Vec<&String> = p.notes.iter().chain(&p.suggest).collect();
        Ok(json!({
            "file": { "id": f.id.to_string(), "name": f.display_name, "size": f.file_length },
            "extra": extra, "total": total, "kept": kept, "notes": notes,
        }))
    })
    .await?;
    Ok(Json(v))
}

/// A project's required, optional and incompatible projects (required
/// first), from its newest release for `mc` / `loader`, like desktop's
/// Dependencies tab (and CurseForge's Relations). With `into`, each says
/// whether that server has it.
async fn deps(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Query(q): Q) -> ApiResult<Json<Value>> {
    let who = require(&app, &headers, &method)?;
    let project = project_of(&q)?;
    let mc = arg(&q, "mc");
    let loader = match arg(&q, "loader").as_str() {
        "" => Loader::Any,
        l => Loader::from_loader_id(l),
    };
    let have = Have::of(&app, &who, &q)?;
    let v = blocking(move || {
        let files = source::files(project, &mc, loader)?;
        let list = files.iter().find(|f| f.release_type == 1).or(files.first()).map(browse::dependencies).unwrap_or_default();
        Ok(list
            .into_iter()
            .map(|(d, rel)| {
                let mut c = mini(&d);
                c["relation"] = json!(match rel {
                    cf::DEP_REQUIRED => "required",
                    cf::DEP_INCOMPATIBLE => "incompatible",
                    _ => "optional",
                });
                if let Some(h) = &have {
                    c["installed"] = h.of_mod(&d);
                }
                c
            })
            .collect::<Vec<_>>())
    })
    .await?;
    Ok(Json(json!({ "deps": v })))
}

/// Everything the author of a project made, most downloaded first.
async fn author(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Query(q): Q) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let project = project_of(&q)?;
    if !hopper_core::modrinth::is_mr(project) && !has_cf_key() {
        return Err(bad("CurseForge needs an API key: add one in Access & devices."));
    }
    let v = blocking(move || {
        let m = source::get_mod(project)?;
        let list = source::author_projects(&m)?;
        Ok(json!({
            "name": m.author(),
            "url": m.authors.first().map(|a| a.url.clone()).unwrap_or_default(),
            "projects": list.iter().map(mini).collect::<Vec<_>>(),
        }))
    })
    .await?;
    Ok(Json(v))
}
