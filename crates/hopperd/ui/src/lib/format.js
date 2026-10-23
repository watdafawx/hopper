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

const PRIVATE = [/^10\./, /^192\.168\./, /^172\.(1[6-9]|2\d|3[01])\./, /\.local$/];
const TAILSCALE = [/^100\.(6[4-9]|[7-9]\d|1[01]\d|12[0-7])\./, /\.ts\.net$/];

/** How the browser reached the panel: Local, LAN, Tailscale or Internet. */
export function pathKind(host = location.hostname) {
  if (host === 'localhost' || host === '127.0.0.1' || host === '[::1]') return 'Local';
  if (PRIVATE.some((r) => r.test(host))) return 'LAN';
  if (TAILSCALE.some((r) => r.test(host))) return 'Tailscale';
  return 'Internet';
}
