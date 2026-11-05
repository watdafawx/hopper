// The History button's badge: how many entries a server's History has got
// since it was last opened in this browser (kept per server, like the
// crash dot). `fresh[id]` is that count.
import { api } from './api.js';

export const fresh = $state({});
const key = (id) => `hopper.history-seen.${id}`;
const url = (id, since) => `/servers/${encodeURIComponent(id)}/mod-backups/count?since=${since}`;

function read(id) {
  try {
    const v = localStorage.getItem(key(id));
    return v == null ? null : Number(v);
  } catch {
    return null;
  }
}

function write(id, when) {
  try {
    localStorage.setItem(key(id), String(when));
  } catch {}
}

/** Looks at the server's History again. A server never looked at starts at nothing new. */
export async function refresh(id) {
  try {
    const since = read(id);
    const r = await api(url(id, since ?? 0));
    if (since == null) write(id, r.newest);
    fresh[id] = since == null ? 0 : r.new;
  } catch {}
}

/** History was opened: everything in it so far counts as seen. */
export async function markSeen(id) {
  try {
    const r = await api(url(id, 9e15));
    write(id, r.newest);
    fresh[id] = 0;
  } catch {}
}
