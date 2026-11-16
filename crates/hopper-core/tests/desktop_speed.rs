use hopper_core::{deps, imgcache, install, instances::{self, Addon, Instance, PackRef}, sources::looks};
use std::{fs, io::Write, path::Path, process::Command, time::Instant};

const JARS: usize = 1_000;

fn timed<T>(name: &str, budget_ms: u128, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let value = f();
    let elapsed = start.elapsed();
    println!("desktop-speed {name}: {:.2} ms (budget {budget_ms} ms)", elapsed.as_secs_f64() * 1000.0);
    assert!(elapsed.as_millis() < budget_ms, "{name} exceeded {budget_ms} ms: {elapsed:?}");
    value
}

fn fixture(root: &Path) {
    let dir = root.join("instances/Big Pack");
    fs::create_dir_all(dir.join("mods")).unwrap();
    let mut inst = Instance {
        root: dir.clone(), name: "Big Pack".into(),
        imported_from_cf: i64::MAX, format: instances::FORMAT,
        pack: Some(PackRef { project_id: 1, file_id: 2, ..Default::default() }),
        ..Default::default()
    };
    for i in 0..JARS {
        let name = format!("fixture-{i:04}.jar");
        let mut zip = zip::ZipWriter::new(fs::File::create(dir.join("mods").join(&name)).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        if i % 2 == 0 {
            zip.start_file("META-INF/mods.toml", options).unwrap();
            write!(zip, "modLoader=\"javafml\"\nloaderVersion=\"[1,)\"\n[[mods]]\nmodId=\"fixture{i}\"\nversion=\"1.0\"\ndisplayName=\"Fixture {i}\"\nauthors=\"Fixture\"\n").unwrap();
        } else {
            zip.start_file("fabric.mod.json", options).unwrap();
            write!(zip, "{}", serde_json::json!({"schemaVersion":1,"id":format!("fixture{i}"),"version":"1.0","name":format!("Fixture {i}"),"authors":["Fixture"]})).unwrap();
        }
        zip.finish().unwrap();
        if i < 960 {
            inst.addons.push(Addon { file_name: name, name: format!("Fixture {i}"), project_id: i as u64 + 10,
                class_id: 6, enabled: true, from_pack: i != 0, ..Default::default() });
        }
    }
    fs::write(dir.join("minecraftinstance.json"), r#"{"modpackOverrides":["mods/fixture-0000.jar"]}"#).unwrap();
    inst.save().unwrap();
}

// Separate processes discard LazyLocks, exercising on-disk caches like a desktop restart.
#[test]
#[ignore = "wall-clock budget; run separately with --ignored --nocapture"]
fn desktop_speed() {
    if let Ok(phase) = std::env::var("HOPPER_SPEED_PHASE") {
        let root = std::path::PathBuf::from(std::env::var_os("HOPPER_DATA_DIR").unwrap());
        let instances = timed(&format!("startup/{phase}"), if phase == "cold" { 2_000 } else { 500 }, || instances::discover(&root.join("instances")));
        assert_eq!(instances.len(), 1);
        let inst = &instances[0];
        assert!(inst.unreadable.is_none());
        assert_eq!(inst.addons.len(), 960);
        assert_eq!(inst.shipped_checked, 2);
        timed(&format!("mods/{phase}"), 500, || {
            let loose = install::loose_jars(inst);
            assert_eq!(loose.len(), 40);
            for name in loose {
                assert_eq!(looks::loose(&inst.mods_dir().join(name)).version, "1.0");
            }
            looks::save();
            // The override claim reads these on first start; explicitly verify persisted ids too.
            for i in 0..JARS {
                assert_eq!(deps::own_ids(&inst.mods_dir().join(format!("fixture-{i:04}.jar"))).0, vec![format!("fixture{i}")]);
            }
            deps::save_own_ids();
        });
        let urls: Vec<String> = (0..JARS).map(|i| format!("https://fixture.invalid/{i}.png")).collect();
        timed(&format!("icons/{phase}"), 300, || {
            let found = imgcache::cached_many(&urls);
            assert_eq!(found.len(), JARS);
            assert!(found.iter().all(|(_, p)| p.is_some()));
        });
        return;
    }
    let root = std::env::temp_dir().join(format!("hopper-desktop-speed-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
    let _cleanup = Cleanup(root.clone());
    fixture(&root);
    // Only children initialize Hopper paths, always inside this throwaway root.
    let images = root.join("cache/images");
    fs::create_dir_all(&images).unwrap();
    for i in 0..JARS {
        let url = format!("https://fixture.invalid/{i}.png");
        let hash = sha1_smol::Sha1::from(url.as_str()).digest().to_string();
        fs::write(images.join(format!("{hash}.png")), b"cached probe only").unwrap();
    }
    for phase in ["cold", "warm", "after-update"] {
        if phase == "after-update" {
            // Older app version, same cache format; force the pack check again.
            let path = root.join("instances/Big Pack/hopper.json");
            let mut json: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            json["shipped_checked"] = 0.into();
            json["addons"][0]["from_pack"] = false.into();
            fs::write(path, serde_json::to_vec(&json).unwrap()).unwrap();
            let path = root.join("cache/jar-ids.json");
            let mut json: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            json["version"] = "older-app-version".into();
            fs::write(path, serde_json::to_vec(&json).unwrap()).unwrap();
        }
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "desktop_speed", "--ignored", "--nocapture"])
            .env("HOPPER_SPEED_PHASE", phase).env("HOPPER_DATA_DIR", &root)
            .env("HOPPER_LOOPBACK", "1").status().unwrap();
        assert!(status.success(), "{phase} subprocess failed");
    }
}
