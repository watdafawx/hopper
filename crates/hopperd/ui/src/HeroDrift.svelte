<script>
  // The server header's backdrop ("reactive drift"): soft colour blobs taken
  // from the server icon wander slowly behind the title, and speed up when you
  // act (lib/heroDrift.js). Desktop Hopper draws the same in its server header.
  // No icon, or one the canvas can't read: nothing is drawn.
  import { onMount } from 'svelte';
  import { onKick, jobRunning } from './lib/heroDrift.js';

  /** src: the icon's URL. */
  let { src = '' } = $props();

  const SPOTS = [
    [-0.04, -0.25, 0.30, 1.1, 0],
    [0.20, -0.05, 0.26, 1.0, 1],
    [0.45, -0.35, 0.30, 1.2, 2],
    [0.72, -0.10, 0.28, 1.0, 0],
    [0.90, 0.20, 0.20, 0.9, 1],
  ];
  const BAND = { y: 0.38, w: 0.09, h: 0.2 };
  const R = [1, Math.SQRT2, 1.6180339887, Math.E / 2, Math.PI / 2.3];
  const wander = (t, k) =>
    (Math.sin(t * 0.11 * R[k % 5] + k) + 0.6 * Math.sin(t * 0.067 * R[(k + 2) % 5] + 2.1 * k) + 0.4 * Math.sin(t * 0.029 * R[(k + 4) % 5] + 0.7 * k)) / 2;

  // Colours per icon URL; null = unusable (tainted, missing, grey).
  const cache = new Map();

  function hsl(r, g, b) {
    const mx = Math.max(r, g, b), mn = Math.min(r, g, b), l = (mx + mn) / 2, dl = mx - mn;
    let h = 0, s = 0;
    if (dl > 1e-6) {
      s = dl / (1 - Math.abs(2 * l - 1));
      h = mx === r ? ((g - b) / dl + 6) % 6 : mx === g ? (b - r) / dl + 2 : (r - g) / dl + 4;
      h /= 6;
    }
    return [h, s, l, mx];
  }
  const css = (h, s, l) => `hsl(${(h * 360).toFixed(0)} ${(s * 100).toFixed(1)}% ${(l * 100).toFixed(1)}%)`;

  function palette(img) {
    const c = document.createElement('canvas');
    c.width = c.height = 64;
    const g = c.getContext('2d', { willReadFrequently: true });
    g.drawImage(img, 0, 0, 64, 64);
    const d = g.getImageData(0, 0, 64, 64).data; // throws when cross-origin tainted
    const buckets = Array.from({ length: 12 }, () => ({ w: 0, r: 0, g: 0, b: 0 }));
    let dark = null;
    let darkL = 2;
    for (let i = 0; i < d.length; i += 4) {
      if (d[i + 3] < 128) continue;
      const [r, gr, b] = [d[i] / 255, d[i + 1] / 255, d[i + 2] / 255];
      const [h, s, l, v] = hsl(r, gr, b);
      if (l < darkL) { darkL = l; dark = [h, s]; }
      const w = s * s * v;
      const k = Math.min(11, Math.floor(h * 12));
      buckets[k].w += w; buckets[k].r += r * w; buckets[k].g += gr * w; buckets[k].b += b * w;
    }
    const top = buckets.filter((x) => x.w > 0.5).sort((a, b) => b.w - a.w).slice(0, 3);
    if (!top.length || !dark) return null;
    const cols = top.map((x) => {
      const [h, s, l] = hsl(x.r / x.w, x.g / x.w, x.b / x.w);
      return css(h, Math.min(0.95, Math.max(0.55, s)) * 0.75, Math.min(0.58, Math.max(0.38, l)));
    });
    while (cols.length < 3) cols.push(cols[cols.length - 1]);
    return { cols, base: css(dark[0], dark[1] * 0.6, Math.min(darkL, 0.06)) };
  }

  let pal = $state(null);
  $effect(() => {
    const url = src;
    pal = null;
    if (!url) return;
    if (cache.has(url)) { pal = cache.get(url); return; }
    let dead = false;
    const img = new Image();
    img.onload = () => {
      let p = null;
      try { p = palette(img); } catch { /* tainted */ }
      cache.set(url, p);
      if (!dead) pal = p;
    };
    img.onerror = () => cache.set(url, null);
    img.src = url;
    return () => (dead = true);
  });

  let root = $state();
  let blobs = [];
  let band = [];
  let w = 0, h = 0;

  // Motion state: targets decay, shown values ease toward them.
  let t = 0, last = 0, drawn = 0, raf = 0, lastInput = 0;
  let tE = 0, tP = 0, tS = 0, E = 0, P = 0, S = 0;
  let reduced = false;

  function draw() {
    if (!w) return;
    SPOTS.forEach(([x, y], i) => {
      const el = blobs[i];
      if (!el) return;
      const ox = wander(t, i) * 0.09 + P * (1 - 0.15 * i);
      const oy = wander(t, i + 5) * 0.28;
      const sc = (1 + wander(t, i + 10) * 0.18) * (1 + S * 0.1);
      el.style.transform = `translate3d(${((x + ox) * w).toFixed(1)}px, ${((y + oy) * h).toFixed(1)}px, 0) scale(${sc.toFixed(3)})`;
    });
    const shift = wander(t, 20) * 0.08;
    band.forEach((el, i) => {
      if (!el) return;
      const x = 0.10 + 0.085 * i + shift + wander(t, 30 + i) * 0.02;
      const y = BAND.y + wander(t, 40 + i) * 0.05;
      el.style.transform = `translate3d(${(x * w).toFixed(1)}px, ${(y * h).toFixed(1)}px, 0)`;
    });
  }

  const boosted = () => jobRunning() || E > 0.02 || Math.abs(P) > 0.003 || S > 0.01;
  const allowed = () => !reduced && !document.hidden && document.hasFocus() && performance.now() - lastInput < 20000;

  function tick(now) {
    raf = 0;
    if (!allowed()) return;
    if (!boosted() && now - drawn < 83) { raf = requestAnimationFrame(tick); return; }
    const dt = Math.min((now - last) / 1000, 0.1);
    last = now; drawn = now;
    tE *= Math.exp(-dt / 0.9); tP *= Math.exp(-dt / 0.8); tS *= Math.exp(-dt / 0.7);
    const k = 1 - Math.exp(-dt / 0.3);
    E += (tE - E) * k; P += (tP - P) * k; S += (tS - S) * k;
    t += dt * (1 + E * 7 + (jobRunning() ? 2 : 0));
    draw();
    raf = requestAnimationFrame(tick);
  }
  function wake() {
    if (raf || !pal || !allowed()) return;
    last = performance.now();
    raf = requestAnimationFrame(tick);
  }
  function stop() {
    if (raf) cancelAnimationFrame(raf);
    raf = 0;
  }
  function input() {
    lastInput = performance.now();
    wake();
  }

  onMount(() => {
    lastInput = performance.now();
    const mq = matchMedia('(prefers-reduced-motion: reduce)');
    reduced = mq.matches;
    const onMq = () => {
      reduced = mq.matches;
      if (reduced) { stop(); t = 0; draw(); } else wake();
    };
    mq.addEventListener('change', onMq);
    const ro = new ResizeObserver(() => { w = root.clientWidth; h = root.clientHeight; draw(); });
    ro.observe(root);
    const off = onKick((n, dir) => {
      if (reduced) return;
      tE = Math.min(4, tE + n * 1.5);
      tP = Math.max(-0.12, Math.min(0.12, tP + dir * 0.06));
      tS = Math.min(1, tS + 0.6);
      wake();
    });
    const vis = () => (document.hidden ? stop() : wake());
    const evs = ['pointermove', 'pointerdown', 'keydown', 'wheel'];
    evs.forEach((e) => addEventListener(e, input, { passive: true }));
    addEventListener('blur', stop);
    addEventListener('focus', input);
    document.addEventListener('visibilitychange', vis);
    return () => {
      stop(); ro.disconnect(); off(); mq.removeEventListener('change', onMq);
      evs.forEach((e) => removeEventListener(e, input));
      removeEventListener('blur', stop); removeEventListener('focus', input);
      document.removeEventListener('visibilitychange', vis);
    };
  });

  // A new palette: draw it once (reduced motion stays on this still frame), then drift.
  $effect(() => {
    if (pal && root) {
      queueMicrotask(() => {
        w = root.clientWidth; h = root.clientHeight;
        draw();
        wake();
      });
    }
  });

  const grain = "url(\"data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' width='160' height='160'><filter id='n'><feTurbulence type='fractalNoise' baseFrequency='.8' numOctaves='2' stitchTiles='stitch'/></filter><rect width='100%' height='100%' filter='url(%23n)'/></svg>\")";
</script>

<div class="drift" bind:this={root} aria-hidden="true" style:background={pal?.base ?? 'transparent'}>
  {#if pal}
    {#each SPOTS as [, , bw, bh, ci], i}
      <i class="blob" bind:this={blobs[i]} style:width="{bw * 100}%" style:height="{bh * 100}%" style:background="radial-gradient(closest-side, {pal.cols[ci]}, transparent 68%)"></i>
    {/each}
    {#each [0, 1, 2, 3, 4] as i}
      <i class="blob band" bind:this={band[i]} style:width="{BAND.w * 100}%" style:height="{BAND.h * 100}%" style:background="radial-gradient(closest-side, {pal.cols[i % 3]}, transparent 68%)"></i>
    {/each}
    <div class="grain" style:background-image={grain}></div>
    <div class="shade"></div>
  {/if}
</div>

<style>
  .drift { position: absolute; inset: 0; overflow: hidden; pointer-events: none; z-index: 0; }
  .blob { position: absolute; left: 0; top: 0; border-radius: 50%; filter: blur(40px); will-change: transform; }
  .blob.band { opacity: .45; filter: blur(30px); }
  .grain { position: absolute; inset: 0; opacity: .05; }
  /* The overlays that keep the title and chips readable: dark on the left, fading into the page below. */
  .shade {
    position: absolute; inset: 0;
    background:
      linear-gradient(to bottom, transparent 55%, var(--bg)),
      linear-gradient(to right, #0c0c0ed9, #0c0c0e80 45%, #0c0c0e33);
  }
</style>
