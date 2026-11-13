// Files of a server on another panel (design/multi-panel.md, B4). This page
// signs in there with a token, which a plain <a href> or <img src> can't
// send, so:
// - downloads and "open in a new tab" ask that panel for a one-use link
//   (60 s, these files only) and follow it: nothing passes through this page;
// - pictures and text shown inside this page are read with the token and
//   shown from memory (a blob), so a folder of thumbnails doesn't use up
//   links.
// On this panel every URL stays as it is: the cookie does the work.
//
// URLs are written as this panel's own (`/api/v1/fs/<id>/file?path=…`);
// `linkBody` says which of them another panel can make a link for.

import { untrack } from 'svelte';
import { post, target, apiBase, headersFor } from './api.js';
import { linkBody } from './filelinks.js';

export { linkBody };

/** Where to go for `url` on panel `p` (null: this panel, the URL itself).
 * Throws with a message for the player when that panel can't give a link. */
export async function linkFor(url, p = target.panel) {
  if (!p) return url;
  const l = linkBody(url);
  if (!l) throw new Error(`This only works on ${p.name}'s own page.`);
  // Backups and files inside a zip came in a later Hopper Server ("web.files").
  if ((l.body.backup || l.body.entry) && p.caps && !p.caps.includes('web.files')) {
    throw new Error(`${p.name} runs an older Hopper Server. Update it to download this from here, or use its own page.`);
  }
  const r = await post(`/fs/${l.server}/link`, l.body, { panel: p });
  return `${p.url}${r.url}`;
}

/** Opens `url` (inline) in a new tab. The tab is opened at once, before the
 * link arrives, so the browser doesn't block it as a popup. */
export async function openTab(url, p = target.panel, onError = () => {}) {
  if (!p) {
    window.open(url, '_blank', 'noopener');
    return;
  }
  const w = window.open('', '_blank');
  if (w) w.opener = null;
  try {
    const to = await linkFor(url, p);
    if (w) w.location.href = to;
    else window.open(to, '_blank', 'noopener');
  } catch (e) {
    w?.close();
    onError(e);
  }
}

/** Reads `url` with panel `p`'s sign-in (this panel: its cookie). */
export function fetchFrom(url, p = target.panel, signal) {
  if (!p) return fetch(url, { credentials: 'same-origin', signal });
  return fetch(`${apiBase(p)}${url.replace(/^\/api\/v1/, '')}`, { headers: headersFor(p), credentials: 'omit', cache: 'no-store', signal });
}

/**
 * `<img use:panelImg={url}>`: the picture at `url` (this panel's own
 * address for it) from the panel whose page is open, or
 * `{ url, panel }`. On another panel it is read with its token when it
 * comes into view (a folder of thumbnails reads only what's shown), and an
 * `error` event fires on the <img> when that fails.
 */
export function panelImg(node, arg) {
  let obj = '';
  let stop = null;
  let seen = null;
  const clear = () => {
    stop?.abort();
    seen?.disconnect();
    if (obj) URL.revokeObjectURL(obj);
    obj = '';
  };
  function set(a) {
    clear();
    const { url, panel } = a && typeof a === 'object' ? { panel: target.panel, ...a } : { url: a, panel: target.panel };
    if (!url) return node.removeAttribute('src');
    if (!panel) return void (node.src = url);
    node.removeAttribute('src');
    const s = (stop = new AbortController());
    const load = () =>
      fetchFrom(url, panel, s.signal)
        .then((r) => (r.ok ? r.blob() : Promise.reject(new Error(String(r.status)))))
        .then((b) => {
          if (s.signal.aborted) return;
          obj = URL.createObjectURL(b);
          node.src = obj;
        })
        .catch(() => !s.signal.aborted && node.dispatchEvent(new Event('error')));
    if (node.loading !== 'lazy' || typeof IntersectionObserver === 'undefined') return void load();
    seen = new IntersectionObserver((es) => {
      if (es.some((e) => e.isIntersecting)) {
        seen.disconnect();
        load();
      }
    }, { rootMargin: '200px' });
    seen.observe(node);
  }
  set(arg);
  return { update: set, destroy: clear };
}

/**
 * A picture's `src` for an <img>, kept up to date: `url()` on this panel,
 * or the picture read with `panel()`'s token. Call it while a component
 * starts (it uses $effect). `failed` is true when it couldn't be read.
 */
export function panelSrc(url, panel = () => target.panel) {
  let src = $state('');
  let failed = $state(false);
  // Read through $derived so a live feed handing over a new server object
  // every second, with the same icon, doesn't read the picture again.
  const want = $derived(url() || '');
  const from = $derived(panel());
  $effect(() => {
    const u = want;
    const p = from;
    failed = false;
    if (!u || !p) {
      src = u || '';
      return;
    }
    src = '';
    let obj = '';
    const stop = new AbortController();
    // Only the address and the panel decide; whatever the call reads doesn't.
    untrack(() => fetchFrom(u, p, stop.signal))
      .then((r) => (r.ok ? r.blob() : Promise.reject(new Error(String(r.status)))))
      .then((b) => {
        if (stop.signal.aborted) return;
        obj = URL.createObjectURL(b);
        src = obj;
      })
      .catch(() => {
        if (!stop.signal.aborted) failed = true;
      });
    return () => {
      stop.abort();
      if (obj) URL.revokeObjectURL(obj);
    };
  });
  return {
    get src() {
      return src;
    },
    get failed() {
      return failed;
    },
  };
}
