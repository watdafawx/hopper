<script>
  // The bug from Settings > Sleep: reads `info`, and load() assigns it
  // when the answer arrives, so the effect runs again, forever.
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
    if (info) load();
  });
</script>
