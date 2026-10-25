<script>
  import { SlidersHorizontal, MemoryStick, Save, Search, ChevronDown, ChevronRight } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import Address from './Address.svelte';

  let { server } = $props();
  let props = $state(null);
  let values = $state({});
  /** What the form started with (file values, else Minecraft's defaults). */
  let orig = $state({});
  let jvm = $state(null);
  let mem = $state(4096);
  let filter = $state('');
  let showOther = $state(false);
  let saving = $state(false);

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const known = $derived(new Set((props?.specs || []).map((s) => s.key)));
  const specs = $derived((props?.specs || []).filter((s) => !filter || `${s.label} ${s.key}`.toLowerCase().includes(filter.toLowerCase())));
  const others = $derived(Object.keys(values).filter((k) => !known.has(k) && (!filter || k.includes(filter.toLowerCase()))).sort());
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

  const gb = (mb) => (mb >= 1024 ? `${(mb / 1024).toFixed(mb % 1024 ? 1 : 0)} GB` : `${mb} MB`);

  $effect(() => {
    server.id;
    load();
  });
</script>

<div class="cols">
  <div class="card">
    <h3><SlidersHorizontal size={14} />server.properties<span class="sp"></span>
      <div class="input find"><Search size={13} /><input placeholder="Find setting" bind:value={filter} /></div>
      <button class="btn sm pri" disabled={!changed.length || saving} onclick={saveProps}><Save size={13} />Save{changed.length ? ` ${changed.length}` : ''}</button>
    </h3>
    {#each specs as s (s.key)}
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
    {/each}
    <button class="btn sm ghost more" onclick={() => (showOther = !showOther)}>
      {#if showOther}<ChevronDown size={13} />{:else}<ChevronRight size={13} />{/if}Other keys ({others.length})
    </button>
    {#if showOther}
      {#each others as k (k)}
        <div class="f"><div class="mono small">{k}</div><div class="input wide"><input bind:value={values[k]} /></div></div>
      {/each}
    {/if}
  </div>

  <div class="stack">
  <Address {server} />
  <div class="card side">
    <h3><MemoryStick size={14} />Memory &amp; Java</h3>
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
        {#if jvm.preset === 'custom'}<span class="chip">custom flags</span>{/if}
      </div>
      <div class="lbl gap">Java</div>
      <div class="small mono ell" title={jvm.java || ''}>{jvm.java || 'java on PATH'}</div>
      <div class="tiny muted">{jvm.java_major ? `Java ${jvm.java_major}` : 'Version unknown'} · from run.bat</div>
      <details class="tiny muted"><summary>Flags ({jvm.flags.length})</summary><div class="mono flags">{jvm.flags.join(' ')}</div></details>
    {/if}
  </div>
  </div>
</div>

<style>
  .cols { display: flex; gap: 14px; align-items: flex-start; }
  .cols > .card:first-child { flex: 1; min-width: 0; }
  .stack { display: flex; flex-direction: column; gap: 14px; flex: none; }
  .side { width: 320px; flex: none; display: flex; flex-direction: column; gap: 8px; }
  .find { padding: 3px 8px; width: 170px; font-weight: 400; }
  .f { display: flex; align-items: center; gap: 16px; justify-content: space-between; padding: 8px 4px; border-bottom: 1px solid var(--line); }
  .f.dirty { background: var(--acc-dim); }
  .lab { min-width: 0; }
  .f input[type='checkbox'] { width: 16px; height: 16px; accent-color: var(--acc); flex: none; }
  .w { width: 110px; padding: 4px 8px; flex: none; }
  .wide { width: 240px; padding: 4px 8px; flex: none; }
  .sel { background: var(--base); border: 1px solid var(--line2); border-radius: 6px; padding: 5px 8px; color: var(--text); flex: none; }
  .more { margin-top: 8px; align-self: flex-start; }
  .range { width: 100%; accent-color: var(--acc); }
  .gap { margin-top: 10px; }
  .wrap { flex-wrap: wrap; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .flags { word-break: break-all; margin-top: 6px; }
  @media (max-width: 900px) { .cols { flex-direction: column; } .stack { align-self: stretch; } .side { width: auto; } .wide { width: 160px; } }
</style>
