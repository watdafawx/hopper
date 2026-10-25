<script>
  // New server, like Hopper desktop's: "Start from" Fresh (pick the game
  // and loader), a Folder on this machine (run where it is, or copied in),
  // a Server pack zip (uploaded or on this machine), or a CurseForge /
  // Modrinth pack. Name, memory, port, address and the EULA are shared.
  import { onMount } from 'svelte';
  import { X, Server, Search, Upload, FolderOpen, FileArchive } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { followJob } from './lib/jobs.js';
  import { session } from './lib/me.svelte.js';
  import { uploadIncoming } from './lib/upload.js';
  import FolderPicker from './FolderPicker.svelte';

  let { close } = $props();

  const LOADERS = [
    ['vanilla', 'Vanilla'],
    ['neoforge', 'NeoForge'],
    ['fabric', 'Fabric'],
    ['quilt', 'Quilt'],
  ];
  const FROM = [
    ['fresh', 'Fresh'],
    ['folder', 'Folder'],
    ['zip', 'Server pack zip'],
    ['pack', 'CurseForge / Modrinth'],
  ];

  let from = $state('fresh');
  let versions = $state([]);
  let loaders = $state({});
  let name = $state('');
  let mc = $state('');
  let loader = $state('neoforge');
  let lv = $state('');
  let memory = $state(6);
  let port = $state('');
  let router = $state(null);
  let addrEdit = $state(null);
  let eula = $state(false);
  let busy = $state('');
  let error = $state('');

  // Folder
  let folder = $state('');
  let copy = $state(false);
  let picking = $state(false);
  // Zip
  let zipFile = $state(null);
  let zipPath = $state('');
  let upload = $state('');
  let zipInfo = $state(null);
  let zipPicking = $state(false);
  let progress = $state(0);
  // Pack
  let source = $state('modrinth');
  let query = $state('');
  let results = $state([]);
  let searching = $state(false);
  let pack = $state(null);
  let packVersions = $state([]);
  let packFile = $state('');
  let cfKey = $state(true);

  const options = $derived(loaders[loader] || []);
  const domain = $derived(router?.config?.enabled ? router.config.domain : '');
  const slug = (n) => n.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');
  const addrName = $derived(addrEdit ?? (domain ? slug(name) : ''));
  const address = $derived(!addrName ? '' : domain && !addrName.includes('.') ? `${addrName}.${domain}` : addrName);
  const needsPick = $derived(from === 'zip' && zipInfo && zipInfo.kind === 'server' && !zipInfo.loader && !zipInfo.mc_version);
  const pickedVersion = $derived(packVersions.find((v) => v.id === packFile));
  const fromList = $derived(FROM.filter(([k]) => k !== 'folder' || session.owner));
  const canMake = $derived(
    !busy &&
      (from === 'folder'
        ? !!folder && (!copy || name.trim())
        : name.trim() && eula &&
          (from === 'fresh'
            ? mc && (loader === 'vanilla' || loader === 'fabric' || loader === 'quilt' || lv)
            : from === 'zip'
              ? !!(zipFile || zipPath) && zipInfo && (!needsPick || (mc && (loader === 'vanilla' || lv || loader === 'fabric' || loader === 'quilt')))
              : !!pack && !!packFile)),
  );
  api('/router').then((r) => (router = r), () => {});
  api('/settings/curseforge').then((r) => (cfKey = !!r.set), () => {});

  async function loadLoaders() {
    if (!mc) return;
    loaders = {};
    try {
      loaders = (await api(`/meta/loaders/${encodeURIComponent(mc)}`)).loaders;
    } catch {
      loaders = {};
    }
  }

  $effect(() => {
    mc;
    loadLoaders();
  });
  $effect(() => {
    lv = options[0] || '';
  });
  $effect(() => {
    if (from === 'fresh') memory = loader === 'vanilla' ? 2 : 6;
  });

  function loaderId() {
    if (loader === 'vanilla') return '';
    if (loader === 'fabric' || loader === 'quilt') return lv ? `${loader}-${lv}` : `${loader}-`;
    return `${loader}-${lv}`;
  }

  async function inspect(q) {
    zipInfo = null;
    error = '';
    try {
      zipInfo = await api(`/servers/zip-info?${q}`);
      if (!name.trim()) name = zipInfo.name;
      if (zipInfo.recommended_mb) memory = Math.round(zipInfo.recommended_mb / 512) / 2;
    } catch (e) {
      error = e.message;
    }
  }

  async function pickZipFile(e) {
    const f = e.currentTarget.files?.[0] || e.dataTransfer?.files?.[0];
    if (!f) return;
    if (!/\.(zip|mrpack)$/i.test(f.name)) return (error = 'Pick a .zip or .mrpack.');
    zipFile = f;
    zipPath = '';
    upload = '';
    progress = 0;
    busy = `Uploading ${f.name}`;
    try {
      upload = await uploadIncoming(f, (p) => (progress = p));
      busy = '';
      await inspect(`upload=${encodeURIComponent(upload)}`);
    } catch (err) {
      error = err.message;
      busy = '';
      zipFile = null;
    }
  }

  async function pickZipPath(p) {
    zipPicking = false;
    zipPath = p;
    zipFile = null;
    upload = '';
    await inspect(`path=${encodeURIComponent(p)}`);
  }

  async function search(e) {
    e?.preventDefault();
    searching = true;
    error = '';
    try {
      results = (await api(`/packs/search?q=${encodeURIComponent(query)}&source=${source}`)).results;
    } catch (err) {
      error = err.message;
      results = [];
    } finally {
      searching = false;
    }
  }

  async function choosePack(r) {
    pack = r;
    packVersions = [];
    packFile = '';
    if (!name.trim()) name = r.name;
    try {
      packVersions = (await api(`/packs/versions?project=${r.id}&source=${r.source}`)).versions;
      packFile = (packVersions.find((v) => /release/i.test(v.type)) || packVersions[0])?.id || '';
    } catch (err) {
      error = err.message;
    }
  }

  async function make() {
    error = '';
    const common = { name: name.trim(), memory_mb: Math.round(memory * 1024), port: port ? Number(port) : null, accept_eula: eula, address };
    try {
      let r;
      if (from === 'folder') {
        const res = await post('/servers/import-folder', { path: folder, copy, name: name.trim() });
        if (res.job) {
          busy = 'Copying';
          r = await followJob(res.job, (st) => (busy = st.detail || 'Copying'));
        } else r = res;
      } else {
        let req;
        if (from === 'fresh') req = post('/servers/create', { ...common, mc_version: mc, loader, loader_version: loader === 'vanilla' ? '' : lv });
        else if (from === 'zip') req = post('/servers/import-zip', { ...common, upload: upload || null, path: zipPath || null, ...(needsPick ? { mc_version: mc, loader: loaderId() } : {}) });
        else req = post('/servers/create-from-pack', { ...common, source: pack.source, project: pack.id, file: packFile });
        const { job } = await req;
        busy = 'Starting';
        r = await followJob(job, (st) => (busy = st.detail || 'Working'));
      }
      say(from === 'folder' && !copy ? 'Added. It runs where it is.' : `Made ${name.trim() || 'the server'}${r?.left_out ? `: ${r.left_out} client-only mods left out (see left-out-client-mods.txt)` : ''}`);
      close();
      if (r?.id) location.hash = `/server/${encodeURIComponent(r.id)}/console`;
    } catch (e) {
      error = e.message;
      busy = '';
    }
  }

  onMount(async () => {
    try {
      versions = (await api('/meta/minecraft')).versions;
      mc = versions[0] || '';
    } catch (e) {
      error = `Couldn't list Minecraft versions: ${e.message}`;
    }
  });

  const KIND = { server: 'Server pack', curseforge: 'CurseForge client pack: built like desktop, client-only mods left out', modrinth: 'Modrinth client pack: built like desktop, client-only mods left out' };
</script>

{#snippet gameFields()}
  <div class="two">
    <div>
      <label class="lbl" for="ns-mc">Minecraft</label>
      <select id="ns-mc" class="sel" bind:value={mc} disabled={!!busy}>{#each versions as v (v)}<option value={v}>{v}</option>{/each}</select>
    </div>
    <div>
      <label class="lbl" for="ns-loader">Loader</label>
      <select id="ns-loader" class="sel" bind:value={loader} disabled={!!busy}>{#each LOADERS as [id, label] (id)}<option value={id}>{label}</option>{/each}</select>
    </div>
  </div>
  {#if loader !== 'vanilla'}
    <label class="lbl" for="ns-lv">{LOADERS.find((l) => l[0] === loader)[1]} version</label>
    {#if options.length}
      <select id="ns-lv" class="sel" bind:value={lv} disabled={!!busy}>{#each options as o (o)}<option value={o}>{o}</option>{/each}</select>
    {:else}
      <div class="small muted">{mc ? `No ${loader} for Minecraft ${mc}.` : ''}</div>
    {/if}
  {/if}
{/snippet}

<div class="back" role="presentation" onclick={(e) => e.target === e.currentTarget && !busy && close()}>
  <div class="box card">
    <h3><Server size={15} />New server<span class="sp"></span>
      <button class="btn sm icon ghost" disabled={!!busy} onclick={close}><X size={14} /></button></h3>
    <span class="lbl">Start from</span>
    <div class="seg from">{#each fromList as [k, label] (k)}<button class:on={from === k} disabled={!!busy} onclick={() => { from = k; error = ''; }}>{label}</button>{/each}</div>

    {#if from === 'folder'}
      <span class="lbl">Server folder on {location.hostname}</span>
      {#if folder && !picking}
        <div class="row picked"><FolderOpen size={14} /><span class="mono small ell grow">{folder}</span><button class="btn sm ghost" disabled={!!busy} onclick={() => (picking = true)}>Change</button></div>
      {:else}
        <FolderPicker pickLabel="Use this folder" onPick={(p) => { folder = p; picking = false; }} onCancel={folder ? () => (picking = false) : null} />
      {/if}
      <div class="seg">
        <button class:on={!copy} onclick={() => (copy = false)}>Run it where it is</button>
        <button class:on={copy} onclick={() => (copy = true)}>Copy into the servers folder</button>
      </div>
      <div class="tiny muted">{copy ? 'A copy goes in the servers folder; the original stays as it is.' : 'Nothing moves. Other panels (Crafty, MCSS) can keep using it too.'}</div>
      {#if copy}
        <label class="lbl" for="ns-name">Name</label>
        <div class="input"><input id="ns-name" bind:value={name} placeholder="Survival" disabled={!!busy} /></div>
      {/if}
    {:else}
      {#if from === 'zip'}
        {#if zipPicking}
          <FolderPicker zips onPick={pickZipPath} onCancel={() => (zipPicking = false)} />
        {:else}
          <label class="drop" ondragover={(e) => e.preventDefault()} ondrop={(e) => { e.preventDefault(); pickZipFile(e); }}>
            <input type="file" accept=".zip,.mrpack" hidden onchange={pickZipFile} disabled={!!busy} />
            {#if zipFile || zipPath}<FileArchive size={16} /><span class="mono small ell">{zipFile?.name || zipPath}</span>
            {:else}<Upload size={16} /><span>Drop a server pack zip here, or click to pick one</span>{/if}
          </label>
          {#if session.owner}<div class="tiny muted">Or a zip already on {location.hostname}: <button class="link" onclick={() => (zipPicking = true)}>browse</button></div>{/if}
        {/if}
        {#if busy && progress > 0 && progress < 1}<div class="bar"><i style:width="{progress * 100}%"></i></div>{/if}
        {#if zipInfo}
          <div class="small info">{KIND[zipInfo.kind] || zipInfo.kind}{zipInfo.mc_version ? ` · Minecraft ${zipInfo.mc_version}` : ''}{zipInfo.loader ? ` · ${zipInfo.loader}` : ''}</div>
          {#if needsPick}<div class="small warn">The zip doesn't say which Minecraft and loader it's for: pick them.</div>{@render gameFields()}{/if}
        {/if}
      {:else if from === 'pack'}
        <form class="row" onsubmit={search}>
          <div class="seg">
            <button type="button" class:on={source === 'modrinth'} onclick={() => (source = 'modrinth')}>Modrinth</button>
            <button type="button" class:on={source === 'curseforge'} disabled={!cfKey} title={cfKey ? '' : 'Needs a CurseForge API key (Access & devices)'} onclick={() => (source = 'curseforge')}>CurseForge</button>
          </div>
          <div class="input grow"><Search size={13} /><input bind:value={query} placeholder="Search modpacks" /></div>
          <button class="btn sm" disabled={searching}>Search</button>
        </form>
        <div class="results">
          {#each results as r (r.source + r.id)}
            <button class="res" class:on={pack?.id === r.id} onclick={() => choosePack(r)}>
              {#if r.icon}<img src={r.icon} alt="" width="34" height="34" loading="lazy" />{:else}<span class="ph"></span>{/if}
              <span class="grow"><b>{r.name}</b> <span class="tiny muted">{r.author}</span><span class="tiny muted ell blk">{r.summary}</span></span>
            </button>
          {:else}<div class="small muted">{searching ? 'Searching…' : 'Search for a modpack. Packs with server files use them; others are built from the client pack.'}</div>{/each}
        </div>
        {#if pack}
          <label class="lbl" for="ns-pv">Version</label>
          <select id="ns-pv" class="sel" bind:value={packFile} disabled={!!busy}>
            {#each packVersions as v (v.id)}<option value={v.id}>{v.name}{v.mc_version ? ` · ${v.mc_version}` : ''}{v.server_files ? ' · server files' : ''}</option>{/each}
          </select>
          {#if pickedVersion}<div class="tiny muted">{pickedVersion.server_files ? 'Uses the pack’s server files: the mods its author picked for servers.' : 'No server files: built from the client pack, client-only mods left out.'}</div>{/if}
        {/if}
      {/if}

      <label class="lbl" for="ns-name">Name</label>
      <div class="input"><input id="ns-name" bind:value={name} placeholder="Survival" disabled={!!busy} /></div>
      {#if from === 'fresh'}{@render gameFields()}{/if}
      <div class="two">
        <div>
          <label class="lbl" for="ns-mem">Memory: {memory} GB</label>
          <input id="ns-mem" class="range" type="range" min="1" max="32" step="0.5" bind:value={memory} disabled={!!busy} />
        </div>
        <div>
          <label class="lbl" for="ns-port">Port</label>
          <div class="input"><input id="ns-port" type="number" bind:value={port} placeholder="first free" disabled={!!busy} /></div>
        </div>
      </div>
      {#if router?.config?.enabled}
        <label class="lbl" for="ns-addr">Address players join</label>
        <div class="row">
          <div class="input grow"><input id="ns-addr" class="mono" value={addrName} oninput={(e) => (addrEdit = e.currentTarget.value.trim().toLowerCase())} placeholder={domain ? 'survival' : 'survival.example.com (optional)'} disabled={!!busy} /></div>
          {#if domain && !addrName.includes('.')}<span class="mono small muted">.{domain}</span>{/if}
        </div>
      {/if}
      <label class="eula small"><input type="checkbox" bind:checked={eula} disabled={!!busy} />
        <span>I agree to the <a href="https://aka.ms/MinecraftEULA" target="_blank" rel="noreferrer">Minecraft EULA</a>.</span></label>
    {/if}
    {#if error}<div class="err">{error}</div>{/if}
    {#if busy}<div class="busy small"><span class="spin"></span>{busy}</div>{/if}
    <div class="row end">
      <span class="tiny muted">{from === 'folder' ? '' : 'Java is found or downloaded for you.'}</span><span class="sp"></span>
      <button class="btn pri" disabled={!canMake} onclick={make}>{from === 'folder' && !copy ? 'Add server' : 'Make server'}</button>
    </div>
  </div>
</div>

<style>
  .grow { flex: 1; min-width: 0; }
  .back { position: fixed; inset: 0; background: #0008; display: flex; justify-content: center; align-items: flex-start; z-index: 55; padding: 56px 16px 16px; overflow: auto; }
  .box { width: min(560px, 100%); display: flex; flex-direction: column; gap: 8px; box-shadow: 0 24px 60px #000c; }
  .box h3 { margin: 0 0 4px; }
  .two { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
  .two > div { display: flex; flex-direction: column; gap: 6px; }
  .sel { background: var(--base); border: 1px solid var(--line2); border-radius: 6px; padding: 7px 8px; color: var(--text); width: 100%; }
  .range { width: 100%; accent-color: var(--acc); margin-top: 8px; }
  .eula { display: flex; gap: 8px; align-items: center; margin-top: 6px; }
  .eula input { accent-color: var(--acc); }
  .end { margin-top: 6px; }
  .busy { display: flex; align-items: center; gap: 8px; color: var(--gold); }
  .spin { width: 12px; height: 12px; border: 2px solid var(--gold); border-right-color: transparent; border-radius: 50%; animation: s 0.8s linear infinite; }
  @keyframes s { to { transform: rotate(360deg); } }
  .seg { display: flex; border: 1px solid var(--line2); border-radius: 6px; overflow: hidden; flex: none; align-self: flex-start; }
  .seg button { padding: 5px 11px; font-size: 12.5px; color: var(--subtle); }
  .seg button + button { border-left: 1px solid var(--line2); }
  .seg button.on { background: var(--acc); color: #1a1a1f; font-weight: 600; }
  .seg button:disabled { opacity: 0.4; }
  .from { align-self: stretch; }
  .from button { flex: 1; }
  .picked { gap: 8px; border: 1px solid var(--line2); border-radius: 8px; padding: 6px 8px; background: var(--base); }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .blk { display: block; }
  .drop { display: flex; align-items: center; justify-content: center; gap: 8px; border: 2px dashed var(--line2); border-radius: 10px; padding: 20px; color: var(--subtle); cursor: pointer; text-align: center; }
  .drop:hover { border-color: #f9731688; }
  .bar { height: 4px; border-radius: 2px; background: var(--line2); overflow: hidden; }
  .bar i { display: block; height: 100%; background: var(--acc); }
  .info { color: var(--subtle); }
  .warn { color: var(--gold); }
  .link { color: var(--acc); text-decoration: underline; }
  .results { max-height: 220px; overflow: auto; display: flex; flex-direction: column; gap: 2px; }
  .res { display: flex; align-items: center; gap: 10px; padding: 6px; border-radius: 6px; text-align: left; }
  .res:hover { background: #ffffff08; }
  .res.on { background: var(--acc-dim); box-shadow: inset 3px 0 0 var(--acc); }
  .res img, .ph { width: 34px; height: 34px; border-radius: 6px; background: var(--raise); flex: none; }
</style>
