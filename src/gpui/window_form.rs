//! The game's Window setting (Window / Maximized / Fullscreen and, as a
//! window, its size). One view shared by an instance's Settings tab and
//! Settings > Playing & Java, so both look and word it the same.

use crate::widgets;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::instances::{WindowMode, WindowSetting};

const MODES: [WindowMode; 3] = [WindowMode::Window, WindowMode::Maximized, WindowMode::Fullscreen];

pub struct WindowForm {
    mode: WindowMode,
    width: Entity<InputState>,
    height: Entity<InputState>,
    /// The last size both fields held as numbers in range, for when one is
    /// empty or half typed.
    last: WindowSetting,
    _subscriptions: Vec<Subscription>,
}

impl WindowForm {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let width = cx.new(|cx| InputState::new(window, cx));
        let height = cx.new(|cx| InputState::new(window, cx));
        let subs = [&width, &height]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |_, _, e: &InputEvent, cx| {
                    if matches!(e, InputEvent::Change) {
                        cx.notify();
                    }
                })
            })
            .collect();
        Self { mode: WindowMode::Window, width, height, last: WindowSetting::default(), _subscriptions: subs }
    }

    /// Shows `s` (dropping what was typed).
    pub fn set(&mut self, s: WindowSetting, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = s.mode;
        self.last = s;
        let (w, h) = (s.width.to_string(), s.height.to_string());
        self.width.update(cx, |i, cx| i.set_value(w, window, cx));
        self.height.update(cx, |i, cx| i.set_value(h, window, cx));
        cx.notify();
    }

    /// What the form holds. A size that isn't a number keeps its last good
    /// value; sizes are held to what Minecraft can open.
    pub fn value(&self, cx: &App) -> WindowSetting {
        let num = |e: &Entity<InputState>, lo: u32, hi: u32, fallback: u32| {
            e.read(cx).value().trim().parse::<u32>().map_or(fallback, |v| v.clamp(lo, hi))
        };
        WindowSetting {
            mode: self.mode,
            width: num(&self.width, 640, 7680, self.last.width),
            height: num(&self.height, 480, 4320, self.last.height),
        }
    }
}

impl Render for WindowForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = self.mode;
        let hint = match mode {
            WindowMode::Window => "Minecraft opens in a window this size.",
            WindowMode::Maximized => "Minecraft opens filling the screen, with the taskbar showing.",
            WindowMode::Fullscreen => "Minecraft opens fullscreen. F11 switches to a window.",
        };
        v_flex()
            .gap_1p5()
            .child(
                widgets::Segmented::new("window-mode")
                    .selected_index(MODES.iter().position(|m| *m == mode).unwrap_or(0))
                    .child(widgets::SegItem::new().label("Window"))
                    .child(widgets::SegItem::new().label("Maximized"))
                    .child(widgets::SegItem::new().label("Fullscreen"))
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.mode = MODES[*ix];
                        cx.notify();
                    })),
            )
            .when(mode == WindowMode::Window, |c| {
                c.child(
                    h_flex()
                        .gap_2()
                        .child(div().w_24().child(Input::new(&self.width)))
                        .child(widgets::caption("×", cx))
                        .child(div().w_24().child(Input::new(&self.height))),
                )
            })
            .child(widgets::caption(hint, cx).text_xs())
    }
}
