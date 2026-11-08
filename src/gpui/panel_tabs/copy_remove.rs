//! The end of a Hopper Server's Settings › General, as the panel's web UI
//! has it (CopyServer.svelte, RemoveServer.svelte): "Copy this server"
//! (Clone, Save as template) and "Remove this server" (Remove from panel,
//! which keeps the folder, and Delete server).

use super::settings::SettingsTab;
use super::{Target, bytes, confirm};
use crate::store::Store;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::{Value, json};

/// The two sections' rows. `alive`: the server runs, so both copying and
/// removing wait until it stops (the panel refuses them too).
pub fn rows(t: &Target, store: &Entity<Store>, alive: bool, cx: &mut Context<SettingsTab>) -> Vec<AnyElement> {
    let theme = cx.theme().clone();
    let name = server_name(t, store, cx);
    let stop_first = alive.then_some("Stop the server first");
    let button = |id: &'static str, icon: IconName, label: &'static str| {
        Button::new(id).outline().small().icon(icon).label(label).disabled(alive).when_some(stop_first, |b, tip| b.tooltip(tip))
    };
    let clone = {
        let (t, store, name) = (t.clone(), store.clone(), name.clone());
        button("settings-clone", IconName::Copy, "Clone…")
            .on_click(move |_, window, cx| open_clone(t.clone(), store.clone(), name.clone(), window, cx))
    };
    let template = {
        let (t, store, name) = (t.clone(), store.clone(), name.clone());
        button("settings-template", IconName::LayoutTemplate, "Save as template…")
            .on_click(move |_, window, cx| open_template(t.clone(), store.clone(), name.clone(), window, cx))
    };
    let remove = {
        let (t, store, name) = (t.clone(), store.clone(), name.clone());
        Button::new("settings-remove")
            .small()
            .outline()
            .icon(IconName::EyeOff)
            .label("Remove from panel")
            .disabled(alive)
            .on_click(move |_, window, cx| ask_remove(t.clone(), store.clone(), name.clone(), window, cx))
    };
    let delete = {
        let (t, store) = (t.clone(), store.clone());
        Button::new("settings-delete")
            .small()
            .danger()
            .icon(IconName::Trash)
            .label("Delete server…")
            .disabled(alive)
            .on_click(move |_, window, cx| {
                let root = store.read(cx).servers().iter().find(|s| s.remote.as_ref() == Some(&t.remote)).map(|s| s.root.clone());
                if let Some(root) = root {
                    crate::server_delete::open(store.clone(), root, false, window, cx);
                }
            })
    };
    vec![
        SettingsTab::heading("Copy this server", cx).into_any_element(),
        SettingsTab::row(
            SettingsTab::label("Clone"),
            Some("A copy in the servers folder with its own port. The address and schedule aren't copied.".into()),
            None,
            clone.into_any_element(),
            false,
            cx,
        )
        .into_any_element(),
        SettingsTab::row(
            SettingsTab::label("Template"),
            Some("Mods, configs and start files, no world. New server › Template starts from it.".into()),
            None,
            template.into_any_element(),
            false,
            cx,
        )
        .into_any_element(),
        SettingsTab::heading("Remove this server", cx).into_any_element(),
        h_flex()
            .flex_wrap()
            .gap_2()
            .items_center()
            .p_3()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.danger.opacity(0.35))
            .bg(theme.danger.opacity(0.04))
            .child(div().flex_1().min_w(rems(14.)).text_sm().text_color(if alive { theme.warning } else { theme.muted_foreground }).child(
                if alive {
                    "Stop the server first."
                } else {
                    "Remove from panel keeps the folder. Delete moves it to the panel's deleted servers for 30 days."
                },
            ))
            .child(remove)
            .child(delete)
            .into_any_element(),
    ]
}

fn server_name(t: &Target, store: &Entity<Store>, cx: &App) -> String {
    store
        .read(cx)
        .servers()
        .iter()
        .find(|s| s.remote.as_ref() == Some(&t.remote))
        .map_or_else(|| t.remote.id.clone(), |s| s.name.clone())
}

/// Asks the panel who this PC is there first: only its owner or a manager
/// may take servers off it, and a paired PC is usually a device. Then
/// either asks to remove, or says where to do it (as Delete server does).
fn ask_remove(t: Target, store: Entity<Store>, name: String, window: &mut Window, cx: &mut App) {
    let handle = window.window_handle();
    let client = t.client.clone();
    let panel = store
        .read(cx)
        .settings()
        .hopper_panels
        .iter()
        .find(|p| p.id == t.remote.panel)
        .map_or_else(|| hopper::hopperd::host_name(&t.remote.base), |p| p.name.clone());
    cx.spawn(async move |cx| {
        let role = cx.background_spawn(async move { client.get("/auth/me").ok() }).await;
        // Unknown (an old panel): offer it; the panel still checks.
        let may = role.is_none_or(|v| hopper::hopperd::may_manage(&v));
        handle
            .update(cx, |_, window, cx| {
                if may {
                    confirm_remove(t, store, name, window, cx);
                } else {
                    let url = format!("{}/#/server/{}/settings/general", t.remote.base, hopper::hopperd::enc(&t.remote.id));
                    confirm(
                        window,
                        cx,
                        format!("Remove {name} in the panel"),
                        hopper::hopperd::cant_manage_text(&panel, "remove"),
                        "Open in browser",
                        false,
                        move |_, cx| cx.open_url(&url),
                        None,
                    );
                }
            })
            .ok();
    })
    .detach();
}

fn confirm_remove(t: Target, store: Entity<Store>, name: String, window: &mut Window, cx: &mut App) {
    let body = "The folder stays as it is. Add it again any time with New server › Folder in the panel.".to_string();
    confirm(
        window,
        cx,
        format!("Remove {name} from the panel?"),
        body,
        "Remove from panel",
        true,
        move |window, cx| {
            let (t, store, name) = (t.clone(), store.clone(), name.clone());
            let handle = window.window_handle();
            let client = t.client.clone();
            let path = t.sp("/remove");
            cx.spawn(async move |cx| {
                let r = cx.background_spawn(async move { client.post(&path, json!({ "mode": "remove" })) }).await;
                handle
                    .update(cx, |_, window, cx| {
                        let n = match r {
                            Ok(_) => {
                                store.update(cx, |s, _| s.rediscover_servers());
                                Notification::success(format!("{name} is off the panel. Its folder is untouched."))
                            }
                            Err(e) => Notification::error(format!("{e:#}")),
                        };
                        window.push_notification(n, cx);
                    })
                    .ok();
            })
            .detach();
        },
        None,
    );
}

// ---- Clone -------------------------------------------------------------------

struct CloneForm {
    name: Entity<InputState>,
    port: Entity<InputState>,
    address: Entity<InputState>,
    with_world: bool,
    /// `GET .../clone-info`: sizes, a free port, the name to suggest, the
    /// panel's domain and servers folder. None while it loads.
    info: Option<Result<Value, String>>,
}

impl CloneForm {
    fn domain(&self) -> Option<String> {
        self.info.as_ref()?.as_ref().ok()?["domain"].as_str().filter(|d| !d.is_empty()).map(str::to_string)
    }

    fn ready(&self, cx: &App) -> bool {
        matches!(self.info, Some(Ok(_))) && !self.name.read(cx).value().trim().is_empty()
    }
}

/// "Survival copy" → "survival-copy", as the web form suggests the address.
fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

impl Render for CloneForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let info = match &self.info {
            None => return v_flex().child(crate::widgets::caption("Measuring…", cx)).into_any_element(),
            Some(Err(e)) => return v_flex().text_sm().text_color(theme.danger).child(e.clone()).into_any_element(),
            Some(Ok(v)) => v.clone(),
        };
        let world = info["sizes"]["world"].as_u64().unwrap_or(0);
        let total = info["sizes"]["total"].as_u64().unwrap_or(0);
        let size = total.saturating_sub(if self.with_world { 0 } else { world });
        let domain = self.domain();
        let lbl = |s: &'static str| div().text_sm().font_medium().child(s);
        v_flex()
            .gap_3()
            .child(v_flex().gap_1().child(lbl("Name")).child(Input::new(&self.name).small()))
            .child(
                v_flex().gap_1().child(lbl("World")).child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(
                            crate::widgets::Segmented::new("clone-world")
                                .selected_index(usize::from(!self.with_world))
                                .child(crate::widgets::SegItem::new().label("With world"))
                                .child(crate::widgets::SegItem::new().label("Fresh world"))
                                .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                    this.with_world = *ix == 0;
                                    cx.notify();
                                })),
                        )
                        .child(crate::widgets::caption(format!("world: {}", bytes(world)), cx).text_xs()),
                ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(v_flex().w(rems(8.)).gap_1().child(lbl("Port")).child(Input::new(&self.port).small()))
                    .when_some(domain, |r, d| {
                        r.child(
                            v_flex().flex_1().gap_1().child(lbl("Address players join")).child(
                                Input::new(&self.address).small().suffix(crate::widgets::caption(format!(".{d}"), cx).text_xs()),
                            ),
                        )
                    }),
            )
            .child(div().text_xs().text_color(theme.muted_foreground).child(format!(
                "Copies {} to {}. Not copied: backups, logs, crash reports, the schedule, the address. Players, ops and the whitelist are copied.",
                bytes(size),
                info["servers_root"].as_str().unwrap_or("the servers folder")
            )))
            .into_any_element()
    }
}

fn open_clone(t: Target, store: Entity<Store>, name: String, window: &mut Window, cx: &mut App) {
    let form = cx.new(|cx| {
        let f = CloneForm {
            name: cx.new(|cx| InputState::new(window, cx)),
            port: cx.new(|cx| InputState::new(window, cx).placeholder("Auto")),
            address: cx.new(|cx| InputState::new(window, cx).placeholder("optional")),
            with_world: true,
            info: None,
        };
        // Changing the name re-suggests the address, as on the web.
        let (n, a) = (f.name.clone(), f.address.clone());
        cx.subscribe_in(&n, window, move |this: &mut CloneForm, st, e: &gpui_kit::component::input::InputEvent, window, cx| {
            if matches!(e, gpui_kit::component::input::InputEvent::Change) && this.domain().is_some() {
                let v = slug(&st.read(cx).value());
                a.update(cx, |s, cx| s.set_value(v, window, cx));
            }
            cx.notify();
        })
        .detach();
        f
    });
    {
        let f = form.clone();
        let client = t.client.clone();
        let path = t.sp("/clone-info");
        let handle = window.window_handle();
        cx.spawn(async move |cx| {
            let r = cx.background_spawn(async move { client.get(&path).map_err(|e| format!("{e:#}")) }).await;
            handle
                .update(cx, |_, window, cx| {
                    f.update(cx, |f, cx| {
                        if let Ok(v) = &r {
                            let n = v["name"].as_str().unwrap_or_default().to_string();
                            let port = v["port"].as_u64().map(|p| p.to_string()).unwrap_or_default();
                            f.name.update(cx, |s, cx| s.set_value(n.clone(), window, cx));
                            f.port.update(cx, |s, cx| s.set_value(port, window, cx));
                            if v["domain"].as_str().is_some_and(|d| !d.is_empty()) {
                                f.address.update(cx, |s, cx| s.set_value(slug(&n), window, cx));
                            }
                        }
                        f.info = Some(r);
                        cx.notify();
                    });
                })
                .ok();
        })
        .detach();
    }
    window.open_dialog(cx, move |dialog, window, cx| {
        let ready = form.read(cx).ready(cx);
        let (f, t, store) = (form.clone(), t.clone(), store.clone());
        dialog
            .title(format!("Clone {name}"))
            .w(rems(32.).to_pixels(window.rem_size()))
            .child(form.clone())
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("dialog-cancel").outline().label("Cancel")))
                    .child(DialogAction::new().child(
                        Button::new("dialog-ok").primary().icon(IconName::Copy).label("Clone").disabled(!ready),
                    )),
            )
            .on_ok(move |_, _, cx| start_clone(&f, &t, &store, cx))
    });
}

fn start_clone(form: &Entity<CloneForm>, t: &Target, store: &Entity<Store>, cx: &mut App) -> bool {
    let f = form.read(cx);
    if !f.ready(cx) {
        return false;
    }
    let name = f.name.read(cx).value().trim().to_string();
    let port = f.port.read(cx).value().trim().parse::<u16>().ok();
    let addr = f.address.read(cx).value().trim().to_string();
    let address = match f.domain() {
        Some(d) if !addr.is_empty() && !addr.contains('.') => format!("{addr}.{d}"),
        _ => addr,
    };
    let body = json!({ "name": name, "with_world": f.with_world, "port": port, "address": address });
    run_job(t, store, format!("Cloning into {name}"), "/clone", body, format!("Made {name}."), cx);
    true
}

// ---- Template ----------------------------------------------------------------

fn open_template(t: Target, store: Entity<Store>, name: String, window: &mut Window, cx: &mut App) {
    let input = cx.new(|cx| {
        let mut s = InputState::new(window, cx).placeholder("Template name");
        s.set_value(name.clone(), window, cx);
        s
    });
    window.open_dialog(cx, move |dialog, window, cx| {
        let theme = cx.theme().clone();
        let (input2, t, store) = (input.clone(), t.clone(), store.clone());
        dialog
            .title(format!("Save {name} as a template"))
            .w(rems(30.).to_pixels(window.rem_size()))
            .child(
                v_flex()
                    .gap_2()
                    .child(div().text_sm().text_color(theme.muted_foreground).child(
                        "Keeps its mods, configs and start files, without the world, players or backups. New server › Template starts a server from it.",
                    ))
                    .child(Input::new(&input).small()),
            )
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("dialog-cancel").outline().label("Cancel")))
                    .child(DialogAction::new().child(Button::new("dialog-ok").primary().label("Save template"))),
            )
            .on_ok(move |_, _, cx| {
                let n = input2.read(cx).value().trim().to_string();
                if n.is_empty() {
                    return false;
                }
                run_job(&t, &store, format!("Saving template {n}"), "/template", json!({ "name": n }), format!("Saved template {n}. New server › Template uses it."), cx);
                true
            })
    });
}

/// A panel job for this server, followed in Tasks; the list is read again
/// when it ends.
fn run_job(t: &Target, store: &Entity<Store>, title: String, rest: &str, body: Value, done: String, cx: &mut App) {
    let job = store.read(cx).start_job(title);
    let client = t.client.clone();
    let path = t.sp(rest);
    let store = store.clone();
    cx.spawn(async move |cx| {
        let r = cx
            .background_spawn(async move {
                let r = client.run_job(&path, body, &job);
                if r.is_ok() {
                    job.note(done);
                }
                job.finish(&r);
                r
            })
            .await;
        if r.is_ok() {
            store.update(cx, |s, _| s.rediscover_servers());
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    #[test]
    fn slug_matches_the_web_form() {
        assert_eq!(super::slug("Survival copy"), "survival-copy");
        assert_eq!(super::slug("ATM10 -- copy!"), "atm10-copy");
    }
}
