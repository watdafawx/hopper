import test from 'node:test';
import assert from 'node:assert/strict';
import { eventsPhrase, stepsPhrase, plain, buildItems, visibleItems, countItems, failingItems, serverLabel, settingsMatch } from '../src/lib/rulesList.js';

const hookEvents = ['start', 'stop', 'crash', 'join', 'leave', 'backup', 'wake', 'sleep', 'schedule', 'problem', 'tps', 'disk', 'ram', 'copy_failed', 'node'].map((id) => ({ id, label: id }));
const ruleEvents = [{ id: 'crash', label: 'The server crashed' }, { id: 'join', label: 'A player joined' }];
const settings = { crash: true, tps_on: true, tps_below: 15, tps_minutes: 2, disk_on: false, disk_gb: 10, ram_on: true, ram_pct: 90, ram_minutes: 5, node_on: true, node_minutes: 5, auto_rcon: false };
const hook = (o = {}) => ({ id: 'h1', name: 'Team Discord', url: 'https://x', kind: 'discord', events: ['crash'], servers: [], enabled: true, ...o });
const rule = (o = {}) => ({
  id: 'r1', name: 'Restart when it lags', enabled: true, servers: [], trigger: { kind: 'tps', below: 12, minutes: 10 }, only: {},
  steps: [{ kind: 'power', action: 'restart' }], cooldown_minutes: 0, history: [], ...o,
});

test('message events read as one sentence, grouped by who does it', () => {
  assert.equal(eventsPhrase(['crash'], hookEvents), 'a server crashes');
  assert.equal(eventsPhrase(['crash', 'start', 'stop'], hookEvents), 'a server starts, stops or crashes');
  assert.equal(eventsPhrase(['leave', 'start', 'join'], hookEvents), 'a server starts, or a player joins or leaves');
});

test('a long event list says how many more', () => {
  const all = hookEvents.map((e) => e.id);
  assert.equal(eventsPhrase(all, hookEvents), 'a server starts, stops or crashes, or a player joins, or 11 more events');
  assert.equal(eventsPhrase(['start', 'stop', 'crash', 'join', 'leave'], hookEvents), 'a server starts, stops or crashes, or a player joins, or 1 more event');
});

test('an unknown event and an empty list still make a sentence', () => {
  assert.equal(eventsPhrase(['nope'], []), 'nope happens');
  assert.equal(eventsPhrase([], hookEvents), 'nothing happens');
});

test('steps read as the end of a sentence', () => {
  assert.equal(stepsPhrase([{ kind: 'power', action: 'restart' }]), 'restart the server');
  assert.equal(stepsPhrase([{ kind: 'command', line: 'say hi' }, { kind: 'wait', seconds: 30 }, { kind: 'backup', worlds_only: true }]), 'run “say hi”, then wait 30 s, then back up worlds');
  assert.equal(stepsPhrase([{ kind: 'url', url: 'https://example.com/x' }]), 'call example.com');
  // Still being typed.
  assert.equal(stepsPhrase([{ kind: 'command', line: ' ' }, { kind: 'url', url: '' }]), 'run a console command, then call a URL');
});

test('a message row: trigger and outcome bold, Discord or URL', () => {
  const [m] = buildItems({ hooks: [hook({ events: ['start', 'stop', 'crash'] })], hookEvents });
  assert.equal(plain(m.parts), 'When a server starts, stops or crashes, post to Discord');
  assert.deepEqual(m.parts.filter((p) => p.b).map((p) => p.text), ['a server starts, stops or crashes', 'post to Discord']);
  const [j] = buildItems({ hooks: [hook({ kind: 'json' })], hookEvents });
  assert.match(plain(j.parts), /post to a URL$/);
});

test('an action row reuses the When and step words', () => {
  const [a] = buildItems({ rules: [rule()], ruleEvents });
  assert.equal(plain(a.parts), 'When TPS is below 12 for 10 min, restart the server');
  const [e] = buildItems({ rules: [rule({ trigger: { kind: 'event', event: 'crash' } })], ruleEvents });
  assert.equal(plain(e.parts), 'When the server crashed, restart the server');
  const [t] = buildItems({ rules: [rule({ trigger: { kind: 'daily', at: '04:00' }, steps: [{ kind: 'backup', worlds_only: true }] })], ruleEvents });
  assert.equal(plain(t.parts), 'Every day at 04:00, back up worlds');
  assert.equal(t.parts[0].b, true);
});

test('warning rows: the five built-ins, node only when there are nodes, numbers editable', () => {
  const w = buildItems({ settings, hasNodes: false });
  assert.deepEqual(w.map((i) => i.id), ['crash', 'tps', 'disk', 'ram']);
  assert.equal(buildItems({ settings, hasNodes: true }).length, 5);
  assert.equal(plain(w[1].parts), 'When TPS is below 15 for 2 min, warn on the server');
  assert.deepEqual(w[1].parts.find((p) => p.field), { field: 'tps_below', value: 15, min: 1, max: 20, step: 0.5, b: true });
  assert.equal(w[2].enabled, false);
  assert.equal(w[0].enabled, true);
  assert.equal(plain(w[3].parts), 'When PC memory is above 90% for 5 min, warn on the server');
  assert.deepEqual(buildItems({ settings: null }), []);
});

test('the list is messages, then warnings, then actions, each in store order', () => {
  const items = buildItems({ hooks: [hook({ id: 'a' }), hook({ id: 'b' })], rules: [rule({ id: 'x' }), rule({ id: 'y' })], settings, hookEvents, ruleEvents });
  assert.deepEqual(items.map((i) => i.kind), ['message', 'message', 'warning', 'warning', 'warning', 'warning', 'action', 'action']);
  assert.deepEqual(items.filter((i) => i.kind !== 'warning').map((i) => i.id), ['a', 'b', 'x', 'y']);
});

test('server chip: all, one name, a count', () => {
  const servers = [{ id: 's1', name: 'Survival' }, { id: 's2', name: 'Creative' }];
  assert.equal(serverLabel([], servers), 'All servers');
  assert.equal(serverLabel(['s2'], servers), 'Creative');
  assert.equal(serverLabel(['gone'], servers), 'gone');
  assert.equal(serverLabel(['s1', 's2'], servers), '2 servers');
});

test('filter and search work on the merged list', () => {
  const items = buildItems({ hooks: [hook()], rules: [rule()], settings, hookEvents, ruleEvents, hasNodes: true });
  assert.equal(visibleItems(items, 'all', '').length, 7);
  assert.deepEqual(visibleItems(items, 'messages', '').map((i) => i.kind), ['message']);
  assert.equal(visibleItems(items, 'warnings', '').length, 5);
  assert.deepEqual(visibleItems(items, 'actions', 'restart').map((i) => i.id), ['r1']);
  assert.deepEqual(visibleItems(items, 'all', 'discord').map((i) => i.id), ['h1']);
  assert.deepEqual(visibleItems(items, 'all', 'warning tps').map((i) => i.id), ['tps']);
  assert.equal(visibleItems(items, 'all', 'node').length, 1);
  assert.equal(visibleItems(items, 'actions', 'discord').length, 0);
  // A search can't find a node warning while there are no nodes.
  assert.equal(visibleItems(buildItems({ settings, hasNodes: false }), 'all', 'node').length, 0);
});

test('counts add up; warnings are zero until the settings load', () => {
  const items = buildItems({ hooks: [hook(), hook({ id: 'h2' })], rules: [rule()], settings, hookEvents, ruleEvents });
  assert.deepEqual(countItems(items), { all: 7, messages: 2, warnings: 4, actions: 1 });
  assert.deepEqual(countItems(buildItems({ hooks: [hook()], settings: null, hookEvents })), { all: 1, messages: 1, warnings: 0, actions: 0 });
});

test('failing counts only what is on', () => {
  const items = buildItems({
    hooks: [hook({ last_error: 'HTTP 404' }), hook({ id: 'h2', last_error: 'bad', enabled: false }), hook({ id: 'h3' })],
    rules: [rule({ history: [{ ok: false, text: 'x', at: 1, server: 's' }] }), rule({ id: 'r2', history: [{ ok: true, text: 'y', at: 1, server: 's' }] }), rule({ id: 'r3' })],
    hookEvents, ruleEvents,
  });
  assert.equal(failingItems(items), 2);
  assert.equal(items.find((i) => i.id === 'h1').last.text, 'HTTP 404');
});

test('the warning settings part shows for a search about it', () => {
  assert.ok(settingsMatch('', true));
  assert.ok(settingsMatch('rcon', true));
  assert.ok(!settingsMatch('discord', true));
  assert.ok(!settingsMatch('', false));
});
