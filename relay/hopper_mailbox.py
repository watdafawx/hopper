#!/usr/bin/env python3
"""Hopper mailbox: pair PCs and carry recipes, never jars.

The desktop app registers once, gets an id and a token, pairs with a friend
using a one-time code, and then publishes or receives share codes. A share
code is a few KB; the mods themselves download from CurseForge, Modrinth,
GitHub and the other sources on each PC.

Only the loopback interface is used here; Cloudflare Tunnel publishes it as
sync.kyuubi.dev. There is no file storage, no jar upload and no web UI.
"""

import json
import os
import secrets
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs

STATE = os.environ.get("HOPPER_MAILBOX_STATE", "/var/lib/hopper-mailbox/state.json")
HOST = os.environ.get("HOPPER_MAILBOX_HOST", "127.0.0.1")
PORT = int(os.environ.get("HOPPER_MAILBOX_PORT", "8787"))
MAX_BODY = 64 * 1024
INVITE_TTL = 15 * 60
ONLINE_SECS = 90
MAX_FRIENDS = 100
MAX_EVENTS = 200

lock = threading.RLock()
changed = threading.Condition(lock)
state = {
    "users": {},      # id -> {name, token, created, last_seen}
    "friends": {},    # id -> [id]
    "invites": {},    # code -> {owner, expires}
    "events": {},     # id -> [{seq, from, inst, code, name, left_out, at}]
    "seq": 0,
}


def load():
    global state
    try:
        with open(STATE, "r", encoding="utf-8") as f:
            s = json.load(f)
        for k in state:
            if k in s:
                state[k] = s[k]
    except FileNotFoundError:
        pass
    except Exception as e:
        print(f"state load failed: {e}", file=sys.stderr)


def save():
    os.makedirs(os.path.dirname(STATE), exist_ok=True)
    tmp = STATE + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(state, f, separators=(",", ":"))
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, STATE)


def now():
    return int(time.time())


def user_by_token(token):
    if not token or len(token) < 20:
        return None
    for uid, u in state["users"].items():
        if secrets.compare_digest(u["token"], token):
            return uid, u
    return None


def public(uid):
    u = state["users"].get(uid, {})
    return {
        "id": uid,
        "name": u.get("name", ""),
        "online": now() - u.get("last_seen", 0) <= ONLINE_SECS,
    }


def friend_ids(uid):
    return list(state["friends"].get(uid, []))


def publish(to, frm, body):
    state["seq"] += 1
    ev = {
        "seq": state["seq"],
        "from": frm,
        "inst": str(body.get("inst", ""))[:128],
        "code": str(body.get("code", ""))[:20000],
        "name": str(body.get("name", ""))[:128],
        "left_out": [str(x)[:200] for x in (body.get("left_out") or [])][:100],
        "at": now(),
    }
    q = state["events"].setdefault(to, [])
    q.append(ev)
    if len(q) > MAX_EVENTS:
        del q[: len(q) - MAX_EVENTS]
    return ev


class Handler(BaseHTTPRequestHandler):
    server_version = "HopperMailbox/1"
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        sys.stderr.write("%s %s\n" % (self.address_string(), fmt % args))

    def body(self):
        try:
            n = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            return None
        if n < 0 or n > MAX_BODY:
            return None
        try:
            return json.loads(self.rfile.read(n) or b"{}")
        except Exception:
            return None

    def send(self, status, obj):
        data = json.dumps(obj, separators=(",", ":")).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def auth(self):
        h = self.headers.get("Authorization", "")
        token = h[7:].strip() if h.startswith("Bearer ") else ""
        with changed:
            found = user_by_token(token)
            if not found:
                return None
            uid, u = found
            u["last_seen"] = now()
            return uid

    def do_GET(self):
        u = urlparse(self.path)
        q = parse_qs(u.query)
        if u.path == "/v1/health":
            return self.send(200, {"ok": True, "service": "hopper-mailbox"})
        uid = self.auth()
        if not uid:
            return self.send(401, {"error": "register or pair first"})
        if u.path == "/v1/friends":
            with changed:
                return self.send(200, {"friends": [public(x) for x in friend_ids(uid) if x in state["users"]]})
        if u.path == "/v1/recipes":
            try:
                after = int((q.get("after") or ["0"])[0])
                wait = max(0, min(120, int((q.get("wait") or ["0"])[0])))
            except ValueError:
                return self.send(400, {"error": "bad query"})
            deadline = time.time() + wait
            with changed:
                while True:
                    evs = [e for e in state["events"].get(uid, []) if e["seq"] > after]
                    if evs or time.time() >= deadline:
                        out = []
                        for e in evs:
                            x = dict(e)
                            x["from_name"] = state["users"].get(e["from"], {}).get("name", "")
                            out.append(x)
                        return self.send(200, {"seq": state["seq"], "events": out})
                    changed.wait(timeout=max(0.1, deadline - time.time()))
        return self.send(404, {"error": "not found"})

    def do_POST(self):
        u = urlparse(self.path)
        body = self.body()
        if body is None:
            return self.send(400, {"error": "bad body"})
        if u.path == "/v1/register":
            name = str(body.get("name", "")).strip()[:64] or "This PC"
            with changed:
                uid = secrets.token_hex(16)
                token = secrets.token_urlsafe(32)
                state["users"][uid] = {
                    "name": name,
                    "token": token,
                    "created": now(),
                    "last_seen": now(),
                }
                state["friends"].setdefault(uid, [])
                state["events"].setdefault(uid, [])
                save()
            return self.send(200, {"id": uid, "token": token, "name": name})
        uid = self.auth()
        if not uid:
            return self.send(401, {"error": "register first"})
        if u.path == "/v1/invite":
            with changed:
                code = secrets.token_hex(4).upper()
                state["invites"][code] = {"owner": uid, "expires": now() + INVITE_TTL}
                save()
            return self.send(200, {"code": code, "expires": INVITE_TTL})
        if u.path == "/v1/join":
            code = str(body.get("code", "")).strip().upper()
            with changed:
                inv = state["invites"].get(code)
                if not inv or inv["expires"] < now() or inv["owner"] == uid:
                    return self.send(404, {"error": "code expired or unknown"})
                owner = inv["owner"]
                if len(state["friends"].setdefault(uid, [])) >= MAX_FRIENDS:
                    return self.send(409, {"error": "friend list is full"})
                del state["invites"][code]
                if owner not in state["friends"][uid]:
                    state["friends"][uid].append(owner)
                state["friends"].setdefault(owner, [])
                if uid not in state["friends"][owner]:
                    state["friends"][owner].append(uid)
                save()
                changed.notify_all()
                return self.send(200, {"friend": public(owner)})
        if u.path == "/v1/remove":
            other = str(body.get("id", ""))
            with changed:
                if other in state["friends"].get(uid, []):
                    state["friends"][uid].remove(other)
                    if uid in state["friends"].get(other, []):
                        state["friends"][other].remove(uid)
                    save()
                return self.send(200, {"ok": True})
        if u.path == "/v1/recipes":
            to = str(body.get("to", ""))
            if to not in friend_ids(uid):
                return self.send(403, {"error": "not a friend"})
            if not str(body.get("code", "")).startswith("HOP-"):
                return self.send(400, {"error": "not a share code"})
            with changed:
                ev = publish(to, uid, body)
                save()
                changed.notify_all()
            return self.send(200, {"seq": ev["seq"]})
        return self.send(404, {"error": "not found"})


def main():
    load()
    srv = ThreadingHTTPServer((HOST, PORT), Handler)
    srv.daemon_threads = True
    print(f"hopper-mailbox on {HOST}:{PORT}, state {STATE}", file=sys.stderr)
    srv.serve_forever()


if __name__ == "__main__":
    main()
