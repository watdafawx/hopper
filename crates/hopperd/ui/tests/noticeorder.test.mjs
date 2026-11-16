import test from 'node:test';
import assert from 'node:assert/strict';
import { order, fold, mainClass } from '../src/lib/noticeorder.js';

const n = (tier, since = 0) => ({ tier, since });

test('stops come first, then choices, then news; newest first in each', () => {
  const items = [n('info', 50), n('stop', 10), n('choice', 99), n('stop', 20), n('info', 60)];
  assert.deepEqual(order(items), [3, 1, 2, 4, 0]);
});

test('ties keep the page order', () => {
  assert.deepEqual(order([n('choice'), n('choice'), n('stop'), n('choice')]), [2, 0, 1, 3]);
});

test('two show, the rest group by tier under "N more"', () => {
  const { shown, groups } = fold(['stop', 'stop', 'choice', 'choice', 'choice', 'info', 'info']);
  assert.deepEqual(shown, [0, 1]);
  assert.deepEqual(groups, [['choice', [2, 3, 4]], ['info', [5, 6]]]);
});

test('one or two notices have no list', () => {
  assert.deepEqual(fold(['info']), { shown: [0], groups: [] });
  assert.deepEqual(fold(['stop', 'info']), { shown: [0, 1], groups: [] });
  assert.deepEqual(fold([]), { shown: [], groups: [] });
});

test('a third stop is never dropped: it heads the list', () => {
  assert.deepEqual(fold(['stop', 'stop', 'stop', 'info']).groups, [['stop', [2]], ['info', [3]]]);
});

test('only the first strip gets the filled button', () => {
  assert.equal(mainClass({ lead: true }), 'btn sm pri');
  assert.equal(mainClass({ lead: false }), 'btn sm');
});
