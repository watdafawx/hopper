//! The full trace of a crash's error, from the game log, when the crash
//! report's own trace is thinner or names a mod that only passed the error
//! on. Used by [`crate::crashdetect`].
//!
//! NeoForge writes the report from the loader's view: "Architectury
//! encountered an error while dispatching the …Event event", with a trace
//! that starts where the loader caught it. The log has the same error with
//! the frames inside the listener, where the mod that actually broke sits
//! between the library's frames:
//!
//! ```text
//! at …/neoforge@…/BuildCreativeModeTabContentsEvent.insertAfter(…)
//! at …/architectury@…/CreativeTabOutput.accept(…)          (library)
//! at …/somemod@…/com.x.SomeMod.lambda$init$1(SomeMod.java:27)
//! at …/architectury@…/CreativeTabRegistryImpl.event(…)     (library)
//! at MC-BOOTSTRAP/net.neoforged.bus/…EventBus.post(…)
//! ```

use regex::Regex;
use std::sync::LazyLock;

fn re(p: &str) -> Regex {
    Regex::new(p).expect("static regex")
}

/// Mods that run other mods' code for them: event and registry layers,
/// config and language libraries, and mods that wrap the game's own methods
/// for speed. An error that passes through them is rarely theirs when
/// another mod's frame sits inside, so they don't take the blame from it.
const PASS_THROUGH: [&str; 18] = [
    "architectury", "balm", "kotlinforforge", "kotlinlangforge", "cloth_config", "modernfix", "mixinextras", "mixin",
    "puzzleslib", "resourcefullib", "connector", "forgified_fabric_api", "fabric_api", "moonlight", "bookshelf",
    "creativecore", "fzzy_config", "yet_another_config_lib_v3",
];

/// Whether a mod id is one of [`PASS_THROUGH`] (or a part of the Fabric
/// API, which Sinytra Connector loads as many small mods).
pub fn is_pass_through(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    PASS_THROUGH.contains(&id.as_str()) || id.starts_with("fabric_") || id.starts_with("fabric-")
}

/// NeoForge's error when a mod puts an item in a creative tab before or
/// after one that isn't in that tab: `Itemstack 0 minecraft:air does not
/// exist in tab's list` (the other item isn't in the game at all, so it
/// reads as air).
static TAB_MISSING: LazyLock<Regex> = LazyLock::new(|| re(r"Itemstack \d+ (\S+) does not exist in tab's list"));

/// A plain explanation for the mod whose code threw a known error, with
/// what to do.
pub fn explain(error: &str) -> Option<String> {
    let item = TAB_MISSING.captures(error)?.get(1)?.as_str().to_string();
    Some(if item == "minecraft:air" {
        "It puts its items in a creative tab next to an item that isn't in the game (the game sees empty air there), so the game couldn't fill its creative tabs. Update or remove this mod, or add the mod that adds the missing item."
            .to_string()
    } else {
        format!(
            "It puts its items in a creative tab next to {item}, which isn't in that tab (a config or another mod may have turned it off), so the game couldn't fill its creative tabs. Update or remove this mod, or turn {item} back on."
        )
    })
}

/// The error line the report is about, as Java wrote it
/// (`java.lang.IllegalArgumentException: …`): the one under NeoForge's
/// "encountered an error" line, else the line under "Description:".
pub fn report_error(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let real = |l: &&&str| {
        let t = l.trim();
        !t.is_empty() && !t.starts_with("at ") && !t.starts_with("- ") && !t.ends_with("encountered:")
    };
    if let Some(i) = lines.iter().position(|l| l.trim_start().starts_with("- ") && l.contains(" encountered an error")) {
        return lines.get(i + 1).filter(real).map(|l| l.trim().to_string());
    }
    let d = lines.iter().position(|l| l.trim_start().starts_with("Description:"))?;
    lines[d + 1..].iter().take(4).find(real).map(|l| l.trim().to_string())
}

/// The log's own trace of `error`: the frames under the last line that is
/// that same error and is followed by a frame of an installed mod (not the
/// game, the loader or Java). Logs print an error more than once (where it
/// was caught, then again as the loader's list); the last copy with frames
/// is the one closest to the crash.
pub fn trace_of<'a>(log: &'a str, error: &str) -> Option<Vec<&'a str>> {
    let error = error.trim();
    if error.is_empty() {
        return None;
    }
    let lines: Vec<&str> = log.lines().collect();
    let mut best = None;
    for (i, l) in lines.iter().enumerate() {
        if l.trim() != error {
            continue;
        }
        let block: Vec<&str> = lines[i + 1..]
            .iter()
            .take_while(|l| {
                let t = l.trim_start();
                t.starts_with("at ") || t.starts_with("...")
            })
            .copied()
            .collect();
        if block.iter().any(|l| frame_mod(l).is_some_and(|(id, _)| !id.is_empty())) {
            best = Some(block);
        }
    }
    best
}

/// `at TRANSFORMER/somemod@1.0/com.x.Y.z(Y.java:1)`, with or without the
/// jar after it.
static MODULE_FRAME: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*at\s+(?:[\w-]+/)?([\w.-]+)@[^/\s]+/\S+\("));
/// The jar NeoForge prints after a frame: `~[somemod-1.0.jar%23123!/:1.0]`.
static FRAME_JAR: LazyLock<Regex> = LazyLock::new(|| re(r"\)\s*~?\[([^\]%!]+?\.jar)"));

/// The mod a frame is in: (mod id, jar), either may be empty. `None` for
/// the game, the loaders and Java.
pub fn frame_mod(line: &str) -> Option<(String, String)> {
    let id = MODULE_FRAME.captures(line).map(|m| m[1].to_string()).unwrap_or_default();
    let jar = FRAME_JAR.captures(line).map(|m| m[1].to_string()).unwrap_or_default();
    // Java modules (`java.base`, `cpw.mods.modlauncher`) have dots; mod ids don't.
    if id.contains('.') || matches!(id.as_str(), "minecraft" | "neoforge" | "forge" | "fml" | "fml_loader" | "fabricloader" | "java") {
        return None;
    }
    if id.is_empty() && jar.is_empty() {
        return None;
    }
    Some((id, jar))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libraries_pass_errors_through() {
        for id in ["architectury", "Balm", "kotlinforforge", "cloth_config", "modernfix", "fabric_api_base"] {
            assert!(is_pass_through(id), "{id}");
        }
        for id in ["litematicatool", "minecolonies", "create"] {
            assert!(!is_pass_through(id), "{id}");
        }
    }

    #[test]
    fn a_missing_tab_item_is_explained() {
        let air = explain("java.lang.IllegalArgumentException: Itemstack 0 minecraft:air does not exist in tab's list").unwrap();
        assert!(air.contains("isn't in the game") && air.contains("Update or remove this mod"), "{air}");
        let named = explain("IllegalArgumentException: Itemstack 1 mysticalagricultureplus:fe_seeds does not exist in tab's list").unwrap();
        assert!(named.contains("next to mysticalagricultureplus:fe_seeds, which isn't in that tab"), "{named}");
        assert_eq!(explain("java.lang.NullPointerException"), None);
    }

    #[test]
    fn frames_name_their_mod_with_or_without_a_jar() {
        assert_eq!(
            frame_mod("\tat TRANSFORMER/litematicatool@2.0.0/com.x.LitematicaTool.lambda$init$1(LitematicaTool.java:27)"),
            Some(("litematicatool".into(), String::new()))
        );
        assert_eq!(
            frame_mod("\tat TRANSFORMER/litematicatool@2.0.0/com.x.L.a(L.java:27) ~[LitematicaTool-neoforge-2.0.0.jar%23566!/:?]"),
            Some(("litematicatool".into(), "LitematicaTool-neoforge-2.0.0.jar".into()))
        );
        assert_eq!(frame_mod("\tat TRANSFORMER/somemod@1.0/com.x.Y$$Lambda/0x0000000801570218.accept(Unknown Source)").unwrap().0, "somemod");
        for game in [
            "\tat TRANSFORMER/minecraft@1.21.1/net.minecraft.world.item.CreativeModeTab.buildContents(CreativeModeTab.java)",
            "\tat MC-BOOTSTRAP/fml_loader@4.0.45/net.neoforged.fml.ModLoader.postEvent(ModLoader.java:358)",
            "\tat MC-BOOTSTRAP/net.neoforged.bus/net.neoforged.bus.EventBus.post(EventBus.java:360)",
            "\tat java.base/java.lang.Iterable.forEach(Iterable.java:75)",
            "\tat cpw.mods.bootstraplauncher@2.0.2/cpw.mods.bootstraplauncher.BootstrapLauncher.main(BootstrapLauncher.java:69)",
        ] {
            assert_eq!(frame_mod(game), None, "{game}");
        }
    }

    #[test]
    fn the_report_error_and_its_trace_in_the_log() {
        let report = "Description: Unexpected error\n\nnet.neoforged.fml.ModLoadingException: Loading errors encountered:\n\
\t- Lib (lib) encountered an error while dispatching the x.YEvent event\n\
\t  java.lang.IllegalStateException: boom\n\n\tat x\n";
        assert_eq!(report_error(report).as_deref(), Some("java.lang.IllegalStateException: boom"));
        assert_eq!(report_error("Description: Rendering\n\njava.lang.NullPointerException: x\n\tat a.b(c)\n").as_deref(), Some("java.lang.NullPointerException: x"));
        let log = "java.lang.IllegalStateException: boom\n\tat TRANSFORMER/old@1/a.B.c(B.java:1)\n\
[later] caught\njava.lang.IllegalStateException: boom\n\tat TRANSFORMER/lib@1/a.B.c(B.java:1)\n\tat TRANSFORMER/real@1/d.E.f(E.java:2)\n\tat java.base/java.lang.Thread.run(Thread.java:1)\n\
\t- Lib (lib) encountered an error\n\t  java.lang.IllegalStateException: boom\n\n";
        let t = trace_of(log, "java.lang.IllegalStateException: boom").unwrap();
        assert_eq!(t.len(), 3);
        assert!(t[1].contains("real@1"));
        assert_eq!(trace_of(log, "java.lang.IllegalStateException: other"), None);
    }
}
