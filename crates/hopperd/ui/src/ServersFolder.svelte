<script>
  // Settings › Panel folder: the folder everything lives in (moved with
  // Move…), and the servers folder inside it: where new servers go and
  // where the panel finds servers on its own. Picking a folder lists what's in it (server
  // folders, server or client pack zips) to add; the old folder's servers
  // stay listed where they are. Deleted servers wait here 30 days.
  import { FolderOpen, FolderCheck, FileArchive, RotateCcw, Trash2, FolderCog, MoveRight } from '@lucide/svelte';
  import { restartPanel } from './lib/restart.js';
  import { api, post, del } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { confirmIt } from './lib/dialog.svelte.js';
  import { followJob } from './lib/jobs.js';
  import FolderPicker from './FolderPicker.svelte';

  let info = $state(null);
  let picking = $state(false);
  let found = $state(null);
  let skip = $state(new Set());
  let zips = $state(new Set());
  let eula = $state(false);
  let busy = $state('');
  let moving = $state(false);
  let moveTo = $state('');

  async function move() {
    busy = 'Saving';
    try {
      await post('/settings/panel-folder/move', { path: moveTo.trim() });
      await restartPanel((m) => (busy = m));
    } catch (e) {
      say(e.message, true);
      busy = '';
    }
  }

  async function load() {
    try {
      info = await api('/settings/servers-folder');
    } catch (e) {
      say(e.message, true);
    }
  }

  async function pick(p) {
    picking = false;
    busy = 'Looking in the folder';
    try {
      found = await api(`/host/scan?path=${encodeURIComponent(p)}`);
      skip = new Set();
      zips = new Set(found.zips.filter((z) => z.kind && (z.kind !== 'server' || z.loader)).map((z) => z.path));
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = '';
    }
  }

  const flip = (set, k) => {
    const s = new Set(set);
    s.has(k) ? s.delete(k) : s.add(k);
    return s;
  };
  const adding = $derived(found ? found.servers.filter((s) => !s.already && !skip.has(s.path)).length + zips.size : 0);

  async function apply() {
    busy = 'Saving';
    try {
      const r = await api('/settings/servers-folder', { method: 'PUT', body: { path: found.path, skip: [...skip], zips: [...zips], accept_eula: eula } });
      say(`Servers folder is ${found.path}. The panel lists ${r.servers} server${r.servers === 1 ? '' : 's'}.`);
      found = null;
      await load();
      for (const job of r.jobs) {
        busy = 'Making servers from the zips';
        try {
          await followJob(job, (st) => (busy = st.detail || busy));
        } catch (e) {
          say(e.message, true);
        }
      }
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = '';
    }
  }

  async function restore(d) {
    try {
      const r = await api(`/deleted/${encodeURIComponent(d.name)}/restore`, { method: 'PUT' });
      say(`${d.server || d.name} is back.`);
      await load();
      if (r.id) location.hash = `/server/${encodeURIComponent(r.id)}`;
    } catch (e) {
      say(e.message, true);
    }
  }

  async function purge(d) {
    if (!(await confirmIt(`Delete ${d.server || d.name} for good?`, { body: 'Its folder is removed now; this can’t be undone.', ok: 'Delete for good', danger: true }))) return;
    try {
      await del(`/deleted/${encodeURIComponent(d.name)}`);
      await load();
    } catch (e) {
      say(e.message, true);
    }
  }

  const when = (s) => new Date(s * 1000).toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
  load();
</script>

<div class="page">
  <div class="card col">
    <div class="row">
      <FolderCog size={16} />
      <span class="mono grow ell">{info?.home || 'Not set'}</span>
      {#if info?.home}<button class="btn sm" disabled={!!busy} onclick={() => { moving = !moving; moveTo = info.home; }}><MoveRight size={13} />Move…</button>{/if}
    </div>
    <div class="small muted">Servers and the panel's own data live here. The panel never shows anything outside it.</div>
    {#if moving}
      <div class="row">
        <div class="input grow"><input class="mono" bind:value={moveTo} spellcheck="false" /></div>
        <button class="btn sm ghost" onclick={() => (moving = false)}>Cancel</button>
        <button class="btn sm pri" disabled={!moveTo.trim() || moveTo.trim() === info.home || !!busy} onclick={move}>Move and restart</button>
      </div>
      <div class="tiny muted">The whole folder (servers too) moves when the panel restarts. Same drive only; stop the servers in it first.</div>
    {/if}
    {#if info?.extra?.length}<div class="tiny muted">Outside it, running where they are: {info.extra.join(', ')}. Move them in or remove them from the panel.</div>{/if}
  </div>
  <h2 class="lbl">Servers folder</h2>
  <div class="card col">
    <div class="row">
      <FolderOpen size={16} />
      <span class="mono grow ell">{info?.path || 'Not set'}</span>
      <button class="btn sm" disabled={!!busy} onclick={() => { picking = !picking; found = null; }}>Change…</button>
    </div>
    <div class="small muted">Inside the panel folder: new servers go here, and every server folder in it shows up on its own.</div>
    {#if picking}<FolderPicker start={info?.path || info?.home || ''} pickLabel="Look in this folder" onPick={pick} onCancel={() => (picking = false)} />{/if}
    {#if busy}<div class="busy small"><span class="spinner"></span>{busy}</div>{/if}

    {#if found}
      <h4>In {found.path}</h4>
      <div class="list">
        {#each found.servers as s (s.path)}
          <label class="it" class:dim={s.already}>
            <input type="checkbox" checked={s.already || !skip.has(s.path)} disabled={s.already} onchange={() => (skip = flip(skip, s.path))} />
            <FolderCheck size={14} class="ok" /><b class="ell">{s.name}</b>
            {#if s.loader}<span class="chip">{s.loader}</span>{/if}
            {#if s.managed_by}<span class="chip">Also in {s.managed_by}</span>{/if}
            <span class="sp"></span><span class="tiny muted">{s.already ? 'already here' : 'runs where it is'}</span>
          </label>
        {/each}
        {#each found.zips as z (z.path)}
          <label class="it" class:dim={!z.kind}>
            <input type="checkbox" checked={zips.has(z.path)} disabled={!z.kind || (z.kind === 'server' && !z.loader)} onchange={() => (zips = flip(zips, z.path))} />
            <FileArchive size={14} /><b class="ell">{z.name}</b>
            <span class="chip acc">{z.kind === 'server' ? 'server pack' : z.kind ? `${z.kind} pack` : 'not a pack'}</span>
            {#if z.loader}<span class="chip">{z.mc_version} · {z.loader}</span>{/if}
            <span class="sp"></span><span class="tiny muted">{!z.kind ? z.error || '' : z.kind === 'server' && !z.loader ? 'doesn’t say its loader: use New server › Server pack zip' : 'becomes a server'}</span>
          </label>
        {/each}
        {#each found.other.slice(0, 8) as o (o)}<div class="it dim"><span class="cbx"></span><span class="ell">{o}</span><span class="sp"></span><span class="tiny muted">no server.properties, skipped</span></div>{/each}
        {#if found.other.length > 8}<div class="tiny muted pad">and {found.other.length - 8} more folders that aren't servers</div>{/if}
        {#if !found.servers.length && !found.zips.length}<div class="small muted pad">No servers or packs in it: new servers will go here.</div>{/if}
      </div>
      {#if zips.size}<label class="eula small"><input type="checkbox" bind:checked={eula} /><span>I agree to the <a href="https://aka.ms/MinecraftEULA" target="_blank" rel="noreferrer">Minecraft EULA</a> for the servers made from zips.</span></label>{/if}
      <div class="row end">
        <span class="tiny muted grow">The servers of the old folder stay on the panel, running where they are.</span>
        <button class="btn sm ghost" onclick={() => (found = null)}>Cancel</button>
        <button class="btn sm pri" disabled={!!busy || (zips.size > 0 && !eula)} onclick={apply}>Use this folder{adding ? `, add ${adding}` : ''}</button>
      </div>
    {/if}
  </div>

  {#if info?.deleted?.length}
    <h2 class="lbl">Deleted servers</h2>
    <div class="card col">
      <div class="small muted">Kept 30 days in .hopper-deleted, then removed.</div>
      {#each info.deleted as d (d.name)}
        <div class="row it">
          <b class="ell">{d.server || d.name}</b><span class="tiny muted">deleted {when(d.at)} · goes {when(d.expires)}</span><span class="sp"></span>
          <button class="btn sm" onclick={() => restore(d)}><RotateCcw size={13} />Restore</button>
          <button class="btn sm ghost icon bad" title="Delete for good" onclick={() => purge(d)}><Trash2 size={13} /></button>
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .page { display: flex; flex-direction: column; gap: 12px; }
  /* Section titles like desktop Settings: small, uppercase, muted. */
  .page h2 { margin: 8px 4px 0; }
  .col { display: flex; flex-direction: column; gap: 10px; }
  .grow { flex: 1; min-width: 0; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .list { display: flex; flex-direction: column; border: 1px solid var(--line); border-radius: var(--radius-lg); }
  .it { display: flex; align-items: center; gap: 8px; padding: 7px 10px; border-bottom: 1px solid var(--line); }
  .it:last-child { border: 0; }
  .it.dim { opacity: 0.55; }
  .it :global(.ok) { color: var(--green); }
  .it input, .eula input { accent-color: var(--acc); width: 15px; height: 15px; }
  .cbx { width: 15px; }
  .pad { padding: 6px 10px; }
  .eula { display: flex; gap: 8px; align-items: center; }
  .end { justify-content: flex-end; gap: 8px; }
  .busy { display: flex; align-items: center; gap: 8px; color: var(--gold); }
  .bad { color: var(--red); }
</style>
