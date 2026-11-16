<script>
  // The notice in the slot above every server tab but Logs & crashes after
  // a crashed start, as Hopper desktop's (server_crash.rs): what stopped
  // the server, "Roll back X to 1.2" when the error names an update, and
  // "See why →", which opens Logs & crashes (the full card lives only
  // there). It stops play, so it can't be closed; it goes once the server
  // starts without crashing.
  import { Undo2 } from '@lucide/svelte';
  import Notice from './lib/Notice.svelte';
  import { mainClass } from './lib/noticeorder.js';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { crashes } from './lib/crashseen.svelte.js';

  let { server, seeWhy, hidden = false } = $props();
  // The live feed hands over a new `server` object every second; effects key on the id.
  const sid = $derived(server.id);

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));
  const settled = $derived(server.state === 'crashed' || server.state === 'stopped');
  let crash = $state(null);
  let mods = $state([]);
  let busy = $state(false);
  let back = $state(false);
  let seq = 0;

  async function load() {
    const n = ++seq;
    try {
      const r = await api(`${base}/crash`);
      const m = r.crash ? (await api(`${base}/mods/list`)).mods || [] : [];
      if (n !== seq) return;
      crash = r.crash;
      mods = m;
      if (r.crash) crashes[server.id] = r.crash.when;
      else delete crashes[server.id];
    } catch {
      if (n === seq) crash = null;
    }
  }

  $effect(() => {
    sid;
    back = false;
    if (settled) load();
    else {
      seq++;
      crash = null;
    }
  });

  // The report the notice describes, by its path under the server folder, so
  // "See why" opens Logs & crashes on exactly that one.
  function reportOf(c) {
    const s = String(c?.source || '').replaceAll('\\', '/');
    const i = s.lastIndexOf('crash-reports/');
    return i < 0 ? '' : s.slice(i);
  }

  const bare = (f) => (f || '').toLowerCase().replace(/\.disabled$/, '');
  const st = $derived(crash?.stopped && (crash.suspects || []).some((s) => s.file_name === crash.stopped.file_name) ? crash.stopped : null);
  const stoppedSuspect = $derived(st ? crash.suspects.find((s) => s.file_name === st.file_name) : null);
  const changed = $derived(stoppedSuspect?.changed || null);
  const mod = $derived(st ? mods.find((m) => bare(m.file) === bare(st.file_name)) : null);
  const modName = $derived(st ? mod?.name || st.file_name.replace(/\.disabled$/, '').replace(/\.jar$/, '') : '');
  const what = $derived(st ? `${modName} stopped it from starting` : crash?.headline || '');
  const rollLabel = $derived(changed ? (changed.from ? `Roll back ${modName || 'the mod'} to ${changed.from}` : `Roll back ${modName || 'the mod'}`) : '');

  async function putBack() {
    busy = true;
    try {
      await post(`${base}/crash/put-back`, { file: changed.file_name });
      say(`${changed.from || 'The older version'} is back. ${changed.to || 'The newer one'} is in Mods › History. Start the server to try again.`);
      back = true;
      await load();
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = false;
    }
  }
</script>

{#if crash && settled && !hidden}
  <Notice id="srv-crash" tier="stop" title="The server crashed" subject={what} since={crash.when} details={st ? crashDetails : null}>
    {#snippet primary(fit)}
      {#if changed && !back}
        <button class={mainClass(fit)} disabled={busy || alive} title={alive ? 'Stop the server first' : `${fit.narrow ? rollLabel + '. ' : ''}Swaps in the older copy. The newer one goes to Mods › History.`} onclick={putBack}><Undo2 size={13} />{fit.narrow ? `Roll back ${modName || 'the mod'}` : rollLabel}</button>
      {/if}
    {/snippet}
    {#snippet actions()}
      <button class="btn sm" title="Opens Logs & crashes on this crash" onclick={() => seeWhy(reportOf(crash))}>See why →</button>
    {/snippet}
  </Notice>
{/if}

{#snippet crashDetails()}
  {#if st}<span class="mod">{#if mod?.icon}<img src={mod.icon} alt="" width="16" height="16" />{:else}<span class="ph"></span>{/if}<b>{modName}</b> stopped it from starting</span>{/if}
{/snippet}

<style>
  .mod { display: inline-flex; align-items: center; gap: 6px; }
  .mod b { font-weight: 600; }
  .mod img, .ph { width: 16px; height: 16px; border-radius: var(--radius-sm); background: var(--raise); flex: none; display: inline-block; }
</style>
