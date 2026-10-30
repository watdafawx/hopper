<script>
  // The Java arguments box as a small code editor, like Hopper desktop's:
  // each flag coloured (name, value, unknown, clashing), a tooltip saying
  // what the flag under the mouse does and what's wrong with it, and
  // suggestions while typing -XX:, -X or -D (up/down, Enter or Tab, Esc).
  // A see-through textarea sits over a coloured copy of the text, so typing,
  // selection, undo and paste stay the browser's own.
  import { tokens, parts, wordAt, complete } from './lib/jvmflags.js';

  let { value = $bindable(''), spans = [], spansText = '', catalog = [], dirty = false, rows = 6, label = 'Java arguments' } = $props();

  let ta = $state(null);
  let hl = $state(null);
  let wrap = $state(null);

  // The panel's marks belong to the text it was sent; while typing runs
  // ahead, match each argument to its mark by text and how many times it
  // came before.
  const marks = $derived.by(() => {
    const byStart = new Map(), byText = new Map();
    const seen = {};
    for (const s of spans || []) {
      const t = (spansText || '').slice(s.range[0], s.range[1]);
      byStart.set(s.range[0], { ...s, t });
      const n = (seen[t] = (seen[t] || 0) + 1);
      byText.set(`${t}#${n}`, s);
    }
    return { byStart, byText, same: spansText === value };
  });

  const toks = $derived.by(() => {
    const seen = {};
    return tokens(value).map((tk) => {
      const n = (seen[tk.text] = (seen[tk.text] || 0) + 1);
      const m = marks.same ? marks.byStart.get(tk.start) : marks.byText.get(`${tk.text}#${n}`);
      const [pe, ne, vs] = parts(tk.text);
      return { ...tk, pe, ne, vs, level: m?.level || 'ok', explain: m?.explain || null, problems: m?.problems || [] };
    });
  });

  // The coloured copy: plain text between arguments, each argument split
  // into its prefix, name, = and value.
  const pieces = $derived.by(() => {
    const out = [];
    let at = 0;
    toks.forEach((tk, i) => {
      if (tk.start > at) out.push({ plain: value.slice(at, tk.start) });
      out.push({ tk, i });
      at = tk.end;
    });
    out.push({ plain: value.slice(at) + '\n' });
    return out;
  });

  let tokEls = $state([]);
  let hover = $state(null); // { i, x, y }
  let menu = $state(null); // { start, end, items, sel, x, y }

  const problems = $derived(toks.filter((t) => t.level === 'error' || t.level === 'warn').length);

  function syncScroll() {
    if (hl && ta) { hl.scrollTop = ta.scrollTop; hl.scrollLeft = ta.scrollLeft; }
  }

  function rectOf(i) {
    const el = tokEls[i];
    if (!el || !wrap) return null;
    const rs = [...el.getClientRects()];
    return rs.length ? { rs, box: wrap.getBoundingClientRect() } : null;
  }

  function onMove(e) {
    for (let i = 0; i < toks.length; i++) {
      const r = rectOf(i);
      if (!r) continue;
      const hit = r.rs.find((q) => e.clientX >= q.left && e.clientX <= q.right && e.clientY >= q.top && e.clientY <= q.bottom);
      if (hit) {
        if (hover?.i !== i) hover = { i, x: hit.left - r.box.left, y: hit.bottom - r.box.top + 4 };
        return;
      }
    }
    hover = null;
  }

  function openMenu() {
    if (!ta) return;
    const caret = ta.selectionStart;
    if (caret !== ta.selectionEnd) { menu = null; return; }
    const { start, word } = wordAt(value, caret);
    const items = complete(word, catalog).slice(0, 50);
    if (!items.length) { menu = null; return; }
    const i = toks.findIndex((t) => t.start === start);
    const r = i >= 0 ? rectOf(i) : null;
    const last = r?.rs[r.rs.length - 1];
    const x = last ? Math.min(last.left - r.box.left, r.box.width - 300) : 12;
    const y = last ? last.bottom - r.box.top + 4 : 30;
    menu = { start, end: caret, items, sel: Math.min(menu?.sel || 0, items.length - 1), x: Math.max(0, x), y };
  }

  function accept(item) {
    const { start, end } = menu;
    // Replace the whole argument under the caret, not just its typed part.
    let stop = end;
    while (stop < value.length && !/\s/.test(value[stop])) stop++;
    value = value.slice(0, start) + item.insert + value.slice(stop);
    menu = null;
    const eq = item.insert.indexOf('=');
    const xm = /^-X(mx|ms|ss)/.exec(item.insert);
    // Select the value so typing replaces it.
    const vs = eq >= 0 ? start + eq + 1 : xm ? start + 4 : start + item.insert.length;
    queueMicrotask(() => { ta.focus(); ta.setSelectionRange(vs, start + item.insert.length); });
  }

  function onKey(e) {
    if (!menu) {
      if (e.key === ' ' && e.ctrlKey) { e.preventDefault(); openMenu(); }
      return;
    }
    const n = menu.items.length;
    if (e.key === 'ArrowDown') { e.preventDefault(); menu.sel = (menu.sel + 1) % n; scrollSel(); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); menu.sel = (menu.sel + n - 1) % n; scrollSel(); }
    else if (e.key === 'Enter' || e.key === 'Tab') { e.preventDefault(); accept(menu.items[menu.sel]); }
    else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); menu = null; }
  }

  let menuEl = $state(null);
  function scrollSel() {
    queueMicrotask(() => menuEl?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: 'nearest' }));
  }

  function onInput(e) {
    hover = null;
    // Only typing opens the list; deleting closes it.
    if (e.inputType?.startsWith('insert') && e.inputType !== 'insertFromPaste') queueMicrotask(openMenu);
    else if (menu) queueMicrotask(openMenu);
  }

  const hovered = $derived(hover && toks[hover.i]);
</script>

<div class="ed" class:dirty bind:this={wrap} role="group" aria-label={label} onmouseleave={() => (hover = null)}>
  <pre class="hl mono" aria-hidden="true" bind:this={hl}>{#each pieces as p, k (k)}{#if p.plain !== undefined}{p.plain}{:else}<span class="tok {p.tk.level}" bind:this={tokEls[p.i]}><span class="pre">{p.tk.text.slice(0, p.tk.pe)}</span><span class="name">{p.tk.text.slice(p.tk.pe, p.tk.ne)}</span>{#if p.tk.vs >= 0}<span class="pre">{p.tk.text.slice(p.tk.ne, p.tk.vs)}</span><span class="val">{p.tk.text.slice(p.tk.vs)}</span>{:else}<span class="val">{p.tk.text.slice(p.tk.ne)}</span>{/if}</span>{/if}{/each}</pre>
  <textarea class="mono" spellcheck="false" {rows} bind:value bind:this={ta} aria-label={label}
    role="combobox" aria-multiline="true" aria-autocomplete="list" aria-expanded={!!menu} aria-controls="jvm-suggest"
    aria-activedescendant={menu ? `jvm-sug-${menu.sel}` : undefined}
    onscroll={syncScroll} oninput={onInput} onkeydown={onKey} onmousemove={onMove}
    onblur={() => setTimeout(() => (menu = null), 120)} onclick={() => (menu = null)}></textarea>

  {#if hovered && !menu}
    <div class="popover tip" style:left={`${hover.x}px`} style:top={`${hover.y}px`} role="tooltip">
      <div class="mono head">{hovered.text}</div>
      {#if hovered.explain}<div>{hovered.explain}</div>{/if}
      {#each hovered.problems as p (p)}<div class="prob {hovered.level}">{p}</div>{/each}
      {#if !hovered.explain && !hovered.problems.length}<div class="muted">Checking…</div>{/if}
    </div>
  {/if}

  {#if menu}
    <div class="popover sug" id="jvm-suggest" role="listbox" aria-label="Flag suggestions" bind:this={menuEl} style:left={`${menu.x}px`} style:top={`${menu.y}px`}>
      {#each menu.items as it, i (it.insert)}
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <div class="row" id={`jvm-sug-${i}`} role="option" aria-selected={i === menu.sel} tabindex="-1"
          onmousedown={(e) => { e.preventDefault(); accept(it); }} onmouseenter={() => (menu.sel = i)}>
          <span class="mono ins">{it.insert}</span><span class="det">{it.detail}</span>
        </div>
      {/each}
      <div class="foot tiny muted">↑↓ to pick · Enter or Tab to insert · Esc to close</div>
    </div>
  {/if}
</div>
{#if problems}<div class="tiny count">{problems} flag{problems === 1 ? '' : 's'} with a problem: hover {problems === 1 ? 'it' : 'them'} to see why.</div>{/if}

<style>
  .ed { position: relative; background: var(--term); border: 1px solid var(--line); border-radius: 8px; }
  .ed:focus-within { border-color: var(--acc); }
  .ed.dirty { border-color: #f9731677; }
  .hl, textarea { margin: 0; font-size: 11.5px; line-height: 1.5; padding: 10px 12px; white-space: pre-wrap; word-break: break-all; overflow-wrap: anywhere; font-family: var(--mono); letter-spacing: 0; tab-size: 4; scrollbar-gutter: stable; box-sizing: border-box; }
  .hl { position: absolute; inset: 0; overflow: hidden; color: var(--text); pointer-events: none; }
  textarea { position: relative; display: block; width: 100%; background: transparent; border: 0; color: transparent; caret-color: var(--text); resize: vertical; max-height: 520px; overflow: auto; outline: none; }
  textarea::selection { background: #38bdf855; color: transparent; }
  .tok .pre { color: var(--muted); }
  .tok .name { color: var(--blue); }
  .tok .val { color: var(--gold); }
  .tok.unknown .name, .tok.unknown .val { color: var(--text); }
  .tok.unknown { text-decoration: underline dotted var(--muted); text-underline-offset: 3px; }
  .tok.warn { text-decoration: underline wavy #eab308; text-underline-offset: 3px; }
  .tok.error { text-decoration: underline wavy var(--red); text-underline-offset: 3px; }
  .tip { position: absolute; z-index: 20; width: min(380px, 90%); padding: 8px 10px; font-size: 12.5px; line-height: 1.45; pointer-events: none; }
  .tip .head { font-weight: 600; margin-bottom: 2px; word-break: break-all; }
  .prob { margin-top: 4px; }
  .prob.error { color: #fca5a5; }
  .prob.warn { color: #fde68a; }
  .prob.unknown { color: var(--muted); }
  .sug { position: absolute; z-index: 21; width: min(520px, 96%); max-height: 260px; overflow: auto; padding: 4px; font-size: 12.5px; }
  .sug .row { display: flex; gap: 10px; align-items: baseline; padding: 4px 8px; border-radius: var(--radius); cursor: pointer; }
  .sug .row[aria-selected='true'] { background: var(--sel); }
  .ins { color: var(--blue); white-space: nowrap; flex: none; }
  .det { color: var(--muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .foot { padding: 4px 8px 2px; border-top: 1px solid var(--line); margin-top: 4px; }
  .count { color: #eab308; margin-top: 4px; }
</style>
