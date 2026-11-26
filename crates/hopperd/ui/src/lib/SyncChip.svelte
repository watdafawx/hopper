<script>
  // The sync chips of a server header, as Hopper desktop's sync_chip.rs: a tinted 20px chip, last
  // in the header's chip row. Click opens a small popover with the details and its actions.
  // Narrow, the label shortens, then it is an icon; two chips become one "Sync" chip listing both;
  // chips that can't fold ("Sync failed") stay. Words and fold order: lib/syncchip.js.
  import { CircleCheck, LoaderCircle, Clock, CircleOff, CircleAlert, Download, Upload, ArrowLeftRight, RotateCcw, Info } from '@lucide/svelte';
  import { untrack } from 'svelte';
  import { merged, plan, levelFor, standsAlone, aloneForm } from './syncchip.js';

  // chips: [{ look, title, line, actions: [{ label, primary, disabled, run }], progress }]. The
  // Restart and "Needs you" chips stand alone beside the sync chips ([syncchip.js] `standsAlone`).
  // A chip may bring its own popover body (`body`, a snippet; `wide` for a list) and `autoOpen`
  // keys: its popover opens by itself the first time one of them is seen in this browser tab. A chip
  // with `onclick` (a summary of the page's notice dock) has no popover: a click runs it.
  let { chips = [], room = 0 } = $props();

  const ICON = { check: CircleCheck, spin: LoaderCircle, behind: Clock, offline: CircleOff, alert: CircleAlert, down: Download, up: Upload, differ: ArrowLeftRight, restart: RotateCcw, note: Info, stuck: Clock };
  let open = $state(null);
  // `chip id|key` pairs that already opened a popover by themselves; they outlive the chip, so
  // leaving the page and coming back doesn't open it again.
  const seen = new Set();
  // Two chips with something new at once (a stuck start and a missing mod) don't open two popovers
  // over each other: the first chip's opens (the run state's comes first), the other stays a red chip.
  $effect(() => {
    let first = null;
    for (const c of chips) {
      if (!c.autoOpen?.length) continue;
      const fresh = c.autoOpen.filter((k) => !seen.has(`${c.id}|${k}`));
      for (const k of fresh) seen.add(`${c.id}|${k}`);
      if (fresh.length && first === null) first = c.id;
    }
    if (first !== null) untrack(() => (open ??= first));
  });

  const looks = $derived(chips.map((c) => c.look));
  const level = $derived(levelFor(looks, room > 0 ? room : 1e4));
  const syncs = $derived(chips.filter((c) => !standsAlone(c.look)));
  // The run state's problem sits first, right after its own pill, before the sync chips.
  const alone = $derived(chips.filter((c) => standsAlone(c.look)));
  const runs = $derived(alone.filter((c) => c.look.source === 'run'));
  const rest = $derived(alone.filter((c) => c.look.source !== 'run'));
  const p = $derived(plan(syncs.map((c) => c.look.fixed), 0, level));
  const both = $derived(syncs.length >= 2 ? merged(syncs[0].look, syncs[1].look) : null);
  // What is drawn: the merged chip (listing every section), or each chip that hasn't folded; then
  // the stand-alone ones, which fold with the sync chips.
  const aloneShown = (list) => (p.chipsFolded ? [] : list.map((c, i) => ({ look: c.look, form: aloneForm(level), sections: [c], key: c.id || `a${i}` })));
  const shown = $derived([
    ...aloneShown(runs),
    ...(p.merged
      ? [{ look: both, form: p.merged, sections: syncs, key: 'both' }]
      : syncs.map((c, i) => ({ look: c.look, form: p.forms[i], sections: [c], key: `c${i}` })).filter((x) => x.form)),
    ...aloneShown(rest),
  ]);
  const hidden = $derived(p.chipsFolded ? p.chipsFolded + alone.length : 0);
  // The popover hangs under its chip; on a narrow window the chip may sit too far right for it, so
  // it slides left to stay 12px inside the window (its width is already capped to fit).
  function keepInside(node) {
    node.style.transform = '';
    const r = node.getBoundingClientRect();
    const dx = Math.max(12 - r.left, Math.min(0, innerWidth - 12 - r.right));
    if (dx) node.style.transform = `translateX(${dx}px)`;
  }
  const text = (x) => (x.form === 'full' ? x.look.label : x.form === 'short' ? x.look.short : '');
  const tip = (x) => x.sections.map((c) => c.look.label).join(' · ');
</script>

<svelte:window onclick={() => (open = null)} onkeydown={(e) => e.key === 'Escape' && (open = null)} />

{#if chips.length}
  <span class="synchips">
    {#each shown as x (x.key)}
      {@const Icon = ICON[x.look.glyph]}
      {@const own = x.sections.length === 1 ? x.sections[0].onclick : null}
      <span class="schip-wrap">
        <button class="schip {x.look.tone}" class:icon={x.form === 'icon'} title={tip(x)} aria-haspopup={own ? undefined : 'dialog'} aria-expanded={own ? undefined : open === x.key} aria-label={tip(x)} onclick={(e) => (e.stopPropagation(), own ? (own(), (open = null)) : (open = open === x.key ? null : x.key))}>
          <span class="si" class:spin={x.look.busy}><Icon size={13} /></span>{#if text(x)}<span class="st">{text(x)}</span>{/if}
          {#if x.look.busy}<i class="sbar"></i>{/if}
        </button>
        {#if open === x.key}
          <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
          <div class="popover spop" use:keepInside class:wide={x.sections.some((c) => c.wide)} role="dialog" aria-label="Details" onclick={(e) => e.stopPropagation()}>
            {#each x.sections as c, i (i)}
              {#if i}<div class="spsep"></div>{/if}
              {#if c.body}
                {@render c.body()}
              {:else}
              <div class="spsec">
                <b>{c.title}</b>
                {#if c.line}<span class="muted small">{c.line}</span>{/if}
                {#if c.progress != null}<span class="spprog"><i style:width="{Math.round(c.progress * 100)}%"></i></span>{/if}
                {#if c.actions?.length}
                  <span class="spacts">{#each c.actions as a (a.label)}<button class="btn sm" class:go={a.primary} disabled={a.disabled} onclick={() => (a.run(), (open = null))}>{a.label}</button>{/each}</span>
                {/if}
              </div>
              {/if}
            {/each}
          </div>
        {/if}
      </span>
    {/each}
    {#if hidden}
      <span class="schip more" title={looks.map((l) => l.label).join(' · ')}>+{hidden}</span>
    {/if}
  </span>
{/if}

<style>
  .synchips { display: inline-flex; align-items: center; gap: 6px; flex: none; min-width: 0; max-width: 100%; }
  .schip-wrap { position: relative; display: inline-flex; }
  .schip { --k: var(--ink-muted); --t: color-mix(in srgb, var(--text) 8%, transparent); position: relative; overflow: hidden; display: inline-flex; align-items: center; gap: 5px; box-sizing: border-box; height: 20px; line-height: 1; padding: 0 8px 2px; border: 0; border-radius: var(--radius-sm); background: var(--t); color: var(--k); font: inherit; font-size: 11.5px; font-weight: 600; white-space: nowrap; cursor: pointer; }
  .schip.icon { padding: 0 5px 2px; }
  .schip:focus-visible { outline: 2px solid var(--acc); outline-offset: 1px; }
  .schip:hover { filter: brightness(1.12); }
  .schip.good { --k: var(--ink-green); --t: var(--green-dim); }
  .schip.warn { --k: var(--ink-gold); --t: var(--gold-dim); }
  .schip.busy { --k: var(--ink-blue); --t: var(--blue-dim); }
  .schip.bad { --k: var(--ink-red); --t: var(--red-dim); }
  .schip.more { cursor: default; padding: 0 7px 2px; }
  .si { display: inline-flex; }
  .si.spin { animation: sspin 1s linear infinite; }
  @keyframes sspin { to { transform: rotate(360deg); } }
  .sbar { position: absolute; left: 0; bottom: 0; height: 2px; width: 40%; background: currentColor; opacity: 0.7; animation: sslide 1.4s ease-in-out infinite; }
  @keyframes sslide { 0% { left: -40%; } 100% { left: 100%; } }
  @media (prefers-reduced-motion: reduce) { .si.spin, .sbar { animation: none; } }
  .spop { position: absolute; z-index: 30; top: calc(100% + 6px); left: 0; width: min(19.5rem, calc(100vw - 24px)); padding: 12px; display: flex; flex-direction: column; gap: 8px; font-size: 13px; }
  .spop.wide { width: min(32rem, calc(100vw - 24px)); }
  .spsec { display: flex; flex-direction: column; gap: 4px; }
  .spsep { border-top: 1px solid var(--line); }
  .spprog { height: 3px; border-radius: 2px; background: color-mix(in srgb, var(--text) 12%, transparent); overflow: hidden; }
  .spprog i { display: block; height: 100%; background: var(--blue); }
  .spacts { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 4px; }
</style>
