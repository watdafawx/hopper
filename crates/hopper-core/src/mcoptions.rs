//! "Minecraft settings": the in-game options a player sets once in Hopper
//! (volume, mouse, FOV, distances, GUI scale, key bindings…) and Hopper
//! writes into each instance's `options.txt` right before it starts.
//!
//! Only the options the player turned on are written; every other line of
//! the file stays as it is, in its order. A setting is either given once
//! ("first start": the pack gets it, and what the player changes in game
//! afterwards stays) or every start. "Once" is remembered per instance as
//! the value Hopper last wrote, so changing a value in Hopper hands the new
//! one to every instance again.
//!
//! Packs that bring their own defaults through the DefaultOptions mod
//! (`config/defaultoptions/`) or YOSBR (`config/yosbr/`) only get them
//! copied by the mod while `options.txt` doesn't exist. So when Hopper has
//! to create the file, it starts from the pack's defaults and puts the
//! player's values over them, instead of a bare file that would make those
//! mods skip the pack's defaults. OptiFine (`optionsof.txt`) and Sodium
//! (`sodium-options.json`) keep their own files for their own options; the
//! vanilla options Hopper sets live in `options.txt` with them too, so
//! nothing there needs touching.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The player's Minecraft settings, as kept in settings.json.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct McOptions {
    /// Hopper applies them to instances at all.
    pub enabled: bool,
    /// By `options.txt` key (see [`SPECS`]). A value stays here when its
    /// switch is off, so turning it back on keeps what was set.
    pub values: BTreeMap<String, McValue>,
    /// Key bindings by action (`key.forward`), as `options.txt` writes the
    /// key (`key.keyboard.w`, with NeoForge/Forge a `:SHIFT`-like modifier
    /// after it).
    pub keys: BTreeMap<String, String>,
    pub keys_on: bool,
    pub keys_always: bool,
    /// The instance the keys were copied from, for the summary.
    pub keys_from: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct McValue {
    /// As `options.txt` writes it ("0.375" for an FOV of 85).
    pub value: String,
    pub on: bool,
    /// Every start, not only the first.
    pub always: bool,
}

/// How a setting shows in Hopper and maps to its `options.txt` value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// 0..1 shown as 0–100%.
    Volume,
    /// 0..1 shown as 0–200% (Minecraft's own scale).
    Sensitivity,
    /// Degrees 30–110, stored as (fov - 70) / 40.
    Fov,
    /// A whole number in a range.
    Number { min: i32, max: i32 },
    /// `guiScale`: 0 is Auto.
    GuiScale,
    /// `maxFps`: 260 is Unlimited.
    MaxFps,
    /// true / false.
    Bool,
    /// The language code (`en_us`).
    Lang,
    /// A switch that writes one fixed value (narrator off).
    Fixed(&'static str),
}

pub struct Spec {
    pub key: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    pub kind: Kind,
    /// The value a new world of options starts with, as written.
    pub default: &'static str,
    /// The oldest Minecraft (minor version: 18 for 1.18) that has it.
    pub since: u32,
}

const fn s(key: &'static str, label: &'static str, group: &'static str, kind: Kind, default: &'static str, since: u32) -> Spec {
    Spec { key, label, group, kind, default, since }
}

pub const SOUND: &str = "Sound";
pub const VIEW: &str = "Mouse & view";
pub const VIDEO: &str = "Video";
pub const START: &str = "First start";

/// Every option Hopper offers, in page order.
pub const SPECS: &[Spec] = &[
    s("soundCategory_master", "Master volume", SOUND, Kind::Volume, "1.0", 0),
    s("soundCategory_music", "Music", SOUND, Kind::Volume, "1.0", 0),
    s("soundCategory_record", "Jukebox / Note blocks", SOUND, Kind::Volume, "1.0", 0),
    s("soundCategory_weather", "Weather", SOUND, Kind::Volume, "1.0", 0),
    s("soundCategory_block", "Blocks", SOUND, Kind::Volume, "1.0", 0),
    s("soundCategory_hostile", "Hostile creatures", SOUND, Kind::Volume, "1.0", 0),
    s("soundCategory_neutral", "Friendly creatures", SOUND, Kind::Volume, "1.0", 0),
    s("soundCategory_player", "Players", SOUND, Kind::Volume, "1.0", 0),
    s("soundCategory_ambient", "Ambient / Environment", SOUND, Kind::Volume, "1.0", 0),
    s("soundCategory_voice", "Voice / Speech", SOUND, Kind::Volume, "1.0", 0),
    s("mouseSensitivity", "Mouse sensitivity", VIEW, Kind::Sensitivity, "0.5", 0),
    s("rawMouseInput", "Raw input", VIEW, Kind::Bool, "true", 15),
    s("invertYMouse", "Invert mouse", VIEW, Kind::Bool, "false", 0),
    s("fov", "Field of view", VIEW, Kind::Fov, "0.0", 0),
    s("guiScale", "GUI scale", VIEW, Kind::GuiScale, "0", 0),
    s("renderDistance", "Render distance", VIDEO, Kind::Number { min: 2, max: 32 }, "12", 0),
    s("simulationDistance", "Simulation distance", VIDEO, Kind::Number { min: 5, max: 32 }, "12", 18),
    s("maxFps", "Max framerate", VIDEO, Kind::MaxFps, "120", 0),
    s("enableVsync", "VSync", VIDEO, Kind::Bool, "true", 0),
    s("fullscreen", "Fullscreen", VIDEO, Kind::Bool, "false", 0),
    s("lang", "Language", START, Kind::Lang, "en_us", 0),
    s("tutorialStep", "Skip the tutorial hints", START, Kind::Fixed("none"), "none", 12),
    s("narrator", "Narrator off", START, Kind::Fixed("0"), "0", 0),
];

pub fn spec(key: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.key == key)
}

// ---- values as the player sees them --------------------------------------------

/// The number a slider shows for a stored value (percent, degrees, chunks…).
pub fn shown(kind: Kind, raw: &str) -> f32 {
    let v: f32 = raw.trim().trim_matches('"').parse().unwrap_or(0.);
    match kind {
        Kind::Volume => (v * 100.).round(),
        Kind::Sensitivity => (v * 200.).round(),
        Kind::Fov => (70. + v * 40.).round(),
        _ => v.round(),
    }
}

/// The stored value for a slider's number: the inverse of [`shown`].
pub fn stored(kind: Kind, shown: f32) -> String {
    match kind {
        Kind::Volume => trim_float(shown.clamp(0., 100.) / 100.),
        Kind::Sensitivity => trim_float(shown.clamp(0., 200.) / 200.),
        Kind::Fov => trim_float((shown.round().clamp(30., 110.) - 70.) / 40.),
        Kind::Number { min, max } => (shown.round() as i32).clamp(min, max).to_string(),
        Kind::GuiScale => (shown.round() as i32).clamp(0, 6).to_string(),
        Kind::MaxFps => (shown.round() as i32).clamp(10, 260).to_string(),
        Kind::Bool => (shown != 0.).to_string(),
        Kind::Lang | Kind::Fixed(_) => String::new(),
    }
}

/// Floats as Minecraft writes them: always a decimal point, no float noise.
fn trim_float(v: f32) -> String {
    let s = format!("{:.4}", v);
    let s = s.trim_end_matches('0');
    if s.ends_with('.') { format!("{s}0") } else { s.to_string() }
}

/// The value a row shows ("35%", "85", "Auto", "Unlimited").
pub fn label(kind: Kind, raw: &str) -> String {
    let n = shown(kind, raw);
    match kind {
        Kind::Volume | Kind::Sensitivity => format!("{n}%"),
        Kind::GuiScale if n == 0. => "Auto".into(),
        Kind::MaxFps if n >= 260. => "Unlimited".into(),
        Kind::Bool => if raw.trim() == "true" { "On" } else { "Off" }.into(),
        Kind::Lang => raw.trim().trim_matches('"').to_string(),
        Kind::Fixed(_) => String::new(),
        _ => format!("{n}"),
    }
}

/// A key as players know it: "key.keyboard.left.control" → "Left Control",
/// "key.mouse.4" → "Mouse 4", with a modifier in front ("Shift + G").
pub fn key_label(raw: &str) -> String {
    let mut parts = raw.splitn(2, ":key.");
    let first = parts.next().unwrap_or_default();
    // "key.keyboard.g:SHIFT" (NeoForge/Forge modifier after the key).
    let (key, modifier) = match first.split_once(':') {
        Some((k, m)) => (k, m),
        None => (first, ""),
    };
    let name = if key == "key.keyboard.unknown" || key.is_empty() {
        "Not bound".to_string()
    } else if let Some(m) = key.strip_prefix("key.mouse.") {
        match m {
            "left" => "Left click".into(),
            "right" => "Right click".into(),
            "middle" => "Middle click".into(),
            n => format!("Mouse {n}"),
        }
    } else if let Some(k) = key.strip_prefix("key.keyboard.") {
        k.split('.')
            .map(|w| {
                let mut c = w.chars();
                match c.next() {
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    } else if key.chars().all(|c| c.is_ascii_digit()) {
        // Minecraft 1.12 and older keep key codes.
        format!("Key code {key}")
    } else {
        key.to_string()
    };
    match modifier.trim() {
        "" | "NONE" => name,
        m => {
            let mut c = m.chars();
            let m: String = c.next().map(|f| f.to_string() + &c.as_str().to_lowercase()).unwrap_or_default();
            let m = if m == "Control" { "Ctrl".to_string() } else { m };
            format!("{m} + {name}")
        }
    }
}

/// Minecraft's name for a key pressed in Hopper ("a" → "key.keyboard.a",
/// "pageup" → "key.keyboard.page.up"), or None for keys it has no name for.
pub fn key_from_name(name: &str) -> Option<String> {
    let n = name.to_lowercase();
    let k = match n.as_str() {
        c if c.len() == 1 && c.chars().all(|c| c.is_ascii_alphanumeric()) => c.to_string(),
        f if f.len() <= 3 && f.starts_with('f') && f[1..].parse::<u8>().is_ok_and(|n| (1..=25).contains(&n)) => f.to_string(),
        "space" | "enter" | "tab" | "backspace" | "delete" | "insert" | "home" | "end" | "up" | "down" | "left" | "right"
        | "escape" | "pause" | "menu" => n.clone(),
        "pageup" => "page.up".into(),
        "pagedown" => "page.down".into(),
        "capslock" => "caps.lock".into(),
        "shift" => "left.shift".into(),
        "control" | "ctrl" => "left.control".into(),
        "alt" => "left.alt".into(),
        "platform" | "cmd" | "win" => "left.win".into(),
        "`" => "grave.accent".into(),
        "-" => "minus".into(),
        "=" => "equal".into(),
        "[" => "left.bracket".into(),
        "]" => "right.bracket".into(),
        "\\" => "backslash".into(),
        ";" => "semicolon".into(),
        "'" => "apostrophe".into(),
        "," => "comma".into(),
        "." => "period".into(),
        "/" => "slash".into(),
        _ => return None,
    };
    Some(format!("key.keyboard.{k}"))
}

/// Minecraft's own key for each vanilla action, shown until the player
/// sets one.
pub const DEFAULT_KEYS: &[(&str, &str)] = &[
    ("key.attack", "key.mouse.left"),
    ("key.use", "key.mouse.right"),
    ("key.forward", "key.keyboard.w"),
    ("key.left", "key.keyboard.a"),
    ("key.back", "key.keyboard.s"),
    ("key.right", "key.keyboard.d"),
    ("key.jump", "key.keyboard.space"),
    ("key.sneak", "key.keyboard.left.shift"),
    ("key.sprint", "key.keyboard.left.control"),
    ("key.drop", "key.keyboard.q"),
    ("key.inventory", "key.keyboard.e"),
    ("key.swapOffhand", "key.keyboard.f"),
    ("key.pickItem", "key.mouse.middle"),
    ("key.chat", "key.keyboard.t"),
    ("key.command", "key.keyboard.slash"),
    ("key.playerlist", "key.keyboard.tab"),
    ("key.advancements", "key.keyboard.l"),
    ("key.socialInteractions", "key.keyboard.p"),
    ("key.screenshot", "key.keyboard.f2"),
    ("key.togglePerspective", "key.keyboard.f5"),
    ("key.smoothCamera", "key.keyboard.unknown"),
    ("key.fullscreen", "key.keyboard.f11"),
    ("key.saveToolbarActivator", "key.keyboard.c"),
    ("key.loadToolbarActivator", "key.keyboard.x"),
    ("key.hotbar.1", "key.keyboard.1"),
    ("key.hotbar.2", "key.keyboard.2"),
    ("key.hotbar.3", "key.keyboard.3"),
    ("key.hotbar.4", "key.keyboard.4"),
    ("key.hotbar.5", "key.keyboard.5"),
    ("key.hotbar.6", "key.keyboard.6"),
    ("key.hotbar.7", "key.keyboard.7"),
    ("key.hotbar.8", "key.keyboard.8"),
    ("key.hotbar.9", "key.keyboard.9"),
];

/// A vanilla action's name ("key.forward" → "Walk forwards"); mod actions
/// keep their id.
pub fn action_label(action: &str) -> String {
    let a = action.strip_prefix("key.").unwrap_or(action);
    if let Some(n) = a.strip_prefix("hotbar.") {
        return format!("Hotbar slot {n}");
    }
    let name = match a {
        "attack" => "Attack / Destroy",
        "use" => "Use item / Place block",
        "forward" => "Walk forwards",
        "back" => "Walk backwards",
        "left" => "Strafe left",
        "right" => "Strafe right",
        "jump" => "Jump",
        "sneak" => "Sneak",
        "sprint" => "Sprint",
        "drop" => "Drop selected item",
        "inventory" => "Open / Close inventory",
        "chat" => "Open chat",
        "command" => "Open command",
        "playerlist" => "List players",
        "pickItem" => "Pick block",
        "swapOffhand" => "Swap item with offhand",
        "screenshot" => "Take screenshot",
        "togglePerspective" => "Toggle perspective",
        "smoothCamera" => "Toggle cinematic camera",
        "fullscreen" => "Toggle fullscreen",
        "spectatorOutlines" => "Highlight players (spectators)",
        "spectatorHotbar" => "Spectator hotbar",
        "advancements" => "Advancements",
        "socialInteractions" => "Social interactions",
        "saveToolbarActivator" => "Save hotbar",
        "loadToolbarActivator" => "Load hotbar",
        "quickActions" => "Quick actions",
        "toggleGui" => "Toggle GUI",
        "toggleSpectatorShaderEffects" => "Toggle spectator shaders",
        _ => return humanize(action),
    };
    name.to_string()
}

/// Vanilla actions with no key of their own, besides [`DEFAULT_KEYS`].
const VANILLA_UNBOUND: &[&str] = &[
    "key.spectatorOutlines",
    "key.spectatorHotbar",
    "key.quickActions",
    "key.toggleGui",
    "key.toggleSpectatorShaderEffects",
];

/// Whether Minecraft itself has this action. A list, not the id's shape:
/// mods name theirs `key.ftbultimine` as well as `key.jei.showRecipe`.
pub fn vanilla_action(action: &str) -> bool {
    DEFAULT_KEYS.iter().any(|(a, _)| *a == action) || VANILLA_UNBOUND.contains(&action)
}

/// The mod a key belongs to, from its id ("key.jei.showRecipe" → "jei",
/// "accessories.key.open_screen" → "accessories"), or "" when it can't tell.
pub fn action_mod(action: &str) -> &str {
    if vanilla_action(action) {
        return "";
    }
    let a = action.strip_prefix("key.").unwrap_or(action);
    match a.split_once('.') {
        Some((m, _)) => m,
        None if a != action => a,
        None => "",
    }
}

/// A mod key's id as words: "key.jei.showRecipe" → "Show recipe",
/// "artifacts.key.helium_flamingo.activate" → "Helium flamingo activate".
fn humanize(action: &str) -> String {
    let m = action_mod(action);
    let a = action.strip_prefix("key.").unwrap_or(action);
    let rest = a.strip_prefix(m).map(|r| r.trim_start_matches('.')).unwrap_or(a);
    let rest = if rest.is_empty() { a } else { rest };
    let mut words = String::new();
    let mut prev_lower = false;
    for part in rest.split('.').filter(|p| !matches!(*p, "key" | "keys" | "keybind" | "keybinds" | "keybinding")) {
        if !words.is_empty() {
            words.push(' ');
        }
        for c in part.chars() {
            if c == '_' || c == '-' {
                words.push(' ');
                prev_lower = false;
                continue;
            }
            if c.is_uppercase() && prev_lower {
                words.push(' ');
            }
            prev_lower = c.is_lowercase() || c.is_ascii_digit();
            words.extend(c.to_lowercase());
        }
    }
    let words = words.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut c = words.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => action.to_string(),
    }
}

// ---- Minecraft versions --------------------------------------------------------

/// The minor version of "1.21.1" (21), or None for snapshots and odd ids.
pub fn minor(game_version: &str) -> Option<u32> {
    let mut it = game_version.trim().split('.');
    (it.next()? == "1").then_some(())?;
    it.next()?.parse().ok()
}

/// The data version of `1.<minor>.0`, written at the top of an
/// `options.txt` Hopper makes. Minecraft upgrades older files from it;
/// the first release of the minor is low enough that every fix it needs
/// still runs, and the values Hopper writes are already in today's form.
fn data_version(minor: u32) -> Option<u32> {
    Some(match minor {
        12 => 1139,
        13 => 1519,
        14 => 1952,
        15 => 2225,
        16 => 2566,
        17 => 2724,
        18 => 2860,
        19 => 3105,
        20 => 3463,
        21 => 3953,
        m if m > 21 => 3953,
        _ => return None,
    })
}

/// Whether a key value fits the version: 1.13 and newer name keys
/// (`key.keyboard.w`), 1.12 and older use numbers.
fn key_fits(value: &str, minor: Option<u32>) -> bool {
    let numeric = value.split(':').next().is_some_and(|k| !k.is_empty() && k.chars().all(|c| c.is_ascii_digit() || c == '-'));
    match minor {
        Some(m) if m < 13 => numeric,
        Some(_) => !numeric,
        // Snapshots and the like: today's form.
        None => !numeric,
    }
}

// ---- options.txt ---------------------------------------------------------------

/// The file's lines, and where each key is.
struct OptionsFile {
    lines: Vec<String>,
    /// Written with Windows line ends, kept so an unchanged file is
    /// written back the same.
    crlf: bool,
}

impl OptionsFile {
    fn parse(text: &str) -> Self {
        OptionsFile {
            lines: text.lines().map(|l| l.trim_end_matches('\r').to_string()).collect(),
            crlf: text.contains("\r\n"),
        }
    }

    fn find(&self, key: &str) -> Option<usize> {
        self.lines.iter().position(|l| l.split_once(':').is_some_and(|(k, _)| k == key))
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.find(key).and_then(|i| self.lines[i].split_once(':')).map(|(_, v)| v)
    }

    /// Sets `key`, in its place if it's there, at the end if not.
    fn set(&mut self, key: &str, value: &str) {
        let line = format!("{key}:{value}");
        match self.find(key) {
            Some(i) => self.lines[i] = line,
            None => self.lines.push(line),
        }
    }

    fn text(&self) -> String {
        let end = if self.crlf { "\r\n" } else { "\n" };
        let mut s = self.lines.join(end);
        s.push_str(end);
        s
    }
}

/// Where a pack keeps the options its mod copies in on the first start.
fn pack_defaults(root: &Path) -> Option<(PathBuf, &'static str)> {
    [
        (root.join("config").join("defaultoptions").join("options.txt"), "DefaultOptions"),
        (root.join("config").join("yosbr").join("options.txt"), "YOSBR"),
    ]
    .into_iter()
    .find(|(p, _)| p.is_file())
}

/// The pack's own first-start options: its options file, with the
/// DefaultOptions key bindings (`key_key.x:key.keyboard.w:` with a
/// modifier or nothing after the last colon) in `options.txt`'s form.
fn seed(root: &Path) -> Option<(String, &'static str)> {
    let (path, by) = pack_defaults(root)?;
    let mut text = std::fs::read_to_string(&path).ok()?;
    let keys = root.join("config").join("defaultoptions").join("keybindings.txt");
    if by == "DefaultOptions"
        && let Ok(k) = std::fs::read_to_string(keys)
    {
        let mut file = OptionsFile::parse(&text);
        for line in k.lines() {
            let line = line.trim();
            let Some((key, rest)) = line.split_once(':') else { continue };
            if !key.starts_with("key_") || file.find(key).is_some() {
                continue;
            }
            let rest = rest.strip_suffix(':').unwrap_or(rest);
            let rest = rest.strip_suffix(":NONE").unwrap_or(rest);
            file.set(key, rest);
        }
        text = file.text();
    }
    Some((text, by))
}

/// What applying did, for the launch's progress line and the log.
#[derive(Debug, Default, PartialEq)]
pub struct Applied {
    /// How many options were written.
    pub written: usize,
    /// Mod keys left for a later start, when the game has registered them.
    pub waiting: usize,
    /// The file was made, from the pack's defaults when it has them
    /// ("DefaultOptions"), else "".
    pub created: Option<&'static str>,
}

/// What Hopper wants written for this version: (key, value, every start).
fn wanted(opts: &McOptions, minor: Option<u32>, forge: bool) -> Vec<(String, String, bool)> {
    let mut out = Vec::new();
    for spec in SPECS {
        let Some(v) = opts.values.get(spec.key).filter(|v| v.on) else { continue };
        if minor.is_some_and(|m| m < spec.since) {
            continue;
        }
        let value = match spec.kind {
            Kind::Fixed(f) => f.to_string(),
            _ if v.value.trim().is_empty() => continue,
            _ => v.value.trim().to_string(),
        };
        // 1.20 asks about accessibility on the first start; narrator off
        // says the player doesn't need it.
        if spec.key == "narrator" && minor.is_none_or(|m| m >= 20) {
            out.push(("onboardAccessibility".into(), "false".into(), v.always));
        }
        out.push((spec.key.to_string(), value, v.always));
    }
    if opts.keys_on {
        for (action, key) in &opts.keys {
            if !key_fits(key, minor) {
                continue;
            }
            // Only NeoForge and Forge know key modifiers ("…:SHIFT");
            // elsewhere the whole value reads as no key at all.
            let key = match key.split_once(':') {
                Some((k, _)) if !forge => k.to_string(),
                _ => key.clone(),
            };
            out.push((format!("key_{action}"), key, opts.keys_always));
        }
    }
    out
}

/// Writes the player's settings into the instance's `options.txt`.
/// `applied` is what Hopper wrote there before, by key; it's updated.
pub fn apply(
    root: &Path,
    game_version: &str,
    loader: &str,
    opts: &McOptions,
    applied: &mut BTreeMap<String, String>,
) -> anyhow::Result<Applied> {
    let mut out = Applied::default();
    if !opts.enabled {
        return Ok(out);
    }
    let minor = minor(game_version);
    let forge = loader.starts_with("forge") || loader.starts_with("neoforge");
    let want = wanted(opts, minor, forge);
    if want.is_empty() {
        return Ok(out);
    }
    let path = root.join("options.txt");
    let (mut file, before) = match std::fs::read_to_string(&path) {
        Ok(t) => (OptionsFile::parse(&t), Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let (text, by) = seed(root).unwrap_or_default();
            let mut f = OptionsFile::parse(&text);
            if f.get("version").is_none()
                && let Some(dv) = minor.and_then(data_version)
            {
                f.lines.insert(0, format!("version:{dv}"));
            }
            out.created = Some(by);
            (f, None)
        }
        Err(e) => return Err(anyhow::Error::new(e).context(format!("reading {}", path.display()))),
    };
    for (key, value, always) in want {
        let current = file.get(&key).map(str::to_string);
        // A mod's key only once its line is there: the game writes it after
        // a start with the mod installed (or the pack's defaults have it).
        // Until then it waits, so packs without the mod never get it.
        if let Some(action) = key.strip_prefix("key_")
            && !vanilla_action(action)
            && current.is_none()
        {
            out.waiting += 1;
            continue;
        }
        // First start only: once Hopper has given this value, what's in the
        // file is the player's.
        if !always && applied.get(&key) == Some(&value) {
            continue;
        }
        if current.as_deref() != Some(value.as_str()) {
            file.set(&key, &value);
            out.written += 1;
        }
        applied.insert(key, value);
    }
    let text = file.text();
    if before.as_deref() != Some(text.as_str()) {
        let tmp = path.with_extension("txt.hopper-tmp");
        std::fs::write(&tmp, &text).map_err(|e| anyhow::Error::new(e).context(format!("writing {}", tmp.display())))?;
        std::fs::rename(&tmp, &path).map_err(|e| anyhow::Error::new(e).context(format!("writing {}", path.display())))?;
    }
    Ok(out)
}

/// The options and key bindings in an instance's `options.txt`, to copy
/// into Hopper's settings: (options by key, keys by action).
pub fn read_instance(root: &Path) -> anyhow::Result<(BTreeMap<String, String>, BTreeMap<String, String>)> {
    let path = root.join("options.txt");
    let text = std::fs::read_to_string(&path).map_err(|e| anyhow::Error::new(e).context(format!("reading {}", path.display())))?;
    Ok(read_text(&text))
}

fn read_text(text: &str) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let file = OptionsFile::parse(text);
    let mut values = BTreeMap::new();
    let mut keys = BTreeMap::new();
    for line in &file.lines {
        let Some((k, v)) = line.split_once(':') else { continue };
        if let Some(action) = k.strip_prefix("key_") {
            keys.insert(action.to_string(), v.to_string());
        } else if let Some(spec) = spec(k) {
            match spec.kind {
                // A switch that writes one value is on only if the pack
                // has that value.
                Kind::Fixed(f) if v.trim() != f => {}
                _ => {
                    values.insert(k.to_string(), v.to_string());
                }
            }
        }
    }
    (values, keys)
}

impl McOptions {
    /// Takes what [`read_instance`] found: every option found is turned on
    /// with its value (keeping each one's "every start"), and the keys
    /// replace Hopper's.
    pub fn import(&mut self, from: &str, values: BTreeMap<String, String>, keys: BTreeMap<String, String>) {
        for (k, v) in values {
            let e = self.values.entry(k).or_default();
            e.value = v;
            e.on = true;
        }
        if !keys.is_empty() {
            self.keys = keys;
            self.keys_on = true;
            self.keys_from = from.to_string();
        }
    }

    /// A setting's value, or its default when it was never set.
    pub fn value(&self, spec: &Spec) -> McValue {
        self.values.get(spec.key).cloned().unwrap_or_else(|| McValue { value: spec.default.to_string(), ..Default::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V1_21: &str = "version:3955\nao:true\nfov:0.375\nrenderDistance:8\nlastServer:play.example.com:25565\nsoundCategory_master:0.03716628725165556\nkey_key.forward:key.keyboard.w\nkey_key.jei.showRecipe:key.keyboard.r\nmodelPart_cape:true\n";

    fn opts(pairs: &[(&str, &str, bool)]) -> McOptions {
        let mut o = McOptions { enabled: true, ..Default::default() };
        for (k, v, always) in pairs {
            o.values.insert(k.to_string(), McValue { value: v.to_string(), on: true, always: *always });
        }
        o
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopper-mcopt-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn fov_and_volume_scales() {
        assert_eq!(shown(Kind::Fov, "0.375"), 85.);
        assert_eq!(stored(Kind::Fov, 85.), "0.375");
        assert_eq!(stored(Kind::Fov, 70.), "0.0");
        assert_eq!(stored(Kind::Fov, 110.), "1.0");
        assert_eq!(stored(Kind::Fov, 30.), "-1.0");
        assert_eq!(stored(Kind::Fov, 200.), "1.0");
        assert_eq!(shown(Kind::Volume, "0.03716628725165556"), 4.);
        assert_eq!(stored(Kind::Volume, 35.), "0.35");
        assert_eq!(shown(Kind::Sensitivity, "0.29"), 58.);
        assert_eq!(stored(Kind::Sensitivity, 100.), "0.5");
        assert_eq!(label(Kind::GuiScale, "0"), "Auto");
        assert_eq!(label(Kind::MaxFps, "260"), "Unlimited");
        assert_eq!(stored(Kind::Number { min: 2, max: 32 }, 40.), "32");
    }

    #[test]
    fn keeps_unknown_lines_and_order() {
        let d = dir("order");
        std::fs::write(d.join("options.txt"), V1_21).unwrap();
        let o = opts(&[("fov", "0.5", false), ("soundCategory_music", "0.15", false)]);
        let mut applied = BTreeMap::new();
        let r = apply(&d, "1.21.1", "neoforge-21.1.200", &o, &mut applied).unwrap();
        assert_eq!(r.written, 2);
        let t = std::fs::read_to_string(d.join("options.txt")).unwrap();
        assert_eq!(
            t,
            "version:3955\nao:true\nfov:0.5\nrenderDistance:8\nlastServer:play.example.com:25565\nsoundCategory_master:0.03716628725165556\nkey_key.forward:key.keyboard.w\nkey_key.jei.showRecipe:key.keyboard.r\nmodelPart_cape:true\nsoundCategory_music:0.15\n"
        );
        assert_eq!(applied.get("fov").map(String::as_str), Some("0.5"));
    }

    #[test]
    fn first_start_only_lets_the_game_win_after() {
        let d = dir("first");
        std::fs::write(d.join("options.txt"), V1_21).unwrap();
        let o = opts(&[("fov", "0.5", false), ("renderDistance", "16", true)]);
        let mut applied = BTreeMap::new();
        apply(&d, "1.21.1", "neoforge-21.1.200", &o, &mut applied).unwrap();
        // The player changes both in game.
        let t = std::fs::read_to_string(d.join("options.txt")).unwrap().replace("fov:0.5", "fov:0.0").replace("renderDistance:16", "renderDistance:6");
        std::fs::write(d.join("options.txt"), t).unwrap();
        let r = apply(&d, "1.21.1", "neoforge-21.1.200", &o, &mut applied).unwrap();
        assert_eq!(r.written, 1);
        let t = std::fs::read_to_string(d.join("options.txt")).unwrap();
        assert!(t.contains("fov:0.0\n"), "first-start value stays the player's");
        assert!(t.contains("renderDistance:16\n"), "every-start value is put back");
        // A new value in Hopper goes out again.
        let o = opts(&[("fov", "0.25", false), ("renderDistance", "16", true)]);
        apply(&d, "1.21.1", "neoforge-21.1.200", &o, &mut applied).unwrap();
        assert!(std::fs::read_to_string(d.join("options.txt")).unwrap().contains("fov:0.25\n"));
    }

    #[test]
    fn untouched_file_when_nothing_changes() {
        let d = dir("same");
        let text = V1_21.replace('\n', "\r\n");
        std::fs::write(d.join("options.txt"), &text).unwrap();
        let mut applied = BTreeMap::new();
        let o = opts(&[("fov", "0.375", true)]);
        let r = apply(&d, "1.21.1", "neoforge-21.1.200", &o, &mut applied).unwrap();
        assert_eq!(r.written, 0);
        assert_eq!(std::fs::read_to_string(d.join("options.txt")).unwrap(), text);
        // Off: nothing at all.
        let mut off = o.clone();
        off.enabled = false;
        assert_eq!(apply(&d, "1.21.1", "neoforge-21.1.200", &off, &mut BTreeMap::new()).unwrap(), Applied::default());
    }

    #[test]
    fn fresh_pack_gets_a_file_with_a_version() {
        let d = dir("fresh");
        let mut o = opts(&[("fov", "0.375", false), ("narrator", "", false), ("simulationDistance", "8", false)]);
        o.keys_on = true;
        o.keys.insert("key.drop".into(), "key.keyboard.g".into());
        o.keys.insert("key.jei.showRecipe".into(), "key.keyboard.r".into());
        o.keys.insert("key.ftbultimine".into(), "key.keyboard.v".into());
        let r = apply(&d, "1.21.1", "neoforge-21.1.200", &o, &mut BTreeMap::new()).unwrap();
        assert_eq!(r.created, Some(""));
        assert_eq!(r.waiting, 2, "mod keys wait for the game to register them");
        let t = std::fs::read_to_string(d.join("options.txt")).unwrap();
        assert_eq!(t, "version:3953\nfov:0.375\nsimulationDistance:8\nonboardAccessibility:false\nnarrator:0\nkey_key.drop:key.keyboard.g\n");
    }

    #[test]
    fn mod_keys_only_where_the_mod_is() {
        let d = dir("modkeys");
        std::fs::write(d.join("options.txt"), V1_21).unwrap();
        let mut o = opts(&[]);
        o.keys_on = true;
        o.keys.insert("key.jei.showRecipe".into(), "key.keyboard.u".into());
        o.keys.insert("key.zoomify.zoom".into(), "key.keyboard.c".into());
        let r = apply(&d, "1.21.1", "neoforge-21.1.200", &o, &mut BTreeMap::new()).unwrap();
        assert_eq!((r.written, r.waiting), (1, 1));
        let t = std::fs::read_to_string(d.join("options.txt")).unwrap();
        assert!(t.contains("key_key.jei.showRecipe:key.keyboard.u\n"));
        assert!(!t.contains("zoomify"));
    }

    #[test]
    fn old_versions_skip_what_they_lack() {
        let d = dir("old");
        std::fs::write(d.join("options.txt"), "version:1343\nfov:0.0\nkey_key.forward:17\n").unwrap();
        let mut o = opts(&[("simulationDistance", "8", false), ("fov", "0.25", false)]);
        o.keys_on = true;
        o.keys.insert("key.forward".into(), "key.keyboard.w".into());
        o.keys.insert("key.back".into(), "31".into());
        apply(&d, "1.12.2", "forge-14.23.5", &o, &mut BTreeMap::new()).unwrap();
        assert_eq!(std::fs::read_to_string(d.join("options.txt")).unwrap(), "version:1343\nfov:0.25\nkey_key.forward:17\nkey_key.back:31\n");
        // And the other way round: number keys don't go into 1.20.
        assert_eq!(wanted(&o, Some(20), true).iter().filter(|w| w.0.starts_with("key_")).count(), 1);
    }

    #[test]
    fn key_modifiers_only_for_forge() {
        let mut o = opts(&[]);
        o.keys_on = true;
        o.keys.insert("key.drop".into(), "key.keyboard.g:SHIFT".into());
        let forge = wanted(&o, Some(21), true);
        let fabric = wanted(&o, Some(21), false);
        assert_eq!(forge[0].1, "key.keyboard.g:SHIFT");
        assert_eq!(fabric[0].1, "key.keyboard.g");
    }

    #[test]
    fn starts_from_the_packs_defaultoptions() {
        let d = dir("defaultoptions");
        let cfg = d.join("config").join("defaultoptions");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(cfg.join("options.txt"), "guiScale:2\ndarkMojangStudiosBackground:true").unwrap();
        std::fs::write(cfg.join("keybindings.txt"), "key_key.attack:key.mouse.left:\nkey_key.ftb.chunks:key.keyboard.m:SHIFT\n").unwrap();
        let o = opts(&[("fov", "0.375", false)]);
        let r = apply(&d, "1.21.1", "neoforge-21.1.200", &o, &mut BTreeMap::new()).unwrap();
        assert_eq!(r.created, Some("DefaultOptions"));
        let t = std::fs::read_to_string(d.join("options.txt")).unwrap();
        assert_eq!(t, "version:3953\nguiScale:2\ndarkMojangStudiosBackground:true\nkey_key.attack:key.mouse.left\nkey_key.ftb.chunks:key.keyboard.m:SHIFT\nfov:0.375\n");
        // The player's value wins over the pack's.
        let o = opts(&[("guiScale", "3", false)]);
        std::fs::remove_file(d.join("options.txt")).unwrap();
        apply(&d, "1.21.1", "neoforge-21.1.200", &o, &mut BTreeMap::new()).unwrap();
        assert!(std::fs::read_to_string(d.join("options.txt")).unwrap().contains("guiScale:3\n"));
    }

    #[test]
    fn reads_an_instance_for_import() {
        let (values, keys) = read_text(&format!("{V1_21}narrator:2\ntutorialStep:movement\n"));
        assert_eq!(values.get("fov").map(String::as_str), Some("0.375"));
        assert!(!values.contains_key("ao"));
        assert!(!values.contains_key("narrator"), "narrator is on in that pack");
        assert!(!values.contains_key("tutorialStep"));
        assert_eq!(keys.len(), 2);
        let mut o = McOptions::default();
        o.values.insert("fov".into(), McValue { value: "0".into(), on: false, always: true });
        o.import("ATMons", values, keys);
        assert_eq!(o.values["fov"], McValue { value: "0.375".into(), on: true, always: true });
        assert!(o.keys_on);
        assert_eq!(o.keys_from, "ATMons");
    }

    #[test]
    fn names_for_keys_and_actions() {
        assert_eq!(key_label("key.keyboard.left.control"), "Left Control");
        assert_eq!(key_label("key.mouse.4"), "Mouse 4");
        assert_eq!(key_label("key.mouse.left"), "Left click");
        assert_eq!(key_label("key.keyboard.m:SHIFT"), "Shift + M");
        assert_eq!(key_label("key.keyboard.unknown"), "Not bound");
        assert_eq!(key_label("17"), "Key code 17");
        assert_eq!(action_label("key.forward"), "Walk forwards");
        assert_eq!(action_label("key.hotbar.3"), "Hotbar slot 3");
        assert_eq!(action_label("key.jei.showRecipe"), "Show recipe");
        assert_eq!(action_mod("key.jei.showRecipe"), "jei");
        assert_eq!(action_label("accessories.key.open_accessories_screen"), "Open accessories screen");
        assert_eq!(action_mod("accessories.key.open_accessories_screen"), "accessories");
        assert_eq!(action_label("artifacts.key.helium_flamingo.activate"), "Helium flamingo activate");
        assert_eq!(action_label("key.ftbultimine"), "Ftbultimine");
        assert_eq!(action_mod("key.ftbultimine"), "ftbultimine");
        assert_eq!(action_label("Mask Toggle"), "Mask toggle");
        assert_eq!(action_mod("Mask Toggle"), "");
        assert_eq!(action_mod("key.forward"), "");
        assert!(vanilla_action("key.forward"));
        assert!(vanilla_action("key.hotbar.9"));
        assert!(!vanilla_action("key.jei.showRecipe"));
        assert!(!vanilla_action("key.ftbultimine"));
        assert_eq!(key_from_name("G").as_deref(), Some("key.keyboard.g"));
        assert_eq!(key_from_name("pageup").as_deref(), Some("key.keyboard.page.up"));
        assert_eq!(key_from_name("f12").as_deref(), Some("key.keyboard.f12"));
        assert_eq!(key_from_name("shift").as_deref(), Some("key.keyboard.left.shift"));
        assert_eq!(key_from_name("/").as_deref(), Some("key.keyboard.slash"));
        assert_eq!(key_from_name("fn"), None);
        assert!(DEFAULT_KEYS.iter().all(|(a, _)| vanilla_action(a) && action_label(a) != *a));
        assert_eq!(minor("1.21.1"), Some(21));
        assert_eq!(minor("24w14a"), None);
    }
}
