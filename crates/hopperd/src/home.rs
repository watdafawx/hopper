//! The panel folder: one folder, picked at setup, that holds every server
//! (`servers\` by default) and the panel's own data (`.hopper-server\`:
//! logins, certificates, uploads, update checks). The panel never lists
//! or imports anything outside it.
//!
//! The data folder hopperd starts with (`--data`, or the system's app data
//! folder) then only keeps a pointer to the panel folder (and the service
//! log). The panel's data moves in at the start after the folder is
//! picked, and Move renames the whole folder at the start after that, so
//! nothing has the files open.

use crate::App;
use crate::api::{ApiResult, bad, require_owner};
use crate::config::{Config, FILE};
use axum::extract::State;
use axum::http::{HeaderMap, Method};
use axum::routing::{post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The panel's own data, inside the panel folder.
pub const PANEL_DIR: &str = ".hopper-server";
/// Where new servers go, inside the panel folder.
pub const SERVERS_DIR: &str = "servers";
/// Left where they are: the service writes its log there before it knows
/// the panel folder.
const STAYS: &[&str] = &[FILE, "hopperd.log"];

fn norm(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let s = s.trim_end_matches('/').to_string();
    if cfg!(windows) { s.to_lowercase() } else { s }
}

/// Whether `p` is `home` or inside it (by path, not following links).
pub fn inside(home: &Path, p: &Path) -> bool {
    let (h, p) = (norm(home), norm(p));
    p == h || p.starts_with(&format!("{h}/"))
}

/// Refuses paths outside the panel folder (once there is one).
pub fn check(app: &App, p: &Path) -> ApiResult<()> {
    match app.cfg.read().unwrap().home.clone() {
        Some(h) if !inside(&h, p) => Err(bad(format!("{} is outside the panel folder ({}).", p.display(), h.display()))),
        _ => Ok(()),
    }
}

pub fn home(app: &App) -> Option<PathBuf> {
    app.cfg.read().unwrap().home.clone()
}

fn copy_all(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let to = dst.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_all(&e.path(), &to)?;
        } else {
            std::fs::copy(e.path(), &to)?;
        }
    }
    Ok(())
}

fn rebase(p: &Path, from: &Path, to: &Path) -> PathBuf {
    if inside(from, p) {
        let rest = p.to_string_lossy()[from.to_string_lossy().trim_end_matches(['\\', '/']).len()..].trim_start_matches(['\\', '/']).to_string();
        if rest.is_empty() { to.to_path_buf() } else { to.join(rest) }
    } else {
        p.to_path_buf()
    }
}

/// Run at start, before anything opens the panel's files: applies a Move,
/// moves the panel's data into the panel folder the first time, and
/// returns the data folder to use (with its config).
pub fn settle(boot: &Path, mut cfg: Config) -> anyhow::Result<(PathBuf, Config)> {
    let Some(mut home) = cfg.home.clone() else { return Ok((boot.to_path_buf(), cfg)) };
    if cfg.home.is_some() && norm(boot) == norm(&home.join(PANEL_DIR)) {
        return Ok((boot.to_path_buf(), cfg));
    }
    // A Move asked for last time: rename the folder now, while nothing
    // has its files open.
    if let Some(to) = cfg.move_to.take() {
        let data = home.join(PANEL_DIR);
        let mut inner = Config::load_or_create(&data).unwrap_or_else(|_| cfg.clone());
        match std::fs::rename(&home, &to) {
            Ok(()) => {
                log::info!("moved the panel folder {} to {}", home.display(), to.display());
                for c in [&mut cfg, &mut inner] {
                    c.servers_root = c.servers_root.as_deref().map(|r| rebase(r, &home, &to));
                    c.servers = c.servers.iter().map(|s| rebase(s, &home, &to)).collect();
                    c.hidden = c.hidden.iter().map(|s| rebase(s, &home, &to)).collect();
                    c.home = Some(to.clone());
                    c.move_to = None;
                }
                home = to;
                inner.save(&home.join(PANEL_DIR))?;
            }
            Err(e) => log::error!("couldn't move the panel folder to {}: {e} (it stays at {})", to.display(), home.display()),
        }
        pointer(boot, &home)?;
    }
    let data = home.join(PANEL_DIR);
    if not_ours(boot, &data) {
        // Another panel's data (its folder picked by mistake, or --home):
        // never run as that panel. Stay where we are, without its folder.
        log::error!(
            "{} holds another Hopper Server's data; not using it as this panel's folder. Pick another panel folder in the panel.",
            home.display()
        );
        if cfg.servers_root.as_deref().is_some_and(|r| inside(&home, r)) {
            cfg.servers_root = None;
        }
        cfg.home = None;
        cfg.save(boot)?;
        return Ok((boot.to_path_buf(), cfg));
    }
    if !data.join(FILE).is_file() {
        // First start with a panel folder: the data moves in.
        log::info!("moving the panel's data from {} to {}", boot.display(), data.display());
        std::fs::create_dir_all(&data)?;
        let mut moved = Vec::new();
        for e in std::fs::read_dir(boot)?.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if STAYS.contains(&name.as_str()) {
                continue;
            }
            let to = data.join(&name);
            let r = if e.path().is_dir() { copy_all(&e.path(), &to) } else { std::fs::copy(e.path(), &to).map(|_| ()).map_err(Into::into) };
            if let Err(err) = r {
                log::error!("couldn't move {name} into the panel folder: {err:#}; staying in {}", boot.display());
                let _ = std::fs::remove_dir_all(&data);
                return Ok((boot.to_path_buf(), cfg));
            }
            moved.push(e.path());
        }
        if cfg.servers_root.is_none() {
            cfg.servers_root = Some(if has_servers(&home) { home.clone() } else { home.join(SERVERS_DIR) });
        }
        cfg.save(&data)?;
        pointer(boot, &home)?;
        // Logins and certificates don't stay behind in the old place.
        for p in moved {
            let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
        }
    }
    let mut inner = Config::load_or_create(&data)?;
    inner.home = Some(home.clone());
    if let Some(r) = &inner.servers_root {
        let _ = std::fs::create_dir_all(r);
    }
    Ok((data, inner))
}

/// Another panel's data in `dir`'s `.hopper-server` (not `ours`): its port.
fn panel_in(dir: &Path, ours: &Path) -> Option<u16> {
    let data = dir.join(PANEL_DIR);
    if norm(&data) == norm(ours) || !(data.join(FILE).is_file() || data.join("auth.json").is_file()) {
        return None;
    }
    let cfg = std::fs::read_to_string(data.join(FILE)).ok().and_then(|t| toml::from_str::<Config>(&t).ok());
    Some(cfg.map_or(crate::config::DEFAULT_PORT, |c| c.port))
}

/// Another panel's folder that `dir` is, is inside, or holds (up to three
/// folders down): where it is and its port. Two panels must never share
/// files.
pub fn foreign_panel(dir: &Path, ours: &Path) -> Option<(PathBuf, u16)> {
    for a in dir.ancestors() {
        if let Some(port) = panel_in(a, ours) {
            return Some((a.to_path_buf(), port));
        }
    }
    let mut level = vec![dir.to_path_buf()];
    let mut seen = 0;
    for _ in 0..3 {
        let mut next = Vec::new();
        for d in &level {
            for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
                let name = e.file_name();
                if name.to_string_lossy().starts_with('.') || !e.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                seen += 1;
                if seen > 5000 {
                    return None;
                }
                if let Some(port) = panel_in(&e.path(), ours) {
                    return Some((e.path(), port));
                }
                next.push(e.path());
            }
        }
        level = next;
    }
    None
}

/// `data` already holds a panel's logins, and they aren't the ones still
/// waiting in `boot` to move in: another panel's.
fn not_ours(boot: &Path, data: &Path) -> bool {
    let Ok(mine) = std::fs::read(boot.join("auth.json")) else { return false };
    std::fs::read(data.join("auth.json")).is_ok_and(|theirs| theirs != mine)
}

/// The start-up data folder keeps only where the panel folder is (and a
/// Move asked for).
fn pointer(boot: &Path, home: &Path) -> anyhow::Result<()> {
    write_pointer(boot, home, None)
}

fn write_pointer(boot: &Path, home: &Path, move_to: Option<&Path>) -> anyhow::Result<()> {
    #[derive(serde::Serialize)]
    struct Pointer<'a> {
        home: &'a Path,
        move_to: Option<&'a Path>,
    }
    let text = toml::to_string_pretty(&Pointer { home, move_to })?;
    std::fs::write(boot.join(FILE), format!("# Hopper Server keeps everything in its panel folder.
{text}"))?;
    Ok(())
}

// ---- API ----------------------------------------------------------------------------

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/v1/settings/panel-folder", put(set))
        .route("/api/v1/settings/panel-folder/move", post(move_to))
        .route("/api/v1/panel/restart", post(restart))
}

/// Whether a folder right inside `dir` is a server (has server.properties).
fn has_servers(dir: &Path) -> bool {
    std::fs::read_dir(dir).into_iter().flatten().flatten().any(|e| e.path().join("server.properties").is_file())
}

#[derive(Deserialize)]
struct SetBody {
    path: String,
}

/// Picks the panel folder (once, at setup or on an older panel). Servers
/// go in its `servers` folder, unless the servers folder is in it already;
/// servers elsewhere keep running where they are. The panel's data moves
/// in at the restart that follows.
async fn set(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<SetBody>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    if home(&app).is_some() {
        return Err(bad("The panel folder is set: use Move to put it somewhere else."));
    }
    let dir = crate::absolute(Path::new(b.path.trim()));
    if b.path.trim().is_empty() || dir.parent().is_none() {
        return Err(bad("Pick a folder, not a whole drive."));
    }
    if let Some((other, port)) = foreign_panel(&dir, &app.data_dir) {
        let what = if norm(&other) == norm(&dir) {
            format!("{} is another Hopper Server's panel folder (port {port})", dir.display())
        } else if inside(&other, &dir) {
            format!("{} is inside another Hopper Server's panel folder, {} (port {port})", dir.display(), other.display())
        } else {
            format!("{} holds another Hopper Server's panel folder, {} (port {port})", dir.display(), other.display())
        };
        return Err(bad(format!("{what}. Two panels can't share files: pick a folder outside it.")));
    }
    std::fs::create_dir_all(&dir).map_err(|e| bad(format!("creating {}: {e}", dir.display())))?;
    {
        let mut c = app.cfg.write().unwrap();
        if !c.servers_root.as_deref().is_some_and(|r| inside(&dir, r)) {
            if let Some(old) = c.servers_root.clone() {
                for e in app.servers.list() {
                    if inside(&old, &e.root) && !c.servers.iter().any(|s| norm(s) == norm(&e.root)) {
                        c.servers.push(e.root.clone());
                    }
                }
            }
            // A folder that has servers in it already is the servers
            // folder; else new servers go in its `servers` folder.
            let root = if has_servers(&dir) { dir.clone() } else { dir.join(SERVERS_DIR) };
            std::fs::create_dir_all(&root).map_err(|e| bad(format!("creating {}: {e}", root.display())))?;
            c.servers_root = Some(root);
        }
        c.home = Some(dir.clone());
    }
    let c = app.cfg.read().unwrap().clone();
    c.save(&app.data_dir).map_err(|e| bad(format!("saving settings: {e:#}")))?;
    log::info!("panel folder is {}; the panel's data moves in at the next start", dir.display());
    app.rescan();
    app.refresh.notify_one();
    Ok(Json(json!({ "ok": true, "path": dir, "restart": true })))
}

#[derive(Deserialize)]
struct MoveBody {
    path: String,
}

/// Moves the panel folder (servers included) at the next start. Same
/// drive only: a rename, instant. Every server in it must be stopped.
async fn move_to(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<MoveBody>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let Some(from) = home(&app) else { return Err(bad("Pick a panel folder first.")) };
    let to = crate::absolute(Path::new(b.path.trim()));
    if to.exists() {
        return Err(bad(format!("{} exists already: pick a new folder name.", to.display())));
    }
    if inside(&from, &to) {
        return Err(bad("That's inside the panel folder."));
    }
    let drive = |p: &Path| p.components().next().map(|c| norm(Path::new(c.as_os_str())));
    if cfg!(windows) && drive(&from) != drive(&to) {
        return Err(bad("Move it on the same drive here. To another drive: stop Hopper Server, move the folder yourself, and start it with --home <new folder>."));
    }
    if let Some(e) = app.servers.list().into_iter().find(|e| inside(&from, &e.root) && e.server.state().is_alive()) {
        return Err(bad(format!("Stop {} first: it runs from the panel folder.", e.name)));
    }
    let to_parent = to.parent().map(Path::to_path_buf).ok_or_else(|| bad("Pick a folder."))?;
    std::fs::create_dir_all(&to_parent).map_err(|e| bad(format!("creating {}: {e}", to_parent.display())))?;
    // The start-up folder is read first at the next start: it carries the move.
    write_pointer(&app.boot_dir, &from, Some(&to)).map_err(|e| bad(format!("saving: {e:#}")))?;
    log::info!("the panel folder moves to {} at the next start", to.display());
    Ok(Json(json!({ "ok": true, "restart": true })))
}

/// Restarts the panel: it exits, and the Windows service or systemd starts
/// it again (both restart it when it stops on its own). Servers keep
/// running; the panel finds them again.
async fn restart(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    log::warn!("restarting (asked from the panel)");
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        std::process::exit(75);
    });
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inside_the_panel_folder() {
        let h = Path::new("E:\\Minecraft");
        assert!(inside(h, Path::new("E:\\Minecraft\\servers\\ATM10")));
        assert!(inside(h, Path::new("e:/minecraft/")) == cfg!(windows) || !cfg!(windows));
        assert!(!inside(h, Path::new("E:\\Minecraft2\\x")));
        assert!(!inside(h, Path::new("G:\\Minecraft\\packs\\TNP-LL8")));
        assert_eq!(rebase(Path::new("E:\\Minecraft\\servers\\A"), h, Path::new("E:\\Games\\MC")), PathBuf::from("E:\\Games\\MC").join("servers\\A"));
    }

    #[test]
    fn another_panels_folder_is_never_adopted() {
        let base = std::env::temp_dir().join(format!("hopperd-foreign-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        // The real panel: its data in G:\Minecraft\packs\.hopper-server, port 8791.
        let theirs = base.join("packs");
        Config { port: 8791, home: Some(theirs.clone()), ..Config::default() }.save(&theirs.join(PANEL_DIR)).unwrap();
        std::fs::write(theirs.join(PANEL_DIR).join("auth.json"), r#"{"owner":"theirs"}"#).unwrap();
        // A second panel, set up, picks it.
        let boot = base.join("boot");
        std::fs::create_dir_all(&boot).unwrap();
        std::fs::write(boot.join("auth.json"), r#"{"owner":"mine"}"#).unwrap();
        assert_eq!(foreign_panel(&theirs, &boot), Some((theirs.clone(), 8791)));
        assert_eq!(foreign_panel(&theirs.join("sub"), &boot), Some((theirs.clone(), 8791)), "inside it");
        assert_eq!(foreign_panel(&base, &boot), Some((theirs.clone(), 8791)), "holds it (a parent folder)");
        assert_eq!(foreign_panel(&theirs, &theirs.join(PANEL_DIR)), None, "its own folder");
        std::fs::create_dir_all(base.join("fresh")).unwrap();
        assert_eq!(foreign_panel(&base.join("fresh"), &boot), None);
        // Picked anyway (older panel, --home): start-up refuses it.
        let cfg = Config { port: 8796, home: Some(theirs.clone()), servers_root: Some(theirs.clone()), ..Config::default() };
        cfg.save(&boot).unwrap();
        let (data, c) = settle(&boot, cfg).unwrap();
        assert_eq!((data, c.port, c.home, c.servers_root), (boot.clone(), 8796, None, None));
        assert!(boot.join("auth.json").is_file(), "our logins stay ours");
        assert_eq!(std::fs::read_to_string(theirs.join(PANEL_DIR).join("auth.json")).unwrap(), r#"{"owner":"theirs"}"#);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn data_moves_into_the_panel_folder_once() {
        let base = std::env::temp_dir().join(format!("hopperd-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let boot = base.join("boot");
        std::fs::create_dir_all(boot.join("acme")).unwrap();
        std::fs::write(boot.join("auth.json"), "{}").unwrap();
        std::fs::write(boot.join("acme").join("x"), "y").unwrap();
        let home = base.join("Minecraft");
        let cfg = Config { home: Some(home.clone()), ..Config::default() };
        cfg.save(&boot).unwrap();
        let (data, c) = settle(&boot, cfg).unwrap();
        assert_eq!(data, home.join(PANEL_DIR));
        assert!(data.join("auth.json").is_file() && data.join("acme").join("x").is_file());
        assert!(!boot.join("auth.json").exists(), "secrets don't stay behind");
        assert_eq!(c.servers_root, Some(home.join(SERVERS_DIR)));
        // The pointer leads back to the same place next time.
        let p = Config::load_or_create(&boot).unwrap();
        assert_eq!(p.home, Some(home.clone()));
        let (again, _) = settle(&boot, p).unwrap();
        assert_eq!(again, data);
        let _ = std::fs::remove_dir_all(&base);
    }
}
