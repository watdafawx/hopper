<script>
  import { followConfigs } from './lib/configs.svelte.js';
  // A server's Configs page, like desktop's Configs tab: mods with config
  // files on the left (changed ones first, then all, then files no mod
  // matched), filters and a side picker on top, and the picked mod's files
  // as settings on the right. Edits wait in the unsaved bar until Save.
  // Search works like the game's config menus: the box on top finds mods by
  // name, the open mod has its own box for its settings, and settings of
  // other mods that mention the words wait in a folded group.
  import { onMount } from 'svelte';
  import { Search, X, Funnel, ArrowLeft, SlidersHorizontal, FileQuestion, Users, Send, ChevronRight, ChevronDown, Info } from '@lucide/svelte';
  import ConfigView from './lib/ConfigView.svelte';
  import ConfigBar from './lib/ConfigBar.svelte';
  import PushPlayers from './lib/PushPlayers.svelte';
  import { ago } from './lib/browse.js';
  import { api } from './lib/api.js';
  import { cfg, loadIndex, loadPlayers, playersDiffer, pendingList, SIDES, shortRel, fileRow } from './lib/configs.svelte.js';
  import { configOwners } from './lib/files-config.js';
  import { marks, searchWords } from './lib/mark.js';
  import { jump } from './lib/files.svelte.js';
  import { rightClear } from './lib/rightclear.js';
  import { say } from './lib/toast.svelte.js';

  let { server, path = '', go } = $props();
  // The live feed hands over a new `server` object every second; effects key on the id.
  const sid = $derived(server.id);
  // A running server reads saved settings when it restarts (as on desktop).
  const running = $derived(['running', 'starting'].includes(server.state));

  let error = $state('');
  let query = $state('');
  /** The open mod's own search. */
  let modQuery = $state('');
  let modInput = $state();
  let othersOpen = $state(false);
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
    loadPlayers(server.id);
    configOwners(server.id).then((o) => (mods = o.mods));
  });

  const looks = (m) => {
    const row = mods.get(m.key) || mods.get(`${m.key}.disabled`);
    return { name: row?.name || m.name, icon: m.icon || row?.icon || '' };
  };
  const unsavedRels = $derived(new Set(pendingList().map((x) => x.rel)));
  const sideOk = (rels) => side === 'all' || rels.some((r) => fileRow(r)?.side === side);
  const words = $derived(searchWords(query));
  // A mod's name, or its jar's file name, has every word.
  const nameHit = (name, key = '') => words.every((w) => `${name} ${key}`.toLowerCase().includes(w));
  const modRows = $derived(
    (index?.mods || [])
      .filter((m) => sideOk(m.files))
      .map((m) => ({ ...m, ...looks(m), unsaved: m.files.some((r) => unsavedRels.has(r)) }))
      .sort((a, b) => a.name.toLowerCase().localeCompare(b.name.toLowerCase())),
  );
  const changedRows = $derived(modRows.filter((m) => m.changed || m.unsaved));
  const loose = $derived((index?.unmatched || []).filter((r) => sideOk([r]) && (!words.length || nameHit(r))));
  const keepRel = (r) => (filter === 'unsaved' ? unsavedRels.has(r) : filter === 'players' ? playersDiffer(r) > 0 : fileRow(r)?.changed || unsavedRels.has(r));
  const named = $derived(words.length ? modRows.filter((m) => nameHit(m.name, m.key)) : modRows);
  const shownAll = $derived(filter === 'all' ? named : named.filter((m) => m.files.some(keepRel)));
  const shownLoose = $derived(filter === 'all' ? loose : loose.filter(keepRel));
  const players = $derived(cfg.sid === server.id ? cfg.players : null);
  let pushing = $state(false);
  const sentAgo = (at) => ago(new Date(at * 1000).toISOString());
  const counts = $derived({ changed: changedRows.length, unsaved: pendingList().length });

  const selMod = $derived(index?.mods.find((m) => m.key === sel) || null);
  const selLoose = $derived(!selMod && index?.unmatched.includes(sel) ? sel : '');
  const selRels = $derived(selMod ? selMod.files : selLoose ? [selLoose] : []);
  const selLooks = $derived(selMod ? looks(selMod) : null);

  /** Opens a mod; `q` puts words in its own search (a mod from the other-mods group). */
  function pick(key, q = '') {
    filePick = '';
    modQuery = q;
    go(key);
  }

  /** Enter in the top search: the first mod listed, with the cursor in its own search. */
  function openFirst() {
    const first = shownAll[0]?.key || shownLoose[0];
    if (!first) return;
    pick(first);
    focusMod = true;
  }
  // Right after the mod's page is drawn, so typing goes on in its box.
  let focusMod = $state(false);
  $effect(() => {
    if (focusMod && modInput && sel) {
      modInput.focus();
      focusMod = false;
    }
  });
  function topKeys(ev) {
    if (ev.key === 'Enter') openFirst();
    else if (ev.key === 'Escape') query = '';
  }

  // ---- settings of other mods that mention the search ----------------------------------
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
  /** Mods (and loose files) whose settings match but whose name doesn't, most matches first. */
  const others = $derived.by(() => {
    if (!hits || !words.length) return [];
    const by = new Map();
    for (const h of hits) {
      if (side !== 'all' && fileRow(h.rel)?.side !== side) continue;
      const owner = fileRow(h.rel)?.owner;
      const m = owner ? modRows.find((x) => x.key === owner) : null;
      if (m ? nameHit(m.name, m.key) : nameHit(h.rel)) continue;
      const key = m ? m.key : h.rel;
      if (!by.has(key)) by.set(key, { key, mod: m, n: 0 });
      by.get(key).n++;
    }
    return [...by.values()].sort((a, b) => b.n - a.n || (a.mod?.name || a.key).localeCompare(b.mod?.name || b.key));
  });
  const othersN = $derived(others.reduce((n, o) => n + o.n, 0));

  function editText(rel, line) {
    jump.path = rel;
    jump.line = line || 0;
    jump.text = true;
    location.hash = `/server/${[server.id, 'edit', ...rel.split('/')].map(encodeURIComponent).join('/')}`;
  }
  const initials = (n) => n.replace(/[^A-Za-z0-9 ]/g, '').split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0]).join('').toUpperCase() || '?';

  // Changed elsewhere (another tab, Hopper desktop, on disk): show it.
  $effect(() => followConfigs(sid));
</script>

<div class="cfgp">
  <div class="tools">
    <label class="input find"><Search size={13} /><input use:rightClear placeholder={index ? `Search ${index.mods.length.toLocaleString()} mods` : 'Search mods'} bind:value={query} onkeydown={topKeys} title="Enter opens the first mod" />{#if searching}<span class="spinner"></span>{:else if query}<button class="clr" title="Clear" onclick={() => (query = '')}><X size={12} /></button>{/if}</label>
    <button class="chip dchip" class:on={desc} aria-pressed={desc} title="Look in each setting's description too, in the open mod and in other mods" onclick={() => (desc = !desc)}>Also in descriptions</button>
    <div class="grow"></div>
    <div class="row fgroup">
      <span class="glabel"><Funnel size={12} />Filter</span>
      <div class="utabs" role="tablist">
        <button role="tab" aria-selected={filter === 'all'} class:tab-sel={filter === 'all'} onclick={() => (filter = 'all')}>All</button>
        <button role="tab" aria-selected={filter === 'changed'} class:tab-sel={filter === 'changed'} onclick={() => (filter = 'changed')}>Changed {counts.changed}</button>
        <button role="tab" aria-selected={filter === 'unsaved'} class:tab-sel={filter === 'unsaved'} onclick={() => (filter = 'unsaved')}>Unsaved {counts.unsaved}</button>
        {#if players?.snapshot}<button role="tab" aria-selected={filter === 'players'} class:tab-sel={filter === 'players'} title="Settings where players' copy differs from the server's" onclick={() => (filter = 'players')}>Differs from players {players.differs}</button>{/if}
      </div>
    </div>
    <label class="row small sidepick"><span class="glabel">Side</span>
      <select class="input sidesel" bind:value={side} title="Who reads the file">
        {#each SIDES as [k, label] (k)}<option value={k}>{label}</option>{/each}
      </select>
    </label>
  </div>

  {#if running}
    <div class="rnote small"><Info size={14} />The server is running. Settings you save here apply when it restarts.</div>
  {/if}
  {#if players?.differs}
    <div class="pnote small">
      <Users size={14} />
      <span class="grow"><b>Players' copies differ in {players.differs} setting{players.differs === 1 ? '' : 's'}</b> <span class="muted">· from {players.snapshot.name}'s Hopper, sent {sentAgo(players.snapshot.at)}</span></span>
      <button class="btn sm ghost" onclick={() => (filter = 'players')}>Show them</button>
      <button class="btn sm" onclick={() => (pushing = true)}><Send size={13} />Push {players.differs} to paired Hopper clients</button>
    </div>
  {/if}
  {#if error}
    <div class="empty-state"><span class="es-ic"><SlidersHorizontal size={18} /></span><b>Can't read the configs</b><p class="err">{error}</p></div>
  {:else}
    <div class="body" class:picked={!!selRels.length}>
      <nav class="mods" aria-label="Mods with config files">
        {#if !index}
          <div class="skels">{#each [0, 1, 2, 3, 4, 5, 6] as i (i)}<div class="skel"><i></i></div>{/each}</div>
        {:else}
          {#if changedRows.length && filter === 'all' && !words.length}
            <div class="grp">Changed · {changedRows.length}</div>
            {#each changedRows as m (m.key)}{@render modRow(m)}{/each}
          {/if}
          <div class="grp">{words.length ? 'Mods' : filter === 'all' ? 'All mods' : filter === 'changed' ? 'Changed' : filter === 'players' ? 'Differs from players' : 'Unsaved'} · {shownAll.length}</div>
          {#each shownAll as m (m.key)}{@render modRow(m)}{:else}<div class="small muted pad">{words.length ? 'No mod is called that.' : 'None here.'}</div>{/each}
          {#if others.length}
            <button class="mrow row-hover ofold" aria-expanded={othersOpen} title="{othersN.toLocaleString()} setting{othersN === 1 ? '' : 's'} in {others.length} other mod{others.length === 1 ? '' : 's'} mention “{query.trim()}”" onclick={() => (othersOpen = !othersOpen)}>
              {#if othersOpen}<ChevronDown size={13} />{:else}<ChevronRight size={13} />{/if}<span class="ell">Settings in other mods</span><span class="c">{othersN >= 300 ? '300+' : othersN}</span>
            </button>
            {#if othersOpen}
              {#each others as o (o.key)}
                <button class="mrow row-hover" class:row-sel={sel === o.key} onclick={() => pick(o.key, query.trim())} title={o.mod?.name || o.key}>
                  {#if o.mod}{#if o.mod.icon}<img src={o.mod.icon} alt="" width="20" height="20" loading="lazy" />{:else}<span class="sic ph">{initials(o.mod.name)}</span>{/if}<span class="ell">{o.mod.name}</span>{:else}<span class="sic q"><FileQuestion size={12} /></span><span class="ell mono small">{shortRel(o.key)}</span>{/if}
                  <span class="c">{o.n} match{o.n === 1 ? '' : 'es'}</span>
                </button>
              {/each}
            {/if}
          {/if}
          {#if shownLoose.length}
            <div class="grp">Not matched to a mod · {shownLoose.length}</div>
            {#each shownLoose as r (r)}
              <button class="mrow row-hover" class:row-sel={sel === r} onclick={() => pick(r)} title={r}>
                <span class="sic q"><FileQuestion size={12} /></span><span class="ell mono small">{@render hl(shortRel(r))}</span>
                {#if unsavedRels.has(r)}<span class="c"><span class="udot"></span></span>{:else if fileRow(r)?.changed}<span class="c">{fileRow(r).changed}</span>{/if}
              </button>
            {/each}
          {/if}
        {/if}
      </nav>

      <section class="main">
        {#if selRels.length}
          <div class="phone-back"><button class="btn sm ghost" onclick={() => pick('')}><ArrowLeft size={13} />All mods</button></div>
          <div class="mh">
            {#if selLooks}
              {#if selLooks.icon}<img src={selLooks.icon} alt="" width="32" height="32" />{:else}<span class="sic big">{initials(selLooks.name)}</span>{/if}
              <div class="grow"><div class="nm">{selLooks.name}</div><div class="tiny muted">{selMod.files.length} config file{selMod.files.length === 1 ? '' : 's'}{selMod.changed ? ` · ${selMod.changed} not at default` : ''}</div></div>
            {:else}
              <span class="sic big q"><FileQuestion size={16} /></span>
              <div class="grow"><div class="nm mono">{shortRel(selLoose)}</div><div class="tiny muted">No mod's name matches this file.</div></div>
            {/if}
            <label class="input find msearch"><Search size={13} /><input bind:this={modInput} use:rightClear placeholder={selLooks ? `Search ${selLooks.name}'s settings` : "Search this file's settings"} bind:value={modQuery} onkeydown={(ev) => ev.key === 'Escape' && (modQuery = '')} />{#if modQuery}<button class="clr" title="Clear" onclick={() => (modQuery = '')}><X size={12} /></button>{/if}</label>
          </div>
          {#key sel}<ConfigView {server} rels={selRels} {filter} {side} query={modQuery} {desc} bind:pick={filePick} onEditText={editText} />{/key}
        {:else if index}
          <div class="empty-state">
            <span class="es-ic"><SlidersHorizontal size={18} /></span>
            <b>{index.files.length ? 'Pick a mod' : 'No config files yet'}</b>
            <p>{index.files.length ? `${index.files.length} config files with ${index.settings.toLocaleString()} settings. Pick a mod on the left, or search for one by name.` : 'Mods write their config files the first time the server starts.'}</p>
          </div>
        {/if}
      </section>
    </div>
    <ConfigBar {server} />
  {/if}
</div>
{#if pushing}<PushPlayers {server} onClose={() => (pushing = false)} />{/if}

{#snippet hl(text)}{#each marks(text, words) as p, i (i)}{#if p.hit}<mark class="hit">{p.t}</mark>{:else}{p.t}{/if}{/each}{/snippet}

{#snippet modRow(m)}
  <button class="mrow row-hover" class:row-sel={sel === m.key && !hits} onclick={() => pick(m.key)} title={m.name}>
    {#if m.icon}<img src={m.icon} alt="" width="20" height="20" loading="lazy" />{:else}<span class="sic ph">{initials(m.name)}</span>{/if}
    <span class="ell">{@render hl(m.name)}</span>
    {#if m.unsaved}<span class="c" title="Not saved yet"><span class="udot"></span></span>{:else if m.changed}<span class="c" title="{m.changed} not at the default the file states">{m.changed}</span>{:else if m.files.length > 1}<span class="c">{m.files.length} files</span>{/if}
  </button>
{/snippet}

<style>
  .cfgp { display: flex; flex-direction: column; flex: 1; min-height: 0; border: 1px solid var(--line); border-radius: var(--radius-lg); background: var(--base); overflow: hidden; }
  .tools { flex: none; display: flex; align-items: center; gap: 10px 14px; flex-wrap: wrap; padding: 10px 12px; border-bottom: 1px solid var(--line); }
  .find { width: 340px; padding: 5px 9px; }
  .find input { flex: 1; min-width: 0; }
  .clr { color: var(--muted); display: grid; place-items: center; }
  .dchip { cursor: pointer; height: 26px; padding: 0 10px 2px; font-size: 12px; }
  .fgroup { gap: 6px; }
  .sidesel { width: auto; padding: 3px 8px; color: var(--text); outline: none; }
  .body { flex: 1; min-height: 0; display: grid; grid-template-columns: 260px minmax(0, 1fr); }
  .mods { border-right: 1px solid var(--line); overflow: auto; padding: 6px; display: flex; flex-direction: column; gap: 1px; }
  .grp { font-size: 11px; font-weight: 600; color: var(--muted); letter-spacing: .05em; text-transform: uppercase; padding: 10px 10px 4px; }
  .mrow { display: flex; align-items: center; gap: 8px; padding: 5px 10px; border-radius: var(--radius); text-align: left; color: var(--text); min-height: 32px; /* A pack has hundreds of mods: the browser skips drawing the ones off screen. */ content-visibility: auto; contain-intrinsic-size: auto 32px; }
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
  .msearch { width: 300px; padding: 5px 9px; flex: none; }
  .msearch input { flex: 1; min-width: 0; }
  .ofold { margin-top: 6px; color: var(--muted); font-size: 12px; gap: 6px; }
  .skels { display: flex; flex-direction: column; padding-top: 4px; }
  .phone-back { display: none; }
  .rnote { flex: none; display: flex; align-items: center; gap: 8px; padding: 8px 12px; background: var(--raise); border-bottom: 1px solid var(--line); color: var(--subtle); }
  .rnote :global(svg) { color: var(--muted); flex: none; }
  .pnote { flex: none; display: flex; align-items: center; gap: 10px; flex-wrap: wrap; padding: 8px 12px; background: var(--blue-dim); border-bottom: 1px solid var(--line); color: var(--text); }
  .pnote :global(svg) { color: var(--blue); flex: none; }
  @media (max-width: 700px) {
    /* The page scrolls as a whole on a phone; the unsaved bar sticks to the bottom. */
    :global(.full:has(> .cfgp)) { display: block; overflow: auto; }
    .cfgp { flex: none; overflow: visible; }
    .main { overflow: visible; }
    .find { width: 100%; }
    .mh { flex-wrap: wrap; }
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
