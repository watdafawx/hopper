//! The config dialog: the shared config view ([`crate::config_view`]) in a
//! dialog. Opens from a mod's menu, and from the crash card at the setting
//! it's about.

use crate::config_view::{ConfigView, Host};
use crate::store::Store;
use crate::widgets;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::*;
use std::path::{Path, PathBuf};

pub use crate::config_view::Focus;

/// Endings of config files that are text.
const TEXT_ENDINGS: [&str; 12] = [".toml", ".json", ".json5", ".jsonc", ".cfg", ".properties", ".txt", ".yml", ".yaml", ".snbt", ".ini", ".js"];

/// Opens `rel` (one of `files`) in the dialog.
pub fn open(store: Entity<Store>, root: PathBuf, files: Vec<String>, rel: String, focus: Option<Focus>, window: &mut Window, cx: &mut App) {
    open_titled(store, root, String::new(), None, files, rel, focus, window, cx)
}

#[allow(clippy::too_many_arguments)]
fn open_titled(store: Entity<Store>, root: PathBuf, title: String, icon: Option<PathBuf>, files: Vec<String>, rel: String, focus: Option<Focus>, window: &mut Window, cx: &mut App) {
    let room = widgets::dialog_room(window) - window.rem_size() * 4.;
    let view = cx.new(|cx| ConfigView::new(store, root, Host::Dialog, title.clone(), icon, files, Some(rel), focus, window, cx));
    window.open_dialog(cx, move |dialog, window, _| {
        dialog
            .title(if title.is_empty() { "Config".to_string() } else { format!("{title} · config") })
            .w(rems(64.).to_pixels(window.rem_size()))
            .margin_top(rems(widgets::DIALOG_TOP).to_pixels(window.rem_size()))
            .child(div().h(room).child(view.clone()))
    });
}

/// The mod's text config files, relative to the instance: its jar's own
/// ids matched to `config/`, plus anything the shared index found for it.
pub(crate) fn mod_files(store: &Entity<Store>, root: &Path, project_id: u64, file_name: &str, cx: &mut App) -> Option<(String, Option<PathBuf>, Vec<String>)> {
    let inst = store.read(cx).instance(root)?.clone();
    let addon = inst.addons.iter().find(|a| (project_id != 0 && a.project_id == project_id) || a.file_name == file_name)?.clone();
    let mut files: Vec<String> = hopper::modconfig::files(&inst, &addon)
        .into_iter()
        .filter(|f| {
            let l = f.to_ascii_lowercase();
            TEXT_ENDINGS.iter().any(|e| l.ends_with(e))
        })
        .collect();
    let idx = crate::config_view::indexes(cx).read(cx).peek(root);
    if let Some(idx) = idx {
        for f in idx.mod_files(&addon.file_name) {
            if !files.contains(&f) {
                files.push(f);
            }
        }
    }
    files.sort();
    let icon = store.read(cx).image(&addon.icon).map(Path::to_path_buf);
    Some((addon.name, icon, files))
}

/// Opens a mod's config: its first settings file, with chips for the rest.
pub fn open_for_mod(store: Entity<Store>, root: PathBuf, project_id: u64, file_name: String, window: &mut Window, cx: &mut App) {
    if let Some(r) = crate::store::on_hopper_server(&root) {
        return open_for_mod_on_panel(store, root, r, project_id, file_name, window, cx);
    }
    let Some((name, icon, files)) = mod_files(&store, &root, project_id, &file_name, cx) else { return };
    let first = files.iter().find(|f| hopper::configedit::Format::of(f).is_some()).or(files.first()).cloned();
    match first {
        Some(rel) => open_titled(store, root, name, icon, files, rel, None, window, cx),
        None => window.push_notification(Notification::info(format!("{name} has no config files yet. Most mods write them on the first launch.")), cx),
    }
}

/// [`open_for_mod`] for a server on a Hopper Server: its panel says which
/// files are the mod's and sends their text, and saves go back through it
/// (the same view as its Configs tab, in the dialog).
fn open_for_mod_on_panel(store: Entity<Store>, root: PathBuf, r: hopper::hopperd::Remote, project_id: u64, file_name: String, window: &mut Window, cx: &mut App) {
    let t = match crate::panel_tabs::Target::new(r) {
        Ok(t) => t,
        Err(e) => return window.push_notification(Notification::error(format!("Can't reach the panel: {e}")), cx),
    };
    let s = store.read(cx);
    let addon = s.instance(&root).and_then(|i| i.addons.iter().find(|a| (project_id != 0 && a.project_id == project_id) || a.file_name == file_name).cloned());
    let (name, icon_url) = addon.map(|a| (a.name, a.icon)).unwrap_or_else(|| (file_name.clone(), String::new()));
    let icon = s.image(&icon_url).map(Path::to_path_buf);
    let running = s.status(&root).is_some_and(|st| st.ms().is_some());
    let (r2, f2, t2) = (root.clone(), file_name.clone(), t.clone());
    let task = cx.background_spawn(async move {
        let files: Vec<String> = crate::store::remote_config_files(&r2, &f2)?
            .into_iter()
            .filter(|f| {
                let l = f.to_ascii_lowercase();
                TEXT_ENDINGS.iter().any(|e| l.ends_with(e))
            })
            .collect();
        let want: Vec<String> = files.iter().filter(|f| hopper::configedit::Format::of(f).is_some()).cloned().collect();
        let texts = crate::panel_tabs::http(&*t2.client)?.configs_parse(t2.id(), &want)?;
        anyhow::Ok((files, texts))
    });
    window
        .spawn(cx, async move |cx| {
            let r = task.await;
            cx.update(|window, cx| match r {
                Err(e) => window.push_notification(Notification::error(format!("Couldn't read {name}'s config from the panel: {e:#}")), cx),
                Ok((files, _)) if files.is_empty() => {
                    window.push_notification(Notification::info(format!("{name} has no config files yet. Most mods write them on the first launch.")), cx)
                }
                Ok((files, texts)) => {
                    let room = widgets::dialog_room(window) - window.rem_size() * 4.;
                    let side = crate::config_view::ServerSide { source: crate::config_view::Source::Panel(t), running };
                    let title = name.clone();
                    let view = cx.new(|cx| ConfigView::for_server(store, root, side, texts, title, icon, files, None, window, cx));
                    window.open_dialog(cx, move |dialog, window, _| {
                        dialog
                            .title(format!("{name} · config"))
                            .w(rems(64.).to_pixels(window.rem_size()))
                            .margin_top(rems(widgets::DIALOG_TOP).to_pixels(window.rem_size()))
                            .child(div().h(room).child(view.clone()))
                    });
                }
            })
            .ok();
        })
        .detach();
}
