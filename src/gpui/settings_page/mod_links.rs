//! Settings › Mod sources › "Links from other apps": copied CurseForge and
//! Modrinth links offered in a notification (clip_watch.rs), and Hopper as
//! a choice for links in Windows' Default apps (link_open.rs, browser.rs).

use super::*;
use std::sync::Mutex;

/// The browsers on the PC, read once per visit to the card (the registry
/// isn't read on every frame).
static BROWSERS: Mutex<Option<Vec<hopper::browser::Browser>>> = Mutex::new(None);

fn browsers() -> Vec<hopper::browser::Browser> {
    let mut b = BROWSERS.lock().unwrap_or_else(|e| e.into_inner());
    b.get_or_insert_with(hopper::browser::installed).clone()
}

/// Where other links go: the saved choice, else the browser Windows uses
/// now (when it isn't Hopper), else the first one on the PC.
fn chosen(saved: &str, list: &[hopper::browser::Browser]) -> Option<String> {
    let usable = |p: &str| !p.is_empty() && !p.starts_with("HopperURL");
    Some(saved.to_string())
        .filter(|p| usable(p))
        .or_else(|| hopper::browser::current_default().filter(|p| usable(p)))
        .or_else(|| list.first().map(|b| b.prog_id.clone()))
}

impl SettingsPage {
    pub(super) fn mod_links(&self, s: &hopper::settings::Settings, rem: Pixels, cx: &mut Context<Self>) -> Block {
        let theme = cx.theme().clone();
        let clip = Row::new("clip-links").control(self.switch("clip-links", s.clip_links, |st, v| st.clip_links = v, cx));
        let open = Row::new("open-links").control(
            Switch::new("open-links")
                .checked(s.open_links)
                .on_click(cx.listener(|this, v: &bool, window, cx| this.set_open_links(*v, window, cx))),
        );
        let mut rows = vec![clip, open];
        if s.open_links {
            let list = browsers();
            let now = chosen(&s.open_links_browser, &list);
            let name = |p: &str| list.iter().find(|b| b.prog_id == p).map_or_else(|| p.to_string(), |b| b.name.clone());
            let label = now.as_deref().map_or_else(|| "None found".to_string(), name);
            let ids: Vec<String> = list.iter().map(|b| b.prog_id.clone()).collect();
            let store = self.store.clone();
            rows.push(Row::new("open-links-browser").control(div().w(rems(14.)).child(widgets::picker(
                "open-links-browser",
                label,
                list.iter().map(|b| (b.name.clone(), Some(&b.prog_id) == now.as_ref())).collect(),
                rem,
                Rc::new(move |ix, window, cx| {
                    let Some(id) = ids.get(ix).cloned() else { return };
                    // The browser sits in the command Windows runs, so
                    // Hopper's entry is written again.
                    if let Err(e) = register(&id) {
                        window.push_notification(Notification::error(format!("{e:#}")), cx);
                        return;
                    }
                    store.update(cx, |s, cx| s.update_settings(|st| st.open_links_browser = id, cx));
                }),
            ))));
            let done = hopper::browser::is_default();
            let steps = v_flex()
                .gap_0p5()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("1. Open Windows Settings › Default apps.")
                .child("2. Search for Hopper and open it.")
                .child("3. Press Set default.");
            rows.push(
                Row::new("open-links-windows")
                    .desc(if done {
                        "Windows sends clicked links to Hopper. To stop, turn this off or pick another browser in Default apps."
                    } else {
                        "Windows keeps your browser until you choose Hopper there yourself."
                    })
                    .control(if done {
                        widgets::pill("Done", theme.success, true)
                    } else {
                        widgets::pill("Not yet", theme.muted_foreground, true)
                    })
                    .below(
                        v_flex()
                            .gap_2()
                            .when(!done, |c| c.child(steps))
                            .child(
                                h_flex().child(
                                    Button::new("open-links-settings")
                                        .small()
                                        .outline()
                                        .label("Open Windows Settings")
                                        .on_click(cx.listener(|_, _, _, cx| {
                                            let app = hopper::browser::names().app;
                                            cx.open_url(&format!(
                                                "ms-settings:defaultapps?registeredAppUser={}",
                                                app.replace(' ', "%20")
                                            ));
                                        })),
                                ),
                            ),
                    ),
            );
        }
        Block::card(Some("Links from other apps"), rows)
    }

    fn set_open_links(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        let saved = self.store.read(cx).settings().open_links_browser.clone();
        if on {
            *BROWSERS.lock().unwrap_or_else(|e| e.into_inner()) = None;
            let Some(browser) = chosen(&saved, &browsers()) else {
                window.push_notification(Notification::error("Hopper didn't find a browser to pass other links to."), cx);
                return;
            };
            if let Err(e) = register(&browser) {
                window.push_notification(Notification::error(format!("{e:#}")), cx);
                return;
            }
            // Remember the browser from before Hopper, while Windows still
            // says which one it is.
            self.store.update(cx, |s, cx| {
                s.update_settings(
                    |st| {
                        st.open_links = true;
                        st.open_links_browser = browser;
                    },
                    cx,
                )
            });
        } else {
            let was_default = hopper::browser::is_default();
            if let Err(e) = hopper::browser::unregister() {
                window.push_notification(Notification::error(format!("{e:#}")), cx);
                return;
            }
            if was_default {
                window.push_notification(
                    Notification::info("Hopper no longer opens links. Windows asks which browser to use the next time you click one."),
                    cx,
                );
            }
            self.store.update(cx, |s, cx| s.update_settings(|st| st.open_links = false, cx));
        }
    }
}

fn register(browser: &str) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    hopper::browser::register(&exe, browser)
}
