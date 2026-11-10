# Friends & sync

How Hopper desktops share packs with friends through the mailbox
(`relay/hopper_mailbox.py`, published as `https://sync.kyuubi.dev`), and how
the desktop keeps followed packs in step. Only share codes travel; every
file still downloads from CurseForge, Modrinth, GitHub or its link on each
PC.

Code: relay in `relay/`, client in `crates/hopper-core/src/mailbox.rs`,
settings in `crates/hopper-core/src/settings.rs` (`SharedPack`,
`FollowedRecipe`, `FriendGroup`), desktop in `src/gpui/store/friends.rs`.

## Words

- **Friend**: two PCs paired with a one-time invite code. Friendship is
  mutual; either side can remove it.
- **Share**: one pack (an instance, by its id) shared with one friend or one
  group. Every pack starts private.
- **Follow**: a local instance that keeps matching a friend's pack. Several
  local instances can follow the same pack.
- **Group**: a named set of the owner's friends. Members say yes on their
  own PC before anything reaches them.
- **Keep everyone in step**: a member's opt-in per group. A group pack any
  member follows is followed on this PC too, as a new instance.

## Instance ids

Shares and follows used to name a pack by its folder name, which broke on
rename and could collide. Each instance now has a random `id` in its
`hopper.json`, given the first time it loads (a copy gets a new one; a
CurseForge re-import keeps it). Settings migrate once:

- `shared_instances` (old: every friend) fans out into `shared_with` once the
  friend list is known (`migrate_shared`), keyed by folder name;
- `reconcile_instances` turns folder keys into ids, fills `local` on follows
  and finds a followed instance again after its folder was renamed;
- a follow of a friend's pack by its folder name learns the id from the next
  code, which carries both (`inst` = id, `key` = folder name).

## Protocol (version 2)

All routes except register and health need `Authorization: Bearer <token>`.
Bodies are JSON, at most 64 KB. `GET /v1/health` says `v: 2`; the desktop
asks once and falls back for an older mailbox (one publish per friend, no
revoke, no groups).

### Friends

| Route | Body → answer |
|---|---|
| `POST /v1/invite` | `{inst?}` → `{code, expires}`; one use, 15 minutes |
| `POST /v1/join` | `{code}` → `{friend, inst}` |
| `GET /v1/friends` | `{friends: [{id, name, mc_id, online, last_seen}], max}` |
| `POST /v1/remove` | `{id}` |

`inst` on an invite is the follow part of a share code: the code is
`HOP-…​.<INVITE>` (`sharecode::with_follow`). Pasting it installs the pack,
joins with the invite and follows `inst`. The owner gets a `joined` event and
shares that pack with the new friend, so its changes arrive.

Errors carry a `code`: `invite_unknown` (run out, used, or never made),
`friends_full` (your list is at `max`), `their_friends_full`. The desktop
says "Friend list full" for the first (`mailbox::says`).

Removing a friend also takes each out of the other's groups and queues an
`unfriended` event. A follow of their pack keeps its last version.

### Packs

| Route | Body → answer |
|---|---|
| `POST /v1/recipes` | `{to: [ids], groups: [ids], inst, key, name, code, left_out}` → `{seq, sent, skipped}` |
| `POST /v1/revoke` | `{inst, to: [ids], groups: [ids], reason}` |
| `GET /v1/recipes?after=N&wait=60&v=2` | `{seq, events}`, long-polls up to `wait` seconds |

A publish goes once to each person, whether they get it directly, through
one group or several; the event says which (`direct`, `groups`). `to` must
be friends (else 403). A group the sender isn't a joined member of any more
is skipped and listed in `skipped`; the rest still goes. A pack shared with
a group is stored on the group (latest code), so members who join later get
it. A recipe event replaces older queued recipes of the same pack for that
person: only the newest code waits.

`reason` is `unshared` (the share was turned off) or `deleted` (the pack is
gone on the sender's PC); anything else becomes `unshared`. The desktop
works out what to take back by comparing `mailbox_sent` (what reached the
relay) with what is shared now, so a share turned off while the mailbox was
down is taken back when it's up again.

### Groups

| Route | Who | Body |
|---|---|---|
| `GET /v1/groups` | anyone | → `{groups: [...]}`: owned, joined or invited |
| `POST /v1/groups` | any | `{name, members}` → `{group}`; members must be your friends |
| `POST /v1/groups/<id>` | owner | `{name?, add?, remove?}` |
| `POST /v1/groups/<id>/delete` | owner | |
| `POST /v1/groups/<id>/accept` | invited | gets the group's packs as recipe events |
| `POST /v1/groups/<id>/leave` | member | not the owner (`owner_leaves`); also turns an invitation down |
| `POST /v1/groups/<id>/in-step` | joined | `{on}` |
| `POST /v1/groups/<id>/follow` | joined | `{from, inst}`: a group pack this PC followed |

A group is `{id, name, owner, me: {joined, in_step}, members: [{id, name,
mc_id, online, last_seen, joined, in_step}], packs, follows}`. `packs` and
`follows` are empty until `me.joined`.

Access: anyone not in the group (or invited to it) gets 404 "no such group"
for every route, the same as a group that doesn't exist, so ids can't be
probed. An invited member can only accept or leave. Only joined members can
publish into it, follow through it or switch keeping in step. Only the owner
renames, adds, removes or deletes. Members may not be friends with each
other; that's why nothing reaches a member before they say yes.

Limits (409 with a code): 20 groups owned per PC (`groups_full`), 50 members
per group (`group_full`), 100 packs per group (`group_packs_full`), 20 groups
named per publish. Names are cut at 64 characters.

Membership changes:

- **Accept**: the group's packs (latest codes) arrive as recipe events.
- **Leave or removed**: the member gets a revoke (`left_group`) for each
  group pack; the others get one for each pack the member shared into it.
- **Delete**: like everyone leaving.
- **Follow** (`/follow`): recorded on the group; a `group_follow` event goes
  to each joined member with `in_step` on, except the one who followed and
  the one who shares it.

### Events

Every event has `seq`, `kind`, `from`, `from_name`, `at`. Without `v=2` only
`recipe` events are sent, so an older Hopper never sees the rest.

| kind | fields | the desktop |
|---|---|---|
| `recipe` | `inst, key, name, code, left_out, direct, groups` | lists it; applies it to every local follow |
| `revoke` | `inst, reason, direct, groups` | removes those ways in; once none is left the pack leaves the list and its follows end (`ended` = reason), keeping their last version |
| `group` | `group` | reads the groups again |
| `group_follow` | `group, pack_from, inst` | reads the groups again; keeping in step follows from there |
| `joined` | `inst` | shares `inst` with them (an invite that named a pack) |
| `unfriended` | | drops shares with them; their direct packs leave; follows keep their last version |

The desktop reads the whole feed at each start (to list friends' packs) and
remembers the newest event it acted on (`Settings::mailbox_handled`): older
ones update the lists but show no notice and don't act again. Within one
read, only the newest code of each pack is applied.

## On the desktop

- **Following always asks first.** `store::follow_pack` and `store::redeem`
  run the "mods from other sources" question (`incoming_sources::gate`)
  before anything installs; the store's own follow functions are private.
  A follow nobody can be asked about (a followed pack changing, keeping a
  group in step) leaves out what that question would need
  (`sources::carry::auto_skip`).
- **Keeping in step** installs a new instance; it never replaces one. A pack
  stopped here (or whose instance was deleted in Hopper) is remembered per
  group (`FriendGroup::stopped`) and not followed again.
- **Status**: `Store::sync_status()` is Off, Connecting, Down(text) or Ok.
  Notices (copied, paired, joined, stopped sharing, errors) go out as
  `StoreEvent::Notice`.

## Deploying the relay

Copy `relay/hopper_mailbox.py` to `/opt/hopper-mailbox/` and restart
`hopper-mailbox` (see `relay/README.md`). The state file gains a `groups`
key on its own; nothing to migrate. A new desktop works against the old
mailbox (falls back), and an old desktop against the new one (v1 feed).

Rolling back: reinstall the previous file and restart. Groups are dropped on
its next save; friends and queued events stay. Queued non-recipe events
then reach old and new desktops alike from the old file's feed: an old
desktop ignores fields it doesn't know but would try to follow a revoke as a
recipe with no code (it fails with an error and changes nothing). Clearing
the queues (`events` in the state file) avoids that.
