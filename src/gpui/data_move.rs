//! Moving Hopper's files ([`hopper::paths`]) to another folder: from the old
//! AppData layout to the folder next to hopper.exe (offered once at start,
//! until "Don't ask again"), or anywhere the user picks, from Settings and
//! the setup. Hopper copies, restarts, then deletes the old folder.

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
use std::path::{Path, PathBuf};

/// The flag a restart after a move carries: the new run deletes the old
/// folder once the old run has let go of it.
pub const FINISH_ARG: &str = "--finish-data-move";
/// With [`FINISH_ARG`]: the folder moved away from (none: the AppData
/// layout).
const FROM_ARG: &str = "--moved-from=";

/// At start: after a move, clean up and say so; otherwise offer the move
/// when the old layout is in use.
pub fn on_start(app: &AppView, window: &mut Window, cx: &mut Context<AppView>) {
    if std::env::args().any(|a| a == FINISH_ARG) {
        let from = std::env::args().find_map(|a| a.strip_prefix(FROM_ARG).map(PathBuf::from));
        std::thread::spawn(move || hopper::paths::remove_old(from.as_deref()));
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
    let handle = window.window_handle();
    cx.spawn(async move |_, cx| {
        let found = cx.background_spawn(async { hopper::paths::move_offer() }).await;
        if let Some((size, dest)) = found {
            handle
                .update(cx, |_, window, cx| open(store, size, dest, true, window, cx))
                .ok();
        }
    })
    .detach();
}

/// A folder the user picks for Hopper's files: the folder itself when it's
/// empty, else a "Hopper Data" folder inside it.
pub fn pick_folder(
    prompt: &'static str,
    then: impl FnOnce(PathBuf, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let rx = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some(prompt.into()),
    });
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        if let Ok(Ok(Some(p))) = rx.await
            && let Some(p) = p.into_iter().next()
        {
            let dest = match std::fs::read_dir(&p).map(|mut r| r.next().is_none()) {
                Ok(true) => p,
                _ => p.join(hopper::paths::FOLDER),
            };
            handle.update(cx, |_, window, cx| then(dest, window, cx)).ok();
        }
    })
    .detach();
}

/// Asks for a folder, then confirms moving Hopper's files there.
pub fn pick(store: Entity<Store>, window: &mut Window, cx: &mut App) {
    pick_folder("Move Hopper's files here", move |dest, window, cx| confirm(store, dest, window, cx), window, cx);
}

/// Checks `dest` and the size of what moves, then asks.
pub fn confirm(store: Entity<Store>, dest: PathBuf, window: &mut Window, cx: &mut App) {
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let d2 = dest.clone();
        let r = cx
            .background_spawn(async move { hopper::paths::check_dest(&d2).map(|_| hopper::paths::used()) })
            .await;
        handle
            .update(cx, |_, window, cx| match r {
                Ok(size) => open(store, size, dest, false, window, cx),
                Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
            })
            .ok();
    })
    .detach();
}

/// The question. `legacy`: the offer at start, from the AppData layout,
/// with "Don't ask again".
fn open(store: Entity<Store>, size: u64, dest: PathBuf, legacy: bool, window: &mut Window, cx: &mut App) {
    let size = hopper::instance_ops::human_bytes(size);
    let now = hopper::paths::dirs()
        .map(|d| d.root().unwrap_or(d.data_dir()).to_path_buf())
        .unwrap_or_default();
    let (title, body) = if legacy {
        (
            "Keep all of Hopper in one folder?",
            format!(
                "Hopper keeps its settings, downloads and caches ({size}) in your Windows user folder (drive {}). Move them next to hopper.exe, so everything Hopper needs is in one folder on the drive you picked?",
                drive(&now)
            ),
        )
    } else {
        (
            "Move Hopper's files?",
            format!("Hopper's settings, downloads and caches ({size}) move from {} to the folder below.", now.display()),
        )
    };
    let dest_text = dest.display().to_string();
    window.open_dialog(cx, move |dialog, window, cx| {
        let theme = cx.theme();
        let (store_ok, store_no, dest_ok) = (store.clone(), store.clone(), dest.clone());
        dialog
            .title(title)
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
                            .child(div().min_w_0().child(dest_text.clone())),
                    )
                    .child(
                        widgets::caption(
                            "Instances and servers stay in their folders (see Settings). Hopper restarts when the move is done, then deletes the old copy.",
                            cx,
                        )
                        .text_xs(),
                    ),
            )
            .footer(
                DialogFooter::new()
                    .when(legacy, |f| {
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
                start(store_ok.clone(), dest_ok.clone(), Vec::new(), |_, _| {}, window, cx);
                true
            })
    });
}

fn drive(p: &Path) -> String {
    p.components()
        .next()
        .map(|c| c.as_os_str().to_string_lossy().trim_end_matches('\\').to_string())
        .unwrap_or_default()
}

/// Copies everything over as a task, then restarts into the new folder
/// with `args` added. `failed` runs when the copy fails.
pub fn start(
    store: Entity<Store>,
    dest: PathBuf,
    mut args: Vec<String>,
    failed: impl FnOnce(String, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let job = store.read(cx).jobs().start("Moving Hopper's files");
    let from = hopper::paths::dirs().and_then(|d| d.root()).map(Path::to_path_buf);
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let r = cx
            .background_spawn(async move {
                job.detail(format!("Copying to {}", dest.display()));
                let r = hopper::paths::move_to(&dest, |done, total| {
                    job.progress(done as usize, total as usize)
                });
                job.finish(&r);
                r.map_err(|e| format!("{e:#}"))
            })
            .await;
        handle
            .update(cx, |_, window, cx| match r {
                Ok(()) => {
                    args.insert(0, FINISH_ARG.to_string());
                    if let Some(f) = &from {
                        args.insert(1, format!("{FROM_ARG}{}", f.display()));
                    }
                    restart(&args, cx)
                }
                Err(e) => {
                    window.push_notification(
                        Notification::error(format!("Hopper's files weren't moved: {e}")),
                        cx,
                    );
                    failed(e, cx);
                }
            })
            .ok();
    })
    .detach();
}

fn restart(args: &[String], cx: &mut App) {
    match std::env::current_exe().and_then(|exe| std::process::Command::new(exe).args(args).spawn()) {
        Ok(_) => cx.quit(),
        Err(e) => log::error!("restarting after the move: {e}"),
    }
}
