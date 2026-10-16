//! An instance's backups: what was removed or replaced, by when, to restore
//! or delete for good.

use crate::store::{Msg, Store, open_path};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::backups::{Saved, Set};
use std::collections::HashSet;
use std::path::PathBuf;

pub struct BackupsView {
    store: Entity<Store>,
    root: PathBuf,
    sets: Vec<Set>,
    /// Backups shown open (by folder).
    open: HashSet<PathBuf>,
}

impl BackupsView {
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.sets = hopper::backups::list(&self.root);
        cx.notify();
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        self.store.update(cx, |s, cx| {
            s.spawn(move || Msg::InstanceChanged(root));
            cx.notify();
        });
        self.reload(cx);
    }

    fn restore(&mut self, files: Vec<Saved>, window: &mut Window, cx: &mut Context<Self>) {
        let mut ok = 0;
        let mut renamed = 0;
        let mut errors = Vec::new();
        for f in &files {
            match hopper::backups::restore(&self.root, f) {
                Ok(p) => {
                    ok += 1;
                    if !self.root.join(&f.restore_to).eq(&p) {
                        renamed += 1;
                    }
                }
                Err(e) => errors.push(format!("{}: {e:#}", f.name)),
            }
        }
        let mut msg = match ok {
            0 => String::new(),
            1 => format!("Restored {}", files[0].name),
            n => format!("Restored {n} files"),
        };
        if renamed > 0 {
            msg.push_str(&format!(
                " ({renamed} already there, so the restored copy has \"(restored)\" in its name)"
            ));
        }
        if !msg.is_empty() {
            window.push_notification(Notification::success(msg), cx);
        }
        for e in errors {
            window.push_notification(Notification::error(e), cx);
        }
        self.changed(cx);
    }

    fn ask_delete(&mut self, set: Set, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.entity();
        let n = set.files.len();
        let permanent = self.store.read(cx).settings().delete_permanently;
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (view, set) = (view.clone(), set.clone());
            alert
                .title("Delete this backup?")
                .description(format!(
                    "Its {n} file{} {}.",
                    if n == 1 { "" } else { "s" },
                    if permanent { "are deleted for good" } else { "go to the Recycle Bin" }
                ))
                .button_props(
                    gpui_kit::component::dialog::DialogButtonProps::default()
                        .ok_text("Delete")
                        .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    let r = hopper::backups::delete(&set, permanent);
                    view.update(cx, |v, cx| {
                        if let Err(e) = r {
                            window.push_notification(Notification::error(format!("{e:#}")), cx);
                        }
                        v.reload(cx);
                    });
                    true
                })
        });
    }
}

fn when(secs: i64) -> String {
    hopper::time::relative_secs(secs)
}

impl Render for BackupsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let total_h = (window.viewport_size().height - window.rem_size() * 8.).max(window.rem_size() * 16.);
        let size = hopper::instance_ops::human_bytes;
        let body: AnyElement = if self.sets.is_empty() {
            widgets::caption("No backups. Files you remove with Move to backups show up here.", cx).into_any_element()
        } else {
            v_flex()
                .gap_3()
                .children(self.sets.iter().enumerate().map(|(i, set)| {
                    let open = self.open.contains(&set.dir) || i == 0;
                    let (d1, d2, d3) = (set.dir.clone(), set.dir.clone(), set.clone());
                    let all = set.files.clone();
                    let n = set.files.len();
                    v_flex()
                        .gap_1()
                        .p_3()
                        .rounded(theme.radius_lg)
                        .border_1()
                        .border_color(theme.border)
                        .child(
                            h_flex()
                                .id(("backup-set", i))
                                .gap_2()
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if !this.open.remove(&d1) {
                                        this.open.insert(d1.clone());
                                    }
                                    cx.notify();
                                }))
                                .child(
                                    gpui_kit::component::Icon::new(if open {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    })
                                    .small(),
                                )
                                .child(div().font_semibold().child(when(set.when)))
                                .child(
                                    widgets::caption(
                                        format!("{n} file{} · {}", if n == 1 { "" } else { "s" }, size(set.size())),
                                        cx,
                                    )
                                    .text_xs(),
                                )
                                .child(div().flex_1())
                                .child(
                                    Button::new(("backup-restore-all", i))
                                        .small()
                                        .outline()
                                        .icon(IconName::Undo2)
                                        .label(if n == 1 { "Restore" } else { "Restore all" })
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.restore(all.clone(), window, cx)
                                        })),
                                )
                                .child(
                                    Button::new(("backup-folder", i))
                                        .small()
                                        .ghost()
                                        .icon(IconName::FolderOpen)
                                        .tooltip("Open this backup's folder")
                                        .on_click(move |_, _, _| open_path(&d2)),
                                )
                                .child(
                                    Button::new(("backup-delete", i))
                                        .small()
                                        .ghost()
                                        .icon(IconName::Trash)
                                        .tooltip("Delete this backup (to the Recycle Bin)")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.ask_delete(d3.clone(), window, cx)
                                        })),
                                ),
                        )
                        .when(open, |c| {
                            c.children(set.files.iter().enumerate().map(|(j, f)| {
                                let file = f.clone();
                                h_flex()
                                    .id(("backup-file", i * 10_000 + j))
                                    .gap_2()
                                    .pl_6()
                                    .py_1()
                                    .rounded(theme.radius)
                                    .hover(|s| s.bg(theme.list_hover))
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .child(div().text_sm().truncate().child(f.name.clone()))
                                            .child(
                                                widgets::caption(
                                                    format!("back to {} · {}", f.restore_to, size(f.size)),
                                                    cx,
                                                )
                                                .text_xs()
                                                .truncate(),
                                            ),
                                    )
                                    .child(
                                        Button::new(("backup-restore", i * 10_000 + j))
                                            .xsmall()
                                            .outline()
                                            .label("Restore")
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.restore(vec![file.clone()], window, cx)
                                            })),
                                    )
                            }))
                        })
                }))
                .into_any_element()
        };
        v_flex()
            .when(!self.sets.is_empty(), |c| c.h(total_h))
            .gap_3()
            .child(widgets::caption(
                "Files Hopper set aside: mods and packs you removed, and files replaced by updates and syncs. Newest first.",
                cx,
            ))
            .child(div().id("backups-list").flex_1().min_h_0().overflow_y_scrollbar().child(body))
    }
}

/// Opens an instance's backups.
pub fn open(store: Entity<Store>, root: PathBuf, window: &mut Window, cx: &mut App) {
    let view = cx.new(|_| BackupsView {
        sets: hopper::backups::list(&root),
        store,
        root,
        open: HashSet::new(),
    });
    window.open_dialog(cx, move |dialog, window, _| {
        dialog
            .title("Backups")
            .w(rems(42.).to_pixels(window.rem_size()))
            .child(view.clone())
    });
}
