// A pack's own override jar (ATM10's CC: Tweaked) has no project of its own,
// so nothing updates or syncs it, but the server says which mod of the pack it
// is (`shown_as`). It shows that mod's icon and page; it never gets Update,
// versions or Follow. Same as desktop's `Addon::shown_project`.

/// True for a row that only shows as the pack's mod.
export const isShown = (m) => (m.project_id || '0') === '0' && !!m.shown_as && m.shown_as !== '0';

/// The project a row's page is about: its own, else the one it shows as, else '0'.
export const shownProject = (m) => ((m.project_id || '0') !== '0' ? m.project_id : isShown(m) ? m.shown_as : '0');

/// Whether the jar is one Hopper can't place: no site, no link, and not the pack's own mod.
export const isUnknown = (m) => !m.source && !isShown(m);

/// Where its mod page is in Browse: { source, id }, or null.
export function shownBrowse(m) {
  if (!isShown(m)) return null;
  const source = m.shown_source || 'curseforge';
  const id = source === 'modrinth' ? m.shown_mr : m.shown_as;
  return id ? { source, id } : null;
}
