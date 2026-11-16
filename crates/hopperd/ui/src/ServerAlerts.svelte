<script>
  // A server's alerts and TPS, in three spots on its page: a TPS chip in the
  // header (`part="chip"`), notices in the page's notice slot ("banner")
  // and a TPS line in the Live card ("live").
  import { onMount } from 'svelte';
  import { Share2, Terminal } from '@lucide/svelte';
  import Notice from './lib/Notice.svelte';
  import { mainClass } from './lib/noticeorder.js';
  import ShareLog from './ShareLog.svelte';
  import { alerts, alertsFor, watchAlerts, dismissAlert, refreshAlerts } from './lib/alerts.svelte.js';
  import { post, target } from './lib/api.js';
  import { tpsLevel } from './lib/format.js';
  import { say } from './lib/toast.svelte.js';

  let { server, part = 'chip' } = $props();
  // Alerts are this panel's own and its nodes' (merged by the panel under
  // the namespaced ids); another panel's server shows none here (yet).
  const remote = !!target.panel;
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
    spark_console: 'spark answers too late over RCON; TPS shows when spark next prints its report in the console.',
  };
  const FROM = { console: 'From spark’s report in the console', rcon: 'From the server’s TPS command (RCON)' };
  const tpsTitle = (t) => [FROM[t.source], t.mspt != null ? `${t.mspt.toFixed(1)} ms a tick` : ''].filter(Boolean).join(' · ');
  let turning = $state(false);
  // Turned on from this page: says so until the page changes server.
  let turnedOn = $state('');
  let sharing = $state(false);

  const md = (t) => t.replace(/\*\*/g, '');

  async function rcon() {
    turning = true;
    try {
      const r = await post(`/servers/${encodeURIComponent(id)}/rcon`);
      say(r.restart ? `RCON is on (port ${r.port}). TPS shows after the next restart.` : `RCON is on (port ${r.port}). TPS shows once it runs.`);
      turnedOn = id;
      refreshAlerts();
    } catch (e) {
      say(e.message, true);
    } finally {
      turning = false;
    }
  }

  onMount(watchAlerts);
</script>

{#snippet shareCrash(fit)}
  <button class={mainClass(fit)} onclick={() => (sharing = true)}><Share2 size={13} />{fit.narrow ? 'Share' : 'Share crash report'}</button>
{/snippet}

{#if remote}{:else if part === 'chip'}
  {#if running && tps}
    <span class="chip tps {level}" title={tpsTitle(tps) || 'Ticks per second'}>TPS {tps.tps.toFixed(1)}</span>
  {:else if running}
    <span class="chip tps" title={WHY[why] || 'Reading TPS…'}>TPS —</span>
  {/if}
{:else if part === 'banner'}
  {#if turnedOn && turnedOn === id}
    <Notice id="rcon-on-{id}" tier="info" icon={Terminal} title="RCON is on" subject={running ? 'It starts with the server’s next restart' : 'It starts with the server'} onclose={() => (turnedOn = '')} />
  {:else if server.rcon === false}
    <!-- Older panels don't send `rcon`: no notice for them. -->
    <Notice id="rcon-off-{id}" tier="info" icon={Terminal} title="Console commands and live player info need RCON" subject="Applies on restart">
      {#snippet primary(fit)}
        <button class={mainClass(fit)} disabled={turning} onclick={rcon} title="Gives it its own port and a random password. Never restarts the server.">{turning ? 'Turning on…' : 'Turn on'}</button>
      {/snippet}
    </Notice>
  {/if}
  {#each shown as a (a.kind + a.server_id + a.since)}
    <!-- A crash also has its own notice (CrashStrip); this one shares it. -->
    <Notice id="alert-{a.kind}-{a.server_id}-{a.since}" tier="choice" title={md(a.text)} subject={new Date(a.since * 1000).toLocaleTimeString()} since={a.since} onclose={() => dismissAlert(a)} primary={a.kind === 'crash' ? shareCrash : null} />
  {/each}
  {#if sharing}<ShareLog {server} crash onClose={() => (sharing = false)} />{/if}
{:else if part === 'live'}
  {#if running}
    <div class="t gap">TPS
      {#if tps}<b class:w={level === 'warn'} class:r={level === 'bad'} title={tpsTitle(tps)}>{tps.tps.toFixed(1)}{tps.mspt != null ? ` · ${tps.mspt.toFixed(1)} ms` : ''}</b>
      {:else if why === 'restart'}<b class="muted" title={WHY.restart}>RCON on next restart</b>
      {:else if why === 'rcon_off'}<button class="lnk" disabled={turning} onclick={rcon} title="Gives it a random RCON password; takes effect at the next restart">Turn on RCON to see it</button>
      {:else if why === 'unsupported'}<b class="muted" title="No cheap way to read TPS on this loader (add spark to get it)">n/a</b>
      {:else if why === 'spark_console'}<b class="muted" title={WHY.spark_console}>Waiting for spark</b>
      {:else if why === 'slow'}<b class="muted" title="The server didn't answer within 20 seconds: it's lagging hard (each tick is taking seconds)">No answer</b>
      {:else}<b class="muted">…</b>{/if}
    </div>
  {/if}
{/if}

<style>
  .chip.tps { flex: none; font-variant-numeric: tabular-nums; }
  .chip.good { color: var(--ink-green); background: var(--green-dim); }
  .chip.warn { color: var(--ink-gold); background: var(--gold-dim); }
  .chip.bad { color: var(--ink-red); background: var(--red-dim); }
  .t { display: flex; justify-content: space-between; font-size: 11.5px; color: var(--muted); }
  .t b { color: var(--text); font-weight: 500; }
  .t b.w { color: var(--gold); } .t b.r { color: var(--red-text); }
  .gap { margin-top: 10px; }
  .lnk { color: var(--acc); font-size: inherit; padding: 0; }
</style>
