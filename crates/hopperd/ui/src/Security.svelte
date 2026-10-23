<script>
  import { onMount } from 'svelte';
  import { ShieldCheck, KeyRound, ScrollText, Plus, Trash2, Copy } from '@lucide/svelte';
  import { api, post, del } from './lib/api.js';
  import { live } from './lib/live.svelte.js';
  import { say } from './lib/toast.svelte.js';
  import { confirmIt } from './lib/dialog.svelte.js';

  let twofa = $state(null);
  let setup = $state(null);
  let code = $state('');
  let offPassword = $state('');
  let offCode = $state('');
  let tokens = $state([]);
  let made = $state(null);
  let tName = $state('');
  let tRead = $state(true);
  let tDays = $state('');
  let tServers = $state(new Set());
  let audit = $state([]);

  async function load() {
    try {
      twofa = await api('/auth/2fa');
      tokens = await api('/tokens');
      audit = await api('/audit?limit=60');
    } catch (e) {
      say(e.message, true);
    }
  }

  async function begin() {
    try {
      setup = await post('/auth/2fa/begin');
      code = '';
    } catch (e) {
      say(e.message, true);
    }
  }

  async function enable() {
    try {
      await post('/auth/2fa/enable', { code });
      setup = null;
      say('Two-factor sign-in is on.');
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function disable() {
    try {
      await post('/auth/2fa/disable', { password: offPassword, code: offCode });
      offPassword = offCode = '';
      say('Two-factor sign-in is off.');
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function makeToken() {
    try {
      made = await post('/tokens', {
        name: tName, read_only: tRead, servers: [...tServers], days: tDays ? Number(tDays) : null,
      });
      tName = '';
      tServers = new Set();
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  async function revoke(t) {
    if (!(await confirmIt(`Revoke "${t.name}"?`, { body: 'Anything using it stops working.', ok: 'Revoke', danger: true }))) return;
    try {
      await del(`/tokens/${t.id}`);
      load();
    } catch (e) {
      say(e.message, true);
    }
  }

  function toggleServer(id) {
    const s = new Set(tServers);
    s.has(id) ? s.delete(id) : s.add(id);
    tServers = s;
  }

  const when = (t) => (t ? new Date(t * 1000).toLocaleString() : 'never');
  const netLabel = { local: 'this PC', lan: 'LAN', tailscale: 'Tailscale', internet: 'internet' };

  onMount(load);
</script>

<div class="card">
  <h3><ShieldCheck size={14} />Two-factor sign-in
    <span class="chip" class:running={twofa?.on}>{twofa?.on ? 'on' : 'off'}</span></h3>
  {#if twofa && !twofa.on && !setup}
    <p class="small subtle">Asks for a 6-digit code from an authenticator app when you sign in. Needed to sign in from the internet.</p>
    <button class="btn pri sm" onclick={begin}>Set it up</button>
  {:else if setup}
    <p class="small subtle">Scan this with your authenticator app (Google Authenticator, Aegis, 1Password…), then type the code it shows.</p>
    <div class="qr">{@html setup.qr_svg}</div>
    <div class="tiny muted mono brk">{setup.secret}</div>
    <div class="row">
      <div class="input grow"><input bind:value={code} inputmode="numeric" maxlength="6" placeholder="123456" /></div>
      <button class="btn pri sm" disabled={code.length !== 6} onclick={enable}>Turn on</button>
      <button class="btn sm ghost" onclick={() => (setup = null)}>Cancel</button>
    </div>
  {:else if twofa?.on}
    <p class="small subtle">To turn it off, enter your password and a current code.</p>
    <div class="row">
      <div class="input grow"><input type="password" bind:value={offPassword} placeholder="Password" /></div>
      <div class="input w"><input bind:value={offCode} inputmode="numeric" maxlength="6" placeholder="Code" /></div>
      <button class="btn bad sm" disabled={!offPassword || offCode.length !== 6} onclick={disable}>Turn off</button>
    </div>
  {/if}
</div>

<div class="card">
  <h3><KeyRound size={14} />API tokens</h3>
  <p class="small subtle">For scripts and other apps. Send it as <span class="mono">Authorization: Bearer …</span>.</p>
  {#if made}
    <div class="made">
      <div class="small">Copy it now; it isn't shown again.</div>
      <div class="row"><span class="mono small brk grow">{made.token}</span>
        <button class="btn sm" onclick={() => { navigator.clipboard?.writeText(made.token); say('Copied'); }}><Copy size={13} />Copy</button>
        <button class="btn sm ghost" onclick={() => (made = null)}>Done</button></div>
    </div>
  {/if}
  {#each tokens as t (t.id)}
    <div class="row tk">
      <div class="grow">
        <div>{t.name} <span class="chip">{t.read_only ? 'read only' : 'full'}</span>{#if t.servers.length}<span class="chip">{t.servers.join(', ')}</span>{/if}</div>
        <div class="tiny muted">made {when(t.created)} · used {when(t.last_used)}{t.expires ? ` · expires ${when(t.expires)}` : ''}</div>
      </div>
      <button class="btn sm icon ghost" title="Revoke" onclick={() => revoke(t)}><Trash2 size={13} /></button>
    </div>
  {/each}
  <div class="new">
    <div class="row">
      <div class="input grow"><input bind:value={tName} placeholder="Name, e.g. Grafana" /></div>
      <div class="input w"><input type="number" min="1" bind:value={tDays} placeholder="Days" /></div>
    </div>
    <div class="row wrap">
      <label class="small chk"><input type="checkbox" bind:checked={tRead} />Read only</label>
      <span class="tiny muted">Servers:</span>
      {#each live.servers as s (s.id)}
        <button class="chip" class:on={tServers.has(s.id)} onclick={() => toggleServer(s.id)}>{s.name}</button>
      {/each}
      <span class="tiny muted">{tServers.size ? '' : '(all)'}</span>
      <span class="sp"></span>
      <button class="btn sm pri" disabled={!tName.trim()} onclick={makeToken}><Plus size={13} />Make token</button>
    </div>
  </div>
</div>

<div class="card wide">
  <h3><ScrollText size={14} />Audit log<span class="sp"></span><button class="btn sm ghost" onclick={load}>Refresh</button></h3>
  <div class="audit">
    <table>
      <tbody>
        {#each audit as e, i (i)}
          <tr class:bad={e.status >= 400}>
            <td class="muted nw">{when(e.at)}</td><td>{e.who}</td>
            <td><span class="chip" class:acc={e.net === 'internet'} class:ts={e.net === 'tailscale'}>{netLabel[e.net]}</span></td>
            <td class="mono tiny">{e.method} {e.path}</td><td class="muted">{e.status}</td><td class="muted tiny">{e.ip}</td>
          </tr>
        {:else}
          <tr><td class="muted small">Nothing yet.</td></tr>
        {/each}
      </tbody>
    </table>
  </div>
</div>

<style>
  .card { display: flex; flex-direction: column; gap: 8px; }
  .card h3 { margin-bottom: 2px; }
  .wide { grid-column: 1 / -1; }
  .qr { width: 200px; height: 200px; background: #fff; border-radius: 8px; padding: 6px; }
  .qr :global(svg) { width: 100%; height: 100%; }
  .brk { word-break: break-all; }
  .grow { flex: 1; min-width: 0; }
  .w { width: 90px; }
  .wrap { flex-wrap: wrap; }
  .tk { padding: 6px 0; border-bottom: 1px solid var(--line); }
  .made { border: 1px solid #22c55e55; background: var(--green-dim); border-radius: 8px; padding: 10px; display: flex; flex-direction: column; gap: 6px; }
  .new { border-top: 1px solid var(--line); padding-top: 10px; display: flex; flex-direction: column; gap: 8px; }
  .chk { display: flex; gap: 6px; align-items: center; }
  .chk input { accent-color: var(--acc); }
  .audit { max-height: 360px; overflow: auto; }
  table { width: 100%; border-collapse: collapse; font-size: 12px; }
  td { padding: 5px 8px; border-bottom: 1px solid #26262c; }
  tr.bad td { color: var(--red); }
  .nw { white-space: nowrap; }
</style>
