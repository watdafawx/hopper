# Hopper Server (`hopperd`)

A standalone panel for Minecraft servers: a web UI in any browser, and the API
Hopper desktop uses to control the same servers. It runs on the machine that
hosts the servers. Hopper desktop is not needed to use it.

Status: **M1, LAN only.** Plan and roadmap: [design/server-panel.md](../../design/server-panel.md).

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

Not yet: TLS / internet access, files, backups, schedules, settings editor
(see the roadmap).

## API (v1)

All under `/api/v1`. Browsers use the session cookie and must send
`X-Hopperd: 1` on non-GET requests; devices send `Authorization: Bearer <token>`.

| Route | |
|---|---|
| `GET /hello` | name, version, addresses, capabilities (no auth) |
| `POST /auth/setup` `{code, password}` | first owner password |
| `POST /auth/login` `{password}` / `POST /auth/logout` / `GET /auth/me` | |
| `POST /pair/request` `{device}` → `{id, secret, code}` | device asks to pair (no auth) |
| `POST /pair/status` `{id, secret}` → `waiting` / `approved {token}` / `denied` | device polls |
| `GET /pair/pending`, `POST /pair/{id}/answer` `{allow}` | owner |
| `GET /devices`, `DELETE /devices/{id}` | owner |
| `GET /host` | CPU, memory, disk |
| `GET /servers`, `GET /servers/{id}`, `POST /servers/rescan` | |
| `POST /servers/{id}/power` `{action: start\|stop\|restart\|kill}` | |
| `POST /servers/{id}/command` `{line}` | |
| `GET /servers/{id}/console?since=<n>` → `{lines, total}` | |
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
