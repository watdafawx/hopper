// The panel's colour themes: the same list as Hopper desktop's Appearance
// menu (PALETTES in src/gpui/theme.rs; keep the two in step). A theme sets the
// colour variables from app.css on the page, so every screen follows. The
// choice is kept in this browser; "Follow system" tracks the OS.

const KEY = 'hopper.theme';

// name, dark?, bg, panel, surface, raised, border, text, muted, accent,
// text on accent, success, info, warning, danger
const RAW = [
  ['Hopper Dark', 1, '#121214', '#0e0e10', '#1f1f23', '#28282d', '#26262b', '#ededef', '#9a9aa3', '#f97316', '#1c0d02', '#22c55e', '#38bdf8', '#facc15', '#ef4444'],
  ['Catppuccin Mocha', 1, '#1e1e2e', '#181825', '#262637', '#313244', '#313244', '#cdd6f4', '#a6adc8', '#cba6f7', '#1e1e2e', '#a6e3a1', '#89dceb', '#f9e2af', '#f38ba8'],
  ['Catppuccin Macchiato', 1, '#24273a', '#1e2030', '#2b2e43', '#363a4f', '#363a4f', '#cad3f5', '#a5adcb', '#c6a0f6', '#181926', '#a6da95', '#91d7e3', '#eed49f', '#ed8796'],
  ['Rosé Pine', 1, '#191724', '#16141f', '#1f1d2e', '#26233a', '#26233a', '#e0def4', '#908caa', '#ebbcba', '#191724', '#9ccfd8', '#c4a7e7', '#f6c177', '#eb6f92'],
  ['Rosé Pine Moon', 1, '#232136', '#1d1b2e', '#2a273f', '#393552', '#393552', '#e0def4', '#908caa', '#ea9a97', '#232136', '#9ccfd8', '#c4a7e7', '#f6c177', '#eb6f92'],
  ['Tokyo Night', 1, '#1a1b26', '#16161e', '#1f2335', '#292e42', '#292e42', '#c0caf5', '#9aa5ce', '#7aa2f7', '#1a1b26', '#9ece6a', '#7dcfff', '#e0af68', '#f7768e'],
  ['Dracula', 1, '#282a36', '#21222c', '#303241', '#44475a', '#44475a', '#f8f8f2', '#a3a7c2', '#bd93f9', '#282a36', '#50fa7b', '#8be9fd', '#f1fa8c', '#ff5555'],
  ['Nord', 1, '#2e3440', '#272c36', '#353c4a', '#434c5e', '#3b4252', '#eceff4', '#a3acbf', '#88c0d0', '#2e3440', '#a3be8c', '#81a1c1', '#ebcb8b', '#bf616a'],
  ['Gruvbox Dark', 1, '#282828', '#1d2021', '#32302f', '#3c3836', '#3c3836', '#ebdbb2', '#a89984', '#fabd2f', '#282828', '#b8bb26', '#83a598', '#fe8019', '#fb4934'],
  ['Everforest', 1, '#2d353b', '#272e33', '#343f44', '#3d484d', '#3d484d', '#d3c6aa', '#9da9a0', '#a7c080', '#2d353b', '#a7c080', '#7fbbb3', '#dbbc7f', '#e67e80'],
  ['One Dark', 1, '#282c34', '#21252b', '#2c313a', '#3a3f4b', '#3a3f4b', '#abb2bf', '#8b92a0', '#61afef', '#1e2127', '#98c379', '#56b6c2', '#e5c07b', '#e06c75'],
  ['Ristretto', 1, '#2c2525', '#211c1c', '#352e2e', '#403838', '#403838', '#fff1f3', '#c3b7b8', '#f9cc6c', '#2c2525', '#adda78', '#85dacc', '#f38d70', '#fd6883'],
  ['Kanagawa', 1, '#1f1f28', '#16161d', '#2a2a37', '#363646', '#363646', '#dcd7ba', '#a09c8c', '#7e9cd8', '#1f1f28', '#98bb6c', '#7fb4ca', '#e6c384', '#e46876'],
  ['Hopper Light', 0, '#fafafa', '#f1f1f3', '#f4f4f5', '#e7e7ea', '#e4e4e7', '#18181b', '#6b6b75', '#ea580c', '#ffffff', '#16a34a', '#0284c7', '#ca8a04', '#dc2626'],
  ['Catppuccin Latte', 0, '#eff1f5', '#e6e9ef', '#e6e9ef', '#ccd0da', '#ccd0da', '#4c4f69', '#6c6f85', '#8839ef', '#eff1f5', '#40a02b', '#04a5e5', '#df8e1d', '#d20f39'],
  ['Rosé Pine Dawn', 0, '#faf4ed', '#f2e9e1', '#fffaf3', '#dfdad9', '#dfdad9', '#575279', '#797593', '#d7827e', '#faf4ed', '#56949f', '#907aa9', '#ea9d34', '#b4637a'],
  ['Tokyo Night Day', 0, '#e1e2e7', '#d0d5e3', '#e9e9ed', '#c4c8da', '#c4c8da', '#3760bf', '#6172b0', '#2e7de9', '#ffffff', '#587539', '#007197', '#8c6c3e', '#f52a65'],
  ['Gruvbox Light', 0, '#fbf1c7', '#f2e5bc', '#f9f5d7', '#ebdbb2', '#d5c4a1', '#3c3836', '#7c6f64', '#af3a03', '#fbf1c7', '#79740e', '#076678', '#b57614', '#9d0006'],
  ['Solarized Light', 0, '#fdf6e3', '#eee8d5', '#f5efdc', '#e4ddc8', '#e0d8c0', '#475b62', '#657b83', '#268bd2', '#fdf6e3', '#859900', '#2aa198', '#b58900', '#dc322f'],
];

export const THEMES = RAW.map(([name, dark, bg, panel, surface, raised, border, text, muted, accent, onAccent, success, info, warning, danger]) => (
  { name, dark: !!dark, bg, panel, surface, raised, border, text, muted, accent, onAccent, success, info, warning, danger }
));

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
