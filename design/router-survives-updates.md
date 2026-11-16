# Keep the router and the relay up across Hopper Server updates (draft)

Status: design only (phase 1). No product code changed. Size: L (a new
long-lived process, both OSes, both relay ends).

The problem, in the user's words: "Hopper Server updates take the router
down and anyone connected, especially to our proxy server, disconnects.
Keep our routers up like our servers."

Words used below: **hopperd** is the Hopper Server process (the panel).
**Router** is the Minecraft port (25565) that reads the address a player
typed and passes them to that server's own port
(`crates/hopperd/src/router.rs`, protocol in
`crates/hopper-core/src/hosting/router.rs`). **Relay** is "Relay through a
VPS" (`design/vps-relay.md`, `crates/hopperd/src/vps/`): a `hopperd relay`
process on a VPS takes players and sends them home over one tunnel.
**Edge** is the new process this document proposes.

## What dies today, and why

Minecraft servers survive a hopperd restart because they are their own
processes: started outside hopperd's job (Windows `CREATE_BREAKAWAY_FROM_JOB`,
Linux `setsid`, systemd `KillMode=process`), with a run file holding their
pid and start time, which a new hopperd checks and adopts
(`hosting/process.rs`, `adopt` and `reattach`).

Player connections are not. Every one is a socket inside hopperd:

- **Router, direct joins.** `router::start` binds 25565 inside hopperd's
  tokio runtime. Each join is a task: read the handshake, pick the server,
  connect to `127.0.0.1:<its port>`, replay the bytes read so far, then
  `copy_bidirectional` until someone leaves. Both sockets (player and
  server) belong to hopperd.
- **Stand-ins.** A server that is stopped, sleeping, starting or crashed is
  answered by hopperd itself: the server list entry, the "Sleeping, join to
  wake it" kick, a login held on the connecting screen until the server is
  up (`hold.rs`, then the same replay and copy), or the lobby island
  (`limbo.rs`, which ends with a transfer, so the client reconnects).
- **Relay, home end.** `vps/home.rs` keeps one TLS connection to the VPS.
  On it runs the mux (`tunnel/mux.rs`), and between the mux and TLS the
  relay codec: a dedupe cache mirrored on both ends and one zstd stream per
  direction with a big window. Each player stream from the VPS is turned
  into an in-memory `tokio::io::duplex` and handed to `router::relayed`,
  which does the same connect, replay and copy as a direct join. The TLS
  session, the codec state and the server-side sockets all live in hopperd's
  memory.
- **Relay voice.** Voice packets go home sealed with keys taken from that
  TLS connection (`vps/voice`), and home sends them on from one local UDP
  socket per player. Both die with the connection.
- **Relay, VPS end.** `hopperd relay run` (systemd `hopper-relay.service`,
  `Restart=always`) holds every player's socket on the VPS, the front
  listener, the backend listener Velocity connects back to, and the VPS
  half of the tunnel. Velocity is its own service (`hopper-velocity.service`)
  but every byte between the player and Velocity, and between Velocity and
  home, passes through the relay process.
- **Players tab ping.** For direct joins `player_ping` reads the OS's round
  trip from the player's socket (`SIO_TCP_INFO` / `TCP_INFO`), which needs
  the socket in the same process.

### (a) Home hopperd updates

`autoupdate::install_and_restart` waits for jobs (and on Linux for servers
it couldn't adopt), then `restart_into`:

- **Windows service:** writes `updating.json`, exits with 75; the service's
  failure action starts it again after **10 seconds**
  (`service.rs`, `update_failure_actions`). Then hopperd starts, reads its
  config, adopts servers and binds 25565 again. The router is gone for
  about 12 to 20 seconds.
- **Windows tray (portable, Start with Windows):** exits with
  `EXIT_UPDATED`; the tray starts the new exe. A few seconds.
- **Linux:** `exec`s the new program in the same pid (systemd keeps
  watching it). Rust opens every socket close-on-exec, so all of them close
  at the `exec`. A few seconds. If `exec` fails under systemd it exits 75
  and `RestartSec=5` applies.

What drops: every player on every server that came in through the router;
every player who came in through the relay (the tunnel closes; the VPS end
tells them "Lost connection to the home server"); relay voice; anyone held
on a stand-in or the island. Joins in the gap are refused (nothing listens)
or, through the relay, told "Home server is offline". Players who connect
straight to a server's own port, not through the router, are not affected.

### (b) The VPS relay updates

The VPS program is only replaced by the setup's "Run again"
(`vps/setup/mod.rs`, `step_binary` then `systemctl restart hopper-relay`),
which installs the version that matches the home panel. Nothing updates it
by itself, but every home update is followed, sooner or later, by a Run
again to keep the versions in step (the link takes N and N-1).

What drops: every player on the relay (their sockets are in the restarted
process), Velocity's sessions with them (Velocity sees its player sockets
and its backend sockets close), voice. Players at home are not affected.
Restarting Velocity (Java or Velocity changed) drops relayed players too.

### (c) A plain restart or crash

Same as (a) on that machine, with no wait for jobs. A crash on Windows also
takes the 10 second failure delay. On the VPS `RestartSec=5`.

## What has to be true to keep a player on

A Minecraft session can't move from one TCP connection to another: once a
player is through to a running server, both of its sockets (player side and
server side) must stay open, and every byte between them must arrive, in
order, once. So the sockets must be held by a process that does not restart
when hopperd does.

For relayed players that also covers the tunnel: its TLS session, the mux
and the codec state can't be copied into another process (rustls and zstd
have no "export this session" that we'd want to rely on, and the dedupe
cache is up to 1 GB). The home end of the tunnel has to live in that same
long-lived process, and the VPS end needs its own answer.

The useful fact: once a player is through to a running server, the router
does nothing but copy bytes. All the logic (which server, wake, stand-ins,
the island, holds, server list messages) happens before that, and is the
part that changes from release to release.

## Option A: a separate edge process (recommended)

A small process, `hopperd edge`, that owns the router's port, the relay's
home connection and every live player pipe. hopperd keeps all the thinking
and tells the edge what to do over a local, authenticated channel. The edge
is started detached and adopted on start, the way servers are.

### What the edge does

- **Listens** on the router port (the same bind and port rules as now) and
  accepts players.
- **Pipes.** For each player it opens a leg to hopperd over the local
  channel and copies bytes both ways. It keeps a copy of everything the
  player sent until the connection is spliced (capped at 64 KB; the
  handshake and login are well under 1 KB).
- **Splices.** hopperd runs today's `route` on that leg unchanged. Where it
  would now connect to `127.0.0.1:<port>`, replay and `copy_bidirectional`,
  it instead sends the edge `splice {conn, port, replay, consumed}`:
  `replay` is exactly what it would have written to the server (the
  handshake and login, plus what `hold` kept), `consumed` is how many of the
  player's bytes it read. The edge connects to the server, writes `replay`,
  then any player bytes past `consumed` it already sent to hopperd, closes
  hopperd's leg, and from then on copies player to server by itself.
  Nothing is lost in the hand-over because the edge replays from its own
  record, not from what was in flight. Stand-ins, kicks, the island and
  server list answers stay in hopperd, on the leg, as now.
- **Relay home end.** The edge runs `vps/home.rs`'s connection: TLS pinned
  to the relay's certificate, the key check (`vps/link.rs`), the mux, the
  codec, voice. A player stream from the VPS becomes a leg to hopperd tagged
  with its server (the `Pick::Server` path) and is spliced the same way.
  hopperd sends the edge the relay's config (address, fingerprint, key,
  routes) over the channel; the edge keeps it only in memory.
- **Reports** to hopperd: player counts and bytes per server, the relay's
  link state and counters, and on request each piped player's ping from
  their socket (moving `player_ping`'s socket reading into the edge).
- **When hopperd is away** (the update gap, or a crash), new connections
  have no leg to go to. The edge then answers them itself, from what hopperd
  last gave it: a server list entry per address (the last real answer it
  kept, with the message "Hopper Server is restarting, back in a moment") and
  for a join, holds it on the connecting screen for up to 20 seconds
  (the same wait as `hold.rs`) and hands it to hopperd when it is back,
  else kicks with "Hopper Server is restarting. Join again in a few
  seconds." Live pipes don't notice.

hopperd keeps: the route table, sleep and wake, stand-ins, the island,
holds, live server list messages, History, alerts, webhooks, and the relay's
settings. `router::route` keeps its shape: its socket becomes a leg, and its
final `TcpStream::connect` plus `copy_bidirectional` become one `splice`
call. The `PlayerConn` trait already abstracts the socket type.

### Started, found and stopped

- Same binary, its own copy: at first use hopperd copies itself to
  `<data>/edge/hopperd-edge-<version>[.exe]` and starts that copy with
  `edge run --data <data>`. The main exe can then be swapped by updates
  without touching the running edge (Windows won't let a running exe be
  overwritten; Linux would keep the old inode anyway, but a separate path
  keeps it plain which version is running). On Windows the firewall rule
  follows the program path, so see open question 5: probably one fixed
  path, `hopperd-edge.exe` beside hopperd, replaced only at an edge update.
- Detached like servers: Windows `CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP
  | CREATE_BREAKAWAY_FROM_JOB` (the same fallback when the job refuses);
  Linux `setsid`, and the unit already says `KillMode=process`. Runs as the
  same account as hopperd, so no new user or rights.
- Run file `<data>/edge/edge.json` (owner-only, in the panel's data folder,
  not a server folder players can edit): pid, process start time, version,
  channel protocol version, channel address. A starting hopperd checks pid
  plus start time like `adopt`, connects, authenticates, sends the current
  routes and settings, and from then on legs flow again.
- Stopped only on purpose: turning the router off (and no relay), "Stop
  Hopper Server" from the tray or `hopperd service stop` without the update
  marker, `service uninstall`. An update restart, a crash and a plain
  restart leave it running. On stop it closes the listener and lets live
  pipes finish (or closes them when the owner asked for everything to stop,
  which the panel says).
- If the edge itself dies, hopperd sees the channel close, starts a new one
  and says so in the log and on the router card. Players on it drop, as they
  do today.

### Updating the edge itself

The edge is small and changes rarely: it holds the pipe, splice and
fallback code, and the relay link and codec (which change with the relay
protocol). The channel carries a protocol version.

- hopperd uses an older edge as long as the protocol is one it speaks
  (N and N-1, like the relay link).
- When a newer edge is needed (a newer protocol, or a fix in edge code),
  hopperd says so on the router card: "The router has an update. It
  installs when nobody is playing through it (3 players now)." and an owner
  button "Update the router now (3 players are disconnected)".
- When nobody is piped, hopperd starts the new edge copy, hands it the
  listener (below), tells the old one to stop, and deletes old copies. With
  no live pipes nothing is felt; joins in that moment are held by the
  listener hand-over, not refused.
- Later (phase 6) the same hand-over lets an edge update happen with
  players on: the old edge passes the listener and keeps its live pipes
  until they end (drain), the new one takes every new join. The VPS relay
  must then accept a second home connection for the draining edge (see
  Option C's notes).

### Local channel and security

Nothing new on the network: the edge's only public surface is the router
port, which exists today with the same limits (`MAX_HELD`, the 8 KB cap
before a handshake, timeouts), plus the relay connection, which it dials
out as hopperd does now.

- **Channel:** a Unix socket in `<data>/edge/` (folder 0700, socket 0600)
  on Linux; on Windows a named pipe `\\.\pipe\hopperd-edge-<hash of data
  dir>` with `PIPE_REJECT_REMOTE_CLIENTS` and a security descriptor that
  allows only the account that runs hopperd (and SYSTEM). No TCP port, so
  nothing on the LAN can reach it and `HOPPER_LOOPBACK` has nothing to add.
- **Auth:** a 32-byte random token written to `edge.json` (owner-only) when
  the edge starts; every connection on the channel starts with it, compared
  in constant time. On Linux also check the peer's uid (`SO_PEERCRED`); on
  Windows the pipe's client process id must be a hopperd from the same
  install (`GetNamedPipeClientProcessId`). A wrong token closes the
  connection and is logged; ten in a minute and the edge stops reading the
  channel for a minute.
- **Secrets:** the relay key crosses the channel in memory only; the edge
  never writes or logs it. `vps-relay.json` stays hopperd's.
- **Splice targets:** the edge only ever connects to `127.0.0.1` on a port
  hopperd names, never another host, so a confused or compromised channel
  can't turn it into an open proxy to the LAN or the internet.
- Nothing in Hopper Server's authentication, roles, TLS, rate limits or
  path checks changes; the web panel is still hopperd's alone.

### What survives, what still drops

| Event | Survives | Still drops |
|---|---|---|
| Home hopperd update or restart | the router port; every player on a running server, direct or through the relay; relay voice; the tunnel and its cache | anyone on a stand-in, the island or held at login at that moment (they're on hopperd's leg); server list pings in flight. Joins in the gap are held up to 20 s, then told to join again |
| hopperd crash | the same | the same |
| Edge update (phase 4) | everything, when it waits for nobody on (the default) | players on it if the owner picks "Update now" (until phase 6) |
| Edge crash | servers | every piped player (as today's hopperd restart) |
| VPS relay update | nothing new from A alone | every relayed player (Option C covers it) |
| PC reboot, server restart | | everything, as now |

### Cost

New: `crates/hopperd/src/edge/` (`mod.rs` run and adopt, `pipe.rs` accept,
legs, splice, `channel.rs` the local socket and messages, `fallback.rs` the
answers when hopperd is away), about 1,200 lines plus tests. Changed:
`router.rs` (legs instead of sockets, `splice` instead of connect and copy),
`vps/home.rs` (the connection moves into the edge, the API and stats stay),
`player_ping.rs` (readings come from the edge), `autoupdate.rs` (an edge
update is its own small flow), `service.rs` and the tray (stop the edge on a
real stop). About 8 to 11 working days over phases 2 to 4.

### Risks

- Two processes to keep in step. Mitigated by the protocol version and N-1.
- A bug in the edge drops everyone and is fixed only by an edge update, so
  its code must stay small and well tested, and change rarely.
- Windows named-pipe security descriptors are easy to get wrong; test that
  another local account can't open the pipe.
- The splice changes the most-used path. Keep the old in-process path behind
  a setting ("router in Hopper Server", off by default after a release) for
  one release, to fall back if something is off.
- One more process in Task Manager / `ps`. Name it clearly ("Hopper router").

## Option B: hand the sockets to the new hopperd during an update

No new process. At update time hopperd pauses each pipe, hands the
listener and every live socket pair to the new hopperd, and the new one
carries on.

- **Linux:** clear close-on-exec on the listener and each pair, write their
  fds and any bytes still buffered to a hand-over file, `exec` (same pid).
  The new hopperd reads the file and resumes each pump. Needs a pump that
  can stop at a clean point and give back its buffers (`copy_bidirectional`
  can't), but `exec` makes the rest easy.
- **Windows:** the service process exits and the service manager starts a
  new one 10 seconds later, so there is nobody to hand to. It needs the old
  process to start a holder, pass it the sockets with `WSADuplicateSocketW`
  (which needs the target's pid), and that holder to pass them on to the
  next hopperd: the edge of Option A, only alive for the update.
- **Relay:** can't be handed: the TLS session and the codec state are in
  memory. Relayed players drop, and they are the ones the user cares about
  most ("especially our proxy server").

Survives: direct players on Linux. Still drops: all relayed players, direct
players on Windows unless the holder is built, everyone on a crash (nothing
to hand from). About 4 to 6 days for Linux. Risky in exactly the moment
something else is already changing (an update), and untestable on a crash.
Not recommended; its listener hand-over idea is reused in Option A's edge
updates.

## Option C: the VPS end hot upgrade (needed with A for the relay)

The VPS relay holds every relayed player's socket, so A alone does nothing
for a VPS update. The VPS is always Linux with systemd, which allows the
usual hot binary upgrade (as nginx and HAProxy do it):

- The setup's "Run again" no longer runs `systemctl restart`. It installs
  the new binary and runs `systemctl reload hopper-relay`
  (`ExecReload=/bin/kill -HUP $MAINPID`).
- On SIGHUP the relay starts the new binary as a child, passing its
  listeners (game, tunnel, backend, voice UDP) over a Unix socket with
  `SCM_RIGHTS`. The new one tells systemd it is the main process now
  (`Type=notify`, `NotifyAccess=all`, `sd_notify MAINPID=`), takes every new
  connection, and the old one stops accepting and keeps its live players
  (and its tunnel generation) until they leave or a cap (24 hours), then
  exits. `KillMode=process` (or `mixed`) so a later reload doesn't kill the
  draining one.
- Home's edge keeps two tunnel connections while that happens: the old one
  for the old relay's live streams, a new one for the new relay. Today
  `relay_end` lets a newer home connection take over and closes the older
  one; that becomes "a newer one takes new streams; the older one keeps its
  open streams and closes when it has none" (per epoch, which it already
  counts). The codec cache is per connection, so two connections are two
  caches: memory doubles for the drain time. The relay card says "Relay
  updating: 3 players still on the old version".
- Velocity (Java, its own service): its restarts still drop relayed players.
  Velocity only changes when its pinned version changes, so setup does it
  only when "Run again" says so, and the card warns first, like a server
  restart: "Velocity is updated next: 3 players on the relay are
  disconnected".

About 4 to 5 days, after Option A's relay phase.

## Recommendation

**Option A, then Option C.** A keeps every player on a running server
connected through any home hopperd update, restart or crash, direct or
through the relay, on Windows and Linux, and keeps the port answering in
the gap. C does the same for the VPS end. B saves only direct players on
Linux and none of the relay, so it isn't worth its risk.

## Phased build plan

| Phase | What | Size |
|---|---|---|
| 2 | Edge core, direct joins: `hopperd edge run`, run file and adopt, local channel with auth, legs and splice, fallback answers while hopperd is away, `router.rs` on legs. Old in-process router kept behind a setting. Desktop unaffected (it only reads route settings). | 4 to 5 days |
| 3 | Relay home end in the edge: the tunnel, codec, mux and voice move from `vps/home.rs` into the edge; hopperd keeps the API, stats file and settings and gets counters and pings from the edge; Players tab ping from the edge. | 3 to 4 days |
| 4 | Edge updates: protocol version N/N-1, "updates when nobody is playing" on the router card, "Update the router now" (owner, confirm with the number of players), listener hand-over so no join is refused, cleanup of old copies; stop the edge on a real stop and on uninstall. Web UI card and wording; same wording in the desktop's view of a Hopper Server if it shows the router. | 2 days |
| 5 | VPS hot upgrade (Option C): SIGHUP reload, listener hand-over with `SCM_RIGHTS`, `sd_notify` main pid, drain; `relay_end` keeps older home connections for their open streams; the edge holds two tunnel generations; the setup uses reload; the card says what drains. | 4 to 5 days |
| 6 (optional) | Edge update with players on: old edge drains, new takes the listener (Linux `SCM_RIGHTS`, Windows `WSADuplicateSocketW` to the new edge's pid). | 2 to 3 days |

Report after each phase, with the proof below.

## Tests to add

- Splice: a fake player sends handshake + login + extra bytes in odd
  chunks while hopperd's leg reads only part; after the splice the fake
  server receives exactly the player's bytes once, in order (byte counter
  and a hash on both ends).
- Splice with `hold`: the held login and the bytes `hold` kept reach the
  server exactly as today's `wait_for` sends them.
- The splice refuses any target but `127.0.0.1` and ports outside 1..65535.
- Channel auth: a wrong token, a token from another data folder, and (Linux)
  another uid are refused; the edge keeps running; repeat failures back off.
- Adopt: a run file whose pid exists but started at another time, or isn't
  `hopperd edge`, is not adopted (the `adopt` rules).
- Fallback: with hopperd away, a status ping gets the kept answer with the
  restarting message; a login is held, then handed over when hopperd comes
  back within 20 s, or kicked after.
- Protocol: an N-1 edge works with an N hopperd; an N-2 one makes hopperd
  plan an edge update.
- Relay (phase 3, 5): a relayed stream survives hopperd's restart; with two
  relay generations, old streams stay on the old connection and new ones go
  to the new; the old connection closes when its last stream does.
- Existing router, hold, limbo and relay tests keep passing unchanged
  (`router.rs` tests, `relay_tests.rs`, `relay_stream_tests.rs`,
  `vps/tests.rs`).

## How to prove it on a scratch setup

Everything on 127.0.0.1, scratch folders, `HOPPER_LOOPBACK=1`,
`HOPPERD_SERVICE=hopperd-test`, `--portable --port <free> --home <scratch>
--data <scratch>` (the `hopperd-scratch` skill). No real panel, server, VPS
or relay.

1. **Home:** a scratch hopperd A with the router on a free port (say 25600)
   and one scratch server: a tiny fake Minecraft server (a test binary that
   answers status and, after a login, sends a numbered keepalive line every
   100 ms and echoes what it gets) or a real small vanilla server in the
   scratch home.
2. **Relay:** a second scratch process `hopperd relay run --data <scratch2>
   --bind 127.0.0.1 --game-port 25601 --tunnel-port 25602`, paired with A by
   hand (`PUT /api/v1/vps-relay`), as `design/vps-relay.md` B2 tested it.
3. **Clients:** a test client joins on 25600 (direct) and one on 25601
   (relayed). Each checks that the keepalive numbers keep coming with no gap
   and no repeat, and that its echo comes back, and records any close.
4. **Simulated update:** replace A's exe with a newer test build and trigger
   `POST /api/v1/panel/update/install` (or simply kill A's process and start
   it again for the crash case). Both clients must stay connected, with no
   missing number, across the restart; a third client joining during the gap
   is held and then gets through. Check `edge.json` names the same edge pid
   before and after.
5. **VPS update (phase 5):** `kill -HUP` the relay process (on the WSL test
   box, `design/vps-relay.md` "WSL"); the relayed client stays on, and a
   new relayed join lands on the new relay pid.
6. **Edge update (phase 4):** with clients on, the card says it waits; with
   none, the edge pid changes and a join during the hand-over isn't
   refused.
7. Do it on Windows (FOX, headless; nothing opens a window) and Linux (WSL
   box), and put the client logs (connected seconds, numbers received, gaps)
   in the PR.

## Open questions for the maintainer

1. "Our proxy server": is that the VPS relay set up through Hopper (with
   Velocity on the VPS), or a Velocity or BungeeCord proxy that runs at home
   as one of hopperd's servers, with players coming in on 25565 through the
   router? Option A alone covers the second; the first needs phase 5 too.
2. Is it acceptable that players on a stand-in, the lobby island or held at
   login during an update still drop? (Keeping them needs the edge to run
   those too, which is most of the router logic and would make the edge
   change often.)
3. Should "Stop Hopper Server" also stop the router (players disconnected),
   or leave it piping players to running servers, like servers keep running?
   The draft stops it, with the player count in the confirmation.
4. Edge updates: wait for nobody on (default) with an owner "Update now"
   button, or also an hour of the night setting like scheduled restarts?
5. Windows Firewall allows or blocks by program path, and asks for every
   new program that listens on all networks (`main.rs` notes this). A
   versioned edge copy is a new path per edge version. Two ways: a fixed
   path (`hopperd-edge.exe` next to hopperd, replaced only while no edge
   runs it, i.e. at an edge update) with a rule that `service install` adds,
   or a port rule for the router port instead of a program rule. The draft
   picks the fixed path plus program rule; is a port rule preferred?
