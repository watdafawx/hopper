<script>
  // The bulk bar of a list with several rows picked, as desktop's
  // selection::bar: "3 selected", the list's own actions (children), Delete
  // (when the list can) and a clear button. It floats over the bottom of its
  // list: sticky inside the list's scrolling area, or with `over` placed on
  // the list's (position: relative) wrapper.
  import { Trash2, X } from '@lucide/svelte';

  let { n, info = '', onClear, onDelete = null, deleteLabel = 'Delete', deleteTitle = '', busy = false, over = false, children } = $props();
</script>

<div class="selw" class:over>
  <div class="selb row" role="toolbar" aria-label="{n} selected">
    <b>{n} selected</b>{#if info}<span class="muted small">{info}</span>{/if}
    <span class="vr"></span>
    {@render children?.()}
    {#if onDelete}<button class="btn sm ghost bad" disabled={busy} title={deleteTitle} onclick={onDelete}><Trash2 size={13} />{deleteLabel}</button>{/if}
    <span class="vr"></span>
    <button class="btn sm ghost icon" title="Clear selection (Esc)" aria-label="Clear selection" onclick={onClear}><X size={13} /></button>
  </div>
</div>

<style>
  .selw { position: sticky; bottom: 16px; display: flex; justify-content: center; z-index: 30; pointer-events: none; animation: rise 0.16s ease-out; }
  .selw.over { position: absolute; left: 0; right: 0; bottom: 16px; padding: 0 12px; }
  @keyframes rise { from { opacity: 0; transform: translateY(8px); } }
  .selb { pointer-events: auto; gap: 4px; background: var(--raise); border: 1px solid var(--line2); border-radius: var(--radius-lg); padding: 6px 8px; flex-wrap: wrap; box-shadow: 0 14px 40px #000c; max-width: 100%; }
  .selb b { padding: 0 8px; font-size: 12.5px; white-space: nowrap; }
  .selb b:has(+ .muted) { padding-right: 0; }
  .selb .muted { padding-right: 8px; white-space: nowrap; }
  .vr { width: 1px; height: 20px; background: var(--line2); margin: 0 2px; }
  .bad { color: var(--red); }
</style>
