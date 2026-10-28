<script>
  // A tiny line chart, oldest first, with a faint fill under the line
  // (desktop's widgets::sparkline). `color` is any CSS colour, var(--blue)
  // too: it's set as a style, where custom properties resolve.
  let { values = [], color = 'var(--acc)', height = 34 } = $props();
  const id = `sp${Math.random().toString(36).slice(2, 9)}`;
  const pts = $derived(
    values.length < 2
      ? ''
      : values.map((v, i) => `${(i * 200) / (values.length - 1)},${height - 2 - v * (height - 4)}`).join(' '),
  );
</script>

<svg viewBox="0 0 200 {height}" preserveAspectRatio="none" style:height="{height}px" aria-hidden="true">
  <defs>
    <linearGradient {id} x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" style:stop-color={color} stop-opacity="0.3" />
      <stop offset="1" style:stop-color={color} stop-opacity="0" />
    </linearGradient>
  </defs>
  {#if pts}
    <polygon fill="url(#{id})" points="0,{height} {pts} 200,{height}" />
    <polyline fill="none" style:stroke={color} stroke-width="1.5" vector-effect="non-scaling-stroke" points={pts} />
  {/if}
</svg>

<style>
  svg { width: 100%; display: block; margin-top: 8px; }
</style>
