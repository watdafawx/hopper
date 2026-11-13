// Calls to hopperd's API. Every request carries X-Hopperd, which the panel
// requires from browsers (other sites can't add it), and X-Hopperd-Client,
// so the change events it causes don't reload this tab (lib/changes.svelte.js).
//
// Calls go to this panel, or to another panel added to this page
// (lib/panels.svelte.js): `target.panel` is the panel whose server page is
// open, so a server page and everything in it talks to its own panel with
// that panel's browser session (a bearer token, never this panel's cookie).
// Calls about this panel itself (its tasks, updates, pairing) pass
// `home: true`; the server list passes the `panel` it wants.
import { clientId } from './changes.svelte.js';
import { createBrake } from './brake.js';
import { say } from './toast.svelte.js';
import { tokenFor } from './panelTokens.js';

// Identical GETs at the same moment share one request, and a page asking
// for the same thing in a loop is stopped here (lib/brake.js).
const brake = createBrake({
  onLoop: (text) => console.error(text),
  onToast: (text) => say(text, true),
});
const inflight = new Map();

/** The other panel the open server page belongs to; null for this panel. */
export const target = { panel: null };

/** The start of a server page's address on the panel whose page is open:
 * '' here, `/p/<id>` on another panel (`${here()}/server/<id>/...`). */
export const here = () => (target.panel ? `/p/${encodeURIComponent(target.panel.id)}` : '');

/** Set by lib/panels.svelte.js: told when another panel says "sign in first". */
export const hooks = { signedOut: null };

export class ApiError extends Error {
  constructor(status, message, data = null) {
    super(message);
    this.status = status;
    /** The reply's JSON, for flags like need_code. */
    this.data = data;
  }
}

/**
 * `poll: true` is for requests that are meant to repeat fast (a job's
 * progress): the loop brake leaves them alone. `timeout` (ms) gives up on
 * a panel that doesn't answer.
 */
export function api(path, { method = 'GET', body, poll = false, home = false, panel, timeout } = {}) {
  const p = panel !== undefined ? panel : home ? null : target.panel;
  if (method !== 'GET' || body) return send(p, path, method, body, timeout);
  const key = `${p?.url || ''}${path}`;
  const shared = inflight.get(key);
  if (shared) return shared.then((d) => (d && typeof d === 'object' ? structuredClone(d) : d));
  if (!poll && !brake.allow(key)) {
    return Promise.reject(new ApiError(0, 'This page asked for the same thing too often, so it stopped. Reload the page.', { loop: true }));
  }
  const q = send(p, path, method, body, timeout).finally(() => inflight.delete(key));
  inflight.set(key, q);
  return q;
}

/** The headers a call to panel `p` (null: this one) carries. */
export function headersFor(p, json = false) {
  const h = { 'X-Hopperd': '1', 'X-Hopperd-Client': clientId, ...(json ? { 'Content-Type': 'application/json' } : {}) };
  const tok = p && tokenFor(p.url);
  if (tok) h.Authorization = `Bearer ${tok}`;
  return h;
}

/** Where panel `p`'s API lives (null: this panel, a relative path). */
export const apiBase = (p) => (p ? `${p.url}/api/v1` : '/api/v1');

async function send(p, path, method, body, timeout) {
  let res;
  try {
    res = await fetch(`${apiBase(p)}${path}`, {
      method,
      headers: headersFor(p, !!body),
      body: body ? JSON.stringify(body) : undefined,
      // Another panel never gets this browser's cookies: only its token.
      credentials: p ? 'omit' : 'same-origin',
      cache: p ? 'no-store' : undefined,
      signal: timeout ? AbortSignal.timeout(timeout) : undefined,
    });
  } catch (e) {
    if (!p) throw e;
    throw new ApiError(0, e?.name === 'TimeoutError' ? `${p.name || p.url} didn't answer in time.` : `Can't reach ${p.name || p.url}.`, { network: true });
  }
  let data = null;
  try {
    data = await res.json();
  } catch {
    // empty body
  }
  if (p && res.status === 401) hooks.signedOut?.(p);
  if (!res.ok) throw new ApiError(res.status, data?.error || `${res.status} ${res.statusText}`, data);
  return data;
}

export const post = (path, body = {}, o = {}) => api(path, { ...o, method: 'POST', body });
export const put = (path, body = {}, o = {}) => api(path, { ...o, method: 'PUT', body });
export const del = (path, o = {}) => api(path, { ...o, method: 'DELETE' });
