//! The tabs of a server on a Hopper Server (hopperd) that only a panel
//! has: Files, Mods, Players, Backups, Schedule and Settings. Each talks to
//! the panel's API (see crates/hopperd/README.md, "API (v1)") and looks like
//! the same tab in the panel's web UI.
//!
//! Every tab is a view with `new(store, window, cx)` and
//! `show(target, window, cx)`; the Servers page calls `show` when its server
//! or tab changes.

pub mod backups;
pub mod files;
pub mod mods;
pub mod players;
pub mod schedule;
pub mod settings;

use crate::store::Store;
use gpui_kit::*;
use hopper::hopperd::{Client, Remote};
use serde_json::Value;
use std::sync::Arc;

/// A server on a Hopper Server, and a client for its panel.
#[derive(Clone)]
pub struct Target {
    pub remote: Remote,
    pub client: Arc<Client>,
}

impl PartialEq for Target {
    fn eq(&self, o: &Self) -> bool {
        self.remote == o.remote
    }
}

impl Target {
    pub fn new(remote: Remote) -> Result<Self, String> {
        let client = Client::for_remote(&remote).map_err(|e| format!("{e:#}"))?;
        Ok(Target { remote, client: Arc::new(client) })
    }

    /// The server's id on its panel.
    pub fn id(&self) -> &str {
        &self.remote.id
    }

    /// `/servers/{id}{rest}`.
    pub fn sp(&self, rest: &str) -> String {
        Client::server_path(&self.remote.id, rest)
    }

    /// `/fs/{id}{rest}`.
    pub fn fp(&self, rest: &str) -> String {
        Client::fs_path(&self.remote.id, rest)
    }
}

/// `s` encoded for a query value.
pub fn q(s: &str) -> String {
    hopper::hopperd::enc(s)
}

/// Runs `work` off the UI thread with the target's client, then `done` on
/// the view with its result (errors as text, ready to show).
pub fn call<V: 'static, T: Send + 'static>(
    cx: &mut Context<V>,
    t: &Target,
    work: impl FnOnce(&Client) -> anyhow::Result<T> + Send + 'static,
    done: impl FnOnce(&mut V, Result<T, String>, &mut Context<V>) + 'static,
) {
    let client = t.client.clone();
    cx.spawn(async move |this, cx| {
        let r = cx.background_spawn(async move { work(&client).map_err(|e| format!("{e:#}")) }).await;
        this.update(cx, |v, cx| {
            done(v, r, cx);
            cx.notify();
        })
        .ok();
    })
    .detach();
}

/// [`call`] for work whose result needs the window (inputs, editors and
/// dialogs are made with one).
pub fn call_in<V: 'static, T: Send + 'static>(
    window: &mut Window,
    cx: &mut Context<V>,
    t: &Target,
    work: impl FnOnce(&Client) -> anyhow::Result<T> + Send + 'static,
    done: impl FnOnce(&mut V, Result<T, String>, &mut Window, &mut Context<V>) + 'static,
) {
    let client = t.client.clone();
    cx.spawn_in(window, async move |this, cx| {
        let r = cx.background_spawn(async move { work(&client).map_err(|e| format!("{e:#}")) }).await;
        this.update_in(cx, |v, window, cx| {
            done(v, r, window, cx);
            cx.notify();
        })
        .ok();
    })
    .detach();
}

/// A yes/no question before something that can't be undone: `ok` runs on
/// yes, `cancel` (if any) on no. `danger` makes yes red.
#[allow(clippy::too_many_arguments)]
pub fn confirm(
    window: &mut Window,
    cx: &mut App,
    title: String,
    body: String,
    ok_label: &'static str,
    danger: bool,
    ok: impl Fn(&mut Window, &mut App) + 'static,
    cancel: Option<std::rc::Rc<dyn Fn(&mut Window, &mut App)>>,
) {
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::component::button::ButtonVariant;
    use gpui_kit::prelude::FluentBuilder as _;
    let ok: std::rc::Rc<dyn Fn(&mut Window, &mut App)> = std::rc::Rc::new(ok);
    window.open_alert_dialog(cx, move |alert, _, _| {
        let (ok, cancel) = (ok.clone(), cancel.clone());
        alert
            .title(title.clone())
            .description(body.clone())
            .confirm()
            .ok_text(ok_label)
            .when(danger, |a| a.ok_variant(ButtonVariant::Danger))
            .on_ok(move |_, w, cx| {
                ok(w, cx);
                true
            })
            .when_some(cancel, |a, c| {
                a.on_cancel(move |_, w, cx| {
                    c(w, cx);
                    true
                })
            })
    });
}

/// "12 KB", "3.4 GB".
pub fn bytes(n: u64) -> String {
    hopper::instance_ops::human_bytes(n)
}

/// A GET, its JSON to `done`.
pub fn get<V: 'static>(
    cx: &mut Context<V>,
    t: &Target,
    path: String,
    done: impl FnOnce(&mut V, Result<Value, String>, &mut Context<V>) + 'static,
) {
    call(cx, t, move |c| c.get(&path), done);
}

/// Starts a panel job with a POST and follows it as a job in Hopper's
/// Tasks (cancelling it there cancels it on the panel); `done` gets its
/// result.
pub fn job<V: 'static>(
    cx: &mut Context<V>,
    store: &Entity<Store>,
    t: &Target,
    title: impl Into<String>,
    path: String,
    body: Value,
    done: impl FnOnce(&mut V, Result<Value, String>, &mut Context<V>) + 'static,
) {
    let job = store.read(cx).start_job(title);
    call(
        cx,
        t,
        move |c| {
            let r = c.run_job(&path, body, &job);
            job.finish(&r);
            r
        },
        done,
    );
}

/// What a tab shows while its first answer is on the way, or when it failed.
pub enum Load<T> {
    Waiting,
    Failed(String),
    Ready(T),
}

impl<T> Load<T> {
    pub fn ready(&self) -> Option<&T> {
        match self {
            Load::Ready(t) => Some(t),
            _ => None,
        }
    }

    pub fn from(r: Result<T, String>) -> Self {
        match r {
            Ok(t) => Load::Ready(t),
            Err(e) => Load::Failed(e),
        }
    }
}

/// The waiting / failed states as an element; None when ready.
pub fn load_state<T>(l: &Load<T>, cx: &App) -> Option<AnyElement> {
    use gpui_kit::assets::IconName;
    match l {
        Load::Waiting => Some(crate::widgets::skeleton_rows(6, rems(2.25), cx).into_any_element()),
        Load::Failed(e) => Some(
            crate::widgets::empty_state(IconName::TriangleAlert, "The panel didn't answer", Some(e.clone().into()), cx)
                .into_any_element(),
        ),
        Load::Ready(_) => None,
    }
}
