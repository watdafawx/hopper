// Panels turned off on this page (lib/panels.svelte.js): the browser asks
// them nothing and their servers leave the list, the filters and bulk
// actions. Plain functions, so tests can run them without the page.

/** The panels that are on: the only ones to start, list or act on. */
export const onOnly = (list) => list.filter((p) => !p.off);

/** The panels that are off, for the list's "Turned off" lines. */
export const offOnly = (list) => list.filter((p) => p.off);

/** What the home panel keeps per panel: address, name, and off when off. */
export const savedList = (list) => list.map((p) => (p.off ? { url: p.url, name: p.name, off: true } : { url: p.url, name: p.name }));

/** The panels the list's refresh asks again: on, and not answering. */
export const stuck = (list) => onOnly(list).filter((p) => p.state === 'reconnecting' || p.state === 'offline' || p.state === 'refused' || p.state === 'loading');

/** Wording shared with Hopper desktop's panel switch. */
export const SWITCH_TIP = 'On: this panel is checked and its servers are listed. Off: nothing asks it anything and its servers are hidden. It stays set up, so turning it on again needs nothing else.';
