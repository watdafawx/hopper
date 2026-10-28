<script>
  // Alert rules, beside Webhooks: they fire to the hooks that have the
  // matching chip, and show on the server's page.
  import { onMount } from 'svelte';
  import { BellRing } from '@lucide/svelte';
  import { api } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { alerts, refreshAlerts } from './lib/alerts.svelte.js';

  let s = $state(null);
  let dirty = $state(false);
  let failed = $state(false);

  onMount(async () => {
    await refreshAlerts();
    s = alerts.settings ? { ...alerts.settings } : null;
    failed = !s;
  });

  const set = (k, v) => {
    s[k] = v;
    dirty = true;
  };

  async function save() {
    try {
      const r = await api('/alerts/settings', { method: 'PUT', body: { ...s, tps_below: Number(s.tps_below), disk_gb: Number(s.disk_gb), ram_pct: Number(s.ram_pct), tps_minutes: Number(s.tps_minutes), ram_minutes: Number(s.ram_minutes) } });
      s = { ...r.settings };
      dirty = false;
      say('Alerts saved');
    } catch (e) {
      say(e.message, true);
    }
  }
</script>

<div class="card">
  <h3><BellRing size={14} />Alerts<span class="sp"></span>{#if dirty}<button class="btn sm pri" onclick={save}>Save</button>{/if}</h3>
  {#if s}
    <div class="f"><div class="l">Crashed<small>Shown on the server; webhooks get "Server crashed"</small></div>
      <label class="switch"><input type="checkbox" checked={s.crash} onchange={(e) => set('crash', e.target.checked)} /><span></span></label></div>
    <div class="f"><div class="l">TPS below<small>For {s.tps_minutes} min. Servers with RCON only (the Console offers to turn it on).</small></div>
      <div class="input w"><input type="number" min="1" max="20" step="0.5" value={s.tps_below} oninput={(e) => set('tps_below', e.target.value)} /></div>
      <label class="switch"><input type="checkbox" checked={s.tps_on} onchange={(e) => set('tps_on', e.target.checked)} /><span></span></label></div>
    <div class="f"><div class="l">Drive free below<small>GB, on each server's drive and the panel's</small></div>
      <div class="input w"><input type="number" min="1" value={s.disk_gb} oninput={(e) => set('disk_gb', e.target.value)} /></div>
      <label class="switch"><input type="checkbox" checked={s.disk_on} onchange={(e) => set('disk_on', e.target.checked)} /><span></span></label></div>
    <div class="f"><div class="l">PC memory above<small>% in use, for {s.ram_minutes} min</small></div>
      <div class="input w"><input type="number" min="50" max="100" value={s.ram_pct} oninput={(e) => set('ram_pct', e.target.value)} /></div>
      <label class="switch"><input type="checkbox" checked={s.ram_on} onchange={(e) => set('ram_on', e.target.checked)} /><span></span></label></div>
    <p class="tiny muted">Each alert fires once, then again only after it has been clear for 10 minutes. Pick "Low TPS", "Drive nearly full" and "PC nearly out of memory" in a webhook to get them on Discord.</p>
  {:else if failed}
    <p class="small muted">Couldn't load the alert rules. Open this page again to retry.</p>
  {:else}
    <div class="skel"><i></i></div><div class="skel"><i></i></div><div class="skel"><i></i></div>
  {/if}
</div>

<style>
  .f { display: flex; align-items: center; gap: 10px; padding: 7px 0; border-bottom: 1px solid var(--line); }
  .l { flex: 1; font-size: 12.5px; }
  .l small { display: block; color: var(--muted); font-size: 11px; }
  .input.w { width: 72px; }
  p { margin-top: 8px; }
</style>
