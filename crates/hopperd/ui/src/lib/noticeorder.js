// Which notices the slot above a server's tabs shows, in what order, as
// Hopper desktop's (src/gpui/notice_slot.rs): what stops play first, then
// what needs a choice, then news; newest first within each; the page's own
// order breaks ties. The top two show as strips, the rest under "N more",
// grouped by tier.

export const TIERS = ['stop', 'choice', 'info'];
export const HEADING = { stop: 'Stops play', choice: 'Needs a choice', info: 'For your information' };
/** Strips shown before "N more". */
export const SHOWN = 2;

const rank = (t) => {
  const i = TIERS.indexOf(t);
  return i < 0 ? TIERS.length : i;
};

/** Indexes of `items` ({ tier, since }) in the order the slot shows them. */
export function order(items) {
  return items
    .map((_, i) => i)
    .sort((a, b) => rank(items[a].tier) - rank(items[b].tier) || (items[b].since || 0) - (items[a].since || 0) || a - b);
}

/** For tiers already in order: the indexes shown as strips, and the rest
 * grouped by tier for the "N more" list ([tier, indexes]). */
export function fold(tiers) {
  const shown = tiers.slice(0, SHOWN).map((_, i) => i);
  const groups = [];
  for (let i = SHOWN; i < tiers.length; i++) {
    const last = groups[groups.length - 1];
    if (last && last[0] === tiers[i]) last[1].push(i);
    else groups.push([tiers[i], [i]]);
  }
  return { shown, groups };
}

/** A notice's main button: filled on the slot's first strip only (one
 * filled button per region), outlined elsewhere. */
export const mainClass = (fit) => (fit?.lead ? 'btn sm pri' : 'btn sm');
