//! Tests from recorded answers (GitHub, Maven, Jenkins); no network.

use super::*;
use crate::cf::Loader;

const MODMENU: &str = include_str!("fixtures/github-modmenu.json");
const AE2: &str = include_str!("fixtures/github-ae2.json");
const MAVEN: &str = include_str!("fixtures/maven-modmenu.xml");
const JENKINS: &str = include_str!("fixtures/jenkins-build.json");

fn judged(mut all: Vec<Candidate>, mc: &str, loader: Loader, pre: bool) -> Vec<Candidate> {
    all.retain(|c| pre || !c.prerelease);
    for c in &mut all {
        c.fit = fit(&c.file_name, &c.version, mc, loader);
    }
    all
}

#[test]
fn links() {
    let s = parse_link("https://github.com/TerraformersMC/ModMenu").unwrap();
    assert_eq!((s.kind, s.url.as_str()), (Kind::GitHub, "TerraformersMC/ModMenu"));
    let s = parse_link("https://github.com/TerraformersMC/ModMenu/releases/tag/v20.0.3").unwrap();
    assert_eq!(s.url, "TerraformersMC/ModMenu");
    assert_eq!(github::tag_in_link("https://github.com/a/b/releases/tag/v20.0.3").as_deref(), Some("v20.0.3"));
    let dl = "https://github.com/AppliedEnergistics/Applied-Energistics-2/releases/download/neoforge%2Fv19.2.18/appliedenergistics2-19.2.18.jar";
    assert_eq!(github::tag_in_link(dl).as_deref(), Some("neoforge/v19.2.18"));
    assert_eq!(github::asset_in_link(dl).as_deref(), Some("appliedenergistics2-19.2.18.jar"));
    assert_eq!(parse_link("github.com/a/b.git").unwrap().url, "a/b");

    let m = parse_link("https://maven.terraformersmc.com/releases/com/terraformersmc/modmenu/maven-metadata.xml").unwrap();
    assert_eq!((m.kind, m.url.as_str()), (Kind::Maven, "https://maven.terraformersmc.com/releases/com/terraformersmc/modmenu"));
    let m = parse_link("https://maven.example.com/releases/com/x/mymod/1.2.0/mymod-1.2.0.jar").unwrap();
    assert_eq!((m.kind, m.url.as_str()), (Kind::Maven, "https://maven.example.com/releases/com/x/mymod"));

    let j = parse_link("https://ci.example.org/job/spark/512/artifact/x.jar").unwrap();
    assert_eq!((j.kind, j.url.as_str()), (Kind::Jenkins, "https://ci.example.org/job/spark"));

    let u = parse_link("https://cdn.example.com/files/cool-mod-2.0.jar?dl=1").unwrap();
    assert_eq!(u.kind, Kind::Url);
    assert_eq!(url::name_in(&u.url), "cool-mod-2.0.jar");

    assert!(parse_link("https://modrinth.com/mod/sodium").is_err());
    assert!(parse_link("not a link").is_err());
}

#[test]
fn github_releases_parse() {
    let r = github::parse_releases(MODMENU).unwrap();
    let all = github::from_releases(&r);
    assert_eq!(all.len(), 30);
    let first = &all[0];
    assert_eq!(first.file_name, "modmenu-22.0.0-alpha.3.jar");
    assert!(first.prerelease);
    assert_eq!(first.digest.len(), 64, "sha256 from the asset's digest");
    assert!(first.url.starts_with("https://github.com/TerraformersMC/ModMenu/releases/download/"));
    // Stable only: the first is v20.0.3.
    let stable = judged(all, "1.21.1", Loader::Fabric, false);
    assert_eq!(stable[0].version, "v20.0.3");
    // Its name places it nowhere: the jar has to say.
    assert_eq!(stable[0].fit, Fit::Unknown);
}

#[test]
fn ae2_picks_by_loader_tag_and_skips_extras() {
    let all = github::from_releases(&github::parse_releases(AE2).unwrap());
    // guide-assets.zip isn't a candidate at all.
    assert!(all.iter().all(|c| c.file_name.ends_with(".jar")));
    let all = judged(all, "1.21.1", Loader::NeoForge, false);
    let fits: Vec<&str> = all.iter().filter(|c| c.fit.usable()).map(|c| c.file_name.as_str()).collect();
    // Forge/Fabric jars, -api/-javadoc and the 26.x builds are out.
    assert_eq!(fits, ["appliedenergistics2-19.2.18.jar"]);
    let api = all.iter().find(|c| c.file_name == "appliedenergistics2-19.2.18-api.jar").unwrap();
    assert_eq!(api.fit, Fit::Extra("API only".into()));
    let fabric = all.iter().find(|c| c.file_name == "appliedenergistics2-fabric-15.4.12.jar").unwrap();
    assert_eq!(fabric.fit, Fit::Loader("Fabric".into()));
    let r = pick(parse_link("https://github.com/AppliedEnergistics/Applied-Energistics-2").unwrap(), all, None);
    let c = &r.candidates[r.pick.unwrap()];
    assert_eq!(c.version, "neoforge/v19.2.18");
    assert!(!r.ambiguous);
    assert_eq!(r.source.pattern, "appliedenergistics#-#.jar");
}

#[test]
fn year_versions_count_as_minecraft() {
    assert_eq!(mc_versions("appliedenergistics2-26.3.1-alpha.jar"), ["26.3.1"]);
    assert_eq!(mc_versions("sodium-fabric-0.6.13+mc1.21.1.jar"), ["1.21.1"]);
    assert_eq!(mc_versions("mymod-1.2.0.jar"), Vec::<String>::new());
    assert_eq!(mc_versions("thing-1.20.x-3.jar"), ["1.20.x"]);
    assert_eq!(fit("x-26.3.1.jar", "", "26.3.1", Loader::Fabric), Fit::Good);
    assert_eq!(fit("x-26.3.1.jar", "", "1.21.1", Loader::Fabric), Fit::Minecraft("26.3.1".into()));
}

#[test]
fn fit_by_name() {
    let f = |n: &str| fit(n, "", "1.21.1", Loader::NeoForge);
    assert_eq!(f("sodium-neoforge-0.6.13+mc1.21.1.jar"), Fit::Good);
    assert_eq!(f("sodium-fabric-0.6.13+mc1.21.1.jar"), Fit::Loader("Fabric".into()));
    assert_eq!(f("mod-neoforge-1.20.1-2.0.jar"), Fit::Minecraft("1.20.1".into()));
    assert_eq!(f("mod-1.21-2.0.jar"), Fit::Good, "1.21 covers 1.21.1");
    assert_eq!(f("mod-2.0-sources.jar"), Fit::Extra("Source code".into()));
    assert_eq!(f("mod-2.0-dev.jar"), Fit::Extra("Dev build".into()));
    assert_eq!(f("mod-2.0.zip"), Fit::Extra("Not a jar".into()));
    assert_eq!(f("forgeconfigapi-2.0.jar"), Fit::Unknown, "a word inside another isn't a loader");
    // Quilt takes Fabric jars.
    assert_eq!(fit("x-fabric-1.21.1.jar", "", "1.21.1", Loader::Quilt), Fit::Good);
}

#[test]
fn ambiguous_when_two_fit_the_same() {
    let c = |n: &str| Candidate { file_name: n.into(), version: "v1".into(), ..Default::default() };
    let all = judged(vec![c("a-1.0.jar"), c("a-extra-1.0.jar")], "1.21.1", Loader::Fabric, false);
    let r = pick(ModSource::default(), all, None);
    assert!(r.ambiguous);
    let all = judged(vec![c("a-fabric-1.21.1-1.0.jar"), c("a-forge-1.21.1-1.0.jar")], "1.21.1", Loader::Fabric, false);
    let r = pick(ModSource::default(), all, None);
    assert!(!r.ambiguous);
    assert_eq!(r.candidates[r.pick.unwrap()].file_name, "a-fabric-1.21.1-1.0.jar");
}

#[test]
fn nothing_fits_lists_newest_for_picker() {
    let c = |n: &str, v: &str| Candidate { file_name: n.into(), version: v.into(), ..Default::default() };
    let all = judged(vec![c("a-fabric-1.21.1.jar", "v2"), c("a-fabric-1.20.1.jar", "v1")], "1.21.1", Loader::NeoForge, false);
    let r = pick(ModSource::default(), all, None);
    assert_eq!(r.pick, None);
    assert_eq!(r.candidates.len(), 1);
}

#[test]
fn patterns() {
    assert_eq!(pattern_of("sodium-neoforge-0.6.13+mc1.21.1.jar"), "sodium-neoforge-#+mc#.jar");
    assert_eq!(pattern_of("modmenu-20.0.3.jar"), "modmenu-#.jar");
    // A pre-release has the same shape as the release.
    assert_eq!(pattern_of("modmenu-22.0.0-alpha.3.jar"), "modmenu-#.jar");
    assert_eq!(pattern_of("appliedenergistics2-26.1.13-beta.jar"), "appliedenergistics#-#.jar");
    assert!(matches_pattern("modmenu-#.jar", "ModMenu-21.0.0.jar"));
    assert!(!matches_pattern("mod-fabric-#.jar", "mod-neoforge-2.0.jar"));
    assert!(matches_pattern("", "anything.jar"));
}

#[test]
fn maven_metadata() {
    let base = "https://maven.terraformersmc.com/releases/com/terraformersmc/modmenu";
    let all = maven::from_metadata(base, MAVEN);
    assert_eq!(all[0].version, "22.0.0-alpha.3", "newest first, whatever the file's order");
    assert!(all[0].prerelease);
    assert_eq!(all[0].date, "2026-10-27T20:20:06Z");
    assert_eq!(all[0].url, format!("{base}/22.0.0-alpha.3/modmenu-22.0.0-alpha.3.jar"));
    let stable: Vec<_> = all.iter().filter(|c| !c.prerelease).collect();
    assert_eq!(stable[0].version, "21.0.0");
    assert!(all.len() <= 25);
}

#[test]
fn jenkins_build() {
    let all = jenkins::from_build(JENKINS).unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].version, "#512");
    assert_eq!(all[0].url, "https://ci.example.org/job/spark/512/artifact/spark-fabric/build/libs/spark-1.10.142-fabric.jar");
    assert!(all[0].notes.contains("Fix sampler"));
    let all = judged(all, "1.21.1", Loader::NeoForge, false);
    let r = pick(jenkins::parse_link("https://ci.example.org/job/spark"), all, None);
    assert_eq!(r.candidates[r.pick.unwrap()].file_name, "spark-1.10.142-neoforge.jar");
}

#[test]
fn content_disposition() {
    assert_eq!(http::filename_in("attachment; filename=\"cool-2.0.jar\"").as_deref(), Some("cool-2.0.jar"));
    assert_eq!(http::filename_in("attachment; filename=../../evil.jar").as_deref(), Some("evil.jar"));
    assert_eq!(http::filename_in("inline"), None);
}

fn src(seen: &str, sha: &str) -> ModSource {
    ModSource {
        kind: Kind::GitHub,
        url: "a/b".into(),
        pattern: "mod-#.jar".into(),
        seen: seen.into(),
        sha256: sha.into(),
        ..Default::default()
    }
}

fn addon(file: &str, s: ModSource) -> Addon {
    Addon { project_id: src_id("x"), file_name: file.into(), name: "Mod".into(), source: Some(s), ..Default::default() }
}

#[test]
fn offers_newer_release_and_flags_changed_file() {
    let inst = Instance { game_version: "1.21.1".into(), loader: "fabric-0.16.0".into(), ..Default::default() };
    let cand = |v: &str, n: &str, d: &str| Candidate {
        version: v.into(),
        file_name: n.into(),
        digest: d.into(),
        fit: Fit::Good,
        ..Default::default()
    };
    let a = addon("mod-1.0.jar", src("v1.0", "aaa"));
    let s = a.source.clone().unwrap();
    // Same release, same file: nothing.
    let all = vec![cand("v1.0", "mod-1.0.jar", "aaa")];
    assert!(offer(&inst, &a, &s, &all, Loader::Fabric).is_none());
    // A newer release.
    let all = vec![cand("v1.1", "mod-1.1.jar", "bbb"), cand("v1.0", "mod-1.0.jar", "aaa")];
    let u = offer(&inst, &a, &s, &all, Loader::Fabric).unwrap();
    assert_eq!(u.file.file_name, "mod-1.1.jar");
    let sf = u.file.source.as_ref().unwrap();
    assert!(!sf.changed);
    assert_eq!(sf.source.seen, "v1.1");
    assert_eq!(sf.want_loader, "fabric");
    // Same release, same name, other contents: flagged.
    let all = vec![cand("v1.0", "mod-1.0.jar", "ccc")];
    let u = offer(&inst, &a, &s, &all, Loader::Fabric).unwrap();
    assert!(u.file.source.as_ref().unwrap().changed);
    // Another shape (the Fabric jar of a two-loader repo) isn't offered.
    let all = vec![cand("v2", "mod-forge-2.0.jar", "ddd")];
    assert!(offer(&inst, &a, &s, &all, Loader::Fabric).is_none());
}

#[test]
fn ids_stay_apart() {
    let id = src_id("github:a/b|mod-#.jar");
    assert!(is_src(id));
    assert!(!crate::modrinth::is_mr(id));
    assert!(!is_src(crate::modrinth::num_id("AANobbMI")));
    assert!(!is_src(238222));
}

fn jar_with(files: &[(&str, &str)]) -> std::io::Cursor<Vec<u8>> {
    use std::io::Write;
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        for (name, body) in files {
            z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(body.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    }
    buf.set_position(0);
    buf
}

#[test]
fn jar_metadata() {
    let fabric = jar_with(&[(
        "fabric.mod.json",
        r#"{"id":"modmenu","name":"Mod Menu","depends":{"minecraft":">=1.21.9 <1.21.11","fabricloader":">=0.16"}}"#,
    )]);
    let info = jar::read_from(fabric).unwrap();
    assert_eq!(info.mod_id, "modmenu");
    assert_eq!(info.loaders, ["fabric"]);
    assert_eq!(jar::misfit(&info, "1.21.10", "fabric"), None);
    assert!(jar::misfit(&info, "1.21.1", "fabric").unwrap().contains("not 1.21.1"));
    assert!(jar::misfit(&info, "1.21.10", "neoforge").unwrap().contains("Fabric mod"));
    assert_eq!(jar::misfit(&info, "1.21.10", "quilt"), None);

    let neo = jar_with(&[(
        "META-INF/neoforge.mods.toml",
        r#"
modLoader="javafml"
loaderVersion="[4,)"
[[mods]]
modId="ae2"
displayName="Applied Energistics 2"
[[dependencies.ae2]]
modId="neoforge"
versionRange="[21.1.1,)"
[[dependencies.ae2]]
modId="minecraft"
versionRange="[1.21.1,1.21.2)"
"#,
    )]);
    let info = jar::read_from(neo).unwrap();
    assert_eq!((info.mod_id.as_str(), info.minecraft.as_str()), ("ae2", "[1.21.1,1.21.2)"));
    assert_eq!(jar::misfit(&info, "1.21.1", "neoforge"), None);
    assert!(jar::misfit(&info, "1.21.4", "neoforge").is_some());
    assert!(jar::misfit(&info, "1.21.1", "fabric").is_some());
}

#[test]
fn version_ranges() {
    use jar::accepts;
    assert_eq!(accepts("[1.21,1.22)", "1.21.1"), Some(true));
    assert_eq!(accepts("[1.21,1.22)", "1.22"), Some(false));
    assert_eq!(accepts("[1.20.1]", "1.20.1"), Some(true));
    assert_eq!(accepts("[1.20,1.20.1],[1.21,)", "1.21.4"), Some(true));
    assert_eq!(accepts("(,1.20]", "1.20.1"), Some(false));
    assert_eq!(accepts("~1.21.1", "1.21.3"), Some(true));
    assert_eq!(accepts("~1.21.1", "1.22"), Some(false));
    assert_eq!(accepts("1.21.x", "1.21.4"), Some(true));
    assert_eq!(accepts("1.21.x", "1.20.1"), Some(false));
    assert_eq!(accepts(">=1.21 <1.21.2", "1.21.1"), Some(true));
    assert_eq!(accepts(">=1.21 <1.21.2", "1.21.2"), Some(false));
    assert_eq!(accepts("1.20.1 || 1.21.1", "1.21.1"), Some(true));
    assert_eq!(accepts("*", "26.1"), Some(true));
    assert_eq!(accepts(">=1.21-alpha.24.10.a", "1.21.1"), Some(true));
    assert_eq!(accepts("something odd", "1.21.1"), None);
    assert_eq!(jar::range_label("[1.21.9,)"), "MC 1.21.9");
    assert_eq!(jar::range_label(">=1.21.9 <1.21.11"), "MC 1.21.9");
}

#[test]
fn settle_stops_at_a_name_that_fits() {
    // No probing needed (and no network) when a name already fits.
    let mut all = vec![
        Candidate { file_name: "a-fabric-1.20.1.jar".into(), fit: Fit::Minecraft("1.20.1".into()), ..Default::default() },
        Candidate { file_name: "a-fabric-1.21.1.jar".into(), fit: Fit::Good, ..Default::default() },
    ];
    settle(&mut all, "", "1.21.1", Loader::Fabric);
    assert_eq!(all[1].fit, Fit::Good);
}

#[test]
fn jar_links_suggest_a_repo() {
    let neo = jar_with(&[(
        "META-INF/neoforge.mods.toml",
        r#"
modLoader="javafml"
loaderVersion="[4,)"
issueTrackerURL="https://github.com/Owner/cool-mod/issues"
[[mods]]
modId="coolmod"
version="1.4.2"
displayName="Cool Mod"
displayURL="https://coolmod.example.com"
"#,
    )]);
    let info = jar::read_from(neo).unwrap();
    assert_eq!(info.version, "1.4.2");
    assert_eq!(jar::suggested_link(&info).as_deref(), Some("https://github.com/Owner/cool-mod"));

    let fabric = jar_with(&[(
        "fabric.mod.json",
        r#"{"id":"modmenu","version":"11.0.3","name":"Mod Menu","contact":{"homepage":"https://modrinth.com/mod/modmenu","sources":"https://github.com/TerraformersMC/ModMenu","issues":"https://github.com/TerraformersMC/ModMenu/issues"}}"#,
    )]);
    let info = jar::read_from(fabric).unwrap();
    assert_eq!(info.version, "11.0.3");
    assert_eq!(jar::suggested_link(&info).as_deref(), Some("https://github.com/TerraformersMC/ModMenu"));

    // A build placeholder isn't a version; no GitHub link, no suggestion.
    let bare = jar_with(&[(
        "META-INF/mods.toml",
        "modLoader=\"javafml\"
[[mods]]
modId=\"x\"
version=\"${file.jarVersion}\"
displayURL=\"https://example.com\"
",
    )]);
    let info = jar::read_from(bare).unwrap();
    assert_eq!(info.version, "");
    assert_eq!(jar::suggested_link(&info), None);

    assert_eq!(jar::github_repo("https://github.com/orgs/foo").as_deref(), None);
    assert_eq!(jar::github_repo("https://raw.githubusercontent.com/a/b/main/x.json").as_deref(), Some("a/b"));
}

#[test]
fn tag_versions_compare() {
    assert_eq!(tag_version("neoforge/v19.2.18"), "19.2.18");
    assert_eq!(tag_version("v1.4.2+neoforge-1.21.1"), "1.4.2");
    assert_eq!(tag_version("mc1.21.1-0.6.13"), "0.6.13");
    assert_eq!(tag_version("#512"), "512");
    assert!(newer("v11.0.5", "v11.0.3"));
    assert!(!newer("v11.0.3", "v11.0.3"));
    assert!(!newer("v11.0.1", "11.0.3"));
}

#[test]
fn adopt_matches_by_hash_then_version() {
    let all = github::from_releases(&github::parse_releases(MODMENU).unwrap());
    // Exact: the jar's sha256 is one asset's digest.
    let d = all.iter().find(|c| c.version == "v17.0.1").unwrap().digest.clone();
    let (m, p) = match_jar(&all, &d, "modmenu-17.0.1.jar", "17.0.1");
    assert_eq!(m, Matched::Exact { tag: "v17.0.1".into() });
    assert_eq!(p, "modmenu-#.jar");
    // A jar that's not byte for byte any asset, version 11.0.4: the
    // nearest release at or before it (v11.0.5 is later, so not it).
    let (m, _) = match_jar(&all, "00", "modmenu-11.0.4.jar", "11.0.4");
    assert_eq!(m, Matched::Baseline { version: "11.0.4".into(), tag: String::new() });
    let (m, _) = match_jar(&all, "00", "modmenu-12.0.3.jar", "12.0.3");
    assert_eq!(m, Matched::Baseline { version: "12.0.3".into(), tag: "v12.0.1".into() });
    // Renamed jar: the releases' shape is kept for updates.
    let (_, p) = match_jar(&all, "00", "ModMenu Fabric.jar", "20.0.3");
    assert_eq!(p, "modmenu-#.jar");
}

#[test]
fn adopted_mods_get_only_newer_releases() {
    let inst = Instance { game_version: "1.21.1".into(), loader: "fabric-0.16.0".into(), ..Default::default() };
    let cand = |v: &str, n: &str| Candidate { version: v.into(), file_name: n.into(), fit: Fit::Good, ..Default::default() };
    // Baseline from the jar: v1.4.2 (no such tag); the list's newest fit is older.
    let a = addon("mod-1.4.2.jar", src("v1.4.2", "aaa"));
    let s = a.source.clone().unwrap();
    assert!(offer(&inst, &a, &s, &[cand("v1.4.1", "mod-1.4.1.jar")], Loader::Fabric).is_none());
    let u = offer(&inst, &a, &s, &[cand("v1.5.0", "mod-1.5.0.jar")], Loader::Fabric).unwrap();
    assert_eq!(u.file.file_name, "mod-1.5.0.jar");
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("hopper-src-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("mods")).unwrap();
    d
}

fn pinned_instance(root: &std::path::Path) -> (Instance, String) {
    std::fs::write(root.join("mods/modmenu-11.0.3.jar"), b"jar bytes").unwrap();
    let sha = sha256_file(&root.join("mods/modmenu-11.0.3.jar")).unwrap();
    let s = ModSource {
        kind: Kind::GitHub,
        url: "TerraformersMC/ModMenu".into(),
        pattern: "modmenu-#.jar".into(),
        seen: "v11.0.3".into(),
        sha256: sha.clone(),
        publisher: "TerraformersMC/ModMenu".into(),
        prev_project: 77,
        ..Default::default()
    };
    let inst = Instance {
        name: "A".into(),
        root: root.to_path_buf(),
        game_version: "1.21.1".into(),
        loader: "fabric-0.16.9".into(),
        addons: vec![Addon {
            project_id: src_id("x"),
            file_name: "modmenu-11.0.3.jar".into(),
            name: "Mod Menu".into(),
            enabled: true,
            class_id: crate::cf::CLASS_MODS,
            source: Some(s),
            ..Default::default()
        }],
        ..Default::default()
    };
    inst.save().unwrap();
    (inst, sha)
}

#[test]
fn pack_export_import_round_trip() {
    let a = scratch("pack-a");
    let (inst, sha) = pinned_instance(&a);
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        carry::write_pack(&mut z, &inst).unwrap();
        z.finish().unwrap();
    }
    buf.set_position(0);
    let pins = carry::read_pack(buf);
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0].source.seen, "v11.0.3");
    assert_eq!(pins[0].source.sha256, sha);
    assert_eq!(pins[0].source.prev_project, 0, "undo details stay home");

    // The importing side: same jar → it follows the source; other bytes → it doesn't.
    let b = scratch("pack-b");
    std::fs::write(b.join("mods/modmenu-11.0.3.jar"), b"jar bytes").unwrap();
    let mut got = Instance { root: b.clone(), ..Default::default() };
    assert_eq!(carry::attach(&mut got, &pins), 1);
    let s = got.addons[0].source.as_ref().unwrap();
    assert_eq!((s.seen.as_str(), s.kind), ("v11.0.3", Kind::GitHub));
    assert!(is_src(got.addons[0].project_id));
    let c = scratch("pack-c");
    std::fs::write(c.join("mods/modmenu-11.0.3.jar"), b"other bytes").unwrap();
    let mut other = Instance { root: c.clone(), ..Default::default() };
    assert_eq!(carry::attach(&mut other, &pins), 0);
    for d in [a, b, c] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[test]
fn share_code_carries_pins() {
    let a = scratch("share");
    let (inst, _) = pinned_instance(&a);
    let jobs = crate::jobs::Jobs::new(crate::waker::noop());
    let made = crate::sharecode::make(&inst, &jobs.start("t")).unwrap();
    assert!(made.left_out.is_empty(), "a source-tracked mod isn't left out");
    let back = crate::sharecode::decode(&made.code).unwrap();
    assert_eq!(back, made.share);
    match &back.entries[0] {
        crate::sharecode::Entry::Source { pin, enabled } => {
            assert!(*enabled);
            assert_eq!(pin.file, "modmenu-11.0.3.jar");
            assert_eq!(pin.source.seen, "v11.0.3");
        }
        e => panic!("{e:?}"),
    }
    assert_eq!(crate::sharecode::sources_of(&back), [("GitHub".to_string(), "TerraformersMC/ModMenu".to_string())]);
    let _ = std::fs::remove_dir_all(a);
}

#[test]
fn sync_carries_pins_to_a_server_folder() {
    let inst_root = scratch("sync-inst");
    let (_, _) = pinned_instance(&inst_root);
    let server = scratch("sync-srv");
    // A server Hopper manages has its own hopper.json.
    Instance { root: server.clone(), ..Default::default() }.save().unwrap();
    std::fs::copy(inst_root.join("mods/modmenu-11.0.3.jar"), server.join("mods/modmenu-11.0.3.jar")).unwrap();
    let results = vec![crate::model::SyncResult {
        op: crate::model::SyncOp::Copy { src: inst_root.join("mods/modmenu-11.0.3.jar"), dst: server.join("mods/modmenu-11.0.3.jar") },
        ok: true,
        error: None,
        backup: None,
    }];
    let jars = carry::copied_jars(&results, &server);
    assert_eq!(jars, ["modmenu-11.0.3.jar"]);
    carry::carry(&inst_root, &server, &jars);
    let pins = carry::pins_in(&server);
    assert_eq!(pins.get("modmenu-11.0.3.jar").map(|s| s.seen.as_str()), Some("v11.0.3"));
    // And the peer manifest shows it for that jar.
    let shared = crate::peer::SharedInst { key: "srv".into(), root: server.clone(), ..Default::default() };
    let m = crate::peer::manifest(&shared, "mods", &Default::default());
    assert_eq!(m.files[0].source.as_ref().map(|s| s.kind), Some(Kind::GitHub));
    for d in [inst_root, server] {
        let _ = std::fs::remove_dir_all(d);
    }
}

// ---- looks: logos, authors, versions and sides from the jar ----

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(w, h, image::Rgba([200, 40, 40, 255]));
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

fn jar_bytes(files: &[(&str, &[u8])]) -> std::io::Cursor<Vec<u8>> {
    use std::io::Write;
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        for (name, body) in files {
            z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(body).unwrap();
        }
        z.finish().unwrap();
    }
    buf.set_position(0);
    buf
}

#[test]
fn neoforge_logo_authors_version_and_side() {
    let logo = png(16, 16);
    let toml = br#"modLoader="javafml"
license="MIT"
logoFile="wrong.png"
[[mods]]
modId="foxstweaks"
displayName="Fox's Tweaks"
version="${file.jarVersion}"
authors="watdafawx, Someone Else"
logoFile="foxstweaks.png"
displayTest="IGNORE_SERVER_VERSION"
"#;
    let manifest = b"Manifest-Version: 1.0\r\nImplementation-Version: 1.18.0\r\n";
    let j = looks::read_from(jar_bytes(&[
        ("META-INF/neoforge.mods.toml", toml),
        ("META-INF/MANIFEST.MF", manifest),
        ("foxstweaks.png", &logo),
    ]))
    .unwrap();
    // The mod's own logoFile wins over the file-level one.
    assert_eq!(j.logo.as_ref().map(|(p, b)| (p.as_str(), b.len())), Some(("foxstweaks.png", logo.len())));
    assert_eq!(j.authors, ["watdafawx", "Someone Else"]);
    // `${file.jarVersion}` comes from the manifest.
    assert_eq!(j.version, "1.18.0");
    assert_eq!(j.side, "client");

    // Only a file-level logo, under a folder, written with a leading slash.
    let toml = br#"logoFile="/assets/emi/logo.png"
[[mods]]
modId="emi"
version="0.1.0"
"#;
    let j = looks::read_from(jar_bytes(&[("META-INF/neoforge.mods.toml", toml), ("assets/emi/logo.png", &logo)])).unwrap();
    assert_eq!(j.logo.map(|(p, _)| p), Some("assets/emi/logo.png".to_string()));
    assert_eq!((j.version.as_str(), j.side.as_str()), ("0.1.0", ""));

    // A logo the jar names but doesn't have: none.
    let toml = b"[[mods]]\nmodId=\"x\"\nlogoFile=\"gone.png\"\n";
    assert!(looks::read_from(jar_bytes(&[("META-INF/mods.toml", toml)])).unwrap().logo.is_none());
}

#[test]
fn fabric_logo_authors_and_side() {
    let (small, big) = (png(64, 64), png(512, 512));
    let json = br#"{"schemaVersion":1,"id":"emi_slim","version":"0.1.0","environment":"client",
        "authors":["watdafawx",{"name":"Helper","contact":{}}],
        "icon":{"64":"assets/emi_slim/icon64.png","512":"assets/emi_slim/icon512.png"}}"#;
    let j = looks::read_from(jar_bytes(&[
        ("fabric.mod.json", json),
        ("assets/emi_slim/icon64.png", &small),
        ("assets/emi_slim/icon512.png", &big),
    ]))
    .unwrap();
    // The largest at most 128.
    assert_eq!(j.logo.map(|(p, _)| p), Some("assets/emi_slim/icon64.png".to_string()));
    assert_eq!(j.authors, ["watdafawx", "Helper"]);
    assert_eq!((j.version.as_str(), j.side.as_str()), ("0.1.0", "client"));

    // Only bigger sizes: the smallest of them. A plain string: that path.
    let only_big = serde_json::json!({"256": "a.png", "1024": "b.png"});
    assert_eq!(looks::fabric_icon(&only_big).as_deref(), Some("a.png"));
    assert_eq!(looks::fabric_icon(&serde_json::json!("icon.png")).as_deref(), Some("icon.png"));

    // "*" runs on both sides.
    let json = br#"{"id":"x","version":"1","environment":"*","icon":"x.png"}"#;
    assert_eq!(looks::read_from(jar_bytes(&[("fabric.mod.json", json)])).unwrap().side, "both");
}

#[test]
fn logo_is_saved_small() {
    let saved = looks::save_logo(&png(512, 256)).unwrap();
    let (w, h) = image::image_dimensions(&saved).unwrap();
    assert_eq!((w, h), (128, 64));
    assert!(looks::is_jar_logo(&saved.to_string_lossy()));
    let tiny = looks::save_logo(&png(16, 16)).unwrap();
    assert_eq!(image::image_dimensions(&tiny).unwrap(), (16, 16));
}

// ---- the peek: README links, and never CurseForge for a source mod ----

#[test]
fn readme_links_point_into_the_repo() {
    let md = "![logo](docs/logo.png) see [the guide](./GUIDE.md) and [site](https://x.dev)\n\
<p align=\"center\"><img src=\"art/banner.png\" width=\"300\"></p>\n\
![shot](https://github.com/watdafawx/emi-slim/blob/main/shots/a.png) [top](#usage) ![root](/icon.png)";
    let out = page::resolve_relative(md, "watdafawx/emi-slim", "main", "README.md");
    assert!(out.contains("![logo](https://raw.githubusercontent.com/watdafawx/emi-slim/main/docs/logo.png)"), "{out}");
    assert!(out.contains("[the guide](https://github.com/watdafawx/emi-slim/blob/main/GUIDE.md)"), "{out}");
    assert!(out.contains("[site](https://x.dev)"));
    assert!(out.contains("src=\"https://raw.githubusercontent.com/watdafawx/emi-slim/main/art/banner.png\""), "{out}");
    assert!(out.contains("![shot](https://raw.githubusercontent.com/watdafawx/emi-slim/main/shots/a.png)"), "{out}");
    assert!(out.contains("[top](#usage)"));
    assert!(out.contains("![root](https://raw.githubusercontent.com/watdafawx/emi-slim/main/icon.png)"), "{out}");
    // A README in a folder: relative to that folder.
    let out = page::resolve_relative("![a](a.png)", "o/r", "dev", "docs/README.md");
    assert_eq!(out, "![a](https://raw.githubusercontent.com/o/r/dev/docs/a.png)");
}

#[test]
fn source_mods_never_ask_curseforge() {
    // Unknown to this run: an answer from Hopper, not a CurseForge 404.
    let stray = src_id("github:nobody/never-remembered");
    let e = crate::source::description(stray).unwrap_err().to_string();
    assert!(e.contains("doesn't know where this mod comes from"), "{e}");
    // A file of a source mod without release notes: no CurseForge changelog call.
    let f = cf::File { id: 5, mod_id: stray, ..Default::default() };
    assert!(!crate::source::changelog(&f).unwrap().is_empty());

    // The peek's header facts come from the source.
    let src = ModSource { kind: Kind::Maven, url: "https://maven.example.net/releases/com/x/coolmod".into(), ..Default::default() };
    let id = src_id(&src.key());
    remember(id, &src, "Cool Mod");
    let o = page::origin(id).unwrap();
    assert_eq!((o.kind, o.name.as_str(), o.open_label()), (Kind::Maven, "coolmod", "Open link"));
    let gh = ModSource { kind: Kind::GitHub, url: "watdafawx/emi-slim".into(), publisher: "watdafawx/emi-slim".into(), ..Default::default() };
    let gid = src_id(&gh.key());
    remember(gid, &gh, "EMI Slim");
    let o = page::origin(gid).unwrap();
    assert_eq!((o.page.as_str(), o.open_label()), ("https://github.com/watdafawx/emi-slim", "Open on GitHub"));
    // No jar seen: the owner's picture, and the repo as the author.
    let m = get_mod(gid).unwrap();
    assert_eq!(m.icon(), Some("https://github.com/watdafawx.png?size=128"));
    assert_eq!(m.author(), "watdafawx/emi-slim");
    remember_looks(gid, r"C:\cache\logo.png", "watdafawx");
    let m = get_mod(gid).unwrap();
    assert_eq!((m.icon(), m.author()), (Some(r"C:\cache\logo.png"), "watdafawx"));
}
