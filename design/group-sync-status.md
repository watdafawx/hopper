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
- nothing the group sent was left out on purpose of the PC, other than the
  two things that never count as behind: a mod **kept only on this PC**
  (`onlyhere`) and a mod the player **locked** (`KeptWhy::Locked`).

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
| In step | **In step** | "Same as the group. Checked 2 min ago." | nothing below applies and the PC is connected |
| Updating | **Updating** | "Taking in 3 changes from Mia." / "Getting the group's changes." | a merge runs, or codes wait and nothing blocks them |
| Sending | **Sending your changes** | "Your changes reach the group in a moment." | a change made here is not sent yet (and nothing else applies) |
| Behind, game | **Behind: close the game** | "The pack's new version / the loader / N mods change when you close the game." | `Wait::Game`, loader moved while the game runs, or jars queued in `.hopper-locked.json` |
| Behind, you | **Behind: needs you** | "Some of your settings changed in this update too. Pick what to keep." | `Wait::Pick` |
| Can't apply | **Can't update** | the plain reason + whether Hopper tries again | a failed try (`group_retry`, `Wait::Failed`), GitHub's rate limit, or a mod from a link the player hasn't trusted yet (see below) |
| Offline | **Offline** | "Can't reach the group right now. Your copy may be out of date. Last checked 8 min ago." | the PC is not connected |

When several apply, the first one the player must act on wins: Can't update,
Behind: needs you, Behind: close the game, Updating, Offline, Sending,
In step.

"Waiting for the game" is shown exactly where sync waits for it today and
nowhere else: the pack version (`Wait::Game`), a moved loader
(`group_loader_waiting`), and removals, renames and on/off switches of a jar
the running game holds open, which Windows refuses and Hopper queues in
`.hopper-locked.json` until the game closes. Adding a mod never waits.
Behaviour is unchanged; it is only visible now.

### A mod from a link the player hasn't trusted

`groupsync::file_change` skips a mod from GitHub or another link when its
source is not trusted yet (or Settings › Mod sources is off), and the member's
code is then marked as taken in, so it is never tried again. This is a likely
way a PC ends up without a mod nobody can see. Now `Merged::skipped` lists
those mods, `PackCopy::skipped` remembers them (settings), the state says
"Can't update: Sodium Extra is from GitHub (owner/repo) and this PC doesn't
trust that source yet", and Pull now tries them again once the source is
trusted.

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

Per member: **In step** / **Behind by N** (with the names of the mods in the
tooltip) / **Offline** (not connected; last seen), plus "not heard from yet"
when no code of theirs has arrived. No relay change is needed for this.

## Pull now

A button on the group copy's instance page (secondary, outlined) and in the
group's Packs list row. It:

1. asks the relay for its newest codes and takes them into `waiting` (the
   same path as an arriving event, so it is safe to repeat);
2. forgets the retry backoff of a failed try, the "waits for the game" timer
   and a given-up state, so the merge runs now;
3. runs the same merge as the automatic one (`groupsync::merge`), also when
   nothing waits (it retries mods left out for an untrusted link, and
   releases jars a closed game left queued);
4. says what happened in one line next to the button and as a notice: "Took in
   3 changes from Mia", "Already in step with the group", "Couldn't reach the
   group", or the reason it can't apply.

It does not skip what waits by design: the pack version still waits for the
game, a pick still waits for the player; the line says so.

## Where the code goes

- `crates/hopper-core/src/groupstatus.rs`: `Status`, `Input`, `status()`,
  `waiting_changes()`, wording; later `member_status()`. Tests.
- `groupsync.rs`: `Merged::skipped`, `PackCopy::skipped`, retry of them.
- `src/gpui/store/friends.rs`: gathers `Input`, `pull_group_now`.
- `src/gpui/group_pack.rs` (instance page notice) and `friends.rs` (group
  list rows, chip).
