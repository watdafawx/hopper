//! The delete question every delete of an instance, world, backup or
//! server on this PC asks: what goes, where it goes (Recently deleted, for
//! a week), and a "Delete for good now" tick box that changes the button.
//! No typing a name: the tick box is the deliberate step.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::rc::Rc;

/// The tick box's label, here and in the server delete.
pub const FOR_GOOD: &str = "Delete for good now (can't be undone)";
/// The button once it's ticked.
pub const FOR_GOOD_OK: &str = "Delete for good";

/// Where a delete goes, for the line under what goes. `many`: they.
pub fn where_text(for_good: bool, many: bool) -> String {
    let days = hopper::recently_deleted::KEEP_DAYS;
    match (for_good, many) {
        (true, false) => "It's deleted for good now and can't be restored.".into(),
        (true, true) => "They're deleted for good now and can't be restored.".into(),
        (false, false) => format!("It moves to Recently deleted (Settings › Library & folders) for {days} days, then goes for good."),
        (false, true) => format!("They move to Recently deleted (Settings › Library & folders) for {days} days, then go for good."),
    }
}

/// The note and the tick box, for a form of its own (the server delete).
pub fn for_good_rows<V: 'static>(
    for_good: bool,
    many: bool,
    cx: &mut Context<V>,
    on_change: impl Fn(&mut V, bool, &mut Context<V>) + 'static,
) -> Div {
    let theme = cx.theme();
    let color = if for_good { theme.danger } else { theme.muted_foreground };
    // Two lines' room either way, so ticking the box (one line of text)
    // doesn't pull the buttons up under the pointer.
    v_flex()
        .gap_3()
        .child(div().min_h(rems(2.5)).text_xs().text_color(color).child(where_text(for_good, many)))
        .child(
            Checkbox::new("delete-for-good")
                .label(FOR_GOOD)
                .checked(for_good)
                .on_change(cx.listener(move |this, v: &bool, _, cx| {
                    on_change(this, *v, cx);
                    cx.notify();
                })),
        )
}

struct Form {
    what: String,
    many: bool,
    for_good: bool,
}

impl Render for Form {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let (what, many, for_good) = (self.what.clone(), self.many, self.for_good);
        v_flex()
            .gap_3()
            .when(!what.is_empty(), |c| c.child(div().text_sm().text_color(muted).child(what)))
            .child(for_good_rows(for_good, many, cx, |this: &mut Self, v, _| this.for_good = v))
    }
}

/// Asks before deleting: `title`, `what` goes (a sentence, or empty),
/// `ok` the button (it reads "Delete for good" once ticked). `on_ok` gets
/// whether to delete for good.
pub fn open(
    title: String,
    what: String,
    many: bool,
    ok: &'static str,
    window: &mut Window,
    cx: &mut App,
    on_ok: impl Fn(bool, &mut Window, &mut App) + 'static,
) {
    let form = cx.new(|_| Form { what, many, for_good: false });
    let on_ok = Rc::new(on_ok);
    window.open_dialog(cx, move |dialog, window, cx| {
        let for_good = form.read(cx).for_good;
        let (f, on_ok) = (form.clone(), on_ok.clone());
        dialog
            .title(title.clone())
            .w(rems(30.).to_pixels(window.rem_size()))
            .child(form.clone())
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("dialog-cancel").outline().label("Cancel")))
                    .child(DialogAction::new().child(Button::new("dialog-ok").danger().label(if for_good { FOR_GOOD_OK } else { ok }))),
            )
            .on_ok(move |_, window, cx| {
                let for_good = f.read(cx).for_good;
                on_ok(for_good, window, cx);
                true
            })
    });
}
