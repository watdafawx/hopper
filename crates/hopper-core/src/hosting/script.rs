//! The run script. Hopper writes a `run.bat` like NeoForge's for every
//! loader, so [`crate::server`] and [`crate::server_update`] read it the
//! same way, and a hosted server can still be started by double-clicking.
//! [`launch_command`] turns it back into a java command line, so Hopper
//! starts java itself (no cmd window) exactly as the script would.

use super::jvm::{ARGS_FILE, JvmArgs, tokenize};
use anyhow::Context as _;
use std::fs;
use std::path::{Path, PathBuf};

pub const RUN_BAT: &str = "run.bat";

/// What the run line starts after the JVM arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchTarget {
    /// NeoForge or Forge 1.17+: `@libraries/.../win_args.txt` (forward slashes).
    ArgsFile(String),
    /// `-jar <name>`: vanilla, Fabric, Quilt, old Forge.
    Jar(String),
}

impl LaunchTarget {
    fn tokens(&self) -> String {
        match self {
            LaunchTarget::ArgsFile(p) => format!("@{}", p.replace('\\', "/")),
            LaunchTarget::Jar(j) => format!("-jar {}", quote(j)),
        }
    }
}

fn quote(s: &str) -> String {
    if s.contains(char::is_whitespace) {
        format!("\"{s}\"")
    } else {
        s.to_string()
    }
}

/// `java.exe` next to a `javaw.exe` (javaw has no console streams worth
/// having for a server).
pub fn console_java(java: &Path) -> PathBuf {
    let is_javaw = java
        .file_name()
        .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("javaw.exe"));
    if is_javaw {
        let j = java.with_file_name("java.exe");
        if j.exists() {
            return j;
        }
    }
    java.to_path_buf()
}

/// The run.bat text. Java 9+ reads `@user_jvm_args.txt` itself; Java 8
/// can't, so the script reads the file into a variable first.
pub fn run_bat(java: &Path, target: &LaunchTarget, java_major: u32) -> String {
    let java = java.to_string_lossy();
    let head = format!(
        "@echo off\r\nREM Written by Hopper. Memory and GC flags are in {ARGS_FILE}.\r\n"
    );
    if java_major >= 9 || java_major == 0 {
        format!(
            "{head}\"{java}\" @{ARGS_FILE} {} nogui %*\r\npause\r\n",
            target.tokens()
        )
    } else {
        format!(
            "{head}setlocal enabledelayedexpansion\r\nset ARGS=\r\nfor /f \"usebackq eol=# delims=\" %%a in (\"{ARGS_FILE}\") do set ARGS=!ARGS! %%a\r\n\"{java}\" !ARGS! {} nogui %*\r\npause\r\n",
            target.tokens()
        )
    }
}

pub fn write_run_bat(root: &Path, java: &Path, target: &LaunchTarget, java_major: u32) -> anyhow::Result<()> {
    fs::write(root.join(RUN_BAT), run_bat(java, target, java_major))
        .with_context(|| format!("writing {}", root.join(RUN_BAT).display()))
}

/// The launch line: the first line that isn't a comment and mentions java
/// (the same rule [`crate::server`] uses).
fn launch_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|l| {
        let low = l.to_ascii_lowercase();
        !(low.starts_with("rem") || low.starts_with("::") || low.starts_with('@'))
            && low.contains("java")
    })
}

/// A java command line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Launch {
    pub java: PathBuf,
    pub args: Vec<String>,
    /// Why Hopper changed something about it (shown in the console).
    pub note: Option<String>,
}

/// The command `run.bat` would run, with `user_jvm_args.txt` expanded into
/// it (so it works on Java 8 too) and `%*` dropped.
pub fn parse_launch(text: &str, jvm: &JvmArgs) -> Option<Launch> {
    let mut toks = tokenize(launch_line(text)?).into_iter();
    let java = PathBuf::from(toks.next()?);
    let mut args = Vec::new();
    for t in toks {
        let t = t.strip_suffix("%*").unwrap_or(&t).to_string();
        match t.as_str() {
            "" => {}
            "!ARGS!" | "%ARGS%" => args.extend(jvm.args()),
            a if a.eq_ignore_ascii_case(&format!("@{ARGS_FILE}")) => args.extend(jvm.args()),
            _ => args.push(t),
        }
    }
    if !args.iter().any(|a| a.eq_ignore_ascii_case("nogui")) {
        args.push("nogui".into());
    }
    Some(Launch { java, args, note: None })
}

/// Start scripts Hopper reads, in order (run.bat is NeoForge's and
/// Hopper's; start.bat is common in packs and other panels).
pub const SCRIPTS: [&str; 2] = [RUN_BAT, "start.bat"];

/// How to start the server in `root`: its run.bat (or start.bat), else
/// `server.jar` with `java` from PATH. The folder is used as it is; when the
/// script says plain `java` (whatever is first on PATH, which changes when
/// a new Java is installed), the installed Java that fits its Minecraft
/// version is used instead.
pub fn launch_command(root: &Path) -> anyhow::Result<Launch> {
    let jvm = JvmArgs::load(root)?;
    let mut found = None;
    for script in SCRIPTS {
        if let Ok(text) = fs::read_to_string(root.join(script))
            && let Some(l) = parse_launch(&text, &jvm)
        {
            found = Some((l, text));
            break;
        }
    }
    let (mut l, text) = match found {
        Some(f) => f,
        None => {
            let jar = ["server.jar", "fabric-server-launch.jar", "quilt-server-launch.jar"]
                .into_iter()
                .find(|j| root.join(j).is_file())
                .with_context(|| format!("{} has no run.bat, start.bat or server.jar to start", root.display()))?;
            let mut args = jvm.args();
            args.extend(["-jar".into(), jar.into(), "nogui".into()]);
            (Launch { java: "java".into(), args, note: None }, String::new())
        }
    };
    if is_bare_java(&l.java)
        && let Some(mc) = minecraft_version(root, &text)
        && let Some(best) = crate::java::best(&crate::java::detect(Path::new("")), &mc, crate::java::Side::Server)
    {
        let exe = console_java(&best.install.exe);
        l.note = Some(format!(
            "The start script says plain \"java\"; using {} (Java {}) for Minecraft {mc}.",
            exe.display(),
            best.install.major
        ));
        l.java = exe;
    }
    l.java = console_java(&l.java);
    Ok(l)
}

/// Whether `java` is plain `java` (looked up on PATH).
pub fn is_bare_java(java: &Path) -> bool {
    matches!(java.to_string_lossy().to_ascii_lowercase().as_str(), "java" | "java.exe" | "javaw" | "javaw.exe")
}

/// The Minecraft version a server folder is for: from Forge's or
/// NeoForge's library path in its start script, Fabric's server jar, or
/// what Hopper noted when it made the server.
pub fn minecraft_version(root: &Path, script: &str) -> Option<String> {
    let re_forge = regex::Regex::new(r"minecraftforge/forge/(\d+\.\d+(?:\.\d+)?)-").ok()?;
    if let Some(c) = re_forge.captures(script) {
        return Some(c[1].to_string());
    }
    let re_neo = regex::Regex::new(r"neoforged/neoforge/(\d+)\.(\d+)\.").ok()?;
    if let Some(c) = re_neo.captures(script) {
        let (a, b): (u32, u32) = (c[1].parse().ok()?, c[2].parse().ok()?);
        // 21.1.x is for 1.21.1; from 26.1 on the versions match Minecraft's.
        return Some(if a >= 26 {
            format!("{a}.{b}")
        } else if b == 0 {
            format!("1.{a}")
        } else {
            format!("1.{a}.{b}")
        });
    }
    for e in fs::read_dir(root.join(".fabric").join("server")).into_iter().flatten().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if let Some(v) = n.strip_suffix("-server.jar")
            && v.chars().next().is_some_and(|c| c.is_ascii_digit())
        {
            return Some(v.to_string());
        }
    }
    super::HostedMeta::load(root).map(|m| m.minecraft).filter(|v| !v.is_empty())
}

/// Points the launch line at another java, keeping everything else.
pub fn rewrite_java(text: &str, java: &Path) -> Option<String> {
    let line = launch_line(text)?;
    let rest = if let Some(r) = line.strip_prefix('"') {
        &r[r.find('"')? + 1..]
    } else {
        &line[line.find(char::is_whitespace).unwrap_or(line.len())..]
    };
    let new_line = format!("\"{}\"{rest}", java.to_string_lossy());
    Some(text.replacen(line, &new_line, 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hosting::jvm::JvmPreset;

    fn jvm() -> JvmArgs {
        JvmArgs::new(4096, JvmPreset::Plain, 21)
    }

    #[test]
    fn neoforge_script_matches_what_server_rs_reads() {
        let t = LaunchTarget::ArgsFile("libraries/net/neoforged/neoforge/21.1.256/win_args.txt".into());
        let bat = run_bat(Path::new(r"G:\jdk 21\bin\java.exe"), &t, 21);
        assert!(bat.contains("\"G:\\jdk 21\\bin\\java.exe\" @user_jvm_args.txt @libraries/net/neoforged/neoforge/21.1.256/win_args.txt nogui %*"));
        // server_update finds and rewrites the version token in it.
        let l = parse_launch(&bat, &jvm()).unwrap();
        assert_eq!(l.java, PathBuf::from(r"G:\jdk 21\bin\java.exe"));
        assert_eq!(
            l.args,
            ["-Xms4G", "-Xmx4G", "@libraries/net/neoforged/neoforge/21.1.256/win_args.txt", "nogui"]
        );
    }

    #[test]
    fn jar_script_and_java8() {
        let t = LaunchTarget::Jar("fabric-server-launch.jar".into());
        let l = parse_launch(&run_bat(Path::new("java"), &t, 21), &jvm()).unwrap();
        assert_eq!(l.args, ["-Xms4G", "-Xmx4G", "-jar", "fabric-server-launch.jar", "nogui"]);

        let old = run_bat(Path::new(r"C:\jre8\bin\java.exe"), &LaunchTarget::Jar("forge-1.12.2-14.23.5.2860.jar".into()), 8);
        assert!(old.contains("!ARGS!"));
        assert!(!old.contains("@user_jvm_args"));
        let l = parse_launch(&old, &jvm()).unwrap();
        assert_eq!(l.java, PathBuf::from(r"C:\jre8\bin\java.exe"));
        assert_eq!(l.args, ["-Xms4G", "-Xmx4G", "-jar", "forge-1.12.2-14.23.5.2860.jar", "nogui"]);
    }

    #[test]
    fn reads_installer_scripts_too() {
        // NeoForge's own script: nogui glued to %*, a trailing pause.
        let bat = "@echo off\r\nREM Forge requires a configured set of both JVM and program arguments.\r\njava @user_jvm_args.txt @libraries/net/neoforged/neoforge/21.1.256/win_args.txt nogui%*\r\npause\r\n";
        let l = parse_launch(bat, &JvmArgs::default()).unwrap();
        assert_eq!(l.java, PathBuf::from("java"));
        assert_eq!(l.args, ["@libraries/net/neoforged/neoforge/21.1.256/win_args.txt", "nogui"]);
    }

    #[test]
    fn swaps_the_java() {
        let bat = "@echo off\r\njava @user_jvm_args.txt -jar server.jar nogui %*\r\n";
        let out = rewrite_java(bat, Path::new(r"C:\j\bin\java.exe")).unwrap();
        assert!(out.contains("\"C:\\j\\bin\\java.exe\" @user_jvm_args.txt -jar server.jar nogui %*"));
        let again = rewrite_java(&out, Path::new("java")).unwrap();
        assert!(again.contains("\"java\" @user_jvm_args.txt -jar"));
    }

    #[test]
    fn finds_minecraft_versions() {
        let r = Path::new("Z:/nowhere");
        let v = |c: &str| minecraft_version(r, c);
        assert_eq!(v("java @libraries/net/minecraftforge/forge/1.20.1-47.4.10/win_args.txt").as_deref(), Some("1.20.1"));
        assert_eq!(v("java @libraries/net/neoforged/neoforge/21.1.219/win_args.txt").as_deref(), Some("1.21.1"));
        assert_eq!(v("java @libraries/net/neoforged/neoforge/20.4.237/win_args.txt").as_deref(), Some("1.20.4"));
        assert_eq!(v("java @libraries/net/neoforged/neoforge/21.0.167/win_args.txt").as_deref(), Some("1.21"));
        assert_eq!(v("java @libraries/net/neoforged/neoforge/26.3.0.97/win_args.txt").as_deref(), Some("26.3"));
        assert_eq!(v("java -jar server.jar"), None);
        assert!(is_bare_java(Path::new("java")) && is_bare_java(Path::new("JAVA.EXE")));
        assert!(!is_bare_java(Path::new(r"G:\jdk\bin\java.exe")));
    }

    #[test]
    fn reads_start_bat_and_fabric_versions() {
        let root = std::env::temp_dir().join(format!("hopper-script-start-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".fabric/server")).unwrap();
        fs::write(root.join(".fabric/server/1.20.1-server.jar"), "").unwrap();
        // A pack's own script, as another panel or the pack left it.
        fs::write(root.join("start.bat"), "java -Dfabric.log.level=DEBUG -Xmx8G -jar fabric-server-launch.jar nogui\r\npause\r\n").unwrap();
        let l = launch_command(&root).unwrap();
        assert!(l.args.contains(&"-Xmx8G".to_string()) && l.args.contains(&"fabric-server-launch.jar".to_string()), "{:?}", l.args);
        assert_eq!(minecraft_version(&root, ""), Some("1.20.1".into()));
        // With a Java for 1.20.1 installed, plain java was swapped for it.
        if l.note.is_some() {
            assert!(!is_bare_java(&l.java), "{:?}", l.java);
        }
        let _ = fs::remove_dir_all(&root);
    }
}
