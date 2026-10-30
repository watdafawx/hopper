<script>
  // A server's alerts and TPS, in three spots on its page: a TPS chip in the
  // header (`part="chip"`), banners under it ("banner") and a TPS line in
  // the Live card ("live").
  import { onMount } from 'svelte';
  import { AlertTriangle, X, Share2 } from '@lucide/svelte';
  import ShareLog from './ShareLog.svelte';
  import { alerts, alertsFor, watchAlerts, dismissAlert, refreshAlerts } from './lib/alerts.svelte.js';
  import { post } from './lib/api.js';
  import { tpsLevel } from './lib/format.js';
  import { say } from './lib/toast.svelte.js';

  let { server, part = 'chip' } = $props();
  const id = $derived(server.id);
  const tps = $derived(alerts.tps[id]);
  const running = $derived(server.state === 'running');
  const low = $derived(alerts.active.some((a) => a.server_id === id && a.kind === 'tps'));
  const shown = $derived(alertsFor(id).filter((a) => !a.dismissed));
  const why = $derived(alerts.no_tps[id]);
  // A low-TPS alert colours it red even while a reading drifts back up.
  const level = $derived(low ? 'bad' : tps ? tpsLevel(tps.tps) : '');
  const WHY = {
    rcon_off: 'TPS needs RCON: turn it on in the server’s Live card, or for every server in Settings › Alerts.',
    restart: 'RCON is set up and starts with the server’s next restart. TPS shows then.',
    unsupported: 'No cheap way to read TPS on this loader (add spark to get it).',
    slow: 'The server didn’t answer within 20 seconds: it’s lagging hard (each tick is taking seconds).',
  };
  let turning = $state(false);
  let sharing = $state(false);

  const md = (t) => t.replace(/\*\*/g, '');

  async function rcon() {
    turning = true;
    try {
      const r = await post(`/servers/${encodeURIComponent(id)}/rcon`);
      say(r.restart ? `RCON is on (port ${r.port}). TPS shows after the next restart.` : `RCON is on (port ${r.port}). TPS shows once it runs.`);
      refreshAlerts();
    } catch (e) {
      say(e.message, true);
    } finally {
      turning = false;
    }
  }

  onMount(watchAlerts);
</script>

{#if part === 'chip'}
  {#if running && tps}
    <span class="chip tps {level}" title={tps.mspt != null ? `${tps.mspt.toFixed(1)} ms a tick` : 'Ticks per second'}>TPS {tps.tps.toFixed(1)}</span>
  {:else if running}
    <span class="chip tps" title={WHY[why] || 'Reading TPS…'}>TPS —</span>
  {/if}
{:else if part === 'banner'}
  {#each shown as a (a.kind + a.server_id + a.since)}
    <div class="ban" class:red={a.kind === 'crash'}>
      <AlertTriangle size={14} />
      <span class="grow">{md(a.text)}</span>
      <span class="tiny muted">{new Date(a.since * 1000).toLocaleTimeString()}</span>
      {#if a.kind === 'crash'}<button class="btn sm" onclick={() => (sharing = true)}><Share2 size={13} />Share crash report</button>{/if}
      <button class="btn sm ghost icon" title="Dismiss" onclick={() => dismissAlert(a)}><X size={13} /></button>
    </div>
  {/each}
  {#if sharing}<ShareLog {server} crash onClose={() => (sharing = false)} />{/if}
{:else if part === 'live'}
  {#if running}
    <div class="t gap">TPS
      {#if tps}<b class:w={level === 'warn'} class:r={level === 'bad'}>{tps.tps.toFixed(1)}{tps.mspt != null ? ` · ${tps.mspt.toFixed(1)} ms` : ''}</b>
      {:else if why === 'restart'}<b class="muted" title={WHY.restart}>RCON on next restart</b>
      {:else if why === 'rcon_off'}<button class="lnk" disabled={turning} onclick={rcon} title="Gives it a random RCON password; takes effect at the next restart">Turn on RCON to see it</button>
      {:else if why === 'unsupported'}<b class="muted" title="No cheap way to read TPS on this loader (add spark to get it)">n/a</b>
      {:else if why === 'slow'}<b class="muted" title="The server didn't answer within 20 seconds: it's lagging hard (each tick is taking seconds)">No answer</b>
      {:else}<b class="muted">…</b>{/if}
    </div>
  {/if}
{/if}

<style>
  .chip.tps { flex: none; font-variant-numeric: tabular-nums; }
  .chip.good { color: var(--green); background: var(--green-dim); }
  .chip.warn { color: var(--gold); background: var(--gold-dim); }
  .chip.bad { color: var(--red); background: var(--red-dim); }
  .ban { display: flex; align-items: center; gap: 10px; margin: 0 18px 10px; padding: 7px 12px; border-radius: 8px; background: var(--gold-dim); border: 1px solid #eab30840; color: var(--text); font-size: 12.5px; }
  .ban.red { background: var(--red-dim); border-color: #ef444440; }
  .ban :global(svg) { color: var(--gold); flex: none; }
  .ban.red :global(svg) { color: var(--red); }
  .grow { flex: 1; min-width: 0; }
  .t { display: flex; justify-content: space-between; font-size: 11.5px; color: var(--muted); }
  .t b { color: var(--text); font-weight: 500; }
  .t b.w { color: var(--gold); } .t b.r { color: var(--red); }
  .gap { margin-top: 10px; }
  .lnk { color: var(--acc); font-size: inherit; padding: 0; }
</style>
