<script>
  let { server } = $props();
  let list = $state([]);
  async function load() {
    list = await fetch(`/x/${server.id}`);
  }
  $effect(() => {
    server.state;
    untrack(load);
  });
</script>
