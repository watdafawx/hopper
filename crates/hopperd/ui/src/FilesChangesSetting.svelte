<script>
  // Settings › Content: show what changed in the files when a mod has no
  // changelog (off by default), as in Hopper desktop. The owner's, panel-wide.
  import { FileSearch } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { fc, HELP } from './lib/filesChanges.svelte.js';

  let on = $state(null);
  let saving = $state(false);

  async function load() {
    try {
      on = (await api('/settings/files-changes')).on;
      fc.on = on;
    } catch {
      on = null;
    }
  }

  async function put(v) {
    saving = true;
    try {
      await api('/settings/files-changes', { method: 'PUT', body: { on: v } });
      on = v;
      fc.on = v;
    } catch (e) {
      say(e.message, true);
      await load();
    } finally {
      saving = false;
    }
  }

  load();
</script>

{#if on != null}
  <div class="card col fc">
    <h3><FileSearch size={14} />Changes in the files</h3>
    <label class="row small"><input type="checkbox" checked={on} disabled={saving} onchange={(e) => put(e.currentTarget.checked)} /><b>Show what changed in the files</b></label>
    <div class="small subtle">{HELP}</div>
  </div>
{/if}

<style>
  .fc { gap: 8px; }
</style>
