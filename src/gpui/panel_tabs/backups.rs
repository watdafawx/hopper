//! A Hopper Server server's Backups tab, like the panel's: make a backup,
//! look inside one, restore all or some of it, delete it.

use super::{Load, Target, bytes, get, job, load_state, q};
use crate::store::Store;
use crate::widgets::{self, Clickable as _};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};

#[derive(Clone)]
struct Backup {
    name: String,
    at: u64,
    size: u64,
    full: bool,
    kind: String,
    note: String,
}

struct Data {
    backups: Vec<Backup>,
    total: u64,
    free: u64,
}

/// A top-level folder or file inside a backup.
#[derive(Clone)]
struct Item {
    /// What to restore: `world/` for a folder, the path for a file.
    path: String,
    files: usize,
    size: u64,
}

pub struct BackupsTab {
    store: Entity<Store>,
    target: Option<Target>,
    data: Load<Data>,
    note: Entity<InputState>,
    worlds_only: bool,
    /// What the panel is doing (making or restoring), while it does it.
    busy: Option<String>,
    error: Option<String>,
    status: Option<String>,
    open: Option<Backup>,
    items: Load<Vec<Item>>,
    picked: HashSet<String>,
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn n(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}

fn parse(v: &Value) -> Data {
    let backups = v
        .get("backups")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|b| Backup {
                    name: s(b, "name"),
                    at: n(b, "at"),
                    size: n(b, "size"),
                    full: s(b, "scope") == "full",
                    kind: s(b, "kind"),
                    note: s(b, "note"),
                })
                .collect()
        })
        .unwrap_or_default();
    Data { backups, total: n(v, "total_size"), free: n(v, "disk_free") }
}

/// The entries of a backup grouped by top-level folder or file.
fn group(v: &Value) -> Vec<Item> {
    let mut by: BTreeMap<String, Item> = BTreeMap::new();
    for e in v.get("entries").and_then(Value::as_array).into_iter().flatten() {
        let (Some(p), size) = (e.get(0).and_then(Value::as_str), e.get(1).and_then(Value::as_u64).unwrap_or(0)) else {
            continue;
        };
        let path = match p.split_once('/') {
            Some((top, _)) => format!("{top}/"),
            None => p.to_string(),
        };
        let it = by.entry(path.clone()).or_insert(Item { path, files: 0, size: 0 });
        it.files += 1;
        it.size += size;
    }
    by.into_values().collect()
}


/// "2026-10-25 14:03" (UTC) from unix seconds, with how long ago when recent.
fn when(at: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(at);
    let ago = now.saturating_sub(at);
    if ago < 60 {
        return "Just now".into();
    }
    if ago < 3600 {
        return format!("{} min ago", ago / 60);
    }
    if ago < 86400 {
        let h = ago / 3600;
        return format!("{h} hour{} ago", if h == 1 { "" } else { "s" });
    }
    let days = (at / 86400) as i64;
    let rem = at % 86400;
    // Days since 1970-01-01 to a civil date.
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y}-{m:02}-{d:02} {:02}:{:02}", rem / 3600, rem % 3600 / 60)
}

impl BackupsTab {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let note = cx.new(|cx| InputState::new(window, cx).placeholder("Note (optional), e.g. Before the update"));
        Self {
            store,
            target: None,
            data: Load::Waiting,
            note,
            worlds_only: false,
            busy: None,
            error: None,
            status: None,
            open: None,
            items: Load::Waiting,
            picked: HashSet::new(),
        }
    }

    /// Shows `target`'s Backups (again when it's the same server: a refresh).
    pub fn show(&mut self, target: Target, _window: &mut Window, cx: &mut Context<Self>) {
        if self.target.as_ref() != Some(&target) {
            self.data = Load::Waiting;
            self.open = None;
            self.busy = None;
            self.status = None;
        }
        self.error = None;
        self.target = Some(target);
        self.reload(cx);
        cx.notify();
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let id = t.id().to_string();
        get(cx, &t, t.sp("/backups"), move |v, r, _| {
            // A reply for a server that's no longer shown.
            if v.target.as_ref().map(|t| t.id()) != Some(id.as_str()) {
                return;
            }
            match r {
                Ok(j) => v.data = Load::Ready(parse(&j)),
                Err(e) => v.data = Load::Failed(e),
            }
        });
    }

    fn make(&mut self, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let note = self.note.read(cx).value().trim().to_string();
        let worlds = self.worlds_only;
        self.busy = Some("Backing up".into());
        self.error = None;
        self.status = None;
        let title = if worlds { "Backing up the worlds" } else { "Backing up everything" };
        job(cx, &self.store.clone(), &t, title, t.sp("/backups"), json!({ "worlds_only": worlds, "note": note }), |v, r, cx| {
            v.busy = None;
            match r {
                Ok(b) => v.status = Some(format!("Backed up: {}, {} files.", bytes(n(&b, "size")), n(&b, "files"))),
                Err(e) => v.error = Some(e),
            }
            v.reload(cx);
        });
        cx.notify();
    }

    fn browse(&mut self, b: Backup, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let name = b.name.clone();
        self.open = Some(b);
        self.items = Load::Waiting;
        self.picked.clear();
        get(cx, &t, t.sp(&format!("/backups/{}/entries", q(&name))), move |v, r, _| {
            if v.open.as_ref().map(|o| o.name.as_str()) != Some(name.as_str()) {
                return;
            }
            v.items = match r {
                Ok(j) => Load::Ready(group(&j)),
                Err(e) => Load::Failed(e),
            };
        });
        cx.notify();
    }

    fn restore(&mut self, b: Backup, paths: Vec<String>, backup_first: bool, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        self.busy = Some("Restoring".into());
        self.error = None;
        self.status = None;
        job(
            cx,
            &self.store.clone(),
            &t,
            "Restoring a backup",
            t.sp(&format!("/backups/{}/restore", q(&b.name))),
            json!({ "paths": paths, "backup_first": backup_first }),
            |v, r, _| {
                v.busy = None;
                match r {
                    Ok(j) => {
                        let undo = j["safety_note"].as_str().map(|s| format!(" To undo, restore \"{s}\".")).unwrap_or_default();
                        v.status = Some(format!("Restored {} files.{undo}", n(&j, "files")));
                        v.open = None;
                    }
                    Err(e) => v.error = Some(e),
                }
            },
        );
        cx.notify();
    }

    fn delete(&mut self, b: Backup, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        self.error = None;
        if self.open.as_ref().is_some_and(|o| o.name == b.name) {
            self.open = None;
        }
        let path = t.sp(&format!("/backups/{}", q(&b.name)));
        super::call(cx, &t, move |c| c.delete(&path), |v, r, cx| {
            if let Err(e) = r {
                v.error = Some(e);
            }
            v.reload(cx);
        });
        cx.notify();
    }

    fn ask_restore(&mut self, b: Backup, paths: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity();
        let what = if paths.is_empty() {
            format!("Restore everything in the backup from {}?", when(b.at))
        } else {
            format!("Restore {} item{} from the backup?", paths.len(), if paths.len() == 1 { "" } else { "s" })
        };
        let body = if paths.is_empty() {
            "Files in the backup replace the ones on the server. Worlds in it are replaced whole; the current ones are moved aside, not deleted. The server has to be stopped."
        } else {
            "Those files replace the ones on the server. The server has to be stopped."
        };
        // "Back up first" is off unless ticked: backups are the player's to make.
        let first = std::rc::Rc::new(std::cell::Cell::new(false));
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (this, b, paths) = (this.clone(), b.clone(), paths.clone());
            let (f2, f3) = (first.clone(), first.clone());
            alert
                .title(what.clone())
                .description(
                    v_flex()
                        .gap_2()
                        .child(body)
                        .child(
                            Checkbox::new("restore-backup-first")
                                .label("Back up first, so this can be undone")
                                .checked(f2.get())
                                .on_click(move |v: &bool, window, _| {
                                    f3.set(*v);
                                    window.refresh();
                                }),
                        ),
                )
                .confirm()
                .ok_text("Restore")
                .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let (b, paths, first) = (b.clone(), paths.clone(), f2.get());
                    this.update(cx, |t, cx| t.restore(b, paths, first, cx));
                    true
                })
        });
    }

    fn ask_delete(&mut self, b: Backup, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (this, b) = (this.clone(), b.clone());
            alert
                .title(format!("Delete the backup from {}?", when(b.at)))
                .description("It can't be brought back.")
                .confirm()
                .ok_text("Delete")
                .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let b = b.clone();
                    this.update(cx, |t, cx| t.delete(b, cx));
                    true
                })
        });
    }

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.busy.is_some();
        let theme = cx.theme().clone();
        let this = cx.entity();
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(div().flex_1().child(Input::new(&self.note).disabled(busy)))
                    .child(
                        Checkbox::new("worlds-only")
                            .label("Worlds only")
                            .checked(self.worlds_only)
                            .on_click(move |v: &bool, _, cx| {
                                let v = *v;
                                this.update(cx, |t, cx| {
                                    t.worlds_only = v;
                                    cx.notify();
                                });
                            }),
                    )
                    .child(
                        Button::new("back-up-now")
                            .primary()
                            .icon(IconName::Archive)
                            .label("Back up now")
                            .disabled(busy)
                            .on_click(cx.listener(|t, _, _, cx| t.make(cx))),
                    ),
            )
            .child({
                let mut info = Vec::new();
                if let Some(d) = self.data.ready() {
                    info.push(format!("{} backup{}", d.backups.len(), if d.backups.len() == 1 { "" } else { "s" }));
                    info.push(bytes(d.total));
                    if d.free > 0 {
                        info.push(format!("{} free on the drive", bytes(d.free)));
                    }
                }
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .when_some(self.busy.clone(), |r, b| r.child(Spinner::new().xsmall()).child(b))
                    .when(self.busy.is_none(), |r| r.child(info.join(" · ")))
            })
            .when_some(self.status.clone(), |c, st| c.child(div().text_sm().text_color(theme.success).child(st)))
            .when_some(self.error.clone(), |c, e| c.child(div().text_sm().text_color(theme.danger).child(e)))
            .into_any_element()
    }

    fn list(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let _ = window;
        let theme = cx.theme().clone();
        let busy = self.busy.is_some();
        let Some(d) = self.data.ready() else {
            return load_state(&self.data, cx).unwrap_or_else(|| div().into_any_element());
        };
        if d.backups.is_empty() {
            return widgets::empty_state(IconName::Archive, "No backups yet", Some("Back up now makes the first one.".into()), cx)
                .into_any_element();
        }
        let open = self.open.as_ref().map(|o| o.name.clone());
        let rows: Vec<AnyElement> = d
            .backups
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let sel = open.as_deref() == Some(b.name.as_str());
                let (b1, b2, b3) = (b.clone(), b.clone(), b.clone());
                let row = h_flex()
                    .id(("backup", i))
                    .gap_3()
                    .items_center()
                    .px_3()
                    .py_2()
                    .rounded(theme.radius)
                    .clickable(cx)
                    .on_click(cx.listener(move |t, _, _, cx| t.browse(b1.clone(), cx)))
                    .child(
                        Icon::new(if b.full { IconName::Database } else { IconName::Archive })
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(div().text_sm().child(when(b.at)))
                                    .child(widgets::chip(theme.muted_foreground).child(if b.full { "Everything" } else { "Worlds only" }))
                                    .when(!b.kind.is_empty() && b.kind != "manual", |r| {
                                        r.child(widgets::chip(theme.muted_foreground).child(b.kind.clone()))
                                    }),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .truncate()
                                    .text_color(theme.muted_foreground)
                                    .child(if b.note.is_empty() { b.name.clone() } else { b.note.clone() }),
                            ),
                    )
                    .child(div().text_sm().text_color(theme.muted_foreground).child(bytes(b.size)))
                    .child(
                        Button::new(("restore-all", i))
                            .outline()
                            .small()
                            .icon(IconName::RotateCcw)
                            .label("Restore")
                            .disabled(busy)
                            .on_click(cx.listener(move |t, _, w, cx| t.ask_restore(b2.clone(), vec![], w, cx))),
                    )
                    .child(
                        Button::new(("delete", i))
                            .ghost()
                            .small()
                            .icon(IconName::Trash)
                            .tooltip("Delete")
                            .on_click(cx.listener(move |t, _, w, cx| t.ask_delete(b3.clone(), w, cx))),
                    );
                if sel { widgets::row_selected(row, cx).into_any_element() } else { widgets::row_hover(row, cx).into_any_element() }
            })
            .collect();
        v_flex().id("backup-list").size_full().gap_1().overflow_y_scrollbar().children(rows).into_any_element()
    }

    fn entries(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let b = self.open.clone()?;
        let theme = cx.theme().clone();
        let busy = self.busy.is_some();
        let picked = self.picked.len();
        let body = match load_state(&self.items, cx) {
            Some(el) => el,
            None => {
                let items = self.items.ready().cloned().unwrap_or_default();
                if items.is_empty() {
                    widgets::empty_state(IconName::FolderOpen, "Nothing in this backup", None, cx).into_any_element()
                } else {
                    let rows = items.into_iter().enumerate().map(|(i, it)| {
                        let path = it.path.clone();
                        h_flex()
                            .gap_2()
                            .items_center()
                            .px_2()
                            .py_1()
                            .child(
                                Checkbox::new(("pick", i))
                                    .checked(self.picked.contains(&it.path))
                                    .on_click(cx.listener(move |t, v: &bool, _, cx| {
                                        if *v {
                                            t.picked.insert(path.clone());
                                        } else {
                                            t.picked.remove(&path);
                                        }
                                        cx.notify();
                                    })),
                            )
                            .child(div().flex_1().min_w_0().truncate().text_sm().child(it.path.clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("{} · {}", if it.files == 1 { "1 file".into() } else { format!("{} files", it.files) }, bytes(it.size))),
                            )
                    });
                    v_flex().id("backup-entries").flex_1().min_h_0().overflow_y_scrollbar().children(rows).into_any_element()
                }
            }
        };
        let b2 = b.clone();
        Some(
            v_flex()
                .w(px(380.))
                .h_full()
                .flex_none()
                .gap_2()
                .pl_3()
                .border_l_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(div().flex_1().min_w_0().truncate().font_semibold().child(format!("Inside the backup from {}", when(b.at))))
                        .child(
                            Button::new("close-entries")
                                .ghost()
                                .small()
                                .icon(IconName::X)
                                .on_click(cx.listener(|t, _, _, cx| {
                                    t.open = None;
                                    cx.notify();
                                })),
                        ),
                )
                .child(
                    Button::new("restore-picked")
                        .primary()
                        .small()
                        .icon(IconName::RotateCcw)
                        .label(if picked == 0 { "Restore selected".to_string() } else { format!("Restore selected ({picked})") })
                        .disabled(picked == 0 || busy)
                        .on_click(cx.listener(move |t, _, w, cx| {
                            let mut paths: Vec<String> = t.picked.iter().cloned().collect();
                            paths.sort();
                            t.ask_restore(b2.clone(), paths, w, cx)
                        })),
                )
                .child(body)
                .into_any_element(),
        )
    }
}

impl Render for BackupsTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.header(cx);
        let list = self.list(window, cx);
        let entries = self.entries(cx);
        v_flex()
            .size_full()
            .gap_3()
            .child(header)
            .child(h_flex().flex_1().min_h_0().items_start().child(div().flex_1().min_w_0().h_full().child(list)).children(entries))
    }
}
