// Modpack updates the panel knows of, per server id: { from, to, count }.
// The sidebar shows a dot for these; a server's page checks its own pack
// and writes the answer here too.
import { api } from './api.js';

export const packUpdates = $state({ byId: {} });

/** Reads every server's cached check (the panel checks stale ones in the background). */
export async function refreshPackUpdates() {
  try {
    packUpdates.byId = (await api('/pack-updates')).servers || {};
  } catch {
    // older panel, or signed out: no dots
  }
}

/** One server's check answer (`/servers/{id}/pack/check`), or null when it has none. */
export function notePackCheck(id, check) {
  const n = check?.newer || [];
  const next = { ...packUpdates.byId };
  if (check?.pack && n.length) next[id] = { from: check.pack.current.name, to: n[0].name, count: n.length };
  else delete next[id];
  packUpdates.byId = next;
}

/**
 * Two version names without the part they share: "ATM10-2.38" and
 * "ATM10-2.40" → ["2.38", "2.40"]. Cut at a separator, so "1.9" → "1.10"
 * stays whole.
 */
export function shortVersions(a = '', b = '') {
  let i = 0;
  while (i < a.length && i < b.length && a[i] === b[i]) i++;
  const at = i ? Math.max(a.lastIndexOf(' ', i - 1), a.lastIndexOf('-', i - 1), a.lastIndexOf('_', i - 1)) + 1 : 0;
  const sa = a.slice(at).replace(/\.(zip|mrpack)$/i, '').trim();
  const sb = b.slice(at).replace(/\.(zip|mrpack)$/i, '').trim();
  return sa && sb ? [sa, sb] : [a, b];
}
