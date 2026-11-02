<script>
  // The server list message: colour and style buttons, variables, two line
  // boxes and a preview drawn like Minecraft's multiplayer list. The boxes
  // show the text formatted (codes hidden, variables as chips); the style
  // buttons toggle over the selection. "Codes" shows the raw § codes for
  // editing by hand. The text is a template: {pack_version} and the rest are
  // filled in when the server starts (and every minute while it runs, for
  // players asking through the address).
  import { tick as settled } from 'svelte';
  import { Braces } from '@lucide/svelte';
  import Avatar from './Avatar.svelte';
  import {
    COLORS, FORMATS, LIST_WIDTH, VARS, ampToSection, applyCode, expand, listLines, runs, visible,
    parseLine, lineText, toCodes, commonStyle, styleAt, toggle, clearStyle, withCode, editLine, varAt, has, hexOf, rawToText, textToRaw,
  } from './lib/motd.js';

  /** `value`: the template (lines split by "\n"); `vars`: name → value now. */
  let { server, value = $bindable(''), vars = {} } = $props();

  const lines = $derived.by(() => {
    const p = String(value ?? '').split('\n');
    return [p[0] ?? '', p.slice(1).join(' ')];
  });
  const filled = $derived(lines.map((l) => expand(ampToSection(l), vars)));
  // As the game draws it: word-wrapped at the list's width, two rows.
  const list = $derived(listLines(filled));
  const shown = $derived(list.shown);
  /** Characters typed against about how many fit (the list goes by
   * width, so narrow letters fit more): over is cut off, near is close. */
  const count = (i) => {
    const n = visible(filled[i]);
    const w = list.lines[i].width;
    return { n, fit: n + Math.floor((LIST_WIDTH - w) / 6), over: w > LIST_WIDTH, near: w > LIST_WIDTH - 12 };
  };
  const hint = $derived.by(() => {
    const [a, b] = list.lines;
    if (a.wraps) {
      if (filled[1]) return "Line 1 is too long for the server list: it wraps onto the second row, so line 2 doesn't show.";
      return a.cut === undefined ? 'Line 1 is too long for one row: the server list wraps it onto the second.' : `Line 1 is cut off in the server list after ${a.cut} characters.`;
    }
    return b.cut ? `Line 2 is cut off in the server list after ${b.cut} characters.` : '';
  });

  // Raw § codes instead of the formatted boxes; remembered in this browser.
  let raw = $state((() => { try { return localStorage.getItem('hopperd.motd.codes') === '1'; } catch { return false; } })());
  function setRaw(on) {
    raw = on;
    try { localStorage.setItem('hopperd.motd.codes', on ? '1' : '0'); } catch { /* private window */ }
  }

  // The formatted lines. Read again from `value` whenever it changes from
  // anywhere but these boxes (loading, the Codes boxes).
  let models = $state([[], []]);
  let mine = null;
  $effect.pre(() => {
    const v = String(value ?? '');
    if (v === mine) return;
    mine = v;
    models = lines.map((l) => parseLine(ampToSection(l)));
  });

  let inputs = $state([]);
  let last = $state(0);
  /** The selection in the last focused box: [start, end]. */
  let sel = $state([0, 0]);
  /** A style picked with nothing selected, for what's typed next at `pos`. */
  let pending = $state(null);
  let scroll = $state([0, 0]);
  let menu = $state(false);
  let menuEl = $state();

  function setLine(i, text) {
    const p = [...lines];
    p[i] = text;
    value = p[1] ? `${p[0]}\n${p[1]}` : p[0];
  }

  /** Saves formatted line `i`. */
  function commit(i, spans) {
    models[i] = spans;
    const p = [...lines];
    p[i] = toCodes(spans);
    const v = p[1] ? `${p[0]}\n${p[1]}` : p[0];
    mine = v;
    value = v;
  }

  /** Puts the caret (or selection) back after the box re-renders. */
  function select(i, a, b) {
    settled().then(() => {
      const el = inputs[i];
      if (!el) return;
      if (!raw && el.value !== lineText(models[i])) el.value = lineText(models[i]);
      el.focus();
      el.setSelectionRange(a, b);
      track(i);
    });
  }

  function track(i) {
    const el = inputs[i];
    if (!el || document.activeElement !== el) return;
    last = i;
    const s = [el.selectionStart ?? 0, el.selectionEnd ?? 0];
    if (pending && (pending.line !== i || s[0] !== pending.pos || s[1] !== pending.pos)) pending = null;
    sel = s;
    scroll[i] = el.scrollLeft;
  }

  /** What the buttons show as on: the selection's shared style (raw mode
   * reads the codes the same way). */
  const current = $derived.by(() => {
    if (pending && pending.line === last) return pending.st;
    if (raw) {
      const t = ampToSection(lines[last]);
      return commonStyle(parseLine(t), rawToText(t, sel[0]), rawToText(t, sel[1]));
    }
    return commonStyle(models[last], sel[0], sel[1]);
  });

  function code(c) {
    const i = last;
    const [a, b] = sel;
    if (raw) {
      // Shortcuts typed so far count as codes from here on.
      const t = ampToSection(lines[i]);
      if (a === b) {
        const [out, x, y] = applyCode(t, a, b, c);
        setLine(i, out);
        return select(i, x, y);
      }
      const [ta, tb] = [rawToText(t, a), rawToText(t, b)];
      const m = parseLine(t);
      const out = toCodes(c === 'r' ? clearStyle(m, ta, tb) : toggle(m, ta, tb, c));
      setLine(i, out);
      return select(i, textToRaw(out, ta, true), textToRaw(out, tb, false));
    }
    if (a === b) {
      pending = { line: i, pos: a, st: withCode(current, c) };
      return select(i, a, b);
    }
    commit(i, c === 'r' ? clearStyle(models[i], a, b) : toggle(models[i], a, b, c));
    select(i, a, b);
  }

  function typed(i, e) {
    if (raw) return setLine(i, e.currentTarget.value);
    const st = pending && pending.line === i ? pending.st : null;
    const r = editLine(models[i], e.currentTarget.value, st);
    pending = null;
    commit(i, r.spans);
    if (lineText(r.spans) !== e.currentTarget.value) select(i, r.caret, r.caret);
    else track(i);
  }

  function insertVar(name) {
    menu = false;
    const i = last;
    const s = `{${name}}`;
    const [a, b] = sel;
    if (raw) {
      const t = lines[i];
      setLine(i, t.slice(0, a) + s + t.slice(b));
      return select(i, a + s.length, a + s.length);
    }
    const t = lineText(models[i]);
    const r = editLine(models[i], t.slice(0, a) + s + t.slice(b), pending?.line === i ? pending.st : null);
    pending = null;
    commit(i, r.spans);
    select(i, r.caret, r.caret);
  }

  // The live value of the variable under the pointer.
  let tip = $state(null);
  let cell = 0;
  function hover(i, e) {
    const el = inputs[i];
    if (raw || !el) return (tip = null);
    if (!cell) {
      const c = document.createElement('canvas').getContext('2d');
      c.font = getComputedStyle(el).font;
      cell = c.measureText('0000000000').width / 10;
    }
    const pad = parseFloat(getComputedStyle(el).paddingLeft);
    const pos = Math.floor((e.offsetX - pad + el.scrollLeft) / cell);
    const v = varAt(models[i], pos);
    if (!v) return (tip = null);
    const desc = VARS.find(([k]) => k === v.name)?.[1];
    tip = { line: i, x: pad + v.from * cell - el.scrollLeft, desc: desc ?? 'Not a variable Hopper knows: shown as typed', value: desc ? vars[v.name] ?? '' : null };
  }

  /** Minecraft's text shadow: the colour at a quarter. */
  const shadow = (h) => '#' + [1, 3, 5].map((i) => Math.floor(parseInt(h.slice(i, i + 2), 16) / 4).toString(16).padStart(2, '0')).join('');
  const STYLE = { l: 'font-weight:700', o: 'font-style:italic', n: 'text-decoration:underline', m: 'text-decoration:line-through' };
  const look = (st) => {
    const h = hexOf(st);
    const deco = [st.n && 'underline', st.m && 'line-through'].filter(Boolean).join(' ');
    return `color:${h};text-shadow:1px 1px 0 ${shadow(h)};${st.l ? STYLE.l : ''};${st.o ? STYLE.o : ''};${deco ? `text-decoration:${deco}` : ''}`;
  };

  // Obfuscated text scrambles, as in the game.
  let tick = $state(0);
  $effect(() => {
    if (!shown.some((l) => /§k/i.test(l))) return;
    const t = setInterval(() => tick++, 90);
    return () => clearInterval(t);
  });
  const scramble = (s) => (tick, s.replace(/\S/g, () => String.fromCharCode(33 + Math.floor(Math.random() * 90))));

  function outside(e) {
    if (menu && menuEl && !menuEl.contains(e.target)) menu = false;
  }
  const players = $derived(['running', 'starting'].includes(server.state) ? server.players?.length ?? 0 : 0);
  const keep = (e) => e.preventDefault();
</script>

<svelte:window onpointerdown={outside} />
<svelte:document onselectionchange={() => { const i = inputs.indexOf(document.activeElement); if (i >= 0) track(i); }} />

<div class="motd">
  <div class="tools" role="toolbar" aria-label="Formatting">
    {#each COLORS as [c, name, hex] (c)}
      <button class="sw" class:on={current.color === c} title="{name} (§{c})" aria-label={name} aria-pressed={current.color === c} style:background={hex} onmousedown={keep} onclick={() => code(c)}></button>
    {/each}
    <span class="sep"></span>
    {#each FORMATS as [c, name, label] (c)}
      {@const on = c !== 'r' && has(current, c)}
      <button class="fm" class:on title="{name} (§{c})" aria-label={name} aria-pressed={c === 'r' ? undefined : on} style={STYLE[c] || ''} onmousedown={keep} onclick={() => code(c)}>{label}</button>
    {/each}
    <span class="sep"></span>
    <span class="pick" bind:this={menuEl}>
      <button class="btn sm" aria-haspopup="menu" aria-expanded={menu} onmousedown={keep} onclick={() => (menu = !menu)}><Braces size={13} />Variable</button>
      {#if menu}
        <div class="popover menu vars" role="menu">
          {#each VARS as [k, desc] (k)}
            <button class="menu-item" role="menuitem" onmousedown={keep} onclick={() => insertVar(k)}>
              <span class="grow">{desc} <span class="mono tiny muted">{`{${k}}`}</span></span><span class="mono tiny val">{vars[k] ?? ''}</span>
            </button>
          {/each}
        </div>
      {/if}
    </span>
    <label class="codes tiny muted" title="Show and edit the § codes by hand">
      <span class="switch"><input type="checkbox" checked={raw} onchange={(e) => setRaw(e.currentTarget.checked)} /><span></span></span>Codes
    </label>
  </div>

  {#each [0, 1] as i (i)}
    {@const c = count(i)}
    <div class="ln">
      <span class="tiny muted n">Line {i + 1}</span>
      {#if raw}
        <div class="input grow"><input class="mono" bind:this={inputs[i]} value={lines[i]} oninput={(e) => setLine(i, e.currentTarget.value)} onfocus={() => track(i)} spellcheck="false" aria-label="Message line {i + 1}, with codes" /></div>
      {:else}
        <div class="rich grow" class:pend={pending?.line === i}>
          <div class="mirror" aria-hidden="true" style:transform="translateX({-scroll[i]}px)">{#each models[i] as s, j (j)}{#if s.v}<span class="vchip" style={look(s.st)}><span class="br">{'{'}</span>{s.text.slice(1, -1)}<span class="br">{'}'}</span></span>{:else}<span class:obf={s.st.k} style={look(s.st)}>{s.text}</span>{/if}{/each}</div>
          <input bind:this={inputs[i]} value={lineText(models[i])} oninput={(e) => typed(i, e)} onfocus={() => track(i)} onscroll={() => track(i)} onpointermove={(e) => hover(i, e)} onpointerleave={() => (tip = null)} spellcheck="false" autocomplete="off" aria-label="Message line {i + 1}" />
          {#if tip?.line === i}
            <div class="tip tiny" style:left="{Math.max(0, tip.x)}px">{tip.desc}{#if tip.value !== null}: <b>{tip.value || 'not known yet'}</b>{/if}</div>
          {/if}
        </div>
      {/if}
      <span class="tiny cnt" class:near={c.near} class:over={c.over} title="Characters players see, against about how many fit (the list goes by width: narrow letters fit more)">{c.n} / {c.fit}</span>
    </div>
  {/each}
  {#if hint}<div class="tiny warn" role="status">{hint}</div>{/if}
  <div class="tiny muted">
    {#if raw}&amp; codes like &amp;6 work as a shortcut for §6.{:else}Select text and click a style to turn it on or off. Type a variable like {'{day}'} or pick one from Variable.{/if}
    Variables fill in when the server starts; players, day and world size refresh every minute while it runs, for players using its address.
  </div>

  <div class="mc" aria-label="Preview in the server list">
    <div class="entry">
      <Avatar {server} cls="mcic" />
      <div class="txt">
        <div class="top"><span>{server.name}</span><span class="pl">{players}/{vars.max_players ?? server.max_players}</span></div>
        {#each shown as l, i (i)}
          <div class="mline">{#each runs(l) as r, j (j)}<span style="color:{r.color};text-shadow:2px 2px 0 {shadow(r.color)};{r.l ? STYLE.l : ''};{r.o ? STYLE.o : ''};{r.n || r.m ? `text-decoration:${r.n ? 'underline' : ''} ${r.m ? 'line-through' : ''}` : ''}">{r.k ? scramble(r.text) : r.text}</span>{/each}</div>
        {/each}
      </div>
    </div>
  </div>
</div>

<style>
  .motd { display: flex; flex-direction: column; gap: 8px; padding: 2px 0 4px; }
  .tools { display: flex; flex-wrap: wrap; gap: 5px; align-items: center; }
  .sw { width: 18px; height: 18px; border-radius: var(--radius-sm); border: 1px solid #ffffff26; padding: 0; }
  .sw:hover { outline: 2px solid #ffffff66; }
  .sw.on { outline: 2px solid var(--text); outline-offset: 1px; }
  .fm { width: 24px; height: 24px; border: 1px solid var(--line2); border-radius: var(--radius); color: var(--text); font-size: 12px; font-weight: 600; }
  .fm:hover { background: var(--hover); }
  .fm.on { background: color-mix(in srgb, var(--acc) 22%, transparent); border-color: var(--acc); }
  .sep { width: 1px; height: 20px; background: var(--line2); margin: 0 3px; }
  .pick { position: relative; }
  .vars { position: absolute; top: calc(100% + 4px); left: 0; z-index: 20; width: 320px; max-height: 340px; overflow: auto; }
  .vars .val { color: var(--subtle); max-width: 120px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .codes { margin-left: auto; display: flex; align-items: center; gap: 6px; cursor: pointer; }
  .ln { display: flex; gap: 8px; align-items: center; }
  .ln .n { width: 40px; flex: none; }
  .ln .input { padding: 5px 9px; }
  .grow { flex: 1; min-width: 0; }
  .cnt { width: 52px; text-align: right; color: var(--muted); flex: none; }
  .cnt.near { color: #f59e0b; }
  .cnt.over { color: #ef4444; }
  .warn { color: #f59e0b; }
  /* The formatted box: the real input on top with see-through letters (its
     caret and selection show), the formatted copy underneath. Same
     monospace font in both so every letter lines up, bold and italic too. */
  .rich { position: relative; overflow: hidden; background: #17120d; border: 1px solid var(--input-line); border-radius: var(--radius); height: 32px; }
  .rich:focus-within { border-color: var(--acc); }
  .rich.pend { border-style: dashed; }
  .rich input, .mirror { font: 14px/30px Consolas, 'Courier New', monospace; letter-spacing: 0; padding: 0 9px; white-space: pre; }
  .rich input { position: relative; z-index: 1; width: 100%; height: 100%; box-sizing: border-box; background: none; border: 0; outline: 0; box-shadow: none; color: transparent; caret-color: #fff; }
  .rich input::selection { background: #3b82f666; color: transparent; }
  .mirror { position: absolute; left: 0; top: 0; pointer-events: none; }
  .vchip { background: #3b82f633; border-radius: 3px; box-shadow: inset 0 -1px 0 #3b82f699; }
  .vchip .br { opacity: .5; }
  /* Scrambled in the game; here the letters stay readable under a moving pattern. */
  .obf { background: repeating-linear-gradient(135deg, #ffffff22 0 2px, transparent 2px 5px); background-size: 14px 14px; animation: obf 1.2s linear infinite; }
  @keyframes obf { to { background-position: 14px 0; } }
  @media (prefers-reduced-motion: reduce) { .obf { animation: none; } }
  .tip { position: absolute; top: 34px; z-index: 30; white-space: nowrap; background: var(--surface); border: 1px solid var(--line2); border-radius: var(--radius); padding: 3px 8px; color: var(--text); pointer-events: none; }
  .rich:has(.tip) { overflow: visible; }
  /* The server list: dirt-dark background, 64 px icon, two lines. */
  /* At 1.5× the game's GUI scale 1: the message area is LIST_WIDTH (271)
     game pixels, and a 6 px letter is one monospace cell (0.55em). */
  .mc { --s: 1.5; background: #1d150e; background-image: repeating-conic-gradient(#251b12 0 25%, #1d150e 0 50%); background-size: 32px 32px; padding: 10px; border-radius: var(--radius); overflow: hidden; }
  .entry { display: flex; gap: 6px; }
  .entry :global(.mcic) { width: 64px; height: 64px; flex: none; border-radius: 0; display: grid; place-items: center; font-size: 26px; font-weight: 800; color: #1a1a1f; }
  .txt { flex: none; width: calc(271px * var(--s)); font: calc(6px * var(--s) / .55)/19px Consolas, 'Courier New', monospace; }
  .top { display: flex; justify-content: space-between; color: #fff; text-shadow: 2px 2px 0 #3f3f3f; }
  .top .pl { color: #aaa; text-shadow: 2px 2px 0 #2a2a2a; }
  .mline { white-space: pre; height: 19px; }
  @media (max-width: 560px) { .mc { --s: 1; } .mline { line-height: 14px; height: 14px; } }
</style>
