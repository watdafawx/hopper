import test from 'node:test';
import assert from 'node:assert/strict';
import { order, group, mainClass } from '../src/lib/noticeorder.js';

const n = (tier, since = 0) => ({ tier, since });

test('stops come first, then choices, then news; newest first in each', () => {
  const items = [n('info', 50), n('stop', 10), n('choice', 99), n('stop', 20), n('info', 60)];
  assert.deepEqual(order(items), [3, 1, 2, 4, 0]);
});

test('ties keep the page order', () => {
  assert.deepEqual(order([n('choice'), n('choice'), n('stop'), n('choice')]), [2, 0, 1, 3]);
});

test('the list groups by tier, in order', () => {
  assert.deepEqual(group(['stop', 'stop', 'choice', 'choice', 'choice', 'info', 'info']), [['stop', [0, 1]], ['choice', [2, 3, 4]], ['info', [5, 6]]]);
  assert.deepEqual(group(['info']), [['info', [0]]]);
  assert.deepEqual(group([]), []);
});

test('only a lead notice gets the filled button', () => {
  assert.equal(mainClass({ lead: true }), 'btn sm pri');
  assert.equal(mainClass({ lead: false }), 'btn sm');
});
