// The sync chip's words and fold order, a port of hopper-core's syncchip.rs
// so the server panel and Hopper desktop say the same thing. Keep the two in step.

export const TONE_RANK = { good: 0, quiet: 1, busy: 2, warn: 3, bad: 4 };

const look = (source, tone, glyph, label, short, extra = {}) => ({ source, tone, glyph, label, short, busy: false, fixed: false, strip: false, ...extra });

// One server pairing (rigsync) as a chip. `other` is the other side's name ('' if unknown).
export function serverLook(state, other = '') {
  const o = (other || '').trim();
  switch (state.kind) {
    case 'in-sync':
      return look('server', 'good', 'check', o ? `In sync with ${o}` : 'In sync', 'In sync');
    case 'differ': {
      const n = state.mods || 0;
      return look('server', 'warn', 'differ', n === 1 ? '1 mod differs' : `${n} mods differ`, n === 1 ? '1 differs' : `${n} differ`);
    }
    case 'comparing':
      return look('server', 'busy', 'spin', o ? `Comparing with ${o}` : 'Checking…', 'Checking…', { busy: true });
    case 'working': {
      const [word, long] = { pushing: ['Pushing…', 'Pushing to'], pulling: ['Pulling…', 'Pulling from'], removing: ['Removing…', 'Removing from'] }[state.work] || ['Working…', 'Working with'];
      return look('server', 'busy', 'spin', o ? `${long} ${o}` : word, word, { busy: true });
    }
    case 'offline':
      return look('server', 'quiet', 'offline', o ? `${o} offline` : 'Server offline', 'Offline');
    default:
      return look('server', 'bad', 'alert', 'Sync failed', 'Failed', { fixed: true });
  }
}

// Both chips as one "Sync" chip: the worse tone wins, and it can't fold if either can't.
export function merged(a, b) {
  const w = TONE_RANK[b.tone] > TONE_RANK[a.tone] ? b : a;
  return { ...w, label: 'Sync', short: 'Sync', fixed: a.fixed || b.fixed, strip: false };
}

// Highest fold level: forms (full, short, icon; one more for the merged pair), then the chips
// fold into "+N" unless one can't, then the plain pills.
export function maxLevel(fixed, plain) {
  if (!fixed.length) return plain;
  const two = fixed.length >= 2;
  const iconAt = two ? 3 : 2;
  const foldable = two ? (fixed.some(Boolean) ? 0 : 1) : fixed[0] ? 0 : 1;
  return iconAt + foldable + plain;
}

// What each chip shows at a fold level. forms: 'full' | 'short' | 'icon' | null (folded).
export function plan(fixed, plain, level) {
  const n = fixed.length;
  level = Math.min(level, maxLevel(fixed, plain));
  const two = n >= 2;
  const iconAt = two ? 3 : 2;
  const p = { forms: Array(n).fill('full'), merged: null, plainFolded: 0, chipsFolded: 0 };
  if (!n) return { ...p, plainFolded: Math.min(level, plain) };
  const form = level === 0 ? 'full' : level === 1 ? 'short' : 'icon';
  if (two) {
    if (level <= 1) p.forms = Array(n).fill(form);
    else {
      p.forms = Array(n).fill(null);
      p.merged = level === 2 ? 'short' : 'icon';
    }
  } else p.forms = [form];
  if (level > iconAt) {
    let steps = level - iconAt;
    const stuck = two ? fixed.some(Boolean) : fixed[0];
    if (steps > 0 && !stuck) {
      steps -= 1;
      if (two) p.merged = null;
      else p.forms[0] = null;
      p.chipsFolded = n;
    }
    p.plainFolded = Math.min(steps, plain);
  }
  return p;
}

// The lowest fold level whose estimated width fits `room` px (chips only; the web hero's other
// pills wrap on their own). Text is ~6.4px a letter at the chip's size, a chip adds icon and padding.
const CHIP_PAD = 16 + 14 + 5;
const GAP = 6;
const width = (c, form) => (form === 'icon' ? 16 + 14 : CHIP_PAD + Math.ceil((form === 'short' ? c.short : c.label).length * 6.4));
export function levelFor(chips, room) {
  const fixed = chips.map((c) => c.fixed);
  const top = maxLevel(fixed, 0);
  for (let lv = 0; lv <= top; lv++) {
    const p = plan(fixed, 0, lv);
    const parts = p.merged ? [width(merged(chips[0], chips[1]), p.merged)] : p.forms.filter(Boolean).map((f, i) => width(chips[i], f));
    const total = parts.reduce((a, b) => a + b, 0) + GAP * Math.max(0, parts.length - 1);
    if (!parts.length || total <= room) return lv;
  }
  return top;
}
