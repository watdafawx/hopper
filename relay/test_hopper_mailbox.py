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
    "MAX_BODY", "MAX_POLLS_PER_USER", "SOLO_PACK_BYTES", "JOIN_FAILS_IP", "MAX_STATE_BYTES",
    "MAX_USERS", "SENDER_QUOTA_BYTES", "EVENT_BUDGET_FRACTION", "LOAD_LIMIT_FRACTION", "REQUEST_DEADLINE", "MAX_WAIT", "EVENT_TTL", "KEEP_TTL", "IDLE_PRUNE_SECS",
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
        # 8000 characters but 24000 bytes: refused outright, not cut (a cut
        # code would be a different, broken pack). JSON writes each as a
        # 6-byte escape, so this just fits the 64 KB body limit.
        code = "HOP-" + "€" * 8000
        r = self.a.post("/v1/recipes", dict(recipe("p1", code=code), to=[self.b.id], groups=[]))
        self.assertEqual((r["status"], r["code"]), (413, "too_large"))
        self.assertEqual(self.b.events(), [])
        left = ["€" * 150] * 10
        self.a.post("/v1/recipes", dict(recipe("p1", left_out=left), to=[self.b.id], groups=[]))
        e = self.b.events()[0]
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

    def saved_text(self):
        with open(mb.STATE, encoding="utf-8") as f:
            return f.read()

    def test_saving_is_off_the_request_path(self):
        before = self.saved_text()
        mb.SAVE_INTERVAL = 0.1
        mb.start_writer()
        self.a.post("/v1/profile", {"name": "renamed-late"})
        self.assertEqual(self.saved_text(), before)   # the request did not write
        deadline = time.time() + 5
        while "renamed-late" not in self.saved_text() and time.time() < deadline:
            time.sleep(0.05)
        self.assertIn("renamed-late", self.saved_text())
        self.assertFalse(os.path.exists(mb.STATE + ".tmp"))

    def test_requests_alone_do_not_write_the_file(self):
        before = self.saved_text()
        self.a.befriend(self.b)
        self.a.post("/v1/profile", {"name": "z"})
        self.assertEqual(self.saved_text(), before)

    def test_stopping_flushes_what_is_unsaved(self):
        mb.SAVE_INTERVAL = 60
        mb.start_writer()
        self.a.post("/v1/profile", {"name": "unsaved-name"})
        self.assertNotIn("unsaved-name", self.saved_text())
        self.assertTrue(mb.stop_writer())   # what SIGTERM does
        self.assertIn("unsaved-name", self.saved_text())

    @unittest.skipUnless(threading.current_thread() is threading.main_thread(), "signals need the main thread")
    def test_sigterm_stops_the_server_and_saves(self):
        import signal
        mb.SAVE_INTERVAL = 60
        mb.HOST, mb.PORT = "127.0.0.1", 0
        old = {n: signal.getsignal(n) for n in (signal.SIGTERM, signal.SIGINT)}
        def change_then_term():
            # main() loads the file first, so the change comes after that.
            with mb.lock:
                mb.state["seq"] = 41
                mb.mark_dirty()
            signal.raise_signal(signal.SIGTERM)
        threading.Timer(1.0, change_then_term).start()
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
        with mb.lock:
            mb.mark_dirty()
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
        mb.JOIN_FAILS_IP = 3
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
            "users": {"u1": {"name": "Old PC", "token": "T" * 43, "created": 1, "last_seen": time.time(), "mc_id": ""}},
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


class ResilienceTest(Base):
    """The second round: running out of room, restarts, and slow or odd peers."""

    def setUp(self):
        super().setUp()
        self.a = Client(self.relay, "hopper-test-a")
        self.b = Client(self.relay, "hopper-test-b")
        self.c = Client(self.relay, "hopper-test-c")

    def make_group(self, owner, members):
        return owner.post("/v1/groups", {"name": "Crew", "members": [m.id for m in members]})["group"]["id"]

    def reg(self, ip):
        return raw(self.relay, req("/v1/register", headers=["CF-Connecting-IP: " + ip], body=b'{"name":"x"}'))

    def saved(self):
        with open(mb.STATE, encoding="utf-8") as f:
            return json.load(f)

    # -- F1: the service has a size

    def test_new_sign_ups_are_refused_when_the_service_is_full(self):
        mb.MAX_USERS = 3
        status, body, _, head = self.reg("203.0.113.1")
        self.assertEqual((status, json.loads(body)["code"]), (503, "busy"))
        self.assertIn(b"Retry-After", head)
        # Existing accounts carry on.
        self.assertEqual(self.a.get("/v1/friends")["friends"], [])
        mb.MAX_USERS = 100
        mb.MAX_STATE_BYTES = 10
        mb.flush()
        self.assertEqual(self.reg("203.0.113.2")[0], 503)

    def test_idle_accounts_with_nobody_are_forgotten_but_not_others(self):
        self.a.befriend(self.b)
        gid = self.make_group(self.b, [])
        long_ago = mb.now() - mb.IDLE_PRUNE_SECS - 100
        with mb.lock:
            for u in (self.a, self.b, self.c):
                mb.state["users"][u.id]["last_seen"] = long_ago
        d = Client(self.relay, "hopper-test-d")     # in a group only
        with mb.lock:
            mb.state["groups"][gid]["members"][d.id] = {"joined": True}
            mb.state["users"][d.id]["last_seen"] = long_ago
        self.assertEqual(mb.prune_accounts(), 1)    # only C: no friends, no group
        self.assertEqual(self.a.get("/v1/friends")["status"] if "status" in self.a.get("/v1/friends") else 200, 200)
        self.assertEqual(self.c.get("/v1/friends")["status"], 401)
        self.assertNotIn("status", self.d_get(d))

    def d_get(self, d):
        return d.get("/v1/friends")

    def test_a_group_with_nobody_else_holds_little(self):
        mb.SOLO_PACK_BYTES = 30000
        big = "HOP-" + "x" * 19000
        solo = self.make_group(self.a, [])
        self.assertIn("seq", self.a.post("/v1/recipes", dict(recipe("p0", code=big), to=[], groups=[solo])))
        r = self.a.post("/v1/recipes", dict(recipe("p1", code=big), to=[], groups=[solo]))
        self.assertEqual((r["status"], r["code"]), (409, "storage_full"))
        # With a friend in it the bigger allowance applies.
        self.a.befriend(self.b)
        crew = self.make_group(self.a, [self.b])
        self.b.post("/v1/groups/%s/accept" % crew)
        self.assertIn("seq", self.a.post("/v1/recipes", dict(recipe("p1", code=big), to=[], groups=[crew])))

    def test_group_packs_stop_at_the_state_cap(self):
        gid = self.make_group(self.a, [])
        mb.MAX_STATE_BYTES = 1
        mb.flush()
        r = self.a.post("/v1/recipes", dict(recipe("p0"), to=[], groups=[gid]))
        self.assertEqual((r["status"], r["code"]), (503, "busy"))

    def test_live_invites_and_sign_ups_have_their_new_limits(self):
        self.assertEqual((mb.MAX_LIVE_INVITES, mb.REGISTER_PER_HOUR_DEFAULT if hasattr(mb, "REGISTER_PER_HOUR_DEFAULT") else 60,
                          mb.JOIN_FAILS, mb.JOIN_FAILS_IP), (20, 60, 10, 30))

    # -- F2: a state file that can't be read is not touched

    def test_an_unreadable_state_file_is_left_alone_and_the_relay_stops(self):
        mb.flush()
        with open(mb.STATE, "rb") as f:
            before = f.read()
        real_open = open

        def deny(path, *a, **kw):
            if path == mb.STATE:
                raise PermissionError(13, "denied")
            return real_open(path, *a, **kw)

        import builtins
        builtins.open, old = deny, builtins.open
        err = io.StringIO()
        old_err, sys.stderr = sys.stderr, err
        try:
            with self.assertRaises(SystemExit) as cm:
                mb.load()
        finally:
            builtins.open = old
            sys.stderr = old_err
        self.assertEqual(cm.exception.code, 78)
        self.assertIn("left as it is", err.getvalue())
        with open(mb.STATE, "rb") as f:
            self.assertEqual(f.read(), before)
        self.assertEqual([n for n in os.listdir(self.relay.dir.name) if ".corrupt-" in n], [])

    def test_bad_encoding_is_moved_aside_like_bad_json(self):
        with open(mb.STATE, "wb") as f:
            f.write(b"\xff\xfe\x00bad")
        old_err, sys.stderr = sys.stderr, io.StringIO()
        try:
            with self.assertRaises(SystemExit) as cm:
                mb.load()
        finally:
            sys.stderr = old_err
        self.assertEqual(cm.exception.code, 78)
        self.assertEqual(len([n for n in os.listdir(self.relay.dir.name) if ".corrupt-" in n]), 1)

    def test_the_unit_does_not_restart_after_a_refused_start(self):
        with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "hopper-mailbox.service"), encoding="utf-8") as f:
            unit = f.read()
        for line in ("RestartPreventExitStatus=78", "StateDirectory=hopper-mailbox", "StateDirectoryMode=0700",
                     "MemoryMax=512M", "LimitNOFILE=8192", "RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX",
                     "PrivateDevices=true", "ProtectKernelTunables=true", "SystemCallFilter=@system-service",
                     "UMask=0077"):
            self.assertIn(line, unit)

    # -- F3: a sign-up is on disk before it is answered

    def test_a_new_account_is_saved_before_the_answer(self):
        d = Client(self.relay, "durable")
        self.assertIn(d.id, self.saved()["users"])

    def test_a_sign_up_that_cannot_be_saved_is_refused_and_forgotten(self):
        good = mb.STATE
        blocker = os.path.join(self.relay.dir.name, "blocker")
        open(blocker, "w").close()
        mb.STATE = os.path.join(blocker, "state.json")
        old_err, sys.stderr = sys.stderr, io.StringIO()
        try:
            status, body, _, _ = self.reg("203.0.113.9")
        finally:
            sys.stderr = old_err
            mb.STATE = good
        self.assertEqual((status, json.loads(body)["code"]), (503, "save_failed"))
        with mb.lock:
            self.assertEqual(len(mb.state["users"]), 3)
            self.assertEqual(len(mb.token_index), 3)

    # -- F4: a pushed-out poll is told so

    def test_a_pushed_out_poll_gets_an_error_not_an_empty_answer(self):
        mb.MAX_POLLS_PER_USER = 1
        out = []
        t = threading.Thread(target=lambda: out.append(self.a.get("/v1/recipes?after=0&wait=8&v=2")))
        t.start()
        time.sleep(0.4)
        t2 = threading.Thread(target=lambda: self.a.get("/v1/recipes?after=0&wait=1&v=2"))
        t2.start()
        t.join(4)
        t2.join(4)
        self.assertEqual((out[0]["status"], out[0]["code"]), (409, "poll_superseded"))

    def test_wait_is_capped(self):
        mb.MAX_WAIT = 1
        t = time.time()
        self.a.get("/v1/recipes?after=0&wait=60&v=2")
        self.assertLess(time.time() - t, 3)

    # -- F5: the allowance is taken before the body is read

    def test_slow_sign_ups_cannot_all_pass_the_check_together(self):
        mb.REGISTER_PER_HOUR = 2
        mb.register_limit.clear()
        head = req("/v1/register", headers=["CF-Connecting-IP: 198.51.100.77"], body=b"").replace(
            b"Content-Length: 0", b"Content-Length: 20")
        held = []
        try:
            for _ in range(2):
                sk = socket.create_connection(("127.0.0.1", self.relay.port), timeout=3)
                sk.sendall(head)         # body never sent
                held.append(sk)
            time.sleep(0.4)
            status, _, _, _ = raw(self.relay, req("/v1/register", headers=["CF-Connecting-IP: 198.51.100.77"], body=b'{"name":"x"}'))
            self.assertEqual(status, 429)
        finally:
            for sk in held:
                sk.close()

    # -- F6: see test_code_and_left_out_are_cut_by_bytes (413)

    # -- F8: groups, queues and age

    def test_joining_a_group_gives_its_packs_without_using_the_queue(self):
        self.a.befriend(self.b)
        gid = self.make_group(self.a, [self.b])
        for i in range(30):
            self.a.post("/v1/recipes", dict(recipe("p%d" % i), to=[], groups=[gid]))
        self.b.post("/v1/groups/%s/accept" % gid)
        evs = [e for e in self.b.events() if e["kind"] == "recipe"]
        self.assertEqual(sorted(e["inst"] for e in evs), sorted("p%d" % i for i in range(30)))
        self.assertTrue(all(e["groups"] == [gid] and e["direct"] is False and e["from"] == self.a.id for e in evs))
        with mb.lock:
            self.assertEqual(len(mb.state["events"][self.b.id]), 1)    # just the "group" one, not 30 recipes
        # Asked again from the start (a fresh PC) they come again; from the end, never.
        self.b.after = 0
        self.assertEqual(len([e for e in self.b.events() if e["kind"] == "recipe"]), 30)
        self.assertEqual([e for e in self.b.events() if e["kind"] == "recipe"], [])

    def test_a_copys_stamps_go_along_and_bad_ones_are_dropped(self):
        self.a.befriend(self.b)
        gid = self.make_group(self.a, [self.b])
        marks = {
            "p:1": {"seq": 40, "by": "sam"},
            "p:2": {"seq": True, "by": "x"},      # not a number
            "p:3": {"seq": -1, "by": "x"},
            "p:4": "40",
            "n:" + "x" * 900: {"seq": 7, "by": "y" * 100},
        }
        self.a.post("/v1/recipes", dict(recipe("p1", marks=marks), to=[], groups=[gid]))
        self.b.post("/v1/groups/%s/accept" % gid)
        e = [e for e in self.b.events() if e["kind"] == "recipe"][-1]
        self.assertEqual(e["marks"]["p:1"], {"seq": 40, "by": "sam"})
        self.assertEqual(len(e["marks"]), 2)
        long = [k for k in e["marks"] if k.startswith("n:")][0]
        self.assertLessEqual(len(long.encode()), 600)
        self.assertEqual(len(e["marks"][long]["by"]), 64)
        # Through the queue too, and a code without stamps has none.
        self.a.post("/v1/recipes", dict(recipe("p1", code="HOP-2", marks={"p:1": {"seq": 41, "by": "a"}}), to=[], groups=[gid]))
        self.assertEqual(self.b.events()[-1]["marks"], {"p:1": {"seq": 41, "by": "a"}})
        self.a.post("/v1/recipes", dict(recipe("p1", code="HOP-3"), to=[], groups=[gid]))
        self.assertNotIn("marks", self.b.events()[-1])

    def test_a_pack_published_after_joining_comes_once_through_the_queue(self):
        self.a.befriend(self.b)
        gid = self.make_group(self.a, [self.b])
        self.b.post("/v1/groups/%s/accept" % gid)
        self.b.events()
        self.a.post("/v1/recipes", dict(recipe("late"), to=[], groups=[gid]))
        evs = [e for e in self.b.events() if e["kind"] == "recipe"]
        self.assertEqual([e["inst"] for e in evs], ["late"])
        self.b.after = 0
        again = [e for e in self.b.events() if e["kind"] == "recipe"]
        self.assertEqual([e["inst"] for e in again], ["late"])     # not twice

    def test_a_friend_with_many_packs_keeps_them_all(self):
        self.a.befriend(self.b)
        for i in range(40):
            self.a.post("/v1/recipes", dict(recipe("p%d" % i), to=[self.b.id]))
        self.assertEqual(len([e for e in self.b.events() if e["kind"] == "recipe"]), 40)
        # A new version of one pack replaces its old event.
        self.a.post("/v1/recipes", dict(recipe("p3", code="HOP-new"), to=[self.b.id]))
        self.b.after = 0
        evs = [e for e in self.b.events() if e["kind"] == "recipe" and e["inst"] == "p3"]
        self.assertEqual([e["code"] for e in evs], ["HOP-new"])

    def test_old_events_expire_but_recipes_last_longer(self):
        self.a.befriend(self.b)
        self.a.post("/v1/recipes", dict(recipe("keep"), to=[self.b.id]))
        with mb.lock:
            mb.make_event(self.b.id, "group", self.a.id, group="old")
            for e in mb.state["events"][self.b.id]:
                e["at"] = mb.now() - mb.EVENT_TTL - 10
        mb.prune_accounts()
        kinds = [e["kind"] for e in self.b.events()]
        self.assertIn("recipe", kinds)
        self.assertNotIn("group", kinds)

    # -- F9, F10: odd headers, slow requests

    def test_bad_header_names_are_refused(self):
        for name in ("Bad Name", "Bad(Name)", "Bad\x01Name", "Bad@Name"):
            data = req("/v1/health", "GET", headers=["%s: v" % name], cl=False)
            status, _, closed, _ = raw(self.relay, data)
            self.assertEqual(status, 400, name)
            self.assertTrue(closed, name)
        self.assertEqual(raw(self.relay, req("/v1/health", "GET", headers=["X-Fine_Name.1: v"], cl=False), wait=0.5)[0], 200)

    def test_a_request_that_trickles_in_is_cut_at_the_deadline(self):
        mb.REQUEST_DEADLINE = 1.5
        sk = socket.create_connection(("127.0.0.1", self.relay.port), timeout=6)
        t = time.time()
        closed = False
        try:
            sk.sendall(b"POST /v1/register HTTP/1.1\r\nHost: x\r\nContent-Length: 40\r\n\r\n")
            while time.time() - t < 5:
                try:
                    sk.sendall(b"{")
                except OSError:
                    closed = True
                    break
                time.sleep(0.3)      # well inside the per-read timeout
                sk.settimeout(0.01)
                try:
                    if sk.recv(100) == b"":
                        closed = True
                        break
                except socket.timeout:
                    pass
                except OSError:
                    closed = True
                    break
        finally:
            sk.close()
        self.assertTrue(closed)
        self.assertLess(time.time() - t, 4)

    # -- F11, F12, F13

    def test_a_dropped_connection_logs_one_line(self):
        err = io.StringIO()
        old, sys.stderr = sys.stderr, err
        try:
            try:
                raise ConnectionResetError("peer gone")
            except ConnectionResetError:
                self.relay.srv.handle_error(None, ("203.0.113.4", 5))
        finally:
            sys.stderr = old
        out = err.getvalue()
        self.assertEqual(out.count("\n"), 1)
        self.assertIn("203.0.113.4", out)
        self.assertNotIn("Traceback", out)

    def test_the_saver_survives_an_error(self):
        mb.SAVE_INTERVAL = 0.05
        real = mb.flush
        calls = []

        def flaky():
            calls.append(1)
            if len(calls) == 1:
                raise RuntimeError("disk exploded")
            return real()

        mb.flush = flaky
        old_err, sys.stderr = sys.stderr, io.StringIO()
        try:
            mb.start_writer()
            with mb.lock:
                mb.mark_dirty()
            deadline = time.time() + 8
            while len(calls) < 2 and time.time() < deadline:
                with mb.lock:
                    mb.mark_dirty()
                time.sleep(0.1)
        finally:
            sys.stderr = old_err
            mb.flush = real
        self.assertGreaterEqual(len(calls), 2)
        self.assertTrue(mb._writer["thread"].is_alive())

    def test_new_requests_get_503_once_the_service_is_closing(self):
        mb._closing.set()
        try:
            status, body, closed, _ = raw(self.relay, req("/v1/health", "GET", cl=False))
        finally:
            mb._closing.clear()
        self.assertEqual((status, json.loads(body)["code"]), (503, "busy"))
        self.assertTrue(closed)
        self.assertEqual(raw(self.relay, req("/v1/health", "GET", cl=False), wait=0.5)[0], 200)

    def test_the_final_save_reports_whether_it_worked(self):
        with mb.lock:
            mb.mark_dirty()
        self.assertTrue(mb.stop_writer())
        with mb.lock:
            mb.mark_dirty()
        good = mb.STATE
        mb.STATE = os.path.join(self.relay.dir.name, "nodir", "x", "state.json")
        os.makedirs(os.path.join(self.relay.dir.name, "nodir"))
        open(os.path.join(self.relay.dir.name, "nodir", "x"), "w").close()
        old_err, sys.stderr = sys.stderr, io.StringIO()
        try:
            self.assertFalse(mb.stop_writer())
        finally:
            sys.stderr = old_err
            mb.STATE = good


class RoomTest(Base):
    """The size budget: share codes can't use up the room accounts need."""

    def setUp(self):
        super().setUp()
        self.a = Client(self.relay, "hopper-test-a")
        self.b = Client(self.relay, "hopper-test-b")
        self.c = Client(self.relay, "hopper-test-c")
        self.a.befriend(self.b)
        self.a.befriend(self.c)
        self.b.befriend(self.c)

    def send(self, who, inst, n, to):
        return who.post("/v1/recipes", dict(recipe(inst, code="HOP-" + "a" * n), to=to, groups=[]))

    def test_direct_sends_stop_at_the_budget_but_updates_and_sign_ups_go_on(self):
        mb.flush()
        base = mb.state_used()
        # Share codes may use 25000 more bytes; the whole state a third more.
        mb.MAX_STATE_BYTES = int((base + 25000) / mb.EVENT_BUDGET_FRACTION)
        self.assertNotIn("status", self.send(self.a, "p1", 10000, [self.b.id]))
        self.assertNotIn("status", self.send(self.a, "p2", 10000, [self.b.id]))
        r = self.send(self.a, "p3", 10000, [self.b.id])
        self.assertEqual((r["status"], r["code"]), (503, "busy"))
        r = self.send(self.a, "p4", 6000, [self.b.id])
        self.assertEqual((r["status"], r["code"]), (503, "busy"))
        # A new version that is not bigger still goes (a smaller one, and the same size).
        self.assertNotIn("status", self.send(self.a, "p1", 5000, [self.b.id]))
        self.assertNotIn("status", self.send(self.a, "p1", 5000, [self.b.id]))
        # A bigger one does not.
        self.assertEqual(self.send(self.a, "p1", 18000, [self.b.id])["code"], "busy")
        # The older one-friend call is held to the same rule.
        r = self.a.post("/v1/recipes", dict(recipe("old", code="HOP-" + "a" * 12000), to=self.b.id))
        self.assertEqual((r["status"], r["code"]), (503, "busy"))
        # Accounts still sign up: the last quarter of the room is theirs.
        d = Client(self.relay, "late-sign-up")
        self.assertTrue(d.id)
        # Nothing was queued by the refusals.
        got = {e["inst"]: len(e["code"]) for e in self.b.events() if e["kind"] == "recipe"}
        self.assertEqual(sorted(got), ["p1", "p2"])
        self.assertEqual(got["p1"], 5004)

    def test_group_packs_stop_at_the_same_budget(self):
        gid = self.a.post("/v1/groups", {"name": "Crew", "members": [self.b.id]})["group"]["id"]
        self.b.post("/v1/groups/%s/accept" % gid)
        mb.flush()
        mb.MAX_STATE_BYTES = int((mb.state_used() + 100) / mb.EVENT_BUDGET_FRACTION)
        r = self.a.post("/v1/recipes", dict(recipe("g1", code="HOP-" + "a" * 5000), to=[], groups=[gid]))
        self.assertEqual((r["status"], r["code"]), (503, "busy"))

    def test_one_sender_has_a_quota_across_all_friends(self):
        mb.SENDER_QUOTA_BYTES = 50000
        for i in range(3):
            self.assertNotIn("status", self.send(self.a, "p%d" % i, 15000, [self.b.id]))
        r = self.send(self.a, "p3", 15000, [self.b.id])
        self.assertEqual((r["status"], r["code"]), (409, "storage_full"))
        # The quota is for all their friends together ...
        r = self.send(self.a, "p4", 15000, [self.c.id])
        self.assertEqual((r["status"], r["code"]), (409, "storage_full"))
        # ... a new version of the same size fits, and others have their own.
        self.assertNotIn("status", self.send(self.a, "p0", 15000, [self.b.id]))
        self.assertNotIn("status", self.send(self.b, "q0", 15000, [self.c.id]))
        # Packs taken out of the queues (expired here) give the room back.
        with mb.lock:
            for e in mb.state["events"][self.b.id]:
                e["at"] = mb.now() - mb.KEEP_TTL - 10
        mb.prune_accounts()
        self.assertNotIn("status", self.send(self.a, "p5", 15000, [self.c.id]))

    def test_a_full_queue_drops_the_biggest_sender_not_the_smallest(self):
        mb.MAX_QUEUE_BYTES = 40000
        self.assertNotIn("status", self.send(self.c, "small", 50, [self.b.id]))
        for i in range(3):
            self.assertNotIn("status", self.send(self.a, "big%d" % i, 15000, [self.b.id]))
        got = [(e["from"], e["inst"]) for e in self.b.events() if e["kind"] == "recipe"]
        self.assertIn((self.c.id, "small"), got)
        self.assertNotIn((self.a.id, "big0"), got)            # their oldest went
        self.assertEqual(sorted(i for f, i in got if f == self.a.id), ["big1", "big2"])

    def test_a_state_file_over_the_hard_limit_is_not_loaded(self):
        mb.MAX_STATE_BYTES = 1000
        with open(mb.STATE, "w", encoding="utf-8") as f:
            f.write('{"users": {}, "pad": "' + "x" * 2000 + '"}')
        with open(mb.STATE, "rb") as f:
            before = f.read()
        old_err, sys.stderr = sys.stderr, io.StringIO()
        try:
            with self.assertRaises(SystemExit) as cm:
                mb.load()
        finally:
            err_text, sys.stderr = sys.stderr.getvalue(), old_err
        self.assertEqual(cm.exception.code, mb.EXIT_BAD_STATE)
        self.assertIn("too big", err_text.replace("over the", "too big over the"))
        with open(mb.STATE, "rb") as f:
            self.assertEqual(f.read(), before)           # untouched, not moved aside
        self.assertEqual([n for n in os.listdir(self.relay.dir.name) if ".corrupt-" in n], [])
        # Just under the limit loads.
        with open(mb.STATE, "w", encoding="utf-8") as f:
            f.write('{"users": {}}')
        mb.load()

    def test_a_refused_start_uses_a_code_ordinary_crashes_do_not(self):
        self.assertEqual(mb.EXIT_BAD_STATE, 78)
        with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "hopper-mailbox.service"), encoding="utf-8") as f:
            unit = f.read()
        self.assertIn("RestartPreventExitStatus=78", unit)
        self.assertNotIn("RestartPreventExitStatus=1\n", unit)
        self.assertIn("Restart=always", unit)

    def test_one_call_with_a_direct_and_a_group_target_makes_one_event(self):
        gid = self.a.post("/v1/groups", {"name": "Crew", "members": [self.b.id]})["group"]["id"]
        self.b.post("/v1/groups/%s/accept" % gid)
        self.b.events()
        self.a.post("/v1/recipes", dict(recipe("p1"), to=[self.b.id], groups=[gid]))
        evs = [e for e in self.b.events() if e["kind"] == "recipe"]
        self.assertEqual([(e["direct"], e["groups"]) for e in evs], [(True, [gid])])
        # The desktop sends the whole set each time (it keeps the targets per
        # pack), so a later call replaces: the way that was dropped goes with
        # a revoke of its own, not by merging.
        self.b.after = 0
        self.a.post("/v1/recipes", dict(recipe("p1", code="HOP-v2"), to=[], groups=[gid]))
        evs = [e for e in self.b.events() if e["kind"] == "recipe" and e["inst"] == "p1"]
        self.assertEqual([(e["code"], e["direct"], e["groups"]) for e in evs], [("HOP-v2", False, [gid])])


if __name__ == "__main__":
    unittest.main()
