//! Moving Hopper's files from the old AppData layout into one folder next
//! to hopper.exe ([`hopper::paths`]): offered once at start (until "Don't
//! ask again"), and from Settings > Advanced any time.

use crate::app::AppView;
use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::PathBuf;

/// The flag a restart after a move carries: the new run removes the old
/// folders once the old run has let go of them.
pub const FINISH_ARG: &str = "--finish-data-move";

/// At start: after a move, clean up and say so; otherwise offer the move
/// when the old layout is in use.
pub fn on_start(app: &AppView, window: &mut Window, cx: &mut Context<AppView>) {
    if std::env::args().any(|a| a == FINISH_ARG) {
        std::thread::spawn(hopper::paths::remove_old_layout);
        if let Some(root) = hopper::paths::dirs().and_then(|d| d.root()) {
            let msg = format!("Hopper's files now live in {}", root.display());
            cx.defer_in(window, move |_, window, cx| {
                window.push_notification(Notification::success(msg), cx)
            });
        }
        return;
    }
    let store = app.store.clone();
    {
        let s = store.read(cx).settings();
        if s.data_move_declined || s.needs_setup() {
            return;
        }
    }
    offer(store, false, window, cx);
}

/// Looks up the old folders' size, then asks. `asked` is a click in
/// Settings: no "Don't ask again", and a note when there's nothing to move.
pub fn offer(store: Entity<Store>, asked: bool, window: &mut Window, cx: &mut App) {
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let found = cx.background_spawn(async { hopper::paths::move_offer() }).await;
        handle
            .update(cx, |_, window, cx| match found {
                Some((size, dest)) => open(store, size, dest, asked, window, cx),
                None if asked => window.push_notification(
                    Notification::warning("Hopper can't write next to hopper.exe, so its files stay where they are."),
                    cx,
                ),
                None => {}
            })
            .ok();
    })
    .detach();
}

fn open(store: Entity<Store>, size: u64, dest: PathBuf, asked: bool, window: &mut Window, cx: &mut App) {
    let size = hopper::instance_ops::human_bytes(size);
    let drive = |p: &std::path::Path| {
        p.components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().trim_end_matches('\\').to_string())
            .unwrap_or_default()
    };
    let from = hopper::paths::old_layout()
        .map(|d| drive(d.data_dir()))
        .unwrap_or_default();
    let body = format!(
        "Hopper keeps its settings, downloads and caches ({size}) in your Windows user folder (drive {from}). Move them next to hopper.exe, so everything Hopper needs is in one folder on the drive you picked?"
    );
    let dest_text = dest.display().to_string();
    window.open_dialog(cx, move |dialog, window, cx| {
        let theme = cx.theme();
        let (store_ok, store_no, dest_ok) = (store.clone(), store.clone(), dest.clone());
        dialog
            .title("Keep all of Hopper in one folder?")
            .w(rems(36.).to_pixels(window.rem_size()))
            .child(
                v_flex()
                    .gap_3()
                    .child(div().text_sm().child(body.clone()))
                    .child(
                        h_flex()
                            .gap_2()
                            .px_3()
                            .py_2()
                            .rounded(theme.radius)
                            .bg(theme.muted)
                            .text_sm()
                            .child(Icon::new(IconName::FolderOpen).small().text_color(theme.muted_foreground))
                            .child(div().min_w_0().truncate().child(dest_text.clone())),
                    )
                    .child(
                        widgets::caption(
                            "Instances and servers stay in their folders (see Settings). Hopper restarts when the move is done.",
                            cx,
                        )
                        .text_xs(),
                    ),
            )
            .footer(
                DialogFooter::new()
                    .when(!asked, |f| {
                        f.child(
                            Button::new("move-never")
                                .ghost()
                                .label("Don't ask again")
                                .on_click(move |_, window, cx| {
                                    store_no.update(cx, |s, cx| {
                                        s.update_settings(|st| st.data_move_declined = true, cx)
                                    });
                                    window.close_dialog(cx);
                                }),
                        )
                    })
                    .child(DialogClose::new().child(Button::new("move-later").outline().label("Not now")))
                    .child(DialogAction::new().child(
                        Button::new("move-go").primary().icon(IconName::FolderOpen).label("Move and restart"),
                    )),
            )
            .on_ok(move |_, window, cx| {
                start(store_ok.clone(), dest_ok.clone(), window, cx);
                true
            })
    });
}

/// Copies everything over as a task, then restarts into the new folder.
fn start(store: Entity<Store>, dest: PathBuf, window: &mut Window, cx: &mut App) {
    let job = store.read(cx).jobs().start("Moving Hopper's files");
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let r = cx
            .background_spawn(async move {
                job.detail(format!("Copying to {}", dest.display()));
                let r = hopper::paths::move_old_layout(&dest, |done, total| {
                    job.progress(done as usize, total as usize)
                });
                job.finish(&r);
                r.map_err(|e| format!("{e:#}"))
            })
            .await;
        handle
            .update(cx, |_, window, cx| match r {
                Ok(()) => restart(cx),
                Err(e) => window.push_notification(
                    Notification::error(format!("Hopper's files weren't moved: {e}")),
                    cx,
                ),
            })
            .ok();
    })
    .detach();
}

fn restart(cx: &mut App) {
    match std::env::current_exe().and_then(|exe| std::process::Command::new(exe).arg(FINISH_ARG).spawn()) {
        Ok(_) => cx.quit(),
        Err(e) => log::error!("restarting after the move: {e}"),
    }
}
