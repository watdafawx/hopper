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
    pub failed: Vec<String>,
    /// What happened to the panel's own launch command, if anything.
    pub panel: Option<String>,
}

impl Report {
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some((from, to)) = &self.loader {
            parts.push(format!("NeoForge {from} » {to}"));
        }
        parts.push(format!(
            "{} mods copied, {} removed",
            self.copied, self.removed
        ));
        if !self.failed.is_empty() {
            parts.push(format!("{} failed", self.failed.len()));
        }
        if let Some(p) = &self.panel {
            parts.push(p.clone());
        }
        parts.join(" · ")
    }
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
/// skip rules. Refuses while the server answers a ping. A `panel` that keeps
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
    if server.remote.is_none() && matches!(server::ping(host, server.port()), Status::Online { .. }) {
        bail!("{} is running. Stop it in Crafty/MCSS first.", server.name);
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
    job.detail("Comparing mods");
    let mut client = crate::scan::scan_folder(&inst.mods_dir())?;
    // Read by the panel when the server is on a Hopper Server.
    let theirs = crate::rigsync::server_mods(server)?;
    crate::serverpack::annotate(&mut client, inst, sync_anyway);
    let mut diff = crate::diff::compute_diff(&client, &theirs);
    // "Never sync" mods: neither copied nor removed.
    diff.retain(|d| !crate::rigsync::is_hidden(d, hidden));
    let ops = crate::sync::plan(&diff, &inst.mods_dir(), &server.mods_dir());
    job.detail(format!("Syncing {} files", ops.len()));
    let results = crate::sync::execute(ops, &server.root);
    crate::history::record(&server.root, "server", &server.root, &results);
    for r in results {
        match (&r.op, r.ok) {
            (crate::model::SyncOp::Copy { .. }, true) => report.copied += 1,
            (crate::model::SyncOp::Delete { .. }, true) => report.removed += 1,
            (op, false) => report
                .failed
                .push(format!("{op:?}: {}", r.error.unwrap_or_default())),
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
