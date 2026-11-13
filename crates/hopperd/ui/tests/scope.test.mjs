import test from 'node:test';
import assert from 'node:assert/strict';
import { scopeRows, covers, flipScope, scopeText, scopeCount, scopeHint, NO_SERVERS } from '../src/lib/scope.js';

const servers = [{ id: 'lobby', name: 'Lobby' }, { id: 'ab12cd34ef56:atm', name: 'ATM10', node: 'ab12cd34ef56' }, { id: '0011aabbccdd:sky', name: 'Skyblock', node: '0011aabbccdd' }];
const nodes = [{ id: 'ab12cd34ef56', name: 'vm-frankfurt' }, { id: '0011aabbccdd', name: 'vm-jeddah' }, { id: 'ffffffffffff', name: 'vm-empty' }];

test('the picker has this panel, then a row per node, empty ones too', () => {
  const rows = scopeRows(servers, nodes);
  assert.deepEqual(rows.map((r) => [r.name, r.servers.map((s) => s.id)]), [['This panel', ['lobby']], ['vm-frankfurt', ['ab12cd34ef56:atm']], ['vm-jeddah', ['0011aabbccdd:sky']], ['vm-empty', []]]);
});

test('a whole node covers its servers and replaces single picks there', () => {
  let l = flipScope([], 'ab12cd34ef56:atm');
  l = flipScope(l, 'lobby');
  l = flipScope(l, 'node:ab12cd34ef56');
  assert.deepEqual(l, ['lobby', 'node:ab12cd34ef56']);
  assert.ok(covers(l, 'ab12cd34ef56:later') && !covers(l, '0011aabbccdd:sky'));
  assert.deepEqual(flipScope(l, 'node:ab12cd34ef56'), ['lobby']);
  // "No servers" goes once something is picked.
  assert.deepEqual(flipScope([NO_SERVERS], 'lobby'), ['lobby']);
});

test('rows say what a list covers in plain words', () => {
  assert.equal(scopeText([], servers, nodes), 'All servers');
  assert.equal(scopeText([NO_SERVERS], servers, nodes), 'No servers (their node was removed)');
  assert.equal(scopeText(['node:ab12cd34ef56'], servers, nodes), 'All on vm-frankfurt');
  assert.equal(scopeText(['lobby', 'node:ab12cd34ef56'], servers, nodes), 'Lobby, all on vm-frankfurt');
  assert.equal(scopeText(['node:999999999999'], servers, nodes), 'All on a removed node');
  assert.equal(scopeCount([], nodes), 'all servers');
  assert.equal(scopeCount(['lobby', 'x'], nodes), '2 servers');
  assert.equal(scopeCount(['node:ab12cd34ef56'], nodes), 'all servers on vm-frankfurt');
  assert.equal(scopeCount(['lobby', 'node:0011aabbccdd'], nodes), '1 server and all on vm-jeddah');
  assert.equal(scopeCount([NO_SERVERS], nodes), 'no servers');
  assert.match(scopeHint(['node:ab12cd34ef56'], servers, nodes), /made there later/);
});
