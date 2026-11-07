import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { checkSource } from '../scripts/check-effects.mjs';

const fx = (n) => checkSource(fs.readFileSync(new URL(`./fixtures/${n}`, import.meta.url), 'utf8'), n);

test('the Sleep loop is flagged', () => {
  const f = fx('sleep-loop.svelte');
  assert.equal(f.length, 1);
  assert.equal(f[0].kind, 'loop');
  assert.match(f[0].msg, /`info`/);
});

test('untrack and a loop-safe note pass', () => {
  assert.deepEqual(fx('sleep-fixed.svelte'), []);
});

test('reading the live server object is flagged', () => {
  assert.deepEqual(fx('live-prop.svelte').map((f) => f.kind), ['prop']);
});

test('a bare load() with no key is flagged, onChange is not', () => {
  const f = fx('no-key.svelte');
  assert.deepEqual(f.map((x) => x.kind), ['key']);
});

test('the real code base is clean', async () => {
  const { checkFiles } = await import('../scripts/check-effects.mjs');
  assert.deepEqual(checkFiles([new URL('../src', import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1')]), []);
});
