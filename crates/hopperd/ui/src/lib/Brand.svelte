<script>
  // A site, loader or git host logo, from the same SVG files Hopper desktop
  // uses (assets/brand). Single-color: it takes the text color, or `color`.
  const FILES = import.meta.glob('../../../../../assets/brand/*.svg', { query: '?raw', import: 'default', eager: true });
  const SVG = Object.fromEntries(Object.entries(FILES).map(([p, s]) => [p.split('/').pop().replace('.svg', ''), s]));

  let { name, size = 13, color = '' } = $props();
  // Any case: "NeoForge", "GitHub", "curseforge".
  const svg = $derived(SVG[name?.toLowerCase()] || '');
</script>

{#if svg}<span class="brand" style:width="{size}px" style:height="{size}px" style:color={color || null} aria-hidden="true">{@html svg}</span>{/if}

<style>
  .brand { display: inline-flex; flex: none; }
  .brand :global(svg) { width: 100%; height: 100%; fill: currentColor; }
</style>
