<script>
  // Versions and changelogs, as in Hopper desktop: every version after the
  // installed one up to the update on offer, each with its changelog
  // (newest first, the installed one last), then the other versions to
  // update or roll back to. One view for the dialog and the side panel.
  import { X, Download, RotateCcw, Settings2, Lock, LockOpen, ChevronRight, ChevronDown } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { md } from './lib/md.js';
  import { installVersion, resetConfig, setLocked } from './lib/modops.js';

  let { server, mod, dialog = false, onChanged = () => {}, onClose = () => {} } = $props();
  let info = $state(null);
  let error = $state('');
  let busy = $state('');
  let open = $state({});

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));
  const key = $derived(mod.file);
  const when = (d) => (d ? new Date(d).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' }) : '');
  const stem = (f) => (f || '').replace(/\.jar(\.disabled)?$/i, '');

  async function load() {
    info = null;
    error = '';
    open = {};
    if (!mod.source) return;
    try {
      info = await api(`${base}/mods/changelogs?file=${encodeURIComponent(mod.file)}`);
    } catch (e) {
      error = e.message;
    }
  }

  async function other(o) {
    if (open[o.file_id] !== undefined) {
      delete open[o.file_id];
      return;
    }
    open[o.file_id] = null;
    try {
      open[o.file_id] = (await api(`${base}/mods/changelog?project=${info.project_id}&file=${o.file_id}`)).markdown || '';
    } catch (e) {
      open[o.file_id] = `!${e.message}`;
    }
  }

  async function install(fileId, label) {
    if (await installVersion(base, mod, fileId, label, alive, (s) => (busy = s))) {
      onChanged(true);
      if (dialog) onClose();
      else load();
    }
  }

  async function unlock() {
    if (await setLocked(base, [mod], false)) {
      info.locked = false;
      onChanged(false);
    }
  }

  $effect(() => {
    key;
    load();
  });
</script>

{#snippet body()}
  {#if dialog}
    <div class="head row">
      {#if mod.icon}<img src={mod.icon} alt="" width="40" height="40" />{:else}<span class="ph"></span>{/if}
      <div class="grow"><b>{mod.name}</b><div class="tiny muted ell">{info?.to ? `${stem(info.from)}  →  ${stem(info.to)}` : `Installed: ${stem(mod.file)}`}</div></div>
    </div>
  {/if}
  {#if info?.locked}
    <div class="locknote row small"><Lock size={14} /><span class="grow">Locked: Hopper keeps this version. Unlock it to update or switch.</span><button class="btn sm" onclick={unlock}><LockOpen size={13} />Unlock</button></div>
  {/if}
  {#if busy}<div class="busy small"><span class="spin"></span>{busy}</div>{/if}
  {#if !mod.source}
    <div class="small muted">Hopper doesn't know where this jar is from yet, so there are no versions or changelogs. Identify looks it up on CurseForge and Modrinth.</div>
  {:else if error}
    <div class="err small">{error}</div>
  {:else if !info}
    <div class="small muted row"><span class="spin b"></span>Fetching versions</div>
  {:else}
    <div class="row status">
      <b class:ok={!info.behind}>{info.behind === 0 ? 'Up to date' : info.behind === 1 ? '1 version behind' : `${info.behind} versions behind · newest first`}</b>
      {#if info.latest && info.behind}<button class="btn sm pri" disabled={info.locked || !!busy} onclick={() => install(info.latest.file_id, 'Updating')}><Download size={13} />Update to {info.latest.version}</button>{/if}
    </div>
    {#each info.entries as e, i (e.name)}
      <div class="vc" class:target={i === 0 && !e.installed} class:cur={e.installed}>
        <div class="row wrap"><b class="vname">{e.name}</b><span class="chip">{e.release}</span>{#if e.installed}<span class="chip running">Installed</span>{/if}</div>
        <div class="row wrap">
          <span class="tiny muted">{when(e.date)}</span><span class="sp"></span>
          {#if !e.installed && e.file_id}<button class="btn sm" disabled={info.locked || !!busy} onclick={() => install(e.file_id, 'Updating')}><Download size={13} />Update to {e.version || e.name}</button>{/if}
          {#if e.installed && e.file_id}<button class="btn sm" disabled={info.locked || !!busy} title="Download this version again in place of the installed file" onclick={() => install(e.file_id, 'Reinstalling')}><RotateCcw size={13} />Reinstall</button>
            <button class="btn sm ghost" title="Move this mod's config files to backups so it writes fresh defaults" onclick={() => resetConfig(base, mod)}><Settings2 size={13} />Reset config</button>{/if}
        </div>
        {#if e.same_as}<div class="small muted"><i>Same notes as {e.same_as}</i></div>
        {:else if e.markdown.trim()}<div class="notes small">{@html md(e.markdown)}</div>
        {:else}<div class="small muted">No changelog for this version.</div>{/if}
      </div>
    {/each}
    {#if info.others.length}<div class="lbl">Other versions</div>{/if}
    {#each info.others as o (o.file_id)}
      <div class="oth">
        <div class="row">
          <button class="row grow ohead" onclick={() => other(o)}>{#if open[o.file_id] !== undefined}<ChevronDown size={13} />{:else}<ChevronRight size={13} />{/if}<b class="vname" title={o.name}>{o.name}</b><span class="chip">{o.release}</span></button>
          <span class="tiny muted nowrap">{when(o.date)}</span>
          <button class="btn sm" disabled={info.locked || !!busy} onclick={() => install(o.file_id, o.older ? 'Rolling back' : 'Updating')}>{o.older ? 'Roll back to' : 'Update to'} {o.version || o.name}</button>
        </div>
        {#if open[o.file_id] === null}<div class="small muted row"><span class="spin b"></span>Loading the changelog</div>
        {:else if typeof open[o.file_id] === 'string'}
          {#if open[o.file_id].startsWith('!')}<div class="err small">{open[o.file_id].slice(1)}</div>
          {:else if open[o.file_id].trim()}<div class="notes small">{@html md(open[o.file_id])}</div>
          {:else}<div class="small muted">No changelog for this version.</div>{/if}
        {/if}
      </div>
    {/each}
  {/if}
{/snippet}

{#if dialog}
  <div class="back" role="presentation" onclick={(e) => e.target === e.currentTarget && !busy && onClose()}>
    <div class="box card col">
      <h3 class="row">Versions and changelogs<span class="sp"></span><button class="btn sm ghost icon" title="Close" disabled={!!busy} onclick={onClose}><X size={14} /></button></h3>
      <div class="scroll col">{@render body()}</div>
    </div>
  </div>
{:else}
  <div class="col inl">{@render body()}</div>
{/if}

<style>
  .back { position: fixed; inset: 0; background: #0008; display: flex; justify-content: center; align-items: flex-start; padding: 64px 16px 16px; z-index: 60; }
  .box { width: min(680px, 100%); max-height: calc(100vh - 80px); gap: 10px; box-shadow: 0 24px 60px #000c; }
  .box h3 { margin: 0; font-size: 15px; }
  .scroll { overflow: auto; gap: 10px; min-height: 0; padding-right: 2px; }
  .inl { gap: 8px; }
  .col { display: flex; flex-direction: column; }
  .head img, .ph { width: 40px; height: 40px; border-radius: 8px; background: var(--raise); flex: none; }
  .grow { flex: 1; min-width: 0; }
  .wrap { flex-wrap: wrap; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .nowrap { white-space: nowrap; }
  .status b { color: var(--acc); font-size: 13px; }
  .status b.ok { color: var(--green); }
  .vc { display: flex; flex-direction: column; gap: 6px; border: 1px solid var(--line); border-radius: 10px; padding: 10px 12px; }
  .vc.target { border-color: var(--acc); }
  .vc.cur { background: #ffffff07; }
  .vname { overflow-wrap: anywhere; min-width: 0; }
  .oth { background: #ffffff07; border-radius: 10px; padding: 7px 10px; display: flex; flex-direction: column; gap: 6px; }
  .ohead { gap: 6px; text-align: left; font-size: 12.5px; }
  .lbl { font-size: 11px; color: var(--muted); text-transform: uppercase; letter-spacing: 0.06em; padding-top: 4px; }
  .locknote { gap: 8px; background: var(--raise); border-radius: 8px; padding: 8px 10px; }
  .notes { background: var(--base); border: 1px solid var(--line); border-radius: 6px; padding: 8px 10px; max-height: 320px; overflow: auto; overflow-wrap: anywhere; }
  .notes :global(ul) { margin: 4px 0 4px 18px; }
  .notes :global(h1), .notes :global(h2), .notes :global(h3) { font-size: 13px; margin: 6px 0 2px; }
  .notes :global(a) { color: var(--blue); }
  .notes :global(img) { max-width: 100%; }
  .icon { padding: 3px 5px; }
  .busy { display: flex; align-items: center; gap: 8px; color: var(--gold); }
  .spin { width: 12px; height: 12px; border: 2px solid var(--gold); border-right-color: transparent; border-radius: 50%; animation: s 0.8s linear infinite; flex: none; }
  .spin.b { border-color: var(--blue); border-right-color: transparent; }
  @keyframes s { to { transform: rotate(360deg); } }
</style>
