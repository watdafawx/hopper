// The server list message (MOTD): § formatting codes, {variables}, and a
// preview that draws it like Minecraft's multiplayer list. The rules for
// variables match hopper_core::hosting::motd, which writes the real one.

/** Minecraft's 16 colours, by code. */
export const COLORS = [
  ['0', 'Black', '#000000'], ['1', 'Dark blue', '#0000AA'], ['2', 'Dark green', '#00AA00'], ['3', 'Dark aqua', '#00AAAA'],
  ['4', 'Dark red', '#AA0000'], ['5', 'Dark purple', '#AA00AA'], ['6', 'Gold', '#FFAA00'], ['7', 'Gray', '#AAAAAA'],
  ['8', 'Dark gray', '#555555'], ['9', 'Blue', '#5555FF'], ['a', 'Green', '#55FF55'], ['b', 'Aqua', '#55FFFF'],
  ['c', 'Red', '#FF5555'], ['d', 'Light purple', '#FF55FF'], ['e', 'Yellow', '#FFFF55'], ['f', 'White', '#FFFFFF'],
];
const HEX = Object.fromEntries(COLORS.map(([c, , h]) => [c, h]));

/** Bold, italic and the rest: code, name, short label. */
export const FORMATS = [
  ['l', 'Bold', 'B'], ['o', 'Italic', 'I'], ['n', 'Underline', 'U'], ['m', 'Strikethrough', 'S'], ['k', 'Obfuscated (scrambled letters)', '?'], ['r', 'Clear formatting', 'R'],
];

/** How wide (in Minecraft's pixels, GUI scale 1) the server list's
 * message area is: the 305 px row less the 32 px icon and 2 px gap. The
 * game word-wraps the message at this width and shows two lines. */
export const LIST_WIDTH = 271;

// Minecraft's default font: how far each character moves the pen (glyph
// width plus 1 px), for the ones that aren't 6. Bold adds 1. Mirrors
// hopper_core::hosting::motd::advance.
const NARROW = { ' ': 4, '!': 2, "'": 2, ',': 2, '.': 2, ':': 2, ';': 2, '|': 2, i: 2, '`': 3, l: 3, '"': 4, I: 4, '[': 4, ']': 4, t: 4, '(': 5, ')': 5, '*': 5, '<': 5, '>': 5, '{': 5, '}': 5, f: 5, k: 5, '@': 7, '~': 7 };
const advance = (c, bold) => (NARROW[c] ?? 6) + (bold ? 1 : 0);

/** How wide `line` (with § codes) is in the server list. */
export function lineWidth(line) {
  return runs(line).reduce((w, r) => w + [...r.text].reduce((a, c) => a + advance(c, r.l), 0), 0);
}

/** One message line as the server list breaks it: pieces that fit
 * LIST_WIDTH, broken at the last space like the game (the space goes),
 * else mid-word. Each piece starts with the codes in force, so it draws
 * on its own. */
export function wrapLine(line) {
  const out = [];
  let piece = '';
  let width = 0;
  let codes = '';
  let bold = false;
  let space = null; // the last space in this piece: [index, codes in force there]
  for (let i = 0; i < line.length; i++) {
    const c = line[i];
    if (c === '§' && i + 1 < line.length) {
      const code = line[i + 1].toLowerCase();
      if (HEX[code] || code === 'r') { codes = code === 'r' ? '' : `§${code}`; bold = false; }
      else if ('lonmk'.includes(code)) { codes += `§${code}`; bold ||= code === 'l'; }
      piece += c + line[i + 1];
      i++;
      continue;
    }
    const w = advance(c, bold);
    if (width + w > LIST_WIDTH && piece.replace(/§./g, '')) {
      if (space && lineWidth(space[1] + piece.slice(space[0] + 1)) + w <= LIST_WIDTH) {
        const [at, codesThen] = space;
        out.push(piece.slice(0, at));
        const rest = piece.slice(at + 1);
        piece = codesThen + rest;
        width = lineWidth(piece);
      } else {
        out.push(piece);
        piece = codes;
        width = 0;
      }
      space = null;
    }
    if (c === ' ') space = [piece.length, codes];
    piece += c;
    width += w;
  }
  out.push(piece);
  return out;
}

/** The two lines players see for `lines` (line 1 and line 2, filled in),
 * and how each line fares: `width` in Minecraft pixels; `wraps` when it's
 * wider than the list; `cut`, when its end doesn't show, how many of its
 * characters do (0: none, as line 2 is pushed out when line 1 wraps). */
export function listLines(lines) {
  const a = wrapLine(lines[0] ?? '');
  const b = lines[1] ? wrapLine(lines[1]) : [];
  return {
    shown: [...a, ...b].slice(0, 2),
    lines: [
      { width: lineWidth(lines[0] ?? ''), wraps: a.length > 1, cut: a.length > 2 ? visible(a[0]) + visible(a[1]) : undefined },
      { width: lineWidth(lines[1] ?? ''), wraps: b.length > 1, cut: !b.length ? undefined : a.length > 1 ? 0 : b.length > 1 ? visible(b[0]) : undefined },
    ],
  };
}

/** `&c` typed as a shortcut becomes `§c` (only before a real code). */
export const ampToSection = (s) => s.replace(/&([0-9a-fk-or])/gi, '§$1');

/** `{name}` → its value; unknown names stay as typed; `{{` and `}}` are
 * a literal brace. */
export function expand(t, vars) {
  let out = '';
  for (let i = 0; i < t.length; ) {
    if ((t[i] === '{' && t[i + 1] === '{') || (t[i] === '}' && t[i + 1] === '}')) {
      out += t[i];
      i += 2;
      continue;
    }
    if (t[i] === '{') {
      const m = /^\{([a-z_]+)\}/.exec(t.slice(i));
      if (m && vars && m[1] in vars) {
        out += vars[m[1]];
        i += m[0].length;
        continue;
      }
    }
    out += t[i++];
  }
  return out;
}

/** Text split into runs that look the same: [{ text, color, l, o, n, m, k }]. */
export function runs(text) {
  const out = [];
  let st = { color: HEX['7'], l: false, o: false, n: false, m: false, k: false };
  let buf = '';
  const flush = () => {
    if (buf) out.push({ text: buf, ...st });
    buf = '';
  };
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (c === '§' && i + 1 < text.length) {
      const code = text[i + 1].toLowerCase();
      flush();
      i++;
      if (HEX[code]) st = { color: HEX[code], l: false, o: false, n: false, m: false, k: false };
      else if (code === 'r') st = { color: HEX['7'], l: false, o: false, n: false, m: false, k: false };
      else if ('lonmk'.includes(code)) st = { ...st, [code]: true };
      continue;
    }
    buf += c;
  }
  flush();
  return out;
}

/** Characters a line shows (codes left out). */
export const visible = (line) => line.replace(/§./g, '').length;

/** The codes in force at `pos` in `text`, as codes to put back ("§6§l"),
 * or '' for plain text. */
function stateAt(text, pos) {
  let color = '';
  let fmt = '';
  for (let i = 0; i < pos - 1; i++) {
    if (text[i] !== '§') continue;
    const c = (text[i + 1] || '').toLowerCase();
    if (HEX[c]) { color = c; fmt = ''; }
    else if (c === 'r') { color = ''; fmt = ''; }
    else if ('lonmk'.includes(c) && !fmt.includes(c)) fmt += c;
    i++;
  }
  return (color ? `§${color}` : '') + [...fmt].map((f) => `§${f}`).join('');
}

/** Drops codes that change nothing: in a row of codes with no text
 * between, a colour or §r cancels the ones before it (Minecraft resets
 * styles on a colour), and a style given twice counts once. Codes at the
 * very end go too unless `keepEnd` (the caret may be about to type there). */
export function tidy(text, keepEnd = false) {
  let out = '';
  let run = [];
  const flush = () => {
    out += run.map((c) => `§${c}`).join('');
    run = [];
  };
  for (let i = 0; i < text.length; i++) {
    if (text[i] === '§' && i + 1 < text.length) {
      const c = text[i + 1].toLowerCase();
      i++;
      if (HEX[c] || c === 'r') run = [c];
      else if (!run.includes(c)) run.push(c);
      continue;
    }
    flush();
    out += text[i];
  }
  if (keepEnd) flush();
  return out;
}

/** Applies code `code` to text[a..b]: the code before, then what was in
 * force before it put back after (a colour clears bold and the rest in
 * Minecraft, so they're put back after a colour too). With nothing
 * selected the code goes in at the cursor. Returns [text, new a, new b]. */
export function applyCode(text, a, b, code) {
  const before = stateAt(text, a);
  let open = `§${code}`;
  if (HEX[code]) open += before.replace(/§[0-9a-f]/g, '');
  const head = tidy(text.slice(0, a) + open, true);
  if (a === b) return [head + text.slice(b), head.length, head.length];
  const close = code === 'r' ? '' : '§r' + before;
  const mid = text.slice(a, b);
  const tail = tidy(close + text.slice(b), true);
  return [head + mid + tail, head.length, head.length + mid.length];
}

/** The variables players can put in the message, with what each means. */
export const VARS = [
  ['pack', 'Modpack name'], ['pack_version', 'Modpack version'], ['mc', 'Minecraft version'],
  ['loader', 'Loader and version'], ['loader_version', 'Loader version only'],
  ['mods', 'Mods installed'], ['extra_mods', 'Mods added on top of the pack'],
  ['players', 'Players online'], ['max_players', 'Player slots'],
  ['day', 'In-game day'], ['world_size', 'World size on disk'],
  ['difficulty', 'Difficulty'], ['gamemode', 'Game mode'], ['name', 'Server name'],
];

// ---- The formatted editor (hopper_core::hosting::motd_edit has the same) ----
// A line is a list of spans { text, st, v }: text in one style `st`
// ({ color: code or null for plain gray, l, o, n, m, k }), and `v` for a
// variable ({day}), which is edited as a whole. Positions are indexes into
// the text the box shows: codes left out, variables as typed.

const FMT = ['l', 'o', 'n', 'm', 'k'];
/** Plain gray, no styles. */
export const plain = () => ({ color: null, l: false, o: false, n: false, m: false, k: false });
const same = (a, b) => a.color === b.color && FMT.every((f) => a[f] === b[f]);
/** Does style `st` have `code` (a colour or style code)? */
export const has = (st, code) => (FMT.includes(code) ? st[code] : st.color === code);
/** The style's colour as #RRGGBB. */
export const hexOf = (st) => HEX[st.color] ?? HEX['7'];

/** § codes read into characters, starting in style `start`. */
function parseUnits(raw, start) {
  const out = [];
  let st = { ...start };
  for (let i = 0; i < raw.length; i++) {
    if (raw[i] === '§' && i + 1 < raw.length) {
      const c = raw[i + 1].toLowerCase();
      i++;
      if (HEX[c]) st = { ...plain(), color: c };
      else if (c === 'r') st = plain();
      else if (FMT.includes(c)) st = { ...st, [c]: true };
      continue;
    }
    const ch = String.fromCodePoint(raw.codePointAt(i));
    if (ch.length > 1) i++;
    out.push({ text: ch, st, v: false });
  }
  return out;
}

/** Plain text split into text and {name} variables, as `expand` reads them. */
function splitVars(text, st, out) {
  let buf = '';
  for (let i = 0; i < text.length; ) {
    const two = text.slice(i, i + 2);
    if (two === '{{' || two === '}}') {
      buf += two;
      i += 2;
      continue;
    }
    const m = text[i] === '{' && /^\{[a-z_]+\}/.exec(text.slice(i));
    if (m) {
      if (buf) out.push({ text: buf, st, v: false });
      buf = '';
      out.push({ text: m[0], st, v: true });
      i += m[0].length;
      continue;
    }
    buf += text[i++];
  }
  if (buf) out.push({ text: buf, st, v: false });
}

function fromUnits(units) {
  const joined = [];
  for (const u of units) {
    const l = joined[joined.length - 1];
    if (l && !l.v && !u.v && same(l.st, u.st)) l.text += u.text;
    else joined.push({ ...u });
  }
  const out = [];
  for (const s of joined) {
    if (s.v) out.push(s);
    else splitVars(s.text, s.st, out);
  }
  return out;
}

function units(spans) {
  const out = [];
  for (const s of spans) {
    if (s.v) out.push({ ...s });
    else for (const ch of s.text) out.push({ text: ch, st: s.st, v: false });
  }
  return out;
}

/** Each unit with where it sits: [start, end, unit]. */
function placed(us) {
  let at = 0;
  return us.map((u) => [at, (at += u.text.length), u]);
}

/** Reads a line with § codes (turn & shortcuts into § first). */
export const parseLine = (raw) => fromUnits(parseUnits(raw, plain()));
/** The text the box shows. */
export const lineText = (spans) => spans.map((s) => s.text).join('');

/** The fewest § codes that draw the line. A colour resets the styles in
 * Minecraft, so styles still wanted are given again after one; dropping a
 * style means a colour (or §r for plain gray) first. */
export function toCodes(spans) {
  let out = '';
  let cur = plain();
  for (const s of spans) {
    if (!s.text) continue;
    const st = s.st;
    if (!same(st, cur)) {
      if (st.color === cur.color && FMT.every((f) => !cur[f] || st[f])) out += FMT.filter((f) => st[f] && !cur[f]).map((f) => `§${f}`).join('');
      else out += `§${st.color ?? 'r'}` + FMT.filter((f) => st[f]).map((f) => `§${f}`).join('');
      cur = st;
    }
    out += s.text;
  }
  return out;
}

/** The style typing at `pos` gets: the character before (after, at the start). */
export function styleAt(spans, pos) {
  let after = null;
  let at = 0;
  for (const s of spans) {
    const a = at;
    const b = (at += s.text.length);
    if (a < pos && pos <= b) return s.st;
    if (a === pos && !after) after = s.st;
  }
  return after ?? plain();
}

/** What every character in a..b shares (the style at the caret when empty). */
export function commonStyle(spans, a, b) {
  if (a >= b) return styleAt(spans, a);
  let out = null;
  for (const [x, y, u] of placed(units(spans))) {
    if (!(x < b && y > a)) continue;
    if (!out) out = { ...u.st };
    else {
      if (out.color !== u.st.color) out.color = null;
      for (const f of FMT) out[f] = out[f] && u.st[f];
    }
  }
  return out ?? styleAt(spans, a);
}

function restyle(spans, a, b, f) {
  return fromUnits(placed(units(spans)).map(([x, y, u]) => (x < b && y > a ? { ...u, st: f({ ...u.st }) } : u)));
}

/** A style button pressed over a..b: off when all of it has it, else on. A
 * colour clicked again goes back to the colour around it (the text before,
 * else after, else plain gray); a variable counts whole. */
export function toggle(spans, a, b, code) {
  if (a >= b) return spans;
  const on = !has(commonStyle(spans, a, b), code);
  const back = !on && !FMT.includes(code) ? colorAround(spans, a, b, code) : null;
  return restyle(spans, a, b, (st) => {
    if (FMT.includes(code)) st[code] = on;
    else st.color = on ? code : back;
    return st;
  });
}

/** The colour next to a..b that isn't `not`: the text before, else after. */
function colorAround(spans, a, b, not) {
  const pl = placed(units(spans));
  const first = pl.findIndex(([, y]) => y > a);
  let last = -1;
  pl.forEach(([x], i) => { if (x < b) last = i; });
  for (const i of [first - 1, last + 1]) {
    const u = pl[i]?.[2];
    if (u && u.st.color !== not) return u.st.color;
  }
  return null;
}

/** R: plain gray over a..b. */
export const clearStyle = (spans, a, b) => restyle(spans, a, b, () => plain());

/** A style with button `code` turned on or off (for typing with nothing
 * selected). */
export function withCode(st, code) {
  const out = { ...st };
  if (code === 'r') return plain();
  if (FMT.includes(code)) out[code] = !out[code];
  else out.color = out.color === code ? null : code;
  return out;
}

/** Brings the line in step with what the box now shows after typing,
 * deleting or pasting. New text takes `typing` (a style picked with nothing
 * selected) or the style where it went in; pasted § codes are read; a
 * variable cut into goes whole. Returns { spans, caret }; when lineText
 * then differs from `now`, put it back in the box. */
export function editLine(spans, now, typing) {
  const old = lineText(spans);
  if (old === now) return { spans, caret: now.length };
  let p = 0;
  while (p < old.length && p < now.length && old[p] === now[p]) p++;
  let s = 0;
  while (s < old.length - p && s < now.length - p && old[old.length - 1 - s] === now[now.length - 1 - s]) s++;
  const g0 = p;
  const g1 = old.length - s;
  const put = now.slice(p, now.length - s).replace(/[\r\n]/g, ' ');
  const st = typing ?? styleAt(spans, p);
  const added = put.includes('§') ? parseUnits(put, st) : [...put].map((ch) => ({ text: ch, st, v: false }));
  const n = added.reduce((w, u) => w + u.text.length, 0);
  // Typing inside a variable turns it into letters; cutting into one takes it whole.
  const us = [];
  for (const [x, y, u] of placed(units(spans))) {
    if (u.v && g0 === g1 && x < p && p < y) for (const ch of u.text) us.push({ text: ch, st: u.st, v: false });
    else us.push(u);
  }
  let from = g0;
  let to = g1;
  const pl = placed(us);
  if (g1 > g0) {
    for (const [x, y] of pl) {
      if (x < g1 && y > g0) {
        from = Math.min(from, x);
        to = Math.max(to, y);
      }
    }
  }
  const head = pl.filter(([, y]) => y <= from).map(([, , u]) => u);
  const tail = pl.filter(([x]) => x >= to).map(([, , u]) => u);
  const caret = head.reduce((w, u) => w + u.text.length, 0) + n;
  return { spans: fromUnits([...head, ...added, ...tail]), caret };
}

/** The variable at `pos`: { from, to, name }. */
export function varAt(spans, pos) {
  let at = 0;
  for (const s of spans) {
    const a = at;
    const b = (at += s.text.length);
    if (s.v && a <= pos && pos < b) return { from: a, to: b, name: s.text.slice(1, -1) };
  }
  return null;
}

/** Index `pos` of a raw line (§ codes) as an index in the shown text. */
export function rawToText(raw, pos) {
  let n = 0;
  for (let i = 0; i < pos && i < raw.length; i++) {
    if (raw[i] === '§' && i + 1 < raw.length) {
      i++;
      continue;
    }
    n++;
  }
  return n;
}

/** Index `pos` of the shown text in raw `raw`: after the codes there when
 * `afterCodes` (a selection's start), else before them. */
export function textToRaw(raw, pos, afterCodes) {
  let n = 0;
  for (let i = 0; i < raw.length; i++) {
    const code = raw[i] === '§' && i + 1 < raw.length;
    if (n === pos && !(afterCodes && code)) return i;
    if (code) {
      i++;
      continue;
    }
    n++;
  }
  return raw.length;
}
