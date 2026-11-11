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
    let rep = carry::carry(&inst_root, &server, &jars);
    assert_eq!((rep.sent, rep.attached, rep.note()), (1, 1, None));
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

/// TNP-LL8, 2026-11-11: jars synced off reached the server as bare entries
/// ("Add a source to get updates"). Their CurseForge record goes along now,
/// on or off, and takes the bare entry's place.
#[test]
fn sync_carries_curseforge_records_on_or_off() {
    let inst_root = scratch("ids-inst");
    let server = scratch("ids-srv");
    let cf = |file: &str, pid: u64, on: bool| Addon {
        project_id: pid,
        file_id: pid * 10,
        file_name: file.into(),
        name: format!("Mod {pid}"),
        icon: if pid == 1 { "C:/cache/logo.png".into() } else { "https://x/icon.png".into() },
        enabled: on,
        class_id: crate::cf::CLASS_MODS,
        from_pack: true,
        locked: true,
        ..Default::default()
    };
    std::fs::write(inst_root.join("mods/magic-1.0.jar.disabled"), b"magic").unwrap();
    std::fs::write(inst_root.join("mods/plus-3.0.jar"), b"plus").unwrap();
    std::fs::write(inst_root.join("mods/named-1.0.jar"), b"named").unwrap();
    std::fs::write(inst_root.join("mods/changed-1.0.jar"), b"pack copy").unwrap();
    Instance {
        root: inst_root.clone(),
        addons: vec![cf("magic-1.0.jar", 1, false), cf("plus-3.0.jar", 2, true), cf("named-1.0.jar", 3, true), cf("changed-1.0.jar", 4, true)],
        ..Default::default()
    }
    .save()
    .unwrap();
    // The server: magic arrived and was switched off through the panel (a
    // bare entry), plus arrived on, named already has its own record, and
    // changed holds other bytes than the pack's.
    std::fs::write(server.join("mods/magic-1.0.jar.disabled"), b"magic").unwrap();
    std::fs::write(server.join("mods/plus-3.0.jar"), b"plus").unwrap();
    std::fs::write(server.join("mods/named-1.0.jar"), b"named").unwrap();
    std::fs::write(server.join("mods/changed-1.0.jar"), b"other").unwrap();
    let bare = Addon { file_name: "magic-1.0.jar".into(), name: "magic-1.0.jar".into(), class_id: crate::cf::CLASS_MODS, ..Default::default() };
    assert!(crate::install::is_bare(&bare));
    let mut own = cf("named-1.0.jar", 99, true);
    own.name = "Server's own".into();
    Instance { root: server.clone(), addons: vec![bare, own], ..Default::default() }.save().unwrap();

    // A jar turned on there by a sync counts as well as one copied.
    let turned_on = vec![crate::model::SyncResult {
        op: crate::model::SyncOp::Enable { path: server.join("mods/magic-1.0.jar") },
        ok: true,
        error: None,
        backup: None,
    }];
    assert_eq!(carry::copied_jars(&turned_on, &server), ["magic-1.0.jar"]);
    let files: Vec<String> = ["magic-1.0.jar", "plus-3.0.jar", "named-1.0.jar", "changed-1.0.jar"].map(String::from).into();
    assert_eq!(carry::carry_ids(&inst_root, &server, &files), 2);
    let srv = crate::instances::load(&server).unwrap();
    let find = |f: &str| srv.addons.iter().filter(|a| a.file_name == f).collect::<Vec<_>>();
    let magic = find("magic-1.0.jar");
    assert_eq!(magic.len(), 1, "the bare entry gave way: {:?}", srv.addons);
    assert_eq!((magic[0].project_id, magic[0].enabled), (1, false));
    assert!(magic[0].icon.is_empty(), "a picture in this PC's cache doesn't go along");
    assert!(!magic[0].from_pack && !magic[0].locked, "the pack's own marks stay in the pack");
    let plus = find("plus-3.0.jar");
    assert_eq!((plus.len(), plus[0].project_id, plus[0].enabled, plus[0].icon.as_str()), (1, 2, true, "https://x/icon.png"));
    assert_eq!(find("named-1.0.jar")[0].name, "Server's own");
    assert!(find("changed-1.0.jar").is_empty(), "other bytes: not that mod's file");
    // A second sync of the same files changes nothing.
    assert_eq!(carry::carry_ids(&inst_root, &server, &files), 0);
    assert!(crate::install::unnamed_jars(&crate::instances::load(&server).unwrap()).iter().all(|f| f.starts_with("changed")));
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
fn source_mod_authors_follow_the_jar() {
    // A renamed mod: the name saved when it was installed (the old authors)
    // gives way to what the jar says now.
    let root = scratch("authors-follow-jar");
    let toml = b"[[mods]]\nmodId=\"foxstweaks\"\nversion=\"2.0.0\"\nauthors=\"watdafawx\"\n";
    std::fs::write(root.join("mods/foxstweaks-2.0.0.jar"), jar_bytes(&[("META-INF/neoforge.mods.toml", toml)]).into_inner()).unwrap();
    let src = ModSource {
        kind: Kind::GitHub,
        url: "watdafawx/foxs-tweaks".into(),
        publisher: "watdafawx/foxs-tweaks".into(),
        ..Default::default()
    };
    let mut inst = Instance {
        root: root.clone(),
        addons: vec![Addon {
            project_id: 1,
            file_name: "foxstweaks-2.0.0.jar".into(),
            name: "Fox's Tweaks".into(),
            author: "mtop".into(),
            icon: "C:/cache/icon.png".into(),
            enabled: true,
            class_id: crate::cf::CLASS_MODS,
            source: Some(src),
            ..Default::default()
        }],
        ..Default::default()
    };
    looks::fill(&mut inst);
    assert_eq!(inst.addons[0].author, "watdafawx");
    let _ = std::fs::remove_dir_all(root);
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

/// Importing with Mod sources off and "Skip these mods": the pinned jar is
/// left out (and only the pinned file), the code loses its source entry,
/// and the result names what wasn't installed.
#[test]
fn import_with_sources_off_skips_and_lists_them() {
    let a = scratch("skip-a");
    let (inst, _) = pinned_instance(&a);
    let pins = carry::pins(&inst);
    assert_eq!(pins.len(), 1);

    // A pack's overrides brought the pinned jar and another mod.
    let b = scratch("skip-b");
    std::fs::write(b.join("mods/modmenu-11.0.3.jar"), b"jar bytes").unwrap();
    std::fs::write(b.join("mods/other.jar"), b"other").unwrap();
    let mut got = Instance { root: b.clone(), ..Default::default() };
    carry::attach(&mut got, &pins);
    assert_eq!(carry::skip(&mut got, &pins), vec!["modmenu-11.0.3.jar".to_string()]);
    assert!(!b.join("mods/modmenu-11.0.3.jar").exists());
    assert!(b.join("mods/other.jar").exists(), "everything else stays");
    assert!(got.addons.iter().all(|a| a.file_name != "modmenu-11.0.3.jar"));

    // A jar with other bytes under that name isn't the pinned file: it stays.
    let c = scratch("skip-c");
    std::fs::write(c.join("mods/modmenu-11.0.3.jar"), b"other bytes").unwrap();
    let mut other = Instance { root: c.clone(), ..Default::default() };
    assert!(carry::skip(&mut other, &pins).is_empty());
    assert!(c.join("mods/modmenu-11.0.3.jar").exists());

    // The list the player sees.
    let note = carry::skipped_note(&pins).unwrap();
    assert!(note.contains("won't be installed"), "{note}");
    assert!(note.contains("ModMenu (GitHub: TerraformersMC/ModMenu)"), "{note}");
    let jar = carry::Pin { file: "ezactions-neoforge-1.21.1-2.0.3.5.jar".into(), source: ModSource { kind: Kind::Url, ..Default::default() } };
    assert_eq!(carry::display_name(&jar), "ezactions");
    assert!(carry::skipped_note(&[]).is_none());

    // A share code: the source entry goes, the rest of the code stays.
    let share = crate::sharecode::Share {
        base: crate::sharecode::Base::Plain { game_version: "1.21.1".into(), loader: "fabric".into() },
        name: None,
        loader: None,
        entries: vec![
            crate::sharecode::Entry::CurseFile { file: 1, enabled: true },
            crate::sharecode::Entry::Source { pin: pins[0].clone(), enabled: true },
        ],
    };
    assert_eq!(crate::sharecode::pins(&share), pins);
    let (left, note) = crate::sharecode::without(&share, &pins);
    assert_eq!(left.entries, vec![crate::sharecode::Entry::CurseFile { file: 1, enabled: true }]);
    assert!(note.is_some());
    assert!(crate::sharecode::pins(&left).is_empty());

    // A sync: the copy of the pinned jar is dropped, other copies stay.
    let ops = crate::rigsync::Ops {
        up: vec![
            crate::model::SyncOp::Copy { src: a.join("mods/modmenu-11.0.3.jar"), dst: c.join("mods/modmenu-11.0.3.jar") },
            crate::model::SyncOp::Copy { src: a.join("mods/x.jar"), dst: c.join("mods/x.jar") },
        ],
        down: Vec::new(),
    };
    assert_eq!(ops.source_pins(&c, &a).len(), 1);
    let left = ops.without(&pins);
    assert_eq!(left.up.len(), 1);
    for d in [a, b, c] {
        let _ = std::fs::remove_dir_all(d);
    }
}

// Packs from a folder on a branch (Faithful 32x Mods, recorded 2026-11-03).
const TREE_COMMITS: &str = include_str!("fixtures/tree-faithful-commits.json");
const TREE_CONTENTS: &str = include_str!("fixtures/tree-faithful-contents.json");
const TREE_COMPARE: &str = include_str!("fixtures/tree-faithful-compare.json");

#[test]
fn tree_links() {
    let t = tree::parse_link("https://github.com/Faithful-Resource-Pack/Faithful-32x-Mods/tree/1.20.x").unwrap();
    assert_eq!(
        (t.owner.as_str(), t.repo.as_str(), t.branch.as_str(), t.path.as_str()),
        ("Faithful-Resource-Pack", "Faithful-32x-Mods", "1.20.x", "")
    );
    assert_eq!(t.url(), "Faithful-Resource-Pack/Faithful-32x-Mods/tree/1.20.x");
    assert_eq!(t.file_name(), "Faithful-32x-Mods-1.20.x.zip");
    let sub = tree::parse_link("github.com/o/r/tree/main/packs/Dark%20UI/?tab=x").unwrap();
    assert_eq!((sub.branch.as_str(), sub.path.as_str()), ("main", "packs/Dark UI"));
    assert_eq!(sub.file_name(), "r-Dark-UI-main.zip");
    assert_eq!(tree::of_url(&sub.url()), Some(sub.clone()));
    assert_eq!(sub.child("x").path, "packs/Dark UI/x");
    // Not a folder: the repo, a release, a file.
    assert!(tree::parse_link("https://github.com/o/r").is_none());
    assert!(tree::parse_link("https://github.com/o/r/releases/tag/v1").is_none());
    assert!(tree::parse_link("https://github.com/o/r/blob/main/pack.mcmeta").is_none());
    // As a source: GitHub, keyed by the folder, published by the repo.
    let s = parse_link("https://github.com/Faithful-Resource-Pack/Faithful-32x-Mods/tree/1.20.x/").unwrap();
    assert_eq!(s.kind, Kind::GitHub);
    assert_eq!(s.url, "Faithful-Resource-Pack/Faithful-32x-Mods/tree/1.20.x");
    assert_eq!(s.publisher, "Faithful-Resource-Pack/Faithful-32x-Mods");
    assert_eq!(s.page(), "https://github.com/Faithful-Resource-Pack/Faithful-32x-Mods/tree/1.20.x");
    assert_eq!(s.key(), "github:faithful-resource-pack/faithful-32x-mods/tree/1.20.x");
    // A mod can't come from a folder; nothing is asked of GitHub.
    let e = resolve("https://github.com/o/r/tree/main", "1.21.1", Loader::NeoForge, false).unwrap_err();
    assert!(format!("{e}").contains("folder on a branch"));
}

#[test]
fn tree_commits_and_listing() {
    let c = tree::parse_commits(TREE_COMMITS).unwrap();
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].sha, "989f3bfcc77c024e0b25594222a1fe0dcffe8637");
    assert_eq!(c[0].date, "2026-08-01T04:44:17Z");
    assert_eq!(c[0].label(), "989f3bf · 2026-08-01");
    let t = tree::parse_link("https://github.com/Faithful-Resource-Pack/Faithful-32x-Mods/tree/1.20.x").unwrap();
    let cand = tree::candidate(&t, &c[0], 0);
    assert_eq!(
        cand.url,
        "https://codeload.github.com/Faithful-Resource-Pack/Faithful-32x-Mods/zip/989f3bfcc77c024e0b25594222a1fe0dcffe8637"
    );
    assert_eq!(cand.file_name, "Faithful-32x-Mods-1.20.x.zip");
    assert_eq!(cand.version, c[0].sha);
    let l = tree::parse_listing(TREE_CONTENTS).unwrap();
    assert!(l.iter().any(|e| e.name == "assets" && e.kind == "dir"));
    assert!(tree::is_pack_root(&l, cf::CLASS_RESOURCE_PACKS));
    assert!(!tree::is_pack_root(&l, cf::CLASS_SHADERS));
    // What changed: the commits since the installed one, newest first.
    let mut ch = tree::parse_compare(TREE_COMPARE).unwrap();
    ch.reverse();
    assert_eq!(ch[0].sha, c[0].sha);
    let notes = tree::notes(&ch);
    assert!(notes.starts_with("- Faithful License v4 (989f3bf)"), "{notes}");
    assert!(notes.contains("add thermal start"));
}

/// The same commit's file twice has the same id and date, so the update
/// check doesn't offer what's installed; a later commit is offered.
#[test]
fn tree_installed_file_matches_listing() {
    let c = tree::parse_commits(TREE_COMMITS).unwrap().remove(0);
    let t = tree::parse_link("https://github.com/Faithful-Resource-Pack/Faithful-32x-Mods/tree/1.20.x").unwrap();
    let mut src = parse_link(&t.page()).unwrap();
    src.class = cf::CLASS_RESOURCE_PACKS;
    src.pattern = pattern_of(&t.file_name());
    src.seen = c.sha.clone();
    let id = packs::id_of(&src);
    let rec = packs::Record {
        file_name: t.file_name(),
        sha256: "x".into(),
        name: "Faithful".into(),
        source: src.clone(),
        date: c.date.clone(),
    };
    let installed = packs::installed_file(id, &rec);
    let listed = to_file(id, &src, &tree::candidate(&t, &c, 0), "", Loader::Any, false);
    assert_eq!(installed.id, listed.id);
    assert_eq!(installed.file_date, listed.file_date);
    assert_eq!(installed.display_name, "989f3bf · 2026-08-01");
    let later = tree::Commit { sha: "0123456789abcdef".into(), date: "2026-09-01T00:00:00Z".into(), ..Default::default() };
    let next = to_file(id, &src, &tree::candidate(&t, &later, 0), "", Loader::Any, false);
    assert_ne!(next.id, installed.id);
    assert!(next.file_date > installed.file_date);
    assert_eq!(mod_for(id, &src, "Faithful").class_id, Some(cf::CLASS_RESOURCE_PACKS));
}

fn archive(path: &std::path::Path, files: &[(&str, &str)]) {
    let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    for (n, body) in files {
        if n.ends_with('/') {
            z.add_directory(n.trim_end_matches('/'), zip::write::SimpleFileOptions::default()).unwrap();
        } else {
            z.start_file(*n, zip::write::SimpleFileOptions::default()).unwrap();
            std::io::Write::write_all(&mut z, body.as_bytes()).unwrap();
        }
    }
    z.finish().unwrap();
}

fn zip_names(path: &std::path::Path) -> Vec<String> {
    let z = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut v: Vec<String> = z.file_names().map(str::to_string).collect();
    v.sort();
    v
}

#[test]
fn tree_repack() {
    let dir = std::env::temp_dir().join(format!("hopper-tree-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let a = dir.join("a.zip");
    archive(
        &a,
        &[
            ("Repo-abc/", ""),
            ("Repo-abc/pack.mcmeta", "{}"),
            ("Repo-abc/.gitignore", "x"),
            ("Repo-abc/.github/workflows/ci.yml", "x"),
            ("Repo-abc/assets/minecraft/textures/a.png", "png"),
            ("Repo-abc/packs/dark/pack.mcmeta", "{}"),
            ("Repo-abc/packs/dark/assets/b.png", "png"),
            ("Repo-abc/packs/plain/readme.txt", "no pack"),
            ("Repo-abc/shaderpack/shaders/final.fsh", "x"),
            ("Repo-abc/../evil.txt", "x"),
        ],
    );
    // The repo's root: pack.mcmeta at the top, repo housekeeping left out.
    let out = dir.join("root.zip");
    tree::repack(&a, "", cf::CLASS_RESOURCE_PACKS, &out).unwrap();
    let n = zip_names(&out);
    assert!(n.contains(&"pack.mcmeta".to_string()));
    assert!(n.contains(&"assets/minecraft/textures/a.png".to_string()));
    assert!(!n.iter().any(|x| x.starts_with(".git") || x.contains("..")), "{n:?}");
    // Same commit, same bytes.
    let again = dir.join("again.zip");
    tree::repack(&a, "", cf::CLASS_RESOURCE_PACKS, &again).unwrap();
    assert_eq!(sha256_file(&out).unwrap(), sha256_file(&again).unwrap());
    // A folder further down.
    let sub = dir.join("sub.zip");
    tree::repack(&a, "packs/dark", cf::CLASS_RESOURCE_PACKS, &sub).unwrap();
    assert_eq!(zip_names(&sub), vec!["assets/b.png".to_string(), "pack.mcmeta".to_string()]);
    let s = SrcFile::default();
    assert!(packs::check(&sub, &s, cf::CLASS_RESOURCE_PACKS).is_ok());
    // Not a pack: stops, writes nothing.
    let bad = dir.join("bad.zip");
    let e = tree::repack(&a, "packs/plain", cf::CLASS_RESOURCE_PACKS, &bad).unwrap_err();
    assert!(format!("{e}").contains("pack.mcmeta"));
    assert!(!bad.exists());
    // Shader packs go by their shaders folder.
    let sh = dir.join("sh.zip");
    tree::repack(&a, "shaderpack", cf::CLASS_SHADERS, &sh).unwrap();
    assert!(packs::check(&sh, &s, cf::CLASS_SHADERS).is_ok());
    assert!(packs::check(&sh, &s, cf::CLASS_RESOURCE_PACKS).is_err());
    // A stated checksum must match.
    let wrong = SrcFile { digest: "00".into(), ..Default::default() };
    assert!(packs::check(&sub, &wrong, cf::CLASS_RESOURCE_PACKS).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Release assets that are zips: packs take them, mods don't.
#[test]
fn pack_release_zips() {
    let rel = github::Release {
        tag_name: "v2.0".into(),
        assets: vec![
            github::Asset { name: "Dark-UI-1.21.zip".into(), ..Default::default() },
            github::Asset { name: "Dark-UI-1.20.zip".into(), ..Default::default() },
            github::Asset { name: "Dark-UI-sources.zip".into(), ..Default::default() },
            github::Asset { name: "darkui-fabric.jar".into(), ..Default::default() },
        ],
        ..Default::default()
    };
    let mods = github::from_releases(std::slice::from_ref(&rel));
    assert_eq!(mods.len(), 1);
    let zips = github::from_releases_for(std::slice::from_ref(&rel), cf::CLASS_RESOURCE_PACKS);
    assert_eq!(zips.len(), 3);
    let f = |n: &str| fit_class(n, "v2.0", "1.21.1", Loader::NeoForge, cf::CLASS_RESOURCE_PACKS);
    assert_eq!(f("Dark-UI-1.21.zip"), Fit::Good);
    assert_eq!(f("Dark-UI-1.20.zip"), Fit::Minecraft("1.20".into()));
    assert_eq!(f("Dark-UI.zip"), Fit::Unknown);
    assert!(matches!(f("Dark-UI-sources.zip"), Fit::Extra(_)));
    assert!(matches!(f("darkui.jar"), Fit::Extra(_)));
    let mut all = zips;
    for c in &mut all {
        c.fit = f(&c.file_name);
    }
    let src = ModSource { class: cf::CLASS_RESOURCE_PACKS, url: "o/r".into(), ..Default::default() };
    let r = pick(src, all, None);
    assert_eq!(r.pick.map(|i| r.candidates[i].file_name.clone()).as_deref(), Some("Dark-UI-1.21.zip"));
    // Old records (no class) read as mods; a mod's record doesn't gain one.
    let m: ModSource = serde_json::from_str(r#"{"kind":"github","url":"o/r"}"#).unwrap();
    assert_eq!(m.class, 0);
    assert!(!serde_json::to_string(&m).unwrap().contains("class"));
}

#[test]
fn carry_report_says_why_sources_stayed_behind() {
    use carry::CarryReport;
    use serde_json::json;
    // The panel has mods from links off: nothing attached, and it says so.
    let off = CarryReport::from_answer(9, &json!({ "attached": 0, "off": true }));
    assert!(off.needs_panel_setting());
    assert_eq!(off.note().unwrap(), "9 mods' sources didn't go along: this panel has mods from links turned off.");
    // One mod: singular.
    let one = CarryReport::from_answer(1, &json!({ "attached": 0, "off": true }));
    assert_eq!(one.note().unwrap(), "1 mod's source didn't go along: this panel has mods from links turned off.");
    // Some jars weren't the pinned file.
    let part = CarryReport::from_answer(5, &json!({ "attached": 3 }));
    assert_eq!((part.missed(), part.needs_panel_setting()), (2, false));
    assert_eq!(part.note().unwrap(), "2 mods' sources didn't go along: those jars aren't the file the source was pinned to.");
    // All attached: no line.
    assert_eq!(CarryReport::from_answer(4, &json!({ "attached": 4 })).note(), None);
    // An old panel answers without `attached`: nothing to say.
    assert_eq!(CarryReport::from_answer(4, &json!({ "ok": true })).note(), None);
    // Nothing sent: no line.
    assert_eq!(CarryReport::default().note(), None);
    // Couldn't reach the panel.
    let failed = CarryReport { sent: 2, failed: Some("connection refused\nmore".into()), ..Default::default() };
    assert_eq!(failed.note().unwrap(), "2 mods' sources didn't go along: they couldn't be sent (connection refused).");
    // A sync that goes both ways adds up.
    let mut both = off.clone();
    both.merge(CarryReport { sent: 2, attached: 2, ..Default::default() });
    assert_eq!((both.sent, both.attached, both.missed(), both.off), (11, 2, 9, true));
}
