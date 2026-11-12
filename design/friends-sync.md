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
Bodies are JSON, at most 64 KB (`413`), with exactly one numeric
`Content-Length` and no `Transfer-Encoding` (`400` `bad_framing`); after any
answer that didn't read the body the connection closes. `GET /v1/health`
says `v: 2`; the desktop asks once and falls back for an older mailbox (one
publish per friend, no revoke, no groups).

The mailbox is public, so it limits what a stranger can make it hold or do
(see "Limits" below). The extra answer is `429` with `Retry-After`; the
desktop shows its message like any other error.

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
`friends_full` (your list is at `max`), `their_friends_full`, `rate_limited`
(429: ten wrong codes in 15 minutes from this account or this address, or too
many sign-ups from one address) and `invites_full` (429: five open
invites). The desktop says "Friend list full" for the first
(`mailbox::says`). The invite code stays eight hex characters: the share
code's follow part (`sharecode::follow_part`) only accepts that, so guessing
is held back by the limits instead of a longer code.

Removing a friend also takes each out of the other's groups and queues an
`unfriended` event. A follow of their pack keeps its last version.

### Packs

| Route | Body → answer |
|---|---|
| `POST /v1/recipes` | `{to: [ids], groups: [ids], inst, key, name, code, left_out, marks?}` → `{seq, sent, skipped}` |
| `POST /v1/revoke` | `{inst, to: [ids], groups: [ids], reason}` |
| `GET /v1/recipes?after=N&wait=60&v=2` | `{seq, events}`, long-polls up to `wait` seconds |

A publish goes once to each person, whether they get it directly, through
one group or several; the event says which (`direct`, `groups`). `to` must
be friends (else 403). A group the sender isn't a joined member of any more
is skipped and listed in `skipped`; the rest still goes. A pack shared with
a group is stored on the group (latest code), so members who join later get
it. A recipe event replaces older queued recipes of the same pack for that
person: only the newest code waits.

`marks` (optional, group copies only) dates each mod in the code:
`{mod key: {seq, by}}`, the event number and member of the change that made
it that way. The relay passes it on as it is (cut to 2000 entries) and an
older relay drops it. A member who passes on a change it took late then
can't undo a newer one: its copy's change is dated by the stamp, not by the
number of its own, later event. A member's first code is taken in only for
its stamped mods, since an unstamped difference may just be that member
being behind. Without stamps it all works as before.

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
mc_id, online, last_seen, joined, in_step}], packs, follows}`. What you see
depends on where you stand:

- **Invited, not yet joined**: the name, the owner and `members` with only
  the owner (who is your friend). `packs` and `follows` are empty.
- **Joined**: every member's name and `joined`. `mc_id`, `online`,
  `last_seen` and `in_step` are filled in only for members who are your
  friends (and for you); for the others they are `""`, `false`, `0`, `false`.
  A group is not a way to watch strangers.

Access: anyone not in the group (or invited to it) gets 404 "no such group"
for every route, the same as a group that doesn't exist, so ids can't be
probed. An invited member can only accept or leave. Only joined members can
publish into it, follow through it or switch keeping in step. Only the owner
renames, adds, removes or deletes. Members may not be friends with each
other; that's why nothing reaches a member before they say yes.

Limits (409 with a code): 20 groups owned per PC (`groups_full`), 50 members
per group (`group_full`), 100 packs per group (`group_packs_full`), 20 groups
named per publish. Names are cut at 64 characters. What a PC shares into
groups may not add up to more than 1 MB (`storage_full`), and 64 KB in
groups where no one else has joined yet; counted against the one sharing,
whoever owns the group, so a group of one is no free storage. A share code
over 20000 bytes is refused (`413`), never cut. A publish, an edit or a new group that is refused changes nothing:
every check happens before the first change.

Membership changes:

- **Accept**: the group's packs (latest codes) reach the new member without
  being queued: the feed adds them, as recipe events numbered with the
  joining, while the member's cursor is before it (so a fresh start sees
  them again, like any event, and a long list can't push their other events
  out of the queue). Packs published after the joining arrive normally.
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
| `recipe` | `inst, key, name, code, left_out, direct, groups`, `marks` when sent | lists it; applies it to every local follow |
| `revoke` | `inst, reason, direct, groups` | removes those ways in; once none is left the pack leaves the list and its follows end (`ended` = reason), keeping their last version |
| `group` | `group` | reads the groups again |
| `group_follow` | `group, pack_from, inst` | reads the groups again; keeping in step follows from there |
| `joined` | `inst` | shares `inst` with them (an invite that named a pack) |
| `unfriended` | | drops shares with them; their direct packs leave; follows keep their last version |

Queue limits (per person waiting): 1000 events, 2 MB, and 150 from any one
sender (a backstop: a friend with a few dozen packs is nowhere near it).
Across all the queues one sender may have 3 MB waiting (`storage_full`).
Events are kept 14 days, `recipe` and `revoke` 180 (the desktop lists a
friend's packs from them). A new `recipe` from a sender for the same pack
replaces the older one, so there is one waiting per pack and not per call.
When full, the oldest event that is not a `recipe` or a `revoke` goes
first; only with none left does a queue drop from the sender holding the
most bytes in it (their oldest), so one friend sending a lot never pushes
out another friend's single pack.

A recipe event is replaced by a newer one for the same sender and pack, and
carries the whole set of ways it was shared (`direct`, `groups`): the desktop
sends every target of a pack in one call, so nothing is merged; a way that
stopped is taken back by its own revoke. A `group` or
`group_follow` event replaces an older one from the same sender for the same
group, since both only mean "read the groups again". `/v1/remove` also
throws away what the removed friend had queued for the remover (the group
revokes it causes are added after that and stay).

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

## Limits (abuse)

The relay sits behind Cloudflare Tunnel on the open internet. All of this is
in `relay/hopper_mailbox.py` (constants at the top) and listed in
`relay/README.md`:

- **The service has a size**: 20000 accounts and 32 MB of state. Share codes
  (direct and group packs) stop at 75% of that (a new version that isn't
  bigger always goes), sign-ups at 100%, so a flood of codes can't lock
  accounts out; one sender may have 3 MB waiting. A state file over 150% is
  not loaded (exit 78, untouched; recovery steps in `relay/README.md`). Accounts with no friends,
  no groups and nothing for 30 days are forgotten. A Cloudflare rate-limit
  rule on `/v1/register` is recommended on top (see `relay/README.md`).
- **Sign-ups**: 60 per hour per client address (`CF-Connecting-IP`, believed
  only from a connection on this machine, else the peer address; IPv6 per /64).
  The place is taken before the body is read, and the account is saved
  before it is answered.
- **Wrong invite codes**: 10 per 15 minutes per account, 30 per address.
- **Open invites**: 20 per account; run-out invites are swept on every
  invite and join.
- **Sizes**: bytes, not characters: share code 20000 (larger is `413`),
  each `left_out` entry 400 (100 entries), names 256, `marks` 2000
  entries (keys 600 bytes, `by` 64 characters). `inst` may not contain `/`.
- **Connections**: 2000 at once (`503` above), 30 s socket timeout and 30 s
  for a whole request, header names must be tokens, 3 long-polls per
  account (a fourth ends the oldest, which is answered `409
  poll_superseded` so the desktop backs off), `wait` up to 90 s.
- **Tokens** are stored as sha256; lookups go through a hash index.
- **Presence** (online, last seen, Minecraft id) is shown to friends only.
- Saving is a dirty flag plus one writer thread (about every 1.5 s, and on
  SIGTERM): a crash loses the last second or two. If saving fails it is
  logged and retried; requests still answer from memory. A state file that
  is invalid is renamed `state.json.corrupt-<time>`, one that can't be read
  is left untouched, and either way the service exits (status 78, which the
  unit does not restart; an ordinary crash exits 1 and does restart) instead of starting empty. While stopping, new
  requests get `503` and the log says whether the final save worked.
- Any unexpected error is a JSON `500` (`internal`), logged once.

## Deploying the relay

First deploy of this version over one that stores plain tokens (the
public VM): do it in this order, and keep the saved copy.

1. `sudo systemctl stop hopper-mailbox`
2. `sudo cp -a /var/lib/hopper-mailbox/state.json /var/lib/hopper-mailbox/state.json.pre-hash`
3. Copy `relay/hopper_mailbox.py` to `/opt/hopper-mailbox/`.
4. Install `relay/hopper-mailbox.service` (`UMask=0077`, `StateDirectory`,
   `RestartPreventExitStatus=78`, memory and file limits, syscall filter),
   then `sudo systemctl daemon-reload` and
   `systemd-analyze verify /etc/systemd/system/hopper-mailbox.service`.
5. `sudo systemctl start hopper-mailbox`; the journal says `hashed N stored
   tokens`. `curl -s http://127.0.0.1:8787/v1/health` answers `v: 2`.

The first start replaces each stored token with its sha256 and saves that;
PCs keep working since they still send the token and it hashes to the same
value. A `groups` key is added by itself. A new desktop works against the old
mailbox (falls back), and an old desktop against the new one (v1 feed).

**Rolling back after that first start needs `state.json.pre-hash`.** The old
relay compares plain tokens and can't read a hashed state file: with the new
file every PC would get 401 and have to sign up again, losing its friends.
So: stop the service, put `state.json.pre-hash` back as `state.json` with
`sudo cp -a` or `sudo mv` (keeps the owner and mode),
reinstall the previous `hopper_mailbox.py` and unit, start it. Accounts,
pairings and groups made since the copy are lost. Delete the copy once you
won't go back (it holds every token in the clear). With the old file,
queued non-recipe events reach old and new desktops alike from its feed: an
old desktop ignores fields it doesn't know but would try to follow a revoke
as a recipe with no code (it fails with an error and changes nothing).
Clearing the queues (`events` in the state file) avoids that.

Later updates (no format change) are: copy the file, restart.
