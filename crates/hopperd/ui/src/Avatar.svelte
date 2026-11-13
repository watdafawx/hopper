<script>
  // A server's picture: its server-icon.png (what the multiplayer list
  // shows), else its first letter on a colour. Sidebar, header and Overview
  // all use this. A server on another panel (`panel`, left out: the one
  // whose page is open) loads its icon with that panel's token, since an
  // <img> can't send one.
  import { color } from './lib/format.js';
  import { target, apiBase, headersFor } from './lib/api.js';
  let { server, cls = 'ic', panel = target.panel } = $props();
  let broken = $state(false);
  // The live feed hands over a new `server` object every second; key on the icon.
  const icon = $derived(server.icon);
  // A new icon gets another try.
  $effect(() => {
    icon;
    broken = false;
  });
  const path = $derived(server.icon && !broken ? `/servers/${encodeURIComponent(server.id)}/icon?v=${server.icon}` : '');
  let blob = $state('');
  $effect(() => {
    if (!panel || !path) return;
    let url = '';
    let gone = false;
    fetch(`${apiBase(panel)}${path}`, { headers: headersFor(panel), credentials: 'omit' })
      .then((r) => (r.ok ? r.blob() : Promise.reject(new Error())))
      .then((b) => {
        if (gone) return;
        url = URL.createObjectURL(b);
        blob = url;
      })
      .catch(() => !gone && (broken = true));
    return () => {
      gone = true;
      if (url) URL.revokeObjectURL(url);
    };
  });
  const src = $derived(!path ? '' : panel ? blob : `/api/v1${path}`);
</script>

{#if src}
  <img class="{cls} pic" {src} alt="" onerror={() => (broken = true)} />
{:else}
  <span class={cls} style:background={color(server.name)}>{server.name[0]?.toUpperCase()}</span>
{/if}

<style>
  .pic { object-fit: cover; image-rendering: pixelated; background: var(--raise); padding: 0; }
</style>
