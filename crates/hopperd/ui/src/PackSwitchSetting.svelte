<script>
  // Settings › Updates: whether moving a server to another pack version
  // (rolling back, or updating to a picked one) compares first, as in
  // Hopper desktop's "Compare versions before switching".
  import { GitCompare } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';

  let compare = $state(null);
  let saving = $state(false);

  async function load() {
    try {
      compare = (await api('/settings/pack-switch')).compare;
    } catch {
      compare = null;
    }
  }

  async function put(on) {
    saving = true;
    try {
      await api('/settings/pack-switch', { method: 'PUT', body: { compare: on } });
      compare = on;
    } catch (e) {
      say(e.message, true);
    } finally {
      saving = false;
    }
  }

  load();
</script>

{#if compare != null}
  <div class="card col sw">
    <h3><GitCompare size={14} />Pack versions</h3>
    <label class="row small"><input type="checkbox" checked={compare} disabled={saving} onchange={(e) => put(e.currentTarget.checked)} /><b>Compare versions before switching</b></label>
    <div class="small subtle">Rolling a server's modpack back, or moving it to a version you pick, shows what changes first. Off, it updates straight away and only stops if something needs your say. Each version keeps a Compare first button.</div>
  </div>
{/if}

<style>
  .sw { gap: 8px; }
</style>
