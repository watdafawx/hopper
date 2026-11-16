import test from 'node:test';
import assert from 'node:assert/strict';
import { twiceOf, olderOf, clientOnOf, briefNames } from '../src/lib/modsnotes.js';

const m = (project_id, file, extra = {}) => ({ project_id, file, name: `mod ${project_id}`, source: 'curseforge', enabled: true, ...extra });

test('copies of one mod group; unknown jars and single mods do not', () => {
  const mods = [m('1', 'a-1.jar'), m('1', 'a-2.jar'), m('2', 'b.jar'), m('0', 'x.jar'), m('0', 'y.jar'), m('3', 'c.jar', { source: '' }), m('3', 'c2.jar', { source: '' })];
  assert.deepEqual(twiceOf(mods).map((l) => l.map((x) => x.file)), [['a-1.jar', 'a-2.jar']]);
});

test('keep newest keeps the turned-on copy, else the newest file', () => {
  const on = [m('1', 'old.jar', { file_date: '2026-01-01' }), m('1', 'new.jar', { file_date: '2026-05-01', enabled: false })];
  assert.deepEqual(olderOf([on]).map((x) => x.file), ['new.jar']);
  const both = [m('2', 'old.jar', { file_date: '2026-01-01' }), m('2', 'new.jar', { file_date: '2026-05-01' })];
  assert.deepEqual(olderOf([both]).map((x) => x.file), ['old.jar']);
});

test('client-only mods that are on are counted', () => {
  assert.equal(clientOnOf([m('1', 'a', { side: 'client' }), m('2', 'b', { side: 'client', enabled: false }), m('3', 'c', { side: 'both' })]), 1);
});

test('names: three listed, more fold into a count, repeats once', () => {
  assert.equal(briefNames(['A', 'B', 'A', 'C']), 'A, B, C');
  assert.equal(briefNames(['A', 'B', 'C', 'D', 'E']), 'A, B and 3 more');
  assert.equal(briefNames(['', 'A']), 'A');
});
