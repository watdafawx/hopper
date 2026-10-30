//! The Java arguments box as a small code editor: each flag coloured
//! (prefix, name, value; unknown, clashing and malformed flags underlined),
//! a hover card saying what the flag does and what's wrong with it, and
//! suggestions while typing `-XX:`, `-X` or `-D`. The marks and suggestions
//! come from hopper-core's `jvm_editor`, which Hopper Server's panel uses too.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::{
    CompletionProvider, EditorState, HoverProvider, IndentInline, Rope, RopeExt as _, TextDecoration,
    TextDecorationCollection, highlight_contains,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::jvm_args::Target;
use hopper::jvm_editor::{self, Level, Memory};
use lsp_types::{
    CompletionContext, CompletionItem, CompletionResponse, CompletionTextEdit, Documentation, Hover,
    HoverContents, MarkupContent, MarkupKind, TextEdit,
};
use std::cell::RefCell;
use std::rc::Rc;

/// What the marks depend on besides the text.
#[derive(Clone, Copy, PartialEq)]
pub struct Context {
    pub target: Target,
    pub memory: Memory,
}

/// The box's height in rows: never less than this.
const MIN_ROWS: f32 = 3.;
/// Fitted to the flags, it grows up to this many rows, then scrolls.
const AUTO_ROWS: f32 = 12.;
/// Dragged taller, it stops here.
const MAX_ROWS: f32 = 40.;
/// Padding above and below the rows, plus room for the sideways scrollbar.
const PAD: f32 = 14.;

/// Dragging the box's bottom edge.
pub struct Resize;

struct ResizeGhost;

impl Render for ResizeGhost {
    fn render(&mut self, _: &mut Window, _: &mut gpui_kit::Context<Self>) -> impl IntoElement {
        div()
    }
}

/// The editor plus what its hover card reads. It shows one flag per line;
/// [`jvm_editor::compact`] turns that back into the one-line saved value.
pub struct ArgsEditor {
    pub state: Entity<EditorState>,
    ctx: Rc<RefCell<Context>>,
    marks: RefCell<Option<TextDecorationCollection>>,
    /// (text, context) the marks were drawn for.
    drawn: RefCell<Option<(String, Context)>>,
    /// Rows while the bottom edge is dragged.
    drag: Option<f32>,
}

impl ArgsEditor {
    pub fn new(placeholder: &'static str, window: &mut Window, cx: &mut App) -> Self {
        let ctx = Rc::new(RefCell::new(Context { target: Target::default(), memory: Memory::Free }));
        let hover_ctx = ctx.clone();
        let state = cx.new(|cx| {
            // One flag per line with its number; a long one scrolls sideways
            // rather than wrapping in the middle of the flag.
            let mut s = EditorState::new(window, cx)
                .line_number(true)
                .folding(false)
                .auto_close(false)
                .smart_indent(false)
                .searchable(false)
                .soft_wrap(false)
                .placeholder(placeholder);
            let lsp = s.lsp_mut();
            lsp.completion_provider = Some(Rc::new(Flags));
            lsp.hover_provider = Some(Rc::new(Explain(hover_ctx)));
            lsp.completion_menu.max_width = px(560.);
            lsp.set_completion_highlighter(Rc::new(highlight_contains));
            s
        });
        Self { state, ctx, marks: RefCell::new(None), drawn: RefCell::new(None), drag: None }
    }

    /// Shows `args` (a saved value) one flag per line.
    pub fn show(&self, args: &str, window: &mut Window, cx: &mut App) {
        let lines = jvm_editor::one_per_line(args);
        self.state.update(cx, |s, cx| {
            if s.value().as_ref() != lines {
                s.set_value(lines, window, cx);
            }
        });
    }

    /// The text as it's saved: the flags on one line.
    pub fn saved(&self, cx: &App) -> String {
        jvm_editor::compact(self.state.read(cx).value().as_ref())
    }

    /// Puts flags typed side by side (or pasted) back on lines of their own.
    /// Called when the box loses focus, so the caret never jumps while typing.
    pub fn tidy(&self, window: &mut Window, cx: &mut App) {
        let now = self.state.read(cx).value().to_string();
        self.show(&now, window, cx);
    }

    /// Tab and Enter pick the highlighted suggestion, and only that: no
    /// indent, and no new line after the flag. Put on an element around the
    /// editor.
    pub fn tab_accepts<E: InteractiveElement>(&self, el: E) -> E {
        fn accept(state: &Entity<EditorState>, window: &mut Window, cx: &mut App) {
            state.update(cx, |s, cx| {
                if s.completion_menu_state().open {
                    let enter = gpui_kit::component::input::Enter { secondary: false, shift: false };
                    if s.route_overlay_action(Box::new(enter), window, cx) {
                        cx.stop_propagation();
                    }
                }
            });
        }
        let (tab, enter) = (self.state.clone(), self.state.clone());
        el.capture_action(move |_: &IndentInline, window, cx| accept(&tab, window, cx))
            .capture_action(move |_: &gpui_kit::component::input::Enter, window, cx| accept(&enter, window, cx))
    }

    /// Recolours the flags when the text or what they're checked against
    /// changed. Cheap to call on every render.
    pub fn refresh(&self, ctx: Context, cx: &mut App) {
        let text = self.state.read(cx).value().to_string();
        let text = text.as_str();
        if self.drawn.borrow().as_ref().is_some_and(|(t, c)| t == text && *c == ctx) {
            return;
        }
        *self.ctx.borrow_mut() = ctx;
        *self.drawn.borrow_mut() = Some((text.to_string(), ctx));
        let theme = cx.theme();
        let (muted, name, value, warn, error) =
            (theme.muted_foreground, theme.blue, theme.yellow, theme.warning, theme.red);
        let mut decos = Vec::new();
        let color = |c: Hsla| HighlightStyle { color: Some(c), ..Default::default() };
        for s in jvm_editor::analyze(text, ctx.target, ctx.memory) {
            let line = match s.level {
                Level::Ok => None,
                Level::Unknown => Some((muted, false)),
                Level::Warn => Some((warn, true)),
                Level::Error => Some((error, true)),
            };
            if s.name.start > s.range.start {
                decos.push(TextDecoration::new(s.range.start..s.name.start, color(muted)));
            }
            if s.level != Level::Unknown && !s.name.is_empty() {
                decos.push(TextDecoration::new(s.name.clone(), color(name)));
            }
            if let Some(v) = &s.value {
                if v.start > s.name.end {
                    decos.push(TextDecoration::new(s.name.end..v.start, color(muted)));
                }
                if s.level != Level::Unknown && !v.is_empty() {
                    decos.push(TextDecoration::new(v.clone(), color(value)));
                }
            }
            if let Some((c, wavy)) = line {
                decos.push(TextDecoration::new(
                    s.range.clone(),
                    HighlightStyle {
                        underline: Some(UnderlineStyle { thickness: px(1.), color: Some(c), wavy }),
                        ..Default::default()
                    },
                ));
            }
        }
        let mut marks = self.marks.borrow_mut();
        match marks.as_ref() {
            Some(m) => m.set(decos, cx),
            None => *marks = Some(self.state.update(cx, |s, cx| s.create_decorations_collection(decos, cx))),
        }
    }

    fn line_height(&self, cx: &App) -> Pixels {
        self.state.read(cx).line_height().unwrap_or(px(20.))
    }

    /// Height for the box: the dragged or saved rows (`saved_rows`, 0 for
    /// none), else fitted to the flags up to [`AUTO_ROWS`], then it scrolls.
    pub fn height(&self, saved_rows: f32, cx: &App) -> Pixels {
        let rows = match self.drag.or((saved_rows > 0.).then_some(saved_rows)) {
            Some(r) => r.clamp(MIN_ROWS, MAX_ROWS),
            None => (self.state.read(cx).display_map().wrap_row_count() as f32).clamp(MIN_ROWS, AUTO_ROWS),
        };
        self.line_height(cx) * rows + px(PAD)
    }

    /// The editor `h` tall with a handle on its bottom edge: drag it to set
    /// the height (saved in the settings), double-click it to fit the flags
    /// again. `lens` finds this editor in the view that draws it.
    pub fn with_handle<V: 'static>(
        &self,
        editor: impl IntoElement,
        h: Pixels,
        lens: fn(&mut V) -> &mut ArgsEditor,
        store: &Entity<crate::store::Store>,
        cx: &mut gpui_kit::Context<V>,
    ) -> Stateful<Div> {
        let (bar, active) = (cx.theme().primary, self.drag.is_some());
        let grip = cx.theme().muted_foreground.opacity(0.45);
        let drag_move = cx.listener(move |this: &mut V, e: &DragMoveEvent<Resize>, _, cx| {
            let ed = lens(this);
            let rows = (e.event.position.y - e.bounds.top() - px(PAD)) / ed.line_height(cx);
            ed.drag = Some(rows.round().clamp(MIN_ROWS, MAX_ROWS));
            cx.notify();
        });
        let save = |store: &Entity<crate::store::Store>, rows: f32, cx: &mut App| {
            store.update(cx, |s, cx| s.update_settings(|st| st.java_args_rows = rows, cx));
        };
        let end = {
            let store = store.clone();
            move |this: &mut V, _: &MouseUpEvent, _: &mut Window, cx: &mut gpui_kit::Context<V>| {
                if let Some(rows) = lens(this).drag.take() {
                    save(&store, rows, cx);
                    cx.notify();
                }
            }
        };
        let reset = {
            let store = store.clone();
            cx.listener(move |this: &mut V, e: &ClickEvent, _, cx| {
                if e.click_count() == 2 {
                    lens(this).drag = None;
                    save(&store, 0., cx);
                    cx.notify();
                }
            })
        };
        div()
            .id("java-args")
            .relative()
            .w_full()
            .h(h)
            .on_drag_move(drag_move)
            .capture_any_mouse_up(cx.listener(end.clone()))
            .on_mouse_up_out(MouseButton::Left, cx.listener(end))
            .child(div().size_full().child(editor))
            .child(
                div()
                    .id("java-args-resize")
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom(px(-4.))
                    .h(px(9.))
                    .flex()
                    .justify_center()
                    .items_center()
                    .cursor_row_resize()
                    .rounded_sm()
                    .when(active, |d| d.bg(bar.opacity(0.5)))
                    .hover(move |s| s.bg(bar.opacity(0.35)))
                    .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("Drag to resize, double-click to fit").build(w, cx))
                    .child(div().w(px(36.)).h(px(3.)).rounded_full().bg(grip))
                    .on_click(reset)
                    .on_drag(Resize, |_, _, _, cx| cx.new(|_| ResizeGhost)),
            )
    }
}

/// Suggestions while typing a flag.
struct Flags;

impl CompletionProvider for Flags {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _: CompletionContext,
        _: &mut Window,
        _: &mut App,
    ) -> Task<anyhow::Result<CompletionResponse>> {
        let s = text.to_string();
        let (start, word) = jvm_editor::word_at(&s, offset);
        let range = lsp_types::Range::new(text.offset_to_position(start), text.offset_to_position(offset));
        let items = jvm_editor::complete(word)
            .into_iter()
            .map(|sug| CompletionItem {
                label: sug.insert.clone(),
                detail: Some(short(sug.detail)),
                documentation: Some(Documentation::String(sug.detail.to_string())),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit { range, new_text: sug.insert })),
                ..Default::default()
            })
            .collect();
        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        !new_text.is_empty() && !new_text.chars().any(char::is_whitespace)
    }
}

/// The sentence cut to fit beside the flag in the list.
fn short(text: &str) -> String {
    let first = text.split([':', ';']).next().unwrap_or(text);
    if first.chars().count() > 48 {
        format!("{}…", first.chars().take(47).collect::<String>().trim_end())
    } else {
        first.to_string()
    }
}

/// The hover card: what the flag does and what's wrong with it.
struct Explain(Rc<RefCell<Context>>);

impl HoverProvider for Explain {
    fn hover(&self, text: &Rope, offset: usize, _: &mut Window, _: &mut App) -> Task<anyhow::Result<Option<Hover>>> {
        let s = text.to_string();
        let ctx = *self.0.borrow();
        let spans = jvm_editor::analyze(&s, ctx.target, ctx.memory);
        let Some(span) = spans.into_iter().find(|sp| sp.range.start <= offset && offset < sp.range.end) else {
            return Task::ready(Ok(None));
        };
        let mut md = format!("**{}**", &s[span.range.clone()]);
        if let Some(e) = &span.explain {
            md.push_str(&format!("\n\n{e}"));
        }
        for p in &span.problems {
            match span.level {
                Level::Error => md.push_str(&format!("\n\n**Won't work:** {p}")),
                Level::Warn => md.push_str(&format!("\n\n**Note:** {p}")),
                _ => md.push_str(&format!("\n\n*{p}*")),
            }
        }
        let range = lsp_types::Range::new(text.offset_to_position(span.range.start), text.offset_to_position(span.range.end));
        Task::ready(Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent { kind: MarkupKind::Markdown, value: md }),
            range: Some(range),
        })))
    }
}
