//! Times each stage of opening the Configs tab on a folder (an instance or
//! a server): `cargo run --release -p hopper-core --example config_bench -- <root> [query]`.
//! Run it twice to see a cold and a warm open (the second run reads the
//! on-disk caches the first one wrote).

use hopper_core::configedit::{self, Format};
use hopper_core::{configindex, configowner};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn ms(d: Duration) -> String {
    format!("{:>8.1} ms", d.as_secs_f64() * 1000.0)
}

fn jar_list(root: &std::path::Path) -> Vec<(String, PathBuf, String)> {
    std::fs::read_dir(root.join("mods"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().to_lowercase().ends_with(".jar"))
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path(), String::new()))
        .collect()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().expect("usage: config_bench <root> [query]"));
    let query = args.next().unwrap_or_else(|| "spawn rate".into());
    let total = Instant::now();

    // The jars, as hopperd lists them.
    let t = Instant::now();
    let jars = jar_list(&root);
    let mods = configowner::of_jars(jars);
    let owners = configowner::Owners::new(mods);
    println!("jars (ids, names)     {}  {} mods", ms(t.elapsed()), owners.mods().len());

    let t = Instant::now();
    let files = configindex::index(&root, &owners);
    println!("walk + owners         {}  {} files, {} matched", ms(t.elapsed()), files.len(), files.iter().filter(|f| f.owner.is_some()).count());

    // Reading and parsing on one thread, by format, for the breakdown.
    if std::env::var_os("BENCH_BREAKDOWN").is_some() {
        let t = Instant::now();
        let texts: Vec<(Format, String)> = files
            .iter()
            .filter_map(|f| Some((f.format?, String::from_utf8(std::fs::read(root.join(&f.rel)).ok()?).ok()?)))
            .collect();
        println!("read (1 thread)       {}", ms(t.elapsed()));
        let mut by: BTreeMap<String, (Duration, usize, usize)> = BTreeMap::new();
        for (f, text) in &texts {
            let t = Instant::now();
            let n = configedit::parse_as(*f, text).map_or(0, |s| s.iter().map(|s| s.entries.len()).sum());
            let e = by.entry(format!("{f:?}")).or_default();
            e.0 += t.elapsed();
            e.1 += 1;
            e.2 += n;
        }
        for (f, (d, n, s)) in by {
            println!("  parse {f:<14}{}  {n} files, {s} settings", ms(d));
        }
    }

    let t = Instant::now();
    let rels: Vec<String> = files.iter().map(|f| f.rel.clone()).collect();
    let sums = configindex::summaries(&root, &rels);
    let settings: usize = sums.iter().map(|(_, s)| s.settings).sum();
    let changed: usize = sums.iter().map(|(_, s)| s.changed).sum();
    println!("counts (summaries)    {}  {settings} settings, {changed} changed", ms(t.elapsed()));
    println!("OPEN TOTAL            {}", ms(total.elapsed()));

    // The same open again in this run (Hopper Server's index route, or a
    // second instance's tab), with everything in memory.
    let t = Instant::now();
    let again = configindex::index(&root, &configowner::Owners::new(configowner::of_jars(jar_list(&root))));
    let _ = configindex::summaries(&root, &rels);
    println!("open again (same run) {}  {} files", ms(t.elapsed()), again.len());

    let t = Instant::now();
    configindex::warm(&root, &rels);
    println!("warm search index     {}", ms(t.elapsed()));

    let mut dump = String::new();
    for (label, desc) in [("search", false), ("search again", false), ("search + desc", true)] {
        let t = Instant::now();
        let hits = configindex::search(&root, &rels, &query, desc, 20_000);
        println!("{label:<22}{}  {} hits for {query:?}", ms(t.elapsed()), hits.len());
        for h in &hits {
            dump += &format!("hit {desc} {} {} {}\n", h.rel, h.entry.path.join("."), h.entry.key);
        }
    }
    for q in ["max", "enable", "a", "energy per tick", "zzzznothing"] {
        for desc in [false, true] {
            let hits = configindex::search(&root, &rels, q, desc, 20_000);
            dump += &format!("query {q:?} {desc} -> {}\n", hits.len());
            for h in hits.iter().step_by(97) {
                dump += &format!("  {} {} {}\n", h.rel, h.entry.path.join("."), h.entry.key);
            }
        }
    }

    // Everything the Configs tab shows, to compare one build with another.
    if let Some(out) = std::env::var_os("BENCH_DUMP") {
        for m in owners.mods() {
            dump += &format!("mod {} {:?} {}\n", m.key, m.ids, m.name);
        }
        let by_rel: std::collections::HashMap<&str, &configindex::Summary> = sums.iter().map(|(r, s)| (r.as_str(), s)).collect();
        for f in &files {
            let s = by_rel[f.rel.as_str()];
            let err = s.error.clone().unwrap_or_default();
            dump += &format!("file {} {:?} {:?} {} {:?} changed={} settings={} {err}
", f.rel, f.format, f.side, f.size, f.owner, s.changed, s.settings);
        }
        std::fs::write(out, dump).unwrap();
    }
}
