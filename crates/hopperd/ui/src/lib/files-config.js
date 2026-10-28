// Which mod each entry of a server's config/ belongs to (by the mod ids in
// its jar), with the mod's row from the Mods list for its icon and name, and
// each mod's text config files. Cached per server for a short while.

import { api } from './api.js';

const cache = new Map();
const KEEP = 30_000;

/**
 * { owners: { entry: jar }, files: { jar: [rel] }, mods: Map<jar, mod row> }
 * for a server. Never throws: an empty map when it can't be read.
 */
export async function configOwners(sid, fresh = false) {
  const hit = cache.get(sid);
  if (hit && !fresh && Date.now() - hit.at < KEEP) return hit.value;
  const value = (async () => {
    const id = encodeURIComponent(sid);
    const [o, m] = await Promise.all([
      api(`/fs/${id}/config-owners`).catch(() => ({ owners: {}, files: {} })),
      api(`/servers/${id}/mods/list`).catch(() => ({ mods: [] })),
    ]);
    const rows = m.mods || m.rows || [];
    const mods = new Map(rows.map((r) => [r.file, r]));
    return { owners: o.owners || {}, files: o.files || {}, mods };
  })();
  cache.set(sid, { at: Date.now(), value });
  return value;
}

/** The mod row a path under config/ belongs to, or null. */
export function ownerOf(data, rel) {
  if (!data || !rel.startsWith('config/')) return null;
  const top = rel.split('/')[1];
  const jar = data.owners[top];
  if (!jar) return null;
  return data.mods.get(jar) || { file: jar, name: jar.replace(/\.jar$/i, ''), icon: '' };
}

/** The text config files of the mod `rel` belongs to (rel itself included). */
export function siblingsOf(data, rel) {
  const m = ownerOf(data, rel);
  return (m && data.files[m.file]) || [];
}
