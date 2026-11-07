// The panel's colour themes: the same list as Hopper desktop's Appearance
// menu. A theme sets the
// colour variables from app.css on the page, so every screen follows. The
// choice is kept in this browser; "Follow system" tracks the OS.

import themes from './themes.json';

const KEY = 'hopper.theme';

// One list for both apps: this file is written from PALETTES in
// src/gpui/theme.rs (a test fails when they differ; UPDATE_THEMES=1 cargo test
// rewrites it), in the menu's order: Hopper Dark, the other dark ones, the
// light ones.
export const THEMES = themes;

export const FOLLOW = ''; // "Follow system"

function read() {
  try { return localStorage.getItem(KEY) ?? FOLLOW; } catch { return FOLLOW; }
}

// What is chosen and kept, and what is on screen while one is being tried.
export const theme = $state({ saved: read(), shown: null });

const mix = (a, pa, b) => `color-mix(in srgb, ${a} ${pa}%, ${b})`;

function resolve(name) {
  if (name) return THEMES.find((t) => t.name === name) ?? THEMES[0];
  const dark = !window.matchMedia?.('(prefers-color-scheme: light)').matches;
  return THEMES.find((t) => t.name === (dark ? 'Hopper Dark' : 'Hopper Light'));
}

/** Puts a theme on the page (Hopper Dark is app.css as written). */
export function paint(name) {
  const t = resolve(name);
  const st = document.documentElement.style;
  const vars = t.name === 'Hopper Dark' ? {} : {
    '--bg': t.bg, '--side': t.panel, '--base': mix(t.bg, 55, t.surface), '--surface': t.surface, '--raise': t.raised,
    '--line': t.border, '--line2': mix(t.border, 70, t.text), '--term': mix(t.bg, 75, t.panel),
    '--text': t.text, '--subtle': t.muted, '--muted': t.muted,
    '--acc': t.accent, '--acc-dim': mix(t.accent, 12, 'transparent'),
    '--green': t.success, '--green-dim': mix(t.success, 10, 'transparent'),
    '--gold': t.warning, '--gold-dim': mix(t.warning, 10, 'transparent'),
    '--blue': t.info, '--blue-dim': mix(t.info, 10, 'transparent'),
    '--red': t.danger, '--red-dim': mix(t.danger, 10, 'transparent'), '--danger': t.danger,
    '--input-line': mix(t.border, 55, t.text), '--side-line': mix(t.panel, 70, t.border),
    '--hover': mix(t.text, 6, 'transparent'), '--sel': mix(t.accent, 12, 'transparent'),
    '--popover': mix(t.bg, 55, t.surface),
  };
  const all = ['--bg', '--side', '--base', '--surface', '--raise', '--line', '--line2', '--term', '--text', '--subtle', '--muted', '--acc', '--acc-dim',
    '--green', '--green-dim', '--gold', '--gold-dim', '--blue', '--blue-dim', '--red', '--red-dim', '--danger', '--input-line', '--side-line',
    '--hover', '--sel', '--popover'];
  for (const k of all) {
    if (vars[k]) st.setProperty(k, vars[k]); else st.removeProperty(k);
  }
  st.colorScheme = t.dark ? 'dark' : 'light';
}

/** Shows `name` without keeping it; null puts the kept one back. */
export function preview(name) {
  theme.shown = name;
  paint(name ?? theme.saved);
}

/** Keeps `name`. */
export function choose(name) {
  theme.saved = name;
  theme.shown = null;
  try { localStorage.setItem(KEY, name); } catch { /* private window: it lasts until reload */ }
  paint(name);
}

// On load, before the first screen, and when the OS flips while following it.
paint(theme.saved);
window.matchMedia?.('(prefers-color-scheme: light)').addEventListener('change', () => {
  if (theme.saved === FOLLOW && theme.shown === null) paint(FOLLOW);
});
