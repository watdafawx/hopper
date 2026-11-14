// Which servers a person or API token may use (design/multi-panel.md, A4).
// A list of server ids, where `node:<id>` means every server on that node,
// including ones made there later, and `none:` (left when the last entry's
// node was removed) means no server at all. An empty list is every server.
// Kept apart from the components so the wording is tested
// (tests/scope.test.mjs).

export const NODE_SCOPE = 'node:';
export const NO_SERVERS = 'none:';

const nodeOfId = (id) => (String(id).includes(':') ? String(id).split(':')[0] : '');
export const scopeNode = (entry) => (String(entry).startsWith(NODE_SCOPE) ? String(entry).slice(NODE_SCOPE.length) : '');

/** The picker's rows: this panel's servers, then one row per node with
 * its servers. `servers` and `nodes` are the live feed's. */
export function scopeRows(servers, nodes) {
  const rows = [{ node: null, name: 'This panel', servers: servers.filter((s) => !nodeOfId(s.id)) }];
  for (const n of nodes ?? []) rows.push({ node: n, name: n.name, servers: servers.filter((s) => nodeOfId(s.id) === n.id) });
  return rows.filter((r) => r.node || r.servers.length);
}

/** Whether entry list `list` covers server `id` (by name or its whole node). */
export const covers = (list, id) => list.includes(id) || list.includes(NODE_SCOPE + nodeOfId(id));

/** `list` with `entry` (a server id or `node:<id>`) turned on or off.
 * Picking a whole node drops its single servers: the node entry holds
 * them. `none:` goes as soon as anything is picked. */
export function flipScope(list, entry) {
  const rest = list.filter((x) => x !== NO_SERVERS);
  if (rest.includes(entry)) return rest.filter((x) => x !== entry);
  const node = scopeNode(entry);
  return node ? [...rest.filter((x) => nodeOfId(x) !== node), entry] : [...rest, entry];
}

/** How a list reads in a row: "All servers", "No servers", or names. */
export function scopeText(list, servers, nodes) {
  if (!list?.length) return 'All servers';
  if (list.length === 1 && list[0] === NO_SERVERS) return 'No servers (their node was removed)';
  return list
    .filter((x) => x !== NO_SERVERS)
    .map((x) => {
      const node = scopeNode(x);
      if (node) return `all on ${(nodes ?? []).find((n) => n.id === node)?.name || 'a removed node'}`;
      return servers.find((s) => s.id === x)?.name || x;
    })
    .join(', ')
    .replace(/^all/, 'All');
}

/** Your own access in a few words: "all servers", "3 servers",
 * "all servers on vm-frankfurt", "2 servers and all on vm-jeddah". */
export function scopeCount(list, nodes) {
  if (!list?.length) return 'all servers';
  if (list.length === 1 && list[0] === NO_SERVERS) return 'no servers';
  const singles = list.filter((x) => x !== NO_SERVERS && !scopeNode(x)).length;
  const whole = list.filter((x) => scopeNode(x)).map((x) => (nodes ?? []).find((n) => n.id === scopeNode(x))?.name || 'a node');
  const parts = [];
  if (singles) parts.push(`${singles} server${singles === 1 ? '' : 's'}`);
  if (whole.length) parts.push(`${singles ? 'all' : 'all servers'} on ${whole.join(', ')}`);
  return parts.join(' and ');
}

/** The line under the picker: what the choice covers. */
export function scopeHint(list, servers, nodes) {
  if (!list.length) return 'Nothing picked: every server, on this panel and every node.';
  const t = scopeText(list, servers, nodes);
  return list.some((x) => scopeNode(x)) ? `${t}. A whole node includes servers made there later.` : `${t}.`;
}
