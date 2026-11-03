<script>
  // CurseForge's own pick for its Browse tab ("Hidden gem", "Popular",
  // "Sponsored"), as a small chip beside a project's name. Same words and
  // colours as Hopper desktop's.
  import { Gem, Flame, Megaphone } from '@lucide/svelte';

  let { tag = '' } = $props();
  const ICON = { 'Hidden gem': Gem, Popular: Flame, Sponsored: Megaphone };
  const Icon = $derived(ICON[tag]);
</script>

{#if Icon}
  <span class="chip feat" class:gem={tag === 'Hidden gem'} class:hot={tag === 'Popular'} title="Picked by CurseForge"><Icon size={11} />{tag}</span>
{/if}

<style>
  .feat { height: 18px; font-size: 11px; padding: 0 6px 1px; gap: 4px; vertical-align: middle; flex: none; }
  .gem { color: var(--blue); background: var(--blue-dim); }
  .hot { color: var(--gold); background: var(--gold-dim); }
</style>
