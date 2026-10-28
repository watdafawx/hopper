// Mod actions shared by the Mods table, its menu, the bulk bar, the side
// panel and the Versions dialog, so each one asks and words things the same
// way (as Hopper desktop does).
import { api, post } from './api.js';
import { say } from './toast.svelte.js';
import { confirmIt, choose } from './dialog.svelte.js';
import { followJob } from './jobs.js';
import { removing, askRemove, offerUndo } from './modsremove.svelte.js';

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

/**
 * Removes mods (moved to .hopper-backups), as desktop asks: the mods that
 * need them, and the ones nothing else needs once they go (offered,
 * unticked). "Removed X" then offers Undo; `onUndo` runs after it put
 * them back.
 */
export async function removeMods(base, mods, alive, { onUndo = () => {} } = {}) {
  let files = mods.map((m) => m.file);
  // Both lists are extras: the question still makes sense without them.
  const [need, orphans] = await Promise.all([
    post(`${base}/mods/related`, { files, on: false }).then((r) => r.mods, () => []),
    removing.hosts ? post(`${base}/mods/orphans`, { files }).then((r) => r.mods, () => []) : [],
  ]);
  const w = what(mods);
  if (removing.hosts) {
    const extra = await askRemove(`Remove ${w}?`, need, orphans);
    if (!extra) return false;
    files = [...files, ...extra];
  } else {
    const ok = await confirmIt(`Remove ${w}?`, {
      body: `Move to backups keeps a copy in .hopper-backups (Mods › Backups).${need.length ? ` ${need.map((n) => n.name).join(', ')} ${need.length === 1 ? 'needs' : 'need'} ${mods.length === 1 ? 'it' : 'them'}: the server stops at loading without.` : ''}`,
      ok: 'Move to backups',
    });
    if (!ok) return false;
  }
  try {
    const r = await post(`${base}/mods/remove-many`, { files });
    const text = `Removed ${r.removed === 1 && files.length === 1 ? w : `${r.removed} mods`}. ${r.removed === 1 ? "It's" : "They're"} in Mods › Backups.${restart(alive)}`;
    if (r.undo && removing.hosts) offerUndo({ base, token: r.undo, text, alive, onUndo });
    else say(text);
    return true;
  } catch (e) {
    say(e.message, true);
    // Some may have gone before the one that failed.
    return /removed \d+ before/i.test(e.message);
  }
}

/** Puts the last remove back (the toast's Undo). */
export async function undoRemove(u) {
  try {
    const r = await post(`${u.base}/mods/undo-remove`, { token: u.token });
    say(`Put back ${r.restored === 1 ? '1 mod' : `${r.restored} mods`}.${r.restart_needed ? ' Restart the server to load them.' : ''}`);
    u.onUndo?.();
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

/**
 * Keeps one offered file out (desktop's "Lock 1.2.3 (keep 1.2.2)"), or lets
 * it in again with `version` null.
 */
export async function setVersionLock(base, mod, version) {
  try {
    await post(`${base}/mods/version-lock`, { file: mod.file, version });
    return true;
  } catch (e) {
    say(e.message, true);
    return false;
  }
}

/** A mod's config files in config/, named after it (empty when none). */
export async function configFiles(base, mod) {
  try {
    return (await api(`${base}/mods/config?file=${encodeURIComponent(mod.file)}`)).files || [];
  } catch (e) {
    say(e.message, true);
    return null;
  }
}

/**
 * Why an update would stop the server loading: what the new file needs
 * that isn't there, and the mods it breaks (they need this one at a
 * version range the update leaves), as desktop's tooltip says them.
 */
export const problems = (u) => [
  ...(u?.issues || []),
  ...(u?.breaks || []).map((b) => `stops ${b.name} loading (it needs ${b.needs})`),
];

/** The offered file is one the user keeps out (desktop's version lock). */
export const versionLocked = (m) => !!(m.update && m.locked_version && m.locked_version === m.update.file);

/**
 * Desktop's list search: every word is in a field, or (3+ letters) its
 * letters come in order in a word starting with the same letter.
 */
export function wordsMatch(query, fields) {
  const fs = fields.map((f) => (f || '').toLowerCase());
  return query.split(/\s+/).filter(Boolean).every((w) => {
    w = w.toLowerCase();
    if (fs.some((f) => f.includes(w))) return true;
    if ([...w].length < 3) return false;
    return fs.some((f) => f.split(/[^\p{L}\p{N}]+/u).some((word) => {
      if (word[0] !== w[0]) return false;
      let i = 0;
      for (const c of word) if (c === w[i]) i++;
      return i >= w.length;
    }));
  });
}

/** "Today", "Yesterday", "3 days ago", "2 weeks ago", "5 months ago", as desktop says them. */
export function ago(d) {
  if (!d) return '';
  const t = new Date(d).getTime();
  // CurseForge writes 0001-01-01 for "never".
  if (!t || t < 946684800000) return '';
  const days = Math.floor(Date.now() / 86400000) - Math.floor(t / 86400000);
  if (days <= 0) return 'Today';
  if (days === 1) return 'Yesterday';
  if (days < 7) return `${days} days ago`;
  if (days < 14) return '1 week ago';
  if (days < 30) return `${Math.floor(days / 7)} weeks ago`;
  if (days < 60) return '1 month ago';
  if (days < 365) return `${Math.floor(days / 30)} months ago`;
  if (days < 730) return '1 year ago';
  return `${Math.floor(days / 365)} years ago`;
}

/** The exact day, "27/09/2026", as desktop's tooltips say it. */
export function dateOf(d) {
  const t = d ? new Date(d) : null;
  if (!t || !t.getTime() || t.getTime() < 946684800000) return '';
  const p = (n) => String(n).padStart(2, '0');
  return `${p(t.getUTCDate())}/${p(t.getUTCMonth() + 1)}/${t.getUTCFullYear()}`;
}
