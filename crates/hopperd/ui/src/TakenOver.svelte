<script>
  // Settings › "Taken over from Crafty": when, which Java, and Give back.
  import { ArrowLeftRight, Undo2 } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { confirmIt } from './lib/dialog.svelte.js';
  import { say } from './lib/toast.svelte.js';

  let { server } = $props();
  let t = $state(null);
  let busy = $state(false);
  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));
  const java = $derived(t?.java_pinned ? t.java_pinned.split(/[\\/]/).slice(-3, -2)[0] : '');

  async function load() {
    try {
      t = (await api(`${base}/takeover`)).taken_over;
    } catch (e) {
      say(e.message, true);
    }
  }

  async function giveBack() {
    const ok = await confirmIt(`Give ${server.name} back to Crafty?`, {
      body: "Its old run.bat and Crafty's marker come back, and Hopper stops running it. Start it from Crafty afterwards.",
      ok: 'Give back',
    });
    if (!ok) return;
    busy = true;
    try {
      await post(`${base}/give-back`);
      say(`${server.name} is Crafty's again.`);
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = false;
    }
  }

  $effect(() => {
    server.id;
    load();
  });
</script>

<div class="card side col">
  <h3><ArrowLeftRight size={14} />Taken over from {server.taken_over_from}</h3>
  {#if t}
    <div class="tiny muted">{new Date(t.at * 1000).toLocaleDateString()}{t.minecraft ? ` · Minecraft ${t.minecraft}` : ''}</div>
    <div class="tiny muted">{#if t.java_pinned}Java pinned to <span class="mono" title={t.java_pinned}>{java}</span> (Crafty used plain java){:else}Crafty's Java kept{/if}</div>
  {/if}
  <div class="row"><span class="sp"></span>
    <button class="btn sm" disabled={alive || busy} title={alive ? 'Stop it first' : ''} onclick={giveBack}><Undo2 size={13} />Give back to {server.taken_over_from}</button></div>
</div>

<style>
  .side { width: 320px; flex: none; gap: 6px; }
  .side h3 { margin-bottom: 2px; }
  @media (max-width: 900px) { .side { width: auto; } }
</style>
