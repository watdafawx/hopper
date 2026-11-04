// Change events from the live feed: something about a server changed (in
// another tab, another browser, Hopper desktop, or on disk), so the page
// showing it loads again. An event only names what changed:
// { server, areas: ['mods', 'configs', …], by }. See crates/hopperd/src/changes.rs.
//
// A page subscribes with one line, and the effect re-subscribes when the
// server changes:
//   $effect(() => onChange(server.id, ['mods'], load));

/** This tab, so a change it made itself doesn't reload what it shows. */
export const clientId = (globalThis.crypto?.randomUUID?.() ?? `${Date.now()}-${Math.random()}`).slice(0, 64);

const subs = new Set();

/**
 * Calls `reload` when `areas` of server `id` change (`id` null: any
 * server). Waits for a quiet moment so a burst is one reload, never runs
 * two at once, and waits while the tab is hidden. Returns the unsubscribe.
 * `own: true` also reloads on this tab's own changes.
 */
export function onChange(id, areas, reload, { own = false, quiet = 300 } = {}) {
  const sub = { id, areas, reload, own, quiet, timer: null, running: false, again: false, hidden: false };
  subs.add(sub);
  return () => {
    clearTimeout(sub.timer);
    subs.delete(sub);
  };
}

function wants(sub, msg) {
  if (msg.by && msg.by === clientId && !sub.own) return false;
  if (msg.server && sub.id && msg.server !== sub.id) return false;
  return msg.areas.includes('*') || msg.areas.some((a) => sub.areas.includes(a));
}

function fire(sub) {
  if (!subs.has(sub)) return;
  if (document.hidden) {
    sub.hidden = true;
    return;
  }
  if (sub.running) {
    sub.again = true;
    return;
  }
  sub.running = true;
  Promise.resolve()
    .then(() => sub.reload())
    .catch(() => {})
    .finally(() => {
      sub.running = false;
      if (sub.again) {
        sub.again = false;
        fire(sub);
      }
    });
}

function soon(sub) {
  clearTimeout(sub.timer);
  sub.timer = setTimeout(() => fire(sub), sub.quiet);
}

/** A `changed` message from the live feed. */
export function dispatch(msg) {
  if (!Array.isArray(msg.areas)) return;
  for (const sub of subs) if (wants(sub, msg)) soon(sub);
}

/** The feed came back after a drop: events may be lost, so every open
 * page loads again. */
export function resync() {
  for (const sub of subs) soon(sub);
}

if (typeof document !== 'undefined') {
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) return;
    for (const sub of subs) {
      if (sub.hidden) {
        sub.hidden = false;
        soon(sub);
      }
    }
  });
}
