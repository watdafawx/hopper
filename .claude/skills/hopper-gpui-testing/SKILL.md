---
name: hopper-gpui-testing
description: How to build, run, drive and screenshot a Hopper (GPUI desktop app) test build safely - test exe, --mcp port, scratch HOPPER_DATA_DIR, throwaway instances, MCP click/scroll quirks. Use before running or checking any Hopper desktop change on screen.
---

# Testing Hopper desktop on screen

Machine-specific commands (build wrapper, ports, real instance folders) are in `CLAUDE.local.md` when present.

## Build the test exe
`cargo build --profile fast` (no LTO, incremental: small changes rebuild in seconds). Copy the exe to a scratch folder as `hv-<session>.exe` and run that copy, so parallel sessions don't lock each other's binary. Kill only your own copy; never kill `hopper.exe` by name (that's the user's Hopper).

## Run it isolated (every time)
1. Make a scratch data root `X` and write `X\data\settings.json` first. Settings directly in `X` fall back to defaults, which find the user's real CurseForge instances.
2. Launch with `HOPPER_DATA_DIR=X`, `HOPPER_LOOPBACK=1`, your own `HOPPER_MCP_PORT`, and `--mcp`. Check the port is free; never use the user's Hopper's port (47801) or its link handler (47802).
3. Call MCP `state` first. If any real instance shows, kill the exe at once.
- Loopback binds everything to 127.0.0.1 with no LAN discovery, so there are no firewall prompts. Never bind 0.0.0.0.

## Never touch real data
- Never test on the user's packs. Build a throwaway instance (a few jars plus a filtered `minecraftinstance.json`) in the scratch root, and delete it (Recycle Bin) afterwards.
- Never click Start/Stop/Kill on real servers or change the user's settings. For server features use a scratch Hopper Server (skill `hopperd-scratch`).

## Driving the app (MCP, never desktop computer use)
- POST JSON-RPC `tools/call` to `http://127.0.0.1:<port>/mcp`. Tools: screenshot, state, navigate (tab, instance, view, search, server), click, hover, scroll, type, key, drag.
- Send `arguments` as a raw JSON string (PowerShell `ConvertTo-Json` of nested hashtables gets ignored).
- Prefer `navigate` over clicks. Screenshot before every click. The first click on a new spot often only hovers: `hover`, then `click`, then confirm with a screenshot.
- Fresh builds open "What's new"; close it first.
- `key` takes `{"name":"enter"|"escape"|"backspace"}`; there are no ctrl combos.
- Dev args: `--theme NAME`, `--page`, `--instance`, `--tab`, `--dialog`. Don't pass `curseforge://` links: they forward to the user's running Hopper.

## GPUI gotchas worth checking on screen
- Scroll areas: the parent needs a fixed height and the area `.flex_1().min_h_0().overflow_y_scrollbar()`. Always test with MCP `scroll`.
- Title-bar buttons need `.occlude()`. MCP clicks skip Windows hit-testing, so check with WM_NCHITTEST.
- A clickable wrapper squeezed below a `flex_none` child only clicks at its start: put the handlers on the child.

## Memory check (before a release, not in CI)
`python .claude\skills\hopper-gpui-testing\mem-scratch.py <scratch>` builds a fake 1,156-mod instance plus 7 small ones; `pwsh -NoProfile -File .claude\skills\hopper-gpui-testing\mem-check.ps1 -Exe <hv-copy.exe> -Scratch <scratch> -Port 478xx -Label <version>` walks Library, Mods, scroll to the end, Versions, Servers, idle, and prints working set / private / GPU MB. Compare with the last release's numbers (0.58.0: 276 / 352 / 87 MB after scrolling the big list; with the icon cache: 141 / 167 / 39).

`mem-peek.ps1` (same args) measures the 1920x1080 banner, the Screenshots grid and the image viewer open / close. Memory log for a real run: start Hopper with `HOPPER_MEMLOG=1`; one line per 15 s and per page change goes to `memlog.txt` in the data folder (UTC times).
