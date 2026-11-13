//! Shared mod settings in a friend group (`design/config-sync.md`): a member
//! picks the mods whose settings the group shares, and from then on a
//! setting one person changes reaches everyone else, setting by setting,
//! the newest change winning and naming who made it.
//!
//! Three pieces of state:
//!
//! - [`Configs`], kept with the group copy (`groupsync::PackCopy`) and sent
//!   whole with every recipe: per shared mod, whether it is shared (a
//!   latest-wins switch) and every setting someone changed since, with its
//!   value and stamp. Because the whole state rides along, the mailbox
//!   keeping only the newest recipe loses nothing for a friend who was away.
//! - [`Local`], beside the instance (`.hopper-config-sync.json`): the values
//!   this PC last saw for each shared setting (the snapshot), Keep mine, and
//!   the settings Hopper wrote from friends since the game last ran.
//! - The files themselves, only ever edited value by value
//!   (`configedit::save_edits`), never replaced.
//!
//! A setting whose parsed value differs from the snapshot is this player's
//! change. One Hopper wrote that the game changed back is not: it counts as
//! refused here. Nothing here writes while the game runs; the desktop calls
//! [`tick`] only when it doesn't.

use crate::configedit::{self, Edit, Entry, Value};
use crate::groupsync::{self, Verdict};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The most a recipe's settings may take after compression, as the relay
/// allows (`MAX_CONFIGS` in relay/hopper_mailbox.py).
pub const MAX_PAYLOAD: usize = 16384;

/// Where [`Local`] lives in an instance. Dotted, so the config list, the
/// disk watcher and share codes leave it alone.
pub const LOCAL_FILE: &str = ".hopper-config-sync.json";

// ---- setting names ------------------------------------------------------------------

/// A setting's name across PCs: its section path and key joined with `/`,
/// each part escaped (`~` as `~0`, `/` as `~1`) so a key holding a slash
/// reads back.
pub fn key_of(path: &[String], key: &str) -> String {
    let esc = |s: &str| s.replace('~', "~0").replace('/', "~1");
    let mut parts: Vec<String> = path.iter().map(|p| esc(p)).collect();
    parts.push(esc(key));
    parts.join("/")
}

/// [`key_of`] read back into the path and key.
pub fn split_key(k: &str) -> (Vec<String>, String) {
    let un = |s: &str| s.replace("~1", "/").replace("~0", "~");
    let mut parts: Vec<String> = k.split('/').map(un).collect();
    let key = parts.pop().unwrap_or_default();
    (parts, key)
}

// ---- what never travels -------------------------------------------------------------

/// Why a setting or file is never shared, for the Configs tab's line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// Key bindings, window and HUD positions, field of view, volumes and
    /// the like: each player's own.
    Personal,
    /// Named or shaped like a password or token.
    Secret,
    /// A folder on this PC.
    PathValue,
    /// The whole file stays on each PC.
    File,
}

impl Why {
    pub fn says(self) -> &'static str {
        match self {
            Why::Personal | Why::File => "Never shared: personal",
            Why::Secret => "Never shared: looks like a password",
            Why::PathValue => "Never shared: a folder on this PC",
        }
    }
}

/// Whether a file's settings can be shared at all: a mod's settings under
/// `config/` or `defaultconfigs/` that Hopper can edit, not one of the files
/// that stay on each PC (`options.txt` and friends) or pack content.
pub fn file_ok(rel: &str) -> bool {
    let r = rel.replace('\\', "/");
    if !(r.starts_with("config/") || r.starts_with("defaultconfigs/")) || !configedit::editable(&r) {
        return false;
    }
    // `defaultconfigs/` counts as pack content in an update (the pack's
    // copy wins there), but its server settings are just what a group
    // wants alike.
    let content = crate::packdiff::pack_content(&r) && !r.starts_with("defaultconfigs/");
    if configedit::Format::of(&r).is_none() || content || crate::configdiff::generated(&r) {
        return false;
    }
    let name = r.rsplit('/').next().unwrap_or(&r);
    if crate::packdiff::keep_local(name) {
        return false;
    }
    let low = r.to_ascii_lowercase();
    // Known mods' personal files: map waypoints, key setups, per-world data.
    !PERSONAL_FILES.iter().any(|p| low.contains(p))
}

/// Parts of a path that mark a mod's personal files.
const PERSONAL_FILES: &[&str] = &["waypoint", "controlling", "journeymap/data", "xaero/minimap/", "xaero/world-map/", "keybind", "hotkey"];

/// Parts of a squashed key (lower case, letters and digits only) that make
/// it personal.
const PERSONAL_KEYS: &[&str] = &[
    "keybind", "hotkey", "keymapping", "keycode", "window", "fullscreen", "guiscale", "hudx", "hudy", "hudpos", "hudoffset", "overlayx",
    "overlayy", "overlaypos", "offsetx", "offsety", "fov", "fieldofview", "volume", "language", "renderdistance", "brightness", "gamma",
    "screenposition", "waypoint",
];

/// Whether a key is personal by its name.
pub fn personal_key(key: &str) -> bool {
    let low = key.to_ascii_lowercase();
    if low.starts_with("key.") || low.starts_with("key_") || low == "key" {
        return true;
    }
    let k = crate::configowner::squash(key);
    PERSONAL_KEYS.iter().any(|p| k.contains(p))
}

/// A value that looks like a token: 20 or more characters of hex or base64
/// with no spaces, holding both letters and digits (so a long enum name or
/// a word list isn't one).
pub fn secret_value(v: &str) -> bool {
    let t = v.trim();
    t.len() >= 20
        && t.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_' | '.'))
        && t.chars().any(|c| c.is_ascii_digit())
        && t.chars().any(|c| c.is_ascii_alphabetic())
        && !t.contains("..")
}

/// A value that is a folder or file on this PC (`C:\…`, `/home/…`).
pub fn path_value(v: &str) -> bool {
    let t = v.trim();
    let b = t.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
        || t.starts_with("\\\\")
        || ["/home/", "/Users/", "/root/", "/mnt/", "/var/", "/opt/", "/tmp/", "~/"].iter().any(|p| t.starts_with(p))
}

/// Why the setting `key` (with its section `path`) in `rel`, valued `v`
/// (as text), is never shared, if it isn't. Checked when sending and again
/// when receiving.
pub fn never(rel: &str, path: &[String], key: &str, v: &str) -> Option<Why> {
    if !file_ok(rel) {
        return Some(Why::File);
    }
    if configedit::secret_key(key) || path.iter().any(|p| configedit::secret_key(p)) || secret_value(v) {
        return Some(Why::Secret);
    }
    if path_value(v) {
        return Some(Why::PathValue);
    }
    if personal_key(key) {
        return Some(Why::Personal);
    }
    None
}

/// [`never`] for a setting by its [`key_of`] name.
pub fn never_k(rel: &str, k: &str, v: &str) -> Option<Why> {
    let (path, key) = split_key(k);
    never(rel, &path, &key, v)
}

/// Mods whose settings are personal by nature (minimaps, shaders, menus):
/// their switch starts off with "Usually personal", though they can still
/// be shared.
pub fn usually_personal(rels: &[String]) -> bool {
    !rels.is_empty()
        && rels.iter().all(|r| {
            let r = r.replace('\\', "/");
            match r.split_once('/') {
                Some((folder, rest)) => crate::configdiff::client_side(folder, rest),
                None => false,
            }
        })
}

/// A web address (a map server, a chat bridge): fine to share, but the
/// sharer is told friends will use it too.
pub fn address_value(v: &str) -> bool {
    let t = v.trim().to_ascii_lowercase();
    t.starts_with("http://") || t.starts_with("https://") || t.starts_with("ws://") || t.starts_with("wss://") || t.parse::<std::net::IpAddr>().is_ok()
}

// ---- a file's settings ------------------------------------------------------------------

/// A file's settings by [`key_of`] name.
pub type Settings = BTreeMap<String, Entry>;

/// The settings in `text`, read as `rel`'s format. None when it doesn't read.
pub fn settings_of(rel: &str, text: &str) -> Option<Settings> {
    let sections = configedit::parse_file(rel, text).ok()?;
    let mut out = BTreeMap::new();
    for e in sections.into_iter().flat_map(|s| s.entries) {
        out.insert(key_of(&e.path, &e.key), e);
    }
    Some(out)
}

/// Whether the text `snap` (as the snapshot holds it) is the same value as
/// `now`, the way the game reads them (`1` and `1.0`, list spacing).
fn same_text(now: &Value, snap: &str) -> bool {
    match now.parse_like(snap) {
        Ok(v) => configedit::same(now, &v),
        Err(_) => now.text().trim() == snap.trim(),
    }
}

// ---- the group's state ------------------------------------------------------------------

/// Whether a mod's settings are shared, as the latest member to change it
/// left it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Turn {
    pub on: bool,
    /// The relay's number for the event that carried it; 0 for this PC's
    /// own switch not sent yet.
    pub seq: u64,
    /// The member's mailbox id.
    pub by: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(skip_serializing_if = "is_zero")]
    pub at: u64,
}

/// One shared setting's value and stamp.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Val {
    /// The value as text, the way `configedit::Value::text` gives it.
    pub v: String,
    pub seq: u64,
    pub by: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(skip_serializing_if = "is_zero")]
    pub at: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// One mod's shared settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ModCfg {
    pub shared: Turn,
    /// File (relative path) -> setting ([`key_of`]) -> value.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub set: BTreeMap<String, BTreeMap<String, Val>>,
}

/// A group pack's shared settings, by mod key (`groupsync::addon_key`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Configs {
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub mods: BTreeMap<String, ModCfg>,
}

impl Configs {
    pub fn is_empty(&self) -> bool {
        self.mods.is_empty()
    }

    pub fn is_shared(&self, m: &str) -> bool {
        self.mods.get(m).is_some_and(|c| c.shared.on)
    }

    /// The mods whose settings are shared now.
    pub fn shared(&self) -> impl Iterator<Item = (&String, &ModCfg)> {
        self.mods.iter().filter(|(_, c)| c.shared.on)
    }

    /// How many settings of `m` the group changed.
    pub fn count(&self, m: &str) -> usize {
        self.mods.get(m).map_or(0, |c| c.set.values().map(|f| f.len()).sum())
    }

    /// Whether anything here wasn't sent yet (this PC's own, seq 0).
    pub fn unsent(&self, me: &str) -> bool {
        self.mods.values().any(|c| (c.shared.seq == 0 && c.shared.by == me) || c.set.values().flat_map(|f| f.values()).any(|v| v.seq == 0 && v.by == me))
    }

    /// Turns sharing of `m` on or off as this PC's own change. Turning on
    /// starts from no settings: what goes out first is what [`Local::start`]
    /// counted as changed from the pack.
    pub fn turn(&mut self, m: &str, on: bool, me: &str) {
        let c = self.mods.entry(m.to_string()).or_default();
        if c.shared.on != on {
            c.set.clear();
        }
        c.shared = Turn { on, seq: 0, by: me.to_string(), name: String::new(), at: now() };
    }

    /// This PC's own changed settings, as found by [`scan`]: each goes out
    /// with the next publish.
    pub fn own(&mut self, changes: &[Own], me: &str) {
        for o in changes {
            let Some(c) = self.mods.get_mut(&o.m).filter(|c| c.shared.on) else { continue };
            let file = c.set.entry(o.rel.clone()).or_default();
            if file.get(&o.k).is_some_and(|v| v.v == o.v && (v.seq == 0 || v.by == me)) {
                continue;
            }
            file.insert(o.k.clone(), Val { v: o.v.clone(), seq: 0, by: me.to_string(), name: String::new(), at: now() });
        }
    }

    /// A publish carrying `sent` (this state as it went out) got `seq`:
    /// this PC's own unsent changes in it take that number, so a member who
    /// passes them on later can't date them with their own.
    pub fn stamp(&mut self, sent: &Configs, me: &str, seq: u64) {
        for (m, sc) in &sent.mods {
            let Some(c) = self.mods.get_mut(m) else { continue };
            if sc.shared.seq == 0 && c.shared.seq == 0 && c.shared.by == me && c.shared.on == sc.shared.on {
                c.shared.seq = seq;
            }
            for (rel, keys) in &sc.set {
                let Some(file) = c.set.get_mut(rel) else { continue };
                for (k, v) in keys {
                    if v.seq != 0 {
                        continue;
                    }
                    if let Some(x) = file.get_mut(k).filter(|x| x.seq == 0 && x.v == v.v && x.by == me) {
                        x.seq = seq;
                    }
                }
            }
        }
    }

    /// The mailbox lost its numbers (see `PackCopy::relay_reset`): every
    /// stamp starts over, so new changes don't look older than old ones.
    pub fn relay_reset(&mut self) {
        for c in self.mods.values_mut() {
            c.shared.seq = 0;
            for v in c.set.values_mut().flat_map(|f| f.values_mut()) {
                v.seq = 0;
            }
        }
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

// ---- on the wire --------------------------------------------------------------------

/// What a recipe's `configs` field holds.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Wire {
    v: u32,
    mods: BTreeMap<String, ModCfg>,
}

/// Didn't fit [`MAX_PAYLOAD`].
#[derive(Debug)]
pub struct TooBig(pub usize);

impl std::fmt::Display for TooBig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Too many changed settings to share. Share fewer mods.")
    }
}

impl std::error::Error for TooBig {}

/// The state as a recipe carries it: base64 of deflated JSON, with every
/// setting the never-share rules catch left out (checked again on the
/// other end). Names and times stay home; the receiver knows its members.
pub fn encode(c: &Configs) -> Result<String, TooBig> {
    use base64::Engine as _;
    use std::io::Write as _;
    let mut mods = BTreeMap::new();
    for (m, mc) in &c.mods {
        let mut out = ModCfg { shared: Turn { name: String::new(), at: 0, ..mc.shared.clone() }, set: BTreeMap::new() };
        if mc.shared.on {
            for (rel, keys) in &mc.set {
                let keys: BTreeMap<String, Val> = keys
                    .iter()
                    .filter(|(k, v)| never_k(rel, k, &v.v).is_none())
                    .map(|(k, v)| (k.clone(), Val { name: String::new(), at: 0, ..v.clone() }))
                    .collect();
                if !keys.is_empty() {
                    out.set.insert(rel.clone(), keys);
                }
            }
        }
        mods.insert(m.clone(), out);
    }
    let json = serde_json::to_vec(&Wire { v: 1, mods }).unwrap_or_default();
    let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
    let _ = z.write_all(&json);
    let packed = z.finish().unwrap_or_default();
    let s = base64::engine::general_purpose::STANDARD.encode(packed);
    if s.len() > MAX_PAYLOAD { Err(TooBig(s.len())) } else { Ok(s) }
}

/// A recipe's `configs` read back; None when it isn't one (a newer format,
/// or damaged).
pub fn decode(s: &str) -> Option<Configs> {
    use base64::Engine as _;
    use std::io::Read as _;
    if s.is_empty() || s.len() > MAX_PAYLOAD {
        return None;
    }
    let packed = base64::engine::general_purpose::STANDARD.decode(s.trim()).ok()?;
    let mut json = Vec::new();
    // A small payload can't unpack into a huge one.
    flate2::read::DeflateDecoder::new(&packed[..]).take(4 << 20).read_to_end(&mut json).ok()?;
    let w: Wire = serde_json::from_slice(&json).ok()?;
    (w.v == 1).then_some(Configs { mods: w.mods })
}

// ---- taking a member's state in ---------------------------------------------------------

/// What [`take`] changed.
#[derive(Debug, Default, PartialEq)]
pub struct Taken {
    /// Mods whose sharing a member turned on or off: (mod, on, who).
    pub turned: Vec<(String, bool, String)>,
    /// Settings that took a member's value: (mod, file, setting, who).
    pub vals: Vec<(String, String, String, String)>,
}

impl Taken {
    pub fn is_empty(&self) -> bool {
        self.turned.is_empty() && self.vals.is_empty()
    }
}

/// Takes a member's state (`theirs`, from a recipe numbered `seq`) into this
/// copy's, latest wins per mod switch and per setting (the same rule as for
/// mods, `groupsync::decide`). A stamp of 0 is that recipe's own change,
/// dated with its number. This PC's own unsent change wins (it goes out
/// later). Never-share settings are dropped whatever the sender's Hopper
/// thought. `name_of` names a member by id.
pub fn take(state: &mut Configs, from: &str, from_name: &str, seq: u64, at: u64, theirs: &Configs, me: &str, name_of: impl Fn(&str) -> String) -> Taken {
    let mut out = Taken::default();
    let who = |s: u64, by: &str| -> (u64, String, String) {
        if s == 0 || s > seq || by.is_empty() {
            (seq, from.to_string(), from_name.to_string())
        } else if by == from {
            (s, from.to_string(), from_name.to_string())
        } else {
            (s, by.to_string(), name_of(by))
        }
    };
    for (m, tc) in &theirs.mods {
        let (tseq, tby, tname) = who(tc.shared.seq, &tc.shared.by);
        let mine = state.mods.get(m).map(|c| c.shared.clone());
        let unsent = mine.as_ref().is_some_and(|t| t.seq == 0 && t.by == me && !t.by.is_empty());
        let same = mine.as_ref().map_or(!tc.shared.on, |t| t.on == tc.shared.on);
        if groupsync::decide(same, tseq, mine.as_ref().map(|t| t.seq), unsent) == Verdict::Apply {
            let c = state.mods.entry(m.clone()).or_default();
            c.set.clear();
            c.shared = Turn { on: tc.shared.on, seq: tseq, by: tby.clone(), name: tname.clone(), at };
            out.turned.push((m.clone(), tc.shared.on, tname));
        }
        let Some(c) = state.mods.get_mut(m).filter(|c| c.shared.on) else { continue };
        let since = c.shared.seq;
        for (rel, keys) in &tc.set {
            for (k, v) in keys {
                if never_k(rel, k, &v.v).is_some() {
                    continue;
                }
                let (vseq, vby, vname) = who(v.seq, &v.by);
                // From before the mod was (last) shared: not the group's.
                if vseq < since {
                    continue;
                }
                let mine = c.set.get(rel).and_then(|f| f.get(k));
                let unsent = mine.is_some_and(|x| x.seq == 0 && x.by == me);
                let same = mine.is_some_and(|x| x.v == v.v);
                if groupsync::decide(same, vseq, mine.map(|x| x.seq), unsent) == Verdict::Apply {
                    c.set.entry(rel.clone()).or_default().insert(k.clone(), Val { v: v.v.clone(), seq: vseq, by: vby, name: vname.clone(), at });
                    out.vals.push((m.clone(), rel.clone(), k.clone(), vname));
                }
            }
        }
    }
    out
}

// ---- this PC: snapshot, Keep mine --------------------------------------------------------

/// A setting Hopper wrote from a friend, until it changes: the value it
/// replaced, so a game (or Undo in History) that puts it back is noticed
/// and not sent to the group as this player's change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Wrote {
    pub was: String,
    pub seq: u64,
}

/// This PC's side of the shared settings, kept beside the instance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Local {
    /// Mod -> file -> setting -> the value as last seen here.
    pub snap: BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>>,
    /// Mods whose friends' settings are remembered but not written here.
    pub keep: BTreeSet<String>,
    /// Single settings kept the same way ("<file>\n<setting>").
    pub keep_keys: BTreeSet<String>,
    /// "<file>\n<setting>" -> what Hopper wrote over.
    pub wrote: BTreeMap<String, Wrote>,
    /// "<file>\n<setting>" -> the group value's number the game refused.
    pub refused: BTreeMap<String, u64>,
}

/// The [`Local`] map key for a setting in a file.
pub fn fk(rel: &str, k: &str) -> String {
    format!("{rel}\n{k}")
}

impl Local {
    pub fn load(root: &Path) -> Local {
        std::fs::read(root.join(LOCAL_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, root: &Path) -> anyhow::Result<()> {
        let p = root.join(LOCAL_FILE);
        let tmp = root.join(format!("{LOCAL_FILE}.tmp"));
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        std::fs::rename(&tmp, &p)?;
        Ok(())
    }

    /// Snapshot of `m` from its files now, for the settings it doesn't
    /// have yet. A setting not in the snapshot is never a change.
    pub fn seed(&mut self, m: &str, files: &[FileNow]) {
        let snap = self.snap.entry(m.to_string()).or_default();
        for f in files {
            let Some(s) = &f.settings else { continue };
            let keys = snap.entry(f.rel.clone()).or_default();
            for (k, e) in s {
                keys.entry(k.clone()).or_insert_with(|| e.value.text());
            }
        }
    }

    /// Sharing `m` starts here (this PC turned it on): the snapshot is the
    /// pack's own copy of each file where the pack ships one, so the
    /// settings the player changed from the pack are the first to go out;
    /// a file the pack doesn't ship starts from how it is now. Returns
    /// how many settings that is.
    pub fn start(&mut self, m: &str, files: &[FileNow], pack: &BTreeMap<String, Settings>) -> usize {
        self.snap.remove(m);
        let snap = self.snap.entry(m.to_string()).or_default();
        for f in files {
            if let Some(p) = pack.get(&f.rel) {
                let keys = snap.entry(f.rel.clone()).or_default();
                for (k, e) in p {
                    keys.insert(k.clone(), e.value.text());
                }
            }
        }
        self.seed(m, files);
        let d = differs(m, files, pack);
        d
    }
}

/// How many of `m`'s settings differ from the pack's own copy (and would go
/// out when sharing starts), never-share ones left out.
pub fn differs(_m: &str, files: &[FileNow], pack: &BTreeMap<String, Settings>) -> usize {
    let mut n = 0;
    for f in files {
        let (Some(now), Some(p)) = (&f.settings, pack.get(&f.rel)) else { continue };
        for (k, e) in now {
            if let Some(pe) = p.get(k)
                && !configedit::same(&e.value, &pe.value)
                && never(&f.rel, &e.path, &e.key, &e.value.text()).is_none()
            {
                n += 1;
            }
        }
    }
    n
}

/// One of a mod's files as it is on disk now; None: missing or unreadable.
#[derive(Clone, Debug)]
pub struct FileNow {
    pub rel: String,
    pub settings: Option<Settings>,
    /// The text, for `save_edits`.
    pub text: Option<String>,
}

impl FileNow {
    pub fn read(root: &Path, rel: &str) -> FileNow {
        let text = std::fs::read_to_string(root.join(rel)).ok();
        let settings = text.as_deref().and_then(|t| settings_of(rel, t));
        FileNow { rel: rel.to_string(), settings, text }
    }

    #[cfg(test)]
    pub fn of(rel: &str, text: &str) -> FileNow {
        FileNow { rel: rel.into(), settings: settings_of(rel, text), text: Some(text.into()) }
    }
}

/// One setting this player changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Own {
    pub m: String,
    pub rel: String,
    pub k: String,
    pub v: String,
}

/// What [`scan`] found besides this player's changes.
#[derive(Debug, Default, PartialEq)]
pub struct Scanned {
    pub own: Vec<Own>,
    /// Friends' values the game changed back: (mod, file, setting).
    pub refused: Vec<(String, String, String)>,
}

/// Compares each shared mod's files with the snapshot: a changed value is
/// this player's change (and the snapshot moves on), except one Hopper
/// wrote that the game put back, which counts as refused here, and one the
/// never-share rules catch. Mods kept with Keep mine aren't read: they send
/// the group's values, so nothing echoes. Run only while the game isn't.
pub fn scan(state: &Configs, local: &mut Local, mods: &BTreeMap<String, Vec<FileNow>>) -> Scanned {
    let mut out = Scanned::default();
    for (m, _) in state.shared() {
        if local.keep.contains(m) {
            continue;
        }
        let Some(files) = mods.get(m) else { continue };
        if !local.snap.contains_key(m) {
            local.seed(m, files);
            continue;
        }
        for f in files {
            let Some(now) = &f.settings else { continue };
            if !file_ok(&f.rel) {
                continue;
            }
            let snap = local.snap.entry(m.clone()).or_default().entry(f.rel.clone()).or_default();
            for (k, e) in now {
                let text = e.value.text();
                let Some(was) = snap.get(k) else {
                    snap.insert(k.clone(), text);
                    continue;
                };
                if same_text(&e.value, was) {
                    continue;
                }
                let key = fk(&f.rel, k);
                let w = local.wrote.remove(&key);
                snap.insert(k.clone(), text.clone());
                if let Some(w) = w
                    && same_text(&e.value, &w.was)
                {
                    local.refused.insert(key.clone(), w.seq);
                    out.refused.push((m.clone(), f.rel.clone(), k.clone()));
                    continue;
                }
                if local.keep_keys.contains(&key) || never(&f.rel, &e.path, &e.key, &text).is_some() {
                    continue;
                }
                out.own.push(Own { m: m.clone(), rel: f.rel.clone(), k: k.clone(), v: text });
            }
        }
    }
    out
}

/// One setting to write from the group.
#[derive(Clone, Debug, PartialEq)]
pub struct Put {
    pub k: String,
    pub old: Value,
    pub new: Value,
    /// Its stamp, for "refused" and History.
    pub seq: u64,
    pub by: String,
    pub name: String,
}

/// The settings of one file to write.
#[derive(Clone, Debug, PartialEq)]
pub struct FilePut {
    pub m: String,
    pub rel: String,
    pub puts: Vec<Put>,
}

/// A setting that waits: the file doesn't have it (a different version of
/// the mod) or doesn't exist yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Waiting {
    pub m: String,
    pub rel: String,
    pub k: String,
    pub name: String,
}

/// What [`plan`] decided.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    pub files: Vec<FilePut>,
    pub waiting: Vec<Waiting>,
}

/// The group's values to write here: each shared setting whose value here
/// differs, unless this player changed it and hasn't sent it yet (theirs
/// goes out and wins), the game refused that very value, Keep mine holds
/// it, the never-share rules catch it, or the value doesn't fit the
/// setting. A setting the file lacks waits; it isn't added.
pub fn plan(state: &Configs, local: &Local, mods: &BTreeMap<String, Vec<FileNow>>, me: &str) -> Plan {
    let mut out = Plan::default();
    for (m, c) in state.shared() {
        if local.keep.contains(m) {
            continue;
        }
        let files = mods.get(m).map(Vec::as_slice).unwrap_or(&[]);
        for (rel, keys) in &c.set {
            let now = files.iter().find(|f| f.rel == *rel).and_then(|f| f.settings.as_ref());
            let snap = local.snap.get(m).and_then(|s| s.get(rel));
            let mut puts = Vec::new();
            for (k, v) in keys {
                if v.by == me && v.seq == 0 {
                    continue;
                }
                let key = fk(rel, k);
                if local.keep_keys.contains(&key) || never_k(rel, k, &v.v).is_some() || local.refused.get(&key) == Some(&v.seq) {
                    continue;
                }
                let Some(e) = now.and_then(|s| s.get(k)) else {
                    out.waiting.push(Waiting { m: m.clone(), rel: rel.clone(), k: k.clone(), name: v.name.clone() });
                    continue;
                };
                let Ok(new) = e.value.parse_like(&v.v) else { continue };
                if configedit::same(&e.value, &new) || e.check(&new).is_some() {
                    continue;
                }
                // Changed here since last seen: this player's change, not
                // sent yet; it goes out and wins.
                if snap.and_then(|s| s.get(k)).is_some_and(|was| !same_text(&e.value, was)) {
                    continue;
                }
                puts.push(Put { k: k.clone(), old: e.value.clone(), new, seq: v.seq, by: v.by.clone(), name: v.name.clone() });
            }
            if !puts.is_empty() {
                out.files.push(FilePut { m: m.clone(), rel: rel.clone(), puts });
            }
        }
    }
    out
}

// ---- on disk ------------------------------------------------------------------------------

/// A mod's settings files in an instance, by mod key, with its name: the
/// files `configowner` gives each jar, in `config/` and `defaultconfigs/`.
pub fn mod_files(inst: &crate::instances::Instance) -> BTreeMap<String, (String, Vec<String>)> {
    let mods_dir = inst.root.join("mods");
    let jars: Vec<(String, std::path::PathBuf, String)> =
        inst.addons.iter().map(|a| (a.file_name.clone(), mods_dir.join(&a.file_name), a.name.clone())).filter(|(_, p, _)| p.is_file()).collect();
    let owners = crate::configowner::Owners::new(crate::configowner::of_jars(jars));
    let mut by_jar: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for f in crate::configindex::index(&inst.root, &owners) {
        if let Some(o) = f.owner
            && file_ok(&f.rel)
        {
            by_jar.entry(o).or_default().push(f.rel);
        }
    }
    let mut out = BTreeMap::new();
    for a in &inst.addons {
        if let Some(rels) = by_jar.remove(&a.file_name) {
            out.insert(groupsync::addon_key(a), (a.name.clone(), rels));
        }
    }
    out
}

/// The pack's own copy of `rels` (the pack zip in Hopper's cache), parsed;
/// files the pack doesn't ship are left out.
pub fn pack_copy(inst: &crate::instances::Instance, rels: &[String]) -> BTreeMap<String, Settings> {
    let Some(p) = inst.pack.as_ref().filter(|p| p.file_id != 0) else { return BTreeMap::new() };
    rels.iter()
        .filter_map(|r| {
            let text = crate::modpack::cached_override_text(p.file_id, r)?;
            Some((r.clone(), settings_of(r, &text)?))
        })
        .collect()
}

/// Every file `state` names for the shared mods, plus their own files here.
fn read_mods(root: &Path, state: &Configs, files: &BTreeMap<String, (String, Vec<String>)>) -> BTreeMap<String, Vec<FileNow>> {
    let mut out = BTreeMap::new();
    for (m, c) in state.shared() {
        let mut rels: BTreeSet<String> = files.get(m).map(|(_, r)| r.iter().cloned().collect()).unwrap_or_default();
        rels.extend(c.set.keys().filter(|r| file_ok(r)).cloned());
        out.insert(m.clone(), rels.iter().map(|r| FileNow::read(root, r)).collect());
    }
    out
}

/// One mod's settings written from the group by a [`tick`].
#[derive(Debug, Clone, PartialEq)]
pub struct Wrote1 {
    pub m: String,
    pub mod_name: String,
    /// How many settings.
    pub n: usize,
    /// Who made the newest of them.
    pub who: String,
    /// Each setting written, (file, key, its value before): what [`undo`]
    /// puts back. Per setting, so a later friend's change to the same file
    /// stays.
    pub keys: Vec<(String, String, String)>,
}

/// What a [`tick`] did.
#[derive(Debug, Default)]
pub struct Ticked {
    /// This player's changes, to put in the state ([`Configs::own`]).
    pub own: Vec<Own>,
    pub refused: Vec<(String, String, String)>,
    /// Written from the group, one per mod.
    pub wrote: Vec<Wrote1>,
    pub waiting: Vec<Waiting>,
    /// A file that couldn't be written, said plainly.
    pub failed: Vec<String>,
}

/// Reads this player's changes, then writes the group's values, in the
/// instance at `root`. Never call it while the game runs. Each mod's
/// writes are one History entry, named after whoever made the newest of
/// them, with the settings and their old and new values noted.
pub fn tick(
    root: &Path,
    state: &Configs,
    files: &BTreeMap<String, (String, Vec<String>)>,
    me: &str,
    group: &str,
) -> anyhow::Result<Ticked> {
    let mut local = Local::load(root);
    let before = local.clone();
    let mods = read_mods(root, state, files);
    let mut out = Ticked::default();
    let s = scan(state, &mut local, &mods);
    out.own = s.own;
    out.refused = s.refused;
    // This player's changes just found aren't in the state yet: the plan
    // must not write over them.
    let mut st = state.clone();
    st.own(&out.own, me);
    let p = plan(&st, &local, &mods, me);
    out.waiting = p.waiting;
    let mut by_mod: BTreeMap<String, Vec<FilePut>> = BTreeMap::new();
    for f in p.files {
        by_mod.entry(f.m.clone()).or_default().push(f);
    }
    for (m, puts) in by_mod {
        let newest = puts.iter().flat_map(|f| &f.puts).max_by_key(|p| p.seq).cloned();
        let Some(newest) = newest else { continue };
        let who = crate::actor::Who::friend(&newest.name, &newest.by, group, "");
        let mod_name = files.get(&m).map(|(n, _)| n.clone()).unwrap_or_default();
        let _scope = crate::actor::begin(root, Some(who.clone()));
        let mut n = 0;
        let since = now();
        let mut keys = Vec::new();
        for f in &puts {
            let base = mods.get(&m).and_then(|v| v.iter().find(|x| x.rel == f.rel)).and_then(|x| x.text.clone()).unwrap_or_default();
            let edits: Vec<Edit> = f
                .puts
                .iter()
                .map(|p| {
                    let (path, key) = split_key(&p.k);
                    Edit { path, key, value: p.new.clone() }
                })
                .collect();
            match configedit::save_edits(root, &f.rel, &base, &edits) {
                Ok(_) => {
                    let note: Vec<crate::backups::SettingNote> = f
                        .puts
                        .iter()
                        .map(|p| crate::backups::SettingNote { key: split_key(&p.k).1, was: p.old.text(), now: p.new.text() })
                        .collect();
                    crate::backups::note_settings(root, since, &f.rel, &mod_name, &note);
                    let snap = local.snap.entry(m.clone()).or_default().entry(f.rel.clone()).or_default();
                    for p in &f.puts {
                        snap.insert(p.k.clone(), p.new.text());
                        local.wrote.insert(fk(&f.rel, &p.k), Wrote { was: p.old.text(), seq: p.seq });
                    }
                    n += f.puts.len();
                    keys.extend(f.puts.iter().map(|p| (f.rel.clone(), p.k.clone(), p.old.text())));
                }
                Err(e) => out.failed.push(format!("{}: {e:#}", f.rel)),
            }
        }
        if n > 0 {
            out.wrote.push(Wrote1 { m, mod_name, n, who: newest.name, keys });
        }
    }
    if local != before {
        local.save(root)?;
    }
    Ok(out)
}

/// Undo for settings a [`tick`] wrote: puts each one back to its value
/// before, setting by setting (so other settings, and later changes to the
/// same file, stay), recorded in History like any edit. The settings then
/// count as refused here (see [`scan`]): the group keeps its values, this
/// PC keeps the old ones.
pub fn undo(root: &Path, w: &Wrote1) -> anyhow::Result<()> {
    let mut by_rel: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();
    for (rel, k, was) in &w.keys {
        by_rel.entry(rel.as_str()).or_default().push((k.as_str(), was.as_str()));
    }
    let mut done = 0;
    for (rel, keys) in by_rel {
        let f = FileNow::read(root, rel);
        let (Some(settings), Some(base)) = (f.settings.as_ref(), f.text.as_ref()) else { continue };
        let mut edits = Vec::new();
        let mut note = Vec::new();
        for (k, was) in keys {
            let Some(e) = settings.get(k) else { continue };
            let Ok(old) = e.value.parse_like(was) else { continue };
            if configedit::same(&e.value, &old) {
                continue;
            }
            let (path, key) = split_key(k);
            note.push(crate::backups::SettingNote { key: key.clone(), was: e.value.text(), now: old.text() });
            edits.push(Edit { path, key, value: old });
        }
        if edits.is_empty() {
            continue;
        }
        let since = now();
        configedit::save_edits(root, rel, base, &edits)?;
        crate::backups::note_settings(root, since, rel, &w.mod_name, &note);
        done += edits.len();
    }
    anyhow::ensure!(done > 0, "These settings changed again since. Nothing to put back.");
    Ok(())
}

/// The group's settings that would be written now, by mod: (mod, how many,
/// who made the newest), for "Settings from Sam wait until you close the
/// game" while the game runs. Reads, never writes.
pub fn pending(root: &Path, state: &Configs, files: &BTreeMap<String, (String, Vec<String>)>, me: &str) -> Vec<(String, usize, String)> {
    let local = Local::load(root);
    let mods = read_mods(root, state, files);
    let p = plan(state, &local, &mods, me);
    let mut out: BTreeMap<String, (usize, u64, String)> = BTreeMap::new();
    for f in p.files {
        let e = out.entry(f.m.clone()).or_default();
        for p in f.puts {
            e.0 += 1;
            if p.seq >= e.1 {
                e.1 = p.seq;
                e.2 = p.name;
            }
        }
    }
    out.into_iter().map(|(m, (n, _, who))| (m, n, who)).collect()
}

/// This PC starts sharing `m`: the snapshot comes from the pack's copy (see
/// [`Local::start`]). Returns how many settings go out first.
pub fn start_share(root: &Path, inst: &crate::instances::Instance, m: &str) -> anyhow::Result<usize> {
    let files = mod_files(inst);
    let rels = files.get(m).map(|(_, r)| r.clone()).unwrap_or_default();
    let now: Vec<FileNow> = rels.iter().map(|r| FileNow::read(root, r)).collect();
    let pack = pack_copy(inst, &rels);
    let mut local = Local::load(root);
    let n = local.start(m, &now, &pack);
    local.keep.remove(m);
    local.save(root)?;
    Ok(n)
}

/// How many of `m`'s settings differ from the pack's copy now, for the
/// switch's confirm line; None when the pack ships none of its files.
pub fn count_differs(inst: &crate::instances::Instance, m: &str) -> Option<usize> {
    let files = mod_files(inst);
    let rels = files.get(m).map(|(_, r)| r.clone()).unwrap_or_default();
    let pack = pack_copy(inst, &rels);
    if pack.is_empty() {
        return None;
    }
    let now: Vec<FileNow> = rels.iter().map(|r| FileNow::read(&inst.root, r)).collect();
    Some(differs(m, &now, &pack))
}

/// Keep mine for `m` on (friends' values are remembered, not written) or off
/// (Sync again: the group's values go in on the next tick, over this
/// player's own).
pub fn keep(root: &Path, m: &str, on: bool) -> anyhow::Result<()> {
    let mut local = Local::load(root);
    if on {
        local.keep.insert(m.to_string());
    } else {
        local.keep.remove(m);
        // Changes made while kept aren't this player's to send: the
        // snapshot starts again from the files, so the group's values win.
        local.snap.remove(m);
    }
    local.save(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOML: &str = "# JEI-ish\n[general]\n\t# Cheats\n\tcheatMode = false\n\tmaxColumns = 9\n\tbotToken = \"\"\n\n[client]\n\tfov = 70\n\tlistOnLeft = true\n";
    const JSON5: &str = "{\n  // a comment\n  \"enabled\": true,\n  \"range\": 32,\n  \"apiKey\": \"\",\n  \"nested\": { \"speed\": 1.5 }\n}\n";
    const CFG: &str = "# Forge cfg\ngeneral {\n    B:enableThing=true\n    I:maxThings=4\n    S:networkToken=\n}\n";
    const PROPS: &str = "# props\nmode=fast\nsound_volume=0.8\ntitle=Hello\n";

    fn st(mods: &[(&str, &[(&str, &str, &str, u64, &str)])]) -> Configs {
        let mut c = Configs::default();
        for (m, vals) in mods {
            let mc = c.mods.entry(m.to_string()).or_default();
            mc.shared = Turn { on: true, seq: 1, by: "mia".into(), ..Default::default() };
            for (rel, k, v, seq, by) in vals.iter() {
                mc.set.entry(rel.to_string()).or_default().insert(k.to_string(), Val { v: v.to_string(), seq: *seq, by: by.to_string(), ..Default::default() });
            }
        }
        c
    }

    fn files(m: &str, fs: Vec<FileNow>) -> BTreeMap<String, Vec<FileNow>> {
        BTreeMap::from([(m.to_string(), fs)])
    }

    fn change(text: &str, from: &str, to: &str) -> String {
        assert!(text.contains(from), "{from}");
        text.replacen(from, to, 1)
    }

    #[test]
    fn setting_names_survive_slashes_and_tildes() {
        let p = vec!["a/b".to_string(), "c~d".to_string()];
        let k = key_of(&p, "e/f");
        assert_eq!(k, "a~1b/c~0d/e~1f");
        assert_eq!(split_key(&k), (p, "e/f".to_string()));
        assert_eq!(split_key("top"), (vec![], "top".to_string()));
    }

    #[test]
    fn changed_settings_are_found_in_every_format_and_reformatting_is_not_one() {
        for (rel, text, from, to, key) in [
            ("config/jei-client.toml", TOML, "maxColumns = 9", "maxColumns = 12", "general/maxColumns"),
            ("config/thing.json5", JSON5, "\"range\": 32", "\"range\": 48", "range"),
            ("config/thing.cfg", CFG, "I:maxThings=4", "I:maxThings=6", "general/maxThings"),
            ("config/thing.properties", PROPS, "mode=fast", "mode=fancy", "mode"),
        ] {
            let state = st(&[("p:1", &[])]);
            let mut local = Local::default();
            // First look: the snapshot starts, nothing is a change.
            let s = scan(&state, &mut local, &files("p:1", vec![FileNow::of(rel, text)]));
            assert!(s.own.is_empty(), "{rel}");
            // Reformatted only (spacing, a number written another way): no change.
            let same = text.replace("= 9", "=   9").replace("\"range\": 32", "\"range\":32").replace("maxThings=4", "maxThings=4 ");
            let s = scan(&state, &mut local, &files("p:1", vec![FileNow::of(rel, &same)]));
            assert!(s.own.is_empty(), "{rel}: {:?}", s.own);
            let s = scan(&state, &mut local, &files("p:1", vec![FileNow::of(rel, &change(text, from, to))]));
            assert_eq!(s.own.iter().map(|o| o.k.as_str()).collect::<Vec<_>>(), [key], "{rel}");
            // Seen now: not a change again.
            let s = scan(&state, &mut local, &files("p:1", vec![FileNow::of(rel, &change(text, from, to))]));
            assert!(s.own.is_empty(), "{rel}");
        }
    }

    #[test]
    fn one_and_one_point_oh_are_the_same_value() {
        let state = st(&[("p:1", &[])]);
        let mut local = Local::default();
        let a = "[x]\nspeed = 1.0\n";
        scan(&state, &mut local, &files("p:1", vec![FileNow::of("config/m.toml", a)]));
        let s = scan(&state, &mut local, &files("p:1", vec![FileNow::of("config/m.toml", "[x]\nspeed = 1.00\n")]));
        assert!(s.own.is_empty());
    }

    #[test]
    fn never_shared_names_values_files_and_paths() {
        // Secrets by name (WaterMedia's network.token, Discord bridges,
        // Simple Voice Chat, Distant Horizons) and by shape.
        for (path, key) in [
            (&["network"][..], "token"),
            (&["discord"][..], "botToken"),
            (&[][..], "webhookUrl"),
            (&["general"][..], "apiKey"),
            (&[][..], "voice_chat_password"),
            (&["auth"][..], "value"),
            (&[][..], "sessionId"),
            (&[][..], "authKey"),
        ] {
            let p: Vec<String> = path.iter().map(|s| s.to_string()).collect();
            assert_eq!(never("config/m.toml", &p, key, "x"), Some(Why::Secret), "{key}");
        }
        assert_eq!(never("config/m.toml", &[], "serverId", "a1b2c3d4e5f6a7b8c9d0e1f2"), Some(Why::Secret));
        assert_eq!(never("config/m.toml", &[], "uuid", "123e4567-e89b-12d3-a456-426614174000"), Some(Why::Secret));
        // Not secrets: an author name, a long enum, a block id, a short number.
        assert_eq!(never("config/m.toml", &[], "showAuthor", "true"), None);
        assert_eq!(never("config/m.toml", &[], "mode", "ALWAYS_SHOW_WHEN_HOLDING"), None);
        assert_eq!(never("config/m.toml", &[], "block", "minecraft:stone_bricks"), None);
        // Personal by name.
        for key in ["key.jei.showRecipe", "toggleHotkey", "windowWidth", "fullscreen", "guiScale", "hud_x", "offset_x", "fov", "masterVolume", "language", "renderDistance"] {
            assert_eq!(never("config/m.toml", &[], key, "1"), Some(Why::Personal), "{key}");
        }
        // Folders on this PC stay; web addresses go (with a word to the sharer).
        assert_eq!(never("config/m.toml", &[], "folder", "C:\\Users\\mia\\maps"), Some(Why::PathValue));
        assert_eq!(never("config/m.toml", &[], "folder", "/home/mia/maps"), Some(Why::PathValue));
        assert_eq!(never("config/m.toml", &[], "mapUrl", "https://map.example.org"), None);
        assert!(address_value("https://map.example.org") && address_value("10.0.0.5") && !address_value("fast"));
        // Whole files.
        for rel in ["options.txt", "config/../options.txt", "saves/w/serverconfig/m-server.toml", "kubejs/config/x.json", "config/xaero/minimap/waypoints.txt", "config/m.unknown", "config/openloader/x.json"] {
            assert_eq!(never(rel, &[], "a", "1"), Some(Why::File), "{rel}");
        }
        assert!(file_ok("config/jei/jei-client.ini") && file_ok("defaultconfigs/create-server.toml"));
        assert!(usually_personal(&["config/jei/jei-client.ini".to_string()]));
        assert!(!usually_personal(&["config/create-common.toml".to_string()]));
    }

    #[test]
    fn a_secret_neither_goes_out_nor_comes_in() {
        let mut state = st(&[("p:1", &[("config/m.toml", "general/botToken", "abc", 0, "me"), ("config/m.toml", "general/maxColumns", "12", 0, "me")])]);
        let wire = decode(&encode(&state).unwrap()).unwrap();
        let keys: Vec<&String> = wire.mods["p:1"].set["config/m.toml"].keys().collect();
        assert_eq!(keys, ["general/maxColumns"]);
        // An older or broken Hopper sent one anyway: it is dropped here.
        let theirs = st(&[("p:1", &[("config/m.toml", "general/botToken", "stolen", 5, "mia")])]);
        let t = take(&mut state, "mia", "Mia", 5, 5, &theirs, "me", |_| String::new());
        assert!(t.vals.is_empty());
        assert_eq!(state.mods["p:1"].set["config/m.toml"]["general/botToken"].v, "abc");
        // And never written, even when it sits in the state.
        let p = plan(&state, &Local::default(), &files("p:1", vec![FileNow::of("config/m.toml", TOML)]), "x");
        assert!(p.files.iter().flat_map(|f| &f.puts).all(|p| p.k != "general/botToken"));
    }

    #[test]
    fn the_payload_reads_back_and_is_capped() {
        let state = st(&[("p:238222", &[("config/jei/jei-client.ini", "advanced/itemListOnLeft", "true", 4190, "a1b2"), ("config/jei/jei-client.ini", "general/cheatMode", "false", 0, "me")])]);
        let back = decode(&encode(&state).unwrap()).unwrap();
        assert_eq!(back, state);
        assert!(decode("not base64!").is_none() && decode("").is_none());
        // Many changed settings: refused whole, never cut.
        let mut big = Configs::default();
        let mc = big.mods.entry("p:1".into()).or_default();
        mc.shared.on = true;
        let f = mc.set.entry("config/m.json".into()).or_default();
        for i in 0..4000 {
            f.insert(format!("k{i}"), Val { v: format!("{:x}", (i as u64).wrapping_mul(0x9E3779B97F4A7C15)), seq: i, by: "x".into(), ..Default::default() });
        }
        assert!(matches!(encode(&big), Err(TooBig(n)) if n > MAX_PAYLOAD));
    }

    #[test]
    fn latest_wins_per_setting_and_echoes_change_nothing() {
        let rel = "config/m.toml";
        let mut here = st(&[("p:1", &[(rel, "general/maxColumns", "12", 50, "sam")])]);
        // Newer than what's known: taken.
        let mia = st(&[("p:1", &[(rel, "general/maxColumns", "16", 60, "mia")])]);
        let t = take(&mut here, "mia", "Mia", 60, 60, &mia, "me", |_| String::new());
        assert_eq!(t.vals, [("p:1".into(), rel.into(), "general/maxColumns".into(), "Mia".into())]);
        assert_eq!(here.mods["p:1"].set[rel]["general/maxColumns"].v, "16");
        // Older: lost.
        let old = st(&[("p:1", &[(rel, "general/maxColumns", "8", 40, "sam")])]);
        assert!(take(&mut here, "sam", "Sam", 70, 70, &old, "me", |_| String::new()).is_empty());
        // The same value back (an echo, passed on by Sam): nothing.
        assert!(take(&mut here, "sam", "Sam", 71, 71, &mia, "me", |_| String::new()).is_empty());
        // This PC changed it and hasn't sent it yet: held, ours goes out later.
        here.own(&[Own { m: "p:1".into(), rel: rel.into(), k: "general/maxColumns".into(), v: "4".into() }], "me");
        let later = st(&[("p:1", &[(rel, "general/maxColumns", "20", 0, "")])]);
        assert!(take(&mut here, "mia", "Mia", 80, 80, &later, "me", |_| String::new()).is_empty());
        // Once sent as 90 it is ours; Mia's 80 lost.
        let sent = here.clone();
        here.stamp(&sent, "me", 90);
        let v = &here.mods["p:1"].set[rel]["general/maxColumns"];
        assert_eq!((v.v.as_str(), v.seq), ("4", 90));
        assert!(take(&mut here, "mia", "Mia", 80, 80, &later, "me", |_| String::new()).is_empty());
    }

    #[test]
    fn a_change_passed_on_late_keeps_its_stamp() {
        let rel = "config/m.toml";
        // Sam passes on Mia's change (seq 10) in his recipe numbered 30; we
        // know Ann changed it at 20: Ann's stays.
        let mut here = st(&[("p:1", &[(rel, "a", "ann", 20, "ann")])]);
        let sam = st(&[("p:1", &[(rel, "a", "mia", 10, "mia")])]);
        assert!(take(&mut here, "sam", "Sam", 30, 30, &sam, "me", |id| if id == "mia" { "Mia".into() } else { String::new() }).is_empty());
        // Unknown before: taken with Mia's name and number, not Sam's.
        let mut fresh = st(&[("p:1", &[])]);
        let t = take(&mut fresh, "sam", "Sam", 30, 30, &sam, "me", |id| if id == "mia" { "Mia".into() } else { String::new() });
        assert_eq!(t.vals[0].3, "Mia");
        assert_eq!(fresh.mods["p:1"].set[rel]["a"].seq, 10);
    }

    #[test]
    fn sharing_is_a_latest_wins_switch_and_turning_off_keeps_values() {
        let mut here = Configs::default();
        let mut on = Configs::default();
        on.turn("p:1", true, "mia");
        on.mods.get_mut("p:1").unwrap().set.entry("config/m.toml".into()).or_default().insert("a".into(), Val { v: "1".into(), seq: 0, by: "mia".into(), ..Default::default() });
        let t = take(&mut here, "mia", "Mia", 10, 10, &on, "me", |_| String::new());
        assert_eq!(t.turned, [("p:1".into(), true, "Mia".into())]);
        assert_eq!(here.count("p:1"), 1);
        // Sam turns it off later: off for everyone, values dropped from the
        // group's state (each PC keeps its files as they are).
        let mut off = here.clone();
        off.mods.get_mut("p:1").unwrap().shared = Turn { on: false, seq: 0, by: "sam".into(), ..Default::default() };
        let t = take(&mut here, "sam", "Sam", 20, 20, &off, "me", |_| String::new());
        assert_eq!(t.turned, [("p:1".into(), false, "Sam".into())]);
        assert!(!here.is_shared("p:1") && here.count("p:1") == 0);
        // Mia's old recipe (on at 10) can't turn it back on.
        assert!(take(&mut here, "mia", "Mia", 10, 10, &on, "me", |_| String::new()).is_empty());
        // Shared again later: settings from before don't come back.
        let mut again = on.clone();
        again.mods.get_mut("p:1").unwrap().shared = Turn { on: true, seq: 30, by: "mia".into(), ..Default::default() };
        again.mods.get_mut("p:1").unwrap().set.get_mut("config/m.toml").unwrap().get_mut("a").unwrap().seq = 10;
        let t = take(&mut here, "mia", "Mia", 31, 31, &again, "me", |_| String::new());
        assert!(here.is_shared("p:1") && t.vals.is_empty());
    }

    #[test]
    fn offline_catch_up_takes_every_change_from_the_newest_recipe() {
        // Away for a month: only Mia's newest recipe waits, but it holds
        // every setting the group changed, each with its own stamp.
        let rel = "config/m.toml";
        let newest = st(&[("p:1", &[(rel, "general/cheatMode", "true", 12, "sam"), (rel, "general/maxColumns", "16", 0, "mia"), (rel, "client/listOnLeft", "false", 5, "ann")])]);
        let mut here = st(&[("p:1", &[])]);
        let t = take(&mut here, "mia", "Mia", 40, 40, &newest, "me", |id| id.to_uppercase());
        assert_eq!(t.vals.len(), 3);
        let p = plan(&here, &Local::default(), &files("p:1", vec![FileNow::of(rel, TOML)]), "me");
        let mut got: Vec<(String, String, String)> = p.files[0].puts.iter().map(|p| (p.k.clone(), p.new.text(), p.name.clone())).collect();
        got.sort();
        assert_eq!(
            got,
            [
                ("client/listOnLeft".into(), "false".into(), "ANN".into()),
                ("general/cheatMode".into(), "true".into(), "SAM".into()),
                ("general/maxColumns".into(), "16".into(), "Mia".into()),
            ]
        );
    }

    #[test]
    fn the_merge_writes_only_the_shared_settings_and_waits_for_missing_ones() {
        let dir = std::env::temp_dir().join(format!("hopper-configsync-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        let rel = "config/jei-client.toml";
        // The player's own copy: their maxColumns is 9 and fov 90.
        let mine = TOML.replace("fov = 70", "fov = 90");
        std::fs::write(dir.join(rel), &mine).unwrap();
        let m = BTreeMap::from([("p:1".to_string(), ("JEI".to_string(), vec![rel.to_string()]))]);
        let mut state = st(&[("p:1", &[])]);
        // First tick: snapshot only.
        let t = tick(&dir, &state, &m, "me", "g").unwrap();
        assert!(t.own.is_empty() && t.wrote.is_empty());
        // Mia changed cheatMode; Sam has a key this version doesn't.
        let theirs = st(&[("p:1", &[(rel, "general/cheatMode", "true", 10, "mia"), (rel, "general/newThing", "3", 11, "sam"), (rel, "client/fov", "50", 12, "sam")])]);
        take(&mut state, "mia", "Mia", 12, 12, &theirs, "me", |id| if id == "sam" { "Sam".into() } else { "Mia".into() });
        assert_eq!(pending(&dir, &state, &m, "me"), [("p:1".to_string(), 1, "Mia".to_string())]);
        let t = tick(&dir, &state, &m, "me", "g").unwrap();
        assert_eq!(t.wrote.iter().map(|w| (w.m.as_str(), w.n, w.who.as_str())).collect::<Vec<_>>(), [("p:1", 1, "Mia")]);
        let first = t.wrote[0].clone();
        assert_eq!(t.waiting.iter().map(|w| w.k.as_str()).collect::<Vec<_>>(), ["general/newThing"]);
        let disk = std::fs::read_to_string(dir.join(rel)).unwrap();
        // One value changed in place; everything else as the player had it.
        assert_eq!(disk, mine.replace("cheatMode = false", "cheatMode = true"));
        // History has it, with the setting and its old and new value.
        let sets = crate::backups::list(&dir);
        assert_eq!(sets.len(), 1);
        let f = &sets[0].files[0];
        assert_eq!(f.who.as_ref().map(|w| w.name.as_str()), Some("Mia"));
        let note = crate::backups::settings_note(&sets[0].dir, &f.restore_to).unwrap();
        assert_eq!((note.mod_name.as_str(), note.changes[0].key.as_str(), note.changes[0].was.as_str()), ("JEI", "cheatMode", "false"));
        // The write isn't sent back as this player's change.
        let t = tick(&dir, &state, &m, "me", "g").unwrap();
        assert!(t.own.is_empty() && t.wrote.is_empty());
        // The mod updated and has the key now: it goes in.
        std::fs::write(dir.join(rel), disk.replace("maxColumns = 9", "maxColumns = 9\n\tnewThing = 1")).unwrap();
        let t = tick(&dir, &state, &m, "me", "g").unwrap();
        assert_eq!(t.wrote.iter().map(|w| (w.n, w.who.as_str())).collect::<Vec<_>>(), [(1, "Sam")]);
        assert!(std::fs::read_to_string(dir.join(rel)).unwrap().contains("newThing = 3"));
        // Undo of Mia's change: the file goes back, and that isn't sent
        // as this player's change nor written again.
        undo(&dir, &first).unwrap();
        assert!(std::fs::read_to_string(dir.join(rel)).unwrap().contains("cheatMode = false"));
        let t = tick(&dir, &state, &m, "me", "g").unwrap();
        assert!(t.own.is_empty(), "{:?}", t.own);
        assert!(t.wrote.iter().all(|w| w.who != "Mia"), "{:?}", t.wrote);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_value_the_game_put_back_is_refused_not_sent() {
        let rel = "config/m.toml";
        let mut state = st(&[("p:1", &[(rel, "general/maxColumns", "200", 10, "mia")])]);
        let mut local = Local::default();
        local.seed("p:1", &[FileNow::of(rel, TOML)]);
        let p = plan(&state, &local, &files("p:1", vec![FileNow::of(rel, TOML)]), "me");
        assert_eq!(p.files.len(), 1);
        // Hopper wrote 200 over 9; the game started and put 9 back.
        local.snap.get_mut("p:1").unwrap().get_mut(rel).unwrap().insert("general/maxColumns".into(), "200".into());
        local.wrote.insert(fk(rel, "general/maxColumns"), Wrote { was: "9".into(), seq: 10 });
        let s = scan(&state, &mut local, &files("p:1", vec![FileNow::of(rel, TOML)]));
        assert!(s.own.is_empty());
        assert_eq!(s.refused.len(), 1);
        // Not written again for the same value; a newer one is tried.
        assert!(plan(&state, &local, &files("p:1", vec![FileNow::of(rel, TOML)]), "me").files.is_empty());
        state.mods.get_mut("p:1").unwrap().set.get_mut(rel).unwrap().insert("general/maxColumns".into(), Val { v: "12".into(), seq: 11, by: "mia".into(), ..Default::default() });
        assert_eq!(plan(&state, &local, &files("p:1", vec![FileNow::of(rel, TOML)]), "me").files.len(), 1);
    }

    #[test]
    fn an_unsent_change_here_is_not_written_over() {
        let rel = "config/m.toml";
        let state = st(&[("p:1", &[(rel, "general/maxColumns", "16", 10, "mia")])]);
        let mut local = Local::default();
        local.seed("p:1", &[FileNow::of(rel, TOML)]);
        // The player set 12 a moment ago; not scanned yet.
        let now = TOML.replace("maxColumns = 9", "maxColumns = 12");
        assert!(plan(&state, &local, &files("p:1", vec![FileNow::of(rel, &now)]), "me").files.is_empty());
    }

    #[test]
    fn keep_mine_writes_nothing_and_sends_nothing() {
        let rel = "config/m.toml";
        let state = st(&[("p:1", &[(rel, "general/maxColumns", "16", 10, "mia")])]);
        let mut local = Local::default();
        local.seed("p:1", &[FileNow::of(rel, TOML)]);
        local.keep.insert("p:1".into());
        let mine = TOML.replace("cheatMode = false", "cheatMode = true");
        assert!(scan(&state, &mut local, &files("p:1", vec![FileNow::of(rel, &mine)])).own.is_empty());
        assert!(plan(&state, &local, &files("p:1", vec![FileNow::of(rel, &mine)]), "me").files.is_empty());
        // What goes out is the group's state as it is: no echo of ours.
        let out = decode(&encode(&state).unwrap()).unwrap();
        assert_eq!(out.mods["p:1"].set[rel].len(), 1);
    }

    #[test]
    fn first_share_sends_what_differs_from_the_pack() {
        let rel = "config/m.toml";
        let pack = BTreeMap::from([(rel.to_string(), settings_of(rel, TOML).unwrap())]);
        // The player tuned two settings from the pack's; one is personal.
        let mine = TOML.replace("maxColumns = 9", "maxColumns = 14").replace("cheatMode = false", "cheatMode = true").replace("fov = 70", "fov = 90");
        let now = vec![FileNow::of(rel, &mine)];
        let mut local = Local::default();
        assert_eq!(local.start("p:1", &now, &pack), 2);
        let mut state = Configs::default();
        state.turn("p:1", true, "me");
        let s = scan(&state, &mut local, &files("p:1", now.clone()));
        let mut keys: Vec<&str> = s.own.iter().map(|o| o.k.as_str()).collect();
        keys.sort();
        assert_eq!(keys, ["general/cheatMode", "general/maxColumns"]);
        state.own(&s.own, "me");
        assert!(state.unsent("me"));
        // No pack copy: nothing to compare with, nothing goes out now.
        let mut local = Local::default();
        assert_eq!(local.start("p:1", &now, &BTreeMap::new()), 0);
        assert!(scan(&state, &mut local, &files("p:1", now)).own.is_empty());
    }
}
