# Crash detection: draft

Status: phases 1 to 3 built (phase 1 in Hopper 0.8.74, phases 2 and 3 in 0.8.75, 2026-10-22).

Phase 1 as built differs from the draft in three places:
- the last crash is kept in `.hopper-crash.json` in the instance folder, not
  in `hopper.json`;
- the exit code isn't used yet (Hopper finds games by process, so it doesn't
  get one); a new crash report or `hs_err_pid*.log` is what counts;
- the Mods tab tag goes when the crash is dismissed, a game ends normally or
  the mod's file changes (updated); turning a mod off keeps it.
- "Turn off the likely ones and play" is two steps: **Turn off the likely
  ones**, then **Play without them** (or **Turn them back on**), so nothing
  launches by surprise.
- Fabric: nested jars (`META-INF/jars`) aren't in the package index yet, and
  a package shipped in two jars (a shaded library) names neither.

## Goal

When a game started from Hopper crashes, Hopper tells the player which mods
are involved, highlights them in the Mods tab, and offers a quick way to try
again without them.

Everything comes from what Minecraft, the mod loader and Java write
themselves:

- the crash report in `crash-reports/`,
- `logs/latest.log` and `logs/debug.log`,
- a JVM crash file (`hs_err_pid*.log`) in the instance folder,
- the game process's exit code.

Hopper reads no other mod's files or output.

## When it runs

Hopper already knows when an instance's game is running (`game.rs`, the
`Running` entry with its start time). When that entry goes away, the game has
ended. It counts as a crash when any of these is true:

1. a crash report in `crash-reports/` was written after the game started;
2. an `hs_err_pid*.log` in the instance folder was written after it started;
3. the process ended with a non-zero exit code (only known when Hopper
   launched the game itself);
4. `latest.log` ends with a loader start-up failure (see Signals) instead of a
   normal shutdown line.

The scan runs once, in the background, on that crash. It reads at most the
last 2 MB of each log.

## Signals

Each signal names one or more mods, with how sure Hopper is.

| Signal | Where | Names | Certainty |
|---|---|---|---|
| Loader start-up error list ("Loading errors encountered" on NeoForge; Fabric's "Incompatible mods found") | latest.log, crash report | the mod the loader names, and what it needs | Certain |
| Missing or wrong-version dependency in that list | same | the mod that needs it; the missing mod id and version | Certain |
| Stack frame in the exception's own trace from a mod jar. NeoForge and Forge print the jar on each frame: `~[somemod-1.2.jar%23215!/:?]` | crash report, latest.log | the first non-library jar in the first exception's trace | Likely |
| Frames further down, or in "Caused by:" | same | each further mod jar, once | Possible |
| Mixins applied to the crashing class (`{re:mixin,pl:mixin:APP:somemod.mixins.json:…}` on the frame) | same | mods whose mixin config is listed | Possible |
| "Mixin apply failed" lines naming a `*.mixins.json` | latest.log | the mod that owns that mixin config | Likely |
| `OutOfMemoryError` | crash report, log | no mod; a memory issue | Note only |
| Native crash (`hs_err_pid*.log`) | instance folder | the problem library, if it's a mod's native lib; otherwise a driver or Java issue | Note only |

**Library frames are skipped:** `java.`, `jdk.`, `sun.`, `net.minecraft.`,
`com.mojang.`, `net.neoforged.`, `net.minecraftforge.`, `cpw.mods.`,
`net.fabricmc.`, `org.spongepowered.`, `io.netty.`, `com.google.`,
`org.apache.`, `it.unimi.`, `org.lwjgl.`. So are frames from Hopper's own
launch wrapper.

**Matching a name to a mod in the instance:**

1. jar file name, against `Instance::addons` file names (also as `.disabled`);
2. mod id, from the jar's `META-INF/neoforge.mods.toml`, `mods.toml` or
   `fabric.mod.json` (Hopper already reads these for dependencies in
   `deps.rs`);
3. mixin config name, from the jar's own manifest or mod metadata (one read
   per jar, cached by jar size and time like `scan-cache.json`).

Fabric doesn't print the jar on stack frames. For Fabric, Hopper can map a
frame's package to a jar from each jar's file list (the zip's central
directory, cached). That comes in phase 2.

## Result

```rust
pub struct Crash {
    pub when: i64,                  // when the game ended
    pub source: PathBuf,            // the file the main finding came from
    pub headline: String,           // the report's "Description:" and exception line
    pub kind: CrashKind,            // Mods, Memory, Native, Unknown
    pub suspects: Vec<Suspect>,     // most certain first
}

pub struct Suspect {
    pub file_name: String,          // the jar in the instance
    pub project_id: u64,            // 0 when not from a site
    pub certainty: Certainty,       // Certain, Likely, Possible
    pub why: String,                // one plain sentence
    pub needs: Option<(String, String)>, // missing mod id and versions, if that's the cause
}
```

Only the latest crash per instance is kept, in `hopper.json`, and it's
cleared after the next normal game exit.

## What the player sees

1. **Notification** when the game ends in a crash: "Pack Name crashed. Hopper
   thinks Some Mod caused it." It has **Show** (opens the Logs & crashes tab)
   and **Dismiss** buttons.
2. **Logs & crashes › Crash** card at the top:
   - the headline, and when it happened;
   - suspects as mod rows (icon, name, opens its page), each with its
     certainty as a tag and the reason under it;
   - missing dependencies with an **Install** chip (the same banner logic the
     Mods tab has);
   - **Turn off the likely ones and play** (turns off Certain + Likely
     suspects, launches, and can be undone with **Turn them back on**);
   - **Open crash report** and **Share log** (mclo.gs, as now).
3. **Mods tab:** suspects get a red **Crashed** tag with the reason as its
   tooltip, until the next normal game exit or until the mod is updated or
   turned off.
4. **Memory and native crashes** say what they mean and point to the
   setting: "Out of memory: give it more in Settings › Memory," "Java itself
   crashed: try another Java (Settings › Java) or update your graphics
   driver."
5. **Unknown:** "Hopper couldn't tell which mod. The crash report is below."
   It shows the report's summary lines (`logs::crash_summary`).

Wording follows the rest of the app: "Off," "turned off," the mod's own name,
not its id.

## Out of scope for this version

- Remembering past crashes, or comparing mod lists between runs.
- Lists of known problem mods or known fixes.
- Checking jars or config files for damage.
- Graphics-driver or hardware detection.
- Servers: their crash banner (`crashscan.rs`) stays as it is.

## Code

- `src/crashdetect.rs`: library, no UI. `detect(inst, started) -> Option<Crash>`,
  plus `signals(text) -> Vec<RawHit>` with pure string parsing for tests.
- `src/gpui/crash_card.rs`: the card on the Logs tab.
- Store: when a running instance disappears, spawn `detect` in the
  background and send `Msg::Crashed(root, Crash)`. That notifies, saves to
  `hopper.json` and redraws the Mods tab.
- Mods table: a `crashed` flag in the row tags (`widgets::ModTags`).

## Tests

- Unit tests over short, hand-written log and crash-report excerpts, one per
  signal: NeoForge loading errors, a frame with `~[x.jar%23…]`, a mixin
  apply failure, OutOfMemoryError, a library-only trace that yields no
  suspect.
- An `#[ignore]` test over the real `crash-reports/` of the user's instances
  that prints what it finds, to check wording against real crashes.
- In the app: a throwaway instance with a deliberately broken setup (a mod
  without its required library) to check the notification, the card, the
  Mods tab tag and "Turn off and play".

## Phases

1. NeoForge and Forge: loader errors, stack frames with jar names, mixin
   failures, memory and native notes. Card, notification and Mods tag.
2. Fabric and Quilt: frame-to-jar mapping from jar file lists.
3. "Turn off and play" with undo, and the Install chip for missing
   dependencies on the card.

Rough size: phase 1 is about a day with tests; phases 2 and 3 are about half
a day each.
