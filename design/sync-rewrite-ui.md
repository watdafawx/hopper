# Friend sync rewrite: P8 UI mocks

Status: mocks, 2026-11-25. Design Artifact (one artboard per option):
https://claude.ai/artifact/9P3ciQRRPEzccZdzYKzWkR. Builds on
[sync-rewrite.md](sync-rewrite.md) §4-§9.

Picks (to build unless the user says otherwise):

1. **Needs you cards (1A).** One card per author and batch in the existing
   Needs you popover (`notice_slot`, `anchored_popover`, 440 px, scrolls).
   Who, what, what stays meanwhile, buttons; the safe answer is filled.
   More than 5 removals: a tick ("I looked at the list...") unlocks the
   button, which then says "Remove 12 mods". "Do the same for his other
   change" tick when one author has several batches.
2. **This PC differs (2A).** Own chip state, popover rows with Put back and
   Remove for everyone (inline confirm: tick + "Remove Jade for everyone").
   Narrow: the second button folds into a ⋯ menu.
3. **Pack update plan (3A).** "Your friends" block under the "what happens"
   line in `pack_update.rs`, one line per friend from `pack_plan()`, then a
   "Clean for everyone" tick (off). Ticked, the button reads "Update for
   everyone".
4. **Away (4A).** An Info note in the Needs you list ("1 note"), per-friend
   rows with See › (History filtered) and Put back…; the Put back dialog
   uses `undo_member` with a Today / Since you were away / This week
   segmented control and lists what is left alone.
5. **History (5A).** Rows as sentences starting with the person; a Why line
   only for changes Hopper made for someone (answer, pack update, undo,
   drift, both at once). The mod row's last-changed chip tooltip says the
   same.
6. **Sync chip.** Seven states, worst first: Needs you, This PC differs,
   Out of date, Behind (incl. "Getting changes"), Sending, Offline, In
   sync. `syncchip::group_look(&Status)` is the only place that words a
   sync state; the hero chip, its popover, the Friends page row and the
   library dot all print that `Look`. "Syncing" is retired.

Hopper Server has no friend groups yet, so no web screen changes in P8.
