//! Routes added to bring the panel's browse area closer to Hopper desktop:
//! what installing a mod would also download (desktop's install card and
//! "+N deps"), a project's dependencies (its Dependencies tab) and
//! everything its author made (the "by …" link), and the project a
//! description's link points at.

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
        .route("/api/v1/browse/link", get(link))
        .route("/api/v1/browse/comments", get(comments))
        .route("/api/v1/browse/issues", get(issues))
        .route("/api/v1/browse/issues/repo", get(issues_repo))
}

/// The GitHub repo whose issues project `q.project` shows (its tracker
/// link, or a GitHub mod's own repo), worked out here from the project so
/// the browser never names a repo: the panel only ever asks api.github.com
/// about a repo a project points at.
fn repo_of_project(project: u64) -> anyhow::Result<Option<String>> {
    Ok(hopper_core::ghissues::repo_of_mod(&source::get_mod(project)?))
}

/// Whether a project has a GitHub Issues tab: `{repo}` or `{repo: null}`.
async fn issues_repo(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Query(q): Q) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let project = project_of(&q)?;
    let repo = blocking(move || repo_of_project(project)).await?;
    Ok(Json(json!({ "repo": repo })))
}

fn issue_json(i: &hopper_core::ghissues::Issue) -> Value {
    json!({
        "number": i.number,
        "title": i.title,
        "url": i.url,
        "open": i.open,
        "labels": i.labels.iter().map(|l| json!({ "name": l.name, "color": l.color })).collect::<Vec<_>>(),
        "comments": i.comments,
        // Seconds, as the browse routes' other dates.
        "created": i.created,
        "closed": i.closed,
        "author": i.author,
    })
}

/// One page (from 1) of a project's GitHub issues (desktop's Issues tab):
/// `state` open / closed / all, `q` search words. GitHub's limit comes
/// back as `error: "busy"`, any other failure as `"failed"`, so the page
/// can say which.
async fn issues(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Query(q): Q) -> ApiResult<Json<Value>> {
    use hopper_core::ghissues::{self, Error, Filter};
    require(&app, &headers, &method)?;
    let project = project_of(&q)?;
    let filter = match arg(&q, "state").as_str() {
        "" => Filter::Open,
        s => Filter::parse(s).ok_or_else(|| bad("Open, closed or all?"))?,
    };
    // A search is a few words; GitHub takes no more than 256 characters.
    let words: String = arg(&q, "q").chars().take(200).collect();
    let page: u32 = arg(&q, "page").parse().unwrap_or(1).clamp(1, 50);
    let v = blocking(move || {
        let Some(repo) = repo_of_project(project)? else {
            return Ok(json!({ "repo": null, "issues": [], "more": false, "error": "failed" }));
        };
        let site = ghissues::site_url(&repo, filter, &words);
        Ok(match ghissues::page(&repo, filter, &words, page) {
            Ok(p) => json!({
                "repo": repo,
                "site": site,
                "issues": p.issues.iter().map(issue_json).collect::<Vec<_>>(),
                "more": p.more,
                "total": p.total,
            }),
            Err(e) => json!({
                "repo": repo,
                "site": site,
                "issues": [],
                "more": false,
                "error": if e == Error::Busy { "busy" } else { "failed" },
            }),
        })
    })
    .await?;
    Ok(Json(v))
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
        let kept: Vec<Value> = p.kept.iter().map(|k| json!({ "id": k.id.to_string(), "source": if hopper_core::modrinth::is_mr(k.id) { "modrinth" } else { "curseforge" }, "name": k.name, "icon": k.icon, "on": k.on })).collect();
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

fn comment_json(c: &hopper_core::cfcomments::Comment) -> Value {
    json!({
        "id": c.id.to_string(),
        "author": c.author,
        "avatar": c.avatar,
        // Seconds, as the browse routes' other dates.
        "date": c.posted,
        "text": c.text,
        "pinned": c.pinned,
        "removed": c.removed,
        "replies": c.replies.iter().map(comment_json).collect::<Vec<_>>(),
    })
}

/// One page of a CurseForge project's comments (desktop's Comments tab),
/// fetched here so the browser never talks to CurseForge's website. Only a
/// CurseForge project's number is taken; Modrinth has no comments.
async fn comments(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Query(q): Q) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let project = project_of(&q)?;
    if hopper_core::modrinth::is_mr(project) || hopper_core::sources::is_src(project) {
        return Err(bad("Only CurseForge projects have comments."));
    }
    let page: u32 = arg(&q, "page").parse().unwrap_or(0).min(10_000);
    let v = blocking(move || {
        let p = hopper_core::cfcomments::page(project, page)?;
        Ok(json!({
            "comments": p.comments.iter().map(comment_json).collect::<Vec<_>>(),
            "page": p.index,
            "total": p.total,
            "more": p.more(),
        }))
    })
    .await?;
    Ok(Json(v))
}

/// The project a CurseForge or Modrinth page link points at, as a card, so
/// a description's link opens it in the panel; `null` for any other link.
async fn link(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Query(q): Q) -> ApiResult<Json<Value>> {
    require(&app, &headers, &method)?;
    let Some(l) = source::project_link(&arg(&q, "url")) else {
        return Ok(Json(json!({ "project": null })));
    };
    if !matches!(l, source::ProjectLink::Mr(_)) && !has_cf_key() {
        return Ok(Json(json!({ "project": null })));
    }
    let v = blocking(move || {
        let m = source::get_mod(source::project_from_link(&l)?)?;
        Ok(json!({ "project": mini(&m) }))
    })
    .await?;
    Ok(Json(v))
}
