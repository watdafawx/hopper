//! Shared data types. Ported from mod-sync's `models.go`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// One jar in a `mods` folder, with what its metadata says about it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ModInfo {
    pub file_name: String,
    pub mod_id: String,
    pub mod_name: String,
    pub version: String,
    /// "client" | "server" | "both" | "" when the jar does not say.
    pub side: String,
    pub file_size: u64,
    pub modified: i64,
    /// CurseForge project, when the instance knows it.
    #[serde(default)]
    pub project_id: u64,
    #[serde(default)]
    pub icon: String,
    /// Part of the installed modpack.
    #[serde(default)]
    pub from_pack: bool,
    /// Why this mod should stay off servers, if it should.
    #[serde(default)]
    pub wont_sync: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffStatus {
    /// Client has a newer (or different) build than the server.
    ClientNewer,
    /// Server has a newer build than the client.
    ServerNewer,
    OnlyClient,
    OnlyServer,
    /// Server holds an older file of a mod the client already has under another name.
    ServerStale,
    Same,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffAction {
    Skip,
    CopyToServer,
    DeleteFromServer,
}

#[derive(Clone, Debug)]
pub struct DiffEntry {
    pub mod_name: String,
    pub client: Option<ModInfo>,
    pub server: Option<ModInfo>,
    pub status: DiffStatus,
    pub action: DiffAction,
    /// Why the default is "skip" for a mod only the client has.
    pub skip_reason: Option<String>,
}

#[derive(Clone, Debug)]
pub enum SyncOp {
    Copy { src: PathBuf, dst: PathBuf },
    Delete { path: PathBuf },
}

#[derive(Clone, Debug)]
pub struct SyncResult {
    pub op: SyncOp,
    pub ok: bool,
    pub error: Option<String>,
    /// Where the overwritten or deleted file was preserved.
    pub backup: Option<PathBuf>,
}
