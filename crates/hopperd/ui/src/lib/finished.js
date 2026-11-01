// "TNP-LL8 started in 1m 32s": one toast when a start or stop ends, from
// the live feed's `finished` (whoever clicked, here or in desktop Hopper).
// The stop half of a restart stays quiet; its start says it.
import { say } from './toast.svelte.js';
import { exactTime } from './format.js';

/** The last finish toasted per server id. */
const last = new Map();

export function noteFinished(servers) {
  for (const s of servers) {
    const f = s.finished;
    if (!f) continue;
    const key = `${f.kind}:${f.secs}`;
    if (last.get(s.id) === key) continue;
    last.set(s.id, key);
    // Old news when the page opened, or the stop before a restart's start.
    if (f.ago_secs > 8 || (f.kind === 'stop' && (f.restarting || s.restarting))) continue;
    say(`${s.name} ${f.kind === 'start' ? 'started' : 'stopped'} in ${exactTime(f.secs)}.`);
  }
}
