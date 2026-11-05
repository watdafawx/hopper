---
name: hopper-crash-engine
description: Rules for changing Hopper's crash and stuck-start detection (crates/hopper-core/src/crashdetect.rs, stuckstart.rs, stuckfile.rs, crash_story) - blame logic, confidence levels, fixtures, license limits. Use when a crash card blames the wrong mod or when adding a detection rule.
---

# Crash engine rules

- Never trust the loader's blamed mod. FML/NeoForge names whichever mod's event was running, and the cause is often another mod. Find the owner from evidence:
  - the namespace in a registry key (`ns:path` → the mod providing `ns`);
  - the mixin config's mod (`Attach error for x.json ... from mod Y`);
  - the class's jar;
  - a class-init cycle in thread dumps.
- Confidence:
  - Certain only when the evidence names exactly one mod.
  - Likely when it points to one mod, but indirectly.
  - Possible for the mod that merely triggered or touched it ("was reading it when it failed").
  - Knock-on errors after the first failure are marked as knock-on.
- Mod ids map to jars through jar metadata, not filenames or CurseForge projects.
- The suggested action follows the real cause: "Update or turn off <mod>" for broken or mismatched builds.
- Every fix gets a fixture. Copy only the needed log lines into `crates/hopper-core/tests/` (crash corpus or `stuck_dumps/`), reading the user's instance without changing it. The corpus test runs only with `CI` or `CORPUS=1` set.
- No code from the Crash Assistant mod (license). Detecting or installing it is fine.
- Step references in the card text point at the step they follow from, never at themselves. Desktop crash cards and Hopper Server's `ServerCrash.svelte` use the same wording.
- Design notes: `design/crash-detection.md`.
