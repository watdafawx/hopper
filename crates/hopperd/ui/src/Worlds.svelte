<script>
  // A server's worlds, as desktop's Worlds tab shows an instance's: one card
  // per world (size, last played, seed), the one in level-name marked
  // Active. New world, Make active, Reset (backup first), delete, upload,
  // download, and pre-generation with Chunky on the active world.
  import { Globe, FolderOpen, Download, Archive, RotateCcw, Trash2, Plus, Upload, ChevronRight, ChevronDown, Pause, Play, X, AlertTriangle } from '@lucide/svelte';
  import { api, post, del } from './lib/api.js';
  import { bytes } from './lib/format.js';
  import { say } from './lib/toast.svelte.js';
  import { confirmIt } from './lib/dialog.svelte.js';
  import { followJob } from './lib/jobs.js';
  import { download, queueUploads, onUploaded } from './lib/transfers.svelte.js';
  import { onDestroy } from 'svelte';

  let { server, openFiles, openBackups } = $props();
  const sid = $derived(server.id);
  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));

  let data = $state(null);
  let busy = $state('');
  let pgOpen = $state(true);
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

  function ago(secs) {
    if (!secs) return 'Never played';
    const d = Date.now() / 1000 - secs;
    if (d < 120) return 'Played just now';
    if (d < 3600) return `Last played ${Math.round(d / 60)} minutes ago`;
    if (d < 86400 * 2) return `Last played ${Math.round(d / 3600)} hours ago`;
    if (d < 86400 * 60) return `Last played ${Math.round(d / 86400)} days ago`;
    return `Last played ${new Date(secs * 1000).toLocaleDateString()}`;
  }

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
  let pendingUpload = null;
  function pickZip(e) {
    const f = e.target.files?.[0];
    e.target.value = '';
    if (!f) return;
    const name = f.name.replace(/\.zip$/i, '').replace(/[^A-Za-z0-9 _-]/g, '_').slice(0, 60) || 'uploaded';
    pendingUpload = { file: `world-upload-${Date.now()}.zip`, name };
    queueUploads(server, [{ file: f, path: pendingUpload.file }]);
  }
  const off = onUploaded((srv) => {
    if (!pendingUpload || srv?.id !== sid) return;
    const u = pendingUpload;
    pendingUpload = null;
    run(`Unpacking ${u.name}`, () => post(`${base}/worlds/import`, { zip: u.file, name: u.name }), `${u.name} added. Make it active to play it.`);
  });
  onDestroy(off);
</script>

<div class="top row">
  <span class="small muted">{data ? `${data.worlds.length} ${data.worlds.length === 1 ? 'world' : 'worlds'} · ${bytes(data.total_size)}` : 'Loading…'}</span>
  {#if busy}<span class="busy small"><span class="spin"></span>{busy}</span>{/if}
  <span class="sp"></span>
  <button class="btn sm" disabled={!!busy} onclick={() => fileInput.click()} title="Upload a world as a zip"><Upload size={13} />Upload</button>
  <input type="file" accept=".zip" bind:this={fileInput} onchange={pickZip} hidden />
  <button class="btn sm" onclick={openBackups}><Archive size={13} />Backups</button>
  <button class="btn sm pri" disabled={!!busy} onclick={openNew}><Plus size={13} />New world</button>
</div>

{#if data}
  {#each data.worlds as w (w.name)}
    <div class="card world" class:act={w.active}>
      <div class="row">
        <div class="wq" class:act={w.active}><Globe size={20} /></div>
        <div class="t">
          <div class="nm">{w.name}{#if w.active}<span class="chip acc">Active</span>{/if}</div>
          <div class="small muted">
            {w.created ? ago(w.last_played) : 'Made on the next start'}{w.created ? ` · ${bytes(w.size)}` : ''}{#if w.seed} · seed <span class="mono">{w.seed}</span>{:else if !w.created} · random seed{/if}
          </div>
        </div>
        {#if w.created}
          <button class="btn sm icon ghost" title="Open in Files" onclick={() => openFiles(w.name)}><FolderOpen size={14} /></button>
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

      {#if w.active}
        <button class="exp small" onclick={() => (pgOpen = !pgOpen)}>
          {#if pgOpen}<ChevronDown size={14} />{:else}<ChevronRight size={14} />{/if}Pre-generate
          {#if !pgOpen && pgRunning}<span class="chip acc">{pg.percent.toFixed(1)}%</span>
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
    <div class="card empty"><Globe size={22} /><b>No worlds yet</b><span class="small muted">Start the server once to make one, add a new world, or upload one.</span></div>
  {/each}
{/if}

{#if dlg}
  <div class="back" role="presentation" onclick={(e) => e.target === e.currentTarget && !busy && (dlg = null)}>
    <div class="box card">
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
        <div class="row end"><span class="sp"></span><button class="btn" disabled={!!busy} onclick={() => (dlg = null)}>Cancel</button><button class="btn badf" disabled={!!busy || (dlg.mode === 'custom' && !dlg.seed.trim())} onclick={submitReset}><RotateCcw size={13} />Back up and reset</button></div>
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
      {#if busy}<div class="busy small"><span class="spin"></span>{busy}</div>{/if}
    </div>
  </div>
{/if}

<style>
  .top { margin-bottom: 10px; gap: 8px; flex-wrap: wrap; }
  .world { padding: 12px 14px; margin-bottom: 8px; display: flex; flex-direction: column; gap: 6px; }
  .world.act { border-color: #f9731655; }
  .world > .row { gap: 8px; }
  .wq { width: 42px; height: 42px; border-radius: 6px; flex: none; display: grid; place-items: center; background: var(--surface); color: var(--subtle); }
  .wq.act { background: var(--green-dim); color: var(--green); }
  .t { flex: 1; min-width: 0; max-width: 40rem; margin-left: 4px; }
  .t + * { margin-left: auto; }
  .nm { font-weight: 600; font-size: 13.5px; display: flex; align-items: center; gap: 8px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .exp { display: flex; align-items: center; gap: 6px; color: var(--subtle); margin-left: 50px; padding: 2px 0; width: fit-content; }
  .exp:hover { color: var(--text); }
  .pg { margin-left: 50px; background: var(--base); border: 1px solid var(--line); border-radius: 8px; padding: 9px 11px; display: flex; flex-direction: column; gap: 6px; }
  .pg .row { gap: 8px; }
  .bar { flex: 1; height: 7px; border-radius: 99px; background: var(--surface); overflow: hidden; }
  .bar i { display: block; height: 100%; background: var(--acc); transition: width 0.5s; }
  .grow { flex: 1; min-width: 0; }
  .warnc { color: var(--gold); }
  .empty { display: flex; flex-direction: column; align-items: center; gap: 6px; padding: 34px; color: var(--subtle); }
  .bad { color: var(--red); border-color: #ef444466; }
  .badf { background: var(--red); border-color: var(--red); color: #fff; font-weight: 600; }
  .busy { display: flex; align-items: center; gap: 8px; color: var(--gold); }
  .spin { width: 12px; height: 12px; border: 2px solid var(--gold); border-right-color: transparent; border-radius: 50%; animation: s 0.8s linear infinite; }
  @keyframes s { to { transform: rotate(360deg); } }
  .back { position: fixed; inset: 0; background: #0008; display: flex; justify-content: center; align-items: flex-start; z-index: 55; padding: 56px 16px 16px; overflow: auto; }
  .box { width: min(500px, 100%); display: flex; flex-direction: column; gap: 8px; box-shadow: 0 24px 60px #000c; }
  .box h3 { margin: 0 0 4px; display: flex; align-items: center; gap: 8px; }
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
  .tiny { font-size: 11.5px; }
</style>
