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

test("a node's servers are their own group: its ceiling, pause and state decide", async () => {
  const { nodeGroup } = await import('../src/lib/bulk.js');
  const n = (over) => ({ id: 'ab12cd34ef56', name: 'vm', state: 'connected', ceiling: 'operate', maintenance: null, ...over });
  const g = (node, servers, role = 'owner') => ({ ...nodeGroup(node, role), servers });
  const up = srv('ab12cd34ef56:a', 'running');
  const off = srv('ab12cd34ef56:b', 'stopped');
  let plan = planBulk([home([srv('h', 'running')]), g(n(), [up, off])], 'restart');
  assert.deepEqual(counts(plan), { send: 3, skip: 0, online: 0, panels: 2 });
  assert.equal(plan[1].node, 'ab12cd34ef56');
  plan = planBulk([g(n({ ceiling: 'look' }), [up])], 'restart');
  assert.equal(plan[0].skip[0].reason, 'vm lets this panel only look.');
  plan = planBulk([g(n({ maintenance: { by: 'owner' } }), [up, off])], 'restart');
  // Paused: a running one restarts, a stopped one doesn't start.
  assert.deepEqual(plan[0].send.map((x) => x.server.id), ['ab12cd34ef56:a']);
  assert.equal(plan[0].skip[0].reason, 'vm is under maintenance.');
  assert.equal(planBulk([g(n({ state: 'offline' }), [up])], 'stop')[0].skip[0].reason, "Can't reach vm right now.");
  assert.match(planBulk([g(n({ state: 'update_needed' }), [up])], 'stop')[0].skip[0].reason, /needs an update/);
  assert.equal(planBulk([g(n(), [up], 'viewer')], 'stop')[0].skip[0].reason, 'Your account can only look.');
});

test('a moved copy, or one being sent, is skipped by a bulk start', () => {
  const moved = { ...srv('m', 'stopped'), moved: { to: 'vm' } };
  const sending = { ...srv('s', 'stopped'), sending: true };
  for (const action of ['start', 'restart']) {
    const plan = planBulk([home([moved, sending, srv('a', 'stopped')])], action);
    assert.deepEqual(plan[0].send.map((x) => x.server.id), ['a']);
    assert.match(plan[0].skip[0].reason, /^This copy was moved to vm\. Choose Keep using this copy/);
    assert.match(plan[0].skip[1].reason, /being sent/);
  }
  // Stopping one that is somehow running still goes.
  const up = { ...srv('u', 'running'), moved: { to: 'vm' } };
  assert.equal(planBulk([home([up])], 'stop')[0].send.length, 1);
});
