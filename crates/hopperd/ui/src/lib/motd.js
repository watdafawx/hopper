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

/** The server list shows two lines of about this many characters. */
export const LINE_CHARS = 45;

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
