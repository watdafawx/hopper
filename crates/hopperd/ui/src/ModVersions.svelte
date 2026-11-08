<script>
  // Versions and changelogs, as in Hopper desktop: every version after the
  // installed one up to the update on offer, each with its changelog
  // (newest first, the installed one last), then the other versions to
  // update or roll back to. One view for the dialog and the side panel.
  import { untrack } from 'svelte';
  import { X, Download, RotateCcw, Settings2, Lock, LockOpen, ChevronRight, ChevronDown } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import Changelog from './lib/Changelog.svelte';
  import VersionCard from './lib/VersionCard.svelte';
  import FilesChanges from './lib/FilesChanges.svelte';
  import { dialog as question } from './lib/dialog.svelte.js';
  import { installVersion, resetConfig, setLocked } from './lib/modops.js';

  let { server, mod, dialog = false, onChanged = () => {}, onClose = () => {} } = $props();
  let info = $state(null);
  let error = $state('');
  let busy = $state('');
  let open = $state({});
  let earlierOpen = $state({});
  // "Other versions" cards: what a file's own changelog repeats of older releases.
  let otherEarlier = $state({});

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));
  const key = $derived(mod.file);
  const when = (d) => (d ? new Date(d).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' }) : '');
  // At most a few versions of one list compare their files at once.
  const scanning = $derived.by(() => {
    let n = 0;
    const m = {};
    for (const e of info?.entries || []) if (!e.same_as && !e.markdown?.trim() && e.file_id) m[e.file_id] = n++;
    return m;
  });
  const stem = (f) => (f || '').replace(/\.jar(\.disabled)?$/i, '');

  async function load() {
    info = null;
    error = '';
    open = {};
    earlierOpen = {};
    otherEarlier = {};
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
      const r = await api(`${base}/mods/changelog?project=${info.project_id}&file=${o.file_id}`);
      open[o.file_id] = r.markdown || '';
      if (r.earlier_count) otherEarlier[o.file_id] = { text: r.earlier, count: r.earlier_count };
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

  // Again only for another mod: the list hands over new objects on every
  // reload, which mustn't fetch the versions again or close what's open.
  $effect(() => {
    key;
    base;
    untrack(load);
  });
</script>

{#snippet log(md)}
  {#if md?.trim()}<div class="notes"><Changelog markdown={md} /></div>{:else}<Changelog markdown="" />{/if}
{/snippet}

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
  {#if busy}<div class="busy small"><span class="spinner"></span>{busy}</div>{/if}
  {#if !mod.source}
    <div class="small muted">Hopper doesn't know where this jar is from yet, so there are no versions or changelogs. Identify looks it up on CurseForge and Modrinth.</div>
  {:else if error}
    <div class="err small">{error}</div>
  {:else if !info}
    <div class="small muted row"><span class="spinner blue"></span>Fetching versions</div>
  {:else}
    <div class="row status">
      <b class:ok={!info.behind}>{info.behind === 0 ? 'Up to date' : info.behind === 1 ? '1 version behind' : `${info.behind} versions behind · newest first`}</b>
      {#if info.latest && info.behind}<button class="btn sm pri" disabled={info.locked || !!busy} onclick={() => install(info.latest.file_id, 'Updating')}><Download size={13} />Update to {info.latest.version}</button>{/if}
    </div>
    {#each info.entries as e, i (`${i}:${e.name}`)}
      <VersionCard name={e.name} release={e.release} date={when(e.date)} installed={e.installed} target={i === 0 && !e.installed}>
        {#snippet actions()}
          {#if !e.installed && e.file_id}<button class="btn sm" disabled={info.locked || !!busy} onclick={() => install(e.file_id, 'Updating')}><Download size={13} />Update to {e.version || e.name}</button>{/if}
          {#if e.installed && e.file_id}<button class="btn sm" disabled={info.locked || !!busy} title="Download this version again in place of the installed file" onclick={() => install(e.file_id, 'Reinstalling')}><RotateCcw size={13} />Reinstall</button>
            <button class="btn sm ghost" title="Move this mod's config files to backups so it writes fresh defaults" onclick={() => resetConfig(base, mod)}><Settings2 size={13} />Reset config</button>{/if}
        {/snippet}
        {#if e.same_as}<div class="small muted"><i>Same notes as {e.same_as}</i></div>
        {:else if !e.markdown?.trim() && e.file_id}<FilesChanges {base} file={mod.file} project={info.project_id} version={e.file_id} enabled={info.files_changes} scan={scanning[e.file_id] < 5} offer={scanning[e.file_id] === 0} />
        {:else}{@render log(e.markdown)}{/if}
        {#if e.earlier_count && !e.same_as}
          <button class="row ohead small muted" onclick={() => (earlierOpen[e.name] = !earlierOpen[e.name])}>{#if earlierOpen[e.name]}<ChevronDown size={13} />{:else}<ChevronRight size={13} />{/if}Earlier versions ({e.earlier_count})</button>
          {#if earlierOpen[e.name]}<div class="notes"><Changelog markdown={e.earlier} /></div>{/if}
        {/if}
      </VersionCard>
    {/each}
    {#if info.others.length}<div class="lbl">Other versions</div>{/if}
    {#each info.others as o (o.file_id)}
      <div class="oth">
        <div class="row">
          <button class="row grow ohead" onclick={() => other(o)}>{#if open[o.file_id] !== undefined}<ChevronDown size={13} />{:else}<ChevronRight size={13} />{/if}<b class="vname" title={o.name}>{o.name}</b><span class="chip">{o.release}</span></button>
          <span class="tiny muted nowrap">{when(o.date)}</span>
          <button class="btn sm" disabled={info.locked || !!busy} onclick={() => install(o.file_id, o.older ? 'Rolling back' : 'Updating')}>{o.older ? 'Roll back to' : 'Update to'} {o.version || o.name}</button>
        </div>
        {#if open[o.file_id] === null}<div class="small muted row"><span class="spinner blue"></span>Loading the changelog</div>
        {:else if typeof open[o.file_id] === 'string'}
          {#if open[o.file_id].startsWith('!')}<div class="err small">{open[o.file_id].slice(1)}</div>
          {:else if !open[o.file_id].trim()}<FilesChanges {base} file={mod.file} project={info.project_id} version={o.file_id} enabled={info.files_changes} />
          {:else}{@render log(open[o.file_id])}{/if}
          {#if otherEarlier[o.file_id]}
            <button class="row ohead small muted" onclick={() => (earlierOpen['o' + o.file_id] = !earlierOpen['o' + o.file_id])}>{#if earlierOpen['o' + o.file_id]}<ChevronDown size={13} />{:else}<ChevronRight size={13} />{/if}Earlier versions ({otherEarlier[o.file_id].count})</button>
            {#if earlierOpen['o' + o.file_id]}<div class="notes"><Changelog markdown={otherEarlier[o.file_id].text} /></div>{/if}
          {/if}
        {/if}
      </div>
    {/each}
  {/if}
{/snippet}

<svelte:window onkeydown={(e) => dialog && e.key === 'Escape' && !busy && !question.open && onClose()} />

{#if dialog}
  <div class="dialog-back" role="presentation" onclick={(e) => e.target === e.currentTarget && !busy && onClose()}>
    <div class="box popover col">
      <h3 class="row">Versions and changelogs<span class="sp"></span><button class="btn sm ghost icon" title="Close" disabled={!!busy} onclick={onClose}><X size={14} /></button></h3>
      <div class="scroll col">{@render body()}</div>
    </div>
  </div>
{:else}
  <div class="col inl">{@render body()}</div>
{/if}

<style>
  .box { width: min(680px, 100%); max-height: calc(100vh - var(--dialog-top) - 16px); gap: 10px; padding: 18px; }
  .box h3 { margin: 0; font-size: 15px; font-weight: 600; display: flex; align-items: center; gap: 8px; }
  .scroll { overflow: auto; gap: 10px; min-height: 0; padding-right: 2px; }
  .inl { gap: 8px; }
  .col { display: flex; flex-direction: column; }
  .head img, .ph { width: 40px; height: 40px; border-radius: var(--radius); background: var(--raise); flex: none; }
  .grow { flex: 1; min-width: 0; }
  .wrap { flex-wrap: wrap; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .nowrap { white-space: nowrap; }
  .status b { color: var(--acc); font-size: 13px; }
  .status b.ok { color: var(--green); }
  .oth { background: #ffffff07; border-radius: var(--radius-lg); padding: 7px 10px; display: flex; flex-direction: column; gap: 6px; }
  .ohead { gap: 6px; text-align: left; font-size: 12.5px; }
  .lbl { padding-top: 4px; }
  .locknote { gap: 8px; background: var(--raise); border-radius: var(--radius-lg); padding: 8px 10px; }
  .notes { background: var(--base); border: 1px solid var(--line); border-radius: var(--radius); padding: 8px 10px; max-height: 320px; overflow: auto; overflow-wrap: anywhere; }
  .icon { padding: 3px 5px; }
  .busy { display: flex; align-items: center; gap: 8px; color: var(--gold); }
  .spinner.blue { color: var(--blue); }
</style>
