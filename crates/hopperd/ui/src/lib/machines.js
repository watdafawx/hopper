// Other machines: this panel's nodes and the panels signed in to on this
// page, as one list for the player. Underneath they stay two different
// things (a node dials in to this panel; a signed-in panel is reached by
// this browser, see design/multi-panel.md), but they read the same: a
// light, a name, how it's connected, its state and its servers. Kept apart
// from the components so the wording and order are tested
// (tests/machines.test.mjs).
import { nodeLine, updateLine, ceilingLabel, span } from './nodes.js';

/** How each kind of machine is connected, as its chip says. */
export const KIND = { home: 'This panel', node: 'Node', panel: 'Signed in' };

/** Only people signed in with a password keep a list of other panels: a
 * paired device or an API token has no account to keep it on. */
export const keepsPanels = (role) => !!role && role !== 'device' && role !== 'token';

/** "1 server", "3 servers"; '' when it isn't known (a panel not signed in). */
export const serverCount = (n) => (n == null ? '' : n === 1 ? '1 server' : `${n} servers`);

/** A node's light: green connected, amber reconnecting, new, updating,
 * under maintenance or needing an update, red offline. */
export const nodeLight = (n) => (n.state === 'offline' ? 'crashed' : n.state === 'connected' && !n.maintenance && n.update?.state !== 'updating' ? 'running' : 'starting');

/** A signed-in panel's light: green up, amber on its way or waiting for a
 * sign-in, red when it can't be used, grey when turned off on this page. */
export const panelLight = (p) => (p.off ? '' : p.state === 'ok' ? 'running' : p.state === 'loading' || p.state === 'reconnecting' || p.state === 'signin' ? 'starting' : 'crashed');

/** How many servers a signed-in panel has, as far as this page knows: none
 * known until it has answered once. */
export const panelCount = (p) => (p.off || p.state === 'signin' || (p.state !== 'ok' && !p.servers?.length) ? null : p.servers?.length ?? 0);

// Trouble first: what needs the player sits at the top of a long list.
const RANK = { crashed: 0, starting: 1, warn: 1, running: 2, '': 3 };

/** Nodes and signed-in panels as one list: `{ kind, key, name, light,
 * count, node | panel }`, trouble first, then by name; panels turned off
 * last. */
export function machines(nodes = [], panels = []) {
  const out = [
    ...nodes.map((n) => ({ kind: 'node', key: `node:${n.id}`, name: n.name, light: nodeLight(n), count: n.count ?? 0, node: n })),
    ...panels.map((p) => ({ kind: 'panel', key: p.id, name: p.name, light: panelLight(p), count: panelCount(p), panel: p })),
  ];
  return out.sort((a, b) => (RANK[a.light] ?? 1) - (RANK[b.light] ?? 1) || a.name.localeCompare(b.name));
}

/** How many machines in the list want the player: red or amber ones, not
 * the ones turned off. */
export const needYou = (list) => list.filter((m) => m.light === 'crashed' || m.light === 'starting').length;

/** How long ago time `ms` was: "just now", "5 min ago", "2 h ago". */
export function ago(ms, now = Date.now()) {
  const s = Math.round((now - ms) / 1000);
  if (s < 60) return 'just now';
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400) return `${Math.round(s / 3600)} h ago`;
  return `${Math.round(s / 86400)} days ago`;
}

/** The line under a signed-in panel's name on Other machines: `{ text,
 * bad, act }`, where `act` names the one thing to do about it ('retry',
 * 'signin', 'open'). `role` is the player's role there, already worded. */
export function panelLine(p, role = '', now = Date.now()) {
  const seen = p.lastSeen ? ` · seen ${ago(p.lastSeen, now)}` : '';
  if (p.off) return { text: 'Turned off on this page · nothing asks it anything', bad: false };
  switch (p.state) {
    case 'ok':
      return { text: ['Connected', p.version && `hopperd ${p.version}`, p.ping != null && `${p.ping} ms`, role && `you're its ${role}`].filter(Boolean).join(' · '), bad: false };
    case 'loading':
      return { text: 'Connecting…', bad: false };
    // Amber while it still tries on its own; red once it has given up.
    case 'reconnecting':
      return { text: `Can't reach it · Reconnecting…${seen}`, bad: false, act: 'retry' };
    case 'offline':
      return { text: `Can't reach it${seen}`, bad: true, act: 'retry' };
    case 'signin':
      return { text: p.reason === 'expired' ? 'Your sign-in ended' : p.reason === 'out' ? 'Signed out on this browser' : 'Not signed in yet', bad: false, act: 'signin' };
    case 'refused':
      return { text: "It doesn't let this page in yet", bad: true, act: 'open' };
    case 'old':
      return { text: `Runs Hopper Server${p.version ? ` ${p.version}` : ''}, too old to use from here`, bad: true, act: 'open' };
    case 'setup':
      return { text: 'Not set up yet: its owner account is missing', bad: true, act: 'open' };
    default:
      return { text: p.error || "Can't use it right now", bad: true, act: 'retry' };
  }
}

/** The line under a node's name on Other machines: `{ text, bad }`. Its
 * state comes first, since its chip now says how it's connected. */
export function nodeRowLine(n, now = Date.now() / 1000) {
  if (n.maintenance) {
    const by = n.maintenance.by ? ` · by ${n.maintenance.by}` : '';
    const since = n.maintenance.since ? ` ${span(now - n.maintenance.since)} ago` : '';
    return { text: `Under maintenance${by}${since} · nothing starts here`, bad: false };
  }
  const up = updateLine(n);
  if (up) return up;
  const line = nodeLine(n, now);
  switch (n.state) {
    case 'connected':
      return { text: line ? `Connected · ${line}` : 'Connected', bad: false };
    case 'reconnecting':
      return { text: `Reconnecting… · ${line}`, bad: false };
    case 'update_needed':
      return { text: `Update needed · ${line}`, bad: true };
    case 'main_older':
      return { text: `Newer than this panel · ${line}`, bad: true };
    case 'offline':
      return { text: line, bad: true };
    default:
      return { text: line, bad: false };
  }
}

/** The chip's tooltip: what the kind means, and what this panel may do
 * there (a node) or the player's role there (a signed-in panel). */
export function kindTip(kind, { ceiling = '', role = '' } = {}) {
  if (kind === 'node') return `A node of this panel: it connects here by itself.${ceiling ? ` This panel may: ${ceilingLabel(ceiling)}.` : ''}`;
  if (kind === 'panel') return `This browser signs in to its own panel.${role ? ` Your role there: ${role}.` : ''}`;
  return role ? `This panel. Your role here: ${role}.` : 'This panel.';
}
