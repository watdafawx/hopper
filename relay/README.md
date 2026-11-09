# Hopper mailbox

Pairs Hopper desktops that can't reach each other directly and carries
**share codes**, never jars. It runs on the Oracle VM and is published by a
Cloudflare Tunnel as `https://sync.kyuubi.dev`.

## What it stores

- a random id and bearer token per PC;
- the friend list and one-time invite codes (15 minutes);
- the latest recipe events per PC (at most 200; no files).

State is `/var/lib/hopper-mailbox/state.json`, owned by the
`hopper-mailbox` user. Deleting it resets every account and pairing:

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
| `POST /v1/invite` | → `{code}` (one use, 15 min) |
| `POST /v1/join` | `{code}` → `{friend}` |
| `GET /v1/friends` | `{friends: [{id,name,online}]}` |
| `POST /v1/remove` | `{id}` |
| `POST /v1/recipes` | `{to, inst, name, code, left_out}` |
| `GET /v1/recipes?after=N&wait=60` | `{seq, events}`; long-polls |
| `GET /v1/health` | `{ok:true}` |

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
- Rate limits are basic (body size and event caps); add per-token limits
  before opening this to the public.
