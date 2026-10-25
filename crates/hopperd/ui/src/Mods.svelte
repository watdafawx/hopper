<script>
  // The Mods tab, laid out like Hopper desktop's: sub-tabs (Mods, Backups),
  // filter chips, rows with update / lock / side chips, and a details panel
  // for the picked mod. Unknown jars are identified in the background and
  // updates are checked on CurseForge and Modrinth on their own.
  import CfKey from './CfKey.svelte';
  import ModPeek from './ModPeek.svelte';
  import ReviewUpdates from './ReviewUpdates.svelte';
  import ModBackups from './ModBackups.svelte';
  import { Package, Search, Plus, ScanSearch, RefreshCw, X, Settings2, Lock, ArrowUpCircle } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { ask } from './lib/dialog.svelte.js';
  import { followJob } from './lib/jobs.js';

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
  let adding = $state(false);
  let query = $state('');
  let source = $state('modrinth');
  let results = $state([]);
  let searching = $state(false);
  let updates = $state(null);
  let checking = $state(false);
  let reviewing = $state(false);
  let picked = $state(null);
  let backupCount = $state(null);

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));
  const mods = $derived(data?.mods || []);
  const counts = $derived({
    all: mods.length,
    on: mods.filter((m) => m.enabled).length,
    off: mods.filter((m) => !m.enabled).length,
    yours: mods.filter((m) => m.source && !m.from_pack).length,
    client: mods.filter((m) => m.side === 'client').length,
    updates: mods.filter((m) => m.update).length,
    unknown: mods.filter((m) => !m.source).length,
  });
  const CHIPS = [
    ['all', 'All'], ['on', 'On'], ['off', 'Off'], ['yours', 'Only yours'],
    ['client', 'Client-only'], ['updates', 'Updates'], ['unknown', 'Unknown'],
  ];
  const fits = {
    all: () => true, on: (m) => m.enabled, off: (m) => !m.enabled, yours: (m) => m.source && !m.from_pack,
    client: (m) => m.side === 'client', updates: (m) => !!m.update, unknown: (m) => !m.source,
  };
  const shown = $derived(mods.filter((m) => fits[chip](m) && (!filter || `${m.name} ${m.file} ${m.author}`.toLowerCase().includes(filter.toLowerCase()))));
  const clientOn = $derived(mods.filter((m) => m.side === 'client' && m.enabled).length);
  const pickedMod = $derived(picked ? mods.find((m) => m.file === picked) : null);

  async function load() {
    try {
      data = await api(`${base}/mods/list`);
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
      if (force && !updates.length) say('Every mod added on top of the pack is up to date.');
    } catch (e) {
      if (force) say(e.message, true);
    } finally {
      checking = false;
    }
  }

  async function applyUpdates(ids) {
    const r = await run('Updating', 'update', { project_ids: ids });
    if (r) {
      say(`Updated ${r.updated.length}.${alive ? ' Restart the server to load them.' : ''} The old jars are in Backups.`);
      reviewing = false;
      updates = null;
      checkUpdates(false);
      loadBackupCount();
    }
  }

  async function toggle(m, e) {
    e?.stopPropagation();
    try {
      const r = await post(`${base}/mods/toggle`, { file: m.file, enabled: !m.enabled });
      if (r.restart_needed) say('Restart the server to apply.');
      load();
    } catch (err) {
      say(err.message, true);
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
    await load();
    loadBackupCount();
    const key = `hopperd-identified-${sid}`;
    let done = false;
    try {
      done = sessionStorage.getItem(key) === '1';
      sessionStorage.setItem(key, '1');
    } catch {
      // private mode: identify every visit
    }
    if (counts.unknown && !done) identify(true);
    else checkUpdates(false);
  }

  $effect(() => {
    sid;
    first();
  });
</script>

<div class="card">
  <nav class="subtabs">
    <button class:on={sub === 'mods'} onclick={() => (sub = 'mods')}>Mods{data ? ` ${counts.all}` : ''}</button>
    <button class:on={sub === 'backups'} onclick={() => (sub = 'backups')}>Backups{backupCount ? ` ${backupCount}` : ''}</button>
    <span class="sp"></span>
    {#if data}
      <button class="chip meta" title="Change" onclick={editMeta}><Settings2 size={11} />{data.game_version || 'version?'} · {data.loader || 'loader?'}</button>
      {#if data.pack}<span class="chip acc" title="Pack mods update through Update from server pack">{data.pack}</span>{/if}
    {/if}
  </nav>

  {#if sub === 'backups'}
    <ModBackups {server} onChanged={() => { load(); loadBackupCount(); }} />
  {:else}
    <div class="row wrap toolbar">
      {#each CHIPS as [k, label] (k)}
        {#if k === 'all' || counts[k]}<button class="fchip" class:on={chip === k} onclick={() => (chip = k)}>{label} {counts[k]}</button>{/if}
      {/each}
      <span class="sp"></span>
      <div class="input find"><Search size={13} /><input placeholder="Filter mods" bind:value={filter} /></div>
      <button class="btn sm" disabled={!!identifying || !counts.unknown} onclick={() => identify(false)} title="Look jars up on CurseForge and Modrinth"><ScanSearch size={13} />Identify{counts.unknown ? ` ${counts.unknown}` : ''}</button>
      <button class="btn sm" disabled={checking} onclick={() => checkUpdates(true)}><RefreshCw size={13} />{checking ? 'Checking…' : 'Check updates'}</button>
      <button class="btn sm pri" disabled={!!busy} onclick={() => (adding = !adding)}><Plus size={13} />Add mods</button>
    </div>
    {#if identifying}<div class="banner small"><span class="spin b"></span>{identifying}… names, icons and updates fill in when it's done.</div>{/if}
    {#if busy}<div class="busy small"><span class="spin"></span>{busy}</div>{/if}
    {#if data && !data.curseforge}<CfKey inline onSaved={() => { load(); checkUpdates(true); }} />{/if}
    {#if clientOn}<div class="small warn">{clientOn} client-only mod{clientOn === 1 ? '' : 's'} on: servers usually don't need {clientOn === 1 ? 'it' : 'them'}, and some won't start with them. <button class="link" onclick={() => (chip = 'client')}>Show</button></div>{/if}
    {#if updates?.length}
      <div class="ups row small"><ArrowUpCircle size={14} /><b>{updates.length} update{updates.length === 1 ? '' : 's'}</b>
        <span class="muted ell">· {updates.slice(0, 3).map((u) => u.name).join(', ')}{updates.length > 3 ? ` +${updates.length - 3}` : ''}</span>
        <span class="sp"></span><button class="btn sm pri" disabled={!!busy} onclick={() => (reviewing = true)}>Review {updates.length} update{updates.length === 1 ? '' : 's'}</button></div>
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
          <button type="button" class="btn sm ghost icon" title="Close" onclick={() => (adding = false)}><X size={13} /></button>
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

    <div class="split">
      <div class="list">
        {#each shown as m (m.file)}
          <div class="mod" class:off={!m.enabled} class:sel={picked === m.file} role="button" tabindex="0" onclick={(e) => !e.target.closest('.sw') && (picked = picked === m.file ? null : m.file)} onkeydown={(e) => e.key === 'Enter' && (picked = m.file)}>
            {#if m.icon}<img src={m.icon} alt="" width="28" height="28" loading="lazy" />{:else}<span class="ph sm"></span>{/if}
            <div class="grow">
              <div class="nm">{m.name}
                {#if m.update}<span class="chip acc" title="Update to {m.update.to}">↑ {m.update.to}</span>{/if}
                {#if m.locked}<span class="chip ts" title="Locked: update checks skip it"><Lock size={10} />locked</span>{/if}
                {#if m.side === 'client'}<span class="chip warn">client-only</span>{/if}
                {#if m.from_pack}<span class="chip">pack</span>{/if}
                {#if !m.source}<span class="chip">unknown</span>{/if}
              </div>
              <div class="tiny muted ell">{m.version ? `${m.version} · ` : ''}{m.source ? `${m.source === 'modrinth' ? 'Modrinth' : 'CurseForge'}${m.author ? ` · ${m.author}` : ''}` : m.file}</div>
            </div>
            <label class="sw" title={m.enabled ? 'Turn off' : 'Turn on'}><input type="checkbox" checked={m.enabled} onchange={(e) => toggle(m, e)} /><span></span></label>
          </div>
        {:else}
          <p class="small muted pad">{data ? (mods.length ? 'No mod fits the filter.' : 'No mods.') : 'Loading…'}</p>
        {/each}
      </div>
      {#if pickedMod}<ModPeek {server} mod={pickedMod} onClose={() => (picked = null)} onChanged={(changed) => { load(); if (changed) { updates = null; checkUpdates(false); loadBackupCount(); } }} />{/if}
    </div>
  {/if}
</div>

{#if reviewing && updates?.length}<ReviewUpdates {server} {updates} {busy} onApply={applyUpdates} onClose={() => (reviewing = false)} />{/if}

<style>
  .card { display: flex; flex-direction: column; gap: 8px; }
  .subtabs { display: flex; align-items: center; gap: 4px; border-bottom: 1px solid var(--line); padding-bottom: 8px; }
  .subtabs > button:not(.chip) { padding: 4px 12px; border-radius: 99px; color: var(--subtle); border: 1px solid transparent; }
  .subtabs > button.on { background: var(--raise); color: var(--text); border-color: var(--line2); font-weight: 600; }
  .toolbar { gap: 6px; }
  .fchip { padding: 3px 10px; border-radius: 99px; border: 1px solid var(--line2); color: var(--subtle); font-size: 12px; }
  .fchip.on { background: var(--raise); color: var(--text); border-color: #f9731688; }
  .meta { gap: 4px; }
  .find { padding: 3px 8px; width: 170px; font-weight: 400; }
  .grow { flex: 1; min-width: 0; }
  .wrap { flex-wrap: wrap; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .nm { font-weight: 500; display: flex; gap: 6px; align-items: center; flex-wrap: wrap; }
  .warn { color: var(--gold); }
  .link { color: var(--acc); text-decoration: underline; font-size: 12px; }
  .banner { display: flex; align-items: center; gap: 8px; border: 1px solid #60a5fa44; background: var(--blue-dim); border-radius: 8px; padding: 6px 10px; color: var(--subtle); }
  .busy { display: flex; align-items: center; gap: 8px; color: var(--gold); }
  .spin { width: 12px; height: 12px; border: 2px solid var(--gold); border-right-color: transparent; border-radius: 50%; animation: s 0.8s linear infinite; flex: none; }
  .spin.b { border-color: var(--blue); border-right-color: transparent; }
  @keyframes s { to { transform: rotate(360deg); } }
  .ups { border: 1px solid #f9731655; background: var(--acc-dim); border-radius: 8px; padding: 6px 10px; color: var(--acc); }
  .ups b { color: var(--text); }
  .split { display: flex; gap: 12px; align-items: flex-start; }
  .list { flex: 1; min-width: 0; }
  .mod { display: flex; align-items: center; gap: 10px; padding: 7px 6px; border-bottom: 1px solid var(--line); cursor: pointer; border-radius: 4px; }
  .mod:hover { background: #ffffff06; }
  .mod.sel { background: var(--acc-dim); box-shadow: inset 3px 0 0 var(--acc); }
  .mod.off { opacity: 0.55; }
  .mod img, .res img { border-radius: 6px; background: var(--raise); flex: none; }
  .ph { width: 34px; height: 34px; border-radius: 6px; background: var(--raise); flex: none; }
  .ph.sm { width: 28px; height: 28px; }
  .pad { padding: 8px 4px; }
  .add { border: 1px solid var(--line2); border-radius: 8px; padding: 10px; display: flex; flex-direction: column; gap: 8px; background: var(--base); }
  .results { max-height: 380px; overflow: auto; }
  .res { display: flex; align-items: center; gap: 10px; padding: 7px 2px; border-bottom: 1px solid var(--line); }
  .seg { display: flex; border: 1px solid var(--line2); border-radius: 6px; overflow: hidden; flex: none; }
  .seg button { padding: 4px 10px; font-size: 12.5px; color: var(--subtle); }
  .seg button.on { background: var(--acc); color: #1a1a1f; font-weight: 600; }
  .seg button:disabled { opacity: 0.4; }
  .icon { padding: 3px 5px; }
  .sw { position: relative; width: 30px; height: 17px; flex: none; cursor: pointer; }
  .sw input { opacity: 0; width: 0; height: 0; }
  .sw span { position: absolute; inset: 0; background: var(--line2); border-radius: 9px; transition: 0.15s; }
  .sw span::after { content: ''; position: absolute; width: 13px; height: 13px; border-radius: 50%; background: #fff; top: 2px; left: 2px; transition: 0.15s; }
  .sw input:checked + span { background: var(--acc); }
  .sw input:checked + span::after { left: 15px; }
</style>
