// Start, stop, restart or send a command to several servers at once, on
// this panel and on other panels added to the page (multi-panel B3,
// design/multi-panel.md). Each server's call goes to its own panel with
// that panel's own sign-in, exactly as a single click on its page would,
// so every panel checks the role itself and writes its own audit log.
//
// What this adds is the planning around it: one plan per panel listing what
// will be sent and what is skipped (and why) before anything is sent, so
// the single confirmation can show all of it; a server the person's role
// on its panel can't touch is skipped here and never sent.
//
// This panel's nodes (A4) are groups of their own: their servers' calls go
// to this panel, which relays them and writes them in its audit log with
// the node named. A node's group carries what the node allows (its
// ceiling), whether it's paused, and whether it can be reached.

import { startBlocked } from './moves.js';

export const ACTIONS = ['start', 'stop', 'restart', 'command'];
export const VERB = { start: 'Start', stop: 'Stop', restart: 'Restart', command: 'Send command' };
const DOING = { start: 'Starting', stop: 'Stopping', restart: 'Restarting', command: 'Sent' };

const up = (s) => s.state === 'running' || s.state === 'starting';
const players = (s) => (Array.isArray(s.players) ? s.players.filter((p) => p && String(p).trim()) : []);

/** Why `group` can't take actions at all, or ''. `group`: { name, role,
 * state } where state is the panel's ('ok' when usable; this panel is
 * always 'ok'). */
export function groupBlock(group) {
  if (group.node) {
    if (group.state === 'update_needed' || group.state === 'main_older') return `${group.name} needs an update before it takes actions.`;
    if (group.state !== 'connected' && group.state !== 'reconnecting') return `Can't reach ${group.name} right now.`;
    if (group.ceiling === 'look') return `${group.name} lets this panel only look.`;
    if (group.role === 'viewer') return `Your account can only look.`;
    return '';
  }
  if (group.state && group.state !== 'ok') {
    if (group.state === 'signin') return `Sign in to ${group.name} again first.`;
    return `Can't reach ${group.name} right now.`;
  }
  if (group.role === 'viewer') return `Your account on ${group.name} can only look.`;
  return '';
}

/** What `action` does to server `s`: { act, note } to send (act is the
 * power action, or 'command'), or { skip } with the reason. */
export function forServer(s, action) {
  if (s.state === 'stopping') return { skip: 'Still stopping.' };
  // A copy that was moved (or is being sent) never starts from here.
  const blocked = !up(s) && (action === 'start' || action === 'restart') ? startBlocked(s) : '';
  if (blocked) return { skip: blocked };
  if (action === 'start') return up(s) ? { skip: 'Already running.' } : { act: 'start', note: '' };
  if (action === 'stop') return up(s) ? { act: 'stop', note: '' } : { skip: 'Not running.' };
  if (action === 'command') return up(s) ? { act: 'command', note: '' } : { skip: 'Not running.' };
  // A restart of a stopped server starts it, as people expect from the button.
  if (action === 'restart') return up(s) ? { act: 'restart', note: '' } : { act: 'start', note: 'Not running, so it starts.' };
  return { skip: 'Not something servers can do.' };
}

/** The bulk group of node `n` (the live feed's head) for someone whose
 * role on this panel is `role`. */
export const nodeGroup = (n, role) => ({ key: `node:${n.id}`, name: n.name, panel: null, node: n.id, role, state: n.state, ceiling: n.ceiling, maintenance: !!n.maintenance, servers: [] });

/**
 * The plan: one entry per group that has picked servers, in the order the
 * groups were given. Each: { key, name, panel, send: [{ server, act, note,
 * online }], skip: [{ server, reason }] }.
 * - groups: [{ key, name, panel (null: this panel), role, state, servers }]
 *   where servers are the picked ones of that group.
 */
export function planBulk(groups, action) {
  const out = [];
  for (const g of groups) {
    if (!g.servers.length) continue;
    const block = groupBlock(g);
    const entry = { key: g.key, name: g.name, panel: g.panel ?? null, node: g.node ?? '', send: [], skip: [] };
    for (const s of g.servers) {
      if (block) {
        entry.skip.push({ server: s, reason: block });
        continue;
      }
      const r = forServer(s, action);
      // A paused node takes stops and commands, never a start.
      if (!r.skip && r.act === 'start' && g.maintenance) entry.skip.push({ server: s, reason: `${g.name} is under maintenance.` });
      else if (r.skip) entry.skip.push({ server: s, reason: r.skip });
      else entry.send.push({ server: s, act: r.act, note: r.note, online: r.act === 'stop' || r.act === 'restart' ? players(s) : [] });
    }
    out.push(entry);
  }
  return out;
}

/** Totals of a plan: { send, skip, online, panels } (panels: groups with
 * something to send). */
export function counts(plan) {
  let send = 0, skip = 0, online = 0, panels = 0;
  for (const g of plan) {
    send += g.send.length;
    skip += g.skip.length;
    online += g.send.reduce((n, x) => n + x.online.length, 0);
    if (g.send.length) panels++;
  }
  return { send, skip, online, panels };
}

/** "Restart 3 servers?" and the like. */
export function title(action, n) {
  const what = n === 1 ? '1 server' : `${n} servers`;
  return action === 'command' ? `Send a command to ${what}?` : `${VERB[action]} ${what}?`;
}

/** The OK button's words: says how many, so it reads as what happens. */
export function okLabel(action, n) {
  const what = n === 1 ? '1 server' : `${n} servers`;
  return action === 'command' ? `Send to ${what}` : `${VERB[action]} ${what}`;
}

/**
 * Sends the plan. Panels run side by side; on one panel the servers go one
 * after another (gentle on its rate limit, and its audit log reads in
 * order). Skipped servers are never sent.
 * - send(panel, server, act, line): resolves when that panel took it,
 *   rejects with an Error whose message says why not.
 * - onResult(groupKey, serverId, { status: 'ok' | 'failed', text }).
 * Resolves to the results: { [groupKey]: { [serverId]: result } }, skipped
 * ones included as { status: 'skipped', text: reason }.
 */
export async function runBulk(plan, line, send, onResult = () => {}) {
  const results = {};
  for (const g of plan) {
    results[g.key] = {};
    for (const x of g.skip) results[g.key][x.server.id] = { status: 'skipped', text: x.reason };
  }
  await Promise.all(
    plan.map(async (g) => {
      for (const x of g.send) {
        let r;
        try {
          await send(g.panel, x.server, x.act, line);
          r = { status: 'ok', text: x.act === 'command' ? 'Sent.' : `${DOING[x.act]}.` };
        } catch (e) {
          r = { status: 'failed', text: (e && e.message) || 'It failed.' };
        }
        results[g.key][x.server.id] = r;
        onResult(g.key, x.server.id, r);
      }
    }),
  );
  return results;
}

/** Totals of the results: { ok, failed, skipped }. */
export function tally(results) {
  const t = { ok: 0, failed: 0, skipped: 0 };
  for (const g of Object.values(results)) for (const r of Object.values(g)) if (t[r.status] != null) t[r.status]++;
  return t;
}

// ---- the list's filters ----

/** The state filter a server falls under: 'running' (also starting and
 * stopping), 'crashed' (also a failed wake), else 'stopped' (also asleep). */
export function stateGroup(s) {
  if (s.state === 'running' || s.state === 'starting' || s.state === 'stopping') return 'running';
  if (s.state === 'crashed' || s.wake_failed) return 'crashed';
  return 'stopped';
}

/** Whether server `s` passes the state filter (`all` passes everything). */
export const passes = (s, filter) => filter === 'all' || stateGroup(s) === filter;
