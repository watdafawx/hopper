<script>
  import { onMount } from 'svelte';
  import { Link, Check, X, Network, Wifi, Globe, Waypoints, Lock, MonitorSmartphone, Trash2, FolderTree } from '@lucide/svelte';
  import Security from './Security.svelte';
  import Webhooks from './Webhooks.svelte';
  import Users from './Users.svelte';
  import { pathKind } from './lib/format.js';

  const kindOf = (url) => pathKind(url.split('://')[1].split(/[:/]/)[0].replace(/^\[|\]$/g, ''));
  import { api, post, del } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';

  let { hello, me, pending = $bindable([]) } = $props();
  const owner = $derived(me?.role === 'owner');
  let devices = $state([]);

  async function loadDevices() {
    try {
      devices = await api('/devices');
    } catch (e) {
      say(e.message, true);
    }
  }

  async function answer(r, allow) {
    try {
      await post(`/pair/${r.id}/answer`, { allow });
      pending = pending.filter((p) => p.id !== r.id);
      say(allow ? `${r.device} can now use this panel.` : `Refused ${r.device}.`);
      loadDevices();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function forget(d) {
    if (!confirm(`Remove ${d.name}? It will have to pair again.`)) return;
    try {
      await del(`/devices/${d.id}`);
      loadDevices();
    } catch (e) {
      say(e.message, true);
    }
  }

  const when = (t) => new Date(t * 1000).toLocaleString();

  onMount(() => owner && loadDevices());
</script>

<div class="pad">
  {#if !owner}
  <h2>Your account</h2>
  <div class="grid">
    <div class="card"><h3>{me?.name}</h3><p class="small subtle">Role: <b class="txt">{me?.role}</b> · {me?.servers?.length ? `${me.servers.length} server${me.servers.length === 1 ? '' : 's'}` : 'all servers'}. The owner manages what you can do.</p></div>
    <Security own />
  </div>
  {:else}
  <h2>Access &amp; devices</h2>
  <div class="grid">
    <div class="card">
      <h3><Link size={14} />Pair a device</h3>
      {#each pending as r (r.id)}
        <div class="req">
          <div class="small subtle"><b class="txt">{r.device}</b> wants to connect from <span class="mono">{r.ip}</span>. Check it shows the same code.</div>
          <div class="code">{#each r.code.split('') as c, i (i)}<span>{c}</span>{/each}</div>
          <div class="row center">
            <button class="btn" onclick={() => answer(r, false)}><X size={14} />Refuse</button>
            <button class="btn pri" onclick={() => answer(r, true)}><Check size={14} />Allow {r.device}</button>
          </div>
        </div>
      {:else}
        <p class="small subtle">
          No requests. In Hopper desktop, add a Hopper Server panel and pick this one; its request and a 6-digit code show
          up here to allow.
        </p>
      {/each}
    </div>

    <div class="card">
      <h3><Network size={14} />How clients reach this panel</h3>
      <div class="col">
        {#each hello.addresses as a (a)}
          {@const k = kindOf(a)}
          <div class="path" class:ts={k === 'Tailscale'} class:net={k === 'Internet'}>
            {#if k === 'Tailscale'}<Waypoints size={14} />{:else if k === 'Internet'}<Globe size={14} />{:else}<Wifi size={14} />{/if}
            <span class="mono small ell">{a}</span><span class="sp"></span><span class="chip" class:lan={k === 'LAN' || k === 'Local'} class:ts={k === 'Tailscale'} class:acc={k === 'Internet'}>{k}</span>
          </div>
        {:else}
          <div class="small muted">No address found.</div>
        {/each}
      </div>
      {#if hello.tls}
        <div class="fp"><Lock size={13} /><div><div class="tiny muted">Certificate fingerprint (Hopper desktop shows the same when pairing)</div>
          <div class="mono tiny">{hello.tls.fingerprint}</div></div></div>
      {/if}
      {#if hello.sftp}
        <div class="fp sftp"><FolderTree size={13} /><div><div class="tiny muted">SFTP (FileZilla, WinSCP) on port <b class="txt">{hello.sftp.port}</b>: your name and password from the LAN or Tailscale, or an API token as the password from anywhere. Host key</div>
          <div class="mono tiny">{hello.sftp.fingerprint}</div></div></div>
      {/if}
      <p class="tiny muted note">Plain HTTP works from this PC, the LAN and Tailscale; from the internet only HTTPS, and signing in needs two-factor. Browsers warn about the panel's own certificate; set a domain (<span class="mono">acme_domain</span> in hopperd.toml, port 443 forwarded) for a Let's Encrypt one. Announced on the LAN as <span class="mono">_hopperd._tcp</span>.</p>
    </div>

    <Security />

    <Users />

    <Webhooks />

    <div class="card wide">
      <h3><MonitorSmartphone size={14} />Paired devices</h3>
      {#each devices as d (d.id)}
        <div class="row dev">
          <div>
            <div>{d.name}</div>
            <div class="tiny muted">Paired {when(d.created)} · last used {when(d.last_seen)}</div>
          </div>
          <span class="sp"></span>
          <button class="btn sm ghost" title="Remove" onclick={() => forget(d)}><Trash2 size={13} /></button>
        </div>
      {:else}
        <div class="small muted">No devices yet.</div>
      {/each}
    </div>
  </div>
  {/if}
</div>

<style>
  .pad { padding: 18px; }
  h2 { font-size: 17px; margin-bottom: 14px; }
  .grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(320px, 1fr)); gap: 14px; }
  .wide { grid-column: 1 / -1; }
  .txt { color: var(--text); }
  .req { padding-bottom: 12px; border-bottom: 1px solid var(--line); margin-bottom: 12px; }
  .req:last-child { border: 0; margin: 0; padding: 0; }
  .code { display: flex; gap: 8px; justify-content: center; margin: 14px 0; }
  .code span { width: 42px; height: 52px; border-radius: 9px; background: var(--base); border: 1px solid var(--line2); display: grid; place-items: center; font: 600 24px var(--mono); }
  .center { justify-content: center; }
  .path { display: flex; align-items: center; gap: 10px; padding: 10px; border: 1px solid #60a5fa55; border-radius: 8px; background: #60a5fa0d; color: var(--blue); }
  .path.dim { border-color: var(--line); background: var(--base); color: var(--muted); }
  .path.ts { border-color: #a78bfa55; background: #a78bfa0d; color: var(--violet); }
  .path.net { border-color: #f9731655; background: #f973160d; color: var(--acc); }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .fp { display: flex; gap: 8px; align-items: flex-start; margin-top: 10px; color: var(--green); word-break: break-all; }
  .note { margin-top: 10px; }
  .fp.sftp { color: var(--blue); }
  .dev { padding: 8px 0; border-bottom: 1px solid var(--line); }
  .dev:last-child { border: 0; }
</style>
