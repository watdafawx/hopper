<script>
  // The unsaved-changes bar under the config view: how many and where,
  // Discard, Review (old → new, untick to drop one) and Save, the one
  // filled button. Save writes each file with its edits checked together;
  // the old copy goes to the server's .hopper-backups, and the message says
  // where. A value the panel refuses stays unsaved with its reason on its row.
  import { X, ArrowRight } from '@lucide/svelte';
  import { MASK, pendingList, discardAll, saveAll, dropEdit, shortRel } from './configs.svelte.js';
  import { say } from './toast.svelte.js';
  import { confirmIt } from './dialog.svelte.js';

  let { server, compact = false } = $props();

  const list = $derived(pendingList());
  const files = $derived([...new Set(list.map((x) => x.rel))]);
  const where = $derived(files.length === 1 ? shortRel(files[0]) : `${files.length} files`);
  const running = $derived(['running', 'starting'].includes(server.state));
  let saving = $state(false);
  let review = $state(null);

  async function save(keep = null) {
    if (saving) return;
    saving = true;
    try {
      const r = await saveAll(server.id, keep);
      if (r.saved) {
        const back = r.backups.length === 1 ? ` The old copy is in ${r.backups[0]}.` : r.backups.length ? ` The old copies are in ${server.name}'s .hopper-backups.` : '';
        const re = r.reapplied.length ? ` ${r.reapplied.map(shortRel).join(', ')} had changed on the server, so your changes went onto the newer copy.` : '';
        say(`Saved ${r.saved} setting${r.saved === 1 ? '' : 's'}.${back}${re}${running ? ' Most mods pick this up live; some need a restart.' : ''}`);
      }
      if (r.failed.length) say(`Not saved: ${r.failed.map(shortRel).join(', ')}. The reason is on the setting.`, true);
    } catch (e) {
      say(e.message, true);
    } finally {
      saving = false;
    }
  }

  async function discard() {
    if (list.length > 3 && !(await confirmIt(`Discard ${list.length} unsaved changes?`, { ok: 'Discard', danger: true }))) return;
    discardAll();
  }

  function openReview() {
    review = new Set(list.map((x) => `${x.rel}\u0000${x.id}`));
  }
  function toggle(k) {
    const n = new Set(review);
    if (n.has(k)) n.delete(k);
    else n.add(k);
    review = n;
  }
  async function saveReviewed() {
    const keep = review;
    // Unticked ones are dropped, as the dialog says.
    for (const x of list) if (!keep.has(`${x.rel}\u0000${x.id}`)) dropEdit(x.rel, x.id);
    review = null;
    await save(keep);
  }
  // A hidden value (a token) stays hidden here too.
  const shown = (x, hidden = false) => (hidden ? '(new value)' : x.value === '' ? '(empty)' : x.value);
</script>

{#if list.length}
  <div class="ub" class:compact role="region" aria-label="Unsaved changes">
    <span class="dot acc"></span>
    <span class="what"><b>{list.length} unsaved change{list.length === 1 ? '' : 's'}</b> in <span class="mono">{where}</span></span>
    {#if !compact}<span class="cap">{running ? 'The server is running: most mods pick changes up live.' : 'Applies on the next start.'} The old copy goes to .hopper-backups.</span>{/if}
    <span class="sp"></span>
    <button class="btn ghost" disabled={saving} onclick={discard}>Discard</button>
    <button class="btn" disabled={saving} onclick={openReview}>Review</button>
    <button class="btn pri" disabled={saving} onclick={() => save()}>{#if saving}<span class="spinner"></span>{/if}Save</button>
  </div>
{/if}

{#if review}
  <div class="dialog-back" role="presentation" onclick={(ev) => ev.target === ev.currentTarget && (review = null)} onkeydown={(ev) => ev.key === 'Escape' && (review = null)}>
    <div class="box popover col" role="dialog" aria-label="Review changes">
      <h3 class="row">Review {list.length} change{list.length === 1 ? '' : 's'}<span class="sp"></span><button class="btn sm ghost icon" title="Close" onclick={() => (review = null)}><X size={14} /></button></h3>
      <div class="small subtle">Untick a change to drop it. The rest are saved, with the old copy of each file in .hopper-backups.</div>
      <div class="rl">
        {#each files as f (f)}
          <div class="rf mono">{shortRel(f)}</div>
          {#each list.filter((x) => x.rel === f) as x (x.id)}
            {@const k = `${x.rel}\u0000${x.id}`}
            <label class="ri row-hover">
              <input type="checkbox" checked={review.has(k)} onchange={() => toggle(k)} />
              <span class="rn"><b>{x.label}</b><span class="mono tiny muted">{[...x.path, x.key].join('.')}</span></span>
              <span class="ov mono"><s class="muted ell">{shown({ value: x.old })}</s><ArrowRight size={12} /><span class="ell">{shown(x, x.old === MASK)}</span></span>
            </label>
          {/each}
        {/each}
      </div>
      <div class="row">
        <span class="sp"></span>
        <button class="btn ghost" onclick={() => (review = null)}>Cancel</button>
        <button class="btn pri" disabled={!review.size || saving} onclick={saveReviewed}>Save {review.size}</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .ub { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; padding: 10px 14px; border-top: 1px solid var(--line); background: var(--base); position: sticky; bottom: 0; z-index: 5; }
  .ub.compact { padding: 8px 10px; gap: 6px; }
  .dot.acc { width: 6px; height: 6px; background: var(--acc); }
  .what { min-width: 0; }
  .cap { color: var(--muted); font-size: 11.5px; }
  .box { width: min(640px, 100%); gap: 10px; padding: 18px; }
  .box h3 { margin: 0; font-size: 15px; font-weight: 600; }
  .rl { display: flex; flex-direction: column; overflow: auto; min-height: 0; max-height: 55vh; }
  .rf { font-size: 11.5px; color: var(--muted); padding: 10px 8px 4px; }
  .ri { display: grid; grid-template-columns: auto minmax(0, 1fr) minmax(0, 1.1fr); gap: 10px; align-items: center; padding: 6px 8px; border-radius: var(--radius); cursor: pointer; }
  .ri input { accent-color: var(--acc); }
  .rn { display: flex; flex-direction: column; min-width: 0; }
  .rn .tiny { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .ov { display: flex; align-items: center; gap: 6px; font-size: 12px; min-width: 0; justify-content: flex-end; }
  .ov :global(svg) { flex: none; color: var(--muted); }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .icon { padding: 3px 5px; }
  @media (max-width: 700px) {
    .ub .cap { display: none; }
    .ub .btn { min-height: 44px; }
    .ri { grid-template-columns: auto minmax(0, 1fr); min-height: 44px; }
    .ov { grid-column: 2; justify-content: flex-start; }
  }
</style>
