<script>
  // The one question before a bulk start / stop / restart / command on the
  // servers picked in the list (lib/bulk.js), and then each server's result
  // in the same place: grouped by panel, what will be sent, what is skipped
  // and why. Players online on servers that would stop need a tick first
  // (a tick box and the button saying how many, never typing a name).
  import { tick, untrack } from 'svelte';
  import { Check, X, Minus, LoaderCircle, Play, Square, RotateCw, Terminal } from '@lucide/svelte';
  import { post } from './lib/api.js';
  import { planBulk, runBulk, counts, tally, title, okLabel } from './lib/bulk.js';
  import { whoOnline } from './lib/consoleKit.js';
  import Avatar from './Avatar.svelte';

  /** groups: [{ key, name, panel, role, state, servers }] (lib/bulk.js planBulk). */
  let { action, groups, onClose } = $props();

  // Planned once, when the question opens: the list moving underneath
  // (a server stopping by itself) doesn't change what was asked.
  const plan = untrack(() => planBulk(groups, action));
  const n = counts(plan);
  let line = $state('');
  let disconnect = $state(false);
  let phase = $state('ask'); // ask | sending | done
  let results = $state({});
  let field = $state();
  let okBtn = $state();
  let box = $state();

  const ready = $derived(n.send > 0 && (action !== 'command' || line.trim()) && (!n.online || disconnect));
  const done = $derived(tally(results));
  const ICON = { start: Play, stop: Square, restart: RotateCw, command: Terminal };
  const Icon = untrack(() => ICON[action]);

  tick().then(() => (untrack(() => action) === 'command' ? field : okBtn)?.focus());

  function send(panel, server, act, text) {
    const id = encodeURIComponent(server.id);
    return act === 'command' ? post(`/servers/${id}/command`, { line: text }, { panel }) : post(`/servers/${id}/power`, { action: act }, { panel });
  }

  async function go(e) {
    e.preventDefault();
    if (!ready || phase !== 'ask') return;
    phase = 'sending';
    const pre = {};
    for (const g of plan) {
      pre[g.key] = {};
      for (const x of g.skip) pre[g.key][x.server.id] = { status: 'skipped', text: x.reason };
    }
    results = pre;
    await runBulk(plan, line.trim(), send, (g, id, r) => (results[g][id] = r));
    phase = 'done';
    await tick();
    okBtn?.focus();
  }

  const close = () => phase !== 'sending' && onClose();

  function keys(e) {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      close();
    } else if (e.key === 'Tab' && box) {
      const els = [...box.querySelectorAll('button:not(:disabled), input:not(:disabled)')];
      if (!els.length) return;
      const [first, last] = [els[0], els[els.length - 1]];
      if (!box.contains(document.activeElement)) {
        e.preventDefault();
        first.focus();
      } else if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    }
  }

  const res = (g, s) => results[g.key]?.[s.id];
  const summary = $derived(
    [done.ok && `${done.ok} done`, done.failed && `${done.failed} failed`, done.skipped && `${done.skipped} skipped`].filter(Boolean).join(' · '),
  );
</script>

<svelte:window onkeydowncapture={keys} />

<div class="dialog-back" role="presentation" onclick={(e) => e.target === e.currentTarget && close()}>
  <div class="box popover" role="dialog" aria-modal="true" aria-labelledby="ba-title" bind:this={box}>
    <form class="f" onsubmit={go}>
      <h3 id="ba-title">{phase === 'ask' ? title(action, n.send) : phase === 'sending' ? 'Sending…' : summary || 'Done'}</h3>
      {#if phase === 'ask'}
        <p class="small subtle">
          {#if !n.send}None of the picked servers can take this.{:else if plan.some((g) => g.node)}Each panel gets its own request and writes it in its own audit log; this panel writes its nodes' with the node named.{:else}Each panel gets its own request and writes it in its own audit log.{/if}
        </p>
      {/if}

      <div class="groups">
        {#each plan as g (g.key)}
          <section class="pg" aria-label={g.name}>
            <div class="pgh">
              <span class="pgn">{g.name}{#if g.node}<span class="tiny muted">{' · node'}</span>{/if}</span>
              <span class="pgc">{g.send.length ? `${g.send.length} to ${action === 'command' ? 'send' : action}` : ''}{g.send.length && g.skip.length ? ' · ' : ''}{g.skip.length ? `${g.skip.length} skipped` : ''}</span>
            </div>
            {#each g.send as x (x.server.id)}
              {@const r = res(g, x.server)}
              <div class="it" class:bad={r?.status === 'failed'}>
                <span class="st" class:ok={r?.status === 'ok'} class:no={r?.status === 'failed'}>
                  {#if r?.status === 'ok'}<Check size={13} />{:else if r?.status === 'failed'}<X size={13} />{:else if phase === 'sending'}<LoaderCircle size={13} class="spin" />{:else}<Icon size={12} />{/if}
                </span>
                <Avatar server={x.server} cls="sic" panel={g.panel} />
                <span class="nm">{x.server.name}</span>
                <span class="why" title={r?.text || x.note || (x.online.length ? whoOnline(x.online) : '')}>{r ? r.text : x.note || (x.online.length ? whoOnline(x.online) : '')}</span>
              </div>
            {/each}
            {#each g.skip as x (x.server.id)}
              <div class="it skip">
                <span class="st"><Minus size={13} /></span>
                <Avatar server={x.server} cls="sic" panel={g.panel} />
                <span class="nm">{x.server.name}</span>
                <span class="why" title={x.reason}>Skipped: {x.reason}</span>
              </div>
            {/each}
          </section>
        {/each}
      </div>

      {#if phase === 'ask' && action === 'command' && n.send}
        <div class="input"><Terminal size={13} /><input bind:this={field} bind:value={line} placeholder="say Restarting in 5 minutes" aria-label="Command" spellcheck="false" autocomplete="off" /></div>
      {/if}
      {#if phase === 'ask' && n.online}
        <label class="row small tk"><input type="checkbox" bind:checked={disconnect} />{n.online === 1 ? '1 player is' : `${n.online} players are`} online. {action === 'stop' ? 'Stop' : 'Restart'} anyway</label>
      {/if}

      <div class="row end">
        {#if phase === 'ask'}
          <button type="button" class="btn" onclick={close}>Cancel</button>
          <button class="btn {action === 'start' ? 'go' : 'pri'}" bind:this={okBtn} disabled={!ready}>{okLabel(action, n.send)}</button>
        {:else}
          <button type="button" class="btn" bind:this={okBtn} disabled={phase === 'sending'} onclick={close}>Done</button>
        {/if}
      </div>
    </form>
  </div>
</div>

<style>
  .dialog-back { z-index: 80; }
  .box { width: min(480px, 100%); padding: 18px; }
  .f { display: flex; flex-direction: column; gap: 12px; }
  .box h3 { margin: 0; font-size: 15px; font-weight: 600; }
  .f p { margin: 0; }
  .groups { display: flex; flex-direction: column; gap: 10px; }
  .pg { display: flex; flex-direction: column; gap: 2px; }
  .pgh { display: flex; align-items: baseline; gap: 8px; padding: 0 2px 4px; border-bottom: 1px solid var(--line); margin-bottom: 2px; }
  .pgn { font-weight: 600; font-size: 13px; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .pgc { margin-left: auto; flex: none; font-size: 11.5px; color: var(--muted); }
  .it { display: flex; align-items: center; gap: 8px; min-width: 0; padding: 5px 2px; font-size: 12.5px; }
  .st { width: 16px; flex: none; display: grid; place-items: center; color: var(--muted); }
  .st.ok { color: var(--green); }
  .st.no { color: var(--red); }
  .nm { flex: none; max-width: 45%; font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .why { flex: 1; min-width: 0; text-align: right; font-size: 11.5px; color: var(--muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  /* Why it failed or was skipped is the point of the row: wrap, never cut. */
  .it.bad .why, .it.skip .why { white-space: normal; }
  .it.bad .why { color: var(--red); }
  .it.skip .nm { color: var(--subtle); }
  .tk { gap: 8px; align-items: center; }
  .tk input { accent-color: var(--acc); }
  .end { justify-content: flex-end; }
  .groups :global(.spin) { animation: spin 1s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
</style>
