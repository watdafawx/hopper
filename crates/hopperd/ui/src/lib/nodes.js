// Settings › Nodes (design/multi-panel.md, A1): how a node's row reads.
// Kept apart from the component so the wording is tested
// (tests/nodes.test.mjs).

/** Nodes show now that a node runs servers for its main (A2). Off: they
 * stay out of the UI, as before A2. */
export const NODES_SHOWN = true;

/** The state chip: its text and the shared chip/dot class. */
export const STATES = {
  connected: { label: 'Connected', cls: 'running' },
  reconnecting: { label: 'Reconnecting', cls: 'warn' },
  offline: { label: 'Offline', cls: 'crashed' },
  new: { label: 'Not connected yet', cls: '' },
  // Two tunnel versions apart (design/multi-panel.md, A3): it can't
  // connect until one side is updated; its servers show read-only.
  update_needed: { label: 'Update needed', cls: 'warn' },
  main_older: { label: 'Update this panel', cls: 'warn' },
};

/** A node's state chip, with "Updating" over Connected while this panel
 * updates it. */
export const stateOf = (n) => (n.update?.state === 'updating' ? { label: 'Updating', cls: 'warn' } : (STATES[n.state] ?? STATES.new));

/** The line about an update this panel asked of the node, if any:
 * `{ text, bad }`. A finished one shows until the node is current. */
export function updateLine(n) {
  const u = n.update;
  if (!u) return null;
  if (u.state === 'updating') return { text: `Updating · ${u.message || 'Asking it to update'}`, bad: false };
  if (u.state === 'failed') return { text: `Update failed: ${u.message}`, bad: true };
  return n.outdated ? null : { text: u.message, bad: false };
}

/** What Update says on node `n`: its target version. */
export const updateLabel = (n, version) => (n.update?.state === 'failed' ? 'Try again' : version ? `Update to ${version}` : 'Update');

/** The list head: how many nodes are behind, and how many Update all
 * would update. */
export function behind(data) {
  const nodes = data?.nodes ?? [];
  return { older: nodes.filter((n) => n.outdated || n.state === 'update_needed').length, updatable: nodes.filter((n) => n.can_update).length };
}

/** The sentence over the node list while nodes run an older version. */
export function behindLine(data) {
  const { older } = behind(data);
  if (!older) return '';
  const v = data.version ? `hopperd ${data.version}` : 'a newer hopperd';
  return `This panel runs ${v}. ${older === 1 ? '1 node runs' : `${older} nodes run`} an older one.`;
}

export const CEILINGS = [
  { id: 'look', label: 'Look', blurb: 'This panel can see the servers there and read their consoles.' },
  { id: 'operate', label: 'Operate', blurb: 'This panel can see the servers there, start, stop and restart them, send console commands and make backups.' },
  { id: 'manage', label: 'Manage', blurb: 'This panel can do everything there: files, mods, settings, adding and removing servers. That is code running on that machine.' },
];

export const ceilingLabel = (id) => CEILINGS.find((c) => c.id === id)?.label ?? id;

/** A server that lives on a node: its id is `<node>:<server>` (the
 * backend's relay::SEP; a panel's own ids never hold a `:`), and the
 * merged list also marks it with `node`. */
export const onNode = (s) => !!s && (!!s.node || String(s.id ?? '').includes(':'));

/** The node a server id lives on ('' for this panel's own). */
export const nodeOf = (id) => (String(id ?? '').includes(':') ? String(id).split(':')[0] : '');

/** Whether alert `a` shows on the page of server `id` ('' is the Overview):
 * the server's own alerts, plus alerts about the machine it runs on (a
 * drive, memory). The Overview shows every machine's: this panel's and
 * each node's, which name their node. */
export const alertShows = (a, id) => a.server_id === id || (!a.server_id && (!id || (a.node || '') === nodeOf(id)));

/** Whether "Share on mclo.gs" works for server `s`. Sharing isn't relayed
 * to a node (the panel itself posts the log), so it shows only for a
 * panel's own servers. */
export const canShareLog = (s) => !onNode(s);

/** "14:02" today, else "12 Nov 14:02". */
export function clock(t, now = Date.now() / 1000) {
  if (!t) return '';
  const d = new Date(t * 1000);
  const hm = d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  return new Date(now * 1000).toDateString() === d.toDateString() ? hm : `${d.toLocaleDateString([], { day: 'numeric', month: 'short' })} ${hm}`;
}

/** "40 s", "3 min", "2 h". */
export function span(s) {
  if (s < 60) return `${Math.max(0, Math.round(s))} s`;
  if (s < 3600) return `${Math.floor(s / 60)} min`;
  if (s < 86400) return `${Math.floor(s / 3600)} h`;
  return `${Math.floor(s / 86400)} days`;
}

/** The line under a node's name. */
export function nodeLine(n, now = Date.now() / 1000) {
  switch (n.state) {
    case 'connected':
      return [n.version && `hopperd ${n.version}`, n.rtt_ms != null && `${n.rtt_ms} ms`, n.since && `since ${clock(n.since, now)}`, n.ip].filter(Boolean).join(' · ');
    case 'update_needed':
    case 'main_older':
      return n.skew || (n.state === 'update_needed' ? 'Too old for this panel. Update it on the node itself.' : 'Newer than this panel. Update this panel first.');
    case 'reconnecting':
      return n.quiet_secs != null ? `Last heard ${span(n.quiet_secs)} ago` : 'Reconnecting';
    case 'offline': {
      const since = n.last_seen ? `Offline since ${clock(n.last_seen, now)}` : 'Offline since this panel started';
      return n.ended ? `${since} · ${n.ended}` : since;
    }
    default:
      return `Joined ${clock(n.created, now)}. Waiting for it to connect.`;
  }
}

/** The 6-digit code as two groups, like the node prints it. */
export const codeGroups = (code) => [code.slice(0, 3), code.slice(3)];

/** Where nodes should reach this panel: the page's own address over
 * HTTPS, else the first HTTPS address that isn't this PC only. */
export function defaultAddress(loc, hello) {
  if (loc.protocol === 'https:') return loc.origin;
  const away = (hello?.addresses ?? []).find((a) => a.startsWith('https://') && !/\/\/(localhost|127\.|\[::1\])/.test(a));
  return away ?? `https://${loc.hostname}:${hello?.port ?? 8790}`;
}

/** What This node's status says, from the node's own view. */
export const NODE_SIDE = {
  connected: { label: 'Connected', cls: 'running' },
  connecting: { label: 'Connecting', cls: 'warn' },
  reconnecting: { label: 'Reconnecting', cls: 'warn' },
  stopped: { label: 'Stopped', cls: 'crashed' },
};
