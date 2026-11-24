// The "Needs you" chip's list for one page: each <Notice> on the page (in any
// component under it, on any tab) puts itself here, and the chip
// (`needsChip`, listed by <NoticeList>) shows them. A page makes its own
// with `provideNotices()`.
import { getContext, setContext, untrack } from 'svelte';
import { order } from './noticeorder.js';
import { attentionLook } from './syncchip.js';

const KEY = Symbol('notices');

class Notices {
  items = $state.raw({});
  #seq = 0;
  /** A place in the page's order, for ties. */
  next() {
    return this.#seq++;
  }
  put(id, n) {
    // Called from effects: reading the list here must not make them rerun.
    untrack(() => (this.items = { ...this.items, [id]: n }));
  }
  drop(id) {
    untrack(() => {
      if (!(id in this.items)) return;
      const { [id]: _, ...rest } = this.items;
      this.items = rest;
    });
  }
}

export function provideNotices() {
  const n = new Notices();
  setContext(KEY, n);
  return n;
}

/** The page's list, or null outside a page that has a chip. */
export const pageNotices = () => getContext(KEY) || null;

/** The notices in the order the list shows them (what stops play first). */
export function ordered(notices) {
  const all = Object.values(notices.items).sort((a, b) => a.at - b.at);
  return order(all).map((i) => all[i]);
}

/** The "Needs you" entry for <SyncChip>'s `chips`, or null when the page has no notice. `key` names
 * the page (the server), so a problem that stops play opens the popover once per server. `body` is
 * the popover's snippet (<NoticeList>). */
export function needsChip(notices, key, body) {
  const list = ordered(notices);
  const look = attentionLook(list.map((n) => n.tier), list[0]?.title || '');
  if (!look) return null;
  return { id: `needs:${key}`, look, title: '', line: '', actions: [], body, wide: true, autoOpen: list.filter((n) => n.tier === 'stop').map((n) => n.id) };
}

// Notices closed in this browser, by key: they come back when the key does
// (a key names what the notice is about, so a new cause is a new key).
const closed = $state({});
export function isClosed(key) {
  if (!key) return false;
  if (key in closed) return closed[key];
  try {
    return localStorage.getItem(key) === '1';
  } catch {
    return false;
  }
}
export function close(key) {
  closed[key] = true;
  try {
    localStorage.setItem(key, '1');
  } catch {}
}
