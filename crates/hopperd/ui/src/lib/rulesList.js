// The Rules page as one list. Three stores sit behind it (messages = webhooks,
// warnings = the built-in alerts, actions = automations); this file turns them
// into rows that each read as "When X, then Y", and sorts, searches and counts
// those rows. No Svelte in here, so tests can run it.
import { whenText, stepText, onlyText } from './rules.js';
import { matches } from './rulesPage.js';

export const KIND_LABEL = { message: 'Message', warning: 'Warning', action: 'Action' };
/** Filter value -> row kind. */
export const FILTER_KIND = { messages: 'message', warnings: 'warning', actions: 'action' };
const RANK = { message: 0, warning: 1, action: 2 };

/** "a server" + "starts": what each message event reads like in a sentence. */
const EVENT_WORDS = {
  start: ['a server', 'starts'],
  stop: ['a server', 'stops'],
  crash: ['a server', 'crashes'],
  join: ['a player', 'joins'],
  leave: ['a player', 'leaves'],
  backup: ['a backup', 'is made'],
  wake: ['a server', 'is woken up'],
  sleep: ['a server', 'goes to sleep'],
  schedule: ['a scheduled restart or warning', 'happens'],
  problem: ['something the panel tried', 'fails'],
  tps: ['TPS', 'is low'],
  disk: ['a drive', 'is nearly full'],
  ram: ['PC memory', 'is nearly full'],
  copy_failed: ['an off-site backup copy', 'fails'],
  node: ['a node', 'goes offline'],
};
/** Events a message sentence names before it says "and N more". */
const MAX_EVENTS = 4;

const list = (xs) => (xs.length < 2 ? xs.join('') : `${xs.slice(0, -1).join(', ')} or ${xs[xs.length - 1]}`);

/** "a server starts, stops or crashes, or a player joins". `order` is the
 * panel's own event order (the list the API sends), so the words come out the
 * same however the chips were clicked. */
export function eventsPhrase(ids, order = []) {
  const rank = (id) => {
    const i = order.findIndex((e) => e.id === id);
    return i < 0 ? 999 : i;
  };
  const sorted = [...ids].sort((a, b) => rank(a) - rank(b));
  if (!sorted.length) return 'nothing happens';
  const shown = sorted.slice(0, MAX_EVENTS);
  const more = sorted.length - shown.length;
  const groups = [];
  for (const id of shown) {
    const [who, verb] = EVENT_WORDS[id] || [id, 'happens'];
    const g = groups.find((x) => x.who === who);
    if (g) g.verbs.push(verb);
    else groups.push({ who, verbs: [verb] });
  }
  const text = groups.map((g) => `${g.who} ${list(g.verbs)}`).join(', or ');
  return more ? `${text}, or ${more} more ${more === 1 ? 'event' : 'events'}` : text;
}

/** What a step list does, as the end of a sentence. A step still being typed
 * (no command or URL yet) says what kind it is. */
export function stepsPhrase(steps) {
  const one = (s) => {
    if (s.kind === 'command') return s.line.trim() ? `run “${stepText(s)}”` : 'run a console command';
    if (s.kind === 'url') return s.url.trim() ? stepText(s) : 'call a URL';
    return s.kind === 'power' ? `${stepText(s)} the server` : stepText(s);
  };
  return steps.map(one).join(', then ');
}

export const messageOutcome = (kind) => (kind === 'discord' ? 'post to Discord' : 'post to a URL');

/** Sentence pieces: `b` is bold, `field` is a number the row lets you edit in place. */
const seg = (text, b = false) => ({ text, b });
const num = (field, value, min, max, step = 1) => ({ field, value, min, max, step, b: true });

/** "When x" -> plain "When " + bold "x"; "Every 5 min" stays bold whole. */
function triggerParts(text) {
  return text.startsWith('When ') ? [seg('When '), seg(text.slice(5), true)] : [seg(text, true)];
}

/** The pieces joined into plain text (search, tests, aria labels). */
export const plain = (parts) => parts.map((p) => (p.field ? String(p.value) : p.text)).join('');

export const messageSentence = (events, kind, order) => [...triggerParts(`When ${eventsPhrase(events, order)}`), seg(', '), seg(messageOutcome(kind), true)];
export const actionSentence = (r, ruleEvents) => [...triggerParts(whenText(r.trigger, ruleEvents)), seg(', '), seg(stepsPhrase(r.steps), true)];

/** The five built-in warnings: settings key, the on/off key, and the sentence. */
export const WARNING_DEFS = [
  { id: 'crash', flag: 'crash', label: 'Server crashed', hint: 'Shown on the server; messages get “Server crashed”', parts: () => [seg('When '), seg('a server crashes', true)] },
  {
    id: 'tps',
    flag: 'tps_on',
    label: 'Low TPS',
    hint: 'Servers with RCON only (the Console offers to turn it on)',
    parts: (s) => [seg('When '), seg('TPS is below ', true), num('tps_below', s.tps_below, 1, 20, 0.5), seg(` for ${s.tps_minutes} min`, true)],
  },
  {
    id: 'disk',
    flag: 'disk_on',
    label: 'Drive nearly full',
    hint: 'On each server’s drive and the panel’s',
    parts: (s) => [seg('When '), seg('free space on a drive is below ', true), num('disk_gb', s.disk_gb, 1, 100000), seg(' GB', true)],
  },
  {
    id: 'ram',
    flag: 'ram_on',
    label: 'PC memory nearly full',
    hint: '',
    parts: (s) => [seg('When '), seg('PC memory is above ', true), num('ram_pct', s.ram_pct, 50, 100), seg(`% for ${s.ram_minutes} min`, true)],
  },
  {
    id: 'node',
    flag: 'node_on',
    label: 'Node offline',
    hint: 'Nodes under maintenance stay quiet',
    needsNodes: true,
    parts: (s) => [seg('When '), seg('a node has been silent for ', true), num('node_minutes', s.node_minutes, 2, 120), seg(' min', true)],
  },
];
const WARN_OUTCOME = 'warn on the server';

/** "All servers", the server's name, or "3 servers". */
export function serverLabel(ids, servers = []) {
  if (!ids.length) return 'All servers';
  if (ids.length === 1) return servers.find((s) => s.id === ids[0])?.name || ids[0];
  return `${ids.length} servers`;
}

/**
 * Every row of the list, sorted: messages, then warnings, then actions, each
 * kind in the order its store keeps them. `last` is what the row's "last run"
 * line is built from; `fail` is true while that last run failed.
 */
export function buildItems({ hooks = [], hookEvents = [], rules = [], ruleEvents = [], settings = null, hasNodes = false, servers = [] }) {
  const items = [];
  for (const h of hooks) {
    const parts = messageSentence(h.events, h.kind, hookEvents);
    const label = serverLabel(h.servers, servers);
    items.push({
      key: `message:${h.id}`, kind: 'message', id: h.id, enabled: h.enabled, name: h.name, parts,
      servers: h.servers, serverText: label, extra: '', last: h.last_error ? { ok: false, text: h.last_error } : null, fail: !!h.last_error,
      search: [KIND_LABEL.message, h.name, plain(parts), label, h.kind, h.last_error, ...h.events.map((id) => hookEvents.find((e) => e.id === id)?.label)],
      src: h,
    });
  }
  if (settings) {
    for (const d of WARNING_DEFS) {
      if (d.needsNodes && !hasNodes) continue;
      const parts = [...d.parts(settings), seg(', '), seg(WARN_OUTCOME, true)];
      items.push({
        key: `warning:${d.id}`, kind: 'warning', id: d.id, flag: d.flag, enabled: !!settings[d.flag], name: d.label, parts,
        servers: [], serverText: 'All servers', extra: d.hint, last: null, fail: false,
        search: [KIND_LABEL.warning, 'warnings alerts', d.label, plain(parts), 'All servers', d.hint],
        src: settings,
      });
    }
  }
  for (const r of rules) {
    const parts = actionSentence(r, ruleEvents);
    const last = r.history?.[0] || null;
    const label = serverLabel(r.servers, servers);
    const extra = [onlyText(r.only), r.cooldown_minutes ? `at most every ${r.cooldown_minutes} min` : ''].filter(Boolean).join(' · ');
    items.push({
      key: `action:${r.id}`, kind: 'action', id: r.id, enabled: r.enabled, name: r.name, parts,
      servers: r.servers, serverText: label, extra, last, fail: !!last && !last.ok,
      search: [KIND_LABEL.action, r.name, plain(parts), label, extra, last && !last.ok ? last.text : null],
      src: r,
    });
  }
  // Array.sort is stable, so each kind keeps its store's order.
  return items.sort((a, b) => RANK[a.kind] - RANK[b.kind]);
}

/** The rows a filter and a search leave. */
export function visibleItems(items, filter, query) {
  const kind = FILTER_KIND[filter];
  return items.filter((i) => (!kind || i.kind === kind) && matches(query, ...i.search));
}

/** Numbers on the filter: how many rows of each kind. */
export function countItems(items) {
  const c = { all: items.length, messages: 0, warnings: 0, actions: 0 };
  for (const i of items) c[`${i.kind}s`]++;
  return c;
}

/** How many rows that are on failed their last run. */
export const failingItems = (items) => items.filter((i) => i.enabled && i.fail).length;

/** Does a search leave the "Warning settings" part showing? */
export const settingsMatch = (query, hasWarnings) => hasWarnings && matches(query, 'warning settings alerts rcon remote console automatically');
