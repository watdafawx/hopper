//! Delete server (the server header's ⋯ menu): a dialog that names what
//! goes (its worlds, mods, configs, backups), can keep the worlds, and asks
//! for the server's name. A server on this PC goes to the Recycle Bin; one
//! on a Hopper Server goes to the panel's deleted servers, kept 30 days.

use crate::store::Store;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::PathBuf;

/// Whether Hopper can delete the server in `root`: one it hosts here, or
/// one on a paired Hopper Server.
pub fn can_delete(store: &Store, root: &std::path::Path) -> bool {
    store.is_hosted(root) || store.servers().iter().any(|s| s.root == root && s.remote.is_some())
}

struct DeleteForm {
    store: Entity<Store>,
    root: PathBuf,
    name: String,
    /// The panel's name when it's on a Hopper Server.
    panel: Option<String>,
    /// Its worlds' folders and their size (this PC only).
    worlds: Vec<(String, u64)>,
    backups: bool,
    keep_world: bool,
    /// On a Hopper Server: whether this PC may delete there (only the
    /// owner or a manager may; a paired PC is a device). None while asking.
    may: Option<bool>,
    /// The server's Settings › General page in the panel, where its owner deletes it.
    web: Option<String>,
    /// Delete for good instead of the Recycle Bin / the panel's 30 days.
    permanent: bool,
}

impl DeleteForm {
    fn ready(&self, cx: &App) -> bool {
        let _ = cx;
        // No typing the name: the tick box is the deliberate step for
        // deleting for good; otherwise a deleted server can be restored
        // (panel: 30 days in its deleted servers; this PC: the Recycle Bin).
        self.may != Some(false)
    }
}

fn size(bytes: u64) -> String {
    let gb = bytes as f64 / 1_073_741_824.0;
    if gb >= 1.0 { format!("{gb:.1} GB") } else { format!("{:.0} MB", (bytes as f64 / 1_048_576.0).max(1.0)) }
}

impl Render for DeleteForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let host = self.panel.clone().unwrap_or_else(|| "this PC".into());
        let row = |what: String, how_big: Option<String>| {
            h_flex()
                .gap_3()
                .text_sm()
                .child(div().flex_1().min_w_0().child(what))
                .children(how_big.map(|b| div().text_color(theme.muted_foreground).child(b)))
        };
        let world_line = match self.worlds.as_slice() {
            [] if self.panel.is_none() => None,
            [] => Some(row("Its worlds".into(), None)),
            ws => Some(row(
                format!("Its world ({})", ws.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ")),
                Some(size(ws.iter().map(|(_, b)| b).sum())),
            )),
        };
        if self.may == Some(false) {
            let web = self.web.clone();
            return v_flex()
                .gap_3()
                .child(div().text_sm().child(hopper::hopperd::cant_manage_text(&host, "delete")))
                .when_some(web, |c, url| {
                    c.child(
                        h_flex().child(
                            Button::new("delete-open-panel")
                                .outline()
                                .small()
                                .icon(gpui_kit::assets::IconName::ExternalLink)
                                .label("Open in browser")
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                        ),
                    )
                })
                .into_any_element();
        }
        let kept_where = format!("Kept worlds\\{}", self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        v_flex()
            .gap_3()
            .child(div().text_sm().text_color(theme.muted_foreground).child(format!("On {host}. This deletes:")))
            .child(
                v_flex()
                    .gap_1p5()
                    .p_3()
                    .rounded(px(crate::theme::RADIUS))
                    .bg(theme.muted)
                    .children(world_line)
                    .child(row("Its mods, configs and every other file".into(), None))
                    .when(self.backups, |c| c.child(row("Its backups in .hopper-backups".into(), None))),
            )
            .when(self.panel.is_some() || !self.worlds.is_empty(), |c| {
                c.child(
                    v_flex()
                        .gap_1()
                        .child(
                            Checkbox::new("delete-keep-world")
                                .label("Keep the world")
                                .checked(self.keep_world)
                                .on_change(cx.listener(|this, v: &bool, _, cx| {
                                    this.keep_world = *v;
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("Moved to {kept_where} in {host}'s servers folder, so a new server can use it.")),
                        ),
                )
            })
            .child(div().text_xs().text_color(if self.permanent { theme.danger } else { theme.muted_foreground }).child(match (&self.panel, self.permanent) {
                (_, true) => "The folder is deleted for good and can't be restored.".to_string(),
                (Some(p), _) => format!("{p} keeps deleted servers for 30 days: restore one from the panel's Settings › Panel folder."),
                (None, _) => "The rest of the folder goes to the Recycle Bin, where you can restore it.".into(),
            }))
            .child(
                Checkbox::new("delete-permanent")
                    .label("Delete permanently (can't be undone)")
                    .checked(self.permanent)
                    .on_change(cx.listener(|this, v: &bool, _, cx| {
                        this.permanent = *v;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }
}

/// Opens the dialog for the server in `root`; `running` (by its panel's
/// last word) says stop it first instead.
pub fn open(store: Entity<Store>, root: PathBuf, running: bool, window: &mut Window, cx: &mut App) {
    let Some(srv) = store.read(cx).servers().iter().find(|s| s.root == root).cloned() else { return };
    if running {
        window.push_notification(Notification::info(format!("Stop {} first, then delete it.", srv.name)), cx);
        return;
    }
    let panel = srv.remote.as_ref().map(|r| {
        store
            .read(cx)
            .settings()
            .hopper_panels
            .iter()
            .find(|p| p.id == r.panel)
            .map_or_else(|| hopper::hopperd::host_name(&r.base), |p| p.name.clone())
    });
    let (worlds, backups) = if panel.is_none() {
        let ws = hopper::hosting::worlds::list(&root)
            .into_iter()
            .filter(|w| w.created)
            .map(|w| (w.name, w.size))
            .collect();
        (ws, root.join(".hopper-backups").is_dir())
    } else {
        (Vec::new(), true)
    };
    let client = srv.remote.as_ref().and_then(|r| {
        store.read(cx).settings().hopper_panels.iter().find(|p| p.id == r.panel).and_then(hopper::hopperd::Client::for_panel)
    });
    let web = srv.remote.as_ref().map(|r| format!("{}/#/server/{}/settings/general", r.base, hopper::hopperd::enc(&r.id)));
    let form = cx.new(|cx| {
        // Ask the panel who this PC is there before offering to delete.
        if let Some(c) = client {
            cx.spawn(async move |this: WeakEntity<DeleteForm>, cx: &mut AsyncApp| {
                let role = cx.background_spawn(async move { c.get("/auth/me").ok() }).await;
                let may = role.map(|v| hopper::hopperd::may_manage(&v));
                this.update(cx, |this, cx| {
                    // Unknown (an old panel): offer it; the panel still checks.
                    this.may = Some(may.unwrap_or(true));
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        let may = if panel.is_some() { None } else { Some(true) };
        DeleteForm { store: store.clone(), root: root.clone(), name: srv.name.clone(), panel, worlds, backups, keep_world: true, may, web, permanent: false }
    });
    window.open_dialog(cx, move |dialog, window, cx| {
        let ready = form.read(cx).ready(cx);
        let permanent = form.read(cx).permanent;
        let f = form.clone();
        dialog
            .title(format!("Delete {}?", form.read(cx).name))
            .w(rems(34.).to_pixels(window.rem_size()))
            .child(form.clone())
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("dialog-cancel").outline().label("Cancel")))
                    .child(
                        DialogAction::new().child(
                            Button::new("dialog-ok").danger().label(if permanent { "Delete permanently" } else { "Delete server" }).disabled(!ready),
                        ),
                    ),
            )
            .on_ok(move |_, window, cx| delete(&f, window, cx))
    });
}

/// Takes the folder of a server on this PC to the Recycle Bin, or deletes
/// it for good. A folder somebody has open is refused with the program's
/// name; for good, the folder is renamed away first so a locked one fails
/// before anything is removed.
fn delete_local(name: &str, root: &std::path::Path, permanent: bool) -> anyhow::Result<()> {
    use hopper::hosting::fsmove;
    if !permanent {
        return trash::delete(root).map_err(|e| {
            let who = fsmove::holders(root);
            if who.is_empty() {
                anyhow::anyhow!("Couldn't move {name} to the Recycle Bin: {e}. Nothing was deleted.")
            } else {
                anyhow::anyhow!(fsmove::locked_message(name, &who, &std::io::Error::from_raw_os_error(5)))
            }
        });
    }
    let parent = root.parent().ok_or_else(|| anyhow::anyhow!("{} has no parent folder", root.display()))?;
    let meta = std::fs::symlink_metadata(root)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        anyhow::bail!("{} is a link, not a folder: Hopper won't delete it for good.", root.display());
    }
    let folder = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let tmp = parent.join(format!("{}{stamp}-{folder}", fsmove::DELETING_PREFIX));
    fsmove::move_folder(name, root, &tmp, &|| {})?;
    fsmove::remove_tree(&tmp).map_err(|msg| anyhow::anyhow!("{name} is gone from Hopper, but not all of its files could be deleted: {msg}"))
}

/// Deletes it in a job (Tasks shows it), then lists the servers again.
fn delete(form: &Entity<DeleteForm>, window: &mut Window, cx: &mut App) -> bool {
    let f = form.read(cx);
    if !f.ready(cx) {
        return false;
    }
    let (root, name, keep, permanent) = (f.root.clone(), f.name.clone(), f.keep_world, f.permanent);
    let store = f.store.clone();
    let s = store.read(cx);
    let remote = s.servers().iter().find(|x| x.root == root).and_then(|x| x.remote.clone());
    let local = s.local_panel();
    let client = remote.as_ref().and_then(|r| {
        s.settings().hopper_panels.iter().find(|p| p.id == r.panel).and_then(hopper::hopperd::Client::for_panel)
    });
    if remote.is_some() && client.is_none() {
        window.push_notification(Notification::error("Pair with that Hopper Server again first."), cx);
        return false;
    }
    let job = s.start_job(format!("Deleting {name}"));
    cx.spawn(async move |cx| {
        let r = cx
            .background_spawn(async move {
                let r = (|| -> anyhow::Result<Option<String>> {
                    match (remote, client) {
                        (Some(rm), Some(c)) => {
                            job.detail("Asking the panel to delete it");
                            let v = c.post(
                                &hopper::hopperd::Client::server_path(&rm.id, "/remove"),
                                // Older panels still want the name typed to confirm.
                                serde_json::json!({ "mode": "delete", "confirm": name, "keep_world": keep, "permanent": permanent }),
                            )?;
                            if permanent && v["permanent"] != true {
                                job.note("This panel is too old to delete for good: it moved the server to its deleted servers (kept 30 days) instead.");
                            }
                            Ok(v["kept_world"].as_str().map(str::to_string))
                        }
                        _ => {
                            local.remove(&root)?;
                            let kept = if keep { hopper::hosting::worlds::keep_worlds(&root)? } else { None };
                            job.detail(if permanent { "Deleting the folder" } else { "Moving the folder to the Recycle Bin" });
                            if let Err(err) = delete_local(&name, &root, permanent) {
                                // Nothing is deleted: the worlds go back.
                                if let Some(k) = &kept {
                                    if let Err(e2) = hopper::hosting::worlds::unkeep(&root, k) {
                                        return Err(err.context(format!("The world couldn't be put back either ({e2}); it is in {}", k.display())));
                                    }
                                }
                                return Err(err);
                            }
                            Ok(kept.map(|p| p.display().to_string()))
                        }
                    }
                })();
                if let Ok(Some(k)) = &r {
                    job.note(format!("The world is kept in {k}."));
                }
                job.finish(&r);
                r
            })
            .await;
        if r.is_ok() {
            store.update(cx, |s, _| s.rediscover_servers());
        }
    })
    .detach();
    true
}
