//! Mods added to a pack that runs on servers. Adding a mod only ever writes
//! into the instance, so a mod the server needs stayed missing there until
//! the next full sync, without a word. After an install Hopper compares
//! each paired server and offers to copy just the new mods it lacks.

use super::{Msg, Store, StoreEvent};
use gpui_kit::*;
use hopper::model::DiffEntry;
use hopper::server::ServerInfo;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// What an instance had before an install: the website-only pages waiting
/// and the jars in its mods folder (lower case).
#[derive(Default)]
pub struct Before {
    pages: Vec<String>,
    jars: HashSet<String>,
}

impl Before {
    pub fn at(root: &Path) -> Before {
        Before {
            pages: hopper::modpack::pending_urls_at(root),
            jars: jars(root),
        }
    }
}

/// The jar files in an instance's mods folder, lower case.
fn jars(root: &Path) -> HashSet<String> {
    std::fs::read_dir(root.join("mods"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_lowercase())
        .filter(|n| n.ends_with(".jar"))
        .collect()
}

impl Msg {
    /// Like [`Msg::installed`], and when the install put new jars in the
    /// mods folder, the paired servers are checked for them.
    pub fn added(root: PathBuf, before: &Before, by: Option<u64>) -> Msg {
        let inner = Msg::installed(root.clone(), &before.pages, by);
        let new: HashSet<String> = jars(&root).difference(&before.jars).cloned().collect();
        if new.is_empty() { inner } else { Msg::Added(root, new, by, Box::new(inner)) }
    }
}

/// A server's offer: the instance, the server, and the new mods it lacks.
#[derive(Clone)]
pub struct ServerOffer {
    pub inst: PathBuf,
    pub server: ServerInfo,
    pub rows: Vec<DiffEntry>,
}

impl ServerOffer {
    /// "Hands-On Delight and 1 more aren't on dg".
    pub fn title(&self) -> String {
        let first = self.rows.first().map(|d| d.mod_name.clone()).unwrap_or_default();
        match self.rows.len() {
            1 => format!("{first} isn't on {}", self.server.name),
            2 => format!("{first} and 1 more aren't on {}", self.server.name),
            n => format!("{first} and {} more aren't on {}", n - 1, self.server.name),
        }
    }
}

impl Store {
    /// Compares each server paired with `root` off the UI thread; a server
    /// that lacks any of `added` (jar names) gets an offer.
    pub(super) fn check_added(&self, root: &Path, added: HashSet<String>, by: Option<u64>) {
        let Some(inst) = self.instance(root).cloned() else { return };
        // The mod the player picked leads the offer, not its dependencies.
        let picked: Option<String> = by
            .and_then(|p| inst.addons.iter().find(|a| a.project_id == p))
            .map(|a| a.file_name.to_lowercase());
        let servers: Vec<ServerInfo> = self.servers_of(root).into_iter().cloned().collect();
        if servers.is_empty() {
            return;
        }
        let anyway = self.settings.sync_anyway.clone();
        let hidden = self.settings.sync_blacklist.clone();
        // Mods an earlier offer named and the player hasn't added yet are
        // looked for again, so the new offer (which replaces it) has them.
        let servers: Vec<(ServerInfo, HashSet<String>)> = servers
            .into_iter()
            .map(|s| {
                let mut look = added.clone();
                if let Some(old) = self.server_offers.get(&(root.to_path_buf(), s.root.clone())) {
                    look.extend(old.iter().cloned());
                }
                (s, look)
            })
            .collect();
        let (tx, wake) = (self.tx.clone(), self.wake.clone());
        std::thread::spawn(move || {
            for (server, look) in servers {
                let rows = match hopper::rigsync::compare(&inst, &server, &anyway) {
                    Ok((diff, _)) => {
                        let mut rows = hopper::rigsync::to_offer(&diff, &look, &hidden);
                        rows.sort_by_key(|d| d.client.as_ref().map(|c| c.file_name.to_lowercase()) != picked);
                        rows
                    }
                    Err(e) => {
                        log::warn!("checking {} for new mods: {e}", server.name);
                        continue;
                    }
                };
                if !rows.is_empty() {
                    let _ = tx.send(Msg::ServerOffer(Box::new(ServerOffer { inst: inst.root.clone(), server, rows })));
                    wake();
                }
            }
        });
    }

    pub(super) fn on_server_offer(&mut self, offer: ServerOffer, cx: &mut Context<Self>) {
        // Still paired: the pairing may have changed while it compared.
        if self.servers_of(&offer.inst).iter().any(|s| s.root == offer.server.root) {
            let jars = offer.rows.iter().filter_map(|d| Some(d.client.as_ref()?.file_name.to_lowercase())).collect();
            self.server_offers.insert((offer.inst.clone(), offer.server.root.clone()), jars);
            cx.emit(StoreEvent::ServerOffer(Box::new(offer)));
        }
    }

    /// Copies just the offered mods to the server, as a task. Nothing is
    /// removed there; the copies go in the server's History like a sync's.
    pub fn add_to_server(&mut self, offer: &ServerOffer) -> Option<u64> {
        self.server_offers.remove(&(offer.inst.clone(), offer.server.root.clone()));
        let inst = self.instance(&offer.inst)?.clone();
        let server = offer.server.clone();
        let rows = offer.rows.clone();
        // Running: its pages offer a restart once this is done.
        let live = crate::rigsync::server_online(self, None, &server.root).then(|| crate::rigsync::server_started(self, None, &server.root));
        let title = format!("Adding {} to {}", and_names(&rows), server.name);
        Some(self.spawn_job_id(title, move |job| {
            let ops = hopper::rigsync::Ops { up: hopper::rigsync::push(&rows, &inst, &server), down: Vec::new() };
            let (results, carry) = hopper::rigsync::run_on_report(ops, &server, &inst.root);
            let copied = results.iter().filter(|r| r.ok).count();
            match hopper::rigsync::summary(&results) {
                Ok(done) => job.detail(done),
                Err(failed) => {
                    for f in &failed {
                        job.note(f.clone());
                    }
                    anyhow::bail!("{} of {} mods didn't copy", failed.len(), results.len());
                }
            }
            if let Some(note) = carry.note() {
                job.note(note);
            }
            Ok(match live {
                Some(started) if copied > 0 => Msg::SyncedLive(server.root.clone(), started),
                _ => Msg::ServersChanged,
            })
        }))
    }
}

/// "Hands-On Delight and Diegesis".
fn and_names(rows: &[DiffEntry]) -> String {
    super::and_list(&rows.iter().map(|d| d.mod_name.clone()).collect::<Vec<_>>())
}
