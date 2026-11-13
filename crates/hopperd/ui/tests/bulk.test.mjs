import test from 'node:test';
import assert from 'node:assert/strict';
import { planBulk, runBulk, counts, tally, title, okLabel, stateGroup, passes } from '../src/lib/bulk.js';

const srv = (id, state, players = []) => ({ id, name: id, state, players });
const home = (servers, role = 'owner') => ({ key: 'home', name: 'Home', panel: null, role, state: 'ok', servers });
const other = (servers, role, state = 'ok') => ({ key: 'b', name: 'Panel B', panel: { id: 'b' }, role, state, servers });

test('a viewer panel is skipped whole and never sent', async () => {
  const plan = planBulk([home([srv('a', 'running'), srv('b', 'running')]), other([srv('c', 'running')], 'viewer')], 'restart');
  assert.deepEqual(counts(plan), { send: 2, skip: 1, online: 0, panels: 1 });
  assert.equal(plan[1].skip[0].reason, 'Your account on Panel B can only look.');
  const sent = [];
  const res = await runBulk(plan, '', async (p, s, act) => sent.push(`${p ? p.id : 'home'}/${s.id}/${act}`));
  assert.deepEqual(sent, ['home/a/restart', 'home/b/restart']);
  assert.deepEqual(tally(res), { ok: 2, failed: 0, skipped: 1 });
  assert.equal(res.b.c.status, 'skipped');
});

test('a panel that is down or signed out is skipped with its reason', () => {
  const off = planBulk([other([srv('c', 'running')], 'owner', 'offline')], 'stop');
  assert.equal(off[0].skip[0].reason, "Can't reach Panel B right now.");
  const out = planBulk([other([srv('c', 'running')], 'owner', 'signin')], 'stop');
  assert.equal(out[0].skip[0].reason, 'Sign in to Panel B again first.');
});

test('each action skips servers it makes no sense for', () => {
  const g = () => [home([srv('up', 'running'), srv('off', 'stopped'), srv('busy', 'stopping')])];
  const ids = (plan, k) => plan[0][k].map((x) => x.server.id);
  assert.deepEqual(ids(planBulk(g(), 'start'), 'send'), ['off']);
  assert.deepEqual(ids(planBulk(g(), 'stop'), 'send'), ['up']);
  assert.deepEqual(ids(planBulk(g(), 'command'), 'send'), ['up']);
  const r = planBulk(g(), 'restart');
  // A stopped server is started by a restart, and says so.
  assert.deepEqual(r[0].send.map((x) => [x.server.id, x.act]), [['up', 'restart'], ['off', 'start']]);
  assert.match(r[0].send[1].note, /starts/);
  assert.deepEqual(ids(r, 'skip'), ['busy']);
});

test('players online are counted for stop and restart only', () => {
  const s = [srv('a', 'running', ['Steve', 'Alex']), srv('b', 'running', ['  '])];
  assert.equal(counts(planBulk([home(s)], 'stop')).online, 2);
  assert.equal(counts(planBulk([home(s)], 'command')).online, 0);
});

test('a failure on one panel is reported and the rest still go', async () => {
  const plan = planBulk([home([srv('a', 'running'), srv('b', 'running')]), other([srv('c', 'running')], 'admin')], 'stop');
  const seen = [];
  const res = await runBulk(
    plan,
    '',
    async (p, s) => {
      if (s.id === 'a') throw new Error('a is being restored from a backup.');
    },
    (g, id, r) => seen.push(`${g}/${id}/${r.status}`),
  );
  assert.equal(res.home.a.status, 'failed');
  assert.equal(res.home.a.text, 'a is being restored from a backup.');
  assert.equal(res.home.b.status, 'ok');
  assert.equal(res.b.c.status, 'ok');
  assert.equal(seen.length, 3);
});

test('a command line reaches every sent server', async () => {
  const plan = planBulk([home([srv('a', 'running')])], 'command');
  const lines = [];
  await runBulk(plan, 'say hi', async (p, s, act, line) => lines.push(`${act}:${line}`));
  assert.deepEqual(lines, ['command:say hi']);
});

test('groups with nothing picked are left out of the plan', () => {
  assert.equal(planBulk([home([]), other([srv('c', 'running')], 'owner')], 'stop').length, 1);
});

test('words', () => {
  assert.equal(title('restart', 3), 'Restart 3 servers?');
  assert.equal(title('command', 1), 'Send a command to 1 server?');
  assert.equal(okLabel('stop', 2), 'Stop 2 servers');
});

test('state filter', () => {
  assert.equal(stateGroup(srv('a', 'starting')), 'running');
  assert.equal(stateGroup(srv('a', 'crashed')), 'crashed');
  assert.equal(stateGroup({ ...srv('a', 'stopped'), wake_failed: true }), 'crashed');
  assert.equal(stateGroup(srv('a', 'stopped')), 'stopped');
  assert.ok(passes(srv('a', 'crashed'), 'all'));
  assert.ok(!passes(srv('a', 'crashed'), 'running'));
});
