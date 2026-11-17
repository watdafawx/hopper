//! Mods in a pack that its paired server lacks. Adding a mod only ever
//! writes into the instance, so the instance page's "has other mods than
//! this pack" notice names them and offers to copy just those to the server
//! (see `join_help::pre_join_bar`). It used to be a toast after each
//! install too, which said the same thing as the notice beside it.

use super::{Msg, Store};
use hopper::model::DiffEntry;
use hopper::server::ServerInfo;
use std::path::{Path, PathBuf};

/// What an instance had before an install: the website-only pages waiting.
#[derive(Default)]
pub struct Before {
    pages: Vec<String>,
    /// The jars in the mods folder, to tell what the add brought in
    /// (`None`: not known, so nothing is asked about).
    jars: Option<std::collections::HashSet<String>>,
}

impl Before {
    pub fn at(root: &Path) -> Before {
        Before { pages: hopper::modpack::pending_urls_at(root), jars: jars_at(root) }
    }
}

/// The file names in the instance's mods folder.
pub fn jars_at(root: &Path) -> Option<std::collections::HashSet<String>> {
    let dir = std::fs::read_dir(root.join("mods")).ok()?;
    Some(dir.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
}

impl Msg {
    /// [`Msg::installed`] with the pages that were waiting before, for a mod
    /// the player added: in a group's pack, what it needs is asked about.
    pub fn added(root: PathBuf, before: &Before, by: Option<u64>) -> Msg {
        Msg::Added(Box::new(Msg::installed(root, &before.pages, by)), before.jars.clone())
    }
}

/// Mods to copy to a server: the instance, the server, and the mods it lacks.
#[derive(Clone)]
pub struct ServerOffer {
    pub inst: PathBuf,
    pub server: ServerInfo,
    pub rows: Vec<DiffEntry>,
}

impl Store {
    /// Copies just the offered mods to the server, as a task. Nothing is
    /// removed there; the copies go in the server's History like a sync's.
    pub fn add_to_server(&mut self, offer: &ServerOffer) -> Option<u64> {
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
