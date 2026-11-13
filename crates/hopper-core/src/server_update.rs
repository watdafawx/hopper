//! Bringing a server up to its pack: NeoForge version and mods. Ported from
//! mod-sync's `loader.go`.
//!
//! The run script has one meaningful line:
//!   "C:\...\java.exe" @user_jvm_args.txt @libraries/net/neoforged/neoforge/21.1.248/win_args.txt nogui %*
//! A loader update installs the new version's libraries, then rewrites only
//! the version in that `@libraries/...` token, keeping the Java path and
//! program arguments.

use crate::instances::Instance;
use crate::jobs::Job;
use crate::server::{self, ServerInfo, Status};
use anyhow::bail;
use std::fs;
use std::path::{Path, PathBuf};

/// Files the NeoForge installer regenerates, clobbering custom settings.
const INSTALLER_REWRITES: [&str; 4] = ["run.bat", "run.sh", "run.cmd", "user_jvm_args.txt"];
/// Files the installer drops that are not part of a server.
const INSTALLER_JUNK: [&str; 2] = ["installer.log", "installer.log.gz"];

fn args_file(server_root: &Path, version: &str) -> PathBuf {
    server_root
        .join("libraries/net/neoforged/neoforge")
        .join(version)
        .join("win_args.txt")
}

/// The NeoForge version a server's run.bat points at.
pub fn server_neoforge(server_root: &Path) -> Option<String> {
    if let Some(r) = crate::hopperd::locate(server_root) {
        return r.and_then(|(c, id, _)| c.neoforge(&id)).ok().flatten();
    }
    let text = fs::read_to_string(server_root.join("run.bat")).ok()?;
    version_in(&text)
}

pub(crate) fn version_in(text: &str) -> Option<String> {
    let needle = "neoforge";
    for line in text.lines() {
        let l = line.replace('\\', "/");
        if let Some(i) = l.find("libraries/net/neoforged/neoforge/") {
            let rest = &l[i + "libraries/net/neoforged/".len() + needle.len() + 1..];
            if let Some(v) = rest.split('/').next().filter(|v| !v.is_empty()) {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Rewrites the version inside the neoforge args token only.
fn rewrite_version(text: &str, old: &str, new: &str) -> Option<String> {
    let mut changed = false;
    let lines: Vec<String> = text
        .split('\n')
        .map(|line| {
            let norm = line.replace('\\', "/");
            let token = format!("neoforged/neoforge/{old}/");
            match norm.find(&token) {
                Some(_) => {
                    changed = true;
                    // Keep the original separators: replace "<sep>old<sep>" after "neoforge".
                    let mut out = line.to_string();
                    for sep in ['/', '\\'] {
                        let from = format!("neoforge{sep}{old}{sep}");
                        let to = format!("neoforge{sep}{new}{sep}");
                        if out.contains(&from) {
                            out = out.replacen(&from, &to, 1);
                            break;
                        }
                    }
                    out
                }
                None => line.to_string(),
            }
        })
        .collect();
    changed.then(|| lines.join("\n"))
}

/// `text` with every NeoForge `old` path switched to `new`: the folder
/// (`neoforge/<v>/`, either slash) and the jar (`neoforge-<v>-server.jar`).
/// For a panel's own launch command and executable.
pub(crate) fn swap_neoforge(text: &str, old: &str, new: &str) -> String {
    let mut out = text.to_string();
    for sep in ['/', '\\'] {
        out = out.replace(&format!("neoforge{sep}{old}{sep}"), &format!("neoforge{sep}{new}{sep}"));
    }
    out.replace(&format!("neoforge-{old}-"), &format!("neoforge-{new}-"))
}

/// Points run.bat (and run.sh) at `new`, backing the old files up.
pub fn rewrite_run_scripts(server_root: &Path, old: &str, new: &str) -> anyhow::Result<()> {
    let mut any = false;
    for name in ["run.bat", "run.sh"] {
        let p = server_root.join(name);
        let Ok(text) = fs::read_to_string(&p) else {
            continue;
        };
        if let Some(new_text) = rewrite_version(&text, old, new) {
            let backup = server_root
                .join(crate::sync::BACKUP_DIR)
                .join(format!("{name}.{old}"));
            fs::create_dir_all(backup.parent().unwrap_or(server_root))?;
            fs::copy(&p, &backup)?;
            let tmp = p.with_extension("hopper-tmp");
            fs::write(&tmp, new_text)?;
            fs::rename(tmp, &p)?;
            any = true;
        }
    }
    anyhow::ensure!(any, "run.bat has no NeoForge {old} line to update");
    Ok(())
}

/// Installs NeoForge `version` into the server folder with its installer,
/// keeping the server's own launch files.
pub fn install_neoforge_server(
    server_root: &Path,
    version: &str,
    java: &Path,
    job: &Job,
) -> anyhow::Result<()> {
    crate::remotefs::local_only(server_root, "Installing NeoForge")?;
    if args_file(server_root, version).exists() {
        return Ok(());
    }
    let preserved: Vec<(PathBuf, Vec<u8>)> = INSTALLER_REWRITES
        .iter()
        .map(|n| server_root.join(n))
        .filter_map(|p| fs::read(&p).ok().map(|b| (p, b)))
        .collect();
    let junk_before: Vec<bool> = INSTALLER_JUNK
        .iter()
        .map(|n| server_root.join(n).exists())
        .collect();

    let installer = crate::paths::temp_dir().join(format!("hopper-neoforge-{version}-installer.jar"));
    job.detail(format!("Downloading the NeoForge {version} installer"));
    crate::cf::download(
        &format!(
            "https://maven.neoforged.net/releases/net/neoforged/neoforge/{version}/neoforge-{version}-installer.jar"
        ),
        &installer,
        |_| {},
    )?;
    let out = crate::loader_installer::run(
        java,
        &installer,
        &["--install-server".as_ref(), server_root.as_os_str()],
        server_root,
        &format!("Installing NeoForge {version} on the server"),
        job,
    );
    let _ = fs::remove_file(&installer);
    let out = out?;

    // Put back what the installer overwrote; drop its logs.
    for (p, bytes) in preserved {
        if fs::read(&p).ok().as_deref() != Some(&bytes[..]) {
            fs::write(&p, bytes)?;
        }
    }
    for (name, existed) in INSTALLER_JUNK.iter().zip(junk_before) {
        if !existed {
            let _ = fs::remove_file(server_root.join(name));
        }
    }
    if !out.status.success() || !args_file(server_root, version).exists() {
        bail!(
            "the NeoForge installer did not finish: {}",
            out.last_line
        );
    }
    Ok(())
}

/// What `update_server` did.
#[derive(Debug, Default)]
pub struct Report {
    pub loader: Option<(String, String)>,
    pub copied: usize,
    pub removed: usize,
    /// Mods turned off on the server (they're off in the pack).
    pub turned_off: usize,
    /// Mods turned on on the server (they're on in the pack).
    pub turned_on: usize,
    /// Each file that failed: which mod, which file, and why.
    pub failed: Vec<String>,
    /// What happened to the panel's own launch command, if anything.
    pub panel: Option<String>,
    /// The mods it changed, by name, for the summary.
    pub changes: Changes,
    /// Jars a Hopper Server downloaded itself, and ones Hopper uploaded
    /// to it (with their bytes).
    pub fetched: usize,
    pub uploaded: usize,
    pub uploaded_bytes: u64,
}

/// The mods a server sync changed, by name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Changes {
    pub added: Vec<String>,
    pub updated: Vec<String>,
    pub removed: Vec<String>,
    pub turned_off: Vec<String>,
    pub turned_on: Vec<String>,
}

impl Changes {
    /// "Added Create, Jade; updated JEI; removed Old Mod", or "" when
    /// nothing changed. Long lists name a few and count the rest.
    pub fn line(&self) -> String {
        let parts: Vec<String> = [
            ("added", &self.added),
            ("updated", &self.updated),
            ("removed", &self.removed),
            ("turned off", &self.turned_off),
            ("turned on", &self.turned_on),
        ]
        .into_iter()
        .filter(|(_, l)| !l.is_empty())
        .map(|(verb, l)| format!("{verb} {}", crate::words::names(l, 3)))
        .collect();
        let line = parts.join("; ");
        let mut c = line.chars();
        match c.next() {
            Some(f) => f.to_uppercase().chain(c).collect(),
            None => line,
        }
    }
}

impl Report {
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some((from, to)) = &self.loader {
            parts.push(format!("NeoForge {from} » {to}"));
        }
        let line = self.changes.line();
        if !line.is_empty() {
            parts.push(line);
        } else if self.failed.is_empty() {
            parts.push("Mods already match the pack".to_string());
        }
        if self.fetched > 0 {
            let mut t = format!("Hopper Server downloaded {} itself", crate::words::count(self.fetched, "file"));
            if self.uploaded > 0 {
                t.push_str(&format!(", {} uploaded ({})", self.uploaded, crate::instance_ops::human_bytes(self.uploaded_bytes)));
            }
            parts.push(t);
        }
        if !self.failed.is_empty() {
            parts.push(format!("{} failed", crate::words::count(self.failed.len(), "file")));
        }
        if let Some(p) = &self.panel {
            parts.push(p.clone());
        }
        parts.join(" · ")
    }
}

/// A file's key in [`owners`]: its name in lower case, without ".disabled".
fn file_key(p: &Path) -> String {
    let n = p.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
    n.strip_suffix(".disabled").map(str::to_string).unwrap_or(n)
}

/// The comparison row each file of a sync belongs to, by [`file_key`]: the
/// pack's jar and the server's old one both point at their mod.
fn owners(diff: &[crate::model::DiffEntry]) -> std::collections::HashMap<String, usize> {
    let mut m = std::collections::HashMap::new();
    for (i, d) in diff.iter().enumerate() {
        for f in [&d.client, &d.server].into_iter().flatten() {
            m.insert(file_key(Path::new(&f.file_name)), i);
        }
    }
    m
}

fn op_path(op: &crate::model::SyncOp) -> &Path {
    use crate::model::SyncOp;
    match op {
        SyncOp::Copy { dst, .. } => dst,
        SyncOp::Delete { path } | SyncOp::Disable { path } | SyncOp::Enable { path } => path,
    }
}

/// What a running server sync is doing, in a player's words:
/// "Uploading mods to the server · 3 of 7 files". `from`: a Hopper Server
/// is downloading the file itself.
pub fn step_line(op: &crate::model::SyncOp, remote: bool, from: Option<&str>, index: usize, count: usize) -> String {
    use crate::model::SyncOp;
    let what = match op {
        SyncOp::Copy { .. } if from.is_some() => "Hopper Server is downloading mods",
        SyncOp::Copy { .. } if remote => "Uploading mods to the server",
        SyncOp::Copy { .. } => "Copying mods to the server",
        SyncOp::Delete { .. } => "Removing old mods",
        SyncOp::Disable { .. } => "Turning mods off",
        SyncOp::Enable { .. } => "Turning mods on",
    };
    format!("{what} · {} of {}", index + 1, crate::words::count(count, "file"))
}

/// A server sync's copies as they go, for the line under its step: which
/// the server downloads itself, and the bytes and speed of the uploads.
#[derive(Default)]
struct Watch {
    /// By operation: the host the server downloads it from, or `None` for an upload.
    from: std::collections::HashMap<usize, Option<&'static str>>,
    /// Bytes sent of each upload.
    sent: std::collections::HashMap<usize, u64>,
    /// Seconds spent on uploads before the current one.
    secs: f64,
    /// The upload under way: its operation, when it began, where the run stood.
    now: Option<(usize, std::time::Instant, u64)>,
}

impl Watch {
    /// "From Modrinth", or "3 MB of 8 MB · 210 KB/s · about 24 s left": the
    /// speed is the average over this run's uploads, steady enough for a
    /// rough time left.
    fn step(&mut self, s: &crate::sync::Step, op: &crate::model::SyncOp) -> String {
        self.from.insert(s.index, s.from);
        if self.now.is_some_and(|n| n.0 != s.index || s.from.is_some()) {
            let (_, t, _) = self.now.take().unwrap_or((0, std::time::Instant::now(), 0));
            self.secs += t.elapsed().as_secs_f64();
        }
        if let Some(from) = s.from {
            return format!("Hopper Server is downloading it from {from}");
        }
        let (_, t, at) = *self.now.get_or_insert((s.index, std::time::Instant::now(), s.done));
        let sent = s.done.saturating_sub(at);
        self.sent.insert(s.index, sent);
        let secs = self.secs + t.elapsed().as_secs_f64();
        let bytes: u64 = self.sent.values().sum();
        let speed = if secs >= 1.0 { bytes as f64 / secs } else { 0.0 };
        crate::words::transfer(sent, crate::sync::op_weight(op), speed)
    }
}

/// The mod (name, icon, project) a sync's file belongs to, else the file's name.
fn mod_of(diff: &[crate::model::DiffEntry], owners: &std::collections::HashMap<String, usize>, p: &Path) -> (String, String, u64) {
    let file = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let Some(d) = owners.get(&file_key(p)).and_then(|&i| diff.get(i)) else {
        return (file, String::new(), 0);
    };
    let info = d.client.as_ref().or(d.server.as_ref());
    let name = if d.mod_name.is_empty() { file } else { d.mod_name.clone() };
    (name, info.map(|m| m.icon.clone()).unwrap_or_default(), info.map_or(0, |m| m.project_id))
}

/// Sorts a sync's results into the mods it added, updated, removed and
/// turned off or on (one mod's old jar going and new jar coming is one
/// update), and a line for each file that failed.
fn tally(diff: &[crate::model::DiffEntry], results: &[crate::model::SyncResult], remote: bool) -> (Changes, Vec<String>) {
    use crate::model::SyncOp;
    let owners = owners(diff);
    // Per comparison row: copied, removed, turned off, turned on.
    let mut did = vec![[false; 4]; diff.len()];
    let mut loose = Changes::default();
    let mut failed = Vec::new();
    for r in results {
        let p = op_path(&r.op);
        let (name, _, _) = mod_of(diff, &owners, p);
        let slot = match &r.op {
            SyncOp::Copy { .. } => 0,
            SyncOp::Delete { .. } => 1,
            SyncOp::Disable { .. } => 2,
            SyncOp::Enable { .. } => 3,
        };
        if !r.ok {
            let verb = match &r.op {
                SyncOp::Copy { .. } if remote => "upload",
                SyncOp::Copy { .. } => "copy",
                SyncOp::Delete { .. } => "remove",
                SyncOp::Disable { .. } => "turn off",
                SyncOp::Enable { .. } => "turn on",
            };
            let file = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let what = if name == file { name.clone() } else { format!("{name} ({file})") };
            failed.push(format!("Couldn't {verb} {what}: {}", r.error.as_deref().unwrap_or("no reason given")));
            continue;
        }
        match owners.get(&file_key(p)) {
            Some(&i) => did[i][slot] = true,
            // A file the comparison doesn't list: named by its file.
            None => [&mut loose.added, &mut loose.removed, &mut loose.turned_off, &mut loose.turned_on][slot].push(name),
        }
    }
    let mut c = Changes::default();
    for (d, [copied, removed, off, on]) in diff.iter().zip(did) {
        let name = match d.client.as_ref().or(d.server.as_ref()) {
            Some(m) if d.mod_name.is_empty() => m.file_name.clone(),
            _ => d.mod_name.clone(),
        };
        if copied {
            if d.server.is_some() { c.updated.push(name) } else { c.added.push(name) }
        } else if removed {
            c.removed.push(name);
        } else if off {
            c.turned_off.push(name);
        } else if on {
            c.turned_on.push(name);
        }
    }
    c.added.append(&mut loose.added);
    c.removed.append(&mut loose.removed);
    c.turned_off.append(&mut loose.turned_off);
    c.turned_on.append(&mut loose.turned_on);
    (c, failed)
}

/// Points the panel's own launch command for `server` (Crafty's Server
/// Execution Command) at NeoForge `want`. What happened, if anything.
pub fn point_panel(
    server: &ServerInfo,
    want: &str,
    panel: &dyn crate::panel::ServerPanel,
    job: &Job,
) -> Option<String> {
    job.detail(format!("Checking {}'s launch command", panel.name()));
    let r = panel.servers().and_then(|list| match crate::panel::match_server(&list, server) {
        Some(ps) => panel.use_neoforge(&ps.id, want),
        None => Ok(None),
    });
    match r {
        Ok(msg) => msg,
        Err(e) => Some(format!(
            "{0}'s launch command still names the old NeoForge ({e:#}). Change it to {want} in {0}",
            panel.name()
        )),
    }
}

/// Switches `server` to NeoForge `want` (any version, older or newer):
/// installs it, points run.bat at it, and the panel's launch command too.
/// Mods are left alone. Refuses while the server answers a ping.
pub fn set_server_neoforge(
    server: &ServerInfo,
    want: &str,
    host: &str,
    java: &Path,
    panel: Option<&dyn crate::panel::ServerPanel>,
    job: &Job,
) -> anyhow::Result<String> {
    // Its folder is on the other PC: the panel installs it there.
    if let Some(r) = &server.remote {
        return set_remote_neoforge(r, want, job);
    }
    crate::remotefs::local_only(&server.root, "Changing NeoForge")?;
    if matches!(server::ping(host, server.port()), Status::Online { .. }) {
        bail!("{} is running. Stop it first.", server.name);
    }
    let have = server_neoforge(&server.root)
        .ok_or_else(|| anyhow::anyhow!("run.bat doesn't name a NeoForge version"))?;
    let mut parts = Vec::new();
    if have != want {
        install_neoforge_server(&server.root, want, java, job)?;
        rewrite_run_scripts(&server.root, &have, want)?;
        parts.push(format!("NeoForge {have} » {want}"));
    }
    if let Some(p) = panel.and_then(|p| point_panel(server, want, p, job)) {
        parts.push(p);
    }
    Ok(if parts.is_empty() {
        format!("Already on NeoForge {want}")
    } else {
        parts.join(" · ")
    })
}

/// Switches a server on a Hopper Server to NeoForge `want` with the panel's
/// own version change. Its folder is on the server's PC, so the installer
/// has to run there; the panel keeps the old start files for Undo (on its
/// Version page) and refuses while it's running.
pub fn set_remote_neoforge(r: &crate::hopperd::Remote, want: &str, job: &Job) -> anyhow::Result<String> {
    use crate::hopperd::Client;
    use crate::hosting::version_change::{Installed, Record};
    let c = Client::for_remote(r)?;
    job.detail("Asking Hopper Server what the server runs");
    let v = c.get(&Client::server_path(&r.id, "/version"))?;
    let have: Installed = serde_json::from_value(v["installed"].clone()).unwrap_or_default();
    if have.kind != "neoforge" {
        bail!("Hopper Server doesn't see NeoForge on this server, so it can't switch it.");
    }
    if have.version == want {
        return Ok(format!("Already on NeoForge {want}"));
    }
    job.detail(format!("Hopper Server is installing NeoForge {want}"));
    let done = c.run_job(
        &Client::server_path(&r.id, "/version/change"),
        serde_json::json!({ "minecraft": have.minecraft, "loader_version": want }),
        job,
    )?;
    let rec: Record = serde_json::from_value(done).unwrap_or_default();
    let mut parts = vec![format!("NeoForge {} » {want}", have.version)];
    parts.extend(rec.checks.iter().filter(|c| !c.ok).map(|c| c.text.clone()));
    parts.push("Undo in Hopper Server".to_string());
    Ok(parts.join(" · "))
}

/// Brings `server` up to `inst`: loader first, then mods with the usual
/// skip rules. Mods can change while it runs (they load on its restart);
/// a NeoForge change can't, so that refuses while the server answers a
/// ping. A `panel` that keeps
/// its own launch command (Crafty's Server Execution Command) is pointed at
/// the new loader too, or it would keep starting the old one.
pub fn update_server(
    server: &ServerInfo,
    inst: &Instance,
    host: &str,
    sync_anyway: &std::collections::HashSet<u64>,
    hidden: &std::collections::HashSet<String>,
    java: &Path,
    panel: Option<&dyn crate::panel::ServerPanel>,
    job: &Job,
) -> anyhow::Result<Report> {
    crate::remotefs::local_only(&server.root, "Updating the server's pack")?;
    let new_loader = server.remote.is_none()
        && matches!(
            (inst.loader.strip_prefix("neoforge-"), server_neoforge(&server.root)),
            (Some(want), Some(have)) if want != have
        );
    if new_loader && matches!(server::ping(host, server.port()), Status::Online { .. }) {
        bail!("{} is running and needs a new NeoForge. Stop it first.", server.name);
    }
    let mut report = Report::default();

    // Loader. A server on a Hopper Server changes it there (its panel
    // keeps its own launch line in step), and refuses while it runs.
    if let (Some(r), Some(want)) = (&server.remote, inst.loader.strip_prefix("neoforge-")) {
        let have = server_neoforge(&server.root);
        if have.as_deref().is_some_and(|h| h != want) {
            set_remote_neoforge(r, want, job)?;
            report.loader = have.map(|h| (h, want.to_string()));
        }
    } else if let (Some(want), Some(have)) = (
        inst.loader.strip_prefix("neoforge-"),
        server_neoforge(&server.root),
    ) && want != have
    {
        install_neoforge_server(&server.root, want, java, job)?;
        rewrite_run_scripts(&server.root, &have, want)?;
        report.loader = Some((have, want.to_string()));
    }
    // The panel's copy of the launch line, checked even when run.bat was
    // already right (an earlier update, or a hand edit).
    if let (Some(want), Some(panel)) = (inst.loader.strip_prefix("neoforge-"), panel.filter(|_| server.remote.is_none())) {
        report.panel = point_panel(server, want, panel, job);
    }

    // Mods.
    job.detail("Comparing the pack's mods with the server's");
    // The same comparison the banner and the Sync tab show.
    let (client, _) = crate::rigsync::client_mods(inst, sync_anyway)?;
    // Read by the panel when the server is on a Hopper Server. The ones
    // turned off there count: the server follows the pack's on / off too.
    let theirs = crate::rigsync::server_mods_with_off(server)?;
    let mut diff = crate::rigsync::diff_mods(&client, &theirs);
    // "Never sync" mods: neither copied nor removed.
    diff.retain(|d| !crate::rigsync::is_hidden(d, hidden));
    // Mods only the server has that keep the pack from joining go too (to
    // the backups or Hopper Server's trash); ones turned off in the pack
    // are turned off there.
    for d in &mut diff {
        if crate::rigsync::blocks_join(d, hidden) {
            d.action = crate::model::DiffAction::DeleteFromServer;
        }
    }
    let ops = crate::sync::plan(&diff, &inst.mods_dir(), &server.mods_dir());
    job.detail(format!("Syncing {}", crate::words::count(ops.len(), "file")));
    if let Some(r) = &server.remote {
        crate::hopperd::register(&server.root, r);
    }
    // Over the network (a Hopper Server, SFTP / FTP) a copy is an upload.
    let remote = server.remote.is_some() || crate::remotefs::mirror_of(&server.root).is_some();
    // A Hopper Server downloads the jars that have a public link itself:
    // its connection is usually far faster than this PC's upload.
    let links = if server.remote.is_some() {
        let jars: Vec<PathBuf> = ops
            .iter()
            .filter_map(|op| match op {
                crate::model::SyncOp::Copy { src, .. } => Some(src.clone()),
                _ => None,
            })
            .collect();
        if jars.is_empty() {
            Default::default()
        } else {
            job.detail("Looking up where the server can download the mods");
            crate::modfetch::links(inst, &jars)
        }
    } else {
        Default::default()
    };
    let who = owners(&diff);
    let watch = std::sync::Mutex::new(Watch::default());
    let results = crate::sync::execute_links(ops, &server.root, &links, &|s| {
        job.progress(s.done as usize, s.total as usize);
        let Some(op) = s.op else {
            job.item(None);
            return;
        };
        job.detail(step_line(op, remote, s.from, s.index, s.count));
        let (name, icon, project) = mod_of(&diff, &who, op_path(op));
        let info = match op {
            crate::model::SyncOp::Copy { .. } => watch.lock().unwrap_or_else(|e| e.into_inner()).step(&s, op),
            _ => op_path(op).file_name().unwrap_or_default().to_string_lossy().into_owned(),
        };
        job.item(Some(crate::jobs::JobItem { name, icon, project, info }));
    });
    let w = watch.into_inner().unwrap_or_else(|e| e.into_inner());
    for (i, _) in results.iter().enumerate().filter(|(_, r)| r.ok && matches!(r.op, crate::model::SyncOp::Copy { .. })) {
        match w.from.get(&i) {
            Some(Some(_)) => report.fetched += 1,
            Some(None) if server.remote.is_some() => {
                report.uploaded += 1;
                report.uploaded_bytes += w.sent.get(&i).copied().unwrap_or(0);
            }
            _ => {}
        }
    }
    job.detail("Saving the changes to History");
    crate::history::record(&server.root, "server", &server.root, &results);
    // The jars' CurseForge / Modrinth records go along, as with Sync to
    // server, so the server lists them as those mods.
    crate::sources::carry::carry_ids(&inst.root, &server.root, &crate::sources::carry::copied_jars(&results, &server.root));
    let (changes, failed) = tally(&diff, &results, remote);
    report.changes = changes;
    report.failed = failed;
    for r in results.iter().filter(|r| r.ok) {
        match &r.op {
            crate::model::SyncOp::Copy { .. } => report.copied += 1,
            crate::model::SyncOp::Delete { .. } => report.removed += 1,
            crate::model::SyncOp::Disable { .. } => report.turned_off += 1,
            crate::model::SyncOp::Enable { .. } => report.turned_on += 1,
        }
    }
    Ok(report)
}

#[cfg(test)]
mod swap_tests {
    #[test]
    fn swaps_panel_command_and_jar() {
        let cmd = r#""G:\jdk\bin\java.exe" @user_jvm_args.txt @libraries/net/neoforged/neoforge/21.1.256/win_args.txt nogui  %*"#;
        let out = super::swap_neoforge(cmd, "21.1.256", "21.1.260");
        assert!(out.contains("neoforge/21.1.260/win_args.txt"));
        assert!(out.starts_with(r#""G:\jdk\bin\java.exe" @user_jvm_args.txt"#));
        let exe = "libraries/net/neoforged/neoforge/21.1.230/neoforge-21.1.230-server.jar";
        assert_eq!(
            super::swap_neoforge(exe, "21.1.230", "21.1.260"),
            "libraries/net/neoforged/neoforge/21.1.260/neoforge-21.1.260-server.jar"
        );
        let back = r"libraries\net\neoforged\neoforge\21.1.230\win_args.txt";
        assert!(super::swap_neoforge(back, "21.1.230", "21.1.260").contains(r"neoforge\21.1.260\win"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BAT: &str = "@echo off\r\nREM Forge requires...\r\n\"G:\\graalvm\\bin\\java.exe\" @user_jvm_args.txt @libraries/net/neoforged/neoforge/21.1.256/win_args.txt nogui%*\r\npause\r\n";

    #[test]
    fn reads_and_rewrites_the_version_token_only() {
        assert_eq!(version_in(BAT).as_deref(), Some("21.1.256"));
        let out = rewrite_version(BAT, "21.1.256", "21.1.260").unwrap();
        assert!(out.contains("neoforge/21.1.260/win_args.txt"));
        assert!(out.contains("\"G:\\graalvm\\bin\\java.exe\""));
        assert!(out.contains("nogui%*"));
        assert!(rewrite_version(BAT, "9.9.9", "1").is_none());
    }

    fn jar(file: &str) -> crate::model::ModInfo {
        crate::model::ModInfo { file_name: file.into(), icon: format!("https://icons/{file}"), ..Default::default() }
    }

    fn row(name: &str, client: Option<&str>, server: Option<&str>) -> crate::model::DiffEntry {
        crate::model::DiffEntry {
            mod_name: name.into(),
            client: client.map(jar),
            server: server.map(jar),
            status: crate::model::DiffStatus::ClientNewer,
            action: crate::model::DiffAction::CopyToServer,
            skip_reason: None,
        }
    }

    fn done(op: crate::model::SyncOp, error: Option<&str>) -> crate::model::SyncResult {
        crate::model::SyncResult { op, ok: error.is_none(), error: error.map(str::to_string), backup: None }
    }

    #[test]
    fn a_sync_names_what_it_added_updated_removed_and_what_failed() {
        use crate::model::SyncOp;
        let m = |f: &str| PathBuf::from("mods").join(f);
        let diff = vec![
            row("Create", Some("create-6.jar"), Some("create-5.jar")),
            row("Jade", Some("jade-2.jar"), None),
            row("Old Mod", None, Some("old.jar")),
            row("JEI", Some("jei-9.jar"), None),
            row("Sodium", Some("sodium.jar"), Some("sodium.jar.disabled")),
        ];
        let results = vec![
            done(SyncOp::Delete { path: m("create-5.jar") }, None),
            done(SyncOp::Copy { src: m("create-6.jar"), dst: m("create-6.jar") }, None),
            done(SyncOp::Copy { src: m("jade-2.jar"), dst: m("jade-2.jar") }, None),
            done(SyncOp::Delete { path: m("old.jar") }, None),
            done(SyncOp::Copy { src: m("jei-9.jar"), dst: m("jei-9.jar") }, Some("disk full")),
            done(SyncOp::Enable { path: m("sodium.jar") }, None),
            done(SyncOp::Copy { src: m("custom.jar"), dst: m("custom.jar") }, None),
        ];
        let (c, failed) = tally(&diff, &results, true);
        assert_eq!(c.added, ["Jade", "custom.jar"]);
        assert_eq!(c.updated, ["Create"]);
        assert_eq!(c.removed, ["Old Mod"]);
        assert_eq!(c.turned_on, ["Sodium"]);
        assert_eq!(failed, ["Couldn't upload JEI (jei-9.jar): disk full"]);
        assert_eq!(c.line(), "Added Jade, custom.jar; updated Create; removed Old Mod; turned on Sodium");
    }

    #[test]
    fn the_summary_says_what_the_server_downloaded_and_what_failed() {
        let mut r = Report {
            loader: None,
            copied: 0,
            removed: 0,
            turned_off: 0,
            turned_on: 0,
            failed: vec![],
            panel: None,
            changes: Changes::default(),
            fetched: 0,
            uploaded: 0,
            uploaded_bytes: 0,
        };
        assert_eq!(r.summary(), "Mods already match the pack");
        r.changes.added = vec!["Jade".into()];
        r.fetched = 5;
        r.uploaded = 2;
        r.uploaded_bytes = 3 * 1024 * 1024;
        r.failed = vec!["Couldn't upload JEI (jei-9.jar): disk full".into()];
        assert_eq!(r.summary(), "Added Jade · Hopper Server downloaded 5 files itself, 2 uploaded (3 MB) · 1 file failed");
    }

    #[test]
    fn step_lines_say_what_and_how_far() {
        use crate::model::SyncOp;
        let copy = SyncOp::Copy { src: "a.jar".into(), dst: "a.jar".into() };
        assert_eq!(step_line(&copy, true, None, 2, 7), "Uploading mods to the server · 3 of 7 files");
        assert_eq!(step_line(&copy, true, Some("Modrinth"), 0, 7), "Hopper Server is downloading mods · 1 of 7 files");
        assert_eq!(step_line(&copy, false, None, 0, 1), "Copying mods to the server · 1 of 1 file");
        assert_eq!(step_line(&SyncOp::Delete { path: "a.jar".into() }, true, None, 4, 7), "Removing old mods · 5 of 7 files");
    }

    #[test]
    fn backslash_tokens_too() {
        let sh = "java @libraries\\net\\neoforged\\neoforge\\21.1.256\\win_args.txt";
        let out = rewrite_version(sh, "21.1.256", "21.1.260").unwrap();
        assert!(out.contains("neoforge\\21.1.260\\win_args"));
    }
}

/// Installs a NeoForge server into a temp folder and checks the launch
/// files survive. `cargo test real_neoforge_server -- --ignored --nocapture`
#[cfg(test)]
#[test]
#[ignore]
fn real_neoforge_server() {
    let root = std::env::temp_dir().join("hopper-nfserver");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let bat = "\"C:/custom/java.exe\" @user_jvm_args.txt @libraries/net/neoforged/neoforge/21.1.250/win_args.txt nogui %*\r\n";
    fs::write(root.join("run.bat"), bat).unwrap();
    fs::write(root.join("user_jvm_args.txt"), "-Xmx12G\n").unwrap();
    let java = crate::java::detect(Path::new(r"G:\Games\Minecraft\Curse\Install"))
        .into_iter()
        .find(|j| j.major == 21)
        .unwrap()
        .exe;
    let jobs = crate::jobs::Jobs::default();
    install_neoforge_server(&root, "21.1.256", &java, &jobs.start("install")).unwrap();
    assert!(args_file(&root, "21.1.256").exists());
    assert_eq!(
        fs::read_to_string(root.join("run.bat")).unwrap(),
        bat,
        "run.bat must be ours, not the installer's"
    );
    assert_eq!(
        fs::read_to_string(root.join("user_jvm_args.txt")).unwrap(),
        "-Xmx12G\n"
    );
    rewrite_run_scripts(&root, "21.1.250", "21.1.256").unwrap();
    assert_eq!(server_neoforge(&root).as_deref(), Some("21.1.256"));
    println!(
        "installed and rewired: {}",
        fs::read_to_string(root.join("run.bat")).unwrap().trim()
    );
    let _ = fs::remove_dir_all(&root);
}

/// Against a scratch Hopper Server (never a real one): switches its server
/// to another NeoForge build through the panel, then runs a full update
/// with an empty instance. `HOPPER_REMOTE_LOADER="<base>|<id>|<ca.pem>|<version>"`,
/// the token in `HOPPER_PANEL_TOKEN`.
/// `cargo test remote_loader_live -- --ignored --nocapture`
#[cfg(test)]
#[test]
#[ignore]
fn remote_loader_live() {
    let Ok(spec) = std::env::var("HOPPER_REMOTE_LOADER") else { return };
    let p: Vec<&str> = spec.split('|').collect();
    let ca = fs::read_to_string(p[2]).unwrap();
    let remote = crate::hopperd::Remote { base: p[0].into(), id: p[1].into(), ca, panel: String::new() };
    let root = crate::hopperd::panel_root("", p[1]);
    crate::hopperd::register(&root, &remote);
    let server = ServerInfo { name: p[1].into(), root, remote: Some(remote.clone()), ..crate::server::load(Path::new("")) };
    let jobs = crate::jobs::Jobs::default();
    let job = jobs.start("loader");
    println!("before: {:?}", server_neoforge(&server.root));
    let r = set_server_neoforge(&server, p[3], "127.0.0.1", Path::new("java"), None, &job);
    println!("set_server_neoforge: {r:?}");
    let inst_root = std::env::temp_dir().join("hopper-remote-loader-inst");
    fs::create_dir_all(inst_root.join("mods")).unwrap();
    let inst = Instance { root: inst_root, loader: format!("neoforge-{}", p[3]), ..Default::default() };
    let u = update_server(&server, &inst, "127.0.0.1", &Default::default(), &Default::default(), Path::new("java"), None, &job);
    println!("update_server: {:?}", u.map(|r| r.summary()));
    println!("after: {:?}", server_neoforge(&server.root));
}
