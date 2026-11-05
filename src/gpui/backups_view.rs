//! An instance's History dialog: History (what was removed, updated or
//! edited, with Undo; see `history_view`) and Snapshots (the whole
//! instance at one moment, made by the user or on a schedule, to compare
//! with now and restore).

use crate::widgets::Clickable as _;
use crate::store::{Msg, Store};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::backups::Set;
use hopper::snapshots::{Changes, Kind, Reason, Snapshot};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    History,
    Snapshots,
}

/// Changes listed under an open snapshot before "Show all".
const SHOWN: usize = 6;

pub struct BackupsView {
    pub(crate) store: Entity<Store>,
    pub(crate) root: PathBuf,
    pub(crate) sets: Vec<Set>,
    /// History is read (in the background: a big pack has hundreds of
    /// entries).
    pub(crate) sets_loaded: bool,
    /// Bytes of full mod copies History holds from before notes, for the
    /// "Free up space" offer; `None` while reading.
    pub(crate) copies: Option<u64>,
    /// Projects the instance has had, by file name, for backed-up files.
    projects: HashMap<String, (u64, String)>,
    /// Multi-file entries shown open (by folder).
    pub(crate) open: HashSet<PathBuf>,
    mode: Mode,
    /// `None` while loading.
    snaps: Option<Vec<Snapshot>>,
    /// What the backups folder holds, and what a clean up would free now;
    /// `None` while reading.
    usage: Option<(hopper::retention::Usage, hopper::retention::Pruned)>,
    /// Snapshots shown open, and those showing every change (by id).
    open_snaps: HashSet<String>,
    all: HashSet<String>,
    /// Each snapshot compared with now; absent while comparing.
    changes: HashMap<String, Result<Changes, String>>,
    comparing: HashSet<String>,
    /// What's running (a snapshot or restore), shown instead of the intro.
    pub(crate) busy: Option<String>,
    /// The snapshot to show open the first time the list loads, instead of
    /// the newest (a task's "View snapshot").
    focus: Option<String>,
    /// A mod's page beside the lists (the dialog stays open).
    peek: Option<Entity<crate::browse::BrowseView>>,
    _peek_watch: Option<Subscription>,
    /// Redraws when icons arrive.
    _store_watch: Option<Subscription>,
}

impl BackupsView {
    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        cx.spawn(async move |this, cx| {
            let (sets, projects, copies) = cx
                .background_spawn(async move {
                    (hopper::backups::list(&root), hopper::snapshots::projects_seen(&root), hopper::backups::copies_size(&root))
                })
                .await;
            this.update(cx, |this, cx| {
                this.sets = sets;
                this.copies = Some(copies);
                this.projects = projects;
                this.sets_loaded = true;
                this.reload_usage(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn reload_usage(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        cx.spawn(async move |this, cx| {
            let u = cx
                .background_spawn(async move {
                    (hopper::retention::usage(&root), hopper::retention::preview(&root))
                })
                .await;
            this.update(cx, |this, cx| {
                this.usage = Some(u);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Runs the clean up rules (Settings › Library & folders) now.
    fn clean_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.root.clone();
        self.busy = Some("Clearing old History…".into());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let p = cx.background_spawn(async move { hopper::retention::clean_up(&root) }).await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                let n = p.count();
                let msg = if n == 0 {
                    "Nothing to clean up".to_string()
                } else {
                    format!(
                        "Cleared {n} old History entr{} ({})",
                        if n == 1 { "y" } else { "ies" },
                        hopper::instance_ops::human_bytes(p.freed)
                    )
                };
                window.push_notification(Notification::success(msg), cx);
                this.reload(cx);
                this.reload_snaps(cx);
            })
            .ok();
        })
        .detach();
    }

    /// "History uses 4.9 GB · Over the 1 GB limit · Keeps … · Clean up".
    fn usage_line(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let r = hopper::retention::policy();
        let Some((u, would)) = &self.usage else {
            return widgets::caption("Reading History…", cx).text_xs().into_any_element();
        };
        let cap = r.max_mb * 1024 * 1024;
        let over = cap > 0 && u.limited() > cap;
        let gb = |b: u64| hopper::instance_ops::human_bytes(b);
        let used = if cap > 0 && !over { format!("History uses {} of {}", gb(u.total()), gb(cap)) } else { format!("History uses {}", gb(u.total())) };
        let rules = format!("Keeps the last {} actions and {} days", r.keep_actions, r.keep_days);
        let label = if would.freed > 0 { format!("Clean up · frees {}", gb(would.freed)) } else { "Clean up".to_string() };
        h_flex()
            .gap_2()
            .pt_2()
            .border_t_1()
            .border_color(theme.border)
            .child(div().text_sm().child(used))
            .when(over, |r| r.child(widgets::chip(theme.warning).child(format!("Over the {} limit", gb(cap)))))
            .child(div().flex_1())
            .child(widgets::caption(rules, cx).text_xs())
            .child(
                Button::new("backups-clean")
                    .small()
                    .outline()
                    .label(label)
                    .tooltip("Clears History older than the rules in Settings › Library & folders (to the Recycle Bin). Hopper does this by itself as History grows, but leaves older entries for you. Snapshots you took and world backups stay.")
                    .disabled(self.busy.is_some() || would.count() == 0)
                    .on_click(cx.listener(|this, _, window, cx| this.clean_up(window, cx))),
            )
            .into_any_element()
    }

    fn reload_snaps(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        cx.spawn(async move |this, cx| {
            let snaps = cx.background_spawn(async move { hopper::snapshots::list(&root) }).await;
            this.update(cx, |this, cx| {
                // Now has likely moved on: compare again.
                this.changes.clear();
                this.comparing.clear();
                // The newest opens when it's new (the first read, or one
                // just taken); otherwise what you folded stays folded.
                let known = this.snaps.as_ref().map(|v| v.iter().map(|s| s.id.clone()).collect::<HashSet<_>>());
                let first = match this.focus.take() {
                    Some(f) if snaps.iter().any(|s| s.id == f) => Some(f),
                    _ => snaps
                        .first()
                        .filter(|s| known.as_ref().is_none_or(|k| !k.contains(&s.id)))
                        .map(|s| s.id.clone()),
                };
                this.snaps = Some(snaps);
                this.reload_usage(cx);
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

    /// Opens a mod's page beside the lists, leaving the dialog open.
    fn open_peek(&mut self, project: u64, window: &mut Window, cx: &mut Context<Self>) {
        if project == 0 {
            return;
        }
        if self.peek.is_none() {
            let store = self.store.clone();
            let p = cx.new(|cx| crate::browse::BrowseView::peek(store, window, cx));
            // Its own close button empties it: then the panel goes (once
            // it showed one: it's empty while the first loads).
            let mut shown = false;
            self._peek_watch = Some(cx.observe(&p, move |this, p, cx| match p.read(cx).selected_id() {
                Some(_) => shown = true,
                None if shown => {
                    this.peek = None;
                    this._peek_watch = None;
                    cx.notify();
                }
                None => {}
            }));
            self.peek = Some(p);
        }
        let Some(peek) = self.peek.clone() else { return };
        let root = self.root.clone();
        peek.update(cx, |p, cx| p.peek_project(root, Some(project), cx));
        window.refresh();
        cx.notify();
    }

    /// The instance's project and icon for a backed-up file, by its name
    /// (or, for another version of it, its name without the version).
    pub(crate) fn project_of(&self, file: &str, _: &App) -> Option<(u64, String)> {
        let lower = file.to_lowercase();
        let bare = lower.strip_suffix(".disabled").unwrap_or(&lower).to_string();
        if let Some(p) = self.projects.get(&bare) {
            return Some(p.clone());
        }
        let stem = hopper::scan::strip_version_suffix(&bare);
        self.projects
            .iter()
            .find(|(f, _)| hopper::scan::strip_version_suffix(f) == stem)
            .map(|(_, p)| p.clone())
    }

    /// A mod's icon and name (or file name) that opens its page, when its
    /// project is known; asks for icons not loaded yet.
    pub(crate) fn mod_cell(&self, id: ElementId, text: String, project: u64, icon: &str, small: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let path = self.store.read(cx).image(icon).map(std::path::Path::to_path_buf);
        if path.is_none() && !icon.is_empty() {
            let icon = icon.to_string();
            self.store.update(cx, |s, _| s.want_image(&icon));
        }
        let primary = cx.theme().primary;
        h_flex()
            .id(id)
            .min_w_0()
            .gap_2()
            .child(widgets::thumb(path.as_deref(), rems(if small { 1. } else { 1.5 }), cx))
            .child(div().min_w_0().truncate().child(text))
            .when(project != 0, |d| {
                d.clickable(cx)
                    .hover(move |s| s.text_color(primary))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_peek(project, window, cx)
                    }))
            })
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

    pub(crate) fn changed(&mut self, cx: &mut Context<Self>) {
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
        // "Snapshot first" is off unless ticked: backups are the player's to make.
        let first = std::rc::Rc::new(std::cell::Cell::new(false));
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let theme = cx.theme();
            let line = |k: &'static str, v: String| {
                h_flex()
                    .gap_3()
                    .child(div().w(rems(5.)).flex_none().text_color(theme.muted_foreground).child(k))
                    .child(div().flex_1().min_w_0().child(v))
            };
            let (view, id) = (view.clone(), snap.id.clone());
            let (f2, f3) = (first.clone(), first.clone());
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
                        .child(
                            gpui_kit::component::checkbox::Checkbox::new("restore-snap-first")
                                .label("Take a snapshot first, so this can be undone")
                                .checked(f2.get())
                                .on_click(move |v: &bool, window, _| {
                                    f3.set(*v);
                                    window.refresh();
                                }),
                        )
                        .child(div().text_color(theme.muted_foreground).child(if f2.get() {
                            "Close the game before restoring."
                        } else {
                            "Without one, what it takes away can be put back from History. Close the game before restoring."
                        })),
                )
                .confirm()
                .ok_text(format!("Restore {total} change{}", if total == 1 { "" } else { "s" }))
                .on_ok(move |_, window, cx| {
                    let first = f2.get();
                    view.update(cx, |this, cx| this.restore_snapshot(id.clone(), first, window, cx));
                    true
                })
        });
    }

    fn restore_snapshot(&mut self, id: String, snapshot_first: bool, window: &mut Window, cx: &mut Context<Self>) {
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
            let r = cx.background_spawn(async move { hopper::snapshots::restore(&root, &id, snapshot_first) }).await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match r {
                    Ok(r) => {
                        let undo = match (&r.safety, &r.aside) {
                            (Some(s), _) => format!(" To undo, restore \"{}\".", s.label()),
                            (None, Some(_)) => " What it took away can be put back from History.".to_string(),
                            (None, None) => String::new(),
                        };
                        window.push_notification(
                            Notification::success(format!(
                                "Restored \"{label}\" ({} file{}).{undo}",
                                r.changed,
                                if r.changed == 1 { "" } else { "s" },
                            )),
                            cx,
                        )
                    }
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
                                widgets::row_actions(("snap-actions", i))
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
                                    )
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
        // One line per change: (mark, color, text, and for a mod its
        // project and icon).
        let mut lines: Vec<(&str, Hsla, String, Option<(u64, String)>)> = c
            .mods
            .iter()
            .map(|m| {
                let p = Some((m.project, m.icon.clone()));
                match m.kind() {
                    Kind::Changed => {
                        let (a, b) = m.versions();
                        ("~", theme.warning, format!("{} {a} → {b}", m.name), p)
                    }
                    Kind::Added => ("+", theme.success, format!("{} added", m.name), p),
                    Kind::Removed => ("−", theme.danger, format!("{} removed", m.name), p),
                }
            })
            .collect();
        lines.extend(c.files.iter().map(|f| match f.kind {
            Kind::Changed => ("~", theme.warning, format!("{} changed", f.rel), None),
            Kind::Added => ("+", theme.success, format!("{} added", f.rel), None),
            Kind::Removed => ("−", theme.danger, format!("{} removed", f.rel), None),
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
            .children(lines.into_iter().enumerate().map(|(k, (mark, color, text, project))| {
                let row = h_flex().gap_2().text_xs().child(div().w(rems(0.75)).flex_none().text_color(color).child(mark));
                match project {
                    // A mod: its icon, and its page on click.
                    Some((pid, icon)) => row.child(
                        self.mod_cell(ElementId::Name(format!("snap-mod:{id}:{k}").into()), text, pid, &icon, true, cx)
                            .flex_1(),
                    ),
                    None => row.child(div().flex_1().min_w_0().truncate().child(text)),
                }
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

}

use crate::widgets::DIALOG_TOP;

impl Render for BackupsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let n_snaps = self.snaps.as_ref().map_or(0, Vec::len);
        let snaps_mode = self.mode == Mode::Snapshots;
        let filled = if snaps_mode { n_snaps > 0 } else { !self.sets.is_empty() };
        let intro = match (&self.busy, snaps_mode) {
            (Some(b), _) => b.clone(),
            (None, true) => "Mods, configs and hopper.json at one moment, made by you or on a schedule, to compare with now and put back. They stay until you delete them.".to_string(),
            (None, false) => {
                "What changed in this instance, newest first. Undo puts a mod back by downloading it again; configs come back from the text Hopper kept.".into()
            }
        };
        let body = if snaps_mode { self.snapshots_body(cx) } else { self.history_body(cx) };
        let banner = if snaps_mode { None } else { self.free_banner(cx) };
        let room = widgets::dialog_room(window).max(window.rem_size() * 12.);
        // A mod's page, beside the lists.
        let peek = self.peek.clone().map(|p| v_flex().w(rems(30.)).flex_none().h(room).child(div().flex_1().min_h_0().child(p)));
        let main = v_flex()
            .flex_1()
            .min_w_0()
            .when(filled || peek.is_some(), |c| c.h(room))
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(widgets::underline_tabs(
                        "backups-mode",
                        vec![
                            ("History".into(), self.sets_loaded.then_some(self.sets.len())),
                            ("Snapshots".into(), self.snaps.as_ref().map(|_| n_snaps)),
                        ],
                        if snaps_mode { 1 } else { 0 },
                        cx.listener(|this, ix: &usize, _, cx| {
                            this.mode = if *ix == 0 { Mode::History } else { Mode::Snapshots };
                            cx.notify();
                        }),
                        cx,
                    ))
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
            .children(banner)
            // Empty, it shows its empty state at its own height (a flex_1 scroll
            // area has none without a set height).
            .child(if filled { div().id("backups-list").flex_1().min_h_0().overflow_y_scrollbar().child(body).into_any_element() } else { body })
            .child(self.usage_line(cx));
        h_flex().items_start().gap_4().child(main).children(peek)
    }
}

/// Opens an instance's History.
pub fn open(store: Entity<Store>, root: PathBuf, window: &mut Window, cx: &mut App) {
    open_at(store, root, None, window, cx)
}

/// Opens an instance's History dialog: on its snapshots with `snap` open,
/// else on History.
pub fn open_at(store: Entity<Store>, root: PathBuf, snap: Option<String>, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| {
        let mut v = BackupsView {
            sets: Vec::new(),
            sets_loaded: false,
            copies: None,
            projects: HashMap::new(),
            store,
            root,
            open: HashSet::new(),
            mode: if snap.is_some() { Mode::Snapshots } else { Mode::History },
            snaps: None,
            usage: None,
            open_snaps: HashSet::new(),
            all: HashSet::new(),
            changes: HashMap::new(),
            comparing: HashSet::new(),
            busy: None,
            focus: snap,
            peek: None,
            _peek_watch: None,
            _store_watch: None,
        };
        v._store_watch = Some(cx.observe(&v.store, |_, _, cx| cx.notify()));
        v.reload_snaps(cx);
        v.reload(cx);
        v
    });
    window.open_dialog(cx, move |dialog, window, cx| {
        dialog
            .title("History")
            .w(rems(if view.read(cx).peek.is_some() { 74. } else { 42. }).to_pixels(window.rem_size()))
            .margin_top(rems(DIALOG_TOP).to_pixels(window.rem_size()))
            .child(view.clone())
    });
}
