<script>
  // Finding the mod that breaks the server, as in Hopper desktop: the
  // start card, then a strip with the round, its answers and the result.
  // On a server Hopper can run the rounds itself when it won't start or
  // crashes: start, wait for "Done", answer, repeat.
  import { ScanSearch, Play, Pause, Undo2, RotateCcw, Check, Target, TriangleAlert, RefreshCw } from '@lucide/svelte';
  import { post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { confirmIt } from './lib/dialog.svelte.js';
  import { followJob } from './lib/jobs.js';

  let { base, server, st, setup = $bindable(null), onChanged = () => {} } = $props();
  let busy = $state('');
  let autoRun = $state(true);
  let scratch = $state(true);
  let now = $state(Date.now());

  $effect(() => {
    const t = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(t);
  });

  const h = $derived(st?.hunt);
  const crash = $derived(h ? h.kind === 'crash' : setup === 'crash');
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));
  const players = $derived(server.players?.length ?? st?.players ?? 0);
  const roundsGuess = $derived(Math.max(1, Math.ceil(Math.log2(Math.max(2, st?.on || 2)))) + 1);
  const mmss = (s) => `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
  const elapsed = $derived(st?.round_started ? Math.max(0, Math.round(now / 1000 - st.round_started)) : null);

  async function job(label, path, body = {}) {
    busy = label;
    try {
      const r = await post(`${base}/mods/hunt/${path}`, body);
      const res = r.job ? await followJob(r.job, (s) => (busy = s.detail || label)) : r;
      if (res?.message) say(res.message);
      return true;
    } catch (e) {
      say(e.message, true);
      return false;
    } finally {
      busy = '';
      onChanged();
    }
  }

  async function start() {
    if (autoRun && crash && players && !(await confirmIt(`${players} player${players === 1 ? ' is' : 's are'} online`, { body: 'Each round restarts the server, so they get kicked every few minutes until the hunt ends.', ok: 'Start anyway' }))) return;
    if (await job('Getting round 1 ready', 'start', { kind: setup, auto: autoRun && crash, scratch_world: scratch })) setup = null;
  }

  const answer = (a) => job('Next round', 'answer', { answer: a });
  const undo = () => job('Taking back the last answer', 'undo');
  const finish = (keep) => job(keep ? 'Restoring every mod but the culprit' : 'Restoring every mod', 'finish', { keep_off: keep });

  async function auto(run) {
    if (run && players && !(await confirmIt(`${players} player${players === 1 ? ' is' : 's are'} online`, { body: 'Each round restarts the server, so they get kicked.', ok: 'Run rounds' }))) return;
    try {
      await post(`${base}/mods/hunt/auto`, { run });
    } catch (e) {
      say(e.message, true);
    }
    onChanged();
  }

  async function restart() {
    busy = alive ? 'Restarting the server' : 'Starting the server';
    try {
      await post(`/servers/${encodeURIComponent(server.id)}/power`, { action: alive ? 'restart' : 'start' });
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = '';
    }
  }
</script>

{#if busy && !h}
  <div class="strip"><div class="t"><span class="spin"></span>{busy}</div><div class="note">Hopper saves which mods are on, reads what each one needs, then turns the first half off.</div></div>
{:else if !h && setup}
  <div class="strip">
    <div class="t">
      <ScanSearch size={15} class="ac" />Find the mod that breaks it
      <span class="sp"></span>
      <button class="btn sm ghost" onclick={() => (setup = null)}>Cancel</button>
      <button class="btn sm pri" disabled={!!busy} onclick={start}><Play size={13} />Start</button>
    </div>
    <div class="note">{st?.on ?? 'The'} mods are on. Each round Hopper turns about half of them off (always with the mods that need them) and you say whether it's still broken. About {roundsGuess} rounds. Stop &amp; restore puts every mod back exactly as it is now.</div>
    <div class="row wrap">
      <span class="small">What goes wrong?</span>
      <button class="fchip" class:on={setup === 'crash'} onclick={() => (setup = 'crash')}>It won't start / crashes</button>
      <button class="fchip" class:on={setup === 'misbehaves'} onclick={() => (setup = 'misbehaves')}>It misbehaves (no crash)</button>
    </div>
    {#if crash}
      <label class="cbl"><input type="checkbox" bind:checked={autoRun} /><span><b>Run rounds for me</b> <span class="muted">· Hopper restarts the server each round and counts reaching “Done” as working. Up to {st?.timeout_minutes ?? 10} min a round.</span></span></label>
      {#if autoRun && players}<div class="small warn">{players} player{players === 1 ? ' is' : 's are'} online: every round kicks them.</div>{/if}
    {/if}
    <label class="cbl"><input type="checkbox" bind:checked={scratch} /><span><b>Test on a throwaway world</b> <span class="muted">· your world isn't touched. With it off, a world saved while mods are off loses their blocks: back it up first.</span></span></label>
  </div>
{:else if h && h.phase === 'found'}
  <div class="strip ok">
    <div class="t">
      <Target size={15} class="okc" /><span class="ell">Found it: {h.culprits.join(', ')}</span>
      <span class="sp"></span>
      {#if h.rounds}<button class="btn sm ghost" disabled={!!busy} onclick={undo}><Undo2 size={13} />Undo answer</button>{/if}
      <button class="btn sm" disabled={!!busy} title="Every mod back on or off as before the hunt" onclick={() => finish(false)}><RotateCcw size={13} />Restore everything</button>
      <button class="btn sm pri" disabled={!!busy} onclick={() => finish(true)}><Check size={13} />Keep it off, restore the rest</button>
    </div>
    <div class="note">After {h.rounds} round{h.rounds === 1 ? '' : 's'}. {h.culprits.length === 1 ? "It's" : "They're"} off now and everything else is back on, so the server should start.{#if h.along.length} Off with it, because they need it: {h.along.slice(0, 4).join(', ')}{h.along.length > 4 ? ` and ${h.along.length - 4} more` : ''}.{/if}{#if st.scratch_world} Ending the hunt brings your world back.{/if}</div>
    {#if busy}<div class="small busyc row"><span class="spin"></span>{busy}</div>{/if}
  </div>
{:else if h && h.phase === 'not_found'}
  <div class="strip wn">
    <div class="t">
      <TriangleAlert size={15} class="wc" />No single mod breaks it
      <span class="sp"></span>
      {#if h.rounds}<button class="btn sm ghost" disabled={!!busy} onclick={undo}><Undo2 size={13} />Undo answer</button>{/if}
      <button class="btn sm pri" disabled={!!busy} onclick={() => finish(false)}><RotateCcw size={13} />Restore everything</button>
    </div>
    <div class="note">It stayed broken with every suspect off. The cause is likely a config, the world, Java or the loader, or mods that only break together.</div>
    {#if busy}<div class="small busyc row"><span class="spin"></span>{busy}</div>{/if}
  </div>
{:else if h}
  <div class="strip">
    <div class="t">
      <ScanSearch size={15} class="ac" />Finding the {crash ? 'crash' : 'problem'} · round {h.round} of ~{h.estimate}
      <span class="chip starting">{h.parked} parked</span>
      <span class="chip running">{h.cleared} cleared</span>
      <span class="sp"></span>
      {#if st.active}
        <button class="btn sm" onclick={() => auto(false)} title="Hopper stops after this check; you can answer yourself"><Pause size={13} />Pause</button>
        <button class="btn sm bad" disabled={!!busy} onclick={() => finish(false)}>Stop &amp; restore</button>
      {:else}
        {#if h.rounds}<button class="btn sm ghost" disabled={!!busy} onclick={undo}><Undo2 size={13} />Undo answer</button>{/if}
        <button class="btn sm bad" disabled={!!busy} title="Ends the hunt: every mod back on or off as before it" onclick={() => finish(false)}>Stop &amp; restore</button>
        {#if crash}<button class="btn sm" disabled={!!busy} title="Hopper restarts the server each round and answers by itself" onclick={() => auto(true)}><Play size={13} />Run rounds for me</button>{/if}
        <button class="btn sm" disabled={!!busy || server.state === 'stopping'} onclick={restart}><RefreshCw size={13} />{alive ? 'Restart' : 'Start'} to test</button>
        <button class="btn sm" disabled={!!busy} onclick={() => answer('broken')}>{crash ? 'Still crashes' : 'Still broken'}</button>
        <button class="btn sm pri" disabled={!!busy} onclick={() => answer('works')}>Works now</button>
      {/if}
    </div>
    <div class="bar"><i style:width="{Math.max(2, Math.min(100, (h.rounds / h.estimate) * 100))}%"></i></div>
    {#if st.active}
      <div class="note">Hopper runs the rounds: {elapsed != null ? `round ${h.round} starting, ${mmss(elapsed)} so far` : 'between rounds'}{st.last_round_secs ? ` (the last one took ${mmss(st.last_round_secs)})` : ''}. You can leave this page.</div>
    {:else if st.paused}
      <div class="small warn">Hopper's rounds paused: {st.paused}. Answer yourself, or run them again.</div>
    {:else if h.all_suspects_off}
      <div class="note">Checking: all {h.suspects} suspects are off this round. If it works, they hold the cause.</div>
    {:else if h.phase === 'libraries'}
      <div class="note">The other mods are cleared: now testing libraries, the mods others need (with them off, what needs them is off too).</div>
    {:else}
      <div class="note">{h.suspects} suspects left. {alive ? 'Watch the console' : 'Start the server'}, then say whether it still {crash ? 'crashes' : 'misbehaves'}. Answering stops the server so its mods can change.</div>
    {/if}
    {#if st.scratch_world}<div class="note">Rounds play on a throwaway world; yours comes back when the hunt ends.</div>{/if}
    {#if st.log?.length}<div class="log">{#each st.log.slice(-4) as l, i (i)}<div>{l}</div>{/each}</div>{/if}
    {#if busy}<div class="small busyc row"><span class="spin"></span>{busy}</div>{/if}
  </div>
{/if}

<style>
  .strip { border: 1px solid #f9731655; background: #f973160d; border-radius: 10px; padding: 10px 12px; display: flex; flex-direction: column; gap: 8px; }
  .strip.ok { border-color: #22c55e55; background: #22c55e0d; }
  .strip.wn { border-color: #eab30855; background: #eab3080d; }
  .t { font-weight: 600; display: flex; align-items: center; gap: 8px; flex-wrap: wrap; min-width: 0; }
  .t :global(.ac) { color: var(--acc); }
  .t :global(.okc) { color: var(--green); }
  .t :global(.wc) { color: var(--gold); }
  .t .chip { font-weight: 500; }
  .note { color: var(--subtle); font-size: 12.5px; }
  .warn { color: var(--gold); }
  .busyc { color: var(--gold); }
  .wrap { flex-wrap: wrap; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .fchip { padding: 3px 10px; border-radius: 99px; border: 1px solid transparent; color: var(--subtle); font-size: 12px; }
  .fchip:hover { background: #ffffff0a; }
  .fchip.on { background: var(--raise); color: var(--text); }
  .cbl { display: flex; align-items: flex-start; gap: 8px; font-size: 12.5px; cursor: pointer; }
  .cbl input { width: 15px; height: 15px; accent-color: var(--acc); margin-top: 1px; flex: none; }
  .log { font: 11.5px var(--mono); background: var(--term); border: 1px solid var(--line); border-radius: 6px; padding: 6px 8px; color: var(--subtle); }
  .spin { width: 11px; height: 11px; border: 2px solid var(--gold); border-right-color: transparent; border-radius: 50%; animation: s 0.8s linear infinite; flex: none; display: inline-block; }
  @keyframes s { to { transform: rotate(360deg); } }
</style>
