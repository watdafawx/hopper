# Crash corpus

Real crashes (from the maintainer's own packs and server, anonymised), each
replayed through the same code the crash card uses: `crashdetect::from_report`
for a game, `crashdetect::server` for a server. A new rule or signature is
checked against all of them, so fixing one crash can't quietly break another.

```bash
cargo test -p hopper-core --test crash_corpus -- --nocapture
# with every suspect, its reason and what was weighed:
CORPUS_WHY=1 cargo test -p hopper-core --test crash_corpus -- --nocapture
```

The test runs each case twice, with the older rules alone
(`HOPPER_CRASH_ENGINE=off`) and with the engine (`crashengine.rs`), prints both
and the hit rate over the distinct problems. It fails when a case marked
`pass = true` misses, or when a case the older rules got right goes wrong.

## A case

`<case>/`:

- `report.txt`: the crash report, its mod list cut out (it's in the mod file).
- `latest.log.gz`, `debug.log.gz` (optional): the game log as it stood when the
  crash was written (up to 2 MB before it, plus the loader's early failure
  lines), gzipped.
- `case.toml`:

```toml
side = "server"                  # "client" or "server"
mods = "5b7a3f79b4"              # the mod list in _mods/<hash>.txt.gz
report = "crash-2026-11-03_18.38.11-server.txt"
problem = 12                     # duplicates of one problem share a number
expect = ["mythsandlegends"]     # right answers for the top suspect; [] = no mod
at_least = "Likely"              # the top suspect is at least this sure
advice = "another"               # a word the advice must have (optional)
pass = true                      # enforced (else only tracked)
scored = true                    # false: nobody can pin it on a mod yet
note = "What really caused it, in a sentence."
```

`_mods/<hash>.txt.gz` is the instance's mod list at the time, one jar per line
(tab-separated): file, mod ids, name, version, required mods, asset/data
namespaces, the class packages the case's texts name, mixin configs, and `gone`
when the jar wasn't there any more to read. The test makes a fake jar per line
(metadata, folders and empty classes, no code), so the mod index and the
dependents count work as on the real folder.

## Adding a crash

Add it here first, then fix it with a general rule (or a hint), never a hint
alone. Anonymise: user names, player names, `C:\Users\<name>`, IP addresses,
launch tokens. The maintainer's extractor (not in the repo) does this from an
instance folder; by hand, cut the report's mod list and keep the log window.
