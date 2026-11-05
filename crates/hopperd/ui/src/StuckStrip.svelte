<script>
  // One line above the server's tabs while its start looks stuck (no
  // progress, almost no CPU), as Hopper desktop's (stuck.rs) and like the
  // crash strip: what's stuck, the mod the thread dump points at first,
  // "Restart server…", and "See why →", which opens Logs & crashes on the
  // saved dump. The server process watches itself
  // (hopper_core::stuckstart); this only shows `server.stuck`.
  import { Clock, RotateCcw, X } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { power } from './lib/power.js';

  let { server, seeWhy, hidden = false } = $props();

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const r = $derived(server.state === 'starting' ? server.stuck : null);
  let mods = $state([]);
  let busy = $state(false);

  $effect(() => {
    if (!r) return;
    api(`${base}/mods/list`)
      .then((m) => (mods = m.mods || []))
      .catch(() => {});
  });

  const bare = (f) => (f || '').toLowerCase().replace(/\.disabled$/, '');
  const topIx = $derived(r ? r.suspects.findIndex((s) => !s.knock_on) : -1);
  const top = $derived(topIx >= 0 ? r.suspects[topIx] : null);
  const mod = $derived(top ? mods.find((m) => bare(m.file) === bare(top.file_name)) : null);
  const topName = $derived(top ? r.names?.[topIx] || mod?.name || top.file_name.replace(/\.disabled$/, '').replace(/\.jar$/, '') : '');
  // "while loading mods" from the report's headline, with this server's name.
  const doing = $derived(r ? (r.headline.split(' looks stuck ')[1] || 'while starting').split(' (')[0] : '');
  // The dump's path under the server folder, for Logs & crashes.
  const dump = $derived.by(() => {
    const p = String(r?.dump || '').replaceAll('\\', '/');
    const i = p.lastIndexOf('crash-reports/');
    return i < 0 ? '' : p.slice(i);
  });

  async function wait() {
    busy = true;
    try {
      await post(`${base}/stuck/snooze`, {});
      say('Hopper asks again in 5 minutes if it is still stuck.');
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = false;
    }
  }
</script>

{#if r && !hidden}
  <div class="strip">
    <Clock size={14} />
    <b class="t">{server.name} looks stuck {doing}</b>
    {#if top}
      <span class="muted">·</span>
      <span class="what" title={top.why}>
        <span class="mod">{#if mod?.icon}<img src={mod.icon} alt="" width="16" height="16" />{:else}<span class="ph"></span>{/if}<b>{topName}</b></span>
        <span class="chip {top.certainty === 'Likely' ? 'warn' : ''}">{top.certainty}</span>
      </span>
    {/if}
    <span class="grow"></span>
    <button class="btn sm pri" onclick={() => power(server, 'restart')}><RotateCcw size={13} />Restart server…</button>
    <button class="btn sm" onclick={() => seeWhy(dump)}>See why →</button>
    <button class="btn sm ghost icon" disabled={busy} title="Wait: hides this and asks again in 5 minutes if it's still stuck" onclick={wait}><X size={13} /></button>
  </div>
{/if}

<style>
  .strip { display: flex; align-items: center; gap: 8px; margin: 0 18px 10px; padding: 6px 8px 6px 12px; border: 1px solid #facc1599; border-radius: var(--radius-lg); background: #facc150f; color: var(--gold); font-size: 13px; }
  .t { font-weight: 600; flex: none; }
  .what { display: inline-flex; align-items: center; gap: 8px; color: var(--text); min-width: 0; overflow: hidden; white-space: nowrap; }
  .mod { display: inline-flex; align-items: center; gap: 6px; vertical-align: middle; }
  .mod b { font-weight: 600; }
  .mod img, .ph { width: 16px; height: 16px; border-radius: var(--radius-sm); background: var(--raise); flex: none; display: inline-block; }
  .grow { flex: 1; }
</style>
