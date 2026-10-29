// Hopper Server's own version and updates, shared by the version beside the
// logo, its versions window and Settings › Updates (GET /panel/update).
// Looks every minute, every second and a half while an update runs; once
// the panel answers again after an update restart, boot.svelte.js reloads
// the page (the new version brings its own web UI).
import { api } from './api.js';
import { check, expectRestart } from './boot.svelte.js';

export const upd = $state({ st: null, open: false });

/** Whether version `a` is newer than `b` (0.13.10 > 0.13.9), as hopper-core's is_newer. */
export function isNewer(a, b) {
  const p = (v) => String(v || '').split(/[^0-9]+/).filter(Boolean).map(Number);
  const x = p(a);
  const y = p(b);
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    const d = (x[i] || 0) - (y[i] || 0);
    if (d) return d > 0;
  }
  return false;
}

/** States while an update runs. */
export const BUSY = ['checking', 'downloading', 'waiting', 'restarting'];

let timer = null;
let users = 0;
/** The last look failed while an update was restarting the panel. */
let away = false;

export async function loadUpdate() {
  clearTimeout(timer);
  let lost = false;
  try {
    upd.st = await api('/panel/update');
    if (upd.st.state === 'restarting') expectRestart();
    if (away) {
      away = false;
      check();
    }
  } catch {
    // Restarting into the new version: keep looking until it answers.
    lost = upd.st?.state === 'restarting' || upd.st?.state === 'waiting';
    if (lost) expectRestart();
    away = lost;
  }
  if (!users) return;
  const busy = lost || BUSY.includes(upd.st?.state);
  timer = setTimeout(loadUpdate, busy ? 1500 : 60000);
}

/** Starts following the state; returns the stop for onDestroy / $effect. */
export function followUpdate() {
  users += 1;
  if (users === 1 || !upd.st) loadUpdate();
  return () => {
    users -= 1;
    if (!users) clearTimeout(timer);
  };
}
