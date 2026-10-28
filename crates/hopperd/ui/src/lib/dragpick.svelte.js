// Press a row control and drag across rows: every row the pointer crosses
// gets the same, as in Hopper desktop (drag_pick.rs).
//
// - Tick boxes ('tick'): rows take the state the first box got (ticked,
//   or unticked when it started ticked), as the pointer crosses them.
// - On/off switches ('switch'): the crossed switches show the new state
//   while the button is held; letting go switches them all in one go.
// - Row buttons such as Update ('action'): the crossed buttons light up;
//   letting go runs them all.
//
// Held near the scrolling area's top or bottom edge, it scrolls by itself
// and keeps picking up rows (works with virtual lists: rows are found by
// their data-pick-ix under the pointer).
//
// A list: the rows carry `data-pick-ix={index in the shown list}` inside an
// element with `data-pick-list={name}`. A control's onpointerdown calls
// `startPick(e, {...})`; its click handler (and the row's) asks
// `clickSwallowed()` first, so letting go over it doesn't run it twice.
//
// Also "painting" a choice down a plain list (Keep mine / Pack's on each
// file): `paintDown(group, value, set)` on an option's pointerdown,
// `paintOver(group, e, set)` on the row's pointerenter / pointermove.

/** The running sweep, for rows to show what letting go will do. */
export const pick = $state({ list: '', kind: '', start: -1, cur: -1, on: true, spread: false });

let opts = null;
let base = null;
let y = 0;
let x = 0;
let frame = 0;
let chip = null;
let swallow = false;

/** Rows `start..=cur` of the running sweep in `list` (none until it spread). */
export function swept(list, ix) {
  if (!pick.spread || pick.list !== list) return false;
  return ix >= Math.min(pick.start, pick.cur) && ix <= Math.max(pick.start, pick.cur);
}

/** The running sweep's kind in `list` once it crossed rows, else ''. */
export function sweeping(list) {
  return pick.spread && pick.list === list ? pick.kind : '';
}

/**
 * Starts a drag from a row control.
 * - list: the data-pick-list name; ix: the row's index.
 * - kind: 'tick' | 'switch' | 'action'; on: the state rows get.
 * - keyAt(ix): the row's key (null: not pickable, e.g. no update).
 * - selected (tick): the current picks, a Set of keys.
 * - onTick(set) (tick): the new picks as the drag moves.
 * - onCommit(keys) (switch, action): let go after crossing rows.
 * - label(n): the chip's text for n rows ("Ticking 5").
 */
export function startPick(e, o) {
  if (e.button !== 0) return;
  opts = o;
  base = o.kind === 'tick' ? new Set(o.selected || []) : null;
  Object.assign(pick, { list: o.list, kind: o.kind, start: o.ix, cur: o.ix, on: o.on, spread: false });
  x = e.clientX;
  y = e.clientY;
  window.addEventListener('pointermove', move);
  window.addEventListener('pointerup', up, true);
  window.addEventListener('keydown', esc, true);
}

/** For a swept control's (or its row's) click: true when a drag that
 * crossed rows just ended, so the sweep did the work. */
export function clickSwallowed() {
  return swallow;
}

function listEl() {
  return document.querySelector(`[data-pick-list="${CSS.escape(pick.list)}"]`);
}

/** The scrolling element around the list (the page when none). */
function scroller(el) {
  for (let p = el?.parentElement; p; p = p.parentElement) {
    const s = getComputedStyle(p).overflowY;
    if ((s === 'auto' || s === 'scroll') && p.scrollHeight > p.clientHeight) return p;
  }
  return document.scrollingElement;
}

/** The row under the pointer's height, clamped to the list and its view. */
function rowAt() {
  const el = listEl();
  if (!el) return pick.cur;
  const sc = scroller(el);
  const view = sc === document.scrollingElement ? { top: 0, bottom: innerHeight } : sc.getBoundingClientRect();
  const box = el.getBoundingClientRect();
  const top = Math.max(view.top, box.top);
  const bottom = Math.min(view.bottom, box.bottom);
  const yy = Math.min(Math.max(y, top + 1), bottom - 1);
  const rows = el.querySelectorAll('[data-pick-ix]');
  let best = null;
  for (const r of rows) {
    const b = r.getBoundingClientRect();
    if (yy >= b.top && yy < b.bottom) return Number(r.dataset.pickIx);
    // Between rows (a gap): the nearest.
    const d = Math.min(Math.abs(yy - b.top), Math.abs(yy - b.bottom));
    if (!best || d < best.d) best = { d, ix: Number(r.dataset.pickIx) };
  }
  return best ? best.ix : pick.cur;
}

function track() {
  const ix = rowAt();
  if (ix === pick.cur) return;
  pick.cur = ix;
  if (ix !== pick.start && !pick.spread) {
    pick.spread = true;
    document.body.classList.add('picking');
  }
  changed();
}

function keys() {
  const out = [];
  const [a, b] = [Math.min(pick.start, pick.cur), Math.max(pick.start, pick.cur)];
  for (let i = a; i <= b; i++) {
    const k = opts.keyAt(i);
    if (k != null) out.push(k);
  }
  return out;
}

function changed() {
  if (!pick.spread) return;
  const ks = keys();
  if (pick.kind === 'tick') {
    const s = new Set(base);
    for (const k of ks) pick.on ? s.add(k) : s.delete(k);
    opts.onTick?.(s);
  }
  showChip(opts.label ? opts.label(ks.length) : '');
}

function showChip(text) {
  if (!text) {
    chip?.remove();
    chip = null;
    return;
  }
  if (!chip) {
    chip = document.createElement('div');
    chip.className = 'pick-chip';
    document.body.appendChild(chip);
  }
  chip.textContent = text;
  chip.style.left = `${x + 16}px`;
  chip.style.top = `${y + 12}px`;
}

function move(e) {
  x = e.clientX;
  y = e.clientY;
  if (!(e.buttons & 1)) return up();
  track();
  if (chip) showChip(chip.textContent);
  if (pick.spread && !frame) frame = requestAnimationFrame(edge);
}

/** One frame of the edge scroll while the drag runs. */
function edge() {
  frame = 0;
  if (!opts) return;
  const sc = scroller(listEl());
  const view = sc === document.scrollingElement ? { top: 0, bottom: innerHeight } : sc.getBoundingClientRect();
  const zone = 40;
  let step = 0;
  if (y < view.top + zone) step = -Math.min((view.top + zone - y) / 3, 24);
  else if (y > view.bottom - zone) step = Math.min((y - (view.bottom - zone)) / 3, 24);
  if (step) {
    const before = sc.scrollTop;
    sc.scrollTop += step;
    if (sc.scrollTop !== before) {
      // The virtual list draws the new rows on the next frame.
      requestAnimationFrame(() => opts && track());
    }
    frame = requestAnimationFrame(edge);
  }
}

function esc(e) {
  if (e.key !== 'Escape') return;
  e.stopPropagation();
  if (pick.kind === 'tick' && pick.spread) opts.onTick?.(new Set(base));
  finish(false);
}

function up() {
  finish(true);
}

function finish(commit) {
  const o = opts;
  const spread = pick.spread;
  const ks = o && spread ? keys() : [];
  window.removeEventListener('pointermove', move);
  window.removeEventListener('pointerup', up, true);
  window.removeEventListener('keydown', esc, true);
  if (frame) cancelAnimationFrame(frame);
  frame = 0;
  showChip('');
  document.body.classList.remove('picking');
  opts = null;
  base = null;
  Object.assign(pick, { list: '', kind: '', start: -1, cur: -1, spread: false });
  if (!spread) return;
  // The click that follows this pointerup (on the control or the row) is
  // the sweep's, not a click of its own.
  swallow = true;
  setTimeout(() => (swallow = false), 0);
  if (commit && o.kind !== 'tick' && ks.length) o.onCommit?.(ks);
}

/** "Ticking 5" / "Unticking 5". */
export const tickLabel = (on) => (n) => (n ? `${on ? 'Ticking' : 'Unticking'} ${n}` : '');

/** "Updating 5", or `none` when no crossed row has one. */
export const countLabel = (verb, none) => (n) => (n ? `${verb} ${n}` : none);

// ---- painting a choice down a plain list ----------------------------------------

let paint = null;

function paintEnd() {
  paint = null;
  window.removeEventListener('pointerup', paintEnd, true);
}

/** Pressed option `value` on row `ix` of list `group`: this row gets it now,
 * and rows the pointer moves over with the button held get it too. */
export function paintDown(e, group, ix, value, setAt) {
  if (e.button !== 0) return;
  paint = { group, value, last: ix };
  window.addEventListener('pointerup', paintEnd, true);
  setAt(ix, value);
}

/** The pointer moved over row `ix` of `group` (its pointerenter): it and
 * every row between it and the last one painted (a fast move skips rows)
 * take the paint. */
export function paintOver(e, group, ix, setAt) {
  if (!paint || paint.group !== group) return;
  if (!(e.buttons & 1)) return paintEnd();
  const [a, b] = [Math.min(paint.last, ix), Math.max(paint.last, ix)];
  for (let i = a; i <= b; i++) setAt(i, paint.value);
  paint.last = ix;
}

/** A painted tick box's click: the press already set it (a pointer click
 * only puts the box back in step); a keyboard click (Space) toggles. */
export function paintClick(e, toggle) {
  if (e.detail === 0) toggle();
  else e.preventDefault();
}
