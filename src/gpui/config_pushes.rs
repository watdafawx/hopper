//! Settings a Hopper Server's owner pushed to this PC ("Push N to paired
//! Hopper clients" on the panel's Configs page): one notification per
//! server with Apply and Dismiss. Nothing changes here without the Apply
//! click; the old copy of each file goes to the instance's backups first.
//! Also sends this PC's copy of the settings files both sides read, so the
//! panel can show players' copies beside the server's.

use crate::store::Store;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::*;
use hopper::configedit;
use hopper::configowner;
use hopper::hopperd::{Client, ConfigPush};
use hopper::server::ServerInfo;
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// The notification's id type (one per server at a time).
struct PushNote;

/// Looks for pushes from `server` (a Hopper Server's server) for the
/// instance at `inst_root` and, when there are any, shows "<server> wants
/// to change N settings in <mods>" with Apply and Dismiss. Quiet when the
/// panel can't be reached or there's nothing waiting.
pub fn check(_store: &Entity<Store>, inst_root: &Path, server: &ServerInfo, window: &mut Window, cx: &mut App) {
    let Some(remote) = server.remote.clone() else { return };
    let root = inst_root.to_path_buf();
    let name = server.name.clone();
    window
        .spawn(cx, async move |cx| {
            let found = cx
                .background_executor()
                .spawn({
                    let (remote, root) = (remote.clone(), root.clone());
                    async move {
                        let c = Client::for_remote(&remote)?;
                        let pushes = c.configs_pushes(&remote.id)?;
                        let mods = mods_named(&root, &pushes);
                        anyhow::Ok((pushes, mods))
                    }
                })
                .await;
            let (pushes, mods) = match found {
                Ok(f) if !f.0.is_empty() => f,
                Ok(_) => return,
                Err(e) => {
                    log::info!("config pushes from {name}: {e:#}");
                    return;
                }
            };
            let _ = cx.update(|window, cx| show(name, remote, root, pushes, mods, window, cx));
        })
        .detach();
}

/// Sends this PC's copy of the settings files the server reads too
/// (`configcompare::compared`) to `server`'s panel, in the background.
/// Does nothing for a server that isn't on a Hopper Server.
pub fn send_snapshot(inst_root: &Path, server: &ServerInfo) {
    let Some(remote) = server.remote.clone() else { return };
    let root = inst_root.to_path_buf();
    std::thread::spawn(move || {
        let files = hopper::hopperd::snapshot_files(&root);
        if files.is_empty() {
            return;
        }
        if let Err(e) = Client::for_remote(&remote).and_then(|c| c.configs_snapshot(&remote.id, &files)) {
            log::info!("sending config snapshot: {e:#}");
        }
    });
}

/// The mods the pushed files belong to, by name ("Create, Mekanism and 2 more").
fn mods_named(root: &Path, pushes: &[ConfigPush]) -> String {
    let jars: Vec<(String, PathBuf, String)> = std::fs::read_dir(root.join("mods"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().to_lowercase().ends_with(".jar"))
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path(), String::new()))
        .collect();
    let owners = configowner::Owners::new(configowner::of_jars(jars));
    let mut names: Vec<String> = Vec::new();
    for p in pushes {
        let n = owners.owner(&p.rel).map(|m| m.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| p.rel.rsplit('/').next().unwrap_or(&p.rel).to_string());
        if !names.contains(&n) {
            names.push(n);
        }
    }
    match names.len() {
        0 => String::new(),
        1 => names[0].clone(),
        2 => format!("{} and {}", names[0], names[1]),
        3 => format!("{}, {} and {}", names[0], names[1], names[2]),
        n => format!("{}, {} and {} more", names[0], names[1], n - 2),
    }
}

fn show(name: String, remote: hopper::hopperd::Remote, root: PathBuf, pushes: Vec<ConfigPush>, mods: String, window: &mut Window, cx: &mut App) {
    let n: usize = pushes.iter().map(|p| p.edits.len()).sum();
    let text = format!("{name} wants to change {n} setting{} in {mods}.", if n == 1 { "" } else { "s" });
    let done = Rc::new(Cell::new(false));
    let pushes = Rc::new(pushes);
    let note = Notification::info(text)
        .title("Settings from the server")
        .id1::<PushNote>(SharedString::from(name.clone()))
        .action({
            let (done, pushes, remote, root, name) = (done.clone(), pushes.clone(), remote.clone(), root.clone(), name.clone());
            move |_, _, cx| {
                let (done, pushes, remote, root, name) = (done.clone(), pushes.clone(), remote.clone(), root.clone(), name.clone());
                Button::new("cfg-push-apply").label("Apply").primary().on_click(cx.listener(move |note, _, window, cx| {
                    if done.replace(true) {
                        return;
                    }
                    let (ok, failed) = apply(&root, &pushes, &remote);
                    note.dismiss(window, cx);
                    let msg = if failed.is_empty() {
                        format!("Applied {ok} setting{} from {name}. Undo it from ⋯ › History.", if ok == 1 { "" } else { "s" })
                    } else {
                        format!("Applied {ok} setting{} from {name}. Not applied: {}", if ok == 1 { "" } else { "s" }, failed.join(" "))
                    };
                    let n = if failed.is_empty() { Notification::success(msg) } else { Notification::warning(msg) };
                    window.push_notification(n, cx);
                }))
            }
        })
        // Closing it without Apply is the Dismiss: the panel shows the owner it was turned down.
        .on_close({
            let (done, pushes, remote) = (done.clone(), pushes.clone(), remote.clone());
            move |_, _| {
                if done.replace(true) {
                    return;
                }
                let (pushes, remote) = ((*pushes).clone(), remote.clone());
                std::thread::spawn(move || {
                    if let Ok(c) = Client::for_remote(&remote) {
                        for p in &pushes {
                            let _ = c.configs_push_done(&remote.id, &p.id, "dismissed", "Dismissed on this PC.");
                        }
                    }
                });
            }
        });
    window.push_notification(note, cx);
}

/// Saves each push into this PC's copy (old copy to backups first) and
/// tells the panel how it went. The settings saved, and why the others weren't.
fn apply(root: &Path, pushes: &[ConfigPush], remote: &hopper::hopperd::Remote) -> (usize, Vec<String>) {
    let mut ok = 0;
    let mut failed = Vec::new();
    let mut results = Vec::new();
    for p in pushes {
        let res = std::fs::read_to_string(root.join(&p.rel))
            .map_err(|_| format!("This PC has no {}.", p.rel))
            .and_then(|text| p.typed(&text).map(|edits| (text, edits)))
            .and_then(|(text, edits)| configedit::save_edits(root, &p.rel, &text, &edits).map(|_| edits.len()).map_err(|e| format!("{e:#}")));
        match res {
            Ok(n) => {
                ok += n;
                results.push((p.id.clone(), "applied", format!("Applied {n} on this PC.")));
            }
            Err(e) => {
                failed.push(format!("{}: {e}", p.rel));
                results.push((p.id.clone(), "dismissed", format!("Couldn't apply: {e}")));
            }
        }
    }
    let remote = remote.clone();
    std::thread::spawn(move || {
        if let Ok(c) = Client::for_remote(&remote) {
            for (pid, state, note) in results {
                if let Err(e) = c.configs_push_done(&remote.id, &pid, state, &note) {
                    log::info!("marking a config push {state}: {e:#}");
                }
            }
        }
    });
    (ok, failed)
}
