import test from 'node:test';
import assert from 'node:assert/strict';
import { createBrake, LOOP_AT, LOOP_MS, TOAST_AT } from '../src/lib/brake.js';

function rig() {
  const r = { t: 0, loops: [], toasts: [] };
  r.brake = createBrake({ now: () => r.t, onLoop: (x) => r.loops.push(x), onToast: (x) => r.toasts.push(x) });
  return r;
}

test('a loop is cut at the limit and says so once', () => {
  const r = rig();
  let sent = 0;
  for (let i = 0; i < 40; i++) {
    r.t += 50; // 20 a second, like the Sleep loop on a LAN
    if (r.brake.allow('/servers/a/address')) sent++;
  }
  assert.equal(sent, LOOP_AT);
  assert.equal(r.loops.length, 1);
  assert.match(r.loops[0], /^request loop\?: GET \/servers\/a\/address x 11 in 5 s$/);
});

test('a toast comes only after the page keeps going', () => {
  const r = rig();
  for (let i = 0; i < TOAST_AT; i++) {
    r.t += 100;
    r.brake.allow('/x');
  }
  assert.equal(r.toasts.length, 0, '30 in 3 s is not past the line yet');
  r.t += 100;
  r.brake.allow('/x');
  r.brake.allow('/x');
  assert.deepEqual(r.toasts, ['This page is asking too often, paused it']);
});

test('normal use passes: a 2 s poll, 1 per second, other urls', () => {
  const r = rig();
  for (let i = 0; i < 100; i++) {
    r.t += 1000;
    assert.ok(r.brake.allow('/servers/a/logs'));
    assert.ok(r.brake.allow(`/other/${i}`));
  }
  assert.equal(r.loops.length + r.toasts.length, 0);
});

test('it lets go once the page calms down', () => {
  const r = rig();
  for (let i = 0; i < 20; i++) r.brake.allow('/x');
  assert.equal(r.brake.allow('/x'), false);
  r.t += LOOP_MS + 1;
  assert.equal(r.brake.allow('/x'), true);
});
