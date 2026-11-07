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

## Prove a visual change; never judge it from the code
Reading your own diff proves nothing about pixels: a "centred" element, a "readable" colour or a "fixed" overflow is a claim until you have measured it on a screenshot of the running app. This applies to every UI change, whatever the model; "looks right" and "the code does X" are not evidence.
1. **Take the screenshot yourself** of the real state (the actual app, not a mock, not the code), at the size and theme where it matters, plus the worst case (the longest label, the biggest number, a long name, the narrowest window, a light and a dark theme).
2. **Measure, with numbers.** Use `measure_shot.py` (this folder: `python -I measure_shot.py shot.png --box x0 y0 x1 y1 [--round] [--zoom crop.png]`) to get the gaps on each side of the content, or compare two elements that must line up. Alignment and centring: left/right and top/bottom gaps within 1 px. Spacing and sizes: read the pixel distances (or `getBoundingClientRect` / computed style in the web UI) and write them down. Colour and contrast: sample the real pixels (foreground and background) and compute the WCAG ratio (>= 4.5:1 for text, 3:1 for large text and icons); do not trust the theme tokens alone. Overflow and wrapping: check the element's width against its container's in the running page, and that the text is on one line.
3. **If a measurement disagrees with the code, the screenshot wins.** Say so and fix the code (an explicit `line_height`, a font's built-in offset or a border you forgot are common reasons). Re-measure after the fix. Do not mark a change 'centred' or 'readable' before the re-measure passes.
4. **Compare with the thing it must match** (the History button's badge, the neighbouring button): same size box, same measurement, numbers side by side.
5. **Put the evidence in the change card**: the screenshot, the zoomed crop (`--zoom`), the printed numbers, and which states you measured. A card that says "centred" without numbers is not done; say what you did not measure.
6. Both apps: measure the desktop screenshot and the web UI screenshot (real browser, not DOM text).
