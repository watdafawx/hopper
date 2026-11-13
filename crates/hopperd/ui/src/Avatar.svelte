<script>
  // A server's picture: its server-icon.png (what the multiplayer list
  // shows), else its first letter on a colour. Sidebar, header and Overview
  // all use this. A server on another panel (`panel`, left out: the one
  // whose page is open) loads its icon with that panel's token, since an
  // <img> can't send one (lib/panelfiles.svelte.js).
  import { color } from './lib/format.js';
  import { target } from './lib/api.js';
  import { panelSrc } from './lib/panelfiles.svelte.js';
  let { server, cls = 'ic', panel = target.panel } = $props();
  let broken = $state(false);
  // The live feed hands over a new `server` object every second; key on the icon.
  const icon = $derived(server.icon);
  // A new icon gets another try.
  $effect(() => {
    icon;
    broken = false;
  });
  const pic = panelSrc(() => (icon && !broken ? `/api/v1/servers/${encodeURIComponent(server.id)}/icon?v=${icon}` : ''), () => panel);
  $effect(() => {
    if (pic.failed) broken = true;
  });
</script>

{#if pic.src}
  <img class="{cls} pic" src={pic.src} alt="" onerror={() => (broken = true)} />
{:else}
  <span class={cls} style:background={color(server.name)}>{server.name[0]?.toUpperCase()}</span>
{/if}

<style>
  .pic { object-fit: cover; image-rendering: pixelated; background: var(--raise); padding: 0; }
</style>
