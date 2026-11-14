# Relay through a VPS (Hopper Server, desktop parity) (draft)

Status: design only (phase 1 of an L feature). Mock:
`design/mocks/vps-relay.html` (2 or 3 options per screen, picks marked,
light and dark), also published as a Design Artifact (one artboard per
screen). Nothing here is built yet.

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

A hand-built setup outside Hopper (2026-11-14) proved the idea with ATM10:

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
- Fabric and old Forge backends in the first version (PCF covers NeoForge
  1.20.2+ and Forge; Fabric needs FabricProxy-Lite, open question 7).
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

- zstd: **`zstd` 0.13** (libzstd via `zstd-sys`, builds with `cc` on MSVC
  and Linux; `long_distance_matching`, `window_log` up to 28 for 256 MB,
  `ZSTD_e_flush` per burst). `ruzstd` (pure Rust) has no strong encoder;
  rejected.
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
| System | `/etc/os-release`, `uname -m`, free memory (≥ 2 GB, warn under 4 GB with the default cache), disk (≥ 2 GB), `sudo -n true` | |
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
| ssh login | the player | **not kept** by default: used for setup, asked again for repair or removal | Open question 4 |

Rules (AGENTS.md security, `design/server-secrets.md`):

- Never logged, never in an error message, never in the audit log, never in
  `/api` answers. ssh commands that write a secret send it on stdin, not on
  the command line (so it isn't in `ps` or the shell history).
- `config/proxy-compatible-forge.toml` joins `secrets::is_secret_file`, and
  its `secret` key is masked by `configedit::secret_key` already (named
  "secret"): Viewers and Operators can't read it, it stays out of shared
  logs, zips and backups for those who can't see secrets.
- The ssh key or password, when typed, lives in memory for the setup only;
  the dialog can also make a new ed25519 key and show the public half to
  paste into the cloud page (then the private half is kept, 0600, in the
  panel's data folder, because the player chose it).
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
player clicks again. Never automatic (open question 2).

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
| B1 | M | Codec in hopper-core: dedupe port, zstd stream with flush, counters (raw, sent, vanilla, cache hits), limits. Replay tool (`hopperd relay-replay rec.bin`) | No screen. Tests (round trip, cache desync impossible, bad references refused, fuzz). Replay numbers vs the Go tool on the same recording. |
| B2 | L | Relay role (`hopperd relay`) + home client: TLS, mux, server-id streams into the router, control stream, reconnect, status answer while home is down. Relay card (picked option) with live numbers | Two scratch hopperd on loopback, scratch server, a real client joining through scratch Velocity. Card measured in light and dark. |
| B3 | L | Setup over ssh: dialog, checks, steps (idempotent), cloud port check from home, "Remove the relay", repair | Against a throwaway VM or a local Ubuntu container with sshd. Never the live relay. Screenshots of every step state. |
| B4 | M | Server changes: PCF install + config, properties, voice host, History group, Stop using the relay, Play at home, router relay-only refusal, Settings lock and wording | Scratch server: History shows each entry, undo restores, direct login refused. |
| B5 | M | Players tab ping from the VPS front, PROXY v2 to Velocity | Ping column vs a known latency (tc netem on the test VM). |
| B6 | M | Voice relay (UDP, home dials out, kept open by keepalives, AEAD) | Voice works through a test VM; if UDP is blocked, the card says so. |
| B7 | M | Several servers: forced hosts, per-server loopback ports, relay list in Settings › Relays | Two scratch servers behind one test relay. |
| B8 | M | Desktop parity: relay card, ping column, wording, setup through the API | Desktop test build paired with a scratch panel. |
| B9 | M | Security review + fixes; moving the hand-built setup over (a checklist for the maintainer, run by them) | Review table filled in. |

B1 to B4 are the useful core (one server, no voice); B5 to B9 can be
reordered.

## Open questions for the maintainer (ranked)

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
