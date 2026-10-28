// Shared bits of the Browse page and a pack's page.

export const SITE = { curseforge: 'CurseForge', modrinth: 'Modrinth' };

/// What Browse shows, in desktop's order. `into`: installs into a server
/// (picked under "Install into"); modpacks make a new server instead.
/// (Resource packs, shaders and worlds are for the game, not a server.)
export const TYPES = {
  mods: { label: 'Mods', one: 'mod', many: 'mods', into: true },
  modpacks: { label: 'Modpacks', one: 'pack', many: 'packs', into: false },
  datapacks: { label: 'Data packs', one: 'data pack', many: 'data packs', into: true },
};

export const LOADER_NAME = { neoforge: 'NeoForge', forge: 'Forge', fabric: 'Fabric', quilt: 'Quilt' };

/// Where a project runs, as desktop's facts say it.
export const SIDE = { client: 'Client', server: 'Server', both: 'Client & server' };

/// "A", "A and B", "A, B and C".
export function andList(names = []) {
  if (names.length < 2) return names[0] || '';
  return `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}`;
}

/// 840 KB, 12 MB, 1.2 GB: like desktop's sizes.
export function size(b) {
  b = Number(b) || 0;
  const K = 1024;
  if (b >= K ** 3) return `${(b / K ** 3).toFixed(1)} GB`;
  if (b >= K ** 2) return `${Math.round(b / K ** 2)} MB`;
  return `${Math.round(b / K)} KB`;
}

/// Browse's hash for a type and the server installs go into:
/// "/browse/mods/<server id>". Modpacks with no server is plain "/browse".
/// Mods' "Add mods" button goes to `#${browseHash('mods', serverId)}`.
export function browseHash(type = 'modpacks', into = '') {
  if (type === 'modpacks' && !into) return '/browse';
  return `/browse/${type}/${into ? encodeURIComponent(into) : '-'}`;
}

/// 22.6M, 13.2K, 940: like desktop's counts.
export function count(n) {
  n = Number(n) || 0;
  if (n >= 1e9) return `${(n / 1e9).toFixed(1)}B`;
  if (n >= 1e6) return `${(n / 1e6).toFixed(1)}M`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(1)}K`;
  return String(Math.round(n));
}

/// "yesterday", "3 days ago", "2 weeks ago", "1 year ago".
export function ago(iso) {
  const t = Date.parse(iso);
  // CurseForge writes 0001-01-01 for "never".
  if (!t || t < 946684800000) return '';
  const d = (Date.now() - t) / 864e5;
  if (d < 1) return 'today';
  if (d < 2) return 'yesterday';
  if (d < 7) return `${Math.floor(d)} days ago`;
  if (d < 30) return `${Math.floor(d / 7)} week${d < 14 ? '' : 's'} ago`;
  if (d < 365) return `${Math.floor(d / 30)} month${d < 60 ? '' : 's'} ago`;
  return `${Math.floor(d / 365)} year${d < 730 ? '' : 's'} ago`;
}

export function date(iso) {
  const t = Date.parse(iso);
  return t ? new Date(t).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' }) : '';
}

/// What New server needs to start from this pack (and version).
export function preset(p, v = null) {
  return {
    id: p.num, source: p.source, name: p.name, icon: p.icon, author: p.author, summary: p.summary, loaders: p.loaders || [], url: p.url || '',
    file: v?.id || '', version: v ? { id: v.id, name: v.name, mc_version: v.mc_version, server_files: v.server_files, type: v.type, loaders: v.loaders || [] } : null,
  };
}

export function store(key, value) {
  try {
    if (value === undefined) return localStorage.getItem(key);
    localStorage.setItem(key, value);
  } catch {
    return null;
  }
}

/// Copies `text`; resolves true when it worked. The clipboard API needs a
/// secure page (https or localhost), so plain http on the LAN falls back
/// to a hidden text box.
export async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    const t = document.createElement('textarea');
    t.value = text;
    t.style.cssText = 'position:fixed;opacity:0;pointer-events:none';
    document.body.appendChild(t);
    t.select();
    let ok = false;
    try {
      ok = document.execCommand('copy');
    } catch {
      ok = false;
    }
    t.remove();
    return ok;
  }
}
