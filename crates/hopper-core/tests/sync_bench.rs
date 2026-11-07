//! Timing of the Sync tab's comparison on generated fixtures (1,200 jars per
//! side). Ignored by default; run with
//! `cargo test -p hopper-core --test sync_bench --profile local -- --ignored --nocapture`.
//! Set `SYNC_BENCH_DIR` to a scratch folder (fixtures are kept there between runs).

use hopper_core::model::ModInfo;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn xorshift(s: &mut u64) -> u64 {
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    *s
}

/// A jar (stored zip) with a mods.toml and random filler of `size` bytes.
fn write_jar(path: &Path, id: &str, version: &str, size: usize, seed: u64, mtime_shift: u64) {
    let f = std::fs::File::create(path).unwrap();
    let mut z = zip::ZipWriter::new(f);
    let o = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    z.start_file("META-INF/neoforge.mods.toml", o).unwrap();
    write!(z, "modLoader=\"javafml\"\n[[mods]]\nmodId=\"{id}\"\nversion=\"{version}\"\ndisplayName=\"Mod {id}\"\n").unwrap();
    z.start_file("data.bin", o).unwrap();
    let mut s = seed | 1;
    let mut buf = vec![0u8; size];
    for c in buf.chunks_mut(8) {
        let r = xorshift(&mut s).to_le_bytes();
        c.copy_from_slice(&r[..c.len()]);
    }
    z.write_all(&buf).unwrap();
    z.finish().unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000 + mtime_shift)).unwrap();
}

/// `n` mods: client and server share most, the rest differ (150 in all:
/// 50 changed, 60 only in the pack, 40 only on the server).
pub fn make(root: &Path, n: usize) -> (PathBuf, PathBuf) {
    let (c, s) = (root.join("instance/mods"), root.join("server/mods"));
    if c.exists() && std::fs::read_dir(&c).map_or(0, |d| d.count()) >= n {
        return (c, s);
    }
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(&c).unwrap();
    std::fs::create_dir_all(&s).unwrap();
    let mut seed = 0x9E3779B97F4A7C15u64;
    for i in 0..n {
        let r = (xorshift(&mut seed) % 1000) as f64 / 1000.0;
        let size = 50_000 + (r * r * r * 3_000_000.0) as usize;
        let id = format!("mod{i:04}");
        let name = format!("{id}-1.{}.0.jar", i % 9);
        let only_pack = i % 20 == 3 && i / 20 < 60;
        let only_server = i % 30 == 7 && i / 30 < 40;
        let changed = i % 24 == 11 && i / 24 < 50;
        if !only_server {
            write_jar(&c.join(&name), &id, &format!("1.{}.0", i % 9), size, i as u64 + 1, 0);
        }
        if only_pack {
            continue;
        }
        if changed {
            let sname = format!("{id}-1.{}.1.jar", i % 9);
            write_jar(&s.join(sname), &id, &format!("1.{}.1", i % 9), size + 1000, i as u64 + 7777, 5);
        } else {
            write_jar(&s.join(&name), &id, &format!("1.{}.0", i % 9), size, i as u64 + 1, 5);
        }
    }
    (c, s)
}

fn t<R>(label: &str, f: impl FnOnce() -> R) -> R {
    let at = Instant::now();
    let r = f();
    eprintln!("BENCH {label:<40} {:>8.1} ms", at.elapsed().as_secs_f64() * 1000.0);
    r
}

fn drop_scan_cache(data: &Path) {
    let _ = std::fs::remove_file(data.join("cache/scan-cache.json"));
}

#[test]
#[ignore]
fn compare_stages() {
    let root = PathBuf::from(std::env::var("SYNC_BENCH_DIR").expect("set SYNC_BENCH_DIR"));
    let data = root.join("data");
    // Everything Hopper caches goes here, never in the real data folder.
    unsafe { std::env::set_var("HOPPER_DATA_DIR", &data) };
    let (client, server) = t("generate fixtures (only when missing)", || make(&root.join("fx"), 1200));
    let _ = std::fs::create_dir_all(data.join("cache"));
    eprintln!("cache dir: {:?}", hopper_core::paths::dirs().map(|d| d.cache_dir().to_path_buf()));

    // The cache is in memory after the first read, so only a fresh process is
    // truly cold: this test measures the first call in the process as cold.
    let cold_c = t("scan client  COLD", || hopper_core::rigsync::mods_with_off(&client).unwrap());
    let cold_s = t("scan server  COLD", || hopper_core::rigsync::mods_with_off(&server).unwrap());
    let warm_c = t("scan client  warm", || hopper_core::rigsync::mods_with_off(&client).unwrap());
    let warm_s = t("scan server  warm", || hopper_core::rigsync::mods_with_off(&server).unwrap());
    assert_eq!(cold_c.len(), warm_c.len());
    assert_eq!(cold_s.len(), warm_s.len());

    let d = t("diff_mods (both sides)", || hopper_core::rigsync::diff_mods(&warm_c, &warm_s));
    eprintln!("diff rows: {}", d.len());

    // Both sides at once, warm.
    t("client+server warm, sequential", || {
        let a = hopper_core::rigsync::mods_with_off(&client).unwrap();
        let b = hopper_core::rigsync::mods_with_off(&server).unwrap();
        (a, b)
    });
    t("client+server warm, concurrent", || {
        std::thread::scope(|s| {
            let a = s.spawn(|| hopper_core::rigsync::mods_with_off(&client).unwrap());
            let b = hopper_core::rigsync::mods_with_off(&server).unwrap();
            (a.join().unwrap(), b)
        })
    });

    // What `sides_of` does for jars whose side is unknown (every jar here):
    // read each file and sha1 it, one after the other.
    let paths: Vec<PathBuf> = warm_c.iter().map(|m: &ModInfo| client.join(&m.file_name)).collect();
    t("sha1 of all client jars, sequential", || {
        paths.iter().map(|p| hopper_core::modrinth::sha1(&std::fs::read(p).unwrap())).collect::<Vec<_>>()
    });
    let seq: Vec<String> = paths.iter().map(|p| hopper_core::modrinth::sha1(&std::fs::read(p).unwrap())).collect();
    // The same work as `sides_of` now does (configindex::par_map).
    let par = t("sha1 of all client jars, par_map", || {
        hopper_core::configindex::par_map(&paths, |p| hopper_core::modrinth::sha1(&std::fs::read(p).unwrap()))
    });
    assert_eq!(seq, par, "parallel hashes must equal sequential ones, in order");
    drop_scan_cache(&data);
}
