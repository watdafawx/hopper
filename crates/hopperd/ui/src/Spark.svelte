<script>
  let { values = [], color = 'var(--acc)', height = 34 } = $props();
  const pts = $derived(
    values.length < 2
      ? ''
      : values.map((v, i) => `${(i * 200) / (values.length - 1)},${height - 2 - v * (height - 4)}`).join(' '),
  );
</script>

<svg viewBox="0 0 200 {height}" preserveAspectRatio="none" style:height="{height}px" aria-hidden="true">
  {#if pts}<polyline fill="none" stroke={color} stroke-width="1.6" vector-effect="non-scaling-stroke" points={pts} />{/if}
</svg>

<style>
  svg { width: 100%; display: block; margin-top: 8px; }
</style>
