// Which notices the slot above a server's tabs shows, in what order, as
// Hopper desktop's (src/gpui/notice_slot.rs): what stops play first, then
// what needs a choice, then news; newest first within each; the page's own
// order breaks ties. They are the "Needs you" chip's list, grouped by tier.

export const TIERS = ['stop', 'choice', 'info'];
export const HEADING = { stop: 'Stops play', choice: 'Needs a choice', info: 'For your information' };

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

/** For tiers already in order: their indexes grouped by tier ([tier, indexes]), the popover's list. */
export function group(tiers) {
  const groups = [];
  for (let i = 0; i < tiers.length; i++) {
    const last = groups[groups.length - 1];
    if (last && last[0] === tiers[i]) last[1].push(i);
    else groups.push([tiers[i], [i]]);
  }
  return groups;
}

/** A notice's main button: filled when `fit.lead`, outlined elsewhere (the
 * chip's list has no lead: the chip is the one filled thing it isn't). */
export const mainClass = (fit) => (fit?.lead ? 'btn sm pri' : 'btn sm');
