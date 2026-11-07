<script>
  import { onChange } from './lib/changes.svelte.js';
  // A server's data packs as one list, as desktop's Data packs tab: every
  // place a pack can be (Paxi, Global Packs, OpenLoader, each world) folded
  // into one row, grouped by where it counts (Always on, In worlds, Off).
  // Each row's Where pill moves it; dragging a row to another group does
  // the same, and inside Paxi sets its load order. Nothing is deleted: a
  // copy that stops counting is switched off (".disabled").
  import { GripVertical, ChevronDown, Check, FolderOpen, Package, Search } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';

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
  const locs = $derived(data?.locations || []);
  const loaders = $derived(locs.filter((l) => !l.world));
  const worlds = $derived(locs.filter((l) => l.world));
  const label = (key) => locs.find((l) => l.key === key)?.label || key;
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
  const pillLabel = (g) => g.kind === 'global' ? (g.key === 'paxi' ? 'Always on' : `Always on · ${label(g.key)}`) : g.kind === 'in_worlds' ? 'In worlds' : 'Off';
  const tone = (g) => (g.kind === 'global' ? 'lan' : g.kind === 'in_worlds' ? 'running' : '');
  const dirOf = (g) => (g.kind === 'global' ? locs.find((l) => l.key === g.key)?.dir : null);

  const matches = (e) =>
    (!query.trim() || e.file.toLowerCase().includes(query.trim().toLowerCase())) &&
    (show === 'all' || (show === 'on' && e.group.kind === 'global') || (show === 'worlds' && e.group.kind === 'in_worlds') || (show === 'off' && e.group.kind === 'off'));
  const rowsOf = (g) => (data?.entries || []).filter((e) => gkey(e.group) === gkey(g) && matches(e)).sort((a, b) => (a.rank ?? 1e9) - (b.rank ?? 1e9));
  const count = (f) => (data?.entries || []).filter(f).length;
  const filtered = $derived(!!query.trim() || show !== 'all');

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
    const e = data.entries.find((x) => x.file === d.file);
    if (e) setWhere(e, targetOf(g, e));
  }
  async function dropOnRow(t) {
    const d = drag;
    end();
    if (!d || d.file === t.file) return;
    const e = data.entries.find((x) => x.file === d.file);
    if (!e) return;
    if (d.group !== gkey(t.group)) return setWhere(e, targetOf(t.group, e));
    // Inside Paxi: the dropped pack takes the target's place.
    if (t.group.kind !== 'global' || t.group.key !== 'paxi') return;
    const names = rowsOf(t.group).map((x) => x.file).filter((f) => f !== e.file);
    names.splice(names.indexOf(t.file) + ((e.rank ?? 0) < (t.rank ?? 0) ? 1 : 0), 0, e.file);
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

  // Changed elsewhere (another tab, Hopper desktop, on disk): show it.
  $effect(() => onChange(sid, ['worlds', 'mods'], load));
</script>

<svelte:window onclick={() => (menu = '')} onkeydown={(e) => e.key === 'Escape' && menu && (menu = '')} />

<div class="card dps">
  <div class="head row">
    <b>Data packs</b>
    {#if data}<span class="chip">{data.entries.length}</span>{/if}
    <span class="sp"></span>
    <div class="input sm srch"><Search size={13} /><input bind:value={query} placeholder="Search packs" aria-label="Search data packs" /></div>
    <div class="seg" role="tablist" aria-label="Filter">
      {#each [['all', 'All', null], ['on', 'Always on', (e) => e.group.kind === 'global'], ['worlds', 'In worlds', (e) => e.group.kind === 'in_worlds'], ['off', 'Off', (e) => e.group.kind === 'off']] as [k, l, f] (k)}
        <button role="tab" aria-selected={show === k} class:on={show === k} onclick={() => (show = k)}>{l}{#if f && data} <span class="n">{count(f)}</span>{/if}</button>
      {/each}
    </div>
  </div>

  {#if !data}
    <div class="skel"><i></i></div>
  {:else if !data.entries.length && !locs.length}
    <div class="small muted">No data pack folders yet. Start the server once to make a world, or add Paxi to load packs into every world.</div>
  {:else}
    {#each groups as g (gkey(g))}
      {@const rows = rowsOf(g)}
      {#if rows.length || !filtered}
        <div
          class="grp row"
          class:drop={drag && drag.group !== gkey(g)}
          class:over={over === gkey(g)}
          role="group"
          aria-label={title(g)}
          ondragover={(ev) => allow(ev, gkey(g))}
          ondragleave={() => over === gkey(g) && (over = '')}
          ondrop={(ev) => { ev.preventDefault(); dropOnGroup(g); }}
        >
          <b>{title(g)}</b><span class="chip {tone(g)}">{rows.length}</span>
          <span class="small muted hint">{drag && drag.group !== gkey(g) ? `Drop here: ${pillLabel(g)}` : hint(g)}</span>
          <span class="sp"></span>
          {#if dirOf(g)}<button class="btn sm ghost" onclick={() => openFiles(dirOf(g))}><FolderOpen size={13} />Open folder</button>{/if}
        </div>
        {#each rows as e (e.file)}
          <div
            class="dp row-hover"
            class:over={over === e.file}
            class:dim={e.group.kind === 'off'}
            role="listitem"
            ondragover={(ev) => allow(ev, e.file)}
            ondragleave={() => over === e.file && (over = '')}
            ondrop={(ev) => { ev.preventDefault(); dropOnRow(e); }}
          >
            <button class="grip" draggable="true" title="Drag to reorder, or to another group" aria-label="Drag {e.file}" ondragstart={(ev) => start(ev, e)} ondragend={end}><GripVertical size={14} /></button>
            {#if e.rank && g.kind === 'global' && g.key === 'paxi'}<span class="rank">{e.rank}</span>{:else}<span class="rank none"></span>{/if}
            <Package size={15} class="muted" />
            <span class="dpn">{e.file}</span>
            {#if e.copies.length > 1}<span class="chip" title={e.copies.map((c) => `${c.label}${c.on ? '' : ' (off)'}`).join(', ')}>{e.copies.length} copies{e.using ? `, using ${e.using}'s` : ''}</span>{/if}
            {#if e.group.kind === 'in_worlds'}<span class="chip running" title={e.worlds.map(label).join(', ')}>{e.worlds.length === 1 ? '1 world' : `${e.worlds.length} worlds`}</span>{/if}
            <span class="sp"></span>
            <div class="wrapp">
              <button class="btn sm pill-w {tone(e.group)}" disabled={busy} aria-haspopup="menu" aria-expanded={menu === e.file} onclick={(ev) => { ev.stopPropagation(); menu = menu === e.file ? '' : e.file; }}>
                {pillLabel(e.group)}<ChevronDown size={12} />
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
          </div>
        {/each}
      {/if}
    {/each}
  {/if}
</div>

<style>
  .dps { padding: 12px 14px; margin-bottom: 10px; display: flex; flex-direction: column; gap: 2px; }
  .head { gap: 8px; flex-wrap: wrap; margin-bottom: 6px; }
  .srch { display: flex; align-items: center; gap: 6px; padding: 3px 8px; width: 200px; color: var(--muted); }
  .srch input { flex: 1; min-width: 0; }
  .seg { display: flex; border: 1px solid var(--line2); border-radius: var(--radius); overflow: hidden; }
  .seg button { padding: 4px 10px; font-size: 12.5px; color: var(--subtle); }
  .seg button + button { border-left: 1px solid var(--line2); }
  .seg button.on { background: var(--acc-dim); color: var(--acc); }
  .seg .n { color: var(--muted); }
  .grp { gap: 8px; padding: 8px 8px 4px; margin-top: 4px; border: 1px dashed transparent; border-radius: var(--radius-lg); font-size: 13px; }
  .grp.drop { border-color: #38bdf880; background: #38bdf80f; }
  .grp.over { background: var(--blue-dim); }
  .hint { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .grp.drop .hint { color: var(--blue); }
  .dp { display: flex; align-items: center; gap: 8px; padding: 3px 8px; min-height: 36px; border-radius: var(--radius); font-size: 12.5px; }
  .dp.over { background: var(--blue-dim); }
  .dp.dim .dpn { opacity: 0.6; }
  .grip { color: var(--muted); cursor: grab; display: inline-flex; padding: 2px; border-radius: var(--radius-sm); }
  .rank { width: 20px; height: 20px; border-radius: 10px; flex: none; display: grid; place-items: center; font-size: 11px; font-weight: 700; color: var(--blue); background: var(--blue-dim); }
  .rank.none { background: none; }
  .dpn { min-width: 0; max-width: 38rem; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .wrapp { position: relative; }
  .pill-w { width: 10.5rem; justify-content: space-between; border-color: transparent; background: #ededef14; }
  .pill-w.lan { color: var(--blue); background: var(--blue-dim); }
  .pill-w.running { color: var(--green); background: var(--green-dim); }
  .pop { position: absolute; z-index: 70; top: calc(100% + 4px); right: 0; width: 290px; display: flex; flex-direction: column; }
  .d { display: block; }
  .skel { height: 40px; }
</style>
