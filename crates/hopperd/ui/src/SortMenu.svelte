<script>
  // Sort ▾, as in Hopper desktop's Mods toolbar: shows the list's order
  // and sets the same order the column headers do.
  import { ArrowUpDown, ArrowDown, ArrowUp, ChevronDown, Check } from '@lucide/svelte';

  // choices: [[col, label]]; sort: { col, desc }; onSort(col) works like a header click.
  let { choices, sort, updatesFirst = $bindable(), onSort } = $props();
  let open = $state(false);

  const name = $derived(choices.find(([c]) => c === sort.col)?.[1] || 'Name');
  const order = $derived(updatesFirst ? `Updates first, then ${name}` : name);
  const dir = (col, desc) =>
    col === 'added' ? (desc ? 'Newest first' : 'Oldest first')
    : col === 'on' ? (desc ? 'Enabled first' : 'Disabled first')
    : desc ? 'Z to A' : 'A to Z';

  function toggleOpen(e) {
    e.stopPropagation();
    open = !open;
  }
</script>

<svelte:window onclick={() => (open = false)} onkeydown={(e) => e.key === 'Escape' && open && (open = false)} />

<div class="wrapp">
  <button class="btn sm" aria-haspopup="menu" aria-expanded={open} onclick={toggleOpen}>
    <span class="m"><ArrowUpDown size={13} /></span><span class="m">Sort:</span>{order}{#if sort.desc}<ArrowDown size={12} />{:else}<ArrowUp size={12} />{/if}<span class="m"><ChevronDown size={12} /></span>
  </button>
  {#if open}
    <div class="pop popover menu" role="menu" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.key === 'Escape' && (open = false)}>
      <div class="hd">Sort by</div>
      {#each choices as [col, label] (col)}
        {@const on = sort.col === col}
        <button class="menu-item" class:sel={on} role="menuitemradio" aria-checked={on} onclick={() => { open = false; onSort(col); }}>
          <span class="ck">{#if on}<Check size={13} />{/if}</span>{label}
          {#if on}<span class="dir">{dir(col, sort.desc)}{#if sort.desc}<ArrowDown size={12} />{:else}<ArrowUp size={12} />{/if}</span>{/if}
        </button>
      {/each}
      <div class="menu-sep"></div>
      <button class="menu-item" role="menuitemcheckbox" aria-checked={updatesFirst} onclick={() => { open = false; updatesFirst = !updatesFirst; }}>
        <span class="ck">{#if updatesFirst}<Check size={13} />{/if}</span>Updates first
      </button>
      <div class="hd">Clicking a column header changes this too.</div>
    </div>
  {/if}
</div>

<style>
  .wrapp { position: relative; }
  .m { color: var(--muted); display: inline-flex; }
  .pop { position: absolute; z-index: 70; top: calc(100% + 4px); left: 0; width: 280px; display: flex; flex-direction: column; }
  .hd { color: var(--muted); font-size: 11.5px; padding: 5px 10px; }
  .ck { width: 13px; display: grid; flex: none; }
  .sel { font-weight: 600; }
  .dir { margin-left: auto; display: inline-flex; align-items: center; gap: 3px; color: var(--muted); font-weight: 400; }
</style>
