import test from 'node:test';
import assert from 'node:assert/strict';
import { serverLook, merged, plan, maxLevel, levelFor, attentionLook, restartLook } from '../src/lib/syncchip.js';

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

test('needs-you chip: words, tone, folding (as desktop)', () => {
  assert.equal(attentionLook([]), null);
  const notes = attentionLook(['info', 'info']);
  assert.deepEqual([notes.label, notes.tone, notes.fixed], ['2 notes', 'quiet', false]);
  assert.equal(attentionLook(['info']).label, '1 note');
  const one = attentionLook(['stop'], '1 required mod is missing');
  assert.deepEqual([one.label, one.short, one.tone, one.fixed], ['1 required mod is missing', 'Stops play', 'bad', true]);
  const many = attentionLook(['choice', 'stop', 'info'], 'x');
  assert.deepEqual([many.label, many.short, many.tone], ['3 need you', 'Needs you', 'bad']);
  assert.equal(attentionLook(['choice'], 'Client-only mods on').tone, 'warn');
  const long = attentionLook(['stop'], 'A very long title about a mod that is missing');
  assert.ok([...long.label].length <= 28 && long.label.endsWith('…'), long.label);
  // Beside a sync chip: a fixed one keeps every chip, a quiet one folds with them.
  assert.equal(maxLevel([false], 0, true), 2);
  assert.deepEqual(plan([false], 0, 9, true).forms, ['icon']);
  const sync = serverLook({ kind: 'in-sync' });
  assert.ok(levelFor([sync, one], 1) <= 2);
  assert.ok(levelFor([sync, notes], 1) >= 3);
  assert.ok(levelFor([restartLook(true), one], 1e4) === 0);
});
