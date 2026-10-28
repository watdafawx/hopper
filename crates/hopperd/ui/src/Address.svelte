<script>
  // Settings › Address: the addresses players join this server by, and
  // sleeping (stop when empty, start when someone joins).
  import { Globe, Save, Plus, X, TriangleAlert, ArrowRightLeft, SearchCheck } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';

  let { server } = $props();
  // The live feed hands over a new `server` object every second; effects
  // that should run once per server key on the id alone.
  const sid = $derived(server.id);
  let info = $state(null);
  let router = $state(null);
  let addrs = $state([]);
  let sleepOn = $state(false);
  let minutes = $state(15);
  let saved = $state('');
  let saving = $state(false);
  let dns = $state(null);
  let checking = $state(false);

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const domain = $derived(router?.config?.domain || '');
  const body = $derived({
    addresses: addrs.map((a) => a.trim()).filter(Boolean),
    sleep_after_minutes: sleepOn ? Math.max(1, Number(minutes) || 1) : 0,
  });
  const dirty = $derived(JSON.stringify(body) !== saved);
  const clash = $derived(info && router?.config?.enabled && info.port === info.router_port);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));

  async function load() {
    try {
      [info, router] = await Promise.all([api(`${base}/address`), api('/router')]);
      addrs = info.addresses.length ? [...info.addresses] : [''];
      sleepOn = info.sleep_after_minutes > 0;
      minutes = info.sleep_after_minutes || 15;
      saved = JSON.stringify({ addresses: info.addresses, sleep_after_minutes: info.sleep_after_minutes });
      dns = null;
    } catch (e) {
      say(e.message, true);
    }
  }

  // With a domain set, an address under it is edited as just its name.
  const suffix = $derived(domain ? `.${domain}` : '');
  const under = (a) => suffix && (a === '' || a.endsWith(suffix));
  const nameOf = (a) => (a.endsWith(suffix) ? a.slice(0, -suffix.length) : a);
  function setName(i, v) {
    addrs[i] = v.includes('.') ? v : v ? v + suffix : '';
  }

  async function save() {
    saving = true;
    try {
      const r = await api(`${base}/address`, { method: 'PUT', body });
      say(r.addresses.length ? `Players can join at ${r.addresses[0]}` : 'Address removed');
      await load();
    } catch (e) {
      say(e.message, true);
    } finally {
      saving = false;
    }
  }

  async function movePort() {
    try {
      const r = await post(`${base}/address/move-port`);
      say(`${server.name} now uses port ${r.port}. Players still join on ${info.router_port}.`);
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function check() {
    checking = true;
    try {
      const r = await post('/router/check');
      dns = Object.fromEntries(r.dns.map((d) => [d.name, d]));
    } catch (e) {
      say(e.message, true);
    } finally {
      checking = false;
    }
  }
  /** The DNS result for an address: its own, else its domain's wildcard. */
  const dnsOf = (a) => dns && (dns[a] || (suffix && a.endsWith(suffix) ? dns[`*.${domain}`] : null));

  $effect(() => {
    sid;
    load();
  });
</script>

<div class="card side col">
  <h3><Globe size={14} />Address<span class="sp"></span>
    {#if info?.addresses.length}<button class="btn sm ghost" disabled={checking} onclick={check} title="Check DNS"><SearchCheck size={13} />{checking ? 'Checking…' : 'Check DNS'}</button>{/if}
    <button class="btn sm pri" disabled={!dirty || saving} onclick={save}><Save size={13} />Save</button>
  </h3>
  {#if info && router}
    {#if clash}
      <div class="warn small"><TriangleAlert size={13} /><div>This server uses port {info.port}, the port players join on. Give it its own port so the router can take {info.port}.
        <div><button class="btn sm" disabled={alive} title={alive ? 'Stop it first' : ''} onclick={movePort}><ArrowRightLeft size={13} />Move it</button>{#if alive}<span class="tiny muted"> Stop it first.</span>{/if}</div></div></div>
    {/if}
    {#each addrs as a, i (i)}
      <div class="row">
        {#if under(a)}
          <div class="input name"><input class="mono" value={nameOf(a)} placeholder={server.id} oninput={(e) => setName(i, e.currentTarget.value.trim().toLowerCase())} /></div>
          <span class="mono tiny muted ell">{suffix}</span>
        {:else}
          <div class="input full"><input class="mono" bind:value={addrs[i]} placeholder="survival.example.com" /></div>
        {/if}
        <span class="sp"></span>
        {#if dnsOf(a)}<span class="chip" class:running={dnsOf(a).ok} class:crashed={!dnsOf(a).ok} title={dnsOf(a).ips.join(', ')}>{dnsOf(a).ok ? '✓ DNS' : dnsOf(a).note}</span>{/if}
        {#if addrs.length > 1 || a}<button class="btn sm ghost icon" title="Remove" onclick={() => (addrs = addrs.length > 1 ? addrs.filter((_, j) => j !== i) : [''])}><X size={13} /></button>{/if}
      </div>
    {/each}
    <button class="btn sm ghost add" onclick={() => (addrs = [...addrs, ''])}><Plus size={13} />Add another address</button>
    <label class="row small"><input type="checkbox" bind:checked={sleepOn} />Sleep when empty for
      <span class="input mins"><input type="number" min="1" max="1440" bind:value={minutes} disabled={!sleepOn} /></span>min</label>
    <div class="tiny muted">
      {#if sleepOn}Joining starts it again; the server list shows "Sleeping" meanwhile.
      {:else}Off: it runs until someone stops it.{/if}
    </div>
    {#if !router.config.enabled}
      <div class="tiny muted">Server addresses are off. Turn them on in <a href="#/access">Access &amp; devices</a>.</div>
    {:else if router.status.error}
      <div class="err small">{router.status.error}</div>
    {:else if router.status.listening}
      <div class="tiny muted">Players join on port {router.status.listening}.{domain ? '' : ' Set a domain in Access & devices to type only names.'}</div>
    {:else}
      <div class="tiny muted">The router starts on port {router.config.port} once a server has an address.</div>
    {/if}
  {/if}
</div>

<style>
  .side { gap: 8px; }
  .side h3 { margin-bottom: 2px; }
  .name { width: 120px; padding: 4px 8px; flex: none; }
  .full { flex: 1; padding: 4px 8px; min-width: 0; }
  .mins { width: 64px; padding: 2px 6px; display: inline-flex; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .add { align-self: flex-start; }
  .icon { padding: 3px 5px; }
  .warn { display: flex; gap: 8px; align-items: flex-start; color: var(--gold); background: var(--gold-dim); border: 1px solid #eab30855; border-radius: 8px; padding: 8px 10px; }
  .warn > div { display: flex; flex-direction: column; gap: 6px; color: var(--text); }
  label.row input[type='checkbox'] { width: 15px; height: 15px; accent-color: var(--acc); }
</style>
