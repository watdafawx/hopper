<script>
  // The last setup step (and, on an older panel, the first thing the owner
  // sees): the panel folder. Servers and the panel's own data live in it,
  // and the panel never shows anything outside it.
  import { FolderCog, FolderCheck, FileArchive } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { restartPanel } from './lib/restart.js';
  import FolderPicker from './FolderPicker.svelte';

  let { onLater } = $props();
  let start = $state('');
  let existing = $state(false);
  let picked = $state('');
  let found = $state(null);
  let busy = $state('');
  let error = $state('');

  // Suggest the folder the servers are in now (its parent), if any.
  api('/settings/servers-folder').then((r) => {
    if (r.path) {
      const p = r.path.replace(/[\\/]+$/, '');
      start = p.slice(0, Math.max(p.lastIndexOf('\\'), p.lastIndexOf('/'))) || p;
    }
    existing = (r.extra?.length || 0) > 0 || !!r.path;
  }, () => {});

  async function pick(p) {
    picked = p;
    error = '';
    found = null;
    try {
      found = await api(`/host/scan?path=${encodeURIComponent(p)}`);
    } catch {
      found = null; // a new folder: nothing in it yet
    }
  }

  async function use() {
    busy = 'Saving';
    error = '';
    try {
      await api('/settings/panel-folder', { method: 'PUT', body: { path: picked } });
      await restartPanel((s) => (busy = s));
    } catch (e) {
      error = e.message;
      busy = '';
    }
  }
</script>

<div class="wrap">
  <div class="card box">
    <div class="tiny muted">Setup · Panel folder</div>
    <h2><FolderCog size={18} />Where should Hopper Server keep everything?</h2>
    <p class="small subtle">Servers, backups, uploads and the panel's own data (logins, certificates) go in this folder. The panel never shows anything outside it: making servers, imports and folder pickers stay inside.</p>
    {#if !picked}
      <FolderPicker {start} allowNew pickLabel="Use this folder" onPick={pick} />
      <div class="tiny muted">Type a path that doesn't exist yet to make a new folder, like E:\Minecraft.</div>
    {:else}
      <div class="row chosen"><FolderCheck size={16} /><b class="mono grow ell">{picked}</b><button class="btn sm ghost" disabled={!!busy} onclick={() => (picked = '')}>Change</button></div>
      {#if found && (found.servers.length || found.zips.length)}
        <div class="small subtle">In it: {found.servers.length} server folder{found.servers.length === 1 ? '' : 's'}{found.zips.length ? `, ${found.zips.length} zip${found.zips.length === 1 ? '' : 's'}` : ''}. Pick which ones the panel lists in Settings › Panel folder afterwards.</div>
        <div class="list">
          {#each found.servers.slice(0, 6) as s (s.path)}<div class="it"><FolderCheck size={13} class="ok" />{s.name}{#if s.loader}<span class="chip">{s.loader}</span>{/if}</div>{/each}
          {#each found.zips.slice(0, 4) as z (z.path)}<div class="it"><FileArchive size={13} />{z.name}</div>{/each}
        </div>
      {:else}
        <div class="small subtle">New servers go in <span class="mono">{picked}\servers</span>.</div>
      {/if}
      <div class="small subtle">The panel restarts to move its data in. Running servers keep running.</div>
    {/if}
    {#if error}<div class="err small">{error}</div>{/if}
    {#if busy}<div class="busy small"><span class="spin"></span>{busy}</div>{/if}
    <div class="row end">
      {#if existing}<button class="btn ghost" disabled={!!busy} onclick={onLater}>Not now</button>{/if}
      {#if picked}<button class="btn pri" disabled={!!busy} onclick={use}>Use {picked.split(/[\\/]/).filter(Boolean).pop()} and restart</button>{/if}
    </div>
  </div>
</div>

<style>
  .wrap { height: 100%; display: flex; justify-content: center; align-items: flex-start; padding: 8vh 16px 16px; overflow: auto; }
  .box { width: min(620px, 100%); display: flex; flex-direction: column; gap: 10px; }
  h2 { margin: 0; font-size: 17px; display: flex; align-items: center; gap: 8px; }
  p { margin: 0; }
  .chosen { gap: 8px; border: 1px solid var(--line2); border-radius: 8px; padding: 8px 10px; background: var(--base); }
  .grow { flex: 1; min-width: 0; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .list { display: flex; flex-direction: column; gap: 2px; }
  .it { display: flex; align-items: center; gap: 8px; font-size: 12.5px; padding: 2px 4px; }
  .it :global(.ok) { color: var(--green); }
  .end { justify-content: flex-end; gap: 8px; }
  .busy { display: flex; align-items: center; gap: 8px; color: var(--gold); }
  .spin { width: 12px; height: 12px; border: 2px solid var(--gold); border-right-color: transparent; border-radius: 50%; animation: s 0.8s linear infinite; }
  @keyframes s { to { transform: rotate(360deg); } }
</style>
