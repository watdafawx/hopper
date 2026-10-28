<script>
  // Picks a folder (or a zip, with `zips`) on the machine the panel runs
  // on: drives first, then folders. Server folders are marked.
  import { ArrowUp, Folder, FolderCheck, FileArchive, HardDrive } from '@lucide/svelte';
  import { api } from './lib/api.js';

  let { start = '', zips = false, allowNew = false, pickLabel = 'Use this folder', hideButton = false, onPick, onCancel, onPath = () => {} } = $props();
  let path = $state('');
  let parent = $state(null);
  let entries = $state([]);
  let error = $state('');
  let typed = $state('');
  let loading = $state(false);

  let seq = 0;
  async function open(p) {
    const n = ++seq;
    loading = true;
    error = '';
    try {
      const r = await api(`/host/browse?path=${encodeURIComponent(p || '')}`);
      // A slower answer for a folder already left.
      if (n !== seq) return;
      path = r.path || '';
      parent = r.parent;
      entries = r.entries.filter((e) => e.dir || zips);
      typed = path;
      onPath(path, entries);
    } catch (e) {
      if (n !== seq) return;
      error = e.message;
      onPath('', []);
    } finally {
      if (n === seq) loading = false;
    }
  }

  const size = (n) => (n > 1 << 30 ? `${(n / (1 << 30)).toFixed(1)} GB` : `${Math.max(1, Math.round(n / (1 << 20)))} MB`);

  $effect(() => {
    open(start);
  });
</script>

<div class="fp">
  <form class="row" onsubmit={(e) => { e.preventDefault(); open(typed); }}>
    <button type="button" class="btn sm ghost icon" title="Up" disabled={!path} onclick={() => open(parent || '')}><ArrowUp size={13} /></button>
    <div class="input grow"><input class="mono" bind:value={typed} placeholder="Drives" spellcheck="false" /></div>
  </form>
  <div class="list">
    <!-- The old list stays while the next loads, so a double-click's second click lands on the same row. -->
    {#if loading && !entries.length}{#each [40, 28, 52] as w, i (i)}<div class="skel"><i style:width="{w}%"></i></div>{/each}
    {:else if error}<div class="err small pad">{error}</div>
      {#if allowNew && typed.trim()}<div class="pad"><button class="btn sm" onclick={() => onPick(typed.trim())}>Make a new folder {typed.trim()}</button></div>{/if}
    {:else}
      {#each entries as e (e.path)}
        {#if e.dir}
          <!-- One click opens the folder; a double-click on a server folder picks it (its second click doesn't open it again). -->
          <button class="ent row-hover" ondblclick={() => e.server && onPick(e.path)} onclick={(ev) => ev.detail < 2 && open(e.path)}>
            {#if !path}<HardDrive size={14} />{:else if e.server}<FolderCheck size={14} class="ok" />{:else}<Folder size={14} />{/if}
            <span class="ell">{e.name}</span>{#if e.server}<span class="chip running tiny">server</span>{/if}
          </button>
        {:else}
          <button class="ent row-hover" onclick={() => onPick(e.path)}><FileArchive size={14} /><span class="ell">{e.name}</span><span class="sp"></span><span class="tiny muted">{size(e.size)}</span></button>
        {/if}
      {:else}<div class="small muted pad">{zips ? 'No folders or zips here.' : 'No folders here.'}</div>{/each}
    {/if}
  </div>
  <div class="row">
    <span class="tiny muted ell grow mono">{path}</span>
    {#if onCancel}<button class="btn sm ghost" onclick={onCancel}>Cancel</button>{/if}
    {#if !zips && !hideButton}<button class="btn sm" disabled={!path} onclick={() => onPick(path)}>{pickLabel}</button>{/if}
  </div>
</div>

<style>
  .fp { display: flex; flex-direction: column; gap: 6px; border: 1px solid var(--line2); border-radius: 8px; padding: 8px; background: var(--base); }
  .list { max-height: 240px; overflow: auto; display: flex; flex-direction: column; }
  .ent { display: flex; align-items: center; gap: 8px; padding: 5px 8px; border-radius: 5px; text-align: left; font-size: 12.5px; }
  .ent :global(.ok) { color: var(--green); }
  .grow { flex: 1; min-width: 0; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .pad { padding: 6px 8px; }
  .icon { padding: 3px 5px; }
</style>
