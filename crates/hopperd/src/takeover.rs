//! Taking a server over from Crafty (and giving it back). The work is in
//! hopper_core::hosting::crafty; this is the API and the checks that
//! Crafty isn't running the server right now.

use crate::App;
use crate::api::{ApiResult, bad, not_found, require};
use crate::servers::Entry;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method};
use axum::routing::{get, post};
use axum::{Json, Router};
use hopper_core::hosting::crafty::{self, Takeover};
use hopper_core::hosting::Properties;
use serde_json::{Value, json};
use std::sync::Arc;

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/servers/{id}/takeover", get(status).post(take))
        .route("/api/v1/servers/{id}/give-back", post(give_back))
}

/// Why the server looks like it's running somewhere else right now.
pub fn busy_elsewhere(e: &Entry) -> Option<String> {
    let props = Properties::load(&e.root.join("server.properties")).unwrap_or_default();
    let port = props.port();
    if !hopper_core::hosting::port_free(port) {
        return Some(format!("Something is using its port {port}: it's probably running in Crafty. Stop it there first."));
    }
    // Minecraft holds a lock on the world's session.lock while it runs.
    let world = props.get("level-name").filter(|w| !w.trim().is_empty()).unwrap_or("world");
    let lock = e.root.join(world).join("session.lock");
    if let Ok(f) = std::fs::OpenOptions::new().read(true).write(true).open(&lock) {
        match f.try_lock() {
            Ok(()) => {
                let _ = f.unlock();
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Some(format!("Its world ({world}) is open in another program: it's probably running in Crafty. Stop it there first."));
            }
            Err(_) => {}
        }
    }
    None
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(bad)?.map_err(|e| bad(format!("{e:#}")))
}

fn entry(app: &App, headers: &HeaderMap, method: &Method, id: &str) -> ApiResult<Arc<Entry>> {
    require(app, headers, method)?;
    app.servers.get(id).ok_or_else(|| not_found("server"))
}

/// What a takeover would use: Crafty's settings for this server.
async fn status(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    let done = Takeover::load(&e.root);
    let root = e.root.clone();
    let name = e.name.clone();
    // Crafty's settings, and what the takeover would make of them.
    let found = if done.is_none() && e.managed_by == Some("Crafty") {
        Some(
            blocking(move || {
                let c = crafty::entry_for(&root)?;
                let plan = match java_for(&name, &root, &c.command) {
                    Ok((java, notes)) => json!({
                        "minecraft": crafty::minecraft_version(&root, &c.command),
                        "java": java,
                        "java_major": java.as_deref().and_then(hopper_core::java::inspect).map(|j| j.major),
                        "bare_java": crafty::uses_bare_java(&c.command),
                        "memory_mb": memory_mb(&root, &c.command),
                        "notes": notes,
                    }),
                    Err(err) => json!({ "error": format!("{err:#}") }),
                };
                Ok((c, plan))
            })
            .await,
        )
    } else {
        None
    };
    let e2 = e.clone();
    let busy = tokio::task::spawn_blocking(move || busy_elsewhere(&e2)).await.ok().flatten();
    let (crafty, plan, crafty_error) = match found {
        Some(Ok((c, plan))) => (Some(c), Some(plan), None),
        Some(Err(err)) => (None, None, Some(err.0.to_string())),
        None => (None, None, None),
    };
    Ok(Json(json!({
        "managed_by": e.managed_by,
        "taken_over": done,
        "crafty": crafty,
        "plan": plan,
        "crafty_error": crafty_error,
        "busy": busy,
    })))
}

/// The memory the server gets: -Xmx in the command, else user_jvm_args.txt.
fn memory_mb(root: &std::path::Path, command: &str) -> Option<u32> {
    for t in hopper_core::hosting::jvm::tokenize(command) {
        if let Some(v) = t.strip_prefix("-Xmx") {
            let (num, unit) = v.split_at(v.find(|c: char| !c.is_ascii_digit()).unwrap_or(v.len()));
            let n: u32 = num.parse().ok()?;
            return Some(match unit.to_ascii_lowercase().as_str() {
                "g" => n * 1024,
                "k" => n / 1024,
                "" => n / (1024 * 1024),
                _ => n,
            });
        }
    }
    hopper_core::hosting::JvmArgs::load(root).ok().and_then(|j| j.xmx_mb)
}

async fn take(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    if e.managed_by != Some("Crafty") {
        return Err(bad(format!("{} isn't run by Crafty.", e.name)));
    }
    let a = app.clone();
    let t = blocking(move || {
        if let Some(why) = busy_elsewhere(&e) {
            anyhow::bail!(why);
        }
        let c = crafty::entry_for(&e.root)?;
        let (java, mut notes) = java_for(&e.name, &e.root, &c.command)?;
        let t = crafty::take_over(&e.root, &c, java.as_deref())?;
        a.servers.reload(&e.root);
        log::info!("{}: taken over from Crafty (its start command: {}; Java: {:?})", e.name, c.command, t.java_pinned);
        if c.auto_start {
            notes.push("Crafty starts this server with itself: turn that off in Crafty, or remove the server there.".into());
        }
        Ok((t, notes))
    })
    .await?;
    let (t, notes) = t;
    app.router.changed();
    app.refresh.notify_one();
    Ok(Json(json!({ "ok": true, "takeover": t, "notes": notes })))
}

/// The Java to put in place of a plain `java` in Crafty's command (PATH's
/// Java changes when a new one is installed; the server needs the one for
/// its Minecraft version), and notes about the Java it'll use.
fn java_for(name: &str, root: &std::path::Path, command: &str) -> anyhow::Result<(Option<std::path::PathBuf>, Vec<String>)> {
    use hopper_core::java;
    let mut notes = Vec::new();
    let Some(mc) = crafty::minecraft_version(root, command) else {
        notes.push("Couldn't tell its Minecraft version, so it keeps Crafty's Java as is.".into());
        return Ok((None, notes));
    };
    let need = java::recommended_major(&mc);
    if crafty::uses_bare_java(command) {
        let installs = java::detect(std::path::Path::new(""));
        let Some(best) = java::best(&installs, &mc, java::Side::Server) else {
            anyhow::bail!("{name} is Minecraft {mc} and needs Java {need}, which isn't installed. Install Java {need} first.");
        };
        let exe = hopper_core::hosting::script::console_java(&best.install.exe);
        notes.push(format!("Crafty started it with plain \"java\"; it now uses {} (Java {}) for Minecraft {mc}.", exe.display(), best.install.major));
        return Ok((Some(exe), notes));
    }
    // Crafty named a Java: kept, but checked.
    let exe = hopper_core::hosting::jvm::tokenize(command.trim()).into_iter().next().unwrap_or_default();
    let (_, have) = hopper_core::hosting::java_check(&mc, std::path::Path::new(&exe));
    if let Some(have) = have
        && have < need
    {
        notes.push(format!("Its Java ({exe}) is Java {have}; Minecraft {mc} needs Java {need}."));
    }
    Ok((None, notes))
}

async fn give_back(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let e = entry(&app, &headers, &method, &id)?;
    if e.server.state().is_alive() {
        return Err(bad(format!("Stop {} first.", e.name)));
    }
    let a = app.clone();
    blocking(move || {
        crafty::give_back(&e.root)?;
        a.servers.reload(&e.root);
        log::info!("{}: given back to Crafty", e.name);
        Ok(())
    })
    .await?;
    app.refresh.notify_one();
    Ok(Json(json!({ "ok": true })))
}
