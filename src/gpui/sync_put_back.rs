//! "Put back…" on a friend's note: undo what that friend changed, for
//! everyone (design/sync-rewrite-ui.md, back-from-away). The dialog offers
//! three spans, lists what each would do and what it leaves alone, and asks
//! for a tick only when it would take more than a few mods away.

use crate::store::Store;
use crate::widgets::{self, SegItem, Segmented};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::*;
use hopper::synccore::cards::{UndoPlan, undo_button};
use std::path::PathBuf;

/// How far back "Put back" can reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Span {
    Today,
    Away,
    Week,
}

impl Span {
    pub const ALL: [Span; 3] = [Span::Today, Span::Away, Span::Week];

    pub fn label(self) -> &'static str {
        match self {
            Span::Today => "Today",
            Span::Away => "Since you were away",
            Span::Week => "This week",
        }
    }

    /// Relay time the span starts at.
    pub fn since(self, now: u64, away_since: u64) -> u64 {
        match self {
            Span::Today => now.saturating_sub(86_400),
            Span::Away => away_since,
            Span::Week => now.saturating_sub(7 * 86_400),
        }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The dialog's rows, from a plan: what each change does, and what stays.
pub fn plan_view(plan: &UndoPlan, env: &crate::sync_cards::Env, cx: &App) -> AnyElement {
    let theme = cx.theme().clone();
    let mut list = v_flex().w_full().gap_1();
    for (ix, r) in plan.rows.iter().enumerate() {
        let info = (env.info)(&r.file, cx);
        let peek = env.peek.clone();
        let name = if info.name.is_empty() { crate::sync_cards::name_of_file(&r.file) } else { info.name.clone() };
        list = list.child(
            h_flex()
                .id(("undo-row", ix))
                .w_full()
                .gap_2()
                .p_1()
                .items_center()
                .text_sm()
                .rounded(theme.radius)
                .hover(|d| widgets::row_hover(d, cx))
                .child(h_flex().flex_none().max_w(rems(14.)).overflow_hidden().child(widgets::mod_link(format!("undo-mod:{ix}"), name, info.icon.as_deref(), info.project, move |p, w, cx| peek(p, w, cx), cx).font_semibold()))
                .child(div().min_w_0().flex_1().truncate().text_color(theme.muted_foreground).child(r.says.clone())),
        );
    }
    if !plan.left.is_empty() {
        let n = plan.left.len();
        list = list.child(
            div()
                .mt_2()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!("{} left alone: someone changed {} after them.", if n == 1 { "1 mod".to_string() } else { format!("{n} mods") }, if n == 1 { "it" } else { "them" })),
        );
    }
    list.into_any_element()
}

/// Opens the dialog for `member`'s changes (`since`: when this PC last had
/// contact before them, for "Since you were away").
pub fn open(store: Entity<Store>, root: PathBuf, member: String, name: String, away_since: u64, window: &mut Window, cx: &mut App) {
    let many = hopper::synccore::Limits::default().many_removals;
    window.open_dialog(cx, move |dialog, window, cx| {
        let span_state = window.use_keyed_state(ElementId::Name(format!("put-back-span:{member}").into()), cx, |_, _| 1usize);
        let tick_state = window.use_keyed_state(ElementId::Name(format!("put-back-tick:{member}").into()), cx, |_, _| false);
        let (span_ix, ticked) = (*span_state.read(cx), *tick_state.read(cx));
        let span = Span::ALL[span_ix.min(2)];
        let now = unix_now();
        let plan = store.read(cx).sync_undo_plan(&root, &member, span.since(now, away_since));
        let (label, tick) = undo_button(plan.rows.len(), plan.removes, many);
        let ready = !plan.rows.is_empty() && (tick.is_none() || ticked);
        let env = crate::group_pack::sync_env(&store, &root);
        let theme = cx.theme().clone();

        let seg = Segmented::new("put-back-span")
            .children(Span::ALL.iter().map(|s| SegItem::new().label(s.label())))
            .selected_index(span_ix)
            .on_click({
                let (s, t) = (span_state.clone(), tick_state.clone());
                move |ix: &usize, _, cx| {
                    let ix = *ix;
                    s.update(cx, |v, cx| {
                        *v = ix;
                        cx.notify();
                    });
                    t.update(cx, |v, cx| {
                        *v = false;
                        cx.notify();
                    });
                }
            });
        let mut body = v_flex().w_full().gap_3().child(div().text_sm().child(format!("Everyone gets what {name} changed put back, as your changes. History keeps both.")));
        body = body.child(seg);
        body = body.child(if plan.rows.is_empty() {
            div().text_sm().text_color(theme.muted_foreground).child(format!("{name} changed nothing in this time that can be put back.")).into_any_element()
        } else {
            div().w_full().max_h(rems(18.)).id("put-back-rows").overflow_y_scrollbar().child(plan_view(&plan, &env, cx)).into_any_element()
        });
        if let Some(text) = tick {
            let t = tick_state.clone();
            body = body.child(Checkbox::new("put-back-tick-box").label(text).checked(ticked).on_click(move |v: &bool, _, cx| {
                let v = *v;
                t.update(cx, |s, cx| {
                    *s = v;
                    cx.notify();
                });
            }));
        }
        let (st, rt, mid) = (store.clone(), root.clone(), member.clone());
        let go = Button::new("put-back-go").primary().label(label).disabled(!ready);
        dialog
            .title(format!("Put back {name}'s changes"))
            .w(rems(34.).to_pixels(window.rem_size()))
            .margin_top(rems(widgets::DIALOG_TOP).to_pixels(window.rem_size()))
            .child(body)
            .footer(DialogFooter::new().child(DialogClose::new().child(Button::new("put-back-cancel").outline().label("Cancel"))).child(DialogAction::new().child(go)))
            .on_ok(move |_, window, cx| {
                if !ready {
                    return false;
                }
                let (st, rt, mid) = (st.clone(), rt.clone(), mid.clone());
                st.update(cx, |s, cx| s.sync_undo_member(&rt, &mid, span.since(unix_now(), away_since), cx));
                window.close_dialog(cx);
                true
            })
    });
}
