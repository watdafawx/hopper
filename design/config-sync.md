# Config sync (shared mod settings)

Friends in a group already keep one pack in step, mod by mod. This note adds
the same for a mod's settings: a player picks the mods whose settings the
group shares, and from then on a setting one person changes reaches everyone
else, setting by setting, the newest change winning and naming who made it.

Mock: https://claude.ai/artifact/UZPT8ZrCBPq8TypXH95icK (A is the pick; B and
C are the options turned down; "Receiving" applies to all of them).

## Decisions that need the maintainer

1. **The relay changes** (a new `configs` field on recipes, see Transport).
   It is deployed by hand on the VM, so the desktop side can't ship until
   it is. The desktop checks the relay's health answer and hides the feature
   on an older relay; nothing breaks either way.
2. **Who may share a mod's settings**: any joined member (as with mods,
   recommended) or only the group's owner. This note assumes any member.
3. **The first share of a big config** (see Smart parts) defaults to "only
   settings you change from now on". The other choice, sending every setting
   that differs from the pack, is one click away for the sharer. Say if the
   default should be the other way round.

Everything else below is decided.

## What exists to build on

- `crates/hopper-core/src/groupsync.rs`: a group's pack copy (`PackCopy`),
  codes arriving per member (`arrive`), deciding each change (`plan`,
  `verdict`: Apply, Same, Older, Held), marks per mod (`Mark`: state, kind,
  who, the relay's event number), stamps carried in codes (`Stamp`) so a
  change passed on late keeps its first number, and `reconcile`.
- `crates/hopper-core/src/onlyhere.rs`: "Only on this PC" for mods. Its rule
  (a kept mod is sent the way friends last had it, so nothing echoes) is the
  model for "Keep mine" below.
- `crates/hopper-core/src/configedit/`: reads NeoForge / Forge TOML, old
  Forge `.cfg`, FTB `.snbt`, JSON / JSON5 / JSONC, `.properties`, YAML and
  `.ini` into the same sections and settings, and edits a value in place
  (comments, order and spacing stay). `apply` checks a set of edits
  together and refuses all of them if one is bad; `save_edits` puts edits
  onto the file as it is on disk now, with the old copy in backups;
  `moved_back` finds edits the game wrote back.
- `crates/hopper-core/src/configowner.rs`: which mod a config file belongs
  to, one rule for every screen.
- `crates/hopper-core/src/configindex.rs`: every settings file, its mod and
  its side (`Side::Client`, `Common`, `Server`, `Startup`).
- `crates/hopper-core/src/configcompare.rs` and `configdiff.rs`: a config
  setting by setting beside the server's copy, used by the Configs tab and
  by Sync to a server.
- History and undo: every config write goes through `write_safely`, which
  moves the old copy into `.hopper-backups/<time>/` (`sync::backup_into`,
  `backups::record`) and shows in History as "Changed"
  (`backups::Change::Edited`). `actor::begin` makes one batch one History
  row, `actor::Who::friend` names the friend, and `undo::undo` puts a
  config back from the kept copy.
- Lists that already say what stays on one PC: `packdiff::KEEP_LOCAL`
  (`options.txt`, `optionsof.txt`, `optionsshaders.txt`, `servers.dat`,
  `server.properties`), `configdiff::client_side` and its `CLIENT_TOPS`
  (JEI, EMI, Iris, Sodium, Xaero's, Controlling and others), and
  `configindex`'s skip of pack content under `config/`.
- Secret keys: `crates/hopperd/src/configs.rs` `secret(key)` (token,
  secret, password, passwd, apikey, webhook, privatekey) with masking for
  the web UI. It lives in hopperd only today.
- The game watcher: `game::watch` and `Store::running(root)`; the config
  editor already notices the game writing a setting back
  (`watch_moved_back`, "Re-apply").
- Hopper Server's own settings push (`crates/hopperd/src/config_sync.rs`):
  the panel owner pushes setting edits to paired players, who click Apply
  (`src/gpui/config_pushes.rs`).
- The mailbox (`relay/hopper_mailbox.py`): one recipe waits per sender and
  pack, replaced by the newer one, kept 180 days; share code up to 20000
  bytes, body 64 KB, 1 MB per sender in groups.

## 1. Picking

**The choice is per mod.** What travels is per setting.

- **Where**: one switch at the top of a mod's settings, "Share these
  settings with Survival crew". It goes in `ConfigView`'s header, so the
  Configs tab (a `Pick::Mod`) and the Mods tab's side panel (`ConfigPanel`)
  both get it from one place. The same as a menu item in the Mods tab's mod
  menu ("Share settings with Survival crew"), next to "Only on this PC" and
  "Edit config…", and in the selection bar for several mods at once. The
  Configs tab's mod rows have no menu today; they get a "Shared" chip, not a
  menu (the mock's menu there stands for the Mods tab one). The group panel
  gets a read-only "Shared settings" list (each mod, who shared it, how many
  settings) with Stop on each row.
- The switch only shows on an instance that is a group copy or a follow.
- **Per mod, not per file**: players think in mods ("share my JEI
  settings"), and a mod's files are already grouped by `configowner`. A
  mod's `-client`, `-common` and `defaultconfigs/` files all come with it.
- **Not per setting**: Create alone has about 180 settings over three files.
  A pin per setting means a setting someone tunes later never travels until
  everyone pins it, and a mod update that renames it quietly drops the pin.
  Per setting stays only as an exception on the receiving side: "Keep mine"
  on one setting (see Receiving).
- Option B (the group panel as the place to pick, with tick boxes) was
  turned down as the main place: it is far from where you change settings,
  so you'd change a setting, then go elsewhere to share it. Its list is
  kept as the overview.
- A mod that is "Only on this PC" can't have its settings shared; its
  switch is off and says why.
- Sharing is a group fact, like a mod being in the pack: once one member
  shares JEI's settings, JEI's settings are shared in the whole group, and
  that choice itself is latest-wins with a name ("Mia shares JEI settings
  now"). Turning it off is the same for everyone.

## 2. Smart parts

**Only settings that changed travel.** Each PC keeps, per shared mod, the
values it last saw for every setting (a snapshot). A setting whose parsed
value differs from the snapshot is a change. Comparing parsed values
(`configedit::same`), not text, so the game reformatting a file or
reordering keys is not a change.

**Per-setting merge.** A received setting is written into the receiver's own
file with `configedit::save_edits`, one value at a time. Every other setting
in that file stays as the receiver has it. Whole files are never sent or
replaced.

**What a recipe carries is the group's state, not a list of edits.** For each
shared mod: every setting someone in the group changed since it was shared,
with its value and stamp. That is what makes the mailbox's "only the newest
recipe waits" safe: a friend offline for a month gets the newest recipe and
with it every setting anyone changed, not a gap.

**Never shared**, checked when sending and again when receiving (a PC never
writes one of these from a friend, even if an older Hopper sent it):

- Whole files: `packdiff::KEEP_LOCAL` (`options.txt` and friends),
  anything outside `config/` and `defaultconfigs/`, any file
  `configedit::editable` refuses, and pack content `configindex` skips.
- Mods whose settings are personal by nature: `configdiff::CLIENT_TOPS`
  (minimaps, shaders, Controlling, menus) start with the switch off and a
  line saying "Usually personal"; the player can still share them (a group
  may want the same JEI layout), unlike the rules below.
- Personal settings, by key name: key bindings and hotkeys, window size and
  position, fullscreen, GUI scale, HUD and overlay positions, field of view,
  render distance, brightness, sound volumes, language. A built-in list of
  name patterns (`keybind`, `hotkey`, `key.`, `window`, `fullscreen`,
  `gui_scale`, `hud_x`/`hud_y`/`offset_x`, `fov`, `volume`, `language`, …)
  plus a small list of known mods' personal files (minimap waypoints,
  Controlling, JourneyMap's per-world data).
- Secrets: a key named like `token`, `password`, `passwd`, `secret`,
  `api_key`, `apikey`, `auth`, `session`, `cookie`, `webhook`, or a value
  that looks like one (20 or more characters of hex or base64 with no
  spaces). WaterMedia's `network.token` is caught by both. These are never
  sent. The Configs tab marks them "Never shared: looks like a password".
  The key rule is hopperd's `secret(key)`, moved into `hopper-core` so the
  desktop, the web UI and this feature share one list (with `auth`,
  `session` and `cookie` added).
- Paths and addresses: a value that is an absolute path (`C:\…`, `/home/…`)
  is personal and stays. A URL or IP address is allowed (a group's map
  server or a chat bridge is a fair thing to share), but the sharer sees
  "This setting has a web address; friends will use it too" once.
- A key the never-share list catches is shown in the Configs tab with its
  reason, so the player knows why it didn't go.

**Folders.**

- `config/` is shared, client and common files alike.
- `defaultconfigs/` is shared. NeoForge copies it into a new world's
  `serverconfig/`, so it is how a group agrees on server settings for the
  worlds they make.
- `saves/<world>/serverconfig/` is never shared between players: it belongs
  to one world, and on a real server the server's copy wins anyway. A
  shared setting from a `-server` file goes into `defaultconfigs/` on the
  receiver. Getting it onto the group's server is Sync's job (section 5).
- `kubejs/config/` and other pack content (quests, scripts) stay out. They
  are pack files, not a mod's settings, and the pack update carries them.

**Client and server side.** `configindex::Side` decides where a setting is
used. Client and common settings reach players. Server and common settings
are also offered to the server (section 5). Startup files are shared like
client ones; they only take effect on the next start anyway.

**A mod update that renames keys.** A received setting the receiver's file
doesn't have is not added (the mod may refuse unknown keys, or the key may
be stale). It waits in the copy, and is tried again whenever that mod
changes or the file is rewritten. If the receiver's mod is older, the
setting goes in once they update. The notice says "2 settings from Mia don't
exist in your JEI yet". After 30 days without a match a waiting setting is
dropped. A key that exists in neither the sender's nor anyone's current file
for 30 days is dropped from the group's state too, so renamed keys don't
pile up.

**A file that doesn't exist yet** (the mod writes it on first start): the
settings wait until it does, then go in on the next game close.

**Files the game rewrites on start.** Many mods read their file at start and
write it back, correcting values out of range or dropping keys they don't
know. After every game close Hopper compares each shared mod's files with
its snapshot:

- a setting Hopper wrote that the game changed back (`moved_back`) counts as
  refused here: it is not sent as this player's change, and the Configs tab
  shows "Your game didn't take Mia's value";
- any other changed setting is this player's change (in-game config screens
  are a normal way to change settings) and goes out.

**While the game is running.** Nothing is written into an instance whose
game is running (`Store::running`). Received settings wait and go in when the
game closes, and also right before the next launch in case Hopper was
closed. The notice says so ("Settings from Sam wait until you close the
game"). Local changes are read on game close, after a save in the Configs
tab, and at Hopper's start (edits made in a text editor while Hopper was
closed).

**First share of a big config.** Turning sharing on shows one choice:

- "Only settings I change from now on" (default): nothing is sent now; the
  snapshot starts here.
- "Also send my 23 settings that differ from the pack": the settings whose
  value differs from the pack's own copy of that file (the pack's overrides,
  which the pack update already reads). When the pack ships no copy of the
  file there is nothing to compare with, so this choice isn't offered.

Never "send all 640 settings": most are defaults, and sending them would
overwrite every friend's own choices with no reason.

**Removing a share.** Turning sharing off for a mod is a group change, like
removing a mod: settings stop travelling for everyone and every PC keeps the
values it has. Nobody's settings are reverted. A member who only wants to
stop receiving uses "Keep mine" instead (below).

## 3. Receiving

**Auto-apply, like mods.** It follows the group sync rule: two-way, latest
wins, shows who, no Update click. Each applied batch is one History entry per
mod ("Just Enough Items settings changed by Mia", the settings and their
old and new values underneath) with the file's old copy, so Undo puts it
back. Undo is a change of this player's and goes out as one, the same as
undoing a mod change.

**The opt-out is per mod on the receiver**: "Keep mine for this mod". It
works like "Only on this PC" for mods: friends' settings for that mod are
remembered but not written, and this PC sends that mod's settings the way the
group has them, so nothing echoes. "Sync again" writes the group's values.
"Keep mine" on a single setting works the same way, for the player who
shares everything about JEI except one setting.

A one-way follow (a friend's pack, not a group copy) receives shared
settings the same way and sends nothing, as it does for mods.

**Notices** (plain, short, say what happened and where):

- "Mia changed 3 Just Enough Items settings. Survival crew pack has them
  now. Your other settings stayed the same." Links: See what changed, Undo.
- "Mia now shares FTB Chunks settings. 1 setting changed to match the
  group." Link: Keep mine for this mod.
- "Settings from Sam wait until you close the game."
- "2 settings from Mia don't exist in your Just Enough Items yet. You have
  a different version. They go in once you have the same one."
- "Mia stopped sharing Create settings. Yours stay as they are."
- Sender: "Shared 2 JEI settings with Survival crew."
- Configs tab, per setting: "Changed by Mia", "Changed, goes out", "Never
  shared: personal", "Never shared: looks like a password", "Your game
  didn't take Mia's value".

Many settings at once (a first share) are one notice per sender and pack,
not one per mod.

## 4. Transport

**Field.** Recipes get a new optional field, `configs`, beside `code` and
`marks`: base64 of deflated JSON. The share code itself doesn't change, so
an older Hopper still reads it and ignores the rest.

```json
{
  "v": 1,
  "mods": {
    "p:238222": {
      "shared": {"on": true, "seq": 4182, "by": "a1b2"},
      "set": {
        "config/jei/jei-client.ini": {
          "advanced/itemListOnLeft": {"v": "true", "seq": 4190, "by": "a1b2"},
          "general/cheatMode": {"v": "false", "seq": 0}
        }
      }
    }
  }
}
```

- Mods by the same key as everywhere (`groupsync::addon_key`), files by
  their path, settings by their section path and key (`/` joined, escaped).
- `v` is the value as text in the file's own format, so it is written back
  the way the mod wrote it.
- `seq` and `by` are the stamp of that setting, as for mods: the relay's
  number for the event that first carried the change, and who made it.
  `seq: 0` means this publish's own change; the receiver dates it with the
  recipe's own number, exactly as `plan` does for an unstamped mod.

**Latest wins per setting.** The decision is `groupsync`'s, per setting
instead of per mod: Apply, Same, Older (a later change is known), Held (this
PC changed it and hasn't sent yet). The same code should decide both; the
change is to make `plan` and `verdict` work over a key and a state rather
than over mods only (a small generic step, with the mod tests unchanged),
not a copy of them.

**Sizes.** A setting is about 60 to 100 bytes in JSON, under half that
deflated. Three mods with ten changed settings each is about 1 KB. The cap is
16 KB after compression per pack per sender (about 400 to 600 changed
settings). Past it the sharer is told "Too many changed settings to share.
Share fewer mods." and nothing new goes out until it fits; it is never cut
silently. This fits next to a 20000-byte code inside the 64 KB body, and
inside the 1 MB group storage per sender.

**Offline catch-up** comes free: the mailbox keeps the newest recipe per
sender and pack for 180 days, and that recipe holds the whole state of the
shared settings. Someone back after a month gets every setting the group
changed, each with its stamp.

**Relay change (deployed separately).** In `relay/hopper_mailbox.py`:

- `recipe_fields` takes `configs` as text up to 16384 bytes, like `code`;
- `pack_bytes` counts it, so group storage limits still hold;
- the feed's field list for group packs (`inst, key, code, name, left_out,
  marks`) adds `configs`, so a member who joins later gets it;
- `/v1/health` answers `configs: true`, which the desktop checks once.

An older relay drops the field: settings don't travel and the desktop hides
the switch, saying the sync service needs an update. A new relay with an
old desktop: the field is ignored. The format of `state.json` doesn't
change, so rolling back is the usual "copy the file, restart".

## 5. Hopper Server

The group's server is not a mailbox member, so shared settings don't reach
it by themselves, and they shouldn't: a server's files change only with a
confirmation and a History entry.

- Two existing paths already carry a setting to a server: the Configs
  tab's "Compare with server" (`config_compare.rs`, per setting through
  `configcompare::pairs`, which also knows a `-server` file lives in
  `<level>/serverconfig/`), and Server sync's Configs section
  (`src/gpui/configs.rs`, file by file through `configdiff`, recorded with
  `history::record`).
- A setting shared in the group and read by the server (Server or Common
  side) gets a "Shared by Survival crew" chip in "Compare with server",
  with its arrow pre-set towards the server. The player sends it like any
  other setting there; the server's History records it. The per-setting
  compare is the right place, not the file-by-file Server sync, because
  pushing a whole file would also push this player's own settings.
- `-server` settings that arrive into `defaultconfigs/` show in that same
  compare against the server's `serverconfig/` copy.
- Nothing is pushed to a server automatically.

Web UI parity (`crates/hopperd/ui`): the panel isn't in the group and has no
share switch. Its Configs page (`Configs.svelte`) shows the same chip on a
setting when the desktop that sent it marked it shared (the desktop's
client snapshot, `hopperd::snapshot_files`, carries a list of shared keys).
The panel's own "push settings to players" (`config_sync.rs`, players click
Apply) stays as it is: it is the server owner telling players what the
server wants, which is a different thing from friends agreeing. The two
differ on purpose (Apply click vs. automatic); the PR should say so.

Found on the way (not part of this work): `hopperd::snapshot_files` uploads
whole files to the panel without masking secret keys; the panel masks them
only when showing them.

## 6. Phases, size, risks, tests

Phases in order. Each ends with the app run and checked on screen.

1. **Core, no UI (M, about 1.5 h).** `crates/hopper-core/src/configsync.rs`:
   a mod's shared files and settings (via `configowner` and `configedit`),
   the never-share rules, snapshots and changed settings, the `configs`
   payload (encode, decode, size cap), and `plan` / `verdict` made generic
   in `groupsync.rs`. Tests only.
2. **Relay (S, about 30 min, plus the deploy).** The four relay changes
   above and their tests. The maintainer deploys it.
3. **Desktop send and receive (M).** Publish `configs` with the copy's code;
   take them in with the merge (one edit lock, nothing written while the game
   runs, apply on close and before launch); History entries and
   `actor::claim`; notices.
4. **Desktop UI (M, look change, mock A).** The Configs tab switch, the row
   menu item and selection bar, the per-setting chips, Keep mine, the first
   share choice, the group panel's Shared settings list.
5. **Server (S to M).** The "Shared by" chip in "Compare with server", the
   `defaultconfigs/` to `serverconfig/` pairing, the shared-keys list in the
   client snapshot and the chip in the web UI's Configs page.

Overall L, in five reviewable pieces.

**Risks.**

- A secret slips past the name and value checks. Mitigation: both checks, on
  both ends, plus the Configs tab showing what goes; tests with real secret
  keys from popular mods (WaterMedia, Discord bridges, Simple Voice Chat,
  Distant Horizons). A setting can be marked "Never share" by hand.
- Two players fight over one setting (each with a game that writes it back).
  Latest wins ends it, and `moved_back` keeps a game's rewrite from counting
  as a change. Watch for a loop in the two-PC test.
- A mod reading a value differently by version (a setting that changed
  meaning). Rare; the per-mod Keep mine and Undo cover it.
- Edits made outside Hopper while it was closed are only found at its next
  start, so a change can look later than it was. Acceptable.
- Large packs: hashing every shared file at each game close. Only shared
  mods' files are read, and `configindex` caches by size and time.

**What to test.**

- Unit: changed-setting detection across every format, including
  reformatting with no value change; the never-share rules (names, values,
  files, paths) with fixtures; per-setting plan and verdict (latest wins,
  stamps passed on late, Held, Same) reusing the mod cases; payload size cap;
  a key missing on the receiver waits and goes in after a mod update;
  `defaultconfigs/` mapping; Keep mine echoes nothing.
- Relay: `configs` kept, counted in storage, replaced with the recipe,
  handed to a member who joins later, dropped above 16384 bytes with `413`.
- On screen, two scratch desktops (`HOPPER_DATA_DIR`, `HOPPER_LOOPBACK=1`)
  and a scratch relay, with a throwaway group and a small throwaway pack:
  share JEI, change a setting on one, see it arrive with the notice and a
  History entry on the other, Undo it, change it while the other's game
  runs (it waits), turn sharing off, Keep mine. Screenshots of each state,
  measured as the design checklist says.
- Server: a scratch Hopper Server, push a shared common setting through
  Sync, check its History.
