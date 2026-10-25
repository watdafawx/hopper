<script>
  // A mod's details, like Hopper desktop's mod peek: versions for this
  // server's Minecraft and loader, changelogs, update or roll back, lock,
  // config reset, remove.
  import { X, ArrowUpCircle, Lock, LockOpen, RotateCcw, Trash2, ExternalLink } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { confirmIt } from './lib/dialog.svelte.js';
  import { followJob } from './lib/jobs.js';
  import { md } from './lib/md.js';

  let { server, mod, onClose, onChanged } = $props();
  let info = $state(null);
  let error = $state('');
  let shownFile = $state('');
  let notes = $state('');
  let notesLoading = $state(false);
  let busy = $state('');

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));
  const key = $derived(mod.file);

  async function load() {
    info = null;
    error = '';
    notes = '';
    shownFile = '';
    if (!mod.source) return;
    try {
      info = await api(`${base}/mods/versions?file=${encodeURIComponent(mod.file)}`);
      const first = info.versions.find((v) => !v.installed) || info.versions[0];
      if (first) showNotes(first);
    } catch (e) {
      error = e.message;
    }
  }

  async function showNotes(v) {
    shownFile = v.id;
    notesLoading = true;
    try {
      notes = (await api(`${base}/mods/changelog?project=${info.project_id}&file=${v.id}`)).markdown || '';
    } catch (e) {
      notes = '';
      say(e.message, true);
    } finally {
      notesLoading = false;
    }
  }

  async function install(v, verb) {
    busy = `${verb} ${mod.name}`;
    try {
      const { job } = await post(`${base}/mods/version`, { file: mod.file, file_id: v.id });
      await followJob(job, (s) => (busy = s.detail || busy));
      say(`${mod.name} is ${v.version || v.name} now.${alive ? ' Restart the server to load it.' : ''} The old jar is in Backups.`);
      onChanged(true);
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = '';
    }
  }

  async function lock() {
    try {
      await post(`${base}/mods/lock`, { file: mod.file, locked: !mod.locked });
      say(mod.locked ? `${mod.name} gets updates again.` : `${mod.name} stays on this version: update checks skip it.`);
      onChanged(false);
    } catch (e) {
      say(e.message, true);
    }
  }

  async function resetConfig() {
    try {
      const { files } = await api(`${base}/mods/config?file=${encodeURIComponent(mod.file)}`);
      if (!files.length) return say(`No config files named after ${mod.name} in config/.`);
      const ok = await confirmIt(`Reset ${mod.name}'s config?`, {
        body: `${files.length === 1 ? 'This file goes' : `These ${files.length} files go`} to .hopper-backups: ${files.join(', ')}. The mod writes fresh ones on the next start.`,
        ok: 'Reset config',
        danger: true,
      });
      if (!ok) return;
      const r = await post(`${base}/mods/config-reset`, { file: mod.file });
      say(`Moved ${r.reset} file${r.reset === 1 ? '' : 's'} to .hopper-backups.${r.restart_needed ? ' Restart the server to apply.' : ''}`);
    } catch (e) {
      say(e.message, true);
    }
  }

  async function remove() {
    if (!(await confirmIt(`Remove ${mod.name}?`, { body: "It's kept in .hopper-backups (Mods › Backups).", ok: 'Remove', danger: true }))) return;
    try {
      await post(`${base}/mods/remove`, { file: mod.file });
      onChanged(true);
      onClose();
    } catch (e) {
      say(e.message, true);
    }
  }

  const when = (d) => (d ? new Date(d).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' }) : '');

  $effect(() => {
    key;
    load();
  });
</script>

<aside class="peek card col">
  <div class="row top">
    {#if mod.icon}<img src={mod.icon} alt="" width="40" height="40" />{:else}<span class="ph"></span>{/if}
    <div class="grow"><div class="nm">{mod.name}</div><div class="tiny muted">{mod.author || mod.source || 'unknown source'}{mod.side ? ` · ${mod.side}` : ''}{mod.from_pack ? ' · from the pack' : ' · added on top'}</div></div>
    <button class="btn sm ghost icon" title="Close" onclick={onClose}><X size={14} /></button>
  </div>
  <div class="row wrap">
    {#if mod.update}<button class="btn sm pri" disabled={!!busy} onclick={() => install({ id: mod.update.file_id, name: mod.update.to, version: mod.update.to }, 'Updating')}><ArrowUpCircle size={13} />Update to {mod.update.to}</button>{/if}
    {#if mod.source}<button class="btn sm" onclick={lock}>{#if mod.locked}<LockOpen size={13} />Unlock{:else}<Lock size={13} />Lock{/if}</button>{/if}
    {#if mod.source}<button class="btn sm ghost" onclick={resetConfig}><RotateCcw size={13} />Reset config…</button>{/if}
    <span class="sp"></span>
    {#if mod.page}<a class="btn sm ghost" href={mod.page} target="_blank" rel="noreferrer"><ExternalLink size={13} />{mod.source === 'modrinth' ? 'Modrinth' : 'CurseForge'}</a>{/if}
    <button class="btn sm ghost icon bad" title="Remove (kept in Backups)" onclick={remove}><Trash2 size={13} /></button>
  </div>
  {#if busy}<div class="busy small"><span class="spin"></span>{busy}</div>{/if}
  {#if mod.locked}<div class="tiny muted">Locked: update checks skip it.</div>{/if}
  <div class="tiny muted mono ell" title={mod.file}>{mod.file}</div>

  {#if !mod.source}
    <div class="small muted">Hopper doesn't know where this jar is from yet, so there are no versions or changelogs. Identify looks it up on CurseForge and Modrinth.</div>
  {:else if error}
    <div class="err small">{error}</div>
  {:else if !info}
    <div class="small muted">Loading versions…</div>
  {:else}
    <div class="lbl">Versions for Minecraft {server.version || ''}</div>
    <div class="vers">
      {#each info.versions as v (v.id)}
        <div class="ver" class:cur={v.installed} class:on={shownFile === v.id}>
          <button class="vbtn" onclick={() => showNotes(v)}><b class="ell" title={v.name}>{v.version || v.name}</b><span class="tiny muted">{when(v.date)}{v.type !== 'release' ? ` · ${v.type}` : ''}</span></button>
          {#if v.installed}<span class="chip running">installed</span>
          {:else}<button class="btn sm" disabled={!!busy} onclick={() => install(v, 'Installing')}>{info.versions.findIndex((x) => x.installed) >= 0 && info.versions.indexOf(v) > info.versions.findIndex((x) => x.installed) ? 'Roll back' : 'Install'}</button>{/if}
        </div>
      {:else}
        <div class="small muted">No files for this Minecraft version and loader.</div>
      {/each}
    </div>
    {#if shownFile}
      <div class="lbl">Changelog</div>
      <div class="notes small">{#if notesLoading}<span class="muted">Loading…</span>{:else if notes.trim()}{@html md(notes)}{:else}<span class="muted">No changelog for this version.</span>{/if}</div>
    {/if}
  {/if}
</aside>

<style>
  .peek { gap: 8px; width: 380px; flex: none; position: sticky; top: 0; max-height: calc(100vh - 170px); overflow: auto; }
  .top img, .ph { width: 40px; height: 40px; border-radius: 8px; background: var(--raise); flex: none; }
  .nm { font-weight: 600; font-size: 14px; }
  .grow { flex: 1; min-width: 0; }
  .wrap { flex-wrap: wrap; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .icon { padding: 3px 5px; }
  .bad { color: var(--red); }
  .vers { display: flex; flex-direction: column; gap: 4px; max-height: 260px; overflow: auto; }
  .ver { display: flex; align-items: center; gap: 8px; padding: 4px 6px; border: 1px solid var(--line); border-radius: 6px; background: var(--base); }
  .ver.cur { border-color: #22c55e55; }
  .ver.on { border-color: #f9731688; }
  .vbtn { flex: 1; min-width: 0; display: flex; flex-direction: column; align-items: flex-start; text-align: left; font-size: 12px; }
  .notes { background: var(--base); border: 1px solid var(--line); border-radius: 6px; padding: 8px 10px; max-height: 320px; overflow: auto; }
  .notes :global(ul) { margin: 4px 0 4px 18px; }
  .notes :global(h1), .notes :global(h2), .notes :global(h3) { font-size: 13px; margin: 6px 0 2px; }
  .notes :global(a) { color: var(--blue); }
  .busy { display: flex; align-items: center; gap: 8px; color: var(--gold); }
  .spin { width: 12px; height: 12px; border: 2px solid var(--gold); border-right-color: transparent; border-radius: 50%; animation: s 0.8s linear infinite; }
  @keyframes s { to { transform: rotate(360deg); } }
  @media (max-width: 1150px) { .peek { position: fixed; right: 12px; top: 64px; bottom: 12px; max-height: none; z-index: 40; box-shadow: 0 20px 60px #000c; } }
</style>
