export function bytes(b) {
  if (!b) return '0 B';
  const u = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0;
  while (b >= 1024 && i < u.length - 1) {
    b /= 1024;
    i++;
  }
  return `${b.toFixed(b >= 100 || i === 0 ? 0 : 1)} ${u[i]}`;
}

export function uptime(s) {
  if (s == null) return '';
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (d) return `${d} d ${h} h`;
  if (h) return `${h} h ${m} m`;
  if (m) return `${m} m`;
  return `${s} s`;
}

/** An -Xmx value ("12G") in bytes. */
export function xmx(v) {
  const m = /^(\d+)([kKmMgG]?)/.exec(v || '');
  if (!m) return 0;
  const mult = { k: 1024, m: 1024 ** 2, g: 1024 ** 3 }[m[2].toLowerCase()] || 1;
  return Number(m[1]) * mult;
}

/** Hover text for a server's RAM. The number is the whole Java process, and
 * -Xmx only caps Java's heap, so a server above its setting is normal. */
export function memTitle(used, cap) {
  if (!cap) return `The server uses ${bytes(used)}.`;
  const set = `The server uses ${bytes(used)}. Its memory setting lets Java use up to ${bytes(cap)}.`;
  return used > cap ? `${set} Java also uses some memory outside that limit, so going over it is normal.` : set;
}

const LOADER_NAMES = { neoforge: 'NeoForge', forge: 'Forge', fabric: 'Fabric', quilt: 'Quilt', paper: 'Paper', purpur: 'Purpur', vanilla: 'Vanilla' };

/** "neoforge 21.1.259" as "NeoForge 21.1.259", or just "NeoForge" with nameOnly. */
export function loaderText(l, nameOnly = false) {
  if (!l) return '';
  const [id, ...rest] = l.split(' ');
  const name = LOADER_NAMES[id.toLowerCase()] || id;
  return nameOnly ? name : [name, ...rest].join(' ');
}

const COLORS = ['#84cc16', '#60a5fa', '#f472b6', '#f59e0b', '#a78bfa', '#14b8a6', '#ef4444', '#a3a3ad'];

export function color(name) {
  let h = 0;
  for (const c of name) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return COLORS[h % COLORS.length];
}

export const stateLabel = {
  stopped: 'Stopped',
  starting: 'Starting',
  running: 'Running',
  stopping: 'Stopping',
  crashed: 'Crashed',
};

/** A server's state in words; a stop that is saving its worlds says so,
 * a start how far it has got. */
export const stateText = (s) => {
  if (s.state === 'stopping' && s.saving) return 'Saving worlds…';
  if (s.state === 'starting' && s.boot) return `Starting ${s.boot.percent}%`;
  return stateLabel[s.state];
};

/** "45s", "1m 20s", "3m": rounded like the server list says it. */
export function shortTime(secs) {
  const step = secs < 60 ? 5 : secs < 600 ? 10 : 60;
  const s = Math.max(5, Math.round(secs / step) * step);
  const m = Math.floor(s / 60);
  const r = s % 60;
  if (!m) return `${r}s`;
  return r ? `${m}m ${String(r).padStart(2, '0')}s` : `${m}m`;
}

/** A start's time left in words ("About 1m 20s left (usually 3m 40s)"). */
export function bootLeft(b) {
  if (!b) return '';
  if (b.slow) return `Taking longer than usual (usually ${shortTime(b.usual_secs)})`;
  if (b.left_secs != null) return `About ${shortTime(b.left_secs)} left (usually ${shortTime(b.usual_secs)})`;
  return 'First start: no estimate yet';
}

/** A start's progress for a tooltip: what it is doing, then the time. */
export const bootText = (s) => (s.state === 'starting' && s.boot ? `${s.boot.label} · ${bootLeft(s.boot)}` : '');

const PRIVATE = [/^10\./, /^192\.168\./, /^172\.(1[6-9]|2\d|3[01])\./, /\.local$/];
const TAILSCALE = [/^100\.(6[4-9]|[7-9]\d|1[01]\d|12[0-7])\./, /\.ts\.net$/];

const VIA = { local: 'Local', lan: 'LAN', tailscale: 'Tailscale', internet: 'Internet' };

/** How this browser reaches the panel. The panel's answer (`via` in /hello)
 * wins: it sees the real client address, also behind a reverse proxy, where
 * a public name can still point at the LAN. The address typed is the guess
 * until it answers. */
export const routeKind = (via) => VIA[via] ?? pathKind();

/** What an address looks like: Local, LAN, Tailscale or Internet. */
export function pathKind(host = location.hostname) {
  if (host === 'localhost' || host === '127.0.0.1' || host === '[::1]') return 'Local';
  if (PRIVATE.some((r) => r.test(host))) return 'LAN';
  if (TAILSCALE.some((r) => r.test(host))) return 'Tailscale';
  return 'Internet';
}

/** How healthy a TPS reading is: 20 is full speed, under 15 players
 * notice, under 10 it's badly lagging. */
export function tpsLevel(tps) {
  return tps >= 15 ? 'good' : tps >= 10 ? 'warn' : 'bad';
}
