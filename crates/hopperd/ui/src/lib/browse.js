// Shared bits of the Browse page and a pack's page.

export const SITE = { curseforge: 'CurseForge', modrinth: 'Modrinth' };

/// What Browse shows. `into`: installs into a server (picked under
/// "Install into"); modpacks make a new server instead.
export const TYPES = {
  modpacks: { label: 'Modpacks', one: 'pack', many: 'packs', into: false },
  mods: { label: 'Mods', one: 'mod', many: 'mods', into: true },
  datapacks: { label: 'Data packs', one: 'data pack', many: 'data packs', into: true },
};

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
  if (!t) return '';
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
    id: p.num, source: p.source, name: p.name, icon: p.icon, author: p.author, summary: p.summary,
    file: v?.id || '', version: v ? { id: v.id, name: v.name, mc_version: v.mc_version, server_files: v.server_files, type: v.type } : null,
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
