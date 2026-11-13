<script>
  // A CurseForge project's comments, as desktop's Comments tab: pinned
  // first, then newest; each with its author's picture, name and when, as
  // plain text (never the site's HTML), replies under it. Asked for only
  // when shown, 20 at a time, kept for the visit. The panel fetches them
  // (the browser never talks to CurseForge), and when that fails it says so
  // and links to the page.
  import { untrack } from 'svelte';
  import { Pin, MessageSquareOff, TriangleAlert, ExternalLink } from '@lucide/svelte';
  import { api } from './api.js';
  import { ago, date } from './browse.js';

  // `project`: the CurseForge project's number; `url`: its page on the site.
  let { project, url = '' } = $props();

  // project -> { pages: [...comments], next, more, busy, error }
  const seen = new Map();
  let st = $state({ list: [], next: 0, more: false, busy: true, error: '' });

  const iso = (s) => (s ? new Date(s * 1000).toISOString() : '');
  const when = (s) => ago(iso(s)) || date(iso(s));
  const link = $derived(url ? `${url.replace(/\/+$/, '')}/comments` : '');

  async function load(pid, page) {
    st.busy = true;
    st.error = '';
    try {
      const r = await api(`/browse/comments?project=${pid}&page=${page}`);
      if (pid !== project) return;
      // A page can repeat the end of the last one if comments came in.
      const have = new Set(st.list.map((c) => c.id));
      st.list = [...st.list, ...r.comments.filter((c) => !have.has(c.id))];
      st.next = page + 1;
      st.more = r.more;
    } catch {
      if (pid !== project) return;
      st.error = "Comments couldn't be loaded.";
    }
    st.busy = false;
    seen.set(pid, $state.snapshot(st));
  }

  // Only a new project starts over; the list's own changes don't.
  $effect(() => {
    const pid = project;
    untrack(() => {
      const kept = seen.get(pid);
      if (kept) st = kept;
      else {
        st = { list: [], next: 0, more: false, busy: true, error: '' };
        load(pid, 0);
      }
    });
  });
</script>

{#snippet one(c, reply)}
  <div class="cm" class:reply class:pinned={c.pinned}>
    {#if c.avatar}<img class="av" src={c.avatar} alt="" width="28" height="28" loading="lazy" referrerpolicy="no-referrer" />{:else}<span class="av ph">{c.removed ? '' : (c.author || '?')[0].toUpperCase()}</span>{/if}
    <div class="col grow">
      <div class="row hd">
        <b class="small ell">{c.removed ? 'Removed' : c.author}</b>
        {#if c.pinned}<span class="chip pin"><Pin size={11} />Pinned</span>{/if}
        <span class="tiny muted" title={date(iso(c.date))}>{when(c.date)}</span>
      </div>
      {#if c.removed}<p class="small muted">This comment was removed.</p>{:else}<p class="small txt">{c.text}</p>{/if}
      {#each c.replies as r (r.id)}{@render one(r, true)}{/each}
    </div>
  </div>
{/snippet}

<div class="comments">
  {#if st.list.length}
    {#each st.list as c (c.id)}{@render one(c, false)}{/each}
  {:else if st.busy}
    {#each Array(4) as _, i (i)}<div class="skel"><i></i></div>{/each}
  {:else if st.error}
    <div class="empty-state"><span class="es-ic"><TriangleAlert size={18} /></span><b>{st.error}</b><p>CurseForge didn't answer. They're still on its page.</p></div>
  {:else}
    <div class="empty-state"><span class="es-ic"><MessageSquareOff size={18} /></span><b>No comments</b><p>Nobody has commented yet, or comments are off.</p></div>
  {/if}
  {#if st.list.length && st.error}<div class="small err">{st.error}</div>{/if}
  <div class="row foot">
    {#if st.more && st.list.length}<button class="btn sm" disabled={st.busy} onclick={() => load(project, st.next)}>{st.busy ? 'Loading…' : 'Show more'}</button>{/if}
    <span class="sp"></span>
    {#if link}<a class="btn sm ghost" href={link} target="_blank" rel="noreferrer"><ExternalLink size={13} />Open comments on CurseForge</a>{/if}
  </div>
</div>

<style>
  .comments { display: flex; flex-direction: column; gap: 4px; max-width: 860px; }
  .cm { display: flex; gap: 10px; padding: 10px 8px; border-radius: var(--radius); align-items: flex-start; }
  .cm + .cm:not(.reply) { border-top: 1px solid var(--line); border-radius: 0; }
  .cm.reply { padding: 8px 0 0; }
  .av { width: 28px; height: 28px; border-radius: 50%; flex: none; object-fit: cover; background: var(--raise); }
  .av.ph { display: grid; place-items: center; font-size: 12px; font-weight: 600; color: var(--muted); }
  .reply .av { width: 22px; height: 22px; }
  .grow { flex: 1; min-width: 0; gap: 2px; }
  .hd { gap: 8px; align-items: center; min-width: 0; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .txt { white-space: pre-wrap; overflow-wrap: anywhere; line-height: 1.45; }
  .pin { display: inline-flex; align-items: center; gap: 3px; color: var(--acc); background: var(--acc-dim); flex: none; }
  .hd .tiny { flex: none; }
  .foot { gap: 8px; margin-top: 8px; align-items: center; }
  .sp { flex: 1; }
  .err { color: var(--red); }
</style>
