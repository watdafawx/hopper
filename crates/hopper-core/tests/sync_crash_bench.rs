//! Timing of the Sync tab's "crash and config reads" on a generated server:
//! ~1,150 jars, a few thousand config files, tens of crash reports and a big
//! latest.log. Ignored by default; run with
//! `SYNC_BENCH_DIR=<scratch> cargo test -p hopper-core --test sync_crash_bench --profile local -- --ignored --nocapture`.
//! The crash itself is a corpus case (its report, log and mod list), so the
//! detection has something to blame; the rest is padding.

use hopper_core::{crashdetect, crashscan};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("crash_corpus")
}

fn gunzip(p: &Path) -> String {
    let mut s = String::new();
    flate2::read::GzDecoder::new(fs::File::open(p).unwrap()).read_to_string(&mut s).unwrap();
    s
}

fn jar(dir: &Path, file: &str, ids: &[String], size: usize) {
    let mut z = zip::ZipWriter::new(fs::File::create(dir.join(file)).unwrap());
    let o = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    z.start_file("META-INF/neoforge.mods.toml", o).unwrap();
    let mut t = String::from("modLoader=\"javafml\"\nloaderVersion=\"[1,)\"\nlicense=\"x\"\n");
    for id in ids {
        t.push_str(&format!("[[mods]]\nmodId=\"{id}\"\nversion=\"1.0\"\ndisplayName=\"{id}\"\n"));
    }
    z.write_all(t.as_bytes()).unwrap();
    z.start_file("data.bin", o).unwrap();
    z.write_all(&vec![7u8; size]).unwrap();
    z.finish().unwrap();
}

fn t<R>(label: &str, f: impl FnOnce() -> R) -> R {
    let at = Instant::now();
    let r = f();
    eprintln!("BENCH {label:<46} {:>8.1} ms", at.elapsed().as_secs_f64() * 1000.0);
    r
}

/// A server folder and a paired instance folder.
fn make(base: &Path) -> (PathBuf, PathBuf) {
    let (server, inst) = (base.join("server"), base.join("instance"));
    if server.join("mods").is_dir() {
        return (server, inst);
    }
    let case = corpus().join("server-1103-1541");
    let text = gunzip(&corpus().join("_mods").join("7391c040e3.txt.gz"));
    let mut ids_all: Vec<String> = Vec::new();
    for root in [&server, &inst] {
        for d in ["mods", "logs", "crash-reports", "config"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
    }
    let mut n = 0;
    for l in text.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        let c: Vec<&str> = l.split('\t').collect();
        let ids: Vec<String> = c.get(1).copied().unwrap_or("").split(',').filter(|x| !x.is_empty()).map(str::to_string).collect();
        for root in [&server, &inst] {
            jar(&root.join("mods"), c[0], &ids, 40_000);
        }
        ids_all.extend(ids);
        n += 1;
    }
    // Pad to the size of the real pack.
    while n < 1157 {
        let id = format!("padmod{n:04}");
        for root in [&server, &inst] {
            jar(&root.join("mods"), &format!("{id}-1.0.jar"), std::slice::from_ref(&id), 40_000);
        }
        ids_all.push(id);
        n += 1;
    }
    // Configs: ~3,000 files, a few per mod, in both folders; every 7th differs.
    let mut k = 0usize;
    'outer: for id in ids_all.iter().cycle() {
        for (i, ext) in ["toml", "json", "json5"].iter().enumerate() {
            let rel = format!("config/{id}-{i}.{ext}");
            let body = format!("# {id}\nvalue = {k}\n{}", "key = true\n".repeat(400));
            fs::write(server.join(&rel), &body).unwrap();
            let mine = if k % 7 == 0 { body.replace("value", "other") } else { body.clone() };
            fs::write(inst.join(&rel), mine).unwrap();
            k += 1;
            if k >= 3000 {
                break 'outer;
            }
        }
    }
    // 40 old crash reports (300 KB each) and the one that matters.
    let report = fs::read_to_string(case.join("report.txt")).unwrap();
    let big = format!("{report}\n{}", "at some.Frame(Frame.java:1)\n".repeat(11_000));
    for d in 0..40 {
        fs::write(server.join("crash-reports").join(format!("crash-2026-10-{:02}_10.00.00-server.txt", d % 28 + 1)), &big).unwrap();
    }
    let name = "crash-2026-11-03_15.41.50-server.txt";
    fs::write(server.join("crash-reports").join(name), &report).unwrap();
    fs::write(server.join("logs/latest.log"), gunzip(&case.join("latest.log.gz"))).unwrap();
    let now = std::time::SystemTime::now();
    for f in [server.join("crash-reports").join(name), server.join("logs/latest.log")] {
        fs::File::options().write(true).open(&f).unwrap().set_modified(now).unwrap();
    }
    (server, inst)
}

#[test]
#[ignore]
fn crash_and_config_stage() {
    let base = PathBuf::from(std::env::var("SYNC_BENCH_DIR").expect("set SYNC_BENCH_DIR"));
    unsafe { std::env::set_var("HOPPER_DATA_DIR", base.join("data")) };
    let _ = fs::create_dir_all(base.join("data/cache"));
    let (server, inst) = t("generate fixtures (only when missing)", || make(&base.join("fx")));
    let crash = t("last_crash COLD (jar ids not cached)", || crashscan::last_crash(&server));
    let crash = crash.expect("fixture crash");
    eprintln!("suspects: {:?}", crash.suspects.iter().map(|s| (&s.mod_id, s.certainty)).collect::<Vec<_>>());
    let _ = t("last_crash warm", || crashscan::last_crash(&server));
    let n = std::cell::Cell::new(0usize);
    let diffs = t("config_differs (local reads)", || {
        crashdetect::config_differs(&inst, &crash, |rel| {
            n.set(n.get() + 1);
            fs::read_to_string(server.join(rel)).ok()
        })
    });
    eprintln!("config diffs: {}, server file reads: {}", diffs.len(), n.get());
}
