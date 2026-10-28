<script>
  // A server's worlds, as desktop's Worlds tab shows an instance's: one card
  // per world (size, last played, seed), the one in level-name marked
  // Active. New world, Make active, Reset (backup first), delete, upload,
  // download, and pre-generation with Chunky on the active world.
  import { Globe, FolderOpen, Download, Archive, RotateCcw, Trash2, Plus, Upload, ChevronRight, ChevronDown, Pause, Play, X, AlertTriangle, Package } from '@lucide/svelte';
  import { relative, fullDate, plural } from './lib/worlds.js';
  import { api, post, del } from './lib/api.js';
  import { bytes } from './lib/format.js';
  import { say } from './lib/toast.svelte.js';
  import { confirmIt } from './lib/dialog.svelte.js';
  import { followJob } from './lib/jobs.js';
  import { download, queueUploads, transfers } from './lib/transfers.svelte.js';
  import { onDestroy, untrack } from 'svelte';

  let { server, openFiles, openBackups } = $props();
  const sid = $derived(server.id);
  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));

  let data = $state(null);
  let busy = $state('');
  let pgOpen = $state(true);
  // Worlds whose Data packs are shown open (closed at first, as desktop).
  let packsOpen = $state(new Set());
  let dlg = $state(null); // { kind: 'new' | 'reset' | 'pregen', ... }
  let error = $state('');
  let fileInput = $state();

  async function load() {
    try {
      data = await api(`${base}/worlds`);
    } catch (e) {
      say(e.message, true);
    }
  }

  // Once per server and power state; faster while Chunky runs.
  $effect(() => {
    sid;
    server.state;
    load();
  });
  const pg = $derived(data?.pregen);
  const pgRunning = $derived(pg?.state === 'running');
  let timer = setInterval(() => {
    if (pgRunning || busy) load();
  }, 2000);
  onDestroy(() => clearInterval(timer));

  // Desktop's caption: "Last played Today", "Last played 3 days ago".
  const ago = (secs) => (secs ? `Last played ${relative(secs)}` : 'Never played');
  const iconUrl = (w) => `/api/v1/fs/${enc(sid)}/file?path=${enc(`${w.name}/icon.png`)}&inline=1`;

  function togglePacks(w) {
    const s = new Set(packsOpen);
    s.has(w.name) ? s.delete(w.name) : s.add(w.name);
    packsOpen = s;
  }
  // As desktop's Remove: no question, it can be got back (Files › Trash).
  const removePack = (w, p) => run(`Moving ${p.name} to the trash`, () => del(`${base}/worlds/${enc(w.name)}/datapacks/${enc(p.name)}`), `${p.name} moved to the trash (Files › Trash).`);

  async function run(label, start, done) {
    busy = label;
    error = '';
    try {
      const r = await start();
      if (r?.job) await followJob(r.job, (st) => (busy = st.detail || label));
      if (done) say(done);
      dlg = null;
    } catch (e) {
      if (dlg) error = e.message;
      else say(e.message, true);
    } finally {
      busy = '';
      load();
    }
  }

  const enc = encodeURIComponent;
  const makeActive = (w) => run(`Switching to ${w.name}`, () => post(`${base}/worlds/${enc(w.name)}/activate`), `${w.name} loads on the next start.`);
  const backup = (w) => run(`Backing up ${w.name}`, () => post(`${base}/worlds/${enc(w.name)}/backup`), `${w.name} backed up. It's in the Backups tab.`);
  const zip = (w) => download(`/api/v1/fs/${enc(sid)}/zip?${w.folders.map((f) => `path=${enc(f)}`).join('&')}`);

  async function remove(w) {
    const ok = await confirmIt(`Delete ${w.name}?`, {
      body: w.created ? 'A backup is made first (Backups tab), then the world moves to the server\'s trash (Files › Trash) for 30 days.' : 'It hasn\'t been played yet, so nothing is lost.',
      ok: 'Delete world',
      danger: true,
    });
    if (ok) run(`Deleting ${w.name}`, () => del(`${base}/worlds/${enc(w.name)}`), `${w.name} deleted.`);
  }

  // ---- dialogs ----
  function openNew() {
    error = '';
    dlg = { kind: 'new', name: '', seed: '', activate: !alive };
  }
  function openReset(w) {
    error = '';
    dlg = { kind: 'reset', w, mode: w.seed ? 'same' : 'new', seed: '', keep: false };
  }
  function openPregen() {
    error = '';
    dlg = { kind: 'pregen', radius: data?.last_pregen?.radius || 3000, center: 'spawn', x: 0, z: 0 };
  }

  function submitNew() {
    const d = dlg;
    run(`Adding ${d.name}`, () => post(`${base}/worlds`, { name: d.name.trim(), seed: d.seed.trim(), activate: d.activate }),
      d.activate ? `${d.name.trim()} is made on the next start.` : `${d.name.trim()} added. Make it active to play it.`);
  }
  function submitReset() {
    const d = dlg;
    const seed = d.mode === 'custom' ? { mode: 'custom', seed: d.seed.trim() } : { mode: d.mode };
    run(`Resetting ${d.w.name}`, () => post(`${base}/worlds/${enc(d.w.name)}/reset`, { seed, keep_dims: d.keep }),
      `${d.w.name} reset. A fresh one is made on the next start.`);
  }
  function submitPregen() {
    const d = dlg;
    const center = d.center === 'spawn' ? null : d.center === 'origin' ? [0, 0] : [Number(d.x) || 0, Number(d.z) || 0];
    run('Starting pre-generation', () => post(`${base}/worlds/pregen`, { radius: Math.round(Number(d.radius)), center }), 'Pre-generation started.');
  }
  const pregenOp = (op) => run(op === 'cancel' ? 'Cancelling' : op === 'pause' ? 'Pausing' : 'Continuing', () => post(`${base}/worlds/pregen/${op}`));

  const chunks = (r) => Math.pow(Math.ceil((2 * r) / 16), 2);
  const chunkyName = $derived(data?.chunky?.kind ? (data.chunky.kind.kind === 'plugin' ? 'the Paper plugin' : `the ${data.chunky.kind.loader[0].toUpperCase()}${data.chunky.kind.loader.slice(1)} mod`) : '');

  // ---- upload ----
  // The zip goes up through the transfer queue (it shows the progress);
  // once that upload is done it's unpacked into a new world.
  let pendingUpload = $state(null);
  /** A world folder name from the zip's, not one that's taken. */
  function uploadName(file) {
    const stem = file.replace(/\.zip$/i, '').replace(/[^A-Za-z0-9 _-]/g, '_').replace(/^[ .]+|[ .]+$/g, '').slice(0, 56) || 'uploaded';
    const taken = new Set((data?.worlds || []).map((w) => w.name.toLowerCase()));
    let name = stem;
    for (let n = 2; taken.has(name.toLowerCase()); n++) name = `${stem}-${n}`;
    return name;
  }
  function pickZip(e) {
    const f = e.target.files?.[0];
    e.target.value = '';
    if (!f) return;
    if (pendingUpload) return say('A world is already uploading; wait for it to finish.', true);
    pendingUpload = { file: `world-upload-${Date.now()}.zip`, name: uploadName(f.name), server: sid };
    queueUploads(sid, [{ file: f, path: pendingUpload.file }]);
  }
  $effect(() => {
    const u = pendingUpload;
    if (!u) return;
    const t = transfers.list.find((x) => x.server === u.server && x.path === u.file);
    if (t?.state === 'done') {
      pendingUpload = null;
      untrack(() => run(`Unpacking ${u.name}`, () => post(`/servers/${enc(u.server)}/worlds/import`, { zip: u.file, name: u.name }), `${u.name} added. Make it active to play it.`));
    } else if (!t || t.state === 'failed' || t.state === 'cancelled') {
      pendingUpload = null;
      if (t?.state === 'failed') say(`Uploading the world failed: ${t.error}`, true);
    }
  });

  // ---- dialog keys: Escape closes (unless something runs), Tab stays inside ----
  let box = $state();
  const dlgOpen = $derived(!!dlg);
  $effect(() => {
    if (!dlgOpen) return;
    const before = untrack(() => (document.activeElement instanceof HTMLElement ? document.activeElement : null));
    queueMicrotask(() => box?.querySelector('input:not(:disabled), button.pri, button.danger')?.focus());
    return () => before?.isConnected && before.focus();
  });
  function dlgKeys(e) {
    if (!dlg) return;
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      if (!busy) dlg = null;
    } else if (e.key === 'Tab' && box) {
      const els = [...box.querySelectorAll('button:not(:disabled), input:not(:disabled)')];
      if (!els.length) return;
      const [first, last] = [els[0], els[els.length - 1]];
      if (!box.contains(document.activeElement)) {
        e.preventDefault();
        first.focus();
      } else if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    }
  }
</script>

<svelte:window onkeydowncapture={dlgKeys} />

<div class="top row">
  <span class="small muted">{data ? `${plural(data.worlds.length, 'world')} · ${bytes(data.total_size)}` : ''}</span>
  {#if busy}<span class="busy small"><span class="spinner"></span>{busy}</span>{/if}
  <span class="sp"></span>
  <button class="btn sm" disabled={!!busy || !!pendingUpload} onclick={() => fileInput.click()} title="Upload a world as a zip"><Upload size={13} />{pendingUpload ? 'Uploading…' : 'Upload world'}</button>
  <input type="file" accept=".zip" bind:this={fileInput} onchange={pickZip} hidden />
  <button class="btn sm" onclick={openBackups}><Archive size={13} />Backups</button>
  <button class="btn sm pri" disabled={!!busy} onclick={openNew}><Plus size={13} />New world</button>
</div>

{#if data}
  {#each data.worlds as w (w.name)}
    <div class="card world" class:act={w.active}>
      <div class="row">
        {#if w.icon}<img class="wq" src={iconUrl(w)} alt="" />{:else}<div class="wq" class:act={w.active}><Globe size={20} /></div>{/if}
        <div class="t">
          <div class="nm">{w.name}{#if w.active}<span class="chip acc">Active</span>{/if}</div>
          <div class="small muted">
            <span title={w.last_played ? fullDate(w.last_played) : ''}>{w.created ? ago(w.last_played) : 'Made on the next start'}</span>{w.created ? ` · ${bytes(w.size)}` : ''}{#if w.seed} · seed <span class="mono">{w.seed}</span>{:else if !w.created} · random seed{/if}
          </div>
        </div>
        {#if w.created}
          <button class="btn sm icon ghost" title="Open world folder" onclick={() => openFiles(w.name)}><FolderOpen size={14} /></button>
          <button class="btn sm icon ghost" title="Download as zip" onclick={() => zip(w)}><Download size={14} /></button>
          <button class="btn sm" disabled={!!busy} onclick={() => backup(w)}><Archive size={13} />Back up</button>
        {/if}
        {#if w.active}
          {#if w.created}<button class="btn sm" disabled={alive || !!busy} title={alive ? 'Stop the server first' : 'Start this world over'} onclick={() => openReset(w)}><RotateCcw size={13} />Reset…</button>{/if}
        {:else}
          <button class="btn sm" disabled={alive || !!busy} title={alive ? 'Stop the server first' : 'Load this world on the next start'} onclick={() => makeActive(w)}>Make active</button>
          <button class="btn sm icon ghost" disabled={!!busy} title="Delete (backup first, then the trash)" onclick={() => remove(w)}><Trash2 size={14} /></button>
        {/if}
      </div>

      <button class="exp small" onclick={() => togglePacks(w)}>
        {#if packsOpen.has(w.name)}<ChevronDown size={14} />{:else}<ChevronRight size={14} />{/if}Data packs ({(w.datapacks || []).length})
      </button>
      {#if packsOpen.has(w.name)}
        <div class="packs">
          {#each w.datapacks || [] as p (p.name)}
            <div class="dp row-hover">
              <Package size={14} class="muted" />
              <span class="dpn">{p.name}</span>
              {#if !p.dir}<span class="tiny muted">{bytes(p.size)}</span>{/if}
              <button class="btn sm icon ghost" title="Move to the trash (Files › Trash)" disabled={!!busy} onclick={() => removePack(w, p)}><Trash2 size={13} /></button>
            </div>
          {/each}
          <div class="row">
            <a class="btn sm" href="#/browse/datapacks/{enc(sid)}"><Plus size={13} />Add data packs</a>
            <button class="btn sm ghost" onclick={() => openFiles(`${w.name}/datapacks`)}><FolderOpen size={13} />Open folder</button>
          </div>
        </div>
      {/if}

      {#if w.active}
        <button class="exp small" onclick={() => (pgOpen = !pgOpen)}>
          {#if pgOpen}<ChevronDown size={14} />{:else}<ChevronRight size={14} />{/if}Pre-generate
          {#if !pgOpen && pgRunning}<span class="pill acc"><span class="glow"></span>{pg.percent.toFixed(1)}%</span>
          {:else if !pgOpen && data.last_pregen?.world === w.name}<span class="chip">Pre-generated {data.last_pregen.radius.toLocaleString()} blocks</span>{/if}
        </button>
        {#if pgOpen}
          <div class="pg">
            {#if !data.chunky.kind}
              <div class="small muted">Pre-generation uses Chunky, a mod or plugin. This server runs plain Minecraft, which can't load either; a Fabric, NeoForge, Forge, Quilt or Paper server can.</div>
            {:else if pg && (pg.state === 'running' || pg.state === 'paused')}
              <div class="row small">
                <span>{pg.dimension} · {data.last_pregen ? `radius ${data.last_pregen.radius.toLocaleString()} blocks` : ''}</span>
                <span class="sp"></span>
                {#if pg.state === 'running'}<span class="muted">{pg.eta ? `ETA ${pg.eta} · ` : ''}{Math.round(pg.rate)} chunks/s</span>{:else}<span class="warnc">Paused</span>{/if}
              </div>
              <div class="row">
                <div class="bar"><i style="width:{Math.min(100, pg.percent)}%"></i></div>
                <b class="small">{pg.percent.toFixed(1)}%</b>
                {#if pg.state === 'running'}<button class="btn sm" disabled={!!busy} onclick={() => pregenOp('pause')}><Pause size={12} />Pause</button>
                {:else}<button class="btn sm" disabled={!!busy} onclick={() => pregenOp('continue')}><Play size={12} />Continue</button>{/if}
                <button class="btn sm bad" disabled={!!busy} onclick={() => pregenOp('cancel')}><X size={12} />Cancel</button>
              </div>
              <div class="tiny muted">{data.chunky.jar?.split('/').pop()} · {pg.chunks.toLocaleString()} chunks done · keeps going if you leave this page</div>
            {:else}
              <div class="row small">
                <span class="grow muted">
                  {#if pg?.state === 'done'}Done: {pg.chunks.toLocaleString()} chunks generated.
                  {:else if pg?.state === 'cancelled'}Cancelled.
                  {:else if data.last_pregen?.world === w.name}Last run: radius {data.last_pregen.radius.toLocaleString()} blocks, {new Date(data.last_pregen.at * 1000).toLocaleDateString()}.
                  {:else}Generate the land around spawn now, so players don't wait for it (and the server doesn't lag) later.{/if}
                  {#if !data.chunky.jar} Adds Chunky ({chunkyName}) first.{/if}
                </span>
                <button class="btn sm" disabled={!!busy || server.state === 'starting' || server.state === 'stopping'} onclick={openPregen}>Pre-generate…</button>
              </div>
            {/if}
          </div>
        {/if}
      {/if}
    </div>
  {:else}
    <div class="empty-state">
      <span class="es-ic"><Globe size={20} /></span>
      <b>No worlds yet</b>
      <p>Start the server once to create one, or upload a world to start from.</p>
      <button class="btn sm" disabled={!!busy || !!pendingUpload} onclick={() => fileInput.click()}><Upload size={13} />Upload world</button>
    </div>
  {/each}
{:else}
  {#each [0, 1, 2] as i (i)}<div class="skel"><i></i></div>{/each}
{/if}

{#if dlg}
  <div class="dialog-back" role="presentation" onclick={(e) => e.target === e.currentTarget && !busy && (dlg = null)}>
    <div class="box popover" role="dialog" aria-modal="true" aria-label={dlg.kind === 'new' ? 'New world' : dlg.kind === 'reset' ? `Reset ${dlg.w.name}` : 'Pre-generate'} bind:this={box}>
      {#if dlg.kind === 'new'}
        <h3><Plus size={15} />New world<span class="sp"></span><button class="btn sm icon ghost" disabled={!!busy} onclick={() => (dlg = null)}><X size={14} /></button></h3>
        <label class="lbl" for="w-name">Name (its folder)</label>
        <div class="input"><input id="w-name" bind:value={dlg.name} placeholder="event-2026" disabled={!!busy} /></div>
        <label class="lbl" for="w-seed">Seed</label>
        <div class="input"><input id="w-seed" class="mono" bind:value={dlg.seed} placeholder="Empty: random" disabled={!!busy} /></div>
        <label class="cb small"><input type="checkbox" bind:checked={dlg.activate} disabled={alive || !!busy} />Make it the active world{alive ? ' (stop the server first)' : ''}</label>
        <div class="note small">Minecraft makes it the first time the server starts with it active.</div>
        {#if error}<div class="err">{error}</div>{/if}
        <div class="row end"><span class="sp"></span><button class="btn" disabled={!!busy} onclick={() => (dlg = null)}>Cancel</button><button class="btn pri" disabled={!!busy || !dlg.name.trim()} onclick={submitNew}><Plus size={13} />Add world</button></div>
      {:else if dlg.kind === 'reset'}
        <h3><RotateCcw size={15} />Reset {dlg.w.name}?<span class="sp"></span><button class="btn sm icon ghost" disabled={!!busy} onclick={() => (dlg = null)}><X size={14} /></button></h3>
        <div class="small muted">Players get a fresh world on the next start.</div>
        <div class="rad">
          <label class:off={!dlg.w.seed}><input type="radio" bind:group={dlg.mode} value="same" disabled={!dlg.w.seed} /><div>Same seed<div class="tiny muted">{dlg.w.seed ? `${dlg.w.seed}: same terrain, everything built is gone` : 'Seed unknown'}</div></div></label>
          <label><input type="radio" bind:group={dlg.mode} value="new" /><div>New random seed</div></label>
          <label><input type="radio" bind:group={dlg.mode} value="custom" /><div class="row">This seed <div class="input sm"><input class="mono" bind:value={dlg.seed} onfocus={() => (dlg.mode = 'custom')} /></div></div></label>
        </div>
        <label class="cb small"><input type="checkbox" bind:checked={dlg.keep} />Keep the Nether and the End</label>
        <div class="note small">A backup of the world is made first ({bytes(dlg.w.size)}, Backups tab). The old folder goes to the server's trash for 30 days.</div>
        {#if error}<div class="err">{error}</div>{/if}
        <div class="row end"><span class="sp"></span><button class="btn" disabled={!!busy} onclick={() => (dlg = null)}>Cancel</button><button class="btn danger" disabled={!!busy || (dlg.mode === 'custom' && !dlg.seed.trim())} onclick={submitReset}><RotateCcw size={13} />Back up and reset</button></div>
      {:else}
        <h3><Globe size={15} />Pre-generate {data.level}<span class="sp"></span><button class="btn sm icon ghost" disabled={!!busy} onclick={() => (dlg = null)}><X size={14} /></button></h3>
        <div class="small muted">Generates the overworld around a point now, so players don't wait for chunks later.</div>
        <span class="lbl">Radius (blocks)</span>
        <div class="row">
          <div class="seg">{#each [1000, 3000, 5000, 10000] as r}<button class:on={Number(dlg.radius) === r} onclick={() => (dlg.radius = r)}>{r / 1000}k</button>{/each}</div>
          <div class="input sm num"><input type="number" min="16" max="100000" bind:value={dlg.radius} /></div>
          <span class="tiny muted">≈ {chunks(Number(dlg.radius) || 0).toLocaleString()} chunks</span>
        </div>
        <span class="lbl">Center</span>
        <div class="row">
          <div class="seg"><button class:on={dlg.center === 'spawn'} onclick={() => (dlg.center = 'spawn')}>World spawn</button><button class:on={dlg.center === 'origin'} onclick={() => (dlg.center = 'origin')}>0, 0</button><button class:on={dlg.center === 'custom'} onclick={() => (dlg.center = 'custom')}>Custom</button></div>
          {#if dlg.center === 'custom'}<div class="input sm num"><input type="number" bind:value={dlg.x} placeholder="x" /></div><div class="input sm num"><input type="number" bind:value={dlg.z} placeholder="z" /></div>{/if}
        </div>
        {#if !data.chunky.jar}
          <div class="warnbox small"><AlertTriangle size={13} />Chunky isn't installed. Hopper adds {chunkyName} for Minecraft {data.chunky.minecraft || '?'} from Modrinth, then {data.running ? 'restarts' : 'starts'} the server to load it and starts pre-generating.</div>
          {#if data.running && data.players > 0}<div class="err small">{data.players} {data.players === 1 ? 'player is' : 'players are'} online. Installing needs a restart; try again when they've left.</div>{/if}
        {:else if !data.running}
          <div class="note small">The server starts first; pre-generation begins once it's up.</div>
        {/if}
        {#if data.running && data.players > 0 && data.chunky.jar}<div class="warnbox small"><AlertTriangle size={13} />{data.players} online. Chunky runs alongside them but the server may lag a bit while it works.</div>{/if}
        {#if error}<div class="err">{error}</div>{/if}
        <div class="row end"><span class="sp"></span><button class="btn" disabled={!!busy} onclick={() => (dlg = null)}>Cancel</button>
          <button class="btn pri" disabled={!!busy || !(Number(dlg.radius) >= 16) || (!data.chunky.jar && data.running && data.players > 0)} onclick={submitPregen}>{!data.chunky.jar ? (data.running ? 'Install, restart and start' : 'Install, start and run') : data.running ? 'Start' : 'Start server and run'}</button></div>
      {/if}
      {#if busy}<div class="busy small"><span class="spinner"></span>{busy}</div>{/if}
    </div>
  </div>
{/if}

<style>
  .top { margin-bottom: 10px; gap: 8px; flex-wrap: wrap; }
  .world { padding: 12px 14px; margin-bottom: 8px; display: flex; flex-direction: column; gap: 6px; }
  .world.act { border-color: #f9731655; }
  .world > .row { gap: 8px; }
  .wq { width: 48px; height: 48px; border-radius: 6px; flex: none; display: grid; place-items: center; background: var(--raise); color: var(--subtle); object-fit: cover; image-rendering: pixelated; }
  .wq.act { background: var(--green-dim); color: var(--green); }
  .t { flex: 1; min-width: 0; max-width: 40rem; margin-left: 4px; }
  .t + * { margin-left: auto; }
  .nm { font-weight: 600; font-size: 13.5px; display: flex; align-items: center; gap: 8px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .exp { display: flex; align-items: center; gap: 6px; color: var(--subtle); margin-left: 50px; padding: 2px 0; width: fit-content; }
  .exp:hover { color: var(--text); }
  .pg { margin-left: 50px; background: var(--base); border: 1px solid var(--line); border-radius: 8px; padding: 9px 11px; display: flex; flex-direction: column; gap: 6px; }
  .pg .row { gap: 8px; }
  .pg .bar { flex: 1; height: 7px; background: var(--surface); }
  .grow { flex: 1; min-width: 0; }
  .warnc { color: var(--gold); }
  /* As desktop's widgets::empty_state and skeleton_rows. */
  /* The shared loading row, at a world card's height and icon size. */
  .skel { height: 74px; padding: 0 14px; margin-bottom: 8px; border: 1px solid var(--line); border-radius: var(--radius-lg); }
  .skel::before { width: 48px; height: 48px; }
  .packs { margin-left: 50px; display: flex; flex-direction: column; gap: 4px; }
  .dp { display: flex; align-items: center; gap: 10px; padding: 3px 8px; border-radius: 6px; font-size: 12.5px; }
  .dpn { flex: 1; min-width: 0; max-width: 38.5rem; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .busy { display: flex; align-items: center; gap: 8px; color: var(--gold); }
  .box { width: min(500px, 100%); display: flex; flex-direction: column; gap: 8px; padding: 18px; }
  .box h3 { margin: 0 0 4px; display: flex; align-items: center; gap: 8px; font-size: 15px; font-weight: 600; }
  .seg { display: flex; border: 1px solid var(--line2); border-radius: 6px; overflow: hidden; }
  .seg button { padding: 5px 11px; font-size: 12.5px; color: var(--subtle); }
  .seg button + button { border-left: 1px solid var(--line2); }
  .seg button.on { background: var(--acc); color: #1a1a1f; font-weight: 600; }
  .input.sm { padding: 3px 8px; }
  .num { width: 96px; }
  .rad { display: flex; flex-direction: column; gap: 8px; margin: 4px 0; }
  .rad label { display: flex; gap: 8px; align-items: flex-start; font-size: 13px; }
  .rad label.off { opacity: 0.5; }
  .rad input[type='radio'] { margin-top: 3px; accent-color: var(--acc); }
  .cb { display: flex; gap: 8px; align-items: center; }
  .cb input { accent-color: var(--acc); }
  .note { border: 1px solid var(--line); background: var(--base); border-radius: 8px; padding: 8px 10px; color: var(--subtle); }
  .warnbox { display: flex; gap: 8px; align-items: flex-start; border: 1px solid #eab30840; background: var(--gold-dim); border-radius: 8px; padding: 8px 10px; color: #e6d27a; }
  .end { margin-top: 6px; gap: 8px; }
</style>
