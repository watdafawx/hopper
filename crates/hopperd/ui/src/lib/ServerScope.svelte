<script>
  // The server picker for a person or an API token (design/multi-panel.md,
  // A4): this panel's servers, then each node with "All on <node>" in
  // front of its servers. Picking the whole node covers its servers,
  // including ones made there later.
  import { live } from './live.svelte.js';
  import { scopeRows, covers, flipScope, scopeHint, NODE_SCOPE } from './scope.js';

  let { value = $bindable([]) } = $props();
  const rows = $derived(scopeRows(live.servers, live.nodes));
</script>

<div class="scope">
  {#each rows as r (r.node?.id ?? '')}
    <div class="r">
      <span class="tiny muted lbl" title={r.name}>{r.name}</span>
      <div class="chips">
        {#if r.node}
          {@const whole = value.includes(NODE_SCOPE + r.node.id)}
          <button type="button" class="chip" class:acc={whole} aria-pressed={whole} onclick={() => (value = flipScope(value, NODE_SCOPE + r.node.id))}>All on {r.node.name}</button>
        {/if}
        {#each r.servers as s (s.id)}
          {@const whole = r.node && value.includes(NODE_SCOPE + r.node.id)}
          <button type="button" class="chip" class:acc={covers(value, s.id)} class:held={whole} aria-pressed={covers(value, s.id)} disabled={whole} title={whole ? `Included: all servers on ${r.node.name}` : null} onclick={() => (value = flipScope(value, s.id))}>{s.name}</button>
        {/each}
        {#if r.node && !r.servers.length}<span class="tiny muted">No servers there yet</span>{/if}
      </div>
    </div>
  {/each}
  <span class="tiny muted">{scopeHint(value, live.servers, live.nodes)}</span>
</div>

<style>
  .scope { display: flex; flex-direction: column; gap: 6px; min-width: 0; }
  .r { display: flex; align-items: flex-start; gap: 8px; min-width: 0; }
  /* The chips' height, so its text sits level with theirs. */
  .lbl { flex: none; width: 96px; line-height: 20px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .chips { display: flex; flex-wrap: wrap; gap: 6px; min-width: 0; flex: 1; }
  .chip { cursor: pointer; }
  /* Covered by the whole node: shown picked, not separately clickable. */
  .chip.held { opacity: 0.7; cursor: default; }
  @media (max-width: 600px) {
    .r { flex-direction: column; gap: 4px; }
    .lbl { width: auto; }
  }
</style>
