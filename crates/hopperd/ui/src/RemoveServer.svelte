<script>
  // Bottom of Settings › General: take the server off the panel (the folder
  // stays), or delete it: the folder moves to .hopper-deleted in the
  // servers folder for 30 days (Settings › Panel folder can restore it).
  // Servers that run where they are (maybe for another panel too) can only
  // be taken off.
  import { Trash2, EyeOff } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { confirmIt } from './lib/dialog.svelte.js';

  let { server } = $props();
  let root = $state(null);
  let startPermanent = $state(false);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));
  const norm = (p) => (p || '').replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase();
  const inRoot = $derived(!!root && norm(server.path).slice(0, norm(server.path).lastIndexOf('/')) === norm(root));

  api('/settings/servers-folder').then((r) => ((root = r.path), (startPermanent = !!r.delete_permanently)), () => {});

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

  // Delete opens its own dialog with two choices. Moving is the default (30
  // days to change your mind); deleting for good is one tick box. Settings › Panel folder sets which one it starts on.
  let dlg = $state(null); // { permanent, keep, busy }

  function openDelete() {
    dlg = { permanent: !!startPermanent, keep: true, busy: false };
  }

  async function del() {
    if (!dlg || dlg.busy) return;
    dlg.busy = true;
    const permanent = dlg.permanent;
    try {
      const r = await post(`/servers/${encodeURIComponent(server.id)}/remove`, {
        mode: 'delete',
        keep_world: dlg.keep,
        permanent,
      });
      const world = r?.kept_world ? `Its world is kept in ${r.kept_world}. ` : '';
      say(permanent ? `${server.name} deleted for good. ${world}` : `${server.name} deleted. ${world}Restore the server within 30 days from Settings › Panel folder.`);
      dlg = null;
      location.hash = '/';
    } catch (e) {
      say(e.message, true);
      if (dlg) dlg.busy = false;
    }
  }

  function keys(e) {
    if (dlg && e.key === 'Escape' && !dlg.busy) {
      e.preventDefault();
      e.stopPropagation();
      dlg = null;
    }
  }
</script>

<svelte:window onkeydowncapture={keys} />

<h4>Remove this server</h4>
<div class="dz">
  <div class="row wrap">
    <div class="grow small">
      {#if alive}<span class="warn">Stop the server first.</span>
      {:else if inRoot}Remove from panel keeps the folder. Delete moves it to .hopper-deleted for 30 days, or deletes it for good.
      {:else}It runs where it is{server.managed_by ? `, and ${server.managed_by} knows it too` : ''}: the panel can take it off its list, never delete its files.{/if}
    </div>
    <button class="btn sm bad" disabled={alive} onclick={takeOff}><EyeOff size={13} />Remove from panel</button>
    {#if inRoot}<button class="btn sm danger" disabled={alive} onclick={openDelete}><Trash2 size={13} />Delete server…</button>{/if}
  </div>
</div>

{#if dlg}
  <div class="dialog-back" role="presentation" onclick={(e) => e.target === e.currentTarget && !dlg.busy && (dlg = null)}>
    <div class="box popover" role="dialog" aria-modal="true" aria-label="Delete {server.name}">
      <form class="f" onsubmit={(e) => (e.preventDefault(), del())}>
        <h3>Delete {server.name}?</h3>
        <label class="row small"><input type="checkbox" bind:checked={dlg.keep} disabled={dlg.busy} />Keep the world (moved to Kept worlds in the servers folder)</label>
        <label class="row small"><input type="checkbox" bind:checked={dlg.permanent} disabled={dlg.busy} />Delete permanently (can't be undone)</label>
        {#if dlg.permanent}
          <p class="small warn-text">The folder is deleted for good and can't be restored.</p>
        {:else}
          <p class="small subtle">Its folder moves to .hopper-deleted in the servers folder for 30 days: restore it from Settings › Panel folder.</p>
        {/if}
        <div class="row end">
          <button type="button" class="btn" disabled={dlg.busy} onclick={() => (dlg = null)}>Cancel</button>
          <button class="btn danger" disabled={dlg.busy}>{dlg.busy ? 'Deleting…' : dlg.permanent ? 'Delete permanently' : 'Delete server'}</button>
        </div>
      </form>
    </div>
  </div>
{/if}

<style>
  .dz { border: 1px solid #ef444455; background: #ef44440a; border-radius: var(--radius-lg); padding: 10px 12px; margin-top: 4px; }
  .grow { flex: 1; min-width: 200px; color: var(--subtle); }
  .wrap { flex-wrap: wrap; gap: 8px; }
  .warn { color: var(--gold); }
  .dialog-back { z-index: 80; }
  .box { width: min(440px, 100%); padding: 18px; }
  .f { display: flex; flex-direction: column; gap: 12px; }
  .box h3 { margin: 0; font-size: 15px; font-weight: 600; }
  .end { justify-content: flex-end; }
  .warn-text { color: var(--danger-text, #f87171); margin: 0; }
</style>
