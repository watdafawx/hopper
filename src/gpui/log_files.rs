//! Deleting crash reports and old logs (servers and instances): one plain
//! confirm, then the files go to the trash. On a server run by Hopper
//! Server the panel does it; on this PC [`hopper::crashfiles`] does.

use crate::widgets;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{WindowExt as _, v_flex};
use gpui_kit::*;
use hopper::crashfiles::Kind;
use std::path::Path;

/// One older log: path under the folder, size, modified (unix seconds).
pub struct OldLog {
    pub rel: String,
    pub size: u64,
    pub modified: i64,
}

/// "Today · 14:54:12" for lists and headers; its tooltip is
/// [`hopper::time::exact_full`].
pub fn when(t: i64) -> String {
    hopper::time::exact(t)
}

/// Deletes `rels` (paths under `root`). `running`: the game or server is
/// up, so the log it writes can't go.
pub fn delete(root: &Path, rels: &[String], running: bool) -> anyhow::Result<usize> {
    match hopper::hopperd::locate(root) {
        Some(r) => {
            let (c, id, _) = r?;
            c.delete_logs(&id, rels)
        }
        None => hopper::crashfiles::delete(root, rels, running),
    }
}

/// "Clear all" for crash reports, or old logs.
pub fn clear(root: &Path, kind: Kind) -> anyhow::Result<usize> {
    match hopper::hopperd::locate(root) {
        Some(r) => {
            let (c, id, _) = r?;
            c.clear_logs(&id, if kind == Kind::Crashes { "crashes" } else { "logs" })
        }
        None => hopper::crashfiles::clear(root, kind),
    }
}

/// The older logs of `root` (everything in logs/ but the current latest.log
/// and debug.log), newest first.
pub fn old_logs(root: &Path) -> Result<Vec<OldLog>, String> {
    if let Some(r) = hopper::hopperd::locate(root) {
        let (c, id, _) = r.map_err(|e| format!("{e:#}"))?;
        let list = c.old_logs(&id).map_err(|e| format!("{e:#}"))?;
        return Ok(list.into_iter().map(|(rel, size, modified)| OldLog { rel, size, modified }).collect());
    }
    let mut out: Vec<OldLog> = hopper::crashfiles::clearable(root, Kind::Logs)
        .into_iter()
        .filter_map(|rel| {
            let m = std::fs::metadata(root.join(&rel)).ok()?;
            let modified = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
            Some(OldLog { rel, size: m.len(), modified })
        })
        .collect();
    out.sort_by(|a, b| b.modified.cmp(&a.modified).then(b.rel.cmp(&a.rel)));
    Ok(out)
}

/// A plain confirm: what goes, that it can be got back, Cancel / Delete.
/// `go` runs on Delete.
pub fn confirm(window: &mut Window, cx: &mut App, title: String, body: String, go: impl Fn(&mut Window, &mut App) + 'static) {
    let go: std::rc::Rc<dyn Fn(&mut Window, &mut App)> = std::rc::Rc::new(go);
    window.open_dialog(cx, move |dialog, window, _| {
        let go = go.clone();
        dialog
            .title(title.clone())
            .w(rems(26.).to_pixels(window.rem_size()))
            .margin_top(rems(widgets::DIALOG_TOP).to_pixels(window.rem_size()))
            .child(v_flex().gap_2().child(div().text_sm().child(body.clone())))
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("del-cancel").outline().label("Cancel")))
                    .child(DialogAction::new().child(Button::new("del-ok").danger().label("Delete"))),
            )
            .on_ok(move |_, window, cx| {
                go(window, cx);
                true
            })
    });
}

/// Tells how a delete went.
pub fn report(window: &mut Window, cx: &mut App, r: anyhow::Result<usize>) {
    match r {
        Ok(0) => {}
        Ok(1) => window.push_notification(Notification::success("Deleted 1 file. It's in the trash."), cx),
        Ok(n) => window.push_notification(Notification::success(format!("Deleted {n} files. They're in the trash.")), cx),
        Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
    }
}

/// Where deleted things can be got back from, for the confirm's text.
pub fn trash_name(root: &Path) -> &'static str {
    if hopper::hopperd::is_remote_root(root) { "the server's trash (Files › Trash)" } else { "the Recycle Bin" }
}

/// "3.2 MB".
pub fn size(b: u64) -> String {
    hopper::hosting::local_panel::format_bytes(b)
}
