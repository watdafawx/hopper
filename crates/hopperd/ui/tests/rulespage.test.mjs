import test from 'node:test';
import assert from 'node:assert/strict';
import { startFilter, isOldRulesPage, showPart, matches, counts, failing, warningsMatch } from '../src/lib/rulesPage.js';

test('old settings links open the matching part', () => {
  assert.equal(startFilter('webhooks'), 'messages');
  assert.equal(startFilter('alerts'), 'warnings');
  assert.equal(startFilter('automations'), 'actions');
  assert.equal(startFilter('rules'), 'all');
  assert.equal(startFilter(''), 'all');
  assert.ok(isOldRulesPage('alerts') && !isOldRulesPage('rules'));
});

test('a filter shows its own part, All shows everything', () => {
  assert.ok(showPart('all', 'actions') && showPart('actions', 'actions'));
  assert.ok(!showPart('messages', 'actions'));
});

test('search needs every word, any case', () => {
  assert.ok(matches('', 'x'));
  assert.ok(matches('  ', 'x'));
  assert.ok(matches('DISCORD crash', 'My Discord', 'Started, Crashed'));
  assert.ok(!matches('discord backup', 'My Discord', 'Started, Crashed'));
  assert.ok(matches('x', null, undefined, 'xyz'));
});

test('warning search finds a warning or the card', () => {
  assert.ok(warningsMatch('', false));
  assert.ok(warningsMatch('tps', false));
  assert.ok(warningsMatch('alerts', false));
  assert.ok(!warningsMatch('node offline', false));
  assert.ok(warningsMatch('node offline', true));
  assert.ok(!warningsMatch('discord', true));
});

test('counts add up; warnings are zero until loaded', () => {
  assert.deepEqual(counts([{}, {}], { crash: true }, [{}], false), { all: 7, messages: 2, warnings: 4, actions: 1 });
  assert.equal(counts([], { crash: true }, [], true).warnings, 5);
  assert.equal(counts([{}], null, [], false).all, 1);
});

test('failing counts only what is on', () => {
  const hooks = [{ enabled: true, last_error: 'bad' }, { enabled: false, last_error: 'bad' }, { enabled: true }];
  const rules = [{ enabled: true, history: [{ ok: false }] }, { enabled: true, history: [{ ok: true }] }, { enabled: true, history: [] }];
  assert.equal(failing(hooks, rules), 2);
});
