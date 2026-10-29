// Who is signed in, for parts of the UI only the owner gets (browsing this
// machine's folders, the servers folder, deleting servers). `owner` is true
// for co-owners too, who have the owner's access; `trueOwner` only for the
// owner, for the few things co-owners can't do (managing co-owners,
// removing the owner's devices).
export const session = $state({ owner: false, trueOwner: false, home: '' });

/** The owner or a co-owner. */
export const ownerLike = (me) => me?.role === 'owner' || me?.role === 'coowner';

const ROLE_LABELS = { owner: 'Owner', coowner: 'Co-owner', admin: 'Admin', operator: 'Operator', viewer: 'Viewer', device: 'Device', token: 'API token' };
/** A role as people read it ("Co-owner", not "coowner"). */
export const roleLabel = (r) => ROLE_LABELS[r] || r || '';

/** Whether `path` is outside the panel folder (false while none is set). */
export function outside(path) {
  if (!session.home || !path) return false;
  const n = (p) => p.replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase();
  const h = n(session.home);
  const q = n(path);
  return !(q === h || q.startsWith(h + '/'));
}
