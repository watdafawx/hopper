//! Launching through Mojang's launcher, the way CurseForge does: install the
//! loader version into the launcher's folder, write a profile for the
//! instance into `launcher_profiles.json`, then start `minecraft.exe
//! --workDir`. Mojang's launcher keeps doing sign-in, assets and Java.

use crate::cf;
use crate::instances::Instance;
use crate::jobs::Job;
use anyhow::{Context, bail};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Mojang launcher version id for an instance loader id.
/// "neoforge-21.1.256" -> "neoforge-21.1.256"; "forge-47.3.0" -> "1.20.1-forge-47.3.0";
/// "fabric-0.16.5" -> "fabric-loader-0.16.5-1.21.1"; "" -> the bare game version.
pub fn version_id(loader: &str, game_version: &str) -> String {
    let l = loader.to_ascii_lowercase();
    if l.is_empty() {
        game_version.to_string()
    } else if l.starts_with("neoforge-") {
        loader.to_string()
    } else if let Some(v) = l.strip_prefix("forge-") {
        format!("{game_version}-forge-{v}")
    } else if let Some(v) = l.strip_prefix("fabric-") {
        format!("fabric-loader-{v}-{game_version}")
    } else if let Some(v) = l.strip_prefix("quilt-") {
        format!("quilt-loader-{v}-{game_version}")
    } else {
        loader.to_string()
    }
}

/// A Java to run loader installers with: the newest one CurseForge bundles,
/// else `java` on PATH.
pub fn installer_java(launcher_dir: &Path) -> PathBuf {
    let mut jres: Vec<PathBuf> = fs::read_dir(launcher_dir.join("java"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path().join("bin").join("java.exe"))
        .filter(|p| p.exists())
        .collect();
    jres.sort();
    jres.pop().unwrap_or_else(|| PathBuf::from("java"))
}

pub fn launcher_exe(launcher_dir: &Path) -> PathBuf {
    launcher_dir.join("minecraft.exe")
}

/// Makes sure `versions/<id>/<id>.json` exists for the instance's loader.
pub fn ensure_loader(inst: &Instance, launcher_dir: &Path, job: &Job) -> anyhow::Result<String> {
    let id = version_id(&inst.loader, &inst.game_version);
    let json = launcher_dir
        .join("versions")
        .join(&id)
        .join(format!("{id}.json"));
    if json.exists() || inst.loader.is_empty() {
        return Ok(id);
    }
    let l = inst.loader.to_ascii_lowercase();
    let mc = &inst.game_version;
    if let Some(v) = l
        .strip_prefix("fabric-")
        .or_else(|| l.strip_prefix("quilt-"))
    {
        let url = if l.starts_with("fabric-") {
            format!("https://meta.fabricmc.net/v2/versions/loader/{mc}/{v}/profile/json")
        } else {
            format!("https://meta.quiltmc.org/v3/versions/loader/{mc}/{v}/profile/json")
        };
        job.detail(format!("Fetching {id}"));
        let body = cf::agent().get(&url).call()?.body_mut().read_to_string()?;
        fs::create_dir_all(json.parent().unwrap_or(launcher_dir))?;
        fs::write(&json, body)?;
        return Ok(id);
    }

    let installer_url = if let Some(v) = l.strip_prefix("neoforge-") {
        format!(
            "https://maven.neoforged.net/releases/net/neoforged/neoforge/{v}/neoforge-{v}-installer.jar"
        )
    } else if let Some(v) = l.strip_prefix("forge-") {
        format!(
            "https://maven.minecraftforge.net/net/minecraftforge/forge/{mc}-{v}/forge-{mc}-{v}-installer.jar"
        )
    } else {
        bail!("Don't know how to install loader {}", inst.loader);
    };
    let installer = crate::paths::temp_dir().join(format!("hopper-{id}-installer.jar"));
    job.detail(format!("Downloading the {} installer", inst.loader));
    cf::download(&installer_url, &installer, |_| {})?;

    let java = installer_java(launcher_dir);
    // NeoForge renamed the flag; try the new spelling, then the old one.
    for flag in ["--install-client", "--installClient"] {
        let out = crate::loader_installer::run(
            &java,
            &installer,
            &[flag.as_ref(), launcher_dir.as_os_str()],
            &std::env::temp_dir(),
            &format!("Installing {}", inst.loader),
            job,
        )?;
        if json.exists() {
            let _ = fs::remove_file(&installer);
            return Ok(id);
        }
        log::warn!("{flag} failed: {}", out.last_line);
    }
    bail!(
        "The {} installer did not create {}",
        inst.loader,
        json.display()
    )
}

/// Writes (or refreshes) the instance's profile and makes it the most
/// recently used, so the launcher opens with it selected.
/// What instances fall back to when they leave a setting empty.
#[derive(Clone, Debug, Default)]
pub struct Defaults {
    pub memory_mb: u32,
    pub java_args: String,
    pub java_path: String,
    /// Have Java write a GC log for the session record
    /// ([`crate::perf::gc_log_arg`]).
    pub gc_log: bool,
}

pub fn write_profile(
    inst: &Instance,
    launcher_dir: &Path,
    version: &str,
    defaults: &Defaults,
) -> anyhow::Result<String> {
    let path = launcher_dir.join("launcher_profiles.json");
    let mut doc: serde_json::Value = fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| serde_json::json!({ "profiles": {}, "settings": {}, "version": 3 }));
    let key = format!("hopper-{}", Instance::folder_name(&inst.name));
    let mem = if inst.settings.memory_mb > 0 {
        inst.settings.memory_mb
    } else {
        defaults.memory_mb
    };
    let custom = custom_args(&inst.settings.java_args, &defaults.java_args);
    let xms = initial_memory(&inst.settings.java_args, &defaults.java_args, mem);
    let mut java_args = format!("-Xmx{mem}m -Xms{xms}m");
    if !custom.trim().is_empty() {
        // Drop any -Xmx/-Xms in the custom args; the memory setting owns those.
        // The launcher re-parses this string, so keep each kept argument as
        // the user wrote it (quotes and all).
        let extra: Vec<String> = split_args_raw(custom)
            .into_iter()
            .filter(|(_, a)| !a.starts_with("-Xmx") && !a.starts_with("-Xms"))
            .map(|(raw, _)| raw)
            .collect();
        java_args = format!("{java_args} {}", extra.join(" "));
    }
    // The launcher's bundled Java matches the game version's need.
    if defaults.gc_log
        && crate::perf::wants_gc_log(crate::java::recommended_major(&inst.game_version), custom)
    {
        let _ = fs::create_dir_all(inst.root.join("logs"));
        java_args = format!("{java_args} {}", crate::perf::gc_log_arg());
    }
    let now = crate::time::now_iso();
    let mut profile = serde_json::json!({
        "created": now,
        "gameDir": inst.root.to_string_lossy(),
        "icon": "Furnace",
        "javaArgs": java_args,
        "lastUsed": now,
        "lastVersionId": version,
        "name": inst.name,
        "resolution": { "width": inst.settings.width, "height": inst.settings.height },
        "type": "custom",
    });
    let java = if inst.settings.java_path.trim().is_empty() {
        &defaults.java_path
    } else {
        &inst.settings.java_path
    };
    if !java.trim().is_empty() {
        profile["javaDir"] = serde_json::Value::String(java.trim().to_string());
    }
    if let Some(old) = doc["profiles"]
        .get(&key)
        .and_then(|p| p.get("created"))
        .cloned()
    {
        profile["created"] = old;
    }
    doc["profiles"][&key] = profile;
    let tmp = path.with_extension("json.hopper-tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(&doc)?)?;
    let _ = fs::copy(&path, path.with_extension("json.bak"));
    fs::rename(tmp, &path)?;
    Ok(key)
}

/// Splits a Java argument string the way Windows splits a command line
/// (`CommandLineToArgvW`): whitespace separates arguments except inside
/// double quotes, which are removed; `\"` is a literal quote, and backslashes
/// are only special right before a quote (2n backslashes + `"` give n
/// backslashes and toggle quoting; 2n+1 give n and a literal `"`). So
/// `-XX:HeapDumpPath="C:\My Dumps"` stays one argument,
/// `-XX:HeapDumpPath=C:\My Dumps`. Single quotes are ordinary characters,
/// as on Windows (paths like `O'Brien` keep working).
pub fn split_args(s: &str) -> Vec<String> {
    split_args_raw(s).into_iter().map(|(_, a)| a).collect()
}

/// [`split_args`], with each argument's original text beside its value.
/// The extra Java arguments an instance starts with: its own, or the
/// defaults when it has none past memory flags (the memory slider writes
/// those into the arguments).
pub fn custom_args<'a>(inst_args: &'a str, default_args: &'a str) -> &'a str {
    if crate::jvm_args::without_memory(inst_args).is_empty() {
        default_args
    } else {
        inst_args
    }
}

/// `-Xms` in MB: the instance's, else the defaults', at most `max_mb`;
/// 256 when neither sets one.
pub fn initial_memory(inst_args: &str, default_args: &str, max_mb: u32) -> u32 {
    crate::jvm_args::memory(inst_args)
        .0
        .or(crate::jvm_args::memory(default_args).0)
        .unwrap_or(256)
        .min(max_mb)
        .max(1)
}

pub fn split_args_raw(s: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        let mut arg = String::new();
        let mut quoted = false;
        while i < chars.len() {
            let c = chars[i];
            if c == '\\' {
                let mut n = 0;
                while i < chars.len() && chars[i] == '\\' {
                    n += 1;
                    i += 1;
                }
                if chars.get(i) == Some(&'"') {
                    arg.extend(std::iter::repeat_n('\\', n / 2));
                    if n % 2 == 1 {
                        arg.push('"');
                        i += 1;
                    }
                    // Even: the quote is a delimiter, handled next round.
                } else {
                    arg.extend(std::iter::repeat_n('\\', n));
                }
                continue;
            }
            if c == '"' {
                // `""` inside quotes is a literal quote, as in CommandLineToArgvW.
                if quoted && chars.get(i + 1) == Some(&'"') {
                    arg.push('"');
                    i += 2;
                    continue;
                }
                quoted = !quoted;
                i += 1;
                continue;
            }
            if c.is_whitespace() && !quoted {
                break;
            }
            arg.push(c);
            i += 1;
        }
        out.push((chars[start..i].iter().collect(), arg));
    }
    out
}

/// Full launch: loader, profile, then the launcher.
pub fn launch(
    inst: &Instance,
    launcher_dir: &Path,
    defaults: &Defaults,
    job: &Job,
) -> anyhow::Result<()> {
    let exe = launcher_exe(launcher_dir);
    if !exe.exists() {
        bail!(
            "No minecraft.exe in {}. Point Settings → Folders → Launcher at CurseForge's Install folder.",
            launcher_dir.display()
        );
    }
    let version = ensure_loader(inst, launcher_dir, job)?;
    write_profile(inst, launcher_dir, &version, defaults)?;
    job.detail("Starting the Minecraft launcher; press Play there");
    Command::new(&exe)
        .arg("--workDir")
        .arg(launcher_dir)
        .current_dir(launcher_dir)
        .spawn()
        .with_context(|| format!("starting {}", exe.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_from_the_arguments() {
        // Memory flags alone still fall back to the defaults' other flags.
        assert_eq!(custom_args("-Xms4G -Xmx8G", "-XX:+UseZGC"), "-XX:+UseZGC");
        assert_eq!(custom_args("", "-XX:+UseZGC"), "-XX:+UseZGC");
        assert_eq!(custom_args("-Xms4G -XX:+UseG1GC", "-XX:+UseZGC"), "-Xms4G -XX:+UseG1GC");
        assert_eq!(initial_memory("-Xms4G -Xmx8G", "", 8192), 4096);
        assert_eq!(initial_memory("", "-Xms2G", 8192), 2048);
        assert_eq!(initial_memory("", "", 8192), 256);
        // Never above -Xmx, or Java won't start.
        assert_eq!(initial_memory("-Xms12G", "", 8192), 8192);
        assert_eq!(initial_memory("", "", 128), 128);
    }

    #[test]
    fn splits_quoted_args() {
        assert_eq!(
            split_args(r#"-XX:+UseG1GC  -XX:HeapDumpPath="C:\My Dumps" -Dx=1"#),
            ["-XX:+UseG1GC", r"-XX:HeapDumpPath=C:\My Dumps", "-Dx=1"]
        );
        assert_eq!(split_args(r#""-Dname=a b" c"#), ["-Dname=a b", "c"]);
        // Escaped quotes stay in the value.
        assert_eq!(split_args(r#"-Dq=\"hi\" x"#), [r#"-Dq="hi""#, "x"]);
        assert_eq!(split_args(r#""say \"a b\"" z"#), [r#"say "a b""#, "z"]);
        // 2n backslashes before a quote: n backslashes, then the quote ends.
        assert_eq!(split_args(r#""C:\dir\\" next"#), [r"C:\dir\", "next"]);
        // Other backslashes are path separators; single quotes are plain.
        assert_eq!(
            split_args(r"-Dp=C:\a\b -Du=O'Brien"),
            [r"-Dp=C:\a\b", "-Du=O'Brien"]
        );
        assert!(split_args("  \t ").is_empty());
        assert_eq!(split_args(r#"a "" b"#), ["a", "", "b"]);
        let raw = split_args_raw(r#"-Xmx2G -XX:HeapDumpPath="C:\My Dumps""#);
        assert_eq!(raw[1].0, r#"-XX:HeapDumpPath="C:\My Dumps""#);
        assert_eq!(raw[1].1, r"-XX:HeapDumpPath=C:\My Dumps");
    }

    #[test]
    fn version_ids() {
        assert_eq!(
            version_id("neoforge-21.1.256", "1.21.1"),
            "neoforge-21.1.256"
        );
        assert_eq!(version_id("forge-47.3.0", "1.20.1"), "1.20.1-forge-47.3.0");
        assert_eq!(
            version_id("fabric-0.16.5", "1.21.1"),
            "fabric-loader-0.16.5-1.21.1"
        );
        assert_eq!(version_id("", "1.21.1"), "1.21.1");
    }

    #[test]
    fn profile_keeps_other_profiles() {
        let dir = std::env::temp_dir().join(format!("hopper-prof-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("launcher_profiles.json"),
            r#"{"profiles":{"x":{"name":"keep"}},"settings":{},"version":3}"#,
        )
        .unwrap();
        let inst = Instance {
            name: "My Pack".into(),
            root: dir.clone(),
            game_version: "1.21.1".into(),
            ..Default::default()
        };
        let defaults = Defaults {
            memory_mb: 4096,
            ..Default::default()
        };
        let key = write_profile(&inst, &dir, "1.21.1", &defaults).unwrap();
        let gc = Defaults { gc_log: true, ..defaults.clone() };
        let k2 = write_profile(&inst, &dir, "1.21.1", &gc).unwrap();
        let v: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join("launcher_profiles.json")).unwrap()).unwrap();
        assert!(v["profiles"][&k2]["javaArgs"].as_str().unwrap().ends_with(&crate::perf::gc_log_arg()));
        write_profile(&inst, &dir, "1.21.1", &defaults).unwrap();
        let v: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join("launcher_profiles.json")).unwrap()).unwrap();
        assert_eq!(v["profiles"]["x"]["name"], "keep");
        assert_eq!(v["profiles"][&key]["lastVersionId"], "1.21.1");
        assert!(
            v["profiles"][&key]["javaArgs"]
                .as_str()
                .unwrap()
                .contains("-Xmx4096m")
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
