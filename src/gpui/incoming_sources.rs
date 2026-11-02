//! A pack, share code, peer or sync bringing mods from GitHub and other
//! links. Before anything installs: with Settings › Mod sources off, ask
//! whether to allow them ("Allow and install" turns it on) or leave them
//! out ("Skip these mods"). Then, for each source this PC hasn't trusted
//! yet, the usual once-per-source confirm. The flows call [`gate`].

use crate::store::Store;
use crate::widgets::{self, Clickable as _};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::*;
use hopper::sources::{self, carry::Pin};
use std::rc::Rc;

type Then = Rc<dyn Fn(Vec<Pin>, &mut Window, &mut App)>;

/// Runs `then` with the mods to leave out (empty: install them all) once
/// the player has answered. Nothing to ask (no source mods): `then` runs
/// straight away. Closing the ask without an answer cancels the import.
pub fn gate(
    store: Entity<Store>,
    pins: Vec<Pin>,
    window: &mut Window,
    cx: &mut App,
    then: impl Fn(Vec<Pin>, &mut Window, &mut App) + 'static,
) {
    let then: Then = Rc::new(then);
    if pins.is_empty() {
        then(Vec::new(), window, cx);
        return;
    }
    if store.read(cx).settings().allow_sources {
        confirm_each(sources::carry::new_sources(&pins), then, window, cx);
        return;
    }
    let n = pins.len();
    // GitHub owners' pictures for the list, through the image cache.
    store.update(cx, |s, _| {
        for p in &pins {
            if let Some(u) = avatar(p) {
                s.want_image(&u);
            }
        }
    });
    window.open_dialog(cx, move |dialog, window, cx| {
        let (s1, p1, p2, t1, t2) = (store.clone(), pins.clone(), pins.clone(), then.clone(), then.clone());
        let theme = cx.theme().clone();
        dialog
            .title("Allow mods from other sources?")
            .w(rems(34.).to_pixels(window.rem_size()))
            .margin_top(rems(widgets::DIALOG_TOP).to_pixels(window.rem_size()))
            .child(
                v_flex()
                    .gap_3()
                    .child(div().text_sm().child(if n == 1 {
                        "This pack has 1 mod from GitHub and other links. Mods from other sources are off on this PC.".to_string()
                    } else {
                        format!("This pack has {n} mods from GitHub and other links. Mods from other sources are off on this PC.")
                    }))
                    .child(list(&store, &pins, cx))
                    .child(div().text_sm().text_color(theme.muted_foreground).child(
                        "Allow turns on Settings › Mod sources, then asks about each new source. Skip installs everything else and leaves these out.",
                    )),
            )
            .footer(
                DialogFooter::new()
                    .child(div().size_full().child(Button::new("incoming-skip").outline().w_full().label("Skip these mods").on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        let (p, t) = (p1.clone(), t1.clone());
                        window.defer(cx, move |window, cx| t(p, window, cx));
                    })))
                    .child(DialogAction::new().child(Button::new("incoming-allow").primary().label("Allow and install"))),
            )
            .on_ok(move |_, window, cx| {
                s1.update(cx, |st, cx| st.update_settings(|x| x.allow_sources = true, cx));
                let (p, t) = (p2.clone(), t2.clone());
                // The trust confirms open once this dialog has closed.
                window.defer(cx, move |window, cx| confirm_each(sources::carry::new_sources(&p), t, window, cx));
                true
            })
    });
}

/// The mods a pack brings from other sources: icon, name, where from.
/// Clicking one opens its page.
pub fn list(store: &Entity<Store>, pins: &[Pin], cx: &App) -> AnyElement {
    let theme = cx.theme();
    let store = store.read(cx);
    v_flex()
        .gap_0p5()
        .p_1()
        .max_h(rems(16.))
        .id("incoming-list")
        .overflow_y_scroll()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .children(pins.iter().enumerate().map(|(i, p)| {
            let name = sources::carry::display_name(p);
            let page = p.source.page();
            let icon = avatar(p).and_then(|u| store.image(&u)).map(|p| p.to_path_buf());
            let row = h_flex()
                .id(("incoming-mod", i))
                .gap_2()
                .px_2()
                .py_1()
                .rounded(theme.radius)
                .child(widgets::thumb(icon.as_deref(), rems(1.25), cx))
                .child(div().min_w_0().truncate().text_sm().child(name))
                .child(
                    div()
                        .ml_auto()
                        .flex_none()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("{}: {}", p.source.kind.label(), p.source.publisher)),
                )
                .clickable(cx)
                .on_click(move |_, _, cx| cx.open_url(&page));
            widgets::row_hover(row, cx)
        }))
        .into_any_element()
}

/// The once-per-source confirm for each source not trusted yet, one after
/// another; then `then` (installing everything either way: an untrusted
/// source's mod still installs, pinned, and just gets no updates).
fn confirm_each(mut srcs: Vec<sources::ModSource>, then: Then, window: &mut Window, cx: &mut App) {
    if srcs.is_empty() {
        then(Vec::new(), window, cx);
        return;
    }
    let src = srcs.remove(0);
    window.open_dialog(cx, move |dialog, window, cx| {
        let (s2, rest1, rest2, t1, t2) = (src.clone(), srcs.clone(), srcs.clone(), then.clone(), then.clone());
        let theme = cx.theme().clone();
        dialog
            .title(format!("Get updates from {}?", src.publisher))
            .w(rems(32.).to_pixels(window.rem_size()))
            .margin_top(rems(widgets::DIALOG_TOP).to_pixels(window.rem_size()))
            .child(
                v_flex()
                    .gap_2()
                    .child(div().text_sm().child(format!(
                        "Mods from {} install as the exact file the pack names, checked by its sha256. Hopper checks {} for updates once you trust it.",
                        src.publisher,
                        src.page()
                    )))
                    .child(
                        v_flex()
                            .gap_1()
                            .p_3()
                            .rounded(px(crate::theme::RADIUS_LG))
                            .bg(theme.warning.opacity(widgets::alpha::TINT))
                            .child(div().text_sm().font_semibold().child(format!("First file from {}", src.publisher)))
                            .child(div().text_sm().child(format!(
                                "Files from {} aren't checked by CurseForge or Modrinth. Trust them only from people you know. Hopper never runs anything from the download.",
                                src.kind.label()
                            ))),
                    ),
            )
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("trust-later").outline().label("Not now")))
                    .child(DialogAction::new().child(Button::new("trust-ok").primary().label("Trust and check for updates"))),
            )
            // Not now (or closing it): the mod still installs, without updates.
            .on_cancel(move |_, window, cx| {
                let (r, t) = (rest1.clone(), t1.clone());
                window.defer(cx, move |window, cx| confirm_each(r, t, window, cx));
                true
            })
            .on_ok(move |_, window, cx| {
                if let Err(e) = sources::trust(&s2) {
                    window.push_notification(gpui_kit::component::notification::Notification::error(format!("{e:#}")), cx);
                }
                let (r, t) = (rest2.clone(), t2.clone());
                window.defer(cx, move |window, cx| confirm_each(r, t, window, cx));
                true
            })
    });
}

/// A GitHub mod's owner picture (other sources have none to show).
fn avatar(p: &Pin) -> Option<String> {
    (p.source.kind == sources::Kind::GitHub).then(|| sources::looks::avatar(&p.source.url)).flatten()
}
