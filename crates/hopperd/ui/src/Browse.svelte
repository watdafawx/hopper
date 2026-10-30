<script>
  // Browse like Hopper desktop's Browse: a filter column (site, type,
  // install into, Minecraft, loader, category), sorts right of the count,
  // one card per project. Modpacks make a new server; mods and data packs
  // install into the server picked under "Install into", whose Minecraft
  // and loader set the filters, and what it already has shows "Installed".
  //
  // Routes (Shell hands us the hash's segments after #/browse):
  //   #/browse                                modpacks
  //   #/browse/<type>/<server id or ->        type: modpacks, mods, datapacks
  //   #/browse/<type>/<server>/<site>/<id>    a project's page
  //   #/browse/<site>/<id>                    a modpack's page (older links)
  // A project's page opens over the list, which stays mounted underneath,
  // so Back keeps its filters and scroll.
  import { onMount, untrack } from 'svelte';
  import { Search, Flame, Sparkles, RefreshCw, Download, ArrowDownAZ, Server, Hammer, SlidersHorizontal, X, SearchX, KeyRound, Package, Puzzle, Braces, Check, Monitor, Layers, Tag, Star, Plus } from '@lucide/svelte';
  import { loaderText } from './lib/format.js';
  import { api, post } from './lib/api.js';
  import { session } from './lib/me.svelte.js';
  import { live } from './lib/live.svelte.js';
  import { say } from './lib/toast.svelte.js';
  import { followJob } from './lib/jobs.js';
  import { SITE, TYPES, count, ago, preset, store, browseHash, andList } from './lib/browse.js';
  import BrowsePack from './BrowsePack.svelte';
  import BrowseMenu from './BrowseMenu.svelte';
  import NewServer from './NewServer.svelte';

  let { segs = [] } = $props();

  // Desktop's sorts, in its order and words.
  const SORTS = [
    ['popularity', 'Popularity', Flame],
    ['featured', 'Featured', Star],
    ['downloads', 'Downloads', Download],
    ['updated', 'Last updated', RefreshCw],
    ['newest', 'Newest', Sparkles],
    ['name', 'Name', ArrowDownAZ],
  ];
  const LOADERS = [['', 'Any loader'], ['neoforge', 'NeoForge'], ['forge', 'Forge'], ['fabric', 'Fabric'], ['quilt', 'Quilt']];
  const ICON = { modpacks: Package, mods: Puzzle, datapacks: Braces };
  const isSite = (s) => s === 'curseforge' || s === 'modrinth';

  // What the hash says: the list (type, target) and the open project.
  const legacy = $derived(isSite(segs[0]));
  const hType = $derived(!legacy && TYPES[segs[0]] ? segs[0] : 'modpacks');
  const hInto = $derived(!legacy && segs[1] && segs[1] !== '-' ? segs[1] : '');
  const openSite = $derived(legacy ? segs[0] : isSite(segs[2]) ? segs[2] : null);
  const openId = $derived(!openSite ? '' : legacy ? segs[1] || '' : segs[3] || '');

  let type = $state('modpacks');
  let into = $state('');
  let target = $state(null); // { id, name, game_version, loader, loader_name, running, datapack_dirs }
  let targetError = $state('');
  let dest = $state('');

  let source = $state(store('browse.source') || 'both');
  let query = $state('');
  let mc = $state('');
  let loader = $state('');
  let sort = $state(store('browse.sort') || 'popularity');
  let category = $state(null); // { name, cf, mr }
  let mcVersions = $state([]);
  let cats = $state({}); // type → { curseforge, modrinth, both }
  let cfKey = $state(true);

  let results = $state([]);
  let page = $state(0);
  let more = $state(false);
  let total = $state(0);
  let warnings = $state([]);
  let loading = $state(false);
  let error = $state('');
  let showFilters = $state(false);
  let making = $state(null);
  let busy = $state({}); // source:id → what the install is doing
  // What installing a mod into the target also downloads, by server and
  // source:id (asked when its card is hovered, like desktop): 'loading',
  // the plan, or { error }.
  let plans = $state({});
  let menu = $state(null); // a card's right-click menu: { r, x, y }
  let listEl = $state();
  let sentinel = $state();
  let seq = 0;

  const T = $derived(TYPES[type]);
  const intoServer = $derived(T.into);
  const modded = $derived(!target || !!target.loader);
  // Types a server can use: no loader, no mods.
  const typeList = $derived(Object.entries(TYPES).filter(([k]) => k !== 'mods' || modded || type === 'mods'));
  const useLoader = $derived(type !== 'datapacks');
  const typeCats = $derived(cats[type] || { curseforge: [], modrinth: [], both: [] });
  // Categories for the picked site; Both shows the ones both sites have.
  const catList = $derived(
    source === 'curseforge'
      ? typeCats.curseforge.map((c) => ({ name: c.name, cf: c.id, mr: '', icon: c.icon }))
      : source === 'modrinth'
        ? typeCats.modrinth.map((c) => ({ name: c.name, cf: '', mr: c.id }))
        : typeCats.both,
  );
  const both = $derived(source === 'both');
  const filterText = $derived([mc, useLoader ? LOADERS.find((l) => l[0] === loader)?.[1] : ''].filter((x) => x && x !== 'Any loader').join(' · '));
  const servers = $derived([...live.servers].sort((a, b) => a.name.localeCompare(b.name)));

  function params(p) {
    const u = new URLSearchParams({ q: query, source, sort, page: String(p), type });
    if (mc) u.set('mc', mc);
    if (loader && useLoader) u.set('loader', loader);
    if (category?.cf && source !== 'modrinth') u.set('cat_cf', category.cf);
    if (category?.mr && source !== 'curseforge') u.set('cat_mr', category.mr);
    if (intoServer && target) u.set('into', target.id);
    return u;
  }

  async function run(append = false) {
    const my = ++seq;
    // A search now replaces one still waiting on the typing pause.
    if (!append) clearTimeout(timer);
    const p = append ? page + 1 : 0;
    loading = true;
    error = '';
    if (!append) {
      results = [];
      saved = 0;
    }
    try {
      const r = await api(`/browse/search?${params(p)}`);
      if (my !== seq) return;
      results = append ? [...results, ...r.results] : r.results;
      page = p;
      more = r.more;
      total = r.total;
      warnings = r.warnings || [];
      cfKey = r.curseforge;
    } catch (e) {
      if (my !== seq) return;
      error = e.message;
      more = false;
    } finally {
      if (my === seq) loading = false;
    }
  }

  let timer;
  function soon() {
    clearTimeout(timer);
    timer = setTimeout(() => run(false), 350);
  }

  function loadCats(t) {
    if (cats[t]) return;
    api(`/browse/categories?type=${t}`).then((r) => (cats = { ...cats, [t]: r }), () => {});
  }

  /** The server to install into: its Minecraft and loader set the filters. */
  async function loadTarget(id) {
    targetError = '';
    if (!id) {
      target = null;
      return;
    }
    try {
      const t = await api(`/browse/target?into=${encodeURIComponent(id)}`);
      if (into !== id) return;
      target = t;
      mc = t.game_version || '';
      loader = t.loader || '';
      dest = t.datapack_dirs?.[0]?.path || '';
    } catch (e) {
      target = null;
      targetError = e.message;
    }
  }

  // The list follows the hash's type and server; opening a project (and
  // Back) leaves them alone, so the list isn't searched again.
  let listKey = '';
  $effect(() => {
    const t = hType;
    const i = hInto;
    const k = `${t}|${i}`;
    if (k === listKey) return;
    listKey = k;
    untrack(async () => {
      const typeChanged = t !== type;
      type = t;
      if (typeChanged) category = null;
      loadCats(t);
      if (i !== into || (i && !target)) {
        // No server any more: its Minecraft and loader stop filtering.
        if (!i && into) {
          mc = '';
          loader = '';
        }
        into = i;
        if (i) store('browse.into', i);
        await loadTarget(i);
      }
      run(false);
    });
  });

  function go(t, i) {
    location.hash = browseHash(t, i);
  }

  function setType(t) {
    if (t === type) return;
    // Modpacks make a new server, so they drop the target; the last one
    // picked comes back for Mods and Data packs.
    go(t, TYPES[t].into ? into || store('browse.into') || '' : '');
  }

  function setInto(id) {
    go(type, id);
  }

  function setSource(s) {
    if (s === source) return;
    source = s;
    store('browse.source', s);
    // Keep the category when the other site has one of the same name.
    if (category) {
      const name = category.name.toLowerCase();
      const list = s === 'curseforge' ? typeCats.curseforge.map((c) => ({ name: c.name, cf: c.id, mr: '' })) : s === 'modrinth' ? typeCats.modrinth.map((c) => ({ name: c.name, cf: '', mr: c.id })) : typeCats.both;
      category = list.find((c) => (category.cf && c.cf === category.cf) || (category.mr && c.mr === category.mr) || c.name.toLowerCase() === name) || null;
    }
    run(false);
  }

  function setSort(s) {
    if (s === sort) return;
    sort = s;
    store('browse.sort', s);
    run(false);
  }

  /** Back to the server's own Minecraft and loader (or none). */
  function clearFilters() {
    query = '';
    mc = target?.game_version || '';
    loader = target?.loader || '';
    category = null;
    run(false);
  }

  function openMenu(e, r) {
    e.preventDefault();
    wantPlan(r);
    menu = { r, x: e.clientX, y: e.clientY };
  }

  function open(r) {
    saved = scroller()?.scrollTop || 0;
    location.hash = `${browseHash(type, into)}/${r.source}/${encodeURIComponent(r.id)}`;
  }

  /** Asks once what installing mod `r` into the target would also download. */
  function wantPlan(r) {
    if (type !== 'mods' || !target?.loader || r.installed) return;
    const key = `${target.id}|${r.source}:${r.id}`;
    if (plans[key]) return;
    plans = { ...plans, [key]: 'loading' };
    api(`/browse/plan?into=${encodeURIComponent(target.id)}&project=${r.num}`).then(
      (p) => (plans = { ...plans, [key]: p }),
      (e) => (plans = { ...plans, [key]: { error: e.message } }),
    );
  }
  /** Whether a card's Install can run now (the button and its menu item). */
  const canAdd = (r) => !!target && !(type === 'mods' && !target.loader) && r.distribution !== false;
  const planOf = (r) => (target ? plans[`${target.id}|${r.source}:${r.id}`] : null);

  /** A card's category in the filter list (by name), so a click filters by it. */
  function catOf(name) {
    const n = name.toLowerCase();
    return catList.find((c) => c.name.toLowerCase() === n) || null;
  }

  /**
   * Installs `r` (a card or a project page; `v` a picked version) into the
   * target. Resolves with the job's result, or null when it failed.
   */
  async function install(r, v = null) {
    if (!target) {
      say('Pick a server to install into first.', true);
      return null;
    }
    const key = `${r.source}:${r.id}`;
    if (busy[key]) return null;
    busy = { ...busy, [key]: 'Starting…' };
    try {
      const { job } = await post('/browse/install', { into: target.id, type, project: r.num, file: v?.id || null, dest });
      const res = await followJob(job, (st) => (busy = { ...busy, [key]: st.detail || 'Adding…' }));
      results = results.map((x) => (x.source === r.source && x.id === r.id ? { ...x, installed: { same_project: true, enabled: x.installed?.enabled ?? true, from: r.source } } : x));
      // What other mods would pull in may be on the server now: ask again.
      plans = {};
      const where = type === 'datapacks' ? (target.datapack_dirs.find((d) => d.path === dest)?.label || dest) : target.name;
      say(`Added ${res?.name || r.name}${v ? ` ${v.name}` : ''} to ${where}.${res?.restart_needed ? ' Restart the server to load it.' : ''}`);
      return res || {};
    } catch (e) {
      say(e.message, true);
      return null;
    } finally {
      const { [key]: _, ...rest } = busy;
      busy = rest;
    }
  }

  // Load the next page when the end of the list comes into view; watching
  // again after each page asks once more if it's still in view.
  const io = new IntersectionObserver((es) => {
    if (es.some((e) => e.isIntersecting) && more && !loading && !error && !openSite) run(true);
  }, { rootMargin: '400px' });
  $effect(() => {
    results.length;
    const el = sentinel;
    if (!el || loading || openSite) return;
    io.observe(el);
    return () => io.unobserve(el);
  });

  // The list keeps its scroll while a project's page is open.
  let saved = 0;
  const scroller = () => listEl?.closest('.content');
  $effect(() => {
    if (openSite) return;
    const y = saved;
    requestAnimationFrame(() => scroller()?.scrollTo({ top: y }));
  });

  onMount(() => {
    api('/meta/minecraft').then((r) => (mcVersions = r.versions || []), () => {});
    return () => io.disconnect();
  });
</script>

<div class="browse" class:hidden={!!openSite}>
  <div class="page-h row">
    <h2>Browse {T.label.toLowerCase()}</h2>
    {#if intoServer && target}<span class="chip acc" title="Installs go here">into {target.name}</span>{/if}
    <span class="sp"></span>
    <button class="btn sm ghost filt" onclick={() => (showFilters = !showFilters)}><SlidersHorizontal size={13} />Filters{filterText || category ? ' ·' : ''} {[filterText, category?.name].filter(Boolean).join(' · ')}</button>
  </div>
  <div class="cols">
    <aside class="filters" class:open={showFilters}>
      <div class="seg site">
        <button class:on={both} onclick={() => setSource('both')} title={cfKey ? 'CurseForge and Modrinth together' : 'Modrinth only until a CurseForge key is added'}>Both</button>
        <button class="cf" class:on={source === 'curseforge'} disabled={!cfKey} title={cfKey ? '' : 'Needs a CurseForge API key (Access & devices)'} onclick={() => setSource('curseforge')}>CurseForge</button>
        <button class="mr" class:on={source === 'modrinth'} onclick={() => setSource('modrinth')}>Modrinth</button>
      </div>
      <span class="lbl">Type</span>
      <div class="cats">
        {#each typeList as [k, t] (k)}
          {@const Icon = ICON[k]}
          <button class="cat row-hover" class:row-sel={type === k} onclick={() => setType(k)}><Icon size={14} />{t.label}</button>
        {/each}
      </div>
      {#if intoServer}
        <label class="lbl" for="br-into">Install into</label>
        <select id="br-into" class="sel" value={into} onchange={(e) => setInto(e.currentTarget.value)}>
          <option value="">Pick a server</option>
          {#if into && !servers.some((s) => s.id === into)}<option value={into}>{target?.name || into}</option>{/if}
          {#each servers as s (s.id)}<option value={s.id}>{s.name}{s.version ? ` · ${s.version}` : ''}{s.loader ? ` ${loaderText(s.loader)}` : ''}</option>{/each}
        </select>
        {#if targetError}<div class="tiny err">{targetError}</div>{/if}
        {#if type === 'datapacks' && target?.datapack_dirs?.length > 1}
          <label class="lbl" for="br-dest">Data packs go to</label>
          <select id="br-dest" class="sel" bind:value={dest}>
            {#each target.datapack_dirs as d (d.path)}<option value={d.path}>{d.label}</option>{/each}
          </select>
        {/if}
      {/if}
      <label class="lbl" for="br-mc">Minecraft</label>
      <select id="br-mc" class="sel" bind:value={mc} onchange={() => run(false)}>
        <option value="">Any version</option>
        {#if mc && !mcVersions.includes(mc)}<option value={mc}>{mc}</option>{/if}
        {#each mcVersions as v (v)}<option value={v}>{v}</option>{/each}
      </select>
      {#if useLoader}
        <label class="lbl" for="br-loader">Loader</label>
        <select id="br-loader" class="sel" bind:value={loader} onchange={() => run(false)}>
          {#each LOADERS as [k, label] (k)}<option value={k}>{label}</option>{/each}
        </select>
      {/if}
      {#if intoServer && target && (mc !== (target.game_version || '') || (useLoader && loader !== (target.loader || '')))}
        <button class="btn sm ghost reset" onclick={() => { mc = target.game_version || ''; loader = target.loader || ''; run(false); }}>Use {target.name}'s {[target.game_version, useLoader ? target.loader_name : ''].filter(Boolean).join(' ')}</button>
      {/if}
      <span class="lbl">Category</span>
      <div class="cats">
        <button class="cat row-hover" class:row-sel={!category} onclick={() => { category = null; run(false); }}>All</button>
        {#each catList as c (c.name)}
          <button class="cat row-hover" class:row-sel={category?.name === c.name} onclick={() => { category = c; run(false); }}>
            {#if c.icon}<img src={c.icon} alt="" width="14" height="14" loading="lazy" />{/if}{c.name}
          </button>
        {/each}
      </div>
      {#if both && catList.length}<div class="tiny muted note">Both shows the categories the two sites share. Pick one site for all of its own.</div>{/if}
    </aside>

    <div class="results" bind:this={listEl}>
      <div class="input"><Search size={14} /><input bind:value={query} oninput={soon} onkeydown={(e) => e.key === 'Enter' && (clearTimeout(timer), run(false))} placeholder={both ? `Search ${T.label.toLowerCase()} on CurseForge and Modrinth` : `Search ${T.label.toLowerCase()} on ${SITE[source]}`} />
        {#if query}<button class="clr" title="Clear" onclick={() => { query = ''; run(false); }}><X size={13} /></button>{/if}</div>
      {#if intoServer && !into}
        <div class="small muted row note-bar"><Server size={13} />Pick a server under Install into to add {T.label.toLowerCase()} to it; its Minecraft and loader set the filters.</div>
      {:else if type === 'mods' && target && !target.loader}
        <div class="small warn row note-bar">{target.name} has no mod loader, so it can't run mods. Data packs work on any server.
          <button class="btn sm" onclick={() => setType('datapacks')}>Browse data packs</button></div>
      {/if}
      <div class="row countbar">
        <span class="small subtle">{loading && !results.length ? 'Searching…' : `${total >= 10000 ? '10,000+' : total.toLocaleString()} ${total === 1 ? T.one : T.many}`}{#if !both}<span class="muted">{' on '}</span><span class={source}>{SITE[source]}</span>{/if}{filterText ? ` · ${filterText}` : ''}</span>
        <span class="sp"></span>
        <div class="sorts"><span class="lbl">Sort</span>
          {#each SORTS as [k, label, Icon] (k)}<button class:on={sort === k} title={label} onclick={() => setSort(k)}><Icon size={13} /><span class="sl">{label}</span></button>{/each}
        </div>
      </div>
      {#if type === 'mods' && target?.pack_source && !both && source !== target.pack_source}
        <div class="small warn row note-bar">You're browsing {SITE[source]}. {target.name} is a {SITE[target.pack_source]} pack: mods installed from here (and what they need) come from {SITE[source]}.
          <button class="btn sm" onclick={() => setSource(target.pack_source)}>Switch to {SITE[target.pack_source]}</button></div>
      {/if}
      {#each warnings.filter((w) => w !== 'nokey') as w (w)}<div class="small warn">{w}</div>{/each}
      {#if warnings.includes('nokey') && both}<div class="small muted row"><KeyRound size={13} />Showing Modrinth only: add a CurseForge API key in Access &amp; devices to search both.</div>{/if}

      <div class="list">
        {#each results as r (r.source + ':' + r.id)}
          {@const b = busy[r.source + ':' + r.id]}
          {@const plan = planOf(r)}
          {@const extra = plan?.extra?.map((d) => d.name) || []}
          {@const have = r.installed}
          <div class="rc" role="button" tabindex="0" onclick={() => open(r)} onmouseenter={() => wantPlan(r)} oncontextmenu={(e) => openMenu(e, r)} onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && e.target === e.currentTarget && (e.preventDefault(), open(r))}>
            {#if r.icon}<img class="ico" src={r.icon} alt="" width="48" height="48" loading="lazy" decoding="async" />{:else}<span class="ico ph"></span>{/if}
            <div class="body">
              <div class="t"><b>{r.name}</b>{#if r.author}<span class="muted">{` by ${r.author}`}</span>{/if}</div>
              <div class="s ell">{r.summary}</div>
              <div class="meta">
                <span class="dl"><Download size={11} />{count(r.downloads)}</span>
                {#if ago(r.updated)}<span class="up"><RefreshCw size={11} />{ago(r.updated)}</span>{/if}
                {#if both}<span class="chip {r.source}">{SITE[r.source]}</span>{#if r.also_on}<span class="chip {r.also_on}" title="Also on {SITE[r.also_on]}; this page shows the {SITE[r.source]} one">{SITE[r.also_on]}</span>{/if}{/if}
                {#if useLoader}{#each r.loaders.slice(0, 2) as l (l)}<span class="chip lan">{l}</span>{/each}{/if}
                {#if r.mc_versions[0]}<span class="chip">{r.mc_versions[0]}</span>{/if}
                {#if r.categories.length}
                  {@const cs = r.categories.slice(0, 3)}
                  <span class="catline ell"><Tag size={11} />{#each cs as c, i (c)}{@const cat = catOf(c)}{#if cat}<button class="catlink" title="Show only {c}" onclick={(e) => { e.stopPropagation(); category = cat; run(false); }}>{c}</button>{:else}<span>{c}</span>{/if}{i + 1 < cs.length ? ', ' : ''}{/each}</span>
                {/if}
              </div>
            </div>
            <div class="act">
              {#if type === 'modpacks'}
                {#if r.server_files === true}<span class="chip running" title="The pack has server files: the mods its author picked for servers"><Server size={11} />Server files</span>
                {:else if r.server_files === false}<span class="chip starting" title="No server files: Hopper builds the server from the client pack and leaves client-only mods out"><Hammer size={11} />Built from client pack</span>{/if}
                {#if session.owner}<button class="btn sm" onclick={(e) => { e.stopPropagation(); making = preset(r); }}>Make a server</button>{/if}
              {:else}
                {#if r.side === 'client'}<span class="chip starting" title="Modrinth says this runs on the client only: a server doesn't need it"><Monitor size={11} />Client only</span>{/if}
                {#if have && have.enabled === false}<span class="chip starting" title="Off in {target?.name}. Updating keeps it off.">Off</span>{/if}
                {#if b}<span class="busy small"><span class="spinner"></span><span class="ell">{b}</span></span>
                {:else if have?.update}<button class="btn sm" title={have.enabled === false ? 'A newer file is out; replaces the installed one, which stays disabled' : 'A newer file is out; replaces the installed one'} onclick={(e) => { e.stopPropagation(); install(r); }}><Download size={13} />Update</button>
                {:else if have}<span class="chip running" title="{target?.name} has this already{have.file ? `: ${have.file}` : ''}"><Check size={11} />{have.same_project === false && have.from && have.from !== r.source ? `Installed (${SITE[have.from]})` : 'Installed'}</span>
                {:else}
                  {#if extra.length}<span class="deps" title="Also downloads {andList(extra)}"><Layers size={11} />+{extra.length} {extra.length === 1 ? 'dep' : 'deps'}</span>{/if}
                  <button class="btn sm" disabled={!canAdd(r)} title={!target ? 'Pick a server under Install into first' : r.distribution === false ? 'The author only allows downloads from the CurseForge website' : extra.length ? `Also downloads ${andList(extra)}` : `Add the newest file for ${[target.game_version, useLoader ? target.loader_name : ''].filter(Boolean).join(' ')}`} onclick={(e) => { e.stopPropagation(); install(r); }}><Download size={13} />Install</button>
                {/if}
              {/if}
            </div>
          </div>
        {/each}
        {#if loading}
          {#each Array(results.length ? 2 : 6) as _, i (i)}<div class="skel csk" style:opacity={1 - i * 0.12}><i></i></div>{/each}
        {:else if error}
          <div class="empty-state"><span class="es-ic"><SearchX size={18} /></span><b>Couldn't load results</b><p>Check your connection and try again. ({error})</p><button class="btn sm" onclick={() => run(false)}>Retry</button></div>
        {:else if !results.length}
          <div class="empty-state"><span class="es-ic"><SearchX size={18} /></span><b>No {T.many} match</b><p>Nothing{query ? ` for “${query}”` : ''}{filterText ? ` on ${filterText}` : ''}{category ? ` in ${category.name}` : ''}.</p>
            {#if query || category || mc !== (target?.game_version || '') || loader !== (target?.loader || '')}<button class="btn sm" onclick={clearFilters}>Clear filters</button>{/if}</div>
        {/if}
        <div bind:this={sentinel} class="sentinel"></div>
      </div>
    </div>
  </div>
</div>

{#if openSite && type === hType && into === hInto && (!intoServer || !into || target || targetError)}
  {#key openSite + openId}
    <BrowsePack source={openSite} id={openId} {type} target={intoServer ? target : null} {mc} loader={useLoader ? loader : ''}
      back={browseHash(type, into)} busy={busy[openSite + ':' + openId]} onMake={(p) => (making = p)} onInstall={install}
      canFilter={(n) => !!catOf(n)} onCategory={(n) => { category = catOf(n); saved = 0; location.hash = browseHash(type, into); run(false); }} />
  {/key}
{/if}
{#if making}<NewServer preset={making} close={() => (making = null)} />{/if}
{#if menu && !openSite}
  {@const r = menu.r}
  <BrowseMenu at={menu} onClose={() => (menu = null)} onOpen={() => open(r)}>
    {#snippet extra(act)}
      {#if type === 'modpacks'}
        {#if session.owner}<button class="menu-item" role="menuitem" onclick={() => act(() => (making = preset(r)))}><Plus size={14} />Make a server</button>{/if}
      {:else if !busy[r.source + ':' + r.id] && (!r.installed || r.installed.update) && canAdd(r)}
        <button class="menu-item" role="menuitem" onclick={() => act(() => install(r))}><Download size={14} />{r.installed ? 'Update' : `Install into ${target.name}`}</button>
      {/if}
    {/snippet}
  </BrowseMenu>
{/if}

<style>
  .browse { display: flex; flex-direction: column; gap: 12px; min-height: 0; padding: 18px; }
  .hidden { display: none; }
  .page-h h2 { font-size: 18px; font-weight: 700; }
  .filt { display: none; }
  .cols { display: flex; gap: 18px; align-items: flex-start; }
  .filters { width: 210px; flex: none; display: flex; flex-direction: column; gap: 6px; position: sticky; top: 0; }
  .filters .lbl { margin-top: 8px; }
  .sel { background: var(--base); border: 1px solid var(--line2); border-radius: 6px; padding: 6px 8px; color: var(--text); width: 100%; }
  .seg { display: flex; border: 1px solid var(--line2); border-radius: 6px; overflow: hidden; }
  .seg button { flex: 1; padding: 5px 4px; font-size: 12px; color: var(--subtle); }
  .seg button + button { border-left: 1px solid var(--line2); }
  .seg button:hover:not(:disabled):not(.on) { background: #ffffff0a; }
  .seg button.on { background: var(--raise); color: var(--text); font-weight: 600; }
  .seg button.cf.on, .curseforge { color: var(--acc); }
  .seg button.mr.on, .modrinth { color: #1bd96a; }
  .seg button:disabled { opacity: 0.4; cursor: default; }
  .cats { display: flex; flex-direction: column; gap: 1px; }
  .cat { display: flex; align-items: center; gap: 7px; padding: 5px 9px; border-radius: 6px; color: var(--subtle); text-align: left; font-size: 12.5px; }
  .cat img { border-radius: 3px; flex: none; }
  .cat.row-sel { color: var(--text); font-weight: 600; }
  .reset { align-self: flex-start; white-space: normal; text-align: left; }
  .note { margin-top: 6px; }
  .note-bar { flex-wrap: wrap; padding: 8px 10px; border: 1px solid var(--line); border-radius: 8px; background: var(--base); }
  .results { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 10px; }
  .clr { color: var(--muted); display: grid; }
  .countbar { flex-wrap: wrap; row-gap: 4px; }
  .sorts { display: flex; align-items: center; gap: 2px; }
  .sorts .lbl { margin-right: 6px; }
  .sorts button { display: inline-flex; align-items: center; gap: 5px; padding: 3px 8px; border-radius: 6px; color: var(--subtle); font-size: 12px; }
  .sorts button:hover:not(.on) { background: #ffffff0a; color: var(--text); }
  .sorts button.on { background: var(--acc-dim); color: var(--acc); font-weight: 600; }
  .sorts button:not(.on) .sl { display: none; }
  .list { display: flex; flex-direction: column; gap: 6px; }
  /* Cards off screen skip layout and paint: long lists stay quick. */
  .rc { display: grid; grid-template-columns: 48px minmax(0, 1fr) auto; gap: 12px; align-items: center; padding: 10px 12px; border: 1px solid var(--line); border-radius: 8px; background: var(--base); cursor: pointer; content-visibility: auto; contain-intrinsic-size: auto 72px; }
  .rc:hover { background: var(--hover); }
  .ico { width: 48px; height: 48px; border-radius: 8px; object-fit: cover; background: var(--raise); }
  .ph { display: block; }
  .body { min-width: 0; }
  .t { font-size: 13.5px; }
  .t .muted { font-size: 12px; }
  .s { color: #cfcfd4; font-size: 12.5px; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .meta { display: flex; align-items: center; gap: 8px; font-size: 11.5px; color: var(--muted); margin-top: 3px; flex-wrap: wrap; min-width: 0; }
  .meta > span:not(.chip) { display: inline-flex; align-items: center; gap: 3px; }
  .meta .dl { color: var(--blue); }
  .meta .up { color: var(--green); }
  .meta .catline { max-width: 300px; display: inline-block; }
  .meta .catline :global(svg) { vertical-align: -1px; margin-right: 3px; }
  .catlink { color: inherit; font: inherit; padding: 0; }
  .catlink:hover { color: var(--text); text-decoration: underline; }
  .deps { display: inline-flex; align-items: center; gap: 4px; padding: 1px 6px; border: 1px solid var(--line2); border-radius: 6px; background: var(--raise); font-size: 11px; color: var(--text); white-space: nowrap; }
  .deps :global(svg) { color: var(--muted); }
  .chip.curseforge { color: var(--acc); background: var(--acc-dim); }
  .chip.modrinth { color: #1bd96a; background: #1bd96a1a; }
  .act { display: flex; align-items: center; gap: 8px; }
  .busy { display: inline-flex; align-items: center; gap: 6px; color: var(--gold); max-width: 220px; min-width: 0; }
  /* Loading cards: the shared skeleton row (app.css .skel) at a card's size. */
  .csk { height: 70px; padding: 0 12px; border: 1px solid var(--line); border-radius: 8px; }
  .csk::before { width: 48px; height: 48px; border-radius: 8px; }
  .warn { color: var(--gold); }
  .sentinel { height: 1px; }
  @media (max-width: 1100px) {
    .browse, :global(.pack) { padding: 16px; }
    .filt { display: inline-flex; }
    .cols { flex-direction: column; align-items: stretch; }
    .filters { display: none; width: auto; position: static; }
    .filters.open { display: flex; border: 1px solid var(--line); border-radius: 8px; padding: 10px; background: var(--base); }
    .cats { flex-direction: row; flex-wrap: wrap; max-height: 180px; overflow: auto; }
    .act .chip:not(.running) { display: none; }
  }
  @media (max-width: 640px) {
    .rc { grid-template-columns: 40px minmax(0, 1fr); }
    .ico { width: 40px; height: 40px; }
    .act { grid-column: 2; }
  }
</style>
