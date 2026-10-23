<script>
  import { Package, Search, Plus, ScanSearch, RefreshCw, Trash2, ArrowUpCircle, X, Settings2 } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { ask, confirmIt } from './lib/dialog.svelte.js';
  import { followJob } from './lib/jobs.js';

  let { server } = $props();
  let data = $state(null);
  let filter = $state('');
  let busy = $state('');
  let adding = $state(false);
  let query = $state('');
  let source = $state('modrinth');
  let results = $state([]);
  let searching = $state(false);
  let updates = $state(null);

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));
  const shown = $derived((data?.mods || []).filter((m) => !filter || `${m.name} ${m.file}`.toLowerCase().includes(filter.toLowerCase())));
  const unknown = $derived((data?.mods || []).filter((m) => !m.source).length);
  const clientOnly = $derived((data?.mods || []).filter((m) => m.side === 'client' && m.enabled).length);

  async function load() {
    try {
      data = await api(`${base}/mods/list`);
    } catch (e) {
      say(e.message, true);
    }
  }

  async function run(label, path, body = {}) {
    try {
      const { job } = await post(`${base}/mods/${path}`, body);
      busy = label;
      const r = await followJob(job, (st) => (busy = st.detail || label));
      return r;
    } catch (e) {
      say(e.message, true);
      return null;
    } finally {
      busy = '';
      load();
    }
  }

  async function identify() {
    const r = await run('Identifying', 'identify');
    if (r) say(`Identified ${r.identified}${r.unknown ? `, ${r.unknown} unknown` : ''}`);
  }

  async function toggle(m) {
    try {
      const r = await post(`${base}/mods/toggle`, { file: m.file, enabled: !m.enabled });
      if (r.restart_needed) say('Restart the server to apply.');
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function remove(m) {
    if (!(await confirmIt(`Remove ${m.name}?`, { body: "It's kept in the server's .hopper-backups.", ok: 'Remove', danger: true }))) return;
    try {
      await post(`${base}/mods/remove`, { file: m.file });
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function search(e) {
    e?.preventDefault();
    searching = true;
    try {
      results = (await api(`${base}/mods/search?q=${encodeURIComponent(query)}&source=${source}`)).results;
    } catch (err) {
      say(err.message, true);
      results = [];
    } finally {
      searching = false;
    }
  }

  async function install(r) {
    const res = await run(`Adding ${r.name}`, 'install', { project_id: r.id });
    if (res) {
      say(`Added ${res.added.join(', ')}${alive ? '. Restart to load it.' : ''}`);
      r.installed = true;
    }
  }

  async function checkUpdates() {
    busy = 'Checking for updates';
    try {
      updates = (await api(`${base}/mods/updates`)).updates;
      if (!updates.length) say('Everything added on top of the pack is up to date.');
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = '';
    }
  }

  async function applyUpdates() {
    const r = await run('Updating', 'update', { project_ids: updates.map((u) => u.project_id) });
    if (r) {
      say(`Updated ${r.updated.length}`);
      updates = null;
    }
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

  $effect(() => {
    server.id;
    load();
  });
</script>

<div class="card">
  <h3><Package size={14} />Mods
    {#if data}
      <button class="chip meta" title="Change" onclick={editMeta}><Settings2 size={11} />{data.game_version || 'version?'} · {data.loader || 'loader?'}</button>
      {#if data.pack}<span class="chip acc" title="Pack mods update through Update from server pack">{data.pack}</span>{/if}
    {/if}
    <span class="sp"></span>
    <div class="input find"><Search size={13} /><input placeholder="Filter" bind:value={filter} /></div>
    <button class="btn sm" disabled={!!busy} onclick={identify} title="Look jars up on CurseForge and Modrinth"><ScanSearch size={13} />Identify{unknown ? ` ${unknown}` : ''}</button>
    <button class="btn sm" disabled={!!busy || !data?.curseforge} title={data?.curseforge ? '' : 'Needs a CurseForge key'} onclick={checkUpdates}><RefreshCw size={13} />Updates</button>
    <button class="btn sm pri" disabled={!!busy} onclick={() => (adding = !adding)}><Plus size={13} />Add mods</button>
  </h3>
  {#if busy}<div class="busy small"><span class="spin"></span>{busy}</div>{/if}
  {#if clientOnly}<div class="small warn">{clientOnly} client-only mod{clientOnly === 1 ? '' : 's'} on: servers usually don't need {clientOnly === 1 ? 'it' : 'them'}, and some won't start with them.</div>{/if}

  {#if updates?.length}
    <div class="ups">
      <div class="row"><b class="small">{updates.length} update{updates.length === 1 ? '' : 's'} for mods added on top of the pack</b><span class="sp"></span>
        <button class="btn sm pri" disabled={!!busy} onclick={applyUpdates}><ArrowUpCircle size={13} />Update all</button>
        <button class="btn sm icon ghost" onclick={() => (updates = null)}><X size={13} /></button></div>
      {#each updates as u (u.project_id)}<div class="small">{u.name} <span class="muted">{u.from} → {u.to}</span></div>{/each}
    </div>
  {/if}

  {#if adding}
    <div class="add">
      <form class="row" onsubmit={search}>
        <div class="seg">
          <button type="button" class:on={source === 'modrinth'} onclick={() => (source = 'modrinth')}>Modrinth</button>
          <button type="button" class:on={source === 'curseforge'} disabled={!data?.curseforge} onclick={() => (source = 'curseforge')}>CurseForge</button>
        </div>
        <div class="input grow"><Search size={13} /><input bind:value={query} placeholder="Search mods for {data?.game_version}" /></div>
        <button class="btn sm pri" disabled={searching}>Search</button>
      </form>
      <div class="results">
        {#each results as r (r.id)}
          <div class="res">
            {#if r.icon}<img src={r.icon} alt="" width="34" height="34" loading="lazy" />{:else}<span class="ph"></span>{/if}
            <div class="grow"><div class="nm">{r.name} <span class="tiny muted">{r.author}</span></div><div class="tiny muted ell">{r.summary}</div></div>
            {#if r.installed}<span class="chip running">added</span>{:else}<button class="btn sm" disabled={!!busy} onclick={() => install(r)}><Plus size={13} />Add</button>{/if}
          </div>
        {:else}
          <p class="small muted">{searching ? 'Searching…' : 'Search to find mods. Dependencies are added with them.'}</p>
        {/each}
      </div>
    </div>
  {/if}

  <div class="list">
    {#each shown as m (m.file)}
      <div class="mod" class:off={!m.enabled}>
        {#if m.icon}<img src={m.icon} alt="" width="28" height="28" loading="lazy" />{:else}<span class="ph sm"></span>{/if}
        <div class="grow">
          <div class="nm">{m.name}
            {#if m.side === 'client'}<span class="chip warn">client only</span>{/if}
            {#if m.from_pack}<span class="chip">pack</span>{/if}
            {#if !m.source}<span class="chip">unknown</span>{/if}
          </div>
          <div class="tiny muted ell">{m.version ? `${m.version} · ` : ''}{m.file}</div>
        </div>
        <label class="sw" title={m.enabled ? 'Turn off' : 'Turn on'}><input type="checkbox" checked={m.enabled} onchange={() => toggle(m)} /><span></span></label>
        <button class="btn sm icon ghost" title="Remove" onclick={() => remove(m)}><Trash2 size={13} /></button>
      </div>
    {:else}
      <p class="small muted">{data ? 'No mods.' : 'Loading…'}</p>
    {/each}
  </div>
</div>

<style>
  .card { display: flex; flex-direction: column; gap: 8px; }
  .card h3 { flex-wrap: wrap; margin-bottom: 2px; }
  .meta { gap: 4px; }
  .find { padding: 3px 8px; width: 160px; font-weight: 400; }
  .grow { flex: 1; min-width: 0; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .nm { font-weight: 500; display: flex; gap: 6px; align-items: center; flex-wrap: wrap; }
  .warn { color: var(--gold); }
  .busy { display: flex; align-items: center; gap: 8px; color: var(--gold); }
  .spin { width: 12px; height: 12px; border: 2px solid var(--gold); border-right-color: transparent; border-radius: 50%; animation: s 0.8s linear infinite; }
  @keyframes s { to { transform: rotate(360deg); } }
  .mod { display: flex; align-items: center; gap: 10px; padding: 7px 2px; border-bottom: 1px solid var(--line); }
  .mod.off { opacity: 0.55; }
  .mod img, .res img { border-radius: 6px; background: var(--raise); flex: none; }
  .ph { width: 34px; height: 34px; border-radius: 6px; background: var(--raise); flex: none; }
  .ph.sm { width: 28px; height: 28px; }
  .add { border: 1px solid var(--line2); border-radius: 8px; padding: 10px; display: flex; flex-direction: column; gap: 8px; background: var(--base); }
  .results { max-height: 380px; overflow: auto; }
  .res { display: flex; align-items: center; gap: 10px; padding: 7px 2px; border-bottom: 1px solid var(--line); }
  .seg { display: flex; border: 1px solid var(--line2); border-radius: 6px; overflow: hidden; flex: none; }
  .seg button { padding: 4px 10px; font-size: 12.5px; color: var(--subtle); }
  .seg button.on { background: var(--acc); color: #1a1a1f; font-weight: 600; }
  .seg button:disabled { opacity: 0.4; }
  .ups { border: 1px solid #f9731655; background: var(--acc-dim); border-radius: 8px; padding: 10px; display: flex; flex-direction: column; gap: 4px; }
  .sw { position: relative; width: 30px; height: 17px; flex: none; cursor: pointer; }
  .sw input { opacity: 0; width: 0; height: 0; }
  .sw span { position: absolute; inset: 0; background: var(--line2); border-radius: 9px; transition: 0.15s; }
  .sw span::after { content: ''; position: absolute; width: 13px; height: 13px; border-radius: 50%; background: #fff; top: 2px; left: 2px; transition: 0.15s; }
  .sw input:checked + span { background: var(--acc); }
  .sw input:checked + span::after { left: 15px; }
</style>
