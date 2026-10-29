// The live feed from /api/v1/ws: servers and host stats (about once a
// second) and the consoles the UI follows. Reconnects on its own, and
// after a reconnect checks whether the panel came back on a new version.
import { hello, seen, check } from './boot.svelte.js';

const MAX_LINES = 2000;

export const live = $state({
  connected: false,
  servers: [],
  host: null,
  /** Round trip to the panel in ms, from the last /hello. */
  ping: null,
});

/** Console lines per server id, oldest first. */
export const consoles = $state({});

/** The last two minutes of samples, for sparklines: host.cpu, host.mem,
 * and `${id}.cpu` / `${id}.mem` per server. Values are 0..1. */
export const history = $state({});
const SAMPLES = 120;

function sample(key, v) {
  const h = history[key] || [];
  h.push(Math.max(0, Math.min(1, v || 0)));
  if (h.length > SAMPLES) h.shift();
  history[key] = h;
}

function record(servers, host) {
  if (host?.mem_total) {
    sample('host.cpu', host.cpu / 100);
    sample('host.mem', host.mem_used / host.mem_total);
  }
  for (const s of servers) {
    sample(`${s.id}.cpu`, s.cpu / 100);
    sample(`${s.id}.mem`, host?.mem_total ? s.mem_bytes / host.mem_total : 0);
  }
}

let socket = null;
let retry = 0;
/** The feed was up once: an open after that is a reconnect. */
let wasUp = false;
const followed = new Set();

function send(msg) {
  if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify(msg));
}

export function connect() {
  if (socket) return;
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  socket = new WebSocket(`${proto}://${location.host}/api/v1/ws`);
  socket.onopen = () => {
    live.connected = true;
    retry = 0;
    if (wasUp) check();
    wasUp = true;
    for (const id of followed) send({ op: 'console', server: id });
  };
  socket.onmessage = (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.t === 'snapshot') {
      live.servers = msg.servers;
      live.host = msg.host;
      record(msg.servers, msg.host);
    } else if (msg.t === 'console') {
      const prev = msg.reset ? [] : consoles[msg.server] || [];
      const next = prev.concat(msg.lines);
      consoles[msg.server] = next.length > MAX_LINES ? next.slice(next.length - MAX_LINES) : next;
    }
  };
  socket.onclose = () => {
    live.connected = false;
    socket = null;
    retry = Math.min(retry + 1, 6);
    setTimeout(connect, 300 * 2 ** retry);
  };
}

export function disconnect() {
  const s = socket;
  socket = null;
  if (s) {
    s.onclose = null;
    s.close();
  }
  live.connected = false;
}

export function follow(id) {
  followed.add(id);
  send({ op: 'console', server: id });
}

export function unfollow(id) {
  followed.delete(id);
  send({ op: 'unconsole', server: id });
}

/** Measures the round trip every 10 s (and notices a new version). */
export function startPing() {
  const once = async () => {
    const t = performance.now();
    try {
      const h = await hello();
      live.ping = Math.round(performance.now() - t);
      seen(h);
    } catch {
      live.ping = null;
    }
  };
  once();
  return setInterval(once, 10000);
}
