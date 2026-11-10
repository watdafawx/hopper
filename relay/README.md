# Hopper mailbox

Pairs Hopper desktops that can't reach each other directly and carries
**share codes**, never jars. It runs on the Oracle VM and is published by a
Cloudflare Tunnel as `https://sync.kyuubi.dev`.

## What it stores

- a random id per PC and the **sha256 of its bearer token** (the token
  itself is never kept);
- the friend list and one-time invite codes (15 minutes);
- groups: name, owner, members (each with "said yes" and "keep in step"),
  the latest share code of every pack shared with the group, and which of
  those packs a member follows;
- the latest events per PC (at most 1000 and 4 MB, kept 14 days; share codes
  and revokes 180 days; no files).

State is `/var/lib/hopper-mailbox/state.json`, owned by the
`hopper-mailbox` user, mode 600. It is written by one background thread
about every 1.5 s after a change, and once more when the service stops
(SIGTERM). A crash can lose the last second or two, except for a new
sign-up, which is saved before it is answered. At start:

- a file that is there but not valid (bad JSON or encoding, wrong shape) is
  renamed to `state.json.corrupt-<time>` and the service exits with status 1;
- a file that can't be read at all (rights, disk) is **left exactly where it
  is** and the service exits with status 1.

Either way it never starts over on top of real data, and the unit has
`RestartPreventExitStatus=1`, so systemd does not keep retrying: look at
`journalctl -u hopper-mailbox`, fix the cause (or restore a copy), then
`systemctl start hopper-mailbox`. Deleting the file resets every account and
pairing (desktops notice, sign up again and tell their player to invite
their friends again):

```bash
sudo rm /var/lib/hopper-mailbox/state.json
sudo systemctl restart hopper-mailbox
```

## API

All routes except `register` and `health` need
`Authorization: Bearer <token>`.

| Route | What |
|---|---|
| `POST /v1/register` | `{name}` → `{id, token}` |
| `POST /v1/profile` | `{name, mc_id}` |
| `POST /v1/invite` | `{inst?}` → `{code, expires}` (seconds) (one use, 15 min) |
| `POST /v1/join` | `{code}` → `{friend, inst}`; 409 `friends_full` / `their_friends_full` |
| `GET /v1/friends` | `{friends: [{id,name,mc_id,online,last_seen}], max}` |
| `POST /v1/remove` | `{id}`; also leaves each other's groups |
| `POST /v1/recipes` | `{to: [ids], groups: [ids], inst, key, name, code, left_out}` → `{seq, sent, skipped}`; one event per person |
| `POST /v1/revoke` | `{inst, to: [ids], groups: [ids], reason}` (`unshared` / `deleted`) |
| `GET /v1/recipes?after=N&wait=60&v=2` | `{seq, events}`; long-polls. Without `v=2` only `recipe` events |
| `GET /v1/groups` | groups you own, belong to or are invited to |
| `POST /v1/groups` | `{name, members}` → `{group}` (members must be your friends) |
| `POST /v1/groups/<id>` | owner: `{name?, add?, remove?}` |
| `POST /v1/groups/<id>/delete` | owner |
| `POST /v1/groups/<id>/accept` | invited member says yes; gets the group's packs |
| `POST /v1/groups/<id>/leave` | member (not the owner) |
| `POST /v1/groups/<id>/in-step` | `{on}`: follow what other members follow |
| `POST /v1/groups/<id>/follow` | `{from, inst}`: tell in-step members you followed a group pack |
| `GET /v1/health` | `{ok:true, v:2}` |

A group is invisible to anyone who isn't in it (404). Until you say yes to
an invitation you see its name and its owner only; once in, other members
who aren't your friends show a name and "joined", with no presence and no
Minecraft id. The protocol, event kinds and limits are in
`design/friends-sync.md`.

## Limits

| What | Limit | Answer |
|---|---|---|
| Request body | 64 KB; no `Transfer-Encoding`, one numeric `Content-Length` | `413` / `400` `bad_framing`, then the connection closes |
| Accounts | 20000, and 32 MB of state in all; new sign-ups only | `503` `busy` |
| Idle accounts | no friends, no groups and nothing for 30 days: forgotten (checked hourly) | |
| Sign-ups | 60 per hour per client address (`CF-Connecting-IP`, else the peer; IPv6 by /64). The place is taken before the body is read | `429` `rate_limited` |
| Wrong invite codes | 10 per 15 min per account, 30 per 15 min per address | `429` `rate_limited` |
| Open invites | 20 per account | `429` `invites_full` |
| Packs shared into groups | 1 MB per account, 64 KB while no one else has joined the group; counted against whoever shares | `409` `storage_full` (`503` `busy` when the state is full) |
| Share code | 20000 bytes; larger is refused, not cut | `413` `too_large` |
| Waiting events | 1000 and 4 MB per PC, 150 per sender as a backstop; a new version of a pack replaces its old event; 14 days (share codes and revokes 180); `recipe` and `revoke` are dropped last | oldest dropped |
| Packs a group already had | not queued: read from the group when the new member asks | |
| Long-polls | 3 per account (a 4th ends the oldest, which gets `409` `poll_superseded`); `wait` up to 90 s | |
| Connections | 2000 at once; a whole request must arrive in 30 s, and a socket stalled for 30 s is closed | `503` `busy` |
| Headers | names must be RFC tokens, no folded lines | `400` `bad_framing` |
| Shutting down | new requests while the last save is written | `503` `busy` |

`429` and `503` answers carry `Retry-After`. `left_out` is cut at 100
entries of 400 bytes, names at 64 characters. Unexpected errors are a JSON
`500` with `code: internal` and one log line; a dropped connection is one log
line too.
Set `HOPPER_MAILBOX_REGISTER_LIMIT`, `HOPPER_MAILBOX_MAX_CONNECTIONS`,
`HOPPER_MAILBOX_MAX_USERS` or `HOPPER_MAILBOX_MAX_STATE_BYTES` to change
those (tests and local runs).

**Put a Cloudflare rate-limiting rule on `POST /v1/register`** (Security →
WAF → Rate limiting rules; for example 10 requests per minute per IP, action
Block for 10 minutes). The relay counts sign-ups per address itself, but
Cloudflare can stop a flood before it reaches the tunnel. Nothing here sets
that rule up; it is a dashboard setting.

## Tests

`python -B -m unittest relay/test_hopper_mailbox.py` (from the repo root)
runs the mailbox on a loopback port with a temporary state file.

## Deploy

The service is a single standard-library Python file. On the VM:

```bash
sudo useradd --system --home /var/lib/hopper-mailbox --shell /usr/sbin/nologin hopper-mailbox
sudo mkdir -p /opt/hopper-mailbox /var/lib/hopper-mailbox
sudo chown hopper-mailbox:hopper-mailbox /var/lib/hopper-mailbox
sudo install -m 644 hopper_mailbox.py /opt/hopper-mailbox/hopper_mailbox.py
sudo install -m 644 hopper-mailbox.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now hopper-mailbox
```

### Updating from a version that stored plain tokens

The first start of this version replaces every stored token with its sha256
and saves that on the next flush. PCs keep working: they still send the same
token and it hashes to the same value. Do this in order:

```bash
sudo systemctl stop hopper-mailbox
sudo cp -a /var/lib/hopper-mailbox/state.json /var/lib/hopper-mailbox/state.json.pre-hash   # keep this
sudo install -m 644 hopper_mailbox.py /opt/hopper-mailbox/hopper_mailbox.py
sudo install -m 644 hopper-mailbox.service /etc/systemd/system/   # UMask, StateDirectory, hardening lines
sudo systemctl daemon-reload
sudo systemctl start hopper-mailbox
sudo chmod 600 /var/lib/hopper-mailbox/state.json                 # the old file's mode; the next save is 600 anyway
journalctl -u hopper-mailbox -n 20     # "hashed N stored tokens"
curl -s http://127.0.0.1:8787/v1/health
```

**Rolling back after the first start of this version needs the saved copy.**
The old relay compares plain tokens, so it can't read the hashed state: every
PC would get "register first" and have to sign up again, losing its friends
(and the new-version file holds no token to recover). To go back, stop the
service, put `state.json.pre-hash` back as `state.json`, install the old
`hopper_mailbox.py` and unit, and start it. Whatever happened since the copy
(new accounts, pairings) is lost, so decide quickly or accept that. Delete
`state.json.pre-hash` once you are sure you won't go back; it holds every
token in the clear.

Updating later (same version family) is the `install` line for
`hopper_mailbox.py` plus `sudo systemctl restart hopper-mailbox`. If the unit
changed, install it and `sudo systemctl daemon-reload` first. A state file
from before groups loads as is (it gets an empty `groups`).

The unit (`hopper-mailbox.service`) also sets `StateDirectory=hopper-mailbox`
(mode 0700), `MemoryMax=512M`, `LimitNOFILE=8192`, address families limited
to IP and Unix, `PrivateDevices`, `ProtectKernelTunables` and
`SystemCallFilter=@system-service`. It was checked for syntax by eye only;
after installing, run `systemd-analyze verify /etc/systemd/system/hopper-mailbox.service`
and `systemd-analyze security hopper-mailbox`, and if the service won't
start, the journal says which line (the filter is the likeliest; remove that
one line and report it).

**Desktops after a data reset.** A Hopper that finds its saved token
unknown to the service (`401`) signs up again once, keeps its local
settings and tells its player: "Your friends list was reset on the sync
server. Invite your friends again."

`hopper-mailbox.service` binds `127.0.0.1:8787`; nothing listens publicly.
The tunnel is a separate unit, `hopper-tunnel.service`, running
`cloudflared tunnel run --token-file /etc/hopper-mailbox/tunnel.token`.
Its ingress (`sync.kyuubi.dev → http://127.0.0.1:8787`) is managed in the
Cloudflare dashboard or by the API. The token file is mode 600 and the
directory 700, both owned by `ubuntu` (the tunnel's user).

## Notes

- No CurseForge keys, player tokens or jars go here.
- A token is a bearer credential; use TLS only (the tunnel provides it).
- There is no admin UI. Reset the state file to clear test accounts.
- The service believes `CF-Connecting-IP` only from a connection that
  comes from this machine (the tunnel). Keep `HOPPER_MAILBOX_HOST` on
  loopback; if it ever listens publicly the header is ignored and everyone
  counts as their peer address.
- A save that fails (disk full, bad rights) is logged as `state save
  failed` and tried again every second or so; requests still answer from
  memory meanwhile. Watch the journal for it.
- A request that fails with a non-2xx status changed nothing (a 500 from a
  bug may have changed part of it in memory).
