//! Changing a stopped server's Minecraft or loader version, with one-step
//! Undo where it can be undone.
//!
//! The steps: copies of the few files the change rewrites (start scripts,
//! `user_jvm_args.txt`, `.hopper-server.json`) set aside, the loader's
//! server files installed over the folder (the same installers New server
//! uses), the start script pointed at them (only the loader's token on the
//! java line changes, so the server's own flags and Java stay; a script
//! Hopper can't read is written new), `.hopper-server.json` and
//! `hopper.json` updated, then the result checked. A step that fails puts
//! those copies back.
//!
//! No backup is made unless the person asks for one ([`Change::backup`]):
//! backups are theirs to make, by hand or on their schedule.
//!
//! Undo: a loader build switch on the same Minecraft puts the copies back,
//! so the start script points at the old build again (an installer adds
//! the new build's libraries next to the old one's; it doesn't remove
//! them). A Minecraft change upgrades the world, which only a backup can
//! take back: with one, Undo restores it (mod files added since are moved
//! to `.hopper-backups` first so no mod is there twice, and the world comes
//! back too); without one there's no Undo.
//!
//! Before a Minecraft change, [`mod_check`] looks every identified mod up
//! on CurseForge / Modrinth for the new version and loader.

use super::backup;
use super::create::{ServerLoader, install_loader};
use super::script::{self, LaunchTarget, RUN_BAT, SCRIPTS};
use crate::instances::{self, Instance};
use crate::jobs::Job;
use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// The last change, kept for Undo.
pub const RECORD: &str = ".hopper-version-change.json";

/// Where the files a change rewrites are kept for Undo.
pub const KEPT_DIR: &str = ".hopper-version-change";

/// The files a change rewrites: what's needed to put a loader switch back.
const REWRITTEN: [&str; 5] = ["run.bat", "run.sh", "start.bat", "user_jvm_args.txt", super::META_FILE];

/// Files loader installers rewrite that hold the server's own settings.
const KEEP_THROUGH_INSTALL: [&str; 5] = ["run.bat", "run.sh", "start.bat", "user_jvm_args.txt", "server.properties"];

/// What a server folder runs now.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Installed {
    pub minecraft: String,
    /// "vanilla", "neoforge", "forge", "fabric", "quilt", "paper",
    /// "purpur"; "" when Hopper can't tell.
    pub kind: String,
    /// The loader's version ("21.1.265", "47.4.10", "0.16.9", a Paper
    /// build); "" for vanilla or when unknown.
    pub version: String,
}

impl Installed {
    /// "NeoForge 21.1.265", "Vanilla".
    pub fn label(&self) -> String {
        let name = kind_name(&self.kind);
        format!("{name} {}", self.version).trim().to_string()
    }

    /// The loader id an instance (`hopper.json`) uses.
    pub fn instance_loader(&self) -> String {
        match self.kind.as_str() {
            "neoforge" | "forge" | "fabric" | "quilt" if !self.version.is_empty() => format!("{}-{}", self.kind, self.version),
            "fabric" | "quilt" => self.kind.clone(),
            _ => String::new(),
        }
    }
}

pub fn kind_name(kind: &str) -> &'static str {
    match kind {
        "vanilla" => "Vanilla",
        "neoforge" => "NeoForge",
        "forge" => "Forge",
        "fabric" => "Fabric",
        "quilt" => "Quilt",
        "paper" => "Paper",
        "purpur" => "Purpur",
        _ => "",
    }
}

fn kind_of(l: &ServerLoader) -> String {
    l.name().to_ascii_lowercase()
}

/// What `root` runs: the start script's loader (the files are the truth),
/// with what Hopper noted when it made the server filling the gaps.
pub fn installed(root: &Path) -> Installed {
    let info = script::start_info(root);
    let meta = super::HostedMeta::load(root).unwrap_or_default();
    let mut out = Installed { minecraft: info.minecraft.clone().unwrap_or_default(), ..Default::default() };
    if let Some(l) = &info.loader {
        let (k, v) = l.split_once(' ').unwrap_or((l.as_str(), ""));
        out.kind = k.to_string();
        // Forge: "1.20.1-47.4.10".
        out.version = if k == "forge" { v.split_once('-').map_or(v, |(_, f)| f).to_string() } else { v.to_string() };
    } else {
        let low = meta.loader.to_ascii_lowercase();
        let k = low.split(' ').next().unwrap_or("");
        if !kind_name(k).is_empty() {
            out.kind = k.to_string();
            out.version = meta.loader.split_once(' ').map_or("", |(_, v)| v).trim().to_string();
        } else if root.join("server.jar").is_file() {
            out.kind = "vanilla".into();
        }
    }
    if out.minecraft.is_empty() {
        out.minecraft = meta.minecraft;
    }
    out
}

/// The loader `kind` at `version` (empty: the newest for Fabric, Quilt,
/// Paper, Purpur).
pub fn loader_for(kind: &str, version: &str) -> anyhow::Result<ServerLoader> {
    let opt = (!version.is_empty()).then(|| version.to_string());
    Ok(match kind {
        "vanilla" => ServerLoader::Vanilla,
        "neoforge" => ServerLoader::NeoForge(opt.context("Pick a NeoForge version.")?),
        "forge" => ServerLoader::Forge(opt.context("Pick a Forge version.")?),
        "fabric" => ServerLoader::Fabric(opt),
        "quilt" => ServerLoader::Quilt(opt),
        "" => bail!("Hopper can't tell what this server runs."),
        // Plugin servers and anything newer: the instance-style id.
        other => ServerLoader::from_instance(&format!("{other}-{version}"))?,
    })
}

/// One thing checked after a change.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Check {
    pub ok: bool,
    pub text: String,
}

/// The last change, for Undo.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Record {
    /// The backup made first, when the person asked for one; "" if none.
    pub backup: String,
    /// Of [`REWRITTEN`], the ones that weren't there before the change
    /// (the rest are copied into [`KEPT_DIR`]).
    pub absent: Vec<String>,
    pub from: Installed,
    pub to: Installed,
    /// Unix seconds.
    pub at: u64,
    pub checks: Vec<Check>,
    /// What else happened (mods updated / turned off).
    pub notes: Vec<String>,
}

impl Record {
    pub fn load(root: &Path) -> Option<Record> {
        serde_json::from_slice(&fs::read(root.join(RECORD)).ok()?).ok()
    }

    pub fn save(&self, root: &Path) -> anyhow::Result<()> {
        fs::write(root.join(RECORD), serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    /// Whether the server has run since (its log is newer): Undo then
    /// takes the world back too.
    pub fn ran_since(&self, root: &Path) -> bool {
        fs::metadata(root.join("logs/latest.log"))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .is_some_and(|d| d.as_secs() > self.at + 5)
    }

    pub fn ok(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }

    /// Whether Undo can put the server back: from the backup, or for a
    /// loader build switch on the same Minecraft, from the kept files.
    pub fn can_undo(&self) -> bool {
        !self.backup.is_empty() || (!self.from.minecraft.is_empty() && self.from.minecraft == self.to.minecraft)
    }
}

/// Copies of the files a change rewrites, held until the change is done.
struct Kept {
    files: Vec<(String, Vec<u8>)>,
    absent: Vec<String>,
}

impl Kept {
    fn take(root: &Path) -> Kept {
        let (mut files, mut absent) = (Vec::new(), Vec::new());
        for n in REWRITTEN {
            match fs::read(root.join(n)) {
                Ok(b) => files.push((n.to_string(), b)),
                Err(_) => absent.push(n.to_string()),
            }
        }
        Kept { files, absent }
    }

    /// Puts the files back as they were, and removes the ones that weren't
    /// there.
    fn put_back(&self, root: &Path) -> anyhow::Result<()> {
        for (n, b) in &self.files {
            fs::write(root.join(n), b).with_context(|| format!("putting {n} back"))?;
        }
        for n in &self.absent {
            let _ = fs::remove_file(root.join(n));
        }
        Ok(())
    }

    /// Keeps the copies in [`KEPT_DIR`] for Undo.
    fn save(&self, root: &Path) -> anyhow::Result<()> {
        let dir = root.join(KEPT_DIR);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir)?;
        for (n, b) in &self.files {
            fs::write(dir.join(n), b)?;
        }
        Ok(())
    }

    /// The copies a change kept, by the record's list.
    fn load(root: &Path, r: &Record) -> anyhow::Result<Kept> {
        let dir = root.join(KEPT_DIR);
        let mut files = Vec::new();
        for n in REWRITTEN.iter().filter(|n| !r.absent.iter().any(|a| a == *n)) {
            let p = dir.join(n);
            if p.is_file() {
                files.push((n.to_string(), fs::read(&p).with_context(|| format!("reading the kept {n}"))?));
            }
        }
        anyhow::ensure!(!files.is_empty(), "The files kept from before the change are gone, so it can't be undone.");
        Ok(Kept { files, absent: r.absent.clone() })
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The start script `read_launch` used, with its text.
fn current_script(root: &Path) -> Option<(PathBuf, String)> {
    let (_, text) = script::read_launch(root).ok()?;
    if text.is_empty() {
        return None;
    }
    SCRIPTS
        .iter()
        .map(|s| root.join(s))
        .find(|p| fs::read_to_string(p).ok().as_deref() == Some(text.as_str()))
        .map(|p| (p, text))
}

/// `text` with the loader token on its java line swapped for `target`'s:
/// an `@libraries/.../win_args.txt` (or unix_args) for another, or the
/// `-jar` file for another. None when the line has neither.
pub fn swap_target(text: &str, target: &LaunchTarget) -> Option<String> {
    match target {
        LaunchTarget::ArgsFile(p) => {
            let re = regex::Regex::new(r#"libraries[/\\][^\s"]*?(?:win|unix)_args\.txt"#).ok()?;
            let m = re.find(text)?;
            let new = if m.as_str().contains('\\') { p.replace('/', "\\") } else { p.clone() };
            Some(format!("{}{}{}", &text[..m.start()], new, &text[m.end()..]))
        }
        LaunchTarget::Jar(j) => {
            let re = regex::Regex::new(r#"(?i)-jar\s+("[^"]+"|\S+)"#).ok()?;
            let c = re.captures(text)?;
            let old = c.get(1)?;
            let new = if j.contains(char::is_whitespace) { format!("\"{j}\"") } else { j.clone() };
            Some(format!("{}{}{}", &text[..old.start()], new, &text[old.end()..]))
        }
    }
}

/// Points the start script at `target` (and `java`, when given), keeping
/// everything else on its line. A server without a script Hopper can
/// read gets a new run.bat.
fn point_script(root: &Path, target: &LaunchTarget, java: Option<&Path>, java_major: u32) -> anyhow::Result<String> {
    let fallback_java = java.map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("java"));
    let Some((path, text)) = current_script(root) else {
        script::write_run_bat(root, &fallback_java, target, java_major)?;
        return Ok("run.bat written".into());
    };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| RUN_BAT.into());
    let Some(mut new) = swap_target(&text, target) else {
        // A java line Hopper can't follow: Hopper's own run.bat instead.
        let launch = script::read_launch(root)?.0;
        script::write_run_bat(root, java.unwrap_or(&launch.java), target, java_major)?;
        return Ok(format!("{name} had no loader line; run.bat written"));
    };
    if let Some(j) = java
        && let Some(t) = script::rewrite_java(&new, j)
    {
        new = t;
    }
    if new != text {
        fs::write(&path, new).with_context(|| format!("writing {}", path.display()))?;
        return Ok(format!("{name} updated"));
    }
    Ok(format!("{name} already right"))
}

/// What the start script starts for `loader` after its files are in.
fn target_for(root: &Path, mc: &str, loader: &ServerLoader, java: &Path, job: &Job) -> anyhow::Result<LaunchTarget> {
    // Installers rewrite run.bat and friends; the server's own stay.
    let kept: Vec<(PathBuf, Vec<u8>)> = KEEP_THROUGH_INSTALL
        .iter()
        .map(|n| root.join(n))
        .filter_map(|p| fs::read(&p).ok().map(|b| (p, b)))
        .collect();
    let made: Vec<PathBuf> = KEEP_THROUGH_INSTALL.iter().map(|n| root.join(n)).filter(|p| !p.exists()).collect();
    let r = install_loader(root, mc, loader, java, job);
    for (p, b) in &kept {
        let _ = fs::write(p, b);
    }
    for p in made {
        let _ = fs::remove_file(p);
    }
    r
}

/// Checks the folder starts what it should.
fn check(root: &Path, to: &Installed, target: &LaunchTarget) -> Vec<Check> {
    let mut out = Vec::new();
    match script::launch_command(root) {
        Ok(l) => {
            out.push(Check { ok: true, text: "The start script reads".into() });
            let need = crate::java::recommended_major(&to.minecraft);
            match crate::java::inspect(&l.java) {
                Some(j) => out.push(Check {
                    ok: j.major >= need,
                    text: if j.major >= need {
                        format!("Java {} fits Minecraft {}", j.major, to.minecraft)
                    } else {
                        format!("Minecraft {} needs Java {need}; the server starts Java {}", to.minecraft, j.major)
                    },
                }),
                None if script::is_bare_java(&l.java) => {
                    out.push(Check { ok: true, text: format!("Plain java: Hopper picks a Java {need} at the start") })
                }
                None => out.push(Check { ok: false, text: format!("{} isn't a Java Hopper can run", l.java.display()) }),
            }
        }
        Err(e) => out.push(Check { ok: false, text: format!("The start script doesn't read: {e:#}") }),
    }
    let file = match target {
        LaunchTarget::ArgsFile(p) => p.clone(),
        LaunchTarget::Jar(j) => j.clone(),
    };
    let there = root.join(&file).is_file();
    out.push(Check { ok: there, text: if there { format!("{file} is there") } else { format!("{file} is missing") } });
    // Fabric and Quilt only show their new loader in the folder after the
    // first start; Forge and NeoForge name it in the start script.
    let now = installed(root);
    let named = matches!(to.kind.as_str(), "neoforge" | "forge");
    let same = now.minecraft == to.minecraft && (!named || now.version == to.version);
    out.push(Check {
        ok: same,
        text: if same {
            format!("The server reads as {} on Minecraft {}", to.label(), to.minecraft)
        } else {
            format!("The server reads as {} on Minecraft {}, not {} on {}", now.label(), now.minecraft, to.label(), to.minecraft)
        },
    });
    out
}

/// What [`change`] is asked for.
#[derive(Clone, Debug)]
pub struct Change {
    pub minecraft: String,
    pub loader: ServerLoader,
    /// The loader's version as picked ("" for vanilla, or the newest).
    pub version: String,
    /// A Java for the new version (runs installers; goes into the script
    /// only when the script's Java is too old).
    pub java: PathBuf,
    /// Make a full backup first (the person ticked "Back up the server
    /// first"). It's what lets a Minecraft change be undone.
    pub backup: bool,
}

/// Changes the stopped server in `root`. A failed install puts the files
/// it rewrote back and returns the error.
pub fn change(root: &Path, c: &Change, job: &Job) -> anyhow::Result<Record> {
    let from = installed(root);
    let to_kind = kind_of(&c.loader);
    if !from.kind.is_empty() && from.kind != to_kind {
        bail!("This server runs {}; changing to {} isn't something Hopper does in place.", kind_name(&from.kind), c.loader.name());
    }
    anyhow::ensure!(!c.minecraft.trim().is_empty(), "Pick a Minecraft version.");
    if from.minecraft == c.minecraft && !from.version.is_empty() && from.version == c.version {
        bail!("It's on {} for Minecraft {} already.", from.label(), from.minecraft);
    }
    let to = Installed { minecraft: c.minecraft.clone(), kind: to_kind, version: c.version.clone() };

    let backup_name = if c.backup {
        job.detail("Backing up the whole server");
        backup::make(
            root,
            backup::Scope::Full,
            backup::Kind::Manual,
            &format!("Before changing to {} on Minecraft {} (was {} on {})", to.label(), to.minecraft, from.label(), from.minecraft),
            None,
            job,
        )?
        .name
    } else {
        String::new()
    };
    let kept = Kept::take(root);

    let work = (|| -> anyhow::Result<Vec<String>> {
        let mut notes = Vec::new();
        let java = script::console_java(&c.java);
        let need = crate::java::recommended_major(&c.minecraft);
        let new_major = crate::java::inspect(&java).map_or(need, |j| j.major);
        // The script's Java stays unless it's too old for the new version.
        let old_java = script::read_launch(root).ok().map(|(l, _)| l.java);
        let swap_java = match &old_java {
            Some(j) if script::is_bare_java(j) => None,
            Some(j) => crate::java::inspect(j).filter(|i| i.major >= need).is_none().then_some(java.as_path()),
            None => Some(java.as_path()),
        };
        job.detail(format!("Installing {}", to.label()));
        let target = target_for(root, &c.minecraft, &c.loader, &java, job)?;
        notes.push(point_script(root, &target, swap_java, new_major)?);
        if let Some(j) = swap_java {
            notes.push(format!("Java changed to {}", j.display()));
        }
        super::HostedMeta { minecraft: to.minecraft.clone(), loader: to.label() }.save(root)?;
        if root.join(instances::HOPPER_FILE).is_file() {
            let (mc, loader) = (to.minecraft.clone(), to.instance_loader());
            Instance::update_on_disk(root, |i| {
                i.game_version = mc;
                if !loader.is_empty() {
                    i.loader = loader;
                }
            })?;
        }
        Ok(notes)
    })();
    let notes = match work {
        Ok(n) => n,
        Err(e) => {
            job.detail("Putting the start files back");
            return Err(match kept.put_back(root) {
                Ok(_) => e.context("the change failed; the server starts as it did before"),
                Err(e2) => e.context(format!("the change failed, and putting the start files back failed too: {e2:#}")),
            });
        }
    };
    kept.save(root)?;
    job.detail("Checking the result");
    let target = match script::read_launch(root) {
        Ok((l, _)) => target_of(&l).unwrap_or(LaunchTarget::Jar("server.jar".into())),
        Err(_) => LaunchTarget::Jar("server.jar".into()),
    };
    let r = Record { backup: backup_name, absent: kept.absent, checks: check(root, &to, &target), from, to, at: now(), notes };
    r.save(root)?;
    Ok(r)
}

/// The loader token of a launch.
fn target_of(l: &script::Launch) -> Option<LaunchTarget> {
    if let Some(a) = l.args.iter().find(|a| a.starts_with('@') && a.contains("_args.txt")) {
        return Some(LaunchTarget::ArgsFile(a[1..].replace('\\', "/")));
    }
    let i = l.args.iter().position(|a| a == "-jar")?;
    Some(LaunchTarget::Jar(l.args.get(i + 1)?.clone()))
}

/// Mod files in `mods/` (top level) that `backup` doesn't hold go to
/// `.hopper-backups/undo-<time>`, then the backup comes back.
fn undo_files(root: &Path, backup: &str, job: &Job) -> anyhow::Result<usize> {
    let had: std::collections::HashSet<String> = backup::entries(root, backup)?.into_iter().map(|(n, _)| n).collect();
    let aside = root.join(".hopper-backups").join(format!("undo-{}", now()));
    for folder in ["mods", "plugins"] {
        for e in fs::read_dir(root.join(folder)).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.path().is_file() && !had.contains(&format!("{folder}/{name}")) {
                fs::create_dir_all(aside.join(folder))?;
                fs::rename(e.path(), aside.join(folder).join(&name)).with_context(|| format!("moving {folder}/{name} aside"))?;
            }
        }
    }
    backup::restore(root, backup, &[], job)
}

/// Undoes the last change: its backup comes back (world too) when one
/// was made; otherwise a loader switch on the same Minecraft gets its old
/// start files back, pointing at the old build again.
pub fn undo(root: &Path, job: &Job) -> anyhow::Result<Record> {
    let r = Record::load(root).context("There's no version change to undo.")?;
    if !r.backup.is_empty() {
        undo_files(root, &r.backup, job)?;
    } else if r.can_undo() {
        job.detail("Putting the start files back");
        Kept::load(root, &r)?.put_back(root)?;
        if root.join(instances::HOPPER_FILE).is_file() {
            let (mc, loader) = (r.from.minecraft.clone(), r.from.instance_loader());
            Instance::update_on_disk(root, |i| {
                i.game_version = mc;
                if !loader.is_empty() {
                    i.loader = loader;
                }
            })?;
        }
    } else {
        bail!("No backup was made before this Minecraft change, so it can't be undone.");
    }
    let _ = fs::remove_file(root.join(RECORD));
    let _ = fs::remove_dir_all(root.join(KEPT_DIR));
    Ok(r)
}

/// How a mod fares on another version.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    /// The installed file works there too.
    Fits,
    /// Another file is for it.
    Update,
    /// No file for it.
    None,
    /// Not on CurseForge or Modrinth (or not looked up yet).
    Unknown,
}

/// Ids as strings in JSON: Modrinth's numbers use all 64 bits, more than
/// a browser's numbers hold.
mod id_str {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Id {
            S(String),
            N(u64),
        }
        match Id::deserialize(d)? {
            Id::S(s) => s.parse().map_err(serde::de::Error::custom),
            Id::N(n) => Ok(n),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModFit {
    pub file: String,
    pub name: String,
    pub icon: String,
    #[serde(with = "id_str")]
    pub project_id: u64,
    pub fit: Fit,
    /// For `Update`: the file to install.
    #[serde(with = "id_str")]
    pub new_file_id: u64,
    pub new_file: String,
    pub from_pack: bool,
}

/// Every enabled mod in `inst` on Minecraft `mc` with loader id `loader`
/// (`hopper.json` form): fits, has another file, has none, or unknown.
pub fn mod_check(inst: &Instance, mc: &str, loader: &str) -> Vec<ModFit> {
    let mut there = inst.clone();
    there.game_version = mc.to_string();
    if !loader.is_empty() {
        there.loader = loader.to_string();
    }
    let addons: Vec<_> = inst
        .addons
        .iter()
        .filter(|a| a.enabled && a.folder() == "mods")
        .cloned()
        .collect();
    let tracked: std::collections::HashSet<String> = inst.addons.iter().map(|a| a.file_name.to_lowercase()).collect();
    // A few at a time: one lookup per mod.
    let next = std::sync::atomic::AtomicUsize::new(0);
    let out = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(a) = addons.get(i) else { break };
                let (fit, id, file) = if a.project_id == 0 {
                    (Fit::Unknown, 0, String::new())
                } else {
                    match crate::install::best_file(a.project_id, Some(if a.class_id == 0 { crate::cf::CLASS_MODS } else { a.class_id }), &there) {
                        Ok(Some(f)) if f.id == a.file_id => (Fit::Fits, 0, String::new()),
                        Ok(Some(f)) => (Fit::Update, f.id, f.file_name),
                        Ok(None) => (Fit::None, 0, String::new()),
                        Err(e) => {
                            log::warn!("version check: {}: {e:#}", a.name);
                            (Fit::Unknown, 0, String::new())
                        }
                    }
                };
                out.lock().unwrap().push(ModFit {
                    file: a.file_name.clone(),
                    name: if a.name.is_empty() { a.file_name.clone() } else { a.name.clone() },
                    icon: a.icon.clone(),
                    project_id: a.project_id,
                    fit,
                    new_file_id: id,
                    new_file: file,
                    from_pack: a.from_pack,
                });
            });
        }
    });
    let mut out = out.into_inner().unwrap();
    // Jars Hopper doesn't track at all.
    for e in fs::read_dir(inst.mods_dir()).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.to_ascii_lowercase().ends_with(".jar") && !tracked.contains(&name.to_lowercase()) {
            out.push(ModFit {
                file: name.clone(),
                name,
                icon: String::new(),
                project_id: 0,
                fit: Fit::Unknown,
                new_file_id: 0,
                new_file: String::new(),
                from_pack: false,
            });
        }
    }
    let order = |f: &Fit| match f {
        Fit::None => 0,
        Fit::Unknown => 1,
        Fit::Update => 2,
        Fit::Fits => 3,
    };
    out.sort_by(|a, b| order(&a.fit).cmp(&order(&b.fit)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    out
}

/// After a Minecraft change: installs the `Update` files, and turns off
/// the `None` ones (and `Unknown`, when `off_unknown`). Notes for the
/// record.
pub fn apply_mods(root: &Path, fits: &[ModFit], update: bool, off_none: bool, off_unknown: bool, job: &Job) -> Vec<String> {
    let Some(mut inst) = instances::load(root) else { return Vec::new() };
    let (mut updated, mut off, mut failed) = (0, 0, Vec::new());
    for (i, m) in fits.iter().enumerate() {
        job.progress(i, fits.len());
        let r = match m.fit {
            Fit::Update if update => {
                job.detail(format!("Updating {}", m.name));
                crate::install::install_file(&mut inst, m.project_id, m.new_file_id, job).map(|_| updated += 1)
            }
            Fit::None if off_none => turn_off(&mut inst, &m.file).map(|_| off += 1),
            Fit::Unknown if off_unknown => turn_off(&mut inst, &m.file).map(|_| off += 1),
            _ => Ok(()),
        };
        if let Err(e) = r {
            log::warn!("{}: {e:#}", m.name);
            failed.push(m.name.clone());
        }
    }
    let mut notes = Vec::new();
    if updated > 0 {
        notes.push(format!("{updated} mod{} updated", if updated == 1 { "" } else { "s" }));
    }
    if off > 0 {
        notes.push(format!("{off} mod{} turned off", if off == 1 { "" } else { "s" }));
    }
    if !failed.is_empty() {
        notes.push(format!("Couldn't change: {}", failed.join(", ")));
    }
    notes
}

fn turn_off(inst: &mut Instance, file: &str) -> anyhow::Result<()> {
    if let Some(i) = inst.addons.iter().position(|a| a.file_name.eq_ignore_ascii_case(file)) {
        return crate::install::set_enabled(inst, i, false);
    }
    let p = inst.mods_dir().join(file);
    fs::rename(&p, p.with_file_name(format!("{file}.disabled"))).with_context(|| format!("turning off {file}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swaps_only_the_loader_token() {
        let neo = "@echo off\r\n\"C:\\Java\\bin\\java.exe\" -Dx=1 @user_jvm_args.txt @libraries/net/neoforged/neoforge/21.1.200/win_args.txt nogui %*\r\npause\r\n";
        let t = LaunchTarget::ArgsFile("libraries/net/neoforged/neoforge/21.1.265/win_args.txt".into());
        let out = swap_target(neo, &t).unwrap();
        assert_eq!(out, neo.replace("21.1.200", "21.1.265"));
        let back = "java @user_jvm_args.txt @libraries\\net\\neoforged\\neoforge\\21.1.200\\win_args.txt nogui";
        assert_eq!(swap_target(back, &t).unwrap(), back.replace("21.1.200", "21.1.265"));
        let forge = "java @libraries/net/minecraftforge/forge/1.20.1-47.3.0/win_args.txt nogui";
        let ft = LaunchTarget::ArgsFile("libraries/net/minecraftforge/forge/1.20.1-47.4.10/win_args.txt".into());
        assert_eq!(swap_target(forge, &ft).unwrap(), forge.replace("47.3.0", "47.4.10"));

        let paper = "java -Xmx4G -jar paper-1.21.1-133.jar nogui";
        let pt = LaunchTarget::Jar("paper-1.21.4-15.jar".into());
        assert_eq!(swap_target(paper, &pt).unwrap(), "java -Xmx4G -jar paper-1.21.4-15.jar nogui");
        let quoted = "java -jar \"my server.jar\" nogui";
        assert_eq!(swap_target(quoted, &LaunchTarget::Jar("server.jar".into())).unwrap(), "java -jar server.jar nogui");
        assert!(swap_target("java -cp x Main", &pt).is_none());
    }

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopper-vc-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn mod_ids_survive_a_browser() {
        let m = ModFit {
            file: "a.jar".into(),
            name: "A".into(),
            icon: String::new(),
            project_id: 17092382206347359202,
            fit: Fit::Update,
            new_file_id: 1 << 63 | 5,
            new_file: "a2.jar".into(),
            from_pack: false,
        };
        let j = serde_json::to_value(&m).unwrap();
        assert_eq!(j["project_id"], "17092382206347359202");
        assert_eq!(serde_json::from_value::<ModFit>(j).unwrap(), m);
        let n: ModFit = serde_json::from_str(r#"{"file":"","name":"","icon":"","project_id":7,"fit":"none","new_file_id":0,"new_file":"","from_pack":false}"#).unwrap();
        assert_eq!((n.project_id, n.fit), (7, Fit::None));
    }

    #[test]
    fn reads_what_a_server_runs() {
        let root = temp("installed");
        fs::write(root.join("run.bat"), "java @user_jvm_args.txt @libraries/net/neoforged/neoforge/21.1.200/win_args.txt nogui").unwrap();
        let i = installed(&root);
        assert_eq!((i.minecraft.as_str(), i.kind.as_str(), i.version.as_str()), ("1.21.1", "neoforge", "21.1.200"));
        assert_eq!((i.label().as_str(), i.instance_loader().as_str()), ("NeoForge 21.1.200", "neoforge-21.1.200"));

        fs::write(root.join("run.bat"), "java @libraries/net/minecraftforge/forge/1.20.1-47.4.10/win_args.txt nogui").unwrap();
        let i = installed(&root);
        assert_eq!((i.minecraft.as_str(), i.kind.as_str(), i.version.as_str()), ("1.20.1", "forge", "47.4.10"));

        fs::write(root.join("run.bat"), "java -jar server.jar nogui").unwrap();
        fs::write(root.join("server.jar"), "jar").unwrap();
        super::super::HostedMeta { minecraft: "1.21.4".into(), loader: "Vanilla".into() }.save(&root).unwrap();
        let i = installed(&root);
        assert_eq!((i.minecraft.as_str(), i.kind.as_str(), i.version.as_str()), ("1.21.4", "vanilla", ""));
        assert_eq!(i.label(), "Vanilla");
        let _ = fs::remove_dir_all(&root);
    }

    fn inst(mc: &str, v: &str) -> Installed {
        Installed { minecraft: mc.into(), kind: "neoforge".into(), version: v.into() }
    }

    #[test]
    fn a_loader_switch_undoes_from_the_kept_start_files() {
        let root = temp("kept");
        fs::write(root.join("run.bat"), "java @libraries/net/neoforged/neoforge/21.1.259/win_args.txt nogui").unwrap();
        fs::write(root.join("user_jvm_args.txt"), "-Xmx8G").unwrap();
        let kept = Kept::take(&root);
        assert!(kept.absent.iter().any(|n| n == "run.sh"));
        kept.save(&root).unwrap();
        let r = Record { from: inst("1.21.1", "21.1.259"), to: inst("1.21.1", "21.1.265"), absent: kept.absent.clone(), at: now(), ..Default::default() };
        assert!(r.can_undo());
        r.save(&root).unwrap();
        // The switch: the script points at the new build, an installer
        // left a run.sh, the old build's libraries stay.
        fs::write(root.join("run.bat"), "java @libraries/net/neoforged/neoforge/21.1.265/win_args.txt nogui").unwrap();
        fs::write(root.join("run.sh"), "new").unwrap();
        fs::create_dir_all(root.join("world")).unwrap();
        fs::write(root.join("world/level.dat"), "w2").unwrap();

        let job = crate::jobs::Jobs::default().start("t");
        undo(&root, &job).unwrap();
        assert!(fs::read_to_string(root.join("run.bat")).unwrap().contains("21.1.259"));
        assert_eq!(fs::read_to_string(root.join("user_jvm_args.txt")).unwrap(), "-Xmx8G");
        assert!(!root.join("run.sh").exists());
        // Nothing else is touched, and no backup was made.
        assert_eq!(fs::read_to_string(root.join("world/level.dat")).unwrap(), "w2");
        assert!(backup::list(&root).is_empty());
        assert!(!root.join(KEPT_DIR).exists() && Record::load(&root).is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_minecraft_change_without_a_backup_has_no_undo() {
        let root = temp("noundo");
        fs::write(root.join("run.bat"), "old").unwrap();
        let r = Record { from: inst("1.20.1", "47.4.10"), to: inst("1.21.1", "21.1.265"), at: now(), ..Default::default() };
        assert!(!r.can_undo());
        r.save(&root).unwrap();
        let job = crate::jobs::Jobs::default().start("t");
        assert!(undo(&root, &job).is_err());
        assert!(Record::load(&root).is_some());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_failed_change_puts_the_start_files_back_without_a_backup() {
        let root = temp("fail");
        fs::write(root.join("run.bat"), "java @libraries/net/neoforged/neoforge/21.1.259/win_args.txt nogui").unwrap();
        fs::write(root.join("server.properties"), "level-name=world
").unwrap();
        // A build that doesn't exist: the installer download fails.
        let c = Change {
            minecraft: "1.21.1".into(),
            loader: ServerLoader::NeoForge("0.0.0-nope".into()),
            version: "0.0.0-nope".into(),
            java: PathBuf::from("java"),
            backup: false,
        };
        let job = crate::jobs::Jobs::default().start("t");
        assert!(change(&root, &c, &job).is_err());
        assert!(fs::read_to_string(root.join("run.bat")).unwrap().contains("21.1.259"));
        assert!(!root.join("run.sh").exists());
        assert!(backup::list(&root).is_empty());
        assert!(Record::load(&root).is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn undo_puts_files_back_and_moves_new_mods_aside() {
        let root = temp("undo");
        fs::write(root.join("server.properties"), "level-name=world\n").unwrap();
        fs::create_dir_all(root.join("mods")).unwrap();
        fs::create_dir_all(root.join("world")).unwrap();
        fs::write(root.join("mods/a-1.jar"), "a1").unwrap();
        fs::write(root.join("run.bat"), "old").unwrap();
        fs::write(root.join("world/level.dat"), "w1").unwrap();
        let job = crate::jobs::Jobs::default().start("t");
        let b = backup::make(&root, backup::Scope::Full, backup::Kind::Safety, "t", None, &job).unwrap();
        Record { backup: b.name.clone(), at: now(), ..Default::default() }.save(&root).unwrap();
        // The change: a new mod file, the old one off, a new script.
        fs::write(root.join("mods/a-2.jar"), "a2").unwrap();
        fs::rename(root.join("mods/a-1.jar"), root.join("mods/a-1.jar.disabled")).unwrap();
        fs::write(root.join("run.bat"), "new").unwrap();
        fs::write(root.join("world/level.dat"), "w2").unwrap();

        undo(&root, &job).unwrap();
        assert_eq!(fs::read_to_string(root.join("run.bat")).unwrap(), "old");
        assert_eq!(fs::read_to_string(root.join("mods/a-1.jar")).unwrap(), "a1");
        assert!(!root.join("mods/a-2.jar").exists() && !root.join("mods/a-1.jar.disabled").exists());
        assert_eq!(fs::read_to_string(root.join("world/level.dat")).unwrap(), "w1");
        assert!(Record::load(&root).is_none());
        assert!(undo(&root, &job).is_err());
        let _ = fs::remove_dir_all(&root);
    }
}
