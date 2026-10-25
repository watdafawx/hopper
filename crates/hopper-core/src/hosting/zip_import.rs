//! A server from a zip on disk (uploaded to Hopper Server, or found in its
//! servers folder): a server pack, whose Minecraft and loader are read from
//! the zip itself, or a client pack (a CurseForge zip or a .mrpack), built
//! like [`super::create_from_client_pack`] with client-only mods left out.

use super::create::{CreateOptions, CreateReport, ServerLoader, create_from_instance};
use super::server_pack::{PackTarget, build_from_zip, check_folder, wrapper_prefix, zip_names};
use crate::instances::Instance;
use crate::jobs::Job;
use anyhow::{Context as _, bail};
use std::collections::HashSet;
use std::fs;
use std::io::Read as _;
use std::path::Path;

/// What a zip holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZipKind {
    /// A server pack: mods and config to lay over a server.
    ServerPack,
    /// A CurseForge client pack (manifest.json).
    CurseForgePack,
    /// A Modrinth client pack (modrinth.index.json).
    ModrinthPack,
}

impl ZipKind {
    pub fn key(self) -> &'static str {
        match self {
            ZipKind::ServerPack => "server",
            ZipKind::CurseForgePack => "curseforge",
            ZipKind::ModrinthPack => "modrinth",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ZipInfo {
    pub kind: ZipKind,
    /// Minecraft and loader, when the zip says; a server pack that doesn't
    /// needs them picked.
    pub target: Option<PackTarget>,
    /// A name for the server: the pack's own, else the file's.
    pub name: String,
}

fn read_text(zip_path: &Path, name: &str) -> Option<String> {
    let mut z = zip::ZipArchive::new(fs::File::open(zip_path).ok()?).ok()?;
    let mut f = z.by_name(name).ok()?;
    if f.size() > 1 << 20 {
        return None;
    }
    let mut s = String::new();
    f.read_to_string(&mut s).ok()?;
    Some(s)
}

/// Minecraft from a NeoForge version: 21.1.x is 1.21.1, 21.0.x is 1.21,
/// and from 26.1 on the versions match Minecraft's.
fn mc_for_neoforge(v: &str) -> Option<String> {
    let mut p = v.split('.');
    let (a, b): (u32, u32) = (p.next()?.parse().ok()?, p.next()?.parse().ok()?);
    Some(if a >= 26 {
        format!("{a}.{b}")
    } else if b == 0 {
        format!("1.{a}")
    } else {
        format!("1.{a}.{b}")
    })
}

fn loader_id(kind: &str, ver: &str) -> Option<String> {
    let k = kind.trim().to_ascii_lowercase();
    let v = ver.trim().trim_matches('"');
    if v.is_empty() {
        return None;
    }
    Some(match k.as_str() {
        "neoforge" | "neo" => format!("neoforge-{v}"),
        "forge" => format!("forge-{}", v.rsplit('-').next().unwrap_or(v)),
        "fabric" => format!("fabric-{v}"),
        "quilt" => format!("quilt-{v}"),
        _ => return None,
    })
}

/// The Minecraft and loader a server pack runs on, from what's in it: a
/// variables.txt (ATM-style start scripts), a ServerStarter config, the
/// loader's installer jar, or its libraries.
pub fn server_pack_target(names: &[String], zip_path: &Path) -> Option<PackTarget> {
    let prefix = wrapper_prefix(names);
    let rel = |n: &str| n.strip_prefix(prefix.as_str()).unwrap_or(n).to_string();
    let rels: Vec<String> = names.iter().map(|n| rel(n)).collect();
    let mut t = PackTarget::default();

    if let Some(text) = read_text(zip_path, &format!("{prefix}variables.txt")) {
        let get = |k: &str| {
            text.lines()
                .filter_map(|l| l.split_once('='))
                .find(|(a, _)| a.trim().eq_ignore_ascii_case(k))
                .map(|(_, v)| v.trim().trim_matches('"').to_string())
        };
        if let (Some(mc), Some(kind), Some(v)) = (get("MINECRAFT_VERSION"), get("MODLOADER"), get("MODLOADER_VERSION"))
            && let Some(l) = loader_id(&kind, &v)
        {
            t.mc_version = mc;
            t.loader = l;
        }
    }
    if t.loader.is_empty()
        && let Some(text) = read_text(zip_path, &format!("{prefix}server-setup-config.yaml"))
    {
        let cap = |re: &str| regex::Regex::new(re).ok()?.captures(&text).map(|c| c[1].to_string());
        let mc = cap(r#"mcVersion:\s*"?([0-9][0-9.]*)"#);
        let v = cap(r#"loaderVersion:\s*"?([^"\s]+)"#);
        let kind = cap(r#"modLoader:\s*"?([A-Za-z]+)"#).unwrap_or_else(|| "forge".into());
        if let (Some(mc), Some(v)) = (mc, v)
            && let Some(l) = loader_id(&kind, &v)
        {
            t.mc_version = mc;
            t.loader = l;
        }
    }
    if t.loader.is_empty() {
        let neo = regex::Regex::new(r"^(?:neoforge-([0-9][^/]*?)-installer\.jar|libraries/net/neoforged/neoforge/([0-9][^/]*)/)").ok()?;
        let forge = regex::Regex::new(r"^(?:forge-([0-9.]+)-([0-9][^/]*?)-installer\.jar|libraries/net/minecraftforge/forge/([0-9.]+)-([^/]+)/)").ok()?;
        let fabric = regex::Regex::new(r"^libraries/net/fabricmc/fabric-loader/([0-9][^/]*)/").ok()?;
        let mc_re = regex::Regex::new(r"^(?:libraries/net/fabricmc/intermediary/([0-9][^/]*)/|\.fabric/server/([0-9][^/]*?)-server\.jar|versions/([0-9][0-9.]*)/|libraries/net/minecraft/server/([0-9][0-9.]*)/)").ok()?;
        let mc_named = rels.iter().find_map(|r| {
            let c = mc_re.captures(r)?;
            (1..=4).find_map(|i| c.get(i)).map(|m| m.as_str().to_string())
        });
        for r in &rels {
            if let Some(c) = neo.captures(r) {
                let v = c.get(1).or(c.get(2)).unwrap().as_str().to_string();
                t.mc_version = mc_for_neoforge(&v).unwrap_or_default();
                t.loader = format!("neoforge-{v}");
                break;
            }
            if let Some(c) = forge.captures(r) {
                let (mc, v) = match (c.get(1), c.get(2)) {
                    (Some(a), Some(b)) => (a.as_str(), b.as_str()),
                    _ => (c.get(3).unwrap().as_str(), c.get(4).unwrap().as_str()),
                };
                t.mc_version = mc.to_string();
                t.loader = format!("forge-{v}");
                break;
            }
            if let Some(c) = fabric.captures(r) {
                t.loader = format!("fabric-{}", &c[1]);
                t.mc_version = mc_named.clone().unwrap_or_default();
                break;
            }
        }
        if t.loader.is_empty()
            && let Some(mc) = mc_named
        {
            // A vanilla server jar and nothing else.
            t.mc_version = mc;
        }
    }
    (!t.mc_version.is_empty()).then_some(t)
}

/// What `zip_path` is and, when it says, what it runs on.
pub fn inspect(zip_path: &Path) -> anyhow::Result<ZipInfo> {
    let names = zip_names(zip_path)?;
    let file_name = zip_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Imported server".into());
    if names.iter().any(|n| n == "modrinth.index.json") {
        let index = crate::modpack::read_mr_index(zip_path)?;
        let name = if index.name.is_empty() { file_name } else { index.name.clone() };
        return Ok(ZipInfo {
            kind: ZipKind::ModrinthPack,
            target: Some(PackTarget { mc_version: index.minecraft(), loader: index.loader(), recommended_mb: 0 }),
            name,
        });
    }
    if names.iter().any(|n| n == "manifest.json")
        && let Ok(m) = crate::modpack::read_manifest(zip_path)
        && !m.minecraft.version.is_empty()
        // A server pack can carry the client manifest beside its mods; it's
        // a client pack only when the files are under overrides/.
        && !names.iter().any(|n| n.starts_with("mods/") && n.ends_with(".jar"))
    {
        let name = if m.name.is_empty() { file_name } else { m.name.clone() };
        return Ok(ZipInfo {
            kind: ZipKind::CurseForgePack,
            target: Some(PackTarget { mc_version: m.minecraft.version.clone(), loader: m.loader(), recommended_mb: m.minecraft.recommended_ram }),
            name,
        });
    }
    Ok(ZipInfo { kind: ZipKind::ServerPack, target: server_pack_target(&names, zip_path), name: file_name })
}

/// A server from the zip at `zip_path`. A server pack needs `opts` to carry
/// its Minecraft and loader (from [`inspect`] or picked); a client pack's
/// own replace them. Returns the report, the options it was made with and
/// the client-only mods left out (file, why).
pub fn create_from_local_zip(
    zip_path: &Path,
    opts: &CreateOptions,
    job: &Job,
) -> anyhow::Result<(CreateReport, CreateOptions, Option<String>)> {
    let info = inspect(zip_path)?;
    let label = zip_path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    match info.kind {
        ZipKind::ServerPack => {
            check_folder(opts)?;
            let r = build_from_zip(zip_path, &label, None, opts, job)?;
            let note = (!r.properties_from_pack.is_empty())
                .then(|| format!("server.properties from the pack: {}", r.properties_from_pack.join(", ")));
            Ok((r.create, opts.clone(), note))
        }
        ZipKind::CurseForgePack | ZipKind::ModrinthPack => {
            anyhow::ensure!(!opts.name.trim().is_empty(), "Give the server a name.");
            if opts.folder().exists() {
                bail!("{} already exists.", opts.folder().display());
            }
            let stage = opts.servers_root.join(format!(".hopper-build-{}", Instance::folder_name(&opts.name)));
            let _ = fs::remove_dir_all(&stage);
            fs::create_dir_all(&stage)?;
            struct Cleanup(std::path::PathBuf);
            impl Drop for Cleanup {
                fn drop(&mut self) {
                    let _ = fs::remove_dir_all(&self.0);
                }
            }
            let _cleanup = Cleanup(stage.clone());
            job.detail(format!("Installing {} to read its mods and config", info.name));
            let inst = if info.kind == ZipKind::ModrinthPack {
                crate::modpack::import_mrpack(&stage, zip_path, job)?
            } else {
                crate::modpack::import_zip(&stage, zip_path, job)?
            };
            job.check_cancelled()?;
            let mut o = opts.clone();
            o.mc_version = inst.game_version.clone();
            o.loader = ServerLoader::from_instance(&inst.loader).context("the pack's loader")?;
            let r = create_from_instance(&inst, &o, &HashSet::new(), job)?;
            let note = (!r.skipped.is_empty()).then(|| format!("{} client-only mods left out", r.skipped.len()));
            Ok((r, o, note))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn zip_with(files: &[(&str, &str)]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("hopper-zipimport-{}-{}.zip", std::process::id(), files.len() * 7 + files[0].0.len()));
        let mut w = zip::ZipWriter::new(fs::File::create(&p).unwrap());
        for (n, body) in files {
            w.start_file(*n, zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        w.finish().unwrap();
        p
    }

    #[test]
    fn reads_what_a_server_pack_runs_on() {
        let z = zip_with(&[
            ("ATM10 Server/variables.txt", "MINECRAFT_VERSION=1.21.1\nMODLOADER=NeoForge\nMODLOADER_VERSION=21.1.219\n"),
            ("ATM10 Server/mods/a.jar", "x"),
        ]);
        let i = inspect(&z).unwrap();
        assert_eq!(i.kind, ZipKind::ServerPack);
        let t = i.target.unwrap();
        assert_eq!((t.mc_version.as_str(), t.loader.as_str()), ("1.21.1", "neoforge-21.1.219"));
        let _ = fs::remove_file(z);

        let z = zip_with(&[("neoforge-21.1.172-installer.jar", "x"), ("mods/b.jar", "x"), ("config/c.toml", "")]);
        let t = inspect(&z).unwrap().target.unwrap();
        assert_eq!((t.mc_version.as_str(), t.loader.as_str()), ("1.21.1", "neoforge-21.1.172"));
        let _ = fs::remove_file(z);

        let z = zip_with(&[("libraries/net/fabricmc/fabric-loader/0.16.9/x.jar", "x"), ("libraries/net/fabricmc/intermediary/1.21.1/y.jar", "x"), ("mods/d.jar", "x")]);
        let t = inspect(&z).unwrap().target.unwrap();
        assert_eq!((t.mc_version.as_str(), t.loader.as_str()), ("1.21.1", "fabric-0.16.9"));
        let _ = fs::remove_file(z);

        let z = zip_with(&[("mods/e.jar", "x"), ("config/e.toml", "")]);
        assert!(inspect(&z).unwrap().target.is_none());
        let _ = fs::remove_file(z);
    }
}
