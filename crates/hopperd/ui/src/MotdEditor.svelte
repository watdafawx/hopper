<script>
  // The server list message: colour and style buttons, variables, two line
  // boxes and a preview drawn like Minecraft's multiplayer list. The text
  // is a template: {pack_version} and the rest are filled in when the
  // server starts (and every minute while it runs, for players asking
  // through the address).
  import { tick as settled } from 'svelte';
  import { Braces } from '@lucide/svelte';
  import Avatar from './Avatar.svelte';
  import { COLORS, FORMATS, LIST_WIDTH, VARS, ampToSection, applyCode, expand, listLines, runs, visible } from './lib/motd.js';

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
  let inputs = $state([]);
  let last = $state(0);
  let menu = $state(false);
  let menuEl = $state();

  function setLine(i, text) {
    const p = [...lines];
    p[i] = text;
    value = p[1] ? `${p[0]}\n${p[1]}` : p[0];
  }

  /** Puts the caret (or selection) back after the box re-renders. */
  function select(i, a, b) {
    settled().then(() => {
      const el = inputs[i];
      el?.focus();
      el?.setSelectionRange(a, b);
    });
  }

  function code(c) {
    const el = inputs[last];
    const t = ampToSection(lines[last]);
    // Shortcuts typed so far count as codes from here on.
    const [out, a, b] = applyCode(t, el?.selectionStart ?? t.length, el?.selectionEnd ?? t.length, c);
    setLine(last, out);
    select(last, a, b);
  }

  function insertVar(name) {
    menu = false;
    const el = inputs[last];
    const t = lines[last];
    const a = el?.selectionStart ?? t.length;
    const s = `{${name}}`;
    setLine(last, t.slice(0, a) + s + t.slice(el?.selectionEnd ?? a));
    select(last, a + s.length, a + s.length);
  }

  /** Minecraft's text shadow: the colour at a quarter. */
  const shadow = (h) => '#' + [1, 3, 5].map((i) => Math.floor(parseInt(h.slice(i, i + 2), 16) / 4).toString(16).padStart(2, '0')).join('');
  const STYLE = { l: 'font-weight:700', o: 'font-style:italic', n: 'text-decoration:underline', m: 'text-decoration:line-through' };

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
</script>

<svelte:window onpointerdown={outside} />

<div class="motd">
  <div class="tools" role="toolbar" aria-label="Formatting">
    {#each COLORS as [c, name, hex] (c)}
      <button class="sw" title="{name} (§{c})" aria-label={name} style:background={hex} onmousedown={(e) => e.preventDefault()} onclick={() => code(c)}></button>
    {/each}
    <span class="sep"></span>
    {#each FORMATS as [c, name, label] (c)}
      <button class="fm" title="{name} (§{c})" aria-label={name} style={STYLE[c] || ''} onmousedown={(e) => e.preventDefault()} onclick={() => code(c)}>{label}</button>
    {/each}
    <span class="sep"></span>
    <span class="pick" bind:this={menuEl}>
      <button class="btn sm" aria-haspopup="menu" aria-expanded={menu} onmousedown={(e) => e.preventDefault()} onclick={() => (menu = !menu)}><Braces size={13} />Variable</button>
      {#if menu}
        <div class="popover menu vars" role="menu">
          {#each VARS as [k, desc] (k)}
            <button class="menu-item" role="menuitem" onmousedown={(e) => e.preventDefault()} onclick={() => insertVar(k)}>
              <span class="grow">{desc} <span class="mono tiny muted">{`{${k}}`}</span></span><span class="mono tiny val">{vars[k] ?? ''}</span>
            </button>
          {/each}
        </div>
      {/if}
    </span>
  </div>

  {#each [0, 1] as i (i)}
    {@const c = count(i)}
    <div class="ln">
      <span class="tiny muted n">Line {i + 1}</span>
      <div class="input grow"><input class="mono" bind:this={inputs[i]} value={lines[i]} oninput={(e) => setLine(i, e.currentTarget.value)} onfocus={() => (last = i)} spellcheck="false" aria-label="Message line {i + 1}" /></div>
      <span class="tiny cnt" class:near={c.near} class:over={c.over} title="Characters players see, against about how many fit (the list goes by width: narrow letters fit more)">{c.n} / {c.fit}</span>
    </div>
  {/each}
  {#if hint}<div class="tiny warn" role="status">{hint}</div>{/if}
  <div class="tiny muted">&amp; codes like &amp;6 work as a shortcut for §6. Variables fill in when the server starts; players, day and world size refresh every minute while it runs, for players using its address.</div>

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
  .sw { width: 18px; height: 18px; border-radius: 4px; border: 1px solid #ffffff26; padding: 0; }
  .sw:hover { outline: 2px solid #ffffff66; }
  .fm { width: 24px; height: 24px; border: 1px solid var(--line2); border-radius: 5px; color: var(--text); font-size: 12px; font-weight: 600; }
  .fm:hover { background: var(--hover); }
  .sep { width: 1px; height: 20px; background: var(--line2); margin: 0 3px; }
  .pick { position: relative; }
  .vars { position: absolute; top: calc(100% + 4px); left: 0; z-index: 20; width: 320px; max-height: 340px; overflow: auto; }
  .vars .val { color: var(--subtle); max-width: 120px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .ln { display: flex; gap: 8px; align-items: center; }
  .ln .n { width: 40px; flex: none; }
  .ln .input { padding: 5px 9px; }
  .grow { flex: 1; min-width: 0; }
  .cnt { width: 52px; text-align: right; color: var(--muted); flex: none; }
  .cnt.near { color: #f59e0b; }
  .cnt.over { color: #ef4444; }
  .warn { color: #f59e0b; }
  /* The server list: dirt-dark background, 64 px icon, two lines. */
  /* At 1.5× the game's GUI scale 1: the message area is LIST_WIDTH (271)
     game pixels, and a 6 px letter is one monospace cell (0.55em). */
  .mc { --s: 1.5; background: #1d150e; background-image: repeating-conic-gradient(#251b12 0 25%, #1d150e 0 50%); background-size: 32px 32px; padding: 10px; border-radius: 6px; overflow: hidden; }
  .entry { display: flex; gap: 6px; }
  .entry :global(.mcic) { width: 64px; height: 64px; flex: none; border-radius: 0; display: grid; place-items: center; font-size: 26px; font-weight: 800; color: #1a1a1f; }
  .txt { flex: none; width: calc(271px * var(--s)); font: calc(6px * var(--s) / .55)/19px Consolas, 'Courier New', monospace; }
  .top { display: flex; justify-content: space-between; color: #fff; text-shadow: 2px 2px 0 #3f3f3f; }
  .top .pl { color: #aaa; text-shadow: 2px 2px 0 #2a2a2a; }
  .mline { white-space: pre; height: 19px; }
  @media (max-width: 560px) { .mc { --s: 1; } .mline { line-height: 14px; height: 14px; } }
</style>
