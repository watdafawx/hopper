# Plan: one source for what both apps share (desktop and Hopper Server web UI)

Status: planned 2026-11-07, to start in the week of 9 November.
Decided: keep both render stacks (GPUI desktop, Svelte web UI). No GPUI-in-WebAssembly, no WebAssembly build of `hopper-core` for the panel. Do the three things that remove the double work and the drift:

1. **One source of data**: themes and palettes, design tokens, shared wording.
2. **Logic stays on the server**: the web UI draws results from `hopper-core` through the API; it does not re-implement rules.
3. **A written spec per widget**, and every change to a widget is measured against it in both apps.

## Why
Every UI change is built twice (this week: the filter rail, count badges, the appearance popover). Copies drift: the web theme list vs the desktop one, badge sizes, wording. The pixel-measuring rule (AGENTS.md, skill `hopper-design`) catches drift after the fact; this plan removes the causes.

## Phases (in this order; each ends with tests green and a change card with measurements)

### 0. Inventory of duplicates (S, about half a day, read-only)
Find everything that exists in both `src/gpui` / `crates/hopper-core` and `crates/hopperd/ui/src`:
- palettes and theme names (`src/gpui/theme.rs` vs `ui/src/lib/theme.svelte.js` and the colour variables in `ui/src/app.css`),
- sizes and tokens (radius, control heights, paddings, alpha for selected and hover),
- UI wording: filter labels, state labels (Sleeping, Stopped, Updates), tooltips, empty states, crash card text (`ServerCrash.svelte` against the desktop card),
- icon choices per action, keyboard shortcuts,
- rules re-implemented in Svelte (counts, sorting, "Added by you" marks, update eligibility, client-only side, sleep text).
Output: a table (item, desktop place, web place, same or drifted, proposed owner) in `design/shared-ui-inventory.md`.

### 1. Themes and tokens from one file (M, about 1 day)
- `design/tokens/themes.json` (every palette, in the format of the user's own theme files) and `design/tokens/tokens.json` (radii, heights, spacing, alphas).
- Generators, no new heavy dependencies: a small Rust bin or `build.rs` writes the desktop table; a plain Node script writes `ui/src/lib/generated/themes.js` and the CSS variables. Generated files are committed; CI fails when they are out of date.
- One contrast test over every palette (the Rust one already exists: text on primary at least 4.5:1), run against the JSON, plus the same check in a Node test.
- User theme files keep working and load through the same parser on both sides; the web list and the desktop list have the same names, order and groups.
- Done when: adding or changing a theme means editing one JSON file; both apps show identical lists; contrast tests pass.

### 2. Shared wording (S-M, about half a day)
`design/tokens/strings.json` for text that appears in both apps (filter and state labels, tooltips, empty states). Generated into both. Text that comes from a rule (crash cards, sleep explanations) moves into `hopper-core` and is sent by the API, so the web UI shows the server's words.

### 3. Component specs (M, about 1 day for the first four)
`design/components/<name>.md` per widget: purpose, anatomy, exact sizes (px), tokens used, states (default, hover, focus, selected, disabled, long text, big numbers, narrow), the measurements both apps must meet, and which boxes to measure with `.claude/skills/hopper-design/measure_shot.py`. First four: count badge, filter rail (and its split update button), appearance popover, side panel. A change to a widget updates its spec; a change card for a widget contains the spec's measurement table for desktop and web.

### 4. Rules move to the server (M, audit first)
From the inventory, list rules living in Svelte; move each to `hopper-core` with a test and send the result through the API (new fields), then delete the Svelte copy. One rule per commit.

### 5. Parity check (optional, S)
A script that compares a desktop screenshot and a web screenshot of the same state using the boxes from a component spec and reports pass or fail. Only if the manual measuring turns out to be too slow.

## Order and sessions
0 -> 1 -> 3 (can overlap with 2) -> 4. Each phase is one session (Sonnet medium; phase 1 Sonnet high because of the generators and CI check), started from `main`, one writer per file set. Never mix with other UI work in `app.css`, `theme.rs` or `widgets.rs` while phase 1 runs.

## Risks
- Generated files cause merge conflicts: regenerate, never hand-merge; CI check names the command.
- A theme-file format change would break users' own themes: keep the format, only add optional fields.
- Spec rot: a spec the cards don't measure against is dead; the release gate checks for the table.
- Build time: the generators must run in seconds and only when their inputs change.

## Not doing
GPUI to WebAssembly; WebAssembly builds of `hopper-core` for the panel (the server already answers); a rewrite of the web UI in a Rust framework.
