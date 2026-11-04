<script>
  // A mod's config in the Mods tab's side panel (desktop's config icon on a
  // mod row opens the same): its files and settings stacked for the narrow
  // width, the unsaved bar under them, and "Open in Configs" for the page.
  import { X, SquareArrowOutUpRight } from '@lucide/svelte';
  import ConfigView from './lib/ConfigView.svelte';
  import ConfigBar from './lib/ConfigBar.svelte';
  import { jump } from './lib/files.svelte.js';

  let { server, mod, cmod, onClose, onOpenPage = null } = $props();
  let query = $state('');
  let pick = $state('');

  function editText(rel, line) {
    jump.path = rel;
    jump.line = line || 0;
    jump.text = true;
    location.hash = `/server/${[server.id, 'edit', ...rel.split('/')].map(encodeURIComponent).join('/')}`;
  }
</script>

<aside class="cpanel card col" aria-label="{mod.name} config">
  <div class="row top">
    {#if mod.icon}<img src={mod.icon} alt="" width="32" height="32" />{:else}<span class="ph"></span>{/if}
    <div class="grow"><div class="nm ell">{mod.name}</div><div class="tiny muted">Config · {cmod.files.length} file{cmod.files.length === 1 ? '' : 's'}{cmod.changed ? ` · ${cmod.changed} not at default` : ''}</div></div>
    {#if onOpenPage}<button class="btn sm ghost" title="Open this mod in the Configs tab" onclick={onOpenPage}><SquareArrowOutUpRight size={13} />Open in Configs</button>{/if}
    <button class="btn sm ghost icon" title="Close" onclick={onClose}><X size={14} /></button>
  </div>
  <input class="input find" placeholder="Search this mod's settings" bind:value={query} />
  <div class="scroll">
    {#key cmod.key}<ConfigView {server} rels={cmod.files} {query} stacked bind:pick onEditText={editText} />{/key}
  </div>
  <ConfigBar {server} compact />
</aside>

<style>
  .cpanel { gap: 8px; width: 420px; flex: none; position: sticky; top: 0; max-height: max(360px, calc(100dvh - 330px)); padding: 12px 12px 0; overflow: hidden; }
  .top img, .ph { width: 32px; height: 32px; border-radius: var(--radius); background: var(--raise); flex: none; }
  .nm { font-weight: 600; font-size: 14px; }
  .grow { flex: 1; min-width: 0; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .icon { padding: 3px 5px; }
  .find { padding: 5px 9px; color: var(--text); outline: none; }
  .scroll { overflow: auto; min-height: 0; flex: 1; margin: 0 -6px; padding: 0 6px 10px; }
  .cpanel :global(.ub) { margin: 0 -12px; border-radius: 0 0 var(--radius-lg) var(--radius-lg); }
  @media (max-width: 700px) { .cpanel { width: 100%; max-height: none; position: static; } .find { min-height: 44px; } }
</style>
