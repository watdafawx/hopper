# Hopper Server (`hopperd`)

A standalone panel for Minecraft servers: a web UI in any browser, and the API
Hopper desktop uses to control the same servers. It runs on the machine that
hosts the servers. Hopper desktop is not needed to use it.

Status: **M6 (v1): servers, console, files (history, search, SFTP), mods, players, backups, schedules, settings, new servers; people with roles, webhooks, MCP; LAN, Tailscale and internet (HTTPS, two-factor); Windows service, systemd and Docker. 0.2.0: server addresses with sleeping servers. 0.4.0: servers other panels know run in place. 0.5.0: mods like Hopper desktop. 0.6.0: files explorer and settings pages. 0.7.0: Mods table like Hopper desktop. 0.8.0: mod profiles and finding the mod that breaks a server. 0.9.0: panel folder, importing and deleting servers. 0.10.0: double-click on Windows (portable, start with Windows or service, tray), Docker image on GHCR. 0.11.0: Browse and pack updates like Hopper desktop, worlds, alerts, deduplicated and off-site backups, copying servers, changing versions.** Plan and roadmap: [design/server-panel.md](../../design/server-panel.md).

## Run it

**Windows:** double-click `hopperd.exe`. The first time, it asks how to run:

- **Just run it**: runs from where it is, as you, with a tray icon. It
  stops when you quit or sign out.
- **Start with Windows**: the same, and it starts again when you sign in
  (your own startup entry, no admin).
- **Install as a service**: copies itself to `C:\Program Files\Hopper Server`
  and installs the `hopperd` Windows service (one admin prompt). It runs
  without anyone signed in and restarts after a crash.

The browser then opens the panel. The tray icon's menu has Open panel,
Copy address for other devices, Open panel folder, Start with Windows,
Install as a service… and Quit (it asks before leaving servers running).
Running `hopperd.exe` again while it runs offers Open panel, Update (when
the file is newer) and Uninstall instead of a second copy. From a
terminal, or with `--portable`, it just runs, as on Linux:

```
hopperd
```

- First start: on the server PC itself, open `http://localhost:8790` and
  set the owner password; no code needed. From another device you need the
  one-time **setup code** from the log (also in `setup-code.txt` in the data
  folder). Then pick the **panel folder**: servers go in its
  `servers` folder and the panel's own data in `.hopper-server`, and the
  panel never shows anything outside it. `--home <folder>` sets it from the
  command line.
- Every folder in the servers folder with a `server.properties` is a server.
- The panel listens on port **8790** (`--port` to change). Open
  `http://<this-machine>:8790` from any device on the LAN.
- Before a panel folder is picked, settings and accounts are in the data folder: `%APPDATA%\hopperd\data` on
  Windows, `~/.local/share/hopperd` on Linux, or `--data <dir>`.
  `hopperd.toml` holds the port, bind address, servers folder and extra
  server folders.
- Servers keep running when hopperd stops or restarts; it reconnects to
  them on start (Windows; Linux follows).
- Folders Crafty or MCSS manage are listed but hopperd won't start them.
- Setting up without the code trusts only a direct connection from
  127.0.0.1/::1 with a `localhost` Host and no proxy headers, and only until
  there's an owner. Anyone or any program on that PC can claim the panel in
  that window. A reverse proxy or tunnel on the same PC that strips
  `X-Forwarded-For` and rewrites Host to localhost would let the internet
  through too: set `trust_local_setup = false` in `hopperd.toml` if you run
  one. It's always off in Docker.

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

Incremental and off-site backups: see 0.11.0 below.

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

## Worlds, alerts, backups and copies (0.11.0)

- **Backups** are deduplicated: files are cut into chunks stored once, so a second backup of an unchanged server takes seconds and almost no space. Older zip backups still list, browse and restore, and **Convert** turns one into the new kind. Keep rules (newest N, daily, weekly) apply to scheduled backups.
- **Off-site copies:** every backup can also go, encrypted, to a folder or network share, an SFTP server, S3-compatible storage (AWS, B2, R2, MinIO) or another Hopper Server (Access & devices). Keep the recovery key: a backup can be brought back from a copy even after the server's own is gone.
- **Worlds tab:** every world with its size, seed and last played; new worlds, switching the active one, upload, download, reset (same seed, new seed or a chosen one, a backup first, the old folder to the server's trash) and pre-generating with Chunky.
- **Alerts:** a crash (also when the server was restarted after it), low TPS (read over RCON, which the panel can turn on), a nearly full drive or the PC nearly out of memory. They show on the server's page and the Overview, and go to webhooks.
- **Copy a server** (Settings › General): a clone with or without its world on its own port, or a template (mods, configs and start files) that New server can start from.
- **Change version** (Settings › Version): another Minecraft or loader version for a stopped server, with a full backup first, a check of what the server starts with, and one-step Undo.
- **Share a log or crash report** on mclo.gs, after a preview of what's hidden (IP addresses, tokens, the Windows user name).

## Browse, adding content and pack updates

- **Browse** works like Hopper desktop's: a Type list (Modpacks, Mods, Data packs), **Install into** a server (sets the Minecraft and loader filters from it), categories and sorts, an Installed chip on what the server already has, and a project page with its description, versions, changelogs and Install / Install this version. Mods come with their dependencies; data packs go in a world's `datapacks` folder (or a global one). The Mods tab's **Add content** opens it for that server.
- **Pack updates:** a server made from a CurseForge or Modrinth pack shows "Pack update 1.2.0 → 1.3.0" next to its status, and a dot in the sidebar. The review shows the changelog of every version in between, then Compare downloads the new server pack and lists what changes (loader, mods added / updated / removed, config files, with a choice per file both you and the pack changed). Update runs with the server stopped; replaced files go to `.hopper-backups`. In-place updates need a CurseForge server pack; other packs get the notice and changelogs.
- The Mods tab draws only the rows on screen, so 1000+ mod packs scroll and filter without lag.
- API: `GET browse/target`, `POST browse/install`, `browse/search|categories|pack|versions?type=mods|datapacks&into=<id>`, `GET servers/{id}/pack/check`, `GET pack-updates`, `GET servers/{id}/pack/diff`, and `pack/plan` / `pack/apply` also take `{file}` / `{plan_id, take}` from the web.

## Panel folder, importing and deleting servers (0.9.0)

- Setup asks for the panel folder (older panels ask the owner on sign-in, "Not now" skips). On the next start the panel moves its data into `<folder>\.hopper-server`; the start-up data folder keeps only a pointer (`home = ...`). Picking it restarts the panel (exit code 75; the Windows service and systemd start it again).
- Folder pickers, scans and imports refuse paths outside the panel folder. Servers already listed outside it keep running, marked "Outside the panel folder".
- Panel folder page (sidebar): Move… renames the whole folder at the next start (same drive; servers in it stopped), and the servers folder inside it can be changed: what's in a picked folder is listed to add (server folders run where they are, server pack and client pack zips become servers).
- New server › Start from: Fresh, Folder (run where it is or copy in), Server pack zip (upload or a zip in the panel folder; Minecraft and loader read from variables.txt, a ServerStarter config, the installer jar or libraries), CurseForge / Modrinth (server files when the pack has them, else built from the client pack with client-only mods left out).
- Settings › General › Remove this server: Remove from panel (folder kept), or Delete (type the name; the folder moves to `.hopper-deleted` in the servers folder for 30 days, restore or delete for good from the Panel folder page). Servers that run where they are can only be removed from the panel.
- API: `GET host/browse`, `GET host/scan`, `PUT settings/panel-folder`, `POST settings/panel-folder/move`, `POST panel/restart`, `GET|PUT settings/servers-folder`, `POST servers/import-folder`, `incoming/uploads` (chunked, like file uploads), `GET servers/zip-info`, `POST servers/import-zip`, `GET packs/search`, `GET packs/versions`, `POST servers/create-from-pack`, `POST servers/{id}/remove`, `PUT deleted/{name}/restore`, `DELETE deleted/{name}`.

## Mod profiles and finding the mod that breaks it (0.8.0)

- Profiles (Mods toolbar): switch to a saved set of mods that are off, save the current one, delete one. Each shows what switching would turn on and off.
- Find the mod that breaks it: hopper-core's `bisect` rounds (half the suspects off, always with what needs them), with the strip, row chips and answers from Hopper desktop. The On switches wait until the hunt ends.
- "Run rounds for me" (when it won't start or crashes): hopperd stops the server, parks the round's mods, starts it, and answers by itself: reaching "Done" works, a crash, an exit or 10 minutes is still broken. Warns when players are online. Pause / resume; after a hopperd restart the rounds show as paused.
- When the loader refuses a round because a mod needs one that's parked ("Mod X requires Y"), the hunt learns that pair and plays the round again instead of taking the answer.
- Rounds play on a throwaway world by default (`level-name` is switched to `hopper-hunt-world` and put back, and that world deleted, when the hunt ends), so the real world never saves with mods missing.
- Schedules (crash restarts, scheduled restarts, autostart) skip a server while a hunt runs.
- Files: `hopper-bisect.json` (core's journal) and `hopper-bisect-server.json` (auto rounds, log, world) in the server folder.
- API: `GET mods/profiles`, `POST mods/profiles/save|use|delete` `{name}`; `GET mods/hunt`, `POST mods/hunt/start` `{kind, auto, scratch_world}` (job), `.../answer` `{answer}` (job), `.../undo` (job), `.../fine` `{file, fine}` (job), `.../finish` `{keep_off}` (job), `.../auto` `{run}`. Capabilities `mods.profiles`, `mods.hunt`.

## Mods table like Hopper desktop (0.7.0)

- The Mods tab is desktop's table: Name with tags, Update / What changed (or Changelog on hover), Author, Side, On and Added columns that sort, Updates first and Only yours.
- Versions and changelogs (dialog and side panel): every version from the installed one to the newest with its changelog, Update to newest, Reinstall, Reset config, and older versions to roll back to.
- Review updates groups Patch / Major / Alpha-beta; updates whose new file would stop the server loading start unticked.
- Checkboxes (shift for a range) and a bulk bar: on, off, update, ignore / allow updates, remove. Right-click or ⋯ for the row menu, including switching between CurseForge and Modrinth.
- Turning off or removing a mod others need asks first. Mods in twice get "Keep newest".
- The update check is kept in the data folder (`update-checks.json`) across restarts, and one update no longer re-checks the whole server.
- API: `GET mods/changelogs?file=`, `POST mods/related`, `mods/toggle-many`, `mods/remove-many`, `mods/lock-many`, `mods/switch` (job).

## Files explorer and Settings pages (0.6.0)

- Files works like a file explorer: Back / Forward / Up (also Alt+arrows and the mouse side buttons), a path you can type, a folder tree, list or tiles with image thumbnails, and a preview pane (text, images, a jar's mod name and contents).
- Double-click opens by type: text in the editor, images in a viewer, zips and jars browsed like folders (nothing is unpacked), anything else downloads.
- Right-click menu, Ctrl+X / C / V across folders, drag onto a folder, the tree or the path to move (Ctrl copies), drop files from your PC onto a folder to upload into it, drag on empty space to select. Chrome and Edge can drag a file out to the desktop.
- Settings is one page per section (General, Players & access, World, Network & address, Memory & Java, All properties), one column each, with search across all pages and a server icon picker.

## Mods like Hopper desktop (0.5.0)

- Mods tab: unknown jars are identified on CurseForge and Modrinth on their own (names, icons, links), and updates are checked on both (Modrinth needs no key). Filter chips: On, Off, Only yours, Client-only, Updates, Unknown.
- Click a mod for its details: versions for the server's Minecraft and loader with changelogs, update / install another / roll back, lock (updates skip it), reset its config, remove.
- "Review N updates" lists every update with its changelog; untick what to skip.
- Mods › Backups shows `.hopper-backups` (from Hopper desktop or the panel): restore a jar (the current one of the same mod is set aside first) or delete old backups.
- Settings has sections (General, Players, World, Network, Address, Memory & Java, Other keys) with a list to jump between them.

## Servers other panels know (0.4.0)

Any server folder runs here in place, also ones Crafty or MCSS know: their
files are left alone and nothing in the folder is rewritten, so you can
keep moving between panels. Such servers show "Also in Crafty" (or MCSS).

- Starting one that's already running elsewhere is refused: its port is in
  use, or its world's `session.lock` is held.
- The start script is `run.bat`, else `start.bat`, else the server jar.
  When it says plain `java` (whatever Java is first on PATH, which changes
  when a new one is installed), the installed Java that fits the server's
  Minecraft version is used, and the console says so.

## Install as a service

On Windows the first-run dialog or the tray does this for you. By hand,
from an admin terminal (Windows) or with sudo (Linux):

```
hopperd service install [--data <dir>] [--copy]
hopperd service status
hopperd service update        (Windows: put this exe in the service's place)
hopperd service uninstall [--remove-files]
```

Windows: a service that starts with the machine (as LocalSystem; to run
as a user, change Log On in services.msc). `--copy` copies the exe to
`C:\Program Files\Hopper Server` first; `--remove-files` deletes that copy.
Logs go to `hopperd.log` in the data folder. `install` prints where to
open the panel (the port from the panel folder's settings, once there is
one). Linux: a systemd unit running as the user behind sudo, with
`KillMode=process` so servers survive restarts. On Linux hopperd starts
servers but doesn't yet reattach to them after it restarts.

Releases: pushing a `hopperd-v<version>` tag builds Windows and Linux
downloads and the Docker image.

## Docker

The image (amd64 and arm64) has Java 8, 17 and 21, so it runs servers for
every Minecraft version (1.16 and older need 8, 1.17 to 1.20.4 need 17,
1.20.5 and newer need 21). Everything lives in one volume, `/hopper`, the
panel folder: servers in `/hopper/servers`, the panel's data (logins,
certificates, settings) in `/hopper/.hopper-server`. Setup doesn't ask for
a panel folder in the container.

```
docker run -d --name hopperd --hostname hopper --restart unless-stopped --stop-timeout 120 \
  -p 8790:8790 -p 8022:8022 -p 25565:25565 \
  -v hopperd:/hopper ghcr.io/watdafawx/hopperd:latest
docker logs hopperd
```

Open `http://<host>:8790` and use the setup code from the log. Ports:

| Port | |
|---|---|
| 8790 | the panel (web UI and API) |
| 8022 | SFTP |
| 25565 | server addresses: every server joined by its address on one port |
| each server's own port | add `-p 25566:25566` and so on for servers joined by port |

- LAN discovery (Hopper desktop finding the panel by itself) needs
  `--network host` (Linux hosts) instead of `-p`.
- `docker stop` stops the panel, then the servers (they save their worlds),
  so give it time: `--stop-timeout 120`.
- A folder instead of a volume works too: `-v /srv/hopper:/hopper`. Servers
  you already have go in its `servers` folder.
- Or with Compose: `crates/hopperd/packaging/docker-compose.yml`.

Build it yourself, from the repository root:
`docker build -f crates/hopperd/Dockerfile -t hopperd .`

**From the older image (`/data` and `/servers`).** A container started with
those two volumes keeps working as before: the image notices `/data` and
runs the old way. To move to `/hopper` (one volume, no panel folder step):

1. Make a backup of both, then `docker stop hopperd`.
2. Copy the servers into the new volume's `servers` folder and the panel's
   data into `.hopper-server`:
   ```
   docker run --rm -v hopperd-data:/data -v /srv/minecraft:/servers -v hopperd:/hopper ubuntu:24.04 \
     sh -c 'mkdir -p /hopper/servers /hopper/.hopper-server && cp -a /servers/. /hopper/servers/ && cp -a /data/. /hopper/.hopper-server/ && sed -i "s#\"/servers#\"/hopper/servers#g" /hopper/.hopper-server/hopperd.toml'
   ```
3. Start the new container with only `-v hopperd:/hopper` (as above) and
   check the servers are listed, then remove the old volumes.

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
| `GET /servers/{id}/mods/versions?file=`, `GET .../mods/changelog?project=&file=`, `POST .../mods/version` `{file, file_id}` (job), `POST .../mods/lock` `{file, locked}`, `GET .../mods/config?file=`, `POST .../mods/config-reset` `{file}` | a mod's details |
| `GET /fs/{id}/zip-list?path=&inner=`, `GET /fs/{id}/zip-entry?path=&entry=[&inline=1]` | inside a zip or jar |
| `GET /servers/{id}/mod-backups`, `POST .../mod-backups/restore` `{when, name?}`, `POST .../mod-backups/delete` `{when}` | `.hopper-backups` |
| `GET` / `PUT /settings/curseforge` `{key}` | CurseForge key (set: owner) |
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
