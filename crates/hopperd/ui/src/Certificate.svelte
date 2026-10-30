<script>
  // Settings › Access & devices: the panel's trusted (Let's Encrypt)
  // certificate, so browsers from the internet don't warn. hopperd picks
  // the name itself: the domain, else the public IP, else its sslip.io name.
  import { onMount, onDestroy } from 'svelte';
  import { ShieldCheck, Copy } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';

  let { port } = $props();
  let s = $state(null);
  let failed = $state(false);
  let busy = $state(false);
  let timer;

  async function load() {
    try {
      s = await api('/settings/certificate');
      failed = false;
    } catch {
      failed = true;
    }
  }

  async function toggle(on) {
    busy = true;
    try {
      s = await api('/settings/certificate', { method: 'PUT', body: { auto: on } });
      say(on ? 'Getting a certificate. This takes a minute.' : 'Automatic certificate off. Browsers get the panel’s own certificate again.');
    } catch (e) {
      say(e.message, true);
    } finally {
      busy = false;
    }
  }

  async function copy() {
    try {
      await navigator.clipboard.writeText(s.share);
      say('Address copied.');
    } catch {
      say('Couldn’t copy. Select the address and copy it.', true);
    }
  }

  const day = 86400;
  const span = (secs) => {
    const d = Math.round(secs / day);
    if (secs < day) return `${Math.max(1, Math.round(secs / 3600))} h`;
    return `${d} day${d === 1 ? '' : 's'}`;
  };
  const now = () => Date.now() / 1000;
  const date = (t) => new Date(t * 1000).toLocaleString();

  onMount(() => {
    load();
    // Follows a certificate being fetched.
    timer = setInterval(() => s?.enabled && load(), 5000);
  });
  onDestroy(() => clearInterval(timer));
</script>

<div class="card">
  <h3><ShieldCheck size={14} />Trusted certificate<span class="sp"></span>
    {#if s}<span class="tiny muted">Automatic</span>
      <label class="switch" title="Get a free Let's Encrypt certificate by itself"><input type="checkbox" checked={s.enabled} disabled={busy} onchange={(e) => toggle(e.target.checked)} /><span></span></label>{/if}
  </h3>
  {#if s}
    {#if s.share}
      <div class="share"><ShieldCheck size={14} /><span class="mono small ell">{s.share}</span><span class="sp"></span>
        <span class="chip running">{s.staging ? 'Test certificate' : 'Trusted'}</span>
        <button class="btn sm" onclick={copy}><Copy size={13} />Copy</button></div>
    {/if}
    {#if s.enabled}
      <div>
        {#if s.covers}<div class="f"><span class="l">Covers</span><span class="mono small">{s.covers}</span></div>{/if}
        {#if s.issuer}<div class="f"><span class="l">Issued by</span><span class="small">{s.issuer}</span></div>{/if}
        {#if s.not_after}<div class="f"><span class="l">Expires</span><span class="small" title={date(s.not_after)}>in {span(s.not_after - now())}{#if s.renew_at} · renews {s.renew_at > now() ? `in ${span(s.renew_at - now())}` : 'now'}{/if}</span></div>{/if}
        {#if s.working}<div class="f"><span class="l">Now</span><span class="small">Getting a certificate{s.public_ip ? ` for ${s.public_ip}` : ''}…</span></div>
        {:else if !s.covers && s.next_try}<div class="f"><span class="l">Next try</span><span class="small">{date(s.next_try)}</span></div>{/if}
        {#if s.ip_change}<div class="f"><span class="l">IP changed</span><span class="small warn">{s.ip_change}</span></div>{/if}
        {#if s.note}<div class="f"><span class="l">Note</span><span class="small">{s.note}</span></div>{/if}
        {#if s.last_error}<div class="f"><span class="l">Last try</span><span class="small err">{s.last_error}</span></div>{/if}
      </div>
      <p class="tiny muted">Port 443 on {s.public_ip ?? 'your public IP'} must reach this panel: forward it to port {port} on your router (port 80 too helps if 443 is blocked). The LAN and paired Hopper desktops keep working either way.</p>
      {#if !s.domain}<p class="tiny muted">Home connection whose IP changes? Then this address changes too. A domain stays the same: a free dynamic DNS name such as <a href="https://www.duckdns.org" target="_blank" rel="noopener">DuckDNS</a>, set as <span class="mono">acme_domain</span> in hopperd.toml.</p>{/if}
    {:else}
      <p class="small subtle">Browsers on the internet warn about the panel's own certificate. Turn this on to get a free one from Let's Encrypt, renewed by itself: for your domain (<span class="mono">acme_domain</span> in hopperd.toml), else your public IP, else its sslip.io name. Port 443 must reach this panel. Not needed behind a reverse proxy that has its own certificate.</p>
    {/if}
  {:else if failed}
    <p class="small muted">Couldn't load the certificate status. Open this page again to retry.</p>
  {:else}
    <div class="skel"><i></i></div><div class="skel"><i></i></div>
  {/if}
</div>

<style>
  .share { display: flex; align-items: center; gap: 10px; padding: 10px; border: 1px solid #22c55e4d; border-radius: var(--radius-lg); background: var(--green-dim); color: var(--green); margin-bottom: 4px; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .f { display: flex; gap: 10px; padding: 6px 0; border-bottom: 1px solid var(--line); }
  .f:last-child { border: 0; }
  .l { width: 92px; flex: none; color: var(--muted); font-size: 12.5px; }
  .err { color: var(--red); }
  .warn { color: var(--gold); }
</style>
