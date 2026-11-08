import test from 'node:test';
import assert from 'node:assert/strict';
import { showSourcesHint } from '../src/lib/sourcehint.js';

const base = { known: true, allowed: false, chip: 'unknown', unknown: 9, closed: false };

test('shows only for Unknown mods with mods from links off', () => {
  assert.equal(showSourcesHint(base), true);
});
test('stays away when it would not be true or useful', () => {
  assert.equal(showSourcesHint({ ...base, allowed: true }), false, 'sources on');
  assert.equal(showSourcesHint({ ...base, known: false }), false, 'setting unknown or not allowed to read');
  assert.equal(showSourcesHint({ ...base, unknown: 0 }), false, 'nothing unknown');
  assert.equal(showSourcesHint({ ...base, chip: 'all' }), false, 'other views');
  assert.equal(showSourcesHint({ ...base, closed: true }), false, 'dismissed');
});
