//! The check of a group copy against what its mods folder really has
//! (`hopper::groupverify`), and the repair of what it finds.
//!
//! Group sync works on the difference between codes, never on the files, so a
//! copy that silently lacks a mod said "same as the group" for good. Here the
//! copy is compared with the group's latest state, mod by mod:
//!
//! - the check runs off the UI thread when the copy or its mods change (never
//!   on the publish tick), and its answer sits here for the group strip;
//! - nothing is downloaded by a check. "Make my copy match" (and Pull now)
//!   apply exactly the rows the check found, through the same install path as
//!   sync's own changes.

use super::{Msg, Store, StoreEvent};
use gpui_kit::*;
use hopper::groupstatus::Check;
use hopper::groupverify::{self, Row, Verified, Why};
use hopper::sources::carry::Pin;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a check that couldn't finish waits before Hopper tries it again
/// by itself (a site that didn't answer).
const RETRY: Duration = Duration::from_secs(600);

#[derive(Default)]
pub struct State {
    /// The last check per group copy (its local instance id).
    list: HashMap<String, Read>,
    /// Group copies being checked now.
    checking: HashSet<String>,
    /// Group copies being repaired now.
    pub(super) repairing: HashSet<String>,
    /// Pull now is waiting for the repair: (changes it took in, whose).
    pulling: HashMap<String, (usize, Vec<String>)>,
    /// Name, project and icon of a mod by key, once looked up.
    names: HashMap<String, (String, u64, String)>,
}

struct Read {
    /// What the copy and its mods were when this was read ([`stamp`]).
    stamp: u64,
    /// The copy or its mods changed since: the answer is out of date.
    stale: bool,
    at: Instant,
    v: Verified,
}

/// A check's answer: the answer to [`Store::want_group_verify`].
pub struct Checked {
    local: String,
    stamp: u64,
    v: Verified,
    /// Names looked up this time: mod key -> (name, project, icon).
    found: Vec<(String, (String, u64, String))>,
}

/// What a repair did.
pub struct Repair {
    pub local: String,
    pub root: PathBuf,
    pub result: Result<hopper::groupverify::Repaired, String>,
}

/// What a copy and its mods are, as one number: the check runs again when it
/// changes. Cheap (no disk): the record the store holds.
fn stamp(inst: &hopper::instances::Instance, c: &hopper::groupsync::PackCopy, pending: &[hopper::modpack::Pending], allow: bool) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    allow.hash(&mut h);
    for a in &inst.addons {
        (&a.project_id, &a.file_id, &a.mr_version, &a.file_name, a.enabled, a.locked, a.from_pack).hash(&mut h);
    }
    for p in pending {
        (p.project_id, p.file_id, &p.file_name).hash(&mut h);
    }
    inst.only_here.keys().for_each(|k| k.hash(&mut h));
    for (k, g) in &c.got {
        (k, g.seq, g.code.len()).hash(&mut h);
    }
    for (k, m) in &c.marks {
        (k, m.seq).hash(&mut h);
    }
    for o in &c.owed {
        (&o.key, o.seq, o.approved).hash(&mut h);
    }
    (c.sent_seq, c.sent.len(), c.waiting.len()).hash(&mut h);
    h.finish()
}

impl Store {
    /// Checks the group copy at `root` against its mods folder unless it
    /// was checked since either changed. Nothing for an instance that isn't
    /// a group copy.
    pub(super) fn want_group_verify(&mut self, root: &Path) {
        if self.group_preview.is_some() || super::server_mods::remote(root).is_some() {
            return;
        }
        let Some(copy) = self.group_copy(root).cloned() else { return };
        let Some(inst) = self.instance(root).cloned() else { return };
        let allow = self.settings.allow_sources;
        let stamp = stamp(&inst, &copy, self.pending(root), allow);
        let local = copy.local.clone();
        match self.group_verify.list.get_mut(&local) {
            Some(r) if r.stamp == stamp => {
                // A check that couldn't finish is tried again after a while.
                if r.v.unchecked.is_empty() || r.at.elapsed() < RETRY {
                    return;
                }
            }
            Some(r) => r.stale = true,
            None => {}
        }
        // Not while something changes the instance: the answer would be
        // out of date as it arrives; its end changes the stamp and asks again.
        if self.group_verify.checking.contains(&local) || self.group_verify.repairing.contains(&local) || self.follow_busy(root) || self.group_merging.contains(&local) {
            return;
        }
        self.group_verify.checking.insert(local.clone());
        let names = self.group_verify.names.clone();
        self.spawn(move || {
            let mut v = groupverify::check(&inst, &copy, allow, unix_secs());
            let found = look_up_names(&inst, &mut v, &names);
            Msg::GroupChecked(Box::new(Checked { local, stamp, v, found }))
        });
    }

    pub(super) fn on_group_checked(&mut self, c: Checked, cx: &mut Context<Self>) {
        let Checked { local, stamp, v, found } = c;
        self.group_verify.checking.remove(&local);
        for (k, row) in found {
            if !row.2.is_empty() {
                self.want_image(&row.2);
            }
            self.group_verify.names.insert(k, row);
        }
        self.group_verify.list.insert(local, Read { stamp, stale: false, at: Instant::now(), v });
        cx.notify();
    }

    /// Looks at every group copy not checked yet or changed since (from the
    /// store's wake-ups).
    pub(super) fn seed_group_verify(&mut self) {
        let roots: Vec<PathBuf> = self
            .settings
            .group_copies
            .iter()
            .filter_map(|c| self.instances.iter().find(|i| i.id == c.local).map(|i| i.root.clone()))
            .collect();
        for root in roots {
            self.want_group_verify(&root);
        }
    }

    /// How the copy at `root` compares with the group, for the status.
    pub fn group_check(&self, root: &Path) -> Check {
        let Some(c) = self.group_copy(root) else { return Check::Checking };
        match self.group_verify.list.get(&c.local) {
            Some(r) if !r.stale => r.v.check(),
            _ => Check::Checking,
        }
    }

    /// The last check of the copy at `root`, when it is up to date.
    pub fn group_verified(&self, root: &Path) -> Option<&Verified> {
        let c = self.group_copy(root)?;
        self.group_verify.list.get(&c.local).filter(|r| !r.stale).map(|r| &r.v)
    }

    /// `--dialog group-sync --state blink|long|checking`: makes the check's
    /// answer out of date (or fresh again), as a change to the mods does,
    /// so the strip's Checking state can be watched.
    pub fn preview_check_stale(&mut self, root: &Path, stale: bool, cx: &mut Context<Self>) {
        let Some(local) = self.group_copy(root).map(|c| c.local.clone()) else { return };
        if let Some(r) = self.group_verify.list.get_mut(&local) {
            r.stale = stale;
        }
        cx.notify();
    }

    /// The name, project and icon url of a mod of the check's list by key.
    pub fn verify_name(&self, key: &str) -> Option<&(String, u64, String)> {
        self.group_verify.names.get(key)
    }

    /// The group copy at `root` is being made to match.
    pub fn group_repairing(&self, root: &Path) -> bool {
        self.group_copy(root).is_some_and(|c| self.group_verify.repairing.contains(&c.local))
    }

    /// Forgets what was checked, so the next wake-up looks again (Try again).
    pub fn recheck_group(&mut self, root: &Path, cx: &mut Context<Self>) {
        let Some(local) = self.group_copy(root).map(|c| c.local.clone()) else { return };
        if let Some(r) = self.group_verify.list.get_mut(&local) {
            r.stale = true;
            r.stamp = 0;
        }
        self.want_group_verify(root);
        cx.notify();
    }

    /// "Make my copy match": gets, updates or switches the mods the check
    /// listed, nothing else. `keys`: only these mods (a row's own button);
    /// None: every row that can be repaired. `approved`: the sources the
    /// player said yes to ("Allow and get it"). Nothing was downloaded before
    /// this.
    pub fn repair_group(&mut self, root: &Path, keys: Option<Vec<String>>, approved: Vec<Pin>, cx: &mut Context<Self>) {
        self.repair_group_inner(root, keys, approved, None, cx);
    }

    fn repair_group_inner(&mut self, root: &Path, keys: Option<Vec<String>>, approved: Vec<Pin>, pull: Option<(usize, Vec<String>)>, cx: &mut Context<Self>) {
        let Some(copy) = self.group_copy(root).cloned() else { return };
        let local = copy.local.clone();
        if self.group_verify.repairing.contains(&local) || self.group_merging.contains(&local) {
            return;
        }
        if self.group_preview.is_some() {
            self.preview_repair(&local, keys, cx);
            return;
        }
        if let Some(p) = pull {
            self.group_verify.pulling.insert(local.clone(), p);
        }
        self.group_verify.repairing.insert(local.clone());
        let name = self.instance(root).map(|i| i.name.clone()).unwrap_or_default();
        let allow = self.settings.allow_sources;
        let (tx, r2) = (self.tx.clone(), root.to_path_buf());
        // Quiet: it shows in Tasks when it changes something, as sync's own
        // merges do.
        self.spawn_instance_job_quiet(root, format!("Making {name} match the group"), move |job| {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| groupverify::repair(&r2, &copy, keys.as_deref(), &approved, allow, job)))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("Hopper hit a bug and stopped this task.")));
            match r {
                Ok(done) => Ok(Msg::GroupRepaired(Box::new(Repair { local, root: r2, result: Ok(done) }))),
                Err(e) => {
                    let text = format!("{e:#}");
                    let _ = tx.send(Msg::GroupRepaired(Box::new(Repair { local, root: r2, result: Err(text.clone()) })));
                    Err(anyhow::anyhow!(text))
                }
            }
        });
        cx.notify();
    }

    pub(super) fn on_group_repaired(&mut self, r: Repair, cx: &mut Context<Self>) {
        let Repair { local, root, result } = r;
        self.group_verify.repairing.remove(&local);
        // Pull now is waiting for this: its note stays until `finish_pull`
        // has told what it did.
        let pull = self.group_verify.pulling.get(&local).cloned();
        match result {
            Ok(mut done) => {
                // Who changed each mod last, and what is no longer owed.
                let merged = std::mem::take(&mut done.merged);
                self.update_settings_quietly(|st| {
                    let Some(c) = st.group_copies.iter_mut().find(|c| c.local == local) else { return };
                    for (k, mk) in merged.marks {
                        if c.marks.get(&k).is_none_or(|x| x.seq <= mk.seq) {
                            c.marks.insert(k, mk);
                        }
                    }
                    if let Some(owed) = merged.owed {
                        c.owed = owed;
                    }
                });
                let n = done.done.len();
                let by_hand = std::mem::take(&mut done.by_hand);
                for p in &by_hand {
                    self.kept_said.insert(format!("{local}|by_hand|{}", p.file_id));
                }
                let mut made_rows = std::mem::take(&mut done.done);
                self.name_list(&root, &mut made_rows);
                if let Some(mut after) = done.after.take() {
                    let mut rows = std::mem::take(&mut after.rows);
                    self.name_list(&root, &mut rows);
                    after.rows = rows;
                    let stamp = self
                        .settings
                        .group_copies
                        .iter()
                        .find(|c| c.local == local)
                        .zip(self.instance(&root))
                        .map(|(c, i)| {
                            let pending = hopper::modpack::pending_downloads(i);
                            stamp(i, c, &pending, self.settings.allow_sources)
                        })
                        .unwrap_or(0);
                    // The instance changed: this answer is replaced by the
                    // next one (the stamp differs), but it says the result
                    // now.
                    self.group_verify.list.insert(local.clone(), Read { stamp, stale: n > 0, at: Instant::now(), v: after });
                }
                self.want_group_needs(&root);
                if n > 0 || !by_hand.is_empty() {
                    self.handle(Msg::InstanceChanged(root.clone()), cx);
                }
                match pull {
                    // Pull now: what went in counts with what it took in.
                    Some((made, mut from)) => {
                        from.extend(made_rows.iter().map(|r| r.by_name.clone()).filter(|n| !n.is_empty()));
                        from.dedup();
                        self.finish_pull(&local, &root, made + n, from, cx);
                    }
                    None if n > 0 || !by_hand.is_empty() => {
                        let names: Vec<String> = by_hand.iter().map(|p| p.name.clone()).collect();
                        let text = match (n > 0, !names.is_empty()) {
                            (true, false) => groupverify::done_says(&made_rows),
                            (false, true) => hopper::groupstatus::by_hand_says(&names),
                            (true, true) => format!("{} {}", groupverify::done_says(&made_rows), hopper::groupstatus::by_hand_says(&names)),
                            (false, false) => String::new(),
                        };
                        if !text.is_empty() {
                            cx.emit(StoreEvent::Notice(text, false));
                        }
                    }
                    None => {}
                }
            }
            Err(why) => {
                log::warn!("making {local} match the group: {why}");
                // Some may have gone in: look again.
                self.recheck_group(&root, cx);
                self.handle(Msg::InstanceChanged(root.clone()), cx);
                let text = format!("Couldn't make your copy match the group. {why}");
                self.group_verify.pulling.remove(&local);
                self.group_pull_from.remove(&local);
                self.group_pulling.remove(&local);
                self.group_pulled.insert(local.clone(), (text.clone(), true, Instant::now()));
                cx.emit(StoreEvent::Notice(text, true));
            }
        }
        cx.notify();
    }

    /// Pull now, after it read the group and took in what waited: makes the
    /// copy match what the group has, which is what "in step" never looked
    /// at. Returns true when that started; `finish_pull` runs again with its
    /// answer, and is not asked twice (the note in `pulling`).
    pub(super) fn pull_repairs(&mut self, local: &str, root: &Path, made: usize, from: &[String], cx: &mut Context<Self>) -> bool {
        if self.group_preview.is_some() || self.group_verify.pulling.contains_key(local) {
            return false;
        }
        // Only when the group answered: without it, what this PC knows of the
        // group is what it knew before, and Make my copy match is there.
        if !self.group_pulling.get(local).copied().unwrap_or(false) {
            return false;
        }
        self.repair_group_inner(root, None, Vec::new(), Some((made, from.to_vec())), cx);
        self.group_verify.pulling.contains_key(local)
    }

    /// Pull now has told what it did: it isn't waiting for a repair any more.
    pub(super) fn pull_told(&mut self, local: &str) {
        self.group_verify.pulling.remove(local);
    }

    /// Names for the rows of a list: the mod here, else what was looked up.
    fn name_list(&self, root: &Path, rows: &mut [Row]) {
        let inst = self.instance(root);
        for r in rows.iter_mut().filter(|r| r.name.is_empty()) {
            if let Some((n, _, _)) = self.group_verify.names.get(&r.key) {
                r.name = n.clone();
            } else if let Some(a) = inst.and_then(|i| hopper::groupsync::find_addon(i, &r.key).map(|ix| &i.addons[ix])) {
                r.name = a.name.clone();
            }
        }
    }

    /// `--dialog group-sync --state verify-*`: a made-up check, to see every
    /// state of the list without a friend, a site or a mods folder. Nothing
    /// is downloaded: repairing it only clears the rows that can be.
    pub(super) fn preview_group_verify(&mut self, root: &Path, local: &str, state: &str) {
        let cf = |key: &str, name: &str, project: u64, why: Why, by: &str| {
            let change = hopper::groupsync::Change {
                key: key.into(),
                was: hopper::groupsync::State::Unlisted,
                now: hopper::groupsync::State::File { file: format!("cf:{project}1"), on: !matches!(why, Why::OnHere) },
                entry: Some(hopper::sharecode::Entry::CurseFile { file: project * 10 + 1, enabled: true }),
            };
            Row {
                key: key.into(),
                name: name.into(),
                project,
                why,
                older: matches!(why, Why::Different),
                change: why.repairable().then_some(change),
                pin: None,
                by: by.to_lowercase(),
                by_name: by.into(),
                mark: None,
            }
        };
        let v = match state {
            "verify-matches" => Verified { at: unix_secs(), ..Default::default() },
            "verify-cant" => Verified { unchecked: vec![hopper::sharecode::Entry::CurseFile { file: 7, enabled: true }], at: unix_secs(), ..Default::default() },
            "verify-kept" => Verified {
                rows: vec![cf("p:5", "Create", 328085, Why::OnlyHere, "Mia"), cf("p:6", "Sodium", 394468, Why::Locked, "Noah")],
                at: unix_secs(),
                ..Default::default()
            },
            "verify-behind" => Verified {
                rows: vec![
                    cf("p:1", "Jade", 324717, Why::Missing, "Mia"),
                    cf("p:2", "Xaero's Minimap", 263420, Why::Missing, "Mia"),
                ],
                at: unix_secs(),
                ..Default::default()
            },
            // Every reason once, the longest names the list will meet.
            _ => Verified {
                rows: vec![
                    cf("p:1", "Jade", 324717, Why::Missing, "Mia"),
                    cf("p:2", "Applied Energistics 2 (ae2-neoforge-19.2.17)", 223794, Why::Different, "Noah"),
                    cf("p:3", "Mekanism", 268560, Why::OffHere, "Mia"),
                    cf("p:4", "Just Enough Items", 238222, Why::OnHere, "Ava"),
                    cf("p:7", "EZActions", 0, Why::LeftOut, "Mia"),
                    cf("p:5", "Create", 328085, Why::OnlyHere, "Mia"),
                    cf("p:6", "Sodium", 394468, Why::Locked, "Noah"),
                ],
                at: unix_secs(),
                ..Default::default()
            },
        };
        self.group_verify.list.insert(local.to_string(), Read { stamp: u64::MAX, stale: false, at: Instant::now(), v });
        let _ = root;
    }

    /// Repair in the made-up group: the rows that can be made go.
    fn preview_repair(&mut self, local: &str, keys: Option<Vec<String>>, cx: &mut Context<Self>) {
        if let Some(r) = self.group_verify.list.get_mut(local) {
            r.v.rows.retain(|row| !(row.fixable() && row.why != Why::LeftOut && keys.as_ref().is_none_or(|k| k.contains(&row.key))));
        }
        cx.notify();
    }
}

fn unix_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Names for the rows of `v` that have none: the mod here when there is one,
/// else asked from the sites in one batch for CurseForge and one by one for
/// the few from Modrinth. Returns what was found, for the store's cache.
fn look_up_names(inst: &hopper::instances::Instance, v: &mut Verified, known: &HashMap<String, (String, u64, String)>) -> Vec<(String, (String, u64, String))> {
    let mut found: Vec<(String, (String, u64, String))> = Vec::new();
    let mut ask: Vec<(String, u64)> = Vec::new();
    for r in &mut v.rows {
        if let Some(i) = hopper::groupsync::find_addon(inst, &r.key) {
            let a = &inst.addons[i];
            if r.name.is_empty() {
                r.name = a.name.clone();
            }
            found.push((r.key.clone(), (a.name.clone(), r.project, a.icon.clone())));
        } else if let Some((n, _, _)) = known.get(&r.key) {
            r.name = n.clone();
        } else if r.name.is_empty() && r.project != 0 {
            ask.push((r.key.clone(), r.project));
        }
    }
    let (modrinth, curse): (Vec<_>, Vec<_>) = ask.into_iter().partition(|(_, p)| hopper::modrinth::is_mr(*p));
    if !curse.is_empty() {
        let ids: Vec<u64> = curse.iter().map(|(_, p)| *p).collect();
        if let Ok(mods) = hopper::cf::get_mods(&ids) {
            for (key, p) in &curse {
                if let Some(m) = mods.iter().find(|m| m.id == *p) {
                    found.push((key.clone(), (m.name.clone(), m.id, m.icon().unwrap_or_default().to_string())));
                }
            }
        }
    }
    for (key, p) in modrinth.iter().take(12) {
        if let Ok(m) = hopper::source::get_mod(*p) {
            found.push((key.clone(), (m.name.clone(), m.id, m.icon().unwrap_or_default().to_string())));
        }
    }
    for r in &mut v.rows {
        if r.name.is_empty()
            && let Some((_, (n, _, _))) = found.iter().find(|(k, _)| *k == r.key)
        {
            r.name = n.clone();
        }
    }
    found
}
