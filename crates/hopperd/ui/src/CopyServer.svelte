<script>
  // Settings › General, above Remove: copy this server. Clone makes a new
  // server in the servers folder (with or without the world, own port; the
  // address and schedule stay with this one). Save as template keeps it
  // without world or players for New server › Template.
  import { Copy, LayoutTemplate, X } from '@lucide/svelte';
  import { api, post } from './lib/api.js';
  import { say } from './lib/toast.svelte.js';
  import { ask } from './lib/dialog.svelte.js';
  import { followJob } from './lib/jobs.js';
  import { bytes } from './lib/format.js';

  let { server } = $props();
  const alive = $derived(['running', 'starting', 'stopping'].includes(server.state));
  const base = $derived(`/servers/${encodeURIComponent(server.id)}`);

  let open = $state(false);
  let info = $state(null);
  let name = $state('');
  let withWorld = $state(true);
  let port = $state('');
  let addr = $state('');
  let busy = $state('');
  let error = $state('');
  let saving = $state('');

  const slug = (n) => n.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');
  const size = $derived(info ? info.sizes.total - (withWorld ? 0 : info.sizes.world) : 0);

  async function openClone() {
    error = '';
    busy = '';
    withWorld = true;
    info = null;
    open = true;
    try {
      info = await api(`${base}/clone-info`);
      name = info.name;
      port = String(info.port);
      addr = info.domain ? slug(name) : '';
    } catch (e) {
      error = e.message;
    }
  }

  async function clone() {
    error = '';
    busy = 'Starting';
    const address = !addr.trim() ? '' : info?.domain && !addr.includes('.') ? `${addr.trim()}.${info.domain}` : addr.trim();
    try {
      const { job } = await post(`${base}/clone`, { name: name.trim(), with_world: withWorld, port: port ? Number(port) : null, address });
      const r = await followJob(job, (st) => (busy = st.detail || 'Copying'));
      say(`Made ${name.trim()}, a copy of ${server.name}${withWorld ? '' : ' with a fresh world'}.`);
      open = false;
      if (r?.id) location.hash = `/server/${encodeURIComponent(r.id)}/console`;
    } catch (e) {
      error = e.message;
      busy = '';
    }
  }

  async function template() {
    const n = await ask(`Save ${server.name} as a template`, server.name, {
      body: 'Keeps its mods, configs and start files, without the world, players or backups. New server › Template starts a server from it.',
      ok: 'Save template',
      placeholder: 'Template name',
    });
    if (!n?.trim()) return;
    saving = 'Starting';
    try {
      const { job } = await post(`${base}/template`, { name: n.trim() });
      const t = await followJob(job, (st) => (saving = st.detail || 'Copying'));
      say(`Saved template ${t.name} (${t.mods} mod${t.mods === 1 ? '' : 's'}, ${bytes(t.size)}). New server › Template uses it.`);
    } catch (e) {
      say(e.message, true);
    } finally {
      saving = '';
    }
  }
</script>

<h4>Copy this server</h4>
<div class="fl">
  <div class="lab"><div>Clone</div><div class="tiny muted">A copy in the servers folder with its own port. The address and schedule aren't copied.</div></div>
  <div class="ctl"><button class="btn sm" disabled={alive} title={alive ? 'Stop the server first' : ''} onclick={openClone}><Copy size={13} />Clone…</button></div>
</div>
<div class="fl">
  <div class="lab"><div>Template</div><div class="tiny muted">{saving ? saving : 'Mods, configs and start files, no world. New server › Template starts from it.'}</div></div>
  <div class="ctl"><button class="btn sm" disabled={alive || !!saving} title={alive ? 'Stop the server first' : ''} onclick={template}><LayoutTemplate size={13} />Save as template…</button></div>
</div>
{#if alive}<div class="tiny warn">Stop the server first to copy it.</div>{/if}

{#if open}
  <div class="back" role="presentation" onclick={(e) => e.target === e.currentTarget && !busy && (open = false)}>
    <div class="box card">
      <h3><Copy size={15} />Clone {server.name}<span class="sp"></span><button class="btn sm icon ghost" disabled={!!busy} onclick={() => (open = false)}><X size={14} /></button></h3>
      <label class="lbl" for="cl-name">Name</label>
      <div class="input"><input id="cl-name" bind:value={name} disabled={!!busy} oninput={() => info?.domain && (addr = slug(name))} /></div>
      <span class="lbl">World</span>
      <div class="row">
        <div class="seg"><button class:on={withWorld} disabled={!!busy} onclick={() => (withWorld = true)}>With world</button><button class:on={!withWorld} disabled={!!busy} onclick={() => (withWorld = false)}>Fresh world</button></div>
        {#if info}<span class="tiny muted">world: {bytes(info.sizes.world)}</span>{/if}
      </div>
      <div class="two">
        <div><label class="lbl" for="cl-port">Port</label><div class="input"><input id="cl-port" type="number" bind:value={port} disabled={!!busy} /></div></div>
        {#if info?.domain}
          <div><label class="lbl" for="cl-addr">Address players join</label>
            <div class="row"><div class="input grow"><input id="cl-addr" class="mono" bind:value={addr} placeholder={info?.domain ? 'survival-copy' : 'optional'} disabled={!!busy} /></div>{#if info?.domain && addr && !addr.includes('.')}<span class="mono tiny muted">.{info.domain}</span>{/if}</div></div>
        {/if}
      </div>
      <div class="note small">
        {#if info}Copies {bytes(size)} to <span class="mono">{info.servers_root}</span>.{:else}Measuring…{/if}
        Not copied: backups, logs, crash reports, the schedule, the address. Players, ops and the whitelist are copied.
      </div>
      {#if error}<div class="err">{error}</div>{/if}
      {#if busy}<div class="busy small"><span class="spin"></span>{busy}</div>{/if}
      <div class="row end"><span class="sp"></span><button class="btn" disabled={!!busy} onclick={() => (open = false)}>Cancel</button><button class="btn pri" disabled={!!busy || !name.trim() || !info} onclick={clone}><Copy size={13} />Clone</button></div>
    </div>
  </div>
{/if}

<style>
  .fl { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 18px; align-items: center; padding: 10px 4px; border-bottom: 1px solid var(--line); }
  .lab > div:first-child { font-weight: 500; }
  .ctl { display: flex; align-items: center; gap: 8px; justify-content: flex-end; }
  .warn { color: var(--gold); margin-top: 4px; }
  .back { position: fixed; inset: 0; background: #0008; display: flex; justify-content: center; align-items: flex-start; z-index: 55; padding: 56px 16px 16px; overflow: auto; }
  .box { width: min(500px, 100%); display: flex; flex-direction: column; gap: 8px; box-shadow: 0 24px 60px #000c; }
  .box h3 { margin: 0 0 4px; display: flex; align-items: center; gap: 8px; }
  .two { display: grid; grid-template-columns: 140px 1fr; gap: 12px; }
  .two > div { display: flex; flex-direction: column; gap: 6px; }
  .grow { flex: 1; min-width: 0; }
  .seg { display: flex; border: 1px solid var(--line2); border-radius: 6px; overflow: hidden; }
  .seg button { padding: 5px 11px; font-size: 12.5px; color: var(--subtle); }
  .seg button + button { border-left: 1px solid var(--line2); }
  .seg button.on { background: var(--acc); color: #1a1a1f; font-weight: 600; }
  .note { border: 1px solid var(--line); background: var(--base); border-radius: 8px; padding: 8px 10px; color: var(--subtle); }
  .end { margin-top: 6px; gap: 8px; }
  .busy { display: flex; align-items: center; gap: 8px; color: var(--gold); }
  .spin { width: 12px; height: 12px; border: 2px solid var(--gold); border-right-color: transparent; border-radius: 50%; animation: s 0.8s linear infinite; }
  @keyframes s { to { transform: rotate(360deg); } }
  @media (max-width: 900px) { .fl { grid-template-columns: 1fr; } .ctl { justify-content: flex-start; } .two { grid-template-columns: 1fr; } }
</style>
