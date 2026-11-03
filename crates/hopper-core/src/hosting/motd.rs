//! The server list message (MOTD) as a template: `{pack_version}`,
//! `{day}`, `{players}` and the rest fill in from the server's files, so
//! the message stays right after a pack update. The template is kept in
//! [`FILE`] in the server folder; server.properties gets the filled-in
//! message at every start (Minecraft reads it only then). A panel that
//! answers the server list itself (the router) fills it in fresh.
//!
//! `{{` and `}}` are a literal brace; an unknown `{name}` stays as typed.

use super::properties::Properties;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// In the server folder: the template and the message last written from it.
pub const FILE: &str = ".hopper-motd.json";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    template: String,
    /// What went into server.properties from it. When the file says
    /// something else, someone changed the message there by hand, and that
    /// wins.
    written: String,
}

/// What a running server knows that its files don't (yet).
#[derive(Clone, Debug, Default)]
pub struct Live {
    pub players: Option<u32>,
    pub max_players: Option<u32>,
    pub day: Option<i64>,
}

/// Variables that change while the server runs: worth filling in again.
pub const DYNAMIC: &[&str] = &["day", "players", "max_players", "world_size"];

/// `&6` typed as a shortcut becomes `§6`, only before a real code (so
/// "Tom & Jerry" stays). A typed `\n` is a line break, as in
/// server.properties.
pub fn normalize(s: &str) -> String {
    amp_to_section(s).replace("\\n", "\n")
}

/// `{name}` names in `t` (not escaped ones).
fn names(t: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let b = t.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if (b[i] == b'{' || b[i] == b'}') && b.get(i + 1) == Some(&b[i]) {
            i += 2;
            continue;
        }
        if b[i] == b'{'
            && let Some(len) = t[i + 1..].find('}')
        {
            let n = &t[i + 1..i + 1 + len];
            if !n.is_empty() && n.bytes().all(|c| c.is_ascii_lowercase() || c == b'_') {
                out.push(n);
                i += len + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// The template uses a variable (else it's a plain message).
pub fn has_vars(t: &str) -> bool {
    !names(t).is_empty() || t.contains("{{") || t.contains("}}")
}

/// The template uses something that changes while the server runs.
pub fn is_dynamic(t: &str) -> bool {
    names(t).iter().any(|n| DYNAMIC.contains(n))
}

/// Fills in `t`'s variables from `vars`.
pub fn expand(t: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(t.len());
    let mut rest = t;
    while let Some(c) = rest.chars().next() {
        if rest.starts_with("{{") || rest.starts_with("}}") {
            out.push(c);
            rest = &rest[2..];
            continue;
        }
        if c == '{'
            && let Some(len) = rest[1..].find('}')
            && let Some(v) = vars.get(&rest[1..1 + len])
        {
            out.push_str(v);
            rest = &rest[len + 2..];
            continue;
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// "Limitless 8 - v1.78.1" → "v1.78.1": the pack's version without its
/// name.
fn short_version(pack: &str, version: &str) -> String {
    let v = version.trim();
    let v = v.strip_prefix(pack.trim()).unwrap_or(v).trim_start_matches([' ', '-', '_', '|', ':']).trim();
    if !v.contains(char::is_whitespace) {
        return v.to_string();
    }
    v.split_whitespace().rev().find(|w| w.chars().any(|c| c.is_ascii_digit())).unwrap_or(v).to_string()
}

/// "TNP-Limitless-8-Server-1.78.1.zip" → "1.78.1": the last dotted number.
fn version_in_file_name(f: &str) -> Option<String> {
    let f = f.trim_end_matches(".zip");
    f.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map(|p| p.trim_matches('.'))
        .filter(|p| p.contains('.') && p.split('.').all(|x| !x.is_empty()))
        .last()
        .map(str::to_string)
}

/// Every variable's value for the server in `root`.
pub fn vars(root: &Path, live: &Live) -> BTreeMap<String, String> {
    let mut v = BTreeMap::new();
    let mut put = |k: &str, s: String| {
        v.insert(k.to_string(), s);
    };
    let props = Properties::load(&root.join("server.properties")).unwrap_or_default();
    let prop = |k: &str, d: &str| props.get(k).map(str::trim).filter(|s| !s.is_empty()).unwrap_or(d).to_string();
    let start = super::script::start_info(root);
    let meta = super::HostedMeta::load(root);
    let inst = crate::instances::load(root);

    put("name", crate::server::load(root).name);
    // The pack: Hopper's own record, else the server pack it came from.
    let marker = crate::server_pack_update::recorded(root);
    if let Some(p) = inst.as_ref().and_then(|i| i.pack.as_ref()).filter(|p| !p.name.is_empty()) {
        put("pack", p.name.clone());
        let ver = short_version(&p.name, &p.version);
        let ver = if ver.is_empty() { version_in_file_name(&p.file_name).unwrap_or_default() } else { ver };
        put("pack_version", ver);
    } else if let Some(m) = &marker {
        put("pack_version", version_in_file_name(&m.server_pack).unwrap_or_default());
    }

    let mc = meta
        .as_ref()
        .map(|m| m.minecraft.clone())
        .filter(|m| !m.is_empty())
        .or(start.minecraft.clone())
        .or_else(|| inst.as_ref().map(|i| i.game_version.clone()))
        .unwrap_or_default();
    put("mc", mc);
    // "neoforge 21.1.259" from the start script, "NeoForge 21.1.259" from
    // Hopper's record, "neoforge-21.1.259" from the mod list's.
    let loader = start
        .loader
        .clone()
        .or_else(|| meta.as_ref().map(|m| m.loader.clone()).filter(|l| !l.is_empty()))
        .or_else(|| inst.as_ref().map(|i| i.loader.replacen('-', " ", 1)))
        .unwrap_or_default();
    let mut parts = loader.split_whitespace();
    let id = parts.next().unwrap_or("").to_lowercase();
    let ver = parts.next().unwrap_or("").to_string();
    let pretty = match id.as_str() {
        "neoforge" => "NeoForge",
        "forge" => "Forge",
        "fabric" => "Fabric",
        "quilt" => "Quilt",
        "paper" => "Paper",
        "purpur" => "Purpur",
        "" | "vanilla" => "Vanilla",
        _ => "",
    };
    let pretty = if pretty.is_empty() { id.clone() } else { pretty.to_string() };
    put("loader", if ver.is_empty() { pretty } else { format!("{pretty} {ver}") });
    put("loader_version", ver.clone());
    // {neoforge}, {fabric}…: the version of that loader.
    if !id.is_empty() && id != "vanilla" && !ver.is_empty() {
        put(&id, ver);
    }

    // Mods: jars that are on. The pack's are the ones Hopper recorded as
    // part of it; the rest were added on top.
    let jars: Vec<String> = std::fs::read_dir(root.join("mods"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.to_lowercase().ends_with(".jar"))
        .collect();
    put("mods", jars.len().to_string());
    // Without a pack nothing is extra; with one whose mods can't be
    // told apart, no number beats a wrong one.
    let extra = match &inst {
        Some(i) if i.pack.is_some() || marker.is_some() => {
            crate::modpack::PackMods::of(i).map(|p| extra_mods(&i.addons, &jars, &p))
        }
        _ => Some(0),
    };
    put("extra_mods", extra.map(|n| n.to_string()).unwrap_or_default());

    put("difficulty", prop("difficulty", "easy"));
    put("gamemode", prop("gamemode", "survival"));
    put("players", live.players.unwrap_or(0).to_string());
    put("max_players", live.max_players.map(|m| m.to_string()).unwrap_or_else(|| prop("max-players", "20")));

    let world = root.join(super::worlds::level_name(root));
    let day = live.day.or_else(|| super::worlds::read_level(&world.join("level.dat")).day_time.map(|t| t / 24000));
    put("day", day.unwrap_or(0).to_string());
    put("world_size", crate::instance_ops::human_bytes(super::worlds::world_size(&world)));
    v
}

/// Mods added on top of the pack, counted like the Mods tab's "Only
/// yours": mods Hopper knows (jars it couldn't name aren't counted; packs
/// ship some of those themselves) that are on and aren't the pack's.
fn extra_mods(addons: &[crate::instances::Addon], jars: &[String], pack: &crate::modpack::PackMods) -> usize {
    let on: std::collections::HashSet<String> = jars.iter().map(|j| j.to_lowercase()).collect();
    addons.iter().filter(|a| a.folder() == "mods" && on.contains(&a.file_name.to_lowercase()) && !pack.has(a)).count()
}

fn load_saved(root: &Path) -> Option<Saved> {
    serde_json::from_str(&std::fs::read_to_string(root.join(FILE)).ok()?).ok()
}

fn props_motd(root: &Path) -> String {
    Properties::load(&root.join("server.properties")).ok().and_then(|p| p.get("motd").map(str::to_string)).unwrap_or_default()
}

/// The message as the player edits it: the saved template, unless
/// server.properties' message was changed elsewhere since (then that).
pub fn template(root: &Path) -> String {
    let now = props_motd(root);
    match load_saved(root) {
        Some(s) if s.written == now => s.template,
        _ => now,
    }
}

/// Saves `t` (shortcuts turned into § codes) as the server's message and
/// writes it, filled in, to server.properties. A message without
/// variables is just written; the template file goes.
pub fn save(root: &Path, t: &str) -> anyhow::Result<String> {
    let t = normalize(t.trim());
    let out = if has_vars(&t) { expand(&t, &vars(root, &Live::default())) } else { t.clone() };
    let path = root.join("server.properties");
    let mut p = Properties::load(&path)?;
    p.set("motd", &out);
    p.save(&path)?;
    if has_vars(&t) {
        std::fs::write(root.join(FILE), serde_json::to_string_pretty(&Saved { template: t, written: out.clone() })?)?;
    } else {
        let _ = std::fs::remove_file(root.join(FILE));
    }
    Ok(out)
}

/// The message filled in now (for the router's answer), or None when the
/// server has no template.
pub fn current(root: &Path, live: &Live) -> Option<String> {
    let t = template(root);
    has_vars(&t).then(|| expand(&t, &vars(root, live)))
}

/// Before a start: writes the template, filled in, to server.properties
/// (only when it changed).
pub fn apply(root: &Path) -> anyhow::Result<()> {
    let Some(s) = load_saved(root) else { return Ok(()) };
    let now = props_motd(root);
    if s.written != now {
        // Changed by hand: the file's message is the message now.
        let _ = std::fs::remove_file(root.join(FILE));
        return Ok(());
    }
    let out = expand(&s.template, &vars(root, &Live::default()));
    if out != now {
        let path = root.join("server.properties");
        let mut p = Properties::load(&path)?;
        p.set("motd", &out);
        p.save(&path)?;
        std::fs::write(root.join(FILE), serde_json::to_string_pretty(&Saved { template: s.template, written: out })?)?;
    }
    Ok(())
}

/// While the server runs: records the message the router just filled in as
/// server.properties' message, so the next start opens with the same day
/// and size the router showed (the running server keeps the one it read
/// at its start; Minecraft has no command to change it). Does nothing
/// without a template, or when the file's message was changed by hand.
pub fn remember(root: &Path, out: &str) -> anyhow::Result<()> {
    let Some(s) = load_saved(root) else { return Ok(()) };
    if s.written != props_motd(root) || s.written == out {
        return Ok(());
    }
    let path = root.join("server.properties");
    let mut p = Properties::load(&path)?;
    p.set("motd", out);
    p.save(&path)?;
    std::fs::write(root.join(FILE), serde_json::to_string_pretty(&Saved { template: s.template, written: out.to_string() })?)?;
    Ok(())
}

// ---- editing and drawing it (the panel's web UI has the same in lib/motd.js) ----

/// Minecraft's 16 colours: code, name, RGB.
pub const COLORS: [(char, &str, u32); 16] = [
    ('0', "Black", 0x000000),
    ('1', "Dark blue", 0x0000AA),
    ('2', "Dark green", 0x00AA00),
    ('3', "Dark aqua", 0x00AAAA),
    ('4', "Dark red", 0xAA0000),
    ('5', "Dark purple", 0xAA00AA),
    ('6', "Gold", 0xFFAA00),
    ('7', "Gray", 0xAAAAAA),
    ('8', "Dark gray", 0x555555),
    ('9', "Blue", 0x5555FF),
    ('a', "Green", 0x55FF55),
    ('b', "Aqua", 0x55FFFF),
    ('c', "Red", 0xFF5555),
    ('d', "Light purple", 0xFF55FF),
    ('e', "Yellow", 0xFFFF55),
    ('f', "White", 0xFFFFFF),
];

/// Styles: code, name, button label.
pub const FORMATS: [(char, &str, &str); 6] = [
    ('l', "Bold", "B"),
    ('o', "Italic", "I"),
    ('n', "Underline", "U"),
    ('m', "Strikethrough", "S"),
    ('k', "Obfuscated (scrambled letters)", "?"),
    ('r', "Clear formatting", "R"),
];

/// The variables, with what each means.
pub const VARS: [(&str, &str); 14] = [
    ("pack", "Modpack name"),
    ("pack_version", "Modpack version"),
    ("mc", "Minecraft version"),
    ("loader", "Loader and version"),
    ("loader_version", "Loader version only"),
    ("mods", "Mods installed"),
    ("extra_mods", "Mods added on top of the pack"),
    ("players", "Players online"),
    ("max_players", "Player slots"),
    ("day", "In-game day"),
    ("world_size", "World size on disk"),
    ("difficulty", "Difficulty"),
    ("gamemode", "Game mode"),
    ("name", "Server name"),
];

/// The colour text has with no code (the server list's gray).
pub const DEFAULT_COLOR: u32 = 0xAAAAAA;

fn color_of(c: char) -> Option<u32> {
    COLORS.iter().find(|x| x.0 == c).map(|x| x.2)
}

/// `&6` → `§6` (only before a real code).
pub fn amp_to_section(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match (c, it.peek()) {
            ('&', Some(n)) if n.is_ascii_hexdigit() || matches!(n.to_ascii_lowercase(), 'k'..='o' | 'r') => out.push('§'),
            _ => out.push(c),
        }
    }
    out
}

/// A stretch of text that looks the same.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Run {
    pub text: String,
    pub color: u32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub obfuscated: bool,
}

/// `text` split into [`Run`]s, as Minecraft draws it: a colour clears the
/// styles, §r clears everything.
pub fn runs(text: &str) -> Vec<Run> {
    let mut out = Vec::new();
    let mut st = Run { color: DEFAULT_COLOR, ..Default::default() };
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        if c == '§'
            && let Some(&n) = it.peek()
        {
            it.next();
            if !st.text.is_empty() {
                out.push(st.clone());
                st.text.clear();
            }
            let n = n.to_ascii_lowercase();
            match n {
                'r' => st = Run { color: DEFAULT_COLOR, ..Default::default() },
                'l' => st.bold = true,
                'o' => st.italic = true,
                'n' => st.underline = true,
                'm' => st.strike = true,
                'k' => st.obfuscated = true,
                _ => {
                    if let Some(col) = color_of(n) {
                        st = Run { color: col, ..Default::default() };
                    }
                }
            }
            continue;
        }
        st.text.push(c);
    }
    if !st.text.is_empty() {
        out.push(st);
    }
    out
}

/// Characters a line shows (codes left out).
pub fn visible(line: &str) -> usize {
    runs(line).iter().map(|r| r.text.chars().count()).sum()
}

/// How wide (in Minecraft's pixels, GUI scale 1) the server list's
/// message area is: the 305 px row less the 32 px icon and 2 px gap. The
/// game word-wraps the message at this width and shows two rows.
pub const LIST_WIDTH: u32 = 271;

/// How far a character moves the pen in Minecraft's default font: glyph
/// width plus 1 px, and 1 more in bold. Mirrored in the web UI's motd.js.
pub fn advance(c: char, bold: bool) -> u32 {
    let w = match c {
        ' ' => 4,
        '!' | '\'' | ',' | '.' | ':' | ';' | '|' | 'i' => 2,
        '`' | 'l' => 3,
        '"' | 'I' | '[' | ']' | 't' => 4,
        '(' | ')' | '*' | '<' | '>' | '{' | '}' | 'f' | 'k' => 5,
        '@' | '~' => 7,
        _ => 6,
    };
    w + bold as u32
}

/// How wide `line` (with § codes) is in the server list.
pub fn line_width(line: &str) -> u32 {
    runs(line).iter().map(|r| r.text.chars().map(|c| advance(c, r.bold)).sum::<u32>()).sum()
}

/// One message line as the server list breaks it: pieces that fit
/// [`LIST_WIDTH`], broken at the last space like the game (the space
/// goes), else mid-word. Each piece starts with the codes in force, so it
/// draws on its own.
pub fn wrap_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut piece = String::new();
    let mut width = 0;
    let mut codes = String::new();
    let mut bold = false;
    // The last space in this piece: byte index, and the codes in force there.
    let mut space: Option<(usize, String)> = None;
    let mut it = line.chars().peekable();
    while let Some(c) = it.next() {
        if c == '§'
            && let Some(&n) = it.peek()
        {
            it.next();
            let l = n.to_ascii_lowercase();
            if l == 'r' || color_of(l).is_some() {
                codes = if l == 'r' { String::new() } else { format!("§{l}") };
                bold = false;
            } else if "lonmk".contains(l) {
                codes.push('§');
                codes.push(l);
                bold |= l == 'l';
            }
            piece.push(c);
            piece.push(n);
            continue;
        }
        let w = advance(c, bold);
        if width + w > LIST_WIDTH && visible(&piece) > 0 {
            let rest = space.as_ref().map(|(at, then)| format!("{then}{}", &piece[at + 1..]));
            match (space.take(), rest) {
                (Some((at, _)), Some(rest)) if line_width(&rest) + w <= LIST_WIDTH => {
                    piece.truncate(at);
                    out.push(std::mem::replace(&mut piece, rest));
                    width = line_width(&piece);
                }
                _ => {
                    out.push(std::mem::replace(&mut piece, codes.clone()));
                    width = 0;
                }
            }
        }
        if c == ' ' {
            space = Some((piece.len(), codes.clone()));
        }
        piece.push(c);
        width += w;
    }
    out.push(piece);
    out
}

/// How one message line fares in the server list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LineFit {
    /// In Minecraft pixels.
    pub width: u32,
    /// Wider than the list: it wraps onto another row.
    pub wraps: bool,
    /// When its end doesn't show, how many of its characters do (0: none,
    /// as line 2 is pushed out when line 1 wraps).
    pub cut: Option<usize>,
}

/// The two rows players see for message lines 1 and 2 (filled in), and
/// how each line fares.
pub fn list_lines(l1: &str, l2: &str) -> (Vec<String>, [LineFit; 2]) {
    let a = wrap_line(l1);
    let b = if l2.is_empty() { Vec::new() } else { wrap_line(l2) };
    let fit1 = LineFit {
        width: line_width(l1),
        wraps: a.len() > 1,
        cut: (a.len() > 2).then(|| visible(&a[0]) + visible(&a[1])),
    };
    let fit2 = LineFit {
        width: line_width(l2),
        wraps: b.len() > 1,
        cut: if b.is_empty() {
            None
        } else if a.len() > 1 {
            Some(0)
        } else {
            (b.len() > 1).then(|| visible(&b[0]))
        },
    };
    (a.into_iter().chain(b).take(2).collect(), [fit1, fit2])
}

/// What to tell the player about lines that don't fit, if anything.
pub fn fit_hint(fits: &[LineFit; 2], has_line2: bool) -> Option<String> {
    let [a, b] = fits;
    if a.wraps {
        return Some(if has_line2 {
            "Line 1 is too long for the server list: it wraps onto the second row, so line 2 doesn't show.".into()
        } else {
            match a.cut {
                Some(n) => format!("Line 1 is cut off in the server list after {n} characters."),
                None => "Line 1 is too long for one row: the server list wraps it onto the second.".into(),
            }
        });
    }
    b.cut.filter(|n| *n > 0).map(|n| format!("Line 2 is cut off in the server list after {n} characters."))
}

/// Characters shown against about how many fit (the list goes by width,
/// so narrow letters fit more).
pub fn fit_count(line: &str, fit: &LineFit) -> (usize, usize) {
    let n = visible(line);
    (n, (n as i64 + (LIST_WIDTH as i64 - fit.width as i64).div_euclid(6)).max(0) as usize)
}

/// Drops codes that change nothing: in a row of codes with no text
/// between, a colour or §r cancels the ones before it, and a style given
/// twice counts once. Codes at the very end go too unless `keep_end`.
pub fn tidy(text: &str, keep_end: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run: Vec<char> = Vec::new();
    let flush = |out: &mut String, run: &mut Vec<char>| {
        for c in run.drain(..) {
            out.push('§');
            out.push(c);
        }
    };
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        if c == '§'
            && let Some(&n) = it.peek()
        {
            it.next();
            let n = n.to_ascii_lowercase();
            if n == 'r' || color_of(n).is_some() {
                run = vec![n];
            } else if !run.contains(&n) {
                run.push(n);
            }
            continue;
        }
        flush(&mut out, &mut run);
        out.push(c);
    }
    if keep_end {
        flush(&mut out, &mut run);
    }
    out
}

/// The colour and styles in force at byte `pos` (styles as codes, "lo").
fn state_at(text: &str, pos: usize) -> (Option<char>, String) {
    let mut color = None;
    let mut fmt = String::new();
    let mut it = text[..pos.min(text.len())].chars().peekable();
    while let Some(c) = it.next() {
        if c != '§' {
            continue;
        }
        let Some(&n) = it.peek() else { break };
        it.next();
        let n = n.to_ascii_lowercase();
        if color_of(n).is_some() {
            color = Some(n);
            fmt.clear();
        } else if n == 'r' {
            color = None;
            fmt.clear();
        } else if "lonmk".contains(n) && !fmt.contains(n) {
            fmt.push(n);
        }
    }
    (color, fmt)
}

fn codes(cs: impl IntoIterator<Item = char>) -> String {
    cs.into_iter().flat_map(|c| ['§', c]).collect()
}

/// Applies `code` to the bytes `a..b` of `text`: the code before, and what
/// was in force before put back after (a colour clears bold and the rest
/// in Minecraft, so those are put back after a colour too). With nothing
/// selected the code goes in at the caret. Returns the text and the new
/// selection (bytes).
pub fn apply_code(text: &str, a: usize, b: usize, code: char) -> (String, usize, usize) {
    let (a, b) = (a.min(text.len()), b.min(text.len()));
    let (a, b) = (a.min(b), a.max(b));
    let (color, fmt) = state_at(text, a);
    let before = codes(color.into_iter().chain(fmt.chars()));
    let mut open = format!("§{code}");
    if color_of(code).is_some() {
        open.push_str(&codes(fmt.chars()));
    }
    let head = tidy(&format!("{}{open}", &text[..a]), true);
    if a == b {
        let n = head.len();
        return (format!("{head}{}", &text[b..]), n, n);
    }
    let close = if code == 'r' { String::new() } else { format!("§r{before}") };
    let mid = &text[a..b];
    let tail = tidy(&format!("{close}{}", &text[b..]), true);
    let n = head.len();
    (format!("{head}{mid}{tail}"), n, n + mid.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_go_around_a_selection_without_piling_up() {
        let t = "§fT§6N§fP Test Server";
        let a = t.find("Test").unwrap();
        let (t1, x, y) = apply_code(t, a, a + 4, '6');
        assert_eq!(t1, "§fT§6N§fP §6Test§f Server");
        assert_eq!(&t1[x..y], "Test");
        let (t2, x, y) = apply_code(&t1, x, y, '6');
        assert_eq!(t2, t1);
        let (t3, x, y) = apply_code(&t2, x, y, 'l');
        assert_eq!(t3, "§fT§6N§fP §6§lTest§f Server");
        assert_eq!(&t3[x..y], "Test");
        assert_eq!(apply_code("abc", 3, 3, 'c'), ("abc§c".to_string(), 6, 6));
        // A colour inside bold text keeps it bold.
        let (t4, _, _) = apply_code("§lab", 4, 4, 'c');
        assert_eq!(t4, "§la§c§lb");
    }

    #[test]
    fn draws_runs_like_minecraft() {
        let r = runs("§6a§lb§cc§rd");
        assert_eq!(r.len(), 4);
        assert_eq!((r[0].color, r[0].bold), (0xFFAA00, false));
        assert_eq!((r[1].color, r[1].bold), (0xFFAA00, true));
        assert_eq!((r[2].color, r[2].bold), (0xFF5555, false));
        assert_eq!(r[3].color, DEFAULT_COLOR);
        assert_eq!(visible("§6ab§lc"), 3);
        assert_eq!(amp_to_section("&6a & b"), "§6a & b");
    }

    fn m(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn expands_known_names_and_keeps_the_rest() {
        let v = m(&[("pack_version", "v1.78.1"), ("day", "148")]);
        assert_eq!(expand("§6TNP | {pack_version} · day {day}", &v), "§6TNP | v1.78.1 · day 148");
        assert_eq!(expand("{nope} {Day} {pack_version", &v), "{nope} {Day} {pack_version");
        assert_eq!(expand("{{day}} {{{day}}}", &v), "{day} {148}");
        assert_eq!(expand("é {day} ü", &v), "é 148 ü");
    }

    #[test]
    fn finds_variables() {
        assert!(has_vars("a {day}"));
        assert!(has_vars("{{literal}}"));
        assert!(!has_vars("plain § text {Not A Var}"));
        assert!(is_dynamic("{players}/{max_players}"));
        assert!(!is_dynamic("{pack_version} {day"));
    }

    #[test]
    fn ampersand_shortcuts_and_line_breaks() {
        assert_eq!(normalize("&6Gold &lbold &Rreset"), "§6Gold §lbold §Rreset");
        assert_eq!(normalize("Tom & Jerry &z &"), "Tom & Jerry &z &");
        assert_eq!(normalize("one\\ntwo"), "one\ntwo");
    }

    #[test]
    fn pack_versions() {
        assert_eq!(short_version("TNP Limitless 8", "Limitless 8 - v1.78.1"), "v1.78.1");
        assert_eq!(short_version("ATM10", "ATM10-2.41"), "2.41");
        assert_eq!(short_version("Pack", "1.2.3"), "1.2.3");
        assert_eq!(version_in_file_name("TNP-Limitless-8-Server-1.78.1.zip").as_deref(), Some("1.78.1"));
        assert_eq!(version_in_file_name("server.zip"), None);
    }

    #[test]
    fn template_survives_a_start_and_hand_edits_win() {
        let dir = std::env::temp_dir().join(format!("hopper-motd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("mods")).unwrap();
        std::fs::write(dir.join("mods/a.jar"), "").unwrap();
        std::fs::write(dir.join("mods/b.jar.disabled"), "").unwrap();
        std::fs::write(dir.join("server.properties"), "motd=old\ndifficulty=hard\n").unwrap();
        let out = save(&dir, "&6Hi {difficulty} {mods} mods\\n{nope}").unwrap();
        assert_eq!(out, "§6Hi hard 1 mods\n{nope}");
        assert_eq!(props_motd(&dir), out);
        assert_eq!(template(&dir), "§6Hi {difficulty} {mods} mods\n{nope}");
        // A later start fills in the new value.
        let mut p = Properties::load(&dir.join("server.properties")).unwrap();
        p.set("difficulty", "easy");
        p.save(&dir.join("server.properties")).unwrap();
        apply(&dir).unwrap();
        assert_eq!(props_motd(&dir), "§6Hi easy 1 mods\n{nope}");
        assert_eq!(current(&dir, &Live::default()).as_deref(), Some("§6Hi easy 1 mods\n{nope}"));
        // The router's live message is kept for the next start.
        remember(&dir, "§6Hi easy 2 mods\n{nope}").unwrap();
        assert_eq!(props_motd(&dir), "§6Hi easy 2 mods\n{nope}");
        assert_eq!(template(&dir), "§6Hi {difficulty} {mods} mods\n{nope}");
        // Changed in server.properties by hand: that message is kept.
        let mut p = Properties::load(&dir.join("server.properties")).unwrap();
        p.set("motd", "typed by hand");
        p.save(&dir.join("server.properties")).unwrap();
        assert_eq!(template(&dir), "typed by hand");
        apply(&dir).unwrap();
        assert_eq!(props_motd(&dir), "typed by hand");
        assert!(!dir.join(FILE).exists());
        // A plain message needs no template.
        save(&dir, "just text").unwrap();
        assert!(!dir.join(FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extra_mods_counts_only_known_mods_the_pack_lacks() {
        use crate::instances::Addon;
        let a = |id: u64, f: &str, from_pack: bool| Addon { project_id: id, file_name: f.into(), from_pack, ..Default::default() };
        let jars: Vec<String> = ["a.jar", "b.jar", "c.jar", "loose.jar"].map(String::from).to_vec();
        use crate::modpack::PackMods;
        // Marked by Hopper when it installed the pack.
        let marked = [a(1, "a.jar", true), a(2, "b.jar", false), a(3, "c.jar", true), a(4, "off.jar", false)];
        assert_eq!(extra_mods(&marked, &jars, &PackMods::Marked), 1);
        // No marks (a server made from a server pack): the manifest says.
        let unmarked = [a(1, "a.jar", false), a(2, "b.jar", false), a(3, "C.JAR", false)];
        assert_eq!(extra_mods(&unmarked, &jars, &PackMods::Listed([1, 3].into_iter().collect())), 1);
    }

    #[test]
    fn wraps_like_the_server_list() {
        // The line from the report fits: Minecraft's letters are narrower
        // than 45 monospace cells suggest.
        let l2 = "mc 1.21.1 NeoForge 21.1.259 extra mods 981";
        assert_eq!(line_width(l2), 222);
        let (shown, fits) = list_lines("§6Hi", l2);
        assert_eq!(shown, vec!["§6Hi".to_string(), l2.to_string()]);
        assert_eq!(fit_hint(&fits, true), None);
        assert_eq!(fit_count(l2, &fits[1]), (42, 50));
        // Too long: broken at the last space that fits, the rest lost.
        let long = format!("{l2} and some more words to push it past the edge");
        let (shown, fits) = list_lines("x", &long);
        assert_eq!(shown[1], "mc 1.21.1 NeoForge 21.1.259 extra mods 981 and");
        assert_eq!(fits[1].cut, Some(46));
        assert_eq!(fit_hint(&fits, true).unwrap(), "Line 2 is cut off in the server list after 46 characters.");
        // A long line 1 takes both rows and keeps its codes on the second.
        let (shown, fits) = list_lines(&format!("§l{long}"), "x");
        assert!(shown[1].starts_with("§l"));
        assert_eq!(fits[1].cut, Some(0));
        // No space to break at: mid-word.
        assert_eq!(wrap_line(&"a".repeat(60)).iter().map(|p| p.len()).collect::<Vec<_>>(), [45, 15]);
    }
}
