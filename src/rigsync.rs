//! Comparing a pack with the rig server that runs it, independent of any UI:
//! which instance a server runs, the comparison and its filters, and the
//! file operations a sync performs.

use crate::instances::Instance;
use crate::model::{DiffAction, DiffEntry, DiffStatus, ModInfo, SyncOp, SyncResult};
use crate::serverpack::Annotation;
use crate::server::ServerInfo;
use std::collections::{HashMap, HashSet};
use std::path::Path;

// ---- pairing -----------------------------------------------------------

fn words(s: &str) -> HashSet<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !matches!(*w, "the" | "of" | "and" | "a"))
        .map(str::to_lowercase)
        .collect()
}

/// Best name match with at least one shared word, if it is unambiguous.
pub fn guess_pairing<'a>(server: &str, instances: &'a [Instance]) -> Option<&'a Instance> {
    let sw = words(server);
    let mut scored: Vec<(usize, &Instance)> = instances
        .iter()
        .map(|i| (words(&i.name).intersection(&sw).count(), i))
        .filter(|(n, _)| *n > 0)
        .collect();
    scored.sort_by_key(|s| std::cmp::Reverse(s.0));
    match scored.as_slice() {
        [(a, i), (b, _), ..] if a > b => Some(i),
        [(_, i)] => Some(i),
        _ => None,
    }
}

/// The pack a server runs: the saved pairing (server root → instance
/// root), else the instance whose name shares the most words with it.
pub fn paired<'a>(
    pairings: &HashMap<String, String>,
    server: &ServerInfo,
    instances: &'a [Instance],
) -> Option<&'a Instance> {
    if let Some(inst) = pairings.get(server.root.to_string_lossy().as_ref()) {
        return instances.iter().find(|i| i.root.to_string_lossy() == *inst);
    }
    guess_pairing(&server.name, instances)
}

// ---- comparison --------------------------------------------------------

/// Scans both mods folders and compares them, marking what shouldn't sync.
pub fn compare(
    inst: &Instance,
    server: &ServerInfo,
    sync_anyway: &HashSet<u64>,
) -> Result<(Vec<DiffEntry>, Annotation), String> {
    let mut client =
        crate::scan::scan_folder(&inst.mods_dir()).map_err(|e| format!("client: {e:#}"))?;
    let theirs =
        crate::scan::scan_folder(&server.mods_dir()).map_err(|e| format!("server: {e:#}"))?;
    let note = crate::serverpack::annotate(&mut client, inst, sync_anyway);
    Ok((crate::diff::compute_diff(&client, &theirs), note))
}

/// What one row's chip does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowAct {
    Push,
    Pull,
    Remove,
}

pub fn row_action(s: DiffStatus) -> Option<RowAct> {
    match s {
        DiffStatus::OnlyClient | DiffStatus::ClientNewer | DiffStatus::ServerNewer => {
            Some(RowAct::Push)
        }
        DiffStatus::OnlyServer => Some(RowAct::Pull),
        DiffStatus::ServerStale => Some(RowAct::Remove),
        DiffStatus::Same => None,
    }
}

/// A row's identity across rescans.
pub fn row_key(d: &DiffEntry) -> String {
    d.client
        .as_ref()
        .or(d.server.as_ref())
        .map(|m| m.file_name.to_lowercase())
        .unwrap_or_else(|| d.mod_name.clone())
}

pub fn status_label(s: DiffStatus) -> &'static str {
    match s {
        DiffStatus::OnlyClient => "Not on server",
        DiffStatus::OnlyServer => "Not in pack",
        DiffStatus::ServerStale => "Old on server",
        DiffStatus::ClientNewer => "Update",
        DiffStatus::ServerNewer => "Downgrade",
        DiffStatus::Same => "In sync",
    }
}

/// What a row's state means and what its chip does.
pub fn explain(d: &DiffEntry) -> String {
    let v = |m: &Option<ModInfo>| m.as_ref().map_or("?".to_string(), |m| m.version.clone());
    match d.status {
        DiffStatus::OnlyClient => "In the pack but not on the server. Click to copy it to the server.".into(),
        DiffStatus::OnlyServer => "On the server but not in the pack. Click to copy it down to the pack, or select it and Remove.".into(),
        DiffStatus::ServerStale => "An older copy left on the server; the client has this mod under a newer file. Click the chip to remove the leftover.".into(),
        DiffStatus::ClientNewer | DiffStatus::ServerNewer => format!(
            "Client is v{}, server is v{}. Pushing copies the client's file over and removes the server's old one.",
            v(&d.client),
            v(&d.server)
        ),
        DiffStatus::Same => "Client and server already match.".into(),
    }
}

/// A pack mod that stays off the server (client-only, or flagged).
pub fn is_wont(d: &DiffEntry) -> bool {
    d.client.as_ref().is_some_and(|c| c.wont_sync.is_some())
        || (d.status == DiffStatus::OnlyClient && d.skip_reason.is_some())
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum StatusFilter {
    #[default]
    All,
    Client,
    Updates,
    Server,
    InSync,
}

impl StatusFilter {
    pub const ALL: [StatusFilter; 5] = [
        StatusFilter::All,
        StatusFilter::Client,
        StatusFilter::Updates,
        StatusFilter::Server,
        StatusFilter::InSync,
    ];

    pub fn label(self) -> &'static str {
        match self {
            StatusFilter::All => "All",
            StatusFilter::Client => "Not on server",
            StatusFilter::Updates => "Updates",
            StatusFilter::Server => "Not in pack",
            StatusFilter::InSync => "In sync",
        }
    }

    pub fn matches(self, s: DiffStatus) -> bool {
        match self {
            StatusFilter::All => true,
            StatusFilter::Client => s == DiffStatus::OnlyClient,
            StatusFilter::Updates => matches!(s, DiffStatus::ClientNewer | DiffStatus::ServerNewer),
            StatusFilter::Server => matches!(s, DiffStatus::OnlyServer | DiffStatus::ServerStale),
            StatusFilter::InSync => s == DiffStatus::Same,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum EnvFilter {
    #[default]
    Any,
    Client,
    Server,
    Both,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum SourceFilter {
    #[default]
    Any,
    Pack,
    Added,
}

/// What the comparison shows.
#[derive(Clone, Default, Debug)]
pub struct Filters {
    pub text: String,
    pub status: StatusFilter,
    pub env: EnvFilter,
    pub source: SourceFilter,
    pub wont_only: bool,
    pub hide_wont: bool,
}

impl Filters {
    pub fn passes(&self, d: &DiffEntry, blacklist: &HashSet<String>) -> bool {
        let m = d.client.as_ref().or(d.server.as_ref());
        if m.is_some_and(|m| {
            blacklist.contains(&m.mod_id.to_lowercase())
                || blacklist.contains(&m.file_name.to_lowercase())
        }) {
            return false;
        }
        let env = m.map_or("", |m| m.side.as_str());
        let ok_env = match self.env {
            EnvFilter::Any => true,
            EnvFilter::Client => env == "client" || d.skip_reason.is_some(),
            EnvFilter::Server => env == "server",
            EnvFilter::Both => env == "both" || env.is_empty(),
        };
        let from_pack = d.client.as_ref().is_some_and(|c| c.from_pack);
        let ok_source = match self.source {
            SourceFilter::Any => true,
            SourceFilter::Pack => from_pack,
            SourceFilter::Added => d.client.is_some() && !from_pack,
        };
        let wont = is_wont(d);
        let ok_wont = if self.wont_only {
            wont
        } else {
            !(self.hide_wont && wont)
        };
        let t = self.text.to_lowercase();
        let ok_text = t.is_empty()
            || d.mod_name.to_lowercase().contains(&t)
            || m.is_some_and(|m| {
                m.file_name.to_lowercase().contains(&t) || m.mod_id.to_lowercase().contains(&t)
            });
        self.status.matches(d.status) && ok_env && ok_source && ok_wont && ok_text
    }
}

/// The key a blacklist entry uses for a row's mod.
pub fn blacklist_key(d: &DiffEntry) -> Option<String> {
    let m = d.client.as_ref().or(d.server.as_ref())?;
    Some(if m.mod_id.is_empty() {
        m.file_name.to_lowercase()
    } else {
        m.mod_id.to_lowercase()
    })
}

// ---- operations --------------------------------------------------------

/// File operations for rows: (to the server, to the client).
#[derive(Default, Debug)]
pub struct Ops {
    pub up: Vec<SyncOp>,
    pub down: Vec<SyncOp>,
}

impl Ops {
    pub fn is_empty(&self) -> bool {
        self.up.is_empty() && self.down.is_empty()
    }
}

/// Copies client files over (replacing the server's old builds).
pub fn push(rows: &[DiffEntry], inst: &Instance, server: &ServerInfo) -> Vec<SyncOp> {
    let rows: Vec<DiffEntry> = rows
        .iter()
        .filter(|d| d.client.is_some() && row_action(d.status) == Some(RowAct::Push))
        .cloned()
        .map(|mut d| {
            d.action = DiffAction::CopyToServer;
            d
        })
        .collect();
    crate::sync::plan(&rows, &inst.mods_dir(), &server.mods_dir())
}

/// Copies server-only files down to the client.
pub fn pull(rows: &[DiffEntry], inst: &Instance, server: &ServerInfo) -> Vec<SyncOp> {
    rows.iter()
        .filter(|d| d.status == DiffStatus::OnlyServer)
        .filter_map(|d| d.server.as_ref())
        .map(|m| SyncOp::Copy {
            src: server.mods_dir().join(&m.file_name),
            dst: inst.mods_dir().join(&m.file_name),
        })
        .collect()
}

/// Deletes server-only and leftover files from the server.
pub fn remove(rows: &[DiffEntry], server: &ServerInfo) -> Vec<SyncOp> {
    rows.iter()
        .filter(|d| matches!(d.status, DiffStatus::OnlyServer | DiffStatus::ServerStale))
        .filter_map(|d| d.server.as_ref())
        .map(|m| SyncOp::Delete {
            path: server.mods_dir().join(&m.file_name),
        })
        .collect()
}

/// What one row's chip does, as operations.
pub fn chip_ops(act: RowAct, d: &DiffEntry, inst: &Instance, server: &ServerInfo) -> Ops {
    let one = std::slice::from_ref(d);
    match act {
        RowAct::Push => Ops {
            up: push(one, inst, server),
            down: Vec::new(),
        },
        RowAct::Pull => Ops {
            up: Vec::new(),
            down: pull(one, inst, server),
        },
        RowAct::Remove => Ops {
            up: remove(one, server),
            down: Vec::new(),
        },
    }
}

/// What can be done to one of the instance's mods on its paired server.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModAct {
    /// Copy the client's file up (new on the server, or replacing its build).
    Push,
    /// Delete the server's copy.
    RemoveFromServer,
}

/// The server actions that make sense for a mod's row, most useful first.
pub fn mod_acts(d: &DiffEntry) -> Vec<ModAct> {
    let mut acts = Vec::new();
    if d.client.is_some()
        && matches!(
            d.status,
            DiffStatus::OnlyClient | DiffStatus::ClientNewer | DiffStatus::ServerNewer
        )
    {
        acts.push(ModAct::Push);
    }
    if d.server.is_some() {
        acts.push(ModAct::RemoveFromServer);
    }
    acts
}

/// A mod action as operations.
pub fn mod_ops(act: ModAct, d: &DiffEntry, inst: &Instance, server: &ServerInfo) -> Ops {
    match act {
        ModAct::Push => chip_ops(RowAct::Push, d, inst, server),
        ModAct::RemoveFromServer => Ops {
            up: d
                .server
                .iter()
                .map(|m| SyncOp::Delete {
                    path: server.mods_dir().join(&m.file_name),
                })
                .collect(),
            down: Vec::new(),
        },
    }
}

/// Runs the operations and records them in the server's history (pulls
/// back up into the client, so their backups go there).
pub fn run(ops: Ops, server_root: &Path, inst_root: &Path) -> Vec<SyncResult> {
    let mut results = crate::sync::execute(ops.up, server_root);
    crate::history::record(server_root, "server", server_root, &results);
    let pulled = crate::sync::execute(ops.down, inst_root);
    crate::history::record(server_root, "client", inst_root, &pulled);
    results.extend(pulled);
    results
}

/// "Done: 12 files; 3 kept in .hopper-backups." or the failures.
pub fn summary(results: &[SyncResult]) -> Result<String, Vec<String>> {
    let failed: Vec<String> = results
        .iter()
        .filter(|r| !r.ok)
        .map(|r| r.error.clone().unwrap_or_default())
        .collect();
    if !failed.is_empty() {
        return Err(failed);
    }
    let kept = results.iter().filter(|r| r.backup.is_some()).count();
    Ok(format!(
        "Done: {} files; {kept} replaced or removed jars kept in .hopper-backups.",
        results.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(n: &str) -> Instance {
        Instance {
            name: n.into(),
            ..Default::default()
        }
    }

    #[test]
    fn guesses_pairings_by_shared_words() {
        let list = vec![inst("All the Mods 10"), inst("TNP Limitless 8"), inst("Craft to Exile 2")];
        assert_eq!(guess_pairing("ATM10 - All Mods", &list).unwrap().name, "All the Mods 10");
        assert_eq!(guess_pairing("tnp-limitless", &list).unwrap().name, "TNP Limitless 8");
        assert!(guess_pairing("vanilla", &list).is_none());
    }

    fn entry(status: DiffStatus, side: &str, from_pack: bool) -> DiffEntry {
        let m = ModInfo {
            file_name: "a.jar".into(),
            mod_id: "a".into(),
            mod_name: "A".into(),
            side: side.into(),
            from_pack,
            ..Default::default()
        };
        DiffEntry {
            mod_name: "A".into(),
            client: (status != DiffStatus::OnlyServer).then(|| m.clone()),
            server: (status != DiffStatus::OnlyClient).then_some(m),
            status,
            action: DiffAction::Skip,
            skip_reason: None,
        }
    }

    #[test]
    fn filters_combine() {
        let none = HashSet::new();
        let d = entry(DiffStatus::OnlyClient, "client", false);
        let mut f = Filters::default();
        assert!(f.passes(&d, &none));
        f.status = StatusFilter::Server;
        assert!(!f.passes(&d, &none));
        f.status = StatusFilter::Client;
        f.source = SourceFilter::Pack;
        assert!(!f.passes(&d, &none));
        f.source = SourceFilter::Added;
        f.env = EnvFilter::Client;
        assert!(f.passes(&d, &none));
        let black: HashSet<String> = ["a".to_string()].into();
        assert!(!f.passes(&d, &black));
    }

    #[test]
    fn mod_actions_follow_status() {
        use ModAct::*;
        assert_eq!(mod_acts(&entry(DiffStatus::OnlyClient, "both", false)), vec![Push]);
        assert_eq!(
            mod_acts(&entry(DiffStatus::ClientNewer, "both", false)),
            vec![Push, RemoveFromServer]
        );
        assert_eq!(mod_acts(&entry(DiffStatus::Same, "both", false)), vec![RemoveFromServer]);
        let server = ServerInfo {
            name: "S".into(),
            root: "S".into(),
            manager: crate::server::Manager::Unknown,
            properties: Default::default(),
            loader: None,
            java: None,
            max_memory: None,
        };
        let ops = mod_ops(RemoveFromServer, &entry(DiffStatus::Same, "both", false), &inst("I"), &server);
        assert!(matches!(&ops.up[..], [SyncOp::Delete { path }] if path.ends_with("a.jar")));
        assert!(ops.down.is_empty());
    }

    #[test]
    fn row_actions_follow_status() {
        assert_eq!(row_action(DiffStatus::OnlyServer), Some(RowAct::Pull));
        assert_eq!(row_action(DiffStatus::ServerStale), Some(RowAct::Remove));
        assert_eq!(row_action(DiffStatus::Same), None);
    }
}
