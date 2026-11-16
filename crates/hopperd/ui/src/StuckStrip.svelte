<script>
  // The notice in the slot above the server's tabs while its start looks
  // stuck (no progress, almost no CPU), as Hopper desktop's (stuck.rs):
  // what's stuck, the mod the thread dump points at first, "Restart
  // server…", "See why →" (Logs & crashes on the saved dump) and "Wait".
  // The server process watches itself (hopper_core::stuckstart); this only
  // shows `server.stuck`.
  import { Clock, RotateCcw } from '@lucide/svelte';
  import Notice from './lib/Notice.svelte';
  import { mainClass } from './lib/noticeorder.js';
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
  <Notice id="srv-stuck" tier="stop" icon={Clock} title="{server.name} looks stuck {doing}" subject={top ? `${topName} (${top.certainty})` : ''} since={r.when || 0} details={top ? stuckDetails : null}>
    {#snippet primary(fit)}
      <button class={mainClass(fit)} title="It won't finish on its own; starting again usually gets past it" onclick={() => power(server, 'restart')}><RotateCcw size={13} />{fit.narrow ? 'Restart…' : 'Restart server…'}</button>
    {/snippet}
    {#snippet actions()}
      <button class="btn sm" title="Opens Logs & crashes on the thread dump" onclick={() => seeWhy(dump)}>See why →</button>
      <button class="btn sm ghost" disabled={busy} title="Hides this and asks again in 5 minutes if it's still stuck" onclick={wait}>Wait</button>
    {/snippet}
  </Notice>
{/if}

{#snippet stuckDetails()}
  {#if top}
    <span class="what" title={top.why}>
      <span class="mod">{#if mod?.icon}<img src={mod.icon} alt="" width="16" height="16" />{:else}<span class="ph"></span>{/if}<b>{topName}</b></span>
      <span class="chip {top.certainty === 'Likely' ? 'warn' : ''}">{top.certainty}</span>
    </span>
    <span class="small muted">{top.why}</span>
  {/if}
{/snippet}

<style>
  .what { display: inline-flex; align-items: center; gap: 8px; min-width: 0; }
  .mod { display: inline-flex; align-items: center; gap: 6px; }
  .mod b { font-weight: 600; }
  .mod img, .ph { width: 16px; height: 16px; border-radius: var(--radius-sm); background: var(--raise); flex: none; display: inline-block; }
</style>
