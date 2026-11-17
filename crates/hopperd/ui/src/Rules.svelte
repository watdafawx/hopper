<script>
  // One page for what the panel tells you and does on its own: Messages
  // (webhooks), Warnings (alerts) and Actions (automations). Each part keeps
  // its own card; this page adds the filter, the search and the Add buttons.
  import { onMount } from 'svelte';
  import { Plus, Search, X, AlertTriangle } from '@lucide/svelte';
  import { onChange } from './lib/changes.svelte.js';
  import { alerts, refreshAlerts } from './lib/alerts.svelte.js';
  import { live } from './lib/live.svelte.js';
  import { rightClear } from './lib/rightclear.js';
  import { FILTERS, showPart, counts, failing, warningsMatch } from './lib/rulesPage.js';
  import Webhooks from './Webhooks.svelte';
  import Alerts from './Alerts.svelte';
  import Automations from './Automations.svelte';

  // `start`: the part an old link (Settings › Alerts) opens on.
  let { start = 'all' } = $props();
  let filter = $state(start);
  let query = $state('');
  let hooks = $state([]);
  let rules = $state([]);
  let hook = $state();
  let act = $state();

  const hasNodes = $derived((live.nodes || []).length > 0);
  const n = $derived(counts(hooks, alerts.settings, rules, hasNodes));
  const bad = $derived(failing(hooks, rules));
  const warnShown = $derived(showPart(filter, 'warnings') && warningsMatch(query, hasNodes));

  async function load() {
    // The Messages and Actions cards load their own lists (bound below).
    refreshAlerts();
  }

  // Add opens the form in its part, so show that part first.
  async function add(part) {
    filter = part;
    query = '';
    await Promise.resolve();
    (part === 'messages' ? hook : act)?.add();
  }

  onMount(load);
  // Changed elsewhere (another tab, Hopper desktop, on disk): show it.
  $effect(() => onChange(null, ['panel'], load));
  // A different old link while the page is open.
  $effect(() => {
    filter = start;
  });
</script>

<div class="rbar">
  <div class="seg icons" role="group" aria-label="Show">
    {#each FILTERS as [k, label] (k)}
      <button class="si" aria-pressed={filter === k} onclick={() => (filter = k)}>{label}<span class="ct">{n[k]}</span></button>
    {/each}
  </div>
  <div class="input find"><Search size={13} /><input use:rightClear placeholder="Search" bind:value={query} spellcheck="false" />{#if query}<button class="clr" title="Clear" onclick={() => (query = '')}><X size={12} /></button>{/if}</div>
  <span class="sp"></span>
  <button class="btn sm" onclick={() => add('messages')}><Plus size={13} />Message</button>
  <button class="btn sm pri" onclick={() => add('actions')}><Plus size={13} />Action</button>
</div>
{#if bad}<p class="tiny warn"><AlertTriangle size={12} />{bad === 1 ? '1 rule needs a look' : `${bad} rules need a look`}: it failed the last time it ran.</p>{/if}

<div class:hide={!showPart(filter, 'messages')}><Webhooks bind:this={hook} bind:hooks hosted {query} /></div>
{#if warnShown}<Alerts />{/if}
<div class:hide={!showPart(filter, 'actions')}><Automations bind:this={act} bind:rules hosted {query} /></div>

<style>
  .rbar { flex: none; display: flex; flex-wrap: wrap; align-items: center; gap: 8px; margin-bottom: 12px; }
  .find { width: 180px; flex: 0 1 180px; min-width: 120px; }
  .si .ct { margin-left: 5px; color: var(--muted); font-size: 11px; }
  .warn { flex: none; display: flex; align-items: center; gap: 5px; color: var(--red-text); margin: -2px 0 10px; }
  .hide { display: none; }
</style>
