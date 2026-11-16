// What the Mods tab's notices read from a server's mods list, as desktop's
// (addons_extra.rs): the same mod in twice and client-only mods turned on.
// Pure, so the Mods tab and the page's notices on other tabs agree.

/** Groups of copies of one mod (an old jar left beside the new one). */
export function twiceOf(mods) {
  const g = new Map();
  for (const m of mods) if (m.source && m.project_id !== '0') g.set(m.project_id, [...(g.get(m.project_id) || []), m]);
  return [...g.values()].filter((l) => l.length > 1);
}

/** Every copy but the newest (a turned-on one first), as desktop's "Keep newest". */
export function olderOf(twice) {
  return twice.flatMap((l) => {
    const keep = [...l].sort((a, b) => Number(b.enabled) - Number(a.enabled) || (b.file_date || '').localeCompare(a.file_date || ''))[0];
    return l.filter((m) => m !== keep);
  });
}

/** Client-only mods that are turned on. */
export const clientOnOf = (mods) => mods.filter((m) => m.side === 'client' && m.enabled).length;

/** A notice's subject from names: "A, B, C" or "A, B and 4 more" (desktop's brief_names). */
export function briefNames(names) {
  const n = [...new Set(names.filter(Boolean))];
  return n.length <= 3 ? n.join(', ') : `${n[0]}, ${n[1]} and ${n.length - 2} more`;
}
