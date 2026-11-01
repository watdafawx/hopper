<script>
  // The peek's page for a mod from GitHub or a link: stars, last release
  // and license, and its README (or what's known about a Maven artifact,
  // Jenkins job or link). Never CurseForge or Modrinth.
  import { Code, Link, Star, RefreshCw, BookOpen } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { count, ago } from './lib/browse.js';
  import BrowseRich from './BrowseRich.svelte';

  let { server, mod } = $props();
  let page = $state(null);
  let err = $state('');

  $effect(() => {
    const file = mod.file;
    page = null;
    err = '';
    api(`/servers/${encodeURIComponent(server.id)}/mods/source/page?file=${encodeURIComponent(file)}`)
      .then((p) => { if (mod.file === file) page = p; })
      .catch((e) => { if (mod.file === file) err = e.message || String(e); });
  });
</script>

{#if err}
  <div class="small bad">{err}</div>
{:else if !page}
  <div class="small muted row"><span class="spinner"></span>Loading its page…</div>
{:else}
  <div class="facts row wrap tiny">
    <span class="row">{#if page.kind === 'GitHub'}<Code size={12} />{:else}<Link size={12} />{/if}{page.kind}</span>
    {#if page.stars != null}<span class="row"><Star size={12} />{count(page.stars)} stars</span>{/if}
    {#if page.released}<span class="row ok"><RefreshCw size={12} />Released {ago(page.released)}</span>{/if}
    {#if page.license}<span class="row"><BookOpen size={12} />{page.license}</span>{/if}
  </div>
  <div class="desc"><BrowseRich blocks={page.description} empty={page.summary || 'No description.'} /></div>
{/if}

<style>
  .facts { gap: 12px; color: var(--muted); }
  .facts .row { gap: 4px; }
  .ok { color: var(--green); }
  .wrap { flex-wrap: wrap; }
  .bad { color: var(--red); }
  .desc { max-height: 420px; overflow: auto; border-top: 1px solid var(--line); padding-top: 8px; }
</style>
