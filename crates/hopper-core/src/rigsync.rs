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
    let (client, note) = client_mods(inst, sync_anyway).map_err(|e| format!("client: {e:#}"))?;
    let theirs = server_mods_with_off(server).map_err(|e| format!("server: {e:#}"))?;
    Ok((diff_mods(&client, &theirs), note))
}

/// The jars in `dir`, on and off: an off one (`foo.jar.disabled`) is listed
/// as `foo.jar` with `off` set, unless the same jar is there on too.
pub fn mods_with_off(dir: &Path) -> anyhow::Result<Vec<ModInfo>> {
    let mut mods = crate::scan::scan_folder(dir)?;
    for e in std::fs::read_dir(dir)?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(jar) = name.strip_suffix(".disabled").filter(|j| j.to_ascii_lowercase().ends_with(".jar")) else { continue };
        // Both copies there: the one that's on wins.
        if mods.iter().any(|m| m.file_name.eq_ignore_ascii_case(jar)) || !e.path().is_file() {
            continue;
        }
        let mut m = crate::scan::scan_file(&e.path());
        m.file_name = jar.to_string();
        if m.mod_name.is_empty() || m.mod_name.to_ascii_lowercase().contains(".disabled") {
            m.mod_name = crate::scan::strip_version_suffix(jar);
        }
        m.off = true;
        mods.push(m);
    }
    Ok(mods)
}

/// Why a mod turned off in the pack stays off the server.
pub const TURNED_OFF: &str = "Turned off in this pack";

/// The pack's mods, the ones turned off included (`off`, held back from the
/// server), marked with what shouldn't sync.
pub fn client_mods(inst: &Instance, sync_anyway: &HashSet<u64>) -> anyhow::Result<(Vec<ModInfo>, Annotation)> {
    let mut client = mods_with_off(&inst.mods_dir())?;
    let note = crate::serverpack::annotate(&mut client, inst, sync_anyway);
    for m in client.iter_mut().filter(|m| m.off) {
        m.wont_sync = Some(TURNED_OFF.into());
    }
    Ok((client, note))
}

/// Why a mod the server has turned off isn't added again.
pub const OFF_ON_SERVER: &str = "Turned off on the server";

/// Compares the pack's mods ([`client_mods`]) with the server's. A mod
/// turned off in the pack that the server still runs isn't "only on the
/// server": its row asks to turn the server's copy off too. A mod the
/// server has turned off (any version) isn't added there again: that left
/// the old copy off beside a new one on, and the mod back on.
pub fn diff_mods(client: &[ModInfo], server: &[ModInfo]) -> Vec<DiffEntry> {
    let (server_off, server_on): (Vec<&ModInfo>, Vec<&ModInfo>) = server.iter().partition(|m| m.off);
    let server: Vec<ModInfo> = server_on.into_iter().cloned().collect();
    let off_ids: HashSet<String> = server_off.iter().filter(|m| !m.mod_id.is_empty()).map(|m| m.mod_id.to_lowercase()).collect();
    let off_stems: HashSet<String> = server_off.iter().map(|m| crate::serverpack::jar_stem(&m.file_name)).collect();
    let mut diff = diff_on(client, &server);
    for d in &mut diff {
        let Some(c) = d.client.as_ref().filter(|_| d.status == DiffStatus::OnlyClient) else { continue };
        let off_there = (!c.mod_id.is_empty() && off_ids.contains(&c.mod_id.to_lowercase()))
            || off_stems.contains(&crate::serverpack::jar_stem(&c.file_name));
        if off_there {
            d.action = DiffAction::Skip;
            d.skip_reason = Some(OFF_ON_SERVER.into());
        }
    }
    diff
}

fn diff_on(client: &[ModInfo], server: &[ModInfo]) -> Vec<DiffEntry> {
    // A turned-off jar whose mod another jar that's on provides (the pack's
    // copy swapped for a player's own build) isn't turned off: the mod still
    // runs, so the server is compared with the jar that's on. Matched by the
    // jar's mod id, since a fork has another project and file name.
    let on_ids: HashSet<String> = client.iter().filter(|m| !m.off && !m.mod_id.is_empty()).map(|m| m.mod_id.to_lowercase()).collect();
    let client: Vec<ModInfo> = client.iter().filter(|m| !(m.off && on_ids.contains(&m.mod_id.to_lowercase()))).cloned().collect();
    let mut diff = crate::diff::compute_diff(&client, server);
    for d in &mut diff {
        if d.client.as_ref().is_some_and(|c| c.off) && d.server.is_some() {
            d.mod_name = d.client.as_ref().map_or(d.mod_name.clone(), |c| c.mod_name.clone());
            d.client = None;
            d.status = DiffStatus::OnlyServer;
            d.action = DiffAction::DisableOnServer;
            d.skip_reason = Some(TURNED_OFF.into());
        }
    }
    diff
}

/// A server mod the pack has turned off: syncing turns it off there too.
pub fn is_off_here(d: &DiffEntry) -> bool {
    d.action == DiffAction::DisableOnServer
}

/// [`server_mods`] with the jars turned off there (`off` set), so Sync
/// knows what the server has turned off. An older Hopper Server sends only
/// the ones that are on.
pub fn server_mods_with_off(server: &ServerInfo) -> anyhow::Result<Vec<crate::model::ModInfo>> {
    match &server.remote {
        Some(r) => crate::hopperd::Client::for_remote(r)?.mods_with_off(&r.id),
        None => {
            crate::remotefs::refresh_path(&server.root)?;
            mods_with_off(&server.mods_dir())
        }
    }
}

/// The server's mods; a Hopper Server reads its own jars and sends the
/// list.
pub fn server_mods(server: &ServerInfo) -> anyhow::Result<Vec<crate::model::ModInfo>> {
    match &server.remote {
        Some(r) => crate::hopperd::Client::for_remote(r)?.mods(&r.id),
        None => {
            // Over SFTP / FTP: fetch what changed into Hopper's copy first.
            crate::remotefs::refresh_path(&server.root)?;
            crate::scan::scan_folder(&server.mods_dir())
        }
    }
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
        DiffStatus::OnlyServer if is_off_here(d) => "Turned off in this pack but still on on the server. Turn it off there too, so both match.".into(),
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

/// Where a row goes in the changeset: what would happen on the server.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Group {
    /// In the pack, missing on the server.
    Add,
    /// The server has another build.
    Update,
    /// Turned off in the pack, still on on the server.
    TurnOff,
    /// Only the server has it (or an old copy left behind).
    ServerOnly,
    /// Missing on the server on purpose: client-only or left out.
    KeptOff,
    /// Same on both.
    InSync,
}

impl Group {
    pub const ALL: [Group; 6] = [
        Group::Add,
        Group::Update,
        Group::TurnOff,
        Group::ServerOnly,
        Group::KeptOff,
        Group::InSync,
    ];

    pub fn of(d: &DiffEntry) -> Group {
        match d.status {
            DiffStatus::OnlyClient if is_wont(d) => Group::KeptOff,
            DiffStatus::OnlyClient => Group::Add,
            DiffStatus::ClientNewer | DiffStatus::ServerNewer => Group::Update,
            DiffStatus::OnlyServer if is_off_here(d) => Group::TurnOff,
            DiffStatus::OnlyServer | DiffStatus::ServerStale => Group::ServerOnly,
            DiffStatus::Same => Group::InSync,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Group::Add => "Will be added",
            Group::Update => "Updates",
            Group::TurnOff => "Turned off in the pack",
            Group::ServerOnly => "Only on the server",
            Group::KeptOff => "Kept off the server",
            Group::InSync => "In sync",
        }
    }

    /// The summary card's wording, after its count.
    pub fn short(self) -> &'static str {
        match self {
            Group::Add => "to add",
            Group::Update => "updates",
            Group::TurnOff => "to turn off",
            Group::ServerOnly => "only on server",
            Group::KeptOff => "kept off",
            Group::InSync => "in sync",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Group::Add => "In the pack, not on the server",
            Group::Update => "The server has another version",
            Group::TurnOff => "Still on on the server",
            Group::ServerOnly => "Not in the pack",
            Group::KeptOff => "Client-only, turned off in the pack, or left out by its author",
            Group::InSync => "Same file on both",
        }
    }

    /// Whether its rows can be picked for Apply.
    pub fn pickable(self) -> bool {
        matches!(self, Group::Add | Group::Update | Group::TurnOff | Group::ServerOnly)
    }
}

/// What Apply does with picked rows: copy adds and updates up, delete
/// server-only files (kept in .hopper-backups).
pub fn apply_ops(rows: &[DiffEntry], inst: &Instance, server: &ServerInfo) -> Ops {
    let mut up = push(rows, inst, server);
    up.extend(remove(rows, server));
    Ops {
        up,
        down: Vec::new(),
    }
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
        if is_hidden(d, blacklist) {
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
        let ok_text = match m {
            Some(m) => crate::fuzzy::words_match(&self.text, &[&d.mod_name, &m.file_name, &m.mod_id]),
            None => crate::fuzzy::words_match(&self.text, &[&d.mod_name]),
        };
        self.status.matches(d.status) && ok_env && ok_source && ok_wont && ok_text
    }
}

/// The mod is on the "Never sync" list (by mod id or file name).
pub fn mod_hidden(m: &crate::model::ModInfo, blacklist: &HashSet<String>) -> bool {
    blacklist.contains(&m.mod_id.to_lowercase()) || blacklist.contains(&m.file_name.to_lowercase())
}

/// A row whose mod is on the "Never sync" list, on either side: syncing
/// leaves both copies alone.
pub fn is_hidden(d: &DiffEntry, blacklist: &HashSet<String>) -> bool {
    [d.client.as_ref(), d.server.as_ref()].into_iter().flatten().any(|m| mod_hidden(m, blacklist))
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

// ---- before joining ----------------------------------------------------

/// What would keep this pack from joining its server: a different
/// NeoForge, and mods on only one side or at other versions. Mods that
/// belong on one side only (client-only, server-only, flagged) and the
/// "Never sync" list don't count.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JoinCheck {
    /// (the pack's, the server's) NeoForge, when they differ.
    pub loader: Option<(String, String)>,
    /// Mods in the pack the server lacks (names).
    pub only_pack: Vec<String>,
    /// Mods on the server the pack lacks (names).
    pub only_server: Vec<String>,
    /// Mods turned off in the pack the server still runs (names).
    pub off_here: Vec<String>,
    /// (name, the pack's version, the server's).
    pub versions: Vec<(String, String, String)>,
}

impl JoinCheck {
    pub fn is_empty(&self) -> bool {
        self.loader.is_none() && self.only_pack.is_empty() && self.only_server.is_empty() && self.off_here.is_empty() && self.versions.is_empty()
    }

    /// The one-line summary, for the banner.
    pub fn summary(&self) -> String {
        let n = |k: usize, one: &str, many: &str| if k == 1 { format!("1 {one}") } else { format!("{k} {many}") };
        let mut parts = Vec::new();
        if let Some((p, s)) = &self.loader {
            parts.push(format!("the server runs NeoForge {s}, this pack {p}"));
        }
        if !self.only_pack.is_empty() {
            parts.push(format!("{} only in the pack", n(self.only_pack.len(), "mod", "mods")));
        }
        if !self.only_server.is_empty() {
            parts.push(format!("{} only on the server", n(self.only_server.len(), "mod", "mods")));
        }
        if !self.off_here.is_empty() {
            parts.push(format!("{} turned off here but on on the server", n(self.off_here.len(), "mod", "mods")));
        }
        if !self.versions.is_empty() {
            parts.push(format!("{} at another version", n(self.versions.len(), "mod", "mods")));
        }
        let mut s = parts.join(", ");
        if let Some(c) = s.get(..1) {
            s = c.to_uppercase() + &s[1..];
        }
        s
    }

    /// A short, stable fingerprint, so a closed banner stays closed until
    /// something else differs.
    pub fn signature(&self) -> String {
        format!("{:?}|{}|{}|{}|{}", self.loader, self.only_pack.join(","), self.only_server.join(","), self.off_here.join(","), self.versions.iter().map(|v| format!("{}:{}:{}", v.0, v.1, v.2)).collect::<Vec<_>>().join(","))
    }
}

/// [`JoinCheck`] from a comparison and both NeoForge builds (`None` when a
/// side doesn't run NeoForge or it isn't known).
pub fn join_check(diff: &[DiffEntry], blacklist: &HashSet<String>, pack_nf: Option<&str>, server_nf: Option<&str>) -> JoinCheck {
    let mut c = JoinCheck::default();
    if let (Some(p), Some(s)) = (pack_nf, server_nf)
        && p != s
    {
        c.loader = Some((p.to_string(), s.to_string()));
    }
    let name = |d: &DiffEntry| {
        let m = d.client.as_ref().or(d.server.as_ref());
        m.map(|m| if m.mod_name.is_empty() { m.file_name.clone() } else { m.mod_name.clone() }).filter(|n| !n.is_empty()).unwrap_or_else(|| d.mod_name.clone())
    };
    for d in diff {
        if is_hidden(d, blacklist) || is_wont(d) {
            continue;
        }
        let side = |m: &Option<ModInfo>| m.as_ref().map_or("", |m| m.side.as_str()).to_string();
        match d.status {
            DiffStatus::OnlyServer if is_off_here(d) => c.off_here.push(name(d)),
            DiffStatus::OnlyClient if side(&d.client) != "client" => c.only_pack.push(name(d)),
            DiffStatus::OnlyServer if blocks_join(d, blacklist) => c.only_server.push(name(d)),
            DiffStatus::ClientNewer | DiffStatus::ServerNewer if side(&d.client) != "client" => {
                let v = |m: &Option<ModInfo>| m.as_ref().map_or(String::new(), |m| m.version.clone());
                c.versions.push((name(d), v(&d.client), v(&d.server)));
            }
            _ => {}
        }
    }
    c.only_pack.sort();
    c.only_server.sort();
    c.off_here.sort();
    c.versions.sort();
    c
}

/// A server-only mod that keeps the pack from joining (what the banner calls
/// "only on the server"): not tagged server-only, not on "Never sync".
pub fn blocks_join(d: &DiffEntry, blacklist: &HashSet<String>) -> bool {
    d.status == DiffStatus::OnlyServer
        && !is_off_here(d)
        && !is_hidden(d, blacklist)
        && d.server.as_ref().is_some_and(|m| m.side != "server")
}

/// Everything a [`join_check`] is worked out from, short of reading the
/// mods: when any of it changes, the last answer may be wrong.
#[derive(Clone, Debug, Default)]
pub struct JoinInputs<'a> {
    pub inst: Option<&'a Instance>,
    pub server_root: Option<&'a Path>,
    /// Bumped by the app whenever it changed the server's files (a sync, an
    /// update, a pushed config) or read its server list again.
    pub server_rev: u64,
    /// The server's mods folder fingerprint, from panels that send one
    /// ([`crate::server::mods_stamp`]): changes made elsewhere, like Hopper
    /// Server's web page.
    pub mods_stamp: Option<u64>,
    /// Running and starting: a restart can change what the server loads.
    pub running: (bool, bool),
    pub blacklist: Option<&'a HashSet<String>>,
    pub sync_anyway: Option<&'a HashSet<u64>>,
}

impl JoinInputs<'_> {
    pub fn key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.server_root.hash(&mut h);
        (self.server_rev, self.mods_stamp, self.running).hash(&mut h);
        if let Some(i) = self.inst {
            i.root.hash(&mut h);
            i.loader.hash(&mut h);
            for a in &i.addons {
                (&a.file_name, a.enabled, a.file_id).hash(&mut h);
            }
        }
        let mut hidden: Vec<&String> = self.blacklist.into_iter().flatten().collect();
        hidden.sort();
        hidden.hash(&mut h);
        let mut anyway: Vec<&u64> = self.sync_anyway.into_iter().flatten().collect();
        anyway.sort();
        anyway.hash(&mut h);
        h.finish()
    }
}

/// One pack's comparison with its server, kept current: the last answer
/// stays until a new one lands, and a new one is asked for as soon as what
/// it depends on ([`JoinInputs::key`]) changes, not on a timer. Only one
/// runs at a time; one that ends after its inputs moved on is followed by
/// another.
#[derive(Clone, Debug)]
pub struct JoinWatch<T> {
    /// The last answer and the key it was worked out for.
    done: Option<(u64, std::time::Instant, T)>,
    /// The key of the comparison running now.
    running: Option<u64>,
}

impl<T> Default for JoinWatch<T> {
    fn default() -> Self {
        JoinWatch { done: None, running: None }
    }
}

impl<T> JoinWatch<T> {
    /// Whether to start a comparison for `key` now (it is then counted as
    /// running). Also when the last answer is older than `max_age`: a server
    /// read over a share or SFTP sends no fingerprint.
    pub fn want(&mut self, key: u64, max_age: std::time::Duration) -> bool {
        if self.running.is_some() {
            return false;
        }
        if let Some((k, at, _)) = &self.done
            && *k == key
            && at.elapsed() < max_age
        {
            return false;
        }
        self.running = Some(key);
        true
    }

    /// A comparison for `key` ended. The caller asks [`Self::want`] again
    /// with today's key, in case it changed while this one ran.
    pub fn finish(&mut self, key: u64, value: T) {
        self.running = None;
        self.done = Some((key, std::time::Instant::now(), value));
    }

    pub fn value(&self) -> Option<&T> {
        self.done.as_ref().map(|d| &d.2)
    }

    /// The answer shown is for other inputs than `key`: a new one is on its
    /// way, so its numbers shouldn't be trusted.
    pub fn stale(&self, key: u64) -> bool {
        self.done.as_ref().is_some_and(|d| d.0 != key)
    }
}

// ---- operations --------------------------------------------------------

/// File operations for rows: (to the server, to the client).
#[derive(Default, Debug, Clone)]
pub struct Ops {
    pub up: Vec<SyncOp>,
    pub down: Vec<SyncOp>,
}

impl Ops {
    pub fn is_empty(&self) -> bool {
        self.up.is_empty() && self.down.is_empty()
    }

    /// The mods from GitHub and other links these operations copy (their
    /// pins on the side they come from), for the ask before a sync.
    pub fn source_pins(&self, server_root: &Path, inst_root: &Path) -> Vec<crate::sources::carry::Pin> {
        let jars = |ops: &[SyncOp]| -> Vec<String> {
            ops.iter()
                .filter_map(|o| match o {
                    SyncOp::Copy { src, .. } => Some(src.file_name()?.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect()
        };
        let mut pins = crate::sources::carry::pins_for(inst_root, &jars(&self.up));
        pins.extend(crate::sources::carry::pins_for(server_root, &jars(&self.down)));
        pins
    }

    /// The operations without the copies of the jars in `skip` (the player
    /// skipped mods from other sources).
    pub fn without(mut self, skip: &[crate::sources::carry::Pin]) -> Ops {
        let keep = |o: &SyncOp| match o {
            SyncOp::Copy { src, .. } => {
                let name = src.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
                !skip.iter().any(|p| p.file.to_lowercase() == name.trim_end_matches(".disabled"))
            }
            _ => true,
        };
        self.up.retain(keep);
        self.down.retain(keep);
        self
    }
}

/// Copies client files over (replacing the server's old builds).
pub fn push(rows: &[DiffEntry], inst: &Instance, server: &ServerInfo) -> Vec<SyncOp> {
    let rows: Vec<DiffEntry> = rows
        .iter()
        .filter(|d| d.client.as_ref().is_some_and(|c| !c.off) && row_action(d.status) == Some(RowAct::Push))
        .cloned()
        .map(|mut d| {
            d.action = DiffAction::CopyToServer;
            d
        })
        .collect();
    crate::sync::plan(&rows, &inst.mods_dir(), &server.mods_dir())
}

/// Copies server files down to the client: server-only jars, and the
/// server's build of a mod both sides have in another version (the
/// client's old file is removed when its name differs).
pub fn pull(rows: &[DiffEntry], inst: &Instance, server: &ServerInfo) -> Vec<SyncOp> {
    let mut ops = Vec::new();
    for d in rows {
        let Some(s) = &d.server else { continue };
        match d.status {
            DiffStatus::OnlyServer if is_off_here(d) => continue,
            DiffStatus::OnlyServer => {}
            DiffStatus::ServerNewer | DiffStatus::ClientNewer => {
                if let Some(c) = &d.client
                    && !c.file_name.eq_ignore_ascii_case(&s.file_name)
                {
                    ops.push(SyncOp::Delete {
                        path: inst.mods_dir().join(&c.file_name),
                    });
                }
            }
            _ => continue,
        }
        ops.push(SyncOp::Copy {
            src: server.mods_dir().join(&s.file_name),
            dst: inst.mods_dir().join(&s.file_name),
        });
    }
    ops
}

/// Deletes server-only and leftover files from the server.
pub fn remove(rows: &[DiffEntry], server: &ServerInfo) -> Vec<SyncOp> {
    rows.iter()
        .filter(|d| matches!(d.status, DiffStatus::OnlyServer | DiffStatus::ServerStale))
        .filter_map(|d| Some((d, d.server.as_ref()?)))
        .map(|(d, m)| {
            let path = server.mods_dir().join(&m.file_name);
            if is_off_here(d) { SyncOp::Disable { path } } else { SyncOp::Delete { path } }
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
    // Mods from GitHub and other links keep their source on the other side.
    crate::sources::carry::carry(inst_root, server_root, &crate::sources::carry::copied_jars(&results, server_root));
    let pulled = crate::sync::execute(ops.down, inst_root);
    crate::history::record(server_root, "client", inst_root, &pulled);
    crate::sources::carry::carry(server_root, inst_root, &crate::sources::carry::copied_jars(&pulled, inst_root));
    results.extend(pulled);
    results
}

/// Like [`run`], for any server: a server on a Hopper Server syncs over its
/// API (replaced and removed files land in the panel's trash; pulls keep
/// local backups as usual).
pub fn run_on(ops: Ops, server: &ServerInfo, inst_root: &Path) -> Vec<SyncResult> {
    if let Some(r) = &server.remote {
        crate::hopperd::register(&server.root, r);
    }
    run(ops, &server.root, inst_root)
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

/// A Sync button's label for what it will do: `push` jars copied to the
/// server (added or updated) and `remove` taken off it. Only removals:
/// "Remove 2 from server"; only pushes: "Push 3 to server"; both:
/// "Sync 5 (push 3, remove 2)". `None` when there is nothing to do.
pub fn sync_label(push: usize, remove: usize) -> Option<String> {
    match (push, remove) {
        (0, 0) => None,
        (p, 0) => Some(format!("Push {p} to server")),
        (0, r) => Some(format!("Remove {r} from server")),
        (p, r) => Some(format!("Sync {} (push {p}, remove {r})", p + r)),
    }
}

/// What a Sync button does, naming only the actions it will take; `kept_in`
/// is where replaced and removed jars go.
pub fn sync_help(add: usize, update: usize, remove: usize, kept_in: &str) -> String {
    let jars = |n: usize| if n == 1 { "jar" } else { "jars" };
    let mut push = Vec::new();
    if add > 0 {
        push.push(format!("{add} new"));
    }
    if update > 0 {
        push.push(format!("{update} updated"));
    }
    let n_push = add + update;
    let push = push.join(" and ");
    match (n_push, remove) {
        (0, 0) => "Tick mods to choose what to sync.".to_string(),
        (_, 0) if update > 0 => format!("Copies {push} {} to the server. Replaced jars are kept in {kept_in}.", jars(n_push)),
        (_, 0) => format!("Copies {push} {} to the server.", jars(n_push)),
        (0, r) => format!("Removes {r} {} the server no longer needs. They are kept in {kept_in}.", jars(r)),
        (_, r) => format!(
            "Copies {push} {} to the server and removes {r} it no longer needs. Replaced and removed jars are kept in {kept_in}.",
            jars(n_push)
        ),
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn never_sync_mods_drop_out_of_a_full_sync() {
        use crate::model::ModInfo;
        let m = |id: &str, file: &str| ModInfo { mod_id: id.into(), file_name: file.into(), ..Default::default() };
        let client = vec![m("ibp", "InteractiveBlockPrompt-1.0.jar"), m("jei", "jei-1.jar")];
        let server = vec![m("oldhidden", "OldHidden-2.jar")];
        let hidden: HashSet<String> = ["ibp".to_string(), "oldhidden-2.jar".to_string()].into();
        let mut diff = crate::diff::compute_diff(&client, &server);
        diff.retain(|d| !is_hidden(d, &hidden));
        let names: Vec<_> = diff.iter().filter_map(|d| d.client.as_ref().or(d.server.as_ref())).map(|m| m.mod_id.as_str()).collect();
        assert_eq!(names, ["jei"]);
    }
    use super::*;

    #[test]
    fn join_check_leaves_out_one_sided_and_never_sync_mods() {
        use crate::model::ModInfo;
        let m = |id: &str, file: &str, ver: &str, side: &str| ModInfo {
            mod_id: id.into(),
            mod_name: id.into(),
            file_name: file.into(),
            version: ver.into(),
            side: side.into(),
            ..Default::default()
        };
        let client = vec![
            m("create", "create-6.0.4.jar", "6.0.4", "both"),
            m("jei", "jei-19.jar", "19", "both"),
            m("sodium", "sodium-0.6.jar", "0.6", "client"),
            m("extra", "extra-1.jar", "1", ""),
            m("hidden", "hidden-1.jar", "1", ""),
        ];
        let server = vec![
            m("create", "create-6.0.3.jar", "6.0.3", "both"),
            m("jei", "jei-19.jar", "19", "both"),
            m("spark_srv", "serveronly-1.jar", "1", "server"),
            m("waystones", "waystones-21.jar", "21", "both"),
        ];
        let diff = crate::diff::compute_diff(&client, &server);
        let hidden: HashSet<String> = ["hidden".to_string()].into();
        let c = join_check(&diff, &hidden, Some("21.1.215"), Some("21.1.77"));
        assert_eq!(c.loader, Some(("21.1.215".into(), "21.1.77".into())));
        assert_eq!(c.only_pack, ["extra"]);
        assert_eq!(c.only_server, ["waystones"]);
        assert_eq!(c.versions.len(), 1, "{:?}", c.versions);
        assert_eq!(c.versions[0].0, "create");
        assert!(c.summary().starts_with("The server runs NeoForge 21.1.77, this pack 21.1.215"), "{}", c.summary());
        // Same NeoForge and the same mods: nothing to say.
        let same = crate::diff::compute_diff(&server, &server);
        assert!(join_check(&same, &HashSet::new(), Some("21.1.77"), Some("21.1.77")).is_empty());
    }

    #[test]
    fn join_check_is_asked_again_after_a_sync() {
        use std::time::Duration;
        let long = Duration::from_secs(3600);
        let mut pack = inst("TNP");
        pack.addons.push(crate::instances::Addon { file_name: "ae2wcwt-1.3.11.jar".into(), enabled: true, ..Default::default() });
        let root = Path::new("srv");
        let inputs = |pack: &Instance, rev: u64, stamp: Option<u64>, running: (bool, bool)| {
            JoinInputs { inst: Some(pack), server_root: Some(root), server_rev: rev, mods_stamp: stamp, running, ..Default::default() }.key()
        };
        let mut w: JoinWatch<&str> = JoinWatch::default();
        let k1 = inputs(&pack, 1, Some(7), (true, false));
        assert!(w.want(k1, long));
        // One at a time.
        assert!(!w.want(k1, long));
        w.finish(k1, "AE2 WCWT differs");
        assert!(!w.want(k1, long), "nothing changed: no new comparison");
        assert!(!w.stale(k1));

        // A sync to the server bumps its revision: compare again at once,
        // and the old answer counts as stale until the new one lands.
        let k2 = inputs(&pack, 2, Some(7), (true, false));
        assert_ne!(k1, k2);
        assert!(w.stale(k2));
        assert!(w.want(k2, long));
        w.finish(k2, "");
        assert!(!w.stale(k2));
        assert_eq!(w.value(), Some(&""));

        // Changes made elsewhere (the server's own page), a restart, and the
        // pack's mods (a toggle) each count too.
        assert_ne!(inputs(&pack, 2, Some(8), (true, false)), k2);
        assert_ne!(inputs(&pack, 2, Some(7), (true, true)), k2);
        pack.addons[0].enabled = false;
        assert_ne!(inputs(&pack, 2, Some(7), (true, false)), k2);
    }

    #[test]
    fn join_check_started_before_a_sync_is_followed_by_another() {
        use std::time::Duration;
        let long = Duration::from_secs(3600);
        let mut w: JoinWatch<u32> = JoinWatch::default();
        assert!(w.want(1, long));
        // The sync ends while the first comparison still runs.
        assert!(!w.want(2, long));
        w.finish(1, 5);
        // Its answer is for the old server: stale, and asked again.
        assert!(w.stale(2));
        assert!(w.want(2, long));
        w.finish(2, 0);
        assert!(!w.stale(2));
        // An old answer is refreshed even when nothing says it changed.
        assert!(w.want(2, Duration::ZERO));
    }

    /// A mod turned off in the pack isn't "only on the server": while the
    /// server still runs it, it's to turn off there (and syncing turns it
    /// off, not deletes it); once it's off there, it's kept off.
    #[test]
    fn mod_turned_off_in_the_pack_is_turned_off_on_the_server() {
        use crate::model::ModInfo;
        let root = std::env::temp_dir().join(format!("hopper-test-offpack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let pack = Instance { name: "P".into(), root: root.join("pack"), ..Default::default() };
        std::fs::create_dir_all(pack.mods_dir()).unwrap();
        std::fs::write(pack.mods_dir().join("MysticalAgriculturePlus-2.0.2.jar.disabled"), b"x").unwrap();
        let (client, _) = client_mods(&pack, &HashSet::new()).unwrap();
        let off = client.iter().find(|m| m.off).expect("the turned-off jar is listed");
        assert_eq!(off.file_name, "MysticalAgriculturePlus-2.0.2.jar");
        assert_eq!(off.wont_sync.as_deref(), Some(TURNED_OFF));

        let m = |file: &str| ModInfo { file_name: file.into(), mod_name: file.into(), side: "both".into(), ..Default::default() };
        let server_info = ServerInfo {
            name: "S".into(),
            root: root.join("srv"),
            manager: crate::server::Manager::Unknown,
            properties: Default::default(),
            loader: None,
            java: None,
            max_memory: None,
            remote: None,
        };
        // Still on on the server.
        let diff = diff_mods(&client, &[m("MysticalAgriculturePlus-2.0.2.jar")]);
        let row = diff.iter().find(|d| d.server.as_ref().is_some_and(|s| s.file_name.starts_with("Mystical"))).unwrap();
        assert_eq!(Group::of(row), Group::TurnOff);
        assert!(Group::of(row).pickable());
        let c = join_check(&diff, &HashSet::new(), None, None);
        assert!(c.only_server.is_empty(), "{c:?}");
        assert_eq!(c.off_here.len(), 1);
        assert!(!c.is_empty());
        let rows = std::slice::from_ref(row);
        assert!(matches!(&remove(rows, &server_info)[..], [SyncOp::Disable { path }] if path.ends_with("MysticalAgriculturePlus-2.0.2.jar")));
        assert!(pull(rows, &pack, &server_info).is_empty());
        assert!(push(rows, &pack, &server_info).is_empty());
        assert!(matches!(&crate::sync::plan(&diff, &pack.mods_dir(), &server_info.mods_dir())[..], [SyncOp::Disable { .. }]));

        // Turned off there too: kept off, nothing to do, nothing to send.
        let diff = diff_mods(&client, &[]);
        let row = diff.iter().find(|d| d.client.as_ref().is_some_and(|c| c.off)).unwrap();
        assert_eq!(Group::of(row), Group::KeptOff);
        assert!(join_check(&diff, &HashSet::new(), None, None).is_empty());
        assert!(crate::sync::plan(&diff, &pack.mods_dir(), &server_info.mods_dir()).is_empty());
        assert!(push(&diff, &pack, &server_info).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The pack's copy turned off and the player's own build of the same mod
    /// (same mod id, other file) turned on: the mod isn't turned off, and the
    /// server is compared with the build that's on.
    #[test]
    fn turned_off_mod_replaced_by_a_fork_is_not_turned_off() {
        use crate::model::ModInfo;
        let m = |file: &str, id: &str, version: &str, off: bool| ModInfo {
            file_name: file.into(),
            mod_id: id.into(),
            mod_name: "Effortless Building".into(),
            version: version.into(),
            side: "both".into(),
            off,
            wont_sync: off.then(|| TURNED_OFF.into()),
            ..Default::default()
        };
        let client = [
            m("effortlessbuilding-1.21.1-4.0.2-all.jar", "effortlessbuilding", "4.0.2", true),
            m("EffortlessBuilding-fork-4.2.37.jar", "effortlessbuilding", "4.2.37", false),
        ];

        // The server runs the fork: in sync, nothing to turn off.
        let diff = diff_mods(&client, &[m("EffortlessBuilding-fork-4.2.37.jar", "effortlessbuilding", "4.2.37", false)]);
        assert!(diff.iter().all(|d| !is_off_here(d)), "{diff:?}");
        assert_eq!(diff.len(), 1, "{diff:?}");
        assert_eq!(Group::of(&diff[0]), Group::InSync);
        assert!(join_check(&diff, &HashSet::new(), None, None).is_empty());

        // The server runs another build of the fork: an update, not a turn-off.
        let diff = diff_mods(&client, &[m("EffortlessBuilding-fork-4.2.30.jar", "effortlessbuilding", "4.2.30", false)]);
        assert!(diff.iter().all(|d| !is_off_here(d)), "{diff:?}");
        assert_eq!(diff.len(), 1, "{diff:?}");
        assert_eq!(Group::of(&diff[0]), Group::Update);
        assert_eq!(diff[0].client.as_ref().unwrap().version, "4.2.37");
        let c = join_check(&diff, &HashSet::new(), None, None);
        assert!(c.off_here.is_empty() && c.versions.len() == 1, "{c:?}");

        // Without the fork, the turned-off copy still asks to turn it off.
        let diff = diff_mods(&client[..1], &[m("EffortlessBuilding-fork-4.2.37.jar", "effortlessbuilding", "4.2.37", false)]);
        assert!(diff.iter().any(is_off_here), "{diff:?}");
    }

    /// TNP-LL8, 2026-11-06: Advancement Frames was off on the server; Sync
    /// added the pack's newer version on beside the old copy left off.
    #[test]
    fn a_mod_off_on_the_server_is_not_added_again() {
        use crate::model::ModInfo;
        let m = |file: &str, version: &str, off: bool| ModInfo {
            file_name: file.into(),
            mod_id: "advancementframes".into(),
            mod_name: "Advancement Frames".into(),
            version: version.into(),
            side: "both".into(),
            off,
            ..Default::default()
        };
        let client = [m("advancementframes-1.21-2.3.2-neoforge.jar", "2.3.2", false)];
        let diff = diff_mods(&client, &[m("advancementframes-1.21-2.3.0-neoforge.jar", "2.3.0", true)]);
        assert_eq!(diff.len(), 1, "{diff:?}");
        assert_eq!(diff[0].action, DiffAction::Skip);
        assert_eq!(diff[0].skip_reason.as_deref(), Some(OFF_ON_SERVER));
        // Not there at all: added as before.
        let diff = diff_mods(&client, &[]);
        assert_eq!(diff[0].action, DiffAction::CopyToServer, "{diff:?}");
    }

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
    fn groups_say_what_happens_on_the_server() {
        assert_eq!(Group::of(&entry(DiffStatus::OnlyClient, "both", false)), Group::Add);
        let mut kept = entry(DiffStatus::OnlyClient, "client", false);
        kept.skip_reason = Some("Client-only".into());
        assert_eq!(Group::of(&kept), Group::KeptOff);
        // Already on the server: a won't-sync claim doesn't move it.
        let mut on = entry(DiffStatus::Same, "both", true);
        on.client.as_mut().unwrap().wont_sync = Some("left out".into());
        assert_eq!(Group::of(&on), Group::InSync);
        assert_eq!(Group::of(&entry(DiffStatus::ServerNewer, "both", false)), Group::Update);
        assert_eq!(Group::of(&entry(DiffStatus::ServerStale, "both", false)), Group::ServerOnly);
        assert!(Group::Add.pickable() && !Group::KeptOff.pickable());
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
            remote: None,
        };
        let ops = mod_ops(RemoveFromServer, &entry(DiffStatus::Same, "both", false), &inst("I"), &server);
        assert!(matches!(&ops.up[..], [SyncOp::Delete { path }] if path.ends_with("a.jar")));
        assert!(ops.down.is_empty());
    }

    #[test]
    fn pull_takes_the_servers_build() {
        let server = ServerInfo {
            name: "S".into(),
            root: "S".into(),
            manager: crate::server::Manager::Unknown,
            properties: Default::default(),
            loader: None,
            java: None,
            max_memory: None,
            remote: None,
        };
        let mut d = entry(DiffStatus::ServerNewer, "both", false);
        d.server.as_mut().unwrap().file_name = "a-2.jar".into();
        let ops = pull(std::slice::from_ref(&d), &inst("I"), &server);
        assert!(matches!(&ops[..], [
            SyncOp::Delete { path },
            SyncOp::Copy { src, dst },
        ] if path.ends_with("mods/a.jar") && src.ends_with("S/mods/a-2.jar") && dst.ends_with("mods/a-2.jar")));
        // Same name: overwritten in place, nothing deleted.
        let same = entry(DiffStatus::ClientNewer, "both", false);
        assert!(matches!(&pull(&[same], &inst("I"), &server)[..], [SyncOp::Copy { .. }]));
        assert!(pull(&[entry(DiffStatus::Same, "both", false)], &inst("I"), &server).is_empty());
    }

    #[test]
    fn row_actions_follow_status() {
        assert_eq!(row_action(DiffStatus::OnlyServer), Some(RowAct::Pull));
        assert_eq!(row_action(DiffStatus::ServerStale), Some(RowAct::Remove));
        assert_eq!(row_action(DiffStatus::Same), None);
    }

    #[test]
    fn sync_label_says_what_it_does() {
        assert_eq!(sync_label(0, 0), None);
        assert_eq!(sync_label(0, 2).as_deref(), Some("Remove 2 from server"));
        assert_eq!(sync_label(3, 0).as_deref(), Some("Push 3 to server"));
        assert_eq!(sync_label(3, 2).as_deref(), Some("Sync 5 (push 3, remove 2)"));
    }

    #[test]
    fn sync_help_names_only_present_actions() {
        let k = ".hopper-backups";
        assert_eq!(sync_help(0, 0, 2, k), "Removes 2 jars the server no longer needs. They are kept in .hopper-backups.");
        assert_eq!(sync_help(3, 0, 0, k), "Copies 3 new jars to the server.");
        assert_eq!(sync_help(3, 1, 0, k), "Copies 3 new and 1 updated jars to the server. Replaced jars are kept in .hopper-backups.");
        assert_eq!(
            sync_help(1, 0, 1, k),
            "Copies 1 new jar to the server and removes 1 it no longer needs. Replaced and removed jars are kept in .hopper-backups."
        );
    }
}
