//! An engine's saved state, `data/sync/<copy>.json` (atomic write). IO.
//!
//! The file holds the log (the registers are folded from it again on load),
//! what was applied, trust, the members' last status and the small sets the
//! engine keeps. A file that can't be read, is another version or belongs to
//! another copy is set aside as `<copy>.json.bad` and the copy starts clean:
//! it is refilled from the next checkpoint a friend sends. The old group
//! sync's saved state is never read or migrated.

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
        // Set by the app on each start, so not saved.
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
    fn an_odd_copy_id_is_a_safe_file_name() {
        let p = path(Path::new("data"), "cf:123/456 \\ x");
        assert_eq!(p.file_name().unwrap().to_string_lossy(), "cf_123_456___x.json");
    }
}
