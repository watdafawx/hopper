<script>
  // Move or copy to… (design/multi-panel.md, A5). One dialog, like Clone:
  // where to, move or copy and the checks on one screen, so a failing check
  // sits next to the choice that caused it. Progress shows in the same box;
  // closing it lets the move go on (the server row shows it's sending), and
  // opening the dialog again on that server picks it back up.
  import { ArrowRightLeft, X, Check, TriangleAlert, CircleX, Square, RotateCw } from '@lucide/svelte';
  import { api, post, del } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { moveDialog, closeMove } from './lib/moveDialog.svelte.js';
  import { active, resumable, percent, amount, title, resumedLine, goLabel } from './lib/moves.js';
  import { power } from './lib/power.js';

  const server = $derived(moveDialog.server);
  let places = $state(null);
  let to = $state('');
  let mode = $state('move');
  let backups = $state(false);
  let check = $state(null);
  let checking = $state(false);
  let error = $state('');
  let move = $state(null);
  let starting = $state(false);
  let timer = null;
  let recheck = $state(0);
  let seq = 0;

  const place = $derived(places?.find((p) => p.id === to));
  const busyMove = $derived(!!move && (active(move) || resumable(move)));

  // A fresh open: a move already under way on this server shows its
  // progress; otherwise the places, with the first one that can take it.
  $effect(() => {
    const s = server;
    if (!s) return;
    reset();
    load(s);
    return () => clearTimeout(timer);
  });

  function reset() {
    places = null;
    to = '';
    mode = 'move';
    backups = false;
    check = null;
    error = '';
    move = null;
    starting = false;
    clearTimeout(timer);
  }

  async function load(s) {
    try {
      const all = await api('/moves', { poll: true });
      const open = all.filter((m) => m.server === s.id).sort((a, b) => b.started - a.started)[0];
      if (open && (active(open) || resumable(open))) {
        move = open;
        poll();
        return;
      }
      places = await api(`/moves/places?server=${encodeURIComponent(s.id)}`);
      to = places.find((p) => p.ok && !p.here)?.id ?? '';
    } catch (e) {
      error = e.message;
    }
  }

  // The checks run again whenever where, how or backups change.
  $effect(() => {
    if (!server || !to || move) return;
    const body = { server: server.id, to, mode, backups };
    recheck;
    const mine = ++seq;
    checking = true;
    error = '';
    post('/moves/precheck', body).then(
      (r) => mine === seq && ((check = r), (checking = false)),
      (e) => mine === seq && ((check = null), (error = e.message), (checking = false)),
    );
  });

  async function go() {
    if (!check?.ok || starting) return;
    starting = true;
    error = '';
    try {
      move = await post('/moves', { server: server.id, to, mode, backups });
      poll();
    } catch (e) {
      error = e.message;
    } finally {
      starting = false;
    }
  }

  function poll() {
    clearTimeout(timer);
    if (!move) return;
    const id = move.id;
    timer = setTimeout(async () => {
      try {
        const m = await api(`/moves/${encodeURIComponent(id)}`, { poll: true });
        if (move?.id !== id) return;
        move = m;
        if (m.state === 'done') say(`${title(m)}.`);
      } catch {
        // The panel didn't answer this time: ask again.
      }
      if (move && active(move)) poll();
    }, 700);
  }

  async function resume() {
    try {
      move = await post(`/moves/${encodeURIComponent(move.id)}/resume`);
      poll();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function cancelMove() {
    try {
      await post(`/moves/${encodeURIComponent(move.id)}/cancel`);
      move = await api(`/moves/${encodeURIComponent(move.id)}`, { poll: true });
      poll();
    } catch (e) {
      say(e.message, true);
    }
  }

  // Failed or cancelled: back to the choices, as they were.
  function again() {
    const m = move;
    move = null;
    if (!places) load(server);
    if (m) {
      to = m.to || 'main';
      mode = m.mode;
      backups = m.backups;
    }
  }

  // Stop asks first, as everywhere; then the checks run again until they
  // see it stopped (about half a minute at most).
  async function stopIt() {
    await power(server, 'stop');
    for (let i = 0; i < 20 && server && !move && check?.running; i++) {
      await new Promise((r) => setTimeout(r, 1500));
      recheck++;
    }
  }

  function openNew() {
    const id = move.new_id;
    closeMove();
    if (id) location.hash = `/server/${encodeURIComponent(id)}/console`;
  }

  function close() {
    if (move && !active(move) && !resumable(move)) del(`/moves/${encodeURIComponent(move.id)}`).catch(() => {});
    closeMove();
  }

  const ICON = { ok: Check, warn: TriangleAlert, fail: CircleX };
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && server && !starting && !e.defaultPrevented && (e.preventDefault(), close())} />

{#if server}
  <div class="dialog-back" role="presentation" onclick={(e) => e.target === e.currentTarget && !starting && close()}>
    <div class="box popover" role="dialog" aria-modal="true" aria-label="Move or copy {server.name}">
      {#if !move}
        <h3><ArrowRightLeft size={15} />Move or copy {server.name}<span class="sp"></span><button class="btn sm icon ghost" title="Close" onclick={close}><X size={14} /></button></h3>
        <span class="lbl">To</span>
        {#if !places}
          <div class="small muted">{error ? '' : 'Looking for places…'}</div>
        {:else}
          <div class="places" role="radiogroup" aria-label="Where to">
            {#each places as p (p.id)}
              <button class="place" role="radio" aria-checked={to === p.id} class:row-sel={to === p.id} class:row-hover={to !== p.id} disabled={!p.ok || p.here} onclick={() => (to = p.id)}>
                <span class="pn">{p.name}</span>
                <span class="pw tiny" class:okc={p.ok && !p.here}>{p.here ? 'Here now' : p.ok ? 'Online' : p.why}</span>
              </button>
            {/each}
          </div>
          {#if !places.some((p) => p.ok && !p.here)}<div class="small muted">No other place can take it. Add a node in Settings › Nodes, with Manage.</div>{/if}
        {/if}
        <div class="row wrap opts">
          <div class="seg"><button class:on={mode === 'move'} onclick={() => (mode = 'move')}>Move</button><button class:on={mode === 'copy'} onclick={() => (mode = 'copy')}>Copy</button></div>
          <label class="tick small"><input type="checkbox" bind:checked={backups} />Bring backups</label>
        </div>
        <div class="tiny muted">{mode === 'copy' ? 'Copy leaves this server as it is.' : 'Move keeps this copy, stopped and marked as moved. You can delete it later.'}</div>
        {#if to}
          <div class="checks" aria-live="polite">
            {#if check}
              {#each check.rows as r (r.key)}
                {@const Icon = ICON[r.level] ?? Check}
                <div class="ck {r.level}"><Icon size={13} /><span>{r.text}</span></div>
              {/each}
              {#if check.running}<div class="row"><button class="btn sm" onclick={stopIt}><Square size={12} />Stop {server.name}</button></div>{/if}
            {:else if checking}
              <div class="small muted">Checking…</div>
            {/if}
          </div>
        {/if}
        {#if error}<div class="err">{error}</div>{/if}
        <div class="row end"><span class="sp"></span><button class="btn" onclick={close}>Cancel</button><button class="btn pri" disabled={!check?.ok || checking || starting || !place} onclick={go}><ArrowRightLeft size={13} />{goLabel(mode, place)}</button></div>
      {:else}
        <h3><ArrowRightLeft size={15} />{title(move)}<span class="sp"></span><button class="btn sm icon ghost" title="Close" onclick={close}><X size={14} /></button></h3>
        {#if move.state !== 'done' && move.state !== 'failed' && move.state !== 'cancelled'}
          <div class="bar" class:busy={move.state === 'preparing' && !move.total} role="progressbar" aria-valuenow={percent(move)} aria-valuemin="0" aria-valuemax="100" aria-label={move.step}><i style:width="{percent(move)}%"></i></div>
          <div class="row small prog"><span class="stp" class:warn={move.state === 'waiting' || move.state === 'paused'}>{move.step}</span><span class="sp"></span><span class="muted amt">{amount(move)}</span></div>
        {:else}
          <div class="small" class:err={move.state === 'failed'} class:muted={move.state !== 'failed'}>{move.state === 'failed' ? move.error || move.step : move.step}</div>
        {/if}
        {#if move.state === 'failed'}<div class="tiny muted">Nothing was left on {move.to_name}. {server.name} is as it was.</div>{/if}
        {#if resumedLine(move)}<div class="tiny muted">{resumedLine(move)}</div>{/if}
        <div class="row end">
          <span class="sp"></span>
          {#if active(move)}
            <button class="btn" onclick={cancelMove}>Cancel {move.mode === 'copy' ? 'copy' : 'move'}</button>
            <button class="btn" onclick={close}>Close</button>
          {:else if resumable(move)}
            <button class="btn" onclick={cancelMove}>Cancel {move.mode === 'copy' ? 'copy' : 'move'}</button>
            <button class="btn pri" onclick={resume}><RotateCw size={13} />Resume</button>
          {:else if move.state === 'done'}
            <button class="btn" onclick={close}>Close</button>
            <button class="btn pri" disabled={!move.new_id} onclick={openNew}>Open it on {move.to_name}</button>
          {:else}
            <button class="btn" onclick={close}>Close</button>
            <button class="btn pri" onclick={again}><RotateCw size={13} />Try again</button>
          {/if}
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .box { width: min(500px, 100%); display: flex; flex-direction: column; gap: 8px; padding: 14px; }
  .box h3 { margin: 0 0 4px; display: flex; align-items: center; gap: 8px; font-size: 14px; min-width: 0; }
  .places { display: flex; flex-direction: column; border: 1px solid var(--line); border-radius: var(--radius); overflow: hidden; }
  .place { display: flex; align-items: center; gap: 10px; padding: 8px 10px; text-align: left; color: var(--text); min-width: 0; }
  .place + .place { border-top: 1px solid var(--line); }
  .place:disabled { color: var(--muted); cursor: default; }
  .pn { font-weight: 500; flex: none; }
  .pw { margin-left: auto; color: var(--muted); text-align: right; min-width: 0; }
  .pw.okc { color: var(--green); }
  .opts { gap: 12px; align-items: center; }
  .tick { display: flex; align-items: center; gap: 6px; color: var(--subtle); }
  .seg { display: flex; border: 1px solid var(--line2); border-radius: var(--radius); overflow: hidden; }
  .seg button { padding: 5px 11px; font-size: 12.5px; color: var(--subtle); }
  .seg button + button { border-left: 1px solid var(--line2); }
  .seg button.on { background: var(--acc); color: #1a1a1f; font-weight: 600; }
  .checks { display: flex; flex-direction: column; gap: 5px; border: 1px solid var(--line); background: var(--base); border-radius: var(--radius-lg); padding: 8px 10px; }
  .checks:empty { display: none; }
  .ck { display: flex; gap: 7px; align-items: flex-start; font-size: 12.5px; }
  .ck :global(svg) { flex: none; margin-top: 2px; }
  .ck.ok :global(svg) { color: var(--green); }
  .ck.warn :global(svg) { color: var(--gold); }
  .ck.fail :global(svg) { color: var(--red); }
  .prog { gap: 8px; min-width: 0; }
  .stp { min-width: 0; }
  .amt { flex: none; white-space: nowrap; }
  .stp.warn { color: var(--gold); }
  .end { margin-top: 6px; gap: 8px; }
</style>
