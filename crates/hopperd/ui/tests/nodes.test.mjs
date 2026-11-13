import { test } from 'node:test';
import assert from 'node:assert/strict';
import { nodeLine, span, codeGroups, defaultAddress, STATES, ceilingLabel, onNode, canShareLog, nodeOf, alertShows } from '../src/lib/nodes.js';

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
