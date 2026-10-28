<script>
  // The Mods tab, laid out like Hopper desktop's: a table with Update /
  // What changed (or Changelog) per row, Author, Side, On and Added
  // columns that sort, checkboxes for bulk work, a ⋯ / right-click menu,
  // and the side panel for the picked mod. Unknown jars are identified in
  // the background and updates are checked on CurseForge and Modrinth.
  import { untrack } from 'svelte';
  import { browseHash } from './lib/browse.js';
  import CfKey from './CfKey.svelte';
  import ModPeek from './ModPeek.svelte';
  import ModVersions from './ModVersions.svelte';
  import ReviewUpdates from './ReviewUpdates.svelte';
  import ModBackups from './ModBackups.svelte';
  import ModProfiles from './ModProfiles.svelte';
  import LoaderVersions from './LoaderVersions.svelte';
  import {
    Search, Plus, ScanSearch, RefreshCw, X, Settings2, Lock, LockOpen, Download, ScrollText, Ellipsis, Trash2,
    Monitor, Server as ServerIcon, ArrowLeftRight, ArrowDown, ArrowUp, ExternalLink, Shuffle, FolderOpen, CircleCheck, Clock,
  } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { ask } from './lib/dialog.svelte.js';
  import { followJob } from './lib/jobs.js';
  import { setOn, removeMods, setLocked, resetConfig, switchSource, ago } from './lib/modops.js';
  import { pick, startPick, swept, sweeping, clickSwallowed, tickLabel, countLabel } from './lib/dragpick.svelte.js';

  let { server } = $props();
  // The live feed hands over a new `server` object every second; effects
  // that should run once per server key on the id alone.
  const sid = $derived(server.id);
  let sub = $state('mods');
  let data = $state(null);
  let filter = $state('');
  let chip = $state('all');
  let busy = $state('');
  let identifying = $state('');
  let updates = $state(null);
  let checking = $state(false);
  let reviewing = $state(false);
  let picked = $state(null);
  let versionsFor = $state(null);
  let loaderOpen = $state(false);
  let backupCount = $state(null);
  let selected = $state(new Set());
  let anchor = null;
  let menu = $state(null);
  let updating = $state(new Set());
  let done = $state(new Set());
  let catTries = 0;
  // Finding the mod that breaks it: its state, and the start card's kind.

  const pref = (k, d) => {
    try {
      const v = localStorage.getItem(`hopperd-mods-${k}`);
      return v == null ? d : JSON.parse(v);
    } catch {
      return d;
    }
  };
  const keep = (k, v) => {
    try {
      localStorage.setItem(`hopperd-mods-${k}`, JSON.stringify(v));
    } catch {
      // private mode: not remembered
    }
  };
  let onlyYours = $state(pref('yours', false));
  let updatesFirst = $state(pref('updates-first', true));
  let sort = $state(pref('sort', { col: 'added', desc: true }));
  $effect(() => keep('yours', onlyYours));
  $effect(() => keep('updates-first', updatesFirst));
  $effect(() => keep('sort', sort));

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));
  const mods = $derived(data?.mods || []);
  // "Yours" only means something on a server made from a pack.
  const hasPack = $derived(!!data?.pack);
  const yours = (m) => hasPack && m.source && !m.from_pack;
  const base0 = $derived(mods.filter((m) => !onlyYours || !hasPack || yours(m)));
  const counts = $derived({
    all: base0.length,
    updates: base0.filter((m) => m.update && !m.locked).length,
    off: base0.filter((m) => !m.enabled).length,
    client: base0.filter((m) => m.side === 'client').length,
    unknown: base0.filter((m) => !m.source).length,
    yours: mods.filter(yours).length,
  });
  const CHIPS = [['all', 'All'], ['updates', 'Updates'], ['off', 'Off'], ['client', 'Client-only'], ['unknown', 'Unknown']];
  const fits = {
    all: () => true, updates: (m) => !!m.update && !m.locked, off: (m) => !m.enabled,
    client: (m) => m.side === 'client', unknown: (m) => !m.source,
  };
  const SIDE_RANK = { client: 0, both: 1, server: 2 };
  const by = {
    name: (a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: 'base' }),
    author: (a, b) => (a.author || '~').localeCompare(b.author || '~'),
    side: (a, b) => (SIDE_RANK[a.side] ?? 3) - (SIDE_RANK[b.side] ?? 3),
    on: (a, b) => Number(b.enabled) - Number(a.enabled),
    added: (a, b) => (a.date_added || '').localeCompare(b.date_added || ''),
  };
  const shown = $derived.by(() => {
    const q = filter.trim().toLowerCase();
    const rows = base0.filter((m) => fits[chip](m) && (!q || `${m.name} ${m.file} ${m.author} ${(m.categories || []).join(' ')}`.toLowerCase().includes(q)));
    const cmp = by[sort.col] || by.name;
    rows.sort((a, b) => {
      if (updatesFirst) {
        const d = Number(!!(b.update && !b.locked)) - Number(!!(a.update && !a.locked));
        if (d) return d;
      }
      const c = cmp(a, b);
      return (sort.desc ? -c : c) || by.name(a, b);
    });
    return rows;
  });
  // The same mod in twice (an old jar left beside the new one): every
  // copy but the newest, as desktop's "Keep newest" finds them.
  const twice = $derived.by(() => {
    const g = new Map();
    for (const m of mods) if (m.source && m.project_id !== '0') g.set(m.project_id, [...(g.get(m.project_id) || []), m]);
    return [...g.values()].filter((l) => l.length > 1);
  });
  const older = $derived(twice.flatMap((l) => {
    const keep = [...l].sort((a, b) => Number(b.enabled) - Number(a.enabled) || (b.file_date || '').localeCompare(a.file_date || ''))[0];
    return l.filter((m) => m !== keep);
  }));
  const clientOn = $derived(mods.filter((m) => m.side === 'client' && m.enabled).length);
  const pickedMod = $derived(picked ? mods.find((m) => m.file === picked) : null);
  const selMods = $derived(mods.filter((m) => selected.has(m.file)));
  const selUpdates = $derived(selMods.filter((m) => m.update && !m.locked));
  const allShownPicked = $derived(shown.length > 0 && shown.every((m) => selected.has(m.file)));

  // Big packs have 1000+ mods: only the rows near the screen are drawn
  // (every row is ROW px tall), the rest is empty space of the same height.
  const ROW = 52;
  const EXTRA = 12;
  let rowsEl = $state();
  let win = $state({ a: 0, b: 40 });
  const visible = $derived(shown.slice(win.a, win.b));
  function measure() {
    if (!rowsEl) return;
    const sc = rowsEl.closest('.scroll');
    const view = sc ? sc.getBoundingClientRect() : { top: 0, height: innerHeight };
    const top = rowsEl.getBoundingClientRect().top - view.top;
    const a = Math.max(0, Math.floor(-top / ROW) - EXTRA);
    const b = Math.max(a + 1, Math.ceil((view.height - top) / ROW) + EXTRA);
    if (a !== win.a || b !== win.b) win = { a, b };
  }
  $effect(() => {
    if (!rowsEl) return;
    const sc = rowsEl.closest('.scroll') || window;
    const on = () => measure();
    sc.addEventListener('scroll', on, { passive: true });
    window.addEventListener('resize', on);
    measure();
    return () => {
      sc.removeEventListener('scroll', on);
      window.removeEventListener('resize', on);
    };
  });
  // The list's length or the banners above it changed: the window moves.
  $effect(() => {
    shown.length;
    selected.size;
    queueMicrotask(measure);
  });
  // The offers on the rows: the table, the review and the counts agree.
  const pendingUpdates = $derived(updates ? mods.filter((m) => m.update && !m.locked).map((m) => m.update) : []);

  async function load() {
    try {
      data = await api(`${base}/mods/list`);
      // Categories arrive from the sites in the background: look again.
      if (data.categories_pending && catTries++ < 6) setTimeout(() => sid && load(), 4000);
    } catch (e) {
      say(e.message, true);
    }
  }

  async function loadBackupCount() {
    try {
      backupCount = (await api(`${base}/mod-backups`)).sets.length;
    } catch {
      backupCount = null;
    }
  }

  async function run(label, path, body = {}) {
    try {
      const { job } = await post(`${base}/mods/${path}`, body);
      busy = label;
      return await followJob(job, (st) => (busy = st.detail || label));
    } catch (e) {
      say(e.message, true);
      return null;
    } finally {
      busy = '';
      await load();
    }
  }

  /** Identifies unknown jars; on its own once per server per visit. */
  async function identify(auto = false) {
    if (identifying) return;
    identifying = 'Identifying jars on CurseForge and Modrinth';
    try {
      const { job } = await post(`${base}/mods/identify`);
      const r = await followJob(job, (st) => (identifying = st.detail || identifying));
      if (!auto || r?.identified) say(`Identified ${r.identified}${r.unknown ? `; ${r.unknown} still unknown` : ''}`);
      await load();
      checkUpdates(false);
    } catch (e) {
      if (!auto) say(e.message, true);
    } finally {
      identifying = '';
    }
  }

  async function checkUpdates(force) {
    checking = true;
    try {
      const r = await api(`${base}/mods/updates${force ? '?force=1' : ''}`);
      updates = r.updates;
      await load();
      if (force && !updates.length) say(hasPack ? 'Every mod added on top of the pack is up to date.' : 'Every mod is up to date.');
    } catch (e) {
      if (force) say(e.message, true);
    } finally {
      checking = false;
    }
  }

  async function applyUpdates(ids) {
    updating = new Set([...updating, ...ids]);
    const r = await run(ids.length === 1 ? 'Updating' : `Updating ${ids.length} mods`, 'update', { project_ids: ids });
    updating = new Set([...updating].filter((i) => !ids.includes(i)));
    if (r) {
      const n = r.updated.length;
      const bad = r.failed?.length ? ` ${r.failed.length} failed: ${r.failed.slice(0, 3).join(', ')}${r.failed.length > 3 ? '…' : ''}.` : '';
      const web = r.web ? ` ${r.web} only download from CurseForge's website: get them on its page and drop them in mods/.` : '';
      say(`${n === 1 && ids.length === 1 ? `Updated ${r.updated[0]}` : `Updated ${n}`}.${bad}${web}${n && alive ? ' Restart the server to load them.' : ''}${n ? ' The old jars are in Backups.' : ''}`, !n);
      // The quiet green "Updated" mark, on the rows that did.
      const ok = ids.filter((i) => r.updated.includes((updates || []).find((u) => u.project_id === i)?.name));
      done = new Set([...done, ...ok]);
      reviewing = false;
      updates = null;
      checkUpdates(false);
      loadBackupCount();
    }
  }

  async function changed(big) {
    await load();
    if (big) {
      updates = null;
      checkUpdates(false);
      loadBackupCount();
    }
  }

  // The switch only moves once the change is made (a question can cancel it).
  async function toggle(m, e) {
    e?.preventDefault();
    e?.stopPropagation();
    if (e && clickSwallowed()) return;
    if (await setOn(base, [m], !m.enabled, alive)) await load();
  }

  async function bulk(kind) {
    const list = selMods;
    let ok = false;
    if (kind === 'on' || kind === 'off') ok = await setOn(base, list, kind === 'on', alive);
    else if (kind === 'lock' || kind === 'unlock') ok = await setLocked(base, list.filter((m) => m.source), kind === 'lock');
    else if (kind === 'remove') ok = await removeMods(base, list, alive);
    else if (kind === 'update') return applyUpdates(selUpdates.map((m) => m.project_id));
    if (ok) {
      if (kind === 'remove') selected = new Set();
      changed(kind !== 'on' && kind !== 'off');
    }
  }

  // Drag across rows, as in desktop: tick boxes tick as the pointer
  // crosses them; switches and Update buttons act on letting go.
  function tickDown(e, m, ix) {
    const on = !selected.has(m.file);
    startPick(e, {
      list: 'mods', kind: 'tick', ix, on, selected,
      keyAt: (i) => shown[i]?.file ?? null,
      onTick: (s) => (selected = s),
      label: tickLabel(on),
    });
  }
  function switchDown(e, m, ix) {
    const on = !m.enabled;
    // Only the switches that change.
    const keyAt = (i) => (shown[i] && shown[i].enabled !== on ? shown[i].file : null);
    startPick(e, {
      list: 'mods', kind: 'switch', ix, on, keyAt,
      label: countLabel(on ? 'Turning on' : 'Turning off', on ? 'All on' : 'All off'),
      onCommit: async (files) => {
        // All at once, so what they need is checked once, for the lot.
        if (await setOn(base, mods.filter((x) => files.includes(x.file)), on, alive)) await load();
      },
    });
  }
  function updateDown(e, m, ix) {
    startPick(e, {
      list: 'mods', kind: 'action', ix, on: true,
      keyAt: (i) => (shown[i]?.update && !shown[i].locked && !updating.has(shown[i].project_id) ? shown[i].project_id : null),
      label: countLabel('Updating', 'No updates'),
      onCommit: (ids) => applyUpdates(ids),
    });
  }
  const swSweep = $derived(sweeping('mods'));

  function check(m, e) {
    e.stopPropagation();
    if (clickSwallowed()) return e.preventDefault();
    const s = new Set(selected);
    const on = !s.has(m.file);
    if (e.shiftKey && anchor) {
      const a = shown.findIndex((x) => x.file === anchor);
      const b = shown.findIndex((x) => x.file === m.file);
      if (a >= 0 && b >= 0) for (const x of shown.slice(Math.min(a, b), Math.max(a, b) + 1)) on ? s.add(x.file) : s.delete(x.file);
    } else on ? s.add(m.file) : s.delete(m.file);
    anchor = m.file;
    selected = s;
  }

  function checkAll() {
    selected = allShownPicked ? new Set() : new Set(shown.map((m) => m.file));
  }

  function setSort(col) {
    sort = sort.col === col ? { col, desc: !sort.desc } : { col, desc: col === 'added' };
  }

  function openMenu(m, e) {
    e.preventDefault();
    e.stopPropagation();
    const w = 230;
    const h = 330;
    menu = { m, x: Math.min(e.clientX, innerWidth - w - 8), y: Math.min(e.clientY, innerHeight - h - 8) };
  }

  async function act(f) {
    const m = menu.m;
    menu = null;
    await f(m);
  }

  async function editMeta() {
    const v = await ask('Minecraft version and loader', `${data.game_version} ${data.loader}`.trim(), {
      body: 'Used to pick mod files. For example: 1.21.1 neoforge-21.1.259 (or fabric, quilt, forge-47.4.0).',
      ok: 'Save',
    });
    if (!v) return;
    const [game_version, loader = ''] = v.trim().split(/\s+/);
    try {
      await api(`${base}/mods/meta`, { method: 'PUT', body: { game_version, loader } });
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function first() {
    picked = null;
    updates = null;
    data = null;
    selected = new Set();
    done = new Set();
    catTries = 0;
    await load();
    loadBackupCount();
    const key = `hopperd-identified-${sid}`;
    let did = false;
    try {
      did = sessionStorage.getItem(key) === '1';
      sessionStorage.setItem(key, '1');
    } catch {
      // private mode: identify every visit
    }
    if (counts.unknown && !did) identify(true);
    else checkUpdates(false);
  }

  // Once per server: first() reads state, which must not
  // make this run again.
  $effect(() => {
    sid;
    untrack(first);
  });

  const SIDE = { client: [Monitor, 'Client only: players need it, the server usually doesn\'t'], server: [ServerIcon, 'Server only'], both: [ArrowLeftRight, 'Client + server'] };
  const SIDE_TAG = { client: 'Client', server: 'Server', both: 'Client + server' };
  const site = (m) => (m.source === 'modrinth' ? 'Modrinth' : 'CurseForge');
</script>

<svelte:window onclick={() => (menu = null)} onkeydown={(e) => e.key === 'Escape' && ((menu = null), (selected = new Set()))} />

<div class="card">
  <nav class="subtabs">
    <button class:on={sub === 'mods'} onclick={() => (sub = 'mods')}>Mods{data ? ` ${mods.length}` : ''}</button>
    <button class:on={sub === 'backups'} onclick={() => (sub = 'backups')}>Backups{backupCount ? ` ${backupCount}` : ''}</button>
    <span class="sp"></span>
    {#if data}
      <button class="chip meta" title={/^(neoforge|forge|fabric|quilt)/i.test(data.loader || '') ? 'Loader versions and changelog' : 'Change'} onclick={() => (/^(neoforge|forge|fabric|quilt)/i.test(data.loader || '') ? (loaderOpen = true) : editMeta())}><Settings2 size={11} />{data.game_version || 'version?'} · {data.loader || 'loader?'}</button>
      {#if data.pack}<span class="chip acc" title="Pack mods update through Update from server pack">{data.pack}</span>{/if}
    {/if}
  </nav>

  {#if sub === 'backups'}
    <ModBackups {server} onChanged={() => { load(); loadBackupCount(); }} />
  {:else}
    <div class="row wrap toolbar">
      <div class="input find"><Search size={13} /><input placeholder="Search" bind:value={filter} />{#if filter}<button class="clr" title="Clear" onclick={() => (filter = '')}><X size={12} /></button>{/if}</div>
      <a class="btn sm" href="#{browseHash('mods', server.id)}"><Plus size={13} />Add content</a>
      {#each CHIPS as [k, label] (k)}
        {#if k === 'all' || counts[k]}<button class="fchip" class:on={chip === k} onclick={() => (chip = k)}>{label}{k === 'all' ? '' : ` ${counts[k]}`}</button>{/if}
      {/each}
      {#if hasPack}<label class="cbl" title="Only what you added on top of the modpack"><input type="checkbox" bind:checked={onlyYours} />Only yours ({counts.yours})</label>{/if}
      <label class="cbl"><input type="checkbox" bind:checked={updatesFirst} />Updates first</label>
      <span class="sp"></span>
      <ModProfiles {base} {alive} onChanged={() => changed(false)} />
      {#if counts.unknown}<button class="btn sm ghost" disabled={!!identifying} onclick={() => identify(false)} title="Match them by fingerprint on CurseForge and Modrinth"><ScanSearch size={13} />Identify {counts.unknown}</button>{/if}
      {#if checking}<span class="small muted row"><span class="spin b"></span>Checking for updates</span>
      {:else if pendingUpdates.length}<button class="btn sm pri" disabled={!!busy} onclick={() => (reviewing = true)}><Download size={13} />Review {pendingUpdates.length} update{pendingUpdates.length === 1 ? '' : 's'}</button>
      {:else if updates}<span class="small okt row"><CircleCheck size={14} />Up to date</span>{/if}
      <button class="btn sm ghost icon" disabled={checking} title="Check for updates again" onclick={() => checkUpdates(true)}><RefreshCw size={13} /></button>
    </div>
    {#if identifying}<div class="banner small"><span class="spin b"></span>{identifying}… names, icons and updates fill in when it's done.</div>{/if}
    {#if busy}<div class="busy small"><span class="spin"></span>{busy}</div>{/if}
    {#if data && !data.curseforge}<CfKey inline onSaved={() => { load(); checkUpdates(true); }} />{/if}
    {#if twice.length}<div class="small warn row wrap">{twice.length} mod{twice.length === 1 ? ' is' : 's are'} in twice: {twice.map((l) => l[0].name).join(', ')}. <button class="link" onclick={async () => (await removeMods(base, older, alive)) && changed(true)}>Keep newest</button><span class="muted">(the older jars go to Backups)</span></div>{/if}
    {#if clientOn}<div class="small warn">{clientOn} client-only mod{clientOn === 1 ? '' : 's'} on: servers usually don't need {clientOn === 1 ? 'it' : 'them'}, and some won't start with them. <button class="link" onclick={() => (chip = 'client')}>Show</button></div>{/if}


    {#if selected.size}
      <div class="bulk row">
        <b>{selected.size} selected</b>
        <button class="btn sm" onclick={() => bulk('on')}>Turn on</button>
        <button class="btn sm" onclick={() => bulk('off')}>Turn off</button>
        {#if selUpdates.length}<button class="btn sm" disabled={!!busy} onclick={() => bulk('update')}><Download size={13} />Update {selUpdates.length}</button>{/if}
        {#if selMods.some((m) => m.source && !m.locked)}<button class="btn sm" onclick={() => bulk('lock')}><Lock size={13} />Ignore updates</button>{/if}
        {#if selMods.some((m) => m.locked)}<button class="btn sm" onclick={() => bulk('unlock')}><LockOpen size={13} />Allow updates again</button>{/if}
        <button class="btn sm bad" onclick={() => bulk('remove')}><Trash2 size={13} />Remove</button>
        <span class="sp"></span>
        <button class="btn sm ghost icon" title="Clear selection" onclick={() => (selected = new Set())}><X size={13} /></button>
      </div>
    {/if}

    <div class="split">
      <div class="list">
        <div class="tr th">
          <span class="c-ck"><input type="checkbox" checked={allShownPicked} onchange={checkAll} title="Select all shown" /></span>
          <span></span>
          <button class="c-name" onclick={() => setSort('name')}>Name{#if sort.col === 'name'}{#if sort.desc}<ArrowUp size={11} />{:else}<ArrowDown size={11} />{/if}{/if}</button>
          <span class="c-act"></span>
          <button class="c-author" onclick={() => setSort('author')}>Author{#if sort.col === 'author'}{#if sort.desc}<ArrowUp size={11} />{:else}<ArrowDown size={11} />{/if}{/if}</button>
          <button class="c-side" onclick={() => setSort('side')}>Side{#if sort.col === 'side'}{#if sort.desc}<ArrowUp size={11} />{:else}<ArrowDown size={11} />{/if}{/if}</button>
          <button class="c-on" onclick={() => setSort('on')}>On{#if sort.col === 'on'}{#if sort.desc}<ArrowUp size={11} />{:else}<ArrowDown size={11} />{/if}{/if}</button>
          <button class="c-added" onclick={() => setSort('added')}>Added{#if sort.col === 'added'}{#if sort.desc}<ArrowDown size={11} />{:else}<ArrowUp size={11} />{/if}{/if}</button>
          <span class="c-end"></span>
        </div>
        <div class="rows" data-pick-list="mods" bind:this={rowsEl} style:height="{shown.length * ROW}px">
        <div class="win" style:transform="translateY({win.a * ROW}px)">
        {#each visible as m, vi (m.file)}
          {@const ix = win.a + vi}
          {@const up = m.update && !m.locked ? m.update : null}
          {@const swOn = swSweep === 'switch' && swept('mods', ix) ? pick.on : m.enabled}
          {@const sel = picked === m.file || selected.has(m.file) || menu?.m === m}
          <div class="tr mod clickable" class:off={!m.enabled} class:sel class:yours={yours(m)} role="button" tabindex="0" data-pick-ix={ix}
            onclick={(e) => !clickSwallowed() && !e.target.closest('button, input, label, a') && (picked = picked === m.file ? null : m.file)}
            onkeydown={(e) => e.key === 'Enter' && (picked = m.file)}
            oncontextmenu={(e) => openMenu(m, e)}>
            <span class="c-ck"><input type="checkbox" checked={selected.has(m.file)} onpointerdown={(e) => tickDown(e, m, ix)} onclick={(e) => check(m, e)} /></span>
            {#if m.icon}<img src={m.icon} alt="" width="30" height="30" loading="lazy" />{:else}<span class="ph sm"></span>{/if}
            <div class="c-name">
              <div class="nm"><span class="ell" title={m.author ? `by ${m.author}` : m.file}>{m.name}</span>{#if m.locked}<span class="lk" title="Updates ignored. Right-click to allow them."><Lock size={11} /></span>{/if}</div>
              <div class="tags">
                {#if yours(m)}<span class="chip tag gold">Yours</span>{/if}
                {#if m.source === 'modrinth'}<span class="chip tag running">Modrinth</span>{/if}
                {#if !m.source}<span class="chip tag">Unknown</span>{/if}
                {#if m.side}<span class="chip tag" class:warnc={m.side === 'client'}>{SIDE_TAG[m.side] || m.side}</span>{/if}
                {#each (m.categories || []).slice(0, 2) as c (c)}<span class="chip tag">{c}</span>{/each}
                {#if (m.categories || []).length > 2}<span class="tiny muted">+{m.categories.length - 2}</span>{/if}
                {#if m.version}<span class="chip tag ver ell" title={m.version}>{m.version}</span>{/if}
              </div>
            </div>
            <div class="c-act">
              {#if done.has(m.project_id) && !up}<span class="okt small row"><CircleCheck size={13} />Updated</span>
              {:else if up}
                <div class="splitb" title="{m.update.from} → {m.update.file}">
                  <button class="lead" class:lit={swSweep === 'action' && swept('mods', ix)} disabled={updating.has(m.project_id) || !!busy} onpointerdown={(e) => updateDown(e, m, ix)} onclick={() => !clickSwallowed() && applyUpdates([m.project_id])}>{#if updating.has(m.project_id)}<span class="spin"></span>Updating{:else}<Download size={12} />Update{/if}</button>
                  <button onclick={() => (versionsFor = m)}>What changed</button>
                </div>
              {:else if m.source}
                <button class="btn sm ghostcl" onclick={() => (versionsFor = m)} title="Versions and changelogs"><ScrollText size={12} />Changelog</button>
              {/if}
            </div>
            <span class="c-author muted ell" title={m.author}>{m.author}</span>
            <span class="c-side muted" title={SIDE[m.side]?.[1] || 'Not known'}>{#if SIDE[m.side]}{@const I = SIDE[m.side][0]}<I size={13} />{/if}</span>
            <span class="c-on"><label class="sw" class:pend={swOn !== m.enabled} title={m.enabled ? 'Turn off' : 'Turn on'} onpointerdown={(e) => switchDown(e, m, ix)}><input type="checkbox" checked={swOn} onclick={(e) => toggle(m, e)} /><span></span></label></span>
            <span class="c-added muted" title={m.date_added ? new Date(m.date_added).toLocaleString() : ''}>{ago(m.date_added)}</span>
            <span class="c-end row">
              <button class="ib more" title="More" onclick={(e) => openMenu(m, e)}><Ellipsis size={14} /></button>
              <button class="ib del" title="Remove (kept in .hopper-backups)" onclick={async (e) => { e.stopPropagation(); if (await removeMods(base, [m], alive)) changed(true); }}><Trash2 size={13} /></button>
            </span>
          </div>
        {/each}
        </div>
        </div>
        {#if !shown.length}
          <p class="small muted pad">{data ? (mods.length ? 'No mod fits the search and filters.' : 'Nothing installed here yet.') : 'Loading…'}</p>
        {/if}
      </div>
      {#if pickedMod}<ModPeek {server} mod={pickedMod} onClose={() => (picked = null)} onChanged={changed} onUpdate={(m) => applyUpdates([m.project_id])} />{/if}
    </div>
  {/if}
</div>

{#if menu}
  {@const m = menu.m}
  <div class="ctx card" style:left="{menu.x}px" style:top="{menu.y}px" role="menu" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={() => {}}>
    {#if m.source}<button onclick={() => act((m) => (versionsFor = m))}><Clock size={13} />Versions and changelogs</button>{/if}
    {#if m.page}<a href={m.page} target="_blank" rel="noreferrer" onclick={() => (menu = null)}><ExternalLink size={13} />Open on {site(m)}</a>{/if}
    {#if m.source}<button onclick={() => act(async (m) => (await switchSource(base, m, alive, (s) => (busy = s))) && changed(true))}><Shuffle size={13} />Switch to {m.source === 'modrinth' ? 'CurseForge' : 'Modrinth'}</button>{/if}
    {#if m.source}<button onclick={() => act(async (m) => (await setLocked(base, [m], !m.locked)) && changed(true))}>{#if m.locked}<LockOpen size={13} />Allow updates{:else}<Lock size={13} />Ignore updates{/if}</button>{/if}
    {#if m.source}<button onclick={() => act((m) => resetConfig(base, m))}><Settings2 size={13} />Reset config…</button>{/if}
    <button onclick={() => act(() => (location.hash = `/server/${encodeURIComponent(server.id)}/files/mods`))}><FolderOpen size={13} />Show in Files</button>
    <button onclick={() => act((m) => toggle(m))}><span class="noi"></span>{m.enabled ? 'Disable' : 'Enable'}</button>
    <hr />
    <button class="bad" onclick={() => act(async (m) => (await removeMods(base, [m], alive)) && changed(true))}><Trash2 size={13} />Remove</button>
  </div>
{/if}

{#if loaderOpen}<LoaderVersions {server} onClose={() => (loaderOpen = false)} onChanged={() => changed(true)} />{/if}
{#if versionsFor}<ModVersions {server} mod={versionsFor} dialog onClose={() => (versionsFor = null)} onChanged={changed} />{/if}
{#if reviewing && pendingUpdates.length}<ReviewUpdates {server} updates={pendingUpdates} {mods} {busy} onApply={applyUpdates} onClose={() => (reviewing = false)} />{/if}

<style>
  .card { display: flex; flex-direction: column; gap: 8px; }
  .subtabs { display: flex; align-items: center; gap: 4px; border-bottom: 1px solid var(--line); padding-bottom: 8px; }
  .subtabs > button:not(.chip) { padding: 4px 12px; border-radius: 99px; color: var(--subtle); border: 1px solid transparent; }
  .subtabs > button.on { background: var(--raise); color: var(--text); border-color: var(--line2); font-weight: 600; }
  .toolbar { gap: 6px; }
  .fchip { padding: 3px 10px; border-radius: 99px; border: 1px solid transparent; color: var(--subtle); font-size: 12px; }
  .fchip:hover { background: #ffffff0a; }
  .fchip.on { background: var(--raise); color: var(--text); }
  .cbl { display: inline-flex; align-items: center; gap: 6px; font-size: 12.5px; color: var(--subtle); cursor: pointer; padding: 0 4px; }
  .cbl input, .c-ck input { width: 15px; height: 15px; accent-color: var(--acc); }
  .meta { gap: 4px; }
  .find { padding: 3px 8px; width: 220px; font-weight: 400; }
  .clr { color: var(--muted); display: grid; }
  .grow { flex: 1; min-width: 0; }
  .wrap { flex-wrap: wrap; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .warn { color: var(--gold); }
  .okt { color: var(--green); gap: 5px; }
  .link { color: var(--acc); text-decoration: underline; font-size: 12px; }
  .banner { display: flex; align-items: center; gap: 8px; border: 1px solid #60a5fa44; background: var(--blue-dim); border-radius: 8px; padding: 6px 10px; color: var(--subtle); }
  .busy { display: flex; align-items: center; gap: 8px; color: var(--gold); }
  .spin { width: 11px; height: 11px; border: 2px solid var(--gold); border-right-color: transparent; border-radius: 50%; animation: s 0.8s linear infinite; flex: none; display: inline-block; }
  .spin.b { border-color: var(--blue); border-right-color: transparent; }
  @keyframes s { to { transform: rotate(360deg); } }
  .bulk { gap: 6px; background: var(--raise); border: 1px solid var(--line2); border-radius: 8px; padding: 6px 10px; flex-wrap: wrap; }
  .bad { color: var(--red); }
  .split { display: flex; gap: 12px; align-items: flex-start; }
  .list { flex: 1; min-width: 0; container-type: inline-size; }
  /* Desktop's columns: check, icon, name, Update / Changelog, Author, Side, On, Added, menu. */
  .tr { display: grid; grid-template-columns: 22px 30px minmax(0, 1fr) 190px 130px 40px 44px 86px 52px; gap: 10px; align-items: center; padding: 6px 8px; position: relative; border-radius: 8px; }
  .th { color: var(--muted); font-size: 11px; padding-top: 2px; padding-bottom: 6px; border-bottom: 1px solid var(--line); border-radius: 0; }
  .th button { display: inline-flex; align-items: center; gap: 3px; color: var(--muted); font-size: 11.5px; text-align: left; }
  .th button:hover { color: var(--text); }
  .mod { cursor: pointer; height: 52px; overflow: hidden; }
  .rows { position: relative; }
  .win { will-change: transform; }
  .mod:hover { background: #ffffff08; }
  .mod.sel { background: var(--acc-dim); box-shadow: inset 3px 0 0 var(--acc); }
  .mod.yours::before { content: ''; position: absolute; left: 0; top: 10px; bottom: 10px; width: 2px; border-radius: 2px; background: var(--gold); }
  .mod.sel.yours::before { display: none; }
  .mod.off { opacity: 0.55; }
  .mod img { border-radius: 6px; background: var(--raise); flex: none; }
  .nm { font-weight: 500; display: flex; gap: 6px; align-items: center; min-width: 0; }
  .lk { color: var(--muted); display: grid; }
  .tags { display: flex; gap: 4px; align-items: center; margin-top: 2px; min-width: 0; overflow: hidden; flex-wrap: nowrap; }
  .tag { font-size: 10.5px; padding: 0 6px; flex: none; }
  .tag.gold { color: var(--gold); background: var(--gold-dim); }
  .tag.warnc { color: var(--gold); }
  .tag.ver { flex: 0 1 auto; max-width: 160px; display: inline-block; }
  .c-act { display: flex; justify-content: flex-start; }
  .c-on { display: flex; }
  .c-end { gap: 2px; justify-content: flex-end; }
  .ib { color: var(--muted); padding: 3px; border-radius: 5px; display: grid; }
  .ib:hover { color: var(--text); background: #ffffff10; }
  .ib.del:hover { color: var(--red); }
  /* Hover-only controls stay shown on the picked row. */
  .mod .more, .mod .ghostcl { visibility: hidden; }
  .mod:hover .more, .mod:hover .ghostcl, .mod.sel .more, .mod.sel .ghostcl { visibility: visible; }
  .ghostcl { color: var(--subtle); gap: 5px; }
  .splitb { display: inline-flex; border: 1px solid var(--line2); border-radius: 6px; overflow: hidden; font-size: 12px; max-width: 100%; }
  .splitb button { padding: 3px 8px; display: inline-flex; align-items: center; gap: 5px; white-space: nowrap; color: var(--subtle); }
  .splitb button:hover:not(:disabled) { background: #ffffff10; color: var(--text); }
  .splitb .lead { color: var(--text); border-right: 1px solid var(--line2); }
  .splitb button:disabled { opacity: 0.7; }
  .splitb .lead.lit { background: var(--acc-dim); color: var(--acc); }
  .sw.pend span { outline: 2px dashed var(--acc); outline-offset: 2px; }
  @container (max-width: 1050px) { .tr { grid-template-columns: 22px 30px minmax(0, 1fr) 190px 40px 44px 86px 52px; } .tr .c-author { display: none; } }
  @container (max-width: 860px) { .tr { grid-template-columns: 22px 30px minmax(0, 1fr) 190px 40px 44px 52px; } .tr .c-added { display: none; } }
  @container (max-width: 640px) { .tr { grid-template-columns: 22px 30px minmax(0, 1fr) 110px 44px 52px; } .tr .c-side { display: none; } .splitb button:not(.lead) { display: none; } .ghostcl { font-size: 0; gap: 0; } }
  .ph { width: 34px; height: 34px; border-radius: 6px; background: var(--raise); flex: none; }
  .ph.sm { width: 30px; height: 30px; }
  .pad { padding: 8px 4px; }
  .icon { padding: 3px 5px; }
  .sw { position: relative; width: 30px; height: 17px; flex: none; cursor: pointer; }
  .sw input { opacity: 0; width: 0; height: 0; }
  .sw span { position: absolute; inset: 0; background: var(--line2); border-radius: 9px; transition: 0.15s; }
  .sw span::after { content: ''; position: absolute; width: 13px; height: 13px; border-radius: 50%; background: #fff; top: 2px; left: 2px; transition: 0.15s; }
  .sw input:checked + span { background: var(--acc); }
  .sw input:checked + span::after { left: 15px; }
  .ctx { position: fixed; z-index: 80; width: 230px; padding: 4px; display: flex; flex-direction: column; box-shadow: 0 14px 40px #000c; }
  .ctx button, .ctx a { display: flex; align-items: center; gap: 8px; padding: 6px 10px; border-radius: 5px; font-size: 12.5px; text-align: left; color: var(--text); text-decoration: none; }
  .ctx button:hover, .ctx a:hover { background: var(--raise); }
  .ctx .bad { color: var(--red); }
  .ctx hr { border: 0; border-top: 1px solid var(--line); margin: 4px 0; width: 100%; }
  .noi { width: 13px; }
  .ctx button:disabled { opacity: 0.5; }
  .sw.dis { cursor: default; opacity: 0.6; }
</style>
