// Who is signed in, for parts of the UI only the owner gets (browsing this
// machine's folders, the servers folder, deleting servers). `owner` is true
// for managers too, who have the owner's access; `trueOwner` only for the
// owner, for the few things managers can't do (changing managers,
// removing the owner's devices).
export const session = $state({ owner: false, trueOwner: false, role: '', home: '', servers: '' });

/** The owner or a manager. */
export const ownerLike = (me) => me?.role === 'owner' || me?.role === 'manager';

const ROLE_LABELS = { owner: 'Owner', manager: 'Manager', admin: 'Admin', operator: 'Operator', viewer: 'Viewer', device: 'Device', token: 'API token' };
/** A role as people read it ("API token", not "token"). */
export const roleLabel = (r) => ROLE_LABELS[r] || r || '';

/** Whether server `s` sits outside both the panel folder and the servers
 * folder (false while none is set): a server the panel made in its own
 * servers folder is never "outside", wherever that folder is. A node's
 * server lives in the node's folders, which this panel can't judge. */
export function outside(s) {
  const path = s?.path;
  if (!session.home || !path || s.node) return false;
  const n = (p) => p.replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase();
  const q = n(path);
  const under = (dir) => !!dir && (q === n(dir) || q.startsWith(n(dir) + '/'));
  return !under(session.home) && !under(session.servers);
}
