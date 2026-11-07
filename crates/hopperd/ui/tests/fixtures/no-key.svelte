<script>
  import { onChange } from '../../src/lib/changes.svelte.js';
  let { sid } = $props();
  let rows = $state([]);
  async function load() {
    rows = await fetch(`/x/${sid}`);
  }
  $effect(() => {
    load();
  });
  $effect(() => onChange(sid, ['files'], load));
</script>
