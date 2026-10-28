# Should Hopper Server get its own repository?

Draft for discussion (2026-10-28). Nothing here is decided or done.

## Short answer

**Not yet.** Split the *releases, issues and CI* now (cheap, fixes real
problems), keep the code in one repository while desktop and the server panel
are being made "the same app", and revisit a code split when the panel's
pace slows or outside contributors arrive for it alone. If we do split, the
best shape is a mirror first (option C), then a real split with
`hopper-core` as a versioned dependency (option B).

## Where things stand

- One Cargo workspace: `hopper` (desktop, `src/gpui`), `crates/hopper-core`
  (shared), `crates/hopperd` (server panel + Svelte UI).
- Released separately already: desktop tags `vX.Y.Z`, server tags
  `hopperd-vX.Y.Z`, two workflows, versions in separate `Cargo.toml`s.
- **Coupling is high and two-way:**
  - hopperd uses `hopper_core` in ~250 places across ~30 modules: hosting
    (75), scan, cf, instances, server, java, jobs, configdiff,
    server_pack_update, packdiff, install, richtext…
  - Desktop drives hopperd over HTTP through `hopper_core::hopperd` (client,
    886 lines) and `hopper_core::panel::hopperd`, and sends shared types as
    JSON (`ServerPlan`, `Instance`, `cf::File`). Both sides must agree on
    those shapes.
  - Parity work moves logic into core for both apps (this week: the pack
    author's advice finder, server pack updates, loader changelogs).
- Of 282 commits, 32 touch `crates/hopperd` and 41 `crates/hopper-core`;
  most server commits also touch core or desktop.

## Problems a split would solve, and cheaper fixes

| Problem | Seen | Cheaper fix (no split) |
|---|---|---|
| "Latest release" can be a Hopper Server release, which breaks the README's `releases/latest/download/hopper.exe` link | **Yes, today** (fixed: v0.15.3 marked Latest; the server workflow now publishes with `--latest=false`) | Done |
| Self-update has to tell the two apps' tags apart | Handled in code (`is_hopper_tag`, `hopperd-v` prefix) | Keep the prefix rule; add a test for each side |
| Server-only PRs build and test desktop too (slow CI) | Mildly | Path filters in `ci.yml`: server-only changes skip the desktop build, desktop-only skip the server jobs; core runs both |
| Issues for the two apps mixed | Some | Labels `app: desktop` / `app: server` + issue templates that ask which |
| Server users land on a desktop README | Yes | Short "Hopper Server" section at the top of the README pointing to `crates/hopperd/README.md`, and a page on the website |
| Contributors who only want the server panel clone a big repo | Not yet | Option C below if it becomes real |

## Options

### A. Stay one repo, tidy it up (recommended now)

- Path-filtered CI jobs; labels and issue templates per app.
- `--latest=false` for server releases (done), a release-notes script for
  the server like desktop's.
- An **API contract test**: a test in `crates/hopperd` that serializes the
  shapes desktop sends and reads (`ServerPlan`, `/hello`, `/pack/plan`
  results) and fails when they change, so a server change can't silently
  break desktop's Servers page.
- Pros: parity work stays one PR; shared code changes are atomic; no version
  skew. Cons: one big repo; release tags interleave.

### B. Real split: `hopper` (desktop + core) and `hopper-server`

- `hopper-server` depends on `hopper-core` by git tag
  (`hopper-core = { git = "…/hopper", tag = "core-v0.16.0" }`) or a
  published crate.
- Every core change needed by the server becomes: PR in `hopper`, tag core,
  bump the dependency in `hopper-server`, PR there. Parity changes like this
  week's are two to three PRs instead of one.
- The HTTP contract between desktop and server becomes a versioned API with
  real compatibility rules (old desktop ↔ new server and the reverse).
- Pros: clean ownership, smaller clones, separate stars/issues.
  Cons: slower cross-cutting work right when parity needs it most; version
  skew bugs; duplicate CI/release plumbing.

### C. Mirror: develop here, publish `crates/hopperd` to its own repo

- A workflow runs `git subtree split --prefix crates/hopperd` (plus a copy
  of `hopper-core` as a pinned dependency) and pushes to
  `watdafawx/hopper-server` on each server release; issues and releases can
  live there, PRs are taken here.
- Pros: server gets its own home page, stars and issue tracker; development
  stays atomic. Cons: two places to look; mirror plumbing to maintain; the
  mirror doesn't build alone unless core is vendored or pinned.

### D. Three repos: core, desktop, server

Most separation, most overhead. Only worth it if other tools start using
`hopper-core`. Not recommended.

## If we split later (B), the steps

1. Before: option A's contract test, and move every type desktop and server
   exchange into one module in core (`hopper_core::api`), versioned.
2. Tag core (`core-vX`); make core build without desktop-only features
   (translate, Windows-only bits behind cfg) so the server's Linux and
   Docker builds stay small.
3. `git filter-repo --path crates/hopperd` into the new repo, keeping
   history; add its own workspace, `rust-toolchain.toml`, CI, release and
   Docker workflows, README, AGENTS.md.
4. **Self-update:** the server's auto-update reads releases from
   `watdafawx/hopper` (`autoupdate::REPO`). Ship one last release in this
   repo whose updater reads the new repo, then publish there. Keep the old
   repo's server releases (don't delete), so old copies can still reach the
   bridge release.
5. Docker image name stays `ghcr.io/watdafawx/hopperd`; update the
   workflow's source label.
6. Desktop: `hopper_core::hopperd` stays in core; desktop's "Hopper Server"
   links and docs point to the new repo.
7. Remove `crates/hopperd` here; keep a README stub pointing to the new
   repo.

## Questions for you

1. Is the goal of a split visibility (its own page, stars, issues),
   contributors, or keeping this repo smaller? That decides C vs B.
2. Is the desktop ↔ server API allowed to break between versions (always
   update both), or should old desktops keep working with new servers?
3. OK to do option A's tidy-up now (path-filtered CI, labels, contract
   test)?
