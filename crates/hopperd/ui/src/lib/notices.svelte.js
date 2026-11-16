// The notice slot's list for one page: each <Notice> on the page (in any
// component under it, on any tab) puts itself here, and <NoticeSlot> shows
// them. A page makes its own with `provideNotices()`.
import { getContext, setContext, untrack } from 'svelte';

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

/** The page's list, or null outside a page that has a slot. */
export const pageNotices = () => getContext(KEY) || null;

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
