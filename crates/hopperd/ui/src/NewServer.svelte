<script>
  import { onMount } from 'svelte';
  import { X, Server } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { followJob } from './lib/jobs.js';

  let { close } = $props();

  const LOADERS = [
    ['vanilla', 'Vanilla'],
    ['neoforge', 'NeoForge'],
    ['fabric', 'Fabric'],
    ['quilt', 'Quilt'],
  ];

  let versions = $state([]);
  let loaders = $state({});
  let name = $state('');
  let mc = $state('');
  let loader = $state('neoforge');
  let lv = $state('');
  let memory = $state(6);
  let port = $state('');
  /** The router's settings, for the address field. */
  let router = $state(null);
  /** The address's name part (with a domain) or the whole address; null: follow the server name. */
  let addrEdit = $state(null);
  let eula = $state(false);
  let busy = $state('');
  let error = $state('');

  const options = $derived(loaders[loader] || []);
  const domain = $derived(router?.config?.enabled ? router.config.domain : '');
  const slug = (n) => n.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');
  const addrName = $derived(addrEdit ?? (domain ? slug(name) : ''));
  const address = $derived(!addrName ? '' : domain && !addrName.includes('.') ? `${addrName}.${domain}` : addrName);
  api('/router').then((r) => (router = r), () => {});
  const canMake = $derived(name.trim() && mc && eula && (loader === 'vanilla' || loader === 'fabric' || loader === 'quilt' || lv) && !busy);

  async function loadLoaders() {
    if (!mc) return;
    loaders = {};
    try {
      loaders = (await api(`/meta/loaders/${encodeURIComponent(mc)}`)).loaders;
    } catch {
      loaders = {};
    }
  }

  $effect(() => {
    mc;
    loadLoaders();
  });
  $effect(() => {
    lv = options[0] || '';
  });
  $effect(() => {
    memory = loader === 'vanilla' ? 2 : 6;
  });

  async function make() {
    error = '';
    try {
      const { job } = await post('/servers/create', {
        name: name.trim(), mc_version: mc, loader, loader_version: loader === 'vanilla' ? '' : lv,
        memory_mb: Math.round(memory * 1024), port: port ? Number(port) : null, accept_eula: eula, address,
      });
      busy = 'Starting';
      const r = await followJob(job, (st) => (busy = st.detail || 'Working'));
      say(`Made ${name.trim()}`);
      close();
      if (r?.id) location.hash = `/server/${encodeURIComponent(r.id)}/console`;
    } catch (e) {
      error = e.message;
      busy = '';
    }
  }

  onMount(async () => {
    try {
      versions = (await api('/meta/minecraft')).versions;
      mc = versions[0] || '';
    } catch (e) {
      error = `Couldn't list Minecraft versions: ${e.message}`;
    }
  });
</script>

<div class="back" role="presentation" onclick={(e) => e.target === e.currentTarget && !busy && close()}>
  <div class="box card">
    <h3><Server size={15} />New server<span class="sp"></span>
      <button class="btn sm icon ghost" disabled={!!busy} onclick={close}><X size={14} /></button></h3>
    <label class="lbl" for="ns-name">Name</label>
    <div class="input"><input id="ns-name" bind:value={name} placeholder="Survival" disabled={!!busy} /></div>
    <div class="two">
      <div>
        <label class="lbl" for="ns-mc">Minecraft</label>
        <select id="ns-mc" class="sel" bind:value={mc} disabled={!!busy}>{#each versions as v (v)}<option value={v}>{v}</option>{/each}</select>
      </div>
      <div>
        <label class="lbl" for="ns-loader">Loader</label>
        <select id="ns-loader" class="sel" bind:value={loader} disabled={!!busy}>{#each LOADERS as [id, label] (id)}<option value={id}>{label}</option>{/each}</select>
      </div>
    </div>
    {#if loader !== 'vanilla'}
      <label class="lbl" for="ns-lv">{LOADERS.find((l) => l[0] === loader)[1]} version</label>
      {#if options.length}
        <select id="ns-lv" class="sel" bind:value={lv} disabled={!!busy}>{#each options as o (o)}<option value={o}>{o}</option>{/each}</select>
      {:else}
        <div class="small muted">{mc ? `No ${loader} for Minecraft ${mc}.` : ''}</div>
      {/if}
    {/if}
    <div class="two">
      <div>
        <label class="lbl" for="ns-mem">Memory: {memory} GB</label>
        <input id="ns-mem" class="range" type="range" min="1" max="32" step="0.5" bind:value={memory} disabled={!!busy} />
      </div>
      <div>
        <label class="lbl" for="ns-port">Port</label>
        <div class="input"><input id="ns-port" type="number" bind:value={port} placeholder="first free" disabled={!!busy} /></div>
      </div>
    </div>
    {#if router?.config?.enabled}
      <label class="lbl" for="ns-addr">Address players join</label>
      <div class="row">
        <div class="input grow"><input id="ns-addr" class="mono" value={addrName} oninput={(e) => (addrEdit = e.currentTarget.value.trim().toLowerCase())} placeholder={domain ? 'survival' : 'survival.example.com (optional)'} disabled={!!busy} /></div>
        {#if domain && !addrName.includes('.')}<span class="mono small muted">.{domain}</span>{/if}
      </div>
    {/if}
    <label class="eula small"><input type="checkbox" bind:checked={eula} disabled={!!busy} />
      <span>I agree to the <a href="https://aka.ms/MinecraftEULA" target="_blank" rel="noreferrer">Minecraft EULA</a>.</span></label>
    {#if error}<div class="err">{error}</div>{/if}
    {#if busy}<div class="busy small"><span class="spin"></span>{busy}</div>{/if}
    <div class="row end">
      <span class="tiny muted">Java is found or downloaded for you.</span><span class="sp"></span>
      <button class="btn pri" disabled={!canMake} onclick={make}>Make server</button>
    </div>
  </div>
</div>

<style>
  .grow { flex: 1; }
  .back { position: fixed; inset: 0; background: #0008; display: grid; place-items: center; z-index: 55; padding: 16px; }
  .box { width: min(480px, 100%); display: flex; flex-direction: column; gap: 8px; box-shadow: 0 24px 60px #000c; }
  .box h3 { margin: 0 0 4px; }
  .two { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
  .two > div { display: flex; flex-direction: column; gap: 6px; }
  .sel { background: var(--base); border: 1px solid var(--line2); border-radius: 6px; padding: 7px 8px; color: var(--text); width: 100%; }
  .range { width: 100%; accent-color: var(--acc); margin-top: 8px; }
  .eula { display: flex; gap: 8px; align-items: center; margin-top: 6px; }
  .eula input { accent-color: var(--acc); }
  .end { margin-top: 6px; }
  .busy { display: flex; align-items: center; gap: 8px; color: var(--gold); }
  .spin { width: 12px; height: 12px; border: 2px solid var(--gold); border-right-color: transparent; border-radius: 50%; animation: s 0.8s linear infinite; }
  @keyframes s { to { transform: rotate(360deg); } }
</style>
