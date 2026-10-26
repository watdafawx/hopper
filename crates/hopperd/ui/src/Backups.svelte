<script>
  // A server's backups: the list (each new one only stores what changed),
  // and a rail with the space they take, which ones are kept, and copies
  // somewhere else.
  import { Archive, Database, RotateCcw, Trash2, Download, Plus, FolderOpen, X, HardDrive, Clock, CloudUpload, Check, RefreshCw, AlertTriangle, Undo2 } from '@lucide/svelte';
  import { api, post, del } from './lib/api.js';
  import { bytes } from './lib/format.js';
  import { say } from './lib/toast.svelte.js';
  import { ask, confirmIt } from './lib/dialog.svelte.js';
  import { followJob } from './lib/jobs.js';
  import { download } from './lib/transfers.svelte.js';

  let { server, openSchedule } = $props();
  // The live feed hands over a new `server` object every second; effects
  // that should run once per server key on the id alone.
  const sid = $derived(server.id);
  let data = $state(null);
  let busy = $state('');
  let browsing = $state(null);
  let entries = $state([]);
  let pick = $state(new Set());
  let filter = $state('');
  let keepEdit = $state(null);
  let copiesEdit = $state(null);
  let remote = $state(null);

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));
  const shown = $derived(entries.filter((e) => !filter || e[0].toLowerCase().includes(filter.toLowerCase())).slice(0, 400));
  const isZip = (b) => b.name.endsWith('.zip');
  const onTargets = $derived((data?.targets || []).filter((t) => t.on && t.enabled));
  const saved = $derived(data && data.as_zips > data.total_size ? data.as_zips - data.total_size : 0);

  async function load() {
    try {
      data = await api(`${base}/backups`);
    } catch (e) {
      say(e.message, true);
    }
  }

  /** Where one backup's off-site copies stand. */
  function copies(b) {
    if (isZip(b) || !onTargets.length) return null;
    const done = onTargets.filter((t) => t.done.includes(b.name));
    const failing = onTargets.some((t) => t.last_error && !t.done.includes(b.name));
    return { done: done.length, of: onTargets.length, failing, names: onTargets.map((t) => `${t.name}: ${t.done.includes(b.name) ? 'copied' : t.last_error ? t.last_error : t.working === b.name ? 'copying now' : 'waiting'}`).join('\n') };
  }

  async function run(title, job, done) {
    busy = title;
    try {
      const r = await followJob(job, (st) => (busy = st.detail || title));
      done?.(r);
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = '';
      load();
    }
  }

  async function make(worldsOnly) {
    const note = await ask(worldsOnly ? 'Back up the worlds' : 'Back up everything', '', {
      body: alive ? 'The world is saved first and saving is paused while the backup is made.' : 'A note to find it by (optional).',
      ok: 'Back up',
      placeholder: 'Before the 1.21 update',
    });
    if (note === null) return;
    try {
      const { job } = await post(`${base}/backups`, { worlds_only: worldsOnly, note });
      run('Starting', job, (b) => say(`Backed up: ${b.added ? `${bytes(b.added)} new` : 'nothing changed since the last one'} · ${b.files} files`));
    } catch (e) {
      say(e.message, true);
    }
  }

  async function restore(b, paths = []) {
    if (alive) {
      say(`Stop ${server.name} first.`, true);
      return;
    }
    const all = !paths.length;
    const ok = await confirmIt(all ? `Restore ${when(b.at)}?` : `Restore ${paths.length} item${paths.length === 1 ? '' : 's'}?`, {
      body: all
        ? 'Files in the backup replace the ones on the server. Worlds in it are replaced whole; the current ones are moved aside, not deleted.'
        : 'Those files replace the ones on the server.',
      ok: 'Restore',
      danger: true,
    });
    if (!ok) return;
    try {
      const { job } = await post(`${base}/backups/${encodeURIComponent(b.name)}/restore`, { paths });
      run('Restoring', job, (r) => {
        say(`Restored ${r.files} files`);
        browsing = null;
      });
    } catch (e) {
      say(e.message, true);
    }
  }

  async function remove(b) {
    if (!(await confirmIt(`Delete the backup from ${when(b.at)}?`, { body: "It can't be brought back from here. Off-site copies of it go once a newer backup is copied.", ok: 'Delete', danger: true }))) return;
    try {
      await del(`${base}/backups/${encodeURIComponent(b.name)}`);
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function convert(b) {
    if (!(await confirmIt('Convert to the new format?', { body: 'It then shares its data with the other backups (taking less space) and can be copied off-site. The zip goes once that is done.', ok: 'Convert' }))) return;
    try {
      const { job } = await post(`${base}/backups/${encodeURIComponent(b.name)}/convert`);
      run('Converting', job, () => say('Converted'));
    } catch (e) {
      say(e.message, true);
    }
  }

  async function convertAll() {
    for (const b of (data?.backups || []).filter(isZip)) {
      try {
        const { job } = await post(`${base}/backups/${encodeURIComponent(b.name)}/convert`);
        busy = 'Converting';
        await followJob(job, (st) => (busy = st.detail || 'Converting'));
      } catch (e) {
        say(e.message, true);
        break;
      }
    }
    busy = '';
    load();
  }

  async function browse(b) {
    browsing = b;
    pick = new Set();
    filter = '';
    entries = [];
    try {
      entries = (await api(`${base}/backups/${encodeURIComponent(b.name)}/entries`)).entries;
    } catch (e) {
      say(e.message, true);
    }
  }

  function toggle(p) {
    const s = new Set(pick);
    s.has(p) ? s.delete(p) : s.add(p);
    pick = s;
  }

  async function saveKeep() {
    try {
      const k = { last: Number(keepEdit.last) || 1, daily_days: Number(keepEdit.daily_days) || 0, weekly_weeks: Number(keepEdit.weekly_weeks) || 0 };
      await api(`${base}/backups/keep`, { method: 'PUT', body: k });
      keepEdit = null;
      say('Saved. Applies when the next scheduled backup is made.');
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function saveCopies() {
    try {
      await api(`${base}/backups/copies`, { method: 'PUT', body: { targets: [...copiesEdit] } });
      copiesEdit = null;
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function openRemote(t, from = '') {
    remote = { target: t, loading: true, servers: [], snapshots: [], from };
    try {
      const r = await api(`${base}/backups/remote?target=${encodeURIComponent(t.id)}${from ? `&from=${encodeURIComponent(from)}` : ''}`);
      remote = { target: t, loading: false, ...r };
    } catch (e) {
      remote = null;
      say(e.message, true);
    }
  }

  async function bring(s) {
    try {
      const { job } = await post(`${base}/backups/remote/bring`, { target: remote.target.id, from: remote.from, name: s.name });
      const t = remote.target;
      const from = remote.from;
      run('Bringing it back', job, () => {
        say('Brought back. It is in the list; restore it from there.');
        openRemote(t, from);
      });
    } catch (e) {
      say(e.message, true);
    }
  }

  const when = (t) => new Date(t * 1000).toLocaleString();
  const ago = (t) => {
    const s = Date.now() / 1000 - t;
    return s < 90 ? 'just now' : s < 5400 ? `${Math.round(s / 60)} min ago` : s < 129600 ? `${Math.round(s / 3600)} h ago` : `${Math.round(s / 86400)} days ago`;
  };

  $effect(() => {
    sid;
    load();
  });
  // Copies run in the background: look again now and then while some wait.
  $effect(() => {
    if (!onTargets.some((t) => t.pending || t.working)) return;
    const t = setInterval(load, 5000);
    return () => clearInterval(t);
  });
</script>

<div class="split">
  <div class="main">
    <div class="card">
      <h3><Archive size={14} />Backups
        <span class="sp"></span>
        <button class="btn sm" disabled={!!busy} onclick={() => make(true)}><Plus size={13} />Worlds</button>
        <button class="btn sm pri" disabled={!!busy} onclick={() => make(false)}><Plus size={13} />Everything</button>
      </h3>
      {#if data}
        <div class="small muted">
          {data.backups.length} backup{data.backups.length === 1 ? '' : 's'} · {bytes(data.total_size)} on disk{saved > 0 ? ` (${bytes(data.as_zips)} as zips)` : ''}{data.disk_free ? ` · ${bytes(data.disk_free)} free` : ''}
        </div>
      {/if}
      {#if busy}<div class="busy small"><span class="spin"></span>{busy}</div>{/if}
      <div class="list">
        {#each data?.backups || [] as b (b.name)}
          {@const c = copies(b)}
          <div class="b">
            {#if b.scope === 'full'}<Database size={15} color="var(--blue)" />{:else}<Archive size={15} color="var(--muted)" />{/if}
            <div class="nmw">
              <div class="ell">{when(b.at)} <span class="muted small">· {b.scope === 'full' ? 'everything' : 'worlds'} · {b.kind === 'safety' ? 'before a change' : b.kind}</span>{#if isZip(b)} <span class="chip" title="Made by an older Hopper Server: a full zip">zip</span>{/if}</div>
              <div class="tiny muted ell">{#if b.note}{b.note} · {/if}{isZip(b) ? bytes(b.size) : b.added ? `+ ${bytes(b.added)} new · ${bytes(b.size)} in all` : `nothing new · ${bytes(b.size)} in all`}</div>
            </div>
            {#if c}
              <span class="chip" class:ok={c.done === c.of} class:wait={c.done < c.of && !c.failing} class:bad={c.failing} title={c.names}>
                {#if c.done === c.of}<Check size={11} />{c.of === 1 ? 'Copied' : `${c.of} copies`}{:else if c.failing}<AlertTriangle size={11} />{c.done} of {c.of}{:else}<RefreshCw size={11} />{c.done} of {c.of}{/if}
              </span>
            {/if}
            {#if b.kind !== 'scheduled'}<span class="chip" title="Manual and before-a-change backups are never deleted by the keep rules">kept</span>{/if}
            <button class="btn sm icon ghost" title="Download as a zip" onclick={() => download(`/api/v1${base}/backups/${encodeURIComponent(b.name)}/download`)}><Download size={13} /></button>
            <button class="btn sm" onclick={() => browse(b)}><FolderOpen size={13} />Browse</button>
            <button class="btn sm" disabled={alive || !!busy} title={alive ? 'Stop the server first' : ''} onclick={() => restore(b)}><RotateCcw size={13} />Restore</button>
            <button class="btn sm icon ghost" title="Delete" disabled={!!busy} onclick={() => remove(b)}><Trash2 size={13} /></button>
          </div>
        {:else}
          <p class="small muted pad">{data ? 'No backups yet.' : 'Loading…'}</p>
        {/each}
      </div>
    </div>

    {#if browsing}
      <div class="card">
        <h3><FolderOpen size={14} />{when(browsing.at)}<span class="sp"></span>
          <button class="btn sm icon ghost" onclick={() => (browsing = null)}><X size={14} /></button></h3>
        <div class="row">
          <div class="input grow"><input placeholder="Filter files" bind:value={filter} /></div>
          <button class="btn sm pri" disabled={!pick.size || alive} onclick={() => restore(browsing, [...pick])}><RotateCcw size={13} />Restore {pick.size || ''}</button>
        </div>
        <div class="entries">
          {#each shown as e (e[0])}
            <label class="ent"><input type="checkbox" checked={pick.has(e[0])} onchange={() => toggle(e[0])} /><span class="mono ell">{e[0]}</span><span class="sp"></span><span class="tiny muted">{bytes(e[1])}</span></label>
          {/each}
          {#if entries.length > shown.length}<p class="tiny muted pad">{entries.length - shown.length} more; filter to find them.</p>{/if}
        </div>
      </div>
    {/if}

    {#if remote}
      <div class="card">
        <h3><Undo2 size={14} />Bring back from {remote.target.name}<span class="sp"></span>
          <button class="btn sm icon ghost" onclick={() => (remote = null)}><X size={14} /></button></h3>
        {#if remote.loading}
          <p class="small muted">Reading the copy…</p>
        {:else}
          {#if remote.servers.length > 1 || (remote.servers.length && !remote.servers.includes(server.id))}
            <div class="row small wrapr">Copies of
              {#each remote.servers as s (s)}<button class="chip pickc" class:on={s === remote.from} onclick={() => openRemote(remote.target, s)}>{s}</button>{/each}
            </div>
          {/if}
          {#each remote.snapshots as s (s.name)}
            <div class="b">
              <Archive size={15} color="var(--muted)" />
              <div class="nmw"><div class="ell">{when(s.at)} <span class="muted small">· {s.scope === 'full' ? 'everything' : 'worlds'} · {s.kind}</span></div>
                <div class="tiny muted ell">{s.note ? `${s.note} · ` : ''}{bytes(s.size)} · {s.files} files</div></div>
              {#if s.here}<span class="chip">Already here</span>{:else}<button class="btn sm" disabled={!!busy} onclick={() => bring(s)}><Download size={13} />Bring back</button>{/if}
            </div>
          {:else}
            <p class="small muted">No copies of {remote.from || server.id} there.</p>
          {/each}
        {/if}
      </div>
    {/if}
  </div>

  <aside class="rail">
    <div class="card">
      <h3><HardDrive size={14} />Space</h3>
      {#if data}
        {#if data.as_zips > 0}
          <div class="bar"><i style="width:{Math.max(3, Math.min(100, (data.total_size / Math.max(data.as_zips, 1)) * 100))}%"></i></div>
        {/if}
        <div class="kv">
          <span>On disk</span><span>{bytes(data.total_size)}</span>
          <span>As zips</span><span>{bytes(data.as_zips)}</span>
          {#if data.zips_size}<span>Old zips</span><span>{bytes(data.zips_size)} · <button class="lnk" disabled={!!busy} onclick={convertAll}>convert</button></span>{/if}
        </div>
        <p class="tiny muted">Each backup stores only what changed since the others.</p>
      {/if}
    </div>

    <div class="card">
      <h3><Clock size={14} />Keep<span class="sp"></span>
        {#if data && !keepEdit}<button class="btn sm ghost" onclick={() => (keepEdit = { ...data.keep })}>Edit</button>{/if}</h3>
      {#if keepEdit}
        <div class="f"><span>Newest</span><div class="input w"><input type="number" min="1" max="500" bind:value={keepEdit.last} /></div></div>
        <div class="f"><span>One a day for <small>days</small></span><div class="input w"><input type="number" min="0" max="366" bind:value={keepEdit.daily_days} /></div></div>
        <div class="f"><span>One a week for <small>weeks</small></span><div class="input w"><input type="number" min="0" max="520" bind:value={keepEdit.weekly_weeks} /></div></div>
        <div class="row end"><button class="btn sm" onclick={() => (keepEdit = null)}>Cancel</button><button class="btn sm pri" onclick={saveKeep}>Save</button></div>
      {:else if data}
        <div class="small subtle">Newest {data.keep.last}{data.keep.daily_days ? ` · one a day for ${data.keep.daily_days} days` : ''}{data.keep.weekly_weeks ? ` · one a week for ${data.keep.weekly_weeks} weeks` : ''}.</div>
        <div class="tiny muted gap">Applies to scheduled backups. Manual and before-a-change ones stay until you delete them.</div>
      {/if}
      {#if data}
        <div class="tiny muted gap">Schedule: {data.every_hours ? `every ${data.every_hours} h while it runs` : 'off'} · <button class="lnk" onclick={openSchedule}>{data.every_hours ? 'change' : 'set up'}</button></div>
      {/if}
    </div>

    <div class="card">
      <h3><CloudUpload size={14} />Copies off-site<span class="sp"></span>
        {#if data?.can_edit_targets && data.targets.length && !copiesEdit}<button class="btn sm ghost" onclick={() => (copiesEdit = new Set(data.targets.filter((t) => t.on).map((t) => t.id)))}>Edit</button>{/if}</h3>
      {#if copiesEdit}
        {#each data.targets as t (t.id)}
          <label class="tck"><input type="checkbox" checked={copiesEdit.has(t.id)} onchange={() => { const s = new Set(copiesEdit); s.has(t.id) ? s.delete(t.id) : s.add(t.id); copiesEdit = s; }} />{t.name}{#if !t.enabled}<span class="tiny muted"> (off)</span>{/if}</label>
        {/each}
        <p class="tiny muted">New backups are copied within a few minutes; old ones too, newest first. Old zips aren't copied (convert them first).</p>
        <div class="row end"><button class="btn sm" onclick={() => (copiesEdit = null)}>Cancel</button><button class="btn sm pri" onclick={saveCopies}>Save</button></div>
      {:else if data}
        {#each data.targets.filter((t) => t.on) as t (t.id)}
          <div class="tg">
            {#if t.last_error}<AlertTriangle size={13} color="var(--red)" />{:else if t.working || t.pending}<RefreshCw size={13} color="var(--gold)" />{:else}<Check size={13} color="var(--green)" />{/if}
            <div class="nmw"><div class="ell small">{t.name}{#if !t.enabled}<span class="muted"> (off)</span>{/if}</div>
              <div class="tiny ell" class:muted={!t.last_error} class:errc={t.last_error} title={t.last_error || ''}>
                {t.last_error ? t.last_error : t.working ? 'Copying now' : t.pending ? `${t.pending} waiting` : t.last_ok ? `Up to date · ${ago(t.last_ok)}` : 'Waiting'}</div></div>
            {#if data.can_edit_targets}<button class="btn sm icon ghost" title="Bring a backup back from here" onclick={() => openRemote(t)}><Undo2 size={13} /></button>{/if}
          </div>
        {:else}
          <p class="small muted">{data.targets.length ? 'Not copied anywhere yet.' : 'No places to copy to yet.'}
            {#if data.can_edit_targets}{#if !data.targets.length}<a href="#/access">Add one</a> in Access &amp; devices.{/if}{/if}</p>
        {/each}
        {#if data.can_edit_targets && data.targets.some((t) => !t.on)}
          <div class="tiny muted gap">Bring back from: {#each data.targets.filter((t) => !t.on) as t, i (t.id)}{i ? ', ' : ''}<button class="lnk" onclick={() => openRemote(t)}>{t.name}</button>{/each}</div>
        {/if}
      {/if}
    </div>
  </aside>
</div>

<style>
  .split { display: flex; gap: 14px; align-items: flex-start; }
  .main { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 12px; }
  .rail { width: 290px; flex: none; display: flex; flex-direction: column; gap: 12px; }
  @media (max-width: 1000px) { .split { flex-direction: column; } .rail { width: 100%; } }
  .card h3 { margin-bottom: 6px; }
  .list { margin-top: 8px; }
  .b { display: flex; align-items: center; gap: 10px; padding: 9px 2px; border-bottom: 1px solid var(--line); }
  .b:last-child { border: 0; }
  .nmw { flex: 1; min-width: 0; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .pad { padding: 10px 0; }
  .grow { flex: 1; }
  .gap { margin-top: 6px; }
  .end { justify-content: flex-end; margin-top: 6px; }
  .lnk { color: var(--acc); font-size: inherit; padding: 0; }
  .busy { display: flex; align-items: center; gap: 8px; margin-top: 8px; color: var(--gold); }
  .spin { width: 12px; height: 12px; border: 2px solid var(--gold); border-right-color: transparent; border-radius: 50%; animation: s 0.8s linear infinite; }
  @keyframes s { to { transform: rotate(360deg); } }
  .entries { max-height: 420px; overflow: auto; margin-top: 8px; font-size: 12px; }
  .ent { display: flex; align-items: center; gap: 8px; padding: 3px 0; border-bottom: 1px solid #26262c; cursor: pointer; }
  .ent input, .tck input { accent-color: var(--acc); }
  .chip.ok { color: var(--green); background: var(--green-dim); }
  .chip.wait { color: var(--gold); background: var(--gold-dim); }
  .chip.bad { color: var(--red); background: var(--red-dim); }
  .bar { height: 8px; border-radius: 4px; background: var(--blue-dim); overflow: hidden; margin-bottom: 8px; }
  .bar i { display: block; height: 100%; background: var(--blue); }
  .kv { display: grid; grid-template-columns: auto 1fr; gap: 4px 12px; font-size: 12.5px; }
  .kv span:nth-child(odd) { color: var(--muted); }
  .f { display: flex; align-items: center; gap: 8px; padding: 4px 0; font-size: 12.5px; }
  .f span { flex: 1; }
  .f small { color: var(--muted); }
  .input.w { width: 72px; }
  .tck { display: flex; align-items: center; gap: 8px; padding: 4px 0; font-size: 12.5px; cursor: pointer; }
  .tg { display: flex; align-items: center; gap: 8px; padding: 5px 0; border-bottom: 1px solid var(--line); }
  .tg:last-of-type { border: 0; }
  .errc { color: var(--red); }
  .wrapr { flex-wrap: wrap; margin-bottom: 6px; }
  .pickc { cursor: pointer; }
  .pickc.on { color: #fb923c; background: var(--acc-dim); }
</style>
