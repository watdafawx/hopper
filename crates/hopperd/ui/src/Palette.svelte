<script>
  // The command palette (Ctrl+K / Ctrl+P), like Hopper desktop's
  // (src/gpui/palette.rs): "Search or run a command…", sections, matched
  // letters in the accent, recent picks first, ↑ ↓ ↵ Esc.
  // `items`: { id, section, icon, title, hint?, key?, words?, deep?, run }.
  import { onMount, tick, untrack } from 'svelte';
  import { Search, CornerDownLeft } from '@lucide/svelte';

  let { items = [], context = '', onClose } = $props();

  const ORDER = ['Recent', 'Actions', 'Navigation', 'Servers', 'Settings'];
  const CAP = { Recent: 5, Actions: 6, Navigation: 6, Servers: 8, Settings: 4 };
  const RKEY = 'hopperd.palette.recent';

  let text = $state('');
  let sel = $state(0);
  let input = $state();
  let list = $state();

  const recentIds = (() => {
    try {
      return JSON.parse(localStorage.getItem(RKEY) || '[]');
    } catch {
      return [];
    }
  })();

  function remember(id) {
    try {
      localStorage.setItem(RKEY, JSON.stringify([id, ...recentIds.filter((r) => r !== id)].slice(0, 8)));
    } catch {
      // private window: not kept
    }
  }

  /** Subsequence match: the matched positions and a score (higher is better), or null. */
  function fuzzy(q, s) {
    const lo = s.toLowerCase();
    const at = [];
    let i = 0, score = 0, last = -2;
    for (const c of q) {
      if (c === ' ') continue;
      const j = lo.indexOf(c, i);
      if (j < 0) return null;
      at.push(j);
      score += j === last + 1 ? 3 : 1;
      if (j === 0 || /[\s›·\-_]/.test(lo[j - 1])) score += 2;
      last = j;
      i = j + 1;
    }
    return { at, score: score - s.length * 0.01 };
  }

  const rows = $derived.by(() => {
    const q = text.trim().toLowerCase();
    const out = [];
    if (!q) {
      const byId = new Map(items.map((it) => [it.id, it]));
      const recent = recentIds.map((id) => byId.get(id)).filter(Boolean).slice(0, CAP.Recent);
      if (recent.length) out.push({ head: 'Recent' }, ...recent.map((it) => ({ it, at: [] })));
      for (const sec of ORDER.slice(1)) {
        // `deep` items (a server's tabs) only show once you type.
        const its = items.filter((it) => it.section === sec && !it.deep && !recent.includes(it)).slice(0, CAP[sec]);
        if (its.length) out.push({ head: sec }, ...its.map((it) => ({ it, at: [] })));
      }
      return out;
    }
    const hits = [];
    for (const it of items) {
      const m = fuzzy(q, it.title);
      const w = !m && it.words && q.split(/\s+/).every((p) => it.words.includes(p) || it.title.toLowerCase().includes(p));
      if (m || w) hits.push({ it, at: m ? m.at : [], score: m ? m.score : 0.5 });
    }
    for (const sec of ORDER.slice(1)) {
      const its = hits.filter((h) => h.it.section === sec).sort((a, b) => b.score - a.score);
      if (its.length) out.push({ head: sec }, ...its);
    }
    return out;
  });

  const picks = $derived(rows.map((r, i) => (r.it ? i : -1)).filter((i) => i >= 0));

  $effect(() => {
    // A new query starts at the first match.
    text;
    untrack(() => (sel = picks[0] ?? 0));
  });

  async function move(d) {
    if (!picks.length) return;
    const k = picks.indexOf(sel);
    sel = picks[Math.max(0, Math.min(picks.length - 1, (k < 0 ? 0 : k) + d))];
    await tick();
    list?.querySelector('.on')?.scrollIntoView({ block: 'nearest' });
  }

  function run(i) {
    const r = rows[i];
    if (!r?.it) return;
    remember(r.it.id);
    onClose();
    r.it.run();
  }

  function keys(e) {
    if (e.isComposing) return;
    if (e.key === 'ArrowDown') (e.preventDefault(), move(1));
    else if (e.key === 'ArrowUp') (e.preventDefault(), move(-1));
    else if (e.key === 'PageDown') (e.preventDefault(), move(8));
    else if (e.key === 'PageUp') (e.preventDefault(), move(-8));
    else if (e.key === 'Enter') (e.preventDefault(), run(sel));
    else if (e.key === 'Escape') (e.preventDefault(), onClose());
  }

  /** The title in pieces, matched letters marked. */
  function parts(title, at) {
    if (!at.length) return [{ t: title, m: false }];
    const set = new Set(at);
    const out = [];
    for (let i = 0; i < title.length; i++) {
      const m = set.has(i);
      if (out.length && out[out.length - 1].m === m) out[out.length - 1].t += title[i];
      else out.push({ t: title[i], m });
    }
    return out;
  }

  onMount(() => {
    // Focus goes back where it was when the palette closes, unless what it
    // ran moved it (a dialog, a page's field).
    const before = document.activeElement;
    input?.focus();
    return () => {
      if (!(before instanceof HTMLElement)) return;
      queueMicrotask(() => {
        const now = document.activeElement;
        if (before.isConnected && (!now || now === document.body)) before.focus();
      });
    };
  });
</script>

<div class="scrim" role="presentation" onpointerdown={(e) => e.target === e.currentTarget && onClose()}>
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div class="pal popover" role="dialog" aria-label="Command palette" tabindex="-1" onkeydown={keys} onpointerdown={(e) => e.target !== input && e.target.closest('.it') == null && queueMicrotask(() => input?.focus())}>
    <div class="sr">
      <Search size={16} color="var(--muted)" />
      {#if context}<span class="ctx">{context}</span>{/if}
      <input bind:this={input} bind:value={text} placeholder="Search or run a command…" spellcheck="false" autocomplete="off" />
    </div>
    <div class="rows" bind:this={list}>
      {#each rows as r, i (r.head ? `h:${r.head}` : `i:${r.it.id}`)}
        {#if r.head}
          <div class="hd">{r.head}</div>
        {:else}
          {@const Icon = r.it.icon}
          <button class="it" class:on={i === sel} tabindex="-1" onpointermove={() => (sel = i)} onclick={() => run(i)}>
            {#if Icon}<Icon size={15} />{/if}
            <span class="tt">{#each parts(r.it.title, r.at) as p, k (k)}{#if p.m}<b>{p.t}</b>{:else}{p.t}{/if}{/each}</span>
            {#if r.it.hint}<span class="hint">{r.it.hint}</span>{/if}
            {#if r.it.key}<span class="kbd">{r.it.key}</span>{/if}
            {#if i === sel}<CornerDownLeft size={12} color="var(--muted)" />{/if}
          </button>
        {/if}
      {:else}
        <div class="none">Nothing matches.</div>
      {/each}
    </div>
    <div class="ft">
      <span><span class="kbd">↑</span><span class="kbd">↓</span> navigate</span>
      <span><span class="kbd">↵</span> run</span>
      <span class="sp"></span>
      <span><span class="kbd">Esc</span> close</span>
    </div>
  </div>
</div>

<style>
  .scrim { position: fixed; inset: 0; background: #0000004d; z-index: 90; display: flex; justify-content: center; align-items: flex-start; padding: 104px 16px 16px; }
  .pal { width: min(640px, 100%); max-height: min(544px, calc(100vh - 120px)); display: flex; flex-direction: column; overflow: hidden; }
  .sr { display: flex; align-items: center; gap: 10px; height: 52px; padding: 0 16px; border-bottom: 1px solid var(--line); flex: none; }
  .sr input { flex: 1; min-width: 0; background: none; border: 0; outline: 0; font-size: 14px; color: var(--text); }
  .ctx { flex: none; max-width: 14rem; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; padding: 2px 8px; border-radius: var(--radius); background: var(--surface); color: var(--muted); font-size: 11px; }
  .rows { min-height: 0; overflow-y: auto; padding: 6px; }
  .hd { padding: 8px 10px 4px; font-size: 11px; font-weight: 500; color: var(--muted); }
  .it { display: flex; align-items: center; gap: 10px; width: 100%; height: 36px; padding: 0 10px; border-radius: var(--radius); color: var(--muted); text-align: left; }
  .it.on { background: var(--hover); color: var(--text); }
  .tt { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--text); }
  .tt b { color: var(--acc); font-weight: 600; }
  .hint { flex: none; max-width: 16rem; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 11px; color: var(--muted); }
  .none { padding: 16px; color: var(--muted); }
  .ft { display: flex; align-items: center; gap: 14px; height: 36px; padding: 0 14px; border-top: 1px solid var(--line); font-size: 11px; color: var(--muted); flex: none; }
  .ft > span { display: inline-flex; align-items: center; gap: 4px; }
</style>
