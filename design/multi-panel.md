# Hopper Server: several panels in one web UI, and node mode (draft)

Status: design only, nothing built. Mock: `.claude/changes/multi-panel/mock.html`
(published as a Design Artifact), picks in `.claude/changes/multi-panel/picks.md`.

Two features share one web UI and one idea of "a place servers live":

- **B, several panels in one browser tab (build first).** The web UI served by
  one panel also lists and drives servers on other panels. The browser talks
  to each panel directly and signs in to each one separately. Nothing is
  stored panel-to-panel.
- **A, node mode (later).** A hopperd on a VM runs as a *node* and dials out
  to a *main* panel, like Pterodactyl/Pelican's panel and wings. The VM
  opens no inbound port. The main panel lists the node's servers and relays
  everything to it.

B is the web equivalent of what Hopper desktop already does (it pairs with
any number of panels and shows one group per panel, `src/gpui/servers.rs`
`groups()`). A is new for both apps.

Words used below: **home panel** is the panel that served the page you are
looking at. **Other panel** is one you added to that page. **Main** and
**node** are the two ends of A.

## What exists today (read from the code)

Sign-in and sessions (`crates/hopperd/src/api.rs`, `auth.rs`):

- A browser signs in with `POST /auth/login` (name + password, optional
  two-factor code). The reply sets `hopperd_session`: **HttpOnly,
  SameSite=Strict, Path=/, 30 days, Secure over TLS**. The token is 32 random
  bytes; only its SHA-256 is stored (`Session { hash, user, created,
  last_seen, ip }`).
- A cookie request that changes state must also send `X-Hopperd` (the CSRF
  guard). A bearer token (`Authorization: Bearer`) skips that guard because
  nothing ambient carries it. Devices (the paired desktop) and API tokens use
  bearer.
- `Who` is `Owner`, `Device`, `Token` (read-only, per-server scope) or `User`
  with a `Role`: manager, admin, operator, viewer, plus a server list.
  `security::refusal` applies role and server scope to every request;
  owner-only routes call `require_owner`.
- The WebSocket (`/api/v1/ws`) accepts a cookie only when `Origin` equals
  `Host`, or a bearer header (which browsers cannot set on a WebSocket).
- The page and the API are the same origin, so there is **no CORS code at
  all**, and the served UI sends no Content-Security-Policy.
- Rate limits: 1200 requests a minute per address, 20 a minute on sign-in,
  setup, 2FA and `pair/request`. Five wrong passwords lock an address out for
  10 minutes. Setup needs the one-time setup code (or a loopback browser).
- Pairing with the desktop: the desktop learns the panel's CA over a checked
  connection, pins it, and both sides show a 6-digit code derived from the CA
  fingerprint, the request id and a secret (`hopper_core::hopperd::pair_code`).
  A machine in the middle has another CA, so the codes differ. The owner
  allows it in the panel; the desktop keeps a bearer token in Credential
  Manager.
- TLS: hopperd is its own certificate authority (`tls.rs`), or uses Let's
  Encrypt (`acme.rs`). Plain HTTP is allowed from this PC, the LAN and
  Tailscale, never from the internet. `GET /hello` is open and carries the
  CA PEM and fingerprint.

The web UI (`crates/hopperd/ui/src`) assumes one panel everywhere: `api.js`
calls relative `/api/v1/...`, `live.svelte.js` holds one WebSocket and one
`live.servers` list, `me.svelte.js` holds one `session`, `ServerList.svelte`
lists `live.servers`, downloads and previews are plain `<a href>` / `<img
src>` that rely on the cookie.

## B. Several panels in one browser tab

### The shape

The home panel keeps its cookie session exactly as it is. Each other panel
gets a **browser session**: a bearer token the page holds and sends in an
`Authorization` header. The page calls each panel's `/api/v1` directly with
`fetch` and one WebSocket per panel. The home panel is not a proxy and sees
none of the other panels' traffic or credentials.

Why not cookies for other panels: a cookie for `https://rig:8790` is sent
only if the page is same-site, and SameSite=Strict (our setting, and the
right one) blocks it when the page is on another host. SameSite=None needs
HTTPS on every panel and still dies to third-party-cookie blocking in
Safari and Firefox and soon Chrome. Also, cookies ignore ports, so two
panels on the same host (`rig:8790`, `rig:8791`) would overwrite each
other's `hopperd_session`. Bearer sessions avoid all three.

### Adding a panel

1. **Add a panel** (button at the foot of the server list). One field: the
   address (`https://rig:8790`, `192.168.50.3:8790`). A bare address tries
   `https://` first, then `http://`.
2. The page asks the panel `GET /hello` (open endpoint). From the answer it
   shows the panel's name and version and decides the next step:
   - `setup_needed`: "This panel has no owner yet. Set it up on its own
     page" with a link. **The setup code is never typed into another
     origin's page.** Claiming a fresh panel stays on that panel's own page,
     where `trusted_local` and the setup code rules are unchanged.
   - Otherwise a sign-in form: name (empty = owner) and password, and the
     two-factor code if the panel asks (`need_code`, same flow as today).
3. Sign-in is `POST /auth/login` with `{ "bearer": true }`. The reply carries
   the token in the body instead of `Set-Cookie`. The page stores it (see
   below) and adds the group to the list.
4. If step 2 fails, the page cannot tell *why* from the error alone (browsers
   hide it on purpose), so it probes: a `no-cors` fetch to the same URL
   resolves (opaque) when the panel is reachable with a certificate the
   browser accepts, and rejects when it is unreachable, blocked as mixed
   content, or has an untrusted certificate. Combined with the failed normal
   fetch that gives four messages:
   - reachable but refused: **CORS hint** with the exact origin to copy
     ("On *rig*, open Settings › Access › Other web pages and add
     `http://fox:8790`").
   - not reachable: "Can't reach it. Check the address and that it is on."
   - reachable only after trusting its certificate: "Open the panel once and
     accept its certificate, then try again" with a link (see TLS below).
   - page is HTTPS and panel is HTTP: "Your browser won't let this secure
     page talk to a plain HTTP panel. Open this page over HTTP, or give the
     other panel HTTPS."

The list of added panels (name, URL, last role seen) is a per-account
preference stored **on the home panel** (`/auth/me/panels`, URLs and names
only), so a second browser sees the same groups, each one asking to sign in.
Tokens never go there. Browsers without that (a private window) just start
empty. This keeps "no credentials stored panel-to-panel": the home panel
learns which addresses you use, and nothing that signs in to them.

### Where the sessions live, and what that exposes

Today an HttpOnly cookie means a script on the page cannot read the token.
With bearer sessions the page holds the token, so the exposure changes:

| | Cookie (today, home panel) | Bearer in the page (other panels) |
|---|---|---|
| XSS in the web UI | Can act as the user while the page is open; cannot take the token away | Can read every other panel's token and use it from anywhere until it expires or is revoked |
| CSRF from another site | Blocked by SameSite=Strict and `X-Hopperd` | None: nothing is sent automatically. A preflight must pass first anyway |
| Token theft from disk | Cookie jar | `localStorage` / IndexedDB of the home panel's origin |

What we do about it:

1. **A Content-Security-Policy on the served UI before anything else ships**
   (B0). `script-src 'self'`, no inline script, `object-src 'none'`,
   `base-uri 'none'`, `frame-ancestors 'none'`. The UI renders markdown (mod
   descriptions, changelogs) and log text, so this is the real defence.
   `connect-src` has to allow other panels, so it is `'self' https: http:
   ws: wss:`; script injection is stopped by `script-src`, not by limiting
   where the page may call.
2. **Storage.** Tokens in `localStorage` under the home origin, keyed by the
   other panel's origin, so they survive a reload. Optional "Don't keep me
   signed in on this browser" uses `sessionStorage` (gone when the tab
   closes). We do not pretend WebCrypto "non-extractable keys" help: a
   script on the page can still use the key. Everything is wrapped in
   try/catch because storage can be off; the page then just asks to sign in.
3. **Shorter and visible.** Browser sessions last 7 days from last use
   (sliding), not 30. They are listed in the other panel's Settings › Access
   with name ("Web UI on rig, Chrome"), IP, last used, and a Revoke button
   (the sessions list exists; this adds a `kind: browser` label).
4. **A browser session cannot change who can get in.** From an origin other
   than the panel's own, the panel refuses managing users, devices, API
   tokens, two-factor, the allowed-pages list below, and TLS settings, and
   points to "Open on rig". Everything day to day (servers, files, mods,
   console, backups, schedule) works. This limits what a stolen token can do
   to lock the real owner out or plant a second way in. It costs parity (the
   owner cannot add a user on panel B from panel A's page). Open question 1.
5. **Never store a password.** An expired session shows "Sign in again" and
   asks for the password. The page does no silent retry with saved
   credentials, which also keeps the 5-wrong-tries lockout from being hit by
   a loop.

### CORS (on the panel being called)

New owner-only setting on every panel: **Other web pages that may use this
panel** (`web_origins` in `hopperd.toml`, a list of exact origins like
`http://fox:8790`; empty by default = today's behaviour).

- Middleware, ahead of `security::guard`'s auth checks: if the request has an
  `Origin` that exactly matches the list, answer with
  `Access-Control-Allow-Origin: <that origin>` (never `*`), `Vary: Origin`,
  `Access-Control-Allow-Headers: authorization, content-type, x-hopperd,
  x-hopperd-client`, the methods the API uses, `Access-Control-Max-Age: 600`,
  `Access-Control-Expose-Headers: content-disposition, content-length`.
  No match: no CORS headers at all.
- **No `Access-Control-Allow-Credentials`.** The page uses
  `credentials: 'omit'` for other panels, so the browser never attaches the
  panel's cookie to a cross-origin call. That is deliberate: an allowed
  origin cannot ride the user's cookie, it needs the bearer token.
- Preflight: every call with an `Authorization` header is preflighted, so a
  page on a non-listed origin is stopped before its request is sent. The
  `OPTIONS` answer needs no auth, is cheap, and does not count towards the
  sign-in limit. It does count towards the 1200/minute address limit.
- Chrome's Private Network Access: a page on a public HTTPS name calling a
  LAN panel is preflighted for `Access-Control-Request-Private-Network`. The
  allowed origin gets `Access-Control-Allow-Private-Network: true`.
- Changing the list is owner-only and (point 4 above) not possible from a
  browser session on another origin. The panel's own page shows the list,
  validates entries (scheme + host + port, no path, no wildcard), and logs
  every change in the audit log.
- The allowed origin is the **page's** origin, not the target's. If a person
  reaches the home panel as `rig`, `192.168.50.3` and a Tailscale name, those
  are three origins; the CORS hint always prints the one the page is on now
  (`location.origin`) so it can be copied.

### TLS and self-signed certificates

A browser cannot pin a fingerprint or a CA for one `fetch`. It uses its own
trust store. So for an other panel:

- **Let's Encrypt / trusted certificate** (hopperd's ACME, or a reverse
  proxy): works with no prompts. This is what we recommend for anything
  reached over the internet.
- **hopperd's own CA** (the default): the certificate is signed by a CA the
  browser does not know. Without a fix the browser shows a "not private"
  page when you open the panel and `fetch` just fails. The player sees
  "Open the panel once and accept its certificate" with a link; once accepted
  for that host and port, calls from the page work. The better fix is
  installing the CA into the OS (the add flow offers it: a "Download this
  panel's certificate" button, from the open `hello.tls.ca`, with plain
  steps). Because hopperd re-issues the server certificate at each start but
  keeps the CA, installing the CA once is enough.
- **Plain HTTP on the LAN** (allowed by design) is the lowest-friction path
  when the home page is also HTTP. A home page on HTTPS cannot call an HTTP
  panel (mixed content, except `localhost`). We say that in the add flow
  rather than offering a workaround.
- We **do not show the fingerprint as proof of identity** in the browser. A
  page can read `hello.tls.fingerprint`, but cannot check it belongs to the
  connection it came over, and a man in the middle can forge the JSON. Showing
  it would look like a safety check and not be one. The desktop pairing code
  stays the real mutual check; for the browser, the honest line is "your
  browser trusts this address because: a public certificate / a certificate
  you accepted / plain HTTP on your own network".
- Recommendation: other panels over the internet = Let's Encrypt, or Tailscale
  (private, plain HTTP allowed, or `tailscale cert`). LAN = HTTP or the
  installed CA.

### What the player sees when things go wrong

- **Panel offline**: its group stays in the list with a red light and "Can't
  reach rig · retry". Its servers show the last known list, dimmed, no
  buttons, with "last seen 2 min ago". Other groups are unaffected. Each
  panel reconnects on its own with backoff (the same schedule as
  `live.svelte.js`: 300 ms doubling to about 20 s).
- **Session expired or revoked** (`401`): the group shows "Sign in again" with
  an inline form; the cached list stays dimmed. Other panels keep working.
  No password retry loop.
- **Panel updating or restarting**: same as offline, plus the existing "new
  version, reload" logic per panel (`boot.svelte.js` becomes per panel).
- **Older panel** without the browser-session login or CORS: `hello.api` /
  `capabilities` lacks `web.remote`; the add flow says "This panel needs a
  newer Hopper Server" and offers "Open its own page".
- **Role**: each group header shows the role you have there (a small chip:
  Owner, Admin, Operator, Viewer), from that panel's `/auth/me`. Buttons
  inside a group follow *that* panel's role, never the home panel's. A
  viewer on panel B sees B's servers with no power buttons while being owner
  on home. `me.svelte.js`'s single `session` object becomes one per panel.

### Console and live feed across origins

- One WebSocket per panel. Browsers cannot send an `Authorization` header on
  a WebSocket, so the page first calls `POST /ws/ticket` (bearer) and
  receives a **one-use ticket, valid 30 seconds, bound to that session**; it
  connects to `wss://panel/api/v1/ws?ticket=...`. `ws_allowed` accepts a
  ticket only when `Origin` is in the allowed list. The cookie path
  (same-origin) is unchanged.
- The ticket is in a URL, so it could land in a log. The panel's request log
  already records paths only, never queries (`security.rs`), the ticket is
  single-use and dies in 30 seconds, and `Referrer-Policy: same-origin`
  stays. Alternative considered: carrying it in `Sec-WebSocket-Protocol`.
  That keeps it out of the URL; we prefer it if axum's WebSocket upgrade
  exposes the subprotocol cleanly (a spike in B1).
- Console follow messages (`{op: "console", server}`) are per socket, so
  nothing changes in the protocol. The client's `consoles`, `history` and
  `live` stores become per panel.
- Server ids come from folder names and can repeat across panels, so every id
  in the UI is namespaced by panel: route `#/p/<panel>/server/<id>/...`. The
  home panel keeps today's `#/server/<id>` so existing links work.

### Files: upload and download across origins

- **Uploads**: `fetch` with the Authorization header and the chunked upload
  already used (`lib/upload.js`) work cross-origin with the CORS headers
  above. Progress comes from the chunking, not `fetch`.
- **Downloads and previews** are the hard part: `<a href>`, `<img src>`,
  `<video>`, zip downloads and image previews send no Authorization header.
  Reading a multi-gigabyte world zip into a blob is not acceptable. So a new
  call, `POST /files/{id}/link` with `{path, zip?}`, returns a **one-use
  link valid 60 seconds, bound to the session, the server and the exact
  path(s)**, and the page navigates to it (`Content-Disposition: attachment`).
  Redeeming it re-runs the normal path checks. Previews use the same links
  with `inline` for the types the panel already allows (it refuses HTML and
  SVG).
- Every other place that uses a bare URL the cookie authenticates (mod
  icons on `/servers/{id}/...`, log downloads, `EventSource` if any) needs the
  same treatment. B2 starts with a grep list; anything not converted shows
  an "Open on rig" link instead of breaking.

### Where B touches Hopper Server security (AGENTS.md: never weaken it)

| Area | What B changes | Why it is not a weakening |
|---|---|---|
| Auth | New bearer browser session; login can return the token in the body | Same token strength, hashing and 2FA as a cookie session; shorter life; revocable and listed |
| Roles | None | Roles and server scope still come from `Who` and `refusal()`; per panel |
| Setup code | Never sent cross-origin; a fresh panel is set up on its own page | Setup is unchanged |
| TLS | No change to server; documented that browsers use their own trust and cannot pin | No claim of verification the browser cannot make |
| Rate limits | `OPTIONS` preflights do not count as sign-in attempts; the add-panel form hits the target's normal 20/min and 5-fail lockout | The tight limit still covers every sign-in |
| Path checks | One-use file links re-run the existing path check at redemption | No new way to name a path |
| CSRF / origin | `Origin` allowlist replaces "Origin must equal Host" only for ticketed WebSockets; cookie paths unchanged; no credentialed CORS | Cookies never work cross-origin |
| New | Allowed-origins list, ws tickets, file links, remote-session limits, CSP | Each needs its own tests and a review |

## A. Node mode

### The idea

`hopperd --node` (or Settings › Make this a node) on a VM. The node keeps
running its servers on its own, but takes its orders from one **main** panel
over a connection **the node opens to the main**. The VM needs no open port,
no port forward, no domain. Players connect to the game servers as they do
today (that is the game's port, separate from the panel).

### Pairing (existing flow, reused)

The pairing code that already protects desktop pairing becomes the node
pairing:

1. On the node (console or `hopperd node join https://main:8790`): the node
   fetches the main's CA over TLS, shows its fingerprint, and sends `POST
   /nodes/request { name }` to the main.
2. The main creates a pending request with `pair_code(fingerprint, id,
   secret)`, exactly like `Auth::pair_request`, with the same 5-minute TTL,
   the same `MAX_PENDING`, and the same tight rate limit on the request route.
3. The node shows the 6-digit code; the main shows the same code in Settings ›
   Nodes with the node's name, address as seen and a ceiling picker. The
   owner compares the codes (they differ if something sits in the middle)
   and allows it. **Only the true owner may add a node**, not a manager: a
   node is another machine they can run code on. Open question 2.
4. The node receives a **node token** (32 random bytes, only the hash kept on
   the main, bound to this node id) and stores it with the main's CA pin in
   its own data folder (permissions as `auth.json`).

The setup code is not involved in pairing, and the node keeps its **own**
setup code and owner (claimed with the node's own code through the console or
loopback). The main never learns the node's owner password. In node mode the
node's own web UI binds loopback only.

### The connection

Recommendation: **one WebSocket over TLS from node to main**
(`wss://main/api/v1/node/connect`), carrying JSON control frames and
multiplexed binary streams. Reasons: the main already serves axum WebSockets
and TLS (no new listener, no new dependency), it passes through Cloudflare and
reverse proxies, and a node behind NAT or a tailnet works without anything
else. Alternatives considered: gRPC/HTTP2 (new dependency and proxy trouble),
yamux over a raw TLS socket (a new port on the main), SSH reverse tunnel
(operational, not integrated).

- The node pins the main's CA (fingerprint stored at pairing); a different
  certificate is refused even if a public CA signed it. The main pins nothing
  from the node; it checks the node token on the upgrade request
  (`Authorization: Bearer`, no browser involved).
- Keepalive: WebSocket ping every 20 s, 60 s without a pong = dead. The node
  reconnects with backoff (1 s doubling to 60 s, with jitter). The main shows
  the node offline after two missed pings.
- A node whose token is refused (`401`, revoked) **stops** and says so; it does
  not hammer the main (so a revoked token cannot trip the lockout of its
  address, which may be shared by other nodes behind one NAT).
- Auth failures on `/nodes/connect` count per token and per address toward the
  tight limit; successful reconnects do not.
- **The node is autonomous.** Its servers keep running, its schedules,
  backups, crash alerts and History keep working while the main is
  unreachable. When the main returns it learns what happened (History is read
  from the node, not pushed).

### Trust model: what a compromised main can do

Be honest first: a main whose ceiling lets it manage servers can ask the node
to write files in a server folder and start that server, and a server is a
Java process running mods. That is code running as the node's hopperd user.
That is already true for Admin on any single panel. A node adds no new power
beyond that; it adds a second machine in the blast radius. What we limit:

1. **Node-side ceiling, chosen at pairing, changeable on the node**: *Look*
   (view, console read), *Operate* (+ start/stop/restart, console commands,
   backups), *Manage* (+ files, mods, settings, create/delete servers). The
   main's roles apply on top, never above the ceiling. Default: Manage, shown
   plainly with the warning above.
2. **Node enforces its own checks.** Every relayed request is re-run by the
   node through its normal router with a `Who::Main { on_behalf_of, role,
   servers }`. Path checks against the server folder run on the node, not
   trusted from the main. The main cannot name a path outside a server folder
   or outside the node's panel folder.
3. **Node routes the main can never reach**, whatever the ceiling: node
   owner/users/devices/tokens, node TLS and its pin, the node's own update
   switch, `node` settings, SFTP keys, the panel-folder move, browsing
   arbitrary host folders (`browse`), the allowed-origins list. These are done
   on the node.
4. **The node records its own History** for every change a relayed request
   makes, with who: "via rig, as mikey (admin)". The main's audit log is not
   the only record; a compromised main cannot erase the node's.
5. **Per-node, revocable token.** Revoke from the main (Settings › Nodes ›
   Remove: deletes the hash and closes the socket) or from the node
   (`hopperd node leave`, or Settings on loopback: forgets the token and the
   pin, works offline). Either side revoking is final until re-paired. Tokens
   rotate on reconnect every 30 days.
6. **Commands only from its paired main**: structural, since the node only
   serves frames from the one connection it opened to the pinned main, and
   frames carry the main's id and a per-connection counter so a replay on
   reconnect is dropped.
7. **Confirmations stay.** Start, stop, restart and kill keep the UI
   confirmation on the main. The node does not need its own prompt because
   the request is the user's.
8. **Everything is logged on both sides**: node connects, disconnects, ceiling
   changes, refused requests.

### Relaying requests, console and files

- **Requests.** The main takes the user's API request, does its own auth,
  role and server-scope check (`require`, `refusal`), then forwards a compact
  request frame (method, path under `/servers/{id}/...` or `/fs/{id}/...`,
  body or a body stream, the acting user's name and role) to the node, which
  runs it through its own router. One router means one set of handlers: no
  second, thinner copy of mods/files/backups for nodes (parity rule). Only an
  allowlist of route families is relayed; new routes are not relayed until
  added to it.
- **Console and live feed.** The node sends the same snapshot (1 s) and
  console batches (150 ms) the local panel produces; the main merges node
  servers into its snapshot with ids namespaced `<node>/<server>`. The
  browser and the desktop do not know the difference.
- **Files.** Streams with flow control (credit windows) so a world upload or
  zip download through the main uses a bounded buffer and no temp file on the
  main. Throughput is the slower of the two links; this is an accepted cost
  of "no inbound port". Downloads use the same one-use link idea as B, issued
  by the main.
- **Players and traffic**: the Minecraft port is not relayed. Hopper's
  wake-on-join router stays per node.

### Version skew

- The first frame each way carries `{version, api, tunnel, capabilities}`
  (the same `CAPABILITIES` list the API already publishes). The main lights up
  only the **intersection**, per node.
- Tunnel versions N and N-1 are supported. Outside that, the node shows as
  "Update needed" in the main, read-only: list and status from the last
  compatible snapshot, no actions that might be misread by the other side.
- Updating a node from the main is a separate action, ceiling-gated
  (*Manage* only), and uses the node's own signed-release update path
  (`autoupdate.rs`), never an arbitrary binary from the main.
- A node newer than its main works for the shared capabilities and says "Main
  is older".

### Roles

The main's roles and per-user server lists are the only roles. The node does
not have users for this path. Owner-only actions on the main (users, devices,
tokens, TLS, create/remove node, panel settings) stay owner-only. Actions
that affect the node itself rather than a server on it (item 3 above) are
simply not reachable through the main, even for its owner. Creating or
deleting a *server* on a node needs main role Manager or Owner **and** the
Manage ceiling.

### Where A touches Hopper Server security (AGENTS.md: never weaken it)

| Area | What A changes | Why it is not a weakening |
|---|---|---|
| Auth | New node token and `Who::Main` principal on the node | Per-node, hashed, revocable on both sides; never a user or owner login |
| Roles | Main's roles apply; node adds a ceiling | The ceiling can only lower what a role may do |
| Setup code | Not used for pairing; the node keeps its own | Claiming a node stays local |
| TLS | Node pins the main's CA; wss only; plain ws refused | Same pin model as the desktop |
| Rate limits | Pair request keeps the 20/min limit; connect failures counted per token and address | The tight limit stays where guessing is possible |
| Path checks | Run again on the node for every relayed path | The main is not trusted to name paths |

### What must get a security review before release

Both features are on the list; nothing ships with "reviewed by the author
only".

B: the allowed-origins middleware (exact match, `Vary`, no credentials, PNA);
browser sessions and the remote-session restrictions; ws tickets and file
links (one-use, binding, expiry, replay, log leakage); CSP completeness against
the markdown renderer; the add-panel probe (it runs in the browser, so only
the browser's reach is used and the panel gains no fetch-this-URL route);
rate limits for preflight and sign-in; the "setup never cross-origin" rule.

A: the pairing (code derivation reuse, who may approve, request flooding); the
tunnel (frame parsing, stream limits, memory caps, reconnect storms); the
route allowlist and the "never through the main" list; `Who::Main` and
ceiling enforcement in every handler that checks `Who`; path checks on the
node for relayed paths, including symlinks and `..`; History attribution;
revocation on both sides; token storage and rotation; update gating; the
audit trail.

## How the desktop fits

The desktop already pairs with each panel directly (`hopper_panels`, one
`HopperPanel { id, name, url, ca }` each, a bearer device token in Credential
Manager) and shows one group per panel ("rig Hopper Server · 7"). B needs no
desktop change; the web UI is catching up to the desktop. Wording and
behaviour must match: group header with chevron, state light (green listed,
amber loading, red error), name, "Hopper Server · N", a + to make a server on
that panel, a context menu with "Open in browser".

For A, the desktop pairs with the **main** only, and a node's servers arrive
through it (the server record gets a `node` field next to `remote.panel`). The
desktop shows a node as **its own group** next to the main's, captioned "node
· via rig · 3", not a sub-list inside the main's group: groups in the
desktop mean "where these servers run", a node is a different machine with its
own online state and its own ceiling, and it keeps the list flat (no new
nesting widget). The + creates a server on that node (Manage ceiling only);
"Open in browser" opens the main's page on that server. The web UI uses the
same flat grouping so both apps read the same.

## Phased plan

Sizes use AGENTS.md's S/M/L plus a day estimate for one agent with review.

**B (first)**
- **B0, prerequisites (M, ~1.5 d).** CSP on the served UI (and fix whatever
  it breaks); `web_origins` setting + middleware + preflight + tests; Settings
  page entry (owner only) with validation and audit log; capability
  `web.remote` in `hello`. Review.
- **B1, browser sessions and tickets (M, ~2 d).** `login {bearer:true}`,
  sliding 7-day life, "Browser" label in the session list with Revoke, the
  remote-session restrictions, `POST /ws/ticket`, `ws_allowed` ticket path,
  `POST /files/{id}/link` + redemption. Rust tests for each refusal. Review.
- **B2, web UI multi-panel (L, ~4-5 d).** Panel context in `api.js`,
  per-panel `live` / `consoles` / `me`, namespaced routes, ServerList groups,
  Add a panel dialog with the probe and CORS hint, offline / expired states,
  role chips, panel list preference on the home panel. Light and dark
  screenshots with measured gaps; two scratch panels (`hopperd-test`).
- **B3, cross-origin files and polish (M, ~1.5 d).** Uploads, downloads,
  previews, bare-URL audit, CA download help, docs, screenshots, a report of
  which places only offer "Open on rig".

**A (later)**
- **A0, protocol spike (S, ~1 d).** WebSocket subprotocol for the ticket,
  frame format, credit flow, a throwaway node and main echo. Settles open
  question 4.
- **A1, pair and connect (L, ~4 d).** `node join`, requests and approval in the
  main, node token, pin, keepalive, reconnect, revoke from both sides, "Nodes"
  settings on main and node. Review.
- **A2, relay requests, console, History (L, ~5 d).** Route allowlist,
  `Who::Main`, ceiling, namespaced ids, merged snapshot, History attribution,
  parity tests with a fake node.
- **A3, files, skew, update, apps (L, ~4 d).** Streamed files, capability
  intersection, update gating, desktop group, web group.
- **A4, hardening (M, ~2 d).** The security review items, docs, a real VM
  test (a throwaway node, never a real server).

## Open questions for the maintainer

1. **Remote-session limits (B).** Should a browser session on another origin be
   barred from managing users, devices, tokens, 2FA, TLS and the allowed-pages
   list ("Open on rig" instead)? Safer, costs parity. My recommendation: yes.
2. **Who may add a node (A).** True owner only, or managers too? I recommend
   owner only.
3. **Panel list roaming (B).** Store the list of added panels (URLs and names,
   no tokens) on the home panel per account so every browser shows them, or
   keep it in the browser only? I recommend on the home panel.
4. **Ticket carrier (B/A).** Query string versus `Sec-WebSocket-Protocol`:
   prefer the subprotocol if the spike shows axum handles it cleanly.
5. **Default node ceiling (A).** Manage (everything on servers), or Operate and
   an explicit upgrade? I recommend Manage with a clear warning, because
   Operate-only nodes are not useful for most people's first try.
6. **Session length (B).** 7 days sliding for browser sessions, against 30 days
   for the home cookie. Is that acceptable, and should "remember me" be a
   choice in the add flow?
7. **Group look (A).** Node as its own flat group (my pick) versus a sub-list
   inside the main's group. Same answer for both apps?
8. **Home panel offline.** The page itself disappears when the home panel is
   down. Fine for v1, or do you want a small installable shell that opens
   without a home panel? I recommend leaving it.
9. **Name of the setting.** "Other web pages that may use this panel" versus
   "Allowed web UIs". The first is plainer; please confirm the wording.
10. **Finding panels (B).** Should the add flow list panels the home panel can
    see on its LAN (browsers can't do mDNS) next to typing an address?
