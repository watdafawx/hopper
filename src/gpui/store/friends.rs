//! Friends & sync on the store: the mailbox account and its feed, the packs
//! friends share with this PC, what this PC shares (and takes back), friend
//! groups, and following packs. The protocol is in `design/friends-sync.md`.
//!
//! A follow the player starts always goes through [`follow_pack`] or
//! [`redeem`], which ask about mods from GitHub and other links first. A
//! follow nobody can be asked about (a followed pack changing, keeping a
//! group in step) leaves out what that question would need
//! (`sources::carry::auto_skip`). The store's own follow functions are
//! private to this file, so a view can't skip the question.

use super::{Msg, Store, StoreEvent};
use gpui_kit::*;
use hopper::mailbox::{self, Event, Group, Kind, Mailbox};
use hopper::settings::{FollowedRecipe, FriendGroup, PublishTo, SharedPack};
use hopper::sources::carry::Pin;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often the groups are read again when nothing says they changed.
const GROUPS_EVERY: Duration = Duration::from_secs(60);
/// How long a failed publish waits before it's tried again.
const RETRY_AFTER: Duration = Duration::from_secs(60);
/// Packs from friends kept for the Friends screen.
const RECIPES_KEPT: usize = 200;

/// How the sync service is doing, for the Friends screen's status line.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncStatus {
    /// Friends & sync is off (or has no mailbox address).
    Off,
    /// Starting: nothing heard from the mailbox yet.
    Connecting,
    /// The mailbox can't be reached or refused; Hopper keeps trying. The
    /// text says what's wrong, for players.
    Down(String),
    Ok,
}

/// A live invite code, for its countdown.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct Invite {
    pub code: String,
    pub until: Instant,
    /// The pack (instance id) it follows; empty for a plain invite.
    pub inst: String,
}

/// A friend's pack to follow: from the mailbox feed ([`FollowReq::of`]) or
/// from a PC on the network.
#[derive(Clone, Debug, Default)]
pub struct FollowReq {
    pub peer_id: String,
    pub peer_name: String,
    /// Their instance id (a folder name for a PC on the network).
    pub inst: String,
    pub name: String,
    pub code: String,
    pub left_out: Vec<String>,
    pub at: u64,
    /// The groups it reached this PC through. Following it tells them, so
    /// members who keep the group in step follow it too.
    pub groups: Vec<String>,
}

impl FollowReq {
    pub fn of(e: &Event) -> Self {
        Self {
            peer_id: e.from.clone(),
            peer_name: e.from_name.clone(),
            inst: e.inst.clone(),
            name: e.name.clone(),
            code: e.code.clone(),
            left_out: e.left_out.clone(),
            at: e.at,
            groups: e.groups.clone(),
        }
    }

    fn who(&self) -> &str {
        if self.peer_name.is_empty() { "a friend" } else { &self.peer_name }
    }

    fn pack(&self) -> &str {
        if self.name.is_empty() { &self.inst } else { &self.name }
    }

    /// The settings record for following it at `root` (`code`: what's
    /// already applied there, empty until it is).
    fn recipe(&self, root: &Path, local: &str, code: &str, group: &str) -> FollowedRecipe {
        FollowedRecipe {
            peer_id: self.peer_id.clone(),
            peer_name: self.peer_name.clone(),
            inst: self.inst.clone(),
            name: self.name.clone(),
            root: root.to_string_lossy().into_owned(),
            local: local.to_string(),
            code: code.to_string(),
            seen: self.code.clone(),
            left_out: self.left_out.clone(),
            at: self.at,
            ended: String::new(),
            group: group.to_string(),
        }
    }
}

/// Where a followed pack goes.
#[derive(Clone, Debug, PartialEq)]
pub enum FollowTarget {
    /// Over an instance the code fits (`recipe::fit`); its mods are
    /// replaced with theirs.
    Existing(PathBuf),
    /// Installed as a new instance.
    New,
}

/// Follows a friend's pack: asks about mods from GitHub and other links
/// first, then installs or applies it. The one way a view starts a follow.
pub fn follow_pack(store: &Entity<Store>, req: FollowReq, target: FollowTarget, window: &mut Window, cx: &mut App) {
    let share = match hopper::sharecode::decode(&req.code) {
        Ok(s) => s,
        Err(e) => {
            store.update(cx, |_, cx| cx.emit(StoreEvent::Notice(format!("{e:#}"), true)));
            return;
        }
    };
    let pins = hopper::sharecode::pins(&share);
    let store = store.clone();
    // Called from a dialog's OK or a click: the question opens once that
    // has finished.
    window.defer(cx, move |window, cx| {
        let s2 = store.clone();
        crate::incoming_sources::gate(store.clone(), pins, window, cx, move |skip, _, cx| {
            let (req, target) = (req.clone(), target.clone());
            s2.update(cx, |s, cx| s.follow(req, target, Some(skip), "", cx));
        });
    });
}

/// Installs a share code as a new instance. A code with a follow part (see
/// `sharecode::with_follow`) also makes its sender a friend and follows
/// the pack. Asks about mods from other sources first.
pub fn redeem(store: &Entity<Store>, text: &str, window: &mut Window, cx: &mut App) -> bool {
    let Ok(share) = hopper::sharecode::decode(text) else {
        return false;
    };
    let (code, invite) = hopper::sharecode::follow_part(text);
    let pins = hopper::sharecode::pins(&share);
    let store = store.clone();
    window.defer(cx, move |window, cx| {
        let s2 = store.clone();
        let (share, code, invite) = (share.clone(), code.clone(), invite.clone());
        crate::incoming_sources::gate(store.clone(), pins, window, cx, move |skip, _, cx| {
            let (share, note) = hopper::sharecode::without(&share, &skip);
            let s = s2.read(cx);
            let root = PathBuf::from(&s.settings().instances_root);
            let mb = s.mailbox.clone();
            let (code, invite) = (code.clone(), invite.clone());
            s.spawn_job("Setting up a shared pack", move |job| {
                job.announce();
                let inst = hopper::sharecode::install_share(&root, &share, job)?;
                job.detail(format!("Installed {}", inst.name));
                if let Some(n) = note {
                    job.note(n);
                }
                let Some(invite) = invite else {
                    return Ok(Msg::installed(inst.root, &[], None));
                };
                // The pack is in either way; following needs the friend.
                let Some(mb) = mb else {
                    job.note("Turn on Friends & sync to get their changes to this pack.");
                    return Ok(Msg::installed(inst.root, &[], None));
                };
                match mb.join(&invite) {
                    Ok((friend, their)) if !their.is_empty() => {
                        let req = FollowReq {
                            peer_id: friend.id.clone(),
                            peer_name: friend.name.clone(),
                            inst: their,
                            name: inst.name.clone(),
                            code: code.clone(),
                            ..Default::default()
                        };
                        Ok(Msg::Followed(Box::new(req.recipe(&inst.root, &inst.id, &code, ""))))
                    }
                    Ok(_) => Ok(Msg::installed(inst.root, &[], None)),
                    Err(e) => {
                        job.note(format!("Not following their changes: {}", mailbox::says(&e)));
                        Ok(Msg::installed(inst.root, &[], None))
                    }
                }
            });
        });
    });
    true
}

// The Friends screen (being rebuilt on top of this) calls most of these.
#[allow(dead_code)]
impl Store {
    // ---- What the Friends screen reads -----------------------------------

    pub fn sync_status(&self) -> SyncStatus {
        if !self.settings.peer_sync || self.settings.mailbox_url.trim().is_empty() {
            SyncStatus::Off
        } else if let Some(e) = &self.mailbox_error {
            SyncStatus::Down(e.clone())
        } else if self.mailbox.is_none() || !self.mailbox_heard {
            SyncStatus::Connecting
        } else {
            SyncStatus::Ok
        }
    }

    pub fn mailbox(&self) -> Option<Arc<Mailbox>> {
        self.mailbox.clone()
    }

    /// What's wrong with the mailbox, in words for players.
    pub fn mailbox_error(&self) -> Option<String> {
        self.mailbox_error.clone()
    }

    /// This PC's mailbox id (empty until registered).
    pub fn mailbox_id(&self) -> String {
        self.mailbox.as_ref().map(|m| m.account().id.clone()).unwrap_or_default()
    }

    /// Friends, with online and last seen.
    pub fn mailbox_friends(&self) -> &[mailbox::Friend] {
        &self.mailbox_friends
    }

    /// The friend list is full: "Friend list full" instead of inviting.
    pub fn friends_full(&self) -> bool {
        self.mailbox_max > 0 && self.mailbox_friends.len() >= self.mailbox_max
    }

    /// Packs friends share with this PC: the latest code of each, with how
    /// it got here (`direct`, `groups`) and when it last changed (`at`).
    pub fn mailbox_recipes(&self) -> &[Event] {
        &self.mailbox_recipes
    }

    /// Groups this PC owns, belongs to or is invited to (`me.joined` false:
    /// an invitation to accept or turn down).
    pub fn mailbox_groups(&self) -> &[Group] {
        &self.mailbox_groups
    }

    /// The invite code made last, while it lasts.
    pub fn mailbox_invite(&self) -> Option<Invite> {
        self.mailbox_invite.clone().filter(|i| Instant::now() < i.until)
    }

    /// The local instances following a friend's pack. A follow whose
    /// `ended` is set gets no more changes (the pack is gone); its instance
    /// stays as it was.
    pub fn followers_of(&self, from: &str, inst: &str) -> Vec<&FollowedRecipe> {
        self.settings
            .followed_recipes
            .iter()
            .filter(|f| f.matches(from, inst, ""))
            .collect()
    }

    /// What the instance at `root` follows, if anything.
    pub fn follow_at(&self, root: &Path) -> Option<&FollowedRecipe> {
        self.settings
            .followed_recipes
            .iter()
            .find(|f| Path::new(&f.root) == root)
    }

    // ---- What the Friends screen does ------------------------------------

    /// Makes an invite code and copies it. `inst` (an instance id, or
    /// empty) makes whoever joins follow that pack.
    pub fn make_invite(&mut self, inst: &str, cx: &mut Context<Self>) {
        let Some(mb) = self.mailbox_or_say(cx) else { return };
        if self.friends_full() {
            cx.emit(StoreEvent::Notice("Friend list full. Remove a friend to add another.".into(), true));
            return;
        }
        let inst = inst.to_string();
        cx.spawn(async move |this, cx| {
            let i2 = inst.clone();
            let r = cx.background_spawn(async move { mb.invite_for(&i2) }).await;
            this.update(cx, |s, cx| match r {
                Ok((code, secs)) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                    s.mailbox_invite = Some(Invite {
                        code,
                        until: Instant::now() + Duration::from_secs(secs),
                        inst,
                    });
                    cx.emit(StoreEvent::Notice("Code copied. Send it to your friend; it works once.".into(), false));
                    cx.notify();
                }
                Err(e) => cx.emit(StoreEvent::Notice(mailbox::says(&e), true)),
            })
            .ok();
        })
        .detach();
    }

    /// Copies the instance's share code with a follow part: whoever pastes
    /// it installs the pack, becomes a friend and follows it. Works once,
    /// for as long as an invite.
    pub fn copy_follow_code(&mut self, root: &Path, cx: &mut Context<Self>) {
        let Some(inst) = self.instance(root).cloned() else { return };
        let Some(mb) = self.mailbox_or_say(cx) else { return };
        if self.friends_full() {
            cx.emit(StoreEvent::Notice("Friend list full. Remove a friend to add another.".into(), true));
            return;
        }
        cx.spawn(async move |this, cx| {
            let id = inst.id.clone();
            let r = cx
                .background_spawn(async move {
                    let jobs = hopper::jobs::Jobs::new(Arc::new(|| {}));
                    let j = jobs.start_quiet(format!("Sharing {}", inst.name));
                    let made = hopper::sharecode::make(&inst, &j);
                    j.finish(&Ok::<(), anyhow::Error>(()));
                    let made = made?;
                    let (invite, secs) = mb.invite_for(&inst.id)?;
                    anyhow::Ok((hopper::sharecode::with_follow(&made.code, &invite), invite, secs))
                })
                .await;
            this.update(cx, |s, cx| match r {
                Ok((text, invite, secs)) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    s.mailbox_invite = Some(Invite {
                        code: invite,
                        until: Instant::now() + Duration::from_secs(secs),
                        inst: id,
                    });
                    cx.emit(StoreEvent::Notice(
                        format!(
                            "Share code copied. It works once, for {} minutes, and keeps them following this pack.",
                            secs.div_ceil(60)
                        ),
                        false,
                    ));
                    cx.notify();
                }
                Err(e) => cx.emit(StoreEvent::Notice(mailbox::says(&e), true)),
            })
            .ok();
        })
        .detach();
    }

    /// Joins with a friend's invite code.
    pub fn join_friend(&mut self, code: &str, cx: &mut Context<Self>) {
        let code = code.trim().to_string();
        if code.is_empty() {
            return;
        }
        self.mailbox_do(
            move |mb| {
                let (f, _) = mb.join(&code)?;
                Ok(format!(
                    "Paired with {}. Their packs appear here when they share one.",
                    if f.name.is_empty() { "them" } else { &f.name }
                ))
            },
            |_, _| {},
            cx,
        );
    }

    /// Removes a friend. Packs followed from them keep their last version;
    /// nothing more arrives, and they stop getting this PC's packs.
    pub fn remove_friend(&mut self, id: &str, cx: &mut Context<Self>) {
        let name = self.friend_name(id);
        let (id, id2) = (id.to_string(), id.to_string());
        self.mailbox_do(
            move |mb| {
                mb.remove(&id)?;
                Ok(format!("Removed {name}. Packs you follow from them stay as they are."))
            },
            move |s, cx| s.lost_friend(&id2, cx),
            cx,
        );
    }

    /// Stops following at `root`. The instance stays as it is.
    pub fn stop_following(&mut self, root: &Path, cx: &mut Context<Self>) {
        let r = root.to_string_lossy().into_owned();
        let mut did = false;
        self.update_settings(|st| did = st.stop_following(&r), cx);
        self.pending_follow.remove(root);
        if did {
            cx.emit(StoreEvent::Notice("Stopped following. The instance stays as it is.".into(), false));
        }
    }

    /// Shares a pack (instance id) with one friend, or stops. A pack shared
    /// before is taken back from them on the next publish.
    pub fn set_shared(&mut self, inst: &str, friend: &str, on: bool, cx: &mut Context<Self>) {
        self.update_settings(
            |st| {
                st.set_shared(inst, friend, on);
                st.share_prompt_done = true;
            },
            cx,
        );
    }

    /// Shares a pack with a friend group (everyone in it, once each), or
    /// stops.
    pub fn set_shared_group(&mut self, inst: &str, group: &str, on: bool, cx: &mut Context<Self>) {
        self.update_settings(
            |st| {
                st.set_shared_group(inst, group, on);
                st.share_prompt_done = true;
            },
            cx,
        );
    }

    /// "Share all": every pack with every friend.
    pub fn share_all(&mut self, cx: &mut Context<Self>) {
        let insts: Vec<String> = self.instances.iter().filter(|i| !i.id.is_empty()).map(|i| i.id.clone()).collect();
        let friends: Vec<String> = self.mailbox_friends.iter().map(|f| f.id.clone()).collect();
        self.update_settings(
            |st| {
                st.share_with_all(&insts, &friends);
                st.share_prompt_done = true;
            },
            cx,
        );
    }

    pub fn dismiss_share_prompt(&mut self, cx: &mut Context<Self>) {
        self.update_settings(|st| st.share_prompt_done = true, cx);
    }

    /// Makes a group of some friends; each gets an invitation.
    pub fn create_group(&mut self, name: &str, members: Vec<String>, cx: &mut Context<Self>) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        self.mailbox_do(
            move |mb| {
                mb.create_group(&name, &members)?;
                Ok(format!("Made {name}. Your friends see an invitation to join."))
            },
            |_, _| {},
            cx,
        );
    }

    pub fn rename_group(&mut self, id: &str, name: &str, cx: &mut Context<Self>) {
        let (id, name) = (id.to_string(), name.trim().to_string());
        if name.is_empty() {
            return;
        }
        self.mailbox_do(
            move |mb| {
                mb.edit_group(&id, Some(&name), &[], &[])?;
                Ok(String::new())
            },
            |_, _| {},
            cx,
        );
    }

    /// Invites friends into a group (owner only).
    pub fn add_to_group(&mut self, id: &str, members: Vec<String>, cx: &mut Context<Self>) {
        let (id, group) = (id.to_string(), self.group_name(id));
        let n = members.len();
        self.mailbox_do(
            move |mb| {
                mb.edit_group(&id, None, &members, &[])?;
                Ok(format!("Invited {n} friend{} to {group}.", if n == 1 { "" } else { "s" }))
            },
            |_, _| {},
            cx,
        );
    }

    /// Takes members out of a group (owner only); what came through it
    /// stops for them.
    pub fn remove_from_group(&mut self, id: &str, members: Vec<String>, cx: &mut Context<Self>) {
        let (id, group) = (id.to_string(), self.group_name(id));
        self.mailbox_do(
            move |mb| {
                mb.edit_group(&id, None, &[], &members)?;
                Ok(format!("Removed from {group}."))
            },
            |_, _| {},
            cx,
        );
    }

    pub fn delete_group(&mut self, id: &str, cx: &mut Context<Self>) {
        let (id, group) = (id.to_string(), self.group_name(id));
        self.mailbox_do(
            move |mb| {
                mb.delete_group(&id)?;
                Ok(format!("Deleted {group}. Packs shared through it stop updating for its members."))
            },
            |_, _| {},
            cx,
        );
    }

    /// Says yes to a group invitation.
    pub fn accept_group(&mut self, id: &str, cx: &mut Context<Self>) {
        let (id, group) = (id.to_string(), self.group_name(id));
        self.mailbox_do(
            move |mb| {
                mb.accept_group(&id)?;
                Ok(format!("Joined {group}."))
            },
            |_, _| {},
            cx,
        );
    }

    /// Leaves a group or turns its invitation down. Packs that came only
    /// through it stop updating; their instances stay.
    pub fn leave_group(&mut self, id: &str, cx: &mut Context<Self>) {
        let (id, group) = (id.to_string(), self.group_name(id));
        let joined = self.mailbox_groups.iter().any(|g| g.id == id && g.me.joined);
        self.mailbox_do(
            move |mb| {
                mb.leave_group(&id)?;
                Ok(if joined {
                    format!("Left {group}. Packs that came only through it stop updating.")
                } else {
                    String::new()
                })
            },
            |_, _| {},
            cx,
        );
    }

    /// "Keep everyone in step": a group pack another member follows is
    /// followed on this PC too. Turning it on follows what they follow now.
    pub fn set_in_step(&mut self, id: &str, on: bool, cx: &mut Context<Self>) {
        let (id, group) = (id.to_string(), self.group_name(id));
        self.mailbox_do(
            move |mb| {
                mb.set_in_step(&id, on)?;
                Ok(if on {
                    format!("{group}: packs the others follow are followed here too.")
                } else {
                    format!("{group}: no longer kept in step. Packs already followed stay.")
                })
            },
            |_, _| {},
            cx,
        );
    }

    // ---- The rest of the store calls these -------------------------------

    /// Starts the mailbox account and its feed. Safe to call often.
    pub(super) fn start_mailbox(&mut self, cx: &mut Context<Self>) {
        if self._mailbox_watch.is_some() {
            return;
        }
        let url = self.settings.mailbox_url.trim().to_string();
        if url.is_empty() {
            return;
        }
        let (name, mc_id) = self.mailbox_profile();
        self._mailbox_watch = Some(cx.spawn(async move |this, cx| loop {
            let (u, n, m) = (url.clone(), name.clone(), mc_id.clone());
            let mb = cx
                .background_spawn(async move { Mailbox::load(&u, &n, &m) })
                .await;
            let mb = match mb {
                Ok(mb) => Arc::new(mb),
                Err(e) => {
                    this.update(cx, |s, cx| {
                        s.mailbox_error = Some(mailbox::says(&e));
                        cx.notify();
                    })
                    .ok();
                    cx.background_executor().timer(Duration::from_secs(30)).await;
                    continue;
                }
            };
            if this
                .update(cx, |s, cx| {
                    s.mailbox = Some(mb.clone());
                    s.mailbox_error = None;
                    if mb.was_reset() {
                        // The service lost its data: friends are gone there,
                        // and its event numbers start over.
                        s.mailbox_seq = 0;
                        s.update_settings(|st| st.mailbox_handled = 0, cx);
                        cx.emit(StoreEvent::Notice(
                            "Your friends list was reset on the sync server. Invite your friends again.".into(),
                            true,
                        ));
                    }
                    cx.notify();
                })
                .is_err()
            {
                return;
            }
            loop {
                let got = this.update(cx, |s, _| (s.mailbox_seq, s.mailbox.clone()));
                let Ok((after, Some(mb2))) = got else { return };
                let r = cx.background_spawn(async move { mb2.events(after, 60) }).await;
                match r {
                    Ok((seq, events)) => {
                        if this
                            .update(cx, |s, cx| {
                                s.mailbox_seq = seq.max(after);
                                s.mailbox_error = None;
                                s.mailbox_heard = true;
                                let handled = s.settings.mailbox_handled;
                                let newest = events.iter().map(|e| e.seq).max().unwrap_or(0);
                                for e in newest_codes_only(events) {
                                    // Each start reads the whole feed again
                                    // to list friends' packs; only newer
                                    // events show notices or act once.
                                    let fresh = e.seq > handled;
                                    s.mailbox_event(e, fresh, cx);
                                }
                                if newest > handled {
                                    s.update_settings(|st| st.mailbox_handled = newest, cx);
                                }
                                cx.notify();
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(e) => {
                        this.update(cx, |s, cx| {
                            s.mailbox_error = Some(mailbox::says(&e));
                            cx.notify();
                        })
                        .ok();
                        // The service forgot this PC: load the account again,
                        // which signs up once more.
                        if mailbox::unauthorized(&e) {
                            break;
                        }
                        cx.background_executor().timer(Duration::from_secs(20)).await;
                    }
                }
            }
        }));
        self.start_mailbox_publish(cx);
    }

    /// Friends & sync turned off: drops the account and feed (settings stay).
    pub(super) fn stop_mailbox(&mut self) {
        self.mailbox = None;
        self.mailbox_error = None;
        self.mailbox_heard = false;
        self.mailbox_invite = None;
        self._mailbox_watch = None;
        self._mailbox_publish = None;
    }

    /// A new instance following a friend's pack was made.
    pub(super) fn on_followed(&mut self, f: FollowedRecipe, cx: &mut Context<Self>) {
        let root = PathBuf::from(&f.root);
        self.auto_following.remove(&format!("{}/{}", f.peer_id, f.inst));
        self.update_settings(
            |st| {
                st.followed_recipes.retain(|x| Path::new(&x.root) != root);
                st.followed_recipes.push(f);
            },
            cx,
        );
        self.handle(Msg::installed(root, &[], None), cx);
    }

    /// A followed pack's code was applied at `root`.
    pub(super) fn on_recipe_applied(&mut self, root: PathBuf, code: String, cx: &mut Context<Self>) {
        self.pending_follow.remove(&root);
        self.update_settings(
            |st| {
                for f in st.followed_recipes.iter_mut().filter(|f| Path::new(&f.root) == root) {
                    f.code = code.clone();
                }
            },
            cx,
        );
        self.handle(Msg::InstanceChanged(root), cx);
    }

    /// An instance was deleted in Hopper: what it followed stops (and a
    /// group pack isn't followed again by keeping the group in step).
    pub(super) fn forget_follows(&mut self, root: &Path) {
        let r = root.to_string_lossy().into_owned();
        self.pending_follow.remove(root);
        self.update_settings_quietly(|st| {
            st.stop_following(&r);
        });
    }

    /// Applies follows that waited for their game to close.
    pub(super) fn follow_tick(&mut self, cx: &mut Context<Self>) {
        if self.pending_follow.is_empty() {
            return;
        }
        let ready: Vec<PathBuf> = self.pending_follow.keys().filter(|r| !self.is_busy(r)).cloned().collect();
        for root in ready {
            let skip = self.pending_follow.remove(&root).flatten();
            let Some(f) = self.follow_at(&root).cloned() else { continue };
            self.apply_follow(root, f.seen.clone(), format!("Updating from {}'s {}", f.peer_name, f.name), skip, cx);
        }
    }

    // ---- Inside ----------------------------------------------------------

    fn mailbox_or_say(&self, cx: &mut Context<Self>) -> Option<Arc<Mailbox>> {
        if self.mailbox.is_none() {
            let text = match self.sync_status() {
                SyncStatus::Off => "Turn on Friends & sync first.".to_string(),
                SyncStatus::Down(e) => e,
                _ => "Still connecting to the sync service. Try again in a moment.".to_string(),
            };
            cx.emit(StoreEvent::Notice(text, true));
        }
        self.mailbox.clone()
    }

    /// Runs a mailbox call off the UI thread: its text (if any) becomes a
    /// notice, `after` runs on success, a failure is said plainly. Groups
    /// are read again afterwards, as most calls change them.
    fn mailbox_do(
        &mut self,
        work: impl FnOnce(&Mailbox) -> anyhow::Result<String> + Send + 'static,
        after: impl FnOnce(&mut Store, &mut Context<Store>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(mb) = self.mailbox_or_say(cx) else { return };
        cx.spawn(async move |this, cx| {
            let r = cx.background_spawn(async move { work(&mb) }).await;
            this.update(cx, |s, cx| {
                match r {
                    Ok(text) => {
                        after(s, cx);
                        if !text.is_empty() {
                            cx.emit(StoreEvent::Notice(text, false));
                        }
                    }
                    Err(e) => cx.emit(StoreEvent::Notice(mailbox::says(&e), true)),
                }
                s.groups_due = true;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn friend_name(&self, id: &str) -> String {
        self.mailbox_friends
            .iter()
            .find(|f| f.id == id)
            .map(|f| f.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "them".into())
    }

    fn group_name(&self, id: &str) -> String {
        self.mailbox_groups
            .iter()
            .find(|g| g.id == id)
            .map(|g| g.name.clone())
            .unwrap_or_else(|| "the group".into())
    }

    /// One event from the feed. `fresh`: not acted on before (see
    /// `Settings::mailbox_handled`).
    fn mailbox_event(&mut self, e: Event, fresh: bool, cx: &mut Context<Self>) {
        match e.kind() {
            Kind::Recipe => self.got_recipe(e, cx),
            Kind::Revoke => self.got_revoke(e, fresh, cx),
            // Read the groups again; keeping in step happens there.
            Kind::Group | Kind::GroupFollow => self.groups_due = true,
            Kind::Joined if fresh => self.got_joined(e, cx),
            Kind::Unfriended => {
                let who = if e.from_name.is_empty() { "A friend".to_string() } else { e.from_name.clone() };
                self.lost_friend(&e.from, cx);
                if fresh {
                    cx.emit(StoreEvent::Notice(
                        format!("{who} removed you as a friend. Packs you follow from them stay as they are."),
                        false,
                    ));
                }
            }
            Kind::Joined | Kind::Other => {}
        }
    }

    /// A friend's pack, new or changed: listed, and applied on every
    /// instance here that follows it.
    fn got_recipe(&mut self, e: Event, cx: &mut Context<Self>) {
        // One entry per pack; one from before ids (its folder name) is the
        // same pack.
        self.mailbox_recipes.retain(|x| {
            !(x.from == e.from && (x.inst == e.inst || (!e.key.is_empty() && x.inst.eq_ignore_ascii_case(&e.key))))
        });
        self.mailbox_recipes.push(e.clone());
        if self.mailbox_recipes.len() > RECIPES_KEPT {
            self.mailbox_recipes.remove(0);
        }
        let mut apply: Vec<(PathBuf, String)> = Vec::new();
        self.update_settings_quietly(|st| {
            for ix in st.followers(&e.from, &e.inst, &e.key) {
                let f = &mut st.followed_recipes[ix];
                // A follow from before ids learns the id here.
                f.inst = e.inst.clone();
                f.name = e.name.clone();
                if !e.from_name.is_empty() {
                    f.peer_name = e.from_name.clone();
                }
                f.left_out = e.left_out.clone();
                f.at = e.at;
                // Shared again after it stopped.
                f.ended.clear();
                if f.seen != e.code {
                    f.seen = e.code.clone();
                    // An instance deleted outside Hopper has nothing to update.
                    if Path::new(&f.root).is_dir() {
                        apply.push((PathBuf::from(&f.root), format!("Updating from {}'s {}", f.peer_name, f.name)));
                    }
                }
            }
        });
        for (root, title) in apply {
            self.apply_follow(root, e.code.clone(), title, None, cx);
        }
    }

    /// A friend stopped sharing a pack with this PC (or one of the ways it
    /// came here went). Once no way is left it leaves the list, and what
    /// follows it here stays at its last version.
    fn got_revoke(&mut self, e: Event, fresh: bool, cx: &mut Context<Self>) {
        let mut gone = true;
        if let Some(ix) = self.mailbox_recipes.iter().position(|x| x.from == e.from && x.inst == e.inst) {
            gone = drop_ways(&mut self.mailbox_recipes[ix], &e);
            if gone {
                self.mailbox_recipes.remove(ix);
            }
        }
        if gone {
            self.end_follows(&e.from, &e.inst, &e.reason, fresh, cx);
        }
    }

    /// Marks the follows of a pack that won't change any more, and says so.
    fn end_follows(&mut self, from: &str, inst: &str, reason: &str, fresh: bool, cx: &mut Context<Self>) {
        let mut ended: Vec<(String, String)> = Vec::new();
        self.update_settings_quietly(|st| {
            for ix in st.followers(from, inst, "") {
                let f = &mut st.followed_recipes[ix];
                if f.ended.is_empty() {
                    f.ended = reason.to_string();
                    ended.push((f.peer_name.clone(), f.name.clone()));
                }
            }
        });
        if !fresh || reason == "unfriended" {
            return;
        }
        for (who, pack) in ended {
            let who = if who.is_empty() { "Your friend".to_string() } else { who };
            let what = match reason {
                "deleted" => format!("{who} deleted {pack}."),
                "left_group" => format!("{who}'s {pack} isn't shared with you any more."),
                _ => format!("{who} stopped sharing {pack}."),
            };
            cx.emit(StoreEvent::Notice(format!("{what} Your copy stays as it is."), false));
        }
    }

    /// Someone joined with this PC's invite. One that named a pack shares
    /// it with them, so they follow it.
    fn got_joined(&mut self, e: Event, cx: &mut Context<Self>) {
        let who = if e.from_name.is_empty() { "A friend".to_string() } else { e.from_name.clone() };
        let pack = (!e.inst.is_empty())
            .then(|| self.instances.iter().find(|i| i.id == e.inst))
            .flatten()
            .map(|i| i.name.clone());
        match pack {
            Some(name) => {
                let (inst, from) = (e.inst.clone(), e.from.clone());
                self.update_settings(|st| st.set_shared(&inst, &from, true), cx);
                cx.emit(StoreEvent::Notice(format!("{who} joined with your code and follows {name}."), false));
            }
            None => cx.emit(StoreEvent::Notice(format!("{who} is now your friend."), false)),
        }
        self.mailbox_invite = None;
    }

    /// A friend is gone (removed here or there): their direct shares stop,
    /// and this PC's shares with them are dropped.
    fn lost_friend(&mut self, id: &str, cx: &mut Context<Self>) {
        self.mailbox_friends.retain(|f| f.id != id);
        self.update_settings_quietly(|st| {
            st.shared_with.retain(|s| s.friend != id);
            st.mailbox_sent.retain(|s| s.friend != id);
        });
        let mut gone = Vec::new();
        self.mailbox_recipes.retain_mut(|r| {
            if r.from != id {
                return true;
            }
            r.direct = false;
            if r.groups.is_empty() {
                gone.push(r.inst.clone());
                return false;
            }
            true
        });
        for inst in gone {
            self.end_follows(id, &inst, "unfriended", false, cx);
        }
    }

    /// The relay's groups: kept for the screen, and this PC's copy of the
    /// ones it's in updated (shares with groups it left go). Then keeps the
    /// in-step groups in step.
    fn got_groups(&mut self, groups: Vec<Group>, cx: &mut Context<Self>) {
        let mine: Vec<FriendGroup> = groups
            .iter()
            .filter(|g| g.me.joined)
            .map(|g| FriendGroup {
                id: g.id.clone(),
                name: g.name.clone(),
                members: g.members.iter().filter(|m| m.joined).map(|m| m.id.clone()).collect(),
                owner: g.owner.clone(),
                in_step: g.me.in_step,
                stopped: Vec::new(),
            })
            .collect();
        let left: Vec<String> = self
            .settings
            .friend_groups
            .iter()
            .filter(|g| !mine.iter().any(|m| m.id == g.id))
            .map(|g| g.id.clone())
            .collect();
        self.update_settings_quietly(|st| {
            st.set_friend_groups(mine);
        });
        // The relay sends revokes for these too; this covers a feed that
        // dropped them.
        for gid in &left {
            let ends: Vec<(String, String)> = self
                .mailbox_recipes
                .iter()
                .filter(|r| r.groups.contains(gid) && !r.direct && r.groups.len() == 1)
                .map(|r| (r.from.clone(), r.inst.clone()))
                .collect();
            for r in &mut self.mailbox_recipes {
                r.groups.retain(|g| g != gid);
            }
            self.mailbox_recipes.retain(|r| r.direct || !r.groups.is_empty());
            for (from, inst) in ends {
                self.end_follows(&from, &inst, "left_group", false, cx);
            }
        }
        if groups != self.mailbox_groups {
            self.mailbox_groups = groups;
            cx.notify();
        }
        self.keep_in_step(cx);
    }

    /// For each group kept in step: every group pack a member follows is
    /// followed here too, as a new instance, unless this PC already
    /// follows it or stopped following it.
    fn keep_in_step(&mut self, cx: &mut Context<Self>) {
        let me = self.mailbox_id();
        let mut todo: Vec<(FollowReq, String, String)> = Vec::new();
        for g in self.mailbox_groups.iter().filter(|g| g.me.joined && g.me.in_step) {
            let stopped = self
                .settings
                .friend_groups
                .iter()
                .find(|x| x.id == g.id)
                .map(|x| x.stopped.clone())
                .unwrap_or_default();
            for f in &g.follows {
                let key = format!("{}/{}", f.from, f.inst);
                if f.from == me
                    || stopped.contains(&key)
                    || self.auto_following.contains(&key)
                    || todo.iter().any(|(r, _, _)| r.peer_id == f.from && r.inst == f.inst)
                    || !self.settings.followers(&f.from, &f.inst, "").is_empty()
                {
                    continue;
                }
                let Some(e) = self.mailbox_recipes.iter().find(|e| e.from == f.from && e.inst == f.inst) else {
                    // Its code hasn't arrived yet; the next look gets it.
                    continue;
                };
                todo.push((FollowReq::of(e), g.id.clone(), g.name.clone()));
            }
        }
        for (req, gid, gname) in todo {
            self.auto_following.insert(format!("{}/{}", req.peer_id, req.inst));
            cx.emit(StoreEvent::Notice(
                format!("Following {}'s {} to keep {gname} in step.", req.who(), req.pack()),
                false,
            ));
            self.follow(req, FollowTarget::New, None, &gid, cx);
        }
    }

    /// Starts following. `skip`: the player's answer about mods from other
    /// sources, or None when nobody was asked (then `auto_skip`). `group`:
    /// the group kept in step that followed it.
    fn follow(&mut self, req: FollowReq, target: FollowTarget, skip: Option<Vec<Pin>>, group: &str, cx: &mut Context<Self>) {
        let title = format!("Following {}'s {}", req.who(), req.pack());
        let asked = skip.is_some();
        match target {
            FollowTarget::Existing(root) => {
                let local = self.instance(&root).map(|i| i.id.clone()).unwrap_or_default();
                // What was applied there before stays until this applies,
                // so a failed apply still shows the update waiting.
                let before = self.follow_at(&root).map(|f| f.code.clone()).unwrap_or_default();
                let f = req.recipe(&root, &local, &before, group);
                self.update_settings(
                    |st| {
                        st.followed_recipes.retain(|x| Path::new(&x.root) != root);
                        st.followed_recipes.push(f);
                    },
                    cx,
                );
                self.apply_follow(root, req.code.clone(), title, skip, cx);
            }
            FollowTarget::New => {
                let share = match hopper::sharecode::decode(&req.code) {
                    Ok(s) => s,
                    Err(e) => {
                        cx.emit(StoreEvent::Notice(format!("{e:#}"), true));
                        return;
                    }
                };
                let skip = skip.unwrap_or_else(|| {
                    hopper::sources::carry::auto_skip(&hopper::sharecode::pins(&share), self.settings.allow_sources)
                });
                let (share, note) = hopper::sharecode::without(&share, &skip);
                let base = PathBuf::from(&self.settings.instances_root);
                let f = req.recipe(Path::new(""), "", &req.code, group);
                self.spawn_job(title, move |job| {
                    job.announce();
                    let inst = hopper::sharecode::install_share(&base, &share, job)?;
                    job.detail(format!("Installed {}", inst.name));
                    if let Some(n) = note {
                        job.note(n);
                    }
                    Ok(Msg::Followed(Box::new(FollowedRecipe {
                        root: inst.root.to_string_lossy().into_owned(),
                        local: inst.id.clone(),
                        ..f
                    })))
                });
            }
        }
        // A group pack the player followed: members who keep the group in
        // step follow it too.
        if asked
            && !req.groups.is_empty()
            && let Some(mb) = self.mailbox.clone()
        {
            let (groups, from, inst) = (req.groups.clone(), req.peer_id.clone(), req.inst.clone());
            cx.background_spawn(async move {
                for g in groups {
                    if let Err(e) = mb.group_follow(&g, &from, &inst) {
                        log::warn!("telling group {g} about a follow: {e:#}");
                    }
                }
            })
            .detach();
        }
    }

    /// Applies a followed pack's code at `root` now, or when its game
    /// closes.
    fn apply_follow(&mut self, root: PathBuf, code: String, title: String, skip: Option<Vec<Pin>>, _cx: &mut Context<Self>) {
        if self.is_busy(&root) {
            self.pending_follow.insert(root, skip);
            return;
        }
        let allow = self.settings.allow_sources;
        let r2 = root.clone();
        self.spawn_instance_job(&root, title, move |job| {
            let skip = match skip {
                Some(s) => s,
                None => hopper::sources::carry::auto_skip(
                    &hopper::sharecode::pins(&hopper::sharecode::decode(&code)?),
                    allow,
                ),
            };
            hopper::recipe::apply_to(&r2, &code, &skip, job)?;
            Ok(Msg::RecipeApplied(r2.clone(), code.clone()))
        });
    }

    /// Fills in instance ids for shares and follows from before ids, and
    /// fans out the old all-friends share list once friends are known.
    fn reconcile_shares(&mut self) {
        let live: Vec<(String, String)> = self
            .instances
            .iter()
            .filter(|i| !i.id.is_empty())
            .map(|i| (i.id.clone(), i.root.to_string_lossy().into_owned()))
            .collect();
        let friends: Vec<String> = self.mailbox_friends.iter().map(|f| f.id.clone()).collect();
        self.update_settings_quietly(|st| {
            st.migrate_shared(&friends);
            st.reconcile_instances(&live);
        });
    }

    fn got_friends(&mut self, friends: Vec<mailbox::Friend>, max: usize, cx: &mut Context<Self>) {
        self.mailbox_max = max;
        if friends == self.mailbox_friends {
            return;
        }
        self.update_settings_quietly(|st| {
            for f in &friends {
                for x in st.followed_recipes.iter_mut().filter(|x| x.peer_id == f.id) {
                    x.peer_name = f.name.clone();
                }
            }
        });
        for e in &mut self.mailbox_recipes {
            if let Some(f) = friends.iter().find(|f| f.id == e.from) {
                e.from_name = f.name.clone();
            }
        }
        self.mailbox_friends = friends;
        cx.notify();
    }

    /// Publishes recipes whenever shares, instances, friends or groups
    /// change, and takes back shares turned off or packs deleted.
    fn start_mailbox_publish(&mut self, cx: &mut Context<Self>) {
        if self._mailbox_publish.is_some() {
            return;
        }
        self._mailbox_publish = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(3)).await;
            let got = this.update(cx, |s, _| {
                let mb = s.mailbox.clone()?;
                s.reconcile_shares();
                let due = s.groups_due || s.groups_at.is_none_or(|t| t.elapsed() >= GROUPS_EVERY);
                if due {
                    s.groups_due = false;
                    s.groups_at = Some(Instant::now());
                }
                Some((mb, due))
            });
            let Ok(Some((mb, due))) = got else { continue };
            let (friends, groups) = {
                let mb = mb.clone();
                cx.background_spawn(async move { (mb.friend_list(), due.then(|| mb.groups())) }).await
            };
            // The feed reports the mailbox being down.
            let Ok((friends, max)) = friends else { continue };
            let work = this.update(cx, |s, cx| {
                s.got_friends(friends, max, cx);
                match groups {
                    Some(Ok(g)) => s.got_groups(g, cx),
                    Some(Err(e)) => {
                        log::warn!("reading friend groups: {e:#}");
                        s.groups_due = true;
                    }
                    None => {}
                }
                s.publish_work()
            });
            let Ok(Some(work)) = work else { continue };
            let done = {
                let mb = mb.clone();
                cx.background_spawn(async move { work.run(&mb) }).await
            };
            this.update(cx, |s, cx| s.publish_done(done, cx)).ok();
        }));
    }

    /// What to send now, if anything changed.
    fn publish_work(&mut self) -> Option<PublishWork> {
        if self.publish_failed_at.is_some_and(|t| t.elapsed() < RETRY_AFTER) {
            return None;
        }
        let retry = self.publish_failed_at.take().is_some();
        let live: Vec<String> = self.instances.iter().filter(|i| !i.id.is_empty()).map(|i| i.id.clone()).collect();
        let friends: Vec<String> = self.mailbox_friends.iter().map(|f| f.id.clone()).collect();
        let joined: Vec<String> = self.settings.friend_groups.iter().map(|g| g.id.clone()).collect();
        let plan = self.settings.publish_plan(&live, &friends, &joined);
        // With no instances read (the folder is missing, or not scanned
        // yet) every pack would look deleted; take nothing back then.
        let scanned = !self.instances.is_empty() && Path::new(&self.settings.instances_root).is_dir();
        let stale = if scanned { self.settings.stale_shares(&live) } else { Vec::new() };
        let sig = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            self.instances_sig().hash(&mut h);
            for p in &plan {
                (&p.inst, &p.to, &p.groups).hash(&mut h);
            }
            h.finish()
        };
        if sig == self.mailbox_sig && stale.is_empty() && !retry {
            return None;
        }
        let packs = plan
            .into_iter()
            .filter_map(|p| Some((self.instances.iter().find(|i| i.id == p.inst)?.clone(), p)))
            .collect();
        Some(PublishWork { sig, stale, packs })
    }

    fn publish_done(&mut self, done: PublishDone, cx: &mut Context<Self>) {
        self.mailbox_sig = done.sig;
        if done.failed {
            self.publish_failed_at = Some(Instant::now());
        }
        self.update_settings_quietly(|st| {
            for (s, deleted) in &done.revoked {
                st.forget_sent(s, *deleted);
            }
            for p in &done.sent {
                st.note_sent(p);
            }
        });
        cx.notify();
    }

    /// Everything a share code reads, so a change to a shared pack
    /// publishes it again.
    fn instances_sig(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.instances.len().hash(&mut h);
        for i in &self.instances {
            i.id.hash(&mut h);
            i.name.hash(&mut h);
            i.game_version.hash(&mut h);
            i.loader.hash(&mut h);
            if let Some(p) = &i.pack {
                p.project_id.hash(&mut h);
                p.file_id.hash(&mut h);
                p.mr_project.hash(&mut h);
                p.mr_version.hash(&mut h);
                p.name.hash(&mut h);
            }
            for a in &i.addons {
                a.project_id.hash(&mut h);
                a.file_id.hash(&mut h);
                a.enabled.hash(&mut h);
                a.mr_version.hash(&mut h);
                a.class_id.hash(&mut h);
                if let Some(s) = &a.source {
                    s.url.hash(&mut h);
                    s.seen.hash(&mut h);
                    s.sha256.hash(&mut h);
                    s.class.hash(&mut h);
                }
            }
        }
        h.finish()
    }
}

/// One publish: shares to take back and packs to send.
struct PublishWork {
    sig: u64,
    stale: Vec<(SharedPack, &'static str)>,
    packs: Vec<(hopper::instances::Instance, PublishTo)>,
}

struct PublishDone {
    sig: u64,
    /// Taken back, and whether because the pack was deleted.
    revoked: Vec<(SharedPack, bool)>,
    /// What reached the relay (groups it skipped left out).
    sent: Vec<PublishTo>,
    failed: bool,
}

impl PublishWork {
    /// Runs on a background thread.
    fn run(self, mb: &Mailbox) -> PublishDone {
        let mut out = PublishDone {
            sig: self.sig,
            revoked: Vec::new(),
            sent: Vec::new(),
            failed: false,
        };
        for (s, reason) in self.stale {
            let (to, groups) = if s.group.is_empty() {
                (vec![s.friend.clone()], Vec::new())
            } else {
                (Vec::new(), vec![s.group.clone()])
            };
            match mb.revoke(&s.inst, &to, &groups, reason) {
                Ok(true) => out.revoked.push((s, reason == "deleted")),
                // An old mailbox can't take it back; tried again after it
                // is updated.
                Ok(false) => {}
                Err(e) => {
                    log::warn!("taking back a share: {e:#}");
                    out.failed = true;
                }
            }
        }
        let jobs = hopper::jobs::Jobs::new(Arc::new(|| {}));
        for (inst, mut plan) in self.packs {
            let key = inst.root.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let j = jobs.start_quiet(format!("Sharing {}", inst.name));
            let made = hopper::sharecode::make(&inst, &j);
            j.finish(&Ok::<(), anyhow::Error>(()));
            let Ok(made) = made else { continue };
            match mb.publish_to(&plan.to, &plan.groups, &inst.id, &key, &inst.name, &made.code, &made.left_out) {
                Ok(sent) => {
                    plan.groups.retain(|g| !sent.skipped.contains(g));
                    out.sent.push(plan);
                }
                Err(e) => {
                    log::warn!("sharing {}: {e:#}", inst.name);
                    out.failed = true;
                }
            }
        }
        out
    }
}

/// The feed can hold several codes of one pack (an older mailbox keeps
/// them all); applying each in turn would undo and redo changes. Keeps
/// only the newest code of each pack, and every other event.
fn newest_codes_only(events: Vec<Event>) -> Vec<Event> {
    let mut latest: HashMap<(String, String), u64> = HashMap::new();
    for e in events.iter().filter(|e| e.kind() == Kind::Recipe) {
        latest.insert((e.from.clone(), e.inst.clone()), e.seq);
    }
    events
        .into_iter()
        .filter(|e| e.kind() != Kind::Recipe || latest.get(&(e.from.clone(), e.inst.clone())) == Some(&e.seq))
        .collect()
}

/// Takes the ways a revoke closes off a received pack: shared directly,
/// through some groups. Returns whether none is left (the pack is gone).
fn drop_ways(r: &mut Event, revoke: &Event) -> bool {
    if revoke.direct {
        r.direct = false;
    }
    r.groups.retain(|g| !revoke.groups.contains(g));
    !r.direct && r.groups.is_empty()
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's own `test` macro would come with it.
    use super::{drop_ways, newest_codes_only};
    use hopper::mailbox::Event;

    fn ev(seq: u64, kind: &str, inst: &str, code: &str) -> Event {
        Event {
            seq,
            kind: kind.into(),
            from: "aa".into(),
            inst: inst.into(),
            code: code.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_replayed_feed_applies_each_pack_once() {
        let got = newest_codes_only(vec![
            ev(1, "recipe", "p1", "HOP-1"),
            ev(2, "recipe", "p2", "HOP-a"),
            ev(3, "revoke", "p2", ""),
            ev(4, "recipe", "p1", "HOP-2"),
            ev(5, "group", "", ""),
        ]);
        let seqs: Vec<u64> = got.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, [2, 3, 4, 5]);
    }

    #[test]
    fn a_pack_stays_while_one_way_in_is_left() {
        let mut r = Event {
            direct: true,
            groups: vec!["g1".into(), "g2".into()],
            ..ev(1, "recipe", "p1", "HOP-1")
        };
        // Unshared directly: still there through both groups.
        let unshare = Event { direct: true, groups: Vec::new(), ..ev(2, "revoke", "p1", "") };
        assert!(!drop_ways(&mut r, &unshare));
        // Left one group.
        let left = Event { direct: false, groups: vec!["g1".into()], ..ev(3, "revoke", "p1", "") };
        assert!(!drop_ways(&mut r, &left));
        assert_eq!(r.groups, ["g2"]);
        // The last one goes: the pack is gone.
        let last = Event { direct: false, groups: vec!["g2".into()], ..ev(4, "revoke", "p1", "") };
        assert!(drop_ways(&mut r, &last));
    }
}
