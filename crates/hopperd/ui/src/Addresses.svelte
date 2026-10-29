<script>
  // Access & devices › Server addresses: the router's port and domain,
  // where unknown addresses go, and checks of DNS and the port.
  import { Globe, Save, SearchCheck, TriangleAlert } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { live } from './lib/live.svelte.js';
  import { say } from './lib/toast.svelte.js';
  import { routerChip } from './lib/router.js';

  let r = $state(null);
  let cfg = $state(null);
  let saved = $state('');
  let saving = $state(false);
  let result = $state(null);
  let checking = $state(false);

  const dirty = $derived(cfg && JSON.stringify(cfg) !== saved);
  const withAddr = $derived(live.servers.filter((s) => s.addresses?.length));
  const chip = $derived(r && routerChip(r));

  async function load() {
    try {
      r = await api('/router');
      cfg = { ...r.config };
      saved = JSON.stringify(cfg);
    } catch (e) {
      say(e.message, true);
    }
  }

  async function save() {
    saving = true;
    try {
      r = await api('/router', { method: 'PUT', body: { ...cfg, port: Number(cfg.port) } });
      cfg = { ...r.config };
      saved = JSON.stringify(cfg);
      say('Saved');
    } catch (e) {
      say(e.message, true);
    } finally {
      saving = false;
    }
  }

  async function check() {
    checking = true;
    try {
      result = await post('/router/check');
    } catch (e) {
      say(e.message, true);
    } finally {
      checking = false;
    }
  }

  load();
</script>

<div class="card col">
  <h3><Globe size={14} />Server addresses<span class="sp"></span>
    {#if chip}<span class="chip {chip.cls}" title={chip.note}>{chip.label}{r.state === 'running' ? ` · port ${r.status.listening}` : ''}</span>{/if}
  </h3>
  {#if cfg}
    <p class="small subtle">Players join <span class="mono">survival.{cfg.domain || 'example.com'}</span> instead of an IP and port. One Minecraft port is shared by every server; each server's address is set in its Settings tab.</p>
    {#if r.status.error}<div class="warn small"><TriangleAlert size={13} /><span>{r.status.error}{#if r.clash} <a href="#/server/{r.clash.id}/settings">Open {r.clash.name}</a>{/if}</span></div>{/if}
    <label class="row small"><input type="checkbox" bind:checked={cfg.enabled} />Route players by the address they join</label>
    <div class="f"><div><div>Port players join on</div><div class="tiny muted">25565 means nobody has to type a port.</div></div>
      <div class="input w"><input type="number" min="1" max="65535" bind:value={cfg.port} /></div></div>
    <div class="f"><div><div>Domain</div><div class="tiny muted">Servers get <span class="mono">name.{cfg.domain || 'this-domain'}</span> by default.</div></div>
      <div class="input wide"><input class="mono" bind:value={cfg.domain} placeholder="mc.example.com" /></div></div>
    <div class="f stack"><div><div>Unknown addresses</div><div class="tiny muted">An address no server has, or this PC's IP.</div></div>
      <select class="sel" bind:value={cfg.unknown_server}>
        <option value="">Say "No server at this address"</option>
        {#each live.servers as s (s.id)}<option value={s.id}>Send them to {s.name}</option>{/each}
      </select></div>
    <div class="row"><span class="sp"></span>
      <button class="btn sm" disabled={checking || !withAddr.length && !cfg.domain} onclick={check}><SearchCheck size={13} />{checking ? 'Checking…' : 'Check DNS and port'}</button>
      <button class="btn sm pri" disabled={!dirty || saving} onclick={save}><Save size={13} />Save</button></div>
    {#if !withAddr.length && cfg.enabled}<div class="tiny muted">The router starts once a server has an address{cfg.domain ? '' : ': set a domain to give every server one'}.</div>{/if}
    {#if live.servers.length}
      <div class="lbl">Players join at</div>
      <div class="list">
        {#each live.servers as s (s.id)}
          <a class="srow" href="#/server/{encodeURIComponent(s.id)}/settings" title="Change it in {s.name}'s Settings">
            <span class="ell">{s.name}</span>
            {#if s.addresses?.length}<span class="mono ell">{s.addresses[0]}{s.addresses.length > 1 ? ` +${s.addresses.length - 1}` : ''}</span>
              <span class="chip" class:lan={s.address_default} class:acc={!s.address_default}>{s.address_default ? 'Default' : 'Custom'}</span>
            {:else}<span class="muted">No address</span>{#if r.config.enabled && r.config.domain}<span class="chip" title="Turned off in its Settings">Off</span>{/if}{/if}
          </a>
        {/each}
      </div>
    {/if}
    {#if result}
      <div class="lbl">DNS records</div>
      {#if cfg.domain && result.public_ip}
        <div class="tiny muted">Make this one record where the domain is managed; it covers every server under {cfg.domain}:</div>
        <div class="dns mono"><b>Type</b><b>Name</b><b>Value</b><span>A</span><span>*.{cfg.domain}</span><span>{result.public_ip}</span></div>
      {/if}
      {#each result.dns as d (d.name)}
        <div class="row small"><span class="mono ell">{d.name}</span><span class="sp"></span>
          <span class="chip" class:running={d.ok} class:crashed={!d.ok} title={d.ips.join(', ')}>{d.ok ? '✓ points here' : d.note}</span></div>
      {/each}
      <div class="lbl">Port</div>
      <div class="small" class:okc={result.port.reachable === true} class:badc={result.port.reachable === false}>Port {result.port.port}: {result.port.note}.</div>
      {#if result.port.reachable === false && result.lan?.length}<div class="tiny muted">Forward TCP {result.port.port} on the router to {result.lan[0]}. Players on the LAN and Tailscale can join already.</div>{/if}
    {/if}
  {/if}
</div>

<style>
  .f { display: flex; align-items: center; gap: 16px; justify-content: space-between; padding: 6px 0; border-bottom: 1px solid var(--line); }
  .w { width: 100px; padding: 4px 8px; flex: none; }
  .wide { width: 190px; padding: 4px 8px; flex: none; }
  .sel { background: var(--base); border: 1px solid var(--line2); border-radius: 6px; padding: 5px 8px; color: var(--text); }
  .f.stack { flex-direction: column; align-items: stretch; gap: 6px; }
  label.row input[type='checkbox'] { width: 15px; height: 15px; accent-color: var(--acc); }
  .warn { display: flex; gap: 8px; align-items: flex-start; color: var(--gold); background: var(--gold-dim); border: 1px solid #eab30855; border-radius: 8px; padding: 8px 10px; }
  .warn span { color: var(--text); }
  .dns { display: grid; grid-template-columns: 50px 1fr 1fr; gap: 3px 10px; font-size: 12px; background: var(--base); border: 1px solid var(--line); border-radius: 6px; padding: 6px 10px; }
  .dns b { font: 600 10.5px Inter, system-ui, sans-serif; text-transform: uppercase; letter-spacing: .05em; color: var(--muted); }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .okc { color: var(--green); }
  .list { display: flex; flex-direction: column; }
  .srow { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1.4fr) auto; gap: 10px; align-items: center; padding: 5px 8px; border-radius: 6px; color: inherit; text-decoration: none; font-size: 12.5px; }
  .srow:hover { background: var(--hover); }
  .srow .mono { font-size: 12px; }
  .badc { color: var(--gold); }
</style>
