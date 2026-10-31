//! Plain-words signatures for common crash causes that the crash report or
//! the game log spells out: a mod installed twice, a dependency missing in
//! Forge's format, a mixin that couldn't inject, a config file that can't
//! be read, a class / method / field a mod expected that isn't there, a mod
//! built for a newer Java, a world entry whose mod is gone, and a graphics
//! driver that crashed Java. Each names the mod (when there is one) and says
//! what to do. Used by [`crate::crashdetect`].
//!
//! Wording and patterns are Hopper's own, written from the loaders' and
//! Java's messages.

use crate::crashdetect::{Certainty, Raw};
use regex::Regex;
use std::sync::LazyLock;

fn re(p: &str) -> Regex {
    Regex::new(p).expect("static regex")
}

fn raw(mod_id: &str, jar: &str, certainty: Certainty, why: String) -> Raw {
    Raw { mod_id: mod_id.into(), jar: jar.into(), certainty, why, class: String::new(), needs: Vec::new() }
}

/// Forge 1.18–1.20's dependency list:
/// `Mod ID: 'create', Requested by: 'createaddon', Expected range: '[0.5.1,)', Actual version: '[MISSING]'`
static FORGE_DEP: LazyLock<Regex> = LazyLock::new(|| {
    re(r"Mod ID: '([\w.-]+)', Requested by: '([\w.-]+)', Expected range: '([^']*)', Actual version: '([^']*)'")
});
/// Forge: `Mod ID: 'jei' from mod files: jei-1.jar, jei-2.jar`.
static FORGE_DUP: LazyLock<Regex> = LazyLock::new(|| re(r"Mod ID: '([\w.-]+)' from mod files: (.+)$"));
/// NeoForge: `Mod jei is present in multiple files: jei-1.jar, jei-2.jar`.
static NEO_DUP: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\bmod ([\w.-]+) is present in multiple files: (.+)$"));
/// Fabric: `- Mod 'Sodium' (sodium) 0.5.8 is loaded from 2 places: sodium-a.jar, sodium-b.jar`
/// (under "Duplicate mods found").
static FABRIC_DUP: LazyLock<Regex> = LazyLock::new(|| re(r"Mod '[^']+' \(([\w.-]+)\)[^:]*?(?:loaded from|multiple)[^:]*: (.+)$"));
static JAR: LazyLock<Regex> = LazyLock::new(|| re(r"([^\s,\\/]+\.jar)\b"));
/// A mixin that couldn't inject: `Critical injection failure: … in x.mixins.json:FooMixin from mod x`,
/// `InvalidInjectionException: … [x.mixins.json:FooMixin from mod x]`.
static INJECTION: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?:Critical injection failure|InvalidInjectionException|InjectionError|InvalidMixinException|InvalidAccessorException).*?[\w.-]+\.json:[\w$.]+ from mod ([\w.-]+)")
});
/// `Failed loading config file x-common.toml of type COMMON for modid x`
/// (Forge, NeoForge), `Failed to load config file … for mod x`.
static CONFIG: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)fail(?:ed)? (?:loading|to load|to read) (?:the )?config file (\S+?)(?: of type \w+)? for mod(?:id)? ([\w.-]+)")
});
/// `UnsupportedClassVersionError: com/x/Y has been compiled by a more recent
/// version of the Java Runtime (class file version 65.0), this version of
/// the Java Runtime only recognizes class file versions up to 61.0`
static JAVA_VERSION: LazyLock<Regex> = LazyLock::new(|| {
    re(r"UnsupportedClassVersionError: ([\w/.$]+) has been compiled by a more recent version of the Java Runtime \(class file version (\d+)(?:\.\d+)?\), this version of the Java Runtime only recognizes class file versions up to (\d+)")
});
/// `Missing key in ResourceKey[minecraft:root / minecraft:worldgen/biome]: ResourceKey[minecraft:worldgen/biome / somemod:ash_plains]`
static MISSING_KEY: LazyLock<Regex> =
    LazyLock::new(|| re(r"Missing key in ResourceKey\[[^\]]*\]: ResourceKey\[[^\]]* / ([a-z0-9_.-]+):([^\]\s]+)\]"));
/// Fabric/NeoForge registry sync: the server has entries the client lacks.
static REGISTRY_SYNC: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)(?:registry remapping failed|Received a registry sync packet with unknown entries|unknown registry entries|missing registry entries)")
});
/// What the error says is missing from the code.
static NO_CLASS: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?:NoClassDefFoundError|ClassNotFoundException): (?:Could not initialize class )?([\w/.$]+)"));
/// `NoSuchMethodError: 'void com.x.Y.m(int)'` (Java 11+) or `com.x.Y.m(I)V`.
static NO_METHOD: LazyLock<Regex> = LazyLock::new(|| re(r"NoSuchMethodError: '?(?:[\w.$\[\]<>]+ )?([\w.$]+)\.([\w$<>]+)\("));
/// `NoSuchFieldError: Class com.x.Y does not have member field 'int z'`, or just the name.
static NO_FIELD: LazyLock<Regex> = LazyLock::new(|| {
    re(r"NoSuchFieldError: (?:Class ([\w.$]+) does not have member field '(?:[\w.$\[\]]+ )?([\w$]+)'|([\w$]+)\s*$)")
});

/// What a mod's code looked for and didn't find: (kind, name, the class it
/// is in — empty when the error doesn't say).
pub fn missing_code(error: &str) -> Option<(&'static str, String, String)> {
    if error.contains("UnsupportedClassVersionError") {
        return None;
    }
    if let Some(m) = NO_CLASS.captures(error) {
        let c = m[1].replace('/', ".");
        return Some(("class", c.clone(), c));
    }
    if let Some(m) = NO_METHOD.captures(error) {
        return Some(("method", format!("{}.{}", &m[1], &m[2]), m[1].to_string()));
    }
    if let Some(m) = NO_FIELD.captures(error) {
        return Some(match (m.get(1), m.get(2), m.get(3)) {
            (Some(c), Some(f), _) => ("field", format!("{}.{}", c.as_str(), f.as_str()), c.as_str().to_string()),
            (_, _, Some(f)) => ("field", f.as_str().to_string(), String::new()),
            _ => return None,
        });
    }
    None
}

/// Whether a class is the game's own (not a mod's).
fn game_class(class: &str) -> bool {
    ["net.minecraft.", "com.mojang."].iter().any(|p| class.starts_with(p))
}

/// The Java version of a class file version (65 → 21).
fn java_of(class_file: &str) -> Option<u32> {
    class_file.parse::<u32>().ok().filter(|v| *v > 44).map(|v| v - 44)
}

/// The plain explanation for the mod whose own code threw `error`, with
/// what to do, for the errors that say what went wrong.
pub fn explain_code(error: &str) -> Option<String> {
    if let Some(m) = JAVA_VERSION.captures(error) {
        let (need, have) = (java_of(&m[2])?, java_of(&m[3])?);
        return Some(format!(
            "It's built for Java {need}, but the game ran on Java {have}. Choose Java {need} or newer in the Java settings (or update the game to the version this mod is for)."
        ));
    }
    let (kind, name, class) = missing_code(error)?;
    Some(if game_class(&class) {
        format!(
            "Its code looks for the {kind} {name} in Minecraft itself, which this version of the game doesn't have: it was made for a different Minecraft version. Update it to the version for this game, or remove it."
        )
    } else {
        format!(
            "Its code looks for the {kind} {name}, which isn't there: the mod that has it is missing, or a version without it is installed. Update this mod and the mod it needs (or add that mod)."
        )
    })
}

/// Findings that name a mod from one line of the loader's or Java's own
/// text, for [`crate::crashdetect::signals`].
pub fn mod_signals(lines: &[&str], out: &mut Vec<Raw>) {
    let dup_block = lines.iter().any(|l| l.contains("uplicate mod"));
    for l in lines {
        if let Some(m) = FORGE_DEP.captures(l) {
            let (dep, by, range, actual) = (&m[1], &m[2], &m[3], &m[4]);
            let why = if actual.contains("MISSING") || actual.is_empty() {
                format!("It needs {dep} {range}, which isn't installed. Install {dep}, or remove this mod.")
            } else {
                format!("It needs {dep} {range}, but {dep} {actual} is installed. Update one of the two so they match.")
            };
            let mut r = raw(by, "", Certainty::Certain, why);
            if actual.contains("MISSING") || actual.is_empty() {
                r.needs.push((dep.to_string(), range.to_string()));
            }
            out.push(r);
            continue;
        }
        let dup = FORGE_DUP.captures(l).or_else(|| NEO_DUP.captures(l)).or_else(|| FABRIC_DUP.captures(l).filter(|_| dup_block));
        if let Some(m) = dup {
            let jars: Vec<String> = JAR.captures_iter(&m[2]).map(|j| j[1].to_string()).collect();
            if jars.len() >= 2 {
                let why = format!("It's installed twice ({}). Keep the newest copy and remove the others.", jars.join(", "));
                out.push(raw(&m[1], &jars[0], Certainty::Certain, why));
                continue;
            }
        }
        if let Some(m) = INJECTION.captures(l) {
            out.push(raw(&m[1], "", Certainty::Certain, crate::crashdetect::MIXIN_ERROR_WHY.into()));
            continue;
        }
        if let Some(m) = CONFIG.captures(l) {
            let file = m[1].trim_matches(['\'', '"']).rsplit(['/', '\\']).next().unwrap_or("").to_string();
            let why = format!(
                "Its settings file {file} is damaged and can't be read. Delete it from the config folder (or reset it in the config editor) and the game makes a fresh one with default settings."
            );
            out.push(raw(&m[2], "", Certainty::Certain, why));
            continue;
        }
        // A class built for a newer Java: its jar is found by its package.
        if let Some(m) = JAVA_VERSION.captures(l)
            && let Some(why) = explain_code(l)
        {
            let class = m[1].replace('/', ".");
            out.push(Raw { mod_id: String::new(), jar: String::new(), certainty: Certainty::Likely, why, class, needs: Vec::new() });
        }
    }
}

/// The mod whose code is missing what another mod's code looked for: found
/// by the class's package. Not the game's own classes.
pub fn missing_owner(error: &str) -> Option<Raw> {
    let (kind, name, class) = missing_code(error)?;
    if class.is_empty() || game_class(&class) || class.starts_with("java.") || class.starts_with("javax.") {
        return None;
    }
    let why = format!(
        "Another mod looked for the {kind} {name} in its code, which this version doesn't have. Updating one of the two usually fixes it."
    );
    Some(Raw { mod_id: String::new(), jar: String::new(), certainty: Certainty::Possible, why, class, needs: Vec::new() })
}

/// Findings only the crash report's own error can make (the log has many
/// errors the game recovers from): a world entry whose mod is gone names
/// that mod.
pub fn report_signals(text: &str, out: &mut Vec<Raw>) {
    if let Some(m) = MISSING_KEY.captures(text)
        && !matches!(&m[1], "minecraft" | "neoforge" | "forge" | "c" | "fabric")
    {
        let why = format!("The world or a data pack needs {}:{} from it, and the installed version doesn't have it. Update it (or put back the version the world was made with).", &m[1], &m[2]);
        out.push(raw(&m[1], "", Certainty::Likely, why));
    }
}

/// The graphics driver's DLLs, by maker.
fn driver_maker(dll: &str) -> Option<&'static str> {
    let d = dll.to_ascii_lowercase();
    if d.starts_with("nvogl") || d.starts_with("nvd3d") || d.starts_with("nvwgf") {
        Some("NVIDIA")
    } else if d.starts_with("atio") || d.starts_with("atig") || d.starts_with("amdxx") || d.starts_with("amdvlk") {
        Some("AMD")
    } else if d.starts_with("ig") && (d.contains("icd") || d.contains("gl") || d.contains("xelp") || d.contains("d3d")) {
        Some("Intel")
    } else {
        None
    }
}

static PROBLEM_FRAME: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^#\s+C\s+\[([\w.-]+\.dll)"));

/// The graphics driver's maker, when Java's crash file blames its DLL.
pub fn driver_crash(native: &str) -> Option<&'static str> {
    PROBLEM_FRAME.captures(native).and_then(|m| driver_maker(&m[1]))
}

/// One plain sentence about a cause that isn't one mod, with what to do:
/// the wrong Java, a graphics driver, a world entry whose mod is gone, a
/// server and player with different mods. `text` is the crash report, `log`
/// the game log, `native` Java's own crash file. Empty when none applies.
pub fn advice(text: &str, log: &str, native: &str, server: bool) -> String {
    let java_where = if server { "the server's Java settings" } else { "Hopper's Java settings for this instance" };
    let java = JAVA_VERSION.captures(text).or_else(|| JAVA_VERSION.captures(log));
    if let Some(m) = java
        && let (Some(need), Some(have)) = (java_of(&m[2]), java_of(&m[3]))
    {
        return format!("A mod needs Java {need}, but it ran on Java {have}. Choose Java {need} or newer in {java_where}.");
    }
    if let Some(maker) = driver_crash(native) {
        return format!(
            "The {maker} graphics driver crashed Java. Install the newest driver from {maker}'s website. If you use shaders, try once without them."
        );
    }
    let gl = |t: &str| t.contains("Pixel format not accelerated") || t.contains("does not appear to support OpenGL");
    if gl(text) || gl(log) {
        return "The graphics driver doesn't support OpenGL properly (often Windows' generic driver). Install the driver from your graphics card's maker.".into();
    }
    if let Some(m) = MISSING_KEY.captures(text) {
        return format!(
            "The world needs {}:{}, which isn't in the game: the mod \"{}\" is missing, turned off, or a version without it is installed. Add or update that mod, then open the world again.",
            &m[1], &m[2], &m[1]
        );
    }
    if server && let Some(p) = port_in_use(log).or_else(|| port_in_use(text)) {
        return p;
    }
    if !server && (REGISTRY_SYNC.is_match(text) || REGISTRY_SYNC.is_match(log)) {
        return "The server has content this game doesn't: your mods don't match the server's. Install the server's mod list (same mods, same versions).".into();
    }
    String::new()
}

/// The port a server couldn't open (vanilla's `**** FAILED TO BIND TO
/// PORT!`, "Failed to bind to port", with the BindException's "Address
/// already in use" under it), said plainly. Only when the server never
/// finished starting after it. The port comes from "Starting Minecraft
/// server on *:25565".
pub fn port_in_use(log: &str) -> Option<String> {
    static ON: LazyLock<Regex> = LazyLock::new(|| re(r"Starting Minecraft server on \S*?:(\d+)"));
    let lower = log.to_ascii_lowercase();
    let at = lower.rfind("failed to bind to port")?;
    if log[at..].contains("]: Done (") {
        return None;
    }
    let port = ON.captures_iter(&log[..at]).last().map(|m| m[1].to_string());
    Some(match port {
        Some(p) => format!(
            "Another server or program is already using port {p}, so the server couldn't start. Stop the other one, or give this server another port (server-port in its server.properties)."
        ),
        None => "Another server or program is already using the server's port, so it couldn't start. Stop the other one, or give this server another port (server-port in its server.properties).".into(),
    })
}

/// Shader mods: a factor when the graphics driver crashed.
pub const SHADER_MODS: [&str; 3] = ["iris", "oculus", "optifine"];

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(text: &str) -> Vec<(String, String, Certainty)> {
        let lines: Vec<&str> = text.lines().collect();
        let mut out = Vec::new();
        mod_signals(&lines, &mut out);
        out.into_iter().map(|r| (r.mod_id, r.jar, r.certainty)).collect()
    }

    #[test]
    fn forge_dependency_list() {
        let report = "net.minecraftforge.fml.ModLoadingException: Missing or unsupported mandatory dependencies:\n\
\tMod ID: 'create', Requested by: 'createaddon', Expected range: '[0.5.1.f,0.5.2)', Actual version: '[MISSING]'\n\
\tMod ID: 'minecraft', Requested by: 'oldmod', Expected range: '[1.19.2,1.19.3)', Actual version: '1.20.1'\n";
        let lines: Vec<&str> = report.lines().collect();
        let mut out = Vec::new();
        mod_signals(&lines, &mut out);
        assert_eq!(out[0].mod_id, "createaddon");
        assert_eq!(out[0].needs, [("create".to_string(), "[0.5.1.f,0.5.2)".to_string())]);
        assert!(out[0].why.contains("isn't installed"), "{}", out[0].why);
        assert_eq!(out[1].mod_id, "oldmod");
        assert!(out[1].why.contains("minecraft 1.20.1 is installed") && out[1].needs.is_empty(), "{}", out[1].why);
    }

    #[test]
    fn duplicate_mods_in_each_loader() {
        let forge = "Found duplicate mods:\n\tMod ID: 'jei' from mod files: jei-1.20.1-forge-15.2.0.jar, jei-1.20.1-forge-15.3.0.jar\n";
        assert_eq!(ids(forge), [("jei".into(), "jei-1.20.1-forge-15.2.0.jar".into(), Certainty::Certain)]);
        let neo = "\t- Mod jei is present in multiple files: jei-1.21.1-neoforge-19.1.jar, jei-1.21.1-neoforge-19.2.jar\n";
        assert_eq!(ids(neo)[0].0, "jei");
        let fabric = "net.fabricmc.loader.impl.FormattedException: Duplicate mods found!\n\t- Mod 'Sodium' (sodium) 0.5.8 is loaded from 2 places: sodium-fabric-0.5.8.jar, sodium-fabric-0.5.11.jar\n";
        assert_eq!(ids(fabric)[0].0, "sodium");
        let lines: Vec<&str> = fabric.lines().collect();
        let mut out = Vec::new();
        mod_signals(&lines, &mut out);
        assert!(out[0].why.contains("installed twice (sodium-fabric-0.5.8.jar, sodium-fabric-0.5.11.jar)"), "{}", out[0].why);
    }

    #[test]
    fn mixin_injection_failures_name_their_mod() {
        let log = "org.spongepowered.asm.mixin.injection.throwables.InvalidInjectionException: Critical injection failure: @Inject annotation on onTick could not find any targets matching 'tick' in net.minecraft.world.entity.Entity. Using refmap fancymod.refmap.json [PREINJECT Applicator Phase -> fancymod.mixins.json:EntityMixin from mod fancymod -> Prepare Injections -> handler$zzb000$fancymod$onTick(Lorg/spongepowered/asm/mixin/injection/callback/CallbackInfo;)V -> Parse]\n";
        assert_eq!(ids(log), [("fancymod".into(), String::new(), Certainty::Certain)]);
    }

    #[test]
    fn a_broken_config_file_names_its_mod() {
        let log = "[12:00:01] [main/ERROR] [ne.mi.fm.co.ConfigTracker/CONFIG]: Failed loading config file somemod-common.toml of type COMMON for modid somemod\n\
com.electronwill.nightconfig.core.io.ParsingException: Not enough data available\n";
        let lines: Vec<&str> = log.lines().collect();
        let mut out = Vec::new();
        mod_signals(&lines, &mut out);
        assert_eq!((out[0].mod_id.as_str(), out[0].certainty), ("somemod", Certainty::Certain));
        assert!(out[0].why.contains("somemod-common.toml is damaged"), "{}", out[0].why);
    }

    #[test]
    fn java_too_old_for_a_mod() {
        let report = "Description: Initializing game\n\njava.lang.UnsupportedClassVersionError: com/example/newmod/NewMod has been compiled by a more recent version of the Java Runtime (class file version 65.0), this version of the Java Runtime only recognizes class file versions up to 61.0\n";
        let lines: Vec<&str> = report.lines().collect();
        let mut out = Vec::new();
        mod_signals(&lines, &mut out);
        assert_eq!((out[0].class.as_str(), out[0].certainty), ("com.example.newmod.NewMod", Certainty::Likely));
        assert!(out[0].why.contains("built for Java 21, but the game ran on Java 17"), "{}", out[0].why);
        assert_eq!(advice(report, "", "", false), "A mod needs Java 21, but it ran on Java 17. Choose Java 21 or newer in Hopper's Java settings for this instance.");
        assert!(advice(report, "", "", true).contains("the server's Java settings"));
    }

    #[test]
    fn missing_classes_methods_and_fields() {
        assert_eq!(
            missing_code("java.lang.NoClassDefFoundError: com/simibubi/create/foundation/Thing"),
            Some(("class", "com.simibubi.create.foundation.Thing".into(), "com.simibubi.create.foundation.Thing".into()))
        );
        assert_eq!(
            missing_code("java.lang.NoSuchMethodError: 'void dev.lib.Api.register(java.lang.String, int)'").map(|m| (m.1, m.2)),
            Some(("dev.lib.Api.register".into(), "dev.lib.Api".into()))
        );
        assert_eq!(
            missing_code("java.lang.NoSuchFieldError: Class dev.lib.Api does not have member field 'int SIZE'").map(|m| m.1),
            Some("dev.lib.Api.SIZE".into())
        );
        assert_eq!(missing_code("java.lang.NoSuchFieldError: SIZE").map(|m| (m.1, m.2)), Some(("SIZE".into(), String::new())));
        let game = explain_code("java.lang.NoSuchMethodError: 'void net.minecraft.world.level.Level.oldMethod()'").unwrap();
        assert!(game.contains("made for a different Minecraft version"), "{game}");
        let other = explain_code("java.lang.ClassNotFoundException: dev.lib.Api").unwrap();
        assert!(other.contains("the class dev.lib.Api, which isn't there"), "{other}");
        assert_eq!(missing_owner("java.lang.NoSuchMethodError: 'void net.minecraft.X.y()'"), None);
        assert_eq!(missing_owner("java.lang.ClassNotFoundException: dev.lib.Api").unwrap().class, "dev.lib.Api");
    }

    #[test]
    fn a_world_entry_whose_mod_is_gone() {
        let report = "Description: Loading world\n\njava.lang.IllegalStateException: Missing key in ResourceKey[minecraft:root / minecraft:worldgen/biome]: ResourceKey[minecraft:worldgen/biome / ashlands:ash_plains]\n";
        let mut out = Vec::new();
        report_signals(report, &mut out);
        assert_eq!((out[0].mod_id.as_str(), out[0].certainty), ("ashlands", Certainty::Likely));
        assert!(advice(report, "", "", false).contains("the mod \"ashlands\" is missing"));
        // The game's own keys name no mod.
        let mut out = Vec::new();
        report_signals("Missing key in ResourceKey[minecraft:root / minecraft:item]: ResourceKey[minecraft:item / minecraft:stone]", &mut out);
        assert!(out.is_empty());
        let log = "[Render thread/ERROR]: Received a registry sync packet with unknown entries: [somemod:widget]";
        assert!(advice("", log, "", false).contains("your mods don't match the server's"));
    }

    #[test]
    fn a_server_port_in_use() {
        let log = "[12:00:01] [Server thread/INFO] [minecraft/DedicatedServer]: Starting Minecraft server on *:25565
[12:00:01] [Server thread/WARN] [minecraft/DedicatedServer]: **** FAILED TO BIND TO PORT!
[12:00:01] [Server thread/WARN] [minecraft/DedicatedServer]: The exception was: java.net.BindException: Address already in use: bind
[12:00:01] [Server thread/WARN] [minecraft/DedicatedServer]: Perhaps a server is already running on that port?
[12:00:01] [Server thread/INFO] [minecraft/MinecraftServer]: Stopping server
";
        let a = port_in_use(log).unwrap();
        assert!(a.starts_with("Another server or program is already using port 25565"), "{a}");
        assert!(a.contains("server-port"), "{a}");
        assert_eq!(advice("", log, "", true), a);
        // Started fine after all (a later line says so): not the cause.
        assert_eq!(port_in_use(&format!("{log}[12:00:09] [Server thread/INFO] [minecraft/DedicatedServer]: Done (8.1s)! For help, type \"help\"
")), None);
        assert_eq!(port_in_use("[12:00:01] [Server thread/INFO]: Done (8.1s)!"), None);
        assert!(port_in_use("Failed to bind to port").unwrap().contains("the server's port"));
    }

    #[test]
    fn graphics_driver_crashes() {
        let hs = "#\n# A fatal error has been detected by the Java Runtime Environment:\n#\n#  EXCEPTION_ACCESS_VIOLATION (0xc0000005) at pc=0x00007ffb1c0e5e3a, pid=1234, tid=5678\n#\n# Problematic frame:\n# C  [nvoglv64.dll+0xb15e3a]\n";
        assert_eq!(driver_crash(hs), Some("NVIDIA"));
        assert!(advice("", "", hs, false).starts_with("The NVIDIA graphics driver crashed Java"));
        assert_eq!(driver_crash("# C  [atio6axx.dll+0x1234]"), Some("AMD"));
        assert_eq!(driver_crash("# C  [ig9icd64.dll+0x1234]"), Some("Intel"));
        assert_eq!(driver_crash("# C  [lwjgl.dll+0x1234]"), None);
        let log = "[Render thread/ERROR]: GLFW error 65542: WGL: The driver does not appear to support OpenGL";
        assert!(advice("", log, "", false).contains("doesn't support OpenGL"));
        assert!(advice("Description: x\njava.lang.NullPointerException", "", "", false).is_empty());
    }
}
