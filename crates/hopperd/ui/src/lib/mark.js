// Search highlighting, as desktop's widgets::marked: a search's words, and
// a text cut into pieces with every place a word appears marked.

/** The words of a search box, lower-case. */
export const searchWords = (q) => (q || '').trim().toLowerCase().split(/\s+/).filter(Boolean);

/** `text` as [{ t, hit }] pieces; one plain piece when nothing matches. */
export function marks(text, words) {
  text = text == null ? '' : String(text);
  if (!words?.length || !text) return [{ t: text, hit: false }];
  const lower = text.toLowerCase();
  // Lower-casing can change lengths outside ASCII; then the offsets wouldn't line up.
  if (lower.length !== text.length) return [{ t: text, hit: false }];
  const at = [];
  for (const w of words) {
    for (let i = lower.indexOf(w); i >= 0; i = lower.indexOf(w, i + w.length)) at.push([i, i + w.length]);
  }
  if (!at.length) return [{ t: text, hit: false }];
  at.sort((a, b) => a[0] - b[0]);
  const merged = [];
  for (const r of at) {
    const last = merged[merged.length - 1];
    if (last && r[0] <= last[1]) last[1] = Math.max(last[1], r[1]);
    else merged.push([...r]);
  }
  const out = [];
  let from = 0;
  for (const [s, e] of merged) {
    if (s > from) out.push({ t: text.slice(from, s), hit: false });
    out.push({ t: text.slice(s, e), hit: true });
    from = e;
  }
  if (from < text.length) out.push({ t: text.slice(from), hit: false });
  return out;
}
