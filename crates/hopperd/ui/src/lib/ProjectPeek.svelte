<script>
  // A mod site's project, in the side panel beside what you're on, for a mod
  // the server doesn't have (a crash's "needs X"): its page with a way to
  // close, in the same panel as an installed mod's (ModPeek) and an author's.
  import BrowsePack from '../BrowsePack.svelte';

  // `source`: "modrinth" or "curseforge"; `id`: the project's id there.
  let { source, id, onClose } = $props();
</script>

<aside class="peek card col" aria-label="Project">
  {#key `${source}:${id}`}<BrowsePack {source} {id} type="mods" onBack={onClose} />{/key}
</aside>

<style>
  .peek { gap: 8px; width: 400px; flex: none; position: sticky; top: 0; max-height: calc(100vh - 170px); overflow: auto; max-width: 100%; container-type: inline-size; }
  @media (max-width: 1150px) { .peek { position: fixed; right: 12px; top: 64px; bottom: 12px; max-height: none; z-index: 40; box-shadow: 0 20px 60px #000c; max-width: calc(100vw - 24px); } }
</style>
