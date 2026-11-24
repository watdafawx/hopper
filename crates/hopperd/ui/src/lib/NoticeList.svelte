<script>
  // The "Needs you" chip's popover: every notice the page has, grouped Stops play / Needs a choice /
  // For your information, a row each (as desktop's notice_slot.rs list). The notices come from
  // <Notice>s anywhere on the page. Narrow, the row's other buttons move into its details.
  import { ChevronDown, ChevronUp, X } from '@lucide/svelte';
  import { HEADING, group } from './noticeorder.js';
  import { ordered } from './notices.svelte.js';

  let { notices } = $props();

  let open = $state({});
  const list = $derived(ordered(notices));
  const groups = $derived(group(list.map((n) => n.tier)).map(([tier, ix]) => [tier, ix.map((i) => list[i])]));
  const infoOnly = $derived(list.length > 0 && list.every((n) => n.tier === 'info'));
  const toggle = (id) => (open = { ...open, [id]: !open[id] });
  const tip = (n) => (n.subject ? `${n.title}. ${n.subject}` : n.title);
  const fit = { narrow: false, lead: false };
</script>

<div class="nlist" role="list">
  <div class="nhead">{infoOnly ? `${list.length} note${list.length === 1 ? '' : 's'}` : 'Needs you'}</div>
  {#each groups as [tier, items] (tier)}
    <div class="ngroup">{HEADING[tier]} · {items.length}</div>
    {#each items as m (m.id)}
      {@const rowMore = !!m.details || !!m.actions}
      <div class="nrow" role="listitem">
        <i class="ndot {m.tier}"></i>
        <span class="nrt"><b title={tip(m)}>{m.title}</b>{#if m.subject}<span class="tiny muted" title={tip(m)}>{m.subject}</span>{/if}</span>
        {#if m.primary}{@render m.primary(fit)}{/if}
        {#if rowMore}<button class="btn sm ghost icon" title={open[m.id] ? 'Hide details' : 'Details'} aria-expanded={!!open[m.id]} onclick={() => toggle(m.id)}>{#if open[m.id]}<ChevronUp size={13} />{:else}<ChevronDown size={13} />{/if}</button>{/if}
        {#if m.dismiss}<button class="btn sm ghost icon" title="Hide this notice" aria-label="Hide this notice" onclick={m.dismiss}><X size={13} /></button>{/if}
      </div>
      {#if open[m.id]}
        <div class="ndetails in-list {m.tier}">
          {#if m.actions}<div class="nacts wrap">{@render m.actions(fit)}</div>{/if}
          {#if m.details}{@render m.details()}{/if}
        </div>
      {/if}
    {/each}
  {/each}
</div>
