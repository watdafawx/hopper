<script>
  import { Archive, Database, RotateCcw, Trash2, Download, Plus, FolderOpen, X } from '@lucide/svelte';
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

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));
  const shown = $derived(entries.filter((e) => !filter || e[0].toLowerCase().includes(filter.toLowerCase())).slice(0, 400));

  async function load() {
    try {
      data = await api(`${base}/backups`);
    } catch (e) {
      say(e.message, true);
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
      busy = 'Starting';
      const b = await followJob(job, (st) => (busy = st.detail || 'Working'));
      say(`Backed up: ${bytes(b.size)}, ${b.files} files`);
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = '';
      load();
    }
  }

  async function restore(b, paths = []) {
    if (alive) {
      say(`Stop ${server.name} first.`, true);
      return;
    }
    const all = !paths.length;
    const ok = await confirmIt(all ? `Restore ${b.name}?` : `Restore ${paths.length} item${paths.length === 1 ? '' : 's'}?`, {
      body: all
        ? 'Files in the backup replace the ones on the server. Worlds in it are replaced whole; the current ones are moved aside, not deleted.'
        : 'Those files replace the ones on the server.',
      ok: 'Restore',
      danger: true,
    });
    if (!ok) return;
    try {
      const { job } = await post(`${base}/backups/${encodeURIComponent(b.name)}/restore`, { paths });
      busy = 'Restoring';
      const r = await followJob(job, (st) => (busy = st.detail || 'Restoring'));
      say(`Restored ${r.files} files`);
      browsing = null;
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = '';
    }
  }

  async function remove(b) {
    if (!(await confirmIt(`Delete ${b.name}?`, { body: "It can't be brought back.", ok: 'Delete', danger: true }))) return;
    try {
      await del(`${base}/backups/${encodeURIComponent(b.name)}`);
      load();
    } catch (e) {
      say(e.message, true);
    }
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

  const when = (t) => new Date(t * 1000).toLocaleString();

  $effect(() => {
    sid;
    load();
  });
</script>

<div class="wrap">
  <div class="card">
    <h3><Archive size={14} />Backups
      <span class="sp"></span>
      <button class="btn sm" disabled={!!busy} onclick={() => make(true)}><Plus size={13} />Worlds</button>
      <button class="btn sm pri" disabled={!!busy} onclick={() => make(false)}><Plus size={13} />Everything</button>
    </h3>
    {#if data}
      <div class="small muted">
        {data.backups.length} backups · {bytes(data.total_size)}{data.disk_free ? ` · ${bytes(data.disk_free)} free on the drive` : ''} ·
        <button class="lnk" onclick={openSchedule}>scheduled backups</button>
      </div>
    {/if}
    {#if busy}<div class="busy small"><span class="spin"></span>{busy}</div>{/if}
    <div class="list">
      {#each data?.backups || [] as b (b.name)}
        <div class="b">
          {#if b.scope === 'full'}<Database size={15} color="var(--blue)" />{:else}<Archive size={15} color="var(--muted)" />{/if}
          <div class="nmw">
            <div>{when(b.at)} <span class="muted small">· {b.scope === 'full' ? 'everything' : 'worlds'} · {b.kind}</span></div>
            <div class="tiny muted ell">{b.note || b.name}</div>
          </div>
          <span class="small muted nw">{bytes(b.size)}</span>
          <button class="btn sm icon ghost" title="Download" onclick={() => download(`/api/v1/fs/${encodeURIComponent(server.id)}/file?path=${encodeURIComponent(`${data.folder}/${b.name}`)}`)}><Download size={13} /></button>
          <button class="btn sm" onclick={() => browse(b)}><FolderOpen size={13} />Browse</button>
          <button class="btn sm" disabled={alive || !!busy} title={alive ? 'Stop the server first' : ''} onclick={() => restore(b)}><RotateCcw size={13} />Restore</button>
          <button class="btn sm icon ghost" title="Delete" onclick={() => remove(b)}><Trash2 size={13} /></button>
        </div>
      {:else}
        <p class="small muted pad">{data ? 'No backups yet.' : 'Loading…'}</p>
      {/each}
    </div>
  </div>

  {#if browsing}
    <div class="card">
      <h3><FolderOpen size={14} />{browsing.name}<span class="sp"></span>
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
</div>

<style>
  .wrap { display: flex; flex-direction: column; gap: 12px; }
  .card h3 { margin-bottom: 6px; }
  .list { margin-top: 8px; }
  .b { display: flex; align-items: center; gap: 10px; padding: 9px 2px; border-bottom: 1px solid var(--line); }
  .b:last-child { border: 0; }
  .nmw { flex: 1; min-width: 0; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .nw { white-space: nowrap; }
  .pad { padding: 10px 0; }
  .grow { flex: 1; }
  .lnk { color: var(--acc); font-size: inherit; padding: 0; }
  .busy { display: flex; align-items: center; gap: 8px; margin-top: 8px; color: var(--gold); }
  .spin { width: 12px; height: 12px; border: 2px solid var(--gold); border-right-color: transparent; border-radius: 50%; animation: s 0.8s linear infinite; }
  @keyframes s { to { transform: rotate(360deg); } }
  .entries { max-height: 420px; overflow: auto; margin-top: 8px; font-size: 12px; }
  .ent { display: flex; align-items: center; gap: 8px; padding: 3px 0; border-bottom: 1px solid #26262c; cursor: pointer; }
  .ent input { accent-color: var(--acc); }
</style>
