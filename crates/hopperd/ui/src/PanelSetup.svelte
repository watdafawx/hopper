<script>
  // The first thing the owner sees on a panel without a panel folder, in one
  // screen like Hopper desktop's first run: what the panel found (the
  // servers it already lists) and one "Use these" button that keeps them
  // and puts the panel's data next to them. The folder picker sits behind
  // "Change…"; with nothing found it is open from the start.
  import { Check, FolderCog, Server } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { restartPanel } from './lib/restart.js';
  import FolderPicker from './FolderPicker.svelte';

  let { onLater } = $props();
  let start = $state('');
  let existing = $state(false);
  /** The servers the panel lists now; null while loading. */
  let found = $state(null);
  /** The folder the picker is in, and how many servers are right inside it. */
  let current = $state('');
  let servers = $state(0);
  /** A folder typed in that doesn't exist yet. */
  let made = $state('');
  /** The folder picker is open. */
  let changing = $state(false);
  let busy = $state('');
  let error = $state('');

  const target = $derived(made || current);
  const name = $derived(target.split(/[\\/]/).filter(Boolean).pop() || target);
  // A drive root holds everything on it: not a panel folder.
  const isDrive = $derived(/^[A-Za-z]:\\?$/.test(target) || target === '/');
  const anything = $derived(!!found?.length);
  /** The servers folder the panel uses now. */
  let serversRoot = $state('');
  // `p` is `dir` or inside it (Windows paths ignore case).
  function within(dir, p) {
    const trim = (s) => s.replace(/[\\/]+$/, '');
    let [a, b] = [trim(dir), trim(p)];
    if (/^[A-Za-z]:/.test(a)) [a, b] = [a.toLowerCase(), b.toLowerCase()];
    return b === a || b.startsWith(a + '\\') || b.startsWith(a + '/');
  }
  // Where new servers go once this folder is used: a servers folder already
  // inside it stays; otherwise its `servers` folder (`home::set`).
  const newIn = $derived(serversRoot && within(target, serversRoot) ? serversRoot : `${target}\\servers`);
  const picking = $derived(changing || (found !== null && !anything));

  // Suggest the folder the servers are in now (its parent), if any.
  api('/settings/servers-folder').then((r) => {
    if (r.path) {
      serversRoot = r.path;
      const p = r.path.replace(/[\\/]+$/, '');
      start = p.slice(0, Math.max(p.lastIndexOf('\\'), p.lastIndexOf('/'))) || p;
    }
    existing = (r.extra?.length || 0) > 0 || !!r.path;
  }, () => {});
  api('/servers').then((l) => (found = Array.isArray(l) ? l : []), () => (found = []));

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
    <div class="head">
      <span class="badge">{#if anything}<Check size={18} />{:else}<FolderCog size={18} />{/if}</span>
      <div class="col">
        {#if anything}
          <h2>Here's what we found</h2>
          <p class="small subtle">Hopper Server is set up from what's on this PC. Use it, or change anything first.</p>
        {:else}
          <h2>Welcome to Hopper Server</h2>
          <p class="small subtle">No servers here yet, so you start fresh. Pick the folder Hopper Server keeps everything in.</p>
        {/if}
      </div>
    </div>

    {#if anything}
      <div class="tiny muted label">Your servers · shown on the panel, left where they are</div>
      <div class="list" class:long={found.length > 5}>
        {#each found as s (s.id)}
          <div class="srv"><span class="tile"><Server size={14} /></span><span class="ell">{s.name}</span></div>
        {/each}
      </div>
    {/if}

    <div class="tiny muted label">{anything ? 'Everything else goes in' : 'Your panel folder'}</div>
    {#if !picking}
      <div class="folder">
        <div class="input grow" title={target}><span class="mono small ell path">{target || '…'}</span></div>
        <button class="btn" disabled={!!busy} onclick={() => (changing = true)}>Change…</button>
      </div>
    {/if}
    <!-- Kept mounted while closed: it reports the folder the button uses. -->
    <div class:hidden={!picking}>
      <FolderPicker {start} allowNew hideButton onPath={(p, entries) => { current = p; made = ''; servers = entries.filter((e) => e.server).length; }} onPick={(p) => (made = p)} />
    </div>
    <div class="tiny muted wrapany">
      {#if made}A new folder: new servers go in <span class="mono">{made}\servers</span>.
      {:else if isDrive || !current}Open the folder to use, or type a path that doesn't exist yet to make one, like E:\Minecraft.
      {:else if picking && servers}The {servers} server{servers === 1 ? '' : 's'} in it show up on the panel, and new servers go here too.
      {:else}Backups, uploads and the panel's own data (logins, certificates) go here, and new servers go in <span class="mono">{newIn}</span>.{/if}
      The panel never shows anything outside it.
    </div>
    {#if error}<div class="err small">{error}</div>{/if}
    {#if busy}<div class="busy small"><span class="spinner"></span>{busy}</div>{/if}
    <div class="foot">
      {#if existing}<button class="btn ghost" disabled={!!busy} onclick={onLater} title="Asks again the next time you open the panel.">Skip</button>{/if}
      <span class="tiny muted grow right">The panel restarts to move its data in; running servers keep running.</span>
      <button class="btn pri" disabled={!target || isDrive || !!busy} onclick={use}>
        {#if anything && !made && !changing}Use these{:else if target && !isDrive}Use {name} and restart{:else}Use this folder{/if}
      </button>
    </div>
  </div>
</div>

<style>
  .wrap { height: 100%; display: flex; justify-content: center; align-items: flex-start; padding: 8vh 16px 16px; overflow: auto; }
  .box { width: min(620px, 100%); display: flex; flex-direction: column; gap: 10px; }
  .head { display: flex; gap: 12px; align-items: flex-start; margin-bottom: 4px; }
  .badge { flex: none; width: 36px; height: 36px; display: grid; place-items: center; border-radius: var(--radius); background: var(--acc-dim); color: var(--ink-acc); }
  .col { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  h2 { margin: 0; font-size: 17px; }
  p { margin: 0; }
  .label { margin-top: 4px; }
  .list { display: flex; flex-direction: column; border: 1px solid var(--line); border-radius: var(--radius); padding: 4px; }
  .list.long { max-height: 220px; overflow: auto; }
  .srv { display: flex; align-items: center; gap: 10px; min-width: 0; padding: 6px 8px; border-radius: var(--radius-sm); }
  .tile { flex: none; width: 26px; height: 26px; display: grid; place-items: center; border-radius: var(--radius-sm); background: var(--green-dim); color: var(--ink-green); }
  .folder { display: flex; align-items: center; gap: 8px; min-width: 0; }
  .grow { flex: 1; min-width: 0; }
  .path { color: var(--text); font-weight: 400; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .hidden { display: none; }
  /* A long path breaks anywhere instead of widening a narrow page. */
  .wrapany { overflow-wrap: anywhere; }
  .foot { display: flex; align-items: center; gap: 8px; margin-top: 4px; }
  .right { text-align: right; }
  .busy { display: flex; align-items: center; gap: 8px; color: var(--gold); }
</style>
