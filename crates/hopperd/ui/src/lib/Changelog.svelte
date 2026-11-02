<script>
  // A changelog or release notes, one look everywhere (as in Hopper
  // desktop): body text with headings only a step above it, so a
  // CurseForge `<h2>` and a GitHub `##` read the same. Takes markdown
  // (Versions, Review, release notes) or hopper-core's parsed blocks
  // (Browse). Shortcodes like `:sparkles:` arrive as emoji from the server.
  import BrowseRich from '../BrowseRich.svelte';
  import { md } from './md.js';
  let { markdown = null, blocks = null, empty = 'No changelog for this version.' } = $props();
</script>

{#if blocks}<BrowseRich {blocks} {empty} compact />
{:else if markdown?.trim()}<div class="changelog">{@html md(markdown)}</div>
{:else}<div class="small muted">{empty}</div>{/if}

<style>
  .changelog { color: #d4d4da; font-size: 13px; line-height: 1.6; overflow-wrap: anywhere; }
  .changelog :global(p) { margin: 0 0 8px; }
  .changelog :global(:is(h1, h2, h3, h4, h5, h6)) { color: var(--text); font-weight: 650; font-size: 13px; margin: 10px 0 4px; }
  .changelog :global(h3) { font-size: 14px; }
  .changelog :global(:is(h1, h2, h3, h4, h5, h6):first-child) { margin-top: 0; }
  .changelog :global(ul) { margin: 2px 0 8px; padding-left: 18px; }
  .changelog :global(li) { margin: 2px 0; }
  .changelog :global(a) { color: var(--blue); }
  .changelog :global(b) { font-weight: 650; color: var(--text); }
  .changelog :global(code) { font: 12px var(--mono); background: var(--raise); padding: 0 5px; border-radius: var(--radius-sm); }
  .changelog :global(img) { max-width: 100%; }
  .changelog > :global(:last-child) { margin-bottom: 0; }
</style>
