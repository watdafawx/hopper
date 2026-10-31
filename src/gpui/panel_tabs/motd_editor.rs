//! The server list message in a server's Settings: colour and style
//! buttons, variables, two line boxes and a preview drawn like Minecraft's
//! multiplayer list. Same as the panel's web editor; the editing rules live
//! in `hopper::hosting::motd`.

use crate::widgets;
use crate::widgets::Clickable as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::*;
use hopper::hosting::motd;
use std::collections::BTreeMap;

/// The message changed (the whole template, lines joined by "\n").
pub struct MotdChanged(pub String);

pub struct MotdEditor {
    lines: [Entity<InputState>; 2],
    /// The line the buttons work on: the one last focused.
    last: usize,
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
        let make = |v: String, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut st = InputState::new(window, cx);
                st.set_value(v, window, cx);
                st
            })
        };
        let lines = [make(a, window, cx), make(b, window, cx)];
        let mut subs = Vec::new();
        for (i, l) in lines.iter().enumerate() {
            subs.push(cx.subscribe(l, move |this: &mut Self, _, e: &InputEvent, cx| match e {
                InputEvent::Change => {
                    cx.emit(MotdChanged(this.value(cx)));
                    cx.notify();
                }
                InputEvent::Focus => this.last = i,
                _ => {}
            }));
        }
        Self { lines, last: 0, vars, name, players, _subs: subs }
    }

    pub fn value(&self, cx: &App) -> String {
        let a = self.lines[0].read(cx).value().to_string();
        let b = self.lines[1].read(cx).value().to_string();
        if b.is_empty() { a } else { format!("{a}\n{b}") }
    }

    /// Puts `code` around the selection (or at the caret) of the last line.
    fn code(&mut self, code: char, window: &mut Window, cx: &mut Context<Self>) {
        let line = self.lines[self.last].clone();
        line.update(cx, |st, cx| {
            // Shortcuts typed so far count as codes from here on (same length).
            let text = motd::amp_to_section(&st.value());
            let r = st.selected_range();
            let (out, a, b) = motd::apply_code(&text, r.start, r.end, code);
            st.set_value(out, window, cx);
            st.set_selected_range(a..b, cx);
            st.focus(window, cx);
        });
        cx.emit(MotdChanged(self.value(cx)));
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
        cx.emit(MotdChanged(self.value(cx)));
        cx.notify();
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

impl Render for MotdEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let rem = window.rem_size();
        let view = cx.entity();
        let mut tools = h_flex().flex_wrap().gap_1p5().items_center();
        for (c, name, hex) in motd::COLORS {
            tools = tools.child(
                div()
                    .id(SharedString::from(format!("motd-c-{c}")))
                    .size(px(18.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(white().opacity(0.15))
                    .bg(rgb(hex))
                    .clickable(cx)
                    .tooltip(move |w, cx| Tooltip::new(format!("{name} (§{c})")).build(w, cx))
                    .on_click(cx.listener(move |this, _, window, cx| this.code(c, window, cx))),
            );
        }
        tools = tools.child(div().w(px(1.)).h(px(18.)).mx_1().bg(theme.border));
        for (c, name, label) in motd::FORMATS {
            let b = div()
                .id(SharedString::from(format!("motd-f-{c}")))
                .size(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .border_1()
                .border_color(theme.border)
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

        let shown: Vec<String> = (0..2)
            .map(|i| motd::expand(&motd::amp_to_section(&self.lines[i].read(cx).value()), &self.vars))
            .collect();
        let line_row = |i: usize| {
            let n = motd::visible(&shown[i]);
            h_flex()
                .gap_2()
                .child(div().w(rems(3.)).flex_none().text_xs().text_color(theme.muted_foreground).child(format!("Line {}", i + 1)))
                .child(div().flex_1().min_w_0().font_family(theme.mono_font_family.clone()).child(Input::new(&self.lines[i]).small()))
                .child(
                    div()
                        .w(rems(3.5))
                        .flex_none()
                        .text_right()
                        .text_xs()
                        .text_color(if n > motd::LINE_CHARS { theme.warning } else { theme.muted_foreground })
                        .child(format!("{n} / {}", motd::LINE_CHARS)),
                )
        };

        // The server list: a dark dirt-brown strip, a 64 px icon, the name
        // and player count, then the two lines.
        let letter = self.name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
        let mc_font = theme.mono_font_family.clone();
        let preview = div().p_2p5().rounded(theme.radius).bg(rgb(0x1d150e)).child(
            h_flex()
                .items_start()
                .gap_1p5()
                .max_w(px(430.))
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
                        .flex_1()
                        .min_w_0()
                        .font_family(mc_font)
                        .text_size(px(15.))
                        .line_height(px(19.))
                        .child(
                            h_flex()
                                .justify_between()
                                .child(div().text_color(white()).child(self.name.clone()))
                                .child(div().text_color(rgb(0xaaaaaa)).child(self.players.clone())),
                        )
                        .children(shown.iter().map(|l| div().h(px(19.)).overflow_hidden().whitespace_nowrap().child(Self::preview_line(l)))),
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
            .child(widgets::fine_print(
                "& codes like &6 work as a shortcut for §6. Variables fill in when the server starts; players, day and world size refresh every minute while it runs, for players using its address.",
                cx,
            ))
            .child(preview)
    }
}
