---
name: hopper-design
description: Hopper's UI workflow on top of AGENTS.md's design rules - mock-first with Design Artifacts, shared widgets, drag-pick lists, and desktop <-> Hopper Server web UI parity. Use before any UI change in src/gpui or crates/hopperd/ui.
---

# Hopper UI work

The design rules themselves are in `AGENTS.md` ("Design rules"). This skill is the workflow around them.

## Mock first (tier M with a look change, and tier L)
1. Mock the options in HTML under `mockups/` (gitignored): Now, A, B... "Now" is a real screenshot, inlined as a data: URI or uploaded as an asset.
2. Publish the set as a Design Artifact (Artifact tool: `action: "quickstart"`, `intent: "design"`, then create from the Design type), one artboard per option. Mark your pick with a one-line why and give the link: local HTML shows as source, not rendered. Update the same artifact for revisions.
3. The maintainer runs in full-auto mode: build your pick unless you're told to wait.

## Building it
- Look for a helper in `src/gpui/widgets.rs` before writing UI. If two places would share it, move it there.
- Every list row gets `widgets::row_hover`. Lists with tick boxes, switches or row buttons use drag-pick (`src/gpui/drag_pick.rs`; web UI `lib/dragpick.svelte.js`) plus Ctrl/Shift-click and the shared selection bar.
- Mod names anywhere use `widgets::mod_link` (icon + peek).
- Same screen in Hopper Server's web UI (`crates/hopperd/ui`, Svelte 5 runes)? Change it there too: filter bars, labels, chips, buttons, mod rows, dialogs. Shared look lives in `ui/src/app.css`.
- Instance and server pages share views with a source parameter; never add a thinner server-only copy.
- Check on screen: desktop via skill `hopper-gpui-testing`, web UI via skill `hopperd-scratch`.
