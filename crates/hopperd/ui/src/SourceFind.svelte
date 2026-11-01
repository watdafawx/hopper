<script>
  // Find sources for N unknown mods, as on desktop: each unknown jar with
  // the GitHub repo its own files link to, ticked when there is one, and
  // followed in one go. The jars stay as they are.
  import { X } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { followJob } from './lib/jobs.js';
  import { dialog } from './lib/dialog.svelte.js';

  let { server, onClose, onChanged = () => {} } = $props();
  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  let rows = $state(null);
  let error = $state('');
  let busy = $state('');

  async function load() {
    try {
      rows = (await api(`${base}/mods/source/unknown`)).mods.map((m) => ({ ...m, on: !!m.link }));
    } catch (e) {
      error = e.message;
      rows = [];
    }
  }

  const picked = $derived((rows || []).filter((r) => r.on && r.link));

  async function follow() {
    busy = 'Finding sources';
    try {
      const { job } = await post(`${base}/mods/source/adopt-many`, { items: picked.map((r) => ({ file: r.file_name, link: r.link })) });
      const r = await followJob(job, (st) => (busy = st.detail || busy));
      say(`${r.added?.length ?? picked.length} mods follow a source now. Later releases show in Updates.`);
      onChanged(true);
      onClose();
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = '';
    }
  }

  load();
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && !dialog.open && onClose()} />

<div class="dialog-back" role="presentation" onclick={(e) => e.target === e.currentTarget && onClose()}>
  <div class="box popover col" role="dialog" aria-label="Find sources for unknown mods">
    <h3 class="row">Find sources for unknown mods<span class="sp"></span><button class="btn sm ghost icon" title="Close" onclick={onClose}><X size={14} /></button></h3>
    {#if rows === null}
      <div class="skel"></div><div class="skel"></div><div class="skel"></div>
    {:else if error}
      <div class="small err">{error}</div>
    {:else if !rows.length}
      <div class="small subtle">Every mod here is from CurseForge, Modrinth or a source already.</div>
    {:else}
      <div class="small subtle">{rows.filter((r) => r.link).length} of {rows.length} jars link to their GitHub repo in their own files. Tick the ones to follow; each jar stays as it is, and later releases show in Updates.</div>
      <div class="list">
        {#each rows as r (r.file_name)}
          <label class="it row-hover" class:dim={!r.link}>
            <input type="checkbox" bind:checked={r.on} disabled={!r.link} />
            <span class="col grow min0"><span class="ell">{r.name}</span><span class="tiny muted ell">{r.file_name}</span></span>
            <span class="tiny ell repo">{r.link ? r.link.replace('https://github.com/', '') : 'No link in the jar'}</span>
          </label>
        {/each}
      </div>
    {/if}
    <div class="row end">
      {#if busy}<span class="tiny muted ell">{busy}</span>{/if}
      <button class="btn sm" onclick={onClose}>Cancel</button>
      <button class="btn sm pri" disabled={!picked.length || !!busy} onclick={follow}>{picked.length === 1 ? 'Follow 1 source' : 'Follow sources'}</button>
    </div>
  </div>
</div>

<style>
  .box { width: min(600px, 100%); max-height: calc(100vh - var(--dialog-top) - 16px); gap: 12px; padding: 18px; }
  .box h3 { margin: 0; font-size: 15px; font-weight: 600; display: flex; align-items: center; gap: 8px; }
  .list { overflow: auto; display: flex; flex-direction: column; gap: 2px; min-height: 0; max-height: 360px; }
  .it { display: flex; align-items: center; gap: 10px; padding: 6px 8px; border-radius: var(--radius); font-size: 12.5px; }
  .dim { opacity: .6; }
  .grow { flex: 1; }
  .min0 { min-width: 0; }
  .repo { max-width: 220px; }
  .end { justify-content: flex-end; gap: 8px; }
  .err { color: var(--red, #f87171); }
  .skel { height: 34px; border-radius: var(--radius); background: rgba(255,255,255,.05); }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .icon { padding: 3px 5px; }
</style>
