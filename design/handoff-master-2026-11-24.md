# Master handoff, 2026-11-24 (notices batch)

Start the next master session from this file. Read AGENTS.md, CLAUDE.md,
`.claude/skills/hopper-release/SKILL.md` (untracked in the main checkout) and
`design/notices.md` first.

## Where things stand

User asked (2026-11-24): stop content jumping around by moving notices
into something that doesn't shift the page; the first sync chip should say
who it is in sync with; mod peek source links get a copy split pill; the
pack update button uses the pack's version tag.

All of it is built and merged into `main` (fast-forward from `t3/6386a470`):

| Item | State |
|---|---|
| Group chip "In sync with friends" (`syncchip.rs`) | done |
| "Update pack to v8.6" (`grouppack::version_tag/update_label`), TNP names ("8-1.78.1" -> "1.78.1") fixed in `instance_ops::pack_version_label` | done |
| Mod peek: GitHub/Issues/Wiki/Discord/Source are `widgets::link_pill` split pills (web: `.sitepair`) | done |
| Notices phases 0-7 of `design/notices.md` (option A: hero chips, no strips above the tabs): hero fixed height, toasts bottom right, server chip takes pre-join, Downloads chip, Restart-to-load chip (desktop + web), Needs-you chip, stuck/crash in the run chip (desktop + web), group strip into the group chip popover, mod row markers | done |
| Release 0.92.0 / Hopper Server 0.81.0 | released: tags `v0.92.0`, `hopperd-v0.81.0` pushed after green CI (run 45405012055); site changelog e8d04b64. Local exe built at `X:/Dev/hopper/target/perf/local/hopper.exe`; a background job copies it over `G:\Games\Minecraft\hopper.exe` once the running one closes (check it landed). Check the Release workflow runs with `gh run list --workflow Release` and `gh release view v0.92.0` |

Mock (private): https://claude.ai/artifact/Ttvp249CVG9Kmo9XycCKAT

## Not seen live (check these in the real app, throwaway instances only)

- Row markers "Stops loading" and "Not on <server>" (no fixture; words/tones unit-tested only). "Needs X" was seen.
- Real stuck start and real crash in the run chip (preview mechanism only).
- Amber Needs-you chip with only Choice items (unit-tested only).
- Restart chip's "waiting" state with a real running game, and on a real paired server page.
- Narrow widths: group chip popover, row markers.
- Web: light theme for the Restart/stuck chips, toast placement, web copy pills in a browser, web stuck state.
- `Vanilla+ 4.1` and `Create-Live-1.5` pack names on the update button (tests only).

## Known bugs / follow-ups

1. Escape does not close an auto-opened popover on desktop (S).
2. A run problem that appears while another auto popover is open stays a red chip and doesn't pop up (S).
3. `notice_slot.rs` (489 lines) no longer draws a strip; it holds `Notice`, `Tier`, `chip`, `run_chip` and the popover list. A rename (e.g. `notices.rs`) is optional churn.
4. `scan::short_version` still gives "8-1.78.1" for mod-jar style names; only `pack_version_label` was fixed.
5. Friends page (`friends.rs:778`) chip still says plain "In sync" (no server chip beside it, left on purpose).
6. Web has no Downloads chip, pre-join chip or groups; `mods-identifying` busy chip on web was not checked.
7. Open design questions from `design/notices.md` already decided: Play starts as today; toasts bottom right in both apps; one "Sync to server" button; Restart chip on server pages; web CurseForge key form stays in the popover (move to Settings later?).

## Housekeeping

- The visual changelog entry for this release: write a "Site: changelog for 0.92.0" section in `docs/changelog/index.html` (step 10 of the release skill). No change cards were written by these sessions; list them as "no card". Screenshots from the sessions are not in the repo.
- Old `t3-*` worktree folders under `C:\Users\MIcha\.t3\worktrees\hopper\` are from other sessions; not touched. Ask before removing.
- After release: remind the user to update Hopper Server on rig.
- Weekly limit was nearly out when this was written: use Sonnet/OpenCode for the follow-ups, Opus only for new screens.
