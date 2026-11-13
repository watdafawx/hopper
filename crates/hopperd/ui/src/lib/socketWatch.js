// A watchdog for another panel's live socket (lib/panels.svelte.js). A panel
// that freezes (its process hangs, or the network drops without a word) keeps
// the connection open, so the browser never says "closed" and the panel would
// stay green with stale servers. The panel sends its servers about once a
// second, so a socket that has gone quiet is a dead one: give up on it
// ourselves instead of waiting for a close that may take minutes.
// Plain functions with the clock passed in, so tests can run them without
// the page.

/** An open socket that says nothing for this long is given up on. */
export const SILENT_AFTER = 10000;

/**
 * `onStale()` runs once the timer set by `arm(ms)` runs out. Arming again
 * restarts it (every message from the panel does that); `stop()` cancels it.
 * `set` / `clear` are the timer functions; the tests give their own.
 */
export function createWatchdog(onStale, { set = setTimeout, clear = clearTimeout } = {}) {
  let timer = 0;
  return {
    arm(ms) {
      clear(timer);
      timer = set(() => {
        timer = 0;
        onStale();
      }, ms);
    },
    stop() {
      clear(timer);
      timer = 0;
    },
  };
}
