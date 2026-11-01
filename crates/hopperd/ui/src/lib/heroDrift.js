// The header backdrop's link to the page: an action "kicks" the drift so it
// bends for a moment, and a running job keeps it moving faster. HeroDrift.svelte
// listens; with no header on screen a kick does nothing.
const listeners = new Set();
let jobs = 0;

/** Bend the motion: n = strength (about 1 for a click), dir = -1, 0 or 1 (which way a tab switch went). */
export function kick(n = 1, dir = 0) {
  for (const f of listeners) f(n, dir);
}
/** A task or progress began (kicks 1.2 and keeps the drift fast until jobEnded). */
export function jobStarted() {
  jobs++;
  kick(1.2, 0);
}
/** It finished (kicks 2.2). */
export function jobEnded() {
  jobs = Math.max(0, jobs - 1);
  kick(2.2, 0);
}
export const jobRunning = () => jobs > 0;
/** For the backdrop: call back on every kick; returns the unsubscribe. */
export function onKick(f) {
  listeners.add(f);
  return () => listeners.delete(f);
}
