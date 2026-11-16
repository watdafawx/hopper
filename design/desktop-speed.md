# Desktop speed budget

Headless CI checks the shared work behind desktop start-up and opening Mods.
Runs on Windows and Linux, without GPUI, a window, GPU, network, or real packs.

| Thing (1,000 generated jars) | Limit |
|---|---|
| Instance discovery, cold caches | < 2,000 ms |
| Instance discovery, persisted caches | < 500 ms |
| Instance discovery, after-update pack recheck | < 500 ms |
| Mods: loose list, metadata and mod-id cache reads/saves | < 500 ms per phase |
| Icons: 1,000 cached URL probes via `cached_many` | < 300 ms per phase |

How:
- `crates/hopper-core/tests/desktop_speed.rs` creates tiny ZIP jars, alternating
  Forge `META-INF/mods.toml` and `fabric.mod.json`, in a temporary directory.
  One pack has 960 tracked mods, one override needing claiming, and 40 loose jars.
  Cached icon files only exercise existence probes; no decoding or downloads.
- Fresh subprocesses call the actual `instances::discover` path, including
  `load`, `claim_shipped_jars` and `sources::looks::fill`. Mods uses
  `install::loose_jars`, `sources::looks::loose`, `deps::own_ids`, and cache saves.
- Cold means empty Hopper caches, not an empty OS disk cache. Warm uses persisted
  caches in a new process. After-update adds an older version field to the
  current-format jar-id cache and clears the instance's pack-check note to force
  claiming again (restoring one unclaimed override as an older install would). It does not install another Hopper binary. Format migration
  correctness remains covered by stampcache's existing unit tests.
- Every phase prints elapsed time and limit, and checks counts and parsed values
  so missing work cannot pass as fast. Fixture generation and cleanup are outside
  individual budgets, but included in total test runtime.
- Run separately: `cargo test -p hopper-core --test desktop_speed -- --ignored --nocapture`.
  CI uses the existing test profile (dependencies already optimized at level 2),
  avoiding a second release dependency build. Compare timings in this profile.

Local baseline (FOX, Windows, test profile, 2026-11-16), ms:

| Phase | Start-up | Mods | Icons |
|---|---:|---:|---:|
| Cold | 532.67 | 100.73 | 62.20 |
| Warm | 16.33 | 81.02 | 64.70 |
| After-update recheck | 162.15 | 99.97 | 69.12 |

Limits exceed 3x this baseline; warm start has a 500 ms disk/scheduling floor.
Full fixture/test/cleanup took 2.66 s; incremental compile/link took 3.40 s.
Expected CI addition: about 6 s per job with dependencies already built; allow
10-15 s on slower runners. Hosted Windows/Linux numbers remain to be measured.

Limits deliberately allow slow CI disks and scheduling noise. They are regression
alarms, not interactive frame targets. Re-measure before changing limits; never
raise one merely to silence a failure.

This does not cover GPUI layout/paint, UI-thread placement of work, settings saves,
accounts, config counts, network, or large real jar payloads. In particular, it
cannot detect moving a background operation onto the UI thread. Desktop screen
checks remain necessary for those regressions. Hopper Server behaviour is unchanged;
it shares the core paths under test and keeps its own panel budget.

AI-assisted implementation: GPT-6.1 Sol via Codex.
