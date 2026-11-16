// Readable small red text, whatever the theme. A theme's `danger` is picked
// to look right as a fill or an icon; as small text it is 2.1:1 (Nord) to
// 6.3:1 (Gruvbox Light) on the page. `--red-text` is that red pulled toward
// the text colour (dark themes) or toward black (light themes) just far
// enough to read on every surface text sits on and on its own red tint.
// Pure, so scripts/red-contrast.mjs and tests/ink.test.mjs use the same
// code as the page (lib/theme.svelte.js paint()).

/** WCAG needs 4.5:1 for small text; thin small text anti-aliases lighter
 * than its colour, so the fit aims a little higher (as desktop's INK_RATIO). */
export const MIN = 4.5;
export const AIM = 5;

export const rgb = (h) => { const n = parseInt(h.slice(1, 7), 16); return [(n >> 16) & 255, (n >> 8) & 255, n & 255]; };
export const toHex = (c) => `#${c.map((v) => Math.round(Math.min(255, Math.max(0, v))).toString(16).padStart(2, '0')).join('')}`;
/** color-mix(in srgb, a pa%, b) */
export const mix = (a, pa, b) => a.map((v, i) => v * (pa / 100) + b[i] * (1 - pa / 100));
const lin = (v) => { v /= 255; return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; };
const lum = ([r, g, b]) => 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
/** WCAG contrast ratio of two [r,g,b] colours. */
export const ratio = (a, b) => { const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05); };

/** The surfaces text sits on, as paint() sets them from a theme: page,
 * sidebar, base (cards' backdrop and popovers), card, raised. Hopper Dark is
 * app.css as written. */
export function surfaces(t) {
  if (t.name === 'Hopper Dark') return { page: rgb('#121214'), side: rgb('#0e0e10'), base: rgb('#17171a'), card: rgb('#1f1f23'), raise: rgb('#28282d') };
  const bg = rgb(t.bg);
  return { page: bg, side: rgb(t.panel), base: mix(bg, 55, rgb(t.surface)), card: rgb(t.surface), raise: rgb(t.raised) };
}

/** The worst ratio of `fg` over `surf` and over `tint` of `color` on each. */
export function worst(fg, surf, color, tint = 10) {
  const bgs = Object.values(surf);
  return Math.min(...bgs.map((b) => ratio(fg, b)), ...bgs.map((b) => ratio(fg, mix(color, tint, b))));
}

const toHsl = ([r, g, b]) => {
  r /= 255; g /= 255; b /= 255;
  const mx = Math.max(r, g, b), mn = Math.min(r, g, b), l = (mx + mn) / 2, d = mx - mn;
  if (!d) return [0, 0, l];
  const s = d / (1 - Math.abs(2 * l - 1));
  const h = mx === r ? ((g - b) / d + (g < b ? 6 : 0)) : mx === g ? (b - r) / d + 2 : (r - g) / d + 4;
  return [h * 60, s, l];
};
const fromHsl = ([h, s, l]) => {
  const c = (1 - Math.abs(2 * l - 1)) * s, x = c * (1 - Math.abs(((h / 60) % 2) - 1)), m = l - c / 2;
  const [r, g, b] = h < 60 ? [c, x, 0] : h < 120 ? [x, c, 0] : h < 180 ? [0, c, x] : h < 240 ? [0, x, c] : h < 300 ? [x, 0, c] : [c, 0, x];
  return [(r + m) * 255, (g + m) * 255, (b + m) * 255];
};

/** The theme's red as small text: same hue and saturation, lightness moved
 * (up on a dark theme, down on a light one) the least that reaches `aim` on
 * every surface and on its own red tint. Keeping the hue keeps it red:
 * blending toward the text colour turns Nord's red grey-pink. */
export function inkRed(t, aim = AIM) {
  const red = rgb(t.danger), surf = surfaces(t);
  const [h, s, l0] = toHsl(red);
  const step = t.dark ? 0.005 : -0.005;
  let out = red;
  for (let l = l0; l >= 0 && l <= 1; l += step) {
    out = fromHsl([h, s, l]);
    if (worst(out, surf, red) >= aim) break;
  }
  return out;
}
