// Which of this panel's own download addresses another panel can make a
// one-use link for, and the link request for it (lib/panelfiles.svelte.js).

/** The one-use link request for a download URL, or null when there is none. */
export function linkBody(url) {
  let m = url.match(/^\/api\/v1\/fs\/([^/?]+)\/(file|zip|zip-entry)\?(.*)$/);
  if (m) {
    const q = new URLSearchParams(m[3]);
    const inline = q.has('inline');
    if (m[2] === 'file') return { server: m[1], body: { path: q.get('path') || '', inline } };
    if (m[2] === 'zip-entry') return { server: m[1], body: { path: q.get('path') || '', entry: q.get('entry') || '', inline } };
    const name = q.get('name');
    return { server: m[1], body: { paths: q.getAll('path'), zip: true, ...(name ? { name } : {}) } };
  }
  m = url.match(/^\/api\/v1\/servers\/([^/?]+)\/backups\/([^/?]+)\/download$/);
  if (m) return { server: m[1], body: { backup: decodeURIComponent(m[2]) } };
  return null;
}

