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
use hopper::groupsync::{self, Got, PackCopy};
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
/// How often shared packs are checked for changes to publish.
const PUBLISH_TICK: Duration = Duration::from_millis(500);
/// How often the friend list is read from the mailbox.
const FRIENDS_EVERY: Duration = Duration::from_secs(3);
/// Packs from friends kept for the Friends screen.
const RECIPES_KEPT: usize = 200;
/// How long other members' changes wait for a change made here to be
/// sent before they go in anyway (see `groupsync::Verdict::Held`).
const HELD_FOR: Duration = Duration::from_secs(10);

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

/// "1 minute" / "15 minutes", rounded up, for how long a code lasts.
fn minutes(secs: u64) -> String {
    match secs.div_ceil(60).max(1) {
        1 => "1 minute".into(),
        m => format!("{m} minutes"),
    }
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
            ..Default::default()
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

/// A followed pack's code applied over an instance.
pub struct RecipeApplied {
    pub root: PathBuf,
    pub code: String,
    /// Mods from other sources it left out without asking (names).
    pub held: Vec<String>,
    /// The player's answer about those mods (`hopper::recipe::pin_key`s to
    /// skip), when they were asked.
    pub declined: Option<Vec<String>>,
}

/// Other members' changes taken into a group copy (`hopper::groupsync`).
pub struct GroupMerged {
    pub local: String,
    pub root: PathBuf,
    pub merged: groupsync::Merged,
}

/// The newest waiting code's number, which a failed try is kept against.
fn newest_waiting(c: &PackCopy) -> String {
    c.waiting.values().map(|g| g.seq).max().unwrap_or(0).to_string()
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
            s.spawn_job("Installing a pack from a friend", move |job| {
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
                    job.note("Turn on friends to get their changes to this pack.");
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
                    Err(e) if mailbox::failure(&e) == mailbox::Failure::InviteUnknown => {
                        job.note("This copy won't get their changes. The code ran out or was used already. Ask your friend for a new code to get them.");
                        Ok(Msg::installed(inst.root, &[], None))
                    }
                    Err(e) => {
                        job.note(format!("This copy won't get their changes. {}", mailbox::says(&e)));
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

    /// The open invite code for `inst` (an instance id, or empty for a
    /// plain invite), while it lasts.
    pub fn mailbox_invite(&self, inst: &str) -> Option<Invite> {
        let now = Instant::now();
        self.mailbox_invites.iter().find(|i| i.inst == inst && now < i.until).cloned()
    }

    /// An open invite worth sending again: the mailbox only lets a PC keep
    /// a few open at once, so a second click copies the same code instead
    /// of making another. One about to run out isn't worth sending.
    fn reusable_invite(&self, inst: &str) -> Option<Invite> {
        self.mailbox_invite(inst)
            .filter(|i| i.until.saturating_duration_since(Instant::now()) >= Duration::from_secs(60))
    }

    /// Keeps `inv` as the open invite for its purpose.
    fn keep_invite(&mut self, inv: Invite) {
        let now = Instant::now();
        self.mailbox_invites.retain(|i| i.inst != inv.inst && now < i.until);
        self.mailbox_invites.push(inv);
    }

    /// Forgets the invite for `inst` (it was used).
    fn drop_invite(&mut self, inst: &str) {
        self.mailbox_invites.retain(|i| i.inst != inst);
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
            cx.emit(StoreEvent::Notice("Your friend list is full. Remove a friend to add someone new.".into(), true));
            return;
        }
        if let Some(open) = self.reusable_invite(inst) {
            cx.write_to_clipboard(ClipboardItem::new_string(open.code.clone()));
            let left = open.until.saturating_duration_since(Instant::now()).as_secs();
            cx.emit(StoreEvent::Notice(
                format!("Code copied again. It still works for one friend, for {} more.", minutes(left)),
                false,
            ));
            return;
        }
        let inst = inst.to_string();
        cx.spawn(async move |this, cx| {
            let i2 = inst.clone();
            let r = cx.background_spawn(async move { mb.invite_for(&i2) }).await;
            this.update(cx, |s, cx| match r {
                Ok((code, secs)) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                    s.keep_invite(Invite {
                        code,
                        until: Instant::now() + Duration::from_secs(secs),
                        inst,
                    });
                    cx.emit(StoreEvent::Notice(
                        format!("Code copied. Send it to your friend. It works once, for {}.", minutes(secs)),
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

    /// Copies the instance's share code with a follow part: whoever pastes
    /// it installs the pack, becomes a friend and follows it. Works once,
    /// for as long as an invite.
    pub fn copy_follow_code(&mut self, root: &Path, cx: &mut Context<Self>) {
        let Some(inst) = self.instance(root).cloned() else { return };
        let Some(mb) = self.mailbox_or_say(cx) else { return };
        if self.friends_full() {
            cx.emit(StoreEvent::Notice("Your friend list is full. Remove a friend to add someone new.".into(), true));
            return;
        }
        // The pack may have changed since the last copy, so the share code is
        // made fresh each time; only the follow part is reused.
        let open = self
            .reusable_invite(&inst.id)
            .map(|i| (i.code, i.until.saturating_duration_since(Instant::now()).as_secs()));
        cx.spawn(async move |this, cx| {
            let id = inst.id.clone();
            let r = cx
                .background_spawn(async move {
                    let jobs = hopper::jobs::Jobs::new(Arc::new(|| {}));
                    let j = jobs.start_quiet(format!("Sharing {}", inst.name));
                    let made = hopper::sharecode::make(&inst, &j);
                    j.finish(&Ok::<(), anyhow::Error>(()));
                    let made = made?;
                    let reused = open.is_some();
                    let (invite, secs) = match open {
                        Some(o) => o,
                        None => mb.invite_for(&inst.id)?,
                    };
                    anyhow::Ok((hopper::sharecode::with_follow(&made.code, &invite), invite, secs, reused))
                })
                .await;
            this.update(cx, |s, cx| match r {
                Ok((text, invite, secs, reused)) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    if !reused {
                        s.keep_invite(Invite {
                            code: invite,
                            until: Instant::now() + Duration::from_secs(secs),
                            inst: id,
                        });
                    }
                    let more = if reused { " more" } else { "" };
                    cx.emit(StoreEvent::Notice(
                        format!(
                            "Share code copied. It works once, for {}{more}. Whoever uses it gets your changes to this pack.",
                            minutes(secs)
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
                    "{} is now your friend. Their packs show here when they share one.",
                    if f.name.is_empty() { "Your friend" } else { &f.name }
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
                Ok(format!("Removed {name}. Packs you got from them stay on your PC as they are."))
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
        self.forget_follow_state(root);
        self.sync_copies();
        if did {
            cx.emit(StoreEvent::Notice("Stopped following. Your copy stays as it is and won't change when theirs does.".into(), false));
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
        self.sync_copies();
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

    /// Something waits in the Friends panel: a friend's pack changed since
    /// it was last open, or a group invitation. The title bar's dot.
    pub fn friends_news(&self) -> bool {
        if self.sync_status() == SyncStatus::Off {
            return false;
        }
        let seen = self.settings.friends_seen;
        self.mailbox_recipes.iter().any(|e| e.at > seen && !self.pack_hidden(e))
            || self.mailbox_groups.iter().any(|g| !g.me.joined)
    }

    /// The Friends panel was open: what's there now isn't new any more.
    /// A friend's own "2 new" stays until their page is opened, so anyone
    /// without a time of their own keeps the old one.
    pub fn mark_friends_seen(&mut self, cx: &mut Context<Self>) {
        let newest = self.mailbox_recipes.iter().map(|e| e.at).max().unwrap_or(0);
        if newest > self.settings.friends_seen {
            let old = self.settings.friends_seen;
            let ids: Vec<String> = self.mailbox_recipes.iter().map(|e| e.from.clone()).collect();
            self.update_settings_quietly(|st| {
                for id in ids {
                    st.friend_seen.entry(id).or_insert(old);
                }
                st.friends_seen = newest;
            });
            cx.notify();
        }
    }

    /// Changes to `id`'s packs newer than this are new to the player.
    pub fn friend_seen(&self, id: &str) -> u64 {
        self.settings.friend_seen.get(id).copied().unwrap_or(self.settings.friends_seen)
    }

    /// `id`'s page was open: their packs aren't new any more.
    pub fn mark_friend_seen(&mut self, id: &str, cx: &mut Context<Self>) {
        let newest = self.mailbox_recipes.iter().filter(|e| e.from == id).map(|e| e.at).max().unwrap_or(0);
        if newest > self.friend_seen(id) {
            let id = id.to_string();
            self.update_settings_quietly(|st| {
                st.friend_seen.insert(id, newest);
            });
            cx.notify();
        }
    }

    /// Hides a friend's pack from the panel until it changes again.
    pub fn hide_friend_pack(&mut self, e: &Event, cx: &mut Context<Self>) {
        let key = hidden_key(e);
        self.update_settings(
            |st| {
                // One entry per pack: an older change's is dropped.
                let pack = format!("{}/{}/", e.from, e.inst);
                st.friends_hidden.retain(|k| !k.starts_with(&pack));
                st.friends_hidden.push(key);
            },
            cx,
        );
    }

    pub fn pack_hidden(&self, e: &Event) -> bool {
        self.settings.friends_hidden.contains(&hidden_key(e))
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
                Ok(format!("Created {name}. Your friends get an invitation to join."))
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

    /// Adds and takes out members without a notice of its own, for the
    /// Friends panel's drags and menus: their toast already says it and
    /// offers Undo. Errors still show.
    pub fn edit_members(&mut self, id: &str, add: Vec<String>, out: Vec<String>, cx: &mut Context<Self>) {
        let id = id.to_string();
        self.mailbox_do(
            move |mb| {
                mb.edit_group(&id, None, &add, &out)?;
                Ok(String::new())
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
                Ok(format!("Took them out of {group}."))
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
                Ok(format!("Deleted {group}. Its packs stop getting changes. Everyone keeps their copies."))
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
                    format!("Left {group}. Packs you got only from the group stop getting changes. Your copies stay.")
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
                    format!("{group}: your packs now stay the same as the group's.")
                } else {
                    format!("{group}: your packs won't follow the group any more. Packs you already have stay.")
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
                        s.update_settings(
                            |st| {
                                st.mailbox_handled = 0;
                                st.group_copies.iter_mut().for_each(|c| c.relay_reset());
                            },
                            cx,
                        );
                        cx.emit(StoreEvent::Notice(
                            "Your friend list was lost. Invite your friends again with a new code.".into(),
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
        self.mailbox_invites.clear();
        self._mailbox_watch = None;
        self._mailbox_publish = None;
    }

    /// A new instance following a friend's pack exists and its mods are
    /// going in: it's the follow from now on, even if that never finishes
    /// (`code` stays empty until it does, so the update shows waiting).
    pub(super) fn on_follow_made(&mut self, f: FollowedRecipe, cx: &mut Context<Self>) {
        let root = PathBuf::from(&f.root);
        // The install is still running on it; the tick leaves it alone.
        self.follow_applying.insert(root.clone());
        self.update_settings(
            |st| {
                st.followed_recipes.retain(|x| Path::new(&x.root) != root);
                st.followed_recipes.push(f);
            },
            cx,
        );
    }

    /// A new instance following a friend's pack was made.
    pub(super) fn on_followed(&mut self, f: FollowedRecipe, cx: &mut Context<Self>) {
        let root = PathBuf::from(&f.root);
        self.follow_applying.remove(&root);
        self.auto_following.remove(&format!("{}/{}", f.peer_id, f.inst));
        self.update_settings(
            |st| {
                st.followed_recipes.retain(|x| Path::new(&x.root) != root);
                st.followed_recipes.push(f);
            },
            cx,
        );
        self.sync_copies();
        self.handle(Msg::installed(root, &[], None), cx);
    }

    /// A followed pack's code was applied at its root.
    pub(super) fn on_recipe_applied(&mut self, r: RecipeApplied, cx: &mut Context<Self>) {
        let RecipeApplied { root, code, held, declined } = r;
        self.follow_applying.remove(&root);
        // It worked: the failed tries before it are done with.
        if let Some(old) = self.follow_retry.remove(&root) {
            self.jobs.dismiss(old.job);
        }
        // The player answered about mods from other sources while this ran
        // by itself: apply again with their answer.
        let answer = match self.pending_follow.remove(&root) {
            Some(Some(skip)) if declined.is_none() => Some(skip),
            _ => None,
        };
        self.update_settings(
            |st| {
                for f in st.followed_recipes.iter_mut().filter(|f| Path::new(&f.root) == root) {
                    f.code = code.clone();
                    f.held = held.clone();
                    if let Some(d) = &declined {
                        f.declined = d.clone();
                    }
                }
            },
            cx,
        );
        // A group pack's first version is in: from now on it syncs both ways.
        self.sync_copies();
        self.handle(Msg::InstanceChanged(root.clone()), cx);
        if let Some(skip) = answer
            && let Some(f) = self.follow_at(&root).cloned()
        {
            self.apply_follow(root, f.seen.clone(), format!("Updating from {}'s {}", f.peer_name, f.name), Some(skip), cx);
        }
    }

    /// Applying a followed pack's code failed: said once, shown on its row,
    /// and tried again after a while (unless trying again can't help).
    pub(super) fn on_recipe_failed(&mut self, root: PathBuf, code: String, e: anyhow::Error, job: u64, cx: &mut Context<Self>) {
        self.follow_applying.remove(&root);
        let prev = self.follow_retry.get(&root);
        // One failed task per follow on the task list, the newest.
        if let Some(p) = prev.filter(|p| p.job != job) {
            self.jobs.dismiss(p.job);
        }
        let r = hopper::recipe::Retry::failed(prev, &code, &e, job, Instant::now());
        log::warn!("applying a followed pack at {}: {e:#} (try {})", root.display(), r.failures);
        if r.limited.is_some() {
            self.tell_github_limit(&r, cx);
        } else if r.tell
            && let Some(f) = self.follow_at(&root)
        {
            let local = self.instance(&root).map(|i| i.name.clone()).unwrap_or_else(|| f.name.clone());
            let who = if f.peer_name.is_empty() { "your friend" } else { &f.peer_name };
            let again = if r.next.is_some() { " Hopper tries again in a moment." } else { "" };
            cx.emit(StoreEvent::Notice(format!("Couldn't update {local} with {who}'s changes. {}{again}", r.error), true));
        }
        self.follow_retry.insert(root, r);
        cx.notify();
    }

    /// Says GitHub's rate limit once until it resets, however many packs
    /// and retries it holds up.
    fn tell_github_limit(&mut self, r: &hopper::recipe::Retry, cx: &mut Context<Self>) {
        let now = Instant::now();
        if self.github_limit_told.is_some_and(|t| now < t) {
            return;
        }
        self.github_limit_told = r.limited;
        cx.emit(StoreEvent::GitHubLimited(r.error.clone()));
    }

    /// Why the last automatic update of the follow at `root` failed, and
    /// whether Hopper tries again by itself.
    pub fn follow_error(&self, root: &Path) -> Option<(&str, bool)> {
        if let Some(c) = self.group_copy(root) {
            let r = self
                .group_retry
                .get(&c.local)
                .filter(|r| !c.waiting.is_empty() && (r.code == newest_waiting(c) || r.waits_for_limit(Instant::now())))?;
            return Some((r.error.as_str(), r.next.is_some()));
        }
        let f = self.follow_at(root)?;
        let r = self.follow_retry.get(root).filter(|r| f.waiting() && (r.code == f.seen || r.waits_for_limit(Instant::now())))?;
        Some((r.error.as_str(), r.next.is_some()))
    }

    /// Whether the follow at `root` is being updated now, or will be as
    /// soon as another task on it ends or its game finishes launching.
    pub fn follow_updating(&self, root: &Path) -> bool {
        if let Some(c) = self.group_copy(root) {
            return self.group_merging.contains(&c.local) || (!c.waiting.is_empty() && self.follow_error(root).is_none());
        }
        self.follow_applying.contains(root)
            || self.follow_at(root).is_some_and(|f| f.waiting() && self.follow_error(root).is_none())
    }

    /// An instance was deleted in Hopper: what it followed stops (and a
    /// group pack isn't followed again by keeping the group in step).
    pub(super) fn forget_follows(&mut self, root: &Path) {
        let r = root.to_string_lossy().into_owned();
        self.forget_follow_state(root);
        self.update_settings_quietly(|st| {
            st.stop_following(&r);
        });
        self.sync_copies();
    }

    fn forget_follow_state(&mut self, root: &Path) {
        self.pending_follow.remove(root);
        self.follow_applying.remove(root);
        if let Some(r) = self.follow_retry.remove(root) {
            self.jobs.dismiss(r.job);
        }
    }

    /// A task changing the instance, or its game launching (reading the
    /// mods folder): a follow waits. A running game doesn't stop it, as with
    /// Update mods: the changes load on the game's next start, the Mods tab
    /// says so, and a jar the game holds open changes once it lets go
    /// (`hopper::locked`).
    pub(super) fn follow_busy(&self, root: &Path) -> bool {
        self.is_busy(root) || self.is_launching(root)
    }

    /// Applies every follow whose newest code isn't applied yet and can be
    /// now: a code that just came, one that waited for a task to end or a
    /// launch, one Hopper was closed before applying (on start), and
    /// failed tries once their wait is over (`hopper::recipe::due`). Runs
    /// on the store's wake-ups and twice a second while friends sync runs.
    pub(super) fn follow_tick(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        let due: Vec<FollowedRecipe> = self
            .settings
            .followed_recipes
            .iter()
            .filter(|f| {
                let root = Path::new(&f.root);
                // A pack from the feed, or one the player asked to update
                // (a PC on the network, which only updates on request).
                let ours = self.pending_follow.contains_key(root)
                    || self.mailbox_recipes.iter().any(|e| f.matches(&e.from, &e.inst, &e.key));
                ours && !self.follow_applying.contains(root)
                    // Kept both ways with a group: changes come mod by mod.
                    && !self.settings.group_copies.iter().any(|c| c.inst == f.inst && c.local == f.local)
                    && hopper::recipe::due(f, self.follow_retry.get(root), self.follow_busy(root), now)
                    // An instance deleted outside Hopper has nothing to update.
                    && root.is_dir()
            })
            .cloned()
            .collect();
        for f in due {
            let root = PathBuf::from(&f.root);
            let skip = self.pending_follow.remove(&root).flatten();
            self.apply_follow(root, f.seen.clone(), format!("Updating from {}'s {}", f.peer_name, f.name), skip, cx);
        }
        // Group packs with other members' changes waiting.
        let mut ready: Vec<(PackCopy, PathBuf, bool)> = Vec::new();
        for c in &self.settings.group_copies {
            if c.waiting.is_empty() || self.group_merging.contains(&c.local) {
                continue;
            }
            let Some(root) = self.instances.iter().find(|i| i.id == c.local).map(|i| i.root.clone()) else { continue };
            // Waiting for a change made here to be sent; after a while the
            // others' changes go in anyway.
            let ignore_unsent = match self.group_held.get(&c.local) {
                Some((seq, t)) if *seq == c.sent_seq => {
                    if t.elapsed() < HELD_FOR {
                        continue;
                    }
                    true
                }
                _ => false,
            };
            let due = match self.group_retry.get(&c.local) {
                // GitHub's limit: a newer code would fail the same way.
                Some(r) if r.waits_for_limit(now) => false,
                Some(r) if r.code == newest_waiting(c) => r.next.is_some_and(|t| now >= t),
                _ => true,
            };
            if due && !self.follow_busy(&root) && root.is_dir() {
                ready.push((c.clone(), root, ignore_unsent));
            }
        }
        for (c, root, ignore_unsent) in ready {
            self.merge_group(c, root, ignore_unsent);
        }
    }

    // ---- Group packs kept both ways ---------------------------------------

    /// The copy of a group pack at `root`, when it is kept both ways.
    pub fn group_copy(&self, root: &Path) -> Option<&PackCopy> {
        let id = &self.instance(root)?.id;
        if id.is_empty() {
            return None;
        }
        self.settings.group_copies.iter().find(|c| &c.local == id)
    }

    /// Who in the group changed this mod last, when it wasn't this PC:
    /// "Turned off by Mia".
    pub fn group_mark(&self, root: &Path, a: &hopper::instances::Addon) -> Option<String> {
        use groupsync::Kind;
        // Kept only on this PC: what others did to it didn't happen here.
        if self.instance(root).is_some_and(|i| hopper::onlyhere::is_kept(i, a)) {
            return None;
        }
        // A pack mod may be marked by its file name, as codes name it.
        let marks = &self.group_copy(root)?.marks;
        let m = marks
            .get(&groupsync::addon_key(a))
            .or_else(|| marks.get(&format!("n:{}", a.file_name.trim_end_matches(".disabled").to_lowercase())))?;
        if m.name.is_empty() || m.by == self.mailbox_id() {
            return None;
        }
        // Changed here since (not sent yet): the mark is out of date.
        match m.kind {
            Kind::Off if a.enabled => return None,
            Kind::On | Kind::Added if !a.enabled => return None,
            _ => {}
        }
        // A pack mod set to another file reads as an update.
        let kind = if m.kind == Kind::Added && a.from_pack { Kind::Updated } else { m.kind };
        Some(kind.says(&m.name))
    }

    /// Keeps the list of group packs kept both ways matching what is
    /// shared and followed here. A member's copy that stopped is taken
    /// back from the group, so nobody keeps getting an old version of it.
    pub(super) fn sync_copies(&mut self) {
        let mut copies = self.settings.group_copies.clone();
        let recipes = &self.mailbox_recipes;
        let gone = groupsync::reconcile(&mut copies, &self.settings, |f| {
            recipes
                .iter()
                .find(|e| f.matches(&e.from, &e.inst, &e.key))
                .map(|e| e.groups.clone())
                .unwrap_or_default()
        });
        // Members' copies that came before this one was set up.
        let mut late: Vec<Event> = Vec::new();
        for c in &mut copies {
            let strays: Vec<Event> = self.group_strays.iter().filter(|((_, i), _)| *i == c.inst).map(|(_, e)| e.clone()).collect();
            for e in strays {
                self.group_strays.remove(&(e.from.clone(), e.inst.clone()));
                late.push(e.clone());
                c.arrive(&e.from, Got { code: e.code, seq: e.seq, at: e.at, name: e.from_name, stamps: e.marks });
            }
        }
        // A copy set up only now (the follow went in after the sharer's
        // recipe came) catches up on the sharer's shared settings as well.
        for c in copies.iter().filter(|c| c.local != c.inst && !self.settings.group_copies.iter().any(|o| o.local == c.local && o.inst == c.inst)) {
            late.extend(self.mailbox_recipes.iter().filter(|e| e.inst == c.inst && !e.configs.is_empty()).cloned());
        }
        if copies != self.settings.group_copies {
            self.update_settings_quietly(|st| st.group_copies = copies);
        }
        // Their shared settings too: a copy set up now catches up on them.
        for e in &late {
            let taken = self.take_configs(e);
            self.cfg.unsaid.extend(taken);
        }
        let back: Vec<PackCopy> = gone.into_iter().filter(|c| c.local != c.inst).collect();
        for c in &back {
            self.group_held.remove(&c.local);
            if let Some(r) = self.group_retry.remove(&c.local) {
                self.jobs.dismiss(r.job);
            }
        }
        if !back.is_empty()
            && let Some(mb) = self.mailbox.clone()
        {
            std::thread::spawn(move || {
                for c in back {
                    if let Err(e) = mb.revoke(&c.inst, &[], &[c.group.clone()], "unshared") {
                        log::warn!("taking back this PC's copy of a group pack: {e:#}");
                    }
                }
            });
        }
    }

    /// A member's copy of a group pack arrived: taken into this PC's copy.
    fn got_copy(&mut self, e: Event, cx: &mut Context<Self>) {
        if e.from == self.mailbox_id() {
            return;
        }
        let name = if e.from_name.is_empty() { self.friend_name(&e.from) } else { e.from_name.clone() };
        let g = Got { code: e.code.clone(), seq: e.seq, at: e.at, name, stamps: e.marks.clone() };
        if self.settings.group_copies.iter().any(|c| c.inst == e.inst) {
            self.update_settings_quietly(|st| {
                for c in st.group_copies.iter_mut().filter(|c| c.inst == e.inst) {
                    c.arrive(&e.from, g.clone());
                }
            });
            let taken = self.take_configs(&e);
            self.say_taken(taken, cx);
            self.follow_tick(cx);
        } else {
            let k = (e.from.clone(), e.inst.clone());
            if self.group_strays.get(&k).is_none_or(|x| x.seq < e.seq) {
                self.group_strays.insert(k, e);
            }
        }
    }

    /// Takes other members' waiting changes into the copy at `root`, off
    /// the task list unless it changes something.
    fn merge_group(&mut self, copy: PackCopy, root: PathBuf, ignore_unsent: bool) {
        if let Some(r) = self.group_retry.get(&copy.local) {
            self.jobs.dismiss(r.job);
        }
        self.group_merging.insert(copy.local.clone());
        let allow = self.settings.allow_sources;
        let name = self.instance(&root).map(|i| i.name.clone()).unwrap_or_default();
        let mut who: Vec<&str> = copy.waiting.values().map(|g| g.name.as_str()).filter(|n| !n.is_empty()).collect();
        who.dedup();
        let title = match who.as_slice() {
            [one] => format!("Updating {name} with {one}'s changes"),
            _ => format!("Updating {name} with the group's changes"),
        };
        let (tx, local, r2) = (self.tx.clone(), copy.local.clone(), root.clone());
        self.spawn_instance_job_quiet(&root, title, move |job| {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| groupsync::merge(&r2, &copy, allow, ignore_unsent, job)))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("Hopper hit a bug and stopped this task.")));
            match r {
                Ok(merged) => Ok(Msg::GroupMerged(Box::new(GroupMerged { local, root: r2, merged }))),
                Err(e) => {
                    let text = format!("{e:#}");
                    let _ = tx.send(Msg::GroupMergeFailed(local, e, job.id()));
                    Err(anyhow::anyhow!(text))
                }
            }
        });
    }

    pub(super) fn on_group_merged(&mut self, m: GroupMerged, cx: &mut Context<Self>) {
        let GroupMerged { local, root, merged } = m;
        self.group_merging.remove(&local);
        if let Some(old) = self.group_retry.remove(&local) {
            self.jobs.dismiss(old.job);
        }
        let mut sent_seq = 0;
        self.update_settings_quietly(|st| {
            let Some(c) = st.group_copies.iter_mut().find(|c| c.local == local) else { return };
            let mut taken: Vec<(String, String)> = Vec::new();
            for (from, g) in merged.taken {
                if c.waiting.get(&from).is_some_and(|w| w.seq == g.seq) {
                    c.waiting.remove(&from);
                }
                taken.push((from.clone(), g.code.clone()));
                c.got.insert(from, g);
            }
            for (k, mk) in merged.marks {
                if c.marks.get(&k).is_none_or(|x| x.seq <= mk.seq) {
                    c.marks.insert(k, mk);
                }
            }
            sent_seq = c.sent_seq;
            // The sharer's code is in: the follow has it applied now.
            for f in st.followed_recipes.iter_mut().filter(|f| f.local == local) {
                if let Some((_, code)) = taken.iter().find(|(from, _)| *from == f.peer_id) {
                    f.code = code.clone();
                }
            }
        });
        if merged.held {
            if self.group_held.get(&local).is_none_or(|(s, _)| *s != sent_seq) {
                self.group_held.insert(local, (sent_seq, Instant::now()));
            }
        } else {
            self.group_held.remove(&local);
        }
        // The group moved this instance to another loader version (saved by
        // the merge). Installed like a change in its Settings, but a running
        // game is left alone until it closes.
        if merged.loader.is_some() {
            if self.running(&root).is_some() {
                self.group_loader_waiting.insert(root.clone());
            } else if let Some(inst) = hopper::instances::load(&root) {
                self.install_loader(inst);
            }
        }
        if !merged.made.is_empty() {
            self.handle(Msg::InstanceChanged(root), cx);
        }
        cx.notify();
    }

    pub(super) fn on_group_merge_failed(&mut self, local: String, e: anyhow::Error, job: u64, cx: &mut Context<Self>) {
        self.group_merging.remove(&local);
        let Some(c) = self.settings.group_copies.iter().find(|c| c.local == local) else { return };
        let key = newest_waiting(c);
        let prev = self.group_retry.get(&local);
        if let Some(p) = prev.filter(|p| p.job != job) {
            self.jobs.dismiss(p.job);
        }
        let r = hopper::recipe::Retry::failed(prev, &key, &e, job, Instant::now());
        log::warn!("taking group changes into {local}: {e:#} (try {})", r.failures);
        let root = self.instances.iter().find(|i| i.id == local).map(|i| (i.root.clone(), i.name.clone()));
        if r.limited.is_some() {
            self.tell_github_limit(&r, cx);
        } else if r.tell
            && let Some((_, name)) = &root
        {
            let again = if r.next.is_some() { " Hopper tries again in a moment." } else { "" };
            cx.emit(StoreEvent::Notice(format!("Couldn't update {name} with the group's changes. {}{again}", r.error), true));
        }
        self.group_retry.insert(local, r);
        // Some changes may have gone in before the one that failed.
        if let Some((root, _)) = root {
            self.handle(Msg::InstanceChanged(root), cx);
        }
        cx.notify();
    }

    // ---- Inside ----------------------------------------------------------

    fn mailbox_or_say(&self, cx: &mut Context<Self>) -> Option<Arc<Mailbox>> {
        if self.mailbox.is_none() {
            let text = match self.sync_status() {
                SyncStatus::Off => "Turn on friends first.".to_string(),
                SyncStatus::Down(e) => e,
                _ => "Still connecting. Try again in a moment.".to_string(),
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
                        format!("{who} removed you as a friend. Packs you got from them stay on your PC but won't change any more."),
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
        if e.key == groupsync::COPY_KEY {
            self.got_copy(e, cx);
            return;
        }
        // One entry per pack; one from before ids (its folder name) is the
        // same pack.
        self.mailbox_recipes.retain(|x| {
            !(x.from == e.from && (x.inst == e.inst || (!e.key.is_empty() && x.inst.eq_ignore_ascii_case(&e.key))))
        });
        self.mailbox_recipes.push(e.clone());
        if self.mailbox_recipes.len() > RECIPES_KEPT {
            self.mailbox_recipes.remove(0);
        }
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
                f.seen = e.code.clone();
            }
        });
        // A group pack kept both ways takes its sharer's code mod by mod,
        // like every other member's.
        self.sync_copies();
        if self.settings.group_copies.iter().any(|c| c.inst == e.inst && c.local != c.inst) {
            let name = if e.from_name.is_empty() { self.friend_name(&e.from) } else { e.from_name.clone() };
            let g = Got { code: e.code.clone(), seq: e.seq, at: e.at, name, stamps: e.marks.clone() };
            // The follow's applied code moves on once the merge took it in
            // (`on_group_merged`), not now: a copy set up again from the
            // follow would start from a code whose changes never went in.
            self.update_settings_quietly(|st| {
                for c in st.group_copies.iter_mut().filter(|c| c.inst == e.inst && c.local != c.inst) {
                    c.arrive(&e.from, g.clone());
                }
            });
            let taken = self.take_configs(&e);
            self.say_taken(taken, cx);
        }
        // Applied by the tick, which also catches a code seen before but
        // never applied (Hopper closed first, or the apply failed).
        self.follow_tick(cx);
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
            cx.emit(StoreEvent::Notice(format!("{what} Your copy stays on your PC but won't change any more."), false));
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
                cx.emit(StoreEvent::Notice(format!("{who} joined with your code and now gets your changes to {name}."), false));
            }
            None => cx.emit(StoreEvent::Notice(format!("{who} is now your friend."), false)),
        }
        self.drop_invite(&e.inst);
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
        self.sync_copies();
        self.keep_in_step(cx);
    }

    /// For each group kept in step: every group pack a member follows is
    /// followed here too, as a new instance, unless this PC already
    /// follows it or stopped following it (`groupsync::to_follow`).
    fn keep_in_step(&mut self, cx: &mut Context<Self>) {
        // `auto_following` covers the minutes before the new instance
        // exists; from then on the follow is in settings.
        let todo = groupsync::to_follow(&self.settings, &self.mailbox_groups, &self.mailbox_recipes, &self.mailbox_id(), |k| {
            self.auto_following.contains(k)
        });
        for (e, gid, gname) in todo {
            let req = FollowReq::of(&e);
            self.auto_following.insert(format!("{}/{}", req.peer_id, req.inst));
            cx.emit(StoreEvent::Notice(
                format!("Following {}'s {}, so your packs stay the same as {gname}.", req.who(), req.pack()),
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
                let (tx, wake) = (self.tx.clone(), self.wake.clone());
                self.spawn_job(title, move |job| {
                    job.announce();
                    // The follow is recorded as soon as the instance exists,
                    // with nothing applied yet: if putting the mods in fails,
                    // is cancelled or Hopper closes first, the follow finishes
                    // into this instance later instead of a new copy.
                    let mut made: Option<PathBuf> = None;
                    let r = hopper::sharecode::install_share_then(&base, &share, job, |inst| {
                        made = Some(inst.root.clone());
                        let started = FollowedRecipe {
                            root: inst.root.to_string_lossy().into_owned(),
                            local: inst.id.clone(),
                            code: String::new(),
                            ..f.clone()
                        };
                        let _ = tx.send(Msg::FollowMade(Box::new(started)));
                        wake();
                    });
                    let inst = match (r, made) {
                        (Ok(inst), _) => inst,
                        // Shown on the follow and tried again like a failed
                        // update; the task keeps the same text.
                        (Err(e), Some(root)) => {
                            let text = format!("{e:#}");
                            let _ = tx.send(Msg::InstanceChanged(root.clone()));
                            let _ = tx.send(Msg::RecipeFailed(root, f.code.clone(), e, job.id()));
                            return Err(anyhow::anyhow!(text));
                        }
                        (Err(e), None) => return Err(e),
                    };
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

    /// Applies a followed pack's code at `root` now, or once its game
    /// closes and other tasks on it end ([`Self::follow_tick`]). `skip`:
    /// the player's answer about mods from other sources (None: nobody
    /// was asked).
    fn apply_follow(&mut self, root: PathBuf, code: String, title: String, skip: Option<Vec<Pin>>, _cx: &mut Context<Self>) {
        if self.follow_busy(&root) || self.follow_applying.contains(&root) {
            // Keep an answer the player gave; the tick applies it later.
            if skip.is_some() || !self.pending_follow.contains_key(&root) {
                self.pending_follow.insert(root, skip);
            }
            return;
        }
        // The next try starts: the last failed one leaves the task list.
        if let Some(r) = self.follow_retry.get(&root) {
            self.jobs.dismiss(r.job);
        }
        self.follow_applying.insert(root.clone());
        let allow = self.settings.allow_sources;
        // What was applied here last, so what they removed or turned back
        // on since then changes here too.
        let (prev, declined) = self
            .follow_at(&root)
            .map(|f| (f.code.clone(), f.declined.clone()))
            .unwrap_or_default();
        // History names the friend on what this changes, and only on that.
        let who = self.follow_at(&root).map(|f| {
            let mc = self.mailbox_friends().iter().find(|m| m.id == f.peer_id).map(|m| m.mc_id.clone()).unwrap_or_default();
            hopper::actor::Who::friend(&f.peer_name, &f.peer_id, &f.group, &f.name).with_face(&mc)
        });
        let (tx, r2) = (self.tx.clone(), root.clone());
        self.spawn_instance_job(&root, title, move |job| {
            let work = || -> anyhow::Result<RecipeApplied> {
                use hopper::recipe::pin_key;
                let (skip, held, answer) = match skip {
                    Some(s) => {
                        let answer = s.iter().map(pin_key).collect();
                        (s, Vec::new(), Some(answer))
                    }
                    None => {
                        let pins = hopper::sharecode::pins(&hopper::sharecode::decode(&code)?);
                        let s = hopper::sources::carry::auto_skip(&pins, allow);
                        // Left out without asking, unless the player said
                        // to skip it before: Update asks about these.
                        let held = s
                            .iter()
                            .filter(|p| !declined.contains(&pin_key(p)))
                            .map(hopper::sources::carry::display_name)
                            .collect();
                        (s, held, None)
                    }
                };
                let _who = hopper::actor::begin(&r2, who.clone());
                hopper::recipe::apply_to(&r2, &code, &prev, &skip, job)?;
                Ok(RecipeApplied { root: r2.clone(), code: code.clone(), held, declined: answer })
            };
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("Hopper hit a bug and stopped this task.")));
            match r {
                Ok(a) => Ok(Msg::RecipeApplied(Box::new(a))),
                // The store hears about a failure too, to try again; the
                // task keeps the same text.
                Err(e) => {
                    let text = format!("{e:#}");
                    let _ = tx.send(Msg::RecipeFailed(r2.clone(), code.clone(), e, job.id()));
                    Err(anyhow::anyhow!(text))
                }
            }
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
        // The relay answered. On a quiet feed the first long poll only
        // returns after a minute, so this is what ends "Connecting".
        if !self.mailbox_heard || self.mailbox_error.is_some() {
            self.mailbox_heard = true;
            self.mailbox_error = None;
            cx.notify();
        }
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
        // A plain invite sends no event when it's used, so a new friend
        // turning up while one is open means it was used (codes work once).
        if self.mailbox_invite("").is_some() {
            let new: Vec<&mailbox::Friend> =
                friends.iter().filter(|f| !self.mailbox_friends.iter().any(|o| o.id == f.id)).collect();
            if let Some(f) = new.first() {
                let who = if f.name.is_empty() { "Your friend".to_string() } else { f.name.clone() };
                cx.emit(StoreEvent::Notice(format!("{who} joined with your code."), false));
                self.drop_invite("");
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
        // A change is looked for often, so a friend following the pack sees
        // it within a second or two (their feed is woken at once); the
        // friend list, a call to the mailbox, is read less often.
        let mut read_at: Option<Instant> = None;
        self._mailbox_publish = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(PUBLISH_TICK).await;
            let read = read_at.is_none_or(|t| t.elapsed() >= FRIENDS_EVERY);
            let got = this.update(cx, |s, cx| {
                // Follows waiting on a game, a task or a retry's delay.
                s.follow_tick(cx);
                // Shared mod settings changed here, or waiting to go in.
                s.config_tick(cx);
                let mb = s.mailbox.clone()?;
                if !read {
                    return Some((mb, None));
                }
                s.reconcile_shares();
                s.sync_copies();
                s.retry_unreadable();
                let due = s.groups_due || s.groups_at.is_none_or(|t| t.elapsed() >= GROUPS_EVERY);
                if due {
                    s.groups_due = false;
                    s.groups_at = Some(Instant::now());
                }
                Some((mb, Some(due)))
            });
            let Ok(Some((mb, due))) = got else { continue };
            let work = match due {
                // Between reads: the friends known from the last one, while
                // the mailbox answers (the feed says when it doesn't).
                None => this.update(cx, |s, _| if s.mailbox_error.is_none() { s.publish_work() } else { None }),
                Some(due) => {
                    read_at = Some(Instant::now());
                    let (friends, groups) = {
                        let mb = mb.clone();
                        cx.background_spawn(async move { (mb.friend_list(), due.then(|| mb.groups())) }).await
                    };
                    // The feed reports the mailbox being down.
                    let Ok((friends, max)) = friends else { continue };
                    this.update(cx, |s, cx| {
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
                    })
                }
            };
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
        for i in self.instances.iter().filter(|i| !i.id.is_empty()) {
            if self.instance_homes.get(&i.id) != Some(&i.root) {
                self.instance_homes.insert(i.id.clone(), i.root.clone());
            }
        }
        // A pack that only looked deleted (still on disk) is looked at
        // again after a while, not on every tick; a share turned off goes
        // at once.
        let held = self.stale_held_at.is_some_and(|t| t.elapsed() < RETRY_AFTER);
        let stale_due = stale.iter().any(|(_, why)| *why != "deleted") || (!stale.is_empty() && !held);
        let sig = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            self.instances_sig().hash(&mut h);
            for p in &plan {
                (&p.inst, &p.to, &p.groups).hash(&mut h);
            }
            for c in &self.settings.group_copies {
                (&c.inst, &c.local, &c.group).hash(&mut h);
                // A shared setting changed or a mod's sharing turned; not
                // the numbers a publish stamps on them afterwards.
                let mut cfg = c.configs.clone();
                cfg.relay_reset();
                serde_json::to_string(&cfg).unwrap_or_default().hash(&mut h);
            }
            h.finish()
        };
        if sig == self.mailbox_sig && !stale_due && !retry {
            return None;
        }
        let copies = &self.settings.group_copies;
        let mut packs: Vec<(hopper::instances::Instance, PublishTo, Option<CopyOut>)> = plan
            .into_iter()
            .filter_map(|p| {
                let inst = self.instances.iter().find(|i| i.id == p.inst)?.clone();
                // The sharer's own copy goes out with the share.
                let copy = copies
                    .iter()
                    .find(|c| c.local == p.inst && c.inst == p.inst && p.groups.contains(&c.group))
                    .map(|c| CopyOut { copy: c.clone(), member: false });
                Some((inst, p, copy))
            })
            .collect();
        // A member's copy goes to the group under the pack's own id.
        for c in copies.iter().filter(|c| c.local != c.inst) {
            let Some(inst) = self.instances.iter().find(|i| i.id == c.local) else { continue };
            let to = PublishTo { inst: c.local.clone(), to: Vec::new(), groups: vec![c.group.clone()] };
            packs.push((inst.clone(), to, Some(CopyOut { copy: c.clone(), member: true })));
        }
        // Where to look for each pack that seems deleted: where it was last
        // seen, else the instances folder.
        let homes = stale
            .iter()
            .filter(|(_, why)| *why == "deleted")
            .map(|(s, _)| {
                let at = self.instance_homes.get(&s.inst).cloned();
                (s.inst.clone(), at)
            })
            .collect();
        Some(PublishWork {
            sig,
            stale,
            packs,
            homes,
            instances_root: PathBuf::from(&self.settings.instances_root),
            me: self.mailbox_id(),
        })
    }

    fn publish_done(&mut self, done: PublishDone, cx: &mut Context<Self>) {
        self.mailbox_sig = done.sig;
        if done.failed {
            self.publish_failed_at = Some(Instant::now());
        }
        if done.held {
            self.stale_held_at = Some(Instant::now());
        }
        for s in &done.copies {
            self.configs_sent(&s.local, s.configs.clone(), s.too_big, s.seq);
        }
        self.update_settings_quietly(|st| {
            for (s, deleted) in &done.revoked {
                st.forget_sent(s, *deleted);
            }
            for p in &done.sent {
                st.note_sent(p);
            }
            for s in &done.copies {
                let Some(c) = st.group_copies.iter_mut().find(|c| c.local == s.local) else { continue };
                c.sent = s.code.clone();
                c.sent_seq = s.seq;
                for (k, m) in &s.marks {
                    // Another member's change taken in meanwhile keeps
                    // their name.
                    if c.marks.get(k).is_none_or(|x| x.seq <= m.seq && x.state != m.state) {
                        c.marks.insert(k.clone(), m.clone());
                    }
                }
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
            i.only_here.keys().for_each(|k| k.hash(&mut h));
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

/// A group copy going out with a publish: the copy as it was, and whether
/// it's a member's copy (sent under the pack's id) or the sharer's own.
struct CopyOut {
    copy: PackCopy,
    member: bool,
}

/// A group copy that went out: its code, number and own changes' marks.
struct CopySent {
    local: String,
    code: String,
    seq: u64,
    marks: Vec<(String, groupsync::Mark)>,
    /// The shared settings it carried (None: none, or too many to send).
    configs: Option<hopper::configsync::Configs>,
    too_big: bool,
}

/// One publish: shares to take back and packs to send.
struct PublishWork {
    sig: u64,
    stale: Vec<(SharedPack, &'static str)>,
    packs: Vec<(hopper::instances::Instance, PublishTo, Option<CopyOut>)>,
    /// For each pack that seems deleted (by id), the folder it was last
    /// seen in.
    homes: HashMap<String, Option<PathBuf>>,
    instances_root: PathBuf,
    /// This PC's mailbox id, for its own changes' marks.
    me: String,
}

struct PublishDone {
    sig: u64,
    /// Taken back, and whether because the pack was deleted.
    revoked: Vec<(SharedPack, bool)>,
    /// What reached the relay (groups it skipped left out).
    sent: Vec<PublishTo>,
    copies: Vec<CopySent>,
    failed: bool,
    /// A pack missing from the list was still on disk, so it wasn't taken
    /// back.
    held: bool,
}

impl PublishWork {
    /// Runs on a background thread.
    fn run(self, mb: &Mailbox) -> PublishDone {
        let mut out = PublishDone {
            sig: self.sig,
            revoked: Vec::new(),
            sent: Vec::new(),
            copies: Vec::new(),
            failed: false,
            held: false,
        };
        for (s, reason) in self.stale {
            // Missing from the list isn't deleted: a pack whose file was
            // open in another program, or half written by the CurseForge
            // app, fails to load once. Taking it back then would end every
            // friend's follow and forget the share for good.
            if reason == "deleted" {
                let still = match self.homes.get(&s.inst).cloned().flatten() {
                    Some(root) => hopper::instances::may_hold_id(&root, &s.inst),
                    None => hopper::instances::folder_may_hold_id(&self.instances_root, &s.inst),
                };
                if still {
                    log::info!("shared pack {} didn't load but is still on disk; not taking it back", s.inst);
                    out.held = true;
                    continue;
                }
            }
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
        for (inst, mut plan, copy) in self.packs {
            let member = copy.as_ref().is_some_and(|c| c.member);
            let (relay_inst, key) = match &copy {
                Some(c) if c.member => (c.copy.inst.clone(), groupsync::COPY_KEY.to_string()),
                _ => (inst.id.clone(), inst.root.file_name().unwrap_or_default().to_string_lossy().into_owned()),
            };
            let j = jobs.start_quiet(format!("Sharing {}", inst.name));
            let made = hopper::sharecode::make(&inst, &j);
            j.finish(&Ok::<(), anyhow::Error>(()));
            // Making the code can need CurseForge or Modrinth (the pack's
            // file list); if that failed, the change is sent on the retry
            // instead of never.
            let made = match made {
                Ok(m) => m,
                Err(e) => {
                    log::warn!("making the share code of {}: {e:#}", inst.name);
                    out.failed = true;
                    continue;
                }
            };
            // A group copy says when each mod got the way it is, so a change
            // it only passes on keeps its own date (see `groupsync::Stamp`).
            let marks = copy.as_ref().map(|c| groupsync::stamps(&inst, &c.copy, &made.share)).unwrap_or_default();
            // The group's shared mod settings ride along whole, so a member
            // who was away catches up from this one recipe.
            let (configs, carried, too_big) = copy.as_ref().map(|c| super::config_sync::payload(&c.copy.configs)).unwrap_or_default();
            match mb.publish_with(&plan.to, &plan.groups, &relay_inst, &key, &inst.name, &made.code, &made.left_out, &marks, &configs) {
                Ok(sent) => {
                    if let Some(c) = &copy
                        && !sent.skipped.contains(&c.copy.group)
                    {
                        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                        let marks = groupsync::sent_marks(&inst, &c.copy, &made.share, &self.me, sent.seq, at);
                        out.copies.push(CopySent { local: c.copy.local.clone(), code: made.code.clone(), seq: sent.seq, marks, configs: carried, too_big });
                    }
                    plan.groups.retain(|g| !sent.skipped.contains(g));
                    // A member's copy isn't a share of this PC's to take
                    // back later (see `sync_copies`).
                    if !member {
                        out.sent.push(plan);
                    }
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

/// A hidden pack's key: who, which pack, and which change of it.
fn hidden_key(e: &Event) -> String {
    format!("{}/{}/{}", e.from, e.inst, e.at)
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
    use super::{drop_ways, minutes, newest_codes_only};
    use hopper::mailbox::Event;

    #[test]
    fn code_lifetimes_read_as_whole_minutes() {
        assert_eq!(minutes(900), "15 minutes");
        assert_eq!(minutes(841), "15 minutes");
        assert_eq!(minutes(60), "1 minute");
        assert_eq!(minutes(40), "1 minute");
        assert_eq!(minutes(0), "1 minute");
    }

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
