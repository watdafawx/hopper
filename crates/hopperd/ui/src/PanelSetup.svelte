<script>
  // The last setup step (and, on an older panel, the first thing the owner
  // sees): the panel folder. Servers and the panel's own data live in it,
  // and the panel never shows anything outside it. One button: it uses the
  // folder the picker is in (or a new one typed in).
  import { FolderCog } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { restartPanel } from './lib/restart.js';
  import FolderPicker from './FolderPicker.svelte';

  let { onLater } = $props();
  let start = $state('');
  let existing = $state(false);
  /** The folder the picker is in, and how many servers are right inside it. */
  let current = $state('');
  let servers = $state(0);
  /** A folder typed in that doesn't exist yet. */
  let made = $state('');
  let busy = $state('');
  let error = $state('');

  const target = $derived(made || current);
  const name = $derived(target.split(/[\\/]/).filter(Boolean).pop() || target);
  // A drive root holds everything on it: not a panel folder.
  const isDrive = $derived(/^[A-Za-z]:\\?$/.test(target) || target === '/');

  // Suggest the folder the servers are in now (its parent), if any.
  api('/settings/servers-folder').then((r) => {
    if (r.path) {
      const p = r.path.replace(/[\\/]+$/, '');
      start = p.slice(0, Math.max(p.lastIndexOf('\\'), p.lastIndexOf('/'))) || p;
    }
    existing = (r.extra?.length || 0) > 0 || !!r.path;
  }, () => {});

  async function use() {
    busy = 'Saving';
    error = '';
    try {
      await api('/settings/panel-folder', { method: 'PUT', body: { path: target } });
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
    <FolderPicker {start} allowNew hideButton onPath={(p, entries) => { current = p; made = ''; servers = entries.filter((e) => e.server).length; }} onPick={(p) => (made = p)} />
    <div class="tiny muted">
      {#if made}A new folder: new servers go in <span class="mono">{made}\servers</span>.
      {:else if isDrive || !current}Open the folder to use, or type a path that doesn't exist yet to make one, like E:\Minecraft.
      {:else if servers}The {servers} server{servers === 1 ? '' : 's'} in it show up on the panel, and new servers go here too.
      {:else}New servers go in <span class="mono">{current}\servers</span>.{/if}
      The panel restarts to move its data in; running servers keep running.
    </div>
    {#if error}<div class="err small">{error}</div>{/if}
    {#if busy}<div class="busy small"><span class="spinner"></span>{busy}</div>{/if}
    <div class="row end">
      {#if existing}<button class="btn ghost" disabled={!!busy} onclick={onLater}>Not now</button>{/if}
      <button class="btn pri" disabled={!target || isDrive || !!busy} onclick={use}>{target && !isDrive ? `Use ${name} and restart` : 'Use this folder'}</button>
    </div>
  </div>
</div>

<style>
  .wrap { height: 100%; display: flex; justify-content: center; align-items: flex-start; padding: 8vh 16px 16px; overflow: auto; }
  .box { width: min(620px, 100%); display: flex; flex-direction: column; gap: 10px; }
  h2 { margin: 0; font-size: 17px; display: flex; align-items: center; gap: 8px; }
  p { margin: 0; }
  .end { justify-content: flex-end; gap: 8px; }
  .busy { display: flex; align-items: center; gap: 8px; color: var(--gold); }
</style>
