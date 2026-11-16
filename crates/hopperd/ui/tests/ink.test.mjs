import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { MIN, rgb, toHex, ratio, surfaces, worst, inkRed } from '../src/lib/ink.js';

const themes = JSON.parse(readFileSync(new URL('../src/lib/themes.json', import.meta.url), 'utf8'));

test('red small text reads (>= 4.5:1) in every theme, on every surface and on its own tint', () => {
  assert.equal(themes.length, 19);
  for (const t of themes) {
    const surf = surfaces(t);
    const ink = inkRed(t);
    for (const [name, bg] of Object.entries(surf)) assert.ok(ratio(ink, bg) >= MIN, `${t.name}: on ${name} ${ratio(ink, bg).toFixed(2)}:1`);
    assert.ok(worst(ink, surf, rgb(t.danger)) >= MIN, `${t.name}: on its red tint`);
  }
});

test('a red that already reads is left alone', () => {
  const t = themes.find((x) => x.name === 'Gruvbox Light');
  assert.equal(toHex(inkRed(t)), t.danger);
});

test('it stays red: the hue is kept, only the lightness moves', () => {
  for (const t of themes) {
    const [r, g, b] = inkRed(t);
    assert.ok(r >= g && r >= b, `${t.name}: red channel leads (${toHex([r, g, b])})`);
  }
});

test('the Hopper Dark value in app.css is what the fit gives', () => {
  const css = readFileSync(new URL('../src/app.css', import.meta.url), 'utf8');
  const m = css.match(/--red-text:\s*(#[0-9a-f]{6})/i);
  assert.ok(m, 'app.css has --red-text');
  assert.equal(m[1].toLowerCase(), toHex(inkRed(themes.find((t) => t.name === 'Hopper Dark'))));
});

test('no text colour in the UI is the plain red', () => {
  // The plain red is for fills, dots and borders; text and icons use --red-text.
  function walk(dir, out = []) {
    for (const f of readdirSync(dir)) {
      const p = `${dir}/${f}`;
      statSync(p).isDirectory() ? walk(p, out) : /\.(svelte|css)$/.test(f) && out.push(p);
    }
    return out;
  }
  const root = new URL('../src', import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1');
  const bad = [];
  for (const p of walk(root)) {
    readFileSync(p, 'utf8').split(/\r?\n/).forEach((l, i) => {
      if (/(^|[^-\w])color:\s*var\(--red\)/.test(l) || l.includes('color="var(--red)"')) bad.push(`${p.slice(root.length)}:${i + 1}`);
    });
  }
  assert.deepEqual(bad, []);
});
