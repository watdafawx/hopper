<script>
  // Mods › Backups: what Hopper (desktop or this panel) set aside in
  // .hopper-backups: jars replaced by updates or removed, configs reset.
  // Restore puts files back; a jar coming back first moves the mod's current
  // jar into a new backup, so it can be undone the same way.
  import { Undo2, Trash2, ChevronDown, ChevronRight } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { confirmIt } from './lib/dialog.svelte.js';
  import { bytes } from './lib/format.js';

  let { server, onChanged = () => {} } = $props();
  const sid = $derived(server.id);
  let data = $state(null);
  let open = $state(new Set());
  let busy = $state(false);
  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const old = $derived((data?.sets || []).filter((s) => Date.now() / 1000 - s.when > 30 * 86400));

  async function load() {
    try {
      data = await api(`${base}/mod-backups`);
    } catch (e) {
      say(e.message, true);
    }
  }

  async function restore(set, file = null) {
    busy = true;
    try {
      const r = await post(`${base}/mod-backups/restore`, { when: set.when, name: file?.name ?? null });
      say(`Put back ${r.restored.join(', ')}.${r.restart_needed ? ' Restart the server to load it.' : ''}`);
      await load();
      onChanged();
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = false;
    }
  }

  async function remove(sets, label) {
    if (!(await confirmIt(label, { body: 'They go to the Recycle Bin on the server PC.', ok: 'Delete', danger: true }))) return;
    busy = true;
    try {
      for (const s of sets) await post(`${base}/mod-backups/delete`, { when: s.when });
      await load();
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = false;
    }
  }

  function flip(w) {
    const s = new Set(open);
    s.has(w) ? s.delete(w) : s.add(w);
    open = s;
  }

  const when = (t) => new Date(t * 1000).toLocaleString(undefined, { month: 'short', day: 'numeric', year: 'numeric', hour: '2-digit', minute: '2-digit' });
  const summary = (s) => (s.files.length === 1 ? s.files[0].name : `${s.files.length} files: ${s.files.slice(0, 3).map((f) => f.name.replace(/[-_]?(neoforge|forge|fabric|mc)?[-_]?\d.*$/i, '')).join(', ')}${s.files.length > 3 ? '…' : ''}`);

  $effect(() => {
    sid;
    load();
  });
</script>

<div class="col bk">
  <div class="row small">
    {#if data}<span class="muted">{data.sets.length} backup{data.sets.length === 1 ? '' : 's'} · {bytes(data.total_size)} in .hopper-backups</span>{/if}
    <span class="sp"></span>
    {#if old.length}<button class="btn sm" disabled={busy} onclick={() => remove(old, `Delete ${old.length} backup${old.length === 1 ? '' : 's'} older than 30 days?`)}><Trash2 size={13} />Delete older than 30 days</button>{/if}
  </div>
  {#if !data}
    <div class="small muted">Loading…</div>
  {:else if !data.sets.length}
    <div class="small muted">Nothing set aside yet. Jars that updates replace and mods you remove land here.</div>
  {:else}
    <div class="list">
      {#each data.sets as s (s.when)}
        <div class="set">
          <div class="row">
            <button class="nm grow" onclick={() => flip(s.when)}>{#if open.has(s.when)}<ChevronDown size={13} />{:else}<ChevronRight size={13} />{/if}<span class="w">{when(s.when)}</span><span class="ell">{summary(s)}</span></button>
            <span class="tiny muted">{bytes(s.size)}</span>
            <button class="btn sm" disabled={busy} onclick={() => restore(s)}><Undo2 size={13} />{s.files.length === 1 ? 'Restore' : 'Restore all'}</button>
            <button class="btn sm ghost icon" title="Delete this backup" disabled={busy} onclick={() => remove([s], 'Delete this backup?')}><Trash2 size={13} /></button>
          </div>
          {#if open.has(s.when)}
            {#each s.files as f (f.name)}
              <div class="row file small"><span class="mono ell grow" title={f.restore_to}>{f.name}</span><span class="tiny muted">→ {f.restore_to.split('/')[0]}/ · {bytes(f.size)}</span>
                {#if s.files.length > 1}<button class="btn sm ghost" disabled={busy} onclick={() => restore(s, f)}>Restore</button>{/if}</div>
            {/each}
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .bk { gap: 8px; }
  .list { border: 1px solid var(--line); border-radius: 8px; }
  .set { padding: 6px 10px; border-bottom: 1px solid var(--line); }
  .set:last-child { border: 0; }
  .nm { display: flex; align-items: center; gap: 8px; text-align: left; min-width: 0; }
  .w { white-space: nowrap; color: var(--subtle); font-variant-numeric: tabular-nums; width: 150px; flex: none; }
  .grow { flex: 1; min-width: 0; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .file { padding: 3px 0 3px 30px; }
  .icon { padding: 3px 5px; }
</style>
