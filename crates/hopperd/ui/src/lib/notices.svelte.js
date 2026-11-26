// The notices of one page: each <Notice> on the page (in any component under
// it, on any tab) puts itself here. A server page docks them under its
// content (<NoticeDock>) and sums them up in the hero's chips (`needsChip`,
// `runChip`); the panel Overview lists them in its chip's popover
// (<NoticeList>). A page makes its own with `provideNotices()`.
import { getContext, setContext, untrack } from 'svelte';
import { order } from './noticeorder.js';
import { attentionLook, runLook } from './syncchip.js';

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

/** The notices in the order the list shows them (what stops play first). `run`: the run state's own
 * problems (a stuck start, a crash) instead of the others; they have their own chip. */
export function ordered(notices, run = false) {
  const all = Object.values(notices.items)
    .filter((n) => !!n.run === run)
    .sort((a, b) => a.at - b.at);
  return order(all).map((i) => all[i]);
}

/** Every notice of the page, run state's problems included, in the order the dock
 * (<NoticeDock>) shows them: what stops play first. */
export function docked(notices) {
  const all = Object.values(notices.items).sort((a, b) => a.at - b.at);
  return order(all).map((i) => all[i]);
}

// The strip a chip or a row marker asked the dock to show: the page's key, the notice's id (the
// first strip when it has none), whether to open its details, and a request number so the dock acts
// on each request once.
export const flashed = $state({ key: '', id: '', open: false, n: 0 });
export function flash(key, id = '', open = false) {
  flashed.key = key;
  flashed.id = id;
  flashed.open = open;
  flashed.n += 1;
}
// `page|notice@since` of problems that stop play whose details already opened by themselves in the
// dock. It outlives the dock, so leaving the page and coming back doesn't open them again.
export const stopSeen = new Set();

/** The "Needs you" entry for <SyncChip>'s `chips`, or null when the page has no notice. `key` names
 * the page (the server). With a `body` (the popover's snippet, <NoticeList>: the panel Overview) a
 * click opens that list, and a problem that stops play opens it once; without one the notices are in
 * the page's <NoticeDock> and a click flashes the first strip there. */
export function needsChip(notices, key, body = null) {
  const list = ordered(notices);
  const look = attentionLook(list.map((n) => n.tier), list[0]?.title || '');
  if (!look) return null;
  if (!body) return { id: `needs:${key}`, look, title: '', line: '', actions: [], onclick: () => flash(key, list[0].id) };
  return { id: `needs:${key}`, look, title: '', line: '', actions: [], body, wide: true, autoOpen: list.filter((n) => n.tier === 'stop').map((n) => n.id) };
}

/** The run state's chip for <SyncChip>'s `chips`: red "Crashed" or "Looks stuck · N min". Its notice
 * is a strip in the page's <NoticeDock>; a click flashes it. Null when the server has no such
 * problem. */
export function runChip(notices, key) {
  const list = ordered(notices, true);
  if (!list.length) return null;
  const crashed = list.find((n) => n.run === 'crashed');
  const problem = crashed ? { kind: 'crashed' } : { kind: 'stuck', minutes: list[0].minutes ?? null };
  return { id: `run:${key}`, look: runLook(problem), title: '', line: '', actions: [], onclick: () => flash(key, list[0].id) };
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
