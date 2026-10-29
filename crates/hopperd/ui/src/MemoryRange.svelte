<script>
  // Two thumbs on one track, like desktop's memory slider: the left one is
  // -Xms, the right one -Xmx. The left thumb at the very start means "no
  // -Xms" (0), so Java picks its own start size.
  let { xms = $bindable(0), xmx = $bindable(4096), min = 1024, max = 32768, step = 512, label = 'Memory' } = $props();
  const lo = $derived(Math.max(min, Math.min(xms || min, xmx)));
  const pct = (v) => ((Math.min(max, Math.max(min, v)) - min) / (max - min || 1)) * 100;

  function setLo(v) {
    v = Math.min(Number(v), xmx);
    xms = v <= min ? 0 : v;
  }
  function setHi(v) {
    v = Math.max(Number(v), min);
    xmx = v;
    if (xms > v) xms = v;
  }
</script>

<div class="dual" style="--lo:{pct(lo)}%; --hi:{pct(xmx)}%">
  <div class="track"></div>
  <div class="fill"></div>
  <input type="range" {min} {max} {step} value={lo} oninput={(e) => setLo(e.currentTarget.value)} aria-label="{label}: start size (-Xms)" />
  <input type="range" {min} {max} {step} value={Math.min(xmx, max)} oninput={(e) => setHi(e.currentTarget.value)} aria-label="{label}: most it may use (-Xmx)" />
</div>

<style>
  .dual { position: relative; width: 260px; height: 22px; flex: none; }
  .track, .fill { position: absolute; top: 9px; height: 4px; border-radius: 2px; }
  .track { left: 0; right: 0; background: var(--line2); }
  .fill { left: var(--lo); right: calc(100% - var(--hi)); background: var(--acc); }
  input { position: absolute; inset: 0; width: 100%; margin: 0; background: none; pointer-events: none; appearance: none; -webkit-appearance: none; height: 22px; }
  input::-webkit-slider-runnable-track { background: none; }
  input::-moz-range-track { background: none; }
  input::-webkit-slider-thumb { -webkit-appearance: none; pointer-events: auto; width: 16px; height: 16px; border-radius: 50%; background: var(--text); border: 2px solid var(--acc); cursor: pointer; }
  input::-moz-range-thumb { pointer-events: auto; width: 13px; height: 13px; border-radius: 50%; background: var(--text); border: 2px solid var(--acc); cursor: pointer; }
  input:focus-visible::-webkit-slider-thumb { box-shadow: 0 0 0 2px var(--bg), 0 0 0 4px var(--acc); }
  input:focus-visible::-moz-range-thumb { box-shadow: 0 0 0 2px var(--bg), 0 0 0 4px var(--acc); }
  @media (max-width: 900px) { .dual { width: 100%; } }
</style>
