# Multi-panel research: what server admins want from other panels

Date: 2026-11-13. Input for `design/multi-panel.md` (section "Adjusted after
research").

**How this was gathered.** Official docs, GitHub/GitLab issues sorted by
reactions, vendor forums, HN and 2025-2026 comparison posts. Reddit
(r/admincraft, r/selfhosted, r/homelab) could not be fetched (blocked), so
community signal here is GitHub/GitLab reactions, not upvotes. Do a manual
Reddit pass before quoting "the community says". Counts are reactions on the
original post at fetch time. Items marked *secondary* come from third-party
blogs or doc mirrors and should be checked before a design depends on them.

Signal: **S** strong (primary doc, or 50+ reactions), **M** medium (10-50
reactions, or an official doc claim), **W** weak (a single report or a blog).

## Per panel

### Pterodactyl and Pelican (panel + Wings)

- **Shape.** One panel, one Wings daemon per node. The panel calls Wings
  *inbound* (HTTP API on 8080, SFTP on 2022), and the **browser opens the
  console WebSocket directly to Wings** with a panel-issued JWT. A node
  belongs to exactly one panel. Linux and Docker only; Windows is
  explicitly unsupported. https://pelican.dev/docs/wings/install
- **Adding a node**: create it in the panel (FQDN, SSL, ports, memory and
  disk totals, overallocate %), copy `config.yml` or run an auto-deploy
  command, install Docker, run Wings under systemd as root. "If the panel
  uses SSL, Wings must too." Many steps before the first green heart. (M)
- **Loved**: Docker isolation, per-server subusers with granular
  permissions, the client API, a large ecosystem (eggs, themes, Pelican's
  plugin hub).
- **Hated / most-reacted**:
  - Fine-grained admin permissions, including per-node scope: **#279, ~118
    reactions**, closed unbuilt in a 2026 cleanup. Pelican shipped roles
    scoped to nodes (pelican #613). https://github.com/pterodactyl/panel/issues/279 (S)
  - Separate SFTP address because a node behind Cloudflare can't carry
    SFTP: **#2483, ~115 reactions**, open since 2020; also breaks transfers.
    https://github.com/pterodactyl/panel/issues/2483 (S)
  - SSO by proxy header: #4026, 71; people left for Pelican over it. (S)
  - Move a server to another node: **#18, ~69**, shipped "very beta". Known
    failures: target looks full because `-1` overallocate is broken
    (#4940), stale data left after a failed transfer (#5555), symlinks lost
    (#5429). https://github.com/pterodactyl/panel/issues/18 (S)
  - Audit log: #1219, 55; shipped incomplete because power and console
    actions go browser -> Wings and never pass the panel. (S)
  - Backups: #114, 53; S3 backups failing behind Cloudflare's 100 MB body
    limit (#3265). (S)
  - One dead node slows the whole panel: #3936 (open since 2022, workaround
    "put the node in maintenance mode"). (M)
  - Bulk start/stop "all servers / all on node X": #278, 26, never built. (M)
  - Every server needs an IP:port allocation, even bots: #2754, 32. (M)
  - TLS: expired Wings certs (#3799), mixed content when the panel moved to
    HTTPS after the node was made, `ERR_CERT_COMMON_NAME_INVALID`. (M)
  - ARM: Wings segfaulted on Oracle Ampere A1 with Oracle Linux (#3638). (W)
  - Mobile console unreadable / can't select text (#4561, #5084). (W)
  - Heavy: PHP + DB + Redis + web server + Docker + Wings; 1.5-2 GB idle
    (*secondary*, mineguard.pro). Pelican's own guide calls it overkill under
    5 servers. (W)
- **Pelican's changes for nodes**: node-scoped roles, node stats and
  diagnostics with log upload, a "reachable from your browser" column, SFTP
  alias, per-node backup hosts, CPU limit per node, auto-fetched node IPs,
  `-1` overallocate fixed, transfer API, webhooks, passkeys. Still beta;
  licence moved MIT -> AGPLv3. Still inbound + Docker + a cert per node.
  https://pelican.dev/docs/comparison

### Calagopus (Rust rewrite of Pterodactyl, newer)

- Vendor feature list (claims): live migration between nodes with optional
  backup transfer, bulk config push to many nodes, live per-node charts,
  remote Wings log tail, Wings self-upgrade with SHA-256 check, per-role 2FA,
  admin audit log plus per-server/per-account activity with retention,
  scoped expiring API keys with IP allowlist. Open bug: restoring a shared
  backup on another node fails (#273). https://github.com/calagopus/panel (W/M)

### AMP (CubeCoders)

- **Shape**: ADS in Standalone, Controller, Target or Hybrid mode.
  Controller-target traffic is **two-way; both must reach each other; NAT
  not supported**; HTTPS required for the controller's auth URL. Native on
  Windows and Linux, Docker optional, ARM64 Linux only. Multi-server needs a
  Professional licence or higher.
  https://discourse.cubecoders.com/t/ads-deployment-modes/2245 (S, official)
- **Loved**: one-time price for many servers, single-command install, runs
  natively on Windows, fine-grained per-instance lock-outs, and **instances
  keep running and keep their schedules when the controller is down**.
  https://discourse.cubecoders.com/t/why-did-you-pick-amp/11866 (W)
- **Hated**: targets disconnect after major updates (several forum threads,
  mostly unanswered); pairing fails behind a spinner with no reason; up to 9
  ports to open; `ampinstmgr repairauth` needed to recover; Windows update
  management. https://discourse.cubecoders.com/t/target-disconnected-after-amp-update/40226 (M by repetition)

### MCSManager (panel + daemon)

- **Shape**: panel connects to each daemon (default port 24444) with a
  daemon key; **the browser connects directly to every daemon** for console
  and files. Each daemon must be publicly reachable; an HTTPS panel forces
  `wss://` and a reverse proxy per daemon. Node.js, Windows zip or Linux
  script. https://docs.mcsmanager.com/ops/mcsm_network.html (S, official)
- **Loved**: light, no Docker, many daemons from one panel, easy Windows
  install (*secondary*).
- **Hated**: the browser-direct design: remote console/files fail over
  HTTPS (#1570), nginx-proxy-manager can't express it (#1129, 27 comments),
  two daemons behind one SSL setup (#1145). Cloudflare Tunnel users ask for
  "the panel proxies the WebSocket" (#2340): the clearest user ask for a
  relay. Docs advise ordinary users *not* to set up HTTPS. A daemon-only box
  left with its web setup page open can be taken over (docs warning). (M)

### Crafty Controller 4

- Single machine. Multi-node requested (GitLab #131, #694) with few votes.
  A 2026 fork proposal (#763) adds a "hub" over Crafty's API v2: pin the
  node's cert fingerprint at add time, a dedicated non-admin API user on the
  node, per-node levels **monitor / control / manage**, poll every 30 s with
  a time budget, treat node replies as untrusted. Same idea as our ceiling,
  but chosen by the hub, not enforced by the node.
  https://gitlab.com/crafty-controller/crafty-4/-/work_items/763 (W)
- Top multi-user ask: LDAP (#39, 12). (W)

### PufferPanel

- Nodes are full PufferPanel installs; paste generated config into
  `config.json`; master calls node REST. (M, official)
- **Hated**: one offline node breaks the server list for *all* servers
  ("Unknown error", #1246); ghost servers block node removal (#1236);
  renaming remote servers breaks the link (#1375); maintainers answered
  per-node stats with "use Netdata" (#943). (M)

### Multicraft

- Panel plus daemons that all reach one shared MySQL; multi-daemon needs a
  higher licence. Admins left mostly over slowed development and price
  (*secondary*). The shared-DB design means a lot of inbound exposure; don't
  copy it. https://multicraft.org/site/page/features (W)

### Lodestone (Rust core, dashboard connects to many cores)

- Closest to our plan B. Its 2026 PRs fix exactly B's problems: HTTPS by
  default with its own cert, plain HTTP only from loopback (#460);
  per-host fingerprint pinning in its *desktop* client (#493); a `403
  LocalOnly` reply **that carries CORS headers so the page can show a plain
  message** (#696); "Lodestone Connect" hole-punched tunnel with the
  fingerprint pinned in the invite link (#629). Users complain the client
  forgets connected cores (#181).
  https://github.com/Lodestone-Team/lodestone (W/M)

### Non-Minecraft agent models

- **Komodo** (core + periphery). v2 added agent-dials-core and made it the
  default: "usually both easier and more secure… not much downside" (release
  PR, 115 reactions). Replaced one shared passkey (top security complaint,
  #123) with per-server keys, pinning, **daily automatic rotation**, and
  **onboarding keys** that only create a node and can't be used as its
  identity. Core and periphery had to be upgraded together for v2.
  https://github.com/moghtech/komodo/pull/889 (S)
- **Portainer**. Edge agent dials out and polls; a second "tunnel" port
  (8000) caused repeated reverse-proxy trouble (#4683, #6251). A pinned
  fingerprint format change silently broke agents after an upgrade (#8531).
  Docker 29 broke the manager-to-host link: #12925, **327 reactions, 439
  comments**. Anger at SSO/2FA behind a paywall (#3582, 36) and a missing
  mobile UI (#3059, 49: "sometimes I just want to restart a container").
  https://docs.portainer.io/admin/environments/add/docker/edge (S)
- **Beszel** (hub + agent). "Add system" in the hub generates the exact
  install command with key, token and URL filled in; the agent dials out
  over WebSocket; `--auto-update` daily; hub key pinned in the agent.
  Complaints: agent stuck after one failed DNS lookup (#1341), users want to
  see which transport each agent uses (#1017), container missing root CAs
  (#2291), version skew after upgrades (#985, 30).
  https://www.beszel.dev/guide/agent-installation (S, official)
- **Netdata**. Agent dials out to Cloud; claim token in a generated
  one-liner, or prove local control by pasting a UUID read from the node's
  disk (same idea as our 6-digit code). Wanted: alert when a child stops
  streaming (#18436). (S, official)
- **Coolify / Dokploy**. Main SSHes *in* to each server and installs
  Docker. Most setup failures; an Oracle A1.Flex install failed mid-script
  (Coolify #7116); false "server unreachable" emails every hour that
  **switched automations off** (#4407). Dokploy's top asks once people have
  two servers: **move an app to another node (#812, 89; #2260, 34)**,
  backups (#709, 59), off-site backup targets (#416, 30). (S)
- **Tailscale**. Two ways to join: a device code approved in the browser, or
  a pre-made one-time / ephemeral auth key for scripts. (S, official)

## Ranked: what admins and power users want

Ranked by evidence across panels, then by fit with Hopper.

1. **Nodes that work without inbound ports, per-node certs or a VPN.** The
   most repeated pain in every inbound or browser-direct design: Pterodactyl
   #2483 (~115), MCSManager #1570/#1129/#1145/#2340, AMP "NAT not supported",
   Portainer #4683/#6251, Komodo #281 and its v2 switch. Komodo, Beszel,
   Netdata and Portainer Edge all moved to agent-dials-out.
2. **Move a server to another node**, safely: Dokploy #812 (89) + #2260
   (34), Pterodactyl #18 (~69) and its failures (#4940, #5555, #5429).
   Wanted details: disk-space precheck, port conflicts, stop first, keep or
   delete the old copy, bring backups along, clean up after a failure.
3. **Permissions scoped per node, and read-only staff**: Pterodactyl #279
   (~118), Pelican #613, Crafty #763 levels, AMP lock-outs.
4. **A complete audit trail**, including power and console actions:
   Pterodactyl #1219 (55) shipped with holes because of browser-direct
   traffic.
5. **Backups with off-site targets, across nodes**: Pterodactyl #114 (53),
   Dokploy #709 (59) and #416 (30), PufferPanel #1197 (19), Calagopus #273.
6. **One dead node must not hurt anything else**, and an offline node must
   be shown honestly without alert storms: PufferPanel #1246, Pterodactyl
   #3936, Coolify #4407, Beszel #1341, Netdata #18436. AMP's "instances keep
   running when the controller is down" is the model.
7. **Easy install and pairing with a reason when it fails**: one-liner with
   the token filled in (Beszel, Netdata, Tailscale); AMP's silent pairing
   spinner and post-update disconnects are the anti-pattern.
8. **Upgrades that don't break nodes**, with version skew shown per node and
   updates from the main: Portainer #12925 (327), Komodo v2, Beszel #985,
   AMP update threads, Calagopus self-upgrade.
9. **Bulk actions** across servers and "all on node X": Pterodactyl #278
   (26), MCSManager groups (#444), Calagopus bulk config push.
10. **Per-node health at a glance**: CPU, RAM, disk, version, last seen,
    transport. PufferPanel #943 (punted), Pelican stats + reachability,
    Calagopus charts, Beszel #1017.
11. **Native, light, cross-platform** (Windows, ARM64, small VMs): Docker
    and Linux-only are the most named reasons against Pterodactyl; AMP is
    praised for Windows; Oracle A1 failures (Pterodactyl #3638, Coolify
    #7116).
12. **SSO / OIDC / LDAP / proxy header** (#4026 71, MCSManager #1160 18,
    PufferPanel #1056 16, Crafty #39 12). Separate topic from multi-panel.
13. **Mobile-friendly UI** (Portainer #3059 49; Pterodactyl console on
    phones).
14. **Everything in the API**, including node settings (Pterodactyl #1518,
    #3624, #4288), and scoped, expiring keys (Calagopus, MCSManager #1829).
15. **No paywall on security or multi-node** (Portainer #3582, AMP editions).

## Ranked: what they hate

1. Cert/TLS/port-forward/mixed-content work per node (see want 1).
2. A dead or slow node breaking or slowing the whole panel.
3. Heavy stacks and Docker as a hard requirement; no Windows.
4. Upgrades that silently break the link, with no diagnostics.
5. Long multi-step node installs (config files, FQDN, DNS, firewall).
6. Transfers that half-fail and leave junk, or refuse because of bad
   capacity maths.
7. Features closed as "done" without being built (Pterodactyl's 2026
   cleanup) and paywalled basics.
8. False offline alerts that also switch automation off.

## What this means for Hopper (short)

- Plan A (node dials out, pairing code, node-side ceiling, autonomous node)
  is where the field went; few game panels have it. It is our strongest
  multi-node story, especially for Oracle A1 and CGNAT boxes.
- Plan B has almost no precedent outside Lodestone and MCSManager, and both
  show its cost: browser-direct needs every panel reachable with a cert the
  browser trusts. Keep it for LAN, Tailscale and panels that already have a
  real certificate, and point remote cases to A.
- Our existing pieces cover several top wants already: History/undo (audit
  plus undo), backup copies to another panel (`offsite.rs` target kind
  `hopper`), clone, alerts and webhooks, per-server user lists, scoped API
  tokens, MCP. The plan should build on them rather than add new copies.
