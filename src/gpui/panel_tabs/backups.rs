//! A Hopper Server server's Backups tab, like the panel's: make a backup,
//! look inside one and what changed since, restore all or some of it,
//! download it as a zip, delete it; the keep rules, off-site copies, and
//! turning old zips into the new format.

use super::{Load, Target, bytes, get, job, load_state, q};
use crate::selection::{self, KeyAct, Selection};
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
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

#[derive(Clone)]
struct Backup {
    name: String,
    at: u64,
    size: u64,
    full: bool,
    kind: String,
    note: String,
    /// Made by an older Hopper Server: a whole zip, not sharing data.
    zip: bool,
}

/// A place the panel copies backups to (another panel, a folder, SFTP),
/// and how this server's copies there stand.
#[derive(Clone)]
struct Place {
    id: String,
    name: String,
    enabled: bool,
    /// This server's backups are copied there.
    on: bool,
    /// The backups copied there, by name.
    done: Vec<String>,
    last_ok: u64,
    last_error: String,
    pending: u64,
    /// The backup being copied now.
    working: String,
}

/// How many scheduled backups clean up keeps.
#[derive(Clone, Copy)]
struct Keep {
    last: u64,
    daily_days: u64,
    weekly_weeks: u64,
}

struct Data {
    backups: Vec<Backup>,
    total: u64,
    free: u64,
    /// Bytes the old zips take (0: none left).
    zips: u64,
    keep: Keep,
    places: Vec<Place>,
    /// The places can be changed (owners only).
    can_edit_places: bool,
}

/// What changed on the server since a backup: what a whole restore would
/// put back (the panel's "Since then").
#[derive(Clone)]
struct Changes {
    changed: Vec<String>,
    removed: Vec<String>,
    added: Vec<String>,
}

impl Changes {
    fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.removed.is_empty() && self.added.is_empty()
    }
}

/// Changes listed under an open backup before "Show all", as desktop's
/// instance backups and the panel show them.
const SHOWN: usize = 8;

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
    /// What the panel is doing (making or restoring), while it does it.
    busy: Option<String>,
    error: Option<String>,
    status: Option<String>,
    open: Option<Backup>,
    items: Load<Vec<Item>>,
    /// What changed since the open backup, by its name.
    since: Option<(String, Load<Changes>)>,
    since_all: bool,
    /// Newest, days, weeks: the keep rules while they're edited.
    keep_inputs: [Entity<InputState>; 3],
    keep_edit: bool,
    /// The places ticked while the off-site copies are edited.
    copies_edit: Option<BTreeSet<String>>,
    /// The ticked items of the open backup.
    picked: Selection,
    /// Backups picked in the list, by name (Ctrl / Shift clicks).
    sel: Selection,
    focus: FocusHandle,
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
                    zip: s(b, "name").ends_with(".zip"),
                })
                .collect()
        })
        .unwrap_or_default();
    let places = v
        .get("targets")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|t| Place {
                    id: s(t, "id"),
                    name: s(t, "name"),
                    enabled: t["enabled"].as_bool().unwrap_or(false),
                    on: t["on"].as_bool().unwrap_or(false),
                    done: strings(t, "done"),
                    last_ok: n(t, "last_ok"),
                    last_error: s(t, "last_error"),
                    pending: n(t, "pending"),
                    working: s(t, "working"),
                })
                .collect()
        })
        .unwrap_or_default();
    let k = &v["keep"];
    Data {
        backups,
        total: n(v, "total_size"),
        free: n(v, "disk_free"),
        zips: n(v, "zips_size"),
        keep: Keep { last: n(k, "last").max(1), daily_days: n(k, "daily_days"), weekly_weeks: n(k, "weekly_weeks") },
        places,
        can_edit_places: v["can_edit_targets"].as_bool().unwrap_or(false),
    }
}

fn strings(v: &Value, k: &str) -> Vec<String> {
    v.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

fn changes(v: &Value) -> Changes {
    Changes { changed: strings(v, "changed"), removed: strings(v, "removed"), added: strings(v, "added") }
}

fn plural(n: usize, one: &str) -> String {
    hopper::words::count(n, one)
}

/// "Newest 12 · one a day for 7 days", as the panel says the keep rules.
fn keep_text(k: Keep) -> String {
    let mut t = format!("Newest {}", k.last);
    if k.daily_days > 0 {
        t.push_str(&format!(" · one a day for {} days", k.daily_days));
    }
    if k.weekly_weeks > 0 {
        t.push_str(&format!(" · one a week for {} weeks", k.weekly_weeks));
    }
    t
}

/// How one place's copies of this server stand, in the panel's words.
fn place_status(p: &Place) -> String {
    if !p.last_error.is_empty() {
        p.last_error.clone()
    } else if !p.working.is_empty() {
        "Copying now".into()
    } else if p.pending > 0 {
        format!("{} waiting", p.pending)
    } else if p.last_ok > 0 {
        format!("Up to date · {}", when(p.last_ok).to_lowercase())
    } else {
        "Waiting".into()
    }
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
        let keep_inputs = ["12", "0", "0"].map(|ph| cx.new(|cx| InputState::new(window, cx).placeholder(ph)));
        Self {
            store,
            target: None,
            data: Load::Waiting,
            note,
            busy: None,
            error: None,
            status: None,
            open: None,
            items: Load::Waiting,
            since: None,
            since_all: false,
            keep_inputs,
            keep_edit: false,
            copies_edit: None,
            picked: Selection::new(),
            sel: Selection::new(),
            focus: cx.focus_handle(),
        }
    }

    /// The backups' names in the order the list shows them.
    fn order(&self) -> Vec<String> {
        self.data.ready().map(|d| d.backups.iter().map(|b| b.name.clone()).collect()).unwrap_or_default()
    }

    /// The open backup's items in the order they're shown.
    fn item_order(&self) -> Vec<String> {
        self.items.ready().map(|l| l.iter().map(|i| i.path.clone()).collect()).unwrap_or_default()
    }

    fn picked_backups(&self) -> Vec<Backup> {
        self.data.ready().map(|d| d.backups.iter().filter(|b| self.sel.contains(&b.name)).cloned().collect()).unwrap_or_default()
    }

    /// Shows `target`'s Backups (again when it's the same server: a refresh).
    pub fn show(&mut self, target: Target, _window: &mut Window, cx: &mut Context<Self>) {
        if self.target.as_ref() != Some(&target) {
            self.data = Load::Waiting;
            self.open = None;
            self.since = None;
            self.busy = None;
            self.status = None;
            self.keep_edit = false;
            self.copies_edit = None;
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
            let order = v.order();
            v.sel.retain(&order);
        });
    }

    fn make(&mut self, worlds: bool, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let note = self.note.read(cx).value().trim().to_string();
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
        self.since_all = false;
        self.compare(name.clone(), cx);
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

    /// Looks again at what changed since the backup `name` (a running
    /// server keeps changing). A server on this PC has no such comparison,
    /// so its open backups show only their files.
    fn compare(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone().filter(|t| t.local.is_none()) else {
            self.since = None;
            return;
        };
        self.since = Some((name.clone(), Load::Waiting));
        get(cx, &t, t.sp(&format!("/backups/{}/changes", q(&name))), move |v, r, _| {
            if v.since.as_ref().map(|s| s.0.as_str()) != Some(name.as_str()) {
                return;
            }
            v.since = Some((name, Load::from(r.map(|j| changes(&j)))));
        });
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
                        v.since = None;
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
            self.since = None;
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

    /// A whole restore compares with now first, as the panel does: when
    /// nothing differs there's nothing to ask about.
    fn ask_restore(&mut self, b: Backup, paths: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        if !paths.is_empty() || t.local.is_some() {
            return self.confirm_restore(b, paths, None, window, cx);
        }
        self.busy = Some("Comparing with now…".into());
        self.error = None;
        self.status = None;
        let path = t.sp(&format!("/backups/{}/changes", q(&b.name)));
        super::call_in(window, cx, &t, move |c| c.get(&path), move |v, r, window, cx| {
            v.busy = None;
            // Couldn't compare: ask anyway, without the list of changes.
            let ch = r.ok().map(|j| changes(&j));
            if ch.as_ref().is_some_and(Changes::is_empty) {
                v.status = Some("Nothing to restore: the server is the same as this backup.".into());
                return;
            }
            v.confirm_restore(b, paths, ch, window, cx)
        });
        cx.notify();
    }

    fn confirm_restore(&mut self, b: Backup, paths: Vec<String>, ch: Option<Changes>, window: &mut Window, cx: &mut Context<Self>) {
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
        // What goes back, in the panel's words, when it was compared.
        let what_changes = ch.map(|c| {
            let mut out = Vec::new();
            if !c.changed.is_empty() {
                out.push(format!("{} back to the old copy", plural(c.changed.len(), "file")));
            }
            if !c.removed.is_empty() {
                out.push(format!("{} put back", c.removed.len()));
            }
            let mut t = format!("Files: {}.", if out.is_empty() { "No changes".to_string() } else { out.join(", ") });
            if !c.added.is_empty() {
                t.push_str(&format!(" {} made since in its worlds are moved aside with them.", plural(c.added.len(), "file")));
            }
            t
        });
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
                        .children(what_changes.clone())
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
        self.ask_delete_all(vec![b], window, cx)
    }

    /// One question for one backup or several, then each goes the way
    /// a single Delete does.
    fn ask_delete_all(&mut self, list: Vec<Backup>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(first) = list.first() else { return };
        let n = list.len();
        let title = if n == 1 { format!("Delete the backup from {}?", when(first.at)) } else { format!("Delete {n} backups?") };
        let this = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (this, list) = (this.clone(), list.clone());
            alert
                .title(title.clone())
                .description(if n == 1 { "It can't be brought back." } else { "They can't be brought back." })
                .confirm()
                .ok_text("Delete")
                .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let list = list.clone();
                    this.update(cx, |t, cx| {
                        for b in list {
                            t.delete(b, cx);
                        }
                    });
                    true
                })
        });
    }

    /// Asks where to save the backup as a zip (the Downloads folder first),
    /// then downloads it as a job in Tasks, as the Files tab does.
    fn download(&mut self, b: Backup, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let dir = directories::UserDirs::new().and_then(|u| u.download_dir().map(|d| d.to_path_buf())).unwrap_or_default();
        // The panel's name for it: the server, then the backup's own name.
        let server = match &t.local {
            Some(p) => p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "server".into()),
            None => t.id().to_string(),
        };
        let file = format!("{server}-{}.zip", b.name.trim_end_matches(".zip").trim_end_matches(".snap"));
        let rx = cx.prompt_for_new_path(&dir, Some(file.as_str()));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(local))) = rx.await else { return };
            this.update(cx, |this, cx| this.save_zip(b, local, cx)).ok();
        })
        .detach();
    }

    fn save_zip(&mut self, b: Backup, local: PathBuf, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let job = self.store.read(cx).start_job(format!("Downloading the backup from {}", when(b.at)));
        let path = t.sp(&format!("/backups/{}/download", q(&b.name)));
        let shown = local.display().to_string();
        self.error = None;
        self.status = None;
        super::call(
            cx,
            &t,
            move |c| {
                let r = c.save(&path, &local);
                job.finish(&r);
                r
            },
            move |v, r, _| match r {
                Ok(()) => v.status = Some(format!("Saved the backup to {shown}")),
                Err(e) => v.error = Some(e),
            },
        );
        cx.notify();
    }

    /// Old zips into the new format, one after another, after one question.
    fn ask_convert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let names: Vec<String> =
            self.data.ready().map(|d| d.backups.iter().filter(|b| b.zip).map(|b| b.name.clone()).collect()).unwrap_or_default();
        if names.is_empty() {
            return;
        }
        let body = if names.len() == 1 {
            "It then shares its data with the other backups (taking less space) and can be copied off-site. The zip goes once that is done."
        } else {
            "They then share their data with the other backups (taking less space) and can be copied off-site. Each zip goes once that is done."
        };
        let this = cx.entity();
        super::confirm(
            window,
            cx,
            "Convert to the new format?".into(),
            body.into(),
            "Convert",
            false,
            move |_, cx| {
                let names = names.clone();
                this.update(cx, |t, cx| t.convert(names, cx));
            },
            None,
        );
    }

    fn convert(&mut self, mut names: Vec<String>, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        if names.is_empty() {
            return;
        }
        let name = names.remove(0);
        self.busy = Some("Converting".into());
        self.error = None;
        self.status = None;
        let path = t.sp(&format!("/backups/{}/convert", q(&name)));
        job(cx, &self.store.clone(), &t, "Converting a backup", path, Value::Null, move |v, r, cx| {
            match r {
                Ok(_) if !names.is_empty() => return v.convert(names, cx),
                Ok(_) => v.status = Some("Converted".into()),
                Err(e) => v.error = Some(e),
            }
            v.busy = None;
            v.reload(cx);
        });
        cx.notify();
    }

    fn edit_keep(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(k) = self.data.ready().map(|d| d.keep) else { return };
        for (input, v) in self.keep_inputs.iter().zip([k.last, k.daily_days, k.weekly_weeks]) {
            input.update(cx, |st, cx| st.set_value(v.to_string(), window, cx));
        }
        self.keep_edit = true;
        cx.notify();
    }

    fn save_keep(&mut self, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        // As the panel: an empty or bad number is the lowest it can be.
        let [last, daily, weekly] = self.keep_inputs.each_ref().map(|i| i.read(cx).value().trim().parse::<u32>().unwrap_or(0));
        let body = json!({ "last": last.max(1), "daily_days": daily, "weekly_weeks": weekly });
        let path = t.sp("/backups/keep");
        self.error = None;
        super::call(cx, &t, move |c| c.put(&path, body), |v, r, cx| {
            match r {
                Ok(_) => {
                    v.keep_edit = false;
                    v.status = Some("Saved. Applies when the next scheduled backup is made.".into());
                }
                Err(e) => v.error = Some(e),
            }
            v.reload(cx);
        });
    }

    fn save_copies(&mut self, cx: &mut Context<Self>) {
        let (Some(t), Some(on)) = (self.target.clone(), self.copies_edit.clone()) else { return };
        let path = t.sp("/backups/copies");
        let body = json!({ "targets": on.into_iter().collect::<Vec<_>>() });
        self.error = None;
        super::call(cx, &t, move |c| c.put(&path, body), |v, r, cx| {
            match r {
                Ok(_) => v.copies_edit = None,
                Err(e) => v.error = Some(e),
            }
            v.reload(cx);
        });
    }

    /// Where one backup's off-site copies stand, as a chip: none when it
    /// isn't copied anywhere (or is an old zip, which isn't copied).
    fn copies_chip(&self, b: &Backup, i: usize, cx: &App) -> Option<AnyElement> {
        let theme = cx.theme();
        let d = self.data.ready()?;
        let on: Vec<&Place> = d.places.iter().filter(|p| p.on && p.enabled).collect();
        if b.zip || on.is_empty() {
            return None;
        }
        let done = on.iter().filter(|p| p.done.contains(&b.name)).count();
        let failing = on.iter().any(|p| !p.last_error.is_empty() && !p.done.contains(&b.name));
        let (icon, color, text) = if done == on.len() {
            (IconName::Check, theme.success, if on.len() == 1 { "Copied".to_string() } else { format!("{} copies", on.len()) })
        } else if failing {
            (IconName::TriangleAlert, theme.danger, format!("{done} of {}", on.len()))
        } else {
            (IconName::RefreshCw, theme.warning, format!("{done} of {}", on.len()))
        };
        let names = on
            .iter()
            .map(|p| {
                let st = if p.done.contains(&b.name) {
                    "copied".to_string()
                } else if !p.last_error.is_empty() {
                    p.last_error.clone()
                } else if p.working == b.name {
                    "copying now".into()
                } else {
                    "waiting".into()
                };
                format!("{}: {st}", p.name)
            })
            .collect::<Vec<_>>()
            .join("\n");
        Some(
            div()
                .id(("copies", i))
                .flex_none()
                .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(names.clone()).build(w, cx))
                .child(widgets::chip(color).child(h_flex().gap_1().items_center().child(Icon::new(icon).xsmall()).child(text)))
                .into_any_element(),
        )
    }

    /// The keep rules, off-site copies and old zips, under the buttons: the
    /// panel's side cards, as lines.
    fn rules(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let d = self.data.ready()?;
        let theme = cx.theme().clone();
        let busy = self.busy.is_some();
        let remote = self.target.as_ref().is_some_and(|t| t.local.is_none());
        let line = || h_flex().gap_2().items_center().flex_wrap().text_xs().text_color(theme.muted_foreground);
        let link = |id: &'static str, label: &'static str| Button::new(id).link().xsmall().label(label);
        let keep = if self.keep_edit {
            let field = |label: &'static str, input: &Entity<InputState>, unit: Option<&'static str>| {
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(label)
                    .child(div().w_16().child(Input::new(input).small()))
                    .children(unit)
            };
            line()
                .child("Keep")
                .child(field("Newest", &self.keep_inputs[0], None))
                .child(field("One a day for", &self.keep_inputs[1], Some("days")))
                .child(field("One a week for", &self.keep_inputs[2], Some("weeks")))
                .child(
                    Button::new("keep-cancel").outline().xsmall().label("Cancel").on_click(cx.listener(|t, _, _, cx| {
                        t.keep_edit = false;
                        cx.notify();
                    })),
                )
                .child(Button::new("keep-save").primary().xsmall().label("Save").on_click(cx.listener(|t, _, _, cx| t.save_keep(cx))))
        } else {
            line()
                .child(format!(
                    "Keep: {}. Applies to scheduled backups. Manual and before-a-change ones stay until you delete them.",
                    keep_text(d.keep)
                ))
                .child(link("keep-edit", "Edit").on_click(cx.listener(|t, _, w, cx| t.edit_keep(w, cx))))
        };
        // Off-site copies are the panel's (a server on this PC has none).
        let copies = remote.then(|| match &self.copies_edit {
            Some(on) => line()
                .child("Copies off-site:")
                .children(d.places.iter().enumerate().map(|(i, p)| {
                    let id = p.id.clone();
                    Checkbox::new(("copy-to", i))
                        .label(if p.enabled { p.name.clone() } else { format!("{} (off)", p.name) })
                        .checked(on.contains(&p.id))
                        .on_click(cx.listener(move |t, v: &bool, _, cx| {
                            if let Some(s) = t.copies_edit.as_mut() {
                                if *v {
                                    s.insert(id.clone());
                                } else {
                                    s.remove(&id);
                                }
                            }
                            cx.notify();
                        }))
                }))
                .child(
                    Button::new("copies-cancel").outline().xsmall().label("Cancel").on_click(cx.listener(|t, _, _, cx| {
                        t.copies_edit = None;
                        cx.notify();
                    })),
                )
                .child(Button::new("copies-save").primary().xsmall().label("Save").on_click(cx.listener(|t, _, _, cx| t.save_copies(cx))))
                .child(
                    div()
                        .w_full()
                        .child("New backups are copied within a few minutes; old ones too, newest first. Old zips aren't copied (convert them first)."),
                ),
            None => {
                let on: Vec<&Place> = d.places.iter().filter(|p| p.on).collect();
                let text = if on.is_empty() {
                    if d.places.is_empty() {
                        if d.can_edit_places {
                            "Copies off-site: no places to copy to yet. Add one in the panel's Access & devices.".to_string()
                        } else {
                            "Copies off-site: no places to copy to yet.".to_string()
                        }
                    } else {
                        "Copies off-site: not copied anywhere yet.".to_string()
                    }
                } else {
                    let each: Vec<String> = on
                        .iter()
                        .map(|p| format!("{}{}: {}", p.name, if p.enabled { "" } else { " (off)" }, place_status(p)))
                        .collect();
                    format!("Copies off-site: {}", each.join(" · "))
                };
                let failing = on.iter().any(|p| !p.last_error.is_empty());
                line()
                    .child(div().when(failing, |d| d.text_color(theme.danger)).child(text))
                    .when(d.can_edit_places && !d.places.is_empty(), |r| {
                        r.child(link("copies-edit", "Edit").on_click(cx.listener(|t, _, _, cx| {
                            let on = t.data.ready().map(|d| d.places.iter().filter(|p| p.on).map(|p| p.id.clone()).collect()).unwrap_or_default();
                            t.copies_edit = Some(on);
                            cx.notify();
                        })))
                    })
            }
        });
        let zips = (d.zips > 0).then(|| {
            line()
                .child(format!("Old zips from an older Hopper Server: {}.", bytes(d.zips)))
                .child(link("convert-zips", "Convert").disabled(busy).on_click(cx.listener(|t, _, w, cx| t.ask_convert(w, cx))))
        });
        Some(v_flex().gap_1().child(keep).children(copies).children(zips).into_any_element())
    }

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.busy.is_some();
        let theme = cx.theme().clone();
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(div().flex_1().child(Input::new(&self.note).disabled(busy)))
                    // Two buttons, as the web panel's Backups tab has them.
                    .child(
                        Button::new("back-up-worlds")
                            .outline()
                            .icon(IconName::Plus)
                            .label("Back up worlds")
                            .disabled(busy)
                            .on_click(cx.listener(|t, _, _, cx| t.make(true, cx))),
                    )
                    .child(
                        Button::new("back-up-everything")
                            .primary()
                            .icon(IconName::Plus)
                            .label("Back up everything")
                            .disabled(busy)
                            .on_click(cx.listener(|t, _, _, cx| t.make(false, cx))),
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
            return widgets::empty_state(IconName::Archive, "No backups yet", Some("Back up worlds or Back up everything makes the first one.".into()), cx)
                .into_any_element();
        }
        let open = self.open.as_ref().map(|o| o.name.clone());
        let many = self.sel.many();
        let rows: Vec<AnyElement> = d
            .backups
            .iter()
            .enumerate()
            .map(|(i, b)| {
                // Several picked show as picked; else the one open does.
                let sel = if many { self.sel.contains(&b.name) } else { open.as_deref() == Some(b.name.as_str()) };
                let (b1, b2, b3, b4) = (b.clone(), b.clone(), b.clone(), b.clone());
                let copies = self.copies_chip(b, i, cx);
                let row = h_flex()
                    .id(("backup", i))
                    .gap_3()
                    .items_center()
                    .px_3()
                    .py_2()
                    .rounded(theme.radius)
                    .clickable(cx)
                    // A click opens it; Ctrl and Shift pick several.
                    .on_click(cx.listener(move |t, e: &ClickEvent, _, cx| {
                        let order = t.order();
                        if t.sel.click(&b1.name, e.modifiers(), &order) {
                            t.browse(b1.clone(), cx)
                        } else {
                            cx.notify()
                        }
                    }))
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
                                    })
                                    .when(b.zip, |r| {
                                        r.child(
                                            div()
                                                .id(("zip", i))
                                                .flex_none()
                                                .tooltip(|w, cx| {
                                                    gpui_kit::component::tooltip::Tooltip::new("Made by an older Hopper Server: a full zip").build(w, cx)
                                                })
                                                .child(widgets::chip(theme.muted_foreground).child("zip")),
                                        )
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
                    .children(copies)
                    .child(div().text_sm().text_color(theme.muted_foreground).child(bytes(b.size)))
                    .child(
                        Button::new(("download", i))
                            .ghost()
                            .small()
                            .icon(IconName::Download)
                            .tooltip("Download as a zip")
                            .on_click(cx.listener(move |t, _, w, cx| t.download(b4.clone(), w, cx))),
                    )
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
        let bar = many.then(|| {
            let view = cx.entity();
            let v2 = view.clone();
            selection::floating(
                "backups-bar",
                selection::bar(
                    "backups-bar",
                    self.sel.len(),
                    Vec::new(),
                    Some((
                        "Delete".into(),
                        Box::new(move |_, window, cx| {
                            view.update(cx, |t, cx| {
                                let list = t.picked_backups();
                                t.ask_delete_all(list, window, cx)
                            })
                        }),
                    )),
                    move |_, _, cx| {
                        v2.update(cx, |t, cx| {
                            t.sel.clear();
                            cx.notify();
                        })
                    },
                    cx,
                ),
            )
        });
        div()
            .relative()
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|t, e: &KeyDownEvent, window, cx| {
                let order = t.order();
                match selection::on_key(&mut t.sel, e, &order, cx) {
                    KeyAct::None => return,
                    KeyAct::Delete => {
                        let list = t.picked_backups();
                        t.ask_delete_all(list, window, cx)
                    }
                    // The arrows show the backup they land on.
                    KeyAct::Moved(name) if !t.sel.many() => {
                        if let Some(b) = t.data.ready().and_then(|d| d.backups.iter().find(|b| b.name == name).cloned()) {
                            t.browse(b, cx)
                        }
                    }
                    KeyAct::Changed | KeyAct::Moved(_) => {}
                }
                cx.notify();
            }))
            .child(v_flex().id("backup-list").size_full().gap_1().overflow_y_scrollbar().children(rows))
            .children(bar)
            .into_any_element()
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
                        let (path, p2) = (it.path.clone(), it.path.clone());
                        let ticked = self.picked.contains(&it.path);
                        let row = h_flex()
                            .id(("pick-row", i))
                            .gap_2()
                            .items_center()
                            .px_2()
                            .py_1()
                            .rounded(theme.radius)
                            .clickable(cx)
                            // A tick list: a click ticks or unticks the row,
                            // Shift ticks the rows from the last one to here.
                            .on_click(cx.listener(move |t, e: &ClickEvent, _, cx| {
                                let order = t.item_order();
                                t.picked.click(&p2, tick_click(e.modifiers()), &order);
                                cx.notify();
                            }))
                            .child(
                                Checkbox::new(("pick", i))
                                    .checked(ticked)
                                    .on_click(cx.listener(move |t, _: &bool, window, cx| {
                                        // The row's own click would tick it a second time.
                                        cx.stop_propagation();
                                        let order = t.item_order();
                                        t.picked.click(&path, tick_click(window.modifiers()), &order);
                                        cx.notify();
                                    })),
                            )
                            .child(div().flex_1().min_w_0().truncate().text_sm().child(it.path.clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("{} · {}", if it.files == 1 { "1 file".into() } else { format!("{} files", it.files) }, bytes(it.size))),
                            );
                        if ticked { widgets::row_selected(row, cx) } else { row.hover(|s| widgets::row_hover(s, cx)) }
                    });
                    v_flex()
                        .id("backup-entries")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scrollbar()
                        // Ctrl+A ticks every item, Esc unticks them.
                        .on_key_down(cx.listener(|t, e: &KeyDownEvent, _, cx| {
                            let m = &e.keystroke.modifiers;
                            match e.keystroke.key.as_str() {
                                "a" if (m.control || m.platform) && !m.shift && !m.alt => {
                                    let order = t.item_order();
                                    t.picked.select_all(&order);
                                }
                                "escape" if !t.picked.is_empty() => t.picked.clear(),
                                _ => return,
                            }
                            cx.stop_propagation();
                            cx.notify();
                        }))
                        .children(rows)
                        .into_any_element()
                }
            }
        };
        let b2 = b.clone();
        let since = self.since_view(&b.name, cx);
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
                .children(since)
                .child(
                    Button::new("restore-picked")
                        .primary()
                        .small()
                        .icon(IconName::RotateCcw)
                        .label(if picked == 0 { "Restore selected".to_string() } else { format!("Restore selected ({picked})") })
                        .disabled(picked == 0 || busy)
                        .on_click(cx.listener(move |t, _, w, cx| {
                            let mut paths: Vec<String> = t.picked.keys().iter().cloned().collect();
                            paths.sort();
                            t.ask_restore(b2.clone(), paths, w, cx)
                        })),
                )
                .child(body)
                .into_any_element(),
        )
    }
}

impl BackupsTab {
    /// What changed on the server since the open backup, as desktop's
    /// instance backups show it: counts, then up to SHOWN lines.
    fn since_view(&self, name: &str, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (n0, l) = self.since.as_ref()?;
        if n0 != name {
            return None;
        }
        let theme = cx.theme().clone();
        let c = match l {
            Load::Waiting => {
                return Some(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Spinner::new().xsmall())
                        .child(widgets::caption("Comparing with now…", cx).text_xs())
                        .into_any_element(),
                );
            }
            Load::Failed(e) => {
                return Some(div().text_xs().text_color(theme.danger).child(format!("Couldn't compare with now: {e}")).into_any_element());
            }
            Load::Ready(c) => c,
        };
        if c.is_empty() {
            return Some(widgets::caption("Same as now.", cx).text_xs().into_any_element());
        }
        // Changed, then removed (put back), then added.
        let mut lines: Vec<(&str, Hsla, &str, &str)> = c.changed.iter().map(|p| ("~", theme.warning, p.as_str(), "changed")).collect();
        lines.extend(c.removed.iter().map(|p| ("−", theme.danger, p.as_str(), "removed")));
        lines.extend(c.added.iter().map(|p| ("+", theme.success, p.as_str(), "added")));
        let more = lines.len().saturating_sub(SHOWN);
        if !self.since_all {
            lines.truncate(SHOWN);
        }
        let mono = theme.mono_font_family.clone();
        let rows = lines.into_iter().map(|(mark, color, path, what)| {
            h_flex()
                .gap_2()
                .text_xs()
                .child(div().w(rems(0.75)).flex_none().text_color(color).child(mark))
                .child(div().flex_1().min_w_0().truncate().font_family(mono.clone()).child(path.to_string()))
                .child(div().flex_none().text_color(theme.muted_foreground).child(what))
        });
        Some(
            v_flex()
                .gap_1()
                .child(
                    h_flex()
                        .gap_1()
                        .flex_wrap()
                        .child(widgets::caption("Since then:", cx).text_xs())
                        .when(!c.changed.is_empty(), |r| r.child(widgets::chip(theme.warning).child(format!("~{} changed", c.changed.len()))))
                        .when(!c.added.is_empty(), |r| r.child(widgets::chip(theme.success).child(format!("+{} added", c.added.len()))))
                        .when(!c.removed.is_empty(), |r| r.child(widgets::chip(theme.danger).child(format!("−{} removed", c.removed.len())))),
                )
                // Many changes scroll here, so the files below stay in view.
                .child(v_flex().id("since-lines").max_h(px(220.)).overflow_y_scrollbar().children(rows))
                .when(more > 0, |col| {
                    col.child(
                        div()
                            .id("since-all")
                            .text_xs()
                            .text_color(theme.primary)
                            .clickable(cx)
                            .on_click(cx.listener(|t, _, _, cx| {
                                t.since_all = !t.since_all;
                                cx.notify();
                            }))
                            .child(if self.since_all { "Show fewer".to_string() } else { format!("Show all changes ({more} more)") }),
                    )
                })
                .into_any_element(),
        )
    }
}

/// A click in a tick list adds or takes away (as Ctrl does in a plain
/// list), and Shift adds the range rather than replacing the ticks.
fn tick_click(m: Modifiers) -> Modifiers {
    Modifiers { control: true, platform: false, ..m }
}

impl Render for BackupsTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.header(cx);
        let rules = self.rules(cx);
        let list = self.list(window, cx);
        let entries = self.entries(cx);
        v_flex()
            .size_full()
            .gap_3()
            .child(header)
            .children(rules)
            .child(h_flex().flex_1().min_h_0().items_start().child(div().flex_1().min_w_0().h_full().child(list)).children(entries))
    }
}
