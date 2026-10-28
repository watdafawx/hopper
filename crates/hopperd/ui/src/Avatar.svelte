<script>
  // A server's picture: its server-icon.png (what the multiplayer list
  // shows), else its first letter on a colour. Sidebar, header and Overview
  // all use this.
  import { color } from './lib/format.js';
  let { server, cls = 'ic' } = $props();
  let broken = $state(false);
  // A new icon gets another try.
  $effect(() => {
    server.icon;
    broken = false;
  });
  const src = $derived(server.icon && !broken ? `/api/v1/servers/${encodeURIComponent(server.id)}/icon?v=${server.icon}` : '');
</script>

{#if src}
  <img class="{cls} pic" {src} alt="" onerror={() => (broken = true)} />
{:else}
  <span class={cls} style:background={color(server.name)}>{server.name[0]?.toUpperCase()}</span>
{/if}

<style>
  .pic { object-fit: cover; image-rendering: pixelated; background: var(--raise); padding: 0; }
</style>
