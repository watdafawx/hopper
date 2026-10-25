# Hopper Server (`hopperd`)

A standalone panel for Minecraft servers: a web UI in any browser, and the API
Hopper desktop uses to control the same servers. It runs on the machine that
hosts the servers. Hopper desktop is not needed to use it.

Status: **M6 (v1): servers, console, files (history, search, SFTP), mods, players, backups, schedules, settings, new servers; people with roles, webhooks, MCP; LAN, Tailscale and internet (HTTPS, two-factor); Windows service, systemd and Docker. 0.2.0: server addresses with sleeping servers. 0.3.0: taking servers over from Crafty.** Plan and roadmap: [design/server-panel.md](../../design/server-panel.md).

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

Not yet: incremental and off-site backups (see the roadmap).

## v1 extras (M6)

- **Mods tab:** the server is managed like an instance (`hopper.json` in
  its folder): identify jars on CurseForge and Modrinth, add mods from
  either (with dependencies), turn them on and off, remove, and update the
  ones added on top of the pack. Pack mods update from the pack's server
  files. CurseForge needs a key (`curseforge_key`, or the Mods tab).
- **File history and search:** the editor's History shows earlier
  versions side by side with the current file and can load one back. The
  file manager's filter box searches inside files on Enter (plain text or
  a regular expression).
- **People:** the owner adds people with a name, a password and a role:
  admin (everything on their servers), operator (power, console, players,
  backups) or viewer (looking only), limited to chosen servers. Everyone
  can change their password and set up their own two-factor sign-in.
- **Webhooks:** Discord or any URL (JSON) for start, stop, crash, join,
  leave, backups, schedules and failures. Access & devices.
- **MCP:** `/api/v1/mcp` (Streamable HTTP) for AI assistants, with an API
  token. Tools: servers, console, power, commands, files (list, read,
  write, search), mods, last crash, backups. Read-only tokens only look.
- **SFTP** on port 8022 (`sftp_port`, 0 = off): `/` lists your servers.
  Your name and password from the LAN or Tailscale, or an API token as
  the password from anywhere. Saved-over files keep their old version and
  deleted ones go to the trash.

## Server addresses (0.2.0)

Players join `survival.mc.example.com` instead of an IP and port. The panel
listens on one Minecraft port (25565 by default) for every server, reads
the address the player typed and passes them to that server's own port.

- Set a server's addresses in its **Settings › Address** card. With a domain
  set (Access & devices › Server addresses), you type only the name.
  `*.example.com` wildcards work; an exact address wins over a wildcard.
- One DNS record covers every server: `A *.mc.example.com → your public IP`.
  "Check DNS and port" shows the record and tests both from outside.
- A server can't use the router's port itself; **Move it** gives it a free one.
- **Sleep when empty:** a server with nobody on for N minutes is stopped.
  The server list then shows "Sleeping · join to wake it", and joining
  starts it (the player is told how long it usually takes). Webhooks:
  `wake`, `sleep`.
- Stopped, starting and crashed servers answer in the server list instead
  of "Can't connect"; an address no server has says so (or goes to a server
  you pick).
- Settings: `[router]` in `hopperd.toml` (`enabled`, `port`, `domain`,
  `unknown_server`); per server in `.hopper-route.json` in its folder.
- Servers see players as coming from 127.0.0.1 (no proxy forwarding yet).

## Taking servers over from Crafty (0.3.0)

Servers Crafty Controller manages are listed with "Run by Crafty", and
hopperd won't start them. To move one over:

- Stop it in Crafty, then open it here: **Take over…** in the strip under
  its name. The dialog shows what Hopper will use before anything changes.
- Its `run.bat` gets Crafty's exact start command (memory, arguments, its
  Java); an old `run.bat` is kept as `run.bat.before-hopper`. Crafty's
  `crafty_managed.txt` is set aside. Nothing in the world or mods changes.
- Where Crafty's command says plain `java` (whatever Java is first on PATH,
  which changes when a new one is installed), the right installed Java for
  the server's Minecraft version is written in instead.
- Crafty's settings are read from a copy of its database
  (`crafty\app\config\db\crafty.sqlite` at the top of a drive or in
  Program Files; `CRAFTY_DIR` to point elsewhere). Hopper never writes to
  Crafty.
- Refused while Crafty runs the server (its port is in use or its world is
  locked). Remove the server from Crafty afterwards, or never start it
  there, so both panels don't run it.
- **Give back to Crafty** (Settings) puts the old run.bat and the marker
  back.

## Install as a service

From an admin terminal (Windows) or with sudo (Linux):

```
hopperd service install [--data <dir>]
hopperd service status
hopperd service uninstall
```

Windows: a service that starts with the machine (as LocalSystem; to run
as a user, change Log On in services.msc). Logs go to `hopperd.log` in the
data folder. Linux: a systemd unit running as the user behind sudo, with
`KillMode=process` so servers survive restarts. On Linux hopperd starts
servers but doesn't yet reattach to them after it restarts.

Docker (Java 21 included), from the repository root:

```
docker build -f crates/hopperd/Dockerfile -t hopperd .
docker run -d -p 8790:8790 -p 8022:8022 -p 25565:25565 -v hopperd-data:/data -v /srv/minecraft:/servers hopperd
```

or `crates/hopperd/packaging/docker-compose.yml`. Releases: pushing a
`hopperd-v<version>` tag builds Windows and Linux downloads.

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
| `POST /auth/password` `{current, new}` | your own password |
| `GET` / `POST /users` `{name, password, role, servers}`, `PUT` / `DELETE /users/{id}`, `POST /users/{id}/reset-2fa` | people (owner); roles `admin`, `operator`, `viewer` |
| `GET` / `POST /webhooks` `{name, url, events, servers}`, `PUT` / `DELETE /webhooks/{id}`, `POST /webhooks/{id}/test` | webhooks (owner) |
| `GET /servers/{id}/mods/list`, `PUT .../mods/meta`, `POST .../mods/identify` (job), `.../toggle` `{file, enabled}`, `.../remove` `{file}`, `GET .../mods/search?q=&source=`, `POST .../mods/install` `{project_id}` (job), `GET .../mods/updates`, `POST .../mods/update` `{project_ids}` (job) | the server as an instance |
| `PUT /settings/curseforge` `{key}` | CurseForge key (owner) |
| `GET /fs/{id}/history?path=`, `GET /fs/{id}/diff?path=&version=` | earlier versions, line diff against now |
| `GET /fs/{id}/search?q=&path=[&regex=1]` → `{hits: [{path, line, text}], more}` | text in files and names |
| `GET` / `PUT /router` `{enabled, port, domain, unknown_server}`, `POST /router/check` | server addresses: settings (owner), DNS and port check |
| `GET` / `PUT /servers/{id}/address` `{addresses, sleep_after_minutes}`, `POST .../address/move-port` | a server's addresses and sleep |
| `GET` / `POST /servers/{id}/takeover`, `POST /servers/{id}/give-back` | take a Crafty server over (`GET` shows the plan: Java, memory), give it back |
| `POST /mcp` | Model Context Protocol (JSON-RPC) |
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
