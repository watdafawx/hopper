//! "Changes in the files": what visibly changed between two jars of a mod,
//! for a version whose author wrote no changelog (design/changelog-from-files.md).
//!
//! It reads only what a person can read: block and item names from the
//! language file, config option names with the description the mod ships,
//! languages, the mod's metadata, and data files (recipes, loot tables, tags,
//! advancements) compared by meaning. It never decompiles code and never says
//! why something changed.
//!
//! Entries are compared by the CRC-32 and size in the zip's own index (the
//! same bytes give the same pair whether the entry is stored or compressed),
//! so a jar that was only re-zipped is not "changed". Everything is bounded:
//! entry count, bytes read per entry and in total.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

/// More entries than this and the jar is not scanned.
const MAX_ENTRIES: usize = 50_000;
/// Largest JSON, toml or class file read (uncompressed bytes).
const MAX_FILE: u64 = 4 * 1024 * 1024;
/// Everything read from one jar, in total.
const MAX_TOTAL: u64 = 96 * 1024 * 1024;
/// Config classes read per jar.
const MAX_CONFIG_CLASSES: usize = 12;
/// Names listed before "and N more".
const LIST_MAX: usize = 6;

/// A config option the mod ships a description for.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ConfigOption {
    pub name: String,
    pub description: String,
}

/// A mod dependency from the metadata.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Dep {
    pub id: String,
    /// "required" or "optional".
    pub kind: String,
}

/// Data files of one kind (recipes, loot tables, tags, advancements).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DataCount {
    pub kind: String,
    pub added: usize,
    pub removed: usize,
    /// Changed in meaning.
    pub changed: usize,
    /// Rewritten in a newer format with the same meaning.
    pub format_only: usize,
}

/// What changed between two jars.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct JarChanges {
    pub blocks_added: Vec<String>,
    pub blocks_removed: Vec<String>,
    pub items_added: Vec<String>,
    pub items_removed: Vec<String>,
    pub options_added: Vec<ConfigOption>,
    pub options_removed: Vec<String>,
    pub languages_added: Vec<String>,
    pub languages_removed: Vec<String>,
    /// Other-language files rewritten (not added or removed).
    pub translations_updated: usize,
    /// English texts added / changed / removed that aren't a block or item name above.
    pub texts: [usize; 3],
    pub version: Option<(String, String)>,
    pub minecraft: Option<(String, String)>,
    pub loader: Option<(String, String)>,
    pub loader_name: String,
    pub deps_added: Vec<Dep>,
    pub deps_removed: Vec<String>,
    pub deps_changed: Vec<Dep>,
    pub pack_format_changed: bool,
    pub data: Vec<DataCount>,
    /// Textures added / changed / removed.
    pub textures: [usize; 3],
    /// Models and block states changed (not added or removed).
    pub models_changed: usize,
    /// Any class (or other non-readable file) differs.
    pub code_changed: bool,
}

/// The one-line answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Something readable changed.
    Changes,
    /// Nothing readable changed, the code did.
    CodeOnly,
    /// Same content in every file (metadata may differ).
    NoChange,
}

pub const CODE_ONLY: &str = "Only the mod's code changed. The author hasn't written what.";
pub const NO_CHANGE: &str = "The files have the same content. Only the version details changed.";

impl JarChanges {
    pub fn verdict(&self) -> Verdict {
        let data = self.data.iter().any(|d| d.added + d.removed + d.changed > 0);
        let readable = !self.blocks_added.is_empty()
            || !self.blocks_removed.is_empty()
            || !self.items_added.is_empty()
            || !self.items_removed.is_empty()
            || !self.options_added.is_empty()
            || !self.options_removed.is_empty()
            || !self.languages_added.is_empty()
            || !self.languages_removed.is_empty()
            || self.translations_updated > 0
            || self.texts.iter().any(|&n| n > 0)
            || self.textures.iter().any(|&n| n > 0)
            || self.models_changed > 0
            || data;
        // Version details (game and loader ranges, dependencies, pack format)
        // alone are "the same content". Beside changed code they are the news.
        let details = self.minecraft.is_some()
            || self.loader.is_some()
            || !self.deps_added.is_empty()
            || !self.deps_removed.is_empty()
            || !self.deps_changed.is_empty()
            || self.pack_format_changed;
        if readable || (details && self.code_changed) {
            Verdict::Changes
        } else if self.code_changed {
            Verdict::CodeOnly
        } else {
            Verdict::NoChange
        }
    }

    /// The result as markdown, the same text in every app. Plain bullets,
    /// each a bold lead and what changed; the verdict as a sentence when
    /// nothing readable did.
    pub fn to_markdown(&self) -> String {
        let mut out: Vec<String> = Vec::new();
        fn bullet(out: &mut Vec<String>, lead: String, rest: String) {
            out.push(if rest.is_empty() { format!("- **{lead}**") } else { format!("- **{lead}** {rest}") });
        }
        let named = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let list = |names: &[String]| -> String {
            let shown: Vec<&str> = names.iter().take(LIST_MAX).map(String::as_str).collect();
            let more = names.len().saturating_sub(LIST_MAX);
            let mut s = shown.join(", ");
            if more > 0 {
                s.push_str(&format!(", and {more} more"));
            }
            s
        };
        for (names, one, many, tail) in [
            (&self.blocks_added, "new block", "new blocks", ""),
            (&self.blocks_removed, "block removed", "blocks removed", ""),
            (&self.items_added, "new item", "new items", ""),
            (&self.items_removed, "item removed", "items removed", ""),
        ] {
            if !names.is_empty() {
                bullet(&mut out, format!("{}:", named(names.len(), one, many)), format!("{}{tail}", list(names)));
            }
        }
        if !self.options_added.is_empty() {
            let n = self.options_added.len();
            bullet(&mut out, format!("{}:", named(n, "new config option", "new config options")), String::new());
            for o in self.options_added.iter().take(LIST_MAX) {
                out.push(format!("  - `{}`: {}", o.name, one_line(&o.description, 140)));
            }
            if n > LIST_MAX {
                out.push(format!("  - and {} more", n - LIST_MAX));
            }
        }
        if !self.options_removed.is_empty() {
            let names: Vec<String> = self.options_removed.iter().map(|n| format!("`{n}`")).collect();
            bullet(&mut out, 
                format!("{}:", named(names.len(), "config option removed", "config options removed")),
                list(&names),
            );
        }
        if !self.languages_added.is_empty() {
            bullet(&mut out, "Languages added:".into(), list(&self.languages_added));
        }
        if !self.languages_removed.is_empty() {
            bullet(&mut out, "Languages removed:".into(), list(&self.languages_removed));
        }
        if self.translations_updated > 0 {
            bullet(&mut out, format!("{}.", named(self.translations_updated, "translation updated", "translations updated")), String::new());
        }
        let [ta, tc, tr] = self.texts;
        if ta + tc + tr > 0 {
            bullet(&mut out, "English text:".into(), counts(&[(ta, "added"), (tc, "changed"), (tr, "removed")]));
        }
        for d in &self.data {
            let k = d.kind.as_str();
            let plural = |n: usize| if n == 1 { k.to_string() } else { plural_of(k) };
            if d.added > 0 {
                bullet(&mut out, format!("{} new {}.", d.added, plural(d.added)), String::new());
            }
            if d.changed > 0 {
                bullet(&mut out, format!("{} {} changed.", d.changed, plural(d.changed)), String::new());
            }
            if d.removed > 0 {
                bullet(&mut out, format!("{} {} removed.", d.removed, plural(d.removed)), String::new());
            }
        }
        let [xa, xc, xr] = self.textures;
        if xa + xc + xr > 0 {
            bullet(&mut out, "Textures:".into(), counts(&[(xa, "added"), (xc, "changed"), (xr, "removed")]));
        }
        if self.models_changed > 0 {
            bullet(&mut out, format!("{}.", named(self.models_changed, "model or block shape changed", "models or block shapes changed")), String::new());
        }
        if let Some((a, b)) = &self.minecraft {
            bullet(&mut out, "Minecraft versions:".into(), format!("{a} → {b}"));
        }
        if let Some((a, b)) = &self.loader {
            let name = if self.loader_name.is_empty() { "Loader" } else { &self.loader_name };
            bullet(&mut out, format!("{name} versions:"), format!("{a} → {b}"));
        }
        if self.pack_format_changed {
            bullet(&mut out, "Supported pack format changed.".into(), String::new());
        }
        let dep = |d: &Dep| format!("{} ({})", d.id, d.kind);
        if !self.deps_added.is_empty() {
            let v: Vec<String> = self.deps_added.iter().map(dep).collect();
            bullet(&mut out, "Needs now:".into(), list(&v));
        }
        if !self.deps_removed.is_empty() {
            bullet(&mut out, "No longer needs:".into(), list(&self.deps_removed));
        }
        if !self.deps_changed.is_empty() {
            let v: Vec<String> = self.deps_changed.iter().map(|d| format!("{} is now {}", d.id, d.kind)).collect();
            bullet(&mut out, "Dependencies:".into(), list(&v));
        }
        let format_only: usize = self.data.iter().map(|d| d.format_only).sum();
        if format_only > 0 {
            bullet(&mut out, 
                format!("{} rewritten for a newer format,", named(format_only, "data file", "data files")),
                "same meaning, no gameplay change.".into(),
            );
        }
        match self.verdict() {
            Verdict::Changes => {}
            Verdict::CodeOnly => out.insert(0, CODE_ONLY.to_string()),
            Verdict::NoChange => out.insert(0, NO_CHANGE.to_string()),
        }
        // A sentence verdict is followed by a blank line before any bullets.
        if matches!(self.verdict(), Verdict::CodeOnly | Verdict::NoChange) && out.len() > 1 {
            out.insert(1, String::new());
        }
        out.join("\n")
    }
}

fn counts(parts: &[(usize, &str)]) -> String {
    parts.iter().filter(|(n, _)| *n > 0).map(|(n, w)| format!("{n} {w}")).collect::<Vec<_>>().join(", ")
}

fn plural_of(kind: &str) -> String {
    match kind {
        "loot table" => "loot tables".into(),
        "recipe" => "recipes".into(),
        "tag" => "tags".into(),
        "advancement" => "advancements".into(),
        o => format!("{o}s"),
    }
}

fn one_line(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        return s;
    }
    let cut: String = s.chars().take(max).collect();
    format!("{}…", cut.trim_end())
}

// ---------------------------------------------------------------- reading

/// One jar's index and the few files read from it.
struct Jar<R: Read + Seek> {
    z: zip::ZipArchive<R>,
    /// name -> (crc32, size, index); only safe, non-directory names.
    index: BTreeMap<String, (u32, u64, usize)>,
    read: u64,
}

impl<R: Read + Seek> Jar<R> {
    fn open(r: R) -> anyhow::Result<Self> {
        let mut z = zip::ZipArchive::new(r)?;
        anyhow::ensure!(z.len() <= MAX_ENTRIES, "This file has too many parts to compare.");
        let mut index = BTreeMap::new();
        for i in 0..z.len() {
            let f = z.by_index_raw(i)?;
            if f.is_dir() {
                continue;
            }
            // Names that leave the jar (`..`, absolute) are not ours to read.
            if f.enclosed_name().is_none() {
                continue;
            }
            index.insert(f.name().replace('\\', "/"), (f.crc32(), f.size(), i));
        }
        Ok(Self { z, index, read: 0 })
    }

    /// A file's bytes, `None` when absent, too big, or the budget is spent.
    fn bytes(&mut self, name: &str, limit: u64) -> Option<Vec<u8>> {
        let &(_, size, i) = self.index.get(name)?;
        if size > limit || self.read.saturating_add(size) > MAX_TOTAL {
            return None;
        }
        let f = self.z.by_index(i).ok()?;
        let mut buf = Vec::with_capacity(size.min(limit) as usize);
        // `take` bounds a lying header too.
        f.take(limit + 1).read_to_end(&mut buf).ok()?;
        if buf.len() as u64 > limit {
            return None;
        }
        self.read += buf.len() as u64;
        Some(buf)
    }

    fn json(&mut self, name: &str) -> Option<Value> {
        let b = self.bytes(name, MAX_FILE)?;
        serde_json::from_slice(strip_bom(&b)).ok()
    }

    fn text(&mut self, name: &str) -> Option<String> {
        let b = self.bytes(name, MAX_FILE)?;
        Some(String::from_utf8_lossy(strip_bom(&b)).into_owned())
    }

    fn same(&self, other: &Self, name: &str) -> bool {
        match (self.index.get(name), other.index.get(name)) {
            (Some(a), Some(b)) => a.0 == b.0 && a.1 == b.1,
            _ => false,
        }
    }
}

fn strip_bom(b: &[u8]) -> &[u8] {
    b.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(b)
}

/// Compares two jar files on disk.
pub fn compare_files(old: &std::path::Path, new: &std::path::Path) -> anyhow::Result<JarChanges> {
    let o = std::io::BufReader::new(std::fs::File::open(old)?);
    let n = std::io::BufReader::new(std::fs::File::open(new)?);
    compare(o, n)
}

/// Compares two jars.
pub fn compare<R: Read + Seek>(old: R, new: R) -> anyhow::Result<JarChanges> {
    let mut o = Jar::open(old)?;
    let mut n = Jar::open(new)?;
    let mut out = JarChanges::default();

    let names: BTreeSet<&String> = o.index.keys().chain(n.index.keys()).collect();
    let names: Vec<String> = names.into_iter().cloned().collect();
    let mut blocks: [BTreeSet<(String, String)>; 2] = Default::default();
    let mut items: [BTreeSet<(String, String)>; 2] = Default::default();
    let mut data: BTreeMap<&'static str, DataCount> = BTreeMap::new();
    let mut langs: [BTreeSet<String>; 2] = Default::default();
    let mut lang_changed: Vec<String> = Vec::new();

    for name in &names {
        let (in_o, in_n) = (o.index.contains_key(name), n.index.contains_key(name));
        let differs = in_o != in_n || !o.same(&n, name);
        if !differs {
            continue;
        }
        let side = |s: usize, set: &mut [BTreeSet<(String, String)>; 2], v: (String, String)| {
            set[s].insert(v);
        };
        let kind = classify(name);
        match kind {
            Kind::Blockstate(ns, id) => {
                if in_o && in_n {
                    out.models_changed += 1;
                } else {
                    side(usize::from(in_n), &mut blocks, (ns, id));
                }
            }
            Kind::ItemModel(ns, id) => {
                if !(in_o && in_n) {
                    side(usize::from(in_n), &mut items, (ns, id));
                } else {
                    out.models_changed += 1;
                }
            }
            Kind::Model => {
                if in_o && in_n {
                    out.models_changed += 1;
                }
            }
            Kind::Texture => match (in_o, in_n) {
                (false, true) => out.textures[0] += 1,
                (true, true) => out.textures[1] += 1,
                _ => out.textures[2] += 1,
            },
            Kind::Lang(code) => {
                if code != "en_us" {
                    match (in_o, in_n) {
                        (false, true) => {
                            langs[1].insert(code);
                        }
                        (true, false) => {
                            langs[0].insert(code);
                        }
                        _ => lang_changed.push(code),
                    }
                }
            }
            Kind::Data(k) => {
                let c = data.entry(k).or_insert_with(|| DataCount { kind: k.into(), ..Default::default() });
                match (in_o, in_n) {
                    (false, true) => c.added += 1,
                    (true, false) => c.removed += 1,
                    _ => {
                        if same_meaning(&mut o, &mut n, name) {
                            c.format_only += 1;
                        } else {
                            c.changed += 1;
                        }
                    }
                }
            }
            Kind::Meta => {}
            Kind::Code => out.code_changed = true,
        }
    }
    out.translations_updated = lang_changed.len();
    out.languages_added = langs[1].iter().map(|c| language_name(c)).collect();
    out.languages_removed = langs[0].iter().map(|c| language_name(c)).collect();
    out.data = data.into_values().collect();

    // Blocks and items by name: a block's item model isn't a second item.
    let block_ids = |s: &BTreeSet<(String, String)>| -> BTreeSet<(String, String)> { s.clone() };
    let all_blocks_new: BTreeSet<(String, String)> =
        n.index.keys().filter_map(|k| if let Kind::Blockstate(a, b) = classify(k) { Some((a, b)) } else { None }).collect();
    let all_blocks_old: BTreeSet<(String, String)> =
        o.index.keys().filter_map(|k| if let Kind::Blockstate(a, b) = classify(k) { Some((a, b)) } else { None }).collect();
    let mut en_new: BTreeMap<String, String> = BTreeMap::new();
    let mut en_old: BTreeMap<String, String> = BTreeMap::new();
    for (jar, dst) in [(&mut n, &mut en_new), (&mut o, &mut en_old)] {
        let langs: Vec<String> = jar.index.keys().filter(|k| matches!(classify(k), Kind::Lang(c) if c == "en_us")).cloned().collect();
        for l in langs {
            if let Some(Value::Object(m)) = jar.json(&l) {
                for (k, v) in m {
                    if let Some(s) = v.as_str() {
                        dst.insert(k, s.to_string());
                    }
                }
            }
        }
    }
    let pretty = |lang: &BTreeMap<String, String>, prefix: &str, (ns, id): &(String, String)| -> String {
        lang.get(&format!("{prefix}.{ns}.{}", id.replace('/', "."))).cloned().unwrap_or_else(|| prettify(id))
    };
    let mut explained: BTreeSet<String> = BTreeSet::new();
    for (side, lang, add) in [(1usize, &en_new, true), (0, &en_old, false)] {
        for b in block_ids(&blocks[side]) {
            // Moved to or from another folder in the same jar: not new or gone.
            let other = if add { &all_blocks_old } else { &all_blocks_new };
            if other.contains(&b) {
                continue;
            }
            explained.insert(format!("block.{}.{}", b.0, b.1));
            let nm = pretty(lang, "block", &b);
            if add { out.blocks_added.push(nm) } else { out.blocks_removed.push(nm) }
        }
        for it in &items[side] {
            // A block's own item model isn't a separate item.
            let blk = if add { &all_blocks_new } else { &all_blocks_old };
            if blk.contains(it) {
                continue;
            }
            explained.insert(format!("item.{}.{}", it.0, it.1));
            let nm = pretty(lang, "item", it);
            if add { out.items_added.push(nm) } else { out.items_removed.push(nm) }
        }
    }
    for v in [&mut out.blocks_added, &mut out.blocks_removed, &mut out.items_added, &mut out.items_removed] {
        v.sort();
        v.dedup();
    }
    // English texts that aren't a name already listed above.
    for (k, v) in &en_new {
        if explained.contains(k) {
            continue;
        }
        match en_old.get(k) {
            None => out.texts[0] += 1,
            Some(ov) if ov != v => out.texts[1] += 1,
            _ => {}
        }
    }
    for k in en_old.keys() {
        if !en_new.contains_key(k) && !explained.contains(k) {
            out.texts[2] += 1;
        }
    }

    metadata(&mut o, &mut n, &mut out);
    config_options(&mut o, &mut n, &mut out);
    Ok(out)
}

enum Kind {
    Blockstate(String, String),
    ItemModel(String, String),
    Model,
    Texture,
    Lang(String),
    Data(&'static str),
    /// Read through the metadata step, not as a file.
    Meta,
    /// Classes and everything we don't read.
    Code,
}

/// `a/b/c.json` under `<root>/<ns>/<dir>/`: the namespace and the rest.
fn under<'a>(path: &'a str, root: &str, dirs: &[&str]) -> Option<(&'a str, &'a str)> {
    let rest = path.strip_prefix(root)?.strip_prefix('/')?;
    let (ns, rest) = rest.split_once('/')?;
    for d in dirs {
        if let Some(r) = rest.strip_prefix(d).and_then(|r| r.strip_prefix('/')) {
            return Some((ns, r));
        }
    }
    None
}

fn classify(path: &str) -> Kind {
    let stem = |s: &str| s.strip_suffix(".json").map(str::to_string);
    if let Some((ns, r)) = under(path, "assets", &["blockstates"])
        && let Some(id) = stem(r)
    {
        return Kind::Blockstate(ns.into(), id);
    }
    if let Some((ns, r)) = under(path, "assets", &["models/item", "items"])
        && let Some(id) = stem(r)
    {
        return Kind::ItemModel(ns.into(), id);
    }
    if under(path, "assets", &["models"]).is_some() {
        return Kind::Model;
    }
    if under(path, "assets", &["textures"]).is_some_and(|(_, r)| r.ends_with(".png")) {
        return Kind::Texture;
    }
    if let Some((_, r)) = under(path, "assets", &["lang"])
        && let Some(code) = r.strip_suffix(".json")
        && !code.contains('/')
    {
        return Kind::Lang(code.to_ascii_lowercase());
    }
    if path.ends_with(".json") {
        for (dirs, k) in [
            (&["recipe", "recipes"][..], "recipe"),
            (&["loot_table", "loot_tables"], "loot table"),
            (&["tags"], "tag"),
            (&["advancement", "advancements"], "advancement"),
        ] {
            if under(path, "data", dirs).is_some() {
                return Kind::Data(k);
            }
        }
    }
    match path {
        "META-INF/neoforge.mods.toml" | "META-INF/mods.toml" | "fabric.mod.json" | "pack.mcmeta" | "META-INF/MANIFEST.MF" => Kind::Meta,
        p if p.starts_with("META-INF/") && (p.ends_with(".SF") || p.ends_with(".RSA") || p.ends_with(".DSA") || p.ends_with(".EC")) => Kind::Meta,
        _ => Kind::Code,
    }
}

/// `lead_boulder` -> "Lead Boulder".
fn prettify(id: &str) -> String {
    let id = id.rsplit('/').next().unwrap_or(id);
    id.split(['_', '-'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn language_name(code: &str) -> String {
    let n = match code {
        "de_de" | "de_at" | "de_ch" => "German",
        "es_es" => "Spanish (Spain)",
        "es_mx" => "Spanish (Mexico)",
        "es_ar" => "Spanish (Argentina)",
        "es_cl" => "Spanish (Chile)",
        "es_uy" => "Spanish (Uruguay)",
        "es_ve" => "Spanish (Venezuela)",
        "fr_fr" => "French",
        "fr_ca" => "French (Canada)",
        "it_it" => "Italian",
        "pt_br" => "Portuguese (Brazil)",
        "pt_pt" => "Portuguese (Portugal)",
        "ru_ru" => "Russian",
        "uk_ua" => "Ukrainian",
        "pl_pl" => "Polish",
        "cs_cz" => "Czech",
        "nl_nl" => "Dutch",
        "sv_se" => "Swedish",
        "tr_tr" => "Turkish",
        "ja_jp" => "Japanese",
        "ko_kr" => "Korean",
        "zh_cn" => "Chinese (Simplified)",
        "zh_tw" => "Chinese (Traditional)",
        "en_gb" => "English (UK)",
        "en_au" => "English (Australia)",
        "hu_hu" => "Hungarian",
        "fi_fi" => "Finnish",
        "da_dk" => "Danish",
        "nb_no" | "no_no" => "Norwegian",
        "ro_ro" => "Romanian",
        "el_gr" => "Greek",
        "he_il" => "Hebrew",
        "ar_sa" => "Arabic",
        "vi_vn" => "Vietnamese",
        "th_th" => "Thai",
        "id_id" => "Indonesian",
        _ => return code.to_string(),
    };
    n.to_string()
}

// ---------------------------------------------------------- data by meaning

/// Known format-only renames: a key or a value spelled the old way means the
/// same as the new way. Applied to both sides before comparing.
fn normalize(v: &mut Value) {
    match v {
        Value::Object(m) => {
            let old: Vec<String> = m.keys().filter(|k| *k == "conditions" || *k == "forge:conditions").cloned().collect();
            for k in old {
                if let Some(val) = m.remove(&k) {
                    m.entry("neoforge:conditions".to_string()).or_insert(val);
                }
            }
            for (k, val) in m.iter_mut() {
                // Condition types moved from the forge: to the neoforge: namespace.
                if k == "type"
                    && let Value::String(s) = val
                    && let Some(rest) = s.strip_prefix("forge:")
                {
                    *s = format!("neoforge:{rest}");
                }
                normalize(val);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(normalize),
        _ => {}
    }
}

/// Do two versions of a data file mean the same thing? Unreadable JSON on
/// either side is a change (we can't show it isn't).
fn same_meaning<R: Read + Seek>(o: &mut Jar<R>, n: &mut Jar<R>, name: &str) -> bool {
    let (Some(mut a), Some(mut b)) = (o.json(name), n.json(name)) else { return false };
    normalize(&mut a);
    normalize(&mut b);
    a == b
}

// ----------------------------------------------------------------- metadata

#[derive(Default)]
struct Meta {
    version: String,
    minecraft: String,
    loader: String,
    loader_name: &'static str,
    deps: BTreeMap<String, String>,
    pack_format: String,
}

fn read_meta<R: Read + Seek>(j: &mut Jar<R>) -> Meta {
    let mut m = Meta::default();
    if let Some(t) = j.text("META-INF/neoforge.mods.toml").map(|t| ("NeoForge", t)).or_else(|| j.text("META-INF/mods.toml").map(|t| ("Forge", t)))
        && let Ok(doc) = t.1.parse::<toml::Table>()
    {
        m.loader_name = t.0;
        let first = doc.get("mods").and_then(|v| v.as_array()).and_then(|a| a.first()).and_then(|v| v.as_table());
        let modid = first.and_then(|t| t.get("modId")).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let mut ver = first.and_then(|t| t.get("version")).and_then(|v| v.as_str()).unwrap_or("").to_string();
        if ver.contains("${") {
            ver = manifest_version(j).unwrap_or_default();
        }
        m.version = ver;
        if let Some(deps) = doc.get("dependencies").and_then(|d| d.as_table()) {
            for (_, list) in deps.iter().filter(|(k, _)| modid.is_empty() || **k == modid) {
                for d in list.as_array().into_iter().flatten().filter_map(|d| d.as_table()) {
                    let id = d.get("modId").and_then(|v| v.as_str()).unwrap_or("");
                    let range = d.get("versionRange").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let kind = match d.get("type").and_then(|v| v.as_str()) {
                        Some("optional") => "optional",
                        Some("required") => "required",
                        Some(_) => continue, // incompatible, discouraged
                        None => {
                            if d.get("mandatory").and_then(|v| v.as_bool()).unwrap_or(false) { "required" } else { "optional" }
                        }
                    };
                    match id {
                        "minecraft" => m.minecraft = range,
                        "neoforge" | "forge" => m.loader = range,
                        "" | "java" => {}
                        _ => {
                            m.deps.insert(id.to_string(), kind.to_string());
                        }
                    }
                }
            }
        }
    } else if let Some(v) = j.json("fabric.mod.json") {
        m.loader_name = "Fabric";
        m.version = v["version"].as_str().unwrap_or("").to_string();
        for (key, kind) in [("depends", "required"), ("recommends", "optional"), ("suggests", "optional")] {
            for (id, range) in v[key].as_object().into_iter().flatten() {
                let range = match range {
                    Value::String(s) => s.clone(),
                    Value::Array(a) => a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" || "),
                    _ => String::new(),
                };
                match id.as_str() {
                    "minecraft" => m.minecraft = range,
                    "fabricloader" => m.loader = range,
                    "java" | "fabric-api" | "fabric" => {
                        if id != "java" {
                            m.deps.entry(id.clone()).or_insert_with(|| kind.to_string());
                        }
                    }
                    _ => {
                        m.deps.entry(id.clone()).or_insert_with(|| kind.to_string());
                    }
                }
            }
        }
    }
    if let Some(v) = j.json("pack.mcmeta") {
        let p = &v["pack"];
        m.pack_format = format!("{}|{}|{}", p["pack_format"], p["min_format"], p["max_format"]);
    }
    m
}

fn manifest_version<R: Read + Seek>(j: &mut Jar<R>) -> Option<String> {
    let t = j.text("META-INF/MANIFEST.MF")?;
    t.lines().find_map(|l| l.strip_prefix("Implementation-Version:")).map(|v| v.trim().to_string())
}

fn metadata<R: Read + Seek>(o: &mut Jar<R>, n: &mut Jar<R>, out: &mut JarChanges) {
    let (a, b) = (read_meta(o), read_meta(n));
    out.loader_name = if b.loader_name.is_empty() { a.loader_name } else { b.loader_name }.to_string();
    let pair = |x: &str, y: &str| (x != y && !(x.is_empty() && y.is_empty())).then(|| (or_none(x), or_none(y)));
    out.version = pair(&a.version, &b.version);
    out.minecraft = pair(&a.minecraft, &b.minecraft);
    out.loader = pair(&a.loader, &b.loader);
    out.pack_format_changed = a.pack_format != b.pack_format;
    for (id, kind) in &b.deps {
        match a.deps.get(id) {
            None => out.deps_added.push(Dep { id: id.clone(), kind: kind.clone() }),
            Some(k) if k != kind => out.deps_changed.push(Dep { id: id.clone(), kind: kind.clone() }),
            _ => {}
        }
    }
    out.deps_removed = a.deps.keys().filter(|k| !b.deps.contains_key(*k)).cloned().collect();
}

fn or_none(s: &str) -> String {
    if s.is_empty() { "none".into() } else { s.to_string() }
}

// ------------------------------------------------------------ config options

/// Utf8 strings of a class file's constant pool, in pool order. Only what
/// the pool holds; code is never read. `None` for anything malformed.
fn class_strings(b: &[u8]) -> Option<Vec<String>> {
    let mut p = 0usize;
    let rd = |p: &mut usize, n: usize| -> Option<&[u8]> {
        let s = b.get(*p..p.checked_add(n)?)?;
        *p += n;
        Some(s)
    };
    if rd(&mut p, 4)? != [0xCA, 0xFE, 0xBA, 0xBE] {
        return None;
    }
    rd(&mut p, 4)?;
    let count = u16::from_be_bytes(rd(&mut p, 2)?.try_into().ok()?) as usize;
    let mut out = Vec::new();
    let mut i = 1;
    while i < count {
        let tag = *rd(&mut p, 1)?.first()?;
        match tag {
            1 => {
                let len = u16::from_be_bytes(rd(&mut p, 2)?.try_into().ok()?) as usize;
                let s = rd(&mut p, len)?;
                out.push(String::from_utf8_lossy(s).into_owned());
            }
            3 | 4 | 9 | 10 | 11 | 12 | 17 | 18 => {
                rd(&mut p, 4)?;
            }
            5 | 6 => {
                rd(&mut p, 8)?;
                i += 1; // long and double take two slots
            }
            7 | 8 | 16 | 19 | 20 => {
                rd(&mut p, 2)?;
            }
            15 => {
                rd(&mut p, 3)?;
            }
            _ => return None,
        }
        i += 1;
    }
    Some(out)
}

fn looks_like_option_name(s: &str) -> bool {
    (3..=48).contains(&s.len())
        && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        // A Java identifier of a class or method, e.g. `getValue`, `Builder`.
        && (s.contains('_') || s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()) || s.chars().any(|c| c.is_ascii_uppercase()) && s.chars().next().is_some_and(|c| c.is_ascii_lowercase()))
}

fn looks_like_description(s: &str) -> bool {
    let words = s.split_whitespace().count();
    (12..=400).contains(&s.len())
        && words >= 3
        && s.chars().next().is_some_and(|c| c.is_alphanumeric())
        && !s.contains(['/', ';', '<', '>', '{', '}', '\n'])
        && !s.starts_with('(')
}

/// Options clearly shipped as a description followed by the option's name in
/// the class's string table (the order `builder.comment("…").define("name", …)`
/// leaves). Anything not in that shape is not claimed.
fn option_pairs(strings: &[String]) -> (Vec<ConfigOption>, BTreeSet<String>) {
    let mut pairs = Vec::new();
    let all: BTreeSet<String> = strings.iter().cloned().collect();
    for w in strings.windows(2) {
        if looks_like_description(&w[0]) && looks_like_option_name(&w[1]) {
            pairs.push(ConfigOption { name: w[1].clone(), description: w[0].clone() });
        }
    }
    (pairs, all)
}

fn config_options<R: Read + Seek>(o: &mut Jar<R>, n: &mut Jar<R>, out: &mut JarChanges) {
    let read = |j: &mut Jar<R>| -> (Vec<ConfigOption>, BTreeSet<String>) {
        let classes: Vec<String> = j
            .index
            .keys()
            .filter(|k| k.ends_with(".class") && !k.starts_with("META-INF/") && is_config_class(k))
            .take(MAX_CONFIG_CLASSES)
            .cloned()
            .collect();
        let (mut pairs, mut all) = (Vec::new(), BTreeSet::new());
        for c in classes {
            if let Some(strings) = j.bytes(&c, MAX_FILE).and_then(|b| class_strings(&b)) {
                let (p, a) = option_pairs(&strings);
                pairs.extend(p);
                all.extend(a);
            }
        }
        (pairs, all)
    };
    let (op, oall) = read(o);
    let (np, nall) = read(n);
    let old_names: BTreeSet<&str> = op.iter().map(|p| p.name.as_str()).collect();
    let new_names: BTreeSet<&str> = np.iter().map(|p| p.name.as_str()).collect();
    let mut seen = BTreeSet::new();
    for p in &np {
        // Not a name the old config had anywhere in its strings.
        if !oall.contains(&p.name) && seen.insert(p.name.clone()) {
            out.options_added.push(p.clone());
        }
    }
    for p in &op {
        if !nall.contains(&p.name) && !new_names.contains(p.name.as_str()) && seen.insert(format!("-{}", p.name)) {
            out.options_removed.push(p.name.clone());
        }
    }
    let _ = old_names;
}

fn is_config_class(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    let l = file.to_ascii_lowercase();
    l.contains("config") && !l.contains("$lambda") && !l.starts_with("package-info")
}

// -------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    /// A jar in memory; `stored` leaves entries uncompressed.
    fn jar(files: &[(&str, &[u8])], stored: bool) -> Cursor<Vec<u8>> {
        let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(if stored { zip::CompressionMethod::Stored } else { zip::CompressionMethod::Deflated });
        for (n, b) in files {
            z.start_file(*n, opts).unwrap();
            z.write_all(b).unwrap();
        }
        let mut c = z.finish().unwrap();
        c.set_position(0);
        c
    }

    /// A class file whose constant pool holds exactly these Utf8 strings.
    fn class(strings: &[&str]) -> Vec<u8> {
        let mut b = vec![0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 65];
        b.extend(((strings.len() + 1) as u16).to_be_bytes());
        for s in strings {
            b.push(1);
            b.extend((s.len() as u16).to_be_bytes());
            b.extend(s.as_bytes());
        }
        b
    }

    fn toml(version: &str, mc: &str, extra: &str) -> String {
        format!(
            "modLoader=\"javafml\"\n[[mods]]\nmodId=\"m\"\nversion=\"{version}\"\n[[dependencies.m]]\nmodId=\"minecraft\"\ntype=\"required\"\nversionRange=\"{mc}\"\n{extra}"
        )
    }

    #[test]
    fn data_only_pack_with_new_version_is_no_change() {
        // Explorify-like: the same content, a new version, pack format and one optional dependency less.
        let content: Vec<(&str, &[u8])> = vec![
            ("assets/m/textures/block/a.png", b"png-a"),
            ("assets/m/lang/en_us.json", br#"{"block.m.a":"A"}"#),
            ("data/m/recipe/r.json", br#"{"type":"minecraft:crafting_shaped"}"#),
        ];
        let t_old = toml("1.6.4", "[1.21,1.22)", "[[dependencies.m]]\nmodId=\"cristellib\"\ntype=\"optional\"\nversionRange=\"*\"\n");
        let t_new = toml("1.6.5", "[1.21,1.22)", "");
        let mut a = content.clone();
        a.push(("META-INF/neoforge.mods.toml", t_old.as_bytes()));
        a.push(("pack.mcmeta", br#"{"pack":{"pack_format":88}}"#));
        let mut b = content;
        b.push(("META-INF/neoforge.mods.toml", t_new.as_bytes()));
        b.push(("pack.mcmeta", br#"{"pack":{"pack_format":512}}"#));
        // One side stored, the other compressed: same content is not a change.
        let r = compare(jar(&a, true), jar(&b, false)).unwrap();
        assert_eq!(r.verdict(), Verdict::NoChange);
        assert_eq!(r.version, Some(("1.6.4".into(), "1.6.5".into())));
        assert_eq!(r.deps_removed, vec!["cristellib".to_string()]);
        assert!(r.pack_format_changed);
        assert!(!r.code_changed);
        assert!(r.to_markdown().starts_with(NO_CHANGE));
    }

    #[test]
    fn content_mod_lists_blocks_config_and_languages() {
        let old_cfg = class(&["Chance of lead boulders per chunk.", "lead_chance", "other"]);
        let new_cfg = class(&[
            "Chance of lead boulders per chunk.",
            "lead_chance",
            "How often tin boulders appear in the world.",
            "tin_chance",
            "other",
        ]);
        let lang_old = br#"{"block.m.stone_boulder":"Stone Boulder"}"#;
        let lang_new = br#"{"block.m.stone_boulder":"Stone Boulder","block.m.lead_boulder":"Lead Boulder","block.m.tin_boulder":"Tin Boulder","m.title":"New title"}"#;
        let a = jar(
            &[
                ("assets/m/blockstates/stone_boulder.json", b"{}"),
                ("assets/m/models/item/stone_boulder.json", b"{}"),
                ("assets/m/lang/en_us.json", lang_old),
                ("m/Config.class", &old_cfg),
                ("m/Main.class", b"v1"),
            ],
            false,
        );
        let b = jar(
            &[
                ("assets/m/blockstates/stone_boulder.json", b"{}"),
                ("assets/m/blockstates/lead_boulder.json", b"{}"),
                ("assets/m/blockstates/tin_boulder.json", b"{}"),
                // A block's own item model is not a separate item.
                ("assets/m/models/item/lead_boulder.json", b"{}"),
                ("assets/m/models/item/stone_boulder.json", b"{}"),
                ("assets/m/models/item/wrench.json", b"{}"),
                ("assets/m/lang/en_us.json", lang_new),
                ("assets/m/lang/de_de.json", b"{}"),
                ("m/Config.class", &new_cfg),
                ("m/Main.class", b"v2"),
            ],
            false,
        );
        let r = compare(a, b).unwrap();
        assert_eq!(r.blocks_added, vec!["Lead Boulder", "Tin Boulder"]);
        assert_eq!(r.items_added, vec!["Wrench"]);
        assert_eq!(r.languages_added, vec!["German"]);
        assert_eq!(r.options_added.len(), 1);
        assert_eq!(r.options_added[0].name, "tin_chance");
        assert_eq!(r.options_added[0].description, "How often tin boulders appear in the world.");
        assert!(r.options_removed.is_empty());
        // Only the one text that isn't a block name.
        assert_eq!(r.texts, [1, 0, 0]);
        assert_eq!(r.verdict(), Verdict::Changes);
        let md = r.to_markdown();
        assert!(md.contains("**2 new blocks:** Lead Boulder, Tin Boulder"), "{md}");
        assert!(md.contains("`tin_chance`: How often tin boulders appear in the world."), "{md}");
        assert!(!md.contains("class"), "no class lines: {md}");
    }

    #[test]
    fn format_only_recipe_rename_is_not_a_change() {
        // Iron Furnaces-like: `conditions` became `neoforge:conditions`.
        let old = br#"{"type":"minecraft:crafting_shaped","conditions":[{"type":"forge:mod_loaded","modid":"x"}],"result":{"id":"m:a"}}"#;
        let new = br#"{"type":"minecraft:crafting_shaped","neoforge:conditions":[{"type":"neoforge:mod_loaded","modid":"x"}],"result":{"id":"m:a"}}"#;
        let real_old = br#"{"type":"minecraft:smelting","result":{"id":"m:a"}}"#;
        let real_new = br#"{"type":"minecraft:smelting","result":{"id":"m:b"}}"#;
        let a = jar(&[("data/m/recipe/a.json", old), ("data/m/recipe/b.json", real_old), ("m/Main.class", b"1")], false);
        let b = jar(&[("data/m/recipe/a.json", new), ("data/m/recipe/b.json", real_new), ("m/Main.class", b"2")], false);
        let r = compare(a, b).unwrap();
        let rec = r.data.iter().find(|d| d.kind == "recipe").unwrap();
        assert_eq!((rec.format_only, rec.changed), (1, 1));

        let a = jar(&[("data/m/recipe/a.json", old), ("m/Main.class", b"1")], false);
        let b = jar(&[("data/m/recipe/a.json", new), ("m/Main.class", b"2")], false);
        let r = compare(a, b).unwrap();
        assert_eq!(r.verdict(), Verdict::CodeOnly);
        let md = r.to_markdown();
        assert!(md.contains("no gameplay change"), "{md}");
        assert!(!md.contains("recipe changed") && !md.contains("recipes changed"), "{md}");
    }

    #[test]
    fn code_only_says_so_without_a_class_count() {
        let a = jar(&[("m/A.class", b"1"), ("m/B.class", b"1"), ("assets/m/lang/en_us.json", b"{}")], false);
        let b = jar(&[("m/A.class", b"2"), ("m/B.class", b"2"), ("assets/m/lang/en_us.json", b"{}")], false);
        let r = compare(a, b).unwrap();
        assert_eq!(r.verdict(), Verdict::CodeOnly);
        assert_eq!(r.to_markdown(), CODE_ONLY);
    }

    #[test]
    fn translations_added_are_listed_beside_recompiled_code() {
        // Pipez-like.
        let a = jar(&[("m/A.class", b"1"), ("assets/m/lang/en_us.json", b"{}")], false);
        let b = jar(&[("m/A.class", b"2"), ("assets/m/lang/en_us.json", b"{}"), ("assets/m/lang/es_es.json", b"{}"), ("assets/m/lang/es_mx.json", b"{}")], false);
        let r = compare(a, b).unwrap();
        assert_eq!(r.verdict(), Verdict::Changes);
        assert_eq!(r.languages_added, vec!["Spanish (Spain)", "Spanish (Mexico)"]);
        let md = r.to_markdown();
        assert!(md.contains("Languages added:"), "{md}");
        assert!(!md.contains("code"), "{md}");
    }

    #[test]
    fn many_blocks_are_grouped_not_listed() {
        let names: Vec<String> = (0..30).map(|i| format!("assets/m/blockstates/b{i:02}.json")).collect();
        let files: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b"{}"[..])).collect();
        let r = compare(jar(&[("m/A.class", b"1")], false), jar(&files, false)).unwrap();
        let md = r.to_markdown();
        assert!(md.contains("30 new blocks:") && md.contains("and 24 more"), "{md}");
        assert_eq!(md.lines().count(), 1);
    }

    #[test]
    fn unsafe_names_and_huge_entries_are_skipped() {
        let a = jar(&[("../evil.json", b"{}"), ("m/A.class", b"1")], false);
        let b = jar(&[("m/A.class", b"1")], false);
        let r = compare(a, b).unwrap();
        assert_eq!(r.verdict(), Verdict::NoChange);
        // A lang file over the read limit is not read, never a crash.
        let big = vec![b' '; (MAX_FILE + 1) as usize];
        let a = jar(&[("assets/m/lang/en_us.json", &big)], false);
        let b = jar(&[("assets/m/lang/en_us.json", b"{}")], false);
        assert!(compare(a, b).is_ok());
        assert!(compare(Cursor::new(b"not a zip".to_vec()), jar(&[], false)).is_err());
    }

    #[test]
    fn fabric_metadata_and_dependencies() {
        let a = br#"{"version":"1.0","depends":{"minecraft":">=1.21","fabricloader":">=0.15","java":">=21"},"suggests":{"modmenu":"*"}}"#;
        let b = br#"{"version":"1.1","depends":{"minecraft":">=1.21.1","fabricloader":">=0.15","java":">=21","lib":"*"}}"#;
        let r = compare(jar(&[("fabric.mod.json", a)], false), jar(&[("fabric.mod.json", b)], false)).unwrap();
        assert_eq!(r.minecraft, Some((">=1.21".into(), ">=1.21.1".into())));
        assert_eq!(r.deps_added, vec![Dep { id: "lib".into(), kind: "required".into() }]);
        assert_eq!(r.deps_removed, vec!["modmenu".to_string()]);
        assert_eq!(r.loader_name, "Fabric");
    }

    #[test]
    fn class_strings_reads_the_pool_and_rejects_junk() {
        assert_eq!(class_strings(&class(&["a", "bb"])).unwrap(), vec!["a", "bb"]);
        assert!(class_strings(b"\xCA\xFE\xBA\xBE\0\0\0\x41\0\x09\x01").is_none());
        assert!(class_strings(b"nope").is_none());
    }

    /// Prints the result for two real jars: `JAR_OLD=a.jar JAR_NEW=b.jar cargo test -- --ignored real_jars --nocapture`.
    #[test]
    #[ignore]
    fn real_jars() {
        let (Some(a), Some(b)) = (std::env::var_os("JAR_OLD"), std::env::var_os("JAR_NEW")) else { return };
        let r = compare_files(a.as_ref(), b.as_ref()).unwrap();
        println!("{:?}
{}", r.verdict(), r.to_markdown());
    }
}
