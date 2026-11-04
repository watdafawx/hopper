<script>
  // A server's Logs & crashes tab, as desktop's: its crash reports, newest
  // first with Hopper's one-line verdict, and the picked one read like the
  // crash card (ServerCrash with `given`); then latest.log and debug.log,
  // followed while the server runs (LogText).
  import { FileWarning, RefreshCw, FileText, Share2 } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { whenLabel } from './lib/worlds.js';
  import ServerCrash from './ServerCrash.svelte';
  import LogText from './LogText.svelte';
  import ModPeek from './ModPeek.svelte';

  /** sub: 'crashes' | 'latest' | 'debug'; go(sub) changes it. */
  let { server, sub = 'crashes', go, onShare, openFile, openMods } = $props();

  const SUBS = [
    ['crashes', 'Crash reports'],
    ['latest', 'latest.log'],
    ['debug', 'debug.log'],
  ];
  const view = $derived(SUBS.some(([k]) => k === sub) ? sub : 'crashes');
  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);

  let list = $state(null);
  let listErr = $state('');
  let picked = $state('');
  let read = $state(null);
  let readErr = $state('');
  let peek = $state(null);

  // The panel works out a few verdicts per ask; ask again until done.
  let more = 0;
  async function loadList() {
    listErr = '';
    clearTimeout(more);
    try {
      const r = await api(`${base}/crashes`);
      list = r.crashes || [];
      if (!list.some((c) => c.path === picked)) picked = list[0]?.path || '';
      if (r.pending && view === 'crashes') more = setTimeout(loadList, 200);
    } catch (e) {
      list = list || [];
      listErr = e.message;
    }
  }

  // Only the report picked last shows (picking fast).
  let seq = 0;
  async function loadRead(p) {
    const n = ++seq;
    read = null;
    readErr = '';
    if (!p) return;
    try {
      const r = await api(`${base}/crashes/read?path=${encodeURIComponent(p)}`);
      if (n === seq) read = r;
    } catch (e) {
      if (n === seq) readErr = e.message;
    }
  }

  $effect(() => () => clearTimeout(more));

  // Looked at again when the server stops or crashes (a new report).
  const settled = $derived(server.state === 'crashed' || server.state === 'stopped');
  $effect(() => {
    settled;
    if (view === 'crashes') loadList();
  });
  $effect(() => {
    loadRead(picked);
  });

  const fileOf = (p) => p.split('/').pop();
</script>

<div class="logs">
  <div class="utabs sub" role="tablist" aria-label="Crashes and logs">
    {#each SUBS as [k, label] (k)}
      <button role="tab" aria-selected={view === k} class:tab-sel={view === k} onclick={() => go(k)}>{label}</button>
    {/each}
  </div>

  {#if view === 'crashes'}
    <div class="split">
      <div class="list card">
        <div class="row lh"><b>Crash reports</b><span class="sp"></span><button class="btn sm ghost icon" title="Look again" onclick={loadList}><RefreshCw size={13} /></button></div>
        {#if listErr}<div class="small err">{listErr}</div>{/if}
        {#if list === null}
          {#each [0, 1, 2] as k (k)}<div class="skel"><i></i></div>{/each}
        {:else if !list.length}
          <div class="empty-state"><div class="es-ic"><FileWarning size={18} /></div><b>No crash reports</b><p>If the server crashes, its report shows here with what Hopper makes of it.</p></div>
        {:else}
          {#each list as c (c.path)}
            <button class="rep row-hover" class:row-sel={picked === c.path} title={c.path} onclick={() => (picked = c.path)}>
              <span class="small t">{whenLabel(c.modified)}</span>
              <span class="tiny v" class:muted={!c.verdict}>{c.verdict || fileOf(c.path)}</span>
            </button>
          {/each}
        {/if}
      </div>
      <div class="read">
        {#if readErr}
          <div class="small err">{readErr}</div>
        {:else if picked && !read}
          <div class="muted small row"><span class="spinner"></span>Reading the report…</div>
        {:else if read?.crash}
          <ServerCrash {server} given={read} onPeek={(m) => (peek = m)} {onShare} {openFile} />
        {:else if read}
          <div class="card col">
            <b class="small">Hopper couldn't read this report</b>
            <p class="tiny muted">It may not be a Minecraft crash report. Open it to see what it says.</p>
            <div class="row acts">
              <button class="btn sm" onclick={() => openFile(read.report)}><FileText size={13} />Open report</button>
              <button class="btn sm" onclick={() => onShare(read.report)}><Share2 size={13} />Share on mclo.gs</button>
            </div>
          </div>
        {/if}
      </div>
      {#if peek}<ModPeek {server} mod={peek} onClose={() => (peek = null)} onChanged={() => {}} onUpdate={() => { peek = null; openMods(); }} />{/if}
    </div>
  {:else}
    {#key view}<LogText {server} path={view === 'debug' ? 'logs/debug.log' : 'logs/latest.log'} {onShare} {openFile} />{/key}
  {/if}
</div>

<style>
  .logs { flex: 1; min-height: 0; display: flex; flex-direction: column; gap: 10px; }
  .sub { align-self: flex-start; }
  .split { flex: 1; min-height: 0; display: flex; gap: 14px; align-items: flex-start; }
  .list { width: 300px; flex: none; display: flex; flex-direction: column; gap: 2px; padding: 10px; max-height: 100%; overflow: auto; }
  .lh { padding: 0 4px 6px; }
  .lh b { font-size: 13px; font-weight: 600; }
  .rep { display: flex; flex-direction: column; align-items: flex-start; gap: 1px; padding: 6px 8px 6px 10px; border-radius: var(--radius); text-align: left; color: var(--text); width: 100%; }
  .t { font-weight: 500; }
  .v { white-space: normal; overflow-wrap: anywhere; color: var(--subtle); }
  .v.muted { color: var(--muted); }
  .read { flex: 1; min-width: 0; max-height: 100%; overflow: auto; }
  .col { display: flex; flex-direction: column; gap: 6px; }
  .acts { gap: 6px; flex-wrap: wrap; }
  @media (max-width: 900px) { .split { flex-direction: column; align-items: stretch; } .list { width: auto; max-height: 40vh; } }
</style>
