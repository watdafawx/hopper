# Hopper Server: companion server panel (draft)

Status: M0–M4 built (2026-10-23). Mockups: `mockups/panel.html`. Run and API: `crates/hopperd/README.md`.

**M1 as built** differs from the draft in a few places:
- No separate shim process: hopper-core's `LocalServer` already starts each
  server detached and keeps its stdin handle inside the server process, so
  servers survive hopperd restarts (tested). That works on Windows only;
  Linux re-attach (a FIFO for stdin) is still to do.
- Plain HTTP on the LAN for now; TLS and internet access are M5. The owner
  is set with a one-time setup code from the log, so nobody else on the LAN
  can claim a fresh panel.
- Browser requests that change state need an `X-Hopperd` header (CSRF
  guard) on top of the SameSite=Strict cookie.
- Live updates are JSON over one WebSocket (snapshot once a second plus
  followed consoles every 150 ms); MessagePack can come later if needed.
- Discovery is mDNS only (`_hopperd._tcp.local.`), no UDP broadcast.
- Folders Crafty or MCSS manage are listed but never started by hopperd.
- Tested with a fake server (a Java program printing Minecraft-style logs);
  not yet with a real modded server on rig.

**M2 as built** (files):
- Measured on FOX over localhost: upload 696 MB/s, download 916 MB/s,
  10 000-file folder listed in ~25 ms (36 KB gzipped). LAN speed will be
  the network's.
- Uploads are sequential chunks per file (three files in parallel), not
  parallel chunks of one file; a single stream already fills a LAN.
- Zips are stored, not compressed (jars and worlds are already compressed;
  the disk is the limit on a LAN). Written by our own small zip64 streamer
  because the `zip` crate needs a seekable output.
- "File history" is the trash: every save and every replaced upload keeps
  the old copy (20 per file, 30 days), restorable. Diff views are later.
- Not in M2 yet: folder tree, content search, mod-aware columns in `mods/`,
  zip / unzip on the server, SFTP, per-path permissions (all v1).

**M3 as built** (Hopper desktop, in Hopper 0.8.96):
- Settings → Servers → Panel: **Hopper Server**. Find (mDNS) fills the
  address; Pair shows a 6-digit code that the owner allows in the panel; the
  token is kept in Credential Manager apart from other panels' API keys.
- With Hopper Server as the panel, the server list comes from the panel
  (no `Q:` drive). A remote server is a normal `ServerInfo` with `remote`
  set and a stand-in root `hopperd:\<folder>`, so the existing screens work
  unchanged and local file reads on it fail at once.
- Server sync's mods: the panel scans its own `mods/` (`/servers/{id}/mods`)
  so no jar crosses the wire to compare; Add uploads, Copy to pack
  downloads (through local backups), Remove goes to the panel's trash.
  Start / stop / restart / console go through the panel like Crafty's.
- M3b (Hopper 0.8.97): the rest goes over the API too. Shared code finds
  a remote server from its stand-in path (a registry filled when the panel
  lists its servers), so config compare (`/fs/{id}/walk`, `/hashes`,
  `/dirs`, `/text`), file copies, the crash scan (`/servers/{id}/crash`)
  and the NeoForge check (`/servers/{id}/neoforge`) work unchanged in the
  UI. Server pack updates run on the panel as jobs (`/servers/{id}/pack/
  plan`, `/pack/apply`, `/jobs/{id}`); the desktop shows the same review
  and mirrors the job's progress. The desktop passes its CurseForge key
  for the job when the panel has none (plain HTTP until M5's TLS).
  "Open server" buttons open that file or folder in the panel's web UI.
- Still local-only: folder mirror and link rules (they say so).

**M4 as built** (hopperd tabs: Players, Backups, Schedule, Settings; New server):
- Backups (hopper-core `hosting::backup`): zips in `.hopper-server-backups`,
  everything (minus logs, crash reports, Hopper's folders) or worlds only;
  a running server is flushed with `save-off` / `save-all flush` / waits
  for "Saved the game" / `save-on`. Restore needs the server stopped;
  worlds it contains are replaced whole and the current ones moved to
  `replaced/`. Restoring single files from a backup works. Not yet:
  incremental / deduplicated, off-site targets (v1).
- Schedules: Hopper's existing scheduler (daily restart with warnings,
  crash restart with a limit, start with the panel) now runs in hopperd,
  plus "back up every N hours while running, keep M".
- server.properties form (Hopper's spec table, validated), other keys raw;
  JVM memory and GC presets in `user_jvm_args.txt`.
- Players: online, ops, whitelist, bans from the server's json files;
  changes go through the server's own commands, so the server must run
  (offline edits with name lookup are v1).
- New server: vanilla, NeoForge, Fabric, Quilt, any release version;
  Java found or Temurin downloaded. Not tested end to end yet because it
  accepts Mojang's EULA (guard and version lists are tested).

## Goal

A standalone server panel ("Hopper Server", binary `hopperd`) that runs on the
machine hosting Minecraft servers (rig today, any Windows / Linux / macOS box or
a Pi tomorrow). It is a complete panel on its own: a web UI anyone can open in a
browser. Hopper desktop talks to the same API and gets the same control,
plus things only a launcher can do (pack to server sync, pairing instances).

Three priorities, in order:
1. **Speed.** Panel feels local even over the internet. LAN feels instant.
2. **Files.** Best file manager of any panel. Bulk, fast, safe, resumable.
3. **LAN first.** Same API on LAN and internet; the client always picks the
   fastest working path, and moves big transfers onto LAN when it can.

## What exists in Hopper already (reuse, don't rewrite)

| Module | Gives the panel |
|---|---|
| `panel.rs` `ServerPanel` trait | The control surface Hopper already drives (Crafty, MCSS, Pterodactyl, LocalPanel). `hopperd` becomes kind `"hopper"`. |
| `hosting/*` | Create server (vanilla, Fabric, Quilt, Forge, NeoForge), server packs, `server.properties` specs, JVM presets (Aikar / ZGC), run scripts, process supervision, schedules (warn, restart, crash restart, give up), log parsing. |
| `peer.rs` | LAN discovery (UDP 42424), Tailscale probing, 6-digit TOFU pairing, shared token, manifest diff, pull/push. |
| `rigsync.rs`, `serverpack.rs` | Pack ↔ server diff, client-only mod detection, sync ops with backups. |
| `java.rs`, `javadl.rs`, `loader_installer.rs` | Java runtime discovery and download, loader installs. |
| `crashscan.rs`, `crashdetect.rs`, `mclogs.rs` | Crash analysis, mclo.gs upload. |
| `modrinth.rs`, `cf.rs` | Mod search and install. |
| `mcp.rs` | MCP server pattern (Hopper on 47801). |

Step 0 of the build: move these into a `hopper-core` crate in a Cargo
workspace so the desktop app and `hopperd` share one copy.

## Architecture

```
            ┌──────────────── hopperd (one Rust binary) ────────────────┐
 browser ─┐ │  axum HTTP/2 + WebSocket   ── auth, roles, audit          │
 Hopper ──┼─┤  /api/v1 (REST, OpenAPI)   ── servers, files, backups ... │
 phone  ──┤ │  /api/v1/ws (events)       ── console, stats, file watch  │
 Claude ──┘ │  /mcp                      ── MCP tools over the same API  │
            │  embedded web UI (static, gzip+brotli, ~200 KB)            │
            │  supervisor ─ servers run in a shim, survive panel restart │
            │  hopper-core ─ hosting, sync, java, mods, crash scan       │
            │  SQLite (state, audit, users)  +  blake3 hash cache        │
            └────────────────────────────────────────────────────────────┘
```

- **Language:** Rust, tokio + axum. Single static binary per target:
  `x86_64/aarch64` × Windows, Linux (musl), macOS. Installs as a Windows
  service, systemd unit or launchd agent. Optional tray icon on desktops.
- **Web UI:** Svelte 5 + Vite, compiled and embedded in the binary
  (`include_dir`). No runtime CDN. Installable PWA for phones.
- **Servers outlive the panel.** Each server runs under a tiny shim process
  that owns stdin/stdout (Windows named pipe, Unix socket / pty). Updating or
  restarting `hopperd` never stops a game server; it re-attaches.
- **Multi-node (M8, after rig cutover):** one panel can add other `hopperd`
  nodes and show all their servers in one list (same API, proxied).

## Networking: LAN first, internet ready

| Path | How it is found | Used for |
|---|---|---|
| LAN | mDNS `_hopperd._tcp` + UDP broadcast on 42424 (same as peer sync) | Everything; always preferred |
| Tailscale | `tailscale status` probe (already in `peer.rs`) | Remote, zero config |
| Internet | Public URL: port forward + ACME (Let's Encrypt) cert, or a reverse proxy | Remote without Tailscale |
| Relay (later) | Optional rendezvous for NAT'd boxes | Last resort |

- `GET /api/v1/hello` returns every address the panel answers on (LAN IPs,
  Tailscale IP, public URL) plus the cert fingerprint.
- Clients race them (happy eyeballs), keep the fastest, re-check when the
  network changes. The connection badge always shows which path is live:
  `LAN · 192.168.50.3 · 1 ms`.
- **Split path:** control can stay on the internet URL while a big transfer
  probes LAN and switches to it if reachable (laptop at home on public URL).
- **TLS everywhere.** Self-signed cert pinned at pairing (fingerprint shown
  next to the 6-digit code), or ACME cert for the public URL. HTTP only on
  `localhost`.

## Speed budget (targets, measured in CI)

| Thing | Target |
|---|---|
| `hopperd` cold start | < 150 ms |
| Idle memory | < 30 MB |
| UI first paint on LAN | < 300 ms; over internet < 1 s |
| Console line, server → screen | < 50 ms LAN |
| Stats update | 1 s, pushed over WS (no polling) |
| List folder with 10 000 files | < 100 ms |
| LAN transfer | Saturate link: ~110 MB/s on 1 GbE, ~280 MB/s on 2.5 GbE |
| Re-sync a 300-mod pack, nothing changed | < 1 s (hash cache, no re-reads) |
| Re-sync, 5 mods changed | Only those 5 move |

How:
- One WebSocket per client, multiplexed channels (console, stats, file
  watch, jobs). Binary frames (MessagePack) for hot paths.
- Server-side hash cache: blake3 per file keyed by (path, size, mtime);
  watcher (`notify`) keeps it current. Manifests are instant.
- Transfers: parallel chunked streams (4–8), resumable, zstd for text-like
  files only (jars and zips skip it), content-addressed so a file already
  present anywhere on the server is copied locally, not re-sent.
- UI: virtualized lists, optimistic updates, everything cached in IndexedDB
  and reconciled from events.

## File manager (headline feature)

| Feature | Tier |
|---|---|
| Tree + list, breadcrumbs, virtualized 100k-file folders | MVP |
| Drag-drop upload of files and whole folders, multi-select, resumable | MVP |
| Download file / selection / folder (streamed zip, no temp file) | MVP |
| Rename, move, copy, new file/folder, all as background jobs with progress | MVP |
| Trash with restore (never hard delete by default) | MVP |
| Editor with syntax for `toml json json5 properties snbt yml cfg js (KubeJS) mcfunction` | MVP |
| Validate on save (TOML/JSON parse errors before writing) | MVP |
| Transfer queue: speed, path in use (LAN / Tailscale / Internet), pause, resume | MVP |
| Fuzzy file find + content search (ripgrep-style, regex) | v1 |
| Zip / unzip in place, extract server packs | v1 |
| File history: every edit keeps the previous version; diff and revert | v1 |
| Diff any file vs backup, vs another server, vs Hopper instance | v1 |
| Live updates when files change on disk (watcher) | v1 |
| Folder sync (one-way / two-way) with preview before apply | v1 |
| Mod-aware view in `mods/`: icon, name, version, side (client/server), updates | v1 |
| SFTP server built in (same accounts, path-scoped) | v1 |
| Per-path permissions for sub-users (e.g. only `config/`) | v1 |
| Bulk find-and-replace across configs | later |
| Image / NBT / region viewer | later |

## Feature set

Tiers: **MVP** = first usable release, **v1** = parity with good panels,
**later** = nice to have. ★ = Hopper-only (no other panel has it).

### Servers
| Feature | Tier |
|---|---|
| Server list: state, players, CPU, RAM, TPS, uptime | MVP |
| Start / stop / restart / kill, graceful stop with timeout then kill | MVP |
| Create server: vanilla, Paper, Purpur, Fabric, Quilt, Forge, NeoForge | MVP |
| ★ Create from a Hopper instance or CurseForge/Modrinth server pack | MVP |
| Import existing folder; import from Crafty / MCSS (rig migration) | MVP |
| Import from Pterodactyl | v1 |
| Clone server, templates | v1 |
| Change Minecraft / loader version with backup first | v1 |
| Proxy support: Velocity, BungeeCord, Waterfall | later |
| Bedrock (BDS), Geyser | later |
| Non-Minecraft game servers (generic process) | later |

### Console and logs
| Feature | Tier |
|---|---|
| Live console, ANSI colours, search, pause scroll, copy | MVP |
| Command input with history and command autocomplete | MVP |
| Log files browser, download, mclo.gs share | MVP |
| ★ Crash analysis: likely mods, same engine as Hopper's crash help | v1 |
| Log highlights: errors, warnings, player joins, filter chips | v1 |
| Multi-console view (several servers tiled) | later |

### Monitoring
| Feature | Tier |
|---|---|
| CPU, RAM, disk, network graphs (1 h, 24 h, 7 d) | MVP |
| TPS / MSPT (from RCON `forge tps` / `neoforge tps` / Paper `tps`) | v1 |
| Host overview: all servers, free RAM, disk, Java processes | MVP |
| ★ spark integration: run profiler, open report | v1 |
| Alerts: crash, low TPS, disk low, RAM high | v1 |

### Players
| Feature | Tier |
|---|---|
| Online list with heads, ping, playtime | MVP |
| Op, kick, ban, whitelist (with UUID lookup) | MVP |
| Player history (first/last seen, sessions) | v1 |
| Inventory / position viewer (NBT) | later |

### Configuration
| Feature | Tier |
|---|---|
| `server.properties` form with descriptions (specs in `hosting/properties.rs`) | MVP |
| JVM memory + flag presets (Aikar, ZGC, plain, custom) | MVP |
| Java runtime per server, auto-download right version | MVP |
| Startup command preview, env vars | MVP |
| Ports: detect conflicts, UPnP open (opt-in) | v1 |
| Resource limits: CPU affinity / priority, RAM cap (Linux cgroups) | v1 |
| Docker isolation per server, opt-in toggle, off by default (Linux, Windows via Docker Desktop) | v1 |

### Mods and plugins

**A server is managed like an instance.** Its mods get the same treatment
as a client instance's in Hopper: a mod list with icons, names, versions
and side; enable / disable; add, update and remove; browse CurseForge and
Modrinth for new mods (server-side and both-side ones, with dependencies).
This works in the panel's web UI and in Hopper desktop for a server on a
Hopper Server, through the same API. Updates for a modpack server still
come from the pack's **server files** (the server pack), not from each
mod's newest build: a pack update moves the server to the pack's new
server pack, and single-mod updates are for mods added on top of the pack
(or when the pack author's server files lag behind).

| Feature | Tier |
|---|---|
| Installed list with icons, versions, side, enable / disable | v1 |
| Search + install from Modrinth / CurseForge with dependencies (server-side filter) | v1 |
| Updates: pack mods from the pack's server files; extra mods from CurseForge / Modrinth (backup first) | v1 |
| ★ Server treated as an instance in Hopper desktop (same Mods tab, peek, add / update) | v1 |
| ★ Pack ↔ server sync from Hopper (diff, client-only mods skipped) | MVP |
| ★ Server pack update when the modpack updates | v1 |

### Worlds
| Feature | Tier |
|---|---|
| Upload / download world (zip, resumable) | MVP |
| Reset world, new seed, multiple worlds per server, switch active | v1 |
| Pre-generation via Chunky with progress | v1 |
| Map preview (BlueMap / squaremap link-out) | later |

### Backups
| Feature | Tier |
|---|---|
| Manual backup, `save-off` / `save-all` / `save-on` around it | MVP |
| Scheduled backups with retention (keep N, daily / weekly thinning) | MVP |
| Incremental + deduplicated (content-addressed chunks) | v1 |
| Restore whole or single file / folder | MVP |
| Off-site targets: another `hopperd`, S3 / B2 / R2, SFTP, local folder | v1 |
| Backup before every risky action (update, sync, version change) | MVP |

### Schedules and automation
| Feature | Tier |
|---|---|
| Scheduled restart with in-game warnings (exists in `hosting/schedule.rs`) | MVP |
| Crash auto-restart with give-up limit (exists) | MVP |
| Auto-start on boot with delay and order | MVP |
| Cron tasks: command, backup, restart, script | v1 |
| Triggers: on player join/leave, empty for N min → stop / sleep | v1 |
| ★ Auto-sleep: stop when empty, wake on connection attempt | later |

### Users and security
| Feature | Tier |
|---|---|
| Owner account, password + TOTP 2FA, sessions list, logout all | MVP |
| ★ Pair Hopper desktop with 6-digit code + cert fingerprint | MVP |
| Sub-users with roles (owner, admin, operator, viewer) and per-server rights | v1 |
| Scoped API tokens (per server, per permission, expiry) | MVP |
| Audit log: who did what, from which IP and path | MVP |
| Login rate limiting, lockout, IP allow-list | MVP |
| Passkeys (WebAuthn) | later |
| OIDC / SSO | later |

### Integrations and notifications
| Feature | Tier |
|---|---|
| Discord webhooks: start, stop, crash, player join, backups | v1 |
| Desktop notifications in Hopper, push on PWA | v1 |
| ★ MCP endpoint (same tools as Hopper's MCP, scoped token) | v1 |
| Prometheus `/metrics` | later |
| Generic outgoing webhooks | v1 |

### Panel itself
| Feature | Tier |
|---|---|
| Self-update (signed releases) without stopping servers | MVP |
| Installers: Windows MSI + service, `.deb` / `.rpm` / script, Homebrew, Docker image | MVP / v1 |
| First-run wizard: owner account, servers folder, import existing | MVP |
| Dark / light, mobile layout, keyboard palette (Ctrl+K) | MVP |
| Panel backup / restore of its own data | v1 |

## API

REST + one WebSocket. Versioned under `/api/v1`, OpenAPI generated from code
(`utoipa`), so Hopper's client and the web UI use the same typed models.

```
GET  /api/v1/hello                      addresses, version, fingerprint, capabilities
POST /api/v1/pair                       6-digit pairing → token
POST /api/v1/auth/login | /logout | /totp

GET  /api/v1/servers                    list + live summary
POST /api/v1/servers                    create (loader, version, from pack)
GET  /api/v1/servers/{id}               detail
POST /api/v1/servers/{id}/power         {start|stop|restart|kill}
POST /api/v1/servers/{id}/command       console command
GET  /api/v1/servers/{id}/logs[/{file}]
GET|PUT /api/v1/servers/{id}/properties
GET|PUT /api/v1/servers/{id}/jvm
GET  /api/v1/servers/{id}/players       + POST op|kick|ban|whitelist
GET|POST /api/v1/servers/{id}/backups   + /{b}/restore
GET|PUT /api/v1/servers/{id}/schedules
GET  /api/v1/servers/{id}/mods          + install|update|remove

GET  /api/v1/fs/{server}/list?path=     entries (name, size, mtime, hash?)
GET  /api/v1/fs/{server}/manifest?path= blake3 manifest for sync
GET  /api/v1/fs/{server}/file?path=     ranged download
PUT  /api/v1/fs/{server}/file?path=     upload (chunked, resumable: upload id + offset)
POST /api/v1/fs/{server}/ops            batch: move, copy, rename, mkdir, trash, zip, unzip
POST /api/v1/fs/{server}/sync           apply a diff plan (backed up first)
GET  /api/v1/fs/{server}/search?q=
GET  /api/v1/fs/{server}/history?path=  versions + revert

GET  /api/v1/jobs | /jobs/{id}          long-running work with progress, cancel
GET  /api/v1/host                       CPU, RAM, disk, Java runtimes
GET  /api/v1/audit
WS   /api/v1/ws                         subscribe: console:{id}, stats:{id}, fs:{id}, jobs, host
```

`capabilities` lets old Hopper builds work with newer panels (and the reverse):
features light up only when both sides have them.

## Hopper desktop integration

- New panel kind **Hopper Server** in Settings → Server panel, next to Crafty,
  MCSS, Pterodactyl. Found automatically on LAN; one click + 6-digit code.
- `ServerPanel` impl for it covers everything the trait has today; new
  capabilities (files, mods, players, schedules) get Hopper UI only for this
  kind.
- **Sync stops using the `Q:` SMB drive.** Pack → server sync goes through
  `/fs/.../manifest` + chunked upload: faster, works off-LAN, gets backups and
  audit for free.
- Server tab in Library shows the paired server's state, console, and a
  "Sync to server" diff exactly like today, now backed by the API.
- Hopper keeps working against Crafty / MCSS: nothing removed.

## Build order

| Milestone | Contents | Rough size |
|---|---|---|
| M0 ✅ | Workspace split: `hopper-core`, `hopper` (desktop), `hopperd` | 2–3 days |
| M1 ✅ | `hopperd` MVP: servers list, power, console WS, stats, shim supervisor, auth + pairing, LAN discovery, embedded UI shell | 1.5–2 weeks |
| M2 ✅ | Files MVP: browse, upload/download chunked + resumable, ops, editor, trash, transfer queue | 1–1.5 weeks |
| M3 ✅ | Hopper integration: panel kind, pairing UI, sync over API (drop `Q:`) | 1 week |
| M4 ✅ | Backups, schedules, properties, JVM, players, create server | 1.5 weeks |
| M5 | Internet: Tailscale path + open port (ACME / pinned TLS, required 2FA, rate limit, allow-list), split path transfers, audit, tokens | 1 week |
| M6 | v1 rest: mods, history/diff, search, SFTP, sub-users, webhooks, MCP, installers | 2–3 weeks |

| M7 | Rig cutover: import every Crafty / MCSS server, run side by side, then retire them | 2–3 days |
| M8 | Multi-node (after the panel runs fully on rig) | 1–1.5 weeks |

## Decisions (2026-10-23)

1. **Name: Hopper Server**, binary `hopperd`.
2. **Goal: replace Crafty and MCSS on rig.** Import from Crafty / MCSS is
   MVP (it is the migration path). Every Crafty / MCSS feature the rig uses
   must exist before cutover (M7): auto-start order, crash restart,
   scheduled restarts and backups, console, stats.
3. **Web UI: Svelte 5.**
4. **Multi-node:** wanted, but only after the panel runs fully on rig (M8).
   Keep the API node-aware from day one (`/nodes/{node}/servers/...` alias)
   so it slots in without breaking clients.
5. **Remote access: Tailscale and open port, both in M5.** Open port uses
   an ACME cert when a domain is set, else the pinned self-signed cert. Open
   port gets 2FA required, login rate limiting and an IP allow-list option.
6. **Docker isolation: offered per server, off by default.** v1 tier.
   Servers run as plain processes unless the toggle is on. Works on Linux
   and on Windows through Docker Desktop (rig has it, with GPU runtime).
7. **Repo: stays in the Hopper repo for now** (Cargo workspace:
   `hopper-core`, `hopper` desktop, `hopperd`). Released on its own with
   `hopperd-v<version>` tags, its own installer and docs, so it works without
   Hopper desktop. Split into its own repo (`git subtree split`) once the API
   settles.
