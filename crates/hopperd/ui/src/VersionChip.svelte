<script>
  // The version beside the logo, as in Hopper desktop's title bar: "v0.13.6"
  // opens the versions window. With an update out (owner) it's a split
  // pill: the solid half updates, the notes half opens the window on the
  // new release. While an update runs it says how far it got.
  import { RotateCcw, ScrollText, Download, RefreshCw, CircleCheck } from '@lucide/svelte';
  import { onDestroy } from 'svelte';
  import { post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { session } from './lib/me.svelte.js';
  import { upd, followUpdate, loadUpdate } from './lib/panelupdate.svelte.js';

  onDestroy(followUpdate());

  const st = $derived(upd.st);
  const canUpdate = $derived(session.owner && st && !st.why_not);
  const goal = $derived(st?.target || st?.latest || '');
  const busy = $derived(
    st?.state === 'downloading' ? { text: `Downloading ${goal}…`, icon: Download, tip: `Downloading Hopper Server ${goal}` }
      : st?.state === 'waiting' ? { text: `Installing ${goal}…`, icon: Download, tip: 'The panel restarts into it once running jobs finish' }
        : st?.state === 'restarting' ? { text: 'Restarting…', icon: RefreshCw, tip: `Restarting into Hopper Server ${goal}` }
          : st?.state === 'ready' ? { text: `${goal} on next start`, icon: CircleCheck, tip: `Hopper Server ${goal} is in place: it runs from the next start`, ok: true }
            : null,
  );

  async function update() {
    try {
      await post('/panel/update/install', { version: st.latest });
      st.state = 'downloading';
      st.target = st.latest;
      loadUpdate();
    } catch (e) {
      say(e.message, true);
    }
  }
</script>

{#if st}
  {#if busy}
    {@const I = busy.icon}
    <button class="vstate" class:ok={busy.ok} title={busy.tip} onclick={() => (upd.open = true)}>
      <I size={12} />{busy.text}
    </button>
  {:else if st.latest && canUpdate}
    <span class="split">
      <button class="go" title="Download Hopper Server {st.latest} and restart into it. Minecraft servers keep running." onclick={update}><RotateCcw size={12} />Update to {st.latest}</button>
      <button class="nt" title="What's new in {st.latest}" aria-label="What's new in {st.latest}" onclick={() => (upd.open = true)}><ScrollText size={12} /></button>
    </span>
  {:else}
    <button class="ver" title={session.owner ? 'Hopper Server versions: update or roll back' : 'Hopper Server versions'} onclick={() => (upd.open = true)}>v{st.current}</button>
  {/if}
{/if}

<style>
  .ver { height: 22px; padding: 0 7px; border-radius: var(--radius); font-size: 11.5px; color: var(--muted); white-space: nowrap; }
  .ver:hover { background: var(--hover); color: var(--text); }
  .split { display: inline-flex; height: 22px; border: 1px solid #f9731673; border-radius: var(--radius); overflow: hidden; font-size: 11.5px; font-weight: 600; flex: none; }
  .split button { display: flex; align-items: center; gap: 4px; height: 100%; }
  .go { padding: 0 8px; background: var(--acc); color: #1c0d02; white-space: nowrap; }
  .go:hover { background: #fb8a3c; }
  .nt { padding: 0 6px; background: var(--surface); color: var(--acc); }
  .nt:hover { background: var(--raise); }
  .vstate { display: inline-flex; align-items: center; gap: 5px; height: 22px; padding: 0 8px; border-radius: var(--radius); font-size: 11.5px; font-weight: 600; white-space: nowrap; color: var(--blue); background: var(--blue-dim); border: 1px solid #38bdf84d; }
  .vstate:hover { background: #38bdf833; }
  .vstate.ok { color: var(--green); background: var(--green-dim); border-color: #22c55e4d; }
</style>
