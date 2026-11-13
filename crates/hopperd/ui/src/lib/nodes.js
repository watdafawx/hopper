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
};

export const CEILINGS = [
  { id: 'look', label: 'Look', blurb: 'This panel can see the servers there and read their consoles.' },
  { id: 'operate', label: 'Operate', blurb: 'This panel can see the servers there, start, stop and restart them, send console commands and make backups.' },
  { id: 'manage', label: 'Manage', blurb: 'This panel can do everything there: files, mods, settings, adding and removing servers. That is code running on that machine.' },
];

export const ceilingLabel = (id) => CEILINGS.find((c) => c.id === id)?.label ?? id;

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
