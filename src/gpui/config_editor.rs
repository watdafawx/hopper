//! The config editor: a mod's config file as settings (TOML: switches,
//! pickers and values, from the file's own comments) or as text. Opens from
//! a mod's menu, and from the crash card at the setting it's about. Saving
//! changes only what was edited; the file as it was goes to Backups.

use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{Cancel, Confirm};
use gpui_kit::component::highlighter::{Diagnostic, DiagnosticSeverity};
use gpui_kit::component::input::{
    Editor, EditorState, Input, InputEvent, InputState, Position, RangeDecoration, RangeDecorationCollection, RangeDecorationStyle,
};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::configedit::{self, Section, Value};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Files bigger than this open in the system's editor instead.
const MAX_BYTES: u64 = 2 << 20;

/// Endings of config files that are text.
const TEXT_ENDINGS: [&str; 10] = [".toml", ".json", ".json5", ".cfg", ".properties", ".txt", ".yml", ".yaml", ".snbt", ".ini"];

/// Where to open: the setting on this line (1-based), with a note on it.
#[derive(Clone, Default)]
pub struct Focus {
    pub line: usize,
    pub note: Option<String>,
}

/// A line of the settings list: a section's title or one of its settings.
#[derive(Clone, Copy)]
enum Row {
    Head(usize),
    Entry(usize, usize),
}

pub struct ConfigEditor {
    store: Entity<Store>,
    root: PathBuf,
    /// The mod's config files (relative to the instance); `rel` is open.
    files: Vec<String>,
    rel: String,
    /// The file as on disk, and as edited so far (settings view).
    saved: String,
    text: String,
    sections: Result<Vec<Section>, String>,
    text_view: bool,
    editor: Entity<EditorState>,
    search: Entity<InputState>,
    query: String,
    /// The setting being typed into (table path, key), its field, its error.
    editing: Option<(Vec<String>, String)>,
    edit_input: Entity<InputState>,
    edit_error: Option<String>,
    focus: Option<Focus>,
    scroll: ScrollHandle,
    /// The focused setting was scrolled to.
    scrolled: bool,
    /// Why the file couldn't be read.
    problem: Option<String>,
    /// The focused line's highlight in the text view.
    _mark: Option<RangeDecorationCollection>,
    _subs: Vec<Subscription>,
}

fn is_toml(rel: &str) -> bool {
    rel.to_ascii_lowercase().ends_with(".toml")
}

fn file_name(rel: &str) -> String {
    rel.rsplit('/').next().unwrap_or(rel).to_string()
}

impl ConfigEditor {
    fn new(store: Entity<Store>, root: PathBuf, files: Vec<String>, rel: String, focus: Option<Focus>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| EditorState::new(window, cx).line_number(true).searchable(true).soft_wrap(false));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search settings"));
        let edit_input = cx.new(|cx| InputState::new(window, cx));
        let subs = vec![
            cx.subscribe(&search, |this, s, e: &InputEvent, cx| {
                if let InputEvent::Change = e {
                    this.query = s.read(cx).value().to_string();
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
        let mut this = Self {
            store,
            root,
            files,
            rel: String::new(),
            saved: String::new(),
            text: String::new(),
            sections: Ok(Vec::new()),
            text_view: false,
            editor,
            search,
            query: String::new(),
            editing: None,
            edit_input,
            edit_error: None,
            focus,
            scroll: ScrollHandle::new(),
            scrolled: false,
            problem: None,
            _mark: None,
            _subs: subs,
        };
        this.load(rel, window, cx);
        this
    }

    /// Opens `rel`: TOML that reads as settings opens on Settings, the rest
    /// as text.
    fn load(&mut self, rel: String, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.root.join(&rel);
        self.rel = rel;
        self.editing = None;
        self.edit_error = None;
        self.scrolled = false;
        self.problem = None;
        let text = match std::fs::metadata(&path) {
            Ok(m) if m.len() > MAX_BYTES => {
                self.problem = Some("This file is too big to edit here. Open it in your editor instead.".into());
                String::new()
            }
            _ => match std::fs::read(&path) {
                Ok(b) => String::from_utf8(b).unwrap_or_else(|e| {
                    self.problem = Some("This file isn't plain text. Open it in your editor instead.".into());
                    String::from_utf8_lossy(e.as_bytes()).into_owned()
                }),
                Err(e) => {
                    self.problem = Some(format!("Couldn't read it: {e}"));
                    String::new()
                }
            },
        };
        self.saved = text.clone();
        self.text = text.clone();
        self.sections = if is_toml(&self.rel) { configedit::parse(&text) } else { Err(String::new()) };
        self.text_view = self.sections.is_err();
        self.editor.update(cx, |e, cx| e.set_value(text, window, cx));
        if self.text_view {
            self.mark_line(window, cx);
        }
        cx.notify();
    }

    /// The text as it would be saved.
    fn current(&self, cx: &App) -> String {
        if self.text_view { self.editor.read(cx).value().to_string() } else { self.text.clone() }
    }

    fn dirty(&self, cx: &App) -> bool {
        self.problem.is_none() && self.current(cx) != self.saved
    }

    /// In the text view: the focused line highlighted, in view with some
    /// room below it, and the cursor on it.
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
            e.create_range_decorations_collection(
                vec![RangeDecoration::new(start..start + line.len()).with_style(RangeDecorationStyle::Fill).with_color(color)],
                cx,
            )
        });
        self._mark = Some(mark);
    }

    fn show_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_view {
            return;
        }
        self.commit_edit(window, cx);
        let text = self.text.clone();
        self.editor.update(cx, |e, cx| e.set_value(text, window, cx));
        self.text_view = true;
        self.mark_line(window, cx);
        cx.notify();
    }

    fn show_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.text_view || !is_toml(&self.rel) {
            return;
        }
        let t = self.editor.read(cx).value().to_string();
        match configedit::parse(&t) {
            Ok(s) => {
                self.text = t;
                self.sections = Ok(s);
                self.text_view = false;
                self.scrolled = false;
            }
            Err(e) => window.push_notification(Notification::warning(format!("The text has a mistake, so it can't show as settings: {e}")), cx),
        }
        cx.notify();
    }

    /// Sets one setting in the edited text.
    fn set(&mut self, path: Vec<String>, key: String, v: Value, window: &mut Window, cx: &mut Context<Self>) {
        match configedit::set(&self.text, &path, &key, &v).and_then(|t| configedit::parse(&t).map(|s| (t, s))) {
            Ok((t, s)) => {
                self.text = t;
                self.sections = Ok(s);
            }
            Err(e) => window.push_notification(Notification::warning(format!("{key}: {e}")), cx),
        }
        cx.notify();
    }

    fn start_edit(&mut self, path: Vec<String>, key: String, value: String, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_edit(window, cx);
        self.editing = Some((path, key));
        self.edit_error = None;
        self.edit_input.update(cx, |s, cx| {
            s.set_value(value, window, cx);
            s.focus(window, cx);
        });
        cx.notify();
    }

    /// Takes what was typed into the open field, if it's a valid value.
    fn commit_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((path, key)) = self.editing.clone() else { return };
        let Ok(sections) = &self.sections else { return };
        let Some(entry) = sections.iter().flat_map(|s| &s.entries).find(|e| e.path == path && e.key == key).cloned() else {
            self.editing = None;
            return;
        };
        let typed = self.edit_input.read(cx).value().to_string();
        let v = match entry.value.parse_like(&typed) {
            Ok(v) => v,
            Err(e) => {
                self.edit_error = Some(e);
                cx.notify();
                return;
            }
        };
        if let Some(e) = entry.check(&v) {
            self.edit_error = Some(e);
            cx.notify();
            return;
        }
        self.editing = None;
        self.set(path, key, v, window, cx);
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_edit(window, cx);
        if self.edit_error.is_some() {
            return;
        }
        let text = self.current(cx);
        if is_toml(&self.rel)
            && let Err(e) = configedit::parse(&text)
        {
            window.push_notification(Notification::warning(format!("Not saved: the text has a mistake. {e}")), cx);
            return;
        }
        match configedit::save(&self.root, &self.rel, &text) {
            Ok(()) => {
                self.saved = text;
                // A crash card's switch this turned off stops offering it.
                let root = self.root.clone();
                self.store.update(cx, |s, cx| {
                    s.crash_config_saved(&root);
                    cx.notify();
                });
                let running = self.store.read(cx).running(&self.root).is_some();
                window.push_notification(
                    Notification::success(format!(
                        "Saved {}. The old copy is in Backups.{}",
                        file_name(&self.rel),
                        if running { " Some settings apply only after a restart." } else { "" }
                    )),
                    cx,
                );
            }
            Err(e) => window.push_notification(Notification::error(format!("Couldn't save {}: {e:#}", file_name(&self.rel))), cx),
        }
        cx.notify();
    }

    fn rows(&self) -> Vec<Row> {
        let Ok(sections) = &self.sections else { return Vec::new() };
        let q = self.query.trim().to_lowercase();
        let mut out = Vec::new();
        for (si, s) in sections.iter().enumerate() {
            let in_path = !q.is_empty() && s.path.join(".").to_lowercase().contains(&q);
            let hits: Vec<usize> = s
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| q.is_empty() || in_path || e.key.to_lowercase().contains(&q) || e.comment.to_lowercase().contains(&q))
                .map(|(i, _)| i)
                .collect();
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

    fn render_row(&self, row: Row, rem: Pixels, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Ok(sections) = &self.sections else { return div().into_any_element() };
        match row {
            Row::Head(si) => {
                let s = &sections[si];
                v_flex()
                    .pt_3()
                    .pb_1()
                    .px_3()
                    .child(div().text_xs().font_semibold().text_color(theme.muted_foreground).child(s.path.join(" › ")))
                    .when(!s.comment.is_empty(), |d| {
                        d.child(div().text_xs().text_color(theme.muted_foreground).whitespace_normal().child(s.comment.clone()))
                    })
                    .into_any_element()
            }
            Row::Entry(si, ei) => {
                let e = &sections[si].entries[ei];
                let id = format!("cfg-{si}-{ei}");
                let focused = self.focus.as_ref().is_some_and(|f| f.line == e.line);
                let note = self.focus.as_ref().filter(|_| focused).and_then(|f| f.note.clone());
                let editing = self.editing.as_ref().is_some_and(|(p, k)| *p == e.path && *k == e.key);
                let (path, key) = (e.path.clone(), e.key.clone());
                let mut limits = Vec::new();
                if let Some((lo, hi)) = e.range {
                    match (lo, hi) {
                        (Some(l), Some(h)) => limits.push(format!("{l} to {h}")),
                        (Some(l), None) => limits.push(format!("at least {l}")),
                        (None, Some(h)) => limits.push(format!("at most {h}")),
                        _ => {}
                    }
                }
                if let Some(d) = &e.default {
                    limits.push(format!("default {d}"));
                }
                let control: AnyElement = match &e.value {
                    Value::Bool(b) => Switch::new(SharedString::from(id.clone()))
                        .checked(*b)
                        .on_click(cx.listener(move |this, v: &bool, window, cx| {
                            this.set(path.clone(), key.clone(), Value::Bool(*v), window, cx)
                        }))
                        .into_any_element(),
                    Value::Str(s) if !e.allowed.is_empty() && !editing => {
                        let allowed = e.allowed.clone();
                        let view = cx.entity();
                        widgets::picker(
                            SharedString::from(id.clone()),
                            s.clone(),
                            allowed.iter().map(|a| (a.clone(), a == s)).collect(),
                            rem,
                            Rc::new(move |i, window, cx| {
                                let Some(v) = allowed.get(i).cloned() else { return };
                                let (p, k) = (path.clone(), key.clone());
                                view.update(cx, |this, cx| this.set(p, k, Value::Str(v), window, cx));
                            }),
                        )
                        .into_any_element()
                    }
                    _ if editing => v_flex()
                        .w_full()
                        .gap_0p5()
                        .child(Input::new(&self.edit_input).small())
                        .when_some(self.edit_error.clone(), |d, err| d.child(div().text_xs().text_color(theme.danger).child(err)))
                        .into_any_element(),
                    v => {
                        let text = v.text();
                        let shown = if text.is_empty() { "(empty)".to_string() } else { text.clone() };
                        Button::new(SharedString::from(id.clone()))
                            .outline()
                            .small()
                            .w_full()
                            .child(div().min_w_0().truncate().font_family("Consolas").child(shown))
                            .tooltip("Click to change")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.start_edit(path.clone(), key.clone(), text.clone(), window, cx)
                            }))
                            .into_any_element()
                    }
                };
                let row = h_flex()
                    .id(SharedString::from(format!("{id}-row")))
                    .gap_4()
                    .px_3()
                    .py_2()
                    .rounded(theme.radius)
                    .items_start()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(div().text_sm().font_semibold().child(e.key.clone()))
                            .when(!e.comment.is_empty(), |d| {
                                d.child(div().text_xs().text_color(theme.muted_foreground).whitespace_normal().child(e.comment.clone()))
                            })
                            .when(!limits.is_empty(), |d| d.child(widgets::caption(limits.join(" · "), cx).text_xs()))
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
                            }),
                    )
                    .child(h_flex().w(rems(16.)).flex_none().justify_end().child(control));
                let row = if focused { widgets::row_selected(row, cx) } else { row.hover(|st| widgets::row_hover(st, cx)) };
                row.into_any_element()
            }
        }
    }
}

impl Render for ConfigEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let rem = window.rem_size();
        let body_h = (widgets::dialog_room(window) - rem * 9.).max(rem * 12.);
        let toml = is_toml(&self.rel);
        let readable = toml && self.sections.is_ok();
        let running = self.store.read(cx).running(&self.root).is_some();
        let dirty = self.dirty(cx);

        // The file: a picker when the mod has several.
        let file_pick: AnyElement = if self.files.len() > 1 {
            let files = self.files.clone();
            let view = cx.entity();
            div()
                .w(rems(22.))
                .child(widgets::picker(
                    "cfg-file",
                    file_name(&self.rel),
                    files.iter().map(|f| (f.trim_start_matches("config/").to_string(), *f == self.rel)).collect(),
                    rem,
                    Rc::new(move |i, window, cx| {
                        let Some(f) = files.get(i).cloned() else { return };
                        view.update(cx, |this, cx| {
                            if this.dirty(cx) {
                                window.push_notification(Notification::warning("Save or undo your changes first."), cx);
                                return;
                            }
                            this.focus = None;
                            this.load(f, window, cx);
                        });
                    }),
                ))
                .into_any_element()
        } else {
            div().text_sm().font_semibold().child(self.rel.clone()).into_any_element()
        };

        let tabs = TabBar::new("cfg-tabs")
            .underline()
            .small()
            .selected_index(if self.text_view { 1 } else { 0 })
            .child(Tab::new().label("Settings").disabled(!toml))
            .child(Tab::new().label("Text"))
            .on_click(cx.listener(|this, ix: &usize, window, cx| {
                if *ix == 0 { this.show_settings(window, cx) } else { this.show_text(window, cx) }
            }));

        let body: AnyElement = if let Some(p) = &self.problem {
            let path = self.root.join(&self.rel);
            v_flex()
                .gap_2()
                .p_4()
                .child(widgets::caption(p.clone(), cx).whitespace_normal())
                .child(
                    Button::new("cfg-open-outside")
                        .outline()
                        .small()
                        .icon(IconName::ExternalLink)
                        .label("Open in my editor")
                        .on_click(move |_, _, _| crate::store::open_path(&path)),
                )
                .into_any_element()
        } else if self.text_view || !readable {
            Editor::new(&self.editor).h(body_h).into_any_element()
        } else {
            let rows = self.rows();
            if !self.scrolled
                && let Some(f) = &self.focus
            {
                let target = rows.iter().position(|r| match r {
                    Row::Entry(si, ei) => self.sections.as_ref().is_ok_and(|s| s[*si].entries[*ei].line == f.line),
                    _ => false,
                });
                match target {
                    // Laid out: scroll so its section title shows too.
                    Some(ix) if self.scroll.bounds_for_item(ix).is_some() => {
                        self.scroll.scroll_to_top_of_item(ix.saturating_sub(1));
                        self.scrolled = true;
                    }
                    // Not laid out yet: next frame.
                    Some(_) => cx.on_next_frame(window, |_, _, cx| cx.notify()),
                    None => self.scrolled = true,
                }
            }
            let children: Vec<AnyElement> = rows.into_iter().map(|r| self.render_row(r, rem, cx)).collect();
            if children.is_empty() {
                div()
                    .h(body_h)
                    .child(widgets::empty_state(
                        IconName::Search,
                        if self.query.is_empty() { "No settings" } else { "No settings match" },
                        None,
                        cx,
                    ))
                    .into_any_element()
            } else {
                div()
                    .id("cfg-rows")
                    .h(body_h)
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(children)
                    .into_any_element()
            }
        };

        let path = self.root.join(&self.rel);
        v_flex()
            .gap_2()
            // Enter in a field takes the value, and Escape drops the edit;
            // neither closes the editor (the dialog's own keys would).
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.commit_edit(window, cx)))
            .on_action(cx.listener(|this, _: &Cancel, window, cx| {
                if this.editing.is_some() {
                    this.editing = None;
                    this.edit_error = None;
                    cx.notify();
                } else if this.dirty(cx) {
                    window.push_notification(Notification::info("Not saved yet: Save, or Discard changes."), cx);
                } else {
                    window.close_dialog(cx);
                }
            }))
            .child(
                h_flex()
                    .gap_3()
                    .child(file_pick)
                    .child(div().flex_1())
                    .when(!self.text_view && readable, |d| d.child(div().w(rems(16.)).child(Input::new(&self.search).small().cleanable(true)))),
            )
            .child(tabs)
            .when(running, |d| {
                d.child(
                    widgets::notice_bar(theme.warning, IconName::TriangleAlert, cx)
                        .text_sm()
                        .child("The game is running. Some settings only apply after a restart, and a few mods rewrite their config when the game closes."),
                )
            })
            .child(body)
            .child(
                h_flex()
                    .gap_2()
                    .pt_1()
                    .child(
                        Button::new("cfg-outside")
                            .ghost()
                            .small()
                            .icon(IconName::ExternalLink)
                            .label("Open in my editor")
                            .on_click(move |_, _, _| crate::store::open_path(&path)),
                    )
                    .child(div().flex_1())
                    .when(dirty, |d| d.child(widgets::caption("Not saved yet", cx).text_xs()))
                    .child(
                        Button::new("cfg-close")
                            .ghost()
                            .small()
                            .label(if dirty { "Discard changes" } else { "Close" })
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("cfg-save")
                            .primary()
                            .small()
                            .icon(IconName::Check)
                            .label("Save")
                            .disabled(!dirty)
                            .tooltip("The file as it was goes to Backups, so you can undo this.")
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    ),
            )
    }
}

/// Opens `rel` (one of `files`) in the editor.
pub fn open(store: Entity<Store>, root: PathBuf, files: Vec<String>, rel: String, focus: Option<Focus>, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| ConfigEditor::new(store, root, files, rel, focus, window, cx));
    window.open_dialog(cx, move |dialog, window, _| {
        dialog
            .title("Config")
            .w(rems(64.).to_pixels(window.rem_size()))
            .margin_top(rems(widgets::DIALOG_TOP).to_pixels(window.rem_size()))
            .child(view.clone())
    });
}

/// The mod's text config files, relative to the instance.
fn mod_files(store: &Entity<Store>, root: &Path, project_id: u64, file_name: &str, cx: &App) -> Option<(String, Vec<String>)> {
    let inst = store.read(cx).instance(root)?.clone();
    let addon = inst.addons.iter().find(|a| (project_id != 0 && a.project_id == project_id) || a.file_name == file_name)?.clone();
    let files: Vec<String> = hopper::modconfig::files(&inst, &addon)
        .into_iter()
        .filter(|f| {
            let l = f.to_ascii_lowercase();
            TEXT_ENDINGS.iter().any(|e| l.ends_with(e))
        })
        .collect();
    Some((addon.name, files))
}

/// Opens a mod's config: its first file, with a picker for the rest.
pub fn open_for_mod(store: Entity<Store>, root: PathBuf, project_id: u64, file_name: String, window: &mut Window, cx: &mut App) {
    let Some((name, files)) = mod_files(&store, &root, project_id, &file_name, cx) else { return };
    // TOML first: those open as settings.
    let first = files.iter().find(|f| is_toml(f)).or(files.first()).cloned();
    match first {
        Some(rel) => open(store, root, files, rel, None, window, cx),
        None => window.push_notification(Notification::info(format!("{name} has no config files in config/ yet. Most mods write them on the first launch.")), cx),
    }
}
