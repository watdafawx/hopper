<script>
  // A start or stop under way: bar, what it is doing, the time. One look
  // for the server header, the server list and the Overview cards
  // (desktop Hopper shows the same in its server card and header).
  // A stop that has gone quiet offers Kill, behind the usual confirm.
  import { askPower } from './lib/consoleKit.js';
  import { progressSteps } from './lib/format.js';

  /** server: a summary with `progress`; compact: one short line under the bar. */
  let { server, compact = false } = $props();
  const p = $derived(server.progress);
  const tone = $derived(p?.stuck ? 'bad' : p?.kind === 'stop' ? 'stop' : p?.slow ? 'stop' : 'start');
</script>

{#if p}
  <div class="prog" class:compact title={progressSteps(p)}>
    {#if !compact}
      <div class="top">
        <span class="step">{p.label}</span>
        {#if p.stuck}<button class="btn sm bad" onclick={(e) => { e.preventDefault(); e.stopPropagation(); askPower(server, 'kill'); }}>Kill…</button>{/if}
      </div>
    {/if}
    <div class="bar {tone}" role="progressbar" aria-valuenow={p.percent} aria-valuemin="0" aria-valuemax="100" aria-label={p.label}><i style:width="{p.percent}%"></i></div>
    <div class="tiny muted txt">{compact ? `${p.label} · ${p.short}` : p.detail}</div>
  </div>
{/if}

<style>
  .prog { display: flex; flex-direction: column; gap: 5px; min-width: 0; max-width: 520px; margin-top: 7px; }
  .prog.compact { gap: 3px; margin-top: 4px; }
  .top { display: flex; align-items: center; gap: 8px; min-width: 0; }
  .step { font-size: 13px; color: var(--text); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: 1; }
  .compact .bar { height: 3px; }
  .bar.start i { background: var(--blue); }
  .bar.stop i { background: var(--gold); }
  .bar.bad i { background: var(--red); }
  .txt { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
</style>
