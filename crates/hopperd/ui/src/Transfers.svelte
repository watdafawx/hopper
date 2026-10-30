<script>
  import { ArrowUp, Pause, Play, X, ChevronDown, ChevronUp, Check, RefreshCw } from '@lucide/svelte';
  import { transfers, pause, resume, cancel, clearDone, replace } from './lib/transfers.svelte.js';
  import { bytes, routeKind } from './lib/format.js';
  import { live } from './lib/live.svelte.js';

  const active = $derived(transfers.list.filter((t) => t.state === 'running' || t.state === 'queued'));
  const total = $derived(active.reduce((n, t) => n + t.speed, 0));
  const kind = $derived(routeKind(live.via));

  function eta(t) {
    if (!t.speed) return '';
    const s = Math.round((t.size - t.done) / t.speed);
    return s < 60 ? `${s} s left` : `${Math.round(s / 60)} min left`;
  }
</script>

{#if transfers.list.length}
  <div class="panel">
    <button class="hd" onclick={() => (transfers.open = !transfers.open)}>
      <b>Transfers</b>
      {#if active.length}<span class="small muted">{active.length} active · {bytes(total)}/s</span>{/if}
      <span class="sp"></span>
      <span class="chip lan">{kind}</span>
      {#if transfers.open}<ChevronDown size={14} />{:else}<ChevronUp size={14} />{/if}
    </button>
    {#if transfers.open}
      <div class="items">
        {#each transfers.list as t (t.id)}
          <div class="q">
            <div class="row small">
              {#if t.state === 'done'}<Check size={13} color="var(--green)" />{:else}<ArrowUp size={13} color="var(--acc)" />{/if}
              <b class="nm" title={t.path}>{t.path}</b>
              <span class="muted nw">{bytes(t.done)} / {bytes(t.size)}</span>
            </div>
            <div class="bar" class:g={t.state === 'done'}><i style:width="{t.size ? (t.done / t.size) * 100 : 100}%" class:dim={t.state === 'paused' || t.state === 'failed'}></i></div>
            <div class="row tiny muted">
              {#if t.state === 'running'}<span>{bytes(t.speed)}/s</span><span class="sp"></span><span>{eta(t)}</span>
              {:else if t.state === 'failed'}<span class="err ell">{t.error}</span><span class="sp"></span>
              {:else}<span>{{ queued: 'Waiting', paused: 'Paused — resumes where it stopped', done: 'Done' }[t.state]}</span><span class="sp"></span>{/if}
              {#if t.state === 'running' && t.error}<span class="ell">{t.error}</span>{/if}
              {#if t.state === 'running' || t.state === 'queued'}
                <button class="btn sm icon ghost" title="Pause" onclick={() => pause(t.id)}><Pause size={12} /></button>
              {:else if t.state === 'paused'}
                <button class="btn sm icon ghost" title="Resume" onclick={() => resume(t.id)}><Play size={12} /></button>
              {:else if t.state === 'failed'}
                {#if t.error.endsWith('already exists.')}
                  <button class="btn sm ghost" onclick={() => replace(t.id)}>Replace</button>
                {:else}
                  <button class="btn sm icon ghost" title="Retry" onclick={() => resume(t.id)}><RefreshCw size={12} /></button>
                {/if}
              {/if}
              {#if t.state !== 'done'}<button class="btn sm icon ghost" title="Cancel" onclick={() => cancel(t.id)}><X size={12} /></button>{/if}
            </div>
          </div>
        {/each}
      </div>
      {#if transfers.list.some((t) => t.state === 'done')}
        <button class="btn sm ghost clear" onclick={clearDone}>Clear finished</button>
      {/if}
    {/if}
  </div>
{/if}

<style>
  .panel { position: fixed; right: 16px; bottom: 16px; width: min(360px, calc(100vw - 32px)); background: var(--surface); border: 1px solid var(--line2); border-radius: 10px; box-shadow: 0 16px 40px #000a; z-index: 40; overflow: hidden; }
  .hd { display: flex; align-items: center; gap: 8px; width: 100%; padding: 10px 12px; text-align: left; }
  .items { max-height: 50vh; overflow: auto; border-top: 1px solid var(--line); }
  .q { padding: 9px 12px; border-bottom: 1px solid var(--line); display: flex; flex-direction: column; gap: 5px; }
  .nm { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-weight: 500; }
  .nw { white-space: nowrap; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .dim { background: var(--muted) !important; }
  .clear { margin: 6px 8px; }
</style>
