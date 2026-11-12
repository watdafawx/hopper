---
name: hopperd-scratch
description: Run a throwaway Hopper Server (hopperd) for testing server features, its web UI (crates/hopperd/ui) or desktop pairing, without touching a real panel or its data. Use before starting any hopperd process or testing server pages.
---

# Scratch Hopper Server

## Start it
```
HOPPERD_SERVICE=hopperd-test HOPPER_LOOPBACK=1 hopperd --data <scratch>\pdata --home <scratch>\home --servers-root <scratch>\srv --port <free port>
```
- `--data` is required. With `--home` alone, hopperd moves the real `%APPDATA%\hopperd\data` into the scratch home and rewrites its `hopperd.toml`. After the run, check that folder is untouched.
- Loopback puts the panel, router and SFTP on 127.0.0.1 only, with no mDNS, so there are no firewall prompts. Never `--bind 0.0.0.0`.
- When the web UI changed, build it first: `pnpm build` in `crates/hopperd/ui`. If you start a Vite dev server, stop it before you finish: a left-over vite/esbuild process locks the worktree folder.

## Test servers
- Make a small scratch server under `<scratch>\srv`, vanilla or a small Fabric pack. Pair a scratch desktop instance to it (desktop test build per skill `hopper-gpui-testing`: launch it with `HOPPER_TEST_WINDOW=1` so it opens on the second monitor and takes no focus).
- Starting and stopping your scratch server is fine. Never start, stop, kill or edit real servers or a real Hopper Server install (see `CLAUDE.local.md` for which hosts are real).

## Check the web UI
Open the panel in the built-in browser at `http://127.0.0.1:<port>` and screenshot each changed screen. "It builds" doesn't count as checked: if you couldn't open it, say so.
