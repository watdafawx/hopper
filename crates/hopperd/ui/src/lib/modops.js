// Mod actions shared by the Mods table, its menu, the bulk bar, the side
// panel and the Versions dialog, so each one asks and words things the same
// way (as Hopper desktop does).
import { api, post } from './api.js';
import { say } from './toast.svelte.js';
import { confirmIt, choose } from './dialog.svelte.js';
import { followJob } from './jobs.js';

const what = (mods) => (mods.length === 1 ? mods[0].name : `${mods.length} mods`);
const restart = (alive) => (alive ? ' Restart the server to apply.' : '');

/**
 * Turns mods on or off. Turning off, the mods that need them are asked
 * about; turning on, the mods they need that are off. True when anything
 * changed.
 */
export async function setOn(base, mods, on, alive) {
  let files = mods.map((m) => m.file);
  try {
    const { mods: rel } = await post(`${base}/mods/related`, { files, on });
    if (rel.length) {
      const n = rel.length;
      const names = rel.map((r) => r.name).join(', ');
      const w = what(mods);
      const pick = await choose(on ? `Turn on ${w}?` : `Turn off ${w}?`, {
        body: on
          ? `${w} needs ${names}, which ${n === 1 ? 'is' : 'are'} off. The server stops at loading without ${n === 1 ? 'it' : 'them'}.`
          : `${n === 1 ? 'This mod needs' : 'These mods need'} ${w}: ${names}. The server stops at loading while a mod it needs is off.`,
        ok: `Turn ${on ? 'on' : 'off'} all ${n + files.length}`,
        alt: `Only ${w}`,
      });
      if (!pick) return false;
      if (pick === 'ok') files = [...files, ...rel.map((r) => r.file)];
    }
    const r = await post(`${base}/mods/toggle-many`, { files, enabled: on });
    if (r.restart_needed) say(`Turned ${on ? 'on' : 'off'} ${r.changed === 1 ? '1 mod' : `${r.changed} mods`}.${restart(true)}`);
    return true;
  } catch (e) {
    say(e.message, true);
    return false;
  }
}

/** Removes mods (kept in .hopper-backups), warning about mods that need them. */
export async function removeMods(base, mods, alive) {
  const files = mods.map((m) => m.file);
  let need = [];
  try {
    need = (await post(`${base}/mods/related`, { files, on: false })).mods;
  } catch {
    // The question still makes sense without the list.
  }
  const w = what(mods);
  const ok = await confirmIt(`Remove ${w}?`, {
    body: `${mods.length === 1 ? "It's" : "They're"} kept in .hopper-backups (Mods › Backups).${need.length ? ` ${need.map((n) => n.name).join(', ')} ${need.length === 1 ? 'needs' : 'need'} ${mods.length === 1 ? 'it' : 'them'}: the server stops at loading without.` : ''}`,
    ok: 'Remove',
    danger: true,
  });
  if (!ok) return false;
  try {
    const r = await post(`${base}/mods/remove-many`, { files });
    say(`Removed ${r.removed === 1 ? w : `${r.removed} mods`}. Put back from Mods › Backups.${restart(alive)}`);
    return true;
  } catch (e) {
    say(e.message, true);
    return false;
  }
}

/** Ignores or allows updates (desktop's lock). */
export async function setLocked(base, mods, locked) {
  try {
    await post(`${base}/mods/lock-many`, { files: mods.map((m) => m.file), locked });
    const w = what(mods);
    say(locked ? `${w} ${mods.length === 1 ? 'stays' : 'stay'} on this version: update checks skip ${mods.length === 1 ? 'it' : 'them'}.` : `${w} ${mods.length === 1 ? 'gets' : 'get'} updates again.`);
    return true;
  } catch (e) {
    say(e.message, true);
    return false;
  }
}

/** Moves a mod's config files to .hopper-backups so it writes fresh ones. */
export async function resetConfig(base, mod) {
  try {
    const { files } = await api(`${base}/mods/config?file=${encodeURIComponent(mod.file)}`);
    if (!files.length) return say(`No config files named after ${mod.name} in config/.`);
    const ok = await confirmIt(`Reset ${mod.name}'s config?`, {
      body: `${files.length === 1 ? 'This file goes' : `These ${files.length} files go`} to .hopper-backups: ${files.join(', ')}. The mod writes fresh ones on the next start.`,
      ok: 'Move to backups',
      danger: true,
    });
    if (!ok) return;
    const r = await post(`${base}/mods/config-reset`, { file: mod.file });
    say(`Moved ${r.reset} file${r.reset === 1 ? '' : 's'} to .hopper-backups.${r.restart_needed ? ' Restart the server to apply.' : ''}`);
  } catch (e) {
    say(e.message, true);
  }
}

/** The same mod from the other site; the old jar goes to Backups. */
export async function switchSource(base, mod, alive, onStatus = () => {}) {
  try {
    const { job } = await post(`${base}/mods/switch`, { file: mod.file });
    const r = await followJob(job, (st) => onStatus(st.detail || `Switching ${mod.name}`));
    say(`${r.message}${restart(alive)}`);
    return true;
  } catch (e) {
    say(e.message, true);
    return false;
  } finally {
    onStatus('');
  }
}

/** Installs one version of a mod (update, roll back or reinstall). */
export async function installVersion(base, mod, fileId, label, alive, onStatus = () => {}) {
  try {
    const { job } = await post(`${base}/mods/version`, { file: mod.file, file_id: fileId });
    await followJob(job, (st) => onStatus(st.detail || `${label} ${mod.name}`));
    say(`${mod.name}: ${label.toLowerCase()} done.${restart(alive)} The old jar is in Backups.`);
    return true;
  } catch (e) {
    say(e.message, true);
    return false;
  } finally {
    onStatus('');
  }
}

/** "Today", "3 days ago", "2 months ago". */
export function ago(d) {
  if (!d) return '';
  const t = new Date(d).getTime();
  if (!t) return '';
  const days = Math.floor((Date.now() - t) / 86400000);
  if (days < 1) return 'Today';
  if (days < 2) return 'Yesterday';
  if (days < 31) return `${days} days ago`;
  const m = Math.floor(days / 30.4);
  if (m < 12) return m === 1 ? '1 month ago' : `${m} months ago`;
  const y = Math.floor(days / 365);
  return y === 1 ? '1 year ago' : `${y} years ago`;
}
