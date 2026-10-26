//! A Hopper Server server's Files tab: browse the server folder, edit text
//! files, upload, download, rename, make folders, search inside files, and
//! the trash (restore or delete for good). Like the panel's Files page.

use super::{Load, Target, bytes, call_in, confirm, q};
use crate::store::Store;
use crate::widgets::{self, Clickable as _};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::{Value, json};
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

/// A row of a folder (`GET /fs/{id}/list`).
#[derive(Clone)]
struct Entry {
    name: String,
    dir: bool,
    size: u64,
    modified: u64,
}

/// Something in the trash (`GET /fs/{id}/trash`).
#[derive(Clone)]
struct TrashItem {
    id: String,
    path: String,
    /// "deleted", or "saved-over" for a file's previous version.
    kind: String,
    at: u64,
    size: u64,
    dir: bool,
}

/// A search match (`GET /fs/{id}/search`); line 0 is a name match.
#[derive(Clone)]
struct Hit {
    path: String,
    line: usize,
    text: String,
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").to_string()
}

fn u(v: &Value, k: &str) -> u64 {
    v[k].as_u64().unwrap_or(0)
}

/// The objects in `v[key]`, each read by `f`.
fn list_of<T>(v: &Value, key: &str, f: impl Fn(&Value) -> T) -> Vec<T> {
    v[key].as_array().map(|a| a.iter().map(f).collect()).unwrap_or_default()
}

fn entry(v: &Value) -> Entry {
    Entry { name: s(v, "name"), dir: v["dir"].as_bool().unwrap_or(false), size: u(v, "size"), modified: u(v, "modified") }
}

fn trash_item(v: &Value) -> TrashItem {
    TrashItem {
        id: s(v, "id"),
        path: s(v, "path"),
        kind: s(v, "kind"),
        at: u(v, "at"),
        size: u(v, "size"),
        dir: v["dir"].as_bool().unwrap_or(false),
    }
}

fn hit(v: &Value) -> Hit {
    Hit { path: s(v, "path"), line: u(v, "line") as usize, text: s(v, "text") }
}

/// What the main area shows.
#[derive(Clone, Copy, PartialEq)]
enum View {
    List,
    Trash,
    Search,
}

/// A text file open in the editor pane.
struct Open {
    path: String,
    /// The modified time it was loaded with (the save checks it).
    modified: Option<u64>,
    saved: String,
    state: Entity<EditorState>,
    dirty: bool,
    saving: bool,
    _sub: Subscription,
}

/// What the name bar is asking for.
#[derive(Clone)]
enum Naming {
    Folder,
    Rename(String),
}

pub struct FilesTab {
    store: Entity<Store>,
    target: Option<Target>,
    /// The folder shown, relative to the server folder ("" is the top).
    path: String,
    entries: Load<Vec<Entry>>,
    selected: Option<String>,
    filter: Entity<InputState>,
    query: String,
    view: View,
    trash: Load<Vec<TrashItem>>,
    /// Search matches and whether there were more.
    found: Load<(Vec<Hit>, bool)>,
    open: Option<Open>,
    naming: Option<Naming>,
    name_input: Entity<InputState>,
    /// The last action's outcome.
    note: Option<Result<String, String>>,
    list_scroll: UniformListScrollHandle,
    _subs: Vec<Subscription>,
}

/// Files the editor opens (the rest download).
fn is_text(name: &str) -> bool {
    const EXT: [&str; 30] = [
        "toml", "json", "json5", "properties", "txt", "cfg", "conf", "ini", "yml", "yaml", "js", "ts", "mcmeta", "snbt",
        "log", "md", "mcfunction", "csv", "bat", "sh", "cmd", "ps1", "xml", "zs", "lang", "secret", "list", "nfo", "env",
        "html",
    ];
    let lower = name.to_lowercase();
    lower.rsplit_once('.').is_some_and(|(_, x)| EXT.contains(&x))
}

/// The editor's highlighting for a file name.
fn language(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    match lower.rsplit_once('.').map(|(_, x)| x).unwrap_or("") {
        "toml" => "toml",
        "json" | "json5" | "mcmeta" => "json",
        "yml" | "yaml" => "yaml",
        "js" | "zs" => "javascript",
        "ts" => "typescript",
        "md" => "markdown",
        "sh" => "bash",
        "xml" | "html" => "html",
        _ => "text",
    }
}

fn icon(e: &Entry) -> IconName {
    let lower = e.name.to_lowercase();
    if e.dir {
        IconName::Folder
    } else if is_text(&e.name) {
        IconName::FileText
    } else if [".zip", ".jar", ".mrpack", ".gz", ".7z", ".rar"].iter().any(|x| lower.ends_with(x)) {
        IconName::FileArchive
    } else if [".png", ".jpg", ".jpeg", ".gif", ".webp"].iter().any(|x| lower.ends_with(x)) {
        IconName::Image
    } else {
        IconName::File
    }
}


fn when(t: u64) -> String {
    if t == 0 { String::new() } else { hopper::time::relative_secs(t as i64) }
}

fn name_of(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

fn parent_of(path: &str) -> String {
    path.rsplit_once('/').map(|(p, _)| p.to_string()).unwrap_or_default()
}



impl FilesTab {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter · Enter searches inside"));
        let name_input = cx.new(|cx| InputState::new(window, cx));
        let subs = vec![
            cx.subscribe_in(&filter, window, |this, state, e: &InputEvent, window, cx| match e {
                InputEvent::Change => {
                    this.query = state.read(cx).value().trim().to_lowercase();
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.search(window, cx),
                _ => {}
            }),
            cx.subscribe_in(&name_input, window, |this, _, e: &InputEvent, window, cx| {
                if matches!(e, InputEvent::PressEnter { .. }) {
                    this.submit_name(window, cx);
                }
            }),
        ];
        Self {
            store,
            target: None,
            path: String::new(),
            entries: Load::Waiting,
            selected: None,
            filter,
            query: String::new(),
            view: View::List,
            trash: Load::Waiting,
            found: Load::Waiting,
            open: None,
            naming: None,
            name_input,
            note: None,
            list_scroll: UniformListScrollHandle::new(),
            _subs: subs,
        }
    }

    /// Shows `target`'s Files (again when it's the same server: a refresh).
    pub fn show(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        if self.target.as_ref() != Some(&target) {
            self.target = Some(target);
            self.open = None;
            self.note = None;
            self.go(String::new(), window, cx);
        } else {
            self.target = Some(target);
            match self.view {
                View::Trash => self.load_trash(cx),
                _ => self.load(cx),
            }
        }
        cx.notify();
    }

    fn join(&self, name: &str) -> String {
        if self.path.is_empty() { name.to_string() } else { format!("{}/{name}", self.path) }
    }

    // ---- loading -----------------------------------------------------------

    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let path = self.path.clone();
        super::get(cx, &t, t.fp(&format!("/list?path={}", q(&path))), move |this, r, _| {
            if this.path != path {
                return;
            }
            let r = r.map(|v| list_of(&v, "entries", entry));
            if let Ok(list) = &r
                && this.selected.as_ref().is_some_and(|s| !list.iter().any(|e| &e.name == s))
            {
                this.selected = None;
            }
            this.entries = Load::from(r);
        });
    }

    fn go(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        self.path = path;
        self.selected = None;
        self.entries = Load::Waiting;
        self.view = View::List;
        self.naming = None;
        self.query.clear();
        self.filter.update(cx, |s, cx| s.set_value("", window, cx));
        self.list_scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.load(cx);
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        match self.view {
            View::Trash => self.load_trash(cx),
            _ => self.load(cx),
        }
    }

    fn shown(&self) -> Vec<Entry> {
        let Some(all) = self.entries.ready() else { return Vec::new() };
        if self.query.is_empty() {
            return all.clone();
        }
        all.iter().filter(|e| hopper::fuzzy::words_match(&self.query, &[&e.name])).cloned().collect()
    }

    // ---- opening -----------------------------------------------------------

    fn open_entry(&mut self, e: &Entry, window: &mut Window, cx: &mut Context<Self>) {
        let p = self.join(&e.name);
        if e.dir {
            self.go(p, window, cx);
        } else if is_text(&e.name) || e.size < 64 * 1024 {
            self.edit(p, window, cx);
        } else {
            self.selected = Some(e.name.clone());
            self.download(p, window, cx);
        }
    }

    fn edit(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        if self.open.as_ref().is_some_and(|o| o.dirty && o.path != path) {
            let view = cx.entity();
            let now = self.open.as_ref().map(|o| name_of(&o.path)).unwrap_or_default();
            confirm(
                window,
                cx,
                format!("Close {now} without saving?"),
                "Your changes to it are lost.".into(),
                "Close",
                true,
                move |w, cx| {
                    let path = path.clone();
                    view.update(cx, |this, cx| {
                        this.open = None;
                        this.edit(path, w, cx);
                    })
                },
                None,
            );
            return;
        }
        let url = t.fp(&format!("/text?path={}", q(&path)));
        call_in(window, cx, &t, move |c| c.get(&url), move |this, r, window, cx| {
            match r {
                Ok(v) => {
                    let content = v["content"].as_str().unwrap_or("").to_string();
                    let lang = language(&path);
                    let state = cx.new(|cx| EditorState::new(window, cx).language(lang).default_value(content));
                    let saved = state.read(cx).value().to_string();
                    let sub = cx.subscribe(&state, |this, state, e: &InputEvent, cx| {
                        if matches!(e, InputEvent::Change)
                            && let Some(o) = this.open.as_mut()
                            && o.state == state
                        {
                            let dirty = state.read(cx).value().as_ref() != o.saved.as_str();
                            if dirty != o.dirty {
                                o.dirty = dirty;
                                cx.notify();
                            }
                        }
                    });
                    this.selected = Some(name_of(&path));
                    this.open = Some(Open {
                        path,
                        modified: v["modified"].as_u64(),
                        saved,
                        state,
                        dirty: false,
                        saving: false,
                        _sub: sub,
                    });
                }
                Err(e) => this.note = Some(Err(e)),
            }
        });
    }

    fn save(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let Some(o) = self.open.as_mut() else { return };
        if o.saving {
            return;
        }
        o.saving = true;
        let content = o.state.read(cx).value().to_string();
        let path = o.path.clone();
        let body = json!({ "path": path, "content": content, "modified": o.modified, "force": force });
        let url = t.fp("/text");
        call_in(window, cx, &t, move |c| c.put(&url, body), move |this, r, window, cx| {
            let Some(o) = this.open.as_mut().filter(|o| o.path == path) else { return };
            o.saving = false;
            let name = name_of(&path);
            let v = match r {
                Ok(v) => v,
                Err(e) => {
                    this.note = Some(Err(e));
                    return;
                }
            };
            let view = cx.entity();
            let again = move |w: &mut Window, cx: &mut App| view.update(cx, |this, cx| this.save(true, w, cx));
            match v["result"].as_str().unwrap_or("") {
                "saved" => {
                    o.modified = v["modified"].as_u64();
                    o.dirty = o.state.read(cx).value().as_ref() != content.as_str();
                    o.saved = content;
                    this.note = Some(Ok(format!("Saved {name}")));
                    this.load(cx);
                }
                "invalid" => confirm(
                    window,
                    cx,
                    "Save with this error?".into(),
                    v["error"].as_str().unwrap_or("The file has a syntax error.").to_string(),
                    "Save anyway",
                    true,
                    again,
                    None,
                ),
                "conflict" => confirm(
                    window,
                    cx,
                    format!("{name} changed on the server"),
                    "Someone or something saved it after you opened it. Save yours over it? The other version goes to the trash."
                        .into(),
                    "Save over it",
                    true,
                    again,
                    None,
                ),
                other => this.note = Some(Err(format!("The panel answered \"{other}\"."))),
            }
        });
        cx.notify();
    }

    fn close_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(o) = self.open.as_ref() else { return };
        if !o.dirty {
            self.open = None;
            cx.notify();
            return;
        }
        let view = cx.entity();
        confirm(
            window,
            cx,
            format!("Close {} without saving?", name_of(&o.path)),
            "Your changes are lost.".into(),
            "Close",
            true,
            move |_, cx| {
                view.update(cx, |this, cx| {
                    this.open = None;
                    cx.notify();
                })
            },
            None,
        );
    }

    // ---- operations --------------------------------------------------------

    fn ops(&mut self, ops: Value, message: String, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let url = t.fp("/ops");
        super::call(cx, &t, move |c| c.post(&url, json!({ "ops": ops })), move |this, r, cx| {
            this.note = Some(r.map(|_| message));
            this.load(cx);
        });
    }

    fn start_naming(&mut self, n: Naming, window: &mut Window, cx: &mut Context<Self>) {
        let (value, placeholder) = match &n {
            Naming::Folder => (String::new(), "Folder name"),
            Naming::Rename(name) => (name.clone(), "New name"),
        };
        self.name_input.update(cx, |s, cx| {
            s.set_placeholder(placeholder, window, cx);
            s.set_value(value, window, cx);
            s.focus(window, cx);
        });
        self.naming = Some(n);
        cx.notify();
    }

    fn submit_name(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(n) = self.naming.clone() else { return };
        let name = self.name_input.read(cx).value().trim().to_string();
        if name.is_empty() {
            return;
        }
        self.naming = None;
        match n {
            Naming::Folder => {
                let p = self.join(&name);
                self.ops(json!([{ "op": "mkdir", "path": p }]), format!("Made {name}"), cx);
            }
            Naming::Rename(old) => {
                if old == name {
                    cx.notify();
                    return;
                }
                let p = self.join(&old);
                if self.open.as_ref().is_some_and(|o| o.path == p && !o.dirty) {
                    self.open = None;
                }
                self.ops(json!([{ "op": "rename", "path": p, "name": name }]), format!("Renamed to {name}"), cx);
            }
        }
    }

    fn trash_entry(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let p = self.join(&name);
        let view = cx.entity();
        confirm(
            window,
            cx,
            format!("Move {name} to the trash?"),
            "You can restore it from the trash for 30 days.".into(),
            "Move to trash",
            true,
            move |_, cx| {
                let (p, name) = (p.clone(), name.clone());
                view.update(cx, |this, cx| {
                    if this.open.as_ref().is_some_and(|o| o.path == p || o.path.starts_with(&format!("{p}/"))) {
                        this.open = None;
                    }
                    this.ops(json!([{ "op": "trash", "paths": [p] }]), format!("Moved {name} to the trash"), cx);
                })
            },
            None,
        );
    }

    // ---- transfers ---------------------------------------------------------

    fn pick_upload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Upload".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            this.update_in(cx, |this, window, cx| this.upload(paths, window, cx)).ok();
        })
        .detach();
    }

    /// Uploads `files` into the folder shown, asking first before replacing.
    fn upload(&mut self, files: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if files.is_empty() {
            return;
        }
        let names: Vec<String> = self.entries.ready().map(|l| l.iter().map(|e| e.name.clone()).collect()).unwrap_or_default();
        let clash: Vec<String> = files
            .iter()
            .filter_map(|f| f.file_name().map(|n| n.to_string_lossy().into_owned()))
            .filter(|n| names.contains(n))
            .collect();
        if clash.is_empty() {
            self.start_upload(files, cx);
            return;
        }
        let view = cx.entity();
        let title = if clash.len() == 1 { format!("Replace {}?", clash[0]) } else { format!("Replace {} files?", clash.len()) };
        let mut list = clash.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
        if clash.len() > 5 {
            list.push('…');
        }
        let rest: Vec<PathBuf> = files
            .iter()
            .filter(|f| f.file_name().is_none_or(|n| !clash.contains(&n.to_string_lossy().into_owned())))
            .cloned()
            .collect();
        let v2 = view.clone();
        confirm(
            window,
            cx,
            title,
            format!("{list} already {} there. The old copies go to the trash.", if clash.len() == 1 { "exists" } else { "exist" }),
            "Replace",
            false,
            move |_, cx| {
                let files = files.clone();
                view.update(cx, |this, cx| this.start_upload(files, cx))
            },
            Some(Rc::new(move |_, cx| {
                let rest = rest.clone();
                v2.update(cx, |this, cx| this.start_upload(rest, cx))
            })),
        );
    }

    fn start_upload(&mut self, files: Vec<PathBuf>, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        if files.is_empty() {
            return;
        }
        let dir = self.path.clone();
        let n = files.len();
        let title = if n == 1 {
            format!("Uploading {}", files[0].file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default())
        } else {
            format!("Uploading {n} files")
        };
        let job = self.store.read(cx).start_job(title);
        let id = t.id().to_string();
        let to = dir.clone();
        super::call(
            cx,
            &t,
            move |c| {
                let r: anyhow::Result<()> = (|| {
                    for (i, f) in files.iter().enumerate() {
                        job.check_cancelled()?;
                        let name = f.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                        job.detail(name.clone());
                        job.progress(i, n);
                        let rel = if to.is_empty() { name } else { format!("{to}/{name}") };
                        c.upload(&id, f, &rel)?;
                    }
                    job.progress(n, n);
                    Ok(())
                })();
                job.finish(&r);
                r
            },
            move |this, r, cx| {
                this.note = Some(r.map(|_| if n == 1 { "Uploaded 1 file".to_string() } else { format!("Uploaded {n} files") }));
                if this.path == dir {
                    this.load(cx);
                }
            },
        );
    }

    fn download(&mut self, rel: String, window: &mut Window, cx: &mut Context<Self>) {
        let dir = directories::UserDirs::new().and_then(|u| u.download_dir().map(|d| d.to_path_buf())).unwrap_or_default();
        let name = name_of(&rel);
        let rx = cx.prompt_for_new_path(&dir, Some(name.as_str()));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(local))) = rx.await else { return };
            this.update(cx, |this, cx| this.start_download(rel, local, cx)).ok();
        })
        .detach();
    }

    fn start_download(&mut self, rel: String, local: PathBuf, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let name = name_of(&rel);
        let job = self.store.read(cx).start_job(format!("Downloading {name}"));
        let id = t.id().to_string();
        let shown = local.display().to_string();
        super::call(
            cx,
            &t,
            move |c| {
                let r = c.download(&id, &rel, &local);
                job.finish(&r);
                r
            },
            move |this, r, _| this.note = Some(r.map(|_| format!("Saved {name} to {shown}"))),
        );
    }

    // ---- trash -------------------------------------------------------------

    fn open_trash(&mut self, cx: &mut Context<Self>) {
        self.view = View::Trash;
        self.trash = Load::Waiting;
        self.load_trash(cx);
        cx.notify();
    }

    fn load_trash(&mut self, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        super::get(cx, &t, t.fp("/trash"), |this, r, _| {
            this.trash = Load::from(r.map(|v| list_of(&v, "items", trash_item)));
        });
    }

    fn restore(&mut self, item: TrashItem, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let url = t.fp("/trash/restore");
        let id = item.id.clone();
        super::call(cx, &t, move |c| c.post(&url, json!({ "id": id })), move |this, r, cx| {
            this.note = Some(r.map(|v| {
                let p = v["path"].as_str().unwrap_or(&item.path).to_string();
                if item.kind == "saved-over" { format!("Restored the older {p}") } else { format!("Restored {p}") }
            }));
            this.load_trash(cx);
            this.load(cx);
        });
    }

    /// Deletes trash items for good (all of them when `ids` is empty).
    fn purge(&mut self, ids: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let all = ids.is_empty();
        let count = self.trash.ready().map_or(0, |l| l.len());
        let view = cx.entity();
        confirm(
            window,
            cx,
            if all { "Empty the trash?".into() } else { "Delete this for good?".into() },
            if all { format!("{count} items are deleted for good. This can't be undone.") } else { "This can't be undone.".into() },
            "Delete for good",
            true,
            move |_, cx| {
                let ids = ids.clone();
                view.update(cx, |this, cx| {
                    let Some(t) = this.target.clone() else { return };
                    let url = t.fp("/trash/purge");
                    super::call(cx, &t, move |c| c.post(&url, json!({ "ids": ids })), |this, r, cx| {
                        this.note = Some(r.map(|v| format!("Deleted {}", v["removed"].as_u64().unwrap_or(0))));
                        this.load_trash(cx);
                    });
                })
            },
            None,
        );
    }

    // ---- search ------------------------------------------------------------

    fn search(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let query = self.filter.read(cx).value().trim().to_string();
        if query.chars().count() < 2 {
            self.note = Some(Err("Search for at least 2 characters.".into()));
            cx.notify();
            return;
        }
        self.view = View::Search;
        self.found = Load::Waiting;
        let url = t.fp(&format!("/search?path={}&q={}", q(&self.path), q(&query)));
        super::get(cx, &t, url, |this, r, _| {
            this.found = Load::from(r.map(|v| (list_of(&v, "hits", hit), v["more"].as_bool().unwrap_or(false))));
        });
        cx.notify();
    }

    // ---- drawing -----------------------------------------------------------

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let crumb = |id: ElementId, label: SharedString, to: String, current: bool, cx: &mut Context<Self>| {
            let theme = cx.theme();
            div()
                .id(id)
                .clickable(cx)
                .flex_none()
                .px_1p5()
                .py_0p5()
                .rounded(theme.radius)
                .whitespace_nowrap()
                .text_color(if current { theme.foreground } else { theme.muted_foreground })
                .when(current, |d| d.font_semibold())
                .hover(|s| widgets::row_hover(s, cx))
                .child(label)
                .on_click(cx.listener(move |this, _, window, cx| this.go(to.clone(), window, cx)))
        };
        let segs: Vec<String> = if self.path.is_empty() { Vec::new() } else { self.path.split('/').map(String::from).collect() };
        let n = segs.len();
        let mut crumbs = h_flex()
            .flex_1()
            .min_w_0()
            .overflow_x_hidden()
            .gap_0p5()
            .px_1()
            .h_8()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .text_sm()
            .child(Icon::new(IconName::HardDrive).small().flex_none().text_color(theme.muted_foreground).ml_1())
            .child(crumb("crumb-root".into(), "Server folder".into(), String::new(), n == 0, cx));
        for (i, s) in segs.iter().enumerate() {
            let to = segs[..=i].join("/");
            crumbs = crumbs
                .child(Icon::new(IconName::ChevronRight).xsmall().flex_none().text_color(cx.theme().muted_foreground))
                .child(crumb(("crumb", i).into(), s.clone().into(), to, i + 1 == n, cx));
        }
        let trash = self.view == View::Trash;
        h_flex()
            .gap_1()
            .child(
                Button::new("files-up")
                    .ghost()
                    .small()
                    .icon(IconName::ArrowUp)
                    .tooltip("Up")
                    .disabled(self.path.is_empty() && !trash)
                    .on_click(cx.listener(|this, _, window, cx| {
                        if this.view != View::List {
                            this.view = View::List;
                            cx.notify();
                        } else {
                            let p = parent_of(&this.path);
                            this.go(p, window, cx);
                        }
                    })),
            )
            .child(crumbs)
            .child(div().w_56().flex_none().child(Input::new(&self.filter).small().cleanable(true)))
            .child(
                Button::new("files-search")
                    .ghost()
                    .small()
                    .icon(IconName::TextSearch)
                    .tooltip("Search inside files here and below")
                    .on_click(cx.listener(|this, _, window, cx| this.search(window, cx))),
            )
            .child(
                Button::new("files-refresh")
                    .ghost()
                    .small()
                    .icon(IconName::RefreshCw)
                    .tooltip("Refresh")
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
            .child(
                Button::new("files-mkdir")
                    .ghost()
                    .small()
                    .icon(IconName::FolderPlus)
                    .tooltip("New folder")
                    .on_click(cx.listener(|this, _, window, cx| this.start_naming(Naming::Folder, window, cx))),
            )
            .child(
                Button::new("files-upload")
                    .primary()
                    .small()
                    .icon(IconName::Upload)
                    .label("Upload")
                    .on_click(cx.listener(|this, _, window, cx| this.pick_upload(window, cx))),
            )
            .child(
                Button::new("files-trash")
                    .ghost()
                    .small()
                    .icon(IconName::Trash)
                    .tooltip("Trash")
                    .when(trash, |b| b.outline())
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.view == View::Trash {
                            this.view = View::List;
                            cx.notify();
                        } else {
                            this.open_trash(cx);
                        }
                    })),
            )
    }

    fn name_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let n = self.naming.as_ref()?;
        let (label, action) = match n {
            Naming::Folder => ("New folder in this folder".to_string(), "Create"),
            Naming::Rename(name) => (format!("Rename {name}"), "Rename"),
        };
        Some(
            h_flex()
                .gap_2()
                .child(widgets::caption(label, cx))
                .child(div().w_64().child(Input::new(&self.name_input).small()))
                .child(
                    Button::new("name-ok")
                        .small()
                        .outline()
                        .label(action)
                        .on_click(cx.listener(|this, _, window, cx| this.submit_name(window, cx))),
                )
                .child(Button::new("name-cancel").small().ghost().label("Cancel").on_click(cx.listener(|this, _, _, cx| {
                    this.naming = None;
                    cx.notify();
                }))),
        )
    }

    fn rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let shown = self.shown();
        let view = cx.entity();
        range
            .filter_map(|i| shown.get(i).cloned().map(|e| (i, e)))
            .map(|(i, e)| {
                let theme = cx.theme();
                let selected = self.selected.as_deref() == Some(e.name.as_str());
                let (e1, v) = (e.clone(), view.clone());
                h_flex()
                    .id(("file", i))
                    .clickable(cx)
                    .w_full()
                    .h(rems(2.25))
                    .px_3()
                    .gap_3()
                    .text_sm()
                    .rounded(theme.radius)
                    .when(selected, |r| widgets::row_selected(r, cx))
                    .when(!selected, |r| r.hover(|s| widgets::row_hover(s, cx)))
                    .child(
                        Icon::new(icon(&e))
                            .small()
                            .flex_none()
                            .text_color(if e.dir { theme.primary } else { theme.muted_foreground }),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(e.name.clone()))
                    .child(div().w_32().flex_none().text_color(theme.muted_foreground).child(when(e.modified)))
                    .child(
                        div()
                            .w_20()
                            .flex_none()
                            .text_right()
                            .text_color(theme.muted_foreground)
                            .child(if e.dir { String::new() } else { bytes(e.size) }),
                    )
                    .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                        let e = e1.clone();
                        // A folder or text file opens on a click; other
                        // files select, and download on a double-click.
                        if e.dir || is_text(&e.name) || ev.click_count() >= 2 {
                            this.open_entry(&e, window, cx);
                        } else {
                            this.selected = Some(e.name.clone());
                        }
                        cx.notify();
                    }))
                    .context_menu(move |menu, _, _| {
                        let e = e.clone();
                        let (v1, v2, v3, v4) = (v.clone(), v.clone(), v.clone(), v.clone());
                        let (e1, e2, e3, e4) = (e.clone(), e.clone(), e.clone(), e.clone());
                        menu.item(
                            PopupMenuItem::new(if e.dir { "Open" } else if is_text(&e.name) || e.size < 64 * 1024 { "Edit" } else { "Open" })
                                .icon(if e.dir { IconName::FolderOpen } else { IconName::Pencil })
                                .on_click(move |_, w, cx| v1.update(cx, |this, cx| this.open_entry(&e1, w, cx))),
                        )
                        .item(
                            PopupMenuItem::new("Download")
                                .icon(IconName::Download)
                                .disabled(e.dir)
                                .on_click(move |_, w, cx| {
                                    v2.update(cx, |this, cx| {
                                        let p = this.join(&e2.name);
                                        this.download(p, w, cx)
                                    })
                                }),
                        )
                        .item(PopupMenuItem::new("Rename").icon(IconName::Pencil).on_click(move |_, w, cx| {
                            v3.update(cx, |this, cx| {
                                this.selected = Some(e3.name.clone());
                                this.start_naming(Naming::Rename(e3.name.clone()), w, cx)
                            })
                        }))
                        .separator()
                        .item(
                            PopupMenuItem::new("Move to trash")
                                .icon(IconName::Trash)
                                .on_click(move |_, w, cx| v4.update(cx, |this, cx| this.trash_entry(e4.name.clone(), w, cx))),
                        )
                    })
                    .into_any_element()
            })
            .collect()
    }

    fn list(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(el) = super::load_state(&self.entries, cx) {
            return el;
        }
        let n = self.shown().len();
        if n == 0 {
            let (title, body) = if self.query.is_empty() {
                ("This folder is empty", Some("Upload files into it with Upload.".into()))
            } else {
                ("Nothing matches", Some("Press Enter to search inside the files too.".into()))
            };
            return widgets::empty_state(IconName::FolderOpen, title, body, cx).into_any_element();
        }
        let theme = cx.theme();
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .px_3()
                    .gap_3()
                    .h_7()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().w_4().flex_none())
                    .child(div().flex_1().child("Name"))
                    .child(div().w_32().flex_none().child("Changed"))
                    .child(div().w_20().flex_none().text_right().child("Size")),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        uniform_list("files-list", n, cx.processor(|this, range: Range<usize>, _, cx| this.rows(range, cx)))
                            .track_scroll(&self.list_scroll)
                            .size_full(),
                    )
                    .vertical_scrollbar(&self.list_scroll),
            )
            .into_any_element()
    }

    fn trash_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let items = self.trash.ready().cloned().unwrap_or_default();
        let head = h_flex()
            .gap_2()
            .pb_2()
            .child(div().font_semibold().child("Trash"))
            .child(widgets::caption("Deleted files and older versions of saved files, kept 30 days.", cx))
            .child(div().flex_1())
            .child(
                Button::new("trash-empty")
                    .small()
                    .outline()
                    .icon(IconName::Trash)
                    .label("Empty trash")
                    .disabled(items.is_empty())
                    .on_click(cx.listener(|this, _, window, cx| this.purge(Vec::new(), window, cx))),
            );
        let body = if let Some(el) = super::load_state(&self.trash, cx) {
            el
        } else if items.is_empty() {
            widgets::empty_state(IconName::Trash, "The trash is empty", None, cx).into_any_element()
        } else {
            v_flex()
                .id("trash-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .children(items.into_iter().enumerate().map(|(i, it)| {
                    let (icon, color) = if it.dir {
                        (IconName::Folder, theme.primary)
                    } else if it.kind == "saved-over" {
                        (IconName::FileClock, theme.info)
                    } else {
                        (IconName::File, theme.muted_foreground)
                    };
                    let what = if it.kind == "saved-over" { "Version before a save" } else { "Deleted" };
                    let detail = format!("{what} · {} · {}", when(it.at), bytes(it.size));
                    let (i1, id) = (it.clone(), it.id.clone());
                    h_flex()
                        .id(("trash", i))
                        .px_3()
                        .py_1p5()
                        .gap_3()
                        .rounded(theme.radius)
                        .hover(|s| widgets::row_hover(s, cx))
                        .child(Icon::new(icon).small().flex_none().text_color(color))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_sm().truncate().child(it.path.clone()))
                                .child(widgets::caption(detail, cx)),
                        )
                        .child(
                            Button::new(("restore", i))
                                .small()
                                .outline()
                                .icon(IconName::RotateCcw)
                                .label("Restore")
                                .on_click(cx.listener(move |this, _, _, cx| this.restore(i1.clone(), cx))),
                        )
                        .child(
                            Button::new(("purge", i))
                                .small()
                                .ghost()
                                .icon(IconName::Trash)
                                .tooltip("Delete for good")
                                .on_click(cx.listener(move |this, _, window, cx| this.purge(vec![id.clone()], window, cx))),
                        )
                }))
                .into_any_element()
        };
        v_flex().size_full().child(head).child(body).into_any_element()
    }

    fn search_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let close = Button::new("search-close")
            .small()
            .ghost()
            .icon(IconName::X)
            .tooltip("Close search")
            .on_click(cx.listener(|this, _, _, cx| {
                this.view = View::List;
                cx.notify();
            }));
        let Some((hits, more)) = self.found.ready() else {
            let state = super::load_state(&self.found, cx).unwrap_or_else(|| div().into_any_element());
            return v_flex()
                .size_full()
                .child(h_flex().child(widgets::caption("Searching…", cx)).child(div().flex_1()).child(close))
                .child(state)
                .into_any_element();
        };
        // Matches grouped by file, in the order they came.
        let mut groups: Vec<(String, Vec<Hit>)> = Vec::new();
        for h in hits {
            if groups.last().is_none_or(|g| g.0 != h.path) {
                groups.push((h.path.clone(), Vec::new()));
            }
            if h.line > 0
                && let Some(g) = groups.last_mut()
            {
                g.1.push(h.clone());
            }
        }
        let files = groups.len();
        let summary = format!(
            "{}{} matches in {files} file{}",
            hits.len(),
            if *more { "+" } else { "" },
            if files == 1 { "" } else { "s" }
        );
        let body = if groups.is_empty() {
            widgets::empty_state(IconName::Search, "Nothing found", None, cx).into_any_element()
        } else {
            v_flex()
                .id("search-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .gap_2()
                .children(groups.into_iter().enumerate().map(|(gi, (path, lines))| {
                    let p1 = path.clone();
                    let extra = lines.len().saturating_sub(20);
                    v_flex()
                        .child(
                            div()
                                .id(("hit-file", gi))
                                .clickable(cx)
                                .px_2()
                                .py_0p5()
                                .rounded(theme.radius)
                                .text_sm()
                                .font_family(theme.mono_font_family.clone())
                                .hover(|s| widgets::row_hover(s, cx))
                                .child(path.clone())
                                .on_click(cx.listener(move |this, _, window, cx| this.edit(p1.clone(), window, cx))),
                        )
                        .children(lines.into_iter().take(20).enumerate().map(|(li, h)| {
                            let p2 = path.clone();
                            h_flex()
                                .id(("hit", gi * 1000 + li))
                                .clickable(cx)
                                .pl_4()
                                .pr_2()
                                .gap_2()
                                .rounded(theme.radius)
                                .text_xs()
                                .font_family(theme.mono_font_family.clone())
                                .hover(|s| widgets::row_hover(s, cx))
                                .child(div().w_10().flex_none().text_right().text_color(theme.muted_foreground).child(h.line.to_string()))
                                .child(div().flex_1().min_w_0().truncate().child(h.text))
                                .on_click(cx.listener(move |this, _, window, cx| this.edit(p2.clone(), window, cx)))
                        }))
                        .when(extra > 0, |c| c.child(div().pl_4().child(widgets::caption(format!("and {extra} more"), cx))))
                }))
                .into_any_element()
        };
        v_flex()
            .size_full()
            .gap_2()
            .child(h_flex().child(widgets::caption(summary, cx)).child(div().flex_1()).child(close))
            .child(body)
            .into_any_element()
    }

    fn editor_pane(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let o = self.open.as_ref()?;
        let theme = cx.theme();
        Some(
            v_flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .gap_2()
                .child(
                    h_flex()
                        .gap_2()
                        .child(Icon::new(IconName::FileText).small().flex_none().text_color(theme.muted_foreground))
                        .child(div().min_w_0().truncate().font_semibold().child(name_of(&o.path)))
                        .child(div().min_w_0().truncate().child(widgets::caption(o.path.clone(), cx)))
                        .when(o.dirty, |r| r.child(widgets::chip(cx.theme().warning).child("Unsaved")))
                        .child(div().flex_1())
                        .child(
                            Button::new("editor-save")
                                .small()
                                .primary()
                                .icon(IconName::Save)
                                .label(if o.saving { "Saving…" } else { "Save" })
                                .disabled(!o.dirty || o.saving)
                                .on_click(cx.listener(|this, _, window, cx| this.save(false, window, cx))),
                        )
                        .child(
                            Button::new("editor-close")
                                .small()
                                .ghost()
                                .icon(IconName::X)
                                .tooltip("Close")
                                .on_click(cx.listener(|this, _, window, cx| this.close_editor(window, cx))),
                        ),
                )
                .child(div().flex_1().min_h_0().child(Editor::new(&o.state).size_full()))
                .into_any_element(),
        )
    }
}

impl Render for FilesTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let main = match self.view {
            View::List => self.list(cx),
            View::Trash => self.trash_view(cx),
            View::Search => self.search_view(cx),
        };
        let editor = self.editor_pane(cx);
        let note = self.note.clone().map(|n| {
            let theme = cx.theme();
            let (text, color) = match n {
                Ok(t) => (t, theme.muted_foreground),
                Err(e) => (e, theme.danger),
            };
            h_flex()
                .gap_2()
                .text_sm()
                .text_color(color)
                .child(div().flex_1().min_w_0().child(text))
                .child(Button::new("note-x").xsmall().ghost().icon(IconName::X).on_click(cx.listener(|this, _, _, cx| {
                    this.note = None;
                    cx.notify();
                })))
        });
        let has_editor = editor.is_some();
        v_flex()
            .size_full()
            .gap_2()
            .child(self.toolbar(cx))
            .children(self.name_bar(cx))
            .children(note)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .items_start()
                    .gap_3()
                    .child(
                        div()
                            .h_full()
                            .min_w_0()
                            .map(|d| if has_editor { d.w(relative(0.4)).flex_none() } else { d.flex_1() })
                            .child(main),
                    )
                    .children(editor),
            )
    }
}
