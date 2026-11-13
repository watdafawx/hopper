import { test } from 'node:test';
import assert from 'node:assert/strict';
import { nodeLine, span, codeGroups, defaultAddress, STATES, ceilingLabel, onNode, canShareLog, nodeOf, alertShows, stateOf, updateLine, updateLabel, behind, behindLine } from '../src/lib/nodes.js';

const now = 1_800_000_000;

test('a connected node shows version, round trip, since and address', () => {
  const line = nodeLine({ state: 'connected', version: '0.62.0', rtt_ms: 41, since: now - 60, ip: '10.0.0.4' }, now);
  assert.match(line, /^hopperd 0\.62\.0 · 41 ms · since \d\d?:\d\d( [AP]M)? · 10\.0\.0\.4$/);
  // Nothing missing shows as "undefined".
  assert.equal(nodeLine({ state: 'connected' }, now), '');
});

test('reconnecting and offline say how long', () => {
  assert.equal(nodeLine({ state: 'reconnecting', quiet_secs: 40 }, now), 'Last heard 40 s ago');
  assert.match(nodeLine({ state: 'offline', last_seen: now - 120, ended: 'stopped answering' }, now), /^Offline since \d\d?:\d\d( [AP]M)? · stopped answering$/);
  assert.equal(nodeLine({ state: 'offline', last_seen: 0 }, now), 'Offline since this panel started');
  assert.match(nodeLine({ state: 'new', created: now }, now), /Waiting for it to connect\.$/);
});

test('spans and codes', () => {
  assert.equal(span(59), '59 s');
  assert.equal(span(61), '1 min');
  assert.equal(span(7300), '2 h');
  assert.deepEqual(codeGroups('481072'), ['481', '072']);
  assert.equal(ceilingLabel('operate'), 'Operate');
  for (const s of ['connected', 'reconnecting', 'offline', 'new']) assert.ok(STATES[s].label);
});

test('the address nodes use defaults to this page over HTTPS', () => {
  assert.equal(defaultAddress({ protocol: 'https:', origin: 'https://fox:8790', hostname: 'fox' }, {}), 'https://fox:8790');
  const hello = { port: 8790, addresses: ['https://127.0.0.1:8790', 'https://192.168.50.3:8790'] };
  assert.equal(defaultAddress({ protocol: 'http:', origin: 'http://localhost:8790', hostname: 'localhost' }, hello), 'https://192.168.50.3:8790');
  assert.equal(defaultAddress({ protocol: 'http:', hostname: 'rig' }, { port: 9 }), 'https://rig:9');
});

test("sharing a log on mclo.gs shows only for a panel's own servers", () => {
  // Not relayed to nodes: the node would answer 403.
  assert.equal(canShareLog({ id: 'tnp' }), true);
  assert.equal(canShareLog({ id: 'ab12cd34ef56:tnp' }), false);
  assert.equal(canShareLog({ id: 'tnp', node: 'ab12cd34ef56' }), false);
  assert.equal(onNode(null), false);
  assert.equal(onNode({}), false);
});

test('a node machine alert shows on that node servers and the Overview only', () => {
  assert.equal(nodeOf('ab12:dg'), 'ab12');
  assert.equal(nodeOf('dg'), '');
  const nodeDisk = { server_id: '', kind: 'disk', node: 'ab12' };
  const ownDisk = { server_id: '', kind: 'disk' };
  const dgTps = { server_id: 'ab12:dg', kind: 'tps', node: 'ab12' };
  assert.ok(alertShows(nodeDisk, 'ab12:dg'));
  assert.ok(!alertShows(nodeDisk, 'local'));
  assert.ok(!alertShows(nodeDisk, 'cd34:other'));
  assert.ok(alertShows(nodeDisk, ''), 'the Overview shows every machine');
  assert.ok(alertShows(ownDisk, 'local'));
  assert.ok(!alertShows(ownDisk, 'ab12:dg'), "this panel's drive isn't the node's");
  assert.ok(alertShows(dgTps, 'ab12:dg'));
  assert.ok(!alertShows(dgTps, 'dg'));
  assert.ok(!alertShows(dgTps, ''), 'a server alert stays on its page');
});

test('a node two versions apart says what to do, read-only', () => {
  const n = { state: 'update_needed', skew: 'vm (hopperd 0.40.0) is too old for this panel. Update it on the node itself: its own update, or install the new hopperd there.' };
  assert.equal(stateOf(n).label, 'Update needed');
  assert.equal(nodeLine(n, now), n.skew);
  assert.match(nodeLine({ state: 'main_older' }, now), /Update this panel first/);
  assert.equal(STATES.main_older.label, 'Update this panel');
});

test('an update in progress, failed and done', () => {
  const n = { state: 'connected', outdated: true, can_update: true };
  assert.equal(updateLine(n), null);
  assert.equal(updateLabel(n, '0.63.0'), 'Update to 0.63.0');
  const going = { ...n, update: { state: 'updating', message: 'Downloading hopperd 0.63.0' } };
  assert.equal(stateOf(going).label, 'Updating');
  assert.deepEqual(updateLine(going), { text: 'Updating · Downloading hopperd 0.63.0', bad: false });
  const failed = { ...n, update: { state: 'failed', message: "vm: A development build: it doesn't update itself." } };
  assert.equal(updateLine(failed).bad, true);
  assert.equal(updateLabel(failed, '0.63.0'), 'Try again');
  assert.equal(stateOf(failed).label, 'Connected');
  // Done and current: the message shows; done but still behind: nothing.
  assert.equal(updateLine({ state: 'connected', outdated: false, update: { state: 'done', message: 'vm runs hopperd 0.63.0 now.' } }).text, 'vm runs hopperd 0.63.0 now.');
});

test('the list head counts nodes behind and what Update all does', () => {
  const data = { version: '0.63.0', nodes: [{ outdated: true, can_update: true }, { state: 'update_needed' }, { outdated: false }] };
  assert.deepEqual(behind(data), { older: 2, updatable: 1 });
  assert.equal(behindLine(data), 'This panel runs hopperd 0.63.0. 2 nodes run an older one.');
  assert.equal(behindLine({ nodes: [{}] }), '');
});
