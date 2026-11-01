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
  ['l', 'Bold', 'B'], ['o', 'Italic', 'I'], ['n', 'Underline', 'U'], ['m', 'Strikethrough', 'S'], ['k', 'Obfuscated (scrambled letters)', '?'], ['r', 'Reset: back to plain gray', 'R'],
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
