# Hopper Server (`hopperd`)

A standalone panel for Minecraft servers: a web UI in any browser, and the API
Hopper desktop uses to control the same servers. It runs on the machine that
hosts the servers. Hopper desktop is not needed to use it.

Status: **M5: servers, console, files, players, backups, schedules, settings, new servers; LAN, Tailscale and internet (HTTPS, two-factor).** Plan and roadmap: [design/server-panel.md](../../design/server-panel.md).

## Run it

```
hopperd --servers-root G:\Minecraft\packs
```

- Every folder in the servers folder with a `server.properties` is a server.
- The panel listens on port **8790** (`--port` to change). Open
  `http://<this-machine>:8790` from any device on the LAN.
- First start: the log prints a one-time **setup code** (also in
  `setup-code.txt` in the data folder). Use it in the browser to set the
  owner password.
- Settings and accounts are in the data folder: `%APPDATA%\hopperd\data` on
  Windows, `~/.local/share/hopperd` on Linux, or `--data <dir>`.
  `hopperd.toml` holds the port, bind address, servers folder and extra
  server folders.
- Servers keep running when hopperd stops or restarts; it reconnects to
  them on start (Windows; Linux follows).
- Folders Crafty or MCSS manage are listed but hopperd won't start them.

## What works (M1)

- Server list with live state, CPU, RAM, players and uptime; host CPU,
  memory and disk.
- Start, stop (saves the world), restart, kill.
- Live console over a WebSocket, filters, command input with history.
- Owner password (Argon2) with login lockout; browser sessions via an
  HttpOnly cookie.
- Pairing devices such as Hopper desktop: 6-digit code, owner allows it in
  the UI, device gets a bearer token.
- LAN discovery: mDNS `_hopperd._tcp.local.`

## With Hopper desktop (M3)

In Hopper: Settings → Servers → Panel → **Hopper Server**, then **Find** and
**Pair**. Allow the request in the panel (Access & devices) after checking the
code matches. Hopper then lists the panel's servers and works with them over the network,
no shared drive needed: mods sync, config compare and push/pull, crash
scan, NeoForge version, and server pack updates (run here as jobs).

## Files (M2)

- Browse any server folder (fast even with 10 000 files: the list is
  virtualized and the reply is gzipped).
- Upload files or whole folders (button or drag-drop): 8 MB chunks, three
  files at a time, pause / resume, automatic retry; a dropped connection
  carries on from the last chunk.
- Download a file (resumable: Range requests) or any selection as a zip,
  streamed while it's made (no temp file; zip64, so worlds over 4 GB work).
- New file / folder, rename, move, copy, trash. Keyboard: Delete, F2,
  Enter, Backspace, Ctrl+A.
- Editor (CodeMirror) for configs: TOML / JSON checked before saving,
  conflicts with changes made on disk caught, Ctrl+S.
- Trash per server (`.hopper-trash`, 30 days): deleted items and the
  previous version of every saved or replaced file; restore or delete for
  good.

## Running servers (M4)

- **Players:** online list with op / kick / ban, whitelist, ops and bans.
- **Backups:** everything or worlds only, from the Backups tab or on a
  schedule (every N hours while running, keep M). A running server's world
  is saved first. Restore whole (stopped server; current worlds are moved
  aside) or pick files from a backup.
- **Schedule:** daily restart with chat warnings, restart after a crash
  (with a per-hour limit), start with the panel, scheduled backups.
- **Settings:** server.properties with descriptions and checks; memory and
  GC preset for Java.
- **New server:** vanilla, NeoForge, Fabric or Quilt on any release; Java
  is found or downloaded.

## Security and remote access (M5)

- HTTPS on the same port as before. The panel makes its own certificate
  authority on first start; Hopper desktop pins it when pairing. Browsers
  warn about it; for a trusted certificate set `acme_domain` (and
  `acme_email`) in `hopperd.toml` and forward port 443 on that name to the
  panel: it gets one from Let's Encrypt.
- Plain HTTP still works from this PC, the LAN and Tailscale (set
  `require_tls = true` to refuse it); from the internet only HTTPS.
- Tailscale: nothing to set up; the panel's Tailscale name is in its
  certificate and address list.
- Open port: forward the panel's port (or 443 with ACME). Signing in from
  the internet needs two-factor sign-in (Access & devices).
- Rate limits, an audit log of changes and sign-ins, and API tokens
  (read-only or full, per server, expiring) in Access & devices.

Not yet: incremental and off-site backups, mods tab (see the roadmap).

## API (v1)

All under `/api/v1`. Browsers use the session cookie and must send
`X-Hopperd: 1` on non-GET requests; devices send `Authorization: Bearer <token>`.

| Route | |
|---|---|
| `GET /hello` | name, version, addresses, capabilities (no auth) |
| `POST /auth/setup` `{code, password}` | first owner password |
| `POST /auth/login` `{password, code}` / `POST /auth/logout` / `GET /auth/me` | `need_code` / `need_two_factor` in a 401 |
| `POST /pair/request` `{device}` → `{id, secret}` | device asks to pair (no auth); the code is `pair_code(CA fingerprint, id, secret)` on both sides |
| `POST /pair/status` `{id, secret}` → `waiting` / `approved {token}` / `denied` | device polls |
| `GET /pair/pending`, `POST /pair/{id}/answer` `{allow}` | owner |
| `GET /devices`, `DELETE /devices/{id}` | owner |
| `GET /host` | CPU, memory, disk |
| `GET /servers`, `GET /servers/{id}`, `POST /servers/rescan` | |
| `POST /servers/{id}/power` `{action: start\|stop\|restart\|kill}` | |
| `POST /servers/{id}/command` `{line}` | |
| `GET /servers/{id}/console?since=<n>` → `{lines, total}` | |
| `GET /fs/{id}/list?path=` | folder entries |
| `GET /fs/{id}/file?path=[&inline]` | download, supports `Range` |
| `GET /fs/{id}/zip?path=a&path=b[&name=]` | streamed zip (no path: the whole server) |
| `GET /fs/{id}/text?path=` / `PUT /fs/{id}/text` `{path, content, modified, force}` | editor; `saved` / `invalid` / `conflict` |
| `POST /fs/{id}/ops` `{ops: [{op: mkdir\|rename\|move\|copy\|trash, ...}]}` | |
| `POST /fs/{id}/uploads` `{path, size, overwrite}` → `{id}` | start an upload |
| `PUT /fs/{id}/uploads/{up}?offset=n` (raw body, ≤ 32 MB) | a chunk; 409 `{offset}` if out of step |
| `GET` / `DELETE /fs/{id}/uploads/{up}` | progress / cancel |
| `GET /fs/{id}/trash`, `POST .../trash/restore` `{id}`, `POST .../trash/purge` `{ids}` | |
| `GET /servers/infos` | every server as Hopper desktop describes one |
| `GET /servers/{id}/mods` | the server's mods, scanned on the panel |
| `GET /servers/{id}/crash`, `GET /servers/{id}/neoforge` | last crash, NeoForge version |
| `GET /fs/{id}/walk?path=`, `POST /fs/{id}/hashes`, `GET /fs/{id}/dirs?names=` | config compare |
| `POST /servers/{id}/pack/plan` `{inst, pack, cf_key}`, `POST /servers/{id}/pack/apply` `{plan, take}` | server pack update jobs |
| `GET /jobs/{job}`, `POST /jobs/{job}/cancel` | progress and result |
| `GET` / `PUT /servers/{id}/properties`, `GET` / `PUT /servers/{id}/jvm` | settings |
| `GET /servers/{id}/players`, `POST` `{action: op\|deop\|kick\|ban\|pardon\|whitelist_add\|whitelist_remove, name, reason}` | players |
| `GET` / `POST /servers/{id}/backups` `{worlds_only, note}`, `DELETE .../backups/{name}`, `GET .../entries`, `POST .../restore` `{paths}` | backups (jobs) |
| `GET` / `PUT /servers/{id}/schedule` | schedule |
| `POST /servers/create` `{name, mc_version, loader, loader_version, memory_mb, port, accept_eula}`, `GET /meta/minecraft`, `GET /meta/loaders/{mc}` | new servers (job) |
| `GET /auth/2fa`, `POST /auth/2fa/begin`, `/enable` `{code}`, `/disable` `{password, code}` | two-factor (owner) |
| `GET` / `POST /tokens` `{name, read_only, servers, days}`, `DELETE /tokens/{id}` | API tokens (owner) |
| `GET /audit?limit=` | audit log (owner) |
| `GET /ws` | live: `snapshot` every second; send `{"op":"console","server":id}` to follow a console |

## Develop

```
cd crates/hopperd/ui
pnpm install
pnpm build          # the UI the binary embeds
pnpm dev            # hot reload on :5173, API proxied to a local hopperd
cargo run -p hopperd -- --data ./hd-data --servers-root <folder>
```

Without `ui/dist`, the binary serves a short page saying how to build the UI;
the API works either way.
