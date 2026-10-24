//! The Mods table's Side hover card: where a mod runs, how it stands on the
//! paired rig server, and the server actions for it (as in mod-sync).

use crate::widgets;
use crate::addons::{AddonsEvent, AddonsView};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::hover_card::HoverCard;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::model::{DiffEntry, DiffStatus};
use hopper::rigsync::{self, ModAct};
use std::time::Duration;

/// What the card shows for one row, captured when the row renders.
#[derive(Clone)]
pub struct SideCard {
    pub view: Entity<AddonsView>,
    pub name: String,
    pub env: String,
    pub project_id: u64,
    pub from_pack: bool,
    /// The paired server's name, when there is one.
    pub server: Option<String>,
    /// Whether the Server column (the comparison) is on.
    pub compared: bool,
    pub entry: Option<DiffEntry>,
    pub blacklisted: bool,
    pub anyway: bool,
}

/// The side icon and what it means.
pub fn side_of(env: &str) -> Option<(IconName, &'static str)> {
    match env {
        "client" => Some((IconName::Monitor, "Client only")),
        "server" => Some((IconName::Server, "Server only")),
        "both" => Some((IconName::ArrowLeftRight, "Client and server")),
        _ => None,
    }
}

/// `trigger` (the Side cell) opening the card on hover.
pub fn wrap(id: ElementId, trigger: impl IntoElement + 'static, card: SideCard) -> HoverCard {
    HoverCard::new(id)
        .anchor(Anchor::TopRight)
        .open_delay(Duration::from_millis(250))
        .close_delay(Duration::from_millis(200))
        .trigger(trigger)
        .content(move |_, window, cx| body(&card, window, cx))
}

fn body(c: &SideCard, _: &mut Window, cx: &mut App) -> AnyElement {
    let theme = cx.theme().clone();
    let note = |text: String| {
        div()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(text)
    };
    let (icon, side) = side_of(&c.env).unwrap_or((IconName::CircleQuestionMark, "Side not declared"));
    let origin = if c.from_pack { "From the pack" } else { "Added by you" };
    let mut card = v_flex()
        .w(rems(21.))
        .gap_2()
        .text_sm()
        .child(
            h_flex()
                .gap_2()
                .child(Icon::new(icon).small().text_color(theme.muted_foreground))
                .child(div().min_w_0().font_semibold().truncate().child(c.name.clone())),
        )
        .child(note(format!("{side} · {origin}")));

    let Some(server) = c.server.clone() else {
        return card.child(note("No paired rig server.".into())).into_any_element();
    };
    if !c.compared {
        let view = c.view.clone();
        return card
            .child(note(format!("Compare with {server} to see this mod there.")))
            .child(
                Button::new("side-compare")
                    .small()
                    .outline()
                    .icon(IconName::ArrowLeftRight)
                    .label("Compare with server")
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| this.show_server_column(cx))
                    }),
            )
            .into_any_element();
    }
    let Some(d) = c.entry.clone() else {
        return card
            .child(note(format!("Comparing with {server}…")))
            .into_any_element();
    };

    let (tag, status) = match d.status {
        DiffStatus::Same => (widgets::chip(cx.theme().success), "In sync"),
        DiffStatus::OnlyClient if d.skip_reason.is_some() => (widgets::chip(cx.theme().muted_foreground), "Kept off"),
        DiffStatus::OnlyClient => (widgets::chip(cx.theme().warning), "Not on the server"),
        DiffStatus::ClientNewer => (widgets::chip(cx.theme().info), "Older on the server"),
        DiffStatus::ServerNewer => (widgets::chip(cx.theme().warning), "Newer on the server"),
        s => (widgets::chip(cx.theme().muted_foreground), rigsync::status_label(s)),
    };
    let version = |m: &Option<hopper::model::ModInfo>| {
        m.as_ref().map_or("—".to_string(), |m| {
            if m.version.is_empty() {
                m.file_name.clone()
            } else {
                format!("v{}", m.version)
            }
        })
    };
    let line = |label: &str, value: String| {
        h_flex()
            .gap_2()
            .text_xs()
            .child(div().w(rems(5.5)).flex_none().text_color(theme.muted_foreground).child(label.to_string()))
            .child(div().min_w_0().truncate().child(value))
    };
    card = card
        .child(div().h_px().bg(theme.border))
        .child(
            h_flex()
                .gap_2()
                .child(tag.xsmall().child(status))
                .child(note(format!("on {server}"))),
        )
        .child(line("This instance", version(&d.client)))
        .child(line("Server", version(&d.server)))
        .child(note(rigsync::explain(&d)))
        .when_some(d.skip_reason.clone(), |c, r| {
            c.child(
                div()
                    .text_xs()
                    .text_color(theme.warning)
                    .child(format!("Kept off the server: {r}")),
            )
        });

    let mut actions = h_flex().flex_wrap().gap_1p5().pt_1();
    for act in rigsync::mod_acts(&d) {
        let (view, d) = (c.view.clone(), d.clone());
        let b = match act {
            ModAct::Push => Button::new("side-push")
                .primary()
                .icon(IconName::Upload)
                .label(match d.status {
                    DiffStatus::ClientNewer => "Update on server",
                    DiffStatus::ServerNewer => "Replace server's copy",
                    _ => "Copy to server",
                }),
            ModAct::RemoveFromServer => Button::new("side-remove")
                .danger()
                .outline()
                .icon(IconName::Trash)
                .label("Remove from server"),
        };
        actions = actions.child(b.small().on_click(move |_, window, cx| {
            view.update(cx, |this, cx| this.server_act(act, d.clone(), window, cx))
        }));
    }
    if let Some(key) = rigsync::blacklist_key(&d) {
        let (view, on) = (c.view.clone(), c.blacklisted);
        actions = actions.child(
            Button::new("side-never")
                .small()
                .ghost()
                .icon(if on { IconName::Eye } else { IconName::EyeOff })
                .label(if on { "Allow syncing" } else { "Never sync" })
                .tooltip(if on {
                    "Show this mod in Server sync again"
                } else {
                    "Hide this mod from Server sync and leave the server's copy alone"
                })
                .on_click(move |_, _, cx| {
                    view.update(cx, |this, cx| this.toggle_blacklist(key.clone(), cx))
                }),
        );
    }
    let wont = d.client.as_ref().is_some_and(|m| m.wont_sync.is_some());
    if (wont || c.anyway) && c.project_id != 0 {
        let (view, pid, on) = (c.view.clone(), c.project_id, c.anyway);
        actions = actions.child(
            Button::new("side-anyway")
                .small()
                .ghost()
                .label(if on { "Stop syncing anyway" } else { "Sync anyway" })
                .tooltip("Sync this mod even though it is marked client-side")
                .on_click(move |_, _, cx| {
                    view.update(cx, |this, cx| this.toggle_anyway(pid, cx))
                }),
        );
    }
    let view = c.view.clone();
    actions = actions.child(
        Button::new("side-open-sync")
            .small()
            .ghost()
            .icon(IconName::ArrowLeftRight)
            .label("Server sync")
            .tooltip("Compare the whole instance with the server")
            .on_click(move |_, _, cx| view.update(cx, |this, cx| this.open_rig_sync(cx))),
    );
    card.child(actions).into_any_element()
}

impl AddonsView {
    /// The card for one of the table's mods.
    pub(crate) fn side_card(&self, a: &hopper::instances::Addon, cx: &Context<Self>) -> SideCard {
        let store = self.store.read(cx);
        let settings = store.settings();
        let entry = self
            .marks
            .as_ref()
            .and_then(|(_, _, m)| m.get(&a.file_name.to_lowercase()))
            .cloned();
        SideCard {
            view: cx.entity(),
            name: a.name.clone(),
            env: a.env.clone(),
            project_id: a.project_id,
            from_pack: a.from_pack,
            server: self.root.as_ref().and_then(|r| store.server_of(r)).map(|s| s.name.clone()),
            compared: settings.mods_server_column,
            blacklisted: entry
                .as_ref()
                .and_then(rigsync::blacklist_key)
                .is_some_and(|k| settings.sync_blacklist.contains(&k)),
            anyway: a.project_id != 0 && settings.sync_anyway.contains(&a.project_id),
            entry,
        }
    }

    /// A card in a dialog, for scripted screenshots (`--dialog side`): the
    /// first mod the server lacks or has another build of.
    pub(crate) fn preview_side_card(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(inst) = self.root.as_ref().and_then(|r| self.store.read(cx).instance(r)).cloned() else {
            return;
        };
        let marks = self.marks.as_ref().map(|(_, _, m)| m);
        let pick = inst
            .addons
            .iter()
            .filter(|a| a.class_id == hopper::cf::CLASS_MODS)
            .find(|a| {
                marks
                    .and_then(|m| m.get(&a.file_name.to_lowercase()))
                    .is_some_and(|e| e.status == DiffStatus::ClientNewer)
            })
            .or_else(|| inst.addons.first());
        let Some(a) = pick else { return };
        let card = self.side_card(a, cx);
        window.open_dialog(cx, move |d, window, cx| d.child(body(&card, window, cx)));
    }

    /// Turns the Server column on, which compares with the paired server.
    pub(crate) fn show_server_column(&mut self, cx: &mut Context<Self>) {
        self.store
            .update(cx, |s, cx| s.update_settings(|st| st.mods_server_column = true, cx));
        self.marks = None;
        self.want_marks(cx);
        cx.notify();
    }

    /// Runs one server action for a mod, then compares again.
    pub(crate) fn server_act(
        &mut self,
        act: ModAct,
        d: DiffEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.root.clone() else { return };
        let store = self.store.read(cx);
        let (Some(server), Some(inst)) =
            (store.server_of(&root).cloned(), store.instance(&root).cloned())
        else {
            return;
        };
        let ops = rigsync::mod_ops(act, &d, &inst, &server);
        if ops.is_empty() {
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let results = cx
                .background_spawn(async move { rigsync::run(ops, &server.root, &inst.root) })
                .await;
            this.update_in(cx, |this, window, cx| {
                let n = match rigsync::summary(&results) {
                    Ok(done) => Notification::success(done),
                    Err(failed) => Notification::error(failed.join("\n")),
                };
                window.push_notification(n, cx);
                this.marks = None;
                this.want_marks(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn toggle_blacklist(&mut self, key: String, cx: &mut Context<Self>) {
        self.store.update(cx, |s, cx| {
            s.update_settings(
                |st| {
                    if !st.sync_blacklist.remove(&key) {
                        st.sync_blacklist.insert(key);
                    }
                },
                cx,
            )
        });
        cx.notify();
    }

    pub(crate) fn toggle_anyway(&mut self, project: u64, cx: &mut Context<Self>) {
        self.store.update(cx, |s, cx| {
            s.update_settings(
                |st| {
                    if !st.sync_anyway.remove(&project) {
                        st.sync_anyway.insert(project);
                    }
                },
                cx,
            )
        });
        // Syncing anyway changes the comparison.
        self.marks = None;
        self.want_marks(cx);
        cx.notify();
    }

    pub(crate) fn open_rig_sync(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        if let Some(s) = self.store.read(cx).server_of(&root) {
            cx.emit(AddonsEvent::Compare(s.root.clone()));
        }
    }
}
