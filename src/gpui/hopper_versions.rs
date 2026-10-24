//! Hopper's own versions, opened from the version beside the logo: the
//! update when there is one, and every earlier release to go back to, with
//! what going back gives up compared to the newest.

use crate::store::{SelfUpdate, Store};
use crate::widgets::{self, Clickable as _};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::selfupdate::{CURRENT, Release, is_newer};

/// The first Hopper that keeps settings it doesn't know: going back to an
/// earlier one resets settings added after it.
const KEEPS_NEWER_SETTINGS: &str = "0.8.21";

pub struct HopperVersions {
    store: Entity<Store>,
    releases: Option<Result<Vec<Release>, String>>,
    /// The version whose notes show on the right.
    picked: String,
    _watch: Subscription,
}

impl HopperVersions {
    fn load(&mut self, cx: &mut Context<Self>) {
        let repo = self.store.read(cx).update_repo();
        self.releases = None;
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move { hopper::selfupdate::list(&repo) })
                .await
                .map_err(|e| format!("{e:#}"));
            this.update(cx, |this, cx| {
                // The list doubles as an update check: the title bar and
                // Settings show what it found.
                if let Ok(list) = &r {
                    this.store.update(cx, |s, cx| {
                        s.saw_releases(list.first());
                        cx.notify();
                    });
                }
                // An update on offer opens on the newest release.
                if let Ok(list) = &r
                    && let Some(newest) = list.first()
                    && is_newer(&newest.version, CURRENT)
                    && this.picked == CURRENT
                {
                    this.picked = newest.version.clone();
                }
                this.releases = Some(r);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn notes(id: impl Into<SharedString>, r: &Release, cx: &App) -> AnyElement {
        let md = hopper::selfupdate::clean_notes(&r.notes);
        if md.is_empty() {
            return widgets::caption("No notes for this release.", cx).italic().into_any_element();
        }
        // The column around it scrolls; the text must not catch the wheel.
        TextView::markdown(ElementId::Name(id.into()), md)
            .selectable(true)
            .scrollable(false)
            .on_link_click(|url, _, _, cx| cx.open_url(url))
            .into_any_element()
    }

    /// One release's notes on a card, the version it's on outlined.
    fn card(r: &Release, cx: &App) -> Div {
        let theme = cx.theme();
        let this = r.version == CURRENT;
        v_flex()
            .gap_1p5()
            .p_3()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(if this { theme.primary } else { theme.border })
            .child(
                h_flex()
                    .gap_2()
                    .child(div().font_semibold().child(format!("Hopper {}", r.version)))
                    .child(widgets::caption(day(&r.published), cx).text_xs())
                    .when(this, |c| c.child(widgets::chip(theme.primary).child("You're on this"))),
            )
            .child(Self::notes(format!("hv-notes-{}", r.version), r, cx))
    }

    fn install(&mut self, r: Release, window: &mut Window, cx: &mut Context<Self>) {
        self.store.update(cx, |s, cx| {
            s.install_hopper_version(r);
            cx.notify();
        });
        window.close_dialog(cx);
    }

    /// What the picked version means next to the one running: its notes,
    /// what an update brings, or what going back drops.
    fn detail(&self, list: &[Release], busy: bool, cx: &mut Context<Self>) -> AnyElement {
        let Some(picked) = list.iter().find(|r| r.version == self.picked).cloned() else {
            return widgets::caption("Pick a version to see what changes.", cx).into_any_element();
        };
        let newest = list.first().map(|r| r.version.clone()).unwrap_or_default();
        let theme = cx.theme().clone();
        let ver = picked.version.clone();
        if ver == CURRENT {
            return v_flex()
                .gap_3()
                .child(div().font_semibold().child(format!("Hopper {ver} is the version you're on")))
                .child(Self::card(&picked, cx))
                .into_any_element();
        }
        let up = is_newer(&ver, CURRENT);
        // An update: the releases between this one and it. Going back:
        // everything after it, up to the newest.
        let changes: Vec<Release> = if up {
            list.iter()
                .filter(|r| is_newer(&r.version, CURRENT) && !is_newer(&r.version, &ver))
                .cloned()
                .collect()
        } else {
            hopper::selfupdate::after(list, &ver).into_iter().cloned().collect()
        };
        let (title, line) = if up {
            (
                format!("Update to {ver}"),
                format!(
                    "What you get over {CURRENT}: {} release{}.",
                    changes.len(),
                    if changes.len() == 1 { "" } else { "s" }
                ),
            )
        } else {
            (
                format!("Roll back to {ver}"),
                format!(
                    "What you'd give up compared to the latest ({newest}): {} release{}.",
                    changes.len(),
                    if changes.len() == 1 { "" } else { "s" }
                ),
            )
        };
        let button = Button::new("hv-install")
            .small()
            .icon(if up { IconName::Download } else { IconName::Undo2 })
            .label(if up { format!("Update to {ver}") } else { format!("Roll back to {ver}") })
            .disabled(busy)
            .map(|b| if up { b.primary() } else { b.danger() })
            .on_click(cx.listener(move |this, _, window, cx| this.install(picked.clone(), window, cx)));
        let mut notes = vec![
            "Instances, mods and worlds aren't touched. Hopper restarts into it once its tasks finish. Games and servers hosted on this PC keep running."
                .to_string(),
        ];
        if !up {
            notes.push("To come back, click the version beside the logo and update.".into());
            if is_newer(KEEPS_NEWER_SETTINGS, &ver) {
                notes.push(format!("Settings added after {ver} go back to their defaults."));
            }
        }
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().font_semibold().child(title))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(if up { theme.primary } else { theme.warning })
                                    .child(line),
                            ),
                    )
                    .child(button),
            )
            .child(
                v_flex()
                    .gap_0p5()
                    .px_3()
                    .py_2()
                    .rounded(theme.radius_lg)
                    .bg(theme.muted)
                    .children(notes.into_iter().map(|n| widgets::caption(n, cx).text_xs())),
            )
            .children(changes.iter().map(|r| Self::card(r, cx)))
            .into_any_element()
    }
}

/// 2026-10-18T09:08:51Z as 2026-10-18.
fn day(published: &str) -> String {
    published.split('T').next().unwrap_or("").to_string()
}

impl Render for HopperVersions {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rem = window.rem_size();
        // Under the same top offset as every dialog (see versions.rs).
        let total_h = crate::widgets::dialog_room(window).max(rem * 18.);
        let theme = cx.theme().clone();
        let state = self.store.read(cx).self_update().cloned();
        let busy = matches!(state, Some(SelfUpdate::Installing(_)));
        let status: AnyElement = match &state {
            Some(SelfUpdate::Available(r)) => widgets::pill(format!("{} is out", r.version), theme.primary, true).into_any_element(),
            Some(SelfUpdate::Installing(v)) => widgets::pill(format!("Installing {v}…"), theme.info, true).into_any_element(),
            Some(SelfUpdate::Ready(v)) => h_flex()
                .gap_2()
                .child(widgets::pill(format!("{v} installed"), theme.success, true))
                .child(
                    Button::new("hv-restart")
                        .xsmall()
                        .primary()
                        .label("Restart now")
                        .on_click(|_, _, cx| crate::settings_page::restart(cx)),
                )
                .into_any_element(),
            Some(SelfUpdate::UpToDate) => widgets::pill("Up to date", theme.success, true).into_any_element(),
            _ => div().into_any_element(),
        };
        let header = h_flex()
            .gap_3()
            .child(widgets::logo().size_8())
            .child(
                v_flex()
                    .child(div().font_semibold().child(format!("Hopper {CURRENT}")))
                    .child(widgets::caption("The version you're running", cx).text_xs()),
            )
            .child(status)
            .child(div().flex_1())
            .child(
                Button::new("hv-github")
                    .small()
                    .ghost()
                    .icon(IconName::ExternalLink)
                    .label("Releases on GitHub")
                    .on_click({
                        let repo = self.store.read(cx).update_repo();
                        move |_, _, cx| cx.open_url(&format!("https://github.com/{}/releases", repo.trim_matches('/')))
                    }),
            );
        let body: AnyElement = match &self.releases {
            None => h_flex()
                .gap_2()
                .py_6()
                .justify_center()
                .child(Spinner::new().small())
                .child(widgets::caption("Reading Hopper's releases…", cx))
                .into_any_element(),
            Some(Err(e)) => v_flex()
                .gap_2()
                .items_start()
                .child(div().text_sm().text_color(theme.danger).child(format!("Couldn't read the releases: {e}")))
                .child(
                    Button::new("hv-retry")
                        .small()
                        .outline()
                        .icon(IconName::RefreshCw)
                        .label("Try again")
                        .on_click(cx.listener(|this, _, _, cx| this.load(cx))),
                )
                .into_any_element(),
            Some(Ok(list)) => {
                let list = list.clone();
                let newest = list.first().map(|r| r.version.clone());
                let rows = list.iter().map(|r| {
                    let v = r.version.clone();
                    let picked = v == self.picked;
                    let accent = theme.primary;
                    let older = is_newer(CURRENT, &v);
                    h_flex()
                        .id(SharedString::from(format!("hv-row-{v}")))
                        .gap_2()
                        .px_2p5()
                        .py_1p5()
                        .rounded(theme.radius)
                        .clickable(cx)
                        .map(|r| if picked { widgets::row_selected(r, cx) } else { r.hover(|s| widgets::row_hover(s, cx)) })
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_medium()
                                        .when(picked, |d| d.text_color(accent))
                                        .when(older && !picked, |d| d.text_color(theme.muted_foreground))
                                        .child(v.clone()),
                                )
                                .child(widgets::caption(day(&r.published), cx).text_xs()),
                        )
                        .when(newest.as_deref() == Some(v.as_str()), |c| {
                            c.child(widgets::chip(theme.success).child("Latest"))
                        })
                        .when(v == CURRENT, |c| c.child(widgets::chip(theme.primary).child("Yours")))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.picked = v.clone();
                            cx.notify();
                        }))
                });
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .gap_4()
                    .child(
                        div()
                            .id("hv-list")
                            .w(rems(13.))
                            .h_full()
                            .flex_none()
                            .overflow_y_scrollbar()
                            .child(v_flex().gap_0p5().pr_3().children(rows)),
                    )
                    .child(div().w_px().h_full().bg(theme.border))
                    .child(
                        div()
                            .id("hv-detail")
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_y_scrollbar()
                            .child(div().pr_3().pb_px().child(self.detail(&list, busy, cx))),
                    )
                    .into_any_element()
            }
        };
        let loaded = matches!(self.releases, Some(Ok(_)));
        v_flex()
            .when(loaded, |c| c.h(total_h))
            .gap_4()
            .child(header)
            .child(body)
    }
}

/// Opens Hopper's versions.
pub fn open(store: Entity<Store>, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| {
        let watch = cx.observe(&store, |_, _, cx| cx.notify());
        let mut v = HopperVersions {
            store,
            releases: None,
            picked: CURRENT.to_string(),
            _watch: watch,
        };
        v.load(cx);
        v
    });
    window.open_dialog(cx, move |dialog, window, _| {
        dialog
            .title("Hopper versions")
            .w(rems(56.).to_pixels(window.rem_size()))
            .margin_top(rems(crate::widgets::DIALOG_TOP).to_pixels(window.rem_size()))
            .child(view.clone())
    });
}
