<script>
  import { onMount, onDestroy } from 'svelte';
  import { ArrowLeft, Save, Download, CheckCircle2, AlertTriangle, ChevronRight, History, X, RotateCcw, Share2 } from '@lucide/svelte';
  import ShareLog from './ShareLog.svelte';
  import { api } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { confirmIt } from './lib/dialog.svelte.js';
  import { download } from './lib/transfers.svelte.js';

  let { server, path, close } = $props();
  let sharing = $state(false);

  let host = $state();
  let ed = null;
  let loaded = $state(false);
  let error = $state('');
  let modified = null;
  let saved = '';
  let dirty = $state(false);
  let problem = $state('');
  let saving = $state(false);

  let versions = $state(null);
  let picked = $state(null);
  let rows = $state([]);

  async function history() {
    if (versions) {
      versions = null;
      picked = null;
      return;
    }
    try {
      versions = (await api(`${base}/history?path=${encodeURIComponent(path)}`)).versions;
      if (versions.length) show(versions[0]);
    } catch (e) {
      say(e.message, true);
    }
  }

  async function show(v) {
    picked = v;
    try {
      rows = (await api(`${base}/diff?path=${encodeURIComponent(path)}&version=${encodeURIComponent(v.id)}`)).lines;
    } catch (e) {
      rows = [];
      say(e.message, true);
    }
  }

  function useVersion() {
    ed?.set(rows.filter((l) => l.old != null).map((l) => l.old).join('\n'));
    versions = null;
    picked = null;
    say('Loaded the old version. Save to keep it.');
  }

  const changes = $derived(rows.filter((l) => l.k !== 'same').length);
  const when = (t) => new Date(t * 1000).toLocaleString();

  const base = $derived(`/fs/${encodeURIComponent(server.id)}`);
  const name = $derived(path.split('/').pop());
  const crumbs = $derived(path.split('/'));

  async function save(force = false) {
    if (!ed || saving) return;
    saving = true;
    const content = ed.get();
    try {
      const r = await api(`${base}/text`, { method: 'PUT', body: { path, content, modified, force } });
      if (r.result === 'saved') {
        modified = r.modified;
        saved = content;
        dirty = false;
        problem = '';
        say(`Saved ${name}`);
      } else if (r.result === 'invalid') {
        problem = r.error;
        const m = /line (\d+)/.exec(r.error);
        if (m) ed.goto(Number(m[1]));
        if (await confirmIt('Save with this error?', { body: r.error, ok: 'Save anyway', danger: true })) {
          saving = false;
          return save(true);
        }
      } else if (r.result === 'conflict') {
        if (await confirmIt(`${name} changed on the server`, {
          body: 'Someone or something saved it after you opened it. Save yours over it? The other version goes to the trash.',
          ok: 'Save mine', danger: true,
        })) {
          saving = false;
          return save(true);
        }
      }
    } catch (e) {
      say(e.message, true);
    } finally {
      saving = false;
    }
  }

  async function back() {
    if (dirty && !(await confirmIt(`Close ${name} without saving?`, { ok: 'Close', danger: true }))) return;
    close();
  }

  function guard(e) {
    if (dirty) e.preventDefault();
  }

  onMount(async () => {
    addEventListener('beforeunload', guard);
    try {
      const [t, cm] = await Promise.all([api(`${base}/text?path=${encodeURIComponent(path)}`), import('./lib/cm.js')]);
      modified = t.modified;
      saved = t.content;
      loaded = true;
      ed = cm.createEditor(host, t.content, name, {
        onSave: () => save(),
        onChange: (text) => {
          dirty = text !== saved;
          problem = '';
        },
      });
      ed.focus();
    } catch (e) {
      error = e.message;
    }
  });

  onDestroy(() => {
    removeEventListener('beforeunload', guard);
    ed?.destroy();
  });
</script>

<div class="wrap">
  <div class="ftool">
    <button class="btn sm icon ghost" title="Back to the folder" onclick={back}><ArrowLeft size={14} /></button>
    <div class="crumbs">
      {#each crumbs as c, i (i)}
        {#if i}<ChevronRight size={13} />{/if}<span class:cur={i === crumbs.length - 1}>{c}</span>
      {/each}
    </div>
    {#if dirty}<span class="chip acc">Unsaved</span>{/if}
    {#if problem}<span class="chip crashed" title={problem}><AlertTriangle size={12} />{problem.length > 60 ? `${problem.slice(0, 60)}…` : problem}</span>
    {:else if loaded && !dirty}<span class="chip running"><CheckCircle2 size={12} />Saved</span>{/if}
    <span class="sp"></span>
    <button class="btn sm" class:on={versions} title="Earlier versions" onclick={history}><History size={14} />History</button>
    {#if /^(logs\/[^/]+\.log(\.gz)?|crash-reports\/[^/]+\.txt)$/i.test(path)}<button class="btn sm" title="Share on mclo.gs" onclick={() => (sharing = true)}><Share2 size={14} />Share</button>{/if}
    <button class="btn sm icon" title="Download" onclick={() => download(`/api/v1${base}/file?path=${encodeURIComponent(path)}`)}><Download size={14} /></button>
    <button class="btn sm pri" disabled={!dirty || saving} onclick={() => save()}><Save size={14} />Save <span class="kbd k">Ctrl S</span></button>
  </div>
  {#if error}
    <div class="msg err">{error}</div>
  {/if}
  {#if versions}
    <div class="hist">
      <div class="vers">
        {#each versions as v (v.id)}
          <button class="ver" class:on={picked?.id === v.id} onclick={() => show(v)}>
            <div class="small">{when(v.at)}</div><div class="tiny muted">{v.kind === 'deleted' ? 'deleted' : 'saved over'}</div>
          </button>
        {:else}
          <p class="small muted pad">No earlier versions. One is kept in the trash each time the file is saved.</p>
        {/each}
      </div>
      {#if picked}
        <div class="diff">
          <div class="dh"><span class="small">{changes ? `${changes} changed line${changes === 1 ? '' : 's'}` : 'Same as now'} <span class="muted">· old on the left, current on the right</span></span><span class="sp"></span>
            <button class="btn sm" disabled={!changes} onclick={useVersion}><RotateCcw size={13} />Use this version</button>
            <button class="btn sm icon ghost" onclick={history}><X size={13} /></button></div>
          <div class="dl mono">
            {#each rows as l, i (i)}
              {#if l.k !== 'same' || rows.slice(Math.max(0, i - 3), i + 4).some((x) => x.k !== 'same')}
                <div class="dr {l.k}"><span class="n">{l.on ?? ''}</span><span class="t o">{l.old ?? ''}</span><span class="n">{l.nn ?? ''}</span><span class="t c">{l.new ?? ''}</span></div>
              {:else if rows[i - 1]?.k !== 'same' || i === 0}
                <div class="dr gap"><span class="tiny muted">⋯</span></div>
              {/if}
            {/each}
          </div>
        </div>
      {/if}
    </div>
  {/if}
  <div class="host" class:hide={versions} bind:this={host}></div>
  <div class="foot tiny muted">The previous version is kept in the trash each time you save.</div>
</div>


{#if sharing}<ShareLog {server} {path} onClose={() => (sharing = false)} />{/if}
<style>
  .wrap { display: flex; flex-direction: column; flex: 1; min-height: 0; border: 1px solid var(--line); border-radius: 10px; overflow: hidden; background: var(--term); }
  .ftool { display: flex; align-items: center; gap: 8px; padding: 8px 10px; border-bottom: 1px solid var(--line); background: var(--base); flex-wrap: wrap; }
  .crumbs { display: flex; align-items: center; gap: 4px; color: var(--subtle); font-size: 12.5px; min-width: 0; flex-wrap: wrap; }
  .cur { color: var(--text); font-weight: 600; }
  .k { border-color: #0003; color: #1a1a1f; }
  .host { flex: 1; min-height: 300px; overflow: hidden; }
  .msg { padding: 24px; text-align: center; }
  .hide { display: none; }
  .on { border-color: var(--acc); color: var(--acc); }
  .hist { flex: 1; min-height: 300px; display: flex; overflow: hidden; }
  .vers { width: 190px; flex: none; border-right: 1px solid var(--line); overflow: auto; background: var(--base); }
  .ver { display: block; width: 100%; text-align: left; padding: 8px 12px; border-bottom: 1px solid var(--line); }
  .ver.on, .ver:hover { background: var(--acc-dim); color: var(--text); }
  .pad { padding: 12px; }
  .diff { flex: 1; min-width: 0; display: flex; flex-direction: column; }
  .dh { display: flex; align-items: center; gap: 8px; padding: 6px 10px; border-bottom: 1px solid var(--line); }
  .dl { flex: 1; overflow: auto; font-size: 12px; }
  .dr { display: grid; grid-template-columns: 44px 1fr 44px 1fr; white-space: pre-wrap; word-break: break-all; }
  .dr .n { color: var(--muted); text-align: right; padding-right: 8px; user-select: none; }
  .dr .t { padding: 0 6px; }
  .dr.del .o, .dr.change .o { background: #ef444426; }
  .dr.add .c, .dr.change .c { background: #22c55e22; }
  .dr.gap { display: block; padding: 0 12px; }
  .foot { padding: 6px 12px; border-top: 1px solid var(--line); background: var(--base); }
</style>
