<script>
  // Edit config…: a mod's config files in config/, one to open in the
  // editor (desktop's config editor lists them the same way).
  import { X, FileCog } from '@lucide/svelte';
  import { dialog } from './lib/dialog.svelte.js';

  let { mod, files, onPick, onClose } = $props();
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && !dialog.open && onClose()} />

<div class="dialog-back" role="presentation" onclick={(e) => e.target === e.currentTarget && onClose()}>
  <div class="box popover col">
    <h3 class="row">
      {#if mod.icon}<img src={mod.icon} alt="" width="20" height="20" />{/if}
      Edit {mod.name}'s config<span class="sp"></span><button class="btn sm ghost icon" title="Close" onclick={onClose}><X size={14} /></button>
    </h3>
    <div class="small subtle">{files.length} config files are named after it. Pick one to open in the editor.</div>
    <div class="list">
      {#each files as f (f)}
        <button class="it row-hover" onclick={() => onPick(f)}><FileCog size={13} /><span class="mono ell">{f}</span></button>
      {/each}
    </div>
  </div>
</div>

<style>
  .box { width: min(520px, 100%); max-height: calc(100vh - var(--dialog-top) - 16px); gap: 10px; padding: 18px; }
  .box h3 { margin: 0; font-size: 15px; font-weight: 600; display: flex; align-items: center; gap: 8px; }
  .box h3 img { border-radius: 4px; }
  .list { overflow: auto; display: flex; flex-direction: column; gap: 2px; min-height: 0; }
  .it { display: flex; align-items: center; gap: 8px; padding: 6px 8px; border-radius: 6px; text-align: left; font-size: 12.5px; color: var(--subtle); }
  .it:hover { color: var(--text); }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .icon { padding: 3px 5px; }
</style>
