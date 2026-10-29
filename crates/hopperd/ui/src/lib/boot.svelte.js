// Notices when Hopper Server comes back from a restart on another version
// (or another build of its web UI) and reloads the page, so the new
// version's panel and data show without a manual refresh. Every place the
// panel reconnects goes through here: the live feed, the ping, and the
// update status while an update restarts.
//
// It doesn't reload over someone's work: with a confirm or form dialog
// open, unsaved edits, or text being typed, it shows a banner with a
// Reload button instead (`boot.updated`).
import { leave } from './leave.js';
import { dialog } from './dialog.svelte.js';

export const boot = $state({ updated: null });

/** What this page loaded with: { version, build, boot } from the first /hello. */
let first = null;
/** An update was restarting the panel when it went away. */
let updating = false;

/** GET /api/v1/hello, never from cache, giving up after `ms` (a request
 * into a panel that's going down can otherwise hang). */
export async function hello(ms = 5000) {
  const res = await fetch('/api/v1/hello', { cache: 'no-store', signal: AbortSignal.timeout(ms) });
  if (!res.ok) throw new Error(`${res.status}`);
  return res.json();
}

/** Called when the update status says the panel is restarting into a new version. */
export function expectRestart() {
  updating = true;
}

/** Compares a /hello reply with what the page loaded with. */
export function seen(h) {
  if (!h?.version) return;
  if (!first) {
    first = { version: h.version, build: h.build, boot: h.boot };
    return;
  }
  const other = h.version !== first.version || (h.build && first.build && h.build !== first.build);
  const restarted = h.boot && first.boot && h.boot !== first.boot;
  if (other || (restarted && updating)) reloadSoon(h.version);
  // Back on the version this page runs (a rollback before anyone reloaded).
  else boot.updated = null;
}

/** Asks the panel now and compares (after a reconnect). */
export async function check() {
  try {
    seen(await hello());
  } catch {
    // Still away; the next reconnect or ping looks again.
  }
}

/** True while a reload would lose something the user is doing. */
function inUse() {
  if (leave.guard?.blocked()) return true;
  if (dialog.open || document.querySelector('[aria-modal="true"]')) return true;
  const el = document.activeElement;
  if (el?.isContentEditable) return true;
  if (el instanceof HTMLTextAreaElement) return !!el.value;
  if (el instanceof HTMLInputElement) return !!el.value && !['checkbox', 'radio', 'button', 'submit', 'range'].includes(el.type);
  return false;
}

/** Reloads, or offers to; false when it only offered. */
function reloadSoon(version) {
  if (inUse()) {
    boot.updated = version;
    return false;
  }
  location.reload();
  return true;
}

// A page from before an update can't load its old lazy files any more
// (the server only has the new ones): that also means "reload".
addEventListener('vite:preloadError', (e) => {
  if (reloadSoon(first?.version || '')) e.preventDefault();
});
