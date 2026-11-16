<script>
  // The notice on a server that was moved away (design/multi-panel.md, A5),
  // in the notice slot above its tabs.
  // The copy stays, stopped, until someone keeps using it or deletes it.
  // Deleting is the usual delete (to .hopper-deleted for 30 days), behind a
  // tick box and a button that says what it does.
  import { ArrowRightLeft, Trash2 } from '@lucide/svelte';
  import { post, del } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { movedLine } from './lib/moves.js';
  import Notice from './lib/Notice.svelte';
  import { mainClass } from './lib/noticeorder.js';

  let { server, owner = false } = $props();
  const mark = $derived(server.moved);
  const there = $derived(mark ? (mark.to_node ? `${mark.to_node}:${mark.to_id}` : mark.to_id) : '');
  let dlg = $state(null); // { ticked, busy }

  async function keep() {
    try {
      await del(`/servers/${encodeURIComponent(server.id)}/moved`);
      say(`${server.name} can start here again.`);
    } catch (e) {
      say(e.message, true);
    }
  }

  async function remove() {
    if (!dlg?.ticked || dlg.busy) return;
    dlg.busy = true;
    try {
      await post(`/servers/${encodeURIComponent(server.id)}/remove`, { mode: 'delete', keep_world: false, permanent: false });
      say(`Deleted the old copy of ${server.name}. Restore it within 30 days from Settings › Panel folder.`);
      dlg = null;
      location.hash = there ? `/server/${encodeURIComponent(there)}/console` : '/';
    } catch (e) {
      say(e.message, true);
      if (dlg) dlg.busy = false;
    }
  }
</script>

<svelte:window onkeydowncapture={(e) => dlg && e.key === 'Escape' && !dlg.busy && (e.preventDefault(), e.stopPropagation(), (dlg = null))} />

{#if mark}
  <!-- Only the owner or a manager decides what happens to the old copy. -->
  <Notice id="moved" tier="choice" icon={ArrowRightLeft} title={movedLine(mark).split('. This copy')[0]} subject="This copy is stopped and kept" since={mark.at || 0} primary={there ? openThere : null} actions={owner ? decide : null} />
{/if}

{#snippet openThere(fit)}<a class={mainClass(fit)} href="#/server/{encodeURIComponent(there)}/console">{fit.narrow ? 'Open it' : `Open it on ${mark.to}`}</a>{/snippet}
{#snippet decide()}
  <button class="btn sm" onclick={keep}>Keep using this copy</button>
  <button class="btn sm bad" onclick={() => (dlg = { ticked: false, busy: false })}><Trash2 size={12} />Delete the old copy…</button>
{/snippet}

{#if dlg}
  <div class="dialog-back" role="presentation" onclick={(e) => e.target === e.currentTarget && !dlg.busy && (dlg = null)}>
    <div class="box popover" role="dialog" aria-modal="true" aria-label="Delete the old copy of {server.name}">
      <h3>Delete the old copy of {server.name}?</h3>
      <div class="small muted">The copy on {mark?.to} stays as it is. This one goes to the servers folder's bin for 30 days (Settings › Panel folder can restore it).</div>
      <label class="tick small"><input type="checkbox" bind:checked={dlg.ticked} disabled={dlg.busy} />The server on {mark?.to} works; delete this copy</label>
      <div class="row end"><span class="sp"></span><button class="btn" disabled={dlg.busy} onclick={() => (dlg = null)}>Cancel</button><button class="btn bad" disabled={!dlg.ticked || dlg.busy} onclick={remove}><Trash2 size={13} />{dlg.ticked ? 'Delete the old copy' : 'Tick the box to delete'}</button></div>
    </div>
  </div>
{/if}

<style>
  .box { width: min(460px, 100%); display: flex; flex-direction: column; gap: 10px; padding: 14px; }
  .box h3 { margin: 0; font-size: 14px; }
  .tick { display: flex; align-items: center; gap: 7px; }
  .end { gap: 8px; }
</style>
