// Files tab state that outlives the tab: back/forward history per server,
// the cut/copy clipboard, and view choices.

const hist = new Map();

/** { back: string[], fwd: string[] } for a server. */
export function historyOf(sid) {
  if (!hist.has(sid)) hist.set(sid, { back: [], fwd: [] });
  return hist.get(sid);
}

/** Cut or copied files: { op: 'move' | 'copy', sid, paths, from }. */
export const clip = $state({ op: '', sid: '', paths: [], from: '' });

function stored(key, fallback) {
  try {
    return localStorage.getItem(key) ?? fallback;
  } catch {
    return fallback;
  }
}

export function store(key, value) {
  try {
    localStorage.setItem(key, String(value));
  } catch {
    // private mode: not remembered
  }
}

export const prefs = $state({
  view: stored('hopperd-files-view', 'list'),
  tree: stored('hopperd-files-tree', '1') === '1',
  preview: stored('hopperd-files-preview', '1') === '1',
  /** config/ grouped under the mod each file belongs to. */
  group: stored('hopperd-files-group', '1') === '1',
});

export const TEXT = /\.(toml|json5?|properties|txt|cfg|conf|ini|ya?ml|js|ts|mcmeta|snbt|log|md|mcfunction|csv|bat|sh|cmd|ps1|xml|zs|nbt\.txt|lang|secret)$/i;
export const ARCHIVE = /\.(zip|jar|mrpack)$/i;
export const PACKED = /\.(gz|tgz|7z|rar)$/i;
export const IMAGE = /\.(png|jpe?g|gif|webp)$/i;

/** Where the next opened file should land: its 1-based line (search hits). */
export const jump = $state({ path: '', line: 0 });
