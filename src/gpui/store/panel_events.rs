//! Change events from each paired Hopper Server: a server created, removed
//! or renamed, its mods, files, players, backups, schedule or settings
//! changed (from the web panel, another PC, or on its disk). The open tab
//! loads again and the server list updates without waiting for a poll.
//!
//! One background thread per paired panel long-polls `GET /changes` (see
//! hopperd's `changes.rs`). A panel too old to have it, or one that's down,
//! is tried again later; the regular five-second poll still runs meanwhile.

use super::{Msg, Store, StoreEvent};
use gpui_kit::*;
use hopper::hopperd::{Client, PanelChange};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

/// How long one ask waits for a change (under the client's reply limit).
const WAIT: u64 = 15;

/// The panels being followed: id -> generation. A thread whose panel left,
/// or was paired again, sees its generation gone and ends.
static FOLLOWING: LazyLock<Mutex<HashMap<String, u64>>> = LazyLock::new(Default::default);

fn current(panel: &str, generation: u64) -> bool {
    FOLLOWING.lock().unwrap_or_else(|e| e.into_inner()).get(panel) == Some(&generation)
}

/// Everything changed: what a missed stretch of events comes back as.
fn everything() -> Vec<PanelChange> {
    vec![PanelChange { server: None, areas: vec!["*".into()] }]
}

impl Store {
    /// Follows every paired panel's changes (call again when the pairings
    /// change: panels no longer paired stop, new ones start).
    pub(super) fn follow_panels(&self) {
        let mut on = FOLLOWING.lock().unwrap_or_else(|e| e.into_inner());
        let wanted: Vec<(String, String)> = self
            .settings
            .hopper_panels_on()
            .map(|p| (p.id.clone(), format!("{}|{}", p.url, hopper::panel::hopperd_token_for(&p.id).unwrap_or_default())))
            .collect();
        on.retain(|id, _| wanted.iter().any(|(w, _)| w == id));
        for p in self.settings.hopper_panels_on().cloned().collect::<Vec<_>>() {
            let key = wanted.iter().find(|(id, _)| *id == p.id).map(|w| w.1.clone()).unwrap_or_default();
            // Same address and token: the running thread carries on.
            let generation = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                key.hash(&mut h);
                h.finish()
            };
            if on.get(&p.id) == Some(&generation) {
                continue;
            }
            let Some(client) = Client::for_panel(&p) else { continue };
            on.insert(p.id.clone(), generation);
            let (tx, wake, id) = (self.tx.clone(), self.wake.clone(), p.id.clone());
            let _ = std::thread::Builder::new()
                .name("panel-events".into())
                .spawn(move || follow(client, id, generation, tx, wake));
        }
    }

    /// A panel's changes arrived: list its servers again when the list or
    /// a server's settings changed, and tell the open views.
    pub(super) fn panel_changed(&mut self, panel: String, changes: Vec<PanelChange>, cx: &mut Context<Self>) {
        let relist = changes.iter().any(|c| c.touches("list") || c.touches("server"));
        if relist && let Some(p) = self.settings.hopper_panels_on().find(|p| p.id == panel).cloned() {
            if let Some(client) = Client::for_panel(&p) {
                let id = p.id.clone();
                self.list_node_heads(id.clone(), &p);
                self.spawn(move || Msg::PanelServers(id, client.server_infos().map_err(|e| format!("{e:#}"))));
            }
        }
        // Each change for the servers it's about (no server: all of them).
        let mut by_root: Vec<(PathBuf, Vec<String>)> = Vec::new();
        for c in &changes {
            for s in &self.servers {
                let Some(r) = &s.remote else { continue };
                if r.panel != panel || c.server.as_ref().is_some_and(|id| *id != r.id) {
                    continue;
                }
                match by_root.iter_mut().find(|(root, _)| *root == s.root) {
                    Some((_, a)) => a.extend(c.areas.iter().cloned()),
                    None => by_root.push((s.root.clone(), c.areas.clone())),
                }
            }
        }
        if by_root.iter().any(|(_, a)| a.iter().any(|x| x == "mods" || x == "*")) {
            // The Sync tab compares the paired pack again.
            self.server_rev += 1;
        }
        for (root, mut areas) in by_root {
            areas.sort();
            areas.dedup();
            if areas.iter().any(|a| a == "mods" || a == "*") {
                self.refetch_server_mods(&root);
            }
            cx.emit(StoreEvent::PanelChanged(root, areas));
        }
        cx.notify();
    }
}

/// One panel's long-poll, until its pairing changes.
fn follow(client: Client, panel: String, generation: u64, tx: Sender<Msg>, wake: hopper::waker::Waker) {
    let send = |changes: Vec<PanelChange>| {
        let _ = tx.send(Msg::PanelEvents(panel.clone(), changes));
        wake();
    };
    let mut at: Option<(String, u64)> = None;
    let mut wait = Duration::from_secs(5);
    let mut failed = false;
    while current(&panel, generation) {
        match client.changes(at.as_ref().map(|(b, r)| (b.as_str(), *r)), WAIT) {
            Ok(c) => {
                // Back after a failure, a panel restart or too long away:
                // what changed meanwhile wasn't heard.
                let restarted = at.as_ref().is_some_and(|(b, _)| *b != c.boot);
                if (failed || restarted || c.reset) && at.is_some() {
                    send(everything());
                } else if !c.changes.is_empty() {
                    send(c.changes);
                }
                at = Some((c.boot, c.rev));
                failed = false;
                wait = Duration::from_secs(5);
            }
            Err(e) => {
                log::debug!("panel {panel} changes: {e:#}");
                failed = true;
                std::thread::sleep(wait);
                // An older Hopper Server without change events, or one
                // that's off: ask less often.
                wait = (wait * 2).min(Duration::from_secs(120));
            }
        }
    }
}
