import test from 'node:test';
import assert from 'node:assert/strict';
import { serverLook, merged, plan, maxLevel, levelFor } from '../src/lib/syncchip.js';

test('server words match desktop', () => {
  assert.equal(serverLook({ kind: 'differ', mods: 3 }).label, '3 mods differ');
  assert.equal(serverLook({ kind: 'differ', mods: 1 }).short, '1 differs');
  assert.equal(serverLook({ kind: 'in-sync' }, 'Skyblock Server').label, 'In sync with Skyblock Server');
  assert.equal(serverLook({ kind: 'working', work: 'pushing' }, 'X').label, 'Pushing to X');
  const f = serverLook({ kind: 'failed' });
  assert.ok(f.fixed && f.tone === 'bad');
  assert.ok(serverLook({ kind: 'comparing' }).busy);
});

test('merged takes the worse tone and fixed', () => {
  const m = merged(serverLook({ kind: 'in-sync' }), serverLook({ kind: 'failed' }));
  assert.deepEqual([m.tone, m.label, m.fixed], ['bad', 'Sync', true]);
});

test('one chip folds full, short, icon, +N', () => {
  const fx = [false];
  assert.equal(maxLevel(fx, 0), 3);
  assert.deepEqual(plan(fx, 0, 0).forms, ['full']);
  assert.deepEqual(plan(fx, 0, 1).forms, ['short']);
  assert.deepEqual(plan(fx, 0, 2).forms, ['icon']);
  const p = plan(fx, 0, 3);
  assert.deepEqual([p.forms[0], p.chipsFolded], [null, 1]);
});

test('a fixed chip never folds into +N', () => {
  assert.equal(maxLevel([true], 0), 2);
  assert.deepEqual(plan([true], 0, 9).forms, ['icon']);
});

test('two chips merge before folding', () => {
  const fx = [false, false];
  assert.deepEqual(plan(fx, 0, 1).forms, ['short', 'short']);
  const m = plan(fx, 0, 2);
  assert.deepEqual([m.forms, m.merged], [[null, null], 'short']);
  assert.equal(plan(fx, 0, 3).merged, 'icon');
  assert.equal(plan(fx, 0, 4).chipsFolded, 2);
  assert.equal(maxLevel([false, true], 0), 3);
});

test('levelFor picks the first level that fits', () => {
  const a = serverLook({ kind: 'differ', mods: 3 });
  assert.equal(levelFor([a], 400), 0);
  assert.ok(levelFor([a], 60) >= 2);
  assert.equal(levelFor([serverLook({ kind: 'failed' })], 10), 2);
});
