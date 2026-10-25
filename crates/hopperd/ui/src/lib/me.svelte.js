// Who is signed in, for parts of the UI only the owner gets (browsing this
// machine's folders, the servers folder, deleting servers).
export const session = $state({ owner: false, home: '' });

/** Whether `path` is outside the panel folder (false while none is set). */
export function outside(path) {
  if (!session.home || !path) return false;
  const n = (p) => p.replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase();
  const h = n(session.home);
  const q = n(path);
  return !(q === h || q.startsWith(h + '/'));
}
