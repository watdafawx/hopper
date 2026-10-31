<script>
  // A server's backups: the list (each new one only stores what changed),
  // and a rail with the space they take, which ones are kept, and copies
  // somewhere else.
  import { Archive, RotateCcw, Trash2, Download, Plus, X, HardDrive, Clock, CloudUpload, Check, RefreshCw, AlertTriangle, Undo2, ChevronRight, ChevronDown } from '@lucide/svelte';
  import { api, post, del } from './lib/api.js';
  import { bytes } from './lib/format.js';
  import { say } from './lib/toast.svelte.js';
  import { ask, confirmIt } from './lib/dialog.svelte.js';
  import { followJob } from './lib/jobs.js';
  import { download } from './lib/transfers.svelte.js';
  import { startPick, clickSwallowed, tickLabel } from './lib/dragpick.svelte.js';
  import { whenLabel, fullDate, plural } from './lib/worlds.js';
  import { rightClear } from './lib/rightclear.js';
  import BackupRestore from './BackupRestore.svelte';

  let { server, openSchedule } = $props();
  // The live feed hands over a new `server` object every second; effects
  // that should run once per server key on the id alone.
  const sid = $derived(server.id);
  let data = $state(null);
  let busy = $state('');
  let browsing = $state(null);
  let entries = $state(null);
  let pick = $state(new Set());
  let filter = $state('');
  let keepEdit = $state(null);
  let copiesEdit = $state(null);
  let remote = $state(null);
  /** The restore being asked about: { b, paths, changes }. */
  let asking = $state(null);
  /** What changed since each opened backup (desktop's "Since then"), by
   * name: { data } or { error }; missing while it's compared. */
  let since = $state({});
  let sinceAll = $state(false);
  let anchor = null;

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));
  const shown = $derived((entries || []).filter((e) => !filter || e[0].toLowerCase().includes(filter.toLowerCase())).slice(0, 400));
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

  // What a backup is called, as desktop's snapshot labels: its note, else
  // what made it.
  const KIND = { manual: 'Backup', scheduled: 'Scheduled backup', safety: 'Before a change' };
  const title = (b) => b.note || KIND[b.kind] || 'Backup';

  /** What a whole restore of `b` would change, against the server now
   * (looked at again each time: a running server keeps changing). */
  async function compare(b) {
    try {
      const data = await api(`${base}/backups/${encodeURIComponent(b.name)}/changes`);
      since = { ...since, [b.name]: { data } };
      return data;
    } catch (e) {
      since = { ...since, [b.name]: { error: e.message } };
      return null;
    }
  }
  /** Changes listed under an open backup before "Show all" (desktop's SHOWN). */
  const SHOWN = 8;
  /** One line per change: changed, then removed (put back), then added. */
  const changeLines = (c) => [
    ...c.changed.map((path) => ({ path, kind: 'changed', mark: '~', what: 'changed' })),
    ...c.removed.map((path) => ({ path, kind: 'removed', mark: '−', what: 'removed' })),
    ...c.added.map((path) => ({ path, kind: 'added', mark: '+', what: 'added' })),
  ];
  const nothing = (c) => c && !c.changed.length && !c.removed.length && !c.added.length;

  async function restore(b, paths = []) {
    if (alive) {
      say(`Stop ${server.name} first.`, true);
      return;
    }
    if (busy) return;
    // As desktop: compare with now first, and say so when nothing differs.
    let changes = null;
    if (!paths.length) {
      busy = 'Comparing with now…';
      changes = await compare(b);
      busy = '';
      if (nothing(changes)) {
        say('Nothing to restore: the server is the same as this backup.');
        return;
      }
    }
    asking = { b, paths, changes };
  }

  async function doRestore(backupFirst = false) {
    const { b, paths } = asking;
    asking = null;
    busy = 'Restoring';
    try {
      const { job } = await post(`${base}/backups/${encodeURIComponent(b.name)}/restore`, { paths, backup_first: backupFirst });
      await run('Restoring', job, (r) => {
        say(`Restored "${title(b)}" (${plural(r.files, 'file')}).${r.safety_note ? ` To undo, restore "${r.safety_note}".` : ''}`);
        pick = new Set();
      });
    } catch (e) {
      busy = '';
      say(e.message, true);
    } finally {
      // The server changed (or may have): compare again when asked.
      since = {};
      if (browsing) compare(browsing);
    }
  }

  async function remove(b) {
    if (!(await confirmIt(`Delete "${title(b)}"?`, { body: `${whenLabel(b.at)}. Files only this backup keeps are deleted for good. The server itself isn't touched. Off-site copies of it go once a newer backup is copied.`, ok: 'Delete', danger: true }))) return;
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

  // A backup shown open lists its files, to restore some of them.
  async function browse(b) {
    if (browsing?.name === b.name) {
      browsing = null;
      return;
    }
    browsing = b;
    pick = new Set();
    filter = '';
    entries = null;
    anchor = null;
    sinceAll = false;
    compare(b);
    try {
      entries = (await api(`${base}/backups/${encodeURIComponent(b.name)}/entries`)).entries;
    } catch (e) {
      entries = [];
      say(e.message, true);
    }
  }

  function toggle(p, e) {
    if (clickSwallowed()) return e.preventDefault();
    const s = new Set(pick);
    const on = !s.has(p);
    // Shift-click ticks the rows between, as in the Mods tab.
    if (e.shiftKey && anchor) {
      const a = shown.findIndex((x) => x[0] === anchor);
      const z = shown.findIndex((x) => x[0] === p);
      if (a >= 0 && z >= 0) for (const x of shown.slice(Math.min(a, z), Math.max(a, z) + 1)) on ? s.add(x[0]) : s.delete(x[0]);
    } else on ? s.add(p) : s.delete(p);
    anchor = p;
    pick = s;
  }

  // Drag across the tick boxes to tick (or untick) several, as desktop.
  function tickDown(e, p, ix) {
    const on = !pick.has(p);
    startPick(e, { list: 'backup-files', kind: 'tick', ix, on, selected: pick, keyAt: (i) => shown[i]?.[0] ?? null, onTick: (s) => (pick = s), label: tickLabel(on) });
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
        <button class="btn sm" disabled={!!busy} onclick={() => make(true)}><Plus size={13} />Back up worlds</button>
        <button class="btn sm pri" disabled={!!busy} onclick={() => make(false)}><Plus size={13} />Back up everything</button>
      </h3>
      <!-- What's running, else what this is (desktop shows it in the intro's place). -->
      {#if busy}<div class="busy small"><span class="spinner"></span>{busy}</div>
      {:else}<div class="small muted">The worlds, or everything, saved on the schedule and before resets and restores. Each one stores only what changed. Scheduled ones follow the keep rules; yours stay until you delete them.</div>{/if}
      <div class="list">
        {#if !data}
          {#each [0, 1, 2] as i (i)}<div class="skel"><i></i></div>{/each}
        {:else}
          {#each data.backups as b (b.name)}
            {@const c = copies(b)}
            {@const open = browsing?.name === b.name}
            {@const auto = b.kind !== 'manual'}
            <div class="bk" class:open>
              <div class="b row-hover" role="button" tabindex="0" aria-expanded={open} onclick={(e) => !e.target.closest('button, a') && browse(b)} onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && e.target === e.currentTarget && (e.preventDefault(), browse(b))}>
                {#if open}<ChevronDown size={15} class="chev" />{:else}<ChevronRight size={15} class="chev" />{/if}
                <div class="nmw">
                  <div class="ell nm">{title(b)}{#if isZip(b)} <span class="chip" title="Made by an older Hopper Server: a full zip">zip</span>{/if}</div>
                  <div class="tiny muted ell"><span title={fullDate(b.at)}>{whenLabel(b.at)}</span> · {b.scope === 'full' ? 'Everything' : 'Worlds'}{b.files ? ` · ${plural(b.files, 'file')}` : ''} · {bytes(b.size)}{isZip(b) ? '' : b.added ? ` · ${bytes(b.added)} new` : ' · nothing new'}</div>
                </div>
                {#if c}
                  <span class="chip" class:ok={c.done === c.of} class:wait={c.done < c.of && !c.failing} class:bad={c.failing} title={c.names}>
                    {#if c.done === c.of}<Check size={11} />{c.of === 1 ? 'Copied' : `${c.of} copies`}{:else if c.failing}<AlertTriangle size={11} />{c.done} of {c.of}{:else}<RefreshCw size={11} />{c.done} of {c.of}{/if}
                  </span>
                {/if}
                <span class="chip" class:acc={!auto} title={b.kind === 'scheduled' ? 'Made on the schedule; the keep rules thin these out' : b.kind === 'safety' ? 'Made before a reset, restore or update; stays until you delete it' : 'Yours; stays until you delete it'}>{auto ? 'auto' : 'manual'}</span>
                <button class="btn sm icon ghost" title="Download as a zip" onclick={() => download(`/api/v1${base}/backups/${encodeURIComponent(b.name)}/download`)}><Download size={13} /></button>
                <button class="btn sm" disabled={alive || !!busy} title={alive ? 'Stop the server first' : 'Put the server back to how it was then'} onclick={() => restore(b)}><Undo2 size={13} />Restore</button>
                <button class="btn sm icon ghost" title="Delete this backup" disabled={!!busy} onclick={() => remove(b)}><Trash2 size={13} /></button>
              </div>
              {#if open}
                {@const sc = since[b.name]}
                <div class="files">
                  {#if !sc}
                    <div class="tiny muted row"><span class="spinner"></span>Comparing with now…</div>
                  {:else if sc.error}
                    <div class="tiny err">Couldn't compare with now: {sc.error}</div>
                  {:else if nothing(sc.data)}
                    <div class="tiny muted">Same as now.</div>
                  {:else}
                    {@const lines = changeLines(sc.data)}
                    <div class="since">
                      <div class="row wrapr tiny"><span class="muted">Since then:</span>
                        {#if sc.data.changed.length}<span class="chip wait">~{sc.data.changed.length} changed</span>{/if}
                        {#if sc.data.added.length}<span class="chip ok">+{sc.data.added.length} added</span>{/if}
                        {#if sc.data.removed.length}<span class="chip bad">−{sc.data.removed.length} removed</span>{/if}
                      </div>
                      {#each sinceAll ? lines : lines.slice(0, SHOWN) as l (l.kind + l.path)}
                        <div class="chg tiny"><span class="mk {l.kind}">{l.mark}</span><span class="mono ell">{l.path}</span><span class="muted">{l.what}</span></div>
                      {/each}
                      {#if lines.length > SHOWN}<button class="lnk tiny" onclick={() => (sinceAll = !sinceAll)}>{sinceAll ? 'Show fewer' : `Show all changes (${lines.length - SHOWN} more)`}</button>{/if}
                    </div>
                  {/if}
                  {#if !entries}
                    <div class="tiny muted">Reading its files…</div>
                  {:else}
                    <div class="row">
                      <div class="input sm grow"><input use:rightClear placeholder="Filter {plural(entries.length, 'file')}" bind:value={filter} /></div>
                      {#if pick.size}<button class="btn sm ghost" onclick={() => (pick = new Set())}>Clear</button>{/if}
                      <button class="btn sm" disabled={!pick.size || alive || !!busy} title={alive ? 'Stop the server first' : pick.size ? '' : 'Tick files to restore only those'} onclick={() => restore(b, [...pick])}><Undo2 size={13} />Restore {pick.size || ''}</button>
                    </div>
                    <div class="entries" data-pick-list="backup-files">
                      {#each shown as e, ix (e[0])}
                        <label class="ent row-hover" data-pick-ix={ix}><input type="checkbox" checked={pick.has(e[0])} onpointerdown={(ev) => tickDown(ev, e[0], ix)} onclick={(ev) => toggle(e[0], ev)} /><span class="mono ell">{e[0]}</span><span class="sp"></span><span class="tiny muted">{bytes(e[1])}</span></label>
                      {/each}
                      {#if entries.length > shown.length}<p class="tiny muted pad">{entries.length - shown.length} more; filter to find them.</p>{/if}
                    </div>
                  {/if}
                </div>
              {/if}
            </div>
          {:else}
            <div class="empty-state">
              <span class="es-ic"><Archive size={20} /></span>
              <b>No backups yet</b>
              <p>Hopper Server makes one on the schedule and before resets and restores. Back up now to keep this setup.</p>
            </div>
          {/each}
        {/if}
      </div>
      {#if data?.backups.length}
        <div class="tiny muted foot">{bytes(data.total_size)} on disk for backups{saved > 0 ? ` (${bytes(data.as_zips)} as zips)` : ''}{data.disk_free ? ` · ${bytes(data.disk_free)} free` : ''}</div>
      {/if}
    </div>

    {#if remote}
      <div class="card">
        <h3><Undo2 size={14} />Bring back from {remote.target.name}<span class="sp"></span>
          <button class="btn sm icon ghost" onclick={() => (remote = null)}><X size={14} /></button></h3>
        {#if remote.loading}
          <p class="small muted">Reading the copy…</p>
        {:else}
          {#if remote.servers.length > 1 || (remote.servers.length && !remote.servers.includes(server.id))}
            <div class="row small wrapr">Copies of
              {#each remote.servers as s (s)}<button class="chip pickc" class:on={s === remote.from} aria-pressed={s === remote.from} onclick={() => openRemote(remote.target, s)}>{s}</button>{/each}
            </div>
          {/if}
          {#each remote.snapshots as s (s.name)}
            <div class="b">
              <div class="nmw"><div class="ell nm">{title(s)}</div>
                <div class="tiny muted ell"><span title={fullDate(s.at)}>{whenLabel(s.at)}</span> · {s.scope === 'full' ? 'Everything' : 'Worlds'} · {plural(s.files, 'file')} · {bytes(s.size)}</div></div>
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
          <div class="bar b space"><i style="width:{Math.max(3, Math.min(100, (data.total_size / Math.max(data.as_zips, 1)) * 100))}%"></i></div>
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

{#if asking}<BackupRestore b={asking.b} title={title(asking.b)} paths={asking.paths} changes={asking.changes} onOk={doRestore} onCancel={() => (asking = null)} />{/if}

<style>
  .split { display: flex; gap: 14px; align-items: flex-start; }
  .main { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 12px; }
  .rail { width: 290px; flex: none; display: flex; flex-direction: column; gap: 12px; }
  @media (max-width: 1000px) { .split { flex-direction: column; } .rail { width: 100%; } }
  .card h3 { margin-bottom: 6px; }
  .list { margin-top: 8px; }
  /* One card per backup, as desktop's snapshot rows: click to open its files. */
  .bk { border: 1px solid var(--line); border-radius: 10px; margin-bottom: 8px; }
  .bk .b { padding: 10px 12px; border: 0; cursor: pointer; border-radius: 10px; }
  .b { display: flex; align-items: center; gap: 10px; padding: 9px 2px; border-bottom: 1px solid var(--line); }
  .b:last-child { border-bottom: 0; }
  .b :global(.chev) { flex: none; color: var(--subtle); }
  .nm { font-weight: 600; }
  .files { padding: 0 12px 10px 37px; display: flex; flex-direction: column; gap: 6px; }
  .input.sm { padding: 3px 8px; }
  .foot { margin-top: 6px; }
  /* The shared loading row, at a backup card's height. */
  .skel { height: 56px; padding: 0 12px; margin-bottom: 8px; border: 1px solid var(--line); border-radius: var(--radius-lg); }
  .nmw { flex: 1; min-width: 0; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .pad { padding: 10px 0; }
  .grow { flex: 1; }
  .gap { margin-top: 6px; }
  .end { justify-content: flex-end; margin-top: 6px; }
  .lnk { color: var(--acc); font-size: inherit; padding: 0; }
  .busy { display: flex; align-items: center; gap: 8px; margin-top: 8px; color: var(--gold); }
  .entries { max-height: 420px; overflow: auto; font-size: 12px; }
  .ent { display: flex; align-items: center; gap: 8px; padding: 3px 6px; border-radius: 5px; cursor: pointer; }
  .ent input, .tck input { accent-color: var(--acc); }
  .chip.ok { color: var(--green); background: var(--green-dim); }
  .chip.wait { color: var(--gold); background: var(--gold-dim); }
  .chip.bad { color: var(--red); background: var(--red-dim); }
  .space { margin-bottom: 8px; }
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
  .wrapr { flex-wrap: wrap; margin-bottom: 6px; gap: 5px; }
  .since { display: flex; flex-direction: column; gap: 2px; margin-bottom: 4px; }
  .since .wrapr { margin-bottom: 2px; }
  .chg { display: flex; align-items: center; gap: 8px; min-width: 0; padding: 0 6px; }
  .mk { width: 10px; flex: none; font-weight: 700; text-align: center; }
  .mk.changed { color: var(--gold); } .mk.added { color: var(--green); } .mk.removed { color: var(--red); }
  .pickc { cursor: pointer; }
</style>
