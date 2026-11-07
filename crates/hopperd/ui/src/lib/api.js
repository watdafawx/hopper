// Calls to hopperd's API. Every request carries X-Hopperd, which the panel
// requires from browsers (other sites can't add it), and X-Hopperd-Client,
// so the change events it causes don't reload this tab (lib/changes.svelte.js).
import { clientId } from './changes.svelte.js';
import { createBrake } from './brake.js';
import { say } from './toast.svelte.js';

// Identical GETs at the same moment share one request, and a page asking
// for the same thing in a loop is stopped here (lib/brake.js).
const brake = createBrake({
  onLoop: (text) => console.error(text),
  onToast: (text) => say(text, true),
});
const inflight = new Map();

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
 * progress): the loop brake leaves them alone.
 */
export function api(path, { method = 'GET', body, poll = false } = {}) {
  if (method !== 'GET' || body) return send(path, method, body);
  const shared = inflight.get(path);
  if (shared) return shared.then((d) => (d && typeof d === 'object' ? structuredClone(d) : d));
  if (!poll && !brake.allow(path)) {
    return Promise.reject(new ApiError(0, 'This page asked for the same thing too often, so it stopped. Reload the page.', { loop: true }));
  }
  const p = send(path, method, body).finally(() => inflight.delete(path));
  inflight.set(path, p);
  return p;
}

async function send(path, method, body) {
  const res = await fetch(`/api/v1${path}`, {
    method,
    headers: { 'X-Hopperd': '1', 'X-Hopperd-Client': clientId, ...(body ? { 'Content-Type': 'application/json' } : {}) },
    body: body ? JSON.stringify(body) : undefined,
    credentials: 'same-origin',
  });
  let data = null;
  try {
    data = await res.json();
  } catch {
    // empty body
  }
  if (!res.ok) throw new ApiError(res.status, data?.error || `${res.status} ${res.statusText}`, data);
  return data;
}

export const post = (path, body = {}) => api(path, { method: 'POST', body });
export const del = (path) => api(path, { method: 'DELETE' });
