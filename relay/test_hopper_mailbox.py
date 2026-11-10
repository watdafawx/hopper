"""Tests for the mailbox, against a real server on a loopback port.

Run from the repo root (standard library only):

    python -B -m unittest relay/test_hopper_mailbox.py -v

Each test gets a fresh state file in a temp folder; nothing talks to the
public relay.
"""

import json
import os
import sys
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from http.server import ThreadingHTTPServer

sys.dont_write_bytecode = True
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import hopper_mailbox as mb  # noqa: E402


class Relay:
    """One mailbox on 127.0.0.1 with its own state file."""

    def __init__(self):
        self.dir = tempfile.TemporaryDirectory()
        mb.STATE = os.path.join(self.dir.name, "state.json")
        mb.state = {"users": {}, "friends": {}, "invites": {}, "events": {}, "groups": {}, "seq": 0}
        self.srv = ThreadingHTTPServer(("127.0.0.1", 0), mb.Handler)
        self.srv.daemon_threads = True
        self.url = "http://127.0.0.1:%d" % self.srv.server_address[1]
        self.thread = threading.Thread(target=self.srv.serve_forever, daemon=True)
        self.thread.start()

    def close(self):
        self.srv.shutdown()
        self.srv.server_close()
        self.dir.cleanup()


class Client:
    def __init__(self, relay, name):
        self.relay = relay
        self.token = None
        r = self.post("/v1/register", {"name": name})
        self.id, self.token = r["id"], r["token"]
        self.after = 0

    def call(self, method, path, body=None):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(self.relay.url + path, data=data, method=method)
        if data is not None:
            req.add_header("Content-Type", "application/json")
        if self.token:
            req.add_header("Authorization", "Bearer " + self.token)
        try:
            with urllib.request.urlopen(req, timeout=10) as r:
                return json.loads(r.read())
        except urllib.error.HTTPError as e:
            out = json.loads(e.read())
            out["status"] = e.code
            return out

    def post(self, path, body=None):
        return self.call("POST", path, body or {})

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


class MailboxTest(unittest.TestCase):
    def setUp(self):
        self.relay = Relay()
        self.a = Client(self.relay, "hopper-test-a")
        self.b = Client(self.relay, "hopper-test-b")
        self.c = Client(self.relay, "hopper-test-c")

    def tearDown(self):
        self.relay.close()

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
        with open(mb.STATE, encoding="utf-8") as f:
            saved = json.load(f)
        self.assertIn(gid, saved["groups"])
        # An old state file (no groups) loads with an empty list.
        with open(mb.STATE, "w", encoding="utf-8") as f:
            json.dump({k: v for k, v in saved.items() if k != "groups"}, f)
        mb.state["groups"] = {}
        mb.load()
        self.assertEqual(mb.state["groups"], {})
        self.assertIn(self.a.id, mb.state["users"])


if __name__ == "__main__":
    unittest.main()
