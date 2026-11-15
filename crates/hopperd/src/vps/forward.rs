//! A server checking accounts through the relay's Velocity
//! (design/vps-relay.md, B4): "Use the relay" adds the forwarding mod
//! (PCF or FabricProxy-Lite) and its config with the forwarding secret and
//! sets four `server.properties` keys; "Stop using the relay" puts all of
//! it back; "Play at home" puts the keys back and turns the mod off while
//! the relay is down, and "Back on the relay" reverses that. Every file it
//! changes is recorded in the server's History, as one row per action.
//!
//! The logic (which mod, which keys, which steps) is
//! [`hopper_core::hosting::proxy_forward`]; this does the files and keeps,
//! per server, what it changed (`vps-relay-servers.json`, owner-only: it
//! holds the forwarding secret and the mod's old config).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context as _;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use hopper_core::hosting::properties::Properties;
use hopper_core::hosting::proxy_forward::{self as pf, Action, ModStep, Mode, Record};
use hopper_core::panel::PanelAction;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::home::write_private;
use crate::App;
use crate::api::{ApiError, ApiResult, bad, not_found, require_owner};
use crate::servers::Entry;

const FILE: &str = "vps-relay-servers.json";
/// How History names the changes.
const MOD_NAME: &str = "Relay through a VPS";

#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
struct Saved {
    /// Velocity's modern forwarding secret (64 hex). Never in an API answer.
    secret: String,
    servers: BTreeMap<String, Record>,
}

pub struct Forward {
    dir: PathBuf,
    saved: Mutex<Saved>,
}

impl Forward {
    pub fn new(dir: &Path) -> Forward {
        let saved = std::fs::read(dir.join(FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Forward { dir: dir.to_path_buf(), saved: Mutex::new(saved) }
    }

    fn write(&self, s: &Saved) -> anyhow::Result<()> {
        write_private(&self.dir.join(FILE), &serde_json::to_vec_pretty(s)?)
    }

    pub fn mode(&self, id: &str) -> Mode {
        self.saved.lock().unwrap().servers.get(id).map(|r| r.mode).unwrap_or_default()
    }

    pub fn record(&self, id: &str) -> Record {
        self.saved.lock().unwrap().servers.get(id).cloned().unwrap_or_default()
    }

    fn set(&self, id: &str, r: Option<Record>) -> anyhow::Result<()> {
        let mut s = self.saved.lock().unwrap();
        let mut next = s.clone();
        match r {
            Some(r) => _ = next.servers.insert(id.to_string(), r),
            None => _ = next.servers.remove(id),
        }
        self.write(&next)?;
        *s = next;
        Ok(())
    }

    pub fn secret(&self) -> String {
        self.saved.lock().unwrap().secret.clone()
    }

    /// The forwarding secret, made the first time (the setup's Velocity
    /// step sends it to the VPS; each server's mod config holds it too).
    pub fn ensure_secret(&self) -> anyhow::Result<String> {
        let mut s = self.saved.lock().unwrap();
        if s.secret.is_empty() {
            let mut next = s.clone();
            next.secret = crate::auth::random_hex(32);
            self.write(&next)?;
            *s = next;
        }
        Ok(s.secret.clone())
    }

    /// A server that isn't direct (its files are changed for the relay).
    pub fn first_changed(&self) -> Option<String> {
        self.saved.lock().unwrap().servers.iter().find(|(_, r)| r.mode != Mode::Direct).map(|(id, _)| id.clone())
    }

    /// Why `key` can't be changed on server `id` now, if it can't.
    pub fn locked(&self, id: &str, key: &str) -> Option<&'static str> {
        (self.mode(id) == Mode::Relay).then(|| pf::locked(key)).flatten()
    }
}

/// Every key locked on server `id`, with why.
pub fn locked_keys(app: &App, id: &str) -> BTreeMap<&'static str, &'static str> {
    pf::MANAGED.iter().filter_map(|(k, _)| app.vps_forward.locked(id, k).map(|why| (*k, why))).collect()
}

fn unix() -> u64 {
    hopper_core::time::now().max(0) as u64
}

/// The second the newest History folder of `root` was made, 0 for none.
fn newest_history(root: &Path) -> u64 {
    std::fs::read_dir(root.join(hopper_core::sync::BACKUP_DIR))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|d| d.file_name().to_str()?.parse::<u64>().ok())
        .max()
        .unwrap_or(0)
}

/// Writes `text` to `path`, setting the old file aside in History first.
fn write_recorded(root: &Path, path: &Path, text: &str) -> anyhow::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".hopper-tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text)?;
    let saved = match hopper_core::sync::backup_into(root, path) {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        if let Some(s) = saved {
            let _ = std::fs::rename(&s, path);
        }
        return Err(e.into());
    }
    Ok(())
}

fn notes(changes: &[pf::KeyChange]) -> Vec<hopper_core::backups::SettingNote> {
    let show = |v: &Option<String>| v.clone().unwrap_or_else(|| "(not set)".into());
    changes.iter().map(|c| hopper_core::backups::SettingNote { key: c.key.clone(), was: show(&c.was), now: show(&c.now) }).collect()
}

fn reason(act: Action) -> &'static str {
    match act {
        Action::Use => "Turned on the relay through a VPS",
        Action::Stop => "Stopped using the relay through a VPS",
        Action::PlayAtHome => "Playing at home while the relay is down",
        Action::BackToRelay => "Back on the relay through a VPS",
    }
}

/// Adds Modrinth project `project` to `inst` unless it's there; returns the
/// files that came with it (none when it was there already).
fn add_project(inst: &mut hopper_core::instances::Instance, project: &str, name: &str, job: &hopper_core::jobs::Job) -> anyhow::Result<Vec<String>> {
    let id = hopper_core::modrinth::num_id(project);
    if inst.addons.iter().any(|a| a.project_id == id) {
        return Ok(Vec::new());
    }
    job.detail(format!("Adding {name}"));
    let had: Vec<String> = inst.addons.iter().map(|a| a.file_name.clone()).collect();
    let m = hopper_core::source::get_mod(id)?;
    let f = hopper_core::install::best_file(m.id, m.class_id, inst)?
        .with_context(|| format!("{name} has no file for Minecraft {} with this loader.", inst.game_version))?;
    hopper_core::install::install(inst, &m, &f, job)?;
    Ok(inst.addons.iter().map(|a| a.file_name.clone()).filter(|f| !had.contains(f)).collect())
}

/// Where the forwarding mod is in the server's mod list.
fn find_mod(inst: &hopper_core::instances::Instance, m: pf::ForwardMod, file: &str) -> Option<usize> {
    let project = hopper_core::modrinth::num_id(m.project());
    inst.addons.iter().position(|a| (!file.is_empty() && a.file_name == file) || a.project_id == project || m.matches("", &a.file_name))
}

/// The mod's config and the `server.properties` keys for step `p`, each
/// written with the old file set aside in History. Returns the keys changed.
fn files_step(root: &Path, fm: pf::ForwardMod, secret: &str, p: &pf::Plan, voice_to: Option<(&str, u16)>, rec: &mut Record, since: u64) -> anyhow::Result<Vec<pf::KeyChange>> {
    let cfg_path = root.join(fm.config_rel());
    match p.config {
        Some(true) => {
            rec.config_before = std::fs::read_to_string(&cfg_path).ok();
            write_recorded(root, &cfg_path, &fm.config_text(secret))?;
        }
        Some(false) => match rec.config_before.take() {
            Some(old) => write_recorded(root, &cfg_path, &old)?,
            // There was none: the relay's goes into History.
            None => _ = hopper_core::sync::backup_into(root, &cfg_path)?,
        },
        None => {}
    }
    let props_path = root.join("server.properties");
    let mut changed = Vec::new();
    if let Some(on) = p.keys {
        let mut props = Properties::load(&props_path)?;
        changed = if on {
            let (before, changed) = pf::apply(&mut props);
            rec.keys_before = before;
            changed
        } else {
            pf::restore(&mut props, &rec.keys_before)
        };
        if !changed.is_empty() {
            write_recorded(root, &props_path, &props.to_text())?;
            hopper_core::backups::note_settings(root, since, "server.properties", MOD_NAME, &notes(&changed));
        }
        voice_step(root, on, voice_to, rec, since)?;
    }
    Ok(changed)
}

/// The voice mod's advertised address (B6), with the keys: pointed at the
/// relay (`to`: its players' host and voice port) or put back. Players'
/// voice then goes to the VPS like their game connection; without it the
/// mod tells them the home address, which they can't reach.
fn voice_step(root: &Path, on: bool, to: Option<(&str, u16)>, rec: &mut Record, since: u64) -> anyhow::Result<()> {
    use hopper_core::hosting::voice_mods as vm;
    let vnotes = |c: &[vm::VoiceChange]| notes(&c.iter().map(|c| pf::KeyChange { key: c.key.clone(), was: c.was.clone(), now: c.now.clone() }).collect::<Vec<_>>());
    if on {
        let (Some((host, port)), None) = (to, rec.voice.as_ref()) else { return Ok(()) };
        let Some(d) = vm::detect(root, super::home::server_port(root)) else { return Ok(()) };
        let path = root.join(d.kind.config_rel());
        let text = std::fs::read_to_string(&path).ok();
        let (new, before, changes) = vm::apply(d.kind, text.as_deref(), host, port);
        if let Some(t) = new {
            write_recorded(root, &path, &t)?;
            hopper_core::backups::note_settings(root, since, d.kind.config_rel(), MOD_NAME, &vnotes(&changes));
        }
        rec.voice = Some(before);
    } else if let Some(b) = rec.voice.take() {
        let Some(kind) = b.kind else { return Ok(()) };
        let path = root.join(kind.config_rel());
        // Gone (the mod was removed): nothing to put back.
        let Ok(text) = std::fs::read_to_string(&path) else { return Ok(()) };
        let (new, changes) = vm::restore(&text, &b);
        if let Some(t) = new {
            write_recorded(root, &path, &t)?;
            hopper_core::backups::note_settings(root, since, kind.config_rel(), MOD_NAME, &vnotes(&changes));
        }
    }
    Ok(())
}

/// Does `act` to server `e`'s files. The server must be stopped.
pub fn apply(app: &App, e: &Entry, act: Action, who: hopper_core::actor::Who, job: &hopper_core::jobs::Job) -> anyhow::Result<Value> {
    let mut rec = app.vps_forward.record(&e.id);
    let p = pf::plan(rec.mode, act).map_err(anyhow::Error::msg)?;
    anyhow::ensure!(!e.server.state().is_alive(), "Stop the server first: the change needs a restart.");
    let mut inst = crate::mods_api::instance_of(e)?;
    if act == Action::Use {
        anyhow::ensure!(app.vps.config().servers.iter().any(|s| *s == e.id), "Set up the relay for this server first.");
        let made_dirs = pf::DIRS.iter().filter(|d| !e.root.join(d).exists()).map(|d| d.to_string()).collect();
        rec = Record { forward_mod: Some(pf::choose(&inst.loader, &inst.game_version).map_err(anyhow::Error::msg)?), made_dirs, ..Default::default() };
    }
    let fm = rec.forward_mod.context("The relay's record for this server is missing its mod.")?;
    let secret = app.vps_forward.secret();
    if p.config == Some(true) {
        anyhow::ensure!(!secret.is_empty(), "The relay on the VPS has no Velocity yet. Run the relay setup again (Repair), then try again.");
    }
    let _by = hopper_core::actor::batch(&e.root, who);
    // History folders are named by the second: start in a second of our
    // own, or this change's files and reason land in the last change's
    // folder (Stop right after Use did).
    while newest_history(&e.root) >= unix() {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let since = unix();
    // The mod first: it's the step that needs the internet.
    match p.mod_ {
        ModStep::None => {}
        ModStep::Add => {
            match find_mod(&inst, fm, "") {
                Some(i) => {
                    rec.mod_file = inst.addons[i].file_name.clone();
                    rec.mod_was_off = !inst.addons[i].enabled;
                    if rec.mod_was_off {
                        hopper_core::install::set_enabled_later(&mut inst, i, true)?;
                    }
                }
                None => {
                    rec.added_files = add_project(&mut inst, fm.project(), fm.name(), job)?;
                    rec.added_mod = true;
                    rec.mod_file = find_mod(&inst, fm, "").map(|i| inst.addons[i].file_name.clone()).unwrap_or_default();
                }
            }
            // What it needs and the server lacks; taken out again with it.
            for need in fm.needs() {
                let added = add_project(&mut inst, need, "Fabric API", job)?;
                rec.added_files.extend(added);
            }
        }
        ModStep::Remove => {
            // What the relay added (the mod and what it needed), nothing else.
            let mine = rec.added_files.iter().chain(rec.added_mod.then_some(&rec.mod_file));
            for f in mine {
                if let Some(i) = inst.addons.iter().position(|a| a.file_name == *f) {
                    hopper_core::install::remove(&mut inst, i)?;
                }
            }
            if !rec.added_mod
                && let Some(i) = find_mod(&inst, fm, &rec.mod_file)
            {
                // It was there before: back to on or off as it was.
                hopper_core::install::set_enabled_later(&mut inst, i, !rec.mod_was_off)?;
            }
        }
        ModStep::On | ModStep::Off => {
            if let Some(i) = find_mod(&inst, fm, &rec.mod_file) {
                hopper_core::install::set_enabled_later(&mut inst, i, p.mod_ == ModStep::On)?;
            }
        }
    }
    let to = app.vps.voice_to();
    let changed = files_step(&e.root, fm, &secret, &p, to.as_ref().map(|(h, p)| (h.as_str(), *p)), &mut rec, since)?;
    if p.to == Mode::Direct {
        // Only empty ones go (remove_dir refuses the rest).
        for d in &rec.made_dirs {
            let _ = std::fs::remove_dir(e.root.join(d));
        }
    }
    hopper_core::backups::mark_reason(&e.root, since, reason(act));
    rec.mode = p.to;
    rec.since = unix();
    app.vps_forward.set(&e.id, (p.to != Mode::Direct).then_some(rec))?;
    app.vps.wake();
    log::info!("{}: {} ({} key(s) changed)", e.name, reason(act).to_lowercase(), changed.len());
    Ok(json!({ "mode": p.to, "keys": changed.len() }))
}

/// The card's part: where the server stands and what it can do.
pub fn status_json(app: &App, e: &Entry) -> Value {
    let r = app.vps_forward.record(&e.id);
    let (loader, mc) = crate::mods_api::instance_of(e).map(|i| (i.loader, i.game_version)).unwrap_or_default();
    let can = pf::choose(&loader, &mc);
    let m = r.forward_mod.or(can.as_ref().ok().copied());
    json!({
        "mode": r.mode,
        "mod": m.map(|m| m.name()),
        "why_not": can.err(),
        "since": (r.mode != Mode::Direct).then_some(r.since),
        "velocity": !app.vps_forward.secret().is_empty(),
        "running": e.server.state().is_alive(),
    })
}

// ---- API -------------------------------------------------------------------------------

pub fn routes() -> Router<Arc<App>> {
    Router::new().route("/api/v1/servers/{id}/vps-relay/forward", post(post_forward))
}

#[derive(Deserialize)]
struct ForwardBody {
    action: Action,
    /// Stop the server, change, and start it again (the dialog asked).
    #[serde(default)]
    restart: bool,
}

/// Owner (and Manager) only: it turns account checks off on the server.
async fn post_forward(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, UrlPath(id): UrlPath<String>, Json(b): Json<ForwardBody>) -> ApiResult<Json<Value>> {
    require_owner(&app, &headers, &method)?;
    let e = app.servers.get(&id).ok_or_else(|| not_found("server"))?;
    let rec = app.vps_forward.record(&id);
    pf::plan(rec.mode, b.action).map_err(bad)?;
    let alive = e.server.state().is_alive();
    if alive && !b.restart {
        return Err(ApiError(StatusCode::CONFLICT, "Stop the server first, or let Hopper stop it, change it and start it again.".into()));
    }
    let who = crate::mods_api::changer(&app, &headers, &method);
    let a = app.clone();
    let title = format!("{} for {}", reason(b.action), e.name);
    let job = crate::pack_api::spawn(&app, title, move |job| {
        if alive {
            job.detail("Stopping the server");
            a.servers.act(&e, PanelAction::Stop)?;
            let mut waited = 0;
            while e.server.state().is_alive() {
                anyhow::ensure!(waited < 600, "The server didn't stop in 5 minutes, so nothing was changed.");
                std::thread::sleep(Duration::from_millis(500));
                waited += 1;
            }
        }
        let r = apply(&a, &e, b.action, who, job);
        // Started again even when the change failed half-way: it was running.
        if alive {
            job.detail("Starting the server");
            crate::router::started_by_hand(&e);
            if let Err(err) = a.servers.act(&e, PanelAction::Start) {
                log::warn!("{}: starting again after the relay change: {err:#}", e.name);
            }
        }
        a.refresh.notify_one();
        r
    });
    Ok(Json(json!({ "job": job })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hopper_core::hosting::proxy_forward::ForwardMod;

    const PROPS: &str = "#Minecraft server properties\nmotd=Test\nonline-mode=true\nserver-port=25571\n";

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopperd-forward-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn history(root: &Path) -> Vec<(String, String)> {
        hopper_core::backups::list(root).iter().flat_map(|s| s.files.iter().map(|f| (f.restore_to.clone(), f.reason.clone()))).collect()
    }

    #[test]
    fn use_then_stop_puts_every_file_back_and_history_has_each() {
        let root = scratch("use-stop");
        std::fs::write(root.join("server.properties"), PROPS).unwrap();
        let mut rec = Record { forward_mod: Some(ForwardMod::FabricProxyLite), ..Default::default() };
        let fm = ForwardMod::FabricProxyLite;
        let on = pf::plan(Mode::Direct, Action::Use).unwrap();
        let changed = files_step(&root, fm, "s3cret", &on, None, &mut rec, 0).unwrap();
        hopper_core::backups::mark_reason(&root, 0, reason(Action::Use));
        assert_eq!(changed.len(), 4);
        let props = std::fs::read_to_string(root.join("server.properties")).unwrap();
        assert!(props.contains("online-mode=false") && props.contains("server-ip=127.0.0.1") && props.contains("network-compression-threshold=-1"));
        let cfg = std::fs::read_to_string(root.join("config/FabricProxy-Lite.toml")).unwrap();
        assert!(cfg.contains("secret = \"s3cret\""));
        assert_eq!(rec.config_before, None);
        // History: the old server.properties, with the keys noted and the reason.
        let h = history(&root);
        assert!(h.iter().any(|(to, why)| to == "server.properties" && why == "Turned on the relay through a VPS"), "{h:?}");
        let set = &hopper_core::backups::list(&root)[0];
        let note = hopper_core::backups::settings_note(&set.dir, "server.properties").unwrap();
        assert_eq!(note.mod_name, MOD_NAME);
        assert!(note.changes.iter().any(|c| c.key == "online-mode" && c.was == "true" && c.now == "false"));
        assert!(note.changes.iter().any(|c| c.key == "server-ip" && c.was == "(not set)"));

        let off = pf::plan(Mode::Relay, Action::Stop).unwrap();
        files_step(&root, fm, "s3cret", &off, None, &mut rec, 0).unwrap();
        // Byte for byte as before, and the relay's config set aside in History.
        assert_eq!(std::fs::read_to_string(root.join("server.properties")).unwrap(), PROPS);
        assert!(!root.join("config/FabricProxy-Lite.toml").exists());
        let h = history(&root);
        assert!(h.iter().any(|(to, _)| to == "config/FabricProxy-Lite.toml"), "{h:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_windows_properties_file_comes_back_byte_for_byte() {
        // Minecraft on Windows writes CRLF; the undo must not turn it into LF.
        let root = scratch("crlf");
        let crlf = PROPS.replace('\n', "\r\n");
        std::fs::write(root.join("server.properties"), &crlf).unwrap();
        let fm = ForwardMod::FabricProxyLite;
        let mut rec = Record { forward_mod: Some(fm), ..Default::default() };
        files_step(&root, fm, "k", &pf::plan(Mode::Direct, Action::Use).unwrap(), None, &mut rec, 0).unwrap();
        files_step(&root, fm, "k", &pf::plan(Mode::Relay, Action::Stop).unwrap(), None, &mut rec, 0).unwrap();
        assert_eq!(std::fs::read(root.join("server.properties")).unwrap(), crlf.as_bytes());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_config_that_was_there_comes_back() {
        let root = scratch("cfg-back");
        std::fs::write(root.join("server.properties"), PROPS).unwrap();
        std::fs::create_dir_all(root.join("config")).unwrap();
        let old = "[forwarding]\n\tenabled = false\n";
        std::fs::write(root.join("config/proxy-compatible-forge.toml"), old).unwrap();
        let fm = ForwardMod::Pcf;
        let mut rec = Record { forward_mod: Some(fm), ..Default::default() };
        files_step(&root, fm, "k", &pf::plan(Mode::Direct, Action::Use).unwrap(), None, &mut rec, 0).unwrap();
        assert_eq!(rec.config_before.as_deref(), Some(old));
        assert!(std::fs::read_to_string(root.join("config/proxy-compatible-forge.toml")).unwrap().contains("mode = \"MODERN\""));
        files_step(&root, fm, "k", &pf::plan(Mode::Relay, Action::Stop).unwrap(), None, &mut rec, 0).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("config/proxy-compatible-forge.toml")).unwrap(), old);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn play_at_home_turns_accounts_back_on_and_back_on_the_relay_off() {
        let root = scratch("home");
        std::fs::write(root.join("server.properties"), PROPS).unwrap();
        let fm = ForwardMod::FabricProxyLite;
        let mut rec = Record { forward_mod: Some(fm), ..Default::default() };
        files_step(&root, fm, "k", &pf::plan(Mode::Direct, Action::Use).unwrap(), None, &mut rec, 0).unwrap();
        files_step(&root, fm, "k", &pf::plan(Mode::Relay, Action::PlayAtHome).unwrap(), None, &mut rec, 0).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("server.properties")).unwrap(), PROPS);
        // The mod's config stays (the mod is off), so going back needs no secret again.
        assert!(root.join("config/FabricProxy-Lite.toml").exists());
        files_step(&root, fm, "k", &pf::plan(Mode::Home, Action::BackToRelay).unwrap(), None, &mut rec, 0).unwrap();
        assert!(std::fs::read_to_string(root.join("server.properties")).unwrap().contains("online-mode=false"));
        files_step(&root, fm, "k", &pf::plan(Mode::Relay, Action::PlayAtHome).unwrap(), None, &mut rec, 0).unwrap();
        files_step(&root, fm, "k", &pf::plan(Mode::Home, Action::Stop).unwrap(), None, &mut rec, 0).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("server.properties")).unwrap(), PROPS);
        assert!(!root.join("config/FabricProxy-Lite.toml").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_voice_mod_points_at_the_relay_and_back_with_the_keys() {
        use hopper_core::hosting::voice_mods as vm;
        let root = scratch("voice");
        std::fs::write(root.join("server.properties"), PROPS).unwrap();
        std::fs::create_dir_all(root.join("mods")).unwrap();
        std::fs::write(root.join("mods/voicechat-fabric-1.21.1-2.5.26.jar"), b"jar").unwrap();
        std::fs::create_dir_all(root.join("config/voicechat")).unwrap();
        let old = "# Simple Voice Chat server config\r\nport=24454\r\nvoice_host=\r\nallow_pings=true\r\n";
        std::fs::write(root.join(vm::SVC_CONFIG), old).unwrap();
        let fm = ForwardMod::FabricProxyLite;
        let mut rec = Record { forward_mod: Some(fm), ..Default::default() };
        let to = Some(("play.example.net", 24454));
        let read = || std::fs::read_to_string(root.join(vm::SVC_CONFIG)).unwrap();
        files_step(&root, fm, "k", &pf::plan(Mode::Direct, Action::Use).unwrap(), to, &mut rec, 0).unwrap();
        // Java properties escape the colon; the mod reads it back as one.
        assert!(read().contains(r"voice_host=play.example.net\:24454"), "{}", read());
        assert_eq!(rec.voice.as_ref().and_then(|v| v.kind), Some(vm::VoiceMod::SimpleVoiceChat));
        // History has the old config, with the change noted.
        assert!(history(&root).iter().any(|(to, _)| to == vm::SVC_CONFIG), "{:?}", history(&root));
        let set = &hopper_core::backups::list(&root)[0];
        let note = hopper_core::backups::settings_note(&set.dir, vm::SVC_CONFIG).unwrap();
        assert!(note.changes.iter().any(|c| c.key == "voice_host" && c.now == "play.example.net:24454"), "{:?}", note.changes);
        // Play at home: back as it was; back on the relay: pointed again.
        files_step(&root, fm, "k", &pf::plan(Mode::Relay, Action::PlayAtHome).unwrap(), to, &mut rec, 0).unwrap();
        assert_eq!(read(), old);
        assert!(rec.voice.is_none());
        files_step(&root, fm, "k", &pf::plan(Mode::Home, Action::BackToRelay).unwrap(), to, &mut rec, 0).unwrap();
        assert!(read().contains(r"voice_host=play.example.net\:24454"));
        files_step(&root, fm, "k", &pf::plan(Mode::Relay, Action::Stop).unwrap(), to, &mut rec, 0).unwrap();
        assert_eq!(read(), old, "byte for byte, CRLF kept");
        // A relay without a voice port (`to` None) leaves the mod alone.
        files_step(&root, fm, "k", &pf::plan(Mode::Direct, Action::Use).unwrap(), None, &mut rec, 0).unwrap();
        assert_eq!(read(), old);
        assert!(rec.voice.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn keys_lock_only_while_on_the_relay() {
        let dir = scratch("lock");
        let f = Forward::new(&dir);
        assert_eq!(f.locked("a", "online-mode"), None);
        f.set("a", Some(Record { mode: Mode::Relay, ..Default::default() })).unwrap();
        assert!(f.locked("a", "online-mode").unwrap().contains("checked by the relay"));
        assert_eq!(f.locked("a", "motd"), None);
        assert_eq!(f.locked("b", "online-mode"), None);
        f.set("a", Some(Record { mode: Mode::Home, ..Default::default() })).unwrap();
        assert_eq!(f.locked("a", "online-mode"), None, "playing at home: the player may change it");
        assert_eq!(f.first_changed().as_deref(), Some("a"));
        // Kept across a restart, the secret made once.
        let s = f.ensure_secret().unwrap();
        assert_eq!(s.len(), 64);
        let again = Forward::new(&dir);
        assert_eq!(again.secret(), s);
        assert_eq!(again.mode("a"), Mode::Home);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
