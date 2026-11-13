// Other panels shown on this page (multi-panel B2, design/multi-panel.md).
// The browser talks to each one directly with its own browser session (a
// bearer token from lib/panelTokens.js) and its own live WebSocket, opened
// with a one-use ticket. This panel only remembers the list of addresses
// and names per account (/auth/me/panels); never a token.
//
// One slow or dead panel never holds up anything else: each has its own
// timeouts and reconnects on its own, and the list shows its last servers
// dimmed with "Reconnecting", then "Offline" after a grace period. A
// session that ended shows "Sign in again" on that group only; there is no
// retry with a saved password (none is kept).
import { api, hooks, ApiError, apiBase, headersFor, target } from './api.js';
import { tokenFor, setToken, forgetToken } from './panelTokens.js';
import { session, ownerLike } from './me.svelte.js';
import { savedList, stuck } from './panelOff.js';

/** Lists and sign-in checks give up after this (a dead panel mustn't hold the page). */
export const LIST_TIMEOUT = 5000;
/** "Reconnecting" turns into "Offline" after this long without the panel. */
export const OFFLINE_AFTER = 15000;
const PING_EVERY = 10000;
const MAX_LINES = 2000;
const SAMPLES = 120;

export const panels = $state({ list: [], loaded: false });

/** Things that aren't shown: sockets, timers, followed consoles. */
const rt = new Map();
const runtime = (p) => {
  let r = rt.get(p.id);
  if (!r) rt.set(p.id, (r = { socket: null, retry: 0, timer: 0, ping: 0, grace: 0, followed: new Set(), stopped: false }));
  return r;
};

/** A panel's id in addresses (`#/p/<id>/server/...`): its host and port. */
export function panelId(url) {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

export const byId = (id) => panels.list.find((p) => p.id === id) || null;

function make({ url, name, off = false }) {
  return {
    id: panelId(url), url, name: name || panelId(url),
    /** Turned off on this page: nothing asks it anything (lib/panelOff.js). */
    off: !!off,
    /** loading | ok | reconnecting | offline | signin | refused | old | setup | off */
    state: off ? 'off' : 'loading',
    /** Why it needs a sign-in: new (never signed in here) | expired | out. */
    reason: '',
    error: '', version: '', ping: null, me: null, role: '',
    servers: [], host: null, connected: false, lastSeen: 0, downSince: 0,
    consoles: {}, history: {},
  };
}

// ---- the list (kept on this panel, per account) ----

export async function loadPanels() {
  let list = [];
  try {
    list = await api('/auth/me/panels', { home: true });
  } catch {
    // An older home panel, a device or an API token: no list.
  }
  for (const r of rt.values()) stopRuntime(r);
  rt.clear();
  panels.list = (list || []).map(make);
  panels.loaded = true;
  for (const p of panels.list) if (!p.off) start(byId(p.id));
}

async function saveList() {
  try {
    await api('/auth/me/panels', { home: true, method: 'PUT', body: { panels: savedList(panels.list) } });
  } catch {
    // Kept for this page; the next change tries again.
  }
}

/** Adds a checked panel (from `check`) and signs in to it. */
export async function addPanel(url, name) {
  if (!byId(panelId(url))) panels.list.push(make({ url, name }));
  await saveList();
  const p = byId(panelId(url));
  start(p);
  return p;
}

/** Turns a panel on or off on this page. Off stops everything that asks
 * it (socket, pings, retries) and drops its servers; on starts it again at
 * once with the sign-in it had. */
export async function setOff(p, off) {
  const r = rt.get(p.id);
  if (r) stopRuntime(r);
  rt.delete(p.id);
  p.off = off;
  p.connected = false;
  p.downSince = 0;
  if (off) {
    Object.assign(p, { state: 'off', servers: [], host: null, consoles: {}, history: {}, ping: null, error: '' });
  } else {
    p.state = 'loading';
    start(p);
  }
  await saveList();
}

/** The list's refresh: every panel that's on and not answering, asked again now. */
export function retryStuck() {
  for (const p of stuck(panels.list)) retry(p);
}

export async function removePanel(p) {
  const r = rt.get(p.id);
  if (r) stopRuntime(r);
  rt.delete(p.id);
  if (tokenFor(p.url)) {
    // End the session on that panel too, so it doesn't linger in its list.
    fetch(`${apiBase(p)}/auth/logout`, { method: 'POST', headers: headersFor(p), credentials: 'omit', signal: AbortSignal.timeout(LIST_TIMEOUT) }).catch(() => {});
  }
  forgetToken(p.url);
  panels.list = panels.list.filter((q) => q.id !== p.id);
  await saveList();
}

// ---- talking to one panel ----

/** GET /hello on another panel: open, never cached, CORS for listed pages. */
async function hello(url, ms = LIST_TIMEOUT) {
  const res = await fetch(`${url}/api/v1/hello`, { credentials: 'omit', cache: 'no-store', signal: AbortSignal.timeout(ms) });
  if (!res.ok) throw new Error(`${res.status}`);
  return res.json();
}

/** True when `url` answers at all, readable or not (a no-cors probe: it
 * resolves when the panel is up with a certificate the browser accepts,
 * and fails when it's down, blocked or has an untrusted certificate). */
async function reachable(url, ms = LIST_TIMEOUT) {
  try {
    await fetch(`${url}/api/v1/hello`, { mode: 'no-cors', credentials: 'omit', cache: 'no-store', signal: AbortSignal.timeout(ms) });
    return true;
  } catch {
    return false;
  }
}

const isLocal = (host) => host === 'localhost' || host === '127.0.0.1' || host === '[::1]' || host.endsWith('.localhost');

/** Whether this (secure) page may call `url` at all (mixed content). */
const mixed = (url) => location.protocol === 'https:' && url.startsWith('http:') && !isLocal(new URL(url).hostname);

/** Fills in name, version, role and state from /hello and /auth/me. */
async function refresh(p) {
  const r = runtime(p);
  const t = performance.now();
  let h;
  try {
    h = await hello(p.url);
  } catch (e) {
    // Turned off while it was asked: the answer is dropped.
    if (p.off) return;
    // No answer in time: it's down, no need for a second 5 s wait to say so.
    if (e?.name === 'TimeoutError') down(p);
    else if (await reachable(p.url)) {
      p.state = 'refused';
      p.error = '';
    } else down(p);
    return;
  }
  if (p.off) return;
  p.ping = Math.round(performance.now() - t);
  p.version = h.version || '';
  p.caps = h.capabilities || [];
  if (h.name && h.name !== p.name) {
    p.name = h.name;
    saveList();
  }
  if (!h.capabilities?.includes('web.remote')) {
    p.state = 'old';
    return;
  }
  if (h.setup_needed) {
    p.state = 'setup';
    return;
  }
  if (!tokenFor(p.url)) {
    p.state = 'signin';
    p.reason = p.reason || 'new';
    return;
  }
  try {
    p.me = await api('/auth/me', { panel: p, timeout: LIST_TIMEOUT });
    if (p.off) return;
    p.role = p.me?.role || '';
    p.reason = '';
    if (p.state !== 'ok' || !r.socket) p.state = p.connected ? 'ok' : 'loading';
  } catch (e) {
    if (e.status === 401) return; // hooks.signedOut set "Sign in again"
    if (e.status === 403) {
      p.state = 'refused';
      p.error = e.message;
    } else down(p);
  }
}

/** The panel went away: "Reconnecting" now, "Offline" after the grace period. */
function down(p) {
  if (p.off) return;
  const r = runtime(p);
  p.connected = false;
  if (!p.downSince) p.downSince = Date.now();
  const left = OFFLINE_AFTER - (Date.now() - p.downSince);
  p.state = left <= 0 ? 'offline' : 'reconnecting';
  clearTimeout(r.grace);
  if (left > 0) r.grace = setTimeout(() => { if (p.state === 'reconnecting') p.state = 'offline'; }, left);
}

function stopRuntime(r) {
  r.stopped = true;
  clearTimeout(r.timer);
  clearTimeout(r.grace);
  clearInterval(r.ping);
  const s = r.socket;
  r.socket = null;
  if (s) {
    s.onclose = null;
    s.close();
  }
}

async function start(p) {
  if (p.off) return;
  const r = runtime(p);
  r.stopped = false;
  await refresh(p);
  if (r.stopped) return;
  if (p.me && (p.state === 'loading' || p.state === 'ok')) openSocket(p);
  else if (p.state === 'reconnecting' || p.state === 'offline' || p.state === 'refused') later(p);
  clearInterval(r.ping);
  r.ping = setInterval(() => pingOnce(p), PING_EVERY);
}

/** Every 10 s: the round trip and version of a panel that's up. */
async function pingOnce(p) {
  if (document.hidden || !p.connected) return;
  const t = performance.now();
  try {
    const h = await hello(p.url);
    p.ping = Math.round(performance.now() - t);
    p.version = h.version || p.version;
  } catch {
    // The socket notices a dead panel.
  }
}

/** Tries again later, backing off like the home panel's feed (300 ms doubling to ~20 s). */
function later(p) {
  const r = runtime(p);
  if (r.stopped || p.state === 'signin') return;
  clearTimeout(r.timer);
  r.retry = Math.min(r.retry + 1, 6);
  r.timer = setTimeout(async () => {
    await refresh(p);
    if (r.stopped) return;
    if (p.me && p.state !== 'signin' && p.state !== 'refused' && p.state !== 'old') openSocket(p);
    else if (p.state !== 'signin' && p.state !== 'old') later(p);
  }, 300 * 2 ** r.retry);
}

/** Try again now (the group's "Try again"). */
export function retry(p) {
  if (p.off) return;
  const r = runtime(p);
  r.retry = 0;
  clearTimeout(r.timer);
  start(p);
}

function sample(p, key, v) {
  const h = p.history[key] || [];
  h.push(Math.max(0, Math.min(1, v || 0)));
  if (h.length > SAMPLES) h.shift();
  p.history[key] = h;
}

async function openSocket(p) {
  if (p.off) return;
  const r = runtime(p);
  if (r.socket || r.stopped) return;
  let t;
  try {
    t = await api('/ws/ticket', { panel: p, method: 'POST', body: {}, timeout: LIST_TIMEOUT });
  } catch (e) {
    if (e.status === 401) return;
    if (e.status === 403) {
      p.state = 'refused';
      p.error = e.message;
      return;
    }
    down(p);
    later(p);
    return;
  }
  if (r.socket || r.stopped || p.off) return;
  const ws = new WebSocket(`${p.url.replace(/^http/, 'ws')}/api/v1/ws`, t.protocols);
  r.socket = ws;
  ws.onopen = () => {
    p.connected = true;
    p.downSince = 0;
    p.state = 'ok';
    r.retry = 0;
    clearTimeout(r.grace);
    for (const id of r.followed) ws.send(JSON.stringify({ op: 'console', server: id }));
  };
  ws.onmessage = (ev) => {
    let msg;
    try {
      msg = JSON.parse(ev.data);
    } catch {
      return;
    }
    if (msg.t === 'snapshot') {
      p.servers = msg.servers || [];
      p.host = msg.host;
      p.lastSeen = Date.now();
      if (msg.host?.mem_total) {
        sample(p, 'host.cpu', msg.host.cpu / 100);
        sample(p, 'host.mem', msg.host.mem_used / msg.host.mem_total);
      }
      for (const s of p.servers) {
        sample(p, `${s.id}.cpu`, s.cpu / 100);
        sample(p, `${s.id}.mem`, msg.host?.mem_total ? s.mem_bytes / msg.host.mem_total : 0);
      }
    } else if (msg.t === 'console') {
      const prev = msg.reset ? [] : p.consoles[msg.server] || [];
      const next = prev.concat(msg.lines);
      p.consoles[msg.server] = next.length > MAX_LINES ? next.slice(next.length - MAX_LINES) : next;
    }
  };
  ws.onclose = () => {
    if (r.socket === ws) r.socket = null;
    if (r.stopped || p.state === 'signin') {
      p.connected = false;
      return;
    }
    down(p);
    later(p);
  };
}

// Another panel said "sign in first" (revoked, expired, signed out there):
// that group asks for the password; nothing retries on its own.
hooks.signedOut = (q) => {
  const p = byId(q.id);
  if (!p) return;
  forgetToken(p.url);
  const r = runtime(p);
  clearTimeout(r.timer);
  const s = r.socket;
  r.socket = null;
  if (s) {
    s.onclose = null;
    s.close();
  }
  p.connected = false;
  if (p.state !== 'signin') p.reason = p.reason === 'new' ? 'new' : 'expired';
  p.state = 'signin';
};

/**
 * Signs in to another panel with a browser session (the panel must list
 * this page). `keep: false` keeps the token for this tab only. Throws an
 * ApiError whose `data.need_code` asks for the two-factor code.
 */
export async function signIn(p, { name = '', password, code = null, keep = true }) {
  let res;
  try {
    res = await fetch(`${apiBase(p)}/auth/login`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', 'X-Hopperd': '1' },
      body: JSON.stringify({ name, password, code, bearer: true }),
      credentials: 'omit',
      signal: AbortSignal.timeout(LIST_TIMEOUT * 2),
    });
  } catch {
    throw new ApiError(0, `Can't reach ${p.name}.`);
  }
  let data = null;
  try {
    data = await res.json();
  } catch {
    // empty
  }
  if (!res.ok || !data?.token) throw new ApiError(res.status, data?.error || `${res.status} ${res.statusText}`, data);
  setToken(p.url, data.token, keep);
  p.reason = '';
  p.state = 'loading';
  const r = runtime(p);
  r.retry = 0;
  await start(p);
}

/** Signs this page out of one panel (its group then offers Sign in). */
export async function signOut(p) {
  await fetch(`${apiBase(p)}/auth/logout`, { method: 'POST', headers: headersFor(p), credentials: 'omit', signal: AbortSignal.timeout(LIST_TIMEOUT) }).catch(() => {});
  hooks.signedOut(p);
  p.reason = 'out';
}

// ---- consoles on another panel ----

export function followOn(p, id) {
  const r = runtime(p);
  r.followed.add(id);
  if (r.socket?.readyState === WebSocket.OPEN) r.socket.send(JSON.stringify({ op: 'console', server: id }));
}

export function unfollowOn(p, id) {
  const r = runtime(p);
  r.followed.delete(id);
  if (r.socket?.readyState === WebSocket.OPEN) r.socket.send(JSON.stringify({ op: 'unconsole', server: id }));
}

// ---- the server page of another panel ----

let homeSession = null;

/** A server page of panel `p` opened (null: this panel's): its calls, role
 * and folders follow that panel until it closes. */
export function enter(p) {
  target.panel = p;
  if (p) {
    homeSession ??= { ...session };
    const me = p.me || {};
    session.owner = ownerLike(me);
    session.trueOwner = me.role === 'owner';
    session.role = me.role || '';
    session.home = me.panel_folder || '';
    session.servers = me.servers_folder || '';
  } else if (homeSession) {
    Object.assign(session, homeSession);
    homeSession = null;
  }
}

/** The page of panel `p` closed (only if nothing else took over). */
export function leave(p) {
  if (target.panel === p) enter(null);
}

// ---- adding a panel: what's in the way, step by step ----

/** "rig:8790" -> candidates to try: https first, then http. */
export function candidates(input) {
  const s = input.trim().replace(/\/+$/, '');
  if (!s) return [];
  if (/^https?:\/\//i.test(s)) {
    try {
      const u = new URL(s);
      if (u.pathname !== '/' || u.search || u.hash || u.username) return [];
      return [u.origin];
    } catch {
      return [];
    }
  }
  const out = [];
  for (const scheme of ['https', 'http']) {
    try {
      const u = new URL(`${scheme}://${s}`);
      if (u.pathname === '/' && !u.search && !u.hash && !u.username) out.push(u.origin);
    } catch {
      // not an address
    }
  }
  return out;
}

/**
 * Checks an address before adding it. Resolves to { ok, url, name,
 * version, problem, steps }: `steps` are the checks in order, each
 * { text, ok } (ok null = not reached); `problem` names the first that
 * failed: bad | self | dup | mixed | unreachable | cert | notlisted | old | setup.
 */
export async function check(input) {
  const list = candidates(input);
  const steps = [];
  if (!list.length) return { ok: false, problem: 'bad', steps };
  if (list.includes(location.origin)) return { ok: false, problem: 'self', url: location.origin, steps };
  const dup = list.map((u) => byId(panelId(u))).find(Boolean);
  if (dup) return { ok: false, problem: 'dup', url: dup.url, name: dup.name, steps };
  let last = null;
  for (const url of list) {
    if (mixed(url)) {
      // A bare address tried https first: when that failed on its
      // certificate, the certificate is the thing to fix, not http.
      if (!last) last = { ok: false, problem: 'mixed', url, steps: [{ text: 'This secure page may talk to it', ok: false }] };
      continue;
    }
    let h = null;
    let slow = false;
    try {
      h = await hello(url);
    } catch (e) {
      // Unreadable: up but not listing this page, or not reachable at all.
      slow = e?.name === 'TimeoutError';
    }
    if (h) {
      const tls = url.startsWith('https:') ? 'Your browser trusts its certificate' : 'Plain HTTP: fine on your own network or Tailscale';
      const s = [
        { text: `Reachable: ${h.name || panelId(url)}, Hopper Server ${h.version || ''}`.trim(), ok: true },
        { text: tls, ok: true },
        { text: `${h.name || 'It'} lists this page`, ok: true },
      ];
      if (!h.capabilities?.includes('web.remote')) return { ok: false, problem: 'old', url, name: h.name, version: h.version, steps: [...s, { text: 'New enough for other pages', ok: false }] };
      if (h.setup_needed) return { ok: false, problem: 'setup', url, name: h.name, version: h.version, steps: s };
      return { ok: true, url, name: h.name || panelId(url), version: h.version, steps: s };
    }
    if (!slow && (await reachable(url))) {
      return {
        ok: false, problem: 'notlisted', url,
        steps: [
          { text: 'Reachable', ok: true },
          { text: url.startsWith('https:') ? 'Your browser trusts its certificate' : 'Plain HTTP: fine on your own network or Tailscale', ok: true },
          { text: "It lists this page", ok: false },
        ],
      };
    }
    const https = url.startsWith('https:');
    const r = { ok: false, problem: https ? 'cert' : 'unreachable', url, steps: [{ text: https ? 'Reachable with a certificate your browser trusts' : 'Reachable', ok: false }] };
    // A bare address tried https first: if plain http fails too, it's most
    // likely off or the address is wrong (the certificate hint stays).
    if (last?.problem === 'cert') last = { ...r, problem: 'unreachable', maybeCert: last.url };
    else if (!last || last.problem === 'mixed') last = r;
  }
  return last;
}
