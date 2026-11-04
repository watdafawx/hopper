// A server's config files as settings, shared by the Configs page and each
// mod's config panel (Mods tab), as desktop's config view is shared by its
// Configs tab and side panel: the index (files, their mods, sides, changed
// counts), the files read so far, and the edits not saved yet. Edits stay
// while you move between mods, the page and the panel; Save writes them
// one file at a time, each checked together on the panel first.

import { api, post } from './api.js';

/** What a hidden value (a token, a password) shows as; the panel sends it. */
export const MASK = '••••••';

export const cfg = $state({
  sid: '',
  /** GET configs/index: { mods, files, unmatched, settings }. */
  index: null,
  /** rel -> { ok, text, sections, side, side_label, format } | { ok: false, error } */
  files: {},
  /** rel -> { id -> { path, key, value, old, label } } */
  edits: {},
  /** rel -> { id -> message } from the last Save. */
  errors: {},
  /** GET configs/players: the newest players' copy beside the server's. */
  players: null,
});

/** One setting's id inside its file. */
export const eid = (e) => `${(e.path || []).join('.')}\u0000${e.key}`;

const sidPath = (sid) => `/servers/${encodeURIComponent(sid)}/configs`;

function use(sid) {
  if (cfg.sid === sid) return;
  cfg.sid = sid;
  cfg.index = null;
  cfg.files = {};
  cfg.edits = {};
  cfg.errors = {};
  cfg.players = null;
}

let loading = null;
/** The index for a server (kept until `fresh`). */
export async function loadIndex(sid, fresh = false) {
  use(sid);
  if (cfg.index && !fresh) return cfg.index;
  if (loading?.sid === sid && !fresh) return loading.p;
  const p = api(`${sidPath(sid)}/index`).then((v) => {
    if (cfg.sid === sid) cfg.index = v;
    return v;
  });
  loading = { sid, p };
  try {
    return await p;
  } finally {
    if (loading?.p === p) loading = null;
  }
}

/** The players' copy compare (quiet when the panel can't say). */
export async function loadPlayers(sid) {
  use(sid);
  try {
    const v = await api(`${sidPath(sid)}/players`);
    if (cfg.sid === sid) cfg.players = v;
  } catch {
    // an older panel, or no access: no column
  }
  return cfg.players;
}

/** The players' copy of a setting when it differs from the server's: { client, server, client_rel } or null. */
export function playersOf(rel, e) {
  const f = cfg.players?.files.find((x) => x.rel === rel);
  const d = f?.diffs.find((x) => x.key === e.key && (x.path || []).join('.') === (e.path || []).join('.'));
  return d ? { ...d, client_rel: f.client_rel } : null;
}

/** Server files whose players' copy differs, with how many settings. */
export const playersDiffer = (rel) => cfg.players?.files.find((x) => x.rel === rel)?.diffs.length || 0;

/** Reads the files not read yet (or all of them with `fresh`). */
export async function loadFiles(sid, rels, fresh = false) {
  use(sid);
  const want = rels.filter((r) => fresh || !cfg.files[r]);
  if (!want.length) return;
  const r = await post(`${sidPath(sid)}/parse`, { rels: want });
  if (cfg.sid !== sid) return;
  for (const [rel, f] of Object.entries(r.files || {})) cfg.files[rel] = f;
}

/** The mod an index row of `key` is, with the Mods list's icon when the index has none. */
export function modOf(key) {
  return cfg.index?.mods.find((m) => m.key === key) || null;
}

/** The index's mod row for a Mods-list row (its file, maybe with .disabled). */
export function modForFile(file) {
  const k = (file || '').replace(/\.disabled$/i, '').toLowerCase();
  return cfg.index?.mods.find((m) => m.key.toLowerCase() === k) || null;
}

export const fileRow = (rel) => cfg.index?.files.find((f) => f.rel === rel) || null;

// ---- edits ------------------------------------------------------------------------

/** The value `e` will be saved with (the edit, else the file's). */
export function valueOf(rel, e) {
  return cfg.edits[rel]?.[eid(e)]?.value ?? e.value;
}

export const edited = (rel, e) => !!cfg.edits[rel]?.[eid(e)];
export const errorOf = (rel, e) => cfg.errors[rel]?.[eid(e)] || '';

/** Two values the way the game reads them (1 and 1.0, list spacing). */
export function same(a, b) {
  if (a === b) return true;
  const x = Number(a), y = Number(b);
  if (a !== '' && b !== '' && !Number.isNaN(x) && !Number.isNaN(y)) return x === y;
  return String(a).replace(/\s+/g, '') === String(b).replace(/\s+/g, '');
}

/** Sets `e` to `value` (typed text); back to the file's value drops the edit. */
export function setEdit(rel, e, value) {
  const id = eid(e);
  if (cfg.errors[rel]?.[id]) delete cfg.errors[rel][id];
  if (same(value, e.value) && !(e.masked && value !== e.value)) {
    if (cfg.edits[rel]) {
      delete cfg.edits[rel][id];
      if (!Object.keys(cfg.edits[rel]).length) delete cfg.edits[rel];
    }
    return;
  }
  cfg.edits[rel] ??= {};
  cfg.edits[rel][id] = { path: e.path || [], key: e.key, value, old: e.value, label: e.label || e.key };
}

export function dropEdit(rel, id) {
  if (!cfg.edits[rel]) return;
  delete cfg.edits[rel][id];
  if (!Object.keys(cfg.edits[rel]).length) delete cfg.edits[rel];
}

export function discardAll(rels = null) {
  for (const r of rels || Object.keys(cfg.edits)) delete cfg.edits[r];
  cfg.errors = {};
}

/** [{ rel, id, ...edit }] of everything not saved. */
export function pendingList(rels = null) {
  const out = [];
  for (const [rel, m] of Object.entries(cfg.edits)) {
    if (rels && !rels.includes(rel)) continue;
    for (const [id, x] of Object.entries(m)) out.push({ rel, id, ...x });
  }
  return out;
}

/**
 * Saves the edits (all, or the ones `keep` says), one file at a time.
 * Resolves to { saved: n settings, files: [rel], backups: [text], reapplied: [rel], failed: [rel] }.
 */
export async function saveAll(sid, keep = null) {
  const list = pendingList().filter((x) => !keep || keep.has(`${x.rel}\u0000${x.id}`));
  const byFile = new Map();
  for (const x of list) {
    if (!byFile.has(x.rel)) byFile.set(x.rel, []);
    byFile.get(x.rel).push(x);
  }
  const out = { saved: 0, files: [], backups: [], reapplied: [], failed: [] };
  for (const [rel, xs] of byFile) {
    try {
      if (!cfg.files[rel]?.ok) await loadFiles(sid, [rel], true);
      const base = cfg.files[rel]?.text ?? '';
      const r = await post(`${sidPath(sid)}/batch`, { rel, base, edits: xs.map((x) => ({ path: x.path, key: x.key, value: x.value })) });
      if (!r.ok) {
        out.failed.push(rel);
        const errs = {};
        for (const er of r.errors || []) {
          const id = er.key ? `${(er.path || []).join('.')}\u0000${er.key}` : xs[0].id;
          errs[id] = er.error;
        }
        cfg.errors[rel] = errs;
        continue;
      }
      cfg.files[rel] = { ...cfg.files[rel], ok: true, text: r.text, sections: r.sections };
      for (const x of xs) dropEdit(rel, x.id);
      delete cfg.errors[rel];
      out.saved += xs.length;
      out.files.push(rel);
      if (r.backup_path) out.backups.push(r.backup_path);
      if (r.reapplied) out.reapplied.push(rel);
    } catch (e) {
      out.failed.push(rel);
      cfg.errors[rel] = { [xs[0].id]: e.message };
    }
  }
  // The changed counts moved.
  if (out.saved) loadIndex(sid, true).catch(() => {});
  return out;
}

// ---- words ---------------------------------------------------------------------------

const num = (x) => (Number.isInteger(x) ? String(x) : String(Number(x.toPrecision(6))));
/** "No limit" stands in for the huge maxima some mods write (3.4E38). */
const huge = (x) => x != null && Math.abs(x) >= 1e15;

/** "0 to 4 · default 2" */
export function limits(e) {
  const out = [];
  if (e.range) {
    let [lo, hi] = e.range;
    if (huge(lo)) lo = null;
    if (huge(hi)) hi = null;
    if (lo != null && hi != null) out.push(`${num(lo)} to ${num(hi)}`);
    else if (lo != null) out.push(`at least ${num(lo)}`);
    else if (hi != null) out.push(`at most ${num(hi)}`);
  }
  if (e.default != null && e.default !== '') out.push(`default ${e.default}`);
  return out.join(' · ');
}

/** The local check before a value is kept: why it isn't allowed, or ''. */
export function check(e, text) {
  const t = String(text).trim();
  if (e.kind === 'int' && !/^[-+]?\d+$/.test(t)) return 'Type a whole number.';
  if ((e.kind === 'float' || e.kind === 'int') && (t === '' || Number.isNaN(Number(t)))) return 'Type a number.';
  if ((e.kind === 'int' || e.kind === 'float') && e.range) {
    const n = Number(t);
    const [lo, hi] = e.range;
    if (lo != null && hi != null && (n < lo || n > hi)) return `Between ${num(lo)} and ${num(hi)}.`;
    if (lo != null && n < lo) return `At least ${num(lo)}.`;
    if (hi != null && n > hi) return `At most ${num(hi)}.`;
  }
  return '';
}

/** A list's text from its items, quoted the way the file has it. */
export function listText(items, e) {
  const orig = e.value || '';
  const quoted = /["']/.test(orig) || (!(e.list || []).length && items.some((i) => Number.isNaN(Number(i)) && i !== 'true' && i !== 'false'));
  const parts = items.map((i) => (quoted ? `"${i.replace(/\\/g, '\\\\').replace(/"/g, '\\"')}"` : i));
  return `[${parts.join(', ')}]`;
}

/** The items of a flat list's text (`["a", "b"]`), for an edited list. */
export function listItems(text) {
  const t = String(text).trim();
  if (!t.startsWith('[') || !t.endsWith(']')) return [];
  const out = [];
  const re = /"((?:[^"\\]|\\.)*)"|'([^']*)'|([^,\s][^,]*)/g;
  let m;
  const inner = t.slice(1, -1);
  while ((m = re.exec(inner))) {
    if (m[1] != null) out.push(m[1].replace(/\\(.)/g, '$1'));
    else if (m[2] != null) out.push(m[2]);
    else if (m[3].trim()) out.push(m[3].trim());
  }
  return out;
}

const hex2 = (n) => n.toString(16).padStart(2, '0');

/** `#rrggbb` for the swatch from a colour setting's value. */
export function swatchOf(e, value) {
  const v = String(value).trim();
  let n = null;
  if (/^#|^0x/i.test(v)) {
    let h = v.replace(/^#|^0x/i, '');
    if (h.length === 3) h = [...h].map((c) => c + c).join('');
    if (/^[0-9a-f]{6}$|^[0-9a-f]{8}$/i.test(h)) n = parseInt(h.slice(-6), 16);
  } else if (/^-?\d+$/.test(v)) {
    n = Number(v) & 0xffffff;
  }
  return n == null ? null : `#${hex2((n >> 16) & 255)}${hex2((n >> 8) & 255)}${hex2(n & 255)}`;
}

/** The colour picked (`#rrggbb`) written the way the setting is, alpha kept. */
export function colorText(e, picked) {
  const rgb = parseInt(picked.slice(1), 16);
  const v = String(e.value).trim();
  if (e.color_fmt === 'int') {
    const n = Number(v);
    const alpha = n > 0xffffff || n < 0 ? (n >>> 24) & 255 : null;
    if (alpha == null) return String(rgb);
    const full = ((alpha << 24) | rgb) >>> 0;
    return String(n < 0 ? full | 0 : full);
  }
  const body = v.replace(/^#|^0x/i, '');
  const upper = /[A-F]/.test(body);
  let out = rgb.toString(16).padStart(6, '0');
  if (body.length === 8) out = body.slice(0, 2) + out;
  if (upper) out = out.toUpperCase();
  const prefix = v.startsWith('#') ? '#' : v.startsWith('0X') ? '0X' : '0x';
  return prefix + out;
}

/** The side picker's choices: who reads a file. */
export const SIDES = [
  ['all', 'All'],
  ['client', "Players' game"],
  ['common', 'Both'],
  ['server', 'Server decides'],
];

/** A file's side chip, in the panel's words (desktop says "This PC"). */
export function sideChip(side) {
  return { client: ["Players' game", ''], common: ['Both', 'lan'], server: ['Server decides', 'warn'], startup: ['At start', ''] }[side] || null;
}

/** The file's name without the config folder. */
export const shortRel = (rel) => rel.replace(/^(config|defaultconfigs|kubejs\/config)\//, '');
