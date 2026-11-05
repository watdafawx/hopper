<script>
  // Settings › Updates: whether pack updates and roll backs are clean by
  // default (every pack mod and file becomes the server pack's, like the
  // CurseForge app), as in Hopper desktop's "Clean modpack updates". Each
  // server can pick its own in Versions and changelogs.
  import { GitCompare } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';

  let clean = $state(null);
  let saving = $state(false);

  async function load() {
    try {
      clean = (await api('/settings/pack-switch')).clean_default;
    } catch {
      clean = null;
    }
  }

  async function put(on) {
    saving = true;
    try {
      await api('/settings/pack-switch', { method: 'PUT', body: { clean_default: on } });
      clean = on;
    } catch (e) {
      say(e.message, true);
    } finally {
      saving = false;
    }
  }

  load();
</script>

{#if clean != null}
  <div class="card col sw">
    <h3><GitCompare size={14} />Pack versions</h3>
    <label class="row small"><input type="checkbox" checked={clean} disabled={saving} onchange={(e) => put(e.currentTarget.checked)} /><b>Clean modpack updates</b></label>
    <div class="small subtle">Updates and roll backs make every pack mod and pack file the server pack's, like the CurseForge app. The server's own mods, worlds and server.properties stay, and History undoes the rest. Off, they start on Keep the server's changes: a preview where you pick which edits stay. Each server can pick its own in Versions and changelogs.</div>
  </div>
{/if}

<style>
  .sw { gap: 8px; }
</style>
