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

/// The editor plus what its hover card reads.
pub struct ArgsEditor {
    pub state: Entity<EditorState>,
    ctx: Rc<RefCell<Context>>,
    marks: RefCell<Option<TextDecorationCollection>>,
    /// (text, context) the marks were drawn for.
    drawn: RefCell<Option<(String, Context)>>,
}

impl ArgsEditor {
    pub fn new(placeholder: &'static str, window: &mut Window, cx: &mut App) -> Self {
        let ctx = Rc::new(RefCell::new(Context { target: Target::default(), memory: Memory::Free }));
        let hover_ctx = ctx.clone();
        let state = cx.new(|cx| {
            let mut s = EditorState::new(window, cx)
                .line_number(false)
                .folding(false)
                .auto_close(false)
                .smart_indent(false)
                .searchable(false)
                .soft_wrap(true)
                .placeholder(placeholder);
            let lsp = s.lsp_mut();
            lsp.completion_provider = Some(Rc::new(Flags));
            lsp.hover_provider = Some(Rc::new(Explain(hover_ctx)));
            lsp.completion_menu.max_width = px(560.);
            lsp.set_completion_highlighter(Rc::new(highlight_contains));
            s
        });
        Self { state, ctx, marks: RefCell::new(None), drawn: RefCell::new(None) }
    }

    /// Tab picks the highlighted suggestion (Enter does already) instead of
    /// indenting. Put on an element around the editor.
    pub fn tab_accepts<E: InteractiveElement>(&self, el: E) -> E {
        let state = self.state.clone();
        el.capture_action(move |_: &IndentInline, window, cx| {
            state.update(cx, |s, cx| {
                if s.completion_menu_state().open {
                    let enter = gpui_kit::component::input::Enter { secondary: false, shift: false };
                    if s.route_overlay_action(Box::new(enter), window, cx) {
                        cx.stop_propagation();
                    }
                }
            });
        })
    }

    /// Recolours the flags when the text or what they're checked against
    /// changed. Cheap to call on every render.
    pub fn refresh(&self, text: &str, ctx: Context, cx: &mut App) {
        if self.drawn.borrow().as_ref().is_some_and(|(t, c)| t == text && *c == ctx) {
            return;
        }
        *self.ctx.borrow_mut() = ctx;
        // The editor may hold newer text than the caller's copy for a moment.
        if self.state.read(cx).value().as_ref() != text {
            return;
        }
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

    /// Height for the text: at least `min` rows, at most `max`, then it scrolls.
    pub fn height(&self, min: usize, max: usize, cx: &App) -> Pixels {
        let s = self.state.read(cx);
        let rows = s.display_map().wrap_row_count().clamp(min, max);
        s.line_height().unwrap_or(px(20.)) * rows as f32 + px(14.)
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
