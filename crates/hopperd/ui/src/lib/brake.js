// A brake for pages that ask for the same thing over and over (an effect
// that reloads whatever it just loaded). The panel answers "Too many
// requests" after 1,200 a minute; this stops the page well before that.
//
//   - more than LOOP_AT identical GETs within LOOP_MS: the extra ones are
//     refused here, never sent, and the console says `request loop?: ...`;
//   - more than TOAST_AT of them within TOAST_MS (the page kept going
//     after being refused): one visible toast, once.
//
// Polls that are meant to be fast (a job's progress) opt out with
// `api(path, { poll: true })`. The panel's live feed isn't an API call, so it
// never comes through here.

export const LOOP_AT = 10;
export const LOOP_MS = 5000;
export const TOAST_AT = 30;
export const TOAST_MS = 10000;

/**
 * `now` is the clock (milliseconds), `onLoop(text)` the console line and
 * `onToast(text)` the toast; the tests give their own.
 */
export function createBrake({ now = () => Date.now(), onLoop = () => {}, onToast = () => {} } = {}) {
  /** url -> times of its recent GETs (attempts, refused ones too). */
  const seen = new Map();
  let toasted = false;

  return {
    /** True when this GET may go out; false when it's refused as a loop. */
    allow(url) {
      const t = now();
      const times = (seen.get(url) || []).filter((x) => t - x < TOAST_MS);
      times.push(t);
      seen.set(url, times);
      if (seen.size > 500) {
        for (const [k, v] of seen) if (t - v[v.length - 1] >= TOAST_MS) seen.delete(k);
      }
      const recent = times.filter((x) => t - x < LOOP_MS).length;
      if (times.length > TOAST_AT && !toasted) {
        toasted = true;
        onToast('This page is asking too often, paused it');
      }
      if (recent > LOOP_AT) {
        // One console line per refused batch, not one per request.
        if (recent === LOOP_AT + 1 || recent % 50 === 0) onLoop(`request loop?: GET ${url} x ${recent} in ${LOOP_MS / 1000} s`);
        return false;
      }
      return true;
    },
  };
}
