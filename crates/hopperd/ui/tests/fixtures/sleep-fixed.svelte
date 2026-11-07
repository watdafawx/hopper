<script>
  import { untrack } from 'svelte';
  import { api } from '../../src/lib/api.js';
  let { server } = $props();
  const sid = $derived(server.id);
  let info = $state(null);
  let minutes = $state(15);

  async function load() {
    info = await api(`/servers/${sid}/address`);
    minutes = info.sleep_after_minutes;
  }

  $effect(() => {
    sid;
    untrack(() => {
      if (info) load();
    });
  });
  // loop-safe: only runs when the id changes, load() reads nothing tracked
  $effect(() => {
    sid;
    load();
  });
</script>
