import { test } from 'node:test';
import assert from 'node:assert/strict';
import { onOnly, offOnly, savedList, stuck } from '../src/lib/panelOff.js';

const p = (id, state, off = false) => ({ id, url: `http://${id}:8790`, name: id, state, off });

test('a panel turned off is never started, listed or asked again', async () => {
  const list = [p('rig', 'ok'), p('vm', 'off', true), p('a1', 'offline')];
  // What the page does at load and on refresh, with a counting fake.
  const asked = [];
  for (const q of onOnly(list)) asked.push(q.id);
  for (const q of stuck(list)) asked.push(`retry:${q.id}`);
  assert.deepEqual(asked, ['rig', 'a1', 'retry:a1']);
  assert.ok(!asked.some((a) => a.includes('vm')));
  assert.deepEqual(offOnly(list).map((q) => q.id), ['vm']);
});

test('refresh asks again only panels that are on and not answering', () => {
  const list = [p('a', 'reconnecting'), p('b', 'offline'), p('c', 'refused'), p('d', 'loading'), p('e', 'ok'), p('f', 'signin'), p('g', 'offline', true)];
  assert.deepEqual(stuck(list).map((q) => q.id), ['a', 'b', 'c', 'd']);
});

test('the saved list keeps off, and on panels save as before', () => {
  assert.deepEqual(savedList([p('rig', 'ok'), p('vm', 'off', true)]), [
    { url: 'http://rig:8790', name: 'rig' },
    { url: 'http://vm:8790', name: 'vm', off: true },
  ]);
});
