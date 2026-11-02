//! The server list message in a server's Settings: colour and style
//! buttons, variables, two line boxes and a preview drawn like Minecraft's
//! multiplayer list. The boxes show the text formatted (codes hidden,
//! variables tinted, a hover card with their value now) and the style
//! buttons toggle over the selection; "Codes" shows the raw § codes. Same as
//! the panel's web editor; the rules live in `hopper::hosting::motd_edit`.

use crate::widgets;
use crate::widgets::Clickable as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Editor, EditorState, HoverProvider, InputEvent, Rope, RopeExt as _, TextDecoration, TextDecorationCollection};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::hosting::motd;
use hopper::hosting::motd_edit::{self, Line, Style};
use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The message changed (the whole template, lines joined by "\n").
pub struct MotdChanged(pub String);

/// "Codes" stays as last set while Hopper runs.
static RAW: AtomicBool = AtomicBool::new(false);

/// What a line's hover card reads: the line and the variables' values.
type Shared = Rc<RefCell<(Line, BTreeMap<String, String>)>>;

pub struct MotdEditor {
    lines: [Entity<EditorState>; 2],
    /// The formatted lines (kept in step with the boxes while not `raw`).
    models: [Shared; 2],
    marks: [RefCell<Option<TextDecorationCollection>>; 2],
    /// Raw § codes in the boxes.
    raw: bool,
    /// The line the buttons work on: the one last focused.
    last: usize,
    /// A style picked with nothing selected: (line, caret, style) for what's
    /// typed next there.
    pending: Option<(usize, usize, Style)>,
    vars: BTreeMap<String, String>,
    name: String,
    players: String,
    _subs: Vec<Subscription>,
}

impl EventEmitter<MotdChanged> for MotdEditor {}

impl MotdEditor {
    /// `players`: "0/20" for the preview's corner.
    pub fn new(value: &str, vars: BTreeMap<String, String>, name: String, players: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut parts = value.splitn(2, '\n');
        let (a, b) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").replace('\n', " "));
        let raw = RAW.load(Ordering::Relaxed);
        let models: [Shared; 2] = [a.as_str(), b.as_str()].map(|t| Rc::new(RefCell::new((Line::parse(&motd::amp_to_section(t)), vars.clone()))));
        let make = |i: usize, v: String, window: &mut Window, cx: &mut Context<Self>| {
            let hover = models[i].clone();
            cx.new(|cx| {
                // One line, no gutter: Enter is caught around it.
                let mut st = EditorState::new(window, cx)
                    .line_number(false)
                    .folding(false)
                    .auto_close(false)
                    .smart_indent(false)
                    .searchable(false)
                    .soft_wrap(false);
                st.lsp_mut().hover_provider = Some(Rc::new(VarCard(hover)));
                st.set_value(v, window, cx);
                st
            })
        };
        let shown = |i: usize, t: &str| if raw { t.to_string() } else { models[i].borrow().0.text() };
        let lines = [make(0, shown(0, &a), window, cx), make(1, shown(1, &b), window, cx)];
        let mut subs = Vec::new();
        for (i, l) in lines.iter().enumerate() {
            subs.push(cx.subscribe_in(l, window, move |this: &mut Self, _, e: &InputEvent, window, cx| match e {
                InputEvent::Change => this.changed(i, window, cx),
                InputEvent::Focus => {
                    this.last = i;
                    cx.notify();
                }
                _ => {}
            }));
            // The buttons show the style at the caret, so redraw as it moves.
            subs.push(cx.observe(l, |_, _, cx| cx.notify()));
        }
        let this = Self {
            lines,
            models,
            marks: [RefCell::new(None), RefCell::new(None)],
            raw,
            last: 0,
            pending: None,
            vars,
            name,
            players,
            _subs: subs,
        };
        this.paint(0, cx);
        this.paint(1, cx);
        this
    }

    fn line_value(&self, i: usize, cx: &App) -> String {
        if self.raw { self.lines[i].read(cx).value().to_string() } else { self.models[i].borrow().0.to_codes() }
    }

    pub fn value(&self, cx: &App) -> String {
        let (a, b) = (self.line_value(0, cx), self.line_value(1, cx));
        if b.is_empty() { a } else { format!("{a}\n{b}") }
    }

    /// The box changed: typed, deleted or pasted. Formatted, the line takes
    /// the change (and the box is put right when codes were pasted or a
    /// variable was cut into).
    fn changed(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.raw {
            let text = self.lines[i].read(cx).value().to_string();
            let typing = self.pending.filter(|p| p.0 == i).map(|p| p.2);
            let (caret, now) = {
                let mut m = self.models[i].borrow_mut();
                if m.0.text() == text {
                    return;
                }
                let caret = m.0.edit(&text, typing);
                (caret, m.0.text())
            };
            self.pending = None;
            if now != text {
                self.lines[i].update(cx, |st, cx| {
                    st.set_value(now, window, cx);
                    st.set_selected_range(caret..caret, cx);
                });
            }
            self.paint(i, cx);
        }
        cx.emit(MotdChanged(self.value(cx)));
        cx.notify();
    }

    /// Colours the formatted box from its line.
    fn paint(&self, i: usize, cx: &mut App) {
        let mut decos = Vec::new();
        if !self.raw {
            let chip = cx.theme().primary.opacity(0.22);
            let scramble = white().opacity(0.10);
            for (r, s) in self.models[i].borrow().0.ranges() {
                let st = s.style;
                decos.push(TextDecoration::new(
                    r,
                    HighlightStyle {
                        color: Some(rgb(st.rgb()).into()),
                        font_weight: st.bold.then_some(FontWeight::BOLD),
                        font_style: st.italic.then_some(FontStyle::Italic),
                        underline: st.underline.then(|| UnderlineStyle { thickness: px(1.), ..Default::default() }),
                        strikethrough: st.strike.then(|| StrikethroughStyle { thickness: px(1.), ..Default::default() }),
                        // Variables tinted like a chip; scrambled text (it
                        // can't be read in game) faintly shaded.
                        background_color: if s.var { Some(chip) } else if st.obfuscated { Some(scramble) } else { None },
                        ..Default::default()
                    },
                ));
            }
        }
        let mut marks = self.marks[i].borrow_mut();
        match marks.as_ref() {
            Some(m) => m.set(decos, cx),
            None => *marks = Some(self.lines[i].update(cx, |s, cx| s.create_decorations_collection(decos, cx))),
        }
    }

    /// Switches between the formatted boxes and raw codes.
    fn set_raw(&mut self, raw: bool, window: &mut Window, cx: &mut Context<Self>) {
        if raw == self.raw {
            return;
        }
        RAW.store(raw, Ordering::Relaxed);
        let texts: Vec<String> = (0..2).map(|i| self.line_value(i, cx)).collect();
        self.raw = raw;
        self.pending = None;
        for (i, t) in texts.iter().enumerate() {
            let shown = if raw {
                t.clone()
            } else {
                let l = Line::parse(&motd::amp_to_section(t));
                let s = l.text();
                self.models[i].borrow_mut().0 = l;
                s
            };
            self.lines[i].update(cx, |st, cx| st.set_value(shown, window, cx));
            self.paint(i, cx);
        }
        cx.notify();
    }

    /// The style the buttons show as on: the selection's shared style, or
    /// the one picked for typing.
    fn current(&self, cx: &App) -> Style {
        let r = self.lines[self.last].read(cx).selected_range();
        if let Some((l, at, st)) = self.pending
            && l == self.last
            && r == (at..at)
        {
            return st;
        }
        if self.raw {
            let t = motd::amp_to_section(&self.lines[self.last].read(cx).value());
            let raw = self.lines[self.last].read(cx).value().to_string();
            let (a, b) = (motd_edit::amp_pos(&raw, r.start), motd_edit::amp_pos(&raw, r.end));
            return Line::parse(&t).common_style(motd_edit::raw_to_text(&t, a), motd_edit::raw_to_text(&t, b));
        }
        self.models[self.last].borrow().0.common_style(r.start, r.end)
    }

    /// A colour or style button: toggles it over the selection, or picks it
    /// for typing at the caret.
    fn code(&mut self, code: char, window: &mut Window, cx: &mut Context<Self>) {
        let i = self.last;
        let line = self.lines[i].clone();
        let r = line.read(cx).selected_range();
        if self.raw {
            line.update(cx, |st, cx| {
                // Shortcuts typed so far count as codes from here on.
                let raw = st.value().to_string();
                let text = motd::amp_to_section(&raw);
                let (a, b) = (motd_edit::amp_pos(&raw, r.start), motd_edit::amp_pos(&raw, r.end));
                let (out, x, y) = if a == b {
                    motd::apply_code(&text, a, b, code)
                } else {
                    let (ta, tb) = (motd_edit::raw_to_text(&text, a), motd_edit::raw_to_text(&text, b));
                    let mut l = Line::parse(&text);
                    if code == 'r' { l.clear(ta, tb) } else if let Some(at) = motd_edit::Attr::from_code(code) { l.toggle(ta, tb, at) }
                    let out = l.to_codes();
                    let (x, y) = (motd_edit::text_to_raw(&out, ta, true), motd_edit::text_to_raw(&out, tb, false));
                    (out, x, y)
                };
                st.set_value(out, window, cx);
                st.set_selected_range(x..y, cx);
                st.focus(window, cx);
            });
            return;
        }
        if r.is_empty() {
            let st = self.current(cx).toggled(code);
            self.pending = Some((i, r.start, st));
        } else {
            {
                let mut m = self.models[i].borrow_mut();
                if code == 'r' {
                    m.0.clear(r.start, r.end)
                } else if let Some(at) = motd_edit::Attr::from_code(code) {
                    m.0.toggle(r.start, r.end, at)
                }
            }
            self.paint(i, cx);
            cx.emit(MotdChanged(self.value(cx)));
        }
        line.update(cx, |st, cx| {
            st.set_selected_range(r, cx);
            st.focus(window, cx);
        });
        cx.notify();
    }

    fn insert_var(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let line = self.lines[self.last].clone();
        let s = format!("{{{name}}}");
        line.update(cx, |st, cx| {
            let text = st.value().to_string();
            let r = st.selected_range();
            let (a, b) = (r.start.min(text.len()), r.end.min(text.len()));
            st.set_value(format!("{}{s}{}", &text[..a], &text[b..]), window, cx);
            st.set_selected_range(a + s.len()..a + s.len(), cx);
            st.focus(window, cx);
        });
    }

    /// One line of the preview, drawn with its colours and styles.
    fn preview_line(text: &str) -> StyledText {
        let mut s = String::new();
        let mut marks = Vec::new();
        for r in motd::runs(text) {
            let t = if r.obfuscated {
                // Scrambled letters, as in the game (a new set each draw).
                r.text.chars().enumerate().map(|(i, c)| if c == ' ' { c } else { char::from(b'!' + ((i * 37 + s.len() * 11 + 5) % 90) as u8) }).collect()
            } else {
                r.text.clone()
            };
            let start = s.len();
            s.push_str(&t);
            marks.push((
                start..s.len(),
                HighlightStyle {
                    color: Some(rgb(r.color).into()),
                    font_weight: r.bold.then_some(FontWeight::BOLD),
                    font_style: r.italic.then_some(FontStyle::Italic),
                    underline: r.underline.then(|| UnderlineStyle { thickness: px(1.), ..Default::default() }),
                    strikethrough: r.strike.then(|| StrikethroughStyle { thickness: px(1.), ..Default::default() }),
                    ..Default::default()
                },
            ));
        }
        StyledText::new(s).with_highlights(marks)
    }
}

/// The hover card on a variable: what it is and its value now.
struct VarCard(Shared);

impl HoverProvider for VarCard {
    fn hover(&self, text: &Rope, offset: usize, _: &mut Window, _: &mut App) -> Task<anyhow::Result<Option<Hover>>> {
        let shared = self.0.borrow();
        let (line, vars) = &*shared;
        let Some((r, name)) = line.var_at(offset) else {
            return Task::ready(Ok(None));
        };
        let md = match motd::VARS.iter().find(|v| v.0 == name) {
            Some((_, desc)) => match vars.get(name).filter(|v| !v.is_empty()) {
                Some(v) => format!("**{desc}**: {v}"),
                None => format!("**{desc}**: not known yet"),
            },
            None => "Not a variable Hopper knows: shown as typed".to_string(),
        };
        let range = lsp_types::Range::new(text.offset_to_position(r.start), text.offset_to_position(r.end));
        Task::ready(Ok(Some(Hover { contents: HoverContents::Markup(MarkupContent { kind: MarkupKind::Markdown, value: md }), range: Some(range) })))
    }
}

impl Render for MotdEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let rem = window.rem_size();
        let view = cx.entity();
        // A picked style lasts until the caret moves. A caret moved by typing
        // that the line hasn't taken in yet doesn't count: that typing gets
        // the style.
        if let Some((l, at, _)) = self.pending
            && self.lines[l].read(cx).selected_range() != (at..at)
            && self.lines[l].read(cx).value().as_ref() == self.models[l].borrow().0.text()
        {
            self.pending = None;
        }
        let cur = self.current(cx);
        let on_bg = theme.primary.opacity(0.22);
        let mut tools = h_flex().flex_wrap().gap_1p5().items_center();
        for (c, name, hex) in motd::COLORS {
            let on = cur.color == Some(c);
            tools = tools.child(
                div()
                    .id(SharedString::from(format!("motd-c-{c}")))
                    .size(px(18.))
                    .rounded(crate::theme::radius_sm())
                    .border_1()
                    .border_color(if on { theme.foreground } else { white().opacity(0.15) })
                    .when(on, |d| d.border_2())
                    .bg(rgb(hex))
                    .clickable(cx)
                    .tooltip(move |w, cx| Tooltip::new(format!("{name} (§{c})")).build(w, cx))
                    .on_click(cx.listener(move |this, _, window, cx| this.code(c, window, cx))),
            );
        }
        tools = tools.child(div().w(px(1.)).h(px(18.)).mx_1().bg(theme.border));
        for (c, name, label) in motd::FORMATS {
            let on = motd_edit::Attr::from_code(c).is_some_and(|a| cur.has(a));
            let b = div()
                .id(SharedString::from(format!("motd-f-{c}")))
                .size(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(crate::theme::RADIUS))
                .border_1()
                .border_color(if on { theme.primary } else { theme.border })
                .when(on, |d| d.bg(on_bg))
                .text_xs()
                .font_semibold()
                .child(label)
                .clickable(cx)
                .hover(|s| s.bg(theme.muted))
                .tooltip(move |w, cx| Tooltip::new(format!("{name} (§{c})")).build(w, cx))
                .on_click(cx.listener(move |this, _, window, cx| this.code(c, window, cx)));
            tools = tools.child(match c {
                'l' => b.font_weight(FontWeight::BOLD),
                'o' => b.italic(),
                'n' => b.underline(),
                'm' => b.line_through(),
                _ => b,
            });
        }
        tools = tools.child(div().w(px(1.)).h(px(18.)).mx_1().bg(theme.border));
        let vars = self.vars.clone();
        tools = tools.child(Button::new("motd-var").outline().small().icon(IconName::Braces).label("Variable").dropdown_caret(true).dropdown_menu(
            move |menu: PopupMenu, _, _| {
                let mut menu = menu.min_w(rems(20.).to_pixels(rem)).max_h(rems(24.).to_pixels(rem)).scrollable(true);
                for (k, desc) in motd::VARS {
                    let v = vars.get(k).cloned().unwrap_or_default();
                    let view = view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(format!("{desc}  {{{k}}}  {v}"))
                            .on_click(move |_, w, cx| view.update(cx, |this, cx| this.insert_var(k, w, cx))),
                    );
                }
                menu
            },
        ));
        let raw = self.raw;
        let toggle = cx.entity().downgrade();
        tools = tools.child(div().flex_1()).child(
            crate::switch::Switch::new("motd-codes").checked(raw).label("Codes").tooltip("Show and edit the § codes by hand").on_change(move |v, w, cx| {
                let v = *v;
                let _ = toggle.update(cx, |this, cx| this.set_raw(v, w, cx));
            }),
        );

        let filled: Vec<String> = (0..2).map(|i| motd::expand(&motd::amp_to_section(&self.line_value(i, cx)), &self.vars)).collect();
        // As the game draws it: word-wrapped at the list's width, two rows.
        let (shown, fits) = motd::list_lines(&filled[0], &filled[1]);
        let hint = motd::fit_hint(&fits, !filled[1].is_empty());
        let line_h = self.lines[0].read(cx).line_height().unwrap_or(px(20.));
        let line_row = |i: usize| {
            let (n, fit) = motd::fit_count(&filled[i], &fits[i]);
            let color = if fits[i].width > motd::LIST_WIDTH {
                theme.danger
            } else if fits[i].width > motd::LIST_WIDTH - 12 {
                theme.warning
            } else {
                theme.muted_foreground
            };
            let focused = self.lines[i].read(cx).focus_handle(cx).is_focused(window);
            let pend = self.pending.is_some_and(|p| p.0 == i);
            // Enter would start a second row: there's only one.
            let ignore_enter = |_: &gpui_kit::component::input::Enter, _: &mut Window, cx: &mut App| cx.stop_propagation();
            let boxed = div()
                .flex_1()
                .min_w_0()
                .h(line_h + px(12.))
                .px_1()
                .py(px(5.))
                .overflow_hidden()
                .rounded(theme.radius)
                .border_1()
                .when(pend, |d| d.border_dashed())
                .border_color(if focused { theme.primary } else { theme.input })
                .font_family(theme.mono_font_family.clone())
                .capture_action(ignore_enter)
                .when(!raw, |d| d.bg(rgb(0x17120d)))
                // XSmall: no padding of its own above the row.
                .child(Editor::new(&self.lines[i]).xsmall().appearance(false).bordered(false).h_full());
            h_flex()
                .gap_2()
                .child(div().w(rems(3.)).flex_none().text_xs().text_color(theme.muted_foreground).child(format!("Line {}", i + 1)))
                .child(boxed)
                .child(div().w(rems(3.5)).flex_none().text_right().text_xs().text_color(color).child(format!("{n} / {fit}")))
        };

        // The server list: a dark dirt-brown strip, a 64 px icon, the name
        // and player count, then the two lines.
        let letter = self.name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
        let mc_font = theme.mono_font_family.clone();
        // At 1.5x the game's GUI scale 1: the message area is LIST_WIDTH
        // game pixels, and a 6 px letter is one monospace cell (0.55 em).
        let scale = 1.5;
        let preview = div().p_2p5().overflow_hidden().rounded(theme.radius).bg(rgb(0x1d150e)).child(
            h_flex()
                .items_start()
                .gap_1p5()
                .child(
                    div()
                        .size(px(64.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(theme.primary)
                        .text_2xl()
                        .font_bold()
                        .text_color(rgb(0x1a1a1f))
                        .child(letter),
                )
                .child(
                    v_flex()
                        .flex_none()
                        .w(px(motd::LIST_WIDTH as f32 * scale))
                        .font_family(mc_font)
                        .text_size(px(6. * scale / 0.55))
                        .line_height(px(19.))
                        .child(
                            h_flex()
                                .justify_between()
                                .child(div().text_color(white()).child(self.name.clone()))
                                .child(div().text_color(rgb(0xaaaaaa)).child(self.players.clone())),
                        )
                        .children(shown.iter().map(|l| div().h(px(19.)).whitespace_nowrap().child(Self::preview_line(l)))),
                ),
        );

        v_flex()
            .w_full()
            .min_w_0()
            .max_w(rems(46.))
            .gap_2()
            .child(tools)
            .child(line_row(0))
            .child(line_row(1))
            .children(hint.map(|h| div().text_xs().text_color(theme.warning).child(h)))
            .child(widgets::fine_print(
                if raw {
                    "& codes like &6 work as a shortcut for §6. Variables fill in when the server starts; players, day and world size refresh every minute while it runs, for players using its address."
                } else {
                    "Select text and click a style to turn it on or off. Type a variable like {day} or pick one from Variable. Variables fill in when the server starts; players, day and world size refresh every minute while it runs, for players using its address."
                },
                cx,
            ))
            .child(preview)
    }
}
