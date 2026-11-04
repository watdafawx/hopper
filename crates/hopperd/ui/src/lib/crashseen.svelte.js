// Which server has a crash its Logs & crashes tab hasn't shown yet: the
// tab's red dot. `crashes[id]` is the crash's time (set by CrashStrip);
// opening Logs & crashes marks it seen, per crash, in this browser.
export const crashes = $state({});
const mark = (id, when) => `hopper.logs-seen.${id}.${when}`;

export function unseen(id) {
  const when = crashes[id];
  if (!when) return false;
  try {
    return localStorage.getItem(mark(id, when)) !== '1';
  } catch {
    return true;
  }
}

export function markSeen(id) {
  const when = crashes[id];
  if (!when) return;
  try {
    localStorage.setItem(mark(id, when), '1');
  } catch {}
  crashes[id] = when;
}
