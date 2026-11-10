//! The mailbox on the internet: pairing and recipe delivery for PCs that
//! can't reach each other directly.
//!
//! Only share codes travel here. Files still come from CurseForge, Modrinth,
//! GitHub and the other sources on each PC. The account is a random id and
//! bearer token kept in Hopper's data folder; the server holds no files.
//!
//! The protocol is in `design/friends-sync.md`. Version 2 added taking
//! shares back (revoke) and groups; against an older mailbox those calls
//! fall back or say the mailbox needs an update, so a new Hopper keeps
//! working before the server is updated.

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

/// Where the public mailbox is. Change for a self-hosted one.
pub const DEFAULT_URL: &str = "https://sync.kyuubi.dev";

/// How long a one-time invite lasts when the mailbox doesn't say.
pub const INVITE_SECS: u64 = 15 * 60;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Account {
    pub id: String,
    pub token: String,
    pub name: String,
    /// Minecraft UUID (no dashes) when signed in.
    pub mc_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Friend {
    pub id: String,
    pub name: String,
    /// Minecraft UUID (no dashes) when they are signed in.
    pub mc_id: String,
    pub online: bool,
    /// Unix seconds the mailbox last heard from them; 0 from an old one.
    pub last_seen: u64,
}

/// What an [`Event`] is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A pack's latest share code.
    Recipe,
    /// The sender stopped sharing a pack with this PC (unshared, deleted,
    /// or the group it came through is gone for this PC).
    Revoke,
    /// A group this PC is in (or invited to) changed; fetch the groups.
    Group,
    /// A group member followed a group pack; follow it too when keeping
    /// that group in step.
    GroupFollow,
    /// Someone joined with an invite that named one of this PC's packs.
    Joined,
    /// A friend removed this PC.
    Unfriended,
    /// From a newer mailbox; ignored.
    Other,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Event {
    pub seq: u64,
    /// Empty from an old mailbox, which only sent recipes.
    pub kind: String,
    pub from: String,
    pub from_name: String,
    /// The sender's instance id (or folder name from an old Hopper).
    pub inst: String,
    /// The sender's folder name, so a follow made before instances had
    /// ids can be matched once.
    pub key: String,
    pub name: String,
    pub code: String,
    pub left_out: Vec<String>,
    pub at: u64,
    /// Shared with this PC directly (as well as through `groups`).
    pub direct: bool,
    /// The groups this pack reached this PC through.
    pub groups: Vec<String>,
    /// Revoke: "unshared", "deleted" or "left_group".
    pub reason: String,
    /// Group / group_follow: the group's id.
    pub group: String,
    /// Group_follow: who shares the pack (`from` is the member who
    /// followed it).
    pub pack_from: String,
}

impl Default for Event {
    fn default() -> Self {
        Self {
            seq: 0,
            kind: String::new(),
            from: String::new(),
            from_name: String::new(),
            inst: String::new(),
            key: String::new(),
            name: String::new(),
            code: String::new(),
            left_out: Vec::new(),
            at: 0,
            // An old mailbox only delivered direct shares.
            direct: true,
            groups: Vec::new(),
            reason: String::new(),
            group: String::new(),
            pack_from: String::new(),
        }
    }
}

impl Event {
    pub fn kind(&self) -> Kind {
        match self.kind.as_str() {
            "" | "recipe" => Kind::Recipe,
            "revoke" => Kind::Revoke,
            "group" => Kind::Group,
            "group_follow" => Kind::GroupFollow,
            "joined" => Kind::Joined,
            "unfriended" => Kind::Unfriended,
            _ => Kind::Other,
        }
    }
}

/// A group as this PC sees it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Group {
    pub id: String,
    pub name: String,
    /// The friend who made it; only they can rename it or change members.
    pub owner: String,
    pub me: GroupMe,
    pub members: Vec<GroupMember>,
    /// Packs shared with the group. Empty until this PC says yes.
    pub packs: Vec<GroupPack>,
    /// Group packs some member follows.
    pub follows: Vec<GroupFollow>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupMe {
    /// Said yes on this PC. Until then the group is only an invitation.
    pub joined: bool,
    /// Follows what the other members follow.
    pub in_step: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupMember {
    pub id: String,
    pub name: String,
    pub mc_id: String,
    pub online: bool,
    pub last_seen: u64,
    pub joined: bool,
    pub in_step: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupPack {
    pub from: String,
    pub inst: String,
    pub name: String,
    pub at: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupFollow {
    pub from: String,
    pub inst: String,
}

/// What a publish reached.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sent {
    pub seq: u64,
    /// People it went to (each once).
    pub sent: usize,
    /// Groups it didn't go to: left or removed since this PC last looked,
    /// or an old mailbox without groups.
    pub skipped: Vec<String>,
}

/// Why a mailbox call failed, for the window to say it plainly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// No answer: this PC is offline, or the mailbox (or its tunnel) is down.
    Unreachable,
    /// This PC's friend list is full.
    FriendsFull,
    /// The other PC's friend list is full.
    TheirFriendsFull,
    /// The invite ran out, was used, or never existed.
    InviteUnknown,
    /// The mailbox is older than this Hopper (no groups yet).
    NeedsUpdate,
    /// Anything else, with the mailbox's message.
    Other(String),
}

/// The error mailbox calls return (inside the `anyhow::Error`).
#[derive(Clone, Debug)]
pub struct MailboxError {
    /// HTTP status; 0 when the request never got an answer.
    pub status: u16,
    /// The mailbox's error code (`friends_full`...), when it gave one.
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for MailboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for MailboxError {}

/// Sorts a mailbox error into what the window should say.
pub fn failure(e: &anyhow::Error) -> Failure {
    let Some(m) = e.downcast_ref::<MailboxError>() else {
        return Failure::Other(format!("{e:#}"));
    };
    match m.code.as_str() {
        "friends_full" => return Failure::FriendsFull,
        "their_friends_full" => return Failure::TheirFriendsFull,
        "invite_unknown" => return Failure::InviteUnknown,
        "needs_update" => return Failure::NeedsUpdate,
        _ => {}
    }
    // 0: no answer. 502-504 and Cloudflare's 52x: the tunnel is up but the
    // mailbox behind it isn't.
    if m.status == 0 || ((501..600).contains(&m.status)) {
        return Failure::Unreachable;
    }
    Failure::Other(m.message.clone())
}

/// Whether the mailbox no longer knows this PC's token (its data was reset).
pub fn unauthorized(e: &anyhow::Error) -> bool {
    e.downcast_ref::<MailboxError>().is_some_and(|m| m.status == 401)
}

/// One plain sentence for a failed mailbox call.
pub fn says(e: &anyhow::Error) -> String {
    match failure(e) {
        Failure::Unreachable => "Can't reach the sync service. Hopper keeps trying.".into(),
        Failure::FriendsFull => "Friend list full. Remove a friend to add another.".into(),
        Failure::TheirFriendsFull => "Their friend list is full.".into(),
        Failure::InviteUnknown => {
            "That code ran out or was already used. Ask them for a new one.".into()
        }
        Failure::NeedsUpdate => "The sync service needs an update for groups.".into(),
        Failure::Other(s) => s,
    }
}

pub struct Mailbox {
    base: String,
    account: Account,
    agent: ureq::Agent,
    /// The mailbox's protocol version, asked once.
    proto: OnceLock<u64>,
    /// This PC's saved account was unknown to the mailbox, so it signed up
    /// again (the service lost its data).
    reset: bool,
}

impl Mailbox {
    fn new(base: &str, account: Account) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(90)))
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            base: base.trim_end_matches('/').to_string(),
            account,
            agent,
            proto: OnceLock::new(),
            reset: false,
        }
    }

    /// True when the mailbox had forgotten this PC and a new account was
    /// made: friends and groups are gone, local settings are not.
    pub fn was_reset(&self) -> bool {
        self.reset
    }

    /// Loads this PC's account, registering it the first time. `name` and
    /// `mc_id` are the signed-in Minecraft profile when there is one.
    pub fn load(base: &str, name: &str, mc_id: &str) -> anyhow::Result<Self> {
        Self::load_at(&account_path(), base, name, mc_id)
    }

    fn load_at(path: &Path, base: &str, name: &str, mc_id: &str) -> anyhow::Result<Self> {
        let mut account: Account = std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let mut me = Self::new(base, account.clone());
        if !account.id.is_empty() && !account.token.is_empty() {
            // A mailbox that lost its data answers 401 to a token it never
            // issued. Sign up again, once, instead of failing for ever. Any
            // other failure (offline, the tunnel down) is not that, and the
            // saved account is kept.
            if let Err(e) = me.get("/v1/friends", Some(&account.token)) {
                if unauthorized(&e) {
                    account = Account::default();
                    me.account = Account::default();
                    me.reset = true;
                }
            }
        }
        if account.id.is_empty() || account.token.is_empty() {
            let v = me.post(
                "/v1/register",
                None,
                serde_json::json!({ "name": name, "mc_id": mc_id }),
            )?;
            account = Account {
                id: v["id"].as_str().context("mailbox: no id")?.into(),
                token: v["token"].as_str().context("mailbox: no token")?.into(),
                name: v["name"].as_str().unwrap_or(name).into(),
                mc_id: mc_id.into(),
            };
            me.account = account.clone();
            if let Some(d) = path.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            std::fs::write(path, serde_json::to_vec_pretty(&account)?)
                .with_context(|| format!("saving {}", path.display()))?;
            return Ok(me);
        }
        if account.name != name || account.mc_id != mc_id {
            me.post(
                "/v1/profile",
                Some(&account.token),
                serde_json::json!({ "name": name, "mc_id": mc_id }),
            )?;
            account.name = name.to_string();
            account.mc_id = mc_id.to_string();
            me.account = account.clone();
            let _ = std::fs::write(path, serde_json::to_vec_pretty(&account)?);
        }
        Ok(me)
    }

    /// Tells the mailbox this PC's Minecraft name and UUID after a sign-in.
    pub fn update_profile(&self, name: &str, mc_id: &str) -> anyhow::Result<()> {
        self.call("/v1/profile", serde_json::json!({ "name": name, "mc_id": mc_id }))?;
        Ok(())
    }

    /// A fresh account, not saved. Tests use two of these to pair.
    pub fn register(base: &str, name: &str) -> anyhow::Result<Self> {
        let mut me = Self::new(base, Account::default());
        let v = me.post("/v1/register", None, serde_json::json!({ "name": name }))?;
        me.account = Account {
            id: v["id"].as_str().context("mailbox: no id")?.into(),
            token: v["token"].as_str().context("mailbox: no token")?.into(),
            name: v["name"].as_str().unwrap_or(name).into(),
            mc_id: String::new(),
        };
        Ok(me)
    }

    /// The saved account, for showing this PC's name.
    pub fn account(&self) -> &Account {
        &self.account
    }

    /// Whether the mailbox knows groups and taking shares back. Asked once.
    pub fn has_groups(&self) -> anyhow::Result<bool> {
        Ok(self.proto()? >= 2)
    }

    fn proto(&self) -> anyhow::Result<u64> {
        if let Some(v) = self.proto.get() {
            return Ok(*v);
        }
        let v = self.get("/v1/health", None)?;
        let p = v["v"].as_u64().unwrap_or(1);
        Ok(*self.proto.get_or_init(|| p))
    }

    fn need_groups(&self) -> anyhow::Result<()> {
        if self.has_groups()? {
            return Ok(());
        }
        Err(MailboxError {
            status: 400,
            code: "needs_update".into(),
            message: "The sync service needs an update for groups.".into(),
        }
        .into())
    }

    /// Makes a one-time invite code and says how many seconds it lasts,
    /// so the window can count it down instead of promising "15 minutes".
    pub fn invite(&self) -> anyhow::Result<(String, u64)> {
        self.invite_for("")
    }

    /// An invite that also names one of this PC's packs (by instance id):
    /// the follow part of a share code. Whoever joins with it follows that
    /// pack, and this PC hears who joined (a `joined` event).
    pub fn invite_for(&self, inst: &str) -> anyhow::Result<(String, u64)> {
        let v = self.call("/v1/invite", serde_json::json!({ "inst": inst }))?;
        let code = v["code"].as_str().context("mailbox: no code")?.to_string();
        let secs = v["expires"].as_u64().unwrap_or(INVITE_SECS);
        Ok((code, secs))
    }

    /// Joins with an invite code. Returns the new friend and the pack the
    /// invite named (empty for a plain invite or an old mailbox).
    pub fn join(&self, code: &str) -> anyhow::Result<(Friend, String)> {
        let v = self.call(
            "/v1/join",
            serde_json::json!({ "code": code.trim().to_uppercase() }),
        )?;
        let friend = serde_json::from_value(v["friend"].clone()).unwrap_or_default();
        Ok((friend, v["inst"].as_str().unwrap_or("").to_string()))
    }

    pub fn friends(&self) -> anyhow::Result<Vec<Friend>> {
        Ok(self.friend_list()?.0)
    }

    /// The friends and how many the mailbox allows (0 when it doesn't say).
    pub fn friend_list(&self) -> anyhow::Result<(Vec<Friend>, usize)> {
        let v = self.get("/v1/friends", Some(&self.account.token))?;
        Ok((
            serde_json::from_value(v["friends"].clone()).unwrap_or_default(),
            v["max"].as_u64().unwrap_or(0) as usize,
        ))
    }

    pub fn remove(&self, id: &str) -> anyhow::Result<()> {
        self.call("/v1/remove", serde_json::json!({ "id": id }))?;
        Ok(())
    }

    /// Sends a pack's share code to friends and groups, once per person.
    /// `inst` is the instance id; `key` its folder name, for friends whose
    /// follow predates ids.
    #[allow(clippy::too_many_arguments)]
    pub fn publish_to(
        &self,
        to: &[String],
        groups: &[String],
        inst: &str,
        key: &str,
        name: &str,
        code: &str,
        left_out: &[String],
    ) -> anyhow::Result<Sent> {
        if !self.has_groups()? {
            // An old mailbox takes one friend per call and knows no groups.
            let mut out = Sent {
                skipped: groups.to_vec(),
                ..Default::default()
            };
            for f in to {
                let v = self.call(
                    "/v1/recipes",
                    serde_json::json!({
                        "to": f, "inst": inst, "name": name, "code": code, "left_out": left_out,
                    }),
                )?;
                out.seq = v["seq"].as_u64().unwrap_or(out.seq);
                out.sent += 1;
            }
            return Ok(out);
        }
        let v = self.call(
            "/v1/recipes",
            serde_json::json!({
                "to": to,
                "groups": groups,
                "inst": inst,
                "key": key,
                "name": name,
                "code": code,
                "left_out": left_out,
            }),
        )?;
        Ok(Sent {
            seq: v["seq"].as_u64().unwrap_or(0),
            sent: v["sent"].as_u64().unwrap_or(0) as usize,
            skipped: serde_json::from_value(v["skipped"].clone()).unwrap_or_default(),
        })
    }

    /// Takes a pack back from friends and groups. `reason` is "unshared" or
    /// "deleted". An old mailbox can't; returns false then so the share is
    /// tried again after the update.
    pub fn revoke(
        &self,
        inst: &str,
        to: &[String],
        groups: &[String],
        reason: &str,
    ) -> anyhow::Result<bool> {
        if !self.has_groups()? {
            return Ok(false);
        }
        self.call(
            "/v1/revoke",
            serde_json::json!({ "inst": inst, "to": to, "groups": groups, "reason": reason }),
        )?;
        Ok(true)
    }

    /// Groups this PC owns, belongs to or is invited to. Empty from an old
    /// mailbox.
    pub fn groups(&self) -> anyhow::Result<Vec<Group>> {
        if !self.has_groups()? {
            return Ok(Vec::new());
        }
        let v = self.get("/v1/groups", Some(&self.account.token))?;
        Ok(serde_json::from_value(v["groups"].clone()).unwrap_or_default())
    }

    /// Makes a group of some of this PC's friends. They are invited; each
    /// says yes on their own PC.
    pub fn create_group(&self, name: &str, members: &[String]) -> anyhow::Result<Group> {
        self.need_groups()?;
        let v = self.call(
            "/v1/groups",
            serde_json::json!({ "name": name, "members": members }),
        )?;
        Ok(serde_json::from_value(v["group"].clone()).unwrap_or_default())
    }

    /// Owner only: renames the group and invites or removes members.
    pub fn edit_group(
        &self,
        id: &str,
        name: Option<&str>,
        add: &[String],
        remove: &[String],
    ) -> anyhow::Result<()> {
        self.need_groups()?;
        let mut body = serde_json::json!({ "add": add, "remove": remove });
        if let Some(n) = name {
            body["name"] = n.into();
        }
        self.call(&format!("/v1/groups/{id}"), body)?;
        Ok(())
    }

    /// Owner only: deletes the group; members lose what came through it.
    pub fn delete_group(&self, id: &str) -> anyhow::Result<()> {
        self.group_post(id, "delete", serde_json::json!({}))
    }

    /// Says yes to a group invitation; its packs arrive as events.
    pub fn accept_group(&self, id: &str) -> anyhow::Result<()> {
        self.group_post(id, "accept", serde_json::json!({}))
    }

    /// Leaves a group (or turns its invitation down).
    pub fn leave_group(&self, id: &str) -> anyhow::Result<()> {
        self.group_post(id, "leave", serde_json::json!({}))
    }

    /// Turns "follow what the others follow" on or off for this PC.
    pub fn set_in_step(&self, id: &str, on: bool) -> anyhow::Result<()> {
        self.group_post(id, "in-step", serde_json::json!({ "on": on }))
    }

    /// Tells the group's in-step members that this PC followed `from`'s
    /// pack `inst`, which the group shares.
    pub fn group_follow(&self, id: &str, from: &str, inst: &str) -> anyhow::Result<()> {
        self.group_post(id, "follow", serde_json::json!({ "from": from, "inst": inst }))
    }

    fn group_post(&self, id: &str, what: &str, body: serde_json::Value) -> anyhow::Result<()> {
        self.need_groups()?;
        self.call(&format!("/v1/groups/{id}/{what}"), body)?;
        Ok(())
    }

    /// Events for this PC after `after`, waiting up to `wait` seconds for
    /// one. Returns the newest sequence seen.
    pub fn events(&self, after: u64, wait: u64) -> anyhow::Result<(u64, Vec<Event>)> {
        let v = self.get(
            &format!("/v1/recipes?after={after}&wait={wait}&v=2"),
            Some(&self.account.token),
        )?;
        Ok((
            v["seq"].as_u64().unwrap_or(after),
            serde_json::from_value(v["events"].clone()).unwrap_or_default(),
        ))
    }

    fn call(&self, path: &str, body: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        self.post(path, Some(&self.account.token), body)
    }

    fn get(&self, path: &str, token: Option<&str>) -> anyhow::Result<serde_json::Value> {
        let mut r = self.agent.get(format!("{}{}", self.base, path));
        if let Some(t) = token {
            r = r.header("Authorization", format!("Bearer {t}"));
        }
        let resp = r.call().map_err(|e| unreachable(path, e))?;
        answer(path, resp)
    }

    fn post(
        &self,
        path: &str,
        token: Option<&str>,
        body: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let mut r = self.agent.post(format!("{}{}", self.base, path));
        if let Some(t) = token {
            r = r.header("Authorization", format!("Bearer {t}"));
        }
        let resp = r.send_json(body).map_err(|e| unreachable(path, e))?;
        answer(path, resp)
    }
}

fn unreachable(path: &str, e: ureq::Error) -> anyhow::Error {
    MailboxError {
        status: 0,
        code: String::new(),
        message: format!("mailbox: {path}: {e}"),
    }
    .into()
}

/// The JSON body, or a [`MailboxError`] with the mailbox's code.
fn answer(
    path: &str,
    mut resp: ureq::http::Response<ureq::Body>,
) -> anyhow::Result<serde_json::Value> {
    let status = resp.status().as_u16();
    let v: serde_json::Value = match resp.body_mut().read_json() {
        Ok(v) => v,
        // A tunnel error page, not the mailbox.
        Err(e) => {
            return Err(MailboxError {
                status: if status < 400 { 502 } else { status },
                code: String::new(),
                message: format!("mailbox: {path}: {e}"),
            }
            .into());
        }
    };
    if status >= 400 || v.get("error").is_some() {
        return Err(MailboxError {
            status,
            code: v["code"].as_str().unwrap_or("").to_string(),
            message: v["error"].as_str().unwrap_or("mailbox error").to_string(),
        }
        .into());
    }
    Ok(v)
}

fn account_path() -> PathBuf {
    crate::paths::dirs()
        .map(|d| d.data_dir().join("mailbox.json"))
        .unwrap_or_else(|| std::env::temp_dir().join("hopper-mailbox.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_account_round_trips() {
        let a = Account {
            id: "a".repeat(32),
            token: "t".repeat(43),
            name: "FOX".into(),
            mc_id: "069a79f444e94726a5befca90e38aaf5".into(),
        };
        let b: Account = serde_json::from_slice(&serde_json::to_vec(&a).unwrap()).unwrap();
        assert_eq!((b.id, b.token, b.name, b.mc_id), (a.id, a.token, a.name, a.mc_id));
    }

    #[test]
    fn events_keep_their_shape() {
        // From a mailbox before kinds: a direct recipe.
        let e: Event = serde_json::from_str(
            r#"{"seq":3,"from":"aa","from_name":"FOX","inst":"TNP","name":"ATM","code":"HOP-x","left_out":["a.jar"],"at":1}"#,
        )
        .unwrap();
        assert_eq!(e.from_name, "FOX");
        assert_eq!(e.left_out, ["a.jar"]);
        assert_eq!(e.kind(), Kind::Recipe);
        assert!(e.direct);

        let e: Event = serde_json::from_str(
            r#"{"seq":4,"kind":"revoke","from":"aa","inst":"x1","reason":"left_group","direct":false,"groups":["g1"]}"#,
        )
        .unwrap();
        assert_eq!((e.kind(), e.reason.as_str(), e.direct), (Kind::Revoke, "left_group", false));
        assert_eq!(e.groups, ["g1"]);
        let e: Event = serde_json::from_str(r#"{"kind":"something_new"}"#).unwrap();
        assert_eq!(e.kind(), Kind::Other);
    }

    /// A mailbox that forgot every account: 401 for the old token, and a
    /// new account for a sign-up. Answers one request per connection.
    fn forgetful_mailbox() -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { return };
                let mut buf = vec![0u8; 8192];
                let n = c.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let line = head.lines().next().unwrap_or("").to_string();
                let (status, body) = if line.starts_with("GET /v1/friends") {
                    ("401 Unauthorized", r#"{"error":"register first","code":"unauthorized"}"#)
                } else if line.starts_with("POST /v1/register") {
                    ("200 OK", r#"{"id":"newid","token":"newtoken","name":"FOX"}"#)
                } else {
                    ("404 Not Found", r#"{"error":"no"}"#)
                };
                let _ = tx.send(line);
                let _ = write!(
                    c,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (url, rx)
    }

    #[test]
    fn a_forgotten_account_signs_up_again_once() {
        let dir = std::env::temp_dir().join(format!("hopper-mailbox-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mailbox.json");
        let old = Account {
            id: "oldid".into(),
            token: "oldtoken".into(),
            name: "FOX".into(),
            mc_id: String::new(),
        };
        std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let (url, seen) = forgetful_mailbox();
        let mb = Mailbox::load_at(&path, &url, "FOX", "").unwrap();
        assert!(mb.was_reset());
        assert_eq!((mb.account().id.as_str(), mb.account().token.as_str()), ("newid", "newtoken"));
        // The new account is what is saved.
        let saved: Account = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.id, "newid");
        let lines: Vec<String> = seen.try_iter().collect();
        assert_eq!(lines.iter().filter(|l| l.starts_with("POST /v1/register")).count(), 1);
        // A second start finds a good account (this fake still says 401 to
        // everything, so only check that a fresh file wasn't needed first).
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreachable_mailbox_keeps_the_saved_account() {
        let dir = std::env::temp_dir().join(format!("hopper-mailbox-test-off-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mailbox.json");
        let old = Account {
            id: "oldid".into(),
            token: "oldtoken".into(),
            name: "FOX".into(),
            mc_id: String::new(),
        };
        std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        // Nothing listens here: the name is unchanged, so no call is needed.
        let mb = Mailbox::load_at(&path, "http://127.0.0.1:9", "FOX", "").unwrap();
        assert!(!mb.was_reset());
        assert_eq!(mb.account().id, "oldid");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failures_are_sorted_for_the_window() {
        let err = |status: u16, code: &str| -> anyhow::Error {
            MailboxError {
                status,
                code: code.into(),
                message: "nope".into(),
            }
            .into()
        };
        assert_eq!(failure(&err(409, "friends_full")), Failure::FriendsFull);
        assert_eq!(failure(&err(409, "their_friends_full")), Failure::TheirFriendsFull);
        assert_eq!(failure(&err(404, "invite_unknown")), Failure::InviteUnknown);
        assert_eq!(failure(&err(0, "")), Failure::Unreachable);
        assert_eq!(failure(&err(502, "")), Failure::Unreachable);
        assert_eq!(failure(&err(530, "")), Failure::Unreachable);
        assert_eq!(failure(&err(403, "")), Failure::Other("nope".into()));
        // Context on top doesn't hide it.
        let wrapped = err(409, "friends_full").context("adding a friend");
        assert_eq!(failure(&wrapped), Failure::FriendsFull);
        assert_eq!(says(&wrapped), "Friend list full. Remove a friend to add another.");
        assert_eq!(failure(&anyhow::anyhow!("disk full")), Failure::Other("disk full".into()));
    }

    #[test]
    fn groups_read_what_the_mailbox_sends() {
        let g: Group = serde_json::from_str(
            r#"{"id":"g1","name":"Crew","owner":"aa","me":{"joined":true,"in_step":false},
                "members":[{"id":"aa","name":"FOX","online":true,"last_seen":5,"joined":true,"in_step":true}],
                "packs":[{"from":"aa","inst":"x1","name":"ATM","at":9}],
                "follows":[{"from":"aa","inst":"x1"}]}"#,
        )
        .unwrap();
        assert!(g.me.joined && !g.me.in_step);
        assert_eq!(g.members[0].last_seen, 5);
        assert_eq!(g.packs[0].inst, "x1");
        assert_eq!(g.follows[0], GroupFollow { from: "aa".into(), inst: "x1".into() });
    }

    fn live() -> String {
        std::env::var("HOPPER_MAILBOX_URL").expect("HOPPER_MAILBOX_URL")
    }

    /// The same calls the Friends buttons make. Needs a running mailbox,
    /// a local one for anything new (`relay/README.md`):
    /// `HOPPER_MAILBOX_URL=http://127.0.0.1:8787 cargo test -p hopper-core
    /// --lib mailbox::tests -- --ignored`.
    #[test]
    #[ignore = "needs a live mailbox (HOPPER_MAILBOX_URL)"]
    fn two_accounts_pair_and_publish() {
        let base = live();
        let a = Mailbox::register(&base, "hopper-test-a").unwrap();
        let b = Mailbox::register(&base, "hopper-test-b").unwrap();
        let (code, _) = a.invite_for("pack-1").unwrap();
        let (friend, inst) = b.join(&code).unwrap();
        assert_eq!(friend.name, "hopper-test-a");
        assert_eq!(inst, "pack-1");
        let mc_id = "069a79f444e94726a5befca90e38aaf5";
        a.update_profile("Notch", mc_id).unwrap();
        let f = b
            .friends()
            .unwrap()
            .into_iter()
            .find(|f| f.id == a.account().id)
            .unwrap();
        assert_eq!((f.name.as_str(), f.mc_id.as_str()), ("Notch", mc_id));
        // Single use.
        assert_eq!(failure(&b.join(&code).unwrap_err()), Failure::InviteUnknown);
        let to = [b.account().id.clone()];
        let sent = a.publish_to(&to, &[], "pack-1", "TNP", "ATM", "HOP-test", &[]).unwrap();
        assert_eq!(sent.sent, 1);
        assert!(a.revoke("pack-1", &to, &[], "unshared").unwrap());
        let (_, events) = b.events(0, 5).unwrap();
        let kinds: Vec<_> = events
            .iter()
            .filter(|e| e.from == a.account().id)
            .map(|e| (e.kind(), e.inst.as_str()))
            .collect();
        assert_eq!(kinds, [(Kind::Recipe, "pack-1"), (Kind::Revoke, "pack-1")]);
        let (_, events) = a.events(0, 0).unwrap();
        assert!(events.iter().any(|e| e.kind() == Kind::Joined && e.from == b.account().id));
    }

    #[test]
    #[ignore = "needs a live mailbox (HOPPER_MAILBOX_URL)"]
    fn a_group_keeps_its_members_in_step() {
        let base = live();
        let a = Mailbox::register(&base, "hopper-test-a").unwrap();
        let b = Mailbox::register(&base, "hopper-test-b").unwrap();
        let c = Mailbox::register(&base, "hopper-test-c").unwrap();
        for m in [&b, &c] {
            m.join(&a.invite().unwrap().0).unwrap();
        }
        let g = a
            .create_group("Crew", &[b.account().id.clone(), c.account().id.clone()])
            .unwrap();
        for m in [&b, &c] {
            m.accept_group(&g.id).unwrap();
        }
        c.set_in_step(&g.id, true).unwrap();
        let sent = a
            .publish_to(&[b.account().id.clone()], &[g.id.clone()], "pack-1", "TNP", "ATM", "HOP-x", &[])
            .unwrap();
        assert_eq!(sent.sent, 2);
        let (after, events) = b.events(0, 0).unwrap();
        let r = events.iter().find(|e| e.kind() == Kind::Recipe).unwrap();
        assert!(r.direct);
        assert_eq!(r.groups, [g.id.clone()]);
        b.group_follow(&g.id, &a.account().id, "pack-1").unwrap();
        let (_, events) = c.events(0, 0).unwrap();
        assert!(events.iter().any(|e| e.kind() == Kind::GroupFollow
            && e.pack_from == a.account().id
            && e.group == g.id));
        b.leave_group(&g.id).unwrap();
        assert!(b.groups().unwrap().is_empty());
        // Leaving takes away only the group's way in; the pack still
        // reaches B directly, which the desktop keeps.
        let (_, events) = b.events(after, 0).unwrap();
        let revoke = events.iter().find(|e| e.kind() == Kind::Revoke).unwrap();
        assert_eq!((revoke.direct, revoke.reason.as_str()), (false, "left_group"));
    }
}
