<script>
  import RemoveServer from './RemoveServer.svelte';
  import { session } from './lib/me.svelte.js';
  // Settings, one section per page like Hopper desktop's settings: a list
  // on the left switches the page, each page is one column (label left,
  // control right). Search finds a setting on any page. Changes to
  // server.properties wait in a save bar.
  import { SlidersHorizontal, MemoryStick, Save, Search, FileCode, Globe, Users, Map as MapIcon, ListTree, X, ImageUp } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { queueUploads, onUploaded } from './lib/transfers.svelte.js';
  import Address from './Address.svelte';
  import Avatar from './Avatar.svelte';

  let { server } = $props();
  // The live feed hands over a new `server` object every second; effects
  // that should run once per server key on the id alone.
  const sid = $derived(server.id);
  let props = $state(null);
  let values = $state({});
  /** What the form started with (file values, else Minecraft's defaults). */
  let orig = $state({});
  let jvm = $state(null);
  let mem = $state(4096);
  let filter = $state('');
  let saving = $state(false);
  let page = $state('general');
  let iconPick = $state();

  /** Pages; groups of server.properties keys on each (a heading, keys). */
  const PAGES = [
    ['general', 'General', SlidersHorizontal, [['Server list', ['motd']], ['Game', ['difficulty', 'gamemode', 'hardcore', 'pvp', 'enable-command-block']]]],
    ['players', 'Players & access', Users, [['Players', ['max-players', 'spawn-protection', 'allow-flight']], ['Who can join', ['online-mode', 'white-list']]]],
    ['world', 'World', MapIcon, [['World', ['level-name', 'level-seed']], ['Performance', ['view-distance', 'simulation-distance']]]],
    ['network', 'Network & address', Globe, [['Network', ['server-port']]]],
    ['java', 'Memory & Java', MemoryStick, []],
    ['files', 'Start files', FileCode, []],
    ['all', 'All properties', ListTree, []],
  ];
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
  const changed = $derived(Object.keys(values).filter((k) => String(values[k]) !== String(orig[k] ?? '')));
  const cur = $derived(PAGES.find((p) => p[0] === page));

  async function load() {
    try {
      props = await api(`${base}/properties`);
      const v = { ...props.values };
      for (const s of props.specs) if (!(s.key in v)) v[s.key] = s.default;
      values = v;
      orig = { ...v };
      jvm = await api(`${base}/jvm`);
      mem = jvm.memory_mb || 4096;
    } catch (e) {
      say(e.message, true);
    }
  }

  async function saveProps() {
    saving = true;
    try {
      const out = {};
      for (const k of changed) out[k] = String(values[k]);
      const r = await api(`${base}/properties`, { method: 'PUT', body: { values: out } });
      say(r.restart_needed ? 'Saved. Restart the server to apply.' : 'Saved.');
      load();
    } catch (e) {
      say(e.message, true);
    } finally {
      saving = false;
    }
  }

  async function saveJvm(body) {
    try {
      const r = await api(`${base}/jvm`, { method: 'PUT', body });
      say(r.restart_needed ? 'Saved. Restart the server to apply.' : 'Saved.');
      jvm = await api(`${base}/jvm`);
      mem = jvm.memory_mb || mem;
    } catch (e) {
      say(e.message, true);
    }
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
  const dirty = (k) => String(values[k]) !== String(orig[k] ?? '');

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
          <label class="sw" title={values[k] === 'true' ? 'On' : 'Off'}><input type="checkbox" checked={values[k] === 'true'} onchange={(e) => (values[k] = e.currentTarget.checked ? 'true' : 'false')} /><span></span></label>
        {:else if s.kind === 'enum' && s.options.length <= 4}
          <div class="seg">{#each s.options as o (o)}<button class:on={values[k] === o} onclick={() => (values[k] = o)}>{o[0].toUpperCase() + o.slice(1)}</button>{/each}</div>
        {:else if s.kind === 'enum'}
          <select class="sel" bind:value={values[k]}>{#each s.options as o (o)}<option value={o}>{o}</option>{/each}</select>
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
      <button class:on={!q && page === id} onclick={() => { filter = ''; page = id; }}><Icon size={14} />{label}{id === 'all' ? ` ${allKeys.length}` : ''}</button>
    {/each}
  </nav>

  <div class="form">
    {#if !props}
      <div class="small muted">Loading…</div>
    {:else if q}
      <h2>{hits.length} setting{hits.length === 1 ? '' : 's'} matching “{filter}”</h2>
      {#each hits as k (k)}{@render field(k)}{:else}<div class="small muted">Nothing matches. Try the key name, like view-distance.</div>{/each}
    {:else}
      <h2>{cur[1]}</h2>
      {#each cur[3] as [heading, keys] (heading)}
        <h4>{heading}</h4>
        {#if page === 'general' && heading === 'Server list'}
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

      {#if page === 'general' && session.owner}
        <RemoveServer {server} />
      {/if}
      {#if page === 'network'}
        <h4>Address players join by</h4>
        <Address {server} />
      {:else if page === 'java'}
        {#if jvm}
          <h4>Memory</h4>
          <div class="fl"><div class="lab"><div>Maximum memory</div><div class="tiny muted">{gb(jvm.host_memory_mb)} on this PC. Modpacks usually want 6–12 GB.</div></div>
            <div class="ctl memc"><div class="row"><input class="range" type="range" min="1024" max={Math.max(2048, Math.min(jvm.host_memory_mb - 1024, 65536))} step="512" bind:value={mem} /><b class="memv">{gb(mem)}</b></div>
              {#if mem !== jvm.memory_mb}<button class="btn sm pri" onclick={() => saveJvm({ memory_mb: Number(mem) })}><Save size={13} />Set {gb(mem)}</button>{/if}</div></div>
          <h4>Java</h4>
          <div class="fl"><div class="lab"><div>Java</div><div class="tiny muted">{jvm.java_major ? `Java ${jvm.java_major}` : 'Version unknown'}: the one the server starts with</div></div>
            <div class="ctl"><span class="mono small ell" title={jvm.java || ''}>{jvm.java || 'java on PATH'}</span></div></div>
          <div class="fl"><div class="lab"><div>Garbage collector</div><div class="tiny muted">How Java frees memory. G1 tuned suits most packs.</div></div>
            <div class="ctl"><div class="seg">{#each jvm.presets as p (p.id)}<button class:on={jvm.preset === p.id} disabled={!p.supported} title={p.supported ? '' : 'Needs a newer Java'} onclick={() => saveJvm({ preset: p.id })}>{p.label}</button>{/each}</div>
              {#if jvm.preset === 'custom'}<span class="tiny muted">custom flags ({jvm.flags.length})</span>{/if}</div></div>
        {/if}
      {:else if page === 'files'}
        {#if jvm}
          <h4>Files that start the server</h4>
          {#each jvm.files || [] as f (f)}
            <div class="fl"><div class="lab"><div class="mono">{f}</div><div class="tiny muted">{f === 'user_jvm_args.txt' ? 'Java arguments: memory and flags' : 'The start script'}</div></div>
              <div class="ctl"><a class="btn sm" href="#/server/{encodeURIComponent(server.id)}/edit/{encodeURIComponent(f)}"><FileCode size={13} />Edit</a></div></div>
          {:else}<div class="small muted">No run.bat, start.bat or user_jvm_args.txt; the server starts from its jar.</div>{/each}
          <h4>Java flags ({jvm.flags.length})</h4>
          <div class="flags mono">{jvm.flags.join('\n') || 'None'}</div>
        {/if}
      {:else if page === 'all'}
        <div class="tiny muted hint">Every key in server.properties. Keys without a description here are shown by name.</div>
        {#each allKeys as k (k)}{@render field(k)}{/each}
      {/if}
    {/if}
  </div>
</div>

{#if changed.length}
  <div class="savebar card row">
    <span class="small">{changed.length} unsaved change{changed.length === 1 ? '' : 's'}: {changed.slice(0, 3).join(', ')}{changed.length > 3 ? '…' : ''}</span><span class="sp"></span>
    <button class="btn sm ghost" disabled={saving} onclick={() => (values = { ...orig })}>Discard</button>
    <button class="btn sm pri" disabled={saving} onclick={saveProps}><Save size={13} />Save {changed.length}</button>
  </div>
{/if}

<style>
  .layout { display: flex; gap: 18px; align-items: flex-start; padding-bottom: 60px; }
  .secs { width: 210px; flex: none; display: flex; flex-direction: column; gap: 2px; position: sticky; top: 0; }
  .find { padding: 5px 9px; margin-bottom: 8px; }
  .find .x { color: var(--muted); }
  .secs > button { display: flex; align-items: center; gap: 9px; padding: 7px 10px; border-radius: 6px; color: var(--subtle); text-align: left; }
  .secs > button:hover { background: #ffffff06; }
  .secs > button.on { background: var(--raise); color: var(--text); box-shadow: inset 3px 0 0 var(--acc); }
  .form { flex: 1; min-width: 0; max-width: 860px; display: flex; flex-direction: column; }
  .form h2 { font-size: 17px; margin-bottom: 6px; }
  .form h4 { font-size: 12px; text-transform: uppercase; letter-spacing: .06em; color: var(--muted); margin: 18px 0 2px; }
  .hint { margin-bottom: 6px; }
  .fl { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 18px; align-items: center; padding: 10px 4px; border-bottom: 1px solid var(--line); }
  .fl.dirty { background: var(--acc-dim); border-radius: 6px; }
  .lab > div:first-child { font-weight: 500; }
  .key { color: var(--muted); margin-left: 4px; }
  .ctl { display: flex; align-items: center; gap: 8px; justify-content: flex-end; min-width: 0; }
  .w { width: 100px; padding: 5px 8px; }
  .wide { width: 300px; padding: 5px 9px; }
  .sel { background: var(--base); border: 1px solid var(--line2); border-radius: 6px; padding: 5px 8px; color: var(--text); }
  .seg { display: inline-flex; border: 1px solid var(--line2); border-radius: 7px; overflow: hidden; flex-wrap: wrap; }
  .seg button { padding: 5px 12px; font-size: 12.5px; color: var(--subtle); border-right: 1px solid var(--line2); }
  .seg button:last-child { border-right: 0; }
  .seg button.on { background: var(--acc); color: #1a1a1f; font-weight: 600; }
  .seg button:disabled { opacity: .4; }
  .sw { position: relative; width: 36px; height: 20px; flex: none; cursor: pointer; }
  .sw input { opacity: 0; width: 0; height: 0; }
  .sw span { position: absolute; inset: 0; background: var(--line2); border-radius: 10px; transition: 0.15s; }
  .sw span::after { content: ''; position: absolute; width: 16px; height: 16px; border-radius: 50%; background: #fff; top: 2px; left: 2px; transition: 0.15s; }
  .sw input:checked + span { background: var(--acc); }
  .sw input:checked + span::after { left: 18px; }
  .memc { flex-direction: column; align-items: flex-end; }
  .range { width: 260px; accent-color: var(--acc); }
  .memv { min-width: 56px; text-align: right; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 380px; display: block; }
  .flags { white-space: pre-wrap; word-break: break-all; font-size: 11.5px; background: var(--term); border: 1px solid var(--line); border-radius: 8px; padding: 10px 12px; max-height: 420px; overflow: auto; color: var(--subtle); }
  .savebar { position: fixed; bottom: 18px; left: 50%; transform: translateX(-50%); z-index: 30; gap: 10px; padding: 8px 12px; box-shadow: 0 12px 40px #000c; border-color: #f9731677; width: min(620px, calc(100vw - 32px)); }
  @media (max-width: 900px) { .layout { flex-direction: column; } .secs { width: 100%; position: static; } .fl { grid-template-columns: 1fr; } .ctl { justify-content: flex-start; } .wide, .range { width: 100%; } }
</style>
