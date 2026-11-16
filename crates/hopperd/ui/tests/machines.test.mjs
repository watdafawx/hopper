import { test } from 'node:test';
import assert from 'node:assert/strict';
import { KIND, keepsPanels, serverCount, nodeLight, panelLight, panelCount, machines, needYou, panelLine, nodeRowLine, kindTip } from '../src/lib/machines.js';

const node = (id, state, more = {}) => ({ id, name: id, state, count: 2, ceiling: 'manage', ...more });
const panel = (id, state, more = {}) => ({ id: `${id}:8790`, url: `http://${id}:8790`, name: id, state, servers: [{ id: 'a' }], off: false, ...more });

test('both kinds read the same: a chip for how it is connected', () => {
  assert.equal(KIND.node, 'Node');
  assert.equal(KIND.panel, 'Signed in');
  assert.equal(KIND.home, 'This panel');
});

test('only a password account keeps a list of other machines', () => {
  assert.ok(keepsPanels('owner'));
  assert.ok(keepsPanels('operator'));
  assert.ok(!keepsPanels('device'));
  assert.ok(!keepsPanels('token'));
  assert.ok(!keepsPanels(''));
  assert.ok(!keepsPanels(undefined));
});

test('server counts', () => {
  assert.equal(serverCount(0), '0 servers');
  assert.equal(serverCount(1), '1 server');
  assert.equal(serverCount(6), '6 servers');
  assert.equal(serverCount(null), '');
});

test('lights: red cannot be used, amber on its way, green up, grey turned off', () => {
  assert.equal(nodeLight(node('a', 'connected')), 'running');
  assert.equal(nodeLight(node('a', 'connected', { maintenance: { by: 'owner' } })), 'starting');
  assert.equal(nodeLight(node('a', 'connected', { update: { state: 'updating' } })), 'starting');
  assert.equal(nodeLight(node('a', 'reconnecting')), 'starting');
  assert.equal(nodeLight(node('a', 'update_needed')), 'starting');
  assert.equal(nodeLight(node('a', 'offline')), 'crashed');
  assert.equal(panelLight(panel('p', 'ok')), 'running');
  assert.equal(panelLight(panel('p', 'signin')), 'starting');
  assert.equal(panelLight(panel('p', 'refused')), 'crashed');
  assert.equal(panelLight(panel('p', 'offline')), 'crashed');
  assert.equal(panelLight(panel('p', 'off', { off: true })), '');
});

test('a panel shows a count only once it is known', () => {
  assert.equal(panelCount(panel('p', 'ok')), 1);
  assert.equal(panelCount(panel('p', 'ok', { servers: [] })), 0);
  assert.equal(panelCount(panel('p', 'offline')), 1);
  assert.equal(panelCount(panel('p', 'offline', { servers: [] })), null);
  assert.equal(panelCount(panel('p', 'signin')), null);
  assert.equal(panelCount(panel('p', 'ok', { off: true })), null);
});

test('one list: trouble first, then by name, turned off last', () => {
  const list = machines(
    [node('vm-frankfurt', 'connected'), node('a1-jeddah', 'offline'), node('pi-garage', 'reconnecting')],
    [panel('sams-box', 'ok'), panel('friends', 'offline'), panel('lan-party', 'signin'), panel('old-laptop', 'off', { off: true })],
  );
  assert.deepEqual(list.map((m) => m.name), ['a1-jeddah', 'friends', 'lan-party', 'pi-garage', 'sams-box', 'vm-frankfurt', 'old-laptop']);
  assert.deepEqual(list.map((m) => m.kind), ['node', 'panel', 'panel', 'node', 'panel', 'node', 'panel']);
  // Keys never clash between the kinds: a node id can look like a host.
  assert.equal(new Set(list.map((m) => m.key)).size, list.length);
  assert.equal(needYou(list), 4);
});

test('an empty list', () => {
  assert.deepEqual(machines(), []);
  assert.equal(needYou([]), 0);
});

test('a signed-in panel says its state and the one thing to do', () => {
  const now = 1_000_000_000_000;
  assert.deepEqual(panelLine(panel('p', 'ok', { version: '0.71.0', ping: 18 }), 'Owner'), { text: "Connected · hopperd 0.71.0 · 18 ms · you're its Owner", bad: false });
  assert.deepEqual(panelLine(panel('p', 'offline', { lastSeen: now - 5 * 60_000 }), '', now), { text: "Can't reach it · seen 5 min ago", bad: true, act: 'retry' });
  assert.equal(panelLine(panel('p', 'reconnecting')).act, 'retry');
  assert.deepEqual(panelLine(panel('p', 'signin', { reason: 'expired' })), { text: 'Your sign-in ended', bad: false, act: 'signin' });
  assert.equal(panelLine(panel('p', 'signin')).text, 'Not signed in yet');
  assert.equal(panelLine(panel('p', 'refused')).act, 'open');
  assert.equal(panelLine(panel('p', 'old', { version: '0.40.0' })).text, 'Runs Hopper Server 0.40.0, too old to use from here');
  assert.equal(panelLine(panel('p', 'old')).text, 'Runs Hopper Server, too old to use from here');
  assert.equal(panelLine(panel('p', 'setup')).act, 'open');
  assert.equal(panelLine(panel('p', 'off', { off: true })).act, undefined);
});

test("a node's line leads with its state, since the chip says it's a node", () => {
  assert.deepEqual(nodeRowLine(node('n', 'connected', { version: '0.71.0', rtt_ms: 31 })), { text: 'Connected · hopperd 0.71.0 · 31 ms', bad: false });
  assert.equal(nodeRowLine(node('n', 'reconnecting', { quiet_secs: 40 })).text, 'Reconnecting… · Last heard 40 s ago');
  assert.ok(nodeRowLine(node('n', 'offline')).bad);
  assert.ok(nodeRowLine(node('n', 'update_needed')).text.startsWith('Update needed · '));
  assert.deepEqual(nodeRowLine(node('n', 'connected', { maintenance: { by: 'owner', since: 1000 } }), 1000 + 3 * 3600), { text: 'Under maintenance · by owner 3 h ago · nothing starts here', bad: false });
  assert.deepEqual(nodeRowLine(node('n', 'connected', { update: { state: 'failed', message: 'no space' } })), { text: 'Update failed: no space', bad: true });
});

test('the chip tooltip says what the kind means', () => {
  assert.equal(kindTip('node', { ceiling: 'operate' }), 'A node of this panel: it connects here by itself. This panel may: Operate.');
  assert.equal(kindTip('panel', { role: 'Owner' }), 'This browser signs in to its own panel. Your role there: Owner.');
  assert.equal(kindTip('panel'), 'This browser signs in to its own panel.');
  assert.equal(kindTip('home', { role: 'Manager' }), 'This panel. Your role here: Manager.');
});
