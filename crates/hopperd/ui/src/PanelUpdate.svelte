<script>
  // Hopper Server's own updates: the version, whether a newer one is out,
  // "Update now", and auto-update (on by default). An update restarts the
  // panel only; Minecraft servers keep running. The tray icon comes back on
  // the new version by itself. "Other versions" opens the same window as
  // the version beside the logo (notes, and rolling back).
  import { RefreshCw, Download, ScrollText, CircleCheck, TriangleAlert, RotateCcw } from '@lucide/svelte';
  import { onDestroy } from 'svelte';
  import { api, post } from './lib/api.js';
  import { session } from './lib/me.svelte.js';
  import { say } from './lib/toast.svelte.js';
  import { upd, followUpdate, loadUpdate } from './lib/panelupdate.svelte.js';

  onDestroy(followUpdate());
  const st = $derived(upd.st);

  async function setAuto(on) {
    try {
      await api('/panel/update', { method: 'PUT', body: { auto: on } });
      st.auto = on;
    } catch (e) {
      say(e.message, true);
    }
  }

  async function now() {
    try {
      await post('/panel/update/now');
      st.state = 'checking';
      loadUpdate();
    } catch (e) {
      say(e.message, true);
    }
  }

  const ago = (s) => {
    if (!s) return 'not yet';
    const m = Math.round((Date.now() / 1000 - s) / 60);
    return m < 1 ? 'just now' : m < 60 ? `${m} min ago` : `${Math.round(m / 60)} h ago`;
  };
  const STATE = {
    checking: 'Checking for updates…',
    downloading: 'Downloading the update…',
    waiting: 'Update ready: restarting once running jobs finish…',
    restarting: 'Restarting into the new version…',
    ready: 'The new version is in place: it runs from the next start.',
  };
</script>

{#if st}
  <div class="pu col">
    <div class="row wrap">
      <b>Hopper Server {st.current}</b>
      {#if st.latest}<span class="chip acc">{st.latest} is out</span>{:else if st.checked}<span class="chip running"><CircleCheck size={11} />Up to date</span>{/if}
      <span class="sp"></span>
      {#if st.latest}<button class="btn sm ghost" onclick={() => (upd.open = true)}><ScrollText size={13} />What's new</button>{/if}
      <button class="btn sm ghost" title="Every release with its notes, including older ones to roll back to" onclick={() => (upd.open = true)}><RotateCcw size={13} />Other versions</button>
      {#if session.owner && !st.why_not}
        {#if st.latest}<button class="btn sm pri" disabled={!!STATE[st.state] && st.state !== 'ready'} onclick={now}><Download size={13} />Update now</button>
        {:else}<button class="btn sm" disabled={st.state === 'checking'} onclick={now}><RefreshCw size={13} />Check now</button>{/if}
      {/if}
    </div>
    {#if STATE[st.state]}<div class="small gold row"><span class="spin"></span>{STATE[st.state]}</div>
    {:else if st.state === 'error'}<div class="small red row"><TriangleAlert size={13} />{st.error}</div>
    {:else}<div class="tiny muted">Last checked {ago(st.checked)}.</div>{/if}
    {#if st.why_not}<div class="tiny muted">{st.why_not}</div>
    {:else}
      <label class="row sw-row">
        <input type="checkbox" checked={st.auto} disabled={!session.owner} onchange={(e) => setAuto(e.currentTarget.checked)} />
        <span><span>Update by itself</span><span class="tiny muted">Installs new versions when they're out and restarts the panel once nothing is running. Minecraft servers keep running, and the tray icon comes back on the new version.</span></span>
      </label>
      {#if st.skip}<div class="tiny muted">Rolled back: it won't install {st.skip} or older by itself. The next new release still installs.</div>{/if}
    {/if}
  </div>
{/if}

<style>
  .pu { gap: 8px; }
  .wrap { flex-wrap: wrap; }
  .gold { color: var(--gold); }
  .red { color: var(--red); }
  .sw-row { align-items: flex-start; gap: 10px; cursor: pointer; }
  .sw-row input { width: 15px; height: 15px; accent-color: var(--acc); margin-top: 2px; flex: none; }
  .sw-row > span { display: flex; flex-direction: column; gap: 2px; }
  .spin { width: 11px; height: 11px; border: 2px solid currentColor; border-right-color: transparent; border-radius: 99px; animation: rot .8s linear infinite; flex: none; }
  @keyframes rot { to { transform: rotate(360deg); } }
</style>
