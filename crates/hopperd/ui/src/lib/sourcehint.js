// The Mods tab's "Unknown" hint: jars that came along with a sync from
// Hopper desktop lose their GitHub or link source when this panel has mods
// from links off. The panel can't see the instance's sources, so it only
// says "may": shown in the Unknown view, to someone who can turn the setting
// on (the settings answer reached us), when it is off and the jars aren't
// identified.
export function showSourcesHint({ known, allowed, chip, unknown, closed }) {
  return !!known && !allowed && chip === 'unknown' && unknown > 0 && !closed;
}
