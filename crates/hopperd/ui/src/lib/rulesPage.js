// The Rules page: webhooks ("Messages"), alert thresholds ("Warnings") and
// automations ("Actions") on one page. This is the page's own logic: which
// part a filter shows, what the search matches, the counts on the filter.

export const FILTERS = [
  ['all', 'All'],
  ['messages', 'Messages'],
  ['warnings', 'Warnings'],
  ['actions', 'Actions'],
];

/** Settings used to be three pages; their old links open the matching part. */
const OLD = { webhooks: 'messages', alerts: 'warnings', automations: 'actions' };

/** Is `id` one of the three old Settings pages the Rules page replaced? */
export const isOldRulesPage = (id) => id in OLD;

/** The filter an old link (or a new one) starts on. */
export const startFilter = (id) => OLD[id] || (FILTERS.some(([k]) => k === id) ? id : 'all');

/** Does the filter show this part (`messages`, `warnings`, `actions`)? */
export const showPart = (filter, part) => filter === 'all' || filter === part;

/** Search: every word has to appear in one of the texts, case ignored. */
export function matches(query, ...texts) {
  const words = String(query || '').toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return true;
  const hay = texts.filter((t) => t != null).join(' ').toLowerCase();
  return words.every((w) => hay.includes(w));
}

/** The alert switches, with plain names (also what the search matches). */
export const WARNINGS = [
  ['crash', 'Server crashed'],
  ['tps_on', 'Low TPS'],
  ['disk_on', 'Drive nearly full'],
  ['ram_on', 'PC memory nearly full'],
  ['node_on', 'Node offline'],
];

/** Warning words that match the query. The Warnings card stays when any
 * does, or when the query names the card itself. */
export function warningsMatch(query, hasNodes) {
  if (!String(query || '').trim()) return true;
  return matches(query, 'warnings alerts', ...WARNINGS.filter(([k]) => hasNodes || k !== 'node_on').map(([, l]) => l));
}

/** Numbers on the filter: how many of each. `alertSettings` null while loading. */
export function counts(hooks, alertSettings, rules, hasNodes = false) {
  const messages = hooks.length;
  const actions = rules.length;
  const warnings = alertSettings ? WARNINGS.filter(([k]) => hasNodes || k !== 'node_on').length : 0;
  return { all: messages + warnings + actions, messages, warnings, actions };
}

/** How many things need a look: a message that failed to send, an action
 * whose last run failed. Only counts what is turned on. */
export function failing(hooks, rules) {
  const h = hooks.filter((x) => x.enabled && x.last_error).length;
  const r = rules.filter((x) => x.enabled && x.history?.[0] && !x.history[0].ok).length;
  return h + r;
}
