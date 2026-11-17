import test from 'node:test';
import assert from 'node:assert/strict';
import { startFilter, isOldRulesPage, matches } from '../src/lib/rulesPage.js';

test('old settings links open the matching filter', () => {
  assert.equal(startFilter('webhooks'), 'messages');
  assert.equal(startFilter('alerts'), 'warnings');
  assert.equal(startFilter('automations'), 'actions');
  assert.equal(startFilter('rules'), 'all');
  assert.equal(startFilter(''), 'all');
  assert.equal(startFilter('actions'), 'actions');
  assert.ok(isOldRulesPage('alerts') && !isOldRulesPage('rules'));
});

test('search needs every word, any case', () => {
  assert.ok(matches('', 'x'));
  assert.ok(matches('  ', 'x'));
  assert.ok(matches('DISCORD crash', 'My Discord', 'Started, Crashed'));
  assert.ok(!matches('discord backup', 'My Discord', 'Started, Crashed'));
  assert.ok(matches('x', null, undefined, 'xyz'));
});
