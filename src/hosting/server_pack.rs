//! Hosting a server from a CurseForge pack's official server pack, so the
//! server runs exactly the mods (and config) the pack author chose for it.
//!
//! Hopper installs the loader itself ([`create_server`]), so run.bat,
//! user_jvm_args.txt, eula.txt and server.properties follow Hopper's
//! conventions. The server pack is then laid over that folder: its mods,
//! config and other content, but not its launch scripts or loader files.
//! Its server.properties values are merged over Hopper's defaults.
//!
//! Server packs that only carry a ServerStarter config (`server-setup-
//! config.yaml`) or a `manifest.json`, and download their mods on first
//! start, fall back to [`create_from_instance`]'s mod filtering.

use super::create::{CreateOptions, CreateReport, create_from_instance, create_server};
use super::properties::Properties;
use crate::browse::ServerFiles;
use crate::cf;
use crate::instances::Instance;
use crate::jobs::Job;
use anyhow::{Context as _, bail};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Read as _;
use std::path::Path;

/// Whether `inst` came from a CurseForge pack whose installed version has a
/// server pack, as Hopper recorded at install or import. No network. A pack
/// imported from an old CurseForge instance may say 0 although the pack has
/// one; [`resolve_server_pack`] asks CurseForge in that case.
pub fn has_server_pack(inst: &Instance) -> bool {
    inst.pack
        .as_ref()
        .is_some_and(|p| p.project_id != 0 && p.server_pack_file_id != 0)
}

/// The server pack file of `inst`'s pack version, from CurseForge. Uses the
/// recorded id, or the pack file's metadata when none was recorded.
pub fn resolve_server_pack(inst: &Instance) -> anyhow::Result<cf::File> {
    let Some(p) = inst.pack.as_ref().filter(|p| p.project_id != 0) else {
        bail!(
            "{} wasn't installed from a CurseForge modpack, so it has no server pack.",
            inst.name
        );
    };
    let mut id = p.server_pack_file_id;
    if id == 0 && p.file_id != 0 {
        id = cf::file(p.project_id, p.file_id)
            .with_context(|| format!("looking up {} on CurseForge", p.name))?
            .server_pack_file_id
            .unwrap_or(0);
    }
    if id == 0 {
        let pack = if p.name.is_empty() {
            inst.name.as_str()
        } else {
            p.name.as_str()
        };
        bail!("The author of {pack} didn't publish a server pack for this version.");
    }
    cf::file(p.project_id, id).context("looking up the server pack on CurseForge")
}

/// What [`create_from_server_pack`] did.
#[derive(Clone, Debug, Default)]
pub struct ServerPackReport {
    /// `mods_copied` and `files_copied` count the server pack's files (or
    /// the instance's, on the fallback); `skipped` holds the server pack's
    /// entries left out, as (path, why), with skipped folders listed once.
    /// `note` says why the server pack's mods weren't used, if they weren't.
    pub create: CreateReport,
    /// The server pack's file name on CurseForge.
    pub server_pack: String,
    /// `true` when the mods came from the server pack; `false` when it
    /// downloads them at first start and Hopper used the instance's mods.
    pub used_server_pack_mods: bool,
    /// Top-level files and folders copied from the server pack, with the
    /// number of files in each.
    pub copied: Vec<(String, usize)>,
    /// server.properties keys taken from the server pack.
    pub properties_from_pack: Vec<String>,
}

/// A server for `inst` from its pack's server pack. `opts` as for
/// [`create_server`] (see [`CreateOptions::for_instance`]). Downloads the
/// server pack into Hopper's pack cache (checked against CurseForge's size
/// and sha1), installs the loader the usual way, then lays the server
/// pack's content over it. Removes the folder again if a step fails.
pub fn create_from_server_pack(
    inst: &Instance,
    opts: &CreateOptions,
    job: &Job,
) -> anyhow::Result<ServerPackReport> {
    check_folder(opts)?;
    job.detail("Finding the pack's server pack");
    let file = resolve_server_pack(inst)?;
    create_from_zip(&file, Some(inst), opts, job)
}

/// What a pack's server files run on, read from the pack itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackTarget {
    /// e.g. "1.21.1".
    pub mc_version: String,
    /// Hopper's loader id, e.g. "neoforge-21.1.219".
    pub loader: String,
    /// The pack's recommended memory in MB, when it says (CurseForge).
    pub recommended_mb: u32,
}

/// The Minecraft and loader of the pack version `files` belong to: from
/// its manifest.json (CurseForge) or modrinth.index.json. Downloads the
/// pack file into Hopper's pack cache, where an install finds it later.
pub fn pack_target(files: &ServerFiles, job: &Job) -> anyhow::Result<PackTarget> {
    job.detail("Reading the pack's Minecraft and loader versions");
    let t = if files.file.is_some() {
        let v = crate::modrinth::version(&crate::modrinth::real_id(files.pack_file)?)?;
        let zip = crate::modpack::fetch_mrpack(&crate::modrinth::version_to_file(&v), job)?;
        let index = crate::modpack::read_mr_index(&zip)?;
        PackTarget {
            mc_version: index.minecraft(),
            loader: index.loader(),
            recommended_mb: 0,
        }
    } else {
        let f = cf::file(files.project, files.pack_file).context("looking up the pack on CurseForge")?;
        let zip = crate::modpack::fetch_pack_zip(&f, job)?;
        let m = crate::modpack::read_manifest(&zip)?;
        PackTarget {
            mc_version: m.minecraft.version.clone(),
            loader: m.loader(),
            recommended_mb: m.minecraft.recommended_ram,
        }
    };
    anyhow::ensure!(!t.mc_version.is_empty(), "The pack doesn't say which Minecraft it's for.");
    Ok(t)
}

/// A server straight from a pack's server files found in Browse, with no
/// instance: `opts` must already carry the pack's Minecraft and loader
/// (see [`pack_target`]). Packs whose server files download their mods at
/// first start can't be built this way.
pub fn create_from_server_files(
    files: &ServerFiles,
    opts: &CreateOptions,
    job: &Job,
) -> anyhow::Result<ServerPackReport> {
    check_folder(opts)?;
    job.detail("Finding the pack's server files");
    let file = crate::browse::server_zip(files)?;
    create_from_zip(&file, None, opts, job)
}

/// Refuses a nameless server and a folder that has something in it.
fn check_folder(opts: &CreateOptions) -> anyhow::Result<()> {
    let root = opts.folder();
    anyhow::ensure!(!opts.name.trim().is_empty(), "Give the server a name.");
    if root.exists()
        && fs::read_dir(&root)
            .map(|mut d| d.next().is_some())
            .unwrap_or(true)
    {
        bail!("{} already exists and isn't empty.", root.display());
    }
    Ok(())
}

/// The server from server pack `file`; `inst` supplies the mods when the
/// server pack downloads its own at first start.
fn create_from_zip(
    file: &cf::File,
    inst: Option<&Instance>,
    opts: &CreateOptions,
    job: &Job,
) -> anyhow::Result<ServerPackReport> {
    let root = opts.folder();
    // Server pack file ids are CurseForge file ids too, so they share the
    // pack cache (and its pruning) without colliding.
    let zip_path = crate::modpack::fetch_pack_zip(&file, job)?;
    job.check_cancelled()?;

    let names = zip_names(&zip_path)?;
    let plan = plan(&names);
    let mut out = ServerPackReport {
        server_pack: file.file_name.clone(),
        ..Default::default()
    };

    if let ModSource::FirstStart(why) = &plan.mods {
        let Some(inst) = inst else {
            bail!(
                "{} {why}, so Hopper can't build the server from it alone. Install the pack, then host it from that instance (its ⋯ menu → Host a server on this PC).",
                file.file_name
            );
        };
        let mut create = create_from_instance(inst, opts, &HashSet::new(), job)?;

        let note = format!(
            "{} {why}, so Hopper copied the pack's mods from this instance and left client-only ones out.",
            file.file_name
        );
        create.note = Some(match create.note.take() {
            Some(n) => format!("{note} {n}"),
            None => note,
        });
        match merge_pack_properties(&zip_path, &plan, &create.root, opts) {
            Ok(keys) => out.properties_from_pack = keys,
            Err(e) => {
                let _ = fs::remove_dir_all(&create.root);
                return Err(e);
            }
        }
        out.create = create;
        return Ok(out);
    }

    let existed = root.exists();
    let root = create_server(opts, job)?;
    let r = (|| -> anyhow::Result<()> {
        let o = overlay(&zip_path, &plan, &root, job)?;
        out.properties_from_pack = merge_pack_properties(&zip_path, &plan, &root, opts)?;
        out.copied = o.copied;
        out.create = CreateReport {
            root: root.clone(),
            mods_copied: o.mods,
            skipped: plan.skipped(),
            files_copied: o.files,
            note: None,
        };
        out.used_server_pack_mods = true;
        Ok(())
    })();
    if let Err(e) = r {
        if !existed {
            let _ = fs::remove_dir_all(&root);
        }
        return Err(e);
    }
    Ok(out)
}

// ---- what to take from the zip ---------------------------------------------

/// Where the server's mods come from.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ModSource {
    /// The server pack carries this many jars under mods/.
    Included(usize),
    /// It downloads them at first start; the reason, for the note.
    FirstStart(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Action {
    /// Copy to this path under the server folder.
    Copy(String),
    /// Merged by [`merge_pack_properties`].
    Properties,
    /// Left out: (path in the pack without the wrapping folder, why).
    Skip(String, &'static str),
    /// Folder entries and macOS junk.
    Ignore,
}

#[derive(Clone, Debug)]
struct Plan {
    /// One per zip entry, in zip order.
    actions: Vec<Action>,
    mods: ModSource,
    /// Index of the server pack's server.properties.
    properties: Option<usize>,
}

impl Plan {
    /// Skipped entries; a skipped folder is listed once with its count.
    fn skipped(&self) -> Vec<(String, String)> {
        let mut dirs: BTreeMap<String, (usize, &'static str)> = BTreeMap::new();
        let mut out = Vec::new();
        for a in &self.actions {
            let Action::Skip(path, why) = a else { continue };
            match path.split_once('/') {
                Some((top, _)) => dirs.entry(format!("{top}/")).or_insert((0, why)).0 += 1,
                None => out.push((path.clone(), why.to_string())),
            }
        }
        out.extend(dirs.into_iter().map(|(d, (n, why))| {
            let files = if n == 1 {
                "1 file".to_string()
            } else {
                format!("{n} files")
            };
            (d, format!("{why} ({files})"))
        }));
        out
    }
}

const WHY_SCRIPT: &str = "Launch script: Hopper writes run.bat";
const WHY_JVM: &str = "Hopper writes the server's Java arguments";
const WHY_LOADER: &str = "Loader files: Hopper installs the loader";
const WHY_EULA: &str = "Hopper asks for the EULA itself";
const WHY_STARTER: &str = "ServerStarter config: Hopper sets up the server";
const WHY_MANIFEST: &str = "CurseForge pack manifest";
const WHY_LOGS: &str = "Logs from the author's test runs";
const WHY_UNSAFE: &str = "Unsafe path in the zip";

/// Top folders that are server content, never a wrapping folder.
const CONTENT_DIRS: &[&str] = &[
    "mods",
    "config",
    "defaultconfigs",
    "kubejs",
    "scripts",
    "libraries",
    "world",
    "resourcepacks",
    "datapacks",
    "global_packs",
    "global_data_packs",
    "openloader",
    "paxi",
    "packmenu",
    "structures",
    "versions",
];

fn zip_names(zip_path: &Path) -> anyhow::Result<Vec<String>> {
    let z = zip::ZipArchive::new(fs::File::open(zip_path)?)
        .with_context(|| format!("{} isn't a readable zip", zip_path.display()))?;
    // In index order, so the plan's actions line up with `by_index`.
    Ok((0..z.len())
        .map(|i| z.name_for_index(i).unwrap_or_default().replace('\\', "/"))
        .collect())
}

/// `true` for zip names that can't escape the folder they're written to.
fn safe_rel(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains(':')
        && path
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != "..")
}

fn is_junk(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    l.starts_with("__macosx/")
        || l.ends_with("/.ds_store")
        || l == ".ds_store"
        || l.ends_with("thumbs.db")
}

/// The wrapping folder(s) to strip: while every file sits under one top
/// folder that isn't itself content (mods/, config/...), strip it.
fn wrapper_prefix(names: &[String]) -> String {
    let files: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|n| !n.ends_with('/') && !is_junk(n))
        .collect();
    let mut prefix = String::new();
    loop {
        let mut top: Option<&str> = None;
        let all_under_one = !files.is_empty()
            && files.iter().all(|f| {
                let Some(rest) = f.strip_prefix(prefix.as_str()) else {
                    return false;
                };
                let Some((t, _)) = rest.split_once('/') else {
                    return false;
                };
                *top.get_or_insert(t) == t
            });
        match top {
            Some(t)
                if all_under_one && !CONTENT_DIRS.contains(&t.to_ascii_lowercase().as_str()) =>
            {
                prefix.push_str(t);
                prefix.push('/');
            }
            _ => return prefix,
        }
    }
}

fn classify(rel: &str) -> Action {
    if !safe_rel(rel) {
        return Action::Skip(rel.to_string(), WHY_UNSAFE);
    }
    let low = rel.to_ascii_lowercase();
    let skip = |why| Action::Skip(rel.to_string(), why);
    match low.split_once('/') {
        Some((top, _)) => match top {
            "libraries" | "versions" | ".fabric" | ".quilt" => skip(WHY_LOADER),
            "logs" | "crash-reports" => skip(WHY_LOGS),
            _ => Action::Copy(rel.to_string()),
        },
        None => {
            let ext = low.rsplit_once('.').map_or("", |(_, e)| e);
            match low.as_str() {
                "server.properties" => Action::Properties,
                "eula.txt" => skip(WHY_EULA),
                "user_jvm_args.txt" | "variables.txt" | "jvm_args.txt" => skip(WHY_JVM),
                "server-setup-config.yaml" | "server-setup-config.yml" => skip(WHY_STARTER),
                "manifest.json" => skip(WHY_MANIFEST),
                "installer.log" | "usercache.json" | "usernamecache.json" => skip(WHY_LOGS),
                _ if matches!(ext, "bat" | "cmd" | "sh" | "ps1" | "command") => skip(WHY_SCRIPT),
                _ if ext == "jar" || low.ends_with(".jar.log") => skip(WHY_LOADER),
                _ => Action::Copy(rel.to_string()),
            }
        }
    }
}

fn plan(names: &[String]) -> Plan {
    let prefix = wrapper_prefix(names);
    let mut properties = None;
    let actions: Vec<Action> = names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            if n.ends_with('/') || is_junk(n) {
                return Action::Ignore;
            }
            let rel = n.strip_prefix(prefix.as_str()).unwrap_or(n);
            let a = classify(rel);
            if a == Action::Properties {
                properties = Some(i);
            }
            a
        })
        .collect();
    let jars = actions
        .iter()
        .filter(|a| {
            matches!(a, Action::Copy(p) if {
                let l = p.to_ascii_lowercase();
                l.starts_with("mods/") && l.ends_with(".jar")
            })
        })
        .count();
    let starter = actions
        .iter()
        .any(|a| matches!(a, Action::Skip(_, w) if *w == WHY_STARTER));
    let manifest = actions
        .iter()
        .any(|a| matches!(a, Action::Skip(_, w) if *w == WHY_MANIFEST));
    let mods = if jars > 0 {
        ModSource::Included(jars)
    } else if starter {
        ModSource::FirstStart("downloads its mods at first start (ServerStarter)".into())
    } else if manifest {
        ModSource::FirstStart("lists its mods in manifest.json instead of including them".into())
    } else {
        ModSource::FirstStart("has no mods".into())
    };
    Plan {
        actions,
        mods,
        properties,
    }
}

// ---- writing it -------------------------------------------------------------

struct Overlay {
    mods: usize,
    files: usize,
    copied: Vec<(String, usize)>,
}

/// Extracts the plan's Copy entries into `root`.
fn overlay(zip_path: &Path, plan: &Plan, root: &Path, job: &Job) -> anyhow::Result<Overlay> {
    let mut z = zip::ZipArchive::new(fs::File::open(zip_path)?)?;
    let total = plan
        .actions
        .iter()
        .filter(|a| matches!(a, Action::Copy(_)))
        .count();
    let mut out = Overlay {
        mods: 0,
        files: 0,
        copied: Vec::new(),
    };
    let mut tops: BTreeMap<String, usize> = BTreeMap::new();
    let mut done = 0;
    for (i, a) in plan.actions.iter().enumerate() {
        let Action::Copy(rel) = a else { continue };
        job.check_cancelled()?;
        if done % 50 == 0 {
            job.detail(format!("Copying the server pack ({done}/{total})"));
            job.progress(done, total);
        }
        let mut entry = z.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        let dest = root.join(rel);
        if let Some(p) = dest.parent() {
            fs::create_dir_all(p)?;
        }
        let mut f =
            fs::File::create(&dest).with_context(|| format!("writing {}", dest.display()))?;
        std::io::copy(&mut entry, &mut f).with_context(|| format!("extracting {rel}"))?;
        done += 1;
        let low = rel.to_ascii_lowercase();
        if low.starts_with("mods/") && low.ends_with(".jar") {
            out.mods += 1;
        } else {
            out.files += 1;
        }
        let top = match rel.split_once('/') {
            Some((t, _)) => format!("{t}/"),
            None => rel.clone(),
        };
        *tops.entry(top).or_default() += 1;
    }
    job.progress(total, total);
    out.copied = tops.into_iter().collect();
    Ok(out)
}

/// Keys Hopper keeps its own value for: the port it picked (and the query
/// port that follows it) and the MOTD the user typed.
const KEEP_HOPPERS: &[&str] = &["server-port", "query.port", "motd"];

/// Sets the pack's values on `hopper`, except [`KEEP_HOPPERS`]; returns the
/// keys that changed or were added.
fn merge_properties(hopper: &mut Properties, pack: &Properties) -> Vec<String> {
    let mut changed = Vec::new();
    for (k, v) in pack.entries() {
        if KEEP_HOPPERS.contains(&k) || hopper.get(k) == Some(v) {
            continue;
        }
        changed.push(k.to_string());
        hopper.set(k, v);
    }
    changed
}

/// Merges the server pack's server.properties (if it has one) into the one
/// Hopper wrote in `root`. A blank MOTD in `opts` lets the pack's through.
fn merge_pack_properties(
    zip_path: &Path,
    plan: &Plan,
    root: &Path,
    opts: &CreateOptions,
) -> anyhow::Result<Vec<String>> {
    let Some(i) = plan.properties else {
        return Ok(Vec::new());
    };
    let mut z = zip::ZipArchive::new(fs::File::open(zip_path)?)?;
    let mut bytes = Vec::new();
    z.by_index(i)?.read_to_end(&mut bytes)?;
    let pack = Properties::parse(&String::from_utf8_lossy(&bytes));
    let path = root.join("server.properties");
    let mut props = Properties::load(&path)?;
    let mut keys = merge_properties(&mut props, &pack);
    if opts.motd.trim().is_empty()
        && let Some(m) = pack.get("motd")
    {
        props.set("motd", m);
        keys.push("motd".into());
    }
    props.save(&path)?;
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn temp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("hopper-sp-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn zip_with(path: &Path, entries: &[(&str, &str)]) {
        let mut z = zip::ZipWriter::new(fs::File::create(path).unwrap());
        let o = zip::write::SimpleFileOptions::default();
        for (n, body) in entries {
            if n.ends_with('/') {
                z.add_directory(n.trim_end_matches('/'), o).unwrap();
            } else {
                z.start_file(*n, o).unwrap();
                z.write_all(body.as_bytes()).unwrap();
            }
        }
        z.finish().unwrap();
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    const WRAPPED: &[(&str, &str)] = &[
        ("ATM10 Server/", ""),
        ("ATM10 Server/mods/", ""),
        ("ATM10 Server/mods/create-6.0.jar", "create"),
        ("ATM10 Server/mods/ftb-ranks-1.0.jar", "ranks"),
        ("ATM10 Server/config/create-common.toml", "a=1"),
        ("ATM10 Server/defaultconfigs/ftbchunks-world.snbt", "{}"),
        ("ATM10 Server/kubejs/server_scripts/recipes.js", "//"),
        ("ATM10 Server/startserver.bat", "@echo off"),
        ("ATM10 Server/startserver.sh", "#!/bin/sh"),
        ("ATM10 Server/run.bat", "java"),
        ("ATM10 Server/Start.ps1", "x"),
        ("ATM10 Server/variables.txt", "MC=1.21.1"),
        ("ATM10 Server/user_jvm_args.txt", "-Xmx4G"),
        ("ATM10 Server/eula.txt", "eula=true"),
        ("ATM10 Server/neoforge-21.1.200-installer.jar", "jar"),
        (
            "ATM10 Server/libraries/net/neoforged/neoforge/21.1.200/win_args.txt",
            "x",
        ),
        (
            "ATM10 Server/libraries/net/neoforged/neoforge/21.1.200/unix_args.txt",
            "x",
        ),
        ("ATM10 Server/server-icon.png", "png"),
        (
            "ATM10 Server/server.properties",
            "motd=All the Mods 10\nserver-port=25565\nallow-flight=true\nmax-players=40\nlevel-type=minecraft\\:normal\n",
        ),
        ("__MACOSX/ATM10 Server/._run.bat", "junk"),
    ];

    #[test]
    fn strips_the_wrapping_folder_only() {
        assert_eq!(
            wrapper_prefix(&names(&["A/B/mods/x.jar", "A/B/config/y"])),
            "A/B/"
        );
        assert_eq!(wrapper_prefix(&names(&["mods/x.jar", "mods/y.jar"])), "");
        assert_eq!(wrapper_prefix(&names(&["mods/x.jar", "config/y"])), "");
        assert_eq!(
            wrapper_prefix(&names(&[
                "Pack/mods/x.jar",
                "Pack/run.bat",
                "__MACOSX/Pack/._x"
            ])),
            "Pack/"
        );
        assert_eq!(wrapper_prefix(&names(&["run.bat", "Pack/mods/x.jar"])), "");
        assert_eq!(wrapper_prefix(&[]), "");
    }

    #[test]
    fn overlays_content_and_skips_scripts_and_loader_files() {
        let dir = temp("overlay");
        let zip = dir.join("sp.zip");
        zip_with(&zip, WRAPPED);
        let root = dir.join("server");
        fs::create_dir_all(&root).unwrap();
        // What Hopper's loader install wrote; the pack must not replace it.
        fs::write(root.join("run.bat"), "hopper").unwrap();
        fs::write(root.join("user_jvm_args.txt"), "-Xmx6G").unwrap();

        let p = plan(&zip_names(&zip).unwrap());
        assert_eq!(p.mods, ModSource::Included(2));
        let jobs = crate::jobs::Jobs::default();
        let o = overlay(&zip, &p, &root, &jobs.start("t")).unwrap();
        assert_eq!(o.mods, 2);
        assert_eq!(o.files, 4);
        assert_eq!(
            o.copied,
            [
                ("config/".to_string(), 1),
                ("defaultconfigs/".to_string(), 1),
                ("kubejs/".to_string(), 1),
                ("mods/".to_string(), 2),
                ("server-icon.png".to_string(), 1),
            ]
        );
        for f in [
            "mods/create-6.0.jar",
            "mods/ftb-ranks-1.0.jar",
            "config/create-common.toml",
            "defaultconfigs/ftbchunks-world.snbt",
            "kubejs/server_scripts/recipes.js",
            "server-icon.png",
        ] {
            assert!(root.join(f).is_file(), "{f} missing");
        }
        assert_eq!(
            fs::read_to_string(root.join("mods/create-6.0.jar")).unwrap(),
            "create"
        );
        assert_eq!(fs::read_to_string(root.join("run.bat")).unwrap(), "hopper");
        assert_eq!(
            fs::read_to_string(root.join("user_jvm_args.txt")).unwrap(),
            "-Xmx6G"
        );
        for f in [
            "ATM10 Server",
            "startserver.bat",
            "startserver.sh",
            "Start.ps1",
            "variables.txt",
            "eula.txt",
            "neoforge-21.1.200-installer.jar",
            "libraries",
            "__MACOSX",
            "server.properties",
        ] {
            assert!(!root.join(f).exists(), "{f} copied");
        }

        let skipped = p.skipped();
        let why = |f: &str| {
            skipped
                .iter()
                .find(|(n, _)| n == f)
                .map(|(_, w)| w.clone())
                .unwrap_or_default()
        };
        assert_eq!(why("startserver.bat"), WHY_SCRIPT);
        assert_eq!(why("run.bat"), WHY_SCRIPT);
        assert_eq!(why("Start.ps1"), WHY_SCRIPT);
        assert_eq!(why("variables.txt"), WHY_JVM);
        assert_eq!(why("eula.txt"), WHY_EULA);
        assert_eq!(why("neoforge-21.1.200-installer.jar"), WHY_LOADER);
        assert_eq!(why("libraries/"), format!("{WHY_LOADER} (2 files)"));
        assert_eq!(skipped.len(), 9);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_unsafe_paths() {
        let p = plan(&names(&[
            "mods/a.jar",
            "../evil.txt",
            "config/../../x",
            "C:/x/y",
            "/abs",
        ]));
        assert_eq!(p.actions[0], Action::Copy("mods/a.jar".into()));
        for a in &p.actions[1..] {
            assert!(matches!(a, Action::Skip(_, w) if *w == WHY_UNSAFE), "{a:?}");
        }
    }

    #[test]
    fn merges_server_properties_keeping_hoppers_port_and_motd() {
        let dir = temp("props");
        let zip = dir.join("sp.zip");
        zip_with(&zip, WRAPPED);
        let root = dir.join("server");
        fs::create_dir_all(&root).unwrap();
        let mut opts = CreateOptions::new(
            "My ATM",
            &dir,
            "1.21.1",
            super::super::ServerLoader::Vanilla,
            Path::new("java"),
        );
        opts.port = 25570;
        super::super::properties::defaults(25570, "My ATM", true)
            .save(&root.join("server.properties"))
            .unwrap();

        let p = plan(&zip_names(&zip).unwrap());
        let keys = merge_pack_properties(&zip, &p, &root, &opts).unwrap();
        let props = Properties::load(&root.join("server.properties")).unwrap();
        assert_eq!(props.get("server-port"), Some("25570"));
        assert_eq!(props.get("query.port"), Some("25570"));
        assert_eq!(props.get("motd"), Some("My ATM"));
        assert_eq!(props.get("max-players"), Some("40"));
        assert_eq!(props.get("level-type"), Some("minecraft:normal"));
        assert_eq!(props.get("allow-flight"), Some("true"));
        assert!(keys.contains(&"max-players".to_string()));
        assert!(keys.contains(&"level-type".to_string()));
        // Same value as Hopper's default: not counted as from the pack.
        assert!(!keys.contains(&"allow-flight".to_string()));
        assert!(!keys.iter().any(|k| k == "server-port" || k == "motd"));

        // A blank MOTD lets the pack's through.
        opts.motd.clear();
        let keys = merge_pack_properties(&zip, &p, &root, &opts).unwrap();
        let props = Properties::load(&root.join("server.properties")).unwrap();
        assert_eq!(props.get("motd"), Some("All the Mods 10"));
        assert_eq!(keys, ["motd"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_properties_in_the_pack_changes_nothing() {
        let mut hopper = Properties::parse("server-port=25566\nmotd=x\n");
        let before = hopper.clone();
        assert!(merge_properties(&mut hopper, &Properties::default()).is_empty());
        assert_eq!(hopper, before);
        let p = plan(&names(&["mods/a.jar"]));
        assert!(p.properties.is_none());
    }

    #[test]
    fn detects_packs_that_download_mods_at_first_start() {
        let starter = plan(&names(&[
            "Pack/",
            "Pack/server-setup-config.yaml",
            "Pack/serverstarter-2.4.0.jar",
            "Pack/startserver.bat",
            "Pack/config/a.toml",
        ]));
        assert!(matches!(&starter.mods, ModSource::FirstStart(w) if w.contains("ServerStarter")));

        let manifest = plan(&names(&[
            "manifest.json",
            "overrides/config/a.toml",
            "start.sh",
        ]));
        assert!(matches!(&manifest.mods, ModSource::FirstStart(w) if w.contains("manifest.json")));

        // An empty mods/ folder with a config yaml is still ServerStarter.
        let empty = plan(&names(&["mods/", "server-setup-config.yaml"]));
        assert!(matches!(&empty.mods, ModSource::FirstStart(_)));

        let none = plan(&names(&["config/a.toml"]));
        assert!(matches!(&none.mods, ModSource::FirstStart(w) if w.contains("no mods")));

        // Jars beside the yaml: the pack ships its mods; use them.
        let both = plan(&names(&["server-setup-config.yaml", "mods/a.jar"]));
        assert_eq!(both.mods, ModSource::Included(1));
    }

    #[test]
    fn has_server_pack_reads_the_recorded_id() {
        let mut inst = Instance::default();
        assert!(!has_server_pack(&inst));
        inst.pack = Some(crate::instances::PackRef {
            project_id: 925200,
            file_id: 1,
            ..Default::default()
        });
        assert!(!has_server_pack(&inst));
        inst.pack.as_mut().unwrap().server_pack_file_id = 2;
        assert!(has_server_pack(&inst));
        assert!(resolve_server_pack(&Instance::default()).is_err());
    }

    /// Resolves All the Mods 10's server pack on CurseForge and plans it
    /// from the zip's file list (range reads, no full download). Needs a
    /// CurseForge API key.
    /// `cargo test --lib real_atm10_server_pack -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_atm10_server_pack() {
        const ATM10: u64 = 925200;
        let pack = cf::get_mod(ATM10).unwrap();
        let mut inst = Instance {
            name: pack.name.clone(),
            pack: Some(crate::instances::PackRef {
                project_id: ATM10,
                file_id: pack.main_file_id,
                name: pack.name.clone(),
                ..Default::default()
            }),
            ..Default::default()
        };
        // Nothing recorded: resolve asks CurseForge for the pack file's id.
        assert!(!has_server_pack(&inst));
        let sp = resolve_server_pack(&inst).unwrap();
        println!(
            "{} -> {} ({} bytes, sha1 {:?})",
            pack.name,
            sp.file_name,
            sp.file_length,
            sp.sha1()
        );
        assert!(sp.file_length > 0);
        assert_eq!(sp.is_server_pack, Some(true));
        inst.pack.as_mut().unwrap().server_pack_file_id = sp.id;
        assert!(has_server_pack(&inst));

        let url = cf::download_url(&sp).unwrap();
        let names = crate::serverpack::remote_zip_names(&url, sp.file_length).unwrap();
        let p = plan(&names);
        println!("{} entries, mods: {:?}", names.len(), p.mods);
        for (n, why) in p.skipped() {
            println!("  skip {n}: {why}");
        }
        assert!(
            matches!(p.mods, ModSource::Included(n) if n > 100)
                || matches!(p.mods, ModSource::FirstStart(_))
        );
    }

    /// Network, ~1 GB: a whole server from Create Live 5's Modrinth server
    /// zip with no instance, as Browse's Host a server makes it. Needs Java
    /// 17 in HOPPER_TEST_JAVA. The EULA stays unaccepted.
    /// `cargo test --lib real_server_from_modrinth_files -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_server_from_modrinth_files() {
        let java = std::env::var("HOPPER_TEST_JAVA").expect("HOPPER_TEST_JAVA");
        let files = crate::browse::modrinth_latest_server_files(crate::modrinth::num_id("VUmd23oG"))
            .unwrap()
            .unwrap();
        let jobs = crate::jobs::Jobs::default();
        let job = jobs.start("t");
        let t = pack_target(&files, &job).unwrap();
        println!("{t:?}");
        assert_eq!(t.mc_version, "1.20.1");
        assert!(t.loader.starts_with("forge-"), "{}", t.loader);
        let dir = temp("from-files");
        let opts = CreateOptions::new(
            "Create Live 5 server",
            &dir,
            &t.mc_version,
            super::super::create::ServerLoader::from_instance(&t.loader).unwrap(),
            Path::new(&java),
        );
        let r = create_from_server_files(&files, &opts, &job).unwrap();
        println!("{} mods from {}, copied {:?}", r.create.mods_copied, r.server_pack, r.copied);
        assert!(r.used_server_pack_mods && r.create.mods_copied > 50);
        let root = r.create.root;
        assert!(root.join("run.bat").is_file());
        assert!(fs::read_dir(root.join("mods")).unwrap().count() > 50);
        assert!(!fs::read_to_string(root.join("eula.txt")).unwrap_or_default().contains("eula=true"));
        let _ = fs::remove_dir_all(dir);
    }

    /// Network: a CurseForge pack's Minecraft and loader from its manifest.
    #[test]
    #[ignore]
    fn real_curseforge_pack_target() {
        let m = cf::get_mod(285109).unwrap(); // RLCraft
        let files = cf::files(m.id, "", cf::Loader::Any).unwrap();
        let s = crate::browse::curse_server_files_in(m.id, m.main_file_id, &files).unwrap();
        let jobs = crate::jobs::Jobs::default();
        let t = pack_target(&s, &jobs.start("t")).unwrap();
        println!("{t:?}");
        assert_eq!(t.mc_version, "1.12.2");
        assert!(t.loader.starts_with("forge-"));
    }
}
