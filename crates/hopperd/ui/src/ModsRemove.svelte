<script>
  // Remove's question and its Undo, as in Hopper desktop: "Remove X?" with
  // the mods that stop loading without it, "Also remove these mods?
  // Nothing else needs them" (offered, never ticked; drag down the boxes
  // to tick several), then "Removed X" with Undo for a few seconds.
  // Put once on a page that removes mods (lib/modops.js asks through it).
  import { onMount } from 'svelte';
  import { Archive, TriangleAlert, Undo2, X } from '@lucide/svelte';
  import { removing, answerRemove } from './lib/modsremove.svelte.js';
  import { undoRemove } from './lib/modops.js';
  import { toast } from './lib/toast.svelte.js';
  import { paintDown, paintOver, paintClick } from './lib/dragpick.svelte.js';

  let also = $state(new Set());
  let okBtn = $state();
  let undoing = $state(false);
  let timer;

  onMount(() => {
    removing.hosts++;
    return () => {
      removing.hosts--;
      answerRemove(null);
    };
  });

  const q = $derived(removing.ask);
  // A new question starts with nothing extra ticked.
  $effect(() => {
    if (q) {
      also = new Set();
      queueMicrotask(() => okBtn?.focus());
    }
  });

  function setAt(i, on) {
    const f = q?.orphans[i]?.file;
    if (!f || also.has(f) === on) return;
    const s = new Set(also);
    on ? s.add(f) : s.delete(f);
    also = s;
  }
  const flip = (i) => setAt(i, !also.has(q.orphans[i].file));

  // The Undo stays a few seconds, as desktop's toast.
  $effect(() => {
    const u = removing.undo;
    clearTimeout(timer);
    if (u) timer = setTimeout(() => removing.undo === u && (removing.undo = null), 9000);
    return () => clearTimeout(timer);
  });

  async function undo() {
    const u = removing.undo;
    if (!u || undoing) return;
    undoing = true;
    removing.undo = null;
    await undoRemove(u);
    undoing = false;
  }

  function keys(e) {
    if (e.key === 'Escape') {
      e.stopPropagation();
      answerRemove(null);
    }
  }
</script>

{#if q}
  <div class="dialog-back" role="presentation" onclick={(e) => e.target === e.currentTarget && answerRemove(null)} onkeydown={keys}>
    <form class="box popover" onsubmit={(e) => { e.preventDefault(); answerRemove([...also]); }}>
      <h3>{q.title}</h3>
      <p class="small subtle">Move to backups keeps a copy in .hopper-backups (Mods › Backups), with Undo.</p>
      {#if q.need.length}
        <div class="need small">
          <div class="row nh"><TriangleAlert size={14} />The server won't start without {q.need.length === 1 ? 'this' : 'these'}:</div>
          {#each q.need.slice(0, 12) as n (n.file)}<div class="ell">{n.name} needs it</div>{/each}
          {#if q.need.length > 12}<div class="tiny muted">and {q.need.length - 12} more</div>{/if}
        </div>
      {/if}
      {#if q.orphans.length}
        <div class="col libs">
          <b class="small">{q.orphans.length === 1 ? 'Also remove this mod? Nothing else needs it:' : `Also remove these ${q.orphans.length} mods? Nothing else needs them:`}</b>
          <div class="ol">
            {#each q.orphans as o, i (o.file)}
              {@const on = also.has(o.file)}
              <label class="row orow row-hover" onpointerenter={(e) => paintOver(e, 'orphans', i, setAt)}>
                <input type="checkbox" checked={on} onpointerdown={(e) => paintDown(e, 'orphans', i, !on, setAt)} onclick={(e) => paintClick(e, () => flip(i))} />
                {#if o.icon}<img src={o.icon} alt="" width="18" height="18" loading="lazy" />{:else}<span class="ph"></span>{/if}
                <span class="ell" title={o.file}>{o.name}</span>
              </label>
            {/each}
          </div>
        </div>
      {/if}
      <div class="row end">
        <button type="button" class="btn" onclick={() => answerRemove(null)}>Cancel</button>
        <button class="btn pri" bind:this={okBtn}><Archive size={13} />Move to backups</button>
      </div>
    </form>
  </div>
{/if}

{#if removing.undo}
  <!-- Above the panel's own toast when both show. -->
  <div class="toast undo" class:up={!!toast.text} role="status">
    <span class="grow">{removing.undo.text}</span>
    <button class="btn sm" disabled={undoing} onclick={undo}><Undo2 size={13} />Undo</button>
    <button class="btn sm ghost icon" title="Close" onclick={() => (removing.undo = null)}><X size={13} /></button>
  </div>
{/if}

<style>
  .box { width: min(460px, 100%); display: flex; flex-direction: column; gap: 12px; padding: 18px; max-height: calc(100vh - var(--dialog-top) - 16px); }
  .box h3 { margin: 0; font-size: 15px; font-weight: 600; }
  .need { display: flex; flex-direction: column; gap: 3px; padding: 10px 12px; border-radius: var(--radius); border: 1px solid #ef444480; background: var(--red-dim); }
  .nh { gap: 6px; color: var(--red); font-weight: 600; }
  .libs { gap: 6px; min-height: 0; }
  .ol { overflow: auto; min-height: 0; max-height: 240px; display: flex; flex-direction: column; }
  .orow { gap: 8px; padding: 4px 6px; border-radius: var(--radius); cursor: pointer; font-size: 12.5px; }
  .orow input { width: 15px; height: 15px; accent-color: var(--acc); flex: none; }
  .orow img, .ph { width: 18px; height: 18px; border-radius: 4px; background: var(--raise); flex: none; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .end { justify-content: flex-end; }
  .grow { flex: 1; min-width: 0; }
  .undo { align-items: center; }
  .undo.up { bottom: 74px; }
  .icon { padding: 3px 5px; }
</style>
