// Who is signed in, for parts of the UI only the owner gets (browsing this
// machine's folders, the servers folder, deleting servers). `owner` is true
// for managers too, who have the owner's access; `trueOwner` only for the
// owner, for the few things managers can't do (changing managers,
// removing the owner's devices).
export const session = $state({ owner: false, trueOwner: false, home: '' });

/** The owner or a manager. */
export const ownerLike = (me) => me?.role === 'owner' || me?.role === 'manager';

const ROLE_LABELS = { owner: 'Owner', manager: 'Manager', admin: 'Admin', operator: 'Operator', viewer: 'Viewer', device: 'Device', token: 'API token' };
/** A role as people read it ("API token", not "token"). */
export const roleLabel = (r) => ROLE_LABELS[r] || r || '';

/** Whether `path` is outside the panel folder (false while none is set). */
export function outside(path) {
  if (!session.home || !path) return false;
  const n = (p) => p.replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase();
  const h = n(session.home);
  const q = n(path);
  return !(q === h || q.startsWith(h + '/'));
}
