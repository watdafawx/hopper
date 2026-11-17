// The Rules page: webhooks ("Messages"), alert thresholds ("Warnings") and
// automations ("Actions") on one page. This is the page's own small logic:
// the filters, old links and the search. The rows themselves are lib/rulesList.js.

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

/** Search: every word has to appear in one of the texts, case ignored. */
export function matches(query, ...texts) {
  const words = String(query || '').toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return true;
  const hay = texts.filter((t) => t != null).join(' ').toLowerCase();
  return words.every((w) => hay.includes(w));
}
