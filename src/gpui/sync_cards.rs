//! The friend-sync screens that sit in the group chip's popover and in dialogs
//! (design/sync-rewrite-ui.md): the "Needs you" cards, the rows of "This PC
//! differs", and the "while you were away" note.
//!
//! Everything here draws data the core already worded
//! (`hopper::synccore::cards`) and calls back through an [`Env`], so the same
//! views run on the real engine and in the preview states without a friend.

use crate::widgets::{self, Clickable as _};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::*;
use hopper::synccore::Answer;
use hopper::synccore::cards::{Btn, Card, CardMod, DriftRow, MOD, What};
use std::path::PathBuf;
use std::rc::Rc;

/// A mod as the screen shows it.
#[derive(Clone, Debug, Default)]
pub struct ModInfo {
    pub name: String,
    pub icon: Option<PathBuf>,
    /// 0: no page to open.
    pub project: u64,
}

/// What the views need from the app: who a mod is, and what the buttons do.
#[derive(Clone)]
pub struct Env {
    /// The mod of a jar's file name (a name from the file when it's unknown).
    pub info: Rc<dyn Fn(&str, &App) -> ModInfo>,
    /// Open a mod's page beside what the player is on.
    pub peek: Rc<dyn Fn(u64, &mut Window, &mut App)>,
    /// A card's answer: `None` puts it aside; the flag also answers the
    /// other cards of its kind.
    pub answer: Rc<dyn Fn(&Card, Option<Answer>, bool, &mut Window, &mut App)>,
    /// A row of "This PC differs": put it back, or share it with everyone.
    pub put_back: Rc<dyn Fn(&DriftRow, &mut Window, &mut App)>,
    pub share: Rc<dyn Fn(&DriftRow, &mut Window, &mut App)>,
}

/// The chip's popover is 27.5 rem wide (`group_pack`): when the window leaves
/// little room around it (a big zoom, a narrow window) the second button of a
/// "This PC differs" row folds into a menu.
pub fn is_narrow(window: &Window) -> bool {
    window.viewport_size().width < rems(27.5).to_pixels(window.rem_size()) + px(120.)
}

/// A name the file gives when nothing else does: "jade-1.2.jar" is "jade-1.2".
pub fn name_of_file(file: &str) -> String {
    let f = file.trim_end_matches(".disabled");
    f.strip_suffix(".jar").unwrap_or(f).to_string()
}

fn link(env: &Env, id: SharedString, info: &ModInfo, cx: &App) -> AnyElement {
    let peek = env.peek.clone();
    widgets::mod_link(id, info.name.clone(), info.icon.as_deref(), info.project, move |p, w, cx| peek(p, w, cx), cx).font_semibold().into_any_element()
}

/// How many mod rows a card shows before "and N more".
const SHOWN: usize = 4;

/// The "Needs you" cards, one per friend and batch (the list scrolls in the
/// popover).
pub fn cards_view(cards: Vec<Card>, env: Env, window: &mut Window, cx: &mut App) -> AnyElement {
    let mut list = v_flex().w_full().gap_2();
    for card in cards {
        list = list.child(card_view(card, &env, window, cx));
    }
    list.into_any_element()
}

fn what_color(w: What, cx: &App) -> Hsla {
    let t = cx.theme();
    match w {
        What::Removed | What::TurnedOff => t.warning,
        What::Added | What::TurnedOn => t.success,
        What::Updated => t.info,
        What::Other => t.muted_foreground,
    }
}

fn card_view(card: Card, env: &Env, window: &mut Window, cx: &mut App) -> AnyElement {
    let theme = cx.theme().clone();
    let key = card.id;
    let ticked = window.use_keyed_state(ElementId::Name(format!("card-tick:{key}").into()), cx, |_, _| false);
    let all_others = window.use_keyed_state(ElementId::Name(format!("card-others:{key}").into()), cx, |_, _| false);
    let expanded = window.use_keyed_state(ElementId::Name(format!("card-more:{key}").into()), cx, |_, _| false);
    let is_ticked = *ticked.read(cx);
    let others_too = *all_others.read(cx);
    let show_all = *expanded.read(cx);

    // The headline, with the mod's icon and name where the text says {mod}.
    let first = card.mods.first().map(|m| (env.info)(&m.file, cx));
    let mut head = h_flex().min_w_0().flex_wrap().items_center().text_sm();
    let (before, after) = card.headline.split_once(MOD).map_or((card.headline.as_str(), ""), |(a, b)| (a, b));
    head = head.child(div().child(before.to_string()));
    if card.headline.contains(MOD)
        && let Some(info) = &first
    {
        head = head.child(link(env, format!("card-mod:{key}").into(), info, cx));
        head = head.child(div().child(after.to_string()));
    }

    let mut body = v_flex().min_w_0().flex_1().gap_1().child(head);
    if !card.stays.is_empty() {
        body = body.child(div().text_xs().text_color(theme.muted_foreground).child(card.stays.clone()));
    }
    // The mods in the batch (a card about one mod has its name in the headline).
    if card.mods.len() > 1 || (card.mods.len() == 1 && !card.headline.contains(MOD)) {
        let rows: Vec<&CardMod> = if show_all { card.mods.iter().collect() } else { card.mods.iter().take(SHOWN).collect() };
        let mut list = v_flex().w_full().gap_0p5().mt_1();
        for (ix, m) in rows.iter().enumerate() {
            let info = (env.info)(&m.file, cx);
            list = list.child(
                h_flex()
                    .id(ElementId::Name(format!("card-row:{key}:{ix}").into()))
                    .w_full()
                    .gap_2()
                    .items_center()
                    .text_sm()
                    .child(h_flex().min_w_0().flex_1().child(link(env, format!("card-mod:{key}:{ix}").into(), &info, cx)))
                    .child(widgets::chip(what_color(m.what, cx)).child(m.what.says())),
            );
        }
        if card.mods.len() > SHOWN {
            let more = card.mods.len() - SHOWN;
            let ex = expanded.clone();
            list = list.child(
                div()
                    .id(("card-more", key as usize))
                    .clickable(cx)
                    .rounded(theme.radius)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(if show_all { "Show fewer".to_string() } else { format!("and {more} more") })
                    .on_click(move |_, _, cx| ex.update(cx, |v, cx| {
                        *v = !*v;
                        cx.notify();
                    })),
            );
        }
        body = body.child(list);
    }
    if let Some(label) = &card.tick {
        let t = ticked.clone();
        body = body.child(
            Checkbox::new(ElementId::Name(format!("card-tick-box:{key}").into()))
                .mt_1()
                .label(label.clone())
                .checked(is_ticked)
                .on_click(move |v: &bool, _, cx| {
                    let v = *v;
                    t.update(cx, |s, cx| {
                        *s = v;
                        cx.notify();
                    });
                }),
        );
    }
    if card.others > 0 {
        let o = all_others.clone();
        body = body.child(
            Checkbox::new(ElementId::Name(format!("card-others-box:{key}").into()))
                .label(if card.others == 1 { "Do the same for the other one".to_string() } else { format!("Do the same for the other {}", card.others) })
                .checked(others_too)
                .on_click(move |v: &bool, _, cx| {
                    let v = *v;
                    o.update(cx, |s, cx| {
                        *s = v;
                        cx.notify();
                    });
                }),
        );
    }
    let mut buttons = h_flex().w_full().flex_wrap().gap_2().mt_1();
    for (ix, b) in card.buttons.iter().enumerate() {
        buttons = buttons.child(card_button(&card, b, ix, is_ticked, others_too, env, cx));
    }
    body = body.child(buttons);

    h_flex()
        .id(("card", key as usize))
        .w_full()
        .gap_2()
        .p_3()
        .items_start()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.muted.opacity(widgets::alpha::HOVER))
        .child(widgets::letter_tile(&card.who, theme.muted_foreground, rems(1.75), cx))
        .child(body)
        .into_any_element()
}

fn card_button(card: &Card, b: &Btn, ix: usize, ticked: bool, others: bool, env: &Env, cx: &App) -> Button {
    let mut label = hopper::synccore::cards::button_label(b, ticked);
    // "Keep {mod}": a button has no room for an icon, so it says the name.
    if label.contains(MOD) {
        let name = card.mods.first().map(|m| (env.info)(&m.file, cx).name).filter(|n| !n.is_empty()).unwrap_or_else(|| "it".to_string());
        label = label.replace(MOD, &name);
    }
    let ready = hopper::synccore::cards::button_ready(b, ticked);
    let (card2, answer, answer_fn) = (card.clone(), b.answer, env.answer.clone());
    let btn = Button::new(ElementId::Name(format!("card-btn:{}:{ix}", card.id).into())).small().label(label).disabled(!ready);
    let btn = if b.lead { btn.primary() } else { btn.outline() };
    btn.on_click(move |_, window, cx| answer_fn(&card2, answer, others, window, cx))
}

/// The rows of "This PC differs": what changed outside Hopper, with Put back
/// and a second button that shares it with everyone, after a tick.
pub fn drift_view(rows: Vec<DriftRow>, env: Env, narrow: bool, window: &mut Window, cx: &mut App) -> AnyElement {
    let mut list = v_flex().w_full().gap_1();
    for (ix, row) in rows.into_iter().enumerate() {
        list = list.child(drift_row(row, ix, &env, narrow, window, cx));
    }
    list.into_any_element()
}

fn drift_row(row: DriftRow, ix: usize, env: &Env, narrow: bool, window: &mut Window, cx: &mut App) -> AnyElement {
    let theme = cx.theme().clone();
    let confirming = window.use_keyed_state(ElementId::Name(format!("drift-confirm:{ix}:{}", row.file).into()), cx, |_, _| false);
    let ticked = window.use_keyed_state(ElementId::Name(format!("drift-tick:{ix}:{}", row.file).into()), cx, |_, _| false);
    let (is_confirming, is_ticked) = (*confirming.read(cx), *ticked.read(cx));
    let mut info = (env.info)(&row.file, cx);
    if info.name.is_empty() {
        info.name = name_of_file(&row.file);
    }
    let put = {
        let (f, r) = (env.put_back.clone(), row.clone());
        Button::new(("drift-put", ix)).small().outline().label(row.put_back).on_click(move |_, w, cx| f(&r, w, cx))
    };
    let share_label = hopper::synccore::cards::share_label(&row, &info.name);
    let open_confirm = {
        let c = confirming.clone();
        move |cx: &mut App| {
            c.update(cx, |v, cx| {
                *v = !*v;
                cx.notify();
            })
        }
    };
    // The second button: beside the first, or in a ⋯ menu when there's no room.
    let share: AnyElement = if narrow {
        let open = open_confirm.clone();
        let text = row.share;
        Button::new(("drift-more", ix))
            .small()
            .ghost()
            .icon(IconName::Ellipsis)
            .tooltip("More")
            .dropdown_menu(move |menu, _, _| {
                let open = open.clone();
                menu.item(PopupMenuItem::new(text).on_click(move |_, _, cx| open(cx)))
            })
            .into_any_element()
    } else {
        let open = open_confirm.clone();
        Button::new(("drift-share", ix)).small().ghost().label(row.share).on_click(move |_, _, cx| open(cx)).into_any_element()
    };
    let head = h_flex()
        .w_full()
        .gap_2()
        .items_center()
        .child(
            v_flex()
                .min_w_0()
                .flex_1()
                .child(h_flex().min_w_0().text_sm().child(link(env, format!("drift-mod:{ix}").into(), &info, cx)))
                .child(div().text_xs().text_color(theme.muted_foreground).truncate().child(row.reason.clone())),
        )
        .child(put)
        .child(share);
    let mut out = v_flex().id(("drift-row", ix)).w_full().p_2().gap_2().rounded(theme.radius).hover(|d| widgets::row_hover(d, cx)).child(head);
    if is_confirming {
        let (t, f, r) = (ticked.clone(), env.share.clone(), row.clone());
        out = out.child(
            // Wraps: a long mod name makes the button too wide for the tick's line.
            h_flex()
                .w_full()
                .flex_wrap()
                .gap_x_3()
                .gap_y_2()
                .items_center()
                .child(
                    Checkbox::new(("drift-tick-box", ix))
                        .label(if row.added { "Everyone gets this mod." } else { "It goes from everyone's PC too." })
                        .checked(is_ticked)
                        .on_click(move |v: &bool, _, cx| {
                            let v = *v;
                            t.update(cx, |s, cx| {
                                *s = v;
                                cx.notify();
                            });
                        }),
                )
                .child(Button::new(("drift-do", ix)).small().primary().label(share_label).disabled(!is_ticked).on_click(move |_, w, cx| f(&r, w, cx))),
        );
    }
    out.into_any_element()
}

/// One friend's "while you were away" note: what they changed, with See (their
/// changes in History) and "Put back…" (the dialog).
pub fn away_row(ix: usize, name: &str, changes: usize, see: Rc<dyn Fn(&mut Window, &mut App)>, put: Rc<dyn Fn(&mut Window, &mut App)>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let says = format!("{name} changed {} while you were away.", if changes == 1 { "1 mod".to_string() } else { format!("{changes} mods") });
    h_flex()
        .id(("away-row", ix))
        .w_full()
        .gap_2()
        .p_2()
        .items_center()
        .rounded(theme.radius)
        .child(gpui_kit::component::Icon::new(IconName::Info).small().text_color(theme.info))
        .child(div().min_w_0().flex_1().text_sm().child(says))
        .child(Button::new(("away-see", ix)).small().ghost().label("See").on_click(move |_, w, cx| see(w, cx)))
        .child(Button::new(("away-put", ix)).small().outline().label("Put back…").on_click(move |_, w, cx| put(w, cx)))
        .into_any_element()
}
