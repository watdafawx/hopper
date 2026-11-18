<script>
  // An author's side panel, like desktop's: the author's name, how many
  // projects and downloads, a link to their page on the site, and every
  // project they made (most downloaded first). A project that is already
  // in the server opens its mod panel; any other opens its page in the same
  // panel, with a way back to this list.
  import { X, Download, ExternalLink, RefreshCw, Check } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { SITE, TYPES, count, ago, date } from './lib/browse.js';
  import BrowsePack from './BrowsePack.svelte';

  // `project`: any project of the author's (the route finds the author from
  // it); `mods`: the server's mods, to tag the ones it has; `onOpenMod(mod)`:
  // shows an installed mod's own panel.
  let { project, name, source, mods = [], onClose, onOpenMod } = $props();

  let author = $state('loading'); // 'loading', { name, url, projects } or { error }
  let open = $state(null); // a project of the author's opened in this panel: { source, id, type }

  $effect(() => {
    const pid = project;
    author = 'loading';
    open = null;
    api(`/browse/author?project=${encodeURIComponent(pid)}`)
      .then((r) => { if (pid === project) author = r; })
      .catch((e) => { if (pid === project) author = { error: e.message }; });
  });

  const site = $derived(SITE[source] || 'its site');
  const list = $derived(author && author !== 'loading' && !author.error ? author.projects : []);
  const total = $derived(list.reduce((n, x) => n + (x.downloads || 0), 0));
  const have = (r) => mods.find((m) => m.project_id === r.num) || null;

  function pick(r) {
    const m = have(r);
    if (m) return onOpenMod(m);
    // Kinds Browse doesn't show (resource packs, shaders…) open on their site.
    if (!TYPES[r.kind]) return window.open(r.url, '_blank', 'noopener');
    open = { source: r.source, id: r.id, type: r.kind };
  }
</script>

<aside class="peek card col" aria-label="{name} projects">
  {#if open}
    <!-- The project's page, in this panel. -->
    {#key open.id}<BrowsePack source={open.source} id={open.id} type={open.type} backLabel="Back to {name}" onBack={() => (open = null)} />{/key}
  {:else}
    <div class="row top">
      <div class="grow">
        <div class="nm ell">{name}</div>
        <div class="tiny muted">{author === 'loading' ? 'Loading projects' : author.error ? `On ${site}` : `${list.length} ${list.length === 1 ? 'project' : 'projects'} · ${count(total)} downloads`}</div>
      </div>
      {#if author?.url}<a class="btn sm ghost" href={author.url} target="_blank" rel="noreferrer noopener" title="Everything {name} made on {site}" aria-label="Open {name} on {site}"><ExternalLink size={13} /><span class="olb">Open on {site}</span></a>{/if}
      <button class="btn sm ghost icon" title="Close" aria-label="Close" onclick={onClose}><X size={14} /></button>
    </div>
    {#if author === 'loading'}
      <div class="plist">{#each Array(5) as _, i (i)}<div class="skel"><i></i></div>{/each}</div>
    {:else if author.error}
      <div class="small err">{author.error}</div>
    {:else}
      <div class="plist">
        {#each list as r (r.source + r.id)}
          {@const mine = have(r)}
          <button class="prow row-hover" title={r.summary} onclick={() => pick(r)}>
            {#if r.icon}<img src={r.icon} alt="" width="32" height="32" loading="lazy" />{:else}<span class="pico"></span>{/if}
            <span class="grow col">
              <span class="row nmrow"><span class="small nmt ell">{r.name}</span>{#if mine}<span class="chip running"><Check size={11} />Installed</span>{/if}</span>
              {#if r.summary}<span class="tiny muted ell">{r.summary}</span>{/if}
              <span class="meta tiny">
                <span class="chip">{TYPES[r.kind]?.label || r.kind}</span>
                <span class="dl"><Download size={11} />{count(r.downloads)}</span>
                {#if ago(r.updated)}<span class="up" title={date(r.updated)}><RefreshCw size={11} />{ago(r.updated)}</span>{/if}
              </span>
            </span>
          </button>
        {:else}<div class="small muted pad">No projects found.</div>{/each}
      </div>
    {/if}
  {/if}
</aside>

<style>
  .peek { gap: 8px; width: 400px; flex: none; position: sticky; top: 0; max-height: calc(100vh - 170px); overflow: auto; max-width: 100%; container-type: inline-size; }
  .nm { font-weight: 600; font-size: 14px; }
  .grow { flex: 1; min-width: 0; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .icon { padding: 3px 5px; }
  .top { align-items: center; gap: 8px; }
  /* In a narrow panel (a phone) the link keeps its icon and the count stays on one line. */
  @container (max-width: 340px) { .olb { display: none; } }
  .plist { display: flex; flex-direction: column; gap: 1px; }
  .prow { display: flex; align-items: flex-start; gap: 10px; padding: 6px 8px; border-radius: var(--radius); text-align: left; width: 100%; min-width: 0; }
  .prow img, .pico { width: 32px; height: 32px; border-radius: var(--radius); object-fit: cover; background: var(--raise); flex: none; display: block; }
  .prow .col { gap: 1px; }
  .nmrow { gap: 6px; align-items: center; min-width: 0; }
  .nmt { min-width: 0; }
  .meta { display: flex; gap: 4px 10px; flex-wrap: wrap; align-items: center; margin-top: 2px; }
  .meta .dl { color: var(--blue); display: inline-flex; align-items: center; gap: 3px; }
  .meta .up { color: var(--green); display: inline-flex; align-items: center; gap: 3px; }
  .pad { padding: 12px; }
  @media (max-width: 1150px) { .peek { position: fixed; right: 12px; top: 64px; bottom: 12px; max-height: none; z-index: 40; box-shadow: 0 20px 60px #000c; max-width: calc(100vw - 24px); } }
</style>
