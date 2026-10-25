<script>
  import { ArrowLeft, RotateCcw, Trash2, Folder, FileClock, File } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { bytes } from './lib/format.js';
  import { say } from './lib/toast.svelte.js';
  import { confirmIt } from './lib/dialog.svelte.js';

  let { server, close } = $props();
  // The live feed hands over a new `server` object every second; effects
  // that should run once per server key on the id alone.
  const sid = $derived(server.id);

  let items = $state([]);
  let loading = $state(true);
  const base = $derived(`/fs/${encodeURIComponent(server.id)}`);

  async function load() {
    loading = true;
    try {
      items = (await api(`${base}/trash`)).items;
    } catch (e) {
      say(e.message, true);
    } finally {
      loading = false;
    }
  }

  async function restore(i) {
    try {
      const r = await post(`${base}/trash/restore`, { id: i.id });
      say(i.kind === 'saved-over' ? `Restored the older ${r.path}` : `Restored ${r.path}`);
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function purge(ids) {
    const all = !ids.length;
    const ok = await confirmIt(all ? 'Empty the trash?' : 'Delete this for good?', {
      body: all ? `${items.length} items are deleted for good. This can't be undone.` : "This can't be undone.",
      ok: 'Delete for good',
      danger: true,
    });
    if (!ok) return;
    try {
      const r = await post(`${base}/trash/purge`, { ids });
      say(`Deleted ${r.removed}`);
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  const when = (t) => new Date(t * 1000).toLocaleString();

  $effect(() => {
    sid;
    load();
  });
</script>

<div class="wrap">
  <div class="ftool">
    <button class="btn sm icon ghost" title="Back to files" onclick={close}><ArrowLeft size={14} /></button>
    <b>Trash</b><span class="small muted">Deleted files and older versions of saved files, kept 30 days.</span>
    <span class="sp"></span>
    <button class="btn sm bad" disabled={!items.length} onclick={() => purge([])}><Trash2 size={13} />Empty trash</button>
  </div>
  <div class="list">
    {#each items as i (i.id)}
      <div class="row it">
        {#if i.dir}<Folder size={15} color="var(--acc)" />{:else if i.kind === 'saved-over'}<FileClock size={15} color="var(--blue)" />{:else}<File size={15} color="var(--muted)" />{/if}
        <div class="nmw">
          <div class="nm">{i.path}</div>
          <div class="tiny muted">{i.kind === 'saved-over' ? 'Version before a save' : 'Deleted'} · {when(i.at)} · {bytes(i.size)}</div>
        </div>
        <button class="btn sm" onclick={() => restore(i)}><RotateCcw size={13} />Restore</button>
        <button class="btn sm icon ghost" title="Delete for good" onclick={() => purge([i.id])}><Trash2 size={13} /></button>
      </div>
    {:else}
      <div class="msg muted">{loading ? 'Loading…' : 'The trash is empty.'}</div>
    {/each}
  </div>
</div>

<style>
  .wrap { display: flex; flex-direction: column; flex: 1; min-height: 0; border: 1px solid var(--line); border-radius: 10px; overflow: hidden; background: var(--base); }
  .ftool { display: flex; align-items: center; gap: 8px; padding: 8px 10px; border-bottom: 1px solid var(--line); flex-wrap: wrap; }
  .list { flex: 1; overflow: auto; }
  .it { padding: 8px 12px; border-bottom: 1px solid #26262c; }
  .nmw { flex: 1; min-width: 0; }
  .nm { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .msg { padding: 24px; text-align: center; }
</style>
