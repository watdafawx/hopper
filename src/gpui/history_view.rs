//! The History tab of an instance's History dialog: what was removed,
//! updated, rolled back or edited, newest first, each with Undo. Undo
//! downloads a mod again (`hopper::undo`); when that version is gone from
//! its site, a picker offers the ones it has.

use crate::backups_view::BackupsView;
use crate::widgets;
use crate::widgets::Clickable as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::backups::{Change, Saved, Set};
use hopper::undo::Gone;
use std::rc::Rc;

/// The day header a set goes under ("Today", "Yesterday", a date).
fn day_of(when: i64) -> String {
    hopper::time::relative_secs(when)
}

/// "14:02".
fn hm(when: i64) -> String {
    let c = hopper::time::clock(when);
    c.get(..5).unwrap_or(&c).to_string()
}

impl BackupsView {
    /// History, grouped by day.
    pub(crate) fn history_body(&self, cx: &mut Context<Self>) -> AnyElement {
        if !self.sets_loaded {
            return widgets::skeleton_rows(4, rems(2.5), cx).into_any_element();
        }
        if self.sets.is_empty() {
            return widgets::empty_state(
                IconName::Undo2,
                "Nothing yet",
                Some("Mods you remove, update or roll back, and configs Hopper changes, show up here with Undo.".into()),
                cx,
            )
            .into_any_element();
        }
        let mut col = v_flex().gap_0p5();
        let mut day = String::new();
        for (i, set) in self.sets.iter().enumerate() {
            let d = day_of(set.when);
            if d != day {
                col = col.child(
                    div()
                        .when(!day.is_empty(), |d| d.pt_3())
                        .pb_1()
                        .text_xs()
                        .font_semibold()
                        .text_color(cx.theme().muted_foreground)
                        .child(d.to_uppercase()),
                );
                day = d;
            }
            col = col.child(self.set_rows(i, set, cx));
        }
        col.into_any_element()
    }

    /// One action: a row, or a fold of rows when it touched several files.
    fn set_rows(&self, i: usize, set: &Set, cx: &mut Context<Self>) -> AnyElement {
        if let [f] = set.files.as_slice() {
            return self.entry_row(i * 10_000, f, set.when, false, cx).into_any_element();
        }
        let open = self.open.contains(&set.dir);
        let dir = set.dir.clone();
        let all = set.files.clone();
        let n = set.files.len();
        let mods = set.files.iter().filter(|f| f.src.is_some()).count();
        let what = if mods == n { format!("{n} mods") } else { format!("{n} files") };
        // "Removed 3 mods" when they all went the same way.
        let same = set.files.iter().all(|f| f.change == set.files[0].change);
        let label = if same { set.files[0].change.label() } else { "Changed" };
        let busy = self.busy.is_some();
        v_flex()
            .child(
                h_flex()
                    .id(("hist-set", i))
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .rounded(cx.theme().radius)
                    .hover(|s| widgets::row_hover(s, cx))
                    .clickable(cx)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.open.remove(&dir) {
                            this.open.insert(dir.clone());
                        }
                        cx.notify();
                    }))
                    .child(
                        div()
                            .size(rems(1.5))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(gpui_kit::component::Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).small()),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .gap_1p5()
                                    .text_sm()
                                    .child(div().text_color(change_color(set.files[0].change, same, cx)).child(label))
                                    .child(what),
                            )
                            .child(
                                widgets::caption(
                                    set.files.iter().take(3).map(Saved::title).collect::<Vec<_>>().join(", ")
                                        + if n > 3 { "…" } else { "" },
                                    cx,
                                )
                                .text_xs()
                                .truncate(),
                            ),
                    )
                    .child(widgets::caption(hm(set.when), cx).text_xs())
                    .child(
                        widgets::row_actions(("hist-set-actions", i)).child(
                            Button::new(("hist-undo-all", i))
                                .xsmall()
                                .outline()
                                .label("Undo all")
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, window, cx| this.undo(all.clone(), window, cx))),
                        ),
                    ),
            )
            .when(open, |c| {
                c.children(set.files.iter().enumerate().map(|(j, f)| self.entry_row(i * 10_000 + j + 1, f, set.when, true, cx)))
            })
            .into_any_element()
    }

    /// One file: what happened to it, its versions, when, and Undo.
    fn entry_row(&self, key: usize, f: &Saved, when: i64, nested: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let (pid, icon, name) = match &f.src {
            Some(r) => {
                let known = self.project_of(&f.name, cx);
                let icon = r.addon.as_ref().map(|a| a.icon.clone()).filter(|s| !s.is_empty()).or_else(|| known.clone().map(|k| k.1)).unwrap_or_default();
                let name = if r.name.is_empty() { f.name.clone() } else { r.name.clone() };
                (r.project_id, icon, name)
            }
            None => {
                let (pid, icon) = self.project_of(&f.name, cx).unwrap_or_default();
                (pid, icon, f.name.clone())
            }
        };
        let file = f.clone();
        let busy = self.busy.is_some();
        h_flex()
            .id(("hist-row", key))
            .gap_2()
            .px_2()
            .py_1p5()
            .when(nested, |r| r.pl_8())
            .rounded(cx.theme().radius)
            .hover(|s| widgets::row_hover(s, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .min_w_0()
                            .text_sm()
                            .child(div().flex_none().text_color(change_color(f.change, true, cx)).child(f.change.label()))
                            .child(self.mod_cell(ElementId::Name(format!("hist-mod:{key}").into()), name, pid, &icon, true, cx)),
                    )
                    .child(widgets::caption(f.detail(), cx).text_xs().truncate()),
            )
            .when(!nested, |r| r.child(widgets::caption(hm(when), cx).text_xs()))
            .child(
                Button::new(("hist-undo", key))
                    .xsmall()
                    .outline()
                    .label("Undo")
                    .disabled(busy)
                    .on_click(cx.listener(move |this, _, window, cx| this.undo(vec![file.clone()], window, cx))),
            )
    }

    /// Undoes entries, newest set first. A mod whose version its site no
    /// longer has opens the version picker.
    pub(crate) fn undo(&mut self, files: Vec<Saved>, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let s = self.store.read(cx);
        if let Some(why) = s.edit_hold_reason(&root) {
            window.push_notification(Notification::error(format!("Can't undo now: {why}.")), cx);
            return;
        }
        let Some(mut inst) = s.instance(&root).cloned() else {
            window.push_notification(Notification::error("That instance is gone."), cx);
            return;
        };
        let downloads = files.iter().any(|f| f.download);
        self.busy = Some(if downloads { "Downloading to put it back…".into() } else { "Putting it back…".into() });
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let (done, gone, errors) = cx
                .background_spawn(async move {
                    let mut done = Vec::new();
                    let mut gone: Option<(Saved, Gone)> = None;
                    let mut errors = Vec::new();
                    for f in &files {
                        match hopper::undo::undo(&mut inst, f) {
                            Ok(p) => done.push(p),
                            Err(e) => match e.downcast::<Gone>() {
                                Ok(g) if gone.is_none() => gone = Some((f.clone(), g)),
                                Ok(g) => errors.push(format!("{}: {g}", f.name)),
                                Err(e) => errors.push(format!("{}: {e:#}", f.name)),
                            },
                        }
                    }
                    (done, gone, errors)
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                this.after_undo(&done, window, cx);
                for e in errors {
                    window.push_notification(Notification::error(e), cx);
                }
                if let Some((f, g)) = gone {
                    this.pick_version(f, g, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Tells the store and the user what came back.
    fn after_undo(&mut self, done: &[std::path::PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let mods = root.join("mods");
        // Jars with no entry (unknown ones): identified, as on any drop.
        let jars: Vec<String> = done
            .iter()
            .filter(|p| p.parent() == Some(mods.as_path()))
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        self.store.update(cx, |s, cx| {
            s.reload_instance(&root, cx);
            s.identify_jars(&root, jars);
        });
        match done {
            [] => {}
            [one] => {
                let n = one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                window.push_notification(Notification::success(format!("Put back {n}")), cx)
            }
            many => window.push_notification(Notification::success(format!("Put back {} files", many.len())), cx),
        }
        self.changed(cx);
    }

    /// The version this entry was is gone from its site: offers the ones it
    /// has, the closest picked.
    fn pick_version(&mut self, f: Saved, g: Gone, window: &mut Window, cx: &mut Context<Self>) {
        if g.choices.is_empty() {
            let msg = format!("{}. It has no other version for this Minecraft and loader, so it can't be put back.", g);
            window.push_notification(Notification::error(format!("{}: {msg}", g.name)), cx);
            return;
        }
        let view = cx.entity();
        let picked = Rc::new(std::cell::Cell::new(g.closest));
        let pid = f.src.as_ref().map_or(0, |r| r.project_id);
        let icon = f.src.as_ref().and_then(|r| r.addon.as_ref()).map(|a| a.icon.clone()).unwrap_or_default();
        window.open_dialog(cx, move |dialog, window, cx| {
            let theme = cx.theme().clone();
            let now = picked.get();
            let label = |file: &hopper::cf::File| hopper::undo::version_of(&file.file_name);
            let ok_label = format!("Put back {}", g.choices.get(now).map(label).unwrap_or_default());
            let (view2, f2, files, p2) = (view.clone(), f.clone(), g.choices.clone(), picked.clone());
            let title = view.update(cx, |v, cx| {
                h_flex()
                    .gap_1p5()
                    .child("Put back")
                    .child(v.mod_cell("pick-title".into(), g.name.clone(), pid, &icon, false, cx))
            });
            dialog
                .title(title)
                .w(rems(30.).to_pixels(window.rem_size()))
                .margin_top(rems(crate::widgets::DIALOG_TOP).to_pixels(window.rem_size()))
                .child(
                    v_flex()
                        .gap_3()
                        .child(div().text_sm().child(format!("{}. Pick another for this instance:", g)))
                        .child(
                            v_flex()
                                .id("pick-list")
                                .max_h(rems(18.))
                                .overflow_y_scroll()
                                .rounded(theme.radius)
                                .border_1()
                                .border_color(theme.border)
                                .children(g.choices.iter().enumerate().map(|(i, file)| {
                                    let p = picked.clone();
                                    let hint = if i == g.closest {
                                        "closest".to_string()
                                    } else if i == 0 {
                                        "newest".to_string()
                                    } else {
                                        String::new()
                                    };
                                    let date = hopper::time::date_of(&file.file_date);
                                    let row = h_flex()
                                        .id(("pick", i))
                                        .gap_2()
                                        .px_3()
                                        .py_1p5()
                                        .text_sm()
                                        .clickable(cx)
                                        .hover(|s| widgets::row_hover(s, cx))
                                        .on_click(move |_, window, _| {
                                            p.set(i);
                                            window.refresh();
                                        })
                                        .child(div().flex_1().min_w_0().truncate().child(label(file)))
                                        .child(widgets::caption([hint, date].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · "), cx).text_xs());
                                    if i == now { widgets::row_selected(row, cx) } else { row }
                                })),
                        ),
                )
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().child(Button::new("pick-cancel").ghost().label("Cancel")))
                        .child(DialogAction::new().child(Button::new("pick-ok").primary().label(ok_label))),
                )
                .on_ok(move |_, _, cx| {
                    let Some(file) = files.get(p2.get()).cloned() else { return true };
                    view2.update(cx, |v, cx| v.put_back_as(f2.clone(), file, cx));
                    true
                })
        });
    }

    /// Puts another version back in place of a gone one.
    fn put_back_as(&mut self, f: Saved, file: hopper::cf::File, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let s = self.store.read(cx);
        let Some(mut inst) = s.instance(&root).cloned() else { return };
        let name = f.src.as_ref().map(|r| r.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| f.name.clone());
        let r2 = root.clone();
        s.spawn_instance_job(&root, format!("Putting back {name}"), move |job| {
            hopper::undo::put_back_as(&mut inst, &f, &file, Some(job))?;
            job.detail(format!("Put back {}", file.file_name));
            Ok(crate::store::Msg::InstanceChanged(r2))
        });
        // The entry leaves History once the job is done.
        let view = cx.entity();
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(3)).await;
            let _ = view.update(cx, |v, cx| v.changed(cx));
        })
        .detach();
    }

    /// The one-time "free space" offer: old jar copies the sites still hand
    /// out become notes. Asked first, never done by itself.
    pub(crate) fn free_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let bytes = self.copies.filter(|b| *b >= 1024 * 1024)?;
        let theme = cx.theme();
        let size = hopper::instance_ops::human_bytes(bytes);
        Some(
            h_flex()
                .gap_3()
                .px_3()
                .py_2()
                .rounded(theme.radius)
                .bg(theme.warning.opacity(0.10))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .child(format!("Older History keeps full copies of mods ({size}). Most can be downloaded again instead.")),
                )
                .child(
                    div().flex_none().child(
                        Button::new("hist-free")
                            .small()
                            .primary()
                            .label("Free up space")
                            .disabled(self.busy.is_some())
                            .on_click(cx.listener(|this, _, window, cx| this.ask_free(window, cx))),
                    ),
                )
                .into_any_element(),
        )
    }

    fn ask_free(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.entity();
        let size = hopper::instance_ops::human_bytes(self.copies.unwrap_or(0));
        window.open_alert_dialog(cx, move |alert, _, _| {
            let view = view.clone();
            alert
                .title("Remove old copies of mods?")
                .description(format!(
                    "History holds {size} of mod copies from older versions of Hopper. The ones CurseForge or Modrinth still hand out are removed; History keeps where to download each, so Undo still works. Copies with no download source stay."
                ))
                .button_props(
                    gpui_kit::component::dialog::DialogButtonProps::default()
                        .ok_text("Free up space")
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    view.update(cx, |v, cx| v.free_space(window, cx));
                    true
                })
        });
    }

    fn free_space(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.root.clone();
        self.busy = Some("Checking which mods can be downloaded again…".into());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let (freed, kept) = cx.background_spawn(async move { hopper::backups::free_space(&root) }).await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                let size = hopper::instance_ops::human_bytes;
                let msg = match (freed, kept) {
                    (0, _) => "None of the copies can be downloaded again, so they stay.".to_string(),
                    (f, 0) => format!("Freed {}", size(f)),
                    (f, k) => format!("Freed {}. {} stays: those mods have no download source.", size(f), size(k)),
                };
                window.push_notification(Notification::success(msg), cx);
                this.reload(cx);
            })
            .ok();
        })
        .detach();
    }
}

fn change_color(c: Change, same: bool, cx: &App) -> Hsla {
    let t = cx.theme();
    if !same {
        return t.muted_foreground;
    }
    match c {
        Change::Removed => t.danger,
        Change::Updated => t.info,
        Change::RolledBack => t.warning,
        Change::Edited | Change::Switched | Change::Replaced => t.primary,
    }
}
