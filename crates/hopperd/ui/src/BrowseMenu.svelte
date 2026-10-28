<script>
  // A project's right-click menu, as desktop's Browse cards have: Open,
  // anything the caller adds (`extra(act)`), then Open on <site> and Copy
  // link. `at`: { r (a card: url, source), x, y } from the contextmenu event.
  import { onMount } from 'svelte';
  import { PanelRight, ExternalLink, Copy } from '@lucide/svelte';
  import { SITE, copyText } from './lib/browse.js';
  import { say } from './lib/toast.svelte.js';

  let { at, onClose, onOpen = null, extra = null } = $props();

  const W = 220;
  const H = 170;
  const x = $derived(Math.max(8, Math.min(at.x, innerWidth - W - 8)));
  const y = $derived(Math.max(8, Math.min(at.y, innerHeight - H - 8)));
  let el = $state();

  /** Closes the menu, then does `f`. */
  function act(f) {
    onClose();
    f();
  }

  async function copy() {
    const url = at.r.url;
    onClose();
    say((await copyText(url)) ? 'Copied the link' : `Couldn't copy: ${url}`, false);
  }

  function down(e) {
    if (!el?.contains(e.target)) onClose();
  }

  function keys(e) {
    if (e.key === 'Escape') {
      e.stopPropagation();
      onClose();
      return;
    }
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    const items = [...(el?.querySelectorAll('.menu-item') || [])];
    if (!items.length) return;
    e.preventDefault();
    const i = items.indexOf(document.activeElement);
    items[(i + (e.key === 'ArrowDown' ? 1 : -1) + items.length) % items.length].focus();
  }

  onMount(() => el?.querySelector('.menu-item')?.focus({ preventScroll: true }));
</script>

<svelte:window onpointerdown={down} onwheel={onClose} onresize={onClose} onblur={onClose} />

<div bind:this={el} class="bm popover menu" style:left="{x}px" style:top="{y}px" role="menu" tabindex="-1" onkeydown={keys} oncontextmenu={(e) => e.preventDefault()}>
  {#if onOpen}<button class="menu-item" role="menuitem" onclick={() => act(onOpen)}><PanelRight size={14} />Open</button>{/if}
  {@render extra?.(act)}
  {#if at.r.url}
    {#if onOpen || extra}<div class="menu-sep"></div>{/if}
    <a class="menu-item" role="menuitem" href={at.r.url} target="_blank" rel="noreferrer noopener" onclick={onClose}><ExternalLink size={14} />Open on {SITE[at.r.source] || 'its site'}</a>
    <button class="menu-item" role="menuitem" onclick={copy}><Copy size={14} />Copy link</button>
  {/if}
</div>

<style>
  .bm { position: fixed; z-index: 90; width: 220px; }
  .bm a.menu-item:hover { color: var(--text); }
</style>
