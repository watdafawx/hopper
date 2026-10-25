<script>
  // "Review N updates": each update with its changelog, ticked by default;
  // Update selected installs the ticked ones (old jars go to Backups).
  import { X, ChevronDown, ChevronRight, ArrowUpCircle } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { md } from './lib/md.js';

  let { server, updates, busy = '', onApply, onClose } = $props();
  let picked = $state(new Set(updates.map((u) => u.project_id)));
  let open = $state(null);
  let notes = $state({});

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);

  async function toggleNotes(u) {
    open = open === u.project_id ? null : u.project_id;
    if (open && notes[u.project_id] == null) {
      notes[u.project_id] = '…';
      try {
        notes[u.project_id] = (await api(`${base}/mods/changelog?project=${u.project_id}&file=${u.file_id}`)).markdown || '';
      } catch {
        notes[u.project_id] = '';
      }
    }
  }

  function flip(id) {
    const s = new Set(picked);
    s.has(id) ? s.delete(id) : s.add(id);
    picked = s;
  }
</script>

<div class="back" role="presentation" onclick={(e) => e.target === e.currentTarget && !busy && onClose()}>
  <div class="box card col">
    <h3>Review {updates.length} update{updates.length === 1 ? '' : 's'}<span class="sp"></span><button class="btn sm ghost icon" disabled={!!busy} onclick={onClose}><X size={14} /></button></h3>
    <div class="small subtle">The jars they replace go to Mods › Backups, so each one can be put back.{server.state === 'running' ? ' Restart the server afterwards to load them.' : ''}</div>
    <div class="row small"><button class="btn sm ghost" onclick={() => (picked = new Set(updates.map((u) => u.project_id)))}>All</button><button class="btn sm ghost" onclick={() => (picked = new Set())}>None</button></div>
    <div class="list">
      {#each updates as u (u.project_id)}
        <div class="u">
          <div class="row">
            <input type="checkbox" checked={picked.has(u.project_id)} onchange={() => flip(u.project_id)} />
            <button class="nm grow" onclick={() => toggleNotes(u)}>{#if open === u.project_id}<ChevronDown size={13} />{:else}<ChevronRight size={13} />{/if}<b>{u.name}</b><span class="muted">{u.from} → <b class="to">{u.to}</b></span></button>
          </div>
          {#if open === u.project_id}<div class="notes small">{#if notes[u.project_id] === '…'}<span class="muted">Loading…</span>{:else if notes[u.project_id]}{@html md(notes[u.project_id])}{:else}<span class="muted">No changelog.</span>{/if}</div>{/if}
        </div>
      {/each}
    </div>
    {#if busy}<div class="small gold">{busy}</div>{/if}
    <div class="row end">
      <button class="btn" disabled={!!busy} onclick={onClose}>Cancel</button>
      <button class="btn pri" disabled={!picked.size || !!busy} onclick={() => onApply([...picked])}><ArrowUpCircle size={14} />Update {picked.size}</button>
    </div>
  </div>
</div>

<style>
  .back { position: fixed; inset: 0; background: #0008; display: grid; place-items: center; z-index: 60; padding: 16px; }
  .box { width: min(640px, 100%); max-height: calc(100vh - 32px); gap: 10px; box-shadow: 0 24px 60px #000c; }
  .box h3 { margin: 0; }
  .list { overflow: auto; display: flex; flex-direction: column; border: 1px solid var(--line); border-radius: 8px; }
  .u { padding: 6px 10px; border-bottom: 1px solid var(--line); }
  .u:last-child { border: 0; }
  .nm { display: flex; gap: 6px; align-items: center; text-align: left; min-width: 0; }
  .grow { flex: 1; }
  .to { color: var(--acc); }
  .notes { margin: 6px 0 2px 24px; background: var(--base); border: 1px solid var(--line); border-radius: 6px; padding: 6px 10px; max-height: 220px; overflow: auto; }
  .notes :global(ul) { margin: 2px 0 2px 18px; }
  .notes :global(h3), .notes :global(h4) { font-size: 12.5px; margin: 4px 0 2px; }
  .end { justify-content: flex-end; }
  .icon { padding: 3px 5px; }
  .gold { color: var(--gold); }
  input[type='checkbox'] { width: 15px; height: 15px; accent-color: var(--acc); }
</style>
