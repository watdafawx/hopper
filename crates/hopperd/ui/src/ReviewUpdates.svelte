<script>
  // "Review N updates", as in Hopper desktop: updates grouped Patch, Major
  // and Alpha / beta, each ticked by default except test versions and ones
  // whose new file would stop the server loading; each row opens what
  // changed.
  import { X, ChevronDown, ChevronRight, Download, ArrowRight, TriangleAlert } from '@lucide/svelte';
  import { untrack } from 'svelte';
  import { api } from './lib/api.js';
  import { md } from './lib/md.js';
  import { paintDown, paintOver, paintClick } from './lib/dragpick.svelte.js';

  let { server, updates, mods = [], busy = '', onApply, onClose } = $props();
  const GROUPS = [
    ['patch', 'Patch', 'running', 'fixes within the same version'],
    ['major', 'Major', 'starting', 'new major or minor versions: worth a look at the changelog'],
    ['pre', 'Alpha / beta', 'crashed', 'test versions, not marked as releases'],
  ];
  // Ticked when the dialog opens; later changes are the user's.
  let picked = $state(untrack(() => new Set(updates.filter((u) => !u.pre_skip && !u.issues?.length).map((u) => u.project_id))));
  let open = $state(null);
  let notes = $state({});

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const icon = (u) => mods.find((m) => m.project_id === u.project_id)?.icon;
  const groups = $derived(GROUPS.map(([k, label, tint, about]) => ({ k, label, tint, about, rows: updates.filter((u) => (u.risk || 'major') === k).sort((a, b) => a.name.localeCompare(b.name)) })).filter((g) => g.rows.length));
  const bad = $derived(updates.filter((u) => u.issues?.length).length);

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

  // Pressed on a tick box and dragged down the list: each row takes the same.
  function set(id, on) {
    if (picked.has(id) === on) return;
    const s = new Set(picked);
    on ? s.add(id) : s.delete(id);
    picked = s;
  }

  // Rows in the order shown, across the groups, for painting.
  const order = $derived(groups.flatMap((g) => g.rows.map((u) => u.project_id)));
  const paintAt = (i, v) => order[i] != null && set(order[i], v);

  function flip(id) {
    const s = new Set(picked);
    s.has(id) ? s.delete(id) : s.add(id);
    picked = s;
  }

  function group(g, on) {
    const s = new Set(picked);
    for (const u of g.rows) on ? s.add(u.project_id) : s.delete(u.project_id);
    picked = s;
  }
</script>

<div class="back" role="presentation" onclick={(e) => e.target === e.currentTarget && !busy && onClose()}>
  <div class="box card col">
    <h3 class="row">Review {updates.length} update{updates.length === 1 ? '' : 's'}<span class="sp"></span><button class="btn sm ghost icon" disabled={!!busy} onclick={onClose}><X size={14} /></button></h3>
    <div class="small subtle">The jars they replace go to Mods › Backups, so each one can be put back.{server.state === 'running' ? ' Restart the server afterwards to load them.' : ''}</div>
    <div class="row wrap small">
      {#each groups as g (g.k)}<span class="row gap4"><span class="chip {g.tint}">{g.label}</span><span class="muted">{g.rows.filter((u) => picked.has(u.project_id)).length} of {g.rows.length}</span></span>{/each}
    </div>
    {#if bad}<div class="row small redt"><TriangleAlert size={14} />{bad} would stop the server loading: left unticked</div>{/if}
    <div class="list">
      {#each groups as g (g.k)}
        {@const all = g.rows.every((u) => picked.has(u.project_id))}
        <div class="gh row">
          <input type="checkbox" checked={all} onchange={() => group(g, !all)} />
          <b class="n">{g.rows.length}</b><span class="chip {g.tint}">{g.label}</span><span class="tiny muted ell">{g.about}</span>
        </div>
        {#each g.rows as u (u.project_id)}
          {@const on = picked.has(u.project_id)}
          <div class="u" class:dim={!on} role="presentation" onpointerenter={(e) => paintOver(e, 'review', order.indexOf(u.project_id), paintAt)}>
            <div class="row">
              <input type="checkbox" checked={on} onpointerdown={(e) => paintDown(e, 'review', order.indexOf(u.project_id), !on, paintAt)} onclick={(e) => paintClick(e, () => flip(u.project_id))} />
              {#if icon(u)}<img src={icon(u)} alt="" width="20" height="20" loading="lazy" />{:else}<span class="ph"></span>{/if}
              <button class="nm grow" onclick={() => toggleNotes(u)} title="What changed">{#if open === u.project_id}<ChevronDown size={13} />{:else}<ChevronRight size={13} />{/if}<span class="ell">{u.name}</span></button>
              <span class="vchip"><span class="ell muted">{u.from}</span><ArrowRight size={11} /><span class="ell to">{u.to}</span></span>
            </div>
            {#if u.issues?.length}<div class="prob tiny" title="{u.issues.join('; ')}. Hopper doesn't add or turn on mods with an update."><TriangleAlert size={11} /><span class="ell">{u.issues.join('; ')}</span></div>{/if}
            {#if open === u.project_id}<div class="notes small">{#if notes[u.project_id] === '…'}<span class="muted">Loading…</span>{:else if notes[u.project_id]}{@html md(notes[u.project_id])}{:else}<span class="muted">No changelog.</span>{/if}</div>{/if}
          </div>
        {/each}
      {/each}
    </div>
    {#if busy}<div class="small gold">{busy}</div>{/if}
    <div class="row end">
      <button class="btn" disabled={!!busy} onclick={onClose}>Cancel</button>
      <button class="btn pri" disabled={!picked.size || !!busy} onclick={() => onApply([...picked])}><Download size={14} />Update {picked.size}</button>
    </div>
  </div>
</div>

<style>
  .back { position: fixed; inset: 0; background: #0008; display: flex; justify-content: center; align-items: flex-start; padding: 64px 16px 16px; z-index: 60; }
  .box { width: min(680px, 100%); max-height: calc(100vh - 80px); gap: 10px; box-shadow: 0 24px 60px #000c; }
  .box h3 { margin: 0; font-size: 15px; }
  .list { overflow: auto; display: flex; flex-direction: column; gap: 2px; min-height: 0; }
  .gh { gap: 8px; padding: 8px 4px 4px; }
  .gh .n { width: 28px; }
  .gap4 { gap: 4px; }
  .u { padding: 4px 4px 4px 24px; border-radius: 6px; }
  .u:hover { background: #ffffff06; }
  .u.dim .nm, .u.dim img, .u.dim .vchip { opacity: 0.55; }
  .u img, .ph { width: 20px; height: 20px; border-radius: 4px; background: var(--raise); flex: none; }
  .nm { display: flex; gap: 6px; align-items: center; text-align: left; min-width: 0; font-size: 13px; }
  .grow { flex: 1; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .vchip { display: inline-flex; align-items: center; gap: 4px; max-width: 240px; min-width: 0; padding: 1px 7px; border-radius: 6px; background: var(--acc-dim); font-size: 11.5px; color: var(--acc); flex: none; }
  .to { color: var(--acc); }
  .prob { display: flex; align-items: center; gap: 4px; color: var(--red); margin: 2px 0 0 52px; min-width: 0; }
  .redt { color: var(--red); gap: 6px; }
  .wrap { flex-wrap: wrap; }
  .notes { margin: 6px 0 2px 52px; background: var(--base); border: 1px solid var(--line); border-radius: 6px; padding: 6px 10px; max-height: 240px; overflow: auto; overflow-wrap: anywhere; }
  .notes :global(ul) { margin: 2px 0 2px 18px; }
  .notes :global(h3), .notes :global(h4) { font-size: 12.5px; margin: 4px 0 2px; }
  .end { justify-content: flex-end; }
  .icon { padding: 3px 5px; }
  .gold { color: var(--gold); }
  input[type='checkbox'] { width: 15px; height: 15px; accent-color: var(--acc); flex: none; }
</style>
