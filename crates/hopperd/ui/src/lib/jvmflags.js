// The Java arguments editor's text side: splitting the box into arguments
// with their parts (for colour) and suggesting flags while typing. The flag
// list and what's wrong with each argument come from the panel (hopper-core's
// jvm_editor, the same code as Hopper desktop's editor); this only mirrors
// its splitting and matching so colours and suggestions keep up with typing.

/** Arguments with [start, end) offsets; a `#` starting one comments out the rest of its line. */
export function tokens(text) {
  const out = [];
  let start = -1, quoted = false, comment = false;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (comment) { comment = c !== '\n'; continue; }
    if (c === '#' && start < 0) { comment = true; continue; }
    if (c === '"') quoted = !quoted;
    if (/\s/.test(c) && !quoted) {
      if (start >= 0) { out.push({ start, end: i, text: text.slice(start, i) }); start = -1; }
    } else if (start < 0) start = i;
  }
  if (start >= 0) out.push({ start, end: text.length, text: text.slice(start) });
  return out;
}

/** Where the name and value sit inside one argument: [prefixEnd, nameEnd, valueStart|-1]. */
export function parts(t) {
  const eq = (from) => {
    const e = t.indexOf('=', from);
    return e < 0 ? [from, t.length, -1] : [from, e, e + 1];
  };
  if (t.startsWith('-XX:')) {
    const rest = t.slice(4);
    return eq(4 + (rest.length - rest.replace(/^[+-]+/, '').length));
  }
  if (t.startsWith('-D')) return eq(2);
  const m = /^-(Xm[xsn]|Xss)/.exec(t);
  if (m) return [1, 1 + m[1].length, t.length > 1 + m[1].length ? 1 + m[1].length : -1];
  return eq(t.length - t.replace(/^-+/, '').length);
}

/** A flag's name for matching, as hopper-core's `key`. */
export function key(t) {
  if (t.startsWith('-XX:')) return t.slice(4).replace(/^[+-]+/, '').split('=')[0];
  if (t.startsWith('-D')) return t.slice(2).split('=')[0].replace(/^(jdk\.graal\.|graal\.)/, '');
  for (const x of ['Xmx', 'Xms', 'Xss']) if (t.startsWith('-' + x)) return x;
  return t;
}

/** The argument being typed: from the last space before `offset` to it. */
export function wordAt(text, offset) {
  const before = text.slice(0, offset);
  const m = /\S*$/.exec(before);
  return { start: offset - m[0].length, word: m[0] };
}

/** Known flags matching what's typed, starting-with first, then name-contains (as hopper-core's `complete`). */
export function complete(word, catalog) {
  if (!word.startsWith('-') || word.length < 2) return [];
  const lower = word.toLowerCase();
  const off = word.startsWith('-XX:-');
  const typed = key(word).toLowerCase();
  const family = (t) => (t.startsWith('-XX:') ? 0 : t.startsWith('-D') ? 1 : 2);
  const wf = lower.startsWith('-xx') ? 0 : word.startsWith('-D') ? 1 : lower === '-x' ? null : word.startsWith('-X') ? 2 : null;
  const hits = [];
  for (const { insert: t, detail } of catalog || []) {
    const insert = off && t.startsWith('-XX:+') ? `-XX:-${t.slice(5)}` : t;
    let rank;
    if (insert.toLowerCase().startsWith(lower)) rank = 0;
    else if (typed.length >= 3 && (wf === null || wf === family(t)) && key(t).toLowerCase().includes(typed)) rank = 1;
    else continue;
    if (insert === word) continue;
    hits.push({ rank, insert, detail });
  }
  hits.sort((a, b) => a.rank - b.rank || (a.insert < b.insert ? -1 : a.insert > b.insert ? 1 : 0));
  return hits;
}
