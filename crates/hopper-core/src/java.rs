//! Finding Java installs and picking the right one for a Minecraft version.
//!
//! Detection reads each install's `release` file (no process is started),
//! falling back to the folder name and, last of all, `java -version`.
//! Downloading new runtimes lives in [`crate::javadl`].

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct JavaInstall {
    /// `javaw.exe` (no console window).
    pub exe: PathBuf,
    /// e.g. "21.0.4".
    pub version: String,
    pub major: u32,
    pub vendor: String,
    /// A 64-bit JVM (32-bit ones can't use more than about 1.5 GB of heap).
    pub is_64bit: bool,
    /// GraalVM (Oracle or Community).
    pub graalvm: bool,
}

impl JavaInstall {
    pub fn label(&self) -> String {
        let vendor = if self.vendor.is_empty() {
            String::new()
        } else {
            format!(" · {}", self.vendor)
        };
        let bits = if self.is_64bit { "" } else { " · 32-bit" };
        format!("Java {} ({}){vendor}{bits}", self.major, self.version)
    }

    /// The install's home folder (the one holding `bin`).
    pub fn home(&self) -> Option<&Path> {
        self.exe.parent()?.parent()
    }

    /// `java.exe` next to `exe` (console; what servers should run), or `exe`.
    pub fn console_exe(&self) -> PathBuf {
        let java = self.exe.with_file_name("java.exe");
        if java.exists() {
            java
        } else {
            self.exe.clone()
        }
    }

    /// Who built it, as a fixed kind (for ranking and icons).
    pub fn vendor_kind(&self) -> Vendor {
        if self.graalvm {
            return Vendor::GraalVm;
        }
        Vendor::classify(&self.vendor, &self.exe)
    }

    /// Whether Hopper downloaded it (it lives in [`hopper_runtimes_dir`]).
    pub fn is_hopper_managed(&self) -> bool {
        hopper_runtimes_dir().is_some_and(|d| path_starts_with(&self.exe, &d))
    }
}

/// Java builds Hopper knows by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Vendor {
    GraalVm,
    Temurin,
    Microsoft,
    Zulu,
    Liberica,
    Corretto,
    Oracle,
    Semeru,
    /// A runtime the Minecraft launcher (or CurseForge) downloaded.
    Mojang,
    Other,
}

impl Vendor {
    pub fn name(self) -> &'static str {
        match self {
            Vendor::GraalVm => "GraalVM",
            Vendor::Temurin => "Eclipse Temurin",
            Vendor::Microsoft => "Microsoft OpenJDK",
            Vendor::Zulu => "Azul Zulu",
            Vendor::Liberica => "BellSoft Liberica",
            Vendor::Corretto => "Amazon Corretto",
            Vendor::Oracle => "Oracle",
            Vendor::Semeru => "IBM Semeru",
            Vendor::Mojang => "Minecraft runtime",
            Vendor::Other => "Other",
        }
    }

    /// From a vendor string ("Eclipse Adoptium", "Temurin-21.0.4+7"…), then the path.
    pub fn classify(vendor: &str, path: &Path) -> Vendor {
        if let Some(v) = Self::from_text(vendor) {
            return v;
        }
        let p = path.to_string_lossy().to_lowercase();
        if p.contains("java-runtime-") || p.contains("jre-legacy") {
            return Vendor::Mojang;
        }
        Self::from_text(&p).unwrap_or(Vendor::Other)
    }

    fn from_text(s: &str) -> Option<Vendor> {
        let s = s.to_lowercase();
        let has = |n: &[&str]| n.iter().any(|n| s.contains(n));
        Some(if has(&["graal", "jvmci"]) {
            Vendor::GraalVm
        } else if has(&["temurin", "adoptium", "adoptopenjdk", "eclipse foundation"]) {
            Vendor::Temurin
        } else if has(&["microsoft"]) {
            Vendor::Microsoft
        } else if has(&["zulu", "azul"]) {
            Vendor::Zulu
        } else if has(&["liberica", "bellsoft"]) {
            Vendor::Liberica
        } else if has(&["corretto", "amazon"]) {
            Vendor::Corretto
        } else if has(&["semeru", "openj9", "ibm"]) {
            Vendor::Semeru
        } else if has(&["oracle", "java(tm)"]) {
            Vendor::Oracle
        } else {
            return None;
        })
    }
}

/// Major version from "1.8.0_402", "17.0.13", "21".
fn major_of(version: &str) -> u32 {
    let mut parts = version.split(['.', '_', '+', '-']);
    match parts.next().and_then(|p| p.parse().ok()) {
        Some(1) => parts.next().and_then(|p| p.parse().ok()).unwrap_or(8),
        Some(n) => n,
        None => 0,
    }
}

fn path_starts_with(p: &Path, dir: &Path) -> bool {
    let p = p.to_string_lossy().to_lowercase().replace('/', "\\");
    let d = dir.to_string_lossy().to_lowercase().replace('/', "\\");
    p.starts_with(d.trim_end_matches('\\'))
}

/// Where Hopper keeps the runtimes it downloads: `<data dir>/java`.
pub fn hopper_runtimes_dir() -> Option<PathBuf> {
    crate::paths::dirs().map(|d| d.data_dir().join("java"))
}

/// Whether a PE executable is 64-bit (x64 or ARM64), from its header.
fn pe_is_64bit(exe: &Path) -> Option<bool> {
    use std::io::Read;
    let mut buf = vec![0u8; 4096];
    let n = std::fs::File::open(exe).ok()?.read(&mut buf).ok()?;
    let buf = &buf[..n];
    if buf.get(..2)? != b"MZ" {
        return None;
    }
    let pe = u32::from_le_bytes(buf.get(0x3c..0x40)?.try_into().ok()?) as usize;
    if buf.get(pe..pe + 4)? != b"PE\0\0" {
        return None;
    }
    let machine = u16::from_le_bytes(buf.get(pe + 4..pe + 6)?.try_into().ok()?);
    match machine {
        0x8664 | 0xaa64 => Some(true),
        0x014c => Some(false),
        _ => None,
    }
}

/// Reads `<home>/release` for version and vendor.
fn probe(home: &Path) -> Option<JavaInstall> {
    let exe = ["javaw.exe", "java.exe", "java"]
        .iter()
        .map(|n| home.join("bin").join(n))
        .find(|p| p.exists())?;
    let release = std::fs::read_to_string(home.join("release")).unwrap_or_default();
    let field = |k: &str| {
        release
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")))
            .map(|v| v.trim().trim_matches('"').to_string())
            .unwrap_or_default()
    };
    let version = field("JAVA_VERSION");
    let graal = field("GRAALVM_VERSION");
    let vendor = {
        let v = field("IMPLEMENTOR");
        let v = if v.is_empty() {
            field("JAVA_VENDOR")
        } else {
            v
        };
        // Oracle's GraalVM reports plain "Oracle Corporation"; its release
        // file has GRAALVM_VERSION (the Community build names itself).
        if !graal.is_empty() && !v.to_lowercase().contains("graalvm") {
            if v.to_lowercase().contains("oracle") {
                format!("Oracle GraalVM {graal}")
            } else {
                format!("GraalVM {graal}")
            }
        } else {
            v
        }
    };
    let arch = field("OS_ARCH").to_lowercase();
    let is_64bit = match arch.as_str() {
        "amd64" | "x86_64" | "x64" | "aarch64" | "arm64" => true,
        "x86" | "i386" | "i486" | "i586" | "i686" => false,
        _ => pe_is_64bit(&home.join("bin").join("java.exe"))
            .or_else(|| pe_is_64bit(&exe))
            .unwrap_or(true),
    };
    let graalvm = !graal.is_empty()
        || vendor.to_lowercase().contains("graal")
        || home.to_string_lossy().to_lowercase().contains("graalvm");
    // Mojang's runtimes have no release file; their folder names say the version.
    let (version, vendor, graalvm) = if !version.is_empty() {
        (version, vendor, graalvm)
    } else if let Some(v) = guess_from_path(home) {
        (v, vendor, graalvm)
    } else {
        let info = run_version(&exe)?;
        (info.version, info.vendor, graalvm || info.graalvm)
    };
    let vendor = if vendor.is_empty() && Vendor::classify("", home) == Vendor::Mojang {
        "Mojang".to_string()
    } else {
        vendor
    };
    Some(JavaInstall {
        major: major_of(&version),
        exe,
        version,
        vendor,
        is_64bit,
        graalvm,
    })
}

fn guess_from_path(home: &Path) -> Option<String> {
    let s = home.to_string_lossy().to_lowercase();
    for (needle, v) in [
        ("java-runtime-epsilon", "25"),
        ("java-runtime-delta", "21"),
        ("java-runtime-gamma", "17"),
        ("java-runtime-beta", "17"),
        ("java-runtime-alpha", "16"),
        ("jre-legacy", "1.8.0"),
    ] {
        if s.contains(needle) {
            return Some(v.to_string());
        }
    }
    let digits: String = home
        .file_name()?
        .to_string_lossy()
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    (!digits.is_empty()).then_some(digits)
}

/// What `java -version` says about itself.
#[derive(Clone, Debug, PartialEq)]
pub struct VersionInfo {
    /// e.g. "21.0.4", "1.8.0_402".
    pub version: String,
    pub major: u32,
    /// e.g. "Eclipse Temurin", "Oracle GraalVM", "" when the output doesn't say.
    pub vendor: String,
    pub is_64bit: bool,
    pub graalvm: bool,
}

/// Parses `java -version` output (it goes to stderr).
pub fn parse_version_output(text: &str) -> Option<VersionInfo> {
    let version = text.lines().find_map(|l| {
        let l = l.trim();
        let rest = l.split_once(" version \"")?.1;
        Some(rest.split('"').next()?.to_string())
    })?;
    // "OpenJDK Runtime Environment Temurin-21.0.4+7 (build …)" → "Temurin-21.0.4+7".
    let env_tag = text
        .lines()
        .find_map(|l| {
            let rest = l.split_once("Runtime Environment")?.1;
            Some(rest.split(" (build").next()?.trim().to_string())
        })
        .unwrap_or_default();
    let lower = text.to_lowercase();
    let graalvm = lower.contains("graalvm") || lower.contains("jvmci");
    let vendor = if lower.contains("oracle graalvm") {
        "Oracle GraalVM".to_string()
    } else if lower.contains("graalvm ce") || lower.contains("graalvm community") {
        "GraalVM Community".to_string()
    } else if graalvm {
        "GraalVM".to_string()
    } else {
        match Vendor::from_text(&env_tag).or_else(|| Vendor::from_text(text)) {
            Some(v) => v.name().to_string(),
            None => String::new(),
        }
    };
    Some(VersionInfo {
        major: major_of(&version),
        version,
        vendor,
        is_64bit: lower.contains("64-bit"),
        graalvm,
    })
}

/// Runs `java -version` (with no console window), waiting at most 10 s.
fn run_version(exe: &Path) -> Option<VersionInfo> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let java = exe.with_file_name("java.exe");
    let java = if java.exists() {
        java
    } else {
        exe.to_path_buf()
    };
    let mut cmd = Command::new(java);
    cmd.arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().ok()?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < std::time::Duration::from_secs(10) => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut text = String::new();
    child.stderr.take()?.read_to_string(&mut text).ok()?;
    if text.trim().is_empty() {
        child.stdout.take()?.read_to_string(&mut text).ok()?;
    }
    parse_version_output(&text)
}

/// Java homes under `dir`, up to `depth` levels down.
fn homes_under(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if dir.join("bin").join("java.exe").exists() || dir.join("bin").join("javaw.exe").exists() {
        out.push(dir.to_path_buf());
        return;
    }
    if depth == 0 {
        return;
    }
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            homes_under(&e.path(), depth - 1, out);
        }
    }
}

/// Folders (and how deep to look) where Java installs usually live.
fn search_roots(launcher_dir: &Path) -> Vec<(PathBuf, usize)> {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let pf = env("ProgramFiles").unwrap_or_else(|| r"C:\Program Files".into());
    let pf86 = env("ProgramFiles(x86)").unwrap_or_else(|| r"C:\Program Files (x86)".into());
    let appdata = env("APPDATA");
    let local = env("LOCALAPPDATA");
    let home = directories::UserDirs::new().map(|u| u.home_dir().to_path_buf());

    let mut roots = Vec::new();
    // Vendor installers (machine-wide and per-user).
    let mut bases = vec![pf.clone(), pf86.clone()];
    if let Some(l) = &local {
        bases.push(l.join("Programs"));
    }
    for base in &bases {
        for vendor in [
            "Java",
            "Eclipse Adoptium",
            "Eclipse Foundation",
            "AdoptOpenJDK",
            "Temurin",
            "Zulu",
            "Azul",
            "Microsoft",
            "BellSoft",
            "Liberica",
            "Amazon Corretto",
            "Corretto",
            "GraalVM",
            "Oracle",
            "Semeru",
            "IBM",
        ] {
            roots.push((base.join(vendor), 2));
        }
    }
    // The launcher Hopper was pointed at (CurseForge's Install folder, usually).
    roots.push((launcher_dir.join("java"), 2));
    roots.push((launcher_dir.join("runtime"), 4));
    // Mojang's launcher: classic, Microsoft Store and the old .minecraft spot.
    roots.push((pf86.join("Minecraft Launcher").join("runtime"), 4));
    roots.push((pf.join("Minecraft Launcher").join("runtime"), 4));
    if let Some(l) = &local {
        roots.push((
            l.join(r"Packages\Microsoft.4297127D64EC6_8wekyb3d8bbwe\LocalCache\Local\runtime"),
            4,
        ));
    }
    if let Some(a) = &appdata {
        roots.push((a.join(".minecraft").join("runtime"), 4));
        // Prism / MultiMC / Modrinth App / GDLauncher downloads.
        roots.push((a.join("PrismLauncher").join("java"), 3));
        roots.push((a.join("MultiMC").join("java"), 3));
        roots.push((a.join("ModrinthApp").join("meta").join("java_versions"), 3));
        roots.push((
            a.join("com.modrinth.theseus")
                .join("meta")
                .join("java_versions"),
            3,
        ));
        roots.push((a.join("gdlauncher_next").join("datastore").join("java"), 3));
    }
    if let Some(h) = &home {
        roots.push((h.join(r"curseforge\minecraft\Install\runtime"), 4));
        roots.push((h.join(r"curseforge\minecraft\Install\java"), 2));
        roots.push((h.join(".jdks"), 2));
        roots.push((h.join("scoop").join("apps"), 2));
    }
    if let Some(d) = hopper_runtimes_dir() {
        roots.push((d, 2));
    }
    roots
}

/// A path's identity for de-duplication: canonical where possible, lower-cased.
fn identity(p: &Path) -> String {
    std::fs::canonicalize(p)
        .unwrap_or_else(|_| p.to_path_buf())
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .trim_end_matches(['\\', '/'])
        .to_lowercase()
}

/// Every Java this PC has in the usual places, newest first, no duplicates.
///
/// Looks in vendor folders under Program Files, JAVA_HOME and PATH, Mojang's
/// launcher runtimes, CurseForge / Prism / MultiMC / Modrinth App runtimes,
/// `~/.jdks`, scoop and Hopper's own downloads.
pub fn detect(launcher_dir: &Path) -> Vec<JavaInstall> {
    let mut homes = Vec::new();
    for (dir, depth) in search_roots(launcher_dir) {
        homes_under(&dir, depth, &mut homes);
    }
    if let Ok(h) = std::env::var("JAVA_HOME") {
        homes.push(PathBuf::from(h.trim().trim_end_matches(['\\', '/'])));
    }
    for dir in std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default()
    {
        let exe = dir.join("java.exe");
        if !exe.exists() {
            continue;
        }
        // Oracle's "javapath" holds symlinks; follow them to the real home.
        let real = std::fs::canonicalize(&exe).unwrap_or(exe);
        if let Some(home) = real.parent().and_then(Path::parent) {
            homes.push(PathBuf::from(
                home.to_string_lossy().trim_start_matches(r"\\?\"),
            ));
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<JavaInstall> = Vec::new();
    for h in homes {
        if !seen.insert(identity(&h)) {
            continue;
        }
        if let Some(j) = probe(&h) {
            out.push(j);
        }
    }
    out.sort_by(|a, b| {
        b.major
            .cmp(&a.major)
            .then_with(|| crate::scan::compare_versions(&b.version, &a.version))
    });
    out
}

/// Inspects a java.exe the user picked by hand.
pub fn inspect(exe: &Path) -> Option<JavaInstall> {
    let home = exe.parent()?.parent()?;
    probe(home)
        .map(|mut j| {
            j.exe = exe.to_path_buf();
            j
        })
        .or_else(|| {
            // Not a normal JDK layout: ask the binary itself.
            let info = run_version(exe)?;
            Some(JavaInstall {
                exe: exe.to_path_buf(),
                version: info.version,
                major: info.major,
                vendor: info.vendor,
                is_64bit: info.is_64bit,
                graalvm: info.graalvm,
            })
        })
}

// ---------------------------------------------------------------------------
// Which Java a Minecraft version needs.

/// Whether Java runs the game (client) or a dedicated server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Client,
    Server,
}

/// What Java a Minecraft version needs.
#[derive(Clone, Debug, PartialEq)]
pub struct JavaRequirement {
    pub game_version: String,
    /// The major Mojang ships for it (and what to download).
    pub major: u32,
    /// Oldest major that starts it.
    pub min: u32,
    /// Newest major known to work, when there is a cap (Forge for ≤1.12.2: 8).
    pub max: Option<u32>,
    /// Majors as good as `major` (1.17: 16 and 17).
    pub ideal: Vec<u32>,
    /// e.g. "Java 21 is required for Minecraft 1.21.1".
    pub reason: String,
    /// `major` came from Mojang's version manifest rather than the built-in table.
    pub from_manifest: bool,
}

impl JavaRequirement {
    /// Whether a Java `major` can run this version at all.
    pub fn fits(&self, major: u32) -> bool {
        major >= self.min && self.max.is_none_or(|m| major <= m)
    }
}

/// (major, minor, patch) of a "1.x.y" release; `None` for other shapes.
fn release_parts(v: &str) -> Option<(u32, u32, u32)> {
    let mut p = v.split('.');
    let a = p.next()?.parse().ok()?;
    let b = p.next().map_or(Some(0), |x| x.parse().ok())?;
    let c = p.next().map_or(Some(0), |x| x.parse().ok())?;
    Some((a, b, c))
}

/// The built-in table: Java major for a game version, and the oldest that works.
/// Handles releases ("1.20.4"), pre-releases ("1.20.5-pre1", "1.18-pre2"),
/// week snapshots ("24w14a") and year-based versions ("26.1", "26.4-snapshot-3").
fn table_major(game_version: &str) -> u32 {
    let v = game_version.trim().to_lowercase();
    // Week snapshots: 21w19a moved to 16, 22w (1.19) to 17, 24w14a to 21.
    if let Some((yy, rest)) = v.split_once('w')
        && let Ok(yy) = yy.parse::<u32>()
        && let Ok(ww) = rest
            .trim_end_matches(|c: char| c.is_ascii_alphabetic())
            .parse::<u32>()
    {
        return match (yy, ww) {
            (25.., _) | (24, 14..) => 21,
            (22.., _) => 17,
            (21, 19..) => 16,
            _ => 8,
        };
    }
    let (base, suffix) = match v.split_once(['-', ' ', '_']) {
        Some((b, s)) => (b, s),
        None => (v.as_str(), ""),
    };
    let Some((a, b, c)) = release_parts(base) else {
        return 21; // Unknown shape: assume a modern version.
    };
    if a != 1 {
        // Year-based versions (26.1 and later) need Java 25.
        return if a >= 26 { 25 } else { 21 };
    }
    // 1.18's first pre-release still ran on 16.
    if (b, c) == (18, 0) && suffix == "pre1" {
        return 16;
    }
    match (b, c) {
        (21.., _) | (20, 5..) => 21,
        (18..=20, _) => 17,
        (17, _) => 16,
        _ => 8,
    }
}

/// The Java major Minecraft `game_version` needs.
pub fn recommended_major(game_version: &str) -> u32 {
    table_major(game_version)
}

/// What Java `game_version` needs, from the built-in table.
pub fn requirement(game_version: &str, side: Side) -> JavaRequirement {
    requirement_with_manifest(game_version, side, None)
}

/// Like [`requirement`], but `manifest_major` (from Mojang's version JSON
/// `javaVersion.majorVersion`, see [`manifest_java_major`]) wins when known.
pub fn requirement_with_manifest(
    game_version: &str,
    side: Side,
    manifest_major: Option<u32>,
) -> JavaRequirement {
    let from_manifest = manifest_major.is_some_and(|m| m > 0);
    let major = manifest_major
        .filter(|m| *m > 0)
        .unwrap_or_else(|| table_major(game_version));
    let parts = release_parts(game_version.split(['-', ' ']).next().unwrap_or(""));
    // Forge on 1.12.2 and older (LaunchWrapper) crashes on Java 9+.
    let legacy_forge = major == 8 && parts.is_some_and(|(a, b, _)| a == 1 && b <= 12);
    let (min, max, ideal) = match major {
        8 if legacy_forge => (8, Some(8), vec![8]),
        8 => (8, None, vec![8]),
        // 1.17 shipped with 16; 17 runs it just as well.
        16 => (16, None, vec![16, 17]),
        m => (m, None, vec![m]),
    };
    let what = match side {
        Side::Client => "Minecraft",
        Side::Server => "a Minecraft server on",
    };
    let reason = if legacy_forge {
        format!("Java 8 is required for {what} {game_version} (older Forge breaks on newer Java)")
    } else if major == 16 {
        format!("Java 16 or 17 is required for {what} {game_version}")
    } else {
        format!("Java {major} is required for {what} {game_version}")
    };
    JavaRequirement {
        game_version: game_version.to_string(),
        major,
        min,
        max,
        ideal,
        reason,
        from_manifest,
    }
}

/// Mojang's `javaVersion.majorVersion` for `game_version`, from the version
/// manifest (network; results are cached for the session). `Ok(None)` when
/// the version isn't listed or doesn't say.
pub fn manifest_java_major(game_version: &str) -> anyhow::Result<Option<u32>> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, Option<u32>>>> = OnceLock::new();
    static INDEX: OnceLock<Mutex<Option<HashMap<String, String>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(hit) = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(game_version)
    {
        return Ok(*hit);
    }
    let fetch = |url: &str| -> anyhow::Result<serde_json::Value> {
        Ok(crate::cf::agent()
            .get(url)
            .call()?
            .body_mut()
            .with_config()
            .limit(16 << 20)
            .read_json()?)
    };
    let url = {
        let index = INDEX.get_or_init(Default::default);
        let mut g = index.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_none() {
            let m = fetch("https://piston-meta.mojang.com/mc/game/version_manifest_v2.json")?;
            *g = Some(parse_manifest_index(&m));
        }
        g.as_ref().and_then(|m| m.get(game_version).cloned())
    };
    let major = match url {
        Some(url) => parse_java_major(&fetch(&url)?),
        None => None,
    };
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(game_version.to_string(), major);
    Ok(major)
}

/// id → version JSON URL, from `version_manifest_v2.json`.
fn parse_manifest_index(m: &serde_json::Value) -> std::collections::HashMap<String, String> {
    m["versions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| {
            Some((
                v["id"].as_str()?.to_string(),
                v["url"].as_str()?.to_string(),
            ))
        })
        .collect()
}

/// `javaVersion.majorVersion` from a version JSON.
fn parse_java_major(v: &serde_json::Value) -> Option<u32> {
    v["javaVersion"]["majorVersion"].as_u64().map(|m| m as u32)
}

/// One detected install, scored for a requirement.
#[derive(Clone, Debug, PartialEq)]
pub struct RankedJava {
    pub install: JavaInstall,
    /// Higher is better; only meaningful relative to the other picks.
    pub score: i32,
    /// It can run the game at all.
    pub fits: bool,
    /// Short reasons for the user, most important first.
    pub reasons: Vec<String>,
}

/// Detected installs ranked for `req`, best first: right major, then
/// 64-bit, then GraalVM / Temurin, then newer builds.
pub fn rank(installs: &[JavaInstall], req: &JavaRequirement, side: Side) -> Vec<RankedJava> {
    let mut out: Vec<RankedJava> = installs
        .iter()
        .map(|j| {
            let mut score = 0;
            let mut reasons = Vec::new();
            let fits = req.fits(j.major);
            if req.ideal.contains(&j.major) {
                score += 1000;
                reasons.push(req.reason.clone());
            } else if fits {
                score += 600 - 10 * (j.major.abs_diff(req.major) as i32).min(30);
                reasons.push(format!(
                    "Java {} is newer than needed (Java {}); usually works, but some mods may not",
                    j.major, req.major
                ));
            } else {
                score -= 1000;
                reasons.push(if j.major < req.min {
                    format!(
                        "Java {} is too old for Minecraft {} (needs {}+)",
                        j.major, req.game_version, req.min
                    )
                } else {
                    format!(
                        "Java {} is too new for Minecraft {} (needs Java {})",
                        j.major,
                        req.game_version,
                        req.max.unwrap_or(req.major)
                    )
                });
            }
            if j.is_64bit {
                score += 200;
            } else {
                reasons.push("32-bit: can't use more than about 1.5 GB of memory".into());
            }
            match j.vendor_kind() {
                Vendor::GraalVm => {
                    score += if side == Side::Server { 80 } else { 60 };
                    reasons.push("GraalVM: often faster for big modpacks".into());
                }
                Vendor::Temurin => {
                    score += 50;
                    reasons.push("Eclipse Temurin: well-tested, widely used build".into());
                }
                Vendor::Mojang => {
                    score += if side == Side::Client { 35 } else { 25 };
                    reasons.push("The runtime the Minecraft launcher uses".into());
                }
                Vendor::Microsoft
                | Vendor::Zulu
                | Vendor::Liberica
                | Vendor::Corretto
                | Vendor::Oracle => score += 30,
                Vendor::Semeru => {
                    reasons.push("OpenJ9 (Semeru): many Minecraft tuning flags don't apply".into());
                }
                Vendor::Other => score += 10,
            }
            RankedJava {
                install: j.clone(),
                score,
                fits,
                reasons,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| crate::scan::compare_versions(&b.install.version, &a.install.version))
    });
    out
}

/// The best detected install for `game_version`, if any can run it.
pub fn best(installs: &[JavaInstall], game_version: &str, side: Side) -> Option<RankedJava> {
    rank(installs, &requirement(game_version, side), side)
        .into_iter()
        .find(|r| r.fits)
}

/// Java argument presets offered in the settings.
pub const PRESETS: [(&str, &str); 2] = [
    (
        "G1 tuned (Aikar)",
        "-XX:+UseG1GC -XX:+ParallelRefProcEnabled -XX:MaxGCPauseMillis=200 -XX:+UnlockExperimentalVMOptions -XX:+DisableExplicitGC -XX:+AlwaysPreTouch -XX:G1NewSizePercent=30 -XX:G1MaxNewSizePercent=40 -XX:G1HeapRegionSize=8M -XX:G1ReservePercent=20 -XX:G1HeapWastePercent=5 -XX:G1MixedGCCountTarget=4 -XX:InitiatingHeapOccupancyPercent=15 -XX:G1MixedGCLiveThresholdPercent=90 -XX:G1RSetUpdatingPauseTimePercent=5 -XX:SurvivorRatio=32 -XX:+PerfDisableSharedMem -XX:MaxTenuringThreshold=1",
    ),
    (
        "ZGC (Java 21+)",
        "-XX:+UseZGC -XX:+ZGenerational -XX:+AlwaysPreTouch -XX:+DisableExplicitGC",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn majors() {
        assert_eq!(major_of("1.8.0_402"), 8);
        assert_eq!(major_of("17.0.13"), 17);
        assert_eq!(major_of("21"), 21);
        assert_eq!(major_of("21.0.12.1+1-LTS"), 21);
        assert_eq!(major_of(""), 0);
    }

    #[test]
    fn recommendations() {
        assert_eq!(recommended_major("1.21.1"), 21);
        assert_eq!(recommended_major("1.20.6"), 21);
        assert_eq!(recommended_major("1.20.5"), 21);
        assert_eq!(recommended_major("1.20.4"), 17);
        assert_eq!(recommended_major("1.20.1"), 17);
        assert_eq!(recommended_major("1.20"), 17);
        assert_eq!(recommended_major("1.18"), 17);
        assert_eq!(recommended_major("1.18.2"), 17);
        assert_eq!(recommended_major("1.17"), 16);
        assert_eq!(recommended_major("1.17.1"), 16);
        assert_eq!(recommended_major("1.16.5"), 8);
        assert_eq!(recommended_major("1.12.2"), 8);
        assert_eq!(recommended_major("1.7.10"), 8);
        assert_eq!(recommended_major("1.21.11"), 21);
        // Year-based versions: Mojang's manifest says 25 for 26.1+.
        assert_eq!(recommended_major("26.1"), 25);
        assert_eq!(recommended_major("26.3"), 25);
        assert_eq!(recommended_major("26.4-snapshot-3"), 25);
        assert_eq!(recommended_major("27.1"), 25);
    }

    #[test]
    fn snapshot_and_prerelease_majors() {
        // Checked against Mojang's version JSONs.
        assert_eq!(recommended_major("1.20.5-pre1"), 21);
        assert_eq!(recommended_major("1.20.5-rc1"), 21);
        assert_eq!(recommended_major("1.18-pre1"), 16);
        assert_eq!(recommended_major("1.18-pre2"), 17);
        assert_eq!(recommended_major("24w14a"), 21);
        assert_eq!(recommended_major("24w13a"), 17);
        assert_eq!(recommended_major("25w46a"), 21);
        assert_eq!(recommended_major("21w44a"), 16);
        assert_eq!(recommended_major("21w19a"), 16);
        assert_eq!(recommended_major("21w18a"), 8);
        assert_eq!(recommended_major("22w11a"), 17);
        assert_eq!(recommended_major(" 1.21.1 "), 21);
        assert_eq!(recommended_major("garbage"), 21);
    }

    #[test]
    fn requirements() {
        let r = requirement("1.21.1", Side::Client);
        assert_eq!(r.major, 21);
        assert_eq!(r.reason, "Java 21 is required for Minecraft 1.21.1");
        assert!(r.fits(21) && r.fits(25) && !r.fits(17));

        let r = requirement("1.12.2", Side::Server);
        assert_eq!((r.min, r.max), (8, Some(8)));
        assert!(!r.fits(17));

        let r = requirement("1.16.5", Side::Client);
        assert_eq!(r.max, None);

        let r = requirement("1.17.1", Side::Client);
        assert_eq!(r.ideal, vec![16, 17]);

        // The manifest wins over the table.
        let r = requirement_with_manifest("26.9", Side::Client, Some(27));
        assert_eq!(r.major, 27);
        assert!(r.from_manifest);
        let r = requirement_with_manifest("1.20.1", Side::Client, None);
        assert!(!r.from_manifest);
    }

    #[test]
    fn manifest_parsing() {
        let m = serde_json::json!({"versions": [
            {"id": "1.21.1", "url": "https://x/1.21.1.json"},
            {"id": "26.3", "url": "https://x/26.3.json"}
        ]});
        let idx = parse_manifest_index(&m);
        assert_eq!(idx["26.3"], "https://x/26.3.json");
        let v = serde_json::json!({"javaVersion": {"component": "java-runtime-epsilon", "majorVersion": 25}});
        assert_eq!(parse_java_major(&v), Some(25));
        assert_eq!(parse_java_major(&serde_json::json!({})), None);
    }

    fn inst(major: u32, vendor: &str, is_64bit: bool, graalvm: bool) -> JavaInstall {
        JavaInstall {
            exe: PathBuf::from(format!(r"C:\j\{vendor}-{major}\bin\javaw.exe")),
            version: format!("{major}.0.1"),
            major,
            vendor: vendor.into(),
            is_64bit,
            graalvm,
        }
    }

    #[test]
    fn ranking() {
        let all = vec![
            inst(17, "Eclipse Adoptium", true, false),
            inst(21, "Microsoft", true, false),
            inst(21, "Eclipse Adoptium", true, false),
            inst(21, "Oracle GraalVM 23.1.5", true, true),
            inst(21, "Eclipse Adoptium", false, false),
            inst(25, "Eclipse Adoptium", true, false),
            inst(8, "Oracle Corporation", true, false),
        ];
        let req = requirement("1.21.1", Side::Server);
        let r = rank(&all, &req, Side::Server);
        let order: Vec<(u32, Vendor, bool)> = r
            .iter()
            .map(|x| (x.install.major, x.install.vendor_kind(), x.install.is_64bit))
            .collect();
        assert_eq!(
            order,
            vec![
                (21, Vendor::GraalVm, true),
                (21, Vendor::Temurin, true),
                (21, Vendor::Microsoft, true),
                (21, Vendor::Temurin, false),
                (25, Vendor::Temurin, true),
                (17, Vendor::Temurin, true),
                (8, Vendor::Oracle, true),
            ]
        );
        assert!(
            r[0].reasons
                .contains(&"GraalVM: often faster for big modpacks".to_string())
        );
        assert_eq!(
            r[0].reasons[0],
            "Java 21 is required for a Minecraft server on 1.21.1"
        );
        assert!(r[3].reasons.iter().any(|s| s.starts_with("32-bit")));
        assert!(r[4].fits && !r[5].fits && !r[6].fits);
        assert_eq!(
            r[5].reasons[0],
            "Java 17 is too old for Minecraft 1.21.1 (needs 21+)"
        );

        // Old Forge: Java 17 is too new, Java 8 wins.
        let r = rank(&all, &requirement("1.12.2", Side::Client), Side::Client);
        assert_eq!(r[0].install.major, 8);
        assert!(r.iter().skip(1).all(|x| !x.fits));
        assert!(r[1].reasons[0].contains("too new"));

        // 1.17: 16 and 17 are both ideal.
        let r = rank(&all, &requirement("1.17.1", Side::Client), Side::Client);
        assert_eq!(r[0].install.major, 17);

        // Newest build wins a tie.
        let mut a = inst(21, "Eclipse Adoptium", true, false);
        a.version = "21.0.3".into();
        let mut b = a.clone();
        b.version = "21.0.12".into();
        let r = rank(&[a, b], &requirement("1.21.1", Side::Client), Side::Client);
        assert_eq!(r[0].install.version, "21.0.12");

        assert!(best(&all[..1], "1.21.1", Side::Client).is_none());
    }

    #[test]
    fn vendor_classification() {
        let p = Path::new(r"C:\x");
        assert_eq!(Vendor::classify("Eclipse Adoptium", p), Vendor::Temurin);
        assert_eq!(Vendor::classify("Azul Systems, Inc.", p), Vendor::Zulu);
        assert_eq!(Vendor::classify("BellSoft", p), Vendor::Liberica);
        assert_eq!(Vendor::classify("Amazon.com Inc.", p), Vendor::Corretto);
        assert_eq!(Vendor::classify("Oracle Corporation", p), Vendor::Oracle);
        assert_eq!(Vendor::classify("Oracle GraalVM 23.1", p), Vendor::GraalVm);
        assert_eq!(
            Vendor::classify(
                "",
                Path::new(r"C:\mc\runtime\java-runtime-delta\windows-x64\java-runtime-delta")
            ),
            Vendor::Mojang
        );
        assert_eq!(
            Vendor::classify("", Path::new(r"C:\Program Files\Zulu\zulu-17")),
            Vendor::Zulu
        );
        assert_eq!(Vendor::classify("", p), Vendor::Other);
    }

    #[test]
    fn version_output_temurin() {
        let t = "openjdk version \"21.0.4\" 2024-07-16 LTS\n\
                 OpenJDK Runtime Environment Temurin-21.0.4+7 (build 21.0.4+7-LTS)\n\
                 OpenJDK 64-Bit Server VM Temurin-21.0.4+7 (build 21.0.4+7-LTS, mixed mode, sharing)\n";
        let v = parse_version_output(t).unwrap();
        assert_eq!(v.version, "21.0.4");
        assert_eq!(v.major, 21);
        assert_eq!(v.vendor, "Eclipse Temurin");
        assert!(v.is_64bit && !v.graalvm);
    }

    #[test]
    fn version_output_vendors() {
        let cases: &[(&str, &str, u32, &str, bool, bool)] = &[
            (
                "oracle 8",
                "java version \"1.8.0_401\"\r\nJava(TM) SE Runtime Environment (build 1.8.0_401-b10)\r\nJava HotSpot(TM) 64-Bit Server VM (build 25.401-b10, mixed mode)\r\n",
                8,
                "Oracle",
                true,
                false,
            ),
            (
                "oracle 8 32-bit",
                "java version \"1.8.0_51\"\nJava(TM) SE Runtime Environment (build 1.8.0_51-b16)\nJava HotSpot(TM) Client VM (build 25.51-b03, mixed mode)\n",
                8,
                "Oracle",
                false,
                false,
            ),
            (
                "oracle graalvm",
                "java version \"21.0.5\" 2024-10-15 LTS\nJava(TM) SE Runtime Environment Oracle GraalVM 21.0.5+9.1 (build 21.0.5+9-LTS-jvmci-23.1-b48)\nJava HotSpot(TM) 64-Bit Server VM Oracle GraalVM 21.0.5+9.1 (build 21.0.5+9-LTS-jvmci-23.1-b48, mixed mode, sharing)\n",
                21,
                "Oracle GraalVM",
                true,
                true,
            ),
            (
                "graalvm ce",
                "openjdk version \"21.0.2\" 2024-01-16\nOpenJDK Runtime Environment GraalVM CE 21.0.2+13.1 (build 21.0.2+13-jvmci-23.1-b30)\nOpenJDK 64-Bit Server VM GraalVM CE 21.0.2+13.1 (build 21.0.2+13-jvmci-23.1-b30, mixed mode, sharing)\n",
                21,
                "GraalVM Community",
                true,
                true,
            ),
            (
                "zulu",
                "openjdk version \"17.0.12\" 2024-07-16 LTS\nOpenJDK Runtime Environment Zulu17.52+17-CA (build 17.0.12+7-LTS)\nOpenJDK 64-Bit Server VM Zulu17.52+17-CA (build 17.0.12+7-LTS, mixed mode, sharing)\n",
                17,
                "Azul Zulu",
                true,
                false,
            ),
            (
                "microsoft",
                "openjdk version \"21.0.4\" 2024-07-16 LTS\nOpenJDK Runtime Environment Microsoft-9889606 (build 21.0.4+7-LTS)\nOpenJDK 64-Bit Server VM Microsoft-9889606 (build 21.0.4+7-LTS, mixed mode)\n",
                21,
                "Microsoft OpenJDK",
                true,
                false,
            ),
            (
                "corretto",
                "openjdk version \"17.0.12\" 2024-07-16 LTS\nOpenJDK Runtime Environment Corretto-17.0.12.7.1 (build 17.0.12+7-LTS)\nOpenJDK 64-Bit Server VM Corretto-17.0.12.7.1 (build 17.0.12+7-LTS, mixed mode, sharing)\n",
                17,
                "Amazon Corretto",
                true,
                false,
            ),
            (
                "liberica (anonymous)",
                "openjdk version \"17.0.12\" 2024-07-16 LTS\nOpenJDK Runtime Environment (build 17.0.12+10-LTS)\nOpenJDK 64-Bit Server VM (build 17.0.12+10-LTS, mixed mode)\n",
                17,
                "",
                true,
                false,
            ),
            (
                "semeru",
                "openjdk version \"17.0.11\" 2024-04-16\nIBM Semeru Runtime Open Edition 17.0.11.0 (build 17.0.11+9)\nEclipse OpenJ9 VM 17.0.11.0 (build openj9-0.44.0, JRE 17 Windows 11 amd64-64-Bit Compressed References 20240418_812 (JIT enabled, AOT enabled))\n",
                17,
                "IBM Semeru",
                true,
                false,
            ),
        ];
        for (name, text, major, vendor, b64, graal) in cases {
            let v = parse_version_output(text).unwrap_or_else(|| panic!("{name}: no parse"));
            assert_eq!(v.major, *major, "{name}");
            assert_eq!(v.vendor, *vendor, "{name}");
            assert_eq!(v.is_64bit, *b64, "{name}");
            assert_eq!(v.graalvm, *graal, "{name}");
        }
        assert!(parse_version_output("Error: could not find java.dll").is_none());
    }

    #[test]
    fn mojang_runtime_folders() {
        assert_eq!(
            guess_from_path(Path::new(
                r"C:\mc\runtime\java-runtime-epsilon\windows-x64\java-runtime-epsilon"
            )),
            Some("25".into())
        );
        assert_eq!(
            guess_from_path(Path::new(
                r"C:\mc\runtime\jre-legacy\windows-x64\jre-legacy"
            )),
            Some("1.8.0".into())
        );
    }

    #[test]
    fn path_prefix() {
        assert!(path_starts_with(
            Path::new(r"C:\Users\A\AppData\Roaming\Hopper\data\java\jdk-21\bin\javaw.exe"),
            Path::new(r"c:\users\a\appdata\roaming\hopper\data\java\")
        ));
        assert!(!path_starts_with(
            Path::new(r"C:\Program Files\Java"),
            Path::new(r"C:\Users")
        ));
    }

    #[test]
    #[ignore]
    fn real_detect() {
        for j in detect(Path::new(r"G:\Games\Minecraft\Curse\Install")) {
            println!(
                "{} [{:?}] -> {}",
                j.label(),
                j.vendor_kind(),
                j.exe.display()
            );
        }
    }
}
