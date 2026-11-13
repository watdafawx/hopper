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

This runs on a public machine, so everything a stranger can send is bounded:
body size, stored bytes per account, queue length, connections, long-polls,
sign-ups and failed invite guesses. Tokens are stored as sha256 hashes.
"""

import hashlib
import io
import ipaddress
import json
import os
import re
import secrets
import signal
import socket
import sys
import threading
import time
from collections import deque
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs


def _env_int(name, default):
    try:
        return int(os.environ.get(name, default))
    except ValueError:
        return default


STATE = os.environ.get("HOPPER_MAILBOX_STATE", "/var/lib/hopper-mailbox/state.json")
HOST = os.environ.get("HOPPER_MAILBOX_HOST", "127.0.0.1")
PORT = int(os.environ.get("HOPPER_MAILBOX_PORT", "8787"))
MAX_BODY = 64 * 1024
INVITE_TTL = 15 * 60
ONLINE_SECS = 90
MAX_FRIENDS = 100
MAX_EVENTS = 1000
# A backstop for one sender. Real packs don't reach it: a recipe replaces the
# older one for the same pack (per sender and pack), so this only stops a
# friend inventing endless pack names to push other people's events out.
MAX_EVENTS_PER_SENDER = 150
# Events are forgotten after this long: recipes and revokes keep the longest,
# since the desktop lists friends' packs from them.
EVENT_TTL = 14 * 24 * 3600
KEEP_TTL = 180 * 24 * 3600
# Bytes: what waits for one person, and what one account keeps in groups
# (a group nobody else has joined keeps much less).
MAX_QUEUE_BYTES = 2 * 1024 * 1024
MAX_PACK_BYTES = 1024 * 1024
SOLO_PACK_BYTES = 64 * 1024
# The whole service: accounts, and the size of the state file. Past either,
# sign-ups and new group packs get 503 `busy` until accounts age out.
MAX_USERS = _env_int("HOPPER_MAILBOX_MAX_USERS", 20000)
MAX_STATE_BYTES = _env_int("HOPPER_MAILBOX_MAX_STATE_BYTES", 32 * 1024 * 1024)
# The state file is the memory of the service (about 7 times its size once
# loaded, and the unit allows 512 MB), so its size is a budget:
#  - share codes (direct, or into groups) stop being accepted at 75% of it,
#    which leaves the last quarter for accounts, so sign-ups never starve;
#  - sign-ups stop at 100%;
#  - a file bigger than 150% is not loaded at all (see load()).
# A new version of a pack that is not bigger than the one it replaces is
# always taken, so normal updates keep working when it is full.
EVENT_BUDGET_FRACTION = 0.75
LOAD_LIMIT_FRACTION = 1.5
# What one sender may have waiting for all their friends together, so a few
# paired accounts can't spend the budget on their own.
SENDER_QUOTA_BYTES = 3 * 1024 * 1024
# Exit status for "the state file is bad, unreadable or too big": the unit
# does not restart on it (RestartPreventExitStatus=78), a person must look.
# An ordinary crash exits 1 and is restarted.
EXIT_BAD_STATE = 78
# Accounts with no friends and no groups, unseen this long, are removed.
IDLE_PRUNE_SECS = 30 * 24 * 3600
PRUNE_EVERY = 3600
# Groups: how many one PC can own, how many people one holds, how many
# packs can be shared into one, and how many groups one publish may name.
MAX_GROUPS_OWNED = 20
MAX_GROUP_MEMBERS = 50
MAX_GROUP_PACKS = 100
MAX_GROUPS_PER_POST = 20
# Stamps one group copy's code may carry (one per mod changed from the pack).
MAX_MARKS = 2000
# A group copy's shared mod settings (base64 of deflated JSON, made by the
# desktop): the whole shared state rides with every recipe, so a member back
# after a month gets every setting the group changed.
MAX_CONFIGS = 16384
# Abuse limits.
SOCKET_TIMEOUT = 30           # seconds a read or write may stall
REQUEST_DEADLINE = 30         # seconds for a whole request's headers and body
MAX_CONNECTIONS = _env_int("HOPPER_MAILBOX_MAX_CONNECTIONS", 2000)
MAX_POLLS_PER_USER = 3        # long-polls one account may hold open
MAX_WAIT = 90                 # longest a long-poll may ask to wait
REGISTER_PER_HOUR = _env_int("HOPPER_MAILBOX_REGISTER_LIMIT", 60)   # per client IP
JOIN_FAILS = 10               # failed joins per account ...
JOIN_FAILS_IP = 30            # ... and per address ...
JOIN_FAIL_WINDOW = 15 * 60    # ... in this many seconds
MAX_LIVE_INVITES = 20         # per account
SAVE_INTERVAL = 1.5           # seconds between background saves
# Event kinds older desktops understand. Anything else is only sent to a
# client that asks with v=2, so an old Hopper never sees a revoke as a
# recipe with no code.
V1_KINDS = ("recipe",)
REVOKE_REASONS = ("unshared", "deleted")
# These two matter most; a full queue drops everything else first.
KEEP_KINDS = ("recipe", "revoke")

lock = threading.RLock()


def _fresh_state():
    return {
        "users": {},      # id -> {name, token_hash, created, last_seen, mc_id}
        "friends": {},    # id -> [id]
        "invites": {},    # code -> {owner, expires, inst}
        "events": {},     # id -> [event]; see make_event
        "groups": {},     # gid -> {name, owner, created, members, packs, follows}
        "seq": 0,
    }


state = _fresh_state()
token_index = {}          # sha256(token) -> id, built from state["users"]
polls = {}                # id -> [poll] for the long-polls open now
wakers = {}               # id -> Condition, only while someone waits


# ── rate limits ──────────────────────────────────────────────────────


class Limiter:
    """Counts events per key inside a sliding window."""

    def __init__(self):
        self.lock = threading.Lock()
        self.hits = {}

    def count(self, key, window):
        t = time.time()
        with self.lock:
            q = self.hits.get(key)
            if not q:
                return 0
            while q and q[0] <= t - window:
                q.popleft()
            if not q:
                del self.hits[key]
                return 0
            return len(q)

    def add(self, key, window):
        t = time.time()
        with self.lock:
            self._forget(t, window)
            self.hits.setdefault(key, deque()).append(t)

    def take(self, key, window, limit):
        """Counts and records in one step, so a burst of requests that all
        arrive before any finishes can't each see room that is already
        spoken for. False when `key` is at `limit`."""
        t = time.time()
        with self.lock:
            q = self.hits.get(key)
            if q:
                while q and q[0] <= t - window:
                    q.popleft()
            if q is not None and len(q) >= limit:
                return False
            self._forget(t, window)
            self.hits.setdefault(key, deque()).append(t)
            return True

    def _forget(self, t, window):
        if len(self.hits) > 50000:
            # Forget keys whose window is over, so a flood of addresses
            # can't grow this without end.
            for k in [k for k, q in self.hits.items() if not q or q[-1] <= t - window]:
                del self.hits[k]

    def clear(self):
        with self.lock:
            self.hits.clear()


register_limit = Limiter()
join_limit = Limiter()


# ── state file ───────────────────────────────────────────────────────


def hash_token(token):
    return hashlib.sha256(token.encode("utf-8", "replace")).hexdigest()


def rebuild_index():
    token_index.clear()
    for uid, u in state["users"].items():
        if u.get("token_hash"):
            token_index[u["token_hash"]] = uid


def reset(path=None):
    """Empty state; tests use this."""
    global STATE
    with lock:
        if path:
            STATE = path
        state.clear()
        state.update(_fresh_state())
        token_index.clear()
        polls.clear()
        wakers.clear()
        register_limit.clear()
        join_limit.clear()
        _writer["dirty"] = False
        _writer["size"] = 0
        _writer["added"] = 0
        _sender_bytes.clear()
        _closing.clear()


def load():
    """Reads the state file. A missing file is a fresh start.

    A file that is there but not valid (bad JSON or encoding, or the wrong
    shape) is moved aside as `<name>.corrupt-<time>` and the service exits
    with status 78. A file that can't be read at all (rights, disk) is left
    exactly as it is, and the service exits with status 78 too. Either way it
    never starts empty on top of real data, and the unit's
    `RestartPreventExitStatus=78` keeps systemd from starting it again."""
    try:
        on_disk = os.path.getsize(STATE)
        if on_disk > load_limit():
            print(f"state load refused: {STATE} is {on_disk} bytes, over the {load_limit()} this service "
                  "can hold in memory; file left as it is; exiting (see relay/README.md, \"A state file "
                  "that is too big\")", file=sys.stderr)
            sys.exit(EXIT_BAD_STATE)
        with open(STATE, "r", encoding="utf-8") as f:
            raw = f.read()
        s = json.loads(raw)
        if not isinstance(s, dict):
            raise ValueError("state is not an object")
        for k in ("users", "friends", "invites", "events", "groups"):
            if k in s and not isinstance(s[k], dict):
                raise ValueError(f"state.{k} is not an object")
        if "seq" in s and not isinstance(s["seq"], int):
            raise ValueError("state.seq is not a number")
    except FileNotFoundError:
        return
    except (ValueError, UnicodeDecodeError) as e:   # JSONDecodeError is a ValueError
        aside = f"{STATE}.corrupt-{int(time.time())}"
        try:
            os.replace(STATE, aside)
            where = f"moved to {aside}"
        except OSError as e2:
            where = f"could not be moved aside ({e2})"
        print(f"state load failed: {e}; file {where}; exiting", file=sys.stderr)
        sys.exit(EXIT_BAD_STATE)
    except Exception as e:   # OSError, or anything we didn't expect
        print(f"state load failed: {type(e).__name__}: {e}; file left as it is; exiting", file=sys.stderr)
        sys.exit(EXIT_BAD_STATE)
    _writer["size"] = len(raw)
    with lock:
        for k in state:
            if k in s:
                state[k] = s[k]
        # Tokens used to be stored as they are. Keep only their hashes; the
        # PCs still send the token itself, which hashes to the same value.
        migrated = 0
        for u in state["users"].values():
            tok = u.pop("token", None)
            if tok and not u.get("token_hash"):
                u["token_hash"] = hash_token(tok)
                migrated += 1
        rebuild_index()
        rebuild_sender_index()
        if migrated:
            print(f"hashed {migrated} stored tokens", file=sys.stderr)
            mark_dirty()
        gone = prune_accounts()
        if gone:
            print(f"removed {gone} idle accounts with no friends or groups", file=sys.stderr)


# The saver: requests only flip `dirty`; one thread writes the file.
# `size` is how big the last state file was, for the service-wide cap.
_writer = {"dirty": False, "size": 0, "added": 0, "thread": None, "stop": threading.Event(), "wake": threading.Event()}
write_lock = threading.Lock()
# Set when the service is stopping: new requests are turned away.
_closing = threading.Event()


def prune_accounts():
    """Forgets accounts with no friends, no groups and no activity for
    IDLE_PRUNE_SECS (a PC that signed up and was never used, or was wiped),
    and events that have outlived their age. Returns how many accounts."""
    with lock:
        t = now()
        cutoff = t - IDLE_PRUNE_SECS
        in_group = set()
        for g in state["groups"].values():
            in_group.update(g["members"])
        drop = [
            uid for uid, u in state["users"].items()
            if u.get("last_seen", 0) < cutoff and not state["friends"].get(uid) and uid not in in_group
        ]
        for uid in drop:
            u = state["users"].pop(uid)
            if token_index.get(u.get("token_hash")) == uid:
                del token_index[u["token_hash"]]
            state["friends"].pop(uid, None)
            state["events"].pop(uid, None)
        if drop:
            gone = set(drop)
            for c, inv in list(state["invites"].items()):
                if inv["owner"] in gone:
                    del state["invites"][c]
            for q in state["events"].values():
                q[:] = [e for e in q if e["from"] not in gone]
        changed = len(drop)
        rebuild_sender_index()
        for q in state["events"].values():
            before = len(q)
            q[:] = [e for e in q if not event_expired(e, t)]
            changed += before - len(q)
        if changed:
            mark_dirty()
        return len(drop)


def mark_dirty():
    _writer["dirty"] = True
    _writer["wake"].set()


def _write_file(data):
    d = os.path.dirname(STATE) or "."
    os.makedirs(d, mode=0o700, exist_ok=True)
    tmp = STATE + ".tmp"
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | getattr(os, "O_BINARY", 0), 0o600)
    try:
        os.chmod(tmp, 0o600)  # a leftover tmp file may have looser rights
    except OSError:
        pass
    with os.fdopen(fd, "wb") as f:
        f.write(data)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, STATE)
    # The rename itself only survives a power cut once the folder is synced.
    try:
        dfd = os.open(d, os.O_RDONLY)
        try:
            os.fsync(dfd)
        finally:
            os.close(dfd)
    except OSError:
        pass  # Windows can't open a folder; the tests run there


def flush():
    """Writes the state if it changed. The JSON is made holding the lock; the
    slow part (disk) happens after it is let go. Returns False if the write
    failed (it is tried again on the next round)."""
    with write_lock:
        with lock:
            if not _writer["dirty"]:
                return True
            _writer["dirty"] = False
            data = json.dumps(state, separators=(",", ":")).encode("utf-8")
            _writer["size"] = len(data)
            _writer["added"] = 0
        try:
            _write_file(data)
            return True
        except Exception as e:
            _writer["dirty"] = True
            _writer["wake"].set()
            print(f"state save failed: {e}", file=sys.stderr)
            return False


def _writer_loop():
    last_prune = time.time()
    while not _writer["stop"].is_set():
        # Nothing in here may end this thread: a stopped saver would lose
        # every change until the next restart.
        try:
            _writer["wake"].wait(timeout=30)
            _writer["wake"].clear()
            if _writer["stop"].is_set():
                break
            if time.time() - last_prune >= PRUNE_EVERY:
                last_prune = time.time()
                prune_accounts()
            # Let a burst of changes become one write.
            _writer["stop"].wait(SAVE_INTERVAL)
            flush()
        except Exception as e:
            print(f"saver error: {type(e).__name__}: {e}", file=sys.stderr)
            _writer["stop"].wait(2)


def start_writer():
    if _writer["thread"] and _writer["thread"].is_alive():
        return
    _writer["stop"].clear()
    t = threading.Thread(target=_writer_loop, name="saver", daemon=True)
    _writer["thread"] = t
    t.start()


def stop_writer():
    """Stops the saver and writes whatever is unsaved. True if that worked."""
    _writer["stop"].set()
    _writer["wake"].set()
    t = _writer["thread"]
    if t:
        t.join(timeout=10)
    _writer["thread"] = None
    return flush()


# ── helpers ──────────────────────────────────────────────────────────


def now():
    return int(time.time())


def user_by_token(token):
    if not token or len(token) < 20:
        return None
    uid = token_index.get(hash_token(token))
    u = state["users"].get(uid) if uid else None
    return (uid, u) if u else None


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


def text(v, n, nbytes=None):
    """A string cut to n characters and to nbytes of UTF-8 (default 4 per
    character), so a limit on length is also a limit on size."""
    s = str(v if v is not None else "")[:n]
    limit = nbytes if nbytes is not None else 4 * n
    b = s.encode("utf-8", "replace")
    if len(b) > limit:
        s = b[:limit].decode("utf-8", "ignore")
    return s


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


def size_of(v):
    """Rough bytes a stored value takes."""
    if isinstance(v, str):
        return len(v) if v.isascii() else len(v.encode("utf-8", "replace"))
    if isinstance(v, (list, tuple)):
        return sum(size_of(x) for x in v) + 2 * len(v)
    if isinstance(v, dict):
        return sum(size_of(k) + size_of(x) for k, x in v.items())
    return 8


def wake(uid):
    c = wakers.get(uid)
    if c:
        c.notify_all()


def event_expired(e, t):
    ttl = KEEP_TTL if e.get("kind", "recipe") in KEEP_KINDS else EVENT_TTL
    return t - e.get("at", t) > ttl


def load_limit():
    return int(MAX_STATE_BYTES * LOAD_LIMIT_FRACTION)


def state_used():
    """About how big the state is now: the last save plus what came in since."""
    return _writer["size"] + _writer["added"]


def event_room(extra=0):
    """Whether share codes may grow the state by `extra` more bytes."""
    return state_used() + extra <= int(MAX_STATE_BYTES * EVENT_BUDGET_FRACTION)


# Bytes of events each sender has waiting in all queues. Additions are exact;
# removals are not tracked, so the number can only be too high. It is checked
# against the real scan before anyone is refused (sender_over_quota).
_sender_bytes = {}


def rebuild_sender_index():
    _sender_bytes.clear()
    for q in state["events"].values():
        for e in q:
            _sender_bytes[e["from"]] = _sender_bytes.get(e["from"], 0) + size_of(e)


def sender_exact(frm):
    n = sum(size_of(e) for q in state["events"].values() for e in q if e["from"] == frm)
    _sender_bytes[frm] = n
    return n


def sender_over_quota(frm, extra):
    if _sender_bytes.get(frm, 0) + extra <= SENDER_QUOTA_BYTES:
        return False
    return sender_exact(frm) + extra > SENDER_QUOTA_BYTES


def recipe_size(frm, how, fields):
    """The size make_event will give the event, worked out the same way."""
    head = {"seq": 0, "kind": "recipe", "from": frm, "at": 0,
            "direct": True, "groups": (how or {}).get("groups", [])}
    return size_of(head) + size_of(fields)


def recipe_growth(to, frm, how, fields):
    """How many bytes a recipe from `frm` adds to `to`'s queue: all of it for
    a new pack, the difference when it replaces the older version."""
    new = recipe_size(frm, how, fields)
    for e in state["events"].get(to, []):
        if e["from"] == frm and e.get("kind", "recipe") == "recipe" and e.get("inst") == fields["inst"]:
            return new - size_of(e)
    return new


def check_recipe_room(frm, reach, fields):
    """None if the recipe may go to everyone in `reach`, else (status, body).
    Nothing has been changed yet."""
    growth = [recipe_growth(m, frm, how, fields) for m, how in reach.items()]
    extra = sum(growth)
    # Over the budget only what makes the state bigger is refused: a new
    # version that is not bigger than the one it replaces always goes.
    if extra > 0 and not event_room(extra):
        return 503, {"error": "the sync service is full; try again later", "code": "busy"}
    if extra > 0 and sender_over_quota(frm, extra):
        return 409, {"error": "you have shared too much with your friends at once", "code": "storage_full"}
    return None


def pick_evictee(q):
    """What a full queue loses. Anything but a recipe or revoke goes first
    (oldest). Otherwise the oldest event of the sender holding the most
    bytes in this queue, so a friend sending a lot pays for it and someone
    with one small pack waiting keeps it."""
    for e in q:
        if e.get("kind", "recipe") not in KEEP_KINDS:
            return e
    per = {}
    for e in q:
        per[e["from"]] = per.get(e["from"], 0) + size_of(e)
    big = max(per, key=per.get)
    for e in q:
        if e["from"] == big:
            return e
    return q[0]


def pick_victim(evs):
    """The oldest event a full queue can lose: not a recipe or a revoke if
    there is any other, else the oldest of all."""
    for e in evs:
        if e.get("kind", "recipe") not in KEEP_KINDS:
            return e
    return evs[0]


def trim_queue(q, frm):
    t = now()
    q[:] = [e for e in q if not event_expired(e, t)]
    mine = [e for e in q if e["from"] == frm]
    while len(mine) > MAX_EVENTS_PER_SENDER:
        v = pick_victim(mine)
        mine.remove(v)
        q.remove(v)
    size = sum(size_of(e) for e in q)
    while q and (len(q) > MAX_EVENTS or size > MAX_QUEUE_BYTES):
        v = pick_evictee(q)
        q.remove(v)
        size -= size_of(v)


def same_event(a, b):
    """Whether `b` makes the older `a` pointless. Each kind has a key, so one
    sender can have only as many events waiting as it has packs or groups,
    not one per call."""
    if a.get("kind", "recipe") != b["kind"] or a["from"] != b["from"]:
        return False
    k = b["kind"]
    if k == "recipe":
        return a.get("inst") == b.get("inst")
    if k in ("group", "group_follow"):
        # These only say "read the groups again", so one waiting is enough.
        return a.get("group") == b.get("group")
    if k == "revoke":
        return (a.get("inst"), a.get("direct"), a.get("groups")) == (b.get("inst"), b.get("direct"), b.get("groups"))
    if k == "joined":
        return a.get("inst") == b.get("inst")
    return k == "unfriended"


def make_event(to, kind, frm, **fields):
    """Queues one event for `to`. Every event has seq, kind, from and at;
    the rest depends on the kind (see design/friends-sync.md)."""
    state["seq"] += 1
    ev = {"seq": state["seq"], "kind": kind, "from": frm, "at": now()}
    ev.update(fields)
    q = state["events"].setdefault(to, [])
    # Only a recipe's newest code matters (a desktop reading the feed from
    # the start doesn't apply each old version in turn), and likewise for
    # the other kinds: a newer one replaces the older one it makes useless.
    q[:] = [e for e in q if not same_event(e, ev)]
    q.append(ev)
    n = size_of(ev)
    _writer["added"] += n
    _sender_bytes[frm] = _sender_bytes.get(frm, 0) + n
    trim_queue(q, frm)
    wake(to)
    return ev


def marks_of(v):
    """A group copy's stamps, passed on as they are: {mod key: {seq, by}},
    when each mod got the way the code has it. Cut to MAX_MARKS; anything
    else is dropped."""
    if not isinstance(v, dict):
        return {}
    out = {}
    for k, m in list(v.items())[:MAX_MARKS]:
        if not isinstance(m, dict):
            continue
        seq = m.get("seq")
        if not isinstance(seq, int) or isinstance(seq, bool) or seq < 0:
            continue
        out[text(k, 300, 600)] = {"seq": min(seq, 2 ** 53), "by": text(m.get("by"), 64)}
    return out


def recipe_fields(body):
    left = body.get("left_out")
    if not isinstance(left, list):
        left = []
    fields = {
        "inst": text(body.get("inst"), 128),
        "key": text(body.get("key"), 128),
        "code": text(body.get("code"), 20000, 20000),
        "name": text(body.get("name"), 128),
        "left_out": [text(x, 200, 400) for x in left[:100]],
    }
    marks = marks_of(body.get("marks"))
    if marks:
        fields["marks"] = marks
    configs = body.get("configs")
    if isinstance(configs, str) and configs:
        fields["configs"] = text(configs, MAX_CONFIGS, MAX_CONFIGS)
    return fields


def pack_bytes(p):
    return size_of([p.get("inst", ""), p.get("key", ""), p.get("name", ""), p.get("code", ""), p.get("left_out", []),
                    p.get("marks", {}), p.get("configs", "")])


def pack_usage(uid):
    """What `uid` keeps in groups, as (all, in groups nobody else has joined).
    Counted against the one who shares, so a group is no free storage, and a
    group of one holds much less than a real one (SOLO_PACK_BYTES)."""
    total = solo = 0
    for g in state["groups"].values():
        alone = not any(m != uid for m in joined_ids(g))
        for p in g["packs"].values():
            if p["from"] == uid:
                b = pack_bytes(p)
                total += b
                if alone:
                    solo += b
    return total, solo


def publish(to, frm, body):
    """The original one-friend publish, kept for older desktops. Returns the
    event, or (status, body) when there is no room."""
    fields = recipe_fields(body)
    no = check_recipe_room(frm, {to: None}, fields)
    if no:
        return no
    return make_event(to, "recipe", frm, direct=True, groups=[], **fields)


def prune_invites():
    t = now()
    for c, inv in list(state["invites"].items()):
        if inv["expires"] < t:
            del state["invites"][c]


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


def member_view(viewer, m, s):
    """One member as `viewer` may see them. Presence and the Minecraft id
    are for friends (and yourself); the others only get a name."""
    out = {
        "id": m,
        "name": state["users"].get(m, {}).get("name", ""),
        "mc_id": "",
        "online": False,
        "last_seen": 0,
        "joined": bool(s.get("joined")),
        "in_step": False,
    }
    if m == viewer or m in state["friends"].get(viewer, []):
        out.update(public(m))
        out["in_step"] = bool(s.get("in_step"))
    return out


def group_view(gid, g, uid):
    me = g["members"].get(uid, {})
    out = {
        "id": gid,
        "name": g["name"],
        "owner": g["owner"],
        "me": {"joined": bool(me.get("joined")), "in_step": bool(me.get("in_step"))},
        "members": [],
        "packs": [],
        "follows": [],
    }
    if not me.get("joined"):
        # An invitation shows the name and who made it, nothing else, until
        # the invited PC says yes.
        owner = g["owner"]
        if owner in state["users"]:
            out["members"] = [dict(public(owner), joined=True, in_step=False)]
        return out
    out["members"] = [member_view(uid, m, s) for m, s in g["members"].items() if m in state["users"]]
    out["packs"] = [
        {"from": p["from"], "inst": p["inst"], "name": p["name"], "at": p["at"]}
        for p in g["packs"].values()
    ]
    out["follows"] = [dict(zip(("from", "inst"), k.split("/", 1))) for k in g["follows"]]
    return out


def group_pack_events(uid, after):
    """The recipe events for packs a group already shared when `uid` joined
    it. They are made up here when the feed is read, all numbered with the
    joining, so they arrive once (the desktop remembers the newest number)
    and again on a fresh start, like any queued event, but cost the queue
    nothing and can't be pushed out of it."""
    out = []
    for gid, g in state["groups"].items():
        m = g["members"].get(uid)
        since = m.get("since") if m and m.get("joined") else None
        if not since or since <= after:
            continue
        for p in g["packs"].values():
            if p["from"] != uid and p.get("seq", 0) <= since:
                ev = {"seq": since, "kind": "recipe", "from": p["from"], "at": p.get("at", 0),
                      "direct": False, "groups": [gid]}
                for f in ("inst", "key", "code", "name", "left_out", "marks", "configs"):
                    if f in p:
                        ev[f] = p[f]
                out.append(ev)
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


# ── the server ───────────────────────────────────────────────────────

_TOKEN = re.compile(r"[!#$%&'*+\-.^_`|~0-9A-Za-z]+")   # RFC 9110 header names
_CONTROL = re.compile(r"[^\x20-\x7e]")
_DIGITS = re.compile(r"[0-9]{1,12}")


def clean(s):
    """A log-safe string: no control characters, so a request can't forge
    log lines."""
    return _CONTROL.sub(lambda m: "\\x%02x" % ord(m.group()), str(s))


def net_key(ip):
    """IPv6 addresses are counted by /64, since one customer gets a whole one."""
    try:
        a = ipaddress.ip_address(ip)
    except ValueError:
        return ip
    if a.version == 6:
        return str(ipaddress.ip_network(f"{a}/64", strict=False).network_address)
    return str(a)


class _DeadlineReader(io.RawIOBase):
    """The request side of the socket, which gives up when the handler's
    deadline passes. The socket timeout alone only limits each wait, so a
    client sending a byte every few seconds could hold a thread forever."""

    def __init__(self, sock, handler):
        self.sock = sock
        self.handler = handler

    def readable(self):
        return True

    def readinto(self, b):
        left = self.handler.deadline - time.monotonic()
        if left <= 0:
            raise TimeoutError("request took too long")
        self.sock.settimeout(min(left, self.handler.timeout or left))
        return self.sock.recv_into(b)


class Handler(BaseHTTPRequestHandler):
    server_version = "hopper-mailbox"
    sys_version = ""
    protocol_version = "HTTP/1.1"
    timeout = SOCKET_TIMEOUT

    def setup(self):
        super().setup()
        self.deadline = time.monotonic() + REQUEST_DEADLINE
        old, self.rfile = self.rfile, io.BufferedReader(_DeadlineReader(self.connection, self))
        old.close()

    def handle_one_request(self):
        # Each request gets REQUEST_DEADLINE seconds to arrive in full (an
        # idle keep-alive connection is dropped by the same clock).
        self.deadline = time.monotonic() + REQUEST_DEADLINE
        super().handle_one_request()

    def version_string(self):
        return self.server_version

    # -- who is asking
    def client_ip(self):
        peer = self.client_address[0]
        headers = getattr(self, "headers", None)
        h = headers.get("CF-Connecting-IP") if headers is not None else None
        # Only the tunnel (on this machine) is believed about who is behind it.
        try:
            loopback = ipaddress.ip_address(peer).is_loopback
        except ValueError:
            loopback = False
        if h and loopback:
            try:
                return str(ipaddress.ip_address(h.strip()))
            except ValueError:
                pass
        return peer

    def log_message(self, fmt, *args):
        try:
            ip = self.client_ip()
        except Exception:
            ip = "-"
        sys.stderr.write("%s %s\n" % (clean(ip), clean(fmt % args)))

    # -- answers
    def send_error(self, code, message=None, explain=None):
        # The library's own error pages are HTML; ours are JSON.
        self.close_connection = True
        try:
            self.send_json(code, {"error": message or "error"}, log=False)
        except OSError:
            pass

    def send(self, status, obj):
        self.send_json(status, obj)

    def encode(self, obj):
        return json.dumps(obj, separators=(",", ":")).encode()

    def send_json(self, status, obj, log=True, raw=None):
        data = raw if raw is not None else self.encode(obj)
        # A request body we didn't read would be taken for the next request.
        if getattr(self, "_unread", False):
            self.close_connection = True
        if log:
            self.send_response(status)
        else:
            self.send_response_only(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        if status in (429, 503):
            self.send_header("Retry-After", "60")
        if self.close_connection:
            self.send_header("Connection", "close")
        self.end_headers()
        if getattr(self, "command", None) != "HEAD":
            self.wfile.write(data)

    def fail(self, status, error, code=None):
        out = {"error": error}
        if code:
            out["code"] = code
        return self.send(status, out)

    # -- framing: one request, one body, no ambiguity
    def framing(self):
        """Checks how the body is delimited. Returns the declared length, or
        None after answering an error (the connection then closes)."""
        self._unread = False
        h = self.headers
        # Header names must be RFC tokens, and the parser must have had no
        # complaint about the block (a folded or malformed line is how two
        # servers come to read one request differently).
        if h.defects or any(not _TOKEN.fullmatch(k) for k in h.keys()):
            self._unread = True
            self.fail(400, "bad header", "bad_framing")
            return None
        if h.get_all("Transfer-Encoding"):
            self._unread = True
            self.fail(400, "chunked bodies are not accepted", "bad_framing")
            return None
        cls = h.get_all("Content-Length") or []
        if len(cls) > 1:
            self._unread = True
            self.fail(400, "bad Content-Length", "bad_framing")
            return None
        if not cls:
            return 0
        v = cls[0].strip()
        if not _DIGITS.fullmatch(v):
            self._unread = True
            self.fail(400, "bad Content-Length", "bad_framing")
            return None
        n = int(v)
        self._unread = n > 0
        if n > MAX_BODY:
            self.fail(413, "body too large", "too_large")
            return None
        return n

    def read_body(self, n):
        """The JSON object sent, or None (the caller answers 400)."""
        raw = self.rfile.read(n) if n else b""
        self._unread = False
        try:
            v = json.loads(raw or b"{}")
        except Exception:
            return None
        return v if isinstance(v, dict) else None

    def auth(self):
        h = self.headers.get("Authorization", "")
        token = h[7:].strip() if h.startswith("Bearer ") else ""
        with lock:
            found = user_by_token(token)
            if not found:
                return None
            uid, u = found
            u["last_seen"] = now()
            return uid

    # -- requests
    def run(self, fn):
        """Runs a request; whatever goes wrong becomes a JSON 500 and one
        log line, and a stalled or dropped peer just ends the connection."""
        try:
            if _closing.is_set():
                self._unread = True
                self.close_connection = True
                return self.send_json(503, {"error": "restarting, try again", "code": "busy"})
            fn()
        except (socket.timeout, TimeoutError, ConnectionError):
            self.close_connection = True
        except Exception as e:
            self.log_message("internal error: %s: %s", type(e).__name__, e)
            self.close_connection = True
            try:
                self.send_json(500, {"error": "internal error", "code": "internal"})
            except Exception:
                pass

    def do_GET(self):
        self.run(self._get)

    def do_POST(self):
        self.run(self._post)

    def _get(self):
        n = self.framing()
        if n is None:
            return
        # A GET with a body is answered, but the body is never read, so the
        # connection closes afterwards (framing() marked it unread).
        u = urlparse(self.path)
        q = parse_qs(u.query)
        if u.path == "/v1/health":
            return self.send(200, {"ok": True, "service": "hopper-mailbox", "v": 2, "configs": True})
        uid = self.auth()
        if not uid:
            return self.fail(401, "register or pair first")
        if u.path == "/v1/friends":
            with lock:
                data = self.encode({
                    "friends": [public(x) for x in friend_ids(uid) if x in state["users"]],
                    "max": MAX_FRIENDS,
                })
            return self.send_json(200, None, raw=data)
        if u.path == "/v1/groups":
            with lock:
                data = self.encode({"groups": [
                    group_view(gid, g, uid)
                    for gid, g in state["groups"].items()
                    if uid in g["members"]
                ]})
            return self.send_json(200, None, raw=data)
        if u.path == "/v1/recipes":
            try:
                after = int((q.get("after") or ["0"])[0])
                wait = max(0, min(MAX_WAIT, int((q.get("wait") or ["0"])[0])))
                v = int((q.get("v") or ["1"])[0])
            except ValueError:
                return self.fail(400, "bad query")
            return self.long_poll(uid, after, wait, v)
        return self.fail(404, "not found")

    def long_poll(self, uid, after, wait, v):
        deadline = time.time() + wait
        poll = {"stop": False}
        with lock:
            mine = polls.setdefault(uid, [])
            mine.append(poll)
            # An account holds a few polls at most. A PC that lost its
            # connection leaves one behind; the oldest gives way.
            while len(mine) > MAX_POLLS_PER_USER:
                mine.pop(0)["stop"] = True
            cond = wakers.get(uid)
            if cond is None:
                cond = wakers[uid] = threading.Condition(lock)
            wake(uid)
            status = 200
            try:
                while True:
                    evs = [
                        e for e in state["events"].get(uid, [])
                        if e["seq"] > after and (v >= 2 or e.get("kind", "recipe") in V1_KINDS)
                    ]
                    evs += group_pack_events(uid, after)
                    evs.sort(key=lambda e: e["seq"])
                    if poll["stop"]:
                        # Pushed out by a newer poll from the same account.
                        # An error, not an empty answer: the desktop waits
                        # before asking again, where an empty one would be
                        # asked again at once and the two would take turns.
                        status = 409
                        data = self.encode({"error": "another connection took over this feed", "code": "poll_superseded"})
                        break
                    if evs or time.time() >= deadline:
                        out = []
                        for e in evs:
                            x = dict(e)
                            x.setdefault("kind", "recipe")
                            x["from_name"] = state["users"].get(e["from"], {}).get("name", "")
                            out.append(x)
                        data = self.encode({"seq": state["seq"], "events": out})
                        break
                    cond.wait(timeout=max(0.1, deadline - time.time()))
            finally:
                if poll in mine:
                    mine.remove(poll)
                if not mine:
                    polls.pop(uid, None)
                    wakers.pop(uid, None)
        # Writing to the socket happens without the lock.
        return self.send_json(status, None, raw=data)

    def _post(self):
        n = self.framing()
        if n is None:
            return
        path = urlparse(self.path).path
        if path == "/v1/register":
            with lock:
                full = len(state["users"]) >= MAX_USERS or state_used() >= MAX_STATE_BYTES
            if full:
                return self.fail(503, "the sync service is full; try again later", "busy")
            # The place in the hourly allowance is taken now, before the body
            # is read: otherwise a crowd of slow requests could all pass the
            # check while none had been counted yet.
            ip = net_key(self.client_ip())
            if not register_limit.take(ip, 3600, REGISTER_PER_HOUR):
                return self.fail(429, "too many new accounts from here; try again later", "rate_limited")
            body = self.read_body(n)
            if body is None:
                return self.fail(400, "bad body")
            name = text(body.get("name"), 64).strip() or "This PC"
            uid = secrets.token_hex(16)
            token = secrets.token_urlsafe(32)
            with lock:
                state["users"][uid] = {
                    "name": name,
                    "token_hash": hash_token(token),
                    "created": now(),
                    "last_seen": now(),
                    "mc_id": text(body.get("mc_id"), 64),
                }
                token_index[state["users"][uid]["token_hash"]] = uid
                state["friends"].setdefault(uid, [])
                state["events"].setdefault(uid, [])
                mark_dirty()
            # Not acknowledged until it is on disk: a crash right after
            # would otherwise leave a PC with a token nobody remembers.
            if not flush():
                with lock:
                    u = state["users"].pop(uid, None)
                    if u:
                        token_index.pop(u["token_hash"], None)
                    state["friends"].pop(uid, None)
                    state["events"].pop(uid, None)
                return self.fail(503, "could not save the new account; try again", "save_failed")
            return self.send(200, {"id": uid, "token": token, "name": name})
        # Everything else knows who is asking before it reads anything.
        uid = self.auth()
        if not uid:
            return self.fail(401, "register first")
        body = self.read_body(n)
        if body is None:
            return self.fail(400, "bad body")
        with lock:
            r = self.route(uid, path, body)
            if r is not None and r[0] < 400:
                mark_dirty()
        if r is None:
            return self.fail(404, "not found")
        status, obj = r
        return self.send(status, obj)

    # Every POST below runs holding the lock; it returns (status, body).
    # Each one checks everything first and changes things after, so an
    # answer other than 2xx means nothing changed.
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
            inst = text(body.get("inst"), 128)
            if "/" in inst:
                return 400, {"error": "bad instance"}
            prune_invites()
            if sum(1 for inv in state["invites"].values() if inv["owner"] == uid) >= MAX_LIVE_INVITES:
                return 429, {"error": "you have too many open invites; use or wait for them", "code": "invites_full"}
            code = secrets.token_hex(4).upper()
            while code in state["invites"]:
                code = secrets.token_hex(4).upper()
            state["invites"][code] = {
                "owner": uid,
                "expires": now() + INVITE_TTL,
                # The pack a share code's follow part is for, if any.
                "inst": inst,
            }
            return 200, {"code": code, "expires": INVITE_TTL}
        if path == "/v1/join":
            return self.join(uid, body)
        if path == "/v1/remove":
            other = text(body.get("id"), 64)
            if other in state["friends"].get(uid, []):
                state["friends"][uid].remove(other)
                if uid in state["friends"].get(other, []):
                    state["friends"][other].remove(uid)
                # What they sent stays out of this PC's feed from now on.
                # (Before the group revokes below, which come from them too
                # and must still arrive.)
                q = state["events"].get(uid)
                if q:
                    q[:] = [e for e in q if e["from"] != other]
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
        ip = net_key(self.client_ip())
        prune_invites()
        # Codes are short, so guessing is slowed down: too many misses from
        # one account or one address and the door stays shut for a while.
        if (join_limit.count("u:" + uid, JOIN_FAIL_WINDOW) >= JOIN_FAILS
                or join_limit.count("i:" + ip, JOIN_FAIL_WINDOW) >= JOIN_FAILS_IP):
            return 429, {"error": "too many wrong codes; try again later", "code": "rate_limited"}
        code = text(body.get("code"), 32).strip().upper()
        inv = state["invites"].get(code)
        if not inv or inv["expires"] < now() or inv["owner"] == uid:
            join_limit.add("u:" + uid, JOIN_FAIL_WINDOW)
            join_limit.add("i:" + ip, JOIN_FAIL_WINDOW)
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
        raw_code = body.get("code")
        if isinstance(raw_code, str) and size_of(raw_code) > 20000:
            # Said out loud: a cut code would be a different, broken pack.
            return 413, {"error": "that share code is too large", "code": "too_large"}
        raw_configs = body.get("configs")
        if raw_configs is not None and (not isinstance(raw_configs, str) or size_of(raw_configs) > MAX_CONFIGS):
            # Cut, it wouldn't read back; the desktop tells the sharer to
            # share fewer mods instead.
            return 413, {"error": "too many shared settings", "code": "too_large"}
        if not text(raw_code, 20000, 20000).startswith("HOP-"):
            return 400, {"error": "not a share code"}
        to = body.get("to")
        friends = friend_ids(uid)
        if isinstance(to, str):
            # Older desktops: one friend per call.
            if to not in friends:
                return 403, {"error": "not a friend"}
            if "/" in text(body.get("inst"), 128):
                return 400, {"error": "bad instance"}
            ev = publish(to, uid, body)
            if isinstance(ev, tuple):
                return ev
            return 200, {"seq": ev["seq"], "sent": 1, "skipped": []}
        direct = id_list(to, MAX_FRIENDS)
        gids = id_list(body.get("groups"), MAX_GROUPS_PER_POST)
        fields = recipe_fields(body)
        if not fields["inst"] or "/" in fields["inst"]:
            return 400, {"error": "no instance"}
        # First work out everything and refuse if any of it can't be done;
        # only then change anything.
        k = uid + "/" + fields["inst"]
        skipped = [f for f in direct if f not in friends]
        targets = []
        for gid in gids:
            g = group_of(gid, uid)
            if g is None:
                # Left or removed since the desktop last looked: the rest
                # still goes out.
                skipped.append(gid)
                continue
            if k not in g["packs"] and len(g["packs"]) >= MAX_GROUP_PACKS:
                return 409, {"error": "the group has too many packs", "code": "group_packs_full"}
            targets.append((gid, g))
        if targets:
            nb = pack_bytes(fields)
            total, solo = pack_usage(uid)
            grew = 0
            for _, g in targets:
                d = nb - (pack_bytes(g["packs"][k]) if k in g["packs"] else 0)
                grew += max(d, 0)
                total += d
                if not any(m != uid for m in joined_ids(g)):
                    solo += d
            if grew and not event_room(grew):
                return 503, {"error": "the sync service is full; try again later", "code": "busy"}
            if total > MAX_PACK_BYTES or solo > SOLO_PACK_BYTES:
                return 409, {"error": "you share too much through groups already", "code": "storage_full"}
        reach = {}
        for f in direct:
            if f in friends:
                reach.setdefault(f, {"direct": False, "groups": []})["direct"] = True
        for gid, g in targets:
            for m in joined_ids(g):
                if m != uid:
                    reach.setdefault(m, {"direct": False, "groups": []})["groups"].append(gid)
        no = check_recipe_room(uid, reach, fields)
        if no:
            return no
        for gid, g in targets:
            old = pack_bytes(g["packs"][k]) if k in g["packs"] else 0
            _writer["added"] += max(0, pack_bytes(fields) - old)
            g["packs"][k] = dict(fields, **{"from": uid, "at": now()})
        seq = state["seq"]
        for m, how in reach.items():
            seq = make_event(m, "recipe", uid, direct=how["direct"], groups=how["groups"], **fields)["seq"]
        # Everyone in the group now has this version by their queue; anyone
        # who joins later is given the pack from the group itself (their
        # joining comes after this number).
        for _, g in targets:
            g["packs"][k]["seq"] = state["seq"]
        return 200, {"seq": seq, "sent": len(reach), "skipped": skipped}

    def revoke(self, uid, body):
        inst = text(body.get("inst"), 128)
        if not inst or "/" in inst:
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
        # A group of one is allowed (the desktop can make one to fill in
        # later). It is no free store: what anyone shares into a group counts
        # against the sharer's own limit (`storage_full`).
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
            # Check it all, then change it: a refused edit changes nothing.
            name = text(body.get("name"), 64).strip()
            friends = friend_ids(uid)
            add = [m for m in id_list(body.get("add"), MAX_GROUP_MEMBERS) if m not in g["members"]]
            if any(m not in friends for m in add):
                return 403, {"error": "not a friend"}
            if len(g["members"]) + len(add) > MAX_GROUP_MEMBERS:
                return 409, {"error": "the group is full", "code": "group_full"}
            if name:
                g["name"] = name
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
                # What the group already shares reaches the new member now:
                # not queued (a long list would push their other events
                # out) but read from the group when they ask, for every pack
                # published before this point (group_pack_events).
                state["seq"] += 1
                me["since"] = state["seq"]
                wake(uid)
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
            if "/" in inst or "/" in frm or frm not in g["members"]:
                return 404, {"error": "the group doesn't share that pack"}
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


class Server(ThreadingHTTPServer):
    """One thread per connection, but only so many at once."""

    daemon_threads = True
    request_queue_size = 128

    def __init__(self, *a, **kw):
        self.slots = threading.BoundedSemaphore(MAX_CONNECTIONS)
        super().__init__(*a, **kw)

    def process_request(self, request, client_address):
        if not self.slots.acquire(blocking=False):
            try:
                body = b'{"error":"busy","code":"busy"}'
                head = (
                    "HTTP/1.1 503 Service Unavailable\r\nRetry-After: 5\r\n"
                    "Content-Type: application/json\r\n"
                    "Content-Length: %d\r\nConnection: close\r\n\r\n" % len(body)
                )
                request.sendall(head.encode("ascii") + body)
                # Read what the client already sent before closing: closing
                # with unread data resets the connection, and the reset can
                # destroy the answer on its way.
                request.shutdown(socket.SHUT_WR)
                request.settimeout(0.2)
                request.recv(4096)
            except OSError:
                pass
            self.shutdown_request(request)
            return
        try:
            super().process_request(request, client_address)
        except BaseException:
            self.slots.release()
            raise

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            self.slots.release()

    def handle_error(self, request, client_address):
        # A peer that resets or vanishes is not news; one line, no traceback.
        e = sys.exc_info()[1]
        sys.stderr.write("%s connection error: %s\n" % (
            clean(client_address[0]), clean(type(e).__name__)))


def main():
    # Threads are small (they only parse JSON and wait); the default 8 MB of
    # address space each is what a crowd of idle connections would pay for.
    threading.stack_size(512 * 1024)
    load()
    start_writer()
    srv = Server((HOST, PORT), Handler)

    def stop(signum, frame):
        # New requests get 503 from now on, so the last save is the last word.
        _closing.set()
        # shutdown() waits for serve_forever, which this signal interrupted
        # on the main thread, so ask from another one.
        threading.Thread(target=srv.shutdown, daemon=True).start()

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    print(f"hopper-mailbox on {HOST}:{PORT}, state {STATE}", file=sys.stderr)
    try:
        srv.serve_forever()
    finally:
        srv.server_close()
        ok = stop_writer()
        print("hopper-mailbox stopped, state saved" if ok else
              "hopper-mailbox stopped, FINAL SAVE FAILED (last changes lost)", file=sys.stderr)


if __name__ == "__main__":
    main()
