<script>
  // A CurseForge project's comments, as desktop's Comments tab: pinned
  // first, then newest; each with its author's picture, name and when, as
  // plain text (never the site's HTML), replies under it, a tag on what the
  // project's own author wrote. 20 at a time, kept for the visit (a peek may
  // have asked for the first page ahead, see comments.svelte.js). The panel
  // fetches them (the browser never talks to CurseForge), and when that
  // fails it says so and links to the page.
  import { untrack } from 'svelte';
  import { Pin, MessageSquareOff, TriangleAlert, ExternalLink } from '@lucide/svelte';
  import { ago, date } from './browse.js';
  import { commentsOf, loadComments } from './comments.svelte.js';

  // `project`: the CurseForge project's number; `url`: its page on the site.
  let { project, url = '' } = $props();

  // The list is kept per project (and may already be there: the peek asks
  // for the first page ahead).
  const st = $derived(commentsOf(project));
  // Nothing back yet (nor a failure): the skeleton, never "No comments".
  const waiting = $derived(st.busy || (st.total == null && !st.error));

  const iso = (s) => (s ? new Date(s * 1000).toISOString() : '');
  const when = (s) => ago(iso(s)) || date(iso(s));
  const link = $derived(url ? `${url.replace(/\/+$/, '')}/comments` : '');

  // Opening the tab with nothing loaded (or an ask-ahead that failed) asks.
  $effect(() => {
    const pid = project;
    untrack(() => {
      const s = commentsOf(pid);
      if (!s.busy && !s.list.length) loadComments(pid, 0);
    });
  });
</script>

{#snippet one(c, reply)}
  <div class="cm" class:reply class:pinned={c.pinned}>
    {#if c.avatar}<img class="av" src={c.avatar} alt="" width="28" height="28" loading="lazy" referrerpolicy="no-referrer" />{:else}<span class="av ph">{c.removed ? '' : (c.author || '?')[0].toUpperCase()}</span>{/if}
    <div class="col grow">
      <div class="row hd">
        <b class="small ell">{c.removed ? 'Removed' : c.author}</b>
        {#if c.by_author}<span class="chip maker">Author</span>{/if}
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
  {:else if waiting}
    {#each Array(4) as _, i (i)}<div class="skel"><i></i></div>{/each}
  {:else if st.error}
    <div class="empty-state"><span class="es-ic"><TriangleAlert size={18} /></span><b>{st.error}</b><p>CurseForge didn't answer. They're still on its page.</p></div>
  {:else}
    <div class="empty-state"><span class="es-ic"><MessageSquareOff size={18} /></span><b>No comments</b><p>Nobody has commented yet, or comments are off.</p></div>
  {/if}
  {#if st.list.length && st.error}<div class="small err">{st.error}</div>{/if}
  <div class="row foot">
    {#if st.more && st.list.length}<button class="btn sm" disabled={st.busy} onclick={() => loadComments(project, st.next)}>{st.busy ? 'Loading…' : 'Show more'}</button>{/if}
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
  /* The blue mixed toward the text colour: readable on the light tint too. */
  .maker { color: color-mix(in srgb, var(--blue) 60%, var(--text)); background: var(--blue-dim); flex: none; }
  .hd .tiny { flex: none; }
  .foot { gap: 8px; margin-top: 8px; align-items: center; }
  .sp { flex: 1; }
  .err { color: var(--red); }
</style>
