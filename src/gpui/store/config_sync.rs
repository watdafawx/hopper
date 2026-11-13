//! Shared mod settings in a friend group, on the store (`hopper::configsync`,
//! `design/config-sync.md`): taking members' settings in as their recipes
//! arrive, reading this player's changes and writing the group's while the
//! game is closed, and the switches the Configs tab and Mods tab show.

use super::{Msg, Store, StoreEvent};
use gpui_kit::*;
use hopper::configsync::{self, Configs, Taken, Ticked, Waiting, Wrote1};
use hopper::groupsync::addon_key;
use hopper::mailbox::Event;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How often shared settings files are read for changes (and the group's
/// written in) while friends sync runs.
const TICK_EVERY: Duration = Duration::from_secs(2);

/// A mod's settings files by mod key, with its name.
type ModFiles = BTreeMap<String, (String, Vec<String>)>;

/// The store's side of shared settings.
#[derive(Default)]
pub struct CfgState {
    at: Option<Instant>,
    /// Copies (local ids) being read or written now.
    busy: HashSet<String>,
    /// Each copy's mods' settings files, and the mod list they were read for.
    files: HashMap<String, (u64, ModFiles)>,
    /// Settings that wait for a different version of the mod, by copy.
    waiting: HashMap<String, Vec<Waiting>>,
    /// What waits for the game to close, by copy, as last said.
    held: HashMap<String, Vec<(String, usize, String)>>,
    /// Copies whose shared settings are too many to send.
    too_big: HashSet<String>,
    /// How many settings differ from the pack, by (root, mod), for the
    /// switch's confirm line; None inside: the pack ships none of its files.
    differs: HashMap<(PathBuf, String), Option<usize>>,
    differs_asked: HashSet<(PathBuf, String)>,
    /// The mailbox passes shared settings on (None: not asked yet).
    relay: Option<bool>,
    relay_asked: bool,
}

/// What a background read / write of one copy found.
pub struct CfgTicked {
    local: String,
    root: PathBuf,
    sig: u64,
    files: ModFiles,
    r: anyhow::Result<Ticked>,
}

/// What waits for a game to close.
pub struct CfgHeld {
    local: String,
    root: PathBuf,
    held: Vec<(String, usize, String)>,
}

fn addons_sig(i: &hopper::instances::Instance) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for a in &i.addons {
        (&a.file_name, a.project_id, a.file_id).hash(&mut h);
    }
    h.finish()
}

impl Store {
    /// Whether shared settings can be offered: friends sync is on and the
    /// mailbox passes them on (an older one would drop them).
    pub fn settings_sharing(&self) -> bool {
        self.mailbox.is_some() && self.cfg.relay == Some(true)
    }

    /// The group's shared settings for the instance at `root`, when it's a
    /// group pack kept both ways.
    pub fn group_configs(&self, root: &Path) -> Option<&Configs> {
        self.group_copy(root).map(|c| &c.configs)
    }

    /// The group's name for a copy at `root` ("Survival crew").
    pub fn group_of(&self, root: &Path) -> Option<String> {
        let c = self.group_copy(root)?;
        Some(self.mailbox_groups().iter().find(|g| g.id == c.group).map(|g| g.name.clone()).unwrap_or_else(|| "the group".into()))
    }

    /// A mod's name in the instance at `root`, by key.
    pub fn mod_name_of(&self, root: &Path, m: &str) -> String {
        self.instance(root).and_then(|i| i.addons.iter().find(|a| addon_key(a) == m)).map(|a| a.name.clone()).unwrap_or_else(|| "this mod".into())
    }

    /// Whether the mod with this jar (file name) at `root` has its settings
    /// shared with the group.
    pub fn settings_shared_jar(&self, root: &Path, jar: &str) -> bool {
        let Some(c) = self.group_configs(root) else { return false };
        let jar = jar.trim_end_matches(".disabled");
        self.instance(root).and_then(|i| i.addons.iter().find(|a| a.file_name.eq_ignore_ascii_case(jar))).is_some_and(|a| c.is_shared(&addon_key(a)))
    }

    /// Whether Keep mine is on for mod `m` at `root`.
    pub fn settings_kept(&self, root: &Path, m: &str) -> bool {
        configsync::Local::load(root).keep.contains(m)
    }

    /// Settings waiting for another version of the mod at `root`.
    pub fn settings_waiting(&self, root: &Path) -> &[Waiting] {
        let id = self.instance(root).map(|i| i.id.clone()).unwrap_or_default();
        self.cfg.waiting.get(&id).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Whether the copy at `root` has more shared settings than fit.
    pub fn settings_too_big(&self, root: &Path) -> bool {
        self.instance(root).is_some_and(|i| self.cfg.too_big.contains(&i.id))
    }

    /// How many of mod `m`'s settings differ from the pack's own copy, for
    /// "Sends the N settings you changed from the pack": None while it's
    /// being counted; Some(None) when the pack ships none of its files.
    pub fn settings_differ(&mut self, root: &Path, m: &str, cx: &mut Context<Self>) -> Option<Option<usize>> {
        let k = (root.to_path_buf(), m.to_string());
        if let Some(n) = self.cfg.differs.get(&k) {
            return Some(*n);
        }
        if self.cfg.differs_asked.insert(k.clone()) {
            let inst = self.instance(root)?.clone();
            cx.spawn(async move |this, cx| {
                let m = k.1.clone();
                let n = cx.background_spawn(async move { configsync::count_differs(&inst, &m) }).await;
                this.update(cx, |s, cx| {
                    s.cfg.differs.insert(k, n);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        None
    }

    /// Shares mod `m`'s settings with the group (or stops): every member's
    /// switch follows, the newest change winning. Turning it on sends the
    /// settings that differ from the pack, then every later change.
    pub fn set_settings_shared(&mut self, root: &Path, m: &str, on: bool, cx: &mut Context<Self>) {
        let me = self.mailbox_id();
        let Some(inst) = self.instance(root).cloned() else { return };
        if me.is_empty() || self.group_copy(root).is_none() {
            return;
        }
        let local = inst.id.clone();
        let m = m.to_string();
        if !on {
            self.update_settings_quietly(|st| {
                if let Some(c) = st.group_copies.iter_mut().find(|c| c.local == local) {
                    c.configs.turn(&m, false, &me);
                }
            });
            cx.notify();
            return;
        }
        // The snapshot comes from the pack's copy first, so the settings
        // tuned from the pack count as changes on the next read.
        let r = root.to_path_buf();
        cx.spawn(async move |this, cx| {
            let m2 = m.clone();
            let n = cx.background_spawn(async move { configsync::start_share(&r, &inst, &m2) }).await;
            this.update(cx, |s, cx| {
                if let Err(e) = n {
                    cx.emit(StoreEvent::Notice(format!("Couldn't share these settings. {e:#}"), true));
                    return;
                }
                s.update_settings_quietly(|st| {
                    if let Some(c) = st.group_copies.iter_mut().find(|c| c.local == local) {
                        c.configs.turn(&m, true, &me);
                    }
                });
                s.cfg.at = None;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Keep mine for mod `m` at `root`: friends' changes to it are kept for
    /// later but not written here (off: they go in now).
    pub fn keep_settings(&mut self, root: &Path, m: &str, on: bool, cx: &mut Context<Self>) {
        match configsync::keep(root, m, on) {
            Ok(()) => {
                let name = self.mod_name_of(root, m);
                let text = if on {
                    format!("Keeping your {name} settings. Friends' changes to them wait until you sync again.")
                } else {
                    format!("{name} settings match the group again.")
                };
                cx.emit(StoreEvent::Notice(text, false));
                self.cfg.at = None;
            }
            Err(e) => cx.emit(StoreEvent::Notice(format!("Couldn't change that. {e:#}"), true)),
        }
        cx.notify();
    }

    /// Undo for settings a friend's change wrote: the files go back, and
    /// the group's values for them aren't written here again.
    pub fn undo_settings(&mut self, root: &Path, w: &Wrote1, cx: &mut Context<Self>) {
        match configsync::undo(root, w) {
            Ok(()) => {
                cx.emit(StoreEvent::Notice(format!("Your {} settings are back.", w.mod_name), false));
                self.handle(Msg::InstanceChanged(root.to_path_buf()), cx);
            }
            Err(e) => cx.emit(StoreEvent::Notice(format!("Couldn't undo that. {e:#}"), true)),
        }
    }

    /// A member's recipe of a group pack arrived: their shared settings go
    /// into this PC's copies of it. Returns what changed, by copy root, so
    /// the caller can tell the player who started sharing what.
    pub(super) fn take_configs(&mut self, e: &Event) -> Vec<(PathBuf, Taken)> {
        let me = self.mailbox_id();
        if e.configs.is_empty() || e.from == me {
            return Vec::new();
        }
        let Some(theirs) = configsync::decode(&e.configs) else {
            log::warn!("shared settings from {} didn't read", e.from);
            return Vec::new();
        };
        let names: HashMap<String, String> = self.mailbox_friends().iter().map(|f| (f.id.clone(), f.name.clone())).collect();
        let from_name = if e.from_name.is_empty() { names.get(&e.from).cloned().unwrap_or_default() } else { e.from_name.clone() };
        let mut out = Vec::new();
        let mut copies = self.settings.group_copies.clone();
        for c in copies.iter_mut().filter(|c| c.inst == e.inst) {
            let t = configsync::take(&mut c.configs, &e.from, &from_name, e.seq, e.at, &theirs, &me, |id| names.get(id).cloned().unwrap_or_default());
            if !t.is_empty()
                && let Some(i) = self.instances.iter().find(|i| i.id == c.local)
            {
                out.push((i.root.clone(), t));
            }
        }
        if copies != self.settings.group_copies {
            self.update_settings_quietly(|st| st.group_copies = copies);
            self.cfg.at = None;
        }
        out
    }

    /// Tells the player who started or stopped sharing which mod's settings.
    pub(super) fn say_taken(&mut self, taken: Vec<(PathBuf, Taken)>, cx: &mut Context<Self>) {
        for (root, t) in taken {
            for (m, on, who) in t.turned {
                let name = self.mod_name_of(&root, &m);
                let who = if who.is_empty() { "A friend".to_string() } else { who };
                if on {
                    cx.emit(StoreEvent::SettingsShared { root: root.clone(), m, mod_name: name, who });
                } else {
                    cx.emit(StoreEvent::Notice(format!("{who} stopped sharing {name} settings. Yours stay as they are."), false));
                }
            }
        }
    }

    /// Reads this player's changes to shared settings and writes the
    /// group's, for every group copy with shared mods, while its game is
    /// closed. While it runs, says once what waits for it to close.
    pub(super) fn config_tick(&mut self, cx: &mut Context<Self>) {
        if self.mailbox.is_some() && !self.cfg.relay_asked {
            self.cfg.relay_asked = true;
            let mb = self.mailbox.clone();
            cx.spawn(async move |this, cx| {
                let ok = cx.background_spawn(async move { mb.map(|m| m.shares_settings().unwrap_or(false)) }).await;
                this.update(cx, |s, cx| {
                    s.cfg.relay = ok;
                    if ok.is_none() {
                        s.cfg.relay_asked = false;
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        if self.cfg.at.is_some_and(|t| t.elapsed() < TICK_EVERY) {
            return;
        }
        self.cfg.at = Some(Instant::now());
        let me = self.mailbox_id();
        if me.is_empty() {
            return;
        }
        let copies: Vec<_> = self.settings.group_copies.iter().filter(|c| c.configs.shared().next().is_some()).cloned().collect();
        for c in copies {
            if self.cfg.busy.contains(&c.local) || self.group_merging.contains(&c.local) {
                continue;
            }
            let Some(inst) = self.instances.iter().find(|i| i.id == c.local).cloned() else { continue };
            let root = inst.root.clone();
            if !root.is_dir() || self.follow_busy(&root) {
                continue;
            }
            let sig = addons_sig(&inst);
            let cached = self.cfg.files.get(&c.local).filter(|(s, _)| *s == sig).map(|(_, f)| f.clone());
            let running = self.running(&root).is_some();
            let group = c.group.clone();
            let (tx, local, state, me) = (self.tx.clone(), c.local.clone(), c.configs.clone(), me.clone());
            self.cfg.busy.insert(local.clone());
            std::thread::spawn(move || {
                let files = cached.unwrap_or_else(|| configsync::mod_files(&inst));
                if running {
                    // Nothing is written while the game runs: it would write
                    // its own values back on close.
                    let held = configsync::pending(&root, &state, &files, &me);
                    let _ = tx.send(Msg::ConfigHeld(Box::new(CfgHeld { local, root, held })));
                    return;
                }
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| configsync::tick(&root, &state, &files, &me, &group)))
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("Hopper hit a bug while reading shared settings.")));
                let _ = tx.send(Msg::ConfigTicked(Box::new(CfgTicked { local, root, sig, files, r })));
            });
        }
    }

    pub(super) fn on_config_held(&mut self, h: CfgHeld, cx: &mut Context<Self>) {
        self.cfg.busy.remove(&h.local);
        let said = self.cfg.held.get(&h.local).cloned().unwrap_or_default();
        if !h.held.is_empty() && h.held != said {
            let mut who: Vec<&str> = h.held.iter().map(|(_, _, w)| w.as_str()).filter(|w| !w.is_empty()).collect();
            who.sort();
            who.dedup();
            let from = match who.as_slice() {
                [one] => format!("Settings from {one}"),
                _ => "Friends' settings".to_string(),
            };
            cx.emit(StoreEvent::Notice(format!("{from} wait until you close the game. Hopper puts them in then."), false));
        }
        if h.held.is_empty() {
            self.cfg.held.remove(&h.local);
        } else {
            self.cfg.held.insert(h.local.clone(), h.held);
        }
        let _ = h.root;
    }

    pub(super) fn on_config_ticked(&mut self, t: CfgTicked, cx: &mut Context<Self>) {
        let CfgTicked { local, root, sig, files, r } = t;
        self.cfg.busy.remove(&local);
        self.cfg.held.remove(&local);
        self.cfg.files.insert(local.clone(), (sig, files));
        let t = match r {
            Ok(t) => t,
            Err(e) => {
                log::warn!("shared settings in {}: {e:#}", root.display());
                return;
            }
        };
        let me = self.mailbox_id();
        if !t.own.is_empty() {
            self.update_settings_quietly(|st| {
                if let Some(c) = st.group_copies.iter_mut().find(|c| c.local == local) {
                    c.configs.own(&t.own, &me);
                }
            });
        }
        for f in &t.failed {
            log::warn!("writing shared settings: {f}");
        }
        let group = self.group_of(&root).unwrap_or_else(|| "The group".into());
        let new_wait = t.waiting.iter().any(|w| !self.cfg.waiting.get(&local).is_some_and(|v| v.contains(w)));
        if new_wait {
            let mut by_mod: BTreeMap<&str, (usize, &str)> = BTreeMap::new();
            for w in &t.waiting {
                let e = by_mod.entry(w.m.as_str()).or_insert((0, w.name.as_str()));
                e.0 += 1;
            }
            for (m, (n, who)) in by_mod {
                let name = self.mod_name_of(&root, m);
                let who = if who.is_empty() { "a friend".to_string() } else { who.to_string() };
                let what = if n == 1 { "1 setting".to_string() } else { format!("{n} settings") };
                cx.emit(StoreEvent::Notice(
                    format!("{what} from {who} don't exist in your {name}. You have a different version. They go in once you have the same one."),
                    false,
                ));
            }
        }
        self.cfg.waiting.insert(local.clone(), t.waiting);
        if !t.wrote.is_empty() {
            for w in t.wrote {
                cx.emit(StoreEvent::SettingsArrived { root: root.clone(), wrote: w, group: group.clone() });
            }
            self.handle(Msg::InstanceChanged(root), cx);
        }
        cx.notify();
    }

    /// A publish went out with these settings (`sent`, None when none were
    /// sent or they were too many): this PC's own changes in it take its
    /// number.
    pub(super) fn configs_sent(&mut self, local: &str, sent: Option<Configs>, too_big: bool, seq: u64) {
        if too_big {
            self.cfg.too_big.insert(local.to_string());
        } else {
            self.cfg.too_big.remove(local);
        }
        let Some(sent) = sent.filter(|_| self.cfg.relay == Some(true)) else { return };
        let me = self.mailbox_id();
        self.update_settings_quietly(|st| {
            if let Some(c) = st.group_copies.iter_mut().find(|c| c.local == local) {
                c.configs.stamp(&sent, &me, seq);
            }
        });
    }
}

/// The settings a copy sends with its recipe: (payload, what it holds), or
/// none when there are none, and whether they were too many.
pub(super) fn payload(c: &Configs) -> (String, Option<Configs>, bool) {
    if c.is_empty() {
        return (String::new(), None, false);
    }
    match configsync::encode(c) {
        Ok(s) => (s, Some(c.clone()), false),
        Err(e) => {
            log::warn!("shared settings: {e} ({} bytes)", e.0);
            (String::new(), None, true)
        }
    }
}
