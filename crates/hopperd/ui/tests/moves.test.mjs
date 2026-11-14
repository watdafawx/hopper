import { test } from 'node:test';
import assert from 'node:assert/strict';
import { active, resumable, percent, amount, title, resumedLine, goLabel, movedLine } from '../src/lib/moves.js';

const m = { name: 'Test SMP', to_name: 'vm-a', mode: 'move', state: 'sending', done: 512 * 1024 * 1024, total: 1024 * 1024 * 1024, resumed: 0 };

test('progress reads as a share and in bytes', () => {
  assert.equal(percent(m), 50);
  assert.match(amount(m), /^512(\.0)? MB of 1(\.0)? GB$/);
  assert.equal(percent({ ...m, state: 'finishing' }), 100);
  assert.equal(amount({ ...m, state: 'done' }), '');
  assert.equal(percent({ ...m, total: 0 }), 0);
  assert.equal(percent(null), 0);
});

test('titles say move or copy and how it ended', () => {
  assert.equal(title(m), 'Moving Test SMP to vm-a');
  assert.equal(title({ ...m, mode: 'copy' }), 'Copying Test SMP to vm-a');
  assert.equal(title({ ...m, state: 'done' }), 'Moved Test SMP to vm-a');
  assert.equal(title({ ...m, state: 'failed', mode: 'copy' }), "Couldn't copy Test SMP");
  assert.equal(title({ ...m, state: 'cancelled' }), 'Move cancelled');
});

test('states that keep polling or can go on', () => {
  for (const s of ['preparing', 'sending', 'waiting', 'finishing']) assert.ok(active({ state: s }), s);
  for (const s of ['paused', 'failed', 'cancelled', 'done']) assert.ok(!active({ state: s }), s);
  assert.ok(resumable({ state: 'paused' }));
  assert.ok(!resumable({ state: 'failed' }));
});

test('a resumed move says so once', () => {
  assert.equal(resumedLine(m), '');
  assert.match(resumedLine({ ...m, resumed: 1 }), /^Went on once after/);
  assert.match(resumedLine({ ...m, resumed: 3 }), /^Went on 3 times after/);
});

test('button and banner wording', () => {
  assert.equal(goLabel('move', { name: 'vm-a' }), 'Move to vm-a');
  assert.equal(goLabel('copy', { name: 'fox' }), 'Copy to fox');
  assert.equal(goLabel('move', null), 'Move to …');
  assert.equal(movedLine({ to: 'vm-a', at: 1 }, () => '14 Nov'), 'Moved to vm-a on 14 Nov. This copy is stopped and kept.');
  assert.equal(movedLine(null), '');
});
