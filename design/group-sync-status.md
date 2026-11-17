# Group sync: am I in step, and Pull now

Hopper desktop only. Hopper Server has no friends groups (nothing in
`crates/hopperd` touches `groupsync`, `group_copies` or the mailbox).

## Problem

Group sync is automatic and two-way, but nobody can see whether a member's
copy has what the group has. A friend's PC lacked a mod the group had and
nobody could tell, not the friend, not the owner. There is also no way to ask
"check now".

## What "in step" means

My copy is **in step** when it has what the group's latest state says, mod by
mod, loader and pack version included:

- for every mod the group changed last (`PackCopy::marks`: key, state, who,
  relay number), my copy has it that way;
- no other member's code is waiting to be taken in (`PackCopy::waiting`);
- nothing the group sent is missing from this PC, other than the two things
  that never count as behind: a mod **kept only on this PC** (`onlyhere`)
  and a mod the player **locked** (`KeptWhy::Locked`).

"The group's latest state" is not stored anywhere central. The relay keeps
only the newest code of each member's copy (`<who>/<inst>`), and each PC takes
those codes in mod by mod. So "the group's latest state" is what this PC knows
of the others' newest codes: `got` (taken in), `waiting` (arrived, not taken
in) and the marks. **Pull now** is how that knowledge is made current: it
reads the relay's newest codes again instead of trusting the local cache.

## States (own copy)

Shown on the group copy's instance page and in the group's Packs list. Words
are for players (see the maintainer's plain-language rule: no relay, mailbox,
recipe, publish, sync server).

| State | Label | Detail line | When |
|---|---|---|---|
| Same as the group | **Same as group** | "Same as the group. Checked 2 min ago." | nothing below applies and the PC is connected |
| Updating | **Updating** | "Taking in 3 changes from Mia." / "Getting the group's changes." | a merge runs, or codes wait and nothing blocks them |
| Sending | **Sending your changes** | "Your changes reach the group in a moment." | a change made here is not sent yet (and nothing else applies) |
| Behind, game | **Behind: close the game** | "The pack's new version / the loader / N mods change when you close the game." | `Wait::Game`, loader moved while the game runs, or jars queued in `.hopper-locked.json` |
| Behind, you | **Behind: needs you** | "Some of your settings changed in this update too. Pick what to keep." | `Wait::Pick` |
| Can't apply | **Can't update** | the plain reason + whether Hopper tries again | a failed try (`group_retry`, `Wait::Failed`), GitHub's rate limit, or a mod from a link the player hasn't trusted yet (see below) |
| Offline | **Offline** | "Can't reach the group right now. Your copy may be out of date. Last checked 8 min ago." | the PC is not connected |

When several apply, the first one the player must act on wins: Can't update,
Behind: needs you, Behind: close the game, Updating, Offline, Sending,
Same as the group.

"Waiting for the game" is shown exactly where sync waits for it today and
nowhere else: the pack version (`Wait::Game`), a moved loader
(`group_loader_waiting`), and removals, renames and on/off switches of a jar
the running game holds open, which Windows refuses and Hopper queues in
`.hopper-locked.json` until the game closes. Adding a mod never waits.
Behaviour is unchanged; it is only visible now.

### A mod from a link the player hasn't trusted

`groupsync::file_change` skips a mod from GitHub or another link when its
source is not trusted yet (or Settings › Mod sources is off), and the member's
code is then marked as taken in, so it is never tried again. This is a
likely way a PC ends up without a mod nobody can see. `PackCopy::owed`
(settings, see `groupsync.rs`) now remembers each such change with all it
needs to make it later. The status reads it (`owed_to_allow()`): "Can't
update: ezactions (GitHub: octo/ezactions) is in the group but not on this
PC. Mods from GitHub and other links are off on this PC", and the next
merge or Pull now makes them once the source is trusted (`owed_ready`). The
strip's **Allow and install** button opens the existing "Allow mods from
other sources?" dialog (`incoming_sources::gate`); a yes sets
`Owed::approved` (`approve_owed`), which lets that one mod past the source
check on the retry, then pulls. A mod the group changed again, or that is on
this PC by now, leaves the list.

## Other members

Phase 2. What the data already tells without a new message:

- each member's newest code is in `copy.got` (and `waiting`): their copy's
  mods, loader and pack version as of their last publish, with the relay
  number (`seq`) and time (`at`) of that publish;
- `GroupMember::online` and `last_seen` (the relay's, friends only).

So **"behind by N"** is computable: compare their newest code with what the
group's latest marks say. A mod the marks say is X while their code says Y
is a change they haven't taken in. "In step" for them = no such mod and their
pack version and loader match. Their code is as fresh as their last
publish: a PC that is closed, or has not taken a change in yet, looks behind
because it is.

What the data does **not** tell, and needs a new field to say (a relay
change: the mailbox validates the event's fields and drops unknown ones, see
`recipe_fields` in `relay/hopper_mailbox.py`):

- *why* a member is behind, in particular "waiting for the game to close"
  or "can't apply: mod from an untrusted link". Their code only shows the
  difference.

Built: per member **Same as group** / **Behind by N** (tooltip says what it means)
or no chip when nothing of theirs has arrived. The rule
(`groupstatus::member_status`): over the group's marks, a mod marked by
someone else at relay number *n* counts as behind for a member when their
stamp for it is older than *n*, or, with no stamp, when their newest code is
older than *n*; a mod they changed themselves never counts. No relay change
is needed for this.

Not built (needs the relay change above): "Behind: waiting for the game"
and "Can't update" for other members.

## Pull now

A button on the group copy's instance page (secondary, outlined) and in the
group's Packs list row. It:

1. asks the relay for its newest codes and takes them into `waiting` (the
   same path as an arriving event, so it is safe to repeat);
2. forgets the retry backoff of a failed try, the "waits for the game" timer
   and a given-up state, so the merge runs now;
3. runs the same merge as the automatic one (`groupsync::merge`), also when
   nothing waits (it retries owed mods, and
   releases jars a closed game left queued);
4. says what happened in one line next to the button and as a notice: "Took in
   3 changes from Mia", "Already the same as the group", "Couldn't reach the
   group", or the reason it can't apply.

It does not skip what waits by design: the pack version still waits for the
game, a pick still waits for the player; the line says so.

## Seeing every state without a friend

`hopper --dialog group-sync --state NAME` fills a made-up group (you, Mia,
Noah, Ava) over the first two instances: the first is a copy of Mia's pack,
the second a pack of yours. NAME is one of in-step, updating, game, pick,
left-out, failed, offline, sending. Nothing goes to the real service
(`Store::group_preview`; the feed's friends and groups are ignored while it
shows) and Pull now settles what waits. Test builds only, like
`given_up_preview`.

## Where the code goes

- `crates/hopper-core/src/groupstatus.rs`: `Status`, `Input`, `status()`,
  `waiting_changes()`, `member_status()`, `merge_due()` / `pack_due_ready()`
  (what Pull now skips), `pull_says()`, wording. Tests.
- `groupsync.rs`: `Owed::approved`, `owed_to_allow()`, `approve_owed()`, `owed_ready()` counting approved mods.
- `src/gpui/store/friends.rs`: gathers `Input`, `pull_group_now`.
- `src/gpui/group_pack.rs` (instance page notice) and `friends.rs` (group
  list rows, chip).
