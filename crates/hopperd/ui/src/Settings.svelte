<script>
  import { untrack } from 'svelte';
  import RemoveServer from './RemoveServer.svelte';
  import CopyServer from './CopyServer.svelte';
  import VersionPage from './VersionPage.svelte';
  import { session } from './lib/me.svelte.js';
  // Settings, one section per page like Hopper desktop's settings: a list
  // on the left switches the page, each page is one column (label left,
  // control right). Search finds a setting on any page. Changes to
  // server.properties, memory, GC flags and the Java flags all wait in one
  // save bar; Discard puts every one back.
  import { SlidersHorizontal, MemoryStick, Save, Search, FileCode, Globe, Users, Map as MapIcon, ListTree, X, ImageUp, ArrowRightLeft } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { confirmIt } from './lib/dialog.svelte.js';
  import { queueUploads, onUploaded } from './lib/transfers.svelte.js';
  import Address from './Address.svelte';
  import Avatar from './Avatar.svelte';

  // `start`: the page to open on (the header's "Minecraft 1.21.1" opens Version).
  let { server, start = '' } = $props();
  // The live feed hands over a new `server` object every second; effects
  // that should run once per server key on the id alone.
  const sid = $derived(server.id);
  let props = $state(null);
  let values = $state({});
  /** What the form started with (file values, else Minecraft's defaults). */
  let orig = $state({});
  let jvm = $state(null);
  let mem = $state(4096);
  /** What the memory slider started at (the server's -Xmx, else 4 GB). */
  let memOrig = $state(4096);
  /** The Java flags box, one flag per line, and what it loaded with. */
  let flagsText = $state('');
  let flagsOrig = $state('');
  const flagCount = $derived(flagsText.split('\n').filter((l) => l.trim() && !l.trim().startsWith('#')).length);
  const hasArgsFile = $derived((jvm?.files || []).includes('user_jvm_args.txt'));
  let filter = $state('');
  let saving = $state(false);
  let page = $state(untrack(() => start || 'general'));
  let iconPick = $state();

  /** Pages; groups of server.properties keys on each (a heading, keys). */
  const PAGES = [
    ['general', 'General', SlidersHorizontal, [['Server list', ['motd']], ['Game', ['difficulty', 'gamemode', 'hardcore', 'pvp', 'enable-command-block']]]],
    ['version', 'Version', ArrowRightLeft, []],
    ['players', 'Players & access', Users, [['Players', ['max-players', 'spawn-protection', 'allow-flight']], ['Who can join', ['online-mode', 'white-list']]]],
    ['world', 'World', MapIcon, [['World', ['level-name', 'level-seed']], ['Performance', ['view-distance', 'simulation-distance']]]],
    ['network', 'Network & address', Globe, [['Network', ['server-port']]]],
    ['java', 'Memory & Java', MemoryStick, []],
    ['files', 'Start files', FileCode, []],
    ['all', 'All properties', ListTree, []],
  ];
  /** Words that find a page (things on it that aren't server.properties keys). */
  const PAGE_WORDS = { java: 'java memory ram xmx xms gc garbage aikar', files: 'start files run.bat start.bat user_jvm_args flags jvm args arguments script', version:'version minecraft loader neoforge forge fabric quilt paper purpur upgrade downgrade update', general: 'clone copy template duplicate remove delete' };
  const PAGE_HINT = { version: 'Change Minecraft or loader version', general: 'Clone, save as template, remove', java: 'Memory, Java and GC flags', files: 'Start scripts and Java flags' };
  const UNITS = { 'view-distance': 'chunks', 'simulation-distance': 'chunks', 'spawn-protection': 'blocks', 'max-players': 'players' };

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const q = $derived(filter.trim().toLowerCase());
  const specs = $derived(props?.specs || []);
  const spec = (k) => specs.find((s) => s.key === k);
  const allKeys = $derived([...new Set([...specs.map((s) => s.key), ...Object.keys(values)])].sort());
  const match = (k) => {
    const s = spec(k);
    return `${k} ${s?.label || ''} ${s?.description || ''}`.toLowerCase().includes(q);
  };
  const hits = $derived(q ? allKeys.filter(match) : []);
  const pageHits = $derived(q ? PAGES.filter(([id]) => PAGE_WORDS[id]?.split(' ').some((w) => w.startsWith(q))) : []);
  // A cleared number box binds null: it's an empty value, not "null".
  const str = (v) => (v == null ? '' : String(v));
  const changed = $derived(Object.keys(values).filter((k) => str(values[k]) !== str(orig[k])));
  // Memory and GC flags wait in the save bar with the properties, as in
  // desktop's Settings tab.
  let preset = $state('');
  const memDirty = $derived(!!jvm && Number(mem) !== memOrig);
  const presetDirty = $derived(!!jvm && preset !== (jvm.preset ?? ''));
  const flagsDirty = $derived(!!jvm && flagsText !== flagsOrig);
  const changes = $derived([...changed, ...(memDirty ? ['Memory'] : []), ...(presetDirty ? ['GC flags'] : []), ...(flagsDirty ? ['Java flags'] : [])]);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));
  const presetLabel = $derived(preset === 'custom' ? `Custom flags (${jvm?.flags?.length || 0})` : jvm?.presets?.find((p) => p.id === preset)?.label || preset);
  // An unknown page in the link (#/server/<id>/settings/<page>) opens General.
  const cur = $derived(PAGES.find((p) => p[0] === page) || PAGES[0]);
  const pg = $derived(cur[0]);

  /** Reads server.properties into the form (Minecraft's defaults for
   * keys the file doesn't have). */
  async function loadProps() {
    props = await api(`${base}/properties`);
    const v = { ...props.values };
    for (const s of props.specs) if (!(s.key in v)) v[s.key] = s.default;
    values = v;
    orig = { ...v };
  }

  async function load() {
    try {
      await loadProps();
      setJvm(await api(`${base}/jvm`));
    } catch (e) {
      say(e.message, true);
    }
  }

  /** Keys among `keys` that changed in the file since the form read it
   * (the server, a command or someone else saved it meanwhile). */
  async function changedOnDisk(keys) {
    try {
      const now = (await api(`${base}/properties`)).values || {};
      return keys.filter((k) => k in now && str(now[k]) !== str(orig[k]));
    } catch {
      return [];
    }
  }

  /** Saves everything in the save bar: server.properties, then memory, GC
   * flags and Java flags in one go. A part that saved stops counting as a
   * change, so after a failure only what didn't save is left. */
  async function saveAll() {
    saving = true;
    let restart = false;
    try {
      if (changed.length) {
        const keys = [...changed];
        const clash = await changedOnDisk(keys);
        if (clash.length && !(await confirmIt('Save anyway?', {
          body: `${clash.slice(0, 3).join(', ')}${clash.length > 3 ? '…' : ''} changed in server.properties since you opened Settings. Saving puts your value${clash.length === 1 ? '' : 's'} in place of ${clash.length === 1 ? 'it' : 'them'}.`,
          ok: 'Save anyway',
        }))) return;
        const out = {};
        for (const k of keys) out[k] = str(values[k]);
        restart = (await api(`${base}/properties`, { method: 'PUT', body: { values: out } })).restart_needed || restart;
        orig = { ...orig, ...out };
      }
      if (memDirty || presetDirty || flagsDirty) {
        // The server applies memory, then the preset, then the flags text.
        const body = {};
        if (memDirty) body.memory_mb = Number(mem);
        if (presetDirty) body.preset = preset;
        if (flagsDirty) body.flags_text = flagsText;
        restart = (await api(`${base}/jvm`, { method: 'PUT', body })).restart_needed || restart;
        setJvm(await api(`${base}/jvm`));
      }
      say(restart ? 'Saved. Restart the server to apply.' : 'Saved.');
      await loadProps();
    } catch (e) {
      say(e.message, true);
    } finally {
      saving = false;
    }
  }

  function discard() {
    values = { ...orig };
    mem = memOrig;
    flagsText = flagsOrig;
    if (jvm) preset = jvm.preset ?? '';
  }

  function setJvm(j) {
    jvm = j;
    memOrig = mem = j.memory_mb || memOrig;
    preset = j.preset ?? '';
    flagsOrig = flagsText = (j.flags || []).join('\n');
  }

  async function pickIcon(ev) {
    const f = ev.target.files?.[0];
    ev.target.value = '';
    if (!f) return;
    if (f.type !== 'image/png') return say('The server icon has to be a PNG (64 × 64).', true);
    const size = await new Promise((res) => {
      const img = new Image();
      img.onload = () => res([img.width, img.height]);
      img.onerror = () => res([0, 0]);
      img.src = URL.createObjectURL(f);
    });
    if (size[0] !== 64 || size[1] !== 64) say(`That picture is ${size[0]} × ${size[1]}; Minecraft shows only 64 × 64 icons.`, true);
    queueUploads(server.id, [{ file: f, path: 'server-icon.png' }], true);
  }

  const gb = (mb) => (mb >= 1024 ? `${(mb / 1024).toFixed(mb % 1024 ? 1 : 0)} GB` : `${mb} MB`);
  const dirty = (k) => str(values[k]) !== str(orig[k]);

  $effect(() => {
    sid;
    load();
    const off = onUploaded((s, folder) => s === server.id && folder === '' && say('Server icon changed. The server list shows it after a restart.'));
    return () => off();
  });
</script>

{#snippet field(k)}
  {@const s = spec(k)}
  {#if s}
    <div class="fl" class:dirty={dirty(k)}>
      <div class="lab"><div>{s.label}</div><div class="tiny muted">{s.description} <span class="mono key">{s.key}</span></div></div>
      <div class="ctl">
        {#if s.kind === 'bool'}
          <label class="switch" title={values[k] === 'true' ? 'On' : 'Off'}><input type="checkbox" checked={values[k] === 'true'} onchange={(e) => (values[k] = e.currentTarget.checked ? 'true' : 'false')} /><span></span></label>
        {:else if s.kind === 'enum'}
          <select class="sel" bind:value={values[k]} aria-label={s.label}>{#each s.options as o (o)}<option value={o}>{o}</option>{/each}</select>
        {:else if s.kind === 'int'}
          <div class="row"><div class="input w"><input type="number" min={s.min} max={s.max} bind:value={values[k]} /></div>{#if UNITS[k]}<span class="tiny muted">{UNITS[k]}</span>{/if}</div>
        {:else}
          <div class="input wide"><input bind:value={values[k]} /></div>
        {/if}
      </div>
    </div>
  {:else}
    <div class="fl" class:dirty={dirty(k)}>
      <div class="lab mono small">{k}</div>
      <div class="ctl"><div class="input wide"><input bind:value={values[k]} /></div></div>
    </div>
  {/if}
{/snippet}

<div class="layout">
  <nav class="secs">
    <div class="input find"><Search size={13} /><input placeholder="Find a setting" bind:value={filter} />{#if filter}<button class="x" onclick={() => (filter = '')}><X size={12} /></button>{/if}</div>
    {#each PAGES as [id, label, Icon] (id)}
      <button class="row-hover" class:row-sel={!q && pg === id} onclick={() => { filter = ''; page = id; }}><Icon size={14} />{label}{id === 'all' ? ` ${allKeys.length}` : ''}</button>
    {/each}
  </nav>

  <div class="form">
    {#if !props}
      <div class="skel"><i></i></div><div class="skel"><i></i></div><div class="skel"><i></i></div><div class="skel"><i></i></div>
    {:else if q}
      <h2>{hits.length} setting{hits.length === 1 ? '' : 's'} matching “{filter}”</h2>
      {#each pageHits as [id, label, Icon] (id)}<button class="pagehit row-hover" onclick={() => { filter = ''; page = id; }}><Icon size={14} />{label}<span class="tiny muted">{PAGE_HINT[id]}</span></button>{/each}
      {#each hits as k (k)}{@render field(k)}{:else}<div class="small muted">Nothing matches. Try the key name, like view-distance.</div>{/each}
    {:else}
      <h2>{cur[1]}</h2>
      {#each cur[3] as [heading, keys] (heading)}
        <h4>{heading}</h4>
        {#if pg === 'general' && heading === 'Server list'}
          {@render field('motd')}
          <div class="fl">
            <div class="lab"><div>Icon</div><div class="tiny muted">server-icon.png in the server folder, 64 × 64 PNG</div></div>
            <div class="ctl row"><Avatar {server} cls="ic" /><button class="btn sm" onclick={() => iconPick.click()}><ImageUp size={13} />Change…</button>
              <input type="file" accept="image/png" hidden bind:this={iconPick} onchange={pickIcon} /></div>
          </div>
        {:else}
          {#each keys as k (k)}{@render field(k)}{/each}
        {/if}
      {/each}

      {#if pg === 'general'}
        <CopyServer {server} />
      {/if}
      {#if pg === 'general' && session.owner}
        <RemoveServer {server} />
      {/if}
      {#if pg === 'version'}
        <VersionPage {server} />
      {/if}
      {#if pg === 'network'}
        <h4>Address players join by</h4>
        <Address {server} />
      {:else if pg === 'java'}
        {#if jvm}
          <h4>Memory</h4>
          <div class="fl" class:dirty={memDirty}><div class="lab"><div>Memory</div><div class="tiny muted">The server's -Xms and -Xmx. Modded servers want 6 to 10 GB.{jvm.host_memory_mb ? ` This PC has ${gb(jvm.host_memory_mb)}.` : ''}</div></div>
            <div class="ctl"><input class="range" type="range" min="1024" max={Math.max(2048, Math.min(jvm.host_memory_mb - 1024, 65536))} step="512" bind:value={mem} aria-label="Memory" /><b class="memv">{gb(Number(mem))}</b></div></div>
          <h4>Java</h4>
          <div class="fl"><div class="lab"><div>Java</div><div class="tiny muted">{jvm.java_major ? `Java ${jvm.java_major}` : 'Version unknown'}: the one the server starts with</div></div>
            <div class="ctl"><span class="mono small ell" title={jvm.java || ''}>{jvm.java || 'java on PATH'}</span></div></div>
          <div class="fl" class:dirty={presetDirty}><div class="lab"><div>GC flags</div><div class="tiny muted">How Java frees memory. Aikar's flags suit almost every server.</div></div>
            <div class="ctl"><select class="sel gc" bind:value={preset} aria-label="GC flags">
              {#if preset === 'custom'}<option value="custom">{presetLabel}</option>{/if}
              {#each jvm.presets as p (p.id)}<option value={p.id} disabled={!p.supported}>{p.supported ? p.label : `${p.label} (needs a newer Java)`}</option>{/each}
            </select></div></div>
        {:else}
          <div class="small muted">The panel didn't say how this server starts.</div>
        {/if}
      {:else if pg === 'files'}
        {#if jvm}
          <h4>Files that start the server</h4>
          {#each jvm.files || [] as f (f)}
            <div class="fl"><div class="lab"><div class="mono">{f}</div><div class="tiny muted">{f === 'user_jvm_args.txt' ? 'Java arguments: memory and flags' : 'The start script'}</div></div>
              <div class="ctl"><a class="btn sm" href="#/server/{encodeURIComponent(server.id)}/edit/{encodeURIComponent(f)}"><FileCode size={13} />Edit</a></div></div>
          {:else}<div class="small muted">No run.bat, start.bat or user_jvm_args.txt; the server starts from its jar.</div>{/each}
          <h4>Java flags ({flagCount})</h4>
          <div class="tiny muted hint">One per line, saved to user_jvm_args.txt.{hasArgsFile ? '' : ' This server has no user_jvm_args.txt yet; its start script may not read one.'}</div>
          <textarea class="flags mono" class:dirty={flagsDirty} spellcheck="false" rows={Math.min(24, Math.max(6, flagCount + 1))} bind:value={flagsText} aria-label="Java flags"></textarea>
        {:else}
          <div class="small muted">The panel didn't say how this server starts.</div>
        {/if}
      {:else if pg === 'all'}
        <div class="tiny muted hint">Every key in server.properties. Keys without a description here are shown by name.</div>
        {#each allKeys as k (k)}{@render field(k)}{/each}
      {/if}
    {/if}
  </div>
</div>

{#if changes.length}
  <div class="savebar card row">
    <div class="grow"><div class="small ell">{changes.length} unsaved change{changes.length === 1 ? '' : 's'}: {changes.slice(0, 3).join(', ')}{changes.length > 3 ? '…' : ''}</div>
      {#if alive}<div class="tiny muted">The server is running: restart it to apply.</div>{/if}</div>
    <button class="btn sm" disabled={saving} onclick={discard}>Discard</button>
    <button class="btn sm pri" disabled={saving} onclick={saveAll}><Save size={13} />Save {changes.length}</button>
  </div>
{/if}

<style>
  .layout { display: flex; gap: 18px; align-items: flex-start; padding-bottom: 60px; }
  .secs { width: 210px; flex: none; display: flex; flex-direction: column; gap: 2px; position: sticky; top: 0; }
  .find { padding: 5px 9px; margin-bottom: 8px; }
  .find .x { color: var(--muted); }
  .secs > button { display: flex; align-items: center; gap: 9px; padding: 7px 10px; border-radius: 6px; color: var(--subtle); text-align: left; }
  .secs > button.row-sel { color: var(--text); }
  .form { flex: 1; min-width: 0; max-width: 860px; display: flex; flex-direction: column; }
  .form h2 { font-size: 17px; margin-bottom: 6px; }
  .form :global(h4) { font-size: 12px; text-transform: uppercase; letter-spacing: .06em; color: var(--muted); margin: 18px 0 2px; }
  .hint { margin-bottom: 6px; }
  .pagehit { display: flex; align-items: center; gap: 9px; padding: 8px 10px; border: 1px solid var(--line); border-radius: 8px; margin-bottom: 6px; text-align: left; }
  .fl { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 18px; align-items: center; padding: 10px 4px; border-bottom: 1px solid var(--line); }
  .fl.dirty { background: var(--acc-dim); border-radius: 6px; }
  .lab > div:first-child { font-weight: 500; }
  .key { color: var(--muted); margin-left: 4px; }
  .ctl { display: flex; align-items: center; gap: 8px; justify-content: flex-end; min-width: 0; }
  .w { width: 100px; padding: 5px 8px; }
  .wide { width: 300px; padding: 5px 9px; }
  .sel { background: var(--base); border: 1px solid var(--line2); border-radius: 6px; padding: 5px 8px; color: var(--text); width: 12rem; font: inherit; }
  .sel.gc { width: 16rem; }
  .grow { flex: 1; min-width: 0; }
  .range { width: 260px; accent-color: var(--acc); }
  .memv { min-width: 56px; text-align: right; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 380px; display: block; }
  .savebar .ell { max-width: none; }
  .flags { white-space: pre-wrap; word-break: break-all; font-size: 11.5px; background: var(--term); border: 1px solid var(--line); border-radius: 8px; padding: 10px 12px; max-height: 520px; overflow: auto; color: var(--text); width: 100%; resize: vertical; line-height: 1.5; }
  .flags:focus { outline: none; border-color: var(--acc); }
  .flags.dirty { border-color: #f9731677; }
  .savebar { position: fixed; bottom: 18px; left: 50%; transform: translateX(-50%); z-index: 30; gap: 10px; padding: 8px 12px; box-shadow: 0 12px 40px #000c; border-color: #f9731677; width: min(620px, calc(100vw - 32px)); }
  @media (max-width: 900px) { .layout { flex-direction: column; } .secs { width: 100%; position: static; } .fl { grid-template-columns: 1fr; } .ctl { justify-content: flex-start; } .wide, .range { width: 100%; } }
</style>
