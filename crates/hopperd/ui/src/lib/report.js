// Links to Hopper's GitHub page and its "Report a problem" form, the same as
// desktop's (crates/hopper-core/src/report.rs). The form reads its fields
// from the link, so the app and version arrive filled in. Nothing else is
// sent: a log is only shared when the player does it on mclo.gs.

export const REPO_URL = 'https://github.com/watdafawx/hopper';

/** This panel's version, from /hello (set once by the shell). */
let version = '';
export function setVersion(v) {
  version = v || '';
}

/** The bug form for Hopper Server, titled `title` (e.g. "Crash: …") when given. */
export function issueUrl(title = '') {
  const q = new URLSearchParams({ template: 'bug_report.yml', app: 'Hopper Server', version });
  const t = (title || '').trim().slice(0, 120);
  if (t) q.set('title', t);
  // GitHub reads "+" as a space too, but %20 matches desktop's links.
  return `${REPO_URL}/issues/new?${q.toString().replace(/\+/g, '%20')}`;
}
