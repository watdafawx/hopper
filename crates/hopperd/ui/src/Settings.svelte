<script>
  // Settings, in sections with a list to jump between them (like Hopper
  // desktop's instance settings): server.properties in groups, the address,
  // memory & Java with the start files, and every other key.
  import { SlidersHorizontal, MemoryStick, Save, Search, FileCode, Globe, Users, Map as MapIcon, Network, ListTree } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import Address from './Address.svelte';

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
  let at = $state('general');
  let content = $state();

  /** server.properties keys by section; keys not listed go to Other. */
  const GROUPS = [
    ['general', 'General', SlidersHorizontal, ['motd', 'difficulty', 'gamemode', 'hardcore', 'pvp', 'enable-command-block']],
    ['players', 'Players', Users, ['max-players', 'white-list', 'online-mode', 'allow-flight', 'spawn-protection']],
    ['world', 'World', MapIcon, ['level-name', 'level-seed', 'view-distance', 'simulation-distance']],
    ['network', 'Network', Network, ['server-port']],
  ];
  const SECTIONS = [
    ...GROUPS.map(([id, label, icon]) => [id, label, icon]),
    ['address', 'Address', Globe],
    ['java', 'Memory & Java', MemoryStick],
    ['other', 'Other keys', ListTree],
  ];

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const q = $derived(filter.trim().toLowerCase());
  const specs = $derived(props?.specs || []);
  const grouped = $derived(new Set(GROUPS.flatMap((g) => g[3])));
  const known = $derived(new Set(specs.map((s) => s.key)));
  const match = (s) => !q || `${s.label} ${s.key} ${s.description}`.toLowerCase().includes(q);
  const inGroup = (keys) => keys.map((k) => specs.find((s) => s.key === k)).filter((s) => s && match(s));
  /** Known keys that no group lists, then keys Hopper has no description for. */
  const extraSpecs = $derived(specs.filter((s) => !grouped.has(s.key) && match(s)));
  const others = $derived(Object.keys(values).filter((k) => !known.has(k) && (!q || k.includes(q))).sort());
  const changed = $derived(Object.keys(values).filter((k) => String(values[k]) !== String(orig[k] ?? '')));

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

  /** While a jump scrolls, the list keeps the section that was clicked. */
  let jumping = 0;
  function jump(id) {
    at = id;
    jumping = Date.now();
    content?.querySelector(`#set-${id}`)?.scrollIntoView({ behavior: 'smooth', block: 'start' });
  }

  /** The section list follows scrolling: the top-most section in view. */
  $effect(() => {
    if (!content || !props) return;
    const seen = new Map();
    const io = new IntersectionObserver((entries) => {
      for (const e of entries) seen.set(e.target.id.slice(4), e.isIntersecting ? e.boundingClientRect.top : null);
      if (Date.now() - jumping < 900) return;
      const first = SECTIONS.map(([id]) => id).find((id) => seen.get(id) != null);
      if (first) at = first;
    }, { rootMargin: '-40px 0px -55% 0px' });
    for (const el of content.querySelectorAll('section[id^="set-"]')) io.observe(el);
    return () => io.disconnect();
  });

  const gb = (mb) => (mb >= 1024 ? `${(mb / 1024).toFixed(mb % 1024 ? 1 : 0)} GB` : `${mb} MB`);

  $effect(() => {
    sid;
    load();
  });
</script>

{#snippet field(s)}
  <div class="f" class:dirty={String(values[s.key]) !== String(orig[s.key] ?? '')}>
    <div class="lab"><div>{s.label} <span class="tiny muted mono">{s.key}</span></div><div class="tiny muted">{s.description}</div></div>
    {#if s.kind === 'bool'}
      <input type="checkbox" checked={values[s.key] === 'true'} onchange={(e) => (values[s.key] = e.currentTarget.checked ? 'true' : 'false')} />
    {:else if s.kind === 'enum'}
      <select class="sel" bind:value={values[s.key]}>{#each s.options as o (o)}<option value={o}>{o}</option>{/each}</select>
    {:else if s.kind === 'int'}
      <div class="input w"><input type="number" min={s.min} max={s.max} bind:value={values[s.key]} /></div>
    {:else}
      <div class="input wide"><input bind:value={values[s.key]} /></div>
    {/if}
  </div>
{/snippet}

<div class="layout">
  <nav class="secs">
    {#each SECTIONS as [id, label, Icon] (id)}
      <button class:on={at === id} onclick={() => jump(id)}><Icon size={13} />{label}{id === 'other' && others.length ? ` ${others.length}` : ''}</button>
    {/each}
  </nav>

  <div class="content" bind:this={content}>
    <div class="bar">
      <div class="input find"><Search size={13} /><input placeholder="Find a setting" bind:value={filter} /></div>
    </div>
    {#if changed.length}
      <div class="savebar card row">
        <span class="small">{changed.length} unsaved change{changed.length === 1 ? '' : 's'} to server.properties</span><span class="sp"></span>
        <button class="btn sm ghost" disabled={saving} onclick={() => (values = { ...orig })}>Discard</button>
        <button class="btn sm pri" disabled={saving} onclick={saveProps}><Save size={13} />Save {changed.length}</button>
      </div>
    {/if}

    <div class="grid">
      {#each GROUPS as [id, label, Icon, keys] (id)}
        {@const list = inGroup(keys)}
        {#if list.length || !q}
          <section class="card" id="set-{id}"><h3><Icon size={14} />{label}</h3>
            {#each list as s (s.key)}{@render field(s)}{/each}
            {#if id === 'general'}{#each extraSpecs as s (s.key)}{@render field(s)}{/each}{/if}
          </section>
        {/if}
      {/each}

      <section id="set-address"><Address {server} /></section>

      <section class="card" id="set-java"><h3><MemoryStick size={14} />Memory &amp; Java</h3>
        {#if jvm}
          <div class="row small"><span>Memory</span><span class="sp"></span><b>{gb(mem)}</b></div>
          <input class="range" type="range" min="1024" max={Math.max(2048, Math.min(jvm.host_memory_mb - 1024, 65536))} step="512" bind:value={mem} />
          <div class="row tiny muted"><span>1 GB</span><span class="sp"></span><span>{gb(jvm.host_memory_mb)} on this PC</span></div>
          <button class="btn sm" disabled={mem === jvm.memory_mb} onclick={() => saveJvm({ memory_mb: Number(mem) })}><Save size={13} />Set memory</button>
          <div class="lbl gap">Garbage collector</div>
          <div class="row wrap">
            {#each jvm.presets as p (p.id)}
              <button class="btn sm" class:pri={jvm.preset === p.id} disabled={!p.supported} title={p.supported ? '' : 'Needs a newer Java'} onclick={() => saveJvm({ preset: p.id })}>{p.label}</button>
            {/each}
            {#if jvm.preset === 'custom'}<span class="chip">custom flags ({jvm.flags.length})</span>{/if}
          </div>
          <div class="lbl gap">Java</div>
          <div class="small mono ell" title={jvm.java || ''}>{jvm.java || 'java on PATH'}</div>
          <div class="tiny muted">{jvm.java_major ? `Java ${jvm.java_major}` : 'Version unknown'}</div>
          {#if jvm.files?.length}
            <div class="lbl gap">Start files</div>
            <div class="row wrap">{#each jvm.files as f (f)}<a class="btn sm" href="#/server/{encodeURIComponent(server.id)}/edit/{encodeURIComponent(f)}"><FileCode size={13} />Edit {f}</a>{/each}</div>
          {/if}
          <details class="tiny muted"><summary>Flags ({jvm.flags.length})</summary><div class="mono flags">{jvm.flags.join('\n')}</div></details>
        {/if}
      </section>

      <section class="card span" id="set-other"><h3><ListTree size={14} />Other keys<span class="tiny muted">· in server.properties, without a description here</span></h3>
        {#each others as k (k)}
          <div class="f" class:dirty={String(values[k]) !== String(orig[k] ?? '')}><div class="mono small">{k}</div><div class="input wide"><input bind:value={values[k]} /></div></div>
        {:else}
          <div class="small muted">{q ? 'None match.' : 'None.'}</div>
        {/each}
      </section>
    </div>
  </div>
</div>

<style>
  .layout { display: flex; gap: 14px; align-items: flex-start; height: 100%; min-height: 0; }
  .secs { width: 170px; flex: none; display: flex; flex-direction: column; gap: 2px; position: sticky; top: 0; }
  .secs button { display: flex; align-items: center; gap: 8px; padding: 6px 10px; border-radius: 6px; color: var(--subtle); text-align: left; }
  .secs button:hover { background: #ffffff06; }
  .secs button.on { background: var(--raise); color: var(--text); box-shadow: inset 3px 0 0 var(--acc); }
  .content { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 12px; }
  .bar { display: flex; align-items: center; gap: 10px; }
  .savebar { position: fixed; bottom: 18px; left: 50%; transform: translateX(-50%); z-index: 30; gap: 10px; padding: 8px 12px; box-shadow: 0 12px 40px #000c; border-color: #f9731677; width: min(560px, calc(100vw - 32px)); }
  .find { padding: 4px 9px; width: 260px; font-weight: 400; }
  .grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(440px, 1fr)); gap: 14px; align-items: start; }
  .span { grid-column: 1 / -1; }
  section.card { display: flex; flex-direction: column; gap: 4px; }
  section { scroll-margin-top: 48px; }
  section h3 { margin-bottom: 4px; }
  .f { display: flex; align-items: center; gap: 16px; justify-content: space-between; padding: 7px 2px; border-bottom: 1px solid var(--line); }
  .f:last-child { border-bottom: 0; }
  .f.dirty { background: var(--acc-dim); }
  .lab { min-width: 0; }
  .f input[type='checkbox'] { width: 16px; height: 16px; accent-color: var(--acc); flex: none; }
  .w { width: 110px; padding: 4px 8px; flex: none; }
  .wide { width: 240px; padding: 4px 8px; flex: none; }
  .sel { background: var(--base); border: 1px solid var(--line2); border-radius: 6px; padding: 5px 8px; color: var(--text); flex: none; }
  .range { width: 100%; accent-color: var(--acc); }
  .gap { margin-top: 10px; }
  .wrap { flex-wrap: wrap; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .flags { white-space: pre-wrap; word-break: break-all; margin-top: 6px; max-height: 240px; overflow: auto; }
  @media (max-width: 900px) { .layout { flex-direction: column; } .secs { flex-direction: row; flex-wrap: wrap; width: auto; position: static; } .grid { grid-template-columns: 1fr; } .wide { width: 160px; } }
</style>
