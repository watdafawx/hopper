<script>
  // Bottom of Settings › General: take the server off the panel (the folder
  // stays), or delete it: the folder moves to .hopper-deleted in the
  // servers folder for 30 days (Settings › Panel folder can restore it).
  // Servers that run where they are (maybe for another panel too) can only
  // be taken off.
  import { Trash2, EyeOff } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { ask, confirmIt } from './lib/dialog.svelte.js';

  let { server } = $props();
  let root = $state(null);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));
  const norm = (p) => (p || '').replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase();
  const inRoot = $derived(!!root && norm(server.path).slice(0, norm(server.path).lastIndexOf('/')) === norm(root));

  api('/settings/servers-folder').then((r) => (root = r.path), () => {});

  async function takeOff() {
    const ok = await confirmIt(`Remove ${server.name} from the panel?`, {
      body: `The folder stays as it is (${server.path}). Add it again any time with New server › Folder.`,
      ok: 'Remove from panel',
      danger: true,
    });
    if (!ok) return;
    try {
      await post(`/servers/${encodeURIComponent(server.id)}/remove`, { mode: 'remove' });
      say(`${server.name} is off the panel. Its folder is untouched.`);
      location.hash = '/';
    } catch (e) {
      say(e.message, true);
    }
  }

  async function del() {
    const typed = await ask(`Delete ${server.name}?`, '', {
      body: `Its folder moves to .hopper-deleted in the servers folder and stays there 30 days: restore it from Settings › Panel folder. Type ${server.name} to delete it.`,
      ok: 'Delete server',
      placeholder: server.name,
    });
    if (typed == null) return;
    try {
      await post(`/servers/${encodeURIComponent(server.id)}/remove`, { mode: 'delete', confirm: typed.trim() });
      say(`${server.name} deleted. Restore it within 30 days from Settings › Panel folder.`);
      location.hash = '/';
    } catch (e) {
      say(e.message, true);
    }
  }
</script>

<h4>Remove this server</h4>
<div class="danger">
  <div class="row wrap">
    <div class="grow small">
      {#if alive}<span class="warn">Stop the server first.</span>
      {:else if inRoot}Remove from panel keeps the folder. Delete moves it to .hopper-deleted for 30 days.
      {:else}It runs where it is{server.managed_by ? `, and ${server.managed_by} knows it too` : ''}: the panel can take it off its list, never delete its files.{/if}
    </div>
    <button class="btn sm bad" disabled={alive} onclick={takeOff}><EyeOff size={13} />Remove from panel</button>
    {#if inRoot}<button class="btn sm badf" disabled={alive} onclick={del}><Trash2 size={13} />Delete server…</button>{/if}
  </div>
</div>

<style>
  .danger { border: 1px solid #ef444455; background: #ef44440a; border-radius: 10px; padding: 10px 12px; margin-top: 4px; }
  .grow { flex: 1; min-width: 200px; color: var(--subtle); }
  .wrap { flex-wrap: wrap; gap: 8px; }
  .warn { color: var(--gold); }
  .bad { color: var(--red); border-color: #ef444466; }
  .badf { background: var(--red); border-color: var(--red); color: #fff; font-weight: 600; }
</style>
