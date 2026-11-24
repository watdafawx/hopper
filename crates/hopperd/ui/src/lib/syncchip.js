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

// "Restart to load" while the server runs: what changed since it started loads only after a
// restart. It stands alone beside the sync chips (never merged with them) and needs nothing now.
export function restartLook(changed, what = 'server') {
  return changed
    ? look('restart', 'busy', 'restart', 'Restart to load', 'Restart')
    : look('restart', 'quiet', 'behind', `Waiting for the ${what} to close`, 'Waiting');
}
// The "Needs you" chip: every notice the page has that no other chip says. One notice shows its own
// title (cut to 28 letters), more say "N need you"; red when one stops play, amber for a choice. Only
// notes make a quiet grey "N notes" that may fold. `needs` are the tiers ('stop' | 'choice' | 'info').
const ATTENTION_TITLE = 28;
export function attentionLook(needs, title = '') {
  if (!needs.length) return null;
  const n = needs.length;
  const worst = needs.includes('stop') ? 'stop' : needs.includes('choice') ? 'choice' : 'info';
  if (worst === 'info') {
    const label = n === 1 ? '1 note' : `${n} notes`;
    return look('attention', 'quiet', 'note', label, label);
  }
  const t = title.trim();
  const label = n === 1 ? ([...t].length > ATTENTION_TITLE ? `${[...t].slice(0, ATTENTION_TITLE - 1).join('').trimEnd()}…` : t) : `${n} need you`;
  return look('attention', worst === 'stop' ? 'bad' : 'warn', 'alert', label, n === 1 ? (worst === 'stop' ? 'Stops play' : 'Needs you') : 'Needs you', { fixed: true });
}
// The chip beside the run state while the server has a problem: red, never folds, its notice in the
// popover. `problem` is { kind: 'stuck', minutes } or { kind: 'crashed' }.
export function runLook(problem) {
  if (problem.kind === 'crashed') return look('run', 'bad', 'alert', 'Crashed', 'Crashed', { fixed: true });
  const m = problem.minutes;
  return look('run', 'bad', 'stuck', m >= 1 ? `Looks stuck · ${m} min` : 'Looks stuck', 'Stuck', { fixed: true });
}
// The whole minutes in a stuck report's headline ("... (no progress for 4 min, CPU idle)"), or null.
export function stuckMinutes(headline) {
  const m = /no progress for (\d+) min\b/.exec(headline || '');
  return m ? Number(m[1]) : null;
}
export const standsAlone = (l) => l.source === 'restart' || l.source === 'attention' || l.source === 'run';
const aloneForm = (level) => (level === 0 ? 'full' : level === 1 ? 'short' : 'icon');
export { aloneForm };

// Both chips as one "Sync" chip: the worse tone wins, and it can't fold if either can't.
export function merged(a, b) {
  const w = TONE_RANK[b.tone] > TONE_RANK[a.tone] ? b : a;
  return { ...w, label: 'Sync', short: 'Sync', fixed: a.fixed || b.fixed, strip: false };
}

// Highest fold level: forms (full, short, icon; one more for the merged pair), then the chips
// fold into "+N" unless one can't, then the plain pills.
// `pinned`: a stand-alone chip that can't fold ("Needs you") keeps every chip.
export function maxLevel(fixed, plain, pinned = false) {
  if (!fixed.length) return plain;
  const two = fixed.length >= 2;
  const iconAt = two ? 3 : 2;
  const foldable = pinned || (two ? fixed.some(Boolean) : fixed[0]) ? 0 : 1;
  return iconAt + foldable + plain;
}

// What each chip shows at a fold level. forms: 'full' | 'short' | 'icon' | null (folded).
export function plan(fixed, plain, level, pinned = false) {
  const n = fixed.length;
  level = Math.min(level, maxLevel(fixed, plain, pinned));
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
    const stuck = pinned || (two ? fixed.some(Boolean) : fixed[0]);
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
export function levelFor(all, room) {
  const chips = all.filter((c) => !standsAlone(c));
  const alone = all.filter(standsAlone);
  const fixed = chips.map((c) => c.fixed);
  const pinned = alone.some((c) => c.fixed);
  const top = Math.max(maxLevel(fixed, 0, pinned), alone.length ? 2 : 0);
  for (let lv = 0; lv <= top; lv++) {
    const p = plan(fixed, 0, lv, pinned);
    const parts = p.merged ? [width(merged(chips[0], chips[1]), p.merged)] : p.forms.filter(Boolean).map((f, i) => width(chips[i], f));
    // The stand-alone chips follow the sync chips, and fold with them.
    if (!p.chipsFolded) parts.push(...alone.map((c) => width(c, aloneForm(lv))));
    const total = parts.reduce((a, b) => a + b, 0) + GAP * Math.max(0, parts.length - 1);
    if (!parts.length || total <= room) return lv;
  }
  return top;
}
