"""Tests for the mailbox, against a real server on a loopback port.

Run from the repo root (standard library only):

    python -B -m unittest relay/test_hopper_mailbox.py -v

Each test gets a fresh state file in a temp folder; nothing talks to the
public relay.
"""

import io
import json
import os
import socket
import stat
import sys
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.request

sys.dont_write_bytecode = True
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import hopper_mailbox as mb  # noqa: E402


class Relay:
    """One mailbox on 127.0.0.1 with its own state file."""

    def __init__(self):
        self.dir = tempfile.TemporaryDirectory()
        self.state_path = os.path.join(self.dir.name, "state.json")
        mb.reset(self.state_path)
        self.srv = mb.Server(("127.0.0.1", 0), mb.Handler)
        self.url = "http://127.0.0.1:%d" % self.srv.server_address[1]
        self.thread = threading.Thread(target=self.srv.serve_forever, daemon=True)
        self.thread.start()

    def close(self):
        self.srv.shutdown()
        self.srv.server_close()
        mb.stop_writer()
        self.dir.cleanup()

    @property
    def port(self):
        return self.srv.server_address[1]


class Client:
    def __init__(self, relay, name):
        self.relay = relay
        self.token = None
        r = self.post("/v1/register", {"name": name})
        self.id, self.token = r["id"], r["token"]
        self.after = 0

    def call(self, method, path, body=None, headers=None):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(self.relay.url + path, data=data, method=method)
        if data is not None:
            req.add_header("Content-Type", "application/json")
        for k, v in (headers or {}).items():
            req.add_header(k, v)
        if self.token:
            req.add_header("Authorization", "Bearer " + self.token)
        try:
            with urllib.request.urlopen(req, timeout=10) as r:
                return json.loads(r.read())
        except urllib.error.HTTPError as e:
            out = json.loads(e.read())
            out["status"] = e.code
            return out

    def post(self, path, body=None, headers=None):
        return self.call("POST", path, body or {}, headers)

    def get(self, path):
        return self.call("GET", path)

    def events(self, v=2):
        """Events since the last call."""
        r = self.get("/v1/recipes?after=%d&wait=0&v=%d" % (self.after, v))
        self.after = r["seq"]
        return r["events"]

    def befriend(self, other, inst=""):
        code = self.post("/v1/invite", {"inst": inst})["code"]
        return other.post("/v1/join", {"code": code})

    def groups(self):
        return {g["id"]: g for g in self.get("/v1/groups")["groups"]}


def recipe(inst, code="HOP-x", **kw):
    return dict({"inst": inst, "key": inst.upper(), "name": "Pack " + inst, "code": code, "left_out": []}, **kw)


# Limits the tests turn down; put back after each one.
CONSTS = [
    "REGISTER_PER_HOUR", "MAX_CONNECTIONS", "MAX_PACK_BYTES", "MAX_QUEUE_BYTES", "MAX_EVENTS",
    "MAX_EVENTS_PER_SENDER", "MAX_LIVE_INVITES", "JOIN_FAILS", "MAX_FRIENDS", "SAVE_INTERVAL",
    "MAX_BODY", "MAX_POLLS_PER_USER",
]


class Base(unittest.TestCase):
    def setUp(self):
        self._saved = {n: getattr(mb, n) for n in CONSTS}
        self._timeout = mb.Handler.timeout
        # Everything here signs up from 127.0.0.1.
        mb.REGISTER_PER_HOUR = 1000
        self.relay = Relay()

    def tearDown(self):
        self.relay.close()
        for n, v in self._saved.items():
            setattr(mb, n, v)
        mb.Handler.timeout = self._timeout


class MailboxTest(Base):
    def setUp(self):
        super().setUp()
        self.a = Client(self.relay, "hopper-test-a")
        self.b = Client(self.relay, "hopper-test-b")
        self.c = Client(self.relay, "hopper-test-c")

    def test_old_publish_still_works_and_old_clients_see_only_recipes(self):
        self.a.befriend(self.b)
        r = self.a.post("/v1/recipes", dict(recipe("p1"), to=self.b.id))
        self.assertIn("seq", r)
        self.a.post("/v1/revoke", {"inst": "p1", "to": [self.b.id]})
        old = self.b.events(v=1)
        self.assertEqual([e["kind"] for e in old], ["recipe"])
        self.assertEqual(old[0]["from_name"], "hopper-test-a")
        self.b.after = 0
        self.assertEqual([e["kind"] for e in self.b.events()], ["recipe", "revoke"])
        # Not a friend: refused.
        r = self.a.post("/v1/recipes", dict(recipe("p1"), to=self.c.id))
        self.assertEqual(r["status"], 403)

    def test_revoke_reaches_the_friend_with_its_reason(self):
        self.a.befriend(self.b)
        self.a.post("/v1/recipes", dict(recipe("p1"), to=[self.b.id]))
        self.a.post("/v1/revoke", {"inst": "p1", "to": [self.b.id], "reason": "deleted"})
        evs = self.b.events()
        self.assertEqual(evs[-1]["kind"], "revoke")
        self.assertEqual((evs[-1]["inst"], evs[-1]["reason"], evs[-1]["direct"]), ("p1", "deleted", True))
        # A made-up reason becomes "unshared"; a stranger gets nothing.
        self.a.post("/v1/revoke", {"inst": "p1", "to": [self.b.id, self.c.id], "reason": "nope"})
        self.assertEqual(self.b.events()[-1]["reason"], "unshared")
        self.assertEqual(self.c.events(), [])

    def test_only_the_newest_code_of_a_pack_waits(self):
        self.a.befriend(self.b)
        for code in ("HOP-1", "HOP-2", "HOP-3"):
            self.a.post("/v1/recipes", dict(recipe("p1", code=code), to=[self.b.id]))
        self.a.post("/v1/recipes", dict(recipe("p2", code="HOP-x"), to=[self.b.id]))
        evs = self.b.events()
        self.assertEqual([(e["inst"], e["code"]) for e in evs], [("p1", "HOP-3"), ("p2", "HOP-x")])

    def test_friend_list_full_is_its_own_error(self):
        mb.MAX_FRIENDS, old = 1, mb.MAX_FRIENDS
        try:
            self.a.befriend(self.b)
            # A's list is full: A can't join C, and C can't join A.
            r = self.c.befriend(self.a)
            self.assertEqual((r["status"], r["code"]), (409, "friends_full"))
            r = self.a.befriend(self.c)
            self.assertEqual((r["status"], r["code"]), (409, "their_friends_full"))
        finally:
            mb.MAX_FRIENDS = old

    def test_friends_show_when_they_were_last_seen(self):
        self.a.befriend(self.b)
        f = self.b.get("/v1/friends")["friends"][0]
        self.assertTrue(f["online"])
        self.assertGreater(f["last_seen"], 0)

    def test_an_invite_with_a_pack_tells_the_owner_who_joined(self):
        r = self.a.befriend(self.b, inst="p1")
        self.assertEqual(r["inst"], "p1")
        evs = self.a.events()
        self.assertEqual([(e["kind"], e["from"], e["inst"]) for e in evs], [("joined", self.b.id, "p1")])
        # Single use.
        code = self.a.post("/v1/invite", {})["code"]
        self.assertNotIn("status", self.b.post("/v1/join", {"code": code}))
        self.assertEqual(self.c.post("/v1/join", {"code": code})["status"], 404)

    def make_group(self, members):
        r = self.a.post("/v1/groups", {"name": "The server crew", "members": [m.id for m in members]})
        return r["group"]["id"]

    def test_groups_are_invisible_to_outsiders(self):
        self.a.befriend(self.b)
        gid = self.make_group([self.b])
        self.assertEqual(self.c.groups(), {})
        for path, body in [
            ("/v1/groups/%s/accept" % gid, {}),
            ("/v1/groups/%s/follow" % gid, {"from": self.a.id, "inst": "p1"}),
            ("/v1/groups/%s" % gid, {"name": "mine now"}),
            ("/v1/groups/%s/in-step" % gid, {"on": True}),
        ]:
            self.assertEqual(self.c.post(path, body)["status"], 404, path)
        r = self.c.post("/v1/recipes", dict(recipe("p9"), to=[], groups=[gid]))
        self.assertEqual(r["sent"], 0)
        self.assertEqual(r["skipped"], [gid])
        # An invited member who hasn't said yes sees the group, not its packs.
        self.a.post("/v1/recipes", dict(recipe("p1"), to=[], groups=[gid]))
        g = self.b.groups()[gid]
        self.assertEqual(g["me"], {"joined": False, "in_step": False})
        self.assertEqual(g["packs"], [])
        self.assertEqual([e for e in self.b.events() if e["kind"] == "recipe"], [])
        # And can't post into it or change it.
        self.assertEqual(self.b.post("/v1/groups/%s/in-step" % gid, {"on": True})["status"], 404)
        # Only friends can be added.
        r = self.a.post("/v1/groups", {"name": "x", "members": [self.c.id]})
        self.assertEqual(r["status"], 403)

    def test_one_copy_per_friend_directly_and_through_a_group(self):
        self.a.befriend(self.b)
        self.a.befriend(self.c)
        gid = self.make_group([self.b, self.c])
        self.b.post("/v1/groups/%s/accept" % gid)
        self.c.post("/v1/groups/%s/accept" % gid)
        self.b.events()
        self.c.events()
        r = self.a.post("/v1/recipes", dict(recipe("p1"), to=[self.b.id, self.b.id], groups=[gid, gid]))
        self.assertEqual(r["sent"], 2)
        evs = [e for e in self.b.events() if e["kind"] == "recipe"]
        self.assertEqual(len(evs), 1)
        self.assertEqual((evs[0]["direct"], evs[0]["groups"], evs[0]["key"]), (True, [gid], "P1"))
        evs = [e for e in self.c.events() if e["kind"] == "recipe"]
        self.assertEqual((evs[0]["direct"], evs[0]["groups"]), (False, [gid]))
        self.assertEqual([p["inst"] for p in self.a.groups()[gid]["packs"]], ["p1"])

    def test_members_joining_and_leaving_get_and_lose_the_group_packs(self):
        self.a.befriend(self.b)
        self.a.befriend(self.c)
        gid = self.make_group([self.b])
        self.b.post("/v1/groups/%s/accept" % gid)
        self.a.post("/v1/recipes", dict(recipe("p1", code="HOP-v1"), to=[], groups=[gid]))
        self.a.post("/v1/recipes", dict(recipe("p1", code="HOP-v2"), to=[], groups=[gid]))
        # Added later: the latest version arrives as soon as they say yes.
        self.a.post("/v1/groups/%s" % gid, {"add": [self.c.id]})
        self.assertEqual([e["kind"] for e in self.c.events()], ["group"])
        self.c.post("/v1/groups/%s/accept" % gid)
        evs = self.c.events()
        self.assertEqual([(e["kind"], e["code"]) for e in evs], [("recipe", "HOP-v2")])
        self.assertEqual(evs[0]["groups"], [gid])
        # C shares into the group too, then leaves: B loses C's pack, C
        # loses A's.
        self.c.post("/v1/recipes", dict(recipe("c1"), to=[], groups=[gid]))
        self.b.events()
        self.c.post("/v1/groups/%s/leave" % gid)
        revs = [(e["from"], e["inst"], e["reason"]) for e in self.c.events() if e["kind"] == "revoke"]
        self.assertEqual(revs, [(self.a.id, "p1", "left_group")])
        revs = [(e["from"], e["inst"]) for e in self.b.events() if e["kind"] == "revoke"]
        self.assertEqual(revs, [(self.c.id, "c1")])
        self.assertNotIn(gid, self.c.groups())
        self.assertEqual([p["inst"] for p in self.b.groups()[gid]["packs"]], ["p1"])
        # The owner removes B.
        self.a.post("/v1/groups/%s" % gid, {"remove": [self.b.id]})
        self.assertEqual([e["inst"] for e in self.b.events() if e["kind"] == "revoke"], ["p1"])
        # The owner can't leave; deleting takes it back from everyone.
        self.assertEqual(self.a.post("/v1/groups/%s/leave" % gid)["code"], "owner_leaves")

    def test_unsharing_from_a_group_reaches_every_member(self):
        self.a.befriend(self.b)
        gid = self.make_group([self.b])
        self.b.post("/v1/groups/%s/accept" % gid)
        self.a.post("/v1/recipes", dict(recipe("p1"), to=[], groups=[gid]))
        self.a.post("/v1/revoke", {"inst": "p1", "groups": [gid]})
        ev = [e for e in self.b.events() if e["kind"] == "revoke"][0]
        self.assertEqual((ev["direct"], ev["groups"]), (False, [gid]))
        self.assertEqual(self.a.groups()[gid]["packs"], [])

    def test_keep_in_step_follows_on_every_opted_in_pc(self):
        self.a.befriend(self.b)
        self.a.befriend(self.c)
        d = Client(self.relay, "hopper-test-d")
        self.a.befriend(d)
        gid = self.make_group([self.b, self.c, d])
        for m in (self.b, self.c, d):
            m.post("/v1/groups/%s/accept" % gid)
        self.c.post("/v1/groups/%s/in-step" % gid, {"on": True})
        self.a.post("/v1/recipes", dict(recipe("p1"), to=[], groups=[gid]))
        for m in (self.a, self.b, self.c, d):
            m.events()
        # Only packs the group shares can be followed this way.
        self.assertEqual(self.b.post("/v1/groups/%s/follow" % gid, {"from": self.a.id, "inst": "zz"})["status"], 404)
        self.b.post("/v1/groups/%s/follow" % gid, {"from": self.a.id, "inst": "p1"})
        evs = [e for e in self.c.events() if e["kind"] == "group_follow"]
        self.assertEqual([(e["from"], e["pack_from"], e["inst"], e["group"]) for e in evs], [(self.b.id, self.a.id, "p1", gid)])
        # D didn't say yes to keeping in step; the owner shares it.
        self.assertEqual([e for e in d.events() if e["kind"] == "group_follow"], [])
        self.assertEqual([e for e in self.a.events() if e["kind"] == "group_follow"], [])
        self.assertEqual(self.c.groups()[gid]["follows"], [{"from": self.a.id, "inst": "p1"}])

    def test_unfriending_takes_them_out_of_the_owners_groups(self):
        self.a.befriend(self.b)
        gid = self.make_group([self.b])
        self.b.post("/v1/groups/%s/accept" % gid)
        self.a.post("/v1/recipes", dict(recipe("p1"), to=[self.b.id], groups=[gid]))
        self.b.events()
        self.b.post("/v1/remove", {"id": self.a.id})
        self.assertNotIn(gid, self.b.groups())
        kinds = [e["kind"] for e in self.a.events()]
        self.assertIn("unfriended", kinds)
        self.assertEqual([e["inst"] for e in self.b.events() if e["kind"] == "revoke"], ["p1"])

    def test_state_survives_a_restart(self):
        self.a.befriend(self.b)
        gid = self.make_group([self.b])
        mb.flush()
        with open(mb.STATE, encoding="utf-8") as f:
            saved = json.load(f)
        self.assertIn(gid, saved["groups"])
        # An old state file (no groups) loads with an empty list.
        with open(mb.STATE, "w", encoding="utf-8") as f:
            json.dump({k: v for k, v in saved.items() if k != "groups"}, f)
        mb.state["groups"] = {}
        mb.state["users"] = {}
        mb.load()
        self.assertEqual(mb.state["groups"], {})
        self.assertIn(self.a.id, mb.state["users"])


def raw(relay, data, wait=1.5):
    """Sends bytes on a fresh connection and reads until the server closes it
    (or `wait` seconds). Returns (status, body bytes, closed, head bytes)."""
    s = socket.create_connection(("127.0.0.1", relay.port), timeout=wait)
    try:
        s.sendall(data)
        out = b""
        closed = False
        try:
            while True:
                c = s.recv(65536)
                if not c:
                    closed = True
                    break
                out += c
        except socket.timeout:
            pass
    finally:
        s.close()
    status = int(out.split(b" ", 2)[1]) if out.startswith(b"HTTP/") else 0
    head, _, body = out.partition(b"\r\n\r\n")
    return status, body, closed, head


def req(path, method="POST", headers=(), body=b"", token=None, cl=True):
    lines = ["%s %s HTTP/1.1" % (method, path), "Host: x"]
    if token:
        lines.append("Authorization: Bearer " + token)
    if cl and body is not None:
        lines.append("Content-Length: %d" % len(body))
    lines += list(headers)
    return ("\r\n".join(lines) + "\r\n\r\n").encode("latin-1") + (body or b"")


class HardeningTest(Base):
    def setUp(self):
        super().setUp()
        self.a = Client(self.relay, "hopper-test-a")
        self.b = Client(self.relay, "hopper-test-b")
        self.c = Client(self.relay, "hopper-test-c")

    def make_group(self, owner, members):
        r = owner.post("/v1/groups", {"name": "Crew", "members": [m.id for m in members]})
        return r["group"]["id"]

    # -- H1: size, sign-ups, saving

    def test_code_and_left_out_are_cut_by_bytes(self):
        self.a.befriend(self.b)
        # 8000 characters but 24000 bytes: cut at 20000 bytes. (JSON writes
        # each as a 6-byte escape, so this just fits the 64 KB body limit.)
        code = "HOP-" + "€" * 8000
        left = ["€" * 150] * 10
        self.a.post("/v1/recipes", dict(recipe("p1", code=code, left_out=left), to=[self.b.id], groups=[]))
        e = self.b.events()[0]
        self.assertLessEqual(len(e["code"].encode()), 20000)
        self.assertEqual(len(e["left_out"]), 10)
        self.assertTrue(all(len(x.encode()) <= 400 for x in e["left_out"]))

    def test_a_group_is_no_free_store(self):
        mb.MAX_PACK_BYTES = 50000
        solo = self.make_group(self.a, [])
        big = "HOP-" + "x" * 19000
        ok = [self.a.post("/v1/recipes", dict(recipe("p%d" % i, code=big), to=[], groups=[solo])) for i in range(2)]
        self.assertTrue(all("seq" in r for r in ok))
        r = self.a.post("/v1/recipes", dict(recipe("p9", code=big), to=[], groups=[solo]))
        self.assertEqual((r["status"], r["code"]), (409, "storage_full"))
        # Replacing a pack doesn't count twice, and a refusal stored nothing.
        r = self.a.post("/v1/recipes", dict(recipe("p0", code=big), to=[], groups=[solo]))
        self.assertIn("seq", r)
        self.assertEqual(sorted(p["inst"] for p in self.a.groups()[solo]["packs"]), ["p0", "p1"])
        # Someone else's allowance is their own.
        g2 = self.make_group(self.b, [])
        self.assertIn("seq", self.b.post("/v1/recipes", dict(recipe("q", code=big), to=[], groups=[g2])))

    def test_a_queue_stops_at_its_byte_cap_and_keeps_recipes(self):
        mb.MAX_QUEUE_BYTES = 30000
        self.a.befriend(self.b)
        self.a.post("/v1/recipes", dict(recipe("keep", code="HOP-k"), to=[self.b.id]))
        with mb.lock:
            for i in range(50):
                mb.make_event(self.b.id, "group", "s%d" % i, group="g" * 1000)
        kinds = [e["kind"] for e in self.b.events()]
        self.assertIn("recipe", kinds)
        self.assertLess(len(kinds), 50)

    def test_register_is_limited_per_client_ip(self):
        mb.REGISTER_PER_HOUR = 3
        mb.register_limit.clear()

        def reg(ip=None):
            hs = ["CF-Connecting-IP: " + ip] if ip else []
            return raw(self.relay, req("/v1/register", headers=hs, body=b'{"name":"x"}'))

        self.assertEqual([reg("203.0.113.7")[0] for _ in range(3)], [200, 200, 200])
        status, body, _, head = reg("203.0.113.7")
        self.assertEqual(status, 429)
        self.assertEqual(json.loads(body)["code"], "rate_limited")
        self.assertIn(b"Retry-After", head)
        # Another address is unaffected; with no header the peer is counted.
        self.assertEqual(reg("203.0.113.8")[0], 200)
        self.assertEqual([reg()[0] for _ in range(4)], [200, 200, 200, 429])
        # IPv6 is counted by /64.
        self.assertEqual(mb.net_key("2001:db8::1:2:3:4"), mb.net_key("2001:db8::ffff:0:0:1"))

    def test_saving_is_off_the_request_path(self):
        self.assertFalse(os.path.exists(mb.STATE))
        mb.SAVE_INTERVAL = 0.1
        mb.start_writer()
        Client(self.relay, "late")
        deadline = time.time() + 5
        while not os.path.exists(mb.STATE) and time.time() < deadline:
            time.sleep(0.05)
        self.assertTrue(os.path.exists(mb.STATE))
        self.assertFalse(os.path.exists(mb.STATE + ".tmp"))

    def test_requests_alone_do_not_write_the_file(self):
        self.a.befriend(self.b)
        self.a.post("/v1/profile", {"name": "z"})
        self.assertFalse(os.path.exists(mb.STATE))

    def test_stopping_flushes_what_is_unsaved(self):
        mb.SAVE_INTERVAL = 60
        mb.start_writer()
        me = Client(self.relay, "unsaved")
        self.assertFalse(os.path.exists(mb.STATE))
        mb.stop_writer()   # what SIGTERM does
        with open(mb.STATE, encoding="utf-8") as f:
            self.assertIn(me.id, json.load(f)["users"])

    @unittest.skipUnless(threading.current_thread() is threading.main_thread(), "signals need the main thread")
    def test_sigterm_stops_the_server_and_saves(self):
        import signal
        mb.SAVE_INTERVAL = 60
        mb.HOST, mb.PORT = "127.0.0.1", 0
        old = {n: signal.getsignal(n) for n in (signal.SIGTERM, signal.SIGINT)}
        with mb.lock:
            mb.state["seq"] = 41
            mb.mark_dirty()
        threading.Timer(1.0, lambda: signal.raise_signal(signal.SIGTERM)).start()
        err = io.StringIO()
        old_err, sys.stderr = sys.stderr, err
        try:
            mb.main()
        finally:
            sys.stderr = old_err
            for n, h in old.items():
                signal.signal(n, h)
        self.assertIn("state saved", err.getvalue())
        with open(mb.STATE, encoding="utf-8") as f:
            self.assertEqual(json.load(f)["seq"], 41)

    def test_a_failed_save_is_kept_and_retried(self):
        Client(self.relay, "x")
        good = mb.STATE
        blocker = os.path.join(self.relay.dir.name, "blocker")
        open(blocker, "w").close()   # a file where a folder must be
        mb.STATE = os.path.join(blocker, "state.json")
        err = io.StringIO()
        old, sys.stderr = sys.stderr, err
        try:
            self.assertFalse(mb.flush())
        finally:
            sys.stderr = old
        mb.STATE = good
        self.assertTrue(mb.flush())
        self.assertTrue(os.path.exists(good))

    # -- H2: nothing is sent while the lock is held; wakes are per person

    def test_responses_are_sent_after_the_lock_is_let_go(self):
        held = []
        orig = mb.Handler.send_json

        def spy(self_, status, obj, log=True, raw=None):
            held.append((self_.path.split("?")[0], mb.lock._is_owned()))
            return orig(self_, status, obj, log, raw)

        mb.Handler.send_json = spy
        try:
            self.a.befriend(self.b)
            self.make_group(self.a, [self.b])
            self.a.get("/v1/friends")
            self.a.get("/v1/groups")
            self.a.get("/v1/recipes?after=0&wait=0&v=2")
            self.a.post("/v1/profile", {"name": "z"})
        finally:
            mb.Handler.send_json = orig
        self.assertGreaterEqual(len(held), 4)
        self.assertEqual([p for p, owned in held if owned], [])

    def test_a_publish_wakes_only_its_recipient(self):
        self.a.befriend(self.b)
        self.a.befriend(self.c)
        got = {}

        def poll(who):
            t = time.time()
            r = who.get("/v1/recipes?after=0&wait=4&v=2")
            got[who.id] = (time.time() - t, r["events"])

        ts = [threading.Thread(target=poll, args=(w,)) for w in (self.b, self.c)]
        for t in ts:
            t.start()
        time.sleep(0.5)
        with mb.lock:
            self.assertEqual(set(mb.wakers), {self.b.id, self.c.id})
        self.a.post("/v1/recipes", dict(recipe("p1"), to=[self.b.id]))
        ts[0].join(3)
        self.assertLess(got[self.b.id][0], 2.5)
        self.assertEqual(len(got[self.b.id][1]), 1)
        self.assertNotIn(self.c.id, got)      # C is still waiting
        ts[1].join(6)
        self.assertEqual(got[self.c.id][1], [])
        with mb.lock:
            self.assertEqual((mb.wakers, mb.polls), ({}, {}))

    # -- H3: timeouts and caps

    def test_a_stalled_body_is_cut_off(self):
        mb.Handler.timeout = 0.6
        data = req("/v1/register", body=b"").replace(b"Content-Length: 0", b"Content-Length: 100") + b'{"na'
        t = time.time()
        _, _, closed, _ = raw(self.relay, data, wait=5)
        self.assertTrue(closed)
        self.assertLess(time.time() - t, 4)

    def test_idle_connections_are_dropped(self):
        mb.Handler.timeout = 0.5
        s = socket.create_connection(("127.0.0.1", self.relay.port), timeout=4)
        try:
            self.assertEqual(s.recv(10), b"")
        finally:
            s.close()

    def test_connections_are_capped(self):
        mb.MAX_CONNECTIONS = 2
        relay = Relay()
        held = []
        try:
            for _ in range(2):
                held.append(socket.create_connection(("127.0.0.1", relay.port), timeout=3))
            time.sleep(0.3)
            status, _, closed, _ = raw(relay, req("/v1/health", "GET", cl=False))
            self.assertEqual(status, 503)
            self.assertTrue(closed)
            held.pop().close()
            time.sleep(0.5)
            self.assertEqual(raw(relay, req("/v1/health", "GET", cl=False), wait=0.5)[0], 200)
        finally:
            for s in held:
                s.close()
            relay.close()

    def test_an_account_holds_three_long_polls(self):
        mb.MAX_POLLS_PER_USER = 3
        done = []

        def poll(i):
            t = time.time()
            self.a.get("/v1/recipes?after=0&wait=6&v=2")
            done.append((i, time.time() - t))

        ts = []
        for i in range(4):
            t = threading.Thread(target=poll, args=(i,))
            t.start()
            ts.append(t)
            time.sleep(0.25)
        time.sleep(0.5)
        # The fourth pushed the first out; the other three still wait.
        self.assertEqual([d[0] for d in done], [0])
        with mb.lock:
            self.assertEqual(len(mb.polls[self.a.id]), 3)
        self.b.befriend(self.a)
        self.b.post("/v1/recipes", dict(recipe("p"), to=[self.a.id]))
        for t in ts:
            t.join(8)
        self.assertEqual(len(done), 4)

    # -- M1: framing

    def test_ambiguous_framing_is_refused_and_closes(self):
        smuggled = b"{}0\r\n\r\n" + req("/v1/health", "GET", cl=False)
        ok = req("/v1/register", body=b"{}")
        cases = {
            "chunked": req("/v1/register", headers=["Transfer-Encoding: chunked"], body=smuggled),
            "two lengths": req("/v1/register", headers=["Content-Length: 2"], body=b"{}"),
            "plus sign": ok.replace(b"Content-Length: 2", b"Content-Length: +2"),
            "hex": ok.replace(b"Content-Length: 2", b"Content-Length: 0x2"),
            "arabic digit": ok.replace(b"Content-Length: 2", "Content-Length: ٢".encode("utf-8")),
            "negative": ok.replace(b"Content-Length: 2", b"Content-Length: -1"),
        }
        for name, data in cases.items():
            status, body, closed, _ = raw(self.relay, data)
            self.assertEqual(status, 400, name)
            self.assertTrue(closed, name)
            # One answer only: the smuggled second request never ran.
            self.assertNotIn(b"HTTP/1.1", body, name)
        with mb.lock:
            self.assertEqual(len(mb.state["users"]), 3)    # just a, b and c

    def test_a_body_over_the_limit_gets_an_answer_and_a_close(self):
        data = req("/v1/register", body=b"").replace(
            b"Content-Length: 0", b"Content-Length: %d" % (mb.MAX_BODY + 1))
        status, _, closed, _ = raw(self.relay, data)
        self.assertEqual(status, 413)
        self.assertTrue(closed)

    def test_errors_before_the_body_is_read_close_the_connection(self):
        # Wrong token with a body: 401, then closed, so the body can't pass
        # for a second request.
        second = req("/v1/health", "GET", cl=False)
        status, _, closed, _ = raw(self.relay, req("/v1/profile", token="x" * 30, body=second))
        self.assertEqual(status, 401)
        self.assertTrue(closed)
        # A GET that carries a body is answered but not kept alive.
        status, _, closed, _ = raw(self.relay, req("/v1/health", "GET", body=b"hello"))
        self.assertEqual(status, 200)
        self.assertTrue(closed)
        # A plain keep-alive request stays open.
        status, _, closed, _ = raw(self.relay, req("/v1/health", "GET", cl=False), wait=0.5)
        self.assertEqual(status, 200)
        self.assertFalse(closed)

    def test_the_token_is_checked_before_the_body_is_read(self):
        # Announces 50 bytes, sends none: a bad token is answered at once.
        data = req("/v1/profile", token="y" * 30, body=b"").replace(b"Content-Length: 0", b"Content-Length: 50")
        t = time.time()
        status, _, _, _ = raw(self.relay, data, wait=2)
        self.assertEqual(status, 401)
        self.assertLess(time.time() - t, 1.5)

    # -- M2: invite codes

    def test_wrong_codes_lock_the_guesser_out(self):
        mb.JOIN_FAILS = 3
        for _ in range(3):
            self.assertEqual(self.b.post("/v1/join", {"code": "00000000"})["status"], 404)
        r = self.b.post("/v1/join", {"code": "00000000"})
        self.assertEqual((r["status"], r["code"]), (429, "rate_limited"))
        # Even a right code is refused for that account while it lasts ...
        code = self.a.post("/v1/invite")["code"]
        self.assertEqual(self.b.post("/v1/join", {"code": code})["status"], 429)
        # ... but not for someone else on another address.
        self.assertNotIn("status", self.c.post("/v1/join", {"code": code}, {"CF-Connecting-IP": "192.0.2.1"}))

    def test_wrong_codes_are_counted_per_address_too(self):
        mb.JOIN_FAILS = 3
        h = {"CF-Connecting-IP": "198.51.100.9"}
        for who in (self.a, self.b, self.c):
            self.assertEqual(who.post("/v1/join", {"code": "00000000"}, h)["status"], 404)
        d = Client(self.relay, "hopper-test-d")
        self.assertEqual(d.post("/v1/join", {"code": "00000000"}, h)["status"], 429)
        self.assertEqual(d.post("/v1/join", {"code": "00000000"})["status"], 404)

    def test_few_live_invites_and_expired_ones_are_swept(self):
        mb.MAX_LIVE_INVITES = 3
        codes = [self.a.post("/v1/invite")["code"] for _ in range(3)]
        r = self.a.post("/v1/invite")
        self.assertEqual((r["status"], r["code"]), (429, "invites_full"))
        self.assertNotIn("status", self.b.post("/v1/invite"))     # their own allowance
        with mb.lock:
            mb.state["invites"][codes[0]]["expires"] = mb.now() - 1
        self.assertNotIn("status", self.a.post("/v1/invite"))      # one ran out, room again
        with mb.lock:
            mb.state["invites"][codes[1]]["expires"] = mb.now() - 1
        self.c.post("/v1/join", {"code": "FFFFFFFF"})                # any join sweeps
        with mb.lock:
            self.assertNotIn(codes[1], mb.state["invites"])

    def test_the_invite_code_keeps_the_shape_the_client_parses(self):
        code = self.a.post("/v1/invite")["code"]
        self.assertRegex(code, r"^[0-9A-F]{8}$")

    # -- M3: nothing half done

    def test_a_refused_group_edit_changes_nothing(self):
        self.a.befriend(self.b)
        gid = self.make_group(self.a, [self.b])
        r = self.a.post("/v1/groups/%s" % gid, {"name": "Renamed", "add": [self.c.id]})   # not a friend
        self.assertEqual(r["status"], 403)
        self.assertEqual(self.a.groups()[gid]["name"], "Crew")
        self.a.befriend(self.c)
        mb.MAX_GROUP_MEMBERS = 2
        try:
            r = self.a.post("/v1/groups/%s" % gid, {"name": "Renamed", "add": [self.c.id]})
        finally:
            mb.MAX_GROUP_MEMBERS = 50
        self.assertEqual((r["status"], r["code"]), (409, "group_full"))
        self.assertEqual(self.a.groups()[gid]["name"], "Crew")
        self.assertEqual(len(self.a.groups()[gid]["members"]), 2)

    def test_a_refused_publish_to_several_groups_stores_nothing(self):
        self.a.befriend(self.b)
        g1 = self.make_group(self.a, [self.b])
        g2 = self.make_group(self.a, [self.b])
        mb.MAX_GROUP_PACKS = 1
        try:
            self.a.post("/v1/recipes", dict(recipe("old"), to=[], groups=[g2]))
            r = self.a.post("/v1/recipes", dict(recipe("new"), to=[self.b.id], groups=[g1, g2]))
        finally:
            mb.MAX_GROUP_PACKS = 100
        self.assertEqual((r["status"], r["code"]), (409, "group_packs_full"))
        self.assertEqual(self.a.groups()[g1]["packs"], [])
        self.assertEqual([e for e in self.b.events() if e["kind"] == "recipe"], [])

    # -- M4: queue flooding

    def test_group_events_collapse_to_one_per_group_and_sender(self):
        self.a.befriend(self.b)
        gid = self.make_group(self.a, [self.b])
        for i in range(6):
            self.a.post("/v1/groups/%s" % gid, {"name": "Crew %d" % i})
        self.assertEqual([e["kind"] for e in self.b.events()], ["group"])
        self.b.post("/v1/groups/%s/accept" % gid)
        self.b.events()
        for i in range(5):
            self.a.post("/v1/groups/%s/in-step" % gid, {"on": bool(i % 2)})
        self.assertEqual([e["kind"] for e in self.b.events()], ["group"])

    def test_one_sender_cannot_fill_anothers_queue(self):
        mb.MAX_EVENTS_PER_SENDER = 5
        self.a.befriend(self.b)
        for i in range(30):
            self.a.post("/v1/recipes", dict(recipe("p%d" % i), to=[self.b.id]))
        self.c.befriend(self.b)
        self.c.post("/v1/recipes", dict(recipe("c1"), to=[self.b.id]))
        evs = self.b.events()
        self.assertEqual(sum(1 for e in evs if e["from"] == self.a.id), 5)
        self.assertEqual(sum(1 for e in evs if e["from"] == self.c.id), 1)

    def test_a_full_queue_drops_noise_before_recipes_and_revokes(self):
        mb.MAX_EVENTS = 10
        self.a.befriend(self.b)
        self.a.post("/v1/recipes", dict(recipe("keep"), to=[self.b.id]))
        self.a.post("/v1/revoke", {"inst": "gone", "to": [self.b.id]})
        with mb.lock:
            for i in range(60):
                mb.make_event(self.b.id, "group", "sender%d" % i, group="g%d" % i)
        evs = self.b.events()
        self.assertEqual(len(evs), 10)
        self.assertEqual([e["kind"] for e in evs if e["kind"] != "group"], ["recipe", "revoke"])
        # With nothing else to drop, the oldest recipe or revoke goes.
        with mb.lock:
            for i in range(30):
                mb.make_event(self.b.id, "recipe", "r%d" % i, inst="i", code="HOP-")
        self.b.after = 0
        self.assertEqual(len(self.b.events()), 10)

    # -- M5: the state file

    def test_a_corrupt_state_file_is_moved_aside_and_the_relay_stops(self):
        with open(mb.STATE, "w", encoding="utf-8") as f:
            f.write('{"users": {"a": ')
        err = io.StringIO()
        old, sys.stderr = sys.stderr, err
        try:
            with self.assertRaises(SystemExit) as cm:
                mb.load()
        finally:
            sys.stderr = old
        self.assertNotEqual(cm.exception.code, 0)
        self.assertFalse(os.path.exists(mb.STATE))
        moved = [n for n in os.listdir(self.relay.dir.name) if n.startswith("state.json.corrupt-")]
        self.assertEqual(len(moved), 1)
        with open(os.path.join(self.relay.dir.name, moved[0]), encoding="utf-8") as f:
            self.assertEqual(f.read(), '{"users": {"a": ')
        # Valid JSON of the wrong shape is just as bad; a missing file is fine.
        with open(mb.STATE, "w", encoding="utf-8") as f:
            f.write("[1, 2]")
        old, sys.stderr = sys.stderr, io.StringIO()
        try:
            with self.assertRaises(SystemExit):
                mb.load()
        finally:
            sys.stderr = old
        mb.load()

    # -- M6: who sees whom

    def test_an_invitation_shows_only_the_name_and_the_owner(self):
        self.a.befriend(self.b)
        self.a.befriend(self.c)
        gid = self.make_group(self.a, [self.b, self.c])
        g = self.b.groups()[gid]
        self.assertEqual((g["name"], g["owner"]), ("Crew", self.a.id))
        self.assertEqual([m["id"] for m in g["members"]], [self.a.id])
        self.assertEqual((g["packs"], g["follows"]), ([], []))
        self.assertFalse(g["me"]["joined"])

    def test_only_friends_show_presence_and_minecraft_ids(self):
        self.a.befriend(self.b)
        self.a.befriend(self.c)
        self.a.post("/v1/profile", {"name": "hopper-test-a", "mc_id": "a" * 32})
        self.c.post("/v1/profile", {"name": "hopper-test-c", "mc_id": "c" * 32})
        gid = self.make_group(self.a, [self.b, self.c])
        self.b.post("/v1/groups/%s/accept" % gid)
        self.c.post("/v1/groups/%s/accept" % gid)
        members = {m["id"]: m for m in self.b.groups()[gid]["members"]}
        self.assertEqual(set(members), {self.a.id, self.b.id, self.c.id})
        # B and C aren't friends: C is a name and "joined", nothing more.
        c = members[self.c.id]
        self.assertEqual((c["name"], c["joined"]), ("hopper-test-c", True))
        self.assertEqual((c["mc_id"], c["online"], c["last_seen"]), ("", False, 0))
        # A is B's friend, and B sees themself.
        self.assertEqual(members[self.a.id]["mc_id"], "a" * 32)
        self.assertTrue(members[self.a.id]["online"])
        self.assertGreater(members[self.b.id]["last_seen"], 0)

    # -- M7: tokens and the state file

    def test_tokens_are_stored_as_hashes_and_the_file_is_private(self):
        mb.flush()
        with open(mb.STATE, encoding="utf-8") as f:
            text_ = f.read()
        self.assertNotIn(self.a.token, text_)
        u = json.loads(text_)["users"][self.a.id]
        self.assertNotIn("token", u)
        self.assertEqual(u["token_hash"], mb.hash_token(self.a.token))
        if os.name == "posix":
            self.assertEqual(stat.S_IMODE(os.stat(mb.STATE).st_mode), 0o600)

    def test_an_old_state_file_with_plain_tokens_still_logs_in(self):
        old = {
            "users": {"u1": {"name": "Old PC", "token": "T" * 43, "created": 1, "last_seen": 1, "mc_id": ""}},
            "friends": {"u1": []}, "invites": {}, "events": {"u1": []}, "groups": {}, "seq": 0,
        }
        with open(mb.STATE, "w", encoding="utf-8") as f:
            json.dump(old, f)
        mb.reset()
        mb.load()
        pc = Client.__new__(Client)
        pc.relay, pc.token, pc.id, pc.after = self.relay, "T" * 43, "u1", 0
        r = pc.get("/v1/friends")
        self.assertEqual(r["friends"], [])
        pc.token = "U" * 43
        self.assertEqual(pc.get("/v1/friends")["status"], 401)
        # The migration marks the state changed, so the next save has only the hash.
        mb.flush()
        with open(mb.STATE, encoding="utf-8") as f:
            text_ = f.read()
        self.assertNotIn("T" * 43, text_)
        self.assertEqual(json.loads(text_)["users"]["u1"]["token_hash"], mb.hash_token("T" * 43))
        # A second start finds nothing left to migrate and still works.
        mb.reset()
        mb.load()
        self.assertEqual(len(mb.token_index), 1)
        pc.token = "T" * 43
        self.assertEqual(pc.get("/v1/friends")["friends"], [])

    # -- M8: odd tokens, and any crash is a JSON 500

    def test_a_non_ascii_token_is_just_wrong(self):
        for method in ("GET", "POST"):
            post = method == "POST"
            data = req("/v1/friends", method, token="é" * 30, body=b"" if post else None, cl=post)
            status, body, _, _ = raw(self.relay, data)
            self.assertEqual(status, 401, method)
            self.assertIn("error", json.loads(body))
        self.assertEqual(self.a.get("/v1/friends")["friends"], [])

    def test_a_crash_inside_a_request_is_a_json_500(self):
        orig = mb.Handler.route

        def boom(self_, uid, path, body):
            raise RuntimeError("boom")

        mb.Handler.route = boom
        err = io.StringIO()
        old, sys.stderr = sys.stderr, err
        try:
            r = self.a.post("/v1/profile", {"name": "x"})
        finally:
            sys.stderr = old
            mb.Handler.route = orig
        self.assertEqual((r["status"], r["code"]), (500, "internal"))
        self.assertEqual(err.getvalue().count("internal error: RuntimeError: boom"), 1)
        self.assertEqual(self.a.get("/v1/friends")["friends"], [])

    # -- low findings

    def test_left_out_that_is_not_a_list_is_empty(self):
        self.a.befriend(self.b)
        for bad in ("abc", {"x": 1}, 7, None):
            self.a.post("/v1/recipes", dict(recipe("p1"), left_out=bad, to=[self.b.id]))
            self.assertEqual(self.b.events()[-1]["left_out"], [])
        # The one-friend form too.
        self.a.post("/v1/recipes", dict(recipe("p2"), left_out="abc", to=self.b.id))
        self.assertEqual(self.b.events()[-1]["left_out"], [])

    def test_slashes_in_pack_ids_are_refused_and_follows_need_a_member(self):
        self.a.befriend(self.b)
        gid = self.make_group(self.a, [self.b])
        self.b.post("/v1/groups/%s/accept" % gid)
        self.assertEqual(self.a.post("/v1/recipes", dict(recipe("a/b"), to=[self.b.id]))["status"], 400)
        self.assertEqual(self.a.post("/v1/revoke", {"inst": "a/b", "to": [self.b.id]})["status"], 400)
        self.assertEqual(self.a.post("/v1/invite", {"inst": "a/b"})["status"], 400)
        self.a.post("/v1/recipes", dict(recipe("p1"), to=[], groups=[gid]))
        ok = self.b.post("/v1/groups/%s/follow" % gid, {"from": self.a.id, "inst": "p1"})
        self.assertNotIn("status", ok)
        # A forged "from" that joins up to a real key, a non-member, a slash.
        for frm, inst in ((self.a.id + "/p1", ""), (self.c.id, "p1"), (self.a.id, "p1/")):
            r = self.b.post("/v1/groups/%s/follow" % gid, {"from": frm, "inst": inst})
            self.assertEqual(r["status"], 404, (frm, inst))

    def test_removing_a_friend_purges_what_they_queued_for_you(self):
        self.a.befriend(self.b)
        gid = self.make_group(self.a, [self.b])
        self.b.post("/v1/groups/%s/accept" % gid)
        self.a.post("/v1/recipes", dict(recipe("direct"), to=[self.b.id]))
        self.a.post("/v1/recipes", dict(recipe("viagroup"), to=[], groups=[gid]))
        self.c.befriend(self.b)
        self.c.post("/v1/recipes", dict(recipe("from-c"), to=[self.b.id]))
        self.b.post("/v1/remove", {"id": self.a.id})
        evs = self.b.events()
        self.assertEqual([e["inst"] for e in evs if e["kind"] == "recipe"], ["from-c"])
        # The group revokes (from them) still arrive: the desktop needs them.
        self.assertEqual([e["inst"] for e in evs if e["kind"] == "revoke"], ["viagroup"])

    def test_log_lines_carry_the_real_address_and_no_control_characters(self):
        self.assertEqual(mb.clean("a\nb\x1b[31m\r\té"), "a\\x0ab\\x1b[31m\\x0d\\x09\\xe9")
        err = io.StringIO()
        old, sys.stderr = sys.stderr, err
        try:
            raw(self.relay, req("/v1/health", "GET", headers=["CF-Connecting-IP: 203.0.113.5"], cl=False), wait=0.3)
            raw(self.relay, req("/v1/health", "GET", headers=["CF-Connecting-IP: bad\x01ip"], cl=False), wait=0.3)
        finally:
            sys.stderr = old
        out = err.getvalue()
        self.assertIn("203.0.113.5 ", out)
        self.assertNotRegex(out, r"[\x00-\x09\x0b-\x1f]")

    def test_the_banner_does_not_name_python(self):
        _, _, _, head = raw(self.relay, req("/v1/health", "GET", cl=False), wait=0.3)
        self.assertIn(b"Server: hopper-mailbox", head)
        self.assertNotIn(b"Python", head)
        # The library's own error pages are JSON and say the same.
        status, body, _, head = raw(self.relay, b"PUT /v1/health HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n")
        self.assertEqual(status, 501)
        self.assertIn("error", json.loads(body))
        self.assertNotIn(b"Python", head)


if __name__ == "__main__":
    unittest.main()
