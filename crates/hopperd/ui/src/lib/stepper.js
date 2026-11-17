// The number math behind a config slider, the mouse wheel and the − / +
// buttons. It mirrors crates/hopper-core/src/configedit/stepper.rs, so the
// web UI and Hopper desktop step a setting the same way: change one, change
// the other (the tests in tests/stepper.test.mjs use the same cases).

/** How many decimals `x` needs (0.05 is 2, 3 is 0), up to 9. */
function decimals(x) {
  let d = 0;
  let y = Math.abs(x);
  while (d < 9 && Math.abs(y - Math.round(y)) > 1e-9 * Math.max(y, 1)) {
    y *= 10;
    d += 1;
  }
  return d;
}

/** `x` rounded to `d` decimals: takes the float noise out (0.1 + 0.2). */
export function roundTo(x, d) {
  const m = 10 ** Math.min(Math.max(d, 0), 9);
  return Math.round(x * m) / m;
}

/** The slider's step for a float setting spanning `span`: a 1 or 5 times a power of ten, at most a hundredth of the span. */
export function sliderStep(span) {
  if (!(Number.isFinite(span) && span > 0)) return 1;
  const target = span / 100;
  const pow = 10 ** Math.floor(Math.log10(target));
  const step = target >= 5 * pow ? 5 * pow : pow;
  return roundTo(step, Math.max(-Math.floor(Math.log10(step)), 0) + 1);
}

/** One notch of the wheel (and a − / + click), before Shift multiplies it: 1 for a whole number; for a float the slider's step when the range is known, else by how big the value is. */
export function stepFor(whole, value, range) {
  if (whole) return 1;
  if (range && range[0] != null && range[1] != null && range[1] > range[0] && range[1] - range[0] <= 100000) {
    return sliderStep(range[1] - range[0]);
  }
  const f = Math.abs(Number(value) || 0);
  if (f >= 10) return 1;
  if (f >= 1 || f === 0) return 0.1;
  return 0.01;
}

function clamp(x, range) {
  if (!range) return x;
  const lo = range[0] ?? -Infinity;
  const hi = range[1] ?? Infinity;
  return Math.min(Math.max(x, lo), hi);
}

/** `value` moved by `notches` steps (positive: up), stopping at the ends of `range`. A whole number stays whole; a float keeps the decimals of its step and of itself, with no float noise. */
export function stepped(whole, value, notches, step, range) {
  const v = Number(value);
  if (whole) return Math.round(clamp(v + Math.round(notches * step), range));
  const d = Math.max(decimals(step), decimals(v));
  return clamp(roundTo(v + notches * step, d), range);
}

/** The setting's value for a slider at `x` (lo..hi): snapped to `step` from the low end, inside the range, whole for an integer setting. */
export function fromSlider(x, lo, hi, step, whole) {
  x = Math.min(Math.max(x, lo), hi);
  if (whole) return Math.round(x);
  if (!(step > 0)) step = 1;
  // The top is always reachable, even when it isn't on the step grid.
  if (hi - x < step / 2) return hi;
  const snapped = lo + Math.round((x - lo) / step) * step;
  const d = Math.max(decimals(step), decimals(lo));
  return Math.min(Math.max(roundTo(snapped, d), lo), hi);
}

/** A number typed in a field, moved by `notches` steps and given back as text. Text that isn't a number counts as the low end (or 0). */
export function stepText(text, notches, step, lo, hi, whole) {
  const t = String(text ?? '').trim();
  let start = Number(t);
  if (t === '' || !Number.isFinite(start)) start = lo ?? 0;
  if (whole) start = Math.round(start);
  return String(stepped(whole, start, notches, step, [lo ?? null, hi ?? null]));
}

/** How many steps a wheel event is: a mouse notch is one (whatever the browser calls its size), a touchpad's small pixel moves add up to one per 40. Up is positive. */
let wheelPx = 0;
export function wheelNotches(ev) {
  // Shift + wheel is sent sideways on Windows.
  const dy = ev.shiftKey && !ev.deltaY ? ev.deltaX : ev.deltaY;
  if (!dy) return 0;
  if (ev.deltaMode !== 0 || Math.abs(dy) >= 50) {
    wheelPx = 0;
    return -Math.sign(dy);
  }
  wheelPx -= dy;
  const n = Math.trunc(wheelPx / 40);
  wheelPx -= n * 40;
  return n;
}

/** A Svelte action: the wheel over a field steps its number. `opts` is { text(): what the field says, step, lo, hi, whole, apply(text) } and may change over time. Shift moves ten steps; the page doesn't scroll under a turn. */
export function wheelStep(node, opts) {
  const on = (ev) => {
    if (ev.ctrlKey || ev.metaKey || node.disabled || node.readOnly) return;
    ev.preventDefault();
    const n = wheelNotches(ev);
    if (!n) return;
    const o = opts;
    const next = stepText(o.text(), n * (ev.shiftKey ? 10 : 1), o.step, o.lo, o.hi, o.whole);
    if (next !== String(o.text()).trim()) o.apply(next);
  };
  // Not passive: the page must not scroll while the number turns.
  node.addEventListener('wheel', on, { passive: false });
  return {
    update(o) {
      opts = o;
    },
    destroy() {
      node.removeEventListener('wheel', on);
    },
  };
}

/** Every plain number field (`<input type="number">`) steps with the wheel, by its own step, min and max. Call once at start. */
export function numberFieldsWheel(doc = document) {
  doc.addEventListener(
    'wheel',
    (ev) => {
      const el = ev.target;
      if (!(el instanceof HTMLInputElement) || el.type !== 'number' || el.disabled || el.readOnly) return;
      if (ev.ctrlKey || ev.metaKey) return;
      ev.preventDefault();
      const n = wheelNotches(ev);
      if (!n) return;
      const stepAttr = Number(el.step);
      const step = Number.isFinite(stepAttr) && stepAttr > 0 ? stepAttr : 1;
      const num = (s) => (s === '' || !Number.isFinite(Number(s)) ? null : Number(s));
      const lo = num(el.min);
      const hi = num(el.max);
      const whole = Number.isInteger(step) && !el.value.includes('.');
      const next = stepText(el.value, n * (ev.shiftKey ? 10 : 1), step, lo, hi, whole);
      if (next === el.value) return;
      el.value = next;
      el.dispatchEvent(new Event('input', { bubbles: true }));
      el.dispatchEvent(new Event('change', { bubbles: true }));
    },
    { passive: false },
  );
}
