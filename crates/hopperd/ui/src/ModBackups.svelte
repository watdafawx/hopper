<script>
  // Mods › Backups: what Hopper (desktop or this panel) set aside in
  // .hopper-backups: jars replaced by updates or removed, configs reset.
  // Restore puts files back; a jar coming back first moves the mod's current
  // jar into a new backup, so it can be undone the same way.
  import { untrack } from 'svelte';
  import { Undo2, Trash2, ChevronDown, ChevronRight, Archive } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { confirmIt } from './lib/dialog.svelte.js';
  import { bytes } from './lib/format.js';
  import { ago, dateOf } from './lib/modops.js';

  let { server, onChanged = () => {} } = $props();
  const sid = $derived(server.id);
  let data = $state(null);
  let open = $state(new Set());
  let busy = $state(false);
  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  // The clean up rules' numbers, as desktop's Backups footer shows them.
  const u = $derived(data?.usage);
  const cap = $derived(u ? u.max_mb * 1024 * 1024 : 0);
  const over = $derived(!!u && cap > 0 && u.limited > cap);

  async function load() {
    try {
      data = await api(`${base}/mod-backups`);
    } catch (e) {
      say(e.message, true);
    }
  }

  // Asked first, as desktop does: the current jar moves aside, so it can be undone.
  async function restore(set, file = null) {
    const what = file ? file.name : set.files.length > 1 ? `these ${set.files.length} files` : summary(set);
    if (!(await confirmIt(`Put back ${what}?`, { body: "A mod's current jar moves to Backups first, so this can be undone the same way.", ok: 'Restore' }))) return;
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

  // Runs the panel's clean up rules now (Settings › Panel folder).
  async function clean() {
    busy = true;
    try {
      const r = await post(`${base}/mod-backups/clean`, {});
      say(r.removed ? `Moved ${r.removed} old backup${r.removed === 1 ? '' : 's'} to the server's trash (${bytes(r.freed)})` : 'Nothing to clean up');
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

  // "27/09/2026 · 3 days ago", as desktop's list says it.
  const when = (t) => `${dateOf(new Date(t * 1000).toISOString())} · ${ago(new Date(t * 1000).toISOString())}`;
  const summary = (s) => (s.files.length === 1 ? s.files[0].name : `${s.files.length} files: ${s.files.slice(0, 3).map((f) => f.name.replace(/[-_]?(neoforge|forge|fabric|mc)?[-_]?\d.*$/i, '')).join(', ')}${s.files.length > 3 ? '…' : ''}`);

  $effect(() => {
    sid;
    untrack(load);
  });
</script>

<div class="col bk">
  <div class="row small">
    {#if u}
      <span class="muted">{data.sets.length} backup{data.sets.length === 1 ? '' : 's'} · Backups use {bytes(u.total)}{cap && !over ? ` of ${bytes(cap)}` : ''}</span>
      {#if over}<span class="chip warn">Over the {bytes(cap)} limit</span>{/if}
    {/if}
    <span class="sp"></span>
    {#if u}
      <span class="tiny muted">Keeps the last {u.keep_actions} actions and {u.keep_days} days</span>
      <button class="btn sm" disabled={busy || !u.removes} title="Moves old backups to the server's trash (Files › Trash), by the rules in Settings › Panel folder. The panel does this by itself after each new backup, but leaves older ones for you. World backups stay." onclick={clean}>{u.frees ? `Clean up · frees ${bytes(u.frees)}` : 'Clean up'}</button>
    {/if}
  </div>
  {#if !data}
    <div class="skels">{#each [0, 1, 2, 3] as i (i)}<div class="skel"><i></i></div>{/each}</div>
  {:else if !data.sets.length}
    <div class="empty-state"><span class="es-ic"><Archive size={20} /></span><b>Nothing set aside yet</b><p>Jars that updates replace and mods you remove land here.</p></div>
  {:else}
    <div class="list">
      {#each data.sets as s (s.when)}
        <div class="set">
          <div class="row">
            <button class="nm grow row-hover" onclick={() => flip(s.when)}>{#if open.has(s.when)}<ChevronDown size={13} />{:else}<ChevronRight size={13} />{/if}<span class="w">{when(s.when)}</span><span class="ell">{summary(s)}</span></button>
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
  .nm { display: flex; align-items: center; gap: 8px; text-align: left; min-width: 0; padding: 3px 6px; margin-left: -6px; border-radius: var(--radius); }
  .w { white-space: nowrap; color: var(--subtle); font-variant-numeric: tabular-nums; width: 190px; flex: none; }
  .grow { flex: 1; min-width: 0; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .file { padding: 3px 0 3px 30px; }
  .icon { padding: 3px 5px; }
  .skels { display: flex; flex-direction: column; gap: 4px; }
</style>
