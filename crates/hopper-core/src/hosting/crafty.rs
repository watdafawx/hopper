//! Taking a server over from Crafty Controller: Hopper runs it from then on,
//! the way Crafty did. Crafty's settings are read from a copy of its
//! database (never the file Crafty has open); the server folder gets a
//! run.bat with Crafty's exact start command (its Java, memory and
//! arguments) and Crafty's marker file is set aside. [`give_back`] undoes
//! both.

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Crafty's marker in a folder it manages.
pub const MARKER: &str = "crafty_managed.txt";
/// The marker, set aside while Hopper runs the server.
pub const MARKER_ASIDE: &str = "crafty_managed.txt.hopper";
/// What the takeover changed, for giving it back.
pub const RECORD: &str = ".hopper-takeover.json";
/// run.bat as it was before the takeover.
pub const RUN_BAT_BEFORE: &str = "run.bat.before-hopper";

/// A server as Crafty has it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CraftyServer {
    pub id: String,
    pub name: String,
    /// Crafty's folder for it (often a junction to the real one).
    pub path: PathBuf,
    /// The command Crafty starts it with.
    pub command: String,
    pub port: u16,
    pub auto_start: bool,
    pub crash_detection: bool,
}

/// What a takeover did.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Takeover {
    pub from: String,
    pub crafty: CraftyServer,
    /// Unix seconds.
    pub at: u64,
    /// A run.bat was there before (now in [`RUN_BAT_BEFORE`]).
    pub had_run_bat: bool,
    /// The Minecraft version found in the folder, if any.
    #[serde(default)]
    pub minecraft: Option<String>,
    /// Crafty's command said plain `java` (whatever is on PATH); this is
    /// the Java put in its place.
    #[serde(default)]
    pub java_pinned: Option<PathBuf>,
}

impl Takeover {
    pub fn load(root: &Path) -> Option<Takeover> {
        serde_json::from_str(&fs::read_to_string(root.join(RECORD)).ok()?).ok()
    }
}

/// Where Crafty's database may be: `CRAFTY_DIR`, then `crafty` folders at
/// the top of each drive and in Program Files.
pub fn find_databases() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = std::env::var_os("CRAFTY_DIR") {
        dirs.push(d.into());
    }
    for letter in 'C'..='Z' {
        dirs.push(PathBuf::from(format!("{letter}:\\crafty")));
        dirs.push(PathBuf::from(format!("{letter}:\\Crafty")));
        dirs.push(PathBuf::from(format!("{letter}:\\crafty-4")));
    }
    for v in ["ProgramFiles", "ProgramData"] {
        if let Some(p) = std::env::var_os(v) {
            dirs.push(PathBuf::from(p).join("Crafty"));
        }
    }
    let mut out: Vec<PathBuf> = Vec::new();
    for d in dirs {
        let db = d.join("app").join("config").join("db").join("crafty.sqlite");
        if db.is_file() && !out.iter().any(|o| same_path(o, &db)) {
            out.push(db);
        }
    }
    out
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Every server in Crafty's database at `db`, read from a copy of it.
pub fn read_servers(db: &Path) -> anyhow::Result<Vec<CraftyServer>> {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    let tmp = std::env::temp_dir().join(format!("hopper-crafty-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&tmp)?;
    let result = (|| {
        let copy = tmp.join("crafty.sqlite");
        fs::copy(db, &copy).with_context(|| format!("copying {}", db.display()))?;
        // Recent changes may still be in the write-ahead log.
        let wal = db.with_extension("sqlite-wal");
        if wal.is_file() {
            let _ = fs::copy(&wal, tmp.join("crafty.sqlite-wal"));
        }
        let conn = rusqlite::Connection::open(&copy)?;
        let mut q = conn.prepare(
            "SELECT server_id, server_name, path, execution_command, server_port, auto_start, crash_detection FROM servers",
        )?;
        let rows = q.query_map([], |r| {
            Ok(CraftyServer {
                id: r.get(0)?,
                name: r.get::<_, String>(1)?.trim().to_string(),
                path: PathBuf::from(r.get::<_, String>(2)?),
                command: r.get(3)?,
                port: r.get::<_, i64>(4)?.clamp(0, 65535) as u16,
                auto_start: r.get::<_, i64>(5)? != 0,
                crash_detection: r.get::<_, i64>(6)? != 0,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    })();
    let _ = fs::remove_dir_all(&tmp);
    result
}

/// Crafty's entry for the server folder `root` (its folder, or a junction
/// to it), from any Crafty database found.
pub fn entry_for(root: &Path) -> anyhow::Result<CraftyServer> {
    let dbs = find_databases();
    if dbs.is_empty() {
        bail!("Couldn't find Crafty's database (looked for crafty\\app\\config\\db\\crafty.sqlite). Set CRAFTY_DIR to Crafty's folder.");
    }
    for db in &dbs {
        if let Some(s) = read_servers(db)?.into_iter().find(|s| same_path(&s.path, root)) {
            return Ok(s);
        }
    }
    bail!("Crafty has no server in {}.", root.display())
}

/// run.bat for Crafty's start command.
pub fn run_bat_for(command: &str, had_run_bat: bool) -> String {
    let cmd = command.trim();
    let cmd = if cmd.contains("%*") { cmd.to_string() } else { format!("{cmd} %*") };
    let before = if had_run_bat { format!("REM The run.bat from before is in {RUN_BAT_BEFORE}.\r\n") } else { String::new() };
    format!("@echo off\r\nREM Hopper Server took this server over from Crafty; this is Crafty's start command.\r\n{before}{cmd}\r\n")
}

/// Whether a command starts plain `java` (whatever is first on PATH).
pub fn uses_bare_java(command: &str) -> bool {
    let first = super::jvm::tokenize(command.trim()).into_iter().next().unwrap_or_default();
    matches!(first.to_ascii_lowercase().as_str(), "java" | "java.exe")
}

/// The Minecraft version a server folder is for: from Forge's or
/// NeoForge's library path in its start command, Fabric's server jar, or
/// what Hopper noted when it made the server.
pub fn minecraft_version(root: &Path, command: &str) -> Option<String> {
    let re_forge = regex::Regex::new(r"minecraftforge/forge/(\d+\.\d+(?:\.\d+)?)-").ok()?;
    if let Some(c) = re_forge.captures(command) {
        return Some(c[1].to_string());
    }
    let re_neo = regex::Regex::new(r"neoforged/neoforge/(\d+)\.(\d+)\.").ok()?;
    if let Some(c) = re_neo.captures(command) {
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
    let fabric = root.join(".fabric").join("server");
    for e in fs::read_dir(&fabric).into_iter().flatten().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if let Some(v) = n.strip_suffix("-server.jar")
            && v.chars().next().is_some_and(|c| c.is_ascii_digit())
        {
            return Some(v.to_string());
        }
    }
    super::HostedMeta::load(root).map(|m| m.minecraft).filter(|v| !v.is_empty())
}

/// Takes the server in `root` over from Crafty, as `crafty` describes it.
/// The caller makes sure Crafty isn't running it.
/// `java` goes in place of a plain `java` in Crafty's command (see
/// [`uses_bare_java`]), so the server keeps the Java it needs whatever
/// PATH says later.
pub fn take_over(root: &Path, crafty: &CraftyServer, java: Option<&Path>) -> anyhow::Result<Takeover> {
    if !root.join(MARKER).is_file() {
        bail!("{} isn't marked as Crafty's.", root.display());
    }
    let run_bat = root.join(super::script::RUN_BAT);
    let had_run_bat = run_bat.is_file();
    if had_run_bat {
        fs::copy(&run_bat, root.join(RUN_BAT_BEFORE)).context("keeping the old run.bat")?;
    }
    let java_pinned = java.filter(|_| uses_bare_java(&crafty.command)).map(Path::to_path_buf);
    let command = match &java_pinned {
        Some(j) => super::script::rewrite_java(&format!("{}\n", crafty.command), j)
            .map(|c| c.trim_end().to_string())
            .unwrap_or_else(|| crafty.command.clone()),
        None => crafty.command.clone(),
    };
    let t = Takeover {
        from: "Crafty".into(),
        crafty: crafty.clone(),
        at: now(),
        had_run_bat,
        minecraft: minecraft_version(root, &crafty.command),
        java_pinned,
    };
    fs::write(root.join(RECORD), serde_json::to_string_pretty(&t)?)?;
    fs::write(&run_bat, run_bat_for(&command, had_run_bat)).context("writing run.bat")?;
    fs::rename(root.join(MARKER), root.join(MARKER_ASIDE)).context("setting Crafty's marker aside")?;
    Ok(t)
}

/// Gives a taken-over server back to Crafty: its marker and the old
/// run.bat return.
pub fn give_back(root: &Path) -> anyhow::Result<()> {
    let t = Takeover::load(root).context("This server wasn't taken over from Crafty.")?;
    let run_bat = root.join(super::script::RUN_BAT);
    if t.had_run_bat {
        fs::copy(root.join(RUN_BAT_BEFORE), &run_bat).context("putting the old run.bat back")?;
        let _ = fs::remove_file(root.join(RUN_BAT_BEFORE));
    } else {
        let _ = fs::remove_file(&run_bat);
    }
    if root.join(MARKER_ASIDE).is_file() {
        fs::rename(root.join(MARKER_ASIDE), root.join(MARKER))?;
    }
    fs::remove_file(root.join(RECORD))?;
    Ok(())
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hosting::jvm::JvmArgs;
    use crate::hosting::script::{RUN_BAT, parse_launch};

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopper-crafty-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn reads_a_crafty_database() {
        let dir = temp("db");
        let db = dir.join("crafty.sqlite");
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch(
            "CREATE TABLE servers (server_id TEXT, server_name TEXT, path TEXT, execution_command TEXT, server_port INTEGER, auto_start INTEGER, crash_detection INTEGER, executable TEXT);
             INSERT INTO servers VALUES ('abc', 'TNP-LL ', 'G:/crafty/servers/abc', '\"G:\\jdk\\bin\\java.exe\" @user_jvm_args.txt nogui  %*', 25567, 0, 1, 'x.jar');",
        )
        .unwrap();
        drop(c);
        let s = read_servers(&db).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!((s[0].name.as_str(), s[0].port, s[0].auto_start, s[0].crash_detection), ("TNP-LL", 25567, false, true));
        assert!(fs::read_dir(std::env::temp_dir()).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with(&format!("hopper-crafty-{}-", std::process::id()))), "copy cleaned up");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn takes_over_and_gives_back() {
        let root = temp("take");
        fs::write(root.join(MARKER), "The server is managed by Crafty Controller.").unwrap();
        fs::write(root.join(RUN_BAT), "@echo off\r\njava @user_jvm_args.txt @libraries/x/win_args.txt %*\r\npause\r\n").unwrap();
        fs::write(root.join("user_jvm_args.txt"), "-Xmx6G\n").unwrap();
        let crafty = CraftyServer {
            id: "abc".into(),
            name: "TNP".into(),
            path: root.clone(),
            command: "\"G:\\graalvm jdk\\bin\\java.exe\" @user_jvm_args.txt @libraries/x/win_args.txt nogui  %*".into(),
            port: 25567,
            auto_start: false,
            crash_detection: false,
        };
        let t = take_over(&root, &crafty, Some(Path::new(r"C:\jdk17\bin\java.exe"))).unwrap();
        assert!(t.had_run_bat);
        assert_eq!(t.java_pinned, None, "Crafty named its Java: kept");
        assert!(!root.join(MARKER).exists() && root.join(MARKER_ASIDE).exists());
        // Hopper now starts it with Crafty's Java and arguments.
        let l = parse_launch(&fs::read_to_string(root.join(RUN_BAT)).unwrap(), &JvmArgs::load(&root).unwrap()).unwrap();
        assert_eq!(l.java, PathBuf::from(r"G:\graalvm jdk\bin\java.exe"));
        assert_eq!(l.args.iter().filter(|a| a.as_str() == "nogui").count(), 1);
        assert!(l.args.contains(&"@libraries/x/win_args.txt".to_string()) && l.args.iter().any(|a| a.starts_with("-Xmx")), "{:?}", l.args);
        assert_eq!(Takeover::load(&root).unwrap().crafty.id, "abc");
        assert!(take_over(&root, &crafty, None).is_err(), "not Crafty's any more");

        give_back(&root).unwrap();
        assert!(root.join(MARKER).exists() && !root.join(MARKER_ASIDE).exists() && !root.join(RECORD).exists());
        assert!(fs::read_to_string(root.join(RUN_BAT)).unwrap().contains("pause"), "old run.bat back");
        assert!(!root.join(RUN_BAT_BEFORE).exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn crafty_commands_become_run_bats() {
        let b = run_bat_for(r#"java -Xms1024M -Xmx4096M -jar "fabric-server-launch.jar" nogui"#, false);
        assert!(!b.contains(RUN_BAT_BEFORE));
        assert!(b.ends_with("java -Xms1024M -Xmx4096M -jar \"fabric-server-launch.jar\" nogui %*\r\n"), "{b}");
        let l = parse_launch(&b, &JvmArgs::default()).unwrap();
        assert_eq!(l.args, ["-Xms1024M", "-Xmx4096M", "-jar", "fabric-server-launch.jar", "nogui"]);
    }

    #[test]
    fn pins_java_for_plain_java_commands() {
        let root = temp("pin");
        fs::write(root.join(MARKER), "").unwrap();
        fs::create_dir_all(root.join(".fabric/server")).unwrap();
        fs::write(root.join(".fabric/server/1.20.1-server.jar"), "").unwrap();
        let crafty = CraftyServer {
            id: "b".into(),
            name: "BCG".into(),
            path: root.clone(),
            command: r#"java -Xmx4096M -jar "fabric-server-launch.jar" nogui"#.into(),
            port: 25570,
            auto_start: false,
            crash_detection: false,
        };
        let t = take_over(&root, &crafty, Some(Path::new(r"C:\Program Files\jdk-17\bin\java.exe"))).unwrap();
        assert_eq!(t.minecraft.as_deref(), Some("1.20.1"));
        let l = parse_launch(&fs::read_to_string(root.join(RUN_BAT)).unwrap(), &JvmArgs::default()).unwrap();
        assert_eq!(l.java, PathBuf::from(r"C:\Program Files\jdk-17\bin\java.exe"));
        assert_eq!(l.args, ["-Xmx4096M", "-jar", "fabric-server-launch.jar", "nogui"]);
        give_back(&root).unwrap();
        assert!(!root.join(RUN_BAT).exists(), "there was none before");
        let _ = fs::remove_dir_all(&root);
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
        assert!(uses_bare_java("java -jar x.jar") && uses_bare_java("JAVA.EXE @a"));
        assert!(!uses_bare_java(r#""G:\jdk\bin\java.exe" @a"#));
    }
}
