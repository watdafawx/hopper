// Drifting field of letters behind the page: soft patches of random
// characters that slowly move and shimmer as letters swap. A click sends a
// ripple (a random shape) through it that lights the letters up; the pointer
// brightens what's under it. Pauses in background tabs, holds still for
// reduced motion.
(() => {
  const canvas = document.getElementById('ocean');
  if (!canvas) return;
  const ctx = canvas.getContext('2d', { alpha: true });
  const still = matchMedia('(prefers-reduced-motion: reduce)').matches;

  const CHARS = 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789';
  const CELL_H = 16, FONT = 11, GAP = 3; // GAP: extra px between letters
  const FONT_CSS = `500 ${FONT}px "IBM Plex Mono", ui-monospace, Consolas, monospace`;
  // Brightness bands for the letters, then two for ripples.
  const INK = [.035, .06, .09, .13, .18].map(a => `rgba(200,205,215,${a})`);
  const GLOW = ['rgba(249,115,22,.45)', 'rgba(253,170,100,.75)'];

  let cols = 0, rows = 0, dpr = 1, cellW = 9;
  let grid = new Uint8Array(0); // which letter sits in each cell
  // Retain unchanged cells. Positions use measured space advances so drawing
  // one glyph matches its position in the original full-row text exactly.
  let painted = new Uint8Array(0), letters = new Uint8Array(0), dirty = new Uint16Array(0), advances = [];
  const ripples = [];
  let px = -1e4, py = -1e4, pAmp = 0;

  // ---- noise: smooth value noise, a few octaves ---------------------------
  const P = new Uint8Array(512);
  {
    const p = [...Array(256).keys()];
    for (let i = 255; i > 0; i--) { const j = (Math.random() * (i + 1)) | 0; [p[i], p[j]] = [p[j], p[i]]; }
    for (let i = 0; i < 512; i++) P[i] = p[i & 255];
  }
  const fade = t => t * t * (3 - 2 * t);
  function vnoise(x, y) {
    const xi = Math.floor(x), yi = Math.floor(y);
    const xf = x - xi, yf = y - yi;
    const X = xi & 255, Y = yi & 255;
    const a = P[P[X] + Y], b = P[P[X + 1] + Y], c = P[P[X] + Y + 1], d = P[P[X + 1] + Y + 1];
    const u = fade(xf), v = fade(yf);
    return ((a + (b - a) * u) * (1 - v) + (c + (d - c) * u) * v) / 255;
  }
  function fbm(x, y) {
    return vnoise(x, y) * 0.55 + vnoise(x * 2.1 + 7.3, y * 2.1 - 3.1) * 0.3 + vnoise(x * 4.3 - 1.7, y * 4.3 + 9.2) * 0.15;
  }
  const smooth = (a, b, x) => { const t = Math.max(0, Math.min(1, (x - a) / (b - a))); return t * t * (3 - 2 * t); };

  // ---- ripple shapes --------------------------------------------------------
  // Each is a "distance" whose level sets are the shape, so a front at
  // distance R draws that shape at size R.
  const TAU = Math.PI * 2;
  function polygon(u, v, n) {
    const seg = TAU / n;
    const a = ((Math.atan2(v, u) % seg) + seg) % seg - seg / 2;
    return Math.hypot(u, v) * Math.cos(seg / 2) / Math.cos(a);
  }
  const SHAPES = [
    (u, v) => Math.hypot(u, v),                                                       // circle
    (u, v) => (Math.abs(u) + Math.abs(v)) * 0.78,                                     // diamond
    (u, v) => Math.max(Math.abs(u), Math.abs(v)),                                     // square
    (u, v, k) => polygon(u, v, k.n),                                                  // 3–7 sides
    (u, v, k) => Math.hypot(u, v) / (1 + 0.32 * Math.cos(k.n * Math.atan2(v, u))),     // star
    (u, v, k) => Math.hypot(u, v) / (1 + 0.14 * Math.sin(k.n * Math.atan2(v, u))),     // flower
  ];
  function shapeDist(rp, dx, dy) {
    const u = (dx * rp.cos + dy * rp.sin) / rp.sx;
    const v = (dy * rp.cos - dx * rp.sin) / rp.sy;
    return rp.shape(u, v, rp);
  }

  function resize() {
    dpr = Math.min(devicePixelRatio || 1, 2);
    const w = document.documentElement.clientWidth, h = innerHeight;
    canvas.width = Math.ceil(w * dpr);
    canvas.height = Math.ceil(h * dpr);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.font = FONT_CSS;
    ctx.textBaseline = 'top';
    // Keep the original row text's font advance and optional letter spacing.
    const spaced = 'letterSpacing' in ctx;
    if (spaced) ctx.letterSpacing = GAP + 'px';
    cellW = ctx.measureText('0123456789').width / 10 || 9;
    cols = Math.ceil(w / cellW) + 1;
    rows = Math.ceil(h / CELL_H) + 1;
    advances = Array.from({ length: cols + 1 }, (_, c) => ctx.measureText(' '.repeat(c)).width);
    grid = new Uint8Array(cols * rows);
    painted = new Uint8Array(cols * rows).fill(255);
    letters = new Uint8Array(cols * rows);
    dirty = new Uint16Array(cols);
    for (let i = 0; i < grid.length; i++) grid[i] = (Math.random() * CHARS.length) | 0;
    if (still) frame(4000);
  }

  function frame(ms) {
    const t = ms / 1000;
    const scroll = scrollY * 0.25; // slow parallax, in px
    for (let i = ripples.length - 1; i >= 0; i--) {
      const rp = ripples[i], age = t - rp.t;
      if (age > 3.4) { ripples.splice(i, 1); continue; }
      rp.front = age * 300;
      rp.fade = Math.max(0, 1 - age / 3.2) * rp.a;
      rp.reach = (rp.front + 30) * rp.extent + 1;
    }
    pAmp *= 0.93;

    // A few letters change every frame: the shimmer.
    if (!still) for (let k = (grid.length * 0.015) | 0; k > 0; k--) {
      grid[(Math.random() * grid.length) | 0] = (Math.random() * CHARS.length) | 0;
    }

    const drift = t * 0.035;
    for (let r = 0; r < rows; r++) {
      const cy = r * CELL_H;
      let changed = 0;
      for (let c = 0; c < cols; c++) {
        const cx = c * cellW;
        // Patches: noise in page space, drifting slowly, cut with a soft edge.
        const i = r * cols + c;
        const n = fbm(cx * 0.0056 + drift, (cy + scroll) * 0.0074 - drift * 0.6);
        let lit = smooth(0.48, 0.78, n);

        let glow = 0;
        for (const rp of ripples) {
          if (!rp.fade) continue;
          const dx = cx - rp.x, dy = cy - rp.y;
          if (Math.abs(dx) > rp.reach || Math.abs(dy) > rp.reach) continue;
          const off = shapeDist(rp, dx, dy) - rp.front;
          if (off > 30 || off < -90) continue;
          glow += Math.exp(-(off * off) / 700) * rp.fade;
        }
        if (pAmp > 0.01) {
          const d = Math.hypot(cx - px, cy - py);
          if (d < 130) lit += (1 - d / 130) * 0.5 * pAmp;
        }

        let band;
        if (glow > 0.3) band = INK.length + (glow > 0.7 ? 1 : 0);
        else {
          lit += glow;
          band = lit < 0.06 ? 255 : Math.min(INK.length - 1, (lit * INK.length) | 0);
        }
        if (painted[i] === band && (band === 255 || letters[i] === grid[i])) continue;
        painted[i] = band;
        letters[i] = grid[i];
        dirty[changed++] = c;
      }
      if (!changed) continue;
      const top = Math.floor(cy * dpr), height = (Math.floor((cy + CELL_H) * dpr) - top) / dpr;
      // A broad ripple can change most of a row: batching is cheaper then.
      if (changed > cols / 8) {
        ctx.clearRect(0, top / dpr, canvas.width, height);
        for (let band = 0; band < INK.length + GLOW.length; band++) {
          let text = '';
          for (let c = 0; c < cols; c++) {
            const i = r * cols + c;
            text += painted[i] === band ? CHARS[letters[i]] : ' ';
          }
          text = text.trimEnd();
          if (!text) continue;
          ctx.fillStyle = band < INK.length ? INK[band] : GLOW[band - INK.length];
          ctx.fillText(text, 0, cy);
        }
        continue;
      }
      for (let k = 0; k < changed; k++) {
        const c = dirty[k], i = r * cols + c, band = painted[i];
        // Shared pixel boundaries prevent a clear from erasing its neighbor.
        const left = Math.floor(advances[c] * dpr);
        ctx.clearRect(left / dpr, top / dpr,
          (Math.floor(advances[c + 1] * dpr) - left) / dpr, height);
        if (band !== 255) {
          ctx.fillStyle = band < INK.length ? INK[band] : GLOW[band - INK.length];
          ctx.fillText(CHARS[grid[i]], advances[c], cy);
        }
      }
    }
  }

  // While the page scrolls (and a moment after) the field keeps moving but
  // redraws at ~20 fps instead of ~30, so it never freezes and then jumps to
  // the new parallax position when the scrolling stops. On a slow machine the
  // gap grows with what a frame costs, so scrolling stays smooth.
  let calmUntil = 0;
  addEventListener('scroll', () => { calmUntil = performance.now() + 200; }, { passive: true });

  let last = 0, raf = 0, cost = 0;
  function loop(ms) {
    raf = requestAnimationFrame(loop);
    const scrolling = ms < calmUntil;
    if (ms - last < (scrolling ? Math.max(50, cost * 5) : 33)) return; // ~20 fps while scrolling, else ~30
    last = ms;
    const began = performance.now();
    frame(ms);
    cost = performance.now() - began;
  }

  function splash(x, y) {
    const rot = Math.random() * TAU;
    // Stretch some of them: ellipses, long diamonds, squashed stars.
    const stretch = Math.random() < 0.4 ? 1 + Math.random() * 0.9 : 1;
    ripples.push({
      x, y, t: performance.now() / 1000, a: 1,
      shape: SHAPES[(Math.random() * SHAPES.length) | 0],
      n: 3 + ((Math.random() * 5) | 0),
      cos: Math.cos(rot), sin: Math.sin(rot),
      sx: stretch, sy: 1 / Math.sqrt(stretch),
    });
    const rp = ripples[ripples.length - 1];
    // Conservative shape bounds skip cells the wave cannot reach. The extra
    // pixel in frame() keeps floating-point boundaries inside the old test.
    const radius = [1, 1 / 0.78, Math.SQRT2, 1 / Math.cos(Math.PI / rp.n), 1.32, 1.14];
    rp.extent = radius[SHAPES.indexOf(rp.shape)] * Math.max(rp.sx, rp.sy);
    if (ripples.length > 8) ripples.shift();
    if (still) frame(performance.now());
  }

  addEventListener('resize', resize);
  addEventListener('pointerdown', e => { if (e.button === 0) splash(e.clientX, e.clientY); }, { passive: true });
  addEventListener('pointermove', e => {
    px = e.clientX; py = e.clientY; pAmp = Math.min(1, pAmp + 0.05);
  }, { passive: true });
  document.addEventListener('visibilitychange', () => {
    if (still) return;
    if (document.hidden) cancelAnimationFrame(raf);
    else raf = requestAnimationFrame(loop);
  });

  const start = () => { resize(); if (!still) raf = requestAnimationFrame(loop); };
  // Load the mono font before measuring it; a fallback has another width.
  (document.fonts ? document.fonts.load(FONT_CSS).catch(() => {}) : Promise.resolve()).then(start);
})();
