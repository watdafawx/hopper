import test from 'node:test';
import assert from 'node:assert/strict';
import { seedBrowse, takeBrowseSeed } from '../src/lib/browse.js';

test('the Browse seed is taken once', () => {
  seedBrowse('paxi');
  assert.equal(takeBrowseSeed(), 'paxi');
  assert.equal(takeBrowseSeed(), '');
});
