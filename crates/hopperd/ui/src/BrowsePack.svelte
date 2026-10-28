<script>
  // A project's page, like desktop's Browse detail: icon, author, counts,
  // site chips, screenshots, then Description, Versions (each with its
  // changelog) and Gallery. A modpack shows which versions have server
  // files and "Make a server from this"; a mod or data pack installs into
  // the target server (the newest that fits, or a version from the list)
  // and shows what the server has already.
  import { onMount } from 'svelte';
  import { ArrowLeft, Download, RefreshCw, Sparkles, ExternalLink, Server, Hammer, Plus, ChevronDown, ChevronRight, ChevronLeft, X, Scale, Check, Monitor } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { session } from './lib/me.svelte.js';
  import { SITE, TYPES, count, ago, date, preset } from './lib/browse.js';
  import BrowseRich from './BrowseRich.svelte';

  // `target`: the server mods and data packs install into (Browse's
  // "Install into"); `busy`: what its install of this project is doing.
  let { source, id, type = 'modpacks', target = null, mc = '', loader = '', back = '/browse', busy = '', onMake, onInstall, onBack = null } = $props();
  const packs = $derived(type === 'modpacks');

  let p = $state(null);
  let error = $state('');
  let tab = $state('description');
  let versions = $state(null);
  let vError = $state('');
  let onlyMatching = $state(!!(mc || loader));
  let openLog = $state({});
  let logs = $state({});
  let viewer = $state(-1);

  const loaderName = { neoforge: 'NeoForge', forge: 'Forge', fabric: 'Fabric', quilt: 'Quilt' };
  const filterText = $derived([mc, loaderName[loader]].filter(Boolean).join(' · '));
  const have = $derived(p?.installed || null);
  const sameFile = (v) => !!have && have.same_project && have.file_id === v.id;
  const newest = $derived(versions?.find((v) => v.type === 'release') || versions?.[0] || null);

  async function load() {
    try {
      const u = new URLSearchParams({ source, id, type });
      if (target && !packs) u.set('into', target.id);
      p = await api(`/browse/pack?${u}`);
    } catch (e) {
      error = e.message;
    }
  }

  async function loadVersions() {
    versions = null;
    vError = '';
    const u = new URLSearchParams({ source, id, type });
    if (onlyMatching && mc) u.set('mc', mc);
    if (onlyMatching && loader) u.set('loader', loader);
    try {
      versions = (await api(`/browse/versions?${u}`)).versions;
    } catch (e) {
      vError = e.message;
      versions = [];
    }
  }

  async function toggleLog(v) {
    openLog = { ...openLog, [v.id]: !openLog[v.id] };
    if (!openLog[v.id] || v.changelog || logs[v.id]) return;
    logs = { ...logs, [v.id]: 'loading' };
    try {
      logs = { ...logs, [v.id]: (await api(`/browse/changelog?id=${encodeURIComponent(id)}&file=${v.id}`)).changelog };
    } catch (e) {
      logs = { ...logs, [v.id]: { error: e.message } };
    }
  }

  // `onBack`: shown as a peek beside a dialog, which closes it.
  function goBack() {
    if (onBack) return onBack();
    location.hash = back;
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
    if (e.key === 'Escape') viewer = -1;
    else if (e.key === 'ArrowRight') viewer = (viewer + 1) % p.gallery.length;
    else if (e.key === 'ArrowLeft') viewer = (viewer - 1 + p.gallery.length) % p.gallery.length;
  }

  onMount(() => {
    load();
    loadVersions();
  });
</script>

<svelte:window onkeydown={key} />

<div class="pack">
  <div class="row"><button class="btn sm ghost" onclick={goBack}><ArrowLeft size={14} />{onBack ? 'Close' : 'Browse'}</button>
    <span class="tiny muted">{TYPES[type]?.label || 'Modpacks'}{filterText ? ` · ${filterText}` : ''}{target ? ` · into ${target.name}` : ''}</span></div>

  {#if error}
    <div class="empty"><b>Couldn't open this {TYPES[type]?.one || 'pack'}</b><span class="small">{error}</span><button class="btn sm" onclick={() => { error = ''; load(); }}>Try again</button></div>
  {:else if !p}
    <div class="hd"><div class="ico skel"></div><div class="col grow"><div class="skel line" style:width="40%"></div><div class="skel line" style:width="25%"></div><div class="skel line" style:width="60%"></div></div></div>
    <div class="skel block"></div>
  {:else}
    <div class="hd">
      {#if p.icon}<img class="ico" src={p.icon} alt="" width="72" height="72" />{:else}<span class="ico"></span>{/if}
      <div class="col grow info">
        <h2>{p.name}</h2>
        {#if p.author}<div class="small subtle">by {p.author}</div>{/if}
        <div class="meta">
          <span class="dl"><Download size={12} />{count(p.downloads)} downloads</span>
          {#if p.updated}<span><RefreshCw size={12} />updated {ago(p.updated)}</span>{/if}
          {#if p.created}<span><Sparkles size={12} />created {date(p.created)}</span>{/if}
          {#if p.license}<span><Scale size={12} />{p.license}</span>{/if}
        </div>
        <div class="row wrap chips">
          <span class="chip {p.source}">{SITE[p.source]}</span>
          {#each p.loaders as l (l)}<span class="chip lan">{l}</span>{/each}
          {#each p.mc_versions.slice(0, 3) as v (v)}<span class="chip">{v}</span>{/each}
          {#each p.categories.slice(0, 4) as c (c)}<span class="chip">{c}</span>{/each}
        </div>
        <div class="row wrap links">
          <a class="btn sm" href={p.url} target="_blank" rel="noreferrer noopener"><ExternalLink size={13} />Open on {SITE[p.source]}</a>
          {#each p.links as l (l.url)}<a class="btn sm ghost" href={l.url} target="_blank" rel="noreferrer noopener">{l.label}</a>{/each}
          {#if packs}
            {#if p.server_files}<span class="chip running"><Server size={11} />Server files</span>
            {:else}<span class="chip starting" title="Hopper builds the server from the client pack and leaves client-only mods out"><Hammer size={11} />No server files: built from the client pack</span>{/if}
          {:else if p.side === 'client'}<span class="chip starting" title="Modrinth says this runs on the client only: a server doesn't need it"><Monitor size={11} />Client only</span>{/if}
        </div>
      </div>
      {#if !packs}
        <div class="mk">
          {#if !target}<span class="tiny muted">Pick a server under Install into to add this.</span>
          {:else if busy}<span class="busy small"><span class="spin"></span><span class="ell">{busy}</span></span>
          {:else if have}
            <span class="chip running"><Check size={11} />Installed on {target.name}</span>
            <span class="tiny muted ell" title={have.file}>{have.file}{have.same_project ? '' : ` (from ${source === 'modrinth' ? 'CurseForge' : 'Modrinth'})`}</span>
          {:else}
            <button class="btn pri" onclick={() => install()} disabled={type === 'mods' && !target.loader}><Plus size={14} />Install</button>
            <span class="tiny muted">Into {target.name} · newest for {[target.game_version, type === 'mods' ? target.loader_name : ''].filter(Boolean).join(' ') || 'it'}{type === 'mods' ? ', with what it needs' : ''}</span>
          {/if}
        </div>
      {:else if session.owner}
        <div class="mk">
          <button class="btn pri" onclick={() => make()} disabled={versions === null && !vError}><Plus size={14} />Make a server from this</button>
          <span class="tiny muted">{newest ? `Newest: ${newest.name}` : versions === null ? 'Looking up versions…' : ''}{newest ? ' · or pick a version' : ''}</span>
        </div>
      {/if}
    </div>

    {#if p.gallery.length}
      <div class="strip">
        {#each p.gallery.slice(0, 5) as g, i (g.url)}<button class="shot" title={g.title} onclick={() => (viewer = i)}><img src={g.thumb} alt={g.title} loading="lazy" /></button>{/each}
        {#if p.gallery.length > 5}<button class="shot more" onclick={() => (tab = 'gallery')}>+{p.gallery.length - 5}</button>{/if}
      </div>
    {/if}

    <div class="tabs">
      <button class:on={tab === 'description'} onclick={() => (tab = 'description')}>Description</button>
      <button class:on={tab === 'versions'} onclick={() => (tab = 'versions')}>Versions{versions ? ` ${versions.length}` : ''}</button>
      {#if p.gallery.length}<button class:on={tab === 'gallery'} onclick={() => (tab = 'gallery')}>Gallery {p.gallery.length}</button>{/if}
    </div>

    {#if tab === 'description'}
      <div class="desc"><BrowseRich blocks={p.description} empty={p.summary || 'No description.'} /></div>
    {:else if tab === 'versions'}
      <div class="row wrap">
        {#if mc || loader}<label class="cbl"><input type="checkbox" bind:checked={onlyMatching} onchange={loadVersions} />Only {filterText}</label>{/if}
        {#if packs}<span class="tiny muted">Versions with server files use them; the others are built from the client pack with client-only mods left out.</span>
        {:else if target}<span class="tiny muted">Install a version to add that one to {target.name}{have ? ' in place of the one it has' : ''}.</span>{/if}
      </div>
      {#if vError}<div class="err">{vError}</div>{/if}
      <div class="vt">
        <div class="vr vh"><span>Version</span><span>Minecraft</span><span>Released</span><span>{packs ? 'Server' : ''}</span><span></span></div>
        {#if versions === null}
          {#each Array(5) as _, i (i)}<div class="vr"><span class="skel line"></span><span class="skel line"></span><span class="skel line"></span><span class="skel line"></span><span></span></div>{/each}
        {:else}
          {#each versions as v (v.id)}
            <div class="vr" class:on={packs ? v === newest : sameFile(v)}>
              <span class="vn ell" title={v.title}>{v.name} {#if v.type}<span class="chip {v.type === 'release' ? 'running' : v.type === 'beta' ? 'starting' : 'crashed'}">{v.type[0].toUpperCase() + v.type.slice(1)}</span>{/if}</span>
              <span class="small">{v.mc_version}{v.loaders.length ? ` · ${v.loaders.join(', ')}` : ''}</span>
              <span class="tiny muted">{date(v.date)}</span>
              <span>{#if !packs}{#if sameFile(v)}<span class="chip running"><Check size={11} />Installed</span>{/if}
                {:else if v.server_files}<span class="chip running"><Server size={11} />Server files</span>{:else}<span class="chip starting"><Hammer size={11} />From client pack</span>{/if}</span>
              <span class="row end">
                <button class="btn sm ghost" onclick={() => toggleLog(v)}>{#if openLog[v.id]}<ChevronDown size={13} />{:else}<ChevronRight size={13} />{/if}Changelog</button>
                {#if !packs}{#if target && !sameFile(v)}<button class="btn sm" disabled={!!busy || (type === 'mods' && !target.loader)} onclick={() => install(v)}>Install this version</button>{/if}
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
    {:else}
      <div class="grid">
        {#each p.gallery as g, i (g.url)}<button class="tile" onclick={() => (viewer = i)}><img src={g.thumb} alt={g.title} loading="lazy" />{#if g.title}<span class="tiny ell">{g.title}</span>{/if}</button>{/each}
      </div>
    {/if}
  {/if}
</div>

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
  .meta { display: flex; gap: 12px; flex-wrap: wrap; font-size: 12px; color: var(--muted); }
  .meta span { display: inline-flex; align-items: center; gap: 4px; }
  .meta .dl { color: var(--blue); }
  .chips { gap: 5px; margin-top: 2px; }
  .links { gap: 6px; margin-top: 4px; }
  .wrap { flex-wrap: wrap; }
  .chip.curseforge { color: var(--acc); background: var(--acc-dim); }
  .chip.modrinth { color: #1bd96a; background: #1bd96a1a; }
  .mk { display: flex; flex-direction: column; align-items: flex-end; gap: 4px; flex: none; max-width: 280px; }
  .busy { display: inline-flex; align-items: center; gap: 6px; color: var(--gold); max-width: 260px; min-width: 0; }
  .spin { width: 12px; height: 12px; flex: none; border: 2px solid var(--gold); border-right-color: transparent; border-radius: 50%; animation: s 0.8s linear infinite; }
  @keyframes s { to { transform: rotate(360deg); } }
  .strip { display: flex; gap: 8px; overflow: hidden; }
  .shot { flex: none; width: 170px; height: 96px; border-radius: 6px; overflow: hidden; background: var(--raise); border: 1px solid var(--line); }
  .shot img { width: 100%; height: 100%; object-fit: cover; display: block; }
  .shot:hover { border-color: var(--line2); }
  .shot.more { width: 64px; color: var(--subtle); }
  .tabs { display: flex; gap: 18px; border-bottom: 1px solid var(--line); }
  .tabs button { padding: 7px 0; color: var(--subtle); border-bottom: 2px solid transparent; margin-bottom: -1px; }
  .tabs button:hover { color: var(--text); }
  .tabs button.on { color: var(--text); border-color: var(--acc); font-weight: 600; }
  .desc { max-width: 860px; }
  .cbl { display: inline-flex; align-items: center; gap: 6px; font-size: 12.5px; color: var(--subtle); cursor: pointer; }
  .cbl input { accent-color: var(--acc); }
  .vt { border: 1px solid var(--line); border-radius: 8px; overflow: hidden; }
  .vr { display: grid; grid-template-columns: minmax(0, 1fr) 170px 110px 150px auto; gap: 10px; align-items: center; padding: 7px 10px; border-bottom: 1px solid var(--line); }
  .vr:last-child { border-bottom: 0; }
  .vr:hover:not(.vh) { background: #ffffff06; }
  .vr.on { background: var(--acc-dim); box-shadow: inset 3px 0 0 var(--acc); }
  .vh { background: var(--surface); color: var(--muted); font-size: 10.5px; text-transform: uppercase; letter-spacing: 0.06em; font-weight: 600; }
  .vn { font-weight: 600; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .end { justify-content: flex-end; gap: 4px; }
  .cl { padding: 8px 14px 10px 22px; border-bottom: 1px solid var(--line); background: var(--bg); max-height: 420px; overflow: auto; }
  .pad { padding: 12px; }
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(220px, 1fr)); gap: 10px; }
  .tile { display: flex; flex-direction: column; gap: 4px; text-align: left; color: var(--subtle); }
  .tile img { width: 100%; aspect-ratio: 16 / 9; object-fit: cover; border-radius: 6px; background: var(--raise); border: 1px solid var(--line); }
  .skel { background: linear-gradient(90deg, var(--base) 0%, var(--surface) 50%, var(--base) 100%); background-size: 200% 100%; animation: sh 1.2s linear infinite; border-radius: 6px; }
  .skel.line { height: 12px; display: block; margin: 4px 0; }
  .skel.block { height: 220px; }
  @keyframes sh { to { background-position: -200% 0; } }
  .empty { display: flex; flex-direction: column; align-items: center; gap: 6px; padding: 36px 16px; border: 1px dashed var(--line2); border-radius: 10px; color: var(--subtle); text-align: center; }
  .empty b { color: var(--text); }
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
