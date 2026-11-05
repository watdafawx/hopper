<script>
  // The History button in a server's header: opens Mods › History, with a
  // badge counting what was added since it was last opened, and a short
  // turn of the icon when a new entry lands. Its own small component, so
  // the header isn't redrawn by it.
  import { History } from '@lucide/svelte';
  import { onChange } from './lib/changes.svelte.js';
  import { fresh, refresh } from './lib/historySeen.svelte.js';

  let { id, onOpen } = $props();
  const count = $derived(fresh[id] ?? 0);
  // Bumped when the count grows, to run the animation once.
  let pulse = $state(0);
  let last = null;
  $effect(() => {
    if (last != null && count > last) pulse++;
    last = count;
  });
  $effect(() => {
    refresh(id);
    const off = onChange(id, ['mods'], () => refresh(id));
    // Changes made outside the panel (Hopper desktop, on disk) arrive as events too; this is the fallback.
    const t = setInterval(() => document.hidden || refresh(id), 15000);
    return () => { off(); clearInterval(t); };
  });
</script>

<button class="btn icon ghost hist" title={count ? `History (${count} new)` : 'History'} aria-label="History" onclick={onOpen}>
  {#key pulse}<span class="ico" class:turn={pulse > 0}><History size={15} /></span>{/key}
  {#if count}<span class="badge">{count > 99 ? '99+' : count}</span>{/if}
</button>

<style>
  .hist { position: relative; }
  .ico { display: inline-flex; }
  .ico.turn { animation: turn 550ms cubic-bezier(0.22, 1, 0.36, 1) 1; }
  @keyframes turn { from { transform: rotate(360deg); } to { transform: rotate(0); } }
  .badge {
    position: absolute; top: -4px; right: -4px; min-width: 15px; height: 15px; padding: 0 4px;
    border-radius: 999px; background: var(--acc); color: #fff; font-size: 10px; font-weight: 600;
    line-height: 15px; text-align: center;
  }
  @media (prefers-reduced-motion: reduce) { .ico.turn { animation: none; } }
</style>
