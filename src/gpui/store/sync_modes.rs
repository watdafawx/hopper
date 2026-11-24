//! The two choices a player makes about friends sync on this PC
//! (`hopper::syncmode`): "Take changes from" per group, and "Share my
//! changes" for every pack this PC sends. Neither changes what goes over
//! the relay; they decide what this PC takes in by itself and when its own
//! changes go out.

use super::{Store, StoreEvent};
use gpui_kit::*;
use hopper::groupsync::PackCopy;
use hopper::mailbox::Event;
use hopper::settings::PublishTo;
use hopper::syncmode::{self, Pending};
use std::path::Path;
use std::sync::Arc;

/// What a pack held back for Push has to send, worked out for one look of
/// it (`Store::share_sig`).
#[derive(Clone, Copy, Debug)]
pub struct ToPush {
    pub sig: u64,
    pub pending: Pending,
}

impl Store {
    // ---- Take changes from -------------------------------------------------

    /// The member whose changes alone the group copy `c` takes in by
    /// itself (id and name), when its group is set so.
    pub(super) fn only_from(&self, c: &PackCopy) -> Option<(String, String)> {
        let me = self.mailbox_id();
        let g = self.settings.friend_groups.iter().find(|g| g.id == c.group)?;
        let id = g.only_from(&me)?.to_string();
        let name = self
            .mailbox_friends()
            .iter()
            .find(|f| f.id == id)
            .map(|f| f.name.clone())
            .filter(|n| !n.is_empty())
            .or_else(|| c.got.get(&id).or_else(|| c.waiting.get(&id)).map(|g| g.name.clone()).filter(|n| !n.is_empty()))
            .unwrap_or_else(|| "the group's maker".to_string());
        Some((id, name))
    }

    /// Whether the group copies of `inst` take `from`'s changes in now:
    /// they take from everyone, `from` made the group, or the player
    /// pressed "Pull from" them.
    pub(super) fn takes_from(&self, inst: &str, from: &str) -> bool {
        let Some(c) = self.settings.group_copies.iter().find(|c| c.inst == inst) else { return true };
        let only = self.only_from(c);
        syncmode::takes(only.as_ref().map(|(id, _)| id.as_str()), from)
            || self.group_pull_from.get(&c.local).is_some_and(|p| p.iter().any(|p| p == from))
    }

    /// The copy as a merge should see it: the codes of the members this PC
    /// takes in by itself, and of those the player is pulling from now.
    /// The others stay waiting in the stored copy.
    pub(super) fn merge_view(&self, c: &PackCopy) -> PackCopy {
        let only = self.only_from(c);
        let pull = self.group_pull_from.get(&c.local).cloned().unwrap_or_default();
        syncmode::for_merge(c, only.as_ref().map(|(id, _)| id.as_str()), &pull)
    }

    /// The group's "Take changes from" for this PC: whether it is set to
    /// the maker only (None: this PC made the group, so it doesn't apply).
    pub fn group_owner_only(&self, group: &str) -> Option<bool> {
        let me = self.mailbox_id();
        let g = self.settings.friend_groups.iter().find(|g| g.id == group)?;
        (!g.owner.is_empty() && g.owner != me).then_some(g.owner_only)
    }

    /// Sets "Take changes from" for a group. Back to everyone: what waited
    /// goes in on the next tick, as any change does.
    pub fn set_group_owner_only(&mut self, group: &str, on: bool, cx: &mut Context<Self>) {
        self.update_settings(
            |st| {
                if let Some(g) = st.friend_groups.iter_mut().find(|g| g.id == group) {
                    g.owner_only = on;
                }
            },
            cx,
        );
        // The last pull's note talks about the old choice ("You take
        // changes only from Mia"); it would read wrong now.
        let locals: Vec<String> = self.settings.group_copies.iter().filter(|c| c.group == group).map(|c| c.local.clone()).collect();
        for l in &locals {
            self.group_pulled.remove(l);
        }
        if !on {
            // The shared settings that waited with those codes come in too.
            let late: Vec<Event> = self
                .mailbox_recipes
                .iter()
                .filter(|e| !e.configs.is_empty() && self.settings.group_copies.iter().any(|c| c.inst == e.inst && c.group == group))
                .cloned()
                .collect();
            for e in &late {
                let taken = self.take_configs(e);
                self.say_taken(taken, cx);
            }
            self.follow_tick(cx);
        }
        cx.notify();
    }

    /// "Pull from Mia": takes in that member's waiting changes at `root`
    /// (and the maker's), asking the group's server for the latest first,
    /// as Pull now does. The others' stay waiting.
    pub fn pull_group_from(&mut self, root: &Path, who: &str, cx: &mut Context<Self>) {
        let Some(c) = self.group_copy(root).cloned() else { return };
        if self.group_preview.is_none() && self.is_sync_copy(root) {
            self.sync_take_from(root, who, cx);
            return;
        }
        if self.group_pulling.contains_key(&c.local) || self.group_merging.contains(&c.local) {
            return;
        }
        self.group_pull_from.insert(c.local.clone(), vec![who.to_string()]);
        // Their shared settings waited with their code.
        let theirs = self.mailbox_recipes.iter().filter(|e| e.from == who && e.inst == c.inst && !e.configs.is_empty()).max_by_key(|e| e.seq).cloned();
        if let Some(e) = theirs {
            let taken = self.take_configs(&e);
            self.say_taken(taken, cx);
        }
        self.pull_group_now(root, cx);
        // Pull now didn't start (no friends service): nothing to keep.
        if !self.group_pulling.contains_key(&c.local) && !self.group_merging.contains(&c.local) {
            self.group_pull_from.remove(&c.local);
        }
    }

    // ---- Share my changes --------------------------------------------------

    pub fn push_by_hand(&self) -> bool {
        self.settings.push_by_hand
    }

    /// Sets "Share my changes". Back to right away: everything held goes
    /// out on the next publish.
    pub fn set_push_by_hand(&mut self, on: bool, cx: &mut Context<Self>) {
        self.update_settings(|st| st.push_by_hand = on, cx);
        if !on {
            self.push_now.clear();
            self.to_push.clear();
        }
        self.mailbox_sig = 0;
        cx.notify();
    }

    /// Whether the pack `id`'s changes stay on this PC for now. `to`: where
    /// a share (not a member's group copy) goes. Not held: a pack never
    /// sent (sharing it is the player's say-so), one shared with someone
    /// new, one the player pressed Push for, and one with nothing of this
    /// PC's own to send (only what it took in from others: that echo lets
    /// the others see this copy in step).
    pub(super) fn held_back(&self, id: &str, to: Option<&PublishTo>) -> bool {
        // A group copy's changes travel through the sync core (it holds them
        // itself); only its share code goes by this path.
        if self.sync_has_copy(id) {
            return false;
        }
        if !self.settings.push_by_hand || self.push_now.contains(id) {
            return false;
        }
        if let Some(sig) = self.share_sig(id)
            && self.to_push.get(id).is_some_and(|p| p.sig == sig && !p.pending.any)
        {
            return false;
        }
        let sent = &self.settings.mailbox_sent;
        let copy = self.settings.group_copies.iter().find(|c| c.local == id);
        let before = copy.is_some_and(|c| !c.sent.is_empty()) || sent.iter().any(|s| s.inst == id);
        let new_target = to.is_some_and(|p| {
            p.to.iter().any(|f| !sent.iter().any(|s| s.inst == id && s.group.is_empty() && s.friend == *f))
                || p.groups.iter().any(|g| !sent.iter().any(|s| s.inst == id && s.group == *g))
        });
        before && !new_target
    }

    /// The last code that went out for the pack `id`.
    fn sent_code(&self, id: &str) -> Option<String> {
        let copy = self.settings.group_copies.iter().find(|c| c.local == id).map(|c| c.sent.clone()).filter(|s| !s.is_empty());
        copy.or_else(|| self.settings.pushed_codes.get(id).cloned())
    }

    /// Works out, off the UI thread, what each held-back pack has to send
    /// once its look changes. Runs on the publish tick.
    pub(super) fn refresh_to_push(&mut self, cx: &mut Context<Self>) {
        if !self.settings.push_by_hand || self.group_preview.is_some() {
            return;
        }
        let ids: Vec<String> = self.sent_ids().into_iter().filter(|id| !self.sync_has_copy(id)).collect();
        let before = self.to_push.len();
        self.to_push.retain(|id, _| ids.contains(id));
        let mut changed = before != self.to_push.len();
        for id in ids {
            if self.push_now.contains(&id) || self.to_push_busy.contains(&id) {
                continue;
            }
            let Some(sig) = self.share_sig(&id) else { continue };
            if self.to_push.get(&id).is_some_and(|p| p.sig == sig) {
                continue;
            }
            let Some(inst) = self.instances.iter().find(|i| i.id == id).cloned() else { continue };
            let sent = self.sent_code(&id);
            let copy = self.settings.group_copies.iter().find(|c| c.local == id);
            let marks = copy.map(|c| c.marks.clone()).unwrap_or_default();
            // A shared setting changed here rides along with the pack.
            let configs = copy.is_some_and(|c| c.configs.unsent(&self.mailbox_id()));
            self.to_push_busy.insert(id.clone());
            changed = true;
            let task = cx.background_spawn(async move {
                let jobs = hopper::jobs::Jobs::new(Arc::new(|| {}));
                let j = jobs.start_quiet(format!("Looking at {}", inst.name));
                let r = syncmode::pending(&inst, sent.as_deref(), &marks, &j);
                j.finish(&Ok::<(), anyhow::Error>(()));
                r
            });
            cx.spawn(async move |this, cx| {
                let r = task.await;
                this.update(cx, |s, cx| {
                    s.to_push_busy.remove(&id);
                    let pending = match r {
                        Ok(mut p) => {
                            p.any |= configs;
                            p
                        }
                        // Can't tell (a mod site didn't answer): there may
                        // be something, so the Push button stays.
                        Err(e) => {
                            log::warn!("working out what {id} has to send: {e:#}");
                            Pending { changes: 0, any: true }
                        }
                    };
                    s.to_push.insert(id, ToPush { sig, pending });
                    // Nothing of its own to send: the echo may go now.
                    if !pending.any {
                        s.mailbox_sig = 0;
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        if changed {
            cx.notify();
        }
    }

    /// How many changes the pack at `root` holds back for Push (Some(0):
    /// some, but can't tell how many), or None when nothing waits.
    pub fn to_push_at(&self, root: &Path) -> Option<usize> {
        let id = &self.instance(root)?.id;
        self.to_push_of(id)
    }

    fn to_push_of(&self, id: &str) -> Option<usize> {
        if !self.settings.push_by_hand || self.push_now.contains(id) {
            return None;
        }
        if self.sync_has_copy(id) {
            return self.sync_held(id);
        }
        self.to_push.get(id).filter(|p| p.pending.any).map(|p| p.pending.changes)
    }

    /// Every pack holding changes back for Push: (instance root, name, how
    /// many changes; 0: can't tell).
    pub fn packs_to_push(&self) -> Vec<(std::path::PathBuf, String, usize)> {
        let held: Vec<&String> = self.to_push.keys().chain(self.sync_copy_ids()).collect();
        let mut out: Vec<_> = held
            .into_iter()
            .filter_map(|id| {
                let n = self.to_push_of(id)?;
                let i = self.instances.iter().find(|i| &i.id == id)?;
                Some((i.root.clone(), i.name.clone(), n))
            })
            .collect();
        out.sort_by(|a, b| a.1.cmp(&b.1));
        out
    }

    /// Whether a Push is on its way for the pack at `root`.
    pub fn pushing_at(&self, root: &Path) -> bool {
        self.instance(root).is_some_and(|i| self.push_now.contains(&i.id))
    }

    /// "Push": the held-back changes of these packs (instance roots) go
    /// out on the next publish, a second or less away.
    pub fn push_changes(&mut self, roots: &[std::path::PathBuf], cx: &mut Context<Self>) {
        let ids: Vec<String> = roots.iter().filter_map(|r| self.instance(r)).map(|i| i.id.clone()).filter(|id| !id.is_empty()).collect();
        if self.group_preview.is_some() {
            for id in &ids {
                self.to_push.remove(id);
            }
            cx.emit(StoreEvent::Notice("Your changes went to the group.".to_string(), false));
            cx.notify();
            return;
        }
        self.push_now.extend(ids);
        self.mailbox_sig = 0;
        cx.notify();
    }

    /// A publish ended: the packs pushed by hand that went out are done
    /// (their new look counts as sent), and a Push asked while it ran
    /// makes the next tick publish again.
    pub(super) fn pushed(&mut self, sent_ids: &[String], failed: bool) {
        for id in sent_ids {
            self.push_now.remove(id);
            self.to_push.remove(id);
        }
        let live = self.sent_ids();
        self.push_now.retain(|id| live.contains(id));
        if !self.push_now.is_empty() && !failed {
            self.mailbox_sig = 0;
        }
    }

    /// `--dialog group-sync --state to-push`: the made-up packs hold
    /// changes back (`changes` for each, by instance id).
    pub(super) fn preview_to_push(&mut self, held: &[(String, usize)]) {
        self.to_push.clear();
        self.push_now.clear();
        for (id, n) in held {
            let sig = self.share_sig(id).unwrap_or_default();
            self.to_push.insert(id.clone(), ToPush { sig, pending: Pending { changes: *n, any: true } });
        }
    }
}
