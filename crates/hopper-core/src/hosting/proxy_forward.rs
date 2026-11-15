//! A server behind the VPS relay's Velocity (design/vps-relay.md, B4):
//! which forwarding mod it needs, what that mod's config says, the
//! `server.properties` keys the relay sets (and how they go back), which
//! settings are locked while it is on, and the steps between "direct",
//! "through the relay" and "playing at home".
//!
//! No I/O here: Hopper Server does the files (recorded in History) with
//! what these functions say.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::properties::Properties;

/// Modrinth projects of the two forwarding mods.
pub const PCF_PROJECT: &str = "vDyrHl8l";
pub const FABRIC_PROXY_LITE_PROJECT: &str = "8dI2tmqs";
/// Fabric API on Modrinth.
pub const FABRIC_API_PROJECT: &str = "P7dR8mSH";

/// The mod on the server that reads Velocity's modern forwarding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ForwardMod {
    /// Proxy Compatible Forge (Forge and NeoForge, LGPL-2.1).
    Pcf,
    /// FabricProxy-Lite (Fabric and Quilt, MIT).
    FabricProxyLite,
}

impl ForwardMod {
    pub fn project(self) -> &'static str {
        match self {
            ForwardMod::Pcf => PCF_PROJECT,
            ForwardMod::FabricProxyLite => FABRIC_PROXY_LITE_PROJECT,
        }
    }

    /// Mods it can't start without that its Modrinth page doesn't list as
    /// required: FabricProxy-Lite uses Fabric API's networking, and a bare
    /// Fabric server has no Fabric API.
    pub fn needs(self) -> &'static [&'static str] {
        match self {
            ForwardMod::Pcf => &[],
            ForwardMod::FabricProxyLite => &[FABRIC_API_PROJECT],
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ForwardMod::Pcf => "Proxy Compatible Forge",
            ForwardMod::FabricProxyLite => "FabricProxy-Lite",
        }
    }

    /// Its config file, relative to the server folder.
    pub fn config_rel(self) -> &'static str {
        match self {
            ForwardMod::Pcf => "config/proxy-compatible-forge.toml",
            ForwardMod::FabricProxyLite => "config/FabricProxy-Lite.toml",
        }
    }

    /// The config with modern forwarding on and `secret` in it. Written
    /// whole: every key the mod reads, so a default it would add on its
    /// first start can't differ from what the relay needs.
    pub fn config_text(self, secret: &str) -> String {
        match self {
            ForwardMod::Pcf => format!(
                "# Written by Hopper Server: this server is joined through the relay's Velocity.\n\
                 # \"Stop using the relay\" on the server page puts back what was here before.\n\
                 [forwarding]\n\tenabled = true\n\tmode = \"MODERN\"\n\tsecret = \"{secret}\"\n\n\
                 [crossStitch]\n\tenabled = true\n\n[debug]\n\tenabled = false\n"
            ),
            ForwardMod::FabricProxyLite => format!(
                "# Written by Hopper Server: this server is joined through the relay's Velocity.\n\
                 # \"Stop using the relay\" on the server page puts back what was here before.\n\
                 hackOnlineMode = true\nhackEarlySend = false\nhackMessageChain = true\nsecret = \"{secret}\"\n"
            ),
        }
    }

    /// A jar's mod id or file name says it is this mod.
    pub fn matches(self, mod_id: &str, file_name: &str) -> bool {
        let id = mod_id.to_ascii_lowercase();
        let file = file_name.to_ascii_lowercase();
        match self {
            ForwardMod::Pcf => id == "pcf" || id == "proxy_compatible_forge" || file.starts_with("proxy-compatible-forge"),
            ForwardMod::FabricProxyLite => id == "fabricproxy-lite" || file.starts_with("fabricproxy-lite"),
        }
    }
}

/// The forwarding mod for a server's loader and Minecraft version, or why
/// it can't go behind the relay's Velocity.
///
/// - Forge and NeoForge from 1.20.2: PCF alone. Before 1.20.2 PCF needs a
///   Velocity plugin as well (Ambassador), and the relay runs no plugins.
/// - Fabric and Quilt from 1.17: FabricProxy-Lite.
pub fn choose(loader: &str, game_version: &str) -> Result<ForwardMod, String> {
    let at_least = |v: &str| crate::scan::compare_versions(game_version.trim(), v) != std::cmp::Ordering::Less;
    match loader.to_ascii_lowercase().as_str() {
        "forge" | "neoforge" if at_least("1.20.2") => Ok(ForwardMod::Pcf),
        "forge" | "neoforge" => Err(format!(
            "Forge and NeoForge servers on Minecraft {game_version} can't check accounts through the relay yet: that needs Minecraft 1.20.2 or newer. Players can still join through the relay's address with the server checking accounts itself."
        )),
        "fabric" | "quilt" if at_least("1.17") => Ok(ForwardMod::FabricProxyLite),
        "fabric" | "quilt" => Err(format!("Fabric servers on Minecraft {game_version} can't check accounts through the relay: that needs Minecraft 1.17 or newer.")),
        _ => Err("This server has no mod loader, so it can't check accounts through the relay. Use a Forge, NeoForge or Fabric server.".into()),
    }
}

/// The keys the relay sets in `server.properties`, and to what.
pub const MANAGED: [(&str, &str); 4] = [
    // Velocity checks accounts; the server takes who Velocity forwards.
    ("online-mode", "false"),
    ("enforce-secure-profile", "false"),
    // The tunnel compresses what goes home; packets go through it
    // uncompressed so the codec can find the repeats.
    ("network-compression-threshold", "-1"),
    // Only the router at home connects to it.
    ("server-ip", "127.0.0.1"),
];

/// What a key held before the relay changed it: its value, or `None` when
/// the file didn't have it (it is taken out again).
pub type Before = BTreeMap<String, Option<String>>;

/// One key the relay changed, for History.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyChange {
    pub key: String,
    pub was: Option<String>,
    pub now: Option<String>,
}

/// Sets the relay's keys in `p`. Returns what each held before (for
/// [`restore`]) and what changed.
pub fn apply(p: &mut Properties) -> (Before, Vec<KeyChange>) {
    let mut before = Before::new();
    let mut changed = Vec::new();
    for (k, v) in MANAGED {
        let was = p.get(k).map(str::to_string);
        if was.as_deref() != Some(v) {
            changed.push(KeyChange { key: k.into(), was: was.clone(), now: Some(v.into()) });
            p.set(k, v);
        }
        before.insert(k.to_string(), was);
    }
    (before, changed)
}

/// Puts the keys back to `before`: their old values, or out of the file
/// when it didn't have them. A key someone changed by hand since (it no
/// longer has the relay's value) is left as it is.
pub fn restore(p: &mut Properties, before: &Before) -> Vec<KeyChange> {
    let mut changed = Vec::new();
    for (k, v) in MANAGED {
        let Some(was) = before.get(k) else { continue };
        let now = p.get(k).map(str::to_string);
        if now.as_deref() != Some(v) || now == *was {
            continue;
        }
        match was {
            Some(w) => p.set(k, w.clone()),
            None => p.remove(k),
        }
        changed.push(KeyChange { key: k.into(), was: now, now: was.clone() });
    }
    changed
}

/// Why a `server.properties` key can't be changed while the server checks
/// accounts through the relay, in the words the settings show.
pub fn locked(key: &str) -> Option<&'static str> {
    match key {
        "online-mode" => Some("Accounts are checked by the relay. Players can't join this server directly."),
        "enforce-secure-profile" | "network-compression-threshold" | "server-ip" => Some("Set by the relay."),
        _ => None,
    }
}

/// Where a server stands with the relay's Velocity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Not behind Velocity: the server checks accounts itself.
    #[default]
    Direct,
    /// Behind Velocity: the forwarding mod on, the keys set.
    Relay,
    /// "Play at home": the relay was down, so the keys are back and the
    /// mod is off until the player switches back.
    Home,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// "Use the relay": Direct → Relay.
    Use,
    /// "Stop using the relay": Relay or Home → Direct.
    Stop,
    /// "Play at home": Relay → Home.
    PlayAtHome,
    /// "Back on the relay": Home → Relay.
    BackToRelay,
}

/// What a step does to the server's files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    pub to: Mode,
    /// Set the relay's keys (true) or put them back (false); `None` leaves them.
    pub keys: Option<bool>,
    pub mod_: ModStep,
    /// Write the mod's config with the secret (true) or put back what was
    /// there before (false); `None` leaves it.
    pub config: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModStep {
    None,
    /// Add it (when it isn't there) and turn it on.
    Add,
    /// Take it out again (only when the relay added it; else back as it was).
    Remove,
    On,
    Off,
}

/// The step from `from` for `act`, or why it isn't one.
pub fn plan(from: Mode, act: Action) -> Result<Plan, &'static str> {
    use Action::*;
    use Mode::*;
    Ok(match (from, act) {
        (Direct, Use) => Plan { to: Relay, keys: Some(true), mod_: ModStep::Add, config: Some(true) },
        (Relay, Stop) => Plan { to: Direct, keys: Some(false), mod_: ModStep::Remove, config: Some(false) },
        // The keys are already back; the mod is off.
        (Home, Stop) => Plan { to: Direct, keys: None, mod_: ModStep::Remove, config: Some(false) },
        (Relay, PlayAtHome) => Plan { to: Home, keys: Some(false), mod_: ModStep::Off, config: None },
        (Home, BackToRelay) => Plan { to: Relay, keys: Some(true), mod_: ModStep::On, config: None },
        (Relay, Use | BackToRelay) => return Err("This server already checks accounts through the relay."),
        (Home, Use) => return Err("This server is playing at home. Switch it back to the relay instead."),
        (Home, PlayAtHome) => return Err("This server is already playing at home."),
        (Direct, _) => return Err("This server doesn't use the relay's account checks."),
    })
}

/// What Hopper Server keeps per server while it isn't direct (owner-only,
/// in the panel's data: it holds the old config, which may hold a secret).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Record {
    pub mode: Mode,
    pub forward_mod: Option<ForwardMod>,
    /// The relay added the mod's jar (else the server had it already).
    pub added_mod: bool,
    /// The jar's file name, to find it again.
    pub mod_file: String,
    /// Every jar the relay added (the mod and what it needed, Fabric API
    /// for newer FabricProxy-Lite), taken out again by "Stop".
    pub added_files: Vec<String>,
    /// It was there but turned off before.
    pub mod_was_off: bool,
    pub keys_before: Before,
    /// The mod's config before the relay wrote it (`None`: there was none).
    pub config_before: Option<String>,
    /// Unix seconds of the last change.
    pub since: u64,
    /// Folders the relay made (`mods`, `config`), taken out by "Stop" when
    /// nothing else is in them.
    pub made_dirs: Vec<String>,
    /// The voice mod's advertised address before the relay pointed it at
    /// itself (B6); `None` when the server has no voice mod or it wasn't
    /// changed.
    pub voice: Option<super::voice_mods::VoiceBefore>,
}

/// The folders "Use the relay" may write into.
pub const DIRS: [&str; 2] = ["mods", "config"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loader_picks_the_mod() {
        assert_eq!(choose("neoforge", "1.21.1"), Ok(ForwardMod::Pcf));
        assert_eq!(choose("forge", "1.20.2"), Ok(ForwardMod::Pcf));
        assert_eq!(choose("Forge", "1.20.4"), Ok(ForwardMod::Pcf));
        assert!(choose("forge", "1.20.1").unwrap_err().contains("1.20.2"));
        assert!(choose("forge", "1.19.2").is_err());
        assert_eq!(choose("fabric", "1.20.1"), Ok(ForwardMod::FabricProxyLite));
        assert_eq!(choose("quilt", "1.21.1"), Ok(ForwardMod::FabricProxyLite));
        assert_eq!(choose("fabric", "1.17"), Ok(ForwardMod::FabricProxyLite));
        assert!(choose("fabric", "1.16.5").is_err());
        assert!(choose("vanilla", "1.21.1").unwrap_err().contains("no mod loader"));
        assert!(choose("", "1.21.1").is_err());
    }

    #[test]
    fn config_holds_the_secret_and_modern_mode() {
        let pcf = ForwardMod::Pcf.config_text("abc123");
        assert!(pcf.contains("mode = \"MODERN\"") && pcf.contains("secret = \"abc123\"") && pcf.contains("[forwarding]\n\tenabled = true"));
        let fpl = ForwardMod::FabricProxyLite.config_text("abc123");
        assert!(fpl.contains("hackOnlineMode = true") && fpl.contains("secret = \"abc123\""));
        assert_eq!(ForwardMod::FabricProxyLite.config_rel(), "config/FabricProxy-Lite.toml");
    }

    #[test]
    fn mod_is_found_by_id_or_file() {
        assert!(ForwardMod::Pcf.matches("pcf", ""));
        assert!(ForwardMod::Pcf.matches("", "proxy-compatible-forge-1.3.1.jar"));
        assert!(ForwardMod::FabricProxyLite.matches("", "FabricProxy-Lite-2.6.0.jar"));
        assert!(!ForwardMod::FabricProxyLite.matches("fabric-api", "fabric-api-0.92.jar"));
    }

    const FILE: &str = "#Minecraft server properties\nmotd=Hi\nonline-mode=true\nserver-port=25570\nnetwork-compression-threshold=256\n";

    #[test]
    fn keys_go_on_and_back_exactly() {
        let mut p = Properties::parse(FILE);
        let (before, changed) = apply(&mut p);
        assert_eq!(p.get("online-mode"), Some("false"));
        assert_eq!(p.get("server-ip"), Some("127.0.0.1"));
        assert_eq!(p.get("network-compression-threshold"), Some("-1"));
        assert_eq!(changed.len(), 4);
        assert_eq!(changed[0], KeyChange { key: "online-mode".into(), was: Some("true".into()), now: Some("false".into()) });
        assert_eq!(before.get("server-ip"), Some(&None));
        // A setting someone changes meanwhile stays.
        p.set("motd", "Bye");
        let back = restore(&mut p, &before);
        assert_eq!(back.len(), 4);
        let mut want = Properties::parse(FILE);
        want.set("motd", "Bye");
        assert_eq!(p.to_text(), want.to_text());
        // Byte for byte when nothing else changed.
        let mut q = Properties::parse(FILE);
        let (b, _) = apply(&mut q);
        restore(&mut q, &b);
        assert_eq!(q.to_text(), FILE);
    }

    #[test]
    fn restore_leaves_a_key_changed_by_hand() {
        let mut p = Properties::parse(FILE);
        let (before, _) = apply(&mut p);
        p.set("server-ip", "10.0.0.5");
        let back = restore(&mut p, &before);
        assert_eq!(p.get("server-ip"), Some("10.0.0.5"));
        assert!(back.iter().all(|c| c.key != "server-ip"));
    }

    #[test]
    fn apply_twice_changes_nothing_more() {
        let mut p = Properties::parse(FILE);
        apply(&mut p);
        let (before, changed) = apply(&mut p);
        assert!(changed.is_empty());
        assert_eq!(before.get("online-mode"), Some(&Some("false".into())));
    }

    #[test]
    fn lock_rules() {
        assert!(locked("online-mode").unwrap().contains("checked by the relay"));
        assert_eq!(locked("server-ip"), Some("Set by the relay."));
        assert_eq!(locked("enforce-secure-profile"), Some("Set by the relay."));
        assert_eq!(locked("network-compression-threshold"), Some("Set by the relay."));
        assert_eq!(locked("motd"), None);
        assert_eq!(locked("server-port"), None);
        for (k, _) in MANAGED {
            assert!(locked(k).is_some(), "{k} is set by the relay, so it is locked");
        }
    }

    #[test]
    fn play_at_home_state_machine() {
        use Action::*;
        use Mode::*;
        let p = plan(Direct, Use).unwrap();
        assert_eq!((p.to, p.keys, p.mod_, p.config), (Relay, Some(true), ModStep::Add, Some(true)));
        let p = plan(Relay, PlayAtHome).unwrap();
        assert_eq!((p.to, p.keys, p.mod_, p.config), (Home, Some(false), ModStep::Off, None));
        let p = plan(Home, BackToRelay).unwrap();
        assert_eq!((p.to, p.keys, p.mod_, p.config), (Relay, Some(true), ModStep::On, None));
        let p = plan(Home, Stop).unwrap();
        assert_eq!((p.to, p.keys, p.mod_, p.config), (Direct, None, ModStep::Remove, Some(false)));
        let p = plan(Relay, Stop).unwrap();
        assert_eq!((p.to, p.keys, p.mod_, p.config), (Direct, Some(false), ModStep::Remove, Some(false)));
        assert!(plan(Direct, Stop).is_err());
        assert!(plan(Direct, PlayAtHome).is_err());
        assert!(plan(Direct, BackToRelay).is_err());
        assert!(plan(Relay, Use).is_err());
        assert!(plan(Relay, BackToRelay).is_err());
        assert!(plan(Home, Use).is_err());
        assert!(plan(Home, PlayAtHome).is_err());
    }
}
