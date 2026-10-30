//! Starting Minecraft directly, without Mojang's launcher: resolve the
//! version JSON (loader on top of vanilla), fetch missing libraries, natives
//! and assets, pick Java, build the command line and run it.
//!
//! Versions, libraries and assets are shared with the launcher folder
//! (CurseForge's Install), so nothing is downloaded twice.

use crate::instances::Instance;
use crate::jobs::Job;
use crate::launch::Defaults;
use anyhow::{Context, bail};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const MANIFEST: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const RESOURCES: &str = "https://resources.download.minecraft.net";

/// This OS in Mojang's version files: its name in rules and old-style
/// `natives` maps, the tag on new-style native jars, the native library
/// extension and the classpath separator.
#[cfg(windows)]
mod os {
    pub const NAME: &str = "windows";
    pub const NATIVES: &str = ":natives-windows";
    pub const LIB_EXT: &[&str] = &[".dll"];
    pub const CP_SEP: &str = ";";
}
#[cfg(target_os = "macos")]
mod os {
    pub const NAME: &str = "osx";
    pub const NATIVES: &str = ":natives-macos";
    pub const LIB_EXT: &[&str] = &[".dylib", ".jnilib"];
    pub const CP_SEP: &str = ":";
}
#[cfg(not(any(windows, target_os = "macos")))]
mod os {
    pub const NAME: &str = "linux";
    pub const NATIVES: &str = ":natives-linux";
    pub const LIB_EXT: &[&str] = &[".so"];
    pub const CP_SEP: &str = ":";
}

/// Who is playing. `offline` is for testing only (no multiplayer).
#[derive(Clone)]
pub struct Player {
    pub name: String,
    pub uuid: String,
    pub access_token: String,
    pub xuid: String,
    pub user_type: &'static str,
}

impl Player {
    pub fn from_session(s: &crate::msauth::Session) -> Self {
        Self {
            name: s.profile.name.clone(),
            uuid: s.profile.id.clone(),
            access_token: s.access_token.clone(),
            xuid: s.xuid.clone(),
            user_type: "msa",
        }
    }

    #[cfg(test)]
    pub fn offline(name: &str) -> Self {
        Self {
            name: name.into(),
            uuid: "00000000000000000000000000000000".into(),
            access_token: "0".into(),
            xuid: "0".into(),
            user_type: "legacy",
        }
    }
}

fn read_json(p: &Path) -> anyhow::Result<Value> {
    Ok(serde_json::from_slice(
        &fs::read(p).with_context(|| format!("reading {}", p.display()))?,
    )?)
}

fn fetch_json(url: &str) -> anyhow::Result<Value> {
    Ok(crate::cf::agent()
        .get(url)
        .call()?
        .body_mut()
        .with_config()
        .limit(64 << 20)
        .read_json()?)
}

/// `versions/<id>/<id>.json`, downloading vanilla ones from Mojang.
fn version_json(dir: &Path, id: &str) -> anyhow::Result<Value> {
    let p = dir.join("versions").join(id).join(format!("{id}.json"));
    if p.exists() {
        return read_json(&p);
    }
    let manifest = fetch_json(MANIFEST)?;
    let url = manifest["versions"]
        .as_array()
        .and_then(|vs| vs.iter().find(|v| v["id"] == id))
        .and_then(|v| v["url"].as_str())
        .with_context(|| format!("Minecraft {id} is not in Mojang's version list"))?;
    let v = fetch_json(url)?;
    fs::create_dir_all(p.parent().unwrap_or(dir))?;
    fs::write(&p, serde_json::to_vec_pretty(&v)?)?;
    Ok(v)
}

/// The loader's JSON merged over the version it inherits from.
pub fn resolve(dir: &Path, id: &str) -> anyhow::Result<Value> {
    let child = version_json(dir, id)?;
    let Some(parent_id) = child["inheritsFrom"].as_str() else {
        return Ok(child);
    };
    let mut merged = resolve(dir, parent_id)?;
    for (k, v) in child.as_object().into_iter().flatten() {
        match k.as_str() {
            "libraries" => {
                // Loader libraries first, then the parent's that it doesn't replace.
                let mut libs = v.as_array().cloned().unwrap_or_default();
                let names: Vec<String> = libs
                    .iter()
                    .filter_map(|l| l["name"].as_str())
                    .map(lib_key)
                    .collect();
                for l in merged["libraries"].as_array().cloned().unwrap_or_default() {
                    if !l["name"]
                        .as_str()
                        .is_some_and(|n| names.contains(&lib_key(n)))
                    {
                        libs.push(l);
                    }
                }
                merged["libraries"] = Value::Array(libs);
            }
            "arguments" => {
                for side in ["game", "jvm"] {
                    let mut a = merged["arguments"][side]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    a.extend(v[side].as_array().cloned().unwrap_or_default());
                    merged["arguments"][side] = Value::Array(a);
                }
            }
            "inheritsFrom" => {}
            _ => merged[k] = v.clone(),
        }
    }
    // The client jar is the vanilla one.
    merged["jar"] = Value::String(merged["jar"].as_str().unwrap_or(parent_id).to_string());
    Ok(merged)
}

/// "group:artifact" without the version, to spot overrides.
fn lib_key(name: &str) -> String {
    let p: Vec<&str> = name.split(':').collect();
    match p.as_slice() {
        [g, a, _, c, ..] => format!("{g}:{a}:{c}"),
        [g, a, ..] => format!("{g}:{a}"),
        _ => name.to_string(),
    }
}

/// Maven coordinate to a path: "g.r.p:art:1.0[:cls][@ext]".
fn maven_path(name: &str) -> String {
    let (coord, ext) = name.split_once('@').unwrap_or((name, "jar"));
    let p: Vec<&str> = coord.split(':').collect();
    let (g, a, v) = (
        p[0].replace('.', "/"),
        p.get(1).copied().unwrap_or(""),
        p.get(2).copied().unwrap_or(""),
    );
    let cls = p.get(3).map(|c| format!("-{c}")).unwrap_or_default();
    format!("{g}/{a}/{v}/{a}-{v}{cls}.{ext}")
}

/// Mojang's rule lists: allow/disallow by OS and features.
fn rules_allow(rules: &Value, features: &HashMap<&str, bool>) -> bool {
    let Some(rules) = rules.as_array() else {
        return true;
    };
    let mut allowed = false;
    for r in rules {
        let mut applies = true;
        if let Some(os) = r["os"].as_object() {
            if os
                .get("name")
                .and_then(|n| n.as_str())
                .is_some_and(|n| n != os::NAME)
            {
                applies = false;
            }
            if os
                .get("arch")
                .and_then(|n| n.as_str())
                .is_some_and(|a| a != "x86_64" && a != "x64")
            {
                applies = false;
            }
        }
        if let Some(f) = r["features"].as_object() {
            for (k, want) in f {
                if features.get(k.as_str()).copied().unwrap_or(false)
                    != want.as_bool().unwrap_or(false)
                {
                    applies = false;
                }
            }
        }
        if applies {
            allowed = r["action"] == "allow";
        }
    }
    allowed
}

struct Download {
    url: String,
    dest: PathBuf,
    size: u64,
}

fn fetch_all(list: Vec<Download>, job: &Job, what: &str) -> anyhow::Result<()> {
    let missing: Vec<Download> = list
        .into_iter()
        .filter(|d| fs::metadata(&d.dest).map_or(true, |m| d.size > 0 && m.len() != d.size))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let done = AtomicUsize::new(0);
    let next = AtomicUsize::new(0);
    let errors = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..16 {
            s.spawn(|| {
                while let Some(d) = missing.get(next.fetch_add(1, Ordering::Relaxed)) {
                    if let Err(e) = crate::cf::download(&d.url, &d.dest, |_| {}) {
                        errors
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(format!("{}: {e:#}", d.url));
                    }
                    let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                    if n.is_multiple_of(25) || n == missing.len() {
                        job.progress(n, missing.len());
                        job.detail(format!("Downloading {what}: {n} of {}", missing.len()));
                    }
                }
            });
        }
    });
    let errors = errors.into_inner().unwrap_or_default();
    if let Some(first) = errors.first() {
        bail!("{} {what} failed to download, e.g. {first}", errors.len());
    }
    Ok(())
}

/// Everything a launch needs on disk, with the paths to put on the command line.
pub struct Prepared {
    pub version: Value,
    pub classpath: Vec<PathBuf>,
    pub natives: PathBuf,
    pub assets: PathBuf,
    pub java: PathBuf,
}

pub fn prepare(
    inst: &Instance,
    dir: &Path,
    defaults: &Defaults,
    job: &Job,
) -> anyhow::Result<Prepared> {
    let id = crate::launch::ensure_loader(inst, dir, job)?;
    job.detail(format!("Reading {id}"));
    let v = resolve(dir, &id)?;
    let features: HashMap<&str, bool> = HashMap::new();
    let libs_dir = dir.join("libraries");
    let natives = dir.join("natives").join(&id);
    fs::create_dir_all(&natives)?;

    // Libraries (and old-style natives).
    let mut classpath = Vec::new();
    let mut downloads = Vec::new();
    let mut native_jars = Vec::new();
    for l in v["libraries"].as_array().cloned().unwrap_or_default() {
        if !rules_allow(&l["rules"], &features) {
            continue;
        }
        let name = l["name"].as_str().unwrap_or_default();
        let art = &l["downloads"]["artifact"];
        let (path, url, size) = if art.is_object() {
            (
                art["path"].as_str().unwrap_or_default().to_string(),
                art["url"].as_str().unwrap_or_default().to_string(),
                art["size"].as_u64().unwrap_or(0),
            )
        } else {
            let base = l["url"]
                .as_str()
                .unwrap_or("https://libraries.minecraft.net/");
            let p = maven_path(name);
            (p.clone(), format!("{}/{p}", base.trim_end_matches('/')), 0)
        };
        if !path.is_empty() {
            let dest = libs_dir.join(&path);
            if !url.is_empty() {
                downloads.push(Download {
                    url,
                    dest: dest.clone(),
                    size,
                });
            }
            if name.contains(os::NATIVES) {
                native_jars.push(dest.clone());
            }
            if !classpath.contains(&dest) {
                classpath.push(dest);
            }
        }
        // Pre-1.19: "natives": {"windows": "natives-windows"} + classifiers.
        if let Some(cls) = l["natives"][os::NAME].as_str() {
            let cls = cls.replace("${arch}", "64");
            let c = &l["downloads"]["classifiers"][&cls];
            if let (Some(p), Some(u)) = (c["path"].as_str(), c["url"].as_str()) {
                let dest = libs_dir.join(p);
                downloads.push(Download {
                    url: u.to_string(),
                    dest: dest.clone(),
                    size: c["size"].as_u64().unwrap_or(0),
                });
                native_jars.push(dest);
            }
        }
    }
    fetch_all(downloads, job, "libraries")?;

    // Natives: unpack the DLLs.
    for jar in native_jars {
        let Ok(f) = fs::File::open(&jar) else {
            continue;
        };
        let Ok(mut z) = zip::ZipArchive::new(f) else {
            continue;
        };
        for i in 0..z.len() {
            let Ok(mut e) = z.by_index(i) else { continue };
            let n = e.name().to_string();
            if os::LIB_EXT.iter().any(|x| n.ends_with(x)) && !n.contains("META-INF") {
                let out = natives.join(n.rsplit('/').next().unwrap_or(&n));
                if !out.exists()
                    && let Ok(mut o) = fs::File::create(&out)
                {
                    let _ = std::io::copy(&mut e, &mut o);
                }
            }
        }
    }

    // Client jar.
    let jar_id = v["jar"].as_str().unwrap_or(&id).to_string();
    let client = dir
        .join("versions")
        .join(&jar_id)
        .join(format!("{jar_id}.jar"));
    if let Some(u) = v["downloads"]["client"]["url"].as_str() {
        fetch_all(
            vec![Download {
                url: u.to_string(),
                dest: client.clone(),
                size: v["downloads"]["client"]["size"].as_u64().unwrap_or(0),
            }],
            job,
            "the game",
        )?;
    }
    // A loader version runs the vanilla jar under its own name, as Mojang's
    // launcher does: NeoForge and Forge skip `${version_name}.jar` on the
    // classpath (-DignoreList). Under the vanilla name it loads a second
    // time beside their patched game and the module system refuses both.
    let client = if jar_id != id {
        let own = dir.join("versions").join(&id).join(format!("{id}.jar"));
        let size = |p: &Path| fs::metadata(p).map(|m| m.len()).ok();
        if size(&own) != size(&client) {
            fs::copy(&client, &own).with_context(|| format!("copying the game jar to {}", own.display()))?;
        }
        own
    } else {
        client
    };
    classpath.push(client);

    // Assets.
    let assets = dir.join("assets");
    let index_id = v["assetIndex"]["id"]
        .as_str()
        .unwrap_or("legacy")
        .to_string();
    let index_path = assets.join("indexes").join(format!("{index_id}.json"));
    if !index_path.exists()
        && let Some(u) = v["assetIndex"]["url"].as_str()
    {
        crate::cf::download(u, &index_path, |_| {})?;
    }
    if let Ok(index) = read_json(&index_path) {
        let objects: Vec<Download> = index["objects"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(_, o)| {
                let h = o["hash"].as_str()?;
                Some(Download {
                    url: format!("{RESOURCES}/{}/{h}", &h[..2]),
                    dest: assets.join("objects").join(&h[..2]).join(h),
                    size: o["size"].as_u64().unwrap_or(0),
                })
            })
            .collect();
        fetch_all(objects, job, "assets")?;
    }

    let java = pick_java(inst, dir, defaults, &v)?;
    Ok(Prepared {
        version: v,
        classpath,
        natives,
        assets,
        java,
    })
}

/// The instance's Java, the default, or a detected install of the major
/// version Minecraft asks for.
fn pick_java(
    inst: &Instance,
    dir: &Path,
    defaults: &Defaults,
    v: &Value,
) -> anyhow::Result<PathBuf> {
    for p in [&inst.settings.java_path, &defaults.java_path] {
        if !p.trim().is_empty() {
            return Ok(PathBuf::from(p.trim()));
        }
    }
    let want = v["javaVersion"]["majorVersion"]
        .as_u64()
        .unwrap_or(crate::java::recommended_major(&inst.game_version) as u64) as u32;
    let all = crate::java::detect(dir);
    // GraalVM-only flags need a GraalVM; other JVMs refuse to start with them.
    let args = crate::launch::custom_args(&inst.settings.java_args, &defaults.java_args);
    if args.contains("JVMCI") || args.to_lowercase().contains("graal") {
        let graal = |j: &&crate::java::JavaInstall| {
            format!("{} {}", j.vendor, j.exe.display())
                .to_lowercase()
                .contains("graal")
        };
        if let Some(j) = all
            .iter()
            .filter(graal)
            .find(|j| j.major == want)
            .or_else(|| all.iter().filter(graal).find(|j| j.major > want))
        {
            return Ok(j.exe.clone());
        }
    }
    all.iter()
        .find(|j| j.major == want)
        .or_else(|| {
            all.iter()
                .filter(|j| j.major > want)
                .min_by_key(|j| j.major)
        })
        .map(|j| j.exe.clone())
        .with_context(|| {
            format!("No Java {want} found. Install it, or pick a Java in this instance's Settings.")
        })
}

/// The full command line.
pub fn command(
    inst: &Instance,
    dir: &Path,
    p: &Prepared,
    player: &Player,
    defaults: &Defaults,
) -> Vec<String> {
    let v = &p.version;
    let cp_sep = os::CP_SEP;
    let classpath = p
        .classpath
        .iter()
        .map(|c| c.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(cp_sep);
    let mut features: HashMap<&str, bool> = HashMap::new();
    features.insert("has_custom_resolution", true);
    let mem = if inst.settings.memory_mb > 0 {
        inst.settings.memory_mb
    } else {
        defaults.memory_mb
    };
    let vars: HashMap<&str, String> = HashMap::from([
        (
            "natives_directory",
            p.natives.to_string_lossy().into_owned(),
        ),
        ("launcher_name", "hopper".into()),
        ("launcher_version", env!("CARGO_PKG_VERSION").into()),
        ("classpath", classpath),
        ("classpath_separator", cp_sep.into()),
        (
            "library_directory",
            dir.join("libraries").to_string_lossy().into_owned(),
        ),
        ("version_name", v["id"].as_str().unwrap_or_default().into()),
        ("auth_player_name", player.name.clone()),
        ("game_directory", inst.root.to_string_lossy().into_owned()),
        ("assets_root", p.assets.to_string_lossy().into_owned()),
        ("game_assets", p.assets.to_string_lossy().into_owned()),
        (
            "assets_index_name",
            v["assetIndex"]["id"].as_str().unwrap_or_default().into(),
        ),
        ("auth_uuid", player.uuid.clone()),
        ("auth_access_token", player.access_token.clone()),
        ("auth_session", player.access_token.clone()),
        ("clientid", crate::msauth::client_id().unwrap_or_default()),
        ("auth_xuid", player.xuid.clone()),
        ("user_type", player.user_type.into()),
        (
            "version_type",
            v["type"].as_str().unwrap_or("release").into(),
        ),
        ("resolution_width", inst.settings.width.to_string()),
        ("resolution_height", inst.settings.height.to_string()),
        ("user_properties", "{}".into()),
    ]);
    let sub = |s: &str| {
        let mut out = s.to_string();
        while let Some(i) = out.find("${") {
            let Some(j) = out[i..].find('}') else { break };
            let key = &out[i + 2..i + j];
            let val = vars.get(key).cloned().unwrap_or_default();
            out.replace_range(i..i + j + 1, &val);
        }
        out
    };
    let expand = |list: &Value, out: &mut Vec<String>| {
        for a in list.as_array().into_iter().flatten() {
            match a {
                Value::String(s) => out.push(sub(s)),
                Value::Object(o)
                    if rules_allow(o.get("rules").unwrap_or(&Value::Null), &features) =>
                {
                    match o.get("value") {
                        Some(Value::String(s)) => out.push(sub(s)),
                        Some(Value::Array(xs)) => {
                            out.extend(xs.iter().filter_map(|x| x.as_str()).map(&sub))
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    };

    let mut args = Vec::new();
    args.push(format!("-Xmx{mem}m"));
    let xms = crate::launch::initial_memory(&inst.settings.java_args, &defaults.java_args, mem);
    args.push(format!("-Xms{xms}m"));
    let custom = crate::launch::custom_args(&inst.settings.java_args, &defaults.java_args);
    args.extend(
        crate::launch::split_args(custom)
            .into_iter()
            .filter(|a| !a.starts_with("-Xmx") && !a.starts_with("-Xms")),
    );
    let java_major = v["javaVersion"]["majorVersion"]
        .as_u64()
        .map_or_else(|| crate::java::recommended_major(&inst.game_version), |m| m as u32);
    if defaults.gc_log && crate::perf::wants_gc_log(java_major, custom) {
        args.push(crate::perf::gc_log_arg());
    }
    if v["arguments"]["jvm"].is_array() {
        expand(&v["arguments"]["jvm"], &mut args);
    } else {
        // Pre-1.13 versions only list game arguments.
        args.push(sub("-Djava.library.path=${natives_directory}"));
        args.push("-cp".into());
        args.push(sub("${classpath}"));
    }
    args.push(
        v["mainClass"]
            .as_str()
            .unwrap_or("net.minecraft.client.main.Main")
            .into(),
    );
    if v["arguments"]["game"].is_array() {
        expand(&v["arguments"]["game"], &mut args);
    } else if let Some(s) = v["minecraftArguments"].as_str() {
        args.extend(s.split_whitespace().map(&sub));
    }
    args
}

/// Prepares and starts the game; returns the running process.
pub fn launch(
    inst: &Instance,
    dir: &Path,
    defaults: &Defaults,
    player: &Player,
    job: &Job,
) -> anyhow::Result<Child> {
    let p = prepare(inst, dir, defaults, job)?;
    let args = command(inst, dir, &p, player, defaults);
    job.detail(format!("Starting Minecraft as {}", player.name));
    let _ = fs::create_dir_all(inst.root.join("logs"));
    let log = fs::File::create(inst.root.join("logs").join("hopper-launch.log")).or_else(|_| {
        fs::create_dir_all(inst.root.join("logs"))?;
        fs::File::create(inst.root.join("logs").join("hopper-launch.log"))
    })?;
    let child = Command::new(&p.java)
        .args(&args)
        .current_dir(&inst.root)
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .spawn()
        .with_context(|| format!("starting {}", p.java.display()))?;
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maven_paths() {
        assert_eq!(
            maven_path("org.lwjgl:lwjgl:3.3.3"),
            "org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3.jar"
        );
        assert_eq!(
            maven_path("org.lwjgl:lwjgl:3.3.3:natives-windows"),
            "org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-windows.jar"
        );
        assert_eq!(
            maven_path("net.neoforged:neoforge:21.1.256:client@zip"),
            "net/neoforged/neoforge/21.1.256/neoforge-21.1.256-client.zip"
        );
    }

    #[test]
    fn rules() {
        let none = HashMap::new();
        // Rules for the OS this runs on and for one it doesn't.
        let other = if os::NAME == "osx" { "windows" } else { "osx" };
        let this = serde_json::json!([{ "action": "allow", "os": { "name": os::NAME } }]);
        let that = serde_json::json!([{ "action": "allow", "os": { "name": other } }]);
        let not_that = serde_json::json!([{ "action": "allow" }, { "action": "disallow", "os": { "name": other } }]);
        assert!(rules_allow(&this, &none));
        assert!(!rules_allow(&that, &none));
        assert!(rules_allow(&not_that, &none));
        let demo = serde_json::json!([{ "action": "allow", "features": { "is_demo_user": true } }]);
        assert!(!rules_allow(&demo, &none));
    }

    /// Prepares TNP from the CurseForge Install folder and prints the command
    /// line, without starting the game.
    /// `cargo test real_prepare -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_prepare() {
        let dir = Path::new(r"G:\Games\Minecraft\Curse\Install");
        let inst = crate::instances::load(Path::new(
            r"G:\Games\Minecraft\Curse\Instances\TNP Limitless 8",
        ))
        .unwrap();
        let jobs = crate::jobs::Jobs::default();
        let defaults = Defaults {
            memory_mb: 8192,
            ..Default::default()
        };
        let p = prepare(&inst, dir, &defaults, &jobs.start("prepare")).unwrap();
        let missing: Vec<_> = p.classpath.iter().filter(|c| !c.exists()).collect();
        println!("java {}", p.java.display());
        println!(
            "classpath {} entries, {} missing",
            p.classpath.len(),
            missing.len()
        );
        let args = command(&inst, dir, &p, &Player::offline("Tester"), &defaults);
        let shown: Vec<String> = args
            .iter()
            .map(|a| {
                if a.len() > 120 {
                    format!("{}…", &a[..120])
                } else {
                    a.clone()
                }
            })
            .collect();
        println!("{}", shown.join("\n"));
        assert!(missing.is_empty());
        assert!(args.iter().any(|a| a == "--gameDir"));
    }
}
