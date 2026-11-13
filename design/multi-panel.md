# Hopper Server: several panels in one web UI, and node mode (draft)

Status: B0 built (CSP, `web_origins` and CORS, the access-management
switch; `crates/hopperd/src/web_access.rs`, `ui.rs`); B1 built (browser
sessions, live tickets, file links; `auth.rs`, `grants.rs`, `api.rs`,
`fs_api.rs`, tests in `cross_origin_tests.rs`, see "B1 as built"); the rest
is design. Mock: `.claude/changes/multi-panel/mock.html`
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
   where the page may call. As built (`ui::CSP`): `style-src 'self'
   'unsafe-inline'` (Svelte writes `style="…"` attributes, and CodeMirror
   always injects a `<style>` element into a document, so a stricter
   `style-src-elem` broke the file editor; a per-response style nonce is the
   upgrade if that ever matters), `img-src 'self'
   https: http: data: blob:` (icons and pictures from CurseForge, Modrinth,
   GitHub and authors' sites), `frame-src 'none'`, `form-action 'self'`, and
   `X-Frame-Options: DENY` for older browsers. B0 publishes the capability
   `web.origins` (CORS and the allowed-pages list); `web.remote` waits for
   B1's browser sessions, so a client never sees it before sign-in works.
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
   owner cannot add a user on panel B from panel A's page), so the owner can
   turn it off per panel ("Let these pages manage who can get in", off by
   default; open question 1, answered).
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

As built (2026-11-13): the main's `/servers/infos` lists its nodes' servers
too (namespaced ids, `node`, `node_name`, and `server.properties` asked from
the node as Look with a 2.5 s limit), and a node server gets the stand-in
root `hopperd:\@<panel>\@<node>\<folder>`. Everything else goes through the
relay unchanged, with the id's `:` sent as `%3A`. Where a node can't do
something from the main the desktop works around it or stays quiet: no
download-from-URL (sync uploads the jar), the desktop shares a node's log
itself (`logs/share` isn't relayed), no ping without a join address, and the
main marks a node server's off-site copies as not editable. Not built yet:
the + on a node's group, and the node's own online light (the group uses
the main's).

## Phased plan

Superseded by the phase list in "Adjusted after research" below; kept for
the day estimates.

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

## Adjusted after research (2026-11-13)

Source: `design/panel-research.md` (Pterodactyl/Pelican, AMP, MCSManager,
Crafty, PufferPanel, Multicraft, Lodestone, Komodo, Portainer, Beszel,
Netdata, Coolify/Dokploy, Tailscale). The phase list in this section
**replaces** "Phased plan" above. The security model above is unchanged;
every addition below is checked against the "never weaken" tables.

### What the research changes

1. **A is the main multi-node story; B is for panels the browser can
   already reach.** Every panel whose browser or main must reach the node
   inbound (Pterodactyl, MCSManager, AMP) has certs, ports and mixed content
   as its top complaint (Pterodactyl #2483, ~115 reactions; MCSManager
   #1570/#1129/#2340). Komodo, Beszel, Netdata and Portainer Edge all moved
   to "agent dials out". So B stays as designed, but its add flow is honest
   about where it works (same LAN, Tailscale, or a panel with a real
   certificate), and when the probe ends in "untrusted certificate" or
   "secure page, plain HTTP panel" it offers **"Add it as a node of this
   panel instead"** once A exists. Order stays B then A: B0 is built and B1
   is small, and A reuses B1's tickets and file links.
2. **Refusals must be readable cross-origin.** Lodestone (#696) learned that
   a refusal without CORS headers reaches the page as an opaque network
   error. Every response to an allowed origin, including 401, 403 and 429
   from `security::guard` and `web_access::access_refusal`, carries the CORS
   headers, so the page can say "Sign in again", "Not allowed from here:
   open on rig" or "Too many tries, wait 10 minutes" instead of "Can't
   reach it". Non-listed origins still get no CORS headers.
3. **A dead panel or node never slows anything else** (PufferPanel #1246,
   Pterodactyl #3936). B: each panel's calls have their own short timeout
   (5 s for lists, no timeout on streams), and the home list renders before
   any other panel answers. A: the main serves a node's servers from its last
   snapshot and never blocks a page on a node; relayed calls to an offline
   node fail fast with "rig-vm is offline since 14:02".
4. **Offline is debounced and alerts once** (Coolify #4407, Netdata
   #18436). A node is "Reconnecting" after one missed ping and "Offline"
   after 60 s. A **Node offline** alert rule (existing `rules`/`alerts`, so
   webhooks and Discord come for free) fires once after 2 minutes offline
   and once on recovery. Nothing is switched off on either side because of
   an outage; the node keeps its schedules (already in the plan).
5. **Maintenance mode per node** (Pterodactyl's standard workaround for
   #3936). "Pause this node" on the main: hides actions, silences its alerts,
   shows "Under maintenance" instead of "Offline". Owner or manager. Pure
   main-side display; the node and its servers are unaffected.
6. **Two ways to join a node** (Tailscale, Beszel, Netdata):
   - *Code* (as designed): `hopperd node join https://main` on the node,
     compare the 6-digit codes, allow on the main.
   - *Join command*: the owner clicks "Add a node" on the main, picks the
     ceiling and gets a one-line command for Linux and for Windows that
     installs hopperd as a service and joins. The command carries the main's
     address, **the main's CA fingerprint** (so the node pins it before
     sending anything) and a **join key**: single use, 30-minute expiry,
     shown once, hash stored, owner-only to create, listed and revocable
     under Settings › Nodes, every use in the audit log. A join key can only
     create one node request that is auto-approved with the ceiling chosen
     when the key was made; it is never the node's identity (Komodo's
     onboarding keys). The node still gets its own node token at the end.
     Why this is not weaker than the code: the code proves "the CA the node
     sees is the main's"; the command proves the same by carrying the
     fingerprint from the owner's signed-in page, and the key replaces the
     owner's "Allow" click.
7. **Pairing and connection say which step failed** (AMP's silent spinner,
   Portainer #8531). `hopperd node status` on the node and the node's row on
   the main show the last attempt as one of: address not found (DNS), can't
   connect (TCP), certificate doesn't match the pinned one (with "re-pair"
   steps), node key refused (revoked), versions too far apart, connected.
   DNS failures are retried like any other (Beszel #1341). The node honours
   `HTTPS_PROXY`. TLS uses rustls with bundled web roots so a bare VM image
   works (Beszel #2291).
8. **Per-node health in the group header** (Pelican, Calagopus, Beszel
   #1017): state, version, last seen, round-trip time, CPU, RAM, disk free,
   and **RAM promised** (sum of the servers' max memory) against the box's
   RAM. Creating or moving a server that would promise more than the box has
   warns plainly and lets the owner continue; no overallocate percentages and
   no `-1` magic (Pterodactyl #4940). B shows the same header per panel from
   its `/host`.
9. **Ports: no allocation records.** The node picks a free port with the
   existing port check; the main shows conflicts. Players reach the node's
   own address; if the node is behind CGNAT the create/move dialog says
   "Players can't reach servers on this node from the internet" from the
   existing port check (Pterodactyl #2754 shows what records cost).
10. **Move or copy a server between nodes** (the top ask once people have two
    machines: Dokploy #812 89, Pterodactyl #18 ~69). Built on what exists:
    clone, the content-addressed backup store and the `hopper` backup-copy
    target (`offsite.rs`), streamed through the main over the two node
    tunnels. Rules from Pterodactyl's failures: precheck disk space, port,
    Java and loader on the target; ask to stop the server first; resumable
    (only missing chunks are sent again); symlinks and file modes kept (test);
    **the source is kept by default**, stopped and marked "Moved to
    rig-vm", with "Delete the old copy" as a separate later action; a failed
    move cleans up the target; History entries on both nodes. Backups are not
    copied unless the user ticks "Bring backups" (no automatic backups rule).
    Main-to-node and node-to-main moves use the same path. Between B panels a
    move is out of scope (the browser cannot carry it); the existing backup
    copy to another panel stays the way to do it.
11. **Node-scoped access** (Pterodactyl #279, ~118, the largest count found).
    A user's server list can also hold "all servers on rig-vm", which
    includes servers created there later. API tokens can be scoped to a node
    the same way. This only narrows or names a set of servers; roles are
    unchanged and the node ceiling still caps everything.
12. **Bulk actions across groups** (Pterodactyl #278, never built). Select
    servers in the list (drag-pick, `lib/dragpick.svelte.js`) across panels
    or nodes, or "All on rig-vm" from the group menu, then start, stop,
    restart or send a command. One confirmation lists what will happen per
    group; each server's result is shown; servers your role can't touch are
    listed as skipped. In B each panel gets its own calls with its own
    token; in A the main fans out.
13. **One complete audit trail** (Pterodactyl #1219 shipped with holes
    because console and power went browser-direct). In A every action passes
    the main, so the main's audit log has all of it, with a node filter, and
    the node keeps its own History (already in the plan). In B each panel's
    own audit log is complete because the browser calls it directly.
14. **Updates without breaking the link** (Portainer #12925, 327 reactions;
    AMP; Komodo v2). On top of N/N-1 tunnel support: the main's own update
    screen lists nodes that will show "Update needed" after the update;
    "Update all nodes" updates them one at a time through each node's signed
    update path, stopping at the first failure; an optional per-node "Keep
    this node up to date with the main" (off by default). A main never
    refuses to talk to an older node silently: the row says why.
15. **Everything through the API, CLI and MCP.** Every node action in the
    UI (join keys, ceiling read, maintenance, update, move) is an API route
    with `require_owner`/`require` like its neighbours. `hopperd node
    join|status|leave|ceiling` on the node, because an Oracle VM has no
    browser and the node's UI is loopback-only. MCP tools list node servers
    the same as local ones.
16. **Mobile.** Group headers collapse; the per-panel and per-node header
    wraps to two lines at 375 px; bulk select works by tapping row ticks. The
    acceptance checks include a 375 px screenshot.

### Cut or deferred

- **LAN panel finder in the add flow** (open question 10): deferred. Nobody
  asked for it and A's join command covers the remote case.
- **"Remember me" choice** (open question 6): no choice; 7 days sliding plus
  the existing "Don't keep me signed in on this browser" covers it.
- **Moving servers between B panels**: out of scope (point 10).
- **SSO / OIDC / proxy-header sign-in**: wanted (Pterodactyl #4026 71,
  MCSManager #1160), but a separate design; not part of multi-panel.
- **Fingerprint shown as proof in the browser**: stays cut (see TLS above).

### Defaults

| Setting | Default | Why |
|---|---|---|
| Node ceiling | Manage, with the warning | Open question 5; Crafty #763 uses the same three levels |
| Join method | Code; join command offered next to it | Code needs nothing pre-made; command for scripts and VMs |
| Join key life | 30 min, single use | Tailscale/Komodo practice |
| Offline alert | After 2 min, once, plus recovery | Coolify #4407 |
| Moved server's source | Kept, stopped | Pterodactyl #18 thread, #5555 |
| Node auto-update | Off | Updates are the most-cited breakage |
| RAM promised over total | Warn, allow | No overallocate maths |
| Per-panel list timeout (B) | 5 s | One slow panel must not hold the page |

### Phases (B first, then A)

Sizes are AGENTS.md's S/M/L. Every phase ends with its acceptance check run
for real (scratch panels with `HOPPERD_SERVICE=hopperd-test`, `--data` and
`HOPPER_LOOPBACK=1`; A's VM steps on a throwaway server on `a1-atm10`,
never a real one) and a review; UI phases add light and dark screenshots
with measured gaps and a 375 px one.

**B**

- **B0, CSP and allowed pages: done.**
- **B1, browser sessions, tickets, file links (M).** `login {bearer:true}`,
  7-day sliding life, Browser label + Revoke, ws ticket (subprotocol if the
  spike allows), `POST /files/{id}/link`, CORS headers on every refusal to an
  allowed origin (point 2), `web.remote` capability.
  *Accept:* Rust tests for each refusal and for ticket/link single use,
  expiry and binding; a page on a listed origin reads a 401 and a 403 body; a
  non-listed origin gets no CORS headers on any status.
- **B2, the web UI with several panels (L).** Per-panel `api`/`live`/`me`,
  namespaced routes, groups, add flow with probe and the four messages,
  per-panel header (point 8), offline/expired states, 5 s list timeout, panel
  list stored on the home panel.
  *Accept:* two scratch panels; stop one, the other group keeps working and
  the page's list renders in under 1 s with "Can't reach … · retry"; revoke
  the browser session on panel 2, its group shows "Sign in again" and nothing
  else changes; role chip and buttons follow each panel's role.
- **B3, bulk actions and filters (M).** Drag-pick selection across groups,
  bulk start/stop/restart/command with one grouped confirmation, per-server
  results, filter by panel and state.
  *Accept:* restart three servers on two scratch panels where the user is
  viewer on one: two restart, one is listed as skipped, each panel's audit log
  has its own entries.
- **B4, cross-origin files and polish (M).** Uploads, downloads, previews
  through one-use links, bare-URL audit, CA download help, the "add it as a
  node instead" pointer (shown once A ships), docs.
  *Accept:* 1 GB upload and zip download between two scratch panels on
  different ports; a list of every place that still says "Open on rig".

**A**

- **A0, protocol spike (S): done** (see "A0 as built"). Frame format, credit flow, ticket carrier,
  echo between a throwaway main and node; measure the node's idle memory and
  CPU with the tunnel open on `a1-atm10`.
  *Accept:* numbers written into this file; node idle under the 30 MB
  budget in `server-panel.md`.
- **A1, join and connect (L).** Code pairing, join keys and the generated
  commands (Linux systemd, Windows service), pending list, node token, CA pin,
  keepalive, reconnect with jitter (DNS included), revoke from both sides,
  `hopperd node join|status|leave`, step-by-step diagnostics (point 7),
  `HTTPS_PROXY`.
  *Accept:* on a throwaway `a1-atm10` setup, the generated command joins and
  the node appears within 10 s; block outbound traffic → "Reconnecting", then
  "Offline" at 60 s, one alert at 2 min, one on recovery; change the main's
  CA → node status says the certificate doesn't match; a used or expired join
  key is refused and logged.
- **A2, relay, ceiling, health (L).** Route allowlist, `Who::Main`, ceiling,
  namespaced ids, merged snapshot, History attribution, node header stats and
  RAM promised, maintenance mode, Node offline alert rule.
  *Accept:* the parity tests run the same handler tests through a fake node;
  a Look ceiling refuses start; `..` and symlink paths sent via the main are
  refused by the node; maintenance silences the alert.
- **A3, files, version skew, updates, apps (L).** Streamed files with credit
  windows, capability intersection, "Update needed" state, update one / all
  nodes, desktop and web groups for nodes.
  *Accept:* 2 GB upload to a node through the main with the main's memory
  growing by under 64 MB and no temp file; a node one tunnel version behind
  works, two behind shows "Update needed" read-only; "Update all" stops at
  the first failure.
- **A4, node-scoped access, bulk, API, MCP (M).** "All servers on node X"
  in user lists and API tokens, bulk actions and "All on node" through the
  main, audit filter by node, CLI `ceiling`, MCP tools.
  *Accept:* a user scoped to node X sees a server created there afterwards
  and nothing on node Y; a node-scoped token is refused on another node; bulk
  restart across main and node lands in one audit view.
- **A5, move or copy a server between nodes (L).** Point 10.
  *Accept:* move a throwaway 1 GB server from a scratch main to the
  `a1-atm10` node; cut the link mid-way, resume sends only the missing
  chunks; symlinks and file modes survive (test); the source stays, stopped
  and marked; a forced failure leaves nothing on the target; both Histories
  have the entry.
- **A6, hardening (M).** The security review list above plus join keys,
  node-scoped access and moves; docs; a full run on the VM.
  *Accept:* review findings closed or written down as accepted; docs list
  what a node can and can't do.

New open questions from this section: 11 and 12 below.

## B1 as built (2026-11-13)

Server side only; the web UI does not use it yet (B2). Capability
`web.remote` is now in `hello`.

### What a listed page can do

- **Sign in**: `POST /api/v1/auth/login` with `{ name, password, code?,
  bearer: true }`. Only from a page whose `Origin` is in `web_origins` and
  is not the panel's own; anything else (no `Origin`, the panel's own page,
  an unlisted page, `null`) gets 403 **before the password is checked**, so
  a refused page uses up no try. The same lockout (5 wrong, 10 minutes),
  the same 20/minute sign-in limit and the same "two-factor needed from the
  internet" rule apply. Answer: `{ ok, token, expires_in }`, no
  `Set-Cookie`. The audit log records the sign-in, right or wrong, as
  `/api/v1/auth/login (from <origin>)`.
- **Use the token**: `Authorization: Bearer <token>` from that same page.
  The session remembers its origin and is accepted **only with that exact
  `Origin`**: not with none (a script), not from another listed page, not
  as a cookie, not for SFTP. It lasts 7 days from last use and ends 30
  days after sign-in whatever its use (cookie sessions keep 30 from last
  use). At most 20 per account and page; a page signing in in a loop
  pushes out its oldest.
- **Live feed**: `POST /api/v1/ws/ticket` (only from a listed page) →
  `{ ticket, expires_in: 30, protocols }`, then `new WebSocket(url,
  protocols)` where `protocols` is `["hopperd", "hopperd-ticket.<t>"]`.
  The ticket rides in `Sec-WebSocket-Protocol`; the panel answers with
  `hopperd` only. *Spike result (open question 4): axum's
  `WebSocketUpgrade::protocols` handles this cleanly, so the ticket is
  never in a URL.* It opens one socket, from the page that asked, while the
  session that asked is still signed in; roles and server scope are read
  again when it opens.
- **Files**: `POST /api/v1/fs/{id}/link` with `{ path }` or `{ paths, zip:
  true }`, optional `inline` and `name` → `{ url: "/api/v1/link?k=<k>",
  expires_in: 60 }`. Path checks (`files::resolve`) run when asking and
  again when used; the file must exist; a folder only as a zip; at most
  1000 paths. Using the link needs no token and no `Origin` (it is a
  navigation); the session that asked must still be signed in, and its role
  and server access are checked again as a read of that server's files.
  Inline previews keep today's rules (text or raster image only, sandbox
  CSP, `nosniff`). The secret is in the query because the panel's logs
  record paths only.
- **Sessions**: `GET /api/v1/auth/sessions` (yours; every one for the owner
  and managers) shows `kind: browser | cookie`, `origin`, browser name, IP,
  last used, `expires` (the earlier of 7 days idle and 30 days from sign-in),
  `current`. The owner sees every session; a manager sees their own and the
  users', never the owner's or another manager's (IP, page, browser).
  `DELETE /api/v1/auth/sessions/{id}` ends one: your own, any for the owner,
  users' (not managers') for a manager.
- **Refusals are readable**: every answer to a listed page carries
  `Access-Control-Allow-Origin` and `Vary: Origin`, 401, 403 and 429
  included (`security::guard` wraps every check). Unlisted pages get no
  `Access-Control-*` header on any answer.

### Choices made where the design was open (safest option)

1. **Bearer sign-in needs a listed origin.** The design didn't say what
   `bearer: true` does without one. Refused: scripts use API tokens, and
   the panel's own page has its cookie.
2. **Sessions are bound to their page's origin.** Not in the design. It
   stops a token taken from one page being used by another page in the
   browser, and lets the panel treat the token as "that page" whatever
   headers come with it (next point). A script can fake `Origin`, so this
   is not protection against a stolen token outside a browser; the 7-day
   life, the session list and Revoke are.
3. **The access-management rule follows the token, not the headers.** B0
   checked "is this request from a listed other page" by `Origin` and
   `Host`. A copied browser-session token sent with `Host` made equal to
   the page would have looked like the panel's own page. Now a request
   signed in with a browser session always counts as that other page.
4. **Taking a page off the list signs its sessions out** (and so do hand
   edits of `hopperd.toml`, at the next start).
5. **Tickets and links are one-use even when the use fails** (wrong page,
   wrong purpose, expired): a guess or a wrong try burns it. They are
   bound to the session's hash, the asking page's origin and their purpose
   (a ticket is not a file link). Only their SHA-256 is kept, in memory;
   waiting at once: at most 64 per session (a looping page is refused),
   64 and 2 MB of paths per account over all its sessions, 2048 and 16 MB
   in all. An account at its share loses its own oldest waiting grant
   instead of being refused (it can't lock itself out of its console; it
   can't take the pool from others either). When the whole pool is full the
   biggest holder gives one up.
6. **Route names**: `/fs/{id}/link` instead of `/files/{id}/link` (the file
   routes are all under `/fs/`, so server scope applies as for any other
   file route) and `/link?k=` for redemption.
7. **Roles treat asking for a ticket or a link as a read**, so a viewer or a
   read-only token can open the live feed and download what they can
   already see. Neither request is written to the audit log (reads aren't).
8. **The session list is "who can get in"**: ending someone's session from
   another page is refused unless the owner's switch is on. Signing
   yourself out (`/auth/logout`) always works.

### Threat list

| Who | Can | Can't | Test (`cross_origin_tests.rs` unless noted) |
|---|---|---|---|
| A page on an **unlisted** origin | Send requests (browsers allow sending simple ones); a cookie sign-in works server-side as before B1 | Read any answer (no CORS headers on any status); get a bearer token; pass a preflight; open a ticketed socket; use the person's cookie (SameSite=Strict) | `unlisted_page_gets_nothing_usable`, `bearer_sign_in_only_for_listed_pages`, `tickets_one_use_bound_and_expiring`; `web_access::tests::preflight_for_unlisted_or_empty_list` |
| A page on an unlisted origin holding a **stolen token** | Nothing in a browser | Use it: the session only works with its own page's `Origin` | `remote_session_is_bound_to_its_page` |
| A **listed but compromised** page (XSS on it) | Everything the signed-in person may do day to day on this panel until the session is revoked, 7 days unused or 30 days old; read the tokens it holds | Change users, devices, tokens, passwords, two-factor, the certificate or end others' sessions (unless the owner turned that on); change the allowed-pages list (never); ride the person's cookie (dropped, no credentialed CORS); go beyond the person's role or servers; name a path outside a server folder; reuse a ticket or link; pile up sessions; keep a token alive past 30 days by using it; use up the shared ticket and link pool for others | `remote_session_cannot_manage_access`, `sessions_listed_and_revoked`, `file_links_follow_server_access`, `file_links_one_use_bound_and_expiring`, `remote_sessions_are_capped`, `browser_sessions_end_after_thirty_days`, `one_account_cannot_fill_the_grant_pool`, `same_origin_cookie_rules_unchanged` |
| A **script with a copied token** (made-up headers) | Use it with the right `Origin`, like the page could | Manage access by faking `Host` or leaving `Origin` out; use it for SFTP | `remote_session_cannot_manage_access` (made-up Host), `remote_session_is_bound_to_its_page` |
| Someone **guessing passwords** through a listed page | 20 sign-ins a minute per address, 5 wrong then 10 minutes locked, as on the panel's own page | More, by using preflights or another origin | `sign_in_limits_apply_across_origins`, `refusals_carry_cors_for_listed_pages` (429) |
| A **network observer** on plain HTTP (LAN, as allowed today) | Read tokens, tickets and links in transit, the same as cookies today | Anything over HTTPS; reuse a used ticket or link; use a ticket after 30 s or a link after 60 s | Not testable as a unit; `grants::tests::one_use_and_expiring` covers reuse and expiry. Plain HTTP stays private-network only (unchanged). |
| Someone reading **logs or history** | See paths; see which page tried to sign in, refused ones too | See ticket or link secrets: tickets travel in a header, links in the query, logs keep paths only; grants are stored hashed; a refused sign-in's log line holds only the origin, cut at 100 characters | `grants::tests::secrets_are_stored_hashed`, `file_links_one_use_bound_and_expiring` (query, not path), `audit_names_the_page_a_sign_in_came_from` (no ticket lines), `refused_bearer_sign_in_is_logged_with_its_origin` |
| A **manager** | See and end their own and the users' sessions | See the owner's or another manager's session details (IP, page, browser) | `managers_do_not_see_the_owners_sessions` |
| The **owner** after removing a page | Its sessions end at once | | `removing_a_page_ends_its_sessions` |

Added after the security review (tests in `cross_origin_tests.rs`):
an open live socket checks its sign-in every 5 seconds and closes when the
session is revoked, signed out, its page taken off the list or the user's
role changes (`live_feed_ends_with_its_session`); changing your password
ends your sessions on other pages except the one you changed it from
(`password_change_ends_browser_sessions`); a link request is at most
256 KB, keeps its paths tidied (`./` padding dropped), and all waiting
links together hold at most 16 MB of paths (`file_links_cannot_fill_memory`,
`grants::tests::waiting_links_hold_little_memory`).

Closed in a second pass over the review's low findings (`cross_origin_tests.rs`):
one account with dozens of sessions can't fill the shared grant pool, so
other people's live feeds and file links keep working
(`one_account_cannot_fill_the_grant_pool`,
`grants::tests::one_account_cannot_fill_the_pool`,
`grants::tests::one_account_cannot_hold_much_memory`); browser sessions end
30 days after sign-in however often used, and the list shows when
(`browser_sessions_end_after_thirty_days`); a bearer sign-in refused for its
page (403, before the password) is logged as `(from <origin>)`, origin cut at
100 characters, nothing else from the request
(`refused_bearer_sign_in_is_logged_with_its_origin`); managers don't see the
owner's or other managers' sessions (`managers_do_not_see_the_owners_sessions`).
Left as is: your own cookie sessions survive a password change (only browser
sessions end), the same as before B1; ending them too would sign the person
out of the panel's own page on every other device, which needs a UI decision.

Also unchanged and still tested: the setup code is never usable from
another page (setup has no bearer path; `trusted_local` refuses a foreign
`Origin`), TLS and the plain-HTTP rule, the cookie and `X-Hopperd` rules
for the panel's own page (`same_origin_cookie_rules_unchanged`).

## B2 as built (2026-11-13)

The web UI uses B1. Mock: Option A (groups in the one server list).

- **Saved list**: `GET/PUT /api/v1/auth/me/panels` on the home panel,
  per account, in `web-panels.json`: only `{ url, name }`, at most 32,
  names cut to 64 characters. No tokens. Tokens stay in this browser
  (`localStorage`, or this tab only when "Don't keep me signed in" is
  ticked).
- **Add flow** (`AddPanel.svelte`, `check()` in `lib/panels.svelte.js`):
  address parsed, then `hello` (does it answer, is it new enough, is it
  set up), then a CORS call (does it list this page). A failure that
  isn't CORS is told apart by a `no-cors` probe: it answers but refuses →
  "doesn't let this page in yet" with the origin to copy; no answer over
  https → maybe the certificate. Mixed content is caught before any call.
  A panel that isn't set up can't list a page yet, so in practice it shows
  as "not listed".
- **Groups** (`ServerList.svelte`): the home panel first, then each added
  panel, each with health (version, latency or "this panel"), a role chip
  and a ⋯ menu (Open in browser, Try again, Sign out of it, Remove from
  this page). Fold state per browser.
- **Per-panel states**: loading, ok, signin, reconnecting (5 s timeout
  hit; servers shown dimmed with "Seen X ago"), offline (after 15 s),
  refused, old, setup. Every call to a panel has its own 5 s timeout; the
  home list never waits for them.
- **Session ended**: a 401, or the live socket closing and `/auth/me`
  then failing, puts only that group in signin ("Your sign-in to X ended.
  Sign in again."). Signing in reopens its socket.
- **Server pages** on another panel: `/p/<panel>/server/<id>/…`,
  `ServerPage` gets the panel, so every call, the ticket WebSocket and the
  one-use file links go to it. Buttons follow that panel's role
  (`session.role`); a viewer sees "View only".
- **Not yet**: inline previews and backup downloads from another panel
  (B4), live `changed` events from other panels, servers of a down panel
  across a reload, "add it as a node instead" (needs phase A), server
  alerts on other panels' pages.

## B3 as built (2026-11-13)

Bulk actions and filters in the web UI's server list. No new backend route.

- **Calls**: each server gets the same call a single click on its page
  sends (`POST /servers/{id}/power`, `POST /servers/{id}/command`), to its
  own panel with that panel's own sign-in (`post(path, body, { panel })`).
  So every panel checks the role itself (`security::refusal`) and writes
  its own audit log. Panels run side by side; on one panel the servers go
  one after another. The plan and the skip rules are in `lib/bulk.js`
  (unit tests in `tests/bulk.test.mjs`).
- **Skipped, never sent**: every server on a panel where the account is a
  viewer ("Your account on X can only look."), signed out ("Sign in to X
  again first.") or down ("Can't reach X right now."); a server that is
  stopping; start of a running server, stop or command to a stopped one.
  The server still refuses anything a client gets wrong. Restart of a
  stopped server sends `start` and says so in the question.
- **Selection** (`ServerList.svelte`): a "Select" icon button in the list
  header turns on tick boxes; click or drag across rows in every group
  (`lib/dragpick.svelte.js`, `lib/selection.svelte.js`), Ctrl+A picks all
  shown, Esc stops. Only shown rows count: a pick hidden by a filter or a
  folded group isn't acted on. The shared `SelectionBar` holds Start, Stop,
  Restart, Command (two rows at the list's width; under 760 px the count
  and close on one row, the four actions on the next).
- **One question per action** (`BulkAction.svelte`): grouped by panel,
  each row says what happens or why it is skipped. Players online on
  servers that would stop or restart need a tick ("N players are online.
  Restart anyway"), never typed names. The same dialog then shows each
  server's result (done, failed with the panel's reason, skipped) and a
  tally ("2 done · 1 failed · 1 skipped").
- **Filters**: by panel (a dropdown, when there are added panels) and by
  state (All, Running, Stopped, Crashed: starting and stopping count as
  running, a failed wake as crashed). Saved per browser
  (`hopperd.servers.filter`). The folded rail ignores filters and has no
  selection.
- **Desktop**: Hopper desktop's server list has no selection or bulk
  actions yet; this is web UI only for now.
- **Not yet**: an "All on <panel>" pick from a group's ⋯ menu, bulk
  backups, a players-online check seen on screen (covered by a unit test).

## A0 as built (2026-11-13)

Code: `crates/hopperd/src/tunnel/` (`frame.rs`, `mux.rs`, `ws.rs`, and the
throwaway `spike.rs`). Nothing in the panel uses it yet; the only entry is
the hidden `hopperd tunnel-spike main|node` subcommand and, for measuring a
whole panel as a node, `HOPPERD_TUNNEL_SPIKE_NODE=<folder>` (read once in
`serve`). One new dependency: `tokio-tungstenite` 0.29 (already in the tree
under axum) for the node's client side, and futures-util's `sink` feature.

### The protocol

- **Transport**: one WebSocket over TLS, node to main
  (`wss://main/api/v1/node/connect`), as designed. Plain `ws://` is refused by
  the client. One binary WebSocket message carries exactly one frame, so
  WebSocket does the length framing; text messages are an error. Both sides
  cap WebSocket messages and frames at one tunnel frame (64 KiB + 5) before
  buffering, and keep a 16 KiB read buffer.
- **Frame**: 1 byte type, 4 bytes stream id (big-endian, 0 = the
  connection), then the payload. Types: `HELLO` (JSON), `OPEN` (stream head,
  JSON, ≤ 16 KiB), `DATA` (≤ 64 KiB), `CREDIT` (u32), `CLOSE` (half close,
  empty), `RESET` (u16 code + reason ≤ 256 bytes), `PING`/`PONG` (u64),
  `GOAWAY` (code + reason). Connection frames on a stream id, stream frames
  on 0, unknown types, wrong fixed sizes and non-UTF-8 reasons are refused.
- **HELLO** first, each way, within 10 s: `{tunnel, version, capabilities,
  window}`. Tunnel versions equal or one apart talk; two apart get `GOAWAY
  VERSION` (the "Update needed" state in A3 starts here).
- **Versions (A3)**: three numbers, kept apart. The *tunnel* version
  (`frame::TUNNEL_VERSION`, now 1) is the framing in this section; it
  changes only when frames change, and decides whether two sides talk at
  all. The *hopperd* version (`version` in HELLO, the release number) is
  what "Update" compares: a node behind the main's release can be updated
  from it. The *capabilities* (HELLO's list, the same names as
  `/api/v1/hello`) say what each side can do: the main keeps the ones both
  sides name (`nodes::intersect`), per node, and hides what the node can't
  do (A3: `nodes.update`, `relay.stream`). `HOPPERD_TEST_TUNNEL` and
  `HOPPERD_TEST_VERSION` make a scratch panel speak another tunnel version
  or claim another release, to try skew by hand.
- **Streams**: the main opens even ids, the node odd (both may open, no
  coordination); ids only grow, reuse or the other side's parity is a
  protocol error. A stream's `OPEN` head says what it is (A2: method, path
  under the allowlist, acting user and role). Each direction closes on its
  own (`CLOSE`, like a TCP FIN), so a request body and its answer share one
  stream; `RESET` kills both directions; dropping both halves of a stream
  that hasn't finished both ways sends `RESET CANCEL`. At most 64 open
  streams per connection, both directions together; a peer's extra `OPEN`
  gets `RESET REFUSED`, our own extra `open` fails locally. OPENs reach the
  wire in id order, and a `RESET` is never sent ahead of its own stream's
  `OPEN`. Empty `DATA` and a second `CLOSE` on a stream are protocol errors.
- **Credit**: each side announces its per-stream receive `window` in HELLO
  (default 512 KiB, accepted range 4 KiB to 16 MiB). A sender takes credit
  before each `DATA` and waits at zero. The receiver returns credit
  (`CREDIT n`) only after the *application has read* half a window, so each
  stream buffers at most one window on the receiving side; data past the
  window is a protocol error that closes the connection (`GOAWAY PROTOCOL`).
  Small `DATA` frames waiting to be read are merged into 64 KiB chunks, so
  a window of one-byte frames is a handful of allocations, not 500,000.
  `CREDIT`, `PING`, `PONG` and `RESET` go on a small priority queue ahead of
  data, so a full data queue never holds back the credit the peer is waiting
  for. The data queue to the socket is 8 frames (512 KiB). Worst case per
  connection: 64 × 512 KiB = 32 MiB buffered; A3 adds a per-connection cap
  for the main (many nodes) and for the node (30 MB budget).
- **Keepalive**: both sides `PING` every 20 s; *any* frame received counts as
  alive; nothing for 60 s = dead (`Timeout`). A frame that can't be written
  for 60 s (a peer that talks but never reads) is also `Timeout`. The node
  reconnects 1 s doubling to 60 s plus up to 1 s jitter; the backoff starts
  over only after a connection that lasted 60 s. Every connect step,
  including the WebSocket upgrade, has a time limit. RTT comes from `PONG`
  (for the node header in A2).
- **Tickets / auth**: the node key goes in `Authorization: Bearer` on the
  upgrade request; the main compares SHA-256 hashes in constant time and
  answers 401 before upgrading. No ticket in the URL or a subprotocol (no
  browser is involved, and the key must not land in access logs). A 401/403
  **stops** the node ("the main refused this node's key"), proven when the
  spike's main had a new key. Browser tickets and one-use file links (B1)
  stay on the main: the main checks them and relays with its own `OPEN`, the
  node never sees a browser credential.
- **Pinning**: the node's TLS trusts only the main's CA from pairing (no
  system roots); connect errors are classed as address / DNS / TCP /
  certificate mismatch / key refused / other (point 7).
- **Change to the design**: the per-frame "main id and per-connection
  counter" against replay (Trust model, item 6) is dropped. TLS already
  numbers and authenticates every record, and stream ids live only for one
  connection, so a frame from an old connection has nothing to apply to.

### Tests

`cargo test -p hopperd tunnel` (16, all pass; 5 repeated runs clean):
round trip of every frame and the header layout; refusal of short,
oversized, unknown, misplaced and malformed frames; 1 MiB echo through a
64 KiB window and clean close (stream forgotten on both sides); **sender
stops at exactly one window on the wire** until the reader reads, and
reading under half a window gives no credit; drop → `RESET CANCEL`, explicit
reset keeps its code, send on a reset stream fails instead of hanging;
half close; stream limit → `REFUSED`; sending past the window, a peer using
our id parity or reusing an id → protocol error; version one apart ok, two
apart refused; silent peer → `Timeout` within 190 to 600 ms of a 200 ms
limit; pings keep an idle link alive and give an RTT, and a dead peer fails
waiting streams; `GOAWAY` ends both sides. Whole crate: 195 passed.
After the review below: 23 tunnel tests (5 repeated runs clean), whole
crate 202 passed.

### A0 security review (2026-11-13)

Read against the threat list for A ("the tunnel: frame parsing, stream
limits, memory caps, reconnect storms"). Each fixed finding has a test that
was written first and failed on the old code.

| # | Rank | Finding (attack) | Status |
|---|---|---|---|
| 1 | High | **Per-frame cost not bounded.** The window counted bytes, but every `DATA` frame was its own entry in an unbounded queue: a peer could turn one 512 KiB window of 1-byte frames into ~500,000 allocations per stream (tens of MB × 64 streams), and empty `DATA` or repeated `CLOSE` frames cost no window at all, so they queued without any limit while the reader was busy. | Fixed: per-stream inbox merges small chunks; empty `DATA` and a second `CLOSE` are protocol errors. Tests `tiny_data_frames_are_merged_while_waiting`, `empty_data_and_double_close_are_refused`. |
| 2 | Medium | **Our own streams were capped at 4096, not 64.** A compromised node that accepts every stream the main opens could make the main hold 4096 × 512 KiB = 2 GiB. | Fixed: one limit for both directions. Test `our_own_opens_are_capped_at_max_streams`. |
| 3 | Medium | **Concurrent opens could reach the wire out of id order** (id taken under the lock, queued after it), and the peer then drops the whole connection ("stream id 24 not allowed", reproduced). Normal A2 load (many browser requests at once) would trigger it. | Fixed: queue room reserved first, id taken and OPEN queued under one lock. Test `concurrent_opens_keep_ids_in_order`. |
| 4 | Medium | **A RESET could overtake its own OPEN** (RESET rides the priority queue): a stream dropped while its OPEN was queued got opened on the peer afterwards and kept for ever, until the peer's 64 slots ran out and it refused everything. | Fixed: the writer holds such a RESET until its OPEN is written. Test `a_reset_never_overtakes_its_open`. |
| 5 | Medium | **A peer that talks but never reads** stalled our writer and every stream behind it: the read side saw frames, so no timeout, and only a full control queue (~85 min of pings) ended it. | Fixed: every write has the 60 s limit (`Timeout`). Test `a_peer_that_never_reads_times_out`. |
| 6 | Low | The node's WebSocket upgrade had no time limit: a main that finished TLS and never answered held the node for ever, outside its reconnect loop. | Fixed (15 s). |
| 7 | Low | `wss://[::1]/...` (IPv6 without a port) was read as host `[:` port `1]`. | Fixed, test `addresses_with_and_without_ports`. |
| 8 | Low | The spike node's backoff started over after every successful hello, so a main that accepts and drops at once gets each node back every 1–2 s. | Fixed in the spike: starts over only after 60 s up. A1's real node loop must keep this rule. |
| 9 | Low | A RESET from dropping a stream uses `try_send` and is lost when the control queue (256) is full, leaving the stream open on the peer. Only on a stalled socket, which #5 now ends within 60 s. | Listed. |
| 10 | Low | PING and `CREDIT 0` floods cost CPU only (one small frame in, at most one out; a peer that doesn't read fills the control queue and is dropped). | Listed; per-connection frame rate limit in A3 if measured. |
| 11 | Low | The spike main's 401 has no rate limit, and its key file uses default permissions. | Spike only; A1 adds the connect-failure limit and stores only the hash. |
| 12 | Low | `HOPPERD_TUNNEL_SPIKE_NODE` in `serve`: whoever can set the panel's environment can already run code as it; the variable only makes it dial the URL in that folder, pinned to the CA there, and answer echo and stats (its own RSS and CPU). | Listed; delete with the spike when A1's node mode lands. |

Checked and found sound: frame decode (short, oversized before
buffering on both sides, unknown types, connection/stream id placement,
exact fixed sizes, UTF-8 reasons); stream ids (parity, only growing, no
u32 wrap: our side stops at `u32::MAX - 2`, a peer at `u32::MAX` only
exhausts itself); credit math (`checked_add`, capped at 2^31, window
increments can't exceed the window); DATA/CREDIT/CLOSE after a stream
ended are ignored only for ids this connection used, otherwise protocol
errors; HELLO size, timeout, version and window range; key compare
(SHA-256 of both sides, constant-time fold, 401 before upgrading, the key
never in a URL or log); 401/403 stops the node; CA pinned with no system
roots; WebSocket text messages refused; no browser credential reaches the
node. The spike is reachable only as `hopperd tunnel-spike ...` (parsed
before the normal arguments, so not from the web UI) or with the variable
above; a normal `serve` without it does nothing tunnel-related, and
`tunnel-spike main` binds 127.0.0.1 only.

### Measured

Local (Windows, two scratch `hopperd tunnel-spike` processes on
127.0.0.1, `HOPPERD_SERVICE=hopperd-test HOPPER_LOOPBACK=1`, scratch
`--data`): small echo 0.2 ms; **200 MiB echoed both ways at once in 0.7 s
(306 MiB/s each way), CRC equal**. Node working set 15.8 MiB idle after
that. With a 64-frame data queue the node stayed at 30.9 MiB after the same
transfer, which is why the queue is 8 frames.

Oracle VM `a1-atm10` (aarch64, 2 OCPU, glibc 2.39, the real panel and
Minecraft server busy on the same box and untouched). Binary cross-built
with `cargo-zigbuild` for `aarch64-unknown-linux-gnu.2.35` (`local`
profile); throwaway processes in `/tmp/a0-spike` on their own ports and
data folders, loopback only, SFTP and auto-update off; the main ran on the
maintainer's PC, reached through an SSH reverse forward (RTT ~98 ms). Three
processes side by side for 7.5 min, sampled every minute with `ps` and
`/proc/<pid>/stat`:

| Process on the VM | RSS (steady) | CPU idle (after the first minute) |
|---|---|---|
| Scratch panel, no servers, **no tunnel** | 20.1 MB | 0.047% (170 ms in 360 s) |
| Same scratch panel **plus tunnel** (`HOPPERD_TUNNEL_SPIKE_NODE`) | 22.2 MB | 0.053% (190 ms in 360 s) |
| Tunnel only (`tunnel-spike node`) | 14.4 MB | 0.006% (20 ms in 360 s) |

So the tunnel costs a node about **2 MB and ~0.005% CPU**, and a node with
an empty panel sits at **22 MB, inside the 30 MB budget** with ~8 MB to
spare. Both nodes first echoed 16 MiB each way: 0.2 MiB/s each, the
maintainer's upload link (a raw `ssh cat` upload measured 0.5 MiB/s for the
two together), not the window (512 KiB at 98 ms allows ~5 MiB/s per
stream). For comparison, the VM's real panel (one Minecraft server, 74 min
up) was at 29.6 MB RSS: a node with servers will be close to the budget
from the panel itself, not from the tunnel. A2/A3 should keep the merged
snapshot and stats off the node (the node sends what it already makes) and
re-measure with a server attached.

### Left for A1 and later

The spike's main serves the route on its own listener with a fresh key per
run and a CA in its scratch folder; A1 moves the route into the panel's
router behind pairing, stores the key hash, and adds the rate limits.
Per-connection memory cap (A3), relay heads and the allowlist (A2), and the
node's `status` diagnostics (A1) are not built.

## B4 as built (2026-11-13)

Files of a server on another panel: downloads, uploads and previews from
the page you have open. The other panel's sign-in is a token, which a plain
`<a href>` or `<img src>` can't send, so:

- **Downloads and "Open in a new tab"** ask that panel for a one-use link
  (`POST /fs/{id}/link`, B1) and follow it. The file goes straight from that
  panel to the browser, nothing passes through this page. `lib/panelfiles.svelte.js`
  `linkFor(url)` turns this panel's own file URLs into a link body
  (`lib/filelinks.js`, unit tested); `download()` uses it, so every download
  button (Files, Worlds, Editor, Backups, zip entries) works on both. The
  new tab is opened first and pointed at the link when it arrives, so
  popup blockers leave it alone.
- **Pictures and text shown in the page** (Files thumbnails and preview,
  the file viewer, a text or picture inside a zip, server icons in the list,
  header and hero) are read with the token (`fetchFrom`) into a blob URL:
  `use:panelImg` for `<img>`, `panelSrc()` for components. Thumbnails load
  when they come into view, so a big folder doesn't read every picture.
- **Uploads** (Files, mod jars dropped on Mods) go to that panel's API with
  its token: `fs` uploads in 8 MB chunks, mod jars streamed.

### Backend

- `POST /fs/{id}/link` takes two more kinds, each on its own:
  - `{ path, entry, inline? }`: one entry of one zip (`zip` refused with it,
    entry 1..4096 chars). Redeemed as `GET /fs/{id}/zip-entry`.
  - `{ backup }`: one backup of the server, by name. The name passes
    `backup::check` and must exist when the link is made. Redeemed as
    `GET /servers/{id}/backups/{name}/download`.
  - The grant stores the entry or backup name (`Purpose::File { entry }`,
    `Purpose::Backup`); redeeming re-checks access as that GET route
    (`security::refusal`) and every path again, like B1's file links.
- `GET /api/v1/ca.crt`: the panel's certificate authority (public, like
  `/hello`), as a download named `hopper-server-<name>-ca.crt`; 404 when the
  panel has none (ACME only).
- Capability `web.files` on `/hello`: a panel that can make backup and
  zip-entry links. Older panels say "runs an older Hopper Server" instead
  of failing on a 400.
- Download and zip-entry bodies moved into `send_backup` / `send_entry` so
  the route and the link share one copy.

### Add a Hopper Server: certificate help

The certificate problem now offers two ways: "This browser, for now" (open
its page, accept the warning) and "This PC, for good": download its
certificate (over plain http on the same port, which the panel answers on
your own network and Tailscale) and install it as a trusted root, with
steps for Windows, Mac and Firefox and the warning that this PC then
trusts every certificate that panel makes. The unreachable case with a
possible certificate offers the same. "Add it as a node instead" (A) is
there but hidden until `/hello` lists `nodes`.

### Home-only places (audit)

Fixed to work for another panel's server: Files thumbnails and preview,
file viewer (picture, new tab, zip entry text, picture and download),
Backups download, zip-entry download, mod jar drop upload, the server page
hero picture, `Avatar` (list, header, Overview).

Left home-only, on purpose:

- Dragging a file out of Files to the desktop (`DownloadURL`) needs a
  URL ready when the drag starts; a one-use link can't be. Off for other
  panels; Download works.
- New server, incoming pack uploads (`upload.js`, New server only), the
  Overview dashboard, Addresses, panel Settings (users, certificate, the
  MCP address from `location.origin`) and the live socket of this panel:
  these are about this panel. Another panel's are on its own page (the
  group's "Open in browser").
- "Change this on the panel's own page." (refusal of access settings from
  another page) is by design (B1).
- Not done: CA help in the server list's offline state for an added panel
  (only in Add a Hopper Server).

### Checked

- Two scratch panels on 8851 and 8852 (loopback), the page on 8851, panel
  8852 added and signed in to. 1 GiB of random bytes uploaded to a server
  on 8852 in about 4 s (SHA-256 the same on disk); a zip of it plus a
  folder downloaded through a one-use link in about 2 s (entry SHA-256 the
  same). Panel 8852's memory, sampled every half second over the uploads,
  downloads and a backup: private 13.5 MB idle, 28.5 MB peak; working set
  39.5 MB peak.
- Backup download, picture thumbnails and viewer, a text file inside a jar,
  the CA download: all from the 8851 page against 8852.
- `cross_origin_tests.rs`: zip-entry links, backup links (viewer allowed,
  access removed refused, bad names refused), uploads from a listed page
  (chunks, path traversal refused, mod jar, unlisted origin gets no CORS,
  viewer refused), the CA download.

### Threat list additions

- A backup link names a backup, never a path: `backup::check` refuses
  separators and dots, and the link only opens that one file.
- A zip-entry link reads inside one zip the account could already read;
  the entry name never touches the file system.
- `ca.crt` is public: it's the same certificate every TLS handshake shows.
  Installing it is the player's choice, with the warning above.
- Zip-entry reads still buffer the entry in memory (up to 512 MB), as before
  B4. Uploads and every other download stream.

## A1 as built (2026-11-13)

Nodes join a main, stay connected and can be removed from either side. A
node still serves nothing through the tunnel: the main's requests are A2,
so a node refuses every stream the main opens. The ceiling (Look, Operate,
Manage) is chosen and stored at join time; A2 enforces it.

### Joining

- **Join command** (Settings › Nodes › Make a join command, owner only).
  The main makes a key `hn_` + 48 hex, kept only as a hash, good once for
  30 minutes (listed for 24 h after). The command pins the main's CA by
  fingerprint and installs the service:
  - Linux: `curl … hopperd-<v>-linux-<arch>.tar.gz | sudo tar xz -C /usr/local/bin hopperd && sudo hopperd node join <url> --key … --ca <fp> --service`
  - Windows (admin PowerShell): download the zip to `%TEMP%`, then
    `hopperd.exe node join … --service` (`service install --copy`).
  `nodes::commands` refuses to build a command from anything that isn't
  shell-safe (unit tested); the dialog shows the key once.
- **Code pairing** (`hopperd node join <url>` with no key). The node trusts
  whatever certificate the main shows *for this request only*, sends a
  request, and both sides show the same 6-digit code (`nodes::node_code`:
  `pair_code` over the fingerprint, the main's request id and secret, and a
  nonce the node committed to by hash in the request and reveals on its
  first status ask; the main shows the code only after the reveal). The owner
  compares, picks the ceiling and allows or refuses. Requests last 5
  minutes, at most 8 at once. Only after "Allow" does the node save the CA
  it saw.
- Either way the node gets a token `hnt_` + 64 hex; the main keeps its hash
  (constant-time compare). `node.json` (0600) in the node's data folder
  holds the main's URL, name, CA (PEM), fingerprint, node id, token and
  ceiling. A running panel watches it every 2 s, so joining or leaving
  doesn't need a restart.

### Connecting

- The node dials `wss://<main>/api/v1/node/connect` with the token. TLS
  offers ALPN `hopperd-node` first, so the main always answers with its own
  certificate (also when it has a Let's Encrypt one), then `http/1.1`.
- Pinning is a custom rustls verifier: the chain must end at the pinned CA
  (`Pin::Ca`), or contain a certificate with the fingerprint pinned at join
  time (`Pin::Fingerprint`). A host-name mismatch under the pinned CA is
  accepted, since nodes often reach the main by an address its certificate
  doesn't list. Limitation: a TLS-terminating proxy in front of the main
  breaks the pin (it shows its own certificate); the node then says the
  certificate doesn't match.
- `HTTPS_PROXY` / `https_proxy` (http:// proxies, optional basic auth) and
  `NO_PROXY` are honoured through a CONNECT tunnel.
- Keepalive is A0's: ping every 20 s, dead after 60 s of silence.
- Reconnect: 1 s doubling to 60 s, plus up to 1 s of jitter, with a fresh
  DNS lookup each try. The backoff resets only after a connection that
  lasted 60 s. A certificate mismatch waits the full 60 s between tries; a
  refused token or a "removed" goodbye stops trying.

### On the main

- States: Connected (heard from under 30 s ago), Reconnecting (30–60 s),
  Offline (60 s or more), Not connected yet (never). A node gone 2 minutes
  raises one alert (`alerts::node_state`, webhook event `node`), and one
  "back online" when it returns. The alerts' 10-minute quiet time stops a
  flapping node from repeating the alert.
- Routes, all `require_owner` plus `is_owner` (the real owner; managers and
  managing devices get 403, tested): `GET /api/v1/nodes`,
  `POST /api/v1/nodes/keys`, `DELETE /api/v1/nodes/keys/{id}`,
  `POST /api/v1/nodes/pending/{id}/answer`, `DELETE /api/v1/nodes/{id}`
  (closes the tunnel with goodbye "removed").
- What nodes call, no session: `/api/v1/node/join`, `/node/request`,
  `/node/request/status`, `/node/check` (token; a one-off diagnostic),
  `/node/leave`, `/node/connect`. Wrong keys, tokens and unknown requests
  count per address: 10 in 10 minutes, then 429 for the rest of the
  window. `join` and `request` are in `security.rs`'s guessable list.
- Audit log: key made or removed, joined (key or code), allowed, refused,
  wrong key with the reason (unknown, used, expired), connected,
  disconnected, removed, left. The status polling route stays out of it.
- `nodes.json` in the main's data folder: nodes (token hash, ceiling,
  joined, last seen) and keys (hash, expiry, used by).

### On the node

- `hopperd node join <url> [--key K --ca FP] [--name N] [--data DIR] [--service]`
- `hopperd node status`: the running panel's state when its status file is
  fresh (under 75 s), else a one-off check that prints each step: address
  found, port open, certificate matches, key accepted (point 7).
- `hopperd node leave`: tells the main (8 s limit) and forgets it either
  way; if the main couldn't be told, it says to remove the node there too.
- Settings › Nodes on a node shows "This panel is a node": the main, the
  state chip, the last message, the pinned fingerprint and Leave
  (`/api/v1/this-node`, real owner only). The add and list cards stay
  hidden there until the node has nodes or keys of its own.
- The A0 spike (`tunnel-spike`, `HOPPERD_TUNNEL_SPIKE_NODE`) is deleted;
  `api::ws_allowed` is test-only now (the spike was its last caller).

### Web UI

Mock option A (https://claude.ai/artifact/9YKfYfAzcgmLFd1VtMh8gF): it
matches Access & devices, and the one-time key sits in a dialog instead of
staying on the page. Settings › Nodes is for the real owner only
(`catsFor(owner, trueOwner)`). Cards: Add a node (button, the code-join
hint, unused keys with Cancel), Waiting for you (code tiles, ceiling
segmented control, Refuse / Allow), then the node list (dot, name, line,
state chip, ceiling chip, ghost remove with a confirmation). The command
dialog shows the key once, with Linux / Windows underline tabs and Copy.
Shared fixes it needed: `.dot.warn` (the Reconnecting dot was grey) and
the neutral `.chip` fill, which was a dark-only colour and invisible in
light themes (now `color-mix(var(--text) 8%)`, the same colour in dark).

### Checked

- Two scratch panels on loopback with a pausable forwarder between them.
  The generated command joined and the node showed Connected 2.4 s later.
  With traffic held: Reconnecting at 30 s of silence, Offline at 60 s, the
  alert at 120 s, "back online" on release, reconnected at once. Main CA
  reset: `node status` says the certificate doesn't match and to leave and
  join again. A used key: refused, logged "join key already used". Expired
  keys: unit test (not waited out). Code pairing allow and refuse through
  the web UI.
- Not checked: the VM run (optional; skipped), a real `--service` install
  (the command is unit-tested, not run), `HTTPS_PROXY` against a real
  proxy (parsing and `NO_PROXY` are unit-tested).

### Deferred / open

- Token rotation every 30 days; a node web UI on loopback only; a badge
  for waiting requests in the settings list.
- Light theme: the shared state chips (green 2.7:1, gold 2.4:1, red 3.8:1
  text contrast) are under 4.5:1 everywhere they're used, not only here.
- The segmented controls in CopyServer and BackupTargets fill the chosen
  option orange; the rule (and Nodes) uses the selected tint.

### A1 security review (2026-11-13)

Read against the A threat list (pairing, tokens, revocation, owner-only
routes, pinning, proxy, generated commands, secrets in output, node data
files). Each fixed High and Medium has a test written first that failed on
the old code.

| # | Rank | Finding (attack) | Status |
|---|---|---|---|
| 1 | High | **A machine in the middle could choose the code.** The main made the id and secret the code came from and sent them to the node, so a middle that showed the node its own certificate (code pairing trusts any certificate until Allow) asked the real main itself, worked out the main's code from its answer (the main's fingerprint is public), and tried secrets for the node until `pair_code(its fp, id, secret)` equalled the main's code: about a million SHA-256s, under a second. The owner saw matching codes and allowed it; the middle got the node token, and the node pinned the middle as its main. | Fixed: commit and reveal. The node sends only `sha256(nonce)`, the main then picks id and secret, the node reveals the nonce, and the code covers all of them (`nodes::node_code`). The main lists a request, and lets it be allowed, only after a reveal that matches; a wrong reveal ends it; a request without a commitment gets 400. A middle now matches by luck only (1 in 10^6 per try, each try a request the owner sees). Test `the_code_needs_the_nonce_the_node_committed_to`. |
| 2 | Medium | **Removing a node during its hello left it connected.** The token was checked before the upgrade and the node attached after both hellos (up to 10 s, and the node decides when to send its own). A removal or `leave` in between left a live tunnel for a node no longer in `nodes.json`, invisible in Settings › Nodes. | Fixed: attaching re-checks the node and its token hash under the same lock as removal, and closes with "removed" otherwise. Test `a_node_removed_during_hello_does_not_stay_connected`. |
| 3 | Medium | **The wrong-guess table had no size limit.** One entry per address for the 10-minute window; the clean-up only dropped expired ones. An IPv6 client (a /64 is 2^64 addresses), or a LAN client sending `X-Forwarded-For` (trusted from private peers), could grow it without end. | Fixed: IPv6 counts per /64, at most 4096 addresses tracked, the oldest forgotten past that. Test `wrong_guesses_from_many_addresses_use_bounded_memory`. |
| 4 | High (not A1) | **Desktop pairing has the same flaw as #1**: `Auth::pair_request` makes id and secret on the panel and the PC computes the code from the certificate it saw. | Fixed: the same commit and reveal, shared in `hopper-core` (`hopperd::pair_nonce` / `pair_commit` / `pair_reveal_matches` / `committed_pair_code`; `nodes::node_code` calls the last). The PC sends `commit` with `/pair/request` and reveals `nonce` on its first `/pair/status`; the panel lists and allows a request only after a matching reveal, a wrong reveal ends it. A panel says `pair.commit` in its hello's capabilities. Never the old flow as a fallback: a new panel answers a request without a commitment with 400 "This version of Hopper is too old to pair with this Hopper Server. Update Hopper, then pair again."; a new PC checks the hello first and says "This Hopper Server is too old to pair with this version of Hopper. Update Hopper Server, then pair again." without sending anything. Paired devices (tokens) are untouched. Residual: a middle that learns the panel's code before revealing can churn unrevealed requests, but `/pair/request` is on the 20-a-minute guess limit and 8 may wait, and the PC waits only 5 minutes: about 100 tries per pairing from one address, 1 in 10,000. Tests `a_machine_in_the_middle_cannot_choose_the_code` (fails on the nonce-free code: the brute force finds a match in ~3 s), `pair_tests::*`, `an_older_panel_is_told_to_update`. |
| 5 | Low | Text from the main (errors, its name) was printed raw in the joining terminal, often a root one, while nothing about the main is proven (code pairing): escape sequences could redraw the screen. The ceiling and token were saved without checks. | Fixed: control characters dropped, lengths capped; node id and token must be plain ASCII, the ceiling one of look / operate / manage. Test `answers_from_the_main_are_checked_before_printing_or_saving`. |
| 6 | Low | A host with a line break or space (typed address, `node.json`) went straight into the proxy's `CONNECT` line and the `Host` header. | Fixed: hosts are letters, digits, `.-_:` only. Test `addresses_with_and_without_ports`. |
| 7 | Low | Join keys were found with a plain `==` on their hashes. | Fixed: constant-time, like tokens. |
| 8 | Low | The join key is on the command line: visible in `ps` to other users of the node for the seconds the join takes, and in shell history. A local user who copies it in time can join first (the real join then fails loudly, "already used"). | Listed: read it from stdin (`--key -`) in the generated command if this matters. |
| 9 | Low | The Linux command pipes a GitHub download into `sudo tar` without checking a signature (`autoupdate.rs` checks signed releases; the join command doesn't). | Listed. |
| 10 | Low | The wrong-guess check runs before the token check on `/node/connect`, and `X-Forwarded-For` is trusted from private peers, so someone on the main's LAN can lock a node's address out for 10 minutes (the node keeps retrying and recovers). 8 waiting code requests in all can also be kept full by one sender rotating addresses (join commands still work). | Listed. |
| 11 | Low | Anonymous code requests put their name (60 chars, no control characters, may contain quotes) in the audit line `node "…" asks to join`. | Listed. |
| 12 | Low | The systemd unit writes `--data "<path>"` without escaping `"` or `%` (pre-A1 `service.rs`; the join command never passes `--data`). | Listed. |

Checked and found sound: keys 96 bits and tokens 128 bits from the system
generator, stored as SHA-256 only, tokens compared in constant time; one
key, two joins at once: one wins (one lock); expiry checked on use, keys
listed only while usable; request status needs the 128-bit secret and
wrong ones count toward the limit; every `/nodes` route needs the real
owner (`Who::Owner`: not a manager, device, API token) and the changing
ones are refused from listed other pages and their bearer sessions
(`web_access::manages_access` covers `/nodes` and `/this-node`); removal
and `leave` close the live tunnel with "removed"/"left" and the node stops;
`wss` only, the CA pinned with no system roots, the host-name mismatch
accepted only under that CA, which signs only this panel's own server
certificates; with a proxy TLS still runs end to end through `CONNECT`, the
proxy's password goes only to the proxy and never into messages or logs;
`--service` doesn't pass the key on (only `--data`); the token is never in
a URL, log line, audit entry or CLI output; `node.json` is written 0600 via
a fresh temp file; a node refuses every stream the main opens (A1 serves
nothing, so the stored ceiling isn't relied on yet; A2 must read it
strictly). Both sides leaving: a node left offline stays listed on the
main as Offline until removed there (the message says so); a node removed
while offline stops at its next 401 and `leave` then clears it.

## A2 as built (2026-11-13)

A node's servers now show in the main's server list and open the same
server pages (console, files, mods, configs, logs, backups, properties,
power) as the main's own. One shared view: the server page gets a
namespaced id and every request goes through the main, which relays it.
No mock: the node group reuses B2's group header and health line, so the
look was already agreed; only the maintenance line and the "Make it on"
choice are new, and both reuse existing pieces (the health line, the
segmented control).

### Ids and routes

- A node's server is `<node id>:<server id>` in the main's API and UI
  (`relay::SEP`). The main's guard sees the prefix (`relay::target`) and
  forwards the request on a tunnel stream (`relay::forward`); everything
  else stays local. Job ids in a node's answers get the same prefix, so
  `/jobs/{id}` finds its way back.
- Allowlist (`relay::allowed`, checked on the main and again on the node,
  where it counts): `servers/create`, `servers/port-check`, `GET servers/{id}`,
  `servers/{id}/<part>` for the parts in `SERVER_PARTS`, `fs/{id}/<part>` for
  `FS_PARTS`, `jobs/{id}` and its cancel/dismiss. Not relayed: copying a
  server or saving it as a template (they write outside its folder),
  off-site backup copies (the node's targets and keys), sharing a log to
  the web, `fs/link` (the main issues its own one-use links), `fs/fetch`
  (it would make the node download from its own network), the browse and
  infos routes. Empty, `.` and `..` path parts are refused before any
  handler runs; query values with `..`, a leading `/` or a drive
  (`query_paths_ok`) too. The node's handlers then check every path against
  the server folder as they do for their own people (`files::resolve`,
  which follows symlinks).
- Only a few headers cross (`PASS_IN` / `PASS_OUT`); bodies are capped at
  `MAX_BODY` (one upload chunk, 8 MB), JSON answers the main rewrites at
  32 MB. A relayed request reaches the node's handlers from 192.0.2.1 (a
  documentation address), so nothing treats it as the node's own PC or LAN.

### Identity and the ceiling

- The stream's OPEN head carries the person: user name, the role they
  have *on the node* and a `lead` flag. The main works the role out
  (`role_on_node`: owner and managers count as Admin) and caps it by the
  ceiling it stored; the node caps it again by the ceiling in its own
  `node.json`. An unknown or missing ceiling or role, or "manager", is
  refused outright.
- On the node the request runs as `Who::Main { main, user, role, lead }`.
  Look = Viewer, Operate = Operator, Manage = Admin. A Look ceiling refuses
  Start with 403 on both sides (tested).
- `lead` is set only when the person is owner-level on the main *and* both
  ceilings are Manage. It lets owner-only routes on the allowlist (removing
  a server) work for the main's owner and managers, as on their own panel;
  a main admin who isn't owner-level, or any ceiling under Manage, still
  gets 403 (tested). It never makes the main the node's owner (`is_owner`
  stays false), so nothing outside the allowlist opens up.
- History and the audit log name who did it: "mia via rig (operator)".

### Live list

- The node sends its snapshot (servers + host stats) and change frames on
  a feed stream; the main keeps the last snapshot per node and merges it
  into `/servers` and the live feed (`live.nodes` carries each node's
  header: state, last seen, RTT, version, ceiling, maintenance, host stats,
  RAM promised, server count). People limited to some servers see only
  those on the node too.
- A node that drops keeps its servers listed, dimmed, with "Node offline ·
  last known state"; their power actions are hidden.

### Maintenance

- `PUT /api/v1/nodes/{id}/maintenance {on}` (owner or manager). While on:
  the node's offline alert is silent, the main refuses Start, Restart and
  New server there with 409, running servers keep running. Stored in
  `nodes.json` with who and when.
- New alert rule "Node offline" in Settings › Alerts (on by default, after
  2 minutes, 2–120), shown when the panel has nodes.

### Web UI

- Node groups follow the home group: the B2 header (light, name, ceiling
  chip, ⋯ menu with Pause node / End maintenance / Node settings), then the
  health line "Node · CPU · RAM a of b GB · free disk · RAM promised · RTT",
  and under maintenance "Under maintenance · by X · nothing starts here ·
  End maintenance". The home group counts only its own servers and says
  "No servers on this panel itself." when they all live on nodes.
- New server: "Make it on" (This panel | each node with a Manage ceiling;
  disabled while offline or under maintenance); the memory slider and disk
  line show the chosen node's numbers.
- "Outside the panel folder" is no longer shown for a node's server (the
  main can't judge the node's folders).
- `NODES_SHOWN = true`: Settings › Nodes and the Add panel's node entry are
  visible again.

### Checked

- Tests (`relay_tests.rs`, a main and a node over an in-memory tunnel):
  the same requests answer the same through the main (file list, text,
  server, a write, one Content-Length after the job-id rewrite, audit);
  History names "mia via rig"; a Look ceiling refuses Start; an unknown
  ceiling refuses everything; `..`, encoded `..`, absolute, drive and
  symlink paths are refused by the node; unlisted routes (fetch, clone)
  never reach a handler; maintenance blocks Start and New server and the
  alert stays silent (`nodes` test); limited people see only their
  servers; removing needs Manage plus an owner-level person.
- Real run: two scratch panels on loopback (main 8861, node 8862, joined
  with a key, ceiling Manage). From the main's web UI: made a fake server
  on the node, started it, console lines arrived, stopped it, removed a
  second one. Found and fixed on the way: the node's Content-Length was
  passed on after the main grew the body (create showed an error), and
  remove was refused for the main's owner (now `lead`).
- **Node idle memory**, one stopped server attached, connected to its main,
  `fast` profile build, sampled every 5 s for 60 s: working set 28.4–28.5
  MB, private 13.9 MB.

### Left for later

- The "making" row for a server being created on a node shows under the
  home group until the job ends.
- Browse/install from the mod browser onto a node's server, server infos,
  copying, templates, `fs/fetch` and off-site backup copies.
- ~~"Share on mclo.gs" shows on a node's server but answers 403.~~ Hidden
  for node servers since the A2 review (one check, `canShareLog` in
  `ui/src/lib/nodes.js`, used by the console, log, crash and editor
  views). Relaying it instead stays open.
- MCP over nodes, node-scoped access for users ("everything on vm-a2"),
  bulk actions per node: A4.

### TPS and alerts of a node's servers (2026-11-13)

- The main's `GET /alerts` asks each connected node for its own
  `GET /alerts` over the tunnel (`relay::ask_alerts`, on the allowlist as
  GET only) as Look, the lowest role, whatever the person's. One answer per
  node is kept for everyone: asked again after 10 s, shown up to 60 s, and
  the main waits at most 1.5 s for nodes it is asking, so a slow or offline
  node never holds the page up (its last answer, or nothing, shows).
- The answer is merged like the server list (`alerts::merge_node`): server
  ids namespaced, only plain ids, only the person's servers, known kinds
  and no-TPS reasons, numbers checked, text one line of at most 500
  characters. A node's drive and memory alerts carry `node` and read
  "**oracle**: Drive C: is nearly full"; they show on the Overview and on
  that node's server pages, not on the main's own (`alertShows` in
  `ui/src/lib/nodes.js`). A node's "node" alerts are dropped.
- Dismissing a node's alert on the main hides it on the main only, until
  the node raises it again (a new `since`). Not sent to the node: there it
  would hide the alert for the node's own people too, and the main only
  reads from the node for alerts.

### A2 security review (2026-11-13)

Read against the A threat list for the relay: identity in the OPEN head,
the ceiling, `lead`, the allowlist, node-side path checks, a compromised
node attacking the main, id namespacing, maintenance and the audit log.
Each fixed High and Medium has a test that was written first and failed
on the old code (the ceiling one by reverting the fix to "read once").

| # | Rank | Finding (attack) | Status |
|---|---|---|---|
| 1 | High | **A compromised node could run script on the main's address.** The main passed the node's `Content-Type` and `Content-Security-Policy` through. A node answering a file download (`fs/{id}/file`, opened from the main's Files view) with `text/html` and its own CSP had its page run on the main's origin with the viewer's session: making API tokens, users, anything the person may do there. | Fixed (`relay::answer_head`): the type is one the panel itself serves (JSON, plain text, raster image, zip, raw bytes), anything else is raw bytes; every relayed answer gets the panel's file sandbox CSP and `nosniff`, whatever the node sent; each passed header once; a non-number length is dropped. Test `a_node_cannot_put_a_page_on_the_mains_address`. |
| 2 | Medium | **Server and job ids from a node weren't checked.** The web UI builds URLs from ids, several without encoding (`/servers/${id}/power`), so a node listing a server `x/../../../users/u1?` made the browser send the person's Stop click to a different route on the main. | Fixed: ids from a node must be plain names (`relay::plain_id`: letters, digits, `-`, `_`, at most 96); others are left out of the list, change lines and job answers. Test `ids_from_a_node_must_be_plain`. |
| 3 | Medium | **Maintenance could be bypassed:** pre-generating a world (`worlds/pregen`) starts a stopped server and wasn't on the "starts something" list. | Fixed: one check, `relay::starts_server` (Start, Restart, new server, pre-generate). Test `maintenance_blocks_starts`. |
| 4 | Medium | **Lowering the ceiling on the node didn't apply to the open connection.** It was read from `node.json` once per connection, so a node owner who lowered it (because they no longer trust the main) still gave the main Manage until the next reconnect, which may never come. | Fixed: read from `node.json` for every stream (`tunnel::node::ceiling_now`), strictly; another join or a missing file refuses. Tests `a_lowered_ceiling_applies_at_once`, `the_ceiling_is_read_from_node_json_each_time`. |
| 5 | Low | A node answering 401 or 407 could make a browser think it was signed out of the main (or of a listed other panel); 1xx and out-of-range statuses went through. | Fixed: those are 502. Same test as #1. |
| 6 | Low | A node could flood every server list with thousands of servers (one 8 MB line at a time). | Fixed: at most `nodes::MAX_NODE_SERVERS` (1000) per node. Test `a_node_lists_a_bounded_number_of_servers`. |
| 7 | Low | Path parts were checked raw, so `..%2F..%2Fx` reached a handler as `../../x` (every handler checked its names; no escape found). | Fixed: parts are checked as the node's router decodes them (no `/`, `\`, `:`, NUL, `.`, `..`). Unit test `the_allowlist`. |
| 8 | Low | The node doesn't know about maintenance: its own schedule (restarts) and automations still run there. | Listed: send maintenance to the node in A3 if wanted. |
| 9 | Low | `/jobs/{node}:{n}` isn't limited to the person's servers (job numbers are small): someone limited to one server can read another job's result. The same holds for local jobs. | Listed (both apps, not only nodes). |
| 10 | Low | A compromised node writes its own History and `changed` lines: it can blame any main user there, or make every browser refresh. The main's audit log, which records every relayed change with the real person, is the record to trust. | Listed (inherent). |
| 11 | Low | Under Manage the main can add a mod source from any URL (`mods/source/*`), so the node fetches from its own network, like the unrelayed `fs/fetch`. Manage already means code on the node (see "Trust model"). | Listed. |
| 12 | Low | The person's name crosses as plain text (control characters dropped, 60 characters), so a main user called "x via other (admin)" reads oddly in the node's log. | Listed. |

Checked and found sound: the OPEN head comes only from the one tunnel the
node dialled to its pinned main, and `relay::relayed()` is a task-local
that only `serve_request` sets (no header or cookie reaches it; the node's
handlers see no session headers); the role in the head is parsed strictly
(unknown, missing or "manager" refuses) and capped by both ceilings; a
main user below Admin stays there on the node (`security::refusal` runs on
both sides with the same role rules); `lead` is set by the main only for
owner-level people under its stored Manage, honoured by the node only under
its own Manage and an Admin role, and only makes `owner_level()` true:
`is_owner()` and `may_manage_user` stay false, and of the owner-only
handlers only `servers/{id}/remove` (and the owner view of the backup list)
is on the allowlist, so users, tokens, devices, settings, the nodes list,
TLS, update and the panel folder stay unreachable; the allowlist is
checked on the node against the path it routes (empty parts, double
slashes and `..` refused; query values with `..`, `/`, drives refused
after decoding; JSON path fields at any depth); file routes resolve on the
node through `files::resolve`, which follows symlinks (tested with `..`,
encoded `..`, absolute, drive and link paths); `Set-Cookie`, `Location`,
`Transfer-Encoding` and `Content-Encoding` never cross; answer heads are
capped at 16 KiB, JSON answers at 32 MB, request bodies at 8 MB, feed lines
at 8 MB; ids can't collide (local ids are slugs without `:`, node ids are
the main's own 12 hex characters, and a node's id with `:` is dropped);
maintenance is checked after the body is read, with the same JSON parser
the node's handler uses; the main's audit log records every relayed change
and early refusal with the real person, and the node's records each
relayed change and refusal as "person via main (role)".

### Server secrets security review (2026-11-13)

`/servers/infos` with node servers (and every properties or file read
relayed to a node) carried `rcon.password` and the management secret at
any ceiling. Now a node sends secrets only to a request at Admin (Manage),
infos never carry them, and the main drops them from an older node's
answer too. The rule, who sees what and the review are in
[server-secrets.md](server-secrets.md); the relay test is
`relay_tests::a_look_node_sends_no_secret`.

## A3 as built (2026-11-13)

Streamed files through the main, version skew, and updating nodes from the
main. Web UI done; Hopper desktop not yet (see "Left").

### Files streamed through the main

- **What streams.** The two routes whose body is raw file bytes:
  `PUT /api/v1/fs/{id}/uploads/{up}` (an upload chunk, web UI and desktop)
  and `POST /api/v1/servers/{id}/mods/upload` (a whole jar).
  `relay::streams_body` names them; both sides use it. Everything else
  still reads its body whole, capped at 32 MB (`relay::MAX_BODY`), so the
  main's maintenance check and the node's path check on JSON bodies are
  unchanged. Downloads (files, zips, backup downloads) already streamed
  since A2: an answer that isn't JSON goes through as it comes.
- **Main.** The browser's body goes to the node by a pump task, frame by
  frame, waiting for the node's credit: the main holds at most one window
  (512 KiB) plus its eight-frame send queue per stream, never the body, and
  writes no temp file. A body that breaks off (the browser went away) resets
  the stream (`RESET CANCEL`); the pump is aborted when the answer is
  dropped.
- **Node.** The handler reads the stream as its request body; credit goes
  back only as it reads, so a slow disk slows the browser instead of
  filling memory. Its own limits stay (32 MB per chunk, 512 MB per jar),
  and so do the guard (`security::secret_refusal`, the per-server rule) and
  the query path checks, which run before any body is read. A reset reaches
  the handler as a read error: `mods/upload` deletes its `.name.upload`
  file and keeps nothing.
- **Older nodes** keep working: they read the streamed body whole, as
  before, so a body over 32 MB is refused there with the old message.

### Version skew

- One tunnel version apart: talks, as in A0. Two apart: refused at HELLO;
  the main now records it (`Nodes::too_far`) with the version the node
  named. The node's row says **Update needed** ("vm (hopperd 0.40.0) is too
  old for this panel. Update it on the node itself: its own update, or
  install the new hopperd there.") or **Update this panel** when the node
  is the newer one. Its servers stay listed from the last snapshot;
  every request to them, reads too, is refused on the main with the same
  sentence (503), since nothing reaches the node. It can't be updated from
  here (there is no connection to ask over).
- Per node, the main keeps the capabilities both sides have and sends them
  with the node's head (`capabilities`), plus `outdated` (its release is
  behind the main's) and `can_update`.

### Updating nodes from the main

- **Settings › Nodes**: an outlined "Update to 0.63.0" button on each node
  that can be updated, "Update all (n)" over the list when more than one
  can, and a line over the list ("This panel runs hopperd 0.63.0. 2 nodes
  run an older one."). While it runs the row shows "Updating · Downloading
  hopperd 0.63.0" and the chip says Updating; a failure shows in red under
  the name with "Try again". The server list's node menu has the same
  Update; its health line says "update available", "Updating · ...",
  "Update needed · update it on the node itself".
- **Who.** The main's owner, a manager, or a device that may manage
  (`require_owner_who`), on a node joined at Manage. Look and Operate nodes
  never get the button, and the node refuses anyway.
- **How.** The main opens an `update` stream naming its own release. The
  node checks: its ceiling is Manage, the asker is a lead with the Admin
  role, the version is well formed and newer than its own (no rollback from
  the main), and its own `why_not()` (Docker, no download for this machine,
  a development build, a folder it can't write). Then it runs its own
  update (`autoupdate::install_for_main` → `install_version`): the release
  comes from GitHub with the same checks as always, never a file from the
  main, and servers keep running as in the panel's own update (Windows
  reattach, Linux re-exec keeps reattachable servers and waits for the
  others as its own update does). It streams its update state as JSON lines
  until it restarts, is ready, or fails; the main shows each step and
  counts the update done when the node is back on that version (10 minutes
  to come back, an hour in all). Both sides write it to their audit log.
- **Update all** goes one node at a time and stops at the first failure:
  "Stopped at vm: vm: the download failed. 1 more not updated."

### Tests

- `relay_tests::stream` (Part 1): a 40 MB jar (over the old 32 MB cap)
  and an upload chunk reach the node whole; a node that stops reading after
  1 MiB holds the main to that plus one window and its queue (measured:
  the main took at most read + 512 KiB + 8 x 64 KiB + 2 x 64 KiB from the
  browser); an upload cut off on its way leaves nothing in `mods`; a JSON
  body over 32 MB is still refused on the main (413).
- `relay_tests::skew` (Parts 2, 3): capability intersection; tunnel
  versions one apart talk and two apart are refused with both versions
  named; a node two behind shows `update_needed`, reads and writes to it get
  the reason (503) and Update is refused; Update follows the node's lines
  to done, and to done after a restart once it is back on the new version;
  Update all stops at the first failure and never asks the next node;
  no Update for a node without `nodes.update`, below Manage, or already
  current; the node refuses an update without a lead or below Manage, and
  with both runs its own checks (a test build refuses as a development
  build, so nothing is downloaded).
- Web UI: `tests/nodes.test.mjs` (the Update needed line, the update
  line, Try again, the list head).
- `cargo test -p hopperd`: 280 passed, 3 ignored. `pnpm test`: 32 passed.

### A3 security review (2026-11-13)

| # | Rank | Finding | Status |
|---|---|---|---|
| 1 | — | Streaming lifts the main's 32 MB cap on two routes. The node's own limits (32 MB per chunk, 512 MB per jar) and its guard still apply, and the main holds one window per stream (64 streams: 32 MiB at most per node), so a big or endless upload can't grow the main's memory. | Sound; tested (slow node, cancelled upload). |
| 2 | — | Streamed bodies skip the main's body checks. Those checks are for JSON (paths in a body, maintenance starts); the two routes carry file bytes whose paths are in the URL, checked on both sides before any byte moves. | Sound. |
| 3 | Low | "Update" lets the main make a node run a new program. Only at Manage (which already lets the main run code there), only for a lead, only to a newer GitHub release the node fetches and checks itself; no downgrade, no file from the main. A compromised main can at most update a node to a real newer release. | By design; listed. |
| 4 | — | Text from the node (update lines, its version in HELLO) is shown on the main: control characters dropped, lengths capped (`clean_text` 500, version 40), lines at most 8 MB; Svelte escapes it. | Sound. |
| 5 | Low | `HOPPERD_TEST_VERSION` / `HOPPERD_TEST_TUNNEL` change what a panel says it runs. Only the person who starts the process sets them; a node that lies about its version only changes what its own row shows. | Listed. |

### Left (not done in A3)

1. **Hopper desktop** (Part 4): a + on each node group (the create dialog
   sending `?node=<id>` like the web UI; only a fresh server, like the web
   UI), the node's own state for the group light, Update needed shown and
   the node read-only, from a new main route with the node heads for a
   signed-in device (`GET /api/v1/nodes/live`, the `live_view` heads).
   Desktop uploads already use the two streamed routes (8 MB chunks and
   `mods/upload`), so they stream through a main with no desktop change;
   not yet checked by hand with a big file.
2. **Measured on real panels**: a 2 GB upload to a node through a scratch
   main, the main's working set sampled (target: under 64 MB growth, no
   temp file). The tests prove the bound per stream; the live number is
   not taken yet.
3. Screenshots of the new states (both apps, dark and light, 375 px web)
   and the change card.

## Open questions for the maintainer

1. **Remote-session limits (B).** *Answered (maintainer, 2026-11-11): a
   per-panel owner setting, "Let these pages manage who can get in", off by
   default.* Off: a request from a listed other page (an `Origin` in
   `web_origins` that isn't the panel's own) is refused for changing users,
   devices and pairing, API tokens, passwords and two-factor, and the
   certificate. On: those go through to the route's normal checks. The
   allowed-pages list and this switch change only on the panel's own page
   (its cookie session, `Origin` equal to `Host`), whatever the switch says.
   One check, `web_access::access_refusal`, called from `security::guard`.
   Built in B0.
2. **Who may add a node (A).** True owner only, or managers too? I recommend
   owner only.
3. **Panel list roaming (B).** Store the list of added panels (URLs and names,
   no tokens) on the home panel per account so every browser shows them, or
   keep it in the browser only? I recommend on the home panel.
4. **Ticket carrier (B/A).** Query string versus `Sec-WebSocket-Protocol`:
   prefer the subprotocol if the spike shows axum handles it cleanly.
   **Settled in B1: the subprotocol; no query-string tickets.**
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
    *Deferred after research (see "Adjusted after research").*
11. **Join command (A).** Offer the generated one-line join command with a
    single-use 30-minute join key next to the code, or code only? I
    recommend both: the code for people at the VM's console, the command for
    scripts and fresh VMs. **Decided 2026-11-13: both.**
12. **Moved server's old copy (A).** Keep the source stopped and marked until
    the user deletes it (my pick), or delete it automatically after a
    successful move? **Decided 2026-11-13: keep it stopped and marked until the
    user deletes it.**
