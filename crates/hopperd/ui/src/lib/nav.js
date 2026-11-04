// Back and forward through the panel's own places. Pages are hash routes
// (a new hash is a new history entry); a page can also add entries of its
// own for places that keep the same address (Browse's filters), carrying
// what it needs to come back in `history.state`. Shell numbers each entry
// (`hopperNav`) to grey its arrows out at either end.

/** Fired on the window when a page added an entry (Shell renumbers). */
export const NAV_EVENT = 'hopper-nav';

const state = () => (history.state && typeof history.state === 'object' ? history.state : {});

/** A new entry at the same address, carrying `extra` (the old one stays behind it). */
export function navPush(extra) {
  const at = state().hopperNav;
  history.pushState({ ...extra, hopperNav: (typeof at === 'number' ? at : 0) + 1 }, '');
  dispatchEvent(new Event(NAV_EVENT));
}

/** Adds `extra` to the entry shown now. */
export function navReplace(extra) {
  history.replaceState({ ...state(), ...extra }, '');
}

/** What the entry shown now carries under `key`. */
export function navGet(key) {
  return state()[key];
}

/**
 * Back (the mouse button, Alt+Left, the arrow) closes what's open on top
 * first, as Hopper desktop does: the topmost dialog gets an Escape, which
 * every dialog answers by closing. True when there was one.
 */
export function closeTopOverlay() {
  const open = [...document.querySelectorAll('[role="dialog"]')].filter((el) => el.isConnected && el.getClientRects().length > 0);
  const top = open[open.length - 1];
  if (!top) return false;
  top.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
  return true;
}
