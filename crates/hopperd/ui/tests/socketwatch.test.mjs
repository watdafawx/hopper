import test from 'node:test';
import assert from 'node:assert/strict';
import { createWatchdog, SILENT_AFTER } from '../src/lib/socketWatch.js';

// A fake clock: timers fire when `tick` moves past them.
function rig() {
  const r = { t: 0, next: 1, timers: new Map(), stale: 0 };
  const set = (fn, ms) => {
    const id = r.next++;
    r.timers.set(id, { at: r.t + ms, fn });
    return id;
  };
  const clear = (id) => r.timers.delete(id);
  r.tick = (ms) => {
    r.t += ms;
    for (const [id, x] of [...r.timers]) {
      if (x.at <= r.t) {
        r.timers.delete(id);
        x.fn();
      }
    }
  };
  r.dog = createWatchdog(() => r.stale++, { set, clear });
  return r;
}

test('a socket that says nothing is given up on', () => {
  const r = rig();
  r.dog.arm(SILENT_AFTER);
  r.tick(SILENT_AFTER - 1);
  assert.equal(r.stale, 0);
  r.tick(1);
  assert.equal(r.stale, 1);
});

test('every message from the panel restarts the wait', () => {
  const r = rig();
  r.dog.arm(SILENT_AFTER);
  for (let i = 0; i < 30; i++) {
    r.tick(1000); // a snapshot a second, for half a minute
    r.dog.arm(SILENT_AFTER);
  }
  assert.equal(r.stale, 0);
  r.tick(SILENT_AFTER);
  assert.equal(r.stale, 1, 'then it freezes');
});

test('a stopped watchdog never fires', () => {
  const r = rig();
  r.dog.arm(SILENT_AFTER);
  r.dog.stop();
  r.tick(SILENT_AFTER * 5);
  assert.equal(r.stale, 0);
});

test('it fires once, not again until armed again', () => {
  const r = rig();
  r.dog.arm(5000);
  r.tick(60000);
  assert.equal(r.stale, 1);
  r.dog.arm(5000);
  r.tick(5000);
  assert.equal(r.stale, 2);
});
