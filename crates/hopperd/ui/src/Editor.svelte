<script>
  // A file opened to edit, like desktop's config editor: a TOML config opens
  // as Settings (switches, pickers and values from the file's own comments;
  // edits keep the rest of the file), with Text beside it; other files open
  // as text. A mod's config shows the mod (its icon opens the mod's panel)
  // and a picker over its other config files. Saving keeps the previous
  // version in the trash; History compares and brings one back.
  import { onMount, onDestroy } from 'svelte';
  import { ArrowLeft, Save, Download, AlertTriangle, ChevronRight, History, X, RotateCcw, Share2, Search, TriangleAlert, Check } from '@lucide/svelte';
  import ShareLog from './ShareLog.svelte';
  import ModPeek from './ModPeek.svelte';
  import ConfigSettings from './ConfigSettings.svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { confirmIt, choose, dialog } from './lib/dialog.svelte.js';
  import { leave } from './lib/leave.js';
  import { download } from './lib/transfers.svelte.js';
  import { jump } from './lib/files.svelte.js';
  import { configOwners, ownerOf, siblingsOf } from './lib/files-config.js';

  let { server, path, close } = $props();
  let sharing = $state(false);

  let host = $state();
  let ed = null;
  let loaded = $state(false);
  let error = $state('');
  let modified = null;
  let saved = '';
  /** The file's line ending: the editor works in \n, saving puts the file's own back. */
  let eol = '\n';
  /** The editor's text with the file's line endings. */
  const edText = () => {
    const t = ed?.get() ?? saved;
    return eol === '\n' ? t : t.replace(/\r?\n/g, eol);
  };
  let dirty = $state(false);
  let problem = $state('');
  let saving = $state(false);

  // Settings view (TOML that reads as settings).
  let view = $state('text');
  let text = '';
  let sections = $state(null);
  let query = $state('');
  let focusLine = $state(0);
  let settingsEl = $state();

  // The mod this config belongs to.
  let owners = $state(null);
  let peek = $state(false);

  let versions = $state(null);
  let picked = $state(null);
  let rows = $state([]);

  const base = $derived(`/fs/${encodeURIComponent(server.id)}`);
  const name = $derived(path.split('/').pop());
  const crumbs = $derived(path.split('/'));
  const toml = $derived(/\.toml$/i.test(path));
  const running = $derived(['running', 'starting'].includes(server.state));
  const configish = $derived(/^(config|defaultconfigs|kubejs)\//i.test(path) || /\.(toml|properties|json5?|cfg|ya?ml|ini|snbt)$/i.test(path));
  const mod = $derived(ownerOf(owners, path));
  const siblings = $derived(siblingsOf(owners, path));
  const peekMod = $derived(mod && owners?.mods.get(mod.file) ? mod : null);

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

  async function useVersion() {
    if (!picked) return;
    let old;
    try {
      old = (await api(`${base}/version-text?version=${encodeURIComponent(picked.id)}`)).content;
    } catch (e) {
      return say(e.message, true);
    }
    if (!(await toText())) return;
    ed?.set(old);
    recheck();
    versions = null;
    picked = null;
    say('Loaded the old version. Save to keep it.');
  }

  const changes = $derived(rows.filter((l) => l.k !== 'same').length);
  const when = (t) => new Date(t * 1000).toLocaleString();

  /** The text as it would be saved. */
  const current = () => (view === 'settings' ? text : edText());
  const recheck = () => (dirty = current() !== saved);

  // ---- settings and text ---------------------------------------------------------

  /** To the text view; false when an open field's value isn't taken. */
  async function toText() {
    if (view === 'text') return true;
    if (settingsEl && !(await settingsEl.flush())) return false;
    await pending;
    if (ed && edText() !== text) ed.set(text);
    view = 'text';
    if (focusLine) setTimeout(() => ed?.goto(focusLine, true), 0);
    else setTimeout(() => ed?.focus(), 0);
    return true;
  }

  async function toSettings() {
    if (view === 'settings' || !toml || !ed) return;
    const t = edText();
    try {
      const r = await post(`${base}/config/parse`, { text: t });
      if (!r.ok) return say(`The text has a mistake, so it can't show as settings: ${r.error}`, true);
      text = t;
      sections = r.sections;
      view = 'settings';
    } catch (e) {
      say(e.message, true);
    }
  }

  /** Edits in flight, one after another: each starts from the text the
   * one before made, so two quick switches both stay. */
  let pending = Promise.resolve();
  /** Sets one setting; resolves to the field's error, or '' when taken. */
  function setOne(table, key, value) {
    const run = pending.then(() => setNow(table, key, value));
    pending = run.catch(() => {});
    return run;
  }
  async function setNow(table, key, value) {
    try {
      const r = await post(`${base}/config/set`, { text, table, key, value });
      if (!r.ok) return r.error;
      text = r.text;
      sections = r.sections;
      recheck();
      problem = '';
      return '';
    } catch (e) {
      return e.message;
    }
  }

  /** The setting at `line`, or the one a comment line there is about. */
  function settingLine(line) {
    const all = (sections || []).flatMap((s) => s.entries.map((e) => e.line)).sort((a, b) => a - b);
    return all.find((l) => l === line) ?? all.find((l) => l > line && l - line <= 12) ?? 0;
  }

  // ---- saving and closing --------------------------------------------------------

  async function save(force = false) {
    if (!ed || saving) return;
    if (view === 'settings' && settingsEl && !(await settingsEl.flush())) return;
    await pending;
    saving = true;
    const content = current();
    try {
      const r = await api(`${base}/text`, { method: 'PUT', body: { path, content, modified, force } });
      if (r.result === 'saved') {
        modified = r.modified;
        saved = content;
        dirty = false;
        problem = '';
        say(`Saved ${name}. The old copy is in the trash.${running && configish ? ' Some settings apply only after a restart.' : ''}`);
      } else if (r.result === 'invalid') {
        problem = r.error;
        const m = /line (\d+)/.exec(r.error);
        if (m && (await toText())) ed.goto(Number(m[1]), true);
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

  let leaving = false;
  async function back() {
    if (dirty && !(await confirmIt(`Close ${name} without saving?`, { ok: 'Discard changes', danger: true }))) return;
    leaving = true;
    close();
  }

  function discard() {
    leaving = true;
    close();
  }

  /** Another of the mod's config files. */
  async function pickFile(rel) {
    if (rel === path) return;
    if (dirty && !(await confirmIt(`Open ${rel.split('/').pop()} without saving ${name}?`, { body: 'Your changes here are dropped.', ok: 'Discard changes', danger: true }))) return;
    leaving = true;
    location.hash = `/server/${[server.id, 'edit', ...rel.split('/')].map(encodeURIComponent).join('/')}`;
  }

  function keys(e) {
    // Ctrl+S anywhere on the page (the text editor takes its own first).
    if ((e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === 's') {
      if (e.defaultPrevented || dialog.open || sharing) return;
      e.preventDefault();
      if (dirty) save();
      return;
    }
    if (e.key !== 'Escape' || dialog.open || sharing || e.defaultPrevented) return;
    // Escape inside the text belongs to the editor (its search, its pick).
    if (e.target.closest?.('.cm-editor, input, select, textarea')) return;
    if (peek) peek = false;
    else if (versions) history();
    else if (dirty) say('Not saved yet: Save, or Discard changes.');
    else discard();
  }

  function guard(e) {
    if (dirty && !leaving) e.preventDefault();
  }

  // Leaving by a tab, link, the palette or back/forward: ask first (Shell
  // puts the old address back while this is open).
  const leaveGuard = {
    blocked: () => dirty && !leaving,
    async settle() {
      const r = await choose(`Save ${name} before leaving?`, { body: 'Your changes are dropped unless you save them.', ok: 'Save', alt: 'Discard changes' });
      if (r === 'alt') leaving = true;
      else if (r === 'ok') await save();
      return !dirty || leaving;
    },
  };
  leave.guard = leaveGuard;

  onMount(async () => {
    addEventListener('beforeunload', guard);
    configOwners(server.id).then((o) => (owners = o));
    const want = jump.path === path ? jump.line : 0;
    jump.path = '';
    jump.line = 0;
    try {
      const [t, cm] = await Promise.all([api(`${base}/text?path=${encodeURIComponent(path)}`), import('./lib/cm.js')]);
      modified = t.modified;
      eol = t.content.includes('\r\n') ? '\r\n' : '\n';
      saved = t.content;
      text = t.content;
      loaded = true;
      ed = cm.createEditor(host, t.content, name, {
        onSave: () => save(),
        onChange: () => {
          if (view === 'text') recheck();
          problem = '';
        },
      });
      if (toml) {
        const r = await post(`${base}/config/parse`, { text: t.content }).catch(() => null);
        if (r?.ok) {
          sections = r.sections;
          view = 'settings';
        }
      }
      if (view === 'settings') {
        focusLine = want ? settingLine(want) : 0;
        // A line the settings don't show (a list of tables, a comment far
        // from any setting) opens in the text.
        if (want && !focusLine) {
          focusLine = want;
          await toText();
        }
      } else if (want) {
        focusLine = want;
        ed.goto(want, true);
      } else {
        ed.focus();
      }
    } catch (e) {
      error = e.message;
    }
  });

  onDestroy(() => {
    if (leave.guard === leaveGuard) leave.guard = null;
    removeEventListener('beforeunload', guard);
    ed?.destroy();
  });
</script>

<svelte:window onkeydown={keys} />

<div class="outer">
<div class="wrap">
  <div class="ftool">
    <button class="btn sm icon ghost" title="Back to the folder (Esc)" onclick={back}><ArrowLeft size={14} /></button>
    {#if mod}
      <button class="modl" title="Show {mod.name}" disabled={!peekMod} onclick={() => (peek = !peek)}>
        {#if mod.icon}<img src={mod.icon} alt="" width="18" height="18" />{:else}<span class="ph"></span>{/if}<b class="ell">{mod.name}</b>
      </button>
      {#if siblings.length > 1}
        <select class="input fpick" value={path} title="This mod's config files" onchange={(e) => { const v = e.currentTarget.value; e.currentTarget.value = path; pickFile(v); }}>
          {#each siblings as f (f)}<option value={f}>{f.replace(/^config\//, '')}</option>{/each}
        </select>
      {:else}
        <span class="small mono subtle ell">{path}</span>
      {/if}
    {:else}
      <div class="crumbs">
        {#each crumbs as c, i (i)}
          {#if i}<ChevronRight size={13} />{/if}<span class:cur={i === crumbs.length - 1}>{c}</span>
        {/each}
      </div>
    {/if}
    {#if problem}<span class="chip crashed" title={problem}><AlertTriangle size={12} />{problem.length > 60 ? `${problem.slice(0, 60)}…` : problem}</span>{/if}
    <span class="sp"></span>
    {#if view === 'settings' && !versions}
      <label class="input find"><Search size={13} /><input placeholder="Search settings" bind:value={query} />{#if query}<button class="clr" title="Clear" onclick={() => (query = '')}><X size={12} /></button>{/if}</label>
    {/if}
    <button class="btn sm" class:on={versions} title="Earlier versions" onclick={history}><History size={14} />History</button>
    {#if /^(logs\/[^/]+\.log(\.gz)?|crash-reports\/[^/]+\.txt)$/i.test(path)}<button class="btn sm" title="Share on mclo.gs" onclick={() => (sharing = true)}><Share2 size={14} />Share</button>{/if}
    <button class="btn sm icon ghost" title="Download" onclick={() => download(`/api/v1${base}/file?path=${encodeURIComponent(path)}`)}><Download size={14} /></button>
  </div>
  {#if toml && loaded && !versions}
    <div class="tabs" role="tablist">
      <button role="tab" aria-selected={view === 'settings'} class:tab-sel={view === 'settings'} onclick={toSettings}>Settings</button>
      <button role="tab" aria-selected={view === 'text'} class:tab-sel={view === 'text'} onclick={toText}>Text</button>
    </div>
  {/if}
  {#if running && configish && loaded}
    <div class="notice small"><TriangleAlert size={14} />The server is running. Some settings only apply after a restart, and a few mods rewrite their config when the server stops.</div>
  {/if}
  {#if error}
    <div class="empty-state"><span class="es-ic"><AlertTriangle size={18} /></span><b>Can't open {name}</b><p class="err">{error}</p></div>
  {:else if !loaded}
    <div class="skels">{#each [62, 48, 75, 40, 66, 54] as w, i (i)}<div class="skel"><i style:width="{w}%"></i></div>{/each}</div>
  {/if}
  {#if versions}
    <div class="hist">
      <div class="vers">
        {#each versions as v (v.id)}
          <button class="ver row-hover" class:row-sel={picked?.id === v.id} onclick={() => show(v)}>
            <div class="small">{when(v.at)}</div><div class="tiny muted">{v.kind === 'deleted' ? 'deleted' : 'saved over'}</div>
          </button>
        {:else}
          <div class="empty-state"><span class="es-ic"><History size={18} /></span><b>No earlier versions</b><p>One is kept in the trash each time the file is saved.</p></div>
        {/each}
      </div>
      {#if picked}
        <div class="diff">
          <div class="dh"><span class="small">{changes ? `${changes} changed line${changes === 1 ? '' : 's'}` : 'Same as now'} <span class="muted">· old on the left, current on the right</span></span><span class="sp"></span>
            <button class="btn sm" disabled={!changes} onclick={useVersion}><RotateCcw size={13} />Use this version</button>
            <button class="btn sm icon ghost" title="Close" onclick={history}><X size={13} /></button></div>
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
  {#if view === 'settings' && sections && !versions}
    <ConfigSettings bind:this={settingsEl} {sections} {query} {focusLine} onSet={setOne} />
  {/if}
  <div class="host" class:hide={versions || view === 'settings' || !loaded} bind:this={host}></div>
  <div class="foot">
    <span class="tiny muted">The previous version is kept in the trash each time you save.</span>
    <span class="sp"></span>
    {#if dirty}<span class="tiny muted">Not saved yet</span>{:else if loaded && !problem}<span class="tiny muted ok"><Check size={12} />Saved</span>{/if}
    <button class="btn sm ghost" onclick={() => (dirty ? discard() : back())}>{dirty ? 'Discard changes' : 'Close'}</button>
    <button class="btn sm pri" disabled={!dirty || saving} title="The previous version goes to the trash, so you can undo this." onclick={() => save()}><Save size={14} />Save <span class="kbd k">Ctrl S</span></button>
  </div>
</div>
{#if peek && peekMod}
  <ModPeek {server} mod={owners.mods.get(peekMod.file)} onClose={() => (peek = false)} onChanged={async () => (owners = await configOwners(server.id, true))} onUpdate={() => (location.hash = `/server/${encodeURIComponent(server.id)}/mods`)} />
{/if}
</div>

{#if sharing}<ShareLog {server} {path} onClose={() => (sharing = false)} />{/if}
<style>
  .outer { display: flex; gap: 12px; flex: 1; min-height: 0; align-items: flex-start; }
  .wrap { display: flex; flex-direction: column; flex: 1; min-width: 0; align-self: stretch; min-height: 0; border: 1px solid var(--line); border-radius: 10px; overflow: hidden; background: var(--term); }
  .ftool { display: flex; align-items: center; gap: 8px; padding: 8px 10px; border-bottom: 1px solid var(--line); background: var(--base); flex-wrap: wrap; }
  .crumbs { display: flex; align-items: center; gap: 4px; color: var(--subtle); font-size: 12.5px; min-width: 0; flex-wrap: wrap; }
  .cur { color: var(--text); font-weight: 600; }
  .modl { display: inline-flex; align-items: center; gap: 7px; padding: 3px 6px; border-radius: 6px; min-width: 0; max-width: 260px; }
  .modl:hover:not(:disabled) { background: var(--hover); }
  .modl:disabled { cursor: default; }
  .modl img, .modl .ph { width: 18px; height: 18px; border-radius: 4px; flex: none; background: var(--raise); }
  .fpick { padding: 3px 8px; max-width: 320px; font-size: 12.5px; }
  .subtle { color: var(--subtle); }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .find { display: flex; align-items: center; gap: 6px; padding: 3px 8px; width: 220px; }
  .find input { flex: 1; min-width: 0; }
  .clr { color: var(--muted); display: grid; place-items: center; }
  .tabs { display: flex; gap: 2px; padding: 0 10px; border-bottom: 1px solid var(--line); background: var(--base); }
  .tabs button { padding: 7px 10px; font-size: 12.5px; color: var(--subtle); }
  .tabs button:hover:not(:disabled) { color: var(--text); }
  .tabs button:disabled { opacity: 0.45; cursor: default; }
  .notice { display: flex; align-items: center; gap: 8px; padding: 7px 12px; background: #eab3081a; color: #eab308; border-bottom: 1px solid var(--line); }
  .k { border-color: #0003; color: #1a1a1f; }
  .host { flex: 1; min-height: 300px; overflow: hidden; }
  .skels { flex: 1; padding: 8px 10px; display: flex; flex-direction: column; }
  .hide { display: none; }
  .on { border-color: var(--acc); color: var(--acc); }
  .hist { flex: 1; min-height: 300px; display: flex; overflow: hidden; }
  .vers { width: 190px; flex: none; border-right: 1px solid var(--line); overflow: auto; background: var(--base); }
  .ver { display: block; width: 100%; text-align: left; padding: 8px 12px; border-bottom: 1px solid var(--line); }
  .diff { flex: 1; min-width: 0; display: flex; flex-direction: column; }
  .dh { display: flex; align-items: center; gap: 8px; padding: 6px 10px; border-bottom: 1px solid var(--line); }
  .dl { flex: 1; overflow: auto; font-size: 12px; }
  .dr { display: grid; grid-template-columns: 44px 1fr 44px 1fr; white-space: pre-wrap; word-break: break-all; }
  .dr .n { color: var(--muted); text-align: right; padding-right: 8px; user-select: none; }
  .dr .t { padding: 0 6px; }
  .dr.del .o, .dr.change .o { background: #ef444426; }
  .dr.add .c, .dr.change .c { background: #22c55e22; }
  .dr.gap { display: block; padding: 0 12px; }
  .foot { display: flex; align-items: center; gap: 8px; padding: 6px 10px; border-top: 1px solid var(--line); background: var(--base); flex-wrap: wrap; }
  .ok { display: inline-flex; align-items: center; gap: 4px; }
  @media (max-width: 860px) { .find { width: 100%; order: 10; } }
</style>
