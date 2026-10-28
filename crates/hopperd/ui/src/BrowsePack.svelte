<script>
  // A project's page, like desktop's Browse detail: icon, "by …" (a link
  // to everything the author made), stats, facts (Minecraft, loaders,
  // where it runs, license), links, screenshots; for a mod or data pack
  // the install card (where it goes and every mod it pulls in); then
  // Description, Versions (each with its changelog), Dependencies and
  // Gallery. A modpack shows which versions have server files and "Make a
  // server from this".
  import { onMount, untrack } from 'svelte';
  import { ArrowLeft, Download, RefreshCw, Clock, ExternalLink, Server, Hammer, Plus, ChevronDown, ChevronRight, ChevronLeft, X, Scale, Check, Monitor, Box, Blocks, Hash, Tag, Code, Bug, BookOpen, MessageCircle, Layers, CircleCheck, RotateCcw, TriangleAlert, Info } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { session } from './lib/me.svelte.js';
  import { SITE, TYPES, SIDE, LOADER_NAME, count, ago, date, preset, size, andList, browseHash } from './lib/browse.js';
  import BrowseRich from './BrowseRich.svelte';
  import BrowseMenu from './BrowseMenu.svelte';

  // `target`: the server mods and data packs install into (Browse's
  // "Install into"); `busy`: what its install of this project is doing.
  // `onBack`: shown as a peek beside a dialog, which closes it; projects
  // opened from it (a dependency, the author's) open in place.
  // `onCategory(name)`: filters Browse by a category, when `canFilter(name)`.
  let { source, id, type = 'modpacks', target = null, mc = '', loader = '', back = '/browse', busy = '', onMake, onInstall, onBack = null, onCategory = null, canFilter = null } = $props();

  // The project shown: the one asked for, or one opened from it in a peek.
  let cur = $state(untrack(() => ({ source, id, type })));
  let trail = $state([]); // in a peek: where Back goes, { source, id, type, name }
  const kind = $derived(cur.type);
  const packs = $derived(kind === 'modpacks');
  const T = $derived(TYPES[kind] || TYPES.mods);

  let p = $state(null);
  let error = $state('');
  let tab = $state('description');
  let versions = $state(null);
  let vError = $state('');
  let onlyMatching = $state(untrack(() => !!(mc || loader)));
  let openLog = $state({});
  let logs = $state({});
  let viewer = $state(-1);
  let deps = $state(null); // null: loading; [] or the list; { error }
  let plan = $state(null); // 'loading', the plan, { error }, or null
  let author = $state(null); // 'loading', { name, url, projects }, { error }
  let menu = $state(null); // a listed project's right-click menu: { r, x, y }

  const filterText = $derived([mc, LOADER_NAME[loader]].filter(Boolean).join(' · '));
  const have = $derived(p?.installed || null);
  const sameFile = (v) => !!have && have.same_project && have.file_id === v.id;
  const newest = $derived(versions?.find((v) => v.type === 'release') || versions?.[0] || null);
  const isMods = $derived(kind === 'mods');
  const canInstall = $derived(!!target && (!isMods || !!target.loader));
  const otherSite = $derived(cur.source === 'modrinth' ? 'CurseForge' : 'Modrinth');
  // Desktop's facts: Minecraft (first five, the rest in a tooltip), loaders, where it runs.
  const mcs = $derived(p?.all_versions?.length ? p.all_versions : p?.mc_versions || []);
  const LINK_ICON = { Source: Code, Issues: Bug, Wiki: BookOpen };

  function reset() {
    p = null;
    error = '';
    tab = 'description';
    versions = null;
    vError = '';
    openLog = {};
    logs = {};
    viewer = -1;
    deps = null;
    plan = null;
    author = null;
    menu = null;
  }

  async function load() {
    const my = cur;
    try {
      const u = new URLSearchParams({ source: my.source, id: my.id, type: my.type });
      if (target && my.type !== 'modpacks') u.set('into', target.id);
      const r = await api(`/browse/pack?${u}`);
      if (my !== cur) return;
      p = r;
      loadPlan();
      if (my.type !== 'modpacks') loadDeps();
    } catch (e) {
      if (my === cur) error = e.message;
    }
  }

  async function loadVersions() {
    const my = cur;
    versions = null;
    vError = '';
    const u = new URLSearchParams({ source: my.source, id: my.id, type: my.type });
    if (onlyMatching && mc) u.set('mc', mc);
    if (onlyMatching && loader && my.type === 'mods') u.set('loader', loader);
    try {
      const v = (await api(`/browse/versions?${u}`)).versions;
      if (my === cur) versions = v;
    } catch (e) {
      if (my !== cur) return;
      vError = e.message;
      versions = [];
    }
  }

  /** What installing it into the target also downloads (mods only, like desktop). */
  async function loadPlan() {
    plan = null;
    if (!isMods || !target?.loader || have || !p) return;
    const my = cur;
    plan = 'loading';
    try {
      const r = await api(`/browse/plan?into=${encodeURIComponent(target.id)}&project=${p.num}`);
      if (my === cur) plan = r;
    } catch (e) {
      if (my === cur) plan = { error: e.message };
    }
  }

  async function loadDeps() {
    const my = cur;
    deps = null;
    const u = new URLSearchParams({ project: p.num });
    if (mc) u.set('mc', mc);
    if (loader && isMods) u.set('loader', loader);
    if (target && isMods) u.set('into', target.id);
    try {
      const d = (await api(`/browse/deps?${u}`)).deps;
      if (my === cur) deps = d;
    } catch (e) {
      if (my === cur) deps = { error: e.message };
    }
  }

  async function showAuthor() {
    if (!p?.author) return;
    const my = cur;
    author = 'loading';
    try {
      const r = await api(`/browse/author?project=${p.num}`);
      if (my === cur) author = r;
    } catch (e) {
      if (my === cur) author = { error: e.message };
    }
  }

  async function toggleLog(v) {
    openLog = { ...openLog, [v.id]: !openLog[v.id] };
    // A failed one is asked again when it's opened again.
    if (!openLog[v.id] || v.changelog || (logs[v.id] && !logs[v.id].error)) return;
    const my = cur;
    logs = { ...logs, [v.id]: 'loading' };
    try {
      const log = (await api(`/browse/changelog?id=${encodeURIComponent(my.id)}&file=${encodeURIComponent(v.id)}`)).changelog;
      if (my === cur) logs = { ...logs, [v.id]: log };
    } catch (e) {
      if (my === cur) logs = { ...logs, [v.id]: { error: e.message } };
    }
  }

  function setOnly(on) {
    onlyMatching = on;
    loadVersions();
  }

  function goBack() {
    if (onBack && trail.length) {
      const prev = trail[trail.length - 1];
      trail = trail.slice(0, -1);
      openHere(prev);
      return;
    }
    if (onBack) return onBack();
    location.hash = back;
  }

  function openHere(to) {
    cur = { source: to.source, id: to.id, type: to.type };
    reset();
    load();
    loadVersions();
  }

  /**
   * Opens another project (a dependency, one the install pulls in, one by
   * the author). On Browse it's a page of its own, so Back works; in a peek
   * it opens in place. Kinds Browse doesn't show open on their site.
   */
  function openProject(r) {
    const k = r.kind || 'mods';
    if (!TYPES[k]) return window.open(r.url, '_blank', 'noopener');
    if (onBack) {
      trail = [...trail, { ...cur, name: p?.name || '' }];
      openHere({ source: r.source, id: r.id, type: k });
      return;
    }
    const into = TYPES[k].into ? target?.id || '' : '';
    location.hash = `${browseHash(k, into)}/${r.source}/${encodeURIComponent(r.id)}`;
  }

  /** Installs the newest fitting file, or `v`; then shows what's there now. */
  async function install(v = null) {
    const r = await onInstall?.(p, v);
    if (r) load();
  }

  function make(v = null) {
    onMake(preset(p, v || newest));
  }

  function key(e) {
    if (viewer < 0) return;
    // Handled here: a dialog under the viewer (the update window) stays open.
    if (e.key === 'Escape') (e.preventDefault(), (viewer = -1));
    else if (e.key === 'ArrowRight') viewer = (viewer + 1) % p.gallery.length;
    else if (e.key === 'ArrowLeft') viewer = (viewer - 1 + p.gallery.length) % p.gallery.length;
  }

  onMount(() => {
    load();
    loadVersions();
  });
</script>

<svelte:window onkeydown={key} />

{#snippet row(r, right)}
  <button class="prow row-hover" title={r.summary} onclick={() => openProject(r)} oncontextmenu={(e) => { e.preventDefault(); menu = { r, x: e.clientX, y: e.clientY }; }}>
    {#if r.icon}<img src={r.icon} alt="" width="28" height="28" loading="lazy" />{:else}<span class="pico"></span>{/if}
    <span class="grow col"><span class="small ell">{r.name}</span>{#if right.sub}<span class="tiny muted ell">{right.sub}</span>{/if}</span>
    {@render right.tags()}
  </button>
{/snippet}

<div class="pack">
  <div class="row"><button class="btn sm ghost" onclick={goBack}><ArrowLeft size={14} />{onBack ? (trail.length ? `Back to ${trail[trail.length - 1].name}` : 'Close') : 'Browse'}</button>
    <span class="tiny muted">{T.label}{filterText ? ` · ${filterText}` : ''}{target && !packs ? ` · into ${target.name}` : ''}</span></div>

  {#if error}
    <div class="empty-state"><span class="es-ic"><TriangleAlert size={18} /></span><b>Couldn't load that project</b><p>Check your connection and try again. ({error})</p><button class="btn sm" onclick={() => { error = ''; load(); }}>Retry</button></div>
  {:else if !p}
    <div class="hd"><div class="ico sk"></div><div class="col grow"><div class="sk line" style:width="40%"></div><div class="sk line" style:width="25%"></div><div class="sk line" style:width="60%"></div></div></div>
    <div class="sk block"></div>
  {:else}
    <div class="hd">
      {#if p.icon}<img class="ico" src={p.icon} alt="" width="72" height="72" />{:else}<span class="ico"></span>{/if}
      <div class="col grow info">
        <h2>{p.name}</h2>
        {#if p.author}<button class="by small" title="Everything {p.author} made" onclick={showAuthor}>by {p.author}</button>{/if}
        <div class="meta">
          <span class="dl"><Download size={12} />{count(p.downloads)} downloads</span>
          {#if ago(p.updated)}<span class="up"><RefreshCw size={12} />Updated {ago(p.updated).toLowerCase()}</span>{/if}
          {#if ago(p.created)}<span class="cr" title={date(p.created)}><Clock size={12} />Created {ago(p.created).toLowerCase()}</span>{/if}
          {#if p.categories.length}
            {@const cs = p.categories.slice(0, 3)}
            <span class="cats"><Tag size={12} />{#each cs as c, i (c)}{#if onCategory && canFilter?.(c)}<button class="catlink" title="Show only {c}" onclick={() => onCategory(c)}>{c}</button>{:else}<span>{c}</span>{/if}{i + 1 < cs.length ? ', ' : ''}{/each}</span>
          {/if}
        </div>
        <div class="facts">
          {#if mcs.length}<span class="grp" title={mcs.join(', ')}><Box size={12} class="v" />{#each mcs.slice(0, 5) as v (v)}<span class="chip">{v}</span>{/each}{#if mcs.length > 5}<span class="chip">+{mcs.length - 5}</span>{/if}</span>{/if}
          {#if p.loaders.length && !['datapacks'].includes(kind)}<span class="grp"><Blocks size={12} class="l" />{#each p.loaders as l (l)}<span class="chip">{l}</span>{/each}</span>{/if}
          {#if SIDE[p.side]}<span class="grp"><Monitor size={12} class="s" /><span class="chip">{SIDE[p.side]}</span></span>{/if}
          {#if p.license}<span class="grp muted"><Scale size={12} />{p.license}</span>{/if}
          {#if p.source === 'curseforge'}<span class="grp muted"><Hash size={12} />{p.id}</span>{/if}
        </div>
        <div class="row wrap links">
          <a class="btn sm {p.source}" href={p.url} target="_blank" rel="noreferrer noopener"><ExternalLink size={13} />Open on {SITE[p.source]}</a>
          {#if packs}
            {#if p.server_files}<span class="chip running" title="The pack author's own server files"><Server size={11} />Server files</span>
            {:else}<span class="chip starting" title="This pack has no official server files: Hopper builds the server from the pack, leaving client-only mods out"><Hammer size={11} />Built from the client pack</span>{/if}
          {:else if p.side === 'client'}<span class="chip starting" title="It says it runs on the client only: a server doesn't need it"><Monitor size={11} />Client only</span>{/if}
          {#each p.links as l (l.url)}
            {@const LIcon = LINK_ICON[l.label] || MessageCircle}
            <a class="btn sm ghost" href={l.url} title={l.url} target="_blank" rel="noreferrer noopener"><LIcon size={13} />{l.label}</a>
          {/each}
        </div>
      </div>
      {#if packs && session.owner}
        <div class="mk">
          <button class="btn pri" onclick={() => make()} disabled={versions === null && !vError}><Plus size={14} />Make a server from this</button>
          <span class="tiny muted">{newest ? `Newest: ${newest.name}` : versions === null ? 'Looking up versions…' : ''}{newest ? ' · or pick a version below' : ''}</span>
        </div>
      {/if}
    </div>

    {#if p.gallery.length}
      <div class="strip">
        {#each p.gallery.slice(0, 5) as g, i (g.url)}<button class="shot" title={g.title} onclick={() => (viewer = i)}><img src={g.thumb} alt={g.title} loading="lazy" /></button>{/each}
        {#if p.gallery.length > 5}<button class="shot more" onclick={() => (tab = 'gallery')}>+{p.gallery.length - 5}</button>{/if}
      </div>
    {/if}

    {#if have && have.enabled === false && target}
      <div class="note warn"><Info size={14} /><span>Off in {target.name}. Updating or switching versions keeps it off; turn it on in the Mods tab.</span></div>
    {/if}
    {#if p.distribution === false && !packs && target}
      <div class="note warn"><TriangleAlert size={14} /><span>The author only allows downloads from the CurseForge website, so the panel can't download it. Get it from its page, then add the jar in the server's Mods tab.</span></div>
    {/if}

    {#if !packs && (target || !onBack)}
      <!-- The install box: where it goes and every mod it pulls in (desktop's install card). -->
      <div class="ic card">
        <div class="row ich">
          <div class="col grow">
            {#if !target}
              <b>Install into a server</b><span class="small muted">Pick a server under Install into to add this.</span>
            {:else if have}
              <b class="ell">{have.update ? `A newer file is out for ${target.name}` : `Installed on ${target.name}`}</b>
              <span class="small muted ell" title={have.file}>{have.file}{have.same_project ? '' : ` (from ${otherSite})`}</span>
            {:else}
              <b class="ell">Install into {target.name}</b>
              {#if isMods && !target.loader}<span class="small warn">{target.name} has no mod loader, so it can't run mods.</span>
              {:else if !isMods}<span class="small muted">The newest for Minecraft {target.game_version || 'it runs'}.</span>
              {:else if plan === 'loading' || plan === null}<span class="small muted row"><span class="spinner"></span>Checking what it needs</span>
              {:else if plan.error}<span class="small muted">{plan.error}</span>
              {:else if !plan.extra.length}<span class="small ok row"><CircleCheck size={13} />Nothing else needed · {size(plan.total)}</span>
              {:else}<span class="small muted row"><Layers size={13} />Also downloads {plan.extra.length} required mod{plan.extra.length === 1 ? '' : 's'} · {size(plan.total)} total</span>{/if}
            {/if}
          </div>
          {#if busy}<span class="busy small"><span class="spinner"></span><span class="ell">{busy}</span></span>
          {:else if have?.update}<button class="btn pri {p.source}" title={have.enabled === false ? 'A newer file is out; replaces the installed one, which stays disabled' : 'A newer file is out; replaces the installed one'} onclick={() => install()}><Download size={14} />Update</button>
          {:else if have}<span class="chip running"><Check size={11} />Installed</span>
          {:else if target}<button class="btn pri {p.source}" disabled={!canInstall || p.distribution === false} onclick={() => install()}><Download size={14} />Install</button>{/if}
        </div>
        {#if target && !have && plan && plan !== 'loading' && !plan.error}
          {#if plan.extra.length}
            <div class="also">
              <div class="lbl">Also downloads</div>
              {#each plan.extra as d (d.source + d.id)}
                {#snippet tags()}<span class="tiny muted">{size(d.size)}</span>{/snippet}
                {@render row(d, { sub: d.by ? `Needed by ${d.by}` : 'Required', tags })}
              {/each}
            </div>
          {/if}
          {@const on = plan.kept.filter((k) => k.on).map((k) => k.name)}
          {@const off = plan.kept.filter((k) => !k.on).map((k) => k.name)}
          {#if on.length}<div class="line muted"><Check size={12} />Already on {target.name}: {andList(on)}</div>{/if}
          {#if off.length}<div class="line warn"><RotateCcw size={12} />Turns back on: {andList(off)}</div>{/if}
          {#each plan.notes as n (n)}<div class="line warn"><TriangleAlert size={12} />{n}</div>{/each}
        {/if}
      </div>
    {/if}

    {#if author}
      <div class="row ah">
        <button class="btn sm ghost" onclick={() => (author = null)}><ArrowLeft size={13} />Back to {p.name}</button>
        <span class="sp"></span>
        {#if author.url}<a class="btn sm ghost" href={author.url} target="_blank" rel="noreferrer noopener"><ExternalLink size={13} />Open on {SITE[p.source]}</a>{/if}
      </div>
      <div class="col">
        <b>{p.author}</b>
        <span class="small muted">{author === 'loading' ? 'Loading projects' : author.error ? `On ${SITE[p.source]}` : `${author.projects.length} projects · ${count(author.projects.reduce((n, x) => n + (x.downloads || 0), 0))} downloads`}</span>
      </div>
      <div class="plist">
        {#if author === 'loading'}
          {#each Array(5) as _, i (i)}<div class="skel"><i></i></div>{/each}
        {:else if author.error}<div class="small err">{author.error}</div>
        {:else}
          {#each author.projects as r (r.source + r.id)}
            {#snippet tags()}<span class="tiny dl"><Download size={11} />{count(r.downloads)}</span>{#if TYPES[r.kind] && r.kind !== kind}<span class="chip">{TYPES[r.kind].label}</span>{:else if !TYPES[r.kind]}<ExternalLink size={12} />{/if}{/snippet}
            {@render row(r, { sub: r.summary, tags })}
          {:else}<div class="small muted pad">No projects found.</div>{/each}
        {/if}
      </div>
    {:else}
      <div class="tabs">
        <button class:tab-sel={tab === 'description'} onclick={() => (tab = 'description')}>Description</button>
        <button class:tab-sel={tab === 'versions'} onclick={() => (tab = 'versions')}>Versions{versions ? ` ${versions.length}` : ''}</button>
        {#if !packs}<button class:tab-sel={tab === 'deps'} onclick={() => (tab = 'deps')}>Dependencies{Array.isArray(deps) ? ` ${deps.length}` : ''}</button>{/if}
        {#if p.gallery.length}<button class:tab-sel={tab === 'gallery'} onclick={() => (tab = 'gallery')}>Gallery {p.gallery.length}</button>{/if}
      </div>

      {#if tab === 'description'}
        <div class="desc"><BrowseRich blocks={p.description} empty={p.summary || 'No description.'} /></div>
      {:else if tab === 'versions'}
        <div class="row wrap">
          {#if mc || loader}<label class="cbl"><span class="switch"><input type="checkbox" checked={onlyMatching} onchange={(e) => setOnly(e.currentTarget.checked)} /><span></span></span>Only {filterText}</label>{/if}
          {#if packs}<span class="tiny muted">Versions with server files use them; the others are built from the client pack with client-only mods left out.</span>
          {:else if target}<span class="tiny muted">Install a version to add that one to {target.name}{have ? ' in place of the one it has' : ''}.</span>{/if}
        </div>
        {#if vError}<div class="err">{vError}</div>{/if}
        <div class="vt">
          <div class="vr vh"><span>Version</span><span>Minecraft</span><span>Released</span><span>{packs ? 'Server' : ''}</span><span></span></div>
          {#if versions === null}
            {#each Array(5) as _, i (i)}<div class="vr"><span class="sk line"></span><span class="sk line"></span><span class="sk line"></span><span class="sk line"></span><span></span></div>{/each}
          {:else}
            {#each versions as v (v.id)}
              <div class="vr" class:row-sel={packs ? v === newest : sameFile(v)}>
                <span class="vn ell" title={v.title}>{v.name} {#if v.type}<span class="chip {v.type === 'release' ? 'running' : v.type === 'beta' ? 'starting' : 'crashed'}">{v.type[0].toUpperCase() + v.type.slice(1)}</span>{/if}</span>
                <span class="small">{v.mc_version}{v.loaders.length ? ` · ${v.loaders.join(', ')}` : ''}</span>
                <span class="tiny muted" title={date(v.date)}>{ago(v.date) || date(v.date)}</span>
                <span>{#if !packs}{#if sameFile(v)}<span class="chip running"><Check size={11} />Installed</span>{/if}
                  {:else if v.server_files}<span class="chip running"><Server size={11} />Server files</span>{:else}<span class="chip starting"><Hammer size={11} />From client pack</span>{/if}</span>
                <span class="row end">
                  <button class="btn sm ghost" onclick={() => toggleLog(v)}>{#if openLog[v.id]}<ChevronDown size={13} />{:else}<ChevronRight size={13} />{/if}Changelog</button>
                  {#if !packs}{#if target && !sameFile(v)}<button class="btn sm" disabled={!!busy || !canInstall || have?.locked || p.distribution === false} title={busy ? 'Another version of it is installing' : have?.locked ? 'Locked: unlock it in the Mods tab to switch versions' : ''} onclick={() => install(v)}>{have ? 'Switch' : 'Install'}</button>{/if}
                  {:else if session.owner}<button class="btn sm" onclick={() => make(v)}>Make a server</button>{/if}
                </span>
              </div>
              {#if openLog[v.id]}
                {@const log = v.changelog || logs[v.id]}
                <div class="cl">
                  {#if log === 'loading' || !log}<span class="small muted">Loading the changelog…</span>
                  {:else if log.error}<span class="err">{log.error}</span>
                  {:else}<BrowseRich blocks={log} empty="No changelog for this version." />{/if}
                </div>
              {/if}
            {:else}
              <div class="small muted pad">No versions{onlyMatching && filterText ? ` for ${filterText}` : ''}.</div>
            {/each}
          {/if}
        </div>
      {:else if tab === 'deps'}
        <div class="plist">
          {#if deps === null}
            {#each Array(3) as _, i (i)}<div class="skel"><i></i></div>{/each}
          {:else if deps.error}<div class="small err">{deps.error}</div>
          {:else}
            {#each deps as d (d.source + d.id)}
              {#snippet tags()}<span class="chip {d.relation === 'required' ? 'starting' : d.relation === 'incompatible' ? 'crashed' : ''}">{d.relation === 'required' ? 'Required' : d.relation === 'incompatible' ? 'Incompatible' : 'Optional'}</span>{#if d.installed}<span class="chip running">Installed</span>{/if}{/snippet}
              {@render row(d, { sub: '', tags })}
            {:else}<div class="small muted pad">No dependencies.</div>{/each}
          {/if}
        </div>
      {:else}
        <div class="grid">
          {#each p.gallery as g, i (g.url)}<button class="tile" onclick={() => (viewer = i)}><img src={g.thumb} alt={g.title} loading="lazy" />{#if g.title}<span class="tiny ell">{g.title}</span>{/if}</button>{/each}
        </div>
      {/if}
    {/if}
  {/if}
</div>

{#if menu}
  {@const r = menu.r}
  <BrowseMenu at={menu} onClose={() => (menu = null)} onOpen={() => openProject(r)} />
{/if}

{#if viewer >= 0 && p}
  {@const g = p.gallery[viewer]}
  <div class="viewer" role="presentation" onclick={(e) => e.target === e.currentTarget && (viewer = -1)}>
    <button class="vbtn close" title="Close" onclick={() => (viewer = -1)}><X size={18} /></button>
    {#if p.gallery.length > 1}<button class="vbtn prev" title="Previous" onclick={() => (viewer = (viewer - 1 + p.gallery.length) % p.gallery.length)}><ChevronLeft size={22} /></button>{/if}
    <figure><img src={g.url} alt={g.title} />{#if g.title}<figcaption>{g.title}</figcaption>{/if}<div class="tiny muted">{viewer + 1} / {p.gallery.length}</div></figure>
    {#if p.gallery.length > 1}<button class="vbtn next" title="Next" onclick={() => (viewer = (viewer + 1) % p.gallery.length)}><ChevronRight size={22} /></button>{/if}
  </div>
{/if}

<style>
  .pack { display: flex; flex-direction: column; gap: 12px; padding: 18px; }
  .grow { flex: 1; min-width: 0; }
  .hd { display: flex; gap: 16px; align-items: flex-start; }
  .ico { width: 72px; height: 72px; border-radius: 10px; object-fit: cover; background: var(--raise); flex: none; display: block; }
  .info { gap: 4px; }
  .info h2 { font-size: 20px; font-weight: 700; }
  .by { align-self: flex-start; color: var(--muted); padding: 0; }
  .by:hover { color: var(--acc); text-decoration: underline; }
  .meta { display: flex; gap: 12px; flex-wrap: wrap; font-size: 12px; color: var(--muted); }
  .meta > span { display: inline-flex; align-items: center; gap: 4px; }
  .meta .dl, .dl { color: var(--blue); display: inline-flex; align-items: center; gap: 3px; }
  .meta .up { color: var(--green); }
  .meta .cr { color: var(--gold); }
  .catlink { color: inherit; font: inherit; padding: 0; }
  .catlink:hover { color: var(--text); text-decoration: underline; }
  .facts { display: flex; gap: 12px; flex-wrap: wrap; font-size: 11.5px; padding-top: 2px; }
  .grp { display: inline-flex; align-items: center; gap: 4px; flex-wrap: wrap; }
  .grp :global(.v) { color: var(--blue); }
  .grp :global(.l) { color: var(--gold); }
  .grp :global(.s) { color: var(--green); }
  .links { gap: 6px; margin-top: 4px; }
  .links a.curseforge { color: var(--acc); }
  .links a.modrinth { color: #1bd96a; }
  .wrap { flex-wrap: wrap; }
  .mk { display: flex; flex-direction: column; align-items: flex-end; gap: 4px; flex: none; max-width: 280px; }
  .busy { display: inline-flex; align-items: center; gap: 6px; color: var(--gold); max-width: 260px; min-width: 0; }
  .note { display: flex; gap: 8px; align-items: flex-start; font-size: 12.5px; }
  .note :global(svg) { flex: none; margin-top: 2px; }
  .warn { color: var(--gold); }
  .ok { color: var(--green); }
  /* The install card: desktop's secondary box with the plan under it. */
  .ic { display: flex; flex-direction: column; gap: 10px; padding: 12px 14px; max-width: 860px; background: var(--surface); }
  .ich { gap: 12px; align-items: center; }
  .ich .col { gap: 2px; }
  .ich .row { gap: 6px; }
  .btn.pri.modrinth { background: #1bd96a; border-color: #1bd96a; color: #06210f; }
  .also { border-top: 1px solid var(--line); padding-top: 8px; display: flex; flex-direction: column; gap: 1px; }
  .also .lbl { padding: 0 8px 4px; }
  .line { display: flex; gap: 6px; align-items: flex-start; padding: 0 8px; font-size: 11.5px; }
  .line :global(svg) { flex: none; margin-top: 2px; }
  .prow { display: flex; align-items: center; gap: 10px; padding: 6px 8px; border-radius: 6px; text-align: left; width: 100%; min-width: 0; }
  .prow img, .pico { width: 28px; height: 28px; border-radius: 6px; object-fit: cover; background: var(--raise); flex: none; display: block; }
  .prow .col { gap: 0; }
  .plist { display: flex; flex-direction: column; gap: 1px; max-width: 860px; }
  .ah { gap: 6px; }
  .strip { display: flex; gap: 8px; overflow: hidden; }
  .shot { flex: none; width: 170px; height: 96px; border-radius: 6px; overflow: hidden; background: var(--raise); border: 1px solid var(--line); }
  .shot img { width: 100%; height: 100%; object-fit: cover; display: block; }
  .shot:hover { border-color: var(--line2); }
  .shot.more { width: 64px; color: var(--subtle); }
  .tabs { display: flex; gap: 18px; border-bottom: 1px solid var(--line); }
  .tabs button { padding: 7px 0; color: var(--subtle); }
  .tabs button:hover { color: var(--text); }
  .desc { max-width: 860px; }
  .cbl { display: inline-flex; align-items: center; gap: 6px; font-size: 12.5px; color: var(--subtle); cursor: pointer; }
  .vt { border: 1px solid var(--line); border-radius: 8px; overflow: hidden; }
  .vr { display: grid; grid-template-columns: minmax(0, 1fr) 170px 110px 150px auto; gap: 10px; align-items: center; padding: 7px 10px; border-bottom: 1px solid var(--line); }
  .vr:last-child { border-bottom: 0; }
  .vr:hover:not(.vh):not(.row-sel) { background: var(--hover); }
  .vh { background: var(--surface); color: var(--muted); font-size: 10.5px; text-transform: uppercase; letter-spacing: 0.06em; font-weight: 600; }
  .vn { font-weight: 600; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .end { justify-content: flex-end; gap: 4px; }
  .cl { padding: 8px 14px 10px 22px; border-bottom: 1px solid var(--line); background: var(--bg); max-height: 420px; overflow: auto; }
  .pad { padding: 12px; }
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(220px, 1fr)); gap: 10px; }
  .tile { display: flex; flex-direction: column; gap: 4px; text-align: left; color: var(--subtle); }
  .tile img { width: 100%; aspect-ratio: 16 / 9; object-fit: cover; border-radius: 6px; background: var(--raise); border: 1px solid var(--line); }
  /* The page's loading shapes: the shared skeleton colour and pulse (app.css .skel). */
  .sk { background: var(--skeleton); border-radius: 6px; animation: sk 1.4s ease-in-out infinite; }
  .sk.line { height: 12px; display: block; margin: 4px 0; border-radius: 99px; }
  .sk.block { height: 220px; }
  @keyframes sk { 50% { opacity: .45; } }
  .viewer { position: fixed; inset: 0; background: #000d; z-index: 60; display: flex; align-items: center; justify-content: center; padding: 40px 64px; }
  .viewer figure { margin: 0; display: flex; flex-direction: column; align-items: center; gap: 8px; max-width: 100%; max-height: 100%; }
  .viewer img { max-width: 100%; max-height: calc(100vh - 140px); border-radius: 6px; object-fit: contain; }
  .viewer figcaption { color: var(--text); }
  .vbtn { position: absolute; color: var(--text); background: #ffffff14; border-radius: 99px; padding: 8px; display: grid; }
  .vbtn:hover { background: #ffffff26; }
  .close { top: 16px; right: 16px; }
  .prev { left: 14px; top: 50%; }
  .next { right: 14px; top: 50%; }
  @media (max-width: 900px) {
    .hd { flex-wrap: wrap; }
    .mk { align-items: flex-start; width: 100%; }
    .vr { grid-template-columns: minmax(0, 1fr) auto; }
    .vr > span:nth-child(2), .vr > span:nth-child(3) { display: none; }
    .vr > span:nth-child(4) { display: none; }
    .vh { display: none; }
  }
</style>
