// Hopper Server's own version and updates, shared by the version beside the
// logo, its versions window and Settings › Updates (GET /panel/update).
// Looks every minute, every second and a half while an update runs; once
// the panel answers again after an update restart, boot.svelte.js reloads
// the page (the new version brings its own web UI).
import { api } from './api.js';
import { check, expectRestart } from './boot.svelte.js';
import { choose } from './dialog.svelte.js';

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

/**
 * Before an update starts: on Linux a restarted panel can't take over
 * servers an older panel started (`running_servers` lists only those), so
 * with some running it asks. Resolves to
 * `{ stop }` (stop them first, or wait until they're stopped), or null when
 * cancelled. Nothing running: `{ stop: false }` without asking.
 */
export async function askAboutServers(st) {
  const names = st?.running_servers || [];
  if (!names.length) return { stop: false };
  const list = names.length > 3 ? `${names.slice(0, 3).join(', ')} and ${names.length - 3} more` : names.join(', ');
  const r = await choose(`${names.length === 1 ? '1 server is' : `${names.length} servers are`} running`, {
    body: `${list}. If the panel restarted now it would lose its console and controls for ${names.length === 1 ? 'it' : 'them'} until ${names.length === 1 ? 'it is' : 'they are'} started again. Stop ${names.length === 1 ? 'it' : 'them'} first and the panel updates once ${names.length === 1 ? 'it has' : 'they have'} stopped and saved (start ${names.length === 1 ? 'it' : 'them'} again afterwards), or let the update wait until you stop ${names.length === 1 ? 'it' : 'them'} yourself.`,
    ok: 'Stop servers and update',
    alt: 'Wait for them to stop',
  });
  return r ? { stop: r === 'ok' } : null;
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
