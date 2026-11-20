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

The last point used to be read from bookkeeping only (nothing queued, nothing
failed), never from the mods folder, so a copy that silently lacked a mod said
"same as the group". It is now measured: see "Checking the copy against the
mods folder" below. "In step" is only said once that check says the copy
matches.

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
| Your change first | **Your change goes first** | "Waiting for your change to Sodium to be shared, then Mia's version goes in." (offline: + "Can't reach the group right now."; held back for Push: the Push line + "Then Mia's version of Sodium goes in.") | a member's change is kept aside for this PC's own change of the same mod (`PackCopy::held`, below) |
| Behind, game | **Behind: close the game** | "The pack's new version / the loader / N mods change when you close the game." | `Wait::Game`, loader moved while the game runs, or jars queued in `.hopper-locked.json` |
| Behind, you | **Behind: needs you** | "Some of your settings changed in this update too. Pick what to keep." | `Wait::Pick` |
| Can't apply | **Can't update** | the plain reason + whether Hopper tries again | a failed try (`group_retry`, `Wait::Failed`), GitHub's rate limit, or a mod from a link the player hasn't trusted yet (see below) |
| Offline | **Offline** | "Can't reach the group right now. Your copy may be out of date. Last checked 8 min ago." | the PC is not connected |
| Missing mods | **Missing mods** / **Differs from group** | "Your copy is missing 2 mods the group has." (or "Your copy differs in 2 mods." when some are another version or switched the other way) with a list, one row per mod | the check of the mods folder found rows (below) |
| Couldn't check | **Couldn't check** | "Hopper couldn't check CurseForge just now, so your copy might be missing something. Try again." | a site didn't answer, so the check can't say |
| Checking | **Checking** | "Comparing your copy with the group." | the check hasn't answered yet (only ever shown in place of Same as the group) |

When several apply, the first one the player must act on wins: Can't update,
Missing mods / Couldn't check, Behind: needs you, Behind: close the game,
Updating, Offline, Sending, Same as the group. Changes the group sent that
wait by design (Only maker, push by hand) are Parked / Waiting to push, never
"missing".

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

### A member's change to a mod this PC changed too and hasn't sent

A member's change to a mod this PC changed and hasn't sent yet waits for
this PC's publish (`Verdict::Held`), and after 10 s (`HELD_FOR`) the member's
code is taken in anyway. That used to drop their change of that mod without
a word. Then a change taken back here before it was sent left this PC on the
old file for good, and one sent much later (offline, a send that kept
failing, "When I press Push") went out as the newest and undid the member's
update for the whole group.

The change is kept now: `PackCopy::held` (settings), one `HeldChange` per
mod, with what is needed to make it later. `groupsync::plan` makes it once
this PC's own change of the mod is no longer unsent, through the same
`apply` as any member's change, and never while it is unsent, so the two
can't swap back and forth:

- **Sent**: the member's change goes in after it, dated right after that
  publish, so it wins on every PC. This PC's code then goes out again as an
  echo of the member's change (no Push needed for it).
- **Taken back**: it goes in as it came, with its own number.

Only the newest change of a mod is kept. A later one that goes in, or that
already is what this PC has, ends the one kept; a later one that loses only
to this PC's own publish takes its place. So a mod the group removed later
is never brought back by an older update. Mods only: a loader, pack version
or lock that waited too long is dropped as before (and logged).

The app looks at what is kept when the pack or its last publish changes
(`Store::held_due`), which is when such a change can go in: no timer, no
extra traffic. The status says it for as long as it waits (the "Your change
goes first" row above), and the check of the mods folder doesn't list the
mod as different meanwhile.

### The "taken in" baseline never claims more than is on disk

Group sync diffs a member's new code against `copy.got[member]`, the last code
taken from them, never against the files. So `got` must not claim a mod that
did not land, or that mod never shows as a change again and the PC stays
without it, silently. Two places used to break this and now share one rule
(prune the code, keep the rest):

- **A lookup failed** (CurseForge / Modrinth down, rate limit): `plan()` used to
  skip the entry and still mark the whole code taken. Now the failed entries
  (`Mods::unknown`, new in the code and not in the previous one) are cut out of
  the code stored in `got` (`sharecode::pruned_code`), so the next read of that
  member's code sees them as new again. The rest of the code applies at once;
  a lookup that never succeeds (file deleted on the site) blocks nothing and
  costs one retry per read. The task says "Couldn't look up N mods just now, so they weren't added. Hopper tries them again ...".
- **A follow's first install** (`recipe::applied_code`): a file that did not
  come in used to stay in the follow's code, and `reconcile()` then seeded
  `got[member]` with it. Now the stored code has only the files that are on the
  instance (`sharecode::landed`), the missing ones go to `left_out` and the
  task notes them.

Repair happens when the member's code arrives again: their next publish, a
restart, or Check now. There is no timer. A member's first code follows the
same rule: a file of it that couldn't be looked up (and that this copy
doesn't list) is cut out of the code stored in `got`. That code is also kept
as `partial`, so the next read of the member's code is a first code again
(`groupsync::first_changes`) and not compared with the cut one: compared, the
file would count as a change the member made, which nobody knows. Baselines
that were already wrong are covered by the check against the mods folder
below.

### Checking the copy against the mods folder

`hopper_core::groupverify::verify` compares what the group has (each member's
newest taken code, `copy.got`, with the marks winning over a code) with the
instance's addons. It is a local comparison: no disk walk per publish tick, no
per-file network call (a lookup only for files that are not on the PC, in
batches, and only when the copy lacks something). It runs off the UI thread on
demand and after a merge, a repair or a change to the mods folder, never on the
publish tick. One row per mod, with a reason:

| Row | Button | Repaired by |
|---|---|---|
| **Missing**: the group has the mod, this copy doesn't | Get it | yes |
| **Different file**: another version (says if yours is older) | Update | yes |
| **Off here, on in the group** / **On here, off in the group** | Turn it on / Turn it off | yes |
| **Left out for its source**: from a link nobody confirmed | Allow and get it | only after the yes (`incoming_sources::gate`) |
| **Kept only on this PC** | none | never (listed) |
| **Locked by you** | none | never (listed) |

A deliberate removal (a mark newer than the add) is never "missing"; a change
made here and not sent yet is this PC's own, not a difference; a pack mod on
another pack version is not "different". If a site didn't answer, the result
is "Couldn't check", never "matches". The instance page's strip shows the
headline and the list; **Make my copy match** applies exactly the rows in the
list through `groupsync::apply`, the same install path as sync's own changes.
Nothing downloads before a button is pressed.

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
3. runs the same merge as the automatic one (`groupsync::merge`) when
   something waits (a code in `waiting`, an owed mod that is ready, or a
   queued jar);
4. then verifies and repairs: it runs the check of the mods folder
   (`groupverify::repair`, which applies exactly the rows the check finds) and
   makes what the group has and this copy lacks, whether or not anything
   waited. This is what "pull" means: the copy ends as the group has it, not
   only "nothing queued". When the group couldn't be reached, the repair is
   not run (the last known state is what the Make my copy match button
   already offers);
5. says what happened in one line next to the button and as a notice: "Took in
   3 changes from Mia", "Already the same as the group", "Couldn't reach the
   group", or the reason it can't apply.

It does not skip what waits by design: the pack version still waits for the
game, a pick still waits for the player; the line says so.

## Seeing every state without a friend

`hopper --dialog group-sync --state NAME` fills a made-up group (you, Mia,
Noah, Ava) over the first two instances: the first is a copy of Mia's pack,
the second a pack of yours. NAME is one of in-step, updating, game, pick,
left-out, failed, offline, sending, after-yours, after-yours-push (a member's
change kept aside for yours; with -push, shared when you press Push), and
for the check of the mods folder
verify-matches, verify-behind (two mods), verify-cant, verify-kept, verify-all
(every reason once). Nothing goes to the real service
(`Store::group_preview`; the feed's friends and groups are ignored while it
shows) and Pull now settles what waits. Test builds only, like
`given_up_preview`.

## Where the code goes

- `crates/hopper-core/src/groupstatus.rs`: `Status`, `Input`, `status()`,
  `waiting_changes()`, `member_status()`, `merge_due()` / `pack_due_ready()`
  (what Pull now skips), `pull_says()`, wording. Tests.
- `crates/hopper-core/src/groupverify.rs`: `verify()`, `check()`, `select()`,
  `repair()`. Tests, one per reason.
- `src/gpui/store/group_verify.rs`: runs the check off the UI thread, keeps its
  answer, `repair_group`, Pull now's repair.
- `groupsync.rs`: `Owed::approved`, `owed_to_allow()`, `approve_owed()`, `owed_ready()` counting approved mods.
- `src/gpui/store/friends.rs`: gathers `Input`, `pull_group_now`.
- `src/gpui/group_pack.rs` (instance page notice) and `friends.rs` (group
  list rows, chip).
