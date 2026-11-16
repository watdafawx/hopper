// Red small text contrast for every theme (WCAG 2.x ratio). Uses lib/ink.js,
// the code the page runs, so the table is what a player gets.
//
//   node scripts/red-contrast.mjs            prints the table (markdown)
//   node scripts/red-contrast.mjs --check    exits 1 when any ratio is < 4.5
import { readFileSync } from 'node:fs';
import { MIN, rgb, toHex, ratio, surfaces, worst, inkRed } from '../src/lib/ink.js';

const themes = JSON.parse(readFileSync(new URL('../src/lib/themes.json', import.meta.url), 'utf8'));
const f = (n) => n.toFixed(2);
const bare = (fg, surf) => Math.min(...Object.values(surf).map((b) => ratio(fg, b)));

export function rows() {
  return themes.map((t) => {
    const surf = surfaces(t), red = rgb(t.danger), ink = inkRed(t);
    return {
      name: t.name, dark: t.dark, danger: t.danger, ink: toHex(ink),
      plainPage: ratio(red, surf.page), plainCard: ratio(red, surf.card), plainWorst: bare(red, surf),
      inkPage: ratio(ink, surf.page), inkCard: ratio(ink, surf.card), inkWorst: bare(ink, surf), inkTint: worst(ink, surf, red),
    };
  });
}

if (process.argv[1].endsWith('red-contrast.mjs')) {
  const all = rows();
  console.log('| Theme | Theme red | Plain on page | Plain on card | Plain worst | --red-text | On page | On card | Worst (5 surfaces + red tint) |');
  console.log('|---|---|---|---|---|---|---|---|---|');
  for (const r of all) {
    console.log(`| ${r.name} | ${r.danger} | ${f(r.plainPage)} | ${f(r.plainCard)} | ${f(r.plainWorst)} | ${r.ink} | ${f(r.inkPage)} | ${f(r.inkCard)} | ${f(r.inkTint)} |`);
  }
  const before = all.filter((r) => r.plainWorst < MIN).length;
  const bad = all.filter((r) => r.inkTint < MIN || r.inkPage < MIN || r.inkCard < MIN);
  console.log(`\n${all.length} themes. Plain red below ${MIN}:1 somewhere: ${before}. --red-text below ${MIN}:1 anywhere: ${bad.length}.`);
  if (process.argv.includes('--check') && bad.length) process.exit(1);
}
