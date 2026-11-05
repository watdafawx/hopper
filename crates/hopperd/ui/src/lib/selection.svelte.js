// Picking several rows of a list the way every file explorer does, shared
// by every list that holds things (crash reports, logs, mods, backups,
// files…), as Hopper desktop's selection.rs:
//
// - a click picks that row alone;
// - Ctrl+click (Cmd on a Mac) adds or takes away one row;
// - Shift+click picks the rows from the last plain or Ctrl click (the
//   anchor) to here; Ctrl+Shift+click adds them to what was picked;
// - Ctrl+A picks every row the list shows (after its filter);
// - Esc lets go of a group (the row the keys are on stays picked);
// - Up / Down move the pick, Shift+Up / Down stretch it;
// - Delete asks the list to delete what is picked.
//
// A list keeps a `new Selection()`, hands its rows' keys in display order
// to `click` / `onKey`, draws picked rows with `row-sel` and, with more than
// one picked, shows lib/SelectionBar.svelte. Rows carry `data-sel-key` so
// the arrow keys can move the focus with the pick.
//
// A list with tick boxes already keeps its picks in a Set (drag-picking in
// dragpick.svelte.js writes it): pass `{ get, set }` and the selection
// reads and writes that same Set.

/** True for the modifier that adds to a pick: Ctrl, or Cmd on a Mac. */
export const addKey = (e) => !!(e && (e.ctrlKey || e.metaKey));

/** The keys from `a` to `b` in `order`, either way round. */
function range(order, a, b) {
  const ia = order.indexOf(a);
  const ib = order.indexOf(b);
  if (ia < 0 && ib < 0) return [];
  const lo = ia < 0 ? ib : ib < 0 ? ia : Math.min(ia, ib);
  const hi = ia < 0 ? ib : ib < 0 ? ia : Math.max(ia, ib);
  return order.slice(lo, hi + 1);
}

/** Moves the focus to the row with `data-sel-key={key}` inside `el`. */
export function focusKey(el, key) {
  const row = el?.querySelector(`[data-sel-key="${CSS.escape(String(key))}"]`);
  if (!row) return;
  row.focus({ preventScroll: true });
  row.scrollIntoView({ block: 'nearest' });
}

export class Selection {
  #own = $state(new Set());
  #store = null;
  /** Where a Shift range starts. */
  anchor = null;
  /** The row the arrow keys move from (the last one clicked or moved to). */
  cursor = $state(null);

  /** store: { get: () => Set, set: (Set) => void } to share a list's Set. */
  constructor(store = null) {
    this.#store = store;
  }

  /** The picked keys (a Set; replaced, never changed in place). */
  get keys() {
    return this.#store ? this.#store.get() : this.#own;
  }
  set keys(s) {
    if (this.#store) this.#store.set(s);
    else this.#own = s;
  }

  get size() {
    return this.keys.size;
  }
  /** More than one row picked: the bulk bar shows. */
  get many() {
    return this.keys.size > 1;
  }
  has(key) {
    return this.keys.has(key);
  }
  /** The picked keys in the order the list shows them. */
  inOrder(order) {
    return order.filter((k) => this.keys.has(k));
  }

  /** Picks `key` alone (a plain click, or the list opening on a row). */
  setOne(key) {
    this.keys = new Set([key]);
    this.anchor = key;
    this.cursor = key;
  }
  /** Puts the anchor and the keys on `key` without changing the picks
   * (a list that picked rows its own way: a drag, a right-click). */
  mark(key) {
    this.anchor = key;
    this.cursor = key;
  }
  clear() {
    if (this.keys.size) this.keys = new Set();
  }
  /** Picks every row the list shows. */
  selectAll(order) {
    this.keys = new Set(order);
  }
  /** Lets go of rows the list no longer has (deleted, filtered out). */
  retain(order) {
    const have = new Set(order);
    if ([...this.keys].some((k) => !have.has(k))) this.keys = new Set([...this.keys].filter((k) => have.has(k)));
    if (this.anchor != null && !have.has(this.anchor)) this.anchor = null;
    if (this.cursor != null && !have.has(this.cursor)) this.cursor = null;
  }

  /** A click on row `key` (its event `e` says which keys were held). True
   * for a plain click: the list then opens or shows that row, as before. */
  click(key, e, order) {
    const add = addKey(e);
    if (!add && !e?.shiftKey) {
      this.setOne(key);
      return true;
    }
    if (!e.shiftKey) {
      const s = new Set(this.keys);
      s.has(key) ? s.delete(key) : s.add(key);
      this.keys = s;
      this.anchor = key;
    } else {
      const from = this.anchor ?? key;
      const s = add ? new Set(this.keys) : new Set();
      for (const k of range(order, from, key)) s.add(k);
      this.keys = s;
      this.anchor = from;
    }
    this.cursor = key;
    return false;
  }

  /** A click on a row's tick box: it flips that row, as Ctrl+click does
   * (a tick never lets go of the others); Shift ticks the rows from the
   * anchor to here, keeping what was ticked. */
  tick(key, e, order) {
    this.click(key, { ctrlKey: true, shiftKey: !!e?.shiftKey && this.anchor != null }, order);
  }

  /** Up (-1) or Down (+1) from the cursor; with `extend` (Shift) the picks
   * stretch from the anchor instead of moving. The new row's key. */
  step(dir, extend, order) {
    if (!order.length) return null;
    const at = this.cursor == null ? -1 : order.indexOf(this.cursor);
    const next = at >= 0 ? Math.max(0, Math.min(order.length - 1, at + dir)) : dir < 0 ? order.length - 1 : 0;
    const key = order[next];
    if (extend) {
      const from = this.anchor ?? order[at >= 0 ? at : next];
      this.keys = new Set(range(order, from, key));
      this.anchor = from;
      this.cursor = key;
    } else this.setOne(key);
    return key;
  }

  /**
   * The selection keys of a list's keydown: Ctrl+A, Esc, Up/Down (with
   * Shift), Delete. Returns what it did ('' when the key isn't one of them,
   * 'changed', 'moved', 'delete') and stops the key going further when it
   * used it.
   * - onDelete(keys): Delete with something picked (keys in list order).
   * - onMove(key): the arrows moved the pick to `key` (show it).
   * - el: the list element, to move the focus to the row (`data-sel-key`).
   * - arrows: false for a list that moves with the arrows itself.
   */
  onKey(e, order, { onDelete, onMove, el, arrows = true } = {}) {
    const t = e.target;
    // Typing keeps its keys; a tick box doesn't type.
    if (t?.closest?.('input:not([type="checkbox"]):not([type="radio"]), textarea, select, [contenteditable]')) return '';
    const add = addKey(e);
    let act = '';
    if (add && !e.shiftKey && !e.altKey && e.key.toLowerCase() === 'a') {
      this.selectAll(order);
      act = 'changed';
    } else if (e.key === 'Escape' && this.many) {
      // One row picked is just the one shown; Esc lets go of a group.
      const keep = this.cursor;
      this.keys = keep != null && this.keys.has(keep) ? new Set([keep]) : new Set();
      act = 'changed';
    } else if (arrows && (e.key === 'ArrowUp' || e.key === 'ArrowDown') && !add && !e.altKey) {
      const k = this.step(e.key === 'ArrowUp' ? -1 : 1, e.shiftKey, order);
      if (k != null) {
        act = 'moved';
        if (el) focusKey(el, k);
        onMove?.(k);
      }
    } else if (e.key === 'Delete' && this.keys.size && onDelete) {
      act = 'delete';
      onDelete(this.inOrder(order));
    }
    if (act) {
      e.preventDefault();
      e.stopPropagation();
    }
    return act;
  }
}
