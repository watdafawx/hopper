<script>
  // After a crashed start: what the panel found in the crash report or the
  // log, as Hopper desktop's server card says it (rigsync.rs): the headline,
  // where it's from, and each mod it points at with its icon (opening the
  // mod peek) and a Remove from server. Only shows when the scan found
  // mods; the crash banner above covers the rest.
  import { AlertTriangle, FileText, Share2, RefreshCw } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { removeMods } from './lib/modops.js';

  let { server, onPeek, onShare, openFile } = $props();

  const sid = $derived(server.id);
  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));
  // Looked at again when the server stops or crashes (a new start's result).
  const settled = $derived(server.state === 'crashed' || server.state === 'stopped');
  let crash = $state(null);
  let mods = $state([]);
  let checking = $state(false);
  // Only the newest look counts (the state can flip while one is out).
  let seq = 0;

  async function load() {
    const n = ++seq;
    checking = true;
    try {
      const c = (await api(`${base}/crash`)).crash;
      const m = c?.hits?.length ? (await api(`${base}/mods/list`)).mods || [] : [];
      if (n !== seq) return;
      crash = c;
      mods = m;
    } catch {
      if (n === seq) crash = null;
    } finally {
      if (n === seq) checking = false;
    }
  }

  $effect(() => {
    sid;
    if (settled) load();
    else {
      seq++;
      crash = null;
      checking = false;
    }
  });

  const lc = (s) => (s || '').toLowerCase();
  const modOf = (h) => mods.find((m) => (h.file_name && lc(m.file) === lc(h.file_name)) || (h.mod_id && lc(m.name) === lc(h.mod_id)));
  const fromReport = $derived(!!crash?.source && !crash.source.includes('/'));

  async function remove(m) {
    if (await removeMods(base, [m], alive)) load();
  }
</script>

{#if crash?.hits?.length && settled}
  <div class="crash">
    <div class="row head"><AlertTriangle size={14} /><b class="grow">The server's last start crashed: {crash.headline}</b>
      <button class="btn sm ghost icon" title="Look again" disabled={checking} onclick={load}><RefreshCw size={13} /></button></div>
    {#if crash.source}<div class="tiny muted">{crash.client_only ? `From ${crash.source}. Client-only mods on the server are the likely cause.` : `From ${crash.source}.`}</div>{/if}
    {#each crash.hits as h, i (i)}
      {@const m = modOf(h)}
      <div class="hit row-hover">
        {#if m}
          <button class="mod" title="Open {m.name}" onclick={() => onPeek(m)}>{#if m.icon}<img src={m.icon} alt="" width="18" height="18" loading="lazy" />{:else}<span class="ph"></span>{/if}<b>{m.name}</b></button>
        {:else}
          <span class="mod"><span class="ph"></span><b>{h.mod_id || h.file_name}</b></span>
        {/if}
        <span class="tiny muted grow ell" title={h.reason}>{h.reason}</span>
        {#if m}<button class="btn sm" disabled={alive} title={alive ? 'Stop the server first' : `Moves ${m.file} to .hopper-backups`} onclick={() => remove(m)}>Remove from server</button>{/if}
      </div>
    {/each}
    <div class="row acts">
      {#if fromReport}<button class="btn sm ghost" title="Opens the crash report file" onclick={() => openFile(`crash-reports/${crash.source}`)}><FileText size={13} />Open report</button>{/if}
      {#if crash.source}<button class="btn sm ghost" onclick={() => onShare(fromReport ? `crash-reports/${crash.source}` : crash.source)}><Share2 size={13} />Share on mclo.gs</button>{/if}
    </div>
  </div>
{/if}

<style>
  .crash { display: flex; flex-direction: column; gap: 6px; padding: 10px 12px; margin-bottom: 10px; border: 1px solid #ef444499; border-radius: var(--radius-lg); background: var(--red-dim); }
  .head { color: var(--red); }
  .head b { font-weight: 600; }
  .grow { flex: 1; min-width: 0; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .hit { display: flex; align-items: center; gap: 12px; padding: 3px 6px; border-radius: var(--radius); }
  .mod { display: inline-flex; align-items: center; gap: 7px; flex: none; color: var(--text); padding: 0; }
  button.mod:hover b { color: var(--acc); }
  .mod img, .ph { width: 18px; height: 18px; border-radius: 4px; background: var(--raise); flex: none; display: inline-block; }
  .mod b { font-weight: 600; font-size: 12.5px; }
  .acts { gap: 4px; }
</style>
