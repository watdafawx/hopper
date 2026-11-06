//! Servers' mods in the same Mods table as an instance's. A server keeps a
//! `hopper.json` like an instance (`hopper::server_mods`), so its mod list
//! is an `Instance` here too:
//!
//! - a server folder on this PC is read from disk, and changed with the
//!   instance code like any instance;
//! - a server on a Hopper Server is fetched from its panel
//!   (`GET /servers/{id}/mods/instance`) and changed through the panel's mod
//!   routes; the list is fetched again after each change.
//!
//! These lists sit beside the Library's instances (`Store::instance` finds
//! both) but never show in the Library.

use super::{Msg, Store, StoreEvent};
use gpui_kit::*;
use hopper::hopperd::{Client, Remote};
use hopper::install::Update;
use hopper::instances::Instance;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A jar on a Hopper Server that its list doesn't track.
#[derive(Clone, Debug, Default)]
pub struct RemoteLoose {
    pub file: String,
    pub icon: String,
}

/// What a fetch of a server's mod list brought back.
pub struct Fetched {
    pub inst: Instance,
    pub loose: Vec<RemoteLoose>,
}

/// The Hopper Server a stand-in root is on.
pub fn remote(root: &Path) -> Option<Remote> {
    hopper::hopperd::remote_of(root).filter(|(r, _)| r == root).map(|(_, r)| r)
}

/// File names as the panel takes them (without `.disabled`).
fn bases(files: &[String]) -> Vec<String> {
    files.iter().map(|f| f.trim_end_matches(".disabled").to_string()).collect()
}

fn fetch_remote(root: &Path, r: &Remote) -> anyhow::Result<Fetched> {
    let c = Client::for_remote(r)?;
    let v = c.get(&Client::server_path(&r.id, "/mods/instance"))?;
    let mut inst: Instance = serde_json::from_value(v["instance"].clone())?;
    inst.root = root.to_path_buf();
    note_untrusted(root, &v);
    let loose = v["loose"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|l| RemoteLoose {
                    file: l["file"].as_str().unwrap_or_default().to_string(),
                    icon: l["icon"].as_str().unwrap_or_default().to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Fetched { inst, loose })
}

/// The panel's update offers as Hopper's own, with the files they'd break
/// and the mods whose installed file the author archived.
type RemoteUpdates = (Vec<Update>, HashSet<u64>, HashMap<u64, Vec<(String, String)>>);

fn updates_remote(r: &Remote, force: bool) -> anyhow::Result<RemoteUpdates> {
    let c = Client::for_remote(r)?;
    let v = c.get(&Client::server_path(&r.id, if force { "/mods/updates?force=1" } else { "/mods/updates" }))?;
    let id = |v: &Value| match v {
        Value::String(s) => s.parse().unwrap_or(0),
        Value::Number(n) => n.as_u64().unwrap_or(0),
        _ => 0,
    };
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
    let mut list = Vec::new();
    let mut breaks = HashMap::new();
    for u in v["updates"].as_array().into_iter().flatten() {
        let pid = id(&u["project_id"]);
        if pid == 0 {
            continue;
        }
        let b: Vec<(String, String)> = u["breaks"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|b| (s(&b["name"]), s(&b["needs"])))
            .collect();
        if !b.is_empty() {
            breaks.insert(pid, b);
        }
        list.push(Update {
            project_id: pid,
            name: s(&u["name"]),
            from: s(&u["from_file"]),
            file: hopper::cf::File {
                id: id(&u["file_id"]),
                mod_id: pid,
                file_name: s(&u["file"]),
                file_date: s(&u["date"]),
                release_type: u["release_type"].as_u64().unwrap_or(1) as u32,
                ..Default::default()
            },
        });
    }
    let archived = v["archived"].as_array().into_iter().flatten().map(id).filter(|&i| i != 0).collect();
    Ok((list, archived, breaks))
}

impl Store {
    /// A server's mod list is shown here (not a Library instance).
    pub fn is_server_list(&self, root: &Path) -> bool {
        self.server_insts.iter().any(|i| i.root == root)
    }

    /// The list for a server on a Hopper Server couldn't be fetched: why.
    pub fn server_mods_error(&self, root: &Path) -> Option<&str> {
        self.server_mods_err.get(root).map(String::as_str)
    }

    /// Untracked jars on a Hopper Server (none for a local folder: the
    /// Mods table reads those from disk).
    pub fn remote_loose(&self, root: &Path) -> &[RemoteLoose] {
        self.remote_loose.get(root).map_or(&[], Vec::as_slice)
    }

    /// Reads (or fetches) a server's mod list for the Mods table. `name` is
    /// the server's, for a list made the first time.
    pub fn open_server_mods(&mut self, root: &Path, name: &str) {
        let root = root.to_path_buf();
        if let Some(r) = remote(&root) {
            self.fetch_server_mods(&root, &r);
            return;
        }
        let name = name.to_string();
        self.spawn(move || {
            let r = hopper::server_mods::load(&root, &name, "")
                .map(|(mut inst, _)| {
                    // View only: a server set up from a server pack has no marks.
                    hopper::modpack::mark_pack_mods(&mut inst);
                    Box::new(Fetched { inst, loose: Vec::new() })
                })
                .map_err(|e| format!("{e:#}"));
            Msg::ServerMods(root, r)
        });
    }

    fn fetch_server_mods(&mut self, root: &Path, r: &Remote) {
        // One fetch at a time per server; a change meanwhile fetches again
        // after it.
        if !self.server_fetching.insert(root.to_path_buf()) {
            self.server_refetch.insert(root.to_path_buf());
            return;
        }
        let (root, r) = (root.to_path_buf(), r.clone());
        self.spawn(move || {
            let res = fetch_remote(&root, &r).map(Box::new).map_err(|e| format!("{e:#}"));
            Msg::ServerMods(root, res)
        });
    }

    pub(super) fn server_mods_arrived(&mut self, root: PathBuf, res: Result<Box<Fetched>, String>) {
        self.server_fetching.remove(&root);
        match res {
            Ok(f) => {
                self.server_mods_err.remove(&root);
                let Fetched { mut inst, loose } = *f;
                inst.root = root.clone();
                if remote(&root).is_some() {
                    for l in &loose {
                        self.want_image(&l.icon);
                    }
                    self.remote_loose.insert(root.clone(), loose);
                }
                if !self.is_server_list(&root) {
                    self.server_insts.push(Instance { root: root.clone(), ..Default::default() });
                }
                self.put_instance(inst);
            }
            Err(e) => {
                log::warn!("mods of {}: {e}", root.display());
                self.server_mods_err.insert(root.clone(), e);
            }
        }
        if self.server_refetch.remove(&root)
            && let Some(r) = remote(&root)
        {
            self.fetch_server_mods(&root, &r);
        }
    }

    /// Takes mods off a shown server list at once (they're being removed).
    pub fn hide_server_mods(&mut self, root: &Path, files: &HashSet<String>) {
        if let Some(i) = self.server_insts.iter_mut().find(|i| i.root == root) {
            i.addons.retain(|a| !files.contains(&a.file_name.trim_end_matches(".disabled").to_lowercase()));
        }
        if let Some(l) = self.remote_loose.get_mut(root) {
            l.retain(|l| !files.contains(&l.file.trim_end_matches(".disabled").to_lowercase()));
        }
    }

    /// Reads a server's list again (after a change made elsewhere).
    pub fn reload_server_mods(&mut self, root: &Path) {
        if remote(root).is_some() {
            self.refetch_server_mods(root);
        }
    }

    /// After a change on a Hopper Server (here or elsewhere): fetch its list
    /// again, when it's shown.
    pub(super) fn refetch_server_mods(&mut self, root: &Path) {
        if self.is_server_list(root)
            && let Some(r) = remote(root)
        {
            self.fetch_server_mods(root, &r);
        }
    }

    /// Runs `rest` on the server's panel (a quick POST), with `local` applied
    /// to the shown list at once so the table doesn't wait. The list is
    /// fetched again either way; a failure shows as a notice.
    pub(super) fn remote_post(&mut self, root: &Path, rest: &'static str, body: Value, local: impl FnOnce(&mut Instance)) {
        let Some(r) = remote(root) else { return };
        if let Some(i) = self.server_insts.iter_mut().find(|i| i.root == root) {
            local(i);
        }
        let root = root.to_path_buf();
        self.spawn(move || {
            let res = Client::for_remote(&r).and_then(|c| c.post(&Client::server_path(&r.id, rest), body));
            match res {
                Ok(_) => Msg::InstanceChanged(root),
                Err(e) => Msg::RemoteFailed(root, format!("{e:#}")),
            }
        });
    }

    /// Runs a panel job (`rest` answers `{job}`) as a task here, then fetches
    /// the list again. `done` turns the job's answer into the message.
    pub(super) fn remote_job(
        &self,
        root: &Path,
        title: String,
        rest: &'static str,
        body: Value,
        done: impl FnOnce(PathBuf, Value) -> Msg + Send + 'static,
    ) {
        let Some(r) = remote(root) else { return };
        let root2 = root.to_path_buf();
        self.spawn_instance_job(root, title, move |job| {
            let c = Client::for_remote(&r)?;
            let v = c.run_job(&Client::server_path(&r.id, rest), body, job)?;
            Ok(done(root2, v))
        });
    }

    pub(super) fn remote_failed(&mut self, root: PathBuf, why: String, cx: &mut Context<Self>) {
        cx.emit(StoreEvent::Notice(why, true));
        self.refetch_server_mods(&root);
    }

    /// The panel's update check, in place of a local one.
    pub(super) fn check_remote_updates(&mut self, root: &Path, r: Remote, force: bool) {
        self.updates_at.insert(root.to_path_buf(), std::time::Instant::now());
        let state = self.updates.entry(root.to_path_buf()).or_default();
        let prev = state.ready().cloned().unwrap_or_default();
        state.begin();
        let root = root.to_path_buf();
        self.spawn(move || match updates_remote(&r, force) {
            Ok((list, archived, breaks)) => Msg::RemoteUpdates(root, list, archived, breaks),
            Err(e) => Msg::Updates { root, result: Err(format!("{e:#}")), prev },
        });
    }

    /// Update buttons on a Hopper Server: the panel downloads them. The
    /// offers the panel made go in one job; a version picked by hand (Versions,
    /// a roll back) goes as that file.
    pub(super) fn run_remote_updates(&self, root: &Path, updates: Vec<Update>) {
        let offered: HashMap<u64, u64> = self
            .updates(root)
            .and_then(|u| u.ready())
            .map(|l| l.iter().map(|u| (u.project_id, u.file.id)).collect())
            .unwrap_or_default();
        let (offers, picked): (Vec<Update>, Vec<Update>) =
            updates.into_iter().partition(|u| offered.get(&u.project_id) == Some(&u.file.id));
        for u in picked {
            let body = json!({ "file": u.from.trim_end_matches(".disabled"), "file_id": u.file.id.to_string() });
            let pid = u.project_id;
            self.remote_job(root, format!("Switching {} to {}", u.name, u.file.file_name), "/mods/version", body, move |root, _| {
                Msg::Updated(root, vec![pid], Vec::new(), None)
            });
        }
        if offers.is_empty() {
            return;
        }
        let updates = offers;
        let ids: Vec<String> = updates.iter().map(|u| u.project_id.to_string()).collect();
        let n = ids.len();
        let name = self.instance(root).map(|i| i.name.clone()).unwrap_or_default();
        self.remote_job(
            root,
            format!("Updating {n} mod{} on {name}", if n == 1 { "" } else { "s" }),
            "/mods/update",
            json!({ "project_ids": ids }),
            |root, v| {
                let done = v["updated_ids"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|i| i.as_str().and_then(|s| s.parse().ok()).or(i.as_u64())).collect())
                    .unwrap_or_default();
                Msg::Updated(root, done, Vec::new(), None)
            },
        );
    }

    /// Mods off or on, on a Hopper Server.
    pub(super) fn remote_set_enabled(&mut self, root: &Path, files: &[String], on: bool) {
        let want: HashSet<String> = files.iter().map(|f| f.to_lowercase()).collect();
        self.remote_post(root, "/mods/toggle-many", json!({ "files": bases(files), "enabled": on }), move |i| {
            for a in i.addons.iter_mut().filter(|a| want.contains(&a.file_name.to_lowercase())) {
                a.enabled = on;
            }
        });
    }

    pub(super) fn remote_set_locked(&mut self, root: &Path, files: &[String], locked: bool) {
        let want: HashSet<String> = files.iter().map(|f| f.to_lowercase()).collect();
        self.remote_post(root, "/mods/lock-many", json!({ "files": bases(files), "locked": locked }), move |i| {
            for a in i.addons.iter_mut().filter(|a| want.contains(&a.file_name.to_lowercase())) {
                a.locked = locked;
            }
        });
    }

    pub(super) fn remote_version_lock(&mut self, root: &Path, file: &str, version: Option<String>) {
        let (f, v) = (file.to_string(), version.clone());
        self.remote_post(root, "/mods/version-lock", json!({ "file": file.trim_end_matches(".disabled"), "version": version }), move |i| {
            if let Some(a) = i.addons.iter_mut().find(|a| a.file_name == f) {
                a.locked_version = v.unwrap_or_default();
            }
        });
    }

    /// Jars dropped on a server's Mods tab. On this PC they're added like on
    /// an instance (another version of a mod there is replaced, the old one
    /// kept in History); on a Hopper Server they're uploaded and the panel
    /// does the same.
    pub fn add_server_jars(&self, root: &Path, jars: Vec<PathBuf>) {
        let Some(r) = remote(root) else {
            // Another version of a mod already there: the new one replaces it.
            let old: Vec<String> = self
                .instance(root)
                .map(|inst| {
                    jars.iter()
                        .filter_map(|j| {
                            let stem = hopper::serverpack::jar_stem(&j.file_name()?.to_string_lossy());
                            inst.addons.iter().find(|a| a.folder() == "mods" && hopper::serverpack::jar_stem(&a.file_name) == stem).map(|a| a.file_name.clone())
                        })
                        .collect()
                })
                .unwrap_or_default();
            self.add_dropped(jars, Some(root.to_path_buf()), "mods".into(), Vec::new(), old);
            return;
        };
        let n = jars.len();
        let root2 = root.to_path_buf();
        self.spawn_instance_job(root, format!("Adding {n} mod{} to the server", if n == 1 { "" } else { "s" }), move |job| {
            let c = Client::for_remote(&r)?;
            for (i, j) in jars.iter().enumerate() {
                let name = j.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                job.detail(format!("Uploading {name} ({}/{n})", i + 1));
                match c.upload_mod(&r.id, j) {
                    Ok(true) => job.note(format!("{name} replaced an older version (kept in the server's backups)")),
                    Ok(false) => {}
                    Err(e) => job.note(format!("{name}: {e:#}")),
                }
            }
            Ok(Msg::InstanceChanged(root2))
        });
    }

    /// Switches a Hopper Server's mods to a saved profile.
    pub fn use_remote_profile(&mut self, root: &Path, name: &str) {
        self.remote_profile(root, "/mods/profiles/use", name);
    }

    pub(super) fn remote_profile(&mut self, root: &Path, rest: &'static str, name: &str) {
        self.remote_post(root, rest, json!({ "name": name }), |_| {});
    }
}

/// Removes mods from a Hopper Server; they go to its `.hopper-backups`.
/// Answers the token its Undo takes, if the panel gave one.
pub fn remote_remove(root: &Path, files: &[String]) -> anyhow::Result<Option<String>> {
    let r = remote(root).ok_or_else(|| anyhow::anyhow!("That server isn't on a Hopper Server."))?;
    let c = Client::for_remote(&r)?;
    let v = c.post(&Client::server_path(&r.id, "/mods/remove-many"), json!({ "files": bases(files) }))?;
    Ok(v["undo"].as_str().map(str::to_string))
}

/// Puts back what [`remote_remove`] took.
pub fn remote_undo_remove(root: &Path, token: &str) -> anyhow::Result<()> {
    let r = remote(root).ok_or_else(|| anyhow::anyhow!("That server isn't on a Hopper Server."))?;
    let c = Client::for_remote(&r)?;
    c.post(&Client::server_path(&r.id, "/mods/undo-remove"), json!({ "token": token }))?;
    Ok(())
}

/// A mod's config files on a Hopper Server (relative to the server folder).
pub fn remote_config_files(root: &Path, file: &str) -> anyhow::Result<Vec<String>> {
    let r = remote(root).ok_or_else(|| anyhow::anyhow!("That server isn't on a Hopper Server."))?;
    let c = Client::for_remote(&r)?;
    let path = format!("/mods/config?file={}", hopper::hopperd::enc(file.trim_end_matches(".disabled")));
    let v = c.get(&Client::server_path(&r.id, &path))?;
    Ok(v["files"].as_array().into_iter().flatten().filter_map(|f| f.as_str().map(str::to_string)).collect())
}

/// Moves a mod's config files to the Hopper Server's backups: how many.
pub fn remote_config_reset(root: &Path, file: &str) -> anyhow::Result<usize> {
    let r = remote(root).ok_or_else(|| anyhow::anyhow!("That server isn't on a Hopper Server."))?;
    let c = Client::for_remote(&r)?;
    let v = c.post(&Client::server_path(&r.id, "/mods/config-reset"), json!({ "file": file.trim_end_matches(".disabled") }))?;
    Ok(v["reset"].as_u64().unwrap_or(0) as usize)
}

// ---- mods from GitHub and other links, on a Hopper Server ----
//
// The jars are on the panel's machine, so the panel looks links up, reads
// the jars, downloads and keeps its own list of trusted publishers: nothing
// here reads them or downloads to this PC, and this PC's trust list isn't
// asked.

/// Files on each Hopper Server list whose source the panel hasn't trusted
/// yet, from the last fetch. A root with no entry: the panel didn't say.
static UNTRUSTED: std::sync::LazyLock<std::sync::Mutex<HashMap<PathBuf, HashSet<String>>>> =
    std::sync::LazyLock::new(Default::default);

fn note_untrusted(root: &Path, v: &Value) {
    let mut map = UNTRUSTED.lock().unwrap_or_else(|e| e.into_inner());
    match v["untrusted"].as_array() {
        Some(a) => {
            map.insert(root.to_path_buf(), a.iter().filter_map(|f| f.as_str()).map(str::to_lowercase).collect());
        }
        None => {
            map.remove(root);
        }
    }
}

fn panel_of(root: &Path) -> anyhow::Result<(Client, Remote)> {
    let r = remote(root).ok_or_else(|| anyhow::anyhow!("That server isn't on a Hopper Server."))?;
    Ok((Client::for_remote(&r)?, r))
}

fn source_kind(label: &str) -> hopper::sources::Kind {
    use hopper::sources::Kind;
    match label {
        "GitHub" => Kind::GitHub,
        "Maven" => Kind::Maven,
        "Jenkins" => Kind::Jenkins,
        _ => Kind::Url,
    }
}

/// The source in a panel's answer: whole from a newer panel, else rebuilt
/// from its kind, publisher and page.
fn source_of(v: &Value) -> hopper::sources::ModSource {
    if let Ok(s) = serde_json::from_value::<hopper::sources::ModSource>(v["source"].clone())
        && !s.publisher.is_empty()
    {
        return s;
    }
    let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    let kind = source_kind(&s("kind"));
    let page = s("page");
    let url = match kind {
        hopper::sources::Kind::GitHub => page.trim_start_matches("https://github.com/").to_string(),
        _ => page,
    };
    hopper::sources::ModSource { kind, url, publisher: s("publisher"), seen: s("seen"), ..Default::default() }
}

fn resolved_of(v: &Value) -> hopper::sources::Resolved {
    use hopper::sources::{Candidate, Fit};
    let s = |v: &Value, k: &str| v[k].as_str().unwrap_or_default().to_string();
    let candidates = v["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| Candidate {
            file_name: s(c, "file"),
            version: s(c, "version"),
            title: s(c, "title"),
            date: s(c, "date"),
            size: c["size"].as_u64().unwrap_or(0),
            author: s(c, "author"),
            prerelease: c["prerelease"].as_bool().unwrap_or(false),
            page: s(c, "page"),
            // An older panel only says whether it fits.
            fit: serde_json::from_value(c["fit"].clone()).unwrap_or_else(|_| {
                if c["good"].as_bool().unwrap_or(false) {
                    Fit::Good
                } else if c["fits"].as_bool().unwrap_or(false) {
                    Fit::Unknown
                } else {
                    Fit::Loader(s(c, "tag"))
                }
            }),
            ..Default::default()
        })
        .collect();
    hopper::sources::Resolved {
        source: source_of(v),
        name: s(v, "name"),
        candidates,
        pick: v["pick"].as_u64().map(|i| i as usize),
        ambiguous: v["ambiguous"].as_bool().unwrap_or(false),
    }
}

fn adoption_of(v: &Value) -> hopper::sources::Adoption {
    let s = |v: &Value, k: &str| v[k].as_str().unwrap_or_default().to_string();
    hopper::sources::Adoption {
        source: source_of(v),
        matched: serde_json::from_value(v["matched"].clone()).unwrap_or(hopper::sources::Matched::Unknown),
        newest: v["newest"].is_object().then(|| hopper::sources::Candidate {
            file_name: s(&v["newest"], "file"),
            version: s(&v["newest"], "version"),
            title: s(&v["newest"], "title"),
            ..Default::default()
        }),
        name: s(v, "name"),
    }
}

impl Store {
    /// Whether the panel allows mods from links (its owner's setting).
    pub fn remote_sources_allowed(root: &Path) -> anyhow::Result<bool> {
        let (c, _) = panel_of(root)?;
        Ok(c.get("/settings/sources")?["allowed"].as_bool().unwrap_or(false))
    }

    /// A link looked up by the panel for the server, and whether the panel
    /// trusts its publisher.
    pub fn remote_source_resolve(root: &Path, link: &str, pre: bool) -> anyhow::Result<(hopper::sources::Resolved, bool)> {
        let (c, r) = panel_of(root)?;
        let v = c.post(&Client::server_path(&r.id, "/mods/source/resolve"), json!({ "link": link, "prerelease": pre }))?;
        Ok((resolved_of(&v), v["trusted"].as_bool().unwrap_or(false)))
    }

    /// How an installed jar on the server would follow `link` (nothing
    /// changes yet), and whether the panel trusts its publisher.
    pub fn remote_source_plan(root: &Path, file: &str, link: &str, pre: bool) -> anyhow::Result<(hopper::sources::Adoption, bool)> {
        let (c, r) = panel_of(root)?;
        let body = json!({ "file": file.trim_end_matches(".disabled"), "link": link, "prerelease": pre });
        let v = c.post(&Client::server_path(&r.id, "/mods/source/plan"), body)?;
        Ok((adoption_of(&v), v["trusted"].as_bool().unwrap_or(false)))
    }

    /// The source a jar on the server links to in its own files.
    pub fn remote_source_suggest(root: &Path, file: &str) -> anyhow::Result<Option<String>> {
        let (c, r) = panel_of(root)?;
        let path = format!("/mods/source/suggest?file={}", hopper::hopperd::enc(file.trim_end_matches(".disabled")));
        Ok(c.get(&Client::server_path(&r.id, &path))?["link"].as_str().filter(|l| !l.is_empty()).map(str::to_string))
    }

    /// The server's unknown mods, each with the source its jar suggests.
    pub fn remote_source_unknown(root: &Path) -> anyhow::Result<Vec<hopper::sources::Unknown>> {
        let (c, r) = panel_of(root)?;
        let v = c.get(&Client::server_path(&r.id, "/mods/source/unknown"))?;
        Ok(serde_json::from_value(v["mods"].clone()).unwrap_or_default())
    }

    /// Whether a mod on a Hopper Server needs its source confirmed, as the
    /// panel said on the last fetch; `None` when it didn't say.
    pub fn remote_source_untrusted(root: &Path, file: &str) -> Option<bool> {
        let map = UNTRUSTED.lock().unwrap_or_else(|e| e.into_inner());
        map.get(root).map(|l| l.contains(&file.to_lowercase()))
    }

    /// Adds the file `file` from `link` to the server, or makes the
    /// installed `follow` follow it: the panel looks the link up again,
    /// trusts its publisher and downloads, as a job.
    pub fn remote_source_add(&self, root: &Path, title: String, link: &str, pre: bool, file: &str, follow: Option<String>) {
        let body = json!({
            "link": link, "prerelease": pre, "file": file,
            "follow": follow.map(|f| f.trim_end_matches(".disabled").to_string()),
        });
        self.remote_job(root, title, "/mods/source/add", body, |root, _| Msg::InstanceChanged(root));
    }

    /// Makes the installed jar follow `link`, the file left as it is.
    pub fn remote_source_adopt(&self, root: &Path, title: String, name: String, file: &str, link: &str, pre: bool) {
        let Some(r) = remote(root) else { return };
        let body = json!({ "file": file.trim_end_matches(".disabled"), "link": link, "prerelease": pre });
        let root2 = root.to_path_buf();
        self.spawn_instance_job(root, title, move |job| {
            let c = Client::for_remote(&r)?;
            let v = c.post(&Client::server_path(&r.id, "/mods/source/adopt"), body)?;
            let who = v["publisher"].as_str().unwrap_or_default();
            job.detail(match v["newest"]["version"].as_str() {
                Some(new) => format!("{name} follows {who}. {new} is out: it's in Updates."),
                None => format!("{name} follows {who}. It's up to date."),
            });
            Ok(Msg::InstanceChanged(root2))
        });
    }

    /// Follows sources for several unknown mods (file, link) in one panel job.
    pub fn remote_source_adopt_many(&self, root: &Path, picked: Vec<(String, String)>) {
        let n = picked.len();
        let items: Vec<Value> = picked
            .iter()
            .map(|(f, l)| json!({ "file": f.trim_end_matches(".disabled"), "link": l }))
            .collect();
        self.remote_job(root, format!("Finding sources for {n} mods"), "/mods/source/adopt-many", json!({ "items": items }), |root, _| {
            Msg::InstanceChanged(root)
        });
    }

    /// "Stop following" on a Hopper Server.
    pub fn remote_source_unfollow(&self, root: &Path, file: &str) {
        let Some(r) = remote(root) else { return };
        let (root2, file) = (root.to_path_buf(), file.trim_end_matches(".disabled").to_string());
        self.spawn_instance_job(root, format!("Stop following a source for {file}"), move |job| {
            let c = Client::for_remote(&r)?;
            c.post(&Client::server_path(&r.id, "/mods/source/unfollow"), json!({ "file": file }))?;
            job.detail(format!("{file} no longer follows its source."));
            Ok(Msg::InstanceChanged(root2))
        });
    }

    /// The panel trusts the source a mod came with (a pack or a sync), then
    /// checks the server's updates again so it shows there.
    pub fn remote_source_trust(&mut self, root: &Path, file: &str) {
        let Some(r) = remote(root) else { return };
        let prev = self.updates.get(root).and_then(|u| u.ready().cloned()).unwrap_or_default();
        let (root, file) = (root.to_path_buf(), file.trim_end_matches(".disabled").to_string());
        self.spawn(move || {
            let asked = Client::for_remote(&r).and_then(|c| c.post(&Client::server_path(&r.id, "/mods/source/trust"), json!({ "file": file })));
            if let Err(e) = asked {
                return Msg::RemoteFailed(root, format!("{e:#}"));
            }
            if let Some(l) = UNTRUSTED.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&root) {
                l.remove(&file.to_lowercase());
            }
            match updates_remote(&r, true) {
                Ok((list, archived, breaks)) => Msg::RemoteUpdates(root, list, archived, breaks),
                Err(e) => Msg::Updates { root, result: Err(format!("{e:#}")), prev },
            }
        });
    }
}

// ---- data packs on a Hopper Server ----
//
// The panel reads and changes the folders (`packs_where_api.rs` there) and
// sends the same one list desktop builds for a folder here; the Data packs
// table shows it under the server's stand-in root.

impl Store {
    /// The server's data packs as the panel lists them, each folder and copy
    /// under the stand-in `root`, and whether the server runs.
    pub fn remote_datapacks(root: &Path) -> anyhow::Result<(Vec<hopper::pack_where::Entry>, Vec<hopper::packs::Location>, bool)> {
        let (c, r) = panel_of(root)?;
        let v = c.get(&Client::server_path(&r.id, "/datapacks"))?;
        let (entries, locs) = hopper::pack_where::from_panel(root, &v)?;
        Ok((entries, locs, v["running"].as_bool().unwrap_or(false)))
    }

    /// Sends a data pack to `to` on the panel. Answers whether the server
    /// runs (it picks the change up after /reload or a restart).
    pub fn remote_datapack_where(root: &Path, file: &str, to: &hopper::pack_where::Target) -> anyhow::Result<bool> {
        let (c, r) = panel_of(root)?;
        let body = json!({ "file": file.trim_end_matches(".disabled"), "to": to });
        Ok(c.post(&Client::server_path(&r.id, "/datapacks/where"), body)?["reload"].as_bool().unwrap_or(false))
    }

    /// Paxi's load order on the panel (first loads first).
    pub fn remote_datapack_order(root: &Path, names: &[String]) -> anyhow::Result<bool> {
        let (c, r) = panel_of(root)?;
        Ok(c.post(&Client::server_path(&r.id, "/datapacks/order"), json!({ "names": names }))?["reload"].as_bool().unwrap_or(false))
    }

    /// Moves files under the stand-in `root` to the server's trash (Files ›
    /// Trash puts them back).
    pub fn remote_trash(root: &Path, paths: &[PathBuf]) -> anyhow::Result<()> {
        let (c, r) = panel_of(root)?;
        let rels: Vec<String> = paths
            .iter()
            .filter_map(|p| p.strip_prefix(root).ok())
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .filter(|p| !p.is_empty())
            .collect();
        if rels.is_empty() {
            return Ok(());
        }
        c.trash(&r.id, &rels)
    }
}
