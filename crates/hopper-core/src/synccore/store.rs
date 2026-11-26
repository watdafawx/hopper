//! An engine's saved state, `data/sync/<copy>.json` (atomic write). IO.
//!
//! The file holds the log (the registers are folded from it again on load),
//! what was applied, trust, the members' last status and the small sets the
//! engine keeps. A file that can't be read, is another version or belongs to
//! another copy is set aside as `<copy>.json.bad` and the copy starts clean:
//! it is refilled from the next checkpoint a friend sends. The old group
//! sync's saved state is never read or migrated.

use super::digest::StateEntry;
use super::engine::Engine;
use super::gate::{Limits, Trust};
use super::model::{Arrival, Change, Dot, Key, MemberId, Value};
use super::register::Log;
use super::wire::MemberStatus;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Saved files of another version are ignored.
const VERSION: u8 = 1;

#[derive(Serialize, Deserialize)]
struct Saved {
    version: u8,
    copy: String,
    me: MemberId,
    name: String,
    /// Every record with how the relay numbered it. A list, because a dot is
    /// not a text and JSON object keys must be.
    log: Vec<(Change, Option<Arrival>)>,
    /// Pairs, for the same reason.
    applied: Vec<(Key, Value)>,
    trust: Vec<(MemberId, Trust)>,
    limits: Limits,
    members: Vec<MemberStatus>,
    taken: Vec<Dot>,
    allowed_sources: Vec<String>,
    told: Vec<Dot>,
    contact: u64,
    outdated: Vec<MemberId>,
    /// Each friend's list of mods with the hash it was sent for (a file
    /// from before this field has none).
    #[serde(default)]
    states: Vec<(MemberId, String, Vec<StateEntry>)>,
}

/// The engine as bytes.
pub fn encode(e: &Engine) -> Vec<u8> {
    let saved = Saved {
        version: VERSION,
        copy: e.copy.clone(),
        me: e.me.clone(),
        name: e.name.clone(),
        log: e.log.changes.values().cloned().collect(),
        applied: e.applied.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        trust: e.trust.iter().map(|(m, t)| (m.clone(), *t)).collect(),
        limits: e.limits,
        members: e.members.values().cloned().collect(),
        taken: e.taken.iter().cloned().collect(),
        allowed_sources: e.allowed_sources.iter().cloned().collect(),
        told: e.told.iter().cloned().collect(),
        contact: e.contact,
        outdated: e.outdated.iter().cloned().collect(),
        states: e.states.iter().map(|(m, (h, l))| (m.clone(), h.clone(), l.clone())).collect(),
    };
    serde_json::to_vec(&saved).expect("saved state is plain data")
}

/// An engine from bytes; `None` when they are damaged, another version, or
/// another copy's.
pub fn decode(bytes: &[u8], copy: &str) -> Option<Engine> {
    let s: Saved = serde_json::from_slice(bytes).ok()?;
    if s.version != VERSION || s.copy != copy {
        return None;
    }
    Some(Engine {
        copy: s.copy,
        me: s.me,
        name: s.name,
        log: Log { changes: s.log.into_iter().map(|(c, a)| (c.dot.clone(), (c, a))).collect() },
        applied: s.applied.into_iter().collect::<BTreeMap<_, _>>(),
        trust: s.trust.into_iter().collect(),
        limits: s.limits,
        members: s.members.into_iter().map(|m| (m.member.clone(), m)).collect(),
        taken: s.taken.into_iter().collect::<BTreeSet<_>>(),
        allowed_sources: s.allowed_sources.into_iter().collect(),
        told: s.told.into_iter().collect(),
        contact: s.contact,
        outdated: s.outdated.into_iter().collect(),
        states: s.states.into_iter().map(|(m, h, l)| (m, (h, l))).collect(),
        // Set by the app on each start, so not saved.
        kept_here: BTreeSet::new(),
        game_running: false,
        edited_configs: Vec::new(),
        retry: BTreeMap::new(),
        blocked: BTreeMap::new(),
        posting: Default::default(),
    })
}

/// `<data>/sync/<copy>.json`; the copy id as a safe file name.
pub fn path(data: &Path, copy: &str) -> PathBuf {
    let safe: String = copy
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' })
        .collect();
    data.join("sync").join(format!("{safe}.json"))
}

/// The saved engine of `copy`: `None` when there is none, or when the file
/// was no good (it is kept beside as `.bad` and the copy starts clean).
pub fn load(data: &Path, copy: &str) -> anyhow::Result<Option<Engine>> {
    let p = path(data, copy);
    let bytes = match std::fs::read(&p) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    match decode(&bytes, copy) {
        Some(e) => Ok(Some(e)),
        None => {
            let mut bad = p.clone().into_os_string();
            bad.push(".bad");
            let _ = std::fs::rename(&p, PathBuf::from(bad));
            Ok(None)
        }
    }
}

/// Saves through a temp file and a rename, so a crash leaves the old file
/// or the new one, never half of one.
pub fn save(data: &Path, engine: &Engine) -> anyhow::Result<()> {
    let p = path(data, &engine.copy);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::instances::write_atomic(&p, &encode(engine))
}

// ---- The jar memory ---------------------------------------------------------------

/// Each jar a copy has had (its name, lower case, without `.disabled`) with
/// the mod it is and the group's file id for it. History's reason lines
/// ("Bob went back to 1.4.8", "Bob chose Use mine for everyone") find the
/// change behind a row through it, also for a jar that is gone. Kept beside
/// the engine's file so it survives a restart.
pub type Jars = BTreeMap<String, (Key, String)>;

/// The most jars the memory keeps; it is pruned to this when saved.
pub const JARS_MAX: usize = 4000;

/// `<data>/sync/<copy>.jars.json`.
pub fn jars_path(data: &Path, copy: &str) -> PathBuf {
    let p = path(data, copy);
    p.with_file_name(format!("{}.jars.json", p.file_stem().unwrap_or_default().to_string_lossy()))
}

/// The saved memory of `copy`; empty when there is none or the file is no
/// good (it only improves History's wording, so it is never an error).
pub fn load_jars(data: &Path, copy: &str) -> Jars {
    std::fs::read(jars_path(data, copy))
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<(String, Key, String)>>(&b).ok())
        .map(|v| v.into_iter().map(|(jar, key, file)| (jar, (key, file))).collect())
        .unwrap_or_default()
}

/// What of `jars` is worth keeping: a jar of a mod the log has a change of
/// (the only ones a reason line looks up), at most [`JARS_MAX`] of them. The
/// rest are re-read from the instance on every start anyway.
pub fn prune_jars(jars: &mut Jars, engine: &Engine) {
    let keys: BTreeSet<&Key> = engine.log.changes.values().map(|(c, _)| &c.key).collect();
    jars.retain(|_, (key, _)| keys.contains(key));
    while jars.len() > JARS_MAX {
        let Some(first) = jars.keys().next().cloned() else { break };
        jars.remove(&first);
    }
}

/// Saves the pruned memory of the engine's copy (atomic, like [`save`]).
pub fn save_jars(data: &Path, engine: &Engine, jars: &Jars) -> anyhow::Result<()> {
    let mut jars = jars.clone();
    prune_jars(&mut jars, engine);
    let p = jars_path(data, &engine.copy);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let list: Vec<(&String, &Key, &String)> = jars.iter().map(|(j, (k, f))| (j, k, f)).collect();
    crate::instances::write_atomic(&p, &serde_json::to_vec(&list)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synccore::model::{Arrival, Edit, Why};

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopper-synccore-store-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn key(n: u64) -> Key {
        Key::Mod(format!("cf:{n}"))
    }

    fn file(n: u64) -> Value {
        Value::File { file: format!("cf:{n}"), on: true }
    }

    /// An engine with its own records, a friend's, a status and the extras.
    fn busy() -> Engine {
        let mut e = Engine::new("pack-1", "me", "Me");
        e.applied.insert(key(1), file(11));
        e.record(Edit { key: key(1), now: file(12), why: Why::Player, batch: "b".into() }, 5);
        let mut friend = Engine::new("pack-1", "mia", "Mia");
        friend.record(Edit { key: key(2), now: Value::Gone, why: Why::Player, batch: String::new() }, 9);
        let packet = friend.checkpoint();
        e.ingest("mia", packet, friend.my_status(), Arrival { seq: 3, at: 100 });
        e.set_trust("mia", Trust::AskFirst);
        e.allow_source(&Key::Mod("src:a.jar".into()));
        e.outdated.insert("noah".into());
        e
    }

    #[test]
    fn save_then_load_gives_the_same_engine() {
        let d = scratch("roundtrip");
        let e = busy();
        save(&d, &e).unwrap();
        let back = load(&d, "pack-1").unwrap().expect("saved");
        assert_eq!(back, e);
        assert!(path(&d, "pack-1").starts_with(d.join("sync")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_copy_never_saved_loads_as_none() {
        let d = scratch("none");
        assert!(load(&d, "pack-1").unwrap().is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_damaged_file_starts_clean_and_is_kept_aside() {
        let d = scratch("corrupt");
        let p = path(&d, "pack-1");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"{\"version\":1,\"copy\":\"pa").unwrap();
        assert!(load(&d, "pack-1").unwrap().is_none());
        assert!(!p.exists());
        assert!(p.with_file_name("pack-1.json.bad").exists());
        // The next save works.
        save(&d, &busy()).unwrap();
        assert!(load(&d, "pack-1").unwrap().is_some());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn another_version_or_another_copy_is_not_read() {
        let e = busy();
        let mut v: serde_json::Value = serde_json::from_slice(&encode(&e)).unwrap();
        assert!(decode(&encode(&e), "pack-2").is_none());
        v["version"] = 2.into();
        assert!(decode(&serde_json::to_vec(&v).unwrap(), "pack-1").is_none());
        // What the old group sync saved is not this format.
        assert!(decode(b"{\"groups\":{\"g1\":{\"packs\":{}}}}", "pack-1").is_none());
        assert!(decode(b"", "pack-1").is_none());
    }

    #[test]
    fn a_save_leaves_no_temp_file_and_replaces_the_old_one() {
        let d = scratch("atomic");
        let mut e = busy();
        save(&d, &e).unwrap();
        e.record(Edit { key: key(3), now: file(31), why: Why::Player, batch: String::new() }, 0);
        save(&d, &e).unwrap();
        let names: Vec<String> = std::fs::read_dir(d.join("sync"))
            .unwrap()
            .map(|f| f.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["pack-1.json"]);
        assert_eq!(load(&d, "pack-1").unwrap().unwrap(), e);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_jar_memory_survives_a_restart() {
        let d = scratch("jars");
        let e = busy();
        let mut jars = Jars::new();
        jars.insert("a-1.jar".into(), (key(1), "cf:11".into()));
        jars.insert("a-2.jar".into(), (key(1), "cf:12".into()));
        // A mod the log has no change of is not kept: it is re-read on start.
        jars.insert("quiet.jar".into(), (key(99), "cf:990".into()));
        save_jars(&d, &e, &jars).unwrap();
        let back = load_jars(&d, "pack-1");
        assert_eq!(back.len(), 2);
        assert_eq!(back.get("a-1.jar"), Some(&(key(1), "cf:11".to_string())));
        assert!(!back.contains_key("quiet.jar"));
        // Beside the engine's file, which doesn't read it as a copy's state.
        assert_eq!(jars_path(&d, "pack-1").file_name().unwrap().to_string_lossy(), "pack-1.jars.json");
        save(&d, &e).unwrap();
        assert_eq!(load(&d, "pack-1").unwrap().unwrap(), e);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_missing_or_damaged_jar_memory_reads_as_empty() {
        let d = scratch("jars-bad");
        assert!(load_jars(&d, "pack-1").is_empty());
        let p = jars_path(&d, "pack-1");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"[[\"a.jar\",{\"Mod\":\"cf:1\"},\"cf").unwrap();
        assert!(load_jars(&d, "pack-1").is_empty());
        std::fs::write(&p, b"{\"not\":\"a list\"}").unwrap();
        assert!(load_jars(&d, "pack-1").is_empty());
        // The next save replaces it.
        let mut jars = Jars::new();
        jars.insert("a.jar".into(), (key(1), "cf:11".into()));
        save_jars(&d, &busy(), &jars).unwrap();
        assert_eq!(load_jars(&d, "pack-1").len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_jar_memory_stays_small() {
        let e = busy();
        let mut jars = Jars::new();
        for i in 0..JARS_MAX + 500 {
            jars.insert(format!("jar-{i:05}.jar"), (key(1), format!("cf:{i}")));
        }
        prune_jars(&mut jars, &e);
        assert_eq!(jars.len(), JARS_MAX);
    }

    #[test]
    fn an_odd_copy_id_is_a_safe_file_name() {
        let p = path(Path::new("data"), "cf:123/456 \\ x");
        assert_eq!(p.file_name().unwrap().to_string_lossy(), "cf_123_456___x.json");
    }
}
