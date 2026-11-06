//! The config view: a mod's config files as settings, shared by the three
//! places it opens (the Configs tab, the side panel from a mod's config
//! button, the dialog from a crash card or a mod's menu). Each setting gets
//! a control for its kind (switch, slider, number, choice, list, colour,
//! text), a dot when it differs from the file's default, Reset, and its
//! timing when the file says. Edits stay unsaved until Save, which checks
//! them together, backs up the old copy and writes only those values.

use crate::store::Store;
use crate::switch::Switch;
use crate::widgets::{self, Clickable as _};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
use gpui_kit::component::dialog::{Cancel, Confirm};
use gpui_kit::component::highlighter::{Diagnostic, DiagnosticSeverity};
use gpui_kit::component::input::{
    Editor, EditorState, Input, InputEvent, InputState, Position, RangeDecoration, RangeDecorationCollection, RangeDecorationStyle,
};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::configedit::{self, Edit, Entry, Format, Kind, Restart, Section, Value};
use hopper::configindex::{CfgFile, Side};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

/// Where to open: the setting on this line (1-based), with a note on it.
#[derive(Clone, Default)]
pub struct Focus {
    pub line: usize,
    pub note: Option<String>,
}

/// Which place the view is drawn in: the panel is narrow, so controls go
/// under the names.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Host {
    Dialog,
    Panel,
    Tab,
}

/// Where the files are: on this PC (an instance, or a server on This PC),
/// or on a Hopper Server, read and saved through its panel (the panel's
/// Configs routes, which check every path and back up before writing).
#[derive(Clone)]
pub enum Source {
    Local,
    Panel(crate::panel_tabs::Target),
}

/// The view is a server's (its Configs tab): saves say when the server
/// picks them up, and the files may be on a Hopper Server.
#[derive(Clone)]
pub struct ServerSide {
    pub source: Source,
    /// The server is running right now.
    pub running: bool,
}

pub enum ConfigViewEvent {
    /// The panel's close button.
    Close,
    /// "Open in Configs tab".
    OpenInTab,
    /// Saved or discarded: counts changed.
    Changed,
}

impl EventEmitter<ConfigViewEvent> for ConfigView {}

/// One file: as read, the edits on it so far, and what it shows with them.
struct FileState {
    base: String,
    edits: Vec<Edit>,
    /// The text after the edits (or as typed in the text view).
    text: String,
    sections: Result<Vec<Section>, String>,
    /// Edited as text: saved as a whole.
    typed: bool,
    problem: Option<String>,
}

impl FileState {
    /// A file whose text was read already (here, or by a server's panel),
    /// or why it couldn't be.
    fn of_text(rel: &str, text: Result<String, String>) -> FileState {
        let (text, problem) = match text {
            Ok(t) => (t, None),
            Err(e) => (String::new(), Some(e)),
        };
        let sections = match Format::of(rel) {
            Some(f) if problem.is_none() => configedit::parse_as(f, &text),
            _ => Err(String::new()),
        };
        FileState { base: text.clone(), edits: Vec::new(), text, sections, typed: false, problem }
    }

    fn load(root: &Path, rel: &str) -> FileState {
        let path = root.join(rel);
        let mut problem = None;
        let text = match std::fs::metadata(&path) {
            Ok(m) if m.len() > hopper::configindex::MAX_BYTES * 2 => {
                problem = Some("This file is too big to edit here. Open it in your editor instead.".into());
                String::new()
            }
            _ => match std::fs::read(&path) {
                Ok(b) => String::from_utf8(b).unwrap_or_else(|e| {
                    problem = Some("This file isn't plain text. Open it in your editor instead.".into());
                    String::from_utf8_lossy(e.as_bytes()).into_owned()
                }),
                Err(e) => {
                    problem = Some(format!("Couldn't read it: {e}"));
                    String::new()
                }
            },
        };
        let sections = match Format::of(rel) {
            Some(f) if problem.is_none() => configedit::parse_as(f, &text),
            _ => Err(String::new()),
        };
        FileState { base: text.clone(), edits: Vec::new(), text, sections, typed: false, problem }
    }

    fn dirty(&self) -> bool {
        self.problem.is_none() && (self.typed && self.text != self.base || !self.edits.is_empty())
    }

    fn entry(&self, path: &[String], key: &str) -> Option<&Entry> {
        self.sections.as_ref().ok()?.iter().flat_map(|s| &s.entries).find(|e| e.path == path && e.key == key)
    }

    /// The value as read, before edits.
    fn base_value(&self, rel: &str, path: &[String], key: &str) -> Option<Value> {
        let f = Format::of(rel)?;
        configedit::parse_as(f, &self.base).ok()?.into_iter().flat_map(|s| s.entries).find(|e| e.path == path && e.key == key).map(|e| e.value)
    }
}

/// What a row's field is typing.
#[derive(Clone, PartialEq)]
enum Typing {
    Value,
    /// A new item for the list.
    ListItem,
}

/// A line of the settings list: a section's title or one of its settings.
#[derive(Clone, Copy)]
enum Row {
    Head(usize),
    Entry(usize, usize),
}

/// What the settings list was built from: when none of it changed since
/// the last frame (a scroll frame), the rows and their heights stay.
#[derive(Clone, PartialEq)]
struct RowsKey {
    rel: String,
    query: String,
    desc: bool,
    changed_only: bool,
    edits: usize,
    text_len: usize,
    sections: usize,
    editing: bool,
    edit_error: bool,
    errors: usize,
    focus: Option<usize>,
}

struct ChipInfo {
    short: String,
    live: Option<(String, SharedString)>,
}

fn file_name(rel: &str) -> String {
    rel.rsplit('/').next().unwrap_or(rel).to_string()
}

/// "client", "common", "server": a file's chip, short in the narrow panel.
fn short_name(rel: &str, all: &[String]) -> String {
    let name = file_name(rel);
    // When the mod's files differ only by their ending, the ending says it.
    let stem = hopper::configowner::stem(rel);
    let others_same = all.iter().filter(|r| hopper::configowner::stem(r) == stem).count() > 1;
    if others_same {
        let l = name.to_lowercase();
        for s in ["client", "common", "server", "startup"] {
            if l.contains(&format!("-{s}")) || l.contains(&format!("_{s}")) {
                return s.to_string();
            }
        }
    }
    name
}

fn key_of(rel: &str, path: &[String], key: &str) -> String {
    format!("{rel}\u{1}{}\u{1}{key}", path.join("\u{2}"))
}

pub struct ConfigView {
    store: Entity<Store>,
    root: PathBuf,
    host: Host,
    /// The mod's name and icon, for the panel's title.
    title: String,
    icon: Option<PathBuf>,
    files: Vec<String>,
    /// Every settings file of the instance (which world copies win).
    pub all_files: Vec<String>,
    rel: String,
    states: HashMap<String, FileState>,
    text_view: bool,
    editor: Entity<EditorState>,
    search: Entity<InputState>,
    query: String,
    /// Search descriptions too (the tab's checkbox).
    pub desc: bool,
    /// Only settings that differ from their default or are unsaved.
    pub changed_only: bool,
    /// Only files read by this side.
    pub side: Option<Side>,
    /// The setting being typed into (file, table, key), its field, its error.
    editing: Option<(String, Vec<String>, String, Typing)>,
    edit_input: Entity<InputState>,
    edit_error: Option<String>,
    /// Values refused on Save, by setting.
    errors: HashMap<String, String>,
    sliders: HashMap<String, Entity<SliderState>>,
    colors: HashMap<String, Entity<ColorPickerState>>,
    /// The Review list is open; unticked edits there are dropped on Save.
    review: bool,
    review_off: HashSet<String>,
    focus: Option<Focus>,
    /// The settings list: only the rows in view are built, so a file with
    /// thousands of settings scrolls as smoothly as one with ten.
    list: ListState,
    /// The rows the list shows, and what they were made from; rebuilt only
    /// when that changes, never by scrolling.
    rows: Rc<Vec<Row>>,
    rows_key: Option<RowsKey>,
    /// Per file chip: its short name and the "who reads it" label, worked out
    /// once (the label scans every config file of the instance).
    chip_info: HashMap<String, ChipInfo>,
    chip_info_for: usize,
    /// Settings matching the search in each file (the chips show it), and
    /// what they were counted for.
    hit_counts: HashMap<String, usize>,
    hit_counts_for: Option<(String, bool, bool, usize)>,
    scrolled: bool,
    /// A server's Configs tab: where its files are and whether it runs.
    pub server: Option<ServerSide>,
    /// A Hopper Server's files as its panel last sent them, by path.
    remote: HashMap<String, Result<String, String>>,
    /// A save to the panel is on its way.
    saving: bool,
    _mark: Option<RangeDecorationCollection>,
    _subs: Vec<Subscription>,
}

impl ConfigView {
    pub fn new(
        store: Entity<Store>,
        root: PathBuf,
        host: Host,
        title: String,
        icon: Option<PathBuf>,
        files: Vec<String>,
        rel: Option<String>,
        focus: Option<Focus>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| EditorState::new(window, cx).line_number(true).searchable(true).soft_wrap(false));
        let search = cx.new(|cx| InputState::new(window, cx).clean_on_escape().placeholder(if title.is_empty() { "Search settings".to_string() } else { format!("Search {title}'s settings") }));
        let edit_input = cx.new(|cx| InputState::new(window, cx));
        let subs = vec![
            cx.subscribe_in(&search, window, |this, s, e: &InputEvent, window, cx| {
                if let InputEvent::Change = e {
                    this.query = s.read(cx).value().to_string();
                    this.follow_hits(window, cx);
                    cx.notify();
                }
            }),
            cx.subscribe_in(&edit_input, window, |this, _, e: &InputEvent, window, cx| match e {
                InputEvent::PressEnter { .. } => this.commit_edit(window, cx),
                InputEvent::Blur => {
                    if this.edit_error.is_none() {
                        this.commit_edit(window, cx);
                    }
                }
                InputEvent::Change => {
                    this.edit_error = None;
                    cx.notify();
                }
                _ => {}
            }),
        ];
        // Settings files first: those open as settings.
        let first = rel.filter(|r| files.contains(r)).or_else(|| files.iter().find(|f| Format::of(f).is_some()).or(files.first()).cloned()).unwrap_or_default();
        let mut this = Self {
            store,
            root,
            host,
            title,
            icon,
            files,
            all_files: Vec::new(),
            rel: String::new(),
            states: HashMap::new(),
            text_view: false,
            editor,
            search,
            query: String::new(),
            desc: false,
            changed_only: false,
            side: None,
            editing: None,
            edit_input,
            edit_error: None,
            errors: HashMap::new(),
            sliders: HashMap::new(),
            colors: HashMap::new(),
            review: false,
            review_off: HashSet::new(),
            focus,
            list: ListState::new(0, ListAlignment::Top, px(600.)),
            rows: Rc::new(Vec::new()),
            rows_key: None,
            chip_info: HashMap::new(),
            chip_info_for: 0,
            hit_counts: HashMap::new(),
            hit_counts_for: None,
            scrolled: false,
            server: None,
            remote: HashMap::new(),
            saving: false,
            _mark: None,
            _subs: subs,
        };
        this.all_files = indexes(cx).read(cx).peek(&this.root).map(|i| i.files.iter().map(|f| f.rel.clone()).collect()).unwrap_or_default();
        if !first.is_empty() {
            this.load(first, window, cx);
        }
        this
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A server's view: `remote` holds the files' text when they're on a
    /// Hopper Server (read before the view opens; the first file shows
    /// from it).
    #[allow(clippy::too_many_arguments)]
    pub fn for_server(
        store: Entity<Store>,
        root: PathBuf,
        server: ServerSide,
        remote: HashMap<String, Result<String, String>>,
        title: String,
        icon: Option<PathBuf>,
        files: Vec<String>,
        rel: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // `new` opens the first file: the text has to be there first, so
        // the view starts with no files and is given them after.
        let mut v = Self::new(store, root, Host::Tab, title, icon, Vec::new(), None, None, window, cx);
        v.server = Some(server);
        v.remote = remote;
        let first = rel.filter(|r| files.contains(r)).or_else(|| files.iter().find(|f| Format::of(f).is_some()).or(files.first()).cloned()).unwrap_or_default();
        v.files = files;
        if !first.is_empty() {
            v.load(first, window, cx);
        }
        v
    }

    fn on_panel(&self) -> Option<&crate::panel_tabs::Target> {
        match &self.server {
            Some(ServerSide { source: Source::Panel(t), .. }) => Some(t),
            _ => None,
        }
    }

    /// A file as read now: from disk, or from what the panel sent.
    fn read_state(&self, rel: &str) -> FileState {
        if self.on_panel().is_some() {
            let text = self.remote.get(rel).cloned().unwrap_or_else(|| Err("The panel didn't send this file.".into()));
            return FileState::of_text(rel, text);
        }
        FileState::load(&self.root, rel)
    }

    /// Puts `q` in this mod's own search: a mod opened from the tab's
    /// "Settings in other mods" list opens on the settings that mention it.
    pub fn set_search(&mut self, q: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.query == q {
            return;
        }
        self.search.update(cx, |s, cx| s.set_value(q.to_string(), window, cx));
        self.query = q.to_string();
        self.follow_hits(window, cx);
        cx.notify();
    }

    pub fn focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.focus(window, cx));
    }

    /// Counts the search's matches in every file of the mod (reading the
    /// ones not opened yet), once per search.
    fn count_hits(&mut self) {
        let q = self.query.trim().to_lowercase();
        let key = (q.clone(), self.desc, self.changed_only, self.unsaved());
        if self.hit_counts_for.as_ref() == Some(&key) {
            return;
        }
        self.hit_counts.clear();
        if !q.is_empty() {
            for f in self.files.clone() {
                if Format::of(&f).is_some() && !self.states.contains_key(&f) {
                    let st = self.read_state(&f);
                    self.states.insert(f, st);
                }
            }
            let counts: HashMap<String, usize> = self
                .files
                .iter()
                .filter_map(|f| {
                    let secs = self.states.get(f)?.sections.as_ref().ok()?;
                    Some((f.clone(), secs.iter().flat_map(|s| &s.entries).filter(|e| self.matches(f, e, &q)).count()))
                })
                .collect();
            self.hit_counts = counts;
        }
        self.hit_counts_for = Some(key);
    }

    /// When the open file has no match but another of the mod's files has,
    /// opens that one: the search covers the whole mod, not one file.
    fn follow_hits(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.count_hits();
        if self.query.trim().is_empty() || self.text_view || self.hit_counts.get(&self.rel).copied().unwrap_or(0) > 0 {
            return;
        }
        let best = self.visible_files().into_iter().filter_map(|f| Some((self.hit_counts.get(&f).copied()?, f))).filter(|(n, _)| *n > 0).max_by_key(|(n, _)| *n);
        if let Some((_, f)) = best {
            self.focus = None;
            self.load(f, window, cx);
        }
    }

    /// Unsaved edits across the files.
    pub fn unsaved(&self) -> usize {
        self.states.values().map(|s| if s.typed && s.text != s.base { s.edits.len().max(1) } else { s.edits.len() }).sum()
    }

    /// Opens `rel`: one that reads as settings opens on Settings, the rest
    /// as text. Edits on other files stay until saved or dropped.
    fn load(&mut self, rel: String, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_edit(window, cx);
        if !self.states.contains_key(&rel) {
            let st = self.read_state(&rel);
            self.states.insert(rel.clone(), st);
        }
        self.rel = rel;
        self.editing = None;
        self.edit_error = None;
        self.scrolled = false;
        let st = &self.states[&self.rel];
        self.text_view = st.sections.is_err() || st.typed;
        let text = st.text.clone();
        self.editor.update(cx, |e, cx| e.set_value(text, window, cx));
        if self.text_view {
            self.mark_line(window, cx);
        }
        cx.notify();
    }

    fn cur(&self) -> Option<&FileState> {
        self.states.get(&self.rel)
    }

    /// In the text view: the focused line highlighted, in view, the cursor on it.
    fn mark_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(f) = self.focus.clone().filter(|f| f.line > 0) else { return };
        let text = self.editor.read(cx).value().to_string();
        let total = text.lines().count().max(1);
        let row = (f.line - 1).min(total - 1);
        let start: usize = text.split_inclusive('\n').take(row).map(str::len).sum();
        let line = text[start..].split('\n').next().unwrap_or("").trim_end_matches('\r');
        let len = line.chars().count() as u32;
        let note = f.note.unwrap_or_else(|| "The setting this is about.".into());
        let color = cx.theme().primary.opacity(0.18);
        let mark = self.editor.update(cx, |e, cx| {
            let rope = e.text().clone();
            if let Some(d) = e.diagnostics_mut() {
                d.reset(&rope);
                d.push(Diagnostic::new(Position::new(row as u32, 0)..Position::new(row as u32, len), note).with_severity(DiagnosticSeverity::Warning));
            }
            e.set_cursor_position(Position::new(row as u32, 0), window, cx);
            e.create_range_decorations_collection(vec![RangeDecoration::new(start..start + line.len()).with_style(RangeDecorationStyle::Fill).with_color(color)], cx)
        });
        self._mark = Some(mark);
    }

    fn show_text(&mut self, line: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_edit(window, cx);
        if let Some(l) = line {
            self.focus = Some(Focus { line: l, note: None });
        }
        if !self.text_view {
            let text = self.cur().map(|s| s.text.clone()).unwrap_or_default();
            self.editor.update(cx, |e, cx| e.set_value(text, window, cx));
            self.text_view = true;
        }
        self.mark_line(window, cx);
        cx.notify();
    }

    fn show_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(f) = Format::of(&self.rel) else { return };
        if !self.text_view {
            return;
        }
        let t = self.editor.read(cx).value().to_string();
        let rel = self.rel.clone();
        let Some(st) = self.states.get_mut(&rel) else { return };
        match configedit::parse_as(f, &t) {
            Ok(s) => {
                if t != st.text {
                    // Typed edits make the file a whole-text save.
                    st.typed = true;
                    st.edits.clear();
                    st.text = t;
                }
                st.sections = Ok(s);
                self.text_view = false;
                self.scrolled = false;
            }
            Err(e) => window.push_notification(Notification::warning(format!("The text has a mistake, so it can't show as settings: {e}")), cx),
        }
        cx.notify();
    }

    /// Sets one setting (unsaved). Setting it back to how it was drops the edit.
    fn set(&mut self, rel: String, path: Vec<String>, key: String, v: Value, window: &mut Window, cx: &mut Context<Self>) {
        let Some(f) = Format::of(&rel) else { return };
        let Some(st) = self.states.get_mut(&rel) else { return };
        if let Some(e) = st.entry(&path, &key)
            && let Some(err) = e.check(&v)
        {
            window.push_notification(Notification::warning(format!("{}: {err}", e.label())), cx);
            return;
        }
        let k = key_of(&rel, &path, &key);
        self.errors.remove(&k);
        if st.typed {
            // Edited as text: change that text directly.
            match configedit::set_as(f, &st.text, &path, &key, &v).and_then(|t| configedit::parse_as(f, &t).map(|s| (t, s))) {
                Ok((t, s)) => {
                    st.text = t;
                    st.sections = Ok(s);
                }
                Err(e) => window.push_notification(Notification::warning(format!("{key}: {e}")), cx),
            }
            cx.notify();
            return;
        }
        let was = st.base_value(&rel, &path, &key);
        st.edits.retain(|e| !(e.path == path && e.key == key));
        if was.as_ref().is_none_or(|w| !configedit::same(w, &v)) {
            st.edits.push(Edit { path: path.clone(), key: key.clone(), value: v });
        }
        match configedit::apply(f, &st.base, &st.edits) {
            Ok(t) => {
                st.sections = configedit::parse_as(f, &t);
                st.text = t;
            }
            Err(bad) => {
                st.edits.retain(|e| !(e.path == path && e.key == key));
                let msg = bad.first().map(|b| b.error.clone()).unwrap_or_default();
                window.push_notification(Notification::warning(format!("{key}: {msg}")), cx);
            }
        }
        cx.emit(ConfigViewEvent::Changed);
        cx.notify();
    }

    fn start_edit(&mut self, path: Vec<String>, key: String, value: String, typing: Typing, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_edit(window, cx);
        self.editing = Some((self.rel.clone(), path, key, typing));
        self.edit_error = None;
        self.edit_input.update(cx, |s, cx| {
            s.set_value(value, window, cx);
            s.focus(window, cx);
        });
        cx.notify();
    }

    /// Takes what was typed into the open field, if it's a valid value.
    fn commit_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((rel, path, key, typing)) = self.editing.clone() else { return };
        let Some(entry) = self.states.get(&rel).and_then(|s| s.entry(&path, &key)).cloned() else {
            self.editing = None;
            return;
        };
        let typed = self.edit_input.read(cx).value().to_string();
        let v = if typing == Typing::ListItem {
            if typed.trim().is_empty() {
                self.editing = None;
                cx.notify();
                return;
            }
            let mut items = configedit::list_items(&entry.value.text()).unwrap_or_default();
            items.push(typed.trim().to_string());
            Value::Text(configedit::list_text(&items, configedit::list_quoted(&entry.value.text(), &items)))
        } else {
            let like = if matches!(entry.kind(), Kind::List) { Value::Text(String::new()) } else { entry.value.clone() };
            match like.parse_like(&typed) {
                Ok(v) => v,
                Err(e) => {
                    self.edit_error = Some(e);
                    cx.notify();
                    return;
                }
            }
        };
        if let Some(e) = entry.check(&v) {
            self.edit_error = Some(e);
            cx.notify();
            return;
        }
        // A colour typed as #hex keeps the file's own way of writing it.
        let v = match (entry.kind(), &v) {
            (Kind::Color(_), Value::Str(s)) if !matches!(entry.value, Value::Str(_)) => match configedit::parse_color(s) {
                Some((c, _)) => configedit::color_value(c & 0xFF_FFFF, &entry.value),
                None => {
                    self.edit_error = Some("Type a colour like #38BDF8.".into());
                    cx.notify();
                    return;
                }
            },
            _ => v,
        };
        self.editing = None;
        self.set(rel, path, key, v, window, cx);
    }

    pub fn discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = None;
        self.errors.clear();
        self.review = false;
        for st in self.states.values_mut() {
            st.edits.clear();
            st.typed = false;
            st.text = st.base.clone();
        }
        let rels: Vec<String> = self.states.keys().cloned().collect();
        for rel in rels {
            let st = self.read_state(&rel);
            self.states.insert(rel.clone(), st);
        }
        let rel = self.rel.clone();
        if !rel.is_empty() {
            self.load(rel, window, cx);
        }
        cx.emit(ConfigViewEvent::Changed);
        cx.notify();
    }

    /// Saves every file with edits: checked together, the old copies to Backups.
    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_edit(window, cx);
        if self.edit_error.is_some() {
            return;
        }
        if self.text_view {
            // What's typed in the text view counts.
            let t = self.editor.read(cx).value().to_string();
            if let Some(st) = self.states.get_mut(&self.rel)
                && t != st.text
            {
                st.typed = true;
                st.edits.clear();
                st.text = t;
            }
        }
        if self.saving {
            return;
        }
        if let Some(t) = self.on_panel().cloned() {
            self.save_panel(t, window, cx);
            return;
        }
        // The game of this instance; a server's own state says for a server.
        let running = self.server.is_none() && self.store.read(cx).running(&self.root).is_some();
        let (mut saved, mut failed, mut reapplied) = (Vec::new(), Vec::new(), false);
        let mut moved_watch: Vec<(String, Vec<Edit>)> = Vec::new();
        let rels: Vec<String> = self.states.iter().filter(|(_, s)| s.dirty()).map(|(r, _)| r.clone()).collect();
        for rel in rels {
            let st = &self.states[&rel];
            let edits: Vec<Edit> = st.edits.iter().filter(|e| !self.review_off.contains(&key_of(&rel, &e.path, &e.key))).cloned().collect();
            let result = if st.typed {
                if let Some(f) = Format::of(&rel)
                    && let Err(e) = configedit::parse_as(f, &st.text)
                {
                    failed.push(format!("{}: the text has a mistake. {e}", file_name(&rel)));
                    continue;
                }
                let disk = std::fs::read_to_string(self.root.join(&rel)).unwrap_or_default();
                if disk != st.base {
                    failed.push(format!("{} changed on disk since you opened it (the game may have written it). Copy your text, then Discard to see the new file.", file_name(&rel)));
                    continue;
                }
                configedit::save(&self.root, &rel, &st.text).map(|()| None)
            } else if edits.is_empty() {
                Ok(None)
            } else {
                configedit::save_edits(&self.root, &rel, &st.base, &edits).map(Some)
            };
            match result {
                Ok(s) => {
                    if s.as_ref().is_some_and(|s| s.reapplied) {
                        reapplied = true;
                    }
                    if !edits.is_empty() {
                        moved_watch.push((rel.clone(), edits.clone()));
                    }
                    saved.push(rel.clone());
                }
                Err(e) => failed.push(format!("{}: {e:#}", file_name(&rel))),
            }
        }
        for rel in &saved {
            let st = self.read_state(rel);
            self.states.insert(rel.clone(), st);
            self.errors.retain(|k, _| !k.starts_with(&format!("{rel}\u{1}")));
        }
        self.review = false;
        self.review_off.clear();
        if !saved.is_empty() {
            let root = self.root.clone();
            self.store.update(cx, |s, cx| {
                // A crash card's switch this turned off stops offering it.
                s.crash_config_saved(&root);
                cx.notify();
            });
            if running {
                watch_moved_back(self.store.clone(), root.clone(), moved_watch, cx);
            }
            invalidate(&root, cx);
            let names: Vec<String> = saved.iter().map(|r| file_name(r)).collect();
            let what = if names.len() == 1 { names[0].clone() } else { format!("{} files", names.len()) };
            let mut msg = if self.server.is_some() { format!("Saved {what}. Undo it from the server's History.") } else { format!("Saved {what}. Undo it from ⋯ › History.") };
            if reapplied {
                msg.push_str(" The game had changed the file meanwhile: your changes went onto its newer copy.");
            }
            if running {
                msg.push_str(" Most mods pick this up live; some need a restart.");
            }
            msg.push_str(self.server_note());
            window.push_notification(Notification::success(msg), cx);
        }
        for f in failed {
            window.push_notification(Notification::error(format!("Not saved: {f}")), cx);
        }
        let rel = self.rel.clone();
        if !rel.is_empty() {
            self.load(rel, window, cx);
        }
        cx.emit(ConfigViewEvent::Changed);
        cx.notify();
    }

    /// What a server's save adds: a running server reads it on restart.
    fn server_note(&self) -> &'static str {
        match &self.server {
            Some(s) if s.running => " The server is running: this applies when it restarts.",
            _ => "",
        }
    }

    /// Saves every file with edits on a Hopper Server, through its panel:
    /// each file's settings checked there together, the old copy to the
    /// server's History first.
    fn save_panel(&mut self, t: crate::panel_tabs::Target, window: &mut Window, cx: &mut Context<Self>) {
        let mut jobs: Vec<(String, String, Vec<Edit>, Option<String>)> = Vec::new();
        let mut failed: Vec<String> = Vec::new();
        for (rel, st) in self.states.iter().filter(|(_, s)| s.dirty()) {
            if st.typed {
                if let Some(f) = Format::of(rel)
                    && let Err(e) = configedit::parse_as(f, &st.text)
                {
                    failed.push(format!("{}: the text has a mistake. {e}", file_name(rel)));
                    continue;
                }
                // The panel only ever sends a password or token hidden:
                // saving that text would write the dots over it.
                if st.base.contains(MASK) {
                    failed.push(format!("{} holds a password or token, so it can't be saved as text from here. Change its settings in the Settings view instead.", file_name(rel)));
                    continue;
                }
                jobs.push((rel.clone(), st.base.clone(), Vec::new(), Some(st.text.clone())));
            } else {
                let edits: Vec<Edit> = st.edits.iter().filter(|e| !self.review_off.contains(&key_of(rel, &e.path, &e.key))).cloned().collect();
                if !edits.is_empty() {
                    jobs.push((rel.clone(), st.base.clone(), edits, None));
                }
            }
        }
        for f in failed {
            window.push_notification(Notification::error(format!("Not saved: {f}")), cx);
        }
        if jobs.is_empty() {
            return;
        }
        self.saving = true;
        cx.notify();
        let id = t.id().to_string();
        crate::panel_tabs::call_in(
            window,
            cx,
            &t,
            move |c| {
                let c = crate::panel_tabs::http(c)?;
                Ok(jobs
                    .into_iter()
                    .map(|(rel, base, edits, typed)| {
                        let r = match typed {
                            Some(text) => save_text_on_panel(c, &id, &rel, &base, &text).map(|()| (text, false)),
                            None => c.configs_batch(&id, &rel, &base, &edits).map(|s| (s.text, s.reapplied)),
                        };
                        (rel, r.map_err(|e| format!("{e:#}")))
                    })
                    .collect::<Vec<_>>())
            },
            |this, r, window, cx| {
                this.saving = false;
                let results = match r {
                    Ok(r) => r,
                    Err(e) => {
                        window.push_notification(Notification::error(format!("Not saved: {e}")), cx);
                        return;
                    }
                };
                let (mut saved, mut reapplied) = (Vec::new(), false);
                for (rel, r) in results {
                    match r {
                        Ok((text, re)) => {
                            reapplied |= re;
                            this.remote.insert(rel.clone(), Ok(text));
                            let st = this.read_state(&rel);
                            this.states.insert(rel.clone(), st);
                            this.errors.retain(|k, _| !k.starts_with(&format!("{rel}\u{1}")));
                            saved.push(rel);
                        }
                        Err(e) => window.push_notification(Notification::error(format!("Not saved: {}: {e}", file_name(&rel))), cx),
                    }
                }
                this.review = false;
                this.review_off.clear();
                if !saved.is_empty() {
                    invalidate(&this.root, cx);
                    let names: Vec<String> = saved.iter().map(|r| file_name(r)).collect();
                    let what = if names.len() == 1 { names[0].clone() } else { format!("{} files", names.len()) };
                    let mut msg = format!("Saved {what}. Undo it from the server's History.");
                    if reapplied {
                        msg.push_str(" The file had changed on the server meanwhile: your changes went onto its newer copy.");
                    }
                    msg.push_str(this.server_note());
                    window.push_notification(Notification::success(msg), cx);
                }
                let rel = this.rel.clone();
                if !rel.is_empty() {
                    this.load(rel, window, cx);
                }
                cx.emit(ConfigViewEvent::Changed);
            },
        );
    }

    fn visible_files(&self) -> Vec<String> {
        self.files.iter().filter(|f| self.side.is_none_or(|s| hopper::configindex::side_of(f) == s)).cloned().collect()
    }

    fn matches(&self, rel: &str, e: &Entry, q: &str) -> bool {
        if self.changed_only {
            let edited = self.states.get(rel).is_some_and(|s| s.edits.iter().any(|x| x.path == e.path && x.key == e.key));
            if !edited && e.is_default() != Some(false) {
                return false;
            }
        }
        if q.is_empty() {
            return true;
        }
        let hay = format!("{} {} {}{}", e.label(), e.key, e.path.join(" "), if self.desc { format!(" {}", e.comment) } else { String::new() }).to_lowercase();
        q.split_whitespace().all(|w| hay.contains(w))
    }

    fn rows_key(&self) -> RowsKey {
        let s = self.cur();
        RowsKey {
            rel: self.rel.clone(),
            query: self.query.clone(),
            desc: self.desc,
            changed_only: self.changed_only,
            edits: s.map_or(0, |s| s.edits.len()),
            text_len: s.map_or(0, |s| s.text.len()),
            sections: s.and_then(|s| s.sections.as_ref().ok()).map_or(0, |v| v.as_ptr() as usize),
            editing: self.editing.is_some(),
            edit_error: self.edit_error.is_some(),
            errors: self.errors.len(),
            focus: self.focus.as_ref().map(|f| f.line),
        }
    }

    /// Remakes the row list for `key`. The scroll spot stays when it's the
    /// same file and search (an edit changed it), else the list starts at the
    /// top, or at the setting an opened-at line points to.
    fn rebuild_rows(&mut self, key: RowsKey) {
        let same = self.rows_key.as_ref().is_some_and(|k| k.rel == key.rel && k.query == key.query && k.desc == key.desc && k.changed_only == key.changed_only);
        let top = self.list.logical_scroll_top();
        let rows = self.rows();
        self.list.reset(rows.len());
        if same && !rows.is_empty() {
            self.list.scroll_to(ListOffset { item_ix: top.item_ix.min(rows.len() - 1), offset_in_item: top.offset_in_item });
        }
        if !self.scrolled
            && let Some(f) = &self.focus
        {
            let target = rows.iter().position(|r| match r {
                Row::Entry(si, ei) => self.cur().and_then(|s| s.sections.as_ref().ok()).is_some_and(|s| s[*si].entries[*ei].line == f.line),
                _ => false,
            });
            if let Some(ix) = target {
                self.list.scroll_to(ListOffset { item_ix: ix.saturating_sub(1), offset_in_item: px(0.) });
            }
            self.scrolled = true;
        }
        self.rows = Rc::new(rows);
        self.rows_key = Some(key);
    }

    fn rows(&self) -> Vec<Row> {
        let Some(Ok(sections)) = self.cur().map(|s| &s.sections) else { return Vec::new() };
        let q = self.query.trim().to_lowercase();
        let mut out = Vec::new();
        for (si, s) in sections.iter().enumerate() {
            let hits: Vec<usize> = s.entries.iter().enumerate().filter(|(_, e)| self.matches(&self.rel, e, &q)).map(|(i, _)| i).collect();
            if hits.is_empty() {
                continue;
            }
            if !s.path.is_empty() {
                out.push(Row::Head(si));
            }
            out.extend(hits.into_iter().map(|i| Row::Entry(si, i)));
        }
        out
    }

    /// The setting's slider, made the first time it's drawn.
    fn slider(&mut self, k: &str, e: &Entry, lo: f64, hi: f64, window: &mut Window, cx: &mut Context<Self>) -> Entity<SliderState> {
        let whole = matches!(e.value, Value::Int(_));
        let now = match e.value {
            Value::Int(i) => i as f32,
            Value::Float(f) => f as f32,
            _ => lo as f32,
        };
        if let Some(s) = self.sliders.get(k) {
            let s = s.clone();
            // Reset, Discard or a typed value moved it.
            if (s.read(cx).value().start() - now).abs() > f32::EPSILON {
                s.update(cx, |st, cx| st.set_value(now, window, cx));
            }
            return s;
        }
        let step = if whole { 1.0 } else { ((hi - lo) / 100.0) as f32 };
        let s = cx.new(|_| SliderState::new().min(lo as f32).max(hi as f32).step(step).default_value(now));
        let (rel, path, key) = (self.rel.clone(), e.path.clone(), e.key.clone());
        let sub = cx.subscribe_in(&s, window, move |this, _, ev: &SliderEvent, window, cx| {
            if let SliderEvent::Release(v) = ev {
                let x = v.start() as f64;
                let v = if whole { Value::Int(x.round() as i64) } else { Value::Float((x * 1000.0).round() / 1000.0) };
                this.set(rel.clone(), path.clone(), key.clone(), v, window, cx);
            }
        });
        self._subs.push(sub);
        self.sliders.insert(k.to_string(), s.clone());
        s
    }

    fn color(&mut self, k: &str, e: &Entry, window: &mut Window, cx: &mut Context<Self>) -> Entity<ColorPickerState> {
        let argb = configedit::color_of(&e.value).unwrap_or(0xFFFF_FFFF);
        let hsla: Hsla = rgb(argb & 0xFF_FFFF).into();
        if let Some(s) = self.colors.get(k) {
            return s.clone();
        }
        let s = cx.new(|cx| ColorPickerState::new(window, cx).default_value(hsla));
        let (rel, path, key, like) = (self.rel.clone(), e.path.clone(), e.key.clone(), e.value.clone());
        let sub = cx.subscribe_in(&s, window, move |this, _, ev: &ColorPickerEvent, window, cx| {
            let ColorPickerEvent::Change(Some(c)) = ev else { return };
            let r = c.to_rgb();
            let to = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u32;
            let rgb = (to(r.r) << 16) | (to(r.g) << 8) | to(r.b);
            this.set(rel.clone(), path.clone(), key.clone(), configedit::color_value(rgb, &like), window, cx);
        });
        self._subs.push(sub);
        self.colors.insert(k.to_string(), s.clone());
        s
    }

    /// The control on the right of a setting (under it in the panel).
    fn control(&mut self, e: &Entry, id: &str, rem: Pixels, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let rel = self.rel.clone();
        let (path, key) = (e.path.clone(), e.key.clone());
        let k = key_of(&rel, &path, &key);
        let editing = self.editing.as_ref().filter(|(r, p, kk, _)| *r == rel && *p == path && *kk == key).map(|x| x.3.clone());
        let field = |this: &Self| {
            v_flex()
                .w_full()
                .gap_0p5()
                .child(Input::new(&this.edit_input).small())
                .when_some(this.edit_error.clone(), |d, err| d.child(div().text_xs().text_color(theme.danger).whitespace_normal().child(err)))
                .into_any_element()
        };
        let value_button = |id: String, text: String, path: Vec<String>, key: String, cx: &mut Context<Self>| {
            let shown = if text.is_empty() { "(empty)".to_string() } else { text.clone() };
            Button::new(SharedString::from(id))
                .outline()
                .small()
                .w_full()
                .child(div().min_w_0().truncate().font_family("Consolas").child(shown))
                .tooltip("Click to change")
                .on_click(cx.listener(move |this, _, window, cx| this.start_edit(path.clone(), key.clone(), text.clone(), Typing::Value, window, cx)))
                .into_any_element()
        };
        match e.kind() {
            Kind::Bool => {
                let Value::Bool(b) = e.value else { return div().into_any_element() };
                Switch::new(SharedString::from(id.to_string()))
                    .checked(b)
                    .on_click(cx.listener(move |this, v: &bool, window, cx| this.set(rel.clone(), path.clone(), key.clone(), Value::Bool(*v), window, cx)))
                    .into_any_element()
            }
            Kind::Choice(allowed) if editing.is_none() => {
                let cur = e.value.text();
                let view = cx.entity();
                widgets::picker(
                    SharedString::from(id.to_string()),
                    cur.clone(),
                    allowed.iter().map(|a| (if Some(a) == e.default.as_ref() { format!("{a} · default") } else { a.clone() }, *a == cur)).collect(),
                    rem,
                    Rc::new(move |i, window, cx| {
                        let Some(v) = allowed.get(i).cloned() else { return };
                        let (r, p, kk) = (rel.clone(), path.clone(), key.clone());
                        view.update(cx, |this, cx| this.set(r, p, kk, Value::Str(v), window, cx));
                    }),
                )
                .into_any_element()
            }
            Kind::Int | Kind::Float => {
                let input: AnyElement = if editing.is_some() { field(self) } else { value_button(format!("{id}-v"), e.value.text(), path.clone(), key.clone(), cx) };
                if let Some((lo, hi)) = e.slider() {
                    let s = self.slider(&k, e, lo, hi, window, cx);
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_start()
                        .child(div().flex_1().min_w_0().pt_2().child(Slider::new(&s).horizontal()))
                        .child(div().w(rems(5.5)).flex_none().child(input))
                        .into_any_element()
                } else {
                    let whole = matches!(e.value, Value::Int(_));
                    let step = |dir: f64| {
                        let (rel, path, key, v, range) = (rel.clone(), path.clone(), key.clone(), e.value.clone(), e.range);
                        move |this: &mut Self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>| {
                            let mut nv = match v {
                                Value::Int(i) => Value::Int(i + dir as i64),
                                Value::Float(f) => Value::Float(((f + dir * if f.abs() >= 10.0 { 1.0 } else { 0.1 }) * 1000.0).round() / 1000.0),
                                ref o => o.clone(),
                            };
                            // Stops at the ends rather than refusing.
                            if let Some((lo, hi)) = range {
                                let clamp = |x: f64| x.max(lo.unwrap_or(f64::MIN)).min(hi.unwrap_or(f64::MAX));
                                nv = match nv {
                                    Value::Int(i) => Value::Int(clamp(i as f64) as i64),
                                    Value::Float(f) => Value::Float(clamp(f)),
                                    o => o,
                                };
                            }
                            this.set(rel.clone(), path.clone(), key.clone(), nv, window, cx);
                        }
                    };
                    let _ = whole;
                    h_flex()
                        .w_full()
                        .gap_1()
                        .items_start()
                        .child(div().flex_1().min_w_0().child(input))
                        .child(Button::new(SharedString::from(format!("{id}-dn"))).ghost().small().icon(IconName::Minus).tooltip("Less").on_click(cx.listener(step(-1.0))))
                        .child(Button::new(SharedString::from(format!("{id}-up"))).ghost().small().icon(IconName::Plus).tooltip("More").on_click(cx.listener(step(1.0))))
                        .into_any_element()
                }
            }
            Kind::Color(_) => {
                let state = self.color(&k, e, window, cx);
                let input: AnyElement = if editing.is_some() { field(self) } else { value_button(format!("{id}-v"), color_text(&e.value), path.clone(), key.clone(), cx) };
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_start()
                    .child(ColorPicker::new(&state).small())
                    .child(div().flex_1().min_w_0().child(input))
                    .into_any_element()
            }
            Kind::List => {
                let text = e.value.text();
                let items = configedit::list_items(&text).unwrap_or_default();
                let quoted = configedit::list_quoted(&text, &items);
                let mut col = v_flex().w_full().gap_0p5();
                for (i, it) in items.iter().enumerate() {
                    let (rel, path, key, items) = (rel.clone(), path.clone(), key.clone(), items.clone());
                    col = col.child(
                        h_flex()
                            .gap_1()
                            .px_2()
                            .py_0p5()
                            .rounded(theme.radius)
                            .bg(theme.secondary)
                            .child(div().flex_1().min_w_0().truncate().font_family("Consolas").text_xs().child(it.clone()))
                            .child(
                                Button::new(SharedString::from(format!("{id}-rm{i}")))
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::X)
                                    .tooltip("Remove")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        let mut items = items.clone();
                                        items.remove(i);
                                        this.set(rel.clone(), path.clone(), key.clone(), Value::Text(configedit::list_text(&items, quoted)), window, cx)
                                    })),
                            ),
                    );
                }
                if items.is_empty() {
                    col = col.child(widgets::caption("Empty list", cx).text_xs());
                }
                let add: AnyElement = if editing == Some(Typing::ListItem) {
                    field(self)
                } else {
                    let (path, key) = (path.clone(), key.clone());
                    Button::new(SharedString::from(format!("{id}-add")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Plus)
                        .label("Add")
                        .on_click(cx.listener(move |this, _, window, cx| this.start_edit(path.clone(), key.clone(), String::new(), Typing::ListItem, window, cx)))
                        .into_any_element()
                };
                col.child(add).into_any_element()
            }
            Kind::Opaque => {
                let line = e.line;
                Button::new(SharedString::from(format!("{id}-text")))
                    .outline()
                    .small()
                    .icon(IconName::FileText)
                    .label(format!("Edit as text · line {line}"))
                    .on_click(cx.listener(move |this, _, window, cx| this.show_text(Some(line), window, cx)))
                    .into_any_element()
            }
            _ if editing.is_some() => field(self),
            _ => value_button(id.to_string(), e.value.text(), path, key, cx),
        }
    }

    fn render_row(&mut self, row: Row, rem: Pixels, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(Ok(sections)) = self.cur().map(|s| &s.sections) else { return div().into_any_element() };
        match row {
            Row::Head(si) => {
                let s = &sections[si];
                v_flex()
                    .w_full()
                    .pt_3()
                    .pb_1()
                    .px_3()
                    .child(div().text_xs().font_semibold().text_color(theme.muted_foreground).child(s.path.iter().map(|p| configedit::label(p)).collect::<Vec<_>>().join(" › ")))
                    .when(!s.comment.is_empty(), |d| d.child(div().text_xs().text_color(theme.muted_foreground).whitespace_normal().child(s.comment.clone())))
                    .into_any_element()
            }
            Row::Entry(si, ei) => {
                let e = sections[si].entries[ei].clone();
                let rel = self.rel.clone();
                let id = format!("cfg-{si}-{ei}");
                let k = key_of(&rel, &e.path, &e.key);
                let focused = self.focus.as_ref().is_some_and(|f| f.line == e.line);
                let note = self.focus.as_ref().filter(|_| focused).and_then(|f| f.note.clone());
                let edited = self.cur().is_some_and(|s| s.edits.iter().any(|x| x.path == e.path && x.key == e.key));
                let not_default = e.is_default() == Some(false);
                let mut limits = Vec::new();
                if let Some((lo, hi)) = e.range {
                    let n = |x: f64| if x.fract() == 0.0 && x.abs() < 1e15 { format!("{}", x as i64) } else { x.to_string() };
                    match (lo, hi) {
                        (Some(l), Some(h)) => limits.push(format!("{} to {}", n(l), n(h))),
                        (Some(l), None) => limits.push(format!("at least {}", n(l))),
                        (None, Some(h)) => limits.push(format!("at most {}", n(h))),
                        _ => {}
                    }
                }
                if let Some(d) = &e.default {
                    limits.push(format!("default {}", if d.is_empty() { "empty" } else { d }));
                }
                if edited {
                    limits.push("not saved yet".into());
                }
                let reset = e.default_value().filter(|d| !configedit::same(d, &e.value)).map(|d| {
                    let (rel, path, key) = (rel.clone(), e.path.clone(), e.key.clone());
                    let shown = e.default.clone().unwrap_or_default();
                    Button::new(SharedString::from(format!("{id}-reset")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::RotateCcw)
                        .tooltip(format!("Back to the default ({shown})"))
                        .on_click(cx.listener(move |this, _, window, cx| this.set(rel.clone(), path.clone(), key.clone(), d.clone(), window, cx)))
                });
                let restart = match e.restart {
                    Restart::Game => Some("Next launch"),
                    Restart::World => Some("Rejoin to apply"),
                    Restart::None => None,
                };
                let error = self.errors.get(&k).cloned();
                let control = self.control(&e, &id, rem, window, cx);
                let theme = cx.theme().clone();
                let words = widgets::search_words(&self.query);
                let name = h_flex()
                    .gap_1p5()
                    .min_w_0()
                    .when(not_default || edited, |d| d.child(div().size(px(6.)).flex_none().rounded_full().bg(theme.primary)))
                    .child(div().min_w_0().truncate().text_sm().font_semibold().child(widgets::marked(e.label(), &words, cx)))
                    .when_some(restart, |d, r| d.child(widgets::chip(theme.warning).child(r)))
                    .when(e.guessed, |d| d.child(widgets::chip(theme.muted_foreground).child("guessed")));
                let info = v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(name)
                    .child(div().text_xs().font_family("Consolas").text_color(theme.muted_foreground).truncate().child(widgets::marked(if e.path.is_empty() { e.key.clone() } else { format!("{}.{}", e.path.join("."), e.key) }, &words, cx)))
                    .when(!e.comment.is_empty(), |d| d.child(div().text_xs().text_color(theme.muted_foreground).whitespace_normal().child(widgets::marked(e.comment.clone(), if self.desc { &words } else { &[] }, cx))))
                    .when(!limits.is_empty(), |d| d.child(widgets::caption(limits.join(" · "), cx).text_xs()))
                    .when_some(error, |d, err| d.child(div().text_xs().text_color(theme.danger).child(err)))
                    .when_some(note, |d, n| {
                        d.child(
                            h_flex()
                                .gap_1p5()
                                .items_start()
                                .text_xs()
                                .text_color(theme.warning)
                                .child(Icon::new(IconName::TriangleAlert).xsmall().flex_none().mt_0p5())
                                .child(div().flex_1().min_w_0().whitespace_normal().child(n)),
                        )
                    });
                let reset_slot = div().w(rems(1.75)).flex_none().children(reset);
                let row = if self.host == Host::Panel {
                    v_flex()
                        .id(SharedString::from(format!("{id}-row")))
                        .w_full()
                        .gap_1p5()
                        .px_3()
                        .py_2()
                        .child(info)
                        .child(h_flex().gap_1().items_start().child(div().flex_1().min_w_0().child(control)).child(reset_slot))
                } else {
                    let ctl_w = if self.host == Host::Tab { 18. } else { 16. };
                    v_flex().id(SharedString::from(format!("{id}-row"))).w_full().px_3().py_2().child(
                        h_flex()
                            .gap_4()
                            .items_start()
                            .child(info)
                            .child(h_flex().w(rems(ctl_w)).flex_none().gap_1().items_start().justify_end().child(div().flex_1().min_w_0().flex().justify_end().child(control)).child(reset_slot)),
                    )
                };
                let row = row.rounded(theme.radius).when(edited, |d| d.bg(theme.primary.opacity(0.05)));
                let row = if focused { widgets::row_selected(row, cx) } else { row.hover(|st| widgets::row_hover(st, cx)) };
                row.into_any_element()
            }
        }
    }

    /// Every unsaved edit, old → new, with a tick box to keep it.
    fn review_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let mut col = v_flex().gap_1().p_2();
        let mut rels: Vec<&String> = self.states.keys().filter(|r| self.states[*r].dirty()).collect();
        rels.sort();
        for rel in rels {
            let st = &self.states[rel];
            col = col.child(div().pt_2().text_xs().font_semibold().text_color(theme.muted_foreground).child(rel.clone()));
            if st.typed {
                col = col.child(widgets::caption("Edited as text: the whole file is saved.", cx).text_xs());
                continue;
            }
            for e in &st.edits {
                let k = key_of(rel, &e.path, &e.key);
                let old = st.base_value(rel, &e.path, &e.key).map(|v| v.text()).unwrap_or_default();
                let on = !self.review_off.contains(&k);
                let k2 = k.clone();
                col = col.child(
                    h_flex()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded(theme.radius)
                        .hover(|s| widgets::row_hover(s, cx))
                        .child(Checkbox::new(SharedString::from(format!("rv-{k}"))).checked(on).on_click(cx.listener(move |this, v: &bool, _, cx| {
                            if *v {
                                this.review_off.remove(&k2);
                            } else {
                                this.review_off.insert(k2.clone());
                            }
                            cx.notify();
                        })))
                        .child(div().flex_1().min_w_0().truncate().text_sm().child(configedit::label(&e.key)))
                        .child(div().max_w(rems(10.)).truncate().font_family("Consolas").text_xs().text_color(theme.muted_foreground).child(old))
                        .child(Icon::new(IconName::ArrowRight).xsmall().text_color(theme.muted_foreground))
                        .child(div().max_w(rems(10.)).truncate().font_family("Consolas").text_xs().child(e.value.text())),
                );
            }
        }
        col.into_any_element()
    }

    /// The unsaved bar: what's pending, Discard, Review, Save.
    fn save_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let n = self.unsaved();
        let running = self.store.read(cx).running(&self.root).is_some();
        let files: Vec<&String> = self.states.iter().filter(|(_, s)| s.dirty()).map(|(r, _)| r).collect();
        let what = match files.as_slice() {
            [] => String::new(),
            [one] => format!("{n} unsaved change{} in {}", if n == 1 { "" } else { "s" }, file_name(one)),
            many => format!("{n} unsaved changes in {} files", many.len()),
        };
        let narrow = self.host == Host::Panel;
        h_flex()
            .gap_2()
            .px_3()
            .py_2()
            .border_t_1()
            .border_color(theme.border)
            .when(n > 0, |d| d.child(div().size(px(6.)).flex_none().rounded_full().bg(theme.primary)).child(div().text_sm().truncate().child(what)))
            .when(n == 0, |d| d.child(widgets::caption("No unsaved changes", cx).text_xs()))
            .when(!narrow && n > 0, |d| {
                d.child(widgets::caption(if running { "The game is running: most mods pick changes up live. Undo it any time from History." } else { "Undo it any time from History." }, cx).text_xs())
            })
            .child(div().flex_1())
            .child(
                Button::new("cfg-discard")
                    .ghost()
                    .small()
                    .label("Discard")
                    .disabled(n == 0)
                    .on_click(cx.listener(|this, _, window, cx| this.discard(window, cx))),
            )
            .child(
                Button::new("cfg-review")
                    .outline()
                    .small()
                    .label(if self.review { "Back" } else { "Review" })
                    .disabled(n == 0 && !self.review)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.commit_edit(window, cx);
                        this.review = !this.review;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("cfg-save")
                    .primary()
                    .small()
                    .icon(IconName::Check)
                    .label("Save")
                    .disabled(n == 0)
                    .tooltip("Checks every change first. You can undo this from History.")
                    .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
            )
            .into_any_element()
    }

    /// The panel's title: the mod's icon and name, its file count, close.
    fn panel_head(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let changed: usize = self.states.values().filter_map(|s| s.sections.as_ref().ok()).map(|s| hopper::configindex::changed_count(s)).sum();
        let n = self.files.len();
        h_flex()
            .gap_2()
            .px_3()
            .pt_3()
            .pb_2()
            .child(widgets::thumb(self.icon.as_deref(), rems(1.75), cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_base().font_semibold().truncate().child(format!("{} · config", self.title)))
                    .child(widgets::caption(format!("{n} file{}{}", if n == 1 { "" } else { "s" }, if changed > 0 { format!(" · {changed} not at default") } else { String::new() }), cx).text_xs()),
            )
            .child(Button::new("cfg-to-tab").ghost().small().icon(IconName::ExternalLink).tooltip("Open in Configs tab").on_click(cx.listener(|_, _, _, cx| cx.emit(ConfigViewEvent::OpenInTab))))
            .child(Button::new("cfg-close-panel").ghost().small().icon(IconName::X).tooltip("Close").on_click(cx.listener(|_, _, _, cx| cx.emit(ConfigViewEvent::Close))))
            .child(div().w(px(0.)).border_color(theme.border))
            .into_any_element()
    }
}

/// `#38BDF8`-style text for a colour value (an int shows as hex too).
fn color_text(v: &Value) -> String {
    match v {
        Value::Str(s) => s.clone(),
        other => configedit::color_of(other).map_or_else(|| other.text(), |c| format!("#{:06X}", c & 0xFF_FFFF)),
    }
}

impl Render for ConfigView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let readable = self.cur().is_some_and(|s| s.sections.is_ok());
        let has_format = Format::of(&self.rel).is_some();
        let problem = self.cur().and_then(|s| s.problem.clone());
        let on_panel = self.on_panel().is_some();
        let files = self.visible_files();

        // The files: chips with who reads each. The names and labels come from
        // a cache: working them out scans every config file of the instance.
        if self.chip_info_for != self.all_files.len() {
            self.chip_info.clear();
            self.chip_info_for = self.all_files.len();
        }
        for f in &files {
            if self.chip_info.contains_key(f) {
                continue;
            }
            // Which copy of a server config the game reads (NeoForge 21.1).
            let world_copies = hopper::configcompare::overrides(f, &self.all_files);
            let live = hopper::configcompare::live_label(f, !world_copies.is_empty()).map(|l| {
                let tip = if world_copies.is_empty() {
                    "The game reads a world's serverconfig copy when there is one, else this file.".to_string()
                } else {
                    format!("{} has its own copy, which wins in that world.", world_copies.iter().map(|r| if r.starts_with("saves/") { r.split('/').nth(1).unwrap_or("A world") } else { r.split('/').next().unwrap_or("The world") }).collect::<Vec<_>>().join(", "))
                };
                (l.to_string(), SharedString::from(tip))
            });
            self.chip_info.insert(f.clone(), ChipInfo { short: short_name(f, &self.files), live });
        }
        self.count_hits();
        let searching = !self.query.trim().is_empty();
        let chips = h_flex().gap_1p5().flex_wrap().children(files.iter().map(|f| {
            let on = *f == self.rel;
            let hits = self.hit_counts.get(f).copied().filter(|n| searching && *n > 0);
            let side = hopper::configindex::side_of(f);
            let dirty = self.states.get(f).is_some_and(FileState::dirty);
            let rel = f.clone();
            let info = &self.chip_info[f];
            let live = info.live.clone().map(|(l, tip)| (l, tip));
            let side_color = match side {
                Side::Client => theme.muted_foreground,
                Side::Common => theme.info,
                Side::Server => theme.warning,
                _ => theme.muted_foreground,
            };
            h_flex()
                .id(SharedString::from(format!("cfg-file:{f}")))
                .gap_1p5()
                .h(rems(1.6))
                .px_2()
                .rounded(theme.radius)
                .border_1()
                .text_xs()
                .clickable(cx)
                .map(|d| if on { d.bg(theme.primary.opacity(0.12)).border_color(theme.primary.opacity(0.35)).text_color(theme.foreground) } else { d.border_color(theme.border).text_color(theme.muted_foreground).hover(|s| widgets::row_hover(s, cx)) })
                .when(dirty, |d| d.child(div().size(px(6.)).rounded_full().bg(theme.primary)))
                .child(info.short.clone())
                .when(!side.label().is_empty(), |d| d.child(widgets::chip(side_color).child(side.label())))
                .when_some(live.clone(), |d, (l, tip)| d.child(div().id(SharedString::from(format!("cfg-live:{f}"))).child(widgets::chip(if l.starts_with("Not used") { theme.muted_foreground } else { theme.success }).child(l)).tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx))))
                .when_some(hits, |d, n| d.child(widgets::chip(theme.primary).child(format!("{n} match{}", if n == 1 { "" } else { "es" }))))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.focus = None;
                    this.load(rel.clone(), window, cx)
                }))
        }));

        let tabs = widgets::underline_tabs(
            "cfg-view-tabs",
            vec![("Settings".into(), None), ("Text".into(), None)],
            if self.text_view { 1 } else { 0 },
            {
                let view = cx.entity();
                move |ix, window, cx| view.update(cx, |this, cx| if *ix == 0 { this.show_settings(window, cx) } else { this.show_text(None, window, cx) })
            },
            cx,
        );

        let body: AnyElement = if self.files.is_empty() {
            widgets::empty_state(IconName::FileCog, "No config files yet", Some("Most mods write theirs on the first launch.".into()), cx).into_any_element()
        } else if let Some(p) = problem {
            let path = self.root.join(&self.rel);
            v_flex()
                .gap_2()
                .p_4()
                .child(widgets::caption(p, cx).whitespace_normal())
                // A Hopper Server's file isn't on this PC: Files opens it there.
                .when(!on_panel, |d| d.child(Button::new("cfg-open-outside").outline().small().icon(IconName::ExternalLink).label("Open in my editor").on_click(move |_, _, _| crate::store::open_path(&path))))
                .into_any_element()
        } else if self.review {
            div().id("cfg-review").size_full().overflow_y_scroll().child(self.review_list(cx)).into_any_element()
        } else if self.text_view || !readable {
            v_flex()
                .size_full()
                .when(!has_format, |d| d.child(div().px_3().pb_1().child(widgets::caption("Hopper can't show this kind of file as settings; edit it as text.", cx).text_xs())))
                .when(has_format && !readable, |d| {
                    let why = self.cur().and_then(|s| s.sections.as_ref().err().cloned()).unwrap_or_default();
                    d.child(div().px_3().pb_1().child(widgets::caption(format!("This file doesn't read as settings: {why}"), cx).text_xs().whitespace_normal()))
                })
                .child(div().flex_1().min_h_0().child(Editor::new(&self.editor).h_full()))
                .into_any_element()
        } else {
            let key = self.rows_key();
            if self.rows_key.as_ref() != Some(&key) {
                self.rebuild_rows(key);
            }
            if self.rows.is_empty() {
                widgets::empty_state(
                    IconName::Search,
                    if self.changed_only { "Nothing changed here" } else if self.query.is_empty() { "No settings" } else { "No settings match" },
                    None,
                    cx,
                )
                .into_any_element()
            } else {
                div()
                    .id("cfg-rows")
                    .size_full()
                    .px_1()
                    .child(
                        list(
                            self.list.clone(),
                            cx.processor(|this, ix: usize, window, cx| match this.rows.get(ix).copied() {
                                Some(r) => {
                                    let rem = window.rem_size();
                                    this.render_row(r, rem, window, cx)
                                }
                                None => div().into_any_element(),
                            }),
                        )
                        .size_full(),
                    )
                    .vertical_scrollbar(&self.list)
                    .into_any_element()
            }
        };

        let path = self.root.join(&self.rel);
        let narrow = self.host == Host::Panel;
        let search = (!self.text_view && readable).then(|| widgets::search_input(&self.search).small().cleanable(true));
        v_flex()
            .size_full()
            .min_h_0()
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.commit_edit(window, cx)))
            .on_action(cx.listener(|this, _: &Cancel, window, cx| {
                if this.editing.is_some() {
                    this.editing = None;
                    this.edit_error = None;
                    cx.notify();
                } else if this.host == Host::Dialog {
                    if this.unsaved() > 0 {
                        window.push_notification(Notification::info("Not saved yet: Save, or Discard."), cx);
                    } else {
                        window.close_dialog(cx);
                    }
                }
            }))
            .when(narrow, |d| d.child(self.panel_head(cx)))
            .child(
                v_flex()
                    .gap_2()
                    .px_3()
                    .pb_2()
                    .when(self.host == Host::Tab, |d| {
                        d.pt_3().child(
                            h_flex()
                                .gap_2()
                                .child(widgets::thumb(self.icon.as_deref(), rems(1.75), cx))
                                .child(div().text_base().font_semibold().truncate().child(self.title.clone()))
                                .child(div().flex_1())
                                .when(!on_panel, |d| {
                                    d.child(Button::new("cfg-outside").ghost().small().icon(IconName::ExternalLink).label("Open in my editor").on_click({
                                        let p = path.clone();
                                        move |_, _, _| crate::store::open_path(&p)
                                    }))
                                }),
                        )
                    })
                    .when(!files.is_empty(), |d| d.child(chips))
                    .child(h_flex().gap_3().child(tabs).child(div().flex_1()).when(!narrow, |d| d.children(search.map(|s| div().w(rems(if self.host == Host::Tab { 20. } else { 16. })).child(s))))),
            )
            .when(narrow, |d| d.when_some(search_panel(&self.search, !self.text_view && readable), |d, s| d.child(div().px_3().pb_2().child(s))))
            .child(div().flex_1().min_h_0().border_t_1().border_color(theme.border).child(body))
            .child(self.save_bar(cx))
            .when(self.host == Host::Dialog, |d| {
                let p = path.clone();
                d.child(
                    h_flex()
                        .px_3()
                        .pb_1()
                        .child(Button::new("cfg-outside-d").ghost().small().icon(IconName::ExternalLink).label("Open in my editor").on_click(move |_, _, _| crate::store::open_path(&p)))
                        .child(div().flex_1())
                        .child(Button::new("cfg-close").ghost().small().label("Close").on_click(|_, window, cx| window.close_dialog(cx))),
                )
            })
    }
}

fn search_panel(search: &Entity<InputState>, show: bool) -> Option<gpui_kit::component::input::Input> {
    show.then(|| widgets::search_input(search).small().cleanable(true))
}

/// What a Hopper Server's panel sends in place of a password or token.
const MASK: &str = "••••••";

/// Writes a whole file on a Hopper Server, as typed in the text view, when
/// the panel's copy is still the one it was typed on.
fn save_text_on_panel(c: &hopper::hopperd::Client, id: &str, rel: &str, base: &str, text: &str) -> anyhow::Result<()> {
    let now = c.configs_parse(id, &[rel.to_string()])?.remove(rel).unwrap_or_else(|| Err("The panel didn't send it.".into()));
    match now {
        Ok(t) if t == base => {}
        Ok(_) => anyhow::bail!("it changed on the server since you opened it (the server may have written it). Copy your text, then Discard to see the new file."),
        Err(e) => anyhow::bail!("{e}"),
    }
    let v = c.put(&hopper::hopperd::Client::fs_path(id, "/text"), serde_json::json!({ "path": rel, "content": text, "force": true }))?;
    if let Some(e) = v["error"].as_str() {
        anyhow::bail!("{e}");
    }
    Ok(())
}

// ---- the instance's config index -----------------------------------------------------

/// An instance's settings files, which mod each belongs to and how many of
/// their settings aren't at default. Built off the UI thread, shared by the
/// Mods tab's config buttons and the Configs tab.
pub struct InstIndex {
    pub files: Vec<CfgFile>,
    /// Settings not at their default, per file.
    pub changed: HashMap<String, usize>,
    /// Files by owner (the jar's file name).
    pub by_owner: BTreeMap<String, Vec<String>>,
    /// A server's mods' names and icons (an image the store fetches), by
    /// key, for the ones its hopper.json knows. An instance's come from
    /// the instance itself.
    pub names: HashMap<String, (String, String)>,
    /// A server's (names come from `names`, not an instance).
    pub server: bool,
}

impl InstIndex {
    pub fn mod_files(&self, key: &str) -> Vec<String> {
        self.by_owner.get(key).cloned().unwrap_or_default()
    }

    pub fn mod_changed(&self, key: &str) -> usize {
        self.by_owner.get(key).map_or(0, |fs| fs.iter().map(|f| self.changed.get(f).copied().unwrap_or(0)).sum())
    }

    pub fn unmatched(&self) -> Vec<String> {
        self.files.iter().filter(|f| f.owner.is_none()).map(|f| f.rel.clone()).collect()
    }

    fn build(root: &Path, mods: Vec<(String, PathBuf, String)>) -> InstIndex {
        let owners = hopper::configowner::Owners::new(hopper::configowner::of_jars(mods));
        let files = hopper::configindex::index(root, &owners);
        let rels: Vec<String> = files.iter().map(|f| f.rel.clone()).collect();
        let (mut changed, mut settings) = (HashMap::new(), HashMap::new());
        for (rel, s) in hopper::configindex::summaries(root, &rels) {
            changed.insert(rel.clone(), s.changed);
            settings.insert(rel, s.settings);
        }
        // The counts come from a cache; read the files for search behind the list.
        let warm_root = root.to_path_buf();
        std::thread::spawn(move || hopper::configindex::warm(&warm_root, &rels));
        let mut by_owner: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for f in &files {
            if let Some(o) = &f.owner {
                by_owner.entry(o.clone()).or_default().push(f.rel.clone());
            }
        }
        InstIndex { files, changed, by_owner, names: HashMap::new(), server: false }
    }

    /// A server on this PC: its jars are the ones in its `mods/` folder.
    fn of_server_folder(root: &Path) -> InstIndex {
        let (refs, known) = hopper::configowner::folder_mods(root, &root.join("mods"));
        // The name hopper.json gives, else the jar's own; the icon only Hopper knows.
        let names = refs.iter().map(|m| (m.key.clone(), (m.name.clone(), known.get(&m.key).map(|(_, i)| i.clone()).unwrap_or_default()))).collect();
        let owners = hopper::configowner::Owners::new(refs);
        let mut idx = Self::of_owners(root, owners);
        idx.names = names;
        idx.server = true;
        idx
    }

    fn of_owners(root: &Path, owners: hopper::configowner::Owners) -> InstIndex {
        let files = hopper::configindex::index(root, &owners);
        let rels: Vec<String> = files.iter().map(|f| f.rel.clone()).collect();
        let mut changed = HashMap::new();
        for (rel, s) in hopper::configindex::summaries(root, &rels) {
            changed.insert(rel, s.changed);
        }
        let warm_root = root.to_path_buf();
        std::thread::spawn(move || hopper::configindex::warm(&warm_root, &rels));
        let mut by_owner: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for f in &files {
            if let Some(o) = &f.owner {
                by_owner.entry(o.clone()).or_default().push(f.rel.clone());
            }
        }
        InstIndex { files, changed, by_owner, names: HashMap::new(), server: false }
    }

    /// A server on a Hopper Server, from its panel's Configs index (the
    /// same rule picks each file's mod there).
    pub fn of_panel(v: &serde_json::Value) -> InstIndex {
        let st = |v: &serde_json::Value, k: &str| v[k].as_str().unwrap_or("").to_string();
        let files: Vec<CfgFile> = v["files"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|f| {
                let rel = st(f, "rel");
                CfgFile { format: Format::of(&rel), side: hopper::configindex::side_of(&rel), size: f["size"].as_u64().unwrap_or(0), owner: f["owner"].as_str().map(str::to_string), rel }
            })
            .collect();
        let changed = v["files"].as_array().into_iter().flatten().map(|f| (st(f, "rel"), f["changed"].as_u64().unwrap_or(0) as usize)).collect();
        let mut by_owner: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for f in &files {
            if let Some(o) = &f.owner {
                by_owner.entry(o.clone()).or_default().push(f.rel.clone());
            }
        }
        let names = v["mods"].as_array().into_iter().flatten().map(|m| (st(m, "key"), (st(m, "name"), st(m, "icon")))).collect();
        InstIndex { files, changed, by_owner, names, server: true }
    }
}

enum Slot {
    Loading,
    Ready(Arc<InstIndex>),
    Failed(String),
}

/// Every instance's index, loaded when first asked for.
pub struct ConfigIndexes {
    map: HashMap<PathBuf, Slot>,
}

struct Indexes(Entity<ConfigIndexes>);
impl Global for Indexes {}

/// The shared indexes (observe it to redraw when one is ready).
pub fn indexes(cx: &mut App) -> Entity<ConfigIndexes> {
    if let Some(g) = cx.try_global::<Indexes>() {
        return g.0.clone();
    }
    let e = cx.new(|_| ConfigIndexes { map: HashMap::new() });
    cx.set_global(Indexes(e.clone()));
    e
}

impl ConfigIndexes {
    /// The index of the instance at `root`, starting it if it isn't there.
    pub fn get(&mut self, root: &Path, store: &Entity<Store>, cx: &mut Context<Self>) -> Option<Arc<InstIndex>> {
        match self.map.get(root) {
            Some(Slot::Ready(i)) => return Some(i.clone()),
            Some(Slot::Loading) => return None,
            Some(Slot::Failed(_)) | None => {}
        }
        let inst = store.read(cx).instance(root)?.clone();
        let mods: Vec<(String, PathBuf, String)> = inst
            .addons
            .iter()
            .filter(|a| a.class_id == hopper::cf::CLASS_MODS || a.file_name.to_lowercase().ends_with(".jar"))
            .map(|a| (a.file_name.clone(), inst.addon_path(a), a.name.clone()))
            .collect();
        self.start(root, cx, move |r| Ok(InstIndex::build(r, mods)));
        None
    }

    /// A server's index (see [`Source`]), starting it if it isn't there.
    pub fn get_server(&mut self, root: &Path, source: &Source, cx: &mut Context<Self>) -> Option<Arc<InstIndex>> {
        match self.map.get(root) {
            Some(Slot::Ready(i)) => return Some(i.clone()),
            Some(Slot::Loading) => return None,
            Some(Slot::Failed(_)) | None => {}
        }
        match source.clone() {
            Source::Local => self.start(root, cx, |r| Ok(InstIndex::of_server_folder(r))),
            Source::Panel(t) => self.start(root, cx, move |_| {
                let v = crate::panel_tabs::http(&*t.client).and_then(|c| c.configs_index(t.id())).map_err(|e| format!("{e:#}"))?;
                Ok(InstIndex::of_panel(&v))
            }),
        }
        None
    }

    /// Why the index at `root` couldn't be read (a panel out of reach).
    pub fn failed(&self, root: &Path) -> Option<String> {
        match self.map.get(root) {
            Some(Slot::Failed(e)) => Some(e.clone()),
            _ => None,
        }
    }

    /// Builds the index at `root` off the UI thread.
    fn start(&mut self, root: &Path, cx: &mut Context<Self>, build: impl FnOnce(&Path) -> Result<InstIndex, String> + Send + 'static) {
        self.map.insert(root.to_path_buf(), Slot::Loading);
        let r = root.to_path_buf();
        cx.spawn(async move |this, cx| {
            let r2 = r.clone();
            let idx = cx.background_spawn(async move { build(&r2) }).await;
            let _ = this.update(cx, |this, cx| {
                this.map.insert(r, idx.map_or_else(Slot::Failed, |i| Slot::Ready(Arc::new(i))));
                cx.notify();
            });
        })
        .detach();
    }

    /// The index right now, without starting one.
    pub fn peek(&self, root: &Path) -> Option<Arc<InstIndex>> {
        match self.map.get(root) {
            Some(Slot::Ready(i)) => Some(i.clone()),
            _ => None,
        }
    }

    fn drop_root(&mut self, root: &Path, cx: &mut Context<Self>) {
        // A save keeps the old index until the new one is ready.
        if matches!(self.map.get(root), Some(Slot::Ready(_) | Slot::Failed(_))) {
            self.map.remove(root);
            cx.notify();
        }
    }
}

/// Reads the instance's index again (after a save).
pub fn invalidate(root: &Path, cx: &mut App) {
    let e = indexes(cx);
    e.update(cx, |i, cx| i.drop_root(root, cx));
}

/// After saving while the game runs: checks the files again when the game
/// closes, and says when it wrote one of the saved values back.
fn watch_moved_back(store: Entity<Store>, root: PathBuf, files: Vec<(String, Vec<Edit>)>, cx: &mut App) {
    if files.is_empty() {
        return;
    }
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(std::time::Duration::from_secs(5)).await;
            let running = cx.update(|cx| store.read(cx).running(&root).is_some());
            if !running {
                break;
            }
        }
        // Mods write their configs as the game closes.
        cx.background_executor().timer(std::time::Duration::from_secs(3)).await;
        let r = root.clone();
        let moved: Vec<(String, Vec<(Edit, Value)>)> = cx
            .background_spawn(async move { files.iter().map(|(rel, e)| (rel.clone(), configedit::moved_back(&r, rel, e))).filter(|(_, m)| !m.is_empty()).collect() })
            .await;
        if moved.is_empty() {
            return;
        }
        let _ = cx.update(|cx| {
            let lines: Vec<String> =
                moved.iter().flat_map(|(rel, m)| m.iter().map(move |(e, now)| format!("{} in {} ({} instead of {})", configedit::label(&e.key), file_name(rel), now.text(), e.value.text()))).collect();
            let edits: Vec<(String, Vec<Edit>)> = moved.iter().map(|(rel, m)| (rel.clone(), m.iter().map(|(e, _)| e.clone()).collect())).collect();
            let r = root.clone();
            if let Some(w) = cx.active_window() {
                let _ = w.update(cx, |_, window, cx| {
                    let reapply = std::rc::Rc::new(move |window: &mut Window, cx: &mut App| {
                        let mut n = 0;
                        for (rel, e) in &edits {
                            let base = std::fs::read_to_string(r.join(rel)).unwrap_or_default();
                            if configedit::save_edits(&r, rel, &base, e).is_ok() {
                                n += e.len();
                            }
                        }
                        invalidate(&r, cx);
                        window.push_notification(Notification::success(format!("Put back {n} setting{}. Undo it from ⋯ › History.", if n == 1 { "" } else { "s" })), cx);
                    });
                    window.push_notification(
                        Notification::warning(format!("The game changed {} back as it closed: {}.", if lines.len() == 1 { "a setting" } else { "some settings" }, lines.join("; ")))
                            .action(move |_, _, _| {
                                let reapply = reapply.clone();
                                Button::new("cfg-reapply").small().outline().label("Re-apply").on_click(move |_, window, cx| reapply(window, cx))
                            }),
                        cx,
                    );
                });
            }
        });
    })
    .detach();
}
