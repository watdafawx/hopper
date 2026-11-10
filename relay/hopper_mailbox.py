#!/usr/bin/env python3
"""Hopper mailbox: pair PCs and carry recipes, never jars.

The desktop app registers once, gets an id and a token, pairs with a friend
using a one-time code, and then publishes or receives share codes. A share
code is a few KB; the mods themselves download from CurseForge, Modrinth,
GitHub and the other sources on each PC.

Friends can also be put in groups. A group lives here so every member sees
the same members and the same packs: a pack shared with a group goes to
everyone in it who said yes, and to anyone who joins later; whoever leaves
gets a "revoke" for it. See design/friends-sync.md for the protocol.

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
# Groups: how many one PC can own, how many people one holds, how many
# packs can be shared into one, and how many groups one publish may name.
MAX_GROUPS_OWNED = 20
MAX_GROUP_MEMBERS = 50
MAX_GROUP_PACKS = 100
MAX_GROUPS_PER_POST = 20
# Event kinds older desktops understand. Anything else is only sent to a
# client that asks with v=2, so an old Hopper never sees a revoke as a
# recipe with no code.
V1_KINDS = ("recipe",)
REVOKE_REASONS = ("unshared", "deleted")

lock = threading.RLock()
changed = threading.Condition(lock)
state = {
    "users": {},      # id -> {name, token, created, last_seen, mc_id}
    "friends": {},    # id -> [id]
    "invites": {},    # code -> {owner, expires, inst}
    "events": {},     # id -> [event]; see make_event
    "groups": {},     # gid -> {name, owner, created, members, packs, follows}
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
    os.makedirs(os.path.dirname(STATE) or ".", exist_ok=True)
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
    seen = u.get("last_seen", 0)
    return {
        "id": uid,
        "name": u.get("name", ""),
        "mc_id": u.get("mc_id", ""),
        "online": now() - seen <= ONLINE_SECS,
        "last_seen": seen,
    }


def friend_ids(uid):
    return list(state["friends"].get(uid, []))


def text(v, n):
    return str(v if v is not None else "")[:n]


def id_list(v, n):
    """A list of ids from a request body, at most n, without repeats."""
    if not isinstance(v, list):
        return []
    out = []
    for x in v[:n]:
        x = text(x, 64)
        if x and x not in out:
            out.append(x)
    return out


def make_event(to, kind, frm, **fields):
    """Queues one event for `to`. Every event has seq, kind, from and at;
    the rest depends on the kind (see design/friends-sync.md)."""
    state["seq"] += 1
    ev = {"seq": state["seq"], "kind": kind, "from": frm, "at": now()}
    ev.update(fields)
    q = state["events"].setdefault(to, [])
    q.append(ev)
    if len(q) > MAX_EVENTS:
        del q[: len(q) - MAX_EVENTS]
    return ev


def recipe_fields(body):
    return {
        "inst": text(body.get("inst"), 128),
        "key": text(body.get("key"), 128),
        "code": text(body.get("code"), 20000),
        "name": text(body.get("name"), 128),
        "left_out": [text(x, 200) for x in (body.get("left_out") or [])][:100],
    }


def publish(to, frm, body):
    """The original one-friend publish, kept for older desktops."""
    return make_event(to, "recipe", frm, direct=True, groups=[], **recipe_fields(body))


# ── groups ───────────────────────────────────────────────────────────


def joined(g, uid):
    m = g["members"].get(uid)
    return bool(m and m.get("joined"))


def joined_ids(g):
    return [u for u, m in g["members"].items() if m.get("joined")]


def group_of(gid, uid, need_joined=True):
    """The group, if `uid` is in it (or invited, when need_joined is
    False). Anyone else gets None and a 404, so a non-member can't even
    tell the group exists."""
    g = state["groups"].get(gid)
    if not g or uid not in g["members"]:
        return None
    if need_joined and not joined(g, uid):
        return None
    return g


def group_view(gid, g, uid):
    me = g["members"].get(uid, {})
    out = {
        "id": gid,
        "name": g["name"],
        "owner": g["owner"],
        "me": {"joined": bool(me.get("joined")), "in_step": bool(me.get("in_step"))},
        "members": [
            dict(public(m), joined=bool(s.get("joined")), in_step=bool(s.get("in_step")))
            for m, s in g["members"].items()
            if m in state["users"]
        ],
        "packs": [],
        "follows": [],
    }
    # Packs and follows only for members who said yes.
    if me.get("joined"):
        out["packs"] = [
            {"from": p["from"], "inst": p["inst"], "name": p["name"], "at": p["at"]}
            for p in g["packs"].values()
        ]
        out["follows"] = [dict(zip(("from", "inst"), k.split("/", 1))) for k in g["follows"]]
    return out


def tell_group(gid, g, actor, also=()):
    """Tells every member (and `also`) that the group changed; their
    desktops fetch GET /v1/groups again."""
    for m in set(list(g["members"].keys()) + list(also)):
        if m != actor:
            make_event(m, "group", actor, group=gid)


def drop_member(gid, g, m, actor):
    """Takes `m` out of the group: they get a revoke for every group pack,
    and the packs they shared into it are taken back from the others."""
    was_joined = joined(g, m)
    del g["members"][m]
    if was_joined:
        for k, p in list(g["packs"].items()):
            if p["from"] == m:
                del g["packs"][k]
                if k in g["follows"]:
                    g["follows"].remove(k)
                for o in joined_ids(g):
                    make_event(o, "revoke", m, inst=p["inst"], reason="left_group", direct=False, groups=[gid])
            else:
                make_event(m, "revoke", p["from"], inst=p["inst"], reason="left_group", direct=False, groups=[gid])
    tell_group(gid, g, actor, also=(m,))


def unfriend_groups(a, b):
    """`a` and `b` aren't friends any more: neither stays in a group the
    other owns, since groups are made of the owner's friends."""
    for gid, g in list(state["groups"].items()):
        if g["owner"] == a and b in g["members"]:
            drop_member(gid, g, b, a)
        elif g["owner"] == b and a in g["members"]:
            drop_member(gid, g, a, b)


class Handler(BaseHTTPRequestHandler):
    server_version = "HopperMailbox/2"
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
            v = json.loads(self.rfile.read(n) or b"{}")
        except Exception:
            return None
        return v if isinstance(v, dict) else None

    def send(self, status, obj):
        data = json.dumps(obj, separators=(",", ":")).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def fail(self, status, error, code=None):
        out = {"error": error}
        if code:
            out["code"] = code
        return self.send(status, out)

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
            return self.send(200, {"ok": True, "service": "hopper-mailbox", "v": 2})
        uid = self.auth()
        if not uid:
            return self.fail(401, "register or pair first")
        if u.path == "/v1/friends":
            with changed:
                return self.send(200, {
                    "friends": [public(x) for x in friend_ids(uid) if x in state["users"]],
                    "max": MAX_FRIENDS,
                })
        if u.path == "/v1/groups":
            with changed:
                return self.send(200, {"groups": [
                    group_view(gid, g, uid)
                    for gid, g in state["groups"].items()
                    if uid in g["members"]
                ]})
        if u.path == "/v1/recipes":
            try:
                after = int((q.get("after") or ["0"])[0])
                wait = max(0, min(120, int((q.get("wait") or ["0"])[0])))
                v = int((q.get("v") or ["1"])[0])
            except ValueError:
                return self.fail(400, "bad query")
            deadline = time.time() + wait
            with changed:
                while True:
                    evs = [
                        e for e in state["events"].get(uid, [])
                        if e["seq"] > after and (v >= 2 or e.get("kind", "recipe") in V1_KINDS)
                    ]
                    if evs or time.time() >= deadline:
                        out = []
                        for e in evs:
                            x = dict(e)
                            x.setdefault("kind", "recipe")
                            x["from_name"] = state["users"].get(e["from"], {}).get("name", "")
                            out.append(x)
                        return self.send(200, {"seq": state["seq"], "events": out})
                    changed.wait(timeout=max(0.1, deadline - time.time()))
        return self.fail(404, "not found")

    def do_POST(self):
        u = urlparse(self.path)
        body = self.body()
        if body is None:
            return self.fail(400, "bad body")
        if u.path == "/v1/register":
            name = text(body.get("name"), 64).strip() or "This PC"
            with changed:
                uid = secrets.token_hex(16)
                token = secrets.token_urlsafe(32)
                state["users"][uid] = {
                    "name": name,
                    "token": token,
                    "created": now(),
                    "last_seen": now(),
                    "mc_id": text(body.get("mc_id"), 64),
                }
                state["friends"].setdefault(uid, [])
                state["events"].setdefault(uid, [])
                save()
            return self.send(200, {"id": uid, "token": token, "name": name})
        uid = self.auth()
        if not uid:
            return self.fail(401, "register first")
        with changed:
            r = self.route(uid, u.path, body)
            if r is None:
                return self.fail(404, "not found")
            status, obj = r
            if status < 400:
                save()
                changed.notify_all()
        return self.send(status, obj)

    # Every POST below runs holding the lock; it returns (status, body).
    def route(self, uid, path, body):
        if path == "/v1/profile":
            me = state["users"].get(uid)
            if not me:
                return 404, {"error": "unknown account"}
            name = text(body.get("name"), 64).strip()
            if name:
                me["name"] = name
            me["mc_id"] = text(body.get("mc_id"), 64)
            return 200, {"ok": True}
        if path == "/v1/invite":
            code = secrets.token_hex(4).upper()
            state["invites"][code] = {
                "owner": uid,
                "expires": now() + INVITE_TTL,
                # The pack a share code's follow part is for, if any.
                "inst": text(body.get("inst"), 128),
            }
            # Forget run-out invites so the list can't grow forever.
            for c, inv in list(state["invites"].items()):
                if inv["expires"] < now():
                    del state["invites"][c]
            return 200, {"code": code, "expires": INVITE_TTL}
        if path == "/v1/join":
            return self.join(uid, body)
        if path == "/v1/remove":
            other = text(body.get("id"), 64)
            if other in state["friends"].get(uid, []):
                state["friends"][uid].remove(other)
                if uid in state["friends"].get(other, []):
                    state["friends"][other].remove(uid)
                unfriend_groups(uid, other)
                make_event(other, "unfriended", uid)
            return 200, {"ok": True}
        if path == "/v1/recipes":
            return self.recipes(uid, body)
        if path == "/v1/revoke":
            return self.revoke(uid, body)
        if path == "/v1/groups":
            return self.new_group(uid, body)
        parts = path.split("/")
        # /v1/groups/<gid> or /v1/groups/<gid>/<action>
        if len(parts) in (4, 5) and parts[1] == "v1" and parts[2] == "groups":
            action = parts[4] if len(parts) == 5 else "edit"
            return self.group_action(uid, parts[3], action, body)
        return None

    def join(self, uid, body):
        code = text(body.get("code"), 32).strip().upper()
        inv = state["invites"].get(code)
        if not inv or inv["expires"] < now() or inv["owner"] == uid:
            return 404, {"error": "code expired or unknown", "code": "invite_unknown"}
        owner = inv["owner"]
        mine = state["friends"].setdefault(uid, [])
        theirs = state["friends"].setdefault(owner, [])
        if owner not in mine:
            if len(mine) >= MAX_FRIENDS:
                return 409, {"error": "friend list is full", "code": "friends_full"}
            if len(theirs) >= MAX_FRIENDS:
                return 409, {"error": "their friend list is full", "code": "their_friends_full"}
        del state["invites"][code]
        if owner not in mine:
            mine.append(owner)
        if uid not in theirs:
            theirs.append(uid)
        inst = inv.get("inst", "")
        if inst:
            # A share code's follow part: the owner's desktop shares that
            # pack with the new friend when it hears this.
            make_event(owner, "joined", uid, inst=inst)
        return 200, {"friend": public(owner), "inst": inst}

    def recipes(self, uid, body):
        if not text(body.get("code"), 20000).startswith("HOP-"):
            return 400, {"error": "not a share code"}
        to = body.get("to")
        friends = friend_ids(uid)
        if isinstance(to, str):
            # Older desktops: one friend per call.
            if to not in friends:
                return 403, {"error": "not a friend"}
            ev = publish(to, uid, body)
            return 200, {"seq": ev["seq"], "sent": 1, "skipped": []}
        direct = id_list(to, MAX_FRIENDS)
        gids = id_list(body.get("groups"), MAX_GROUPS_PER_POST)
        fields = recipe_fields(body)
        if not fields["inst"]:
            return 400, {"error": "no instance"}
        # Who gets it, and how: one copy each, however many ways it
        # reaches them, so their desktop knows what a revoke takes away.
        reach = {}
        skipped = [f for f in direct if f not in friends]
        for f in direct:
            if f in friends:
                reach.setdefault(f, {"direct": False, "groups": []})["direct"] = True
        for gid in gids:
            g = group_of(gid, uid)
            if g is None:
                # Left or removed since the desktop last looked: the rest
                # still goes out.
                skipped.append(gid)
                continue
            k = uid + "/" + fields["inst"]
            if k not in g["packs"] and len(g["packs"]) >= MAX_GROUP_PACKS:
                return 409, {"error": "the group has too many packs", "code": "group_packs_full"}
            g["packs"][k] = dict(fields, **{"from": uid, "at": now()})
            for m in joined_ids(g):
                if m != uid:
                    reach.setdefault(m, {"direct": False, "groups": []})["groups"].append(gid)
        seq = state["seq"]
        for m, how in reach.items():
            seq = make_event(m, "recipe", uid, direct=how["direct"], groups=how["groups"], **fields)["seq"]
        return 200, {"seq": seq, "sent": len(reach), "skipped": skipped}

    def revoke(self, uid, body):
        inst = text(body.get("inst"), 128)
        if not inst:
            return 400, {"error": "no instance"}
        reason = text(body.get("reason"), 16)
        if reason not in REVOKE_REASONS:
            reason = "unshared"
        friends = friend_ids(uid)
        n = 0
        for f in id_list(body.get("to"), MAX_FRIENDS):
            if f in friends:
                make_event(f, "revoke", uid, inst=inst, reason=reason, direct=True, groups=[])
                n += 1
        for gid in id_list(body.get("groups"), MAX_GROUPS_PER_POST):
            g = group_of(gid, uid)
            if g is None:
                continue
            k = uid + "/" + inst
            g["packs"].pop(k, None)
            if k in g["follows"]:
                g["follows"].remove(k)
            for m in joined_ids(g):
                if m != uid:
                    make_event(m, "revoke", uid, inst=inst, reason=reason, direct=False, groups=[gid])
                    n += 1
        return 200, {"ok": True, "sent": n}

    def new_group(self, uid, body):
        name = text(body.get("name"), 64).strip()
        if not name:
            return 400, {"error": "name the group"}
        owned = sum(1 for g in state["groups"].values() if g["owner"] == uid)
        if owned >= MAX_GROUPS_OWNED:
            return 409, {"error": "you have too many groups", "code": "groups_full"}
        members = id_list(body.get("members"), MAX_GROUP_MEMBERS)
        friends = friend_ids(uid)
        if any(m not in friends for m in members):
            return 403, {"error": "not a friend"}
        if len(members) + 1 > MAX_GROUP_MEMBERS:
            return 409, {"error": "the group is full", "code": "group_full"}
        gid = secrets.token_hex(8)
        g = {
            "name": name,
            "owner": uid,
            "created": now(),
            # Everyone the owner adds is asked first; nothing reaches them
            # until they say yes on their own PC.
            "members": {uid: {"joined": True, "in_step": False}},
            "packs": {},
            "follows": [],
        }
        for m in members:
            if m != uid:
                g["members"][m] = {"joined": False, "in_step": False}
        state["groups"][gid] = g
        tell_group(gid, g, uid)
        return 200, {"group": group_view(gid, g, uid)}

    def group_action(self, uid, gid, action, body):
        if action in ("accept", "leave"):
            g = group_of(gid, uid, need_joined=False)
        else:
            g = group_of(gid, uid)
        if g is None:
            return 404, {"error": "no such group"}
        owner = g["owner"] == uid
        if action == "edit":
            if not owner:
                return 403, {"error": "only the group's owner changes it"}
            name = text(body.get("name"), 64).strip()
            if name:
                g["name"] = name
            friends = friend_ids(uid)
            add = [m for m in id_list(body.get("add"), MAX_GROUP_MEMBERS) if m not in g["members"]]
            if any(m not in friends for m in add):
                return 403, {"error": "not a friend"}
            if len(g["members"]) + len(add) > MAX_GROUP_MEMBERS:
                return 409, {"error": "the group is full", "code": "group_full"}
            for m in add:
                g["members"][m] = {"joined": False, "in_step": False}
            for m in id_list(body.get("remove"), MAX_GROUP_MEMBERS):
                if m != uid and m in g["members"]:
                    drop_member(gid, g, m, uid)
            tell_group(gid, g, uid)
            return 200, {"group": group_view(gid, g, uid)}
        if action == "delete":
            if not owner:
                return 403, {"error": "only the group's owner deletes it"}
            for m in list(g["members"].keys()):
                if m != uid:
                    drop_member(gid, g, m, uid)
            del state["groups"][gid]
            return 200, {"ok": True}
        if action == "accept":
            me = g["members"][uid]
            if not me.get("joined"):
                me["joined"] = True
                # What the group already shares reaches the new member now.
                for p in g["packs"].values():
                    if p["from"] != uid:
                        fields = {k: p[k] for k in ("inst", "key", "code", "name", "left_out") if k in p}
                        make_event(uid, "recipe", p["from"], direct=False, groups=[gid], **fields)
                tell_group(gid, g, uid)
            return 200, {"group": group_view(gid, g, uid)}
        if action == "leave":
            if owner:
                return 400, {"error": "the owner deletes the group instead", "code": "owner_leaves"}
            drop_member(gid, g, uid, uid)
            return 200, {"ok": True}
        if action == "in-step":
            g["members"][uid]["in_step"] = bool(body.get("on"))
            tell_group(gid, g, uid)
            return 200, {"group": group_view(gid, g, uid)}
        if action == "follow":
            frm = text(body.get("from"), 64)
            inst = text(body.get("inst"), 128)
            k = frm + "/" + inst
            if k not in g["packs"]:
                return 404, {"error": "the group doesn't share that pack"}
            if k not in g["follows"]:
                g["follows"].append(k)
            # Everyone who keeps the group in step follows it too, except
            # the one who followed and the one who shares it.
            for m, s in g["members"].items():
                if s.get("joined") and s.get("in_step") and m not in (uid, frm):
                    make_event(m, "group_follow", uid, group=gid, pack_from=frm, inst=inst)
            return 200, {"ok": True}
        return None


def main():
    load()
    srv = ThreadingHTTPServer((HOST, PORT), Handler)
    srv.daemon_threads = True
    print(f"hopper-mailbox on {HOST}:{PORT}, state {STATE}", file=sys.stderr)
    srv.serve_forever()


if __name__ == "__main__":
    main()
