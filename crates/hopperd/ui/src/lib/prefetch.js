// Asking ahead, so a project page and its pictures are there by the time
// they're opened: a Browse result's page is fetched while the pointer rests
// on it, and full-size gallery images warm the browser's cache a couple at
// a time while the page is open.
import { api } from './api.js';

/** The `/browse/pack` query for a project, the same wherever it's asked. */
export function packQuery(source, id, type, into = '') {
  const u = new URLSearchParams({ source, id, type });
  if (into && type !== 'modpacks') u.set('into', into);
  return u.toString();
}

// A page asked ahead stays good for a minute: long enough to be clicked,
// short enough that what's installed is still right.
const FRESH = 60 * 1000;
const pages = new Map(); // query -> { at, promise }

/** A project's page, from a fetch already on its way when there is one. `fresh` asks again. */
export function packPage(query, { fresh = false } = {}) {
  const hit = pages.get(query);
  if (!fresh && hit && Date.now() - hit.at < FRESH) return hit.promise;
  const promise = api(`/browse/pack?${query}`);
  pages.set(query, { at: Date.now(), promise });
  promise.catch(() => pages.get(query)?.promise === promise && pages.delete(query));
  while (pages.size > 30) pages.delete(pages.keys().next().value);
  return promise;
}

const loaded = new Set(); // urls the browser has (or is getting)
const queue = []; // { url, group } waiting for a free slot
const running = new Map(); // url -> { img, group }
const SLOTS = 2;

function next() {
  while (running.size < SLOTS && queue.length) {
    const { url, group } = queue.shift();
    if (loaded.has(url)) continue;
    const img = new Image();
    img.decoding = 'async';
    img.fetchPriority = 'low';
    const done = () => {
      running.delete(url);
      next();
    };
    img.onload = () => (loaded.add(url), done());
    img.onerror = done;
    running.set(url, { img, group });
    img.src = url;
  }
}

/** Fetches `url` now, ahead of the page's own images (the viewer's next and previous ones). */
export function preloadNow(url) {
  if (!url || loaded.has(url)) return;
  loaded.add(url);
  const img = new Image();
  img.decoding = 'async';
  img.fetchPriority = 'high';
  img.onerror = () => loaded.delete(url);
  img.src = url;
}

/** Warms `urls` in the background, two at a time, under `group` (see `cancelWarm`). */
export function warm(urls, group) {
  for (const url of urls) {
    if (url && !loaded.has(url) && !running.has(url) && !queue.some((q) => q.url === url)) queue.push({ url, group });
  }
  next();
}

/** Stops warming what `group` asked for: the page was closed or moved on. */
export function cancelWarm(group) {
  for (let i = queue.length - 1; i >= 0; i--) if (queue[i].group === group) queue.splice(i, 1);
  for (const [url, r] of running) {
    if (r.group !== group) continue;
    // An image whose source goes stops downloading.
    r.img.onload = r.img.onerror = null;
    r.img.src = '';
    running.delete(url);
  }
  next();
}
