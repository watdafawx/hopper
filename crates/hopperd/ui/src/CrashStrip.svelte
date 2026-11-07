<script>
  // One line above every server tab but Logs & crashes after a crashed
  // start, as Hopper desktop's (server_crash.rs): what stopped the server,
  // "Put 1.2 back" when the error names an update, and "See why →", which
  // opens Logs & crashes (the full card lives only there). Closing it hides
  // this crash only; the next one shows again.
  import { AlertTriangle, Undo2, X } from '@lucide/svelte';
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

  const key = $derived(crash ? `hopper.crash-strip.${server.id}.${crash.when}` : '');
  let closed = $state(false);
  $effect(() => {
    key;
    try {
      closed = !!key && localStorage.getItem(key) === '1';
    } catch {
      closed = false;
    }
  });
  function close() {
    closed = true;
    try {
      localStorage.setItem(key, '1');
    } catch {}
  }

  const bare = (f) => (f || '').toLowerCase().replace(/\.disabled$/, '');
  const st = $derived(crash?.stopped && (crash.suspects || []).some((s) => s.file_name === crash.stopped.file_name) ? crash.stopped : null);
  const stoppedSuspect = $derived(st ? crash.suspects.find((s) => s.file_name === st.file_name) : null);
  const changed = $derived(stoppedSuspect?.changed || null);
  const mod = $derived(st ? mods.find((m) => bare(m.file) === bare(st.file_name)) : null);
  const what = $derived(st ? 'stopped it from starting' : crash?.headline || '');

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

{#if crash && settled && !closed && !hidden}
  <div class="strip">
    <AlertTriangle size={14} />
    <b class="t">The server crashed</b>
    <span class="muted">·</span>
    <span class="what">
      {#if st}<span class="mod">{#if mod?.icon}<img src={mod.icon} alt="" width="16" height="16" />{:else}<span class="ph"></span>{/if}<b>{mod?.name || st.file_name.replace(/\.disabled$/, '').replace(/\.jar$/, '')}</b></span>{/if}
      {what}
    </span>
    <span class="grow"></span>
    {#if changed && !back}
      <button class="btn sm pri" disabled={busy || alive} title={alive ? 'Stop the server first' : 'Swaps in the older copy. The newer one goes to Mods › History.'} onclick={putBack}><Undo2 size={13} />{changed.from ? `Put ${changed.from} back` : 'Put the older version back'}</button>
    {/if}
    <button class="btn sm" onclick={() => seeWhy(reportOf(crash))}>See why →</button>
    <button class="btn sm ghost icon" title="Hide this notice" onclick={close}><X size={13} /></button>
  </div>
{/if}

<style>
  .strip { display: flex; align-items: center; gap: 8px; margin: 0 18px 10px; padding: 6px 8px 6px 12px; border: 1px solid #ef444499; border-radius: var(--radius-lg); background: #ef44440f; color: var(--red); font-size: 13px; }
  .t { font-weight: 600; flex: none; }
  .what { color: var(--text); min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .mod { display: inline-flex; align-items: center; gap: 6px; margin-right: 4px; vertical-align: middle; }
  .mod b { font-weight: 600; }
  .mod img, .ph { width: 16px; height: 16px; border-radius: var(--radius-sm); background: var(--raise); flex: none; display: inline-block; }
  .grow { flex: 1; }
</style>
