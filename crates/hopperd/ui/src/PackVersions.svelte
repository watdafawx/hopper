<script>
  // A server's modpack versions and changelogs, as Hopper desktop's
  // Versions window: how far behind it is, then every version newest first
  // with its changelog. "Update to this" on a newer one and "Switch to
  // this" on an older one open the update window for that version.
  import { X, Download, ChevronDown, ChevronRight, Package, ExternalLink, ArrowRightLeft } from '@lucide/svelte';
  import { untrack } from 'svelte';
  import { api } from './lib/api.js';
  import { ago } from './lib/browse.js';
  import BrowseRich from './BrowseRich.svelte';

  let { server, check, onPick, onClose } = $props();
  const pack = $derived(check.pack);
  const cur = $derived(pack.current);
  const versions = $derived(check.versions?.length ? check.versions : [...(check.newer || []), cur]);
  const newer = $derived(check.newer || []);
  const ix = (v) => versions.findIndex((x) => x.id === v.id);
  const curIx = $derived(ix(cur));

  let logs = $state({});
  let open = $state(untrack(() => ({ [check.newer?.[0]?.id ?? check.pack.current.id]: true })));
  let mrLogs = null;

  async function loadLog(v) {
    if (logs[v.id]) return;
    logs[v.id] = 'loading';
    try {
      if (pack.source === 'modrinth') {
        mrLogs ??= api(`/browse/versions?source=modrinth&id=${encodeURIComponent(pack.project)}`).then((r) => Object.fromEntries((r.versions || []).map((x) => [x.id, x.changelog])));
        logs[v.id] = (await mrLogs)[v.id] || [];
      } else {
        logs[v.id] = (await api(`/browse/changelog?id=${encodeURIComponent(pack.project)}&file=${encodeURIComponent(v.id)}`)).changelog || [];
      }
    } catch (e) {
      logs[v.id] = { error: e.message };
    }
  }
  $effect(() => {
    for (const v of versions) if (open[v.id]) untrack(() => loadLog(v));
  });

  const why = (v) =>
    !pack.updatable ? "Modrinth packs don't publish server packs, so the server can't change version in place."
      : !v.server_files ? 'No server pack for this version.'
        : '';
  const verb = (v) => (curIx >= 0 && ix(v) > curIx ? 'Switch to this' : 'Update to this');
</script>

<div class="back" role="presentation" onclick={(e) => e.target === e.currentTarget && onClose()}>
  <div class="box card col">
    <h3 class="row">Versions and changelogs<span class="sp"></span><button class="btn sm ghost icon" title="Close" onclick={onClose}><X size={14} /></button></h3>
    <div class="row hd">
      {#if pack.icon}<img class="pi" src={pack.icon} alt="" width="40" height="40" />{:else}<span class="pi ph"><Package size={18} /></span>{/if}
      <div class="col g2 grow">
        <b>{pack.name}</b>
        <span class="small subtle ell">{cur.file_name || cur.name}{newer.length ? `  →  ${newer[0].file_name || newer[0].name}` : ''}</span>
      </div>
      {#if pack.page}<a class="btn sm ghost" href={pack.page} target="_blank" rel="noreferrer noopener"><ExternalLink size={13} />Pack page</a>{/if}
    </div>
    <div class="row">
      {#if newer.length}
        <b class="behind">{newer.length} version{newer.length === 1 ? '' : 's'} behind</b>
        <button class="btn sm pri" disabled={!!why(newer[0])} title={why(newer[0])} onclick={() => onPick(newer[0].id)}><Download size={13} />Update to {newer[0].name}</button>
      {:else}<span class="small subtle">Up to date: {server.name} runs the newest version.</span>{/if}
    </div>

    <div class="scroll col">
      {#each versions as v, i (v.id)}
        {@const installed = v.id === cur.id}
        {@const top = i === 0 && !installed}
        <div class="ver" class:top class:installed>
          <div class="row">
            <button class="vh row grow" onclick={() => (open = { ...open, [v.id]: !open[v.id] })}>
              {#if open[v.id]}<ChevronDown size={14} />{:else}<ChevronRight size={14} />{/if}
              <b class="ell">{v.name}</b>
              <span class="chip xs">{v.type ? v.type[0].toUpperCase() + v.type.slice(1) : 'Release'}</span>
              {#if installed}<span class="chip running xs">Installed</span>{/if}
              {#if pack.updatable && !v.server_files}<span class="chip xs" title="The author didn't publish a server pack for this version">No server files</span>{/if}
              <span class="tiny muted">{ago(v.date)}</span>
            </button>
            {#if !installed}
              <button class="btn sm" class:pri={top} disabled={!!why(v)} title={why(v)} onclick={() => onPick(v.id)}>
                {#if verb(v) === 'Switch to this'}<ArrowRightLeft size={13} />{:else}<Download size={13} />{/if}{verb(v)}
              </button>
            {/if}
          </div>
          {#if open[v.id]}
            <div class="notes small">
              {#if logs[v.id] === 'loading' || !logs[v.id]}<span class="muted row"><span class="spin"></span>Loading the changelog</span>
              {:else if logs[v.id].error}<span class="muted">Couldn't load the changelog: {logs[v.id].error}</span>
              {:else}<BrowseRich blocks={logs[v.id]} empty="No changelog for this version." />{/if}
            </div>
          {/if}
        </div>
      {/each}
    </div>
  </div>
</div>

<style>
  .back { position: fixed; inset: 0; background: #0008; display: flex; justify-content: center; align-items: flex-start; padding: 56px 16px 16px; z-index: 60; }
  .box { width: min(820px, 100%); height: calc(100vh - 80px); gap: 12px; box-shadow: 0 24px 60px #000c; }
  .box h3 { margin: 0; font-size: 15px; }
  .hd { gap: 12px; }
  .pi { border-radius: 8px; background: var(--raise); flex: none; display: grid; place-items: center; width: 40px; height: 40px; }
  .g2 { gap: 2px; min-width: 0; }
  .grow { flex: 1; min-width: 0; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .behind { color: var(--acc); }
  .scroll { overflow: auto; min-height: 0; flex: 1; gap: 8px; padding-right: 4px; }
  .ver { border: 1px solid var(--line); border-radius: 10px; padding: 8px 12px; display: flex; flex-direction: column; gap: 8px; }
  .ver.top { border-color: var(--acc); }
  .ver.installed { background: #ffffff06; }
  .vh { gap: 8px; text-align: left; }
  .vh:hover b { color: var(--acc); }
  .chip.xs { font-size: 10.5px; padding: 0 6px; }
  .notes { padding: 4px 4px 4px 22px; overflow-wrap: anywhere; }
  .spin { width: 11px; height: 11px; border: 2px solid currentColor; border-right-color: transparent; border-radius: 99px; animation: rot .8s linear infinite; flex: none; }
  @keyframes rot { to { transform: rotate(360deg); } }
  .icon { padding: 3px 5px; }
</style>
