<script>
  // A server's Configs page, like desktop's Configs tab: mods with config
  // files on the left (changed ones first, then all, then files no mod
  // matched), a search across every setting, filters and a side picker on
  // top, and the picked mod's files as settings on the right. Edits wait in
  // the unsaved bar until Save.
  import { onMount } from 'svelte';
  import { Search, X, Funnel, ArrowLeft, SlidersHorizontal, FileQuestion } from '@lucide/svelte';
  import ConfigView from './lib/ConfigView.svelte';
  import ConfigRow from './lib/ConfigRow.svelte';
  import ConfigBar from './lib/ConfigBar.svelte';
  import { api } from './lib/api.js';
  import { cfg, loadIndex, pendingList, eid, SIDES, sideChip, shortRel, fileRow } from './lib/configs.svelte.js';
  import { configOwners } from './lib/files-config.js';
  import { jump } from './lib/files.svelte.js';
  import { rightClear } from './lib/rightclear.js';
  import { say } from './lib/toast.svelte.js';

  let { server, path = '', go } = $props();

  let error = $state('');
  let query = $state('');
  let desc = $state(false);
  let filter = $state('all');
  let side = $state('all');
  let filePick = $state('');
  /** The Mods list's rows (icons, names) for mods the index has no icon for. */
  let mods = $state(new Map());

  const index = $derived(cfg.sid === server.id ? cfg.index : null);
  const sel = $derived(path);

  onMount(() => {
    loadIndex(server.id).catch((e) => (error = e.message));
    configOwners(server.id).then((o) => (mods = o.mods));
  });

  const looks = (m) => {
    const row = mods.get(m.key) || mods.get(`${m.key}.disabled`);
    return { name: row?.name || m.name, icon: m.icon || row?.icon || '' };
  };
  const unsavedRels = $derived(new Set(pendingList().map((x) => x.rel)));
  const sideOk = (rels) => side === 'all' || rels.some((r) => fileRow(r)?.side === side);
  const modRows = $derived(
    (index?.mods || [])
      .filter((m) => sideOk(m.files))
      .map((m) => ({ ...m, ...looks(m), unsaved: m.files.some((r) => unsavedRels.has(r)) }))
      .sort((a, b) => a.name.toLowerCase().localeCompare(b.name.toLowerCase())),
  );
  const changedRows = $derived(modRows.filter((m) => m.changed || m.unsaved));
  const loose = $derived((index?.unmatched || []).filter((r) => sideOk([r])));
  const shownAll = $derived(filter === 'all' ? modRows : modRows.filter((m) => (filter === 'unsaved' ? m.unsaved : m.changed || m.unsaved)));
  const shownLoose = $derived(filter === 'all' ? loose : loose.filter((r) => (filter === 'unsaved' ? unsavedRels.has(r) : fileRow(r)?.changed || unsavedRels.has(r))));
  const counts = $derived({ changed: changedRows.length, unsaved: pendingList().length });

  const selMod = $derived(index?.mods.find((m) => m.key === sel) || null);
  const selLoose = $derived(!selMod && index?.unmatched.includes(sel) ? sel : '');
  const selRels = $derived(selMod ? selMod.files : selLoose ? [selLoose] : []);
  const selLooks = $derived(selMod ? looks(selMod) : null);

  function pick(key) {
    filePick = '';
    go(key);
  }

  // ---- search across every mod -------------------------------------------------------
  let hits = $state(null);
  let searching = $state(false);
  let timer;
  $effect(() => {
    const q = query.trim();
    const d = desc;
    clearTimeout(timer);
    if (!q) {
      hits = null;
      return;
    }
    timer = setTimeout(async () => {
      searching = true;
      try {
        const r = await api(`/servers/${encodeURIComponent(server.id)}/configs/search?q=${encodeURIComponent(q)}&desc=${d ? 1 : 0}&limit=300`);
        if (query.trim() === q) hits = r.hits;
      } catch (e) {
        say(e.message, true);
      } finally {
        searching = false;
      }
    }, 250);
  });
  const hitGroups = $derived.by(() => {
    if (!hits) return [];
    const by = new Map();
    for (const h of hits) {
      if (side !== 'all' && fileRow(h.rel)?.side !== side) continue;
      if (!by.has(h.rel)) by.set(h.rel, []);
      by.get(h.rel).push(h.entry);
    }
    return [...by].map(([rel, entries]) => {
      const m = fileRow(rel)?.owner ? index?.mods.find((x) => x.key === fileRow(rel).owner) : null;
      return { rel, entries, mod: m ? looks(m) : null };
    });
  });

  function editText(rel, line) {
    jump.path = rel;
    jump.line = line || 0;
    jump.text = true;
    location.hash = `/server/${[server.id, 'edit', ...rel.split('/')].map(encodeURIComponent).join('/')}`;
  }
  const initials = (n) => n.replace(/[^A-Za-z0-9 ]/g, '').split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0]).join('').toUpperCase() || '?';
</script>

<div class="cfgp">
  <div class="tools">
    <label class="input find"><Search size={13} /><input use:rightClear placeholder={index ? `Search ${index.settings.toLocaleString()} settings in every mod` : 'Search settings'} bind:value={query} />{#if searching}<span class="spinner"></span>{:else if query}<button class="clr" title="Clear" onclick={() => (query = '')}><X size={12} /></button>{/if}</label>
    <label class="ck small"><input type="checkbox" bind:checked={desc} />Search descriptions</label>
    <div class="row fgroup">
      <span class="glabel"><Funnel size={12} />Filter</span>
      <div class="utabs" role="tablist">
        <button role="tab" aria-selected={filter === 'all'} class:tab-sel={filter === 'all'} onclick={() => (filter = 'all')}>All</button>
        <button role="tab" aria-selected={filter === 'changed'} class:tab-sel={filter === 'changed'} onclick={() => (filter = 'changed')}>Changed {counts.changed}</button>
        <button role="tab" aria-selected={filter === 'unsaved'} class:tab-sel={filter === 'unsaved'} onclick={() => (filter = 'unsaved')}>Unsaved {counts.unsaved}</button>
      </div>
    </div>
    <label class="row small sidepick"><span class="glabel">Side</span>
      <select class="input sidesel" bind:value={side} title="Who reads the file">
        {#each SIDES as [k, label] (k)}<option value={k}>{label}</option>{/each}
      </select>
    </label>
  </div>

  {#if error}
    <div class="empty-state"><span class="es-ic"><SlidersHorizontal size={18} /></span><b>Can't read the configs</b><p class="err">{error}</p></div>
  {:else}
    <div class="body" class:picked={!!selRels.length || !!hits}>
      <nav class="mods" aria-label="Mods with config files">
        {#if !index}
          <div class="skels">{#each [0, 1, 2, 3, 4, 5, 6] as i (i)}<div class="skel"><i></i></div>{/each}</div>
        {:else}
          {#if changedRows.length && filter === 'all'}
            <div class="grp">Changed · {changedRows.length}</div>
            {#each changedRows as m (m.key)}{@render modRow(m)}{/each}
          {/if}
          <div class="grp">{filter === 'all' ? 'All mods' : filter === 'changed' ? 'Changed' : 'Unsaved'} · {shownAll.length}</div>
          {#each shownAll as m (m.key)}{@render modRow(m)}{:else}<div class="small muted pad">None here.</div>{/each}
          {#if shownLoose.length}
            <div class="grp">Not matched to a mod · {shownLoose.length}</div>
            {#each shownLoose as r (r)}
              <button class="mrow row-hover" class:row-sel={sel === r && !hits} onclick={() => pick(r)} title={r}>
                <span class="sic q"><FileQuestion size={12} /></span><span class="ell mono small">{shortRel(r)}</span>
                {#if unsavedRels.has(r)}<span class="c"><span class="udot"></span></span>{:else if fileRow(r)?.changed}<span class="c">{fileRow(r).changed}</span>{/if}
              </button>
            {/each}
          {/if}
        {/if}
      </nav>

      <section class="main">
        {#if hits}
          <div class="phone-back"><button class="btn sm ghost" onclick={() => (query = '')}><ArrowLeft size={13} />All mods</button></div>
          <div class="small muted hh">{hits.length >= 300 ? 'The first 300 settings that match' : `${hits.length} setting${hits.length === 1 ? '' : 's'} match`}{desc ? ', descriptions included' : ''}.</div>
          {#each hitGroups as g (g.rel)}
            {@const sc = sideChip(fileRow(g.rel)?.side)}
            <div class="hg">
              {#if g.mod}{#if g.mod.icon}<img src={g.mod.icon} alt="" width="18" height="18" />{:else}<span class="sic ph">{initials(g.mod.name)}</span>{/if}<b>{g.mod.name}</b>{/if}
              <span class="mono small subtle ell">{g.rel}</span>{#if sc}<span class="chip sc {sc[1]}">{sc[0]}</span>{/if}
            </div>
            {#each g.entries as e (eid(e))}<ConfigRow rel={g.rel} {e} onEditText={editText} />{/each}
          {:else}
            <div class="empty-state"><span class="es-ic"><Search size={18} /></span><b>No settings match</b><p>{desc ? 'Try another word.' : 'Try another word, or tick Search descriptions.'}</p></div>
          {/each}
        {:else if selRels.length}
          <div class="phone-back"><button class="btn sm ghost" onclick={() => pick('')}><ArrowLeft size={13} />All mods</button></div>
          <div class="mh">
            {#if selLooks}
              {#if selLooks.icon}<img src={selLooks.icon} alt="" width="32" height="32" />{:else}<span class="sic big">{initials(selLooks.name)}</span>{/if}
              <div class="grow"><div class="nm">{selLooks.name}</div><div class="tiny muted">{selMod.files.length} config file{selMod.files.length === 1 ? '' : 's'}{selMod.changed ? ` · ${selMod.changed} not at default` : ''}</div></div>
            {:else}
              <span class="sic big q"><FileQuestion size={16} /></span>
              <div class="grow"><div class="nm mono">{shortRel(selLoose)}</div><div class="tiny muted">No mod's name matches this file.</div></div>
            {/if}
          </div>
          {#key sel}<ConfigView {server} rels={selRels} {filter} {side} bind:pick={filePick} onEditText={editText} />{/key}
        {:else if index}
          <div class="empty-state">
            <span class="es-ic"><SlidersHorizontal size={18} /></span>
            <b>{index.files.length ? 'Pick a mod' : 'No config files yet'}</b>
            <p>{index.files.length ? `${index.files.length} config files with ${index.settings.toLocaleString()} settings. Pick a mod on the left, or search them all.` : 'Mods write their config files the first time the server starts.'}</p>
          </div>
        {/if}
      </section>
    </div>
    <ConfigBar {server} />
  {/if}
</div>

{#snippet modRow(m)}
  <button class="mrow row-hover" class:row-sel={sel === m.key && !hits} onclick={() => pick(m.key)} title={m.name}>
    {#if m.icon}<img src={m.icon} alt="" width="20" height="20" loading="lazy" />{:else}<span class="sic ph">{initials(m.name)}</span>{/if}
    <span class="ell">{m.name}</span>
    {#if m.unsaved}<span class="c" title="Not saved yet"><span class="udot"></span></span>{:else if m.changed}<span class="c" title="{m.changed} not at the default the file states">{m.changed}</span>{:else if m.files.length > 1}<span class="c">{m.files.length} files</span>{/if}
  </button>
{/snippet}

<style>
  .cfgp { display: flex; flex-direction: column; flex: 1; min-height: 0; border: 1px solid var(--line); border-radius: var(--radius-lg); background: var(--base); overflow: hidden; }
  .tools { flex: none; display: flex; align-items: center; gap: 10px 14px; flex-wrap: wrap; padding: 10px 12px; border-bottom: 1px solid var(--line); }
  .find { width: 340px; padding: 5px 9px; }
  .find input { flex: 1; min-width: 0; }
  .clr { color: var(--muted); display: grid; place-items: center; }
  .ck { display: inline-flex; align-items: center; gap: 6px; color: var(--subtle); cursor: pointer; }
  .ck input { accent-color: var(--acc); }
  .fgroup { gap: 6px; }
  .sidesel { width: auto; padding: 3px 8px; color: var(--text); outline: none; }
  .body { flex: 1; min-height: 0; display: grid; grid-template-columns: 260px minmax(0, 1fr); }
  .mods { border-right: 1px solid var(--line); overflow: auto; padding: 6px; display: flex; flex-direction: column; gap: 1px; }
  .grp { font-size: 11px; font-weight: 600; color: var(--muted); letter-spacing: .05em; text-transform: uppercase; padding: 10px 10px 4px; }
  .mrow { display: flex; align-items: center; gap: 8px; padding: 5px 10px; border-radius: var(--radius); text-align: left; color: var(--text); min-height: 32px; }
  .mrow img, .mh img { border-radius: var(--radius-sm); flex: none; }
  .sic.ph { background: var(--raise); color: var(--subtle); }
  .sic.q { background: var(--raise); color: var(--muted); }
  .sic.big { width: 32px; height: 32px; font-size: 12px; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .c { margin-left: auto; font-size: 11.5px; color: var(--muted); flex: none; }
  .udot { display: inline-block; width: 7px; height: 7px; border-radius: 50%; background: var(--acc); }
  .pad { padding: 4px 10px; }
  .main { overflow: auto; padding: 10px 12px 12px; min-width: 0; }
  .mh { display: flex; align-items: center; gap: 10px; padding: 2px 4px 10px; }
  .nm { font-weight: 600; font-size: 14px; }
  .grow { flex: 1; min-width: 0; }
  .hh { padding: 0 12px 4px; }
  .hg { display: flex; align-items: center; gap: 8px; padding: 14px 12px 4px; min-width: 0; }
  .hg img { border-radius: var(--radius-sm); flex: none; }
  .sc { height: 18px; font-size: 11px; padding: 0 6px 1px; }
  .skels { display: flex; flex-direction: column; padding-top: 4px; }
  .phone-back { display: none; }
  @media (max-width: 700px) {
    /* The page scrolls as a whole on a phone; the unsaved bar sticks to the bottom. */
    :global(.full:has(> .cfgp)) { display: block; overflow: auto; }
    .cfgp { flex: none; overflow: visible; }
    .main { overflow: visible; }
    .find { width: 100%; }
    .find, .sidesel { min-height: 44px; }
    .body { grid-template-columns: minmax(0, 1fr); }
    .mods { border-right: 0; }
    .mrow { min-height: 44px; }
    /* One pane at a time: the list, or what was picked with a way back. */
    .body.picked .mods { display: none; }
    .body:not(.picked) .main { display: none; }
    .phone-back { display: block; margin-bottom: 6px; }
    .phone-back .btn { min-height: 44px; }
    .main { padding: 8px; }
  }
</style>
