//! Start / Stop / Restart for any listed server, from its row's right-click
//! menu in the server list (the web panel's list has the same). Asks first
//! exactly as the server page's buttons do (`PanelAction::confirm_text`).

use crate::store::Store;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::WindowExt as _;
use gpui_kit::*;
use hopper::panel::PanelAction;
use std::path::PathBuf;

/// The row's power items: Start for a stopped server, Restart and Stop for
/// a running one; none when no panel can reach it.
pub fn menu_items(store: &Entity<Store>, root: &PathBuf, cx: &App) -> Vec<(&'static str, gpui_kit::assets::IconName, PanelAction)> {
    use gpui_kit::assets::IconName;
    let s = store.read(cx);
    let Some(srv) = s.servers().iter().find(|x| &x.root == root) else { return Vec::new() };
    if crate::mcp_servers::panel_for(s, srv).is_none() {
        return Vec::new();
    }
    match s.panel_stats(root) {
        Some(p) if p.running || p.starting => {
            vec![("Restart", IconName::RotateCcw, PanelAction::Restart), ("Stop", IconName::Square, PanelAction::Stop)]
        }
        _ => vec![("Start", IconName::Play, PanelAction::Start)],
    }
}

/// Asks when the page's buttons would (players on, or not known), then
/// sends `action` to the server's panel.
pub fn request(store: Entity<Store>, root: PathBuf, action: PanelAction, window: &mut Window, cx: &mut App) {
    let s = store.read(cx);
    let Some(srv) = s.servers().iter().find(|x| x.root == root) else { return };
    let name = srv.name.clone();
    let ask = match s.panel_stats(&root) {
        Some(p) => action.confirm_text(&name, Some(p.online), &p.players),
        None => action.confirm_text(&name, None, &[]),
    };
    let Some(text) = ask else {
        run(store, root, action, window, cx);
        return;
    };
    window.open_alert_dialog(cx, move |alert, _, _| {
        let (store, root) = (store.clone(), root.clone());
        alert
            .title(format!("{} the server?", action.label()))
            .description(text.clone())
            .button_props(
                gpui_kit::component::dialog::DialogButtonProps::default()
                    .ok_text(action.label())
                    .ok_variant(gpui_kit::component::button::ButtonVariant::Primary)
                    .show_cancel(true),
            )
            .on_ok(move |_, window, cx| {
                run(store.clone(), root.clone(), action, window, cx);
                true
            })
    });
}

fn run(store: Entity<Store>, root: PathBuf, action: PanelAction, window: &mut Window, cx: &mut App) {
    let s = store.read(cx);
    let Some(srv) = s.servers().iter().find(|x| x.root == root).cloned() else { return };
    let Some(panel) = crate::mcp_servers::panel_for(s, &srv) else {
        window.push_notification(Notification::error(format!("Hopper can't reach {}'s panel.", srv.name)), cx);
        return;
    };
    crate::hero_drift::kick(1.6, 0.);
    let doing = match action {
        PanelAction::Start => "Starting",
        PanelAction::Stop => "Stopping",
        _ => "Restarting",
    };
    window.push_notification(Notification::info(format!("{doing} {}.", srv.name)), cx);
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let r = cx
            .background_spawn(async move {
                let id = crate::mcp_servers::panel_id(&*panel, &srv).map_err(anyhow::Error::msg)?;
                panel.act(&id, action)
            })
            .await;
        if let Err(e) = r {
            handle
                .update(cx, |_, window, cx| window.push_notification(Notification::error(format!("{e:#}")), cx))
                .ok();
        }
    })
    .detach();
}
