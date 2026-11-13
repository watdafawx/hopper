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
  import { bytes } from './lib/format.js';
  import { track } from './lib/making.svelte.js';
  import { uploadIncoming } from './lib/upload.js';
  import { dialog } from './lib/dialog.svelte.js';
  import { rightClear } from './lib/rightclear.js';
  import FolderPicker from './FolderPicker.svelte';
  import { live } from './lib/live.svelte.js';

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
  // The player moved the slider: their choice stays. Until then memory
  // follows the source (2 GB vanilla, 6 GB modded) and a pack's own
  // recommendation decides on the panel (hosting::default_memory_mb).
  let memTouched = $state(false);
  // The line in players' server lists; empty: the name.
  let motd = $state('');
  // Start it once it's made (the server list does, when its row says Ready).
  let startNow = $state(false);
  let port = $state('');
  let router = $state(null);
  let panelHost = $state(null);
  // Where a fresh server is made: this panel ('') or one of its nodes
  // (design/multi-panel.md, A2). Only a fresh one can go to a node for now.
  let where = $state('');
  const nodeChoices = $derived((live.nodes || []).filter((n) => n.ceiling === 'manage'));
  const whereNode = $derived(where && from === 'fresh' ? nodeChoices.find((n) => n.id === where) : null);
  const onNode = (path) => (whereNode ? `${path}${path.includes('?') ? '&' : '?'}node=${encodeURIComponent(whereNode.id)}` : path);
  // The machine it lands on: its memory and disk are the ones that count.
  const hostInfo = $derived(whereNode ? whereNode.host : panelHost);
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
  // Which versions to offer (see hosting::pack_versions): server files only
  // when the pack has any, the client-only rest behind a toggle, and a
  // warning when the newest version has none yet.
  let versionView = $state(null);
  let showClient = $state(false);
  const offered = $derived(
    showClient || !versionView?.shown ? packVersions : packVersions.filter((v) => versionView.shown.includes(v.id) || v.id === packFile),
  );
  const newestClient = $derived(packVersions.find((v) => v.id === versionView?.newest_client));
  let cfKey = $state(true);
  // A pack from Browse shows as picked; Change opens the search.
  let showSearch = $state(true);
  let searchTimer;
  // What of the picked CurseForge version Hopper may download (see /packs/check):
  // website-only server files fall back to the client pack; a website-only
  // pack needs a zip the player downloads.
  let check = $state(null);

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
              : !!pack && !!packFile && check?.choice !== 'needs_zip')),
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
    if (from === 'pack' && check?.choice === 'needs_zip') return 'Download the pack from CurseForge, then pick the zip.';
    if (from === 'fresh' || needsPick) {
      if (!mc) return 'Pick a Minecraft version.';
      if (!OPTIONAL_VERSION.includes(loader) && !lv) return loadingLoaders ? 'Loading builds' : `No ${loaderLabel(loader)} builds for ${mc}: pick another version.`;
    }
    if (!name.trim()) return 'Name the server.';
    if (!eula) return 'Agree to the Minecraft EULA to make a server.';
    return '';
  });
  // What Create downloads first (bytes), when known: the server pack or
  // pack zip of the picked version, or the zip being uploaded.
  const download = $derived(
    from === 'pack' ? check?.download || pickedVersion?.size || 0 : from === 'zip' ? zipFile?.size || 0 : 0,
  );
  // Room for it: the zip, what it unpacks to and the server (as
  // hosting::room_needed works it out). Blocks Create when short.
  const room = $derived.by(() => {
    const free = hostInfo?.disk_free || 0;
    const need = download * 3 + 512 * 2 ** 20;
    return free && download && need > free ? `Not enough space on ${hostInfo.hostname || host}: this needs about ${bytes(need)}, and ${bytes(free)} is free.` : '';
  });
  // What Create will do, in one line, as desktop's dialog says it.
  const summary = $derived.by(() => {
    if (from === 'folder') return '';
    const parts = [];
    if (download) parts.push(`About ${bytes(download)} to download`);
    else if (from === 'fresh') parts.push('Downloads the server');
    if (hostInfo?.disk_free) parts.push(`${bytes(hostInfo.disk_free)} free on ${hostInfo.hostname || host}`);
    parts.push('RCON on');
    if (from === 'pack' || from === 'zip') parts.push('client-only mods stay out');
    return parts.join(' · ');
  });
  // A port another server here uses (or something listens on): say so and
  // offer a free one, as desktop's New server does. Empty port: the panel picks.
  let portCheck = $state(null);
  // The port an empty Port box gets, and this machine's LAN address, so the
  // form can say where players will join.
  let autoInfo = $state(null);
  $effect(() => {
    const path = onNode('/servers/port-check');
    api(path).then((r) => (autoInfo = r), () => (autoInfo = null));
  });
  const joinAt = $derived(address || (autoInfo?.lan && (port || autoInfo.auto) ? `${autoInfo.lan}:${port || autoInfo.auto}` : ''));
  let portTimer;
  $effect(() => {
    const p = Number(port);
    clearTimeout(portTimer);
    if (!port || !Number.isInteger(p) || p < 1 || p > 65535) { portCheck = null; return; }
    const check = onNode(`/servers/port-check?port=${p}`);
    portTimer = setTimeout(() => api(check).then((r) => { if (Number(port) === p) portCheck = r; }, () => (portCheck = null)), 300);
  });
  api('/router').then((r) => (router = r), () => {});
  api('/templates').then((r) => (templates = r.templates || []), () => {});
  api('/settings/curseforge').then((r) => (cfKey = !!r.set), () => {});
  api('/host').then((r) => (panelHost = r), () => {});

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

  // loop-safe: loadLoaders steps `mc` back at most 5 releases (i < 5), then stops
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
    if (memTouched) return;
    if (from === 'fresh') memory = loader === 'vanilla' || PLUGINS.includes(loader) ? 2 : 6;
    else if (from === 'pack') memory = 6;
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
      if (zipInfo.recommended_mb && !memTouched) memory = Math.ceil(zipInfo.recommended_mb / 512) / 2;
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

  let checkSeq = 0;
  $effect(() => {
    const p = pack, f = packFile;
    check = null;
    if (!p || !f || p.source !== 'curseforge') return;
    const my = ++checkSeq;
    api(`/packs/check?${new URLSearchParams({ project: p.id, file: f, source: p.source })}`).then(
      (r) => my === checkSeq && (check = r),
      () => {},
    );
  });
  /** Opens the website-only file's page and switches to Zip, for the download. */
  function useZipFrom(url) {
    window.open(url, '_blank', 'noopener,noreferrer');
    pickFrom('zip');
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
    versionView = null;
    showClient = false;
    error = '';
    // Picked: the search folds away (Change opens it again), like a pack from Browse.
    showSearch = false;
    loadingVersions = true;
    try {
      const res = await api(`/packs/versions?${new URLSearchParams({ project: r.id, source: r.source })}`);
      let list = res.versions || [];
      if (my !== packSeq) return;
      // An older version picked in Browse may be past the newest 50.
      if (version && !list.some((v) => v.id === version.id)) list = [version, ...list];
      packVersions = list;
      versionView = res.shown ? res : null;
      const pickable = versionView ? list.filter((v) => versionView.shown.includes(v.id)) : list;
      packFile = (file && list.some((v) => v.id === file) ? file : '') || (pickable.find((v) => /release/i.test(v.type)) || pickable[0])?.id || '';
      // A version from Browse without server files: show the client ones.
      if (packFile && versionView && !versionView.shown.includes(packFile)) showClient = true;
    } catch (err) {
      if (my === packSeq) error = err.message;
    } finally {
      if (my === packSeq) loadingVersions = false;
    }
  }

  async function make() {
    if (!canMake) return;
    error = '';
    // Memory left on the default for a pack: null, so the panel takes the
    // pack's own recommendation once it has read the pack.
    const memory_mb = from === 'pack' && !memTouched ? null : Math.round(memory * 1024);
    const common = { name: name.trim(), memory_mb, port: port ? Number(port) : null, accept_eula: eula, address, motd: motd.trim() };
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
        if (from === 'fresh') req = post(onNode('/servers/create'), { ...common, mc_version: mc, loader, loader_version: loader === 'vanilla' ? '' : lv });
        else if (from === 'template') req = post('/servers/from-template', { ...common, template: tpl });
        else if (from === 'zip') req = post('/servers/import-zip', { ...common, upload: upload || null, path: zipPath || null, ...(needsPick ? { mc_version: mc, loader: loaderId() } : {}) });
        else req = post('/servers/create-from-pack', { ...common, source: pack.source, project: pack.id, file: packFile });
        const { job } = await req;
        // The server list shows it from here, with progress and Cancel, as
        // desktop does; the dialog is done.
        track(job, name.trim() || pack?.name || 'New server', startNow);
        close();
        return;
      }
      if (from === 'folder' && r?.warnings?.length) say(`${!copy ? 'Added. It runs where it is. ' : ''}${r.warnings.join(' ')}`, true);
      else say(from === 'folder' && !copy ? 'Added. It runs where it is.' : `Made ${name.trim() || 'the server'}${r?.left_out ? `: ${r.left_out} client-only mods left out (see left-out-client-mods.txt)` : ''}${r?.missing ? `. ${r.missing} mods can only be downloaded from CurseForge: see missing-mods.txt in the server folder` : ''}`);
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
      <span class="lbl" id="ns-loader">Type</span>
      <!-- A value picker: the segmented control, as desktop's New server and New instance. -->
      <div class="seg" role="radiogroup" aria-labelledby="ns-loader">
        {#each ['vanilla', ...LOADER_GROUPS.flatMap(([, ids]) => ids)] as id (id)}
          <button role="radio" aria-checked={loader === id} class:on={loader === id} disabled={!!busy} onclick={() => (loader = id)}>{id === 'vanilla' ? 'Vanilla' : loaderLabel(id)}</button>
        {/each}
      </div>
    </div>
    {#if loader === 'vanilla'}
      <!-- Kept, so switching type doesn't move the fields below. -->
      <div class="f"><span class="lbl">Loader version</span><div class="small muted plain">None: plain Minecraft.</div></div>
    {:else}
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
          <div class="f"><span class="lbl">Template</span>
          <div class="results">
            {#each templates as t (t.id)}
              <button class="res row-hover" class:row-sel={tpl === t.id} onclick={() => { tpl = t.id; if (!name.trim()) name = t.name; if (t.memory_mb) memory = t.memory_mb / 1024; }}>
                <span class="ph"><LayoutTemplate size={16} /></span>
                <span class="grow"><b>{t.name}</b> <span class="tiny muted">from {t.from}</span><span class="tiny muted ell blk">{t.loader || 'Minecraft'}{t.minecraft ? ` · Minecraft ${t.minecraft}` : ''} · {t.mods} mod{t.mods === 1 ? '' : 's'} · no world</span></span>
              </button>
            {/each}
          </div>
          </div>
        {/if}
        {#if from === 'zip'}
          <div class="f"><span class="lbl">Zip</span>
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
            {#if needsPick}<div class="small warn">The zip doesn't say which Minecraft and loader it's for: pick them.</div>{/if}
          {/if}
          </div>
          {#if zipInfo && needsPick}{@render gameFields()}{/if}
        {:else if from === 'pack'}
          <div class="f"><span class="lbl">Modpack</span>
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
              <div class="input grow"><Search size={13} /><input use:rightClear bind:value={query} oninput={searchSoon} placeholder={source === 'both' ? 'Search modpacks on CurseForge and Modrinth' : `Search modpacks on ${SITE[source]}`} /></div>
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
          </div>
          {#if pack}
            <div class="f">
              <label class="lbl" for="ns-pv">Version</label>
              <select id="ns-pv" class="sel" bind:value={packFile} disabled={!!busy || (loadingVersions && !packVersions.length)}>
                {#if !packVersions.length}<option value="">{loadingVersions ? 'Loading versions…' : 'No versions'}</option>{/if}
                {#each offered as v (v.id)}<option value={v.id}>{v.name}{v.mc_version ? ` · ${v.mc_version}` : ''}{showClient && versionView && !v.server_files ? ' · client pack' : ''}</option>{/each}
              </select>
              {#if versionView?.hidden_text}<div class="tiny muted row vv">{versionView.hidden_text}<button class="linkish" onclick={() => { showClient = !showClient; if (!showClient && !versionView.shown.includes(packFile)) packFile = versionView.shown[0] || packFile; }}>{showClient ? 'Server files only' : 'Show client versions'}</button></div>{/if}
              {#if versionView?.warning && newestClient && packFile !== newestClient.id}<div class="tiny warn row vv"><span>{versionView.warning}</span><button class="btn sm" disabled={!!busy} onclick={() => { showClient = true; packFile = newestClient.id; }}>Build {newestClient.name} from the client pack</button></div>{/if}
              {#if pickedVersion}<div class="tiny muted">{pickedVersion.server_files && check?.choice !== 'client_pack' ? `The pack author's server mods and configs, on ${runsOn(pickedVersion)}. Often a big download.` : `Built from the client pack on ${runsOn(pickedVersion)}, client-only mods left out.`}</div>{/if}
              {#if check?.choice === 'needs_zip' && check.pack}
                <div class="notice small">
                  <span><b>{pack.name}</b> can only be downloaded from the CurseForge website. Download {check.pack.file_name} there, then make the server from the zip.</span>
                  <div class="row"><button class="btn sm" onclick={() => useZipFrom(check.pack.url)}><ExternalLink size={13} />Download on CurseForge</button><button class="btn sm ghost" onclick={() => pickFrom('zip')}>I have the zip</button></div>
                </div>
              {:else if check?.server_files}
                <div class="notice small">
                  <span>The pack's server files can't be downloaded outside CurseForge, so Hopper builds the server from the pack itself.</span>
                  <div class="row"><button class="btn sm ghost" onclick={() => useZipFrom(check.server_files.url)}><ExternalLink size={13} />Use the server files anyway</button></div>
                </div>
              {/if}
            </div>
          {/if}
        {/if}

        {#if from === 'fresh' && nodeChoices.length}
          <div class="f">
            <span class="lbl" id="ns-where">Make it on</span>
            <div class="seg" role="radiogroup" aria-labelledby="ns-where">
              <button role="radio" aria-checked={!where} class:on={!where} disabled={!!busy} onclick={() => (where = '')}>This panel</button>
              {#each nodeChoices as n (n.id)}
                {@const blocked = n.state !== 'connected' ? `${n.name} is offline` : n.maintenance ? `${n.name} is under maintenance` : ''}
                <button role="radio" aria-checked={where === n.id} class:on={where === n.id} disabled={!!busy || !!blocked} title={blocked} onclick={() => (where = n.id)}>{n.name}</button>
              {/each}
            </div>
            {#if whereNode}<div class="tiny muted">It runs on {whereNode.name}, in that machine's servers folder. You use it from here like the others.</div>{/if}
          </div>
        {/if}
        {#if from === 'fresh'}{@render gameFields()}{/if}

        <div class="grid">
          <div class="f">
            <label class="lbl" for="ns-name">Name</label>
            <div class="input"><input id="ns-name" bind:this={nameEl} bind:value={name} placeholder="Survival" disabled={!!busy} /></div>
          </div>
          <div class="f">
            <label class="lbl" for="ns-motd">MOTD</label>
            <div class="input"><input id="ns-motd" bind:value={motd} placeholder="Same as the name" disabled={!!busy} /></div>
            <div class="tiny muted">The line players see under the server in their list.</div>
          </div>
          <div class="f">
            <label class="lbl" for="ns-port">Port</label>
            <div class="input"><input id="ns-port" type="number" min="1" max="65535" bind:value={port} placeholder={autoInfo?.auto ? `Auto: ${autoInfo.auto}` : 'Auto'} disabled={!!busy} /></div>
            {#if portCheck?.message}<div class="tiny warn row port-warn"><span>{portCheck.message}</span>{#if portCheck.suggest}<button class="btn sm" disabled={!!busy} onclick={() => (port = portCheck.suggest)}>Use {portCheck.suggest}</button>{/if}</div>{/if}
            <div class="tiny muted">{autoInfo?.auto ? `Empty: the panel uses ${autoInfo.auto}.` : 'Empty: the panel picks a free one.'}{joinAt ? ` Players join at ${joinAt}.` : ''}</div>
          </div>
          <div class="f span">
            <label class="lbl" for="ns-mem">Memory</label>
            <div class="row mem"><input id="ns-mem" class="range" type="range" min="1" max={Math.max(memMax, memory)} step="0.5" bind:value={memory} oninput={() => (memTouched = true)} disabled={!!busy} />
              <span class="small nowrap"><b>{Number(memory).toFixed(1)} GB</b>{#if hostGb}{' '}<span class="muted">of {Math.round(hostGb)} GB on {hostInfo.hostname || host}</span>{/if}</span></div>
            {#if from === 'pack' && !memTouched}<div class="tiny muted">The pack's own recommendation is used when it gives one.</div>{/if}
            {#if hostGb && memory > hostGb * 0.75}<div class="tiny warn">That leaves little for {hostInfo.hostname || 'the host'} and its other servers.</div>{/if}
          </div>
          {#if router?.config?.enabled}
            <div class="f span">
              <label class="lbl" for="ns-addr">Address</label>
              <div class="row">
                <div class="input grow"><input id="ns-addr" class="mono" value={addrName} oninput={(e) => (addrEdit = e.currentTarget.value.trim().toLowerCase())} placeholder={domain ? 'survival' : 'survival.example.com (optional)'} disabled={!!busy} /></div>
                {#if domain && !addrName.includes('.')}<span class="mono small muted">.{domain}</span>{/if}
              </div>
              <div class="tiny muted">A name players join by, when the panel's server addresses are on.</div>
            </div>
          {/if}
        </div>
        <div class="f"><span class="lbl">EULA</span>
          <div class="row eula small"><label class="row"><input type="checkbox" bind:checked={eula} disabled={!!busy} /><span>I agree to the Minecraft EULA</span></label>
            <a class="btn sm ghost" href="https://aka.ms/MinecraftEULA" target="_blank" rel="noreferrer">Read it</a></div>
          <div class="row eula small"><label class="row"><input type="checkbox" bind:checked={startNow} disabled={!!busy} /><span>Start it when it's ready</span></label></div>
        </div>
      {/if}
    </div>

    <div class="foot">
      {#if error}<div class="err">{error}</div>{/if}
      {#if busy}<div class="busy small"><span class="spinner"></span>{busy}</div>{/if}
      {#if summary}<div class="tiny muted">{summary} · Java is found or downloaded for you</div>{/if}
      <div class="row">
        <span class="tiny muted hint" class:warn={!!room}>{room || why}</span>
        <button class="btn" disabled={!!busy} onclick={close}>Cancel</button>
        <button class="btn pri" disabled={!canMake || !!room} title={room || why} onclick={make}>{from === 'folder' && !copy ? 'Add server' : 'Create server'}</button>
      </div>
    </div>
  </div>
</div>

<style>
  .grow { flex: 1; min-width: 0; }
  /* One height for every source, so the dialog doesn't jump; the body scrolls. */
  .box { width: min(680px, 100%); height: min(700px, calc(100dvh - var(--dialog-top) - 16px)); display: flex; flex-direction: column; outline: none; flex: none; }
  .box .head { margin: 0; padding: 12px 10px 10px 16px; border-bottom: 1px solid var(--line); font-size: 13px; font-weight: 600; gap: 7px; }
  .body { flex: 1; min-height: 0; overflow: auto; padding: 14px 16px 16px; display: flex; flex-direction: column; gap: 12px; }
  .foot { border-top: 1px solid var(--line); padding: 10px 16px 12px; display: flex; flex-direction: column; gap: 6px; }
  .hint { flex: 1; min-width: 0; }
  /* Desktop's form: a label column, the field and its notes beside it. */
  .f { display: grid; grid-template-columns: 112px minmax(0, 1fr); column-gap: 14px; row-gap: 6px; align-items: start; min-width: 0; }
  /* Labels read like desktop's: the field's name, not a section header. */
  .f > .lbl { grid-column: 1; grid-row: 1; padding-top: 7px; text-transform: none; letter-spacing: 0; font-size: 13px; font-weight: 600; color: var(--text); }
  .f > :not(.lbl) { grid-column: 2; }
  .sec { display: none; }
  .grid { display: flex; flex-direction: column; gap: 12px; }
  .mem { gap: 10px; align-items: center; }
  .mem .range { flex: 1; min-width: 0; }
  .nowrap { white-space: nowrap; }
  .plain { padding-top: 7px; }
  .foot .row { gap: 8px; }
  .sel { background: var(--base); border: 1px solid var(--line2); border-radius: var(--radius); padding: 7px 8px; color: var(--text); width: 100%; font: inherit; }
  .sel:focus { border-color: var(--acc); outline: none; }
  .sel.ghost { border-style: dashed; }
  .range { width: 100%; accent-color: var(--acc); }
  .eula { display: flex; gap: 8px; align-items: center; }
  .vv { gap: 8px; align-items: center; flex-wrap: wrap; margin-top: 4px; }
  .linkish { background: none; border: 0; padding: 0; color: var(--acc); cursor: pointer; font: inherit; }
  .port-warn { gap: 8px; align-items: center; flex-wrap: wrap; margin-top: 4px; }
  .eula label { gap: 8px; cursor: pointer; }
  .pi { border-radius: var(--radius); flex: none; }
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
  .seg { display: flex; border: 1px solid var(--line2); border-radius: var(--radius); overflow: hidden; flex: none; align-self: flex-start; max-width: 100%; }
  .seg button { padding: 5px 11px; font-size: 12.5px; color: var(--subtle); }
  .seg button + button { border-left: 1px solid var(--line2); }
  .seg button:hover:not(:disabled):not(.on) { background: #ffffff08; }
  .seg button.on { background: var(--acc-dim); color: var(--acc); font-weight: 600; }
  .seg button:disabled { opacity: 0.4; }
  .picked { gap: 8px; border: 1px solid var(--line2); border-radius: var(--radius-lg); padding: 6px 8px; background: var(--base); }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .blk { display: block; }
  .wrap { flex-wrap: wrap; gap: 6px; }
  .drop { display: flex; align-items: center; justify-content: center; gap: 8px; border: 2px dashed var(--line2); border-radius: var(--radius-lg); padding: 20px; color: var(--subtle); cursor: pointer; text-align: center; min-width: 0; }
  .drop.has { color: var(--text); border-style: solid; border-width: 1px; padding: 12px; }
  .drop:hover { border-color: #f9731688; }
  .bar { flex: none; }
  .warn { color: var(--gold); }
  .notice { display: flex; flex-direction: column; gap: 8px; padding: 8px 10px; border-radius: var(--radius-lg); background: #eab30814; color: var(--text); }
  .notice .row { gap: 8px; flex-wrap: wrap; }
  .link { color: var(--acc); text-decoration: underline; }
  .search { flex-wrap: wrap; }
  .search .input { min-width: 160px; }
  .results { max-height: 240px; min-height: 0; overflow: auto; display: flex; flex-direction: column; gap: 2px; flex: none; border: 1px solid var(--line); border-radius: var(--radius-lg); padding: 4px; background: var(--base); }
  .empty { padding: 14px 8px; text-align: center; }
  .res { display: flex; align-items: center; gap: 10px; padding: 6px; border-radius: var(--radius); text-align: left; min-width: 0; }
  .res img, .ph { width: 34px; height: 34px; border-radius: var(--radius); background: var(--raise); flex: none; }
  .ph { display: grid; place-items: center; color: var(--muted); }
  @media (max-width: 520px) {
    .box { height: calc(100dvh - var(--dialog-top) - 16px); }
    .f { display: flex; flex-direction: column; gap: 6px; }
    .tabs { gap: 14px; }
    .body { padding: 12px; }
    .foot { padding: 10px 12px; }
  }
</style>
