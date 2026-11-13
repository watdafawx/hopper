# Passwords in a server's files (Hopper Server)

`server.properties` holds the RCON password (`rcon.password`) and, on newer
Minecraft, the management server's secret (`management-server-secret`,
`management-server-tls-keystore-password`). Whoever has them can run any
command on the server without the panel, from anywhere the RCON or
management port reaches.

## Who sees them

Only people who may change the server's files: they could set a new
password anyway. That is `Who::sees_secrets()`:

| Who | Sees secrets |
|---|---|
| Owner, Manager, Admin (on their servers) | yes |
| A paired PC (Hopper desktop) | yes (it may change files) |
| An API token that may write | yes |
| A read-only API token | no |
| Operator, Viewer | no |
| A node request from the main | as its role after the ceiling: Manage (Admin) yes, Control and Look no |

A key is secret when hopper-core's shared list says so
(`configedit::secret_key`: password, secret, token, ...; the same list
config files are masked with), so a later Minecraft key named like these is
covered too.

## What the others get (`crates/hopperd/src/secrets.rs`)

- `GET /servers/{id}/properties`: the settings without the secret keys.
- `GET /servers/infos`: never the secret keys, for anyone. Hopper desktop
  reads players, the world name and the message there; nothing uses a
  password from it. A main also drops them from a node's answer, for
  nodes of an older version.
- The file itself is refused, as are Hopper's own folders (the trash keeps
  saved-over copies, the backups folder whole servers) and whole backups:
  `fs/{id}/file|text|diff|history|zip-list|zip-entry|zip` with such a path,
  `fs/{id}/version-text` (names a saved-over copy by id only), and
  `servers/{id}/backups/{name}/download`. One check in the guard
  (`security::secret_refusal`) for direct and relayed requests; one-use
  links, MCP's `read_file` and SFTP use `secrets::link_refusal`. A path is
  checked as written and as the file it really opens (trailing dots and
  spaces, `./`, other case and 8.3 short names all name server.properties
  on Windows).
- A zip of a folder leaves server.properties and Hopper's folders out; a
  search inside files (web UI and MCP) leaves their hits out.

Saving never wipes a hidden secret: `PUT /properties` changes only the keys
it's sent, so a key left out keeps its value. Nobody who is shown the
settings without the secrets may save them (Viewer and Operator can't change
properties, read-only tokens can't write), so no placeholder is ever sent
back.

## Security review (2026-11-13)

| # | Rank | Finding | Reachable before, by whom | Status |
|---|---|---|---|---|
| 1 | High | `rcon.password` and the management secret in `/servers/infos` and `/servers/{id}/properties`. | Yes: any signed-in person on their servers (Viewer, Operator, read-only token) and, via the main, at every ceiling (the main asks a node's properties as a Viewer). | Fixed as above. |
| 2 | High | The same passwords as a file: `fs/{id}/text` and `file` of server.properties, its saved-over copies in `.hopper-trash` (also by `version-text`), a whole-server zip, file search, backup downloads (and `.hopper-server-backups/*.zip` through `fs/file`), SFTP, MCP `read_file` / `search_files`, one-use links. | Yes: the same people (all of these are reads). | Fixed as above. |
| 3 | — | Per-server access for users limited to some servers. | No: the guard (`security::refusal`) refuses every `/servers/{id}/...` and `/fs/{id}/...` route (encoded `node:id` too) for a server the user or token doesn't have, before any handler runs, for direct and relayed requests, links and MCP. Routes that name a server in the body or query (`browse/*`, `browse/install`, `pack-updates`) check `who.sees`; `rules`, `jobs`, `deleted`, `incoming` are owner-only. Handlers in `manage_api`, `fs_api`, `fs_zip` and `parity_files` now check `who.sees` too, so a route can't lose it if the guard changes. | Sound; tested. |
| 4 | Low | Operator role writes: only `power`, `command`, `players`, `backups` (making one). An Operator may op people and run any console command, which is more than RCON gives; by design. | — | Listed. |
| 5 | Low | Folder listings and `walk` still show that server.properties exists (name, size, time) to Viewers. | — | Listed (no content). |

Tests: `secrets::tests` (keys, paths, the guard's rule), `secrets_tests`
(a Viewer, an Operator, a read-only token and a one-use link get no secret;
an Admin and a writing token see it; saving without it keeps it; a limited
Admin and a scoped token reach none of another server's routes), and
`relay_tests::a_look_node_sends_no_secret`.
