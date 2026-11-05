<script>
  // A server's Logs & crashes tab, as desktop's: its crash reports, newest
  // first with Hopper's one-line verdict, and the picked one read like the
  // crash card (ServerCrash with `given`, with its fixes for the newest); then latest.log and debug.log,
  // followed while the server runs (LogText).
  import { FileWarning, RefreshCw, FileText, Share2, ChevronRight, Trash2 } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { exactLabel, fullDate } from './lib/worlds.js';
  import { bytes } from './lib/format.js';
  import { confirmIt } from './lib/dialog.svelte.js';
  import { say } from './lib/toast.svelte.js';
  import ServerCrash from './ServerCrash.svelte';
  import LogText from './LogText.svelte';
  import ModPeek from './ModPeek.svelte';

  /** sub: 'crashes' | 'latest' | 'debug'; go(sub) changes it. */
  let { server, sub = 'crashes', go, onShare, openFile, openMods } = $props();

  const SUBS = [
    ['crashes', 'Crash reports'],
    ['latest', 'latest.log'],
    ['debug', 'debug.log'],
    ['older', 'Older logs'],
  ];
  // "See why" opens on the report the crash notice describes (sub is its path).
  const want = $derived(sub.startsWith('crash-reports/') ? sub : '');
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
      if (!list.some((c) => c.path === picked)) picked = (list.some((c) => c.path === want) ? want : '') || list[0]?.path || '';
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

  // ---- delete: one plain confirm, files go to the server's trash --------
  async function removeFiles(paths, title, body) {
    if (!(await confirmIt(title, { body, ok: 'Delete', danger: true }))) return;
    try {
      const r = await post(`${base}/logs/delete`, { paths });
      say(r.deleted === 1 ? 'Deleted 1 file. It is in the trash (Files › Trash).' : `Deleted ${r.deleted} files. They are in the trash (Files › Trash).`);
      await afterDelete();
    } catch (e) {
      say(e.message, true);
    }
  }
  async function clearAll(what, n) {
    const crashes = what === 'crashes';
    if (!n) return;
    const title = crashes ? `Delete all ${n} crash reports?` : `Delete ${n} older logs?`;
    const body = `${crashes ? 'They' : 'They (not latest.log or debug.log)'} go to the server's trash (Files › Trash), where you can get them back.`;
    if (!(await confirmIt(title, { body, ok: 'Delete', danger: true }))) return;
    try {
      const r = await post(`${base}/logs/clear`, { what });
      say(`Deleted ${r.deleted} files. They are in the trash (Files › Trash).`);
      await afterDelete();
    } catch (e) {
      say(e.message, true);
    }
  }
  async function afterDelete() {
    picked = '';
    await loadList();
    await loadOld();
  }
  // A report that explains a start takes its follow-up reports with it.
  const groupOf = (c) => [c.path, ...(list || []).filter((f) => f.follow_up && f.lead === c.path).map((f) => f.path)];
  function delReport(c) {
    const g = groupOf(c);
    removeFiles(g, g.length > 1 ? `Delete these ${g.length} crash reports?` : 'Delete this crash report?', g.length > 1 ? "They are the report and its follow-up reports; they go to the server's trash (Files › Trash), where you can get them back." : "It goes to the server's trash (Files › Trash), where you can get it back.");
  }
  const delLog = (f) => removeFiles([f.path], 'Delete this log?', `${fileOf(f.path)} goes to the server's trash (Files › Trash), where you can get it back.`);
  const onKey = (e, run) => {
    if (e.key === 'Delete') {
      e.preventDefault();
      run();
    }
  };

  let old = $state(null);
  async function loadOld() {
    try {
      const r = await api(`${base}/logs`);
      old = (r.logs || []).filter((f) => !f.crash && f.path !== 'logs/latest.log' && f.path !== 'logs/debug.log').sort((a, b) => b.modified - a.modified);
    } catch (e) {
      old = old || [];
    }
  }
  $effect(() => {
    if (view === 'older') loadOld();
  });

  // Follow-up reports sit folded under the report that explains them, unless
  // that fold is open or one of them is the picked report.
  let opened = $state({});
  const rows = $derived(
    (list || []).filter((c) => !(c.follow_up && c.lead) || picked === c.path || opened[c.lead]),
  );
  const toggle = (p) => (opened = { ...opened, [p]: !opened[p] });
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
        <div class="row lh"><b>Crash reports</b><span class="sp"></span><button class="btn sm ghost" title="Deletes every crash report of this server" disabled={!list?.length} onclick={() => clearAll('crashes', list.length)}><Trash2 size={13} />Clear all</button><button class="btn sm ghost icon" title="Look again" onclick={loadList}><RefreshCw size={13} /></button></div>
        {#if listErr}<div class="small err">{listErr}</div>{/if}
        {#if list === null}
          {#each [0, 1, 2] as k (k)}<div class="skel"><i></i></div>{/each}
        {:else if !list.length}
          <div class="empty-state"><div class="es-ic"><FileWarning size={18} /></div><b>No crash reports</b><p>If the server crashes, its report shows here with what Hopper makes of it.</p></div>
        {:else}
          {#each rows as c (c.path)}
            <div class="repw" role="presentation" oncontextmenu={(e) => { e.preventDefault(); delReport(c); }}>
              <button class="rep row-hover" class:fu={c.follow_up} class:row-sel={picked === c.path} title={c.path} onclick={() => (picked = c.path)} onkeydown={(e) => onKey(e, () => delReport(c))}>
                <span class="small t" title={fullDate(c.modified)}>{exactLabel(c.modified)}</span>
                <span class="tiny v" class:muted={!c.verdict || c.follow_up}>{c.follow_up ? c.title : c.verdict || fileOf(c.path)}</span>
              </button>
              <button class="btn sm ghost icon del" title={c.follow_ups ? 'Delete this report and its follow-ups' : 'Delete this report'} onclick={() => delReport(c)}><Trash2 size={13} /></button>
            </div>
            {#if c.follow_ups}
              <button class="fold row-hover" aria-expanded={!!opened[c.path]} onclick={() => toggle(c.path)}>
                <ChevronRight size={12} class={opened[c.path] ? 'open' : ''} />
                <span class="tiny muted">{opened[c.path] ? 'Hide follow-up reports' : c.follow_ups === 1 ? '+1 follow-up report' : `+${c.follow_ups} follow-up reports`}</span>
              </button>
            {/if}
          {/each}
        {/if}
      </div>
      <div class="read">
        {#if readErr}
          <div class="small err">{readErr}</div>
        {:else if picked && !read}
          <div class="muted small row"><span class="spinner"></span>Reading the report…</div>
        {:else if read?.crash}
          <ServerCrash {server} given={read} fixes={picked === list?.[0]?.path && settled} onPeek={(m) => (peek = m)} {onShare} {openFile} />
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
  {:else if view === 'older'}
    <div class="older card col">
      <div class="row lh"><b>Older logs</b><span class="sp"></span><button class="btn sm ghost" title="Deletes every older log; latest.log and debug.log stay" disabled={!old?.length} onclick={() => clearAll('logs', old.length)}><Trash2 size={13} />Clear old logs</button></div>
      {#if old === null}
        {#each [0, 1, 2] as k (k)}<div class="skel"><i></i></div>{/each}
      {:else if !old.length}
        <div class="empty-state"><div class="es-ic"><FileText size={18} /></div><b>No older logs</b><p>The server keeps its last start in latest.log. Older ones show up here.</p></div>
      {:else}
        {#each old as f (f.path)}
          <div class="orow row-hover" role="presentation" tabindex="-1" onkeydown={(e) => onKey(e, () => delLog(f))}>
            <span class="small grow name">{fileOf(f.path)}</span>
            <span class="tiny muted" title={fullDate(f.modified)}>{exactLabel(f.modified)}</span>
            <span class="tiny muted sz">{bytes(f.size)}</span>
            <button class="btn sm ghost icon" title="Delete this log" onclick={() => delLog(f)}><Trash2 size={13} /></button>
          </div>
        {/each}
      {/if}
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
  .repw { display: flex; align-items: center; gap: 2px; }
  .repw .del { opacity: 0; flex: none; }
  .repw:hover .del, .repw:focus-within .del { opacity: 1; }
  .older { max-width: 760px; gap: 2px; }
  .orow { display: flex; align-items: center; gap: 12px; padding: 4px 8px; border-radius: var(--radius); }
  .orow .name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .orow .sz { width: 64px; text-align: right; }
  .grow { flex: 1; min-width: 0; }
  .fu { padding-left: 22px; }
  .fold { display: flex; align-items: center; gap: 4px; padding: 4px 8px 4px 22px; border-radius: var(--radius); width: 100%; color: var(--muted); text-align: left; }
  .fold :global(.open) { transform: rotate(90deg); }
  .v { white-space: normal; overflow-wrap: anywhere; color: var(--subtle); }
  .v.muted { color: var(--muted); }
  .read { flex: 1; min-width: 0; max-height: 100%; overflow: auto; }
  .col { display: flex; flex-direction: column; gap: 6px; }
  .acts { gap: 6px; flex-wrap: wrap; }
  @media (max-width: 900px) { .split { flex-direction: column; align-items: stretch; } .list { width: auto; max-height: 40vh; } }
</style>
