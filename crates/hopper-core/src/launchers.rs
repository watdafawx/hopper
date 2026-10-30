//! Instances other launchers keep (Modrinth App, Prism / MultiMC,
//! ATLauncher, GDLauncher, FTB App), read where they are so Hopper can
//! manage them in place. CurseForge's own folder is the main library and is
//! read by [`crate::instances`].

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Modrinth,
    Prism,
    MultiMC,
    ATLauncher,
    GDLauncher,
    FtbApp,
}

impl Kind {
    pub const ALL: [Kind; 6] = [
        Kind::Modrinth,
        Kind::Prism,
        Kind::MultiMC,
        Kind::ATLauncher,
        Kind::GDLauncher,
        Kind::FtbApp,
    ];

    /// Stable name for settings ("prism").
    pub fn key(self) -> &'static str {
        match self {
            Kind::Modrinth => "modrinth",
            Kind::Prism => "prism",
            Kind::MultiMC => "multimc",
            Kind::ATLauncher => "atlauncher",
            Kind::GDLauncher => "gdlauncher",
            Kind::FtbApp => "ftbapp",
        }
    }

    pub fn from_key(k: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|x| x.key() == k)
    }

    /// "Prism Launcher", for showing.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Modrinth => "Modrinth App",
            Kind::Prism => "Prism Launcher",
            Kind::MultiMC => "MultiMC",
            Kind::ATLauncher => "ATLauncher",
            Kind::GDLauncher => "GDLauncher",
            Kind::FtbApp => "FTB App",
        }
    }
}

/// An instance another launcher keeps.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Foreign {
    pub kind: Option<Kind>,
    pub name: String,
    /// The folder the game runs in (mods/, saves/, …).
    pub game_dir: PathBuf,
    pub game_version: String,
    /// Hopper's form: "neoforge-21.1.256", "fabric-0.16.5", "" for vanilla.
    pub loader: String,
    pub icon: Option<PathBuf>,
    /// Seconds since the epoch.
    pub last_played: Option<i64>,
    pub play_seconds: u64,
    /// The file the launcher keeps it in (its mtime says when it changed).
    pub meta_file: PathBuf,
    /// CurseForge (project, file) when it came from a CurseForge pack.
    pub cf_pack: Option<(u64, u64)>,
}

/// Every instance in the launcher folders Hopper manages, as Hopper
/// instances (a folder of an unknown launcher is skipped).
pub fn instances(dirs: &[crate::settings::LauncherDir]) -> Vec<crate::instances::Instance> {
    // HOPPER_LAUNCHER_DIR=prism=C:\path adds one for this run (tests).
    let extra: Vec<crate::settings::LauncherDir> = std::env::var("HOPPER_LAUNCHER_DIR")
        .ok()
        .and_then(|v| {
            let (k, p) = v.split_once('=')?;
            Some(crate::settings::LauncherDir { kind: k.into(), path: p.into() })
        })
        .into_iter()
        .collect();
    dirs.iter()
        .chain(&extra)
        .filter_map(|d| Some((Kind::from_key(&d.kind)?, PathBuf::from(&d.path))))
        .flat_map(|(k, p)| list(k, &p))
        .filter_map(|f| crate::instances::load_foreign(&f))
        .collect()
}

/// Launchers installed on this PC with at least one instance, and how many.
pub fn detect() -> Vec<(Kind, PathBuf, usize)> {
    Kind::ALL
        .into_iter()
        .filter_map(|k| {
            let d = default_dir(k)?;
            let n = list(k, &d).len();
            (n > 0).then_some((k, d, n))
        })
        .collect()
}

/// Hopper's loader string from a loader name and version.
pub fn loader_string(name: &str, version: &str) -> String {
    let n = name.to_lowercase();
    let n = n.trim();
    let kind = if n.contains("neoforge") || n.contains("neoforged") {
        "neoforge"
    } else if n.contains("forge") {
        "forge"
    } else if n.contains("quilt") {
        "quilt"
    } else if n.contains("fabric") {
        "fabric"
    } else {
        return String::new();
    };
    let v = version.trim();
    if v.is_empty() { kind.to_string() } else { format!("{kind}-{v}") }
}

/// Where a launcher keeps its instances on this PC, when it's installed.
pub fn default_dir(kind: Kind) -> Option<PathBuf> {
    #[cfg(windows)]
    let (appdata, local, home) = (
        std::env::var_os("APPDATA").map(PathBuf::from),
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
        std::env::var_os("USERPROFILE").map(PathBuf::from),
    );
    // Off Windows the launchers use the user's data folder
    // (~/.local/share, or Application Support on macOS) where Windows
    // has %APPDATA%.
    #[cfg(not(windows))]
    let (appdata, local, home) = {
        let base = directories::BaseDirs::new();
        let data = base.as_ref().map(|b| b.data_dir().to_path_buf());
        (data.clone(), data, base.map(|b| b.home_dir().to_path_buf()))
    };
    let candidates: Vec<PathBuf> = match kind {
        Kind::Modrinth => appdata
            .iter()
            .flat_map(|a| [a.join("ModrinthApp"), a.join("com.modrinth.theseus")])
            .collect(),
        Kind::Prism => {
            let mut v: Vec<PathBuf> = appdata.iter().map(|a| a.join("PrismLauncher")).collect();
            // The instances folder can be moved in Prism's settings.
            if let Some(a) = &appdata
                && let Ok(cfg) = fs::read_to_string(a.join("PrismLauncher").join("prismlauncher.cfg"))
                && let Some(dir) = ini_value(&cfg, "InstanceDir")
            {
                let p = PathBuf::from(&dir);
                v.insert(0, if p.is_absolute() { p } else { a.join("PrismLauncher").join(p) });
            }
            v
        }
        Kind::MultiMC => home.iter().map(|h| h.join("MultiMC")).collect(),
        Kind::ATLauncher => appdata.iter().map(|a| a.join("ATLauncher")).collect(),
        Kind::GDLauncher => appdata
            .iter()
            .flat_map(|a| [a.join("gdlauncher_carbon").join("data"), a.join("gdlauncher_next")])
            .collect(),
        Kind::FtbApp => local
            .iter()
            .map(|l| l.join(".ftba"))
            .chain(home.iter().map(|h| h.join(".ftba")))
            .collect(),
    };
    candidates.into_iter().find(|d| d.is_dir())
}

/// The instances in a launcher's folder (as [`default_dir`] gives, or one
/// the user picked: the launcher's folder or its instances folder).
pub fn list(kind: Kind, dir: &Path) -> Vec<Foreign> {
    let mut out = match kind {
        Kind::Modrinth => modrinth(dir),
        Kind::Prism | Kind::MultiMC => in_instances(dir, |p| prism(p)),
        Kind::ATLauncher => in_instances(dir, atlauncher),
        Kind::GDLauncher => in_instances(dir, gdlauncher),
        Kind::FtbApp => in_instances(dir, ftb),
    };
    for f in &mut out {
        f.kind = Some(kind);
    }
    out.sort_by_key(|f| f.name.to_lowercase());
    out
}

/// Runs `read` over every folder in `dir/instances` (or `dir` itself when
/// it is the instances folder).
fn in_instances(dir: &Path, read: impl Fn(&Path) -> Option<Foreign>) -> Vec<Foreign> {
    let inst = dir.join("instances");
    let base = if inst.is_dir() { inst } else { dir.to_path_buf() };
    fs::read_dir(&base)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter_map(|p| read(&p))
        .collect()
}

fn ini_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        (k.trim() == key).then(|| v.trim().to_string()).filter(|v| !v.is_empty())
    })
}

fn folder_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn json(p: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(p).ok()?).ok()
}

fn s(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

// ── Prism / MultiMC ─────────────────────────────────────────────────

fn prism(dir: &Path) -> Option<Foreign> {
    let cfg_path = dir.join("instance.cfg");
    let cfg = fs::read_to_string(&cfg_path).ok()?;
    // Newer Prism uses minecraft/, MultiMC and older Prism .minecraft/.
    let game_dir = [dir.join("minecraft"), dir.join(".minecraft")]
        .into_iter()
        .find(|p| p.is_dir())
        .unwrap_or_else(|| dir.join("minecraft"));
    let pack = json(&dir.join("mmc-pack.json"));
    let comps = pack.as_ref().and_then(|p| p["components"].as_array().cloned()).unwrap_or_default();
    let version_of = |uid: &str| {
        comps
            .iter()
            .find(|c| c["uid"] == uid)
            .map(|c| s(&c["version"]))
            .unwrap_or_default()
    };
    let loader = [
        ("net.neoforged", "neoforge"),
        ("net.minecraftforge", "forge"),
        ("net.fabricmc.fabric-loader", "fabric"),
        ("org.quiltmc.quilt-loader", "quilt"),
    ]
    .iter()
    .find_map(|(uid, name)| {
        let v = version_of(uid);
        (!v.is_empty()).then(|| loader_string(name, &v))
    })
    .unwrap_or_default();
    let icon = ini_value(&cfg, "iconKey").and_then(|k| {
        let icons = dir.parent()?.parent()?.join("icons");
        ["png", "jpg", "svg", "ico"]
            .iter()
            .map(|x| icons.join(format!("{k}.{x}")))
            .find(|p| p.exists())
    });
    let cf_pack = (ini_value(&cfg, "ManagedPackType").as_deref() == Some("flame"))
        .then(|| {
            let p = ini_value(&cfg, "ManagedPackID")?.parse().ok()?;
            let f = ini_value(&cfg, "ManagedPackVersionID")?.parse().ok()?;
            Some((p, f))
        })
        .flatten();
    Some(Foreign {
        name: ini_value(&cfg, "name").unwrap_or_else(|| folder_name(dir)),
        game_dir,
        game_version: version_of("net.minecraft"),
        loader,
        icon,
        last_played: ini_value(&cfg, "lastLaunchTime")
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|t| *t > 0)
            .map(|ms| ms / 1000),
        play_seconds: ini_value(&cfg, "totalTimePlayed").and_then(|v| v.parse().ok()).unwrap_or(0),
        meta_file: cfg_path,
        cf_pack,
        kind: None,
    })
}

// ── ATLauncher ──────────────────────────────────────────────────────

fn atlauncher(dir: &Path) -> Option<Foreign> {
    let meta = dir.join("instance.json");
    let v = json(&meta)?;
    let l = &v["launcher"];
    let lv = &l["loaderVersion"];
    let cf = &l["curseForgeProject"];
    let cf_file = &l["curseForgeFile"];
    Some(Foreign {
        name: Some(s(&l["name"])).filter(|n| !n.is_empty()).unwrap_or_else(|| folder_name(dir)),
        game_dir: dir.to_path_buf(),
        game_version: Some(s(&v["id"])).filter(|x| !x.is_empty()).unwrap_or_else(|| s(&l["minecraft"])),
        loader: loader_string(&s(&lv["type"]), &s(&lv["version"])),
        icon: Some(dir.join("instance.png")).filter(|p| p.exists()),
        last_played: None,
        play_seconds: 0,
        meta_file: meta,
        cf_pack: cf["id"].as_u64().zip(cf_file["id"].as_u64()),
        kind: None,
    })
}

// ── GDLauncher ──────────────────────────────────────────────────────

fn gdlauncher(dir: &Path) -> Option<Foreign> {
    // GDLauncher (Carbon): instance.json, the game in instance/.
    let carbon = dir.join("instance.json");
    if let Some(v) = json(&carbon) {
        let std = &v["game_configuration"]["version"]["Standard"];
        let ml = std["modloaders"].as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
        let game_dir = dir.join("instance");
        return Some(Foreign {
            name: Some(s(&v["name"])).filter(|n| !n.is_empty()).unwrap_or_else(|| folder_name(dir)),
            game_dir: if game_dir.is_dir() { game_dir } else { dir.to_path_buf() },
            game_version: s(&std["release"]),
            loader: loader_string(&s(&ml["type_"]), &s(&ml["version"])),
            icon: v["icon"]
                .as_str()
                .map(|i| dir.join(i))
                .filter(|p| p.is_file()),
            last_played: v["last_played"].as_str().and_then(crate::time::parse_iso),
            play_seconds: v["seconds_played"].as_u64().unwrap_or(0),
            meta_file: carbon,
            cf_pack: None,
            kind: None,
        });
    }
    // The older GDLauncher: config.json next to mods/.
    let legacy = dir.join("config.json");
    let v = json(&legacy)?;
    let l = &v["loader"];
    let ty = s(&l["loaderType"]);
    Some(Foreign {
        name: folder_name(dir),
        game_dir: dir.to_path_buf(),
        game_version: s(&l["mcVersion"]),
        loader: if ty == "vanilla" {
            String::new()
        } else {
            // Forge versions are written as "1.20.1-47.2.0".
            let lv = s(&l["loaderVersion"]);
            let lv = lv.rsplit('-').next().unwrap_or(&lv).to_string();
            loader_string(&ty, &lv)
        },
        icon: Some(dir.join("background.png")).filter(|p| p.exists()),
        last_played: None,
        play_seconds: v["timePlayed"].as_u64().map_or(0, |m| m * 60),
        meta_file: legacy,
        cf_pack: l["projectID"].as_u64().zip(l["fileID"].as_u64()),
        kind: None,
    })
}

// ── FTB App ─────────────────────────────────────────────────────────

fn ftb(dir: &Path) -> Option<Foreign> {
    let meta = dir.join("instance.json");
    let v = json(&meta)?;
    // "neoforge" + "21.1.x", or "modLoader": "neoforge-21.1.x".
    let ml = s(&v["modLoader"]);
    let loader = match ml.split_once('-') {
        Some((n, ver)) if !ver.is_empty() && ver.chars().next().is_some_and(|c| c.is_ascii_digit()) => {
            loader_string(n, ver)
        }
        _ => loader_string(&ml, &s(&v["modLoaderVersion"])),
    };
    Some(Foreign {
        name: Some(s(&v["name"])).filter(|n| !n.is_empty()).unwrap_or_else(|| folder_name(dir)),
        game_dir: dir.to_path_buf(),
        game_version: s(&v["mcVersion"]),
        loader,
        icon: Some(dir.join("folder.jpg")).filter(|p| p.exists()),
        last_played: v["lastPlayed"].as_i64().filter(|t| *t > 0).map(|ms| ms / 1000),
        play_seconds: v["totalPlayTime"].as_u64().map_or(0, |ms| ms / 1000),
        meta_file: meta,
        cf_pack: None,
        kind: None,
    })
}

// ── Modrinth App ────────────────────────────────────────────────────

fn modrinth(dir: &Path) -> Vec<Foreign> {
    // The app keeps the list in app.db; profiles/ holds the game folders.
    let (base, profiles) = if dir.join("profiles").is_dir() {
        (dir.to_path_buf(), dir.join("profiles"))
    } else {
        (dir.parent().map(Path::to_path_buf).unwrap_or_default(), dir.to_path_buf())
    };
    let db = base.join("app.db");
    if let Ok(v) = modrinth_db(&db, &profiles) {
        return v;
    }
    // Older versions: profile.json in each folder.
    fs::read_dir(&profiles)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter_map(|p| {
            let meta = p.join("profile.json");
            let v = json(&meta)?;
            let m = &v["metadata"];
            Some(Foreign {
                name: Some(s(&m["name"])).filter(|n| !n.is_empty()).unwrap_or_else(|| folder_name(&p)),
                game_version: s(&m["game_version"]),
                loader: loader_string(&s(&m["loader"]), &s(&m["loader_version"]["id"])),
                icon: m["icon"].as_str().map(PathBuf::from).filter(|i| i.is_file()),
                last_played: None,
                play_seconds: 0,
                meta_file: meta,
                cf_pack: None,
                kind: None,
                game_dir: p,
            })
        })
        .collect()
}

fn modrinth_db(db: &Path, profiles: &Path) -> anyhow::Result<Vec<Foreign>> {
    use rusqlite::{Connection, OpenFlags};
    anyhow::ensure!(db.is_file(), "no app.db");
    let c = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    let mut q = c.prepare(
        "SELECT path, name, icon_path, game_version, mod_loader, mod_loader_version, last_played,
                submitted_time_played + recent_time_played
         FROM profiles WHERE install_stage = 'installed'",
    )?;
    let rows = q.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, Option<i64>>(6)?,
            r.get::<_, Option<i64>>(7)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows.flatten() {
        let (path, name, icon, mc, loader, lv, last, played) = row;
        let game_dir = profiles.join(&path);
        if !game_dir.is_dir() {
            continue;
        }
        out.push(Foreign {
            name,
            game_version: mc,
            loader: loader_string(&loader, lv.as_deref().unwrap_or("")),
            icon: icon.map(PathBuf::from).filter(|p| p.is_file()),
            last_played: last.filter(|t| *t > 0),
            play_seconds: played.unwrap_or(0).max(0) as u64,
            meta_file: db.to_path_buf(),
            cf_pack: None,
            kind: None,
            game_dir,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopper-launchers-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn loader_strings() {
        assert_eq!(loader_string("NeoForge", "21.1.256"), "neoforge-21.1.256");
        assert_eq!(loader_string("forge", "47.4.10"), "forge-47.4.10");
        assert_eq!(loader_string("Fabric", "0.16.5"), "fabric-0.16.5");
        assert_eq!(loader_string("vanilla", ""), "");
    }

    #[test]
    fn reads_prism() {
        let root = tmp("prism");
        let inst = root.join("instances").join("ATM10");
        fs::create_dir_all(inst.join("minecraft").join("mods")).unwrap();
        fs::create_dir_all(root.join("icons")).unwrap();
        fs::write(root.join("icons").join("atm.png"), "x").unwrap();
        fs::write(
            inst.join("instance.cfg"),
            "[General]\nname=All the Mods 10\niconKey=atm\nlastLaunchTime=1760000000000\ntotalTimePlayed=7200\nManagedPackType=flame\nManagedPackID=925200\nManagedPackVersionID=6000000\n",
        )
        .unwrap();
        fs::write(
            inst.join("mmc-pack.json"),
            r#"{"components":[{"uid":"net.minecraft","version":"1.21.1"},{"uid":"net.neoforged","version":"21.1.219"}]}"#,
        )
        .unwrap();
        let v = list(Kind::Prism, &root);
        assert_eq!(v.len(), 1);
        let f = &v[0];
        assert_eq!(f.name, "All the Mods 10");
        assert_eq!(f.game_dir, inst.join("minecraft"));
        assert_eq!((f.game_version.as_str(), f.loader.as_str()), ("1.21.1", "neoforge-21.1.219"));
        assert_eq!(f.icon, Some(root.join("icons").join("atm.png")));
        assert_eq!(f.last_played, Some(1_760_000_000));
        assert_eq!(f.play_seconds, 7200);
        assert_eq!(f.cf_pack, Some((925200, 6000000)));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn reads_atlauncher_gdlauncher_ftb() {
        let root = tmp("others");
        let at = root.join("at").join("instances").join("Pack");
        fs::create_dir_all(&at).unwrap();
        fs::write(
            at.join("instance.json"),
            r#"{"id":"1.20.1","launcher":{"name":"Craft to Exile 2","loaderVersion":{"type":"Forge","version":"47.4.10"},"curseForgeProject":{"id":1},"curseForgeFile":{"id":2}}}"#,
        )
        .unwrap();
        let v = list(Kind::ATLauncher, &root.join("at"));
        assert_eq!(v[0].name, "Craft to Exile 2");
        assert_eq!(v[0].loader, "forge-47.4.10");
        assert_eq!(v[0].cf_pack, Some((1, 2)));

        let gd = root.join("gd").join("instances").join("x");
        fs::create_dir_all(gd.join("instance")).unwrap();
        fs::write(
            gd.join("instance.json"),
            r#"{"name":"Fabulously","game_configuration":{"version":{"Standard":{"release":"1.21.1","modloaders":[{"type_":"fabric","version":"0.16.5"}]}}}}"#,
        )
        .unwrap();
        let v = list(Kind::GDLauncher, &root.join("gd"));
        assert_eq!(v[0].game_dir, gd.join("instance"));
        assert_eq!(v[0].loader, "fabric-0.16.5");

        let ftb = root.join("ftb").join("instances").join("uuid");
        fs::create_dir_all(&ftb).unwrap();
        fs::write(
            ftb.join("instance.json"),
            r#"{"name":"FTB StoneBlock 4","mcVersion":"1.21.1","modLoader":"neoforge-21.1.200","lastPlayed":1760000000000,"totalPlayTime":3600000}"#,
        )
        .unwrap();
        let v = list(Kind::FtbApp, &root.join("ftb"));
        assert_eq!(v[0].loader, "neoforge-21.1.200");
        assert_eq!(v[0].play_seconds, 3600);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn manages_in_place() {
        let root = tmp("inplace");
        let inst = root.join("instances").join("Pack");
        let game = inst.join("minecraft");
        fs::create_dir_all(game.join("mods")).unwrap();
        fs::write(inst.join("instance.cfg"), "name=My Pack
").unwrap();
        fs::write(
            inst.join("mmc-pack.json"),
            r#"{"components":[{"uid":"net.minecraft","version":"1.20.1"},{"uid":"net.minecraftforge","version":"47.4.10"}]}"#,
        )
        .unwrap();
        let dirs = [crate::settings::LauncherDir { kind: "prism".into(), path: root.to_string_lossy().into() }];
        let v = instances(&dirs);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].root, game);
        assert_eq!(v[0].source, "prism");
        assert_eq!(v[0].loader, "forge-47.4.10");
        // Hopper's file lives in the game folder; Prism's are untouched.
        assert!(game.join(crate::instances::HOPPER_FILE).exists());
        assert_eq!(fs::read_to_string(inst.join("instance.cfg")).unwrap(), "name=My Pack
");
        // Hopper-only settings survive Prism renaming it.
        let mut own = v[0].clone();
        own.settings.memory_mb = 6144;
        own.save().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        fs::write(inst.join("instance.cfg"), "name=Renamed
").unwrap();
        let v = instances(&dirs);
        assert_eq!(v[0].name, "Renamed");
        assert_eq!(v[0].settings.memory_mb, 6144);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn reads_modrinth_db() {
        let root = tmp("modrinth");
        fs::create_dir_all(root.join("profiles").join("Cobblemon")).unwrap();
        let c = rusqlite::Connection::open(root.join("app.db")).unwrap();
        c.execute_batch(
            "CREATE TABLE profiles (path TEXT, install_stage TEXT, name TEXT, icon_path TEXT, game_version TEXT,
               mod_loader TEXT, mod_loader_version TEXT, last_played INTEGER,
               submitted_time_played INTEGER, recent_time_played INTEGER);
             INSERT INTO profiles VALUES ('Cobblemon','installed','Cobblemon Official','', '1.21.1','fabric','0.16.9',1760000000,100,20);
             INSERT INTO profiles VALUES ('Gone','installed','Gone','', '1.21.1','fabric','0.16.9',NULL,0,0);",
        )
        .unwrap();
        drop(c);
        let v = list(Kind::Modrinth, &root);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "Cobblemon Official");
        assert_eq!(v[0].loader, "fabric-0.16.9");
        assert_eq!(v[0].play_seconds, 120);
        fs::remove_dir_all(&root).unwrap();
    }
}
