<script>
  // Asks before a restore, as desktop's "Restore "…"?" dialog: when it's
  // from, what goes back (what changed since, when known), and a "Back up
  // first" tick, off by default: backups are the person's to make.
  import { RotateCcw } from '@lucide/svelte';
  import { bytes } from './lib/format.js';
  import { fullDate, whenLabel, plural } from './lib/worlds.js';

  /** b: the backup; paths: the picked files (empty: all of it);
   * changes: { changed, removed, added } since then (whole restores). */
  let { b, title, paths = [], changes = null, onOk, onCancel } = $props();
  const all = $derived(!paths.length);
  let okBtn = $state();
  let backupFirst = $state(false);
  let box = $state();

  // Focus Restore; give focus back to what had it when closed.
  $effect(() => {
    const before = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    okBtn?.focus();
    return () => before?.isConnected && before.focus();
  });

  // On the window, as the panel's other dialogs: Escape wherever focus
  // went, and Tab stays inside.
  function keys(e) {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      onCancel();
    } else if (e.key === 'Tab' && box) {
      const els = [...box.querySelectorAll('button:not(:disabled), input:not(:disabled)')];
      if (!els.length) return;
      const [first, last] = [els[0], els[els.length - 1]];
      if (!box.contains(document.activeElement)) {
        e.preventDefault();
        first.focus();
      } else if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    }
  }

  /** "3 files back to the old copy, 2 put back"; desktop's wording. */
  const changeLine = (c) => {
    const out = [];
    if (c.changed.length) out.push(`${plural(c.changed.length, 'file')} back to the old copy`);
    if (c.removed.length) out.push(`${c.removed.length} put back`);
    return out.join(', ') || 'No changes';
  };
</script>

<svelte:window onkeydowncapture={keys} />

<div class="dialog-back" role="presentation" onclick={(e) => e.target === e.currentTarget && onCancel()}>
  <div class="box popover" role="dialog" aria-modal="true" aria-label="Restore" bind:this={box}>
    <h3>{all ? `Restore "${title}"?` : `Restore ${plural(paths.length, 'item')} from "${title}"?`}</h3>
    <div class="small">{whenLabel(b.at)}. {all ? 'Your server goes back to how it was then.' : 'Those files go back to the copy in this backup.'}</div>
    <div class="kv small">
      <span>From</span><span>{fullDate(b.at)}</span>
      {#if all}
        {#if changes}
          <span>Files</span><span>{changeLine(changes)}</span>
          <span>Worlds</span><span>Replaced whole; the current ones are moved aside, not deleted{changes.added.length ? ` (with ${plural(changes.added.length, 'file')} made since)` : ''}</span>
        {:else}
          <span>Files</span><span>{plural(b.files, 'file')} · {bytes(b.size)} ({b.scope === 'full' ? 'everything' : 'the worlds'})</span>
          <span>Worlds</span><span>Replaced whole; the current ones are moved aside, not deleted</span>
        {/if}
        {#if b.scope === 'full'}<span>Other files</span><span>Files added since then outside the worlds stay</span>{/if}
      {:else}
        <span>Files</span><span class="ell" title={paths.join('\n')}>{paths.slice(0, 3).join(', ')}{paths.length > 3 ? ` and ${paths.length - 3} more` : ''}</span>
      {/if}
    </div>
    <label class="row small"><input type="checkbox" bind:checked={backupFirst} />Back up first, so this can be undone</label>
    {#if !backupFirst}<div class="small muted">Without a backup, a restore can't be undone.</div>{/if}
    <div class="row end">
      <button class="btn" onclick={onCancel}>Cancel</button>
      <button class="btn pri" bind:this={okBtn} onclick={() => onOk(backupFirst)}><RotateCcw size={13} />Restore</button>
    </div>
  </div>
</div>

<style>
  .box { width: min(460px, 100%); display: flex; flex-direction: column; gap: 10px; padding: 18px; }
  .box h3 { margin: 0; font-size: 15px; font-weight: 600; }
  .kv { display: grid; grid-template-columns: 5.5rem 1fr; gap: 4px 12px; }
  .kv span:nth-child(odd) { color: var(--muted); }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .end { justify-content: flex-end; margin-top: 4px; }
</style>
