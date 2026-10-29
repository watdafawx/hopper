<script>
  // Settings › Address: the addresses players join this server by, and
  // sleeping (stop when empty, start when someone joins).
  import { Globe, Save, Plus, X, TriangleAlert, ArrowRightLeft, SearchCheck } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { session } from './lib/me.svelte.js';
  import { routerChip } from './lib/router.js';

  let { server } = $props();
  // The live feed hands over a new `server` object every second; effects
  // that should run once per server key on the id alone.
  const sid = $derived(server.id);
  let info = $state(null);
  let router = $state(null);
  let addrs = $state([]);
  // No address at all, not even the default name.<domain>.
  let off = $state(false);
  let sleepOn = $state(false);
  let minutes = $state(15);
  let saved = $state('');
  let saving = $state(false);
  let dns = $state(null);
  let checking = $state(false);

  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);
  const domain = $derived(router?.config?.domain || '');
  const typed = $derived(addrs.map((a) => a.trim()).filter(Boolean));
  // Just the default typed back counts as "use the default", so it keeps
  // following renames.
  const useDefault = $derived(!off && !!info?.default_address && typed.length === 1 && typed[0] === info.default_address);
  const body = $derived({
    addresses: off || useDefault ? [] : typed,
    sleep_after_minutes: sleepOn ? Math.max(1, Number(minutes) || 1) : 0,
    off,
  });
  // What players join at, as saved (default included).
  const live = $derived(info?.effective || []);
  const dirty = $derived(JSON.stringify(body) !== saved);
  const clash = $derived(info && router?.config?.enabled && info.port === info.router_port);
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));
  const chip = $derived(router && routerChip(router));
  // Minecraft joins on 25565 unless the player types a port (or DNS has an
  // SRV record), so a router on another port needs a word.
  const offPort = $derived(live.length && router?.config?.enabled && !clash && router.config.port !== router.join_port);

  async function load() {
    try {
      [info, router] = await Promise.all([api(`${base}/address`), api('/router')]);
      addrs = info.effective.length ? [...info.effective] : [''];
      off = info.off;
      sleepOn = info.sleep_after_minutes > 0;
      minutes = info.sleep_after_minutes || 15;
      saved = JSON.stringify({ addresses: info.addresses, sleep_after_minutes: info.sleep_after_minutes, off: info.off });
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
    if (v) off = false;
  }
  function remove(i) {
    if (addrs.length > 1) addrs = addrs.filter((_, j) => j !== i);
    else {
      addrs = [''];
      // With a default to fall back on, clearing the only address turns
      // the address off; otherwise it's simply empty.
      off = !!info.default_address;
    }
  }
  function resetDefault() {
    addrs = [info.default_address];
    off = false;
  }

  async function save() {
    saving = true;
    try {
      const r = await api(`${base}/address`, { method: 'PUT', body });
      say(r.addresses.length ? `Players can join at ${r.addresses[0]}` : off ? 'Address turned off' : 'Address removed');
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

  async function useJoinPort() {
    try {
      router = await api('/router', { method: 'PUT', body: { ...router.config, port: router.join_port } });
      say(router.status.error || `Players join on port ${router.join_port} now`, !!router.status.error);
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
    {#if dirty}<span class="unsaved">Not saved</span>{/if}
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
          <div class="input name"><input class="mono" value={nameOf(a)} placeholder="name" oninput={(e) => setName(i, e.currentTarget.value.trim().toLowerCase())} /></div>
          <span class="mono tiny muted ell">{suffix}</span>
        {:else}
          <div class="input full"><input class="mono" bind:value={addrs[i]} placeholder="survival.example.com" /></div>
        {/if}
        <span class="sp"></span>
        {#if dnsOf(a)}<span class="chip" class:running={dnsOf(a).ok} class:crashed={!dnsOf(a).ok} title={dnsOf(a).ips.join(', ')}>{dnsOf(a).ok ? '✓ DNS' : dnsOf(a).note}</span>{/if}
        {#if i === 0 && useDefault}<span class="chip lan" title="From the server's name">Default</span>
        {:else if i === 0 && off}<span class="chip">Off</span>
        {:else if i === 0 && info.default_address && a}<span class="chip acc">Custom</span>{/if}
        {#if addrs.length > 1 || a}<button class="btn sm ghost icon" title={addrs.length === 1 && info.default_address ? 'Turn off' : 'Remove'} onclick={() => remove(i)}><X size={13} /></button>{/if}
      </div>
    {/each}
    {#if off}
      <div class="tiny muted">No address: players can't join by name.{#if info.default_address}{' '}<button class="lnk" onclick={resetDefault}>Use default ({nameOf(info.default_address)})</button>{/if}</div>
    {:else if useDefault}
      <div class="tiny muted">From the server's name. Type another name and Save to change it.</div>
      {#if info.default_taken}<div class="tiny muted">{info.default_taken.by} already has <span class="mono">{info.default_taken.address}</span>, so this one gets a number.</div>{/if}
    {:else if dirty && typed.length}
      <div class="tiny gold">Save to start using <span class="mono">{typed[0]}</span>.{#if info.default_address}{' '}<button class="lnk" onclick={resetDefault}>Use default ({nameOf(info.default_address)})</button>{/if}</div>
    {:else if typed.length && info.default_address}
      <div class="tiny"><button class="lnk" onclick={resetDefault}>Use default ({nameOf(info.default_address)})</button></div>
    {:else if !live.length && !dirty}
      <div class="tiny muted">No address yet. Type a name and Save; players then join at <span class="mono">{domain ? `name.${domain}` : 'that address'}</span>.</div>
    {/if}
    {#if offPort}
      <div class="warn small"><TriangleAlert size={13} /><div><span>Players typing <span class="mono">{live[0]}</span> connect on port {router.join_port}, but the router is on {router.config.port}.</span>
        {#if session.owner}<div><button class="btn sm" onclick={useJoinPort}>Use {router.join_port}</button></div>{/if}
        <span class="tiny muted">Keeping {router.config.port}: players add <span class="mono">:{router.config.port}</span>, or add an SRV record pointing at it.{session.owner ? '' : ' The owner or a co-owner can change the port in Access & devices.'}</span></div></div>
    {/if}
    <button class="btn sm ghost add" onclick={() => (addrs = [...addrs, ''])}><Plus size={13} />Add another address</button>
    <label class="row small"><input type="checkbox" bind:checked={sleepOn} />Sleep when empty for
      <span class="input mins"><input type="number" min="1" max="1440" bind:value={minutes} disabled={!sleepOn} /></span>min</label>
    <div class="tiny muted">
      {#if sleepOn}Joining starts it again; the server list shows "Sleeping" meanwhile.
      {:else}Off: it runs until someone stops it.{/if}
    </div>
    {#if router.state === 'error'}<div class="err small">{router.status.error}</div>{/if}
    <div class="status small">
      <span class="muted">Router</span><span class="chip {chip.cls}">{chip.label}</span>
      {#if router.state !== 'error'}<span class="tiny muted ell">{chip.note}{router.state === 'running' && !domain ? '. Set a domain in Access & devices to type only names.' : ''}</span>{/if}
      <span class="sp"></span>
      {#if live.length}<button class="btn sm ghost" disabled={checking} onclick={check} title="Check DNS"><SearchCheck size={13} />{checking ? 'Checking…' : 'Check DNS'}</button>{/if}
    </div>
    {#if router.state === 'off'}<div class="tiny muted">Turn server addresses on in <a href="#/access">Access &amp; devices</a>.</div>{/if}
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
  .unsaved { color: var(--gold); font-size: 11.5px; font-weight: 600; }
  .gold { color: var(--gold); }
  .lnk { background: none; border: none; padding: 0; font: inherit; color: var(--acc); cursor: pointer; }
  .status { display: flex; align-items: center; gap: 8px; border-top: 1px solid var(--line); padding-top: 8px; min-width: 0; }
  .warn .tiny { color: var(--muted); }
  label.row input[type='checkbox'] { width: 15px; height: 15px; accent-color: var(--acc); }
</style>
