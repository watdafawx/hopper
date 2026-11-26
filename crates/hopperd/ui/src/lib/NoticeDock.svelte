<script>
  // The page's notices docked at the bottom edge of its content, under the scrolling list, as
  // desktop's notice_slot.rs `dock`: a strip each (red bar: stops play, amber: the rest), what stops
  // play first. Nothing above it moves when a notice comes or goes; only the list's bottom edge gets
  // shorter. A strip says its title and main button on one line; its other buttons and details open
  // under it (the dock grows upward). A problem that stops play opens its details by itself once.
  // The hero's chips flash a strip (`flash` in notices.svelte.js). Narrow, the sentence keeps its
  // width and the button wraps under it.
  import { ChevronDown, ChevronUp, X } from '@lucide/svelte';
  import { docked, flashed, stopSeen } from './notices.svelte.js';

  // `key` names the page (the server), the same key its chips flash.
  let { notices, key } = $props();

  let open = $state({});
  let width = $state(0);
  const narrow = $derived(width > 0 && width < 640);
  let box = $state(null);
  const list = $derived(docked(notices));
  // The region's one filled button: the first strip's main action.
  const lead = $derived(list.findIndex((n) => !!n.primary));
  const toggle = (id) => (open = { ...open, [id]: !open[id] });
  const tip = (n) => (n.subject ? `${n.title}. ${n.subject}` : n.title);
  const more = (n) => !!n.details || !!n.actions;

  // Every problem that stops play opens its details once (a new report, its `since`, once more).
  $effect(() => {
    const fresh = list.filter((n) => n.tier === 'stop' && more(n) && !stopSeen.has(`${key}|${n.id}@${n.since}`));
    if (!fresh.length) return;
    for (const n of fresh) stopSeen.add(`${key}|${n.id}@${n.since}`);
    open = { ...open, ...Object.fromEntries(fresh.map((n) => [n.id, true])) };
  });

  // A chip or a row marker asked for a strip: bring it into view and flash it.
  let done = 0;
  $effect(() => {
    const ask = flashed.n && flashed.key === key && flashed.n !== done ? { ...flashed } : null;
    if (!ask || !box) return;
    done = ask.n;
    const target = list.find((n) => n.id === ask.id) || list[0];
    if (!target) return;
    if (ask.open && more(target)) open = { ...open, [target.id]: true };
    const el = box.querySelector(`[data-notice="${CSS.escape(target.id)}"]`);
    if (!el) return;
    el.scrollIntoView({ block: 'nearest' });
    const tone = getComputedStyle(el).getPropertyValue('--tone').trim() || 'orange';
    el.animate([{ background: 'var(--surface)' }, { background: `color-mix(in srgb, ${tone} 35%, var(--surface))` }, { background: 'var(--surface)' }], { duration: 1400, easing: 'ease-in-out' });
  });
</script>

{#if list.length}
  <div class="ndock" bind:this={box} bind:clientWidth={width} role="region" aria-label="Notices">
    {#each list as n, i (n.id)}
      {@const fit = { narrow, lead: i === lead }}
      <div class="nstrip {n.tier}" data-notice={n.id}>
        <div class="nshead">
          <div class="nsmain">
            <span class="nsline" title={tip(n)}><b>{n.title}</b>{#if n.subject && !narrow}<span class="muted">{n.subject}</span>{/if}</span>
            {#if n.primary}<span class="nacts">{@render n.primary(fit)}</span>{/if}
          </div>
          {#if more(n)}<button class="btn sm ghost icon" title={open[n.id] ? 'Hide details' : 'Details'} aria-expanded={!!open[n.id]} onclick={() => toggle(n.id)}>{#if open[n.id]}<ChevronDown size={13} />{:else}<ChevronUp size={13} />{/if}</button>{/if}
          {#if n.dismiss}<button class="btn sm ghost icon" title="Hide this notice" aria-label="Hide this notice" onclick={n.dismiss}><X size={13} /></button>{/if}
        </div>
        {#if open[n.id] && more(n)}
          <div class="nsdetails">
            {#if n.actions}<div class="nacts wrap">{@render n.actions({ narrow, lead: false })}</div>{/if}
            {#if n.details}{@render n.details()}{/if}
          </div>
        {/if}
      </div>
    {/each}
  </div>
{/if}

<style>
  .ndock { flex: none; max-height: 45vh; overflow: auto; border-top: 1px solid var(--line); background: var(--bg); padding: 8px 12px; display: flex; flex-direction: column; gap: 6px; }
  .nstrip { --tone: var(--gold); position: relative; border-radius: var(--radius); background: var(--surface); padding: 4px 6px 4px 14px; display: flex; flex-direction: column; gap: 8px; overflow: hidden; }
  .nstrip::before { content: ''; position: absolute; left: 0; top: 0; bottom: 0; width: 3px; background: var(--tone); }
  .nstrip.stop { --tone: var(--red); }
  .nshead { display: flex; align-items: flex-start; gap: 4px; }
  .nshead > .btn { flex: none; margin-top: 2px; }
  .nsmain { flex: 1; min-width: 0; display: flex; flex-wrap: wrap; align-items: center; gap: 4px 12px; }
  .nsline { flex: 1; min-width: 12rem; height: 28px; display: flex; align-items: center; gap: 6px; white-space: nowrap; overflow: hidden; font-size: 13px; }
  .nsline > * { overflow: hidden; text-overflow: ellipsis; min-width: 0; }
  .nsline > .muted { flex: 1; }
  .nsdetails { display: flex; flex-direction: column; gap: 8px; padding: 0 6px 4px 0; font-size: 12.5px; }
</style>
