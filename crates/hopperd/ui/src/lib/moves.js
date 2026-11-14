// Move or copy to… (design/multi-panel.md, A5): how a move reads in the
// dialog and on the server page. Kept apart from the component so the
// wording is tested (tests/moves.test.mjs).
import { bytes } from './format.js';

/** Still going (the dialog keeps polling). */
export const active = (m) => ['preparing', 'sending', 'waiting', 'finishing'].includes(m?.state);

/** Stopped but can go on from where it got to. */
export const resumable = (m) => m?.state === 'paused';

/** The bar's width, 0 to 100. Finishing and done are full. */
export function percent(m) {
  if (!m) return 0;
  if (m.state === 'done' || m.state === 'finishing') return 100;
  if (!m.total) return 0;
  return Math.max(0, Math.min(100, Math.round((m.done / m.total) * 100)));
}

/** The line under the bar: how far, in bytes. */
export function amount(m) {
  if (!m || !m.total || m.state === 'done' || m.state === 'finishing') return '';
  return `${bytes(m.done)} of ${bytes(m.total)}`;
}

/** The dialog's title while and after it runs. */
export function title(m) {
  const copy = m.mode === 'copy';
  if (m.state === 'done') return `${copy ? 'Copied' : 'Moved'} ${m.name} to ${m.to_name}`;
  if (m.state === 'failed') return `Couldn't ${copy ? 'copy' : 'move'} ${m.name}`;
  if (m.state === 'cancelled') return `${copy ? 'Copy' : 'Move'} cancelled`;
  return `${copy ? 'Copying' : 'Moving'} ${m.name} to ${m.to_name}`;
}

/** A small line once the link broke and it went on. */
export function resumedLine(m) {
  if (!m?.resumed) return '';
  const times = m.resumed === 1 ? 'once' : `${m.resumed} times`;
  return `Went on ${times} after the link broke; only what was missing was sent.`;
}

/** The chosen place's button: "Move to vm-a" / "Copy to this panel". */
export const goLabel = (mode, place) => `${mode === 'copy' ? 'Copy' : 'Move'} to ${place?.name ?? '…'}`;

/** The banner on a server that was moved away. */
export function movedLine(mark, when = (t) => new Date(t * 1000).toLocaleDateString()) {
  if (!mark) return '';
  return `Moved to ${mark.to}${mark.at ? ` on ${when(mark.at)}` : ''}. This copy is stopped and kept.`;
}

/** Why Start is off for this server, or '' (the panel refuses it too, in
 *  the same words): its files are being sent, or it was moved away. */
export function startBlocked(s) {
  if (s?.sending) return 'Its files are being sent to another panel. Start it once that\'s done.';
  if (s?.moved) return `This copy was moved to ${s.moved.to || 'another panel'}. Choose Keep using this copy to start it here.`;
  return '';
}
