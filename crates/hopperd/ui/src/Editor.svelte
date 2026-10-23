<script>
  import { onMount, onDestroy } from 'svelte';
  import { ArrowLeft, Save, Download, CheckCircle2, AlertTriangle, ChevronRight } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { confirmIt } from './lib/dialog.svelte.js';
  import { download } from './lib/transfers.svelte.js';

  let { server, path, close } = $props();

  let host = $state();
  let ed = null;
  let loaded = $state(false);
  let error = $state('');
  let modified = null;
  let saved = '';
  let dirty = $state(false);
  let problem = $state('');
  let saving = $state(false);

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
    <button class="btn sm icon" title="Download" onclick={() => download(`/api/v1${base}/file?path=${encodeURIComponent(path)}`)}><Download size={14} /></button>
    <button class="btn sm pri" disabled={!dirty || saving} onclick={() => save()}><Save size={14} />Save <span class="kbd k">Ctrl S</span></button>
  </div>
  {#if error}
    <div class="msg err">{error}</div>
  {/if}
  <div class="host" bind:this={host}></div>
  <div class="foot tiny muted">The previous version is kept in the trash each time you save.</div>
</div>

<style>
  .wrap { display: flex; flex-direction: column; flex: 1; min-height: 0; border: 1px solid var(--line); border-radius: 10px; overflow: hidden; background: var(--term); }
  .ftool { display: flex; align-items: center; gap: 8px; padding: 8px 10px; border-bottom: 1px solid var(--line); background: var(--base); flex-wrap: wrap; }
  .crumbs { display: flex; align-items: center; gap: 4px; color: var(--subtle); font-size: 12.5px; min-width: 0; flex-wrap: wrap; }
  .cur { color: var(--text); font-weight: 600; }
  .k { border-color: #0003; color: #1a1a1f; }
  .host { flex: 1; min-height: 300px; overflow: hidden; }
  .msg { padding: 24px; text-align: center; }
  .foot { padding: 6px 12px; border-top: 1px solid var(--line); background: var(--base); }
</style>
