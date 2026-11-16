<script>
  import { onChange } from './lib/changes.svelte.js';
  // A server's data packs as one list, as desktop's Data packs tab: every
  // place a pack can be (Paxi, Global Packs, OpenLoader, each world) folded
  // into one row, grouped by where it counts (Always on, In worlds, Off).
  // Laid out as the Mods tab (toolbar, columns, row height, name at the same
  // spot) so every content tab reads the same. Each row's Where pill moves
  // it; dragging a row by its grip to another group does the same, and
  // inside Paxi sets its load order. Nothing is deleted: a copy that stops
  // counting is switched off (".disabled").
  import { GripVertical, ChevronDown, ChevronUp, Check, FolderOpen, Package, Search, X, Plus, Layers, Globe, Map as MapIcon, CircleOff, Database } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { browseHash, seedBrowse } from './lib/browse.js';
  import { rightClear } from './lib/rightclear.js';

  let { server, openFiles, reloadKey = 0 } = $props();
  // The live feed hands over a new `server` object every second; effects key on the id.
  const sid = $derived(server.id);
  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);

  let data = $state(null);
  let busy = $state(false);
  let query = $state('');
  let show = $state('all'); // all | on | worlds | off
  let menu = $state(''); // the file whose Where menu is open
  let drag = $state(null); // { file, group }
  let over = $state(''); // the group key or file a drag is over
  let listW = $state(0);

  async function load() {
    try {
      data = await api(`${base}/datapacks`);
    } catch (e) {
      say(e.message, true);
    }
  }
  $effect(() => {
    sid;
    reloadKey;
    load();
  });

  const gkey = (g) => (g.kind === 'global' ? `global:${g.key}` : g.kind);
  const entries = $derived(data?.entries || []);
  const locs = $derived(data?.locations || []);
  const loaders = $derived(locs.filter((l) => !l.world));
  const worlds = $derived(locs.filter((l) => l.world));
  const label = (key) => locs.find((l) => l.key === key)?.label || key;
  // Only the groups that exist here: a loader's Always on group shows when
  // that loader's folder is there, In worlds when there is a world.
  const groups = $derived([
    ...loaders.map((l) => ({ kind: 'global', key: l.key })),
    ...(worlds.length ? [{ kind: 'in_worlds' }] : []),
    { kind: 'off' },
  ]);
  const title = (g) => g.kind === 'global' ? (g.key === 'paxi-optional' ? 'Paxi optional' : `Always on · ${label(g.key)}`) : g.kind === 'in_worlds' ? 'In worlds' : 'Off';
  const hint = (g) =>
    g.kind === 'global' ? (g.key === 'paxi' ? 'Every world, new ones too, in this load order.' : 'Every world.')
    : g.kind === 'in_worlds' ? "Copied into each chosen world's datapacks folder. Order is per world, set in game."
    : 'Drag up, or use the pill, to turn on.';
  // Narrow lists get desktop's short pill words; the full one is the tooltip.
  const narrow = $derived(listW > 0 && listW <= 640);
  const pillLabel = (g) => g.kind === 'global' ? (g.key === 'paxi' ? 'Always on' : `Always on · ${label(g.key)}`) : g.kind === 'in_worlds' ? 'In worlds' : 'Off';
  const pillShort = (g) => (g.kind === 'global' ? 'Always' : g.kind === 'in_worlds' ? 'Worlds' : 'Off');
  const tone = (g) => (g.kind === 'global' ? 'lan' : g.kind === 'in_worlds' ? 'running' : '');
  const dirOf = (g) => (g.kind === 'global' ? locs.find((l) => l.key === g.key)?.dir : null);
  const ranked = (e) => !!e.rank && e.group.kind === 'global' && e.group.key === 'paxi';
  // 1st, 2nd, 3rd, 11th: as desktop's rank chip.
  function ordinal(n) {
    const s = n % 100 >= 11 && n % 100 <= 13 ? 'th' : ({ 1: 'st', 2: 'nd', 3: 'rd' })[n % 10] || 'th';
    return `${n}${s}`;
  }

  // The filter rail, as Mods': what is listed, with counts.
  const fits = {
    all: () => true,
    on: (e) => e.group.kind === 'global',
    worlds: (e) => e.group.kind === 'in_worlds',
    off: (e) => e.group.kind === 'off',
  };
  const rail = $derived([
    { k: 'all', label: 'All', icon: Layers },
    ...(loaders.length || show === 'on' ? [{ k: 'on', label: 'Always on', icon: Globe }] : []),
    ...(worlds.length || show === 'worlds' ? [{ k: 'worlds', label: 'In worlds', icon: MapIcon }] : []),
    { k: 'off', label: 'Off', icon: CircleOff },
  ].map((r) => ({ ...r, n: entries.filter(fits[r.k]).length, on: show === r.k })));
  // Arrow keys move across the rail.
  function railKey(e) {
    const dir = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
    if (!dir) return;
    const bs = [...e.currentTarget.querySelectorAll('button')];
    const at = bs.indexOf(document.activeElement);
    bs[Math.max(0, Math.min(bs.length - 1, at + dir))]?.focus();
    e.preventDefault();
  }

  const matches = (e) => (!query.trim() || e.file.toLowerCase().includes(query.trim().toLowerCase())) && fits[show](e);
  const rowsOf = (g) => entries.filter((e) => gkey(e.group) === gkey(g) && matches(e)).sort((a, b) => (a.rank ?? 1e9) - (b.rank ?? 1e9));
  const filtered = $derived(!!query.trim() || show !== 'all');
  const anyShown = $derived(entries.some(matches));

  async function setWhere(e, to) {
    menu = '';
    busy = true;
    try {
      const r = await post(`${base}/datapacks/where`, { file: e.file, to });
      say(r.reload ? `${e.file} moved. The server picks it up after /reload or a restart.` : `${e.file} moved.`);
    } catch (err) {
      say(err.message, true);
    } finally {
      busy = false;
      load();
    }
  }
  // A drop on a group: In worlds means the worlds it was in, else every world.
  function targetOf(g, e) {
    if (g.kind === 'global') return { kind: 'global', key: g.key };
    if (g.kind === 'off') return { kind: 'off' };
    const had = (e.copies || []).filter((c) => c.key.startsWith('world:')).map((c) => c.key);
    return { kind: 'worlds', key: had.length ? had : worlds.map((w) => w.key) };
  }
  function toggleWorld(e, key) {
    const now = e.group.kind === 'in_worlds' ? e.worlds : [];
    const next = now.includes(key) ? now.filter((k) => k !== key) : [...now, key];
    setWhere(e, next.length ? { kind: 'worlds', key: next } : { kind: 'off' });
  }
  async function saveOrder(names) {
    busy = true;
    try {
      const r = await post(`${base}/datapacks/order`, { names });
      say(r.reload ? 'Load order saved. The server uses it after /reload or a restart.' : 'Load order saved.');
    } catch (err) {
      say(err.message, true);
    } finally {
      busy = false;
      load();
    }
  }
  // Move up / Move down: the keyboard's way to change Paxi's load order, as
  // desktop's row menu. The whole group's order, not just the shown rows.
  function step(e, d) {
    const names = entries.filter((x) => gkey(x.group) === gkey(e.group)).sort((a, b) => (a.rank ?? 1e9) - (b.rank ?? 1e9)).map((x) => x.file);
    const i = names.indexOf(e.file);
    const j = i + d;
    if (i < 0 || j < 0 || j >= names.length) return;
    [names[i], names[j]] = [names[j], names[i]];
    saveOrder(names);
  }
  const groupLen = (g) => entries.filter((x) => gkey(x.group) === gkey(g)).length;

  // ---- dragging: to another group, or to another place in Paxi ----
  function start(ev, e) {
    drag = { file: e.file, group: gkey(e.group) };
    ev.dataTransfer.effectAllowed = 'move';
    ev.dataTransfer.setData('text/plain', e.file);
  }
  function end() {
    drag = null;
    over = '';
  }
  function allow(ev, key) {
    if (!drag) return;
    ev.preventDefault();
    over = key;
  }
  async function dropOnGroup(g) {
    const d = drag;
    end();
    if (!d || d.group === gkey(g)) return;
    const e = entries.find((x) => x.file === d.file);
    if (e) setWhere(e, targetOf(g, e));
  }
  async function dropOnRow(t) {
    const d = drag;
    end();
    if (!d || d.file === t.file) return;
    const e = entries.find((x) => x.file === d.file);
    if (!e) return;
    if (d.group !== gkey(t.group)) return setWhere(e, targetOf(t.group, e));
    // Inside Paxi: the dropped pack takes the target's place.
    if (t.group.kind !== 'global' || t.group.key !== 'paxi') return;
    const names = rowsOf(t.group).map((x) => x.file).filter((f) => f !== e.file);
    names.splice(names.indexOf(t.file) + ((e.rank ?? 0) < (t.rank ?? 0) ? 1 : 0), 0, e.file);
    saveOrder(names);
  }

  // Changed elsewhere (another tab, Hopper desktop, on disk): show it.
  $effect(() => onChange(sid, ['worlds', 'mods'], load));
</script>

<svelte:window onclick={() => (menu = '')} onkeydown={(e) => e.key === 'Escape' && menu && (menu = '')} />

<div class="card">
  <nav class="subtabs">
    <span class="tab tab-sel">Data packs{data ? ` ${entries.length}` : ''}</span>
  </nav>

  {#if !data}
    <div class="skels">{#each [0, 1, 2, 3] as i (i)}<div class="skel"><i></i></div>{/each}</div>
  {:else if !locs.length && !entries.length}
    <!-- Nothing to put packs in yet: no world, no loader. -->
    <div class="empty-state top"><span class="es-ic"><Globe size={20} /></span><b>No world yet</b><p>Data packs go into a world. Start the server once to make one, or install Paxi to load them in every world.</p><a class="btn sm" href="#{browseHash('mods', server.id)}" onclick={() => seedBrowse('paxi')}><Search size={13} />Find Paxi</a></div>
  {:else if !entries.length}
    <!-- Truly empty: no toolbar or columns, as an empty Mods tab. -->
    <div class="empty-state top"><span class="es-ic"><Database size={20} /></span><b>No data packs yet</b><p>They change recipes, loot and world generation, in every world or one.</p><span class="row eb"><a class="btn sm pri" href="#{browseHash('datapacks', server.id)}"><Plus size={13} />Add data packs</a>{#if !loaders.some((l) => l.key === 'paxi')}<a class="btn sm" href="#{browseHash('mods', server.id)}" title="Paxi loads data packs into every world, new ones included" onclick={() => seedBrowse('paxi')}><Search size={13} />Find Paxi</a>{/if}</span></div>
  {:else}
    <!-- Laid out as Mods' toolbar: actions, search, what's listed. -->
    <div class="row toolbar">
      <a class="btn sm" href="#{browseHash('datapacks', server.id)}" title="Add content"><Plus size={13} /><span class="lb">Add content</span></a>
      <span class="vr"></span>
      <div class="input find"><Search size={13} /><input use:rightClear placeholder="Search" aria-label="Search data packs" bind:value={query} />{#if query}<button class="clr" title="Clear" onclick={() => (query = '')}><X size={12} /></button>{/if}</div>
      <div class="seg icons" role="group" aria-label="Filter" onkeydown={railKey}>
        {#each rail as r (r.k)}
          <button class="si" aria-pressed={r.on} aria-label="{r.label}, {r.n}" data-tip="{r.label} · {r.n}" onclick={() => (show = r.k)}>
            <span class="ico"><r.icon size={14} /></span><span class="lbw"><span>{r.label}<span class="ct">{r.n}</span></span></span>
          </button>
        {/each}
      </div>
    </div>

    <div class="list" class:narrow bind:clientWidth={listW}>
      <div class="tr th" role="row">
        <span></span>
        <span></span>
        <span class="c-name">Name</span>
        <span>Where</span>
        <span></span>
      </div>
      {#if !anyShown}
        <div class="empty-state"><span class="es-ic"><Search size={20} /></span><b>Nothing matches</b><p>No data pack here fits the search and filters.</p><button class="btn sm" onclick={() => { query = ''; show = 'all'; }}>Clear search and filters</button></div>
      {/if}
      {#each groups as g (gkey(g))}
        {@const rows = rowsOf(g)}
        {#if rows.length || !filtered}
          <!-- A quiet section heading, as desktop's group rows. -->
          <div
            class="grp"
            class:drop={drag && drag.group !== gkey(g)}
            class:over={over === gkey(g)}
            role="group"
            aria-label={title(g)}
            ondragover={(ev) => allow(ev, gkey(g))}
            ondragleave={() => over === gkey(g) && (over = '')}
            ondrop={(ev) => { ev.preventDefault(); dropOnGroup(g); }}
          >
            <b>{title(g)}</b><span class="n">{rows.length}</span>
            <span class="hint">{drag && drag.group !== gkey(g) ? `Drop here: ${pillLabel(g)}` : hint(g)}</span>
            <span class="sp"></span>
            {#if dirOf(g)}<button class="btn sm ghost" onclick={() => openFiles(dirOf(g))}><FolderOpen size={13} /><span class="lb">Open folder</span></button>{/if}
          </div>
          {#each rows as e (e.file)}
            {@const rk = ranked(e)}
            <div
              class="tr dp row-hover"
              class:over={over === e.file}
              class:off={e.group.kind === 'off'}
              class:menu={menu === e.file}
              role="row"
              tabindex="-1"
              ondragover={(ev) => allow(ev, e.file)}
              ondragleave={() => over === e.file && (over = '')}
              ondrop={(ev) => { ev.preventDefault(); dropOnRow(e); }}
            >
              <!-- The grip shows on hover, in Mods' tick column, so the name sits where Mods' does. -->
              <button class="grip" draggable="true" title={rk ? 'Drag to change the load order, or to another group' : 'Drag to another group'} aria-label="Drag {e.file}" ondragstart={(ev) => start(ev, e)} ondragend={end}><GripVertical size={14} /></button>
              <span class="ph"><Package size={15} /></span>
              <div class="c-name">
                <div class="nm"><span class="ell" title={e.file}>{e.file}</span></div>
                <div class="tags">
                  {#if rk}<span class="chip tag info" title="Paxi loads it {ordinal(e.rank)}. Drag the grip, or use the arrows, to change that.">{ordinal(e.rank)}</span>{/if}
                  {#if e.group.kind === 'in_worlds'}<span class="chip tag okc" title={e.worlds.map(label).join(', ')}>{e.worlds.length === 1 ? '1 world' : `${e.worlds.length} worlds`}</span>{/if}
                  {#if e.copies.length > 1}<span class="chip tag" title={e.copies.map((c) => `${c.label}${c.on ? '' : ' (off)'}`).join(', ')}>{e.copies.length} copies{e.using ? `, using ${e.using}'s` : ''}</span>{/if}
                </div>
              </div>
              <div class="wrapp">
                <button class="btn sm pill-w {tone(e.group)}" disabled={busy} title={narrow ? pillLabel(e.group) : null} aria-haspopup="menu" aria-expanded={menu === e.file} onclick={(ev) => { ev.stopPropagation(); menu = menu === e.file ? '' : e.file; }}>
                  <span class="ell">{narrow ? pillShort(e.group) : pillLabel(e.group)}</span><ChevronDown size={12} />
                </button>
                {#if menu === e.file}
                  <div class="pop popover menu" role="menu" tabindex="-1" onclick={(ev) => ev.stopPropagation()} onkeydown={(ev) => ev.key === 'Escape' && (menu = '')}>
                    <div class="menu-label">Where</div>
                    <button class="menu-item" role="menuitemradio" aria-checked={e.group.kind === 'off'} onclick={() => setWhere(e, { kind: 'off' })}>
                      <span class="menu-ck">{#if e.group.kind === 'off'}<Check size={13} />{/if}</span><span>Off<span class="tiny muted d">Kept, but not loaded</span></span>
                    </button>
                    {#each loaders as l (l.key)}
                      {@const on = e.group.kind === 'global' && e.group.key === l.key}
                      <button class="menu-item" role="menuitemradio" aria-checked={on} onclick={() => setWhere(e, { kind: 'global', key: l.key })}>
                        <span class="menu-ck">{#if on}<Check size={13} />{/if}</span><span>{l.key === 'paxi-optional' ? 'Paxi optional' : `Always on (${l.label})`}<span class="tiny muted d">Every world · {l.dir}</span></span>
                      </button>
                    {/each}
                    {#if worlds.length}
                      <div class="menu-sep"></div>
                      <div class="menu-label">In worlds (pick which)</div>
                      {#each worlds as w (w.key)}
                        {@const on = e.group.kind === 'in_worlds' && e.worlds.includes(w.key)}
                        <button class="menu-item" role="menuitemcheckbox" aria-checked={on} onclick={() => toggleWorld(e, w.key)}>
                          <span class="menu-ck">{#if on}<Check size={13} />{/if}</span>{w.label}
                        </button>
                      {/each}
                      {#if worlds.length > 1}<button class="menu-item" onclick={() => setWhere(e, { kind: 'worlds', key: worlds.map((w) => w.key) })}><span class="menu-ck"></span>Every world</button>{/if}
                      {#if loaders.some((l) => l.key === 'paxi')}<div class="menu-label">New worlds: use Always on (Paxi)</div>{/if}
                    {/if}
                  </div>
                {/if}
              </div>
              <span class="c-end row">
                {#if rk}
                  <button class="ib mv" title="Move up" aria-label="Move {e.file} up" disabled={busy || e.rank <= 1} onclick={() => step(e, -1)}><ChevronUp size={14} /></button>
                  <button class="ib mv" title="Move down" aria-label="Move {e.file} down" disabled={busy || e.rank >= groupLen(g)} onclick={() => step(e, 1)}><ChevronDown size={14} /></button>
                {/if}
              </span>
            </div>
          {/each}
        {/if}
      {/each}
    </div>
  {/if}
</div>

<style>
  /* The Mods tab's frame: sub-tabs, toolbar, columns, 52 px rows. */
  .card { display: flex; flex-direction: column; gap: 8px; }
  .subtabs { display: flex; align-items: center; gap: 4px; border-bottom: 1px solid var(--line); }
  .tab { padding: 6px 12px; color: var(--subtle); }
  .toolbar { gap: 6px 10px; flex-wrap: nowrap; min-width: 0; container: dbar / inline-size; }
  .toolbar > :global(*) { flex: none; }
  .toolbar .find { flex: 0 1 224px; min-width: 90px; }
  @container dbar (max-width: 640px) {
    .toolbar :global(.lb) { display: none; }
    .toolbar .vr { display: none; }
  }
  .vr { width: 1px; height: 20px; background: var(--line2); margin: 0; }
  .find { padding: 3px 8px; width: 224px; font-weight: 400; }
  .clr { color: var(--muted); display: grid; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .list { min-width: 0; --cols: 22px 30px minmax(0, 1fr) 190px 74px; }
  .list.narrow { --cols: 22px 30px minmax(0, 1fr) 110px 74px; }
  /* Same grid, gap and padding as Mods' rows, so the name starts at the same x. */
  .tr { display: grid; grid-template-columns: var(--cols); gap: 10px; align-items: center; padding: 6px 8px; position: relative; border-radius: var(--radius); }
  .th { color: var(--muted); font-size: 11.5px; font-weight: 500; padding-top: 2px; padding-bottom: 6px; border-bottom: 1px solid var(--line); border-radius: 0; user-select: none; }
  .dp { height: 52px; font-size: 12.5px; }
  .dp.over { background: var(--blue-dim); }
  .dp.off .c-name, .dp.off .ph { opacity: 0.55; }
  .grip { color: var(--muted); cursor: grab; display: grid; place-items: center; width: 22px; height: 28px; border-radius: var(--radius-sm); visibility: hidden; }
  .dp:hover .grip, .dp.menu .grip, .grip:focus-visible { visibility: visible; }
  .grip:hover { color: var(--text); background: #ffffff10; }
  .ph { width: 30px; height: 30px; border-radius: var(--radius); background: var(--raise); display: grid; place-items: center; color: var(--muted); }
  .nm { font-weight: 500; display: flex; gap: 6px; align-items: center; min-width: 0; }
  .tags { display: flex; gap: 4px; align-items: center; margin-top: 2px; min-width: 0; overflow: hidden; flex-wrap: nowrap; min-height: 17px; }
  .tag { font-size: 10.5px; padding: 0 6px; flex: none; }
  .tag.info { color: var(--ink-blue); background: var(--blue-dim); }
  .tag.okc { color: var(--ink-green); background: var(--green-dim); }
  /* Group rows: quiet headings over their rows, not boxes. */
  .grp { display: flex; align-items: flex-end; gap: 8px; height: 40px; padding: 0 8px 6px; border: 1px dashed transparent; border-radius: var(--radius); font-size: 12px; color: var(--subtle); }
  .grp b { font-weight: 600; color: var(--text); white-space: nowrap; }
  .grp .n { color: var(--muted); }
  .grp .btn { margin-bottom: -3px; }
  .grp.drop { border-color: #38bdf880; background: #38bdf80f; }
  .grp.over { background: var(--blue-dim); }
  .hint { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--muted); font-size: 11.5px; }
  .grp.drop .hint { color: var(--blue); }
  .wrapp { position: relative; min-width: 0; }
  .pill-w { width: 100%; max-width: 10.5rem; justify-content: space-between; border-color: transparent; background: #ededef14; }
  .pill-w.lan { color: var(--ink-blue); background: var(--blue-dim); }
  .pill-w.running { color: var(--ink-green); background: var(--green-dim); }
  .pop { position: absolute; z-index: 70; top: calc(100% + 4px); right: 0; width: 290px; display: flex; flex-direction: column; }
  .d { display: block; }
  .c-end { gap: 2px; justify-content: flex-end; }
  .ib { color: var(--muted); padding: 3px; border-radius: var(--radius); display: grid; }
  .ib:hover:not(:disabled) { color: var(--text); background: #ffffff10; }
  .ib:disabled { opacity: 0.35; }
  /* Move up / down: on hover, as Mods' row buttons. */
  .dp .mv { visibility: hidden; }
  .dp:hover .mv, .dp:focus-within .mv { visibility: visible; }
  .eb { gap: 8px; justify-content: center; flex-wrap: wrap; }
  .skels { display: flex; flex-direction: column; gap: 4px; padding-top: 6px; }
</style>
