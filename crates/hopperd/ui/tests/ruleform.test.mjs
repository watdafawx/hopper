import test from 'node:test';
import assert from 'node:assert/strict';
import { blankForm, formFromHook, formFromRule, switchThen, hookBody, ruleBody, canSave, needsCooldown, formSentence, DEFAULT_MESSAGE_EVENTS } from '../src/lib/ruleForm.js';

const hookEvents = ['start', 'stop', 'crash', 'join'].map((id) => ({ id, label: id }));
const ruleEvents = [{ id: 'crash', label: 'The server crashed' }, { id: 'join', label: 'A player joined' }];

test('a blank form starts as a message with the usual events', () => {
  const f = blankForm();
  assert.equal(f.then, 'message');
  assert.deepEqual(f.events, DEFAULT_MESSAGE_EVENTS);
  assert.equal(blankForm('steps').then, 'steps');
});

test('editing keeps the store: a webhook stays a message, an automation stays steps', () => {
  const m = formFromHook({ id: 'h', name: 'D', url: 'https://x', events: ['crash'], servers: ['s'], enabled: false });
  assert.deepEqual([m.then, m.id, m.url, m.enabled], ['message', 'h', 'https://x', false]);
  const a = formFromRule({ id: 'r', name: 'R', enabled: true, servers: [], trigger: { kind: 'every', minutes: 5 }, only: { running: true }, steps: [{ kind: 'url', url: 'https://y' }], cooldown_minutes: 3 });
  assert.deepEqual([a.then, a.id, a.cooldown_minutes], ['steps', 'r', 3]);
  assert.deepEqual(a.only.days, []);
});

test('each store gets only its own fields', () => {
  const m = blankForm();
  m.url = 'https://discord.com/api/webhooks/1/x';
  assert.deepEqual(Object.keys(hookBody(m)).sort(), ['enabled', 'events', 'name', 'servers', 'url']);
  const a = blankForm('steps');
  a.name = 'N';
  a.trigger = { kind: 'tps', below: '12', minutes: '10' };
  a.steps = [{ kind: 'wait', seconds: '30' }];
  a.only.players_min = '';
  a.cooldown_minutes = '7';
  const b = ruleBody(a);
  assert.deepEqual([b.trigger.below, b.trigger.minutes, b.steps[0].seconds, b.only.players_min, b.cooldown_minutes], [12, 10, 30, null, 7]);
  assert.equal('url' in b, false);
});

test('switching to steps carries one picked event over', () => {
  const f = blankForm();
  f.events = ['crash'];
  switchThen(f, 'steps', ['crash', 'join']);
  assert.deepEqual([f.then, f.trigger], ['steps', { kind: 'event', event: 'crash' }]);
  const g = blankForm();
  switchThen(g, 'steps', ['crash']);
  assert.deepEqual(g.trigger, { kind: 'event', event: 'join' });
  switchThen(g, 'message', ['crash']);
  assert.equal(g.then, 'message');
});

test('what makes a form savable', () => {
  const m = blankForm();
  assert.equal(canSave(m, false, 5), false);
  m.url = 'https://x';
  assert.equal(canSave(m, false, 5), true);
  m.events = [];
  assert.equal(canSave(m, false, 5), false);
  const a = blankForm('steps');
  assert.equal(canSave(a, false, 5), false);
  a.name = 'N';
  assert.equal(canSave(a, false, 5), true);
  assert.equal(canSave(a, true, 5), false);
  assert.equal(needsCooldown(a, true, 5), true);
  a.cooldown_minutes = 5;
  assert.equal(canSave(a, true, 5), true);
  // A message never needs a cooldown.
  assert.equal(needsCooldown(m, true, 5), false);
});

test('the sentence at the top follows the form', () => {
  const m = blankForm();
  m.events = ['crash', 'start'];
  assert.deepEqual(formSentence(m, hookEvents, ruleEvents), { when: 'When a server starts or crashes', then: 'post to Discord' });
  m.url = 'https://example.com/hook';
  assert.equal(formSentence(m, hookEvents, ruleEvents).then, 'post to a URL');
  m.url = 'https://discord.com/api/webhooks/1/abc';
  assert.equal(formSentence(m, hookEvents, ruleEvents).then, 'post to Discord');
  const a = blankForm('steps');
  a.trigger = { kind: 'event', event: 'crash' };
  a.steps = [{ kind: 'power', action: 'restart' }];
  assert.deepEqual(formSentence(a, hookEvents, ruleEvents), { when: 'When the server crashed', then: 'restart the server' });
});
