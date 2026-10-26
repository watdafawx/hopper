//! An instance's backups: snapshots (the whole instance at one moment, to
//! compare with now and restore) and loose files (what was removed or
//! replaced, by when, to restore or delete for good).

use crate::widgets::Clickable as _;
use crate::store::{Msg, Store, open_path};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::backups::{Saved, Set};
use hopper::snapshots::{Changes, Kind, Reason, Snapshot};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Snapshots,
    Loose,
}

/// Changes listed under an open snapshot before "Show all".
const SHOWN: usize = 6;

pub struct BackupsView {
    store: Entity<Store>,
    root: PathBuf,
    sets: Vec<Set>,
    /// Backups shown open (by folder).
    open: HashSet<PathBuf>,
    mode: Mode,
    /// `None` while loading.
    snaps: Option<Vec<Snapshot>>,
    /// Disk the snapshots take.
    used: u64,
    /// Snapshots shown open, and those showing every change (by id).
    open_snaps: HashSet<String>,
    all: HashSet<String>,
    /// Each snapshot compared with now; absent while comparing.
    changes: HashMap<String, Result<Changes, String>>,
    comparing: HashSet<String>,
    /// What's running (a snapshot or restore), shown instead of the intro.
    busy: Option<String>,
    /// The snapshot to show open the first time the list loads, instead of
    /// the newest (a task's "View snapshot").
    focus: Option<String>,
}

impl BackupsView {
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.sets = hopper::backups::list(&self.root);
        cx.notify();
    }

    fn reload_snaps(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        cx.spawn(async move |this, cx| {
            let (snaps, used) = cx
                .background_spawn(async move { (hopper::snapshots::list(&root), hopper::snapshots::used_bytes(&root)) })
                .await;
            this.update(cx, |this, cx| {
                // Now has likely moved on: compare again.
                this.changes.clear();
                this.comparing.clear();
                let first = match this.focus.take() {
                    Some(f) if snaps.iter().any(|s| s.id == f) => Some(f),
                    _ => snaps.first().map(|s| s.id.clone()),
                };
                this.snaps = Some(snaps);
                this.used = used;
                if let Some(id) = first {
                    this.open_snaps.insert(id.clone());
                }
                for id in this.open_snaps.clone() {
                    this.compare(&id, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn snap(&self, id: &str) -> Option<Snapshot> {
        self.snaps.as_ref()?.iter().find(|s| s.id == id).cloned()
    }

    /// Compares a snapshot with now, in the background.
    fn compare(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.changes.contains_key(id) || !self.comparing.insert(id.to_string()) {
            return;
        }
        let Some(snap) = self.snap(id) else { return };
        let (root, id) = (self.root.clone(), id.to_string());
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move { hopper::snapshots::compare(&root, &snap).map_err(|e| format!("{e:#}")) })
                .await;
            this.update(cx, |this, cx| {
                if this.comparing.remove(&id) {
                    this.changes.insert(id, r);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        self.store.update(cx, |s, cx| {
            let r = root.clone();
            s.spawn(move || Msg::InstanceChanged(r));
            cx.notify();
        });
        // Files went back: once the instance has reloaded, the update list
        // must match them again (as after Undo of an update).
        let store = self.store.clone();
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(1500)).await;
            store.update(cx, |s, _| s.check_updates(&root, true));
        })
        .detach();
        self.reload(cx);
    }

    fn game_running(&self, cx: &App) -> bool {
        self.store.read(cx).running(&self.root).is_some()
    }

    fn take_snapshot(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some("Saving a snapshot…".into());
        cx.notify();
        let root = self.root.clone();
        cx.spawn_in(window, async move |this, cx| {
            let r = cx.background_spawn(async move { hopper::snapshots::take(&root, Reason::Manual) }).await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match r {
                    Ok(s) => {
                        this.open_snaps.insert(s.id);
                        window.push_notification(Notification::success("Snapshot saved"), cx);
                    }
                    Err(e) => window.push_notification(Notification::error(format!("Snapshot: {e:#}")), cx),
                }
                this.reload_snaps(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Restore, after showing what it changes.
    fn ask_restore(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(snap) = self.snap(&id) else { return };
        match self.changes.get(&id).cloned() {
            Some(Ok(c)) => self.confirm_restore(snap, c, window, cx),
            Some(Err(e)) => window.push_notification(Notification::error(format!("Couldn't compare with now: {e}")), cx),
            None => {
                // Compare first, then ask.
                self.busy = Some("Comparing with now…".into());
                cx.notify();
                let root = self.root.clone();
                cx.spawn_in(window, async move |this, cx| {
                    let s2 = snap.clone();
                    let r = cx
                        .background_spawn(async move { hopper::snapshots::compare(&root, &s2).map_err(|e| format!("{e:#}")) })
                        .await;
                    this.update_in(cx, |this, window, cx| {
                        this.busy = None;
                        this.comparing.remove(&id);
                        this.changes.insert(id.clone(), r.clone());
                        match r {
                            Ok(c) => this.confirm_restore(snap, c, window, cx),
                            Err(e) => window.push_notification(Notification::error(format!("Couldn't compare with now: {e}")), cx),
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
        }
    }

    fn confirm_restore(&mut self, snap: Snapshot, c: Changes, window: &mut Window, cx: &mut Context<Self>) {
        if c.is_empty() {
            window.push_notification(Notification::info("Nothing to restore: the instance is the same as this snapshot."), cx);
            return;
        }
        let view = cx.entity();
        let (back, gone, again) = (c.mods_of(Kind::Changed), c.mods_of(Kind::Added), c.mods_of(Kind::Removed));
        let mut mods = Vec::new();
        if back > 0 {
            mods.push(format!("{back} back to their old version"));
        }
        if gone > 0 {
            mods.push(format!("{gone} removed"));
        }
        if again > 0 {
            mods.push(format!("{again} put back"));
        }
        let mods = if mods.is_empty() { "No changes".to_string() } else { mods.join(", ") };
        let files = c.files.len();
        let files = match files {
            0 => "No changes".to_string(),
            1 => "1 file back to the old copy".to_string(),
            n => format!("{n} files back to the old copy"),
        };
        let total = c.total() + usize::from(c.meta);
        let title = snap.label();
        let when = hopper::snapshots::when_label(snap.when);
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let theme = cx.theme();
            let line = |k: &'static str, v: String| {
                h_flex()
                    .gap_3()
                    .child(div().w(rems(5.)).flex_none().text_color(theme.muted_foreground).child(k))
                    .child(div().flex_1().min_w_0().child(v))
            };
            let (view, id) = (view.clone(), snap.id.clone());
            alert
                .title(format!("Restore \"{title}\"?"))
                .description(
                    v_flex()
                        .gap_2()
                        .text_sm()
                        .child(format!("{when}. Your instance goes back to how it was then."))
                        .child(
                            v_flex()
                                .gap_1()
                                .child(line("Mods", mods.clone()))
                                .child(line("Configs", files.clone()))
                                .child(line("Worlds", "Never part of snapshots".into())),
                        )
                        .child(div().text_color(theme.muted_foreground).child(
                            "A \"Before restoring\" snapshot is taken first, so this can be undone. Close the game before restoring.",
                        )),
                )
                .confirm()
                .ok_text(format!("Restore {total} change{}", if total == 1 { "" } else { "s" }))
                .on_ok(move |_, window, cx| {
                    view.update(cx, |this, cx| this.restore_snapshot(id.clone(), window, cx));
                    true
                })
        });
    }

    fn restore_snapshot(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        if self.game_running(cx) {
            window.push_notification(Notification::error("Close the game before restoring a snapshot."), cx);
            return;
        }
        let label = self.snap(&id).map(|s| s.label()).unwrap_or_default();
        self.busy = Some(format!("Restoring \"{label}\"…"));
        cx.notify();
        let root = self.root.clone();
        cx.spawn_in(window, async move |this, cx| {
            let r = cx.background_spawn(async move { hopper::snapshots::restore(&root, &id) }).await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match r {
                    Ok(r) => window.push_notification(
                        Notification::success(format!(
                            "Restored \"{label}\" ({} file{}). To undo, restore \"{}\".",
                            r.changed,
                            if r.changed == 1 { "" } else { "s" },
                            r.safety.label()
                        )),
                        cx,
                    ),
                    Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
                }
                this.changed(cx);
                this.reload_snaps(cx);
            })
            .ok();
        })
        .detach();
    }

    fn ask_delete_snapshot(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(snap) = self.snap(&id) else { return };
        let view = cx.entity();
        let title = snap.label();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (view, id) = (view.clone(), id.clone());
            alert
                .title(format!("Delete \"{title}\"?"))
                .description("Files only this snapshot keeps are deleted for good. The instance itself isn't touched.")
                .button_props(
                    gpui_kit::component::dialog::DialogButtonProps::default()
                        .ok_text("Delete")
                        .ok_variant(ButtonVariant::Danger)
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    let id = id.clone();
                    view.update(cx, |this, cx| {
                        let root = this.root.clone();
                        cx.spawn_in(window, async move |this, cx| {
                            let r = cx.background_spawn(async move { hopper::snapshots::delete(&root, &id) }).await;
                            this.update_in(cx, |this, window, cx| {
                                if let Err(e) = r {
                                    window.push_notification(Notification::error(format!("{e:#}")), cx);
                                }
                                this.reload_snaps(cx);
                            })
                            .ok();
                        })
                        .detach();
                    });
                    true
                })
        });
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
                        .ok_variant(ButtonVariant::Danger)
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

    /// The snapshots list.
    fn snapshots_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let size = hopper::instance_ops::human_bytes;
        let Some(snaps) = &self.snaps else {
            return widgets::skeleton_rows(3, rems(3.5), cx).into_any_element();
        };
        if snaps.is_empty() {
            return widgets::empty_state(
                IconName::Layers,
                "No snapshots yet",
                Some("Hopper saves one before updates, pack updates and config resets. Take one now to keep this setup.".into()),
                cx,
            )
            .into_any_element();
        }
        let running = self.game_running(cx);
        let busy = self.busy.is_some();
        v_flex()
            .gap_3()
            .children(snaps.iter().enumerate().map(|(i, s)| {
                let open = self.open_snaps.contains(&s.id);
                let (id1, id2, id3) = (s.id.clone(), s.id.clone(), s.id.clone());
                let mut facts = vec![hopper::snapshots::when_label(s.when), format!("{} mods", s.mods()), size(s.size())];
                if s.new_bytes > 0 {
                    facts.push(format!("{} new", size(s.new_bytes)));
                }
                let auto = s.reason.is_auto();
                v_flex()
                    .gap_2()
                    .p_3()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .id(("snap", i))
                            .gap_2()
                            .clickable(cx)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.open_snaps.remove(&id1) {
                                    this.open_snaps.insert(id1.clone());
                                    this.compare(&id1, cx);
                                }
                                cx.notify();
                            }))
                            .child(
                                gpui_kit::component::Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                                    .small()
                                    .flex_none(),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().font_semibold().truncate().child(s.label()))
                                    .child(widgets::caption(facts.join(" · "), cx).text_xs()),
                            )
                            .child(
                                widgets::chip(if auto { theme.muted_foreground } else { theme.primary })
                                    .child(if auto { "auto" } else { "manual" }),
                            )
                            .child(
                                Button::new(("snap-restore", i))
                                    .small()
                                    .outline()
                                    .icon(IconName::Undo2)
                                    .label("Restore")
                                    .disabled(running || busy)
                                    .when(running, |b| b.tooltip("Close the game first"))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.ask_restore(id2.clone(), window, cx)
                                    })),
                            )
                            .child(
                                Button::new(("snap-delete", i))
                                    .small()
                                    .ghost()
                                    .icon(IconName::Trash)
                                    .tooltip("Delete this snapshot")
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.ask_delete_snapshot(id3.clone(), window, cx)
                                    })),
                            ),
                    )
                    .when(open, |c| c.child(self.changes_view(i, &s.id, cx)))
            }))
            .into_any_element()
    }

    /// An open snapshot's changes since, against now.
    fn changes_view(&self, i: usize, id: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let c = match self.changes.get(id) {
            None => return widgets::caption("Comparing with now…", cx).pl_6().text_xs().into_any_element(),
            Some(Err(e)) => {
                return div().pl_6().text_xs().text_color(theme.danger).child(e.clone()).into_any_element();
            }
            Some(Ok(c)) => c,
        };
        if c.is_empty() {
            return widgets::caption("Same as now.", cx).pl_6().text_xs().into_any_element();
        }
        let (changed, added, removed) = (c.mods_of(Kind::Changed), c.mods_of(Kind::Added), c.mods_of(Kind::Removed));
        let show_all = self.all.contains(id);
        // One line per change: (mark, color, text).
        let mut lines: Vec<(&str, Hsla, String)> = c
            .mods
            .iter()
            .map(|m| match m.kind() {
                Kind::Changed => {
                    let (a, b) = m.versions();
                    ("~", theme.warning, format!("{} {a} → {b}", m.name))
                }
                Kind::Added => ("+", theme.success, format!("{} added", m.name)),
                Kind::Removed => ("−", theme.danger, format!("{} removed", m.name)),
            })
            .collect();
        lines.extend(c.files.iter().map(|f| match f.kind {
            Kind::Changed => ("~", theme.warning, format!("{} changed", f.rel)),
            Kind::Added => ("+", theme.success, format!("{} added", f.rel)),
            Kind::Removed => ("−", theme.danger, format!("{} removed", f.rel)),
        }));
        let more = lines.len().saturating_sub(SHOWN);
        if !show_all {
            lines.truncate(SHOWN);
        }
        let id = id.to_string();
        v_flex()
            .pl_6()
            .gap_1()
            .child(
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .child(widgets::caption("Since then:", cx).text_xs())
                    .when(changed > 0, |r| r.child(widgets::chip(theme.warning).child(format!("~{changed} updated"))))
                    .when(added > 0, |r| r.child(widgets::chip(theme.success).child(format!("+{added} added"))))
                    .when(removed > 0, |r| r.child(widgets::chip(theme.danger).child(format!("−{removed} removed"))))
                    .when(!c.files.is_empty(), |r| {
                        r.child(widgets::chip(theme.muted_foreground).child(format!(
                            "{} config file{}",
                            c.files.len(),
                            if c.files.len() == 1 { "" } else { "s" }
                        )))
                    })
                    .when(c.meta, |r| r.child(widgets::chip(theme.muted_foreground).child("hopper.json"))),
            )
            .children(lines.into_iter().map(|(mark, color, text)| {
                h_flex()
                    .gap_2()
                    .text_xs()
                    .child(div().w(rems(0.75)).flex_none().text_color(color).child(mark))
                    .child(div().flex_1().min_w_0().truncate().child(text))
            }))
            .when(more > 0, |c| {
                c.child(
                    div()
                        .id(("snap-all", i))
                        .text_xs()
                        .text_color(theme.primary)
                        .clickable(cx)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.all.remove(&id) {
                                this.all.insert(id.clone());
                            }
                            cx.notify();
                        }))
                        .child(if show_all { "Show fewer".to_string() } else { format!("Show all changes ({more} more)") }),
                )
            })
            .into_any_element()
    }

    /// The loose files list.
    fn loose_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let size = hopper::instance_ops::human_bytes;
        if self.sets.is_empty() {
            return widgets::empty_state(
                IconName::Archive,
                "No backups",
                Some("Files you remove with Move to backups, and files updates replace, show up here.".into()),
                cx,
            )
            .into_any_element();
        }
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
                            .clickable(cx)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.open.remove(&d1) {
                                    this.open.insert(d1.clone());
                                }
                                cx.notify();
                            }))
                            .child(
                                gpui_kit::component::Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                                    .small(),
                            )
                            .child(div().font_semibold().child(hopper::time::relative_secs(set.when)))
                            .child(
                                widgets::caption(format!("{n} file{} · {}", if n == 1 { "" } else { "s" }, size(set.size())), cx)
                                    .text_xs(),
                            )
                            .child(div().flex_1())
                            .child(
                                Button::new(("backup-restore-all", i))
                                    .small()
                                    .outline()
                                    .icon(IconName::Undo2)
                                    .label(if n == 1 { "Restore" } else { "Restore all" })
                                    .on_click(cx.listener(move |this, _, window, cx| this.restore(all.clone(), window, cx))),
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
                                    .on_click(cx.listener(move |this, _, window, cx| this.ask_delete(d3.clone(), window, cx))),
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
                                            widgets::caption(format!("back to {} · {}", f.restore_to, size(f.size)), cx)
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
    }
}

use crate::widgets::DIALOG_TOP;

impl Render for BackupsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let n_snaps = self.snaps.as_ref().map_or(0, Vec::len);
        let snaps_mode = self.mode == Mode::Snapshots;
        let filled = if snaps_mode { n_snaps > 0 } else { !self.sets.is_empty() };
        let intro = match (&self.busy, snaps_mode) {
            (Some(b), _) => b.clone(),
            (None, true) => format!(
                "Mods, configs and hopper.json, saved before updates and config resets. Keeps the last {} automatic ones; yours stay until you delete them.",
                hopper::snapshots::KEEP_AUTO
            ),
            (None, false) => {
                "Files Hopper set aside: mods and packs you removed, and files replaced by updates and syncs. Newest first.".into()
            }
        };
        let body = if snaps_mode { self.snapshots_body(cx) } else { self.loose_body(cx) };
        v_flex()
            .when(filled, |c| c.h(widgets::dialog_room(window).max(window.rem_size() * 12.)))
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        TabBar::new("backups-mode")
                            .segmented()
                            .small()
                            .selected_index(if snaps_mode { 0 } else { 1 })
                            .child(Tab::new().label(format!("Snapshots {n_snaps}")))
                            .child(Tab::new().label(format!("Loose files {}", self.sets.len())))
                            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                this.mode = if *ix == 0 { Mode::Snapshots } else { Mode::Loose };
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .when(snaps_mode, |r| {
                        r.child(
                            Button::new("snap-take")
                                .small()
                                .primary()
                                .icon(IconName::Plus)
                                .label("Take snapshot")
                                .disabled(self.busy.is_some())
                                .on_click(cx.listener(|this, _, window, cx| this.take_snapshot(window, cx))),
                        )
                    }),
            )
            .child(widgets::caption(intro, cx))
            .child(div().id("backups-list").flex_1().min_h_0().overflow_y_scrollbar().child(body))
            .when(snaps_mode && n_snaps > 0, |c| {
                c.child(widgets::caption(
                    format!("{} on disk for snapshots", hopper::instance_ops::human_bytes(self.used)),
                    cx,
                ).text_xs())
            })
    }
}

/// Opens an instance's backups.
pub fn open(store: Entity<Store>, root: PathBuf, window: &mut Window, cx: &mut App) {
    open_at(store, root, None, window, cx)
}

/// Opens an instance's backups on its snapshots, with `snap` open (else
/// the newest).
pub fn open_at(store: Entity<Store>, root: PathBuf, snap: Option<String>, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| {
        let mut v = BackupsView {
            sets: hopper::backups::list(&root),
            store,
            root,
            open: HashSet::new(),
            mode: Mode::Snapshots,
            snaps: None,
            used: 0,
            open_snaps: HashSet::new(),
            all: HashSet::new(),
            changes: HashMap::new(),
            comparing: HashSet::new(),
            busy: None,
            focus: snap,
        };
        v.reload_snaps(cx);
        v
    });
    window.open_dialog(cx, move |dialog, window, _| {
        dialog
            .title("Backups")
            .w(rems(42.).to_pixels(window.rem_size()))
            .margin_top(rems(DIALOG_TOP).to_pixels(window.rem_size()))
            .child(view.clone())
    });
}
