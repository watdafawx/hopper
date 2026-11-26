# Friend sync core rewrite

Status: design, 2026-11-24. Branch `dev/sync-rewrite`. Requirements come
from the Friend Sync Audit (37 workflows, 5 risks, 7 fix-history causes;
artifact `3LGB517AUfk7R6RJ8ZTy3T`). This file replaces the guessing layer
in `groupsync.rs` (`plan`, `changes`, `decide`, `sent_marks`, stamps,
`held`, `owed`, `became_packs`, `first_changes`, `partial`). It keeps:
the relay and its long poll (`relay/hopper_mailbox.py`, `mailbox.rs`),
invites and share codes for first installs, History and undo (`actor`,
`history`, `.hopper-backups` per-file copies), shared settings
(`configsync`), and the UI surfaces (group chip, Friends page, pack update
dialog, History).

Only about three friends use group sync, so the wire format breaks on
purpose: everyone updates together, and a packet from an older Hopper is
refused with "Mia's Hopper is out of date" instead of being guessed at.

## 1. Why the old core keeps breaking

Every PC compares a friend's whole mod list with the last list it took
from them and guesses what changed. "Removed", "not looked up yet", "the
pack ships it now", "they are behind" and "they undid it" all look the
same in that diff, and the relay's arrival order decides every clash. The
publish side has the same flaw the other way round: the mods folder on
disk, compared with the last code sent, counts as "my changes", whoever
made them. 14 of 36 fix commits trace straight to this; most of the rest
are stalls and races around it.

The new core never infers intent. A change is a record, made at the
moment Hopper makes it, that says exactly what happened.

## 2. The change record

```text
Change {
  dot:       (author, n)        author's member id + their own counter, 1, 2, 3...
  key:       Mod(id) | Lock(id) | Loader | Pack | Game
  was:       Value              what the author had before (for undo and wording)
  now:       Value              what it is after
  name:      "Mia"              shown name at the time
  lamport:   u64                max(lamport seen) + 1; orders concurrent updates
  seen:      {member: n}        the highest dot of each member the author had
                                taken in when they made it (a version vector)
  online_at: relay secs         relay time of the author's last contact before
                                making it (0 = never online); for "was away"
  batch:     id                 one player action (Update all, a pack update)
  why:       Player | Undo(dot) | Answer(review) | PackUpdate(mode) | SharedDrift
}
Value = Gone | File{file, on} | PackDefault | PackOff | PackRemoved | Text(s)
```

- Keys carry their kind: `Mod("cf:3")`, `Mod("mr:AANobbMI")`,
  `Mod("src:<file>")`, and addons outside `mods/` with a folder prefix
  (`Mod("shader:cf:5")`, `Mod("rp:mr:x")`). The sender knows its keys (it
  installed the files), so a receiver never needs a lookup to know which
  mod a record is about.
- `Game` is the Minecraft version of a plain instance (no modpack), the
  key the old core never sent.
- Time: **no PC clock orders anything.** `dot` and `seen` order changes
  causally; `lamport` breaks ties between concurrent updates; `online_at`
  and relay arrival times (both the relay's clock) only decide whether to
  *ask*, never who wins. Wall time on a PC is display only.
- Every PC keeps every record of the copy it has seen (the log). Records
  older than every member's `seen` and older than History's window are
  compacted into a checkpoint (the winning record per key).

## 3. Merge rule (pure, `register.rs`)

Per key the group keeps a register: the winning record plus any
concurrent records not settled yet. For an incoming record `x` against
each current record `c` of the same key:

| Case | Test | Result |
|---|---|---|
| Follow-up | `x.seen` covers `c.dot` | `x` replaces `c` |
| Stale | `c.seen` covers `x.dot` | `x` kept in the log (History), changes nothing |
| Same | `x.now == c.now` | both kept as one value, no question |
| Concurrent, both keep the mod (update vs update, on vs update, add vs add) | neither covers the other | the newer CurseForge file wins (file ids rise in upload order), then higher `(lamport, author)`, on every PC; so a friend coming back with an older file never rolls a newer one back. The loser's author is told ("Mia changed this too; hers is kept") |
| Concurrent, one removes or turns off, the other keeps or updates | neither covers the other | **clash: ask and keep.** Every PC provisionally keeps the non-destructive side (the update), and a review card asks. The first answer from anyone is a new record whose `seen` covers both, so it is a follow-up everywhere and the clash closes for the whole group. |
| Concurrent pack moves (two versions) | neither covers the other | clash: keep the newer version, ask |

Properties the scenario tests pin down:

1. Order-free: folding the same set of records in any order gives the
   same register on every PC (the relay's arrival order no longer matters).
2. Idempotent: the same record twice changes nothing (relay retries,
   restarts, re-sent checkpoints).
3. A friend who was away can never overwrite a change they hadn't seen
   without it being a clash, because their `seen` doesn't cover it.

## 4. Gate: what waits for the player (`gate.rs`)

The register is the group's agreement. What lands on *this PC's disk* is
the register minus what the gate holds. A held record stays in the log and
in the register; only the apply waits. Answers are records, so they travel.

Held, each as one review card per author and batch:

| Risk | Test | Card |
|---|---|---|
| Clash | merge rule above | "Mia removed Sodium while she was away. You updated it yesterday. [Keep Sodium] [Remove it]" |
| Long away | `arrival.at - online_at > 24 h`, or the record misses changes whose relay arrival is 24 h older than its own | "Mia was away for 3 days. She made 4 changes. [See changes] [Take them] [Keep ours]" |
| Many removals | more than 5 removals or turn-offs in one batch from one author | "Noah removed 12 mods at once. [See them] [Remove them] [Keep them]" |
| Pack back | `Pack` moves to an older version | "Mia moved the pack back to 8.4. [Move back too] [Stay on 8.6]" |
| Other pack | `Pack` moves to another project | "Mia switched to another pack. [Get it as a new instance] [Stay here]" |
| Moved back | a friend's follow-up puts a mod back exactly to what it was before someone else's update (`gate::moves_back`) | "Mia put 5 mods back to older versions. [Keep the updates] [Move back too]" |
| New source | `File` from GitHub or a link this PC hasn't allowed | existing "Allow and install" |
| Trust | the member is set to "Ask me first" | every record from them |

- "Keep" answers write a follow-up record restoring the kept value, named
  after the player who answered. "Take" applies the held records; no new
  record (the group already agrees).
- A tick box "Do the same for the other N" answers a batch at once. No
  typed confirmations anywhere.
- Status while anything waits: the existing "Needs you" chip. Nothing
  held is ever dropped by a timer (the old 10 s hold that lost changes is
  gone: a clash is a register state, not a race).
- Trust modes replace "Take changes from the maker only": per member
  `Auto` (default), `AskFirst`, `Ignore`. "Share my changes: when I press
  Push" stays as an outbox switch.

## 5. Only Hopper-made changes are the player's (`local.rs`)

- Records are written **when Hopper makes a change**, at the same place
  History already records it: the `actor` scope (`actor::begin`, `claim`,
  the add/on/off records and the files set aside under `.hopper-backups`).
  One hook, `synccore::local::note(root, edit)`, is called from that path
  for an instance that is a group copy. Mod table actions, History undo,
  pack updates and accepted reviews all pass through it; nothing else
  makes records.
- The disk is never diffed to make records. A scan only feeds **drift**:
  `drift(applied, disk)` lists keys where the mods folder differs from
  what this PC last applied (a jar deleted in Explorer, the CurseForge
  app, antivirus, a crashed write, a half-empty folder). Drift is shown
  only here, never sent: "3 mods are missing from this PC's mods folder.
  Hopper didn't remove them. [Put them back] [Remove for everyone]".
  "Remove for everyone" makes a `SharedDrift` record; "Put them back" is an
  apply plan from the register.
- Applying a friend's record writes History in the friend's name and
  moves `applied`, so it never echoes back as this PC's change. The
  installers run inside `actor::own_all`, so a save they make (a pack mod
  put back to the pack's file, a lock) is never read as the player's own.
- The edit queue is kept in `.hopper-sync/edits.json` until the engine
  takes it, so a change made just before Hopper closes still becomes a
  record on the next start.

## 6. Pack update plan before Update (`plan.rs`)

Each member publishes a small status with every packet:
`MemberStatus { applied: Seen, pack, edited_configs, off_pack_mods,
missing, game_running, waiting }`. The pack update dialog calls
`Engine::pack_plan(target, mode)` before the player presses Update and
shows one line per friend, from their last status:

- "Your friends move to 8.6 too. Their own settings stay theirs."
- "Mia: keeps her 3 changed settings files. Sodium stays off for her."
- "Noah: waits until his game closes."
- Shared settings (configsync) line: "Shared JEI settings follow yours."

Defaults (audit's open questions, recommended answers): a clean update
cleans only this PC; friends' turned-off pack mods stay off; friends keep
their changed config files. "Clean for everyone" is a separate tick box
(off by default), carried in the record as `PackUpdate { here, friends,
keep_off }`, so every receiver applies the *same* stated mode instead of
its own defaults. The pack version record carries that mode; config file
contents still never travel (only configsync's shared settings do).

## 7. Atomic apply with authors (`apply.rs`, IO)

`Engine::plan()` gives an `ApplyPlan`: steps `(key, from, to, by dot,
name)` plus waits (game running, review, source, manual download).
`apply::run(root, plan, fetch, job)`:

1. Fetch every file the plan needs into `<instance>/.hopper-sync/stage/`.
   A failed fetch marks only its own step (`Failed`, `Blocked` for an
   author-blocked CDN 403 with the by-hand download queue, `Retry{after}`
   for rate limits) and the others go on.
2. Write a journal (`.hopper-sync/journal.json`: the steps about to swap),
   then swap files in (old jar to `.hopper-backups` as today, recorded in
   History under the step's author), then the journal is removed.
3. Return an `ApplyReport` with one outcome per step. `Engine::applied`
   moves `applied` only for `Done` steps. A failed step stays in the
   register under its author and is tried again (back-off per key, never
   blocking other keys). Nothing is ever re-attributed to this PC.
4. On start, `apply::recover(root)` finishes or rolls back a journal left
   by a crash or power cut, so a cut-short install never leaves two copies.

No automatic backups: only the per-file copies History already keeps.

## 8. Away and back

- My own changes made offline queue in the outbox with their `seen` and
  `online_at`. They go out on reconnect unchanged (they are mine).
- On the way in, everything that arrived while I was away is folded at
  once; non-risky records apply, risky ones become cards, and the chip
  shows a one-line summary: "While you were away: Mia made 5 changes,
  Noah 2. [See changes]".
- Receivers judge my returned records with the same rules (clash,
  long away), so an old removal never lands silently anywhere.

## 9. Undo a friend's change

- Per row: History undo, as today, writes an `Undo(dot)` record.
- Per friend: `Engine::undo_member(member, since)` lists that member's
  winning records since a relay time and returns the edits that put each
  key back to its `was`. The desktop runs them as one batch ("Put back
  Noah's 6 changes from today"); they travel as one batch of `Undo`
  records. Keys someone changed after Noah are left alone and listed.

## 10. Wire: delta, hash, size limits (`wire.rs`)

The relay keeps only the newest event per sender and key, up to 150
events per sender per recipient, and caps an event's size. The format is
built for that:

- One event per **(sender, group)**, not per pack: a packet bundles every
  copy the sender has in that group. 150 packs to one offline friend is
  one event, so nothing is pushed out (the 150-pack drop).
- Each packet is **cumulative**: it carries every record of the sender's
  that some member's last `seen` doesn't cover yet (the unacked tail),
  never just "since my last packet". The relay replacing an older event
  with a newer one loses nothing.
- Acks are free: every packet carries the sender's `seen` per copy. When a
  member's `seen` covers my record, it leaves my tail.
- Each packet carries `hash` of the sender's register (winning dot per
  key). A receiver whose own register hash differs after folding, with
  nothing held, sets `needs_full` in its status; the sender's next packet
  is a checkpoint. A checkpoint is also sent to new members and every
  50th publish.
- Size: packets use the share code's deflate + base64url. Over 60 KiB a
  packet splits into parts (`key = group#part-i`, each with the total
  hash); a receiver folds only a complete set. A checkpoint of a 400-mod
  pack is about 50 KB before deflate.
- Version byte `WIRE_VERSION = 1`. Anything else: refused, member row says
  "Mia's Hopper is out of date. Update Hopper to sync with her."
- First install from an invite still uses a share code; the first packet
  that follows is a checkpoint.

No relay deploy is needed: the relay already stores opaque bodies by
key. A later, optional relay change ("tell the sender an event was
dropped") is a nice-to-have only.

## 11. Module layout (`crates/hopper-core/src/synccore/`)

Pure logic is separated from IO. Pure modules take values and return
values; they never touch the disk, the network or the clock.

| File | Kind | Owns |
|---|---|---|
| `mod.rs` | — | re-exports, module docs |
| `model.rs` | pure | `Dot`, `Seen`, `Key`, `Value`, `Change`, `Edit`, `Why`, `Arrival`, `PackMode`, `ConfigMode` |
| `register.rs` | pure | `Log`, `Register`, `merge`, `fold`, register hash |
| `gate.rs` | pure | `Limits`, `Risk`, `GateCtx`, `risks()`, `Trust` |
| `local.rs` | pure + one hook | `Snapshot`, `Drift`, `drift()`; `note()` hook called from `actor` |
| `plan.rs` | pure | `ApplyPlan`, `Step`, `Wait`, `PackPlan`, `FriendPlan`, `pack_plan()` |
| `wire.rs` | pure | `Packet`, `Body`, `MemberStatus`, encode/decode, tail, parts |
| `engine.rs` | pure | `Engine`: the one object callers use (below) |
| `apply.rs` | IO | `Fetch` trait, `run()`, `recover()`, `ApplyReport` |
| `store.rs` | IO | `load`/`save` of `data/sync/<copy>.json` (atomic write) |
| `sim.rs` | test | in-memory group: relay with newest-only and limits, offline switch, fake disks, restart, fetch failures |

### Public API (small, deep)

```rust
impl Engine {
    pub fn new(copy: &str, me: &str, name: &str) -> Engine;
    // this PC
    pub fn record(&mut self, edit: Edit, online_at: u64) -> Dot;   // from local::note
    pub fn drift(&self, disk: &Snapshot) -> Vec<Drift>;
    pub fn outbox(&self) -> Option<Packet>;                         // None: nothing to send
    pub fn sent(&mut self, packet_hash: &str, arrival: Arrival);
    // friends
    pub fn receive(&mut self, packet: Packet, arrival: Arrival) -> Received;
    pub fn reviews(&self) -> Vec<Review>;
    pub fn answer(&mut self, review: u64, answer: Answer, all: bool) -> Vec<Dot>;
    pub fn set_trust(&mut self, member: &str, trust: Trust);
    // disk
    pub fn plan(&self) -> ApplyPlan;
    pub fn applied(&mut self, report: &ApplyReport);
    pub fn pack_plan(&self, version: &str, mode: PackMode) -> PackPlan;
    pub fn undo_member(&self, member: &str, since: u64) -> Vec<Edit>;
    // reading
    pub fn value(&self, key: &Key) -> Value;            // what the group agrees
    pub fn last_change(&self, key: &Key) -> Option<&Change>;   // "Updated by Mia"
    pub fn status(&self, disk: &Snapshot) -> Status;    // from disk, never bookkeeping
    pub fn members(&self) -> Vec<MemberStatus>;
}
```

Everything above is a pure function of the engine's state and its
arguments, so every rule is a unit test and every workflow is a `sim`
scenario with no relay, no files and no sleeps.

### How the apps call it

Desktop (`src/gpui/store/friends.rs`), on its existing background
workers:

1. Publish tick: `engine.outbox()` → `mailbox.post(group key, packet)` →
   `engine.sent(..)` → `store::save`. No disk scan to publish.
2. Feed: each event → `engine.receive(packet, arrival)` → `store::save`.
3. Merge worker: `engine.plan()` → `apply::run(root, plan, fetch, job)`
   (game-running waits stay in the plan) → `engine.applied(report)` →
   `store::save`.
4. Mod table / History / pack update: already inside an `actor` scope;
   the hook calls `engine.record(..)` for group copies.
5. Status chip and Friends page read `engine.status(scan)`,
   `reviews()`, `members()`. Review answers call `answer()`.
6. Pack update dialog (`src/gpui/pack_update.rs`) shows `pack_plan()`.

Hopper Server: has no friend groups today, so no screen needs parity yet.
Shared pieces it gets for free: `apply::run` (atomic apply with authors)
for "Sync to server", and the same review wording if server groups come
later (a server as a read-only follower: `Trust::Auto`, never records).
Out of scope for this rewrite; listed as P9.

## 12. Risk and cause map

| Audit item | Killed by |
|---|---|
| R1 offline edits win on return | `seen` vectors (§3): an unseen change can't be overwritten silently; clash = ask and keep; long-away gate (§4) |
| R2 anything on disk counts as you | records only from the `actor` hook (§5); disk differences are drift, shown here only; mass removal from a broken folder never sends |
| R3 failed merge renames who changed what | per-step `ApplyReport`, `applied` moves per step, records keep their author's dot (§7); no echo (applies don't make records) |
| R4 pack update means two things | `PackUpdate` mode carried in the record; per-friend plan before Update from member status (§6) |
| R5 nothing asks before a change lands | gate + trust modes + review cards (§4); undo a friend in one click (§9) |
| MC version on plain instances | `Key::Game` (§2) |
| 150-pack queue drop | one cumulative event per sender and group; ack-based tail (§10) |
| C1 diff guesses intent wrong (9) | no diffs: removals are explicit records; keys travel with the record, so a lookup failure can't read as a removal; pack-shipped jars are `PackDefault`, not files; a first packet is a checkpoint with dots, not a "first code" |
| C2 disk change treated as the player's (5) | `local::note` is the only record source; stale in-memory lists can't publish (nothing publishes a list) |
| C3 relay keeps newest code only (4) | cumulative packets; make-then-undo is two records, both delivered |
| C4 local races and timing (6) | clashes are register state, not a 10 s hold; publish is outbox-of-records, so a publish mid-merge can't miss or double; game-running is a plan wait; journal stops duplicate copies |
| C5 apply fails, then stalls or loops (6) | per-step outcomes with back-off per key; `Blocked` goes to the by-hand queue; one failing key never blocks others |
| C6 status said "in sync" when it wasn't (6) | `status(disk)` compares the scanned disk with the register; members report `missing` keys so "Mia lacks Create" shows |
| C7 publish missed a field (2) | every key kind, locks included, is a record; an unreadable instance makes no records instead of a blank list |

## 13. Phases

Each phase is one Sonnet-sized session (S ≈ 1 h, M ≈ 2 h, L ≈ 3 h), owns
only the files listed, ends with `cargo test -p hopper-core` green through
`X:\Dev\tools\cargo-ccd1.ps1`, and un-ignores the named scenarios in
`crates/hopper-core/tests/sync_scenarios/`. Phases 1 to 6 touch only
hopper-core, so the desktop keeps working on the old core until P7.

| Phase | Size | Owns | Delivers | Un-ignores |
|---|---|---|---|---|
| P1 merge rule | M | `synccore/model.rs`, `synccore/register.rs` | `Seen::covers`, `merge`, `fold`, register hash; unit tests for order-free and idempotent folds | none (unit tests) |
| P2 gate | S | `synccore/gate.rs` | `risks()`, `Limits`, `Trust`; unit tests per risk | none |
| P3 engine + sim | L | `synccore/engine.rs`, `synccore/sim.rs` | `Engine` (record, receive, reviews, answer, value, last_change, undo_member) on P1/P2; `sim::Group` with newest-only relay, offline, restart (serde round trip) | everyday, clashes, away, undo, trust |
| P4 wire | M | `synccore/wire.rs`, `sim.rs` (relay limits only) | packet format, cumulative tail, acks, hash + `needs_full`, parts, version refusal | wire |
| P5 local + store | M | `synccore/local.rs`, `synccore/store.rs`, `actor.rs` (one hook call), `engine.rs` (`drift`, `status`) | `note` hook, `drift`, disk-based `status`, `data/sync/<copy>.json` | drift, status |
| P6 plan + apply | L | `synccore/plan.rs`, `synccore/apply.rs`, `engine.rs` (`plan`, `applied`, `pack_plan`) | `ApplyPlan`, staged + journaled `run`, `recover`, step outcomes, `pack_plan` from member status | apply, packs |
| P7 desktop wiring | L | `src/gpui/store/friends.rs`, `src/gpui/store/sync_modes.rs`, `src/gpui/store/group_verify.rs`, `src/gpui/store/only_here.rs` | publish/feed/merge on `Engine`; old `PackCopy` state is dropped (breaking); trust modes replace maker-only | existing relay `s01..s18` ported to `Engine` in `tests/sync_scenarios.rs` |
| P8 desktop UI | L | `src/gpui/group_pack.rs`, `src/gpui/friends.rs`, `src/gpui/pack_update.rs`, `src/gpui/history_view.rs` | review cards, "This PC differs", away summary, "Put back Noah's changes", pack plan lines, member "out of date" row; mock first (Design Artifact), plain-language review | checked on rig via `hopper --mcp` |
| P9 remove old core | M | `groupsync.rs`, `grouppack.rs`, `groupstatus.rs`, callers in `src/gpui/addons.rs`, `config_view.rs`, `shared_settings.rs`, `library.rs` | delete `plan`/`merge`/stamps/held/owed; keep helpers still used (`addon_key`, loader wording) | old `gap_*` relay tests renamed or removed |
| P10 hopperd (optional) | M | `crates/hopperd/src` sync-to-server path | use `apply::run` for atomic "Sync to server"; nothing else | — |

P3 and P4 can run in parallel after P2 (P4 owns `sim.rs` relay limits
only; agree the `SimRelay` struct in P3 first). P5 and P6 can run in
parallel after P3 (both touch `engine.rs`: P5 only `drift`/`status`, P6
only `plan`/`applied`/`pack_plan`; each adds its methods in its own
`impl Engine` block in its own file if that keeps diffs apart).

## 14. Scenarios

71 in-memory scenarios in `crates/hopper-core/tests/sync_scenarios/`
(module `rewrite` of the existing `sync_scenarios` test target), written
against `synccore::sim` and ignored as `sync-rewrite` until their phase.
Each names what it covers: all 37 audit workflows (W1..W37), R1 (6),
R2 (3), R3 (2), R4 (3), R5 (6), MC version (1), 150-pack drop (1), and
the fix-history causes C1 (4), C2 (2), C3 (3), C4 (4), C5 (3), C6 (1),
C7 (2). The relay-backed scenarios `s01..s18` and `gap_*` in
`tests/sync_scenarios.rs` stay as they are until P7 ports them.

```text
cargo test -p hopper-core --test sync_scenarios rewrite:: -- --ignored
```

## 15. Decisions taken by default (change here if the user disagrees)

1. Update vs removal clash: ask, keep the mod until someone answers (user).
2. "Away" = 24 h of relay time between last contact and the change
   arriving, or a record missing 24 h of the group's changes (audit).
3. Clean pack update cleans this PC only; friends' turned-off pack mods
   stay off; "Clean for everyone" is a separate tick box (audit).
4. Mods changed outside Hopper show here only, with Put back or Remove
   for everyone; never shared by a timer (audit).
5. Concurrent update vs update: higher Lamport clock wins, the other
   author is told; no question.
6. Many removals = more than 5 in one batch.
7. No backward compatibility: older Hopper packets are refused with a
   member-row note.
