# Notices that don't move the page

Instance and server pages, Hopper desktop and Hopper Server's web UI.
Mock: [design/mocks/notices.html](mocks/notices.html), published as a Design
Artifact (one artboard per option): https://claude.ai/artifact/Ttvp249CVG9Kmo9XycCKAT

## Problem

Every notice an instance or server page has goes through one slot above the
tabs (`src/gpui/notice_slot.rs`, web `lib/NoticeSlot.svelte`): the two that
matter most as one-line strips, the rest under "N more". The slot keeps the
page from filling with strips, but each strip still pushes the tabs and the
list down when it appears and pulls them up when it goes.

Measured on the maintainer's 0.91.0 screenshots (1822 px wide): a strip is
40 px tall with a 9 px gap, so one strip moves the list 49 px and two move it
98 px. They come and go while the player works: a download from the
CurseForge website starts and ends, a mod changes while the game runs, the
paired server is compared again.

Some strips say what the hero already says:

- "TNP-LL8 has other mods than this pack" (pre-join) under a hero chip that
  says "27 mods differ" (the server sync chip, 0.91.0).
- "Downloading from CurseForge..." (pending) while the title bar says
  "Getting files from CurseForge..." and the mod's row says "Waiting...".
- The group sync strip under the group sync chip (both 0.91.0; the strip
  stays for states with a list or a choice).

There is a second, older jump in the hero itself: `hero::render` makes the
hero 6.5 rem when it has no pills and 9.5 rem when it has one, and the
"Running" pill only exists while the game runs. Starting the game on an
instance with no group and no server grows the hero by 3 rem.

Toasts (gpui-kit notifications) open top right, over the hero's Play / Stop
button.

## Audit

Tier is `notice_slot::Tier`: Stop (the game won't start or load), Choice
(the player has to decide), Info. "Now?" says whether the player has to act
now, or it is progress or information.

### Hopper desktop

| id | Where | Tier | Shown when | What the player can do | How often | Now? | Said elsewhere too |
|---|---|---|---|---|---|---|---|
| `stuck` | `stuck.rs:161` | Stop | the game's start looks stuck (stuck-start watchdog) | Stop and start again (confirm), See why, Wait (asks again in 5 min) | rare | yes | none |
| `srv-stuck` | `stuck.rs:259` | Stop | a server's start looks stuck | Restart server (its confirm), See why, Wait | rare | yes | the server's state pill |
| `srv-crash` | `server_crash.rs:246` | Stop | the server crashed (not shown on Logs & crashes) | Roll back the blamed mod, See why | occasional | yes | the Logs tab's dot |
| `sync-crash` | `rigsync.rs:3558` | Stop | the same crash, in the instance's Server tab | as `srv-crash` | occasional | yes | the server chip in the instance hero |
| `blockers` | `addons_extra.rs:1572` | Stop, or Info once it loaded fine | the mods' own files say the game will stop at loading, or it did | Roll back, See why, ✕ (Info only) | occasional | Stop: yes | none |
| `missing` | `addons_extra.rs:1026` | Stop, or Info once it loaded fine | a required mod is missing | Install all N; per mod Install or Enable under Details; ✕ (Info only) | occasional | Stop: yes | left out when the group strip says it (`NeedsMod`) |
| `dups` | `addons_extra.rs:633` | Stop | Fabric/Quilt: a mod is in twice | Keep newest (all, or per mod under Details) | rare | yes | the row's "Old copy" chip |
| `dups-note` | `addons_extra.rs:747` | Info | NeoForge/Forge: a mod is in twice (the higher one loads) | Remove other copy per mod; ✕ | occasional | no | the row's "Old copy" chip |
| `group-status` | `group_pack.rs:306` | by state | the group copy needs the player: missing mod (`NeedsMod`), behind with a list, parked, not sent, a source to allow, couldn't check; and while the group's website-only files download | Add them, Update pack, Pull from X, Push, Make my copy match, Allow and install | occasional (friends groups) | yes, except the download | the hero's group chip says the same state |
| `pre-join` | `join_help.rs:667` | Choice | the paired server has other mods or another NeoForge than the pack | Update server, Match server (NeoForge), Compare, Add to <server> (under Details), ✕ per set of differences | common for server owners | only before joining | the hero's server chip ("27 mods differ") |
| `pending` | `library.rs:2113` | Choice | files the authors only allow from the CurseForge website; with progress while the download window works | Show window / Download, Show files, Check now, ✕ (forget them) | common after CurseForge pack installs and updates | mostly progress | title bar task, the row's "Waiting..." button |
| `cf-change` | `library.rs:2024` | Choice | the CurseForge app wrote other pack info over Hopper's | Keep Hopper's, Keep and fix the CurseForge app, Use the CurseForge app's | rare | choice, not urgent | none |
| `client-on` | `addons.rs:5248` | Choice | a server's mods: N client-only mods are on | Show (the Client-only filter), ✕ per count | occasional | no | each row's Side icon |
| `shader-loader` | `addons_extra.rs:1881` | Choice | shader packs, but no Iris or Oculus | Find Iris / Oculus, ✕ | rare | no | none |
| `queued` | `addons_extra.rs:665` | Info | old files or on/off switches wait for the game to close | ✕ | common while playing | no | none |
| `restart` | `addons_extra.rs:1904` | Info | mods or packs changed while the game runs | ✕ | common while playing | no | none |
| `only-here` | `addons.rs:5206` | Info | a group copy has client-side mods not kept only on this PC | Review (picks them in the list), ✕ | once per pack | no | none |
| `server-hint` | `library.rs:2001` | Info | once per pack that shows signs of a server, server tools off | Set it up, ✕ | once | no | none |

### Hopper Server (web UI, server page)

| id | Where | Tier | Shown when | Same as desktop |
|---|---|---|---|---|
| `srv-crash` | `CrashStrip.svelte` | Stop | the server crashed | `srv-crash` |
| `srv-stuck` | `StuckStrip.svelte` | Stop | its start looks stuck | `srv-stuck` |
| `mods-twice` | `ModsNotices.svelte` | Stop (Fabric/Quilt) or Info | a mod is in twice | `dups` / `dups-note` |
| `mods-client-on` | `ModsNotices.svelte` | Choice | client-only mods are on | `client-on` |
| `mods-restart` | `ModsNotices.svelte` | Info | mods changed while it runs | `restart` |
| `mods-identifying` | `Mods.svelte:905` | Info, progress | the mods are being looked up (already behind the settle delay) | none |
| `mods-src-hint` | `Mods.svelte:907` | Info | mods from links may not have come along (Mods from links is off) | none |
| `mods-cf-key` | `Mods.svelte:913` | Info | no CurseForge API key; a form under Details | none |
| `moved` | `MovedBanner.svelte` | Choice | this copy moved to another panel | none |
| `rcon-on`, `rcon-off`, `alert-*` | `ServerAlerts.svelte` | Info / Choice | RCON state, panel alerts | none |

The panel's Overview page (`Overview.svelte`) has its own slot for panel-wide
alerts. It is a dashboard, not a server page, and is left as it is.

## Options (the mock)

All three show the same states: TNP Limitless 8 running with a file to get
from the website, the server differing, mods changed while it runs and a
toast; All the Mods 10 running with a mod changed; and All the Mods 10 with
a problem that stops play.

- **A. The hero holds it all.** No strips. Status and progress are chips in
  the hero's pill row, each with a popover that holds the strip's buttons
  (the 0.91.0 sync chip). Everything else goes into one "Needs you" chip
  whose popover is today's "N more" list. Mod problems also mark the mod's
  row.
- **B. One reserved line.** A notice line that is always there between the
  hero and the tabs; status and progress are chips as in A. No jump, but
  every page pays 56 px for it, and on most pages it says nothing.
- **C. Floating tray.** Notices float over the top right of the content as
  cards. No jump, but they cover the list's tools and first rows until moved.

**Pick: A.** The hero is a fixed height and already has chips with popovers,
a fold rule (`hopper::syncchip::plan`) and a settle delay (`hopper::settle`).
Moving every notice there means the tabs and the list start at the same
height on every page in every state, without reserving room on quiet pages.
It also ends the doubles above. The risk is that a problem which stops the
game becomes only a chip; the answer is in "Problems that stop play" below.
B is the fallback if that proves too quiet on screen.

## The system (A)

### The chips

The hero's pill row, in this order (most likely to come and go last, so a
chip that appears never moves the ones before it):

1. **Run state** (existing pill): Running, Working, and now its problems:
   "Looks stuck · 4 min" and "Crashed", red. Click opens a popover with the
   stuck or crash notice's buttons.
2. **Group** (existing sync chip). Its popover takes over everything the
   group strip had: the list of mods that differ with Make my copy match,
   Add them, Update pack, Pull from X, Push, Allow and install.
3. **Server** (existing sync chip). Its popover takes over pre-join: "TNP-LL8
   has other mods than this pack", the Only in the pack row with Add to
   <server>, Update server, Match server, Compare, and "Hide until it
   changes" (today's ✕, same key). A NeoForge the server would refuse turns
   the chip red, "Won't let you join", and it never folds. The server's
   crash (`sync-crash`) shows here too.
4. **Needs you**: every notice not covered by another chip, in a popover
   that is today's "N more" list (`notice_slot::list_row`, grouped Stops
   play / Needs a choice / For your information, Details open in place, ✕
   where there is one). The label is the one notice's title when there is
   one ("2 mods in twice"), "N need you" when more; the colour is the worst
   tier's. Only info left: a quiet grey "N notes" that may fold.
5. **Restart to load**: `restart` and `queued` in one chip while the game
   (or server) runs. The popover says what changed and what waits for the
   game to close; Hide is today's ✕.
6. **Downloads**: `pending` and the group's website-only files. Busy with a
   line along its foot while the download window works ("Downloading 1 of
   1"), amber when files wait for the player ("1 to download"). The
   popover has Show window, Show files, Check now and Forget them. It uses
   the settle delay, so a download that ends in a blink never shows.

Chips fold as the sync chips do now: full words, short words, icon, then
into "+N". A chip that needs the player never folds (`Look::fixed`).

### Problems that stop play

A Stop notice (`stuck`, `srv-stuck`, `srv-crash`, `blockers`, `missing`,
`dups` while they stop play) is:

- a red chip that never folds (run state for stuck and crash, Needs you for
  the rest);
- open by itself once, the first time it appears (per notice id, kept in the
  slot's `seen` set, as `open_new` works now). A popover floats over the page
  and pushes nothing. It must not take keyboard focus from a field the
  player is typing in;
- marked on the mod's own row where there is a mod: "Needs Sophisticated
  Core", "Stops loading", "Old copy" (exists). The marker opens the same
  popover.

### Toasts

Toasts open bottom right (`cx.theme().notification.placement`, one global
setting in gpui-component), off the hero's buttons. The web UI's toasts are
already at the bottom (centre).

### Hero height

The hero's height is fixed per page, never per state: an instance or server
page always keeps the pill row's room (`hero::render`'s `short` only for a
page that never has pills). Without this, every chip above would still move
the page on an instance with no other pill.

## Where each notice goes

| Notice | Goes to | Row marker |
|---|---|---|
| `stuck`, `srv-stuck` | run state pill, red, opens once | none |
| `srv-crash` | run state pill on the server page; server chip in the instance hero (`sync-crash`) | the blamed mod: "Stopped the server" |
| `blockers` | Needs you | "Stops loading" |
| `missing` | Needs you (or the group chip when the group says `NeedsMod`, as now) | the mod that needs it: "Needs X" |
| `dups`, `dups-note` | Needs you | "Old copy" (exists) |
| `group-status` | group chip popover; its download part goes to Downloads | none |
| `pre-join` | server chip popover | "Not on <server>" for Only in the pack |
| `pending` | Downloads | "From the website" (exists as the Waiting button) |
| `cf-change`, `client-on`, `shader-loader`, `only-here`, `server-hint` | Needs you | none |
| `restart`, `queued` | Restart to load | none |
| web `mods-identifying` | Downloads-style busy chip (work) | none |
| web `mods-src-hint`, `mods-cf-key`, `moved`, `rcon-*`, `alert-*` | Needs you | none |

### What stays a strip

Nothing above the tabs. The page that replaces an unreadable instance folder
(`library.rs`, "Start over") is a whole page, not a notice, and stays. The
panel Overview's slot (a dashboard) stays.

## What changes in which file

- `crates/hopper-core/src/syncchip.rs`: the chip mapping grows from two
  sources to all chips (`Source::Run`, `Attention`, `Restart`, `Files`), with
  their labels, tones and fold rules, so both apps say the same words. Tests
  for each label and for the fold order.
- `src/gpui/sync_chip.rs`: draws every chip (rename later if it helps); a
  section can carry an element (the verify list, the Only in the pack row,
  the notice list) besides its line and buttons; popover width per chip; open
  once on a Stop.
- `src/gpui/notice_slot.rs`: `slot` returns the Needs you chip instead of
  strips; `order`, `fold`, `list_row`, `hidden` and the keys stay. The strip
  code goes once nothing uses it.
- `src/gpui/hero.rs`: fixed height per page; chips in the order above.
- `src/gpui/library.rs`, `servers.rs`: build the chips instead of the slot;
  `pending_banner` becomes the Downloads chip.
- `src/gpui/join_help.rs`: `pre_join_bar` becomes a server chip section.
- `src/gpui/rigsync.rs`: the server chip carries pre-join and the crash.
- `src/gpui/group_pack.rs`: `group_status_bar` moves into `group_chip`.
- `src/gpui/addons_extra.rs`, `addons.rs`: `restart_note` and `queued` feed
  the Restart chip; row markers for missing and blockers.
- `src/gpui/stuck.rs`, `server_crash.rs`: feed the run state pill.
- `src/gpui/app.rs`: toast placement.
- Web: `lib/SyncChip.svelte` draws every chip; `lib/NoticeSlot.svelte` becomes
  the Needs you chip (same `notices.svelte.js` and `noticeorder.js`);
  `ServerPage.svelte` moves `CrashStrip` / `StuckStrip` into the state pill
  and `ModsNotices`' restart into the Restart chip; `app.css` for the chip
  looks.

## Build order

Each phase ships on its own and leaves no page worse.

| # | Phase | Size | Apps |
|---|---|---|---|
| 0 | Hero height fixed per page; toasts bottom right | S | desktop (web hero and toasts checked) |
| 1 | Pre-join into the server chip's popover; red "Won't let you join" | M | desktop (the web has no pre-join) |
| 2 | Downloads chip (pending + group website files) | M | desktop; web `mods-identifying` as a busy chip |
| 3 | Restart to load chip (`restart` + `queued`) | S | both |
| 4 | Needs you chip replaces the strips; Stop opens once | M | both |
| 5 | Stuck and crash into the run state pill | M | both |
| 6 | Group strip into the group chip's popover | M | desktop (no groups on Hopper Server) |
| 7 | Row markers for missing, blockers, pre-join | M | desktop; web where it has the notice |

Phases 1 to 3 take away the three strips of the TNP example. After phase 4
no strip is left above the tabs. Each phase is checked on screen at a wide
and a narrow window, light and dark theme, with the tab row's top measured
before and after the chip appears (must not move, 0 px).

## Hopper Server parity

The web server page already has the chip row (`SyncChip`, `ServerAlerts`
chips) under the same hero. It gets the same chips, words and order as the
desktop server page: Needs you from its `NoticeSlot`, the state pill with
crash and stuck, Restart to load, and the busy chip for identifying. Wording
comes from `hopper::syncchip` (served to the web UI as the sync chip's is
now). Groups and pre-join are desktop only.

## Open questions

1. Play while something stops play: start anyway (today), or open the Needs
   you list first?
2. Toasts: bottom right on desktop and bottom centre on the web (today's), or
   the same corner in both?
3. Is pre-join's "Update server" the same action as the server chip's "Sync to
   server"? If yes, the popover has one button, not two.
4. Should the Restart chip show on a server page too while the server runs
   (the web says "Changed while the server runs" today)? Proposed: yes.
5. `mods-cf-key` has a form under Details: fine in a popover, or should it
   move to Settings with a link from the chip?
