<script>
  // The one notice slot above a page's tabs, as Hopper desktop's
  // (notice_slot.rs): the two notices that matter most as one-line strips,
  // the rest under "N more" (grouped Stops play / Needs a choice / For your
  // information). Narrow (under 34rem), the line under the title goes,
  // labels shorten, the other buttons move into Details and "N more" says
  // "+N". The notices come from <Notice>s anywhere on the page.
  import { AlertTriangle, AlertCircle, Info, X, ChevronDown, ChevronUp } from '@lucide/svelte';
  import { order, fold, HEADING } from './noticeorder.js';

  let { notices } = $props();

  const ICON = { stop: AlertTriangle, choice: AlertCircle, info: Info };
  let w = $state(0);
  // Narrow at 34rem, as desktop.
  const narrow = $derived(w > 0 && w < 34 * 16);
  let open = $state({});
  let more = $state(false);

  const list = $derived.by(() => {
    const all = Object.values(notices.items).sort((a, b) => a.at - b.at);
    return order(all).map((i) => all[i]);
  });
  const folded = $derived(fold(list.map((n) => n.tier)));
  const shown = $derived(folded.shown.map((i) => list[i]));
  const rest = $derived(folded.groups.map(([tier, ix]) => [tier, ix.map((i) => list[i])]));
  const restN = $derived(list.length - shown.length);
  const dots = $derived(rest.flatMap(([t, l]) => l.map(() => t)).slice(0, narrow ? 1 : 5));

  const toggle = (id) => (open = { ...open, [id]: !open[id] });
  const tip = (n) => (n.subject ? `${n.title}. ${n.subject}` : n.title);
</script>

<svelte:window onclick={() => (more = false)} onkeydown={(e) => more && e.key === 'Escape' && (more = false)} />

{#if list.length}
  <div class="nslot" bind:clientWidth={w}>
    {#each shown as n, i (n.id)}
      {@const Icon = n.icon || ICON[n.tier]}
      {@const fit = { narrow, lead: i === 0 }}
      {@const hasMore = !!n.details || (narrow && !!n.actions)}
      <div class="nstrip {n.tier}">
        <Icon size={14} />
        <b class="nt" class:grow={narrow || !n.subject} title={tip(n)}>{n.title}</b>
        {#if n.subject && !narrow}<span class="ns" title={tip(n)}><span class="muted">·</span> {n.subject}</span>{/if}
        <span class="nacts">
          {#if n.primary}{@render n.primary(fit)}{/if}
          {#if n.actions && !narrow}{@render n.actions(fit)}{/if}
          {#if hasMore}
            <button class="btn sm ghost" class:icon={narrow} title={open[n.id] ? 'Hide details' : 'Details'} aria-expanded={!!open[n.id]} onclick={() => toggle(n.id)}>
              {#if open[n.id]}<ChevronUp size={13} />{:else}<ChevronDown size={13} />{/if}{#if !narrow}Details{/if}
            </button>
          {/if}
          {#if n.dismiss}<button class="btn sm ghost icon" title="Hide this notice" aria-label="Hide this notice" onclick={n.dismiss}><X size={13} /></button>{/if}
          {#if i === shown.length - 1 && restN}
            <span class="nmore-wrap">
              <button class="btn sm nmore" aria-haspopup="dialog" aria-expanded={more} title="{restN} more notice{restN === 1 ? '' : 's'}" onclick={(e) => (e.stopPropagation(), (more = !more))}>
                <span class="ndots">{#each dots as t, k (k)}<i class={t}></i>{/each}</span>{narrow ? `+${restN}` : `${restN} more`}
              </button>
              {#if more}
                <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                <div class="popover nlist" role="dialog" aria-label="More notices" onclick={(e) => e.stopPropagation()}>
                  <div class="nhead">{restN} more notice{restN === 1 ? '' : 's'}</div>
                  {#each rest as [tier, items] (tier + items[0].id)}
                    <div class="ngroup">{HEADING[tier]} · {items.length}</div>
                    {#each items as m (m.id)}
                      {@const rowMore = !!m.details || !!m.actions}
                      <div class="nrow">
                        <i class="ndot {m.tier}"></i>
                        <span class="nrt"><b title={tip(m)}>{m.title}</b>{#if m.subject}<span class="tiny muted" title={tip(m)}>{m.subject}</span>{/if}</span>
                        {#if m.primary}{@render m.primary({ narrow: false, lead: false })}{/if}
                        {#if rowMore}<button class="btn sm ghost icon" title={open[m.id] ? 'Hide details' : 'Details'} aria-expanded={!!open[m.id]} onclick={() => toggle(m.id)}>{#if open[m.id]}<ChevronUp size={13} />{:else}<ChevronDown size={13} />{/if}</button>{/if}
                        {#if m.dismiss}<button class="btn sm ghost icon" title="Hide this notice" aria-label="Hide this notice" onclick={m.dismiss}><X size={13} /></button>{/if}
                      </div>
                      {#if open[m.id]}
                        <div class="ndetails in-list">
                          {#if m.actions}<div class="nacts wrap">{@render m.actions({ narrow: false, lead: false })}</div>{/if}
                          {#if m.details}{@render m.details()}{/if}
                        </div>
                      {/if}
                    {/each}
                  {/each}
                </div>
              {/if}
            </span>
          {/if}
        </span>
        {#if n.progress != null}<span class="nprog" style:width="{Math.round(n.progress * 100)}%"></span>{/if}
      </div>
      {#if open[n.id] && hasMore}
        <div class="ndetails {n.tier}">
          {#if narrow && n.actions}<div class="nacts wrap">{@render n.actions(fit)}</div>{/if}
          {#if n.details}{@render n.details()}{/if}
        </div>
      {/if}
    {/each}
  </div>
{/if}
