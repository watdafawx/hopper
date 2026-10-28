<script>
  // New server, like Hopper desktop's: "Start from" Fresh (pick the game
  // and loader), a Folder on this machine (run where it is, or copied in),
  // a Server pack zip (uploaded or on this machine), a CurseForge /
  // Modrinth pack or a Template. Name, memory, port, address and the EULA
  // are shared. The dialog keeps one height; its body scrolls.
  import { onMount, untrack } from 'svelte';
  import { X, Server, Search, Upload, FolderOpen, FileArchive, Sparkles, Package, LayoutTemplate, Hammer, ExternalLink } from '@lucide/svelte';
  import { SITE, LOADER_NAME, store } from './lib/browse.js';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { followJob } from './lib/jobs.js';
  import { session } from './lib/me.svelte.js';
  import { uploadIncoming } from './lib/upload.js';
  import { dialog } from './lib/dialog.svelte.js';
  import FolderPicker from './FolderPicker.svelte';

  // `preset`: a pack (and version) from Browse's "Make a server from this".
  let { close, preset = null } = $props();

  const LOADERS = [
    ['vanilla', 'Vanilla'],
    ['paper', 'Paper'],
    ['purpur', 'Purpur'],
    ['neoforge', 'NeoForge'],
    ['forge', 'Forge'],
    ['fabric', 'Fabric'],
    ['quilt', 'Quilt'],
  ];
  // The Type dropdown's groups (Vanilla stands alone).
  const LOADER_GROUPS = [
    ['Plugins', ['paper', 'purpur']],
    ['Mods', ['neoforge', 'forge', 'fabric', 'quilt']],
  ];
  const loaderLabel = (id) => LOADERS.find((l) => l[0] === id)?.[1] || id;
  // Types that pick their newest version by themselves when none is chosen.
  const OPTIONAL_VERSION = ['vanilla', 'paper', 'purpur', 'fabric', 'quilt'];
  const PLUGINS = ['paper', 'purpur'];
  const host = location.hostname;
  const FROM = [
    ['fresh', 'Fresh', Sparkles, 'A new server: pick the game and server type.'],
    ['folder', 'Folder', FolderOpen, `A server folder already on ${host}: run it where it is, or copy it in.`],
    ['zip', 'Zip', FileArchive, `A server pack zip or .mrpack, uploaded or already on ${host}.`],
    ['pack', 'Modpack', Package, 'A Modrinth or CurseForge pack: its server files if it has them, otherwise built from the client pack.'],
    ['template', 'Template', LayoutTemplate, 'A saved template: its mods and configs, no world or players.'],
  ];

  let from = $state('fresh');
  // Template: servers saved from Settings › General (no world, no players).
  let templates = $state([]);
  let tpl = $state('');
  let versions = $state([]);
  let loaders = $state({});
  let recommended = $state({});
  let loadingLoaders = $state(false);
  let name = $state('');
  let nameEl = $state();
  let mc = $state('');
  let loader = $state('neoforge');
  let lv = $state('');
  let memory = $state(6);
  let port = $state('');
  let router = $state(null);
  let hostInfo = $state(null);
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
  // Pack: searched like Browse (both sites at once, or one), popular first.
  let source = $state(store('browse.source') || 'both');
  let query = $state('');
  let results = $state([]);
  let searching = $state(false);
  let pack = $state(null);
  let packVersions = $state([]);
  let packFile = $state('');
  let cfKey = $state(true);
  // A pack from Browse shows as picked; Change opens the search.
  let showSearch = $state(true);
  let searchTimer;

  const options = $derived(loaders[loader] || []);
  const domain = $derived(router?.config?.enabled ? router.config.domain : '');
  const slug = (n) => n.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');
  const addrName = $derived(addrEdit ?? (domain ? slug(name) : ''));
  const address = $derived(!addrName ? '' : domain && !addrName.includes('.') ? `${addrName}.${domain}` : addrName);
  const needsPick = $derived(from === 'zip' && zipInfo && zipInfo.kind === 'server' && !zipInfo.loader && !zipInfo.mc_version);
  const pickedVersion = $derived(packVersions.find((v) => v.id === packFile));
  const fromList = $derived(FROM.filter(([k]) => (k !== 'folder' || session.owner) && (k !== 'template' || templates.length)));
  const fromDesc = $derived(FROM.find((f) => f[0] === from)?.[3] || '');
  // The host's RAM, for the memory slider's range and hint.
  const hostGb = $derived(hostInfo?.mem_total ? hostInfo.mem_total / 2 ** 30 : 0);
  const memMax = $derived(hostGb ? Math.max(2, Math.floor(hostGb)) : 32);
  const canMake = $derived(
    !busy &&
      (from === 'folder'
        ? !!folder && (!copy || name.trim())
        : name.trim() && eula &&
          (from === 'template'
            ? !!tpl
            : from === 'fresh'
            ? mc && (OPTIONAL_VERSION.includes(loader) || lv)
            : from === 'zip'
              ? !!(zipFile || zipPath) && zipInfo && (!needsPick || (mc && (OPTIONAL_VERSION.includes(loader) || lv)))
              : !!pack && !!packFile)),
  );
  // Why Create server is off, shown beside it (desktop shows its problem the same way).
  const why = $derived.by(() => {
    if (busy || canMake) return '';
    if (from === 'folder') return !folder ? 'Pick the server folder.' : 'Name the copy.';
    if (from === 'template' && !tpl) return 'Pick a template.';
    if (from === 'zip' && !(zipFile || zipPath)) return 'Pick a zip.';
    if (from === 'zip' && !zipInfo) return error ? '' : 'Reading the zip…';
    if (from === 'pack' && !pack) return 'Pick a modpack.';
    if (from === 'pack' && !packFile) return loadingVersions ? 'Loading its versions…' : 'Pick a version.';
    if (from === 'fresh' || needsPick) {
      if (!mc) return 'Pick a Minecraft version.';
      if (!OPTIONAL_VERSION.includes(loader) && !lv) return loadingLoaders ? 'Loading builds' : `No ${loaderLabel(loader)} builds for ${mc}: pick another version.`;
    }
    if (!name.trim()) return 'Name the server.';
    if (!eula) return 'Agree to the Minecraft EULA to make a server.';
    return '';
  });
  api('/router').then((r) => (router = r), () => {});
  api('/templates').then((r) => (templates = r.templates || []), () => {});
  api('/settings/curseforge').then((r) => (cfKey = !!r.set), () => {});
  api('/host').then((r) => (hostInfo = r), () => {});

  // The first Minecraft picked is the newest release the default type has
  // (a brand-new release can be out before NeoForge is); a pick by hand stops it.
  let autoMc = true;
  let loadSeq = 0;
  async function loadLoaders() {
    if (!mc) return;
    const want = mc;
    const seq = ++loadSeq;
    loaders = {};
    recommended = {};
    loadingLoaders = true;
    try {
      const r = await api(`/meta/loaders/${encodeURIComponent(want)}`);
      if (seq !== loadSeq) return;
      loaders = r.loaders || {};
      recommended = r.recommended || {};
      if (autoMc && from === 'fresh' && loader !== 'vanilla' && !(loaders[loader] || []).length) {
        const i = versions.indexOf(want);
        if (i >= 0 && i < 5 && versions[i + 1]) {
          mc = versions[i + 1];
          return;
        }
      }
      autoMc = false;
    } catch {
      if (seq === loadSeq) loaders = {};
    } finally {
      if (seq === loadSeq) loadingLoaders = false;
    }
  }

  $effect(() => {
    mc;
    loadLoaders();
  });
  // The build desktop picks: the loader's recommended one when it lists it, else the newest.
  $effect(() => {
    const rec = recommended[loader];
    lv = rec && options.includes(rec) ? rec : options[0] || '';
  });
  $effect(() => {
    if (from === 'fresh') memory = loader === 'vanilla' || PLUGINS.includes(loader) ? 2 : 6;
  });

  function loaderId() {
    if (loader === 'vanilla') return '';
    if (OPTIONAL_VERSION.includes(loader)) return lv ? `${loader}-${lv}` : `${loader}-`;
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

  let searchSeq = 0;
  async function search(e) {
    e?.preventDefault();
    clearTimeout(searchTimer);
    const my = ++searchSeq;
    searching = true;
    error = '';
    try {
      const r = await api(`/browse/search?${new URLSearchParams({ type: 'modpacks', q: query, source, sort: 'popularity', page: '0' })}`);
      if (my !== searchSeq) return;
      cfKey = r.curseforge;
      // Browse's cards: `num` is the id New server's pack routes take.
      results = r.results.map((c) => ({ ...c, id: c.num }));
    } catch (err) {
      if (my !== searchSeq) return;
      error = err.message;
      results = [];
    } finally {
      if (my === searchSeq) searching = false;
    }
  }
  function searchSoon() {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(search, 350);
  }
  function setSource(s) {
    if (s === source) return;
    source = s;
    store('browse.source', s);
    search();
  }
  // The pack tab opens on the popular packs, like Browse.
  let listed = false;
  $effect(() => {
    if (from === 'pack' && showSearch && !listed) {
      listed = true;
      if (results.length <= 1 && !query) untrack(() => search());
    }
  });
  /** "NeoForge 1.21.1": what a pack version runs on. */
  function runsOn(v) {
    const l = (v?.loaders || []).map((x) => LOADER_NAME[String(x).toLowerCase()] || '').find(Boolean) || pack?.loaders?.[0] || '';
    return `${l || 'Minecraft'} ${v?.mc_version || ''}`.trim();
  }

  let packSeq = 0;
  let loadingVersions = $state(false);
  async function choosePack(r, file = '', version = null) {
    const my = ++packSeq;
    // A name that came from the last pack follows the new one.
    if (!name.trim() || name === pack?.name) name = r.name;
    pack = r;
    packVersions = version ? [version] : [];
    packFile = version?.id || '';
    error = '';
    // Picked: the search folds away (Change opens it again), like a pack from Browse.
    showSearch = false;
    loadingVersions = true;
    try {
      let list = (await api(`/packs/versions?${new URLSearchParams({ project: r.id, source: r.source })}`)).versions || [];
      if (my !== packSeq) return;
      // An older version picked in Browse may be past the newest 50.
      if (version && !list.some((v) => v.id === version.id)) list = [version, ...list];
      packVersions = list;
      packFile = (file && list.some((v) => v.id === file) ? file : '') || (list.find((v) => /release/i.test(v.type)) || list[0])?.id || '';
    } catch (err) {
      if (my === packSeq) error = err.message;
    } finally {
      if (my === packSeq) loadingVersions = false;
    }
  }

  async function make() {
    if (!canMake) return;
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
        else if (from === 'template') req = post('/servers/from-template', { ...common, template: tpl });
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

  function pickFrom(k) {
    from = k;
    error = '';
  }

  // Escape closes (unless a confirm dialog is on top); Enter in one of
  // its fields makes the server (the pack search box has no id: it searches).
  function onWindowKey(e) {
    if (e.key === 'Escape' && !e.defaultPrevented && !dialog.open && !busy) {
      e.preventDefault();
      close();
    }
  }
  function onBoxKey(e) {
    if (e.key !== 'Enter' || e.isComposing) return;
    const t = e.target;
    // Only this dialog's own fields (not the folder picker's path box).
    if (t.tagName !== 'INPUT' || !t.id?.startsWith('ns-')) return;
    e.preventDefault();
    make();
  }

  onMount(async () => {
    if (preset) {
      from = 'pack';
      showSearch = false;
      results = [preset];
      choosePack(preset, preset.file, preset.version);
    }
    nameEl?.focus();
    try {
      versions = (await api('/meta/minecraft')).versions;
      mc = versions[0] || '';
    } catch (e) {
      error = `Couldn't list Minecraft versions: ${e.message}`;
    }
  });

  const KIND = { server: 'Server pack', curseforge: 'CurseForge client pack', modrinth: 'Modrinth client pack' };
</script>

<svelte:window onkeydown={onWindowKey} />

{#snippet gameFields()}
  <div class="sec">Game</div>
  <div class="grid">
    <div class="f">
      <label class="lbl" for="ns-mc">Minecraft</label>
      <select id="ns-mc" class="sel" bind:value={mc} onchange={() => (autoMc = false)} disabled={!!busy}>{#each versions as v (v)}<option value={v}>{v}</option>{/each}</select>
    </div>
    <div class="f">
      <label class="lbl" for="ns-loader">Type</label>
      <select id="ns-loader" class="sel" bind:value={loader} disabled={!!busy}>
        <option value="vanilla">Vanilla</option>
        {#each LOADER_GROUPS as [group, ids] (group)}<optgroup label={group}>{#each ids as id (id)}<option value={id}>{loaderLabel(id)}</option>{/each}</optgroup>{/each}
      </select>
    </div>
    {#if loader !== 'vanilla'}
      <div class="f span">
        <label class="lbl" for="ns-lv">{loaderLabel(loader)} {PLUGINS.includes(loader) ? 'build' : 'version'}</label>
        {#if options.length}
          <select id="ns-lv" class="sel" bind:value={lv} disabled={!!busy}>{#each options as o (o)}<option value={o}>{o}{o === recommended[loader] ? (loader === 'forge' ? ' · recommended' : ' · latest stable') : ''}</option>{/each}</select>
        {:else}
          <div class="sel ghost small muted">{loadingLoaders ? 'Loading builds' : mc ? `No ${loaderLabel(loader)} builds for ${mc}` : '–'}</div>
        {/if}
        {#if PLUGINS.includes(loader)}
          <div class="tiny muted">Plugins (.jar files from Hangar, Modrinth or SpigotMC) go in the <code>plugins</code> folder. {loaderLabel(loader)} doesn't run Forge or Fabric mods.</div>
        {/if}
      </div>
    {/if}
  </div>
{/snippet}

<div class="dialog-back" role="presentation" onclick={(e) => e.target === e.currentTarget && !busy && close()}>
  <div class="box popover" role="dialog" aria-modal="true" aria-labelledby="ns-title" tabindex="-1" onkeydown={onBoxKey}>
    <h3 id="ns-title" class="head row"><Server size={15} />New server<span class="sp"></span>
      <button class="btn sm icon ghost" title="Close (Esc)" aria-label="Close" disabled={!!busy} onclick={close}><X size={14} /></button></h3>

    <div class="body">
      <div class="f">
        <span class="lbl">Start from</span>
        <div class="tabs" role="tablist">
          {#each fromList as [k, label, Icon] (k)}
            <button role="tab" aria-selected={from === k} class:tab-sel={from === k} disabled={!!busy} onclick={() => pickFrom(k)}><Icon size={14} /><span>{label}</span></button>
          {/each}
        </div>
        <div class="tiny muted desc">{fromDesc}</div>
      </div>

      {#if from === 'folder'}
        <div class="f">
          <span class="lbl">Server folder on {host}</span>
          {#if folder && !picking}
            <div class="row picked"><FolderOpen size={14} /><span class="mono small ell grow">{folder}</span><button class="btn sm ghost" disabled={!!busy} onclick={() => (picking = true)}>Change</button></div>
          {:else}
            <FolderPicker pickLabel="Use this folder" onPick={(p) => { folder = p; picking = false; }} onCancel={folder ? () => (picking = false) : null} />
          {/if}
        </div>
        <div class="f">
          <div class="seg">
            <button class:on={!copy} disabled={!!busy} onclick={() => (copy = false)}>Run it where it is</button>
            <button class:on={copy} disabled={!!busy} onclick={() => (copy = true)}>Copy into the servers folder</button>
          </div>
          <div class="tiny muted">{copy ? 'A copy goes in the servers folder; the original stays as it is.' : 'Nothing moves. Other panels (Crafty, MCSS) can keep using it too.'}</div>
        </div>
        {#if copy}
          <div class="f">
            <label class="lbl" for="ns-name">Name</label>
            <div class="input"><input id="ns-name" bind:value={name} placeholder="Survival" disabled={!!busy} /></div>
          </div>
        {/if}
      {:else}
        {#if from === 'template'}
          <div class="results">
            {#each templates as t (t.id)}
              <button class="res row-hover" class:row-sel={tpl === t.id} onclick={() => { tpl = t.id; if (!name.trim()) name = t.name; if (t.memory_mb) memory = t.memory_mb / 1024; }}>
                <span class="ph"><LayoutTemplate size={16} /></span>
                <span class="grow"><b>{t.name}</b> <span class="tiny muted">from {t.from}</span><span class="tiny muted ell blk">{t.loader || 'Minecraft'}{t.minecraft ? ` · Minecraft ${t.minecraft}` : ''} · {t.mods} mod{t.mods === 1 ? '' : 's'} · no world</span></span>
              </button>
            {/each}
          </div>
        {/if}
        {#if from === 'zip'}
          {#if zipPicking}
            <FolderPicker zips onPick={pickZipPath} onCancel={() => (zipPicking = false)} />
          {:else}
            <label class="drop" class:has={zipFile || zipPath} ondragover={(e) => e.preventDefault()} ondrop={(e) => { e.preventDefault(); pickZipFile(e); }}>
              <input type="file" accept=".zip,.mrpack" hidden onchange={pickZipFile} disabled={!!busy} />
              {#if zipFile || zipPath}<FileArchive size={16} /><span class="mono small ell">{zipFile?.name || zipPath}</span>
              {:else}<Upload size={16} /><span>Drop a server pack zip here, or click to pick one</span>{/if}
            </label>
            {#if session.owner}<div class="tiny muted">Or a zip already on {host}: <button class="link" onclick={() => (zipPicking = true)}>browse</button></div>{/if}
          {/if}
          {#if busy && progress > 0 && progress < 1}<div class="bar"><i style:width="{progress * 100}%"></i></div>{/if}
          {#if zipInfo}
            <div class="row wrap"><span class="chip acc">{KIND[zipInfo.kind] || zipInfo.kind}</span>
              {#if zipInfo.mc_version}<span class="chip">Minecraft {zipInfo.mc_version}</span>{/if}
              {#if zipInfo.loader}<span class="chip">{zipInfo.loader}</span>{/if}</div>
            {#if zipInfo.kind === 'curseforge' || zipInfo.kind === 'modrinth'}<div class="tiny muted">Built like desktop: client-only mods left out.</div>{/if}
            {#if needsPick}<div class="small warn">The zip doesn't say which Minecraft and loader it's for: pick them.</div>{@render gameFields()}{/if}
          {/if}
        {:else if from === 'pack'}
          {#if !showSearch && pack}
            <div class="row picked">
              {#if pack.icon}<img class="pi" src={pack.icon} alt="" width="34" height="34" />{:else}<span class="ph"><Package size={16} /></span>{/if}
              <span class="grow"><b>{pack.name}</b> <span class="tiny {pack.source}">{SITE[pack.source]}</span><span class="tiny muted ell blk">{pack.author ? `by ${pack.author}` : pack.summary}</span></span>
              {#if pack.url}<a class="btn sm ghost" href={pack.url} target="_blank" rel="noreferrer noopener" title="Open on {SITE[pack.source]}"><ExternalLink size={13} /></a>{/if}
              <button class="btn sm ghost" disabled={!!busy} onclick={() => (showSearch = true)}>Change</button>
            </div>
          {:else}
            <form class="row search" onsubmit={search}>
              <div class="seg">
                <button type="button" class:on={source === 'both'} disabled={!!busy} title={cfKey ? 'CurseForge and Modrinth together' : 'Modrinth only until a CurseForge key is added'} onclick={() => setSource('both')}>Both</button>
                <button type="button" class:on={source === 'curseforge'} disabled={!cfKey || !!busy} title={cfKey ? '' : 'Needs a CurseForge API key (Access & devices)'} onclick={() => setSource('curseforge')}>CurseForge</button>
                <button type="button" class:on={source === 'modrinth'} disabled={!!busy} onclick={() => setSource('modrinth')}>Modrinth</button>
              </div>
              <div class="input grow"><Search size={13} /><input bind:value={query} oninput={searchSoon} placeholder={source === 'both' ? 'Search modpacks on CurseForge and Modrinth' : `Search modpacks on ${SITE[source]}`} /></div>
            </form>
            <div class="results">
              {#each results as r (r.source + r.id)}
                <button class="res row-hover" class:row-sel={pack?.id === r.id && pack?.source === r.source} onclick={() => choosePack(r)}>
                  {#if r.icon}<img src={r.icon} alt="" width="34" height="34" loading="lazy" />{:else}<span class="ph"><Package size={16} /></span>{/if}
                  <span class="grow"><b>{r.name}</b> {#if source === 'both'}<span class="tiny {r.source}">{SITE[r.source]}</span>{/if} <span class="tiny muted">{r.author}</span>
                    <span class="tiny muted ell blk">{[r.mc_versions?.[0], r.loaders?.[0]].filter(Boolean).join(' ')}{r.mc_versions?.[0] || r.loaders?.[0] ? ' · ' : ''}{r.server_files === false ? 'no server files: built from the client pack, client-only mods left out' : r.server_files ? 'server files' : r.summary}</span></span>
                  {#if r.server_files === false}<span class="flag" title="No official server files: built from the client pack"><Hammer size={13} /></span>{:else if r.server_files}<span class="flag ok" title="The pack author's own server files"><Server size={13} /></span>{/if}
                </button>
              {:else}<div class="small muted empty">{searching ? 'Searching…' : query ? `No packs match “${query}”.` : 'Search for a modpack.'}</div>{/each}
            </div>
          {/if}
          {#if pack}
            <div class="f">
              <label class="lbl" for="ns-pv">Version</label>
              <select id="ns-pv" class="sel" bind:value={packFile} disabled={!!busy || (loadingVersions && !packVersions.length)}>
                {#if !packVersions.length}<option value="">{loadingVersions ? 'Loading versions…' : 'No versions'}</option>{/if}
                {#each packVersions as v (v.id)}<option value={v.id}>{v.name}{v.mc_version ? ` · ${v.mc_version}` : ''}{v.server_files ? ' · server files' : ''}</option>{/each}
              </select>
              {#if pickedVersion}<div class="tiny muted">{pickedVersion.server_files ? `The pack author's server mods and configs, on ${runsOn(pickedVersion)}. Often a big download.` : `Built from the client pack on ${runsOn(pickedVersion)}, client-only mods left out.`}</div>{/if}
            </div>
          {/if}
        {/if}

        {#if from === 'fresh'}{@render gameFields()}{/if}

        <div class="sec">Server</div>
        <div class="grid">
          <div class="f">
            <label class="lbl" for="ns-name">Name</label>
            <div class="input"><input id="ns-name" bind:this={nameEl} bind:value={name} placeholder="Survival" disabled={!!busy} /></div>
          </div>
          <div class="f">
            <label class="lbl" for="ns-port">Port</label>
            <div class="input"><input id="ns-port" type="number" min="1" max="65535" bind:value={port} placeholder="Auto" disabled={!!busy} /></div>
            <div class="tiny muted">Empty: the panel picks a free one.</div>
          </div>
          <div class="f span">
            <div class="row"><label class="lbl" for="ns-mem">Memory</label><span class="sp"></span><span class="small"><b>{Number(memory).toFixed(1)} GB</b>{#if hostGb}<span class="muted"> of {Math.round(hostGb)} GB on {hostInfo.hostname || host}</span>{/if}</span></div>
            <input id="ns-mem" class="range" type="range" min="1" max={Math.max(memMax, memory)} step="0.5" bind:value={memory} disabled={!!busy} />
            {#if hostGb && memory > hostGb * 0.75}<div class="tiny warn">That leaves little for {hostInfo.hostname || 'the host'} and its other servers.</div>{/if}
          </div>
          {#if router?.config?.enabled}
            <div class="f span">
              <label class="lbl" for="ns-addr">Address players join</label>
              <div class="row">
                <div class="input grow"><input id="ns-addr" class="mono" value={addrName} oninput={(e) => (addrEdit = e.currentTarget.value.trim().toLowerCase())} placeholder={domain ? 'survival' : 'survival.example.com (optional)'} disabled={!!busy} /></div>
                {#if domain && !addrName.includes('.')}<span class="mono small muted">.{domain}</span>{/if}
              </div>
              <div class="tiny muted">A name players join by, when the panel's server addresses are on.</div>
            </div>
          {/if}
        </div>
        <div class="row eula small"><label class="row"><input type="checkbox" bind:checked={eula} disabled={!!busy} /><span>I agree to the Minecraft EULA</span></label>
          <a class="btn sm ghost" href="https://aka.ms/MinecraftEULA" target="_blank" rel="noreferrer">Read it</a></div>
      {/if}
    </div>

    <div class="foot">
      {#if error}<div class="err">{error}</div>{/if}
      {#if busy}<div class="busy small"><span class="spinner"></span>{busy}</div>{/if}
      <div class="row">
        <span class="tiny muted hint">{why || (from === 'folder' ? '' : 'Java is found or downloaded for you.')}</span>
        <button class="btn pri" disabled={!canMake} title={why} onclick={make}>{from === 'folder' && !copy ? 'Add server' : 'Create server'}</button>
      </div>
    </div>
  </div>
</div>

<style>
  .grow { flex: 1; min-width: 0; }
  /* One height for every source, so the dialog doesn't jump; the body scrolls. */
  .box { width: min(560px, 100%); height: min(660px, calc(100dvh - var(--dialog-top) - 16px)); display: flex; flex-direction: column; outline: none; flex: none; }
  .box .head { margin: 0; padding: 12px 10px 10px 16px; border-bottom: 1px solid var(--line); font-size: 13px; font-weight: 600; gap: 7px; }
  .body { flex: 1; min-height: 0; overflow: auto; padding: 14px 16px 16px; display: flex; flex-direction: column; gap: 12px; }
  .foot { border-top: 1px solid var(--line); padding: 10px 16px 12px; display: flex; flex-direction: column; gap: 6px; }
  .hint { flex: 1; min-width: 0; }
  .f { display: flex; flex-direction: column; gap: 6px; min-width: 0; }
  .sec { font-size: 12px; font-weight: 600; color: var(--subtle); padding-top: 10px; border-top: 1px solid var(--line); margin-top: 2px; }
  .grid { display: grid; grid-template-columns: 1fr 1fr; gap: 12px 14px; }
  .span { grid-column: 1 / -1; }
  .sel { background: var(--base); border: 1px solid var(--line2); border-radius: 6px; padding: 7px 8px; color: var(--text); width: 100%; font: inherit; }
  .sel:focus { border-color: var(--acc); outline: none; }
  .sel.ghost { border-style: dashed; }
  .range { width: 100%; accent-color: var(--acc); }
  .eula { display: flex; gap: 8px; align-items: center; }
  .eula label { gap: 8px; cursor: pointer; }
  .pi { border-radius: 6px; flex: none; }
  .curseforge { color: var(--acc); }
  .modrinth { color: #1bd96a; }
  .flag { color: var(--gold); flex: none; display: grid; }
  .flag.ok { color: var(--green); }
  .eula input { accent-color: var(--acc); }
  .busy { display: flex; align-items: center; gap: 8px; color: var(--gold); }
  /* Start from: underline tabs (desktop's tab rule), one line, scrolls if it must. */
  .tabs { display: flex; gap: 18px; border-bottom: 1px solid var(--line); overflow-x: auto; scrollbar-width: none; }
  .tabs button { display: inline-flex; align-items: center; gap: 6px; padding: 7px 0; color: var(--subtle); white-space: nowrap; flex: none; }
  .tabs button:hover:not(:disabled) { color: var(--text); }
  .tabs button:disabled { opacity: 0.5; cursor: default; }
  .desc { min-height: 1.45em; }
  /* Secondary choices: a tint, not a filled button (one filled orange per region). */
  .seg { display: flex; border: 1px solid var(--line2); border-radius: 6px; overflow: hidden; flex: none; align-self: flex-start; max-width: 100%; }
  .seg button { padding: 5px 11px; font-size: 12.5px; color: var(--subtle); }
  .seg button + button { border-left: 1px solid var(--line2); }
  .seg button:hover:not(:disabled):not(.on) { background: #ffffff08; }
  .seg button.on { background: var(--acc-dim); color: var(--acc); font-weight: 600; }
  .seg button:disabled { opacity: 0.4; }
  .picked { gap: 8px; border: 1px solid var(--line2); border-radius: 8px; padding: 6px 8px; background: var(--base); }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .blk { display: block; }
  .wrap { flex-wrap: wrap; gap: 6px; }
  .drop { display: flex; align-items: center; justify-content: center; gap: 8px; border: 2px dashed var(--line2); border-radius: 10px; padding: 20px; color: var(--subtle); cursor: pointer; text-align: center; min-width: 0; }
  .drop.has { color: var(--text); border-style: solid; border-width: 1px; padding: 12px; }
  .drop:hover { border-color: #f9731688; }
  .bar { flex: none; }
  .warn { color: var(--gold); }
  .link { color: var(--acc); text-decoration: underline; }
  .search { flex-wrap: wrap; }
  .search .input { min-width: 160px; }
  .results { max-height: 240px; min-height: 0; overflow: auto; display: flex; flex-direction: column; gap: 2px; flex: none; border: 1px solid var(--line); border-radius: 8px; padding: 4px; background: var(--base); }
  .empty { padding: 14px 8px; text-align: center; }
  .res { display: flex; align-items: center; gap: 10px; padding: 6px; border-radius: 6px; text-align: left; min-width: 0; }
  .res img, .ph { width: 34px; height: 34px; border-radius: 6px; background: var(--raise); flex: none; }
  .ph { display: grid; place-items: center; color: var(--muted); }
  @media (max-width: 520px) {
    .box { height: calc(100dvh - var(--dialog-top) - 16px); }
    .grid { grid-template-columns: 1fr; }
    .tabs { gap: 14px; }
    .body { padding: 12px; }
    .foot { padding: 10px 12px; }
  }
</style>
