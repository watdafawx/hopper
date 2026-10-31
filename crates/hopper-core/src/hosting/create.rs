//! Making a server folder: the loader's server files, then eula.txt,
//! server.properties, user_jvm_args.txt and run.bat. Or from a client
//! instance: its loader, then the mods that belong on a server and its
//! config.

use super::jvm::{JvmArgs, JvmPreset};
use super::script::{LaunchTarget, console_java, write_run_bat};
use crate::instances::Instance;
use crate::jobs::Job;
use crate::model::{DiffAction, ModInfo};
use anyhow::{Context as _, bail};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

const MOJANG_MANIFEST: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const FABRIC_META: &str = "https://meta.fabricmc.net/v2";
const QUILT_META: &str = "https://meta.quiltmc.org/v3";
const PAPER_API: &str = "https://fill.papermc.io/v3/projects/paper";
const PURPUR_API: &str = "https://api.purpurmc.org/v2/purpur";
const FORGE_PROMOS: &str = "https://files.minecraftforge.net/net/minecraftforge/forge/promotions_slim.json";

/// The server software to install.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerLoader {
    Vanilla,
    /// Loader version; `None` picks the newest stable one.
    Fabric(Option<String>),
    /// Loader version; `None` picks the newest non-beta one.
    Quilt(Option<String>),
    /// e.g. "21.1.256".
    NeoForge(String),
    /// Forge's own version without the game version, e.g. "47.3.0".
    Forge(String),
    /// Paper build number ("133"); `None` picks the newest stable one.
    Paper(Option<String>),
    /// Purpur build number ("2329"); `None` picks the newest one.
    Purpur(Option<String>),
}

impl ServerLoader {
    /// From an instance's loader id ("neoforge-21.1.256", "forge-47.3.0",
    /// "fabric-0.16.5", "quilt-0.26.4", "" for vanilla).
    pub fn from_instance(loader: &str) -> anyhow::Result<Self> {
        let l = loader.trim();
        let low = l.to_ascii_lowercase();
        let ver = |p: &str| l[p.len()..].to_string();
        Ok(if l.is_empty() {
            ServerLoader::Vanilla
        } else if low.starts_with("neoforge-") {
            ServerLoader::NeoForge(ver("neoforge-"))
        } else if low.starts_with("forge-") {
            ServerLoader::Forge(ver("forge-"))
        } else if low.starts_with("fabric-") {
            ServerLoader::Fabric(Some(ver("fabric-")))
        } else if low.starts_with("quilt-") {
            ServerLoader::Quilt(Some(ver("quilt-")))
        } else if low.starts_with("paper-") {
            ServerLoader::Paper(Some(ver("paper-")).filter(|v| !v.is_empty()))
        } else if low.starts_with("purpur-") {
            ServerLoader::Purpur(Some(ver("purpur-")).filter(|v| !v.is_empty()))
        } else {
            bail!("Hopper can't host a server for loader {loader}")
        })
    }

    pub fn name(&self) -> &'static str {
        match self {
            ServerLoader::Vanilla => "Vanilla",
            ServerLoader::Fabric(_) => "Fabric",
            ServerLoader::Quilt(_) => "Quilt",
            ServerLoader::NeoForge(_) => "NeoForge",
            ServerLoader::Forge(_) => "Forge",
            ServerLoader::Paper(_) => "Paper",
            ServerLoader::Purpur(_) => "Purpur",
        }
    }

    /// Runs mods (more memory by default, modded server.properties).
    pub fn is_modded(&self) -> bool {
        !matches!(self, ServerLoader::Vanilla | ServerLoader::Paper(_) | ServerLoader::Purpur(_))
    }

    /// Runs Bukkit/Spigot/Paper plugins from a `plugins` folder.
    pub fn runs_plugins(&self) -> bool {
        matches!(self, ServerLoader::Paper(_) | ServerLoader::Purpur(_))
    }
}

/// What [`create_server`] makes.
#[derive(Clone, Debug)]
pub struct CreateOptions {
    /// Folder name (made folder-safe) under `servers_root`.
    pub name: String,
    pub servers_root: PathBuf,
    /// e.g. "1.21.1".
    pub mc_version: String,
    pub loader: ServerLoader,
    /// java.exe (a javaw.exe is swapped for the java.exe beside it). Runs
    /// the loader installer and goes into run.bat.
    pub java: PathBuf,
    pub memory_mb: u32,
    pub preset: JvmPreset,
    /// Only `true` when the user agreed to the Minecraft EULA in the UI.
    pub accept_eula: bool,
    pub port: u16,
    pub motd: String,
}

impl CreateOptions {
    /// What the server lists show for a server Hopper made (Minecraft
    /// version and loader), saved in the server folder.
    pub fn hosted_meta(&self) -> super::HostedMeta {
        super::HostedMeta {
            minecraft: self.mc_version.clone(),
            loader: match &self.loader {
                ServerLoader::Vanilla => "Vanilla".into(),
                ServerLoader::Fabric(v)
                | ServerLoader::Quilt(v)
                | ServerLoader::Paper(v)
                | ServerLoader::Purpur(v) => {
                    format!("{} {}", self.loader.name(), v.clone().unwrap_or_default()).trim().to_string()
                }
                ServerLoader::NeoForge(v) | ServerLoader::Forge(v) => format!("{} {v}", self.loader.name()),
            },
        }
    }

    pub fn new(name: &str, servers_root: &Path, mc_version: &str, loader: ServerLoader, java: &Path) -> Self {
        let memory_mb = if loader.is_modded() { 6144 } else { 2048 };
        CreateOptions {
            name: name.to_string(),
            servers_root: servers_root.to_path_buf(),
            mc_version: mc_version.to_string(),
            loader,
            java: java.to_path_buf(),
            memory_mb,
            preset: JvmPreset::Aikar,
            accept_eula: false,
            port: super::suggest_port(servers_root),
            motd: name.to_string(),
        }
    }

    /// Defaults for a server of `inst`: its game version, loader and name.
    pub fn for_instance(inst: &Instance, servers_root: &Path, java: &Path) -> anyhow::Result<Self> {
        let mut o = Self::new(
            &inst.name,
            servers_root,
            &inst.game_version,
            ServerLoader::from_instance(&inst.loader)?,
            java,
        );
        if inst.settings.memory_mb >= 2048 {
            o.memory_mb = inst.settings.memory_mb;
        }
        Ok(o)
    }

    pub fn folder(&self) -> PathBuf {
        self.servers_root.join(Instance::folder_name(&self.name))
    }
}

/// The Java a server's `mc_version` needs, and the major of `java` when
/// its release file says (`None` for a bare "java" on PATH).
pub fn java_check(mc_version: &str, java: &Path) -> (u32, Option<u32>) {
    let need = crate::java::recommended_major(mc_version);
    (need, crate::java::inspect(java).map(|j| j.major))
}

/// Makes a server folder for `opts` and returns it. Refuses a folder that
/// exists and isn't empty; removes what it made if a step fails.
pub fn create_server(opts: &CreateOptions, job: &Job) -> anyhow::Result<PathBuf> {
    let root = opts.folder();
    anyhow::ensure!(!opts.name.trim().is_empty(), "Give the server a name.");
    if root.exists() && fs::read_dir(&root).map(|mut d| d.next().is_some()).unwrap_or(true) {
        bail!("{} already exists and isn't empty.", root.display());
    }
    let existed = root.exists();
    fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
    let result = fill(&root, opts, job);
    if result.is_err() && !existed {
        let _ = fs::remove_dir_all(&root);
    }
    result.map(|_| root)
}

fn fill(root: &Path, opts: &CreateOptions, job: &Job) -> anyhow::Result<()> {
    let java = console_java(&opts.java);
    let (need, have) = java_check(&opts.mc_version, &java);
    if let Some(have) = have
        && have < need
    {
        bail!("Minecraft {} needs Java {need}; {} is Java {have}.", opts.mc_version, java.display());
    }
    let major = have.unwrap_or(need);
    let target = install_loader(root, &opts.mc_version, &opts.loader, &java, job)?;
    job.detail("Writing the server's settings");
    let preset = if opts.preset.supported(major) { opts.preset } else { JvmPreset::Aikar };
    JvmArgs::new(opts.memory_mb, preset, major).save(root)?;
    write_run_bat(root, &java, &target, major)?;
    super::write_eula(root, opts.accept_eula)?;
    super::properties::defaults(opts.port, &opts.motd, opts.loader.is_modded())
        .save(&root.join("server.properties"))?;
    // Without it, the Minecraft version is only known once the server has
    // run, and adding mods before that picks files for the wrong version.
    opts.hosted_meta().save(root)?;
    Ok(())
}

/// Puts the loader's server files in `root`; returns what run.bat starts.
pub fn install_loader(
    root: &Path,
    mc: &str,
    loader: &ServerLoader,
    java: &Path,
    job: &Job,
) -> anyhow::Result<LaunchTarget> {
    match loader {
        ServerLoader::Vanilla => {
            install_vanilla(root, mc, job)?;
            Ok(LaunchTarget::Jar("server.jar".into()))
        }
        ServerLoader::Fabric(v) => install_fabric(root, mc, v.as_deref(), job),
        ServerLoader::Quilt(v) => install_quilt(root, mc, v.as_deref(), java, job),
        ServerLoader::NeoForge(v) => {
            crate::server_update::install_neoforge_server(root, v, java, job)?;
            cleanup_installer_files(root);
            Ok(LaunchTarget::ArgsFile(format!(
                "libraries/net/neoforged/neoforge/{v}/win_args.txt"
            )))
        }
        ServerLoader::Forge(v) => install_forge(root, mc, v, java, job),
        ServerLoader::Paper(v) => install_paper(root, mc, v.as_deref(), job),
        ServerLoader::Purpur(v) => install_purpur(root, mc, v.as_deref(), job),
    }
}

fn fetch_json(url: &str) -> anyhow::Result<Value> {
    Ok(crate::cf::agent()
        .get(url)
        .call()
        .with_context(|| format!("fetching {url}"))?
        .body_mut()
        .with_config()
        .limit(64 << 20)
        .read_json()?)
}

/// Downloads with the job's progress bar when the size is known.
fn download(url: &str, dest: &Path, size: Option<u64>, job: &Job) -> anyhow::Result<()> {
    crate::cf::download(url, dest, |n| {
        if let Some(total) = size.filter(|t| *t > 0) {
            job.progress(n.min(total) as usize, total as usize);
        }
    })
}

fn sha1_file(path: &Path) -> anyhow::Result<String> {
    Ok(sha1_smol::Sha1::from(fs::read(path)?).digest().to_string())
}

/// Mojang's server.jar for `mc`, checked against its SHA-1.
/// Minecraft's release versions, newest first (Mojang's list).
pub fn release_versions() -> anyhow::Result<Vec<String>> {
    let manifest = fetch_json(MOJANG_MANIFEST)?;
    Ok(manifest["versions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| v["type"] == "release")
        .filter_map(|v| v["id"].as_str().map(str::to_string))
        .collect())
}

pub fn install_vanilla(root: &Path, mc: &str, job: &Job) -> anyhow::Result<()> {
    job.detail(format!("Finding Minecraft {mc}"));
    let manifest = fetch_json(MOJANG_MANIFEST)?;
    let url = manifest["versions"]
        .as_array()
        .and_then(|vs| vs.iter().find(|v| v["id"] == mc))
        .and_then(|v| v["url"].as_str())
        .with_context(|| format!("Mojang doesn't list Minecraft {mc}"))?
        .to_string();
    let version = fetch_json(&url)?;
    let server = &version["downloads"]["server"];
    let jar_url = server["url"]
        .as_str()
        .with_context(|| format!("Minecraft {mc} has no server download"))?;
    let dest = root.join("server.jar");
    job.detail(format!("Downloading the Minecraft {mc} server"));
    download(jar_url, &dest, server["size"].as_u64(), job)?;
    if let Some(want) = server["sha1"].as_str() {
        let got = sha1_file(&dest)?;
        if !got.eq_ignore_ascii_case(want) {
            let _ = fs::remove_file(&dest);
            bail!("server.jar didn't download correctly (checksum mismatch); try again");
        }
    }
    Ok(())
}

/// The first `stable` version in a Fabric meta list (entries either have
/// `version` or `loader.version`).
fn first_stable(list: &Value) -> Option<String> {
    let arr = list.as_array()?;
    let ver = |e: &Value| {
        let e = if e["loader"].is_object() { &e["loader"] } else { e };
        Some((e["version"].as_str()?.to_string(), e["stable"].as_bool().unwrap_or(false)))
    };
    arr.iter()
        .filter_map(ver)
        .find(|(_, s)| *s)
        .or_else(|| arr.iter().filter_map(ver).next())
        .map(|(v, _)| v)
}

/// Fabric's server launcher (it starts the vanilla server.jar beside it,
/// downloaded here too so the first start needs no network).
pub fn install_fabric(root: &Path, mc: &str, loader: Option<&str>, job: &Job) -> anyhow::Result<LaunchTarget> {
    job.detail("Finding Fabric's versions");
    let loader = match loader {
        Some(l) => l.to_string(),
        None => first_stable(&fetch_json(&format!("{FABRIC_META}/versions/loader/{mc}"))?)
            .with_context(|| format!("Fabric has no loader for Minecraft {mc}"))?,
    };
    let installer = first_stable(&fetch_json(&format!("{FABRIC_META}/versions/installer"))?)
        .context("Fabric's installer list is empty")?;
    job.detail(format!("Downloading the Fabric {loader} server launcher"));
    download(
        &format!("{FABRIC_META}/versions/loader/{mc}/{loader}/{installer}/server/jar"),
        &root.join("fabric-server-launch.jar"),
        None,
        job,
    )?;
    install_vanilla(root, mc, job)?;
    Ok(LaunchTarget::Jar("fabric-server-launch.jar".into()))
}

/// Quilt's installer, run headless: `install server <mc> <loader>
/// --download-server --install-dir=<root>`.
pub fn install_quilt(
    root: &Path,
    mc: &str,
    loader: Option<&str>,
    java: &Path,
    job: &Job,
) -> anyhow::Result<LaunchTarget> {
    job.detail("Finding Quilt's versions");
    let loader = match loader {
        Some(l) => l.to_string(),
        None => fetch_json(&format!("{QUILT_META}/versions/loader"))?
            .as_array()
            .and_then(|a| {
                a.iter()
                    .filter_map(|e| e["version"].as_str())
                    .find(|v| !v.contains('-'))
                    .map(String::from)
            })
            .context("Quilt's loader list is empty")?,
    };
    let installers = fetch_json(&format!("{QUILT_META}/versions/installer"))?;
    let url = installers
        .as_array()
        .and_then(|a| a.first())
        .and_then(|i| i["url"].as_str())
        .context("Quilt's installer list is empty")?
        .to_string();
    let jar = crate::paths::temp_dir().join(format!("hopper-quilt-installer-{}.jar", std::process::id()));
    job.detail("Downloading the Quilt installer");
    download(&url, &jar, None, job)?;
    let dir = format!("--install-dir={}", root.display());
    let out = crate::loader_installer::run(
        java,
        &jar,
        &["install", "server", mc, &loader, "--download-server", &dir].map(std::ffi::OsStr::new),
        root,
        &format!("Installing Quilt {loader}"),
        job,
    );
    let _ = fs::remove_file(&jar);
    let out = out?;
    if !root.join("quilt-server-launch.jar").is_file() {
        bail!("the Quilt installer did not finish: {}", out.last_line);
    }
    Ok(LaunchTarget::Jar("quilt-server-launch.jar".into()))
}

/// Forge's installer with `--installServer`. 1.17+ leaves a win_args.txt
/// under libraries; older versions a runnable forge jar in the folder.
pub fn install_forge(root: &Path, mc: &str, version: &str, java: &Path, job: &Job) -> anyhow::Result<LaunchTarget> {
    let full = format!("{mc}-{version}");
    let installer = crate::paths::temp_dir().join(format!("hopper-forge-{full}-installer.jar"));
    job.detail(format!("Downloading the Forge {version} installer"));
    download(
        &format!("https://maven.minecraftforge.net/net/minecraftforge/forge/{full}/forge-{full}-installer.jar"),
        &installer,
        None,
        job,
    )?;
    let out = crate::loader_installer::run(
        java,
        &installer,
        &["--installServer".as_ref(), root.as_os_str()],
        root,
        &format!("Installing Forge {version} on the server"),
        job,
    );
    let _ = fs::remove_file(&installer);
    let out = out?;
    cleanup_installer_files(root);
    forge_target(root, &full).with_context(|| format!("the Forge installer did not finish: {}", out.last_line))
}

/// A bare file name from an API answer ("paper-1.21.1-133.jar").
fn plain_file_name(n: &str) -> bool {
    !n.is_empty() && !n.contains(['/', '\\', ':']) && !n.contains("..")
}

/// Paper's builds for `mc`, newest first: the stable ones, or every build
/// when there's no stable one yet (a new Minecraft version). Empty when
/// Paper doesn't support `mc`.
pub fn paper_builds(mc: &str) -> anyhow::Result<Vec<String>> {
    Ok(paper_build_list(&fetch_paper_builds(mc)?))
}

fn fetch_paper_builds(mc: &str) -> anyhow::Result<Value> {
    // A version Paper doesn't have answers 404.
    match crate::cf::agent().get(&format!("{PAPER_API}/versions/{mc}/builds")).call() {
        Ok(mut r) => Ok(r.body_mut().with_config().limit(64 << 20).read_json()?),
        Err(ureq::Error::StatusCode(404)) => Ok(Value::Array(Vec::new())),
        Err(e) => Err(e).with_context(|| format!("fetching Paper's builds for {mc}")),
    }
}

/// The builds in Paper's `/versions/{mc}/builds` answer, newest first.
fn paper_build_list(list: &Value) -> Vec<String> {
    paper_candidates(list)
        .into_iter()
        .filter_map(|b| b["id"].as_u64())
        .map(|id| id.to_string())
        .collect()
}

/// Stable builds (or all when none is stable), newest first.
fn paper_candidates(list: &Value) -> Vec<&Value> {
    let all: Vec<&Value> = list.as_array().into_iter().flatten().collect();
    let stable: Vec<&Value> = all.iter().copied().filter(|b| b["channel"] == "STABLE").collect();
    let mut pick = if stable.is_empty() { all } else { stable };
    pick.sort_by_key(|b| std::cmp::Reverse(b["id"].as_u64().unwrap_or(0)));
    pick
}

/// Paper's server jar for `mc` (build `build`, or the newest stable),
/// checked against its SHA-256.
pub fn install_paper(root: &Path, mc: &str, build: Option<&str>, job: &Job) -> anyhow::Result<LaunchTarget> {
    job.detail(format!("Finding Paper for Minecraft {mc}"));
    let list = fetch_paper_builds(mc)?;
    let b = match build {
        Some(id) => list
            .as_array()
            .into_iter()
            .flatten()
            .find(|b| b["id"].as_u64().map(|n| n.to_string()).as_deref() == Some(id))
            .with_context(|| format!("Paper has no build {id} for Minecraft {mc}"))?,
        None => *paper_candidates(&list)
            .first()
            .with_context(|| format!("Paper doesn't support Minecraft {mc}"))?,
    };
    let d = &b["downloads"]["server:default"];
    let url = d["url"].as_str().context("Paper's build has no server download")?;
    let file = d["name"].as_str().filter(|n| plain_file_name(n)).unwrap_or("paper.jar").to_string();
    let dest = root.join(&file);
    job.detail(format!("Downloading Paper {mc} build {}", b["id"]));
    download(url, &dest, d["size"].as_u64(), job)?;
    if let Some(want) = d["checksums"]["sha256"].as_str() {
        use sha2::Digest as _;
        let got = format!("{:x}", sha2::Sha256::digest(fs::read(&dest)?));
        if !got.eq_ignore_ascii_case(want) {
            let _ = fs::remove_file(&dest);
            bail!("Paper didn't download correctly (checksum mismatch); try again");
        }
    }
    fs::create_dir_all(root.join("plugins"))?;
    Ok(LaunchTarget::Jar(file))
}

/// Purpur's builds for `mc`, newest first; empty when Purpur doesn't
/// support `mc`.
pub fn purpur_builds(mc: &str) -> anyhow::Result<Vec<String>> {
    match crate::cf::agent().get(&format!("{PURPUR_API}/{mc}")).call() {
        Ok(mut r) => Ok(purpur_build_list(&r.body_mut().read_json()?)),
        Err(ureq::Error::StatusCode(404)) => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("fetching Purpur's builds for {mc}")),
    }
}

/// The builds in Purpur's `/v2/purpur/{mc}` answer, newest first.
fn purpur_build_list(v: &Value) -> Vec<String> {
    let mut all: Vec<String> = v["builds"]["all"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| b.as_str().map(String::from))
        .filter(|b| !b.is_empty() && b.chars().all(|c| c.is_ascii_digit()))
        .collect();
    all.sort_by_key(|b| std::cmp::Reverse(b.parse::<u64>().unwrap_or(0)));
    all
}

/// Purpur's server jar for `mc` (build `build`, or the newest).
pub fn install_purpur(root: &Path, mc: &str, build: Option<&str>, job: &Job) -> anyhow::Result<LaunchTarget> {
    job.detail(format!("Finding Purpur for Minecraft {mc}"));
    let build = match build {
        Some(b) => b.to_string(),
        None => purpur_builds(mc)?
            .into_iter()
            .next()
            .with_context(|| format!("Purpur doesn't support Minecraft {mc}"))?,
    };
    anyhow::ensure!(!build.is_empty() && build.chars().all(|c| c.is_ascii_digit()), "bad Purpur build {build}");
    let file = format!("purpur-{mc}-{build}.jar");
    anyhow::ensure!(plain_file_name(&file), "bad Minecraft version {mc}");
    let dest = root.join(&file);
    job.detail(format!("Downloading Purpur {mc} build {build}"));
    download(&format!("{PURPUR_API}/{mc}/{build}/download"), &dest, None, job)?;
    // A jar is a zip; an error page isn't.
    if !fs::read(&dest).is_ok_and(|b| b.starts_with(b"PK")) {
        let _ = fs::remove_file(&dest);
        bail!("Purpur {mc} build {build} didn't download correctly; try again");
    }
    fs::create_dir_all(root.join("plugins"))?;
    Ok(LaunchTarget::Jar(file))
}

/// Forge's recommended version for `mc` ("47.4.10"), else its latest one.
pub fn forge_promoted(mc: &str) -> Option<String> {
    forge_promo(&fetch_json(FORGE_PROMOS).ok()?, mc)
}

fn forge_promo(promos: &Value, mc: &str) -> Option<String> {
    let p = &promos["promos"];
    p[format!("{mc}-recommended")]
        .as_str()
        .or_else(|| p[format!("{mc}-latest")].as_str())
        .map(String::from)
}

/// What run.bat should start after a Forge install of `full` ("1.20.1-47.3.0").
pub fn forge_target(root: &Path, full: &str) -> anyhow::Result<LaunchTarget> {
    let args = format!("libraries/net/minecraftforge/forge/{full}/win_args.txt");
    if root.join(&args).is_file() {
        return Ok(LaunchTarget::ArgsFile(args));
    }
    let mut jars: Vec<String> = fs::read_dir(root)?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l.starts_with("forge-") && l.ends_with(".jar") && !l.contains("installer")
        })
        .collect();
    // "forge-1.12.2-14.23.5.2860.jar" over "...-universal.jar" if both.
    jars.sort_by_key(|n| (n.contains("universal"), n.len()));
    jars.into_iter()
        .next()
        .map(LaunchTarget::Jar)
        .context("no win_args.txt or forge jar")
}

fn cleanup_installer_files(root: &Path) {
    for n in ["installer.log", "installer.log.gz", "run.sh"] {
        let _ = fs::remove_file(root.join(n));
    }
    if let Ok(entries) = fs::read_dir(root) {
        for e in entries.flatten() {
            let n = e.file_name().to_string_lossy().to_ascii_lowercase();
            if n.ends_with("-installer.jar.log") {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}

// ---- from a client instance ---------------------------------------------

/// Folders copied from an instance to its server, besides mods.
pub const SERVER_DIRS: &[&str] = &[
    "config",
    "defaultconfigs",
    "kubejs",
    "scripts",
    "global_packs",
    "global_data_packs",
    "datapacks",
    "openloader",
    "paxi",
];

/// Pack mods split into (copy to the server, (file, why not)), using
/// Hopper's usual rules: the pack's server pack and CurseForge/Modrinth
/// side tags (already in `wont_sync`/`side` from
/// [`crate::serverpack::annotate`]), then the known client-only list.
pub fn server_mods(client: &[ModInfo]) -> (Vec<String>, Vec<(String, String)>) {
    let mut keep = Vec::new();
    let mut skip = Vec::new();
    for d in crate::diff::compute_diff(client, &[]) {
        let Some(c) = d.client else { continue };
        if d.action == DiffAction::CopyToServer {
            keep.push(c.file_name);
        } else {
            skip.push((c.file_name, d.skip_reason.unwrap_or_else(|| "Skipped".into())));
        }
    }
    (keep, skip)
}

/// What [`create_from_instance`] did.
#[derive(Clone, Debug, Default)]
pub struct CreateReport {
    pub root: PathBuf,
    pub mods_copied: usize,
    /// Client-only mods left out: (file, why).
    pub skipped: Vec<(String, String)>,
    pub files_copied: usize,
    /// Why the pack's server pack wasn't used, if it wasn't.
    pub note: Option<String>,
    /// Mods the pack wants that weren't downloaded (website-only ones), so
    /// aren't on the server either. Listed in `missing-mods.txt` there.
    pub missing: Vec<crate::modpack::Pending>,
}

impl CreateReport {
    /// One line per missing mod, for a job's notes.
    pub fn missing_notes(&self) -> Vec<String> {
        self.missing
            .iter()
            .map(|p| {
                let name = if p.name.is_empty() { &p.file_name } else { &p.name };
                format!("{name} isn't on the server: its author only allows downloads from the CurseForge website. Download it from {} and put it in the server's mods folder.", p.url)
            })
            .collect()
    }
}

/// The mods `inst`'s pack install couldn't download (see
/// [`crate::modpack::pending_downloads`]).
pub(crate) fn missing_mods(inst: &Instance) -> Vec<crate::modpack::Pending> {
    crate::modpack::pending_downloads(inst)
        .into_iter()
        .filter(|p| p.folder.is_empty() || p.folder == "mods")
        // Ones that stay off a server anyway don't need fetching for it.
        .filter(|p| {
            !p.addon.as_ref().is_some_and(|a| a.env == "client") && !crate::diff::known_client_only_file(&p.file_name)
        })
        .collect()
}

/// Writes `missing-mods.txt` into a new server listing `missing`.
pub(crate) fn write_missing(root: &Path, missing: &[crate::modpack::Pending]) {
    if missing.is_empty() {
        return;
    }
    let lines: Vec<String> = missing
        .iter()
        .map(|p| format!("{} ({}): {}", if p.name.is_empty() { &p.file_name } else { &p.name }, p.file_name, p.url))
        .collect();
    let _ = fs::write(
        root.join("missing-mods.txt"),
        format!(
            "These mods can only be downloaded from the CurseForge website, so Hopper couldn't put them on the server. Download each one and put it in the mods folder.

{}
",
            lines.join("
")
        ),
    );
}

/// A server for `inst`: makes the folder with `opts` (see
/// [`CreateOptions::for_instance`]), then copies the mods that belong on a
/// server and the pack's config. `sync_anyway` holds project ids the user
/// wants on the server even though they're marked client-only.
pub fn create_from_instance(
    inst: &Instance,
    opts: &CreateOptions,
    sync_anyway: &HashSet<u64>,
    hidden: &HashSet<String>,
    job: &Job,
) -> anyhow::Result<CreateReport> {
    job.detail("Reading the pack's mods");
    let mut client = crate::scan::scan_folder(&inst.mods_dir()).unwrap_or_default();
    client.retain(|m| !crate::rigsync::mod_hidden(m, hidden));
    let note = crate::serverpack::annotate(&mut client, inst, sync_anyway);
    let (keep, skipped) = server_mods(&client);

    let root = create_server(opts, job)?;
    let copied = copy_instance_files(inst, &root, &keep, job);
    if copied.is_err() {
        let _ = fs::remove_dir_all(&root);
    }
    let (mods_copied, files_copied) = copied?;
    // Website-only mods the pack install couldn't fetch: the server lacks
    // them too, so say which and where to get them.
    let missing = missing_mods(inst);
    write_missing(&root, &missing);
    Ok(CreateReport {
        root,
        mods_copied,
        skipped,
        files_copied,
        note: note.note,
        missing,
    })
}

/// A server for a modpack that publishes no server files: installs the
/// client pack (`file`, a version of `pack`) into a throwaway folder beside
/// the servers, then builds the server from it like
/// [`create_from_instance`]: the loader's server, the mods that belong on a
/// server (client-only ones left out and listed in the report) and the
/// pack's config, defaultconfigs, kubejs and scripts. The pack's Minecraft
/// and loader replace whatever `opts` holds. The throwaway folder is
/// removed afterwards, whatever happens. Returns the report and the
/// options the server was made with.
pub fn create_from_client_pack(
    pack: &crate::cf::Mod,
    file: &crate::cf::File,
    opts: &CreateOptions,
    job: &Job,
) -> anyhow::Result<(CreateReport, CreateOptions)> {
    anyhow::ensure!(!opts.name.trim().is_empty(), "Give the server a name.");
    if opts.folder().exists() {
        bail!("{} already exists.", opts.folder().display());
    }
    // Before the long install: a website-only pack zip can't be fetched,
    // so send the player to its page instead of failing halfway.
    if crate::cf::website_only(file)? {
        return Err(crate::cf::WebsiteOnly::of(pack, file).into());
    }
    let stage = opts.servers_root.join(format!(
        ".hopper-build-{}",
        Instance::folder_name(&opts.name)
    ));
    let _ = fs::remove_dir_all(&stage);
    fs::create_dir_all(&stage)?;
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(stage.clone());

    job.detail(format!("Installing {} to read its mods and config", pack.name));
    let place = crate::browse::Placement {
        instances_root: &stage,
        target: None,
        dest: None,
    };
    // The throwaway install's own notes (files to download into it, "run
    // Update pack again") are about an instance the player never sees.
    let notes = job.note_count();
    let (root, _) =
        crate::browse::install_project(pack, Some(file.clone()), crate::cf::CLASS_MODPACKS, place, job)?;
    job.drop_notes_from(notes);
    job.check_cancelled()?;
    let inst = crate::instances::load(&root)
        .with_context(|| format!("reading the installed pack at {}", root.display()))?;
    let mut opts = opts.clone();
    opts.mc_version = inst.game_version.clone();
    opts.loader = ServerLoader::from_instance(&inst.loader)?;
    let r = create_from_instance(&inst, &opts, &HashSet::new(), &HashSet::new(), job)?;
    Ok((r, opts))
}

fn copy_instance_files(inst: &Instance, root: &Path, mods: &[String], job: &Job) -> anyhow::Result<(usize, usize)> {
    let mods_dir = root.join("mods");
    fs::create_dir_all(&mods_dir)?;
    for (i, m) in mods.iter().enumerate() {
        job.detail(format!("Copying mods ({}/{})", i + 1, mods.len()));
        job.progress(i, mods.len());
        fs::copy(inst.mods_dir().join(m), mods_dir.join(m)).with_context(|| format!("copying {m}"))?;
    }
    job.detail("Copying the pack's config");
    let mut files = 0;
    for d in SERVER_DIRS {
        let src = inst.root.join(d);
        if src.is_dir() {
            files += copy_server_tree(&src, &root.join(d), d)?;
        }
    }
    Ok((mods.len(), files))
}

/// Copies instance folder `rel` to the server, leaving client-only content
/// out ([`crate::configdiff::client_only`]); returns the number of files.
fn copy_server_tree(src: &Path, dst: &Path, rel: &str) -> anyhow::Result<usize> {
    if crate::configdiff::client_only(&format!("{rel}/x")) {
        return Ok(0);
    }
    fs::create_dir_all(dst)?;
    let mut n = 0;
    for e in fs::read_dir(src)? {
        let e = e?;
        let t = e.file_type()?;
        let name = e.file_name();
        let sub = format!("{rel}/{}", name.to_string_lossy());
        let to = dst.join(&name);
        if t.is_dir() {
            n += copy_server_tree(&e.path(), &to, &sub)?;
        } else if t.is_file() && !crate::configdiff::client_only(&sub) {
            fs::copy(e.path(), &to).with_context(|| format!("copying {}", e.path().display()))?;
            n += 1;
        }
    }
    Ok(n)
}

/// Copies a folder recursively; returns the number of files.
pub fn copy_tree(src: &Path, dst: &Path) -> anyhow::Result<usize> {
    fs::create_dir_all(dst)?;
    let mut n = 0;
    for e in fs::read_dir(src)? {
        let e = e?;
        let t = e.file_type()?;
        let to = dst.join(e.file_name());
        if t.is_dir() {
            n += copy_tree(&e.path(), &to)?;
        } else if t.is_file() {
            fs::copy(e.path(), &to).with_context(|| format!("copying {}", e.path().display()))?;
            n += 1;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(file: &str, side: &str, wont: Option<&str>) -> ModInfo {
        ModInfo {
            file_name: file.into(),
            mod_id: file.trim_end_matches(".jar").into(),
            mod_name: file.into(),
            side: side.into(),
            wont_sync: wont.map(String::from),
            ..Default::default()
        }
    }

    #[test]
    fn leaves_client_only_mods_out() {
        let client = vec![
            m("create-1.21.1-6.0.jar", "both", None),
            m("sodium-neoforge-0.6.jar", "", None),
            m("somehud-1.0.jar", "client", None),
            m("fancyclouds-2.0.jar", "both", Some("The pack author left this out of the server pack")),
            m("ftb-ranks-1.0.jar", "server", None),
        ];
        let (mut keep, skip) = server_mods(&client);
        keep.sort();
        assert_eq!(keep, ["create-1.21.1-6.0.jar", "ftb-ranks-1.0.jar"]);
        let reason = |f: &str| skip.iter().find(|(n, _)| n == f).map(|(_, r)| r.clone());
        assert_eq!(skip.len(), 3);
        assert!(reason("fancyclouds-2.0.jar").unwrap().contains("server pack"));
        assert!(reason("somehud-1.0.jar").unwrap().contains("Client-only"));
        assert!(reason("sodium-neoforge-0.6.jar").unwrap().contains("Known client-only"));
    }

    #[test]
    fn loaders_from_instances() {
        assert_eq!(ServerLoader::from_instance("").unwrap(), ServerLoader::Vanilla);
        assert_eq!(
            ServerLoader::from_instance("neoforge-21.1.256").unwrap(),
            ServerLoader::NeoForge("21.1.256".into())
        );
        assert_eq!(ServerLoader::from_instance("forge-47.3.0").unwrap(), ServerLoader::Forge("47.3.0".into()));
        assert_eq!(
            ServerLoader::from_instance("fabric-0.16.5").unwrap(),
            ServerLoader::Fabric(Some("0.16.5".into()))
        );
        assert!(ServerLoader::from_instance("liteloader-1").is_err());
    }

    #[test]
    fn plugin_server_loaders() {
        assert_eq!(ServerLoader::from_instance("paper-133").unwrap(), ServerLoader::Paper(Some("133".into())));
        assert_eq!(ServerLoader::from_instance("paper-").unwrap(), ServerLoader::Paper(None));
        assert_eq!(ServerLoader::from_instance("Purpur-2329").unwrap(), ServerLoader::Purpur(Some("2329".into())));
        assert!(!ServerLoader::Paper(None).is_modded() && ServerLoader::Purpur(None).runs_plugins());
        assert!(ServerLoader::Forge("47.4.10".into()).is_modded() && !ServerLoader::Forge(String::new()).runs_plugins());
        let o = CreateOptions::new("x", Path::new("."), "1.21.1", ServerLoader::Paper(Some("133".into())), Path::new("java"));
        assert_eq!(o.memory_mb, 2048);
        assert_eq!(o.hosted_meta().loader, "Paper 133");
    }

    #[test]
    fn paper_build_lists() {
        let builds = serde_json::json!([
            { "id": 131, "channel": "STABLE" },
            { "id": 134, "channel": "BETA" },
            { "id": 133, "channel": "STABLE" },
            { "id": 132, "channel": "STABLE" }
        ]);
        assert_eq!(paper_build_list(&builds), ["133", "132", "131"]);
        // A new Minecraft version with only beta builds still lists them.
        let beta = serde_json::json!([{ "id": 3, "channel": "BETA" }, { "id": 7, "channel": "ALPHA" }]);
        assert_eq!(paper_build_list(&beta), ["7", "3"]);
        assert!(paper_build_list(&serde_json::json!({ "message": "not found" })).is_empty());
    }

    #[test]
    fn purpur_build_lists() {
        let v = serde_json::json!({ "builds": { "latest": "2329", "all": ["2285", "2329", "999", "x"] } });
        assert_eq!(purpur_build_list(&v), ["2329", "2285", "999"]);
        assert!(purpur_build_list(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn forge_promotions() {
        let p = serde_json::json!({ "promos": {
            "1.20.1-latest": "47.4.12", "1.20.1-recommended": "47.4.10", "1.21.4-latest": "54.1.0"
        } });
        assert_eq!(forge_promo(&p, "1.20.1").as_deref(), Some("47.4.10"));
        assert_eq!(forge_promo(&p, "1.21.4").as_deref(), Some("54.1.0"));
        assert_eq!(forge_promo(&p, "1.99"), None);
        assert!(plain_file_name("paper-1.21.1-133.jar") && !plain_file_name("../x.jar") && !plain_file_name("a\\b"));
    }

    #[test]
    fn fabric_meta_lists() {
        let loaders = serde_json::json!([
            { "loader": { "version": "0.17.0-beta", "stable": false } },
            { "loader": { "version": "0.16.5", "stable": true } }
        ]);
        assert_eq!(first_stable(&loaders).as_deref(), Some("0.16.5"));
        let installers = serde_json::json!([{ "version": "1.1.0", "stable": true }]);
        assert_eq!(first_stable(&installers).as_deref(), Some("1.1.0"));
    }

    #[test]
    fn server_copy_leaves_paxi_resource_packs_out() {
        let dir = std::env::temp_dir().join(format!("hopper-paxi-copy-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let src = dir.join("config");
        fs::create_dir_all(src.join("paxi/resourcepacks")).unwrap();
        fs::create_dir_all(src.join("paxi/datapacks")).unwrap();
        fs::write(src.join("paxi/resourcepacks/rp.zip"), "r").unwrap();
        fs::write(src.join("paxi/datapacks/dp.zip"), "d").unwrap();
        fs::write(src.join("a.toml"), "a").unwrap();
        assert_eq!(copy_server_tree(&src, &dir.join("out"), "config").unwrap(), 2);
        assert!(dir.join("out/paxi/datapacks/dp.zip").is_file());
        assert!(dir.join("out/a.toml").is_file());
        assert!(!dir.join("out/paxi/resourcepacks/rp.zip").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_forge_targets_and_copies_trees() {
        let dir = std::env::temp_dir().join(format!("hopper-forge-target-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("libraries/net/minecraftforge/forge/1.20.1-47.3.0")).unwrap();
        assert!(forge_target(&dir, "1.20.1-47.3.0").is_err());
        fs::write(dir.join("forge-1.12.2-14.23.5.2860.jar"), "x").unwrap();
        fs::write(dir.join("forge-1.12.2-14.23.5.2860-installer.jar"), "x").unwrap();
        assert_eq!(
            forge_target(&dir, "1.12.2-14.23.5.2860").unwrap(),
            LaunchTarget::Jar("forge-1.12.2-14.23.5.2860.jar".into())
        );
        fs::write(dir.join("libraries/net/minecraftforge/forge/1.20.1-47.3.0/win_args.txt"), "x").unwrap();
        assert!(matches!(forge_target(&dir, "1.20.1-47.3.0").unwrap(), LaunchTarget::ArgsFile(_)));
        assert_eq!(copy_tree(&dir.join("libraries"), &dir.join("copy")).unwrap(), 1);
        assert!(dir.join("copy/net/minecraftforge/forge/1.20.1-47.3.0/win_args.txt").is_file());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Mojang's 1.21.1 server.jar into a temp folder.
    /// `cargo test --lib real_vanilla_download -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_vanilla_download() {
        let dir = std::env::temp_dir().join(format!("hopper-vanilla-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let jobs = crate::jobs::Jobs::default();
        install_vanilla(&dir, "1.21.1", &jobs.start("vanilla")).unwrap();
        let len = fs::metadata(dir.join("server.jar")).unwrap().len();
        println!("server.jar: {len} bytes");
        assert!(len > 1_000_000);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Network, a few hundred MB: a server for Cobblemon Official Modpack
    /// (Fabric, no server files) built from its client pack, as Browse's
    /// Build a server makes it. Needs Java 21 in HOPPER_TEST_JAVA. The EULA
    /// stays unaccepted.
    /// `cargo test --lib real_server_from_client_pack -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_server_from_client_pack() {
        let java = std::env::var("HOPPER_TEST_JAVA").expect("HOPPER_TEST_JAVA");
        let pack = crate::source::get_mod(crate::modrinth::num_id("5FFgwNNP")).unwrap();
        let file = crate::browse::newest_pack_file(&pack).unwrap();
        let dir = std::env::temp_dir().join(format!("hopper-clientpack-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let opts = CreateOptions::new("Cobblemon test", &dir, "", ServerLoader::Vanilla, Path::new(&java));
        let jobs = crate::jobs::Jobs::default();
        let (r, used) = create_from_client_pack(&pack, &file, &opts, &jobs.start("t")).unwrap();
        println!("{} {:?}: {} mods, {} files", used.mc_version, used.loader, r.mods_copied, r.files_copied);
        for (f, why) in &r.skipped {
            println!("  left out {f}: {why}");
        }
        assert!(matches!(used.loader, ServerLoader::Fabric(_)));
        assert!(r.mods_copied > 10 && !r.skipped.is_empty());
        assert!(r.root.join("run.bat").is_file() && r.root.join("config").is_dir());
        let mods: Vec<String> = fs::read_dir(r.root.join("mods"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_lowercase())
            .collect();
        assert!(mods.iter().any(|m| m.contains("cobblemon")));
        assert!(!mods.iter().any(|m| m.contains("sodium")), "{mods:?}");
        // The throwaway client install is gone.
        assert!(!fs::read_dir(&dir).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with(".hopper-build")));
        // HOPPER_TEST_KEEP=1 keeps the server to try starting it.
        if std::env::var_os("HOPPER_TEST_KEEP").is_some() {
            println!("kept {}", r.root.display());
        } else {
            let _ = fs::remove_dir_all(dir);
        }
    }
}
