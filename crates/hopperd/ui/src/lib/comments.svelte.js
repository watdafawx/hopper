// A CurseForge project's comments, kept for the visit: the Comments tab
// reads them, and a peek asks for the first page ahead (`prefetchComments`)
// so the tab is ready when picked and can show the total. The panel
// fetches them; the browser never talks to CurseForge.
import { untrack } from 'svelte';
import { SvelteMap } from 'svelte/reactivity';
import { api } from './api.js';

export const COMMENTS_FAILED = "Comments couldn't be loaded.";

// project -> { list, next, more, busy, error, total }; `total` stays null
// until a page has answered (the tab then shows no number, not 0).
// Reading never makes an entry (a reader that made the state it reads would
// not hear of its changes); `loadComments` does.
const kept = new SvelteMap();
const EMPTY = Object.freeze({ list: [], next: 0, more: false, busy: false, error: '', total: null });

export const commentsOf = (project) => kept.get(String(project)) ?? EMPTY;

function entry(project) {
  const key = String(project);
  if (!kept.has(key)) {
    const fresh = $state({ list: [], next: 0, more: false, busy: false, error: '', total: null });
    kept.set(key, fresh);
  }
  return kept.get(key);
}

// The tab's label: the total once the first page is in.
export const commentsLabel = (project) => {
  const t = commentsOf(project).total;
  return t == null ? 'Comments' : `Comments ${t}`;
};

// Asks for one page. `quiet`: asked ahead, so a failure shows nothing and
// the tab asks again when opened.
export async function loadComments(project, page, quiet = false) {
  const st = entry(project);
  if (st.busy) return;
  st.busy = true;
  st.error = '';
  try {
    const r = await api(`/browse/comments?project=${encodeURIComponent(project)}&page=${page}`);
    // A page can repeat the end of the last one if comments came in.
    const have = new Set(st.list.map((c) => c.id));
    st.list = [...st.list, ...r.comments.filter((c) => !have.has(c.id))];
    st.next = page + 1;
    st.more = r.more;
    st.total = r.total;
  } catch {
    if (!quiet) st.error = COMMENTS_FAILED;
  }
  st.busy = false;
}

// The first page, once, in the background (never throws).
export function prefetchComments(project) {
  untrack(() => {
    const st = commentsOf(project);
    if (st.busy || st.list.length || st.total != null) return;
    loadComments(project, 0, true);
  });
}
