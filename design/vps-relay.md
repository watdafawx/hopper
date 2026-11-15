# Relay through a VPS (Hopper Server, desktop parity) (draft)

Status: design only (phase 1 of an L feature). Mock:
`design/mocks/vps-relay.html` (2 or 3 options per screen, picks marked,
light and dark), also published as a Design Artifact (one artboard per
screen). B1 (the codec) is built; the rest is design. Decisions from
2026-11-14 are in "Decisions" below and override the text where they
differ.

Noticed while mocking, not fixed here: Hopper Light's status colours on
their own tints miss 4.5:1 (green `#16a34a` on its tint 2.7:1, red 3.6:1,
blue 3.3:1, accent text `#ea580c` on white 3.6:1). The mock uses darker
text shades in light mode (`#166534`, `#b91c1c`, `#0369a1`, `#9a3412`).
Worth its own change to `lib/themes.json` and the desktop theme.

Words used below: **home** is the PC that runs the Minecraft server and
Hopper Server (hopperd). **VPS** is a rented or free cloud machine with a
public address. **Relay** is everything on the VPS that takes the players
and passes them home. **Proxy** is Velocity.

## Why

A home line's upload is the limit for a big modpack: a player costs about
1.1 Mbit/s of upload at view distance 8, and a home line gives 4 to 5.
Players also need a port forward and a home address they can reach.

A hand-built setup outside Hopper (2026-11-14) proved the idea with one
big pack (ATM10). It is the source of the reference measurements below,
not something this feature is built around: the feature is for any
Hopper Server user, any pack size, any loader and any Debian or Ubuntu
VPS provider.

- Players join a VPS running Velocity 3.5.1 (modern forwarding). The home
  server runs Proxy Compatible Forge (PCF) 1.3.1.
- Home dials out to the VPS over **one** TCP connection (no port forward).
  Everything inside is compressed as one stream with a big window, plus a
  piece cache mirrored on both ends, so what two players both receive is
  sent once.
- Measured: 2 players, 19.7 min: vanilla would have sent 415.7 MB, the
  tunnel sent 30.4 MB (about 14x less upload). Standing still: 3 to 4x less.
- Voice chat (Simple Voice Chat, UDP) goes through the VPS too.
- The tunnel ends at hopperd's router (127.0.0.1:25565), so start, stop,
  console, History and wake-on-join keep working.

This feature builds that into Hopper Server, so a player clicks through a
setup instead of following notes.

## Goals

1. **Relay through a VPS**, from the server page: give an ssh login, Hopper
   checks the VPS, installs what it needs, changes the server, and keeps
   the connection up (starts with hopperd, reconnects by itself).
2. **Voice chat** through the same VPS.
3. **Relay status** on the server page: connected or not, upload saved
   against vanilla, cache hits, the address to give players.
4. **Real ping** in the Players tab: player to VPS (and VPS to home), not
   the tunnel's local round trip, which is always about 0 ms.
5. **Plain wording** where a setting means something else behind a proxy
   ("Online mode: Off", accounts are checked by the relay).
6. **Several servers behind one VPS**, each with its own address.
7. **Undo**: every change to the server's files is in its History, and
   "Stop using the relay" puts the server back the way it was.
8. **Desktop parity**: a server on a paired Hopper Server shows the same
   status and ping in Hopper desktop.

## Non-goals (for now)

- Creating the VPS. The player makes it at their cloud (Oracle, Hetzner...)
  and opens the ports in its firewall page; Hopper says which ports and
  checks them from outside afterwards. Cloud APIs are a later idea.
- Running game servers on the VPS (that is a node, `design/multi-panel.md`).
- Proxies other than Velocity, plugins on Velocity, BungeeCord.
- Old Forge backends without a modern-forwarding mod in the first
  version. (Fabric is in scope: see Decisions.)
- Wire compatibility with the existing `mctunnel` Go tool (not needed: both
  ends are Hopper).
- Load balancing, several VPSes for one server.
- Automatic backups before changing the server (AGENTS.md: none).

## Architecture

```
 player ──25565/tcp──▶ VPS: hopperd (relay role)
                         │  front: reads handshake + Login Start name,
                         │  takes the player's ping (TCP_INFO), adds a
                         │  PROXY v2 header
                         ▼
                       Velocity (127.0.0.1:25577, supervised by hopperd)
                         │  forced hosts → one loopback port per server
                         ▼
                       hopperd relay listener (127.0.0.1:2558x = server id)
                         │
                ══ one TLS connection, home dialled out to VPS:25566 ══
                   frames (mux, credit windows) → dedupe → zstd stream
                         │
 home: hopperd ◀─────────┘  each stream is tagged with its server id and
                            handed to the router in-process: wake-on-join,
                            "starting" answers, History and console as now
                         ▼
                       server (127.0.0.1:<own port>, PCF, online-mode=false)

 voice: player ──24454/udp──▶ VPS hopperd ══ UDP flow home dialled out ══▶ home hopperd ──▶ 127.0.0.1:24454
```

Pieces:

| Piece | Where | Runs as | Notes |
|---|---|---|---|
| hopperd, relay role | VPS | system user `hopper-relay`, systemd `hopper-relay.service` | No web UI (or loopback only), no servers. Supervises Velocity like a server process. |
| Velocity 3.5.x | VPS | child of hopperd, same user | Java 21. Version and sha256 pinned in a Hopper release. |
| Relay client | home, inside hopperd | hopperd | Started with hopperd, reconnects (A1's backoff). |
| PCF jar + config | home, server's `mods/` and `config/` | | Installed like any mod, recorded in History. |

### Codec and transport

- **Frames and streams**: reuse `crates/hopperd/src/tunnel/` (A0's frame
  types, `mux.rs` with credit windows and the 64-stream cap). One stream per
  player connection, `OPEN` head `{server, kind: "game"|"status", player?}`.
  A control stream carries stats, ping readings, Velocity state and log
  lines.
- **Transport**: raw TLS on one VPS port (ALPN `hopper-relay`), not a
  WebSocket: the VPS port is ours, so the WebSocket layer would only cost
  framing. Frames get a 4-byte length prefix on the byte stream.
- **Compression** sits between the mux and TLS, one stream per direction,
  so every player's data compresses against every other's:
  `frames → dedupe (content-defined pieces, mirrored cache) → zstd
  (long window, flushed about 1 ms after a burst) → TLS`. A port of the Go
  `dedupe.go` (the maintainer's own code, so no licence question) and the
  measured settings (klauspost's "best", about libzstd level 11, 256 MB
  window, 1 GB cache) as the starting point, tuned by replaying recordings.
- **Cache state** belongs to one connection. A reconnect starts both caches
  empty, so the two sides can never disagree after a drop.
- **Server tag**: the stream already says which server it is for, so home
  hands it straight to that server's router entry. No hostname matching at
  home (see "Several servers").

## The big decision: port mctunnel to Rust, or ship the Go binary

**Recommendation: port it to Rust, inside Hopper.** The codec (dedupe,
zstd framing, counters) goes in `hopper-core` (`hosting/relay_codec.rs`,
shared with desktop later), the transport and relay role in `hopperd`,
reusing the node tunnel. The VPS end is hopperd itself in a new *relay*
role.

| | Rust in Hopper | Ship the Go `mctunnel` |
|---|---|---|
| Builds and releases | Already built for Windows and Linux x64 + arm64 by `hopperd-release.yml`; nothing new | A second toolchain (Go 1.27) in CI and both release workflows, 4 more binaries, signing, update path |
| Reuse | Mux with credit windows, TLS with CA pinning, join tokens, reconnect backoff, signed self-update, router, TCP_INFO ping: all exist | Duplicates mux (yamux), auth (own HMAC handshake), reconnect |
| Into the router | In-process, tagged with the server id: wake-on-join, per-server routing and pings work | A loopback TCP hop into the router; the router sees 127.0.0.1 and has to guess the server from the hostname |
| Supervision | One process, one log, one set of counters, status straight into the API | Spawn, watch, parse `stats.csv` and log lines, pass a secret file |
| VPS end | hopperd: updates itself from the same signed releases, supervises Velocity, answers pings while home is down | A bare tunnel; Velocity and updates need systemd units and ssh for every change |
| Licences | `zstd` crate (BSD/MIT, wraps libzstd, BSD), our own dedupe, ring/rustls (already in the tree) | Fine too (yamux MPL-2.0, klauspost BSD), but MPL notices to ship |
| Effort | Codec port about 400 lines + tests, transport mostly reuse | Small up front, more forever |
| Risk | Must re-measure: libzstd vs klauspost compress differently | None for the codec; integration risk instead |

Why hopperd on the VPS and not a smaller binary: the relay end has to
supervise Velocity, update it and roll it back, answer status pings when
home is offline, measure player ping and relay voice. hopperd already does
process supervision, signed self-update (`autoupdate.rs`) and ships for
linux-arm64 (Oracle's free A1 machines). A relay role is one mode flag
(`hopperd relay`, like `--node`), not a new program.

**Does it fit nodes?** Partly. A node dials *its main*; here home must dial
*the VPS* (home has no open port), so the direction is reversed and the
VPS runs no servers. What carries over: the frame and mux code, the token
model (VPS keeps only a hash), the CA pin (home pins the relay's CA, learnt
over ssh, so no 6-digit code is needed), version skew (tunnel N and N-1,
"Update needed"), and updating the far end from home via its own
signed-release path. The relay shows on the server page and in Settings ›
Relays, not in Settings › Nodes. A later step could let a relay VPS also
be a node (show its CPU and memory); not in this plan.

Crate choices:

- zstd: **`zstd` 0.14** (BSD-3-Clause; `zstd-safe` BSD-3, `zstd-sys` and
  libzstd 1.5.7 BSD-3/BSD-2, all MIT-compatible). `zstd-sys` builds
  libzstd from its bundled source with `cc` (MSVC on Windows, gcc on the
  native Linux x64 and arm64 runners), so no system library or new CI step;
  default features off. `window_log` up to 28 for 256 MB, `ZSTD_e_flush`
  per burst. `ruzstd` (pure Rust) has no strong encoder; rejected.
- Mux: our own (`tunnel/mux.rs`). `yamux` crate rejected (a second mux).
- Encryption: **TLS 1.3 with rustls/ring** (already used), not a custom
  AES-GCM handshake. A custom HMAC handshake is something to review forever.
- ssh: `russh` 0.64 (already in hopper-core for SFTP).

Note on compressing inside TLS: compression before encryption can leak a
secret when an attacker's data and the secret share a stream (CRIME). The
tunnel carries game traffic after Velocity has finished the player's login
encryption; the only secret in it is the forwarding HMAC, which is a
signature over data the server already knows, not a reusable token. Low
risk; listed for the security review anyway.

## Setting up the VPS over ssh

Supported at first: **Ubuntu 22.04 / 24.04 and Debian 12 / 13, x86_64
and aarch64.** Others are refused at the check step with their name
("Rocky Linux 9 isn't supported yet"). Open question 3.

The setup runs as steps, each one **checks first, then acts**, so running
it again (repair) only does what is missing:

| Step | Check | Act |
|---|---|---|
| Connect | ssh login works; the host key fingerprint is shown and must match what the cloud page says (or "trust on first use", saved) | |
| System | `/etc/os-release`, `uname -m`, free memory (≥ 2 GB; under 4 GB of RAM the relay uses the 512 MB cache), disk (≥ 2 GB), `sudo -n true` | |
| Ports | 25565/tcp, 25566/tcp, 24454/udp not in use (`ss -lntu`) | Offer another port |
| User and folders | `hopper-relay` exists | `useradd --system --no-create-home --shell /usr/sbin/nologin hopper-relay`; `/opt/hopper-relay` (root, 0755), `/var/lib/hopper-relay` (hopper-relay, 0700), `/etc/hopper-relay` (root:hopper-relay, 0750) |
| Java 21 | `java -version` under `/opt/hopper-relay/java` or the system's 21 | Download Temurin 21 JRE (same source and sha256 check as `javadl.rs`) into `/opt/hopper-relay/java`; no apt change |
| hopperd | same version as home | Download the signed `hopperd-<v>-linux-<arch>.tar.gz` from the GitHub release (A1's join command does the same), check signature |
| Velocity | jar sha256 matches Hopper's pinned one | Download from fill.papermc.io, check sha256, write `velocity.toml` (MODERN, `haproxy-protocol = true`, bind loopback, forced hosts) and the JVM flags |
| Secrets | files exist with the right owner and mode | Generate (see Secrets) |
| Service | `systemctl is-enabled hopper-relay` | Write the unit, `daemon-reload`, `enable --now` |
| Firewall (machine) | `iptables -S` / `nft list ruleset` lets the ports in | Insert ACCEPT rules before the image's REJECT, save (`netfilter-persistent` or nft file); `ufw allow` when ufw is active |
| Firewall (cloud) | From home: connect to VPS:25565 and 25566, a UDP echo to 24454 | Can't be done over ssh: the dialog lists the ports to open and re-checks |
| Join | the relay's CA fingerprint, read over ssh | Home saves the pin and the token; the tunnel connects |

What runs as which user:

- The ssh login (often `ubuntu`, `opc`, `root`) needs sudo **only during
  setup, repair and removal**. Hopper runs a fixed list of commands, each
  shown in "Show details", never a shell script fetched from the internet.
- hopperd and Velocity run as `hopper-relay`, no login, no sudo. The unit
  sets `NoNewPrivileges`, `ProtectSystem=strict`, `ProtectHome=true`,
  `PrivateTmp`, `ReadWritePaths=/var/lib/hopper-relay`, and needs no
  capabilities (all ports are above 1024).
- Everything the setup changed is written to
  `/var/lib/hopper-relay/installed.json` (rules added, files, user), so
  "Remove the relay" takes out exactly that and nothing else.

Updates and rollback:

- **hopperd on the VPS** follows home's version: when home updates, it
  offers "Update the relay too" (A3's node update flow: the VPS fetches the
  signed release itself; home never sends a binary). Tunnel N and N-1 still
  talk, so updating one side first is fine.
- **Velocity**: each Hopper release pins a version and sha256. A new one is
  downloaded beside the old jar; the swap restarts Velocity, which drops the
  players, so it waits for nobody online or a click. The previous jar stays
  for **Go back**.
- **Java**: replaced only when the pinned Velocity needs a newer one
  (Velocity 4 needs Java 25), with the same keep-the-old-one rule.

## Changes to the server at home

Each one is a History entry, grouped as one action "Turned on the relay
through a VPS", so it undoes as one (and each part on its own):

| Change | History shows |
|---|---|
| `mods/proxy-compatible-forge-1.3.1.jar` added (Modrinth, sha512 checked) | Mods: added Proxy Compatible Forge 1.3.1 |
| `config/proxy-compatible-forge.toml`: `enabled = true`, `mode = "MODERN"`, `secret` | Config changed (secret masked) |
| `server.properties`: `online-mode` true → false, `enforce-secure-profile` → false, `network-compression-threshold` → -1, `server-ip` → 127.0.0.1 | Settings changed: 4 keys, old and new values |
| Simple Voice Chat `voicechat-server.properties`: `voice_host` → the VPS address and port | Config changed |
| `.hopper-route.json`: relay on, relay-only | Address settings changed |

- Old values are what History keeps; no backup copy of the server.
- **Stop using the relay** undoes the group: PCF removed, the four keys back
  to their old values, voice host back, relay off. It asks for a restart.
- The server must be stopped to turn the relay on or off (the changes need
  a restart anyway); the dialog offers "Stop, change and start".
- A pack update keeps PCF: the pack updater already keeps mods the pack
  doesn't list; the relay re-checks after an update and puts the keys back
  if the pack overwrote `server.properties` (recorded in History).

## Secrets

| Secret | Made by | Where | Mode |
|---|---|---|---|
| Forwarding secret (Velocity ↔ PCF) | home, 32 random bytes, base64 | VPS `/etc/hopper-relay/forwarding.secret`; home `config/proxy-compatible-forge.toml` | VPS root:hopper-relay 0640; home like `auth.json` where the OS allows |
| Relay token (home → VPS) | VPS on join, `hrt_` + 64 hex | home: panel data `relays.json` (0600); VPS keeps only its SHA-256 | |
| Relay CA | VPS hopperd (its own CA, `tls.rs`) | VPS `/var/lib/hopper-relay`; home pins the fingerprint | |
| Voice key | from the TLS exporter of the current connection | memory only | |
| ssh login | the player | **kept** (Decisions): panel data, like other panel secrets; used for setup, repair and removal | 0600 / owner-only ACL |

Rules (AGENTS.md security, `design/server-secrets.md`):

- Never logged, never in an error message, never in the audit log, never in
  `/api` answers. ssh commands that write a secret send it on stdin, not on
  the command line (so it isn't in `ps` or the shell history).
- `config/proxy-compatible-forge.toml` joins `secrets::is_secret_file`, and
  its `secret` key is masked by `configedit::secret_key` already (named
  "secret"): Viewers and Operators can't read it, it stays out of shared
  logs, zips and backups for those who can't see secrets.
- The ssh key is kept (Decisions): in the panel's data folder, owner-only
  (0600, or an owner-only ACL on Windows), never logged or sent back by the
  API. A typed password is never stored; the dialog makes a new ed25519
  key instead and shows the public half to paste into the cloud page.
- Only the **owner** (or Manager) may set up or remove a relay: it runs
  commands on another machine and turns off account checks on the server.

## Failure modes

| What breaks | What players see | What Hopper does |
|---|---|---|
| Tunnel down (home offline, line drop) | Players on the VPS are disconnected ("Lost connection to the home server"). The server list shows the VPS's own answer: "Home server is offline" | The VPS hopperd answers status pings itself (the router's sleeping answer, reused). Home reconnects with backoff. The server keeps running at home. Relay card turns red, alert after 2 min (webhook `relay`). |
| VPS down | Can't connect at all | Relay card: "Can't reach the VPS"; alert. |
| Home server stopped or sleeping | Wake-on-join as today: the join wakes it | Stream goes to the router by server id. |
| Velocity crashed | Can't connect | hopperd restarts it (back-off, then "Velocity keeps stopping", its log on the card). |
| Cloud firewall closed | Can't connect | The outside check fails; the card says which port. |
| Version skew | | Tunnel N/N-1 talk; past that "Update the relay". |

**Is the server reachable at home while the relay is down?** Not by
default. Behind the proxy it runs with `online-mode=false`, so a direct
join must be refused (PCF already refuses logins without the forwarding
data; Hopper's router also refuses them for a relay-only server with "Join
through play.example.com"). Opening it at home again means turning account
checks back on, which needs a restart. So the relay card offers **"Play at
home until the relay is back"**: one click, it undoes the properties and
PCF config (History), restarts, and reverses when the relay returns and the
player clicks again. Never automatic (Decisions).

## Several servers behind one VPS

Two ways:

1. **Velocity forced hosts, generated by Hopper (pick).** Each relayed
   server keeps its Addresses (`dg.example.com`); Hopper writes them as
   forced hosts in `velocity.toml`, each pointing at a VPS loopback port
   that the relay listener maps to that server's id. The stream reaches
   home tagged with the id, so home never reads a hostname. Players can
   also use `/server` between them if the owner lists them in `try`.
2. **One Velocity backend, hopperd maps by hostname at home.** Needs
   Velocity to pass the player's typed address in the backend handshake;
   in modern forwarding it isn't guaranteed (to check in B2 against
   Velocity's `createHandshakeAddress`), and names that only exist for the
   proxy ("proxy-only names") would have to be added to every server's
   Addresses.

Pick 1: the mapping is explicit, survives Velocity changes, and the home
router stays as it is. One DNS name per server pointing at the VPS (or a
wildcard), the same rule as Addresses today.

## Players tab: real ping

The router's ping (`player_ping.rs`, TCP_INFO on the player's socket) reads
the tunnel's loopback socket behind a relay: about 0 ms. Options:

1. **The VPS front takes it (pick).** The VPS hopperd owns the public port,
   reads the handshake and the Login Start name (sent in clear before
   encryption), passes the bytes to Velocity on loopback with a PROXY v2
   header (so Velocity still sees the player's real address for bans and
   limits), and reads TCP_INFO on the player's socket: the same code as
   `player_ping.rs` on the other end. Readings go home on the control
   stream when the Players tab asks.
2. **A Velocity plugin** reporting `Player.getPing()`: a Java plugin to
   build, sign and update. Rejected.
3. **Tunnel RTT plus a guess**: wrong for each player. Rejected.

The column shows `ping to the VPS + VPS to home` (hover: both parts),
because that is what the player feels.

## Relay status on the server page

Numbers come from the codec counters (both ends), every 10 s, kept for an
hour like the CPU sparkline: connected since, upload in the last 10 s
(sent vs vanilla), saved since connected (`1 − sent / vanilla zlib`), cache
hits (bytes and share), VPS → home round trip, the player address. Vanilla
is computed the way `mctunnel` does (per packet zlib level 6, threshold
256) on the raw stream, so the "saved" number is honest.

## Online mode wording behind a proxy

`online-mode` stays a switch in Settings › Players & access. When the
server is relay-only:

- The switch shows Off, locked, with "Accounts are checked by the relay
  (play.example.com). Players can't join this server directly." and a link
  to the relay card. Turning it on there would lock everyone out, so the
  only way is "Stop using the relay" or "Play at home".
- `enforce-secure-profile`, `network-compression-threshold` and `server-ip`
  get the same lock and a short "Set by the relay" line.
- The Overview's Online mode row reads "Off · checked by the relay".
- Desktop's server settings show the same words.

## Desktop parity

- Servers on a paired Hopper Server: the relay card, the ping column and
  the Settings wording come from the same API, so desktop shows them
  (B8). Setup itself runs on the panel (it needs the panel's router); the
  desktop's button opens the same dialog through the API.
- Desktop-local servers (hosted by the desktop): later. The codec is in
  hopper-core for that reason; the transport would move with it.

## Security review notes (this must not weaken Hopper Server)

| Area | Change | Why it isn't a weakening |
|---|---|---|
| New listener on the VPS | Relay port (TLS, token) and the players' port | Not on home. Token checked before any stream; failures rate-limited per address (A1's 10 in 10 min). Home opens no port. |
| Account checks | Server runs `online-mode=false` | Only with PCF modern forwarding on, `server-ip=127.0.0.1`, and the router refusing direct joins. The setup proves it: a direct login probe must be refused before "Done". |
| Who may do it | Owner and Manager | It runs sudo commands on another machine. Admins see the card, can't set up or remove. |
| ssh | Host key shown and pinned; fixed command list; secrets on stdin | No remote script, no stored password by default. |
| Secrets | New secret file | Joins `is_secret_file`; masked for Viewers, Operators, read-only tokens, lower node ceilings. |
| Compression in TLS | One zstd stream inside TLS | No reusable secret in the stream (see above). Review item. |
| Frame parsing | Same caps as the node tunnel (64 streams, credit windows, frame size) plus zstd and dedupe limits: window ≤ 256 MB, cache ≤ the size both agreed in HELLO, a reference to an unknown piece closes the connection | Memory bounded by the agreed sizes. |
| PROXY protocol | Velocity trusts the header | Velocity binds loopback only, so only the front can send it. |
| VPS hopperd | Runs as an unprivileged user under systemd hardening | No web UI on public ports; its own API on loopback only. |

Review before release, by someone other than the author: frame + codec
parsing (fuzz the dedupe decoder), the ssh command list, the
online-mode lock and direct-join refusal, the secret file masking, the
token and pin, the VPS unit's hardening.

## Phased plan

| Phase | Size | What | Checked on screen |
|---|---|---|---|
| B1 | M | **Done.** Codec in hopper-core (`hosting/relay_codec/`): dedupe port, zstd stream with flush, counters (raw, sent, vanilla, cache hits), limits. Replay tool as a hopper-core example (`relay_replay`), not a hopperd subcommand (see "B1 results") | No screen. Tests (round trip, cache desync impossible, bad references refused, fuzz). Replay numbers (synthetic; replay real recordings before B2 fixes the defaults). |
| B2 | L | **Done** (see "B2 as built": no Velocity yet, raw TCP to the router). Relay role (`hopperd relay`) + home client: TLS, mux, server-id streams into the router, control stream, reconnect, status answer while home is down. Relay card (picked option) with live numbers | Two scratch hopperd on loopback, scratch server, a real client joining through scratch Velocity. Card measured in light and dark. |
| B3 | L | Setup over ssh: dialog, checks, steps (idempotent), cloud port check from home, "Remove the relay", repair | Against a throwaway VM or a local Ubuntu container with sshd. Never the live relay. Screenshots of every step state. |
| B4 | M | Server changes: PCF install + config, properties, voice host, History group, Stop using the relay, Play at home, router relay-only refusal, Settings lock and wording | Scratch server: History shows each entry, undo restores, direct login refused. |
| B5 | M | Players tab ping from the VPS front, PROXY v2 to Velocity | Ping column vs a known latency (tc netem on the test VM). |
| B6 | M | Voice relay (UDP, home dials out, kept open by keepalives, AEAD) | Voice works through a test VM; if UDP is blocked, the card says so. |
| B7 | M | Several servers: forced hosts, per-server loopback ports, relay list in Settings › Relays | Two scratch servers behind one test relay. |
| B8 | M | Desktop parity: relay card, ping column, wording, setup through the API | Desktop test build paired with a scratch panel. |
| B9 | M | Security review + fixes. At most a generic note: "switch an existing hand-made Velocity setup over to Hopper's relay" (not the migration of any one server) | Review table filled in. |

B1 to B4 are the useful core (one server, no voice); B5 to B9 can be
reordered.

## Decisions (2026-11-14)

The maintainer's answers to the open questions below, plus the scope:

- **Generic, for any Hopper Server user.** No assumptions about one pack,
  one cloud or one server: any pack size, any loader Hopper hosts, any
  Debian or Ubuntu VPS provider. The ATM10 / Frankfurt numbers stay only as
  reference measurements.
- **Voice**: a built-in UDP relay in hopperd (no AmneziaWG, no kernel
  module, no Windows service). If UDP is blocked, voice falls back to the
  TCP tunnel (works, more jitter), and the relay card says so.
- **Relay down**: red alert, plus a one-click **Play at home** (undo the
  proxy changes and restart). Never automatic.
- **Existing hand-made setups**: moving one over is optional, not a goal.
  Build and test on a fresh throwaway VM. B9 carries at most a generic
  "switch an existing hand-made Velocity setup over" note. A server already
  running behind a hand-made relay stays as it is.
- **Distros**: Ubuntu 22.04 / 24.04 and Debian 12 / 13 only at first.
- **ssh login**: keep the key after setup (for repair and removal), stored
  like the panel's other secrets: owner-only permissions, never logged,
  never in an API answer. Passwords are not stored.
- **Cache**: 512 MB on VPSes under 4 GB of RAM, the full settings (1 GB)
  otherwise (`CodecConfig::for_memory`).
- **Fabric servers**: in scope. They need a Velocity modern-forwarding mod
  on the server instead of PCF: **FabricProxy-Lite** (Modrinth; B4 checks its licence and
  config keys), its config `config/FabricProxy-Lite.toml` holding the
  forwarding secret. B4 installs and records it like PCF; that file joins
  `is_secret_file`.

## B1 results (codec)

What's built, in `crates/hopper-core/src/hosting/relay_codec/`:

- `dedupe.rs`: port of `dedupe.go` (same tokens and sizes: content-defined
  pieces of 1 to 64 KB, about 4 KB on average, references for pieces of 256
  bytes or more, mirrored cache with a byte cap). Changes: ids live in an
  ordered map, so hot pieces don't leave a growing list of stale ids; a big
  write is cut without moving the rest after each piece; each piece counts
  its bytes plus 64 for bookkeeping against the cap, the same on both ends.
- `mod.rs`: `CodecConfig` (`full()`, `small()`, `for_memory()`,
  `validate()` for values from the other end), `Encoder` (`write`, `flush`,
  `take_output`, `stats`, `memory`, `cache_digest`) and `Decoder` (`push`,
  `read`, `decode`, `at_boundary`, the same counters). The decoder caps
  zstd's window at the agreed size, checks every length before allocating,
  and stays failed after the first error.
- `vanilla.rs`: `VanillaEstimate`, one per game connection (vanilla's
  per-packet zlib at threshold 256, like `mc_zlib256`).
- Long distance matching is **off** by default: klauspost has none, and on
  the replay it sent about 15% more.

Replay (`cargo run --profile fast -p hopper-core --example relay_replay --
rec-*.bin`, or `--synthetic P,M,G`) reads `mctunnel -record` files. No
recordings were available to this phase, so the numbers below are
**synthetic** (Minecraft-shaped packets: a 2 MB modded login push, chunks,
a big base of machines, mobs; about 40 MB raw a minute per player, close to
the reference recordings). They show the codec works and how the settings
compare, not what a real pack saves:

| Stream | Settings | Sent | vs vanilla zlib | Cache hits | Encode / decode | Memory (both ends) |
|---|---|---|---|---|---|---|
| 2 players, 20 min, 5 min apart, raw 1.03 GB, vanilla 374 MB | level 11, 256 MB window, no cache | 27.7 MB | 7.4% (13.5x less) | | 186 / 2880 MB/s | 0.53 GB |
| same | + 512 MB cache | 26.6 MB | 7.1% (14.0x less) | 27% of raw | 167 / 1407 MB/s | 1.52 GB |
| same | + 1 GB cache | 26.6 MB | 7.1% (14.0x less) | 27% of raw | 163 / 1238 MB/s | 2.03 GB |
| same | 1 GB cache + long distance matching | 31.5 MB | 8.4% | 27% of raw | 155 / 1200 MB/s | 2.06 GB |
| same, each player in its own stream | 1 GB cache | 31.2 MB | 8.4% | | | |

Reference (Go `mctunnel`, real ATM10 recording, 2 players, 19.7 min):
raw 1589 MB, vanilla 415.7 MB, tunnel 30.4 MB (7.3% of vanilla, about 14x
less); replay of 2 players 5 min apart: 132.5 MB without the cache, 67.6 MB
with it. The synthetic stream lands on the same ratio, but its repeats are
easier: the 256 MB window alone already reaches the second player, so the
cache adds little there, while on the real recording it halved the upload.
**Before B2 fixes the defaults, replay real recordings** with this tool
(`--caches 0,512,1024`, `--level 9`/`11`/`15`, `--ldm`).

Memory: about 1 GB on each end of the busy direction with the full
settings (256 MB window, 1 GB cache counted with bookkeeping, zstd's
tables), about 0.76 GB with the 512 MB cache. The return direction (player
to server) is small, so its cache stays nearly empty; B2 may agree a
smaller cache for it. Level 11 encodes at 160+ MB/s on the dev PC, far
above any home upload; level 19 sent about 10% less on a short run but encodes
at 10 MB/s, too slow for a login burst.

## B2 as built

Code: `crates/hopperd/src/vps/` (`relay_end.rs` the VPS end, `home.rs` the
home client, counters and API, `link.rs` the key check, `wire.rs` frames
through the codec, `tests.rs`), the router's new entry
`router::relayed`, and the card `crates/hopperd/ui/src/RelayCard.svelte`.

### What differs from the plan above, and why

- **No Velocity in B2.** The relay passes each player's raw TCP to home's
  router, which hands it to the server's own port as it does for a LAN
  player. That already works with online-mode servers and needs no proxy
  mod, so B2 can be checked end to end without B4. Velocity, PCF and the
  PROXY v2 header come with B4/B5 (until then the server sees every
  relayed player as 127.0.0.1).
- **Module `vps`, subcommand `hopperd relay`.** `relay.rs` is already the
  node request relay (multi-panel), so the module has another name.
- **Each direction's receiver picks its codec.** The relay asks for the
  busy direction (home to players) by its own memory
  (`CodecConfig::for_memory`, overridable in `relay.json`); home asks for
  the quiet direction `level 3, window 2^22, no cache` (a few MB). Each
  end checks the other's ask against a cap before it allocates anything
  (home: the B1 full settings; relay: window ≤ 2^24, cache ≤ 64 MB).
- **The key is checked before the codec and the mux exist.** After TLS
  (ALPN `hopper-relay`, home pins the relay CA's SHA-256), home sends one
  length-prefixed JSON hello with the key; the relay compares SHA-256s in
  constant time, answers once and closes on a wrong key. A broken hello
  gets no answer. Failed tries count per address: 10 in 10 minutes and
  that address is dropped unread.
- **Which server: one control stream from home.** Home opens a `control`
  stream and sends `{routes: [{server, name}]}` (again when its settings
  change). The relay tags every player stream with the first route's
  server id (one server per relay until B7). Home re-checks the tag
  against its own list of relayed servers and drops anything else, so the
  relay can't reach a server that isn't set to use it.
- **Players per address**: at most 8 at once from one IP on the game port;
  the tunnel carries at most 256 streams (window 256 KiB each).
- **The card sits at the top of the Overview's main column**, above the
  console, not in the 290 px rail: three numbers side by side don't fit
  there. "Play at home until it's back" is shown disabled until B4.
- **Desktop card: not in B2.** Desktop shows a remote server's Overview
  (`src/gpui/server_control.rs`, the Overview cards); the same card there
  is B8, from `GET /api/v1/servers/{id}/vps-relay`.

### Stream protocol

- Home → relay, once, before anything else: `u32 length` + JSON
  `{key, tunnel, version, recv: {level, window_log, long_distance,
  cache_bytes}}`. Relay → home: `{ok, error, tunnel, version, recv,
  game_port}`.
- Then both directions: mux frames (the node tunnel's `tunnel/mux.rs`,
  version check N/N-1), each with a 4-byte length, through one relay codec
  per direction, flushed once per burst. Home is `Side::Node`, the relay
  `Side::Main` (it opens the player streams).
- Stream heads: `{kind: "game", server, peer}` (relay → home, one per
  player connection, server list pings too) and `{kind: "control"}`
  (home → relay). Each side half-closes on its own; a reset or a dropped
  connection ends every stream on both sides.
- While home is away the relay answers by itself: the server list says
  "Home server is offline"; a join is told "The home server is offline.
  Try again in a little while." Nothing is passed anywhere else.

### Listeners and routes

| What | Where | Access |
|---|---|---|
| Relay tunnel port (25566) | VPS | TLS with the relay's own CA, ALPN `hopper-relay`, then the relay key; per-IP failure limit |
| Relay game port (25565) | VPS | Players; bytes only ever go to the paired home's router, or the relay's own "offline" answer; 8 at once per IP |
| `GET/PUT /api/v1/vps-relay` | home panel | `require_owner`; the key is write-only (`has_key`), fingerprint and key format checked, servers must exist |
| `POST /api/v1/vps-relay/retry` | home panel | `require_owner` |
| `GET /api/v1/servers/{id}/vps-relay` | home panel | `require`, plus the per-server access check every `/servers/{id}/` route gets |

Home opens no new listener: it dials out.

### Counters

Per connection and in rolling totals (`vps-relay-stats.json`, owner-only,
saved every minute and when a connection ends), per server: raw bytes to
players, what vanilla would have sent them (`VanillaEstimate`), the
server's share of what the tunnel sent and of the cache hits (by its share
of the raw bytes, since one compressed stream carries every server), bytes
from players and streams. An hour of 10-second samples per server feeds
the card's sparkline.

### Pairing by hand (B3 does this over ssh)

1. On the VPS: `hopperd relay init --data <dir>`. It prints the relay key
   once (only its SHA-256 is kept, in `relay.json`), the certificate
   fingerprint and the ports.
2. Run it: `hopperd relay run --data <dir>` (`--bind`, `--game-port`,
   `--tunnel-port` to change them). Open the game and tunnel ports in the
   VPS firewall.
3. On the home panel, as the owner:
   `PUT /api/v1/vps-relay` with
   `{"enabled": true, "address": "<vps>:25566", "fingerprint": "<printed>",
   "key": "hrt_…", "servers": ["<server id>"], "public_address":
   "<what players type>"}`. The panel connects within a second and keeps
   reconnecting (1 s doubling to 60 s, with jitter) while it runs.
4. The server's Overview shows the card; `GET /api/v1/vps-relay` shows the
   settings without the key.

## B3 as built

Code: `crates/hopperd/src/vps/setup/` (`mod.rs` steps, jobs and routes;
`plan.rs` the fixed commands and the pure checks, tested; `ssh.rs` the
russh client) and `crates/hopperd/ui/src/VpsRelaySetup.svelte`, opened from
the server page's More menu ("Relay through a VPS…") or the relay card's
setup button. Owner only (`require_owner` on every route).

Flow (option A of the mock): **Connect** (address, ssh port, user; a saved
key, a key file, a password used once to put a new key on the VPS, or a new
key made here whose public half the player pastes into the cloud page),
**Check** (OS, arch, RAM → `CodecConfig::for_memory`, disk, sudo, tools,
both ports free), **Install** (steps below, each checks before it acts, so
"Run again" is the repair and reports "Nothing changed" when nothing was),
and **Remove the relay**, from the done screen or from Check.

| Step | Runs as | Check | Act |
|---|---|---|---|
| User | root (sudo) | `id hopper-relay` | `useradd --system --no-create-home --shell /usr/sbin/nologin` |
| Program | root | sha256 of `/opt/hopper-relay/hopperd` against `installed.json` and the source | download (curl/wget) into a 0700 temp folder, `sha256sum -c`, `install -o root -m 0755` |
| Relay key | `hopper-relay` (`runuser`) for `relay status` / `relay init --key-stdin` | key hash, fingerprint, bind and ports match this panel | new key on stdin; data files 0600, folder 0700; saved here at once |
| Service | root | the unit text and `is-active` / `is-enabled` | write the unit (hardened: `NoNewPrivileges`, `ProtectSystem=strict`, no capabilities, `ReadWritePaths=/var/lib/hopper-relay`, `UMask=0077`), `enable --now` |
| Firewall | root | ufw active? iptables INPUT rules for the two ports | `ufw allow <port>/tcp comment hopper-relay`, or `iptables -I` ACCEPTs above the first REJECT/DROP; nothing when nothing blocks them |
| Reach | home panel | TCP connect from home to both ports | warns with the ports to open in the cloud page |
| Pair | home panel | | relay address, fingerprint and key into the panel's relay settings, this server added |

### What differs from the plan above, and why

- **No Velocity, Java, PCF or voice yet.** B3 installs the B2 relay
  (ports 25565 game and 25566 tunnel by default, both changeable). Velocity
  and the server-side changes are B4; voice (24454/udp) is B6.
- `installed.json` lives in `/opt/hopper-relay` (root, 0600), not in the
  relay's data folder: the relay user must not be able to edit the list of
  what Remove takes out. There is no `/etc/hopper-relay` (nothing to put
  there before B4).
- The relay key goes in with `hopperd relay init --key-stdin` (home makes
  it, so it's never printed on the VPS), and `hopperd relay status` prints
  the key's hash, fingerprint, bind and ports so the check can compare
  without reading files.
- The program is checked by the SHA-256 GitHub lists for the release asset
  (`updswap::expected_sha256`), not a signature: releases aren't signed yet.
  Tests upload a local Linux build instead (`HOPPERD_VPS_TEST_BINARY`).
- iptables rules Hopper adds aren't saved across a reboot unless
  `netfilter-persistent` is installed; the step warns and says so. Checks
  read the INPUT chain only (they don't follow jumps into other chains).
- The relay binds IPv4 `0.0.0.0` (tests: `HOPPERD_VPS_TEST_BIND=127.0.0.1`).
- The ssh login is kept in panel data `vps-ssh.json` (0600 / owner-only
  ACL): host, user, the pinned host key, the private key. A typed password
  is used once and never kept; a key Hopper added with it is removed again
  by Remove. The host key is trusted on first use, shown, then must match;
  a changed one needs the "trust the new host key" tick.

### Limits found while testing

- The release Linux binary is built on Ubuntu 24.04; its glibc may be too
  new for Ubuntu 22.04 or Debian 12. The Program step runs `--version`
  after install and fails with that message rather than leaving a broken
  service. A static (musl) build would fix it.
- The release download path is untested end to end: published releases
  don't have `relay init --key-stdin` / `relay status` yet. It works once a
  release with B3 is out.

### Desktop parity (B8)

- Desktop needs a "Relay through a VPS…" entry on a paired Hopper Server's
  server page that drives the same `/api/v1/vps-relay/setup*` routes (the
  setup runs on the panel), with the same three stages, wording and
  confirmations; and the relay card with Connected / disconnected.
- The connect form's key file and password fields send secrets to the
  panel; desktop must send them only over the paired TLS connection and
  never keep them.

## B4 as built

Code: the Velocity steps in `crates/hopperd/src/vps/setup/` (`plan.rs` pins,
paths and `velocity.toml`; `mod.rs` the steps), the server side in
`crates/hopperd/src/vps/forward.rs` (record, jobs, route, settings lock) and
`crates/hopper-core/src/hosting/proxy_forward.rs` (which mod, its config,
the four keys, the plan for each action; pure and tested). UI: the relay
card (`RelayCard.svelte`), the Settings tab lock (`Settings.svelte`) and the
Overview's "Online mode Off · checked by the relay" (`ServerPage.svelte`).

### The chain, and why

Player → VPS front port (the B2 relay, `0.0.0.0:<game port>`) → Velocity on
`127.0.0.1:25577`, with a PROXY v2 header carrying the player's address →
the relay's backend listener `127.0.0.1:25580` → the tunnel → home router →
the server, which runs `online-mode=false` behind FabricProxy-Lite or PCF
with Velocity's modern forwarding. Velocity does the Mojang login on the VPS,
so a player's UUID, skin and address reach the server as with a direct
join, and the server only accepts players Velocity forwarded (checked with
the forwarding secret). The relay keeps its front listener so its own
features (status, "Home server is offline", counters) still work; Velocity
only sees players the relay passed on (`Route.velocity` is true in Relay
mode).

### VPS side (setup step "Velocity and the forwarding secret")

| | |
|---|---|
| Velocity | 3.5.1 build 615, `/opt/hopper-relay/velocity/velocity.jar` (root 0644), sha256 `b4e3164df5377346854dc6cb9e6a78022b1946ff69e89676313f5f6f1c6f0fb3`, 18932366 bytes |
| Java | Temurin 21.0.13+9 JRE in `/opt/hopper-relay/java`: x64 sha256 `a535a1df9c6284d9618872984e026ae0352d1f7e9e0605425c0e6114f47351dd`, aarch64 `b1734db7ade49c3f6acc6ae06233b8c2dd90e3e2f00f67000ae1c972aaa64d8c` |
| User | `hopper-velocity` (system, no shell), separate from `hopper-relay` so neither can read the other's key |
| Settings | `/etc/hopper-relay/velocity.toml` and `forwarding.secret`, root:hopper-velocity 0640 in a 0750 folder; data in `/var/lib/hopper-velocity` (0700) |
| Service | `hopper-velocity.service`, hardened like the relay's; `bind = 127.0.0.1:25577`, `haproxy-protocol = true`, `player-info-forwarding-mode = "modern"`, `online-mode = true` |
| Usage reports | bStats turned off (`plugins/bStats/config.txt`, `enabled=false`), written as the Velocity user |

Each part checks before it acts (sha256, the unit text, the toml), so "Run
again" repairs and changes only what differs; Remove takes out the service,
both users and every folder above. The check fails with "maybe another
Velocity" when 25577 or 25580 is taken by another program.

### Server side (Use the relay / Stop / Play at home / Back to the relay)

`POST /api/v1/servers/{id}/vps-relay/forward` `{action, restart}`, owner
only (`require_owner`); answers 409 while the server runs and `restart` is
false. The job stops the server, changes the files and starts it again; the
web UI asks first and says players online are disconnected.

- **Mod:** PCF (Modrinth `vDyrHl8l`) for Forge and NeoForge on 1.20.2 or
  newer; FabricProxy-Lite (`8dI2tmqs`) for Fabric and Quilt on 1.17 or
  newer, plus Fabric API (`P7dR8mSH`) when the server has none (it needs
  fabric-networking-api). Vanilla, older versions and other loaders are
  refused with a sentence why. A mod that was already there stays; only
  what Hopper added is removed.
- **Config:** `config/FabricProxy-Lite.toml` or
  `config/proxy-compatible-forge.toml` with the secret; one that was there
  before is put back.
- **server.properties:** `online-mode=false`, `enforce-secure-profile=false`,
  `network-compression-threshold=-1` (Velocity compresses),
  `server-ip=127.0.0.1` (nothing but the router reaches it). The old values
  (or "not set") are kept in the record; Stop puts them back exactly and
  keeps the file's line endings (`Properties` now keeps CRLF).
- **History:** every file goes through History: each mod as Added/Removed or
  Turned on/off, the config, and `server.properties` with a settings note
  per key. Rows carry the reason ("Turned on the relay through a VPS",
  "Stopped using the relay through a VPS", "Playing at home while the relay
  is down", "Back on the relay through a VPS"). Each change starts in a
  History second of its own so two quick changes never share a folder.
- **Play at home** turns the mod off and puts the four keys back (accounts
  checked at home again); the config stays, so going back needs nothing new.
  The card offers "Go back to the relay" once the relay is up.
- **Lock:** while on the relay the four keys can't be changed in Settings
  (the field is disabled with the reason and a link to the relay card) and
  the properties API refuses a changed value for them.
- **Record:** panel data `vps-relay-servers.json` (mode, mod, files added,
  old values, folders made). `mods/` and `config/` that the relay made are
  removed again when empty.

### What differs from the plan above, and why

- The forwarding secret is 64 hex characters (32 random bytes), not
  base64: nothing in it needs escaping in a TOML string or a shell line.
- PCF only on 1.20.2+: before that PCF also needs a Velocity plugin
  (Ambassador), and the relay's Velocity runs no plugins. Forge and
  NeoForge 1.20.1 and older can't use the relay's account checks yet.
- Fabric API is added with FabricProxy-Lite (it won't load without it).
- New config files the relay writes have no "before" in History (there was
  none); Stop sets the relay's copy aside in History instead.
- The web History list doesn't show the settings notes (`.settings.json`)
  yet; desktop does. Older gap, not B4's.
- Velocity 3.5.1 is marked deprecated (support ends 2026-09-30); Velocity
  4 needs Java 25, so moving means new pins for both (a B5+ task).

### Tested (WSL box, Fabric 1.20.1 scratch server)

Install, repair (only "binary" and "usage reports off" changed), Use, an
offline-mode login through the relay (UUID matches the offline UUID, the
server log and Velocity log show the client's real address and port), a
direct join refused ("This server requires you to connect with Velocity."),
Play at home (mod off, keys back, unlocked), Back to the relay, Stop with
every file byte for byte as before (History folder aside), and Remove (no
units, users, folders or listeners left). The real Mojang login path is not
tested here (the test box runs Velocity with `online-mode = false` via
`HOPPERD_VPS_TEST_OFFLINE`).

## B5 as built

Ping from the VPS front, the PROXY v2 check, and new Velocity and Java
pins. No new ports: everything rides the existing control stream.

### Players tab ping: the method, and why

Option 1 from "Players tab: real ping" above, as planned. The relay's
front already owns the player's TCP socket, so it reads the kernel's
smoothed round-trip time (`TCP_INFO`, `tcpi_rtt`) on that socket: the same
reading `player_ping.rs` takes for players who join through Hopper's own
address, with no plugin and no guess. Velocity has no way to hand its
`Player.getPing()` to another process without a plugin (option 2, rejected:
a Java plugin to build, sign and keep up to date), and the tunnel's RTT says
nothing about each player (option 3, rejected).

- **Who is who.** After the handshake (login intent only, and only when the
  port has a server) the front reads the Login Start packet, which is sent
  in clear before encryption, and lists the socket under that name in
  `player_ping`, the same list the home router uses. The name is not
  checked by anyone at that point, so it is used only to label a ping in
  the Players tab, never for anything that grants access. When two
  connections claim the same name, the oldest one counts on both ends (a
  second connection with someone's name can't replace their reading).
- **Getting it home.** Home asks only when the Players tab needs it: when
  an online player has no router reading, `player_ping::people` calls
  `Home::relay_pings`. That sends a control line `{routes..., pings: true}`
  and waits at most 800 ms for the relay's answer line
  `{"pings":[{server, name, ms}]}`. An answer is reused for 3 s (the tab
  polls), and counts as current for 15 s. The ask still carries the full
  route list, so an older relay (B4) that doesn't know `pings` just sees the
  same routes again and answers nothing; the tab then shows "—" as before.
  The relay only logs and applies routes when they changed, so asks don't
  fill its log.
- **What the column shows.** `ping to the relay + relay to home` (the
  front's reading plus the tunnel's RTT from the mux pings, every 20 s),
  because that is what the player feels. Hover: "Measured at the relay: 151
  ms to the relay + 20 ms from the relay to this server". Router readings
  win over relay readings, relay readings over spark; players read at the
  relay aren't sent to spark.

### PROXY v2 to Velocity

Already done in B4 (`proxy_v2()` in `relay_end.rs`; Velocity's
`haproxy-protocol = true`, only on its 127.0.0.1:25577 listener). Checked
again: Velocity's log shows each client's own address and port
(`Steve (/127.0.0.1:45661)`, `Alex_The_Long_Nm (/127.0.0.2:45222)` from a
second source address), not the front's.

### Velocity and Java pins

Velocity 3.5.1 lost support on 2026-09-30. PaperMC's API (`fill.papermc.io`
v3) lists 4.3.1 build 45 as the newest STABLE build (2026-10-27); it needs
Java 25. Adoptium's API lists Temurin 25.0.5+8 as the newest Java 25 GA JRE.

| What | Version | SHA-256 |
|---|---|---|
| Velocity | 4.3.1 build 45 | `0844bcabdcdb4f5b875906fd4c3365bc1bb76918e42bc595ed88ad195258dd75` |
| Temurin JRE x64 | 25.0.5+8 | `6fac98f0c8202efc3135a9e9e42aa61963ec42d1c5cbb40caf231e7b14a24ef6` |
| Temurin JRE aarch64 | 25.0.5+8 | `963931723891302ebb2687178532c3d793d71e2577cd058061c7efb396140a86` |

All three were downloaded and hashed before pinning. "Run again" moves a
3.5.1 relay over: the Java step compares `installed.json`'s `java_sha256`
and the Velocity step the jar's SHA-256, so both are replaced and Velocity
restarted. The existing `velocity.toml` (`config-version = "2.7"`) loads in
4.3.1 without warnings. No copy of the old jar or Java is kept to go back
to; a later "Go back" would need one.

### Tested (WSL box, Fabric 1.20.1 scratch server)

- Fresh install with the new pins; Use the relay; offline bots log in
  through Velocity 4.3.1 with modern forwarding into FabricProxy-Lite.
- Ping against added delay (`tc netem`). In WSL's mirrored networking,
  127.0.0.1 traffic goes over `loopback0`, not `lo`, so the delay goes there:

  | Delay added | Shown | Parts |
  |---|---|---|
  | none | 1 ms | relay 1, home 0 |
  | 40 ms, player → front | 42–44 ms | kernel srtt 42.6 ms |
  | 150 ms player → front, 20 ms on the tunnel | 170–173 ms | relay 151–153, home 18–20 |

- Upgrade: the box set back to 3.5.1 + Java 21 (old jar, old JRE, old
  `java_sha256`), then "Run again": java and velocity "downloaded, checksum
  ok", Velocity 4.3.1 boots on Java 25, a bot logs in through it.

### Not done (later phases)

- Desktop's server Players tab (`src/gpui/panel_tabs/players.rs`) gets the
  same `via: "relay"` fields from the server; its tooltip still says the
  generic text. B8 (desktop parity).
- The light theme's amber ping colour (`--gold` on the page background) is
  2.81:1, under 4.5:1. It is the shared colour for every ping, not new
  here.

## Open questions for the maintainer (answered: see Decisions)

1. **Voice: built-in UDP relay instead of AmneziaWG?** AmneziaWG needs a
   kernel module (DKMS) on the VPS and a Windows service at home; a UDP
   relay inside hopperd needs neither. Risk: your ISP blocks plain
   WireGuard; a custom encrypted UDP flow may pass or may not. Fallback:
   voice over the TCP tunnel (works, more jitter).
2. **When the relay is down**: keep it down with an alert and a one-click
   "Play at home" (recommended), or switch automatically?
3. **Distros at first**: Ubuntu 22.04/24.04 and Debian 12/13 only?
4. **ssh after setup**: ask again for repair and removal (recommended), or
   keep the key?
5. **Default memory**: 1 GB cache + 256 MB window costs about 2.5 GB RAM on
   each side; default 512 MB cache on VPSes under 4 GB?
6. **Moving the live Frankfurt setup**: test on a fresh VM first and move
   over in B9 (recommended), or let Hopper take over that VM earlier?
7. **Fabric servers** (FabricProxy-Lite) in scope?
