<script>
  import { onMount } from 'svelte';
  import { api } from './lib/api.js';
  import Auth from './Auth.svelte';
  import Shell from './Shell.svelte';

  let hello = $state(null);
  let signedIn = $state(null);
  let failed = $state('');

  async function load() {
    try {
      hello = await api('/hello');
      if (hello.setup_needed) {
        signedIn = false;
        return;
      }
      await api('/auth/me');
      signedIn = true;
    } catch (e) {
      if (e.status === 401) signedIn = false;
      else failed = e.message;
    }
  }

  onMount(load);
</script>

{#if failed}
  <div class="center"><p class="err">Can't reach the panel: {failed}</p></div>
{:else if hello && signedIn === false}
  <Auth {hello} onDone={() => { hello.setup_needed = false; signedIn = true; }} />
{:else if hello && signedIn}
  <Shell {hello} onSignOut={() => (signedIn = false)} />
{/if}

<style>
  .center { height: 100%; display: grid; place-items: center; }
</style>
