//! Hopper's on/off switch.
//!
//! gpui-component's switch draws a full capsule whenever the theme radius
//! is 4px or more, which made it the roundest control in the app. This one
//! is built from the same gpui-base parts (keyboard, focus, accessibility
//! and the spring stay the same) but takes its corners from Hopper's radius
//! scale: the track uses the button radius, the thumb the small one.

use gpui_kit::base::{Switch as BaseSwitch, SwitchThumb, SwitchTrack, spring};
use gpui_kit::component::{ActiveTheme as _, Disableable, Sizable, Size, StyleSized as _, StyledExt as _, ThemeStyled as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::rc::Rc;

#[derive(IntoElement)]
pub struct Switch {
    id: ElementId,
    style: StyleRefinement,
    checked: bool,
    disabled: bool,
    label: Option<SharedString>,
    tooltip: Option<SharedString>,
    on_click: Option<Rc<dyn Fn(&bool, &mut Window, &mut App)>>,
    size: Size,
}

impl Switch {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            style: StyleRefinement::default(),
            checked: false,
            disabled: false,
            label: None,
            tooltip: None,
            on_click: None,
            size: Size::Medium,
        }
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    /// Text beside the switch; it also names the switch for screen readers.
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    /// Called with the new state when the switch is flipped.
    pub fn on_click(mut self, handler: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }

    /// Same as `on_click` (gpui-component has both names).
    pub fn on_change(self, handler: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_click(handler)
    }
}

impl Styled for Switch {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl Sizable for Switch {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}

impl Disableable for Switch {
    fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl RenderOnce for Switch {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let checked = self.checked;
        let focus = window.use_keyed_state(self.id.clone(), cx, |_, cx| cx.focus_handle()).read(cx).clone();
        let focused = focus.is_focused(window);
        let theme = cx.theme();
        let on_bg: Background = theme.tokens.primary.into();
        let off_bg: Background = theme.tokens.switch.into();
        // Fade the track only: element opacity would let it show through the thumb.
        let disabled_bg = if checked { on_bg } else { off_bg }.opacity(0.5);
        let thumb_bg: Background = theme.tokens.switch_thumb.into();
        let muted = theme.muted_foreground;
        let (w, h, knob) = match self.size {
            Size::XSmall | Size::Small => (px(28.), px(16.), px(12.)),
            Size::Large => (px(44.), px(24.), px(20.)),
            _ => (px(36.), px(20.), px(16.)),
        };
        let inset = px(2.);
        // The track hugs its thumb: the thumb's corner is the track's minus
        // the inset, so both curves stay parallel.
        let track_r = theme.radius;
        let thumb_r = (track_r - inset).max(px(crate::theme::RADIUS_SM) / 2.);
        let motion = theme.motion_tokens().spring_move.clone();
        let x = spring(
            (self.id.clone(), "thumb"),
            if checked { w - knob - inset * 2 } else { px(0.) },
            motion,
            window,
            cx,
        );
        let on_click = self.on_click.clone();
        let tooltip = self.tooltip.clone();
        let size = self.size;

        div().refine_style(&self.style).child(
            BaseSwitch::new(self.id.clone())
                .checked(checked)
                .disabled(self.disabled)
                .styles(|s| s.disabled(|s| s.text_color(muted).cursor_not_allowed()))
                .when_some(self.label.clone(), |this, l| this.accessibility_label(l))
                .when_some(on_click, |this, f| this.on_change(move |next, _, window, cx| f(&next, window, cx)))
                .track_focus(&focus)
                .h_flex()
                .gap_2()
                .items_start()
                .child(
                    SwitchTrack::new((self.id.clone(), "track"))
                        .checked(checked)
                        .disabled(self.disabled)
                        .w(w)
                        .h(h)
                        .flex_shrink_0()
                        .rounded(track_r)
                        .flex()
                        .items_center()
                        // A 1px border plus 1px padding, not a 2px border: the
                        // focus ring tints the border, and the line keeps it
                        // visible on an unchecked track.
                        .border_1()
                        .border_color(cx.theme().transparent)
                        .p(inset - px(1.))
                        .when(!checked, |t| t.bg(off_bg))
                        .styles(|s| s.checked(|s| s.bg(on_bg)).disabled(|s| s.bg(disabled_bg)))
                        .when(focused, |t| t.focus_ring_style(window, cx))
                        .when_some(tooltip, |t, tip| {
                            t.tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
                        })
                        .child(SwitchThumb::new(checked).rounded(thumb_r).size(knob).left(x).bg(thumb_bg)),
                )
                .when_some(self.label, |this, label| {
                    this.child(div().min_w_0().line_height(h).input_text_size(size).child(label))
                }),
        )
    }
}
