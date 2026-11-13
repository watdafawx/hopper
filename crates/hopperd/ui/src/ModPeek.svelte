<script>
  // A mod's side panel: what it is, its actions (update, ignore updates,
  // reset config, switch site, remove) and the same Versions and changelogs
  // view the dialog shows.
  import { X, TriangleAlert, Download, Lock, LockOpen, Settings2, Trash2, ExternalLink, Shuffle, Search, Link, SlidersHorizontal } from '@lucide/svelte';
  import { browseHash } from './lib/browse.js';
  import ModVersions from './ModVersions.svelte';
  import Comments from './lib/Comments.svelte';
  import { commentsLabel, prefetchComments } from './lib/comments.svelte.js';
  import Issues from './lib/Issues.svelte';
  import { api } from './lib/api.js';
  import SourceAbout from './SourceAbout.svelte';
  import { resetConfig, setLocked, removeMods, switchSource, versionLocked, problems } from './lib/modops.js';

  let { server, mod, onClose, onChanged, onUpdate, onFollow, onConfig = null } = $props();
  let busy = $state('');

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));
  // From GitHub or a link: its own page; CurseForge and Modrinth never asked.
  const linked = $derived(mod.source === 'link');
  const site = $derived(linked ? (mod.src_label === 'GitHub' ? 'GitHub' : '') : mod.source === 'modrinth' ? 'Modrinth' : 'CurseForge');
  const sitePage = $derived(linked ? mod.src_page : mod.page);
  const sideText = { client: 'Client only', server: 'Server only', both: 'Client + server' };
  // Its page in Browse (description, gallery, every file).
  const browse = $derived(mod.source && !linked ? `#${browseHash('mods', server.id)}/${mod.source}/${encodeURIComponent(mod.source === 'modrinth' ? mod.mr_project || '' : mod.project_id)}` : '');
  const vl = $derived(versionLocked(mod));
  // CurseForge's comments (desktop's Comments tab): the first page is asked
  // for in the background, so the tab is ready (and shows its count).
  const hasComments = $derived(mod.source === 'curseforge' && !!mod.project_id);
  $effect(() => {
    if (hasComments) prefetchComments(mod.project_id);
  });
  let tab = $state('versions');
  // Its GitHub issues (desktop's Issues tab): the panel says which repo its
  // tracker is, if it's on GitHub.
  let issuesRepo = $state('');
  $effect(() => {
    const pid = mod.project_id;
    issuesRepo = '';
    if (!pid || !mod.source) return;
    api(`/browse/issues/repo?project=${encodeURIComponent(pid)}`)
      .then((r) => { if (pid === mod.project_id) issuesRepo = r.repo || ''; })
      .catch(() => {});
  });
</script>

<aside class="peek card col">
  <div class="row top">
    {#if mod.icon}<img src={mod.icon} alt="" width="40" height="40" />{:else}<span class="ph"></span>{/if}
    <div class="grow"><div class="nm">{mod.name}</div><div class="tiny muted">{mod.author ? `by ${mod.author}` : mod.source || 'unknown source'}{linked && mod.src_publisher && mod.src_publisher !== mod.author ? ` · ${mod.src_publisher}` : ''}{mod.side ? ` · ${sideText[mod.side] || mod.side}` : ''}</div></div>
    <button class="btn sm ghost icon" title="Close" onclick={onClose}><X size={14} /></button>
  </div>
  <div class="row wrap">
    {#if mod.update && !mod.locked && !vl}<button class="btn sm pri" disabled={!!busy} onclick={() => onUpdate(mod)}><Download size={13} />Update to {mod.update.to}</button>{/if}
    {#if mod.source}<button class="btn sm" onclick={async () => (await setLocked(base, [mod], !mod.locked)) && onChanged(false)}>{#if mod.locked}<LockOpen size={13} />Allow updates{:else}<Lock size={13} />Ignore updates{/if}</button>{/if}
    {#if onConfig}<button class="btn sm" title="This mod's settings" onclick={onConfig}><SlidersHorizontal size={13} />Config</button>{/if}
    {#if mod.source}<button class="btn sm ghost" onclick={() => resetConfig(base, mod)}><Settings2 size={13} />Reset config…</button>{/if}
    <span class="sp"></span>
    {#if browse && (mod.source !== 'modrinth' || mod.mr_project)}<a class="btn sm ghost icon" href={browse} title="Open in Browse"><Search size={13} /></a>{/if}
    {#if sitePage}<a class="btn sm ghost icon" href={sitePage} target="_blank" rel="noreferrer" title={site ? `Open on ${site}` : 'Open link'}><ExternalLink size={13} /></a>{/if}
    {#if mod.source && !linked}<button class="btn sm ghost icon" title="Switch to {mod.source === 'modrinth' ? 'CurseForge' : 'Modrinth'}" disabled={!!busy} onclick={async () => (await switchSource(base, mod, alive, (s) => (busy = s))) && onChanged(true)}><Shuffle size={13} /></button>{/if}
    {#if onFollow}<button class="btn sm ghost icon" title={mod.source === 'link' ? 'Change source…' : 'Follow a source…'} onclick={() => onFollow(mod)}><Link size={13} /></button>{/if}
    <button class="btn sm ghost icon bad" title="Remove (kept in Backups)" onclick={async () => { if (await removeMods(base, [mod], alive, { onUndo: () => onChanged(true) })) { onChanged(true); onClose(); } }}><Trash2 size={13} /></button>
  </div>
  {#if mod.update && !mod.locked && problems(mod.update).length}
    <div class="warnline small row"><TriangleAlert size={13} /><span>{mod.update.to} stops the server loading: {problems(mod.update).join('; ')}</span></div>
  {/if}
  {#if busy}<div class="busy small"><span class="spinner"></span>{busy}</div>{/if}
  <div class="tiny muted mono ell" title={mod.file}>{mod.file}</div>
  {#if linked}<SourceAbout {server} {mod} />{/if}
  {#if hasComments || issuesRepo}
    <div class="utabs" role="tablist">
      <button role="tab" aria-selected={tab === 'versions'} class:tab-sel={tab === 'versions'} onclick={() => (tab = 'versions')}>Versions</button>
      {#if hasComments}<button role="tab" aria-selected={tab === 'comments'} class:tab-sel={tab === 'comments'} onclick={() => (tab = 'comments')}>{commentsLabel(mod.project_id)}</button>{/if}
      {#if issuesRepo}<button role="tab" aria-selected={tab === 'issues'} class:tab-sel={tab === 'issues'} onclick={() => (tab = 'issues')}>Issues</button>{/if}
    </div>
  {/if}
  {#if issuesRepo && tab === 'issues'}
    <Issues project={mod.project_id} repo={issuesRepo} />
  {:else if hasComments && tab === 'comments'}
    <Comments project={mod.project_id} url={mod.page} />
  {:else}
    <ModVersions {server} {mod} onChanged={onChanged} />
  {/if}
</aside>

<style>
  .peek { gap: 8px; width: 400px; flex: none; position: sticky; top: 0; max-height: calc(100vh - 170px); overflow: auto; }
  .top img, .ph { width: 40px; height: 40px; border-radius: var(--radius); background: var(--raise); flex: none; }
  .nm { font-weight: 600; font-size: 14px; }
  .grow { flex: 1; min-width: 0; }
  .wrap { flex-wrap: wrap; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .icon { padding: 3px 5px; }
  .bad { color: var(--red); }
  .warnline { gap: 6px; align-items: flex-start; color: var(--gold); }
  .warnline :global(svg) { flex: none; margin-top: 2px; }
  .busy { display: flex; align-items: center; gap: 8px; color: var(--gold); }
  @media (max-width: 1150px) { .peek { position: fixed; right: 12px; top: 64px; bottom: 12px; max-height: none; z-index: 40; box-shadow: 0 20px 60px #000c; } }
</style>
